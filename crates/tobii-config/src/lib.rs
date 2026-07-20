//! Display-setup geometry and TOML config persistence for the Tobii ET5.
//!
//! [`DisplaySetup`] is the physical parametrization a user edits (monitor size,
//! screen tilt, tracker offsets); [`DisplaySetup::to_corners`] converts it to the
//! three tracker-space corners the device wants (Spike S3 "Model B"), and
//! [`DisplaySetup::from_corners`] inverts a device-reported area back to editable
//! params. No I/O beyond the config store (see `store`).

mod edid;
mod setup;
mod store;

pub use edid::{detect_monitors, MonitorInfo};
pub use setup::DisplaySetup;
pub use store::{
    calibration_path, config_path, enabled_eye_path, load, load_calibration, load_calibration_from,
    load_enabled_eye, load_from, save, save_calibration, save_calibration_to, save_enabled_eye,
    save_to,
};
