//! The Games tab re-reads the settings it told you to go and change.
//!
//! Needs a display, so it is ignored by default (CI has none). Run it with:
//!
//!     cargo test -p tobii-gtk --test games_tab_refreshes -- --ignored
//!
//! and run it in a **nested** compositor rather than on the session display:
//!
//!     kwin_wayland --virtual --width 1600 --height 1200 --socket wl-gr &
//!     WAYLAND_DISPLAY=wl-gr GDK_BACKEND=wayland cargo test -p tobii-gtk \
//!         --test games_tab_refreshes -- --ignored
//!
//! No tracker is needed, and nothing the person running this has saved is
//! touched: `XDG_CONFIG_HOME` is redirected to a directory of this test's own
//! before anything reads a path, the Steam walk is pointed at an empty home so
//! it finds nothing, and the one game on the page is made up here.
//!
//! # What this is for
//!
//! Block 1 of the Games tab reports settings the **other** tab owns, and when
//! game output is off it says so in the imperative: *turning it on, on the
//! Tracker tab, starts all of that*. That sentence is an instruction to leave
//! this page, do something, and come back — and until the tab came into view
//! and re-read, coming back showed the same sentence, now false, with nothing
//! on screen looking wrong.
//!
//! The window this tab replaced could not have that bug: it was modal over the
//! card it was reporting, so the settings could not change while it was up.
//! Losing the modality is what made the re-read necessary, which is why the
//! check lives here rather than beside the paragraph's own unit tests — those
//! ask what the words say, and this asks whether anybody went back for them.

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tobii_gtk::device::JoystickStatus;
use tobii_gtk::game_setup;
use tobii_output::games::games_path;

/// The other page of the stack. Its only job is to be somewhere else, so that
/// showing it unmaps the tab under test the way the hub's Tracker tab does.
const AWAY: &str = "away";
const TAB: &str = "games";

fn all(root: &gtk::Widget) -> Vec<gtk::Widget> {
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

/// The paragraph block 1 writes, wherever on the page it ended up.
///
/// By its own first words rather than by a widget name: what is being checked
/// is the sentence a reader sees, and a label found by name that had stopped
/// carrying that sentence would still be found.
fn block_one(root: &gtk::Widget) -> Option<String> {
    all(root)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .map(|l| l.text().to_string())
        .find(|t| t.starts_with("Head tracking for games is"))
}

#[derive(Debug, Default)]
struct Seen {
    /// What block 1 said with the file it was built over.
    before: Option<String>,
    /// And after the file changed while another page was showing.
    after: Option<String>,
    /// How many rows the picker offered — the premise for activating one.
    rows: usize,
    troubles: Vec<String>,
}

#[test]
#[ignore = "needs a display"]
fn the_games_tab_re_reads_the_settings_when_it_comes_back_into_view() {
    // A config home of its own, set before anything reads a path and never
    // restored: the process is the test. This one writes `games.toml`, and it
    // must not be the `games.toml` of whoever is running the tests.
    let dir = std::env::temp_dir().join(format!("tobii-gamestab-refresh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(tobii_config::paths::APP_DIR)).unwrap();
    // SAFETY: single-threaded, before GTK is initialised and before anything
    // in this process has resolved a path.
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    let games = games_path();

    // Off, with two destinations already configured — which is the state whose
    // sentence sends the reader to the other tab. Hand-written and minimal:
    // `OutputConfig::from_toml` ignores every key outside `[games]`.
    std::fs::write(
        &games,
        "[games]\nenabled = false\njoystick = true\nopentrack = \"127.0.0.1:4242\"\n",
    )
    .unwrap();

    // An empty home, so the Steam walk finds no library and every prefix
    // question on the page answers "none" without touching a real install.
    let empty_home = dir.join("home");
    std::fs::create_dir_all(&empty_home).unwrap();
    // A struct literal since `Scan` stopped having a private field: the prefix
    // memo it existed to keep private turned out never to have had a hit, and
    // went with the four-group list.
    let scan = game_setup::Scan {
        profiles_dir: dir.join("profiles"),
        apps: vec![tobii_steam::App {
            appid: "1".to_string(),
            name: "A Game".to_string(),
            buildid: None,
        }],
        steam: Rc::new(tobii_steam::Steam::at(&empty_home)),
        // A file of this test's own. The tab can WRITE this one, and a test
        // that let it reach `$XDG_CONFIG_HOME` would be a test that edits the
        // list of whoever runs the suite — which `Scan` exists to prevent for
        // every other input the tab has.
        custom_games: dir.join("custom-games.tsv"),
    };

    let app = gtk::Application::builder()
        .application_id("dev.tobii.test.gamestabrefresh")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let seen: Rc<RefCell<Seen>> = Rc::default();
    let scan = RefCell::new(Some(scan));

    {
        let (seen, games) = (seen.clone(), games.clone());
        app.connect_activate(move |app| {
            tobii_gtk::load_css();
            let Some(scan) = scan.borrow_mut().take() else {
                return;
            };
            let tab = game_setup::build_with(scan, Arc::new(Mutex::new(JoystickStatus::Off)));
            let root = tab.root.clone();

            // A stack, and not `set_visible` on the tab itself: what has to be
            // reproduced is the hub's own mechanism, and "a GtkStack unmaps the
            // page you switched away from" is exactly the assumption the fix
            // rests on.
            let stack = gtk::Stack::new();
            stack.add_titled(&root, Some(TAB), "Games");
            stack.add_titled(&gtk::Label::new(Some("elsewhere")), Some(AWAY), "Away");
            let win = gtk::ApplicationWindow::builder()
                .application(app)
                .default_width(900)
                .default_height(700)
                .child(&stack)
                .build();
            win.present();

            // ---- pick the game. Block 1 is written per game, so there is no
            //      block 1 at all until a row is activated.
            {
                let (s, r) = (seen.clone(), root.clone());
                let (st, games) = (stack.clone(), games.clone());
                gtk::glib::timeout_add_local_once(Duration::from_millis(300), move || {
                    let mut s = s.borrow_mut();
                    let Some(list) = all(r.upcast_ref())
                        .into_iter()
                        .find_map(|w| w.downcast::<gtk::ListBox>().ok())
                    else {
                        s.troubles.push("the tab has no picker".to_string());
                        return;
                    };
                    let mut rows = 0;
                    while list.row_at_index(rows).is_some() {
                        rows += 1;
                    }
                    s.rows = rows as usize;
                    let Some(row) = list.row_at_index(0) else {
                        s.troubles.push("the picker is empty".to_string());
                        return;
                    };
                    list.emit_by_name::<()>("row-activated", &[&row]);
                    s.before = block_one(r.upcast_ref());
                    // Now the other tab's switch, from outside — which is what
                    // the sentence just told the reader to go and do.
                    if let Err(e) = std::fs::write(
                        &games,
                        "[games]\nenabled = true\njoystick = true\nopentrack = \"127.0.0.1:4242\"\n",
                    ) {
                        s.troubles.push(format!("could not rewrite games.toml: {e}"));
                        return;
                    }
                    st.set_visible_child_name(AWAY);
                });
            }

            // ---- and back.
            {
                let st = stack.clone();
                gtk::glib::timeout_add_local_once(Duration::from_millis(600), move || {
                    st.set_visible_child_name(TAB);
                });
            }

            {
                let (s, r) = (seen.clone(), root.clone());
                gtk::glib::timeout_add_local_once(Duration::from_millis(900), move || {
                    s.borrow_mut().after = block_one(r.upcast_ref());
                });
            }

            {
                let a = app.clone();
                gtk::glib::timeout_add_local_once(Duration::from_millis(1200), move || a.quit());
            }
        });
    }

    app.run_with_args::<&str>(&[]);
    let _ = std::fs::remove_dir_all(&dir);

    let seen = seen.borrow();
    assert!(
        seen.troubles.is_empty(),
        "a step could not run: {:?}\n{seen:#?}",
        seen.troubles
    );
    assert_eq!(seen.rows, 1, "one made-up game, one row: {seen:#?}");
    let before = seen.before.as_deref().unwrap_or("");
    let after = seen.after.as_deref().unwrap_or("");
    assert!(
        before.starts_with("Head tracking for games is off."),
        "the premise: the page was built over a file that says off, and the \
         sentence under test only exists in that state: {seen:#?}"
    );
    assert!(
        before.contains("turning it on, on the Tracker tab"),
        "and it is the sentence that sends the reader to the other tab — without \
         it there is nothing here to come back to: {seen:#?}"
    );
    assert!(
        after.starts_with("Head tracking for games is on,"),
        "the reader did what block 1 told them to and came back to the same \
         paragraph, which is now false: {seen:#?}"
    );
}
