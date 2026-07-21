//! Config-file persistence: `$XDG_CONFIG_HOME/tobii-linux/config.toml`.

use std::io;
use std::path::{Path, PathBuf};

use crate::DisplaySetup;

/// The default config file path: `$XDG_CONFIG_HOME/tobii-linux/config.toml`,
/// falling back to `$HOME/.config/tobii-linux/config.toml`.
pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(".config")
        });
    base.join("tobii-linux").join("config.toml")
}

/// Write `setup` as TOML to `path`, creating parent directories as needed.
pub fn save_to(path: &Path, setup: &DisplaySetup) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, setup.to_toml())
}

/// Read a `DisplaySetup` from `path`. `Ok(None)` if the file does not exist or
/// does not parse.
pub fn load_from(path: &Path) -> io::Result<Option<DisplaySetup>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(DisplaySetup::from_toml(&s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Save to the default [`config_path`].
pub fn save(setup: &DisplaySetup) -> io::Result<()> {
    save_to(&config_path(), setup)
}

/// Load from the default [`config_path`].
pub fn load() -> io::Result<Option<DisplaySetup>> {
    load_from(&config_path())
}

/// Path to the calibration blob, beside `config.toml`.
pub fn calibration_path() -> PathBuf {
    config_path().with_file_name("calibration.bin")
}

/// Write the opaque calibration blob to `path`, creating parent dirs as needed.
///
/// Written atomically (temp file in the same directory, then rename): a plain
/// write truncates first, so a crash or unplug mid-write would leave a
/// truncated blob that [`load_calibration_from`] cannot tell from a good one —
/// and it is re-applied to the device on every connect.
pub fn save_calibration_to(path: &Path, blob: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("bin.tmp");
    std::fs::write(&tmp, blob)?;
    // Same directory, so rename is atomic (never crosses a filesystem).
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp); // don't leave debris behind
            Err(e)
        }
    }
}

/// Read a calibration blob from `path`. `Ok(None)` if the file does not exist.
pub fn load_calibration_from(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Save to the default [`calibration_path`].
pub fn save_calibration(blob: &[u8]) -> io::Result<()> {
    save_calibration_to(&calibration_path(), blob)
}

/// Load from the default [`calibration_path`].
pub fn load_calibration() -> io::Result<Option<Vec<u8>>> {
    load_calibration_from(&calibration_path())
}

/// Path to the persisted "select eyes to detect" choice, beside `config.toml`.
pub fn enabled_eye_path() -> PathBuf {
    config_path().with_file_name("enabled_eye")
}

/// Persist which eye(s) the tracker should detect (stored as the wire value).
pub fn save_enabled_eye(eye: tobii_protocol::EnabledEye) -> io::Result<()> {
    let path = enabled_eye_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, [eye.to_wire() as u8])
}

/// Load the persisted eye choice. `Ok(None)` if unset or unparseable.
pub fn load_enabled_eye() -> io::Result<Option<tobii_protocol::EnabledEye>> {
    match std::fs::read(enabled_eye_path()) {
        Ok(b) if !b.is_empty() => Ok(tobii_protocol::EnabledEye::from_wire(b[0] as u32)),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DisplaySetup {
        DisplaySetup {
            width_mm: 800.0,
            height_mm: 335.0,
            tilt_deg: 20.0,
            offset_x_mm: 0.0,
            offset_y_mm: 40.0,
            offset_z_mm: -5.0,
        }
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = std::env::temp_dir().join("tobii-config-test-save-load");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");
        let s = sample();
        save_to(&path, &s).expect("save");
        let loaded = load_from(&path).expect("load io").expect("some");
        assert_eq!(loaded, s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_returns_none() {
        let path = std::env::temp_dir()
            .join("tobii-config-test-missing")
            .join("nope.toml");
        let _ = std::fs::remove_file(&path);
        assert!(load_from(&path).expect("io ok").is_none());
    }

    #[test]
    fn config_path_ends_with_expected_suffix() {
        let p = config_path();
        assert!(p.ends_with("tobii-linux/config.toml"));
    }

    #[test]
    fn saving_a_calibration_overwrites_atomically_and_leaves_no_temp_file() {
        // The blob is re-applied to the device on every connect, so a truncated
        // file left by an interrupted write would be indistinguishable from a
        // good one. Overwriting must also not strand a .tmp beside it.
        let dir = std::env::temp_dir().join("tobii-config-test-cal-atomic");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("calibration.bin");
        save_calibration_to(&path, &[0xAA; 64]).expect("first save");
        save_calibration_to(&path, &[0xBB; 8]).expect("overwrite");
        assert_eq!(
            load_calibration_from(&path)
                .expect("load io")
                .expect("some"),
            vec![0xBB; 8],
            "overwrite fully replaces the previous blob"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("readdir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calibration_blob_roundtrips() {
        let dir = std::env::temp_dir().join("tobii-config-test-cal");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("calibration.bin");
        let blob = vec![0x01, 0x02, 0x03, 0xFE, 0xFF];
        save_calibration_to(&path, &blob).expect("save");
        assert_eq!(
            load_calibration_from(&path)
                .expect("load io")
                .expect("some"),
            blob
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_calibration_missing_is_none() {
        let path = std::env::temp_dir()
            .join("tobii-config-test-cal-missing")
            .join("calibration.bin");
        let _ = std::fs::remove_file(&path);
        assert!(load_calibration_from(&path).expect("io ok").is_none());
    }

    #[test]
    fn calibration_path_sits_beside_config() {
        assert!(calibration_path().ends_with("tobii-linux/calibration.bin"));
    }
}
