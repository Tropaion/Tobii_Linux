//! Where this program keeps things, by name.
//!
//! # Why this is one list
//!
//! Every file this program writes is named here, and the code that writes each
//! one takes its name from here. `tobii uninstall --purge` deletes **only**
//! names from this list — never a directory tree — so whatever else a user
//! keeps beside them (a hand-made `calibration.bin.baseline-20260810`, a
//! `config.toml.bak`) survives and is reported instead. That only works if the
//! list cannot fall behind the writers, which is why they share it rather than
//! each spelling its own file name.
//!
//! A file added to the config directory without a name here is not a bug that
//! breaks anything; it is a file `--purge` leaves behind and reports as
//! "not written by this program". The test at the bottom checks the writers in
//! this crate against the list.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The application id: the GApplication name, the D-Bus name, and the stem of
/// the desktop entry and icon.
pub const APP_ID: &str = "com.tobiilinux.Configuration";

/// The application-menu entry's file name, and the autostart entry's.
///
/// The same name in both places, which is the convention and which also lets a
/// desktop that matches autostart entries against installed applications find
/// the right one.
pub const DESKTOP_ENTRY: &str = "com.tobiilinux.Configuration.desktop";

/// The icon's file name under `icons/hicolor/scalable/apps/`.
pub const ICON_FILE: &str = "com.tobiilinux.Configuration.svg";

/// The directory name used under every XDG base directory.
pub const APP_DIR: &str = "tobii-linux";

// ----------------------------------------------------------------- config dir

pub const CONFIG_TOML: &str = "config.toml";
pub const CALIBRATION_BIN: &str = "calibration.bin";
pub const CALIBRATION_META: &str = "calibration.meta.toml";
pub const ENABLED_EYE: &str = "enabled_eye";
pub const UPDATE_CHECK: &str = "update_check";
pub const TEXT_SCALE: &str = "text_scale";
pub const PITCH_OFFSET: &str = "headpose_pitch_offset";
pub const SETUP_MONITOR_ID: &str = "setup_monitor_id";
/// Written by `tobii-output`'s game settings.
pub const GAMES_TOML: &str = "games.toml";
/// Written by the hub's accuracy check.
pub const ACCURACY_CSV: &str = "accuracy.csv";
/// Written by `tobii-diagnostics`, mode 0600.
pub const REPORT_SALT: &str = "report_salt";
/// The head-pose model store's directory; `tobii-headpose` names its files.
pub const MODELS_DIR: &str = "models";

/// Every file this program writes directly into [`config_dir`].
pub const CONFIG_FILES: [&str; 11] = [
    CONFIG_TOML,
    CALIBRATION_BIN,
    CALIBRATION_META,
    ENABLED_EYE,
    UPDATE_CHECK,
    TEXT_SCALE,
    PITCH_OFFSET,
    SETUP_MONITOR_ID,
    GAMES_TOML,
    ACCURACY_CSV,
    REPORT_SALT,
];

/// What [`crate::write_atomic`] appends to a file name for its temporary.
///
/// A crash between the write and the rename leaves `<name>.tmp` behind, so
/// that is a name this program writes too.
pub const ATOMIC_TMP_SUFFIX: &str = ".tmp";

// ------------------------------------------------------------------ state dir

/// The log, under [`state_dir`]. `TOBII_LOG_FILE` overrides where it goes.
pub const LOG_FILE: &str = "tobii.log";

/// Where the hub used to save a diagnostics report without asking (it has
/// asked, through a file dialog, since shortly after 9ead161 introduced it).
/// Nothing writes it now; it is listed so `--purge` recognises it.
pub const LEGACY_REPORT_FILE: &str = "diagnostics.txt";

/// Every file this program writes, or has written, into [`state_dir`].
pub const STATE_FILES: [&str; 2] = [LOG_FILE, LEGACY_REPORT_FILE];

// ----------------------------------------------------------------- installs

/// The install manifest's file name, under `<data dir>/tobii-linux/`.
///
/// Written by `scripts/install-payload.sh`: one `bindir=<absolute path>` line
/// per install location. It is a **discovery hint**, never a deletion list —
/// the uninstaller only ever deletes the fixed names above, wherever the
/// manifest says to look.
pub const MANIFEST_FILE: &str = "installs";

/// The data directory a `sudo ./install.sh --system` install writes into.
pub const SYSTEM_DATA_DIR: &str = "/usr/local/share";

/// An XDG base directory: `$VAR` when it is set to an absolute path,
/// otherwise `$HOME/<fallback>`.
///
/// A relative `$VAR` is ignored, as the XDG Base Directory spec says it must
/// be ("If an implementation encounters a relative path in any of these
/// variables it should consider the path invalid and ignore it"). Taking it
/// would resolve against the working directory — for the uninstaller, a
/// different directory on every run.
///
/// An absent or relative HOME gives `/nonexistent/<fallback>`, not a relative
/// path: `PathBuf::default().join(".config")` is `.config`, which would put
/// every file this program writes wherever the working directory happens to
/// be. See [`crate::config_path`], which learned that the hard way.
pub fn xdg_dir(var: Option<&OsStr>, home: Option<&OsStr>, fallback: &str) -> PathBuf {
    if let Some(v) = var.map(Path::new).filter(|v| v.is_absolute()) {
        return v.to_path_buf();
    }
    home.map(PathBuf::from)
        .filter(|h| h.is_absolute())
        .unwrap_or_else(|| PathBuf::from("/nonexistent"))
        .join(fallback)
}

fn from_env(var: &str, fallback: &str) -> PathBuf {
    xdg_dir(
        std::env::var_os(var).as_deref(),
        std::env::var_os("HOME").as_deref(),
        fallback,
    )
}

/// `$XDG_CONFIG_HOME`, or `~/.config`.
pub fn config_home() -> PathBuf {
    from_env("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME`, or `~/.local/share`.
pub fn data_home() -> PathBuf {
    from_env("XDG_DATA_HOME", ".local/share")
}

/// `$XDG_STATE_HOME`, or `~/.local/state`.
pub fn state_home() -> PathBuf {
    from_env("XDG_STATE_HOME", ".local/state")
}

/// `<config home>/tobii-linux`: settings, calibration, models.
pub fn config_dir() -> PathBuf {
    config_home().join(APP_DIR)
}

/// `<state home>/tobii-linux`: the log.
pub fn state_dir() -> PathBuf {
    state_home().join(APP_DIR)
}

/// The install manifest under a given data directory.
pub fn manifest_in(data_dir: &Path) -> PathBuf {
    data_dir.join(APP_DIR).join(MANIFEST_FILE)
}

/// The desktop entry under a given data directory.
pub fn desktop_entry_in(data_dir: &Path) -> PathBuf {
    data_dir.join("applications").join(DESKTOP_ENTRY)
}

/// The `hicolor` theme directory under a given data directory — where the icon
/// cache lives, if anything made one.
pub fn hicolor_in(data_dir: &Path) -> PathBuf {
    data_dir.join("icons").join("hicolor")
}

/// The icon under a given data directory.
pub fn icon_in(data_dir: &Path) -> PathBuf {
    hicolor_in(data_dir)
        .join("scalable")
        .join("apps")
        .join(ICON_FILE)
}

/// The D-Bus object path GApplication exports for [`APP_ID`].
pub fn app_object_path() -> String {
    format!("/{}", APP_ID.replace('.', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn an_xdg_variable_wins_and_an_empty_one_does_not() {
        let home = OsString::from("/home/u");
        assert_eq!(
            xdg_dir(Some(OsStr::new("/x/cfg")), Some(&home), ".config"),
            PathBuf::from("/x/cfg")
        );
        assert_eq!(
            xdg_dir(Some(OsStr::new("")), Some(&home), ".config"),
            PathBuf::from("/home/u/.config")
        );
        assert_eq!(
            xdg_dir(None, Some(&home), ".local/state"),
            PathBuf::from("/home/u/.local/state")
        );
    }

    /// The XDG spec: a relative value in an XDG variable is invalid and is
    /// ignored, so the fallback under HOME is used instead.
    #[test]
    fn a_relative_xdg_variable_is_ignored() {
        let home = OsString::from("/home/u");
        for rel in ["cfg", "./cfg", "../x/cfg", "~/.config"] {
            assert_eq!(
                xdg_dir(Some(OsStr::new(rel)), Some(&home), ".config"),
                PathBuf::from("/home/u/.config"),
                "{rel}"
            );
        }
    }

    /// A relative or missing HOME must never produce a relative path: every
    /// writer would then resolve against the working directory.
    #[test]
    fn no_usable_home_is_never_a_relative_path() {
        for home in [None, Some(OsString::from("")), Some(OsString::from("rel"))] {
            let p = xdg_dir(None, home.as_deref(), ".config");
            assert!(p.is_absolute(), "{}", p.display());
        }
    }

    /// Every path function in this crate names a file from [`CONFIG_FILES`],
    /// in [`config_dir`]. A writer that grows a file of its own must add it to
    /// the list, or `tobii uninstall --purge` will leave it behind.
    #[test]
    fn every_file_this_crate_writes_is_on_the_list() {
        for p in [
            crate::config_path(),
            crate::calibration_path(),
            crate::enabled_eye_path(),
            crate::update_check_path(),
            crate::text_scale_path(),
            crate::pitch_offset_path(),
        ] {
            let name = p.file_name().and_then(|n| n.to_str()).expect("a name");
            assert!(CONFIG_FILES.contains(&name), "{name} is not listed");
            assert_eq!(p.parent(), Some(config_dir().as_path()));
        }
    }

    #[test]
    fn the_object_path_is_the_app_id_with_slashes() {
        assert_eq!(app_object_path(), "/com/tobiilinux/Configuration");
    }

    #[test]
    fn the_desktop_entry_is_named_for_the_app_id() {
        assert_eq!(DESKTOP_ENTRY, format!("{APP_ID}.desktop"));
        assert_eq!(ICON_FILE, format!("{APP_ID}.svg"));
    }
}
