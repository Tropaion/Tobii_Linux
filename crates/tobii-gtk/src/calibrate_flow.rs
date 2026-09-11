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
use crate::{
    add_escape_to_close, calibration_area, eye_preview, focus, particles, screen_aspect,
    screen_height, widget,
};
use tobii_protocol::gaze::present;

/// The 7-point calibration layout (normalized, top-left origin), verified
/// byte-for-byte against the decompiled real Windows software's
/// `CalibrationStateManager` constructor default (see the Phase 3 plan's Context
/// section for decompilation details). Order: center first, then six corner/edge
/// points spread across the screen with edge-aligned coordinates (not an inset
/// grid — these are the exact values from ground truth).
///
/// These are deliberately **not** curvature-corrected (there is no such
/// function any more — one existed and was disproved on hardware),
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

/// Per-point calibration hit-zone radius, matching the decompiled original's
/// `CalibrationProcessViewModel.SetCalibrationPointsAndUpdateZoneRadius`:
/// the real product shows these 7 points in three groups (center alone;
/// indices 1-3 together; indices 4-6 together — see `CalibrationStateManager`)
/// and recomputes the hit-zone radius fresh per group, from only that
/// group's own points. We show the group simultaneously too, and let gaze pick
/// which member is being looked at, so the tolerance must be the group's own:
/// using the spacing of all 7 points at once (as this code did before) gives
/// roughly half the correct radius for the corner points in the last group,
/// making them very hard to hold focus on.
fn group_zone_radii(points: &[(f64, f64); 7], aspect: f64) -> [f64; 7] {
    let center = focus::zone_radius(&points[0..1], aspect); // fewer than 2 points -> f64::MAX, i.e. "always in zone", matching the original's "whole screen" radius for the lone center point
    let group_b = focus::zone_radius(&points[1..4], aspect);
    let group_c = focus::zone_radius(&points[4..7], aspect);
    [center, group_b, group_b, group_b, group_c, group_c, group_c]
}

/// The three simultaneously-shown groups of the decompiled original's
/// `CalibrationStateManager`: the center point alone, then indices 1-3
/// together, then indices 4-6 together. Indices into `FULL_7`/`cal_points`/
/// `group_zone_radii`'s output.
const GROUPS: [&[usize]; 3] = [&[0], &[1, 2, 3], &[4, 5, 6]];

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
//
// USER-FEEDBACK (2026-07-26): raised from 10 to 30 (~330ms -> ~990ms, roughly
// a full second) after direct hands-on comparison against the real Windows
// product: the 10-tick figure captured "too fast", reading as instantaneous
// rather than a deliberate hold ("user should have to focus a bit longer,
// like in the original"). Unlike `EXPLODE_DURATION_TICKS` below (matched
// byte-for-byte against the decompiled particle-storyboard timing), this
// duration is NOT independently ground-truth-verified — the real per-point
// dwell/sampling duration lives inside the native SDK's blocking
// `add_calibration_point` call, not in the decompiled managed layer (whose
// own `CalibratePointAsync` shows only a 200ms `Task.Delay` before invoking
// that native call, which says nothing about real-hardware dwell time). 30
// is a reasoned response to hands-on feedback, not a decompiled figure.
const SETTLE_TICKS: u32 = 30; // ~990 ms of continuously-confirmed in-zone gaze

// How long a gaze-data gap (blink, brief tracking dropout) is tolerated
// without resetting the in-zone confirmation streak — matches the
// decompiled original's `GazeLeftInterval` (~250ms).
const GAZE_GAP_TOLERANCE_TICKS: u32 = 8;

// The captured-point particle burst outlives the dwell it followed — it keeps
// animating concurrently with the next point fading in (or, on the last
// point, with nothing at all — see the tick loop's unconditional age/retire
// block). ~0.8 s, matching the decompiled original's per-particle storyboard
// (`CalibrationProcessStoryboardFactory.CreateParticleCalibratedAnimation`):
// particles fully fade out anywhere from ~400ms to ~799ms after capture,
// staggered by each particle's own random `FadeBeginTime` — long enough to
// read as a distinct "captured!" beat, short enough not to still be running
// when the next point is sampled.
const EXPLODE_DURATION_TICKS: u32 = 24;

// Every UI deadline below must outlast the device-thread work it is waiting on.
// If the UI gives up first the device thread keeps running the old command and
// will not dequeue the abort for the remaining difference — the window in which
// a queued CalBegin/CalFinish can still land on a session the UI has abandoned.
//
// `CalBegin` runs start + clear, and for an "improve" run also
// retrieve_calibration + apply_calibration — up to four requests. The apply
// carries a few hundred KB and has its own 60 s window (`CAL_APPLY_TIMEOUT`),
// so this budget must stay in step with what `CalBegin`'s handler in
// `device.rs` actually does.
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
    ///
    /// The guidance this reads is already damped against per-frame chatter by
    /// `eyeview::EyeHistory`, which does it per gaze frame exactly as the
    /// original software does (in its stream callback, not its UI timer), so
    /// this phase holds no debounce state of its own.
    EyePreview {
        ticks: u32,
        centered_ticks: u32,
        /// Gap-tolerance counter for the centered-dwell streak (see
        /// `eye_preview::update_centered_streak`).
        gap_ticks: u32,
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
        /// Which of `GROUPS` is currently active (0, 1, or 2).
        group: usize,
        /// Which of the 7 points have been captured so far (persists across
        /// groups — a point's `true` never reverts).
        calibrated: [bool; 7],
        /// The point (global 0..7 index) currently established as the dwell
        /// target within the active group, if any — see
        /// `focus::resolve_group_focus`.
        focused: Option<usize>,
        /// Focus-change debounce counter for `focused` (see
        /// `focus::resolve_group_focus`'s `pending_ticks` parameter).
        pending_ticks: u32,
        /// Ticks gaze has been CONTINUOUSLY confirmed in `focused`'s zone —
        /// meaningless while `focused` is `None`. Same role as before this
        /// task, just no longer tied to a fixed sequential index.
        in_zone_ticks: u32,
        /// Ticks since the gaze-in-zone confirmation was last true this tick —
        /// used only to decide when a gap has exceeded `GAZE_GAP_TOLERANCE_TICKS`
        /// (at which point `in_zone_ticks` resets). Reset to 0 whenever gaze IS
        /// confirmed in-zone.
        gap_ticks: u32,
        /// True once `CalCollect` has been sent for `focused` and we're
        /// waiting for the device to ack it. While `true`, `focused` must NOT
        /// be switched away from by `resolve_group_focus` — see the tick arm
        /// below for how this is enforced (only run focus-resolution when NOT
        /// `requested`, exactly like the pre-existing single-point design
        /// did).
        requested: bool,
        /// Elapsed ticks since the ACTIVE GROUP first appeared (not
        /// per-point) — drives the whole group's simultaneous fade-in and the
        /// group-level timeout (see `COLLECT_TIMEOUT_TICKS` usage below).
        ticks: u32,
    },
    Computing {
        /// The session token this phase belongs to (see `Starting`).
        token: u64,
        ticks: u32,
    },
    Done(Result<(), String>),
}

/// One currently-visible, not-yet-captured point in the active group.
struct GroupPoint {
    point: (f64, f64),
    /// This point's OWN dwell-to-sample countdown ring progress (0..1) —
    /// nonzero ONLY for whichever point is currently `focused`; 0 for its
    /// still-visible, not-yet-focused siblings.
    progress: f64,
}

/// What the cairo surface draws this frame.
struct DotView {
    /// The active group's not-yet-captured points, all sharing one
    /// simultaneous fade-in (the group appears together, not one at a time —
    /// see `fade_in`).
    points: Vec<GroupPoint>,
    /// 0 = the group just arrived, 1 = fully visible — shared by every point
    /// in `points` (they fade in together as a group).
    fade_in: f64,
    /// Every still-animating burst — plural because two points in the same
    /// group can be captured close together, each getting its own
    /// independently-aged explosion.
    explosions: Vec<Explosion>,
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
    for exp in &dot.explosions {
        let (ox, oy) = exp.origin;
        let (cx, cy) = (ox * w, oy * h);
        let t = (exp.age_ticks as f64 / EXPLODE_DURATION_TICKS as f64).clamp(0.0, 1.0);

        for p in particles::burst(exp.seed) {
            let (dx, dy, alpha) = particles::particle_pos(t, p);
            if alpha <= 0.0 {
                continue;
            }
            // Every particle draws in the same uniform base teal accent color
            // — matching the decompiled original's `GenerateParticles()`,
            // where all 20 particles share one `Fill` brush with no
            // per-particle color/brightness variation.
            cr.set_source_rgba(0.30, 0.85, 0.85, alpha);
            // Matches the decompiled original's storyboard
            // (`CreateParticleCalibratedAnimation`): each particle's scale
            // animates to 50% over just ~10ms, then holds there for the rest
            // of the burst — it does not continue shrinking gradually
            // throughout. `0.05` approximates that ~10ms/~800ms ratio of the
            // burst's total duration.
            let size_now = if t < 0.05 {
                p.size * (1.0 - 0.5 * (t / 0.05))
            } else {
                p.size * 0.5
            };
            cr.arc(cx + dx, cy + dy, size_now, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
        }
    }
    for gp in &dot.points {
        let (nx, ny) = gp.point;
        let (cx, cy) = (nx * w, ny * h);
        // The ring shrinks onto the dot as the sample approaches: an
        // unambiguous "hold here, now" countdown. The device samples the
        // instant we ask, so the eye must already be still when it lands.
        let p = gp.progress.clamp(0.0, 1.0);
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
            calibrated,
            focused,
            requested,
            ..
        } => {
            let done = calibrated.iter().filter(|c| **c).count();
            let progress = format!("{done} of {} points", calibrated.len());
            // Fixation matters once gaze is actually confirmed on a target
            // (or a sample has already been requested) — NOT once overall
            // elapsed time on the group crosses a threshold. `focused.is_some()`
            // plays the same role `in_zone_ticks > 0` played before this task
            // (equivalent once `in_zone_ticks` only ever advances while
            // `focused` is established): before gaze-verified capture, `ticks`
            // alone was a reliable proxy for "the user is looking"; now
            // nothing may be focused yet if the user hasn't found a dot in
            // the newly-shown group.
            instr.set_text(&if focused.is_some() || *requested {
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
        tobii_diagnostics::log::warn(&format!("calibration failed: {e}"));
    }
    Phase::Done(res)
}

/// Mints a fresh calibration token, sends `CalBegin`, and returns the
/// `Phase::Starting` transition. Does not touch `phase` itself, so it is
/// safe to call whether or not the caller already holds `phase`'s RefCell
/// borrow (see the two call sites).
fn begin_calibration_phase(cmd_tx: &Sender<DeviceCommand>, improve: bool) -> Phase {
    let token = next_cal_token();
    let _ = cmd_tx.send(DeviceCommand::CalBegin { improve, token });
    Phase::Starting {
        mode: CalMode::Full,
        token,
        ticks: 0,
    }
}

/// Open the fullscreen follow-the-dot calibration flow, returning the window so
/// the caller can react to it closing (the hub re-enables its button).
/// `improve` refines the calibration already on the device instead of starting
/// clean — the hub's "Improve calibration" entry. The original gates its own
/// seeding on exactly this distinction (`ShouldImproveCalibration`), because a
/// user recalibrating *because the model is bad* should not be handed that model
/// back as a starting point.
pub fn launch(
    app: &Application,
    state: Arc<Mutex<DeviceState>>,
    cmd_tx: Sender<DeviceCommand>,
    improve: bool,
) -> gtk::ApplicationWindow {
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
    // Sized from the user's measured distance so the outermost stimulus lands
    // on the gaze angle this device still resolves, rather than on a fixed
    // 600mm — see `calibration_area::capped_area_at`. The eye preview has just
    // been showing live eye data, so a distance is normally to hand; without
    // one that helper falls back to Tobii's fixed area rather than guessing a
    // number the answer scales directly with.
    let distance_mm = state
        .lock()
        .ok()
        .and_then(|s| s.eye_view.and_then(|v| v.distance_mm))
        .map(f64::from);
    let cal_area = tobii_config::load()
        .ok()
        .flatten()
        .and_then(|s| calibration_area::capped_area_at(s.width_mm, s.height_mm, distance_mm));
    let raw_points = CalMode::Full.points();
    let cal_points: [(f64, f64); 7] =
        std::array::from_fn(|i| calibration_area::remap_point(raw_points[i], cal_area));

    // The proximity radius gaze must fall within to count as "on" a point —
    // one radius per point (see `group_zone_radii`; the original recomputes
    // this per simultaneously-shown group rather than once for all 7 points),
    // constant across the flow's lifetime (depends on the ACTUAL, possibly
    // remapped point set and the screen's aspect ratio, neither of which
    // change mid-session), so computed once here rather than every tick.
    let zone_radii = group_zone_radii(&cal_points, screen_aspect());

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
    }));
    let dot = Rc::new(RefCell::new(DotView {
        points: Vec::new(),
        fade_in: 0.0,
        explosions: Vec::new(),
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
    // Golden-ratio trackbox, matching the original's `SetTrackBoxToGoldenRatio`
    // (see the hub's own box in `lib.rs` for why the monitor's aspect is wrong
    // here). Padded out by `draw_eye_view`'s inset so the DRAWN box is 1.618:1.
    let eye_panel = DrawingArea::new();
    let eye_pad = 2 * widget::EYE_VIEW_PAD as i32;
    eye_panel.set_content_width(360);
    eye_panel.set_content_height((((360 - eye_pad) as f64) / 1.618).round() as i32 + eye_pad);
    // Redraw on the frame clock, so each gaze frame is shown exactly once
    // rather than being resampled by the 33 ms tick (which is marginally slower
    // than the ~33 Hz stream and so drops ~3 frames a second).
    eye_panel.add_tick_callback(|p, _clock| {
        p.queue_draw();
        glib::ControlFlow::Continue
    });
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

    let continue_btn = crate::widget::button("Continue anyway");
    continue_btn.add_css_class("primary");
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

    let done_btn = crate::widget::button("Done");
    done_btn.add_css_class("primary");
    let retry_btn = crate::widget::button("Try again");
    retry_btn.add_css_class("primary");
    let done_box = gtk::Box::new(Orientation::Horizontal, 10);
    done_box.set_halign(Align::Center);
    done_box.append(&retry_btn);
    done_box.append(&done_btn);
    done_box.set_visible(false);

    let cancel = crate::widget::button("Cancel");
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
            *phase.borrow_mut() = begin_calibration_phase(&cmd_tx, improve);
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
            *phase.borrow_mut() = begin_calibration_phase(&cmd_tx, improve);
        });
    }
    // Every exit routes through `win.close()` so the single close handler below
    // is the one place that aborts the session and stops the tick.
    //
    // Weak references, because both buttons live inside the window: a strong
    // one is a cycle, and the window outlived its own closing — for the life of
    // the process, with everything it holds.
    {
        let win = win.downgrade();
        done_btn.connect_clicked(move |_| {
            if let Some(w) = win.upgrade() {
                w.close();
            }
        });
    }
    {
        let win = win.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(w) = win.upgrade() {
                w.close();
            }
        });
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
            for exp in &mut d.explosions {
                exp.age_ticks += 1;
            }
            d.explosions
                .retain(|exp| exp.age_ticks < EXPLODE_DURATION_TICKS);
        }
        let mut ph = phase.borrow_mut();
        let mut next: Option<Phase> = None;
        match &*ph {
            Phase::EyePreview {
                ticks,
                centered_ticks,
                gap_ticks,
            } => {
                let (ticks, centered_ticks, gap_ticks) = (*ticks, *centered_ticks, *gap_ticks);
                dot.borrow_mut().points.clear(); // no calibration dot yet
                let ev = widget::eye_view_for(&state.lock().unwrap());

                let (centered_ticks, gap_ticks) =
                    eye_preview::update_presence_streak(ev.guidance, centered_ticks, gap_ticks);

                // Live guidance-derived text, recomputed every tick. The
                // guidance is already damped per gaze frame by
                // `eyeview::EyeHistory`, so it is stable enough to show
                // directly — unlike every other phase, `update_ui`
                // deliberately does NOT also set `instr`'s text here (it would
                // just fight this).
                instr.set_text(eye_preview::message(ticks, ev.guidance));
                // No `eye_panel.queue_draw()` here — the panel drives its own
                // redraws off the frame clock (see its `add_tick_callback`).
                if eye_preview::should_advance(ticks, centered_ticks, ev.guidance) {
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
                    next = Some(begin_calibration_phase(&tick_cmd, improve));
                } else {
                    next = Some(Phase::EyePreview {
                        ticks: ticks + 1,
                        centered_ticks,
                        gap_ticks,
                    });
                }
            }
            Phase::Starting { mode, token, ticks } => {
                let (mode, token, ticks) = (*mode, *token, *ticks);
                dot.borrow_mut().points.clear();
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
                        group: 0,
                        calibrated: [false; 7],
                        focused: None,
                        pending_ticks: 0,
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
                group,
                calibrated,
                focused,
                pending_ticks,
                in_zone_ticks,
                gap_ticks,
                requested,
                ticks,
            } => {
                let (
                    token,
                    mode,
                    group,
                    calibrated,
                    focused,
                    pending_ticks,
                    in_zone_ticks,
                    gap_ticks,
                    requested,
                    ticks,
                ) = (
                    *token,
                    *mode,
                    *group,
                    *calibrated,
                    *focused,
                    *pending_ticks,
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
                } else {
                    let active_group = GROUPS[group];
                    // The device's `cal.collected` is a plain counter of how
                    // many `add_point` calls have succeeded so far, total —
                    // not indexed by which point. Comparing it against the
                    // total captured-so-far tells us a NEW capture landed
                    // this tick.
                    let total_captured = calibrated.iter().filter(|c| **c).count();

                    if cal.collected > total_captured {
                        // Capture requests are issued ONE AT A TIME, only
                        // ever for whichever point is `focused` when
                        // `requested` is set — and focus-resolution below
                        // never runs while `requested` is true, so `focused`
                        // cannot have moved since we sent that request.
                        // Reading it here is therefore safe and correct.
                        let captured = focused.expect(
                            "cal.collected increased implies a CalCollect is in flight, which implies focused is set",
                        );
                        let mut calibrated = calibrated;
                        calibrated[captured] = true; // accumulates only — never reverts to false
                        {
                            let mut d = dot.borrow_mut();
                            d.explosions.push(Explosion {
                                origin: cal_points[captured],
                                seed: token ^ (captured as u64),
                                age_ticks: 0,
                            });
                            // Drop the just-captured point from the displayed
                            // set immediately, in this same tick — otherwise
                            // it would still render its dwell ring for one
                            // extra frame alongside its own explosion, since
                            // `d.points` is only otherwise recomputed in the
                            // no-capture-this-tick branch below.
                            d.points.retain(|gp| gp.point != cal_points[captured]);
                        }

                        let group_done = active_group.iter().all(|&i| calibrated[i]);
                        if group_done && group == GROUPS.len() - 1 {
                            let _ = tick_cmd.send(DeviceCommand::CalFinish {
                                mode: mode.label().to_string(),
                            });
                            next = Some(Phase::Computing { token, ticks: 0 });
                        } else if group_done {
                            // Fit and apply what this group gathered before
                            // showing the next one, as the original does. The
                            // device thread runs commands in order, so the next
                            // group's first capture queues behind this compute
                            // rather than racing it.
                            let _ = tick_cmd.send(DeviceCommand::CalComputeGroup);
                            next = Some(Phase::Collecting {
                                token,
                                mode,
                                group: group + 1,
                                calibrated,
                                focused: None,
                                pending_ticks: 0,
                                in_zone_ticks: 0,
                                gap_ticks: 0,
                                requested: false,
                                ticks: 0, // fresh fade-in for the new group
                            });
                        } else {
                            // Points remain in this group: stay on it (same
                            // `ticks` — the group's fade-in does not restart)
                            // so gaze can settle on one of the siblings.
                            next = Some(Phase::Collecting {
                                token,
                                mode,
                                group,
                                calibrated,
                                focused: None,
                                pending_ticks: 0,
                                in_zone_ticks: 0,
                                gap_ticks: 0,
                                requested: false,
                                ticks,
                            });
                        }
                    } else {
                        let t = ticks + 1;

                        // A point counts as "already calibrated" (excluded
                        // from focus consideration) if it's genuinely
                        // captured OR simply not a member of the active
                        // group — points outside the current group must
                        // never be considered focusable even though they
                        // aren't literally captured yet.
                        let calibrated_mask: [bool; 7] =
                            std::array::from_fn(|i| calibrated[i] || !active_group.contains(&i));

                        // Every member of a given group shares one radius by
                        // construction (see `group_zone_radii`'s own tests),
                        // so any member's index is a valid representative —
                        // `active_group[0]` is simplest and always valid
                        // since every group has at least one member.
                        let raw_focus: Option<usize> = state
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
                                    zone_radii[active_group[0]],
                                    screen_aspect(),
                                )
                            });

                        // Focus resolution only runs while no capture is in
                        // flight — while `requested`, `focused` must not
                        // move, exactly like the pre-existing single-point
                        // design never advanced `index` while `requested`.
                        let (new_focused, new_pending_ticks) = if requested {
                            (focused, pending_ticks)
                        } else {
                            focus::resolve_group_focus(raw_focus, focused, pending_ticks)
                        };

                        // A real focus transition (gaining, losing, or
                        // switching) resets the dwell streak — a fresh point
                        // to dwell on, or nothing to dwell on at all now.
                        // While `requested`, `new_focused == focused` always,
                        // so this never fires mid-capture.
                        let (base_in_zone_ticks, base_gap_ticks) = if new_focused != focused {
                            (0, 0)
                        } else {
                            (in_zone_ticks, gap_ticks)
                        };

                        // Gaze is confirmed in-zone on the established focus
                        // THIS tick if the raw reading matches it — same
                        // gap-tolerance mechanic as before this task, just no
                        // longer tied to a fixed sequential index. While
                        // nothing is focused there is nothing to dwell on.
                        let in_zone_now = new_focused.is_some() && raw_focus == new_focused;
                        let (new_in_zone_ticks, new_gap_ticks) = if new_focused.is_none() {
                            (0, 0)
                        } else if in_zone_now {
                            (base_in_zone_ticks + 1, 0)
                        } else if base_gap_ticks + 1 < GAZE_GAP_TOLERANCE_TICKS {
                            (base_in_zone_ticks, base_gap_ticks + 1) // brief gap: hold the streak
                        } else {
                            (0, base_gap_ticks + 1) // gap exceeded tolerance: streak lost
                        };
                        // Only meaningful once `requested` — did we just lose
                        // a previously-held streak while waiting for the
                        // device's ack? (If `in_zone_ticks` was already 0
                        // before this tick, there was no streak to lose —
                        // don't re-trigger a discard on every subsequent
                        // still-out-of-zone tick.)
                        //
                        // KNOWN LIMITATION (unchanged from before this task):
                        // this can only see a lost streak using
                        // `state.latest_gaze`, which the device thread only
                        // refreshes between commands (`device_tick` drains
                        // its whole command queue, including any blocking
                        // `CalCollect`/`CalDiscard` USB round-trip, before it
                        // next reads notifications — see `device.rs`). While
                        // a `CalCollect` this tick loop just sent is still in
                        // flight, `latest_gaze` is frozen at whatever it was
                        // when that command was sent (which showed the user
                        // in-zone — that's why the sample was requested), so
                        // a real look-away during an unusually slow ack
                        // cannot be detected until the NEXT fresh gaze sample
                        // arrives, by which point the ack may have already
                        // landed and advanced past this point. In measured
                        // practice `add_point` acks near-instantly (see the
                        // HARDWARE-OBSERVED comment above), so this blind
                        // spot is normally far under one tick; it only widens
                        // on unusually slow hardware/USB latency, up to
                        // `CAL_POINT_TIMEOUT` (30s, `tobii-usb`). Closing it
                        // fully would need gaze visibility during a blocking
                        // device call (e.g. a non-blocking request path) —
                        // out of scope here, but worth knowing this exists.
                        let lost_focus_while_requested =
                            requested && new_in_zone_ticks == 0 && in_zone_ticks > 0;

                        {
                            let mut d = dot.borrow_mut();
                            d.fade_in = (t as f64 / SETTLE_TICKS as f64).clamp(0.0, 1.0);
                            d.points = active_group
                                .iter()
                                .copied()
                                .filter(|&i| !calibrated[i])
                                .map(|i| GroupPoint {
                                    point: cal_points[i],
                                    progress: if Some(i) == new_focused {
                                        if requested {
                                            1.0
                                        } else {
                                            (new_in_zone_ticks as f64 / SETTLE_TICKS as f64)
                                                .clamp(0.0, 1.0)
                                        }
                                    } else {
                                        0.0
                                    },
                                })
                                .collect();
                        }

                        if lost_focus_while_requested {
                            // The original's FocusChangedDuringCalibration ->
                            // discard+retry: the device may still be
                            // mid-collect for this point; ask it to discard
                            // whatever it gathered and restart the settle
                            // wait from zero, WITHOUT marking it calibrated —
                            // this is the actual fix for the reported bug (a
                            // sample taken while the user looked away is no
                            // longer silently accepted).
                            //
                            // Re-check with a FRESH read, not the tick-start
                            // `cal` snapshot: `CalCollect`/`CalDiscard` are
                            // FIFO on the same device-thread queue, so if the
                            // device's ack for this point actually landed in
                            // the gap between this tick's snapshot and now,
                            // discarding it anyway would silently throw away
                            // an already-accepted sample. Skipping the
                            // discard here when the fresh read shows it
                            // already collected closes that race; the normal
                            // `cal.collected > total_captured` branch above
                            // will pick up the advance on a later tick.
                            let (px, py) =
                                cal_points[focused.expect("requested implies focused is set")];
                            let already_collected =
                                state.lock().unwrap().calibration.collected > total_captured;
                            if !already_collected {
                                let _ = tick_cmd.send(DeviceCommand::CalDiscard { x: px, y: py });
                            }
                            next = Some(Phase::Collecting {
                                token,
                                mode,
                                group,
                                calibrated,
                                focused,
                                pending_ticks: new_pending_ticks,
                                in_zone_ticks: 0,
                                gap_ticks: 0,
                                requested: false,
                                ticks: t,
                            });
                        } else if !requested
                            && new_focused.is_some()
                            && new_in_zone_ticks >= SETTLE_TICKS
                        {
                            let (px, py) = cal_points[new_focused.expect("checked is_some above")];
                            let _ = tick_cmd.send(DeviceCommand::CalCollect { x: px, y: py });
                            next = Some(Phase::Collecting {
                                token,
                                mode,
                                group,
                                calibrated,
                                focused: new_focused,
                                pending_ticks: new_pending_ticks,
                                in_zone_ticks: new_in_zone_ticks,
                                gap_ticks: new_gap_ticks,
                                requested: true,
                                ticks: t,
                            });
                        } else if t >= COLLECT_TIMEOUT_TICKS * active_group.len() as u32 {
                            // Applies REGARDLESS of `requested`/`focused` (a
                            // deliberate behavior carried over unmodified
                            // from before this task): a user who never looks
                            // at any dot in the group at all must still
                            // eventually time out. Scaled by the group's size
                            // since a group can have up to 3 points to get
                            // through — a defensive-only ceiling, same spirit
                            // as before, just scaled to the group's size.
                            let _ = tick_cmd.send(DeviceCommand::CalAbort);
                            next = Some(done_phase(Err("Timed out reading a point.".into())));
                        } else {
                            next = Some(Phase::Collecting {
                                token,
                                mode,
                                group,
                                calibrated,
                                focused: new_focused,
                                pending_ticks: new_pending_ticks,
                                in_zone_ticks: new_in_zone_ticks,
                                gap_ticks: new_gap_ticks,
                                requested,
                                ticks: t,
                            });
                        }
                    }
                }
            }
            Phase::Computing { token, ticks } => {
                let token = *token;
                dot.borrow_mut().points.clear();
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

    /// Regression test for the reported bug ("I cannot focus in point 5"):
    /// point 5 (1-based) = index 4 (0-based), the top-left corner, belongs to
    /// the original's third simultaneous-group `{4,5,6}`. Computing the zone
    /// radius from ALL 7 `FULL_7` points at once (the old, wrong behavior)
    /// gives 0.09 (see `focus.rs`'s own `zone_radius_matches_hand_derivation`)
    /// — exactly half of the 0.18 the original actually uses for this group,
    /// hand-derived below. `group_zone_radii` must produce 0.18 for index 4,
    /// not 0.09.
    #[test]
    fn group_zone_radii_center_point_is_always_in_zone() {
        let radii = group_zone_radii(&FULL_7, 1.0);
        assert_eq!(radii[0], f64::MAX, "center point (index 0) must be `f64::MAX` (whole-screen, always-in-zone), matching the original's single-point-calibration radius");
    }

    /// Hand-derivation for group B (indices 1-3): `(0.1,0.9)`, `(0.5,0.1)`,
    /// `(0.9,0.9)`.
    ///   (0.1,0.9)-(0.5,0.1): dx=0.4, dy=0.8 -> dist = sqrt(0.16+0.64) = sqrt(0.8) ≈ 0.894
    ///   (0.1,0.9)-(0.9,0.9): dx=0.8, dy=0.0 -> dist = 0.8
    ///   (0.5,0.1)-(0.9,0.9): dx=0.4, dy=0.8 -> dist = sqrt(0.8) ≈ 0.894
    /// Minimum is 0.8 (NOT 0.4, the whole-7-point minimum) -> radius = (0.8 / 2.0) * 0.45 = 0.18.
    #[test]
    fn group_zone_radii_group_b_matches_hand_derivation() {
        let radii = group_zone_radii(&FULL_7, 1.0);
        assert_eq!(
            radii[1], radii[2],
            "group B (indices 1-3) shares one radius"
        );
        assert_eq!(
            radii[2], radii[3],
            "group B (indices 1-3) shares one radius"
        );
        assert!(
            (radii[1] - 0.18).abs() < 1e-9,
            "expected 0.18, got {}",
            radii[1]
        );
    }

    /// Hand-derivation for group C (indices 4-6, containing the reported
    /// point 5/index 4): `(0.1,0.1)`, `(0.5,0.9)`, `(0.9,0.1)`.
    ///   (0.1,0.1)-(0.5,0.9): dx=0.4, dy=0.8 -> dist = sqrt(0.8) ≈ 0.894
    ///   (0.1,0.1)-(0.9,0.1): dx=0.8, dy=0.0 -> dist = 0.8
    ///   (0.5,0.9)-(0.9,0.1): dx=0.4, dy=0.8 -> dist = sqrt(0.8) ≈ 0.894
    /// Minimum is 0.8 -> radius = (0.8 / 2.0) * 0.45 = 0.18 — NOT 0.09, the
    /// old, wrong, whole-7-point-derived value (see
    /// `group_zone_radii_old_whole_array_value_is_half_the_correct_one`
    /// below for a direct side-by-side).
    #[test]
    fn group_zone_radii_group_c_matches_hand_derivation() {
        let radii = group_zone_radii(&FULL_7, 1.0);
        assert_eq!(
            radii[4], radii[5],
            "group C (indices 4-6) shares one radius"
        );
        assert_eq!(
            radii[5], radii[6],
            "group C (indices 4-6) shares one radius"
        );
        assert!(
            (radii[4] - 0.18).abs() < 1e-9,
            "expected 0.18, got {}",
            radii[4]
        );
        assert_ne!(
            radii[4], 0.09,
            "must NOT be the old whole-array-derived value (see the sanity check below)"
        );
    }

    /// Direct side-by-side with the OLD (wrong) computation: deriving the
    /// radius from all 7 points at once instead of per-group gives 0.09 —
    /// exactly half of the 0.18 that `group_zone_radii` correctly gives for
    /// group C above. This is the entire reason the reported bug happened:
    /// the corner points' hit-zone was half the size it should have been.
    #[test]
    fn group_zone_radii_old_whole_array_value_is_half_the_correct_one() {
        let old_whole_array_radius = focus::zone_radius(&FULL_7, 1.0);
        assert!(
            (old_whole_array_radius - 0.09).abs() < 1e-9,
            "expected 0.09, got {old_whole_array_radius}"
        );
        let group_c_radius = group_zone_radii(&FULL_7, 1.0)[4];
        assert!(
            (group_c_radius - 2.0 * old_whole_array_radius).abs() < 1e-9,
            "group C's correct radius ({group_c_radius}) should be exactly double the old, \
             wrong, whole-array radius ({old_whole_array_radius})"
        );
    }

    /// `GROUPS` must partition all 7 point indices exactly once each — no
    /// gaps, no overlaps — and the center point (index 0) must be its own,
    /// lone first group, matching the decompiled original's
    /// `FirstCalibrationSinglePoint` -> `SecondCalibrationThreePoints` ->
    /// `ThirdCalibrationThreePoints` sequence.
    #[test]
    fn groups_cover_all_seven_indices_exactly_once() {
        let mut indices: Vec<usize> = GROUPS.iter().flat_map(|g| g.iter().copied()).collect();
        indices.sort_unstable();
        assert_eq!(indices, vec![0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(
            GROUPS[0],
            [0],
            "the center point must be alone in the first group"
        );
    }
}
