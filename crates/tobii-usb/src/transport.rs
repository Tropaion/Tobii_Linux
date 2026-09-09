//! Byte-transport abstraction and its libusb implementation.

use std::time::Duration;

/// Errors from opening or talking to the device.
#[derive(Debug)]
pub enum UsbError {
    /// The Tobii ET5 (2104:0313) was not found on the bus.
    DeviceNotFound,
    /// The device is on the bus but this process may not open it.
    PermissionDenied,
    /// The device is on the bus but another process holds interface 0.
    DeviceBusy,
    /// A libusb operation failed.
    Usb(rusb::Error),
    /// A calibration blob too small to be one — see `MIN_PLAUSIBLE_BLOB`.
    ImplausibleCalibration { len: usize },
    /// A bulk write transferred fewer bytes than requested.
    ShortWrite { wrote: usize, expected: usize },
    /// The protocol handshake did not complete.
    Handshake,
    /// A request was sent but no matching response arrived within the read window.
    NoResponse { op: u32 },
}

impl std::fmt::Display for UsbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsbError::DeviceNotFound => write!(
                f,
                "Tobii ET5 (2104:0313) not found — is it plugged in, and is the udev rule installed?"
            ),
            UsbError::PermissionDenied => write!(
                f,
                "no permission to open the Tobii ET5 (2104:0313) — install assets/60-tobii.rules \
                 into /etc/udev/rules.d/ and replug the tracker"
            ),
            UsbError::DeviceBusy => write!(
                f,
                "the Tobii ET5 (2104:0313) is already claimed by another process — usually \
                 tobii-gtk; close it and retry"
            ),
            UsbError::Usb(e) => write!(f, "libusb error: {e}"),
            UsbError::ShortWrite { wrote, expected } => {
                write!(f, "short bulk write: {wrote}/{expected} bytes")
            }
            UsbError::ImplausibleCalibration { len } => {
                write!(
                    f,
                    "stored calibration is only {len} bytes — too small to be one; recalibrate"
                )
            }
            UsbError::Handshake => write!(f, "handshake failed"),
            UsbError::NoResponse { op } => write!(f, "no device response for op {op:#x}"),
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
/// Split an outbound frame into transfers the device will accept.
///
/// A frame longer than [`CHUNK`] must go out as several bulk transfers, each
/// carrying its own 8-byte envelope. This matters because the ET5's calibration
/// blob is on the order of hundreds of kilobytes, not the few this code once
/// assumed — sent whole, the apply simply fails.
///
/// The first transfer keeps the frame's own header, but its envelope length is
/// rewritten to describe only the bytes in *this* transfer; continuations are a
/// bare envelope plus payload. The length field is little-endian at bytes 4..8,
/// matching `build_out_frame`. Returns borrowed slices where it can, so the
/// common single-transfer case copies nothing.
fn chunk_frame(data: &[u8]) -> Vec<std::borrow::Cow<'_, [u8]>> {
    use std::borrow::Cow;
    if data.len() <= CHUNK {
        return vec![Cow::Borrowed(data)];
    }
    let mut first = data[..CHUNK].to_vec();
    first[4..8].copy_from_slice(&(CONT_DATA as u32).to_le_bytes());
    let mut out = vec![Cow::Owned(first)];
    for part in data[CHUNK..].chunks(CONT_DATA) {
        let mut cont = Vec::with_capacity(ENVELOPE + part.len());
        cont.extend_from_slice(&0u32.to_le_bytes());
        cont.extend_from_slice(&(part.len() as u32).to_le_bytes());
        cont.extend_from_slice(part);
        out.push(Cow::Owned(cont));
    }
    out
}

/// How long one bulk OUT may take. A calibration blob goes out as ~40 back-to-back
/// transfers and the device does not drain them instantly; the reference
/// implementation allows 2 s per transfer and a 1 s limit was seen to time out
/// mid-blob on real hardware.
const WRITE_TIMEOUT: Duration = Duration::from_millis(2000);

/// Ceiling on bytes buffered by `soak_incoming` during one send.
const SOAK_CAP: usize = 1 << 20;

/// Largest single bulk OUT the device accepts.
const CHUNK: usize = 8192;
/// The per-transfer envelope: four zero bytes then a little-endian length.
const ENVELOPE: usize = 8;
/// Payload a continuation transfer can carry.
const CONT_DATA: usize = CHUNK - ENVELOPE;

const SESSION_OPEN: u8 = 0x41;
const SESSION_CLOSE: u8 = 0x42;

/// libusb-backed [`Transport`] for the Tobii ET5.
pub struct UsbTransport {
    handle: rusb::DeviceHandle<GlobalContext>,
    /// Bytes read off the IN endpoint while a multi-transfer frame was going
    /// out, handed to the next [`Transport::recv`] before anything new.
    ///
    /// Sending is synchronous here, so nothing drains IN for the duration —
    /// and gaze notifications keep arriving at ~33 Hz throughout. Pushing a
    /// 324 KB calibration without reading backs the device's IN buffer up and
    /// it stops accepting OUT: observed failing on transfer 4 of 40. The
    /// reference implementation avoids this by running its receive pump
    /// concurrently with sends. Buffering rather than discarding matters
    /// because the parser upstream reassembles a byte *stream*; a hole in it
    /// would desync framing far more thoroughly than the stall did.
    pending: Vec<u8>,
}

impl UsbTransport {
    /// Open the device, detach any kernel driver, claim interface 0, and send
    /// the vendor session-open control transfer.
    pub fn open() -> Result<Self, UsbError> {
        let handle = open_handle()?;

        // Best-effort kernel driver detach (ignored if not attached / unsupported).
        if handle.kernel_driver_active(IFACE).unwrap_or(false) {
            let _ = handle.detach_kernel_driver(IFACE);
        }
        // Claiming is where a second client loses the race, so this failure gets
        // the same treatment as the open above rather than a bare libusb code.
        handle
            .claim_interface(IFACE)
            .map_err(classify_open_failure)?;

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

        Ok(Self {
            handle,
            pending: Vec::new(),
        })
    }
}

/// Find the ET5 and open it, keeping libusb's reason for any failure.
///
/// `rusb::open_device_with_vid_pid` returns an `Option`, so a permission problem
/// and an absent tracker are indistinguishable to the caller — the reason a
/// running GUI used to be reported as "not plugged in". Enumerating by hand
/// costs one descriptor read per device and keeps the error.
fn open_handle() -> Result<rusb::DeviceHandle<GlobalContext>, UsbError> {
    let mut open_failure = None;
    for device in rusb::devices()?.iter() {
        // A descriptor we cannot read cannot be matched against VID/PID; some
        // other device on the bus is not our problem.
        let Ok(desc) = device.device_descriptor() else {
            continue;
        };
        if desc.vendor_id() != VID || desc.product_id() != PID {
            continue;
        }
        match device.open() {
            Ok(handle) => return Ok(handle),
            // Keep looking: with two trackers attached, one being busy says
            // nothing about the other. The last reason survives if none open.
            Err(e) => open_failure = Some(e),
        }
    }
    Err(open_failure.map_or(UsbError::DeviceNotFound, classify_open_failure))
}

/// Map a libusb failure from `Device::open` or `claim_interface` onto an error
/// that tells the user what to do about it.
fn classify_open_failure(e: rusb::Error) -> UsbError {
    match e {
        rusb::Error::Access => UsbError::PermissionDenied,
        rusb::Error::Busy => UsbError::DeviceBusy,
        // The tracker was enumerated but gone by the time we opened it — from
        // here that is the same situation as never having been there.
        rusb::Error::NoDevice => UsbError::DeviceNotFound,
        other => UsbError::Usb(other),
    }
}

impl Transport for UsbTransport {
    fn send(&mut self, data: &[u8]) -> Result<(), UsbError> {
        let parts = chunk_frame(data);
        let total = parts.len();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                // Keep the device's IN side moving between transfers.
                self.soak_incoming();
            }
            self.write_all(part).inspect_err(|_| {
                // Say where it stopped. A frame abandoned part-way leaves the
                // device's TTP reassembly holding an incomplete frame, so the
                // next request can fail for reasons that have nothing to do
                // with it — worth knowing that is what happened.
                if total > 1 {
                    eprintln!(
                        "usb: frame of {} bytes failed on transfer {}/{total}; \
                         the device may hold a partial frame",
                        data.len(),
                        i + 1
                    );
                }
            })?;
        }
        Ok(())
    }
    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> Option<usize> {
        // Anything soaked up during a large send comes first, in order.
        if !self.pending.is_empty() {
            let n = buf.len().min(self.pending.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            return Some(n);
        }
        match self.handle.read_bulk(EP_IN, buf, timeout) {
            Ok(n) if n > 0 => Some(n),
            // Timeout (expected for polling) or zero-length: nothing this call.
            _ => None,
        }
    }
}

impl UsbTransport {
    /// Take whatever the device has queued on IN, without waiting for more.
    ///
    /// Bounded: past [`SOAK_CAP`] we stop buffering and let the reads fall on
    /// the floor, because a send long enough to overflow this has bigger
    /// problems than the frames it is dropping, and growing without limit
    /// would be worse than either.
    fn soak_incoming(&mut self) {
        if self.pending.len() >= SOAK_CAP {
            return;
        }
        let mut scratch = [0u8; 16384];
        // A 0 timeout means *wait forever* in libusb, not "poll" — hence 1ms.
        while let Ok(n) = self
            .handle
            .read_bulk(EP_IN, &mut scratch, Duration::from_millis(1))
        {
            if n == 0 {
                break;
            }
            self.pending.extend_from_slice(&scratch[..n]);
            if self.pending.len() >= SOAK_CAP {
                break;
            }
        }
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), UsbError> {
        let wrote = self.handle.write_bulk(EP_OUT, data, WRITE_TIMEOUT)?;
        if wrote != data.len() {
            return Err(UsbError::ShortWrite {
                wrote,
                expected: data.len(),
            });
        }
        Ok(())
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

    #[test]
    fn a_libusb_access_failure_becomes_a_permission_error() {
        assert!(matches!(
            classify_open_failure(rusb::Error::Access),
            UsbError::PermissionDenied
        ));
    }

    #[test]
    fn a_libusb_busy_failure_becomes_a_device_busy_error() {
        assert!(matches!(
            classify_open_failure(rusb::Error::Busy),
            UsbError::DeviceBusy
        ));
    }

    #[test]
    fn a_device_that_vanished_between_enumeration_and_open_reads_as_not_found() {
        assert!(matches!(
            classify_open_failure(rusb::Error::NoDevice),
            UsbError::DeviceNotFound
        ));
    }

    #[test]
    fn an_unclassified_libusb_failure_keeps_its_original_error() {
        assert!(matches!(
            classify_open_failure(rusb::Error::Pipe),
            UsbError::Usb(rusb::Error::Pipe)
        ));
        assert!(matches!(
            classify_open_failure(rusb::Error::NoMem),
            UsbError::Usb(rusb::Error::NoMem)
        ));
    }

    #[test]
    fn the_permission_error_names_the_udev_rule_and_the_replug() {
        let msg = UsbError::PermissionDenied.to_string();
        assert!(msg.contains("assets/60-tobii.rules"), "{msg}");
        assert!(msg.contains("replug"), "{msg}");
    }

    #[test]
    fn the_busy_error_names_the_gui_as_the_likely_holder() {
        let msg = UsbError::DeviceBusy.to_string();
        assert!(msg.contains("tobii-gtk"), "{msg}");
    }

    #[test]
    fn a_frame_that_fits_is_sent_whole_and_uncopied() {
        let f = vec![7u8; CHUNK];
        let parts = chunk_frame(&f);
        assert_eq!(parts.len(), 1);
        assert!(matches!(parts[0], std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn every_transfer_declares_its_own_length_and_fits() {
        // A calibration-sized frame: hundreds of KB, the case that motivated this.
        let f: Vec<u8> = (0..414_844u32).map(|i| i as u8).collect();
        let parts = chunk_frame(&f);
        assert!(parts.len() > 50, "{} transfers", parts.len());
        for (i, p) in parts.iter().enumerate() {
            assert!(p.len() <= CHUNK, "transfer {i} is {} bytes", p.len());
            let declared = u32::from_le_bytes(p[4..8].try_into().unwrap()) as usize;
            assert_eq!(
                declared,
                p.len() - ENVELOPE,
                "transfer {i} mis-declares itself"
            );
        }
    }

    #[test]
    fn the_split_preserves_every_byte_after_the_first_envelope() {
        let f: Vec<u8> = (0..30_000u32).map(|i| (i * 7) as u8).collect();
        let parts = chunk_frame(&f);
        let mut rebuilt = parts[0][ENVELOPE..].to_vec();
        for p in &parts[1..] {
            rebuilt.extend_from_slice(&p[ENVELOPE..]);
        }
        assert_eq!(rebuilt, f[ENVELOPE..], "a byte was lost or duplicated");
    }

    #[test]
    fn a_frame_exactly_one_byte_over_the_limit_still_splits_cleanly() {
        let f = vec![3u8; CHUNK + 1];
        let parts = chunk_frame(&f);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].len(), ENVELOPE + 1);
    }
}
