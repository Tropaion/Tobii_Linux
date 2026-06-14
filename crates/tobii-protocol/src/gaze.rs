//! Gaze sample decoding (0x500 notification payloads).
//!
//! Coordinate spaces (per the reference): tracker-space (mm, origin at the IR
//! sensor array), display-space (tracker-space shifted by the display-area
//! offset), and normalized 2D ([0,1]² ray→plane intersection on display-area).

use crate::tlv::Reader;

/// `present` bitmask flags for the populated fields of a [`GazeSample`].
pub mod present {
    pub const TIMESTAMP: u32 = 1 << 0;
    pub const FRAME_COUNTER: u32 = 1 << 1;
    pub const VALIDITY_L: u32 = 1 << 2;
    pub const VALIDITY_R: u32 = 1 << 3;
    pub const PUPIL_L: u32 = 1 << 4;
    pub const PUPIL_R: u32 = 1 << 5;
    pub const GAZE_2D: u32 = 1 << 6;
    pub const GAZE_2D_L: u32 = 1 << 7;
    pub const GAZE_2D_R: u32 = 1 << 8;
    pub const EYE_ORIGIN_L: u32 = 1 << 9;
    pub const EYE_ORIGIN_R: u32 = 1 << 10;
    pub const GAZE_2D_UNFILTERED: u32 = 1 << 11;
    pub const EYE_ORIGIN_RAW_L: u32 = 1 << 12;
    pub const EYE_ORIGIN_RAW_R: u32 = 1 << 13;
    pub const GAZE_3D_L: u32 = 1 << 14;
    pub const GAZE_3D_R: u32 = 1 << 15;
}

/// A decoded gaze frame. Fields are only meaningful when their `present` bit
/// is set (check via [`GazeSample::has`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GazeSample {
    pub present_mask: u32,
    pub frame_counter: u32,
    pub validity_l: u32,
    pub validity_r: u32,
    pub timestamp_us: i64,
    pub pupil_l_mm: f64,
    pub pupil_r_mm: f64,
    pub gaze_point_2d: [f64; 2],
    pub gaze_point_2d_unfiltered: [f64; 2],
    pub gaze_point_2d_l: [f64; 2],
    pub gaze_point_2d_r: [f64; 2],
    pub eye_origin_l_mm: [f64; 3],
    pub eye_origin_r_mm: [f64; 3],
    pub eye_origin_raw_l_mm: [f64; 3],
    pub eye_origin_raw_r_mm: [f64; 3],
    pub gaze_point_3d_l_mm: [f64; 3],
    pub gaze_point_3d_r_mm: [f64; 3],
}

/// TLV data kind for an unknown column, used to skip it by reading its width.
#[derive(Clone, Copy)]
enum Kind {
    S64,
    U32,
    Fixed16x16,
    Point2d,
    Point3d,
}

/// Map a column id to its TLV kind (mirrors the reference `columnKind`).
fn column_kind(col: u32) -> Option<Kind> {
    match col {
        0x01 => Some(Kind::S64),
        0x02 | 0x03 | 0x04 | 0x08 | 0x09 | 0x0a | 0x17 | 0x18 | 0x22 | 0x24 | 0x25 | 0x27 => {
            Some(Kind::Point3d)
        }
        0x05 | 0x0b | 0x1c | 0x20 | 0x19 | 0x1a => Some(Kind::Point2d),
        0x06 | 0x0c | 0x29 | 0x2b => Some(Kind::Fixed16x16),
        0x07 | 0x0d | 0x0e | 0x11 | 0x14 | 0x15 | 0x16 | 0x1b | 0x1d | 0x1e | 0x1f | 0x21
        | 0x23 | 0x26 | 0x28 | 0x2a | 0x2c => Some(Kind::U32),
        _ => None,
    }
}

impl GazeSample {
    /// True if `flag` (a [`present`] constant) was populated.
    pub fn has(&self, flag: u32) -> bool {
        self.present_mask & flag != 0
    }

    /// Decode a 0x500 notification payload. Returns `None` only if the payload
    /// is too short or the row header is unreadable. If a column is truncated
    /// or unknown mid-row, decoding stops and the partial sample collected so
    /// far is returned (matching the reference decoder); check populated fields
    /// via [`GazeSample::has`].
    pub fn decode(payload: &[u8]) -> Option<GazeSample> {
        if payload.len() < 2 {
            return None;
        }
        let mut r = Reader::new(payload);
        r.skip(2); // 2-byte payload prefix
        let n_cols = r.read_xds_row().ok()?;

        let mut s = GazeSample::default();
        let mut i = 0;
        while i < n_cols && r.remaining() > 0 {
            i += 1;
            let col = match r.read_xds_column() {
                Ok(c) => c,
                Err(_) => return Some(s),
            };
            match col {
                0x01 => match r.read_s64() {
                    Ok(v) => {
                        s.timestamp_us = v;
                        s.present_mask |= present::TIMESTAMP;
                    }
                    Err(_) => return Some(s),
                },
                0x02 => {
                    if !set3(
                        &mut r,
                        &mut s.eye_origin_l_mm,
                        &mut s.present_mask,
                        present::EYE_ORIGIN_L,
                    ) {
                        return Some(s);
                    }
                }
                0x08 => {
                    if !set3(
                        &mut r,
                        &mut s.eye_origin_r_mm,
                        &mut s.present_mask,
                        present::EYE_ORIGIN_R,
                    ) {
                        return Some(s);
                    }
                }
                0x04 => {
                    if !set3(
                        &mut r,
                        &mut s.gaze_point_3d_l_mm,
                        &mut s.present_mask,
                        present::GAZE_3D_L,
                    ) {
                        return Some(s);
                    }
                }
                0x0a => {
                    if !set3(
                        &mut r,
                        &mut s.gaze_point_3d_r_mm,
                        &mut s.present_mask,
                        present::GAZE_3D_R,
                    ) {
                        return Some(s);
                    }
                }
                0x17 => {
                    if !set3(
                        &mut r,
                        &mut s.eye_origin_raw_l_mm,
                        &mut s.present_mask,
                        present::EYE_ORIGIN_RAW_L,
                    ) {
                        return Some(s);
                    }
                }
                0x18 => {
                    if !set3(
                        &mut r,
                        &mut s.eye_origin_raw_r_mm,
                        &mut s.present_mask,
                        present::EYE_ORIGIN_RAW_R,
                    ) {
                        return Some(s);
                    }
                }
                0x05 => {
                    if !set2(
                        &mut r,
                        &mut s.gaze_point_2d_l,
                        &mut s.present_mask,
                        present::GAZE_2D_L,
                    ) {
                        return Some(s);
                    }
                }
                0x0b => {
                    if !set2(
                        &mut r,
                        &mut s.gaze_point_2d_r,
                        &mut s.present_mask,
                        present::GAZE_2D_R,
                    ) {
                        return Some(s);
                    }
                }
                0x1c => {
                    if !set2(
                        &mut r,
                        &mut s.gaze_point_2d,
                        &mut s.present_mask,
                        present::GAZE_2D,
                    ) {
                        return Some(s);
                    }
                }
                0x20 => {
                    if !set2(
                        &mut r,
                        &mut s.gaze_point_2d_unfiltered,
                        &mut s.present_mask,
                        present::GAZE_2D_UNFILTERED,
                    ) {
                        return Some(s);
                    }
                }
                0x06 => match r.read_fixed16x16() {
                    Ok(v) => {
                        s.pupil_l_mm = v;
                        s.present_mask |= present::PUPIL_L;
                    }
                    Err(_) => return Some(s),
                },
                0x0c => match r.read_fixed16x16() {
                    Ok(v) => {
                        s.pupil_r_mm = v;
                        s.present_mask |= present::PUPIL_R;
                    }
                    Err(_) => return Some(s),
                },
                0x07 => match r.read_u32() {
                    Ok(v) => {
                        s.validity_l = v;
                        s.present_mask |= present::VALIDITY_L;
                    }
                    Err(_) => return Some(s),
                },
                0x0d => match r.read_u32() {
                    Ok(v) => {
                        s.validity_r = v;
                        s.present_mask |= present::VALIDITY_R;
                    }
                    Err(_) => return Some(s),
                },
                0x14 => match r.read_u32() {
                    Ok(v) => {
                        s.frame_counter = v;
                        s.present_mask |= present::FRAME_COUNTER;
                    }
                    Err(_) => return Some(s),
                },
                // Known-but-unmodeled column: read and discard by its kind.
                // Unknown column (or truncation) stops decoding with a partial.
                other => match column_kind(other) {
                    Some(kind) if skip_column(&mut r, kind) => {}
                    _ => return Some(s),
                },
            }
        }
        Some(s)
    }
}

/// Read and discard a known-but-unmodeled column of the given kind, advancing
/// the reader past it. Returns `false` if the column was truncated.
fn skip_column(r: &mut Reader, kind: Kind) -> bool {
    match kind {
        Kind::S64 => r.read_s64().is_ok(),
        Kind::U32 => r.read_u32().is_ok(),
        Kind::Fixed16x16 => r.read_fixed16x16().is_ok(),
        Kind::Point2d => r.read_point2d().is_ok(),
        Kind::Point3d => r.read_point3d().is_ok(),
    }
}

/// Read a point3d into `dst` and set `flag`. Returns `false` if the column was
/// truncated, signalling the caller to stop and return the partial sample
/// (matching the scalar columns and the reference decoder).
fn set3(r: &mut Reader, dst: &mut [f64; 3], mask: &mut u32, flag: u32) -> bool {
    match r.read_point3d() {
        Ok(v) => {
            *dst = v;
            *mask |= flag;
            true
        }
        Err(_) => false,
    }
}

/// Read a point2d into `dst` and set `flag`. Returns `false` on truncation
/// (see [`set3`]).
fn set2(r: &mut Reader, dst: &mut [f64; 2], mask: &mut u32, flag: u32) -> bool {
    match r.read_point2d() {
        Ok(v) => {
            *dst = v;
            *mask |= flag;
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::Writer;
    use crate::tlv::{write_f64_q42, write_tag, write_u32, TAG_POINT2D, TAG_XDS_COLUMN};

    /// Build a minimal 0x500 gaze payload: 2-byte prefix, xds_row(count=3),
    /// column 0x01 (timestamp s64), column 0x07 (validity_L u32=0),
    /// column 0x1c (gaze_point_2d = (0.25, 0.75)).
    fn synthetic_payload() -> Vec<u8> {
        let mut w = Writer::new();
        w.push_u8(0x00);
        w.push_u8(0x00);
        write_tag(&mut w, (3u32 << 16) | 0x0bb8);

        // column 0x01: timestamp (s64, type=6)
        write_tag(&mut w, TAG_XDS_COLUMN);
        write_u32(&mut w, 0x01);
        w.push_u8(6);
        w.push_be32(8);
        w.push_be64(123456i64 as u64);

        // column 0x07: validity_L (u32)
        write_tag(&mut w, TAG_XDS_COLUMN);
        write_u32(&mut w, 0x07);
        write_u32(&mut w, 0);

        // column 0x1c: gaze_point_2d (point2d = 2 × Q42)
        write_tag(&mut w, TAG_XDS_COLUMN);
        write_u32(&mut w, 0x1c);
        write_tag(&mut w, TAG_POINT2D);
        write_f64_q42(&mut w, 0.25);
        write_f64_q42(&mut w, 0.75);

        w.into_vec()
    }

    #[test]
    fn decodes_known_columns() {
        let payload = synthetic_payload();
        let s = GazeSample::decode(&payload).expect("decode");
        assert!(s.has(present::TIMESTAMP));
        assert_eq!(s.timestamp_us, 123456);
        assert!(s.has(present::VALIDITY_L));
        assert_eq!(s.validity_l, 0);
        assert!(s.has(present::GAZE_2D));
        assert!((s.gaze_point_2d[0] - 0.25).abs() < 1e-9);
        assert!((s.gaze_point_2d[1] - 0.75).abs() < 1e-9);
        assert!(!s.has(present::PUPIL_L));
    }

    #[test]
    fn rejects_too_short() {
        assert!(GazeSample::decode(&[0x00]).is_none());
    }

    #[test]
    fn returns_partial_sample_on_truncated_point_column() {
        // Row declares 2 columns: a complete timestamp, then an eye_origin_L
        // (0x02, point3d) whose body is truncated (prolog tag only, no values).
        // The decoder must return the partial sample with the timestamp set,
        // not drop the whole frame.
        let mut w = Writer::new();
        w.push_u8(0x00);
        w.push_u8(0x00);
        write_tag(&mut w, (2u32 << 16) | 0x0bb8);

        write_tag(&mut w, TAG_XDS_COLUMN);
        write_u32(&mut w, 0x01);
        w.push_u8(6);
        w.push_be32(8);
        w.push_be64(777i64 as u64);

        write_tag(&mut w, TAG_XDS_COLUMN);
        write_u32(&mut w, 0x02);
        write_tag(&mut w, crate::tlv::TAG_POINT3D); // prolog only — no f64 bodies follow

        let s = GazeSample::decode(&w.into_vec()).expect("partial sample");
        assert!(s.has(present::TIMESTAMP));
        assert_eq!(s.timestamp_us, 777);
        assert!(!s.has(present::EYE_ORIGIN_L)); // truncated column not populated
    }
}
