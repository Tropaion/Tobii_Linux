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
pub mod opentrack;
pub mod preprocess;

pub use filter::PoseFilter;
pub use model::{ModelConfig, ModelKind, PoseModel};
pub use opentrack::to_opentrack_datagram;

use tobii_protocol::gaze::{present, GazeSample};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
