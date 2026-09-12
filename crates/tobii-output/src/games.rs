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
    /// moment it is installed would be a surprise.
    ///
    /// `tobii headpose` reads it too, and this comment used to say it did not.
    /// Running the command is indeed the opt-in for *sending head pose* — that
    /// is all it did in v0.1.0 — but not for gaze steering the view or for a
    /// second socket to the FreeTrack bridge, which is what these defaults turn
    /// on. Somebody who upgrades and runs the same command they always have
    /// gets the same thing they always got, until they turn this on or pass
    /// `--extended-view`.
    pub enabled: bool,
    /// Frames per second delivered to sinks. Sampling and smoothing still run
    /// at the device's full rate; this throttles only the wire.
    pub rate_hz: f64,
    /// Smoothing strength, `0.0`–`1.0`; higher follows the head more closely.
    pub filter_alpha: f64,
    /// How far the head's position may move between two frames and still be
    /// believed, in millimetres.
    ///
    /// The filter holds a sample that steps further than this instead of
    /// blending it in, which is what keeps a decode fault or a reacquisition
    /// onto somebody else from sweeping the view. It is a key rather than a
    /// constant for one reason: `tobii_headpose::filter::DEFAULT_MAX_STEP_MM`
    /// says in its own documentation that it is **not** measured against real
    /// head motion, because no recording in this repository has a head in it.
    /// Until one does, the person who can tell a rejected lunge from a rejected
    /// glitch is the person it happened to, and this is what they turn the gate
    /// off with (a very large value) or tighten it with. See that constant for
    /// what the 150 mm default is derived from.
    pub filter_max_step_mm: f64,
    /// opentrack endpoint, or `None` to not send there.
    pub opentrack: Option<String>,
    /// Loopback port for the Wine bridge, or `None` to not send there.
    pub bridge_port: Option<u16>,
    /// The composed head angle, in degrees, that drives each joystick axis to
    /// full deflection — yaw, pitch, roll.
    ///
    /// # Why this stage has to exist
    ///
    /// The joystick axes span ±180°/±90°/±180°, because that is the physical
    /// range of the quantity and what every head-tracking guide assumes. A head
    /// does not cover it: with Extended View at *Normal* and a 20° head turn,
    /// composed yaw reaches about 65°, which is **36%** of the axis. Roll gets
    /// no Extended View contribution at all, so a 15° head tilt is **8%**.
    ///
    /// opentrack and TrackIR both have an amplification stage before the wire
    /// for exactly this reason — opentrack's own docs describe mapping 15° of
    /// physical yaw onto 90–180° of camera rotation, and NaturalPoint tell
    /// TrackIR users to shape the motion curve rather than move their heads
    /// further. We copied opentrack's wire scale and not its curve.
    ///
    /// Defaults of 70/35/20 put "look at the edge of the screen and turn your
    /// head slightly" at roughly full deflection. Erring hot is deliberate:
    /// every game with an axis-tuning panel can attenuate a strong signal
    /// trivially, and several — Elite Dangerous exposes a deadzone and nothing
    /// else — cannot amplify a weak one at all.
    pub joystick_full_deg: [f64; 3],
    /// Whether to present a virtual joystick on `/dev/uinput`.
    ///
    /// On by default, unlike every other sink, because it is the only one that
    /// does something on a machine with nothing else installed: the other two
    /// are fire-and-forget UDP to opentrack and to a Wine prefix, so with
    /// neither present, turning game output on would otherwise have no effect
    /// whatsoever and no way to tell that apart from a fault.
    pub joystick: bool,
    /// Gaze-driven camera offset.
    pub extended_view: ExtendedView,
}

impl Default for OutputConfig {
    fn default() -> Self {
        OutputConfig {
            enabled: false,
            rate_hz: DEFAULT_RATE_HZ,
            filter_alpha: tobii_headpose::filter::DEFAULT_ALPHA,
            filter_max_step_mm: tobii_headpose::filter::DEFAULT_MAX_STEP_MM,
            opentrack: Some(DEFAULT_OPENTRACK_ADDR.to_string()),
            bridge_port: Some(DEFAULT_BRIDGE_PORT),
            joystick_full_deg: [70.0, 35.0, 20.0],
            joystick: true,
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
            "filter_max_step_mm = {}\n",
            self.filter_max_step_mm
        ));
        s.push_str(&format!(
            "opentrack = \"{}\"\n",
            self.opentrack.as_deref().unwrap_or("")
        ));
        s.push_str(&format!(
            "bridge_port = {}\n",
            self.bridge_port.unwrap_or(0)
        ));
        s.push_str(&format!("joystick = {}\n", self.joystick));
        s.push_str(&format!(
            "joystick_yaw_full_deg = {}\n",
            self.joystick_full_deg[0]
        ));
        s.push_str(&format!(
            "joystick_pitch_full_deg = {}\n",
            self.joystick_full_deg[1]
        ));
        s.push_str(&format!(
            "joystick_roll_full_deg = {}\n",
            self.joystick_full_deg[2]
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
            // Positive and finite is the whole rule, and there is deliberately
            // no upper bound: a very large value is how somebody turns the gate
            // off to find out whether it is what is holding their pose. A zero
            // or negative limit would refuse every sample, so it is refused
            // here rather than silently repaired — `PoseFilter::with_max_step_mm`
            // falls back to the default for one, and a key that quietly did
            // something other than what it says is worse than a rejection.
            "filter_max_step_mm" => match f(value).filter(|v| *v > 0.0) {
                Some(v) => self.filter_max_step_mm = v,
                None => return false,
            },
            // An empty address is how the file spells "do not send there".
            //
            // A newline is refused rather than escaped. `to_toml` writes this
            // into a quoted string on one line and `from_toml` reads one line at
            // a time, so an embedded newline does not merely corrupt the
            // address: the injected second line can begin with `[`, which turns
            // `in_games` off and makes the parser silently discard every key
            // written after this one — the bridge port, the joystick settings
            // and all twelve Extended View values, reverted to defaults with no
            // error anywhere. Quotes and `#` need no such guard; both already
            // round-trip, which `a_quoted_value_keeps_a_hash_inside_it` covers.
            "opentrack" => {
                if value.contains(['\n', '\r']) {
                    return false;
                }
                self.opentrack = (!value.is_empty()).then(|| value.to_string());
            }
            // Port 0 is never a real destination, so it doubles as "disabled".
            "bridge_port" => match value.parse::<u16>() {
                Ok(0) => self.bridge_port = None,
                Ok(p) => self.bridge_port = Some(p),
                Err(_) => return false,
            },
            "joystick" => match value.parse::<bool>() {
                Ok(b) => self.joystick = b,
                Err(_) => return false,
            },
            // Zero or negative would be a division by zero downstream, and
            // "full deflection at no head movement" is not a thing to want.
            "joystick_yaw_full_deg" => match f(value).filter(|v| *v > 0.0) {
                Some(v) => self.joystick_full_deg[0] = v,
                None => return false,
            },
            "joystick_pitch_full_deg" => match f(value).filter(|v| *v > 0.0) {
                Some(v) => self.joystick_full_deg[1] = v,
                None => return false,
            },
            "joystick_roll_full_deg" => match f(value).filter(|v| *v > 0.0) {
                Some(v) => self.joystick_full_deg[2] = v,
                None => return false,
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
    ///
    /// A hand-written list, guarded by
    /// `the_advertised_keys_are_exactly_the_keys_the_file_writes` rather than
    /// derived — because it drifted the moment it was not. `joystick` was added
    /// to the struct, to `to_toml`, to `apply_key` and to the GUI, and not
    /// here; the CLI meanwhile grew a second list scraped out of `to_toml`,
    /// under a comment claiming it existed so there would not be two lists.
    pub fn keys() -> &'static [&'static str] {
        &[
            "enabled",
            "rate_hz",
            "filter_alpha",
            "filter_max_step_mm",
            "opentrack",
            "bridge_port",
            "joystick",
            "joystick_yaw_full_deg",
            "joystick_pitch_full_deg",
            "joystick_roll_full_deg",
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
    tobii_config::config_path().with_file_name(tobii_config::paths::GAMES_TOML)
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
            filter_max_step_mm: 220.0,
            opentrack: Some("192.168.1.7:4242".to_string()),
            bridge_port: Some(5000),
            // Off, because the default is on: the round-trip tests below only
            // mean anything if every field differs from its default.
            joystick: false,
            joystick_full_deg: [55.0, 30.0, 18.0],
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
            // A limit of zero or less would refuse every sample, and the
            // filter would answer with a default-constructed pose for three
            // frames at a time.
            ("filter_max_step_mm", "0"),
            ("filter_max_step_mm", "-1"),
            ("filter_max_step_mm", "nan"),
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

    /// A value that would break the file's one-key-per-line shape is refused,
    /// because the damage is silent and reaches far past the value itself.
    #[test]
    fn a_value_containing_a_newline_is_refused_rather_than_written() {
        let mut c = OutputConfig::default();
        assert!(!c.apply_key("opentrack", "127.0.0.1:4242\n[other]"));
        assert_eq!(
            c.opentrack.as_deref(),
            Some(DEFAULT_OPENTRACK_ADDR),
            "a refused value must leave the old one in place"
        );
        assert!(!c.apply_key("opentrack", "a\rb"));

        // The damage it prevents: a `[` on the injected line ends the section,
        // and every key `to_toml` writes after `opentrack` is then dropped.
        let injected = OutputConfig {
            opentrack: Some("host\n[x]".to_string()),
            joystick: false,
            ..OutputConfig::default()
        };
        let back = OutputConfig::from_toml(&injected.to_toml());
        assert_ne!(
            back.joystick, injected.joystick,
            "premise: an injected section header silently reverts later keys"
        );
    }

    /// A quoted value must survive a `#` inside it — stripping comments blindly
    /// would truncate an address or a curve spelling.
    #[test]
    fn a_quoted_value_keeps_a_hash_inside_it() {
        let c = OutputConfig::from_toml("[games]\nopentrack = \"host#1:4242\"\n");
        assert_eq!(c.opentrack.as_deref(), Some("host#1:4242"));
    }

    /// The list and the file must name the same set. Adding a field to
    /// `OutputConfig` touches five places, and this is the one with nothing to
    /// remind you: a missing key is not a compile error, it just quietly stops
    /// being mentioned by `tobii games` and by the error a bad key prints.
    #[test]
    fn the_advertised_keys_are_exactly_the_keys_the_file_writes() {
        let doc = OutputConfig::default().to_toml();
        let written: Vec<&str> = doc
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| l.split_once(" = ").map(|(k, _)| k.trim()))
            .collect();
        assert_eq!(
            written,
            OutputConfig::keys(),
            "`keys()` and `to_toml` disagree — one of them was not updated"
        );
    }

    #[test]
    fn every_advertised_key_is_actually_settable() {
        let mut c = OutputConfig::default();
        for key in OutputConfig::keys() {
            let value = match *key {
                "enabled" | "extended_view" | "joystick" => "true",
                k if k.starts_with("joystick_") => "45",
                "opentrack" => "127.0.0.1:9999",
                "bridge_port" => "4243",
                "ev_hold_ms" => "150",
                // Alpha is a fraction, so the generic numeric value below would
                // be legitimately out of range.
                "filter_alpha" => "0.3",
                "filter_max_step_mm" => "200",
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
