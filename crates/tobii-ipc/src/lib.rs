//! Local IPC between the `tobii serve` daemon and its clients.
//!
//! The tracker is claimed exclusively by libusb, so exactly one process can
//! hold it. Today that means the GUI and game output cannot run at the same
//! time — you close one to use the other. This crate is the transport that
//! fixes it: the daemon owns the device and everyone else becomes a client.
//!
//! # What travels, and what does not
//!
//! Deliberately **dependency-free**, including on the other crates in this
//! workspace. Payloads are opaque `Vec<u8>`, so a client decodes a gaze
//! notification with `tobii_protocol::GazeSample::decode` — already tested
//! against captured hardware fixtures — rather than this crate inventing a
//! second representation of a gaze sample that could drift from the first.
//!
//! # Framing
//!
//! ```text
//! off  len  field
//!   0    2  kind    u16 LE
//!   2    2  flags   u16 LE  (reserved, must be 0)
//!   4    4  len     u32 LE  payload length, capped at MAX_PAYLOAD
//!   8  len  payload
//! ```
//!
//! A fixed binary header rather than a line-delimited text protocol, for three
//! reasons: the payloads are already binary (an eye-camera frame is 78 KB, and
//! base64 would cost ~105 KB per client at 33 Hz), this workspace has no JSON
//! or serde and hand-rolling one would be strictly worse, and eight fixed bytes
//! are byte-for-byte unit-testable — which is this repo's favourite kind of
//! test.
//!
//! The length cap is a defensive bound, not a tuning knob: without it a
//! desynchronised stream could ask for an unbounded allocation.

pub mod client;
pub mod codec;
pub mod path;
pub mod server;

pub use client::Client;
pub use codec::{decode, encode, CodecError, Decoded, Msg};
pub use path::{socket_path, BindAction};
pub use server::{ClientId, ClientInfo, Incoming, Server};

/// Protocol version announced in `Hello`/`Welcome`.
pub const PROTO_VERSION: u32 = 1;

/// Size of the fixed message header.
pub const HEADER_LEN: usize = 8;

/// Largest payload accepted. An eye-camera frame is ~78 KB, so this leaves
/// well over an order of magnitude of headroom while still bounding a
/// desynchronised stream.
pub const MAX_PAYLOAD: usize = 1 << 20;

/// A typical eye-camera frame — the largest thing that actually travels.
const CAMERA_FRAME_BYTES: usize = 78 * 1024;

// Checked at compile time rather than in a test: shrinking the cap below a
// camera frame would not fail somewhere obvious, it would silently stop the
// hub's eye preview from ever receiving one.
const _: () = assert!(MAX_PAYLOAD > CAMERA_FRAME_BYTES * 10);

/// Message kind codes, as they appear on the wire.
pub mod kind {
    /// Client → server: announce version and what it wants to receive.
    pub const HELLO: u16 = 0x0001;
    /// Server → client: accept the connection.
    pub const WELCOME: u16 = 0x0002;
    /// Server → client: connection state changed.
    pub const STATUS: u16 = 0x0010;
    /// Server → client: a raw device notification, verbatim.
    pub const NOTIFY: u16 = 0x0011;
    /// Server → client: a cooked tracking frame.
    pub const POSE: u16 = 0x0012;

    /// Reserved for forwarding device commands over IPC. **Not implemented**
    /// on purpose — see the `Lease` documentation for why the daemon hands the
    /// whole device over instead of proxying individual commands.
    pub const COMMAND: u16 = 0x0020;
    /// Reserved companion to [`COMMAND`]. Not implemented.
    pub const CMD_REPLY: u16 = 0x0021;

    /// Client → server: take or release exclusive use of the device.
    pub const LEASE: u16 = 0x0030;
    /// Server → client: whether the lease was granted.
    pub const LEASE_REPLY: u16 = 0x0031;
}

/// Bits a client sets in `Hello.subs` to say what it wants.
pub mod subs {
    /// Raw gaze notifications.
    pub const GAZE: u32 = 1 << 0;
    /// Eye-camera frames. Costly — see [`crate::codec::Msg::Notify`].
    pub const CAMERA: u32 = 1 << 1;
    /// Cooked tracking frames.
    pub const POSE: u32 = 1 << 2;
}

/// What the daemon is currently doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    /// Trying to open the tracker.
    Connecting,
    /// Streaming.
    Connected,
    /// The tracker could not be opened; the text carries the reason.
    Error,
    /// Another client holds the device lease, so nothing is being published.
    Leased,
    /// Nothing is asking for the tracker, so it is deliberately not open.
    ///
    /// Distinct from [`StatusCode::Connecting`], and the distinction is the
    /// whole standby model: a client that cannot tell "off on purpose" from
    /// "trying to open" renders "Connecting…" forever while the tracker sits
    /// dark by design, and its user files a bug about a hang.
    Idle,
}

impl StatusCode {
    /// The wire byte for this status.
    pub fn to_wire(self) -> u8 {
        match self {
            StatusCode::Connecting => 0,
            StatusCode::Connected => 1,
            StatusCode::Error => 2,
            StatusCode::Leased => 3,
            StatusCode::Idle => 4,
        }
    }

    /// Decode a wire byte.
    ///
    /// An unrecognised code reads as [`StatusCode::Error`]: a client that
    /// cannot understand what the daemon is doing should show *something is
    /// wrong* rather than a confident "Connected".
    pub fn from_wire(b: u8) -> StatusCode {
        match b {
            0 => StatusCode::Connecting,
            1 => StatusCode::Connected,
            3 => StatusCode::Leased,
            4 => StatusCode::Idle,
            _ => StatusCode::Error,
        }
    }
}

/// Whether a client wants the device or is giving it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseAction {
    /// Take exclusive use of the tracker.
    Acquire,
    /// Give it back.
    Release,
}

impl LeaseAction {
    /// The wire byte for this action.
    pub fn to_wire(self) -> u8 {
        match self {
            LeaseAction::Acquire => 0,
            LeaseAction::Release => 1,
        }
    }

    /// Decode a wire byte.
    ///
    /// Anything unrecognised reads as [`LeaseAction::Release`], the safe
    /// direction: a garbled byte gives the device *back* rather than seizing
    /// it, so a confused client cannot strand the tracker away from everyone.
    pub fn from_wire(b: u8) -> LeaseAction {
        match b {
            0 => LeaseAction::Acquire,
            _ => LeaseAction::Release,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_round_trip() {
        for s in [
            StatusCode::Connecting,
            StatusCode::Connected,
            StatusCode::Error,
            StatusCode::Leased,
            StatusCode::Idle,
        ] {
            assert_eq!(StatusCode::from_wire(s.to_wire()), s);
        }
    }

    /// `Idle` must not collide with an existing code, or an older client would
    /// silently render "off on purpose" as something else.
    #[test]
    fn idle_has_a_wire_byte_of_its_own() {
        let all = [
            StatusCode::Connecting,
            StatusCode::Connected,
            StatusCode::Error,
            StatusCode::Leased,
            StatusCode::Idle,
        ];
        let mut seen: Vec<u8> = all.iter().map(|s| s.to_wire()).collect();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), before, "two statuses share a wire byte");
        assert_eq!(StatusCode::Idle.to_wire(), 4, "appended, not renumbered");
    }

    /// A client that cannot understand the daemon's state must not display a
    /// confident "Connected"; erring toward Error is the honest direction.
    #[test]
    fn an_unknown_status_reads_as_error() {
        assert_eq!(StatusCode::from_wire(200), StatusCode::Error);
    }

    #[test]
    fn lease_actions_round_trip() {
        for a in [LeaseAction::Acquire, LeaseAction::Release] {
            assert_eq!(LeaseAction::from_wire(a.to_wire()), a);
        }
    }

    /// A garbled byte must give the device back rather than seize it —
    /// otherwise a confused client could strand the tracker away from everyone.
    #[test]
    fn an_unknown_lease_action_releases_rather_than_acquires() {
        assert_eq!(LeaseAction::from_wire(200), LeaseAction::Release);
    }

    /// Reserved kinds must not collide with implemented ones — they exist so a
    /// future command-forwarding path has numbers already set aside.
    #[test]
    fn every_message_kind_is_distinct() {
        let all = [
            kind::HELLO,
            kind::WELCOME,
            kind::STATUS,
            kind::NOTIFY,
            kind::POSE,
            kind::COMMAND,
            kind::CMD_REPLY,
            kind::LEASE,
            kind::LEASE_REPLY,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "duplicate kind code {a:#06x}");
            }
        }
    }
}
