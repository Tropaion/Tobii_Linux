//! Concrete [`Sink`](crate::Sink) implementations.
//!
//! Every sink here is a **datagram** sink, which is what makes the trait's
//! "must not block" contract cheap to honour: `send_to` on a UDP socket either
//! completes or fails immediately, and a peer that is not listening costs
//! nothing. Anything stream-based would need its own buffering thread behind
//! the trait rather than being written in this style.

pub mod bridge_udp;
pub mod opentrack_udp;

pub use bridge_udp::BridgeUdp;
pub use opentrack_udp::OpentrackUdp;
