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
//!
//! # Why it is a sidebar and a search, and not one long scroll
//!
//! It used to be nine cards stacked in one scroller, 2464px of it. That shape
//! answers "read the manual"; nobody arrives with that question. They arrive
//! with **a question about the control they are looking at** — what is
//! "Strength" the strength of, why is Recentre grey — and the fastest honest
//! answer to that is a text box you can type the control's own word into.
//!
//! So the window opens with the focus **in the search box**, and the search
//! reads every topic's heading and the whole of its text. Most of the words on
//! the hub's own controls are in that text, because this page is built from the
//! hub's own strings — but only the ones those strings happen to contain, and
//! that is not the same promise. The three strength presets are the standing
//! exception: they are captioned `Subtle`, `Normal` and `Strong`, their row
//! carries no caption of its own, only [`crate::games::STRENGTH_TOOLTIP`]
//! explains them, and none of those three words is anywhere in here — so typing
//! the word printed on that control lands on the no-match page. Closing it
//! means naming the presets FROM `games::STRENGTHS`, the constant the radios
//! are labelled from, rather than retyping them here. Not done yet.
//! The topic list beside it is the second half of the same
//! idea — nine topics is few enough that a list is not a necessity for finding
//! anything, but it makes the shape of the whole window visible at a glance,
//! which is what tells you whether your question is in here at all.
//!
//! Three consequences worth stating, because each one is a rule the rest of
//! this file keeps:
//!
//! * **Filtering hides rows; it never touches [`topics`].** The coverage test
//!   asserts against that model, not against the widgets, so no query can make
//!   a tooltip-only fact stop being covered. A second assertion checks that the
//!   window builds one page per topic, which is what stops the model and the
//!   window drifting the other way.
//! * **Nothing here may be pointer-only.** A sidebar you can only click would
//!   defeat the entire purpose of the window. Every part of it — search, list,
//!   every topic body — is on the Tab chain, and the "Keyboard" topic below
//!   states the exact path, so the window documents itself.
//! * **Nothing here may hold the window.** Every handler the window owns
//!   captures descendants only (a GTK4 child does not reference its parent, so
//!   those are not cycles), and the single-instance registry is a `WeakRef`.
//!   This project shipped a window that kept itself alive once.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Application, Label, Orientation};

/// One topic: a heading and its body, in the order the hub reads.
pub struct Topic {
    pub title: &'static str,
    pub body: String,
}

impl Topic {
    /// The topic as the single blob the search reads: heading, then body.
    ///
    /// One spelling of it and not three, because three separate readers depend
    /// on it having the same shape — [`matches`] searches it, this module's
    /// coverage test asserts every tooltip string is somewhere in it, and
    /// `tests/help_window.rs` checks the real rack's tooltips against it. Three
    /// hand-written `format!`s that happened to agree is a coverage contract
    /// held together by hand.
    pub fn text(&self) -> String {
        format!("{}\n{}", self.title, self.body)
    }
}

/// The gap and margin the hub uses, so this window reads as the same panel.
const PAGE_MARGIN: i32 = 20;

/// The sidebar's width. Wide enough for "Settings, behind the cogwheel" on two
/// lines and for every other title on one, at text scale 1.0.
const SIDEBAR_WIDTH: i32 = 184;

/// The window width below which the sidebar folds away.
///
/// Chosen from the measurement, not from taste: the sidebar is
/// [`SIDEBAR_WIDTH`] plus its margins, so below about this width the topic text
/// is left with less room than the sidebar has, which is the point at which a
/// permanent sidebar stops being navigation and starts being the window.
const NARROW: i32 = 520;

/// Names on the window's own parts.
///
/// They are CSS handles first (`#help-search` and friends can be styled without
/// giving a widget a class that means nothing anywhere else), and they are also
/// how the display-side test finds the pieces it drives. Public for the same
/// reason [`crate::build_hub`] is: a test that drives the real widget by its
/// real name is worth more than a probe-only twin built beside it.
pub const SEARCH_NAME: &str = "help-search";
/// The topic list. See [`SEARCH_NAME`].
pub const LIST_NAME: &str = "help-topics";
/// The stack that shows one topic at a time. See [`SEARCH_NAME`].
pub const STACK_NAME: &str = "help-topic-stack";
/// The sidebar box. See [`SEARCH_NAME`].
pub const SIDEBAR_NAME: &str = "help-sidebar";
/// The "Topics" button that brings the sidebar back when narrow. See
/// [`SEARCH_NAME`].
pub const TOGGLE_NAME: &str = "help-topics-toggle";
/// The scroller around the topic pane. See [`SEARCH_NAME`].
pub const BODY_SCROLL_NAME: &str = "help-topic-scroller";
/// The stack page shown when a query matches nothing.
pub const NO_MATCH_PAGE: &str = "no-match";

/// The third-party launcher one user reports getting Microsoft Flight
/// Simulator 2024 tracking with.
///
/// Plain text and never a link widget, and that is the whole design of it.
/// This window's topic bodies are selectable labels, so the address can be
/// read and copied and nothing here opens it; there is no button, nothing is
/// fetched, and nothing is written into Steam. The account is a user's, not a
/// measurement of ours — the sentence around it says so in those words — and a
/// clickable control beside it would turn a report into an offer.
const REPORTED_LAUNCHER: &str = "https://github.com/markx86/opentrack-launcher";

/// The stack page name for topic `i`.
fn page_name(i: usize) -> String {
    format!("t{i}")
}

/// Every topic, built from the same strings the UI itself is built from.
///
/// A `String` body rather than a `&'static str` because three of the facts in
/// here belong to something else that already states them — the games card's
/// tooltips, and the refusals [`crate::outputs::recentre_decision`] hands the
/// Recentre button — and a copy is how the tooltip and this page start
/// disagreeing. They are interpolated, never retyped.
///
/// Free of widgets on purpose: this is what lets the coverage test run in CI,
/// which has no display, and it is also what the search runs over, so a query
/// that matches nothing is a fact about this list rather than about the
/// widgets that happen to be showing.
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
             The card says which monitor it is set up for, matched on what the \
             monitor itself reports — so two identical screens are told apart, and \
             one that has been unplugged reads as not connected rather than as \
             nothing having been set up. \u{201c}No display set up yet\u{201d} means \
             the tracker has never been told where the sensor sits, and it cannot \
             report eyes at all until it has.\n\n\
             Set up display asks which monitor the sensor is under and where on it \
             the sensor sits. Running it again replaces that answer, and a \
             calibration made for one screen is not valid on another — the hub \
             offers to recalibrate when it notices."
                .to_string(),
        ),
        topic(
            "Select eyes to detect",
            // The card's own sentence, which is a tooltip now. This is where a
            // keyboard or touch user reads it.
            format!(
                "{help}\n\n\
                 The choice is saved as soon as you make it and sent to the tracker \
                 again every time it connects, so it survives unplugging the sensor \
                 — and choosing it with the sensor unplugged is not lost.\n\n\
                 {caveat} That is a measurement and not a caution. Put to this \
                 hardware on 2026-07-20, the tracker stored the selection — it \
                 reads back, and it survives a reboot — and went on reporting both \
                 eyes in its gaze stream regardless. Tobii's own software applies \
                 the choice by rebuilding the tracking model rather than by \
                 setting it on its own, and nobody here has watched a standalone \
                 set change what this tracker detects. So the card says what was \
                 saved, and does not say what is being detected.\n\n\
                 If the tracker does not answer at all, the radio goes back to \
                 what the tracker last said it was doing rather than sitting on a \
                 selection that did not take. The reason is in the log — \
                 `tobii debug` collects it.",
                help = crate::EYES_HELP,
                caveat = crate::EYES_CAVEAT,
            ),
        ),
        topic(
            "Head tracking",
            "Sends your head position and angle to games and apps, over opentrack.\n\n\
             Angles need a head model, which is downloaded once — \"Get the model…\" \
             asks first and shows what it is about to fetch. Without it the hub still \
             reports yaw and roll from the eyes alone, and pitch reads \"no model\".\n\n\
             The line at the top of the card says what this program is actually \
             running, not what is on the disk: a model file that is there and will \
             not load reads as not being used, rather than as working. It is the \
             tracker connecting that finds that out, so on a hub opened with the \
             sensor unplugged the line reports the file until one does.\n\n\
             \"Set pitch zero…\" measures how your head sits when you look at the \
             middle of the screen: the model reports tilt in its own frame, which is \
             offset by how the sensor is mounted, and this measures that offset once \
             so up-and-down reads zero when you sit normally."
                .to_string(),
        ),
        topic(
            "Preview my gaze",
            format!(
                "{help}. It is a preview, not a feature games use: one dot at the \
                 point you are looking at, drawn over everything.\n\n\
                 Starting a calibration switches it off. The dot is drawn above a \
                 full-screen flow, so you would end up following your own gaze dot \
                 instead of the one you are being asked to look at — which spoils \
                 every sample while still reporting success.\n\n\
                 If the switch is greyed out and the card reads \u{201c}{short}\u{201d}: \
                 {why}",
                help = crate::PREVIEW_HELP,
                short = crate::PREVIEW_UNAVAILABLE,
                why = crate::PREVIEW_UNSUPPORTED,
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
                 It is greyed out when it cannot be taken, and the row's tooltip \
                 says why. A reason reads like one of these:\n{refusals}\n\n\
                 More than one thing can be holding the tracker at once, and then \
                 the tooltip names them all in the same sentence rather than one \
                 at a time — so what you read there may be a longer sentence than \
                 any of these four.\n\n\
                 Every control on this card is global: there is no such thing as \
                 the joystick for one game. What one particular game needs is the \
                 Games tab.",
                switch = switch,
                strength = crate::games::STRENGTH_TOOLTIP,
                joystick = crate::games::JOYSTICK_TOOLTIP,
                recentre = crate::games::RECENTRE_TOOLTIP,
                refusals = refusals,
            ),
        ),
        // The four paragraphs below were bolted onto the topic above while the
        // only door to them was a button on that card. The door is a tab now,
        // and a topic of its own is what a tab gets.
        topic(
            "Games",
            format!(
                "Pick a game on the left and the page beside it \
                 shows the three things that have to be configured for it: this \
                 program's own settings, the Wine bridge inside that game's \
                 Proton prefix, and the game's own options. It reads a game's own \
                 files and never changes them.\n\n\
                 The list is every game this machine has: what Steam says is \
                 installed, plus anything added by hand. A row is drawn at full \
                 strength when the Wine bridge is installed in that game's \
                 prefix and drawn back when it is not, so what is left to do is \
                 visible without reading. The second line of each row says which \
                 it is, and tells apart the two ways of not being set up: a \
                 prefix with no bridge in it is something to press Install on, \
                 and a game never launched under Proton has no prefix to install \
                 into yet.\n\n\
                 The button beside the search box shows only the games that are \
                 set up. It has no caption because it is the same play triangle \
                 Steam's own library uses for the same question; resting a \
                 pointer on it says so. On a machine with one set-up game among \
                 thirty that button is how you find it, and pressing it again \
                 brings the rest back.\n\n\
                 A game Steam has nothing to do with gets in the same way: press \
                 \u{201c}{add_btn}\u{201d} under the list and pick the Wine \
                 prefix it runs in — the folder holding `drive_c` — and the \
                 bridge can be installed into it exactly as it is for a Steam \
                 game. That is how a game Steam does not sell reaches this page \
                 at all. Those rows have no app id, so they get the first two \
                 sections of the page and not the third: reading a game's own \
                 options needs a profile, and a profile is a file named after an \
                 app id. \u{201c}{forget}\u{201d} takes the row out and \
                 touches nothing on disk.\n\n\
                 Under the list, the count of what Steam has installed; on the \
                 rare machine that has any, a line for anything in the profiles \
                 directory that is not a profile this program can use; and, if \
                 there are any, a count of the profiles here whose game Steam \
                 does not list on this machine — an uninstalled title, or one on \
                 a drive that is not plugged in. Those have no row, because there \
                 is nothing on this page that could act on one, but the profile \
                 is not lost and it applies again the moment Steam lists the \
                 title. `tobii games profile show`, with no app id after it, \
                 names every profile on the machine including those.\n\n\
                 The first section reports and never writes. {pointer} Showing \
                 this tab also stops the hub asking for the tracker, for the same \
                 reason looking at another window does: nothing here needs the \
                 sensor, and lighting the illuminators to show somebody a \
                 paragraph is what this program is written not to do. Whether it \
                 then goes dark depends on what else is holding it — a game \
                 receiving through the joystick or the opentrack port, a \
                 calibration, or Keep the tracker awake, which is what an ALWAYS \
                 ON badge in the header means. With none of those it is dark \
                 three seconds later. Ctrl+Page Up comes back.\n\n\
                 The buttons under the page act on the Wine bridge and on \
                 nothing else. Install puts it into the prefix, Reinstall does it \
                 again over what is there, Uninstall takes it back out, and \
                 \u{201c}{details}\u{201d} runs `tobii bridge status` and prints its whole \
                 report without starting anything. When there is nothing to press, the \
                 bar says why instead of going blank.\n\n\
                 \u{201c}{other}\u{201d} appears for a game this \
                 project has watched refuse ours at the signature check, and for \
                 no other. It puts somebody else's client DLL into the prefix \
                 instead — pick the folder holding its NPClient64.dll; opentrack \
                 ships one. It is offered there and nowhere else because ours is \
                 the right default everywhere else, and because a button putting \
                 a third-party binary into every prefix on the machine would be \
                 recommending something nobody has watched work there.\n\n\
                 The third section, the game's own options, is read-only: this \
                 program reports what those files say and never writes them. It \
                 checks them only where a profile says what to look at, and this \
                 build ships {shipped}, so on a fresh install that section says \
                 so rather than guessing.\n\n\
                 A profile is a file per game in {profiles}, named by app id, and \
                 you write it from a terminal — that page prints the same two \
                 commands with the app id already filled in for the game you \
                 picked:\n\n\
                 {save} <app id>\n\
                 {add} <app id> {add_flags}\n\n\
                 `save` writes down this program's own settings under that app \
                 id; `check add` adds one thing to look at in one of the game's \
                 own files, and what it should say. Run `{check}` on its own for \
                 what those fields mean and which file formats this build reads, \
                 and `{check_where} <app id>` for the directory the paths are \
                 taken as relative to.\n\n\
                 The second section, the Wine bridge, is the one that can be set \
                 up correctly and still deliver nothing, which is worth knowing \
                 before you judge it broken. {gate}\n\n\
                 {provider}\n\n\
                 Reported, and not verified by anyone here: one user reports \
                 getting head tracking working in Microsoft Flight Simulator \
                 2024 by running opentrack's Windows build inside that game's \
                 own Proton prefix, started together with the game in a single \
                 Proton launch, using a third-party launcher — \
                 {launcher}. Nobody on this project has run that, with that \
                 launcher or any other. It is written down here because it is \
                 the only account we have of that title tracking at all, and \
                 for no other reason: it is not a recommendation, it is not a \
                 route this program supports, and nothing here will set it up, \
                 fetch anything or change anything in Steam.",
                // The tab and this page are the two surfaces a user without a
                // terminal reads, and a sentence typed into both is a sentence
                // that gets corrected in one of them. So the check comes from
                // `tobii-config`, which `tobii bridge install` also asks, and
                // what has to be running comes from the block that offers the
                // install.
                gate = tobii_config::signature::trackir_gate(),
                provider = tobii_config::signature::provider_note(),
                launcher = REPORTED_LAUNCHER,
                profiles = tobii_config::profiles::profiles_dir().display(),
                // Asked rather than asserted: this sentence, the hub's own
                // and the CLI's all used to carry a hand-typed "ships none".
                shipped = tobii_config::profiles::shipped_profiles(),
                // The same constants the Games tab types, so the two surfaces
                // that name these commands cannot drift apart.
                save = crate::game_setup::PROFILE_SAVE,
                add = crate::game_setup::PROFILE_CHECK_ADD,
                add_flags = crate::game_setup::ADD_FLAGS,
                check = crate::game_setup::PROFILE_CHECK,
                check_where = crate::game_setup::PROFILE_CHECK_WHERE,
                pointer = crate::game_setup::TRACKER_TAB_POINTER,
                add_btn = crate::game_setup::ADD_GAME_CAPTION,
                details = crate::game_setup::DETAILS_CAPTION,
                forget = crate::game_setup::FORGET_CAPTION,
                other = crate::game_setup::OTHER_CLIENT_CAPTION,
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
            // This topic is the window's own documentation, so it has to keep
            // step with `open` below. Every key named here is measured by
            // `tests/help_window.rs`, which drives the same path with GTK's own
            // `child_focus` and `move-cursor` — the actions Tab and the arrow
            // keys resolve to — rather than trusting the sentence.
            //
            // The example words are held to the same standard, by
            // `the_word_on_the_control_opens_the_topic`: it reads every word
            // this paragraph QUOTES back out of this string and checks it
            // against the table of words that really are printed on the hub.
            // The sentence used to offer four and get three of them wrong —
            // "strength", "cogwheel" and "pitch" are on no control anywhere —
            // so a user who did as they were told read their own instructions
            // back, or got the no-match page. Quote nothing here that is not in
            // that table.
            "F1 opens this window, and closes it again. Esc closes it too, from \
             anywhere in it, and closes a full-screen setup or calibration flow.\n\n\
             On the hub, Ctrl+Page Down goes to the Games tab and Ctrl+Page Up \
             comes back to Tracker. Tab reaches the two tab buttons as well, and \
             then Left and Right walk them.\n\n\
             It opens with the search box focused, so you can type your question \
             straight away: the search reads every topic's heading and all of its \
             text, so a word from the thing you are asking about — \"joystick\", \
             \"recentre\", \"standby\", \"tray\" — usually lands on it. Those \
             four are printed on the hub itself, but not every caption is in this \
             text: if a word off a control finds nothing, type what the control \
             does rather than what it is called. Down moves from the box into the \
             list of topics; Up and Down then walk it, and the topic beside the \
             list changes as you go. Ctrl+F comes back to the search box from \
             anywhere in the window.\n\n\
             Tab moves on from the list into the topic itself, whose text can be \
             focused, selected and copied, and Page Up and Page Down scroll it. Tab \
             again reaches Close.\n\n\
             F1 belongs to the hub window only — it deliberately does nothing during \
             a calibration, where a window appearing over the dot you are following \
             would spoil the measurement."
                .to_string(),
        ),
    ]
}

/// Does `topic` answer `query`?
///
/// Case-insensitive, and **every** whitespace-separated word of the query has
/// to appear somewhere in the topic's heading or its body — so "joystick
/// opentrack" narrows rather than widens, which is what somebody typing a
/// second word is asking for. Substring, not word-boundary: "cal" finds
/// "calibration", and somebody who half-remembers a word should not be
/// punished for it.
///
/// An empty query matches everything, because `all` over no words is true.
/// That is the behaviour the filter wants — an empty search box is not a
/// search — and it is asserted rather than left to that reading of `all`.
///
/// It runs over [`Topic`] and not over widgets, which is what lets the search
/// be tested in CI and what keeps the coverage contract intact: hiding a row
/// cannot remove a fact from a list this never touches.
pub fn matches(topic: &Topic, query: &str) -> bool {
    all_terms_in(&topic.text().to_lowercase(), query)
}

fn all_terms_in(hay: &str, query: &str) -> bool {
    query
        .split_whitespace()
        .all(|term| hay.contains(&term.to_lowercase()))
}

/// Which topic a query should be showing, of the ones it matches.
///
/// Not a ranking, a tie-break, and it exists because of one case that would
/// otherwise look broken: type a heading — "Change screen" — and every word of
/// it is also somewhere in two other bodies, so the first *surviving* topic is
/// not the one whose name you just typed. The list is left in its own order
/// (rows that reshuffle under the cursor as you type are worse than a wrong
/// pick), and instead the topic whose **heading** takes every word wins the
/// pane. Failing that, the first match in the hub's order.
///
/// `None` only when nothing matches at all, which is the no-match page.
///
/// Takes the list rather than building its own. It returns an INDEX into that
/// list, and it is called on every keystroke: a second copy built in here is a
/// second thing that has to stay in the same order as the rows, the pages and
/// the filter, and rebuilding all nine topics — every one of which interpolates
/// somebody else's strings — to answer one keypress is work for nothing.
pub fn best_match(all: &[Topic], query: &str) -> Option<usize> {
    let hits: Vec<usize> = (0..all.len())
        .filter(|&i| matches(&all[i], query))
        .collect();
    hits.iter()
        .copied()
        .find(|&i| all_terms_in(&all[i].title.to_lowercase(), query))
        .or_else(|| hits.first().copied())
}

thread_local! {
    /// The help window, while one is open.
    ///
    /// A **weak** reference, which is the v0.3.1 rule written as code: a strong
    /// one here would keep the window alive after it was closed, and with it
    /// everything it holds. Nothing else in this module holds the window
    /// either — the Close button finds it from itself at click time
    /// ([`crate::close_on_click`]), the search box finds it the same way, and
    /// both key controllers hold a weak ref — so closing it is the last
    /// reference gone.
    static OPEN: RefCell<glib::WeakRef<gtk::Window>> = RefCell::new(glib::WeakRef::new());

    /// The topic the window was last showing, so reopening resumes there.
    ///
    /// An index and not a widget, so it outlives the window by design: closing
    /// the window must free it (see `OPEN`), and "where was I" is the one thing
    /// worth keeping across that. A user who reads a paragraph, tries the
    /// control, and presses F1 again is asking about the same control.
    static LAST: RefCell<usize> = const { RefCell::new(0) };
}

/// The first row the filter is currently letting through.
///
/// `is_child_visible`, not `is_visible`: `GtkListBox` filters by setting a
/// row's *child* visibility and leaves the `visible` property alone, so
/// `is_visible` answers true for every row whatever the filter says.
fn first_visible(list: &gtk::ListBox) -> Option<gtk::ListBoxRow> {
    let mut i = 0;
    while let Some(row) = list.row_at_index(i) {
        if row.is_child_visible() {
            return Some(row);
        }
        i += 1;
    }
    None
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

    // Built once, and shared with the two handlers that read it. The rows, the
    // pages and the filter are all addressed by the same index, so a second
    // copy anywhere is a second thing that has to be in the same order.
    let all: Rc<Vec<Topic>> = Rc::new(topics());

    // --- the content: one card per topic, and one page for "nothing matched"
    //
    // A stack rather than the old single column of all nine. The column made
    // every topic a scroll away from every other and made the window's height
    // meaningless; one topic at a time means the heading you picked is at the
    // top of the pane, where a heading belongs.
    let stack = gtk::Stack::new();
    stack.set_widget_name(STACK_NAME);
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
    stack.set_margin_top(PAGE_MARGIN);
    stack.set_margin_bottom(PAGE_MARGIN);
    stack.set_margin_start(PAGE_MARGIN);
    stack.set_margin_end(PAGE_MARGIN);
    let mut bodies = Vec::new();
    for (i, t) in all.iter().enumerate() {
        let (page, label) = card(t.title, &t.body);
        stack.add_named(&page, Some(&page_name(i)));
        bodies.push(label);
    }
    // The page a query with no answer lands on. An empty window with no
    // explanation is the bug this exists to avoid: it says what was searched
    // for, what the search looked at, and how to get back.
    let (no_match_page, no_match) = card("Nothing found", "");
    stack.add_named(&no_match_page, Some(NO_MATCH_PAGE));

    let body_scroll = gtk::ScrolledWindow::new();
    body_scroll.set_widget_name(BODY_SCROLL_NAME);
    body_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    body_scroll.set_vexpand(true);
    body_scroll.set_hexpand(true);
    body_scroll.set_child(Some(&stack));
    // No `set_focusable(true)`: a `GtkScrolledWindow` already is focusable,
    // measured — the line was written, and then a control run removed it and
    // every assertion still passed.

    // The way back to the topic list when the sidebar has folded away. Hidden
    // at any width where the sidebar is on screen, so it costs nothing in the
    // layout most people will ever see.
    // `widget::toggle_button`, not `ToggleButton::with_label`: the built-in
    // label is the path `widget.rs` records the glyph-clipping fault on, and
    // this is the only ToggleButton in the program — so it was the only control
    // left out of the workaround written for it.
    let toggle = crate::widget::toggle_button("Topics");
    toggle.set_widget_name(TOGGLE_NAME);
    toggle.add_css_class("quiet");
    toggle.set_tooltip_text(Some("Show the list of topics"));
    toggle.set_halign(Align::Start);
    toggle.set_margin_top(PAGE_MARGIN);
    toggle.set_margin_start(PAGE_MARGIN);
    // An end margin too, because when the list is the one showing, this button
    // is all its column holds and sits against the window's right edge.
    toggle.set_margin_end(PAGE_MARGIN);
    // In the content column directly rather than in a row of its own, so that
    // its own `visible` is the whole of the narrow/wide state: every handler
    // below asks `toggle.is_visible()` for "is the sidebar folded away", and a
    // wrapper box would have made that question ask the wrong widget — a
    // GTK4 child of a hidden parent still reports `visible` true.
    toggle.set_visible(false);

    let content = gtk::Box::new(Orientation::Vertical, 0);
    // No `set_hexpand` here, deliberately. A GtkBox that is not told computes
    // its own from its children, so the column expands exactly while the topic
    // pane inside it is on screen — and when the pane is hidden (narrow, with
    // the list filling the window) it stops, and the sidebar gets the width
    // instead of sharing it with an empty column. Setting it true here left a
    // 170px band of nothing beside the topic list, which is what rendering the
    // window showed and what reasoning about it did not.
    content.append(&toggle);
    content.append(&body_scroll);

    // --- the sidebar: the search box, then the topics it filters
    let search = gtk::SearchEntry::new();
    search.set_widget_name(SEARCH_NAME);
    search.add_css_class("topic-search");
    search.set_placeholder_text(Some("Search help"));
    search.set_tooltip_text(Some(
        "Search every topic's heading and text (Ctrl+F from anywhere in this window)",
    ));

    let list = gtk::ListBox::new();
    list.set_widget_name(LIST_NAME);
    list.add_css_class("topic-list");
    list.set_selection_mode(gtk::SelectionMode::Single);
    for t in all.iter() {
        let row = gtk::ListBoxRow::new();
        let l = Label::new(Some(t.title));
        l.set_halign(Align::Start);
        l.set_xalign(0.0);
        l.set_wrap(true);
        row.set_child(Some(&l));
        list.append(&row);
    }
    // Shown by `GtkListBox` itself whenever the filter leaves no row visible,
    // which is the sidebar's half of "say something rather than go blank".
    let placeholder = Label::new(Some("No topic matches."));
    placeholder.add_css_class("section-desc");
    placeholder.set_wrap(true);
    placeholder.set_xalign(0.0);
    placeholder.set_margin_top(10);
    placeholder.set_margin_start(10);
    placeholder.set_margin_end(10);
    list.set_placeholder(Some(&placeholder));

    let list_scroll = gtk::ScrolledWindow::new();
    list_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    list_scroll.set_vexpand(true);
    list_scroll.set_child(Some(&list));

    let sidebar = gtk::Box::new(Orientation::Vertical, 8);
    sidebar.set_widget_name(SIDEBAR_NAME);
    sidebar.set_size_request(SIDEBAR_WIDTH, -1);
    sidebar.set_margin_top(PAGE_MARGIN);
    sidebar.set_margin_bottom(PAGE_MARGIN);
    sidebar.set_margin_start(PAGE_MARGIN);
    sidebar.set_margin_end(12);
    sidebar.append(&search);
    sidebar.append(&list_scroll);

    // A hairline between the two, the same one the hub draws between sections.
    let rule = gtk::Box::new(Orientation::Vertical, 0);
    rule.add_css_class("hairline-v");

    let split = gtk::Box::new(Orientation::Horizontal, 0);
    split.set_vexpand(true);
    split.append(&sidebar);
    split.append(&rule);
    split.append(&content);

    let close = crate::widget::button("Close");
    close.add_css_class("quiet");
    crate::close_on_click(&close);
    let footer = gtk::Box::new(Orientation::Horizontal, 0);
    footer.set_halign(Align::End);
    footer.set_margin_end(PAGE_MARGIN);
    footer.set_margin_bottom(PAGE_MARGIN);
    footer.append(&close);

    let root = gtk::Box::new(Orientation::Vertical, 12);
    root.append(&split);
    root.append(&footer);

    // --- wiring
    //
    // Every closure below captures descendants of the window or siblings of
    // itself, never an ancestor. In GTK4 a parent holds a reference to its
    // children and a child holds none to its parent, so a handler on the list
    // that captures the stack is not a cycle, while a handler on the list that
    // captured the window would be — and that is the v0.3.1 bug.
    let query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    {
        let query = query.clone();
        // Data, not widgets: the filter asks `matches` about the same list the
        // coverage test asserts over, and about the very same copy of it the
        // rows and the pages were built from.
        let model = all.clone();
        list.set_filter_func(move |row| {
            let q = query.borrow();
            model
                .get(row.index() as usize)
                .is_some_and(|t| matches(t, &q))
        });
    }
    {
        let (q, l, st, nm) = (query.clone(), list.clone(), stack.clone(), no_match.clone());
        let model = all.clone();
        let n = all.len();
        search.connect_search_changed(move |e| {
            let text = e.text().to_string();
            // What was in the box before this keystroke: `q` still holds it
            // until the line below. It is the whole difference between refining
            // a query and starting one. See `keep`.
            let refining = !q.borrow().trim().is_empty();
            *q.borrow_mut() = text.clone();
            l.invalidate_filter();
            // The content follows the search, so a one-word query usually
            // answers itself without a click. Three rules, in order: a topic
            // that is still showing stays (typing another letter must not throw
            // away the paragraph being read), else `best_match`, else the page
            // that explains why there is nothing. `select_row` drives
            // `row-selected` below, which is what turns a row into a page.
            // "Still showing stays" is a rule about REFINING a query. The
            // first one typed into a box that was empty is a question, and the
            // topic on screen then was not chosen for it — it is whichever one
            // the window resumed on, or the first. Letting that win meant the
            // Keyboard topic, which advertises "joystick", "recentre",
            // "standby" and "tray" as words worth typing, quotes all four:
            // read it, press F1 again, type one of them, and the list narrowed
            // correctly while the pane stayed where it was.
            let keep = if refining {
                l.selected_row().filter(|r| r.is_child_visible())
            } else {
                None
            };
            let pick =
                keep.or_else(|| best_match(&model, &text).and_then(|i| l.row_at_index(i as i32)));
            match pick {
                Some(row) => {
                    // The page is set here and not left to `row-selected`.
                    // That signal fires on a CHANGE of selection, and the one
                    // case that matters most does not change it: a query that
                    // matched nothing leaves the previous row selected but the
                    // pane on the no-match page, so clearing the box picks the
                    // same row again, the signal stays silent, and the window
                    // sits on "Nothing found" with a full list beside it. Found
                    // by rendering the window, not by reasoning about it.
                    let i = row.index().max(0) as usize;
                    l.select_row(Some(&row));
                    st.set_visible_child_name(&page_name(i));
                }
                None => {
                    nm.set_text(&format!(
                        "Nothing in this help matches \u{201c}{text}\u{201d}.\n\n\
                         The search reads every topic's heading and the whole of its \
                         text, and every word you type has to appear somewhere in \
                         one topic — so a second word narrows rather than widens. \
                         Try one word instead, off the control you are looking at.\n\n\
                         Clear the box to see all {n} topics again."
                    ));
                    st.set_visible_child_name(NO_MATCH_PAGE);
                }
            }
        });
    }
    {
        let st = stack.clone();
        list.connect_row_selected(move |_, row| {
            if let Some(r) = row {
                let i = r.index().max(0) as usize;
                st.set_visible_child_name(&page_name(i));
                LAST.with(|c| *c.borrow_mut() = i);
            }
        });
    }
    {
        // Enter (or a click) on a row. Only meaningful while the sidebar is the
        // whole window — at full width the selection already changed the page,
        // and folding the list away there would be a pane vanishing under the
        // user. `is_visible` on the toggle is the narrow flag: `relayout` is
        // the only thing that sets it.
        let tg = toggle.clone();
        list.connect_row_activated(move |_, _| {
            if tg.is_visible() && tg.is_active() {
                tg.set_active(false);
            }
        });
    }
    {
        // `sidebar` WEAKLY: it is the parent of `list`, and `list` holds this
        // toggle through its row-activated handler, so a strong capture here
        // closes sidebar -> list -> toggle -> sidebar. `search` weakly for the
        // same reason one widget along: its own handlers hold `list`.
        let (sb, se, bs, rl) = (
            sidebar.downgrade(),
            search.downgrade(),
            body_scroll.clone(),
            rule.clone(),
        );
        toggle.connect_toggled(move |t| {
            let Some(sb) = sb.upgrade() else { return };
            if t.is_visible() {
                sb.set_visible(t.is_active());
                bs.set_visible(!t.is_active());
                rl.set_visible(false);
                // Hand the focus to the pane that has just come on screen.
                //
                // The keyboard reason is the obvious one: without this,
                // pressing "Topics" unfolds a list that the keyboard cannot
                // reach without a further Ctrl+F, which is the shortcut that
                // already does exactly this by hand.
                //
                // The other reason is memory, and it is the one that was
                // measured. GTK's focus bookkeeping keeps a hold on the
                // subtree the focus was last inside, and folding a pane away
                // while the focus is still in it never gives that hold back:
                // the window frees on close and 80 of its 87 widgets and 127
                // of its 143 event controllers do not — once per fold, for as
                // long as the tray icon runs, accumulating exactly linearly.
                // It is not a cycle in these closures; a window of the same
                // shape with no handlers at all does it too. Asserted in
                // `tests/help_window.rs`, over a census taken AFTER the fold —
                // the one at 3100 ms runs before the narrow phase and passes
                // straight over it.
                if t.is_active() {
                    if let Some(se) = se.upgrade() {
                        se.grab_focus();
                    }
                }
            }
        });
    }
    {
        // Down out of the search box and into the list, so browsing costs one
        // key from the state the window opens in. Enter does the same, which is
        // what `GtkSearchEntry` emits `activate` for.
        let l = list.clone();
        let into_list = move || {
            let row = l
                .selected_row()
                .filter(|r| r.is_child_visible())
                .or_else(|| first_visible(&l));
            if let Some(row) = row {
                l.select_row(Some(&row));
                row.grab_focus();
            }
        };
        let keys = gtk::EventControllerKey::new();
        let down = into_list.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Down {
                down();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        search.add_controller(keys);
        search.connect_activate(move |_| into_list());
    }
    {
        // Tab out of the topic list, rather than through it.
        //
        // Measured, not assumed: with nothing here, `child_focus` from the
        // search box walks
        //
        //     GtkText -> row(Eye position) -> row(Improve my calibration) -> ...
        //
        // one row per press, so reaching the text of the topic you are already
        // looking at costs nine Tabs. That is GTK's own behaviour for a
        // `GtkListBox` of focusable rows and it is right for a list of
        // controls; it is wrong for a list that is navigation, where the arrow
        // keys are what walks it and Tab means "on to the next thing".
        //
        // A plain (bubble-phase) controller on the list is enough to preempt
        // it: GtkWindow's own move-focus is a class keybinding, which runs at
        // the window in the bubble phase — after this.
        // `search` WEAKLY: its own handlers hold `list` (the filter, and
        // Enter-into-the-list), so a strong capture here closes a cycle
        // between two siblings and neither is ever disposed. The window itself
        // still frees — nothing holds an ancestor — which is why a test that
        // weak-refs only the window cannot see it.
        let (bs, se) = (body_scroll.clone(), search.downgrade());
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, state| {
            if key != gtk::gdk::Key::Tab && key != gtk::gdk::Key::ISO_Left_Tab {
                return glib::Propagation::Proceed;
            }
            let back = state.contains(gtk::gdk::ModifierType::SHIFT_MASK)
                || key == gtk::gdk::Key::ISO_Left_Tab;
            if back {
                let Some(se) = se.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                se.grab_focus();
                return glib::Propagation::Stop;
            }
            // Forward goes to the topic pane — the scroller and not the label
            // inside it, so Page Up and Page Down work the moment you leave the
            // list, with the text one more Tab away. When the pane is not on
            // screen at all (narrow, with the list filling the window) there is
            // nothing to go to, and GTK's own behaviour is left alone.
            if bs.is_visible() && bs.grab_focus() {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        list.add_controller(keys);
    }
    // Esc inside the search box closes the window, like Esc everywhere else in
    // it. `GtkSearchEntry` binds Escape to its own `stop-search` signal and
    // consumes the key, so the window's Escape handler never sees it — without
    // this, Escape would silently stop working for exactly the widget the
    // window opens focused. The window is found from the entry at emit time,
    // never captured, for the reason `crate::close_on_click` does the same.
    search.connect_stop_search(|e| {
        if let Some(w) = e.root().and_downcast::<gtk::Window>() {
            w.close();
        }
    });

    let win = gtk::Window::builder()
        .application(app)
        .transient_for(parent.as_ref())
        // The hub's own teardown closes every other window of the application,
        // so this is belt and braces rather than the mechanism — but a help
        // window outliving the hub on some other path would be a window with
        // nothing to be help for.
        .destroy_with_parent(true)
        .title("Help")
        // 620 is the sidebar (184 + 32 of margin) and a topic pane of a little
        // over 400, which is the measure `card` sizes its text for. 660 leaves
        // room for panels on a 768-tall screen, the shortest this program is
        // likely to meet; the longest topic is taller than that and the pane
        // scrolls, which is why the height is chosen to FIT rather than to show
        // everything.
        .default_width(620)
        .default_height(660)
        .child(&root)
        .build();

    // The one breakpoint. See `NARROW`.
    let relayout: Rc<dyn Fn(&gtk::Window)> = {
        let (sb, tg, bs, rl) = (
            sidebar.clone(),
            toggle.clone(),
            body_scroll.clone(),
            rule.clone(),
        );
        Rc::new(move |w: &gtk::Window| {
            // `default-width` and not the allocated width, measured: when the
            // window is resized, `notify::default-width` carries the NEW size
            // and the allocation is still the old one — reading `width()` here
            // folded the sidebar away exactly one resize late, which in a test
            // looked like a breakpoint that never fired at all. GTK keeps
            // `default-width` in step with an ordinary resize for precisely
            // this reason (it is what an application stores to restore its
            // geometry). It stops tracking while the window is maximised or
            // fullscreen — it is holding the size to restore TO — so those two
            // states are answered directly rather than from a stale number,
            // and the allocation is only the fallback before there is a size
            // at all.
            let width = if w.default_width() > 0 {
                w.default_width()
            } else {
                w.width()
            };
            let narrow = !w.is_maximized() && !w.is_fullscreen() && width < NARROW;
            tg.set_visible(narrow);
            if narrow {
                sb.set_hexpand(true);
                sb.set_visible(tg.is_active());
                bs.set_visible(!tg.is_active());
            } else {
                tg.set_active(false);
                sb.set_hexpand(false);
                sb.set_visible(true);
                bs.set_visible(true);
            }
            // The divider belongs to the sidebar, not to the window: left
            // behind it would be a hairline down the edge of a pane with
            // nothing on the other side of it.
            rl.set_visible(sb.is_visible() && bs.is_visible());
        })
    };
    // Three signals and not one: `default-width` is what tracks an ordinary
    // resize, and it is deliberately frozen while the window is maximised or
    // fullscreen (it holds the size to restore to), so those two states have to
    // be heard separately or a maximised window would keep the layout of
    // whatever width it was dragged to before.
    {
        let f = relayout.clone();
        win.connect_default_width_notify(move |w| f(w));
    }
    {
        let f = relayout.clone();
        win.connect_maximized_notify(move |w| f(w));
    }
    {
        let f = relayout.clone();
        win.connect_fullscreened_notify(move |w| f(w));
    }

    crate::add_escape_to_close(&win);
    add_window_keys(&win, &search, &toggle);
    win.present();
    relayout(&win);
    // Resume where the last help window left off, which for a first F1 is the
    // first topic. `select_row` drives `row-selected`, so this is also what
    // puts the stack on a page.
    let start = LAST.with(|c| *c.borrow()).min(all.len().saturating_sub(1));
    if let Some(row) = list.row_at_index(start as i32) {
        list.select_row(Some(&row));
    }
    // Focus after `present`: before it, the window has no focus to give. The
    // search box and not the list, because the question a user arrives with is
    // usually a word rather than a heading — and Down from here is the list.
    search.grab_focus();
    // A selectable GtkLabel selects all of its text the moment focus reaches
    // it, and focus can pass through a topic on the way to wherever it is
    // going — so the window opened with topic one as a solid block of
    // selection highlight. Taking the focus away does not clear it; this does.
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

/// F1 closes the help window; Ctrl+F puts the cursor in the search box.
///
/// Ctrl+F is here, on the window, rather than on the search box, because the
/// point of it is to work from the topic list, from the topic text and from the
/// Close button — everywhere the box is *not*. When the sidebar has folded away
/// it unfolds it first, or Ctrl+F would focus something off screen.
///
/// Weak, for the same reason [`crate::add_escape_to_close`] is: the controller
/// belongs to the window, and a strong reference to the window here is a cycle
/// the window never survives. The two widgets are descendants, which reference
/// nothing upwards, but they are held weakly too so the controller outliving
/// its window cannot resurrect a subtree.
fn add_window_keys(win: &gtk::Window, search: &gtk::SearchEntry, toggle: &gtk::ToggleButton) {
    let keys = gtk::EventControllerKey::new();
    let target = win.downgrade();
    let search = search.downgrade();
    let toggle = toggle.downgrade();
    keys.connect_key_pressed(move |_, key, _, state| {
        if key == gtk::gdk::Key::F1 {
            if let Some(w) = target.upgrade() {
                w.close();
            }
            return glib::Propagation::Stop;
        }
        if state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            && key.to_lower() == gtk::gdk::Key::f
        {
            if let Some(t) = toggle.upgrade() {
                if t.is_visible() && !t.is_active() {
                    t.set_active(true);
                }
            }
            if let Some(s) = search.upgrade() {
                s.grab_focus();
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    win.add_controller(keys);
}

/// One topic, as a card in the hub's own style.
///
/// `set_selectable` is not a nicety: a selectable `GtkLabel` is focusable, so
/// Tab walks into the topic, a screen reader reads it, and the text can be
/// copied into a bug report. Without it this window would be a wall of text no
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
    // 58 rather than the 80 this window used when it was one full-width
    // column: the sidebar takes 184 of the 620, so a topic pane is a little
    // over 400px, and a measure wider than the pane would only ever be a
    // request GTK cannot grant.
    d.set_max_width_chars(58);
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
    /// It asserts over **the model**, [`super::topics`], and not over the
    /// widgets that happen to be on screen. That is deliberate now that the
    /// window has a search: a query filters rows, and if this test read the
    /// widgets, a filter would be able to make a fact "uncovered". The model is
    /// the whole list whatever is typed, so no query can reach this assertion —
    /// and `tests/help_window.rs` checks the other direction, that the window
    /// builds a page for every topic in the model.
    ///
    /// No widgets, so it runs in CI, which has no display. The display-side
    /// half of the same contract — every tooltip on a real hub card, whatever
    /// its source — is `tests/help_window.rs`.
    #[test]
    fn every_tooltip_only_fact_is_in_the_help_window() {
        let text = super::topics()
            .iter()
            .map(super::Topic::text)
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

    /// The window that creates the need names the cure, and so does this one.
    ///
    /// Zero profiles ship, so "the game's own options" is a section that says
    /// nothing for every user of this build until they write a profile — and
    /// both this topic and the game-setup window used to stop at *where*
    /// profiles go. The commands are asserted off the constants both surfaces
    /// interpolate, so deleting the paragraph out of either one fails here
    /// rather than leaving the other to look right on its own.
    #[test]
    fn the_help_window_says_how_to_write_a_profile() {
        let text = super::topics()
            .iter()
            .map(super::Topic::text)
            .collect::<Vec<_>>()
            .join("\n\n");
        for cmd in [
            crate::game_setup::PROFILE_SAVE,
            crate::game_setup::PROFILE_CHECK_ADD,
            crate::game_setup::PROFILE_CHECK_WHERE,
            crate::game_setup::PROFILE_CHECK,
        ] {
            assert!(
                text.contains(cmd),
                "a new hub window, a new file format and nothing anywhere saying what to \
                 type: {cmd:?} is in no help topic"
            );
        }
        // And the same sentence the window prints, so somebody reading the
        // help gets the flags rather than being sent back to guess.
        assert!(text.contains(crate::game_setup::ADD_FLAGS), "{text}");
    }

    /// The manual says why an installed bridge can still deliver nothing.
    ///
    /// A user who never opens a terminal reads two surfaces: the setup window
    /// and this one. `tobii bridge install` tells a terminal user that TrackIR
    /// clients are gated by a signature check ours cannot answer, which titles
    /// were measured against it, and that the answering client is a pure
    /// consumer that needs something filling the mapping behind it. Neither
    /// GTK surface said any of it, so "the bridge is installed" was the last
    /// thing the hub had to say to somebody who then got nothing.
    ///
    /// Asserted off the two sources rather than off sentences typed here —
    /// `tobii_config::signature` for the check and
    /// [`tobii_config::signature::provider_note`] for what has to be running — which
    /// is what makes this a test that the three surfaces share one wording
    /// instead of three that happen to agree today.
    #[test]
    fn the_help_window_says_why_an_installed_bridge_can_still_give_nothing() {
        let text = super::topics()
            .iter()
            .map(super::Topic::text)
            .collect::<Vec<_>>()
            .join("\n\n");
        assert!(
            text.contains(&tobii_config::signature::trackir_gate()),
            "the signature gate is not in any topic, so a keyboard-only user reads that the \
             bridge was installed and nothing about why their game is silent:\n{text}"
        );
        assert!(
            text.contains(&tobii_config::signature::provider_note()),
            "and nothing says the answering client needs the provider behind it:\n{text}"
        );
    }

    /// Every fact the Games tab states only in a tooltip is in here too.
    ///
    /// The same rule the rack keeps, applied to the one surface
    /// `tests/help_window.rs` cannot reach: that test walks `CARDS`, which is
    /// the Tracker tab's six, and always was — so the tab's five tooltips were
    /// covered by nothing at all. GTK4 shows a tooltip on pointer hover and on
    /// nothing else, so a fact that lives only in one is unreachable by
    /// keyboard and by touch.
    ///
    /// Asserted over the constants rather than over retyped copies, and
    /// **fact by fact rather than string by string**: a tooltip is written for
    /// a pointer resting on a button and a help topic is written to be read, so
    /// requiring the sentences to match would force one of them to be written
    /// badly. What each pair shares is the claim.
    #[test]
    fn every_fact_the_games_tab_puts_only_in_a_tooltip_is_in_this_window() {
        let games = super::topics()
            .into_iter()
            .find(|t| t.title == "Games")
            .expect("the Games topic");
        let text = games.text();
        let pairs: [(&str, &[&str]); 6] = [
            (
                crate::game_setup::DETAILS_TIP,
                &["tobii bridge status", "without starting anything"][..],
            ),
            (
                crate::game_setup::UNINSTALL_TIP,
                &["Uninstall takes it back out"],
            ),
            (
                crate::game_setup::OTHER_CLIENT_TIP,
                &["NPClient64.dll", "signature check", "has to be filling it"],
            ),
            (
                crate::game_setup::ADD_GAME_TIP,
                &[crate::game_setup::ADD_GAME_CAPTION],
            ),
            (
                crate::game_setup::FORGET_TIP,
                &[crate::game_setup::FORGET_CAPTION, "touches nothing on disk"],
            ),
            // The only control on this tab with no caption at all, so the
            // tooltip is not a second way of reading it — it is the first. If
            // the help topic does not carry the same two facts, a keyboard or
            // touch user has no way to learn what the button does.
            (
                crate::game_setup::SET_UP_ONLY_TIP,
                &["only the games that are set up", "drawn back"],
            ),
        ];
        for (tip, facts) in pairs {
            for fact in facts {
                assert!(
                    text.contains(fact),
                    "the tooltip {tip:?} states {fact:?} and this window does not, so a \
                     keyboard or touch user cannot reach it:\n{text}"
                );
            }
        }
        // And the list is the whole list, so one more tooltip on that tab
        // cannot be added without this test being made to look at it.
        //
        // Against the pairs above rather than against a number written here: a
        // literal is one more thing to update, and updating it is exactly what
        // somebody does instead of adding the pair.
        assert_eq!(
            crate::game_setup::TIPS.len(),
            pairs.len(),
            "a tooltip was added or removed on the Games tab; the pairs above are what \
             says whether the help window still carries its facts"
        );
    }

    /// A switch this program greys out has to say why, where a keyboard and a
    /// touch user can read it.
    ///
    /// The gaze preview needs `wlr-layer-shell`, and on a desktop without it
    /// the switch used to be live: turning it on presented a window the
    /// compositor had no layer for, so nothing appeared and nothing said
    /// anything. It is insensitive there now — and an insensitive switch with
    /// no words beside it is a bug in this program as far as anyone can tell,
    /// so both the card's short line and the whole reason are here.
    ///
    /// The reason is a tooltip on the row, which is the only place it can be:
    /// GTK skips an insensitive widget when it picks a hover target. A tooltip
    /// is invisible to a keyboard and to a touch user either way, so this
    /// window is where it has to be repeated.
    #[test]
    fn the_preview_topic_says_why_the_switch_can_be_greyed_out() {
        let preview = super::topics()
            .into_iter()
            .find(|t| t.title == "Preview my gaze")
            .expect("the preview topic");
        let text = preview.text();
        assert!(
            text.contains(crate::PREVIEW_UNAVAILABLE),
            "the words on the card, so somebody can match what they are looking \
             at against what they are reading:\n{text}"
        );
        assert!(
            text.contains(crate::PREVIEW_UNSUPPORTED),
            "and the whole reason, which only a pointer can reach on the rack:\n{text}"
        );
        assert!(
            text.contains("wlr-layer-shell"),
            "naming the protocol is what stops somebody hunting for a setting in \
             this program that does not exist:\n{text}"
        );
    }

    /// The eye-selection topic says what was measured, and the card says the
    /// half of it that fits on a card.
    ///
    /// The topic read as a guarantee: it said the choice is saved and re-sent
    /// on every connect, which is true, and said nothing about whether the
    /// tracker then detects one eye — which is the only thing a reader wants
    /// from that control. What was measured is that it does not, on its own,
    /// and this project's standing rule is that a measurement gets stated
    /// rather than softened into a caution.
    #[test]
    fn the_eye_topic_says_what_choosing_one_eye_does_not_do() {
        let eyes = super::topics()
            .into_iter()
            .find(|t| t.title == "Select eyes to detect")
            .expect("the eye topic");
        let text = eyes.text();
        // The card's line is in here word for word, because a tooltip and a
        // card are invisible to a keyboard and a touch user and this window is
        // where they read it.
        assert!(text.contains(crate::EYES_CAVEAT), "{text}");
        assert!(
            text.contains("2026-07-20"),
            "the date it was measured, so a later measurement replaces it rather \
             than arguing with it: {text}"
        );
        assert!(
            text.contains("went on reporting both") && text.contains("stored the selection"),
            "both halves: what the tracker kept, and what it did anyway: {text}"
        );
        assert!(
            text.contains("nobody here has watched"),
            "and the limit of what is known, which is the sentence that stops \
             this becoming a promise in the other direction: {text}"
        );
    }

    /// The Games topic may not promise the tracker goes dark on that tab.
    ///
    /// It did, in a sentence four inches from an ALWAYS ON badge that means the
    /// opposite, and both are on screen at once. Showing the Games tab stops
    /// the **hub** asking for the tracker — `hub_wants_tracker` is the whole of
    /// it — and the hub's is one claim of eight. The other seven are standby
    /// turned off, the virtual joystick, a program on the opentrack port, a
    /// calibration, display setup, the gaze preview and the accuracy
    /// diagnostic, and any one of them on its own keeps the illuminators lit
    /// while this tab is showing, which made the sentence simply untrue.
    ///
    /// Asserted over the words rather than over the mechanism, because the
    /// mechanism is a GTK tab and the defect was a promise. What is pinned is
    /// that the topic names the condition and names the badge, so a reader
    /// holding one of those seven is not told their tracker is off.
    #[test]
    fn the_games_topic_does_not_promise_a_tracker_it_does_not_control() {
        let games = super::topics()
            .into_iter()
            .find(|t| t.title == "Games")
            .expect("the Games topic");
        let text = games.text();
        assert!(
            !text.contains("The tracker goes dark while this tab is showing"),
            "the hub is one of several holders and this says it is the only one:\n{text}"
        );
        assert!(
            text.contains("stops the hub asking for the tracker"),
            "it has to say whose claim this is, or the next rewrite widens it back \
             into a promise about the device:\n{text}"
        );
        assert!(
            text.contains("what else is holding it"),
            "and that something else can be:\n{text}"
        );
        assert!(
            text.contains("ALWAYS ON"),
            "and name the badge that is on screen at the same time saying so — the \
             contradiction a reader actually sees:\n{text}"
        );
    }

    /// The MSFS 2024 account is a user's report and has to read as one.
    ///
    /// It is in here because it is the only account anywhere in this project's
    /// notes of that title tracking at all, and MSFS 2024 is also one of the
    /// two titles we measured stopping dead at the signature check. That makes
    /// it worth a reader's time and makes it dangerous: this project has twice
    /// been found recording a demonstration nobody performed. So the
    /// disclaimer is asserted, not the address alone — a paragraph that named
    /// the launcher and dropped the sentence saying nobody here has run it
    /// would pass a test that only looked for the link.
    #[test]
    fn the_reported_launcher_is_marked_as_nobody_here_having_run_it() {
        let text = super::topics()
            .iter()
            .map(super::Topic::text)
            .collect::<Vec<_>>()
            .join("\n\n");
        assert!(
            text.contains(super::REPORTED_LAUNCHER),
            "the report names where it came from, or a reader cannot check it:\n{text}"
        );
        for said in [
            "not verified by anyone here",
            "Nobody on this project has run that",
            "it is not a recommendation",
            "nothing here will set it up",
        ] {
            assert!(
                text.contains(said),
                "a user's report is one paragraph away from reading as a supported route, \
                 and {said:?} is what stops it:\n{text}"
            );
        }
    }

    /// The topics are the hub's own order, so a topic sits where its card sits.
    ///
    /// "Games" goes after the six cards and before the cogwheel, which is where
    /// the tab it documents sits: to the right of everything on tab 1 and left
    /// of the settings that are not on a tab at all. The order is also what
    /// [`super::best_match`] falls back to, so moving this row moves which
    /// topic a word that is in two bodies opens — see
    /// `the_word_on_the_control_opens_the_topic`.
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
                "Games",
                "Settings, behind the cogwheel",
                "Keyboard",
            ]
        );
    }

    /// The search reads the heading and the body, and every word has to land.
    ///
    /// Headless, over [`super::matches`], because that is the whole of the
    /// search: the widget side only calls this and hides the rows it says no
    /// to. Which is also why the empty query is asserted rather than left to
    /// the reading of `all` over no words — an empty search box that matched
    /// nothing would blank the window the moment it opened.
    #[test]
    fn the_search_reads_the_heading_and_the_body() {
        let all = super::topics();
        let hits = |q: &str| -> Vec<&str> {
            all.iter()
                .filter(|t| super::matches(t, q))
                .map(|t| t.title)
                .collect()
        };

        // An empty box is not a search.
        assert_eq!(
            hits("").len(),
            all.len(),
            "an empty query must match every topic"
        );
        assert_eq!(
            hits("   ").len(),
            all.len(),
            "whitespace is not a query either"
        );

        // A heading finds its topic — among others, because "change" and
        // "screen" are both ordinary words that appear in other bodies. Which
        // of them the pane opens on is `best_match`'s job, asserted below.
        assert!(hits("Change screen").contains(&"Change screen"));
        assert!(
            hits("cHaNgE sCrEeN").contains(&"Change screen"),
            "the search must not care about case"
        );

        // A word that is only ever in a body. "opentrack" appears in no
        // heading at all, and it is the word somebody coming from a flight sim
        // will type.
        let opentrack = hits("opentrack");
        assert!(
            opentrack.contains(&"Head tracking"),
            "\"opentrack\" is in the Head tracking body and must be findable: {opentrack:?}"
        );

        // Part of a word, because half-remembering one should not be punished.
        assert!(
            hits("cal").contains(&"Improve my calibration"),
            "a substring of a word must match"
        );

        // Every word has to land somewhere in the same topic: both of these
        // words are in this window, but not together in one topic.
        assert!(
            hits("calibration").len() > 1 && !hits("cogwheel").is_empty(),
            "the two halves of the next query must each match on their own"
        );
        assert!(
            hits("zzzz calibration").is_empty(),
            "a query is an AND: adding a word that matches nothing must narrow to nothing"
        );
    }

    /// A user arrives with the name of a control, so the control's name opens
    /// the topic that explains it.
    ///
    /// Over [`super::best_match`] and not over [`super::matches`]. `matches` is
    /// the filter, and the filter is not what anybody reads — the pane is, and
    /// `best_match` is what puts a topic in it. A guard that asks only whether
    /// the right row survived the filter passes while the window is showing a
    /// different topic altogether, which is exactly the state the search's pick
    /// exists to prevent and so the state worth asserting.
    ///
    /// One table and one derivation, because the words come from two different
    /// promises. The table is the hub's: these words are printed on a control,
    /// so the control's word must open the control's topic. The derivation is
    /// this window's own: the "Keyboard" topic tells the user to type a word
    /// off a control, and every word it quotes as an example has to be one of
    /// the table's.
    ///
    /// That second half is read out of the topic instead of retyped here,
    /// because the prose and this test drifted apart exactly once and that is
    /// the whole finding: the topic went on advertising `strength`, `cogwheel`
    /// and `pitch` as words off the hub's controls after the table had dropped
    /// them for being on no control anywhere. Retyping the list in the test is
    /// what made that possible, so the list is not retyped.
    ///
    /// The check this replaces was `assert!(matches(&all[i], word))` on
    /// `best_match`'s own answer, which cannot fail: `best_match` filters on
    /// `matches` before it picks, so every index it can return already
    /// satisfies it. Worse, the two words it looked like it was defending —
    /// `strength` and `cogwheel` — were reachable at all only because the
    /// Keyboard topic quoted them while advertising them. The topic was its own
    /// evidence, and a test written that way cannot see it.
    #[test]
    fn the_word_on_the_control_opens_the_topic() {
        let all = super::topics();
        let opened = |q: &str| super::best_match(&all, q).map(|i| all[i].title);

        // Words the user can read off the hub: a check-button caption
        // (`games::check_row`), a button caption, a tab caption, or a
        // settings-row description, which `lib::settings_row` draws as a
        // visible label. Each must open the one topic that is about that
        // control.
        //
        const PRINTED_ON_THE_HUB: [(&str, &str); 4] = [
            ("joystick", "Head tracking for games"),
            ("recentre", "Head tracking for games"),
            ("standby", "Eye position"),
            ("tray", "Settings, behind the cogwheel"),
        ];
        for (word, topic) in PRINTED_ON_THE_HUB {
            assert_eq!(
                opened(word),
                Some(topic),
                "typing {word:?} — a word printed on the hub — must OPEN {topic:?}, \
                 and the pane went to {:?}",
                opened(word)
            );
        }

        // KNOWN GAP, measured rather than guessed: **"games" is now a word
        // printed on the hub** — it is one of the two captions in the tab
        // switcher, so it is the name of half the program — and typing it
        // opens "Head tracking for games", not "Games". `best_match`'s
        // tie-break gives the pane to the topic whose HEADING takes every word
        // of the query, and both headings do; the card's comes first.
        //
        // That is not a bug in the tie-break, it is the collision the tab name
        // created: a card called "Head tracking for games" sitting under a tab
        // called Games. Renaming the card to "Game output" dissolves it, which
        // is a change of its own with its own test churn (this table, `CARDS`
        // in `tests/help_window.rs`, and the topic title). Until then the
        // answer a user gets is the card, which is at least a topic about
        // games and not the no-match page — so this is recorded and not
        // asserted, because asserting today's answer would cement it.
        //
        // KNOWN GAP, stated rather than asserted: the three strength presets
        // are captioned `Subtle`, `Normal` and `Strong` (`games::STRENGTHS`),
        // their row carries no caption of its own, and none of those three
        // words appears in any topic — so typing the word printed on the very
        // control you are asking about lands on the no-match page. Closing it
        // means naming the presets in the topic FROM `STRENGTHS`, rather than
        // retyping them here, and that needs the constant to be visible outside
        // `games.rs`. Until it is, the Keyboard topic says the true thing in
        // the user's own terms — that not every caption is in this text, and
        // what to type instead — which is the part that can be honest from in
        // here.

        // The second promise, read off the topic rather than retyped. Every
        // word the Keyboard paragraph puts in quotes is offered to the user as
        // a word to read off a control and type, so every one of them has to be
        // a word this test has already checked is printed on one AND opens that
        // control's topic. Note what that rules out and "it opens something"
        // did not: a word that appears only in the Keyboard topic passes the
        // weaker test, because the paragraph advertising it is itself a match —
        // the window answering an invitation with the invitation.
        let keyboard = all
            .iter()
            .find(|t| t.title == "Keyboard")
            .expect("the Keyboard topic, which documents the search");
        let advertised: Vec<&str> = keyboard.body.split('"').skip(1).step_by(2).collect();
        assert!(
            !advertised.is_empty(),
            "the Keyboard topic quotes no example word at all, so the loop below \
             checks nothing. Either it stopped advertising words — and then this \
             half of the test wants deleting with them — or the quotes went away \
             and took the guard with them"
        );
        for word in advertised {
            assert!(
                PRINTED_ON_THE_HUB.iter().any(|(w, _)| *w == word),
                "the Keyboard topic offers {word:?} as a word to read off a control \
                 and type, but it is not one of the words checked above as printed \
                 on one: {:?}. Either {word:?} is on no control — which is what \
                 \"strength\", \"cogwheel\" and \"pitch\" were, so a user who did \
                 as they were told got the page that suggested it, or nothing at \
                 all — or it is on one, and belongs in that table beside the topic \
                 it must open",
                PRINTED_ON_THE_HUB.map(|(w, _)| w)
            );
        }
    }

    /// Typing a heading opens that heading, even when other topics match too.
    ///
    /// This is the one case where "the first topic the filter left standing" is
    /// visibly wrong, and it is the case a user is most likely to produce: they
    /// can see the nine headings in the sidebar, so they type one. Both words of
    /// "Change screen" appear in two other bodies, and without the tie-break
    /// the pane would open on "Improve my calibration".
    #[test]
    fn typing_a_heading_opens_that_heading() {
        let all = super::topics();
        let opened = |q: &str| super::best_match(&all, q).map(|i| all[i].title);

        assert_eq!(opened("Change screen"), Some("Change screen"));
        assert_eq!(
            opened("head tracking for games"),
            Some("Head tracking for games")
        );
        // A word that is in no heading at all still opens the topic it is in.
        assert_eq!(opened("opentrack"), Some("Head tracking"));
        // Nothing matched is the no-match page, and nothing else. `best_match`
        // collects the hits and falls back to the first of them, so it answers
        // `None` if and only if no topic matches at all — which makes this also
        // the assertion that the no-match page is reachable rather than dead
        // code, since the widget side reads exactly this condition to swap to
        // it.
        assert_eq!(opened("xyzzyplughquux"), None);
        // An empty box opens the first topic, which is what a first F1 does.
        assert_eq!(opened(""), Some(all[0].title));
    }
}
