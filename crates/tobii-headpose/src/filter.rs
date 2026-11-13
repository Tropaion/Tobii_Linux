//! Smoothing for the derived head pose.
//!
//! Raw eye origins are noisy at the millimetre level, and that noise turns into
//! visible jitter once it is amplified into an in-game camera angle. Two
//! smoothers are offered, chosen by [`Smoothing`]:
//!
//! * **Adaptive** (the default) — a One-Euro filter (Casiez, Roussel & Vogel,
//!   CHI 2012). Its cutoff frequency rises with the speed of the signal, so a
//!   head held still is smoothed hard while a deliberate turn passes with
//!   little lag. A fixed EMA cannot do both: an alpha low enough to calm the
//!   camera at rest makes every turn feel late, and one high enough to follow a
//!   turn lets the millimetre noise and gaze micro-saccades straight through.
//! * **EMA** — the plain exponential moving average this module started as.
//!   Kept for anyone who preferred its feel, and because its arithmetic is
//!   simple enough for tests elsewhere to reason about exactly.
//!
//! In front of the average sits one plausibility gate: a sample whose *position*
//! has moved further in a single frame than a head can move is held rather than
//! blended, because an EMA turns a single bad sample into a swing spread over
//! the following frames instead of a spike. See [`DEFAULT_MAX_STEP_MM`] for
//! where the number comes from and [`MAX_HELD_FRAMES`] for why the hold is
//! bounded.

use crate::HeadPose;

/// Default smoothing factor: a reasonable compromise between jitter and lag at
/// the ET5's frame rate. Tune upwards for a snappier, noisier feel.
pub const DEFAULT_ALPHA: f64 = 0.25;

/// Default One-Euro cutoff at rest, in hertz.
///
/// The cutoff the adaptive filter uses when the head is still. 0.4 Hz is a time
/// constant of about 400 ms — at the ~33 Hz the tracker delivers, a per-frame
/// blend weight of roughly 0.07, more than three times calmer than
/// [`DEFAULT_ALPHA`]. Lower is steadier and slower to settle; higher lets more
/// tremor through. Down from 0.6, which in play still let a visible shimmer
/// through once the output curves added gain.
pub const DEFAULT_MIN_CUTOFF_HZ: f64 = 0.4;

/// Default One-Euro speed coefficient: how much the cutoff rises per unit of
/// speed (degrees or millimetres per second).
///
/// At 0.02 a 60°/s head turn lifts the cutoff to ~1.6 Hz (blend weight ~0.23).
/// Frame-to-frame noise in the eye origins reads as tens of degrees per second
/// of yaw, so a larger beta let the noise open the filter on its own; 0.03 did,
/// in play. A gaze jump still opens it — the output slew limit is what keeps
/// that from arriving as a cut. Raise it if fast movements feel late; lower it
/// if slow movements still shimmer.
pub const DEFAULT_BETA: f64 = 0.02;

/// The cutoff applied to the speed estimate itself, in hertz. The value the
/// One-Euro paper recommends and every implementation ships; not exposed,
/// because nothing a user would describe maps onto it.
const DERIVATIVE_CUTOFF_HZ: f64 = 1.0;

/// The frame interval assumed when a caller does not supply one: the 30.208 ms
/// median measured from `session.tobiicap` (see [`DEFAULT_MAX_STEP_MM`]).
pub const NOMINAL_DT_S: f64 = 0.030_208;

/// The longest interval one update may claim. A gap longer than this is a
/// stall, not motion, and dividing a position change by it would read as a
/// standstill; capping it keeps a resumed stream from being smoothed as though
/// a full second of stillness had passed.
const MAX_DT_S: f64 = 0.2;

/// Which smoother [`PoseFilter`] runs after its plausibility gate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Smoothing {
    /// Fixed-weight exponential moving average.
    Ema {
        /// Blend weight of each new sample, `0`–`1`.
        alpha: f64,
    },
    /// Speed-adaptive One-Euro filter.
    Adaptive {
        /// Cutoff at rest, in hertz.
        min_cutoff_hz: f64,
        /// Cutoff gain per unit of speed.
        beta: f64,
    },
}

impl Default for Smoothing {
    fn default() -> Self {
        Smoothing::Adaptive {
            min_cutoff_hz: DEFAULT_MIN_CUTOFF_HZ,
            beta: DEFAULT_BETA,
        }
    }
}

impl Smoothing {
    /// Replace anything non-finite or out of range with the default for that
    /// parameter, so a bad config value degrades rather than poisons.
    fn sanitised(self) -> Smoothing {
        match self {
            Smoothing::Ema { alpha } => Smoothing::Ema {
                alpha: if alpha.is_finite() {
                    alpha.clamp(0.0, 1.0)
                } else {
                    DEFAULT_ALPHA
                },
            },
            Smoothing::Adaptive {
                min_cutoff_hz,
                beta,
            } => Smoothing::Adaptive {
                min_cutoff_hz: if min_cutoff_hz.is_finite() && min_cutoff_hz > 0.0 {
                    min_cutoff_hz
                } else {
                    DEFAULT_MIN_CUTOFF_HZ
                },
                beta: if beta.is_finite() && beta >= 0.0 {
                    beta
                } else {
                    DEFAULT_BETA
                },
            },
        }
    }
}

/// The blend weight a first-order low-pass with `cutoff_hz` gives a sample
/// `dt_s` after the previous one.
fn lowpass_alpha(cutoff_hz: f64, dt_s: f64) -> f64 {
    let tau = 1.0 / (2.0 * std::f64::consts::PI * cutoff_hz);
    1.0 / (1.0 + tau / dt_s)
}

/// The largest per-frame step in head **position** treated as real, in
/// millimetres of straight-line distance between consecutive accepted samples.
///
/// # There is no recorded step distribution in this repository to derive it from
///
/// Both committed recordings were decoded and checked rather than assumed:
///
/// * `crates/tobii-usb/tests/captures/session.tobiicap` carries 40 gaze frames
///   and every one of them reports validity `(4, 4)` with both eye-origin
///   columns zeroed — it was recorded with nobody in front of the tracker. Its
///   distribution of per-frame steps is therefore empty, not small.
/// * `captures/calibration.tobiicap` is a calibration-blob retrieval and
///   yields no gaze frames at all.
/// * `tobii-protocol`'s committed `gaze.rs::real_frame_payload()` is likewise a
///   single no-eyes frame.
///
/// So this number is **not measured against real head motion, and is unverified
/// until a session with a head in it is recorded** (`tobii record`) and the
/// distribution read off it. What the recording does give is the quantity that
/// turns a speed into a per-frame step: the 39 intervals in `session.tobiicap`
/// are **30.208 ms** apart (median; 30.211 ms max), i.e. 33.1 Hz, which matches
/// the ~33 Hz the transport and camera modules document.
///
/// # What the number is, then
///
/// 150 mm between two frames 30.208 ms apart is **4.97 m/s** of head
/// translation — several times the fastest a seated head moves, so nothing a
/// user does reaches it. That is the intended reading: this rejects a *teleport*
/// — a midpoint that jumps a third of the way across the 400–900 mm tracking
/// volume (`tobii_config::TRACKING_FAR_MM`) in one frame, a reacquisition onto
/// a different person, a decode fault — and deliberately not the few
/// millimetres of error a stale one-eye reconstruction contributes when an eye
/// drops and returns. A limit tight enough to catch *that* would reject a
/// genuine lunge, which is the failure the wiki's Planned-Work item warns about.
///
/// It cannot go lower than 100 mm without changing crates this one does not own:
/// `tobii-output`'s `pipeline.rs` and `tobii-gtk`'s `outputs.rs` each offer a
/// synthetic 100 mm lean between two consecutive frames and assert the axis
/// moves, so a limit below that would have those tests asserting on a held
/// value. 150 mm is the smallest round figure clear of them.
///
/// # Why position only, and no angular limit at all
///
/// The pose reaching this filter is the **composed** one — `fusion::compose`
/// has already added the Extended View term to yaw and pitch — and that term is
/// gaze-driven, so it moves at saccade speed rather than head speed. Measured
/// against the 1193 × 336 mm panel the defaults were tuned for: gaze at the
/// left edge gives `ev_yaw = -45.00°` and at the right edge `+45.00°`, so a
/// single-frame look across the screen legitimately swings composed yaw by 90°
/// (and pitch by 50°). Any angular gate tight enough to catch a rotation glitch
/// would reject that, so there is none. A bad frame that moves position is
/// still rejected whole, angles included.
pub const DEFAULT_MAX_STEP_MM: f64 = 150.0;

/// How many consecutive samples may be rejected before one is accepted anyway.
///
/// Without a bound, reject-and-hold locks up: once the head really is somewhere
/// else — reacquired across a dropout shorter than `pipeline.rs`'s one-second
/// `TRACKING_LOSS_RESET`, during which the user moved — every later sample is
/// just as far from the held value, so the filter would reject the world
/// forever and never recover on its own.
///
/// Three frames is 91 ms at the measured 30.208 ms interval: long enough to
/// swallow the one- or two-frame transition at the edge of an eye dropout,
/// which is what this gate exists for, and short enough that a genuine teleport
/// costs under a tenth of a second before the average starts tracking it. The
/// count is a chosen bound, not a measured one — nothing in the repository
/// records how long a glitch lasts.
pub const MAX_HELD_FRAMES: u32 = 3;

/// Straight-line distance between two poses' positions, in millimetres.
fn step_mm(a: HeadPose, b: HeadPose) -> f64 {
    let dx = a.x_mm - b.x_mm;
    let dy = a.y_mm - b.y_mm;
    let dz = a.z_mm - b.z_mm;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// A smoothing filter over [`HeadPose`]: a plausibility gate, then the
/// [`Smoothing`] chosen at construction.
///
/// Each update blends the new sample into the running state with weight
/// `alpha`: `out = alpha * new + (1 - alpha) * previous`. For the EMA `alpha`
/// is fixed — `1.0` is a pass-through, and smaller values smooth harder at the
/// cost of lag. For the adaptive filter it is recomputed per channel on every
/// update from that channel's smoothed speed, which is what lets it be calm and
/// quick at once.
///
/// The filter never emits a non-finite value: a non-finite input is discarded
/// (the previous output is repeated) so one bad sample cannot poison the state.
///
/// A finite sample whose position has jumped further than
/// [`max_step_mm`](Self::max_step_mm) from the **last accepted raw sample** is
/// held the same way, and the last raw value is held with it — comparing
/// against the smoothed output instead would let a glitch drag the reference
/// after it, one blend at a time, until the view had swept there anyway. At
/// most [`MAX_HELD_FRAMES`] samples in a row are refused; see there for why the
/// hold has to end.
///
/// Angles are blended componentwise, with no shortest-arc wraparound handling.
/// That is safe here because yaw and roll come from `atan2` on an interocular
/// vector that stays well away from ±180° for any pose a seated user can hold;
/// it would need revisiting if these angles were ever sourced from elsewhere.
#[derive(Debug, Clone)]
pub struct PoseFilter {
    smoothing: Smoothing,
    max_step_mm: f64,
    /// The last accepted sample and what it produced, or `None` before the
    /// first one.
    last: Option<Accepted>,
    /// Consecutive samples refused by the gate, reset by every acceptance.
    held: u32,
}

/// The last accepted sample and the output it produced.
///
/// Both or neither: an accepted sample always produces an output, and `reset`
/// drops the pair. Two separate `Option`s would let the gate's hold arm read
/// as though it might have a reference with no output to return, which is a
/// state nothing can reach.
#[derive(Debug, Clone, Copy)]
struct Accepted {
    /// As it arrived. The gate measures against this rather than against
    /// `out`, which lags it by design — comparing against the smoothed output
    /// would let a glitch drag the reference after it, one blend at a time.
    raw: HeadPose,
    /// What the blend produced from it, and what the gate repeats while it
    /// holds.
    out: HeadPose,
    /// The adaptive filter's smoothed speed per channel (x, y, z, yaw, pitch,
    /// roll), in units per second. Unused by the EMA.
    speed: [f64; 6],
}

impl Default for PoseFilter {
    fn default() -> Self {
        Self::new(DEFAULT_ALPHA)
    }
}

impl PoseFilter {
    /// Build a filter with the given smoothing factor. `alpha` is clamped to
    /// `[0, 1]`; a non-finite `alpha` falls back to [`DEFAULT_ALPHA`].
    pub fn new(alpha: f64) -> Self {
        Self::with_max_step_mm(alpha, DEFAULT_MAX_STEP_MM)
    }

    /// As [`new`](Self::new), with an explicit per-frame position limit.
    ///
    /// `f64::INFINITY` turns the gate off and leaves a plain EMA, which is what
    /// a caller testing the blend itself wants. A limit that is NaN or not
    /// positive would refuse everything, so it falls back to
    /// [`DEFAULT_MAX_STEP_MM`] the same way a bad `alpha` does.
    pub fn with_max_step_mm(alpha: f64, max_step_mm: f64) -> Self {
        Self::with_smoothing(Smoothing::Ema { alpha }, max_step_mm)
    }

    /// A filter running `smoothing` behind the plausibility gate.
    ///
    /// Out-of-range parameters fall back to their defaults one by one, the same
    /// way [`with_max_step_mm`](Self::with_max_step_mm) treats a bad `alpha`.
    pub fn with_smoothing(smoothing: Smoothing, max_step_mm: f64) -> Self {
        let max_step_mm = if max_step_mm > 0.0 {
            max_step_mm
        } else {
            DEFAULT_MAX_STEP_MM
        };
        Self {
            smoothing: smoothing.sanitised(),
            max_step_mm,
            last: None,
            held: 0,
        }
    }

    /// The smoother actually in use (after sanitising).
    pub fn smoothing(&self) -> Smoothing {
        self.smoothing
    }

    /// The EMA's smoothing factor actually in use (after clamping). For the
    /// adaptive filter this is the blend weight it applies at rest, at the
    /// nominal frame interval — the number comparable to an EMA's alpha.
    pub fn alpha(&self) -> f64 {
        match self.smoothing {
            Smoothing::Ema { alpha } => alpha,
            Smoothing::Adaptive { min_cutoff_hz, .. } => lowpass_alpha(min_cutoff_hz, NOMINAL_DT_S),
        }
    }

    /// The per-frame position limit actually in use.
    pub fn max_step_mm(&self) -> f64 {
        self.max_step_mm
    }

    /// How many samples in a row the gate has just refused. Zero whenever the
    /// last finite sample was accepted, so it doubles as "is the gate holding".
    pub fn held_frames(&self) -> u32 {
        self.held
    }

    /// The most recent output, or `None` before the first update.
    pub fn current(&self) -> Option<HeadPose> {
        self.last.map(|a| a.out)
    }

    /// Forget the running state, so the next update seeds the filter afresh.
    /// Used when tracking is lost, to avoid dragging a stale pose back in when
    /// the user returns to the trackbox somewhere else entirely.
    ///
    /// This drops the gate's reference too. Whoever comes back is allowed to be
    /// somewhere else entirely — that is the whole reason the caller reset —
    /// so measuring their first sample against where the last person sat would
    /// refuse the reacquisition for no reason.
    pub fn reset(&mut self) {
        self.last = None;
        self.held = 0;
    }

    /// Blend `p` into the running average and return the smoothed pose.
    ///
    /// The first update after construction or [`reset`](Self::reset) adopts `p`
    /// outright rather than ramping up from zero, which would otherwise fling
    /// the pose in from the tracker's origin over the first second.
    ///
    /// Assumes the nominal frame interval; [`update_dt`](Self::update_dt) is the
    /// form for callers that know the real one, which the adaptive filter uses
    /// to turn a change into a speed.
    pub fn update(&mut self, p: HeadPose) -> HeadPose {
        self.update_dt(p, NOMINAL_DT_S)
    }

    /// As [`update`](Self::update), `dt_s` seconds after the previous sample.
    ///
    /// The interval is clamped to a sane range: a zero or negative one (two
    /// samples stamped alike) would divide by zero, and a long one is a stall
    /// rather than a slow movement — see `MAX_DT_S`.
    pub fn update_dt(&mut self, p: HeadPose, dt_s: f64) -> HeadPose {
        let dt = if dt_s.is_finite() {
            dt_s.clamp(0.001, MAX_DT_S)
        } else {
            NOMINAL_DT_S
        };
        if !p.is_finite() {
            // Repeat the last good output; if there is none yet, stay neutral.
            // Deliberately ahead of the gate and deliberately not counted as a
            // rejection: a NaN carries no position to measure a step from, and
            // spending the hold budget on one would let a run of them open the
            // gate for whatever arrived next.
            return self.last.map_or_else(HeadPose::default, |a| a.out);
        }
        let (out, speed) = match self.last {
            // There is nothing to measure against before the first accepted
            // sample, so it seeds the filter unconditionally.
            None => (p, [0.0; 6]),
            Some(last) => {
                // The gate.
                if step_mm(p, last.raw) > self.max_step_mm && self.held < MAX_HELD_FRAMES {
                    self.held += 1;
                    return last.out;
                }
                let new = channels(p);
                let old = channels(last.out);
                let mut out = [0.0; 6];
                let mut speed = last.speed;
                match self.smoothing {
                    Smoothing::Ema { alpha: a } => {
                        for i in 0..6 {
                            out[i] = a * new[i] + (1.0 - a) * old[i];
                        }
                    }
                    Smoothing::Adaptive {
                        min_cutoff_hz,
                        beta,
                    } => {
                        let a_d = lowpass_alpha(DERIVATIVE_CUTOFF_HZ, dt);
                        for i in 0..6 {
                            let raw_speed = (new[i] - old[i]) / dt;
                            speed[i] = a_d * raw_speed + (1.0 - a_d) * last.speed[i];
                            let cutoff = min_cutoff_hz + beta * speed[i].abs();
                            let a = lowpass_alpha(cutoff, dt);
                            out[i] = a * new[i] + (1.0 - a) * old[i];
                        }
                    }
                }
                (from_channels(out), speed)
            }
        };
        self.held = 0;
        self.last = Some(Accepted { raw: p, out, speed });
        out
    }
}

/// The six channels in a fixed order, so both smoothers can loop over them.
fn channels(p: HeadPose) -> [f64; 6] {
    [p.x_mm, p.y_mm, p.z_mm, p.yaw_deg, p.pitch_deg, p.roll_deg]
}

fn from_channels(c: [f64; 6]) -> HeadPose {
    HeadPose {
        x_mm: c[0],
        y_mm: c[1],
        z_mm: c[2],
        yaw_deg: c[3],
        pitch_deg: c[4],
        roll_deg: c[5],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(x: f64, yaw: f64) -> HeadPose {
        HeadPose {
            x_mm: x,
            y_mm: 10.0,
            z_mm: 680.0,
            yaw_deg: yaw,
            pitch_deg: 0.0,
            roll_deg: -3.0,
        }
    }

    #[test]
    fn first_update_passes_the_sample_through() {
        let mut f = PoseFilter::new(0.1);
        let p = pose(100.0, 20.0);
        assert_eq!(f.update(p), p);
    }

    /// Blending a value with itself is a fixed point mathematically but can
    /// land a unit in the last place away in floating point, so compare with a
    /// tolerance rather than for bit equality.
    fn assert_pose_close(actual: HeadPose, expected: HeadPose) {
        let fields = [
            ("x", actual.x_mm, expected.x_mm),
            ("y", actual.y_mm, expected.y_mm),
            ("z", actual.z_mm, expected.z_mm),
            ("yaw", actual.yaw_deg, expected.yaw_deg),
            ("pitch", actual.pitch_deg, expected.pitch_deg),
            ("roll", actual.roll_deg, expected.roll_deg),
        ];
        for (name, a, e) in fields {
            assert!((a - e).abs() < 1e-9, "{name}: expected {e}, got {a}");
        }
    }

    #[test]
    fn constant_input_is_a_fixed_point() {
        let mut f = PoseFilter::new(0.3);
        let p = pose(42.0, -12.0);
        for _ in 0..500 {
            // A tolerance, not bit equality — but a fixed tolerance across 500
            // iterations still proves the value does not creep.
            assert_pose_close(f.update(p), p);
        }
    }

    #[test]
    fn a_step_converges_towards_the_new_value() {
        let mut f = PoseFilter::new(0.5);
        f.update(pose(0.0, 0.0));
        let target = pose(100.0, 30.0);

        // Every step must strictly close the gap. Bounded to 20 iterations so
        // the remaining error (100 × 0.5^20 ≈ 1e-4) stays well clear of the
        // point where it would round to exactly zero and stop shrinking.
        let mut previous_error = f64::INFINITY;
        for _ in 0..20 {
            let out = f.update(target);
            let error = (target.x_mm - out.x_mm).abs();
            assert!(
                error < previous_error,
                "error {error} did not improve on {previous_error}"
            );
            previous_error = error;
        }

        // Left running, it settles on the target.
        for _ in 0..60 {
            f.update(target);
        }
        assert_pose_close(f.current().expect("state after updates"), target);
    }

    #[test]
    fn a_step_never_overshoots_the_target() {
        let mut f = PoseFilter::new(0.4);
        f.update(pose(0.0, 0.0));
        for _ in 0..50 {
            let out = f.update(pose(100.0, 0.0));
            assert!((0.0..=100.0).contains(&out.x_mm), "x={}", out.x_mm);
        }
    }

    #[test]
    fn alpha_one_is_a_pass_through_and_alpha_zero_holds_the_first_sample() {
        let mut snappy = PoseFilter::new(1.0);
        snappy.update(pose(0.0, 0.0));
        assert_eq!(snappy.update(pose(50.0, 5.0)), pose(50.0, 5.0));

        let mut frozen = PoseFilter::new(0.0);
        frozen.update(pose(7.0, 1.0));
        assert_eq!(frozen.update(pose(999.0, 90.0)), pose(7.0, 1.0));
    }

    #[test]
    fn alpha_is_clamped_and_non_finite_alpha_falls_back_to_the_default() {
        assert_eq!(PoseFilter::new(-5.0).alpha(), 0.0);
        assert_eq!(PoseFilter::new(5.0).alpha(), 1.0);
        assert_eq!(PoseFilter::new(f64::NAN).alpha(), DEFAULT_ALPHA);
        assert_eq!(PoseFilter::new(f64::INFINITY).alpha(), DEFAULT_ALPHA);
        assert_eq!(PoseFilter::default().alpha(), DEFAULT_ALPHA);
    }

    #[test]
    fn output_is_never_non_finite_even_for_hostile_input() {
        let hostile = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY];
        let mut f = PoseFilter::new(0.3);
        // Before any good sample: still finite.
        for &bad in &hostile {
            assert!(f.update(pose(bad, 0.0)).is_finite());
        }
        // And a poisoned sample must not corrupt an established state.
        let good = pose(50.0, 10.0);
        f.update(good);
        for &bad in &hostile {
            assert_eq!(f.update(pose(0.0, bad)), good, "bad sample must be ignored");
        }
        assert_pose_close(f.update(good), good);
    }

    /// Also the gate's reset: `reset` has to drop the accepted sample, or this
    /// 500 mm reacquisition is measured against where the previous user sat
    /// and refused, and the filter answers with a default-constructed pose.
    #[test]
    fn reset_makes_the_next_sample_seed_the_filter_again() {
        let mut f = PoseFilter::new(0.1);
        f.update(pose(0.0, 0.0));
        assert!(f.current().is_some());
        f.reset();
        assert!(f.current().is_none());
        let p = pose(500.0, 45.0);
        assert_eq!(f.update(p), p, "after reset the next sample is adopted");
    }

    /// The behaviour the gate exists for: one implausible sample must not reach
    /// the average at all.
    ///
    /// The second filter is the premise, not decoration. Asserting that the
    /// gated output held would pass just as well if `update` had stopped doing
    /// anything, so the same sequence runs through a filter with the gate off
    /// to show that an EMA really does move here — that is the swing being
    /// prevented.
    #[test]
    fn a_teleport_is_held_instead_of_being_blended_in() {
        let steady = pose(0.0, 0.0);
        let glitch = pose(500.0, 0.0);

        let mut gated = PoseFilter::new(0.25);
        let settled = gated.update(steady);
        assert_eq!(gated.update(glitch), settled, "the glitch must not land");
        assert_eq!(gated.held_frames(), 1);

        let mut ungated = PoseFilter::with_max_step_mm(0.25, f64::INFINITY);
        ungated.update(steady);
        let swung = ungated.update(glitch);
        assert!(
            swung.x_mm > 100.0,
            "premise: a plain EMA blends 25% of a 500 mm jump straight in, \
             got {}",
            swung.x_mm
        );
    }

    /// Holding the *raw* reference is what stops a glitch sweeping the view.
    ///
    /// If the refused sample became the new reference, the head's real position
    /// would then look like the jump — so the next honest sample would be
    /// refused in turn, and the gate would walk the view out to the glitch one
    /// rejection at a time instead of ignoring it.
    #[test]
    fn a_refused_sample_does_not_become_the_new_reference() {
        let mut f = PoseFilter::new(0.5);
        f.update(pose(0.0, 0.0));
        f.update(pose(500.0, 0.0));
        assert_eq!(f.held_frames(), 1, "premise: that sample was refused");

        // Back where the head actually is. Measured against the held raw value
        // this is a zero step and is accepted at once.
        f.update(pose(1.0, 0.0));
        assert_eq!(
            f.held_frames(),
            0,
            "the sample after a glitch is measured against the last accepted \
             raw value, not against the glitch"
        );
        assert!(
            f.current().expect("state").x_mm < 1.0,
            "and the output never went near the glitch"
        );
    }

    /// The risk the wiki's Planned-Work item names: too tight a threshold
    /// rejects a genuine fast turn. A real turn spans many frames, so every
    /// frame's step stays well under a per-frame limit.
    ///
    /// Ramped here at 3°/frame of yaw and 30 mm/frame of position — 99°/s and
    /// 1.0 m/s at the 30.208 ms frame interval measured from
    /// `session.tobiicap`, which is a brisk head turn with a lean in it, held
    /// for 30 frames (0.9 s). Not one sample may be refused.
    #[test]
    fn a_fast_head_turn_ramps_through_without_a_rejection() {
        let mut f = PoseFilter::new(0.25);
        for i in 0..30 {
            let p = pose(i as f64 * 30.0, i as f64 * 3.0);
            f.update(p);
            assert_eq!(
                f.held_frames(),
                0,
                "frame {i} of a genuine turn was rejected: {p:?}"
            );
        }
        // And the filter is actually following it, rather than passing the
        // test by having stopped.
        assert!(f.current().expect("state").yaw_deg > 60.0);
    }

    /// The gate measures against the last accepted **raw** sample, never
    /// against the smoothed output — which lags it by design, so using the
    /// output would make the effective limit `max_step_mm - lag`: the faster
    /// the head was already moving, the tighter the gate, which is exactly the
    /// genuine-fast-motion rejection [`DEFAULT_MAX_STEP_MM`] is written to
    /// avoid.
    ///
    /// The lag here is built out of motion this crate already calls plausible —
    /// the same 30 mm/frame (1.0 m/s) ramp as
    /// `a_fast_head_turn_ramps_through_without_a_rejection` — rather than out
    /// of a step no head can make, so the test measures the reference and not
    /// an absurdity.
    #[test]
    fn a_lagging_output_does_not_tighten_the_gate() {
        let mut f = PoseFilter::new(0.25);
        // A sustained ramp leaves the average step * (1 - alpha) / alpha behind
        // the raw samples: 90 mm at 30 mm/frame and alpha 0.25.
        for i in 0..20 {
            f.update(pose(i as f64 * 30.0, 0.0));
        }
        let last_raw_x = 19.0 * 30.0;
        let state_x = f.current().expect("state").x_mm;
        assert!(
            last_raw_x - state_x > 80.0,
            "premise: the average lags the raw samples, by {}",
            last_raw_x - state_x
        );

        // One honest 100 mm lean on top of the ramp: 100 mm from the last
        // accepted raw sample, but ~190 mm from the smoothed output.
        let out = f.update(pose(last_raw_x + 100.0, 0.0));
        assert_eq!(
            f.held_frames(),
            0,
            "an honest 100 mm step must not be refused"
        );
        assert!(
            out.x_mm > state_x,
            "and the average must follow it: {}",
            out.x_mm
        );
    }

    /// The hold has to end. Once the head really is somewhere else, every
    /// later sample is just as far from the held value as the first was, so an
    /// unbounded hold would refuse the world permanently.
    #[test]
    fn a_move_that_persists_is_adopted_once_the_hold_expires() {
        let mut f = PoseFilter::new(0.5);
        let held = f.update(pose(0.0, 0.0));
        let elsewhere = pose(500.0, 0.0);

        for i in 1..=MAX_HELD_FRAMES {
            assert_eq!(f.update(elsewhere), held, "sample {i} should be held");
            assert_eq!(f.held_frames(), i);
        }

        let resumed = f.update(elsewhere);
        assert_eq!(f.held_frames(), 0, "the hold must expire");
        assert!(
            resumed.x_mm > held.x_mm,
            "after the hold the average tracks the head again: {} vs {}",
            resumed.x_mm,
            held.x_mm
        );
    }

    /// Everything inside the limit is the plain EMA it always was, arithmetic
    /// included — the gate must be invisible on ordinary motion.
    #[test]
    fn steps_inside_the_limit_are_the_plain_ema_they_always_were() {
        let mut f = PoseFilter::new(0.5);
        f.update(pose(0.0, 0.0));
        // 100 mm is a large step (3.3 m/s) and still under the limit, so both
        // of these are ordinary blends: 0 -> 50 -> 75.
        assert_pose_close(f.update(pose(100.0, 20.0)), pose(50.0, 10.0));
        assert_pose_close(f.update(pose(100.0, 20.0)), pose(75.0, 15.0));
        assert_eq!(f.held_frames(), 0);
    }

    /// A non-finite sample is not a rejection, and must not spend the budget
    /// that bounds one — otherwise a run of NaNs opens the gate for whatever
    /// arrives next.
    #[test]
    fn a_non_finite_sample_does_not_spend_the_hold_budget() {
        let mut f = PoseFilter::new(0.5);
        let held = f.update(pose(0.0, 0.0));
        f.update(pose(500.0, 0.0));
        assert_eq!(f.held_frames(), 1);

        assert_eq!(f.update(pose(f64::NAN, 0.0)), held);
        assert_eq!(
            f.held_frames(),
            1,
            "a NaN carries no position, so it is neither accepted nor counted"
        );
    }

    #[test]
    fn a_nonsensical_limit_falls_back_to_the_default() {
        assert_eq!(
            PoseFilter::with_max_step_mm(0.5, f64::NAN).max_step_mm(),
            DEFAULT_MAX_STEP_MM
        );
        assert_eq!(
            PoseFilter::with_max_step_mm(0.5, 0.0).max_step_mm(),
            DEFAULT_MAX_STEP_MM
        );
        assert_eq!(
            PoseFilter::with_max_step_mm(0.5, -1.0).max_step_mm(),
            DEFAULT_MAX_STEP_MM
        );
        assert_eq!(PoseFilter::new(0.5).max_step_mm(), DEFAULT_MAX_STEP_MM);
        assert_eq!(
            PoseFilter::with_max_step_mm(0.5, f64::INFINITY).max_step_mm(),
            f64::INFINITY,
            "infinity is how a caller asks for a plain EMA"
        );
    }

    fn adaptive() -> PoseFilter {
        PoseFilter::with_smoothing(Smoothing::default(), DEFAULT_MAX_STEP_MM)
    }

    /// A deterministic ±amp alternating wobble: the worst case for jitter,
    /// since every frame reverses direction.
    fn wobble(i: usize, amp: f64) -> f64 {
        if i.is_multiple_of(2) {
            amp
        } else {
            -amp
        }
    }

    /// The reason the adaptive filter is the default: holding still, it lets
    /// through markedly less tremor than the old fixed EMA did.
    #[test]
    fn the_adaptive_filter_is_calmer_than_the_ema_at_rest() {
        let mut ema = PoseFilter::new(DEFAULT_ALPHA);
        let mut ad = adaptive();
        let (mut ema_peak, mut ad_peak) = (0.0_f64, 0.0_f64);
        for i in 0..200 {
            let p = pose(wobble(i, 2.0), wobble(i, 1.0));
            let e = ema.update(p);
            let a = ad.update(p);
            if i > 50 {
                ema_peak = ema_peak.max(e.yaw_deg.abs());
                ad_peak = ad_peak.max(a.yaw_deg.abs());
            }
        }
        assert!(
            ad_peak < ema_peak * 0.6,
            "adaptive residual jitter {ad_peak:.3}° should be well under the EMA's {ema_peak:.3}°"
        );
    }

    /// And the other half of the trade: a real turn is followed with less lag
    /// than the EMA gives, not more.
    #[test]
    fn the_adaptive_filter_follows_a_turn_faster_than_the_ema() {
        let mut ema = PoseFilter::new(DEFAULT_ALPHA);
        let mut ad = adaptive();
        ema.update(pose(0.0, 0.0));
        ad.update(pose(0.0, 0.0));
        // 90°/s for half a second.
        let (mut e, mut a) = (0.0, 0.0);
        for i in 1..=16 {
            let p = pose(0.0, i as f64 * 90.0 * NOMINAL_DT_S);
            e = ema.update(p).yaw_deg;
            a = ad.update(p).yaw_deg;
        }
        let target = 16.0 * 90.0 * NOMINAL_DT_S;
        assert!(
            target - a < target - e,
            "adaptive lag {:.2}° should be smaller than the EMA's {:.2}°",
            target - a,
            target - e
        );
    }

    #[test]
    fn the_adaptive_filter_settles_on_a_step_without_overshoot() {
        let mut f = adaptive();
        f.update(pose(0.0, 0.0));
        let mut out = pose(0.0, 0.0);
        for _ in 0..300 {
            out = f.update(pose(50.0, 20.0));
            assert!(out.x_mm <= 50.0 + 1e-9 && out.yaw_deg <= 20.0 + 1e-9);
        }
        assert!((out.x_mm - 50.0).abs() < 0.01 && (out.yaw_deg - 20.0).abs() < 0.01);
    }

    #[test]
    fn the_adaptive_filter_tolerates_hostile_intervals_and_parameters() {
        let mut f = PoseFilter::with_smoothing(
            Smoothing::Adaptive {
                min_cutoff_hz: f64::NAN,
                beta: -1.0,
            },
            DEFAULT_MAX_STEP_MM,
        );
        assert_eq!(f.smoothing(), Smoothing::default());
        f.update_dt(pose(0.0, 0.0), 0.0);
        for dt in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e9] {
            assert!(f.update_dt(pose(10.0, 5.0), dt).is_finite(), "dt={dt}");
        }
    }
}
