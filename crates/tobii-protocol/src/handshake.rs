//! ET5 connection handshake state machine (pure, transport-agnostic).
//!
//! Drive it from a transport loop:
//!   let mut hs = Handshake::new(STREAM_GAZE);
//!   loop {
//!       match hs.poll() {
//!           HandshakeAction::Send(bytes) => { transport.send(&bytes); /* then read
//!               responses and call hs.on_response(payload) for each */ }
//!           HandshakeAction::Recv => { /* read more, call hs.on_response(...) */ }
//!           HandshakeAction::Done => break,
//!           HandshakeAction::Failed => return Err(...),
//!       }
//!   }
//!
//! Handshake responses use a looser TLV walk than requests: each field header
//! is [type:u8][pad:u8][size:u16 BE] followed by `size` body bytes.

use crate::commands::{build_hello, build_subscribe};
use crate::md5::hmac_md5;
use crate::realm::{build_open_realm, build_query_realm, build_realm_response, REALM_KEY};

fn resp_u16_be(data: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([data[at], data[at + 1]])
}

/// First `size==4` field's u32 value, or 0 if none.
pub(crate) fn resp_first_u32(data: &[u8]) -> u32 {
    let mut pos = 2usize; // skip 2-byte prefix
    while pos + 4 <= data.len() {
        let size = resp_u16_be(data, pos + 2) as usize;
        pos += 4;
        if size == 4 && pos + 4 <= data.len() {
            return u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        }
        pos += size;
    }
    0
}

/// The `index`-th `size==4` field's u32 value, or 0 if out of range.
pub(crate) fn resp_u32_at(data: &[u8], index: usize) -> u32 {
    let mut pos = 2usize;
    let mut found = 0usize;
    while pos + 4 <= data.len() {
        let size = resp_u16_be(data, pos + 2) as usize;
        pos += 4;
        if size == 4 && pos + 4 <= data.len() {
            if found == index {
                return u32::from_be_bytes([
                    data[pos],
                    data[pos + 1],
                    data[pos + 2],
                    data[pos + 3],
                ]);
            }
            found += 1;
        }
        pos += size;
    }
    0
}

/// First field whose `size > 4` — the realm challenge — or None.
pub(crate) fn resp_extract_challenge(data: &[u8]) -> Option<&[u8]> {
    let mut pos = 2usize;
    while pos + 4 <= data.len() {
        let size = resp_u16_be(data, pos + 2) as usize;
        pos += 4;
        if size > 4 && pos + size <= data.len() {
            return Some(&data[pos..pos + size]);
        }
        pos += size;
    }
    None
}

#[cfg(test)]
mod resp_tests {
    use super::*;

    // A response field: [type=0x02][pad=0x00][size:u16 BE][body].
    fn u32_field(v: u32) -> Vec<u8> {
        let mut f = vec![0x02, 0x00, 0x00, 0x04];
        f.extend_from_slice(&v.to_be_bytes());
        f
    }
    fn blob_field(body: &[u8]) -> Vec<u8> {
        let mut f = vec![0x02, 0x00];
        f.extend_from_slice(&(body.len() as u16).to_be_bytes());
        f.extend_from_slice(body);
        f
    }

    #[test]
    fn first_u32_reads_first_size4_field() {
        let mut p = vec![0x00, 0x00];
        p.extend(u32_field(0x2a));
        p.extend(u32_field(0x99));
        assert_eq!(resp_first_u32(&p), 0x2a);
    }

    #[test]
    fn first_u32_is_zero_when_none() {
        let p = vec![0x00, 0x00];
        assert_eq!(resp_first_u32(&p), 0);
    }

    #[test]
    fn u32_at_indexes_size4_fields() {
        let mut p = vec![0x00, 0x00];
        p.extend(u32_field(0x11));
        p.extend(u32_field(0x22));
        assert_eq!(resp_u32_at(&p, 0), 0x11);
        assert_eq!(resp_u32_at(&p, 1), 0x22);
        assert_eq!(resp_u32_at(&p, 2), 0);
    }

    #[test]
    fn extract_challenge_returns_first_long_field() {
        let mut p = vec![0x00, 0x00];
        p.extend(u32_field(0x5));
        p.extend(blob_field(&[0xaa; 16]));
        assert_eq!(resp_extract_challenge(&p), Some(&[0xaa; 16][..]));
    }

    #[test]
    fn extract_challenge_none_when_all_short() {
        let mut p = vec![0x00, 0x00];
        p.extend(u32_field(0x5));
        assert_eq!(resp_extract_challenge(&p), None);
    }
}
