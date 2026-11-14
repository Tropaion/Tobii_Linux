//! One gaze sample in, one [`TrackingFrame`] out — the sequence every consumer
//! of this crate has to perform, written once.
//!
//! # Why this is not left to the caller
//!
//! Composing a frame is four decisions that have to agree with each other:
//! whether Extended View contributes, what to do while the eyes are shut, when
//! to smooth, and when to forget everything because tracking has been gone long
//! enough that the old state is a lie. Get the order wrong and the failure is
//! not a crash — it is a game camera that drifts, or lags, or snaps when you
//! blink, none of which look like a bug in the thing that caused them.
//!
//! Four of those orderings are load-bearing and none of them is obvious:
//!
//! * **Compose, then filter once.** Smoothing the head pose and the gaze
//!   contribution separately leaves the two out of phase, so a fast look
//!   arrives before the head movement that accompanied it.
//! * **Hold through a blink.** Gaze vanishes for 100–400 ms every few seconds.
//!   Treating that as "look straight ahead" makes the camera lurch on every
//!   blink, so the last Extended View offset is held and decayed instead.
//! * **Recentre after Extended View, never before.** Translation goes out as
//!   displacement from where you normally sit, not as your position in front of
//!   the sensor — see [`FramePipeline::neutral`]. But Extended View is a geometric
//!   construction against display corners measured in the tracker's frame, so
//!   it needs the real position; subtract the neutral first and the head sits
//!   at the origin of its own coordinate system and the angle collapses to
//!   zero.
//! * **Recentre rotation BEFORE `compose`, never after.** The exact opposite of
//!   the line above, for the exact opposite reason: the rotation reference is a
//!   measurement of where the *head* points, and after `compose` the angle also
//!   carries the gaze-driven Extended View term. Taking the reference from the
//!   composed angle while the user happened to be looking at a screen edge
//!   would write that glance into the reference — permanently, since nothing
//!   decays it. See [`FramePipeline::rot_ref`].
//!
//! This existed once as `compose_frame` inside the old `tobii serve` daemon,
//! where nothing else could reach it, and `tobii headpose` grew its own
//! partial copy that had neither of the above. Two copies of a sequence whose
//! errors are invisible is the worst possible thing to have two copies of.

use std::time::{Duration, Instant};

use tobii_headpose::{median_and_spread, HeadPose, PairOffset, PoseFilter, PoseSource};
use tobii_protocol::gaze::present;
use tobii_protocol::{DisplayCorners, GazeSample};

use crate::games::OutputConfig;
use crate::{fusion, Presence, TrackingFrame};

/// How long tracking must be gone before the smoothing state is discarded.
///
/// Short losses are the normal case — a blink, a head turn past the sensor's
/// edge — and resetting on those would undo the smoothing every few seconds.
/// A full second means the person has actually left, and resuming from a
/// second-old pose would swing the camera across the room.
const TRACKING_LOSS_RESET: Duration = Duration::from_millis(1000);

/// How long a rotation recentre averages the head before it adopts a reference.
///
/// A reference caught on one frame is a reference caught mid-turn, and unlike
/// the neutral nothing later corrects it: it stands until the user asks again.
/// So a second is spent measuring instead — about **33 frames** at the 30.208 ms
/// cadence measured between consecutive gaze frames in
/// `crates/tobii-usb/tests/captures/session.tobiicap`. One second is also what
/// `docs/wiki/Planned-Work.md` item 3 asks for, and it is short enough that the
/// user is still holding the pose they pressed the button in.
const RECENTRE_WINDOW: Duration = Duration::from_millis(1000);

/// The fewest poses that window must contain for its average to be a
/// measurement rather than a coincidence.
///
/// Ten of the ~33 frames a second holds. A chosen floor, bounded by two
/// measured numbers: the 2026-08-09 session lost its left eye in 34% of 400
/// frames and its right in 18%, so demanding most of the window would refuse an
/// ordinary session outright — while a window that yielded fewer than ten poses
/// means the tracker was mostly not seeing the user, which is not a posture to
/// take a reference from.
const RECENTRE_MIN_POSES: usize = 10;

/// How far the head may wander across the window and still be called still, in
/// degrees of 10–90% spread.
///
/// [`tobii_headpose::MOVED_SPREAD_DEG`], which is not a new number: it is the
/// spread at which this project's pitch-zero measurement already tells the user
/// "your head moved during the measurement" (`tobii headpose
/// --calibrate-pitch`, and the hub's copy of it). Same shape of measurement —
/// sit still, take the median — so the same figure, named once rather than
/// asserted twice.
///
/// Refused here rather than merely flagged, because the two runs differ in what
/// the user can see afterwards: the pitch run prints the number it measured and
/// the spread beside it, while a rotation reference is invisible once applied —
/// it simply becomes what "straight ahead" means. And the bias this exists to
/// remove is larger than the bar: the head measured off the tracker's axis in
/// that session sat at **17.4°**, twice this spread, so an off-axis user is
/// refused for moving, never for being off-axis.
const RECENTRE_MAX_SPREAD_DEG: f64 = tobii_headpose::MOVED_SPREAD_DEG;

/// How far the head may sit from the neutral position, in millimetres, and
/// still have auto-centre pull the neutral toward it.
///
/// The translation counterpart of `AutoCentre::window_deg`. A lean further than
/// this is a lean the user means — peering at an instrument, ducking under a
/// canopy bow — and adapting toward it would slowly erase the movement they are
/// making. 80 mm is past the few centimetres a seated posture wanders over a
/// session and well short of a deliberate lean.
const AUTO_CENTRE_WINDOW_MM: f64 = 80.0;

/// The part of the screen, as a fraction of its width and height either side
/// of the middle, that auto-centre counts as "looking at the centre".
///
/// ±20% is the middle 40% of the screen each way: where the eyes rest when the
/// user is looking straight ahead at the game, not at a minimap or a gauge.
const AUTO_CENTRE_GAZE_BOX: f64 = 0.2;

/// The longest gap between two frames that auto-centre will integrate over,
/// in seconds. A stall is not a second of the user sitting still.
const AUTO_CENTRE_MAX_DT_S: f64 = 0.2;

/// How far from the middle of the screen, as a fraction of its width and
/// height either side, a recentre's gaze may sit and still be adopted as the
/// Extended View centre.
///
/// ±30% is the middle 60% each way. Inside it, the user was looking at the
/// middle and the tracker placed it a little off — gaze calibration error on a
/// wide panel is easily a fifth of its width — and that error is exactly what
/// should be zeroed. Outside it, they pressed the key while glancing at a side
/// window or a gauge, and adopting that glance as "straight ahead" would bake
/// it in until the next recentre; the gaze reference is then left as it was.
const RECENTRE_GAZE_BOX: f64 = 0.3;

/// What a rotation recentre did, for whoever asked for it to report.
///
/// Both refusals name what went wrong rather than the user simply seeing
/// nothing happen: a recentre that quietly did not take is indistinguishable
/// from one that took and was wrong.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecentreOutcome {
    /// The reference moved. The angles are what now reads as straight ahead.
    Applied {
        yaw_deg: f64,
        pitch_deg: f64,
        roll_deg: f64,
        spread_deg: f64,
    },
    /// The head kept moving through the window, so the old reference stands.
    Moved { spread_deg: f64 },
    /// Too few of the window's frames measured a head angle to average.
    NoHead { poses: usize },
    /// The user left while the window was still measuring, so it never ran its
    /// second.
    ///
    /// Separate from [`RecentreOutcome::NoHead`] because the two are different
    /// answers to the user. `NoHead` is a full window the tracker spent mostly
    /// looking at nobody, and says so with a count below
    /// [`RECENTRE_MIN_POSES`]; this one can carry any count at all — press
    /// recentre, then lose both eyes for a second, and the run holds twenty
    /// perfectly good poses it is not allowed to average, because the rest of
    /// the window would have been measured after the user came back. Reporting
    /// that as `NoHead` prints "only 20 frames measured your head angle"
    /// against a floor of ten, which sends the user looking at their tracker
    /// instead of at the measurement they walked out of.
    Interrupted { poses: usize },
}

impl std::fmt::Display for RecentreOutcome {
    /// One sentence, shared by every front end so the CLI and the hub cannot
    /// end up explaining the same refusal differently.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecentreOutcome::Applied {
                yaw_deg,
                pitch_deg,
                roll_deg,
                ..
            } => write!(
                f,
                "recentred — {yaw_deg:+.1}° yaw, {pitch_deg:+.1}° pitch, {roll_deg:+.1}° roll \
                 and your current position now read as zero"
            ),
            RecentreOutcome::Moved { spread_deg } => write!(
                f,
                "not recentred: your head moved {spread_deg:.1}° while it was measuring — \
                 hold still and ask again"
            ),
            RecentreOutcome::NoHead { poses } => write!(
                f,
                "not recentred: only {poses} frames of that second measured your head angle \
                 — sit square to the tracker and ask again"
            ),
            RecentreOutcome::Interrupted { poses } => write!(
                f,
                "not recentred: the tracker lost you while it was measuring, {poses} frames in \
                 — sit back down and ask again"
            ),
        }
    }
}

/// A pose composed outside this crate, and when the rotation in it was
/// measured.
///
/// The instant is not decoration, and it is not a freshness bound — the caller
/// has already applied one by the time it calls. Both front ends hold the
/// neural model's last rotation across the gaze frames that arrive between
/// camera frames: the two streams both run at ~33 Hz and are not in lockstep,
/// so dropping pitch to zero on every gaze sample that came without an image
/// would shake the head in game. They keep holding it for up to a second after
/// the model stops being confident, which is a whole settle window.
///
/// A held rotation is identical by value to the one it was measured from, so
/// the pipeline cannot tell the two apart from the numbers; only the caller
/// knows, and this is how it says so. The stamp moves when, and only when,
/// something measured the head again.
///
/// Only the settle window consults it, and only to refuse counting one
/// measurement twice. Everything else in the pipeline treats the pose exactly
/// as it treated a bare [`HeadPose`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SuppliedPose {
    /// The pose itself, which wins over the geometric fallback — see
    /// [`FramePipeline::offer`].
    pub pose: HeadPose,
    /// When the rotation in `pose` was measured. The same instant on two
    /// frames is the same measurement on both.
    pub rotation_at: Instant,
    /// Whether `pose.pitch_deg` is the model's measurement. False for the eye
    /// geometry, whose pitch is a fixed 0 — which a front end supplies too,
    /// with no model or with a stale one, so being supplied says nothing about
    /// pitch on its own.
    pub measured_pitch: bool,
}

/// One rotation recentre in flight: the poses seen since it started.
struct RecentreRun {
    started: Instant,
    yaw_deg: Vec<f64>,
    pitch_deg: Vec<f64>,
    roll_deg: Vec<f64>,
    /// Head position relative to the current neutral, x/y/z in mm. Collected
    /// on every frame with a finite position, rotation held or not: a held
    /// rotation still comes with freshly measured eye origins.
    pos_mm: [Vec<f64>; 3],
    /// Raw gaze angles (yaw, pitch) relative to the screen centre, before the
    /// Extended View curve — where the user was looking while they held still.
    gaze_deg: [Vec<f64>; 2],
    /// The normalised gaze points behind `gaze_deg`, for the
    /// [`RECENTRE_GAZE_BOX`] check.
    gaze_xy: [Vec<f64>; 2],
    /// Whether the window's rotation is measured with the model's pitch
    /// (`true`) or the eye geometry's fixed 0. It starts on the geometry and
    /// starts over on the model when a pose of it arrives (see
    /// [`FramePipeline::offer`]); rotations of the other kind are left out. A
    /// window that spanned a model dropping out read the jump between the two
    /// as a head that moved some 20°, and refused.
    measured_pitch: bool,
}

impl RecentreRun {
    fn new(started: Instant, measured_pitch: bool) -> RecentreRun {
        RecentreRun {
            started,
            yaw_deg: Vec::new(),
            pitch_deg: Vec::new(),
            roll_deg: Vec::new(),
            pos_mm: [Vec::new(), Vec::new(), Vec::new()],
            gaze_deg: [Vec::new(), Vec::new()],
            gaze_xy: [Vec::new(), Vec::new()],
            measured_pitch,
        }
    }
}

/// The state carried between frames.
///
/// All of it is smoothing and loss-tracking; none of it is configuration, which
/// is passed in on every call so a settings change takes effect on the next
/// frame rather than at the next restart.
pub struct FramePipeline {
    filter: PoseFilter,
    last_ev: Option<(f64, f64)>,
    gaze_lost_since: Option<Instant>,
    last_tracked: Option<Instant>,
    /// The neutral head position, and why translation is not sent raw.
    ///
    /// [`HeadPose`]'s translation is the head's position **in the tracker's own
    /// frame**: `z` is how far in front of the sensor you are sitting, so an
    /// ordinary user sits at `z ≈ 650–700 mm` and never goes near zero. Every
    /// consumer of this crate wants the opposite quantity — displacement from where
    /// you normally sit, which is what "lean forward" means to a game.
    ///
    /// Sending it raw was not a subtle error. Both encoders saturate:
    ///
    /// | z | joystick axis (`±500 mm` full scale) | TrackIR (`AXIS_LIMIT`) |
    /// |---|---|---|
    /// | 600 mm | 65534 of 65534 | 16383 of 16383 |
    /// | 680 mm | 65534 of 65534 | 16383 of 16383 |
    /// | 700 mm | 65534 of 65534 | 16383 of 16383 |
    ///
    /// So `ABS_Z` and TrackIR's `fNPZ` were pinned hard at their maximum for every
    /// realistic head distance, for every user — a dead axis that looks in a game
    /// like a permanently-leaned-in camera, or like a throttle stuck at full if
    /// something auto-binds it. `x` and `y` were merely offset by wherever the
    /// tracker is mounted, which is smaller and just as wrong.
    ///
    /// The neutral is taken from the first pose after tracking starts, and again
    /// after a loss long enough to have reset the smoothing state — the moment the
    /// tracker picks you up is, by construction, a moment you are sitting normally.
    /// That reuses [`TRACKING_LOSS_RESET`] rather than inventing a second notion of
    /// "you have gone away", and it means somebody who gets up and comes back sat
    /// slightly differently does not acquire a permanent offset.
    neutral: Option<[f64; 3]>,
    /// The one-eye fallback's state: the last measured offset between the two
    /// eyes, plus its age and the eye-count debounce.
    ///
    /// It lives here for the same reason [`FramePipeline::neutral`] does — it
    /// is per-session tracking state, not configuration, and it has to survive
    /// a settings change. Rebuilding the pipeline to pick up a new smoothing
    /// strength would otherwise throw away the offset in the middle of an
    /// outage, which is exactly when it is the only thing producing a pose.
    pair: PairOffset,
    /// What the one-eye fallback has done this session.
    ///
    /// Counted, not logged: this crate has no logger (it cross-compiles for
    /// the Wine bridge), and a fallback that is invisible is the failure mode
    /// the reconstruction risks — a guess that reads exactly like a
    /// measurement. [`FramePipeline::fallback_stats`] is how a front end says
    /// so out loud.
    ///
    /// The public struct is the state rather than a copy of it: the three
    /// numbers exist only to be handed out through that one accessor, and a
    /// private mirror of them is a second place for the two to disagree.
    stats: FallbackStats,
    /// The head rotation that reads as straight ahead: yaw and roll, in
    /// degrees, subtracted from the head term before Extended View joins it.
    ///
    /// # Why rotation needs a reference at all
    ///
    /// This used to say, in the comment inside [`FramePipeline::offer`], that
    /// rotation is never recentred because it is already referenced to facing
    /// the screen. That holds for the Extended View term, which is built
    /// against the screen's own corners — and not for the head term, which is
    /// an `atan2` on the interocular vector (`tobii_headpose::pose_from_eyes`)
    /// and so is absolute in the *tracker's* frame. It is referenced to the
    /// screen only by assuming the tracker is mounted square and the user sits
    /// on its axis. This project measured a session with a head sitting
    /// **17.4° off-axis**; that user's game camera is permanently turned by
    /// most of it, and nothing in the pipeline removed it.
    ///
    /// # Three things it deliberately is not
    ///
    /// * **Not automatic, unless the user turns auto-centre on.** The neutral
    ///   is taken from the first pose because the moment the tracker picks you
    ///   up is, by construction, a moment you are sitting normally — that is
    ///   not true of *where you are looking*, which is what rotation measures.
    ///   So by default this moves only when asked
    ///   ([`FramePipeline::begin_recentre`]) and, unlike the neutral, survives
    ///   a tracking loss: a reference the user set is not something an absence
    ///   should quietly undo. Auto-centre (`auto_centre`, off by default) is
    ///   the opt-in exception: see [`FramePipeline::auto_centre`].
    /// * **Not pitch.** A recentre takes pitch too, but into
    ///   [`pitch_ref`](Self::pitch_ref), which auto-centre cannot reach.
    /// * **Not taken from the composed pose.** See the module docs.
    rot_ref: Option<[f64; 2]>,
    /// The pitch that reads as level, set by a recentre alongside
    /// [`rot_ref`](Self::rot_ref).
    ///
    /// Separate from `rot_ref` so that auto-centre, which deliberately leaves
    /// pitch alone, cannot move it. It sits on top of the measured zero from
    /// `tobii headpose --calibrate-pitch` rather than replacing it: that one
    /// corrects how the tracker is mounted, this one is where the user wants
    /// "level" to be right now — and a recentre that left pitch off-centre
    /// was, in use, a recentre that did not work.
    ///
    /// Because it is measured against that zero, it is dropped whenever the
    /// zero changes: see [`FramePipeline::forget_pitch_reference`]. The
    /// geometric path measures no pitch, so a recentre there takes 0, and the
    /// reference is subtracted from the model's pitch only: the geometric
    /// path's 0 is level already, and a frame that fell back to it while the
    /// model was stale went out as −`pitch_ref`.
    pitch_ref: Option<f64>,
    /// The gaze direction that reads as "looking straight ahead" for Extended
    /// View: raw yaw and pitch, in degrees from the screen centre, set by a
    /// recentre.
    ///
    /// Without it a recentre zeroed the head and left the gaze term alone, so
    /// the joystick's yaw stayed off-centre whenever the tracker's idea of
    /// "the middle of the screen" differed from where the user was actually
    /// looking — a few degrees of gaze calibration error on a wide panel is
    /// tens of degrees of Extended View. Subtracted from the raw angle, before
    /// the deadzone and curve, so the response keeps its full shape around the
    /// new centre rather than being shifted up or down it.
    gaze_ref: Option<(f64, f64)>,
    /// The stamp on the last [`SuppliedPose`] this pipeline was handed.
    ///
    /// Compared, not aged: the caller has already decided how stale a held
    /// rotation may be before it stops offering one at all. All this answers
    /// is whether the rotation on this frame is one the settle window has
    /// already counted.
    last_rotation_at: Option<Instant>,
    /// A recentre being measured right now, if one is.
    recentre: Option<RecentreRun>,
    /// The last finished recentre, until somebody takes it.
    last_recentre: Option<RecentreOutcome>,
    /// When the last pose went into the filter: the adaptive filter turns a
    /// change into a speed with it, and auto-centre integrates over it.
    last_frame_at: Option<Instant>,
    /// The last pose that went out, for the output slew limit
    /// ([`OutputConfig::filter_max_rot_speed_deg_s`]).
    last_out: Option<HeadPose>,
    /// The last smoothed pose before the curves shaped it, for a curve editor
    /// to mark where the head is on each curve.
    last_curve_input: Option<HeadPose>,
}

/// What the one-eye fallback has done this session.
///
/// The two counters are a statement about the **eye tracker**, not about the
/// pose that went out: they count the frames it delivered with two eyes and
/// the frames it delivered with one, whoever ended up supplying the pose for
/// them. That is what makes `reconstructed / (both_eyes + reconstructed)` the
/// measurement of how much the fallback is carrying. A ratio that climbs
/// towards parity is not a bug in this code — it is a tracker that cannot see
/// one of the user's eyes, and the fix for that is physical (aim the tracker,
/// raise the seat).
///
/// `active` is the one field that *is* about the outgoing pose: a model frame
/// is not a reconstruction, however the geometry underneath it turned out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FallbackStats {
    /// Frames on which the tracker measured both eye origins.
    pub both_eyes: u64,
    /// Frames on which it measured one, and the missing eye was placed with
    /// the last measured offset.
    pub reconstructed: u64,
    /// True if the pose most recently sent out was a reconstructed one.
    pub active: bool,
}

impl FramePipeline {
    /// A pipeline smoothing at the configured strength.
    pub fn new(cfg: &OutputConfig) -> Self {
        FramePipeline {
            filter: PoseFilter::with_smoothing(cfg.smoothing(), cfg.filter_max_step_mm),
            last_ev: None,
            gaze_lost_since: None,
            last_tracked: None,
            neutral: None,
            pair: PairOffset::new(),
            stats: FallbackStats::default(),
            rot_ref: None,
            pitch_ref: None,
            gaze_ref: None,
            last_rotation_at: None,
            recentre: None,
            last_recentre: None,
            last_frame_at: None,
            last_out: None,
            last_curve_input: None,
        }
    }

    /// Start measuring a new rotation reference.
    ///
    /// It is not adopted here: the next [`RECENTRE_WINDOW`] of poses are
    /// averaged first, and the result arrives through
    /// [`FramePipeline::take_recentre`] — which can be a refusal. Asking again
    /// while one is in flight restarts the window rather than queueing, because
    /// two presses of the same button mean "start from now", not "do it twice".
    pub fn begin_recentre(&mut self, now: Instant) {
        self.recentre = Some(RecentreRun::new(now, false));
    }

    /// Whether a settle window is running, so a front end can say "hold still".
    pub fn recentring(&self) -> bool {
        self.recentre.is_some()
    }

    /// The outcome of the last recentre, once, for whoever reports it.
    pub fn take_recentre(&mut self) -> Option<RecentreOutcome> {
        self.last_recentre.take()
    }

    /// The rotation that currently reads as straight ahead — yaw and roll in
    /// degrees — or `None` while the head term is being sent as measured.
    pub fn rotation_reference(&self) -> Option<[f64; 2]> {
        self.rot_ref
    }

    /// Drop the pitch a recentre took as level, because the pitch it was
    /// measured against has changed under it.
    ///
    /// For a front end to call whenever the head-pose source is rebuilt: a new
    /// pitch zero from `--calibrate-pitch`, a model installed or removed. The
    /// reference is a number in the old source's frame, so carrying it across
    /// subtracts the calibration a second time: a recentre at an uncalibrated
    /// +22°, then a calibration that saves that 22° as the zero, sent a level
    /// head out as −22° until the next recentre. Yaw, roll, position and gaze
    /// stay as they were; nothing about them moved.
    ///
    /// A recentre still measuring is measuring against the old zero too, and
    /// would install exactly that stale number when its window closes. So it
    /// starts over from `now`: the user is holding still for it either way, and
    /// a fresh window measures what they asked for, which an "interrupted"
    /// refusal blaming the tracker would not.
    pub fn forget_pitch_reference(&mut self, now: Instant) {
        self.pitch_ref = None;
        if self.recentre.is_some() {
            self.begin_recentre(now);
        }
    }

    /// Feed one head term to a running settle window, then apply the reference.
    ///
    /// The order inside matters: a window that completes on this frame is
    /// applied to this frame, so the recentre the user asked for takes effect
    /// on the first frame it possibly can rather than one frame later.
    ///
    /// What is fed in is the **head term before `fusion::compose`** — see the
    /// module docs for why taking it afterwards would bake a glance at the
    /// screen edge into the reference for good. It is also the head term
    /// before `rot_ref` is subtracted, which is what makes a second recentre
    /// *replace* the first rather than measure the residual of it.
    ///
    /// `measured_pitch` says whether `head`'s pitch is the model's.
    fn rereference(
        &mut self,
        head: HeadPose,
        rotation_held: bool,
        measured_pitch: bool,
        now: Instant,
    ) -> HeadPose {
        if let Some(run) = self.recentre.as_mut() {
            // The window measures rotation, so it takes only frames that
            // measured one — and there are two ways to be handed the same
            // measurement twice.
            //
            // A reconstructed frame's yaw and roll ARE the last two-eye
            // frame's: `PairOffset` holds the interocular vector, and
            // `pose_from_eyes` reads the rotation off precisely that vector,
            // so only translation is new. A supplied pose repeats itself for a
            // different reason: the model runs on the camera stream, and the
            // caller holds its last rotation across every gaze frame that
            // arrived without a confident estimate behind it. See
            // [`SuppliedPose`], which is how that frame says which it was.
            //
            // Either way, pushing the copies counts one measurement many
            // times, which pads `RECENTRE_MIN_POSES` and pulls the 10–90%
            // spread toward zero while the head was free to move through the
            // gap: both halves of the gate loosened by the same duplicates.
            // The supplied path is the worse of the two, because a held
            // rotation stands for as long as the caller's staleness bound (a
            // second, in both front ends) and so can fill a window outright,
            // where an outage fits at most nine frames inside
            // `RECONSTRUCTION_MAX_AGE`.
            //
            // Non-finite angles are dropped rather than sorted: a degenerate
            // model quaternion normalises to NaN, `partial_cmp` answers `None`
            // for it, and a median taken over one is not a median. Dropping
            // either kind also costs the run those frames, so a window full of
            // them is refused for having too few poses — which is the truth.
            //
            // And only rotations of the kind of pitch the window measures —
            // position is the eye origins' in either.
            if !rotation_held
                && run.measured_pitch == measured_pitch
                && head.yaw_deg.is_finite()
                && head.pitch_deg.is_finite()
                && head.roll_deg.is_finite()
            {
                run.yaw_deg.push(head.yaw_deg);
                run.pitch_deg.push(head.pitch_deg);
                run.roll_deg.push(head.roll_deg);
            }
            for (v, x) in run.pos_mm.iter_mut().zip([head.x_mm, head.y_mm, head.z_mm]) {
                if x.is_finite() {
                    v.push(x);
                }
            }
        }
        // The window-close check sits outside both gates on purpose: a run
        // whose second runs out during an outage, or after the model dropped
        // out, must still close, or it would hang until the next frame it
        // would have taken.
        if let Some(run) = self
            .recentre
            .take_if(|r| now.saturating_duration_since(r.started) >= RECENTRE_WINDOW)
        {
            self.last_recentre = Some(self.settle(run));
        }
        let head = match self.rot_ref {
            Some([yaw, roll]) => HeadPose {
                yaw_deg: head.yaw_deg - yaw,
                roll_deg: head.roll_deg - roll,
                ..head
            },
            None => head,
        };
        match self.pitch_ref.filter(|_| measured_pitch) {
            Some(pitch) => HeadPose {
                pitch_deg: head.pitch_deg - pitch,
                ..head
            },
            None => head,
        }
    }

    /// Decide what a finished settle window measured.
    ///
    /// Reduced by [`median_and_spread`], the same reduction this project
    /// already takes on its other measured zero — for the same reason: the
    /// first frames of a run are the ones before the user has settled, and a
    /// handful of them must not be able to move the answer. The floor below is
    /// what makes the indexing inside it safe.
    fn settle(&mut self, mut run: RecentreRun) -> RecentreOutcome {
        if run.yaw_deg.len() < RECENTRE_MIN_POSES {
            return RecentreOutcome::NoHead {
                poses: run.yaw_deg.len(),
            };
        }
        let (yaw_deg, yaw_spread) = median_and_spread(&mut run.yaw_deg);
        let (roll_deg, roll_spread) = median_and_spread(&mut run.roll_deg);
        let (pitch_deg, pitch_spread) = median_and_spread(&mut run.pitch_deg);
        // The worst of the three axes, not their average: a head that held its
        // yaw while swinging in roll was still moving. Pitch is in it because
        // pitch is adopted below — left out, a nod during the window became
        // the new level, and a level head then read as tilted by however far
        // the nod went. The geometric path measures no pitch, so its samples
        // are all zero and add no spread.
        let spread_deg = yaw_spread.max(roll_spread).max(pitch_spread);
        if spread_deg > RECENTRE_MAX_SPREAD_DEG {
            // The old reference stands. A reference captured mid-turn is worse
            // than none, and worse still than the one already in use.
            return RecentreOutcome::Moved { spread_deg };
        }
        self.rot_ref = Some([yaw_deg, roll_deg]);
        // Only a window of the model's pitch says where level is; one of the
        // eye geometry's fixed 0 leaves the model's levelling as it was.
        if run.measured_pitch {
            self.pitch_ref = Some(pitch_deg);
        }
        // Position: re-take the neutral at the median of the window, so
        // translation reads zero too. The samples are relative to the neutral
        // in use, so the median is the correction to it.
        // Gaze: only when the window saw enough of it. Without gaze (no display
        // geometry, eyes not tracked) the previous gaze reference stands.
        let near_middle = run.gaze_xy.iter_mut().all(|v| {
            v.len() >= RECENTRE_MIN_POSES
                && (median_and_spread(v).0 - 0.5).abs() <= RECENTRE_GAZE_BOX
        });
        if near_middle && run.gaze_deg.iter().all(|v| v.len() >= RECENTRE_MIN_POSES) {
            let gy = median_and_spread(&mut run.gaze_deg[0]).0;
            let gp = median_and_spread(&mut run.gaze_deg[1]).0;
            self.gaze_ref = Some((gy, gp));
        }
        if let Some(n) = self.neutral.as_mut() {
            for (axis, v) in n.iter_mut().zip(run.pos_mm.iter_mut()) {
                if v.len() >= RECENTRE_MIN_POSES {
                    *axis += median_and_spread(v).0;
                }
            }
        }
        RecentreOutcome::Applied {
            yaw_deg,
            pitch_deg,
            roll_deg,
            spread_deg,
        }
    }

    /// Automatic recentring: nudge the neutral and the rotation reference a
    /// little toward the pose the user is holding right now.
    ///
    /// # When it adapts
    ///
    /// Only when all of these hold, because each one is a sign the user is
    /// sitting the way they mean to sit rather than looking somewhere on
    /// purpose:
    ///
    /// * the user is **looking near the middle of the screen**
    ///   ([`AUTO_CENTRE_GAZE_BOX`]) — the moment of play where "straight ahead"
    ///   is unambiguous, and the reason a deliberate glance at a side window
    ///   does not get absorbed;
    /// * the head's yaw and roll, measured against the current reference, are
    ///   **inside `window_deg`** — a big turn is a turn;
    /// * the head's position is inside [`AUTO_CENTRE_WINDOW_MM`] of the
    ///   neutral — a big lean is a lean;
    /// * **no manual recentre is measuring**, whose result would otherwise be
    ///   mixed with this one.
    ///
    /// # How much
    ///
    /// A first-order pull with time constant `time_s`: each frame closes
    /// `1 - e^(-dt/time_s)` of the gap. At the default eight seconds a 10°
    /// offset is down to under 4° after eight seconds of looking at the middle
    /// of the screen and under 1.5° after sixteen — slow enough not to be felt
    /// as motion, fast enough to undo a posture change within a minute.
    ///
    /// Rotation adapts only on frames that **measured** it (not a held model
    /// rotation, not a one-eye reconstruction), for the same reason the manual
    /// settle window refuses them. Pitch is left alone: it has its own measured
    /// zero, and the manual recentre's [`pitch_ref`](Self::pitch_ref) is out of
    /// reach here on purpose.
    ///
    /// `head` is the head term after the neutral is subtracted and before the
    /// rotation reference is, which is the quantity both references are
    /// measured in.
    fn auto_centre(
        &mut self,
        cfg: &OutputConfig,
        head: HeadPose,
        gaze: Option<[f64; 2]>,
        rotation_held: bool,
        dt_s: f64,
    ) {
        let ac = &cfg.auto_centre;
        if !ac.enabled || self.recentre.is_some() || !ac.time_s.is_finite() || ac.time_s <= 0.0 {
            return;
        }
        let Some([gx, gy]) = gaze else { return };
        if (gx - 0.5).abs() > AUTO_CENTRE_GAZE_BOX || (gy - 0.5).abs() > AUTO_CENTRE_GAZE_BOX {
            return;
        }
        let [ref_yaw, ref_roll] = self.rot_ref.unwrap_or([0.0, 0.0]);
        let (d_yaw, d_roll) = (head.yaw_deg - ref_yaw, head.roll_deg - ref_roll);
        let offset_mm =
            (head.x_mm * head.x_mm + head.y_mm * head.y_mm + head.z_mm * head.z_mm).sqrt();
        if !(d_yaw.abs() <= ac.window_deg && d_roll.abs() <= ac.window_deg)
            || offset_mm.is_nan()
            || offset_mm > AUTO_CENTRE_WINDOW_MM
        {
            return;
        }
        let dt = dt_s.clamp(0.0, AUTO_CENTRE_MAX_DT_S);
        let k = 1.0 - (-dt / ac.time_s).exp();
        if !rotation_held {
            self.rot_ref = Some([ref_yaw + k * d_yaw, ref_roll + k * d_roll]);
        }
        let t_pos = if ac.translation_time_s.is_finite() && ac.translation_time_s > 0.0 {
            ac.translation_time_s
        } else {
            ac.time_s
        };
        let k_pos = 1.0 - (-dt / t_pos).exp();
        if let Some(n) = self.neutral.as_mut() {
            n[0] += k_pos * head.x_mm;
            n[1] += k_pos * head.y_mm;
            n[2] += k_pos * head.z_mm;
        }
    }

    /// The pose the head curves were last applied to: smoothed, recentred,
    /// Extended View composed in, not yet shaped. `None` before the first
    /// tracked frame and after a tracking loss.
    pub fn curve_input(&self) -> Option<HeadPose> {
        self.last_curve_input
    }

    /// How many of this session's frames the one-eye fallback carried.
    ///
    /// For a status line or a diagnostics report: a reconstructed pose is a
    /// guess, and a consumer has to be able to say so rather than present it
    /// as a measurement. See [`FallbackStats`] for what each field counts.
    pub fn fallback_stats(&self) -> FallbackStats {
        self.stats
    }

    /// Rebuild the filter from changed settings, without discarding anything
    /// else.
    ///
    /// Exists so a settings change does not have to mean a new pipeline: the
    /// neutral, the rotation reference and the blink state have nothing to do
    /// with the filter, and re-taking the neutral mid-session silently
    /// recentres the user on whatever pose they happened to be holding.
    ///
    /// Takes the whole config rather than one number, which is what it used to
    /// take: the filter now has two settings (`filter_alpha` and
    /// `filter_max_step_mm`), and a setter that named only one of them would
    /// quietly reset the other to its default every time the strength changed.
    pub fn set_filter(&mut self, cfg: &OutputConfig) {
        self.filter = PoseFilter::with_smoothing(cfg.smoothing(), cfg.filter_max_step_mm);
    }

    /// Compose one frame.
    ///
    /// `pose_in` is the pose from the neural model when there is one. It wins
    /// over the geometric fallback, because the model contributes pitch and two
    /// eye origins cannot: `pose_from_sample` pins pitch at zero by
    /// construction. Passing `None` asks for the geometric pose, which is what
    /// a 5-DOF run gets.
    ///
    /// It arrives as a [`SuppliedPose`] rather than a bare [`HeadPose`] so it
    /// can carry the instant its rotation was measured, which is a fact only
    /// the caller has and which the settle window needs.
    ///
    /// The geometric pose comes from [`PairOffset`], so a frame carrying only
    /// one tracked eye still produces one — with the missing eye placed at the
    /// last measured offset, and never past `RECONSTRUCTION_MAX_AGE`. With
    /// both eyes tracked it is bit for bit the two-eye geometry, unchanged.
    pub fn offer(
        &mut self,
        sample: &GazeSample,
        pose_in: Option<SuppliedPose>,
        cfg: &OutputConfig,
        corners: Option<DisplayCorners>,
        now: Instant,
    ) -> TrackingFrame {
        let presence = Presence::from_validity(sample.validity_l, sample.validity_r);
        let gaze = gaze_of(sample);

        // Offered the sample even when the model's pose is going to win: the
        // offset between the eyes can only be measured on a two-eye frame, so
        // letting the model's presence skip this would leave the fallback with
        // nothing to reconstruct from the moment the model itself drops out.
        // It changes no output — `pose_in` still wins below.
        let geometric = self.pair.pose_from_sample(sample, now);
        let source = geometric.map(|g| g.source);
        // Counted on every frame, whoever ends up supplying the pose. The
        // ratio is a statement about the eye TRACKER, not about the outgoing
        // pose: in every shipped front end `pose_in` IS the two-eye geometry
        // (`tobii_headpose::pose_from_sample`), so counting only when it was
        // absent counted only the one-eye frames — `both_eyes` stayed at zero
        // for ever, taking the denominator with it, and the status line read
        // "one eye 100%" from the first dropout of a session that was almost
        // entirely measured.
        match source {
            Some(PoseSource::BothEyes) => {
                self.stats.both_eyes = self.stats.both_eyes.saturating_add(1)
            }
            Some(PoseSource::Reconstructed) => {
                self.stats.reconstructed = self.stats.reconstructed.saturating_add(1)
            }
            None => {}
        }
        // `active` is about the pose that goes out, not about the rotation in
        // it: a supplied pose is not a reconstruction, however the geometry
        // underneath it turned out.
        let reconstructed = pose_in.is_none() && source == Some(PoseSource::Reconstructed);
        self.stats.active = reconstructed;
        // Whether this frame's rotation was held rather than measured. Both
        // paths can hold one, and only one of the two is this pipeline's to
        // know: the supplied pose carries its own stamp, and a stamp already
        // seen is that measurement arriving a second time. Inferring it from
        // `pose_in.is_none()` alone covers the geometric path and nothing else,
        // which lets a held model rotation — the longer-lived of the two holds
        // by more than three to one — into the window unchallenged.
        let rotation_held =
            reconstructed || pose_in.is_some_and(|p| self.last_rotation_at == Some(p.rotation_at));
        // Which kind of pitch this frame carries, and so which kind a
        // recentre in flight measures: the first frame's, or the model's once
        // the model is back — a window kept on the eye geometry would throw
        // every model frame away and then wipe the model's levelling.
        let measured_pitch = pose_in.is_some_and(|p| p.measured_pitch);
        if let Some(run) = self
            .recentre
            .as_mut()
            .filter(|r| measured_pitch && !r.measured_pitch)
        {
            *run = RecentreRun::new(now, true);
        }
        if let Some(p) = pose_in {
            self.last_rotation_at = Some(p.rotation_at);
        }

        // The supplied pose wins over the geometric one; from here down the
        // two are composed identically, and the stamp has done its work.
        let incoming = pose_in.map(|p| p.pose).or(geometric.map(|g| g.pose));
        let pose = incoming.map(|raw| {
            let (ev_yaw, ev_pitch) = match (cfg.extended_view.enabled, corners, gaze) {
                (true, Some(c), Some(g)) => {
                    let eye = [raw.x_mm, raw.y_mm, raw.z_mm];
                    let angles = fusion::gaze_angles_deg(&c, eye, g);
                    if let (Some((gy, gp)), Some(run)) = (angles, self.recentre.as_mut()) {
                        run.gaze_deg[0].push(gy);
                        run.gaze_deg[1].push(gp);
                        run.gaze_xy[0].push(g[0]);
                        run.gaze_xy[1].push(g[1]);
                    }
                    let (ry, rp) = self.gaze_ref.unwrap_or((0.0, 0.0));
                    let ev = match angles {
                        Some((gy, gp)) => (
                            cfg.extended_view.yaw.shape(gy - ry),
                            cfg.extended_view.pitch.shape(gp - rp),
                        ),
                        None => (0.0, 0.0),
                    };
                    self.last_ev = Some(ev);
                    self.gaze_lost_since = None;
                    ev
                }
                // Eyes shut, or gaze otherwise unavailable, with Extended View
                // on: hold the last offset and let it decay rather than
                // snapping to centre.
                (true, Some(_), None) => {
                    let since = *self.gaze_lost_since.get_or_insert(now);
                    fusion::hold_through_blink(
                        &cfg.extended_view,
                        self.last_ev,
                        now.saturating_duration_since(since).as_millis() as u64,
                    )
                }
                _ => (0.0, 0.0),
            };
            // Recentre AFTER Extended View, never before. Extended View is a
            // geometric construction: it needs the head's real position in the
            // tracker's frame, because the display corners it measures against
            // are in that same frame. Subtracting the neutral first puts the
            // head at the origin of its own coordinate system and the angle
            // collapses to nothing — which two of the tests below caught,
            // rather than it going out as a silently weaker effect.
            //
            // Rotation is recentred the other way round — before `compose`,
            // not after — and only when the user has asked for a reference.
            // See `rot_ref`, and the module docs for why the two orderings are
            // opposite. Pitch is referenced with rotation, by `pitch_ref`, on
            // top of its measured zero from `tobii headpose --calibrate-pitch`.
            let neutral = *self.neutral.get_or_insert([raw.x_mm, raw.y_mm, raw.z_mm]);
            let centred = HeadPose {
                x_mm: raw.x_mm - neutral[0],
                y_mm: raw.y_mm - neutral[1],
                z_mm: raw.z_mm - neutral[2],
                ..raw
            };
            let dt_s = self
                .last_frame_at
                .map_or(tobii_headpose::filter::NOMINAL_DT_S, |t| {
                    now.saturating_duration_since(t).as_secs_f64()
                });
            self.last_frame_at = Some(now);
            let head = centred;
            let centred = self.rereference(centred, rotation_held, measured_pitch, now);
            // After this frame's reference has been applied, so a change shows
            // from the next frame on and never as a step inside this one.
            self.auto_centre(cfg, head, gaze, rotation_held, dt_s);
            // Compose first, then filter once: smoothing head and gaze
            // separately would leave the two contributions out of phase.
            let smoothed = self
                .filter
                .update_dt(fusion::compose(centred, ev_yaw, ev_pitch), dt_s);
            // Curves next: they shape what the user sees, so their deadzone is
            // measured from the centre the user sees, after the noise is gone.
            self.last_curve_input = Some(smoothed);
            let shaped = cfg.head_curves.apply(smoothed);
            // And the slew limit last, on exactly what goes out, so a gain in
            // the curves cannot multiply a step back past it.
            let out = slew_limit(self.last_out, shaped, cfg, dt_s);
            self.last_out = Some(out);
            out
        });

        if pose.is_some() {
            self.last_tracked = Some(now);
        } else {
            let lost_for = self.last_tracked.map(|t| now.saturating_duration_since(t));
            if lost_for.is_none_or(|d| d >= TRACKING_LOSS_RESET) {
                self.filter.reset();
                self.last_frame_at = None;
                self.last_out = None;
                self.last_curve_input = None;
                self.last_tracked = None;
                self.last_ev = None;
                // Dropped with the rest of the state: whoever comes back is
                // sitting down again, and that is the position to call centre.
                self.neutral = None;
                // The offset between the eyes is state about *this* head in
                // *this* posture, so it goes with the rest. Its own
                // `RECONSTRUCTION_MAX_AGE` is far shorter than this, which
                // makes the reset belt-and-braces rather than load-bearing —
                // but leaving a measurement behind that the pipeline has
                // already declared a lie would be a trap for whoever shortens
                // one of the two bounds later.
                self.pair.reset();
                // A settle window measures *this* head in *this* posture, and
                // the head has now been gone for a second: what is left in the
                // run is whatever was seen before the user left, and the rest
                // of the window would be measured after they came back. So the
                // run is abandoned and reported, rather than averaged across
                // the gap — and rather than failing silently, which would leave
                // somebody holding still for a reference that was never coming.
                //
                // The reference itself is deliberately NOT dropped here, unlike
                // the neutral beside it: it is something the user asked for, and
                // walking away is not a request to undo it.
                if let Some(run) = self.recentre.take() {
                    self.last_recentre = Some(RecentreOutcome::Interrupted {
                        poses: run.yaw_deg.len(),
                    });
                }
            }
        }

        TrackingFrame {
            timestamp_us: sample.timestamp_us,
            pose,
            gaze,
            presence,
        }
    }
}

/// Move `prev` toward `next` by no more than the configured speed limits allow
/// in `dt_s`, axis by axis.
///
/// The first pose after a reset passes straight through: there is nothing to
/// limit against, and ramping in from zero would sweep the view on every
/// sit-down.
fn slew_limit(prev: Option<HeadPose>, next: HeadPose, cfg: &OutputConfig, dt_s: f64) -> HeadPose {
    let Some(prev) = prev else { return next };
    // A stall is not time the head had to travel in.
    let dt = if dt_s.is_finite() {
        dt_s.clamp(0.0, 0.2)
    } else {
        tobii_headpose::filter::NOMINAL_DT_S
    };
    let step = |old: f64, new: f64, speed: f64| {
        if !(speed.is_finite() && speed > 0.0) {
            return new;
        }
        let max = speed * dt;
        old + (new - old).clamp(-max, max)
    };
    let r = cfg.filter_max_rot_speed_deg_s;
    let t = cfg.filter_max_move_speed_mm_s;
    HeadPose {
        yaw_deg: step(prev.yaw_deg, next.yaw_deg, r),
        pitch_deg: step(prev.pitch_deg, next.pitch_deg, r),
        roll_deg: step(prev.roll_deg, next.roll_deg, r),
        x_mm: step(prev.x_mm, next.x_mm, t),
        y_mm: step(prev.y_mm, next.y_mm, t),
        z_mm: step(prev.z_mm, next.z_mm, t),
    }
}

/// The gaze point, when the device is actually seeing an eye.
///
/// A present bit is not validity — the sample can carry a stale gaze point with
/// the present bit set — so both are checked. `validity == 0` is "tracked" on
/// this device; see [`tobii_protocol`].
fn gaze_of(s: &GazeSample) -> Option<[f64; 2]> {
    let tracked = s.validity_l == 0 || s.validity_r == 0;
    (s.has(present::GAZE_2D) && tracked).then_some(s.gaze_point_2d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExtendedView;

    /// A sample with both eyes tracked, eye origins set, and a gaze point.
    fn tracked_sample(gaze: Option<[f64; 2]>) -> GazeSample {
        let mut s = GazeSample {
            timestamp_us: 42,
            validity_l: 0,
            validity_r: 0,
            eye_origin_l_mm: [-32.0, 0.0, 600.0],
            eye_origin_r_mm: [32.0, 0.0, 600.0],
            ..Default::default()
        };
        s.present_mask = present::EYE_ORIGIN_L
            | present::EYE_ORIGIN_R
            | present::VALIDITY_L
            | present::VALIDITY_R;
        if let Some(g) = gaze {
            s.present_mask |= present::GAZE_2D;
            s.gaze_point_2d = g;
        }
        s
    }

    /// A tracked sample with both eyes `z` mm in front of the sensor. 680 mm is
    /// where people actually sit, which is the distance the recentring exists
    /// for.
    fn seated(z: f64) -> GazeSample {
        let mut s = tracked_sample(Some([0.5, 0.5]));
        s.eye_origin_l_mm = [-32.0, 0.0, z];
        s.eye_origin_r_mm = [32.0, 0.0, z];
        s
    }

    /// A sample with neither eye tracked — validity 4 is "not found".
    fn lost() -> GazeSample {
        GazeSample {
            validity_l: 4,
            validity_r: 4,
            ..Default::default()
        }
    }

    fn corners() -> DisplayCorners {
        DisplayCorners {
            tl: [-300.0, 200.0, 0.0],
            tr: [300.0, 200.0, 0.0],
            bl: [-300.0, -140.0, 0.0],
        }
    }

    /// A pose handed in from outside, measured at `at` — the shape both front
    /// ends offer. The stamp is the fact the pipeline cannot see for itself:
    /// repeat one and the frame is carrying a rotation it has already counted.
    fn supplied(pose: HeadPose, at: Instant) -> Option<SuppliedPose> {
        Some(SuppliedPose {
            measured_pitch: true,
            pose,
            rotation_at: at,
        })
    }

    /// The composition under test, without the two output-stage effects that
    /// have tests of their own: the slew limit and the mirrored x axis. With
    /// them on, every test here that steps a pose would be measuring the
    /// limiter, and every sign check on x the mirror.
    fn cfg(ev: bool) -> OutputConfig {
        OutputConfig {
            extended_view: ExtendedView {
                enabled: ev,
                ..Default::default()
            },
            filter_max_rot_speed_deg_s: f64::INFINITY,
            filter_max_move_speed_mm_s: f64::INFINITY,
            head_curves: crate::curves::HeadCurves {
                invert: [false; 6],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// The model's pose carries pitch; two eye origins cannot express it, so a
    /// supplied pose must not be thrown away in favour of the geometric one.
    #[test]
    fn a_supplied_model_pose_wins_over_the_geometric_fallback() {
        let mut p = FramePipeline::new(&cfg(false));
        let model = HeadPose {
            pitch_deg: 17.0,
            ..Default::default()
        };
        let now = Instant::now();
        let f = p.offer(
            &tracked_sample(Some([0.5, 0.5])),
            supplied(model, now),
            &cfg(false),
            None,
            now,
        );
        let got = f.pose.expect("a pose");
        assert!(
            got.pitch_deg.abs() > 1.0,
            "the geometric fallback pins pitch at 0, so this came from the \
             model: {got:?}"
        );
    }

    /// Extended View needs all three of: enabled, a display geometry, and a
    /// gaze point. Missing any one of them contributes nothing rather than
    /// contributing garbage.
    #[test]
    fn extended_view_composes_only_when_everything_it_needs_is_present() {
        let now = Instant::now();
        let sample = tracked_sample(Some([0.9, 0.5]));

        let mut off = FramePipeline::new(&cfg(false));
        let a = off
            .offer(&sample, None, &cfg(false), Some(corners()), now)
            .pose
            .expect("a pose");

        let mut no_corners = FramePipeline::new(&cfg(true));
        let b = no_corners
            .offer(&sample, None, &cfg(true), None, now)
            .pose
            .expect("a pose");

        let mut on = FramePipeline::new(&cfg(true));
        let c = on
            .offer(&sample, None, &cfg(true), Some(corners()), now)
            .pose
            .expect("a pose");

        assert_eq!(a.yaw_deg, b.yaw_deg, "no corners must contribute nothing");
        assert!(
            (c.yaw_deg - a.yaw_deg).abs() > 0.01,
            "gaze at the screen edge should steer the view: {} vs {}",
            c.yaw_deg,
            a.yaw_deg
        );
    }

    /// Gaze vanishes for a fraction of a second every few seconds. Treating a
    /// blink as "looking straight ahead" makes the camera lurch each time.
    ///
    /// TWO pipelines with identical history, differing only in what the next
    /// sample carries: one blinks, one looks dead centre. That isolates the
    /// hold. Asserting on the blinking pipeline alone does not — checked, by
    /// deleting `hold_through_blink` and watching the single-pipeline version
    /// still pass, because forty frames of filter history keep the output high
    /// whatever the offset is. It was measuring the smoothing, not the hold.
    #[test]
    fn a_blink_holds_the_last_offset_instead_of_snapping_to_centre() {
        let now = Instant::now();
        let c = cfg(true);
        let mut blinking = FramePipeline::new(&c);
        let mut centring = FramePipeline::new(&c);

        // Identical history: look at the right-hand edge.
        for _ in 0..40 {
            let s = tracked_sample(Some([0.95, 0.5]));
            blinking.offer(&s, None, &c, Some(corners()), now);
            centring.offer(&s, None, &c, Some(corners()), now);
        }
        let held = blinking.last_ev.expect("an offset was established");
        assert!(
            held.0.abs() > 0.5,
            "the look should have produced yaw: {held:?}"
        );

        // One frame later: eyes shut vs. a genuine look at the centre.
        let blink = blinking
            .offer(&tracked_sample(None), None, &c, Some(corners()), now)
            .pose
            .expect("the head is still tracked through a blink");
        let centre = centring
            .offer(
                &tracked_sample(Some([0.5, 0.5])),
                None,
                &c,
                Some(corners()),
                now,
            )
            .pose
            .expect("a pose");

        assert!(
            blink.yaw_deg > centre.yaw_deg + 0.05,
            "a blink must hold the previous offset, not collapse toward centre: \
             blink {:.3}° vs centre {:.3}°",
            blink.yaw_deg,
            centre.yaw_deg
        );
    }

    /// A settings change must not move the centre.
    ///
    /// The device thread used to rebuild the whole pipeline when any setting
    /// changed, which took a fresh neutral from wherever the head was at that
    /// instant — change a setting while leaning and "centre" becomes the lean,
    /// permanently.
    #[test]
    fn changing_the_smoothing_strength_keeps_the_neutral() {
        let now = Instant::now();
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        p.offer(&seated(680.0), None, &c, None, now);
        assert_eq!(p.neutral.map(|n| n[2]), Some(680.0), "premise");

        p.set_filter(&OutputConfig {
            filter_alpha: 0.9,
            ..cfg(false)
        });
        assert_eq!(
            p.neutral.map(|n| n[2]),
            Some(680.0),
            "the centre is not the filter's business"
        );

        // And a later lean is still measured from the original neutral.
        let leaned = p
            .offer(&seated(580.0), None, &c, None, now)
            .pose
            .expect("a pose");
        assert!(
            leaned.z_mm < -1.0,
            "leaning 100 mm closer should read as negative displacement, got {}",
            leaned.z_mm
        );
    }

    /// A short loss keeps the smoothing state — resetting on every blink would
    /// undo the filter constantly. A long one discards it, because resuming
    /// from a second-old pose swings the camera across the room.
    #[test]
    fn the_filter_resets_only_after_a_long_loss() {
        let now = Instant::now();
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        p.offer(&tracked_sample(Some([0.5, 0.5])), None, &c, None, now);
        assert!(p.last_tracked.is_some());

        // Untracked sample, a short time later.
        p.offer(&lost(), None, &c, None, now + Duration::from_millis(200));
        assert!(
            p.last_tracked.is_some(),
            "a 200 ms loss must not discard the smoothing state"
        );

        p.offer(&lost(), None, &c, None, now + TRACKING_LOSS_RESET);
        assert!(
            p.last_tracked.is_none(),
            "a loss of a full second must reset"
        );
    }

    /// The one that matters: a head sitting at a normal distance must not
    /// saturate the axis.
    ///
    /// Translation used to go out as the head's position in the tracker's own
    /// frame, so `z ≈ 680 mm` against a ±500 mm full scale pinned the axis at
    /// its maximum for every user — and the TrackIR encoder at its own limit —
    /// permanently. Nothing failed; the axis was simply dead.
    #[test]
    fn a_head_at_a_normal_distance_does_not_peg_the_translation_axes() {
        use crate::sinks::uinput_joystick::{
            encode_axis, AXIS_CENTRE, AXIS_MAX, TRANSLATION_FULL_SCALE_MM,
        };
        let now = Instant::now();
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);

        let first = p
            .offer(&seated(680.0), None, &c, None, now)
            .pose
            .expect("a pose");
        assert_eq!(
            encode_axis(first.z_mm, TRANSLATION_FULL_SCALE_MM),
            AXIS_CENTRE,
            "the first pose is the neutral, so it must read as centre, not {}",
            AXIS_MAX
        );

        // Lean 100 mm closer: a real, unsaturated deflection.
        let leaned = p
            .offer(&seated(580.0), None, &c, None, now)
            .pose
            .expect("a pose");
        let axis = encode_axis(leaned.z_mm, TRANSLATION_FULL_SCALE_MM);
        assert!(
            axis < AXIS_CENTRE && axis > 0,
            "leaning 100 mm closer should move the axis off centre without \
             pegging it, got {axis}"
        );
    }

    /// Coming back after a long absence re-establishes the neutral, rather
    /// than leaving a permanent offset because you sat down differently.
    #[test]
    fn a_long_absence_recentres_but_a_blink_does_not() {
        let now = Instant::now();
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        p.offer(&seated(680.0), None, &c, None, now);
        assert_eq!(p.neutral.map(|n| n[2]), Some(680.0));

        p.offer(&lost(), None, &c, None, now + Duration::from_millis(200));
        assert_eq!(
            p.neutral.map(|n| n[2]),
            Some(680.0),
            "a short loss must not move the centre"
        );

        p.offer(&lost(), None, &c, None, now + TRACKING_LOSS_RESET);
        assert_eq!(p.neutral, None, "a long loss drops it");

        let back = p
            .offer(
                &seated(620.0),
                None,
                &c,
                None,
                now + Duration::from_secs(30),
            )
            .pose
            .expect("a pose");
        assert!(
            back.z_mm.abs() < 0.01,
            "sitting back down is the new centre, not a 60 mm offset: {}",
            back.z_mm
        );
    }

    /// `s` after the tracker loses the right eye, in the shape the device
    /// really sends: validity 4 with the origin column still **present** and
    /// zeroed. Every gaze frame in the committed capture
    /// (`crates/tobii-usb/tests/captures/session.tobiicap`) looks like that.
    fn right_eye_lost(s: &GazeSample) -> GazeSample {
        GazeSample {
            validity_r: 4,
            eye_origin_r_mm: [0.0; 3],
            ..s.clone()
        }
    }

    /// The regression for the path that did **not** change. With two tracked
    /// eyes the composed pose has to be exactly what it always was — asserted
    /// against pieces this change never touched, not against a recording of
    /// its own output: the first pose is the neutral, so translation is
    /// exactly zero; Extended View is off, so `compose` adds 0.0; and the
    /// filter adopts its first sample outright. What is left is the two-eye
    /// geometry, unchanged.
    #[test]
    fn with_both_eyes_the_composed_pose_is_bit_for_bit_what_it_was() {
        let now = Instant::now();
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        // Deliberately asymmetric in all three axes, so yaw and roll are both
        // non-zero and a dropped component cannot pass unnoticed.
        let mut s = tracked_sample(Some([0.5, 0.5]));
        s.eye_origin_l_mm = [-30.0, 6.0, 670.0];
        s.eye_origin_r_mm = [30.0, -4.0, 690.0];

        let got = p.offer(&s, None, &c, None, now).pose.expect("a pose");
        let geometry = tobii_headpose::pose_from_sample(&s).expect("two tracked eyes");
        assert_eq!(got.yaw_deg, geometry.yaw_deg, "yaw moved");
        assert_eq!(got.roll_deg, geometry.roll_deg, "roll moved");
        assert_eq!(got.pitch_deg, 0.0, "the geometric path pins pitch at zero");
        assert_eq!((got.x_mm, got.y_mm, got.z_mm), (0.0, 0.0, 0.0));
        assert_eq!(
            p.fallback_stats(),
            FallbackStats {
                both_eyes: 1,
                reconstructed: 0,
                active: false
            }
        );
    }

    /// The point of the change. One dropped eye used to cost the frame its
    /// pose; about five times a second, on a tracker the user cannot aim
    /// perfectly.
    #[test]
    fn one_dropped_eye_no_longer_costs_the_frame_its_pose() {
        let now = Instant::now();
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let both = seated(680.0);
        assert_eq!(
            p.offer(&both, None, &c, None, now).pose.map(|p| p.z_mm),
            Some(0.0),
            "premise: the first pose is the neutral"
        );

        let one = right_eye_lost(&both);
        assert!(
            p.offer(&one, None, &c, None, now).pose.is_none(),
            "a single flickering frame must not switch the path"
        );
        let f = p.offer(&one, None, &c, None, now);
        let pose = f.pose.expect("a dropped eye must no longer cost the pose");
        assert_eq!(f.presence, Presence::OneEye);
        // The head did not lurch toward the eye still being seen: that eye
        // sits 32 mm to the left of the centre this reports.
        assert_eq!(pose.x_mm, 0.0, "the reported centre moved sideways");
        assert_eq!(
            p.fallback_stats(),
            FallbackStats {
                both_eyes: 1,
                reconstructed: 1,
                active: true
            }
        );
    }

    /// A reconstructed pose is a guess, so it must not be able to travel as a
    /// measurement. Nothing here logs — this crate cross-compiles for the Wine
    /// bridge and has no logger — so the fallback is counted instead, and a
    /// front end is what says it out loud.
    #[test]
    fn the_fallback_is_counted_rather_than_being_silent() {
        let now = Instant::now();
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        assert_eq!(p.fallback_stats(), FallbackStats::default());

        let both = seated(680.0);
        let one = right_eye_lost(&both);
        p.offer(&both, None, &c, None, now);
        for _ in 0..4 {
            p.offer(&one, None, &c, None, now);
        }
        let stats = p.fallback_stats();
        assert_eq!(stats.both_eyes, 1);
        assert_eq!(stats.reconstructed, 3, "one frame went to the debounce");
        assert!(stats.active);

        // And it stops being "active" the moment two eyes come back.
        p.offer(&both, None, &c, None, now);
        assert!(!p.fallback_stats().active);
    }

    /// The model's pose wins, and the geometry underneath it is measured and
    /// counted anyway. Skipping the sample while a model was in charge would
    /// leave the fallback with nothing to reconstruct from at the moment the
    /// model itself dropped out, which is the moment it is needed — and the
    /// counters are about what the TRACKER delivered, so a frame with two
    /// measured eyes is a two-eye frame whoever composed the pose from it.
    #[test]
    fn a_model_pose_wins_but_the_offset_is_still_measured_underneath_it() {
        let now = Instant::now();
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let model = HeadPose {
            pitch_deg: 17.0,
            ..Default::default()
        };
        let both = seated(680.0);
        let f = p.offer(&both, supplied(model, now), &c, None, now);
        assert_eq!(f.pose.expect("a pose").pitch_deg, 17.0, "the model won");
        assert_eq!(
            p.fallback_stats(),
            FallbackStats {
                both_eyes: 1,
                reconstructed: 0,
                active: false
            },
            "the offset was measured, and counted as measured"
        );

        // The model drops out on the same frame an eye does. The offset was
        // measured while the model was in charge, so there is still a pose.
        let one = right_eye_lost(&both);
        p.offer(&one, None, &c, None, now);
        assert!(
            p.offer(&one, None, &c, None, now).pose.is_some(),
            "the offset was never measured while the model was supplying poses"
        );
    }

    /// The counters, fed the way the front ends actually feed them: a two-eye
    /// frame arrives with its pose already computed, a one-eye frame arrives
    /// with none.
    ///
    /// This is the shape no other test had. `tobii headpose` passes
    /// `fuse_pose(pose_from_sample(&sample), fresh)` and the hub passes the
    /// same stateless pose as `for_games`, and that call succeeds on exactly
    /// the frames the geometric path calls `BothEyes` — so a pipeline that
    /// counted only when `pose_in` was absent counted only the dropouts, and
    /// every front end read "one eye 100%" for the rest of the session.
    #[test]
    fn the_counters_are_the_trackers_own_ratio_at_the_shape_the_callers_use() {
        let now = Instant::now();
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let both = seated(680.0);
        let one = right_eye_lost(&both);
        let measured = || {
            supplied(
                tobii_headpose::pose_from_sample(&both).expect("two eyes"),
                now,
            )
        };

        for _ in 0..20 {
            p.offer(&both, measured(), &c, None, now);
        }
        assert_eq!(
            p.fallback_stats(),
            FallbackStats {
                both_eyes: 20,
                reconstructed: 0,
                active: false
            },
            "a frame the tracker measured with two eyes is a two-eye frame"
        );

        for _ in 0..4 {
            p.offer(&one, None, &c, None, now);
        }
        let stats = p.fallback_stats();
        assert_eq!(stats.both_eyes, 20);
        assert_eq!(stats.reconstructed, 3, "one frame went to the debounce");
        assert!(stats.active, "the pose going out right now is a guess");
        // The ratio a front end prints: a session that was almost entirely
        // measured must not read as almost entirely reconstruction.
        let pct = stats.reconstructed * 100 / (stats.both_eyes + stats.reconstructed);
        assert_eq!(pct, 13, "the ratio is the tracker's dropout rate: {pct}%");

        p.offer(&both, measured(), &c, None, now);
        assert!(
            !p.fallback_stats().active,
            "two eyes are back, so the pose going out is a measurement again"
        );
    }

    /// A loss long enough to drop the neutral drops the offset with it.
    ///
    /// This cannot fail today — `RECONSTRUCTION_MAX_AGE` (300 ms) expires long
    /// before `TRACKING_LOSS_RESET` (1 s), so the offset is stale either way.
    /// It is here for whoever changes one of those two numbers: lengthen the
    /// age bound past the loss reset and the pipeline would otherwise resume
    /// from an offset it has already called a lie.
    #[test]
    fn a_long_loss_forgets_the_offset_as_well_as_the_neutral() {
        let now = Instant::now();
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        p.offer(&seated(680.0), None, &c, None, now);
        p.offer(&lost(), None, &c, None, now + TRACKING_LOSS_RESET);
        assert_eq!(p.neutral, None, "premise: the long loss reset the state");

        let one = right_eye_lost(&seated(680.0));
        let back = now + TRACKING_LOSS_RESET + Duration::from_millis(30);
        for _ in 0..4 {
            assert!(
                p.offer(&one, None, &c, None, back).pose.is_none(),
                "reconstructed from an offset measured before the user left"
            );
        }
    }

    /// A present bit is not validity. A sample can carry a stale gaze point
    /// with the bit set, and gating on the bit alone publishes it.
    #[test]
    fn gaze_needs_both_the_present_bit_and_a_tracked_eye() {
        let mut s = tracked_sample(Some([0.25, 0.75]));
        assert_eq!(gaze_of(&s), Some([0.25, 0.75]));
        s.validity_l = 4;
        s.validity_r = 4;
        assert_eq!(gaze_of(&s), None, "neither eye is tracked");
    }

    /// The cadence measured between consecutive gaze frames in the committed
    /// capture, so a settle window in these tests holds the number of frames it
    /// holds in life (~33).
    const CADENCE: Duration = Duration::from_micros(30_208);

    /// The yaw of the head in this project's own dropout session: 17.4° off the
    /// tracker's optical axis. The bias the recentre exists to remove.
    const OFF_AXIS_YAW_DEG: f64 = 17.4;

    /// A tracked sample with the head turned `yaw_deg` and tilted `roll_deg`.
    ///
    /// Built the way `tobii-headpose`'s own tests build one: each projection's
    /// tangent is set directly, because yaw and roll are independent
    /// projections of the one interocular vector rather than composed
    /// rotations.
    fn head_at(yaw_deg: f64, roll_deg: f64) -> GazeSample {
        const HALF_IPD: f64 = 31.5;
        let half = [
            HALF_IPD,
            HALF_IPD * -roll_deg.to_radians().tan(),
            HALF_IPD * yaw_deg.to_radians().tan(),
        ];
        let mut s = tracked_sample(Some([0.5, 0.5]));
        s.eye_origin_l_mm = [-half[0], -half[1], 680.0 - half[2]];
        s.eye_origin_r_mm = [half[0], half[1], 680.0 + half[2]];
        s
    }

    /// Offer one sample `frames` times at the measured cadence, and answer with
    /// the time of the frame after the last one.
    fn feed(
        p: &mut FramePipeline,
        s: &GazeSample,
        c: &OutputConfig,
        corners: Option<DisplayCorners>,
        from: Instant,
        frames: usize,
    ) -> Instant {
        let mut at = from;
        for _ in 0..frames {
            p.offer(s, None, c, corners, at);
            at += CADENCE;
        }
        at
    }

    /// Frames in a settle window, plus enough after it for the filter to have
    /// followed the step the new reference makes. Sized for the adaptive
    /// filter at its default rest cutoff, the slower of the two smoothers: the
    /// step opens it, and it then closes in on the target at ~0.07 a frame.
    const WINDOW_FRAMES: usize = 60;

    /// The point of the whole thing. A user sitting off the tracker's axis has
    /// a permanent in-game bias, and before this there was no control that
    /// removed it.
    #[test]
    fn a_recentre_makes_the_pose_the_user_is_holding_straight_ahead() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let sitting = head_at(OFF_AXIS_YAW_DEG, -3.0);
        let start = Instant::now();

        let at = feed(&mut p, &sitting, &c, None, start, WINDOW_FRAMES);
        let biased = p.offer(&sitting, None, &c, None, at).pose.expect("a pose");
        assert!(
            (biased.yaw_deg - OFF_AXIS_YAW_DEG).abs() < 0.01,
            "premise: an off-axis head reads as turned: {biased:?}"
        );
        assert_eq!(p.rotation_reference(), None, "premise: nothing referenced");

        p.begin_recentre(at);
        assert!(p.recentring(), "the window is running");
        let at = feed(&mut p, &sitting, &c, None, at, WINDOW_FRAMES * 2);
        assert!(!p.recentring(), "and it ends on its own");

        match p.take_recentre() {
            Some(RecentreOutcome::Applied {
                yaw_deg,
                roll_deg,
                spread_deg,
                ..
            }) => {
                assert!((yaw_deg - OFF_AXIS_YAW_DEG).abs() < 0.01, "yaw {yaw_deg}");
                assert!((roll_deg + 3.0).abs() < 0.01, "roll {roll_deg}");
                assert!(
                    spread_deg < 0.01,
                    "a still head has no spread: {spread_deg}"
                );
            }
            other => panic!("expected an applied reference, got {other:?}"),
        }
        assert_eq!(p.take_recentre(), None, "the outcome is reported once");

        let straight = p.offer(&sitting, None, &c, None, at).pose.expect("a pose");
        assert!(
            straight.yaw_deg.abs() < 0.01 && straight.roll_deg.abs() < 0.01,
            "the pose the user is holding must now read as straight ahead: {straight:?}"
        );
        // And a real turn from there still turns, by the amount turned.
        let turned = feed(
            &mut p,
            &head_at(OFF_AXIS_YAW_DEG + 10.0, -3.0),
            &c,
            None,
            at,
            WINDOW_FRAMES,
        );
        let turned = p
            .offer(
                &head_at(OFF_AXIS_YAW_DEG + 10.0, -3.0),
                None,
                &c,
                None,
                turned,
            )
            .pose
            .expect("a pose");
        assert!(
            (turned.yaw_deg - 10.0).abs() < 0.01,
            "a 10° turn from the reference must read as 10°: {turned:?}"
        );
    }

    /// A reference is absolute, not cumulative. `rereference` feeds the settle
    /// run the head angle BEFORE the standing reference is subtracted, which is
    /// what makes asking twice from a new posture make THAT posture straight
    /// ahead. Hoist the subtraction above the two pushes and the second window
    /// measures a residual of ~0, adopts ~0, and hands the off-axis bias the
    /// first recentre removed straight back — silently.
    #[test]
    fn a_second_recentre_replaces_the_first_rather_than_composing_with_it() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let start = Instant::now();

        let first = head_at(OFF_AXIS_YAW_DEG, 0.0);
        p.begin_recentre(start);
        let at = feed(&mut p, &first, &c, None, start, WINDOW_FRAMES * 2);
        let ref1 = p.rotation_reference().expect("premise: a first reference");
        assert!(
            (ref1[0] - OFF_AXIS_YAW_DEG).abs() < 0.01,
            "premise: {ref1:?}"
        );

        // A refused second window leaves the FIRST reference standing — the
        // case `settle`'s `Moved` arm exists for, and the one the refusal test
        // below cannot see because it has no reference to preserve.
        p.begin_recentre(at);
        let mut turning = at;
        for i in 0..WINDOW_FRAMES + 1 {
            p.offer(&head_at(i as f64, 0.0), None, &c, None, turning);
            turning += CADENCE;
        }
        assert!(matches!(
            p.take_recentre(),
            Some(RecentreOutcome::Moved { .. })
        ));
        assert_eq!(
            p.rotation_reference(),
            Some(ref1),
            "a refusal must not drop the reference already in use"
        );

        // And an accepted one replaces it outright.
        let second = head_at(OFF_AXIS_YAW_DEG + 12.0, 0.0);
        p.begin_recentre(turning);
        let at = feed(&mut p, &second, &c, None, turning, WINDOW_FRAMES * 2);
        let ref2 = p.rotation_reference().expect("a second reference");
        assert!(
            (ref2[0] - (OFF_AXIS_YAW_DEG + 12.0)).abs() < 0.01,
            "the reference is the absolute head angle, not a correction on top \
             of the old one: {ref2:?}"
        );
        let held = p.offer(&second, None, &c, None, at).pose.expect("a pose");
        assert!(
            held.yaw_deg.abs() < 0.01,
            "the new posture is what reads as straight ahead: {held:?}"
        );
    }

    /// Two presses mean "start from now", not "do it twice": the second ask
    /// discards what the first had collected, or the reference is averaged over
    /// exactly the motion the user pressed again to throw away — and refused
    /// for it, while they sit perfectly still.
    #[test]
    fn asking_again_restarts_the_window_rather_than_queueing() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let start = Instant::now();
        p.begin_recentre(start);

        // Two thirds of a window (20 frames, 604 ms) spent settling into the
        // chair, 40° away from where the user ends up.
        let at = feed(&mut p, &head_at(40.0, 0.0), &c, None, start, 20);
        // Then: "hold on" — sit still and ask again.
        p.begin_recentre(at);
        let sitting = head_at(OFF_AXIS_YAW_DEG, 0.0);
        feed(&mut p, &sitting, &c, None, at, WINDOW_FRAMES * 2);

        match p.take_recentre() {
            Some(RecentreOutcome::Applied {
                yaw_deg,
                spread_deg,
                ..
            }) => {
                assert!(
                    (yaw_deg - OFF_AXIS_YAW_DEG).abs() < 0.01,
                    "the restarted window must hold only the still head: {yaw_deg}"
                );
                assert!(
                    spread_deg < 0.01,
                    "and no spread from before the second ask: {spread_deg}"
                );
            }
            // Queueing instead would answer `Moved`: twenty samples at 40° and
            // fifteen at 17.4° in one window.
            other => panic!("the second ask must start a fresh window, got {other:?}"),
        }
    }

    /// The trap this ordering exists for, and the one that cannot be found by
    /// looking at the numbers afterwards: recentring while the user glances at
    /// a screen edge must not write the Extended View offset into the
    /// reference, because nothing ever decays it out again.
    #[test]
    fn recentring_while_looking_at_a_screen_edge_does_not_bake_in_extended_view() {
        let c = cfg(true);
        let edge = {
            let mut s = head_at(OFF_AXIS_YAW_DEG, 0.0);
            s.gaze_point_2d = [0.95, 0.5];
            s
        };
        let centre = head_at(OFF_AXIS_YAW_DEG, 0.0);
        let mut p = FramePipeline::new(&c);
        let start = Instant::now();

        let at = feed(&mut p, &edge, &c, Some(corners()), start, WINDOW_FRAMES);
        let composed = p
            .offer(&edge, None, &c, Some(corners()), at)
            .pose
            .expect("a pose");
        assert!(
            composed.yaw_deg > OFF_AXIS_YAW_DEG + 10.0,
            "premise: the glance contributes most of the composed angle: {composed:?}"
        );

        p.begin_recentre(at);
        let at = feed(&mut p, &edge, &c, Some(corners()), at, WINDOW_FRAMES * 2);
        let reference = p.rotation_reference().expect("a reference was taken");
        assert!(
            (reference[0] - OFF_AXIS_YAW_DEG).abs() < 0.01,
            "the reference is the HEAD's yaw, not the composed angle: {reference:?} \
             against a composed {composed:?}"
        );

        // Looking back at the centre of the screen is now straight ahead...
        let ahead = feed(&mut p, &centre, &c, Some(corners()), at, WINDOW_FRAMES);
        let ahead = p
            .offer(&centre, None, &c, Some(corners()), ahead)
            .pose
            .expect("a pose");
        assert!(
            ahead.yaw_deg.abs() < 0.01,
            "looking at the centre after recentring at the edge must be straight \
             ahead, not {}°",
            ahead.yaw_deg
        );
        // ...and the glance still steers the view exactly as far as it did.
        let glance = feed(&mut p, &edge, &c, Some(corners()), at, WINDOW_FRAMES);
        let glance = p
            .offer(&edge, None, &c, Some(corners()), glance)
            .pose
            .expect("a pose");
        assert!(
            (glance.yaw_deg - (composed.yaw_deg - OFF_AXIS_YAW_DEG)).abs() < 0.01,
            "the Extended View swing must be untouched: {} vs {}",
            glance.yaw_deg,
            composed.yaw_deg - OFF_AXIS_YAW_DEG
        );
    }

    /// And position: somebody who shifted 40 mm to the side since tracking
    /// started reads zero on X after a recentre, not the 40 mm they moved.
    #[test]
    fn a_recentre_zeroes_position_too() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let start = Instant::now();
        // Neutral taken here.
        p.offer(&head_at(0.0, 0.0), None, &c, None, start);
        let mut moved = head_at(0.0, 0.0);
        moved.eye_origin_l_mm[0] += 40.0;
        moved.eye_origin_r_mm[0] += 40.0;
        let mut at = start + CADENCE;
        // Let the filter settle on the move, then recentre.
        for _ in 0..200 {
            p.offer(&moved, None, &c, None, at);
            at += CADENCE;
        }
        let before = p.offer(&moved, None, &c, None, at).pose.expect("a pose");
        assert!((before.x_mm - 40.0).abs() < 1.0, "premise: {before:?}");
        p.begin_recentre(at);
        for _ in 0..WINDOW_FRAMES * 2 {
            at += CADENCE;
            p.offer(&moved, None, &c, None, at);
        }
        assert!(matches!(
            p.take_recentre(),
            Some(RecentreOutcome::Applied { .. })
        ));
        for _ in 0..200 {
            at += CADENCE;
            p.offer(&moved, None, &c, None, at);
        }
        let after = p.offer(&moved, None, &c, None, at).pose.expect("a pose");
        assert!(after.x_mm.abs() < 0.5, "X should read zero: {after:?}");
    }

    /// A recentre zeroes pitch too: a recentre that left any head axis
    /// off-centre read, in use, as a recentre that did not work.
    #[test]
    fn a_recentre_zeroes_pitch_too() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let sample = head_at(OFF_AXIS_YAW_DEG, 0.0);
        // A model pose: the only path that carries pitch at all.
        let model = HeadPose {
            yaw_deg: OFF_AXIS_YAW_DEG,
            pitch_deg: 12.0,
            ..Default::default()
        };
        let start = Instant::now();
        let mut at = start;
        p.begin_recentre(at);
        for _ in 0..WINDOW_FRAMES * 2 {
            p.offer(&sample, supplied(model, at), &c, None, at);
            at += CADENCE;
        }
        assert!(
            matches!(p.take_recentre(), Some(RecentreOutcome::Applied { .. })),
            "premise: the reference was taken"
        );

        let after = p
            .offer(&sample, supplied(model, at), &c, None, at)
            .pose
            .expect("a pose");
        assert!(
            after.pitch_deg.abs() < 0.01,
            "pitch was referenced: {after:?}"
        );
        assert!(after.yaw_deg.abs() < 0.01, "yaw was referenced: {after:?}");
    }

    /// Pitch is adopted, so pitch must be still too. Yaw held perfectly while
    /// the head nods through 40°: the stillness bar used to look at yaw and
    /// roll only, so this was `Applied` with a spread of 0 and the nod's middle
    /// (+15°) became level — a level head then read −15° for the rest of the
    /// session.
    #[test]
    fn a_nod_during_the_window_is_refused_not_adopted_as_level() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let sample = head_at(OFF_AXIS_YAW_DEG, 0.0);
        let start = Instant::now();
        let mut at = start;
        p.begin_recentre(at);
        for i in 0..WINDOW_FRAMES * 2 {
            let nod = HeadPose {
                yaw_deg: OFF_AXIS_YAW_DEG,
                pitch_deg: 15.0 + 20.0 * (i as f64 * 0.3).sin(),
                ..Default::default()
            };
            p.offer(&sample, supplied(nod, at), &c, None, at);
            at += CADENCE;
        }
        match p.take_recentre() {
            Some(RecentreOutcome::Moved { spread_deg }) => assert!(
                spread_deg > RECENTRE_MAX_SPREAD_DEG,
                "refused for a spread inside the bar: {spread_deg}"
            ),
            other => panic!("a nodding head must be refused, got {other:?}"),
        }
        assert_eq!(p.rotation_reference(), None, "nothing was adopted");

        // And the pitch reference was not adopted on the side: level reads level.
        let level = HeadPose {
            yaw_deg: OFF_AXIS_YAW_DEG,
            ..Default::default()
        };
        let mut after = None;
        for _ in 0..200 {
            at += CADENCE;
            after = p.offer(&sample, supplied(level, at), &c, None, at).pose;
        }
        let after = after.expect("a pose");
        assert!(
            after.pitch_deg.abs() < 0.5,
            "level must read level: {after:?}"
        );
    }

    /// A recentre's pitch is a number in the frame of the pitch zero it was
    /// measured against, so it has to go when that zero changes. A recentre at
    /// an uncalibrated +22°, then a calibration that saves those 22° as the
    /// zero: the source now reads 0 for a level head, and a pitch reference
    /// still in place would send that out as −22°.
    #[test]
    fn forgetting_the_pitch_reference_stops_it_subtracting_a_new_zero() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let sample = head_at(OFF_AXIS_YAW_DEG, 0.0);
        let uncalibrated = HeadPose {
            yaw_deg: OFF_AXIS_YAW_DEG,
            pitch_deg: 22.0,
            ..Default::default()
        };
        let start = Instant::now();
        let mut at = start;
        p.begin_recentre(at);
        for _ in 0..WINDOW_FRAMES * 2 {
            p.offer(&sample, supplied(uncalibrated, at), &c, None, at);
            at += CADENCE;
        }
        assert!(
            matches!(p.take_recentre(), Some(RecentreOutcome::Applied { .. })),
            "premise: the reference was taken"
        );

        p.forget_pitch_reference(at);
        let calibrated = HeadPose {
            pitch_deg: 0.0,
            ..uncalibrated
        };
        let mut after = None;
        for _ in 0..200 {
            at += CADENCE;
            after = p
                .offer(&sample, supplied(calibrated, at), &c, None, at)
                .pose;
        }
        let after = after.expect("a pose");
        assert!(
            after.pitch_deg.abs() < 0.5,
            "the old pitch reference was subtracted from the new zero: {after:?}"
        );
        assert!(
            p.rotation_reference().is_some(),
            "yaw and roll are not the pitch zero's to drop"
        );
        assert!(after.yaw_deg.abs() < 0.5, "yaw still referenced: {after:?}");
    }

    /// The model goes stale and the geometric path takes over: its pitch is
    /// level at 0 already, so the model's pitch reference is not subtracted
    /// from it — that sent −22° for a level head. A window that spans the
    /// switch measures the source it started on, rather than reading the jump
    /// between the two as a head that moved.
    #[test]
    fn the_geometric_fallback_neither_takes_the_pitch_reference_nor_spoils_a_window() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let sample = head_at(OFF_AXIS_YAW_DEG, 0.0);
        let model = HeadPose {
            yaw_deg: OFF_AXIS_YAW_DEG,
            pitch_deg: 22.0,
            ..Default::default()
        };
        let mut at = Instant::now();
        // Half the window from the model, the rest from the geometric path.
        p.begin_recentre(at);
        for i in 0..WINDOW_FRAMES * 2 {
            let pose = if i < WINDOW_FRAMES / 2 {
                supplied(model, at)
            } else {
                None
            };
            p.offer(&sample, pose, &c, None, at);
            at += CADENCE;
        }
        match p.take_recentre() {
            Some(RecentreOutcome::Applied { pitch_deg, .. }) => assert!(
                (pitch_deg - 22.0).abs() < 0.5,
                "measured on the model's frames: {pitch_deg}"
            ),
            other => panic!("the switch is not a head that moved, got {other:?}"),
        }
        // The eye geometry, as the pipeline's own fallback and as a front end
        // hands it over when its model has nothing fresh.
        let geometric = |at| {
            Some(SuppliedPose {
                pose: HeadPose {
                    pitch_deg: 0.0,
                    ..model
                },
                rotation_at: at,
                measured_pitch: false,
            })
        };
        for fallback in [None, geometric(at)] {
            let mut after = None;
            for _ in 0..200 {
                at += CADENCE;
                let pose = fallback.map(|f| SuppliedPose {
                    rotation_at: at,
                    ..f
                });
                after = p.offer(&sample, pose, &c, None, at).pose;
            }
            let after = after.expect("a pose");
            assert!(
                after.pitch_deg.abs() < 0.5,
                "the eye geometry's level 0 went out shifted by the model's reference: {after:?}"
            );
        }

        // A window that starts on the eye geometry and sees the model come
        // back measures on the model; one that never sees it leaves the
        // model's levelling as it was.
        p.begin_recentre(at);
        for i in 0..WINDOW_FRAMES * 3 {
            let pose = if i < 5 {
                geometric(at)
            } else {
                supplied(model, at)
            };
            p.offer(&sample, pose, &c, None, at);
            at += CADENCE;
        }
        assert!(
            matches!(p.take_recentre(), Some(RecentreOutcome::Applied { pitch_deg, .. }) if (pitch_deg - 22.0).abs() < 0.5),
            "the model came back, and the window measured on it"
        );
        p.begin_recentre(at);
        for _ in 0..WINDOW_FRAMES * 2 {
            p.offer(&sample, geometric(at), &c, None, at);
            at += CADENCE;
        }
        assert!(matches!(
            p.take_recentre(),
            Some(RecentreOutcome::Applied { .. })
        ));
        let level = p.offer(&sample, supplied(model, at), &c, None, at).pose;
        assert!(
            level.is_some_and(|l| l.pitch_deg.abs() < 0.5),
            "a window of the eye geometry wiped the model's levelling: {level:?}"
        );
    }

    /// The zero changes while a recentre is still measuring. The window has
    /// almost a second of the old zero's 22° in it and would close on a few
    /// frames of the new zero's 0°: `Applied` at 22°, the exact stale number
    /// forgetting was for. It starts over instead and measures the new zero.
    #[test]
    fn forgetting_restarts_a_recentre_that_is_still_measuring() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let sample = head_at(OFF_AXIS_YAW_DEG, 0.0);
        let uncalibrated = HeadPose {
            yaw_deg: OFF_AXIS_YAW_DEG,
            pitch_deg: 22.0,
            ..Default::default()
        };
        let calibrated = HeadPose {
            pitch_deg: 0.0,
            ..uncalibrated
        };
        let mut at = Instant::now();
        p.begin_recentre(at);
        for _ in 0..32 {
            p.offer(&sample, supplied(uncalibrated, at), &c, None, at);
            at += CADENCE;
        }
        assert!(p.recentring(), "premise: the window is still open");

        p.forget_pitch_reference(at);
        for _ in 0..WINDOW_FRAMES * 2 {
            p.offer(&sample, supplied(calibrated, at), &c, None, at);
            at += CADENCE;
        }
        match p.take_recentre() {
            Some(RecentreOutcome::Applied { pitch_deg, .. }) => assert!(
                pitch_deg.abs() < 0.5,
                "the window kept the old zero's samples: {pitch_deg}"
            ),
            other => panic!("a fresh window of a still head applies, got {other:?}"),
        }
    }

    /// A reference captured mid-turn is worse than none — and worse than the
    /// one already in use, so a moving head leaves the old one standing and
    /// says so rather than failing silently.
    #[test]
    fn a_head_that_keeps_moving_is_refused_rather_than_averaged() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let start = Instant::now();
        p.begin_recentre(start);

        // A head turning steadily through 40° over the window — a turn, not a
        // posture.
        let mut at = start;
        for i in 0..WINDOW_FRAMES + 1 {
            let yaw = i as f64;
            p.offer(&head_at(yaw, 0.0), None, &c, None, at);
            at += CADENCE;
        }

        match p.take_recentre() {
            Some(RecentreOutcome::Moved { spread_deg }) => assert!(
                spread_deg > RECENTRE_MAX_SPREAD_DEG,
                "refused for a spread that was inside the bar: {spread_deg}"
            ),
            other => panic!("a turning head must be refused, got {other:?}"),
        }
        assert_eq!(
            p.rotation_reference(),
            None,
            "a refused window must leave the reference alone"
        );
    }

    /// A window the user walks out of is abandoned rather than averaged across
    /// the gap — and must not leave them holding still for a reference that is
    /// never coming.
    ///
    /// Reported as its own outcome, not as `NoHead`. A run cut short can hold
    /// any number of poses, including plenty: this one holds twice
    /// `RECENTRE_MIN_POSES`, so calling it "only 20 frames measured your head
    /// angle" against a floor of ten would send the user to check their
    /// tracker instead of to sit back down.
    #[test]
    fn a_window_the_user_walks_out_of_is_reported_as_interrupted() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let start = Instant::now();
        p.begin_recentre(start);
        // Two thirds of a second of perfectly good poses, well over the floor.
        let at = feed(&mut p, &head_at(OFF_AXIS_YAW_DEG, 0.0), &c, None, start, 20);

        // Then the user walks off, and stays gone long enough to reset.
        p.offer(&lost(), None, &c, None, at + TRACKING_LOSS_RESET);
        assert_eq!(
            p.take_recentre(),
            Some(RecentreOutcome::Interrupted { poses: 20 }),
            "a window cut short is not a window the tracker could not see"
        );
        assert!(!p.recentring(), "the run must not still be waiting");
        assert_eq!(p.rotation_reference(), None);
    }

    /// The worse of the two axes, not just yaw: a head that held its yaw while
    /// swinging in roll was still moving, and a reference taken from the middle
    /// of that swing is permanent.
    #[test]
    fn a_head_that_holds_its_yaw_while_rolling_is_still_refused() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let start = Instant::now();
        p.begin_recentre(start);

        let mut at = start;
        for i in 0..WINDOW_FRAMES + 1 {
            p.offer(&head_at(OFF_AXIS_YAW_DEG, i as f64), None, &c, None, at);
            at += CADENCE;
        }

        match p.take_recentre() {
            Some(RecentreOutcome::Moved { spread_deg }) => assert!(
                spread_deg > RECENTRE_MAX_SPREAD_DEG,
                "a roll swing must be refused: {spread_deg}"
            ),
            other => panic!("a rolling head must be refused, got {other:?}"),
        }
        assert_eq!(
            p.rotation_reference(),
            None,
            "a refused window must leave the reference alone"
        );
    }

    /// A model that degenerates to NaN must cost the window its poses rather
    /// than install a reference: `rot_ref` survives a tracking loss on purpose,
    /// and a NaN in it makes every later frame non-finite, which the pose
    /// filter then holds — the head axes freeze until the user recentres again.
    #[test]
    fn a_window_of_non_finite_angles_is_refused_rather_than_averaged() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let sample = head_at(OFF_AXIS_YAW_DEG, 0.0);
        let broken = HeadPose {
            yaw_deg: f64::NAN,
            roll_deg: f64::NAN,
            ..Default::default()
        };
        let mut at = Instant::now();
        p.begin_recentre(at);
        for _ in 0..WINDOW_FRAMES * 2 {
            p.offer(&sample, supplied(broken, at), &c, None, at);
            at += CADENCE;
        }
        assert_eq!(
            p.take_recentre(),
            Some(RecentreOutcome::NoHead { poses: 0 }),
            "non-finite angles must be dropped and the window refused for being empty"
        );
        assert_eq!(
            p.rotation_reference(),
            None,
            "a NaN reference would freeze the head axes from here on"
        );
    }

    /// Dropping the non-finite samples must not discard the window with them:
    /// the poses that *were* finite are still a measurement, and the median has
    /// to come from them.
    #[test]
    fn non_finite_angles_are_dropped_without_costing_the_window_the_rest() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let sample = head_at(OFF_AXIS_YAW_DEG, 0.0);
        let good = HeadPose {
            yaw_deg: OFF_AXIS_YAW_DEG,
            ..Default::default()
        };
        let broken = HeadPose {
            yaw_deg: f64::NAN,
            roll_deg: f64::NAN,
            ..Default::default()
        };
        let mut at = Instant::now();
        p.begin_recentre(at);
        for i in 0..WINDOW_FRAMES * 2 {
            let pose = if i % 2 == 0 { good } else { broken };
            p.offer(&sample, supplied(pose, at), &c, None, at);
            at += CADENCE;
        }
        match p.take_recentre() {
            Some(RecentreOutcome::Applied { yaw_deg, .. }) => assert!(
                (yaw_deg - OFF_AXIS_YAW_DEG).abs() < 1e-9,
                "the median must come from the finite half: {yaw_deg}"
            ),
            other => panic!("half a window of good poses is still a measurement, got {other:?}"),
        }
    }

    /// The settle window measures rotation, so it counts only frames that
    /// measured one.
    ///
    /// A reconstructed frame's yaw and roll are the last two-eye frame's,
    /// bit for bit — `PairOffset` holds the interocular vector and
    /// `pose_from_eyes` reads the rotation off precisely that vector. Pushing
    /// them would let one measurement fill most of a window: the count climbs
    /// past `RECENTRE_MIN_POSES` on frames that measured nothing, and the
    /// 10-90% spread collapses toward zero while the head was free to move
    /// through the outage. Both halves of the gate, loosened by the same
    /// duplicates.
    ///
    /// The window still has to be able to CLOSE on a reconstructed frame; only
    /// the push is gated. Here it closes on the two-eye frame that ends the
    /// outage.
    #[test]
    fn a_held_rotation_is_not_a_second_measurement_of_the_head() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let sitting = head_at(OFF_AXIS_YAW_DEG, 0.0);
        let one_eye = right_eye_lost(&sitting);
        let start = Instant::now();
        p.begin_recentre(start);

        // One measured frame, which is also what the offset is measured from.
        p.offer(&sitting, None, &c, None, start);
        // Then the outage: the fallback keeps producing poses for as long as
        // `RECONSTRUCTION_MAX_AGE` allows, all carrying that frame's rotation.
        let mut at = start + CADENCE;
        while at < start + RECENTRE_WINDOW {
            p.offer(&one_eye, None, &c, None, at);
            at += CADENCE;
        }
        // Two eyes again, inside `TRACKING_LOSS_RESET`, which closes the window.
        p.offer(&sitting, None, &c, None, at);

        assert_eq!(
            p.take_recentre(),
            Some(RecentreOutcome::NoHead { poses: 2 }),
            "only the two frames that measured a rotation may count"
        );
        assert_eq!(
            p.rotation_reference(),
            None,
            "one measurement repeated is not a second of stillness"
        );
    }

    /// The same gate on the supplied path, where it costs more.
    ///
    /// The model runs on the camera stream, and both front ends hold its last
    /// rotation across the gaze frames that arrive without a confident
    /// estimate behind them — for up to a second, which is the whole window.
    /// So a user who asks for a recentre just as the model loses confidence
    /// would fill the run with one measurement republished ~33 times: the
    /// floor cleared by duplicates, the spread reading 0.0, and a permanent
    /// "straight ahead" adopted from a single stale frame. Three times what a
    /// reconstruction can pad, and exactly what the window exists to refuse.
    ///
    /// Only the caller knows — a held rotation is bit for bit the one it was
    /// measured from — so the frame carries the stamp and the run counts
    /// stamps rather than frames.
    #[test]
    fn a_held_model_rotation_is_not_a_second_measurement_of_the_head() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let sitting = head_at(OFF_AXIS_YAW_DEG, 0.0);
        // A model pose, which is the only path that carries pitch at all.
        let model = HeadPose {
            yaw_deg: OFF_AXIS_YAW_DEG,
            pitch_deg: 4.0,
            ..Default::default()
        };
        let start = Instant::now();
        // Measured once, one frame before the button was pressed, and held
        // from there on: the stamp never moves again.
        let measured_at = start - CADENCE;
        p.begin_recentre(start);

        let mut at = start;
        for _ in 0..WINDOW_FRAMES * 2 {
            p.offer(&sitting, supplied(model, measured_at), &c, None, at);
            at += CADENCE;
        }

        assert_eq!(
            p.take_recentre(),
            Some(RecentreOutcome::NoHead { poses: 1 }),
            "one measurement held across a window is one pose, not eighty"
        );
        assert_eq!(
            p.rotation_reference(),
            None,
            "a reference from one stale frame is the reference this window exists to refuse"
        );
    }

    /// And the gate asks whether the measurement is new, not whether it was
    /// taken on this very frame.
    ///
    /// The camera and the gaze stream both run at ~33 Hz and are not in
    /// lockstep, so every model measurement reaches the pipeline on a frame
    /// slightly later than itself. Gating on "measured at this instant" would
    /// refuse all of them and turn an ordinary session's recentre into
    /// `NoHead` — a refusal the user could do nothing about, since sitting
    /// stiller does not align two free-running streams.
    #[test]
    fn a_model_measurement_older_than_the_frame_carrying_it_still_counts() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let sitting = head_at(OFF_AXIS_YAW_DEG, 0.0);
        let model = HeadPose {
            yaw_deg: OFF_AXIS_YAW_DEG,
            ..Default::default()
        };
        let start = Instant::now();
        p.begin_recentre(start);

        let mut at = start;
        for _ in 0..WINDOW_FRAMES * 2 {
            // A new measurement each frame, each taken a few milliseconds
            // before the gaze frame that carries it.
            let measured_at = at - Duration::from_millis(7);
            p.offer(&sitting, supplied(model, measured_at), &c, None, at);
            at += CADENCE;
        }

        match p.take_recentre() {
            Some(RecentreOutcome::Applied {
                yaw_deg,
                spread_deg,
                ..
            }) => {
                assert!((yaw_deg - OFF_AXIS_YAW_DEG).abs() < 0.01, "yaw {yaw_deg}");
                assert!(
                    spread_deg < 0.01,
                    "a still head has no spread: {spread_deg}"
                );
            }
            other => panic!("a window of fresh measurements must be applied, got {other:?}"),
        }
    }

    /// The reference is something the user asked for, so walking away does not
    /// undo it — unlike the neutral beside it, which is re-taken precisely
    /// because sitting back down is a new centre.
    #[test]
    fn a_tracking_loss_re_takes_the_neutral_but_keeps_the_reference() {
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let sitting = head_at(OFF_AXIS_YAW_DEG, 0.0);
        let start = Instant::now();
        p.begin_recentre(start);
        let at = feed(&mut p, &sitting, &c, None, start, WINDOW_FRAMES * 2);
        let reference = p.rotation_reference().expect("premise: a reference");

        p.offer(&lost(), None, &c, None, at + TRACKING_LOSS_RESET);
        assert_eq!(
            p.neutral, None,
            "premise: the long loss dropped the neutral"
        );
        assert_eq!(
            p.rotation_reference(),
            Some(reference),
            "an absence must not silently undo a reference the user set"
        );
        let back = p
            .offer(&sitting, None, &c, None, at + Duration::from_secs(60))
            .pose
            .expect("a pose");
        assert!(
            back.yaw_deg.abs() < 0.01,
            "sitting back down the same way is still straight ahead: {back:?}"
        );
    }

    /// `filter_max_step_mm` has to reach the filter, or it is a key `tobii
    /// games` lists and nothing reads.
    #[test]
    fn the_configured_jump_limit_reaches_the_filter() {
        let now = Instant::now();
        let tight = OutputConfig {
            // Far below the 100 mm lean below, so that lean is refused. The
            // lean itself is the one both this crate and the hub already use to
            // assert that the axis moves.
            filter_max_step_mm: 10.0,
            ..cfg(false)
        };
        let mut p = FramePipeline::new(&tight);
        p.offer(&seated(680.0), None, &tight, None, now);
        let held = p
            .offer(&seated(580.0), None, &tight, None, now)
            .pose
            .expect("a pose");
        assert!(
            held.z_mm.abs() < 0.01,
            "a 100 mm step past a 10 mm limit must be held, not blended: {held:?}"
        );

        // The same lean, with the shipped default, is ordinary motion.
        let loose = cfg(false);
        let mut p = FramePipeline::new(&loose);
        p.offer(&seated(680.0), None, &loose, None, now);
        let moved = p
            .offer(&seated(580.0), None, &loose, None, now)
            .pose
            .expect("a pose");
        assert!(
            moved.z_mm < -1.0,
            "premise: within the default limit this lean is followed: {moved:?}"
        );
    }

    /// Changing the smoothing strength must not quietly reset the other filter
    /// setting to its default — which is exactly what a setter naming only
    /// alpha would do.
    #[test]
    fn changing_the_smoothing_strength_keeps_the_configured_jump_limit() {
        let now = Instant::now();
        let tight = OutputConfig {
            filter_max_step_mm: 10.0,
            ..cfg(false)
        };
        let mut p = FramePipeline::new(&tight);
        p.set_filter(&OutputConfig {
            filter_alpha: 0.9,
            ..tight.clone()
        });
        p.offer(&seated(680.0), None, &tight, None, now);
        let held = p
            .offer(&seated(580.0), None, &tight, None, now)
            .pose
            .expect("a pose");
        assert!(
            held.z_mm.abs() < 0.01,
            "the jump limit was lost when the strength changed: {held:?}"
        );
    }

    /// Run `secs` of frames with the head held at `yaw` (a fresh measurement
    /// every frame) and gaze at `gaze`, and return the last output yaw.
    fn hold_yaw(
        p: &mut FramePipeline,
        c: &OutputConfig,
        start: Instant,
        yaw: f64,
        gaze: [f64; 2],
        secs: f64,
    ) -> f64 {
        let frames = (secs / 0.030).round() as u32;
        let mut out = f64::NAN;
        for i in 0..frames {
            let at = start + Duration::from_millis(30 * i as u64);
            let head = HeadPose {
                yaw_deg: yaw,
                z_mm: 680.0,
                ..Default::default()
            };
            out = p
                .offer(&tracked_sample(Some(gaze)), supplied(head, at), c, None, at)
                .pose
                .expect("a pose")
                .yaw_deg;
        }
        out
    }

    fn auto(on: bool) -> OutputConfig {
        let mut c = cfg(false);
        c.auto_centre.enabled = on;
        c
    }

    /// The point of auto-centre: somebody sat 10° off the tracker's axis,
    /// looking at the middle of their screen, drifts back to centre on their
    /// own — and with it off, they do not.
    #[test]
    fn auto_centre_pulls_a_constant_offset_back_to_centre() {
        let start = Instant::now();
        let mut off = FramePipeline::new(&auto(false));
        let stuck = hold_yaw(&mut off, &auto(false), start, 10.0, [0.5, 0.5], 30.0);
        assert!(
            (stuck - 10.0).abs() < 0.1,
            "premise: without it the offset stays, got {stuck}"
        );

        let mut on = FramePipeline::new(&auto(true));
        let after = hold_yaw(&mut on, &auto(true), start, 10.0, [0.5, 0.5], 30.0);
        assert!(
            after.abs() < 1.0,
            "30 s at an 8 s time constant should leave <1°, got {after}"
        );
        let r = on.rotation_reference().expect("a reference was built");
        assert!((r[0] - 10.0).abs() < 1.0, "{r:?}");
    }

    /// Looking at the edge of the screen is a look, not a posture.
    #[test]
    fn auto_centre_leaves_alone_a_user_looking_off_centre() {
        let start = Instant::now();
        let mut p = FramePipeline::new(&auto(true));
        let out = hold_yaw(&mut p, &auto(true), start, 10.0, [0.9, 0.5], 30.0);
        assert!((out - 10.0).abs() < 0.1, "got {out}");
    }

    /// And a big turn is a turn, wherever the eyes are.
    #[test]
    fn auto_centre_leaves_alone_a_turn_past_its_window() {
        let start = Instant::now();
        let c = auto(true);
        let mut p = FramePipeline::new(&c);
        let turn = c.auto_centre.window_deg + 10.0;
        let out = hold_yaw(&mut p, &c, start, turn, [0.5, 0.5], 30.0);
        assert!((out - turn).abs() < 0.1, "got {out}");
    }

    /// Curves reach the outgoing frame: a yaw deadzone wider than the head's
    /// turn sends exactly zero.
    #[test]
    fn head_curves_shape_the_outgoing_pose() {
        let start = Instant::now();
        let mut c = cfg(false);
        c.head_curves.axis_mut("yaw").unwrap().deadzone_deg = 5.0;
        let mut p = FramePipeline::new(&c);
        assert_eq!(hold_yaw(&mut p, &c, start, 4.0, [0.5, 0.5], 1.0), 0.0);
        let mut q = FramePipeline::new(&cfg(false));
        assert!(
            hold_yaw(&mut q, &cfg(false), start, 4.0, [0.5, 0.5], 1.0) > 3.9,
            "premise"
        );
    }

    /// A step in the pose leaves as a sweep at the configured speed, not a cut.
    #[test]
    fn the_output_never_moves_faster_than_its_speed_limit() {
        // Both limits set here: `cfg` switches them off, and with the movement
        // limit at infinity "a small move is not held back" held for any move.
        let c = OutputConfig {
            filter_max_rot_speed_deg_s: 60.0,
            filter_max_move_speed_mm_s: 400.0,
            ..cfg(false)
        };
        let step = Duration::from_millis(30);
        let prev = HeadPose::default();
        let next = HeadPose {
            yaw_deg: 40.0,
            x_mm: -1.0,
            ..HeadPose::default()
        };
        let out = slew_limit(Some(prev), next, &c, step.as_secs_f64());
        assert!((out.yaw_deg - 1.8).abs() < 1e-9, "{out:?}");
        assert_eq!(out.x_mm, -1.0, "a small move is not held back");
        // 400 mm/s for 30 ms is 12 mm.
        let lean = HeadPose {
            x_mm: -40.0,
            ..HeadPose::default()
        };
        let out = slew_limit(Some(prev), lean, &c, step.as_secs_f64());
        assert!((out.x_mm + 12.0).abs() < 1e-9, "a large one is: {out:?}");
        assert_eq!(slew_limit(None, next, &c, 0.03), next, "first pose passes");
    }

    /// The shipped defaults, which every other test here switches off (see
    /// `cfg`): x leaves mirrored and a fast move leaves speed-limited. Both
    /// were kept as the default for everyone when PR #3 was merged; this is
    /// what notices if a default config stops doing either.
    #[test]
    fn the_shipped_defaults_mirror_x_and_limit_speed() {
        // Through `offer`, so it is the output path being tested and not the
        // helpers it calls. Only the smoothing is taken out of the way (an EMA
        // that passes every sample whole), which leaves the curves and the
        // speed limit as the only things between the pose and what goes out.
        let c = OutputConfig {
            filter_mode: crate::games::FilterMode::Ema,
            filter_alpha: 1.0,
            ..OutputConfig::default()
        };
        let mut p = FramePipeline::new(&c);
        let sample = head_at(0.0, 0.0);
        let rest = HeadPose::default();
        let mut at = Instant::now();
        for _ in 0..5 {
            p.offer(&sample, supplied(rest, at), &c, None, at);
            at += CADENCE;
        }
        // One frame later: turned 90° and leaned 40 mm to the right.
        let jump = HeadPose {
            yaw_deg: 90.0,
            x_mm: 40.0,
            ..rest
        };
        let out = p
            .offer(&sample, supplied(jump, at), &c, None, at)
            .pose
            .expect("a pose");
        let dt = CADENCE.as_secs_f64();
        let max_turn = crate::games::DEFAULT_MAX_ROT_SPEED_DEG_S * dt;
        let max_move = crate::games::DEFAULT_MAX_MOVE_SPEED_MM_S * dt;
        assert!(
            out.yaw_deg > 0.0 && out.yaw_deg <= max_turn + 1e-9,
            "a 90° cut leaves limited to {max_turn}°: {out:?}"
        );
        assert!(out.x_mm < 0.0, "x leaves mirrored: {out:?}");
        assert!(
            out.x_mm >= -max_move - 1e-9,
            "and limited to {max_move} mm: {out:?}"
        );
    }

    /// The whole output reads zero after a recentre, Extended View included:
    /// a user whose gaze the tracker places 15° left of the screen centre while
    /// they look at it gets a yaw of zero, not 15° of Extended View on top.
    #[test]
    fn a_recentre_zeroes_the_gaze_term_too() {
        let c = cfg(true);
        let mut p = FramePipeline::new(&c);
        let start = Instant::now();
        // Gaze reported well left of the middle while the head is square.
        let mut s = head_at(0.0, 0.0);
        s.gaze_point_2d = [0.25, 0.5];
        let mut at = start;
        for _ in 0..100 {
            p.offer(&s, None, &c, Some(corners()), at);
            at += CADENCE;
        }
        let before = p
            .offer(&s, None, &c, Some(corners()), at)
            .pose
            .expect("a pose");
        assert!(
            before.yaw_deg < -5.0,
            "premise: Extended View pulls yaw: {before:?}"
        );
        p.begin_recentre(at);
        for _ in 0..WINDOW_FRAMES * 2 {
            at += CADENCE;
            p.offer(&s, None, &c, Some(corners()), at);
        }
        assert!(matches!(
            p.take_recentre(),
            Some(RecentreOutcome::Applied { .. })
        ));
        for _ in 0..200 {
            at += CADENCE;
            p.offer(&s, None, &c, Some(corners()), at);
        }
        let after = p
            .offer(&s, None, &c, Some(corners()), at)
            .pose
            .expect("a pose");
        assert!(after.yaw_deg.abs() < 0.5, "yaw should read zero: {after:?}");
        assert!(
            after.pitch_deg.abs() < 0.5,
            "pitch should read zero: {after:?}"
        );

        // And looking away from that new centre still swings the view.
        let mut right = s.clone();
        right.gaze_point_2d = [0.6, 0.5];
        for _ in 0..200 {
            at += CADENCE;
            p.offer(&right, None, &c, Some(corners()), at);
        }
        let look = p
            .offer(&right, None, &c, Some(corners()), at)
            .pose
            .expect("a pose");
        assert!(look.yaw_deg > 5.0, "Extended View still responds: {look:?}");
    }
}
