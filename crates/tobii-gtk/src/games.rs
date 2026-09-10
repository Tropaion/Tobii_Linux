//! The hub's "Head tracking for games" row.
//!
//! A switch for whether game output runs at all, three named strengths for how
//! hard gaze steers the camera, and a status line.
//!
//! # The status line is the point
//!
//! Game output is a pipeline of parts that are each silent when absent — output
//! switched off, no destination configured, nothing running that wants the
//! tracker — and every one of them presents in game as "head tracking does not
//! work". Naming which part is missing is the difference between a setting and
//! a support thread.
//!
//! This row is adapted from an older branch where the same line ended in "the
//! `tobii serve` daemon is not running", because on that branch a separate
//! process owned the device. There is no daemon here: the hub itself publishes,
//! so the question is not whether some other process is alive but whether
//! anything is currently asking for the tracker — which the hub knows exactly,
//! rather than having to probe a socket to guess.

use gtk::prelude::*;
use gtk::{glib, Align, CheckButton, Label, Orientation, Switch};

use std::sync::{Arc, Mutex};

use tobii_output::games::{load_output_config, save_output_config, OutputConfig};

use crate::device::JoystickStatus;

/// Extended View strength presets, as `(yaw output_max, pitch output_max)` in
/// degrees.
///
/// Three named steps rather than ten numeric fields because the underlying
/// question — how far should the view swing when I look at the edge — is one a
/// person answers by trying it, not by choosing a number. The two axes differ
/// because a screen subtends far more horizontal angle than vertical.
const STRENGTHS: [(&str, f64, f64); 3] = [
    ("Subtle", 25.0, 15.0),
    ("Normal", 45.0, 25.0),
    ("Strong", 70.0, 40.0),
];

/// Which preset a config's values correspond to, if any.
///
/// Matched on yaw alone: it is the axis the presets differ most on, and a
/// config hand-tuned past recognition should leave every preset unselected
/// rather than have one silently claim it.
pub fn strength_index(cfg: &OutputConfig) -> Option<usize> {
    STRENGTHS
        .iter()
        .position(|(_, yaw, _)| (cfg.extended_view.yaw.output_max_deg - yaw).abs() < 0.01)
}

/// What the status line should say, given what is actually set up.
///
/// Pure, so the wording can be tested — and the interesting cases are the
/// half-configured ones, each of which otherwise reads in game as "it does not
/// work" with nothing to tell them apart.
///
/// `tracker_on` is whether the device session is open right now. It is not a
/// health check: with game output configured and nothing playing, the tracker is
/// SUPPOSED to be off, and saying so is more useful than a green light that
/// means nothing.
pub fn status_text(cfg: &OutputConfig, tracker_on: bool, joystick: &JoystickStatus) -> String {
    if !cfg.enabled {
        return "Off. Turn on to send head tracking to games.".to_string();
    }
    let mut sinks: Vec<String> = Vec::new();
    // Reported from what the device thread actually did, never from the
    // checkbox. The checkbox is a request; `/dev/uinput` can refuse it, and a
    // line that says "sending to a virtual joystick" when none exists is
    // exactly the support thread this row was written to prevent.
    let mut trouble: Option<String> = None;
    if cfg.joystick {
        match joystick {
            JoystickStatus::Present => sinks.push("a virtual joystick".to_string()),
            // Wanted, and the device thread has not got to it yet — it polls
            // the setting about once a second. Saying so beats a flicker of
            // "no destination is configured" in the second after ticking the
            // box.
            JoystickStatus::Off => sinks.push("a virtual joystick (starting)".to_string()),
            JoystickStatus::Failed(why) => {
                trouble = Some(format!("The virtual joystick could not be created — {why}"));
            }
        }
    }
    if let Some(addr) = &cfg.opentrack {
        sinks.push(format!("opentrack ({addr})"));
    }
    if let Some(port) = cfg.bridge_port {
        sinks.push(format!("the Wine bridge (port {port})"));
    }
    if sinks.is_empty() {
        return match trouble {
            Some(t) => format!("{t}. Nothing else is configured, so nothing will receive it."),
            None => "On, but no destination is configured — nothing will receive it.".to_string(),
        };
    }
    let to = sinks.join(" and ");
    let base = if tracker_on {
        format!("Sending to {to}.")
    } else {
        // Not a fault. The tracker is off because nothing is asking for it,
        // which is the whole standby behaviour — so the line says what to do
        // rather than implying something is broken.
        format!("Ready to send to {to}. Starts when a game asks: tobii game -- <command>")
    };
    match trouble {
        Some(t) => format!("{base} {t}."),
        None => base,
    }
}

/// The control block for the row.
pub struct GamesRow {
    pub controls: gtk::Box,
    status: Label,
    joystick: Arc<Mutex<JoystickStatus>>,
}

impl GamesRow {
    /// Build the row, seeded from the saved configuration.
    ///
    /// `build` rather than `new` because it reads config from disk and
    /// constructs widgets; a `Default` impl — which clippy asks for on a `new()`
    /// taking no arguments — would advertise a cheap, side-effect-free
    /// constructor that this is not.
    pub fn build(joystick: Arc<Mutex<JoystickStatus>>) -> GamesRow {
        let cfg = load_output_config();

        let sw = Switch::new();
        sw.set_valign(Align::Center);
        sw.set_active(cfg.enabled);
        sw.set_tooltip_text(Some(
            "Send head tracking and gaze to games, over opentrack or the Wine bridge",
        ));

        let status = Label::new(None);
        status.set_halign(Align::Start);
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.set_max_width_chars(44);
        status.add_css_class("section-desc");

        // Separate labels rather than a CheckButton's built-in one: the tops of
        // tall glyphs clip on this theme, which is why every radio in this hub
        // is built this way.
        let strength_ctl = gtk::Box::new(Orientation::Horizontal, 16);
        let mut buttons: Vec<CheckButton> = Vec::new();
        for (i, (name, _, _)) in STRENGTHS.iter().enumerate() {
            let cb = CheckButton::new();
            if let Some(first) = buttons.first() {
                cb.set_group(Some(first));
            }
            cb.set_valign(Align::Center);
            let lbl = Label::new(Some(name));
            lbl.set_valign(Align::Center);
            lbl.set_margin_top(2);
            lbl.set_margin_bottom(2);
            let row = gtk::Box::new(Orientation::Horizontal, 5);
            row.append(&cb);
            row.append(&lbl);
            strength_ctl.append(&row);
            if strength_index(&cfg) == Some(i) {
                cb.set_active(true);
            }
            buttons.push(cb);
        }

        // Every write re-reads the file first. Two controls edit the same
        // config, and holding a copy in each would make whichever was touched
        // second overwrite the other's change.
        let refresh = {
            let status = status.clone();
            let joystick = Arc::clone(&joystick);
            move || {
                let js = joystick.lock().unwrap().clone();
                status.set_text(&status_text(&load_output_config(), false, &js));
            }
        };

        {
            let refresh = refresh.clone();
            sw.connect_state_set(move |_, on| {
                let mut cfg = load_output_config();
                cfg.enabled = on;
                if let Err(e) = save_output_config(&cfg) {
                    tobii_diagnostics::log::warn(&format!(
                        "could not save the game-output setting: {e}"
                    ));
                }
                refresh();
                glib::Propagation::Proceed
            });
        }

        for (i, cb) in buttons.iter().enumerate() {
            let refresh = refresh.clone();
            cb.connect_toggled(move |c| {
                if !c.is_active() {
                    return;
                }
                let (_, yaw, pitch) = STRENGTHS[i];
                let mut cfg = load_output_config();
                cfg.extended_view.yaw.output_max_deg = yaw;
                cfg.extended_view.pitch.output_max_deg = pitch;
                if let Err(e) = save_output_config(&cfg) {
                    tobii_diagnostics::log::warn(&format!(
                        "could not save the game-output strength: {e}"
                    ));
                }
                refresh();
            });
        }

        // Its own row rather than a fourth item on the top row: the top row is
        // already a switch and three radios, and the hub is laid out to stay
        // narrow.
        let joy = CheckButton::new();
        joy.set_active(cfg.joystick);
        joy.set_valign(Align::Center);
        let joy_lbl = Label::new(Some("Virtual joystick"));
        joy_lbl.set_valign(Align::Center);
        joy_lbl.set_margin_top(2);
        joy_lbl.set_margin_bottom(2);
        let joy_row = gtk::Box::new(Orientation::Horizontal, 5);
        joy_row.append(&joy);
        joy_row.append(&joy_lbl);
        joy_row.set_tooltip_text(Some(
            "Present head pose and gaze as a game controller, for games with no \
             head-tracking support. Works in native and Proton games without Wine \
             or opentrack.",
        ));
        {
            let refresh = refresh.clone();
            joy.connect_toggled(move |c| {
                let mut cfg = load_output_config();
                cfg.joystick = c.is_active();
                if let Err(e) = save_output_config(&cfg) {
                    tobii_diagnostics::log::warn(&format!(
                        "could not save the virtual-joystick setting: {e}"
                    ));
                }
                refresh();
            });
        }

        let controls = gtk::Box::new(Orientation::Vertical, 8);
        let top = gtk::Box::new(Orientation::Horizontal, 16);
        top.append(&sw);
        top.append(&strength_ctl);
        controls.append(&top);
        controls.append(&joy_row);
        controls.append(&status);

        let row = GamesRow {
            controls,
            status,
            joystick,
        };
        row.refresh(false);
        row
    }

    /// Re-read the config and update the status line.
    ///
    /// Driven from the hub tick, so turning game output on with `tobii games`
    /// in a terminal — or a game starting and stopping — is reflected without
    /// reopening the window, which is exactly how somebody setting this up for
    /// the first time is working.
    pub fn refresh(&self, tracker_on: bool) {
        let js = self.joystick.lock().unwrap().clone();
        self.status
            .set_text(&status_text(&load_output_config(), tracker_on, &js));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config with only the sinks named. `joystick` is off rather than
    /// inherited from the defaults, so these cases keep testing what they were
    /// written to test — with it on, "no destination is configured" would be
    /// unreachable.
    /// Game output on, with the joystick as its only destination.
    fn joystick_only() -> OutputConfig {
        OutputConfig {
            enabled: true,
            opentrack: None,
            bridge_port: None,
            joystick: true,
            ..OutputConfig::default()
        }
    }

    fn cfg_with(enabled: bool, opentrack: Option<&str>, bridge: Option<u16>) -> OutputConfig {
        OutputConfig {
            enabled,
            opentrack: opentrack.map(str::to_string),
            bridge_port: bridge,
            joystick: false,
            ..OutputConfig::default()
        }
    }

    /// Every half-configured state has to be distinguishable. All of them look
    /// identical in game — the camera does not move — so if this line does not
    /// separate them, nothing does.
    #[test]
    fn each_broken_state_says_which_part_is_missing() {
        let off = status_text(
            &cfg_with(false, Some("127.0.0.1:4242"), None),
            false,
            &JoystickStatus::Off,
        );
        assert!(off.contains("Off"), "{off}");

        let no_sink = status_text(&cfg_with(true, None, None), true, &JoystickStatus::Off);
        assert!(no_sink.contains("no destination"), "{no_sink}");

        let ready = status_text(
            &cfg_with(true, Some("127.0.0.1:4242"), None),
            false,
            &JoystickStatus::Off,
        );
        assert!(ready.contains("Ready"), "{ready}");
        assert!(
            ready.contains("tobii game"),
            "a user who sees 'Ready' needs to be told what starts it: {ready}"
        );

        let sending = status_text(
            &cfg_with(true, Some("127.0.0.1:4242"), None),
            true,
            &JoystickStatus::Off,
        );
        assert!(sending.starts_with("Sending"), "{sending}");

        // And they are genuinely different sentences, not four spellings of one.
        let all = [off, no_sink, ready, sending];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two states read identically");
            }
        }
    }

    /// "Ready" is not a fault. With game output configured and nothing playing,
    /// the tracker is SUPPOSED to be off — a status line implying otherwise
    /// would send people looking for a problem that is the design working.
    #[test]
    fn a_dark_tracker_is_not_reported_as_a_problem() {
        let s = status_text(
            &cfg_with(true, Some("127.0.0.1:4242"), None),
            false,
            &JoystickStatus::Off,
        );
        for alarming in ["not running", "error", "failed", "cannot", "problem"] {
            assert!(
                !s.to_lowercase().contains(alarming),
                "{alarming:?} makes standby look broken: {s}"
            );
        }
    }

    /// Both destinations are named when both are set, so somebody debugging
    /// knows which one to look at.
    #[test]
    fn both_destinations_are_named() {
        let s = status_text(
            &cfg_with(true, Some("127.0.0.1:4242"), Some(4243)),
            true,
            &JoystickStatus::Off,
        );
        assert!(s.contains("4242") && s.contains("4243"), "{s}");
    }

    /// The joystick is the sink that works with nothing else installed, so a
    /// user reading this line with no opentrack and no Wine prefix has to be
    /// told that something is nevertheless receiving.
    #[test]
    fn the_virtual_joystick_is_named_like_any_other_destination() {
        let s = status_text(&joystick_only(), true, &JoystickStatus::Present);
        assert!(s.contains("joystick"), "{s}");
        assert!(
            !s.contains("no destination"),
            "a joystick is a destination: {s}"
        );
    }

    /// The checkbox is a request, not an outcome: `/dev/uinput` is root-only on
    /// most distributions and can refuse it. Reporting a controller that does
    /// not exist is precisely the support thread this row was written to
    /// prevent — the user would go looking for it in their game's bind list.
    #[test]
    fn a_joystick_that_could_not_be_created_is_not_reported_as_a_destination() {
        let failed = JoystickStatus::Failed("/dev/uinput: Permission denied".into());

        let alone = status_text(&joystick_only(), true, &failed);
        assert!(
            !alone.contains("Sending to a virtual joystick"),
            "claimed a device that does not exist: {alone}"
        );
        assert!(alone.contains("could not be created"), "{alone}");
        assert!(
            alone.contains("Permission denied"),
            "the reason is the whole point — three failures need three fixes: {alone}"
        );

        // With another sink working, the failure is reported ALONGSIDE it
        // rather than replacing it: opentrack really is still receiving.
        let mut with_udp = joystick_only();
        with_udp.opentrack = Some("127.0.0.1:4242".to_string());
        let both = status_text(&with_udp, true, &failed);
        assert!(both.starts_with("Sending to opentrack"), "{both}");
        assert!(both.contains("could not be created"), "{both}");
    }

    /// The device thread polls the setting about once a second, so there is a
    /// window where the box is ticked and the device does not exist yet.
    /// Reading "no destination is configured" during it would be wrong and
    /// alarming.
    #[test]
    fn a_joystick_that_is_still_starting_does_not_read_as_missing() {
        let s = status_text(&joystick_only(), false, &JoystickStatus::Off);
        assert!(!s.contains("no destination"), "{s}");
        assert!(s.contains("starting"), "{s}");
    }

    #[test]
    fn the_presets_are_recognised_and_a_hand_tuned_config_is_not() {
        for (i, (_, yaw, pitch)) in STRENGTHS.iter().enumerate() {
            let mut c = OutputConfig::default();
            c.extended_view.yaw.output_max_deg = *yaw;
            c.extended_view.pitch.output_max_deg = *pitch;
            assert_eq!(strength_index(&c), Some(i));
        }
        let mut odd = OutputConfig::default();
        odd.extended_view.yaw.output_max_deg = 33.3;
        assert_eq!(
            strength_index(&odd),
            None,
            "a hand-tuned config must leave every preset unselected rather than \
             have one claim it"
        );
    }
}
