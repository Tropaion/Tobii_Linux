//! Pure mapping from a decoded gaze sample to a renderable eye-position view.
//! No GUI-toolkit types here — the widget (hub/flows) draws from this.
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
/// into its rectangle. `left_alpha`/`right_alpha` (`[0,1]`, only meaningful
/// when the matching `left`/`right` is `Some`) are how opaque each eye's dot
/// should render — see `EyeHistory`'s doc comment for why this exists.
/// `distance_mm` is the real operating distance (mm).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EyeView {
    pub left: Option<[f32; 2]>,
    pub right: Option<[f32; 2]>,
    pub left_alpha: f32,
    pub right_alpha: f32,
    pub distance_mm: Option<f32>,
    pub guidance: Guidance,
}

impl EyeView {
    /// The "nothing to show" view: no eyes, no distance. Used both for a sample
    /// that carries no usable eyes and, by the widget, when the device is not
    /// connected at all.
    pub fn none() -> EyeView {
        EyeView {
            left: None,
            right: None,
            left_alpha: 0.0,
            right_alpha: 0.0,
            distance_mm: None,
            guidance: Guidance::NoEyes,
        }
    }

    pub fn from_gaze(s: &GazeSample) -> EyeView {
        let eyes_valid = s.has(present::TRACKBOX_L)
            && s.has(present::TRACKBOX_R)
            && s.validity_l == 0
            && s.validity_r == 0;
        if !eyes_valid {
            return EyeView::none();
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
            left_alpha: 1.0,
            right_alpha: 1.0,
            distance_mm,
            guidance,
        }
    }
}

/// Ticks (frames, ~33ms each at this app's cadence) over which a held eye
/// position fades from fully opaque (`1.0`) to fully absent (`0.0`) once its
/// own reading goes invalid.
///
/// This project tried two other approaches first, on the same real hardware,
/// across several rounds of on-hardware feedback:
/// - Raw pass-through (no history at all): instantly flips to "no eyes" on
///   any single invalid frame — the original reported bug (fast head
///   movement flickering to "no eyes").
/// - A faithful port of the real software's own `ExtrapolatePosition`
///   algorithm (2-point linear extrapolation across an 11-frame window,
///   confirmed correct via direct decompilation): still read as "laggy" —
///   a HELD/PROJECTED position that is confidently wrong for the whole gap,
///   however brief, apparently feels worse than an honest "this is
///   uncertain right now" signal, even though the algorithm matched ground
///   truth exactly.
///
/// This fades instead of projecting: the position simply HOLDS at its last
/// known value (never moves during a gap, so it can never be confidently
/// *wrong* about where the eye currently is) while its rendered opacity
/// ramps down. Live measurement (see the git history around this constant's
/// introduction) showed the median real invalid stretch during fast head
/// movement is ~4 frames — well under this window — so a typical gap only
/// partially dims and recovers to full opacity the instant a valid frame
/// returns, rather than either freezing at full strength or vanishing
/// outright; only a stretch that genuinely exceeds this window fades all
/// the way to absent.
pub const FADE_TICKS: u32 = 8; // ~264ms

/// One eye's decoded per-frame reading: mirror-view trackbox position plus
/// operating distance (mm), or absent if that eye's reading was invalid this
/// frame. Mirrors exactly what `EyeView::from_gaze` already decodes per eye —
/// this struct just lets that decoded value be held/faded instead of
/// used-or-discarded immediately.
#[derive(Clone, Copy, Debug, PartialEq)]
struct EyeSample {
    pos: [f32; 2],
    distance_mm: Option<f32>,
}

/// Decode one eye's reading from a gaze frame: mirrors `EyeView::from_gaze`'s
/// per-eye decode (mirrored x, y passes through unchanged, distance from that
/// eye's own eye-origin z in mm) but keyed to a single eye instead of gating
/// on both eyes being valid at once.
fn decode_eye(
    trackbox_present: bool,
    validity: u32,
    trackbox: [f64; 3],
    origin_present: bool,
    origin_mm: [f64; 3],
) -> Option<EyeSample> {
    if !trackbox_present || validity != 0 {
        return None;
    }
    Some(EyeSample {
        pos: [1.0 - trackbox[0] as f32, trackbox[1] as f32],
        distance_mm: if origin_present {
            Some(origin_mm[2] as f32)
        } else {
            None
        },
    })
}

/// Per-eye hold+fade tracker: on a valid frame, holds that reading at full
/// opacity; on an invalid frame, keeps the LAST known reading unchanged
/// (never projects a new position) while fading its opacity toward zero over
/// `FADE_TICKS`. Returns `None` (fully absent) once the fade completes.
#[derive(Debug, Clone, Copy, Default)]
struct EyeTrack {
    last: Option<EyeSample>,
    ticks_since_valid: u32,
}

impl EyeTrack {
    fn update(&mut self, sample: Option<EyeSample>) -> Option<(EyeSample, f32)> {
        match sample {
            Some(s) => {
                self.last = Some(s);
                self.ticks_since_valid = 0;
                Some((s, 1.0))
            }
            None => {
                self.ticks_since_valid += 1;
                let alpha = 1.0 - (self.ticks_since_valid as f32 / FADE_TICKS as f32);
                if alpha <= 0.0 {
                    self.last = None;
                    None
                } else {
                    self.last.map(|s| (s, alpha))
                }
            }
        }
    }
}

/// Combine both eyes' resolved (position, alpha) pairs into an `EyeView`,
/// reusing `EyeView::from_gaze`'s exact guidance thresholds/selection logic.
///
/// Requires BOTH eyes to still be resolvable (nonzero alpha) before showing
/// anything — matching `EyeView::from_gaze`'s original AND-gate.
///
/// This is a deliberate DISPLAY-level choice, not a per-eye-tracking one: each
/// eye's own hold/fade in `EyeHistory` still runs fully independently (a
/// brief dropout on one eye doesn't touch the other's own state at all).
/// Live-hardware measurement showed the two eyes' invalid stretches are NOT
/// always symmetric — one eye can occasionally fade out entirely while the
/// other is still fine, and showing only the surviving eye's dot reads as a
/// confusing, never-happens-on-the-original "one eye" state (the original's
/// own per-eye-independent status logic can technically do this too, but its
/// much more robust native position source makes it rare enough not to be
/// noticed in practice). Requiring both restores `EyeView::none()` for that
/// specific case instead — an honest "can't show this reliably" rather than
/// a half-populated view.
fn combine(left: Option<(EyeSample, f32)>, right: Option<(EyeSample, f32)>) -> EyeView {
    let (Some((left, left_alpha)), Some((right, right_alpha))) = (left, right) else {
        return EyeView::none();
    };

    let distance_mm = match (left.distance_mm, right.distance_mm) {
        (Some(dl), Some(dr)) => Some((dl + dr) / 2.0),
        (Some(d), None) | (None, Some(d)) => Some(d),
        (None, None) => None,
    };

    let near_edge = [left.pos, right.pos].iter().any(|p| {
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
        left: Some(left.pos),
        right: Some(right.pos),
        left_alpha,
        right_alpha,
        distance_mm,
        guidance,
    }
}

/// Rolling per-eye hold+fade state — see `FADE_TICKS`'s doc comment for the
/// design rationale (holds steady rather than extrapolating, fades opacity
/// rather than flipping to absent instantly).
#[derive(Debug, Clone, Default)]
pub struct EyeHistory {
    left: EyeTrack,
    right: EyeTrack,
}

impl EyeHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one incoming gaze frame and return the resulting (held/faded)
    /// `EyeView` for THIS frame. Call exactly once per incoming gaze
    /// notification, in arrival order — do not call this more than once per
    /// actual frame (the fade-tick counting assumes one update per real frame).
    pub fn update(&mut self, s: &GazeSample) -> EyeView {
        let left_sample = decode_eye(
            s.has(present::TRACKBOX_L),
            s.validity_l,
            s.trackbox_eye_l,
            s.has(present::EYE_ORIGIN_L),
            s.eye_origin_l_mm,
        );
        let right_sample = decode_eye(
            s.has(present::TRACKBOX_R),
            s.validity_r,
            s.trackbox_eye_r,
            s.has(present::EYE_ORIGIN_R),
            s.eye_origin_r_mm,
        );

        combine(
            self.left.update(left_sample),
            self.right.update(right_sample),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Build a sample with independent per-eye validity — for testing that
    /// each eye's history/fade is resolved independently.
    fn sample_split(
        tb_l: [f64; 3],
        tb_r: [f64; 3],
        origin_z_mm: f64,
        valid_l: bool,
        valid_r: bool,
    ) -> GazeSample {
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
            validity_l: if valid_l { 0 } else { 4 },
            validity_r: if valid_r { 0 } else { 4 },
            ..Default::default()
        }
    }

    #[test]
    fn history_single_valid_frame_matches_from_gaze() {
        // No history yet: EyeHistory::update on the very first frame must be
        // identical to the pure, stateless EyeView::from_gaze on that frame
        // (including both alphas at full opacity).
        let g = sample([0.6, 0.5, 0.5], [0.4, 0.5, 0.5], 680.0, true);
        let via_history = EyeHistory::new().update(&g);
        let direct = EyeView::from_gaze(&g);
        assert_eq!(via_history, direct);
    }

    #[test]
    fn brief_invalid_gap_holds_through_it_instead_of_no_eyes() {
        // Regression test for the original reported bug: valid, valid,
        // invalid x3, valid. The invalid stretch must NOT report "no eyes"
        // (it must hold at a fading-but-nonzero opacity).
        let mut hist = EyeHistory::new();
        hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, true));
        hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, true));
        for _ in 0..3 {
            let v = hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, false));
            assert!(
                !matches!(v.guidance, Guidance::NoEyes),
                "a brief gap within the fade window must not report no eyes"
            );
            assert!(v.left.is_some() && v.right.is_some());
        }
        // Recovery: a subsequent valid frame is used directly again, at full opacity.
        let v = hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, true));
        assert!(!matches!(v.guidance, Guidance::NoEyes));
        assert_eq!(v.left_alpha, 1.0);
        assert_eq!(v.right_alpha, 1.0);
    }

    #[test]
    fn full_fade_window_of_invalid_frames_eventually_reports_no_eyes() {
        // The fade window is finite: once FADE_TICKS consecutive invalid
        // frames have elapsed, the eye must be reported absent again (no
        // infinite holding).
        let mut hist = EyeHistory::new();
        hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, true));
        let mut last = None;
        for _ in 0..FADE_TICKS {
            last = Some(hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, false)));
        }
        let v = last.unwrap();
        assert!(matches!(v.guidance, Guidance::NoEyes));
        assert!(v.left.is_none() && v.right.is_none());
    }

    #[test]
    fn single_historical_sample_holds_without_moving() {
        // Only one valid sample seen: the position must hold unchanged on
        // the next invalid frame (no divide-by-zero, no projection).
        let mut hist = EyeHistory::new();
        let v0 = hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, true));
        let v1 = hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, false));
        assert_eq!(v1.left, v0.left);
        assert_eq!(v1.right, v0.right);
        assert!(!matches!(v1.guidance, Guidance::NoEyes));
    }

    #[test]
    fn gap_holds_steady_instead_of_continuing_the_trend() {
        // Two valid samples showing clear movement (raw x decreasing =>
        // mirrored x increasing), then a gap: the held position must equal
        // the second valid frame's position exactly, NOT continue moving
        // further along that trend — this fade design never projects a new
        // position, only holds the last one while dimming it.
        let mut hist = EyeHistory::new();
        hist.update(&sample([0.7, 0.5, 0.5], [0.7, 0.5, 0.5], 680.0, true)); // mirrored x=0.3
        let v_second = hist.update(&sample([0.6, 0.5, 0.5], [0.6, 0.5, 0.5], 680.0, true)); // mirrored x=0.4
        let v_gap = hist.update(&sample([0.6, 0.5, 0.5], [0.6, 0.5, 0.5], 680.0, false));

        assert_eq!(v_gap.left, v_second.left);
        assert_eq!(v_gap.right, v_second.right);
    }

    #[test]
    fn alpha_decays_linearly_then_recovers_instantly() {
        // Concrete alpha values through a gap and back: with FADE_TICKS=8,
        // alpha should be 1 - k/8 after k consecutive invalid frames, then
        // snap straight back to 1.0 the instant a valid frame returns.
        let mut hist = EyeHistory::new();
        hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, true));
        for k in 1..FADE_TICKS {
            let v = hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, false));
            let expected = 1.0 - (k as f32 / FADE_TICKS as f32);
            assert!(
                (v.left_alpha - expected).abs() < 1e-6,
                "k={k}: expected alpha={expected}, got {}",
                v.left_alpha
            );
        }
        let v = hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, true));
        assert_eq!(v.left_alpha, 1.0);
        assert_eq!(v.right_alpha, 1.0);
    }

    #[test]
    fn each_eye_is_resolved_independently() {
        // The left eye has a brief gap; the right eye stays continuously
        // valid. Each eye's OWN hold/fade must be computed independently
        // (a left-eye gap must not touch the right eye's own resolved
        // value or alpha) — this is about `EyeHistory`'s internal per-eye
        // tracking, not the DISPLAY gate (see
        // `both_eyes_required_to_show_anything` below for that): here the
        // left eye is still held (`Some`, fading), so both eyes are
        // populated and this doesn't exercise the gate at all.
        let mut hist = EyeHistory::new();
        hist.update(&sample_split(
            [0.5, 0.5, 0.5],
            [0.3, 0.5, 0.5],
            680.0,
            true,
            true,
        ));
        let v = hist.update(&sample_split(
            [0.5, 0.5, 0.5],
            [0.3, 0.5, 0.5],
            680.0,
            false,
            true,
        ));
        // Right eye: mirrored 1-0.3=0.7, exactly as this frame's own reading,
        // at full opacity since it was never invalid.
        assert!((v.right.unwrap()[0] - 0.7).abs() < 1e-6);
        assert_eq!(v.right_alpha, 1.0);
        // Left eye: held from the prior valid frame, not dropped, fading.
        assert!(v.left.is_some());
        assert!(v.left_alpha < 1.0);
        assert!(!matches!(v.guidance, Guidance::NoEyes));
    }

    #[test]
    fn both_eyes_required_to_show_anything() {
        // Regression test: live-hardware capture showed the two eyes' invalid
        // stretches are not symmetric — one eye's ENTIRE fade window can
        // exhaust (genuinely `None`) while the other stays continuously
        // valid. Showing just the surviving eye's dot reads as a confusing
        // "one eye" state that never happens on the raw, pre-history display
        // (which ANDs both eyes together) — `combine` must require BOTH eyes
        // to still be resolvable before showing anything, falling back to
        // `EyeView::none()` otherwise, even though the right eye here is
        // perfectly fine the whole time.
        let mut hist = EyeHistory::new();
        hist.update(&sample_split(
            [0.5, 0.5, 0.5],
            [0.3, 0.5, 0.5],
            680.0,
            true,
            true,
        ));
        let mut last = None;
        for _ in 0..FADE_TICKS {
            last = Some(hist.update(&sample_split(
                [0.5, 0.5, 0.5],
                [0.3, 0.5, 0.5],
                680.0,
                false, // left eye: invalid for the whole window, exhausting it
                true,  // right eye: continuously valid throughout
            )));
        }
        let v = last.unwrap();
        assert!(
            matches!(v.guidance, Guidance::NoEyes),
            "must show NoEyes, not a lone right eye, once the left eye's fade window is exhausted"
        );
        assert!(v.left.is_none() && v.right.is_none());
    }
}
