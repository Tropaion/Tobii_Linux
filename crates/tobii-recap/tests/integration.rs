//! End-to-end: synthesize a usbmon-mmapped pcap in memory and assert the
//! decoder recovers the frames, directions, ops, and catalog.

use tobii_protocol::frame::{
    build_out_frame, ENVELOPE_SIZE, OP_CAL_START, OP_GAZE_NOTIFY, OP_HELLO, OP_SUBSCRIBE,
    TTP_HDR_SIZE, TTP_MAGIC_NOTIFY, TTP_MAGIC_RSP,
};
use tobii_recap::catalog;
use tobii_recap::decode::decode;

/// Global header. `big` selects endianness; the magic bytes follow suit.
fn global(big: bool, linktype: u32) -> Vec<u8> {
    let mut v = if big {
        vec![0xa1, 0xb2, 0xc3, 0xd4]
    } else {
        vec![0xd4, 0xc3, 0xb2, 0xa1]
    };
    let put16 = |v: &mut Vec<u8>, x: u16| {
        if big {
            v.extend_from_slice(&x.to_be_bytes())
        } else {
            v.extend_from_slice(&x.to_le_bytes())
        }
    };
    let put32 = |v: &mut Vec<u8>, x: u32| {
        if big {
            v.extend_from_slice(&x.to_be_bytes())
        } else {
            v.extend_from_slice(&x.to_le_bytes())
        }
    };
    put16(&mut v, 2);
    put16(&mut v, 4);
    put32(&mut v, 0);
    put32(&mut v, 0);
    put32(&mut v, 65535);
    put32(&mut v, linktype);
    v
}

/// One pcap record wrapping a 64-byte usbmon (mmapped) bulk transfer.
fn record(big: bool, ts_sec: u32, event: u8, epnum: u8, payload: &[u8]) -> Vec<u8> {
    let put32 = |v: &mut Vec<u8>, x: u32| {
        if big {
            v.extend_from_slice(&x.to_be_bytes())
        } else {
            v.extend_from_slice(&x.to_le_bytes())
        }
    };

    let mut usb = vec![0u8; 64];
    usb[8] = event;
    usb[9] = 3; // BULK
    usb[10] = epnum;
    // len_cap at offset 36, always in the file's byte order.
    if big {
        usb[36..40].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    } else {
        usb[36..40].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    }
    usb.extend_from_slice(payload);

    let mut rec = Vec::new();
    put32(&mut rec, ts_sec);
    put32(&mut rec, 500_000); // ts_frac (half a second)
    put32(&mut rec, usb.len() as u32);
    put32(&mut rec, usb.len() as u32);
    rec.extend_from_slice(&usb);
    rec
}

/// Inbound TTP envelope: [01 00 00 00][len incl envelope][ttp hdr][payload].
fn inbound(magic: u32, seq: u32, op: u32, payload: &[u8]) -> Vec<u8> {
    let total = (ENVELOPE_SIZE + TTP_HDR_SIZE + payload.len()) as u32;
    let mut v = vec![0x01, 0, 0, 0];
    v.extend_from_slice(&total.to_le_bytes());
    v.extend_from_slice(&magic.to_be_bytes());
    v.extend_from_slice(&seq.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&op.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    v.extend_from_slice(payload);
    v
}

/// A minimal but real-shaped 0x500 gaze payload: 2-byte prefix, one column
/// (timestamp). Enough for `column_inventory` to report a column count.
fn gaze_payload() -> Vec<u8> {
    // xds_row header: (n_cols << 16) | 0x0bb8, as a 4-byte type tag.
    // column 0x01 = timestamp: xds_column tag, col id, type=6, size=8, i64.
    // Each TLV field header is [type:u8][size:u32 BE] (5 bytes), then the body.
    let mut p = vec![0x00, 0x00];
    // xds_row prolog (type=5, size=4): tag (n_cols<<16)|0x0bb8, n_cols=1.
    p.push(0x05);
    p.extend_from_slice(&4u32.to_be_bytes());
    p.extend_from_slice(&((1u32 << 16) | 0x0bb8).to_be_bytes());
    // xds_column prolog (type=5, size=4): tag TAG_XDS_COLUMN = 0x020bb9.
    p.push(0x05);
    p.extend_from_slice(&4u32.to_be_bytes());
    p.extend_from_slice(&0x020bb9u32.to_be_bytes());
    // column id 0x01 as a u32 field (type=2, size=4).
    p.push(0x02);
    p.extend_from_slice(&4u32.to_be_bytes());
    p.extend_from_slice(&1u32.to_be_bytes());
    // s64 value (type=6, size=8), then 8 bytes big-endian.
    p.push(0x06);
    p.extend_from_slice(&8u32.to_be_bytes());
    p.extend_from_slice(&123_456i64.to_be_bytes());
    p
}

fn build_capture(big: bool) -> Vec<u8> {
    let mut file = global(big, 220);
    // Handshake-ish traffic + a calibration request + a gaze notify + an
    // UNKNOWN op (0x999) so the catalog has an RE target to sort to the top.
    file.extend(record(
        big,
        0,
        b'S',
        0x02,
        &build_out_frame(1, OP_HELLO, &[]),
    ));
    file.extend(record(
        big,
        1,
        b'C',
        0x81,
        &inbound(TTP_MAGIC_RSP, 1, OP_HELLO, &[0x00, 0x00]),
    ));
    file.extend(record(
        big,
        2,
        b'S',
        0x02,
        &build_out_frame(2, OP_SUBSCRIBE, &[0x05, 0x00]),
    ));
    file.extend(record(
        big,
        3,
        b'S',
        0x02,
        &build_out_frame(3, OP_CAL_START, &[]),
    ));
    file.extend(record(
        big,
        4,
        b'S',
        0x02,
        &build_out_frame(4, 0x999, &[0xAB]),
    ));
    file.extend(record(
        big,
        5,
        b'C',
        0x81,
        &inbound(TTP_MAGIC_NOTIFY, 0, OP_GAZE_NOTIFY, &gaze_payload()),
    ));
    file
}

#[test]
fn decodes_a_synthetic_capture_little_endian() {
    let file = build_capture(false);
    let r = decode(&file).expect("decode");
    assert_eq!(r.linktype, 220);
    assert_eq!(r.packet_count, 6);
    // 4 OUT frames + 2 IN frames.
    assert_eq!(r.frames.len(), 6);
    assert!(r.errors.is_empty(), "unexpected errors: {:?}", r.errors);

    let ops: Vec<u32> = r.frames.iter().map(|f| f.frame.op).collect();
    assert_eq!(
        ops,
        vec![
            OP_HELLO,
            OP_HELLO,
            OP_SUBSCRIBE,
            OP_CAL_START,
            0x999,
            OP_GAZE_NOTIFY
        ]
    );

    // Directions: OUT first hello, IN hello resp, then OUT×3, IN gaze.
    let dirs: Vec<bool> = r.frames.iter().map(|f| f.dir_in).collect();
    assert_eq!(dirs, vec![false, true, false, false, false, true]);

    // The gaze notify's payload carries a decodable column.
    let gaze = r.frames.last().unwrap();
    assert_eq!(gaze.frame.op, OP_GAZE_NOTIFY);
    let inv = tobii_protocol::gaze::column_inventory(&gaze.frame.payload);
    assert_eq!(inv.len(), 1);
}

#[test]
fn endianness_detection_matches_little_endian() {
    let le = decode(&build_capture(false)).expect("le decode");
    let be = decode(&build_capture(true)).expect("be decode");
    // Both byte orders must recover the same op sequence.
    let le_ops: Vec<u32> = le.frames.iter().map(|f| f.frame.op).collect();
    let be_ops: Vec<u32> = be.frames.iter().map(|f| f.frame.op).collect();
    assert_eq!(le_ops, be_ops);
    assert_eq!(le.packet_count, be.packet_count);
}

#[test]
fn catalog_flags_unknown_op_first_and_aggregates() {
    let r = decode(&build_capture(false)).expect("decode");
    let cat = catalog::build(&r.frames);
    // The unknown op 0x999 sorts to the top.
    assert_eq!(cat[0].op, 0x999);
    assert!(cat[0].name.is_none());

    // HELLO appears twice, both directions.
    let hello = cat.iter().find(|s| s.op == OP_HELLO).unwrap();
    assert_eq!(hello.count, 2);
    assert_eq!(hello.dir_marker(), "<>");
    assert_eq!(hello.min_len, 0); // request payload was empty
    assert_eq!(hello.max_len, 2); // response payload was 2 bytes
}
