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
//! Three of those orderings are load-bearing and none of them is obvious:
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

use tobii_headpose::{HeadPose, PairOffset, PoseFilter, PoseSource};
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
    /// The one-eye fallback's state: the last measured offset between the two
    /// eyes, plus its age and the eye-count debounce.
    ///
    /// It lives here for the same reason [`FramePipeline::neutral`] does — it
    /// is per-session tracking state, not configuration, and it has to survive
    /// a settings change. Rebuilding the pipeline to pick up a new smoothing
    /// strength would otherwise throw away the offset in the middle of an
    /// outage, which is exactly when it is the only thing producing a pose.
    pair: PairOffset,
    /// Frames whose pose came from two measured eyes, and from one eye plus
    /// the stored offset.
    ///
    /// Counted, not logged: this crate has no logger (it cross-compiles for
    /// the Wine bridge), and a fallback that is invisible is the failure mode
    /// the reconstruction risks — a guess that reads exactly like a
    /// measurement. [`FramePipeline::fallback_stats`] is how a front end says
    /// so out loud.
    both_eye_frames: u64,
    reconstructed_frames: u64,
    /// Whether the pose most recently composed from the geometric path was a
    /// reconstructed one.
    last_was_reconstructed: bool,
}

/// What the one-eye fallback has done this session.
///
/// `reconstructed` counts *frames that exist because of it*: on this hardware
/// they are frames that would otherwise have been dropped, so the ratio to
/// `both_eyes` is the measurement of how much the fallback is carrying. A
/// ratio that climbs towards parity is not a bug in this code — it is a
/// tracker that cannot see one of the user's eyes, and the fix for that is
/// physical (aim the tracker, raise the seat).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FallbackStats {
    /// Poses composed from two measured eye origins.
    pub both_eyes: u64,
    /// Poses composed from one eye plus the last measured offset.
    pub reconstructed: u64,
    /// True if the most recent pose was a reconstructed one.
    pub active: bool,
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
            pair: PairOffset::new(),
            both_eye_frames: 0,
            reconstructed_frames: 0,
            last_was_reconstructed: false,
        }
    }

    /// How many poses this session owes to the one-eye fallback.
    ///
    /// For a status line or a diagnostics report: a reconstructed pose is a
    /// guess, and a consumer has to be able to say so rather than present it
    /// as a measurement.
    pub fn fallback_stats(&self) -> FallbackStats {
        FallbackStats {
            both_eyes: self.both_eye_frames,
            reconstructed: self.reconstructed_frames,
            active: self.last_was_reconstructed,
        }
    }

    /// Change the smoothing strength without discarding anything else.
    ///
    /// Exists so a settings change does not have to mean a new pipeline: the
    /// neutral and the blink state have nothing to do with the filter, and
    /// re-taking the neutral mid-session silently recentres the user on
    /// whatever pose they happened to be holding.
    pub fn set_filter_alpha(&mut self, alpha: f64) {
        self.filter = PoseFilter::new(alpha);
    }

    /// Compose one frame.
    ///
    /// `pose_in` is the pose from the neural model when there is one. It wins
    /// over the geometric fallback, because the model contributes pitch and two
    /// eye origins cannot: `pose_from_sample` pins pitch at zero by
    /// construction. Passing `None` asks for the geometric pose, which is what
    /// a 5-DOF run gets.
    ///
    /// The geometric pose comes from [`PairOffset`], so a frame carrying only
    /// one tracked eye still produces one — with the missing eye placed at the
    /// last measured offset, and never past `RECONSTRUCTION_MAX_AGE`. With
    /// both eyes tracked it is bit for bit the two-eye geometry, unchanged.
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

        // Offered the sample even when the model's pose is going to win: the
        // offset between the eyes can only be measured on a two-eye frame, so
        // letting the model's presence skip this would leave the fallback with
        // nothing to reconstruct from the moment the model itself drops out.
        // It changes no output — `pose_in` still wins below.
        let geometric = self.pair.pose_from_sample(sample, now);
        if pose_in.is_none() {
            self.last_was_reconstructed =
                geometric.map(|g| g.source) == Some(PoseSource::Reconstructed);
            match geometric.map(|g| g.source) {
                Some(PoseSource::BothEyes) => {
                    self.both_eye_frames = self.both_eye_frames.saturating_add(1)
                }
                Some(PoseSource::Reconstructed) => {
                    self.reconstructed_frames = self.reconstructed_frames.saturating_add(1)
                }
                None => {}
            }
        }

        let pose = pose_in.or(geometric.map(|g| g.pose)).map(|raw| {
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
                // The offset between the eyes is state about *this* head in
                // *this* posture, so it goes with the rest. Its own
                // `RECONSTRUCTION_MAX_AGE` is far shorter than this, which
                // makes the reset belt-and-braces rather than load-bearing —
                // but leaving a measurement behind that the pipeline has
                // already declared a lie would be a trap for whoever shortens
                // one of the two bounds later.
                self.pair.reset();
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

        p.set_filter_alpha(0.9);
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

    /// The model's pose wins, and is not counted as geometry — but the sample
    /// still reaches the offset. Skipping it while a model was in charge would
    /// leave the fallback with nothing to reconstruct from at the moment the
    /// model itself dropped out, which is the moment it is needed.
    #[test]
    fn a_model_pose_wins_but_the_offset_is_still_measured_underneath_it() {
        let now = Instant::now();
        let c = cfg(false);
        let mut p = FramePipeline::new(&c);
        let supplied = HeadPose {
            pitch_deg: 17.0,
            ..Default::default()
        };
        let both = seated(680.0);
        let f = p.offer(&both, Some(supplied), &c, None, now);
        assert_eq!(f.pose.expect("a pose").pitch_deg, 17.0, "the model won");
        assert_eq!(
            p.fallback_stats(),
            FallbackStats::default(),
            "a model frame is not the geometric path's business"
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
}
