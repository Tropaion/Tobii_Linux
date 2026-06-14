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

/// Cursor over a TLV byte slice.
pub struct Reader<'a> {
    buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn u8(&mut self) -> Result<u8, ProtocolError> {
        if self.remaining() < 1 {
            return Err(ProtocolError::ShortRead);
        }
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        if self.remaining() < n {
            return Err(ProtocolError::ShortRead);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u32_be(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32_be(&mut self) -> Result<i32, ProtocolError> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i64_be(&mut self) -> Result<i64, ProtocolError> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    /// Read [type=5][size=4][tag:u32], returning the tag.
    pub fn read_prolog_tag(&mut self) -> Result<u32, ProtocolError> {
        let t = self.u8()?;
        if t != 5 {
            return Err(ProtocolError::WrongType { expected: 5, found: t });
        }
        let s = self.u32_be()?;
        if s != 4 {
            return Err(ProtocolError::WrongSize { expected: 4, found: s });
        }
        self.u32_be()
    }

    pub fn read_u32(&mut self) -> Result<u32, ProtocolError> {
        let t = self.u8()?;
        if t != 2 {
            return Err(ProtocolError::WrongType { expected: 2, found: t });
        }
        let s = self.u32_be()?;
        if s != 4 {
            return Err(ProtocolError::WrongSize { expected: 4, found: s });
        }
        self.u32_be()
    }

    pub fn read_fixed16x16(&mut self) -> Result<f64, ProtocolError> {
        let t = self.u8()?;
        if t != 3 {
            return Err(ProtocolError::WrongType { expected: 3, found: t });
        }
        let s = self.u32_be()?;
        if s != 4 {
            return Err(ProtocolError::WrongSize { expected: 4, found: s });
        }
        Ok(self.i32_be()? as f64 / 65536.0)
    }

    pub fn read_fixed22x42(&mut self) -> Result<f64, ProtocolError> {
        let t = self.u8()?;
        if t != 4 {
            return Err(ProtocolError::WrongType { expected: 4, found: t });
        }
        let s = self.u32_be()?;
        if s != 8 {
            return Err(ProtocolError::WrongSize { expected: 8, found: s });
        }
        Ok(self.i64_be()? as f64 / Q42_SCALE)
    }

    pub fn read_s64(&mut self) -> Result<i64, ProtocolError> {
        let t = self.u8()?;
        if t != 6 {
            return Err(ProtocolError::WrongType { expected: 6, found: t });
        }
        let s = self.u32_be()?;
        if s != 8 {
            return Err(ProtocolError::WrongSize { expected: 8, found: s });
        }
        self.i64_be()
    }

    /// Consume an xds_row prolog; returns the column count packed in the tag.
    pub fn read_xds_row(&mut self) -> Result<u32, ProtocolError> {
        let tag = self.read_prolog_tag()?;
        if tag & 0xffff != TAG_XDS_ROW_MASK {
            return Err(ProtocolError::WrongTag { expected: TAG_XDS_ROW_MASK, found: tag });
        }
        Ok((tag >> 16) & 0xfff)
    }

    /// Consume an xds_column prolog + u32; returns the column id.
    pub fn read_xds_column(&mut self) -> Result<u32, ProtocolError> {
        let tag = self.read_prolog_tag()?;
        if tag != TAG_XDS_COLUMN {
            return Err(ProtocolError::WrongTag { expected: TAG_XDS_COLUMN, found: tag });
        }
        self.read_u32()
    }

    pub fn read_point3d(&mut self) -> Result<[f64; 3], ProtocolError> {
        let tag = self.read_prolog_tag()?;
        if tag != TAG_POINT3D {
            return Err(ProtocolError::WrongTag { expected: TAG_POINT3D, found: tag });
        }
        Ok([self.read_fixed22x42()?, self.read_fixed22x42()?, self.read_fixed22x42()?])
    }

    pub fn read_point2d(&mut self) -> Result<[f64; 2], ProtocolError> {
        let tag = self.read_prolog_tag()?;
        if tag != TAG_POINT2D {
            return Err(ProtocolError::WrongTag { expected: TAG_POINT2D, found: tag });
        }
        Ok([self.read_fixed22x42()?, self.read_fixed22x42()?])
    }
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

#[cfg(test)]
mod reader_tests {
    use super::*;
    use crate::bytes::Writer;

    #[test]
    fn round_trips_u32_and_q42() {
        let mut w = Writer::new();
        write_u32(&mut w, 0xDEAD_BEEF);
        write_f64_q42(&mut w, 12.5);
        let buf = w.into_vec();
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_u32().unwrap(), 0xDEAD_BEEF);
        assert!((r.read_fixed22x42().unwrap() - 12.5).abs() < 1e-9);
    }

    #[test]
    fn reads_point3d() {
        let mut w = Writer::new();
        write_point(&mut w, -1.0, 2.0, 300.0);
        let buf = w.into_vec();
        let mut r = Reader::new(&buf);
        let p = r.read_point3d().unwrap();
        assert!((p[0] + 1.0).abs() < 1e-9);
        assert!((p[1] - 2.0).abs() < 1e-9);
        assert!((p[2] - 300.0).abs() < 1e-9);
    }

    #[test]
    fn xds_row_decodes_count_from_tag() {
        let mut w = Writer::new();
        write_tag(&mut w, (5u32 << 16) | 0x0bb8);
        let buf = w.into_vec();
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_xds_row().unwrap(), 5);
    }

    #[test]
    fn short_read_errors() {
        let buf = [0x02u8, 0, 0, 0]; // truncated u32 header
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_u32(), Err(crate::error::ProtocolError::ShortRead));
    }
}
