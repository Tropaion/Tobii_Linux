//! Pure mapping from a decoded gaze sample to a renderable eye-position view.
//! No GUI-toolkit types here — the widget (hub/flows) draws from this.
//!
//! # Provenance
//!
//! This is a faithful port of the original Windows software's own eye-position
//! screen, recovered by decompiling
//! `Tobii.Configuration.Common.EyePositioning.EyesPositioningParametersCalculator`
//! (+ `EyePositioningUtils` for the colour/alpha rules and
//! `Calibration.ViewModels.EyesPositioningViewModel` for the message damping).
//! Native `tobii_stream_engine.dll` was also disassembled end to end and
//! confirmed to be a pure pass-through — it holds no smoothing whatsoever — so
//! the algorithm below is the *complete* original behaviour, not an
//! approximation of it. Every constant here is quoted from that source; the
//! comments name the original identifier so it stays auditable.
//!
//! Coordinate + distance conventions were confirmed against a live ET5
//! (commit 3a980d1):
//! - trackbox columns (0x03/0x09) give eye x/y **normalized `[0,1]`** in the
//!   tracker's camera frame, plus a **normalized z** (NOT millimetres) — which
//!   is exactly the quantity the original algorithm consumes;
//! - the operating **distance in mm** comes from the eye-origin columns
//!   (0x02/0x08) z, and is carried through only for the human-readable readout;
//! - the camera frame is left-right mirrored vs. the user, so x is flipped so
//!   the view reads like a mirror (you move left → your dot moves left). The
//!   original does the same, via `x - (x - 0.5) * 2`, which is just `1 - x`.

use tobii_protocol::gaze::present;
use tobii_protocol::GazeSample;

/// Per-eye sliding window of recent readings used for gap extrapolation.
/// Original: `MaxCountOfExtrapolatedGazeDataPosition = 11`.
const WINDOW: usize = 11;

/// Depth readings averaged for the smoothed distance. Original: `BufferSize = 3`.
const DISTANCE_BUFFER: usize = 3;

/// Trackbox edges, in normalized coords, past which the user is nudged back
/// toward the middle. Original: `_xyMin = 0.1`, `_xyMax = 0.9`.
const XY_MIN: f32 = 0.1;
const XY_MAX: f32 = 0.9;

/// Normalized-depth breakpoints. Original: `MinDistance = 0.2`,
/// `CloseEdge = 0.4`, `FarEdge = 0.65`, `MaxDistance = 0.8`.
const D_NEAR: f64 = 0.2;
const D_CLOSE_EDGE: f64 = 0.4;
const D_FAR_EDGE: f64 = 0.65;
const D_FAR: f64 = 0.8;

/// Rendered eye sizes. Original: `SmallEyeSize = 25`, `DefaultEyeSize = 50`,
/// `BigEyeSize = 100`. The dot grows as you lean in and shrinks as you move
/// away, which is the original's primary distance feedback.
pub const EYE_SIZE_SMALL: i32 = 25;
pub const EYE_SIZE_DEFAULT: i32 = 50;
pub const EYE_SIZE_BIG: i32 = 100;

/// Slopes of the two linear size ramps. Original: `_eyesSizeCloseLineM = 250.0`
/// and `_eyesSizeFarLineM = 166.66666666666663` (i.e. `500/3`), chosen so the
/// piecewise function is continuous at both joins.
const CLOSE_LINE_M: f64 = 250.0;
const FAR_LINE_M: f64 = 500.0 / 3.0;

/// How far (normalized) from a trackbox edge a dot starts fading out, reaching
/// fully transparent exactly at the edge. Original: the `0.2` in
/// `EyePositioningUtils.DarkenEyeColor`.
const EDGE_FADE_MARGIN: f32 = 0.2;

/// The dimmest a dot gets at a distance extreme: RGB(50,50,50) of 255.
/// Original: `GetVeryDarkGrayColor`.
const MIN_BRIGHTNESS: f32 = 50.0 / 255.0;

/// Head-tilt damping. Original: the `0.67` factor in
/// `EyesPositioningViewModel.Refresh`'s `EyeAngle` computation.
const TILT_DAMPING: f64 = 0.67;

/// Frames of a *steady* non-ideal reading required before the guidance text
/// switches away from "you're fine". Original:
/// `MaxCountOfInvalidGazeDataFrom2To3 = 11`.
const HYST_TO_HINT: u32 = 11;

/// Frames of steady eye loss required before admitting we cannot track.
/// Original: `MaxCountOfInvalidGazeDataFrom3To1 = MaxCountOfInvalidGazeDataFrom2To1 = 49`
/// (~1.6 s at the ~33 ms stream cadence). This heavy damping is why the
/// original's guidance text feels stable where a per-frame readout flickers.
const HYST_TO_NO_EYES: u32 = 49;

/// Where the user should move, mirroring the original's `EyesPositionStatus`.
/// `NoEyes`/`MoveBack`/`Centered` are this project's existing names for the
/// original's `CannotTrackEyes`/`LeanBack`/`Valid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guidance {
    NoEyes,
    MoveCloser,
    MoveBack,
    MoveRight,
    MoveLeft,
    MoveDown,
    MoveUp,
    Centered,
}

/// A renderable eye-position snapshot.
///
/// `left`/`right` are **mirror-view** normalized `[0,1]` coordinates (x already
/// flipped) that the widget scales into its rectangle, already gap-filled by
/// extrapolation. `left_alpha`/`right_alpha` fade each dot out as it nears the
/// trackbox edge (and are `0.0` for an absent eye). `eye_size` and `brightness`
/// are shared by both dots and encode operating distance. `angle_deg` is the
/// head tilt implied by the two dots. `distance_mm` is the real operating
/// distance, for the human-readable readout only.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EyeView {
    pub left: Option<[f32; 2]>,
    pub right: Option<[f32; 2]>,
    pub left_alpha: f32,
    pub right_alpha: f32,
    pub eye_size: i32,
    pub brightness: f32,
    pub angle_deg: f32,
    pub distance_mm: Option<f32>,
    pub guidance: Guidance,
}

impl EyeView {
    /// The "nothing to show" view. Used both for a sample carrying no usable
    /// eyes and, by the widget, when the device is not connected at all.
    pub fn none() -> EyeView {
        EyeView {
            left: None,
            right: None,
            left_alpha: 0.0,
            right_alpha: 0.0,
            eye_size: 0,
            brightness: MIN_BRIGHTNESS,
            angle_deg: 0.0,
            distance_mm: None,
            guidance: Guidance::NoEyes,
        }
    }

    /// Stateless single-frame view: what the original would show for this frame
    /// alone, with no history to extrapolate across or damp against.
    ///
    /// Prefer [`EyeHistory::update`] for live rendering — extrapolation and
    /// message damping are what make the original feel steady, and both need
    /// history. This exists for one-shot rendering and as a test oracle.
    pub fn from_gaze(s: &GazeSample) -> EyeView {
        let mut hist = EyeHistory::new();
        hist.update(s)
    }
}

/// One eye's decoded reading for a single frame: mirror-view trackbox position,
/// the normalized depth that drives dot size, and the real distance in mm for
/// the readout.
#[derive(Clone, Copy, Debug, PartialEq)]
struct EyeSample {
    pos: [f32; 2],
    depth_norm: f64,
    distance_mm: Option<f32>,
}

/// Decode one eye's reading: mirror x (camera frame → mirror view), pass y
/// through, take the normalized depth from the trackbox z, and the real mm
/// distance from that eye's own eye-origin z.
///
/// Keyed to a single eye rather than gating on both, because the original
/// treats the two eyes independently — one eye alone is a perfectly valid
/// state, and it is the only possible state under monocular tracking.
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
        depth_norm: trackbox[2],
        distance_mm: if origin_present {
            Some(origin_mm[2] as f32)
        } else {
            None
        },
    })
}

/// Project where an eye probably is now, given the last `WINDOW` frames of
/// readings for it (`None` = that frame had no valid reading for this eye).
///
/// Faithful port of the original's `ExtrapolatePosition`:
/// - newest frame valid → use it as-is, never projected;
/// - otherwise take the two most recent valid readings and continue that
///   trend linearly across however many frames have been missed, clamped
///   back into the unit square (the original's `AdjustPointToBounderies`,
///   which despite its parameters clamps to a hard-coded `[0,1]`);
/// - exactly one valid reading in the window → hold it still;
/// - none at all → the eye is genuinely gone.
///
/// The two index lookups deliberately mirror the original's quirk of locating
/// each point by *value* — the newest index matching the latest point, but the
/// *oldest* index matching the one before it. With a still head, repeated
/// identical readings therefore widen the measured span, which damps the
/// projection rather than amplifying it.
fn extrapolate(points: &[Option<[f32; 2]>]) -> Option<[f32; 2]> {
    // Newest frame valid → no projection needed.
    if let Some(p) = points.last()? {
        return Some(*p);
    }

    let valid: Vec<[f32; 2]> = points.iter().flatten().copied().collect();
    let (&newest, &previous) = match valid.as_slice() {
        [] => return None,
        [only] => return Some(*only),
        [.., previous, newest] => (newest, previous),
    };

    // Both were taken from `points`, so both lookups always hit; holding the
    // newest reading is the only sensible fallback if that ever changes.
    let (Some(i_newest), Some(i_previous)) = (
        points.iter().rposition(|p| *p == Some(newest)),
        points.iter().position(|p| *p == Some(previous)),
    ) else {
        return Some(newest);
    };

    let missed = (points.len() - 1 - i_newest) as f32;
    let span = i_newest.saturating_sub(i_previous);
    if span == 0 {
        return Some(newest);
    }
    let per_frame = missed / span as f32;

    Some([
        (newest[0] + (newest[0] - previous[0]) * per_frame).clamp(0.0, 1.0),
        (newest[1] + (newest[1] - previous[1]) * per_frame).clamp(0.0, 1.0),
    ])
}

/// Rendered dot size for a smoothed normalized depth — the original's
/// `ComputeEyeSize`. Piecewise linear and continuous: full `EYE_SIZE_BIG` when
/// very close, ramping down to `EYE_SIZE_DEFAULT` through the comfortable band,
/// then down to `EYE_SIZE_SMALL` as the user moves away.
fn eye_size_for(depth: f64) -> i32 {
    if depth <= D_NEAR {
        EYE_SIZE_BIG
    } else if depth < D_CLOSE_EDGE {
        (EYE_SIZE_DEFAULT as f64 + CLOSE_LINE_M * (D_CLOSE_EDGE - depth)).ceil() as i32
    } else if depth <= D_FAR_EDGE {
        EYE_SIZE_DEFAULT
    } else if depth < D_FAR {
        (EYE_SIZE_DEFAULT as f64 - FAR_LINE_M * (depth - D_FAR_EDGE)).ceil() as i32
    } else {
        EYE_SIZE_SMALL
    }
}

/// Dot brightness for a dot size — the original's
/// `EyePositioningUtils.EyeSizeToEyeColor`. Brightest (white) at exactly the
/// comfortable size, dimming to near-black at either distance extreme, so the
/// dot visibly loses confidence as the user leaves the good range.
fn brightness_for(eye_size: i32) -> f32 {
    if eye_size == 0 || eye_size == EYE_SIZE_SMALL || eye_size == EYE_SIZE_BIG {
        return MIN_BRIGHTNESS;
    }
    if eye_size == EYE_SIZE_DEFAULT {
        return 1.0;
    }
    // Two linear ramps, each pinned to 255 at EYE_SIZE_DEFAULT and 50 at its
    // own extreme; written as the original derives them.
    let v = if eye_size < EYE_SIZE_DEFAULT {
        let m = 205.0 / EYE_SIZE_SMALL as f64;
        eye_size as f64 * m + (255.0 - m * EYE_SIZE_DEFAULT as f64)
    } else {
        let m = 205.0 / -(EYE_SIZE_DEFAULT as f64);
        eye_size as f64 * m + (255.0 - m * EYE_SIZE_DEFAULT as f64)
    };
    ((v / 255.0) as f32).clamp(MIN_BRIGHTNESS, 1.0)
}

/// How opaque one dot should be, from how close it sits to a trackbox edge —
/// the original's `EyePositioningUtils.DarkenEyeColor`. Fully opaque while
/// comfortably inside the box, fading to fully transparent right at the edge,
/// and transparent outright for an absent eye.
///
/// Note the deliberate asymmetry, faithful to the original: x is renormalized
/// into the valid `[XY_MIN, XY_MAX]` band first, while y is used raw.
///
/// This is what makes extrapolation feel honest rather than wrong: a projected
/// position that runs off the trackbox fades out as it goes instead of parking
/// itself at full strength against the boundary.
fn edge_alpha(pos: Option<[f32; 2]>) -> f32 {
    let Some(p) = pos else { return 0.0 };
    let x = if p[0] < XY_MIN {
        0.0
    } else if p[0] > XY_MAX {
        1.0
    } else {
        (p[0] - XY_MIN) / (XY_MAX - XY_MIN)
    };
    // Margin to the nearest edge on either axis.
    let margin = x.min(1.0 - x).min(p[1]).min(1.0 - p[1]);
    if margin <= 0.0 {
        0.0
    } else if margin < EDGE_FADE_MARGIN {
        1.0 - (EDGE_FADE_MARGIN - margin) / EDGE_FADE_MARGIN
    } else {
        1.0
    }
}

/// Head tilt implied by the two dots, in degrees, damped — the original's
/// `EyeAngle`. Zero unless both eyes are placed. Rounded to whole degrees as
/// the original does, which also quantizes away small jitter.
fn tilt_deg(left: Option<[f32; 2]>, right: Option<[f32; 2]>) -> f32 {
    let (Some(l), Some(r)) = (left, right) else {
        return 0.0;
    };
    let radians = ((r[1] - l[1]) as f64).atan2((r[0] - l[0]) as f64);
    (radians.to_degrees() * TILT_DAMPING).round() as f32
}

/// Where to nudge the user — the original's `ComputeEyesPositionStatus`.
///
/// Crucially this is an OR over the eyes: only losing *both* means we cannot
/// track. A single eye is a valid, fully-supported state (and the only one
/// available under monocular tracking). Distance wins over centring, and the
/// axis tests fire if *either* eye has breached, in the original's order.
fn guidance_for(left: Option<[f32; 2]>, right: Option<[f32; 2]>, eye_size: i32) -> Guidance {
    if left.is_none() && right.is_none() {
        return Guidance::NoEyes;
    }
    if eye_size <= EYE_SIZE_SMALL {
        return Guidance::MoveCloser;
    }
    if eye_size >= EYE_SIZE_BIG {
        return Guidance::MoveBack;
    }
    let either = |f: fn([f32; 2]) -> bool| left.is_some_and(f) || right.is_some_and(f);
    if either(|p| p[0] <= XY_MIN) {
        Guidance::MoveRight
    } else if either(|p| p[0] >= XY_MAX) {
        Guidance::MoveLeft
    } else if either(|p| p[1] <= XY_MIN) {
        Guidance::MoveDown
    } else if either(|p| p[1] >= XY_MAX) {
        Guidance::MoveUp
    } else {
        Guidance::Centered
    }
}

/// Rolling window of one eye's recent readings.
#[derive(Debug, Clone, Default)]
struct EyeWindow {
    points: Vec<Option<[f32; 2]>>,
}

impl EyeWindow {
    fn push(&mut self, p: Option<[f32; 2]>) {
        self.points.push(p);
        if self.points.len() > WINDOW {
            self.points.remove(0);
        }
    }

    fn resolve(&self) -> Option<[f32; 2]> {
        extrapolate(&self.points)
    }
}

/// Rolling mean of the last few depth readings — the original's
/// `_distanceHistory`. Smoothing here (rather than at the dot position) is what
/// keeps dot size and brightness from buzzing frame to frame.
#[derive(Debug, Clone, Default)]
struct DepthHistory {
    buf: Vec<f64>,
}

impl DepthHistory {
    /// Feed this frame's per-eye depths and get the smoothed depth, or `None`
    /// if no eye reported one (in which case the caller keeps the previous dot
    /// size, exactly as the original does).
    fn update(&mut self, depths: &[f64]) -> Option<f64> {
        if depths.is_empty() {
            return None;
        }
        self.buf
            .push(depths.iter().sum::<f64>() / depths.len() as f64);
        if self.buf.len() > DISTANCE_BUFFER {
            self.buf.remove(0);
        }
        Some(self.buf.iter().sum::<f64>() / self.buf.len() as f64)
    }
}

/// Damps the guidance *text* so it does not chatter — the original's
/// `EyesPositioningViewModel.ProcessUserPositionData`.
///
/// Becoming well-positioned shows immediately, but leaving that state is
/// deliberately slow: a nudge needs `HYST_TO_HINT` steady frames and admitting
/// we cannot track needs `HYST_TO_NO_EYES` (~1.6 s). The dots keep moving every
/// frame regardless — only the words are damped.
#[derive(Debug, Clone)]
struct GuidanceDamper {
    displayed: Guidance,
    previous: Guidance,
    count: u32,
}

impl Default for GuidanceDamper {
    fn default() -> Self {
        Self {
            displayed: Guidance::NoEyes,
            previous: Guidance::NoEyes,
            count: 0,
        }
    }
}

impl GuidanceDamper {
    fn update(&mut self, raw: Guidance) -> Guidance {
        match raw {
            // Good position: never delayed.
            Guidance::Centered => {
                self.previous = Guidance::Centered;
                self.count = 0;
                self.displayed = Guidance::Centered;
            }
            Guidance::NoEyes => {
                if self.previous == Guidance::NoEyes {
                    self.displayed = Guidance::NoEyes;
                } else {
                    self.hold_off(raw, HYST_TO_NO_EYES);
                }
            }
            // A directional or distance nudge.
            _ => {
                if self.previous == Guidance::Centered {
                    self.hold_off(raw, HYST_TO_HINT);
                } else {
                    self.count = 0;
                    self.previous = raw;
                    self.displayed = raw;
                }
            }
        }
        self.displayed
    }

    /// Require `needed` consecutive frames before letting `raw` be shown.
    fn hold_off(&mut self, raw: Guidance, needed: u32) {
        if self.count == 0 {
            self.count = 1;
            return;
        }
        self.count += 1;
        if self.count >= needed {
            self.count = 0;
            self.previous = raw;
            self.displayed = raw;
        }
    }
}

/// Rolling eye-position state: per-eye extrapolation windows, the smoothed
/// depth history, and the guidance damper. Together these reproduce the
/// original's eye-position screen — see this module's header for provenance.
#[derive(Debug, Clone, Default)]
pub struct EyeHistory {
    left: EyeWindow,
    right: EyeWindow,
    depth: DepthHistory,
    damper: GuidanceDamper,
    /// Retained across frames: with no depth reading this frame, the original
    /// leaves the previous size (and hence brightness) untouched.
    eye_size: i32,
}

impl EyeHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one incoming gaze frame and return the view to render for it.
    ///
    /// Call exactly once per incoming gaze notification, in arrival order — the
    /// extrapolation window and the text damping both count in frames.
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

        // Positions come from the extrapolated windows; size/brightness come
        // from *this frame's* raw depths, matching the original's ordering.
        self.left.push(left_sample.map(|e| e.pos));
        self.right.push(right_sample.map(|e| e.pos));
        let left = self.left.resolve();
        let right = self.right.resolve();

        let depths: Vec<f64> = [left_sample, right_sample]
            .iter()
            .flatten()
            .map(|e| e.depth_norm)
            .collect();
        if let Some(depth) = self.depth.update(&depths) {
            self.eye_size = eye_size_for(depth);
        }

        let distance_mm = match (
            left_sample.and_then(|e| e.distance_mm),
            right_sample.and_then(|e| e.distance_mm),
        ) {
            (Some(dl), Some(dr)) => Some((dl + dr) / 2.0),
            (Some(d), None) | (None, Some(d)) => Some(d),
            (None, None) => None,
        };

        let guidance = self.damper.update(guidance_for(left, right, self.eye_size));

        EyeView {
            left,
            right,
            left_alpha: edge_alpha(left),
            right_alpha: edge_alpha(right),
            eye_size: self.eye_size,
            brightness: brightness_for(self.eye_size),
            angle_deg: tilt_deg(left, right),
            distance_mm,
            guidance,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a sample with trackbox (normalized x/y + normalized z) and
    /// eye-origin mm, with both eyes sharing a validity.
    fn sample(tb_l: [f64; 3], tb_r: [f64; 3], origin_z_mm: f64, valid: bool) -> GazeSample {
        sample_split(tb_l, tb_r, origin_z_mm, valid, valid)
    }

    /// Build a sample with independent per-eye validity.
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

    /// A centered, comfortably-positioned frame: mid trackbox, mid depth.
    fn good(valid: bool) -> GazeSample {
        sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, valid)
    }

    // ---- decoding + coordinate conventions ----

    #[test]
    fn no_trackbox_columns_means_no_eyes() {
        let v = EyeView::from_gaze(&GazeSample::default());
        assert_eq!(v.guidance, Guidance::NoEyes);
        assert!(v.left.is_none() && v.right.is_none());
    }

    #[test]
    fn invalid_validity_means_no_eyes() {
        let v = EyeView::from_gaze(&good(false));
        assert_eq!(v.guidance, Guidance::NoEyes);
    }

    #[test]
    fn x_is_mirrored_and_y_passes_through() {
        let v = EyeView::from_gaze(&sample([0.6, 0.5, 0.5], [0.4, 0.5, 0.5], 680.0, true));
        assert!((v.left.unwrap()[0] - 0.4).abs() < 1e-6);
        assert!((v.right.unwrap()[0] - 0.6).abs() < 1e-6);
        assert!((v.left.unwrap()[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn distance_readout_uses_eye_origin_mm_not_trackbox_z() {
        // Trackbox z is a normalized 0.5; the readout must be the 680 mm
        // eye-origin value, and a mid-range depth reads as well-positioned.
        let v = EyeView::from_gaze(&sample([0.55, 0.5, 0.5], [0.45, 0.5, 0.5], 680.0, true));
        assert!((v.distance_mm.unwrap() - 680.0).abs() < 1e-3);
        assert_eq!(v.guidance, Guidance::Centered);
    }

    // ---- dot size: the original's piecewise-linear ramps ----

    #[test]
    fn eye_size_hits_the_originals_breakpoints() {
        assert_eq!(eye_size_for(0.0), EYE_SIZE_BIG);
        assert_eq!(eye_size_for(D_NEAR), EYE_SIZE_BIG);
        assert_eq!(eye_size_for(D_CLOSE_EDGE), EYE_SIZE_DEFAULT);
        assert_eq!(eye_size_for(0.5), EYE_SIZE_DEFAULT);
        assert_eq!(eye_size_for(D_FAR_EDGE), EYE_SIZE_DEFAULT);
        assert_eq!(eye_size_for(D_FAR), EYE_SIZE_SMALL);
        assert_eq!(eye_size_for(1.0), EYE_SIZE_SMALL);
    }

    #[test]
    fn eye_size_ramps_meet_the_plateaus_within_the_originals_rounding() {
        // The 250 and 500/3 slopes are chosen so both ramps land exactly on the
        // neighbouring plateau -- but the original rounds each ramp UP
        // (`Math.Ceiling`), so approaching a join from inside a ramp lands one
        // unit above the plateau wherever the ramp is descending toward it.
        // That 1-unit step is the original's own behaviour, quirk included.
        assert_eq!(eye_size_for(D_NEAR + 1e-9), EYE_SIZE_BIG);
        assert_eq!(eye_size_for(D_FAR_EDGE + 1e-9), EYE_SIZE_DEFAULT);
        assert_eq!(eye_size_for(D_CLOSE_EDGE - 1e-9), EYE_SIZE_DEFAULT + 1);
        assert_eq!(eye_size_for(D_FAR - 1e-9), EYE_SIZE_SMALL + 1);
    }

    #[test]
    fn only_the_true_far_plateau_asks_the_user_to_move_closer() {
        // Because the far ramp rounds up, EYE_SIZE_SMALL is reached only at the
        // plateau itself -- so "Move closer" fires there and not one step
        // inside the ramp. Guards against re-deriving the threshold as `< 26`.
        assert_eq!(
            guidance_for(
                Some([0.5, 0.5]),
                Some([0.5, 0.5]),
                eye_size_for(D_FAR - 1e-9)
            ),
            Guidance::Centered
        );
        assert_eq!(
            guidance_for(Some([0.5, 0.5]), Some([0.5, 0.5]), eye_size_for(D_FAR)),
            Guidance::MoveCloser
        );
    }

    #[test]
    fn eye_size_is_monotonically_smaller_with_distance() {
        let mut previous = i32::MAX;
        for step in 0..=100 {
            let size = eye_size_for(step as f64 / 100.0);
            assert!(size <= previous, "size grew at depth {step}");
            previous = size;
        }
    }

    #[test]
    fn leaning_in_grows_the_dot_and_moving_away_shrinks_it() {
        let close = EyeView::from_gaze(&sample([0.5, 0.5, 0.1], [0.5, 0.5, 0.1], 400.0, true));
        assert_eq!(close.eye_size, EYE_SIZE_BIG);
        assert_eq!(close.guidance, Guidance::MoveBack);

        let far = EyeView::from_gaze(&sample([0.5, 0.5, 0.9], [0.5, 0.5, 0.9], 950.0, true));
        assert_eq!(far.eye_size, EYE_SIZE_SMALL);
        assert_eq!(far.guidance, Guidance::MoveCloser);
    }

    // ---- brightness ----

    #[test]
    fn brightness_peaks_at_the_comfortable_size_and_dims_at_both_extremes() {
        assert_eq!(brightness_for(EYE_SIZE_DEFAULT), 1.0);
        assert_eq!(brightness_for(EYE_SIZE_SMALL), MIN_BRIGHTNESS);
        assert_eq!(brightness_for(EYE_SIZE_BIG), MIN_BRIGHTNESS);
        assert_eq!(brightness_for(0), MIN_BRIGHTNESS);
        // Mid-ramp values sit strictly between the two extremes.
        for size in [30, 40, 60, 80] {
            let b = brightness_for(size);
            assert!(b > MIN_BRIGHTNESS && b < 1.0, "size {size} gave {b}");
        }
    }

    #[test]
    fn brightness_ramps_are_continuous_at_the_comfortable_size() {
        // One step either side of DEFAULT must be very close to white, since
        // both ramps are pinned to 255 there.
        assert!(brightness_for(EYE_SIZE_DEFAULT - 1) > 0.95);
        assert!(brightness_for(EYE_SIZE_DEFAULT + 1) > 0.95);
    }

    // ---- edge-proximity alpha ----

    #[test]
    fn absent_eye_is_fully_transparent() {
        assert_eq!(edge_alpha(None), 0.0);
    }

    #[test]
    fn center_of_the_box_is_fully_opaque() {
        assert_eq!(edge_alpha(Some([0.5, 0.5])), 1.0);
    }

    #[test]
    fn dot_fades_to_nothing_at_the_trackbox_edge() {
        // At/outside the valid band the dot is gone; just inside it is partial.
        assert_eq!(edge_alpha(Some([XY_MIN, 0.5])), 0.0);
        assert_eq!(edge_alpha(Some([XY_MAX, 0.5])), 0.0);
        assert_eq!(edge_alpha(Some([0.5, 0.0])), 0.0);
        assert_eq!(edge_alpha(Some([0.5, 1.0])), 0.0);
        let partial = edge_alpha(Some([0.5, 0.1]));
        assert!(partial > 0.0 && partial < 1.0, "got {partial}");
    }

    #[test]
    fn alpha_increases_monotonically_away_from_the_edge() {
        let mut previous = -1.0;
        for step in 0..=50 {
            let y = step as f32 / 100.0; // 0.0 ..= 0.5
            let a = edge_alpha(Some([0.5, y]));
            assert!(a >= previous, "alpha dropped moving inward at y={y}");
            previous = a;
        }
        assert_eq!(previous, 1.0);
    }

    // ---- extrapolation: the ported ExtrapolatePosition ----

    #[test]
    fn empty_window_and_all_invalid_window_yield_nothing() {
        assert_eq!(extrapolate(&[]), None);
        assert_eq!(extrapolate(&[None, None, None]), None);
    }

    #[test]
    fn valid_newest_frame_is_used_verbatim() {
        // Even with movement history, a valid newest frame is never projected.
        let got = extrapolate(&[Some([0.1, 0.1]), Some([0.2, 0.2]), Some([0.3, 0.3])]);
        assert_eq!(got, Some([0.3, 0.3]));
    }

    #[test]
    fn single_valid_reading_is_held_still() {
        assert_eq!(extrapolate(&[Some([0.4, 0.6]), None]), Some([0.4, 0.6]));
        assert_eq!(
            extrapolate(&[None, Some([0.4, 0.6]), None, None]),
            Some([0.4, 0.6])
        );
    }

    #[test]
    fn gap_continues_the_trend_one_frame_per_frame_missed() {
        // Two readings one frame apart moving +0.1/frame in x, then one missed
        // frame: expect exactly one more step of that trend.
        let got = extrapolate(&[Some([0.2, 0.5]), Some([0.3, 0.5]), None]).unwrap();
        assert!((got[0] - 0.4).abs() < 1e-6, "got {got:?}");
        assert!((got[1] - 0.5).abs() < 1e-6, "got {got:?}");

        // Two frames missed: two steps.
        let got = extrapolate(&[Some([0.2, 0.5]), Some([0.3, 0.5]), None, None]).unwrap();
        assert!((got[0] - 0.5).abs() < 1e-6, "got {got:?}");
    }

    #[test]
    fn projection_scales_by_the_span_between_the_two_readings() {
        // Readings two frames apart (+0.2 over 2 frames = 0.1/frame), then one
        // missed frame: one 0.1 step, not a full 0.2.
        let got = extrapolate(&[Some([0.2, 0.5]), None, Some([0.4, 0.5]), None]).unwrap();
        assert!((got[0] - 0.5).abs() < 1e-6, "got {got:?}");
    }

    #[test]
    fn projection_is_clamped_into_the_unit_square() {
        // A fast outward trend must stop at the boundary, never overshoot it.
        let got = extrapolate(&[Some([0.7, 0.5]), Some([0.95, 0.5]), None, None]).unwrap();
        assert!(got[0] <= 1.0 && got[0] >= 0.0, "got {got:?}");
        assert_eq!(got[0], 1.0);

        let got = extrapolate(&[Some([0.3, 0.3]), Some([0.05, 0.05]), None, None]).unwrap();
        assert_eq!(got, [0.0, 0.0]);
    }

    #[test]
    fn a_still_head_projects_no_movement() {
        // Identical repeated readings imply zero velocity: the held position
        // must not drift, however long the gap.
        let got = extrapolate(&[Some([0.5, 0.5]), Some([0.5, 0.5]), None, None, None]);
        assert_eq!(got, Some([0.5, 0.5]));
    }

    // ---- history integration ----

    #[test]
    fn brief_gap_is_bridged_instead_of_reporting_no_eyes() {
        // The original reported bug: a momentary dropout must not flip the
        // display to "no eyes".
        let mut hist = EyeHistory::new();
        hist.update(&good(true));
        hist.update(&good(true));
        for _ in 0..3 {
            let v = hist.update(&good(false));
            assert_ne!(v.guidance, Guidance::NoEyes);
            assert!(v.left.is_some() && v.right.is_some());
        }
    }

    #[test]
    fn window_bounds_how_long_a_gap_can_be_bridged() {
        // Once the whole window holds no valid reading the eyes are reported
        // absent again — the bridge is finite.
        let mut hist = EyeHistory::new();
        hist.update(&good(true));
        let mut last = None;
        for _ in 0..WINDOW {
            last = Some(hist.update(&good(false)));
        }
        let v = last.unwrap();
        assert!(v.left.is_none() && v.right.is_none());
        assert_eq!(v.left_alpha, 0.0);
        assert_eq!(v.right_alpha, 0.0);
    }

    #[test]
    fn each_eye_extrapolates_independently() {
        // A gap on the left eye must not disturb the right eye's own reading.
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
        // Right eye: exactly this frame's own mirrored reading, 1-0.3=0.7.
        assert!((v.right.unwrap()[0] - 0.7).abs() < 1e-6);
        // Left eye: bridged from the previous frame, not dropped.
        assert!(v.left.is_some());
    }

    #[test]
    fn one_eye_alone_is_a_valid_state() {
        // The original only reports "cannot track" when BOTH eyes are gone --
        // this is also the only state monocular tracking can ever produce, so
        // a both-eyes gate would make single-eye mode show nothing at all.
        let mut hist = EyeHistory::new();
        let mut last = None;
        for _ in 0..(WINDOW + 2) {
            last = Some(hist.update(&sample_split(
                [0.5, 0.5, 0.5],
                [0.3, 0.5, 0.5],
                680.0,
                false, // left eye never valid
                true,  // right eye always valid
            )));
        }
        let v = last.unwrap();
        assert!(v.left.is_none(), "left eye should be genuinely absent");
        assert!(v.right.is_some(), "right eye must still be shown alone");
        assert_ne!(
            v.guidance,
            Guidance::NoEyes,
            "one tracked eye must not report NoEyes"
        );
        assert_eq!(v.right_alpha, 1.0, "the surviving eye renders fully opaque");
        assert_eq!(v.left_alpha, 0.0);
    }

    #[test]
    fn dot_size_holds_through_a_gap_rather_than_collapsing() {
        // With no depth reading this frame the original leaves EyeSize alone.
        let mut hist = EyeHistory::new();
        let before = hist.update(&good(true)).eye_size;
        let during = hist.update(&good(false)).eye_size;
        assert_eq!(during, before);
        assert_ne!(during, 0);
    }

    #[test]
    fn depth_is_smoothed_over_several_frames() {
        // A single far outlier must not swing the size all the way, because the
        // depth feeding it is a rolling mean.
        let mut hist = EyeHistory::new();
        for _ in 0..DISTANCE_BUFFER {
            hist.update(&sample([0.5, 0.5, 0.5], [0.5, 0.5, 0.5], 680.0, true));
        }
        let steady = hist.update(&good(true)).eye_size;
        assert_eq!(steady, EYE_SIZE_DEFAULT);

        // One frame at a much greater depth: the mean moves only partway, so
        // the size must not jump straight to the far extreme.
        let jolted = hist
            .update(&sample([0.5, 0.5, 1.0], [0.5, 0.5, 1.0], 950.0, true))
            .eye_size;
        assert!(
            jolted > EYE_SIZE_SMALL,
            "a single outlier should not reach the far extreme, got {jolted}"
        );
    }

    // ---- directional guidance ----

    #[test]
    fn each_box_edge_names_the_direction_to_move() {
        // Raw x=0.95 mirrors to 0.05, i.e. hard against the mirror-view left.
        let cases: [([f64; 3], Guidance); 4] = [
            ([0.95, 0.5, 0.5], Guidance::MoveRight),
            ([0.05, 0.5, 0.5], Guidance::MoveLeft),
            ([0.5, 0.05, 0.5], Guidance::MoveDown),
            ([0.5, 0.95, 0.5], Guidance::MoveUp),
        ];
        for (tb, want) in cases {
            let got = guidance_for(
                Some([1.0 - tb[0] as f32, tb[1] as f32]),
                Some([1.0 - tb[0] as f32, tb[1] as f32]),
                EYE_SIZE_DEFAULT,
            );
            assert_eq!(got, want, "trackbox {tb:?}");
        }
    }

    #[test]
    fn distance_guidance_outranks_centering() {
        // Too close AND against an edge: the original reports the distance.
        let corner = Some([0.02, 0.02]);
        assert_eq!(
            guidance_for(corner, corner, EYE_SIZE_BIG),
            Guidance::MoveBack
        );
        assert_eq!(
            guidance_for(corner, corner, EYE_SIZE_SMALL),
            Guidance::MoveCloser
        );
    }

    #[test]
    fn either_eye_breaching_an_edge_is_enough() {
        assert_eq!(
            guidance_for(Some([0.5, 0.5]), Some([0.95, 0.5]), EYE_SIZE_DEFAULT),
            Guidance::MoveLeft
        );
        assert_eq!(
            guidance_for(Some([0.05, 0.5]), Some([0.5, 0.5]), EYE_SIZE_DEFAULT),
            Guidance::MoveRight
        );
    }

    // ---- head tilt ----

    #[test]
    fn level_eyes_report_no_tilt() {
        assert_eq!(tilt_deg(Some([0.4, 0.5]), Some([0.6, 0.5])), 0.0);
    }

    #[test]
    fn tilt_is_signed_and_damped() {
        // Right eye lower than left tilts one way, higher the other, and the
        // magnitude is damped below the raw geometric angle (45 deg here).
        let down = tilt_deg(Some([0.4, 0.4]), Some([0.6, 0.6]));
        let up = tilt_deg(Some([0.4, 0.6]), Some([0.6, 0.4]));
        assert!(down > 0.0 && up < 0.0, "down={down} up={up}");
        assert_eq!(down, -up);
        assert!(down < 45.0, "tilt should be damped, got {down}");
    }

    #[test]
    fn tilt_needs_both_eyes() {
        assert_eq!(tilt_deg(None, Some([0.6, 0.5])), 0.0);
        assert_eq!(tilt_deg(Some([0.4, 0.5]), None), 0.0);
    }

    // ---- guidance damping ----

    #[test]
    fn becoming_well_positioned_shows_immediately() {
        let mut d = GuidanceDamper::default();
        assert_eq!(d.update(Guidance::Centered), Guidance::Centered);
    }

    #[test]
    fn leaving_a_good_position_holds_the_message_briefly() {
        // A nudge after being centered needs HYST_TO_HINT steady frames.
        let mut d = GuidanceDamper::default();
        d.update(Guidance::Centered);
        for frame in 1..HYST_TO_HINT {
            assert_eq!(
                d.update(Guidance::MoveLeft),
                Guidance::Centered,
                "changed too early at frame {frame}"
            );
        }
        assert_eq!(d.update(Guidance::MoveLeft), Guidance::MoveLeft);
    }

    #[test]
    fn admitting_we_cannot_track_takes_much_longer() {
        // Losing the eyes after being centered must not flip the message for
        // HYST_TO_NO_EYES frames -- this is the anti-flicker behaviour.
        let mut d = GuidanceDamper::default();
        d.update(Guidance::Centered);
        for frame in 1..HYST_TO_NO_EYES {
            assert_eq!(
                d.update(Guidance::NoEyes),
                Guidance::Centered,
                "gave up too early at frame {frame}"
            );
        }
        assert_eq!(d.update(Guidance::NoEyes), Guidance::NoEyes);
    }

    #[test]
    fn a_momentary_dropout_never_changes_the_message() {
        // Well short of the threshold, then recovered: the user sees no churn.
        let mut d = GuidanceDamper::default();
        d.update(Guidance::Centered);
        for _ in 0..5 {
            assert_eq!(d.update(Guidance::NoEyes), Guidance::Centered);
        }
        assert_eq!(d.update(Guidance::Centered), Guidance::Centered);
    }

    #[test]
    fn switching_between_nudges_is_not_delayed() {
        // Damping only guards leaving the good state; once nudging, the
        // direction tracks immediately so the advice stays useful.
        let mut d = GuidanceDamper::default();
        d.update(Guidance::Centered);
        for _ in 0..HYST_TO_HINT {
            d.update(Guidance::MoveLeft);
        }
        assert_eq!(d.update(Guidance::MoveLeft), Guidance::MoveLeft);
        assert_eq!(d.update(Guidance::MoveUp), Guidance::MoveUp);
        assert_eq!(d.update(Guidance::MoveCloser), Guidance::MoveCloser);
    }

    #[test]
    fn from_gaze_matches_a_fresh_history_on_its_first_frame() {
        let g = sample([0.6, 0.5, 0.5], [0.4, 0.5, 0.5], 680.0, true);
        assert_eq!(EyeView::from_gaze(&g), EyeHistory::new().update(&g));
    }
}
