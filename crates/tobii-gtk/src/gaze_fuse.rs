//! Rebuild the gaze point from the two per-eye points instead of taking the
//! device's fused one.
//!
//! # Why
//!
//! A 39-target sweep on a 1193 mm panel showed horizontal error 2.3x worse
//! looking left than right, and the leftward error always leftward. Nothing
//! about the screen can do that: curvature, plane width, plane depth and the
//! display offset are all symmetric about screen centre, or a constant shift.
//! Two eyes are the only asymmetric thing in the system.
//!
//! The per-eye gaze points (`0x05`/`0x0b`) show why. Each carries a large,
//! near-constant horizontal bias — about -62 mm for the left eye and +46 mm for
//! the right on the measured setup — so the two straddle the target by roughly
//! 110 mm. Averaging cancels that, which is why the device's fused point is
//! accurate at centre (10 mm) while each eye alone looks terrible (62 and
//! 46 mm). **This is why "just use the better eye" is wrong**: it trades a
//! small error for half the straddle.
//!
//! But off-centre the two eyes stop being equally good, and in a completely
//! consistent direction: **the eye on the same side as the gaze degrades**, in
//! 28 of 31 targets. Look left and the left eye goes; look right and the right
//! eye goes. That is ordinary physiology — the ipsilateral eye rotates away
//! from the tracker and presents an increasingly oblique cornea — and the
//! device's fusion averages the failing eye in anyway.
//!
//! Removing each eye's own bias and then following the *contralateral* eye
//! scored, against the same sweep:
//!
//! ```text
//!   rule                        left   centre   right    overall
//!   device's own fusion         103      10       55        61 mm
//!   plain average, de-biased    100       7       73        64 mm
//!   contralateral eye            68       7       52        45 mm
//! ```
//!
//! # What this is not
//!
//! A geometric correction applied on top of the device's answer. That was tried
//! and reverted (`8b21938`) for double-counting what the calibration had
//! already absorbed. This recomputes the *fusion* from signals the device hands
//! us, and it reduces to the device's own answer whenever the eyes agree — so
//! at centre it changes nothing, by construction rather than by luck.
//!
//! Held honestly: the numbers above come from one session with one person, and
//! the bias is assumed steady while the head moves. [`Fuser`] therefore learns
//! the bias live and falls back to the device's point until it has one.

/// Fused-gaze-point estimator that follows whichever eye the geometry favours.
#[derive(Debug, Clone, Default)]
pub struct Fuser {
    /// Each eye's running horizontal/vertical offset from the device's fused
    /// point, learned only while gaze is near screen centre.
    bias_l: Option<[f64; 2]>,
    bias_r: Option<[f64; 2]>,
}

/// Gaze within this much of screen centre (normalized) is "looking straight
/// ahead", where both eyes are equally trustworthy and the bias can be learned.
const CENTRE_BAND: f64 = 0.1;

/// How fast the bias estimate follows. Slow: the quantity is a property of the
/// person and their calibration, not of the moment, and a fast filter would let
/// a few off-centre frames poison it.
const BIAS_ALPHA: f64 = 0.02;

/// Over how much of the screen the estimate hands off from one eye to the
/// other. A hard switch at the exact centre is safe in principle — both
/// de-biased eyes agree there — but only as far as the bias estimate is exact,
/// and a ramp degrades gracefully when it is not.
const HANDOFF: f64 = 0.1;

impl Fuser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold in one frame and return the gaze point to use.
    ///
    /// `fused` is the device's own combined point; `left`/`right` are the
    /// per-eye points, `None` for an eye the device did not resolve this frame.
    /// Returns `None` only when the device itself had nothing.
    pub fn update(
        &mut self,
        fused: Option<[f64; 2]>,
        left: Option<[f64; 2]>,
        right: Option<[f64; 2]>,
    ) -> Option<[f64; 2]> {
        let fused = fused?;

        // Learn only near centre. Off-centre one eye is actively wrong, and
        // learning there would fold that error into the constant.
        if (fused[0] - 0.5).abs() < CENTRE_BAND {
            if let Some(l) = left {
                Self::learn(&mut self.bias_l, l, fused);
            }
            if let Some(r) = right {
                Self::learn(&mut self.bias_r, r, fused);
            }
        }

        let (Some(bl), Some(br)) = (self.bias_l, self.bias_r) else {
            // Nothing learned yet: the device's own answer is the best
            // available, and is what every previous frame has been using.
            return Some(fused);
        };
        let de_l = left.map(|l| [l[0] - bl[0], l[1] - bl[1]]);
        let de_r = right.map(|r| [r[0] - br[0], r[1] - br[1]]);

        match (de_l, de_r) {
            (Some(l), Some(r)) => {
                // Looking left of centre favours the RIGHT eye, and vice versa.
                let off = fused[0] - 0.5;
                let w = (off.abs() / HANDOFF).clamp(0.0, 1.0);
                let (contra, ipsi) = if off < 0.0 { (r, l) } else { (l, r) };
                Some([
                    ipsi[0] + (contra[0] - ipsi[0]) * (0.5 + 0.5 * w),
                    ipsi[1] + (contra[1] - ipsi[1]) * (0.5 + 0.5 * w),
                ])
            }
            // One eye is all the device could resolve. De-biased, it is still
            // a better answer than nothing — and the device's own fused point
            // in this situation is computed from that same single eye.
            (Some(one), None) | (None, Some(one)) => Some(one),
            (None, None) => Some(fused),
        }
    }

    fn learn(slot: &mut Option<[f64; 2]>, eye: [f64; 2], fused: [f64; 2]) {
        let obs = [eye[0] - fused[0], eye[1] - fused[1]];
        *slot = Some(match *slot {
            None => obs,
            Some(b) => [
                b[0] + (obs[0] - b[0]) * BIAS_ALPHA,
                b[1] + (obs[1] - b[1]) * BIAS_ALPHA,
            ],
        });
    }

    /// The learned per-eye offsets, for diagnostics. `None` until seen.
    pub fn biases(&self) -> (Option<[f64; 2]>, Option<[f64; 2]>) {
        (self.bias_l, self.bias_r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed enough centred frames for the bias filter to converge.
    fn trained(bias_l: f64, bias_r: f64) -> Fuser {
        let mut f = Fuser::new();
        for _ in 0..600 {
            f.update(
                Some([0.5, 0.5]),
                Some([0.5 + bias_l, 0.5]),
                Some([0.5 + bias_r, 0.5]),
            );
        }
        f
    }

    #[test]
    fn before_learning_anything_it_returns_the_devices_own_point() {
        let mut f = Fuser::new();
        // Off-centre on the very first frame: nothing has been learned, so it
        // must not invent a correction.
        assert_eq!(
            f.update(Some([0.2, 0.5]), Some([0.1, 0.5]), Some([0.3, 0.5])),
            Some([0.2, 0.5])
        );
    }

    #[test]
    fn it_learns_each_eyes_straddle_and_cancels_it_at_centre() {
        let f = trained(-0.05, 0.05);
        let (bl, br) = f.biases();
        assert!((bl.unwrap()[0] + 0.05).abs() < 0.005, "{bl:?}");
        assert!((br.unwrap()[0] - 0.05).abs() < 0.005, "{br:?}");
    }

    #[test]
    fn at_centre_it_reproduces_the_devices_answer() {
        // The whole safety argument: whatever else this does, it must not move
        // the part of the screen that already works.
        let mut f = trained(-0.05, 0.05);
        let got = f
            .update(Some([0.5, 0.5]), Some([0.45, 0.5]), Some([0.55, 0.5]))
            .unwrap();
        assert!((got[0] - 0.5).abs() < 0.002, "{got:?}");
    }

    #[test]
    fn looking_left_it_follows_the_right_eye() {
        let mut f = trained(-0.05, 0.05);
        // Target at 0.2. The right eye is accurate; the left has drifted a
        // long way further left, as it measurably does at this angle.
        let got = f
            .update(Some([0.15, 0.5]), Some([0.05, 0.5]), Some([0.25, 0.5]))
            .unwrap();
        assert!(
            (got[0] - 0.2).abs() < 0.01,
            "expected to track the right eye's 0.20, got {got:?}"
        );
    }

    #[test]
    fn looking_right_it_follows_the_left_eye() {
        let mut f = trained(-0.05, 0.05);
        let got = f
            .update(Some([0.85, 0.5]), Some([0.75, 0.5]), Some([0.95, 0.5]))
            .unwrap();
        assert!(
            (got[0] - 0.8).abs() < 0.01,
            "expected to track the left eye's 0.80, got {got:?}"
        );
    }

    #[test]
    fn a_single_valid_eye_is_used_de_biased() {
        let mut f = trained(-0.05, 0.05);
        let got = f.update(Some([0.3, 0.5]), None, Some([0.35, 0.5])).unwrap();
        assert!((got[0] - 0.3).abs() < 0.005, "{got:?}");
    }

    #[test]
    fn no_device_point_yields_nothing() {
        let mut f = trained(-0.05, 0.05);
        assert_eq!(f.update(None, Some([0.4, 0.5]), Some([0.6, 0.5])), None);
    }

    #[test]
    fn off_centre_frames_do_not_poison_the_learned_bias() {
        let mut f = trained(-0.05, 0.05);
        let before = f.biases();
        for _ in 0..600 {
            // A wildly wrong left eye, far off-centre — exactly the situation
            // the learning gate exists to exclude.
            f.update(Some([0.9, 0.5]), Some([0.2, 0.5]), Some([0.95, 0.5]));
        }
        let after = f.biases();
        assert!(
            (before.0.unwrap()[0] - after.0.unwrap()[0]).abs() < 1e-9,
            "{before:?} -> {after:?}"
        );
    }

    #[test]
    fn the_handoff_is_continuous_across_the_centre() {
        // A jump here would show up as the cursor teleporting as gaze crosses
        // the middle of the screen, which is worse than a steady error.
        let mut f = trained(-0.05, 0.05);
        let mut last: Option<f64> = None;
        for i in 0..=40 {
            let x = 0.3 + 0.4 * (i as f64 / 40.0);
            let got = f
                .update(Some([x, 0.5]), Some([x - 0.05, 0.5]), Some([x + 0.05, 0.5]))
                .unwrap();
            if let Some(p) = last {
                assert!(
                    (got[0] - p).abs() < 0.02,
                    "jump at x={x}: {p} -> {}",
                    got[0]
                );
            }
            last = Some(got[0]);
        }
    }
}
