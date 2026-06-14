//! Display-area decoding. The get_display_area response payload matches the
//! set wire format: [00 00][point TL][point TR][point BL][...].

use crate::tlv::Reader;

/// The three display-area corners reported by the device (tracker-space mm).
/// The bottom-right corner is implied (not sent on the wire).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DisplayCorners {
    pub tl: [f64; 3],
    pub tr: [f64; 3],
    pub bl: [f64; 3],
}

impl DisplayCorners {
    /// Decode a display-area payload. Returns `None` if it does not parse.
    pub fn decode(payload: &[u8]) -> Option<DisplayCorners> {
        if payload.len() < 2 {
            return None;
        }
        let mut r = Reader::new(payload);
        r.pos = 2;
        let tl = r.read_point3d().ok()?;
        let tr = r.read_point3d().ok()?;
        let bl = r.read_point3d().ok()?;
        Some(DisplayCorners { tl, tr, bl })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::Writer;
    use crate::tlv::write_point;

    #[test]
    fn decodes_three_corners() {
        let mut w = Writer::new();
        w.push_u8(0x00);
        w.push_u8(0x00);
        write_point(&mut w, -200.0, 150.0, 0.0); // TL
        write_point(&mut w, 200.0, 150.0, 0.0); // TR
        write_point(&mut w, -200.0, -150.0, 0.0); // BL
        let buf = w.into_vec();

        let c = DisplayCorners::decode(&buf).expect("decode");
        assert!((c.tl[0] + 200.0).abs() < 1e-9);
        assert!((c.tr[0] - 200.0).abs() < 1e-9);
        assert!((c.bl[1] + 150.0).abs() < 1e-9);
    }

    #[test]
    fn rejects_short_payload() {
        assert!(DisplayCorners::decode(&[0x00, 0x00]).is_none());
    }
}
