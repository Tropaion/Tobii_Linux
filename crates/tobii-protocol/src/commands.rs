//! Outbound command frame builders (v1 subset).

use crate::bytes::Writer;
use crate::frame::{build_out_frame, OP_GET_DISPLAY_AREA, OP_HELLO, OP_SET_DISPLAY_AREA, OP_SUBSCRIBE};
use crate::tlv::{write_point, write_tag, write_u32};

/// Captured 47-byte hello payload (op 0x3e8).
const HELLO_PAYLOAD: [u8; 47] = [
    0x00, 0x00, 0x17, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x09, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0x01, 0x00, 0x03, 0x00, 0x01, 0x00, 0x04, 0x00,
    0x01, 0x00, 0x05, 0x00, 0x01, 0x00, 0x06, 0x00, 0x01, 0x00, 0x07, 0x00, 0x01, 0x00, 0x08,
];

pub fn build_hello(seq: u32) -> Vec<u8> {
    build_out_frame(seq, OP_HELLO, &HELLO_PAYLOAD)
}

/// Subscribe to a TTP stream (stream_id at payload bytes 9..10, BE).
pub fn build_subscribe(seq: u32, stream_id: u16) -> Vec<u8> {
    let mut pay: [u8; 20] = [
        0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x17, 0x00, 0x00, 0x00,
        0x04, 0x00, 0x00, 0x00, 0x00,
    ];
    pay[9] = (stream_id >> 8) as u8;
    pay[10] = stream_id as u8;
    build_out_frame(seq, OP_SUBSCRIBE, &pay)
}

pub fn build_get_display_area(seq: u32) -> Vec<u8> {
    build_out_frame(seq, OP_GET_DISPLAY_AREA, &[])
}

/// Set display area from a rect in mm. `ox`/`oy` are the bottom-left offset
/// (tracker-relative); `z` is plane depth. Sends TL, TR, BL corners.
pub fn build_set_display_area(
    seq: u32,
    w_mm: f64,
    h_mm: f64,
    ox_mm: f64,
    oy_mm: f64,
    z_mm: f64,
) -> Vec<u8> {
    let x0 = ox_mm;
    let x1 = ox_mm + w_mm;
    let y0 = oy_mm;
    let y1 = oy_mm + h_mm;
    build_set_display_area_corners(
        seq,
        x0, y1, z_mm, // TL
        x1, y1, z_mm, // TR
        x0, y0, z_mm, // BL
    )
}

/// Set display area from explicit corners (each tracker-relative, mm).
#[allow(clippy::too_many_arguments)]
pub fn build_set_display_area_corners(
    seq: u32,
    tl_x: f64, tl_y: f64, tl_z: f64,
    tr_x: f64, tr_y: f64, tr_z: f64,
    bl_x: f64, bl_y: f64, bl_z: f64,
) -> Vec<u8> {
    let mut pay = Writer::new();
    pay.push_u8(0x00);
    pay.push_u8(0x00);
    write_point(&mut pay, tl_x, tl_y, tl_z);
    write_point(&mut pay, tr_x, tr_y, tr_z);
    write_point(&mut pay, bl_x, bl_y, bl_z);
    write_tag(&mut pay, 0x10100);
    write_u32(&mut pay, 0x3039);
    build_out_frame(seq, OP_SET_DISPLAY_AREA, &pay.into_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_frame_is_79_bytes() {
        let f = build_hello(1);
        assert_eq!(f.len(), 79); // envelope(8) + header(24) + payload(47)
        assert_eq!(f[0], 0x00);
        assert_eq!(f[4], 71); // LE len = 24 + 47
        assert_eq!(&f[8..12], &[0, 0, 0, 0x51]);
        assert_eq!(&f[20..24], &[0, 0, 0x03, 0xe8]);
        assert_eq!(&f[28..32], &[0, 0, 0, 47]);
        assert_eq!(f[32], 0x00);
    }

    #[test]
    fn subscribe_frame_carries_stream_id() {
        let f = build_subscribe(3, 0x500);
        assert_eq!(f.len(), 52); // 8 + 24 + 20
        assert_eq!(&f[20..24], &[0, 0, 0x04, 0xc4]);
        assert_eq!(f[41], 0x05); // stream_id hi at payload[9] -> frame 41
        assert_eq!(f[42], 0x00); // stream_id lo at payload[10] -> frame 42
    }

    #[test]
    fn get_display_area_is_empty_payload() {
        let f = build_get_display_area(4);
        assert_eq!(f.len(), 32); // 8 + 24 + 0
        assert_eq!(&f[20..24], &[0, 0, 0x05, 0x96]);
    }

    #[test]
    fn set_display_area_frame_structure() {
        let f = build_set_display_area(2, 400.0, 300.0, -200.0, 0.0, 0.0);
        assert_eq!(f.len(), 196); // payload 2 + 3*48 + 9 + 9 = 164 -> frame 196
        assert_eq!(&f[20..24], &[0, 0, 0x05, 0xa0]);
    }
}
