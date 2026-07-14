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
}
