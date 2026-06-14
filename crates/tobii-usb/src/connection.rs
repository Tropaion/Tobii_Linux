//! Connection driver: runs the handshake, then yields decoded gaze samples.

use std::collections::VecDeque;
use std::time::Duration;

use tobii_protocol::frame::{OP_GAZE_NOTIFY, STREAM_GAZE, TTP_MAGIC_NOTIFY, TTP_MAGIC_RSP};
use tobii_protocol::{Frame, GazeSample, Handshake, HandshakeAction, Parser};

use crate::transport::{Transport, UsbError};

const READ_BUF: usize = 16384;
const RECV_TIMEOUT: Duration = Duration::from_millis(100);
const GAZE_TIMEOUT: Duration = Duration::from_millis(1000);
const HANDSHAKE_STEP_CAP: u32 = 400;

/// A live connection to the eye tracker. Generic over [`Transport`] so the
/// driver logic is testable without hardware.
pub struct Connection<T: Transport> {
    transport: T,
    parser: Parser,
    gaze_queue: VecDeque<GazeSample>,
}

impl<T: Transport> Connection<T> {
    /// Open a connection: run the handshake to completion, leaving the device
    /// subscribed and streaming gaze.
    pub fn connect(transport: T) -> Result<Self, UsbError> {
        let mut conn = Self {
            transport,
            parser: Parser::new(),
            gaze_queue: VecDeque::new(),
        };
        conn.run_handshake()?;
        Ok(conn)
    }

    /// Access the underlying transport (used in tests).
    pub fn transport(&self) -> &T {
        &self.transport
    }

    fn run_handshake(&mut self) -> Result<(), UsbError> {
        let mut hs = Handshake::new(STREAM_GAZE);
        let mut buf = [0u8; READ_BUF];
        for _ in 0..HANDSHAKE_STEP_CAP {
            match hs.poll() {
                HandshakeAction::Send(bytes) => {
                    self.transport.send(&bytes)?;
                    self.drain(&mut buf, Some(&mut hs));
                }
                HandshakeAction::Recv => {
                    self.drain(&mut buf, Some(&mut hs));
                }
                HandshakeAction::Done => return Ok(()),
                HandshakeAction::Failed => return Err(UsbError::Handshake),
            }
        }
        Err(UsbError::Handshake)
    }

    /// Read one transport chunk, parse frames, and route them. Response frames
    /// go to the handshake (if any); gaze notifications are queued. Other
    /// frames are ignored.
    fn drain(&mut self, buf: &mut [u8], mut hs: Option<&mut Handshake>) {
        if let Some(n) = self.transport.recv(buf, RECV_TIMEOUT) {
            if let Ok(frames) = self.parser.feed(&buf[..n]) {
                for f in frames {
                    self.route(f, hs.as_deref_mut());
                }
            }
        }
    }

    fn route(&mut self, f: Frame, hs: Option<&mut Handshake>) {
        if f.magic == TTP_MAGIC_RSP {
            // Only response frames advance the handshake (seq-safety: gaze
            // notifications must never be mistaken for handshake responses).
            if let Some(hs) = hs {
                hs.on_response(&f.payload);
            }
        } else if f.magic == TTP_MAGIC_NOTIFY && f.op == OP_GAZE_NOTIFY {
            if let Some(s) = GazeSample::decode(&f.payload) {
                self.gaze_queue.push_back(s);
            }
        }
    }

    /// Return the next gaze sample, reading from the device if none is queued.
    /// Returns `None` if no gaze arrives within the read timeout.
    pub fn next_gaze(&mut self) -> Option<GazeSample> {
        if let Some(s) = self.gaze_queue.pop_front() {
            return Some(s);
        }
        let mut buf = [0u8; READ_BUF];
        if let Some(n) = self.transport.recv(&mut buf, GAZE_TIMEOUT) {
            if let Ok(frames) = self.parser.feed(&buf[..n]) {
                for f in frames {
                    self.route(f, None);
                }
            }
        }
        self.gaze_queue.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::Duration;
    use tobii_protocol::frame::{ENVELOPE_SIZE, TTP_HDR_SIZE, TTP_MAGIC_NOTIFY, TTP_MAGIC_RSP};
    use tobii_protocol::tlv::{write_f64_q42, write_tag, write_u32, TAG_POINT2D, TAG_XDS_COLUMN};

    /// Build an inbound USB frame: [01 00 00 00][len_LE][ttp hdr][payload].
    fn inbound(magic: u32, seq: u32, op: u32, payload: &[u8]) -> Vec<u8> {
        let total = (ENVELOPE_SIZE + TTP_HDR_SIZE + payload.len()) as u32;
        let mut v = vec![0x01, 0, 0, 0];
        v.extend_from_slice(&total.to_le_bytes());
        v.extend_from_slice(&magic.to_be_bytes());
        v.extend_from_slice(&seq.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&op.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    // A query-realm response payload selecting realm_type = 0 (no auth).
    fn realm_type_zero() -> Vec<u8> {
        let mut p = vec![0x00, 0x00, 0x02, 0x00, 0x00, 0x04];
        p.extend_from_slice(&0u32.to_be_bytes());
        p
    }

    // A minimal 0x500 gaze payload: timestamp + gaze_point_2d (0.25, 0.75).
    fn gaze_payload() -> Vec<u8> {
        let mut w = Vec::new();
        w.extend_from_slice(&[0x00, 0x00]);
        let mut buf = tobii_protocol::bytes::Writer::new();
        write_tag(&mut buf, (2u32 << 16) | 0x0bb8);
        write_tag(&mut buf, TAG_XDS_COLUMN);
        write_u32(&mut buf, 0x01); // timestamp column
        buf.push_u8(6);
        buf.push_be32(8);
        buf.push_be64(42i64 as u64);
        write_tag(&mut buf, TAG_XDS_COLUMN);
        write_u32(&mut buf, 0x1c); // gaze_point_2d
        write_tag(&mut buf, TAG_POINT2D);
        write_f64_q42(&mut buf, 0.25);
        write_f64_q42(&mut buf, 0.75);
        w.extend_from_slice(&buf.into_vec());
        w
    }

    struct MockTransport {
        sent: Vec<Vec<u8>>,
        to_recv: VecDeque<Vec<u8>>,
    }
    impl Transport for MockTransport {
        fn send(&mut self, data: &[u8]) -> Result<(), UsbError> {
            self.sent.push(data.to_vec());
            Ok(())
        }
        fn recv(&mut self, buf: &mut [u8], _t: Duration) -> Option<usize> {
            let next = self.to_recv.pop_front()?;
            buf[..next.len()].copy_from_slice(&next);
            Some(next.len())
        }
    }

    #[test]
    fn connects_via_no_auth_handshake() {
        let t = MockTransport {
            sent: Vec::new(),
            to_recv: VecDeque::from(vec![
                inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
                inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
                inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
            ]),
        };
        let conn = Connection::connect(t).expect("handshake should complete");
        assert_eq!(conn.transport().sent.len(), 4);
    }

    #[test]
    fn streams_a_gaze_sample_after_connect() {
        let mut to_recv = VecDeque::from(vec![
            inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
            inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
            inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
        ]);
        to_recv.push_back(inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &gaze_payload()));
        let t = MockTransport { sent: Vec::new(), to_recv };

        let mut conn = Connection::connect(t).expect("connect");
        let s = conn.next_gaze().expect("a gaze sample");
        assert!(s.has(tobii_protocol::gaze::present::TIMESTAMP));
        assert_eq!(s.timestamp_us, 42);
        assert!((s.gaze_point_2d[0] - 0.25).abs() < 1e-9);
    }

    #[test]
    fn gaze_notification_during_handshake_does_not_break_it() {
        let t = MockTransport {
            sent: Vec::new(),
            to_recv: VecDeque::from(vec![
                inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
                inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &gaze_payload()),
                inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
                inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
            ]),
        };
        let mut conn = Connection::connect(t).expect("handshake survives stray gaze");
        assert!(conn.next_gaze().is_some());
    }
}
