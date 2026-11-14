//! The Tuning window: what it writes and when, what it reads back, what its
//! tooltips owe the help window, and that it frees itself.
//!
//! Needs a display, so it is ignored by default (CI has none). Run it with:
//!
//!     scripts/display-tests.sh tuning_window
//!
//! which starts a nested KWin that cannot touch your session; see the script.
//!
//! **It writes a `games.toml`, so it runs against a configuration directory of
//! its own**: `XDG_CONFIG_HOME` points at a fresh directory under the system's
//! temporary one before anything reads it. Nothing in the person's own
//! configuration is read or written. No tracker is needed.

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use tobii_output::fusion::Curve;
use tobii_output::games::{load_output_config_from, save_output_config_to, OutputConfig};

#[derive(Debug, Default)]
struct Seen {
    /// Widgets and controllers weak-referenced while the window was open.
    widgets: usize,
    controllers: usize,
    widgets_alive: usize,
    controllers_alive: usize,
    /// Tooltips in the window that no help topic contains.
    uncovered: Vec<String>,
    tooltips: usize,
    /// The power curve the editor showed for an exponent beyond its slider.
    exponent_shown: Option<f64>,
    /// `filter_beta` in the file right after three quick changes, and once
    /// they have had time to be written.
    beta_before_write: Option<f64>,
    beta_after_write: Option<f64>,
    /// The yaw curve after a deadzone edit on that axis.
    curve_after_edit: Option<Curve>,
    deadzone_after_edit: Option<f64>,
    /// What the window showed after the file changed under it.
    beta_after_reread: Option<f64>,
    /// `filter_beta` in the file after a change made just before closing.
    beta_after_close: Option<f64>,
    /// The tracker claims while the window was open, and once it had closed.
    claims_open: Vec<&'static str>,
    claims_after_close: Vec<&'static str>,
    troubles: Vec<String>,
}

#[test]
#[ignore = "needs a display"]
fn the_tuning_window_writes_once_reads_back_explains_itself_and_frees_itself() {
    let dir = std::env::temp_dir().join(format!("tobii-tuning-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch config directory");
    // Before anything reads it: every path the program derives comes from here.
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    let games = tobii_output::games::games_path();
    assert!(
        games.starts_with(&dir),
        "premise: {games:?} is the scratch one"
    );
    // An exponent a terminal can set and the slider does not reach.
    let mut start = OutputConfig::default();
    start.head_curves.axes[0].curve = Curve::Power(8.0);
    save_output_config_to(&games, &start).expect("a starting games.toml");

    let app = gtk::Application::builder()
        .application_id("dev.tobii.test.tuningwindow")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let seen: Rc<RefCell<Seen>> = Rc::default();
    let ws: Rc<RefCell<Vec<gtk::glib::WeakRef<gtk::Widget>>>> = Rc::default();
    let cs: Rc<RefCell<Vec<gtk::glib::WeakRef<gtk::EventController>>>> = Rc::default();

    {
        let (seen, ws, cs, games) = (seen.clone(), ws.clone(), cs.clone(), games.clone());
        app.connect_activate(move |app| {
            tobii_gtk::load_css();
            let anchor = gtk::Button::with_label("Tuning…");
            let parent = gtk::ApplicationWindow::builder()
                .application(app)
                .child(&anchor)
                .build();
            parent.present();
            let demand = tobii_gtk::device::Demand::new();
            tobii_gtk::tuning::open(&anchor, Rc::new(|| None), &demand);

            let (s, ws, cs, d) = (seen.clone(), ws.clone(), cs.clone(), demand.clone());
            at(800, move || {
                let Some(win) = tuning() else {
                    s.borrow_mut().troubles.push("no Tuning window".into());
                    return;
                };
                let all = widgets(win.upcast_ref());
                // The census leaves out every subtree with a surface of its
                // own: GTK parents its one tooltip window into whichever
                // toplevel last showed a tooltip, and this window has dozens
                // (see `hub_lifetime.rs`'s `own_widgets`).
                let own = own_widgets(win.upcast_ref());
                *cs.borrow_mut() = controllers_of(&own).iter().map(|c| c.downgrade()).collect();
                *ws.borrow_mut() = own.iter().map(|w| w.downgrade()).collect();
                let help = tobii_gtk::help::topics()
                    .iter()
                    .map(tobii_gtk::help::Topic::text)
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let mut s = s.borrow_mut();
                // The program's own controls: GTK puts tooltips of its own,
                // in the desktop's language, inside a spin button.
                let ours = |w: &gtk::Widget| {
                    w.is::<gtk::Label>()
                        || w.is::<gtk::Scale>()
                        || w.is::<gtk::SpinButton>()
                        || w.is::<gtk::CheckButton>()
                };
                for w in all.iter().filter(|w| ours(w)) {
                    if let Some(t) = w.tooltip_text() {
                        s.tooltips += 1;
                        if !help.contains(t.as_str()) && !s.uncovered.contains(&t.to_string()) {
                            s.uncovered.push(t.to_string());
                        }
                    }
                }
                s.exponent_shown = spin(&win, "Above 1: gentle").map(|a| a.value());
                s.claims_open = d.reasons();
            });

            // Three quick changes: none of them written at once, all of them
            // as one write a moment later.
            let (s, g) = (seen.clone(), games.clone());
            at(1000, move || {
                let Some(beta) = tuning().and_then(|w| spin(&w, "How quickly the filter")) else {
                    s.borrow_mut()
                        .troubles
                        .push("no Speed response slider".into());
                    return;
                };
                for v in [0.05, 0.06, 0.07] {
                    beta.set_value(v);
                }
                s.borrow_mut().beta_before_write = Some(load_output_config_from(&g).filter_beta);
            });
            let (s, g) = (seen.clone(), games.clone());
            // Twice the wait, from the moment the first change was made.
            let written = 1000 + 2 * tobii_gtk::tuning::SAVE_AFTER.as_millis() as u64;
            at(written, move || {
                s.borrow_mut().beta_after_write = Some(load_output_config_from(&g).filter_beta);
            });

            // A curve-editor edit writes the whole axis from all of its
            // controls, so the exponent beyond the slider must survive it.
            let s = seen.clone();
            at(1700, move || {
                match tuning().and_then(|w| spin(&w, "Movement from centre")) {
                    Some(dz) => dz.set_value(1.0),
                    None => s.borrow_mut().troubles.push("no Deadzone slider".into()),
                }
            });
            let (s, g) = (seen.clone(), games.clone());
            at(2300, move || {
                let yaw = load_output_config_from(&g).head_curves.axes[0];
                let mut s = s.borrow_mut();
                s.curve_after_edit = Some(yaw.curve);
                s.deadzone_after_edit = Some(yaw.deadzone_deg);
            });

            // The file changes under the window — the Games row, a terminal —
            // and the window shows it, without its focus changing.
            let (s, g) = (seen.clone(), games.clone());
            at(2400, move || {
                // Past the slider's end (0.2), so it also shows that the
                // slider widens to what is set rather than clamping it.
                let mut c = load_output_config_from(&g);
                c.filter_beta = 0.5;
                if let Err(e) = save_output_config_to(&g, &c) {
                    s.borrow_mut()
                        .troubles
                        .push(format!("could not write: {e}"));
                }
            });
            let s = seen.clone();
            at(2700, move || {
                s.borrow_mut().beta_after_reread = tuning()
                    .and_then(|w| spin(&w, "How quickly the filter"))
                    .map(|a| a.value());
            });

            // A change made just before closing is written by the close.
            let s = seen.clone();
            at(2800, move || {
                let Some(w) = tuning() else {
                    s.borrow_mut().troubles.push("the window went".into());
                    return;
                };
                if let Some(beta) = spin(&w, "How quickly the filter") {
                    beta.set_value(0.15);
                }
                w.close();
            });
            let (s, g) = (seen.clone(), games.clone());
            let d = demand.clone();
            at(3400, move || {
                let mut s = s.borrow_mut();
                s.beta_after_close = Some(load_output_config_from(&g).filter_beta);
                s.claims_after_close = d.reasons();
            });

            let a = app.downgrade();
            at(3600, move || {
                if let Some(a) = a.upgrade() {
                    a.quit();
                }
            });
        });
    }

    app.run_with_args::<&str>(&[]);
    let _ = std::fs::remove_dir_all(&dir);

    let mut s = seen.borrow_mut();
    s.widgets = ws.borrow().len();
    s.controllers = cs.borrow().len();
    s.widgets_alive = ws.borrow().iter().filter(|w| w.upgrade().is_some()).count();
    s.controllers_alive = cs.borrow().iter().filter(|c| c.upgrade().is_some()).count();
    let s = &*s;
    let default_beta = OutputConfig::default().filter_beta;

    assert!(s.troubles.is_empty(), "a step could not run: {s:#?}");
    assert!(
        s.tooltips >= 46 && s.uncovered.is_empty(),
        "every tooltip in the window has to be in the help window, which is where a keyboard \
         or touch user reads it — these are not: {:#?}\n{s:#?}",
        s.uncovered
    );
    assert_eq!(
        s.exponent_shown,
        Some(8.0),
        "an exponent beyond the slider is shown as it is: {s:#?}"
    );
    assert_eq!(
        s.beta_before_write,
        Some(default_beta),
        "a change is not written the moment it is made: {s:#?}"
    );
    assert_eq!(
        s.beta_after_write,
        Some(0.07),
        "and is written a moment later, last value first: {s:#?}"
    );
    assert_eq!(
        (s.curve_after_edit, s.deadzone_after_edit),
        (Some(Curve::Power(8.0)), Some(1.0)),
        "editing one control of an axis keeps the exponent beyond the slider: {s:#?}"
    );
    assert_eq!(
        s.beta_after_reread,
        Some(0.5),
        "a change made outside the window shows in it, as it is: {s:#?}"
    );
    assert_eq!(
        s.beta_after_close,
        Some(0.15),
        "the close writes what was not written yet: {s:#?}"
    );
    assert_eq!(
        (s.claims_open.as_slice(), s.claims_after_close.as_slice()),
        (&[tobii_gtk::tuning::CLAIM][..], &[][..]),
        "the window keeps the tracker on for its live dot, and only while open: {s:#?}"
    );
    assert!(
        s.widgets > 50 && s.controllers > 10,
        "the census found almost nothing to watch: {s:#?}"
    );
    assert_eq!(
        (s.widgets_alive, s.controllers_alive),
        (0, 0),
        "the window outlived its close: a handler holding something that holds it back, or a \
         poll holding its widgets (see tobii_gtk's crate docs, *Lifetimes*): {s:#?}"
    );
}

/// The open Tuning window.
fn tuning() -> Option<gtk::Window> {
    gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.is_visible() && w.title().as_deref() == Some(tobii_gtk::tuning::TITLE))
}

/// The adjustment of the spin button whose tooltip starts with `tip`.
fn spin(win: &gtk::Window, tip: &str) -> Option<gtk::Adjustment> {
    widgets(win.upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::SpinButton>().ok())
        .find(|s| s.tooltip_text().is_some_and(|t| t.starts_with(tip)))
        .map(|s| s.adjustment())
}

/// `root` and everything under it but the subtrees with a surface of their own.
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

/// Run `f` once, `ms` into the timeline.
fn at<F: FnOnce() + 'static>(ms: u64, f: F) {
    gtk::glib::timeout_add_local_once(Duration::from_millis(ms), f);
}
