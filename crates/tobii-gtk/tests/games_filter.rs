//! The Games list's filter hides rows, and the empty list says which of the two
//! things emptied it.
//!
//! Needs a display, so it is ignored by default (CI has none). Run it with:
//!
//!     cargo test -p tobii-gtk --test games_filter -- --ignored
//!
//! in a **nested** compositor rather than on the session display, or through
//! `scripts/display-tests.sh`, which does both.
//!
//! Nothing the person running it has saved is touched: `XDG_CONFIG_HOME` is
//! redirected to a directory of this test's own before anything reads a path,
//! the Steam walk is pointed at an empty home so it finds nothing, and the one
//! set-up game is a prefix this test creates and fills.
//!
//! # What this is for, and why it cannot be a unit test
//!
//! `picker` decides what the list *should* show and is asserted against
//! directly in `game_setup`'s own tests. What those cannot reach is the two
//! GTK facts the rewrite rests on:
//!
//! * `set_filter_func` hides a row without removing it, so `row_at_index` goes
//!   on finding every row and only `is_child_visible` tells them apart. Every
//!   count taken with `row_at_index` — there is one in `games_tab_refreshes` —
//!   is therefore a count of the whole catalogue from now on, which is a test
//!   that passes while measuring nothing if it was meant to follow the search.
//! * The dim is a CSS class on the `GtkListBoxRow`, applied where the rows are
//!   built. `picker` cannot see it, so nothing headless can: delete the line
//!   that adds it and every row draws at full strength — the whole premise of
//!   collapsing four sections into one — with the entire suite still green.
//! * The paragraph that explains an empty list is this program's own label
//!   beside the list, shown and hidden by `restate`. `picker` decides what it
//!   says and is unit-tested; that pressing the toggle and typing into the
//!   search entry actually *reach* `placeholder.set_visible(!showing)` through
//!   a real widget tree is only true if somebody drives one.
//!
//! The list deliberately does NOT use `GtkListBox::set_placeholder`, and that is
//! why the last of those is checkable at all: GTK parents that widget where
//! `first_child`/`next_sibling` cannot reach, so no test outside the crate could
//! ask whether it was showing. A window that prints a paragraph explaining why
//! it looks empty should not rest on a claim about a toolkit that nothing here
//! can check.

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tobii_gtk::device::JoystickStatus;
use tobii_gtk::game_setup;

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

fn named<T: IsA<gtk::Widget>>(root: &gtk::Widget, name: &str) -> Option<T> {
    all(root)
        .into_iter()
        .find(|w| w.widget_name() == name)
        .and_then(|w| w.downcast::<T>().ok())
}

/// Every row: its name, whether the filter is letting it through, and whether
/// it is drawn back.
///
/// The dim is the third thing and the one with nowhere else to be asserted. It
/// is a CSS class put on the `GtkListBoxRow` in the widget-build path, so
/// `picker` cannot see it and no unit test can: delete the line that adds it
/// and every row draws at full strength, which is the whole premise of
/// collapsing four sections into one, with the entire suite still green.
fn rows(list: &gtk::ListBox) -> Vec<(String, bool, bool)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(row) = list.row_at_index(i) {
        // The name label is the first of the two, and reading it is what makes
        // a failure say WHICH row went missing rather than how many did.
        let name = row
            .child()
            .and_then(|b| b.first_child())
            .and_downcast::<gtk::Label>()
            .map(|l| l.text().to_string())
            .unwrap_or_default();
        out.push((
            name,
            row.is_child_visible(),
            row.has_css_class("not-set-up"),
        ));
        i += 1;
    }
    out
}

#[derive(Debug, Default)]
struct Seen {
    before: Vec<(String, bool, bool)>,
    filtered: Vec<(String, bool, bool)>,
    filtered_and_typed: Vec<(String, bool, bool)>,
    placeholder_when_some_show: Option<bool>,
    placeholder_when_none_show: Option<bool>,
    placeholder_text: String,
    troubles: Vec<String>,
}

#[test]
#[ignore = "needs a display"]
fn the_filter_hides_rows_without_removing_them_and_an_empty_list_says_why() {
    let dir = std::env::temp_dir().join(format!("tobii-games-filter-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    // Before anything in the hub reads a path.
    std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));

    // One game added by hand, whose prefix this test fills with the artifact
    // the list reads — so it is the row that IS set up, without needing a Steam
    // library or a real Proton prefix anywhere.
    let pfx = dir.join("pfx");
    std::fs::create_dir_all(pfx.join("drive_c/tobii-bridge")).unwrap();
    std::fs::write(pfx.join("drive_c/tobii-bridge/freetrackclient64.dll"), b"x").unwrap();
    let custom = dir.join("custom-games.tsv");
    std::fs::write(&custom, format!("Set Up Game\t{}\n", pfx.display())).unwrap();

    let empty_home = dir.join("home");
    std::fs::create_dir_all(&empty_home).unwrap();
    let scan = game_setup::Scan {
        profiles_dir: dir.join("profiles"),
        apps: vec![
            tobii_steam::App {
                appid: "1".to_string(),
                name: "Never Launched".to_string(),
                buildid: None,
            },
            tobii_steam::App {
                appid: "2".to_string(),
                name: "Also Not Set Up".to_string(),
                buildid: None,
            },
        ],
        steam: Rc::new(tobii_steam::Steam::at(&empty_home)),
        custom_games: custom,
    };

    let app = gtk::Application::builder()
        .application_id("dev.tobii.test.gamesfilter")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let seen: Rc<RefCell<Seen>> = Rc::default();
    let scan = RefCell::new(Some(scan));

    {
        let seen = seen.clone();
        app.connect_activate(move |app| {
            tobii_gtk::load_css();
            let Some(scan) = scan.borrow_mut().take() else {
                return;
            };
            let tab = game_setup::build_with(scan, Arc::new(Mutex::new(JoystickStatus::Off)));
            let root = tab.root.clone();
            let win = gtk::ApplicationWindow::builder()
                .application(app)
                .default_width(1000)
                .default_height(700)
                .child(&root)
                .build();
            win.present();

            {
                let (s, r) = (seen.clone(), root.clone());
                gtk::glib::timeout_add_local_once(Duration::from_millis(400), move || {
                    let mut s = s.borrow_mut();
                    let (Some(list), Some(toggle)) = (
                        named::<gtk::ListBox>(r.upcast_ref(), game_setup::LIST_NAME),
                        named::<gtk::ToggleButton>(r.upcast_ref(), game_setup::SET_UP_ONLY_NAME),
                    ) else {
                        s.troubles.push("no list, or no filter beside it".into());
                        return;
                    };
                    s.before = rows(&list);
                    // Down it goes, which is all a user does.
                    toggle.set_active(true);
                    s.filtered = rows(&list);
                    s.placeholder_when_some_show =
                        named::<gtk::Label>(r.upcast_ref(), game_setup::PLACEHOLDER_NAME)
                            .map(|p| p.is_visible());
                });
            }
            {
                let (s, r) = (seen.clone(), root.clone());
                gtk::glib::timeout_add_local_once(Duration::from_millis(700), move || {
                    let mut s = s.borrow_mut();
                    let Some(search) =
                        named::<gtk::SearchEntry>(r.upcast_ref(), game_setup::SEARCH_NAME)
                    else {
                        s.troubles.push("no search box".into());
                        return;
                    };
                    // The filter is still down; now type something no set-up
                    // game is called, which is the state that empties the list
                    // while every row is still in it. `GtkSearchEntry` holds
                    // `search-changed` back for a moment of its own, so typing
                    // and looking have to be two steps: doing both here read
                    // the list as it was BEFORE the keystroke, and passed while
                    // measuring nothing.
                    search.set_text("zzzz");
                });
            }

            {
                let (s, r) = (seen.clone(), root.clone());
                gtk::glib::timeout_add_local_once(Duration::from_millis(1100), move || {
                    let mut s = s.borrow_mut();
                    let Some(list) = named::<gtk::ListBox>(r.upcast_ref(), game_setup::LIST_NAME)
                    else {
                        s.troubles.push("no list on the second look".into());
                        return;
                    };
                    s.filtered_and_typed = rows(&list);
                    let ph = named::<gtk::Label>(r.upcast_ref(), game_setup::PLACEHOLDER_NAME);
                    s.placeholder_when_none_show = ph.as_ref().map(|p| p.is_visible());
                    s.placeholder_text = ph.map(|l| l.text().to_string()).unwrap_or_default();
                });
            }

            let a = app.clone();
            gtk::glib::timeout_add_local_once(Duration::from_millis(1500), move || a.quit());
            let a = app.clone();
            gtk::glib::timeout_add_local_once(Duration::from_secs(8), move || a.quit());
        });
    }
    app.run_with_args::<&str>(&[]);
    let s = seen.borrow();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(s.troubles.is_empty(), "{:?}\n{s:#?}", s.troubles);

    let showing = |v: &[(String, bool, bool)]| -> Vec<String> {
        v.iter()
            .filter(|(_, on, _)| *on)
            .map(|(n, _, _)| n.clone())
            .collect()
    };
    let dimmed = |v: &[(String, bool, bool)]| -> Vec<String> {
        v.iter()
            .filter(|(_, _, dim)| *dim)
            .map(|(n, _, _)| n.clone())
            .collect()
    };

    assert_eq!(
        showing(&s.before),
        vec![
            "Also Not Set Up".to_string(),
            "Never Launched".to_string(),
            "Set Up Game".to_string()
        ],
        "with the filter up, one alphabetical run of everything: {s:#?}"
    );
    assert_eq!(
        showing(&s.filtered),
        vec!["Set Up Game".to_string()],
        "with it down, only the prefix holding the artifact: {s:#?}"
    );
    // The point of the file: the rows are still THERE.
    assert_eq!(
        s.filtered.len(),
        3,
        "a hidden row is still a row, and `row_at_index` still finds it — every count \
         taken that way counts the whole catalogue: {s:#?}"
    );
    // The dim, which is what the list says instead of a heading. Asserted over
    // the same census as the filter so the two cannot disagree: a row the filter
    // keeps is a row that is NOT drawn back, and that equivalence is the whole
    // of what "one list" means here.
    assert_eq!(
        dimmed(&s.before),
        vec!["Also Not Set Up".to_string(), "Never Launched".to_string()],
        "everything without a bridge in its prefix is drawn back, and the one with a \
         bridge is not: {s:#?}"
    );
    assert_eq!(
        dimmed(&s.filtered),
        dimmed(&s.before),
        "pressing the filter hides rows; it must not repaint the ones it keeps: {s:#?}"
    );

    assert_eq!(
        s.placeholder_when_some_show,
        Some(false),
        "something is showing, so the paragraph explaining an empty list must not be: {s:#?}"
    );

    assert!(
        showing(&s.filtered_and_typed).is_empty(),
        "nothing is called zzzz: {s:#?}"
    );
    assert_eq!(
        s.placeholder_when_none_show,
        Some(true),
        "the list is empty because the filter emptied it, so the paragraph saying why has \
         to be up — if this failed, `restate` is not reaching `placeholder.set_visible`, \
         or `picker` and the filter disagree about whether anything is showing: {s:#?}"
    );
    assert!(
        s.placeholder_text.contains("zzzz") && s.placeholder_text.contains("button beside"),
        "and it names both of the things that emptied it: {:?}",
        s.placeholder_text
    );
}
