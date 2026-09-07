//! `tobii-gtk` — GTK4 configuration GUI for the Tobii ET5 Linux runtime.
//!
//! A styled hub window (status + live eye-position view) over the device
//! thread, from which the guided display-setup and calibration flows, the
//! gaze-preview overlay, and the select-eyes control are driven.

pub mod accuracy;
pub mod align;
pub mod calibrate_flow;
pub mod calibration_area;
pub mod device;
pub mod eye_preview;
pub mod eyeview;
pub mod focus;
pub mod head_model;
pub mod overlay;
pub mod particles;
pub mod screen_pick;
pub mod setup_flow;
pub mod widget;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Align, Application, ApplicationWindow, CheckButton, DrawingArea, Label, Orientation, Switch,
};

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
.cal-fail-heading { font-size: 26px; font-weight: bold; }
.cal-fail-tips { font-size: 14px; color: #9aa4ad; }
.cal-fail-detail { font-size: 12px; color: #6b7178; font-style: italic; }
.cal-success-heading { font-size: 32px; font-weight: bold; }
/* No min-height and no label padding. A previous revision claimed those two
   were clipping the tops of tall glyphs; that claim is DISPROVEN — GTK4 pushes
   no clip anywhere in the Button->Label path (only overflow:hidden does, and
   nothing here sets it), and a minimal program using this stylesheet verbatim
   renders cleanly at every renderer tested. Their absence is a simplification,
   not a fix. The clipping is a rendering-stack problem, not a CSS one: see the
   GSK_RENDERER note in `run`. Do not cite this rule as its cause. */
button { background-image: none; background-color: #1f9ea0; color: #ffffff;
         border: none; border-radius: 8px; padding: 10px 18px; }
button:hover { background-color: #26b6b8; }
button:disabled { background-color: #2a2f36; color: #6b7178; }
button:checked { background-color: #14696b; }
button.spin-btn { min-width: 26px; padding: 2px 10px; }
button.help-btn { min-width: 22px; padding: 0 8px; background-color: #2a2f36;
                  color: #9aa4ad; font-size: 12px; }
button.help-btn:hover { background-color: #3a424b; color: #e6e8ea; }
.spin-entry { padding: 2px 6px; }
.overlay-window { background-color: transparent; }
.dialog-heading { font-size: 19px; font-weight: bold; }
.dialog-lead { font-size: 14px; color: #c9d1d8; }
.dialog-terms { font-size: 13px; color: #9aa4ad; }
.dialog-facts { font-size: 12px; color: #7d868e; border-top: 1px solid #262b31;
                padding-top: 10px; margin-top: 2px; }
.dialog-url { font-size: 11px; }
/* The live-view readout. Monospace on the VALUES only: they change every frame,
   and a proportional font makes the column width breathe as digits change,
   which reads as the number twitching rather than the value moving. */
.readout { margin-top: 2px; }
.readout-name { font-size: 12px; color: #7d868e; }
.readout-value { font-size: 13px; color: #e6e8ea; font-family: monospace; }
.readout-alert { color: #f2b134; font-weight: bold; }
button.suggested { background-color: #1f9ea0; }
button.suggested:hover { background-color: #26b6b8; }
";

/// Run the GTK application.
pub fn run() -> glib::ExitCode {
    // No GSK_RENDERER override. A previous revision forced "gl" here, purely to
    // silence a cosmetic Mesa/radv "not a conformant Vulkan implementation"
    // warning — and that pinned the app to one specific renderer on a machine
    // where the tops of tall glyphs are visibly shaved off (reported repeatedly;
    // this display runs at fractional scale 1.15, which is where glyph-atlas
    // rounding bugs live). Silencing a warning is not worth overriding the
    // toolkit's own choice of renderer. The warning may come back; the text
    // matters more.
    //
    // NOT yet proven to be the cause: a minimal program using this stylesheet
    // renders cleanly under "gl" here, so if the clipping survives this change,
    // the renderer was innocent. `GSK_RENDERER=gl|ngl|vulkan|cairo` still works
    // as an override for whoever tests it next.
    let mut builder = Application::builder().application_id(APP_ID);
    if accuracy_mode() {
        // GApplication is single-instance by default: with the hub already
        // running, a second launch would hand off to it and merely raise its
        // window — in a process whose argv has no `--accuracy`, so the
        // diagnostic would silently never run.
        builder = builder.flags(gtk::gio::ApplicationFlags::NON_UNIQUE);
    }
    let app = builder.build();
    app.connect_startup(|_| load_css());
    app.connect_activate(build_ui);
    // GApplication also parses argv itself and aborts on any option it does
    // not recognise, so our own flags have to be withheld from it.
    // `accuracy_mode` reads them straight from the environment instead.
    let gtk_args: Vec<String> = std::env::args().filter(|a| !is_our_flag(a)).collect();
    app.run_with_args(&gtk_args)
}

/// Flags this binary handles itself, which must never reach GTK's parser.
fn is_our_flag(arg: &str) -> bool {
    matches!(arg, "--accuracy")
}

/// Whether to run the gaze-accuracy diagnostic instead of the hub.
pub(crate) fn accuracy_mode() -> bool {
    std::env::args().any(|a| a == "--accuracy")
}

/// Install this app's stylesheet on the default display.
///
/// Public so a dialog can be rendered outside the hub for a visual check —
/// GTK's own `render_texture` gives an honest picture of a widget tree without
/// a compositor screenshot, which is how this dialog's width bug was found.
pub fn load_css() {
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
pub(crate) fn screen_aspect() -> f64 {
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
    // `--accuracy` runs the gaze-accuracy diagnostic instead of the hub. It
    // needs the device thread, so it branches here rather than in `run`.
    if accuracy_mode() {
        accuracy::launch(app, state);
        return;
    }
    // The gaze-preview overlay window, while it is open.
    let overlay_win: Rc<RefCell<Option<ApplicationWindow>>> = Rc::new(RefCell::new(None));
    // "Select eyes to detect": guard against echoing our own seeding as a user
    // change, and seed the radios from the device only once (on first connect).
    let eye_seeding = Rc::new(Cell::new(false));
    let eye_seeded = Rc::new(Cell::new(false));
    // Gate for the once-per-connection `decide()` evaluation (force/recommend
    // calibration). Unlike `eye_seeded`, this must reset on disconnect so a
    // later reconnect — e.g. moved to a different monitor — is re-evaluated.
    let cal_evaluated = Rc::new(Cell::new(false));
    // True while a forced flow (setup or calibration) is open. Prevents a
    // disconnect/reconnect glitch mid-flow from stacking a second forced
    // window on top of the first (the disconnect branch resets
    // `cal_evaluated`, so without this the very next `Connected` tick would
    // re-enter `decide()` while the first window is still up). Cleared when
    // the flow closes, at which point `cal_evaluated` is also reset so
    // `decide()` re-runs immediately — this is what chains ForceSetup ->
    // ForceCalibration within one session instead of waiting for a reconnect.
    let forced_flow_open = Rc::new(Cell::new(false));

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

    // The trackbox is a volume in front of the sensor, not a window onto the
    // screen, so its preview must NOT take the monitor's aspect: the original
    // always stretches the normalized [0,1] box into a fixed golden-ratio
    // rectangle (`SetTrackBoxToGoldenRatio`: height = min(W,H)/2.25, width =
    // height * 1.618). Mirroring the monitor instead — as this did — badly skews
    // the motion gain on a wide screen: on this machine's 5120x1440 (3.56:1)
    // panel the drawn box came out 4.1:1, compressing vertical head movement by
    // ~2.6x relative to horizontal, which reads as the dot being sluggish
    // vertically and twitchy horizontally.
    //
    // `draw_eye_view` insets by `EYE_VIEW_PAD` on each side, so the *content*
    // size is padded out to make the DRAWN rectangle golden.
    const GOLDEN_RATIO: f64 = 1.618;
    // Shared with the draw func below: head orientation is drawn ON the
    // eye-position box rather than in a second widget showing the same head.
    let head_view: Rc<RefCell<Option<widget::HeadView>>> = Rc::new(RefCell::new(None));
    let head_for_draw = head_view.clone();
    let area = DrawingArea::new();
    let box_w = 380;
    let pad = 2 * widget::EYE_VIEW_PAD as i32;
    let box_h = (((box_w - pad) as f64) / GOLDEN_RATIO).round() as i32 + pad;
    area.set_content_width(box_w);
    area.set_content_height(box_h);
    {
        // Sample at PAINT time (as the calibration flow's preview already does)
        // rather than reading a snapshot written by the 33 ms tick: that tick is
        // slightly slower than the ~33 Hz gaze stream, so it silently dropped
        // roughly 3 frames a second — making the dot take an occasional
        // double-length step — and showed the rest up to 33 ms stale.
        let state = state.clone();
        area.set_draw_func(move |_, cr, w, h| {
            let view = widget::eye_view_for(&state.lock().unwrap());
            widget::draw_eye_view(cr, w, h, &view);
            widget::draw_head_overlay(cr, w, h, &view, head_for_draw.borrow().as_ref());
        });
    }
    // Redraw on the frame clock so every gaze frame reaches the screen exactly
    // once, instead of being resampled on the 33 ms tick. Same pattern as
    // `overlay.rs`. The original is likewise push-driven: its stream callback
    // marshals one update per frame straight onto the UI dispatcher.
    area.add_tick_callback(|a, _clock| {
        a.queue_draw();
        glib::ControlFlow::Continue
    });

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

    // One readout for the whole live view. Previously two labels sat here — a
    // guidance nudge and a head-pose sentence — reporting different halves of
    // the same measurement in different formats. A table scans faster and can
    // say "absent" where a sentence would have to omit the value silently.
    let readout = gtk::Grid::new();
    readout.add_css_class("readout");
    readout.set_halign(Align::Center);
    readout.set_column_spacing(14);
    readout.set_row_spacing(2);
    let readout_cells: Vec<(Label, Label)> = (0..5)
        .map(|row| {
            let name = Label::new(None);
            name.add_css_class("readout-name");
            name.set_halign(Align::Start);
            name.set_xalign(0.0);
            let value = Label::new(None);
            value.add_css_class("readout-value");
            value.set_halign(Align::End);
            value.set_xalign(1.0);
            readout.attach(&name, 0, row, 1, 1);
            readout.attach(&value, 1, row, 1, 1);
            (name, value)
        })
        .collect();

    let left = gtk::Box::new(Orientation::Vertical, 10);
    left.set_width_request(380);
    left.append(&eye_title);
    left.append(&area);
    left.append(&readout);
    left.append(&cam_title);
    left.append(&cam_area);

    // --- Right column: settings sections (original wording) ---
    let b_setup = crate::widget::button("Set up display");
    {
        let app = app.clone();
        let cmd_tx = cmd_tx.clone();
        b_setup.connect_clicked(move |_| {
            setup_flow::launch(&app, cmd_tx.clone());
        });
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

    let b_cal = crate::widget::button("Improve calibration");
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
            // The hub's entry is "Improve calibration": refine what is there.
            let win = calibrate_flow::launch(&app, state.clone(), cmd_tx.clone(), true);
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
        "Head tracking",
        "Sends your head position and angle to games and apps, over opentrack.",
        &head_model::control(state.clone(), cmd_tx.clone()),
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

    // --- Recommend-recalibration banner: a dismissible row, not a modal. Shown
    // by the tick's once-per-connection `decide()` evaluation below. ---
    let banner = gtk::Box::new(Orientation::Horizontal, 12);
    banner.add_css_class("section-warn"); // reuse the existing warn-color class
    banner.set_visible(false);
    let banner_label = Label::new(None);
    banner_label.set_hexpand(true);
    banner_label.set_xalign(0.0);
    let banner_recal = crate::widget::button("Recalibrate");
    let banner_dismiss = crate::widget::button("×");
    banner.append(&banner_label);
    banner.append(&banner_recal);
    banner.append(&banner_dismiss);
    {
        let banner = banner.clone();
        banner_dismiss.connect_clicked(move |_| banner.set_visible(false));
    }
    {
        let app = app.clone();
        let state = state.clone();
        let cmd_tx = cmd_tx.clone();
        let sw_preview = sw_preview.clone();
        let banner = banner.clone();
        banner_recal.connect_clicked(move |btn| {
            // Same reasoning as `b_cal`: the gaze-preview overlay would poison
            // the recalibration's own samples.
            sw_preview.set_active(false);
            // A recalibration is now in flight; the recommendation no longer
            // applies (and would otherwise reappear stale once this closes).
            banner.set_visible(false);
            btn.set_sensitive(false);
            // The banner fires when the existing calibration is no longer
            // trusted, so seeding from it would be self-defeating.
            let win = calibrate_flow::launch(&app, state.clone(), cmd_tx.clone(), false);
            let btn = btn.clone();
            win.connect_close_request(move |_| {
                btn.set_sensitive(true);
                glib::Propagation::Proceed
            });
        });
    }

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
    root.append(&banner);
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
    let tick_app = app.clone();
    let tick_window = window.clone();
    let tick_cmd_tx = cmd_tx.clone();
    let tick_banner = banner.clone();
    let tick_banner_label = banner_label.clone();
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
        // Evaluate the calibration state machine once per fresh `Connected`
        // transition (reset on disconnect so a later reconnect — e.g. moved to
        // a different monitor — is re-evaluated). All branching logic lives in
        // `tobii_config::decide`; this only computes its inputs and maps its
        // output to a UI action.
        if conn {
            if !cal_evaluated.get() && !forced_flow_open.get() {
                cal_evaluated.set(true);
                let display_configured = tobii_config::load().ok().flatten().is_some();
                let fp = tobii_config::load()
                    .ok()
                    .flatten()
                    .map(|s| s.fingerprint())
                    .unwrap_or(0);
                let cal = tobii_config::load_calibration()
                    .ok()
                    .flatten()
                    .and_then(|(_, m)| m);
                let active = device::active_monitor_id();
                match tobii_config::decide(display_configured, cal.as_ref(), active.as_deref(), fp)
                {
                    tobii_config::CalAction::ForceSetup => launch_forced(
                        &tick_app,
                        &tick_window,
                        &forced_flow_open,
                        &cal_evaluated,
                        {
                            let cmd_tx = tick_cmd_tx.clone();
                            move |app| setup_flow::launch(app, cmd_tx.clone())
                        },
                    ),
                    tobii_config::CalAction::ForceCalibration => launch_forced(
                        &tick_app,
                        &tick_window,
                        &forced_flow_open,
                        &cal_evaluated,
                        {
                            let state = state.clone();
                            let cmd_tx = tick_cmd_tx.clone();
                            move |app| {
                                calibrate_flow::launch(app, state.clone(), cmd_tx.clone(), false)
                            }
                        },
                    ),
                    tobii_config::CalAction::RecommendCalibration(reason) => {
                        show_recommend_banner(&tick_banner_label, &tick_banner, reason);
                    }
                    tobii_config::CalAction::None => {}
                }
            }
        } else {
            cal_evaluated.set(false);
        }
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
        // Only the guidance *text* is driven from this tick — the dots redraw
        // themselves on the frame clock (see `area.add_tick_callback` above).
        // Text at 33 ms is ample: it is damped over 11-49 frames upstream.
        {
            let ev = widget::eye_view_for(&snap);
            let hv = widget::head_view_for(&snap);
            for (i, (name, value, emphasis)) in widget::readout_rows(&ev, hv.as_ref())
                .into_iter()
                .enumerate()
            {
                let Some((n, v)) = readout_cells.get(i) else {
                    break;
                };
                n.set_text(name);
                v.set_text(&value);
                if emphasis {
                    v.add_css_class("readout-alert");
                } else {
                    v.remove_css_class("readout-alert");
                }
            }
        }
        // Camera preview: keep the last frame between device frames; update on a
        // new one; clear on disconnect so nothing stale lingers. Only redraw when
        // something actually changed — repainting a 78 KB frame every tick is
        // pure waste on the ticks between device frames.
        if !conn {
            let had = cam_frame.borrow().is_some();
            *cam_frame.borrow_mut() = None;
            if had {
                cam_area.queue_draw();
            }
        } else if new_cam.is_some() {
            *cam_frame.borrow_mut() = new_cam;
            cam_area.queue_draw();
        }
        // Head pose: same discipline as the camera — only redraw when the view
        // actually changed, so an unchanged pose costs nothing.
        {
            let next = widget::head_view_for(&snap);
            if *head_view.borrow() != next {
                *head_view.borrow_mut() = next;
                area.queue_draw();
            }
        }
        glib::ControlFlow::Continue
    });

    window.present();
}

/// Open a flow the user cannot skip (missing display setup or calibration):
/// disable the hub while it is open, and re-enable once it closes. Mirrors the
/// existing `b_cal`/`b_setup` single-flow-at-a-time pattern, but disables the
/// whole hub window rather than a single button, since a forced flow has no
/// button of its own to anchor to.
///
/// `forced_flow_open` is set for the lifetime of the window, so the tick's
/// `decide()` evaluation does not re-enter while a forced flow is already up
/// (e.g. a disconnect/reconnect glitch mid-setup). On close, `cal_evaluated`
/// is also reset so `decide()` re-runs on the very next tick while still
/// connected — this is what chains ForceSetup -> ForceCalibration within one
/// session (a fresh install has neither display config nor a calibration, and
/// completing setup must not wait for a reconnect before calibration is
/// forced too).
fn launch_forced(
    app: &Application,
    window: &ApplicationWindow,
    forced_flow_open: &Rc<Cell<bool>>,
    cal_evaluated: &Rc<Cell<bool>>,
    open: impl FnOnce(&Application) -> ApplicationWindow,
) {
    window.set_sensitive(false);
    forced_flow_open.set(true);
    let win = open(app);
    let hub = window.clone();
    let forced_flow_open = forced_flow_open.clone();
    let cal_evaluated = cal_evaluated.clone();
    win.connect_close_request(move |_| {
        hub.set_sensitive(true);
        forced_flow_open.set(false);
        cal_evaluated.set(false);
        glib::Propagation::Proceed
    });
}

/// Populate and show the "recommend recalibration" banner for `reason`.
fn show_recommend_banner(label: &Label, banner: &gtk::Box, reason: tobii_config::RecommendReason) {
    label.set_text(match reason {
        tobii_config::RecommendReason::OtherScreen => {
            "This calibration was made for a different screen. Recalibrate?"
        }
        tobii_config::RecommendReason::GeometryChanged => {
            "Display settings changed since calibration. Recalibrate?"
        }
    });
    banner.set_visible(true);
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
