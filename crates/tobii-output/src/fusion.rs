//! Extended View: letting gaze steer the camera, not just the head.
//!
//! The ET5's geometric head pose is 5-DOF — pitch is structurally unrecoverable
//! from two eye origins, so it is hardcoded to zero (see `tobii-headpose`). A
//! game driven by that alone cannot look up or down at all, which in a cockpit
//! is most of what head tracking is for.
//!
//! Extended View fills the gap with the signal the device is actually good at.
//! Where you are looking on screen becomes an angular offset added to the head
//! pose, so glancing at the top of the screen tips the view up. This is also
//! the only way gaze reaches a game that speaks nothing but head tracking —
//! TrackIR, FreeTrack and opentrack have no gaze channel at all.
//!
//! # Coordinate conventions, and the one that bites
//!
//! Two conventions meet here and they do **not** agree about `+z`:
//!
//! * `tobii-headpose` positions (the eye midpoint) are tracker-space mm with
//!   **+z toward the user** — an eye sits at `z ≈ 680`.
//! * [`DisplayCorners`] from `DisplaySetup::to_corners` put the screen plane at
//!   `z ≈ 0` (the tracker is mounted on the bottom bezel).
//!
//! Both are the same frame; the screen is simply near the origin and the user
//! is out at +z. So the direction *from the eye to the screen* has a *negative*
//! z, and *forward* is `-z`. Every angle here is therefore taken against
//! `-d.z`, never `+d.z`. Getting this backwards does not produce a small error:
//! it produces angles near ±180°, i.e. a view that flips instead of turning.
//!
//! Gaze is in normalized display coordinates with **+y down** (`overlay.rs`
//! draws `gy * height` directly), while tracker `+y` is up. The bilinear
//! corner blend below flips it exactly once, which is why the top edge must
//! come out as positive pitch — pinned by a test.

use tobii_headpose::HeadPose;
use tobii_protocol::DisplayCorners;

/// Minimum forward distance, in mm, for an angle to mean anything.
///
/// Below this the user's eye is level with or behind the screen plane, which is
/// either nonsense data or someone leaning past their monitor. Mirrors the
/// `eye[2].abs() < 1.0` guard in the GUI's accuracy screen, and for the same
/// reason: an angle computed against a degenerate distance is confidently wrong.
const MIN_FORWARD_MM: f64 = 1.0;

/// How an axis's raw gaze angle maps onto output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Curve {
    /// Output rises linearly with the gaze angle.
    Linear,
    /// `u.powf(p)` — `p > 1` keeps small glances gentle and makes the response
    /// build toward the edge of the screen.
    Power(f64),
    /// `3u² − 2u³`: eases in *and* out, so the response also flattens as it
    /// saturates.
    Smoothstep,
}

impl Curve {
    /// Apply the curve to a normalized `[0,1]` input.
    ///
    /// A nonsensical exponent degrades to [`Curve::Linear`] rather than
    /// producing NaN or an inverted response: a bad config value should feel
    /// wrong, not break the pipeline.
    fn apply(self, u: f64) -> f64 {
        match self {
            Curve::Linear => u,
            Curve::Power(p) if p.is_finite() && p > 0.0 => u.powf(p),
            Curve::Power(_) => u,
            Curve::Smoothstep => u * u * (3.0 - 2.0 * u),
        }
    }

    /// Parse the config-file spelling: `linear`, `smoothstep`, or `power:1.5`.
    pub fn parse(s: &str) -> Option<Curve> {
        let s = s.trim();
        match s {
            "linear" => Some(Curve::Linear),
            "smoothstep" => Some(Curve::Smoothstep),
            _ => {
                let p = s.strip_prefix("power:")?.trim().parse::<f64>().ok()?;
                (p.is_finite() && p > 0.0).then_some(Curve::Power(p))
            }
        }
    }

    /// The config-file spelling, round-tripping with [`Curve::parse`].
    pub fn to_config_string(self) -> String {
        match self {
            Curve::Linear => "linear".to_string(),
            Curve::Smoothstep => "smoothstep".to_string(),
            Curve::Power(p) => format!("power:{p}"),
        }
    }
}

/// The response shaping for one axis.
///
/// Deliberately a **range mapping** rather than a bare gain multiplier. The
/// clamp then falls out of the mapping instead of being bolted on, and tuning
/// becomes the question users actually ask — "how far does the view swing when
/// I look at the edge of the screen" — rather than an abstract multiplier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxisResponse {
    /// Gaze angles smaller than this produce exactly zero output, so reading
    /// text near the centre does not stir the camera.
    pub deadzone_deg: f64,
    /// The gaze angle mapped to full output; beyond it the response saturates.
    pub input_max_deg: f64,
    /// Output at `input_max_deg` — the user-facing "strength".
    pub output_max_deg: f64,
    /// Shape of the curve between the deadzone and saturation.
    pub curve: Curve,
    /// Hard ceiling on `|output|`, which also caps a mis-set `output_max_deg`.
    pub clamp_deg: f64,
}

impl AxisResponse {
    /// Map a raw gaze angle (degrees from the reference direction) to output.
    ///
    /// Odd-symmetric by construction: the sign is taken out before shaping and
    /// put back afterwards, so looking left and right respond identically.
    pub fn shape(&self, raw_deg: f64) -> f64 {
        if !raw_deg.is_finite() {
            return 0.0;
        }
        let magnitude = raw_deg.abs();
        let deadzone = self.deadzone_deg.max(0.0);
        if magnitude <= deadzone {
            return 0.0;
        }
        let span = self.input_max_deg - deadzone;
        // A span at or below zero means "everything past the deadzone is full
        // output". Saturating beats dividing by zero, and it keeps a misordered
        // config responsive rather than silently dead.
        let u = if span > 0.0 {
            ((magnitude - deadzone) / span).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let out = raw_deg.signum() * self.curve.apply(u) * self.output_max_deg;
        let ceiling = self.clamp_deg.abs();
        out.clamp(-ceiling, ceiling)
    }
}

/// Gaze-driven camera offset settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExtendedView {
    /// Whether gaze contributes at all.
    pub enabled: bool,
    /// Horizontal response.
    pub yaw: AxisResponse,
    /// Vertical response. This axis *is* the pose's pitch — the geometric head
    /// pose contributes a constant zero there.
    pub pitch: AxisResponse,
    /// How long the last offset is held through a blink before decaying to
    /// zero, in milliseconds.
    pub hold_ms: u64,
}

impl Default for ExtendedView {
    /// Conservative defaults: obviously-correct rather than exciting.
    ///
    /// The two axes differ because a screen subtends far more horizontal than
    /// vertical angle, and both `input_max` values sit inside the measured ~28°
    /// of gaze the ET5 actually tracks usefully (`tobii_config::USABLE_GAZE_DEG`)
    /// — asking for response past that is asking for response from noise.
    ///
    /// **On a wide screen this saturates before the edge, deliberately.** For
    /// the 1193 × 336 mm panel these were tuned against, at a 680 mm viewing
    /// distance, the screen's own edges sit at about ±44° horizontally and
    /// +15°/−12° vertically. A 25° yaw `input_max` therefore reaches full output
    /// at roughly 58% of the way to the left or right edge, and the outer
    /// stretch adds nothing further. That is the intended trade: the tracker
    /// cannot follow gaze reliably out there anyway, so the alternative is not
    /// more range but noisier range. Anyone who wants the full sweep can raise
    /// `input_max_deg` toward their own edge angle in `[games]`.
    fn default() -> Self {
        ExtendedView {
            enabled: true,
            yaw: AxisResponse {
                deadzone_deg: 2.0,
                input_max_deg: 25.0,
                output_max_deg: 45.0,
                curve: Curve::Power(1.5),
                clamp_deg: 60.0,
            },
            pitch: AxisResponse {
                deadzone_deg: 2.0,
                input_max_deg: 12.0,
                output_max_deg: 25.0,
                curve: Curve::Power(1.5),
                clamp_deg: 35.0,
            },
            hold_ms: 200,
        }
    }
}

/// `a - b`, componentwise.
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// The point on the display plane at normalized gaze coordinates `(gx, gy)`.
///
/// Bilinear across the three known corners, so screen tilt and every offset are
/// honoured exactly rather than approximated by a flat rectangle. `gy` is
/// flipped here — and only here — because gaze coordinates put `+y` down while
/// the corners are in tracker space, where `+y` is up.
pub fn point_on_screen(c: &DisplayCorners, gaze: [f64; 2]) -> [f64; 3] {
    let across = sub(c.tr, c.tl);
    let up = sub(c.tl, c.bl);
    let (gx, gy) = (gaze[0], gaze[1]);
    [
        c.bl[0] + gx * across[0] + (1.0 - gy) * up[0],
        c.bl[1] + gx * across[1] + (1.0 - gy) * up[1],
        c.bl[2] + gx * across[2] + (1.0 - gy) * up[2],
    ]
}

/// Raw gaze angles in degrees, relative to looking at the centre of the screen.
///
/// Returns `(yaw, pitch)`: `+yaw` is gaze to the user's right, `+pitch` is gaze
/// up — matching the sign conventions `pose_from_eyes` uses, so the two can
/// simply be added.
///
/// Angles are measured **against the screen centre**, not against the tracker's
/// axis. The tracker sits below the screen, so an absolute angle would report a
/// permanent ~20° of upward pitch just for looking at the middle of the monitor;
/// subtracting the centre direction makes "looking at the middle" contribute
/// exactly zero, with no runtime state and no recentre hotkey.
///
/// `None` when the geometry is degenerate — the eye level with or behind the
/// screen plane, or non-finite input.
pub fn gaze_angles_deg(c: &DisplayCorners, eye_mm: [f64; 3], gaze: [f64; 2]) -> Option<(f64, f64)> {
    if !eye_mm.iter().all(|v| v.is_finite()) || !gaze.iter().all(|v| v.is_finite()) {
        return None;
    }
    if ![c.tl, c.tr, c.bl]
        .iter()
        .all(|p| p.iter().all(|v| v.is_finite()))
    {
        return None;
    }
    let target = point_on_screen(c, gaze);
    let centre = point_on_screen(c, [0.5, 0.5]);
    let d = sub(target, eye_mm);
    let r = sub(centre, eye_mm);

    // Forward is -z: the screen sits near the tracker at z ~ 0 and the user is
    // out at z ~ +680, so the eye-to-screen vector points back toward -z.
    let (d_fwd, r_fwd) = (-d[2], -r[2]);
    if d_fwd < MIN_FORWARD_MM || r_fwd < MIN_FORWARD_MM {
        return None;
    }
    let yaw = d[0].atan2(d_fwd).to_degrees() - r[0].atan2(r_fwd).to_degrees();
    let pitch = d[1].atan2(d_fwd).to_degrees() - r[1].atan2(r_fwd).to_degrees();
    (yaw.is_finite() && pitch.is_finite()).then_some((yaw, pitch))
}

/// The shaped `(yaw, pitch)` offset gaze should contribute, in degrees.
///
/// `(0.0, 0.0)` when disabled or when the geometry does not support an answer —
/// never NaN, so a degenerate frame costs the user a steady camera rather than
/// a poisoned one.
pub fn extended_view(
    ev: &ExtendedView,
    c: &DisplayCorners,
    eye_mm: [f64; 3],
    gaze: [f64; 2],
) -> (f64, f64) {
    if !ev.enabled {
        return (0.0, 0.0);
    }
    match gaze_angles_deg(c, eye_mm, gaze) {
        Some((raw_yaw, raw_pitch)) => (ev.yaw.shape(raw_yaw), ev.pitch.shape(raw_pitch)),
        None => (0.0, 0.0),
    }
}

/// What to contribute while gaze is unavailable — a blink, or a glance away.
///
/// Holds the last good offset for `hold_ms` and then returns zero, letting the
/// downstream smoothing filter glide back to the head-only pose. Never snaps:
/// a blink is a handful of frames, and dropping the offset instantly would jolt
/// the camera several times a minute.
///
/// `lost_for_ms` is passed in rather than read from a clock, so the policy is
/// testable without sleeping.
pub fn hold_through_blink(
    ev: &ExtendedView,
    last_valid: Option<(f64, f64)>,
    lost_for_ms: u64,
) -> (f64, f64) {
    match last_valid {
        Some(last) if lost_for_ms < ev.hold_ms => last,
        _ => (0.0, 0.0),
    }
}

/// Add a gaze offset to a head pose.
///
/// Only the two rotation axes gaze can speak to are touched. Roll and position
/// pass through bit-identically: gaze says nothing about either, and inventing a
/// contribution would be making data up.
pub fn compose(head: HeadPose, ev_yaw_deg: f64, ev_pitch_deg: f64) -> HeadPose {
    HeadPose {
        yaw_deg: head.yaw_deg + ev_yaw_deg,
        pitch_deg: head.pitch_deg + ev_pitch_deg,
        ..head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen roughly like the one this was developed against: 1193 mm wide,
    /// 336 mm tall, tilted 20° back, bottom edge 42 mm above the tracker.
    fn screen() -> DisplayCorners {
        let (w, h, tilt) = (1193.0_f64, 336.0_f64, 20.0_f64.to_radians());
        let (dy, dz) = (h * tilt.cos(), h * tilt.sin());
        DisplayCorners {
            bl: [-w / 2.0, 42.0, 0.0],
            tl: [-w / 2.0, 42.0 + dy, dz],
            tr: [w / 2.0, 42.0 + dy, dz],
        }
    }

    /// A flat, untilted screen — the easy case, for tests about signs rather
    /// than about geometry.
    fn flat_screen() -> DisplayCorners {
        DisplayCorners {
            bl: [-500.0, 0.0, 0.0],
            tl: [-500.0, 300.0, 0.0],
            tr: [500.0, 300.0, 0.0],
        }
    }

    /// An eye 680 mm in front of the tracker, roughly screen-centre height.
    fn eye() -> [f64; 3] {
        [0.0, 200.0, 680.0]
    }

    const CENTRE: [f64; 2] = [0.5, 0.5];

    /// The zero reference. If this drifts, every axis acquires a constant
    /// offset and the camera sits permanently off-centre while the user stares
    /// straight ahead — the tracker being *below* the screen makes this the
    /// natural failure, not a hypothetical one.
    #[test]
    fn looking_at_the_centre_contributes_exactly_nothing() {
        for c in [screen(), flat_screen()] {
            for e in [eye(), [50.0, 120.0, 500.0], [-80.0, 260.0, 900.0]] {
                let (yaw, pitch) = gaze_angles_deg(&c, e, CENTRE).expect("valid geometry");
                assert!(yaw.abs() < 1e-9, "yaw {yaw} at eye {e:?}");
                assert!(pitch.abs() < 1e-9, "pitch {pitch} at eye {e:?}");
            }
        }
    }

    #[test]
    fn looking_right_is_positive_yaw_and_left_is_negative() {
        let (right, _) = gaze_angles_deg(&screen(), eye(), [0.9, 0.5]).expect("valid");
        let (left, _) = gaze_angles_deg(&screen(), eye(), [0.1, 0.5]).expect("valid");
        assert!(right > 0.0, "right edge must be +yaw, got {right}");
        assert!(left < 0.0, "left edge must be -yaw, got {left}");
    }

    /// Gaze `+y` is **down** but tracker `+y` is **up**, and the flip happens
    /// exactly once. If it happened zero times or twice, looking up would tip
    /// the view down — the single most likely bug in this whole feature.
    #[test]
    fn looking_at_the_top_of_the_screen_is_positive_pitch() {
        let (_, top) = gaze_angles_deg(&screen(), eye(), [0.5, 0.0]).expect("valid");
        let (_, bottom) = gaze_angles_deg(&screen(), eye(), [0.5, 1.0]).expect("valid");
        assert!(top > 0.0, "top edge (gy=0) must be +pitch, got {top}");
        assert!(
            bottom < 0.0,
            "bottom edge (gy=1) must be -pitch, got {bottom}"
        );
    }

    /// Angles must be sane magnitudes. Reading `+d.z` instead of `-d.z` as
    /// forward yields values near ±180° rather than a few tens of degrees, so
    /// this catches the convention being flipped.
    #[test]
    fn edge_angles_are_tens_of_degrees_not_near_180() {
        let (yaw, _) = gaze_angles_deg(&screen(), eye(), [1.0, 0.5]).expect("valid");
        assert!(
            (5.0..80.0).contains(&yaw),
            "a screen edge should be a few tens of degrees away, got {yaw}"
        );
    }

    #[test]
    fn mirrored_gaze_gives_exactly_negated_yaw() {
        let c = flat_screen();
        let e = [0.0, 150.0, 700.0];
        let (right, _) = gaze_angles_deg(&c, e, [0.75, 0.5]).expect("valid");
        let (left, _) = gaze_angles_deg(&c, e, [0.25, 0.5]).expect("valid");
        assert!((right + left).abs() < 1e-9, "{right} vs {left}");
    }

    /// Proves the geometry uses the real measured distance rather than a baked-in
    /// scale: the same point on screen subtends a smaller angle from further away.
    #[test]
    fn the_same_gaze_point_subtends_less_angle_from_further_back() {
        let c = screen();
        let (near, _) = gaze_angles_deg(&c, [0.0, 200.0, 400.0], [0.9, 0.5]).expect("valid");
        let (far, _) = gaze_angles_deg(&c, [0.0, 200.0, 800.0], [0.9, 0.5]).expect("valid");
        assert!(near > far, "near {near} should exceed far {far}");
        assert!(far > 0.0);
    }

    #[test]
    fn inside_the_deadzone_is_exactly_zero() {
        let r = ExtendedView::default().yaw;
        for e in [0.0, 0.5, 1.0, 1.999, -1.999, -0.5] {
            assert_eq!(r.shape(e), 0.0, "{e}° must be inside the deadzone");
        }
    }

    /// Continuous at the boundary — no step. A discontinuity here reads in game
    /// as the camera twitching whenever the gaze hovers near the threshold.
    #[test]
    fn the_deadzone_edge_is_continuous_but_the_axis_still_responds() {
        let r = ExtendedView::default().yaw;
        let dz = r.deadzone_deg;
        assert!(
            r.shape(dz + 1e-9).abs() < 1e-9,
            "output must approach zero at the deadzone edge"
        );
        assert!(
            r.shape(dz + 5.0) > 0.0,
            "the axis must still respond past the deadzone"
        );
    }

    #[test]
    fn saturation_holds_at_output_max() {
        let r = ExtendedView::default().yaw;
        let at_max = r.shape(r.input_max_deg);
        assert!((at_max - r.output_max_deg).abs() < 1e-9, "got {at_max}");
        assert!((r.shape(r.input_max_deg * 2.0) - r.output_max_deg).abs() < 1e-9);
        assert!((r.shape(1e6) - r.output_max_deg).abs() < 1e-9);
    }

    #[test]
    fn the_clamp_caps_a_misconfigured_output_max() {
        let r = AxisResponse {
            deadzone_deg: 0.0,
            input_max_deg: 10.0,
            output_max_deg: 500.0,
            curve: Curve::Linear,
            clamp_deg: 60.0,
        };
        assert_eq!(r.shape(10.0), 60.0);
        assert_eq!(r.shape(-10.0), -60.0);
    }

    #[test]
    fn the_response_is_odd_symmetric() {
        let r = ExtendedView::default().yaw;
        for e in [0.1, 2.5, 7.0, 25.0, 100.0] {
            assert_eq!(r.shape(-e), -r.shape(e), "asymmetric at {e}°");
        }
    }

    #[test]
    fn power_one_is_identical_to_linear() {
        let base = ExtendedView::default().yaw;
        let lin = AxisResponse {
            curve: Curve::Linear,
            ..base
        };
        let pow = AxisResponse {
            curve: Curve::Power(1.0),
            ..base
        };
        for i in 0..=100 {
            let e = i as f64 * 0.4;
            assert_eq!(lin.shape(e), pow.shape(e), "differ at {e}°");
        }
    }

    #[test]
    fn every_curve_is_monotonic_over_its_range() {
        let base = ExtendedView::default().yaw;
        for curve in [Curve::Linear, Curve::Power(1.5), Curve::Smoothstep] {
            let r = AxisResponse { curve, ..base };
            let mut prev = f64::NEG_INFINITY;
            for i in 0..=100 {
                let out = r.shape(i as f64 * 0.3);
                assert!(out >= prev - 1e-12, "{curve:?} dipped at step {i}");
                prev = out;
            }
        }
    }

    /// A misordered or nonsensical config must stay responsive rather than
    /// silently emitting nothing, which is indistinguishable from a dead tracker.
    #[test]
    fn a_degenerate_span_saturates_instead_of_dividing_by_zero() {
        let r = AxisResponse {
            deadzone_deg: 10.0,
            input_max_deg: 5.0, // below the deadzone: nonsense
            output_max_deg: 20.0,
            curve: Curve::Linear,
            clamp_deg: 60.0,
        };
        assert_eq!(r.shape(4.0), 0.0, "still inside the deadzone");
        assert_eq!(r.shape(12.0), 20.0, "past it, saturate");
        assert!(r.shape(12.0).is_finite());
    }

    #[test]
    fn a_nonsense_exponent_degrades_to_linear() {
        let base = ExtendedView::default().yaw;
        let lin = AxisResponse {
            curve: Curve::Linear,
            ..base
        };
        for bad in [0.0, -2.0, f64::NAN, f64::INFINITY] {
            let r = AxisResponse {
                curve: Curve::Power(bad),
                ..base
            };
            assert_eq!(r.shape(10.0), lin.shape(10.0), "exponent {bad}");
        }
    }

    /// Hostile geometry must cost a steady camera, never a NaN one: NaN fed
    /// into a game's camera maths tends to take the whole view with it.
    #[test]
    fn degenerate_geometry_yields_zero_and_never_nan() {
        let ev = ExtendedView::default();
        let c = screen();
        let cases: [(&str, DisplayCorners, [f64; 3], [f64; 2]); 6] = [
            ("eye at the screen plane", c, [0.0, 200.0, 0.0], CENTRE),
            ("eye behind the screen", c, [0.0, 200.0, -100.0], CENTRE),
            ("NaN eye", c, [f64::NAN, 200.0, 680.0], CENTRE),
            ("NaN gaze", c, eye(), [f64::NAN, 0.5]),
            ("infinite gaze", c, eye(), [f64::INFINITY, 0.5]),
            (
                "collapsed corners",
                DisplayCorners {
                    tl: [0.0; 3],
                    tr: [0.0; 3],
                    bl: [0.0; 3],
                },
                eye(),
                CENTRE,
            ),
        ];
        for (name, corners, e, g) in cases {
            let (yaw, pitch) = extended_view(&ev, &corners, e, g);
            assert_eq!((yaw, pitch), (0.0, 0.0), "{name} must contribute nothing");
            assert!(yaw.is_finite() && pitch.is_finite(), "{name} produced NaN");
        }
    }

    #[test]
    fn disabling_extended_view_contributes_nothing() {
        let ev = ExtendedView {
            enabled: false,
            ..ExtendedView::default()
        };
        assert_eq!(
            extended_view(&ev, &screen(), eye(), [0.95, 0.05]),
            (0.0, 0.0)
        );
    }

    /// Extended View *is* the pitch axis, since the geometric pose contributes a
    /// constant zero there — so composition must actually move it.
    #[test]
    fn composition_touches_only_yaw_and_pitch() {
        let head = HeadPose {
            x_mm: -12.5,
            y_mm: 33.25,
            z_mm: 681.0,
            yaw_deg: -7.5,
            pitch_deg: 0.0,
            roll_deg: 4.25,
        };
        let out = compose(head, 10.0, -3.0);
        assert_eq!(out.yaw_deg, -7.5 + 10.0);
        assert_eq!(out.pitch_deg, -3.0, "pitch comes entirely from gaze");
        assert_eq!(out.roll_deg, head.roll_deg, "roll must pass through");
        assert_eq!(out.x_mm, head.x_mm);
        assert_eq!(out.y_mm, head.y_mm);
        assert_eq!(out.z_mm, head.z_mm);
    }

    #[test]
    fn a_blink_holds_the_last_offset_then_decays_to_zero() {
        let ev = ExtendedView::default(); // hold_ms = 200
        let last = Some((12.0, -4.0));
        assert_eq!(hold_through_blink(&ev, last, 0), (12.0, -4.0));
        assert_eq!(hold_through_blink(&ev, last, 199), (12.0, -4.0));
        assert_eq!(hold_through_blink(&ev, last, 200), (0.0, 0.0));
        assert_eq!(hold_through_blink(&ev, last, 5_000), (0.0, 0.0));
        assert_eq!(hold_through_blink(&ev, None, 0), (0.0, 0.0));
    }

    #[test]
    fn curves_round_trip_through_their_config_spelling() {
        for c in [Curve::Linear, Curve::Smoothstep, Curve::Power(1.5)] {
            assert_eq!(Curve::parse(&c.to_config_string()), Some(c));
        }
        assert_eq!(Curve::parse("  linear "), Some(Curve::Linear));
        assert_eq!(Curve::parse("power:2"), Some(Curve::Power(2.0)));
        for bad in [
            "",
            "quadratic",
            "power:",
            "power:abc",
            "power:0",
            "power:-1",
        ] {
            assert_eq!(Curve::parse(bad), None, "`{bad}` must be rejected");
        }
    }

    /// End to end at the defaults: a glance at the screen edge should produce a
    /// usefully large swing, and it must stay inside the clamp.
    #[test]
    fn a_glance_at_the_edge_produces_a_usable_swing() {
        let ev = ExtendedView::default();
        let (yaw, _) = extended_view(&ev, &screen(), eye(), [1.0, 0.5]);
        assert!(
            yaw > 10.0 && yaw <= ev.yaw.clamp_deg,
            "edge glance gave {yaw}°, want a real swing within the clamp"
        );
        let (_, pitch) = extended_view(&ev, &screen(), eye(), [0.5, 0.0]);
        assert!(
            pitch > 2.0 && pitch <= ev.pitch.clamp_deg,
            "top-edge glance gave {pitch}°"
        );
    }
}
