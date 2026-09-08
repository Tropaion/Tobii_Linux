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
//! the user presses Update, and the download is verified against the checksums
//! published beside it before anything is put where it would be run.
//!
//! The check runs on a worker thread and the banner stays hidden until it has
//! something to say, so a slow or absent network costs the hub nothing.

use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Label, Orientation};

use tobii_update::release::{Check, Release};

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
        glib::timeout_add_local(Duration::from_millis(150), move || {
            while let Ok(step) = prx.try_recv() {
                text.set_text(&installing(&step));
            }
            match rx.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Ok(Ok(done)) => {
                    text.set_text(&format!("Updated to {}. Restart to run it.", done.version));
                    btn.set_visible(false);
                    dismiss.set_sensitive(true);
                    crate::widget::set_button_text(&dismiss, "Close");
                    glib::ControlFlow::Break
                }
                Ok(Err(e)) => {
                    text.set_text(&format!("Update failed: {e}"));
                    btn.set_sensitive(true);
                    dismiss.set_sensitive(true);
                    glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
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
}
