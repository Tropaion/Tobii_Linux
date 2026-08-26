//! The model-agnostic head-pose backend interface.
//!
//! Head pose can come from two kinds of source, unified here so the driver and
//! opentrack output do not care which is in use:
//!
//! * **Geometric** — [`crate::pose_from_sample`], from the two eye origins. No
//!   model, always available, but 5-DOF only (no pitch).
//! * **Neural** — a [`PoseModel`] run on the NIR camera frames
//!   ([`tobii_protocol::CameraFrame`]). Full 6-DOF. This is how Tobii's own
//!   software does it (a host-side OpenVINO model on the camera images).
//!
//! Three neural backends are planned, all behind the [`PoseModel`] trait so they
//! are interchangeable and the rest of the pipeline is unchanged:
//!
//! | [`ModelKind`] | source | licence | notes |
//! |---|---|---|---|
//! | `TobiiVino` | Tobii's `bdtsdata/NN/model.vino.*` | proprietary | **closed** — AES-encrypted, see the variant's own doc |
//! | `OpentrackOnnx` | opentrack's `head-pose-*.onnx` | **not redistributable** | code is free; the *weights* are not — opentrack's `license.md` puts the training data under CC BY-NC 4.0 and \"Research Use of Data\" terms. GPL-3 s10 forbids us passing that restriction on, so it must be fetched by the user with the terms shown, never bundled or silently downloaded |
//! | `SixDRepNet` | 6DRepNet | research-only | RGB 3-channel, no official ONNX export, needs a separate face detector; dominated |
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
    /// Tobii's own OpenVINO model. **This path is closed** and the variant is
    /// kept only so nobody spends a weekend rediscovering why:
    /// `docs/windows-headpose-findings.md` establishes the `.vino` files are
    /// AES-encrypted (all four share the header `e2 65 f2 ab ...`) and decrypted
    /// in memory by `VNN::Crypto::decryptBuffer`. A user-supplied copy from
    /// their own install is ciphertext, so "user-supplied" was never a way in.
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

/// A loaded head-pose model: NIR frames in, 6-DOF pose out.
///
/// `right` is the second camera stream when available. Whether it is a genuinely
/// different view is **[UNCONFIRMED]**: `camera.rs` asserts a stereo pair, but a
/// measurement found `0x50e` byte-identical to `0x501` — on an empty scene,
/// which is not conclusive. `tobii camera both` settles it with a face in view,
/// and if they are the same image this parameter should go. Returns `None` when the model can't produce a pose this frame (no face
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
