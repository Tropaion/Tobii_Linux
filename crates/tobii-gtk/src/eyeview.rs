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

use std::collections::VecDeque;

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
    /// The "nothing to show" view: no eyes, no distance. Used both for a sample
    /// that carries no usable eyes and, by the widget, when the device is not
    /// connected at all.
    pub fn none() -> EyeView {
        EyeView {
            left: None,
            right: None,
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
            distance_mm,
            guidance,
        }
    }
}

/// How many past frames (per eye) are kept for hold/extrapolation across a
/// brief invalid/missing reading — matches the real software's concrete
/// `EyesPositioningParametersCalculator` (found by decompiling
/// `Tobii.Configuration.Common.dll`'s `Tobii.Configuration.Common.EyePositioning`
/// namespace — its `IEyesPositioningParametersCalculator` interface was found
/// earlier this session, but its actual implementation, in a different
/// namespace, was missed until directly asked to find a better solution):
/// `MaxCountOfExtrapolatedGazeDataPosition = 11`.
pub const MAX_HISTORY_FRAMES: usize = 11;

/// One eye's decoded per-frame reading: mirror-view trackbox position plus
/// operating distance (mm), or absent if that eye's reading was invalid this
/// frame. Mirrors exactly what `EyeView::from_gaze` already decodes per eye —
/// this struct just lets that decoded value be buffered/extrapolated instead
/// of used-or-discarded immediately.
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

/// Resolve one eye's position for the current frame from its rolling
/// history: use this frame's own reading directly if it's valid; otherwise
/// hold the last known-good value if only one is available, or linearly
/// extrapolate from the last two known-good values (frame-index-based, NOT
/// wall-clock time or velocity), clamping the extrapolated position to
/// `[0,1]`. Returns `None` only once the entire window has no valid sample.
///
/// This is a faithful port of the real software's `ExtrapolatePosition`
/// (`EyesPositioningParametersCalculator`, decompiled ground truth — see
/// `MAX_HISTORY_FRAMES`'s doc comment): use the latest frame directly if
/// present; else the single valid entry if there's only one; else
/// extrapolate from the two most recent valid entries using the frame-index
/// gap between them, clamped to `[0,1]` afterward. One deliberate
/// improvement over the literal decompiled code: the original locates its
/// two reference points by re-scanning the buffer for VALUES matching `p2`/
/// `p3` (`List.FindLastIndex`/`FindIndex` comparing X/Y) — fragile if two
/// buffered points happen to share the same coordinates. This scans by
/// INDEX POSITION instead (unambiguous regardless of duplicate values),
/// which is the same algorithm with a more robust implementation, not a
/// behavioral deviation.
fn resolve_eye(buf: &VecDeque<Option<EyeSample>>) -> Option<EyeSample> {
    // This frame's own reading is valid: no extrapolation needed.
    if let Some(Some(latest)) = buf.back() {
        return Some(*latest);
    }

    let n = buf.len();
    // Scan for the two most recent valid entries (p2 = newer, p3 = older),
    // excluding the just-pushed (guaranteed-`None`) current entry.
    let mut p3: Option<(usize, EyeSample)> = None;
    let mut p2: Option<(usize, EyeSample)> = None;
    for (i, entry) in buf.iter().enumerate().take(n.saturating_sub(1)) {
        if let Some(e) = entry {
            p3 = p2;
            p2 = Some((i, *e));
        }
    }

    let (idx_p2, p2) = p2?;
    let Some((idx_p3, p3)) = p3 else {
        // Exactly one valid sample in the window: hold it, no slope to extrapolate.
        return Some(p2);
    };

    // Frame-index-based gaps (NOT wall-clock time or velocity): how many
    // frames since p2, and how many frames separated p3 from p2.
    let gap_p2 = (n - 1 - idx_p2) as f32;
    let gap_p2_p3 = (idx_p2 - idx_p3) as f32;

    let x = (p2.pos[0] + (p2.pos[0] - p3.pos[0]) * gap_p2 / gap_p2_p3).clamp(0.0, 1.0);
    let y = (p2.pos[1] + (p2.pos[1] - p3.pos[1]) * gap_p2 / gap_p2_p3).clamp(0.0, 1.0);
    let distance_mm = match (p2.distance_mm, p3.distance_mm) {
        (Some(d2), Some(d3)) => Some(d2 + (d2 - d3) * gap_p2 / gap_p2_p3),
        (Some(d2), None) => Some(d2),
        (None, _) => None,
    };

    Some(EyeSample {
        pos: [x, y],
        distance_mm,
    })
}

/// Push one frame's decoded (or absent) sample onto an eye's history buffer,
/// capping it at `MAX_HISTORY_FRAMES`.
fn push_capped(buf: &mut VecDeque<Option<EyeSample>>, sample: Option<EyeSample>) {
    buf.push_back(sample);
    if buf.len() > MAX_HISTORY_FRAMES {
        buf.pop_front();
    }
}

/// Combine both eyes' resolved samples into an `EyeView`, reusing
/// `EyeView::from_gaze`'s exact guidance thresholds/selection logic.
///
/// Requires BOTH eyes to have resolved (held/extrapolated or fresh) before
/// showing anything — matching `EyeView::from_gaze`'s original AND-gate.
///
/// This is a deliberate DISPLAY-level choice, not a per-eye-tracking one: each
/// eye's history/extrapolation in `EyeHistory` still runs fully independently
/// (a brief dropout on one eye doesn't touch the other's own state at all).
/// But live-hardware measurement showed the two eyes' invalid stretches are
/// NOT always symmetric — one eye can occasionally run out its whole
/// extrapolation window while the other is still fine, and showing only the
/// surviving eye's dot reads as a confusing, never-happens-on-the-original
/// "one eye" state (the original's own per-eye-independent status logic can
/// technically do this too, but its much more robust native position source
/// makes it rare enough not to be noticed in practice). Requiring both
/// restores `EyeView::none()` for that specific case instead — an honest
/// "can't show this reliably" rather than a half-populated view.
fn combine(left: Option<EyeSample>, right: Option<EyeSample>) -> EyeView {
    let (Some(left), Some(right)) = (left, right) else {
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
        distance_mm,
        guidance,
    }
}

/// Rolling per-eye history providing hold+linear-extrapolation across brief
/// invalid/missing frames instead of instantly reporting "no eyes" for that
/// eye — a faithful port of the real software's
/// `EyesPositioningParametersCalculator.SyncEyesPositioningParameters`/
/// `ExtrapolatePosition` (frame-index-based, NOT wall-clock-time or
/// velocity-based: a fast head movement produces a bigger per-frame delta,
/// which gets extrapolated further, but is not treated specially otherwise —
/// same as the original).
#[derive(Debug, Clone, Default)]
pub struct EyeHistory {
    left: VecDeque<Option<EyeSample>>,
    right: VecDeque<Option<EyeSample>>,
}

impl EyeHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one incoming gaze frame and return the resulting (held or
    /// extrapolated) `EyeView` for THIS frame. Call exactly once per incoming
    /// gaze notification, in arrival order — do not call this more than once
    /// per actual frame (the frame-index-based extrapolation assumes one
    /// buffer push per real frame).
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

        push_capped(&mut self.left, left_sample);
        push_capped(&mut self.right, right_sample);

        combine(resolve_eye(&self.left), resolve_eye(&self.right))
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
    /// each eye's history/extrapolation is resolved independently.
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
        // identical to the pure, stateless EyeView::from_gaze on that frame.
        let g = sample([0.6, 0.5, 0.5], [0.4, 0.5, 0.5], 680.0, true);
        let via_history = EyeHistory::new().update(&g);
        let direct = EyeView::from_gaze(&g);
        assert_eq!(via_history, direct);
    }

    #[test]
    fn brief_invalid_gap_holds_through_it_instead_of_no_eyes() {
        // Regression test for the reported bug: valid, valid, invalid x3, valid.
        // The invalid stretch must NOT report "no eyes" (it must hold/extrapolate).
        let mut hist = EyeHistory::new();
        hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, true));
        hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, true));
        for _ in 0..3 {
            let v = hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, false));
            assert!(
                !matches!(v.guidance, Guidance::NoEyes),
                "a brief gap within the history window must not report no eyes"
            );
            assert!(v.left.is_some() && v.right.is_some());
        }
        // Recovery: a subsequent valid frame is used directly again.
        let v = hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, true));
        assert!(!matches!(v.guidance, Guidance::NoEyes));
    }

    #[test]
    fn full_window_of_invalid_frames_eventually_reports_no_eyes() {
        // The hold/extrapolation window is finite: once MAX_HISTORY_FRAMES
        // consecutive invalid frames have pushed every valid sample out of the
        // buffer, the eye must be reported absent again (no infinite buffering).
        let mut hist = EyeHistory::new();
        hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, true));
        let mut last = None;
        for _ in 0..MAX_HISTORY_FRAMES {
            last = Some(hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, false)));
        }
        let v = last.unwrap();
        assert!(matches!(v.guidance, Guidance::NoEyes));
        assert!(v.left.is_none() && v.right.is_none());
    }

    #[test]
    fn single_historical_sample_holds_without_extrapolating() {
        // Only one valid sample in the window: no slope to derive, so the
        // position must hold unchanged (and must not panic on a would-be
        // divide-by-zero from a missing second sample).
        let mut hist = EyeHistory::new();
        let v0 = hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, true));
        let v1 = hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, false));
        assert_eq!(v1.left, v0.left);
        assert_eq!(v1.right, v0.right);
        assert!(!matches!(v1.guidance, Guidance::NoEyes));
    }

    #[test]
    fn extrapolates_in_the_direction_of_movement() {
        // Two valid samples showing movement (raw x decreasing => mirrored x
        // increasing), then a gap: the extrapolated position must continue
        // moving in that same direction, by the expected amount.
        let mut hist = EyeHistory::new();
        hist.update(&sample([0.7, 0.5, 0.5], [0.7, 0.5, 0.5], 680.0, true)); // mirrored x=0.3
        let v_second = hist.update(&sample([0.6, 0.5, 0.5], [0.6, 0.5, 0.5], 680.0, true)); // mirrored x=0.4
        let v_gap = hist.update(&sample([0.6, 0.5, 0.5], [0.6, 0.5, 0.5], 680.0, false));

        let x_second = v_second.left.unwrap()[0];
        let x_extrapolated = v_gap.left.unwrap()[0];
        assert!(
            x_extrapolated > x_second,
            "expected continued movement in the same direction: {x_extrapolated} <= {x_second}"
        );
        assert!((x_extrapolated - 0.5).abs() < 1e-5);
    }

    #[test]
    fn extrapolated_position_is_clamped_to_unit_range() {
        // Two valid samples trending toward the upper edge; the frame-index
        // extrapolation from them would overshoot past 1.0 — must clamp.
        let mut hist = EyeHistory::new();
        hist.update(&sample([0.2, 0.5, 0.5], [0.2, 0.5, 0.5], 680.0, true)); // mirrored 0.8
        hist.update(&sample([0.05, 0.5, 0.5], [0.05, 0.5, 0.5], 680.0, true)); // mirrored 0.95
        let v = hist.update(&sample([0.05, 0.5, 0.5], [0.05, 0.5, 0.5], 680.0, false));
        // Unclamped this would be 0.95 + (0.95 - 0.8) = 1.10.
        assert!((v.left.unwrap()[0] - 1.0).abs() < 1e-6);
        assert!((v.right.unwrap()[0] - 1.0).abs() < 1e-6);

        // Same check trending toward the lower edge.
        let mut hist = EyeHistory::new();
        hist.update(&sample([0.8, 0.5, 0.5], [0.8, 0.5, 0.5], 680.0, true)); // mirrored 0.2
        hist.update(&sample([0.95, 0.5, 0.5], [0.95, 0.5, 0.5], 680.0, true)); // mirrored 0.05
        let v = hist.update(&sample([0.95, 0.5, 0.5], [0.95, 0.5, 0.5], 680.0, false));
        // Unclamped this would be 0.05 + (0.05 - 0.2) = -0.10.
        assert!(v.left.unwrap()[0].abs() < 1e-6);
        assert!(v.right.unwrap()[0].abs() < 1e-6);
    }

    #[test]
    fn each_eye_is_resolved_independently() {
        // The left eye has a brief gap; the right eye stays continuously
        // valid. Each eye's OWN history/hold must be computed independently
        // (a left-eye gap must not touch the right eye's own resolved
        // value) — this is about `EyeHistory`'s internal per-eye tracking,
        // not the DISPLAY gate (see `both_eyes_required_to_show_anything`
        // below for that): here the left eye is still held (`Some`), so
        // both eyes are populated and this doesn't exercise the gate at all.
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
        // Right eye: mirrored 1-0.3=0.7, exactly as this frame's own reading.
        assert!((v.right.unwrap()[0] - 0.7).abs() < 1e-6);
        // Left eye: held from the prior valid frame, not dropped.
        assert!(v.left.is_some());
        assert!(!matches!(v.guidance, Guidance::NoEyes));
    }

    #[test]
    fn both_eyes_required_to_show_anything() {
        // Regression test: live-hardware capture showed the two eyes' invalid
        // stretches are not symmetric — one eye's ENTIRE extrapolation window
        // can exhaust (genuinely `None`) while the other stays continuously
        // valid. Showing just the surviving eye's dot reads as a confusing
        // "one eye only" state that never happens on the raw, pre-history
        // display (which ANDs both eyes together) — `combine` must require
        // BOTH eyes to have resolved before showing anything, falling back
        // to `EyeView::none()` otherwise, even though the right eye here is
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
        for _ in 0..MAX_HISTORY_FRAMES {
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
            "must show NoEyes, not a lone right eye, once the left eye's window is exhausted"
        );
        assert!(v.left.is_none() && v.right.is_none());
    }
}
