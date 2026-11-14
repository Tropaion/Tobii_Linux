//! The head-model card's two dialogs free themselves, by every way out.
//!
//! Needs a display, so it is ignored by default (CI has none). Run it with:
//!
//!     scripts/display-tests.sh head_model_dialogs
//!
//! which starts a nested KWin that cannot touch your session; see the script.
//!
//! Both dialogs had a button holding its own window — Cancel in the pitch
//! dialog and both buttons in the terms dialog — and the pitch dialog's Esc
//! handler did too. A handler on a widget inside a window that holds that
//! window is a cycle GTK never breaks, so every one of these dialogs ever
//! opened stayed alive, with everything in it, for the rest of the process.
//! The pitch dialog's poll held its widgets as well, and the card's through
//! the card's `refresh`; and neither dialog was one of the application's
//! windows, so the hub's quit left both open, their close handlers unrun.
//!
//! GTK4 emits `destroy` only on finalisation, so this checks the way
//! `help_window.rs` does: weak references taken with the window open, looked
//! at after the close.
//!
//! No tracker is needed and nothing is written. The terms dialog's agree
//! button runs this test's own closure, not a download, and the pitch dialog
//! is given a device state and a command channel of this test's own: the
//! Start button's command lands in a receiver nobody serves, so nothing is
//! measured and no zero is saved.

use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tobii_gtk::device::{DeviceCommand, DeviceState};

/// One way out of one dialog, and what was left of it afterwards.
#[derive(Debug)]
struct Exit {
    name: &'static str,
    /// Widgets — the window first — and controllers weak-referenced while the
    /// dialog was open, so a census that found nothing cannot read as a pass.
    widgets: usize,
    controllers: usize,
    widgets_alive: usize,
    controllers_alive: usize,
}

#[derive(Debug, Default)]
struct Seen {
    exits: Vec<Exit>,
    /// How many times the terms dialog's agree callback ran, over all exits.
    agreed: usize,
    /// What the pitch dialog showed and did at the moments that matter.
    pitch_started: bool,
    pitch_active_after_stop: Option<bool>,
    pitch_outcome_text: Option<String>,
    troubles: Vec<String>,
}

/// What a scenario needs at each of its moments.
struct World {
    app: gtk::Application,
    parent: gtk::ApplicationWindow,
    seen: Rc<RefCell<Seen>>,
    state: Arc<Mutex<DeviceState>>,
    tx: Sender<DeviceCommand>,
    rx: Receiver<DeviceCommand>,
}

/// Something done to an open dialog.
type Act = Box<dyn Fn(&World, &gtk::Window)>;
type Step = (u64, Act);

#[test]
#[ignore = "needs a display"]
fn the_head_model_dialogs_free_themselves_by_every_way_out() {
    let app = gtk::Application::builder()
        .application_id("dev.tobii.test.headmodeldialogs")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let seen: Rc<RefCell<Seen>> = Rc::default();
    let at_quit: Rc<RefCell<Option<Census>>> = Rc::default();

    {
        let (seen, at_quit) = (seen.clone(), at_quit.clone());
        app.connect_activate(move |app| {
            tobii_gtk::load_css();
            // A parent, so the dialogs are opened the way the card opens them:
            // transient for a window, which is also what keeps the
            // application running between them.
            let parent = gtk::ApplicationWindow::builder()
                .application(app)
                .default_width(400)
                .default_height(300)
                .build();
            parent.present();
            let (tx, rx) = std::sync::mpsc::channel();
            let world = Rc::new(World {
                app: app.clone(),
                parent,
                seen: seen.clone(),
                state: Arc::new(Mutex::new(DeviceState::default())),
                tx,
                rx,
            });

            let terms = |w: &World| {
                let seen = w.seen.clone();
                tobii_gtk::head_model::terms_dialog(Some(w.parent.upcast_ref()), move || {
                    seen.borrow_mut().agreed += 1;
                });
                "Head-pose model"
            };
            let pitch = |w: &World| {
                tobii_gtk::head_model::pitch_dialog(
                    Some(w.parent.upcast_ref()),
                    w.state.clone(),
                    w.tx.clone(),
                );
                "Pitch zero"
            };

            let mut t = 400;
            // Each one is looked at 500 ms after its last step.
            let mut run =
                |name: &'static str, open: fn(&World) -> &'static str, steps: Vec<Step>| {
                    let end = steps.iter().map(|(at, _)| *at).max().unwrap_or(0);
                    let census = scenario(&world, t, name, open, steps);
                    let seen = world.seen.clone();
                    at(t + end + 500, move || {
                        seen.borrow_mut().exits.push(census.exit(name))
                    });
                    t += end + 500;
                };

            run("terms: Not now", terms, vec![(300, click("Not now"))]);
            run("terms: Esc", terms, vec![(300, escape())]);
            run(
                "terms: I agree",
                terms,
                vec![(300, click("I agree — download"))],
            );
            run("pitch: Cancel", pitch, vec![(300, click("Cancel"))]);
            run("pitch: Esc", pitch, vec![(300, escape())]);
            run(
                "pitch: Start, then Stop",
                pitch,
                vec![
                    (300, click("Start")),
                    (
                        700,
                        Box::new(|w: &World, win: &gtk::Window| {
                            let sent =
                                matches!(w.rx.try_recv(), Ok(DeviceCommand::PitchCalibrate { .. }));
                            let active = w.state.lock().unwrap().pitch_cal.active;
                            w.seen.borrow_mut().pitch_started = sent && active;
                            click("Stop")(w, win);
                            w.seen.borrow_mut().pitch_active_after_stop =
                                Some(w.state.lock().unwrap().pitch_cal.active);
                        }),
                    ),
                ],
            );
            run(
                "pitch: a result, then Done",
                pitch,
                vec![
                    (300, click("Start")),
                    (
                        600,
                        // What the device thread does when a run ends.
                        Box::new(|w: &World, _: &gtk::Window| {
                            while w.rx.try_recv().is_ok() {}
                            let mut s = w.state.lock().unwrap();
                            s.pitch_cal.active = false;
                            s.pitch_cal.result = Some(Ok(-20.0));
                        }),
                    ),
                    (
                        1100,
                        Box::new(|w: &World, win: &gtk::Window| {
                            w.seen.borrow_mut().pitch_outcome_text = labels(win.upcast_ref())
                                .into_iter()
                                .find(|l| l.starts_with("Pitch zero set"));
                        }),
                    ),
                    // Stop has become Done; a missing one is a trouble.
                    (1200, click("Done")),
                ],
            );

            // The last one ends the program with a measurement running, the
            // way the hub's quit does it: close every other window, then quit.
            // The poll is pending then for good (see `tobii_gtk`'s crate docs,
            // *Lifetimes*), so this is the way out a poll holding its widgets
            // strongly cannot pass. It is looked at after the program ends.
            *at_quit.borrow_mut() = Some(scenario(
                &world,
                t,
                QUIT,
                pitch,
                vec![
                    (300, click("Start")),
                    (
                        700,
                        // Exactly the hub's step 2b and 4: every window of
                        // the application but its own closed, then quit. The
                        // dialog is closed only if it is one of them.
                        Box::new(|w: &World, _: &gtk::Window| {
                            for o in w.app.windows() {
                                if o != *w.parent.upcast_ref::<gtk::Window>() {
                                    o.close();
                                }
                            }
                            w.app.quit();
                        }),
                    ),
                ],
            ));
            // A backstop, so a step that never lets go fails the test instead of
            // hanging it. The application weakly: this is still pending at the
            // end of every run that passes.
            let (a, s) = (app.downgrade(), seen.clone());
            at(t + 20_000, move || {
                s.borrow_mut().troubles.push("the run never ended".into());
                if let Some(a) = a.upgrade() {
                    a.quit();
                }
            });
        });
    }

    app.run_with_args::<&str>(&[]);

    if let Some(census) = at_quit.borrow().as_ref() {
        seen.borrow_mut().exits.push(census.exit(QUIT));
    }
    let s = seen.borrow();
    assert!(s.troubles.is_empty(), "a step could not run: {s:#?}");
    assert_eq!(s.exits.len(), 8, "every scenario has to report: {s:#?}");
    for e in &s.exits {
        assert!(
            e.widgets > 5 && e.controllers > 0,
            "{}: the census found almost nothing to watch ({} widgets, {} controllers), so \
             the dialog was probably never found: {s:#?}",
            e.name,
            e.widgets,
            e.controllers
        );
        assert!(
            e.widgets_alive == 0 && e.controllers_alive == 0,
            "{}: the dialog outlived its close — {} of {} widgets and {} of {} controllers. A \
             handler inside the window that holds the window is a cycle GTK never breaks; so is \
             a poll holding its widgets strongly. {s:#?}",
            e.name,
            e.widgets_alive,
            e.widgets,
            e.controllers_alive,
            e.controllers
        );
    }
    assert_eq!(
        s.agreed, 1,
        "the agree callback runs for the agree button and for nothing else: {s:#?}"
    );
    assert!(
        s.pitch_started,
        "Start sends the measurement and marks it running: {s:#?}"
    );
    assert_eq!(
        s.pitch_active_after_stop,
        Some(false),
        "Stop has to stop the measurement, not only close the window: {s:#?}"
    );
    assert_eq!(
        s.pitch_outcome_text.as_deref(),
        Some("Pitch zero set to -20.0°."),
        "a finished run shows its result: {s:#?}"
    );
}

/// The scenario that quits the program.
const QUIT: &str = "pitch: running when the program quits";

/// Weak references to one dialog and everything in it, taken while it was
/// open.
#[derive(Clone, Default)]
struct Census {
    win: Rc<RefCell<Option<gtk::glib::WeakRef<gtk::Window>>>>,
    ws: Rc<RefCell<Vec<gtk::glib::WeakRef<gtk::Widget>>>>,
    cs: Rc<RefCell<Vec<gtk::glib::WeakRef<gtk::EventController>>>>,
}

impl Census {
    /// What is left of it now.
    fn exit(&self, name: &'static str) -> Exit {
        Exit {
            name,
            widgets: self.ws.borrow().len(),
            controllers: self.cs.borrow().len(),
            widgets_alive: alive(&self.ws),
            controllers_alive: alive(&self.cs),
        }
    }
}

/// How many of `refs` still point at something.
fn alive<T: gtk::glib::object::ObjectType>(refs: &RefCell<Vec<gtk::glib::WeakRef<T>>>) -> usize {
    refs.borrow()
        .iter()
        .filter(|r| r.upgrade().is_some())
        .count()
}

/// Open a dialog at `t`, census it, and run `steps` at their offsets.
fn scenario(
    world: &Rc<World>,
    t: u64,
    name: &'static str,
    open: fn(&World) -> &'static str,
    steps: Vec<Step>,
) -> Census {
    let census = Census::default();
    let Census { win, ws, cs } = census.clone();
    {
        let (world, win) = (world.clone(), win.clone());
        at(t, move || {
            let title = open(&world);
            let Some(w) = gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Window>().ok())
                .find(|w| w.is_visible() && w.title().as_deref() == Some(title))
            else {
                world
                    .seen
                    .borrow_mut()
                    .troubles
                    .push(format!("{name}: no window titled {title:?}"));
                return;
            };
            let all = widgets(w.upcast_ref());
            *cs.borrow_mut() = controllers_of(&all).iter().map(|c| c.downgrade()).collect();
            *ws.borrow_mut() = all.iter().map(|w| w.downgrade()).collect();
            *win.borrow_mut() = Some(w.downgrade());
        });
    }
    for (offset, step) in steps {
        let (world, win) = (world.clone(), win.clone());
        at(t + offset, move || {
            let Some(w) = win.borrow().as_ref().and_then(|w| w.upgrade()) else {
                world.seen.borrow_mut().troubles.push(format!(
                    "{name}: the dialog was gone before a step at +{offset} ms"
                ));
                return;
            };
            step(&world, &w);
        });
    }
    census
}

/// A step that clicks the button showing `text`.
fn click(text: &'static str) -> Act {
    Box::new(
        move |w: &World, win: &gtk::Window| match button_labelled(win.upcast_ref(), text) {
            Some(b) => b.emit_clicked(),
            None => w
                .seen
                .borrow_mut()
                .troubles
                .push(format!("no button {text:?} in {:?}", win.title())),
        },
    )
}

/// A step that presses Escape on the window's own key controllers, the way the
/// window sees it.
fn escape() -> Act {
    Box::new(|_: &World, win: &gtk::Window| {
        let controllers = win.observe_controllers();
        for i in 0..controllers.n_items() {
            if let Some(keys) = controllers
                .item(i)
                .and_downcast::<gtk::EventControllerKey>()
            {
                let _ = keys.emit_by_name::<bool>(
                    "key-pressed",
                    &[
                        &gtk::gdk::Key::Escape.into_glib(),
                        &0u32,
                        &gtk::gdk::ModifierType::empty(),
                    ],
                );
            }
        }
    })
}

/// `root` and everything under it.
fn widgets(root: &gtk::Widget) -> Vec<gtk::Widget> {
    fn go(w: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
        out.push(w.clone());
        let mut c = w.first_child();
        while let Some(ch) = c {
            go(&ch, out);
            c = ch.next_sibling();
        }
    }
    let mut v = Vec::new();
    go(root, &mut v);
    v
}

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

/// Every label's text under `root`.
fn labels(root: &gtk::Widget) -> Vec<String> {
    widgets(root)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .map(|l| l.text().to_string())
        .collect()
}

/// The button whose child label reads `text`, the way `widget::button` builds
/// one.
fn button_labelled(root: &gtk::Widget, text: &str) -> Option<gtk::Button> {
    widgets(root)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| {
            b.child()
                .and_downcast::<gtk::Label>()
                .is_some_and(|l| l.text() == text)
        })
}

/// Run `f` once, `ms` into the timeline.
fn at<F: FnOnce() + 'static>(ms: u64, f: F) {
    gtk::glib::timeout_add_local_once(Duration::from_millis(ms), f);
}
