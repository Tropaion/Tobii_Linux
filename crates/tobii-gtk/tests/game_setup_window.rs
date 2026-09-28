//! The game-setup window: its door, its three exits, its pages, and the one
//! promise about the tracker.
//!
//! Needs a display, so it is ignored by default (CI has none). Run it with:
//!
//!     cargo test -p tobii-gtk --test game_setup_window -- --ignored
//!
//! and run it in a **nested** compositor rather than on the session display:
//!
//!     kwin_wayland --virtual --width 1600 --height 1200 --socket wl-gs &
//!     WAYLAND_DISPLAY=wl-gs GDK_BACKEND=wayland cargo test -p tobii-gtk \
//!         --test game_setup_window -- --ignored
//!
//! No tracker is needed. The one device fact asserted here — that a window
//! which reads files and runs a subprocess takes no claim on the tracker — is
//! the device thread's own list of claim reasons, which is state, not
//! hardware. That is the argument `tests/help_window.rs` and
//! `tests/flows_release_the_tracker.rs` both make about themselves.
//!
//! Nothing the user has saved is touched. The two synthetic runs are given a
//! `Scan` naming directories that do not exist, so `tobii-steam` finds no
//! library and `tobii-config` finds no profile; no button that writes anything
//! is ever pressed, and the installer is never started.
//!
//! Two promises are worth stating, because both are regressions this project
//! has already paid for once:
//!
//! * **It must not keep itself alive.** GTK4 emits `destroy` only on
//!   finalisation, so a window that holds a strong reference to itself — or a
//!   cycle between two of its own siblings — is never freed, and everything it
//!   holds stays with it. That is the v0.3.1 bug. It is checked here through
//!   weak references taken over the whole subtree before the close, and once
//!   more with the test itself holding the window across the close and letting
//!   go afterwards — because a freeing that only happens when nobody else ever
//!   took a reference is not a freeing anybody can rely on.
//! * **It must take no claim on the tracker.** Lighting the illuminators to
//!   show somebody a paragraph is precisely the behaviour the whole demand
//!   mechanism exists to prevent.

use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use tobii_gtk::game_setup;

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

/// The widgets a window actually OWNS: everything, minus every subtree that has
/// a surface of its own.
///
/// The exclusion is `tests/help_window.rs`'s and the argument is the same one:
/// a `GtkTooltipWindow` is a separate surface GTK creates once per display and
/// parents into whichever toplevel is showing a tooltip at that moment, so
/// whether it is inside this tree when a census is taken is decided by where
/// the pointer is sitting, and whether it is alive afterwards is decided by
/// GTK, which still owns it. This window puts a tooltip on its Details button,
/// so it can borrow that surface exactly as the hub's cards do.
///
/// It is the only thing dropped. The only `GtkNative` this module builds is the
/// toplevel itself, which is `root` and is pushed before `descend` is
/// consulted; everything else it builds is a box, a label, a button, a search
/// entry, a scroller, a list, a row or a stack, and not one of those owns a
/// surface. Re-read this if a `Popover` or a second `Window` is ever added to
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

/// The button whose label reads `text`, clicked the way a user would.
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

/// Run `f` once, `ms` into the timeline.
fn at<F: FnOnce() + 'static>(ms: u64, f: F) {
    gtk::glib::timeout_add_local_once(Duration::from_millis(ms), f);
}

/// The window's stack, or [`None`] when this window has none — which only
/// happens when the window found is not the one the step meant to find.
///
/// Tolerant rather than panicking for the reason `Seen::troubles` gives: a
/// panic inside a GLib callback is a non-unwinding abort, and an abort prints
/// a stack trace instead of the assertion that would have named the fault.
fn stack_of(win: &gtk::Window) -> Option<gtk::Stack> {
    all(win.upcast_ref())
        .into_iter()
        .find(|w| w.widget_name() == game_setup::STACK_NAME)
        .and_then(|w| w.downcast::<gtk::Stack>().ok())
}

fn setup_windows(app: &gtk::Application) -> Vec<gtk::Window> {
    app.windows()
        .into_iter()
        .filter(|w| w.title().as_deref() == Some(game_setup::TITLE))
        .collect()
}

/// Is the focus inside `page`?
fn focus_is_under(win: &gtk::Window, page: &gtk::Widget) -> bool {
    // `GtkRoot::get_focus`, which is where GTK4 keeps the focus for a toplevel.
    let Some(mut f) = gtk::prelude::RootExt::focus(win) else {
        return false;
    };
    loop {
        if f == *page {
            return true;
        }
        match f.parent() {
            Some(p) => f = p,
            None => return false,
        }
    }
}

/// A scan that names nothing real: no library is found, no profile is found,
/// and nothing under the user's home is opened.
fn synthetic(apps: Vec<tobii_steam::App>) -> game_setup::Scan {
    let home = PathBuf::from("/nonexistent/tobii-game-setup-test/home");
    game_setup::Scan {
        // Built over the same nonexistent home, so it finds no library and
        // every prefix question the window asks answers "no" — which is what
        // this fixture is for. The window takes its walk from here rather than
        // making one of its own, so a `Scan` that names nothing real is a
        // window that touches nothing real.
        steam: std::rc::Rc::new(tobii_steam::Steam::at(&home)),
        profiles_dir: PathBuf::from("/nonexistent/tobii-game-setup-test/profiles"),
        home,
        apps,
        missing: Vec::new(),
    }
}

/// The joystick status a window opened here is given: nobody has asked for
/// one. This test starts no device thread, and the window only ever reads it.
fn no_joystick() -> std::sync::Arc<std::sync::Mutex<tobii_gtk::device::JoystickStatus>> {
    std::sync::Arc::new(std::sync::Mutex::new(
        tobii_gtk::device::JoystickStatus::Off,
    ))
}

fn app_named(appid: &str, name: &str) -> tobii_steam::App {
    tobii_steam::App {
        appid: appid.to_string(),
        name: name.to_string(),
        buildid: None,
    }
}

/// What one open-and-close looked like.
#[derive(Debug, Default)]
struct Run {
    what: &'static str,
    /// Whether the test itself kept a strong reference across the close.
    held: bool,
    windows_after_open: usize,
    /// After the door was pressed a second time.
    windows_after_second_press: Option<usize>,
    windows_after_close: usize,
    widgets: usize,
    widgets_alive_after_close: usize,
    controllers: usize,
    controllers_alive_after_close: usize,
}

#[derive(Debug, Default)]
struct Seen {
    runs: Vec<Run>,
    claims_before: Vec<&'static str>,
    claims_while_open: Vec<&'static str>,
    claims_after_close: Vec<&'static str>,
    /// The stack page each synthetic run showed, in order.
    pages: Vec<(&'static str, Option<String>)>,
    /// Whether the focus was on the page now showing, after each switch.
    focus_ok: Vec<(&'static str, bool)>,
    /// A step that could not run at all.
    ///
    /// Collected rather than panicked: a panic inside a GLib callback is a
    /// non-unwinding abort, which takes the process down with a stack trace
    /// and no test result at all.
    troubles: Vec<String>,
}

/// A census of a window, taken before it is closed and read after.
struct Census {
    widgets: Vec<gtk::glib::WeakRef<gtk::Widget>>,
    controllers: Vec<gtk::glib::WeakRef<gtk::EventController>>,
}

fn census(win: &gtk::Window) -> Census {
    let ws = own_widgets(win.upcast_ref());
    let cs = controllers_of(&ws);
    Census {
        widgets: ws.iter().map(|w| w.downgrade()).collect(),
        controllers: cs.iter().map(|c| c.downgrade()).collect(),
    }
}

#[test]
#[ignore = "needs a display"]
fn the_game_setup_window_opens_closes_frees_itself_and_takes_no_claim_on_the_tracker() {
    let app = gtk::Application::builder()
        .application_id("dev.tobii.test.gamesetup")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let seen: Rc<RefCell<Seen>> = Rc::default();
    // At test scope, not inside the activate handler: its clones otherwise live
    // only in the one-shot close callback, which is dropped once it has run —
    // and the "held" window would be freed before anything checked it.
    let keep: Rc<RefCell<Vec<gtk::Window>>> = Rc::default();

    {
        let (seen, keep) = (seen.clone(), keep.clone());
        app.connect_activate(move |app| {
            tobii_gtk::load_css();
            let session = tobii_gtk::device::spawn();
            let demand = session.2.clone();
            let hub = tobii_gtk::build_hub(app, session).expect("hub");
            hub.present();

            seen.borrow_mut().claims_before = demand.reasons();

            // Where the census of each run is parked between the close and the
            // check that follows it.
            let pending: Rc<RefCell<Option<Census>>> = Rc::default();

            // ---- run one: the real door, closed with the Close button.

            {
                let h = hub.clone();
                at(600, move || {
                    button_labelled(h.upcast_ref(), "Set up a game…").emit_clicked();
                });
            }
            {
                let (a, s, d, p, h) = (
                    app.clone(),
                    seen.clone(),
                    demand.clone(),
                    pending.clone(),
                    hub.clone(),
                );
                at(1000, move || {
                    s.borrow_mut().claims_while_open = d.reasons();
                    let first = setup_windows(&a).len();
                    // Pressing the door again presents the window that is open
                    // rather than building a second one.
                    button_labelled(h.upcast_ref(), "Set up a game…").emit_clicked();
                    let second = setup_windows(&a).len();
                    let Some(win) = setup_windows(&a).into_iter().next() else {
                        s.borrow_mut()
                            .troubles
                            .push("the hub's button opened no window".to_string());
                        return;
                    };
                    *p.borrow_mut() = Some(census(&win));
                    let mut s = s.borrow_mut();
                    s.runs.push(Run {
                        what: "close button",
                        windows_after_open: first,
                        windows_after_second_press: Some(second),
                        ..Run::default()
                    });
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(1400, move || {
                    let Some(win) = setup_windows(&a).into_iter().next() else {
                        s.borrow_mut()
                            .troubles
                            .push("nothing to press Close on".to_string());
                        return;
                    };
                    button_labelled(win.upcast_ref(), "Close").emit_clicked();
                });
            }
            {
                let (a, s, d, p) = (app.clone(), seen.clone(), demand.clone(), pending.clone());
                at(2200, move || {
                    s.borrow_mut().claims_after_close = d.reasons();
                    let c = p.borrow_mut().take().expect("a census");
                    let mut s = s.borrow_mut();
                    let run = s.runs.last_mut().expect("run one");
                    run.windows_after_close = setup_windows(&a).len();
                    run.widgets = c.widgets.len();
                    run.controllers = c.controllers.len();
                    run.widgets_alive_after_close =
                        c.widgets.iter().filter(|w| w.upgrade().is_some()).count();
                    run.controllers_alive_after_close = c
                        .controllers
                        .iter()
                        .filter(|w| w.upgrade().is_some())
                        .count();
                });
            }

            // ---- run two: a synthetic empty machine, closed with Esc, with
            // the test holding the window across the close.

            {
                let (a, s, h, p, k) = (
                    app.clone(),
                    seen.clone(),
                    hub.clone(),
                    pending.clone(),
                    keep.clone(),
                );
                at(2600, move || {
                    let win = game_setup::open_with(&a, &h, synthetic(Vec::new()), no_joystick());
                    let mut s = s.borrow_mut();
                    s.pages.push((
                        "no games",
                        stack_of(&win).and_then(|st| st.visible_child_name().map(Into::into)),
                    ));
                    s.runs.push(Run {
                        what: "escape",
                        held: true,
                        windows_after_open: setup_windows(&a).len(),
                        ..Run::default()
                    });
                    *p.borrow_mut() = Some(census(&win));
                    k.borrow_mut().push(win);
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(3000, move || {
                    let Some(win) = setup_windows(&a).into_iter().next() else {
                        s.borrow_mut()
                            .troubles
                            .push("nothing to press Esc at".to_string());
                        return;
                    };
                    if !press(&win, gtk::gdk::Key::Escape) {
                        s.borrow_mut()
                            .troubles
                            .push("Esc was not handled by the window".to_string());
                    }
                });
            }
            // The held reference goes AFTER the close and BEFORE the census is
            // read. Holding it across the close is the point — it proves the
            // freeing does not depend on nobody else having taken a reference —
            // but a census read while the test is still holding the window can
            // only ever report everything alive, which measures the test rather
            // than the window.
            {
                let k = keep.clone();
                at(3300, move || k.borrow_mut().clear());
            }
            {
                let (a, s, p) = (app.clone(), seen.clone(), pending.clone());
                at(3600, move || {
                    let c = p.borrow_mut().take().expect("a census");
                    let mut s = s.borrow_mut();
                    let run = s.runs.last_mut().expect("run two");
                    run.windows_after_close = setup_windows(&a).len();
                    run.widgets = c.widgets.len();
                    run.controllers = c.controllers.len();
                    run.widgets_alive_after_close =
                        c.widgets.iter().filter(|w| w.upgrade().is_some()).count();
                    run.controllers_alive_after_close = c
                        .controllers
                        .iter()
                        .filter(|w| w.upgrade().is_some())
                        .count();
                });
            }

            // ---- run three: two synthetic games, driving the stack, closed
            // with `win.close()`.

            {
                let (a, s, h, p) = (app.clone(), seen.clone(), hub.clone(), pending.clone());
                at(3900, move || {
                    let win = game_setup::open_with(
                        &a,
                        &h,
                        synthetic(vec![
                            app_named("359320", "Elite Dangerous"),
                            app_named("220", "Half-Life 2"),
                        ]),
                        no_joystick(),
                    );
                    let mut s = s.borrow_mut();
                    s.pages.push((
                        "two games",
                        stack_of(&win).and_then(|st| st.visible_child_name().map(Into::into)),
                    ));
                    s.runs.push(Run {
                        what: "win.close()",
                        windows_after_open: setup_windows(&a).len(),
                        ..Run::default()
                    });
                    *p.borrow_mut() = Some(census(&win));
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(4200, move || {
                    let Some(win) = setup_windows(&a).into_iter().next() else {
                        s.borrow_mut()
                            .troubles
                            .push("no window to activate a row in".to_string());
                        return;
                    };
                    let list = all(win.upcast_ref())
                        .into_iter()
                        .find(|w| w.widget_name() == game_setup::LIST_NAME)
                        .and_then(|w| w.downcast::<gtk::ListBox>().ok());
                    match list.as_ref().and_then(|l| l.row_at_index(0)) {
                        // What a single click on a GtkListBox emits. The rows
                        // are the buttons here, so this is the path a user
                        // takes.
                        Some(row) => list
                            .expect("the row came from it")
                            .emit_by_name::<()>("row-activated", &[&row]),
                        None => s
                            .borrow_mut()
                            .troubles
                            .push("the pick page had no first row to activate".to_string()),
                    }
                    let st = stack_of(&win);
                    let mut s = s.borrow_mut();
                    s.pages.push((
                        "after activate",
                        st.as_ref()
                            .and_then(|st| st.visible_child_name().map(Into::into)),
                    ));
                    s.focus_ok.push((
                        "after activate",
                        st.and_then(|st| st.visible_child())
                            .is_some_and(|page| focus_is_under(&win, &page)),
                    ));
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(4500, move || {
                    let Some(win) = setup_windows(&a).into_iter().next() else {
                        s.borrow_mut()
                            .troubles
                            .push("no window to press Back in".to_string());
                        return;
                    };
                    button_labelled(win.upcast_ref(), "Back").emit_clicked();
                    let st = stack_of(&win);
                    let mut s = s.borrow_mut();
                    s.pages.push((
                        "after back",
                        st.as_ref()
                            .and_then(|st| st.visible_child_name().map(Into::into)),
                    ));
                    s.focus_ok.push((
                        "after back",
                        st.and_then(|st| st.visible_child())
                            .is_some_and(|page| focus_is_under(&win, &page)),
                    ));
                });
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(4800, move || match setup_windows(&a).into_iter().next() {
                    Some(win) => win.close(),
                    None => s
                        .borrow_mut()
                        .troubles
                        .push("no window to close".to_string()),
                });
            }
            {
                let (a, s, p) = (app.clone(), seen.clone(), pending.clone());
                at(5400, move || {
                    let c = p.borrow_mut().take().expect("a census");
                    let mut s = s.borrow_mut();
                    let run = s.runs.last_mut().expect("run three");
                    run.windows_after_close = setup_windows(&a).len();
                    run.widgets = c.widgets.len();
                    run.controllers = c.controllers.len();
                    run.widgets_alive_after_close =
                        c.widgets.iter().filter(|w| w.upgrade().is_some()).count();
                    run.controllers_alive_after_close = c
                        .controllers
                        .iter()
                        .filter(|w| w.upgrade().is_some())
                        .count();
                });
            }

            {
                let a = app.clone();
                at(5800, move || a.quit());
            }
        });
    }

    app.run_with_args::<&str>(&[]);

    let seen = seen.borrow();
    assert!(
        seen.troubles.is_empty(),
        "a step of the timeline could not run: {:?}\n{seen:#?}",
        seen.troubles
    );
    assert_eq!(seen.runs.len(), 3, "three runs: {seen:#?}");

    // 1. The door.
    let first = &seen.runs[0];
    assert_eq!(
        first.windows_after_open, 1,
        "the hub's button opens exactly one game-setup window: {seen:#?}"
    );
    assert_eq!(
        first.windows_after_second_press,
        Some(1),
        "pressing it again presents the one that is open rather than building a second: \
         {seen:#?}"
    );

    // 2. The tracker. This window reads files and runs a subprocess; it wants
    //    no frames, and the illuminators must not come on for it.
    //
    //    Subset and not equality, in both directions, because one claim in this
    //    list is not this window's to control: the hub takes "the hub window"
    //    while it has focus and gives it back when it does not, so a modal
    //    opening over it REMOVES that claim and closing it puts it back. What
    //    this window may never do is ADD one — which is exactly what
    //    `hold_while_open` would do, and what `tests/help_window.rs` and
    //    `tests/flows_release_the_tracker.rs` assert about their own windows.
    //    The joystick's and keep-awake's claims, if this machine has them on,
    //    are in `claims_before` already and so pass through the subset
    //    unremarked.
    const HUB_FOCUS: &str = "the hub window";
    let added: Vec<&&str> = seen
        .claims_while_open
        .iter()
        .filter(|r| !seen.claims_before.contains(r))
        .collect();
    assert!(
        added.is_empty(),
        "opening the game-setup window took {added:?} on the tracker: it reads files and \
         runs a subprocess and wants no frames, so lighting the illuminators for it is \
         precisely what the demand mechanism exists to prevent: {seen:#?}"
    );
    let left: Vec<&&str> = seen
        .claims_after_close
        .iter()
        .filter(|r| !seen.claims_before.contains(r) && **r != HUB_FOCUS)
        .collect();
    assert!(
        left.is_empty(),
        "closing it left {left:?} behind: {seen:#?}"
    );

    // 3. The exits, and 4. no leak — for each of the three ways out.
    for run in &seen.runs {
        assert_eq!(
            run.windows_after_close, 0,
            "{} did not close the window: {seen:#?}",
            run.what
        );
        assert!(
            run.widgets > 20 && run.controllers > 5,
            "the census is too small to be measuring the window at all ({} widgets, {} \
             controllers) — the finder is probably looking at the wrong tree: {seen:#?}",
            run.widgets,
            run.controllers
        );
        assert_eq!(
            (
                run.widgets_alive_after_close,
                run.controllers_alive_after_close
            ),
            (0, 0),
            "{} left {} of {} widgets and {} of {} event controllers alive after the close, \
             which is a reference cycle between two of the window's own parts{}: {seen:#?}",
            run.what,
            run.widgets_alive_after_close,
            run.widgets,
            run.controllers_alive_after_close,
            run.controllers,
            if run.held {
                " — and this run held the window across the close on purpose and let go \
                 before this census, so what it proves is that the freeing does not depend \
                 on nobody else having taken a reference"
            } else {
                ""
            },
        );
    }

    // 5. The stack.
    assert_eq!(
        seen.pages,
        vec![
            ("no games", Some(game_setup::PAGE_NOTHING.to_string())),
            ("two games", Some(game_setup::PAGE_PICK.to_string())),
            ("after activate", Some(game_setup::PAGE_GAME.to_string())),
            ("after back", Some(game_setup::PAGE_PICK.to_string())),
        ],
        "the window opens on the page its scan calls for, and the rows are the buttons"
    );

    // 6. Focus follows the page. Not cosmetic: `help.rs` records the
    //    measurement that a pane folded away with the focus inside it
    //    permanently holds its subtree, and a stack page is a pane.
    for (when, ok) in &seen.focus_ok {
        assert!(
            ok,
            "{when}: the focus is not on the page now showing, so the page that folded away \
             kept it: {seen:#?}"
        );
    }
}
