//! The hub's `quit` action ends the program through the hub's own teardown, also
//! when the hub is hidden to the tray — the state `tobii uninstall` and the update
//! banner's Quit find it in — and with a calibration flow open, which is a state
//! `tobii uninstall` can find it in at any moment.
//!
//! Needs a display, so it is ignored by default:
//!
//!     cargo test -p tobii-gtk --test quit_action -- --ignored
//!
//! No tracker is needed: what is asserted is that the run loop returns, the
//! device thread's list of claims before the quit and at shutdown, and that the
//! flow's own close handler ran — state, not hardware. The calibration flow is
//! quit at its eye-preview step, a second after it opens, well before it could
//! begin a session.

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

fn walk(w: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
    out.push(w.clone());
    // The cogwheel's panel is a popover, reached through its menu button:
    // walked explicitly, so the Quit row is found whether or not GTK lists the
    // popover among the button's children. Visited twice when it does, which a
    // search does not mind.
    if let Some(p) = w
        .downcast_ref::<gtk::MenuButton>()
        .and_then(|m| m.popover())
    {
        walk(p.upcast_ref(), out);
    }
    let mut c = w.first_child();
    while let Some(ch) = c {
        walk(&ch, out);
        c = ch.next_sibling();
    }
}

fn all_widgets(root: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
    let mut all = Vec::new();
    walk(root.upcast_ref(), &mut all);
    all
}

/// The hub's widget of type `W` whose tooltip starts with `tooltip`.
fn by_tooltip<W: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>, tooltip: &str) -> W {
    all_widgets(root)
        .into_iter()
        .filter_map(|w| w.downcast::<W>().ok())
        .find(|w| {
            w.tooltip_text()
                .is_some_and(|t| t.as_str().starts_with(tooltip))
        })
        .unwrap_or_else(|| panic!("nothing with the tooltip {tooltip:?} in the hub"))
}

/// The button whose label reads `text`, clicked the way a user would.
fn button_labelled(root: &impl IsA<gtk::Widget>, text: &str) -> gtk::Button {
    let mut w = all_widgets(root)
        .into_iter()
        .find(|w| {
            w.downcast_ref::<gtk::Label>()
                .is_some_and(|l| l.text() == text)
        })
        .unwrap_or_else(|| panic!("no label {text:?} in the hub"));
    loop {
        if let Some(b) = w.downcast_ref::<gtk::Button>() {
            return b.clone();
        }
        w = w
            .parent()
            .unwrap_or_else(|| panic!("{text:?} is not inside a button"));
    }
}

#[test]
#[ignore = "needs a display"]
fn quit_ends_a_hub_hidden_to_the_tray_and_releases_its_claims() {
    let app = gtk::Application::builder()
        .application_id("dev.tobii.test.quit")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let at_shutdown: Rc<RefCell<Option<Vec<&'static str>>>> = Rc::default();
    let before: Rc<RefCell<Vec<&'static str>>> = Rc::default();
    let flow_closed = Rc::new(Cell::new(false));
    let quit_row_action: Rc<RefCell<Option<String>>> = Rc::default();
    let timed_out = Rc::new(Cell::new(false));
    {
        let (at_shutdown, before, flow_closed, quit_row_action, timed_out) = (
            at_shutdown.clone(),
            before.clone(),
            flow_closed.clone(),
            quit_row_action.clone(),
            timed_out.clone(),
        );
        app.connect_activate(move |app| {
            tobii_gtk::load_css();
            tobii_gtk::install_quit_action(app);
            let session = tobii_gtk::device::spawn();
            let demand = session.2.clone();
            let hub = tobii_gtk::build_hub(app, session).expect("hub");
            hub.present();
            {
                let (d, s) = (demand.clone(), at_shutdown.clone());
                app.connect_shutdown(move |_| *s.borrow_mut() = Some(d.reasons()));
            }
            // The cogwheel's Quit is the same action, by name: a name that does
            // not resolve leaves a button that silently does nothing, and no
            // other check here would notice.
            let quit_btn: gtk::Button = by_tooltip(&hub, "Exit completely.");
            *quit_row_action.borrow_mut() = quit_btn
                .action_name()
                .and_then(|n| n.as_str().strip_prefix("app.").map(str::to_owned))
                .filter(|n| app.lookup_action(n).is_some());

            // A calibration flow, opened from the hub's own button. Its close
            // handler is the one that aborts a calibration session; a quit that
            // skips it leaves that undone and the flow's claim standing.
            let h = hub.clone();
            gtk::glib::timeout_add_local_once(Duration::from_millis(300), move || {
                button_labelled(&h, "Improve calibration").emit_clicked()
            });
            // Then the gaze preview, after the flow: opening the flow switches
            // it off. It is the one claim that hiding keeps on purpose — the
            // close handler leaves the overlay alone — and that only the
            // teardown releases. Without a claim like that, hiding would already
            // have dropped every claim, and a quit that skipped the hub's
            // teardown (a bare `app.quit()`) would pass the check at shutdown.
            let (h, a, fc) = (hub.clone(), app.clone(), flow_closed.clone());
            gtk::glib::timeout_add_local_once(Duration::from_millis(500), move || {
                by_tooltip::<gtk::Switch>(&h, "Show a dot on screen where you're looking")
                    .set_active(true);
                let flow = a
                    .windows()
                    .into_iter()
                    .find(|w| w.title().as_deref() == Some("Calibration"))
                    .expect("the calibration flow opened");
                // Connected after the flow's own handler, so it runs when that
                // one has: what is recorded is that the close was asked for.
                flow.connect_close_request(move |_| {
                    fc.set(true);
                    gtk::glib::Propagation::Proceed
                });
            });
            // Hidden, as closing to the tray leaves it — then quit from outside.
            let h = hub.clone();
            gtk::glib::timeout_add_local_once(Duration::from_millis(700), move || {
                h.set_visible(false)
            });
            let (d, b) = (demand.clone(), before.clone());
            gtk::glib::timeout_add_local_once(Duration::from_millis(1300), move || {
                *b.borrow_mut() = d.reasons()
            });
            let a = app.clone();
            gtk::glib::timeout_add_local_once(Duration::from_millis(1400), move || {
                a.activate_action("quit", None)
            });
            // If the action does nothing, the test must still end, and fail.
            let (a, t) = (app.clone(), timed_out.clone());
            gtk::glib::timeout_add_local_once(Duration::from_secs(8), move || {
                t.set(true);
                a.quit();
            });
        });
    }
    app.run_with_args::<&str>(&[]);
    assert!(!timed_out.get(), "the quit action did not end the program");
    assert!(
        quit_row_action.borrow().is_some(),
        "the cogwheel's Quit button is not wired to the application's quit action"
    );
    // Otherwise the check at shutdown below proves nothing.
    let before = before.borrow();
    for reason in ["the gaze preview", "calibration"] {
        assert!(
            before.contains(&reason),
            "{reason:?} was not held while the hub was hidden: {before:?}"
        );
    }
    assert!(
        flow_closed.get(),
        "the quit ended the program without closing the calibration flow"
    );
    let claims = at_shutdown.borrow().clone().expect("shutdown ran");
    assert!(claims.is_empty(), "claims left at shutdown: {claims:?}");
}
