//! Error type for pcap / usbmon parsing.

use std::fmt;

/// A fatal error while reading the pcap container. Per-record problems (a
/// truncated or garbage frame) are reported non-fatally as strings in
/// [`crate::decode::DecodeResult::errors`] instead, so one bad record never
/// aborts the whole decode.
#[derive(Debug)]
pub enum RecapError {
    /// The file was shorter than a valid pcap global header.
    ShortGlobalHeader,
    /// The pcap magic did not match any known variant.
    BadMagic([u8; 4]),
    /// The link-layer type is not a usbmon capture we support.
    UnsupportedLinktype(u32),
}

impl fmt::Display for RecapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecapError::ShortGlobalHeader => {
                write!(f, "file is too short to contain a pcap global header")
            }
            RecapError::BadMagic(m) => write!(
                f,
                "unrecognized pcap magic {:02x}{:02x}{:02x}{:02x}",
                m[0], m[1], m[2], m[3]
            ),
            RecapError::UnsupportedLinktype(lt) => write!(
                f,
                "unsupported linktype {lt} (expected 220 DLT_USB_LINUX_MMAPPED or 189 DLT_USB_LINUX)"
            ),
        }
    }
}

impl std::error::Error for RecapError {}
