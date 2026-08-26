//! The hub's "Head tracking" control: model status, licence terms, and fetch.
//!
//! The head-pose model is not part of this program and is not shipped with it.
//! Its weights are trained on non-commercial-only data (see
//! [`tobii_headpose::model_store::TERMS`]), which is incompatible with
//! redistribution under GPL-3.0-only, so it can neither be bundled nor pulled
//! in the background: the terms are shown and a decision is taken *before* any
//! network access happens. That decision lives here, in the UI that showed
//! them — `model_store::fetch` deliberately takes no "yes" flag of its own.
//!
//! The text helpers are pure and unit-tested; the widget wiring is not.

use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Button, Label, Orientation};

use tobii_headpose::model_store::{self, ModelSource, Status};

/// The model this control offers. The localizer is not fetched: our face ROI
/// comes from the tracker's own metric eye origins, which are better than a
/// detector's guess and cost nothing.
const SRC: &ModelSource = &model_store::HEAD_POSE;

/// Status line for the section. Never invites the user to believe a wrong file
/// will be used, and never implies head tracking is unavailable without it.
pub fn status_line(st: &Status) -> String {
    match st {
        Status::Ready => "Model installed and verified — head tracking reports full 6DOF, \
                          including pitch."
            .to_string(),
        Status::Missing => "No model installed. Head tracking still works without one, but \
                            cannot report pitch (looking up and down)."
            .to_string(),
        // Truncated because the label is one line in a narrow column; the full
        // hash is of no use to the user anyway, only the fact of a mismatch is.
        Status::Corrupt { found } => format!(
            "A model file is present but is not the expected one (sha256 {}…). It will not be \
             used — fetch it again.",
            &found[..12.min(found.len())]
        ),
    }
}

/// Progress line while the fetcher runs. `total` is the pinned size, so this
/// cannot run past 100% for the file we actually accept.
pub fn progress_line(so_far: u64, total: u64) -> String {
    format!(
        "Downloading… {:.1} of {:.1} MB",
        so_far as f64 / 1e6,
        total as f64 / 1e6
    )
}

/// Build the control: a status line plus, when there is something to fetch, a
/// button that shows the terms and downloads on explicit agreement.
pub fn control() -> gtk::Box {
    let b = gtk::Box::new(Orientation::Vertical, 6);
    let status = Label::new(None);
    status.add_css_class("section-desc");
    status.set_halign(Align::Start);
    status.set_xalign(0.0);
    status.set_wrap(true);
    let button = Button::with_label("Get the model…");

    let refresh = {
        let status = status.clone();
        let button = button.clone();
        move || {
            let st = model_store::status(SRC);
            status.set_text(&status_line(&st));
            button.set_visible(!matches!(st, Status::Ready));
        }
    };
    refresh();
    b.append(&status);
    b.append(&button);

    let status_for_click = status.clone();
    let refresh_for_click = refresh.clone();
    button.connect_clicked(move |btn| {
        let dlg = gtk::AlertDialog::builder()
            .modal(true)
            .message("Download the head-pose model?")
            .detail(format!(
                "{}\n\nAbout to download {} ({:.1} MB) from {}",
                model_store::TERMS,
                SRC.file,
                SRC.bytes as f64 / 1e6,
                SRC.url
            ))
            .buttons(["Cancel", "I agree — download"])
            // Both the Escape key and the default action must mean "no": the
            // safe answer to a licence prompt is the one that fetches nothing.
            .cancel_button(0)
            .default_button(0)
            .build();
        let btn = btn.clone();
        let status = status_for_click.clone();
        let refresh = refresh_for_click.clone();
        // The hub window is built after this control, so the dialog's parent is
        // taken from the button's own root at click time rather than captured.
        let parent = btn.root().and_downcast::<gtk::Window>();
        dlg.choose(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
            if res.unwrap_or(0) != 1 {
                return;
            }
            btn.set_sensitive(false);
            status.set_text(&progress_line(0, SRC.bytes));
            // Off the UI thread: 13 MB over an unknown link must not freeze the
            // hub. The worker touches no widgets — it only sends its result.
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(model_store::fetch(SRC));
            });
            glib::timeout_add_local(Duration::from_millis(150), move || {
                match rx.try_recv() {
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        // The fetcher is a child process (curl/wget), so the
                        // only progress signal available is the part-file
                        // growing on disk.
                        let so_far = std::fs::metadata(model_store::download_path(SRC))
                            .map(|m| m.len())
                            .unwrap_or(0);
                        status.set_text(&progress_line(so_far, SRC.bytes));
                        glib::ControlFlow::Continue
                    }
                    Ok(Ok(_)) => {
                        btn.set_sensitive(true);
                        refresh();
                        glib::ControlFlow::Break
                    }
                    Ok(Err(e)) => {
                        btn.set_sensitive(true);
                        status.set_text(&format!("Download failed: {e}"));
                        glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        btn.set_sensitive(true);
                        status.set_text("Download failed: the download stopped unexpectedly.");
                        glib::ControlFlow::Break
                    }
                }
            });
        });
    });
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_model_does_not_read_as_a_broken_feature() {
        let s = status_line(&Status::Missing);
        assert!(s.contains("still works"), "{s}");
        assert!(s.contains("pitch"), "{s}");
    }

    #[test]
    fn a_wrong_file_says_so_and_says_it_will_not_be_used() {
        let s = status_line(&Status::Corrupt {
            found: "deadbeefcafebabe0123".into(),
        });
        assert!(s.contains("deadbeefcafe"), "{s}");
        assert!(s.contains("will not be used"), "{s}");
    }

    #[test]
    fn a_short_hash_does_not_panic_the_truncation() {
        // Nothing produces a short digest today, but the slice is the kind of
        // thing that turns a cosmetic surprise into a crash in the hub.
        let s = status_line(&Status::Corrupt { found: "ab".into() });
        assert!(s.contains("ab"), "{s}");
    }

    #[test]
    fn progress_is_reported_in_megabytes_against_the_pinned_size() {
        assert_eq!(progress_line(0, 12_919_981), "Downloading… 0.0 of 12.9 MB");
        assert_eq!(
            progress_line(6_000_000, 12_919_981),
            "Downloading… 6.0 of 12.9 MB"
        );
    }
}
