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

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tobii_output::games::{load_output_config, save_output_config, OutputConfig};

use crate::device::{Demand, JoystickStatus};
use crate::outputs::{recentre_decision, Recentring};

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

/// Whether simply starting opentrack is enough to switch the tracker on.
///
/// Asks [`crate::outputs::watch_target`] rather than reading the two settings
/// again: this line and the thing it describes have to agree, and the way they
/// stop agreeing is somebody adding a third condition in one place. There is
/// one predicate, and the status line is a reader of it.
fn watching_opentrack(cfg: &OutputConfig) -> bool {
    crate::outputs::watch_target(cfg).is_some()
}

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
        //
        // What to do depends on whether the port watch is on, and the honest
        // answer changed when it arrived: with an opentrack address configured
        // and the watch on, simply starting opentrack is enough, and this line
        // used to send those users off to wrap something in `tobii game` for no
        // reason. The wrapper is still named, because it is the answer for
        // everything that is not an opentrack listener.
        let starter = if watching_opentrack(cfg) {
            "Starts when opentrack opens the port, or a game asks: tobii game -- <command>"
        } else {
            "Starts when a game asks: tobii game -- <command>"
        };
        format!("Ready to send to {to}. {starter}")
    };
    match trouble {
        Some(t) => format!("{base} {t}."),
        None => base,
    }
}

/// The same sentence, starting with a capital.
///
/// The wording of a recentre's outcome lives on
/// [`tobii_output::pipeline::RecentreOutcome`] so that this window and `tobii
/// headpose` cannot explain the same refusal differently. It is written for a
/// log line, which is where the CLI puts it; here it is a sentence in a
/// paragraph of other sentences, and the only difference is the first letter.
fn sentence(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// A checkbox with its label beside it, as one row.
///
/// Separate labels rather than a `CheckButton`'s built-in one: the tops of tall
/// glyphs clip on this theme, which is why every radio in this hub is built
/// this way. Same fault and same workaround as [`crate::widget::button`], which
/// records what has already been ruled out.
fn check_row(text: &str) -> (CheckButton, gtk::Box) {
    let cb = CheckButton::new();
    cb.set_valign(Align::Center);
    let lbl = Label::new(Some(text));
    lbl.set_valign(Align::Center);
    lbl.set_margin_top(2);
    lbl.set_margin_bottom(2);
    let row = gtk::Box::new(Orientation::Horizontal, 5);
    row.append(&cb);
    row.append(&lbl);
    (cb, row)
}

/// Read the settings, change them, write them back.
///
/// Every write re-reads the file first. Three controls edit the same config —
/// and `tobii games` in a terminal edits it too — so holding a copy in each
/// would make whichever was touched second overwrite the other's change.
///
/// `what` names the setting in the warning, so a failed write says which
/// control the user just touched did not take.
fn edit_config(what: &str, change: impl FnOnce(&mut OutputConfig)) {
    let mut cfg = load_output_config();
    change(&mut cfg);
    if let Err(e) = save_output_config(&cfg) {
        tobii_diagnostics::log::warn(&format!("could not save the {what}: {e}"));
    }
}

/// The control block for the row.
pub struct GamesRow {
    pub controls: gtk::Box,
    status: Label,
    /// The controls, held so [`GamesRow::refresh`] can put them back in step
    /// with the file. They are not only built and forgotten: `tobii games set`
    /// and the hub edit the same settings, and the status line under these
    /// controls is driven from the file — so a control that never re-reads it
    /// ends up contradicting the sentence directly beneath it.
    enabled: Switch,
    joy: CheckButton,
    strength: Vec<CheckButton>,
    joystick: Arc<Mutex<JoystickStatus>>,
    /// The recentre button, so [`GamesRow::refresh`] can make it insensitive
    /// exactly when [`recentre_decision`] would refuse it. A control that is
    /// pressable and then declines is worse than one that is plainly not
    /// available yet.
    recentre: gtk::Button,
    /// Where the request is left and the outcome comes back from.
    recentring: Recentring,
    demand: Demand,
    /// What `refresh` last knew about the tracker, for the click handler.
    ///
    /// The button's sensitivity is set from the same fact, but sensitivity is a
    /// 33 ms-old snapshot: the honest thing for the click to test is the value
    /// itself, and then the decision is made by the one function the socket
    /// path also uses.
    tracker_on: Rc<Cell<bool>>,
    /// And whether anything is composing a pose to recentre, from the same
    /// 33 ms refresh. Carried rather than read in the handler for the same
    /// reason the config is read once per refresh here: a click handler that
    /// reads `games.toml` itself is a second reader of the same file with its
    /// own idea of what it says.
    composing: Rc<Cell<bool>>,
}

impl GamesRow {
    /// Build the row, seeded from the saved configuration.
    ///
    /// `build` rather than `new` because it reads config from disk and
    /// constructs widgets; a `Default` impl — which clippy asks for on a `new()`
    /// taking no arguments — would advertise a cheap, side-effect-free
    /// constructor that this is not.
    pub fn build(
        joystick: Arc<Mutex<JoystickStatus>>,
        recentring: Recentring,
        demand: Demand,
    ) -> GamesRow {
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

        let strength_ctl = gtk::Box::new(Orientation::Horizontal, 16);
        let mut buttons: Vec<CheckButton> = Vec::new();
        let selected = strength_index(&cfg);
        for (i, (name, _, _)) in STRENGTHS.iter().enumerate() {
            let (cb, row) = check_row(name);
            if let Some(first) = buttons.first() {
                cb.set_group(Some(first));
            }
            strength_ctl.append(&row);
            if selected == Some(i) {
                cb.set_active(true);
            }
            buttons.push(cb);
        }

        // The status line, put back in step after every write. `tracker_on` is
        // false because a save handler has no way to know: the 33 ms hub tick
        // calls `GamesRow::refresh` with the truth immediately afterwards.
        let refresh = {
            let status = status.clone();
            let joystick = Arc::clone(&joystick);
            let recentring = recentring.clone();
            move || {
                let js = joystick.lock().unwrap().clone();
                // What a recentre just said outranks the standing status for a
                // few seconds: it is the answer to something the user did, and
                // it is the only place the answer appears.
                let text = match recentring.message(Instant::now()) {
                    Some(m) => sentence(&m),
                    None => status_text(&load_output_config(), false, &js),
                };
                status.set_text(&text);
            }
        };

        {
            let refresh = refresh.clone();
            sw.connect_state_set(move |_, on| {
                edit_config("game-output setting", |cfg| cfg.enabled = on);
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
                edit_config("game-output strength", |cfg| {
                    cfg.extended_view.yaw.output_max_deg = yaw;
                    cfg.extended_view.pitch.output_max_deg = pitch;
                });
                refresh();
            });
        }

        // Its own row rather than a fourth item on the top row: the top row is
        // already a switch and three radios, and the hub is laid out to stay
        // narrow.
        let (joy, joy_row) = check_row("Virtual joystick");
        joy.set_active(cfg.joystick);
        joy_row.set_tooltip_text(Some(
            "Present head pose and gaze as a game controller, for games with no \
             head-tracking support. Works in native and Proton games without Wine \
             or opentrack.",
        ));
        {
            let refresh = refresh.clone();
            joy.connect_toggled(move |c| {
                let on = c.is_active();
                edit_config("virtual-joystick setting", |cfg| cfg.joystick = on);
                refresh();
            });
        }

        // A button, not a setting: it acts on the pose being sent right now,
        // and it is the only control in this window that does.
        let tracker_on = Rc::new(Cell::new(false));
        let composing = Rc::new(Cell::new(crate::outputs::composing(&cfg)));
        let recentre = crate::widget::button("Recentre view");
        recentre.set_tooltip_text(Some(
            "Sit the way you play, look at the centre of the screen, and press this: \
             the head angle you are holding becomes straight ahead in the game. Hold \
             still for a second while it measures. Games with their own centring key \
             still have it; this fixes a tracker that is not quite square to you.",
        ));
        {
            let recentring = recentring.clone();
            let demand = demand.clone();
            let refresh = refresh.clone();
            let tracker_on = tracker_on.clone();
            let composing = composing.clone();
            recentre.connect_clicked(move |_| {
                let now = Instant::now();
                // The same decision the socket path makes, from the same
                // function: a recentre refused for another program and taken
                // silently for the hub would be two rules for one action.
                match recentre_decision(&demand.reasons(), tracker_on.get(), composing.get()) {
                    Ok(()) => {
                        recentring.request(now);
                        // Said here rather than left to the outcome a second
                        // later, because holding still is the part the user has
                        // to do and they have to be told while it matters.
                        recentring.report(
                            "measuring — hold still and look at the centre of the screen"
                                .to_string(),
                            now,
                        );
                    }
                    Err(why) => recentring.report(why, now),
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
        let recentre_row = gtk::Box::new(Orientation::Horizontal, 8);
        recentre_row.append(&recentre);
        controls.append(&recentre_row);
        controls.append(&status);

        let row = GamesRow {
            controls,
            status,
            enabled: sw,
            joy,
            strength: buttons,
            joystick,
            recentre,
            recentring,
            demand,
            tracker_on,
            composing,
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
        let cfg = load_output_config();

        // Compared before writing, which is what makes this safe to call from
        // the 33 ms hub tick: GTK emits `state-set` and `toggled` only on an
        // actual change, so an equal write is silent and cannot re-enter the
        // save handlers that are listening to these very widgets.
        if self.enabled.is_active() != cfg.enabled {
            self.enabled.set_active(cfg.enabled);
        }
        if self.joy.is_active() != cfg.joystick {
            self.joy.set_active(cfg.joystick);
        }
        if let Some(i) = strength_index(&cfg) {
            if let Some(b) = self.strength.get(i) {
                if !b.is_active() {
                    b.set_active(true);
                }
            }
        }

        // A recentre needs a head to measure, something composing a pose out of
        // it, and a user who is not looking at a calibration dot — so the button
        // says so by being unavailable rather than by refusing after the press.
        self.tracker_on.set(tracker_on);
        self.composing.set(crate::outputs::composing(&cfg));
        self.recentre.set_sensitive(
            recentre_decision(&self.demand.reasons(), tracker_on, self.composing.get()).is_ok(),
        );

        let js = self.joystick.lock().unwrap().clone();
        let text = match self.recentring.message(Instant::now()) {
            Some(m) => sentence(&m),
            None => status_text(&cfg, tracker_on, &js),
        };
        self.status.set_text(&text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`status_text`] for a config that does not ask for a joystick, so the
    /// status it is given is never read. Spelling `JoystickStatus::Off` at
    /// every one of these call sites buried the case each was asserting.
    fn text(cfg: &OutputConfig, tracker_on: bool) -> String {
        status_text(cfg, tracker_on, &JoystickStatus::Off)
    }

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

    /// A config with only the sinks named. `joystick` is off rather than
    /// inherited from the defaults — where it is ON — so these cases keep
    /// testing what they were written to test: with it on, "no destination is
    /// configured" would be unreachable.
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
    ///
    /// The joystick status is `Off` throughout, and never consulted: these
    /// configs come from `cfg_with`, which does not ask for one.
    #[test]
    fn each_broken_state_says_which_part_is_missing() {
        let off = text(&cfg_with(false, Some("127.0.0.1:4242"), None), false);
        assert!(off.contains("Off"), "{off}");

        let no_sink = text(&cfg_with(true, None, None), true);
        assert!(no_sink.contains("no destination"), "{no_sink}");

        let ready = text(&cfg_with(true, Some("127.0.0.1:4242"), None), false);
        assert!(ready.contains("Ready"), "{ready}");
        assert!(
            ready.contains("tobii game"),
            "a user who sees 'Ready' needs to be told what starts it: {ready}"
        );

        let sending = text(&cfg_with(true, Some("127.0.0.1:4242"), None), true);
        assert!(sending.starts_with("Sending"), "{sending}");

        // And they are genuinely different sentences, not four spellings of one.
        let all = [off, no_sink, ready, sending];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two states read identically");
            }
        }
    }

    /// The wrapper is not the only way in any more, and the line that tells a
    /// waiting user what starts the tracker has to say so — that sentence is
    /// where the "you must wrap opentrack in `tobii game`" folklore came from.
    #[test]
    fn a_ready_line_names_opentrack_itself_as_a_way_to_start_it() {
        let ready = text(&cfg_with(true, Some("127.0.0.1:4242"), None), false);
        assert!(
            ready.contains("opentrack opens the port"),
            "starting opentrack is enough by itself: {ready}"
        );
        assert!(
            ready.contains("tobii game"),
            "and the wrapper is still the answer for everything else: {ready}"
        );

        // With the watch off, the wrapper really is the only way, and claiming
        // otherwise would send the user to wait for something that never comes.
        let no_watch = OutputConfig {
            wake_for_opentrack: false,
            ..cfg_with(true, Some("127.0.0.1:4242"), None)
        };
        let s = text(&no_watch, false);
        assert!(!s.contains("opentrack opens the port"), "{s}");
        assert!(s.contains("tobii game"), "{s}");

        // Same when there is no opentrack sink at all: only the joystick.
        let joy = text(&joystick_only(), false);
        assert!(!joy.contains("opentrack opens the port"), "{joy}");
    }

    /// "Ready" is not a fault. With game output configured and nothing playing,
    /// the tracker is SUPPOSED to be off — a status line implying otherwise
    /// would send people looking for a problem that is the design working.
    #[test]
    fn a_dark_tracker_is_not_reported_as_a_problem() {
        let s = text(&cfg_with(true, Some("127.0.0.1:4242"), None), false);
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
        let s = text(&cfg_with(true, Some("127.0.0.1:4242"), Some(4243)), true);
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

    /// The outcome sentences are shared with `tobii headpose`, which wants them
    /// lowercase for a log line; this window wants a sentence. Only the first
    /// letter may differ, or the two front ends have started wording the same
    /// answer differently.
    #[test]
    fn an_outcome_reads_as_a_sentence_without_being_reworded() {
        let applied = tobii_output::pipeline::RecentreOutcome::Applied {
            yaw_deg: 17.4,
            roll_deg: -0.2,
            spread_deg: 0.3,
        }
        .to_string();
        let shown = sentence(&applied);
        assert!(shown.starts_with('R'), "{shown}");
        assert_eq!(shown[1..], applied[1..], "the wording must not diverge");
        assert_eq!(sentence(""), "");
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
