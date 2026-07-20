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

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Align, Application, ApplicationWindow, Button, CheckButton, DrawingArea, Label, Orientation,
    Switch,
};

use crate::eyeview::{EyeView, Guidance};

const APP_ID: &str = "com.tobiilinux.Configuration";

const CSS: &str = "
window { background-color: #15181c; color: #e6e8ea; }
.app-title { font-size: 22px; font-weight: bold; }
.status { font-size: 13px; color: #9aa4ad; }
.guidance { font-size: 14px; }
.section-title { font-size: 15px; font-weight: bold; }
.section-desc { font-size: 12px; color: #9aa4ad; }
button { background-image: none; background-color: #1f9ea0; color: #ffffff;
         border: none; border-radius: 8px; padding: 10px 18px; min-height: 24px; }
button label { padding: 2px 0; }
button:hover { background-color: #26b6b8; }
button:disabled { background-color: #2a2f36; color: #6b7178; }
button:checked { background-color: #14696b; }
button.spin-btn { min-width: 26px; padding: 2px 10px; }
.spin-entry { padding: 2px 6px; }
";

/// Run the GTK application.
pub fn run() -> glib::ExitCode {
    // GTK4's Vulkan renderer makes Mesa/radv print a noisy "not a conformant
    // Vulkan implementation" warning; default to the GL renderer (overridable).
    if std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "gl");
    }
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

/// Aspect ratio (w/h) of the primary monitor, so the eye-position box mirrors
/// the screen's shape (e.g. 21:9). Falls back to 16:9.
fn screen_aspect() -> f64 {
    gtk::gdk::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_then(|obj| obj.downcast::<gtk::gdk::Monitor>().ok())
        .map(|m| {
            let g = m.geometry();
            if g.height() > 0 {
                g.width() as f64 / g.height() as f64
            } else {
                16.0 / 9.0
            }
        })
        .unwrap_or(16.0 / 9.0)
}

fn build_ui(app: &Application) {
    let (state, cmd_tx) = device::spawn();
    // Latest view shared with the DrawingArea's draw callback (UI thread only).
    let view = Rc::new(RefCell::new(no_eyes_view()));

    // --- Header ---
    let title = Label::new(Some("Tobii Configuration"));
    title.add_css_class("app-title");
    title.set_halign(Align::Start);

    // Connection status (round indicator + label) — lower-left corner.
    let connected = Rc::new(Cell::new(false));
    let status_dot = DrawingArea::new();
    status_dot.set_content_width(14);
    status_dot.set_content_height(14);
    status_dot.set_valign(Align::Center);
    {
        let connected = connected.clone();
        status_dot.set_draw_func(move |_, cr, w, h| {
            let (w, h) = (w as f64, h as f64);
            if connected.get() {
                cr.set_source_rgb(0.18, 0.80, 0.45);
            } else {
                cr.set_source_rgb(0.85, 0.25, 0.25);
            }
            cr.arc(
                w / 2.0,
                h / 2.0,
                (w.min(h) / 2.0) - 1.0,
                0.0,
                std::f64::consts::TAU,
            );
            let _ = cr.fill();
        });
    }
    let status_label = Label::new(Some("Disconnected"));
    status_label.add_css_class("status");
    let status_bar = gtk::Box::new(Orientation::Horizontal, 8);
    status_bar.set_halign(Align::Start);
    status_bar.append(&status_dot);
    status_bar.append(&status_label);

    // --- Left column: live eye position ---
    let eye_title = Label::new(Some("Eye position"));
    eye_title.add_css_class("section-title");
    eye_title.set_halign(Align::Start);

    // Eye-position box mirrors the monitor's aspect (e.g. 21:9).
    let area = DrawingArea::new();
    let box_w = 380;
    let box_h = ((box_w as f64) / screen_aspect()).round().max(80.0) as i32;
    area.set_content_width(box_w);
    area.set_content_height(box_h);
    {
        let view = view.clone();
        area.set_draw_func(move |_, cr, w, h| widget::draw_eye_view(cr, w, h, &view.borrow()));
    }

    let guidance = Label::new(None);
    guidance.add_css_class("guidance");
    guidance.set_halign(Align::Start);
    guidance.set_wrap(true);
    guidance.set_xalign(0.0);

    let left = gtk::Box::new(Orientation::Vertical, 10);
    left.set_width_request(380);
    left.append(&eye_title);
    left.append(&area);
    left.append(&guidance);

    // --- Right column: settings sections (original wording) ---
    let b_setup = Button::with_label("Set up display");
    {
        let app = app.clone();
        let cmd_tx = cmd_tx.clone();
        b_setup.connect_clicked(move |_| setup_flow::launch(&app, cmd_tx.clone()));
    }

    let sw_preview = Switch::new();
    sw_preview.set_sensitive(false);
    sw_preview.set_valign(Align::Center);
    sw_preview.set_tooltip_text(Some("Coming soon"));

    // Radio indicators with SEPARATE labels: a CheckButton's built-in label
    // clips its text on this theme, a standalone GtkLabel does not.
    let eyes_ctl = gtk::Box::new(Orientation::Horizontal, 16);
    let radio = |text: &str, group: Option<&CheckButton>| {
        let cb = CheckButton::new();
        if let Some(g) = group {
            cb.set_group(Some(g));
        }
        cb.set_sensitive(false); // enabled once Spike S4 maps the enabled_eye op
        cb.set_valign(Align::Center);
        let lbl = Label::new(Some(text));
        lbl.set_valign(Align::Center);
        lbl.set_margin_top(2);
        lbl.set_margin_bottom(2);
        let row = gtk::Box::new(Orientation::Horizontal, 5);
        row.append(&cb);
        row.append(&lbl);
        (cb, row)
    };
    let (r_both, box_both) = radio("Both eyes", None);
    r_both.set_active(true);
    let (_, box_left) = radio("Left eye only", Some(&r_both));
    let (_, box_right) = radio("Right eye only", Some(&r_both));
    eyes_ctl.append(&box_both);
    eyes_ctl.append(&box_left);
    eyes_ctl.append(&box_right);

    let b_cal = Button::with_label("Improve calibration");
    b_cal.set_sensitive(false);
    b_cal.set_tooltip_text(Some("Calibration — coming in B3"));

    let right = gtk::Box::new(Orientation::Vertical, 18);
    right.set_hexpand(true);
    right.set_valign(Align::Start);
    right.append(&section(
        "Improve my calibration",
        "If the light conditions change or if you experience less tracker precision, you might \
         benefit from improving your calibration.",
        &b_cal,
    ));
    right.append(&section(
        "Preview my gaze",
        "Shows you a visual trail of your gaze.",
        &sw_preview,
    ));
    right.append(&section(
        "Select eyes to detect",
        "If you typically squint or have poor sight in one eye, you can make the eye tracker \
         detect one eye only.",
        &eyes_ctl,
    ));
    right.append(&section(
        "Change screen",
        "If you move the sensor to a different monitor, you'll need to set up the new display.",
        &b_setup,
    ));

    // --- Two-column split ---
    let split = gtk::Box::new(Orientation::Horizontal, 30);
    split.set_hexpand(true);
    split.set_vexpand(true);
    split.append(&left);
    split.append(&right);

    let root = gtk::Box::new(Orientation::Vertical, 12);
    root.set_margin_top(20);
    root.set_margin_bottom(20);
    root.set_margin_start(24);
    root.set_margin_end(24);
    root.append(&title);
    root.append(&split);
    root.append(&status_bar);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Tobii Configuration")
        .default_width(940)
        .default_height(560)
        .build();
    window.set_child(Some(&root));

    // ~30 fps tick: read the device snapshot, refresh status + eye view.
    let tick_view = view.clone();
    glib::timeout_add_local(Duration::from_millis(33), move || {
        let snap = state.lock().unwrap().clone();
        let conn = matches!(snap.status, device::ConnStatus::Connected);
        connected.set(conn);
        status_label.set_text(if conn { "Connected" } else { "Disconnected" });
        status_dot.queue_draw();
        let ev = widget::eye_view_for(&snap);
        guidance.set_text(&widget::guidance_message(&ev));
        *tick_view.borrow_mut() = ev;
        area.queue_draw();
        glib::ControlFlow::Continue
    });

    window.present();
}

/// A settings section: bold title, wrapped description (original wording), and
/// a control widget beneath — the right-column building block.
fn section<W: IsA<gtk::Widget>>(title: &str, desc: &str, control: &W) -> gtk::Box {
    let b = gtk::Box::new(Orientation::Vertical, 6);
    let t = Label::new(Some(title));
    t.add_css_class("section-title");
    t.set_halign(Align::Start);
    let d = Label::new(Some(desc));
    d.add_css_class("section-desc");
    d.set_halign(Align::Start);
    d.set_xalign(0.0);
    d.set_wrap(true);
    control.set_halign(Align::Start);
    control.set_margin_top(4);
    b.append(&t);
    b.append(&d);
    b.append(control);
    b
}
