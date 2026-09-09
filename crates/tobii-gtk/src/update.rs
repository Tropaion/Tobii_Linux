//! The hub's update banner: check on launch, show the changelog, install.
//!
//! # The one automatic network request
//!
//! Everything else in this program asks first — the head-pose model store will
//! not so much as look at the network without an explicit click. This does,
//! once per launch, because knowing a fix exists is worthless if you have to
//! remember to go looking for it.
//!
//! What that buys is deliberately narrow: it fetches the release *listing*,
//! which is metadata. Nothing is downloaded and nothing on disk changes until
//! the user presses Update.
//!
//! It can be switched off — in Settings, or with `TOBII_NO_UPDATE_CHECK=1` —
//! and then no request is made at all. A program that reaches out on its own
//! needs a way to say no that is not "stop using the program".
//!
//! # What pressing Update trusts
//!
//! The checksums are published in the same release as the archive and fetched
//! over the same connection, so they catch a *corrupted download* and nothing
//! more. There is no signature, so installing an update trusts the project's
//! GitHub release exactly as much as downloading a binary from it by hand
//! would. The dialog says so before the button is pressed, rather than implying
//! a guarantee that does not exist. See `tobii_update::install`.
//!
//! The check runs on a worker thread and the banner stays hidden until it has
//! something to say, so a slow or absent network costs the hub nothing.

use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Label, Orientation};

use tobii_update::release::{Blocked, Check, Release};

/// How the banner reads for a release.
pub fn headline(r: &Release) -> String {
    format!("Version {} is available.", r.version)
}

/// The changelog, or an honest stand-in when a release has none.
pub fn changelog(r: &Release) -> String {
    let notes = r.notes.trim();
    if notes.is_empty() {
        format!(
            "{} was published without release notes.\n\nThe full list of changes is on the \
             releases page.",
            r.version
        )
    } else {
        notes.to_string()
    }
}

/// Progress wording while an install runs.
pub fn installing(step: &str) -> String {
    format!("Updating — {step}…")
}

/// How the banner reads when a newer release cannot be installed from here.
///
/// The two reasons need different words. Telling somebody "no build for your
/// machine" when the build is right there and only the checksums are missing
/// sends them looking for something that exists.
pub fn blocked_headline(version: &str, why: Blocked) -> String {
    match why {
        Blocked::NoBuildForTarget => format!(
            "Version {version} is available, but not as a build for {}.",
            tobii_update::Target::triple()
        ),
        Blocked::NoChecksums => format!(
            "Version {version} is available, but it was published without checksums, \
             so it cannot be installed from here."
        ),
    }
}

/// What the user is agreeing to when they press Update.
///
/// Deliberately not reassuring. The checksum published with a release is
/// fetched from that release, so it proves the download arrived intact and
/// nothing about who wrote it.
pub fn trust_note() -> String {
    "Installing replaces this program's binaries with the ones published in this release. \
     The published checksums are used to confirm the download arrived intact; they are not a \
     signature, so this trusts the project's GitHub releases as much as downloading and \
     running a binary from them by hand would."
        .to_string()
}

/// Build the update banner. It is hidden until a check finds something.
pub fn banner() -> gtk::Box {
    let row = gtk::Box::new(Orientation::Horizontal, 12);
    row.add_css_class("banner");
    row.set_visible(false);

    let text = Label::new(None);
    text.add_css_class("banner-text");
    text.set_hexpand(true);
    text.set_xalign(0.0);
    text.set_wrap(true);
    text.set_valign(Align::Center);

    let notes_btn = crate::widget::button("What's new");
    notes_btn.add_css_class("quiet");
    let update_btn = crate::widget::button("Update");
    update_btn.add_css_class("primary");
    let dismiss = crate::widget::button("Later");
    dismiss.add_css_class("quiet");

    row.append(&text);
    row.append(&notes_btn);
    row.append(&update_btn);
    row.append(&dismiss);

    {
        let row = row.clone();
        dismiss.connect_clicked(move |_| row.set_visible(false));
    }

    // Nothing is asked of the network when the user has said not to.
    if !tobii_config::update_check_enabled() {
        return row;
    }

    // The check, off the UI thread. A hub that stalled on a DNS lookup at
    // startup would be a worse bug than the one this feature fixes.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(tobii_update::release::check());
    });

    let widgets = (row.clone(), text.clone(), notes_btn, update_btn, dismiss);
    glib::timeout_add_local(Duration::from_millis(400), move || {
        let (row, text, notes_btn, update_btn, dismiss) = &widgets;
        match rx.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
            // Offline, rate-limited, or no curl. Not worth a banner: the user
            // did not ask for this check, so its failure is not their problem.
            Ok(Err(_)) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return glib::ControlFlow::Break
            }
            Ok(Ok(Check::UpToDate)) => return glib::ControlFlow::Break,
            // A newer release with no build for this machine. Saying so beats
            // an Update button that could only ever fail, and beats silence:
            // the release does exist and can be built from source.
            Ok(Ok(Check::CannotInstall { version, url, why })) => {
                text.set_text(&blocked_headline(&version.to_string(), why));
                update_btn.set_visible(false);
                // Checked before it is handed to the desktop's URI handler.
                // This string comes out of the same release document as every
                // other URL the crate refuses unless `is_trusted` passes, and
                // `launch_default_for_uri` will hand any scheme to whatever
                // claims it — so an unchecked one is a "click here" button
                // pointing wherever the document says.
                let url = url.clone();
                notes_btn.connect_clicked(move |btn| {
                    if tobii_update::net::is_trusted(&url) {
                        let _ = gtk::gio::AppInfo::launch_default_for_uri(
                            &url,
                            gtk::gio::AppLaunchContext::NONE,
                        );
                    } else {
                        tobii_diagnostics::log::warn(&format!(
                            "refusing to open a release URL that is not on the \
                             allowlist: {url}"
                        ));
                    }
                    let _ = btn;
                });
                crate::widget::set_button_text(notes_btn, "Releases");
                row.set_visible(true);
            }
            Ok(Ok(Check::Newer(release))) => {
                text.set_text(&headline(&release));
                wire(row, text, notes_btn, update_btn, dismiss, *release);
                row.set_visible(true);
            }
        }
        glib::ControlFlow::Break
    });
    row
}

/// Attach the two actions once a release is actually in hand.
fn wire(
    row: &gtk::Box,
    text: &Label,
    notes_btn: &gtk::Button,
    update_btn: &gtk::Button,
    dismiss: &gtk::Button,
    release: Release,
) {
    {
        let release = release.clone();
        notes_btn.connect_clicked(move |btn| {
            let parent = btn.root().and_downcast::<gtk::Window>();
            changelog_dialog(parent.as_ref(), &release);
        });
    }
    let (row, text, dismiss, notes_btn) = (
        row.clone(),
        text.clone(),
        dismiss.clone(),
        notes_btn.clone(),
    );
    update_btn.connect_clicked(move |btn| {
        btn.set_sensitive(false);
        notes_btn.set_sensitive(false);
        dismiss.set_sensitive(false);
        text.set_text(&installing("starting"));

        // Closing the window mid-install would otherwise end the process
        // between the two renames, leaving a new `tobii` beside an old
        // `tobii-gtk`. The hold is released when the install finishes, either
        // way. (`swap_in` also rolls back, but only for failures it is told
        // about — a process that simply exits tells it nothing.)
        let guard = gtk::gio::Application::default().map(|app| app.hold());

        // The install downloads, verifies and rewrites files. All of that is
        // off the UI thread; the worker reports through a channel and touches
        // no widgets.
        let (tx, rx) = std::sync::mpsc::channel();
        let (ptx, prx) = std::sync::mpsc::channel::<String>();
        let release = release.clone();
        std::thread::spawn(move || {
            let report = |s: &str| {
                let _ = ptx.send(s.to_string());
            };
            let _ = tx.send(tobii_update::install_release(&release, &report));
        });

        let (row, text, btn, dismiss) = (row.clone(), text.clone(), btn.clone(), dismiss.clone());
        // `hold()` hands back an RAII guard, so releasing it is a drop.
        let guard = std::cell::RefCell::new(guard);
        let release_hold = move || drop(guard.borrow_mut().take());
        glib::timeout_add_local(Duration::from_millis(150), move || {
            while let Ok(step) = prx.try_recv() {
                text.set_text(&installing(&step));
            }
            match rx.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Ok(Ok(done)) => {
                    release_hold();
                    text.set_text(&format!("Updated to {}. Restart to run it.", done.version));
                    btn.set_visible(false);
                    dismiss.set_sensitive(true);
                    crate::widget::set_button_text(&dismiss, "Close");
                    glib::ControlFlow::Break
                }
                Ok(Err(e)) => {
                    release_hold();
                    text.set_text(&format!("Update failed: {e}"));
                    btn.set_sensitive(true);
                    dismiss.set_sensitive(true);
                    glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    release_hold();
                    text.set_text("Update failed: the updater stopped unexpectedly.");
                    btn.set_sensitive(true);
                    dismiss.set_sensitive(true);
                    let _ = &row;
                    glib::ControlFlow::Break
                }
            }
        });
    });
}

/// A window showing the release notes.
///
/// Scrollable, unlike the licence dialog: a changelog has no length this program
/// controls, and one long enough to run off the screen is exactly the release
/// worth reading about.
fn changelog_dialog(parent: Option<&gtk::Window>, release: &Release) {
    let heading = Label::new(Some(&format!("What's new in {}", release.version)));
    heading.add_css_class("dialog-heading");
    heading.set_halign(Align::Start);
    heading.set_xalign(0.0);

    let notes = Label::new(Some(&changelog(release)));
    notes.add_css_class("dialog-terms");
    notes.set_wrap(true);
    notes.set_xalign(0.0);
    notes.set_halign(Align::Start);
    notes.set_max_width_chars(64);
    notes.set_selectable(false);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_child(Some(&notes));
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_vexpand(true);
    scroller.set_min_content_height(260);

    let trust = Label::new(Some(&trust_note()));
    trust.add_css_class("dialog-note");
    trust.set_wrap(true);
    trust.set_xalign(0.0);
    trust.set_halign(Align::Start);
    trust.set_max_width_chars(64);

    let link = Label::new(None);
    link.set_markup(&format!(
        "<a href=\"{url}\">{url}</a>",
        url = glib::markup_escape_text(&release.html_url)
    ));
    link.add_css_class("dialog-url");
    link.set_xalign(0.0);
    link.set_halign(Align::Start);
    link.set_wrap(true);
    link.set_wrap_mode(gtk::pango::WrapMode::Char);
    link.set_max_width_chars(64);

    let close = crate::widget::button("Close");
    let buttons = gtk::Box::new(Orientation::Horizontal, 10);
    buttons.set_halign(Align::End);
    buttons.append(&close);

    let content = gtk::Box::new(Orientation::Vertical, 12);
    content.set_margin_top(24);
    content.set_margin_bottom(20);
    content.set_margin_start(26);
    content.set_margin_end(26);
    content.append(&heading);
    content.append(&scroller);
    content.append(&trust);
    content.append(&link);
    content.append(&buttons);

    let win = gtk::Window::builder()
        .title("Release notes")
        .modal(true)
        .default_width(620)
        .default_height(480)
        .child(&content)
        .build();
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }
    let w = win.clone();
    close.connect_clicked(move |_| w.close());
    let keys = gtk::EventControllerKey::new();
    let w = win.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            w.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(keys);
    win.present();
    close.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tobii_update::Version;

    fn release(notes: &str) -> Release {
        Release {
            tag: "v0.3.0".into(),
            version: Version::parse("0.3.0").unwrap(),
            notes: notes.into(),
            assets: vec![],
            html_url: "https://example.invalid/r".into(),
        }
    }

    #[test]
    fn the_headline_names_the_version_on_offer() {
        assert_eq!(headline(&release("")), "Version 0.3.0 is available.");
    }

    /// A release with no notes must not open an empty window — it should say
    /// there are none and point somewhere that has the answer.
    #[test]
    fn a_release_without_notes_says_so_rather_than_showing_nothing() {
        let c = changelog(&release("   \n  "));
        assert!(c.contains("without release notes"), "{c}");
        assert!(c.contains("releases page"), "{c}");
    }

    #[test]
    fn notes_are_shown_as_written_minus_stray_whitespace() {
        let c = changelog(&release("\n## Fixed\n- the thing\n\n"));
        assert_eq!(c, "## Fixed\n- the thing");
    }

    #[test]
    fn progress_reads_as_a_sentence() {
        assert_eq!(installing("verifying"), "Updating — verifying…");
    }

    /// The banner must not offer an Update button that could only fail, and
    /// must still name the version, so the user knows the release exists.
    #[test]
    fn a_release_that_cannot_be_installed_says_which_reason_it_is() {
        let missing = blocked_headline("0.9.0", Blocked::NoBuildForTarget);
        assert!(missing.contains("0.9.0"), "{missing}");
        assert!(
            missing.contains(&tobii_update::Target::triple()),
            "{missing}"
        );

        let sums = blocked_headline("0.9.0", Blocked::NoChecksums);
        assert!(sums.contains("0.9.0"), "{sums}");
        assert!(sums.contains("checksums"), "{sums}");
        // The trap this split exists to avoid: telling somebody there is no
        // build for their machine when the build is right there.
        assert!(
            !sums.contains(&tobii_update::Target::triple()),
            "a missing SHA256SUMS is not a missing build: {sums}"
        );
    }

    /// The wording shown before Update is pressed has to be accurate: the
    /// checksum is not a signature, and claiming otherwise is the one thing
    /// this dialog must not do.
    #[test]
    fn the_trust_note_does_not_claim_a_guarantee_it_cannot_give() {
        let t = trust_note();
        assert!(t.contains("not a signature"), "{t}");
        assert!(t.contains("arrived intact"), "{t}");
        for overclaim in ["verified", "safe", "secure", "trusted source"] {
            assert!(!t.contains(overclaim), "{overclaim:?} overstates it: {t}");
        }
    }
}
