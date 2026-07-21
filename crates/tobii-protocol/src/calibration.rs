//! Per-user gaze-calibration wire payloads (Phase 2).
//!
//! Payload builders only — the op code is applied by the transport's
//! request/response path (`Connection::request`). Every payload carries the
//! universal 2-byte `00 00` prefix. See the design spec §4 for the wire facts.

use crate::bytes::Writer;
use crate::tlv::{write_f64_q42, write_u32};

/// An opaque device calibration blob (the verbatim `cal_retrieve` response).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CalibrationBlob(pub Vec<u8>);

/// `cal_add_point` payload: `00 00` + Q42(x) + Q42(y) + u32(eye).
/// `x`/`y` are normalized display coords in `[0,1]`; `eye` is 0=both/1=L/2=R.
/// Note: two bare Q42 fields — NOT a point2d prolog.
pub fn cal_add_point_payload(x: f64, y: f64, eye: u32) -> Vec<u8> {
    let mut p = Writer::new();
    p.push_u8(0);
    p.push_u8(0);
    write_f64_q42(&mut p, x);
    write_f64_q42(&mut p, y);
    write_u32(&mut p, eye);
    p.into_vec()
}

/// `cal_start` / `cal_stop` / `cal_clear` payload: the `00 00` prefix only.
/// These session-control ops carry no arguments on the ET5 (the native
/// `tobii_calibration_start` even drops its `enabled_eye` argument on the wire).
pub fn cal_session_payload() -> Vec<u8> {
    vec![0x00, 0x00]
}

/// `cal_compute` (compute AND apply) payload: the `00 00` prefix only.
pub fn cal_compute_payload() -> Vec<u8> {
    vec![0x00, 0x00]
}

/// `cal_retrieve` payload: the `00 00` prefix only.
pub fn cal_retrieve_payload() -> Vec<u8> {
    vec![0x00, 0x00]
}

/// `cal_apply` payload: `00 00` + the raw blob bytes (no TLV header).
pub fn cal_apply_payload(blob: &[u8]) -> Vec<u8> {
    let mut p = Writer::new();
    p.push_u8(0);
    p.push_u8(0);
    p.push_bytes(blob);
    p.into_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_point_payload_is_exact() {
        // x=0.25 -> q42 = 0x0000_0100_0000_0000; y=0.75 -> 0x0000_0300_0000_0000; eye=0.
        let p = cal_add_point_payload(0.25, 0.75, 0);
        let expected: &[u8] = &[
            0x00, 0x00, // universal prefix
            0x04, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, // Q42(0.25)
            0x04, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00,
            0x00, // Q42(0.75)
            0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, // u32(0) eye
        ];
        assert_eq!(p, expected);
        assert_eq!(p.len(), 37); // frame will be 8 + 24 + 37 = 69
    }

    #[test]
    fn compute_and_retrieve_payloads_are_prefix_only() {
        assert_eq!(cal_compute_payload(), vec![0x00, 0x00]);
        assert_eq!(cal_retrieve_payload(), vec![0x00, 0x00]);
    }

    #[test]
    fn session_payload_is_prefix_only() {
        assert_eq!(cal_session_payload(), vec![0x00, 0x00]);
    }

    #[test]
    fn apply_payload_is_prefix_plus_raw_blob() {
        let p = cal_apply_payload(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(p, vec![0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn add_point_encodes_eye_choice() {
        // eye is the trailing u32; byte at index 36 is its low byte.
        assert_eq!(*cal_add_point_payload(0.5, 0.5, 2).last().unwrap(), 2);
    }

    #[test]
    fn real_device_calibration_blob_is_sane() {
        // Captured from a physical ET5 via `tobii calibrate` (live Task 7): the
        // calibration ran on the handshake's already-open realm (no re-unlock),
        // and the retrieved blob round-tripped through `cal_apply` unmodified.
        let blob = include_bytes!("testdata/real-calibration.blob");
        assert!(!blob.is_empty());
        assert!(blob.len() <= 4096, "blob within device cap: {}", blob.len());
    }
}
