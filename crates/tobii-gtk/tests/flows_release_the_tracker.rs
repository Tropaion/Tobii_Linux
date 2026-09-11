//! The fullscreen flows must give the tracker back when their window closes.
//!
//! Needs a display, so it is ignored by default (CI has none). Run it with:
//!
//!     cargo test -p tobii-gtk --test flows_release_the_tracker -- --ignored
//!
//! No tracker is needed: what is asserted is the device thread's list of claim
//! reasons, which is state, not hardware. Nothing the user has saved is touched —
//! the calibration flow is closed at its eye-preview step, before any session
//! begins, and the display-setup flow is cancelled before anything is written.
//!
//! This is the regression the v0.3.0 bug report found: closing either flow left
//! its claim in place for the life of the process, so after one calibration the
//! tracker never went dark again however the hub was minimised or unfocused. The
//! windows were never finalized — each had a key controller and buttons whose
//! handlers held the window strongly — and the claim was released only when the
//! window was finalized.

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

fn walk(w: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
    out.push(w.clone());
    let mut c = w.first_child();
    while let Some(ch) = c {
        walk(&ch, out);
        c = ch.next_sibling();
    }
}

/// The button whose label reads `text` — the hub's real button, clicked the way
/// a user would, so the whole path from the click to the claim is exercised.
fn button_labelled(root: &gtk::Widget, text: &str) -> gtk::Button {
    let mut all = Vec::new();
    walk(root, &mut all);
    let mut w = all
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

#[derive(Debug)]
struct Seen {
    flow: &'static str,
    /// Whether the test itself kept a strong reference across the close.
    held: bool,
    claims_after_close: Vec<&'static str>,
    window_alive_after_close: bool,
}

#[test]
#[ignore = "needs a display"]
fn closing_a_flow_releases_its_tracker_claim_and_frees_its_window() {
    let app = gtk::Application::builder()
        .application_id("dev.tobii.test.flows")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let seen: Rc<RefCell<Vec<Seen>>> = Rc::default();
    // At test scope, not inside the activate handler: its clones otherwise live
    // only in the one-shot close callback, which is dropped once it has run —
    // and the "held" window was freed before anything checked it.
    let keep: Rc<RefCell<Vec<gtk::Window>>> = Rc::default();
    {
        let seen = seen.clone();
        let keep = keep.clone();
        app.connect_activate(move |app| {
            tobii_gtk::load_css();
            let session = tobii_gtk::device::spawn();
            let demand = session.2.clone();
            let hub = tobii_gtk::build_hub(app, session).expect("hub");
            hub.present();

            // (open at, close at, check at, button label, the claim it takes,
            // whether to keep the window alive across the close)
            //
            // The third run holds the window on purpose. The first two prove
            // the flows no longer reference themselves; that one proves the
            // claim does not depend on it — a window kept alive by anything at
            // all must still give the tracker back when it closes. Without it,
            // the next self-reference anyone adds would bring the bug back with
            // every other assertion here still passing.
            let flows = [
                (
                    600u64,
                    1600u64,
                    2400u64,
                    "Improve calibration",
                    "calibration",
                    false,
                ),
                (3000, 4000, 4800, "Set up display", "display setup", false),
                (5400, 6400, 7200, "Improve calibration", "calibration", true),
            ];
            for (open, close, check, label, flow, held) in flows {
                let weak: Rc<RefCell<Option<gtk::glib::WeakRef<gtk::Window>>>> = Rc::default();
                let (h, w) = (hub.clone(), weak.clone());
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(open),
                    move || {
                        button_labelled(h.upcast_ref(), label).emit_clicked();
                    },
                );
                let (h, a, w2, k) = (hub.clone(), app.clone(), weak.clone(), keep.clone());
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(close),
                    move || {
                        let flow_win = a
                            .windows()
                            .into_iter()
                            .find(|x| x != h.upcast_ref::<gtk::Window>())
                            .unwrap_or_else(|| panic!("{flow} opened no window"));
                        *w2.borrow_mut() = Some(flow_win.downgrade());
                        if held {
                            k.borrow_mut().push(flow_win.clone());
                        }
                        flow_win.close();
                    },
                );
                let (d, s) = (demand.clone(), seen.clone());
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(check),
                    move || {
                        s.borrow_mut().push(Seen {
                            flow,
                            held,
                            claims_after_close: d.reasons(),
                            window_alive_after_close: w
                                .borrow()
                                .as_ref()
                                .and_then(|w| w.upgrade())
                                .is_some(),
                        });
                    },
                );
            }
            let a = app.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(7800), move || {
                a.quit()
            });
        });
    }
    app.run_with_args::<&str>(&[]);

    let seen = seen.borrow();
    assert_eq!(seen.len(), 3, "every run was exercised: {seen:?}");
    for s in seen.iter() {
        assert!(
            !s.claims_after_close.contains(&s.flow),
            "closing the {} flow left its claim on the tracker (held={}): {:?}",
            s.flow,
            s.held,
            s.claims_after_close
        );
        if s.held {
            // Otherwise this run proves nothing about the claim's independence.
            assert!(
                s.window_alive_after_close,
                "the held {} window died anyway",
                s.flow
            );
        } else {
            assert!(
                !s.window_alive_after_close,
                "the {} window was never freed after closing",
                s.flow
            );
        }
    }
}
