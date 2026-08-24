//! Connection driver: runs the handshake, then yields decoded gaze samples.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tobii_protocol::calibration::{
    cal_add_point_payload, cal_apply_payload, cal_compute_payload, cal_discard_point_payload,
    cal_retrieve_payload, cal_session_payload, CalibrationBlob,
};
use tobii_protocol::commands::{
    parse_enabled_eye, set_display_area_corners_payload, set_enabled_eye_payload,
    subscribe_payload, EnabledEye,
};
use tobii_protocol::frame::{
    build_out_frame, OP_CAL_ADD_POINT, OP_CAL_APPLY, OP_CAL_CLEAR, OP_CAL_COMPUTE,
    OP_CAL_DISCARD_POINT, OP_CAL_RETRIEVE, OP_CAL_START, OP_CAL_STOP, OP_GAZE_NOTIFY,
    OP_GET_ENABLED_EYE, OP_SET_DISPLAY_AREA, OP_SET_ENABLED_EYE, OP_STREAM_CATALOG, OP_SUBSCRIBE,
    OP_UNSUBSCRIBE, STREAM_GAZE, TTP_MAGIC_NOTIFY, TTP_MAGIC_RSP,
};
use tobii_protocol::{DisplayCorners, Frame, GazeSample, Handshake, HandshakeAction, Parser};

use crate::transport::{Transport, UsbError};

const READ_BUF: usize = 16384;
/// Larger buffer for the streaming read path ([`Connection::read_notifications`]).
/// The eye-camera stream is ~2.6 MB/s (78 KB @ 33 Hz); with synchronous bulk
/// reads, throughput is round-trip-limited, so each transfer must move a lot to
/// keep up — a 16 KB buffer backs the pipe up and the preview lags. Heap-held and
/// reused (too big for the stack, and re-zeroing it every read would be waste).
const STREAM_READ_BUF: usize = 262144;
const RECV_TIMEOUT: Duration = Duration::from_millis(100);
const GAZE_TIMEOUT: Duration = Duration::from_millis(1000);
const HANDSHAKE_STEP_CAP: u32 = 400;
/// Wall-clock window `request` waits for a matching response. A deadline (not
/// an iteration count) is essential: gaze notifications stream concurrently and
/// are routed to the queue from inside the same read loop, so an iteration cap
/// would shrink the effective wait to nothing under normal gaze traffic.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Defensive upper bound for `add_calibration_point`. NOTE: the device actually
/// acks a point almost immediately (verified live 2026-07-21 — it does NOT block
/// while sampling; the fixation dwell is enforced host-side), so this ceiling is
/// never reached in practice; it only guards a pathological stall.
const CAL_POINT_TIMEOUT: Duration = Duration::from_secs(30);
/// Smallest blob worth sending back to the device. Purely a smoke test: the one
/// ET5 measured produced ~414 KB, and the ~1.5 KB stubs this code used to store
/// were the response's status word plus a fragment, not a calibration.
const MIN_PLAUSIBLE_BLOB: usize = 4096;
/// Window for `cal_apply`, which transfers the whole model. The reference
/// implementation allows 60 s for the comparable operation.
const CAL_APPLY_TIMEOUT: Duration = Duration::from_secs(60);

/// Whether a stored blob is big enough to be a real calibration.
///
/// Exposed so callers can decide *not* to offer one, rather than discovering it
/// through an error. Seeding a new session from a previous calibration is an
/// optimisation; treating "no usable previous calibration" as a failure turns a
/// perfectly good from-scratch calibration into an abort.
pub fn is_plausible_calibration(blob: &[u8]) -> bool {
    blob.len() >= MIN_PLAUSIBLE_BLOB
}

/// A live connection to the eye tracker. Generic over [`Transport`] so the
/// driver logic is testable without hardware.
pub struct Connection<T: Transport> {
    transport: T,
    parser: Parser,
    gaze_queue: VecDeque<GazeSample>,
    /// Next TTP sequence number for post-handshake requests.
    seq: u32,
    /// How long [`Connection::request`] waits for a matching response.
    request_timeout: Duration,
    /// Reused large read buffer for the streaming path (see [`STREAM_READ_BUF`]).
    read_buf: Vec<u8>,
}

impl<T: Transport> Connection<T> {
    /// Open a connection: run the handshake to completion, leaving the device
    /// subscribed and streaming gaze.
    pub fn connect(transport: T) -> Result<Self, UsbError> {
        let mut conn = Self {
            transport,
            parser: Parser::new(),
            gaze_queue: VecDeque::new(),
            seq: 1,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            read_buf: vec![0u8; STREAM_READ_BUF],
        };
        conn.run_handshake()?;
        Ok(conn)
    }

    /// Access the underlying transport (used in tests).
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Override how long [`Connection::request`] waits for a response.
    pub fn set_request_timeout(&mut self, t: Duration) {
        self.request_timeout = t;
    }

    fn next_seq(&mut self) -> u32 {
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1);
        if self.seq == 0 {
            self.seq = 1;
        }
        s
    }

    /// Send a request frame and return the payload of the first response frame
    /// whose op AND seq match the request (the device echoes the request seq
    /// back in its response, hardware-verified). Matching on seq as well as op
    /// means a duplicated or stale-seq ack for a repeated op (e.g.
    /// `cal_add_point`, fired once per calibration point) can never be
    /// mismatched to the wrong request. Gaze notifications arriving in the
    /// meantime are queued for [`Connection::next_gaze`]. Returns `Ok(None)`
    /// if no matching response arrives within `self.request_timeout`.
    pub fn request(&mut self, op: u32, payload: &[u8]) -> Result<Option<Vec<u8>>, UsbError> {
        self.request_until(op, payload, self.request_timeout)
    }

    /// [`Connection::request`] with an explicit wall-clock response window.
    fn request_until(
        &mut self,
        op: u32,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Option<Vec<u8>>, UsbError> {
        let seq = self.next_seq();
        self.transport.send(&build_out_frame(seq, op, payload))?;
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; READ_BUF];
        while Instant::now() < deadline {
            let Some(n) = self.transport.recv(&mut buf, RECV_TIMEOUT) else {
                continue;
            };
            let Ok(frames) = self.parser.feed(&buf[..n]) else {
                continue;
            };
            let mut matched = None;
            for f in frames {
                if matched.is_none() && f.magic == TTP_MAGIC_RSP && f.op == op && f.seq == seq {
                    matched = Some(f.payload);
                } else {
                    self.route(f, None);
                }
            }
            if matched.is_some() {
                return Ok(matched);
            }
        }
        Ok(None)
    }

    /// Send a request and require a matching response (calibration ops always
    /// reply). Returns the response payload, or `NoResponse` on timeout.
    fn expect_response(&mut self, op: u32, payload: &[u8]) -> Result<Vec<u8>, UsbError> {
        self.request(op, payload)?
            .ok_or(UsbError::NoResponse { op })
    }

    /// Enter calibration mode. Must precede point collection; the device stays
    /// in calibration mode until `stop_calibration`.
    pub fn start_calibration(&mut self) -> Result<(), UsbError> {
        self.expect_response(OP_CAL_START, &cal_session_payload())?;
        Ok(())
    }

    /// Leave calibration mode (call after `compute_and_apply_calibration`).
    pub fn stop_calibration(&mut self) -> Result<(), UsbError> {
        self.expect_response(OP_CAL_STOP, &cal_session_payload())?;
        Ok(())
    }

    /// Discard collected/active calibration data (the original clears right
    /// after `start`). Destructive: wipes the current calibration.
    pub fn clear_calibration(&mut self) -> Result<(), UsbError> {
        self.expect_response(OP_CAL_CLEAR, &cal_session_payload())?;
        Ok(())
    }

    /// Sample one calibration stimulus point. `x`/`y` normalized `[0,1]`;
    /// `eye` is a mask — 1=L, 2=R, 3=both (`calibration::CAL_EYE_*`). There is
    /// no zero-means-both: 0 selects nothing and the point is silently dropped.
    /// Assumes calibration runs in the already-open realm.
    pub fn add_calibration_point(&mut self, x: f64, y: f64, eye: u32) -> Result<(), UsbError> {
        self.request_until(
            OP_CAL_ADD_POINT,
            &cal_add_point_payload(x, y, eye),
            CAL_POINT_TIMEOUT,
        )?
        .ok_or(UsbError::NoResponse {
            op: OP_CAL_ADD_POINT,
        })?;
        Ok(())
    }

    /// "Redo" a previously-sampled point: discard whatever data collection
    /// gathered for it, so a later `add_calibration_point` call for the same
    /// (x, y) starts fresh. Reverse-engineered from native disassembly;
    /// UNVERIFIED ON REAL HARDWARE — callers should treat a failure here as
    /// best-effort (log and continue) rather than a fatal error, matching this
    /// codebase's existing `set_enabled_eye` best-effort precedent.
    pub fn discard_calibration_point(&mut self, x: f64, y: f64) -> Result<(), UsbError> {
        self.request_until(
            OP_CAL_DISCARD_POINT,
            &cal_discard_point_payload(x, y),
            CAL_POINT_TIMEOUT,
        )?
        .ok_or(UsbError::NoResponse {
            op: OP_CAL_DISCARD_POINT,
        })?;
        Ok(())
    }

    /// Compute and apply the calibration from the collected points.
    pub fn compute_and_apply_calibration(&mut self) -> Result<(), UsbError> {
        self.expect_response(OP_CAL_COMPUTE, &cal_compute_payload())?;
        Ok(())
    }

    /// Retrieve the opaque calibration blob (the verbatim response payload).
    pub fn retrieve_calibration(&mut self) -> Result<CalibrationBlob, UsbError> {
        let payload = self.expect_response(OP_CAL_RETRIEVE, &cal_retrieve_payload())?;
        // Drop the 2-byte status prefix every TTP response payload carries and
        // every other decoder here already skips. `apply_calibration` prepends
        // its own, so keeping it sent `[00 00][00 00][blob]` — the blob shifted
        // two bytes, with nothing checking the reply to notice.
        // `>= 2`, not `> 2`: a status-only response is an empty blob, not a
        // two-byte one. The caller's plausibility check would catch it either
        // way, but "0 bytes" is the truthful answer.
        let blob = if payload.len() >= 2 {
            payload[2..].to_vec()
        } else {
            payload
        };
        Ok(CalibrationBlob(blob))
    }

    /// Re-apply a previously saved calibration blob.
    ///
    /// Rejects a blob too small to be a real calibration rather than sending
    /// it. This device's is on the order of hundreds of kilobytes; anything
    /// tiny is a leftover from before the calibration ops were corrected, when
    /// `retrieve` returned a stub that no amount of applying would help.
    pub fn apply_calibration(&mut self, blob: &[u8]) -> Result<(), UsbError> {
        if blob.len() < MIN_PLAUSIBLE_BLOB {
            return Err(UsbError::ImplausibleCalibration { len: blob.len() });
        }
        // Its own window: this pushes a few hundred KB across ~40 transfers and
        // then waits for the device to ingest a whole calibration model. The
        // 10 s default is a request timeout, sized for one small round trip.
        self.request_until(OP_CAL_APPLY, &cal_apply_payload(blob), CAL_APPLY_TIMEOUT)?
            .ok_or(UsbError::NoResponse { op: OP_CAL_APPLY })?;
        Ok(())
    }

    /// Apply a display-area configuration to the device, returning whether the
    /// device acknowledged it.
    ///
    /// This MUST be called in-session on every connect for eye tracking to
    /// work: the ET5 resets its stored display area to a ~4mm stub whenever it
    /// reboots (which it does on every session close / USB re-enumeration), and
    /// it produces NO eye-detection data (validity stays 4, all origins zero)
    /// until a valid display area is set. The vendor stack re-applies the saved
    /// configuration on every connect for exactly this reason.
    pub fn set_display_area(&mut self, c: &DisplayCorners) -> Result<bool, UsbError> {
        let payload = set_display_area_corners_payload(
            c.tl[0], c.tl[1], c.tl[2], c.tr[0], c.tr[1], c.tr[2], c.bl[0], c.bl[1], c.bl[2],
        );
        Ok(self.request(OP_SET_DISPLAY_AREA, &payload)?.is_some())
    }

    /// Read which eye(s) the tracker is set to detect. `None` if the device did
    /// not respond (e.g. firmware without the enabled_eye property).
    pub fn get_enabled_eye(&mut self) -> Result<Option<EnabledEye>, UsbError> {
        Ok(self
            .request(OP_GET_ENABLED_EYE, &[])?
            .and_then(|p| parse_enabled_eye(&p)))
    }

    /// Set which eye(s) the tracker detects; returns whether the device ack'd.
    pub fn set_enabled_eye(&mut self, eye: EnabledEye) -> Result<bool, UsbError> {
        Ok(self
            .request(OP_SET_ENABLED_EYE, &set_enabled_eye_payload(eye))?
            .is_some())
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
                HandshakeAction::Done => {
                    self.seq = hs.seq();
                    return Ok(());
                }
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

    /// Diagnostic: the raw, **undecoded** payload of the next gaze notification.
    /// `next_gaze` throws away every column the decoder does not model, so this
    /// is the only way to inspect unmapped columns (e.g. head pose) live. The
    /// first gaze frame in the chunk is returned; any other frames are routed
    /// normally so nothing is dropped. Returns `None` on read timeout.
    pub fn next_gaze_payload(&mut self) -> Option<Vec<u8>> {
        let mut buf = [0u8; READ_BUF];
        let n = self.transport.recv(&mut buf, GAZE_TIMEOUT)?;
        let frames = self.parser.feed(&buf[..n]).ok()?;
        let mut found = None;
        for f in frames {
            if found.is_none() && f.magic == TTP_MAGIC_NOTIFY && f.op == OP_GAZE_NOTIFY {
                found = Some(f.payload);
            } else {
                self.route(f, None);
            }
        }
        found
    }

    /// Subscribe to an additional TTP stream beyond the handshake's gaze stream.
    /// Returns whether the device acked (a matching response arrived within the
    /// request window — set a short [`Connection::set_request_timeout`] first
    /// when probing many unknown ids, so silent ones fail fast). Diagnostic:
    /// used to hunt for undiscovered streams (e.g. head pose).
    pub fn subscribe_stream(&mut self, stream_id: u16) -> Result<bool, UsbError> {
        Ok(self
            .request(OP_SUBSCRIBE, &subscribe_payload(stream_id))?
            .is_some())
    }

    /// Stop a stream. Returns whether the device acked.
    ///
    /// Matters for probing: subscribing a range to see what answers leaves every
    /// one of them running for the rest of the session, so a probe permanently
    /// changes the traffic it was trying to characterize. See
    /// [`OP_UNSUBSCRIBE`] for how confident we are in the op number.
    pub fn unsubscribe_stream(&mut self, stream_id: u16) -> Result<bool, UsbError> {
        Ok(self
            .request(OP_UNSUBSCRIBE, &subscribe_payload(stream_id))?
            .is_some())
    }

    /// Ask the device to enumerate its own streams. Returns the raw reply
    /// payload; the encoding is not yet known well enough to model.
    /// See [`OP_STREAM_CATALOG`] — this may simply not be a real op.
    pub fn stream_catalog(&mut self) -> Result<Option<Vec<u8>>, UsbError> {
        self.request(OP_STREAM_CATALOG, &[])
    }

    /// Read the next NOTIFICATION frame of ANY op — the general form of
    /// [`Connection::next_gaze_payload`], which only returns `0x500`. Returns
    /// `(op, payload)`. Diagnostic: for observing streams other than gaze.
    ///
    /// NOTE: returns only the FIRST notify in the transport chunk and drops the
    /// rest — so co-occurring streams are undercounted. Use
    /// [`Connection::read_notifications`] to characterize concurrent streams.
    pub fn next_notification(&mut self) -> Option<(u32, Vec<u8>)> {
        let mut buf = [0u8; READ_BUF];
        let n = self.transport.recv(&mut buf, GAZE_TIMEOUT)?;
        let frames = self.parser.feed(&buf[..n]).ok()?;
        let mut found = None;
        for f in frames {
            if found.is_none() && f.magic == TTP_MAGIC_NOTIFY {
                found = Some((f.op, f.payload));
            } else {
                self.route(f, None);
            }
        }
        found
    }

    /// Read one transport chunk and return EVERY notification frame in it as
    /// `(op, payload)` — unlike [`Connection::next_notification`], notifies that
    /// share a chunk are all kept. Essential for accurately characterizing
    /// multiple concurrent streams (a small stream co-occurring with gaze in the
    /// same chunk would otherwise be dropped). Non-notify frames are routed.
    pub fn read_notifications(&mut self) -> Notifications {
        // Reuse the large heap buffer (disjoint field borrows: transport,
        // read_buf, parser are separate fields).
        let Some(n) = self.transport.recv(&mut self.read_buf, GAZE_TIMEOUT) else {
            return Notifications::silent();
        };
        let Ok(frames) = self.parser.feed(&self.read_buf[..n]) else {
            return Notifications {
                frames: Vec::new(),
                saw_traffic: true,
            };
        };
        let mut out = Vec::new();
        for f in frames {
            if f.magic == TTP_MAGIC_NOTIFY {
                out.push((f.op, f.payload));
            } else {
                self.route(f, None);
            }
        }
        Notifications {
            frames: out,
            saw_traffic: true,
        }
    }
}

/// One streaming read: the complete notifications it yielded, plus whether the
/// transport delivered any bytes at all.
///
/// These are different things, and only one of them means the device has gone
/// quiet. A chunk that ends mid-frame yields **no complete notification** while
/// being perfectly healthy traffic — the rest of that frame is in the next
/// chunk. A caller that reads "no frames" as "no device" and backs off will
/// stall in the middle of a busy stream, which on the gaze path shows up as a
/// freeze followed by a jump when the queued samples all arrive at once.
///
/// Iterates and derefs as the frame list, so it reads exactly like the `Vec` it
/// replaced; check [`Notifications::saw_traffic`] only where the distinction
/// matters.
#[derive(Debug, Default)]
pub struct Notifications {
    frames: Vec<(u32, Vec<u8>)>,
    /// `false` only when the transport returned nothing within its timeout.
    pub saw_traffic: bool,
}

impl Notifications {
    /// The transport timed out: nothing arrived at all.
    fn silent() -> Self {
        Self::default()
    }
}

impl IntoIterator for Notifications {
    type Item = (u32, Vec<u8>);
    type IntoIter = std::vec::IntoIter<(u32, Vec<u8>)>;
    fn into_iter(self) -> Self::IntoIter {
        self.frames.into_iter()
    }
}

impl std::ops::Deref for Notifications {
    type Target = [(u32, Vec<u8>)];
    fn deref(&self) -> &Self::Target {
        &self.frames
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
        let t = MockTransport {
            sent: Vec::new(),
            to_recv,
        };

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

    #[test]
    fn request_returns_matching_response_and_queues_gaze() {
        let mut to_recv = VecDeque::from(vec![
            inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
            inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
            inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
        ]);
        // After connect: a stray gaze frame, then the get-display-area response.
        to_recv.push_back(inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &gaze_payload()));
        to_recv.push_back(inbound(TTP_MAGIC_RSP, 5, 0x596, &[0xAA, 0xBB]));
        let t = MockTransport {
            sent: Vec::new(),
            to_recv,
        };
        let mut conn = Connection::connect(t).expect("connect");
        let resp = conn
            .request(0x596, &[])
            .expect("io ok")
            .expect("a response");
        assert_eq!(resp, vec![0xAA, 0xBB]);
        // The gaze that arrived before the response was queued, not dropped.
        assert!(conn.next_gaze().is_some());
    }

    #[test]
    fn request_queues_gaze_that_trails_the_response_in_one_chunk() {
        // One transport read delivers the matching RSP followed by a gaze
        // NOTIFY; the trailing gaze must be queued, not dropped.
        let mut to_recv = VecDeque::from(vec![
            inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
            inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
            inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
        ]);
        // The no-auth handshake sends 4 frames (hello/query/open/subscribe) but
        // only needs 3 responses; `run_handshake` still issues one opportunistic
        // extra `recv` after the subscribe send. Absorb that with an empty read
        // so the RSP+gaze chunk below survives untouched for `request` itself.
        to_recv.push_back(Vec::new());
        let mut chunk = inbound(TTP_MAGIC_RSP, 5, 0x596, &[0xAA, 0xBB]);
        chunk.extend_from_slice(&inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &gaze_payload()));
        to_recv.push_back(chunk);
        let t = MockTransport {
            sent: Vec::new(),
            to_recv,
        };
        let mut conn = Connection::connect(t).expect("connect");
        let resp = conn
            .request(0x596, &[])
            .expect("io ok")
            .expect("a response");
        assert_eq!(resp, vec![0xAA, 0xBB]);
        assert!(
            conn.next_gaze().is_some(),
            "trailing gaze must be queued, not dropped"
        );
    }

    #[test]
    fn request_returns_none_when_no_matching_response() {
        let t = MockTransport {
            sent: Vec::new(),
            to_recv: VecDeque::from(vec![
                inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
                inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
                inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
            ]),
        };
        let mut conn = Connection::connect(t).expect("connect");
        // The mock's `recv` returns instantly, so shorten the deadline to keep
        // the test fast while still exercising the timeout path.
        conn.set_request_timeout(Duration::from_millis(150));
        // Nothing left to receive → the request window drains to None.
        assert!(conn.request(0x596, &[]).expect("io ok").is_none());
    }

    fn connected_with(post: Vec<Vec<u8>>) -> Connection<MockTransport> {
        let mut to_recv = VecDeque::from(vec![
            inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
            inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
            inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
            Vec::new(), // filler consumed by the post-subscribe drain
        ]);
        to_recv.extend(post);
        Connection::connect(MockTransport {
            sent: Vec::new(),
            to_recv,
        })
        .expect("connect")
    }

    #[test]
    fn add_calibration_point_gets_ack() {
        let mut conn = connected_with(vec![inbound(TTP_MAGIC_RSP, 5, 0x406, &[])]);
        assert!(conn.add_calibration_point(0.25, 0.75, 0).is_ok());
    }

    #[test]
    fn session_ops_send_correct_ops_and_ack() {
        // start (0x3f2), clear (0x424), stop (0x3fc) in sequence; seqs 5,6,7.
        let mut conn = connected_with(vec![
            inbound(TTP_MAGIC_RSP, 5, 0x3f2, &[]),
            inbound(TTP_MAGIC_RSP, 6, 0x424, &[]),
            inbound(TTP_MAGIC_RSP, 7, 0x3fc, &[]),
        ]);
        assert!(conn.start_calibration().is_ok());
        assert!(conn.clear_calibration().is_ok());
        assert!(conn.stop_calibration().is_ok());
    }

    #[test]
    fn subscribe_stream_sends_op_and_reports_ack() {
        // First post-handshake request is seq 5; subscribe op is 0x4c4.
        let mut conn = connected_with(vec![inbound(TTP_MAGIC_RSP, 5, 0x4c4, &[])]);
        assert!(conn.subscribe_stream(0x501).expect("acked"));
        // The sent frame carries the subscribe op and the stream id (BE @ payload 9..10).
        let sent = conn.transport().sent.last().unwrap();
        assert_eq!(&sent[20..24], &[0, 0, 0x04, 0xc4]); // op 0x4c4
    }

    #[test]
    fn read_notifications_keeps_co_occurring_notifies() {
        // Two notifies (gaze 0x500 + a small 0x504) delivered in ONE chunk must
        // both be returned — next_notification would drop the second.
        let g = inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &[0x11]);
        let s = inbound(TTP_MAGIC_NOTIFY, 0, 0x504, &[0x22, 0x33]);
        let mut chunk = g.clone();
        chunk.extend_from_slice(&s);
        let mut conn = connected_with(vec![chunk]);
        let got = conn.read_notifications();
        let ops: Vec<u32> = got.iter().map(|(op, _)| *op).collect();
        assert!(
            ops.contains(&0x500) && ops.contains(&0x504),
            "got ops {ops:?}"
        );
    }

    #[test]
    fn a_chunk_ending_mid_frame_is_traffic_not_silence() {
        // Half a gaze frame: no complete notification, but the device is very
        // much alive and the rest is on its way. Callers that back off on "no
        // frames" would stall the stream here, so the two must stay separable.
        let full = inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &gaze_payload());
        let half = full[..full.len() / 2].to_vec();
        let mut conn = connected_with(vec![half]);

        let got = conn.read_notifications();
        assert!(got.is_empty(), "a truncated frame must not decode");
        assert!(
            got.saw_traffic,
            "bytes did arrive — this is not a silent device"
        );
    }

    #[test]
    fn a_transport_timeout_reports_silence() {
        let mut conn = connected_with(vec![]);
        let got = conn.read_notifications();
        assert!(got.is_empty());
        assert!(!got.saw_traffic);
    }

    #[test]
    fn next_notification_returns_any_notify_op() {
        // A notify frame with a non-gaze op (0x501) must still surface.
        let mut conn = connected_with(vec![inbound(TTP_MAGIC_NOTIFY, 0, 0x501, &[0xAB, 0xCD])]);
        let (op, payload) = conn.next_notification().expect("a notification");
        assert_eq!(op, 0x501);
        assert_eq!(payload, vec![0xAB, 0xCD]);
    }

    #[test]
    fn retrieve_calibration_returns_the_body_without_the_status_prefix() {
        let mut conn = connected_with(vec![inbound(
            TTP_MAGIC_RSP,
            5,
            0x44c,
            &[0xDE, 0xAD, 0xBE, 0xEF],
        )]);
        let blob = conn.retrieve_calibration().expect("blob");
        assert_eq!(blob.0, vec![0xBE, 0xEF]);
    }

    #[test]
    fn compute_without_response_errors() {
        // No post-connect responses -> compute drains to NoResponse.
        let mut conn = connected_with(vec![]);
        conn.set_request_timeout(Duration::from_millis(150));
        assert!(matches!(
            conn.compute_and_apply_calibration(),
            Err(UsbError::NoResponse { op }) if op == 0x42e
        ));
    }

    #[test]
    fn request_skips_wrong_seq_response() {
        let mut to_recv = VecDeque::from(vec![
            inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
            inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
            inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
            Vec::new(),
        ]);
        // Same op, WRONG seq (99) — must be ignored; then the correct seq (5).
        to_recv.push_back(inbound(TTP_MAGIC_RSP, 99, 0x596, &[0xBA, 0xD0]));
        to_recv.push_back(inbound(TTP_MAGIC_RSP, 5, 0x596, &[0xAA, 0xBB]));
        let mut conn = Connection::connect(MockTransport {
            sent: Vec::new(),
            to_recv,
        })
        .expect("connect");
        assert_eq!(
            conn.request(0x596, &[]).expect("io").expect("resp"),
            vec![0xAA, 0xBB]
        );
    }

    #[test]
    fn apply_calibration_sends_prefixed_blob_and_acks() {
        let blob: Vec<u8> = (0..MIN_PLAUSIBLE_BLOB as u32).map(|i| i as u8).collect();
        let mut conn = connected_with(vec![inbound(TTP_MAGIC_RSP, 5, 0x456, &[])]);
        assert!(conn.apply_calibration(&blob).is_ok());
        // Outbound payload (after envelope+header) is exactly `00 00` + blob.
        // The mock transport does no chunking, so the whole frame is one entry.
        let sent = conn.transport().sent.last().expect("a sent frame");
        assert_eq!(&sent[32..], &[&[0x00, 0x00][..], &blob[..]].concat()[..]);
    }

    #[test]
    fn a_retrieved_blob_loses_the_status_prefix_it_arrived_with() {
        // Every response payload starts with two status bytes. Keeping them
        // meant `apply` re-sent `[00 00][00 00][blob]`, shifted by two.
        let mut conn = connected_with(vec![inbound(
            TTP_MAGIC_RSP,
            5,
            0x44c,
            &[0x00, 0x00, 0xDE, 0xAD],
        )]);
        assert_eq!(
            conn.retrieve_calibration().expect("blob").0,
            vec![0xDE, 0xAD]
        );
    }

    #[test]
    fn a_blob_too_small_to_be_a_calibration_is_refused_not_sent() {
        // The stubs stored before the calibration ops were corrected are ~1.5KB
        // and are not calibrations; sending one back is worse than doing nothing.
        let mut conn = connected_with(vec![inbound(TTP_MAGIC_RSP, 5, 0x456, &[])]);
        let before = conn.transport().sent.len();
        assert!(matches!(
            conn.apply_calibration(&[0u8; 1478]),
            Err(UsbError::ImplausibleCalibration { len: 1478 })
        ));
        assert_eq!(conn.transport().sent.len(), before, "nothing was sent");
    }

    #[test]
    fn set_display_area_sends_op_and_reports_ack() {
        let mut conn = connected_with(vec![inbound(TTP_MAGIC_RSP, 5, 0x5a0, &[])]);
        let c = DisplayCorners {
            tl: [-1.0, 1.0, 0.0],
            tr: [1.0, 1.0, 0.0],
            bl: [-1.0, -1.0, 0.0],
        };
        assert!(conn.set_display_area(&c).expect("io ok"));
        // The last sent frame is a SET_DISPLAY_AREA request (op 0x5a0).
        assert_eq!(
            &conn.transport().sent.last().unwrap()[20..24],
            &[0, 0, 0x05, 0xa0]
        );
    }

    #[test]
    fn get_enabled_eye_parses_device_response() {
        // The hardware-captured BOTH response for op 0xc62.
        let resp = [
            0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x03,
        ];
        let mut conn = connected_with(vec![inbound(TTP_MAGIC_RSP, 5, 0xc62, &resp)]);
        assert_eq!(conn.get_enabled_eye().expect("io"), Some(EnabledEye::Both));
    }

    #[test]
    fn set_enabled_eye_sends_op_and_reports_ack() {
        let mut conn = connected_with(vec![inbound(TTP_MAGIC_RSP, 5, 0xc58, &[])]);
        assert!(conn.set_enabled_eye(EnabledEye::Left).expect("io"));
        // The last sent frame is a SET_ENABLED_EYE request (op 0xc58).
        assert_eq!(
            &conn.transport().sent.last().unwrap()[20..24],
            &[0, 0, 0x0c, 0x58]
        );
    }
}
