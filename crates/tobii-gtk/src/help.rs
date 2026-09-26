//! The F1 help window: everything the cards no longer say out loud.
//!
//! # Why this window exists at all
//!
//! The hub's cards were shortened, and two of them gave their sentence up to a
//! tooltip. **GTK4 has no focus-triggered or touch-triggered tooltip**: a
//! tooltip is shown on pointer hover and on nothing else. So a fact that moves
//! into one is a fact a keyboard-only user and a touch user can never read —
//! and the hub already had that problem before this window existed, because
//! the Recentre button's only explanation is a tooltip, and so is the one
//! sentence saying what the three strength names are three strengths *of*.
//!
//! That is the contract this module exists to keep: **every fact that a card in
//! the rack states only in a tooltip is also in here**. It is the rack this
//! window answers for, and the test is scoped the same way: the header and the
//! cogwheel keep their own tooltips, each beside a control that already carries
//! a visible description, where it is plain text, selectable, focusable
//! and reachable with Tab alone. The strings are not copied — they are the same
//! constants the tooltips are set from, and the same
//! [`crate::outputs::recentre_decision`] the refusals come from, so the two
//! readers cannot drift apart. A test asserts it both ways: headless, over the
//! constants (so CI, which has no display, still runs it), and with a display,
//! by walking the real hub's cards and checking every tooltip it finds against
//! this text.

use std::cell::RefCell;

use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Application, Label, Orientation};

/// One topic: a heading and its body, in the order the hub reads.
pub struct Topic {
    pub title: &'static str,
    pub body: String,
}

/// The gap and margin the hub uses, so this window reads as the same panel.
const PAGE_MARGIN: i32 = 20;

/// Every topic, built from the same strings the UI itself is built from.
///
/// A `String` body rather than a `&'static str` because three of the facts in
/// here belong to something else that already states them — the games card's
/// tooltips, and the refusals [`crate::outputs::recentre_decision`] hands the
/// Recentre button — and a copy is how the tooltip and this page start
/// disagreeing. They are interpolated, never retyped.
///
/// Free of widgets on purpose: this is what lets the coverage test run in CI,
/// which has no display.
pub fn topics() -> Vec<Topic> {
    let topic = |title, body: String| Topic { title, body };

    // The four refusals, asked of the function that produces them rather than
    // written out here. `recentre_decision`'s first branch is "a flow wants the
    // device exclusively", and the two flows that do are named in the message,
    // so both are asked for.
    let refusal = |reasons: &[&'static str], tracking, composing| {
        match crate::outputs::recentre_decision(reasons, tracking, composing) {
            Ok(()) => String::new(),
            // The same shape `games::recentre_tooltip` gives it, so the
            // tooltip's second paragraph appears here verbatim.
            Err(why) => format!("Not right now: {why}."),
        }
    };
    let refusals = [
        refusal(&["calibration"], true, true),
        refusal(&["display setup"], true, true),
        refusal(&[], false, true),
        refusal(&[], true, false),
    ]
    .join("\n");

    // The full form of the game switch's tooltip: with the joystick on and
    // allowed to wake the tracker, it is the short form plus one sentence, so
    // quoting the long one covers both.
    let switch = crate::games::switch_tooltip(&tobii_output::games::OutputConfig {
        enabled: true,
        joystick: true,
        wake_for_joystick: true,
        ..Default::default()
    });

    vec![
        topic(
            "Eye position",
            "The box at the top is the space the sensor can see, and the two dots are \
             your eyes. Keep both inside it.\n\n\
             Position says what to do about it — \"good\" when you are in the middle, \
             otherwise \"move closer\", \"lean back\", \"move left\" and so on. While \
             the tracker is in standby it reads \"tracker off\" rather than \"not \
             detected\": a tracker that has been switched off is not a tracker that \
             has lost you.\n\n\
             Distance is how far your eyes are from the sensor. Yaw, pitch and roll \
             are which way your head is facing; pitch reads \"no model\" until the \
             head model is installed — see Head tracking. Beside them is the \
             sensor's own infrared camera."
                .to_string(),
        ),
        topic(
            "Improve my calibration",
            // Verbatim, because this is the sentence the card no longer has
            // room for in full.
            "If the light conditions change or if you experience less tracker \
             precision, you might benefit from improving your calibration.\n\n\
             A dot appears on a dark screen; follow it until it has visited every \
             point. This improves the calibration you already have rather than \
             starting from nothing, so it is worth doing even when only a little is \
             off."
                .to_string(),
        ),
        topic(
            "Change screen",
            "If you move the sensor to a different monitor, you'll need to set up the \
             new display.\n\n\
             Set up display asks which monitor the sensor is under and where on it \
             the sensor sits. The tracker cannot report eyes at all until it has \
             that, and a calibration made for one screen is not valid on another — \
             the hub offers to recalibrate when it notices."
                .to_string(),
        ),
        topic(
            "Select eyes to detect",
            // The card's own sentence, which is a tooltip now. This is where a
            // keyboard or touch user reads it.
            format!(
                "{}\n\n\
                 The choice is saved as soon as you make it and sent to the tracker \
                 again every time it connects, so it survives unplugging the sensor \
                 — and choosing it with the sensor unplugged is not lost.",
                crate::EYES_HELP
            ),
        ),
        topic(
            "Head tracking",
            "Sends your head position and angle to games and apps, over opentrack.\n\n\
             Angles need a head model, which is downloaded once — \"Get the model…\" \
             asks first and shows what it is about to fetch. Without it the hub still \
             reports yaw and roll from the eyes alone, and pitch reads \"no model\".\n\n\
             \"Set pitch zero…\" measures how your head sits when you look at the \
             middle of the screen: the model reports tilt in its own frame, which is \
             offset by how the sensor is mounted, and this measures that offset once \
             so up-and-down reads zero when you sit normally."
                .to_string(),
        ),
        topic(
            "Preview my gaze",
            format!(
                "{}. It is a preview, not a feature games use: one dot at the point \
                 you are looking at, drawn over everything.\n\n\
                 Starting a calibration switches it off. The dot is drawn above a \
                 full-screen flow, so you would end up following your own gaze dot \
                 instead of the one you are being asked to look at — which spoils \
                 every sample while still reporting success.",
                crate::PREVIEW_HELP
            ),
        ),
        topic(
            "Head tracking for games",
            format!(
                "{switch}\n\n\
                 The line under the switch says what is actually happening — which \
                 destinations are receiving, or why none is. It reads the \
                 configuration as it stands, including changes made from a terminal \
                 with `tobii games`.\n\n\
                 Strength — {strength}.\n\n\
                 Virtual joystick — {joystick}\n\n\
                 Recentre view — {recentre}\n\n\
                 It is greyed out when it cannot be taken, and the reason is one of \
                 these:\n{refusals}",
                switch = switch,
                strength = crate::games::STRENGTH_TOOLTIP,
                joystick = crate::games::JOYSTICK_TOOLTIP,
                recentre = crate::games::RECENTRE_TOOLTIP,
                refusals = refusals,
            ),
        ),
        topic(
            "Settings, behind the cogwheel",
            "Keep the tracker awake switches standby off: the tracker stays on, \
             illuminators lit, until you turn it off again. While it is on the header \
             carries an ALWAYS ON badge, because nothing on the hardware will tell \
             you.\n\n\
             Start when I log in runs this program at login with no window. Check for \
             updates asks GitHub when this window opens, and is the only thing this \
             program does on the network unasked. Text size scales every window this \
             program draws. Diagnostics copies or saves the report an issue asks \
             for.\n\n\
             Closing this window does not quit: it goes to the tray icon if your \
             desktop has one and minimises if it does not, so games keep getting head \
             tracking. Quit, in the cogwheel, exits for real."
                .to_string(),
        ),
        topic(
            "Keyboard",
            "F1 opens this window, and closes it again. Esc closes it too, and closes \
             a full-screen setup or calibration flow.\n\n\
             Tab moves through this window: the text of every topic can be focused, \
             selected and copied, and Page Up and Page Down scroll it. F1 belongs to \
             the hub window only — it deliberately does nothing during a calibration, \
             where a window appearing over the dot you are following would spoil the \
             measurement."
                .to_string(),
        ),
    ]
}

thread_local! {
    /// The help window, while one is open.
    ///
    /// A **weak** reference, which is the v0.3.1 rule written as code: a strong
    /// one here would keep the window alive after it was closed, and with it
    /// everything it holds. Nothing else in this module holds the window
    /// either — the Close button finds it from itself at click time
    /// ([`crate::close_on_click`]), and both key controllers hold a weak ref —
    /// so closing it is the last reference gone.
    static OPEN: RefCell<glib::WeakRef<gtk::Window>> = RefCell::new(glib::WeakRef::new());
}

/// Open the help window, or raise the one that is already open.
///
/// Not modal, and not a dialog: this is reference you read while poking the
/// control it describes, and a modal would make you close it to try anything.
///
/// It deliberately does **not** call [`crate::hold_while_open`]. A window of
/// text has no use for the tracker, and lighting the illuminators to show
/// somebody a paragraph is precisely the behaviour the whole demand mechanism
/// exists to prevent.
pub fn open(app: &Application, parent: &impl IsA<gtk::Window>) -> gtk::Window {
    if let Some(win) = OPEN.with(|c| c.borrow().upgrade()) {
        win.present();
        return win;
    }

    let body = gtk::Box::new(Orientation::Vertical, crate::CARD_GAP);
    body.set_margin_top(PAGE_MARGIN);
    body.set_margin_bottom(PAGE_MARGIN);
    body.set_margin_start(PAGE_MARGIN);
    body.set_margin_end(PAGE_MARGIN);
    let mut bodies = Vec::new();
    for t in topics() {
        let (w, label) = card(t.title, &t.body);
        body.append(&w);
        bodies.push(label);
    }

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_vexpand(true);
    scroller.set_child(Some(&body));
    // No `set_focusable(true)` here: a `GtkScrolledWindow` on GTK 4.12 already
    // is focusable, measured — the line was written, and then a control run
    // removed it and every assertion still passed. What the window actually
    // needs is the `grab_focus` after `present` below, which is what puts the
    // focus THERE rather than in the first topic, so Page Up/Down and the
    // arrows scroll without a Tab press first.

    let close = crate::widget::button("Close");
    close.add_css_class("quiet");
    crate::close_on_click(&close);
    let footer = gtk::Box::new(Orientation::Horizontal, 0);
    footer.set_halign(Align::End);
    footer.set_margin_end(PAGE_MARGIN);
    footer.set_margin_bottom(PAGE_MARGIN);
    footer.append(&close);

    let root = gtk::Box::new(Orientation::Vertical, 12);
    root.append(&scroller);
    root.append(&footer);

    let win = gtk::Window::builder()
        .application(app)
        .transient_for(parent.as_ref())
        // The hub's own teardown closes every other window of the application,
        // so this is belt and braces rather than the mechanism — but a help
        // window outliving the hub on some other path would be a window with
        // nothing to be help for.
        .destroy_with_parent(true)
        .title("Help")
        // Measured: the nine topics are 2464px tall at this width (2788 at 520,
        // 2331 at 720), so this window scrolls whatever height it opens at —
        // which is why the height is chosen to FIT rather than to show
        // everything. 660 leaves room for panels on a 768-tall screen, the
        // shortest this program is likely to meet, and the scroller has the
        // focus from the moment it opens so Page Down is the first key that
        // works.
        .default_width(620)
        .default_height(660)
        .child(&root)
        .build();
    crate::add_escape_to_close(&win);
    add_f1_to_close(&win);
    win.present();
    // Focus after `present`: before it, the window has no focus to give.
    scroller.grab_focus();
    // A selectable GtkLabel selects all of its text the moment focus reaches
    // it, and focus passes through the first topic on the way to the scroller —
    // so the window opened with topic one as a solid block of selection
    // highlight. Taking the focus away does not clear it; this does.
    for body in &bodies {
        body.select_region(0, 0);
    }

    OPEN.with(|c| *c.borrow_mut() = win.downgrade());
    win
}

/// Close the help window if one is open.
///
/// Called when the hub hides itself to the tray. `destroy_with_parent` does not
/// cover *hiding* a parent, and a transient window whose parent is hidden is
/// left to the compositor — on some it floats alone on an empty desktop, with
/// no hub to go back to.
pub(crate) fn close() {
    if let Some(win) = OPEN.with(|c| c.borrow().upgrade()) {
        win.close();
    }
}

/// F1 closes the help window, so the key that opens it also puts it away.
///
/// Weak, for the same reason [`crate::add_escape_to_close`] is: the controller
/// belongs to the window, and a strong reference here is a cycle the window
/// never survives.
fn add_f1_to_close(win: &gtk::Window) {
    let keys = gtk::EventControllerKey::new();
    let target = win.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::F1 {
            if let Some(w) = target.upgrade() {
                w.close();
            }
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(keys);
}

/// One topic, as a card in the hub's own style.
///
/// `set_selectable` is not a nicety: a selectable `GtkLabel` is focusable, so
/// Tab walks the topics, a screen reader reads them, and the text can be copied
/// into a bug report. Without it this window would be a wall of text no
/// keyboard could enter — which is the exact failure it exists to prevent.
fn card(title: &str, body: &str) -> (gtk::Box, Label) {
    let b = gtk::Box::new(Orientation::Vertical, 6);
    b.add_css_class("surface");
    b.add_css_class("panel-pad");
    let t = Label::new(Some(title));
    t.add_css_class("section-title");
    t.set_halign(Align::Start);
    t.set_xalign(0.0);
    t.set_wrap(true);
    let d = Label::new(Some(body));
    d.add_css_class("section-desc");
    d.set_halign(Align::Start);
    d.set_xalign(0.0);
    d.set_wrap(true);
    d.set_selectable(true);
    // Wider than the rack's 44: this window is one column of prose, and a
    // measure of about 80 characters is what a paragraph wants.
    d.set_max_width_chars(80);
    b.append(&t);
    b.append(&d);
    (b, d)
}

#[cfg(test)]
mod tests {
    /// Everything that is only a tooltip in the hub has to be in here too.
    ///
    /// This is the contract the shortened cards rest on: GTK4 shows a tooltip
    /// on pointer hover and on nothing else, so a fact that lives only in one
    /// is unreachable by keyboard and by touch. It is asserted over the very
    /// constants the tooltips are set from, which is why it is worth having
    /// even though [`super::topics`] interpolates them: the failure it catches
    /// is somebody *retyping* one of these sentences here, or deleting the
    /// paragraph that quotes it, and then editing the tooltip — after which the
    /// two say different things and nothing else would notice.
    ///
    /// No widgets, so it runs in CI, which has no display. The display-side
    /// half of the same contract — every tooltip on a real hub card, whatever
    /// its source — is `tests/help_window.rs`.
    #[test]
    fn every_tooltip_only_fact_is_in_the_help_window() {
        let text = super::topics()
            .iter()
            .map(|t| format!("{}\n{}", t.title, t.body))
            .collect::<Vec<_>>()
            .join("\n\n");

        let mut want: Vec<String> = vec![
            // The two sentences that left a visible label for a tooltip.
            crate::EYES_HELP.to_string(),
            crate::PREVIEW_HELP.to_string(),
            // The three that were never visible anywhere else.
            crate::games::STRENGTH_TOOLTIP.to_string(),
            crate::games::JOYSTICK_TOOLTIP.to_string(),
            crate::games::RECENTRE_TOOLTIP.to_string(),
        ];
        // And the reasons the Recentre button greys itself out, which the
        // pointer user reads in the row's tooltip and nobody else could.
        for (reasons, tracking, composing) in [
            (&["calibration"][..], true, true),
            (&["display setup"][..], true, true),
            (&[][..], false, true),
            (&[][..], true, false),
        ] {
            let why = crate::outputs::recentre_decision(reasons, tracking, composing)
                .expect_err("these inputs are refusals");
            want.push(format!("Not right now: {why}."));
        }

        for fact in want {
            assert!(
                text.contains(&fact),
                "a fact that is only reachable by hovering is missing from the help \
                 window, so no keyboard or touch user can ever read it: {fact:?}"
            );
        }
    }

    /// The topics are the hub's own order, so a topic sits where its card sits.
    #[test]
    fn the_topics_are_in_the_order_the_hub_reads() {
        let titles: Vec<&str> = super::topics().iter().map(|t| t.title).collect();
        assert_eq!(
            titles,
            [
                "Eye position",
                "Improve my calibration",
                "Change screen",
                "Select eyes to detect",
                "Head tracking",
                "Preview my gaze",
                "Head tracking for games",
                "Settings, behind the cogwheel",
                "Keyboard",
            ]
        );
    }
}
