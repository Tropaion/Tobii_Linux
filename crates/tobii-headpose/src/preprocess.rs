//! Shared image preprocessing for the neural head-pose backends: locate the face
//! in the wide-angle NIR frame, crop it, resize to the model's input size, and
//! normalize to a float tensor.
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

/// Locate the face as the bounding box of the bright region. Pixels brighter
/// than `mean + k*stddev` are taken as face; the box spans them. Returns `None`
/// if the frame is essentially uniform (no eyes / nothing lit). `k ≈ 1.5` is a
/// sensible default for the ET5's high-contrast NIR frames.
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

/// Full preprocessing: locate + square-crop the face, resize to `size`×`size`,
/// and normalize to an `f32` tensor. Returns the tensor in CHW order with a
/// single channel (`1×size×size`), plus the crop box used (for mapping results
/// back). Grayscale is replicated to `channels` if a model expects 3 (RGB).
pub fn preprocess(
    frame: &CameraFrame,
    size: u32,
    channels: usize,
    norm: Normalize,
    pad_frac: f64,
) -> Option<(Vec<f32>, BBox)> {
    let raw = face_bbox(frame, 1.5)?;
    let pad = ((raw.width().max(raw.height()) as f64) * pad_frac) as u32;
    let bbox = raw
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
}
