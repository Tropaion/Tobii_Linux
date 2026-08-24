//! TTP framing and the outbound USB envelope.
//!
//! Outbound wire format (host → device):
//!   [dir=0x00][0 0 0][len_LE:u32 = ttp_len][ttp_header:24 BE][payload]
//! The OUT envelope's length field excludes the 8-byte envelope itself.
//!
//! TTP header (24 bytes, big-endian):
//!   [magic:u32][seq:u32][flag:u32=0][op:u32][0:u32][plen:u32]

use crate::bytes::Writer;

pub const TTP_HDR_SIZE: usize = 24;
pub const ENVELOPE_SIZE: usize = 8;

pub const TTP_MAGIC_REQ: u32 = 0x51;
pub const TTP_MAGIC_RSP: u32 = 0x52;
pub const TTP_MAGIC_NOTIFY: u32 = 0x53;

// Operation codes used in v1 (calibration ops deferred to Phase 2).
pub const OP_HELLO: u32 = 0x3e8;
pub const OP_SUBSCRIBE: u32 = 0x4c4;
/// Stop a stream started with [`OP_SUBSCRIBE`]. Every mapped request/companion
/// pair in this catalog sits exactly +10 apart (`0x596`/`0x5a0`, `0x3f2`/`0x3fc`,
/// `0x44c`/`0x456`, `0x76c`/`0x776`, `0xc58`/`0xc62`), and
/// `0x4c4 + 10 = 0x4ce`, which is also what `njmill/tobii-linux` sends to stop a
/// stream. **[UNCONFIRMED]** — the pattern and the third-party use agree, but we
/// have not watched a stream actually fall silent after sending it.
pub const OP_UNSUBSCRIBE: u32 = 0x4ce;
/// Ask the device to enumerate its own streams (id → name).
/// **[UNCONFIRMED]** — the op number comes from a third-party middleware
/// emulator's canned reply, not from a capture of real hardware. `tobii streams`
/// exists to settle it.
pub const OP_STREAM_CATALOG: u32 = 0x4b0;
pub const OP_SET_DISPLAY_AREA: u32 = 0x5a0;
pub const OP_GET_DISPLAY_AREA: u32 = 0x596;
pub const OP_QUERY_REALM: u32 = 0x640;
pub const OP_OPEN_REALM: u32 = 0x76c;
pub const OP_REALM_RESPONSE: u32 = 0x776;
pub const OP_CLOSE_REALM: u32 = 0x77b;

// "Select eyes to detect" — the enabled_eye platmod property (Spike S4,
// op codes code-verified in the native lib). Wire enum is 1-based: 1=LEFT,
// 2=RIGHT, 3=BOTH (= the C-API enum + 1). NOTE: SET is *lower* than GET.
pub const OP_GET_ENABLED_EYE: u32 = 0xc62;
pub const OP_SET_ENABLED_EYE: u32 = 0xc58;

// Calibration ops (Phase 2). Session control (start/stop/clear) op codes were
// code-verified in the native lib (B3 research): each is a no-arg wrapper over
// the common TTP frame builder. The device enters calibration mode on `start`
// and leaves on `stop`; `clear` discards collected/active calibration data.
pub const OP_CAL_START: u32 = 0x3f2;
pub const OP_CAL_STOP: u32 = 0x3fc;
pub const OP_CAL_CLEAR: u32 = 0x424;
/// `CALIBRATE_POINT_ADD2D`. **Not** `0x408` (`CALIBRATE_POINT_ADD_EYE`), which
/// this used until 2026-08-15: the device acknowledges points sent to `0x408`
/// and then discards them, so a whole calibration ran clean and changed
/// nothing. Payload shape is identical between the two, which is why it went
/// unnoticed for so long. Cross-checked against `ChrisVeigl/tobiifree`, who
/// found it independently on macOS and measured 0.69 deg afterwards against
/// the vendor stack's 0.70 deg on the same device.
///
/// **[HYPOTHESIS]** on *this* hardware: strong, but the confirming measurement
/// has not been taken here. `finish_calibration` logs the compute duration and
/// the blob's size and digest — one real run settles it. Over a second with a
/// changed blob confirms; ~230 ms with an identical blob refutes.
pub const OP_CAL_ADD_POINT: u32 = 0x406;
pub const OP_CAL_DISCARD_POINT: u32 = 0x438; // redo a point
/// `CALIBRATE_POINTS_APPLY` — compute the model from the collected points and
/// apply it. **Not** `0x42f`, which is `CALIBRATE_EYE_APPLY`, a different
/// operation that leaves the model untouched.
///
/// The tell is timing: a real computation takes well over a second, while
/// `0x42f` returns in ~230 ms having done nothing. See [`OP_CAL_ADD_POINT`],
/// including why this is **[HYPOTHESIS]** here and not yet [CONFIRMED].
pub const OP_CAL_COMPUTE: u32 = 0x42e;
pub const OP_CAL_RETRIEVE: u32 = 0x44c;
pub const OP_CAL_APPLY: u32 = 0x456;

/// The gaze notification stream id and its op code.
pub const STREAM_GAZE: u16 = 0x500;
pub const OP_GAZE_NOTIFY: u32 = 0x500;

/// Build a request TTP frame and wrap it in the outbound USB envelope.
pub fn build_out_frame(seq: u32, op: u32, payload: &[u8]) -> Vec<u8> {
    let mut ttp = Writer::with_capacity(TTP_HDR_SIZE + payload.len());
    ttp.push_be32(TTP_MAGIC_REQ);
    ttp.push_be32(seq);
    ttp.push_be32(0); // flag
    ttp.push_be32(op);
    ttp.push_be32(0);
    ttp.push_be32(payload.len() as u32);
    ttp.push_bytes(payload);
    let ttp = ttp.into_vec();

    let mut out = Writer::with_capacity(ENVELOPE_SIZE + ttp.len());
    out.push_be32(0); // dir=0x00 + 3 zero pad bytes
    out.push_le32(ttp.len() as u32);
    out.push_bytes(&ttp);
    out.into_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_frame_layout() {
        let f = build_out_frame(1, OP_HELLO, &[0xAA, 0xBB, 0xCC]);
        assert_eq!(f.len(), 35); // envelope(8) + header(24) + payload(3)
        assert_eq!(f[0], 0x00); // dir
        assert_eq!(f[4], 27); // LE length excludes envelope: 24 + 3
        assert_eq!(f[5], 0);
        assert_eq!(&f[8..12], &[0, 0, 0, 0x51]); // magic
        assert_eq!(&f[12..16], &[0, 0, 0, 1]); // seq
        assert_eq!(&f[20..24], &[0, 0, 0x03, 0xe8]); // op
        assert_eq!(&f[28..32], &[0, 0, 0, 3]); // plen
        assert_eq!(&f[32..35], &[0xAA, 0xBB, 0xCC]); // payload
    }
}
