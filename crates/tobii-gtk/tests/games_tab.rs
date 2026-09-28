//! The hub's two tabs: the container, the keys that walk it, and the Games
//! tab's lifetime.
//!
//! Needs a display, so it is ignored by default (CI has none). Run it with:
//!
//!     cargo test -p tobii-gtk --test games_tab -- --ignored
//!
//! and run it in a **nested** compositor rather than on the session display:
//!
//!     kwin_wayland --virtual --width 1600 --height 1200 --socket wl-gs &
//!     WAYLAND_DISPLAY=wl-gs GDK_BACKEND=wayland cargo test -p tobii-gtk \
//!         --test games_tab -- --ignored
//!
//! No tracker is needed. Nothing the user has saved is touched: no button that
//! writes anything is pressed, the installer is never started, and after this
//! change there is no control anywhere on the Games tab that writes
//! `games.toml` at all.
//!
//! # What this file is for
//!
//! **The lifetime, first and most.** This tree used to belong to a modal
//! window, and that window was closed every time somebody pressed Close — so a
//! reference cycle between two of its own siblings showed up within seconds of
//! anybody using it. The same tree now lives inside the hub, and the hub is
//! closed exactly once, at quit. That is the shape of the v0.3.1 bug, where a
//! flow's window stayed alive for the life of the process and the tracker never
//! went dark again: GTK4 emits `destroy` only on finalisation, so nothing about
//! a leaked subtree is visible until somebody weak-references it and looks.
//!
//! So this takes a census of the whole Games subtree, quits the program the way
//! the cogwheel's Quit does, and reads the weak references afterwards — once
//! with nobody else holding anything, and once with the test itself holding the
//! subtree's root across the quit and letting go afterwards, because a freeing
//! that only happens when nobody else ever took a reference is not a freeing
//! anybody can rely on.
//!
//! **The claim on the tracker.** The Games tab must not light the illuminators.
//! The modal kept that promise by taking the focus away from the hub; a tab
//! takes no focus, so the hub's own claim is now conditional on the Tracker tab
//! being the visible one. The condition itself is asserted headlessly, over
//! `hub_wants_tracker`, in `lib.rs` — a window that never gets focus reports
//! `is_active() == false` whatever tab it shows, so a check here that only
//! watched the reason list would pass with the condition deleted. What is
//! asserted here is the part a nested compositor can honestly observe: that
//! neither tab ADDS a claim.
//!
//! **The container.** That the stack measures its visible child rather than the
//! larger of the two, that a tab switch does not resize the window, that both
//! Ctrl+Page keys work, and that the switcher is reachable by Tab. Plus the
//! geometry, printed rather than asserted: the absolute numbers are this
//! machine's font metrics, and where they are recorded is `lib.rs`.

use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// `root` and its descendants, descending into a child only when `descend`
/// accepts it. `root` itself is always pushed.
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

fn all(root: &gtk::Widget) -> Vec<gtk::Widget> {
    walk(root, &|_| true)
}

/// The widgets a subtree actually OWNS: everything, minus every subtree that
/// has a surface of its own.
///
/// The exclusion is `tests/help_window.rs`'s and the argument is the same one:
/// a `GtkTooltipWindow` is a separate surface GTK creates once per display and
/// parents into whichever toplevel is showing a tooltip at that moment, so
/// whether it is inside this tree when a census is taken is decided by where
/// the pointer is sitting, and whether it is alive afterwards is decided by
/// GTK, which still owns it. The Games tab puts a tooltip on its Details
/// button, so it can borrow that surface.
///
/// It is the only thing dropped. Everything this tab builds is a box, a label,
/// a button, a search entry, a scroller or a list, and not one of those owns a
/// surface. Re-read this if a `Popover` or a `Window` is ever added to
/// `game_setup.rs`, because then the exclusion really could start eating
/// evidence.
fn own_widgets(root: &gtk::Widget) -> Vec<gtk::Widget> {
    walk(root, &|ch| !ch.is::<gtk::Native>())
}

/// Every event controller on every widget of `ws` — what a leaked handler is
/// *made of*, and the more sensitive of the two counts.
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

fn hub_stack(hub: &gtk::Widget) -> Option<gtk::Stack> {
    all(hub)
        .into_iter()
        .find(|w| w.widget_name() == tobii_gtk::HUB_STACK_NAME)
        .and_then(|w| w.downcast::<gtk::Stack>().ok())
}

/// Press `key` on the window's own controllers, the way the window sees it.
fn press(win: &gtk::Window, key: gtk::gdk::Key, mods: gtk::gdk::ModifierType) -> bool {
    let mut handled = false;
    let controllers = win.observe_controllers();
    for i in 0..controllers.n_items() {
        let Some(keys) = controllers
            .item(i)
            .and_downcast::<gtk::EventControllerKey>()
        else {
            continue;
        };
        handled |= keys.emit_by_name::<bool>("key-pressed", &[&key.into_glib(), &0u32, &mods]);
    }
    handled
}

/// Run `f` once, `ms` into the timeline.
fn at<F: FnOnce() + 'static>(ms: u64, f: F) {
    gtk::glib::timeout_add_local_once(Duration::from_millis(ms), f);
}

/// Is the focus inside `w`?
fn focus_is_under(win: &gtk::Window, w: &gtk::Widget) -> bool {
    // `GtkRoot::get_focus`, which is where GTK4 keeps the focus for a toplevel.
    let Some(mut f) = gtk::prelude::RootExt::focus(win) else {
        return false;
    };
    loop {
        if f == *w {
            return true;
        }
        match f.parent() {
            Some(p) => f = p,
            None => return false,
        }
    }
}

/// A census of a subtree, taken before the quit and read after it.
struct Census {
    widgets: Vec<gtk::glib::WeakRef<gtk::Widget>>,
    controllers: Vec<gtk::glib::WeakRef<gtk::EventController>>,
}

fn census(root: &gtk::Widget) -> Census {
    let ws = own_widgets(root);
    let cs = controllers_of(&ws);
    Census {
        widgets: ws.iter().map(|w| w.downgrade()).collect(),
        controllers: cs.iter().map(|c| c.downgrade()).collect(),
    }
}

/// What one whole run of the program looked like.
#[derive(Debug, Default)]
struct Seen {
    /// Whether the test itself held the Games subtree across the quit.
    held: bool,
    claims_before: Vec<&'static str>,
    claims_on_tracker: Vec<&'static str>,
    claims_on_games: Vec<&'static str>,
    /// The visible tab, at each step of the walk.
    tabs: Vec<(&'static str, Option<String>)>,
    /// Whether each key press was handled by the window at all.
    handled: Vec<(&'static str, bool)>,
    /// The window's default size, before and after a tab switch.
    size_on_tracker: (i32, i32),
    size_on_games: (i32, i32),
    /// Whether the stack measures its visible child rather than the largest.
    homogeneous: (bool, bool),
    /// What the stack and each of its two children measure.
    stack_height: i32,
    tracker_height: i32,
    games_height: i32,
    games_min_width: i32,
    hub_natural_width: i32,
    /// How many Tab presses reached the switcher, if any did.
    tabs_to_switcher: Option<u32>,
    /// What the switcher's buttons are captioned, in order.
    switcher_labels: Vec<String>,
    /// The header's children, by type, in order.
    header_order: Vec<String>,
    widgets: usize,
    widgets_alive_after_quit: usize,
    controllers: usize,
    controllers_alive_after_quit: usize,
    /// A step that could not run at all.
    ///
    /// Collected rather than panicked: a panic inside a GLib callback is a
    /// non-unwinding abort, which takes the process down with a stack trace and
    /// no test result at all.
    troubles: Vec<String>,
}

/// Build the hub, walk both tabs, quit through the hub's own teardown, and
/// report what survived.
///
/// `hold` keeps the Games subtree's root alive across the quit; the caller lets
/// go before reading the census.
fn run(app_id: &str, hold: bool) -> Seen {
    let app = gtk::Application::builder()
        .application_id(app_id)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let seen: Rc<RefCell<Seen>> = Rc::default();
    seen.borrow_mut().held = hold;
    // At test scope, not inside the activate handler: a clone that lived only
    // in a one-shot callback would be dropped with it, and the "held" subtree
    // would be freed before anything checked it.
    let keep: Rc<RefCell<Vec<gtk::Widget>>> = Rc::default();
    let pending: Rc<RefCell<Option<Census>>> = Rc::default();

    {
        let (seen, keep, pending) = (seen.clone(), keep.clone(), pending.clone());
        app.connect_activate(move |app| {
            tobii_gtk::load_css();
            // Pinned, because the geometry this prints is compared against a
            // number recorded in `lib.rs` at text scale 1.0, and `load_css`
            // above applies whatever scale this machine's user has chosen.
            tobii_gtk::apply_text_scale(1.0);
            // The same action the cogwheel's Quit and `tobii uninstall` use, so
            // what is torn down here is what is torn down in the field. A bare
            // `app.quit()` ends the loop without running the hub's close
            // handler at all, and the close handler is where the Games tab is
            // told to stop watching.
            tobii_gtk::install_quit_action(app);
            let session = tobii_gtk::device::spawn();
            let demand = session.2.clone();
            let hub = tobii_gtk::build_hub(app, session).expect("hub");
            hub.present();
            seen.borrow_mut().claims_before = demand.reasons();

            // ---- on the Tracker tab: the measurements, and the census.
            {
                let (s, d, h, p, k) = (
                    seen.clone(),
                    demand.clone(),
                    hub.clone(),
                    pending.clone(),
                    keep.clone(),
                );
                at(900, move || {
                    let mut s = s.borrow_mut();
                    s.claims_on_tracker = d.reasons();
                    s.hub_natural_width = h.measure(gtk::Orientation::Horizontal, -1).1;
                    s.size_on_tracker = (h.default_width(), h.default_height());
                    let Some(stack) = hub_stack(h.upcast_ref()) else {
                        s.troubles.push("the hub has no tab stack".to_string());
                        return;
                    };
                    s.tabs
                        .push(("at open", stack.visible_child_name().map(Into::into)));
                    let w = s.size_on_tracker.0;
                    s.homogeneous = (stack.is_vhomogeneous(), stack.is_hhomogeneous());
                    s.stack_height = stack.measure(gtk::Orientation::Vertical, w).1;
                    let (Some(tracker), Some(games)) = (
                        stack.child_by_name(tobii_gtk::TAB_TRACKER),
                        stack.child_by_name(tobii_gtk::TAB_GAMES),
                    ) else {
                        s.troubles.push("the stack is missing a tab".to_string());
                        return;
                    };
                    s.tracker_height = tracker.measure(gtk::Orientation::Vertical, w).1;
                    s.games_height = games.measure(gtk::Orientation::Vertical, w).1;
                    s.games_min_width = games.measure(gtk::Orientation::Horizontal, -1).0;
                    *p.borrow_mut() = Some(census(&games));
                    if hold {
                        k.borrow_mut().push(games);
                    }

                    // Where the switcher sits, and what it says. The captions
                    // are the only words on the hub that name the two halves of
                    // it, and where it sits is the difference between a tab bar
                    // and two buttons lost in a row of icons.
                    let Some(switcher) = all(h.upcast_ref())
                        .into_iter()
                        .find(|w| w.is::<gtk::StackSwitcher>())
                    else {
                        s.troubles.push("the header has no switcher".to_string());
                        return;
                    };
                    s.switcher_labels = all(&switcher)
                        .into_iter()
                        .filter_map(|w| w.downcast::<gtk::Label>().ok())
                        .map(|l| l.text().to_string())
                        .collect();
                    if let Some(header) = switcher.parent() {
                        let mut c = header.first_child();
                        while let Some(ch) = c {
                            s.header_order.push(ch.type_().to_string());
                            c = ch.next_sibling();
                        }
                    }

                    // Tab, from the top, until the focus is inside the
                    // switcher. GTK's own `child_focus`, which is what the Tab
                    // key resolves to.
                    let mut steps = 0;
                    while steps < 40 && s.tabs_to_switcher.is_none() {
                        h.child_focus(gtk::DirectionType::TabForward);
                        steps += 1;
                        let reached = all(h.upcast_ref())
                            .into_iter()
                            .filter(|w| w.is::<gtk::StackSwitcher>())
                            .any(|w| focus_is_under(h.upcast_ref(), &w));
                        if reached {
                            s.tabs_to_switcher = Some(steps);
                        }
                    }
                });
            }

            // ---- Ctrl+Page_Down to the Games tab.
            {
                let (s, h) = (seen.clone(), hub.clone());
                at(1200, move || {
                    let handled = press(
                        h.upcast_ref(),
                        gtk::gdk::Key::Page_Down,
                        gtk::gdk::ModifierType::CONTROL_MASK,
                    );
                    let mut s = s.borrow_mut();
                    s.handled.push(("ctrl+page_down", handled));
                    s.tabs.push((
                        "after ctrl+page_down",
                        hub_stack(h.upcast_ref())
                            .and_then(|t| t.visible_child_name().map(Into::into)),
                    ));
                });
            }

            // ---- on the Games tab: the claims, and the size AFTER a re-fit.
            //
            // The re-fit has to be provoked, or the size assertion below cannot
            // fail: nothing re-fits on a tab switch by design, so a window that
            // did not move proves only that nobody asked it to. `apply_text_scale`
            // is the shortest way in — it is the cogwheel's own path into the
            // `REFIT` closure, and the same closure a banner appearing calls,
            // which is the case the guard was written for. At the same scale
            // it is a no-op everywhere except in the re-fit it triggers.
            //
            // Without the guard this measures the Games page (182px of content)
            // and shrinks the window to about a third of its height under a
            // user who is reading it.
            {
                let (s, d, h) = (seen.clone(), demand.clone(), hub.clone());
                at(1500, move || {
                    tobii_gtk::apply_text_scale(1.0);
                    let mut s = s.borrow_mut();
                    s.claims_on_games = d.reasons();
                    s.size_on_games = (h.default_width(), h.default_height());
                });
            }

            // ---- Ctrl+Page_Up back.
            {
                let (s, h) = (seen.clone(), hub.clone());
                at(1800, move || {
                    let handled = press(
                        h.upcast_ref(),
                        gtk::gdk::Key::Page_Up,
                        gtk::gdk::ModifierType::CONTROL_MASK,
                    );
                    let mut s = s.borrow_mut();
                    s.handled.push(("ctrl+page_up", handled));
                    s.tabs.push((
                        "after ctrl+page_up",
                        hub_stack(h.upcast_ref())
                            .and_then(|t| t.visible_child_name().map(Into::into)),
                    ));
                });
            }

            // ---- and a plain Page_Down, which must NOT move the tab: the
            //      controller is in the Capture phase, so a key it claimed too
            //      eagerly would be taken from every scroller in the window.
            {
                let (s, h) = (seen.clone(), hub.clone());
                at(2000, move || {
                    let handled = press(
                        h.upcast_ref(),
                        gtk::gdk::Key::Page_Down,
                        gtk::gdk::ModifierType::empty(),
                    );
                    let mut s = s.borrow_mut();
                    s.handled.push(("page_down alone", handled));
                    s.tabs.push((
                        "after page_down alone",
                        hub_stack(h.upcast_ref())
                            .and_then(|t| t.visible_child_name().map(Into::into)),
                    ));
                });
            }

            {
                let a = app.clone();
                at(2300, move || a.activate_action("quit", None));
            }
            // If the quit action does nothing the run must still end, and fail.
            {
                let (a, s) = (app.clone(), seen.clone());
                at(9000, move || {
                    s.borrow_mut()
                        .troubles
                        .push("the quit action did not end the program".to_string());
                    a.quit();
                });
            }
        });
    }

    app.run_with_args::<&str>(&[]);

    // The held reference goes AFTER the run loop has ended and BEFORE the
    // census is read. Holding it across the quit is the point; a census read
    // while the test is still holding the subtree could only ever report
    // everything alive, which measures the test rather than the tab.
    keep.borrow_mut().clear();

    let c = pending.borrow_mut().take();
    let mut s = seen.borrow_mut();
    match c {
        Some(c) => {
            s.widgets = c.widgets.len();
            s.controllers = c.controllers.len();
            s.widgets_alive_after_quit = c.widgets.iter().filter(|w| w.upgrade().is_some()).count();
            s.controllers_alive_after_quit = c
                .controllers
                .iter()
                .filter(|w| w.upgrade().is_some())
                .count();
        }
        None => s.troubles.push("no census was taken".to_string()),
    }
    std::mem::take(&mut *s)
}

fn check(seen: &Seen) {
    assert!(
        seen.troubles.is_empty(),
        "a step of the timeline could not run: {:?}\n{seen:#?}",
        seen.troubles
    );

    // 1. The container.
    assert_eq!(
        seen.tabs
            .iter()
            .map(|(when, tab)| (*when, tab.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            ("at open", Some(tobii_gtk::TAB_TRACKER)),
            ("after ctrl+page_down", Some(tobii_gtk::TAB_GAMES)),
            ("after ctrl+page_up", Some(tobii_gtk::TAB_TRACKER)),
            // A bare Page Down belongs to whatever has the focus. The tab keys
            // live on a Capture-phase controller, which sees every key before
            // any focused child does, so this is the assertion that it lets go
            // of the ones that are not its own.
            ("after page_down alone", Some(tobii_gtk::TAB_TRACKER)),
        ],
        "the hub opens on Tracker and the two Ctrl+Page keys walk the tabs: {seen:#?}"
    );
    for (key, handled) in &seen.handled {
        let want = *key != "page_down alone";
        assert_eq!(
            handled, &want,
            "{key}: handled={handled}, and it must be {want} — a Capture-phase \
             controller that swallows a plain Page Down takes it from every scroller \
             in the window: {seen:#?}"
        );
    }

    // 2. The stack measures its VISIBLE child, not the largest of its children.
    //
    //    Asserted as the two settings and NOT as a measurement, and the reason
    //    is worth writing down because it was found by breaking it: today the
    //    Games tab is smaller than the Tracker tab in BOTH directions (see the
    //    two heights and the minimum width below), so `vhomogeneous(true)`
    //    changes nothing a measurement can see, and a check that compared the
    //    stack's height against the visible tab's passed with both settings
    //    deleted. It is insurance against the tab growing — which stage 2, with
    //    a grouped list of every profile on the machine, is exactly the sort of
    //    thing to do — so what has to be asserted is that the insurance is
    //    still there.
    assert_eq!(
        seen.homogeneous,
        (false, false),
        "the stack is homogeneous, so it measures the largest of its children \
         rather than the one showing, and the rack will open in a window sized \
         for a tab nobody is looking at: {seen:#?}"
    );
    assert_eq!(
        seen.stack_height, seen.tracker_height,
        "the stack is measuring something other than the tab that is showing: \
         {seen:#?}"
    );

    // 3. The Games tab may not set the opening width.
    //
    //    `default_width` is derived from the control rack alone, so it cannot —
    //    but GTK warns and clips when a stack child's MINIMUM is wider than the
    //    window it is in, and that is what this guards.
    assert!(
        seen.games_min_width < seen.hub_natural_width,
        "the games tab's minimum width ({}) is not under the hub's natural width \
         ({}), so GTK will clip it: {seen:#?}",
        seen.games_min_width,
        seen.hub_natural_width
    );

    // 4. A tab switch does not resize the window. The re-fit returns early
    //    unless the Tracker tab is showing, precisely so that a banner arriving
    //    while somebody is reading about a game cannot re-measure the window at
    //    that page's height.
    assert_eq!(
        seen.size_on_tracker, seen.size_on_games,
        "switching tabs resized the window, which is worse than a scrollbar: \
         {seen:#?}"
    );

    // 5. The switcher is reachable without a pointer. The tabs are the only way
    //    to the other half of this program, and a hub that can only be walked
    //    with a mouse has half of itself behind one.
    assert!(
        seen.tabs_to_switcher.is_some(),
        "forty Tab presses never reached the tab switcher: {seen:#?}"
    );
    assert_eq!(
        seen.switcher_labels,
        vec!["Tracker".to_string(), "Games".to_string()],
        "the two words that name the halves of this program, in the order the \
         pages were added: {seen:#?}"
    );
    // Where it sits: after the title, before everything that is pinned right.
    // A switcher that drifted to the end of the header would read as two more
    // icon buttons rather than as a tab bar. `GtkLabel` is the app title,
    // `GtkBox` the status bar with the pill and the dot, then the help button
    // and the cogwheel.
    assert_eq!(
        seen.header_order,
        vec![
            "GtkLabel".to_string(),
            "GtkStackSwitcher".to_string(),
            "GtkBox".to_string(),
            "GtkButton".to_string(),
            "GtkMenuButton".to_string(),
        ],
        "the switcher is not between the title and the status bar: {seen:#?}"
    );

    // 6. Neither tab ADDS a claim on the tracker.
    //
    //    A subset check, and in both directions, because one claim in this list
    //    is not the tab's to control: the hub takes "the hub window" while it
    //    has focus AND the Tracker tab is showing, so the number of claims can
    //    legitimately go DOWN on the Games tab. What nothing here may do is add
    //    one — which is what `hold_while_open` would do, and what
    //    `tests/help_window.rs` and `tests/flows_release_the_tracker.rs` assert
    //    about their own windows.
    //
    //    That the claim is DROPPED on the Games tab is asserted where it can
    //    fail: over `hub_wants_tracker`, headlessly, in `lib.rs`. A window in a
    //    nested compositor is never active, so a check here would pass with the
    //    condition deleted.
    for (when, claims) in [
        ("the tracker tab", &seen.claims_on_tracker),
        ("the games tab", &seen.claims_on_games),
    ] {
        let added: Vec<&&str> = claims
            .iter()
            .filter(|r| !seen.claims_before.contains(r))
            .collect();
        assert!(
            added.is_empty(),
            "{when} took {added:?} on the tracker: {seen:#?}"
        );
    }

    // 7. The lifetime, which is what this file is for.
    assert!(
        seen.widgets > 100 && seen.controllers > 5,
        "the census is too small to be measuring the games tab at all ({} widgets, \
         {} controllers) — the finder is probably looking at the wrong tree: \
         {seen:#?}",
        seen.widgets,
        seen.controllers
    );
    assert_eq!(
        (
            seen.widgets_alive_after_quit,
            seen.controllers_alive_after_quit
        ),
        (0, 0),
        "{} of {} widgets and {} of {} event controllers were still alive after the \
         program quit, which is a reference cycle between two of the tab's own \
         parts{}: {seen:#?}",
        seen.widgets_alive_after_quit,
        seen.widgets,
        seen.controllers_alive_after_quit,
        seen.controllers,
        if seen.held {
            " — and this run held the subtree across the quit on purpose and let go \
             before this census, so what it proves is that the freeing does not \
             depend on nobody else having taken a reference"
        } else {
            ""
        },
    );
}

/// Two whole runs of the program, one test.
///
/// **One test and not two**, and that is a constraint rather than a choice:
/// Rust's harness gives every `#[test]` a thread of its own, `--test-threads=1`
/// included, and GTK refuses to be initialised from a second one. Two tests
/// here abort the process with "Attempted to initialize GTK from two different
/// threads" and report nothing at all.
///
/// The second run holds the Games subtree across the quit and lets go
/// afterwards. Without it, a subtree that is freed only because nobody else
/// ever took a reference to it would pass — and "freed unless somebody looks at
/// it" is not a property anything can depend on. It is the half the modal's own
/// test had, and the half that makes the result mean something.
#[test]
#[ignore = "needs a display"]
fn the_games_tab_walks_by_keyboard_takes_no_claim_and_frees_itself() {
    let seen = run("dev.tobii.test.gamestab", false);
    // The geometry, printed rather than asserted: these are this machine's font
    // metrics, and where they are recorded is the comment in `lib.rs` beside
    // the control columns. Re-take them here when either changes.
    println!(
        "GEOMETRY hub natural width {}, window {}x{}, stack {} (tracker {}, games \
         {}), games minimum width {}",
        seen.hub_natural_width,
        seen.size_on_tracker.0,
        seen.size_on_tracker.1,
        seen.stack_height,
        seen.tracker_height,
        seen.games_height,
        seen.games_min_width,
    );
    check(&seen);

    let held = run("dev.tobii.test.gamestab.held", true);
    check(&held);
}
