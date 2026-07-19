//! Pure mapping from a decoded gaze sample to a renderable eye-position view.
//! No egui — the widget (hub/flows) draws from this.
//!
//! Coordinate + distance conventions were confirmed against a live ET5:
//! - trackbox columns (0x03/0x09) give eye x/y **normalized `[0,1]`** in the
//!   tracker's camera frame, plus a normalized z (NOT millimetres);
//! - the true operating **distance in mm** comes from the eye-origin columns
//!   (0x02/0x08) z;
//! - the camera frame is left-right mirrored vs. the user, so x is flipped so
//!   the view reads like a mirror (you move left → your dot moves left).

use tobii_protocol::gaze::present;
use tobii_protocol::GazeSample;

/// Comfortable operating-distance window (mm). The ET5 tracks roughly 50–95 cm.
const DIST_MIN_MM: f32 = 500.0;
const DIST_MAX_MM: f32 = 900.0;
/// How close to a trackbox edge (normalized) before we suggest re-centering.
const EDGE_MARGIN: f32 = 0.08;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Guidance {
    NoEyes,
    MoveCloser,
    MoveBack,
    Centered,
    OffCenter,
}

/// A renderable eye-position snapshot. `left`/`right` are **mirror-view**
/// normalized `[0,1]` coordinates (x already flipped) that the widget scales
/// into its rectangle. `distance_mm` is the real operating distance (mm).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EyeView {
    pub left: Option<[f32; 2]>,
    pub right: Option<[f32; 2]>,
    pub distance_mm: Option<f32>,
    pub guidance: Guidance,
}

impl EyeView {
    pub fn from_gaze(s: &GazeSample) -> EyeView {
        let eyes_valid = s.has(present::TRACKBOX_L)
            && s.has(present::TRACKBOX_R)
            && s.validity_l == 0
            && s.validity_r == 0;
        if !eyes_valid {
            return EyeView {
                left: None,
                right: None,
                distance_mm: None,
                guidance: Guidance::NoEyes,
            };
        }

        // Mirror x (camera frame → mirror view); y passes through.
        let left = [1.0 - s.trackbox_eye_l[0] as f32, s.trackbox_eye_l[1] as f32];
        let right = [1.0 - s.trackbox_eye_r[0] as f32, s.trackbox_eye_r[1] as f32];

        // Real distance is the eye-origin z in mm (the trackbox z is normalized).
        let distance_mm = if s.has(present::EYE_ORIGIN_L) && s.has(present::EYE_ORIGIN_R) {
            Some(((s.eye_origin_l_mm[2] + s.eye_origin_r_mm[2]) / 2.0) as f32)
        } else {
            None
        };

        let near_edge = [left, right].iter().any(|p| {
            p[0] < EDGE_MARGIN
                || p[0] > 1.0 - EDGE_MARGIN
                || p[1] < EDGE_MARGIN
                || p[1] > 1.0 - EDGE_MARGIN
        });

        let guidance = match distance_mm {
            Some(d) if d < DIST_MIN_MM => Guidance::MoveBack,
            Some(d) if d > DIST_MAX_MM => Guidance::MoveCloser,
            _ if near_edge => Guidance::OffCenter,
            _ => Guidance::Centered,
        };

        EyeView {
            left: Some(left),
            right: Some(right),
            distance_mm,
            guidance,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tobii_protocol::gaze::present;
    use tobii_protocol::GazeSample;

    /// Build a sample with trackbox (normalized) + eye-origin (mm) + validity.
    fn sample(tb_l: [f64; 3], tb_r: [f64; 3], origin_z_mm: f64, valid: bool) -> GazeSample {
        let v = if valid { 0 } else { 4 };
        GazeSample {
            trackbox_eye_l: tb_l,
            trackbox_eye_r: tb_r,
            eye_origin_l_mm: [0.0, 0.0, origin_z_mm],
            eye_origin_r_mm: [0.0, 0.0, origin_z_mm],
            present_mask: present::TRACKBOX_L
                | present::TRACKBOX_R
                | present::EYE_ORIGIN_L
                | present::EYE_ORIGIN_R
                | present::VALIDITY_L
                | present::VALIDITY_R,
            validity_l: v,
            validity_r: v,
            ..Default::default()
        }
    }

    #[test]
    fn no_trackbox_columns_means_no_eyes() {
        let v = EyeView::from_gaze(&GazeSample::default());
        assert!(matches!(v.guidance, Guidance::NoEyes));
        assert!(v.left.is_none() && v.right.is_none());
    }

    #[test]
    fn invalid_validity_means_no_eyes() {
        let v = EyeView::from_gaze(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, false));
        assert!(matches!(v.guidance, Guidance::NoEyes));
    }

    #[test]
    fn x_is_mirrored() {
        // Trackbox left-eye at raw x=0.6 must render at 1-0.6=0.4 (mirror view).
        let v = EyeView::from_gaze(&sample([0.6, 0.5, 0.5], [0.4, 0.5, 0.5], 680.0, true));
        assert!((v.left.unwrap()[0] - 0.4).abs() < 1e-6);
        assert!((v.right.unwrap()[0] - 0.6).abs() < 1e-6);
        // y passes through unchanged.
        assert!((v.left.unwrap()[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn distance_comes_from_eye_origin_mm_not_trackbox_z() {
        // Trackbox z is a normalized 0.58; the reported distance must be the
        // eye-origin 680 mm, and a mid-range distance reads as Centered.
        let v = EyeView::from_gaze(&sample([0.55, 0.5, 0.58], [0.45, 0.5, 0.58], 680.0, true));
        assert!((v.distance_mm.unwrap() - 680.0).abs() < 1e-3);
        assert!(matches!(v.guidance, Guidance::Centered));
    }

    #[test]
    fn too_close_and_too_far_use_mm_thresholds() {
        let close = EyeView::from_gaze(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 400.0, true));
        assert!(matches!(close.guidance, Guidance::MoveBack));
        let far = EyeView::from_gaze(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 1000.0, true));
        assert!(matches!(far.guidance, Guidance::MoveCloser));
    }

    #[test]
    fn near_box_edge_is_off_center() {
        // Raw x=0.95 → mirrored 0.05, within EDGE_MARGIN of the edge.
        let v = EyeView::from_gaze(&sample([0.95, 0.5, 0.5], [0.9, 0.5, 0.5], 680.0, true));
        assert!(matches!(v.guidance, Guidance::OffCenter));
    }
}
