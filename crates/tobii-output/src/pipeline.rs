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
//! Two of those orderings are load-bearing and neither is obvious:
//!
//! * **Compose, then filter once.** Smoothing the head pose and the gaze
//!   contribution separately leaves the two out of phase, so a fast look
//!   arrives before the head movement that accompanied it.
//! * **Hold through a blink.** Gaze vanishes for 100–400 ms every few seconds.
//!   Treating that as "look straight ahead" makes the camera lurch on every
//!   blink, so the last Extended View offset is held and decayed instead.
//!
//! This existed once as `compose_frame` inside the old `tobii serve` daemon,
//! where nothing else could reach it, and `tobii headpose` grew its own
//! partial copy that had neither of the above. Two copies of a sequence whose
//! errors are invisible is the worst possible thing to have two copies of.

use std::time::{Duration, Instant};

use tobii_headpose::{pose_from_sample, HeadPose, PoseFilter};
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
}

impl FramePipeline {
    /// A pipeline smoothing at the configured strength.
    pub fn new(cfg: &OutputConfig) -> Self {
        FramePipeline {
            filter: PoseFilter::new(cfg.filter_alpha),
            last_ev: None,
            gaze_lost_since: None,
            last_tracked: None,
        }
    }

    /// Compose one frame.
    ///
    /// `pose_in` is the pose from the neural model when there is one. It wins
    /// over the geometric fallback, because the model contributes pitch and two
    /// eye origins cannot: `pose_from_sample` pins pitch at zero by
    /// construction. Passing `None` asks for the geometric pose, which is what
    /// a 5-DOF run gets.
    pub fn offer(
        &mut self,
        sample: &GazeSample,
        pose_in: Option<HeadPose>,
        cfg: &OutputConfig,
        corners: Option<DisplayCorners>,
        now: Instant,
    ) -> TrackingFrame {
        let presence = Presence::from_validity(sample.validity_l, sample.validity_r);
        let gaze = gaze_of(sample);

        let pose = pose_in.or_else(|| pose_from_sample(sample)).map(|raw| {
            let (ev_yaw, ev_pitch) = match (cfg.extended_view.enabled, corners, gaze) {
                (true, Some(c), Some(g)) => {
                    let ev = fusion::extended_view(
                        &cfg.extended_view,
                        &c,
                        [raw.x_mm, raw.y_mm, raw.z_mm],
                        g,
                    );
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
            // Compose first, then filter once: smoothing head and gaze
            // separately would leave the two contributions out of phase.
            self.filter.update(fusion::compose(raw, ev_yaw, ev_pitch))
        });

        if pose.is_some() {
            self.last_tracked = Some(now);
        } else {
            let lost_for = self.last_tracked.map(|t| now.saturating_duration_since(t));
            if lost_for.is_none_or(|d| d >= TRACKING_LOSS_RESET) {
                self.filter.reset();
                self.last_tracked = None;
                self.last_ev = None;
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

    fn corners() -> DisplayCorners {
        DisplayCorners {
            tl: [-300.0, 200.0, 0.0],
            tr: [300.0, 200.0, 0.0],
            bl: [-300.0, -140.0, 0.0],
        }
    }

    fn cfg(ev: bool) -> OutputConfig {
        OutputConfig {
            extended_view: ExtendedView {
                enabled: ev,
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
        let supplied = HeadPose {
            pitch_deg: 17.0,
            ..Default::default()
        };
        let f = p.offer(
            &tracked_sample(Some([0.5, 0.5])),
            Some(supplied),
            &cfg(false),
            None,
            Instant::now(),
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
        let lost = GazeSample {
            validity_l: 4,
            validity_r: 4,
            ..Default::default()
        };
        p.offer(&lost, None, &c, None, now + Duration::from_millis(200));
        assert!(
            p.last_tracked.is_some(),
            "a 200 ms loss must not discard the smoothing state"
        );

        p.offer(&lost, None, &c, None, now + TRACKING_LOSS_RESET);
        assert!(
            p.last_tracked.is_none(),
            "a loss of a full second must reset"
        );
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
}
