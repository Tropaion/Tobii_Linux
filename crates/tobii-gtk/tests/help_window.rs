//! The F1 help window: its three doors, its three exits, and its two promises.
//!
//! Needs a display, so it is ignored by default (CI has none). Run it with:
//!
//!     cargo test -p tobii-gtk --test help_window -- --ignored
//!
//! No tracker is needed: nothing here reads gaze, and the one device fact the
//! test asserts — that opening a window of text takes no claim on the tracker —
//! is the device thread's list of claim reasons, which is state, not hardware.
//!
//! Two promises are worth stating, because both are regressions this project
//! has already paid for once:
//!
//! * **It must not keep itself alive.** GTK4 emits `destroy` only on
//!   finalisation, so a window that holds a strong reference to itself in one
//!   of its own handlers is never freed, and everything it holds stays with it.
//!   That is the v0.3.1 bug, and it is checked here the way
//!   `flows_release_the_tracker.rs` checks it: through a weak reference, after
//!   the close.
//! * **Every fact in a tooltip is also in the help window.** GTK4 has no
//!   focus-triggered or touch-triggered tooltip, so a tooltip-only fact is
//!   unreachable without a pointer. `help.rs`'s own unit test asserts this over
//!   the constants; this one asserts it over the *widgets*, by walking the real
//!   hub's cards and checking every tooltip it actually finds — which is what
//!   catches a tooltip added later, or one retyped instead of referenced.

use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// The six cards of the control rack, in the order they are built.
const CARDS: [&str; 6] = [
    "Improve my calibration",
    "Change screen",
    "Select eyes to detect",
    "Head tracking",
    "Preview my gaze",
    "Head tracking for games",
];

/// `root` and its descendants in `first_child`/`next_sibling` order, descending
/// into a child only when `descend` accepts it.
///
/// One walker with the difference written down, rather than two copies of the
/// descent with one line between them. A child `descend` rejects is neither
/// pushed nor walked into; its siblings still are, and `root` itself is always
/// pushed whatever `descend` would say about it — which is what lets
/// `own_widgets` take a census of a window that is itself a `GtkNative`.
fn walk(root: &gtk::Widget, descend: &dyn Fn(&gtk::Widget) -> bool) -> Vec<gtk::Widget> {
    fn go(w: &gtk::Widget, descend: &dyn Fn(&gtk::Widget) -> bool, out: &mut Vec<gtk::Widget>) {
        out.push(w.clone());
        let mut c = w.first_child();
        while let Some(ch) = c {
            if descend(&ch) {
                go(&ch, descend, out);
            }
            c = ch.next_sibling();
        }
    }
    let mut v = Vec::new();
    go(root, descend, &mut v);
    v
}

/// Every widget under `root`, stopping at nothing. What the finders below
/// search; NOT what the leak census is taken over — see `own_widgets`.
fn all(root: &gtk::Widget) -> Vec<gtk::Widget> {
    walk(root, &|_| true)
}

/// The widgets a window actually OWNS: `all`, minus every subtree that has a
/// surface of its own. This is the set the leak census is taken over.
///
/// `all` follows `first_child`/`next_sibling`, and in GTK4 that walk reaches
/// further than the window's own content. A `GtkTooltipWindow` is a separate
/// surface that GTK creates once per display and parents into whichever
/// toplevel is showing a tooltip *at that moment*; when the next toplevel
/// shows one, the same object moves there. So whether it is inside this tree
/// when a census is taken is decided by where the pointer happens to be
/// sitting, and whether it is alive afterwards is decided by GTK, which still
/// owns it and has every right to.
///
/// That is the whole of the environment-dependence this census used to have,
/// and it is worth writing down because the failure it produced named the
/// wrong thing. Measured on this machine, same binary, same commit: under a
/// rootful `Xwayland :77 -geometry 1600x1200` the census reports 0 of 87
/// widgets alive and the test passes, four runs out of four; under a rootless
/// `Xwayland :79` it reports "4 of 91 widgets and 1 of 144 event controllers
/// outlived" and fails, four runs out of four. The four, printed: a
/// `GtkTooltipWindow`, its `GtkBox`, its `GtkImage` and its `GtkLabel` — and
/// the parent they have after the close is the HUB's `GtkApplicationWindow`.
/// Not the search box, not the sidebar, nothing this window ever built: GTK's
/// tooltip window, lent to the help window while the pointer was over it and
/// taken back afterwards. The fold was innocent in both runs, and so was the
/// focus rule the failure message blamed.
///
/// `GtkNative` is GTK's own word for "owns its surface" — popovers, and that
/// tooltip window. Nothing behind one of them is this window's to free, so
/// the walk stops there.
///
/// And the tooltip is the ONLY thing it drops — which is the half worth
/// stating, because a filter standing in front of a leak census is a fair
/// thing to suspect of hiding the leak, and a review did suspect exactly that.
/// The argument: the only `GtkNative` `help.rs` builds is the toplevel window
/// itself, and the toplevel is `root`, which is pushed before `descend` is
/// consulted. Everything else it builds is a box, a label, a button, a toggle,
/// a search entry, a scroller, a list, a row or a stack — not one of which
/// owns a surface. So the help window has no `GtkNative` CHILD for this to
/// prune, and the subtree that leaks (the sidebar at `help.rs`'s `split` and
/// the "Topics" toggle) hangs off plain boxes and is counted in full.
///
/// The arithmetic says the same thing: this reports 87 widgets and 143
/// controllers, which is the census `2bd9ab1` measured the leak against
/// before any exclusion existed, and the injected leak still reads 80 of 87.
/// Nothing went missing. What would break the argument is a `Popover` or a
/// second `Window` added to `help.rs` later — re-read this comment if one is,
/// because then the exclusion really could start eating evidence.
fn own_widgets(root: &gtk::Widget) -> Vec<gtk::Widget> {
    walk(root, &|ch| !ch.is::<gtk::Native>())
}

/// Every event controller on every widget of `ws`.
///
/// Counted alongside the widgets because a controller is what a leaked handler
/// is *made of*: a closure that captures a sibling lives in one of these, so a
/// subtree that is still alive and a set of controllers that are still alive
/// are two readings of the same fault — and the controllers outnumber the
/// widgets, which makes them the more sensitive of the two.
///
/// Takes the list and not the root so that it is exactly the list the widget
/// census used: a controller reached through a walk the census did not make
/// would be counted against a window that does not own it, which is the bug
/// `own_widgets` exists to describe.
fn controllers_of(ws: &[gtk::Widget]) -> Vec<gtk::EventController> {
    let mut out = Vec::new();
    for w in ws {
        let cs = w.observe_controllers();
        for i in 0..cs.n_items() {
            if let Some(c) = cs.item(i).and_downcast::<gtk::EventController>() {
                out.push(c);
            }
        }
    }
    out
}

/// `help.rs`'s one breakpoint, which is private there.
///
/// Only ever used to decide whether the narrow layout was ASKED to appear —
/// never to assert that it did. If this number drifts from `help.rs`'s, the
/// drift shows up as a skip firing on a healthy machine, which prints; it
/// cannot turn into a silent pass, because a `default-width` below this one
/// must still produce the fold and is still asserted to.
const NARROW: i32 = 520;

/// What the window looked like after it was asked to go below the breakpoint.
#[derive(Debug, Default, Clone, Copy)]
struct Narrowed {
    /// Whether the sidebar is still on screen.
    sidebar_shown: bool,
    /// The "Topics" button's visibility, which IS the narrow flag: `relayout`
    /// is the only thing that sets it.
    toggle_shown: bool,
    /// The width the compositor actually granted. Reported, never decided on
    /// — see `breakpoint_was_asked`.
    width: i32,
    /// `default-width`: what `relayout` reads, and what `set_default_size`
    /// sets whether or not a compositor grants the resize.
    default_width: i32,
    /// Maximised or fullscreen — the two states `relayout` answers "wide" for
    /// whatever the width says, because GTK freezes `default-width` in them
    /// (it is holding the size to restore TO).
    held_wide: bool,
}

impl Narrowed {
    /// Read all five off a window, which is the only way they are ever taken.
    ///
    /// Both readings — the one at 6.9 s and the one the fold phase takes at
    /// 9.8 s — are the same five questions asked of the same window, and they
    /// were written out twice. The two copies were byte-identical but for one
    /// field, which had found the "Topics" button by two different routes and
    /// so read as a difference that meant something; it did not.
    fn of(help: &gtk::Window) -> Self {
        let root = help.clone().upcast::<gtk::Widget>();
        Narrowed {
            sidebar_shown: named(&root, tobii_gtk::help::SIDEBAR_NAME).is_visible(),
            toggle_shown: named(&root, tobii_gtk::help::TOGGLE_NAME).is_visible(),
            width: help.width(),
            default_width: help.default_width(),
            held_wide: help.is_maximized() || help.is_fullscreen(),
        }
    }

    /// Was the breakpoint asked to fire at all?
    ///
    /// The question every skip below turns on, and deliberately NOT "did the
    /// compositor grant the resize". `relayout` reads `default-width`, which
    /// `set_default_size` sets unconditionally, so the fold needs no
    /// cooperation from a compositor at all — and a skip gated on the
    /// allocation would stand the narrow layout down on a machine where it
    /// works. The two things that really can stop the breakpoint are a window
    /// manager that maximised or fullscreened the window, and a
    /// `set_default_size` that never reached the property.
    fn breakpoint_was_asked(&self) -> bool {
        !self.held_wide && self.default_width > 0 && self.default_width < NARROW
    }

    /// Why it was not asked, as a clause a skip line — or the census failure
    /// above it — can end on.
    fn why_not(&self) -> String {
        if self.held_wide {
            "a window manager had maximised or fullscreened the window, and GTK \
             freezes `default-width` in those states, so the breakpoint never \
             saw the 420 it was given"
                .to_string()
        } else {
            format!(
                "`set_default_size(420, …)` left `default-width` at \
                 {}px, which is not below the {NARROW}px breakpoint",
                self.default_width
            )
        }
    }
}

/// The button whose label reads `text`, clicked the way a user would.
///
/// Every label is considered, not just the first: "Help" is both a settings-row
/// heading and the button under it, and only one of them is inside a button.
fn button_labelled(root: &gtk::Widget, text: &str) -> gtk::Button {
    for w in all(root) {
        let is_it = w
            .downcast_ref::<gtk::Label>()
            .is_some_and(|l| l.text() == text);
        if !is_it {
            continue;
        }
        let mut up = Some(w);
        while let Some(x) = up {
            if let Some(b) = x.downcast_ref::<gtk::Button>() {
                return b.clone();
            }
            up = x.parent();
        }
    }
    panic!("no button labelled {text:?}")
}

/// The button carrying `tip` — how the captionless header icons are found.
fn button_with_tooltip(root: &gtk::Widget, tip: &str) -> gtk::Button {
    all(root)
        .into_iter()
        .find_map(|w| {
            let b = w.downcast::<gtk::Button>().ok()?;
            (b.tooltip_text().as_deref() == Some(tip)).then_some(b)
        })
        .unwrap_or_else(|| panic!("no button with the tooltip {tip:?}"))
}

/// The one widget carrying `name`, which `help.rs` sets on each of its parts.
///
/// `widget_name` falls back to the type name when nothing set it, so this
/// cannot match a widget that was never named.
fn named(root: &gtk::Widget, name: &str) -> gtk::Widget {
    all(root)
        .into_iter()
        .find(|w| w.widget_name() == name)
        .unwrap_or_else(|| panic!("no widget named {name:?}"))
}

/// The first descendant label carrying the `section-desc` class.
///
/// One lookup for the two questions that ask it: a topic page's body — the
/// selectable label Tab has to reach — and a hub card's description, which is
/// the same class on the same kind of label and was a second copy of this walk.
fn desc_label(w: &gtk::Widget) -> Option<gtk::Label> {
    all(w).into_iter().find_map(|x| {
        let l = x.downcast::<gtk::Label>().ok()?;
        l.has_css_class("section-desc").then_some(l)
    })
}

/// The body label of one topic page — the selectable one Tab has to reach.
fn body_label(page: &gtk::Widget) -> gtk::Label {
    desc_label(page).expect("every topic page has a body label")
}

fn row_title(row: &gtk::ListBoxRow) -> String {
    row.child()
        .and_downcast::<gtk::Label>()
        .map(|l| l.text().to_string())
        .unwrap_or_default()
}

/// A focused widget, as a line a failure message can be read off.
fn describe(w: &gtk::Widget) -> String {
    let kind = w.type_().to_string();
    let text = if let Some(l) = w.downcast_ref::<gtk::Label>() {
        l.text().to_string()
    } else if let Some(r) = w.downcast_ref::<gtk::ListBoxRow>() {
        row_title(r)
    } else if let Some(b) = w.downcast_ref::<gtk::Button>() {
        // `widget::button` puts a GtkLabel inside rather than setting the
        // button's own label, so `label()` is None for every button this
        // program builds and the caption has to be looked for inside.
        b.label().map(|s| s.to_string()).unwrap_or_else(|| {
            all(b.upcast_ref())
                .into_iter()
                .find_map(|x| x.downcast::<gtk::Label>().ok())
                .map(|l| l.text().to_string())
                .unwrap_or_default()
        })
    } else {
        String::new()
    };
    let text: String = text
        .split('\n')
        .next()
        .unwrap_or("")
        .chars()
        .take(24)
        .collect();
    if text.is_empty() {
        kind
    } else {
        format!("{kind}({text})")
    }
}

/// Is `w` the search box, or the `GtkText` it hands its focus to?
///
/// An ancestor question and not a type comparison: `GtkSearchEntry` delegates
/// its focus to a `GtkText` inside itself, so the focused widget is never the
/// entry. Written out three times before this existed, and the third copy had
/// quietly lost a branch the other two had.
fn in_search(w: &gtk::Widget) -> bool {
    w.widget_name() == tobii_gtk::help::SEARCH_NAME
        || w.ancestor(gtk::SearchEntry::static_type())
            .is_some_and(|a| a.widget_name() == tobii_gtk::help::SEARCH_NAME)
}

/// What `win` has the focus on, as a name an assertion can be written against.
fn focus_name(win: &gtk::Window) -> String {
    gtk::prelude::GtkWindowExt::focus(win)
        .map(|w| {
            if w.widget_name() == tobii_gtk::help::BODY_SCROLL_NAME {
                "the topic pane".to_string()
            } else if in_search(&w) {
                "the search box".to_string()
            } else {
                describe(&w)
            }
        })
        .unwrap_or_default()
}

/// The topics the filter is currently letting through, in list order.
///
/// `is_child_visible`, because that is how `GtkListBox` filters: it leaves the
/// row's own `visible` property alone, so `is_visible` answers true for every
/// row whatever is typed.
fn visible_rows(list: &gtk::ListBox) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(row) = list.row_at_index(i) {
        if row.is_child_visible() {
            out.push(row_title(&row));
        }
        i += 1;
    }
    out
}

fn the_list(root: &gtk::Widget) -> gtk::ListBox {
    named(root, tobii_gtk::help::LIST_NAME)
        .downcast::<gtk::ListBox>()
        .expect("the topic list")
}

fn the_search(root: &gtk::Widget) -> gtk::SearchEntry {
    named(root, tobii_gtk::help::SEARCH_NAME)
        .downcast::<gtk::SearchEntry>()
        .expect("the search box")
}

fn the_stack(root: &gtk::Widget) -> gtk::Stack {
    named(root, tobii_gtk::help::STACK_NAME)
        .downcast::<gtk::Stack>()
        .expect("the topic stack")
}

/// Which page the topic pane is showing.
fn showing(root: &gtk::Widget) -> String {
    the_stack(root)
        .visible_child_name()
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Press a key at `win`'s own key controllers.
///
/// The signal is emitted on the controller rather than synthesised as a device
/// event: a test cannot make a compositor deliver a key press, and what is
/// under test is the handler and its wiring, not GTK's own dispatch.
fn press(win: &gtk::Window, key: gtk::gdk::Key) -> bool {
    press_with(win, key, gtk::gdk::ModifierType::empty())
}

/// The same, at any widget's own controllers — which is how a key that a
/// widget in the middle of the tree handles for itself is driven.
fn press_with(
    win: &impl IsA<gtk::Widget>,
    key: gtk::gdk::Key,
    state: gtk::gdk::ModifierType,
) -> bool {
    let mut handled = false;
    let controllers = win.as_ref().observe_controllers();
    for i in 0..controllers.n_items() {
        let Some(keys) = controllers
            .item(i)
            .and_downcast::<gtk::EventControllerKey>()
        else {
            continue;
        };
        handled |= keys.emit_by_name::<bool>("key-pressed", &[&key.into_glib(), &0u32, &state]);
    }
    handled
}

/// Run `f` once, `ms` into the timeline.
///
/// A free function and not a closure, because a closure cannot be generic: as
/// one, every step of the timeline had to box its own body by hand, which cost
/// each of them three lines of scaffolding and four columns of indent.
fn at<F: FnOnce() + 'static>(ms: u64, f: F) {
    gtk::glib::timeout_add_local_once(Duration::from_millis(ms), f);
}

fn help_windows(app: &gtk::Application) -> Vec<gtk::Window> {
    app.windows()
        .into_iter()
        .filter(|w| w.title().as_deref() == Some("Help"))
        .collect()
}

/// One card of the rack, as the test sees it.
#[derive(Debug, Default, Clone)]
struct Card {
    title: &'static str,
    /// The visible description, if the card still has one.
    desc: Option<String>,
    /// Every tooltip anywhere inside the card.
    tooltips: Vec<String>,
}

fn card_widget(hub: &gtk::Widget, title: &str) -> gtk::Widget {
    all(hub)
        .into_iter()
        .find(|w| {
            w.downcast_ref::<gtk::Label>()
                .is_some_and(|l| l.text() == title && l.has_css_class("section-title"))
        })
        .and_then(|l| l.parent())
        .unwrap_or_else(|| panic!("no card titled {title:?}"))
}

fn description_of(card: &gtk::Widget) -> Option<String> {
    desc_label(card).map(|l| l.text().to_string())
}

fn rack(hub: &gtk::Widget) -> Vec<Card> {
    CARDS
        .iter()
        .map(|title| {
            let w = card_widget(hub, title);
            let mut card = Card {
                title,
                desc: description_of(&w),
                ..Default::default()
            };
            for x in all(&w) {
                if let Some(t) = x.tooltip_text() {
                    card.tooltips.push(t.to_string());
                }
            }
            card.tooltips.sort();
            card.tooltips.dedup();
            card
        })
        .collect()
}

/// Every topic of the help window, as one blob to search.
fn help_text() -> String {
    tobii_gtk::help::topics()
        .iter()
        .map(tobii_gtk::help::Topic::text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[derive(Debug, Default)]
struct Seen {
    /// Claim reasons before the help window was opened, and while it was open.
    reasons_before: Vec<&'static str>,
    reasons_while_open: Vec<&'static str>,
    /// One entry per door and per exit: how many help windows were open after.
    opened: Vec<(&'static str, usize)>,
    closed: Vec<(&'static str, usize)>,
    /// One entry per topic page the window built: page name, the body text on
    /// it, and whether that body is focusable. Walked through the stack by the
    /// names `help.rs` gives the pages, so a topic the model has and the window
    /// never built is a missing entry rather than a silently shorter list.
    pages: Vec<(String, String, bool)>,
    scroller_focusable: bool,
    focus_on_open: Option<String>,
    /// Whether the compositor had made the help window the ACTIVE window when
    /// the Tab chain below was walked. Recorded because the walk depends on it
    /// and cannot say so for itself: see the assertion.
    active_for_tab_walk: bool,
    /// Whether that focus is inside the search box.
    focus_in_search_on_open: bool,
    /// The Tab chain from the state the window opens in, focus by focus, as
    /// GTK's own focus walk produces it.
    tab_order: Vec<String>,
    /// Every topic row GTK's focus walk stops on, walked forward from the
    /// first row rather than from the search box.
    ///
    /// The chain above is where Tab GOES; this is which rows it can REACH,
    /// and they are separate readings because where GTK enters a `GtkListBox`
    /// from outside it is GTK's own business and is not fixed — see the walk.
    tab_rows: Vec<String>,
    /// Where Tab and Shift+Tab go when the list handles them itself — which
    /// `child_focus` cannot show, because it moves focus without dispatching a
    /// key at all.
    tab_from_list: (String, String),
    /// Walking the topic list with the Down arrow — GTK's own `move-cursor`,
    /// which is the action the arrow key resolves to: the row that took the
    /// focus, and the page the pane switched to because of it.
    by_arrow: Vec<(String, String)>,
    /// At the width the window opens at: sidebar width, topic pane width,
    /// sidebar shown, "Topics" button shown. And the window's own minimum.
    at_620: (i32, i32, bool, bool),
    min_width: i32,
    /// Typing one word: the rows left in the list, and the page shown.
    search_rows: Vec<String>,
    search_page: String,
    /// What the search box actually held, and how many rows the list actually
    /// had, at the instant the two readings above were taken.
    ///
    /// The phase's own precondition, recorded because without it an empty
    /// `search_rows` is three different faults wearing one face: the query
    /// never reached the box, the list it was read from was not the topic
    /// list, or the filter really did drop the topic that answers the query —
    /// and only the last is a bug in the window. It read `[]` twice in 22 runs
    /// of this test on a live desktop, and the failure could say nothing about
    /// which of the three it was.
    search_precondition: (String, usize),
    /// Typing a word that is in no topic at all.
    no_match_rows: Vec<String>,
    no_match_page: String,
    no_match_text: String,
    no_match_placeholder: Option<String>,
    /// The page showing after the search box is cleared again.
    cleared_page: String,
    /// Narrowed below the breakpoint: what the layout did, and what it was
    /// asked to do. Both halves, because "the sidebar is still there" and
    /// "the breakpoint was never asked to fire" are the same reading and only
    /// one of them is a bug.
    at_420: Narrowed,
    /// Ctrl+F while narrow: sidebar shown afterwards, and where the focus went.
    ctrl_f: (bool, String),
    /// Widened again: sidebar shown, "Topics" button shown.
    back_at_620: (bool, bool),
    /// Escape inside the search box — how many help windows were left.
    left_after_stop_search: Option<usize>,
    /// The topic showing when the window was closed, and the topic showing
    /// when it was opened again.
    resume: (String, String),
    /// Characters of topic text selected when the window opens. A selectable
    /// label selects all of itself the moment focus touches it, and focus
    /// passes through the first topic on its way to the scroller — so without
    /// clearing it the window opens as a block of highlight, which is what a
    /// reviewer photographed on a real compositor.
    ///
    /// This assertion is a GUARD, not a proof: removing the `select_region`
    /// that fixes it leaves this test passing. The harness is granted focus
    /// (`focus_on_open` really is the scroller, and the nine bodies really are
    /// focusable) and still reports nothing selected, so whatever routes focus
    /// through the label first does not happen here. Kept because the
    /// invariant is right and cheap to check, not because it caught anything.
    selected_on_open: i32,
    /// Whether the last help window was still alive 800 ms after closing.
    alive_after_close: bool,
    /// How many of the window's INNER widgets were still alive 800 ms after
    /// closing. The window freeing is not enough: two siblings that capture
    /// each other — the search box and the topic list, or the sidebar and the
    /// narrow-mode toggle — keep each other's refcount off zero, so the whole
    /// content subtree leaks while the window itself disappears. A test that
    /// weak-refs only the window cannot see that, and did not.
    inner_alive_after_close: usize,
    /// How many widgets that census weak-ref'd at all.
    ///
    /// Asserted before the count above, for the same reason the narrow census
    /// below states: "nothing survived" and "nothing was ever weak-ref'd" are
    /// the same zero. This one had no size guard while the narrow one was
    /// being given its second — a census that cannot fail for the reason it
    /// names is the shape this project has shipped six of.
    wide_census: usize,
    /// How many widgets and event controllers the narrow-mode census weak-ref'd
    /// at all. Asserted before the count below, because "nothing survived" and
    /// "nothing was ever counted" are the same number.
    narrow_census: (usize, usize),
    /// The topic showing when a reopened window resumed, and the topic showing
    /// after the first word was typed into its empty search box.
    first_query: (String, String),
    /// How many of ALL the window's own widgets and ALL its own event
    /// controllers were still alive 800 ms after closing a window that had
    /// been narrowed and had its "Topics" list unfolded and folded again.
    ///
    /// A second census and not a move of the one above: that one is taken over
    /// a window that was only ever wide, and the fold is the sequence that
    /// leaks, so a census taken before it cannot see this however thorough it
    /// is. This project has shipped a leak census taken before the phase that
    /// leaks once already.
    narrow_alive_after_close: (usize, usize),
    /// What the fold phase found when it went to press "Topics".
    ///
    /// `None` means the phase never ran at all — there was no help window to
    /// narrow — which is a fault in the timeline and not an environment, and
    /// is asserted as one.
    fold: Option<Narrowed>,
    /// Whether the unfold and the fold back each actually took. Recorded
    /// rather than asserted on the spot, for the reason the fold phase gives.
    unfolded: bool,
    folded: bool,
    /// Where the focus was at the moment the sidebar folded away.
    ///
    /// Asserted, because the census below is a test of the leak only while
    /// this is inside the pane that is about to be hidden. Measured: take
    /// `2bd9ab1`'s focus handoff back out of `toggle.connect_toggled` and the
    /// focus sits on the Close button instead, the fold then hides a pane the
    /// focus was never in, and the census goes green over a sequence that no
    /// longer sets the leak up at all. A census passing because its
    /// precondition disappeared is the failure this file already has one of —
    /// the 3100 ms census standing in front of a phase that starts at 6400 —
    /// and this is the guard against the second one.
    ///
    /// So the focus really is inside the hidden sidebar when the census is
    /// taken, on purpose, and the census is green with it there. That is the
    /// census testing the leak rather than stepping around it, and it is also
    /// the answer to a review note that read the fold's un-handed-back focus
    /// as the residual this test was reporting: it was not, on either display
    /// this was run on. The residual was GTK's tooltip window, and
    /// `own_widgets` says where it came from.
    focus_at_fold: String,
    /// The six cards of the real hub.
    rack: Vec<Card>,
    /// Per card: its description, its height at the narrowest its own column
    /// is ever allocated, and its height with room to spare.
    widths: Vec<(&'static str, Option<String>, i32, i32)>,
    /// How many help windows were left open after the hub hid itself.
    open_after_hub_hidden: Option<usize>,
}

#[test]
#[ignore = "needs a display"]
fn the_help_window_opens_closes_frees_itself_and_covers_every_rack_tooltip() {
    let app = gtk::Application::builder()
        .application_id("dev.tobii.test.help")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let seen: Rc<RefCell<Seen>> = Rc::default();
    {
        let seen = seen.clone();
        app.connect_activate(move |app| {
            tobii_gtk::load_css();
            let session = tobii_gtk::device::spawn();
            let demand = session.2.clone();
            let hub = tobii_gtk::build_hub(app, session).expect("hub");
            hub.present();

            // --- door 1: F1 on the hub. Exit 1: Escape. ---
            {
                let (h, d, s) = (hub.clone(), demand.clone(), seen.clone());
                at(600, move || {
                    s.borrow_mut().reasons_before = d.reasons();
                    assert!(
                        press(h.upcast_ref(), gtk::gdk::Key::F1),
                        "F1 was not handled by the hub window"
                    );
                });
            }
            {
                let (a, d, s) = (app.clone(), demand.clone(), seen.clone());
                at(1000, move || {
                    let wins = help_windows(&a);
                    s.borrow_mut().opened.push(("F1", wins.len()));
                    s.borrow_mut().reasons_while_open = d.reasons();
                    let Some(help) = wins.first() else { return };
                    let root = help.clone().upcast::<gtk::Widget>();
                    let mut s = s.borrow_mut();

                    // One page per topic, asked for by the name `help.rs`
                    // gives it, and each body focusable — a selectable
                    // GtkLabel is, which is what lets Tab into a topic at
                    // all. This is the widget half of the coverage
                    // contract: the model is asserted over in `help.rs`,
                    // and this is what stops the window quietly building
                    // fewer pages than the model has topics.
                    let stack = the_stack(&root);
                    for (i, _) in tobii_gtk::help::topics().iter().enumerate() {
                        let name = format!("t{i}");
                        // A page the window never built is a SHORT list,
                        // not a panic. This runs inside a glib timeout
                        // closure, and a panic here crosses an extern "C"
                        // trampoline: it aborts the process, so the
                        // assertion that names the fault never gets to
                        // speak — and the length check it leaves behind
                        // could then never fail, because the list was built
                        // by pushing one entry per model topic.
                        let Some(page) = stack.child_by_name(&name) else {
                            continue;
                        };
                        let l = body_label(&page);
                        s.pages.push((name, l.text().to_string(), l.is_focusable()));
                    }
                    // The scroller Page Up and Page Down drive.
                    s.scroller_focusable =
                        named(&root, tobii_gtk::help::BODY_SCROLL_NAME).is_focusable();
                    // Selection, over every body in the window including
                    // the no-match page's.
                    for w in all(&root) {
                        if let Some(l) = w.downcast_ref::<gtk::Label>() {
                            if l.has_css_class("section-desc") {
                                if let Some((a, b)) = l.selection_bounds() {
                                    s.selected_on_open += b - a;
                                }
                            }
                        }
                    }
                    let focus = gtk::prelude::GtkWindowExt::focus(help);
                    s.focus_on_open = focus.as_ref().map(describe);
                    s.active_for_tab_walk = help.is_active();
                    // GtkSearchEntry hands its focus to the GtkText inside
                    // it, so "is it the search box" is an ancestor question
                    // and not a type comparison.
                    s.focus_in_search_on_open = focus.as_ref().is_some_and(in_search);
                    // The Tab chain, from exactly the state the window
                    // opens in. `child_focus` is what a Tab press resolves
                    // to, so this is the path and not a model of it.
                    if let Some(f) = focus.as_ref() {
                        s.tab_order.push(describe(f));
                    }
                    // One full cycle: the chain wraps back to the search
                    // box, and walking past that would count every row
                    // twice. Worth knowing while reading the result: GTK
                    // selects a list row when the focus lands on it, so
                    // Tab-ing through the list changes the topic as it
                    // goes, and the body this chain arrives at is the topic
                    // it last passed through.
                    for _ in 0..24 {
                        if !help.child_focus(gtk::DirectionType::TabForward) {
                            break;
                        }
                        let Some(w) = gtk::prelude::GtkWindowExt::focus(help) else {
                            break;
                        };
                        let d = describe(&w);
                        if Some(&d) == s.tab_order.first() {
                            break;
                        }
                        s.tab_order.push(d);
                    }
                    // The same walk again, started INSIDE the list instead of
                    // asked to enter it — and this is the one that carries the
                    // "every row is on GTK's own focus walk" claim.
                    //
                    // Where the chain above ENTERS the list is GTK's own
                    // choice, and on a live desktop it is not always the first
                    // row: measured over 22 runs of this test on one KDE
                    // session, with the list in provably the same state every
                    // time (row 0 selected, no focus child, all nine rows
                    // child-visible, mapped, focusable and sensitive, the
                    // scroller at the top), the chain entered at row 1, row 3
                    // and row 5 as well as at row 0. A chain that enters at
                    // row 5 still walks every row from there to the last and
                    // still ends on Close — it is short at the FRONT, not
                    // broken — but counting it against the model's length made
                    // this test fail about one run in five for a window in
                    // which nothing was wrong.
                    //
                    // So the entry point is taken out of the question rather
                    // than asserted about: give the first row the focus, then
                    // ask GTK to move forward and write down every row it
                    // stops on until it leaves the list. `child_focus` is
                    // still GTK's own focus walk and still the action a Tab
                    // press resolves to, so the claim is unchanged — what is
                    // gone is a degree of freedom that was never part of it.
                    // The chain above keeps the rest of the claim: that Tab
                    // out of the search box reaches the list at all, reaches a
                    // topic's text after it, and reaches Close after that.
                    let list = the_list(&root);
                    if let Some(first) = list.row_at_index(0) {
                        first.grab_focus();
                    }
                    for _ in 0..24 {
                        let Some(w) = gtk::prelude::GtkWindowExt::focus(help) else {
                            break;
                        };
                        let Ok(row) = w.downcast::<gtk::ListBoxRow>() else {
                            break;
                        };
                        s.tab_rows.push(row_title(&row));
                        if !help.child_focus(gtk::DirectionType::TabForward) {
                            break;
                        }
                    }
                    // And back to where the chain above left it — the search
                    // box, which is what one full cycle wraps round to — so
                    // the phases after this start from the state they used to.
                    the_search(&root).grab_focus();
                });
            }
            // A second F1 must raise the window it already opened, not open a
            // second one.
            {
                let (h, a, s) = (hub.clone(), app.clone(), seen.clone());
                at(1300, move || {
                    press(h.upcast_ref(), gtk::gdk::Key::F1);
                    s.borrow_mut()
                        .opened
                        .push(("F1 again", help_windows(&a).len()));
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(1600, move || {
                    let help = help_windows(&a).remove(0);
                    assert!(
                        press(&help, gtk::gdk::Key::Escape),
                        "Escape was not handled by the help window"
                    );
                    s.borrow_mut().closed.push(("Escape", usize::MAX));
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(1900, move || {
                    let n = help_windows(&a).len();
                    s.borrow_mut().closed.last_mut().unwrap().1 = n;
                });
            }

            // --- door 2: the "?" button in the header. Exit 2: F1 again. ---
            {
                let (h, a, s) = (hub.clone(), app.clone(), seen.clone());
                at(2200, move || {
                    button_with_tooltip(h.upcast_ref(), "Help (F1)").emit_clicked();
                    s.borrow_mut()
                        .opened
                        .push(("? button", help_windows(&a).len()));
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(2500, move || {
                    let help = help_windows(&a).remove(0);
                    assert!(
                        press(&help, gtk::gdk::Key::F1),
                        "F1 was not handled by the help window"
                    );
                    s.borrow_mut().closed.push(("F1", help_windows(&a).len()));
                });
            }

            // --- door 3: the cogwheel's Help row. Exit 3: the Close button. ---
            {
                let (h, a, s) = (hub.clone(), app.clone(), seen.clone());
                at(2800, move || {
                    button_labelled(h.upcast_ref(), "Help").emit_clicked();
                    s.borrow_mut()
                        .opened
                        .push(("cogwheel", help_windows(&a).len()));
                });
            }
            // The weak reference is taken with the window still open, so what
            // is checked after the close is whether anything still holds it.
            let weak: Rc<RefCell<Option<gtk::glib::WeakRef<gtk::Window>>>> = Rc::default();
            let inner: Rc<RefCell<Vec<gtk::glib::WeakRef<gtk::Widget>>>> = Rc::default();
            {
                let (a, s, weak, inner) = (app.clone(), seen.clone(), weak.clone(), inner.clone());
                at(3100, move || {
                    let help = help_windows(&a).remove(0);
                    *weak.borrow_mut() = Some(help.downgrade());
                    for w in all(help.upcast_ref()) {
                        if w.downcast_ref::<gtk::SearchEntry>().is_some()
                            || w.downcast_ref::<gtk::ListBox>().is_some()
                            || w.downcast_ref::<gtk::Stack>().is_some()
                        {
                            inner.borrow_mut().push(w.downgrade());
                        }
                    }
                    button_labelled(help.upcast_ref(), "Close").emit_clicked();
                    s.borrow_mut().closed.push(("Close button", usize::MAX));
                });
            }
            {
                let (a, s, weak, inner) = (app.clone(), seen.clone(), weak.clone(), inner.clone());
                at(3900, move || {
                    let mut s = s.borrow_mut();
                    s.closed.last_mut().unwrap().1 = help_windows(&a).len();
                    s.alive_after_close =
                        weak.borrow().as_ref().and_then(|w| w.upgrade()).is_some();
                    s.wide_census = inner.borrow().len();
                    s.inner_alive_after_close = inner
                        .borrow()
                        .iter()
                        .filter(|w| w.upgrade().is_some())
                        .count();
                });
            }

            // The cards as the hub really built them, sampled late so the games
            // row's own refresh (driven by the hub's 33 ms tick) has set the
            // dynamic tooltips.
            {
                let (h, s) = (hub.clone(), seen.clone());
                at(4100, move || {
                    s.borrow_mut().rack = rack(h.upcast_ref());
                });
            }
            // How tall each card is at the narrowest its own column is ever
            // allocated, against how tall it is with room to spare. See the
            // assertions for why that is the question, and not "how tall at
            // 380".
            {
                let (h, s) = (hub.clone(), seen.clone());
                at(4200, move || {
                    let root = h.clone().upcast::<gtk::Widget>();
                    for title in CARDS {
                        let card = card_widget(&root, title);
                        let column = card.parent().expect("a card is in a column");
                        // The window's default width is the three-column
                        // layout's minimum plus the page margins, and that
                        // minimum is what the columns measure.
                        let min = column.measure(gtk::Orientation::Horizontal, -1).0;
                        let at_min = card.measure(gtk::Orientation::Vertical, min).1;
                        let roomy = card.measure(gtk::Orientation::Vertical, min + 240).1;
                        s.borrow_mut()
                            .widths
                            .push((title, description_of(&card), at_min, roomy));
                    }
                });
            }
            // Hiding the hub — what closing to the tray does — must take the
            // help window with it. `destroy_with_parent` does not cover a
            // parent that is merely hidden, and a transient window whose parent
            // is hidden is left to the compositor.
            {
                let (h, s) = (hub.clone(), seen.clone());
                at(4400, move || {
                    button_with_tooltip(h.upcast_ref(), "Help (F1)").emit_clicked();
                    assert_eq!(
                        help_windows(&h.application().unwrap()).len(),
                        1,
                        "the help window did not reopen for the hide test"
                    );
                    h.set_visible(false);
                    s.borrow_mut().open_after_hub_hidden =
                        Some(help_windows(&h.application().unwrap()).len());
                });
            }
            {
                let h = hub.clone();
                at(4600, move || h.present());
            }

            // --- the sidebar, the search, and what the window does when it is
            // --- made too narrow to hold both
            //
            // All of it on this one timeline and not in a second `#[test]`:
            // cargo runs tests in threads, and two GTK main loops in one
            // process is not a thing that works.
            {
                let h = hub.clone();
                at(4800, move || {
                    assert!(
                        press(h.upcast_ref(), gtk::gdk::Key::F1),
                        "F1 did not reopen the help window for the sidebar phase"
                    );
                });
            }
            // At the width it opens at: who got how much, and what the whole
            // window's minimum is. Then the topic list walked with the arrow
            // key — GTK's own `move-cursor`, which is the action Down resolves
            // to — one press per topic, checking the pane follows.
            {
                let (a, s) = (app.clone(), seen.clone());
                at(5000, move || {
                    let help = help_windows(&a).remove(0);
                    let root = help.clone().upcast::<gtk::Widget>();
                    let list = the_list(&root);
                    let sidebar = named(&root, tobii_gtk::help::SIDEBAR_NAME);
                    let pane = named(&root, tobii_gtk::help::BODY_SCROLL_NAME);
                    let toggle = named(&root, tobii_gtk::help::TOGGLE_NAME);
                    {
                        let mut s = s.borrow_mut();
                        s.at_620 = (
                            sidebar.width(),
                            pane.width(),
                            sidebar.is_visible(),
                            toggle.is_visible(),
                        );
                        s.min_width = help.measure(gtk::Orientation::Horizontal, -1).0;
                    }
                    // Start the cursor on the first row, which is what Down
                    // out of the search box does, then walk.
                    // Both: `grab_focus` sets the cursor row that
                    // `move-cursor` walks from, and `select_row` is what
                    // the reading below asks about. The window resumed on
                    // whichever topic the Tab chain above left it on, which
                    // is the resume behaviour working, not a stray state.
                    if let Some(first) = list.row_at_index(0) {
                        list.select_row(Some(&first));
                        first.grab_focus();
                    }
                    for _ in 0..tobii_gtk::help::topics().len() {
                        let title = list
                            .selected_row()
                            .map(|r| row_title(&r))
                            .unwrap_or_default();
                        s.borrow_mut().by_arrow.push((title, showing(&root)));
                        list.emit_move_cursor(gtk::MovementStep::DisplayLines, 1, false, false);
                    }

                    // Tab and Shift+Tab as the LIST handles them. Emitted
                    // at its own controller for the reason `press` gives:
                    // a test cannot make a compositor deliver a key, and
                    // what is under test is the handler and its wiring.
                    // `child_focus` above cannot show this at all — it
                    // moves focus without dispatching a key — which is why
                    // both are measured and both are asserted.
                    if let Some(row) = list.row_at_index(3) {
                        row.grab_focus();
                    }
                    press_with(&list, gtk::gdk::Key::Tab, gtk::gdk::ModifierType::empty());
                    s.borrow_mut().tab_from_list.0 = focus_name(&help);
                    if let Some(row) = list.row_at_index(3) {
                        row.grab_focus();
                    }
                    press_with(
                        &list,
                        gtk::gdk::Key::Tab,
                        gtk::gdk::ModifierType::SHIFT_MASK,
                    );
                    s.borrow_mut().tab_from_list.1 = focus_name(&help);
                    // Put the selection back where the arrow walk left it,
                    // so the phases after this start from a known topic.
                    if let Some(row) = list.row_at_index(0) {
                        list.select_row(Some(&row));
                    }
                });
            }
            // One word into the search box. `GtkSearchEntry` debounces
            // `search-changed`, so every reading below is taken a clear 400 ms
            // after the typing that causes it.
            {
                let a = app.clone();
                at(5200, move || {
                    let help = help_windows(&a).remove(0);
                    the_search(&help.upcast()).set_text("joystick");
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(5600, move || {
                    let root = help_windows(&a).remove(0).upcast::<gtk::Widget>();
                    let list = the_list(&root);
                    let mut rows = 0;
                    while list.row_at_index(rows).is_some() {
                        rows += 1;
                    }
                    let mut s = s.borrow_mut();
                    // The precondition first, and off the same widgets in the
                    // same instant as the readings it stands in front of.
                    s.search_precondition = (the_search(&root).text().to_string(), rows as usize);
                    s.search_rows = visible_rows(&list);
                    s.search_page = showing(&root);
                });
            }
            // And a word that is in no topic at all: an empty window with no
            // explanation is the bug this page exists to prevent.
            {
                let a = app.clone();
                at(5800, move || {
                    let help = help_windows(&a).remove(0);
                    the_search(&help.upcast()).set_text("xyzzyplughquux");
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(6200, move || {
                    let root = help_windows(&a).remove(0).upcast::<gtk::Widget>();
                    let stack = the_stack(&root);
                    let mut s = s.borrow_mut();
                    s.no_match_rows = visible_rows(&the_list(&root));
                    s.no_match_page = showing(&root);
                    if let Some(page) = stack.child_by_name(tobii_gtk::help::NO_MATCH_PAGE) {
                        s.no_match_text = body_label(&page).text().to_string();
                    }
                    // The sidebar's half of the same promise: GtkListBox
                    // shows its placeholder when the filter leaves nothing.
                    // GtkListBox parents its placeholder to itself and
                    // shows it by child visibility, exactly as it hides a
                    // filtered row — so it is found the same way, and the
                    // rows' own labels carry no `section-desc` class.
                    s.no_match_placeholder = all(&the_list(&root).upcast::<gtk::Widget>())
                        .into_iter()
                        .find_map(|w| {
                            let l = w.downcast::<gtk::Label>().ok()?;
                            (l.has_css_class("section-desc") && l.is_child_visible())
                                .then(|| l.text().to_string())
                        });
                });
            }
            // Now make it too narrow to hold a sidebar and a topic.
            {
                let a = app.clone();
                at(6400, move || {
                    let help = help_windows(&a).remove(0);
                    the_search(&help.clone().upcast()).set_text("");
                    help.set_default_size(420, 660);
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(6900, move || {
                    let help = help_windows(&a).remove(0);
                    let root = help.clone().upcast::<gtk::Widget>();
                    {
                        let mut s = s.borrow_mut();
                        s.cleared_page = showing(&root);
                        s.at_420 = Narrowed::of(&help);
                    }
                    // Ctrl+F has to reach the search box from here, which
                    // means unfolding the sidebar it lives in first.
                    assert!(
                        press_with(
                            &help,
                            gtk::gdk::Key::f,
                            gtk::gdk::ModifierType::CONTROL_MASK
                        ),
                        "Ctrl+F was not handled by the help window"
                    );
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(7100, move || {
                    let help = help_windows(&a).remove(0);
                    let root = help.clone().upcast::<gtk::Widget>();
                    s.borrow_mut().ctrl_f = (
                        named(&root, tobii_gtk::help::SIDEBAR_NAME).is_visible(),
                        focus_name(&help),
                    );
                    help.set_default_size(620, 660);
                });
            }
            // Wide again, then Escape from inside the search box — which
            // GtkSearchEntry turns into `stop-search` and swallows, so it is
            // the one place Escape could silently stop closing the window.
            {
                let (a, s) = (app.clone(), seen.clone());
                at(7500, move || {
                    let help = help_windows(&a).remove(0);
                    let root = help.clone().upcast::<gtk::Widget>();
                    let list = the_list(&root);
                    {
                        let mut s = s.borrow_mut();
                        s.back_at_620 = (
                            named(&root, tobii_gtk::help::SIDEBAR_NAME).is_visible(),
                            named(&root, tobii_gtk::help::TOGGLE_NAME).is_visible(),
                        );
                    }
                    // Leave it on a topic that is neither the first nor the
                    // last, so "it resumed" cannot be confused with "it
                    // always opens at the top".
                    if let Some(row) = list.row_at_index(5) {
                        list.select_row(Some(&row));
                    }
                    s.borrow_mut().resume.0 = showing(&root);
                    the_search(&root).emit_by_name::<()>("stop-search", &[]);
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(7700, move || {
                    s.borrow_mut().left_after_stop_search = Some(help_windows(&a).len());
                });
            }
            {
                let h = hub.clone();
                at(7900, move || {
                    press(h.upcast_ref(), gtk::gdk::Key::F1);
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(8100, move || {
                    let Some(help) = help_windows(&a).first().cloned() else {
                        return;
                    };
                    s.borrow_mut().resume.1 = showing(&help.upcast());
                });
            }

            // --- the first query typed into a reopened window ---
            //
            // Leave the window on the "Keyboard" topic, close it, and press F1
            // again: it resumes there, by design. That topic is the one that
            // tells a user this window has a search at all, and it names
            // "joystick", "strength", "cogwheel" and "pitch" as the words to
            // try — so it quotes all four, and a rule that keeps the topic
            // already showing whenever it still matches keeps this one for
            // every word it just advertised.
            {
                let a = app.clone();
                at(8200, move || {
                    let help = help_windows(&a).remove(0);
                    let root = help.clone().upcast::<gtk::Widget>();
                    let list = the_list(&root);
                    let last = tobii_gtk::help::topics().len() - 1;
                    if let Some(row) = list.row_at_index(last as i32) {
                        list.select_row(Some(&row));
                    }
                    assert_eq!(
                        row_title(&list.selected_row().expect("a selected row")),
                        "Keyboard",
                        "this phase reads the topic that advertises the search"
                    );
                    help.close();
                });
            }
            {
                let h = hub.clone();
                at(8500, move || {
                    press(h.upcast_ref(), gtk::gdk::Key::F1);
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(8700, move || {
                    let help = help_windows(&a).remove(0);
                    let root = help.clone().upcast::<gtk::Widget>();
                    s.borrow_mut().first_query.0 = showing(&root);
                    the_search(&root).set_text("joystick");
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(9100, move || {
                    let root = help_windows(&a).remove(0).upcast::<gtk::Widget>();
                    s.borrow_mut().first_query.1 = showing(&root);
                    the_search(&root).set_text("");
                });
            }

            // --- narrow mode's fold, and what it leaves behind ---
            //
            // The census at 3100 above is taken over a window that was only
            // ever wide, so it cannot reach this at all: it is the "Topics"
            // button — the way back to the topic list once the sidebar has
            // folded away — that leaves the content subtree behind.
            //
            // The unfold and the fold are a tick apart on purpose. Run in one
            // main-loop iteration they free everything and prove nothing, which
            // is the shape of a reproducer that reports the bug fixed.
            //
            // None of this needs a compositor to agree to anything, which is
            // the point worth knowing before reading the phases below. The fold
            // is `relayout`'s, `relayout` reads `default-width`, and
            // `set_default_size` sets that property whether or not the resize
            // is ever granted — so this sequence runs, and the census means
            // what it says, on a bare X server with no window manager. Checked
            // by injection, twice, with the leak of `2bd9ab1` put back into
            // `toggle.connect_toggled`: "80 of 87 widgets and 127 of 143 event
            // controllers outlived", identically, on a KDE session and under a
            // `Xwayland` with nothing managing it.
            let narrow_w: Rc<RefCell<Vec<gtk::glib::WeakRef<gtk::Widget>>>> = Rc::default();
            let narrow_c: Rc<RefCell<Vec<gtk::glib::WeakRef<gtk::EventController>>>> =
                Rc::default();
            {
                let a = app.clone();
                at(9400, move || {
                    if let Some(help) = help_windows(&a).first() {
                        help.set_default_size(420, 660);
                    }
                });
            }
            {
                let (a, s, nw, nc) = (
                    app.clone(),
                    seen.clone(),
                    narrow_w.clone(),
                    narrow_c.clone(),
                );
                at(9800, move || {
                    let Some(help) = help_windows(&a).first().cloned() else {
                        return;
                    };
                    let root = help.clone().upcast::<gtk::Widget>();
                    let toggle = named(&root, tobii_gtk::help::TOGGLE_NAME)
                        .downcast::<gtk::ToggleButton>()
                        .expect("the Topics toggle");
                    // RECORDED, not asserted — and that is the whole of the
                    // reason this phase reads the way it does. This closure
                    // runs inside a `glib::timeout_add_local_once`, and a
                    // panic here crosses an extern "C" trampoline and aborts
                    // the process (the note at the page walk above says the
                    // same thing about the same trampoline). Every skip in
                    // this file is decided after the main loop returns, three
                    // seconds after this point, so an assertion here does not
                    // fail the test — it kills the run that was going to
                    // explain itself. That is exactly what made the narrow
                    // layout's SKIPPED line unreachable: it could only print
                    // in the environment that aborted before it.
                    let found = Narrowed::of(&help);
                    s.borrow_mut().fold = Some(found);
                    if !found.toggle_shown {
                        return;
                    }
                    // Everything the window OWNS, not the three types the wide
                    // census picks: what leaks here is the whole content
                    // subtree, and a census that names the widgets it expects
                    // to find can only ever confirm what it already believed.
                    // `own_widgets` and not `all`, for the reason written out
                    // there: `all` also reaches GTK's shared tooltip window,
                    // which is in this tree only when the pointer is over it
                    // and is not this window's to free either way.
                    let ws = own_widgets(&root);
                    *nw.borrow_mut() = ws.iter().map(|w| w.downgrade()).collect();
                    *nc.borrow_mut() = controllers_of(&ws).iter().map(|c| c.downgrade()).collect();
                    let mut s = s.borrow_mut();
                    s.narrow_census = (nw.borrow().len(), nc.borrow().len());
                    toggle.set_active(true);
                    s.unfolded = toggle.is_active();
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(10000, move || {
                    if !s.borrow().unfolded {
                        return;
                    }
                    let Some(help) = help_windows(&a).first().cloned() else {
                        return;
                    };
                    let root = help.clone().upcast::<gtk::Widget>();
                    let toggle = named(&root, tobii_gtk::help::TOGGLE_NAME)
                        .downcast::<gtk::ToggleButton>()
                        .expect("the Topics toggle");
                    let focus_at_fold = focus_name(&help);
                    toggle.set_active(false);
                    let mut s = s.borrow_mut();
                    s.focus_at_fold = focus_at_fold;
                    s.folded = !toggle.is_active();
                });
            }
            {
                let a = app.clone();
                at(10200, move || {
                    if let Some(help) = help_windows(&a).first() {
                        help.close();
                    }
                });
            }
            {
                let (s, nw, nc) = (seen.clone(), narrow_w.clone(), narrow_c.clone());
                at(11000, move || {
                    s.borrow_mut().narrow_alive_after_close = (
                        nw.borrow().iter().filter(|w| w.upgrade().is_some()).count(),
                        nc.borrow().iter().filter(|c| c.upgrade().is_some()).count(),
                    );
                });
            }

            let a = app.clone();
            at(11200, move || a.quit());
        });
    }
    app.run_with_args::<&str>(&[]);

    let seen = seen.borrow();

    // --- three doors, one window ---
    assert_eq!(
        seen.opened,
        vec![("F1", 1), ("F1 again", 1), ("? button", 1), ("cogwheel", 1)],
        "each door must open exactly one help window, and a second F1 must raise \
         the one already open rather than stacking another"
    );
    // --- three exits ---
    assert_eq!(
        seen.closed,
        vec![("Escape", 0), ("F1", 0), ("Close button", 0)],
        "Escape, F1 and the Close button must each close the help window"
    );

    // --- the v0.3.1 rule: it must not keep itself alive ---
    //
    // The size of this census before its count, the same way round as the
    // narrow one further down. The three the loop looks for are the search
    // entry, the topic list and the topic stack, one of each; a window that
    // stopped building one of them, or a walk that stopped reaching them,
    // would leave this at nought and the count below would then read zero for
    // never having looked.
    assert_eq!(
        seen.wide_census, 3,
        "the wide-window census weak-ref'd {} of the help window's inner widgets, \
         and there are three to find — its search entry, its topic list and its \
         topic stack. The count below is taken over exactly this set, so it would \
         read zero however much the window leaked",
        seen.wide_census
    );
    assert_eq!(
        seen.inner_alive_after_close, 0,
        "{} of the help window's inner widgets outlived it. The window freeing is \
         not the whole question: two siblings that capture each other keep each \
         other alive, and the content subtree hangs off them",
        seen.inner_alive_after_close
    );
    assert!(
        !seen.alive_after_close,
        "the help window was never freed after closing — something inside it holds \
         a strong reference to it, which is the v0.3.1 bug: GTK4 emits `destroy` \
         only on finalisation, so everything it holds stays with it"
    );

    // --- and the same rule through the narrow mode's fold ---
    //
    // The census above runs at 3100 ms over a window that was only ever wide;
    // the "Topics" button it cannot reach is the thing that leaked. Unfolding
    // the list and folding it back left GTK's focus bookkeeping pointing into a
    // subtree that was no longer on screen, and the whole content subtree went
    // with it — permanently, and once per fold, in a program that sits in the
    // tray all day.
    //
    // This census does NOT need a compositor to grant anything. `relayout`
    // folds on `default-width`, which `set_default_size` sets whether or not
    // the resize lands, so the only environment that can stand it down is a
    // window manager that maximised or fullscreened the window — and that one
    // says so, in `held_wide`, rather than being guessed at from a width.
    let fold = seen.fold.expect(
        "the post-fold phase never ran: there was no help window to narrow at \
         9.4 s. That is a fault in the timeline above, not an environment — the \
         phases before it close and reopen the window, and one of them left it \
         shut",
    );
    // NOT a skip. This is the one automated cover for the folding-sidebar
    // leak, and a check that cannot be run is not a check that passed: libtest
    // captures a passing test's stderr, so the `eprintln!("SKIPPED …")` that
    // used to stand here printed nothing under the plain `-- --ignored` this
    // project's release checklist runs, and the run reported `ok`. A leak that
    // loses 80 of 87 widgets per fold, in a program that sits in the tray all
    // day, then rode out a release on a green tick that meant "the census was
    // never taken". So it fails instead, and says what to do about it.
    //
    // The cost is bounded and was weighed: CI never sees this test at all
    // (`#[ignore = "needs a display"]`, and CI has no display), and the only
    // environment this can fail in without a real regression is one where a
    // window manager maximised or fullscreened a freshly opened 420px help
    // window. That environment cannot cover the leak whatever this line does;
    // the choice is only whether it says so out loud.
    //
    // The sibling narrow-layout block near the end of this function keeps its
    // SKIPPED line, and is still reachable: it reads `at_420`, measured at
    // 6.9 s on a DIFFERENT window from the one folded here (8.2 s closes it,
    // 8.5 s reopens it), so its gate can be false while this one was true.
    // What it guards is layout behaviour, not the leak census.
    assert!(
        fold.breakpoint_was_asked(),
        "the post-fold leak census could not be taken: {}. The window was \
         allocated {}px and the Topics button was {}. Everything else in this \
         test ran, including the wide-window leak census — this failure is \
         about the ONE check on the fold leak, and it is not a claim that the \
         fold leaks. Rerun where the test's own window is not held wide for \
         it: a bare `Xwayland :NN` with no window manager does, and so does an \
         ordinary desktop session that does not maximise a 420px window.",
        fold.why_not(),
        fold.width,
        if fold.toggle_shown {
            "shown"
        } else {
            "not shown"
        },
    );
    // From here the breakpoint WAS asked, so every step is the layout's own
    // doing and is asserted as such — on a bare X server with no window
    // manager exactly as on a desktop.
    assert!(
        fold.toggle_shown,
        "`default-width` is {}px, below the {NARROW}px breakpoint, and the \
         window is neither maximised nor fullscreen — so the Topics button \
         must be on screen and it is not. `relayout` reads `default-width`, \
         which `set_default_size` sets whether or not a compositor grants \
         the resize (the allocation here is {}px), so this is the layout \
         and not the environment",
        fold.default_width, fold.width
    );
    assert!(
        !fold.sidebar_shown,
        "the Topics button is on screen and the sidebar is too, so the fold \
         below would hide a pane that was never the whole window and the \
         census would prove nothing"
    );
    assert!(
        seen.unfolded,
        "pressing \"Topics\" did not unfold the list, so the sequence the \
         census is taken across never happened"
    );
    assert!(
        seen.folded,
        "the Topics list did not fold back, so the census below is taken \
         over a window that was left unfolded — which is not the sequence \
         that leaks"
    );
    assert_eq!(
        seen.focus_at_fold, "the search box",
        "the fold must happen with the focus inside the pane it is about to \
         hide, or the census below is taken across a sequence that cannot \
         leak and passes for that reason. The focus was on {:?}. \
         `toggle.connect_toggled` hands it to the search box when the list \
         unfolds, and the search box is in the sidebar that folds away \
         again a tick later",
        seen.focus_at_fold
    );

    // The size of the census is asserted before the count. "Nothing
    // survived" and "nothing was ever weak-ref'd" are the same number, and
    // a leak census that cannot fail for the reason it names is worse than
    // no census: this file shipped one, at 3100 ms, in front of a phase
    // that starts at 6400.
    let (census_w, census_c) = seen.narrow_census;
    assert!(
        census_w >= 40 && census_c >= 40,
        "the narrow-mode census weak-ref'd {census_w} widgets and {census_c} \
         event controllers, which is too few to be the help window at all — \
         so the count below would read zero whatever the window did with them"
    );
    let (left_w, left_c) = seen.narrow_alive_after_close;
    assert_eq!(
        (left_w, left_c),
        (0, 0),
        "{left_w} of {census_w} widgets and {left_c} of {census_c} event \
         controllers outlived a help window that was narrowed, had its \
         \"Topics\" list unfolded and folded again, and was then closed. \
         What this census, and only this census, can catch is a hold taken \
         DURING the fold: the focus, left pointing into a pane that was \
         hidden, shown and hidden again without ever being given it back, \
         which is what `2bd9ab1` measured at 80 of 87 widgets and 127 of \
         143 event controllers, on a desktop and on a bare X server alike. \
         Do not start by suspecting a reference cycle in `help.rs`'s own \
         handlers — a strong capture that closes sidebar -> list -> toggle \
         -> sidebar exists from construction, needs no fold to hold \
         anything, and so is caught by the WIDE census at 3100 ms, which \
         reads 3 of 3 and stops the run long before this line. Every \
         widget counted is one this window built: GTK's own tooltip \
         window, which wanders between toplevels and is not ours to free, \
         is walked past by `own_widgets`"
    );

    // --- a window of text must not light the illuminators ---
    for r in &seen.reasons_while_open {
        assert!(
            seen.reasons_before.contains(r),
            "opening the help window took a new claim on the tracker ({r:?}): before \
             {:?}, while open {:?}",
            seen.reasons_before,
            seen.reasons_while_open
        );
    }

    // --- everything in it is reachable by keyboard alone ---
    //
    // The window builds a page for every topic the model has, and every one of
    // those pages carries that topic's whole text on a focusable label. This is
    // the widget half of the coverage contract; the model half is `help.rs`'s
    // own unit test, which is where the tooltip strings are compared. Split
    // that way on purpose: the search filters ROWS, so a test that read the
    // visible widgets could be made to pass by a query, and a test that read
    // only the model could not see the window build eight pages for nine
    // topics.
    let model = tobii_gtk::help::topics();
    assert_eq!(
        seen.pages.len(),
        model.len(),
        "the window built {} pages for {} topics: the walk asks the stack for \
         page \"t0\" to \"t{}\" by name and records what it finds, so a short \
         list here is a topic the model has and the window never drew",
        seen.pages.len(),
        model.len(),
        model.len() - 1
    );
    for (i, (name, text, focusable)) in seen.pages.iter().enumerate() {
        assert_eq!(
            text, &model[i].body,
            "page {name:?} does not carry the text of {:?}",
            model[i].title
        );
        assert!(
            focusable,
            "the body of {:?} is not focusable, so Tab cannot reach it — and a \
             window nothing can enter is not a substitute for a visible label",
            model[i].title
        );
    }
    assert!(
        seen.scroller_focusable,
        "the topic scroller must be focusable, or Page Up and Page Down have \
         nothing to scroll"
    );
    // The focus on open is the search box. That is the whole bet this shape
    // makes: a user presses F1 with a question about the control in front of
    // them, and the fastest honest answer is a box they can type its name into.
    // GtkSearchEntry delegates its focus to the GtkText inside it, hence the
    // ancestor test rather than a type comparison.
    assert!(
        seen.focus_in_search_on_open,
        "the help window must open with the search box focused, and the focus was \
         on {:?}",
        seen.focus_on_open
    );
    // And the Tab chain out of it reaches the list, the topic and Close —
    // measured with `child_focus`, which is the action a Tab press resolves to.
    // The exact path, stated as a claim rather than a snapshot, because the
    // window's own "Keyboard" topic promises it to the user.
    // The Tab walk needs the window to be ACTIVE: `child_focus` is GTK's own
    // focus walk, and in a window the compositor never brought to the front it
    // reports that it moved and then leaves the focus where it was — on the
    // first list row. Asserting there says "Tab is broken" about a session
    // where Tab is fine, and no environment available to this project
    // satisfies it: a desktop session has something else in front (this
    // program's own release build is enough), and a bare Xwayland has no
    // window manager to activate anything. So this block alone is skipped;
    // everything after it — the arrows, the search, the leak census, the
    // coverage contract, the narrow layout — does not need focus and still
    // runs. CI skips the whole test anyway, having no display at all.
    //
    // A skip and not a failure, unlike the census gate above, and the
    // difference is the point: that one is the sole cover for a real leak and
    // CAN be run here, so a run that does not take it has a hole in it. This
    // one cannot be run anywhere we have, and a failure every operator must
    // learn to ignore is worse than a line they have to go looking for.
    if !seen.active_for_tab_walk {
        eprintln!(
            "SKIPPED the Tab walk only: the window never became active (focus sat \
             on {:?}). Every other assertion in this test ran. Run it where the \
             test's own window can come to the front to cover Tab too.",
            seen.focus_on_open
        );
    } else {
        // Read only here: the chain is a claim about the walk, and on a skipped
        // run there was no walk to make a claim about.
        let chain = seen.tab_order.join(" -> ");
        let reached = |what: &str| seen.tab_order.iter().any(|x| x.contains(what));
        assert!(
            reached("GtkListBoxRow"),
            "Tab out of the search box must reach the topic list: {chain}"
        );
        assert!(
            reached("GtkLabel"),
            "Tab must go on to reach a topic's own text, which is the only thing in \
         this window that can be selected and copied: {chain}"
        );
        assert!(reached("Close"), "Tab must reach the Close button: {chain}");
        let list_at = seen
            .tab_order
            .iter()
            .position(|x| x.contains("GtkListBoxRow"));
        let label_at = seen
            .tab_order
            .iter()
            .position(|x| x.contains("GtkLabel") && !x.contains("Close"));
        assert!(
            list_at < label_at,
            "the list must come before the topic on the Tab chain — the window reads \
         left to right and so must the keyboard: {chain}"
        );
        // Every row is on GTK's own focus walk, which is what makes the claim
        // above "reachable by Tab alone" rather than "reachable if a handler
        // works". Read off the walk that starts on the first row, because
        // where the chain above enters the list is GTK's choice and not part
        // of the claim — the walk itself says why, and says what it cost.
        let titles: Vec<String> = model.iter().map(|t| t.title.to_string()).collect();
        assert_eq!(
            seen.tab_rows,
            titles,
            "GTK's own focus walk must pass through every topic row, in the list's \
         own order, so that the keyboard reaches the list even with nothing of \
         ours in the way. Walked forward from {:?}",
            titles.first()
        );
        // And the chain out of the search box has to agree with it from
        // wherever it joined: every row from its entry point to the last one,
        // in order, none skipped. A row that Tab cannot leave, or one it steps
        // over, shortens this at the back or punches a hole in it, and both
        // still fail here.
        // `describe` cuts a caption at 24 characters, so the model's own
        // titles are cut the same way to be compared with it.
        let short = |t: &String| -> String { t.chars().take(24).collect() };
        let rows_on_chain: Vec<String> = seen
            .tab_order
            .iter()
            .filter_map(|x| x.strip_prefix("GtkListBoxRow("))
            .filter_map(|x| x.strip_suffix(')'))
            .map(str::to_string)
            .collect();
        let from_entry: Option<Vec<String>> = rows_on_chain.first().and_then(|first| {
            titles
                .iter()
                .position(|t| short(t) == *first)
                .map(|k| titles[k..].iter().map(short).collect())
        });
        assert_eq!(
            from_entry.as_ref(),
            Some(&rows_on_chain),
            "Tab out of the search box entered the topic list at {:?}, and from there \
         it must pass through every row after it, in order, to the last one: {chain}",
            rows_on_chain.first()
        );

        // And the shortcut that makes that chain bearable: nine rows between the
        // search box and the topic you are already looking at is a Tab trap, so the
        // list answers Tab itself. Driven at the list's own controller, because
        // `child_focus` moves focus without dispatching a key and so can never see
        // a key handler at all.
        assert_eq!(
            seen.tab_from_list.0, "the topic pane",
            "Tab from the topic list must go to the topic pane, and it went to {:?} — \
         without this it walks the eight rows after this one first",
            seen.tab_from_list.0
        );
        assert_eq!(
            seen.tab_from_list.1, "the search box",
            "Shift+Tab from the topic list must go back to the search box, and it \
         went to {:?}",
            seen.tab_from_list.1
        );
    }

    // --- the arrow key walks every topic, and the pane follows ---
    assert_eq!(
        seen.by_arrow.len(),
        model.len(),
        "the arrow walk did not visit every topic"
    );
    for (i, (title, page)) in seen.by_arrow.iter().enumerate() {
        assert_eq!(
            title, model[i].title,
            "the {i}th Down press selected {title:?}, not {:?}",
            model[i].title
        );
        assert_eq!(
            page,
            &format!("t{i}"),
            "selecting {title:?} left the pane showing {page:?}: a topic list \
             whose selection does not change the topic is decoration"
        );
    }

    // --- the search ---
    //
    // Its own precondition before its result, for the reason the field gives:
    // "no row survived the query" and "the query never got into the box" are
    // the same empty list, and the second is a fault in this timeline rather
    // than in the window it is asking about.
    let (typed, rows_in_list) = &seen.search_precondition;
    assert_eq!(
        typed, "joystick",
        "the search phase read a list that had been filtered against {typed:?}, not \
         against the word it typed: the reading 400 ms after `set_text` caught the \
         box holding something else, and nothing below it is about the search"
    );
    assert_eq!(
        rows_in_list,
        &model.len(),
        "the search phase read a list of {rows_in_list} rows, and the model has {}: \
         it was not reading the topic list, so an empty result below says nothing \
         about the filter",
        model.len()
    );
    assert!(
        seen.search_rows
            .contains(&"Head tracking for games".to_string()),
        "searching for \"joystick\" — a word on the hub's own switch, and one that \
         is in no heading at all — must leave the topic that explains it: {:?}",
        seen.search_rows
    );
    assert!(
        seen.search_rows.len() < model.len(),
        "searching for \"joystick\" left every topic in the list, so the filter is \
         not filtering: {:?}",
        seen.search_rows
    );
    assert_eq!(
        seen.search_page, "t6",
        "the pane must follow the search to the topic that answers it, and it \
         showed {:?}",
        seen.search_page
    );

    // --- and a search that answers nothing has to SAY so ---
    assert!(
        seen.no_match_rows.is_empty(),
        "this query matches no topic, so no row may survive it: {:?}",
        seen.no_match_rows
    );
    assert_eq!(
        seen.no_match_page,
        tobii_gtk::help::NO_MATCH_PAGE,
        "a query that matches nothing must land on the page that explains that, \
         and it landed on {:?} — an empty window with no explanation is the bug \
         this page exists to prevent",
        seen.no_match_page
    );
    assert!(
        seen.no_match_text.contains("xyzzyplughquux"),
        "the no-match page must quote what was actually searched for: {:?}",
        seen.no_match_text
    );
    assert!(
        seen.no_match_text.contains("Clear the box"),
        "the no-match page must say how to get back: {:?}",
        seen.no_match_text
    );
    assert_eq!(
        seen.no_match_placeholder.as_deref(),
        Some("No topic matches."),
        "the emptied list must say why it is empty too, or the sidebar is the \
         blank half of the same bug"
    );

    // --- and clearing the box brings the topics back ---
    //
    // The case this catches: a query that matched nothing leaves the previously
    // selected row selected, so clearing the box picks that same row again and
    // `row-selected` — which fires on a CHANGE — stays silent. The window then
    // sits on "Nothing found" with all nine topics listed beside it, which is a
    // worse state than the one the no-match page exists to prevent.
    assert!(
        !seen.cleared_page.is_empty() && seen.cleared_page != tobii_gtk::help::NO_MATCH_PAGE,
        "clearing the search box left the pane on {:?}: the list came back and the \
         topic did not",
        seen.cleared_page
    );

    // --- narrow ---
    //
    // The window opens wide enough for both panes; dragged below the
    // breakpoint, the sidebar folds away and a "Topics" button takes its place,
    // because a 184px sidebar beside a 200px column of prose is a sidebar that
    // has become the window. The allocated width is asserted first: a
    // compositor that refused the resize would otherwise read as a breakpoint
    // that never fired.
    let (side_w, pane_w, side_shown, toggle_shown) = seen.at_620;
    assert!(
        side_shown && !toggle_shown,
        "at the width it opens at, the sidebar is shown and the Topics button is \
         not: sidebar {side_shown}, button {toggle_shown}"
    );
    assert!(
        pane_w > side_w,
        "at the width it opens at the topic must have more room than the list of \
         topics: sidebar {side_w}px, topic pane {pane_w}px"
    );
    assert!(
        seen.min_width > 0 && seen.min_width <= 620,
        "the window must be able to be made narrower than it opens, and its \
         minimum measured {}px",
        seen.min_width
    );
    // What this block needs is the BREAKPOINT, not the resize. `relayout` reads
    // `default-width` — which `set_default_size` sets whether or not a
    // compositor grants anything — so a refused resize leaves the allocation
    // wide and folds the sidebar anyway, and gating on the allocation stood
    // this block down on machines where it would have passed. The one thing
    // that really stops the breakpoint is a window manager holding the window
    // maximised or fullscreen, where GTK freezes `default-width`; that is what
    // is skipped, loudly, the same way as the Tab walk above.
    let n = seen.at_420;
    if !n.breakpoint_was_asked() {
        eprintln!(
            "SKIPPED the narrow layout: {}, so the sidebar fold, the Topics \
             button and Ctrl+F-while-folded were not checked. (The window was \
             allocated {}px.)",
            n.why_not(),
            n.width,
        );
    } else {
        let (side_420, toggle_420) = (n.sidebar_shown, n.toggle_shown);
        assert!(
            !side_420 && toggle_420,
            "below the breakpoint the sidebar must fold away and the Topics button \
         must appear: sidebar {side_420}, button {toggle_420}. `default-width` \
         was {}px against a {NARROW}px breakpoint and the window was neither \
         maximised nor fullscreen, so the layout was asked for this",
            n.default_width
        );
        let (unfolded, focus_after) = &seen.ctrl_f;
        assert!(
            *unfolded,
            "Ctrl+F while the sidebar is folded away must unfold it, or it focuses \
         something that is not on screen"
        );
        assert_eq!(
            focus_after, "the search box",
            "Ctrl+F must put the cursor in the search box, and it went to {focus_after:?}"
        );
        assert_eq!(
            seen.back_at_620,
            (true, false),
            "widening the window again must bring the sidebar back and take the \
         Topics button away"
        );
    }

    // --- Escape, from the one widget that eats it ---
    assert_eq!(
        seen.left_after_stop_search,
        Some(0),
        "Escape inside the search box must close the window like Escape anywhere \
         else in it. GtkSearchEntry binds Escape to its own `stop-search` and \
         consumes the key, so without a handler for it Escape would silently \
         stop working for the widget the window opens focused"
    );

    // --- and it reopens where it was left ---
    assert_eq!(
        seen.resume.0, "t5",
        "the test meant to leave the window on the sixth topic"
    );
    assert_eq!(
        seen.resume.1, seen.resume.0,
        "reopening the help window must resume on the topic it was closed on: a \
         user who reads a paragraph, tries the control and presses F1 again is \
         asking about the same control"
    );

    // --- and the topic it resumed on does not outrank the question ---
    //
    // The other half of the rule above. Resuming is right; letting the resumed
    // topic win the FIRST word typed into an empty box is not, because nobody
    // chose that topic for this question and nobody is reading it. It is worst
    // exactly where it is most likely: the "Keyboard" topic is the one that
    // says this window has a search, and it advertises "joystick", "strength",
    // "cogwheel" and "pitch" as words to try — so it quotes all four, and the
    // rule that keeps a still-matching topic kept it for every one of them.
    let (resumed_on, after_first_word) = (&seen.first_query.0, &seen.first_query.1);
    let last = format!("t{}", model.len() - 1);
    assert_eq!(
        resumed_on, &last,
        "this phase meant to reopen on the Keyboard topic and reopened on \
         {resumed_on:?}, so the query below was typed into the wrong window"
    );
    assert_eq!(
        after_first_word, "t6",
        "typing \"joystick\" — one of the four words the Keyboard topic itself \
         advertises — left the pane on {after_first_word:?}, the topic that was \
         already showing. The list narrows correctly and the pane does not \
         follow, which is the one state the search's pick exists to prevent"
    );

    // --- and the contract the shortened cards rest on ---
    let help = help_text();
    assert_eq!(seen.rack.len(), CARDS.len(), "the rack was never sampled");
    let mut tooltips_seen = 0;
    for card in &seen.rack {
        // A card that gave its description up must still say what it said
        // somewhere a pointer can find, or the sentence has left the hub
        // altogether and only the help window has it.
        if card.desc.is_none() {
            assert!(
                !card.tooltips.is_empty(),
                "{:?} has no description and no tooltip either: its guidance is \
                 reachable only by opening the help window",
                card.title
            );
        }
        for tip in &card.tooltips {
            tooltips_seen += 1;
            // Paragraph by paragraph: a composed tooltip — the Recentre row's
            // is "what it is" plus "why it is grey right now" — is two
            // independent facts, and the help window carries them in two
            // different places.
            for para in tip.split("\n\n") {
                assert!(
                    help.contains(para),
                    "a fact that can only be read by hovering is missing from the \
                     help window, so no keyboard or touch user can ever reach it \
                     ({:?}): {para:?}",
                    card.title
                );
            }
        }
    }
    assert!(
        tooltips_seen >= 5,
        "only {tooltips_seen} tooltips were found in the whole rack, so this proved \
         almost nothing — the walk is probably not finding the cards"
    );

    // --- hiding the hub takes the help window with it ---
    assert_eq!(
        seen.open_after_hub_hidden,
        Some(0),
        "the hub hid itself (which is what closing to the tray does) and left the \
         help window behind: it is transient for a window that is no longer on \
         screen, and where it goes then is the compositor's guess"
    );

    // --- no card description may wrap at the width the window opens at ---
    //
    // This is the rake this change stepped on once in design: the obvious width
    // to check a sentence at is the 380px a card is usually measured at, and
    // the window does not open at 380. It opens at the three-column layout's
    // minimum, where the first column gets exactly `COLUMN_WIDTH` — so a
    // sentence that fits at 380 and wraps at 360 silently costs a line of card
    // in the only layout most people will ever see.
    //
    // Stated as "no taller at the minimum than with room to spare" rather than
    // as a pixel count, because a pixel count is this machine's font at text
    // scale 1.0 and a reviewer elsewhere would get a different one. The shape
    // of the claim travels; the numbers do not.
    assert_eq!(
        seen.widths.len(),
        CARDS.len(),
        "the cards were never measured"
    );
    for (title, desc, at_min, roomy) in &seen.widths {
        assert_eq!(
            at_min,
            roomy,
            "{title:?} is {}px taller at the width the window opens at than it is \
             with room to spare, so its description wraps a line there that the \
             usual 380px measurement never shows: {desc:?}",
            at_min - roomy
        );
    }

    // The two cards that gave their sentence to a tooltip and the help window,
    // stated here so that putting a description back is a deliberate act rather
    // than an accident — and so that dropping one from another card is too.
    let without: Vec<&str> = seen
        .widths
        .iter()
        .filter(|(_, d, _, _)| d.is_none())
        .map(|(t, _, _, _)| *t)
        .collect();
    assert_eq!(
        without,
        vec!["Select eyes to detect", "Preview my gaze"],
        "exactly these two cards carry no visible description: the eyes card \
         because its sentence was advice rather than operation, and the preview \
         card because its sentence was false"
    );
    assert_eq!(
        seen.selected_on_open, 0,
        "the window must not open with text selected: a selectable label selects \
         all of itself when focus lands on it, and focus reaches the first topic \
         on its way to the scroller, so {} characters came up highlighted",
        seen.selected_on_open
    );
}
