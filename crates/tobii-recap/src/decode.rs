//! Top-level decode: pcap bytes -> reassembled TTP frames + capture summary.

use tobii_protocol::{Frame, Parser};

use crate::error::RecapError;
use crate::out_parser::OutParser;
use crate::pcap::RecordReader;
use crate::usbmon::UsbEvent;

/// One reassembled frame placed on the timeline, with its direction and the
/// (relative) timestamp of the transfer that completed it.
#[derive(Debug, Clone)]
pub struct TimelineFrame {
    /// Seconds since the first record in the capture.
    pub ts_rel: f64,
    /// True for device -> host (response/notify); false for host -> device.
    pub dir_in: bool,
    pub frame: Frame,
}

/// Everything the CLI needs to render its report.
#[derive(Debug)]
pub struct DecodeResult {
    pub linktype: u32,
    /// Total pcap records read (all link types, not just bulk).
    pub packet_count: usize,
    pub bulk_in_bytes: u64,
    pub bulk_out_bytes: u64,
    pub frames: Vec<TimelineFrame>,
    /// Non-fatal problems (truncated/garbage records or frames), reported so a
    /// single bad record never aborts the decode.
    pub errors: Vec<String>,
}

/// Decode a whole pcap file (already read into memory).
pub fn decode(bytes: &[u8]) -> Result<DecodeResult, RecapError> {
    let (global, reader) = RecordReader::new(bytes)?;
    // Reject up front if the link type is not a usbmon variant we understand;
    // otherwise every record would fail the same way.
    if !matches!(global.linktype, 220 | 189) {
        return Err(RecapError::UnsupportedLinktype(global.linktype));
    }

    let mut in_parser = Parser::new();
    let mut out_parser = OutParser::new();
    let mut result = DecodeResult {
        linktype: global.linktype,
        packet_count: 0,
        bulk_in_bytes: 0,
        bulk_out_bytes: 0,
        frames: Vec::new(),
        errors: Vec::new(),
    };

    let mut t0: Option<f64> = None;

    for rec in reader {
        let rec = match rec {
            Ok(r) => r,
            Err(e) => {
                result.errors.push(e);
                break; // record framing lost — nothing after this is reliable.
            }
        };
        result.packet_count += 1;
        let ts = rec.ts_seconds(global.nanosecond);
        let t0 = *t0.get_or_insert(ts);
        let ts_rel = ts - t0;

        let ev = match UsbEvent::parse(rec.data, global.big_endian, global.linktype) {
            Ok(ev) => ev,
            Err(e) => {
                result.errors.push(e);
                continue;
            }
        };
        if !ev.carries_payload() {
            continue;
        }

        if ev.dir_in {
            result.bulk_in_bytes += ev.data.len() as u64;
            match in_parser.feed(ev.data) {
                Ok(frames) => push_frames(&mut result.frames, frames, ts_rel, true),
                Err(e) => result
                    .errors
                    .push(format!("inbound frame error at t=+{ts_rel:.6}: {e}")),
            }
        } else {
            result.bulk_out_bytes += ev.data.len() as u64;
            match out_parser.feed(ev.data) {
                Ok(frames) => push_frames(&mut result.frames, frames, ts_rel, false),
                Err(e) => result
                    .errors
                    .push(format!("outbound frame error at t=+{ts_rel:.6}: {e}")),
            }
        }
    }

    Ok(result)
}

fn push_frames(out: &mut Vec<TimelineFrame>, frames: Vec<Frame>, ts_rel: f64, dir_in: bool) {
    for frame in frames {
        out.push(TimelineFrame {
            ts_rel,
            dir_in,
            frame,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tobii_protocol::frame::{build_out_frame, ENVELOPE_SIZE, OP_HELLO, TTP_HDR_SIZE};

    // --- pcap/usbmon synthesis helpers, shared with the integration tests. ---

    fn le_global(linktype: u32) -> Vec<u8> {
        let mut v = vec![0xd4, 0xc3, 0xb2, 0xa1];
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&4u16.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&65535u32.to_le_bytes());
        v.extend_from_slice(&linktype.to_le_bytes());
        v
    }

    /// Wrap a bulk payload in a 64-byte usbmon record, then a pcap record header.
    fn pcap_record(ts_sec: u32, event: u8, epnum: u8, payload: &[u8]) -> Vec<u8> {
        let mut usb = vec![0u8; 64];
        usb[8] = event;
        usb[9] = 3; // BULK
        usb[10] = epnum;
        usb[36..40].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        usb.extend_from_slice(payload);

        let mut rec = Vec::new();
        rec.extend_from_slice(&ts_sec.to_le_bytes());
        rec.extend_from_slice(&0u32.to_le_bytes());
        rec.extend_from_slice(&(usb.len() as u32).to_le_bytes());
        rec.extend_from_slice(&(usb.len() as u32).to_le_bytes());
        rec.extend_from_slice(&usb);
        rec
    }

    /// An inbound (device -> host) TTP envelope: [01 00 00 00][len incl][ttp].
    fn inbound(magic: u32, seq: u32, op: u32, payload: &[u8]) -> Vec<u8> {
        let total = (ENVELOPE_SIZE + TTP_HDR_SIZE + payload.len()) as u32;
        let mut v = vec![0x01, 0, 0, 0];
        v.extend_from_slice(&total.to_le_bytes());
        v.extend_from_slice(&magic.to_be_bytes());
        v.extend_from_slice(&seq.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&op.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn decodes_a_request_response_pair() {
        let mut file = le_global(220);
        // Host -> device HELLO request (submit, OUT endpoint 0x02).
        file.extend(pcap_record(
            0,
            b'S',
            0x02,
            &build_out_frame(1, OP_HELLO, &[0xAA]),
        ));
        // Device -> host HELLO response (complete, IN endpoint 0x81).
        file.extend(pcap_record(
            1,
            b'C',
            0x81,
            &inbound(0x52, 1, OP_HELLO, &[0xBB, 0xCC]),
        ));

        let r = decode(&file).unwrap();
        assert_eq!(r.linktype, 220);
        assert_eq!(r.packet_count, 2);
        assert_eq!(r.frames.len(), 2);

        assert!(!r.frames[0].dir_in);
        assert_eq!(r.frames[0].frame.op, OP_HELLO);
        assert_eq!(r.frames[0].frame.seq, 1);
        assert_eq!(r.frames[0].frame.payload, vec![0xAA]);

        assert!(r.frames[1].dir_in);
        assert_eq!(r.frames[1].frame.magic, 0x52);
        assert_eq!(r.frames[1].frame.payload, vec![0xBB, 0xCC]);

        assert_eq!(r.bulk_out_bytes, (ENVELOPE_SIZE + TTP_HDR_SIZE + 1) as u64);
        assert_eq!(r.bulk_in_bytes, (ENVELOPE_SIZE + TTP_HDR_SIZE + 2) as u64);
        assert!(r.errors.is_empty());
    }

    #[test]
    fn unsupported_linktype_is_rejected() {
        let file = le_global(1); // DLT_EN10MB, not usbmon
        assert!(matches!(
            decode(&file),
            Err(RecapError::UnsupportedLinktype(1))
        ));
    }

    #[test]
    fn garbage_bulk_out_is_reported_not_panicked() {
        let mut file = le_global(220);
        // A bulk OUT submit whose payload is not a valid TTP envelope.
        file.extend(pcap_record(
            0,
            b'S',
            0x02,
            &[
                0x00, 0, 0, 0, 24, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        ));
        let r = decode(&file).unwrap();
        assert_eq!(r.frames.len(), 0);
        assert_eq!(r.errors.len(), 1);
        assert!(r.errors[0].contains("outbound"));
    }
}
