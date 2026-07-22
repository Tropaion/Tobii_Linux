//! Reassembler for the *outbound* (host -> device) TTP stream.
//!
//! [`tobii_protocol::Parser`] handles the inbound envelope only: it requires the
//! direction byte `0x01` and treats the length field as *including* the 8-byte
//! envelope. Outbound frames (see `tobii_protocol::frame::build_out_frame`) use
//! direction byte `0x00` and a length that *excludes* the envelope, so `Parser`
//! rejects them outright. This is the minimal mirror for that direction.
//!
//! Layout on the wire (host -> device):
//!   [00 00 00 00][len_LE:u32 = ttp_len][ttp_header:24 BE][payload]
//!
//! Reassembly is driven by the TTP header's own payload-length field (offset
//! 20, big-endian) — authoritative and identical to how `Parser` works — so a
//! frame split across USB transfers is rejoined the same way. Continuation
//! transfers that repeat the 8-byte outbound envelope are detected and stripped,
//! mirroring `Parser`'s inbound continuation handling.

use tobii_protocol::frame::{ENVELOPE_SIZE, TTP_HDR_SIZE, TTP_MAGIC_REQ};
use tobii_protocol::Frame;

const ACC_CAP: usize = 1 << 21; // 2 MiB, matching tobii_protocol::Parser.

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Accumulates outbound USB chunks and yields complete [`Frame`]s.
#[derive(Debug, Default)]
pub struct OutParser {
    acc: Vec<u8>,
}

/// A framing error in the outbound stream, kept local so callers can report it
/// without depending on `tobii_protocol`'s inbound-oriented `ProtocolError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutError {
    BadDirection(u8),
    BadMagic(u32),
    Overflow,
}

impl std::fmt::Display for OutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutError::BadDirection(b) => {
                write!(f, "outbound direction byte {b:#x} (expected 0x00)")
            }
            OutError::BadMagic(m) => write!(f, "outbound TTP magic {m:#x} (expected 0x51)"),
            OutError::Overflow => write!(f, "outbound reassembly buffer overflow"),
        }
    }
}

impl OutParser {
    pub fn new() -> Self {
        Self { acc: Vec::new() }
    }

    /// Bytes currently buffered (an incomplete frame in progress).
    pub fn buffered(&self) -> usize {
        self.acc.len()
    }

    /// Feed one outbound USB chunk; returns any complete frames it produced.
    /// On a framing error the accumulator is reset and the error returned.
    pub fn feed(&mut self, src: &[u8]) -> Result<Vec<Frame>, OutError> {
        let mut data = src;
        // A continuation transfer for an in-progress frame repeats the 8-byte
        // outbound envelope; strip it so `acc` stays a clean [env][hdr][payload].
        if self.acc.len() >= ENVELOPE_SIZE + TTP_HDR_SIZE {
            let plen = be32(&self.acc[ENVELOPE_SIZE + 20..]);
            let frame_size = ENVELOPE_SIZE + TTP_HDR_SIZE + plen as usize;
            if self.acc.len() < frame_size
                && src.len() >= ENVELOPE_SIZE
                && src[0] == 0x00
                && src[1] == 0x00
                && src[2] == 0x00
                && src[3] == 0x00
            {
                data = &src[ENVELOPE_SIZE..];
            }
        }

        if self.acc.len() + data.len() > ACC_CAP {
            self.acc.clear();
            return Err(OutError::Overflow);
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

    fn drain_one(&mut self) -> Result<Option<Frame>, OutError> {
        if self.acc.len() < ENVELOPE_SIZE {
            return Ok(None);
        }
        if self.acc[0] != 0x00 {
            return Err(OutError::BadDirection(self.acc[0]));
        }
        if self.acc.len() < ENVELOPE_SIZE + TTP_HDR_SIZE {
            return Ok(None);
        }
        let hdr = &self.acc[ENVELOPE_SIZE..ENVELOPE_SIZE + TTP_HDR_SIZE];
        let magic = be32(&hdr[0..]);
        // Outbound frames are always requests; a different magic means the byte
        // stream has desynced (or this bulk endpoint is not TTP) — report it.
        if magic != TTP_MAGIC_REQ {
            return Err(OutError::BadMagic(magic));
        }
        let seq = be32(&hdr[4..]);
        let op = be32(&hdr[12..]);
        let plen = be32(&hdr[20..]) as usize;
        let frame_size = ENVELOPE_SIZE + TTP_HDR_SIZE + plen;
        if frame_size > ACC_CAP {
            return Err(OutError::Overflow);
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
    use tobii_protocol::frame::{build_out_frame, OP_HELLO};

    #[test]
    fn decodes_a_single_out_frame() {
        let mut p = OutParser::new();
        let bytes = build_out_frame(7, OP_HELLO, &[0xAA, 0xBB, 0xCC]);
        let frames = p.feed(&bytes).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].magic, TTP_MAGIC_REQ);
        assert_eq!(frames[0].seq, 7);
        assert_eq!(frames[0].op, OP_HELLO);
        assert_eq!(frames[0].payload, vec![0xAA, 0xBB, 0xCC]);
        assert_eq!(p.buffered(), 0);
    }

    #[test]
    fn two_frames_in_one_chunk() {
        let mut p = OutParser::new();
        let mut bytes = build_out_frame(1, 0x100, &[0x11]);
        bytes.extend(build_out_frame(2, 0x200, &[0x22, 0x23]));
        let frames = p.feed(&bytes).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].op, 0x200);
        assert_eq!(frames[1].seq, 2);
    }

    #[test]
    fn frame_split_across_two_chunks() {
        let mut p = OutParser::new();
        let bytes = build_out_frame(9, 0x3f2, &[0xa1, 0xa2, 0xa3, 0xa4]);
        assert_eq!(p.feed(&bytes[..18]).unwrap().len(), 0);
        assert_eq!(p.buffered(), 18);
        let frames = p.feed(&bytes[18..]).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].seq, 9);
        assert_eq!(p.buffered(), 0);
    }

    #[test]
    fn rejects_wrong_direction_byte() {
        let mut p = OutParser::new();
        // Inbound-style dir byte 0x01 on the OUT stream is a framing error.
        let buf = [0x01u8, 0, 0, 0, 24, 0, 0, 0];
        assert_eq!(p.feed(&buf), Err(OutError::BadDirection(0x01)));
        assert_eq!(p.buffered(), 0);
    }

    #[test]
    fn rejects_garbage_magic() {
        let mut p = OutParser::new();
        let mut buf = vec![0x00, 0, 0, 0];
        buf.extend_from_slice(&24u32.to_le_bytes());
        buf.extend_from_slice(&0xDEADBEEFu32.to_be_bytes()); // bad magic
        buf.extend_from_slice(&[0u8; 20]); // rest of the 24-byte header
        assert_eq!(p.feed(&buf), Err(OutError::BadMagic(0xDEADBEEF)));
    }
}
