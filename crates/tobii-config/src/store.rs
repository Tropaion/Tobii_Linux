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

/// Path to the calibration metadata sidecar, beside `calibration.bin`.
fn calibration_meta_path() -> PathBuf {
    config_path().with_file_name("calibration.meta.toml")
}

/// Write `bytes` to `path` atomically (temp file in the same directory, then
/// rename), creating parent dirs as needed.
///
/// A plain write truncates first, so a crash or unplug mid-write would leave
/// a truncated file that [`read_opt`] cannot tell from a good one — this
/// matters both for the calibration blob (re-applied to the device on every
/// connect) and for its metadata sidecar.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Append ".tmp" to the whole file name (not `with_extension`, which would
    // replace rather than append for names that already have a `.` in them).
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    std::fs::write(&tmp, bytes)?;
    // Same directory, so rename is atomic (never crosses a filesystem).
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp); // don't leave debris behind
            Err(e)
        }
    }
}

/// Read `path` into bytes. `Ok(None)` if the file does not exist.
fn read_opt(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Metadata bound to a saved calibration: which monitor it was made for, when,
/// how (quick/full/cli), and a hash of the display geometry at save time —
/// lets a caller detect a calibration made for a different screen or since
/// stale geometry. Always a *separate* file from the calibration blob, which
/// stays byte-verbatim for device replay (see [`save_calibration_to`]).
#[derive(Debug, Clone, PartialEq)]
pub struct CalMeta {
    pub monitor_id: Option<String>,
    pub created_utc: i64,
    pub mode: String,
    pub display_fingerprint: u64,
}

impl CalMeta {
    fn to_toml(&self) -> String {
        format!(
            "# tobii-linux calibration metadata\nmonitor_id = \"{}\"\ncreated_utc = {}\nmode = \"{}\"\ndisplay_fingerprint = {}\n",
            self.monitor_id.as_deref().unwrap_or(""),
            self.created_utc,
            self.mode,
            self.display_fingerprint,
        )
    }

    /// Best-effort parse. Any missing/unparseable field yields `None` rather
    /// than a partially-populated struct — a garbled sidecar must read back
    /// as "no metadata", never a crash or a lie about its contents.
    fn from_toml(s: &str) -> Option<CalMeta> {
        let (mut mid, mut created, mut mode, mut fp) = (None, None, None, None);
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim().trim_matches('"');
            match k.trim() {
                "monitor_id" => {
                    mid = Some(if v.is_empty() {
                        None
                    } else {
                        Some(v.to_string())
                    })
                }
                "created_utc" => created = v.parse::<i64>().ok(),
                "mode" => mode = Some(v.to_string()),
                "display_fingerprint" => fp = v.parse::<u64>().ok(),
                _ => {}
            }
        }
        Some(CalMeta {
            monitor_id: mid?,
            created_utc: created?,
            mode: mode?,
            display_fingerprint: fp?,
        })
    }
}

/// Write the opaque calibration blob and its metadata sidecar.
///
/// Both are written atomically via [`write_atomic`]. The blob is written
/// byte-verbatim (it is re-applied to the device on every connect, so any
/// reformatting would break replay); only the sidecar is human-readable TOML.
pub fn save_calibration_to(
    bin: &Path,
    meta_path: &Path,
    blob: &[u8],
    meta: &CalMeta,
) -> io::Result<()> {
    write_atomic(bin, blob)?;
    write_atomic(meta_path, meta.to_toml().as_bytes())
}

/// Read a calibration blob and its metadata sidecar. `Ok(None)` if the blob
/// itself does not exist. A missing, unreadable, or unparseable sidecar is
/// *not* an error — it yields `Some((blob, None))`, treating the install as
/// legacy/unbound rather than failing the blob load it doesn't own.
pub fn load_calibration_from(
    bin: &Path,
    meta_path: &Path,
) -> io::Result<Option<(Vec<u8>, Option<CalMeta>)>> {
    let Some(blob) = read_opt(bin)? else {
        return Ok(None);
    };
    let meta = read_opt(meta_path)?
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| CalMeta::from_toml(&s));
    Ok(Some((blob, meta)))
}

/// Save to the default [`calibration_path`] and its metadata sidecar.
pub fn save_calibration(blob: &[u8], meta: &CalMeta) -> io::Result<()> {
    save_calibration_to(&calibration_path(), &calibration_meta_path(), blob, meta)
}

/// Load from the default [`calibration_path`] and its metadata sidecar.
pub fn load_calibration() -> io::Result<Option<(Vec<u8>, Option<CalMeta>)>> {
    load_calibration_from(&calibration_path(), &calibration_meta_path())
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

/// Path to the persisted setup-time chosen monitor id, beside `config.toml`.
fn setup_monitor_id_path() -> PathBuf {
    config_path().with_file_name("setup_monitor_id")
}

/// Persist which monitor the tracker is set up on (stored as plain UTF-8 text).
/// `Some(id)` writes it atomically; `None` removes the file.
pub fn save_setup_monitor_id_to(path: &Path, id: Option<&str>) -> io::Result<()> {
    match id {
        Some(id_str) => write_atomic(path, id_str.as_bytes()),
        None => {
            // Remove the file if it exists; treat NotFound as success.
            match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            }
        }
    }
}

/// Load the persisted monitor id. `Ok(None)` if unset, missing, or empty.
pub fn load_setup_monitor_id_from(path: &Path) -> io::Result<Option<String>> {
    match read_opt(path)? {
        Some(bytes) => {
            match String::from_utf8(bytes) {
                Ok(s) if !s.is_empty() => Ok(Some(s)),
                Ok(_) => Ok(None),  // empty string treated as unset
                Err(_) => Ok(None), // invalid UTF-8 treated as unset
            }
        }
        None => Ok(None),
    }
}

/// Save to the default [`setup_monitor_id_path`].
pub fn save_setup_monitor_id(id: Option<&str>) -> io::Result<()> {
    save_setup_monitor_id_to(&setup_monitor_id_path(), id)
}

/// Load from the default [`setup_monitor_id_path`].
pub fn load_setup_monitor_id() -> io::Result<Option<String>> {
    load_setup_monitor_id_from(&setup_monitor_id_path())
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
            curvature_radius_mm: 1800.0,
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

    /// A minimal, arbitrary meta for tests that only care about the blob.
    fn sample_meta() -> CalMeta {
        CalMeta {
            monitor_id: None,
            created_utc: 0,
            mode: "full".into(),
            display_fingerprint: 0,
        }
    }

    #[test]
    fn saving_a_calibration_overwrites_atomically_and_leaves_no_temp_file() {
        // The blob is re-applied to the device on every connect, so a truncated
        // file left by an interrupted write would be indistinguishable from a
        // good one. Overwriting must also not strand a .tmp beside it (blob or
        // meta).
        let dir = std::env::temp_dir().join("tobii-config-test-cal-atomic");
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("calibration.bin");
        let meta_path = dir.join("calibration.meta.toml");
        let meta = sample_meta();
        save_calibration_to(&bin, &meta_path, &[0xAA; 64], &meta).expect("first save");
        save_calibration_to(&bin, &meta_path, &[0xBB; 8], &meta).expect("overwrite");
        let (got_blob, _) = load_calibration_from(&bin, &meta_path)
            .expect("load io")
            .expect("some");
        assert_eq!(
            got_blob,
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
        let bin = dir.join("calibration.bin");
        let meta_path = dir.join("calibration.meta.toml");
        let blob = vec![0x01, 0x02, 0x03, 0xFE, 0xFF];
        save_calibration_to(&bin, &meta_path, &blob, &sample_meta()).expect("save");
        let (got_blob, _) = load_calibration_from(&bin, &meta_path)
            .expect("load io")
            .expect("some");
        assert_eq!(got_blob, blob);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_calibration_missing_is_none() {
        let dir = std::env::temp_dir().join("tobii-config-test-cal-missing");
        let bin = dir.join("calibration.bin");
        let meta_path = dir.join("calibration.meta.toml");
        let _ = std::fs::remove_file(&bin);
        let _ = std::fs::remove_file(&meta_path);
        assert!(load_calibration_from(&bin, &meta_path)
            .expect("io ok")
            .is_none());
    }

    #[test]
    fn calibration_path_sits_beside_config() {
        assert!(calibration_path().ends_with("tobii-linux/calibration.bin"));
    }

    // NOTE: no `tempfile`/`tempdir()` crate is a dependency anywhere in this
    // workspace yet, so — unlike the plan's literal snippet — these mirror the
    // hand-rolled `std::env::temp_dir()` + manual cleanup convention the other
    // calibration tests in this file already use, rather than introducing a
    // new external dev-dependency for two tests.
    #[test]
    fn calibration_meta_round_trips_and_blob_is_verbatim() {
        let dir = std::env::temp_dir().join("tobii-config-test-cal-meta-roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("calibration.bin");
        let meta_path = dir.join("calibration.meta.toml");
        let blob = vec![1u8, 2, 3, 4, 5];
        let m = CalMeta {
            monitor_id: Some("SAM7454-HNTY900001".into()),
            created_utc: 42,
            mode: "full".into(),
            display_fingerprint: 0xDEAD_BEEF,
        };
        save_calibration_to(&bin, &meta_path, &blob, &m).unwrap();
        let (got_blob, got_meta) = load_calibration_from(&bin, &meta_path).unwrap().unwrap();
        assert_eq!(got_blob, blob, "blob must round-trip verbatim");
        let got_meta = got_meta.expect("meta present");
        assert_eq!(got_meta.monitor_id.as_deref(), Some("SAM7454-HNTY900001"));
        assert_eq!(got_meta.display_fingerprint, 0xDEAD_BEEF);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_blob_without_meta_loads_with_none_meta() {
        let dir = std::env::temp_dir().join("tobii-config-test-cal-meta-legacy");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("calibration.bin");
        let meta_path = dir.join("calibration.meta.toml");
        std::fs::write(&bin, [9u8, 9, 9]).unwrap(); // no meta file
        let (blob, m) = load_calibration_from(&bin, &meta_path).unwrap().unwrap();
        assert_eq!(blob, vec![9, 9, 9]);
        assert!(m.is_none(), "missing meta => None (legacy install)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn setup_monitor_id_round_trips() {
        let dir = std::env::temp_dir().join("tobii-config-test-monitor-id-roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("setup_monitor_id");
        save_setup_monitor_id_to(&path, Some("SAM7454-HNTY900001")).expect("save");
        let loaded = load_setup_monitor_id_from(&path)
            .expect("load io")
            .expect("some");
        assert_eq!(loaded, "SAM7454-HNTY900001");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn setup_monitor_id_missing_file_is_none() {
        let dir = std::env::temp_dir().join("tobii-config-test-monitor-id-missing");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("setup_monitor_id");
        let loaded = load_setup_monitor_id_from(&path).expect("load io");
        assert!(loaded.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_none_clears_the_file() {
        let dir = std::env::temp_dir().join("tobii-config-test-monitor-id-clear");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("setup_monitor_id");
        save_setup_monitor_id_to(&path, Some("X")).expect("save");
        save_setup_monitor_id_to(&path, None).expect("clear");
        let loaded = load_setup_monitor_id_from(&path).expect("load io");
        assert!(loaded.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
