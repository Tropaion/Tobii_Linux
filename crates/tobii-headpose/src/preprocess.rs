//! # What an ET5 frame actually looks like
//!
//! **[CONFIRMED]** 2026-08-26, 280x280 8-bit, face at ~70 cm, eyes detected:
//! raw pixel values span roughly **8..42 of 255** — 98% of the frame sits in the
//! darkest eighth, and a capture of an *empty* scene looks nearly identical
//! (peak 30, a smooth radial illuminator vignette). Brightness alone therefore
//! cannot tell "face" from "empty room", which is why `face_bbox` claims a face
//! either way and must never be the gate for whether a pose is emitted.
//!
//! But the structure is there. A plain percentile stretch of 8..42 onto 0..255
//! yields an ordinary, legible greyscale face: brows, eye corners, nose, mouth,
//! jawline, ears. The input is dim, not information-poor, and the contrast work
//! here is what makes it usable rather than a nicety.
//!
//! Two properties any model must cope with, both visible in that capture: the
//! illuminators light the face **from below**, and they put two **blown-out
//! corneal glints** (value 255) exactly where a model trained on photographs
//! expects dark pupils.
//!
//! Shared image preprocessing for the neural head-pose backends: equalise the
//! wide-angle NIR frame, locate the face in it, crop it, resize to the model's
//! input size, and normalize to a float tensor.
//!
//! The ET5 cameras image the whole face wide-angle against a mostly-dark
//! background (the face is the bright region, lit by the tracker's own IR), so a
//! brightness heuristic localizes it well without a separate detector. Every
//! step here is pure and unit-tested; the exact crop padding and normalization a
//! given model wants are parameters its adapter fills in (Tobii's from the
//! extracted spec; the open models from their published preprocessing).

use tobii_protocol::CameraFrame;

/// An axis-aligned pixel box `[x0, y0)`..`[x1, y1)` (half-open), clamped to the
/// image. `x1 > x0` and `y1 > y0` for any non-empty box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BBox {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl BBox {
    pub fn width(&self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }
    pub fn height(&self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }
    /// Expand by `pad` pixels on every side, clamped to `w`×`h`.
    pub fn padded(&self, pad: u32, w: u32, h: u32) -> BBox {
        BBox {
            x0: self.x0.saturating_sub(pad),
            y0: self.y0.saturating_sub(pad),
            x1: (self.x1 + pad).min(w),
            y1: (self.y1 + pad).min(h),
        }
    }
    /// Grow the shorter side so the box is square (side = the longer side),
    /// re-centred and clamped — head-pose models want a square input.
    pub fn squared(&self, w: u32, h: u32) -> BBox {
        let side = self.width().max(self.height());
        let cx = (self.x0 + self.x1) / 2;
        let cy = (self.y0 + self.y1) / 2;
        let half = side / 2;
        BBox {
            x0: cx.saturating_sub(half),
            y0: cy.saturating_sub(half),
            x1: (cx + half).min(w),
            y1: (cy + half).min(h),
        }
    }
}

/// Contrast-limited adaptive histogram equalisation (CLAHE).
///
/// NIR contrast on the ET5 varies enormously: the tracker lights the user with
/// its own IR, so a user who leans back or turns away loses contrast, and the
/// wide-angle optics light one side of a face harder than the other. A *global*
/// percentile stretch was rejected for this: it is one affine map applied to
/// every pixel, so it cannot pull the dim side of a face up relative to the
/// bright side — and for [`face_bbox`]'s `mean + k·std` threshold an affine map
/// is exactly a no-op, since it scales the mean and the standard deviation
/// together and the same pixels pass. Equalising per tile is what lifts local
/// detail on the dim side over the threshold; the clip limit is what stops a
/// flat, dark background tile from being stretched into full-range noise.
///
/// It bounds rather than removes uneven illumination: the redistributed mass
/// leaves each tile's mapping partly identity, so a smooth brightness gradient
/// largely survives (measured: a pure ramp comes through with its slope intact)
/// while local contrast is amplified by up to `clip_limit + 1`.
///
/// Deliberately a straight port of the textbook/OpenCV formulation — per-tile
/// histogram, clip at a multiple of the flat-histogram height, redistribute the
/// clipped mass, bilinear blend between the four surrounding tile mappings — so
/// that a pipeline tuned against OpenCV's `createCLAHE` transfers here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clahe {
    /// Bin ceiling as a multiple of a perfectly flat histogram's height
    /// (`tile_area / 256`), which bounds the contrast gain to roughly this
    /// factor. `<= 0` (or non-finite) disables clipping, degrading to plain
    /// adaptive histogram equalisation.
    pub clip_limit: f32,
    pub tiles_x: u32,
    pub tiles_y: u32,
}

impl Default for Clahe {
    /// Clip limit 2.0 over an 8×8 tile grid — the settings a working
    /// third-party pipeline for these same ET5 NIR frames uses before face and
    /// landmark detection. **UNVERIFIED here**: with no ET5 captures in-repo they
    /// are adopted from that pipeline, not re-derived, and nothing below measures
    /// them against real frames.
    fn default() -> Self {
        Self {
            clip_limit: 2.0,
            tiles_x: 8,
            tiles_y: 8,
        }
    }
}

/// Gray levels in an 8-bit histogram.
const LEVELS: usize = 256;

impl Clahe {
    /// Equalise a `w`×`h` 8-bit grayscale buffer, returning a new `w*h`-byte
    /// buffer (a longer input is read only as far as its stated geometry).
    ///
    /// Degenerate geometry (a zero dimension, or a buffer shorter than `w*h`) is
    /// returned unchanged rather than treated as an error: this sits in the hot
    /// per-frame path, and a malformed frame should cost the caller a bad crop,
    /// not a panic. Tile counts are clamped to at least one and at most one tile
    /// per pixel along each axis.
    pub fn apply(&self, pixels: &[u8], w: u32, h: u32) -> Vec<u8> {
        let (w, h) = (w as usize, h as usize);
        let n = w.saturating_mul(h);
        if n == 0 || pixels.len() < n {
            return pixels.to_vec();
        }
        let tiles_x = (self.tiles_x.max(1) as usize).min(w);
        let tiles_y = (self.tiles_y.max(1) as usize).min(h);
        let luts = self.tile_luts(pixels, w, h, tiles_x, tiles_y);
        let cols = axis_blend(w, tiles_x);
        let rows = axis_blend(h, tiles_y);

        let mut out = Vec::with_capacity(n);
        for y in 0..h {
            let (r0, r1, wy) = rows[y];
            for x in 0..w {
                let (c0, c1, wx) = cols[x];
                let v = pixels[y * w + x] as usize;
                let at = |r: usize, c: usize| luts[(r * tiles_x + c) * LEVELS + v] as f32;
                let top = at(r0, c0) * (1.0 - wx) + at(r0, c1) * wx;
                let bottom = at(r1, c0) * (1.0 - wx) + at(r1, c1) * wx;
                out.push((top * (1.0 - wy) + bottom * wy).round() as u8);
            }
        }
        out
    }

    /// One 256-entry mapping per tile, laid out row-major by tile.
    fn tile_luts(&self, px: &[u8], w: usize, h: usize, tiles_x: usize, tiles_y: usize) -> Vec<u8> {
        let mut luts = vec![0u8; tiles_x * tiles_y * LEVELS];
        // Tile bounds as `i * len / tiles` spread the remainder over the whole
        // grid when the frame does not divide evenly, keeping tiles within a
        // pixel of each other instead of dumping the remainder on the last one.
        for ty in 0..tiles_y {
            let (y0, y1) = (ty * h / tiles_y, (ty + 1) * h / tiles_y);
            for tx in 0..tiles_x {
                let (x0, x1) = (tx * w / tiles_x, (tx + 1) * w / tiles_x);
                let mut hist = [0f32; LEVELS];
                for y in y0..y1 {
                    for &p in &px[y * w + x0..y * w + x1] {
                        hist[p as usize] += 1.0;
                    }
                }
                let area = ((y1 - y0) * (x1 - x0)) as f32;
                clip_histogram(&mut hist, area, self.clip_limit);

                // The mapping is the scaled cumulative histogram; clipping
                // preserves total mass, so it still ends at 255.
                let lut = &mut luts[(ty * tiles_x + tx) * LEVELS..][..LEVELS];
                let mut cumulative = 0.0;
                for (slot, count) in lut.iter_mut().zip(hist) {
                    cumulative += count;
                    *slot = (cumulative * 255.0 / area).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        luts
    }
}

/// Clip every bin and hand the removed mass back out uniformly, so the mapping
/// keeps its full output range while its slope — the local contrast gain — stays
/// bounded.
fn clip_histogram(hist: &mut [f32; LEVELS], area: f32, clip_limit: f32) {
    // A non-positive (or NaN) limit means "no limit": plain adaptive histogram
    // equalisation.
    if clip_limit.is_nan() || clip_limit <= 0.0 {
        return;
    }
    // One gray level's worth of a perfectly flat histogram is `area / 256`; the
    // clip limit counts those (OpenCV's convention, which is what the 2.0 default
    // is expressed in). The one-pixel floor is OpenCV's too, where the limit is
    // an integer and truncating it to 0 really would flatten the tile; here the
    // arithmetic is float and a sub-pixel limit loses nothing (the clipped mass
    // is redistributed either way), so the floor's only effect is to *raise* the
    // effective limit to `256 / area` for tiles under 128 pixels. Kept for
    // OpenCV parity; irrelevant at the 8×8-on-280×280 default, whose tiles are
    // 1225 pixels.
    let limit = (clip_limit * area / LEVELS as f32).max(1.0);
    let mut excess = 0.0;
    for bin in hist.iter_mut() {
        if *bin > limit {
            excess += *bin - limit;
            *bin = limit;
        }
    }
    // A single uniform redistribution pass, not the iterative refill: the bins it
    // pushes back over the limit are over by at most `excess / 256`, i.e. one
    // flat-histogram step, so the gain stays bounded by `clip_limit + 1`.
    let share = excess / LEVELS as f32;
    for bin in hist.iter_mut() {
        *bin += share;
    }
}

/// Per-coordinate interpolation weights along one axis: the two tile indices
/// whose centres bracket each coordinate, and how far along it lies between
/// them.
///
/// Blending the neighbouring tiles' mappings is what makes CLAHE usable at all —
/// applying each tile's mapping to its own pixels alone leaves visible seams at
/// every tile boundary, which a detector reads as edges. Coordinates outside the
/// outermost centres take the nearest tile's mapping unblended.
fn axis_blend(len: usize, tiles: usize) -> Vec<(usize, usize, f32)> {
    // `apply` clamps to at most one tile per pixel, so a tile is never empty and
    // the centres are strictly increasing (which the bracketing scan relies on).
    let centre = |i: usize| {
        let (start, end) = (i * len / tiles, (i + 1) * len / tiles);
        (start + end).saturating_sub(1) as f32 * 0.5
    };
    let mut out = Vec::with_capacity(len);
    let mut i = 0usize;
    for p in 0..len {
        let p = p as f32;
        while i + 1 < tiles && centre(i + 1) <= p {
            i += 1;
        }
        if p <= centre(i) || i + 1 >= tiles {
            out.push((i, i, 0.0));
        } else {
            let (lo, hi) = (centre(i), centre(i + 1));
            out.push((i, i + 1, (p - lo) / (hi - lo)));
        }
    }
    out
}

/// Locate the face as the bounding box of the bright region. Pixels brighter
/// than `mean + k*stddev` are taken as face; the box spans them. Returns `None`
/// if the frame is essentially uniform (no eyes / nothing lit). `k ≈ 1.5` is a
/// sensible default for the ET5's high-contrast NIR frames.
///
/// [`preprocess`] runs this on [`Clahe`]-equalised pixels, which is what makes
/// the threshold survive a face lit unevenly across its width; the statistics
/// below are then the equalised frame's. Note what that does *not* fix: a
/// reflective object in the background bright enough to pass the threshold still
/// drags the box out to enclose it, equalised or not. Only a real detector fixes
/// that.
pub fn face_bbox(frame: &CameraFrame, k: f64) -> Option<BBox> {
    let (w, h) = (frame.width, frame.height);
    if w == 0 || h == 0 || frame.pixels.len() < (w * h) as usize {
        return None;
    }
    let n = (w * h) as f64;
    let sum: f64 = frame.pixels.iter().map(|&b| b as f64).sum();
    let mean = sum / n;
    let var = frame
        .pixels
        .iter()
        .map(|&b| (b as f64 - mean).powi(2))
        .sum::<f64>()
        / n;
    let std = var.sqrt();
    // A near-uniform frame (< 1 gray level of variation) has no bright region to
    // localize — no face lit by the tracker's IR. Guard against it, else the
    // threshold collapses onto the mean and every pixel "passes".
    if std < 1.0 {
        return None;
    }
    let thresh = mean + k * std;

    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
    let mut any = false;
    for y in 0..h {
        let row = &frame.pixels[(y * w) as usize..((y + 1) * w) as usize];
        for (x, &p) in row.iter().enumerate() {
            if p as f64 >= thresh {
                let x = x as u32;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
                any = true;
            }
        }
    }
    any.then_some(BBox { x0, y0, x1, y1 })
}

/// Crop `bbox` and nearest-neighbour resize to `out_w`×`out_h`, returning 8-bit
/// grayscale. Nearest-neighbour keeps this dependency-free and deterministic; a
/// model that needs bilinear can be added per-adapter later.
pub fn crop_resize(frame: &CameraFrame, bbox: BBox, out_w: u32, out_h: u32) -> Vec<u8> {
    let (w, bw, bh) = (frame.width, bbox.width().max(1), bbox.height().max(1));
    let mut out = Vec::with_capacity((out_w * out_h) as usize);
    for oy in 0..out_h {
        let sy = bbox.y0 + (oy * bh) / out_h.max(1);
        for ox in 0..out_w {
            let sx = bbox.x0 + (ox * bw) / out_w.max(1);
            out.push(frame.pixels[(sy * w + sx) as usize]);
        }
    }
    out
}

/// How a model wants its input floats scaled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Normalize {
    /// `p / 255` → `[0, 1]`.
    Unit,
    /// `p / 127.5 - 1` → `[-1, 1]`.
    SignedUnit,
    /// `(p/255 - mean) / std`, per-scalar (grayscale).
    MeanStd { mean: f32, std: f32 },
}

impl Normalize {
    pub fn apply(&self, byte: u8) -> f32 {
        let p = byte as f32;
        match *self {
            Normalize::Unit => p / 255.0,
            Normalize::SignedUnit => p / 127.5 - 1.0,
            Normalize::MeanStd { mean, std } => (p / 255.0 - mean) / std,
        }
    }
}

/// The bright-region threshold used by [`preprocess`], in standard deviations
/// above the frame mean.
const FACE_K: f64 = 1.5;

/// Full preprocessing: equalise, locate + square-crop the face, resize to
/// `size`×`size`, and normalize to an `f32` tensor. Returns the tensor in CHW
/// order with a single channel (`1×size×size`), plus the crop box used (for
/// mapping results back). Grayscale is replicated to `channels` if a model
/// expects 3 (RGB).
///
/// Equalisation is on by default because the open backends this feeds
/// ([`ModelKind::OpentrackOnnx`], [`ModelKind::SixDRepNet`]) are trained on RGB
/// photographs and see raw NIR as out of their training domain — the
/// third-party ET5 pipeline that [`Clahe::default`] comes from reports exactly
/// that failure. A backend that must instead reproduce Tobii's own
/// preprocessing should call [`preprocess_with`] with `None`.
///
/// [`ModelKind::OpentrackOnnx`]: crate::model::ModelKind::OpentrackOnnx
/// [`ModelKind::SixDRepNet`]: crate::model::ModelKind::SixDRepNet
pub fn preprocess(
    frame: &CameraFrame,
    size: u32,
    channels: usize,
    norm: Normalize,
    pad_frac: f64,
) -> Option<(Vec<f32>, BBox)> {
    preprocess_with(
        frame,
        size,
        channels,
        norm,
        pad_frac,
        Some(Clahe::default()),
    )
}

/// [`preprocess`] with the contrast-equalisation step under caller control.
pub fn preprocess_with(
    frame: &CameraFrame,
    size: u32,
    channels: usize,
    norm: Normalize,
    pad_frac: f64,
    equalize: Option<Clahe>,
) -> Option<(Vec<f32>, BBox)> {
    // Whether there is a face at all is decided on the RAW frame, and only then
    // is the equalised frame asked where it is. Equalisation redistributes
    // contrast, it never manufactures signal — but it does multiply the standard
    // deviation that [`face_bbox`]'s uniform-frame guard is phrased in (by up to
    // `clip_limit + 1`), so an empty dark frame carrying a few percent of hot
    // pixels equalises into something that clears the guard and yields a
    // full-frame "face". Measured here on synthetic frames: 5% of pixels 3 gray
    // levels hot reads as `None` raw and as a 280×280 box equalised.
    let raw_box = face_bbox(frame, FACE_K)?;

    // Equalise the whole frame once, before locating: the box and the crop then
    // come from the same pixels, so the contrast the model sees never depends on
    // where the box happened to land, and the tiling stays anchored to the frame
    // rather than drifting with the box from frame to frame.
    let equalized = equalize.map(|c| CameraFrame {
        timestamp_us: frame.timestamp_us,
        width: frame.width,
        height: frame.height,
        bit_depth: frame.bit_depth,
        pixels: c.apply(&frame.pixels, frame.width, frame.height),
    });
    let unpadded = match &equalized {
        // Equalising redistributes the frame's statistics, so the threshold can
        // in principle come up empty on a frame the raw pass localized. Keep the
        // raw box then, rather than dropping a frame that did contain a face.
        Some(eq) => face_bbox(eq, FACE_K).unwrap_or(raw_box),
        None => raw_box,
    };
    let frame = equalized.as_ref().unwrap_or(frame);

    let pad = ((unpadded.width().max(unpadded.height()) as f64) * pad_frac) as u32;
    let bbox = unpadded
        .padded(pad, frame.width, frame.height)
        .squared(frame.width, frame.height);
    let gray = crop_resize(frame, bbox, size, size);
    let plane: Vec<f32> = gray.iter().map(|&b| norm.apply(b)).collect();
    // CHW: for 3-channel models, replicate the grayscale plane across channels.
    let mut tensor = Vec::with_capacity(plane.len() * channels);
    for _ in 0..channels.max(1) {
        tensor.extend_from_slice(&plane);
    }
    Some((tensor, bbox))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w`×`h` frame that is `bg` everywhere except a `fg` rectangle.
    fn frame_with_rect(w: u32, h: u32, r: BBox, bg: u8, fg: u8) -> CameraFrame {
        let mut px = vec![bg; (w * h) as usize];
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                px[(y * w + x) as usize] = fg;
            }
        }
        CameraFrame {
            timestamp_us: 0,
            width: w,
            height: h,
            bit_depth: 8,
            pixels: px,
        }
    }

    /// Deterministic pseudo-random bits, so "sensor noise" in these tests is
    /// reproducible without pulling in a rand dependency (same trick as
    /// `tobii-gtk`'s particle bursts).
    fn splitmix(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Peak-to-peak gray span of a rectangular window, the quantity every
    /// contrast assertion below is phrased in.
    fn span(px: &[u8], w: u32, win: BBox) -> u32 {
        let (mut lo, mut hi) = (255u8, 0u8);
        for y in win.y0..win.y1 {
            for x in win.x0..win.x1 {
                let p = px[(y * w + x) as usize];
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
        (hi - lo) as u32
    }

    /// A horizontal ramp spanning `levels` gray values from `base`: pure
    /// illumination, no local structure at all.
    fn dim_ramp(w: u32, h: u32, base: u8, levels: u32) -> Vec<u8> {
        (0..w * h)
            .map(|i| base + (((i % w) * levels) / w) as u8)
            .collect()
    }

    /// Low-contrast texture: deterministic noise confined to `levels` gray
    /// values above `base` — a dim, far-away user's worth of facial detail.
    fn dim_texture(w: u32, h: u32, base: u8, levels: u32) -> Vec<u8> {
        let mut state = 0x7be1;
        (0..w * h)
            .map(|_| base + (splitmix(&mut state) % levels as u64) as u8)
            .collect()
    }

    const CENTRE_WINDOW: BBox = BBox {
        x0: 56,
        y0: 56,
        x1: 72,
        y1: 72,
    };

    #[test]
    fn a_flat_image_stays_flat_under_equalisation() {
        for level in [0u8, 37, 128, 255] {
            let px = vec![level; 128 * 128];
            let out = Clahe::default().apply(&px, 128, 128);
            assert_eq!(out.len(), px.len());
            assert!(
                out.iter().all(|&p| p == out[0]),
                "level={level}: a flat input must not gain structure"
            );
            // Nor may it be dragged to an arbitrary level: with the histogram
            // mass redistributed the mapping is near-identity, not a stretch.
            let drift = (out[0] as i32 - level as i32).abs();
            assert!(drift <= 8, "level={level} became {} ", out[0]);
        }
    }

    /// A synthetic stand-in for an ET5 NIR frame (280×280, per `camera.rs`): an
    /// elliptical face of radii `rx`×`ry` centred in the frame, lit by the
    /// tracker's own IR to `peak` gray levels with a soft radial falloff, over a
    /// dark noisy background. `dim_side` is the fraction of that illumination
    /// reaching the left edge of the face, so `1.0` is even lighting and `0.45`
    /// is a face lit hard from one side. Returns the frame and the true box.
    fn synthetic_face(rx: f64, ry: f64, peak: f64, dim_side: f64) -> (CameraFrame, BBox) {
        const SIDE: u32 = 280;
        let centre = SIDE as f64 / 2.0;
        let mut state = 0xfacefeed;
        let mut px = vec![0u8; (SIDE * SIDE) as usize];
        for y in 0..SIDE {
            for x in 0..SIDE {
                let (fx, fy) = ((x as f64 - centre) / rx, (y as f64 - centre) / ry);
                let r2 = fx * fx + fy * fy;
                let noise = (splitmix(&mut state) % 7) as f64 - 3.0;
                let v = if r2 <= 1.0 {
                    let across = (x as f64 - (centre - rx)) / (2.0 * rx);
                    let lit = dim_side + (1.0 - dim_side) * across;
                    peak * lit * (1.0 - 0.35 * r2) + noise * 2.0
                } else {
                    10.0 + noise
                };
                px[(y * SIDE + x) as usize] = v.clamp(0.0, 255.0) as u8;
            }
        }
        let truth = BBox {
            x0: (centre - rx) as u32,
            y0: (centre - ry) as u32,
            x1: (centre + rx) as u32,
            y1: (centre + ry) as u32,
        };
        (
            CameraFrame {
                timestamp_us: 0,
                width: SIDE,
                height: SIDE,
                bit_depth: 8,
                pixels: px,
            },
            truth,
        )
    }

    fn iou(a: BBox, b: BBox) -> f64 {
        let ix = a.x1.min(b.x1).saturating_sub(a.x0.max(b.x0)) as f64;
        let iy = a.y1.min(b.y1).saturating_sub(a.y0.max(b.y0)) as f64;
        let inter = ix * iy;
        let union = (a.width() * a.height()) as f64 + (b.width() * b.height()) as f64 - inter;
        if union <= 0.0 {
            0.0
        } else {
            inter / union
        }
    }

    #[test]
    fn a_low_contrast_image_gets_its_range_expanded() {
        let (w, h) = (128, 128);
        let px = dim_texture(w, h, 100, 10);
        let out = Clahe::default().apply(&px, w, h);

        let before = span(&px, w, CENTRE_WINDOW);
        let after = span(&out, w, CENTRE_WINDOW);
        assert!(
            after >= before * 2,
            "local span {before} -> {after}: equalisation did not expand it"
        );
    }

    #[test]
    fn a_pure_illumination_gradient_is_not_amplified_into_structure() {
        // A ramp is illumination, not detail: neighbouring tiles' mappings are
        // the same curve shifted, so blending between them reproduces the ramp
        // instead of stretching each tile to full range (which is what would
        // manufacture edges out of a smooth falloff, and seams at every tile).
        let (w, h) = (128, 128);
        let px = dim_ramp(w, h, 100, 20);
        let out = Clahe::default().apply(&px, w, h);

        let before = span(&px, w, CENTRE_WINDOW);
        let after = span(&out, w, CENTRE_WINDOW);
        assert!(
            after <= before + 1,
            "a smooth gradient was amplified locally: {before} -> {after}"
        );
        // Bound it from below too, else crushing the gradient to flat would pass
        // this test while destroying exactly the falloff a pose model reads
        // shape from. Measured: the ramp survives across the full row, 19 -> 23.
        let full_row = BBox {
            x0: 0,
            y0: h / 2,
            x1: w,
            y1: h / 2 + 1,
        };
        let (row_before, row_after) = (span(&px, w, full_row), span(&out, w, full_row));
        assert!(
            row_after >= row_before,
            "the gradient was flattened away: {row_before} -> {row_after} across the row"
        );
    }

    #[test]
    fn the_clip_limit_bounds_the_contrast_gain() {
        let (w, h) = (128, 128);
        let px = dim_texture(w, h, 100, 10);
        let before = span(&px, w, CENTRE_WINDOW);

        let clipped = Clahe {
            clip_limit: 2.0,
            ..Clahe::default()
        }
        .apply(&px, w, h);
        let unclipped = Clahe {
            clip_limit: 0.0,
            ..Clahe::default()
        }
        .apply(&px, w, h);

        let (bounded, unbounded) = (
            span(&clipped, w, CENTRE_WINDOW),
            span(&unclipped, w, CENTRE_WINDOW),
        );
        assert!(
            bounded < unbounded,
            "clip limit changed nothing: {bounded} vs {unbounded}"
        );
        // The gain ceiling: a bin may hold `clip_limit` flat-histogram steps plus
        // at most one more from the redistributed excess, so the mapping's slope
        // — and with it the span of any input range — grows by no more than
        // `clip_limit + 1`.
        assert!(
            bounded <= before * 3 + 1,
            "gain exceeded the clip bound: {before} -> {bounded}"
        );
        // Without a limit the same input is stretched far past that ceiling,
        // which is what would amplify a dark tile's sensor noise into structure.
        assert!(
            unbounded > before * 3 + 1,
            "unclipped equalisation should blow past the bound: {before} -> {unbounded}"
        );
    }

    #[test]
    fn equalisation_never_inverts_contrast() {
        // One tile means one mapping for the whole image and no interpolation, so
        // the mapping can be read straight off the output: it may compress or
        // expand, but it must never reorder two gray levels.
        let (w, h) = (64, 48);
        let mut state = 0x5eed;
        let px: Vec<u8> = (0..w * h)
            .map(|_| (splitmix(&mut state) >> 56) as u8)
            .collect();
        let out = Clahe {
            clip_limit: 2.0,
            tiles_x: 1,
            tiles_y: 1,
        }
        .apply(&px, w, h);

        let mut mapping: [Option<u8>; 256] = [None; 256];
        for (&input, &output) in px.iter().zip(&out) {
            match mapping[input as usize] {
                None => mapping[input as usize] = Some(output),
                Some(prev) => assert_eq!(prev, output, "one input value, two outputs"),
            }
        }
        let mapped: Vec<u8> = mapping.iter().flatten().copied().collect();
        assert!(
            mapped.windows(2).all(|p| p[0] <= p[1]),
            "the mapping is not monotone: {mapped:?}"
        );
    }

    #[test]
    fn bilinear_blending_flattens_the_tile_seams() {
        // Equalising each tile in isolation leaves a brightness step at every
        // tile boundary, which a detector reads as a face-sized edge. Blending
        // the four surrounding mappings per pixel is what removes it, so compare
        // against the stitched-tiles version this replaces.
        let (w, h) = (128u32, 128u32);
        let (tile, tiles) = (16u32, 8u32);
        let px: Vec<u8> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                // A brightness gradient (so neighbouring tiles disagree about
                // their mapping) plus fine detail (so there is something to
                // equalise at all).
                (40 + x * 120 / w) as u8 + (((x % 3) + (y % 5)) * 2) as u8
            })
            .collect();

        let blended = Clahe::default().apply(&px, w, h);
        let mut stitched = vec![0u8; (w * h) as usize];
        for ty in 0..tiles {
            for tx in 0..tiles {
                let (x0, y0) = (tx * tile, ty * tile);
                let patch: Vec<u8> = (0..tile * tile)
                    .map(|i| px[((y0 + i / tile) * w + x0 + i % tile) as usize])
                    .collect();
                let eq = Clahe {
                    clip_limit: 2.0,
                    tiles_x: 1,
                    tiles_y: 1,
                }
                .apply(&patch, tile, tile);
                for i in 0..tile * tile {
                    stitched[((y0 + i / tile) * w + x0 + i % tile) as usize] = eq[i as usize];
                }
            }
        }

        let worst_step = |img: &[u8]| {
            let mut worst = 0i32;
            for y in 0..h {
                for x in 1..w {
                    let d = img[(y * w + x) as usize] as i32 - img[(y * w + x - 1) as usize] as i32;
                    worst = worst.max(d.abs());
                }
            }
            worst
        };
        let (with_blend, without) = (worst_step(&blended), worst_step(&stitched));
        assert!(
            with_blend * 2 < without,
            "blending barely helped: {with_blend} vs {without} unblended"
        );
    }

    #[test]
    fn degenerate_geometry_is_equalised_without_panicking() {
        let c = Clahe::default();
        assert!(c.apply(&[], 0, 0).is_empty());
        assert_eq!(
            c.apply(&[9], 0, 5),
            vec![9],
            "a zero axis is a pass-through"
        );
        assert_eq!(c.apply(&[7], 1, 1).len(), 1);
        // Non-square and indivisible by the 8x8 grid: every pixel still written.
        let odd: Vec<u8> = (0..37u32 * 23).map(|i| (i % 251) as u8).collect();
        assert_eq!(c.apply(&odd, 37, 23).len(), 37 * 23);
        // More tiles than pixels along an axis.
        let tiny = Clahe {
            clip_limit: 2.0,
            tiles_x: 64,
            tiles_y: 64,
        };
        assert_eq!(tiny.apply(&odd, 37, 23).len(), 37 * 23);
        // A buffer shorter than the claimed geometry is passed through untouched
        // rather than read out of bounds.
        assert_eq!(c.apply(&[1, 2, 3], 4, 4), vec![1, 2, 3]);
    }

    #[test]
    fn face_bbox_finds_the_bright_rectangle() {
        let r = BBox {
            x0: 100,
            y0: 180,
            x1: 180,
            y1: 260,
        };
        let f = frame_with_rect(280, 280, r, 10, 200);
        let b = face_bbox(&f, 1.5).expect("a bright region");
        assert_eq!(b, r);
    }

    #[test]
    fn face_bbox_none_on_uniform_frame() {
        let f = frame_with_rect(
            280,
            280,
            BBox {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            },
            30,
            30,
        );
        assert!(face_bbox(&f, 1.5).is_none());
    }

    #[test]
    fn squared_box_is_square_and_centered() {
        let b = BBox {
            x0: 100,
            y0: 180,
            x1: 180,
            y1: 260,
        }; // 80x80 already square
        let s = b.squared(280, 280);
        assert_eq!(s.width(), s.height());
        let wide = BBox {
            x0: 40,
            y0: 100,
            x1: 200,
            y1: 140,
        }; // 160x40
        let sw = wide.squared(280, 280);
        assert_eq!(sw.width(), sw.height());
    }

    #[test]
    fn crop_resize_produces_requested_size() {
        let r = BBox {
            x0: 90,
            y0: 90,
            x1: 190,
            y1: 190,
        };
        let f = frame_with_rect(280, 280, r, 0, 255);
        let out = crop_resize(&f, r, 64, 64);
        assert_eq!(out.len(), 64 * 64);
        assert!(out.iter().all(|&p| p == 255)); // the whole crop was the fg rect
    }

    #[test]
    fn normalize_ranges() {
        assert_eq!(Normalize::Unit.apply(255), 1.0);
        assert_eq!(Normalize::Unit.apply(0), 0.0);
        assert_eq!(Normalize::SignedUnit.apply(255), 1.0);
        assert!((Normalize::SignedUnit.apply(0) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn preprocess_shape_and_channels() {
        let r = BBox {
            x0: 100,
            y0: 180,
            x1: 180,
            y1: 260,
        };
        let f = frame_with_rect(280, 280, r, 10, 200);
        let (t1, _) = preprocess(&f, 64, 1, Normalize::Unit, 0.2).unwrap();
        assert_eq!(t1.len(), 64 * 64);
        let (t3, _) = preprocess(&f, 64, 3, Normalize::Unit, 0.2).unwrap();
        assert_eq!(t3.len(), 3 * 64 * 64);
        // the 3-channel tensor is the 1-channel plane replicated
        assert_eq!(&t3[..64 * 64], t1.as_slice());
    }

    /// A face lit hard on one side and dimly on the other — the case the plain
    /// threshold handles worst, because the dim side never clears `mean + 1.5σ`.
    ///
    /// Synthetic, not an ET5 capture: this pins the mechanism (equalisation
    /// recovers the dim side of the box), it does not measure real-world
    /// detection accuracy.
    #[test]
    fn equalising_recovers_the_dim_side_of_an_unevenly_lit_face() {
        let (frame, truth) = synthetic_face(70.0, 90.0, 180.0, 0.45);
        let equalized = CameraFrame {
            pixels: Clahe::default().apply(&frame.pixels, frame.width, frame.height),
            ..frame.clone()
        };

        let plain = iou(face_bbox(&frame, FACE_K).expect("a lit face"), truth);
        let equalised = iou(face_bbox(&equalized, FACE_K).expect("a lit face"), truth);
        assert!(
            equalised > plain + 0.1,
            "equalisation did not recover the dim side: iou {plain:.3} -> {equalised:.3}"
        );
    }

    /// The same comparison on an evenly lit face, where there is nothing to
    /// recover: equalisation must not make the easy case worse.
    #[test]
    fn equalising_does_not_spoil_an_evenly_lit_face() {
        let (frame, truth) = synthetic_face(70.0, 90.0, 180.0, 1.0);
        let equalized = CameraFrame {
            pixels: Clahe::default().apply(&frame.pixels, frame.width, frame.height),
            ..frame.clone()
        };

        let plain = iou(face_bbox(&frame, FACE_K).expect("a lit face"), truth);
        let equalised = iou(face_bbox(&equalized, FACE_K).expect("a lit face"), truth);
        assert!(
            equalised >= plain - 0.01,
            "equalisation regressed the easy case: iou {plain:.3} -> {equalised:.3}"
        );
    }

    #[test]
    fn a_frame_of_nothing_but_hot_pixels_is_still_not_a_face() {
        // 5% of pixels three gray levels hot on a dark background: nobody is in
        // front of the tracker, and the raw frame reads as uniform. Equalisation
        // multiplies that noise past the uniform-frame guard, so localising on
        // the equalised frame alone would hand back a full-frame "face" — which
        // is why the existence decision stays on the raw pixels.
        let mut state = 0xabcd;
        let pixels: Vec<u8> = (0..280 * 280u32)
            .map(|_| {
                if splitmix(&mut state) % 100 < 5 {
                    13
                } else {
                    10
                }
            })
            .collect();
        let frame = CameraFrame {
            timestamp_us: 0,
            width: 280,
            height: 280,
            bit_depth: 8,
            pixels,
        };
        let equalized = CameraFrame {
            pixels: Clahe::default().apply(&frame.pixels, 280, 280),
            ..frame.clone()
        };

        assert!(face_bbox(&frame, FACE_K).is_none(), "raw: nothing is lit");
        let phantom = face_bbox(&equalized, FACE_K).expect("equalised noise clears the guard");
        assert_eq!(
            (phantom.width(), phantom.height()),
            (280, 280),
            "the phantom this guards against is a full-frame box"
        );
        assert!(preprocess(&frame, 64, 1, Normalize::Unit, 0.2).is_none());
    }

    #[test]
    fn preprocess_hands_the_model_equalised_pixels_unless_told_otherwise() {
        let (frame, _) = synthetic_face(70.0, 90.0, 120.0, 0.5);
        let (equalised, _) = preprocess(&frame, 64, 1, Normalize::Unit, 0.2).unwrap();
        let (raw, _) = preprocess_with(&frame, 64, 1, Normalize::Unit, 0.2, None).unwrap();

        assert_ne!(equalised, raw, "the default must equalise");
        // And opting out reproduces the pre-equalisation behaviour exactly.
        let bbox = face_bbox(&frame, FACE_K).unwrap();
        let pad = ((bbox.width().max(bbox.height()) as f64) * 0.2) as u32;
        let expected = crop_resize(&frame, bbox.padded(pad, 280, 280).squared(280, 280), 64, 64);
        let expected: Vec<f32> = expected.iter().map(|&b| Normalize::Unit.apply(b)).collect();
        assert_eq!(raw, expected);

        let span_of = |t: &[f32]| {
            let (lo, hi) = t
                .iter()
                .fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v)));
            hi - lo
        };
        assert!(
            span_of(&equalised) >= span_of(&raw),
            "the equalised crop should not have less range than the raw one"
        );
    }
}
