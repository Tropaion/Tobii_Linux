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
        Status::Ready => "Working, with the up-and-down angle.".to_string(),
        Status::Missing => "Working. Add the model for the up-and-down angle too.".to_string(),
        // Truncated because the label is one line in a narrow column; the full
        // hash is of no use to the user anyway, only the fact of a mismatch is.
        Status::Corrupt { found } => format!(
            "The model file on disk is not the expected one (sha256 {}…), so it is not being \
             used. Fetch it again.",
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

/// The pitch-zero line: what it is set to, or that it is not set.
pub fn pitch_line(offset: Option<f64>) -> String {
    match offset {
        Some(d) => format!("Pitch zero set to {d:+.1}°."),
        None => "Pitch zero not set — up-and-down will read about 20° off.".to_string(),
    }
}

/// Progress text for a running pitch measurement.
pub fn pitch_progress(secs_left: u64, samples: usize) -> String {
    if samples == 0 {
        format!("Hold still… {secs_left}s")
    } else {
        format!("Hold still… {secs_left}s, {samples} frames")
    }
}

/// How a finished measurement reads.
pub fn pitch_outcome(r: &Result<f64, String>) -> String {
    match r {
        Ok(d) => format!("Pitch zero set to {d:+.1}°."),
        Err(e) => format!("Not set: {e}"),
    }
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
///
/// Deliberately without the URL: it is 120 characters of raw link, it is shown
/// on its own line below, and a label containing it sets the dialog's natural
/// width to 1131 px — which is how the first version of this dialog came out
/// twice as wide as it needed to be.
pub fn download_summary(src: &ModelSource) -> String {
    format!(
        "{} — {:.1} MB, from the opentrack project",
        src.file,
        src.bytes as f64 / 1e6
    )
}

/// Build the control: a status line, the pitch-zero state, and the actions that
/// apply to whichever of those two is missing.
///
/// `state` and `cmd_tx` are the device thread's, because measuring the pitch
/// zero needs camera frames and the model — everything else here is local.
pub fn control(
    state: std::sync::Arc<std::sync::Mutex<crate::device::DeviceState>>,
    cmd_tx: std::sync::mpsc::Sender<crate::device::DeviceCommand>,
) -> gtk::Box {
    let b = gtk::Box::new(Orientation::Vertical, 6);
    let status = Label::new(None);
    status.add_css_class("section-desc");
    status.set_halign(Align::Start);
    status.set_xalign(0.0);
    status.set_wrap(true);

    let pitch = Label::new(None);
    pitch.add_css_class("section-desc");
    pitch.set_halign(Align::Start);
    pitch.set_xalign(0.0);
    pitch.set_wrap(true);

    let get = Button::with_label("Get the model…");
    let set_pitch = Button::with_label("Set pitch zero…");
    let updates = Button::with_label("Check for updates");
    let remove = Button::with_label("Remove");
    for small in [&updates, &remove] {
        small.add_css_class("help-btn");
    }

    let actions = gtk::Box::new(Orientation::Horizontal, 8);
    actions.set_halign(Align::Start);
    actions.append(&get);
    actions.append(&set_pitch);
    actions.append(&updates);
    actions.append(&remove);

    let refresh = {
        let (status, pitch) = (status.clone(), pitch.clone());
        let (get, set_pitch, updates, remove) = (
            get.clone(),
            set_pitch.clone(),
            updates.clone(),
            remove.clone(),
        );
        move || {
            let st = model_store::status(SRC);
            let ready = matches!(st, Status::Ready);
            status.set_text(&status_line(&st));
            pitch.set_text(&pitch_line(model_store::pitch_offset()));
            pitch.set_visible(ready);
            get.set_visible(!ready);
            for only_when_installed in [&set_pitch, &updates, &remove] {
                only_when_installed.set_visible(ready);
            }
        }
    };
    refresh();
    b.append(&status);
    b.append(&pitch);
    b.append(&actions);

    // --- get the model ---
    {
        let (status, refresh) = (status.clone(), refresh.clone());
        get.connect_clicked(move |btn| {
            let (status, refresh, btn) = (status.clone(), refresh.clone(), btn.clone());
            let parent = btn.root().and_downcast::<gtk::Window>();
            terms_dialog(parent.as_ref(), move || {
                start_download(&btn, &status, refresh.clone())
            });
        });
    }

    // --- measure the pitch zero ---
    {
        let (pitch_label, refresh) = (pitch.clone(), refresh.clone());
        set_pitch.connect_clicked(move |btn| {
            let parent = btn.root().and_downcast::<gtk::Window>();
            pitch_dialog(
                parent.as_ref(),
                state.clone(),
                cmd_tx.clone(),
                pitch_label.clone(),
                refresh.clone(),
            );
        });
    }

    // --- is there a newer model upstream? ---
    {
        let status = status.clone();
        updates.connect_clicked(move |btn| {
            use tobii_headpose::model_store::Update;
            btn.set_sensitive(false);
            status.set_text("Checking…");
            let (tx, rx) = std::sync::mpsc::channel();
            // A network call, so off the UI thread like the download.
            std::thread::spawn(move || {
                let _ = tx.send(model_store::check_update(SRC));
            });
            let (btn, status) = (btn.clone(), status.clone());
            glib::timeout_add_local(Duration::from_millis(150), move || {
                match rx.try_recv() {
                    Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Ok(u) => {
                        btn.set_sensitive(true);
                        status.set_text(&match u {
                            Update::UpToDate => "This is the current model.".to_string(),
                            // Deliberately not offered as a download: a new model
                            // has its own pose conventions and its own pitch
                            // zero, so adopting one is a new release of this
                            // program, not a fetch.
                            Update::Newer { date, .. } => format!(
                                "opentrack published a newer model on {}. It needs new \
                                 measurements before this program can use it.",
                                date.split('T').next().unwrap_or(&date)
                            ),
                            Update::Unknown(why) => format!("Could not check: {why}"),
                        });
                        glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        btn.set_sensitive(true);
                        status.set_text("Could not check.");
                        glib::ControlFlow::Break
                    }
                }
            });
        });
    }

    // --- remove it ---
    {
        let (status, refresh) = (status.clone(), refresh.clone());
        remove.connect_clicked(move |btn| {
            let dlg = gtk::AlertDialog::builder()
                .modal(true)
                .message("Remove the head-pose model?")
                .detail(
                    "Head tracking keeps working without it, minus the up-and-down angle. \
                     You can download it again at any time.",
                )
                .buttons(["Cancel", "Remove"])
                .cancel_button(0)
                .default_button(0)
                .build();
            let (status, refresh) = (status.clone(), refresh.clone());
            let parent = btn.root().and_downcast::<gtk::Window>();
            dlg.choose(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                if res.unwrap_or(0) != 1 {
                    return;
                }
                match std::fs::remove_file(model_store::path_of(SRC)) {
                    Ok(()) => refresh(),
                    Err(e) => status.set_text(&format!("Could not remove it: {e}")),
                }
            });
        });
    }

    b
}

/// The guided pitch-zero measurement: instructions, a live countdown, a result.
fn pitch_dialog<F: Fn() + Clone + 'static>(
    parent: Option<&gtk::Window>,
    state: std::sync::Arc<std::sync::Mutex<crate::device::DeviceState>>,
    cmd_tx: std::sync::mpsc::Sender<crate::device::DeviceCommand>,
    pitch_label: Label,
    refresh: F,
) {
    const SECS: u64 = 10;

    let heading = Label::new(Some("Set the pitch zero"));
    heading.add_css_class("dialog-heading");
    heading.set_halign(Align::Start);
    heading.set_xalign(0.0);

    let body = Label::new(Some(concat!(
        "Sit the way you normally do and look at the middle of the screen. ",
        "Hold still until the countdown finishes.\n\n",
        "The model measures how far your head is tilted in its own frame, which is ",
        "offset from level by how this tracker is mounted. This measures that offset ",
        "once, so up-and-down reads zero when you sit like this.",
    )));
    body.add_css_class("dialog-terms");
    body.set_wrap(true);
    body.set_xalign(0.0);
    body.set_halign(Align::Start);
    body.set_max_width_chars(52);

    let progress = Label::new(Some("Ready."));
    progress.add_css_class("dialog-lead");
    progress.set_halign(Align::Start);
    progress.set_xalign(0.0);
    progress.set_wrap(true);
    progress.set_max_width_chars(52);

    let close = Button::with_label("Cancel");
    let go = Button::with_label("Start");
    go.add_css_class("suggested");
    let buttons = gtk::Box::new(Orientation::Horizontal, 10);
    buttons.set_halign(Align::End);
    buttons.set_margin_top(4);
    buttons.append(&close);
    buttons.append(&go);

    let content = gtk::Box::new(Orientation::Vertical, 12);
    content.set_margin_top(24);
    content.set_margin_bottom(20);
    content.set_margin_start(26);
    content.set_margin_end(26);
    content.append(&heading);
    content.append(&body);
    content.append(&progress);
    content.append(&buttons);

    let win = gtk::Window::builder()
        .title("Pitch zero")
        .modal(true)
        .resizable(false)
        .default_width(520)
        .child(&content)
        .build();
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }

    {
        let w = win.clone();
        let state = state.clone();
        close.connect_clicked(move |_| {
            // Tell a running measurement to stop; the device thread checks this
            // flag each loop and returns without saving.
            state.lock().unwrap().pitch_cal.active = false;
            w.close();
        });
    }
    {
        let (progress, close, pitch_label) = (progress.clone(), close.clone(), pitch_label.clone());
        let refresh = refresh.clone();
        let state = state.clone();
        go.connect_clicked(move |go| {
            go.set_sensitive(false);
            close.set_label("Stop");
            progress.set_text("Get comfortable — starting in 3 seconds…");
            let _ = cmd_tx.send(crate::device::DeviceCommand::PitchCalibrate { secs: SECS });
            let (state, progress, go, close) =
                (state.clone(), progress.clone(), go.clone(), close.clone());
            let (pitch_label, refresh) = (pitch_label.clone(), refresh.clone());
            glib::timeout_add_local(Duration::from_millis(200), move || {
                let cal = state.lock().unwrap().pitch_cal.clone();
                if let Some(result) = &cal.result {
                    progress.set_text(&pitch_outcome(result));
                    pitch_label.set_text(&pitch_line(model_store::pitch_offset()));
                    refresh();
                    go.set_sensitive(true);
                    close.set_label("Done");
                    return glib::ControlFlow::Break;
                }
                if cal.active {
                    progress.set_text(&pitch_progress(cal.secs_left, cal.samples));
                }
                glib::ControlFlow::Continue
            });
        });
    }

    let keys = gtk::EventControllerKey::new();
    let w = win.clone();
    let state_for_esc = state.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            state_for_esc.lock().unwrap().pitch_cal.active = false;
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

/// A modal window showing the licence terms, with an explicit agree/cancel.
///
/// Public so it can be rendered standalone for a visual check; the hub only
/// reaches it through the button in [`control`].
///
/// Deliberately a real window rather than a `gtk::AlertDialog`: `AlertDialog`'s
/// detail text neither scrolls nor takes any layout direction, so several
/// paragraphs of licence simply grow it past the height of the screen.
/// `on_agree` runs only for the agree button.
pub fn terms_dialog<F: Fn() + 'static>(parent: Option<&gtk::Window>, on_agree: F) {
    let heading = Label::new(Some("Download the head-pose model?"));
    heading.add_css_class("dialog-heading");
    heading.set_halign(Align::Start);
    heading.set_xalign(0.0);

    let lead = Label::new(Some(
        "It adds the up-and-down head angle. Everything else already works without it.",
    ));
    lead.add_css_class("dialog-lead");
    lead.set_wrap(true);
    lead.set_xalign(0.0);
    lead.set_halign(Align::Start);
    lead.set_max_width_chars(58);

    let terms = Label::new(Some(&reflow(model_store::TERMS)));
    terms.add_css_class("dialog-terms");
    terms.set_wrap(true);
    terms.set_xalign(0.0);
    terms.set_halign(Align::Start);
    // A wrapping label reports its *unwrapped* width as its natural width, so
    // without this the window opens as wide as the longest paragraph.
    terms.set_max_width_chars(58);
    // NOT selectable. A selectable GtkLabel takes the initial focus and shows
    // its whole text selected the moment the dialog opens, which reads as a
    // rendering fault. The licence URL is a link below instead, which is the
    // only part anyone would want to copy.
    terms.set_selectable(false);

    let facts = gtk::Box::new(Orientation::Vertical, 2);
    facts.add_css_class("dialog-facts");
    let what = Label::new(Some(&download_summary(SRC)));
    what.set_xalign(0.0);
    what.set_halign(Align::Start);
    what.set_wrap(true);
    what.set_max_width_chars(58);
    let from = Label::new(None);
    from.set_markup(&format!(
        "<a href=\"{url}\">{url}</a>",
        url = glib::markup_escape_text(SRC.url)
    ));
    from.add_css_class("dialog-url");
    from.set_xalign(0.0);
    from.set_halign(Align::Start);
    from.set_wrap(true);
    from.set_wrap_mode(gtk::pango::WrapMode::Char);
    from.set_max_width_chars(58);
    facts.append(&what);
    facts.append(&from);

    let cancel = Button::with_label("Not now");
    let agree = Button::with_label("I agree — download");
    agree.add_css_class("suggested");
    let buttons = gtk::Box::new(Orientation::Horizontal, 10);
    buttons.set_halign(Align::End);
    buttons.set_margin_top(4);
    buttons.append(&cancel);
    buttons.append(&agree);

    let content = gtk::Box::new(Orientation::Vertical, 12);
    content.set_margin_top(24);
    content.set_margin_bottom(20);
    content.set_margin_start(26);
    content.set_margin_end(26);
    content.append(&heading);
    content.append(&lead);
    content.append(&terms);
    content.append(&facts);
    content.append(&buttons);

    let win = gtk::Window::builder()
        .title("Head-pose model")
        .modal(true)
        .resizable(false)
        // Every label above is width-capped, because with `resizable(false)` the
        // window takes the widest child's natural width — and one unwrapped URL
        // was enough to make it 1131 px.
        .default_width(560)
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
    // Focus the refusal, not the download. Nothing here should be one stray
    // Return away from a network fetch the user has not read the terms for.
    cancel.grab_focus();
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

    /// The section must never read as "broken" when there is no model: head
    /// tracking works fine without one, it just loses one axis.
    #[test]
    fn a_missing_model_does_not_read_as_a_broken_feature() {
        let s = status_line(&Status::Missing);
        assert!(s.starts_with("Working"), "{s}");
        assert!(s.contains("angle"), "{s}");
    }

    /// These sit under a heading in a narrow column. Long is unread.
    #[test]
    fn the_status_lines_stay_short_enough_to_scan() {
        for st in [Status::Ready, Status::Missing] {
            let s = status_line(&st);
            assert!(s.len() <= 60, "{} chars is too long: {s:?}", s.len());
        }
    }

    #[test]
    fn a_wrong_file_says_so_and_says_it_will_not_be_used() {
        let s = status_line(&Status::Corrupt {
            found: "deadbeefcafebabe0123".into(),
        });
        assert!(s.contains("deadbeefcafe"), "{s}");
        assert!(s.contains("not being used"), "{s}");
    }

    #[test]
    fn a_short_hash_does_not_panic_the_truncation() {
        // Nothing produces a short digest today, but the slice is the kind of
        // thing that turns a cosmetic surprise into a crash in the hub.
        let s = status_line(&Status::Corrupt { found: "ab".into() });
        assert!(s.contains("ab"), "{s}");
    }

    /// An unset pitch zero is not a neutral state — it is about 20 degrees of
    /// error — so the line has to say so rather than just "not set".
    #[test]
    fn an_unset_pitch_zero_says_what_it_costs() {
        let s = pitch_line(None);
        assert!(s.contains("not set"), "{s}");
        assert!(s.contains("20°"), "the consequence must be visible: {s}");
        assert_eq!(pitch_line(Some(-24.08)), "Pitch zero set to -24.1°.");
        assert_eq!(pitch_line(Some(3.0)), "Pitch zero set to +3.0°.");
    }

    #[test]
    fn the_pitch_countdown_reads_sensibly_before_any_frames_arrive() {
        assert_eq!(pitch_progress(12, 0), "Hold still… 12s");
        assert_eq!(pitch_progress(4, 130), "Hold still… 4s, 130 frames");
    }

    #[test]
    fn a_failed_pitch_measurement_does_not_read_as_a_number() {
        assert_eq!(pitch_outcome(&Ok(-24.08)), "Pitch zero set to -24.1°.");
        let s = pitch_outcome(&Err("only 3 usable frames".into()));
        assert!(s.starts_with("Not set:"), "{s}");
        assert!(s.contains("3 usable frames"), "{s}");
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

    /// The bug this function exists for: consent text that a wrapping widget
    /// wraps a second time comes out ragged, every paragraph broken at an
    /// arbitrary point. The invariant is that each paragraph reaches the widget
    /// as ONE logical line — a line ending mid-sentence is the signature of the
    /// bug, so every paragraph must end on real punctuation.
    #[test]
    fn every_paragraph_of_the_real_terms_survives_as_one_line() {
        let got = reflow(model_store::TERMS);
        for line in got.lines() {
            if line.trim().is_empty() || line.starts_with(char::is_whitespace) {
                continue; // blank, or the deliberately-kept licence URL
            }
            let last = line.trim_end().chars().last().unwrap();
            assert!(
                matches!(last, '.' | ':' | '?' | '!'),
                "this line stops mid-sentence, so the paragraph was wrapped: {line:?}"
            );
        }
        assert_eq!(reflow(&got), got, "reflow must be idempotent");
    }

    #[test]
    fn the_download_summary_names_the_file_the_size_and_the_source() {
        let s = download_summary(SRC);
        assert!(s.contains(SRC.file), "{s}");
        assert!(s.contains("12.9 MB"), "{s}");
        assert!(s.contains("opentrack"), "{s}");
        // The URL belongs on its own line. Inline, it sets the dialog's natural
        // width and doubles it.
        assert!(!s.contains("http"), "the URL must not be inline: {s}");
        assert!(
            s.len() < 70,
            "{} chars is too wide for the dialog: {s}",
            s.len()
        );
    }
}
