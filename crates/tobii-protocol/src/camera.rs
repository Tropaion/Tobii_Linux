//! Decode the ET5 eye-camera image streams (subscribe ids `0x501` / `0x50e`).
//!
//! The ET5 has **two near-infrared cameras** (a stereo pair). Each streams
//! full-face wide-angle images at ~33 Hz. Live-decoded 2026-07-23 from real
//! captures: `280 × 280`, 8-bit grayscale, ~78 KB per frame.
//!
//! The payload uses the same XDS/TLV column framing as the gaze stream, but the
//! columns are image metadata + a pixel blob rather than gaze data:
//!
//! | col id | type | meaning                         |
//! |--------|------|---------------------------------|
//! | `0x01` | s64  | timestamp (µs)                  |
//! | `0x02` | u32  | bit depth (8)                   |
//! | `0x03` | u32  | width                           |
//! | `0x04` | u32  | height                          |
//! | `0x05` | blob | 4-byte prefix + `w*h` pixels    |
//!
//! Each TLV element is `[type: u8][len: u32 BE][value: len bytes]`. The row and
//! column markers are the same `0x0bb8` / `0x0bb9` tags the gaze stream uses.
//! This is a clean-room decode of the observed wire format.

/// A decoded camera frame: 8-bit grayscale pixels plus geometry + timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraFrame {
    pub timestamp_us: i64,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u32,
    /// `width * height` grayscale samples, row-major, top-left origin.
    pub pixels: Vec<u8>,
}

/// Number of pixel bytes that precede the image inside the `0x05` blob (a small
/// header observed as `00 01 ..`; its fields are not yet interpreted).
const BLOB_PREFIX: usize = 4;

const TYPE_U32: u8 = 0x02;
const TYPE_S64: u8 = 0x06;
const TYPE_BLOB: u8 = 0x15;

/// One TLV element `[type][len:BE32][value]`, or `None` if truncated.
fn next_tlv(buf: &[u8], at: usize) -> Option<(u8, &[u8], usize)> {
    let t = *buf.get(at)?;
    let len_bytes = buf.get(at + 1..at + 5)?;
    let len = u32::from_be_bytes(len_bytes.try_into().ok()?) as usize;
    let start = at + 5;
    let value = buf.get(start..start + len)?;
    Some((t, value, start + len))
}

/// The XDS marker element type, and its column/row tag values (low 16 bits).
const TYPE_MARKER: u8 = 0x05;
const TAG_COLUMN: u32 = 0x0bb9;
const TAG_ROW: u32 = 0x0bb8;

/// Decode a `0x501` / `0x50e` camera-stream payload into a [`CameraFrame`].
/// Returns `None` if the framing is malformed or the pixel count does not match
/// `width * height` (8-bit assumed).
pub fn decode_camera_frame(payload: &[u8]) -> Option<CameraFrame> {
    // Walk the TLV stream marker-aware. Each column is `[marker][id: u32][value]`
    // where BOTH the id and a u32 value share `type 0x02` — so the first `0x02`
    // after a marker is the column id and the following element is its value.
    // Collect only the VALUES (timestamp s64, metadata u32s, the pixel blob);
    // discard the ids and the structural markers.
    let mut i = 2usize;
    let mut timestamp_us = 0i64;
    let mut values: Vec<u32> = Vec::new();
    let mut blob: Option<&[u8]> = None;
    let mut awaiting_id = false;

    while let Some((t, v, next)) = next_tlv(payload, i) {
        i = next;
        if t == TYPE_MARKER && v.len() == 4 {
            let tag = u32::from_be_bytes(v.try_into().ok()?) & 0xffff;
            if tag == TAG_COLUMN || tag == TAG_ROW {
                awaiting_id = true; // the next 0x02 is this column's id
            }
            continue;
        }
        if awaiting_id && t == TYPE_U32 && v.len() == 4 {
            awaiting_id = false; // consume the column id, keep its value(s) below
            continue;
        }
        match t {
            TYPE_S64 if v.len() == 8 => timestamp_us = i64::from_be_bytes(v.try_into().ok()?),
            TYPE_U32 if v.len() == 4 => values.push(u32::from_be_bytes(v.try_into().ok()?)),
            TYPE_BLOB if v.len() > BLOB_PREFIX => blob = Some(v),
            _ => {}
        }
    }

    let pixels = &blob?[BLOB_PREFIX..];
    let (width, height) = find_dims(&values, pixels.len())?;
    // Metadata values are [bit_depth, width, height]; the bit depth is the one
    // that is not a dimension. Only 8-bit has been observed.
    let bit_depth = values
        .iter()
        .copied()
        .find(|&n| n != width && n != height && n <= 32)
        .unwrap_or(8);

    Some(CameraFrame {
        timestamp_us,
        width,
        height,
        bit_depth,
        pixels: pixels.to_vec(),
    })
}

/// Find `(width, height)` among the observed u32 values whose product equals the
/// pixel count. Prefers the last matching pair (metadata sits after the small
/// column-id values), so a stray `1*n == n` coincidence early on can't win.
fn find_dims(vals: &[u32], pixel_count: usize) -> Option<(u32, u32)> {
    let mut best: Option<(u32, u32)> = None;
    for (a, &w) in vals.iter().enumerate() {
        for &h in &vals[a + 1..] {
            if w >= 8 && h >= 8 && (w as usize) * (h as usize) == pixel_count {
                best = Some((w, h));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal camera payload in the real wire shape: prefix, a row
    /// marker, then per-column `[marker][id][value]` triples (both id and a u32
    /// value are `type 0x02`), matching the live 0x501/0x50e framing.
    fn synth_frame(w: u32, h: u32, fill: u8) -> Vec<u8> {
        fn tlv(out: &mut Vec<u8>, t: u8, v: &[u8]) {
            out.push(t);
            out.extend_from_slice(&(v.len() as u32).to_be_bytes());
            out.extend_from_slice(v);
        }
        fn column(out: &mut Vec<u8>, id: u32, vtype: u8, value: &[u8]) {
            tlv(out, TYPE_MARKER, &(0x0002_0000 | TAG_COLUMN).to_be_bytes());
            tlv(out, TYPE_U32, &id.to_be_bytes());
            tlv(out, vtype, value);
        }
        let mut p = vec![0x00, 0x00];
        tlv(&mut p, TYPE_MARKER, &(0x0005_0000 | TAG_ROW).to_be_bytes()); // 5-col row
        column(&mut p, 1, TYPE_S64, &42i64.to_be_bytes()); // timestamp
        column(&mut p, 2, TYPE_U32, &8u32.to_be_bytes()); // bit depth
        column(&mut p, 3, TYPE_U32, &w.to_be_bytes()); // width
        column(&mut p, 4, TYPE_U32, &h.to_be_bytes()); // height
        let mut blob = vec![0x00, 0x01, 0x00, 0x00]; // 4-byte blob prefix
        blob.extend(std::iter::repeat_n(fill, (w * h) as usize));
        column(&mut p, 5, TYPE_BLOB, &blob); // image
        p
    }

    #[test]
    fn decodes_a_synthetic_camera_frame() {
        let f = decode_camera_frame(&synth_frame(280, 280, 0x1f)).expect("decode");
        assert_eq!((f.width, f.height), (280, 280));
        assert_eq!(f.bit_depth, 8);
        assert_eq!(f.timestamp_us, 42);
        assert_eq!(f.pixels.len(), 280 * 280);
        assert!(f.pixels.iter().all(|&b| b == 0x1f));
    }

    #[test]
    fn dims_matched_by_pixel_product_not_column_id() {
        // Column ids 1..4 are present as u32s; only 280*280 matches the blob.
        let f = decode_camera_frame(&synth_frame(280, 280, 0)).unwrap();
        assert_eq!((f.width, f.height), (280, 280));
    }

    #[test]
    fn rejects_pixel_count_mismatch() {
        // Truncate the blob so width*height no longer matches.
        let mut p = synth_frame(280, 280, 0);
        p.truncate(p.len() - 100);
        assert!(decode_camera_frame(&p).is_none());
    }

    #[test]
    fn non_square_frame_dims_are_ordered() {
        let f = decode_camera_frame(&synth_frame(320, 240, 7)).unwrap();
        assert_eq!((f.width, f.height), (320, 240));
        assert_eq!(f.pixels.len(), 320 * 240);
    }
}
