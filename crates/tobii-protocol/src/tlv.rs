//! TLV codec for the ET5 wire protocol.
//!
//! Field header is a 1-byte TYPE followed by a 4-byte big-endian SIZE,
//! then SIZE bytes of body. Struct fields begin with a `type=5` prolog
//! carrying a 4-byte tag.

use crate::bytes::Writer;
use crate::error::ProtocolError;

/// Q42 fixed-point scale: 2^42.
pub const Q42_SCALE: f64 = 4_398_046_511_104.0;

/// Struct tags (found after a type=5 prolog).
pub const TAG_XDS_ROW_MASK: u32 = 0x0bb8; // low 16 bits of an xds_row tag
pub const TAG_XDS_COLUMN: u32 = 0x020bb9;
pub const TAG_POINT2D: u32 = 0x021f40;
pub const TAG_POINT3D: u32 = 0x031f41;

/// Encode millimetres as a Q42 fixed-point integer: round(mm * 2^42).
pub fn q42_encode(mm: f64) -> i64 {
    (mm * Q42_SCALE).round() as i64
}

/// type=5 prolog carrying a 4-byte tag.
pub fn write_tag(w: &mut Writer, tag: u32) {
    w.push_u8(5);
    w.push_be32(4);
    w.push_be32(tag);
}

/// type=2 u32.
pub fn write_u32(w: &mut Writer, v: u32) {
    w.push_u8(2);
    w.push_be32(4);
    w.push_be32(v);
}

/// type=4 Q42 fixed-point f64 (8-byte BE signed body).
pub fn write_f64_q42(w: &mut Writer, v: f64) {
    w.push_u8(4);
    w.push_be32(8);
    w.push_be64(q42_encode(v) as u64);
}

/// point3d = prolog(0x031f41) + 3 × Q42.
pub fn write_point(w: &mut Writer, x: f64, y: f64, z: f64) {
    write_tag(w, TAG_POINT3D);
    write_f64_q42(w, x);
    write_f64_q42(w, y);
    write_f64_q42(w, z);
}

#[cfg(test)]
mod encode_tests {
    use super::*;
    use crate::bytes::Writer;

    #[test]
    fn q42_matches_reference() {
        assert_eq!(q42_encode(200.0), 879_609_302_220_800);
        assert_eq!(q42_encode(0.0), 0);
        assert_eq!(q42_encode(-200.0), -879_609_302_220_800);
    }

    #[test]
    fn encodes_u32_tlv() {
        let mut w = Writer::new();
        write_u32(&mut w, 0x3039);
        assert_eq!(w.into_vec(), vec![0x02, 0, 0, 0, 4, 0, 0, 0x30, 0x39]);
    }

    #[test]
    fn encodes_tag_tlv() {
        let mut w = Writer::new();
        write_tag(&mut w, 0x10100);
        assert_eq!(w.into_vec(), vec![0x05, 0, 0, 0, 4, 0, 0x01, 0x01, 0x00]);
    }

    #[test]
    fn point_is_48_bytes() {
        let mut w = Writer::new();
        write_point(&mut w, 1.0, 2.0, 3.0);
        assert_eq!(w.len(), 48);
    }
}
