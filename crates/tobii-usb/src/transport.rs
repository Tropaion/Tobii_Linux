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

use rusb::{Direction, GlobalContext, Recipient, RequestType};

const VID: u16 = 0x2104;
const PID: u16 = 0x0313;
const IFACE: u8 = 0;
const EP_OUT: u8 = 0x05;
const EP_IN: u8 = 0x83;
const SESSION_OPEN: u8 = 0x41;
const SESSION_CLOSE: u8 = 0x42;

/// libusb-backed [`Transport`] for the Tobii ET5.
pub struct UsbTransport {
    handle: rusb::DeviceHandle<GlobalContext>,
}

impl UsbTransport {
    /// Open the device, detach any kernel driver, claim interface 0, and send
    /// the vendor session-open control transfer.
    pub fn open() -> Result<Self, UsbError> {
        let handle = rusb::open_device_with_vid_pid(VID, PID).ok_or(UsbError::DeviceNotFound)?;

        // Best-effort kernel driver detach (ignored if not attached / unsupported).
        if handle.kernel_driver_active(IFACE).unwrap_or(false) {
            let _ = handle.detach_kernel_driver(IFACE);
        }
        handle.claim_interface(IFACE)?;

        // Vendor session-open: bmRequestType = vendor | host-to-device | interface.
        let req_type =
            rusb::request_type(Direction::Out, RequestType::Vendor, Recipient::Interface);
        handle.write_control(
            req_type,
            SESSION_OPEN,
            0,
            0,
            &[],
            Duration::from_millis(1000),
        )?;

        Ok(Self { handle })
    }
}

impl Transport for UsbTransport {
    fn send(&mut self, data: &[u8]) -> Result<(), UsbError> {
        let wrote = self
            .handle
            .write_bulk(EP_OUT, data, Duration::from_millis(1000))?;
        if wrote != data.len() {
            return Err(UsbError::ShortWrite {
                wrote,
                expected: data.len(),
            });
        }
        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> Option<usize> {
        match self.handle.read_bulk(EP_IN, buf, timeout) {
            Ok(n) if n > 0 => Some(n),
            // Timeout (expected for polling) or zero-length: nothing this call.
            _ => None,
        }
    }
}

impl Drop for UsbTransport {
    fn drop(&mut self) {
        // Vendor session-close, then release the interface (best effort).
        let req_type =
            rusb::request_type(Direction::Out, RequestType::Vendor, Recipient::Interface);
        let _ = self.handle.write_control(
            req_type,
            SESSION_CLOSE,
            0,
            0,
            &[],
            Duration::from_millis(500),
        );
        let _ = self.handle.release_interface(IFACE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_error_displays() {
        assert!(UsbError::DeviceNotFound.to_string().contains("not found"));
        assert!(UsbError::ShortWrite {
            wrote: 1,
            expected: 8
        }
        .to_string()
        .contains("short"));
    }
}
