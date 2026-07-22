//! Parse the Linux `usbmon` pseudo-header that prefixes each captured packet.
//!
//! Two link types are supported:
//! * DLT_USB_LINUX_MMAPPED (220): the 64-byte "s-header". This is the priority.
//! * DLT_USB_LINUX (189): the 48-byte legacy header.
//!
//! Both share the field offsets we care about — event_type(8), xfer_type(9),
//! epnum(10), len_cap(36) — and both are written in the host's native byte
//! order, which equals the pcap file's byte order (same machine wrote both).
//!
//! 64-byte mmapped layout (little-endian on x86):
//!   id(u64) event_type(u8) xfer_type(u8) epnum(u8) devnum(u8) busnum(u16)
//!   flag_setup(u8) flag_data(u8) ts_sec(u64) ts_usec(u32) status(i32)
//!   length(u32) len_cap(u32) setup/iso-union(8) interval(i32)
//!   start_frame(i32) xfer_flags(u32) ndesc(u32) => 64 bytes, then the data.
//! The legacy 48-byte header stops after the setup/iso union (offset 48).

use crate::pcap::u32_at;

/// usbmon transfer types.
pub const XFER_ISO: u8 = 0;
pub const XFER_INT: u8 = 1;
pub const XFER_CTRL: u8 = 2;
pub const XFER_BULK: u8 = 3;

/// usbmon event types.
pub const EVENT_SUBMIT: u8 = b'S'; // 0x53
pub const EVENT_COMPLETE: u8 = b'C'; // 0x43
pub const EVENT_ERROR: u8 = b'E'; // 0x45

/// A parsed usbmon event: header fields plus a borrow of the captured data.
#[derive(Debug, Clone)]
pub struct UsbEvent<'a> {
    pub event_type: u8,
    pub xfer_type: u8,
    /// True when the endpoint direction bit says device -> host (IN).
    pub dir_in: bool,
    /// Bytes actually captured for this transfer (may be 0).
    pub data: &'a [u8],
}

/// Header length for a given linktype, or `None` if unsupported.
fn header_len(linktype: u32) -> Option<usize> {
    match linktype {
        220 => Some(64),
        189 => Some(48),
        _ => None,
    }
}

impl<'a> UsbEvent<'a> {
    /// Parse a usbmon event from one pcap record's bytes.
    ///
    /// Returns `Err(reason)` if the record is too short to hold the header plus
    /// its declared `len_cap` — reported, not panicked.
    pub fn parse(
        record: &'a [u8],
        big_endian: bool,
        linktype: u32,
    ) -> Result<UsbEvent<'a>, String> {
        let hlen = header_len(linktype)
            .ok_or_else(|| format!("unsupported usbmon linktype {linktype}"))?;
        if record.len() < hlen {
            return Err(format!(
                "usbmon header truncated: {} bytes, need {hlen}",
                record.len()
            ));
        }
        let event_type = record[8];
        let xfer_type = record[9];
        let epnum = record[10];
        let len_cap = u32_at(record, 36, big_endian) as usize;
        let dir_in = epnum & 0x80 != 0;

        let avail = record.len() - hlen;
        if len_cap > avail {
            return Err(format!(
                "usbmon len_cap={len_cap} exceeds {avail} captured bytes after {hlen}-byte header"
            ));
        }
        let data = &record[hlen..hlen + len_cap];
        Ok(UsbEvent {
            event_type,
            xfer_type,
            dir_in,
            data,
        })
    }

    /// Whether this event carries the transfer's payload for its direction.
    ///
    /// For a BULK OUT the host provides the buffer on submit (`S`); for a BULK
    /// IN the device fills it on completion (`C`). Restricting to that pairing
    /// (and to a non-empty capture) yields each frame's bytes exactly once,
    /// avoiding double-counting the submit/complete pair.
    pub fn carries_payload(&self) -> bool {
        if self.xfer_type != XFER_BULK || self.data.is_empty() {
            return false;
        }
        (self.dir_in && self.event_type == EVENT_COMPLETE)
            || (!self.dir_in && self.event_type == EVENT_SUBMIT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 64-byte mmapped usbmon record with the given fields + payload.
    fn mmapped(event: u8, xfer: u8, epnum: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8; 64];
        v[8] = event;
        v[9] = xfer;
        v[10] = epnum;
        // len_cap at offset 36 (little-endian).
        v[36..40].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn parses_bulk_out_submit() {
        let rec = mmapped(EVENT_SUBMIT, XFER_BULK, 0x02, &[0xDE, 0xAD]);
        let ev = UsbEvent::parse(&rec, false, 220).unwrap();
        assert_eq!(ev.xfer_type, XFER_BULK);
        assert!(!ev.dir_in);
        assert_eq!(ev.data, &[0xDE, 0xAD]);
        assert!(ev.carries_payload());
    }

    #[test]
    fn parses_bulk_in_complete() {
        let rec = mmapped(EVENT_COMPLETE, XFER_BULK, 0x81, &[0x01, 0x02, 0x03]);
        let ev = UsbEvent::parse(&rec, false, 220).unwrap();
        assert!(ev.dir_in);
        assert!(ev.carries_payload());
    }

    #[test]
    fn out_completion_does_not_double_count() {
        // The complete event of an OUT transfer must not re-supply the payload.
        let rec = mmapped(EVENT_COMPLETE, XFER_BULK, 0x02, &[0xAA]);
        let ev = UsbEvent::parse(&rec, false, 220).unwrap();
        assert!(!ev.carries_payload());
    }

    #[test]
    fn in_submit_carries_nothing() {
        let rec = mmapped(EVENT_SUBMIT, XFER_BULK, 0x81, &[]);
        let ev = UsbEvent::parse(&rec, false, 220).unwrap();
        assert!(!ev.carries_payload());
    }

    #[test]
    fn non_bulk_is_ignored() {
        let rec = mmapped(EVENT_COMPLETE, XFER_CTRL, 0x80, &[0x01]);
        let ev = UsbEvent::parse(&rec, false, 220).unwrap();
        assert!(!ev.carries_payload());
    }

    #[test]
    fn legacy_48_byte_header() {
        let mut v = vec![0u8; 48];
        v[8] = EVENT_COMPLETE;
        v[9] = XFER_BULK;
        v[10] = 0x81;
        v[36..40].copy_from_slice(&2u32.to_le_bytes());
        v.extend_from_slice(&[0x11, 0x22]);
        let ev = UsbEvent::parse(&v, false, 189).unwrap();
        assert_eq!(ev.data, &[0x11, 0x22]);
        assert!(ev.carries_payload());
    }

    #[test]
    fn truncated_capture_is_reported() {
        // Header says len_cap=10 but no data bytes follow.
        let mut v = vec![0u8; 64];
        v[36..40].copy_from_slice(&10u32.to_le_bytes());
        assert!(UsbEvent::parse(&v, false, 220).is_err());
    }
}
