//! USB transport and connection driver for the Tobii Eye Tracker 5.
//!
//! `UsbTransport` moves bytes over libusb; `Connection` drives the protocol
//! handshake and decodes the live gaze stream. The driver logic is generic
//! over the `Transport` trait so it can be tested without hardware.
//!
//! (Public re-exports are added by later tasks as the items appear.)

mod transport;
pub use transport::{Transport, UsbError, UsbTransport};
