//! Message framing: the 8-byte header and the typed messages behind it.
//!
//! Pure. No sockets, no allocation beyond the messages themselves, so the whole
//! wire format is testable against byte literals.
//!
//! [`decode`] is written for a **stream**: it takes whatever bytes have arrived
//! so far and either yields one message plus how many bytes it consumed, or
//! says "not yet" without consuming anything. A caller keeps a buffer, feeds it
//! in, and drains as many messages as it holds.

use crate::{kind, LeaseAction, StatusCode, HEADER_LEN, MAX_PAYLOAD};

/// A decoded message.
///
/// Payloads that belong to another crate's format stay as bytes — see the crate
/// docs for why this one refuses to know what a gaze sample looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    /// Client → server: version, subscription bits, and a name for logs.
    Hello {
        version: u32,
        subs: u32,
        name: String,
    },
    /// Server → client: accepted.
    Welcome { version: u32, pid: u32, caps: u32 },
    /// Server → client: what the daemon is doing, and why.
    Status { code: StatusCode, text: String },
    /// Server → client: a device notification, forwarded **verbatim**.
    ///
    /// `op` is the device op code and `payload` its untouched bytes, so the
    /// client decodes with the same tested decoder the daemon would have used.
    Notify { op: u32, payload: Vec<u8> },
    /// Server → client: an encoded tracking frame, opaque here.
    Pose(Vec<u8>),
    /// Client → server: take or give back the device.
    ///
    /// The daemon hands over the **whole device** rather than proxying
    /// individual commands. Calibration is a long stateful conversation whose
    /// invariants live in the GUI; mirroring it across a socket would mean
    /// re-deriving them in a second place. Instead the daemon drops its
    /// connection, the client opens the device directly with the code it
    /// already has, and hands it back when done.
    Lease(LeaseAction),
    /// Server → client: whether the lease was granted, and why not.
    LeaseReply { ok: bool, text: String },
    /// Client → server: call the user's current head rotation straight ahead.
    ///
    /// No payload, because there is nothing for the client to say. The head
    /// being re-referenced is the one the server is tracking, the reference is
    /// averaged over a settle window on the server's own clock, and whether it
    /// is allowed at all is the server's business — it refuses while a
    /// calibration or a display setup owns the device, since a reference taken
    /// mid-calibration is a reference taken while the user was looking at a
    /// stimulus dot.
    ///
    /// Unlike [`Msg::Lease`] this asks for nothing exclusive and returns
    /// nothing: it is a request that the *composition* of the pose change, so a
    /// client that sends it and a client that receives poses need not be the
    /// same program.
    Recentre,
    /// Server → client: whether the recentre was accepted, and why not.
    ///
    /// `ok` is "the request was taken", not "the reference moved": the settle
    /// window has not finished when this is sent, and it can still refuse for a
    /// head that would not hold still. Whoever reports that is the server.
    RecentreReply { ok: bool, text: String },
    /// A kind this build does not know.
    ///
    /// Forward compatibility: an older client talking to a newer daemon skips
    /// what it cannot read instead of dropping the connection, so adding a
    /// message kind is not a breaking change.
    Unknown { kind: u16, payload: Vec<u8> },
}

/// The result of trying to decode from a stream buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    /// One message, and how many bytes of the buffer it used.
    Message(Msg, usize),
    /// Not enough bytes yet. **Nothing was consumed** — call again after more
    /// data arrives.
    Incomplete,
}

/// Why a buffer could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// The header declared a payload larger than [`MAX_PAYLOAD`].
    PayloadTooLarge(usize),
    /// A payload was structurally wrong for its kind (too short, bad UTF-8).
    Malformed(&'static str),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::PayloadTooLarge(n) => {
                write!(f, "payload of {n} bytes exceeds the {MAX_PAYLOAD}-byte cap")
            }
            CodecError::Malformed(what) => write!(f, "malformed message: {what}"),
        }
    }
}

impl std::error::Error for CodecError {}

/// Read a little-endian `u32` at `off`.
fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().expect("4-byte slice"))
}

/// Read a little-endian `u16` at `off`.
fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().expect("2-byte slice"))
}

/// Wrap a payload in the fixed header.
fn framed(kind: u16, payload: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // flags, reserved
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

/// A one-byte tag — a status code, or an `ok` flag — then a UTF-8 tail.
///
/// Three kinds share this payload shape, and the next reply kind will be a
/// fourth.
fn tagged(tag: u8, text: &str) -> Vec<u8> {
    let mut p = Vec::with_capacity(1 + text.len());
    p.push(tag);
    p.extend_from_slice(text.as_bytes());
    p
}

/// Encode a message, header included.
///
/// A `Msg::Unknown` re-encodes to exactly the bytes it was decoded from, so a
/// proxy can forward a kind it does not understand without corrupting it.
pub fn encode(msg: &Msg) -> Vec<u8> {
    match msg {
        Msg::Hello {
            version,
            subs,
            name,
        } => {
            let n = name.as_bytes();
            let mut p = Vec::with_capacity(10 + n.len());
            p.extend_from_slice(&version.to_le_bytes());
            p.extend_from_slice(&subs.to_le_bytes());
            p.extend_from_slice(&(n.len() as u16).to_le_bytes());
            p.extend_from_slice(n);
            framed(kind::HELLO, p)
        }
        Msg::Welcome { version, pid, caps } => {
            let mut p = Vec::with_capacity(12);
            p.extend_from_slice(&version.to_le_bytes());
            p.extend_from_slice(&pid.to_le_bytes());
            p.extend_from_slice(&caps.to_le_bytes());
            framed(kind::WELCOME, p)
        }
        Msg::Status { code, text } => framed(kind::STATUS, tagged(code.to_wire(), text)),
        Msg::Notify { op, payload } => {
            let mut p = Vec::with_capacity(4 + payload.len());
            p.extend_from_slice(&op.to_le_bytes());
            p.extend_from_slice(payload);
            framed(kind::NOTIFY, p)
        }
        Msg::Pose(frame) => framed(kind::POSE, frame.clone()),
        Msg::Lease(action) => framed(kind::LEASE, vec![action.to_wire()]),
        Msg::LeaseReply { ok, text } => framed(kind::LEASE_REPLY, tagged(u8::from(*ok), text)),
        Msg::Recentre => framed(kind::RECENTRE, Vec::new()),
        Msg::RecentreReply { ok, text } => {
            framed(kind::RECENTRE_REPLY, tagged(u8::from(*ok), text))
        }
        Msg::Unknown { kind, payload } => framed(*kind, payload.clone()),
    }
}

/// Decode a UTF-8 tail, rejecting invalid sequences.
fn text(bytes: &[u8], what: &'static str) -> Result<String, CodecError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| CodecError::Malformed(what))
}

/// Try to decode one message from the front of `buf`.
///
/// Returns [`Decoded::Incomplete`] without consuming anything if the buffer
/// does not yet hold a whole message — the normal case on a stream socket,
/// where a read can split a message anywhere.
pub fn decode(buf: &[u8]) -> Result<Decoded, CodecError> {
    if buf.len() < HEADER_LEN {
        return Ok(Decoded::Incomplete);
    }
    let k = u16_at(buf, 0);
    let len = u32_at(buf, 4) as usize;
    if len > MAX_PAYLOAD {
        return Err(CodecError::PayloadTooLarge(len));
    }
    let total = HEADER_LEN + len;
    if buf.len() < total {
        return Ok(Decoded::Incomplete);
    }
    let p = &buf[HEADER_LEN..total];

    let msg = match k {
        kind::HELLO => {
            if p.len() < 10 {
                return Err(CodecError::Malformed("hello header"));
            }
            let name_len = u16_at(p, 8) as usize;
            if p.len() < 10 + name_len {
                return Err(CodecError::Malformed("hello name"));
            }
            Msg::Hello {
                version: u32_at(p, 0),
                subs: u32_at(p, 4),
                name: text(&p[10..10 + name_len], "hello name")?,
            }
        }
        kind::WELCOME => {
            if p.len() < 12 {
                return Err(CodecError::Malformed("welcome"));
            }
            Msg::Welcome {
                version: u32_at(p, 0),
                pid: u32_at(p, 4),
                caps: u32_at(p, 8),
            }
        }
        kind::STATUS => {
            if p.is_empty() {
                return Err(CodecError::Malformed("status code"));
            }
            Msg::Status {
                code: StatusCode::from_wire(p[0]),
                text: text(&p[1..], "status text")?,
            }
        }
        kind::NOTIFY => {
            if p.len() < 4 {
                return Err(CodecError::Malformed("notify op"));
            }
            Msg::Notify {
                op: u32_at(p, 0),
                payload: p[4..].to_vec(),
            }
        }
        kind::POSE => Msg::Pose(p.to_vec()),
        kind::LEASE => {
            if p.is_empty() {
                return Err(CodecError::Malformed("lease action"));
            }
            Msg::Lease(LeaseAction::from_wire(p[0]))
        }
        kind::LEASE_REPLY => {
            if p.is_empty() {
                return Err(CodecError::Malformed("lease reply"));
            }
            Msg::LeaseReply {
                ok: p[0] != 0,
                text: text(&p[1..], "lease reply text")?,
            }
        }
        // Any payload at all is accepted and ignored, rather than rejected for
        // being non-empty: this kind carries no arguments today, and refusing
        // whatever a later version adds would make that addition breaking in
        // the one direction the protocol promises it is not.
        kind::RECENTRE => Msg::Recentre,
        kind::RECENTRE_REPLY => {
            if p.is_empty() {
                return Err(CodecError::Malformed("recentre reply"));
            }
            Msg::RecentreReply {
                ok: p[0] != 0,
                text: text(&p[1..], "recentre reply text")?,
            }
        }
        other => Msg::Unknown {
            kind: other,
            payload: p.to_vec(),
        },
    };
    Ok(Decoded::Message(msg, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_kind() -> Vec<Msg> {
        vec![
            Msg::Hello {
                version: crate::PROTO_VERSION,
                subs: crate::subs::GAZE | crate::subs::POSE,
                name: "tobii-gtk".to_string(),
            },
            Msg::Welcome {
                version: crate::PROTO_VERSION,
                pid: 4242,
                caps: 0,
            },
            Msg::Status {
                code: StatusCode::Leased,
                text: "held by tobii-gtk".to_string(),
            },
            Msg::Notify {
                op: 0x500,
                payload: vec![1, 2, 3, 0xFF],
            },
            Msg::Pose(vec![0xAB; 80]),
            Msg::Lease(LeaseAction::Acquire),
            Msg::LeaseReply {
                ok: false,
                text: "the tracker is leased by tobii-gtk".to_string(),
            },
            Msg::Recentre,
            Msg::RecentreReply {
                ok: false,
                text: "the hub is busy with calibration".to_string(),
            },
            Msg::Unknown {
                kind: 0x7FFF,
                payload: vec![9, 9, 9],
            },
        ]
    }

    fn decode_one(buf: &[u8]) -> (Msg, usize) {
        match decode(buf).expect("decodes") {
            Decoded::Message(m, n) => (m, n),
            Decoded::Incomplete => panic!("expected a complete message"),
        }
    }

    #[test]
    fn every_kind_round_trips() {
        for msg in every_kind() {
            let bytes = encode(&msg);
            let (got, n) = decode_one(&bytes);
            assert_eq!(got, msg, "round trip failed");
            assert_eq!(n, bytes.len(), "consumed length must match");
        }
    }

    #[test]
    fn the_header_is_kind_flags_and_length() {
        let bytes = encode(&Msg::Lease(LeaseAction::Acquire));
        assert_eq!(u16_at(&bytes, 0), kind::LEASE);
        assert_eq!(u16_at(&bytes, 2), 0, "flags are reserved and must be zero");
        assert_eq!(u32_at(&bytes, 4), 1, "one payload byte");
        assert_eq!(bytes.len(), HEADER_LEN + 1);
    }

    /// A stream read can split a message anywhere. Reporting Incomplete without
    /// consuming is what lets the caller simply wait for more bytes; consuming
    /// a partial header would desynchronise the stream permanently.
    #[test]
    fn a_truncated_message_consumes_nothing_at_every_split_point() {
        let bytes = encode(&Msg::Notify {
            op: 0x500,
            payload: vec![7; 40],
        });
        for cut in 0..bytes.len() {
            assert_eq!(
                decode(&bytes[..cut]),
                Ok(Decoded::Incomplete),
                "a {cut}-byte prefix must be Incomplete"
            );
        }
        // The whole thing decodes.
        assert!(matches!(decode(&bytes), Ok(Decoded::Message(_, _))));
    }

    #[test]
    fn a_message_split_across_two_reads_reassembles() {
        let msg = Msg::Pose(vec![0xCD; 80]);
        let bytes = encode(&msg);
        let (first, second) = bytes.split_at(21);

        let mut buf = first.to_vec();
        assert_eq!(decode(&buf), Ok(Decoded::Incomplete));
        buf.extend_from_slice(second);
        let (got, n) = decode_one(&buf);
        assert_eq!(got, msg);
        assert_eq!(n, bytes.len());
    }

    #[test]
    fn two_messages_in_one_buffer_both_decode() {
        let a = Msg::Lease(LeaseAction::Acquire);
        let b = Msg::Status {
            code: StatusCode::Connected,
            text: "ok".to_string(),
        };
        let mut buf = encode(&a);
        buf.extend_from_slice(&encode(&b));

        let (got_a, n) = decode_one(&buf);
        assert_eq!(got_a, a);
        let (got_b, m) = decode_one(&buf[n..]);
        assert_eq!(got_b, b);
        assert_eq!(n + m, buf.len(), "the pair must consume the whole buffer");
    }

    /// Without the cap, a desynchronised stream could ask for an unbounded
    /// allocation. It must fail loudly rather than being clamped, because a
    /// length that large means the stream is already lost.
    #[test]
    fn an_oversized_length_is_a_hard_error() {
        let mut bytes = encode(&Msg::Pose(vec![]));
        bytes[4..8].copy_from_slice(&((MAX_PAYLOAD + 1) as u32).to_le_bytes());
        assert_eq!(
            decode(&bytes),
            Err(CodecError::PayloadTooLarge(MAX_PAYLOAD + 1))
        );
    }

    #[test]
    fn a_length_exactly_at_the_cap_is_accepted() {
        let mut bytes = encode(&Msg::Pose(vec![]));
        bytes[4..8].copy_from_slice(&(MAX_PAYLOAD as u32).to_le_bytes());
        // Not enough bytes present, but crucially not an error either.
        assert_eq!(decode(&bytes), Ok(Decoded::Incomplete));
    }

    /// Adding a message kind must not be a breaking change: an older client
    /// skips what it cannot read instead of dropping the connection.
    #[test]
    fn an_unknown_kind_decodes_as_unknown_and_can_be_skipped() {
        let mut buf = encode(&Msg::Unknown {
            kind: 0x0999,
            payload: vec![1, 2, 3],
        });
        let known = Msg::Lease(LeaseAction::Release);
        buf.extend_from_slice(&encode(&known));

        let (got, n) = decode_one(&buf);
        assert_eq!(
            got,
            Msg::Unknown {
                kind: 0x0999,
                payload: vec![1, 2, 3]
            }
        );
        // Skipping it leaves the stream aligned on the next message.
        assert_eq!(decode_one(&buf[n..]).0, known);
    }

    /// A kind with no arguments today must still decode once a later version has
    /// given it some. That is the whole of the forward-compatibility promise, and
    /// `RECENTRE` is the one arm in this file that keeps it — every other
    /// length-checked kind is pinned the other way by
    /// `payloads_too_short_for_their_kind_are_rejected`, and the reply arm two
    /// lines below it is the symmetry a later reader would be tempted to copy.
    #[test]
    fn a_recentre_with_a_payload_from_a_newer_client_still_decodes() {
        let mut buf = framed(kind::RECENTRE, vec![1, 2, 3]);
        let known = Msg::Lease(LeaseAction::Release);
        buf.extend_from_slice(&encode(&known));

        let (got, n) = decode_one(&buf);
        assert_eq!(
            got,
            Msg::Recentre,
            "a payload must be ignored, not rejected"
        );
        // Ignored is not skipped: the next message has to still be reachable, or a
        // client that sent one would have desynchronised the stream.
        assert_eq!(decode_one(&buf[n..]).0, known);
    }

    /// A proxy must be able to forward a kind it does not understand without
    /// corrupting it.
    #[test]
    fn an_unknown_message_re_encodes_to_the_bytes_it_came_from() {
        let original = encode(&Msg::Unknown {
            kind: 0x1234,
            payload: vec![5, 6, 7, 8],
        });
        let (decoded, _) = decode_one(&original);
        assert_eq!(encode(&decoded), original);
    }

    /// The reserved command kinds have no implementation, so they must fall
    /// through to Unknown rather than being silently mistaken for something.
    #[test]
    fn the_reserved_command_kinds_decode_as_unknown() {
        for k in [kind::COMMAND, kind::CMD_REPLY] {
            let bytes = encode(&Msg::Unknown {
                kind: k,
                payload: vec![1],
            });
            let (got, _) = decode_one(&bytes);
            assert!(
                matches!(got, Msg::Unknown { kind, .. } if kind == k),
                "{k:#06x} must decode as Unknown"
            );
        }
    }

    #[test]
    fn payloads_too_short_for_their_kind_are_rejected() {
        type Case = (&'static str, u16, Vec<u8>);
        let cases: [Case; 7] = [
            ("hello header", kind::HELLO, vec![0; 9]),
            ("welcome", kind::WELCOME, vec![0; 11]),
            ("status code", kind::STATUS, vec![]),
            ("notify op", kind::NOTIFY, vec![0; 3]),
            ("lease action", kind::LEASE, vec![]),
            ("lease reply", kind::LEASE_REPLY, vec![]),
            ("recentre reply", kind::RECENTRE_REPLY, vec![]),
        ];
        for (what, k, payload) in cases {
            let bytes = framed(k, payload);
            assert_eq!(
                decode(&bytes),
                Err(CodecError::Malformed(what)),
                "{what} must be rejected"
            );
        }
    }

    #[test]
    fn a_hello_whose_name_length_overruns_its_payload_is_rejected() {
        let mut p = Vec::new();
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&0u32.to_le_bytes());
        p.extend_from_slice(&50u16.to_le_bytes()); // claims 50 bytes of name
        p.extend_from_slice(b"short");
        assert_eq!(
            decode(&framed(kind::HELLO, p)),
            Err(CodecError::Malformed("hello name"))
        );
    }

    #[test]
    fn invalid_utf8_in_text_fields_is_rejected() {
        let bytes = framed(kind::STATUS, vec![1, 0xFF, 0xFE]);
        assert_eq!(decode(&bytes), Err(CodecError::Malformed("status text")));
    }

    /// An empty payload is legitimate for the kinds that allow it, and must not
    /// be confused with a truncated stream.
    #[test]
    fn an_empty_payload_is_a_complete_message_not_an_incomplete_one() {
        let bytes = encode(&Msg::Pose(vec![]));
        assert_eq!(bytes.len(), HEADER_LEN);
        let (got, n) = decode_one(&bytes);
        assert_eq!(got, Msg::Pose(vec![]));
        assert_eq!(n, HEADER_LEN);
    }

    /// The 78 KB eye-camera frame is the largest thing that actually travels;
    /// it must survive framing intact.
    #[test]
    fn a_camera_sized_payload_survives() {
        let msg = Msg::Notify {
            op: 0x501,
            payload: vec![0x5A; 78 * 1024],
        };
        let bytes = encode(&msg);
        let (got, n) = decode_one(&bytes);
        assert_eq!(got, msg);
        assert_eq!(n, bytes.len());
    }
}
