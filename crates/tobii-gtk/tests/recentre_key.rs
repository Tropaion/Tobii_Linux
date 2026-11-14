//! On COSMIC, the recentre-key button shows the key COSMIC's file holds, and
//! follows the file when something else changes it — COSMIC Settings, which
//! writes the same file.
//!
//! Needs a display, so it is ignored by default (CI has none). Run it with:
//!
//!     scripts/display-tests.sh recentre_key
//!
//! which starts a nested KWin that cannot touch your session; see the script.
//!
//! **It writes COSMIC's shortcut file, so it runs against a configuration
//! directory of its own**: `XDG_CONFIG_HOME` points at a fresh directory under
//! the system's temporary one, and `XDG_CURRENT_DESKTOP` says COSMIC, before
//! anything reads either. It builds the games row alone, not the hub, so
//! nothing asks for the tracker.

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use tobii_config::cosmic_shortcut::{self, Binding};

#[test]
#[ignore = "needs a display"]
fn the_cosmic_key_button_follows_the_file() {
    let dir = std::env::temp_dir().join(format!("tobii-recentre-key-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch config directory");
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    std::env::set_var("XDG_CURRENT_DESKTOP", "COSMIC");
    let custom = cosmic_shortcut::custom_path();
    assert!(
        custom.starts_with(&dir),
        "premise: {custom:?} is the scratch one"
    );
    // COSMIC's own settings directory, which a COSMIC session has.
    std::fs::create_dir_all(custom.parent().unwrap()).expect("COSMIC's directory");

    let app = gtk::Application::builder()
        .application_id("dev.tobii.test.recentrekey")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let labels: Rc<RefCell<Vec<String>>> = Rc::default();
    let troubles: Rc<RefCell<Vec<String>>> = Rc::default();

    {
        let (labels, troubles, custom) = (labels.clone(), troubles.clone(), custom.clone());
        app.connect_activate(move |app| {
            tobii_gtk::load_css();
            let (_state, _cmd, demand, joystick, recentring) = tobii_gtk::device::spawn();
            let row =
                tobii_gtk::games::GamesRow::build(joystick, recentring, demand, Rc::new(|| None));
            let win = gtk::ApplicationWindow::builder()
                .application(app)
                .child(&row.controls)
                .build();
            win.present();

            // What the button says at each moment, read off the window.
            let read = {
                let (win, labels, troubles) = (win.clone(), labels.clone(), troubles.clone());
                move || match key_button(win.upcast_ref()) {
                    Some(t) => labels.borrow_mut().push(t),
                    None => troubles.borrow_mut().push("no recentre-key button".into()),
                }
            };
            let r = read.clone();
            at(500, r);
            // COSMIC Settings binding a key: the file changes under the hub.
            let t = troubles.clone();
            at(600, move || {
                let b = Binding::new(&["Ctrl"], "F9");
                if let Err(e) = cosmic_shortcut::set(&b, "tobii recentre") {
                    t.borrow_mut().push(format!("could not set: {e}"));
                }
            });
            let r = read.clone();
            at(2000, r);
            // A file nobody can take apart: the button must not claim "none".
            let c = custom.clone();
            at(2100, move || {
                let _ = std::fs::write(&c, "{ not ron at all");
            });
            at(3500, read);
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

    assert!(troubles.borrow().is_empty(), "{:?}", troubles.borrow());
    assert_eq!(
        *labels.borrow(),
        ["Set key…", "Ctrl+F9", "Unknown…"],
        "the button shows the file's key, and follows the file when it changes"
    );
}

/// The label of the button that sets the recentre key: the one beside the
/// "Recentre key" label.
fn key_button(root: &gtk::Widget) -> Option<String> {
    let name = widgets(root).into_iter().find(|w| {
        w.downcast_ref::<gtk::Label>()
            .is_some_and(|l| l.text() == "Recentre key")
    })?;
    let button = name.next_sibling()?.downcast::<gtk::Button>().ok()?;
    let label = button.child()?.downcast::<gtk::Label>().ok()?;
    Some(label.text().to_string())
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

/// Run `f` once, `ms` into the timeline.
fn at<F: FnOnce() + 'static>(ms: u64, f: F) {
    gtk::glib::timeout_add_local_once(Duration::from_millis(ms), f);
}
