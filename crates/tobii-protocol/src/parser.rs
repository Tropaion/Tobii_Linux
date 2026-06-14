//! Inbound USB byte-stream reassembler.
//!
//! Device → host bytes arrive as length-prefixed envelopes:
//!   [dir=0x01][0 0 0][len_LE:u32][ttp_header:24][payload]
//! The IN envelope length field INCLUDES the 8-byte envelope (asymmetric vs OUT).
//! Large TTP responses are split across multiple USB transfers; continuation
//! transfers carry their own 8-byte envelope header wrapping raw payload bytes,
//! which we strip so the accumulator holds a clean [env][ttp_hdr][payload].

use crate::error::ProtocolError;
use crate::frame::{ENVELOPE_SIZE, TTP_HDR_SIZE};

const ACC_CAP: usize = 1 << 21; // 2 MiB

/// A fully reassembled TTP frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub magic: u32,
    pub seq: u32,
    pub op: u32,
    pub payload: Vec<u8>,
}

/// Accumulates inbound USB chunks and yields complete frames.
#[derive(Debug, Default)]
pub struct Parser {
    acc: Vec<u8>,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

impl Parser {
    pub fn new() -> Self {
        Self { acc: Vec::new() }
    }

    /// Bytes currently buffered (incomplete frame in progress).
    pub fn buffered(&self) -> usize {
        self.acc.len()
    }

    /// Reset the accumulator (e.g. after reconnect).
    pub fn reset(&mut self) {
        self.acc.clear();
    }

    /// Feed a USB chunk; returns any complete frames it produced.
    /// On a framing error the accumulator is reset and the error returned.
    pub fn feed(&mut self, src: &[u8]) -> Result<Vec<Frame>, ProtocolError> {
        let mut data = src;
        if self.acc.len() >= ENVELOPE_SIZE + TTP_HDR_SIZE {
            let plen = be32(&self.acc[ENVELOPE_SIZE + 20..]);
            let frame_size = ENVELOPE_SIZE + TTP_HDR_SIZE + plen as usize;
            if self.acc.len() < frame_size
                && src.len() >= ENVELOPE_SIZE
                && src[0] == 0x01
                && src[1] == 0x00
                && src[2] == 0x00
                && src[3] == 0x00
            {
                data = &src[ENVELOPE_SIZE..];
            }
        }

        if self.acc.len() + data.len() > ACC_CAP {
            self.acc.clear();
            return Err(ProtocolError::Overflow);
        }
        self.acc.extend_from_slice(data);

        let mut frames = Vec::new();
        loop {
            match self.drain_one() {
                Ok(Some(frame)) => frames.push(frame),
                Ok(None) => break,
                Err(e) => {
                    self.acc.clear();
                    return Err(e);
                }
            }
        }
        Ok(frames)
    }

    /// Try to drain one frame from the head of the accumulator.
    fn drain_one(&mut self) -> Result<Option<Frame>, ProtocolError> {
        if self.acc.len() < ENVELOPE_SIZE {
            return Ok(None);
        }
        if self.acc[0] != 0x01 {
            return Err(ProtocolError::BadDirection(self.acc[0]));
        }
        let env_len = le32(&self.acc[4..]);
        if (env_len as usize) < ENVELOPE_SIZE + TTP_HDR_SIZE {
            return Err(ProtocolError::BadLength(env_len));
        }
        if self.acc.len() < ENVELOPE_SIZE + TTP_HDR_SIZE {
            return Ok(None);
        }
        let hdr = &self.acc[ENVELOPE_SIZE..ENVELOPE_SIZE + TTP_HDR_SIZE];
        let magic = be32(&hdr[0..]);
        let seq = be32(&hdr[4..]);
        let op = be32(&hdr[12..]);
        let plen = be32(&hdr[20..]) as usize;
        let frame_size = ENVELOPE_SIZE + TTP_HDR_SIZE + plen;
        if frame_size > ACC_CAP {
            return Err(ProtocolError::BadLength(plen as u32));
        }
        if self.acc.len() < frame_size {
            return Ok(None);
        }
        let payload = self.acc[ENVELOPE_SIZE + TTP_HDR_SIZE..frame_size].to_vec();
        self.acc.drain(..frame_size);
        Ok(Some(Frame {
            magic,
            seq,
            op,
            payload,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{ENVELOPE_SIZE, TTP_HDR_SIZE, TTP_MAGIC_NOTIFY, TTP_MAGIC_RSP};

    fn fake_inbound(magic: u32, seq: u32, op: u32, payload: &[u8]) -> Vec<u8> {
        let total = (ENVELOPE_SIZE + TTP_HDR_SIZE + payload.len()) as u32;
        let mut v = Vec::new();
        v.extend_from_slice(&[0x01, 0, 0, 0]);
        v.extend_from_slice(&total.to_le_bytes());
        v.extend_from_slice(&magic.to_be_bytes());
        v.extend_from_slice(&seq.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes()); // flag
        v.extend_from_slice(&op.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn single_complete_frame() {
        let mut p = Parser::new();
        let buf = fake_inbound(TTP_MAGIC_RSP, 42, 0x3e8, &[0xde, 0xad, 0xbe, 0xef]);
        let frames = p.feed(&buf).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].magic, TTP_MAGIC_RSP);
        assert_eq!(frames[0].seq, 42);
        assert_eq!(frames[0].op, 0x3e8);
        assert_eq!(frames[0].payload, vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(p.buffered(), 0);
    }

    #[test]
    fn two_frames_concatenated() {
        let mut p = Parser::new();
        let mut buf = fake_inbound(TTP_MAGIC_RSP, 1, 0x100, &[0x11]);
        buf.extend(fake_inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &[0x22, 0x23]));
        let frames = p.feed(&buf).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].op, 0x500);
        assert_eq!(frames[1].payload, vec![0x22, 0x23]);
    }

    #[test]
    fn frame_split_across_two_chunks() {
        let mut p = Parser::new();
        let buf = fake_inbound(TTP_MAGIC_RSP, 7, 0x200, &[0xa1, 0xa2, 0xa3, 0xa4]);
        let frames = p.feed(&buf[..20]).unwrap();
        assert_eq!(frames.len(), 0);
        assert_eq!(p.buffered(), 20);
        let frames = p.feed(&buf[20..]).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].seq, 7);
        assert_eq!(p.buffered(), 0);
    }

    #[test]
    fn rejects_bad_direction() {
        let mut p = Parser::new();
        let buf = [0x02u8, 0, 0, 0, 0x20, 0, 0, 0];
        assert_eq!(
            p.feed(&buf),
            Err(crate::error::ProtocolError::BadDirection(0x02))
        );
        assert_eq!(p.buffered(), 0);
    }

    #[test]
    fn rejects_impossibly_small_length() {
        let mut p = Parser::new();
        let buf = [0x01u8, 0, 0, 0, 10, 0, 0, 0]; // len=10 < 8+24
        assert_eq!(
            p.feed(&buf),
            Err(crate::error::ProtocolError::BadLength(10))
        );
    }

    #[test]
    fn fragmented_multi_envelope_response() {
        let mut p = Parser::new();
        let full: Vec<u8> = (0..200u32).map(|i| i as u8).collect();

        let mut c1 = Vec::new();
        c1.extend_from_slice(&[0x01, 0, 0, 0]);
        c1.extend_from_slice(&43u32.to_le_bytes());
        c1.extend_from_slice(&TTP_MAGIC_RSP.to_be_bytes());
        c1.extend_from_slice(&99u32.to_be_bytes());
        c1.extend_from_slice(&0u32.to_be_bytes());
        c1.extend_from_slice(&0x44Cu32.to_be_bytes());
        c1.extend_from_slice(&0u32.to_be_bytes());
        c1.extend_from_slice(&200u32.to_be_bytes());
        c1.extend_from_slice(&full[..11]);
        assert_eq!(p.feed(&c1).unwrap().len(), 0);

        let mut c2 = Vec::new();
        c2.extend_from_slice(&[0x01, 0, 0, 0]);
        c2.extend_from_slice(&100u32.to_le_bytes());
        c2.extend_from_slice(&full[11..103]);
        assert_eq!(p.feed(&c2).unwrap().len(), 0);

        let frames = p.feed(&full[103..200]).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].seq, 99);
        assert_eq!(frames[0].op, 0x44C);
        assert_eq!(frames[0].payload.len(), 200);
        assert_eq!(frames[0].payload[0], 0);
        assert_eq!(p.buffered(), 0);
    }
}
