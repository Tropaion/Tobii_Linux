//! Neural 6-DOF head pose: opentrack's `head-pose-0.5-small.onnx`, on `tract`.
//!
//! This is the backend that supplies the one degree of freedom two eye origins
//! can never give — **pitch**. Everything else about it is in service of making
//! that number trustworthy.
//!
//! # The pipeline
//!
//! ```text
//! CameraFrame 280x280 u8
//!   -> Roi (sub-pixel, square)          seeded from the corneal glints
//!   -> sample_patch   129x129 u8        bilinear, replicate border
//!   -> normalize      129x129 f32       opentrack's adaptive brightness gain
//!   -> tract                            pos_size, quat, box, *_scales
//!   -> next_roi = box                   the ROI for the NEXT frame
//!   -> image->world, perspective correction, Euler
//! ```
//!
//! # Three things here are load-bearing and look optional
//!
//! **1. The box-feedback loop.** The model's own `box` output becomes the next
//! frame's ROI. This is not a refinement: pitch is strongly confounded with crop
//! *scale* (a 0.70x-1.50x zoom sweep moves pitch 19.6 degrees while yaw moves
//! under 2), and the loop is what removes the confound by driving the crop to a
//! fixed scale relative to the head. Measured on the ET5 test frame: the
//! one-shot crop [`crate::preprocess::preprocess`] produces lands 8 degrees off
//! in pitch — *and passes the confidence gate while doing it*.
//!
//! **2. The sub-pixel ROI.** [`crate::preprocess::BBox`] is integer and
//! [`crate::preprocess::crop_resize`] is nearest-neighbour. Running the loop
//! that way, on a frozen image, produces 4.06 degrees of peak-to-peak pitch
//! jitter purely from ROI quantisation, against 0.30 for a sub-pixel bilinear
//! sampler. That is why [`Roi`] exists instead of reusing `BBox`.
//!
//! **3. The ROI is updated on every frame, before the confidence gate.** From a
//! whole-frame start, sigma passes through 0.50, 0.94, 0.80, 0.88, 0.75, 0.51,
//! 0.20 before locking on at 0.08. A gate that suppressed the ROI update on
//! those frames would leave the ROI stuck where it was, forever. Sigma decides
//! whether a pose is *emitted*; it never decides whether the ROI *moves*.
//!
//! # What is measured and what is assumed
//!
//! Everything about the model's numerics is measured: the tensor contract, the
//! quaternion component order, the normalisation, the convergence, the
//! confidence band. See the individual items.
//!
//! What is **not** measured is how the model's rotation sits against the
//! physical world — the sign of yaw and roll relative to
//! [`crate::HeadPose`]'s documented conventions, the absolute pitch zero, and
//! the camera's focal length. Those need the user to move their head in front
//! of the tracker while both streams are recorded, which no amount of analysis
//! substitutes for. They are collected into [`Signs`] and [`OnnxPose::set_focal_px`]
//! so that fixing them is a constant, never a rewrite, and `tobii headpose
//! --check` prints exactly the comparison that settles them.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use tract_onnx::prelude::*;

use tobii_protocol::CameraFrame;

use crate::model::{ModelConfig, ModelKind, PoseModel};
use crate::{model_store, preprocess, sha256, HeadPose};

/// The model's input is a fixed 129x129 single-channel plane. Not negotiable:
/// the ONNX graph declares `x [1, 1, 129, 129]` concretely, and the file's own
/// metadata carries `input_resolution = 129`.
pub const INPUT: usize = 129;

/// Reference head height in millimetres, used only by the model's own position
/// estimate. opentrack's value; the ET5's eye origins are a real measurement and
/// are preferred wherever they are available (see [`fuse`]).
pub const HEAD_SIZE_MM: f64 = 200.0;

/// Focal length in pixels, until the device tells us better.
///
/// **[ASSUMED]** — this corresponds to a head at z ~= 680 mm (hFOV ~= 43
/// degrees) and is the single largest uncertainty in the pipeline, because the
/// perspective correction it feeds is worth 12-16 degrees of pitch.
///
/// It is an assumption with a bounded cost, not a guess in the dark: two
/// independent estimators (glint separation against a 63 mm interocular
/// distance, and the model's own `size` output against a 200 mm head) agree to
/// within 1% at every distance, so only the absolute distance is missing. Over
/// the plausible bracket z in [500, 900] mm, f spans [261, 471] px and the
/// correction spans 9.4-16.6 degrees — a +/-4 degree pitch bias, against the
/// ~28 degree swing that *omitting* the correction produces.
///
/// [`focal_from_eye_origins`] computes the real value from data already on the
/// wire; feed it through [`OnnxPose::set_focal_px`] and this default is unused.
pub const DEFAULT_FOCAL_PX: f32 = 355.0;

/// Reject a pose whose predicted rotation sigma is at least this.
///
/// **[CONFIRMED]** by measurement across 26 probed inputs. Every real face,
/// under every geometric perturbation tried, scored 0.064-0.135; the best
/// garbage of any kind scored 0.423, and a real empty-room capture scored
/// 0.90-1.06. Nothing was ever observed between 0.18 and 0.42, so the threshold
/// sits in the middle of a 5.4x empty band and is not delicate.
///
/// What it does **not** catch: a badly *scaled* crop. The one-shot crop scores
/// 0.093 — a clean pass — while carrying 8 degrees of pitch error. The
/// box-feedback loop is what protects against that; sigma is not a substitute.
pub const SIGMA_MAX: f32 = 0.15;

/// Reseed the ROI after this many consecutive rejections (~0.3 s at 33 Hz).
/// A whole-frame reseed re-converges in ~7 frames, so this is cheap insurance
/// against the ROI locking onto something that is not a head.
pub const SIGMA_RESEED_FRAMES: u32 = 10;

/// Pixel value at or above which a pixel is taken to be a corneal glint.
///
/// **[CONFIRMED]** on the ET5 test frame: at 255 only 4 pixels qualify and they
/// all fall in *one* glint, so the pair splitter fails; at 180 there are 8
/// pixels forming two clean clusters whose centroids match the hand-measured
/// eye centres exactly; at 100, 22 pixels give the same centroids to 0.2 px.
/// Do not "simplify" this to 255.
pub const GLINT_THRESHOLD: u8 = 180;

/// Rejection band for a glint pair, in pixels. Outside it the two clusters are
/// not a pair of eyes at 40-100 cm.
const GLINT_IPD_MIN: f32 = 12.0;
const GLINT_IPD_MAX: f32 = 80.0;

/// Seed geometry, from the converged attractor on the ET5 test frame: the crop
/// the loop settles on is ~2.86x the glint separation, centred ~0.5x that
/// separation *below* the glints (i.e. on the middle of the face, not the eyes).
const SEED_SIDE_PER_IPD: f32 = 2.86;
const SEED_DROP_PER_IPD: f32 = 0.50;

/// Guard rails on the fed-back ROI. Outside these the loop has lost the head and
/// is reseeded rather than followed.
const ROI_MIN_PX: f32 = 16.0;
const ROI_MAX_FRAC: f32 = 1.5;

/// Sign conventions that relate the model's rotation to the physical world.
///
/// **Every field here is [ASSUMED].** The model's *internal* consistency is
/// proven — mirroring the input negates exactly `quat.y` and `quat.z`, which is
/// what identified the component order — but which physical direction a positive
/// number means depends on whether the ET5's camera feed is mirrored, and that
/// has never been checked against the device.
///
/// They live in one struct, settable at runtime, so that the fix when the
/// answer arrives is a constant and not a hunt through the file. `tobii headpose
/// --check` prints the model's yaw and roll beside [`crate::pose_from_eyes`]'s,
/// which is the comparison that settles them: the two must move *together*.
///
/// Note the crate's conventions are the target ([`crate::HeadPose`]: `+yaw` =
/// the user turns to their own right, `+roll` = the user tilts to their own
/// right), not opentrack's. opentrack's `+roll` is a tilt toward the subject's
/// *left* shoulder, which is why `roll` defaults to `-1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Signs {
    pub yaw: f64,
    /// **[CONFIRMED]** relative to opentrack, whose `data[Pitch]` is positive
    /// when looking up, and which is the wire format this ultimately feeds. The
    /// *zero* is a separate matter — see [`Signs::pitch_offset_deg`].
    pub pitch: f64,
    pub roll: f64,
    /// Lateral sign for the model's own position estimate. Only reachable in the
    /// eyes-untracked fallback, since [`fuse`] otherwise takes position from the
    /// eye origins.
    pub x: f64,
    /// Added to pitch after [`Signs::pitch`].
    ///
    /// **Uncalibrated, and known to be non-zero.** Two constant offsets are
    /// folded in here that nothing in this file can separate: the training
    /// dataset's own pose convention, and the ET5's physical camera tilt (the
    /// tracker sits below the screen and looks *up* — the gaze pipeline
    /// measured its mounting rotation at exactly -20.00 degrees). A user sitting
    /// square-on to the screen should read 0; whatever they actually read is
    /// this offset, negated.
    pub pitch_offset_deg: f64,
}

impl Default for Signs {
    fn default() -> Self {
        Self {
            yaw: 1.0,
            pitch: 1.0,
            roll: -1.0,
            x: -1.0,
            pitch_offset_deg: 0.0,
        }
    }
}

/// A square region of interest in **continuous** pixel coordinates: the frame's
/// top-left corner is `(0.0, 0.0)`, pixel `(i, j)` covers `[i, i+1) x [j, j+1)`
/// and is centred at `(i + 0.5, j + 0.5)`. A whole-frame ROI on a 280x280 frame
/// is therefore `{ cx: 140.0, cy: 140.0, side: 280.0 }`.
///
/// [`crate::preprocess::BBox`] cannot express this — its fields are `u32` — and
/// that is the entire reason this type exists. See the module docs for the 13x
/// jitter measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Roi {
    pub cx: f32,
    pub cy: f32,
    /// Full edge length, not a half-extent.
    pub side: f32,
}

impl Roi {
    /// The whole of a `w` x `h` frame.
    pub fn whole(w: f32, h: f32) -> Roi {
        Roi {
            cx: w * 0.5,
            cy: h * 0.5,
            side: w.max(h),
        }
    }

    /// Map a patch-normalised coordinate (the `[-1, +1]` frame the model's
    /// `pos_size` and `box` outputs live in) to frame pixels.
    pub fn to_frame(&self, nx: f32, ny: f32) -> (f32, f32) {
        let half = 0.5 * self.side;
        (self.cx + half * nx, self.cy + half * ny)
    }

    fn plausible(&self, frame_side: f32) -> bool {
        self.cx.is_finite()
            && self.cy.is_finite()
            && self.side.is_finite()
            && self.side >= ROI_MIN_PX
            && self.side <= ROI_MAX_FRAC * frame_side
    }
}

/// Sample a `129x129` patch from `roi` with bilinear interpolation and a
/// replicated border.
///
/// The border matters: zero-padding an ROI that overhangs the frame paints a
/// hard black edge, which the network reads as structure. `cv::getRectSubPix`,
/// which opentrack uses, replicates — so does this.
pub fn sample_patch(frame: &CameraFrame, roi: Roi, out: &mut [u8; INPUT * INPUT]) {
    let (w, h) = (frame.width as i32, frame.height as i32);
    let n = INPUT as f32;
    for v in 0..INPUT {
        let sy = roi.cy + roi.side * ((v as f32 + 0.5) / n - 0.5);
        for u in 0..INPUT {
            let sx = roi.cx + roi.side * ((u as f32 + 0.5) / n - 0.5);
            out[v * INPUT + u] = bilinear(frame, sx, sy, w, h);
        }
    }
}

fn bilinear(frame: &CameraFrame, sx: f32, sy: f32, w: i32, h: i32) -> u8 {
    let (fx, fy) = (sx - 0.5, sy - 0.5);
    let (x0f, y0f) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0f, fy - y0f);
    let (x0, y0) = (x0f as i32, y0f as i32);
    let px = |x: i32, y: i32| -> f32 {
        let x = x.clamp(0, w - 1);
        let y = y.clamp(0, h - 1);
        frame.pixels[(y * w + x) as usize] as f32
    };
    let top = px(x0, y0) * (1.0 - tx) + px(x0 + 1, y0) * tx;
    let bot = px(x0, y0 + 1) * (1.0 - tx) + px(x0 + 1, y0 + 1) * tx;
    (top * (1.0 - ty) + bot * ty).round().clamp(0.0, 255.0) as u8
}

/// The 90th-percentile intensity of a patch: the lowest level whose cumulative
/// count *strictly exceeds* 90% of the pixels.
pub fn intensity_quantile(patch: &[u8; INPUT * INPUT]) -> u8 {
    let mut hist = [0u32; 256];
    for &p in patch.iter() {
        hist[p as usize] += 1;
    }
    let target = (INPUT * INPUT * 90 / 100) as u32; // 14976
    let mut acc = 0u32;
    for (level, &count) in hist.iter().enumerate() {
        acc += count;
        if acc > target {
            return level as u8;
        }
    }
    0
}

/// The multiplier that maps 8-bit levels onto the model's input range.
///
/// opentrack's `normalize_brightness`, verbatim: put the 90th percentile at
/// mid-scale (`0.45 = 0.9 * 0.5`), with a floor so an almost-black patch is not
/// amplified into noise, and a fixed `1/255` once the patch is bright enough to
/// need no help.
///
/// The floor and the branch are both deliberate and both tested. Note the
/// function is *discontinuous* at 127 — `alpha(126) < alpha(127)` — which is
/// upstream's behaviour, not a mistake here.
pub fn brightness_gain(quantile: u8) -> f32 {
    if quantile < 127 {
        0.45 / (quantile.max(5) as f32)
    } else {
        1.0 / 255.0
    }
}

/// Normalise an 8-bit patch into the model's input tensor.
///
/// This is where the ET5's dimness is dealt with. Raw ET5 pixels span roughly
/// 8..42 of 255; a plain `p/255` would land the entire image in a sliver near
/// -0.47 with almost no dynamic range. The adaptive gain lifts it to about a
/// -0.5..0.0 span, which is what the model was deployed against.
///
/// **Do not replace this with a [`crate::preprocess::Normalize`] variant.**
/// `Unit` and `MeanStd { mean: 0.5, std: 1.0 }` happen to work on a bright
/// frame, but `SignedUnit` is catastrophic on this model: measured on the real
/// face it gives yaw -75 degrees and sigma 0.81 (i.e. garbage), against -8.0 and
/// 0.074 here.
pub fn normalize_into(patch: &[u8; INPUT * INPUT], out: &mut Vec<f32>) {
    let alpha = brightness_gain(intensity_quantile(patch));
    out.clear();
    out.extend(patch.iter().map(|&p| p as f32 * alpha - 0.5));
}

/// A pair of corneal glints — the tracker's own illuminators reflected off the
/// user's eyes, which are by far the brightest thing in an ET5 frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlintPair {
    /// Centroid of the left-hand cluster **in image coordinates**, which is the
    /// user's right eye if the feed is not mirrored. Nothing here depends on
    /// which; the seed only needs the midpoint and the separation.
    pub left: [f32; 2],
    pub right: [f32; 2],
    pub ipd_px: f32,
}

impl GlintPair {
    pub fn mid(&self) -> [f32; 2] {
        [
            0.5 * (self.left[0] + self.right[0]),
            0.5 * (self.left[1] + self.right[1]),
        ]
    }
}

/// Find the two glints by thresholding and splitting at the widest horizontal
/// gap. No connected-component labelling: at this threshold there are single
/// digits of qualifying pixels and they fall into two obvious groups.
///
/// Returns `None` when the frame has no bright pair — which is *not* the same as
/// "no face": a face with closed eyes or heavy glasses still has none. It only
/// seeds the ROI, and [`seed_roi`] falls through to coarser tiers.
pub fn find_glints(frame: &CameraFrame, threshold: u8) -> Option<GlintPair> {
    let (w, h) = (frame.width, frame.height);
    if w == 0 || h == 0 || frame.pixels.len() < (w * h) as usize {
        return None;
    }
    let mut hot: Vec<(u32, u32)> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if frame.pixels[(y * w + x) as usize] >= threshold {
                hot.push((x, y));
            }
        }
    }
    if hot.len() < 4 {
        return None;
    }
    hot.sort_unstable();
    // Split at the widest gap in x. Two eyes are the widest gap in any frame
    // where both are lit; a single blown-out glint has no gap worth the name and
    // is rejected by the size and separation checks below.
    let mut split = 0usize;
    let mut widest = 0u32;
    for i in 1..hot.len() {
        let gap = hot[i].0 - hot[i - 1].0;
        if gap > widest {
            widest = gap;
            split = i;
        }
    }
    let (a, b) = hot.split_at(split);
    if a.len() < 2 || b.len() < 2 {
        return None;
    }
    let centroid = |g: &[(u32, u32)]| -> [f32; 2] {
        let n = g.len() as f32;
        let sx: f32 = g.iter().map(|p| p.0 as f32).sum();
        let sy: f32 = g.iter().map(|p| p.1 as f32).sum();
        // +0.5 converts a pixel index to the continuous coordinate of its centre.
        [sx / n + 0.5, sy / n + 0.5]
    };
    let (left, right) = (centroid(a), centroid(b));
    let ipd_px = ((right[0] - left[0]).powi(2) + (right[1] - left[1]).powi(2)).sqrt();
    if !(GLINT_IPD_MIN..=GLINT_IPD_MAX).contains(&ipd_px) {
        return None;
    }
    Some(GlintPair {
        left,
        right,
        ipd_px,
    })
}

/// Seed an ROI for a frame with no history, in three descending tiers: the
/// glints, the bright-region box, and finally the whole frame.
///
/// **The seed barely matters.** The feedback loop converges to the same
/// attractor from all three (and from a deliberately 2x-too-small and a
/// 40 px-offset start), differing only in how many frames it takes: 1 from the
/// glints, 2-3 from the brightness box, ~7 from the whole frame. Do not
/// over-engineer this; the tiers exist so that a frame with no glints still
/// starts somewhere sensible.
pub fn seed_roi(frame: &CameraFrame) -> Option<Roi> {
    let (w, h) = (frame.width as f32, frame.height as f32);
    if w <= 0.0 || h <= 0.0 || frame.pixels.len() < (frame.width * frame.height) as usize {
        return None;
    }
    if let Some(g) = find_glints(frame, GLINT_THRESHOLD) {
        let mid = g.mid();
        return Some(Roi {
            cx: mid[0],
            cy: mid[1] + SEED_DROP_PER_IPD * g.ipd_px,
            side: SEED_SIDE_PER_IPD * g.ipd_px,
        });
    }
    // Tier 2. `face_bbox` cannot tell a face from an empty room — it is a
    // brightness heuristic — so it is a seed and never a gate on emitting a pose.
    if let Some(b) = preprocess::face_bbox(frame, 1.5) {
        let b = b.squared(frame.width, frame.height);
        if b.width() > 0 && b.height() > 0 {
            return Some(Roi {
                cx: b.x0 as f32 + b.width() as f32 * 0.5,
                cy: b.y0 as f32 + b.height() as f32 * 0.5,
                side: b.width().max(b.height()) as f32,
            });
        }
    }
    Some(Roi::whole(w, h))
}

/// The next frame's ROI: the model's own `box`, squared on its centre.
///
/// No expansion and no smoothing — opentrack's `roi_zoom` and `roi_filter_alpha`
/// both default to 1.0, making the fed-back ROI exactly the box. The centre is
/// clamped to the frame (as opentrack does); the *size* is not, so that a box
/// which has run away is caught by [`Roi::plausible`] and reseeded rather than
/// silently squashed into something plausible-looking.
pub fn next_roi(roi: Roi, bbox: [f32; 4], w: f32, h: f32) -> Roi {
    let half = 0.5 * roi.side;
    let x0 = roi.cx + half * bbox[0];
    let y0 = roi.cy + half * bbox[1];
    let x1 = roi.cx + half * bbox[2];
    let y1 = roi.cy + half * bbox[3];
    Roi {
        cx: (0.5 * (x0 + x1)).clamp(0.0, w),
        cy: (0.5 * (y0 + y1)).clamp(0.0, h),
        side: (x1 - x0).max(y1 - y0),
    }
}

/// Reinterpret the model's image-frame quaternion in the world frame.
///
/// Input is `(x, y, z, w)` — **the real part is last** — in the image frame
/// (x right, y down, z into the image). Output is `(w, x, y, z)` in the world
/// frame (x toward the camera, y up, z image-left).
///
/// The map `(w, x, y, z) -> (w, -z, -y, -x)` is conjugation by a 180 degree
/// rotation about `(1, 0, -1)/sqrt(2)`, and is its own inverse. Apply it
/// **exactly once**.
pub fn image_to_world(q: [f32; 4]) -> [f64; 4] {
    let (qx, qy, qz, qw) = (q[0] as f64, q[1] as f64, q[2] as f64, q[3] as f64);
    [qw, -qz, -qy, -qx]
}

/// Hamilton product, `(w, x, y, z)` throughout.
pub fn qmul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [
        a[0] * b[0] - a[1] * b[1] - a[2] * b[2] - a[3] * b[3],
        a[0] * b[1] + a[1] * b[0] + a[2] * b[3] - a[3] * b[2],
        a[0] * b[2] - a[1] * b[3] + a[2] * b[0] + a[3] * b[1],
        a[0] * b[3] + a[1] * b[2] - a[2] * b[1] + a[3] * b[0],
    ]
}

/// The rotation that carries a pose predicted in the crop's local, nearly
/// orthographic frame into the camera's global frame.
///
/// **This is not a refinement either.** The network sees a crop and predicts the
/// head's rotation relative to the ray through that crop, not relative to the
/// optical axis. Without the correction, the reported pitch becomes a function
/// of *where in the frame the head happens to sit* — measured swing across the
/// frame: about 28 degrees. With it, and with the right focal length, the two
/// agree.
///
/// The head's distance cancels out, so only `f_px` and the head's pixel offset
/// from the image centre are needed.
pub fn perspective_correction(cx_px: f64, cy_px: f64, w_px: f64, h_px: f64, f_px: f64) -> [f64; 4] {
    let py = h_px * 0.5 - cy_px; // world +y is up, image +y is down
    let pz = w_px * 0.5 - cx_px; // world +z is image-left
    let r = (py * py + pz * pz).sqrt();
    // `is_finite` first, then a plain comparison: a NaN here must mean "no
    // correction", never "some correction".
    if !r.is_finite() || r <= 1e-9 || !f_px.is_finite() || f_px <= 0.0 {
        return [1.0, 0.0, 0.0, 0.0];
    }
    let angle = (r / f_px).atan();
    let (s, c) = ((angle * 0.5).sin(), (angle * 0.5).cos());
    [c, 0.0, s * (pz / r), s * (-py / r)]
}

/// Quaternion `(w, x, y, z)` to `(yaw, pitch, roll)` in degrees, in **opentrack's**
/// convention: `+yaw` = the subject turns to their own right, `+pitch` = looking
/// up, `+roll` = tipping toward the subject's *left* shoulder.
///
/// This is opentrack's own chain, with its double negation on pitch cancelled
/// and its `-atan2(-a, b)` on roll folded into `atan2(a, b)`.
///
/// Getting the *pitch sign* here right took an argument: an independent Y-X-Z
/// Euler chain over 2000 random unit quaternions agrees with this one bit for
/// bit on yaw and roll and differs by exactly a sign on pitch, always. This
/// convention wins because the deliverable is an opentrack datagram, so
/// `data[Pitch]` is the contract.
pub fn euler_deg(q: [f64; 4]) -> (f64, f64, f64) {
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if !n.is_finite() || n <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let (w, x, y, z) = (q[0] / n, q[1] / n, q[2] / n, q[3] / n);
    let m00 = 1.0 - 2.0 * (y * y + z * z);
    let m10 = 2.0 * (x * y + z * w);
    let m20 = 2.0 * (x * z - y * w);
    // opentrack names these by column: `my(1)` and `mz(1)` are row 1 of columns
    // 1 and 2. Its roll is `-atan2(-mz1, my1)`, which is `atan2(mz1, my1)`.
    let my1 = 1.0 - 2.0 * (x * x + z * z);
    let mz1 = 2.0 * (y * z - x * w);
    let yaw = m20.atan2(m00);
    let pitch = m10.atan2((m20 * m20 + m00 * m00).sqrt());
    let roll = mz1.atan2(my1);
    (yaw.to_degrees(), pitch.to_degrees(), roll.to_degrees())
}

/// Focal length in pixels, from one simultaneous glint pair and gaze sample.
///
/// The full intrinsic matrix is unavailable, but the perspective correction
/// needs only `f`, and `f` is one division away from data the device already
/// sends: the glints give the interocular separation in *pixels* and the gaze
/// frame's eye origins give it in *millimetres*, at a known distance.
///
/// Returns `None` for a frame where the head is turned enough that the projected
/// separation is foreshortened, which would read as a shorter focal length.
pub fn focal_from_eye_origins(ipd_px: f64, left_mm: [f64; 3], right_mm: [f64; 3]) -> Option<f64> {
    let d = [
        right_mm[0] - left_mm[0],
        right_mm[1] - left_mm[1],
        right_mm[2] - left_mm[2],
    ];
    let ipd_xy = (d[0] * d[0] + d[1] * d[1]).sqrt();
    // A real pair of eyes is 55-75 mm apart; anything under 30 is not a pair.
    // A depth difference over 15% of the lateral separation is a yawed head.
    if !ipd_xy.is_finite() || ipd_xy <= 30.0 || d[2].abs() > 0.15 * ipd_xy {
        return None;
    }
    let z = 0.5 * (left_mm[2] + right_mm[2]);
    if !z.is_finite() || z <= 100.0 || !ipd_px.is_finite() || ipd_px <= 1.0 {
        return None;
    }
    Some(ipd_px * z / ipd_xy)
}

/// The model's five outputs for one frame, extracted from tract's tensors.
///
/// Public so that the state machine around inference can be driven in tests, and
/// so a diagnostic can print raw model output without re-deriving the layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawOutputs {
    /// `(px, py, ps)` in patch-normalised units: the head centre as an offset
    /// from the ROI centre in units of the ROI half-side, and half the head's
    /// vertical extent in the same units. `+py` is **down**.
    pub pos_size: [f32; 3],
    /// `(x, y, z, w)` — real part last.
    pub quat: [f32; 4],
    /// `(x0, y0, x1, y1)`, corner form, same patch-normalised units.
    pub bbox: [f32; 4],
    /// Predicted standard deviations for `pos_size`. Telemetry only.
    pub pos_size_scales: [f32; 3],
    /// `rotaxis_scales_tril[0][0]`, the predicted rotation sigma.
    ///
    /// The full 3x3 is always exactly `sigma * I` — by construction, not by
    /// luck: the training head predicts one scalar, broadcasts it to three, and
    /// writes literal zeros off the diagonal. Reading `[0][0]` is exact and
    /// complete for this model.
    pub sigma: f32,
}

/// One frame's worth of model output, in frame pixels and degrees.
///
/// Richer than [`HeadPose`] because `HeadPose` has no room for a confidence and
/// no per-field validity, and both are needed to fuse this with the geometric
/// pose sensibly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPose {
    /// World-frame rotation `(w, x, y, z)` after [`image_to_world`] and the
    /// perspective correction. The single source of truth here; the Euler angles
    /// are derived from it.
    pub quat_world: [f64; 4],
    /// Degrees, in **this crate's** convention (see [`Signs`]), not opentrack's.
    pub yaw_deg: f64,
    pub pitch_deg: f64,
    pub roll_deg: f64,
    /// Position from the model alone: a 200 mm reference head and a focal
    /// length. A fallback — prefer the eye origins, which are measured. See
    /// [`fuse`].
    pub x_mm: f64,
    pub y_mm: f64,
    pub z_mm: f64,
    /// Head centre in frame pixels, and its vertical extent.
    pub centre_px: [f32; 2],
    pub head_height_px: f32,
    /// The gate value. Lower is better; see [`SIGMA_MAX`].
    pub sigma: f32,
    /// `mean(pos_size_scales)`. **Telemetry, never a gate** — it separates face
    /// from garbage cleanly on the one subject it was measured on, and adding a
    /// second single-subject threshold is how a tracker mysteriously stops
    /// working for the next person.
    pub pos_sigma: f32,
    /// The ROI this pose was computed from, and the one the next frame will use.
    pub roi: Roi,
    pub next_roi: Roi,
}

#[derive(Debug)]
pub enum OnnxError {
    /// No model on disk. Not a failure: the caller should fall back to the
    /// 5-DOF geometric path and point the user at `tobii headpose --fetch-model`.
    ModelMissing(PathBuf),
    ModelCorrupt {
        path: PathBuf,
        expected: &'static str,
        found: String,
    },
    WrongKind(ModelKind),
    /// Parse, optimise or plan failure, carrying tract's own message.
    Graph(String),
    OutputMissing {
        want: &'static str,
        have: Vec<String>,
    },
    OutputShape {
        name: &'static str,
        want: &'static [usize],
        got: Vec<usize>,
    },
    InputShape {
        want: [usize; 4],
        got: Vec<usize>,
    },
}

impl std::fmt::Display for OnnxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OnnxError::ModelMissing(p) => write!(
                f,
                "no head-pose model at {} — run `tobii headpose --fetch-model`",
                p.display()
            ),
            OnnxError::ModelCorrupt {
                path,
                expected,
                found,
            } => write!(
                f,
                "the model at {} is not the expected one (sha256 {found}, expected {expected}); \
                 refusing to run it",
                path.display()
            ),
            OnnxError::WrongKind(k) => write!(f, "this backend runs ONNX models, not {k:?}"),
            OnnxError::Graph(m) => write!(f, "could not load the model graph: {m}"),
            OnnxError::OutputMissing { want, have } => write!(
                f,
                "the model has no output named {want}; it has {}",
                have.join(", ")
            ),
            OnnxError::OutputShape { name, want, got } => {
                write!(f, "the model's {name} output is {got:?}, expected {want:?}")
            }
            OnnxError::InputShape { want, got } => {
                write!(f, "the model's input is {got:?}, expected {want:?}")
            }
        }
    }
}

impl std::error::Error for OnnxError {}

/// Output names, in the order [`OnnxPose::slots`] indexes them.
const OUTPUT_NAMES: [&str; 5] = [
    "pos_size",
    "quat",
    "box",
    "pos_size_scales",
    "rotaxis_scales_tril",
];
const OUTPUT_SHAPES: [&[usize]; 5] = [&[1, 3], &[1, 4], &[1, 4], &[1, 3], &[1, 3, 3]];
const SLOT_POS_SIZE: usize = 0;
const SLOT_QUAT: usize = 1;
const SLOT_BOX: usize = 2;
const SLOT_POS_SCALES: usize = 3;
const SLOT_ROT_SCALES: usize = 4;

/// Everything about running the model that is *not* the model: the ROI the
/// box-feedback loop carries between frames, the confidence gate, and the
/// conventions that turn raw outputs into a pose.
///
/// Separate from [`OnnxPose`] because this is where the subtle failure modes
/// live — the ROI must advance on rejected frames, a run of rejections must
/// reseed, a runaway box must not be followed — and all of that can then be
/// tested with injected outputs and no 13 MB model file.
#[derive(Debug, Clone, PartialEq)]
pub struct Tracker {
    roi: Option<Roi>,
    misses: u32,
    focal_px: f32,
    perspective: bool,
    sigma_max: f32,
    signs: Signs,
}

impl Default for Tracker {
    fn default() -> Self {
        Self {
            roi: None,
            misses: 0,
            focal_px: DEFAULT_FOCAL_PX,
            perspective: true,
            sigma_max: SIGMA_MAX,
            signs: Signs::default(),
        }
    }
}

impl Tracker {
    pub fn set_focal_px(&mut self, f: f32) {
        self.focal_px = f;
    }
    pub fn focal_px(&self) -> f32 {
        self.focal_px
    }
    pub fn set_perspective(&mut self, on: bool) {
        self.perspective = on;
    }
    pub fn set_sigma_max(&mut self, s: f32) {
        self.sigma_max = s;
    }
    pub fn signs(&self) -> Signs {
        self.signs
    }
    pub fn set_signs(&mut self, s: Signs) {
        self.signs = s;
    }
    /// The ROI the next frame will be cropped from, if tracking is established.
    pub fn roi(&self) -> Option<Roi> {
        self.roi
    }
    /// Consecutive rejected frames since the last accepted pose.
    pub fn misses(&self) -> u32 {
        self.misses
    }
    /// Forget the ROI, forcing a reseed on the next frame.
    pub fn reset(&mut self) {
        self.roi = None;
        self.misses = 0;
    }
    /// Force the ROI the next frame will be cropped from. For a diagnostic that
    /// wants to watch the loop converge from a chosen start; ordinary use should
    /// let [`seed_roi`] pick.
    pub fn set_roi(&mut self, roi: Option<Roi>) {
        self.roi = roi;
    }

    /// The ROI to crop this frame from: the one carried over, or a fresh seed.
    pub fn roi_for(&self, frame: &CameraFrame) -> Option<Roi> {
        let frame_side = frame.width.max(frame.height) as f32;
        match self.roi {
            Some(r) if r.plausible(frame_side) => Some(r),
            _ => seed_roi(frame),
        }
    }

    /// Count a frame that produced no usable output at all (an inference error),
    /// so a wedged model still trips the reseed rather than freezing the ROI.
    pub fn miss(&mut self) {
        self.misses = self.misses.saturating_add(1);
        if self.misses >= SIGMA_RESEED_FRAMES {
            self.roi = None;
            self.misses = 0;
        }
    }

    /// Everything after inference: advance the ROI, apply the confidence gate,
    /// and turn the raw outputs into a pose.
    pub fn finish(
        &mut self,
        frame_w: f32,
        frame_h: f32,
        roi: Roi,
        raw: RawOutputs,
    ) -> Option<ModelPose> {
        let frame_side = frame_w.max(frame_h);
        let next = next_roi(roi, raw.bbox, frame_w, frame_h);

        // Unconditionally, and before the gate. See the module docs: gating this
        // on sigma makes re-acquisition impossible.
        self.roi = next.plausible(frame_side).then_some(next);

        if !raw.sigma.is_finite() || raw.sigma >= self.sigma_max || self.roi.is_none() {
            self.miss();
            return None;
        }
        self.misses = 0;

        let (cx_px, cy_px) = roi.to_frame(raw.pos_size[0], raw.pos_size[1]);
        let head_height_px = roi.side * raw.pos_size[2];

        let world = image_to_world(raw.quat);
        let quat_world = if self.perspective {
            qmul(
                perspective_correction(
                    cx_px as f64,
                    cy_px as f64,
                    frame_w as f64,
                    frame_h as f64,
                    self.focal_px as f64,
                ),
                world,
            )
        } else {
            world
        };
        let (yaw, pitch, roll) = euler_deg(quat_world);

        let f = self.focal_px as f64;
        let (x_mm, y_mm, z_mm) = if head_height_px > 1.0 && f > 0.0 {
            let d = f * HEAD_SIZE_MM / head_height_px as f64;
            (
                self.signs.x * (cx_px as f64 - frame_w as f64 * 0.5) * d / f,
                (frame_h as f64 * 0.5 - cy_px as f64) * d / f,
                d,
            )
        } else {
            (0.0, 0.0, 0.0)
        };

        Some(ModelPose {
            quat_world,
            yaw_deg: self.signs.yaw * yaw,
            pitch_deg: self.signs.pitch * pitch + self.signs.pitch_offset_deg,
            roll_deg: self.signs.roll * roll,
            x_mm,
            y_mm,
            z_mm,
            centre_px: [cx_px, cy_px],
            head_height_px,
            sigma: raw.sigma,
            pos_sigma: (raw.pos_size_scales[0] + raw.pos_size_scales[1] + raw.pos_size_scales[2])
                / 3.0,
            roi,
            next_roi: next,
        })
    }
}

/// The loaded model, plus its [`Tracker`].
pub struct OnnxPose {
    plan: Arc<TypedRunnableModel>,
    /// Which output slot each of [`OUTPUT_NAMES`] resolved to, fixed at load.
    /// Never positional: a model update that reorders its outputs would
    /// otherwise swap the quaternion for the position, silently and plausibly.
    slots: [usize; 5],
    patch: Box<[u8; INPUT * INPUT]>,
    tensor: Vec<f32>,
    tracker: Tracker,
}

impl std::fmt::Debug for OnnxPose {
    /// The plan holds 13 MB of weights, so this reports the tracking state and
    /// says nothing at all about the graph.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxPose")
            .field("slots", &self.slots)
            .field("tracker", &self.tracker)
            .finish_non_exhaustive()
    }
}

impl OnnxPose {
    /// Load the model [`model_store`] manages — the normal entry point.
    ///
    /// The bytes are read once and then hashed *and* parsed, so there is no
    /// window in which the file could change between the check and the load.
    pub fn from_store() -> Result<Self, OnnxError> {
        let src = &model_store::HEAD_POSE;
        let path = model_store::path_of(src);
        let bytes = std::fs::read(&path).map_err(|_| OnnxError::ModelMissing(path.clone()))?;
        let found = sha256::hex_digest(&bytes);
        if found != src.sha256 {
            return Err(OnnxError::ModelCorrupt {
                path,
                expected: src.sha256,
                found,
            });
        }
        Self::load_bytes(&bytes)
    }

    /// Load a caller-supplied model file. The digest is **not** checked — the
    /// output name and shape validation is the only guard, so this is for a
    /// deliberate "run this exact file" and not for the normal path.
    pub fn load(cfg: &ModelConfig) -> Result<Self, OnnxError> {
        if cfg.kind != ModelKind::OpentrackOnnx {
            return Err(OnnxError::WrongKind(cfg.kind));
        }
        let bytes = std::fs::read(&cfg.model_path)
            .map_err(|_| OnnxError::ModelMissing(cfg.model_path.clone()))?;
        Self::load_bytes(&bytes)
    }

    pub fn load_bytes(bytes: &[u8]) -> Result<Self, OnnxError> {
        let raw = tract_onnx::onnx()
            .model_for_read(&mut Cursor::new(bytes))
            .map_err(|e| OnnxError::Graph(e.to_string()))?;

        // Optimising costs ~50 ms once and buys 2.35x per frame (12.9 ms against
        // 30.3 ms). Unoptimised lands exactly on the camera's 33 Hz with no
        // headroom at all, so this is not optional. Numerically it is a no-op:
        // max |optimised - unoptimised| over every output was 1.5e-7.
        let opt: TypedModel = raw
            .into_optimized()
            .map_err(|e| OnnxError::Graph(e.to_string()))?;

        let ins = opt
            .input_outlets()
            .map_err(|e| OnnxError::Graph(e.to_string()))?;
        let in_shape: Vec<usize> = ins
            .first()
            .and_then(|o| opt.outlet_fact(*o).ok())
            .and_then(|f| f.shape.as_concrete().map(|s| s.to_vec()))
            .unwrap_or_default();
        if in_shape != [1, 1, INPUT, INPUT] {
            return Err(OnnxError::InputShape {
                want: [1, 1, INPUT, INPUT],
                got: in_shape,
            });
        }

        // Resolve outputs by NAME. Labels survive `into_optimized`; `OutletId`s
        // do not, so nothing may be cached across that step.
        let outs = opt
            .output_outlets()
            .map_err(|e| OnnxError::Graph(e.to_string()))?
            .to_vec();
        let have: Vec<String> = outs
            .iter()
            .map(|o| opt.outlet_label(*o).unwrap_or("<unnamed>").to_string())
            .collect();
        let mut slots = [0usize; 5];
        for (i, want) in OUTPUT_NAMES.iter().enumerate() {
            let idx =
                have.iter()
                    .position(|n| n == want)
                    .ok_or_else(|| OnnxError::OutputMissing {
                        want,
                        have: have.clone(),
                    })?;
            let got: Vec<usize> = opt
                .outlet_fact(outs[idx])
                .ok()
                .and_then(|f| f.shape.as_concrete().map(|s| s.to_vec()))
                .unwrap_or_default();
            if got != OUTPUT_SHAPES[i] {
                return Err(OnnxError::OutputShape {
                    name: want,
                    want: OUTPUT_SHAPES[i],
                    got,
                });
            }
            slots[i] = idx;
        }

        let plan: Arc<TypedRunnableModel> = opt
            .into_runnable()
            .map_err(|e| OnnxError::Graph(e.to_string()))?;

        Ok(OnnxPose {
            plan,
            slots,
            patch: Box::new([0u8; INPUT * INPUT]),
            tensor: Vec::with_capacity(INPUT * INPUT),
            tracker: Tracker::default(),
        })
    }

    /// The tracking state and conventions. Set the focal length, the signs and
    /// the confidence threshold through this.
    pub fn tracker(&mut self) -> &mut Tracker {
        &mut self.tracker
    }
    pub fn roi(&self) -> Option<Roi> {
        self.tracker.roi()
    }
    pub fn reset(&mut self) {
        self.tracker.reset();
    }

    /// Run one frame and return the full result.
    ///
    /// `None` means no pose could be trusted for this frame — hold the previous
    /// one rather than snapping to zero. The ROI still advances (see the module
    /// docs), so a rejected frame is progress toward re-acquisition.
    pub fn estimate_detailed(&mut self, frame: &CameraFrame) -> Option<ModelPose> {
        let (w, h) = (frame.width, frame.height);
        if w == 0 || h == 0 || frame.pixels.len() < (w * h) as usize {
            return None;
        }
        let roi = self.tracker.roi_for(frame)?;
        sample_patch(frame, roi, &mut self.patch);
        normalize_into(&self.patch, &mut self.tensor);
        let raw = match self.run() {
            Ok(r) => r,
            Err(_) => {
                // An inference failure is a bug, not a lost face, but it must not
                // be able to wedge tracking: treat it as a miss so the reseed
                // counter still runs.
                self.tracker.miss();
                return None;
            }
        };
        self.tracker.finish(w as f32, h as f32, roi, raw)
    }

    fn run(&self) -> Result<RawOutputs, OnnxError> {
        let input = Tensor::from_shape(&[1, 1, INPUT, INPUT], &self.tensor)
            .map_err(|e| OnnxError::Graph(e.to_string()))?;
        let out = self
            .plan
            .run(tvec!(input.into()))
            .map_err(|e| OnnxError::Graph(e.to_string()))?;
        let get = |slot: usize| -> Result<Vec<f32>, OnnxError> {
            let view = out[self.slots[slot]]
                .to_plain_array_view::<f32>()
                .map_err(|e| OnnxError::Graph(e.to_string()))?;
            Ok(view.iter().copied().collect())
        };
        let pos_size = get(SLOT_POS_SIZE)?;
        let quat = get(SLOT_QUAT)?;
        let bbox = get(SLOT_BOX)?;
        let scales = get(SLOT_POS_SCALES)?;
        let rot = get(SLOT_ROT_SCALES)?;
        Ok(RawOutputs {
            pos_size: [pos_size[0], pos_size[1], pos_size[2]],
            quat: [quat[0], quat[1], quat[2], quat[3]],
            bbox: [bbox[0], bbox[1], bbox[2], bbox[3]],
            pos_size_scales: [scales[0], scales[1], scales[2]],
            sigma: rot[0],
        })
    }
}

impl PoseModel for OnnxPose {
    fn estimate(&mut self, frame: &CameraFrame) -> Option<HeadPose> {
        self.estimate_detailed(frame).map(|m| HeadPose {
            x_mm: m.x_mm,
            y_mm: m.y_mm,
            z_mm: m.z_mm,
            yaw_deg: m.yaw_deg,
            pitch_deg: m.pitch_deg,
            roll_deg: m.roll_deg,
        })
    }

    fn kind(&self) -> ModelKind {
        ModelKind::OpentrackOnnx
    }
}

/// Where the rotation in a fused pose comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RotationSource {
    /// All three angles from the model. The default, because it is the only way
    /// to get a self-consistent rotation: composing yaw from one source's zero
    /// with pitch from another's produces a rotation whose axes disagree.
    #[default]
    Model,
    /// Yaw and roll from the eye origins, pitch from the model. Useful if the
    /// model's yaw turns out to be worse than the geometric one — which is an
    /// open question, not a settled fact.
    Geometric,
}

/// Combine the geometric pose from the eye origins with the model's.
///
/// **Position comes from the eyes whenever they are tracked.** They are a
/// hardware measurement in real millimetres; the model's position needs a focal
/// length and assumes a 200 mm head. Having a metric depth to hand — rather than
/// the fixed head size a webcam tracker must assume — is the one advantage this
/// hardware has over a webcam, and throwing it away for a neural estimate would
/// be perverse.
///
/// **Rotation comes from the model whenever it is confident**, because pitch has
/// no geometric source at all.
///
/// Returns `None` only when neither source has anything.
pub fn fuse(
    eyes: Option<HeadPose>,
    model: Option<&ModelPose>,
    src: RotationSource,
) -> Option<HeadPose> {
    match (eyes, model) {
        (Some(e), Some(m)) => Some(HeadPose {
            x_mm: e.x_mm,
            y_mm: e.y_mm,
            z_mm: e.z_mm,
            yaw_deg: match src {
                RotationSource::Model => m.yaw_deg,
                RotationSource::Geometric => e.yaw_deg,
            },
            pitch_deg: m.pitch_deg,
            roll_deg: match src {
                RotationSource::Model => m.roll_deg,
                RotationSource::Geometric => e.roll_deg,
            },
        }),
        // Eyes only: exactly what the crate shipped before this backend existed.
        // Pitch stays 0.0 rather than being invented; the caller decides whether
        // to hold the last model pitch instead.
        (Some(e), None) => Some(e),
        (None, Some(m)) => Some(HeadPose {
            x_mm: m.x_mm,
            y_mm: m.y_mm,
            z_mm: m.z_mm,
            yaw_deg: m.yaw_deg,
            pitch_deg: m.pitch_deg,
            roll_deg: m.roll_deg,
        }),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    // The fixtures below are values recorded from a reference run, quoted to the
    // digits the reference printed. Truncating them to f32's actual precision
    // would make them harder to match against the record they came from, for no
    // change in what the compiler produces.
    #![allow(clippy::excessive_precision)]

    use super::*;

    /// The real 280x280 ET5 NIR frame, captured from the device with a face at
    /// ~70 cm. Raw and un-stretched: 6..255, with the two corneal glints as the
    /// only pixels above 180.
    const FACE280: &[u8] = include_bytes!("../testdata/face280.pgm");

    /// The reference 129x129 input tensor for that face, as 16641 little-endian
    /// f32 in row-major order. Produced by an independent (Python/OpenCV)
    /// pipeline, so it tests the *model* rather than this file's sampler.
    const FACE_CROP: &[u8] = include_bytes!("../testdata/face_crop_129.f32");

    fn face_frame() -> CameraFrame {
        // P5\n280 280\n255\n<pixels>
        let header_end = FACE280
            .windows(4)
            .position(|w| w == b"255\n")
            .expect("pgm maxval header")
            + 4;
        CameraFrame {
            timestamp_us: 0,
            width: 280,
            height: 280,
            bit_depth: 8,
            pixels: FACE280[header_end..].to_vec(),
        }
    }

    fn face_crop_tensor() -> Vec<f32> {
        FACE_CROP
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    fn flat(w: u32, h: u32, v: u8) -> CameraFrame {
        CameraFrame {
            timestamp_us: 0,
            width: w,
            height: h,
            bit_depth: 8,
            pixels: vec![v; (w * h) as usize],
        }
    }

    /// A frame whose pixel value is a plain function of position, so a sampler
    /// can be checked against arithmetic rather than against an image.
    fn ramp(w: u32, h: u32) -> CameraFrame {
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.push(((x + y) % 256) as u8);
            }
        }
        CameraFrame {
            timestamp_us: 0,
            width: w,
            height: h,
            bit_depth: 8,
            pixels,
        }
    }

    fn patch_of(frame: &CameraFrame, roi: Roi) -> Box<[u8; INPUT * INPUT]> {
        let mut p = Box::new([0u8; INPUT * INPUT]);
        sample_patch(frame, roi, &mut p);
        p
    }

    #[test]
    fn the_intensity_quantile_is_the_first_level_that_strictly_exceeds_ninety_percent() {
        let mut p = [10u8; INPUT * INPUT];
        for slot in p.iter_mut().take(INPUT * INPUT / 2) {
            *slot = 200;
        }
        // 8320 pixels at 10 does not exceed 14976; adding the 200s does.
        assert_eq!(intensity_quantile(&p), 200);
        assert_eq!(intensity_quantile(&[0u8; INPUT * INPUT]), 0);
        assert_eq!(intensity_quantile(&[7u8; INPUT * INPUT]), 7);
    }

    #[test]
    fn the_brightness_gain_floors_at_five_and_switches_at_one_two_seven() {
        assert_eq!(
            brightness_gain(0),
            brightness_gain(5),
            "the floor must bind"
        );
        assert!((brightness_gain(5) - 0.09).abs() < 1e-6);
        assert!((brightness_gain(126) - 0.45 / 126.0).abs() < 1e-9);
        assert!((brightness_gain(127) - 1.0 / 255.0).abs() < 1e-9);
        // Upstream's branch is discontinuous. Pinned so nobody "fixes" it.
        assert!(
            brightness_gain(126) < brightness_gain(127),
            "the gain jumps up at the 127 boundary: {} then {}",
            brightness_gain(126),
            brightness_gain(127)
        );
    }

    /// opentrack's own claim for this formula, and the tripwire for anyone
    /// rewriting it to use the full range: a dark patch is boosted to *half*
    /// scale, landing in -0.5..0.0, not stretched across -0.5..+0.5.
    #[test]
    fn a_dark_patch_is_boosted_to_half_max_not_to_full_range() {
        let mut p = [0u8; INPUT * INPUT];
        for (i, slot) in p.iter_mut().enumerate() {
            *slot = (i % 31) as u8; // 90th percentile lands near 28
        }
        let mut out = Vec::new();
        normalize_into(&p, &mut out);
        let min = out.iter().copied().fold(f32::INFINITY, f32::min);
        let max = out.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (min + 0.5).abs() < 0.05,
            "min should sit at -0.5, got {min}"
        );
        assert!(
            max > -0.10 && max < 0.10,
            "max should sit near 0.0, got {max}"
        );
    }

    /// The ET5 case specifically: pixels spanning 8..42 of 255 must come out of
    /// normalisation using most of the -0.5..0.0 span, not squashed into a
    /// sliver. A tensor whose max is still near -0.5 is the signature of the
    /// gain having been bypassed.
    #[test]
    fn a_real_et5_patch_is_not_left_squashed_against_the_floor() {
        let frame = face_frame();
        let roi = seed_roi(&frame).expect("the test frame has glints");
        let patch = patch_of(&frame, roi);
        let mut out = Vec::new();
        normalize_into(&patch, &mut out);
        let max = out.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            max > -0.1,
            "the adaptive gain must lift the patch off the floor; max was {max}"
        );
    }

    #[test]
    fn the_sampler_replicates_the_border_instead_of_padding_black() {
        // A ROI hanging off the top-left corner: three quarters of it is outside.
        let frame = flat(40, 40, 17);
        let patch = patch_of(
            &frame,
            Roi {
                cx: 0.0,
                cy: 0.0,
                side: 20.0,
            },
        );
        assert!(
            patch.iter().all(|&p| p == 17),
            "replicating a flat frame must give a flat patch; found {:?}",
            patch.iter().find(|&&p| p != 17)
        );
    }

    #[test]
    fn a_whole_frame_roi_reproduces_the_frame_corners() {
        let frame = ramp(280, 280);
        let patch = patch_of(&frame, Roi::whole(280.0, 280.0));
        let corner = |x: u32, y: u32| frame.pixels[(y * 280 + x) as usize] as i32;
        assert!((patch[0] as i32 - corner(0, 0)).abs() <= 2, "top-left");
        assert!(
            (patch[INPUT * INPUT - 1] as i32 - corner(279, 279)).abs() <= 2,
            "bottom-right"
        );
    }

    /// The claim under test is that a quarter-pixel ROI move is *visible*.
    /// Nearest-neighbour would give four identical patches and then a jump —
    /// which is where the 4.06-degree pitch jitter in the module docs comes
    /// from, against 0.30 for this sampler.
    #[test]
    fn a_quarter_pixel_roi_move_changes_the_patch() {
        let frame = ramp(280, 280);
        let mut prev = patch_of(
            &frame,
            Roi {
                cx: 140.0,
                cy: 140.0,
                side: 90.0,
            },
        );
        for step in 1..=4 {
            let roi = Roi {
                cx: 140.0 + 0.25 * step as f32,
                cy: 140.0,
                side: 90.0,
            };
            let now = patch_of(&frame, roi);
            let diff: u32 = now
                .iter()
                .zip(prev.iter())
                .map(|(a, b)| a.abs_diff(*b) as u32)
                .sum();
            assert!(
                diff > 0,
                "step {step} of 0.25 px produced an identical patch"
            );
            prev = now;
        }
    }

    #[test]
    fn patch_normalised_coordinates_round_trip_through_the_roi() {
        let roi = Roi {
            cx: 100.0,
            cy: 150.0,
            side: 80.0,
        };
        assert_eq!(roi.to_frame(0.0, 0.0), (100.0, 150.0));
        assert_eq!(roi.to_frame(-1.0, -1.0), (60.0, 110.0));
        assert_eq!(roi.to_frame(1.0, 1.0), (140.0, 190.0));
        // A box covering the whole patch must feed back the same ROI.
        let same = next_roi(roi, [-1.0, -1.0, 1.0, 1.0], 280.0, 280.0);
        assert_eq!(same, roi);
    }

    /// The anchor test for the whole rotation chain: a quaternion measured off
    /// the real ET5 face, through image->world and Euler, with no perspective
    /// correction. These numbers are reproduced independently by opentrack's own
    /// arithmetic.
    #[test]
    fn euler_from_the_real_face_quaternion_matches_opentrack() {
        let q = [-0.0944984034, -0.0759362578, -0.0319216624, 0.992111266];
        let (yaw, pitch, roll) = euler_deg(image_to_world(q));
        assert!((yaw - -8.475_925_027).abs() < 1e-6, "yaw {yaw}");
        assert!((pitch - 11.090_185_951).abs() < 1e-6, "pitch {pitch}");
        assert!((roll - -2.861_406_006).abs() < 1e-6, "roll {roll}");
    }

    #[test]
    fn the_identity_quaternion_faces_the_camera_upright() {
        let (yaw, pitch, roll) = euler_deg(image_to_world([0.0, 0.0, 0.0, 1.0]));
        assert_eq!((yaw, pitch, roll), (0.0, 0.0, 0.0));
    }

    /// Horizontal mirroring of the input negates exactly `quat.y` and `quat.z` —
    /// the experiment that identified the `(x, y, z, w)` component order. It must
    /// come out the other end as negated yaw and roll with pitch untouched;
    /// anything else means the image->world map is wrong.
    #[test]
    fn mirroring_the_image_quaternion_negates_yaw_and_roll_but_not_pitch() {
        let q = [-0.0944984034, -0.0759362578, -0.0319216624, 0.992111266];
        let (y0, p0, r0) = euler_deg(image_to_world(q));
        let (y1, p1, r1) = euler_deg(image_to_world([q[0], -q[1], -q[2], q[3]]));
        assert!((y1 + y0).abs() < 1e-9, "yaw {y0} then {y1}");
        assert!((r1 + r0).abs() < 1e-9, "roll {r0} then {r1}");
        assert!((p1 - p0).abs() < 1e-9, "pitch {p0} then {p1}");
    }

    /// Reading the real part first is the single most inviting mistake here, and
    /// it produces a *plausible-looking* pose rather than an obvious failure —
    /// so the failure is pinned by a test rather than by a comment.
    #[test]
    fn reading_the_quaternion_real_part_first_produces_an_upside_down_head() {
        let q = [-0.0944984034, -0.0759362578, -0.0319216624, 0.992111266];
        // The mistake: treat q as (w, x, y, z) and skip image->world.
        let (_, _, roll) = euler_deg([q[0], q[1], q[2], q[3]]);
        assert!(
            roll.abs() > 150.0,
            "the w-first misreading should look upside down; roll was {roll}"
        );
    }

    #[test]
    fn the_perspective_correction_is_identity_at_the_image_centre() {
        let q = perspective_correction(140.0, 140.0, 280.0, 280.0, 355.0);
        assert_eq!(q, [1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn a_zero_or_non_finite_focal_length_disables_the_correction() {
        for f in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                perspective_correction(100.0, 200.0, 280.0, 280.0, f),
                [1.0, 0.0, 0.0, 0.0],
                "focal {f} must fall back to no correction"
            );
        }
    }

    /// The correction's whole purpose: it must depend on where in the frame the
    /// head sits, flip sign across the centre, and be monotone in between.
    /// Without it, reported pitch swings ~28 degrees across the frame.
    #[test]
    fn the_perspective_correction_adds_pitch_below_centre_and_removes_it_above() {
        let world = image_to_world([-0.0944984034, -0.0759362578, -0.0319216624, 0.992111266]);
        let pitch_at = |cy: f64| {
            let c = perspective_correction(140.0, cy, 280.0, 280.0, 355.0);
            euler_deg(qmul(c, world)).1
        };
        let base = euler_deg(world).1;
        let low = pitch_at(217.6) - base;
        let high = pitch_at(60.0) - base;
        assert!(
            low > 8.0 && low < 16.0,
            "below centre should add pitch: {low}"
        );
        assert!(
            high < -8.0 && high > -16.0,
            "above centre should remove pitch: {high}"
        );
        let mut prev = f64::NEG_INFINITY;
        for cy in [40.0, 80.0, 120.0, 160.0, 200.0, 240.0] {
            let v = pitch_at(cy);
            assert!(v > prev, "pitch must be monotone in cy, broke at {cy}");
            prev = v;
        }
    }

    #[test]
    fn the_glint_seed_finds_both_pupil_reflections() {
        let frame = face_frame();
        let g = find_glints(&frame, GLINT_THRESHOLD).expect("the test frame has two glints");
        assert!((g.left[0] - 121.0).abs() < 1.0, "left x {:?}", g.left);
        assert!((g.right[0] - 154.0).abs() < 1.0, "right x {:?}", g.right);
        assert!((g.ipd_px - 33.0).abs() < 1.0, "ipd {}", g.ipd_px);
        // Only four pixels reach 255 and they all fall in one glint, so the pair
        // splitter cannot work at that threshold. Pinned so it is not "tidied".
        assert!(
            find_glints(&frame, 255).is_none(),
            "thresholding at 255 must not yield a pair"
        );
        let roi = seed_roi(&frame).expect("seed");
        assert!((roi.side - 94.4).abs() < 3.0, "seed side {}", roi.side);
        assert!(
            (roi.cy - g.mid()[1] - 16.5).abs() < 3.0,
            "the seed sits below the glints, at {}",
            roi.cy
        );
    }

    #[test]
    fn a_frame_with_no_bright_pixels_falls_through_to_a_coarser_seed() {
        let frame = flat(280, 280, 20);
        assert!(find_glints(&frame, GLINT_THRESHOLD).is_none());
        // A uniform frame defeats `face_bbox` too, so the whole frame is the seed.
        assert_eq!(seed_roi(&frame), Some(Roi::whole(280.0, 280.0)));
    }

    #[test]
    fn a_degenerate_frame_yields_no_seed_and_no_panic() {
        assert!(seed_roi(&flat(0, 10, 5)).is_none());
        assert!(seed_roi(&flat(10, 0, 5)).is_none());
        let mut truncated = flat(10, 10, 5);
        truncated.pixels.truncate(30);
        assert!(seed_roi(&truncated).is_none());
        assert!(find_glints(&truncated, 1).is_none());
    }

    #[test]
    fn focal_length_comes_out_of_a_glint_pair_and_two_eye_origins() {
        // 33 px of glint separation, eyes 63 mm apart at 680 mm.
        let f = focal_from_eye_origins(33.0, [-31.5, 0.0, 680.0], [31.5, 0.0, 680.0]).unwrap();
        assert!((f - 33.0 * 680.0 / 63.0).abs() < 1e-9, "{f}");
        assert!((f - 356.2).abs() < 1.0, "should land near the default: {f}");
        // A yawed head foreshortens the separation and must be rejected.
        assert!(focal_from_eye_origins(33.0, [-31.5, 0.0, 640.0], [31.5, 0.0, 700.0]).is_none());
        // So must nonsense geometry.
        assert!(focal_from_eye_origins(33.0, [-5.0, 0.0, 680.0], [5.0, 0.0, 680.0]).is_none());
        assert!(focal_from_eye_origins(33.0, [-31.5, 0.0, 1.0], [31.5, 0.0, 1.0]).is_none());
    }

    #[test]
    fn a_runaway_box_is_rejected_rather_than_followed() {
        let frame_side = 280.0;
        assert!(!Roi {
            cx: 140.0,
            cy: 140.0,
            side: 8.0
        }
        .plausible(frame_side));
        assert!(!Roi {
            cx: 140.0,
            cy: 140.0,
            side: 500.0
        }
        .plausible(frame_side));
        assert!(!Roi {
            cx: f32::NAN,
            cy: 140.0,
            side: 90.0
        }
        .plausible(frame_side));
        assert!(Roi {
            cx: 140.0,
            cy: 140.0,
            side: 90.0
        }
        .plausible(frame_side));
    }

    fn raw(sigma: f32, bbox: [f32; 4]) -> RawOutputs {
        RawOutputs {
            pos_size: [0.0, 0.0, 1.0],
            quat: [0.0, 0.0, 0.0, 1.0],
            bbox,
            pos_size_scales: [0.02, 0.03, 0.03],
            sigma,
        }
    }

    /// The single most dangerous thing to "optimise" in this file. From a
    /// whole-frame start the loop passes through sigma 0.50, 0.94, 0.80, 0.88,
    /// 0.75, 0.51, 0.20 before locking on at 0.08 — so a gate that suppressed
    /// the ROI update on rejected frames would leave the ROI frozen where it was
    /// and re-acquisition could never happen.
    #[test]
    fn the_roi_advances_even_when_the_confidence_gate_rejects() {
        let mut tr = Tracker::default();
        let roi = Roi {
            cx: 140.0,
            cy: 140.0,
            side: 100.0,
        };
        // A box shifted right and slightly smaller, with hopeless confidence.
        let got = tr.finish(280.0, 280.0, roi, raw(0.8, [-0.6, -0.8, 1.0, 0.8]));
        assert!(got.is_none(), "sigma 0.8 must not produce a pose");
        let moved = tr.roi().expect("the ROI must still have advanced");
        assert!(moved != roi, "the ROI did not move: {moved:?}");
        assert!(moved.cx > roi.cx, "it should have followed the box right");
    }

    #[test]
    fn a_run_of_rejections_reseeds_the_roi() {
        let mut tr = Tracker::default();
        let roi = Roi {
            cx: 140.0,
            cy: 140.0,
            side: 100.0,
        };
        for i in 0..SIGMA_RESEED_FRAMES {
            assert!(tr
                .finish(280.0, 280.0, roi, raw(0.9, [-1.0, -1.0, 1.0, 1.0]))
                .is_none());
            if i + 1 < SIGMA_RESEED_FRAMES {
                assert!(tr.roi().is_some(), "gave up after only {} frames", i + 1);
            }
        }
        assert!(tr.roi().is_none(), "the ROI must be dropped for a reseed");
        assert_eq!(tr.misses(), 0, "the counter must restart with the seed");
    }

    #[test]
    fn one_good_frame_clears_the_miss_counter() {
        let mut tr = Tracker::default();
        let roi = Roi {
            cx: 140.0,
            cy: 140.0,
            side: 100.0,
        };
        for _ in 0..3 {
            tr.finish(280.0, 280.0, roi, raw(0.9, [-1.0, -1.0, 1.0, 1.0]));
        }
        assert_eq!(tr.misses(), 3);
        assert!(tr
            .finish(280.0, 280.0, roi, raw(0.07, [-1.0, -1.0, 1.0, 1.0]))
            .is_some());
        assert_eq!(tr.misses(), 0);
    }

    #[test]
    fn a_box_that_runs_away_is_not_followed() {
        let mut tr = Tracker::default();
        let roi = Roi {
            cx: 140.0,
            cy: 140.0,
            side: 100.0,
        };
        // A box eight times the ROI: 800 px on a 280 px frame.
        let got = tr.finish(280.0, 280.0, roi, raw(0.07, [-8.0, -8.0, 8.0, 8.0]));
        assert!(got.is_none(), "an implausible ROI must not yield a pose");
        assert!(tr.roi().is_none(), "nor be carried into the next frame");
    }

    #[test]
    fn a_non_finite_sigma_is_a_rejection_not_a_pass() {
        let mut tr = Tracker::default();
        let roi = Roi {
            cx: 140.0,
            cy: 140.0,
            side: 100.0,
        };
        for s in [f32::NAN, f32::INFINITY] {
            assert!(tr
                .finish(280.0, 280.0, roi, raw(s, [-1.0, -1.0, 1.0, 1.0]))
                .is_none());
        }
    }

    /// Position and size come back out in frame pixels, against the ROI the
    /// pose was computed from — not against the frame and not against the 129
    /// grid.
    #[test]
    fn the_head_centre_and_size_are_reported_in_frame_pixels() {
        let mut tr = Tracker::default();
        tr.set_perspective(false);
        let roi = Roi {
            cx: 100.0,
            cy: 200.0,
            side: 80.0,
        };
        let mut r = raw(0.07, [-1.0, -1.0, 1.0, 1.0]);
        r.pos_size = [0.5, -0.25, 1.2];
        let m = tr.finish(280.0, 280.0, roi, r).unwrap();
        assert_eq!(m.centre_px, [100.0 + 20.0, 200.0 - 10.0]);
        assert_eq!(m.head_height_px, 96.0);
    }

    /// Turning the perspective correction off must change nothing else, and
    /// turning it on must change pitch — that is the whole point of it.
    #[test]
    fn the_perspective_correction_can_be_switched_off() {
        let roi = Roi {
            cx: 133.6,
            cy: 217.6,
            side: 94.5,
        };
        let mut r = raw(0.07, [-1.0, -1.0, 1.0, 1.0]);
        r.quat = [-0.0944984034, -0.0759362578, -0.0319216624, 0.992111266];
        let mut off = Tracker::default();
        off.set_perspective(false);
        let a = off.finish(280.0, 280.0, roi, r).unwrap();
        let mut on = Tracker::default();
        let b = on.finish(280.0, 280.0, roi, r).unwrap();
        assert!(
            (a.pitch_deg - 11.090_185_951).abs() < 1e-6,
            "uncorrected {a:?}"
        );
        assert!(
            (b.pitch_deg - a.pitch_deg) > 8.0,
            "the correction must add pitch below centre: {} then {}",
            a.pitch_deg,
            b.pitch_deg
        );
    }

    /// Every sign lives in one struct so that fixing one is a constant and not a
    /// hunt. Flipping them must flip the output and nothing else.
    #[test]
    fn the_sign_conventions_are_applied_from_one_place() {
        let roi = Roi {
            cx: 140.0,
            cy: 140.0,
            side: 94.5,
        };
        let mut r = raw(0.07, [-1.0, -1.0, 1.0, 1.0]);
        r.quat = [-0.0944984034, -0.0759362578, -0.0319216624, 0.992111266];
        let mut a = Tracker::default();
        let base = a.finish(280.0, 280.0, roi, r).unwrap();
        let mut b = Tracker::default();
        b.set_signs(Signs {
            yaw: -1.0,
            pitch: -1.0,
            roll: 1.0,
            x: 1.0,
            pitch_offset_deg: 5.0,
        });
        let flipped = b.finish(280.0, 280.0, roi, r).unwrap();
        assert!((flipped.yaw_deg + base.yaw_deg).abs() < 1e-9);
        assert!((flipped.roll_deg + base.roll_deg).abs() < 1e-9);
        assert!((flipped.pitch_deg + base.pitch_deg - 5.0).abs() < 1e-9);
        // The rotation itself is untouched by the conventions; only the report.
        assert_eq!(flipped.quat_world, base.quat_world);
    }

    #[test]
    fn fusion_takes_position_from_the_eyes_and_rotation_from_the_model() {
        let eyes = HeadPose {
            x_mm: 1.0,
            y_mm: 2.0,
            z_mm: 3.0,
            yaw_deg: 10.0,
            pitch_deg: 0.0,
            roll_deg: 20.0,
        };
        let model = ModelPose {
            quat_world: [1.0, 0.0, 0.0, 0.0],
            yaw_deg: -40.0,
            pitch_deg: 15.0,
            roll_deg: -50.0,
            x_mm: 100.0,
            y_mm: 200.0,
            z_mm: 300.0,
            centre_px: [0.0, 0.0],
            head_height_px: 90.0,
            sigma: 0.07,
            pos_sigma: 0.02,
            roi: Roi::whole(280.0, 280.0),
            next_roi: Roi::whole(280.0, 280.0),
        };
        let f = fuse(Some(eyes), Some(&model), RotationSource::Model).unwrap();
        assert_eq!(
            (f.x_mm, f.y_mm, f.z_mm),
            (1.0, 2.0, 3.0),
            "position from eyes"
        );
        assert_eq!((f.yaw_deg, f.pitch_deg, f.roll_deg), (-40.0, 15.0, -50.0));

        let g = fuse(Some(eyes), Some(&model), RotationSource::Geometric).unwrap();
        assert_eq!((g.yaw_deg, g.roll_deg), (10.0, 20.0), "yaw/roll from eyes");
        assert_eq!(g.pitch_deg, 15.0, "pitch always from the model");

        assert_eq!(fuse(Some(eyes), None, RotationSource::Model), Some(eyes));
        let m = fuse(None, Some(&model), RotationSource::Model).unwrap();
        assert_eq!((m.x_mm, m.pitch_deg), (100.0, 15.0));
        assert_eq!(fuse(None, None, RotationSource::Model), None);
    }

    // ---- Tests that need the real model file -------------------------------
    //
    // `#[ignore]`d because the weights are 13 MB, are not in this repository and
    // are not redistributable (see `model_store::TERMS`). Fetch them with
    //     tobii headpose --fetch-model
    // and run these with
    //     cargo test -p tobii-headpose -- --ignored

    fn model() -> OnnxPose {
        OnnxPose::from_store().unwrap_or_else(|e| {
            panic!("{e}\nthese tests need the model: `tobii headpose --fetch-model`")
        })
    }

    fn close(got: &[f32], want: &[f32], tol: f32, what: &str) {
        assert_eq!(got.len(), want.len(), "{what}: length");
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert!(
                (g - w).abs() <= tol,
                "{what}[{i}]: expected {w}, got {g} (tolerance {tol})"
            );
        }
    }

    /// The numerics end to end, against values recorded from onnxruntime.
    ///
    /// Tolerance is 1e-5, not equality: tract and ORT agree to about 3e-7 across
    /// all 23 output floats, which is ~30x of margin, but they use different
    /// convolution kernels and bit-equality would be luck.
    #[test]
    #[ignore = "needs the model file; see `tobii headpose --fetch-model`"]
    fn the_model_reproduces_the_recorded_golden_fixture() {
        let mut m = model();
        m.tensor = (0..INPUT * INPUT)
            .map(|k| {
                let (y, i) = (k / INPUT, k % INPUT);
                ((i * 7 + y * 13) % 255) as f32 / 255.0
            })
            .collect();
        let r = m.run().expect("inference");
        close(
            &r.pos_size,
            &[-0.0657382756, 0.195331037, 1.11734915],
            1e-5,
            "pos_size",
        );
        close(
            &r.quat,
            &[0.175782338, 0.342151791, -0.103544787, 0.917230189],
            1e-5,
            "quat",
        );
        close(
            &r.bbox,
            &[-0.618159473, -0.610497713, 1.0023644, 1.34904659],
            1e-5,
            "box",
        );
        close(
            &r.pos_size_scales,
            &[0.264111817, 0.320358574, 0.108970508],
            1e-5,
            "pos_size_scales",
        );
        close(&[r.sigma], &[0.685697794], 1e-5, "sigma");
    }

    /// The same, on a real ET5 face rather than a pattern — and a check that the
    /// confidence gate calls it a face.
    #[test]
    #[ignore = "needs the model file; see `tobii headpose --fetch-model`"]
    fn the_model_reproduces_the_real_face_crop() {
        let mut m = model();
        m.tensor = face_crop_tensor();
        assert_eq!(m.tensor.len(), INPUT * INPUT, "fixture size");
        let r = m.run().expect("inference");
        close(
            &r.pos_size,
            &[0.00863566063, 0.0278123319, 1.15246022],
            1e-5,
            "pos_size",
        );
        close(
            &r.quat,
            &[-0.0944984034, -0.0759362578, -0.0319216624, 0.992111266],
            1e-5,
            "quat",
        );
        close(&[r.sigma], &[0.0656043738], 1e-5, "sigma");
        assert!(r.sigma < SIGMA_MAX, "a real face must pass the gate");
    }

    /// The whole pipeline on a real frame, from the worst possible seed. The
    /// loop must find the head and settle, and the settled pose must match what
    /// an independent Python/onnxruntime implementation of the same algorithm
    /// produced on this exact frame.
    #[test]
    #[ignore = "needs the model file; see `tobii headpose --fetch-model`"]
    fn the_box_feedback_loop_converges_from_the_whole_frame() {
        let frame = face_frame();
        let mut m = model();
        m.tracker().set_perspective(false);
        m.tracker().set_roi(Some(Roi::whole(280.0, 280.0)));
        let mut last: Option<ModelPose> = None;
        let mut locked = None;
        for i in 0..12 {
            let got = m.estimate_detailed(&frame);
            if got.is_some() && locked.is_none() {
                locked = Some(i);
            }
            if let Some(g) = got {
                last = Some(g);
            }
        }
        let it = locked.expect("the loop never locked on");
        assert!(it <= 8, "took {it} frames to lock on");
        let p = last.expect("a pose");
        assert!(
            (p.roi.side - 94.7).abs() < 4.0,
            "converged crop side {} (expected ~94.7)",
            p.roi.side
        );
        assert!((p.roi.cx - 134.2).abs() < 4.0, "converged cx {}", p.roi.cx);
        assert!((p.roi.cy - 217.1).abs() < 4.0, "converged cy {}", p.roi.cy);
        assert!(p.sigma < 0.09, "converged sigma {}", p.sigma);
        // Reference from the Python implementation of this same algorithm on
        // this frame: yaw -7.1, pitch +12.5, roll -2.3, each with about a degree
        // of limit-cycle wobble.
        assert!((p.yaw_deg - -7.1).abs() < 2.0, "yaw {}", p.yaw_deg);
        assert!((p.pitch_deg - 12.5).abs() < 2.5, "pitch {}", p.pitch_deg);
        // The crate reports roll with the opposite sign to opentrack, so the
        // measured -2.3 comes back as +2.3 here.
        assert!((p.roll_deg - 2.3).abs() < 2.0, "roll {}", p.roll_deg);
    }

    /// The glint seed should reach the same attractor as the whole-frame start,
    /// just far sooner — that is the only thing the seed buys.
    #[test]
    #[ignore = "needs the model file; see `tobii headpose --fetch-model`"]
    fn the_glint_seed_locks_on_immediately() {
        let frame = face_frame();
        let mut m = model();
        let first = m
            .estimate_detailed(&frame)
            .expect("a pose on the very first frame");
        assert!(first.sigma < 0.09, "sigma on frame 1 was {}", first.sigma);
    }

    /// The one-shot crop is not merely worse, it is *confidently* worse: it
    /// passes the confidence gate while carrying several degrees of pitch error.
    /// This is why the box-feedback loop is not optional and why sigma is not a
    /// substitute for it.
    #[test]
    #[ignore = "needs the model file; see `tobii headpose --fetch-model`"]
    fn the_one_shot_crop_from_preprocess_is_measurably_worse() {
        let frame = face_frame();
        let mut m = model();
        m.tracker().set_perspective(false);
        let mut converged = None;
        for _ in 0..6 {
            if let Some(p) = m.estimate_detailed(&frame) {
                converged = Some(p);
            }
        }
        let good = converged.expect("the loop should converge");

        let (tensor, _bbox) = preprocess::preprocess(
            &frame,
            INPUT as u32,
            1,
            preprocess::Normalize::MeanStd {
                mean: 0.5,
                std: 1.0,
            },
            0.0,
        )
        .expect("preprocess finds a bright region");
        let mut one_shot = model();
        one_shot.tensor = tensor;
        let r = one_shot.run().expect("inference");
        let (_, pitch, _) = euler_deg(image_to_world(r.quat));

        assert!(
            r.sigma < SIGMA_MAX,
            "the point of this test is that the bad crop PASSES the gate; it scored {}",
            r.sigma
        );
        assert!(
            (pitch - good.pitch_deg).abs() > 4.0,
            "the one-shot crop should be several degrees off: {} against {}",
            pitch,
            good.pitch_deg
        );
    }

    #[test]
    #[ignore = "needs the model file; see `tobii headpose --fetch-model`"]
    fn all_five_output_names_and_shapes_are_resolved_at_load() {
        let m = model();
        // Loading validated them; assert the slots are a permutation, i.e. no
        // two names resolved to the same tensor.
        let mut seen = m.slots.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 5, "slots must be distinct: {:?}", m.slots);
    }

    #[test]
    #[ignore = "needs the model file; see `tobii headpose --fetch-model`"]
    fn the_backend_is_object_safe_and_reports_its_kind() {
        let frame = face_frame();
        let mut m: Box<dyn PoseModel> = Box::new(model());
        assert_eq!(m.kind(), ModelKind::OpentrackOnnx);
        assert!(
            m.estimate(&frame).is_some(),
            "a real face should give a pose"
        );
    }

    #[test]
    fn loading_something_that_is_not_a_model_fails_loudly() {
        let e = OnnxPose::load_bytes(b"not an onnx graph").unwrap_err();
        assert!(matches!(e, OnnxError::Graph(_)), "{e}");
        let e = OnnxPose::load(&ModelConfig {
            kind: ModelKind::TobiiVino,
            model_path: "/nonexistent".into(),
        })
        .unwrap_err();
        assert!(matches!(e, OnnxError::WrongKind(_)), "{e}");
        let e = OnnxPose::load(&ModelConfig {
            kind: ModelKind::OpentrackOnnx,
            model_path: "/nonexistent/head-pose.onnx".into(),
        })
        .unwrap_err();
        assert!(matches!(e, OnnxError::ModelMissing(_)), "{e}");
        // The message has to name the fix, not just the problem.
        assert!(e.to_string().contains("--fetch-model"), "{e}");
    }
}
