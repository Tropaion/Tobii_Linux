//! `tobii-gtk` — GTK4 configuration GUI for the Tobii ET5 Linux runtime.
//!
//! Foundation: a styled hub window (status + live eye-position view) over the
//! ported device thread. Guided flows, the gaze overlay, and select-eyes land
//! in later phases of the GTK4 redesign.

pub mod align;
pub mod device;
pub mod eyeview;
pub mod setup_flow;
pub mod widget;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Application, ApplicationWindow, Button, DrawingArea, Label, Orientation};

use crate::eyeview::{EyeView, Guidance};

const APP_ID: &str = "com.tobiilinux.Configuration";

const CSS: &str = "
window { background-color: #15181c; color: #e6e8ea; }
.app-title { font-size: 22px; font-weight: bold; }
.status { font-size: 13px; color: #9aa4ad; }
.guidance { font-size: 14px; }
button { background-image: none; background-color: #1f9ea0; color: #ffffff;
         border: none; border-radius: 8px; padding: 8px 14px; }
button:hover { background-color: #26b6b8; }
button:disabled { background-color: #2a2f36; color: #6b7178; }
";

/// Run the GTK application.
pub fn run() -> glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| load_css());
    app.connect_activate(build_ui);
    app.run()
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn no_eyes_view() -> EyeView {
    EyeView {
        left: None,
        right: None,
        distance_mm: None,
        guidance: Guidance::NoEyes,
    }
}

fn build_ui(app: &Application) {
    let (state, cmd_tx) = device::spawn();
    // Latest view shared with the DrawingArea's draw callback (UI thread only).
    let view = Rc::new(RefCell::new(no_eyes_view()));

    let root = gtk::Box::new(Orientation::Vertical, 14);
    root.set_margin_top(20);
    root.set_margin_bottom(20);
    root.set_margin_start(24);
    root.set_margin_end(24);

    let title = Label::new(Some("Tobii Configuration"));
    title.add_css_class("app-title");
    title.set_halign(Align::Start);

    let status = Label::new(Some("Connecting…"));
    status.add_css_class("status");
    status.set_halign(Align::Start);

    // Launcher row — wired in later phases (flows / overlay / select-eyes).
    let launchers = gtk::Box::new(Orientation::Horizontal, 10);
    let b_setup = Button::with_label("Set up display…");
    let b_eyes = Button::with_label("Position eyes…");
    let b_preview = Button::with_label("Preview my gaze");
    let b_cal = Button::with_label("Improve calibration");
    b_cal.set_sensitive(false); // B3
    b_cal.set_tooltip_text(Some("Calibration flow — coming in B3"));
    for b in [&b_setup, &b_eyes, &b_preview, &b_cal] {
        launchers.append(b);
    }

    // "Set up display…" launches the fullscreen line-alignment flow.
    {
        let app = app.clone();
        let cmd_tx = cmd_tx.clone();
        b_setup.connect_clicked(move |_| setup_flow::launch(&app, cmd_tx.clone()));
    }

    let area = DrawingArea::new();
    area.set_content_width(360);
    area.set_content_height(220);
    {
        let view = view.clone();
        area.set_draw_func(move |_, cr, w, h| widget::draw_eye_view(cr, w, h, &view.borrow()));
    }

    let guidance = Label::new(None);
    guidance.add_css_class("guidance");
    guidance.set_halign(Align::Start);

    root.append(&title);
    root.append(&status);
    root.append(&launchers);
    root.append(&Label::new(Some("Eye position:")));
    root.append(&area);
    root.append(&guidance);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Tobii Configuration")
        .default_width(720)
        .default_height(520)
        .build();
    window.set_child(Some(&root));

    // ~30 fps tick: read the device snapshot, refresh status + eye view.
    let tick_view = view.clone();
    glib::timeout_add_local(Duration::from_millis(33), move || {
        let snap = state.lock().unwrap().clone();
        status.set_text(&widget::status_text(&snap.status));
        let ev = widget::eye_view_for(&snap);
        guidance.set_text(&widget::guidance_message(&ev));
        *tick_view.borrow_mut() = ev;
        area.queue_draw();
        glib::ControlFlow::Continue
    });

    window.present();
}
