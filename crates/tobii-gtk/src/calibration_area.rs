//! Pure geometry for confining calibration points to a centered sub-rectangle
//! on large screens.
//!
//! On a large/wide monitor, gaze estimation before a personal calibration
//! exists is only reliable within a bounded region around screen center — the
//! real Windows Tobii software never places (or verifies) a calibration dot
//! at the literal screen edge once the monitor gets big enough. Decompiling
//! the original (`CalibrationAreaCalculator`) confirms it caps calibration-dot
//! placement to a centered ~600mm x 340mm physical sub-rectangle whenever the
//! monitor's diagonal is >=700mm or its height is >=360mm, and leaves small
//! screens untouched.
//!
//! This module reimplements just that area-cap + point-remapping math, as a
//! standalone pure-logic unit with no dependency on the rest of this crate. A
//! later task wires it into the actual calibration flow (`calibrate_flow.rs`).

/// Physical size (mm) cap for the confined calibration area — matches the
/// decompiled original's `CalibrationAreaCalculator.MaxCalibrationDisplaySizeMm`.
pub const MAX_CALIBRATION_AREA_MM: (f64, f64) = (600.0, 340.0);

/// Diagonal (mm) at/above which capping applies — matches the decompiled
/// original's diagonal threshold (690mm + a 10mm tolerance = 700mm).
pub const MAX_DIAGONAL_MM: f64 = 700.0;

/// Height (mm) at/above which capping applies even if the diagonal alone
/// wouldn't trigger it — matches the original's height threshold (350mm +
/// a 10mm tolerance = 360mm).
pub const MAX_HEIGHT_MM: f64 = 360.0;

/// A normalized [0,1]x[0,1] sub-rectangle `(x0, y0, width, height)` that
/// calibration points should be confined to, or `None` if the physical
/// screen is small enough that the full screen can be used directly
/// (matches the decompiled original's own "already small enough, use
/// screenBounds unchanged" branch).
pub fn capped_area(width_mm: f64, height_mm: f64) -> Option<(f64, f64, f64, f64)> {
    // Defensive only: a real display setup always yields positive physical
    // dimensions, so this should never trigger in practice. It exists purely
    // so this pure geometry function cannot silently propagate NaN/infinity
    // (from dividing by a zero/negative width or height below) if it somehow
    // does — not a behavior anything relies on.
    if width_mm <= 0.0 || height_mm <= 0.0 {
        return None;
    }
    let diagonal_mm = (width_mm * width_mm + height_mm * height_mm).sqrt();
    if diagonal_mm < MAX_DIAGONAL_MM && height_mm < MAX_HEIGHT_MM {
        return None;
    }
    let cap_w = (MAX_CALIBRATION_AREA_MM.0 / width_mm).min(1.0);
    let cap_h = (MAX_CALIBRATION_AREA_MM.1 / height_mm).min(1.0);
    let x0 = (1.0 - cap_w) / 2.0;
    let y0 = (1.0 - cap_h) / 2.0;
    Some((x0, y0, cap_w, cap_h))
}

/// Remap a point from the "conceptual full-screen" [0,1]x[0,1] space (where
/// the calibration flow's raw point-set values live) into the capped,
/// centered sub-rectangle — or return it unchanged if `area` is `None`.
pub fn remap_point(point: (f64, f64), area: Option<(f64, f64, f64, f64)>) -> (f64, f64) {
    match area {
        None => point,
        Some((x0, y0, w, h)) => (x0 + point.0 * w, y0 + point.1 * h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn approx_pt(a: (f64, f64), b: (f64, f64)) -> bool {
        approx(a.0, b.0) && approx(a.1, b.1)
    }

    // --- capped_area: small screen stays uncapped ---

    #[test]
    fn small_laptop_panel_is_not_capped() {
        // ~16:9 laptop panel, e.g. 15.6": diagonal well under 700mm, height
        // well under 360mm.
        let w: f64 = 344.0;
        let h: f64 = 194.0;
        let diagonal = (w * w + h * h).sqrt();
        assert!(diagonal < MAX_DIAGONAL_MM);
        assert!(h < MAX_HEIGHT_MM);
        assert_eq!(capped_area(w, h), None);
    }

    // --- capped_area: large screen is capped, with hand-computed values ---

    #[test]
    fn large_screen_round_numbers_produce_hand_computed_cap() {
        // Deliberately round numbers so the expected (x0, y0, w, h) can be
        // verified by hand without recomputing the formula:
        //   cap_w = 600/1200 = 0.5,  cap_h = 340/500 = 0.68
        //   x0 = (1 - 0.5)/2 = 0.25, y0 = (1 - 0.68)/2 = 0.16
        let (w, h): (f64, f64) = (1200.0, 500.0);
        let diagonal = (w * w + h * h).sqrt();
        assert!(diagonal >= MAX_DIAGONAL_MM); // 1300mm, well over
        assert!(h >= MAX_HEIGHT_MM); // 500mm, well over

        let area = capped_area(w, h).expect("large screen must be capped");
        // Exact equality would be flaky here: 0.68 has no exact f64
        // representation, so (1.0 - 0.68) / 2.0 lands a ULP away from the
        // literal 0.16 (0.15999999999999998 vs 0.16). `approx` is the right
        // tool, not a weakening of the check.
        assert!(approx(area.0, 0.25));
        assert!(approx(area.1, 0.16));
        assert!(approx(area.2, 0.5));
        assert!(approx(area.3, 0.68));
        assert!(approx(area.0, (1.0 - area.2) / 2.0), "x0 must be centered");
        assert!(approx(area.1, (1.0 - area.3) / 2.0), "y0 must be centered");
    }

    #[test]
    fn realistic_ultrawide_monitor_produces_hand_computed_cap() {
        // A 34" ultrawide, ~797mm x 336mm. Diagonal alone crosses the
        // threshold (height stays under 360mm, so cap_h saturates at 1.0).
        let (w, h): (f64, f64) = (797.0, 336.0);
        let diagonal = (w * w + h * h).sqrt();
        assert!(diagonal >= MAX_DIAGONAL_MM);
        assert!(h < MAX_HEIGHT_MM);

        let expected_w = (600.0_f64 / 797.0).min(1.0);
        let expected_h = (340.0_f64 / 336.0).min(1.0);
        assert!(approx(expected_h, 1.0), "340/336 > 1.0, so cap_h saturates");

        let (x0, y0, cap_w, cap_h) = capped_area(w, h).expect("large screen must be capped");
        assert!(approx(cap_w, expected_w));
        assert!(approx(cap_h, expected_h));
        assert!(approx(x0, (1.0 - cap_w) / 2.0));
        assert!(approx(y0, (1.0 - cap_h) / 2.0));
    }

    // --- capped_area: boundary correctness ---
    //
    // The two conditions (diagonal >= 700mm, height >= 360mm) are each
    // independently sufficient to trigger capping.

    #[test]
    fn both_just_under_threshold_stays_uncapped() {
        // diagonal = sqrt(600^2 + 359^2) ≈ 699.2mm (< 700), height = 359mm (< 360).
        let (w, h): (f64, f64) = (600.0, 359.0);
        let diagonal = (w * w + h * h).sqrt();
        assert!(
            diagonal < MAX_DIAGONAL_MM,
            "diagonal {diagonal} should be just under 700"
        );
        assert!(h < MAX_HEIGHT_MM);
        assert_eq!(capped_area(w, h), None);
    }

    #[test]
    fn diagonal_alone_at_or_over_threshold_triggers_cap() {
        // 196/672/700 is a Pythagorean triple (a multiple of 7/24/25), so the
        // diagonal lands on exactly 700.0mm with no floating-point rounding
        // slop — an exact "at the threshold" case — while height (196mm)
        // stays well under 360mm, isolating the diagonal condition.
        let (w, h): (f64, f64) = (672.0, 196.0);
        let diagonal = (w * w + h * h).sqrt();
        assert_eq!(
            diagonal, MAX_DIAGONAL_MM,
            "672/196 must give an exact 700.0 diagonal"
        );
        assert!(h < MAX_HEIGHT_MM);
        assert!(
            capped_area(w, h).is_some(),
            "diagonal at the threshold must trigger capping on its own"
        );

        // And clearly (not just exactly-at) over the threshold too.
        let (w, h): (f64, f64) = (650.0, 300.0);
        let diagonal = (w * w + h * h).sqrt();
        assert!(diagonal > MAX_DIAGONAL_MM);
        assert!(h < MAX_HEIGHT_MM);
        assert!(capped_area(w, h).is_some());
    }

    #[test]
    fn height_alone_at_or_over_threshold_triggers_cap() {
        // height = 360.0mm exactly (a literal, not a computed sqrt, so no
        // rounding concern), diagonal stays well under 700mm, isolating the
        // height condition.
        let (w, h): (f64, f64) = (300.0, 360.0);
        let diagonal = (w * w + h * h).sqrt();
        assert!(diagonal < MAX_DIAGONAL_MM);
        assert_eq!(h, MAX_HEIGHT_MM);
        assert!(
            capped_area(w, h).is_some(),
            "height at the threshold must trigger capping on its own"
        );
    }

    // --- capped_area: degenerate input ---

    #[test]
    fn degenerate_dimensions_return_none_not_nan() {
        assert_eq!(capped_area(0.0, 500.0), None);
        assert_eq!(capped_area(500.0, 0.0), None);
        assert_eq!(capped_area(-100.0, 500.0), None);
        assert_eq!(capped_area(500.0, -100.0), None);
        assert_eq!(capped_area(0.0, 0.0), None);
    }

    // --- remap_point: None passes through unchanged ---

    #[test]
    fn remap_with_no_area_is_identity() {
        for point in [(0.5, 0.5), (0.0, 0.0), (1.0, 1.0), (0.1, 0.9), (0.37, 0.82)] {
            assert_eq!(remap_point(point, None), point);
        }
    }

    // --- remap_point: concrete Some(area) ---

    #[test]
    fn remap_with_area_matches_hand_computed_points() {
        // Same centered cap as `large_screen_round_numbers_produce_hand_computed_cap`:
        // x0 = 0.25, y0 = 0.16, w = 0.5, h = 0.68.
        let area = Some((0.25, 0.16, 0.5, 0.68));

        // Corner-ish point. Comparisons use `approx` rather than `==`: 0.16
        // and 0.68 have no exact f64 representation, so the hand-computed
        // decimal and the arithmetic result can differ by a ULP even when
        // both are "the same number" mathematically.
        let got = remap_point((0.1, 0.9), area);
        assert!(approx_pt(got, (0.25 + 0.1 * 0.5, 0.16 + 0.9 * 0.68)));
        assert!(approx_pt(got, (0.30, 0.772)));

        // Screen top-left / bottom-right corners map to the sub-rect's corners.
        assert!(approx_pt(remap_point((0.0, 0.0), area), (0.25, 0.16)));
        assert!(approx_pt(remap_point((1.0, 1.0), area), (0.75, 0.84)));

        // Center: a centered cap always keeps the (0.5, 0.5) point value
        // mapping to (0.5, 0.5). This holds for ANY centered cap (not just
        // this one) because x0 = (1 - w)/2, so x0 + 0.5*w = 0.5 always (same
        // for y0/h) — it's a direct consequence of centering, not a
        // coincidence of these particular numbers. It matters because the
        // calibration flow's own first/center point must never move,
        // regardless of screen size.
        let center = remap_point((0.5, 0.5), area);
        assert!(approx_pt(center, (0.25 + 0.5 * 0.5, 0.16 + 0.5 * 0.68)));
        assert!(approx_pt(center, (0.5, 0.5)));
    }

    #[test]
    fn centered_cap_always_fixes_the_center_point() {
        // General version of the identity above: for ANY valid capped_area
        // output (which is always centered by construction), remapping the
        // conceptual center must return the true screen center exactly.
        for (w_mm, h_mm) in [
            (1200.0, 500.0),
            (797.0, 336.0),
            (672.0, 196.0),
            (2000.0, 1000.0),
        ] {
            if let Some(area) = capped_area(w_mm, h_mm) {
                let (cx, cy) = remap_point((0.5, 0.5), Some(area));
                assert!(approx(cx, 0.5), "center.x drifted for {w_mm}x{h_mm}");
                assert!(approx(cy, 0.5), "center.y drifted for {w_mm}x{h_mm}");
            }
        }
    }
}
