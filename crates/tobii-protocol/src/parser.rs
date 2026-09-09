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

/// Whether `src` begins with a continuation envelope for a frame that still
/// needs `remaining` more bytes.
///
/// The `01 00 00 00` direction-plus-padding prefix is **not** sufficient on its
/// own, which is what this used to test. That exact byte sequence is also the
/// start of a legitimate type-`0x01` TLV field header (`01` then a big-endian
/// size whose top three bytes are zero for any realistic length), and it occurs
/// in camera pixel data often enough to matter — a 78 KB frame at 33 Hz gives
/// the pattern many chances to land on a transfer boundary. When it false
/// positives, eight bytes of real payload are silently discarded and the frame
/// is corrupted rather than rejected.
///
/// So the envelope's own length field is checked too. Inbound it **includes**
/// the 8-byte envelope (asymmetric with outbound — see the module docs), so a
/// genuine continuation satisfies both of:
///
/// * it is at least an envelope long;
/// * it does not claim more payload than the in-flight frame still needs.
///
/// The TLV false positive fails the first test: `01 00 00 00 04 …` reads a
/// length of 4, which is smaller than the envelope it would have to be.
///
/// # What this must NOT also check
///
/// An earlier version required `env_len <= src.len()` — that the envelope's
/// length fit inside the USB read carrying it. That looks obviously right and
/// is wrong about this device, and it broke every calibration retrieval on real
/// hardware while every test stayed green.
///
/// Measured on an ET5: `env_len` is the size of the whole continuation **run**,
/// not of the read it arrives in. A 778 KB calibration blob comes back as
///
/// ```text
/// chunk1  len=100    env_len=778188   <- one envelope, then 47 raw reads
/// chunk52 len=8      env_len=572      <- a second envelope, alone in an 8-byte read
/// ```
///
/// and `778188-8 + 572-8 + 11 == 778755 == plen`. Both real envelopes fail
/// `env_len <= src.len()`, so neither was stripped, 16 envelope bytes were
/// spliced into the payload, and the leftovers made the next frame decode as
/// `BadDirection` — which `feed` reports as an error, discarding the frame it
/// had already built. The caller then saw no response at all and timed out.
///
/// CI could not catch it: the committed replay capture contains no fragmented
/// response, and the two tests below build continuations whose length field
/// happens to equal their chunk length.
fn looks_like_continuation(src: &[u8], remaining: usize) -> bool {
    if src.len() < ENVELOPE_SIZE
        || src[0] != 0x01
        || src[1] != 0x00
        || src[2] != 0x00
        || src[3] != 0x00
    {
        return false;
    }
    let env_len = le32(&src[4..]) as usize;
    // Strictly greater: an envelope declaring a run of zero payload bytes is
    // not something the device sends, and believing one swallows 8 bytes of
    // real payload and desyncs the frame.
    env_len > ENVELOPE_SIZE && env_len - ENVELOPE_SIZE <= remaining
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
                && looks_like_continuation(src, frame_size - self.acc.len())
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

    /// The false positive this guard exists for. `01 00 00 00` is the start of
    /// a continuation envelope AND of a legitimate type-0x01 TLV field header,
    /// and it turns up in camera pixel data. Stripping eight bytes on that
    /// alone silently corrupts the frame instead of rejecting anything.
    #[test]
    fn payload_that_merely_starts_like_an_envelope_is_not_stripped() {
        // A frame whose payload continues in a second chunk.
        let payload_len = 16usize;
        let mut first = Vec::new();
        first.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        first.extend_from_slice(
            &((ENVELOPE_SIZE + TTP_HDR_SIZE + payload_len) as u32).to_le_bytes(),
        );
        let mut hdr = vec![0u8; TTP_HDR_SIZE];
        hdr[0..4].copy_from_slice(&0x52u32.to_be_bytes()); // magic
        hdr[4..8].copy_from_slice(&1u32.to_be_bytes()); // seq
        hdr[12..16].copy_from_slice(&0xc62u32.to_be_bytes()); // op
        hdr[20..24].copy_from_slice(&(payload_len as u32).to_be_bytes());
        first.extend_from_slice(&hdr);

        // A type-0x01 TLV field of size 4 — the exact bytes `01 00 00 00 04`.
        let tail: [u8; 16] = [
            0x01, 0x00, 0x00, 0x00, 0x04, 0xde, 0xad, 0xbe, 0xef, 0x11, 0x22, 0x33, 0x44, 0x55,
            0x66, 0x77,
        ];

        let mut p = Parser::new();
        assert!(p.feed(&first).unwrap().is_empty(), "frame is incomplete");
        let frames = p.feed(&tail).unwrap();
        assert_eq!(frames.len(), 1, "the frame should complete");
        assert_eq!(
            frames[0].payload,
            tail.to_vec(),
            "eight bytes of real payload were eaten as a continuation envelope"
        );
    }

    /// The shape a REAL fragmented response has, which no test had.
    ///
    /// Measured on an ET5 retrieving a 778 KB calibration blob: the
    /// continuation envelope's length field is the size of the whole
    /// continuation RUN, so it is far larger than the USB read carrying it, and
    /// a second envelope arrives ALONE in an 8-byte read. A guard that required
    /// the length to fit inside its own chunk rejected both, spliced 16
    /// envelope bytes into the payload, and made every calibration retrieval
    /// fail — with every test green, because every test built an envelope whose
    /// length happened to equal its chunk.
    #[test]
    fn a_continuation_envelope_longer_than_its_own_chunk_is_still_stripped() {
        // Scaled down, same shape: header says 40 payload bytes, delivered as
        // an envelope claiming the whole 40-byte run in a 12-byte read, then
        // the rest raw, then a second envelope alone in an 8-byte read.
        let payload_len = 40usize;
        let mut first = Vec::new();
        first.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        first.extend_from_slice(&((ENVELOPE_SIZE + TTP_HDR_SIZE) as u32).to_le_bytes());
        let mut hdr = vec![0u8; TTP_HDR_SIZE];
        hdr[0..4].copy_from_slice(&0x52u32.to_be_bytes());
        hdr[4..8].copy_from_slice(&7u32.to_be_bytes());
        hdr[12..16].copy_from_slice(&0x44cu32.to_be_bytes()); // OP_CAL_RETRIEVE
        hdr[20..24].copy_from_slice(&(payload_len as u32).to_be_bytes());
        first.extend_from_slice(&hdr);

        let mut p = Parser::new();
        assert!(p.feed(&first).unwrap().is_empty(), "header only so far");

        // First run: 32 payload bytes, announced as 8 + 32 but delivered in a
        // 12-byte read (envelope + 4 bytes) followed by raw reads.
        let mut run1 = vec![0x01, 0x00, 0x00, 0x00];
        run1.extend_from_slice(&((ENVELOPE_SIZE + 32) as u32).to_le_bytes());
        run1.extend_from_slice(&[0xaa; 4]);
        assert!(p.feed(&run1).unwrap().is_empty());
        assert!(p.feed(&[0xaa; 28]).unwrap().is_empty(), "raw continuation");

        // Second run: an envelope ALONE in an 8-byte read, then its 8 bytes.
        let mut run2 = vec![0x01, 0x00, 0x00, 0x00];
        run2.extend_from_slice(&((ENVELOPE_SIZE + 8) as u32).to_le_bytes());
        assert_eq!(run2.len(), 8, "the envelope fills the whole read");
        assert!(p.feed(&run2).unwrap().is_empty());
        let frames = p.feed(&[0xbb; 8]).unwrap();

        assert_eq!(frames.len(), 1, "the frame should have completed");
        assert_eq!(frames[0].op, 0x44c);
        let mut want = vec![0xaa; 32];
        want.extend_from_slice(&[0xbb; 8]);
        assert_eq!(
            frames[0].payload, want,
            "envelope bytes were spliced into the payload"
        );
        assert_eq!(p.buffered(), 0, "nothing should be left over");
    }

    /// A continuation whose declared run happens to equal its chunk.
    ///
    /// The easy case, and — before the hardware measurement above — the ONLY
    /// case any test covered, which is why a guard that required exactly that
    /// passed everything and broke every calibration retrieval.
    #[test]
    fn a_real_continuation_envelope_is_still_stripped() {
        let payload_len = 12usize;
        let mut first = Vec::new();
        first.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        first.extend_from_slice(
            &((ENVELOPE_SIZE + TTP_HDR_SIZE + payload_len) as u32).to_le_bytes(),
        );
        let mut hdr = vec![0u8; TTP_HDR_SIZE];
        hdr[0..4].copy_from_slice(&0x52u32.to_be_bytes());
        hdr[4..8].copy_from_slice(&1u32.to_be_bytes());
        hdr[12..16].copy_from_slice(&0xc62u32.to_be_bytes());
        hdr[20..24].copy_from_slice(&(payload_len as u32).to_be_bytes());
        first.extend_from_slice(&hdr);
        first.extend_from_slice(&[0xaa; 4]); // first 4 payload bytes

        // Continuation: envelope whose length INCLUDES itself, carrying the
        // remaining 8 payload bytes.
        let mut cont = Vec::new();
        cont.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        cont.extend_from_slice(&((ENVELOPE_SIZE + 8) as u32).to_le_bytes());
        cont.extend_from_slice(&[0xbb; 8]);

        let mut p = Parser::new();
        assert!(p.feed(&first).unwrap().is_empty());
        let frames = p.feed(&cont).unwrap();
        assert_eq!(frames.len(), 1);
        let mut want = vec![0xaa; 4];
        want.extend_from_slice(&[0xbb; 8]);
        assert_eq!(
            frames[0].payload, want,
            "the envelope should have been stripped"
        );
    }

    /// The guard must not accept an envelope claiming more than the frame needs.
    #[test]
    fn a_continuation_claiming_more_than_the_frame_needs_is_refused() {
        assert!(!looks_like_continuation(
            &[0x01, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0x00],
            4
        ));
        // Length smaller than the envelope itself: the TLV false positive.
        assert!(!looks_like_continuation(
            &[0x01, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00],
            64
        ));
        // A well-formed one is accepted.
        assert!(looks_like_continuation(
            &[0x01, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00],
            64
        ));
    }
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
