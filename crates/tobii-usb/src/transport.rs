//! Byte-transport abstraction and its libusb implementation.

use std::time::Duration;

/// Errors from opening or talking to the device.
#[derive(Debug)]
pub enum UsbError {
    /// The Tobii ET5 (2104:0313) was not found on the bus.
    DeviceNotFound,
    /// A libusb operation failed.
    Usb(rusb::Error),
    /// A bulk write transferred fewer bytes than requested.
    ShortWrite { wrote: usize, expected: usize },
    /// The protocol handshake did not complete.
    Handshake,
}

impl std::fmt::Display for UsbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsbError::DeviceNotFound => write!(
                f,
                "Tobii ET5 (2104:0313) not found — is it plugged in, and is the udev rule installed?"
            ),
            UsbError::Usb(e) => write!(f, "libusb error: {e}"),
            UsbError::ShortWrite { wrote, expected } => {
                write!(f, "short bulk write: {wrote}/{expected} bytes")
            }
            UsbError::Handshake => write!(f, "handshake failed"),
        }
    }
}

impl std::error::Error for UsbError {}

impl From<rusb::Error> for UsbError {
    fn from(e: rusb::Error) -> Self {
        UsbError::Usb(e)
    }
}

/// A bidirectional byte transport. Implemented by `UsbTransport` for real
/// hardware and by mocks in tests.
pub trait Transport {
    /// Send all bytes of `data`. Errors if not all bytes were transferred.
    fn send(&mut self, data: &[u8]) -> Result<(), UsbError>;
    /// Read available bytes into `buf`, waiting up to `timeout`. Returns the
    /// number of bytes read, or `None` on timeout / no data.
    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> Option<usize>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_error_displays() {
        assert!(UsbError::DeviceNotFound.to_string().contains("not found"));
        assert!(UsbError::ShortWrite { wrote: 1, expected: 8 }
            .to_string()
            .contains("short"));
    }
}
