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

/// Press a key at `win`'s own key controllers.
///
/// The signal is emitted on the controller rather than synthesised as a device
/// event: a test cannot make a compositor deliver a key press, and what is
/// under test is the handler and its wiring, not GTK's own dispatch.
fn press(win: &gtk::Window, key: gtk::gdk::Key) -> bool {
    let mut handled = false;
    let controllers = win.observe_controllers();
    for i in 0..controllers.n_items() {
        let Some(keys) = controllers
            .item(i)
            .and_downcast::<gtk::EventControllerKey>()
        else {
            continue;
        };
        handled |= keys.emit_by_name::<bool>(
            "key-pressed",
            &[&key.into_glib(), &0u32, &gtk::gdk::ModifierType::empty()],
        );
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
    /// Focusable topic bodies and the focused widget the window opens with.
    focusable_bodies: usize,
    topic_bodies: usize,
    scroller_focusable: bool,
    focus_on_open: Option<String>,
    /// Whether the last help window was still alive 800 ms after closing.
    alive_after_close: bool,
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
                        // Keyboard reachability, measured on the real window: a
                        // selectable GtkLabel is focusable, which is what lets
                        // Tab walk the topics at all.
                        let mut s = s.borrow_mut();
                        for w in all(help.upcast_ref()) {
                            if let Some(l) = w.downcast_ref::<gtk::Label>() {
                                if l.has_css_class("section-desc") {
                                    s.topic_bodies += 1;
                                    if l.is_focusable() {
                                        s.focusable_bodies += 1;
                                    }
                                }
                            }
                            if w.downcast_ref::<gtk::ScrolledWindow>().is_some() {
                                s.scroller_focusable = w.is_focusable();
                            }
                        }
                        s.focus_on_open =
                            gtk::prelude::GtkWindowExt::focus(help).map(|w| w.type_().to_string());
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
            {
                let (a, s, weak) = (app.clone(), seen.clone(), weak.clone());
                at(
                    3100,
                    Box::new(move || {
                        let help = help_windows(&a).remove(0);
                        *weak.borrow_mut() = Some(help.downgrade());
                        button_labelled(help.upcast_ref(), "Close").emit_clicked();
                        s.borrow_mut().closed.push(("Close button", usize::MAX));
                    }),
                );
            }
            {
                let (a, s, weak) = (app.clone(), seen.clone(), weak.clone());
                at(
                    3900,
                    Box::new(move || {
                        let mut s = s.borrow_mut();
                        s.closed.last_mut().unwrap().1 = help_windows(&a).len();
                        s.alive_after_close =
                            weak.borrow().as_ref().and_then(|w| w.upgrade()).is_some();
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
            let a = app.clone();
            at(4800, Box::new(move || a.quit()));
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
    assert_eq!(
        seen.topic_bodies,
        seen.focusable_bodies,
        "{} of {} topic bodies are not focusable, so Tab cannot reach them — and a \
         window nothing can enter is not a substitute for a visible label",
        seen.topic_bodies - seen.focusable_bodies,
        seen.topic_bodies
    );
    assert_eq!(
        seen.topic_bodies,
        tobii_gtk::help::topics().len(),
        "every topic must have a body label in the window"
    );
    assert!(
        seen.scroller_focusable,
        "the scroller must be focusable, or `grab_focus` has nothing to give the \
         focus to"
    );
    // And it must be what has the focus when the window opens, so Page Up/Down
    // and the arrows scroll straight away. Without the `grab_focus`, the first
    // Tab lands in the first topic instead and the keys do nothing until then.
    assert_eq!(
        seen.focus_on_open.as_deref(),
        Some("GtkScrolledWindow"),
        "the help window must open with the scroller focused"
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
}
