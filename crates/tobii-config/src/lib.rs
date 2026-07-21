//! Display-setup geometry and TOML config persistence for the Tobii ET5.
//!
//! [`DisplaySetup`] is the physical parametrization a user edits (monitor size,
//! screen tilt, tracker offsets); [`DisplaySetup::to_corners`] converts it to the
//! three tracker-space corners the device wants (Spike S3 "Model B"), and
//! [`DisplaySetup::from_corners`] inverts a device-reported area back to editable
//! params. No I/O beyond the config store (see `store`).
//!
//! The device only ever accepts a plane. Curved panels therefore need the
//! arc->chord width conversion below, but deliberately NO runtime gaze
//! correction: a per-user calibration already absorbs curvature (see
//! `tobii-gtk/src/overlay.rs`).
//! [`chord_from_arc`] / [`arc_from_chord`] convert between the arc width EDID
//! reports and the chord width [`DisplaySetup`] stores.

mod edid;
mod setup;
mod store;

pub use edid::{detect_monitors, pick_monitor, MonitorInfo};
pub use setup::{arc_from_chord, chord_from_arc, DisplaySetup};
pub use store::{
    calibration_path, config_path, enabled_eye_path, load, load_calibration, load_calibration_from,
    load_enabled_eye, load_from, save, save_calibration, save_calibration_to, save_enabled_eye,
    save_to,
};
