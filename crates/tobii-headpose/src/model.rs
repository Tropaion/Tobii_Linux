//! The model-agnostic head-pose backend interface.
//!
//! Head pose can come from two kinds of source, unified here so the driver and
//! opentrack output do not care which is in use:
//!
//! * **Geometric** — [`crate::pose_from_sample`], from the two eye origins. No
//!   model, always available, but 5-DOF only (no pitch).
//! * **Neural** — a [`PoseModel`] run on the stereo NIR camera frames
//!   ([`tobii_protocol::CameraFrame`]). Full 6-DOF. This is how Tobii's own
//!   software does it (a host-side OpenVINO model on the camera images).
//!
//! Three neural backends are planned, all behind the [`PoseModel`] trait so they
//! are interchangeable and the rest of the pipeline is unchanged:
//!
//! | [`ModelKind`] | source | licence | notes |
//! |---|---|---|---|
//! | `TobiiVino` | Tobii's `bdtsdata/NN/model.vino.*`, user-supplied | proprietary | matches the NIR domain exactly; load-only, never shipped |
//! | `OpentrackOnnx` | opentrack's `head-pose-*.onnx` | free | shippable by reference (app can download it) |
//! | `SixDRepNet` | 6DRepNet | free | shippable by reference |
//!
//! Each backend owns its own preprocessing parameters (input size, channel
//! count, [`crate::preprocess::Normalize`], crop padding) and its output mapping
//! to a [`HeadPose`]. The inference engine (OpenVINO or ONNX Runtime) is an
//! implementation detail added with each backend once a model file exists.

use std::path::PathBuf;

use tobii_protocol::CameraFrame;

use crate::HeadPose;

/// Which head-pose model to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    /// Tobii's proprietary OpenVINO model (`bdtsdata/NN/model.vino.xml`+`.bin`),
    /// supplied by the user from their own install. Not redistributable.
    TobiiVino,
    /// opentrack's free ONNX head-pose model.
    OpentrackOnnx,
    /// The 6DRepNet open head-pose model.
    SixDRepNet,
}

impl ModelKind {
    /// Whether this model may be shipped/auto-downloaded (free) vs. must be
    /// user-supplied (proprietary).
    pub fn is_redistributable(self) -> bool {
        !matches!(self, ModelKind::TobiiVino)
    }
}

/// How to load a model: which backend and where its file is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelConfig {
    pub kind: ModelKind,
    /// The model file — `.vino.xml` for OpenVINO, `.onnx` for the ONNX backends.
    /// The `.bin` weights for OpenVINO are found beside the `.xml`.
    pub model_path: PathBuf,
}

/// A loaded head-pose model: stereo NIR frames in, 6-DOF pose out.
///
/// `right` is the second stereo camera when available; monocular backends ignore
/// it. Returns `None` when the model can't produce a pose this frame (no face
/// found, low confidence, inference error) — the caller should hold the previous
/// pose rather than snap to zero.
pub trait PoseModel {
    fn estimate(&mut self, left: &CameraFrame, right: Option<&CameraFrame>) -> Option<HeadPose>;
    fn kind(&self) -> ModelKind;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tobii_protocol::CameraFrame;

    /// A stand-in backend proving the trait is object-safe and the plumbing
    /// (frame in → pose out) works before any real inference engine is wired in.
    struct MockModel {
        pose: HeadPose,
    }
    impl PoseModel for MockModel {
        fn estimate(&mut self, _l: &CameraFrame, _r: Option<&CameraFrame>) -> Option<HeadPose> {
            Some(self.pose)
        }
        fn kind(&self) -> ModelKind {
            ModelKind::OpentrackOnnx
        }
    }

    fn frame() -> CameraFrame {
        CameraFrame {
            timestamp_us: 0,
            width: 4,
            height: 4,
            bit_depth: 8,
            pixels: vec![0; 16],
        }
    }

    #[test]
    fn trait_is_object_safe_and_drives_a_pose() {
        let want = HeadPose {
            pitch_deg: 12.0,
            ..Default::default()
        };
        let mut m: Box<dyn PoseModel> = Box::new(MockModel { pose: want });
        let got = m.estimate(&frame(), None).unwrap();
        assert_eq!(got.pitch_deg, 12.0);
        assert_eq!(m.kind(), ModelKind::OpentrackOnnx);
    }

    #[test]
    fn redistributability_matches_licence() {
        assert!(!ModelKind::TobiiVino.is_redistributable());
        assert!(ModelKind::OpentrackOnnx.is_redistributable());
        assert!(ModelKind::SixDRepNet.is_redistributable());
    }
}
