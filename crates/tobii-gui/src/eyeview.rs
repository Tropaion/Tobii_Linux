//! Pure mapping from a decoded gaze sample to a renderable eye-position view.
//! No egui — the widget (hub/flows) draws from this.

use tobii_protocol::gaze::present;
use tobii_protocol::GazeSample;

/// Comfortable operating-distance window (mm) and centre tolerance for guidance.
const DIST_MIN_MM: f32 = 450.0;
const DIST_MAX_MM: f32 = 750.0;
const CENTRE_TOL: f32 = 0.18; // max |mid - 0.5| on each axis to count as centred

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Guidance {
    NoEyes,
    MoveCloser,
    MoveBack,
    Centered,
    OffCenter,
}

/// A renderable eye-position snapshot. `left`/`right` are normalized `[0,1]`
/// trackbox coordinates (the widget scales them into its rectangle).
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
        let left = [s.trackbox_eye_l[0] as f32, s.trackbox_eye_l[1] as f32];
        let right = [s.trackbox_eye_r[0] as f32, s.trackbox_eye_r[1] as f32];
        let distance = ((s.trackbox_eye_l[2] + s.trackbox_eye_r[2]) / 2.0) as f32;
        let mid_x = (left[0] + right[0]) / 2.0;
        let mid_y = (left[1] + right[1]) / 2.0;

        let guidance = if distance < DIST_MIN_MM {
            Guidance::MoveBack
        } else if distance > DIST_MAX_MM {
            Guidance::MoveCloser
        } else if (mid_x - 0.5).abs() > CENTRE_TOL || (mid_y - 0.5).abs() > CENTRE_TOL {
            Guidance::OffCenter
        } else {
            Guidance::Centered
        };

        EyeView {
            left: Some(left),
            right: Some(right),
            distance_mm: Some(distance),
            guidance,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tobii_protocol::gaze::present;
    use tobii_protocol::GazeSample;

    // The GazeSample struct has many fields; building via `default()` + targeted
    // field assignment (rather than a full struct literal) is the intended
    // pattern here, so this test helper opts out of clippy's default lint.
    #[allow(clippy::field_reassign_with_default)]
    fn sample_with_trackbox(l: [f64; 3], r: [f64; 3], valid: bool) -> GazeSample {
        let mut s = GazeSample::default();
        s.trackbox_eye_l = l;
        s.trackbox_eye_r = r;
        s.present_mask |= present::TRACKBOX_L | present::TRACKBOX_R;
        s.present_mask |= present::VALIDITY_L | present::VALIDITY_R;
        let v = if valid { 0 } else { 4 };
        s.validity_l = v;
        s.validity_r = v;
        s
    }

    #[test]
    fn no_trackbox_columns_means_no_eyes() {
        let v = EyeView::from_gaze(&GazeSample::default());
        assert!(matches!(v.guidance, Guidance::NoEyes));
        assert!(v.left.is_none() && v.right.is_none());
    }

    #[test]
    fn centered_eyes_map_to_box_and_report_centered() {
        let v = EyeView::from_gaze(&sample_with_trackbox(
            [0.45, 0.5, 550.0],
            [0.55, 0.5, 550.0],
            true,
        ));
        assert!(v.left.is_some() && v.right.is_some());
        // x normalized [0,1] -> passed through as f32 for the widget to scale.
        assert!((v.left.unwrap()[0] - 0.45).abs() < 1e-6);
        assert!(matches!(v.guidance, Guidance::Centered));
        assert!((v.distance_mm.unwrap() - 550.0).abs() < 1e-3);
    }

    #[test]
    fn too_close_and_too_far_are_flagged() {
        let close = EyeView::from_gaze(&sample_with_trackbox(
            [0.5, 0.5, 300.0],
            [0.5, 0.5, 300.0],
            true,
        ));
        assert!(matches!(close.guidance, Guidance::MoveBack));
        let far = EyeView::from_gaze(&sample_with_trackbox(
            [0.5, 0.5, 900.0],
            [0.5, 0.5, 900.0],
            true,
        ));
        assert!(matches!(far.guidance, Guidance::MoveCloser));
    }

    #[test]
    fn off_center_eyes_are_flagged() {
        let v = EyeView::from_gaze(&sample_with_trackbox(
            [0.1, 0.5, 550.0],
            [0.2, 0.5, 550.0],
            true,
        ));
        assert!(matches!(v.guidance, Guidance::OffCenter));
    }

    #[test]
    fn invalid_validity_means_no_eyes() {
        let v = EyeView::from_gaze(&sample_with_trackbox(
            [0.5, 0.5, 550.0],
            [0.5, 0.5, 550.0],
            false,
        ));
        assert!(matches!(v.guidance, Guidance::NoEyes));
    }
}
