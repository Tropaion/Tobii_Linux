//! Curved-screen gaze correction.
//!
//! The device only accepts a **plane** (three corners, op `0x5a0`), so it
//! reports the 2D gaze point as the intersection of the gaze ray with that
//! plane. On a curved panel the point the user is physically looking at lies on
//! an **arc** through the same two side edges, bulging away from the viewer, so
//! the reported point is wrong — exactly at the edges (where plane and arc
//! meet) but off by centimetres in the mid-region. For a 49" 1800R panel at
//! ~700 mm the error peaks around 25–30 mm.
//!
//! Panel curvature is cylindrical about a **vertical** axis, so only the
//! horizontal coordinate is affected; the vertical one passes through
//! untouched. Everything here therefore works in the top-down (X–Z) plane.
//!
//! The correction is for *our* consumers only. It is deliberately **not**
//! applied to calibration stimulus points: during calibration the device's
//! plane model is the reference frame both sides agree on.

use crate::DisplaySetup;

/// Below this the eye origin is treated as unusable (all-zero / on the screen).
const MIN_EYE_DISTANCE_MM: f64 = 1.0;

/// A top-down 2D point `(x, z)` in tracker space.
type P2 = (f64, f64);

/// The screen arc in the top-down plane: centre of curvature, radius, the chord
/// end directions, and the half-angle the screen subtends.
struct Arc {
    centre: P2,
    radius: f64,
    /// Unit vector along the chord, left end → right end.
    along: P2,
    /// Unit vector from the centre of curvature toward the screen.
    toward: P2,
    /// Half the angle the screen subtends at the centre of curvature.
    half_angle: f64,
}

fn sub(a: P2, b: P2) -> P2 {
    (a.0 - b.0, a.1 - b.1)
}

fn dot(a: P2, b: P2) -> f64 {
    a.0 * b.0 + a.1 * b.1
}

fn norm(a: P2) -> f64 {
    dot(a, a).sqrt()
}

/// Build the screen arc for `setup`, given where the viewer is.
///
/// The chord endpoints come from [`DisplaySetup::to_corners`] so this can never
/// drift from the geometry we actually send the device. The arc's centre of
/// curvature sits on the **viewer's** side — a monitor curves *around* you —
/// which puts the screen's middle deeper than its edges.
fn screen_arc(setup: &DisplaySetup, eye: P2) -> Option<Arc> {
    let r = setup.curvature_radius_mm;
    if !r.is_finite() || r <= 0.0 || !(setup.width_mm.is_finite() && setup.width_mm > 0.0) {
        return None;
    }
    let c = setup.to_corners();
    // Top-down: x from the side edges, z at mid-height (tilt makes z vary with
    // height, and mid-height is the least-wrong single depth for the panel).
    let z = (c.bl[2] + c.tl[2]) / 2.0;
    let (left, right) = ((c.tl[0], z), (c.tr[0], z));
    let chord = sub(right, left);
    let half_w = norm(chord) / 2.0;
    // No circle of radius r passes through both endpoints if the chord exceeds
    // the diameter; and a chord equal to the diameter degenerates.
    if half_w < 1e-9 || half_w >= r {
        return None;
    }
    let along = (chord.0 / (2.0 * half_w), chord.1 / (2.0 * half_w));
    let mid = ((left.0 + right.0) / 2.0, (left.1 + right.1) / 2.0);
    // Chord normal (either way round); pick the sense pointing at the viewer.
    let mut normal = (-along.1, along.0);
    let to_eye = sub(eye, mid);
    if !to_eye.0.is_finite() || !to_eye.1.is_finite() || norm(to_eye) < MIN_EYE_DISTANCE_MM {
        return None;
    }
    if dot(normal, to_eye) < 0.0 {
        normal = (-normal.0, -normal.1);
    }
    // The eye must be genuinely off the screen plane for the ray cast to mean
    // anything; an eye sliding along the chord line gives no depth to work with.
    if dot(normal, to_eye).abs() < MIN_EYE_DISTANCE_MM {
        return None;
    }
    let sag_depth = (r * r - half_w * half_w).sqrt();
    let centre = (mid.0 + normal.0 * sag_depth, mid.1 + normal.1 * sag_depth);
    // The screen bulges away from the viewer, i.e. away from the centre.
    let toward = (-normal.0, -normal.1);
    Some(Arc {
        centre,
        radius: r,
        along,
        toward,
        half_angle: (half_w / r).asin(),
    })
}

/// Smallest strictly-positive `t` with `|origin + t·dir − centre| == radius`.
fn ray_circle_t(origin: P2, dir: P2, centre: P2, radius: f64) -> Option<f64> {
    let f = sub(origin, centre);
    let a = dot(dir, dir);
    if a < 1e-18 {
        return None;
    }
    let b = 2.0 * dot(f, dir);
    let c = dot(f, f) - radius * radius;
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return None; // ray misses the circle entirely
    }
    let sq = disc.sqrt();
    let (t1, t2) = ((-b - sq) / (2.0 * a), (-b + sq) / (2.0 * a));
    // The eye is normally *inside* the circle (the centre of curvature is
    // behind it), so t1 < 0 < t2 and the far root is the screen.
    [t1, t2]
        .into_iter()
        .filter(|t| *t > 1e-9 && t.is_finite())
        .fold(None, |acc: Option<f64>, t| {
            Some(acc.map_or(t, |m| m.min(t)))
        })
}

/// Correct a device-reported horizontal gaze coordinate for screen curvature.
///
/// `gaze_x` is the device's normalized 2D gaze x in `[0, 1]` (its intersection
/// of the gaze ray with the flat plane); `eye_origin_mm` is the viewer's eye
/// position in tracker-space mm (see `GazeSample::eye_origin_l_mm` /
/// `eye_origin_r_mm`, or their midpoint). Returns the normalized position of
/// the true gaze point measured **by arc length along the physical screen**, so
/// `0.0` and `1.0` stay the physical screen edges.
///
/// Returns `gaze_x` unchanged — never `NaN`, never panicking — when the screen
/// is flat (`curvature_radius_mm <= 0`), the geometry is degenerate, the eye
/// origin is unusable, or the ray misses the screen arc.
pub fn correct_gaze_x(gaze_x: f64, eye_origin_mm: [f64; 3], setup: &DisplaySetup) -> f64 {
    if !gaze_x.is_finite() {
        return gaze_x;
    }
    let gaze_x = gaze_x.clamp(0.0, 1.0);
    if !eye_origin_mm.iter().all(|v| v.is_finite()) {
        return gaze_x;
    }
    let eye: P2 = (eye_origin_mm[0], eye_origin_mm[2]);
    let Some(arc) = screen_arc(setup, eye) else {
        return gaze_x;
    };

    // The reported gaze identifies a point P on the chord: walk `gaze_x` of the
    // way along it from the left end. (Left end = centre + half-angle rotated
    // back; recover it from the arc frame rather than re-deriving corners.)
    let half_w = arc.radius * arc.half_angle.sin();
    let sag_depth = (arc.radius * arc.radius - half_w * half_w).sqrt();
    let mid = (
        arc.centre.0 + arc.toward.0 * sag_depth,
        arc.centre.1 + arc.toward.1 * sag_depth,
    );
    let off = (gaze_x - 0.5) * 2.0 * half_w;
    let p = (mid.0 + arc.along.0 * off, mid.1 + arc.along.1 * off);

    let dir = sub(p, eye);
    let Some(t) = ray_circle_t(eye, dir, arc.centre, arc.radius) else {
        return gaze_x;
    };
    let hit = (eye.0 + dir.0 * t, eye.1 + dir.1 * t);

    // Normalize by arc length: the angle of the hit about the centre of
    // curvature, measured from the screen's left end, over the total subtended
    // angle. Arc length is r·θ, so the angle fraction *is* the length fraction.
    let v = sub(hit, arc.centre);
    let theta = dot(v, arc.along).atan2(dot(v, arc.toward));
    if !theta.is_finite() || arc.half_angle < 1e-12 {
        return gaze_x;
    }
    let out = (theta + arc.half_angle) / (2.0 * arc.half_angle);
    if out.is_finite() {
        out.clamp(0.0, 1.0)
    } else {
        gaze_x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The user's real monitor: 49" 1800R, 1193 mm arc → 1171 mm chord.
    fn odyssey() -> DisplaySetup {
        DisplaySetup {
            width_mm: 1171.0,
            height_mm: 336.0,
            tilt_deg: 0.0,
            offset_x_mm: 0.0,
            offset_y_mm: 100.0,
            offset_z_mm: 0.0,
            curvature_radius_mm: 1800.0,
        }
    }

    fn flat() -> DisplaySetup {
        DisplaySetup {
            curvature_radius_mm: 0.0,
            ..odyssey()
        }
    }

    /// Eye 700 mm in front of the screen (viewer side is −Z), centred.
    const EYE: [f64; 3] = [0.0, 400.0, -700.0];

    #[test]
    fn flat_screen_is_the_identity_for_every_input() {
        let s = flat();
        for i in 0..=20 {
            let x = i as f64 / 20.0;
            assert_eq!(correct_gaze_x(x, EYE, &s), x);
        }
    }

    /// The sign that is easiest to get backwards: a monitor curves *around*
    /// the viewer, so the centre of curvature is on the viewer's side and the
    /// screen's middle sits deeper than its edges.
    #[test]
    fn screen_bulges_away_from_the_viewer() {
        let s = odyssey();
        let eye: P2 = (EYE[0], EYE[2]);
        let arc = screen_arc(&s, eye).expect("curved arc");
        // Centre of curvature is on the eye's side of the screen (z < 0).
        assert!(arc.centre.1 < 0.0, "centre={:?}", arc.centre);
        // The arc's mid-point is deeper (further from the eye) than the chord.
        let mid_arc = (
            arc.centre.0 + arc.toward.0 * arc.radius,
            arc.centre.1 + arc.toward.1 * arc.radius,
        );
        assert!(mid_arc.1 > 0.0, "mid_arc={mid_arc:?}");
        // ...by the sagitta, ~98 mm for this panel.
        assert!((mid_arc.1 - 98.0).abs() < 1.0, "sagitta={}", mid_arc.1);
        // And it is further from the eye than the chord's mid-point is.
        assert!(norm(sub(mid_arc, eye)) > 700.0);
    }

    /// Same conclusion, stated the other way: an eye on the +Z side puts the
    /// centre of curvature on the +Z side too.
    #[test]
    fn curvature_sign_follows_the_viewer_side() {
        let s = odyssey();
        let arc = screen_arc(&s, (0.0, 700.0)).expect("curved arc");
        assert!(arc.centre.1 > 0.0, "centre={:?}", arc.centre);
    }

    #[test]
    fn centre_stays_at_centre() {
        let out = correct_gaze_x(0.5, EYE, &odyssey());
        assert!((out - 0.5).abs() < 1e-9, "out={out}");
    }

    #[test]
    fn edges_stay_at_the_edges() {
        let s = odyssey();
        // The plane and the arc meet exactly at the side edges, so these are
        // fixed points of the correction.
        assert!(correct_gaze_x(0.0, EYE, &s).abs() < 1e-9);
        assert!((correct_gaze_x(1.0, EYE, &s) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn correction_is_symmetric_about_centre_for_a_centred_eye() {
        let s = odyssey();
        for i in 0..=10 {
            let x = i as f64 / 10.0;
            let l = correct_gaze_x(x, EYE, &s);
            let r = correct_gaze_x(1.0 - x, EYE, &s);
            assert!((l + r - 1.0).abs() < 1e-9, "x={x} l={l} r={r}");
        }
    }

    /// Real case, and the whole point of the feature: the physical gaze point
    /// is further *out* than the device reports, because the screen's middle
    /// region is deeper than the plane the device intersects.
    #[test]
    fn real_case_pushes_outward_by_a_plausible_amount() {
        let s = odyssey();
        let left = correct_gaze_x(0.25, EYE, &s);
        let right = correct_gaze_x(0.75, EYE, &s);
        assert!(left < 0.25, "left={left}");
        assert!(right > 0.75, "right={right}");
        // Magnitude in mm of screen: a couple of centimetres, matching what the
        // user observes. Sanity-bracket it rather than pinning the exact value.
        let mm = (0.25 - left) * 1193.0;
        assert!((10.0..45.0).contains(&mm), "shift={mm} mm");
        assert!(((0.25 - left) - (right - 0.75)).abs() < 1e-9);
    }

    #[test]
    fn output_is_always_within_unit_range() {
        let s = odyssey();
        for eye in [
            [0.0, 400.0, -700.0],
            [-500.0, 300.0, -400.0],
            [600.0, 900.0, -1500.0],
            [0.0, 0.0, -60.0],
        ] {
            for i in -5..=25 {
                let x = i as f64 / 20.0;
                let out = correct_gaze_x(x, eye, &s);
                assert!((0.0..=1.0).contains(&out), "x={x} eye={eye:?} out={out}");
            }
        }
    }

    #[test]
    fn garbage_inputs_return_something_sane() {
        let s = odyssey();
        // Eye origin never populated (all zero) — unusable, so pass through.
        assert_eq!(correct_gaze_x(0.25, [0.0; 3], &s), 0.25);
        // Non-finite eye origin.
        assert_eq!(correct_gaze_x(0.25, [f64::NAN, 0.0, -700.0], &s), 0.25);
        assert_eq!(correct_gaze_x(0.25, [0.0, 0.0, f64::INFINITY], &s), 0.25);
        // Non-finite gaze.
        assert!(correct_gaze_x(f64::NAN, EYE, &s).is_nan());
        // Out-of-range gaze is clamped, not extrapolated.
        assert!((0.0..=1.0).contains(&correct_gaze_x(-3.0, EYE, &s)));
        assert!((0.0..=1.0).contains(&correct_gaze_x(9.0, EYE, &s)));
        // Zero / negative width.
        let zero_w = DisplaySetup {
            width_mm: 0.0,
            ..odyssey()
        };
        assert_eq!(correct_gaze_x(0.25, EYE, &zero_w), 0.25);
        let neg_w = DisplaySetup {
            width_mm: -500.0,
            ..odyssey()
        };
        assert_eq!(correct_gaze_x(0.25, EYE, &neg_w), 0.25);
        // Radius smaller than half the width: no such circle.
        let tight = DisplaySetup {
            curvature_radius_mm: 100.0,
            ..odyssey()
        };
        assert_eq!(correct_gaze_x(0.25, EYE, &tight), 0.25);
        // Absurd radius: finite, and indistinguishable from flat.
        let huge = DisplaySetup {
            curvature_radius_mm: 1e12,
            ..odyssey()
        };
        let out = correct_gaze_x(0.25, EYE, &huge);
        assert!(out.is_finite() && (out - 0.25).abs() < 1e-6, "out={out}");
        // Negative / NaN radius is flat.
        for r in [-1800.0, f64::NAN, f64::INFINITY] {
            let bad = DisplaySetup {
                curvature_radius_mm: r,
                ..odyssey()
            };
            assert_eq!(correct_gaze_x(0.25, EYE, &bad), 0.25);
        }
    }

    /// An eye *behind* the screen is nonsense, but must still be well-behaved:
    /// the arc simply flips to that side and the answer stays in range.
    #[test]
    fn eye_behind_the_screen_stays_finite_and_in_range() {
        let s = odyssey();
        for x in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let out = correct_gaze_x(x, [0.0, 400.0, 800.0], &s);
            assert!(out.is_finite() && (0.0..=1.0).contains(&out), "out={out}");
        }
        // An eye sitting essentially *on* the screen plane has no usable depth.
        assert_eq!(correct_gaze_x(0.25, [0.0, 400.0, 0.0], &s), 0.25);
    }

    /// Correction still behaves with a tilted panel (tilt only shifts the
    /// top-down depth we work at; it must not break the fixed points).
    #[test]
    fn tilt_does_not_break_the_fixed_points() {
        let s = DisplaySetup {
            tilt_deg: 20.0,
            ..odyssey()
        };
        assert!(correct_gaze_x(0.0, EYE, &s).abs() < 1e-9);
        assert!((correct_gaze_x(1.0, EYE, &s) - 1.0).abs() < 1e-9);
        assert!((correct_gaze_x(0.5, EYE, &s) - 0.5).abs() < 1e-9);
    }

    /// An off-centre viewer sees an asymmetric correction — the near half of
    /// the screen is compressed relative to the far half.
    #[test]
    fn off_centre_eye_gives_an_asymmetric_correction() {
        let s = odyssey();
        let eye = [-400.0, 400.0, -700.0];
        let l = correct_gaze_x(0.25, eye, &s);
        let r = correct_gaze_x(0.75, eye, &s);
        assert!(l.is_finite() && r.is_finite());
        assert!(((0.25 - l) - (r - 0.75)).abs() > 1e-6, "l={l} r={r}");
    }
}
