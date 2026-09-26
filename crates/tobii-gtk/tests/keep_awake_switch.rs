//! The cogwheel's "Keep the tracker awake" switch and the header's ALWAYS ON
//! badge are the same bit as `tobii games set keep_awake` — in both directions,
//! and while the hub is open.
//!
//! Needs a display, so it is ignored by default:
//!
//!     cargo test -p tobii-gtk --test keep_awake_switch -- --ignored
//!
//! No tracker is needed: what is asserted is what the hub reads out of
//! `games.toml` and what it writes back into it.
//!
//! This exists because the setting once had two stores — a one-line file for
//! the switch and the badge, `games.toml` for the device thread that actually
//! holds the tracker — and each half had a passing test against its own store.
//! The switch could light an ALWAYS ON badge over a tracker that went dark
//! three seconds later, and `tobii games set keep_awake true` could hold the
//! illuminators lit all night with the hub's own switch reading off. Only a
//! test that crosses the seam can see that, so this one drives the real widget
//! and reads the real file.

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use tobii_output::games::{games_path, load_output_config_from};

fn walk(w: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
    out.push(w.clone());
    // The cogwheel's panel is a popover, reached through its menu button, and
    // the switch under test lives in it.
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

/// The hub's widget of type `W` whose tooltip starts with `tooltip`.
fn by_tooltip<W: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>, tooltip: &str) -> W {
    let mut all = Vec::new();
    walk(root.upcast_ref(), &mut all);
    all.into_iter()
        .filter_map(|w| w.downcast::<W>().ok())
        .find(|w| {
            w.tooltip_text()
                .is_some_and(|t| t.as_str().starts_with(tooltip))
        })
        .unwrap_or_else(|| panic!("nothing with the tooltip {tooltip:?} in the hub"))
}

/// What the switch was seeded with, what it said after the file changed under
/// it, and what the file said after the switch was used.
#[derive(Default)]
struct Seen {
    seeded_off: Option<bool>,
    badge_hidden: Option<bool>,
    followed_the_file: Option<bool>,
    badge_shown: Option<bool>,
    file_after_refresh: Option<String>,
    file_after_the_switch: Option<bool>,
    badge_gone: Option<bool>,
}

#[test]
#[ignore = "needs a display"]
fn the_hub_and_the_cli_set_the_same_keep_awake() {
    // A config home of its own: this test turns a real setting on and off, and
    // it must not be the setting of whoever is running the tests. Set before
    // anything reads a path, and never restored — the process is the test.
    let dir = std::env::temp_dir().join(format!("tobii-keepawake-hub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(tobii_config::paths::APP_DIR)).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    let games = games_path();

    // Hand-written and minimal on purpose: `save_output_config_to` writes the
    // whole commented file, so these exact bytes surviving a refresh is what
    // proves the hub answered the change rather than writing its own idea back
    // over it. The `[games]` header is not optional — `OutputConfig::from_toml`
    // ignores every key outside it.
    const CLI_TURNED_IT_ON: &str = "[games]\nkeep_awake = true\n";
    std::fs::write(&games, "[games]\nkeep_awake = false\n").unwrap();

    let app = gtk::Application::builder()
        .application_id("dev.tobii.test.keepawake")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let seen: Rc<RefCell<Seen>> = Rc::default();
    let timed_out = Rc::new(Cell::new(false));
    {
        let (seen, timed_out, games) = (seen.clone(), timed_out.clone(), games.clone());
        app.connect_activate(move |app| {
            tobii_gtk::load_css();
            tobii_gtk::install_quit_action(app);
            let session = tobii_gtk::device::spawn();
            let hub = tobii_gtk::build_hub(app, session).expect("hub");
            hub.present();

            let switch: gtk::Switch = by_tooltip(&hub, "Only for setups nothing can detect");
            let badge: gtk::Label = by_tooltip(&hub, "Standby is off");

            // Seeded from the file, then changed from outside — which is what
            // `tobii games set keep_awake true` in a terminal looks like from
            // in here.
            let (s, sw, b, g) = (seen.clone(), switch.clone(), badge.clone(), games.clone());
            gtk::glib::timeout_add_local_once(Duration::from_millis(400), move || {
                let mut s = s.borrow_mut();
                s.seeded_off = Some(!sw.is_active());
                s.badge_hidden = Some(!b.is_visible());
                std::fs::write(&g, CLI_TURNED_IT_ON).unwrap();
            });

            // The hub tick has had a dozen goes at it by now.
            let (s, sw, b, g) = (seen.clone(), switch.clone(), badge.clone(), games.clone());
            gtk::glib::timeout_add_local_once(Duration::from_millis(900), move || {
                let mut s = s.borrow_mut();
                s.followed_the_file = Some(sw.is_active());
                s.badge_shown = Some(b.is_visible());
                s.file_after_refresh = std::fs::read_to_string(&g).ok();
                // Now the other direction: the user switches it off in the hub.
                sw.set_active(false);
            });

            let (s, b, g) = (seen.clone(), badge.clone(), games.clone());
            gtk::glib::timeout_add_local_once(Duration::from_millis(1300), move || {
                let mut s = s.borrow_mut();
                s.file_after_the_switch = Some(load_output_config_from(&g).keep_awake);
                s.badge_gone = Some(!b.is_visible());
            });

            let a = app.clone();
            gtk::glib::timeout_add_local_once(Duration::from_millis(1500), move || a.quit());
            // If any of that hangs, the test must still end, and fail.
            let (a, t) = (app.clone(), timed_out.clone());
            gtk::glib::timeout_add_local_once(Duration::from_secs(8), move || {
                t.set(true);
                a.quit();
            });
        });
    }
    app.run_with_args::<&str>(&[]);
    let seen = seen.borrow();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!timed_out.get(), "the hub never got through the checks");
    assert_eq!(seen.seeded_off, Some(true), "seeded from `games.toml`");
    assert_eq!(
        seen.badge_hidden,
        Some(true),
        "no badge for a setting that is off"
    );
    assert_eq!(
        seen.followed_the_file,
        Some(true),
        "`tobii games set keep_awake true` must reach the switch in the open hub"
    );
    assert_eq!(
        seen.badge_shown,
        Some(true),
        "and the badge, which is the only notice a lit illuminator gets"
    );
    assert_eq!(
        seen.file_after_refresh.as_deref(),
        Some(CLI_TURNED_IT_ON),
        "a refresh reports the file, it does not write the hub's idea back over it"
    );
    assert_eq!(
        seen.file_after_the_switch,
        Some(false),
        "the switch must write the field `GameSide::sync_keep_awake` holds the tracker by"
    );
    assert_eq!(
        seen.badge_gone,
        Some(true),
        "and the badge follows it back off"
    );
}
