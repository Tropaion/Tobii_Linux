//! Realm (authentication) command builders. The HMAC-MD5 of the device
//! challenge is computed with [`crate::md5::hmac_md5`] and the [`REALM_KEY`].

use crate::bytes::Writer;
use crate::frame::{build_out_frame, OP_CLOSE_REALM, OP_OPEN_REALM, OP_QUERY_REALM, OP_REALM_RESPONSE};
use crate::tlv::write_u32;

/// The realm HMAC key (16 ASCII chars + trailing NUL), as used on the wire.
pub const REALM_KEY: &[u8; 17] = b"IS2LJC6GIRBBEK2K\x00";

pub fn build_query_realm(seq: u32) -> Vec<u8> {
    build_out_frame(seq, OP_QUERY_REALM, &[0x00, 0x00])
}

pub fn build_open_realm(seq: u32, realm_type: u32) -> Vec<u8> {
    let mut pay = Writer::new();
    pay.push_u8(0x00);
    pay.push_u8(0x00);
    write_u32(&mut pay, realm_type);
    pay.push_u8(0x00); // 1-byte choice = 0 (raw, no TLV header)
    build_out_frame(seq, OP_OPEN_REALM, &pay.into_vec())
}

pub fn build_realm_response(seq: u32, realm_id: u32, field_210: u32, digest: &[u8; 16]) -> Vec<u8> {
    let mut pay = Writer::new();
    pay.push_u8(0x00);
    pay.push_u8(0x00);
    write_u32(&mut pay, realm_id);
    write_u32(&mut pay, field_210);
    pay.push_bytes(digest);
    build_out_frame(seq, OP_REALM_RESPONSE, &pay.into_vec())
}

pub fn build_close_realm(seq: u32, realm_id: u32) -> Vec<u8> {
    let mut pay = Writer::new();
    pay.push_u8(0x00);
    pay.push_u8(0x00);
    write_u32(&mut pay, realm_id);
    build_out_frame(seq, OP_CLOSE_REALM, &pay.into_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_realm_frame() {
        let f = build_query_realm(5);
        assert_eq!(f.len(), 34); // 8 + 24 + 2
        assert_eq!(&f[20..24], &[0, 0, 0x06, 0x40]);
    }

    #[test]
    fn open_realm_frame() {
        let f = build_open_realm(5, 1);
        assert_eq!(f.len(), 44); // 8 + 24 + (2 + 9 + 1)
        assert_eq!(&f[20..24], &[0, 0, 0x07, 0x6c]);
        assert_eq!(f[34], 0x02); // TLV type=2
        assert_eq!(f[42], 0x01); // realm_type LSB
        assert_eq!(f[43], 0x00); // choice
    }

    #[test]
    fn realm_response_frame() {
        let digest = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let f = build_realm_response(5, 42, 7, &digest);
        assert_eq!(f.len(), 68); // 8 + 24 + (2 + 9 + 9 + 16)
        assert_eq!(&f[20..24], &[0, 0, 0x07, 0x76]);
        assert_eq!(f[52], 1); // digest start at frame offset 52
        assert_eq!(f[67], 16);
    }

    #[test]
    fn close_realm_frame() {
        let f = build_close_realm(5, 42);
        assert_eq!(f.len(), 43); // 8 + 24 + (2 + 9)
        assert_eq!(&f[20..24], &[0, 0, 0x07, 0x7b]);
    }

    #[test]
    fn realm_key_is_seventeen_bytes() {
        assert_eq!(REALM_KEY.len(), 17);
        assert_eq!(REALM_KEY[16], 0x00);
    }
}
