//! The `TRACKIRDATA` structure a TrackIR game reads, and the conversion into it.
//!
//! # What is measured and what is not
//!
//! The **layout** is public in NaturalPoint's TrackIR Enhanced SDK and is not in
//! doubt. The **encoding** was measured on 2026-08-15, by feeding a known frame
//! through our own provider and reading it back through a separately-installed
//! NPClient DLL that games are known to accept — a black-box comparison of
//! outputs for the same input, not a reading of anyone's source:
//!
//! | input | reference output | implies |
//! |---|---|---|
//! | roll 5° | 455.1 | [`ROTATION_SCALE`] = `16384/180` |
//! | x 12.5 mm | 409.6 | [`TRANSLATION_SCALE`] = `32.768` |
//! | y −33 mm | −1081.3 | same |
//! | z 680 mm | 16383 | clamped at [`AXIS_LIMIT`] |
//!
//! That corrected a real error: the translation scale had been `64.0` on the
//! strength of a widely-repeated claim, nearly double the truth, and nothing
//! clamped — so an ordinary 680 mm head distance went out as 43520 against a
//! 16383 ceiling.
//!
//! Still **[UNKNOWN]**: the per-axis **signs**, and whether a game ignores a
//! frame whose `wPFrameSignature` did not change (assumed yes, which is why the
//! counter advances on every fill). Both need a game that will actually consume
//! this DLL — see `docs/wiki/Game-Output.md` on the signature check that
//! currently prevents that.

#![allow(non_snake_case)]

use crate::freetrack::{offset, FT_HEAP_LEN};

/// The structure a TrackIR game passes to `NP_GetData`.
///
/// Public layout from NaturalPoint's TrackIR Enhanced SDK.
#[repr(C)]
#[derive(Default)]
pub struct TrackIrData {
    pub wNPStatus: u16,
    pub wPFrameSignature: u16,
    pub dwNPIOData: u32,
    pub fNPRoll: f32,
    pub fNPPitch: f32,
    pub fNPYaw: f32,
    pub fNPX: f32,
    pub fNPY: f32,
    pub fNPZ: f32,
    pub fNPRawX: f32,
    pub fNPRawY: f32,
    pub fNPRawZ: f32,
    pub fNPDeltaX: f32,
    pub fNPDeltaY: f32,
    pub fNPDeltaZ: f32,
    pub fNPSmoothX: f32,
    pub fNPSmoothY: f32,
    pub fNPSmoothZ: f32,
}

/// Degrees → TrackIR rotation units. **[CONFIRMED] 2026-08-15.**
///
/// Measured by feeding a known frame through our provider and reading it back
/// through a separately-installed NPClient DLL that games are known to accept:
/// 5° of roll came out of both as 455.1, so this scale is the one in use.
pub const ROTATION_SCALE: f32 = 16384.0 / 180.0;

/// Millimetres → TrackIR translation units. **[CONFIRMED] 2026-08-15.**
///
/// Was `64.0` on the strength of a widely-repeated claim, and that was wrong by
/// nearly a factor of two. Same measurement as [`ROTATION_SCALE`]: 12.5 mm read
/// back as 409.6 and −33 mm as −1081.3, both exactly `mm × 32.768`. Equivalently
/// the axis spans ±500 mm over its ±16384 range.
pub const TRANSLATION_SCALE: f32 = 32.768;

/// Largest magnitude any axis may carry.
///
/// TrackIR's fields are a 14-bit range expressed as floats, and the reference
/// client clamps rather than wrapping: at 680 mm of head distance an unclamped
/// `z` reached 43520 against this ceiling, which is not a large value but a
/// meaningless one. Clamping keeps a far-away head pinned at the limit instead
/// of arriving as noise.
pub const AXIS_LIMIT: f32 = 16383.0;

/// Clamp one encoded axis into the representable range.
fn clamp_axis(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(-AXIS_LIMIT, AXIS_LIMIT)
    } else {
        0.0
    }
}

/// `wNPStatus` value meaning the tracker is running.
const NP_STATUS_REMOTE_ACTIVE: u16 = 0;

/// Read a little-endian `f32` out of raw heap bytes.
fn f32_at(buf: &[u8; FT_HEAP_LEN], off: usize) -> f32 {
    f32::from_le_bytes(buf[off..off + 4].try_into().expect("4-byte slice"))
}

/// Convert one `FTHeap` snapshot into `TRACKIRDATA`.
///
/// Kept separate from the mapping so the conversion is testable on the host
/// without any Windows object in sight — which matters, because this is where
/// the scaling and signs live.
pub fn from_ft_heap(raw: &[u8; FT_HEAP_LEN], signature: u16, out: &mut TrackIrData) {
    // FT_SharedMem holds radians and millimetres; TrackIR wants its own units.
    let deg = |rad: f32| rad.to_degrees();
    out.wNPStatus = NP_STATUS_REMOTE_ACTIVE;
    out.wPFrameSignature = signature;
    out.dwNPIOData = 0;

    out.fNPRoll = clamp_axis(deg(f32_at(raw, offset::ROLL)) * ROTATION_SCALE);
    out.fNPPitch = clamp_axis(deg(f32_at(raw, offset::PITCH)) * ROTATION_SCALE);
    out.fNPYaw = clamp_axis(deg(f32_at(raw, offset::YAW)) * ROTATION_SCALE);

    out.fNPX = clamp_axis(f32_at(raw, offset::X) * TRANSLATION_SCALE);
    out.fNPY = clamp_axis(f32_at(raw, offset::Y) * TRANSLATION_SCALE);
    out.fNPZ = clamp_axis(f32_at(raw, offset::Z) * TRANSLATION_SCALE);

    // We have no separate unfiltered or smoothed path, so these mirror the
    // filtered values rather than being left at zero — a game preferring Raw*
    // or Smooth* would otherwise see a permanently motionless head.
    out.fNPRawX = out.fNPX;
    out.fNPRawY = out.fNPY;
    out.fNPRawZ = out.fNPZ;
    out.fNPSmoothX = out.fNPX;
    out.fNPSmoothY = out.fNPY;
    out.fNPSmoothZ = out.fNPZ;
    out.fNPDeltaX = 0.0;
    out.fNPDeltaY = 0.0;
    out.fNPDeltaZ = 0.0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::freetrack::ft_heap_bytes;
    use crate::TrackingFrame;

    fn heap_from(
        yaw_deg: f64,
        pitch_deg: f64,
        roll_deg: f64,
        x: f64,
        y: f64,
        z: f64,
    ) -> [u8; FT_HEAP_LEN] {
        let pose = crate::TrackingFrame::from_pose(
            0,
            tobii_headpose::HeadPose {
                yaw_deg,
                pitch_deg,
                roll_deg,
                x_mm: x,
                y_mm: y,
                z_mm: z,
            },
        );
        let _ = TrackingFrame::default();
        ft_heap_bytes(&pose, 1, 0)
    }

    /// The structure games read must be exactly the published size, or every
    /// field lands at the wrong offset and the view moves nonsensically with
    /// nothing to point at the cause.
    #[test]
    fn the_structure_is_the_published_size() {
        // 2 + 2 + 4 header, then 15 f32.
        assert_eq!(std::mem::size_of::<TrackIrData>(), 8 + 15 * 4);
    }

    /// The mapping stores radians; TrackIR wants scaled degrees. Skipping the
    /// radians-to-degrees step would be a factor of 57 — the view would peg on
    /// the first head turn.
    #[test]
    fn rotations_go_from_radians_through_degrees_to_trackir_units() {
        let raw = heap_from(90.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let mut out = TrackIrData::default();
        from_ft_heap(&raw, 1, &mut out);
        let expected = 90.0 * ROTATION_SCALE;
        assert!(
            (out.fNPYaw - expected).abs() < 1.0,
            "90 degrees should be {expected}, got {}",
            out.fNPYaw
        );
    }

    #[test]
    fn translations_are_scaled_from_millimetres() {
        let raw = heap_from(0.0, 0.0, 0.0, 10.0, -20.0, 100.0);
        let mut out = TrackIrData::default();
        from_ft_heap(&raw, 1, &mut out);
        assert!((out.fNPX - 10.0 * TRANSLATION_SCALE).abs() < 0.01);
        assert!((out.fNPY - -20.0 * TRANSLATION_SCALE).abs() < 0.01);
        assert!((out.fNPZ - 100.0 * TRANSLATION_SCALE).abs() < 0.1);
    }

    /// The exact values measured against a reference client, which is where
    /// these two constants come from. If either scale drifts, this fails with
    /// the observation that set it.
    #[test]
    fn the_measured_reference_values_reproduce() {
        let raw = heap_from(0.0, 0.0, 5.0, 12.5, -33.0, 0.0);
        let mut out = TrackIrData::default();
        from_ft_heap(&raw, 1, &mut out);
        assert!(
            (out.fNPRoll - 455.1).abs() < 0.1,
            "roll 5deg -> {}",
            out.fNPRoll
        );
        assert!((out.fNPX - 409.6).abs() < 0.1, "12.5mm -> {}", out.fNPX);
        assert!((out.fNPY - -1081.3).abs() < 0.1, "-33mm -> {}", out.fNPY);
    }

    /// A head at a normal viewing distance overruns the range: 680 mm scales to
    /// 22282, well past the ceiling. Clamping keeps that pinned at the limit
    /// rather than delivering a meaningless number.
    #[test]
    fn an_out_of_range_axis_is_clamped_rather_than_sent_raw() {
        let raw = heap_from(0.0, 0.0, 0.0, 0.0, 0.0, 680.0);
        let mut out = TrackIrData::default();
        from_ft_heap(&raw, 1, &mut out);
        assert_eq!(out.fNPZ, AXIS_LIMIT);

        let raw = heap_from(0.0, 0.0, 0.0, 0.0, 0.0, -680.0);
        let mut out = TrackIrData::default();
        from_ft_heap(&raw, 1, &mut out);
        assert_eq!(out.fNPZ, -AXIS_LIMIT);
    }

    /// Rotations can overrun too — a full turn is 32768 units — and must clamp
    /// on the same ceiling rather than wrapping into the opposite direction.
    #[test]
    fn an_out_of_range_rotation_clamps_instead_of_wrapping() {
        let raw = heap_from(200.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let mut out = TrackIrData::default();
        from_ft_heap(&raw, 1, &mut out);
        assert_eq!(out.fNPYaw, AXIS_LIMIT, "must not wrap to a negative angle");
    }

    /// Each rotation must land in its own field. Roll and yaw are adjacent in
    /// this struct but not in the mapping, so a transposition here is easy and
    /// would show up only as the view tilting when you turn your head.
    #[test]
    fn each_axis_lands_in_its_own_field() {
        let mut out = TrackIrData::default();
        from_ft_heap(&heap_from(30.0, 0.0, 0.0, 0.0, 0.0, 0.0), 1, &mut out);
        assert!(out.fNPYaw.abs() > 1.0, "yaw should move");
        assert!(out.fNPPitch.abs() < 1.0, "pitch should not");
        assert!(out.fNPRoll.abs() < 1.0, "roll should not");

        let mut out = TrackIrData::default();
        from_ft_heap(&heap_from(0.0, 0.0, 30.0, 0.0, 0.0, 0.0), 1, &mut out);
        assert!(out.fNPRoll.abs() > 1.0, "roll should move");
        assert!(out.fNPYaw.abs() < 1.0, "yaw should not");
    }

    /// A game may ignore a frame whose signature did not change, so a repeated
    /// value would read as "the tracker stopped" while data kept arriving.
    #[test]
    fn the_frame_signature_is_carried_through_verbatim() {
        let raw = heap_from(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for sig in [0u16, 1, 40_000, u16::MAX] {
            let mut out = TrackIrData::default();
            from_ft_heap(&raw, sig, &mut out);
            assert_eq!(out.wPFrameSignature, sig);
        }
    }

    /// A game preferring Raw* or Smooth* must not see a motionless head.
    #[test]
    fn raw_and_smooth_mirror_the_filtered_translations() {
        let raw = heap_from(0.0, 0.0, 0.0, 1.0, 2.0, 3.0);
        let mut out = TrackIrData::default();
        from_ft_heap(&raw, 1, &mut out);
        assert_eq!(
            [out.fNPRawX, out.fNPRawY, out.fNPRawZ],
            [out.fNPX, out.fNPY, out.fNPZ]
        );
        assert_eq!(
            [out.fNPSmoothX, out.fNPSmoothY, out.fNPSmoothZ],
            [out.fNPX, out.fNPY, out.fNPZ]
        );
    }
}
