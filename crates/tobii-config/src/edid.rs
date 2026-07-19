//! Monitor EDID parsing: physical size (mm) + model, from `/sys/class/drm/*/edid`.
//!
//! We read only the 128-byte base block. Physical size comes from the first
//! Detailed Timing Descriptor's image size (mm), falling back to the basic
//! display block's cm field. Pure `parse_edid`; `detect_monitors` does the I/O.

use std::path::Path;

/// A detected monitor's model name and physical active-area size (mm).
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub model: String,
    pub width_mm: f64,
    pub height_mm: f64,
}

const EDID_HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];

/// Parse an EDID base block into [`MonitorInfo`]. Returns `None` if the header
/// is invalid, the block is too short, or no physical size can be determined.
pub fn parse_edid(edid: &[u8]) -> Option<MonitorInfo> {
    if edid.len() < 128 || edid[0..8] != EDID_HEADER {
        return None;
    }

    // Physical size (mm): prefer the first Detailed Timing Descriptor at byte 54.
    // A descriptor is a DTD when its pixel-clock (bytes 54..56) is non-zero;
    // its image size mm is bytes 66 (h low), 67 (v low), 68 (upper nibbles).
    let (mut w_mm, mut h_mm) = (0u32, 0u32);
    if edid[54] != 0 || edid[55] != 0 {
        w_mm = edid[66] as u32 | (((edid[68] >> 4) as u32) << 8);
        h_mm = edid[67] as u32 | (((edid[68] & 0x0f) as u32) << 8);
    }
    // Fallback: basic display block cm (bytes 21, 22) -> mm.
    if w_mm == 0 || h_mm == 0 {
        w_mm = edid[21] as u32 * 10;
        h_mm = edid[22] as u32 * 10;
    }
    if w_mm == 0 || h_mm == 0 {
        return None;
    }

    // Model name: the descriptor tagged 0xFC (bytes 0..3 == 00 00 00, byte 3 == FC).
    let mut model = String::new();
    for &off in &[54usize, 72, 90, 108] {
        let d = &edid[off..off + 18];
        if d[0] == 0 && d[1] == 0 && d[2] == 0 && d[3] == 0xfc {
            model = d[5..18]
                .iter()
                .take_while(|&&b| b != 0x0a)
                .map(|&b| b as char)
                .collect::<String>()
                .trim()
                .to_string();
            break;
        }
    }

    Some(MonitorInfo {
        model,
        width_mm: w_mm as f64,
        height_mm: h_mm as f64,
    })
}

/// Read every `/sys/class/drm/*/edid` and parse the ones that are valid.
pub fn detect_monitors() -> Vec<MonitorInfo> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(Path::new("/sys/class/drm")) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path().join("edid");
        if let Ok(bytes) = std::fs::read(&path) {
            if let Some(info) = parse_edid(&bytes) {
                out.push(info);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_odyssey_g93sc() {
        let bytes = include_bytes!("testdata/odyssey-g93sc.edid");
        let m = parse_edid(bytes).expect("valid EDID parses");
        assert_eq!(m.model, "Odyssey G93SC");
        assert!((m.width_mm - 1193.0).abs() < 1.0, "width_mm={}", m.width_mm);
        assert!(
            (m.height_mm - 336.0).abs() < 1.0,
            "height_mm={}",
            m.height_mm
        );
    }

    #[test]
    fn rejects_bad_header_and_short_input() {
        assert!(parse_edid(&[0u8; 128]).is_none()); // header all zero
        assert!(parse_edid(&[0xff; 10]).is_none()); // too short
    }
}
