//! Pure codec for the Tobii Eye Tracker 5 USB wire protocol.
//!
//! No I/O, no global state, no external dependencies. Outbound builders
//! return `Vec<u8>`; the inbound [`parser::Parser`] yields complete frames;
//! decoders produce typed [`gaze::GazeSample`] / [`display::DisplayCorners`].
//!
//! Protocol decoded by the `tobiifree` project (GPL-3.0) from USB captures.

pub mod bytes;
pub mod calibration;
pub mod commands;
pub mod display;
pub mod error;
pub mod frame;
pub mod gaze;
pub mod handshake;
pub mod md5;
pub mod parser;
pub mod realm;
pub mod tlv;

pub use calibration::CalibrationBlob;
pub use commands::EnabledEye;
pub use display::DisplayCorners;
pub use error::ProtocolError;
pub use gaze::GazeSample;
pub use handshake::{Handshake, HandshakeAction};
pub use parser::{Frame, Parser};
