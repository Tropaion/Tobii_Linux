//! Display-setup geometry and TOML config persistence for the Tobii ET5.
//!
//! [`DisplaySetup`] is the physical parametrization a user edits (monitor size,
//! screen tilt, tracker offsets); [`DisplaySetup::to_corners`] converts it to the
//! three tracker-space corners the device wants (Spike S3 "Model B"), and
//! [`DisplaySetup::from_corners`] inverts a device-reported area back to editable
//! params. No I/O beyond the config store (see `store`).
//!
//! The device only ever accepts a plane, and [`DisplaySetup::width_mm`] sends it
//! the EDID **arc** width unchanged (see [`plane_width_from_edid`]) — deliberately
//! NO runtime gaze correction: a per-user calibration already absorbs curvature
//! (see `tobii-gtk/src/overlay.rs`).
//! [`chord_from_arc`] / [`arc_from_chord`] remain as general arc/chord helper
//! math; the plane sent to the device no longer goes through them.

/// The XDG autostart entry, shared by the hub and `tobii uninstall`.
pub mod autostart;
mod calibration_state;
/// Games this machine has that Steam does not list: a name and a Wine prefix.
///
/// Linux-only, and that is load-bearing rather than tidy. This module writes
/// and reads prefix paths **byte for byte**, through
/// `std::os::unix::ffi::OsStrExt`, so a path this program cannot decode
/// survives a round trip through the hub. Windows has no byte view of an
/// `OsStr` — its paths are UTF-16 — so the guarantee cannot be kept there and
/// the code does not compile there either.
///
/// That matters because of who else is downstream: `tobii-output` depends on
/// this crate, and the Wine bridge depends on `tobii-output` and cross-compiles
/// to `x86_64-pc-windows-gnu`. Without this gate the whole bridge fails to
/// build, which is exactly what happened between this module landing and
/// v0.6.0: `scripts/build-bridge.sh` is deliberately outside `cargo build`, and
/// at the time CI did not run it either, so nothing said so. CI builds the
/// bridge now, which is what would catch the next one — this gate is what stops
/// there being a next one. Nothing on the Wine side reads a list of hand-added
/// games; it costs that side nothing.
#[cfg(unix)]
pub mod custom_games;
mod edid;
/// Every name this program writes, and the XDG directories they go in.
pub mod paths;
/// What this program knows about setting up one game, per game.
pub mod profiles;
mod setpm;
mod setup;
/// A dependency-free SHA-256.
///
/// Lives here, in the lowest shared crate, because two unrelated things need to
/// verify a download before trusting it: the head-pose model store and the
/// updater. It was written for the first and moved here for the second rather
/// than duplicated, or reached for across a crate that has nothing to do with
/// either.
pub mod sha256;
pub mod signature;
mod store;
/// Reading a Wine prefix's `HKEY_CURRENT_USER` out of `user.reg`, without
/// running wine.
///
/// Here rather than in `tobii-cli` for [`signature`]'s reason, word for word:
/// `tobii-cli` is a `[[bin]]`, so the hub cannot link it, and two surfaces have
/// to answer the same question. The hub's Games tab used to say outright that
/// it could not read these two values — not because reading them is hard (this
/// module has no dependencies and does no I/O) but because the reader was on
/// the wrong side of a binary. So the one fact that decides whether a game
/// loads our client was reachable only by shelling out.
pub mod userreg;

pub use calibration_state::{decide, CalAction, RecommendReason};
pub use edid::{detect_monitors, pick_monitor, MonitorInfo};
pub use setpm::parse_setpm_corners;
pub use setup::{
    arc_from_chord, chord_from_arc, plane_width_from_edid, tracking_coverage, Coverage,
    DisplaySetup, TRACKING_FAR_MM, USABLE_GAZE_DEG,
};
pub use store::write_atomic;
pub use store::{
    calibration_path, config_path, enabled_eye_path, load, load_calibration, load_calibration_from,
    load_enabled_eye, load_from, load_pitch_offset, load_setup_monitor_id, pitch_offset_path, save,
    save_calibration, save_calibration_to, save_enabled_eye, save_pitch_offset,
    save_setup_monitor_id, save_text_scale, save_text_scale_to, save_to, save_update_check,
    text_scale, text_scale_at, text_scale_path, update_check_enabled, update_check_path, CalMeta,
    TEXT_SCALE_MAX, TEXT_SCALE_MIN,
};
