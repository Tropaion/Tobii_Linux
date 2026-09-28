//! The whole hub frees itself when the program quits.
//!
//! Needs a display, so it is ignored by default (CI has none). Run it with:
//!
//!     kwin_wayland --virtual --width 1600 --height 1200 --socket wl-hl &
//!     WAYLAND_DISPLAY=wl-hl GDK_BACKEND=wayland cargo test -p tobii-gtk \
//!         --test hub_lifetime -- --ignored
//!
//! or through `scripts/display-tests.sh`, which does both.
//!
//! # What this is for, and why it is not `tests/games_tab.rs`
//!
//! `games_tab.rs` censuses the **Games subtree** and `help_window.rs` censuses
//! the **help window**. Between them they cover two of the hub's three parts,
//! and the part they do not cover is the one everything else hangs off: the
//! window's own root, with the header, the two banners and the control rack on
//! it. Two reference cycles sat there — `update::banner`'s Dismiss button
//! holding the row that holds it, and the recalibration banner's two buttons
//! doing the same — for as long as both of those tests have existed, and
//! neither could see them.
//!
//! GTK4 emits `destroy` only on finalisation, so nothing about a leaked
//! subtree is visible until somebody weak-references it and looks. That is the
//! shape of the v0.3.1 bug, where a flow's window stayed alive for the life of
//! the process and the tracker never went dark again.
//!
//! # What it does not assert
//!
//! It does not assert a number. It asserts **zero**, and the reason to prefer
//! that to a budget is that a budget is a place to put the next leak: every
//! widget here is one this program built and this program is quitting, so
//! there is no honest reason for any of them to outlive the run.
//!
//! No tracker is needed, and nothing the person running it has saved is
//! touched: no button that writes anything is pressed.

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// `root` and everything under it, minus every subtree with a surface of its
/// own.
///
/// The exclusion is `games_tab.rs`'s and the argument is the same: a
/// `GtkTooltipWindow` is a separate surface GTK creates once per display and
/// parents into whichever toplevel is showing a tooltip, so whether it is
/// inside this tree when a census is taken depends on where the pointer is
/// sitting, and whether it is alive afterwards is GTK's business. The hub has
/// tooltips all over it.
///
/// It also drops the settings popover, for a sharper reason: a `GtkPopover` is
/// a `GtkNative`, its lifetime belongs to the `GtkMenuButton` that owns it, and
/// GTK keeps one alive between showings by design.
fn own_widgets(root: &gtk::Widget) -> Vec<gtk::Widget> {
    fn go(w: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
        out.push(w.clone());
        let mut c = w.first_child();
        while let Some(ch) = c {
            if !ch.is::<gtk::Native>() {
                go(&ch, out);
            }
            c = ch.next_sibling();
        }
    }
    let mut v = Vec::new();
    go(root, &mut v);
    v
}

/// Every event controller on every widget — what a leaked handler is made of,
/// and the more sensitive of the two counts.
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

#[derive(Debug, Default)]
struct Seen {
    widgets: usize,
    widgets_alive: usize,
    controllers: usize,
    controllers_alive: usize,
    /// What is still alive, by type and by any text it carries — a count says
    /// there is a cycle and this says where to start looking.
    survivors: Vec<String>,
    troubles: Vec<String>,
}

#[test]
#[ignore = "needs a display"]
fn the_whole_hub_frees_itself_when_the_program_quits() {
    let app = gtk::Application::builder()
        .application_id("dev.tobii.test.hublifetime")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let seen: Rc<RefCell<Seen>> = Rc::default();
    let pending: Rc<RefCell<Vec<gtk::glib::WeakRef<gtk::Widget>>>> = Rc::default();
    let pending_c: Rc<RefCell<Vec<gtk::glib::WeakRef<gtk::EventController>>>> = Rc::default();

    {
        let (seen, pending, pending_c) = (seen.clone(), pending.clone(), pending_c.clone());
        app.connect_activate(move |app| {
            tobii_gtk::load_css();
            tobii_gtk::apply_text_scale(1.0);
            // The same action the cogwheel's Quit uses, so what is torn down
            // here is what is torn down in the field. A bare `app.quit()` ends
            // the loop without running the hub's close handler at all.
            tobii_gtk::install_quit_action(app);
            let session = tobii_gtk::device::spawn();
            let hub = tobii_gtk::build_hub(app, session).expect("hub");
            hub.present();

            // Both tabs FIRST, because half the tree is on the one that is not
            // showing — and because the Games tab rebuilds its list when it is
            // mapped. A census taken before that walk weak-references rows that
            // are destroyed a moment later, which reads as a pass, and never
            // sees the ones that exist at quit: a cycle in a row's own widgetry
            // would sail through it.
            {
                let h = hub.clone();
                at(600, move || press(&h, gtk::gdk::Key::Page_Down));
            }
            {
                let h = hub.clone();
                at(900, move || press(&h, gtk::gdk::Key::Page_Up));
            }

            // The census is taken over the window's CHILD rather than the
            // window: a `GtkApplicationWindow` is itself a `GtkNative` and the
            // application holds it, so what is being asked about is everything
            // the hub built inside it.
            {
                let (s, h, p, pc) = (
                    seen.clone(),
                    hub.clone(),
                    pending.clone(),
                    pending_c.clone(),
                );
                at(1300, move || {
                    let Some(child) = h.child() else {
                        s.borrow_mut().troubles.push("the hub has no child".into());
                        return;
                    };
                    let ws = own_widgets(&child);
                    let cs = controllers_of(&ws);
                    let mut s = s.borrow_mut();
                    s.widgets = ws.len();
                    s.controllers = cs.len();
                    *p.borrow_mut() = ws.iter().map(|w| w.downgrade()).collect();
                    *pc.borrow_mut() = cs.iter().map(|c| c.downgrade()).collect();
                });
            }

            {
                let a = app.clone();
                at(1900, move || a.activate_action("quit", None));
            }
            {
                let (a, s) = (app.clone(), seen.clone());
                at(9000, move || {
                    s.borrow_mut()
                        .troubles
                        .push("the quit action did not end the program".into());
                    a.quit();
                });
            }
        });
    }

    app.run_with_args::<&str>(&[]);

    {
        let mut s = seen.borrow_mut();
        let alive: Vec<gtk::Widget> = pending
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .collect();
        s.widgets_alive = alive.len();
        s.controllers_alive = pending_c
            .borrow()
            .iter()
            .filter(|c| c.upgrade().is_some())
            .count();
        // Named, not just counted. A bare number tells somebody there is a
        // cycle; this tells them which two widgets to look between, which is
        // the whole of the work.
        s.survivors = alive
            .iter()
            .take(24)
            .map(|w| {
                let kind = w.type_().to_string();
                match w.downcast_ref::<gtk::Label>() {
                    Some(l) => format!("{kind} {:?}", l.text()),
                    None => {
                        let n = w.widget_name();
                        if n.is_empty() || n == kind {
                            kind
                        } else {
                            format!("{kind} #{n}")
                        }
                    }
                }
            })
            .collect();
    }

    let s = seen.borrow();
    assert!(
        s.troubles.is_empty(),
        "a step could not run: {:?}\n{s:#?}",
        s.troubles
    );
    assert!(
        s.widgets > 300 && s.controllers > 10,
        "the census is too small to be the whole hub ({} widgets, {} controllers) — the \
         walk is probably not finding the tree: {s:#?}",
        s.widgets,
        s.controllers
    );
    assert_eq!(
        (s.widgets_alive, s.controllers_alive),
        (0, 0),
        "{} of {} widgets and {} of {} event controllers outlived the program. That is a \
         reference cycle between two parts of one subtree — a handler holding a widget that \
         holds the handler — which GTK never breaks. The rule this tree keeps is: labels and \
         plain boxes strongly, every widget that carries a handler through `downgrade()`. \
         What survived: {:#?}\n{s:#?}",
        s.widgets_alive,
        s.widgets,
        s.controllers_alive,
        s.controllers,
        s.survivors,
    );
}

/// Ctrl+`key` on the window's own controllers, the way the window sees it.
fn press(win: &impl IsA<gtk::Window>, key: gtk::gdk::Key) {
    use gtk::glib::translate::IntoGlib;
    let controllers = win.as_ref().observe_controllers();
    for i in 0..controllers.n_items() {
        if let Some(keys) = controllers
            .item(i)
            .and_downcast::<gtk::EventControllerKey>()
        {
            let _ = keys.emit_by_name::<bool>(
                "key-pressed",
                &[
                    &key.into_glib(),
                    &0u32,
                    &gtk::gdk::ModifierType::CONTROL_MASK,
                ],
            );
        }
    }
}

/// Run `f` once, `ms` into the timeline.
fn at<F: FnOnce() + 'static>(ms: u64, f: F) {
    gtk::glib::timeout_add_local_once(Duration::from_millis(ms), f);
}
