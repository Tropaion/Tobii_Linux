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

/// Undo hard line wrapping so a widget can wrap the text to its own width.
///
/// [`model_store::TERMS`] is wrapped at ~76 columns because its other consumer
/// is a terminal. Handing that to a wrapping `Label` wraps it *twice* — every
/// hard newline becomes a short line, and the paragraph comes out ragged. So
/// each paragraph is joined back into one logical line here and the widget is
/// left to do the wrapping.
///
/// Blank lines separate paragraphs and are kept. A line that starts with
/// whitespace is deliberately *not* joined: that is how the terms set their
/// licence URL apart, and a URL folded into a paragraph is much worse to read.
pub fn reflow(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut open = false; // a paragraph is being accumulated
    for line in text.lines() {
        let indented = line.starts_with(char::is_whitespace);
        if line.trim().is_empty() {
            out.push_str("\n\n");
            open = false;
        } else if indented {
            if open {
                out.push('\n');
            }
            out.push_str(line.trim_end());
            out.push('\n');
            open = false;
        } else {
            if open {
                out.push(' ');
            }
            out.push_str(line.trim());
            open = true;
        }
    }
    // Collapse the runs of blank lines the loop above can leave at a boundary.
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    out.trim().to_string()
}

/// What the download is about to do, in the user's terms rather than ours.
pub fn download_summary(src: &ModelSource) -> String {
    format!(
        "{} — {:.1} MB, from {}",
        src.file,
        src.bytes as f64 / 1e6,
        src.url
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
        let status = status_for_click.clone();
        let refresh = refresh_for_click.clone();
        let btn = btn.clone();
        // The hub window is built after this control, so the parent is taken
        // from the button's own root at click time rather than captured.
        let parent = btn.root().and_downcast::<gtk::Window>();
        terms_dialog(parent.as_ref(), move || {
            start_download(&btn, &status, refresh.clone())
        });
    });
    b
}

/// A modal window showing the licence terms, with an explicit agree/cancel.
///
/// Deliberately a real window rather than a `gtk::AlertDialog`: the terms run to
/// several paragraphs, and `AlertDialog`'s detail text neither scrolls nor gives
/// any control over how it is laid out — it grows the dialog until it is taller
/// than the screen. `on_agree` runs only for the agree button.
fn terms_dialog<F: Fn() + 'static>(parent: Option<&gtk::Window>, on_agree: F) {
    let heading = Label::new(Some("Download the head-pose model?"));
    heading.add_css_class("cal-fail-heading");
    heading.set_halign(Align::Start);
    heading.set_wrap(true);
    heading.set_xalign(0.0);

    let terms = Label::new(Some(&reflow(model_store::TERMS)));
    terms.set_wrap(true);
    terms.set_xalign(0.0);
    terms.set_halign(Align::Start);
    // Without this a wrapping label reports its *unwrapped* width as natural,
    // and the window opens as wide as the longest paragraph.
    terms.set_max_width_chars(64);
    terms.set_selectable(true);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_child(Some(&terms));
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_vexpand(true);
    scroller.set_min_content_height(320);

    let what = Label::new(Some(&download_summary(SRC)));
    what.add_css_class("section-desc");
    what.set_wrap(true);
    what.set_wrap_mode(gtk::pango::WrapMode::WordChar); // the URL has no spaces
    what.set_xalign(0.0);
    what.set_halign(Align::Start);
    what.set_max_width_chars(64);

    let cancel = Button::with_label("Cancel");
    let agree = Button::with_label("I agree — download");
    let buttons = gtk::Box::new(Orientation::Horizontal, 12);
    buttons.set_halign(Align::End);
    buttons.append(&cancel);
    buttons.append(&agree);

    let content = gtk::Box::new(Orientation::Vertical, 14);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.append(&heading);
    content.append(&scroller);
    content.append(&what);
    content.append(&buttons);

    let win = gtk::Window::builder()
        .title("Head-pose model")
        .modal(true)
        .default_width(620)
        .default_height(560)
        .child(&content)
        .build();
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }

    let w = win.clone();
    cancel.connect_clicked(move |_| w.close());
    let w = win.clone();
    agree.connect_clicked(move |_| {
        w.close();
        on_agree();
    });
    // Escape is a "no", like every other refusal path here.
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
}

/// Fetch on a worker thread, reporting progress from the part-file's size.
fn start_download<F: Fn() + Clone + 'static>(btn: &Button, status: &Label, refresh: F) {
    btn.set_sensitive(false);
    btn.set_visible(true);
    status.set_text(&progress_line(0, SRC.bytes));
    // Off the UI thread: 13 MB over an unknown link must not freeze the hub.
    // The worker touches no widgets — it only sends its result.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(model_store::fetch(SRC));
    });
    let btn = btn.clone();
    let status = status.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            // The fetcher is a child process (curl/wget), so the only progress
            // signal available is the part-file growing on disk.
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
    });
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

    #[test]
    fn reflow_joins_a_hard_wrapped_paragraph_into_one_line() {
        let got = reflow("one two\nthree four\n\nsecond para\ncontinues");
        assert_eq!(got, "one two three four\n\nsecond para continues");
    }

    #[test]
    fn reflow_leaves_an_indented_line_alone() {
        // This is how the terms set their licence URL apart; folding a URL into
        // a paragraph makes it much harder to read and to copy.
        let got = reflow("text before\n  https://example.invalid/license.md\ntext after");
        assert!(
            got.contains("\n  https://example.invalid/license.md\n"),
            "the indented URL must keep its own line: {got:?}"
        );
    }

    /// The bug this function exists for: the real terms, rendered by a wrapping
    /// widget, came out ragged because they were already wrapped at 76 columns.
    #[test]
    fn the_real_terms_contain_no_short_hard_wrapped_lines_after_reflow() {
        let got = reflow(model_store::TERMS);
        for line in got.lines() {
            let l = line.trim();
            if l.is_empty() || line.starts_with(char::is_whitespace) {
                continue;
            }
            assert!(
                l.len() > 76,
                "a paragraph should be one long logical line, got {} chars: {l:?}",
                l.len()
            );
        }
    }

    #[test]
    fn the_download_summary_names_the_file_the_size_and_the_source() {
        let s = download_summary(SRC);
        assert!(s.contains(SRC.file), "{s}");
        assert!(s.contains("12.9 MB"), "{s}");
        assert!(s.contains("opentrack"), "{s}");
    }
}
