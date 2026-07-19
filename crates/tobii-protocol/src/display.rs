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
        r.skip(2); // 2-byte payload prefix
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

    #[test]
    fn decodes_real_device_display_area_2026_07_15() {
        // Captured from a physical Tobii ET5 (runtime 2104:0313) via `tobii display
        // get` during the Plan 4 live test, after setting a 600x335 mm screen tilted
        // 15° with its bottom edge 10 mm above the tracker. Real wire bytes (164 B).
        let payload: &[u8] = &[
            0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x04, 0x00, 0x03, 0x1f, 0x41, 0x04, 0x00, 0x00,
            0x00, 0x08, 0xff, 0xfb, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
            0x08, 0x00, 0x05, 0x36, 0x57, 0x32, 0x09, 0x06, 0x42, 0x04, 0x00, 0x00, 0x00, 0x08,
            0x00, 0x01, 0x5a, 0xd1, 0x49, 0x04, 0xf6, 0x59, 0x05, 0x00, 0x00, 0x00, 0x04, 0x00,
            0x03, 0x1f, 0x41, 0x04, 0x00, 0x00, 0x00, 0x08, 0x00, 0x04, 0xb0, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x08, 0x00, 0x05, 0x36, 0x57, 0x32, 0x09, 0x06,
            0x42, 0x04, 0x00, 0x00, 0x00, 0x08, 0x00, 0x01, 0x5a, 0xd1, 0x49, 0x04, 0xf6, 0x59,
            0x05, 0x00, 0x00, 0x00, 0x04, 0x00, 0x03, 0x1f, 0x41, 0x04, 0x00, 0x00, 0x00, 0x08,
            0xff, 0xfb, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x08, 0x00,
            0x00, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x04, 0x00, 0x01, 0x01,
            0x00, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x30, 0x39,
        ];
        let c = DisplayCorners::decode(payload).expect("real device display area decodes");
        // Matches the setup that produced it (tl.z>0: top edge tilted into +Z).
        assert!((c.tl[0] - (-300.0)).abs() < 0.1, "tl.x={}", c.tl[0]);
        assert!((c.tr[0] - 300.0).abs() < 0.1, "tr.x={}", c.tr[0]);
        assert!((c.bl[0] - (-300.0)).abs() < 0.1, "bl.x={}", c.bl[0]);
        assert!((c.bl[1] - 10.0).abs() < 0.1, "bl.y={}", c.bl[1]);
        assert!((c.tl[1] - 333.585).abs() < 0.2, "tl.y={}", c.tl[1]);
        assert!((c.tl[2] - 86.704).abs() < 0.2, "tl.z={}", c.tl[2]);
        assert!((c.bl[2] - 0.0).abs() < 0.1, "bl.z={}", c.bl[2]);
        assert!((c.tr[1] - c.tl[1]).abs() < 1e-9 && (c.tr[2] - c.tl[2]).abs() < 1e-9); // level top edge
        assert!((c.tr[0] - c.tl[0] - 600.0).abs() < 0.1); // width
    }
}
