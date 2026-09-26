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

fn walk(w: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
    out.push(w.clone());
    let mut c = w.first_child();
    while let Some(ch) = c {
        walk(&ch, out);
        c = ch.next_sibling();
    }
}

fn all(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut v = Vec::new();
    walk(root, &mut v);
    v
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

/// The body label of one topic page — the selectable one Tab has to reach.
fn body_label(page: &gtk::Widget) -> gtk::Label {
    all(page)
        .into_iter()
        .find_map(|w| {
            let l = w.downcast::<gtk::Label>().ok()?;
            l.has_css_class("section-desc").then_some(l)
        })
        .expect("every topic page has a body label")
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
    all(card).into_iter().find_map(|w| {
        let l = w.downcast::<gtk::Label>().ok()?;
        l.has_css_class("section-desc")
            .then(|| l.text().to_string())
    })
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
        .map(|t| format!("{}\n{}", t.title, t.body))
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
    /// Whether that focus is inside the search box.
    focus_in_search_on_open: bool,
    /// The Tab chain from the state the window opens in, focus by focus, as
    /// GTK's own focus walk produces it.
    tab_order: Vec<String>,
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
    /// Typing a word that is in no topic at all.
    no_match_rows: Vec<String>,
    no_match_page: String,
    no_match_text: String,
    no_match_placeholder: Option<String>,
    /// The page showing after the search box is cleared again.
    cleared_page: String,
    /// Narrowed below the breakpoint: sidebar shown, "Topics" button shown,
    /// and the width it was actually allocated (a resize a compositor refuses
    /// would otherwise look like a breakpoint that never fired).
    at_420: (bool, bool, i32),
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

            let at = |ms: u64, f: Box<dyn FnOnce()>| {
                gtk::glib::timeout_add_local_once(Duration::from_millis(ms), f)
            };

            // --- door 1: F1 on the hub. Exit 1: Escape. ---
            {
                let (h, d, s) = (hub.clone(), demand.clone(), seen.clone());
                at(
                    600,
                    Box::new(move || {
                        s.borrow_mut().reasons_before = d.reasons();
                        assert!(
                            press(h.upcast_ref(), gtk::gdk::Key::F1),
                            "F1 was not handled by the hub window"
                        );
                    }),
                );
            }
            {
                let (a, d, s) = (app.clone(), demand.clone(), seen.clone());
                at(
                    1000,
                    Box::new(move || {
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
                        for (i, t) in tobii_gtk::help::topics().iter().enumerate() {
                            let name = format!("t{i}");
                            let page = stack.child_by_name(&name).unwrap_or_else(|| {
                                panic!("the window built no page {name:?} for {:?}", t.title)
                            });
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
                        // GtkSearchEntry hands its focus to the GtkText inside
                        // it, so "is it the search box" is an ancestor question
                        // and not a type comparison.
                        s.focus_in_search_on_open = focus.as_ref().is_some_and(|w| {
                            w.widget_name() == tobii_gtk::help::SEARCH_NAME
                                || w.ancestor(gtk::SearchEntry::static_type())
                                    .is_some_and(|a| {
                                        a.widget_name() == tobii_gtk::help::SEARCH_NAME
                                    })
                        });
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
                    }),
                );
            }
            // A second F1 must raise the window it already opened, not open a
            // second one.
            {
                let (h, a, s) = (hub.clone(), app.clone(), seen.clone());
                at(
                    1300,
                    Box::new(move || {
                        press(h.upcast_ref(), gtk::gdk::Key::F1);
                        s.borrow_mut()
                            .opened
                            .push(("F1 again", help_windows(&a).len()));
                    }),
                );
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(
                    1600,
                    Box::new(move || {
                        let help = help_windows(&a).remove(0);
                        assert!(
                            press(&help, gtk::gdk::Key::Escape),
                            "Escape was not handled by the help window"
                        );
                        s.borrow_mut().closed.push(("Escape", usize::MAX));
                    }),
                );
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(
                    1900,
                    Box::new(move || {
                        let n = help_windows(&a).len();
                        s.borrow_mut().closed.last_mut().unwrap().1 = n;
                    }),
                );
            }

            // --- door 2: the "?" button in the header. Exit 2: F1 again. ---
            {
                let (h, a, s) = (hub.clone(), app.clone(), seen.clone());
                at(
                    2200,
                    Box::new(move || {
                        button_with_tooltip(h.upcast_ref(), "Help (F1)").emit_clicked();
                        s.borrow_mut()
                            .opened
                            .push(("? button", help_windows(&a).len()));
                    }),
                );
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(
                    2500,
                    Box::new(move || {
                        let help = help_windows(&a).remove(0);
                        assert!(
                            press(&help, gtk::gdk::Key::F1),
                            "F1 was not handled by the help window"
                        );
                        s.borrow_mut().closed.push(("F1", help_windows(&a).len()));
                    }),
                );
            }

            // --- door 3: the cogwheel's Help row. Exit 3: the Close button. ---
            {
                let (h, a, s) = (hub.clone(), app.clone(), seen.clone());
                at(
                    2800,
                    Box::new(move || {
                        button_labelled(h.upcast_ref(), "Help").emit_clicked();
                        s.borrow_mut()
                            .opened
                            .push(("cogwheel", help_windows(&a).len()));
                    }),
                );
            }
            // The weak reference is taken with the window still open, so what
            // is checked after the close is whether anything still holds it.
            let weak: Rc<RefCell<Option<gtk::glib::WeakRef<gtk::Window>>>> = Rc::default();
            let inner: Rc<RefCell<Vec<gtk::glib::WeakRef<gtk::Widget>>>> = Rc::default();
            {
                let (a, s, weak, inner) = (app.clone(), seen.clone(), weak.clone(), inner.clone());
                at(
                    3100,
                    Box::new(move || {
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
                    }),
                );
            }
            {
                let (a, s, weak, inner) = (app.clone(), seen.clone(), weak.clone(), inner.clone());
                at(
                    3900,
                    Box::new(move || {
                        let mut s = s.borrow_mut();
                        s.closed.last_mut().unwrap().1 = help_windows(&a).len();
                        s.alive_after_close =
                            weak.borrow().as_ref().and_then(|w| w.upgrade()).is_some();
                        s.inner_alive_after_close = inner
                            .borrow()
                            .iter()
                            .filter(|w| w.upgrade().is_some())
                            .count();
                    }),
                );
            }

            // The cards as the hub really built them, sampled late so the games
            // row's own refresh (driven by the hub's 33 ms tick) has set the
            // dynamic tooltips.
            {
                let (h, s) = (hub.clone(), seen.clone());
                at(
                    4100,
                    Box::new(move || {
                        s.borrow_mut().rack = rack(h.upcast_ref());
                    }),
                );
            }
            // How tall each card is at the narrowest its own column is ever
            // allocated, against how tall it is with room to spare. See the
            // assertions for why that is the question, and not "how tall at
            // 380".
            {
                let (h, s) = (hub.clone(), seen.clone());
                at(
                    4200,
                    Box::new(move || {
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
                            s.borrow_mut().widths.push((
                                title,
                                description_of(&card),
                                at_min,
                                roomy,
                            ));
                        }
                    }),
                );
            }
            // Hiding the hub — what closing to the tray does — must take the
            // help window with it. `destroy_with_parent` does not cover a
            // parent that is merely hidden, and a transient window whose parent
            // is hidden is left to the compositor.
            {
                let (h, s) = (hub.clone(), seen.clone());
                at(
                    4400,
                    Box::new(move || {
                        button_with_tooltip(h.upcast_ref(), "Help (F1)").emit_clicked();
                        assert_eq!(
                            help_windows(&h.application().unwrap()).len(),
                            1,
                            "the help window did not reopen for the hide test"
                        );
                        h.set_visible(false);
                        s.borrow_mut().open_after_hub_hidden =
                            Some(help_windows(&h.application().unwrap()).len());
                    }),
                );
            }
            {
                let h = hub.clone();
                at(4600, Box::new(move || h.present()));
            }

            // --- the sidebar, the search, and what the window does when it is
            // --- made too narrow to hold both
            //
            // All of it on this one timeline and not in a second `#[test]`:
            // cargo runs tests in threads, and two GTK main loops in one
            // process is not a thing that works.
            {
                let h = hub.clone();
                at(
                    4800,
                    Box::new(move || {
                        assert!(
                            press(h.upcast_ref(), gtk::gdk::Key::F1),
                            "F1 did not reopen the help window for the sidebar phase"
                        );
                    }),
                );
            }
            // At the width it opens at: who got how much, and what the whole
            // window's minimum is. Then the topic list walked with the arrow
            // key — GTK's own `move-cursor`, which is the action Down resolves
            // to — one press per topic, checking the pane follows.
            {
                let (a, s) = (app.clone(), seen.clone());
                at(
                    5000,
                    Box::new(move || {
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
                        let focused = |root: &gtk::Widget| {
                            root.root()
                                .and_downcast::<gtk::Window>()
                                .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w))
                                .map(|w| {
                                    if w.widget_name() == tobii_gtk::help::BODY_SCROLL_NAME {
                                        "the topic pane".to_string()
                                    } else if w.widget_name() == tobii_gtk::help::SEARCH_NAME
                                        || w.ancestor(gtk::SearchEntry::static_type()).is_some_and(
                                            |x| x.widget_name() == tobii_gtk::help::SEARCH_NAME,
                                        )
                                    {
                                        "the search box".to_string()
                                    } else {
                                        describe(&w)
                                    }
                                })
                                .unwrap_or_default()
                        };
                        if let Some(row) = list.row_at_index(3) {
                            row.grab_focus();
                        }
                        press_with(&list, gtk::gdk::Key::Tab, gtk::gdk::ModifierType::empty());
                        s.borrow_mut().tab_from_list.0 = focused(&root);
                        if let Some(row) = list.row_at_index(3) {
                            row.grab_focus();
                        }
                        press_with(
                            &list,
                            gtk::gdk::Key::Tab,
                            gtk::gdk::ModifierType::SHIFT_MASK,
                        );
                        s.borrow_mut().tab_from_list.1 = focused(&root);
                        // Put the selection back where the arrow walk left it,
                        // so the phases after this start from a known topic.
                        if let Some(row) = list.row_at_index(0) {
                            list.select_row(Some(&row));
                        }
                    }),
                );
            }
            // One word into the search box. `GtkSearchEntry` debounces
            // `search-changed`, so every reading below is taken a clear 400 ms
            // after the typing that causes it.
            {
                let a = app.clone();
                at(
                    5200,
                    Box::new(move || {
                        let help = help_windows(&a).remove(0);
                        the_search(&help.upcast()).set_text("joystick");
                    }),
                );
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(
                    5600,
                    Box::new(move || {
                        let root = help_windows(&a).remove(0).upcast::<gtk::Widget>();
                        let mut s = s.borrow_mut();
                        s.search_rows = visible_rows(&the_list(&root));
                        s.search_page = showing(&root);
                    }),
                );
            }
            // And a word that is in no topic at all: an empty window with no
            // explanation is the bug this page exists to prevent.
            {
                let a = app.clone();
                at(
                    5800,
                    Box::new(move || {
                        let help = help_windows(&a).remove(0);
                        the_search(&help.upcast()).set_text("xyzzyplughquux");
                    }),
                );
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(
                    6200,
                    Box::new(move || {
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
                    }),
                );
            }
            // Now make it too narrow to hold a sidebar and a topic.
            {
                let a = app.clone();
                at(
                    6400,
                    Box::new(move || {
                        let help = help_windows(&a).remove(0);
                        the_search(&help.clone().upcast()).set_text("");
                        help.set_default_size(420, 660);
                    }),
                );
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(
                    6900,
                    Box::new(move || {
                        let help = help_windows(&a).remove(0);
                        let root = help.clone().upcast::<gtk::Widget>();
                        {
                            let mut s = s.borrow_mut();
                            s.cleared_page = showing(&root);
                            s.at_420 = (
                                named(&root, tobii_gtk::help::SIDEBAR_NAME).is_visible(),
                                named(&root, tobii_gtk::help::TOGGLE_NAME).is_visible(),
                                help.width(),
                            );
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
                    }),
                );
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(
                    7100,
                    Box::new(move || {
                        let help = help_windows(&a).remove(0);
                        let root = help.clone().upcast::<gtk::Widget>();
                        let focus = gtk::prelude::GtkWindowExt::focus(&help);
                        s.borrow_mut().ctrl_f = (
                            named(&root, tobii_gtk::help::SIDEBAR_NAME).is_visible(),
                            focus
                                .as_ref()
                                .map(|w| {
                                    if w.widget_name() == tobii_gtk::help::SEARCH_NAME
                                        || w.ancestor(gtk::SearchEntry::static_type()).is_some_and(
                                            |x| x.widget_name() == tobii_gtk::help::SEARCH_NAME,
                                        )
                                    {
                                        "the search box".to_string()
                                    } else {
                                        describe(w)
                                    }
                                })
                                .unwrap_or_default(),
                        );
                        help.set_default_size(620, 660);
                    }),
                );
            }
            // Wide again, then Escape from inside the search box — which
            // GtkSearchEntry turns into `stop-search` and swallows, so it is
            // the one place Escape could silently stop closing the window.
            {
                let (a, s) = (app.clone(), seen.clone());
                at(
                    7500,
                    Box::new(move || {
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
                    }),
                );
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(
                    7700,
                    Box::new(move || {
                        s.borrow_mut().left_after_stop_search = Some(help_windows(&a).len());
                    }),
                );
            }
            {
                let h = hub.clone();
                at(
                    7900,
                    Box::new(move || {
                        press(h.upcast_ref(), gtk::gdk::Key::F1);
                    }),
                );
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(
                    8100,
                    Box::new(move || {
                        let Some(help) = help_windows(&a).first().cloned() else {
                            return;
                        };
                        s.borrow_mut().resume.1 = showing(&help.upcast());
                    }),
                );
            }

            let a = app.clone();
            at(8400, Box::new(move || a.quit()));
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
        "the window built {} pages for {} topics",
        seen.pages.len(),
        model.len()
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
    // Every row is on that chain, which is GTK's own behaviour for a
    // `GtkListBox` of focusable rows and is what makes the claim above
    // "reachable by Tab alone" rather than "reachable if a handler works".
    let rows_on_chain = seen
        .tab_order
        .iter()
        .filter(|x| x.contains("GtkListBoxRow"))
        .count();
    assert_eq!(
        rows_on_chain,
        model.len(),
        "GTK's own focus walk must pass through every topic row, so that the \
         keyboard reaches the list even with nothing of ours in the way: {chain}"
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
    let (side_420, toggle_420, w_420) = seen.at_420;
    assert!(
        w_420 < 520,
        "the resize to 420 was not granted (the window is {w_420}px wide), so \
         nothing below this proves anything about narrow windows"
    );
    assert!(
        !side_420 && toggle_420,
        "below the breakpoint the sidebar must fold away and the Topics button \
         must appear: sidebar {side_420}, button {toggle_420}"
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
