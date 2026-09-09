//! Game-output settings: `$XDG_CONFIG_HOME/tobii-linux/games.toml`.
//!
//! # Why a sidecar file rather than a `[games]` section in `config.toml`
//!
//! `config.toml` is written by `store::save_to`, which serializes a whole
//! [`DisplaySetup`](crate::DisplaySetup) and `fs::write`s the result over the
//! file. Any other section living there would be silently erased the next time
//! the user ran `tobii setup` or finished the GUI's display flow — and erased
//! *quietly*, since nothing reads a section it does not own. A sidecar beside
//! `config.toml` is the convention this repo already uses for exactly this
//! reason (`enabled_eye`, `setup_monitor_id`, `calibration.meta.toml`), and it
//! keeps each writer owning one whole file.
//!
//! # Absent means defaults, not "unconfigured"
//!
//! Unlike `DisplaySetup::from_toml`, this parser never returns `None`. Every
//! key has a sensible default and no combination of them is invalid, so a
//! missing file, a missing key, or an unparseable value all degrade to the
//! default for that one key. A typo in a tuning value should cost you that
//! value, not your whole game output.

use std::path::{Path, PathBuf};

use crate::fusion::{AxisResponse, Curve, ExtendedView};

/// Default opentrack endpoint — its "UDP over network" input's usual address.
pub const DEFAULT_OPENTRACK_ADDR: &str = "127.0.0.1:4242";

/// Default loopback port the Wine-side bridge listens on.
///
/// Deliberately **not** 4242: an opentrack instance and our own bridge should
/// be able to run side by side, which is how the two are compared while the
/// bridge is being brought up.
pub const DEFAULT_BRIDGE_PORT: u16 = 4243;

/// Default frames per second put on the wire.
pub const DEFAULT_RATE_HZ: f64 = 60.0;

/// Everything that decides what game output does.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputConfig {
    /// Whether the daemon emits game output at all.
    ///
    /// Off by default: a background service that starts steering games the
    /// moment it is installed would be a surprise. `tobii headpose` ignores
    /// this — running that command *is* the opt-in.
    pub enabled: bool,
    /// Frames per second delivered to sinks. Sampling and smoothing still run
    /// at the device's full rate; this throttles only the wire.
    pub rate_hz: f64,
    /// Smoothing strength, `0.0`–`1.0`; higher follows the head more closely.
    pub filter_alpha: f64,
    /// opentrack endpoint, or `None` to not send there.
    pub opentrack: Option<String>,
    /// Loopback port for the Wine bridge, or `None` to not send there.
    pub bridge_port: Option<u16>,
    /// Gaze-driven camera offset.
    pub extended_view: ExtendedView,
}

impl Default for OutputConfig {
    fn default() -> Self {
        OutputConfig {
            enabled: false,
            rate_hz: DEFAULT_RATE_HZ,
            filter_alpha: tobii_headpose::filter::DEFAULT_ALPHA,
            opentrack: Some(DEFAULT_OPENTRACK_ADDR.to_string()),
            bridge_port: Some(DEFAULT_BRIDGE_PORT),
            extended_view: ExtendedView::default(),
        }
    }
}

/// Emit one axis's five keys with a `prefix` such as `ev_yaw`.
fn axis_to_toml(prefix: &str, a: &AxisResponse) -> String {
    format!(
        "{prefix}_deadzone_deg = {}\n\
         {prefix}_input_max_deg = {}\n\
         {prefix}_output_max_deg = {}\n\
         {prefix}_curve = \"{}\"\n\
         {prefix}_clamp_deg = {}\n",
        a.deadzone_deg,
        a.input_max_deg,
        a.output_max_deg,
        a.curve.to_config_string(),
        a.clamp_deg,
    )
}

/// Strip surrounding double quotes, if present.
fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(s)
}

impl OutputConfig {
    /// Serialize to a `[games]` TOML document.
    pub fn to_toml(&self) -> String {
        let mut s = String::from(
            "# tobii-linux game output — edit with `tobii games set KEY VALUE`\n\
             #\n\
             # opentrack = \"\"  disables the opentrack sink\n\
             # bridge_port = 0 disables the Wine-bridge sink\n\
             [games]\n",
        );
        s.push_str(&format!("enabled = {}\n", self.enabled));
        s.push_str(&format!("rate_hz = {}\n", self.rate_hz));
        s.push_str(&format!("filter_alpha = {}\n", self.filter_alpha));
        s.push_str(&format!(
            "opentrack = \"{}\"\n",
            self.opentrack.as_deref().unwrap_or("")
        ));
        s.push_str(&format!(
            "bridge_port = {}\n",
            self.bridge_port.unwrap_or(0)
        ));
        s.push_str(&format!("extended_view = {}\n", self.extended_view.enabled));
        s.push_str(&format!("ev_hold_ms = {}\n", self.extended_view.hold_ms));
        s.push_str(&axis_to_toml("ev_yaw", &self.extended_view.yaw));
        s.push_str(&axis_to_toml("ev_pitch", &self.extended_view.pitch));
        s
    }

    /// Parse a `[games]` TOML document.
    ///
    /// Never fails: unknown keys, foreign sections and unparseable values are
    /// all ignored, and anything absent keeps its default. See the module docs.
    pub fn from_toml(s: &str) -> OutputConfig {
        let mut cfg = OutputConfig::default();
        let mut in_games = false;
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                in_games = line == "[games]";
                continue;
            }
            if !in_games {
                continue;
            }
            let Some((key, val)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            // Strip trailing comments only outside quotes, so an address is
            // never truncated at a `#` that is part of the value.
            let raw = val.trim();
            let raw = if raw.starts_with('"') {
                raw
            } else {
                raw.split('#').next().unwrap_or("").trim()
            };
            cfg.apply_key(key, unquote(raw).trim());
        }
        cfg
    }

    /// Apply one `key = value` pair. Unknown keys and unparseable values are
    /// ignored, leaving the current value in place.
    ///
    /// Public because `tobii games set KEY VALUE` writes through the very same
    /// path the file parser uses — so the CLI cannot accept a spelling the file
    /// would reject, or vice versa.
    pub fn apply_key(&mut self, key: &str, value: &str) -> bool {
        let f = |v: &str| v.parse::<f64>().ok().filter(|x| x.is_finite());
        match key {
            "enabled" => match value.parse::<bool>() {
                Ok(b) => self.enabled = b,
                Err(_) => return false,
            },
            "rate_hz" => match f(value).filter(|v| *v > 0.0) {
                Some(v) => self.rate_hz = v,
                None => return false,
            },
            "filter_alpha" => match f(value).filter(|v| (0.0..=1.0).contains(v)) {
                Some(v) => self.filter_alpha = v,
                None => return false,
            },
            // An empty address is how the file spells "do not send there".
            "opentrack" => {
                self.opentrack = (!value.is_empty()).then(|| value.to_string());
            }
            // Port 0 is never a real destination, so it doubles as "disabled".
            "bridge_port" => match value.parse::<u16>() {
                Ok(0) => self.bridge_port = None,
                Ok(p) => self.bridge_port = Some(p),
                Err(_) => return false,
            },
            "extended_view" => match value.parse::<bool>() {
                Ok(b) => self.extended_view.enabled = b,
                Err(_) => return false,
            },
            "ev_hold_ms" => match value.parse::<u64>() {
                Ok(v) => self.extended_view.hold_ms = v,
                Err(_) => return false,
            },
            _ => {
                let (axis, rest) = if let Some(r) = key.strip_prefix("ev_yaw_") {
                    (&mut self.extended_view.yaw, r)
                } else if let Some(r) = key.strip_prefix("ev_pitch_") {
                    (&mut self.extended_view.pitch, r)
                } else {
                    return false;
                };
                match rest {
                    "deadzone_deg" => match f(value).filter(|v| *v >= 0.0) {
                        Some(v) => axis.deadzone_deg = v,
                        None => return false,
                    },
                    "input_max_deg" => match f(value).filter(|v| *v > 0.0) {
                        Some(v) => axis.input_max_deg = v,
                        None => return false,
                    },
                    "output_max_deg" => match f(value) {
                        Some(v) => axis.output_max_deg = v,
                        None => return false,
                    },
                    "clamp_deg" => match f(value).filter(|v| *v >= 0.0) {
                        Some(v) => axis.clamp_deg = v,
                        None => return false,
                    },
                    "curve" => match Curve::parse(value) {
                        Some(c) => axis.curve = c,
                        None => return false,
                    },
                    _ => return false,
                }
            }
        }
        true
    }

    /// Every settable key, for `tobii games` output and error messages.
    pub fn keys() -> &'static [&'static str] {
        &[
            "enabled",
            "rate_hz",
            "filter_alpha",
            "opentrack",
            "bridge_port",
            "extended_view",
            "ev_hold_ms",
            "ev_yaw_deadzone_deg",
            "ev_yaw_input_max_deg",
            "ev_yaw_output_max_deg",
            "ev_yaw_curve",
            "ev_yaw_clamp_deg",
            "ev_pitch_deadzone_deg",
            "ev_pitch_input_max_deg",
            "ev_pitch_output_max_deg",
            "ev_pitch_curve",
            "ev_pitch_clamp_deg",
        ]
    }
}

/// Path to the game-output config, beside `config.toml`.
pub fn games_path() -> PathBuf {
    tobii_config::config_path().with_file_name("games.toml")
}

/// Write the game-output config to an explicit path, atomically.
pub fn save_output_config_to(path: &Path, cfg: &OutputConfig) -> std::io::Result<()> {
    tobii_config::write_atomic(path, cfg.to_toml().as_bytes())
}

/// Read the game-output config from an explicit path.
///
/// A missing or unreadable file yields the defaults, so a first run needs no
/// setup and a damaged file costs tuning rather than output.
pub fn load_output_config_from(path: &Path) -> OutputConfig {
    match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => OutputConfig::from_toml(&text),
            Err(_) => OutputConfig::default(),
        },
        Err(_) => OutputConfig::default(),
    }
}

/// Write the game-output config to the default path.
pub fn save_output_config(cfg: &OutputConfig) -> std::io::Result<()> {
    save_output_config_to(&games_path(), cfg)
}

/// Read the game-output config from the default path.
pub fn load_output_config() -> OutputConfig {
    load_output_config_from(&games_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuned() -> OutputConfig {
        let base = ExtendedView::default();
        let mut c = OutputConfig {
            enabled: true,
            rate_hz: 120.0,
            filter_alpha: 0.4,
            opentrack: Some("192.168.1.7:4242".to_string()),
            bridge_port: Some(5000),
            extended_view: ExtendedView {
                hold_ms: 350,
                yaw: AxisResponse {
                    curve: Curve::Smoothstep,
                    input_max_deg: 40.0,
                    ..base.yaw
                },
                pitch: AxisResponse {
                    curve: Curve::Linear,
                    output_max_deg: 18.5,
                    ..base.pitch
                },
                ..base
            },
        };
        // Touch nothing else: the round-trip tests below are only meaningful if
        // every field differs from its default in at least one direction.
        c.extended_view.enabled = true;
        c
    }

    #[test]
    fn a_tuned_config_round_trips() {
        assert_eq!(OutputConfig::from_toml(&tuned().to_toml()), tuned());
    }

    #[test]
    fn the_defaults_round_trip() {
        let d = OutputConfig::default();
        assert_eq!(OutputConfig::from_toml(&d.to_toml()), d);
    }

    /// Both sinks off must survive the round trip: an empty address and port
    /// zero are the file's spelling of "do not send there", and a config that
    /// silently re-enabled a sink would keep sending after the user turned it
    /// off.
    #[test]
    fn disabled_sinks_round_trip_as_disabled() {
        let c = OutputConfig {
            opentrack: None,
            bridge_port: None,
            ..OutputConfig::default()
        };
        let back = OutputConfig::from_toml(&c.to_toml());
        assert_eq!(back.opentrack, None);
        assert_eq!(back.bridge_port, None);
    }

    #[test]
    fn an_absent_file_reads_as_the_defaults() {
        assert_eq!(OutputConfig::from_toml(""), OutputConfig::default());
        let dir = std::env::temp_dir().join("tobii-config-test-games-missing");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            load_output_config_from(&dir.join("games.toml")),
            OutputConfig::default()
        );
    }

    /// A typo in one tuning value should cost that value, not the whole file.
    /// Failing the entire load would take game output down over a stray letter.
    #[test]
    fn one_bad_value_does_not_take_the_rest_with_it() {
        let text = "[games]\n\
                    enabled = true\n\
                    rate_hz = fast\n\
                    ev_yaw_curve = quadratic\n\
                    filter_alpha = 0.5\n";
        let c = OutputConfig::from_toml(text);
        assert!(c.enabled, "the good keys before the bad one still applied");
        assert_eq!(c.filter_alpha, 0.5, "and the ones after");
        assert_eq!(c.rate_hz, DEFAULT_RATE_HZ, "the bad one kept its default");
        assert_eq!(c.extended_view.yaw.curve, ExtendedView::default().yaw.curve);
    }

    #[test]
    fn out_of_range_values_are_refused_rather_than_clamped_silently() {
        let mut c = OutputConfig::default();
        for (k, v) in [
            ("rate_hz", "0"),
            ("rate_hz", "-5"),
            ("rate_hz", "nan"),
            ("filter_alpha", "1.5"),
            ("filter_alpha", "-0.1"),
            ("ev_yaw_input_max_deg", "0"),
            ("ev_yaw_deadzone_deg", "-1"),
        ] {
            assert!(!c.apply_key(k, v), "{k} = {v} must be refused");
        }
        assert_eq!(c, OutputConfig::default(), "nothing may have changed");
    }

    #[test]
    fn unknown_keys_and_foreign_sections_are_ignored() {
        let text = "[display]\n\
                    width_mm = 1193\n\
                    enabled = true\n\
                    [games]\n\
                    enabled = true\n\
                    nonsense_key = 7\n\
                    [other]\n\
                    enabled = false\n";
        let c = OutputConfig::from_toml(text);
        assert!(c.enabled, "only the [games] section may be read");
    }

    /// The parser must not read `enabled = true` out of `[display]`. Reading a
    /// foreign section would make an unrelated edit turn game output on.
    #[test]
    fn keys_before_any_section_header_are_ignored() {
        let c = OutputConfig::from_toml("enabled = true\nrate_hz = 999\n");
        assert_eq!(c, OutputConfig::default());
    }

    #[test]
    fn comments_and_blank_lines_are_tolerated() {
        let text = "\n# a comment\n\n[games]\n\n  enabled = true  # trailing\n\n";
        assert!(OutputConfig::from_toml(text).enabled);
    }

    /// A quoted value must survive a `#` inside it — stripping comments blindly
    /// would truncate an address or a curve spelling.
    #[test]
    fn a_quoted_value_keeps_a_hash_inside_it() {
        let c = OutputConfig::from_toml("[games]\nopentrack = \"host#1:4242\"\n");
        assert_eq!(c.opentrack.as_deref(), Some("host#1:4242"));
    }

    #[test]
    fn every_advertised_key_is_actually_settable() {
        let mut c = OutputConfig::default();
        for key in OutputConfig::keys() {
            let value = match *key {
                "enabled" | "extended_view" => "true",
                "opentrack" => "127.0.0.1:9999",
                "bridge_port" => "4243",
                "ev_hold_ms" => "150",
                // Alpha is a fraction, so the generic numeric value below would
                // be legitimately out of range.
                "filter_alpha" => "0.3",
                k if k.ends_with("_curve") => "linear",
                _ => "3",
            };
            assert!(
                c.apply_key(key, value),
                "advertised key `{key}` was refused"
            );
        }
    }

    #[test]
    fn a_config_file_round_trips_through_disk() {
        let dir = std::env::temp_dir().join("tobii-config-test-games-roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("games.toml");
        save_output_config_to(&path, &tuned()).expect("save");
        assert_eq!(load_output_config_from(&path), tuned());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_games_file_sits_beside_config_toml() {
        assert!(games_path().ends_with("tobii-linux/games.toml"));
    }
}
