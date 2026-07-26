//! Pure gaze-focus-detection logic for the calibration flow.
//!
//! Reimplements (clean-room — algorithm shape only, no decompiled source
//! copied) the decompiled Windows Tobii software's
//! `CalibrationProcessViewModel` focus-detection geometry: a "zone radius"
//! around each not-yet-calibrated point (half the closest inter-point
//! spacing, shrunk by [`RADIUS_SIZE_MODIFIER`]), and a "closest point within
//! that radius" check against the live gaze position, considering only
//! points not yet calibrated. A later task uses this to trigger point
//! capture on verified gaze rather than a blind timer.
//!
//! Deviation from the original: the original computes zone radii and
//! gaze-to-point distances in true screen pixels, so a non-square screen
//! never distorts the geometry. This module instead works in normalized
//! `[0,1]` display coordinates (matching the point sets in
//! `calibrate_flow.rs`, e.g. `FULL_7`) — simpler, and needs no pixel
//! dimensions — but a raw Euclidean distance in normalized space IS
//! distorted on a non-square screen (a 0.1 x-distance covers more physical
//! screen than a 0.1 y-distance on a 16:9 display). [`aspect_corrected_dist`]
//! corrects for this by scaling the x-component by `aspect` (screen
//! width/height) before computing the Euclidean distance, and both
//! [`zone_radius`] and [`closest_focused_point`] use it consistently so the
//! radius and the containment check always agree on the same metric.

/// Zone-radius shrink factor (matches the decompiled original's
/// `RadiusSizeModifier` exactly): the proximity radius around each
/// not-yet-calibrated point is half the closest inter-point spacing, shrunk
/// by this factor so adjacent zones never overlap.
pub const RADIUS_SIZE_MODIFIER: f64 = 0.45;

/// Debounce for accepting a CHANGE of focus target (not needed to re-confirm
/// staying on the same target) — matches the original's ~150ms
/// `WouldItBeSuitableToChange` debounce, expressed in ticks at this app's
/// 33ms/tick cadence (150/33 ≈ 4.5, round to 5).
///
/// Currently unused by `calibrate_flow.rs`: that flow presents one
/// calibration dot at a time, so its `calibrated_mask` excludes every point
/// except the one currently on screen, and [`closest_focused_point`] can only
/// ever return `Some(index)` (that one live target) or `None` — there is no
/// "different point" for gaze to change TO, so no change-of-target to
/// debounce. `GAZE_GAP_TOLERANCE_TICKS` in `calibrate_flow.rs` instead covers
/// the analogous noise-tolerance need for a single target (a brief gaze gap
/// while still on the one dot). This constant stays public and tested for a
/// future UI that shows several calibration targets simultaneously (as the
/// decompiled original does), where a real change-of-target debounce would
/// apply.
pub const FOCUS_CHANGE_DEBOUNCE_TICKS: u32 = 5;

/// Aspect-ratio-corrected Euclidean distance between two normalized `[0,1]`
/// points. Scales the x-delta by `aspect` (screen width/height) before
/// squaring, so distances compare fairly on a non-square screen instead of
/// being distorted toward whichever axis is physically shorter. See the
/// module doc for why this replaces the original's pixel-space distance.
fn aspect_corrected_dist(a: (f64, f64), b: (f64, f64), aspect: f64) -> f64 {
    let dx = (a.0 - b.0) * aspect;
    let dy = a.1 - b.1;
    (dx * dx + dy * dy).sqrt()
}

/// The proximity radius (normalized units, aspect-ratio-corrected so a
/// non-square screen's x/y distances compare fairly) within which a live
/// gaze point counts as "on" a calibration point.
///
/// Computed as half the minimum aspect-corrected distance between any two
/// distinct points in `points`, shrunk by [`RADIUS_SIZE_MODIFIER`] so
/// adjacent zones never overlap.
///
/// Fewer than 2 points has no meaningful "closest pair", so this returns
/// `f64::MAX` as a defensive fallback — no real caller in this app passes
/// fewer than 2 points (calibration point sets always have several).
pub fn zone_radius(points: &[(f64, f64)], aspect: f64) -> f64 {
    if points.len() < 2 {
        return f64::MAX;
    }
    let mut min_dist = f64::MAX;
    for i in 0..points.len() {
        for j in (i + 1)..points.len() {
            let d = aspect_corrected_dist(points[i], points[j], aspect);
            if d < min_dist {
                min_dist = d;
            }
        }
    }
    (min_dist / 2.0) * RADIUS_SIZE_MODIFIER
}

/// Index of the closest not-yet-calibrated point within `radius` of `gaze`,
/// or `None` if no such point qualifies.
///
/// Only indices `i` where `calibrated[i]` is `false` are considered; among
/// those, the one with the smallest aspect-corrected distance from `gaze`
/// wins, provided that distance is strictly less than `radius`.
///
/// Assumes `calibrated.len() == points.len()`; callers that violate this
/// simply get fewer points considered (via `zip`), rather than a panic.
pub fn closest_focused_point(
    gaze: (f64, f64),
    points: &[(f64, f64)],
    calibrated: &[bool],
    radius: f64,
    aspect: f64,
) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, (point, &is_calibrated)) in points.iter().zip(calibrated.iter()).enumerate() {
        if is_calibrated {
            continue;
        }
        let d = aspect_corrected_dist(gaze, *point, aspect);
        if d < radius && best.is_none_or(|(_, best_d)| d < best_d) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 7-point set used by test 1, shaped like `calibrate_flow::FULL_7`
    /// (a center point plus 6 arranged around it). Hand-computed expected
    /// values below assume exactly these coordinates.
    const SEVEN_POINTS: [(f64, f64); 7] = [
        (0.5, 0.5),
        (0.1, 0.9),
        (0.5, 0.1),
        (0.9, 0.9),
        (0.1, 0.1),
        (0.5, 0.9),
        (0.9, 0.1),
    ];

    /// Hand-derivation for test 1 (aspect = 1.0, so aspect correction is a
    /// no-op and this is plain Euclidean distance):
    ///
    /// All-pairs distances among `SEVEN_POINTS` include several ties at the
    /// minimum, e.g.:
    ///   (0.5,0.5)-(0.5,0.1): dx=0.0, dy=0.4 -> dist = 0.4
    ///   (0.5,0.5)-(0.5,0.9): dx=0.0, dy=0.4 -> dist = 0.4
    ///   (0.1,0.9)-(0.5,0.9): dx=0.4, dy=0.0 -> dist = 0.4
    ///   (0.5,0.1)-(0.1,0.1): dx=0.4, dy=0.0 -> dist = 0.4
    ///   (0.5,0.1)-(0.9,0.1): dx=0.4, dy=0.0 -> dist = 0.4
    ///   (0.9,0.9)-(0.5,0.9): dx=0.4, dy=0.0 -> dist = 0.4
    /// No pair is closer than 0.4 (the corner points are pairwise >= 0.4
    /// apart on at least one axis, e.g. (0.1,0.9)-(0.1,0.1): dy=0.8; the
    /// center to a corner, e.g. (0.5,0.5)-(0.1,0.9): dx=0.4, dy=0.4 -> dist
    /// = sqrt(0.32) ≈ 0.566, which is larger than 0.4). So the minimum
    /// inter-point distance is 0.4.
    ///
    /// zone_radius = (0.4 / 2.0) * 0.45 = 0.2 * 0.45 = 0.09
    #[test]
    fn zone_radius_matches_hand_derivation() {
        let radius = zone_radius(&SEVEN_POINTS, 1.0);
        assert!((radius - 0.09).abs() < 1e-9, "expected 0.09, got {radius}");
    }

    /// Aspect scales the x-component of every pairwise distance, so the
    /// minimum inter-point distance (and thus the radius) changes too: with
    /// only x-separated points, doubling `aspect` doubles the minimum
    /// distance and therefore the radius.
    #[test]
    fn zone_radius_respects_aspect_correction() {
        let points = [(0.0, 0.0), (0.2, 0.0)];
        let radius_1x = zone_radius(&points, 1.0);
        let radius_2x = zone_radius(&points, 2.0);
        // aspect=1.0: dist = 0.2, radius = (0.2/2)*0.45 = 0.045
        assert!((radius_1x - 0.045).abs() < 1e-9, "got {radius_1x}");
        // aspect=2.0: dist = 0.4, radius = (0.4/2)*0.45 = 0.09
        assert!((radius_2x - 0.09).abs() < 1e-9, "got {radius_2x}");
    }

    #[test]
    fn zone_radius_fallback_for_fewer_than_two_points() {
        assert_eq!(zone_radius(&[], 1.0), f64::MAX);
        assert_eq!(zone_radius(&[(0.5, 0.5)], 1.0), f64::MAX);
    }

    #[test]
    fn none_when_gaze_outside_every_zone() {
        let radius = zone_radius(&SEVEN_POINTS, 1.0); // 0.09
        let calibrated = [false; 7];
        // Far from all 7 points (nearest point is (0.1,0.1) or (0.5,0.1),
        // both well beyond 0.09 away).
        let gaze = (0.0, 0.5);
        assert_eq!(
            closest_focused_point(gaze, &SEVEN_POINTS, &calibrated, radius, 1.0),
            None
        );
    }

    #[test]
    fn some_index_when_gaze_inside_exactly_one_zone() {
        let radius = zone_radius(&SEVEN_POINTS, 1.0); // 0.09
        let calibrated = [false; 7];
        // 0.0224 from point 0 (0.5,0.5); every other point is >= 0.39 away
        // (well outside the 0.09 radius).
        let gaze = (0.52, 0.51);
        assert_eq!(
            closest_focused_point(gaze, &SEVEN_POINTS, &calibrated, radius, 1.0),
            Some(0)
        );
    }

    /// Two close-together points (closer than the natural FULL_7 spacing) so
    /// gaze can sit inside both zones at once, letting us prove the
    /// already-calibrated point is skipped rather than merely never being
    /// the closest.
    const CLOSE_PAIR: [(f64, f64); 3] = [(0.5, 0.5), (0.5, 0.52), (0.9, 0.9)];

    #[test]
    fn skips_already_calibrated_point_falling_through_to_next() {
        let gaze = (0.5, 0.505);
        let radius = 0.05;
        // Distances: point 0 = 0.005 (closest), point 1 = 0.015, point 2 ≈ 0.562.
        // With nothing calibrated, point 0 wins as expected.
        assert_eq!(
            closest_focused_point(gaze, &CLOSE_PAIR, &[false, false, false], radius, 1.0),
            Some(0)
        );
        // Point 0 calibrated: falls through to point 1 (still within radius).
        assert_eq!(
            closest_focused_point(gaze, &CLOSE_PAIR, &[true, false, false], radius, 1.0),
            Some(1)
        );
    }

    #[test]
    fn skips_already_calibrated_point_returns_none_if_nothing_else_qualifies() {
        let radius = zone_radius(&SEVEN_POINTS, 1.0); // 0.09
        let gaze = (0.52, 0.51); // only within point 0's zone
        let mut calibrated = [false; 7];
        calibrated[0] = true;
        assert_eq!(
            closest_focused_point(gaze, &SEVEN_POINTS, &calibrated, radius, 1.0),
            None
        );
    }

    /// Proves the aspect correction isn't a no-op: the same raw gaze/point
    /// pair is "in zone" at aspect=1.0 but "out of zone" once x-distance is
    /// scaled up by a wide-screen aspect ratio.
    #[test]
    fn aspect_correction_changes_the_result() {
        let points = [(0.5, 0.5)];
        let calibrated = [false];
        let gaze = (0.59, 0.5); // raw dx = 0.09, dy = 0.0
        let radius = 0.1;

        // aspect=1.0: dist = 0.09 < 0.1 -> in zone.
        assert_eq!(
            closest_focused_point(gaze, &points, &calibrated, radius, 1.0),
            Some(0)
        );
        // aspect=2.5: dist = 0.225 >= 0.1 -> pushed out of zone.
        assert_eq!(
            closest_focused_point(gaze, &points, &calibrated, radius, 2.5),
            None
        );
    }
}
