//! `tobii-gtk` — GTK4 configuration GUI for the Tobii ET5 Linux runtime.
//!
//! A styled hub window (status + live eye-position view) over the device
//! thread, from which the guided display-setup and calibration flows, the
//! gaze-preview overlay, and the select-eyes control are driven.

pub mod accuracy;
pub mod align;
pub mod autostart;
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
pub mod update;
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

/// The stylesheet.
///
/// This is a control panel for a measuring instrument, and it is styled as one
/// rather than as a settings app: the live trackbox is the subject, everything
/// else is a quiet rack of controls beside it.
///
/// Three rules carry that:
///
/// * **Two surfaces, not one.** A darker ground with cards raised on it, so
///   grouping is visible without boxes-inside-boxes. The old sheet painted
///   everything one flat colour and leaned on headings alone.
/// * **One accent, spent once per view.** Teal marks the action that commits;
///   everything else is a quiet surface button. Previously every button was
///   accent-filled, which made five equal settings look like five urgent calls
///   to action.
/// * **Uppercase micro-labels and monospace values.** The panel reports
///   millimetres and degrees; labelling it the way instruments are labelled is
///   truer to the subject than sentence-case captions, and monospace digits
///   stop live values from twitching as they change width.
const CSS: &str = "
/* --- ground and surfaces ------------------------------------------------ */
window { background-color: #0d1013; color: #e8ecef; }
.surface { background-color: #161a1f; border: 1px solid #232a32; border-radius: 12px; }
.panel-pad { padding: 16px; }
.hairline { background-color: #232a32; min-height: 1px; }

/* --- type --------------------------------------------------------------- */
.app-title { font-size: 20px; font-weight: bold; letter-spacing: 0.01em; }
.eyebrow { font-size: 10px; font-weight: bold; letter-spacing: 0.16em;
           color: #79838d; }
.section-title { font-size: 14px; font-weight: bold; }
.section-desc { font-size: 12px; color: #8a949d; }
.section-warn { color: #f2b134; font-weight: bold; }
.status { font-size: 12px; color: #8a949d; }
.guidance { font-size: 14px; }
.hint { font-size: 11px; color: #6f7982; }

/* --- buttons ------------------------------------------------------------ */
/* Base is QUIET. The hub is five co-equal settings; filling every one of them
   with the accent made them shout over each other and over the instrument. */
button { background-image: none; background-color: #1e242b; color: #dfe5ea;
         border: 1px solid #2b333c; border-radius: 9px; padding: 9px 16px; }
button:hover { background-color: #262e37; border-color: #37414c; }
button:active { background-color: #2c3540; }
button:disabled { background-color: #171b20; color: #5b646c; border-color: #232a32; }
button:checked { background-color: #14696b; border-color: #1f9ea0; color: #ffffff; }
/* The accent, for the one action in a view that commits. */
button.primary, button.suggested {
    background-color: #1f9ea0; color: #ffffff; border-color: #1f9ea0; }
button.primary:hover, button.suggested:hover {
    background-color: #27b4b6; border-color: #27b4b6; }
button.primary:disabled, button.suggested:disabled {
    background-color: #1a3f42; border-color: #1a3f42; color: #6d8b8c; }
/* Tertiary: outlined, no fill. Rarely the point, but still a button — an
   invisible border that only appears on hover leaves it looking like a link
   until you happen to touch it. */
button.quiet { background-color: transparent; border-color: #2b333c;
               color: #8a949d; padding: 8px 12px; }
button.quiet:hover { background-color: #1e242b; border-color: #3a444f;
                     color: #e8ecef; }
button.spin-btn { min-width: 26px; padding: 2px 10px; }
button.help-btn { min-width: 22px; padding: 0 8px; background-color: transparent;
                  border-color: transparent; color: #8a949d; font-size: 12px; }
button.help-btn:hover { background-color: #1e242b; color: #e8ecef; }
.spin-entry { padding: 2px 6px; }
.overlay-window { background-color: transparent; }
/* The scrollbar's slider needs an explicit floor. The theme sizes it with a
   negative margin over a small min-size, and against this sheet that computes
   to -2, which GTK reports once per scrollbar as
   \"GtkGizmo (slider) reported min width -2\". Stating the minimum here is the
   fix; it is not a cosmetic choice. */
scrollbar slider { min-width: 8px; min-height: 8px; }

/* --- dialogs ------------------------------------------------------------ */
.dialog-heading { font-size: 19px; font-weight: bold; }
.dialog-lead { font-size: 14px; color: #c9d1d8; }
.dialog-terms { font-size: 13px; color: #8a949d; }
.dialog-facts { font-size: 12px; color: #79838d; border-top: 1px solid #232a32;
                padding-top: 10px; margin-top: 2px; }
.dialog-note { font-size: 12px; color: #79838d; border-top: 1px solid #232a32;
               padding-top: 10px; margin-top: 2px; }
.dialog-url { font-size: 11px; }
.cal-fail-heading { font-size: 26px; font-weight: bold; }
.cal-fail-tips { font-size: 14px; color: #8a949d; }
.cal-fail-detail { font-size: 12px; color: #6f7982; font-style: italic; }
.cal-success-heading { font-size: 32px; font-weight: bold; }

/* --- the live readout ---------------------------------------------------- */
/* Monospace on the VALUES only: they change every frame, and a proportional
   font makes the column width breathe as digits change, which reads as the
   number twitching rather than the value moving. */
.readout-name { font-size: 11px; color: #79838d; letter-spacing: 0.05em; }
.readout-value { font-size: 13px; color: #e8ecef; font-family: monospace; }
.readout-alert { color: #f2b134; font-weight: bold; }

/* --- banner -------------------------------------------------------------- */
.banner { background-color: #2a2313; border: 1px solid #4a3d1a; border-radius: 10px;
          padding: 10px 12px; }
.banner-text { color: #f2b134; font-size: 13px; }
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
    // Answered before GTK is initialised, so it needs no display and opens no
    // window. The updater runs this on a freshly downloaded binary to check it
    // can execute here before replacing the installed one, and a GUI binary
    // that needs a display to say its own version would be useless for that.
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("tobii-gtk {}", env!("CARGO_PKG_VERSION"));
        return gtk::glib::ExitCode::SUCCESS;
    }

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

    // The device thread, started once. In background mode it exists before any
    // window does; otherwise the first activation creates it.
    let session: Rc<RefCell<Option<Session>>> = Rc::new(RefCell::new(None));
    // The hub window, while it is open.
    let hub: Rc<RefCell<Option<ApplicationWindow>>> = Rc::new(RefCell::new(None));
    // What keeps the process alive with no window, in background mode.
    let holder: Rc<RefCell<Option<gtk::gio::ApplicationHoldGuard>>> = Rc::new(RefCell::new(None));

    if autostart::background_mode() {
        let session = session.clone();
        let holder = holder.clone();
        app.connect_startup(move |app| {
            // No window, so nothing would otherwise keep the main loop running.
            *holder.borrow_mut() = Some(app.hold());
            // The device thread starts, but the tracker does not: `Demand` is
            // empty, so no USB session is opened and the illuminators stay off
            // until something actually asks for data.
            *session.borrow_mut() = Some(device::spawn());
        });
    }

    {
        let session = session.clone();
        let hub = hub.clone();
        app.connect_activate(move |app| {
            // A second launch — from the menu, the dock, `tobii-gtk` again —
            // hands off to this instance and lands here. Raise the hub that
            // already exists rather than building a second one.
            if let Some(w) = hub.borrow().as_ref() {
                w.present();
                return;
            }
            let s = session.borrow_mut().take().unwrap_or_else(device::spawn);
            if let Some(w) = build_hub(app, s) {
                *hub.borrow_mut() = Some(w);
            }
        });
    }
    {
        // When the hub is closed, forget it — so the next activation builds a
        // new one instead of presenting a destroyed window. In background mode
        // the process stays alive for the next time; otherwise the last window
        // closing ends it, as usual.
        let hub = hub.clone();
        app.connect_window_removed(move |_, w| {
            let is_hub = hub
                .borrow()
                .as_ref()
                .is_some_and(|h| h.upcast_ref::<gtk::Window>() == w);
            if is_hub {
                *hub.borrow_mut() = None;
            }
        });
    }
    // GApplication also parses argv itself and aborts on any option it does
    // not recognise, so our own flags have to be withheld from it.
    // `accuracy_mode` reads them straight from the environment instead.
    let gtk_args: Vec<String> = std::env::args().filter(|a| !is_our_flag(a)).collect();
    app.run_with_args(&gtk_args)
}

/// Flags this binary handles itself, which must never reach GTK's parser.
fn is_our_flag(arg: &str) -> bool {
    matches!(arg, "--accuracy" | "--version" | "-V" | "--background")
}

/// What the status line says for a device state.
///
/// "Idle" is not an error and must not read like one: it is the normal state of
/// a hub sitting in the background with the tracker deliberately switched off,
/// and a user who sees "Disconnected" there will go looking for a fault that
/// does not exist.
pub fn status_text(status: &device::ConnStatus) -> &'static str {
    match status {
        device::ConnStatus::Connected => "Connected",
        device::ConnStatus::Connecting => "Connecting…",
        device::ConnStatus::Idle => "Tracker off",
        device::ConnStatus::Error(_) => "Disconnected",
    }
}

/// Keep the tracker running for as long as `win` exists.
///
/// Every flow that needs live data runs in its own window, and the hub's own
/// claim is released the moment it loses focus to one of them — so each flow
/// has to ask for the tracker itself, for exactly as long as it is on screen.
fn hold_while_open(demand: &device::Demand, win: &impl IsA<gtk::Window>, reason: &'static str) {
    let guard = RefCell::new(Some(demand.hold(reason)));
    win.as_ref().connect_destroy(move |_| {
        *guard.borrow_mut() = None;
    });
}

/// The device thread and the handles onto it.
type Session = (
    std::sync::Arc<std::sync::Mutex<device::DeviceState>>,
    std::sync::mpsc::Sender<device::DeviceCommand>,
    device::Demand,
);

/// Whether to run the gaze-accuracy diagnostic instead of the hub.
pub(crate) fn accuracy_mode() -> bool {
    std::env::args().any(|a| a == "--accuracy")
}

/// Ask GTK to round font metrics to whole pixels.
///
/// **A targeted guess at the reported glyph clipping, not a proven fix.** The
/// tops of tall glyphs are shaved on the reporter's display, which runs at
/// fractional scale 1.15 — GTK renders at scale 2 and the compositor scales the
/// buffer back down by 0.575. Everything else has been ruled out: the
/// stylesheet, the `GSK_RENDERER=gl` override this program used to force, and
/// rendering the same widgets offscreen at 1.0/1.15/1.25/1.5/2.0, none of which
/// reproduce it.
///
/// `gtk-hint-font-metrics` exists for this situation: with it off, font metrics
/// carry fractional values through a scaled pipeline and glyph extents can land
/// a fraction of a pixel short. It costs nothing to turn on.
///
/// If the clipping survives this, the remaining suspect is the compositor's own
/// downscale, which nothing here can reach — the test for that is to put the
/// display on an integer scale (100% or 200%) and look again.
fn hint_font_metrics() {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_hint_font_metrics(true);
    }
}

/// Install this app's stylesheet on the default display.
///
/// Public so a dialog can be rendered outside the hub for a visual check —
/// GTK's own `render_texture` gives an honest picture of a widget tree without
/// a compositor screenshot, which is how this dialog's width bug was found.
pub fn load_css() {
    hint_font_metrics();
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

/// Build the hub window.
///
/// Public, together with [`load_css`], so the layout can be rendered outside a
/// normal run: GTK's own `render_texture` gives an honest picture of a widget
/// tree without a compositor screenshot, and that is how the model dialog's
/// width and the panel's margins were checked. Exposing the real constructor
/// rather than a probe-only twin means there is nothing here that only test
/// scaffolding calls.
/// Build the hub with its own device thread.
///
/// Kept for callers that just want a hub; `run` uses [`build_hub`] so it can
/// share one device thread with the background session.
pub fn build_ui(app: &Application) {
    build_hub(app, device::spawn());
}

/// Build the hub over an existing device thread.
///
/// Returns the hub window, or `None` when `--accuracy` took over and there is
/// no hub to return.
pub fn build_hub(app: &Application, session: Session) -> Option<ApplicationWindow> {
    let (state, cmd_tx, demand) = session;
    // `--accuracy` runs the gaze-accuracy diagnostic instead of the hub. It
    // needs the device thread, so it branches here rather than in `run`.
    if accuracy_mode() {
        // The diagnostic drives the tracker for its whole run, and nothing else
        // is open to ask for it, so the claim is held for the life of the
        // process rather than tied to a window.
        std::mem::forget(demand.hold("the accuracy diagnostic"));
        accuracy::launch(app, state);
        return None;
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
    // Connection state belongs beside the title, not stranded at the bottom of
    // the window: it is the first thing that decides whether anything else on
    // screen means anything.
    let status_bar = gtk::Box::new(Orientation::Horizontal, 8);
    status_bar.set_halign(Align::End);
    status_bar.set_valign(Align::Center);
    status_bar.append(&status_dot);
    status_bar.append(&status_label);

    // --- Left column: live eye position ---
    let eye_title = Label::new(Some("EYE POSITION"));
    eye_title.add_css_class("eyebrow");
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
    // Shared with the draw func below: head orientation is drawn ON the
    // eye-position box rather than in a second widget showing the same head.
    let head_view: Rc<RefCell<Option<widget::HeadView>>> = Rc::new(RefCell::new(None));
    let head_for_draw = head_view.clone();
    let area = DrawingArea::new();
    // A minimum, not a size. `widget::eye_view_rect` letterboxes the golden box
    // inside whatever it is given, so the instrument can grow with the window
    // without skewing the motion gain — which is what "fixed 380px" was
    // protecting against, at the cost of never using the space.
    let pad = 2 * widget::EYE_VIEW_PAD as i32;
    let min_w = 320;
    let min_h = (((min_w - pad) as f64) / widget::EYE_VIEW_RATIO).round() as i32 + pad;
    area.set_size_request(min_w, min_h);
    area.set_hexpand(true);
    // Height follows width, so the golden box fills the card instead of
    // pillarboxing inside a taller-than-needed allocation. Without this the
    // widget's height is whatever the layout hands it and the drawn box shrinks
    // to fit the *smaller* constraint — which is why it stayed small in a wide
    // window.
    {
        const MAX_H: i32 = 300;
        area.connect_resize(move |a, w, _h| {
            let want = ((((w - pad) as f64) / widget::EYE_VIEW_RATIO).round() as i32 + pad)
                .clamp(min_h, MAX_H);
            if a.content_height() != want {
                a.set_content_height(want);
            }
        });
    }
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
    let cam_title = Label::new(Some("SENSOR VIEW"));
    cam_title.add_css_class("eyebrow");
    cam_title.set_halign(Align::Start);
    let cam_frame: Rc<RefCell<Option<std::sync::Arc<tobii_protocol::CameraFrame>>>> =
        Rc::new(RefCell::new(None));
    let cam_area = DrawingArea::new();
    // A floor plus room to grow. `draw_camera_view` letterboxes the square
    // frame, so extra height makes the image bigger rather than adding black
    // bands — which is what lets this soak up the card's spare height instead of
    // leaving an empty gap under it.
    cam_area.set_size_request(230, 230);
    cam_area.set_hexpand(true);
    cam_area.set_vexpand(true);
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
    readout.set_row_spacing(2);
    // Two groups side by side — where the head is, and which way it faces — so
    // the readout is three rows tall rather than five. Column 2 is a spacer
    // between the groups.
    readout.set_column_spacing(10);
    // `width_chars` per group is what stops the table jumping: the values change
    // every frame, and a grid column sized to its content would resize the whole
    // readout as "good" becomes "not detected" or an angle gains a digit.
    let readout_cells: Vec<Vec<(Label, Label)>> = [(0i32, 2i32, 12), (3, 3, 9)]
        .iter()
        .map(|&(col, rows, chars)| {
            (0..rows)
                .map(|row| {
                    let name = Label::new(None);
                    name.add_css_class("readout-name");
                    name.set_halign(Align::Start);
                    name.set_xalign(0.0);
                    let value = Label::new(None);
                    value.add_css_class("readout-value");
                    value.set_halign(Align::End);
                    value.set_xalign(1.0);
                    value.set_width_chars(chars);
                    readout.attach(&name, col, row, 1, 1);
                    readout.attach(&value, col + 1, row, 1, 1);
                    (name, value)
                })
                .collect()
        })
        .collect();
    let spacer = Label::new(Some(" "));
    readout.attach(&spacer, 2, 0, 1, 1);

    // --- The instrument, as one card: trackbox, readout, camera. ---
    let instrument = gtk::Box::new(Orientation::Vertical, 12);
    instrument.add_css_class("surface");
    instrument.add_css_class("panel-pad");
    instrument.set_hexpand(true);
    // Both columns are the same height — two cards of different heights side by
    // side look like a mistake. The slack that used to sit empty below the
    // sensor view is absorbed by the sensor view itself, which expands into it.
    instrument.set_vexpand(true);
    instrument.append(&eye_title);
    instrument.append(&area);
    let rule = gtk::Box::new(Orientation::Horizontal, 0);
    rule.add_css_class("hairline");
    instrument.append(&rule);
    instrument.append(&readout);
    let rule2 = gtk::Box::new(Orientation::Horizontal, 0);
    rule2.add_css_class("hairline");
    instrument.append(&rule2);
    instrument.append(&cam_title);
    instrument.append(&cam_area);
    let left = instrument;

    // --- Right column: settings sections (original wording) ---
    let b_setup = crate::widget::button("Set up display");
    {
        let app = app.clone();
        let cmd_tx = cmd_tx.clone();
        let demand = demand.clone();
        b_setup.connect_clicked(move |_| {
            let win = setup_flow::launch(&app, cmd_tx.clone());
            hold_while_open(&demand, &win, "display setup");
        });
    }

    let sw_preview = Switch::new();
    sw_preview.set_valign(Align::Center);
    sw_preview.set_tooltip_text(Some("Show a dot on screen where you're looking"));
    {
        let app = app.clone();
        let state = state.clone();
        let overlay_win = overlay_win.clone();
        let demand = demand.clone();
        sw_preview.connect_state_set(move |_sw, on| {
            let mut ow = overlay_win.borrow_mut();
            if on {
                if ow.is_none() {
                    let w = overlay::show(&app, state.clone());
                    // The overlay is the one consumer that is useful precisely
                    // when the hub is not focused, so it must ask for the
                    // tracker in its own right.
                    hold_while_open(&demand, &w, "the gaze preview");
                    *ow = Some(w);
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
        let demand = demand.clone();
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
            hold_while_open(&demand, &win, "calibration");
            let btn = btn.clone();
            win.connect_close_request(move |_| {
                btn.set_sensitive(true);
                glib::Propagation::Proceed
            });
        });
    }

    let right = gtk::Box::new(Orientation::Vertical, 12);
    right.set_size_request(360, -1);
    right.set_valign(Align::Start);
    // Explicit, at build time. Left to the breakpoint handler alone this was
    // never set before the first layout, so the control rack absorbed every
    // spare pixel and the instrument — the thing the window is for — stayed at
    // its minimum in a wide window.
    right.set_hexpand(false);
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
    right.append(&section(
        "Start when I log in",
        "Keeps the tracker set up from login, so it works in games and other apps without \
         opening this window first. The tracker itself stays off until something asks for it.",
        &autostart_switch(),
    ));
    right.append(&section(
        "Check for updates",
        "Asks GitHub for the latest release when this window opens. It's the only thing this \
         program does on the network without being asked. Nothing is downloaded until you \
         choose to update.",
        &update_check_switch(),
    ));

    // --- Recommend-recalibration banner: a dismissible row, not a modal. Shown
    // by the tick's once-per-connection `decide()` evaluation below. ---
    let banner = gtk::Box::new(Orientation::Horizontal, 12);
    banner.add_css_class("banner");
    banner.set_visible(false);
    let banner_label = Label::new(None);
    banner_label.add_css_class("banner-text");
    banner_label.set_hexpand(true);
    banner_label.set_xalign(0.0);
    banner_label.set_wrap(true);
    banner_label.set_valign(Align::Center);
    // The banner is the one place in the hub with a call to action, so it is
    // the one place the accent is spent.
    let banner_recal = crate::widget::button("Recalibrate");
    banner_recal.add_css_class("primary");
    let banner_dismiss = crate::widget::button("Dismiss");
    banner_dismiss.add_css_class("quiet");
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
        let demand = demand.clone();
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
            hold_while_open(&demand, &win, "calibration");
            let btn = btn.clone();
            win.connect_close_request(move |_| {
                btn.set_sensitive(true);
                glib::Propagation::Proceed
            });
        });
    }

    // --- Responsive split -------------------------------------------------
    // Side by side when there is room, stacked when there is not. GTK4 has no
    // media queries, so the breakpoint is watched on the window's width and the
    // container's orientation is swapped — which is all this layout needs,
    // because both halves already expand.
    // A Grid, not a Box. In a Box the rack's natural width won over the
    // instrument's `hexpand` and it absorbed every spare pixel (measured: 590px
    // allocated against a 389px natural, while the instrument sat at its floor).
    // A Grid lets the two columns' expansion be stated per child and honoured.
    let split = gtk::Grid::new();
    split.set_hexpand(true);
    // NOT vexpand. With it, the row took the window's whole remaining height and
    // the instrument card — which fills its cell — grew past the control rack,
    // which sizes to its cards. That is why the two columns kept ending at
    // different heights however the card itself was configured: the mismatch was
    // the ROW being taller than either column needed. Sized to content, both
    // columns end level and any spare window height is plain background.
    split.set_vexpand(false);
    split.set_valign(Align::Start);
    split.set_column_spacing(16);
    split.set_row_spacing(16);
    split.attach(&left, 0, 0, 1, 1);
    split.attach(&right, 1, 0, 1, 1);

    let header = gtk::Box::new(Orientation::Horizontal, 12);
    header.append(&title);
    title.set_hexpand(true);
    header.append(&status_bar);

    // One margin all round, so the frame of background around the content is
    // even. Anything else reads as a mistake at the corners.
    const PAGE_MARGIN: i32 = 20;
    let root = gtk::Box::new(Orientation::Vertical, 14);
    root.set_margin_top(PAGE_MARGIN);
    root.set_margin_bottom(PAGE_MARGIN);
    root.set_margin_start(PAGE_MARGIN);
    root.set_margin_end(PAGE_MARGIN);
    root.append(&header);
    root.append(&update::banner());
    root.append(&banner);
    root.append(&split);

    // Set by the breakpoint block below, and re-checked from the UI tick.
    #[allow(unused_assignments)]
    let mut breakpoint: Option<Rc<dyn Fn(i32)>> = None;

    // How tall the content wants to be at the width the window will open at.
    // Measured before the window exists, so the window can be built around it.
    const DEFAULT_WIDTH: i32 = 1040;
    let (_, natural_height, _, _) = root.measure(Orientation::Vertical, DEFAULT_WIDTH);

    // Scroll rather than clip: a short window (or a stacked narrow one) must
    // still be able to reach the controls at the bottom.
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_child(Some(&root));
    scroller.set_propagate_natural_height(true);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Tobii Configuration")
        .default_width(DEFAULT_WIDTH)
        // Measured, not chosen. Asking the content how tall it wants to be at
        // this width means the background below the last card is exactly
        // `PAGE_MARGIN`, matching the sides — and it stays that way if the
        // user's font metrics differ from the ones this was written against,
        // which a hardcoded number could not.
        .default_height(natural_height)
        .build();
    window.set_child(Some(&scroller));
    // A floor, kept deliberately low. Width is the constraint that carries
    // meaning — below ~800 the two columns stop fitting side by side, which the
    // breakpoint handles by stacking them — while height only decides how much
    // scrolling there is, so there is no reason to stop the user shrinking it.
    window.set_size_request(820, 340);

    // The breakpoint. 820 is where the instrument's 340px floor plus the
    // control column's 360px floor plus margins stop fitting side by side.
    {
        // 820 is where the instrument's 320px floor and the control rack's
        // 360px floor stop fitting side by side with the margins.
        const STACK_BELOW: i32 = 820;
        let split = split.clone();
        let left_bp = left.clone();
        let right_bp = right.clone();
        let stacked_now = std::cell::Cell::new(None::<bool>);
        let apply = move |width: i32| {
            let stacked = width < STACK_BELOW;
            if stacked_now.get() == Some(stacked) {
                return;
            }
            stacked_now.set(Some(stacked));
            // Re-place both children: side by side, or one above the other.
            split.remove(&left_bp);
            split.remove(&right_bp);
            if stacked {
                split.attach(&left_bp, 0, 0, 1, 1);
                split.attach(&right_bp, 0, 1, 1, 1);
            } else {
                split.attach(&left_bp, 0, 0, 1, 1);
                split.attach(&right_bp, 1, 0, 1, 1);
            }
            // Stacked, the rack has the full width to itself and should use it;
            // side by side it must not crowd the instrument.
            right_bp.set_hexpand(stacked);
        };
        // Shared, because one signal is not enough to be sure. `default-width`
        // does not fire for every way a window can change size (tiling and
        // maximising among them), and a breakpoint that misses those is exactly
        // the "not responsive" complaint. The hub already runs a 33 ms tick, so
        // it also re-checks there — one integer compare, and `apply` returns
        // immediately when nothing changed.
        let apply: Rc<dyn Fn(i32)> = Rc::new(apply);
        apply(window.default_width());
        breakpoint = Some(apply.clone());
        window.connect_default_width_notify(move |w| apply(w.width().max(w.default_width())));
    }

    // ~30 fps tick: read the device snapshot, refresh status + eye view.
    let tick_app = app.clone();
    let tick_window = window.clone();
    let tick_cmd_tx = cmd_tx.clone();
    // A forced flow opens without the user clicking anything, so it has to ask
    // for the tracker itself just like the flows the buttons open.
    let tick_demand = demand.clone();
    let tick_banner = banner.clone();
    let tick_banner_label = banner_label.clone();
    let tick_breakpoint = breakpoint.take();
    let bp_window = window.clone();
    // Kept so the close handler can retire it: the tick captures the
    // application and can open a forced flow, so it must not outlive the hub.
    let tick_id: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    *tick_id.borrow_mut() = Some(glib::timeout_add_local(
        Duration::from_millis(33),
        move || {
            // Re-check the layout breakpoint. `notify::default-width` misses some
            // ways a window changes size (tiling, maximising), and this costs one
            // integer compare — `apply` returns immediately when nothing changed.
            if let Some(bp) = tick_breakpoint.as_ref() {
                bp(bp_window.width());
            }
            // Move the camera frame out (no 78 KB clone) and clone the rest cheaply,
            // under one lock. `new_cam` is None on the ticks between device frames.
            let (snap, new_cam) = {
                let mut s = state.lock().unwrap();
                let cam = s.latest_camera.take();
                (s.clone(), cam)
            };
            let conn = matches!(snap.status, device::ConnStatus::Connected);
            connected.set(conn);
            status_label.set_text(status_text(&snap.status));
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
                    match tobii_config::decide(
                        display_configured,
                        cal.as_ref(),
                        active.as_deref(),
                        fp,
                    ) {
                        tobii_config::CalAction::ForceSetup => launch_forced(
                            &tick_app,
                            &tick_window,
                            &forced_flow_open,
                            &cal_evaluated,
                            {
                                let cmd_tx = tick_cmd_tx.clone();
                                let demand = tick_demand.clone();
                                move |app| {
                                    let w = setup_flow::launch(app, cmd_tx.clone());
                                    hold_while_open(&demand, &w, "display setup");
                                    w
                                }
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
                                let demand = tick_demand.clone();
                                move |app| {
                                    let w = calibrate_flow::launch(
                                        app,
                                        state.clone(),
                                        cmd_tx.clone(),
                                        false,
                                    );
                                    hold_while_open(&demand, &w, "calibration");
                                    w
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
            let head_now = widget::head_view_for(&snap);
            {
                let ev = widget::eye_view_for(&snap);
                let hv = head_now;
                for (group, rows) in widget::readout_groups(&ev, hv.as_ref())
                    .into_iter()
                    .enumerate()
                {
                    for (i, r) in rows.into_iter().enumerate() {
                        let Some((n, v)) = readout_cells.get(group).and_then(|g| g.get(i)) else {
                            break;
                        };
                        n.set_text(r.label);
                        v.set_text(&r.value);
                        if r.alert {
                            v.add_css_class("readout-alert");
                        } else {
                            v.remove_css_class("readout-alert");
                        }
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
                let next = head_now;
                if *head_view.borrow() != next {
                    *head_view.borrow_mut() = next;
                    area.queue_draw();
                }
            }
            glib::ControlFlow::Continue
        },
    ));

    // The tracker runs while you are looking at the hub, and not otherwise.
    //
    // Focus rather than visibility: a hub left open on another workspace, or
    // behind a game, is not being read, and a bar of infrared LEDs glowing
    // under the monitor all day is the single most irritating thing a device
    // like this can do. `Demand`'s three-second linger absorbs the focus
    // flicker of opening a dialog, and every flow that needs the tracker
    // without the hub focused — calibration, setup, the gaze overlay — holds
    // its own claim.
    let focus_hold: Rc<RefCell<Option<device::DemandGuard>>> = Rc::new(RefCell::new(None));
    {
        let demand = demand.clone();
        let focus_hold = focus_hold.clone();
        window.connect_is_active_notify(move |w| {
            if w.is_active() {
                let mut h = focus_hold.borrow_mut();
                if h.is_none() {
                    *h = Some(demand.hold("the hub window"));
                }
            } else {
                *focus_hold.borrow_mut() = None;
            }
        });
    }
    // Presenting does not always deliver an is-active notification — on Wayland
    // it depends on the compositor granting focus — so the first claim is taken
    // here rather than waited for. If focus never arrives, the notify handler
    // drops it.
    *focus_hold.borrow_mut() = Some(demand.hold("the hub window"));
    // Everything the hub owns is released here, and all of it matters.
    {
        let focus_hold = focus_hold.clone();
        let overlay_win = overlay_win.clone();
        let tick_id = tick_id.clone();
        window.connect_close_request(move |_| {
            // 1. The tracker claim. A hub closed while focused would otherwise
            //    leave one behind for the life of the process — which in
            //    background mode is until logout, i.e. the illuminators never
            //    go out again.
            *focus_hold.borrow_mut() = None;

            // 2. The gaze overlay. It is a second ApplicationWindow of the same
            //    GtkApplication, on layer-shell's Overlay layer, click-through
            //    and with no decorations — so closing the hub used to leave a
            //    full-screen surface on top of everything with no way to
            //    dismiss it, and a GtkApplication that never exits because a
            //    window remained.
            if let Some(w) = overlay_win.borrow_mut().take() {
                w.close();
            }

            // 3. The 33 ms tick. It captures the application and can open a
            //    forced setup or calibration flow, so left running it would
            //    keep waking on a destroyed hub and could raise a fullscreen
            //    flow after the user closed the window.
            if let Some(id) = tick_id.borrow_mut().take() {
                id.remove();
            }
            glib::Propagation::Proceed
        });
    }

    window.present();
    Some(window)
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

/// The switch for starting the hub at login.
///
/// Writing the entry can fail — a read-only home directory, a full disk — and a
/// switch that silently slides back is worse than one that says why, so the
/// failure is put in the tooltip and the switch is returned to where it was.
fn autostart_switch() -> Switch {
    let sw = Switch::new();
    sw.set_valign(Align::Center);
    sw.set_active(autostart::is_enabled());
    sw.set_tooltip_text(Some(
        "Runs this program in the background at login, with no window.",
    ));
    sw.connect_state_set(|sw, on| {
        match autostart::set_enabled(on) {
            Ok(()) => {
                sw.set_tooltip_text(Some(if on {
                    "Runs this program in the background at login, with no window."
                } else {
                    "Not started at login."
                }));
                glib::Propagation::Proceed
            }
            Err(e) => {
                eprintln!("could not change the start-at-login setting: {e}");
                sw.set_tooltip_text(Some(&format!("Could not save this setting: {e}")));
                // Refuse the change rather than showing a state that is not
                // what is on disk.
                glib::Propagation::Stop
            }
        }
    });
    sw
}

/// The switch that turns the launch-time release check on and off.
///
/// The check is the program's only unprompted network request, so it needs a
/// visible off switch — not just an environment variable a user would have to
/// already know about. `TOBII_NO_UPDATE_CHECK` still wins, and the switch is
/// shown insensitive when it does, because a control that silently does nothing
/// is worse than one that explains itself.
fn update_check_switch() -> Switch {
    let sw = Switch::new();
    sw.set_valign(Align::Center);
    let forced_off = std::env::var_os("TOBII_NO_UPDATE_CHECK").is_some_and(|v| v != "0");
    sw.set_active(tobii_config::update_check_enabled());
    if forced_off {
        sw.set_sensitive(false);
        sw.set_tooltip_text(Some(
            "Turned off for this session by TOBII_NO_UPDATE_CHECK.",
        ));
        return sw;
    }
    sw.set_tooltip_text(Some("Takes effect the next time this window opens."));
    sw.connect_state_set(|_, on| {
        if let Err(e) = tobii_config::save_update_check(on) {
            eprintln!("could not save the update-check setting: {e}");
        }
        glib::Propagation::Proceed
    });
    sw
}

/// A settings section: bold title, wrapped description (original wording), and
/// a control widget beneath — the right-column building block.
/// One setting, as a card: what it is, what it does, and its control.
///
/// A card rather than a bare stack because the hub holds five co-equal
/// settings; stacked headings alone left the eye no boundary between them, so
/// the column read as one long paragraph with buttons in it.
fn section<W: IsA<gtk::Widget>>(title: &str, desc: &str, control: &W) -> gtk::Box {
    let b = gtk::Box::new(Orientation::Vertical, 6);
    b.add_css_class("surface");
    b.add_css_class("panel-pad");
    let t = Label::new(Some(title));
    t.add_css_class("section-title");
    t.set_halign(Align::Start);
    t.set_xalign(0.0);
    t.set_wrap(true);
    let d = Label::new(Some(desc));
    d.add_css_class("section-desc");
    d.set_halign(Align::Start);
    d.set_xalign(0.0);
    d.set_wrap(true);
    // A wrapping label reports its UNWRAPPED width as natural, so without a cap
    // the longest sentence in the column sets the column's width — and the
    // control rack grew until it crowded the instrument beside it.
    d.set_max_width_chars(44);
    control.set_halign(Align::Start);
    control.set_margin_top(6);
    b.append(&t);
    b.append(&d);
    b.append(control);
    b
}
