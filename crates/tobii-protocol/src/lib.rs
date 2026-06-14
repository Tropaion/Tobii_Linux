//! Pure codec for the Tobii Eye Tracker 5 USB wire protocol.
//!
//! No I/O, no global state, no external dependencies. Outbound builders
//! return `Vec<u8>`; the inbound [`parser::Parser`] yields complete frames;
//! decoders produce typed [`gaze::GazeSample`] / [`display::DisplayCorners`].
//!
//! Protocol decoded by the `tobiifree` project (GPL-3.0) from USB captures.

pub mod error;
