//! Smoothing for the derived head pose.
//!
//! Raw eye origins are noisy at the millimetre level, and that noise turns into
//! visible jitter once it is amplified into an in-game camera angle. A plain
//! exponential moving average is enough for a first cut; deliberately *not* a
//! one-euro filter or anything adaptive.

use crate::HeadPose;

/// Default smoothing factor: a reasonable compromise between jitter and lag at
/// the ET5's frame rate. Tune upwards for a snappier, noisier feel.
pub const DEFAULT_ALPHA: f64 = 0.25;

/// An exponential moving average over [`HeadPose`].
///
/// Each update blends the new sample into the running state with weight
/// `alpha`: `out = alpha * new + (1 - alpha) * previous`. `alpha == 1.0` is a
/// pass-through, and smaller values smooth harder at the cost of lag.
///
/// The filter never emits a non-finite value: a non-finite input is discarded
/// (the previous output is repeated) so one bad sample cannot poison the state.
///
/// Angles are blended componentwise, with no shortest-arc wraparound handling.
/// That is safe here because yaw and roll come from `atan2` on an interocular
/// vector that stays well away from ±180° for any pose a seated user can hold;
/// it would need revisiting if these angles were ever sourced from elsewhere.
#[derive(Debug, Clone)]
pub struct PoseFilter {
    alpha: f64,
    state: Option<HeadPose>,
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
        let alpha = if alpha.is_finite() {
            alpha.clamp(0.0, 1.0)
        } else {
            DEFAULT_ALPHA
        };
        Self { alpha, state: None }
    }

    /// The smoothing factor actually in use (after clamping).
    pub fn alpha(&self) -> f64 {
        self.alpha
    }

    /// The most recent output, or `None` before the first update.
    pub fn current(&self) -> Option<HeadPose> {
        self.state
    }

    /// Forget the running state, so the next update seeds the filter afresh.
    /// Used when tracking is lost, to avoid dragging a stale pose back in when
    /// the user returns to the trackbox somewhere else entirely.
    pub fn reset(&mut self) {
        self.state = None;
    }

    /// Blend `p` into the running average and return the smoothed pose.
    ///
    /// The first update after construction or [`reset`](Self::reset) adopts `p`
    /// outright rather than ramping up from zero, which would otherwise fling
    /// the pose in from the tracker's origin over the first second.
    pub fn update(&mut self, p: HeadPose) -> HeadPose {
        if !p.is_finite() {
            // Repeat the last good output; if there is none yet, stay neutral.
            return self.state.unwrap_or_default();
        }
        let out = match self.state {
            None => p,
            Some(prev) => {
                let a = self.alpha;
                let mix = |new: f64, old: f64| a * new + (1.0 - a) * old;
                HeadPose {
                    x_mm: mix(p.x_mm, prev.x_mm),
                    y_mm: mix(p.y_mm, prev.y_mm),
                    z_mm: mix(p.z_mm, prev.z_mm),
                    yaw_deg: mix(p.yaw_deg, prev.yaw_deg),
                    pitch_deg: mix(p.pitch_deg, prev.pitch_deg),
                    roll_deg: mix(p.roll_deg, prev.roll_deg),
                }
            }
        };
        self.state = Some(out);
        out
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
}
