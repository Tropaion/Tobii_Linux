//! `tobii-gtk` — GTK4 configuration GUI for the Tobii ET5 Linux runtime.
//!
//! A styled hub window (status + live eye-position view) over the device
//! thread, from which the guided display-setup and calibration flows, the
//! gaze-preview overlay, and the select-eyes control are driven.

pub mod align;
pub mod calibrate_flow;
pub mod device;
pub mod eyeview;
pub mod fine_tune;
pub mod overlay;
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

use crate::eyeview::EyeView;
use tobii_protocol::EnabledEye;

const APP_ID: &str = "com.tobiilinux.Configuration";

const CSS: &str = "
window { background-color: #15181c; color: #e6e8ea; }
.app-title { font-size: 22px; font-weight: bold; }
.status { font-size: 13px; color: #9aa4ad; }
.guidance { font-size: 14px; }
.section-title { font-size: 15px; font-weight: bold; }
.section-desc { font-size: 12px; color: #9aa4ad; }
.section-warn { color: #f2b134; font-weight: bold; }
button { background-image: none; background-color: #1f9ea0; color: #ffffff;
         border: none; border-radius: 8px; padding: 10px 18px; min-height: 24px; }
button label { padding: 2px 0; }
button:hover { background-color: #26b6b8; }
button:disabled { background-color: #2a2f36; color: #6b7178; }
button:checked { background-color: #14696b; }
button.spin-btn { min-width: 26px; padding: 2px 10px; }
button.help-btn { min-width: 22px; padding: 0 8px; background-color: #2a2f36;
                  color: #9aa4ad; font-size: 12px; }
button.help-btn:hover { background-color: #3a424b; color: #e6e8ea; }
.spin-entry { padding: 2px 6px; }
.overlay-window { background-color: transparent; }
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

/// The primary monitor, if one can be resolved.
fn primary_monitor() -> Option<gtk::gdk::Monitor> {
    gtk::gdk::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_then(|obj| obj.downcast::<gtk::gdk::Monitor>().ok())
}

/// Aspect ratio (w/h) of the primary monitor, so the eye-position box mirrors
/// the screen's shape (e.g. 21:9). Falls back to 16:9.
fn screen_aspect() -> f64 {
    primary_monitor()
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

/// Primary monitor height (px), used by the fullscreen flows to place their
/// header a bit above the middle. Falls back to 1080.
pub(crate) fn screen_height() -> i32 {
    primary_monitor()
        .map(|m| m.geometry().height())
        .filter(|h| *h > 0)
        .unwrap_or(1080)
}

/// Make Esc close `win`. Closing is the flows' single exit route, so each flow's
/// own `close_request` handler still runs (that is where calibration aborts its
/// session) — this only triggers it.
pub(crate) fn add_escape_to_close(win: &ApplicationWindow) {
    let keys = gtk::EventControllerKey::new();
    let win_for_key = win.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            win_for_key.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(keys);
}

fn build_ui(app: &Application) {
    let (state, cmd_tx) = device::spawn();
    // Latest view shared with the DrawingArea's draw callback (UI thread only).
    let view = Rc::new(RefCell::new(EyeView::none()));
    // The gaze-preview overlay window, while it is open.
    let overlay_win: Rc<RefCell<Option<ApplicationWindow>>> = Rc::new(RefCell::new(None));
    // "Select eyes to detect": guard against echoing our own seeding as a user
    // change, and seed the radios from the device only once (on first connect).
    let eye_seeding = Rc::new(Cell::new(false));
    let eye_seeded = Rc::new(Cell::new(false));

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

    // Live NIR camera preview below the eye-position box (square, matches the
    // 280×280 stream). Shared frame drawn by the tick.
    let cam_title = Label::new(Some("Camera"));
    cam_title.add_css_class("section-title");
    cam_title.set_halign(Align::Start);
    let cam_frame: Rc<RefCell<Option<tobii_protocol::CameraFrame>>> = Rc::new(RefCell::new(None));
    let cam_area = DrawingArea::new();
    cam_area.set_content_width(220);
    cam_area.set_content_height(220);
    cam_area.set_halign(Align::Center);
    {
        let cam_frame = cam_frame.clone();
        cam_area.set_draw_func(move |_, cr, w, h| {
            if let Some(f) = cam_frame.borrow().as_ref() {
                widget::draw_camera_view(cr, w, h, f);
            } else {
                cr.set_source_rgb(0.05, 0.05, 0.06);
                let _ = cr.paint();
            }
        });
    }

    let left = gtk::Box::new(Orientation::Vertical, 10);
    left.set_width_request(380);
    left.append(&eye_title);
    left.append(&area);
    left.append(&guidance);
    left.append(&cam_title);
    left.append(&cam_area);

    // --- Right column: settings sections (original wording) ---
    let b_setup = Button::with_label("Set up display");
    {
        let app = app.clone();
        let cmd_tx = cmd_tx.clone();
        b_setup.connect_clicked(move |_| setup_flow::launch(&app, cmd_tx.clone()));
    }

    let sw_preview = Switch::new();
    sw_preview.set_valign(Align::Center);
    sw_preview.set_tooltip_text(Some("Show a dot on screen where you're looking"));
    {
        let app = app.clone();
        let state = state.clone();
        let overlay_win = overlay_win.clone();
        sw_preview.connect_state_set(move |_sw, on| {
            let mut ow = overlay_win.borrow_mut();
            if on {
                if ow.is_none() {
                    *ow = Some(overlay::show(&app, state.clone()));
                }
            } else if let Some(w) = ow.take() {
                w.close();
            }
            glib::Propagation::Proceed
        });
    }

    // Radio indicators with SEPARATE labels: a CheckButton's built-in label
    // clips its text on this theme, a standalone GtkLabel does not.
    let eyes_ctl = gtk::Box::new(Orientation::Horizontal, 16);
    let radio = |text: &str, group: Option<&CheckButton>| {
        let cb = CheckButton::new();
        if let Some(g) = group {
            cb.set_group(Some(g));
        }
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
    let (r_left, box_left) = radio("Left eye only", Some(&r_both));
    let (r_right, box_right) = radio("Right eye only", Some(&r_both));
    eyes_ctl.append(&box_both);
    eyes_ctl.append(&box_left);
    eyes_ctl.append(&box_right);

    // Selecting a radio pushes the choice to the device (unless we're seeding).
    for (cb, eye) in [
        (&r_both, EnabledEye::Both),
        (&r_left, EnabledEye::Left),
        (&r_right, EnabledEye::Right),
    ] {
        let cmd_tx = cmd_tx.clone();
        let seeding = eye_seeding.clone();
        cb.connect_toggled(move |c| {
            if c.is_active() && !seeding.get() {
                let _ = cmd_tx.send(device::DeviceCommand::SetEnabledEye(eye));
            }
        });
    }

    let b_cal = Button::with_label("Improve calibration");
    {
        let app = app.clone();
        let state = state.clone();
        let cmd_tx = cmd_tx.clone();
        let sw_preview = sw_preview.clone();
        b_cal.connect_clicked(move |btn| {
            // The gaze preview is a layer-shell surface on the Overlay layer,
            // which composites ABOVE a fullscreen window — the user would end
            // up chasing their own gaze dot instead of the stimulus dot, which
            // poisons every sample while still reporting success. Switching the
            // toggle off closes it through the switch's own handler, so the
            // switch and the overlay window cannot end up disagreeing.
            sw_preview.set_active(false);
            // One flow at a time: a second window would drive the same device
            // session and corrupt the first's point accounting.
            btn.set_sensitive(false);
            let win = calibrate_flow::launch(&app, state.clone(), cmd_tx.clone());
            let btn = btn.clone();
            win.connect_close_request(move |_| {
                btn.set_sensitive(true);
                glib::Propagation::Proceed
            });
        });
    }

    let b_fine = Button::with_label("Fine-tune alignment");
    {
        let app = app.clone();
        let state = state.clone();
        let cmd_tx = cmd_tx.clone();
        let sw_preview = sw_preview.clone();
        b_fine.connect_clicked(move |btn| {
            // Same reasoning as the calibration flow: the gaze preview is a
            // layer-shell surface that composites ABOVE a fullscreen window, so
            // leaving it on would put a second, unrelated dot on the target
            // cross and the user would align the wrong one.
            sw_preview.set_active(false);
            // One flow at a time — two windows would both write the display
            // geometry and the last one to Apply would silently win.
            btn.set_sensitive(false);
            let win = fine_tune::launch(&app, state.clone(), cmd_tx.clone());
            let btn = btn.clone();
            win.connect_close_request(move |_| {
                btn.set_sensitive(true);
                glib::Propagation::Proceed
            });
        });
    }

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
        "Fine-tune gaze alignment",
        "If the gaze dot is always the same distance off — a few centimetres too high, say — the \
         screen's measured position is out rather than your calibration. Look at a cross, then \
         drag the tracker's answer onto it, and the offset is corrected without a ruler.",
        &b_fine,
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
        .default_height(760)
        .build();
    window.set_child(Some(&root));

    // ~30 fps tick: read the device snapshot, refresh status + eye view.
    let tick_view = view.clone();
    glib::timeout_add_local(Duration::from_millis(33), move || {
        // Move the camera frame out (no 78 KB clone) and clone the rest cheaply,
        // under one lock. `new_cam` is None on the ticks between device frames.
        let (snap, new_cam) = {
            let mut s = state.lock().unwrap();
            let cam = s.latest_camera.take();
            (s.clone(), cam)
        };
        let conn = matches!(snap.status, device::ConnStatus::Connected);
        connected.set(conn);
        status_label.set_text(if conn { "Connected" } else { "Disconnected" });
        status_dot.queue_draw();
        // Seed the eye-selection radios once from the device's current value.
        if conn && !eye_seeded.get() {
            if let Some(e) = snap.enabled_eye {
                eye_seeding.set(true);
                match e {
                    EnabledEye::Both => r_both.set_active(true),
                    EnabledEye::Left => r_left.set_active(true),
                    EnabledEye::Right => r_right.set_active(true),
                }
                eye_seeding.set(false);
                eye_seeded.set(true);
            }
        }
        let ev = widget::eye_view_for(&snap);
        guidance.set_text(&widget::guidance_message(&ev));
        *tick_view.borrow_mut() = ev;
        area.queue_draw();
        // Camera preview: keep the last frame between device frames; update on a
        // new one; clear on disconnect so nothing stale lingers.
        if !conn {
            *cam_frame.borrow_mut() = None;
        } else if new_cam.is_some() {
            *cam_frame.borrow_mut() = new_cam;
        }
        cam_area.queue_draw();
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
