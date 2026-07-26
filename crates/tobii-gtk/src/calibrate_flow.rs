//! Fullscreen calibration flow. Opens on a live eye-position preview (see
//! `eye_preview`); once the user holds a centered position for a moment (or
//! taps "Continue anyway") it auto-advances into the follow-the-dot sequence,
//! where each point is sampled by the device thread (see
//! `device::DeviceCommand::Cal*`) once the user's own gaze has been verified
//! (see `focus`) to actually be on the presented point — capture is no longer
//! a blind elapsed-time timer. The point sets + `CalMode` are unit-tested;
//! the GTK window + cairo dot/eye-preview are live-validated.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{cairo, Align, Application, Button, DrawingArea, Label, Orientation, Overlay};

use crate::device::{next_cal_token, CalPhase, DeviceCommand, DeviceState};
use crate::eyeview::Guidance;
use crate::{
    add_escape_to_close, calibration_area, eye_preview, focus, particles, screen_aspect,
    screen_height, widget,
};
use tobii_protocol::gaze::present;
use tobii_protocol::EnabledEye;

/// The 7-point calibration layout (normalized, top-left origin), verified
/// byte-for-byte against the decompiled real Windows software's
/// `CalibrationStateManager` constructor default (see the Phase 3 plan's Context
/// section for decompilation details). Order: center first, then six corner/edge
/// points spread across the screen with edge-aligned coordinates (not an inset
/// grid — these are the exact values from ground truth).
///
/// These are deliberately **not** run through `tobii_config::correct_gaze_x`,
/// even on a curved screen: during calibration the device's flat-plane model is
/// the reference frame both sides agree on, and pre-distorting the stimulus
/// would bake an unvalidated correction into the calibration itself. The
/// curvature correction belongs downstream, where we consume gaze (see
/// `overlay.rs`).
pub const FULL_7: [(f64, f64); 7] = [
    (0.5, 0.5),
    (0.1, 0.9),
    (0.5, 0.1),
    (0.9, 0.9),
    (0.1, 0.1),
    (0.5, 0.9),
    (0.9, 0.1),
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CalMode {
    Full,
}

impl CalMode {
    /// The stimulus points for this mode.
    pub fn points(self) -> &'static [(f64, f64)] {
        &FULL_7
    }

    /// This mode's label, as recorded into the saved `CalMeta` (see
    /// `device::DeviceCommand::CalFinish`).
    pub fn label(self) -> &'static str {
        "full"
    }
}

// Tick cadence is 33 ms (~30 fps), matching the hub.
//
// HARDWARE-OBSERVED (2026-07-21): the ET5 acks add_calibration_point almost
// immediately — it does NOT block while gathering samples, contrary to what the
// original's managed layer implied. So the fixation wait is entirely ours to
// enforce: without it a sample could be taken mid-saccade, before the user
// could even look at the dot. `SETTLE_TICKS` now means "how long gaze must be
// CONTINUOUSLY confirmed within the current point's proximity zone (see
// `focus::closest_focused_point`) before we sample it" rather than "how long
// to wait after arrival regardless of gaze" — the actual fix for the reported
// bug (dots exploding whether or not the user was looking at them).
const SETTLE_TICKS: u32 = 10; // ~330 ms of continuously-confirmed in-zone gaze

// How long a gaze-data gap (blink, brief tracking dropout) is tolerated
// without resetting the in-zone confirmation streak — matches the
// decompiled original's `GazeLeftInterval` (~250ms).
const GAZE_GAP_TOLERANCE_TICKS: u32 = 8;

// The captured-point particle burst outlives the dwell it followed — it keeps
// animating concurrently with the next point fading in (or, on the last
// point, with nothing at all — see the tick loop's unconditional age/retire
// block). ~0.6 s: long enough to read as a distinct "captured!" beat, short
// enough not to still be running when the next point is sampled.
const EXPLODE_DURATION_TICKS: u32 = 18;

// Every UI deadline below must outlast the device-thread work it is waiting on.
// If the UI gives up first the device thread keeps running the old command and
// will not dequeue the abort for the remaining difference — the window in which
// a queued CalBegin/CalFinish can still land on a session the UI has abandoned.
//
// `CalBegin` runs set_enabled_eye + retrieve_calibration + start + clear +
// (conditionally) apply_calibration — up to five requests, each bounded by
// `tobii-usb` DEFAULT_REQUEST_TIMEOUT (10 s). (Grew from three to five when
// "Improve calibration" seeding was added — this budget must stay in step
// with whatever `CalBegin`'s handler in `device.rs` actually does.)
const START_TIMEOUT_TICKS: u32 = 2000; // ~66 s waiting for the device to ack CalBegin

// One point is bounded by `tobii-usb` CAL_POINT_TIMEOUT (30 s) — keep this
// above it, or a point the USB layer would still have acked is failed here.
const COLLECT_TIMEOUT_TICKS: u32 = 1000; // ~33 s per point before giving up

// Compute+retrieve are two separate device requests, each bounded by the USB
// response deadline (`tobii-usb` DEFAULT_REQUEST_TIMEOUT, 10 s), and the device
// thread may still be finishing an earlier command before either runs. ~45 s
// stays comfortably above that worst case — keep it in step with that deadline.
const COMPUTE_TIMEOUT_TICKS: u32 = 1350; // ~45 s for compute+retrieve

/// UI-side flow state (distinct from the device's `CalPhase`).
#[derive(Clone)]
enum Phase {
    /// Live eye-position preview shown before any calibration session opens.
    /// `ticks` is total time on this step (drives the `eye_preview::message`
    /// copy and the "Continue anyway" fallback); `centered_ticks` is the
    /// currently-running streak of `Guidance::Centered` readings — tolerant
    /// of brief gaps (see `eye_preview::update_centered_streak`) — that
    /// drives auto-advance.
    EyePreview {
        ticks: u32,
        centered_ticks: u32,
        /// Gap-tolerance counter for the centered-dwell streak (see
        /// `eye_preview::update_centered_streak`).
        gap_ticks: u32,
        /// The debounced guidance actually being shown via `message()` right
        /// now (see `eye_preview::debounced_guidance`) — distinct from the
        /// raw, possibly-noisy per-frame reading.
        shown_guidance: Guidance,
        /// Consecutive ticks the raw guidance has differed from
        /// `shown_guidance` (see `eye_preview::debounced_guidance`'s
        /// `unstable_ticks` parameter).
        unstable_ticks: u32,
    },
    /// `CalBegin` sent; waiting for the device thread to publish a `CalPhase`
    /// carrying *our* `token`. The token is what makes this an edge and not a
    /// level: `active`, `collected`, `last_error` and `finished` all persist
    /// from the previous session until the device thread dequeues a command,
    /// and it can be blocked in a 30 s USB request while the UI ticks every
    /// 33 ms. Testing those fields directly would let leftovers from the last
    /// run satisfy the gate, walk every point without sampling any, and then
    /// compute + persist a calibration built from zero new points.
    Starting {
        mode: CalMode,
        token: u64,
        ticks: u32,
    },
    Collecting {
        /// The session token this phase belongs to (see `Starting`).
        token: u64,
        mode: CalMode,
        index: usize,
        /// Ticks gaze has been CONTINUOUSLY confirmed within the current
        /// point's proximity zone (see `focus::closest_focused_point`) — the
        /// actual fix for the reported bug: capture no longer fires on a blind
        /// timer, only once this reaches `SETTLE_TICKS`. Resets to 0 once a
        /// gaze-data gap has exceeded `GAZE_GAP_TOLERANCE_TICKS` (see
        /// `gap_ticks`) — a single dropped/invalid frame does not reset it.
        in_zone_ticks: u32,
        /// Ticks since the gaze-in-zone confirmation was last true this tick —
        /// used only to decide when a gap has exceeded `GAZE_GAP_TOLERANCE_TICKS`
        /// (at which point `in_zone_ticks` resets). Reset to 0 whenever gaze IS
        /// confirmed in-zone.
        gap_ticks: u32,
        /// True once `CalCollect` has been sent for `index` and we're waiting
        /// for the device to ack it (`cal.collected > index`).
        requested: bool,
        /// Overall ticks spent on this point since it was first shown — bounds
        /// the point by `COLLECT_TIMEOUT_TICKS` regardless of gaze state (a
        /// user who never looks at the dot at all must still eventually time
        /// out, same safety property as before this task).
        ticks: u32,
    },
    Computing {
        /// The session token this phase belongs to (see `Starting`).
        token: u64,
        ticks: u32,
    },
    Done(Result<(), String>),
}

/// What the cairo surface draws this frame.
struct DotView {
    point: Option<(f64, f64)>,
    /// 0 = the dot just arrived, 1 = sampling now. Drives the converging ring,
    /// which is the user's only cue for *when* fixation actually matters.
    progress: f64,
    /// The new dot's own fade-in (0 = just arrived, 1 = fully visible), ramping
    /// over `SETTLE_TICKS` — separate from `progress` (which tracks the
    /// dwell-to-sample countdown ring, not visibility).
    fade_in: f64,
    /// The *previous* point's still-animating burst, if any — drawn
    /// concurrently with the new point fading in (the captured screenshots'
    /// brief crossfade overlap, not a hard cut).
    explosion: Option<Explosion>,
}

/// A captured point's outward particle burst, aged independently of whatever
/// phase/point follows it (see the tick loop's unconditional age/retire block).
struct Explosion {
    /// Normalized point coords, same convention as `DotView::point`.
    origin: (f64, f64),
    seed: u64,
    age_ticks: u32,
}

/// Background + burst particles + the pulsing fixation dot (a ring converging
/// on a dot). `black_bg` selects the fully-black background used from
/// `Phase::Starting` onward, vs. today's dark-teal for `Phase::EyePreview`.
fn draw_scene(cr: &cairo::Context, w: i32, h: i32, dot: &DotView, black_bg: bool) {
    let (w, h) = (w as f64, h as f64);
    if black_bg {
        cr.set_source_rgb(0.0, 0.0, 0.0);
    } else {
        cr.set_source_rgb(0.08, 0.09, 0.11);
    }
    let _ = cr.paint();
    if let Some(exp) = &dot.explosion {
        let (ox, oy) = exp.origin;
        let (cx, cy) = (ox * w, oy * h);
        let t = (exp.age_ticks as f64 / EXPLODE_DURATION_TICKS as f64).clamp(0.0, 1.0);
        for p in particles::burst(exp.seed) {
            let (dx, dy, alpha) = particles::particle_pos(t, p);
            if alpha <= 0.0 {
                continue;
            }
            cr.set_source_rgba(0.30, 0.85, 0.85, alpha); // same teal accent as the dot/ring
            cr.arc(cx + dx, cy + dy, p.size, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
        }
    }
    if let Some((nx, ny)) = dot.point {
        let (cx, cy) = (nx * w, ny * h);
        // The ring shrinks onto the dot as the sample approaches: an
        // unambiguous "hold here, now" countdown. The device samples the
        // instant we ask, so the eye must already be still when it lands.
        let p = dot.progress.clamp(0.0, 1.0);
        let fade = dot.fade_in.clamp(0.0, 1.0);
        let ring = (9.0 + (1.0 - p) * 42.0) * fade;
        cr.set_source_rgba(0.30, 0.85, 0.85, 0.35 + 0.45 * p);
        cr.set_line_width(3.0);
        cr.arc(cx, cy, ring, 0.0, std::f64::consts::TAU);
        let _ = cr.stroke();
        cr.set_source_rgb(0.30, 0.85, 0.85);
        cr.arc(cx, cy, 7.0 * fade, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    }
}

/// Widgets `update_ui` toggles visibility/text on, bundled into one struct so
/// adding the failure-detail label (`fail_detail`) didn't push `update_ui` to
/// a ninth positional argument (it was already at 8, with an `#[allow]` for
/// it). `phase` and `instr` stay as `update_ui`'s own top-level parameters:
/// every arm reads/writes both, whereas each field below belongs to a subset
/// of arms (mostly just `Done`).
struct FlowWidgets<'a> {
    eye_preview_box: &'a gtk::Box,
    continue_btn: &'a Button,
    done_box: &'a gtk::Box,
    fail_box: &'a gtk::Box,
    /// The actual error string, shown small/muted under the friendly tips
    /// list (see `fail_box`'s construction in `launch`) — the tips are
    /// generic, but a bug reporter (or the user) still needs to see what
    /// specifically failed.
    fail_detail: &'a Label,
    retry: &'a Button,
    cancel: &'a Button,
}

/// Reflect the current phase in the instruction text + visible controls.
fn update_ui(phase: &Phase, instr: &Label, w: &FlowWidgets) {
    match phase {
        Phase::EyePreview { ticks, .. } => {
            // `instr`'s text is set live in the tick loop (recomputed every
            // frame from the current guidance) — don't fight it here.
            w.eye_preview_box.set_visible(true);
            w.continue_btn
                .set_visible(eye_preview::should_offer_fallback(*ticks));
            w.done_box.set_visible(false);
            w.fail_box.set_visible(false);
            w.cancel.set_visible(true);
        }
        Phase::Starting { .. } => {
            instr.set_text("Starting calibration…");
            w.eye_preview_box.set_visible(false);
            w.done_box.set_visible(false);
            w.fail_box.set_visible(false);
            w.cancel.set_visible(true);
        }
        Phase::Collecting {
            index,
            mode,
            in_zone_ticks,
            requested,
            ..
        } => {
            let progress = format!("point {} of {}", index + 1, mode.points().len());
            // Fixation matters once gaze is actually confirmed in the
            // target's zone (or a sample has already been requested) — NOT
            // once overall elapsed time on this point crosses a threshold.
            // Before gaze-verified capture, `ticks` alone was a reliable
            // proxy for "the user is looking" (the old blind timer always
            // captured within ~1.5s of arrival regardless of gaze); now
            // `in_zone_ticks` can stay 0 indefinitely if the user hasn't
            // looked at the dot yet, so checking `ticks` here would tell
            // them to "hold still" before they've even found it.
            instr.set_text(&if *in_zone_ticks > 0 || *requested {
                format!("Hold still — keep looking at the dot  ·  {progress}")
            } else {
                format!("Follow the dot with your eyes  ·  {progress}")
            });
            w.eye_preview_box.set_visible(false);
            w.done_box.set_visible(false);
            w.fail_box.set_visible(false);
            w.cancel.set_visible(true);
        }
        Phase::Computing { .. } => {
            instr.set_text("Computing your calibration…");
            w.eye_preview_box.set_visible(false);
            w.done_box.set_visible(false);
            w.fail_box.set_visible(false);
            w.cancel.set_visible(false);
        }
        Phase::Done(res) => {
            match res {
                Ok(()) => {
                    instr.set_text("Calibration successful!");
                    instr.add_css_class("cal-success-heading");
                    w.fail_box.set_visible(false);
                }
                Err(e) => {
                    // The restyled failure screen now carries its own heading/
                    // body/tips (fail_box) instead of the raw error string as
                    // the headline — but the raw string is still shown, small
                    // and muted, in `fail_detail` below the tips (see Finding 1
                    // of the final-review fix: it used to be dropped on the
                    // floor entirely).
                    instr.set_text("");
                    // Defensive only, not load-bearing: `cal-success-heading` is
                    // added only in the `Ok(())` arm above, and `Done(Ok(()))`
                    // is a terminal state for this window — retry is hidden on
                    // success (`retry.set_visible(res.is_err())` below), so the
                    // only click available is `done_btn`, which closes the
                    // window outright. There is no reachable path from
                    // `Done(Ok(()))` back into `Done(Err(_))` (or any other
                    // phase) within the same window, so this class can never
                    // actually be set when this arm runs. Removing it here
                    // costs nothing and guards against that invariant changing
                    // later.
                    instr.remove_css_class("cal-success-heading");
                    // Set fresh before `fail_box` becomes visible below, in
                    // this same call — no other arm ever makes `fail_box`
                    // visible, so there is no frame in which a stale string
                    // from an earlier attempt could be showing while the box
                    // is shown; no separate clear-on-other-arms step is
                    // needed for the *text* (only for `fail_box`'s own
                    // visibility, which every other arm above already resets).
                    w.fail_detail.set_text(e);
                    w.fail_box.set_visible(true);
                }
            }
            w.eye_preview_box.set_visible(false);
            w.done_box.set_visible(true);
            // Success offers only Done; Retry belongs to the failure screen.
            w.retry.set_visible(res.is_err());
            w.cancel.set_visible(false);
        }
    }
}

/// Wraps every `Phase::Done` construction so a failure is logged exactly
/// once (at the moment it happens, not every tick `update_ui` re-renders
/// the resulting screen) — the failure screen shows a generic, friendly
/// message, but the actual error must not be silently discarded.
fn done_phase(res: Result<(), String>) -> Phase {
    if let Err(e) = &res {
        eprintln!("calibration failed: {e}");
    }
    Phase::Done(res)
}

/// Mints a fresh calibration token, sends `CalBegin`, and returns the
/// `Phase::Starting` transition. Does not touch `phase` itself, so it is
/// safe to call whether or not the caller already holds `phase`'s RefCell
/// borrow (see the two call sites).
fn begin_calibration_phase(cmd_tx: &Sender<DeviceCommand>, eye: EnabledEye) -> Phase {
    let token = next_cal_token();
    let _ = cmd_tx.send(DeviceCommand::CalBegin { eye, token });
    Phase::Starting {
        mode: CalMode::Full,
        token,
        ticks: 0,
    }
}

/// Open the fullscreen follow-the-dot calibration flow, returning the window so
/// the caller can react to it closing (the hub re-enables its button).
pub fn launch(
    app: &Application,
    state: Arc<Mutex<DeviceState>>,
    cmd_tx: Sender<DeviceCommand>,
) -> gtk::ApplicationWindow {
    // Eye to calibrate: the device's current selection, defaulting to Both.
    let eye = state
        .lock()
        .unwrap()
        .enabled_eye
        .unwrap_or(EnabledEye::Both);

    // If the physical screen is large enough that gaze estimation near the
    // literal edge isn't reliable pre-calibration, confine every calibration
    // point to a centered sub-rectangle instead of the raw full-screen
    // positions in `CalMode::Full.points()` (which otherwise puts 6 of 7
    // points at the literal 0.1/0.9 screen edges) — matches the decompiled
    // original's `CalibrationAreaCalculator`. `tobii_config::load()`'s
    // failure/absence just means "use the full screen" (`cal_area = None`),
    // matching this flow's existing behavior before this fix — calibration
    // is only ever reachable after display setup has already succeeded, so
    // this should always find a saved setup in practice.
    let cal_area = tobii_config::load()
        .ok()
        .flatten()
        .and_then(|s| calibration_area::capped_area(s.width_mm, s.height_mm));
    let raw_points = CalMode::Full.points();
    let cal_points: [(f64, f64); 7] =
        std::array::from_fn(|i| calibration_area::remap_point(raw_points[i], cal_area));

    // The proximity radius gaze must fall within to count as "on" a point —
    // constant across the flow's lifetime (depends on the ACTUAL, possibly
    // remapped point set and the screen's aspect ratio, neither of which
    // change mid-session), so computed once here rather than every tick.
    let zone_radius = focus::zone_radius(&cal_points, screen_aspect());

    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Calibration")
        .build();
    win.set_modal(true);
    win.fullscreen();

    let phase = Rc::new(RefCell::new(Phase::EyePreview {
        ticks: 0,
        centered_ticks: 0,
        gap_ticks: 0,
        shown_guidance: Guidance::NoEyes,
        unstable_ticks: 0,
    }));
    let dot = Rc::new(RefCell::new(DotView {
        point: None,
        progress: 0.0,
        fade_in: 0.0,
        explosion: None,
    }));
    // Whether `draw_scene` paints the fully-black background (Starting onward)
    // vs. the dark-teal used during EyePreview. A separate `Cell` rather than
    // a `DotView` field: it's phase-derived state, not dot-animation state,
    // and keeping it out of `DotView` means the draw closure's single
    // `dot.borrow()` and this `Cell::get()` never contend with each other.
    let black_bg = Rc::new(Cell::new(false));

    let area = DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    {
        let dot = dot.clone();
        let black_bg = black_bg.clone();
        area.set_draw_func(move |_, cr, w, h| draw_scene(cr, w, h, &dot.borrow(), black_bg.get()));
    }

    let instr = Label::new(Some("Calibrate your eye tracker"));
    instr.add_css_class("app-title");
    instr.set_halign(Align::Center);
    instr.set_justify(gtk::Justification::Center);
    instr.set_wrap(true);

    // Small centered live preview of the user's eye position + a fallback
    // button for when they can't get centered (see eye_preview::should_offer_fallback).
    let eye_panel = DrawingArea::new();
    eye_panel.set_content_width(360);
    eye_panel.set_content_height(220);
    {
        // Separate Arc handle for this draw func; the tick loop's closure
        // below moves its own clone/the original in independently.
        let state = state.clone();
        eye_panel.set_draw_func(move |_, cr, w, h| {
            // Same dark-teal background as `draw_scene`; this panel isn't
            // full-bleed, so it paints its own background here.
            cr.set_source_rgb(0.08, 0.09, 0.11);
            let _ = cr.paint();
            let view = widget::eye_view_for(&state.lock().unwrap());
            widget::draw_eye_view(cr, w, h, &view);
        });
    }

    let continue_btn = Button::with_label("Continue anyway");
    continue_btn.set_visible(false);

    let eye_preview_box = gtk::Box::new(Orientation::Vertical, 10);
    eye_preview_box.set_halign(Align::Center);
    eye_preview_box.append(&eye_panel);
    eye_preview_box.append(&continue_btn);

    // Restyled failure screen (English translation of the captured official
    // screenshots): a heading, a short body line, and a bulleted tips list —
    // shown instead of the raw error string when calibration fails (see
    // `update_ui`'s `Phase::Done` arm).
    let fail_heading = Label::new(Some("Oops. No detection."));
    fail_heading.add_css_class("cal-fail-heading");
    fail_heading.set_halign(Align::Center);
    fail_heading.set_justify(gtk::Justification::Center);
    fail_heading.set_wrap(true);

    let fail_body = Label::new(Some(
        "Sorry about this, but the eye tracker can't detect your eyes. Let's try again and maybe follow these tips:",
    ));
    fail_body.set_halign(Align::Center);
    fail_body.set_justify(gtk::Justification::Center);
    fail_body.set_wrap(true);

    let fail_tips = Label::new(Some(
        "• Keep looking at the dot until it explodes.\n\
         • Bright and direct light is no friend of the eye tracker or your eyes. Try to avoid it.\n\
         • Remember to relax, it's ok to blink.\n\
         • If you're wearing glasses, give them a wipe.",
    ));
    fail_tips.add_css_class("cal-fail-tips");
    fail_tips.set_halign(Align::Center);
    // Left-justified within the wrap: this is a bullet list, and a left ragged
    // edge reads more naturally than centering each line independently.
    fail_tips.set_justify(gtk::Justification::Left);
    fail_tips.set_wrap(true);

    // The actual error string (e.g. "device disconnected" vs. "couldn't save
    // to disk") — small and muted so it doesn't compete with the friendly
    // tips above, but present, so it isn't silently discarded (see Finding 1
    // of the final-review fix). Text is set in `update_ui`'s `Phase::Done`
    // `Err(e)` arm.
    let fail_detail = Label::new(None);
    fail_detail.add_css_class("cal-fail-detail");
    fail_detail.set_halign(Align::Center);
    fail_detail.set_justify(gtk::Justification::Center);
    fail_detail.set_wrap(true);

    let fail_box = gtk::Box::new(Orientation::Vertical, 8);
    fail_box.set_halign(Align::Center);
    fail_box.append(&fail_heading);
    fail_box.append(&fail_body);
    fail_box.append(&fail_tips);
    fail_box.append(&fail_detail);
    fail_box.set_visible(false);

    let done_btn = Button::with_label("Done");
    let retry_btn = Button::with_label("Try again");
    let done_box = gtk::Box::new(Orientation::Horizontal, 10);
    done_box.set_halign(Align::Center);
    done_box.append(&retry_btn);
    done_box.append(&done_btn);
    done_box.set_visible(false);

    let cancel = Button::with_label("Cancel");
    cancel.set_halign(Align::Center);

    let header = gtk::Box::new(Orientation::Vertical, 16);
    header.set_halign(Align::Center);
    header.set_valign(Align::Start);
    header.set_margin_top((screen_height() as f64 * 0.30) as i32);
    header.append(&instr);
    header.append(&eye_preview_box);
    header.append(&fail_box);
    header.append(&done_box);
    header.append(&cancel);

    let overlay = Overlay::new();
    overlay.set_child(Some(&area));
    overlay.add_overlay(&header);
    win.set_child(Some(&overlay));

    // EyePreview -> begin calibration (always Full — there's only one mode
    // reachable from the UI now). The flow waits in `Starting` until the
    // device thread acknowledges the new session; it must not trust any
    // counters until then (see `Phase::Starting`). The actual token-mint +
    // `CalBegin` + `Phase::Starting` transition lives in the free function
    // `begin_calibration_phase`, shared with the tick loop's own `EyePreview`
    // arm (auto-advance) — see the comment there for why that call site can't
    // just invoke this closure directly.
    let begin_calibration: Rc<dyn Fn()> = {
        let phase = phase.clone();
        let cmd_tx = cmd_tx.clone();
        Rc::new(move || {
            // "Continue anyway" stays clickable until the next tick hides it,
            // so a double-click would otherwise send a second CalBegin and
            // issue `start` on an already-open realm. Bind the check to drop
            // the shared borrow before the borrow_mut below.
            let in_eye_preview = matches!(&*phase.borrow(), Phase::EyePreview { .. });
            if !in_eye_preview {
                return;
            }
            *phase.borrow_mut() = begin_calibration_phase(&cmd_tx, eye);
        })
    };
    {
        let b = begin_calibration.clone();
        continue_btn.connect_clicked(move |_| b());
    }
    // Retry always restarts straight into point collection (mints a fresh
    // token, sends `CalBegin`, and lands in `Phase::Starting`) — there is no
    // eye-preview/chooser step left to route back through on the failure
    // path (Tasks 6/7 removed it). This is a GTK signal callback, not the
    // tick loop, so it holds no pre-existing `phase` borrow: calling
    // `begin_calibration_phase` (which never touches `phase`) and assigning
    // its result is safe, matching `begin_calibration`'s own pattern above.
    {
        let phase = phase.clone();
        let cmd_tx = cmd_tx.clone();
        retry_btn.connect_clicked(move |_| {
            // `retry_btn` stays clickable until the next tick hides it (same
            // as `continue_btn` above), so a double-click would otherwise
            // send a second CalBegin and issue `start` on an already-open
            // realm. Bind the check to drop the shared borrow before the
            // borrow_mut below.
            let in_done = matches!(&*phase.borrow(), Phase::Done(_));
            if !in_done {
                return;
            }
            *phase.borrow_mut() = begin_calibration_phase(&cmd_tx, eye);
        });
    }
    // Every exit routes through `win.close()` so the single close handler below
    // is the one place that aborts the session and stops the tick.
    {
        let win = win.clone();
        done_btn.connect_clicked(move |_| win.close());
    }
    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }

    // Esc cancels.
    add_escape_to_close(&win);

    // Closing the window — by button, Esc, or the compositor (Alt+F4) — aborts
    // any open session and retires the tick. Without the flag the tick would
    // keep firing forever against a dead window, and could still fire CalFinish
    // and persist a calibration after the flow was gone.
    let closed = Rc::new(Cell::new(false));
    {
        let closed = closed.clone();
        let cmd_tx = cmd_tx.clone();
        win.connect_close_request(move |_| {
            closed.set(true);
            let _ = cmd_tx.send(DeviceCommand::CalAbort);
            glib::Propagation::Proceed
        });
    }

    // ~30 fps state machine: read the device's CalPhase, advance the UI phase.
    let tick_cmd = cmd_tx.clone();
    glib::timeout_add_local(Duration::from_millis(33), move || {
        if closed.get() {
            return glib::ControlFlow::Break;
        }
        let cal: CalPhase = state.lock().unwrap().calibration.clone();
        // Age out any still-animating burst every tick, independent of phase —
        // this keeps the last point's explosion finishing on its own even once
        // there is no "next point" to fade in alongside it (e.g. already into
        // `Computing`). Short-lived borrow, dropped before `phase.borrow_mut()`
        // below.
        {
            let mut d = dot.borrow_mut();
            if let Some(exp) = &mut d.explosion {
                exp.age_ticks += 1;
                if exp.age_ticks >= EXPLODE_DURATION_TICKS {
                    d.explosion = None;
                }
            }
        }
        let mut ph = phase.borrow_mut();
        let mut next: Option<Phase> = None;
        match &*ph {
            Phase::EyePreview {
                ticks,
                centered_ticks,
                gap_ticks,
                shown_guidance,
                unstable_ticks,
            } => {
                let (ticks, centered_ticks, gap_ticks, shown_guidance, unstable_ticks) = (
                    *ticks,
                    *centered_ticks,
                    *gap_ticks,
                    *shown_guidance,
                    *unstable_ticks,
                );
                dot.borrow_mut().point = None; // no calibration dot yet
                let ev = widget::eye_view_for(&state.lock().unwrap());

                let (centered_ticks, gap_ticks) =
                    eye_preview::update_centered_streak(ev.guidance, centered_ticks, gap_ticks);

                let unstable_ticks = if ev.guidance == shown_guidance {
                    0
                } else {
                    unstable_ticks + 1
                };
                let shown_guidance =
                    eye_preview::debounced_guidance(ev.guidance, shown_guidance, unstable_ticks);

                // Live guidance-derived text, recomputed every tick from the
                // DEBOUNCED guidance (not the raw reading) — unlike every
                // other phase, `update_ui` deliberately does NOT also set
                // `instr`'s text here (it would just fight this).
                instr.set_text(eye_preview::message(ticks, shown_guidance));
                eye_panel.queue_draw();
                if eye_preview::should_advance(ticks, centered_ticks) {
                    // Call the free `begin_calibration_phase` helper rather
                    // than the `begin_calibration` closure above: `ph` (a
                    // `RefMut<Phase>` from `phase.borrow_mut()` at the top of
                    // this tick) is held across this whole match, and
                    // `begin_calibration` does its own `phase.borrow()` /
                    // `borrow_mut()` — calling it from here would double-borrow
                    // and panic. `begin_calibration_phase` never touches
                    // `phase`, so it's safe here; the transition it returns is
                    // applied via `next` (assigned to `*ph` once the match
                    // returns), same as every other arm.
                    next = Some(begin_calibration_phase(&tick_cmd, eye));
                } else {
                    next = Some(Phase::EyePreview {
                        ticks: ticks + 1,
                        centered_ticks,
                        gap_ticks,
                        shown_guidance,
                        unstable_ticks,
                    });
                }
            }
            Phase::Starting { mode, token, ticks } => {
                let (mode, token, ticks) = (*mode, *token, *ticks);
                dot.borrow_mut().point = None;
                // Keep waiting while EITHER our CalBegin has not been dequeued
                // (token mismatch — nothing in `cal` is ours, so not `finished`,
                // not `last_error`, not `active`, not `collected` may be read)
                // OR it has been dequeued but `start`/`clear` are still in
                // flight. `started` — not `active` — is the "session is really
                // open" signal: `active` is set before any USB traffic, so
                // gating on it would enter Collecting against an unopened realm
                // and queue a stray point request behind a start that may fail.
                if cal.token != token || (!cal.started && cal.finished.is_none()) {
                    let t = ticks + 1;
                    if t >= START_TIMEOUT_TICKS {
                        // The abort is queued *behind* our CalBegin, so the
                        // device thread still closes whatever CalBegin opened.
                        let _ = tick_cmd.send(DeviceCommand::CalAbort);
                        next = Some(done_phase(Err(
                            "Could not start calibration. Check that the eye tracker is connected."
                                .into(),
                        )));
                    } else {
                        next = Some(Phase::Starting {
                            mode,
                            token,
                            ticks: t,
                        });
                    }
                } else if cal.started {
                    // start + clear are both acked: the session is really open
                    // and the counters below are ours, starting from zero.
                    next = Some(Phase::Collecting {
                        token,
                        mode,
                        index: 0,
                        in_zone_ticks: 0,
                        gap_ticks: 0,
                        requested: false,
                        ticks: 0,
                    });
                } else {
                    // Our CalBegin ran and its start/clear failed. `start` may
                    // still have succeeded (only `clear` failing), leaving the
                    // device in an open session — so abort explicitly.
                    let msg = match &cal.finished {
                        Some(Err(e)) => e.clone(),
                        _ => "Could not start calibration.".to_string(),
                    };
                    let _ = tick_cmd.send(DeviceCommand::CalAbort);
                    next = Some(done_phase(Err(msg)));
                }
            }
            Phase::Collecting {
                token,
                mode,
                index,
                in_zone_ticks,
                gap_ticks,
                requested,
                ticks,
            } => {
                let (token, mode, index, in_zone_ticks, gap_ticks, requested, ticks) = (
                    *token,
                    *mode,
                    *index,
                    *in_zone_ticks,
                    *gap_ticks,
                    *requested,
                    *ticks,
                );
                if cal.token != token {
                    // Another session replaced ours — only reachable if a second
                    // flow window ever opened. Never act on counters that are not
                    // ours; that is precisely what the token exists to prevent.
                    next = Some(done_phase(Err("Calibration was interrupted.".into())));
                } else if let Some(Err(e)) = &cal.finished {
                    // Defensive: within a session nothing finishes it but our
                    // own CalFinish (which leaves for `Computing`). If a finish
                    // does surface here the session may still be open, so stop
                    // it explicitly.
                    let _ = tick_cmd.send(DeviceCommand::CalAbort);
                    next = Some(done_phase(Err(e.clone())));
                } else if let Some(e) = &cal.last_error {
                    let _ = tick_cmd.send(DeviceCommand::CalAbort);
                    next = Some(done_phase(Err(format!(
                        "Couldn't read a point: {e}. Make sure you're seated and looking at the dots."
                    ))));
                } else if cal.collected > index {
                    let pts = &cal_points;
                    // A captured point always bursts, whether or not there's a
                    // next point to follow it — spawn it for the point that
                    // was JUST captured (the OLD `index`), before advancing.
                    {
                        let mut d = dot.borrow_mut();
                        d.explosion = Some(Explosion {
                            origin: pts[index],
                            seed: token ^ (index as u64),
                            age_ticks: 0,
                        });
                    }
                    if index + 1 >= pts.len() {
                        let _ = tick_cmd.send(DeviceCommand::CalFinish {
                            mode: mode.label().to_string(),
                        });
                        next = Some(Phase::Computing { token, ticks: 0 });
                    } else {
                        next = Some(Phase::Collecting {
                            token,
                            mode,
                            index: index + 1,
                            in_zone_ticks: 0,
                            gap_ticks: 0,
                            requested: false,
                            ticks: 0,
                        });
                    }
                } else {
                    let (px, py) = cal_points[index];
                    let t = ticks + 1;

                    // Is gaze right now confirmed within `index`'s proximity
                    // zone? This app shows one dot at a time, so the
                    // `calibrated` mask excludes every point except the one
                    // currently presented — `closest_focused_point` can only
                    // ever return `Some(index)` or `None` here, never a
                    // different index.
                    let calibrated_mask: Vec<bool> =
                        (0..mode.points().len()).map(|i| i != index).collect();
                    let in_zone_now = state
                        .lock()
                        .unwrap()
                        .latest_gaze
                        .as_ref()
                        .filter(|s| {
                            s.has(present::GAZE_2D) && s.validity_l == 0 && s.validity_r == 0
                        })
                        .map(|s| (s.gaze_point_2d[0], s.gaze_point_2d[1]))
                        .and_then(|g| {
                            focus::closest_focused_point(
                                g,
                                &cal_points,
                                &calibrated_mask,
                                zone_radius,
                                screen_aspect(),
                            )
                        })
                        .is_some();

                    let (new_in_zone_ticks, new_gap_ticks) = if in_zone_now {
                        (in_zone_ticks + 1, 0)
                    } else if gap_ticks + 1 < GAZE_GAP_TOLERANCE_TICKS {
                        (in_zone_ticks, gap_ticks + 1) // brief gap: hold the streak
                    } else {
                        (0, gap_ticks + 1) // gap exceeded tolerance: streak lost
                    };
                    // Only meaningful once `requested` — did we just lose a
                    // previously-held streak while waiting for the device's
                    // ack? (If `in_zone_ticks` was already 0 before this tick,
                    // there was no streak to lose — don't re-trigger a discard
                    // on every subsequent still-out-of-zone tick.)
                    //
                    // KNOWN LIMITATION: this can only see a lost streak using
                    // `state.latest_gaze`, which the device thread only
                    // refreshes between commands (`device_tick` drains its
                    // whole command queue, including any blocking
                    // `CalCollect`/`CalDiscard` USB round-trip, before it next
                    // reads notifications — see `device.rs`). While a
                    // `CalCollect` this tick loop just sent is still in
                    // flight, `latest_gaze` is frozen at whatever it was when
                    // that command was sent (which showed the user in-zone —
                    // that's why the sample was requested), so a real
                    // look-away during an unusually slow ack cannot be
                    // detected until the NEXT fresh gaze sample arrives, by
                    // which point the ack may have already landed and
                    // advanced `index`. In measured practice `add_point` acks
                    // near-instantly (see the HARDWARE-OBSERVED comment
                    // above), so this blind spot is normally far under one
                    // tick; it only widens on unusually slow hardware/USB
                    // latency, up to `CAL_POINT_TIMEOUT` (30s, `tobii-usb`).
                    // Closing it fully would need gaze visibility during a
                    // blocking device call (e.g. a non-blocking request path)
                    // — out of scope here, but worth knowing this exists.
                    let lost_focus_while_requested =
                        requested && new_in_zone_ticks == 0 && in_zone_ticks > 0;

                    {
                        let mut d = dot.borrow_mut();
                        d.point = Some((px, py));
                        d.progress = if requested {
                            1.0
                        } else {
                            (new_in_zone_ticks as f64 / SETTLE_TICKS as f64).clamp(0.0, 1.0)
                        };
                        d.fade_in = (t as f64 / SETTLE_TICKS as f64).clamp(0.0, 1.0);
                    }

                    if lost_focus_while_requested {
                        // The original's FocusChangedDuringCalibration ->
                        // discard+retry: the device may still be mid-collect
                        // for this point; ask it to discard whatever it
                        // gathered and restart the settle wait from zero,
                        // WITHOUT advancing `index` — this is the actual fix
                        // for the reported bug (a sample taken while the user
                        // looked away is no longer silently accepted).
                        //
                        // Re-check with a FRESH read, not the tick-start `cal`
                        // snapshot: `CalCollect`/`CalDiscard` are FIFO on the
                        // same device-thread queue, so if the device's ack for
                        // this point actually landed in the gap between this
                        // tick's snapshot and now, discarding it anyway would
                        // silently throw away an already-accepted sample.
                        // Skipping the discard here when the fresh read shows
                        // it already collected closes that race; the normal
                        // `cal.collected > index` branch above will pick up
                        // the advance on a later tick.
                        let already_collected = state.lock().unwrap().calibration.collected > index;
                        if !already_collected {
                            let _ = tick_cmd.send(DeviceCommand::CalDiscard { x: px, y: py });
                        }
                        next = Some(Phase::Collecting {
                            token,
                            mode,
                            index,
                            in_zone_ticks: 0,
                            gap_ticks: 0,
                            requested: false,
                            ticks: t,
                        });
                    } else if !requested && new_in_zone_ticks >= SETTLE_TICKS {
                        let _ = tick_cmd.send(DeviceCommand::CalCollect { x: px, y: py });
                        next = Some(Phase::Collecting {
                            token,
                            mode,
                            index,
                            in_zone_ticks: new_in_zone_ticks,
                            gap_ticks: new_gap_ticks,
                            requested: true,
                            ticks: t,
                        });
                    } else if t >= COLLECT_TIMEOUT_TICKS {
                        // Applies REGARDLESS of `requested` now (a deliberate
                        // behavior change from before this task): previously
                        // this timeout only fired while `requested`, because
                        // the old blind timer always reached `requested=true`
                        // within ~1.5s regardless of gaze. Now a user who
                        // never looks at the dot at all could otherwise wait
                        // here forever — this ceiling must still catch that
                        // case.
                        let _ = tick_cmd.send(DeviceCommand::CalAbort);
                        next = Some(done_phase(Err("Timed out reading a point.".into())));
                    } else {
                        next = Some(Phase::Collecting {
                            token,
                            mode,
                            index,
                            in_zone_ticks: new_in_zone_ticks,
                            gap_ticks: new_gap_ticks,
                            requested,
                            ticks: t,
                        });
                    }
                }
            }
            Phase::Computing { token, ticks } => {
                let token = *token;
                dot.borrow_mut().point = None;
                if cal.token != token {
                    // Another session replaced ours; `finished` below would be
                    // someone else's outcome, so never report it as our own.
                    next = Some(done_phase(Err("Calibration was interrupted.".into())));
                } else if let Some(res) = &cal.finished {
                    next = Some(done_phase(res.clone()));
                } else {
                    let t = ticks + 1;
                    if t >= COMPUTE_TIMEOUT_TICKS {
                        let _ = tick_cmd.send(DeviceCommand::CalAbort);
                        next = Some(done_phase(Err("Calibration computation timed out.".into())));
                    } else {
                        next = Some(Phase::Computing { token, ticks: t });
                    }
                }
            }
            Phase::Done(_) => {}
        }
        if let Some(n) = next {
            *ph = n;
        }
        // Fully black from Starting onward (matches the captured screenshots'
        // dot phases); dark-teal only during EyePreview.
        black_bg.set(!matches!(&*ph, Phase::EyePreview { .. }));
        update_ui(
            &ph,
            &instr,
            &FlowWidgets {
                eye_preview_box: &eye_preview_box,
                continue_btn: &continue_btn,
                done_box: &done_box,
                fail_box: &fail_box,
                fail_detail: &fail_detail,
                retry: &retry_btn,
                cancel: &cancel,
            },
        );
        area.queue_draw();
        glib::ControlFlow::Continue
    });

    win.present();
    win
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_sets_have_expected_counts_and_start_centered() {
        assert_eq!(CalMode::Full.points().len(), 7);
        assert_eq!(CalMode::Full.points()[0], (0.5, 0.5));
    }

    #[test]
    fn all_points_are_within_unit_square() {
        for &(x, y) in CalMode::Full.points() {
            assert!((0.0..=1.0).contains(&x), "x in range: {x}");
            assert!((0.0..=1.0).contains(&y), "y in range: {y}");
        }
    }

    #[test]
    fn full_7_has_correct_layout() {
        let pts = CalMode::Full.points();
        // Center first
        assert_eq!(pts[0], (0.5, 0.5), "point 0: center");
        // First group of three corner/edge points
        assert_eq!(pts[1], (0.1, 0.9), "point 1: bottom-left");
        assert_eq!(pts[2], (0.5, 0.1), "point 2: top-center");
        assert_eq!(pts[3], (0.9, 0.9), "point 3: bottom-right");
        // Second group of three corner/edge points
        assert_eq!(pts[4], (0.1, 0.1), "point 4: top-left");
        assert_eq!(pts[5], (0.5, 0.9), "point 5: bottom-center");
        assert_eq!(pts[6], (0.9, 0.1), "point 6: top-right");
    }
}
