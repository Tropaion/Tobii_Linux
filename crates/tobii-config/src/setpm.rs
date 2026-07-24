//! Decoder for Tobii's `.setpm` screen-plane capture (ground-truth display area).
//!
//! Layout (little-endian): u32 version (=4), u32 count (=1), u32 payload_len (=36),
//! then 9× f32 = three tracker-space corners in order BL, TL, TR (millimetres).

use tobii_protocol::DisplayCorners;

fn le_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn le_f32(b: &[u8], off: usize) -> f64 {
    f32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]) as f64
}

/// Parse a `.setpm` screen-plane capture into tracker-space corners.
/// Returns `None` if the header/length is not the expected 3-corner plane.
pub fn parse_setpm_corners(bytes: &[u8]) -> Option<DisplayCorners> {
    if bytes.len() < 12 {
        return None;
    }
    let version = le_u32(bytes, 0);
    let payload_len = le_u32(bytes, 8) as usize;
    // Expect version 4 and a 9× f32 (36-byte) payload following the 12-byte header.
    if version != 4 || payload_len != 36 || bytes.len() < 12 + 36 {
        return None;
    }
    let f = |i: usize| le_f32(bytes, 12 + i * 4);
    Some(DisplayCorners {
        bl: [f(0), f(1), f(2)],
        tl: [f(3), f(4), f(5)],
        tr: [f(6), f(7), f(8)],
    })
}

#[cfg(test)]
mod tests {
    use super::parse_setpm_corners;

    const SCREENPLANE: &[u8] = include_bytes!("testdata/screenplane.setpm");

    #[test]
    fn decodes_tobii_screenplane_corners() {
        let c = parse_setpm_corners(SCREENPLANE).expect("valid setpm");
        // Captured ground truth for the Samsung Odyssey G93SC (49" 1800R).
        let approx = |a: f64, b: f64| (a - b).abs() < 0.05;
        assert!(
            approx(c.bl[0], -596.5) && approx(c.bl[1], 10.27) && approx(c.bl[2], -3.10),
            "bl={:?}",
            c.bl
        );
        assert!(
            approx(c.tl[0], -596.5) && approx(c.tl[1], 325.56) && approx(c.tl[2], 111.66),
            "tl={:?}",
            c.tl
        );
        assert!(
            approx(c.tr[0], 596.5) && approx(c.tr[1], 325.56) && approx(c.tr[2], 111.66),
            "tr={:?}",
            c.tr
        );
    }

    #[test]
    fn rejects_short_or_bad_header() {
        assert!(parse_setpm_corners(&[]).is_none());
        assert!(parse_setpm_corners(&[0u8; 20]).is_none()); // header ok-ish but no payload
    }
}
