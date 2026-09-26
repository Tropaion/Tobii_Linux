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
//! reads every topic's heading and the whole of its text: the words on the
//! hub's own controls are in that text, because this page is built from the
//! hub's own strings. The topic list beside it is the second half of the same
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
            // This topic is the window's own documentation, so it has to keep
            // step with `open` below. Every key named here is measured by
            // `tests/help_window.rs`, which drives the same path with GTK's own
            // `child_focus` and `move-cursor` — the actions Tab and the arrow
            // keys resolve to — rather than trusting the sentence.
            "F1 opens this window, and closes it again. Esc closes it too, from \
             anywhere in it, and closes a full-screen setup or calibration flow.\n\n\
             It opens with the search box focused, so you can type your question \
             straight away: the search reads every topic's heading and all of its \
             text, and a word off the control you are looking at — \"joystick\", \
             \"strength\", \"cogwheel\", \"pitch\" — is usually enough. Down moves \
             from the box into the list of topics; Up and Down then walk it, and \
             the topic beside the list changes as you go. Ctrl+F comes back to the \
             search box from anywhere in the window.\n\n\
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
    let hay = format!("{}\n{}", topic.title, topic.body).to_lowercase();
    all_terms_in(&hay, query)
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
pub fn best_match(query: &str) -> Option<usize> {
    let all = topics();
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

    let all = topics();

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
    let toggle = gtk::ToggleButton::with_label("Topics");
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
    for t in &all {
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
        // coverage test asserts over.
        let model = topics();
        list.set_filter_func(move |row| {
            let q = query.borrow();
            model
                .get(row.index() as usize)
                .is_some_and(|t| matches(t, &q))
        });
    }
    {
        let (q, l, st, nm) = (query.clone(), list.clone(), stack.clone(), no_match.clone());
        let n = all.len();
        search.connect_search_changed(move |e| {
            let text = e.text().to_string();
            *q.borrow_mut() = text.clone();
            l.invalidate_filter();
            // The content follows the search, so a one-word query usually
            // answers itself without a click. Three rules, in order: a topic
            // that is still showing stays (typing another letter must not throw
            // away the paragraph being read), else `best_match`, else the page
            // that explains why there is nothing. `select_row` drives
            // `row-selected` below, which is what turns a row into a page.
            let keep = l.selected_row().filter(|r| r.is_child_visible());
            let pick = keep.or_else(|| best_match(&text).and_then(|i| l.row_at_index(i as i32)));
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
        // closes sidebar -> list -> toggle -> sidebar.
        let (sb, bs, rl) = (sidebar.downgrade(), body_scroll.clone(), rule.clone());
        toggle.connect_toggled(move |t| {
            let Some(sb) = sb.upgrade() else { return };
            if t.is_visible() {
                sb.set_visible(t.is_active());
                bs.set_visible(!t.is_active());
                rl.set_visible(false);
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

    /// A user arrives with the name of a control, so the control's name finds
    /// its topic.
    ///
    /// This is the search's actual job, and it is asserted over the words that
    /// are on the hub's own controls rather than over words picked to pass:
    /// every one of these is a caption, a switch label or a readout name the
    /// user can read off the window in front of them.
    #[test]
    fn the_word_on_the_control_finds_the_topic() {
        let all = super::topics();
        for (word, topic) in [
            ("strength", "Head tracking for games"),
            ("joystick", "Head tracking for games"),
            ("recentre", "Head tracking for games"),
            ("pitch", "Head tracking"),
            ("cogwheel", "Settings, behind the cogwheel"),
            ("standby", "Eye position"),
            ("infrared", "Eye position"),
            ("tray", "Settings, behind the cogwheel"),
        ] {
            let hits: Vec<&str> = all
                .iter()
                .filter(|t| super::matches(t, word))
                .map(|t| t.title)
                .collect();
            assert!(
                hits.contains(&topic),
                "typing {word:?} — a word the user can read off the hub — must find \
                 {topic:?}, and instead found {hits:?}"
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
        let opened = |q: &str| super::best_match(q).map(|i| all[i].title);

        assert_eq!(opened("Change screen"), Some("Change screen"));
        assert_eq!(
            opened("head tracking for games"),
            Some("Head tracking for games")
        );
        // A word that is in no heading at all still opens the topic it is in.
        assert_eq!(opened("opentrack"), Some("Head tracking"));
        // Nothing matched is the no-match page, and nothing else.
        assert_eq!(opened("xyzzyplughquux"), None);
        // An empty box opens the first topic, which is what a first F1 does.
        assert_eq!(opened(""), Some(all[0].title));
    }

    /// A query with no answer has to be a state the window can render.
    ///
    /// The widget side reads exactly this condition to swap to its "Nothing
    /// found" page; asserting it here is what stops that page being
    /// unreachable dead code, and what states the condition in the place CI
    /// can check it.
    #[test]
    fn a_query_with_no_answer_matches_no_topic_at_all() {
        let all = super::topics();
        let nothing = "xyzzyplughquux";
        assert!(
            !all.iter().any(|t| super::matches(t, nothing)),
            "this query has to match nothing, or the no-match page is never shown"
        );
    }
}
