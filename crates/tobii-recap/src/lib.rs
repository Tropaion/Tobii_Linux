//! Decode a Linux `usbmon` pcap capture of the Tobii ET5 into a human-readable
//! TTP op catalog.
//!
//! The crate parses the pcap container and the `usbmon` pseudo-header by hand
//! (no external pcap dependency), extracts the BULK transfers, and reassembles
//! the TTP frames on each direction:
//!
//! * device -> host (bulk IN) is fed through [`tobii_protocol::Parser`], which
//!   already handles the inbound envelope and multi-transfer reassembly.
//! * host -> device (bulk OUT) is fed through [`out_parser::OutParser`], a small
//!   mirror of `Parser` for the *outbound* envelope (dir byte `0x00`, length
//!   field excludes the envelope) — `Parser` rejects those, so it cannot be
//!   reused directly.
//!
//! The reassembled frames feed a timeline and an aggregated op catalog.

pub mod catalog;
pub mod decode;
pub mod error;
pub mod opnames;
pub mod out_parser;
pub mod pcap;
pub mod usbmon;

pub use decode::{decode, DecodeResult, TimelineFrame};
pub use error::RecapError;
