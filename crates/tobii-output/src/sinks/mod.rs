//! Concrete [`Sink`](crate::Sink) implementations.
//!
//! The two UDP sinks are **datagram** sinks, which is what makes the trait's
//! "must not block" contract cheap to honour: `send_to` on a UDP socket either
//! completes or fails immediately, and a peer that is not listening costs
//! nothing. Anything stream-based would need its own buffering thread behind
//! the trait rather than being written in this style.
//!
//! [`UinputJoystick`] honours the same contract for the same reason: a write to
//! a uinput device is a copy into a kernel ring buffer, never a wait on a
//! reader.

pub mod bridge_udp;
pub mod opentrack_udp;
/// Linux-only: the rest of this crate cross-compiles to
/// `x86_64-pc-windows-gnu` for the Wine bridge, which has no `/dev/uinput`.
#[cfg(target_os = "linux")]
pub mod uinput_joystick;

pub use bridge_udp::BridgeUdp;
pub use opentrack_udp::OpentrackUdp;
#[cfg(target_os = "linux")]
pub use uinput_joystick::{JoystickHandle, UinputJoystick};
