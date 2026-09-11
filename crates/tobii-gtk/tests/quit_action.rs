//! The hub's `quit` action ends the program through the hub's own teardown, also
//! when the hub is hidden to the tray — the state `tobii uninstall` and the update
//! banner's Quit find it in.
//!
//! Needs a display, so it is ignored by default:
//!
//!     cargo test -p tobii-gtk --test quit_action -- --ignored
//!
//! No tracker is needed: what is asserted is that the run loop returns, and the
//! device thread's list of claims at shutdown, which is state, not hardware.

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

#[test]
#[ignore = "needs a display"]
fn quit_ends_a_hub_hidden_to_the_tray_and_releases_its_claims() {
    let app = gtk::Application::builder()
        .application_id("dev.tobii.test.quit")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let at_shutdown: Rc<RefCell<Option<Vec<&'static str>>>> = Rc::default();
    let timed_out = Rc::new(Cell::new(false));
    {
        let (at_shutdown, timed_out) = (at_shutdown.clone(), timed_out.clone());
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
            // Hidden, as closing to the tray leaves it — then quit from outside.
            let h = hub.clone();
            gtk::glib::timeout_add_local_once(Duration::from_millis(700), move || {
                h.set_visible(false)
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
    let claims = at_shutdown.borrow().clone().expect("shutdown ran");
    assert!(claims.is_empty(), "claims left at shutdown: {claims:?}");
}
