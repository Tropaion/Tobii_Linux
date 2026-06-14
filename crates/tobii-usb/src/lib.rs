//! USB transport and connection driver for the Tobii Eye Tracker 5.
//!
//! `UsbTransport` moves bytes over libusb; `Connection` drives the protocol
//! handshake and decodes the live gaze stream. The driver logic is generic
//! over the `Transport` trait so it can be tested without hardware.

mod connection;
mod transport;

pub use connection::Connection;
pub use transport::{Transport, UsbError, UsbTransport};
