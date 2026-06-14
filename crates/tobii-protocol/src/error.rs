//! Error type for the protocol codec.

/// Errors produced while encoding or (mostly) decoding protocol bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// Not enough bytes remaining to read the requested field.
    ShortRead,
    /// A TLV field had an unexpected type byte.
    WrongType { expected: u8, found: u8 },
    /// A TLV field had an unexpected size.
    WrongSize { expected: u32, found: u32 },
    /// A prolog/struct tag did not match what was expected.
    WrongTag { expected: u32, found: u32 },
    /// Inbound USB envelope had a direction byte other than 0x01.
    BadDirection(u8),
    /// Inbound envelope length field was impossibly small.
    BadLength(u32),
    /// Reassembly accumulator would overflow its cap.
    Overflow,
}

impl core::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ProtocolError::ShortRead => write!(f, "short read: not enough bytes"),
            ProtocolError::WrongType { expected, found } => {
                write!(f, "wrong TLV type: expected {expected}, found {found}")
            }
            ProtocolError::WrongSize { expected, found } => {
                write!(f, "wrong TLV size: expected {expected}, found {found}")
            }
            ProtocolError::WrongTag { expected, found } => {
                write!(f, "wrong tag: expected {expected:#x}, found {found:#x}")
            }
            ProtocolError::BadDirection(b) => write!(f, "bad envelope direction byte: {b:#x}"),
            ProtocolError::BadLength(n) => write!(f, "bad envelope length: {n}"),
            ProtocolError::Overflow => write!(f, "reassembly buffer overflow"),
        }
    }
}

impl std::error::Error for ProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_displays_human_text() {
        let e = ProtocolError::WrongType {
            expected: 2,
            found: 5,
        };
        assert_eq!(format!("{e}"), "wrong TLV type: expected 2, found 5");
        assert!(ProtocolError::ShortRead.to_string().contains("short read"));
    }
}
