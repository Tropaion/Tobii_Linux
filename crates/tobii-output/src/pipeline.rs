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
//! * **Recentre after Extended View, never before.** Translation goes out as
//!   displacement from where you normally sit, not as your position in front of
//!   the sensor — see [`FramePipeline::neutral`]. But Extended View is a geometric
//!   construction against display corners measured in the tracker's frame, so
//!   it needs the real position; subtract the neutral first and the head sits
//!   at the origin of its own coordinate system and the angle collapses to
//!   zero.
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
}

impl FramePipeline {
    /// A pipeline smoothing at the configured strength.
    pub fn new(cfg: &OutputConfig) -> Self {
        FramePipeline {
            filter: PoseFilter::new(cfg.filter_alpha),
            last_ev: None,
            gaze_lost_since: None,
            last_tracked: None,
            neutral: None,
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
            // Recentre AFTER Extended View, never before. Extended View is a
            // geometric construction: it needs the head's real position in the
            // tracker's frame, because the display corners it measures against
            // are in that same frame. Subtracting the neutral first puts the
            // head at the origin of its own coordinate system and the angle
            // collapses to nothing — which two of the tests below caught,
            // rather than it going out as a silently weaker effect.
            //
            // Rotation is never recentred: it is already referenced to facing
            // the screen, and pitch has its own measured zero
            // (`tobii headpose --calibrate-pitch`).
            let neutral = *self.neutral.get_or_insert([raw.x_mm, raw.y_mm, raw.z_mm]);
            let centred = HeadPose {
                x_mm: raw.x_mm - neutral[0],
                y_mm: raw.y_mm - neutral[1],
                z_mm: raw.z_mm - neutral[2],
                ..raw
            };
            // Compose first, then filter once: smoothing head and gaze
            // separately would leave the two contributions out of phase.
            self.filter
                .update(fusion::compose(centred, ev_yaw, ev_pitch))
        });

        if pose.is_some() {
            self.last_tracked = Some(now);
        } else {
            let lost_for = self.last_tracked.map(|t| now.saturating_duration_since(t));
            if lost_for.is_none_or(|d| d >= TRACKING_LOSS_RESET) {
                self.filter.reset();
                self.last_tracked = None;
                self.last_ev = None;
                // Dropped with the rest of the state: whoever comes back is
                // sitting down again, and that is the position to call centre.
                self.neutral = None;
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

        // Both eyes 680 mm in front of the sensor, which is where people sit.
        let seated = |z: f64| {
            let mut s = tracked_sample(Some([0.5, 0.5]));
            s.eye_origin_l_mm = [-32.0, 0.0, z];
            s.eye_origin_r_mm = [32.0, 0.0, z];
            s
        };

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
        let seated = |z: f64| {
            let mut s = tracked_sample(Some([0.5, 0.5]));
            s.eye_origin_l_mm = [-32.0, 0.0, z];
            s.eye_origin_r_mm = [32.0, 0.0, z];
            s
        };
        p.offer(&seated(680.0), None, &c, None, now);
        assert_eq!(p.neutral.map(|n| n[2]), Some(680.0));

        let lost = GazeSample {
            validity_l: 4,
            validity_r: 4,
            ..Default::default()
        };
        p.offer(&lost, None, &c, None, now + Duration::from_millis(200));
        assert_eq!(
            p.neutral.map(|n| n[2]),
            Some(680.0),
            "a short loss must not move the centre"
        );

        p.offer(&lost, None, &c, None, now + TRACKING_LOSS_RESET);
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
