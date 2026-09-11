//! Head pose derived from the ET5's two eye origins, for feeding opentrack.
//!
//! # Coordinate conventions
//!
//! Tracker space is millimetres with the origin at the tracker's IR sensor
//! array (the same space as [`GazeSample::eye_origin_l_mm`]):
//!
//! * **+x** — to the user's right, as seen by the tracker. The user's right eye
//!   therefore sits at a larger x than their left eye.
//! * **+y** — up.
//! * **+z** — away from the tracker, towards the user. An eye origin with
//!   `z ≈ 680` means the user's head is ~680 mm in front of the tracker.
//!
//! # Two paths: neural (6-DOF) and geometric fallback (5-DOF)
//!
//! Tobii computes head pose **host-side**, with an OpenVINO neural model run on
//! the two NIR camera images (there is no head-pose stream on the wire — see the
//! `Head-Pose` wiki page). The full pipeline mirrors that: camera frames →
//! [`model::PoseModel`] → 6-DOF. Backends for Tobii's model and the open ONNX
//! models plug in behind that trait; [`preprocess`] holds the shared face-crop
//! and tensor conversion.
//!
//! This function is the **geometric fallback** used when no model is configured:
//! it reconstructs what the two eye origins alone can support —
//!
//! * **position** — the midpoint of the two eye origins.
//! * **yaw** — the interocular vector's angle in the horizontal (x–z) plane.
//! * **roll** — the interocular vector's tilt in the frontal (x–y) plane.
//! * **pitch** — **NOT DERIVABLE.** Two eyes give a single line through the
//!   head; nodding rotates the head *about* that line, which leaves both eye
//!   origins essentially where they were. There is no vertical reference (nose,
//!   chin, forehead) in the data, so pitch cannot be recovered at all.
//!   [`pose_from_eyes`] always reports `pitch_deg = 0.0`. This is a known
//!   limitation to be filled in once the device's real head-pose stream is
//!   reverse-engineered from a USB capture; until then, opentrack will see a
//!   permanently level head.
//!
//! # One eye is not no eyes
//!
//! [`pose_from_sample`] needs both eye origins and gives nothing without them.
//! That is a strict gate on a device that loses the two eyes *independently*:
//! this project measured about five per-eye dropouts a second, with one eye
//! gone far more often than both. [`PairOffset`] is the stateful path beside
//! it — it keeps the last measured offset between the eyes, reconstructs the
//! missing one from the surviving one, and ages the offset out so a guess can
//! never be handed out as a measurement. With both eyes present it returns
//! exactly what [`pose_from_sample`] returns.
//!
//! # Rotation sign conventions — UNVERIFIED ASSUMPTIONS
//!
//! * **yaw > 0** — the user turns their head to *their* right (nose swings
//!   towards +x).
//! * **roll > 0** — the user tilts their head to *their* right (right ear
//!   towards the right shoulder, so the right eye drops below the left).
//!
//! Both of these are **assumptions that have not been validated against real
//! hardware or against opentrack**, and neither has the assumed handedness of
//! the tracker's x axis on which they rest. If in-game head movement comes out
//! mirrored, the fix is to negate the offending angle in [`pose_from_eyes`] —
//! that is the only place either sign is decided. See also
//! [`opentrack::TRANSLATION_SCALE`] for the matching unit caveat on position.

pub mod filter;
pub mod model;
pub mod model_store;
/// The neural 6DOF backend. Behind a feature because it is the only thing in
/// this crate that costs a dependency tree — see the crate's `Cargo.toml`.
#[cfg(feature = "onnx")]
pub mod onnx;
pub mod opentrack;
pub mod preprocess;
/// Re-exported from `tobii-config`, where it moved once the updater needed it
/// too. Kept here so `tobii_headpose::sha256` still resolves.
pub use tobii_config::sha256;

pub use filter::PoseFilter;
pub use model::{ModelConfig, ModelKind, PoseModel};
pub use opentrack::to_opentrack_datagram;

use std::time::{Duration, Instant};

use tobii_protocol::gaze::{present, GazeSample};

/// The pitch zero to apply, and the 10-90% spread, from a run's samples.
///
/// Shared by the GUI and the CLI, which each measure the same thing and were
/// each deciding independently how to reduce it.
///
/// Median rather than mean: the frames before the crop converges are outliers,
/// and a handful of them must not move the answer. The spread comes back so a
/// run where the user moved shows up as a number instead of a quietly wrong
/// zero. Non-finite samples are dropped rather than sorted — a degenerate
/// quaternion normalises to NaN, `partial_cmp` returns `None` for it, and an
/// `expect` there would panic whichever thread is measuring.
pub fn pitch_offset_from(samples: &mut Vec<f64>) -> Option<(f64, f64)> {
    samples.retain(|v| v.is_finite());
    if samples.len() < 20 {
        return None;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = samples[samples.len() / 2];
    let spread = samples[samples.len() * 9 / 10] - samples[samples.len() / 10];
    Some((-median, spread))
}

/// Validity value meaning "this eye is tracked". Anything else (in practice 4,
/// "not detected") means the eye's origin column is meaningless.
const VALIDITY_TRACKED: u32 = 0;

/// A head pose: position in tracker-space millimetres, orientation in degrees.
///
/// `pitch_deg` is always `0.0` — see the [module docs](self).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HeadPose {
    pub x_mm: f64,
    pub y_mm: f64,
    pub z_mm: f64,
    pub yaw_deg: f64,
    pub pitch_deg: f64,
    pub roll_deg: f64,
}

impl HeadPose {
    /// True if every field is finite (no NaN, no infinity).
    pub fn is_finite(&self) -> bool {
        self.x_mm.is_finite()
            && self.y_mm.is_finite()
            && self.z_mm.is_finite()
            && self.yaw_deg.is_finite()
            && self.pitch_deg.is_finite()
            && self.roll_deg.is_finite()
    }
}

/// Derive a head pose from the two eye origins, in tracker-space millimetres.
///
/// Position is the midpoint. Yaw is the interocular vector projected onto the
/// horizontal x–z plane; roll is the same vector projected onto the frontal
/// x–y plane. Because these are independent projections, a pure roll recovers
/// `yaw ≈ 0` and a pure yaw recovers `roll ≈ 0`.
///
/// Degenerate input (both origins identical) yields zero angles rather than
/// NaN, since `atan2(0.0, 0.0)` is defined as `0.0`.
pub fn pose_from_eyes(left_mm: [f64; 3], right_mm: [f64; 3]) -> HeadPose {
    // Interocular vector, pointing from the left eye to the right eye. With a
    // square-on head this is roughly (+interocular_distance, 0, 0).
    let vx = right_mm[0] - left_mm[0];
    let vy = right_mm[1] - left_mm[1];
    let vz = right_mm[2] - left_mm[2];

    // Yaw: turning to the user's right swings the right eye away from the
    // tracker (+z) and the left eye towards it, so vz > 0 for a right turn.
    let yaw_deg = vz.atan2(vx).to_degrees();

    // Roll: tilting to the user's right drops the right eye, so vy < 0 for a
    // right tilt. Negate so that "tilt right" reads positive, matching the
    // usual aviation sense that opentrack profiles expect.
    let roll_deg = (-vy).atan2(vx).to_degrees();

    HeadPose {
        x_mm: (left_mm[0] + right_mm[0]) / 2.0,
        y_mm: (left_mm[1] + right_mm[1]) / 2.0,
        z_mm: (left_mm[2] + right_mm[2]) / 2.0,
        yaw_deg,
        // Not derivable from two points — see the module docs.
        pitch_deg: 0.0,
        roll_deg,
    }
}

/// Derive a head pose from a decoded gaze sample, or `None` if the sample does
/// not carry two tracked eyes.
///
/// The gate is deliberately strict: **both** eyes must report `validity == 0`
/// *and* have their origin columns present. Checking the present bits alone is
/// not enough — the device sends the eye-origin columns on every frame and
/// simply zeroes them when no eye is detected, so a present-bit-only check
/// reports a head sitting exactly on the tracker's sensor. This is pinned by a
/// captured-frame regression test in `tobii-protocol`'s `gaze` module.
pub fn pose_from_sample(s: &GazeSample) -> Option<HeadPose> {
    let present_ok = s.has(present::EYE_ORIGIN_L)
        && s.has(present::EYE_ORIGIN_R)
        && s.has(present::VALIDITY_L)
        && s.has(present::VALIDITY_R);
    let tracked = s.validity_l == VALIDITY_TRACKED && s.validity_r == VALIDITY_TRACKED;
    if !present_ok || !tracked {
        return None;
    }
    Some(pose_from_eyes(s.eye_origin_l_mm, s.eye_origin_r_mm))
}

/// Where a geometric pose came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoseSource {
    /// Both eye origins were measured on this frame.
    BothEyes,
    /// One eye was measured and the other placed with the last measured
    /// offset. Translation follows the eye that is really there; yaw and roll
    /// are the ones the last two-eye frame measured — see [`PairOffset`].
    Reconstructed,
}

/// A pose together with the answer to "was this measured, or partly guessed".
///
/// The two travel together because a consumer that reports the pose has to be
/// able to report that as well: the whole risk of the one-eye path is a
/// reconstruction that looks exactly like a measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourcedPose {
    pub pose: HeadPose,
    pub source: PoseSource,
}

/// Consecutive one-eye samples required before the fallback engages.
///
/// The tracker loses the two eyes independently and briefly, so the eye count
/// does not step from 2 to 1 and back — it flickers. Acting on the first
/// one-eye sample would switch between measured and reconstructed geometry
/// frame by frame, and the two do not agree exactly (one re-measures the
/// interocular vector, the other holds it), so the switching itself becomes a
/// small oscillation in the pose the filter then has to swallow.
///
/// Two means one isolated invalid frame never changes the path. **What it
/// costs is one frame — about 30 ms — at the start of every real outage**,
/// during which there is no pose. That frame is exactly what ships today, so
/// the debounce gives nothing away; it only declines to win the first 30 ms
/// back.
pub const ONE_EYE_DEBOUNCE: u32 = 2;

/// How long a measured interocular offset may go on standing in for a lost eye.
///
/// Ageing is the whole discipline of this path. A reconstructed pose is a
/// guess, and the guess is specifically a **rigid-body** one: the vector from
/// one eye to the other is constant only while the head does not rotate. Since
/// [`pose_from_eyes`] reads yaw and roll off precisely that vector, a
/// reconstructed frame reports the rotation that the last two-eye frame
/// measured, and only translation keeps following the eye that is really
/// there. So the bound answers one question — how long may a held rotation be
/// handed out as a measured one.
///
/// 300 ms, which is about 10 frames at the **30.208 ms** cadence measured
/// between consecutive gaze frames in
/// `crates/tobii-usb/tests/captures/session.tobiicap`. Two things fix it:
///
/// * It stays well inside the 1 s `TRACKING_LOSS_RESET` in `tobii-output`'s
///   pipeline, that crate's statement of when old state has become a lie. A
///   reconstruction must not outlive the state it is composed into.
/// * The 400-frame session of 2026-08-09 — the one
///   `docs/wiki/Planned-Work.md` quotes as roughly five per-eye dropouts a
///   second — makes the ordinary outage short: its left eye read invalid in
///   34% of 400 frames across 43 separate outages (~3.2 frames, ~95 ms each)
///   and its right in 18% across 18 outages (~4 frames, ~120 ms). 300 ms is
///   about two and a half times that, so an ordinary dropout is covered end to
///   end, while that session's two extremes (480 ms on the left, 1260 ms on
///   the right) deliberately run out: a second of frozen rotation is the
///   confident guess this bound exists to refuse.
///
/// What was **not** measured is the distribution between those numbers — the
/// session recorded the rate and the two maxima, not a histogram — so 300 ms
/// is a conservative choice inside them, not a fitted one.
pub const RECONSTRUCTION_MAX_AGE: Duration = Duration::from_millis(300);

/// The last measured offset between the two eyes, used to keep producing a
/// pose while the tracker can only see one of them.
///
/// [`pose_from_sample`] needs both eyes and returns nothing otherwise, which on
/// this hardware is not a corner case: the 2026-08-09 session measured about
/// five per-eye dropouts a second, with one eye lost far more often than both
/// (the head sits off the tracker's optical axis, so the two eyes drop out
/// independently). Every one of those frames is a pose the driver could have
/// sent and did not.
///
/// The algorithm is the one `tobii-gtk`'s `eyeview::PairOffset` already uses to
/// keep drawing both dots — re-measure the delta whenever both eyes are real,
/// reconstruct the missing one from it when exactly one is, age it out so it
/// can never draw a ghost. This is a second implementation rather than a shared
/// one because the two work in different spaces: that one carries normalized
/// trackbox positions for a drawing, this one carries tracker-space
/// millimetres for a pose, and ages in wall-clock time because its consumer
/// already has a clock and a stalled stream must not be able to make an old
/// offset look fresh by simply not arriving.
///
/// **Reconstructing beats dropping the frame, but it is not free.** Feeding the
/// surviving eye's origin straight into [`pose_from_eyes`] as if it were the
/// head centre would move the reported head half an interocular distance
/// sideways the moment an eye blinked out — a yank, where today's behaviour is
/// only a stutter. The stored offset is what keeps the centre still.
#[derive(Debug, Clone, Copy, Default)]
pub struct PairOffset {
    /// Right eye minus left eye, in tracker-space mm, from the most recent
    /// frame that carried both.
    delta_mm: Option<[f64; 3]>,
    /// When that measurement was taken. Only a two-eye frame moves it, so the
    /// age keeps growing for as long as an outage lasts.
    measured_at: Option<Instant>,
    /// Consecutive samples so far with exactly one usable eye.
    one_eye_run: u32,
}

impl PairOffset {
    /// A fresh offset that has measured nothing yet.
    pub fn new() -> Self {
        PairOffset::default()
    }

    /// Forget the measurement, so nothing is reconstructed until both eyes
    /// have been seen together again.
    pub fn reset(&mut self) {
        *self = PairOffset::default();
    }

    /// The stateful counterpart of [`pose_from_sample`]: a pose from two eyes
    /// where there are two, and from one eye plus the last offset where the
    /// tracker has momentarily lost the other.
    ///
    /// With both eyes tracked this returns bit-for-bit what
    /// [`pose_from_sample`] returns, from the same strict gate — present bits
    /// *and* `validity == 0`, because the device sends zeroed origin columns
    /// rather than omitting them. `None` means no pose at all: no eye, no
    /// offset measured yet, the debounce not yet satisfied, or an offset past
    /// [`RECONSTRUCTION_MAX_AGE`].
    pub fn pose_from_sample(&mut self, s: &GazeSample, now: Instant) -> Option<SourcedPose> {
        // Without both validity columns there is nothing to gate on, and
        // validity defaults to 0 — which must not be read as "tracked". The
        // same refusal as the stateless path, and it must come first: a frame
        // that cannot say which eyes it has must not advance the debounce.
        if !s.has(present::VALIDITY_L) || !s.has(present::VALIDITY_R) {
            self.one_eye_run = 0;
            return None;
        }
        let left = s.validity_l == VALIDITY_TRACKED && s.has(present::EYE_ORIGIN_L);
        let right = s.validity_r == VALIDITY_TRACKED && s.has(present::EYE_ORIGIN_R);

        match (left, right) {
            (true, true) => {
                let (l, r) = (s.eye_origin_l_mm, s.eye_origin_r_mm);
                self.delta_mm = Some([r[0] - l[0], r[1] - l[1], r[2] - l[2]]);
                self.measured_at = Some(now);
                self.one_eye_run = 0;
                Some(SourcedPose {
                    pose: pose_from_eyes(l, r),
                    source: PoseSource::BothEyes,
                })
            }
            (true, false) => self.reconstruct(s.eye_origin_l_mm, Seen::Left, now),
            (false, true) => self.reconstruct(s.eye_origin_r_mm, Seen::Right, now),
            (false, false) => {
                // Nobody there. Not a one-eye outage, so the debounce starts
                // again rather than counting this towards one.
                self.one_eye_run = 0;
                None
            }
        }
    }

    /// Place the eye the tracker cannot see at the one it can, plus the stored
    /// offset, and take the pose from the completed pair.
    fn reconstruct(&mut self, seen_mm: [f64; 3], seen: Seen, now: Instant) -> Option<SourcedPose> {
        self.one_eye_run = self.one_eye_run.saturating_add(1);
        if self.one_eye_run < ONE_EYE_DEBOUNCE {
            return None;
        }
        // `?` on both: nothing is reconstructed before both eyes have ever
        // been seen together, and a stale offset yields no pose rather than a
        // confident one.
        let measured_at = self.measured_at?;
        if now.saturating_duration_since(measured_at) > RECONSTRUCTION_MAX_AGE {
            return None;
        }
        let d = self.delta_mm?;
        let shifted = |sign: f64| {
            [
                seen_mm[0] + sign * d[0],
                seen_mm[1] + sign * d[1],
                seen_mm[2] + sign * d[2],
            ]
        };
        let (left_mm, right_mm) = match seen {
            Seen::Left => (seen_mm, shifted(1.0)),
            Seen::Right => (shifted(-1.0), seen_mm),
        };
        Some(SourcedPose {
            pose: pose_from_eyes(left_mm, right_mm),
            source: PoseSource::Reconstructed,
        })
    }
}

/// Which eye the tracker still has, so the offset is applied in the right
/// direction: the delta points from the left eye to the right one.
#[derive(Debug, Clone, Copy)]
enum Seen {
    Left,
    Right,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spread is what makes "you moved" visible instead of silently
    /// producing a confident wrong zero.
    /// Median, not mean: the frames before the ROI converges are outliers, and a
    /// handful of them must not be able to move the answer.
    #[test]
    fn the_pitch_offset_is_a_median_and_is_negated() {
        let mut s: Vec<f64> = (0..100).map(|i| 24.0 + (i % 3) as f64 * 0.1).collect();
        s[0] = -180.0; // an unconverged first frame
        s[1] = 90.0;
        let (offset, _) = pitch_offset_from(&mut s).expect("enough samples");
        assert!(
            (offset + 24.1).abs() < 0.2,
            "two wild outliers moved the answer: {offset}"
        );
    }

    #[test]
    fn a_short_run_is_not_a_measurement() {
        let mut few: Vec<f64> = (0..19).map(|i| i as f64).collect();
        assert_eq!(pitch_offset_from(&mut few), None);
        let mut enough: Vec<f64> = (0..20).map(|i| i as f64).collect();
        assert!(pitch_offset_from(&mut enough).is_some());
    }

    #[test]
    fn the_spread_reports_a_head_that_moved() {
        let mut still: Vec<f64> = (0..100).map(|i| 24.0 + (i % 5) as f64 * 0.05).collect();
        let (_, tight) = pitch_offset_from(&mut still).unwrap();
        assert!(tight < 1.0, "a still head should be tight: {tight}");
        let mut moved: Vec<f64> = (0..100).map(|i| i as f64 * 0.5).collect();
        let (_, wide) = pitch_offset_from(&mut moved).unwrap();
        assert!(wide > 8.0, "a moving head should be visible: {wide}");
    }

    /// Half the interocular distance used to build synthetic eye pairs.
    const HALF_IPD: f64 = 31.5;

    /// Build a pair of eye origins around `centre` whose interocular vector has
    /// exactly the requested yaw (in the x–z plane) and roll (in the x–y plane).
    ///
    /// The two angles are defined as *independent projections* of one vector,
    /// so the construction sets each projection's tangent directly rather than
    /// composing two rigid rotations: composing rotations would make the second
    /// angle's projection depend on the first, which is a property of Euler
    /// extraction and not of the code under test. For a pure yaw or a pure roll
    /// this yields the same direction a true rotation would;
    /// [`rigid_rotation_recovers_yaw_and_preserves_eye_separation`] covers the
    /// physically rigid case explicitly.
    ///
    /// Roll (tilt to the user's right, positive) drops the right eye, so y goes
    /// negative. Yaw (turn to the user's right, positive) swings the right eye
    /// away from the tracker, so z goes positive.
    fn eyes_at(centre: [f64; 3], yaw_deg: f64, roll_deg: f64) -> ([f64; 3], [f64; 3]) {
        // Half the interocular vector, from centre to the right eye.
        let half = [
            HALF_IPD,
            HALF_IPD * -roll_deg.to_radians().tan(),
            HALF_IPD * yaw_deg.to_radians().tan(),
        ];
        let left = [
            centre[0] - half[0],
            centre[1] - half[1],
            centre[2] - half[2],
        ];
        let right = [
            centre[0] + half[0],
            centre[1] + half[1],
            centre[2] + half[2],
        ];
        (left, right)
    }

    fn assert_close(actual: f64, expected: f64, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "{what}: expected {expected}, got {actual}"
        );
    }

    #[test]
    fn square_on_head_is_level_and_centred() {
        let left = [-31.5, 20.0, 680.0];
        let right = [31.5, 20.0, 680.0];
        let p = pose_from_eyes(left, right);
        assert_close(p.x_mm, 0.0, "x");
        assert_close(p.y_mm, 20.0, "y");
        assert_close(p.z_mm, 680.0, "z");
        assert_close(p.yaw_deg, 0.0, "yaw");
        assert_close(p.roll_deg, 0.0, "roll");
        assert_close(p.pitch_deg, 0.0, "pitch");
    }

    #[test]
    fn position_is_the_midpoint_of_the_eyes() {
        // Deliberately asymmetric so a mean is distinguishable from either eye.
        let p = pose_from_eyes([-40.0, 10.0, 600.0], [20.0, 30.0, 700.0]);
        assert_close(p.x_mm, -10.0, "x");
        assert_close(p.y_mm, 20.0, "y");
        assert_close(p.z_mm, 650.0, "z");
    }

    #[test]
    fn yaw_is_recovered_over_a_range_of_angles() {
        for &expected in &[-45.0, -30.0, -10.0, 0.0, 10.0, 30.0, 45.0] {
            let (l, r) = eyes_at([0.0, 0.0, 680.0], expected, 0.0);
            let p = pose_from_eyes(l, r);
            assert_close(p.yaw_deg, expected, "yaw");
        }
    }

    #[test]
    fn yaw_is_positive_when_the_head_turns_to_the_users_right() {
        // Turning right swings the right eye away from the tracker (larger z)
        // and the left eye towards it.
        let p = pose_from_eyes([-30.0, 0.0, 670.0], [30.0, 0.0, 690.0]);
        assert!(p.yaw_deg > 0.0, "yaw={} should be positive", p.yaw_deg);
        // ...and the mirror image must be the opposite sign, same magnitude.
        let q = pose_from_eyes([-30.0, 0.0, 690.0], [30.0, 0.0, 670.0]);
        assert_close(q.yaw_deg, -p.yaw_deg, "mirrored yaw");
    }

    #[test]
    fn roll_is_recovered_over_a_range_of_angles() {
        for &expected in &[-40.0, -15.0, 0.0, 15.0, 40.0] {
            let (l, r) = eyes_at([0.0, 0.0, 680.0], 0.0, expected);
            let p = pose_from_eyes(l, r);
            assert_close(p.roll_deg, expected, "roll");
        }
    }

    #[test]
    fn roll_is_positive_when_the_head_tilts_to_the_users_right() {
        // Tilting right drops the right eye below the left.
        let p = pose_from_eyes([-30.0, 10.0, 680.0], [30.0, -10.0, 680.0]);
        assert!(p.roll_deg > 0.0, "roll={} should be positive", p.roll_deg);
        let q = pose_from_eyes([-30.0, -10.0, 680.0], [30.0, 10.0, 680.0]);
        assert_close(q.roll_deg, -p.roll_deg, "mirrored roll");
    }

    #[test]
    fn pure_yaw_produces_no_roll_and_pure_roll_produces_no_yaw() {
        let (l, r) = eyes_at([0.0, 0.0, 680.0], 35.0, 0.0);
        let yawed = pose_from_eyes(l, r);
        assert_close(yawed.yaw_deg, 35.0, "yaw");
        assert_close(yawed.roll_deg, 0.0, "roll from a pure yaw");

        let (l, r) = eyes_at([0.0, 0.0, 680.0], 0.0, 25.0);
        let rolled = pose_from_eyes(l, r);
        assert_close(rolled.roll_deg, 25.0, "roll");
        assert_close(rolled.yaw_deg, 0.0, "yaw from a pure roll");
    }

    #[test]
    fn combined_yaw_and_roll_are_both_recovered() {
        let (l, r) = eyes_at([10.0, -5.0, 700.0], 20.0, 15.0);
        let p = pose_from_eyes(l, r);
        assert_close(p.yaw_deg, 20.0, "yaw");
        assert_close(p.roll_deg, 15.0, "roll");
        assert_close(p.x_mm, 10.0, "x");
        assert_close(p.y_mm, -5.0, "y");
        assert_close(p.z_mm, 700.0, "z");
    }

    /// Rotate a real, IPD-preserving interocular vector and check the angle
    /// comes back out. This is the physically faithful version of a head turn:
    /// the eyes stay 2 × `HALF_IPD` apart, they just swing about the head's
    /// vertical axis.
    #[test]
    fn rigid_rotation_recovers_yaw_and_preserves_eye_separation() {
        for &expected in &[-60.0f64, -25.0, 0.0, 25.0, 60.0] {
            let a = expected.to_radians();
            let half = [HALF_IPD * a.cos(), 0.0, HALF_IPD * a.sin()];
            let left = [-half[0], 0.0, 680.0 - half[2]];
            let right = [half[0], 0.0, 680.0 + half[2]];

            let sep = ((right[0] - left[0]).powi(2)
                + (right[1] - left[1]).powi(2)
                + (right[2] - left[2]).powi(2))
            .sqrt();
            assert_close(sep, 2.0 * HALF_IPD, "eye separation");

            let p = pose_from_eyes(left, right);
            assert_close(p.yaw_deg, expected, "yaw");
            assert_close(p.roll_deg, 0.0, "roll");
            assert_close(p.z_mm, 680.0, "z");
        }
    }

    /// The rigid counterpart for roll: the head tips about the axis running
    /// out through the nose, so the eyes stay the same distance apart.
    #[test]
    fn rigid_rotation_recovers_roll_and_preserves_eye_separation() {
        for &expected in &[-50.0f64, -20.0, 0.0, 20.0, 50.0] {
            let a = expected.to_radians();
            let half = [HALF_IPD * a.cos(), -HALF_IPD * a.sin(), 0.0];
            let left = [-half[0], -half[1], 680.0];
            let right = [half[0], half[1], 680.0];

            let sep = ((right[0] - left[0]).powi(2) + (right[1] - left[1]).powi(2)).sqrt();
            assert_close(sep, 2.0 * HALF_IPD, "eye separation");

            let p = pose_from_eyes(left, right);
            assert_close(p.roll_deg, expected, "roll");
            assert_close(p.yaw_deg, 0.0, "yaw");
        }
    }

    #[test]
    fn pitch_is_always_zero() {
        let (l, r) = eyes_at([0.0, 0.0, 680.0], 30.0, -20.0);
        assert_eq!(pose_from_eyes(l, r).pitch_deg, 0.0);
    }

    #[test]
    fn coincident_eyes_do_not_produce_nan() {
        let p = pose_from_eyes([1.0, 2.0, 3.0], [1.0, 2.0, 3.0]);
        assert!(p.is_finite(), "{p:?}");
        assert_close(p.yaw_deg, 0.0, "yaw");
        assert_close(p.roll_deg, 0.0, "roll");
    }

    /// A sample with both eyes tracked and their origins populated.
    fn tracked_sample() -> GazeSample {
        GazeSample {
            present_mask: present::EYE_ORIGIN_L
                | present::EYE_ORIGIN_R
                | present::VALIDITY_L
                | present::VALIDITY_R,
            validity_l: 0,
            validity_r: 0,
            eye_origin_l_mm: [-31.5, 20.0, 680.0],
            eye_origin_r_mm: [31.5, 20.0, 680.0],
            ..GazeSample::default()
        }
    }

    #[test]
    fn sample_with_both_eyes_tracked_yields_a_pose() {
        let p = pose_from_sample(&tracked_sample()).expect("both eyes tracked");
        assert_close(p.z_mm, 680.0, "z");
        assert_close(p.yaw_deg, 0.0, "yaw");
    }

    #[test]
    fn sample_is_rejected_when_either_eye_is_not_detected() {
        for (vl, vr) in [(4, 0), (0, 4), (4, 4)] {
            let s = GazeSample {
                validity_l: vl,
                validity_r: vr,
                ..tracked_sample()
            };
            assert!(
                pose_from_sample(&s).is_none(),
                "validity ({vl}, {vr}) must not yield a pose"
            );
        }
    }

    #[test]
    fn sample_is_rejected_when_origins_are_present_but_zeroed() {
        // The real-world no-eyes frame: the device still sends both eye-origin
        // columns, zeroed, with validity 4. Gating on the present bit alone
        // would report a head sitting on the tracker itself.
        let s = GazeSample {
            validity_l: 4,
            validity_r: 4,
            eye_origin_l_mm: [0.0; 3],
            eye_origin_r_mm: [0.0; 3],
            ..tracked_sample()
        };
        assert!(s.has(present::EYE_ORIGIN_L) && s.has(present::EYE_ORIGIN_R));
        assert!(pose_from_sample(&s).is_none());
    }

    #[test]
    fn sample_is_rejected_when_the_origin_columns_are_absent() {
        let s = GazeSample {
            present_mask: present::VALIDITY_L | present::VALIDITY_R,
            ..tracked_sample()
        };
        assert!(pose_from_sample(&s).is_none());
    }

    #[test]
    fn sample_is_rejected_when_the_validity_columns_are_absent() {
        // Without a validity column there is nothing to gate on, and validity
        // defaults to 0 — which must not be mistaken for "tracked".
        let s = GazeSample {
            present_mask: present::EYE_ORIGIN_L | present::EYE_ORIGIN_R,
            ..tracked_sample()
        };
        assert!(pose_from_sample(&s).is_none());
    }

    /// Where a head actually sat in this project's own dropout session: 795 mm
    /// from the tracker and 17.4° off its optical axis, 8.7° horizontally and
    /// 15.3° vertically (the session `docs/wiki/Planned-Work.md` quotes for
    /// both the five-dropouts-a-second rate and the off-axis placement that
    /// causes it). The distances are that session's; the *signs* are a choice,
    /// and nothing below turns on them — the point of an off-centre,
    /// three-way-asymmetric head is that a path which swapped two components
    /// or dropped one cannot match the other path by accident.
    const OFF_AXIS_CENTRE: [f64; 3] = [121.6, 217.4, 795.0];

    /// A both-eyes sample: a real head, at a real angle, in the frame shape the
    /// device sends.
    fn pair_sample(centre: [f64; 3], yaw_deg: f64, roll_deg: f64) -> GazeSample {
        let (l, r) = eyes_at(centre, yaw_deg, roll_deg);
        GazeSample {
            eye_origin_l_mm: l,
            eye_origin_r_mm: r,
            ..tracked_sample()
        }
    }

    /// The same frame after the tracker loses one eye, in the shape it really
    /// arrives in: validity 4 with the origin column still **present** and
    /// zeroed. Every gaze frame in the committed capture
    /// (`crates/tobii-usb/tests/captures/session.tobiicap`, recorded with
    /// nobody in front of the tracker) looks like this, which is why a
    /// present-bit check alone would report an eye sitting on the sensor.
    fn without_eye(s: &GazeSample, drop_right: bool) -> GazeSample {
        let mut out = s.clone();
        if drop_right {
            out.validity_r = 4;
            out.eye_origin_r_mm = [0.0; 3];
        } else {
            out.validity_l = 4;
            out.eye_origin_l_mm = [0.0; 3];
        }
        out
    }

    /// Feed a sample until the debounce is satisfied, returning the pose the
    /// fallback settles on.
    fn after_debounce(pair: &mut PairOffset, s: &GazeSample, now: Instant) -> SourcedPose {
        for _ in 1..ONE_EYE_DEBOUNCE {
            assert!(
                pair.pose_from_sample(s, now).is_none(),
                "the fallback engaged before the debounce was satisfied"
            );
        }
        pair.pose_from_sample(s, now).expect("a reconstructed pose")
    }

    /// The regression that matters most: where the new path is not taken, the
    /// old one must be untouched. Not "close" — the same bits, so the pose a
    /// game receives on an ordinary two-eye frame cannot have moved at all.
    #[test]
    fn with_both_eyes_the_pose_is_bit_for_bit_the_stateless_one() {
        let mut pair = PairOffset::new();
        let now = Instant::now();
        for &(yaw, roll) in &[
            (0.0, 0.0),
            (12.5, -7.25),
            (-31.0, 18.0),
            (45.0, 40.0),
            (-3.75, 0.5),
        ] {
            let s = pair_sample(OFF_AXIS_CENTRE, yaw, roll);
            let stateful = pair
                .pose_from_sample(&s, now)
                .expect("both eyes are tracked");
            assert_eq!(stateful.source, PoseSource::BothEyes);
            assert_eq!(
                stateful.pose,
                pose_from_sample(&s).expect("the stateless path agrees there is a pose"),
                "the two-eye pose moved at yaw {yaw} roll {roll}"
            );
        }
    }

    /// Every gate the stateless path refuses on, the stateful one refuses on
    /// too. A fallback that quietly loosened the validity rule would report a
    /// head sitting on the tracker's own sensor.
    #[test]
    fn the_stateful_path_refuses_everything_the_stateless_one_refuses() {
        let now = Instant::now();
        let refused = [
            // No eyes at all: the real no-eyes frame, both origins zeroed.
            without_eye(&without_eye(&tracked_sample(), true), false),
            // Origin columns absent.
            GazeSample {
                present_mask: present::VALIDITY_L | present::VALIDITY_R,
                ..tracked_sample()
            },
            // Validity columns absent — validity defaults to 0, which is not
            // the same thing as "tracked".
            GazeSample {
                present_mask: present::EYE_ORIGIN_L | present::EYE_ORIGIN_R,
                ..tracked_sample()
            },
        ];
        for s in refused {
            assert_eq!(pose_from_sample(&s), None, "premise");
            // A fresh offset each time: with nothing measured there is nothing
            // to reconstruct from either.
            assert!(PairOffset::new().pose_from_sample(&s, now).is_none());
        }
    }

    /// The reason the offset exists at all. Dropping to "wherever the eye I can
    /// still see is" would move the reported head half an interocular distance
    /// sideways the instant an eye blinked out — a yank, where today's
    /// behaviour is merely a missing frame.
    #[test]
    fn a_lost_eye_does_not_move_the_head_centre() {
        let mut pair = PairOffset::new();
        let now = Instant::now();
        let both = pair_sample(OFF_AXIS_CENTRE, 0.0, 0.0);
        let before = pair.pose_from_sample(&both, now).expect("both eyes").pose;

        let one_eye = without_eye(&both, true);
        let after = after_debounce(&mut pair, &one_eye, now);
        assert_eq!(after.source, PoseSource::Reconstructed);
        assert_close(after.pose.x_mm, before.x_mm, "x");
        assert_close(after.pose.y_mm, before.y_mm, "y");
        assert_close(after.pose.z_mm, before.z_mm, "z");

        // And the naive answer really would have been a long way off: the
        // surviving left eye sits half an interocular distance from centre.
        assert!(
            (one_eye.eye_origin_l_mm[0] - before.x_mm).abs() > HALF_IPD - 1.0,
            "the test is not measuring anything: the surviving eye is already \
             at the centre"
        );
    }

    /// A head that moves while one eye is out still moves: the reconstruction
    /// follows the eye the tracker can see, one for one.
    #[test]
    fn a_reconstructed_pose_follows_the_eye_that_is_still_there() {
        let mut pair = PairOffset::new();
        let now = Instant::now();
        let both = pair_sample(OFF_AXIS_CENTRE, 0.0, 0.0);
        let start = pair.pose_from_sample(&both, now).expect("both eyes").pose;

        // The right eye drops out and the head slides 10 mm left and 5 mm up.
        let mut moved = without_eye(&both, true);
        moved.eye_origin_l_mm[0] -= 10.0;
        moved.eye_origin_l_mm[1] += 5.0;
        let after = after_debounce(&mut pair, &moved, now);
        assert_close(after.pose.x_mm, start.x_mm - 10.0, "x follows the eye");
        assert_close(after.pose.y_mm, start.y_mm + 5.0, "y follows the eye");
    }

    /// What a reconstructed frame is really claiming. Yaw and roll come from
    /// the interocular vector, and during an outage that vector *is* the stored
    /// offset — so rotation is **held at its last measured value**, not
    /// extrapolated. That is the honest half of the guess, and the reason
    /// [`RECONSTRUCTION_MAX_AGE`] is short.
    #[test]
    fn rotation_is_held_at_its_last_measurement_while_an_eye_is_missing() {
        let mut pair = PairOffset::new();
        let now = Instant::now();
        let both = pair_sample(OFF_AXIS_CENTRE, 17.0, -9.0);
        let measured = pair.pose_from_sample(&both, now).expect("both eyes").pose;
        assert_close(measured.yaw_deg, 17.0, "premise: yaw");

        let mut drifting = without_eye(&both, false);
        drifting.eye_origin_r_mm[2] += 25.0; // the visible eye swings backwards
        let after = after_debounce(&mut pair, &drifting, now);
        assert_eq!(
            after.pose.yaw_deg, measured.yaw_deg,
            "yaw must be the held measurement, not a new guess"
        );
        assert_eq!(after.pose.roll_deg, measured.roll_deg, "roll likewise");
    }

    /// The bound, from both sides. A pose at the limit, none past it: past
    /// `RECONSTRUCTION_MAX_AGE` a stale offset is refused rather than reported
    /// as though it were still a measurement.
    #[test]
    fn an_offset_past_its_age_yields_no_pose_rather_than_a_confident_guess() {
        let now = Instant::now();
        let both = pair_sample(OFF_AXIS_CENTRE, 0.0, 0.0);
        let one_eye = without_eye(&both, true);

        let mut fresh = PairOffset::new();
        fresh.pose_from_sample(&both, now);
        assert!(
            after_debounce(&mut fresh, &one_eye, now + RECONSTRUCTION_MAX_AGE)
                .source
                .eq(&PoseSource::Reconstructed),
            "the bound is inclusive at its limit"
        );

        let mut stale = PairOffset::new();
        stale.pose_from_sample(&both, now);
        let past = now + RECONSTRUCTION_MAX_AGE + Duration::from_millis(1);
        for _ in 0..ONE_EYE_DEBOUNCE {
            assert!(
                stale.pose_from_sample(&one_eye, past).is_none(),
                "a stale offset must not produce a pose"
            );
        }
    }

    /// The offset ages from the last *two-eye* frame, not from the last call:
    /// a long outage runs out even though a one-eye sample arrives every 30 ms.
    #[test]
    fn a_long_outage_runs_out_even_though_samples_keep_arriving() {
        let mut pair = PairOffset::new();
        let start = Instant::now();
        pair.pose_from_sample(&pair_sample(OFF_AXIS_CENTRE, 0.0, 0.0), start);
        let one_eye = without_eye(&pair_sample(OFF_AXIS_CENTRE, 0.0, 0.0), true);

        // The cadence measured between consecutive gaze frames in the
        // committed capture, so this is the real frame rate ageing it out.
        let cadence = Duration::from_micros(30_208);
        let mut produced = 0;
        let mut at = start;
        for _ in 0..100 {
            at += cadence;
            if pair.pose_from_sample(&one_eye, at).is_some() {
                produced += 1;
            }
        }
        assert!(produced > 0, "the fallback never engaged at all");
        assert!(
            pair.pose_from_sample(&one_eye, at).is_none(),
            "a three-second outage was still producing a pose"
        );
    }

    #[test]
    fn nothing_is_reconstructed_before_both_eyes_have_ever_been_seen() {
        let mut pair = PairOffset::new();
        let now = Instant::now();
        let one_eye = without_eye(&pair_sample(OFF_AXIS_CENTRE, 0.0, 0.0), true);
        for _ in 0..ONE_EYE_DEBOUNCE * 4 {
            assert!(
                pair.pose_from_sample(&one_eye, now).is_none(),
                "no offset has been measured yet"
            );
        }
    }

    /// The debounce is one-directional on purpose. A 2→1 flicker has to persist
    /// to change the path, but the *return* to two eyes is immediate — anything
    /// else would mean answering a frame that carries two measured eyes with a
    /// reconstruction, and that is the one thing this must never do.
    #[test]
    fn the_debounce_delays_the_fallback_but_never_the_return_to_two_eyes() {
        let mut pair = PairOffset::new();
        let now = Instant::now();
        let both = pair_sample(OFF_AXIS_CENTRE, 6.0, 0.0);
        let one_eye = without_eye(&both, true);
        pair.pose_from_sample(&both, now);

        // A single flickering frame never switches the path.
        for _ in 1..ONE_EYE_DEBOUNCE {
            assert!(pair.pose_from_sample(&one_eye, now).is_none());
            assert_eq!(
                pair.pose_from_sample(&both, now)
                    .expect("two eyes again")
                    .source,
                PoseSource::BothEyes,
                "a two-eye frame must be answered with the two-eye pose at once"
            );
        }
        // A persistent outage does switch it.
        assert_eq!(
            after_debounce(&mut pair, &one_eye, now).source,
            PoseSource::Reconstructed
        );
    }

    /// Losing both eyes is not a one-eye outage: the run starts again, so the
    /// frame after a blank gap cannot arrive already past the debounce.
    #[test]
    fn a_frame_with_no_eyes_restarts_the_debounce() {
        let mut pair = PairOffset::new();
        let now = Instant::now();
        let both = pair_sample(OFF_AXIS_CENTRE, 0.0, 0.0);
        let one_eye = without_eye(&both, true);
        let no_eyes = without_eye(&one_eye, false);
        pair.pose_from_sample(&both, now);

        for _ in 1..ONE_EYE_DEBOUNCE {
            pair.pose_from_sample(&one_eye, now);
        }
        assert!(pair.pose_from_sample(&no_eyes, now).is_none(), "premise");
        for _ in 1..ONE_EYE_DEBOUNCE {
            assert!(
                pair.pose_from_sample(&one_eye, now).is_none(),
                "the debounce carried over a gap with no eyes in it"
            );
        }
    }

    #[test]
    fn a_reset_forgets_the_measurement() {
        let mut pair = PairOffset::new();
        let now = Instant::now();
        let both = pair_sample(OFF_AXIS_CENTRE, 0.0, 0.0);
        let one_eye = without_eye(&both, true);
        pair.pose_from_sample(&both, now);
        assert_eq!(
            after_debounce(&mut pair, &one_eye, now).source,
            PoseSource::Reconstructed,
            "premise"
        );

        pair.reset();
        for _ in 0..ONE_EYE_DEBOUNCE * 2 {
            assert!(
                pair.pose_from_sample(&one_eye, now).is_none(),
                "a reset offset must not still be reconstructing"
            );
        }
    }
}
