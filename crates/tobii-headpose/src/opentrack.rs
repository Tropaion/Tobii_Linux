//! opentrack "UDP over network" wire format.
//!
//! opentrack's UDP input expects a bare datagram of six little-endian `f64`,
//! with no header, framing or checksum, in the order:
//!
//! ```text
//! x, y, z, yaw, pitch, roll
//! ```
//!
//! Translations occupy the first three slots and rotations (in degrees) the
//! last three, giving a fixed 48-byte payload.

use crate::HeadPose;

/// Number of bytes in an opentrack datagram: 6 × `f64`.
pub const DATAGRAM_LEN: usize = 48;

/// The port opentrack's "UDP over network" input listens on by default.
pub const DEFAULT_PORT: u16 = 4242;

/// Multiplier applied to the millimetre positions before they go on the wire.
///
/// **UNVERIFIED — check this first if in-game motion is the wrong magnitude.**
///
/// opentrack's internal translation unit is not obviously millimetres, and it
/// could not be confirmed from the available documentation. The default of
/// `1.0` sends raw millimetres, on the reasoning that a wrong scale is trivial
/// to spot and correct once the pipeline is running end to end, whereas a
/// speculative conversion baked in here would be invisible.
///
/// If translation feels far too large or too small in game, the two knobs are
/// this constant and opentrack's own per-axis mapping curves — prefer fixing it
/// here so the wire format stays honest about what it is sending. This is the
/// single place the unit conversion happens.
pub const TRANSLATION_SCALE: f64 = 1.0;

/// Encode a pose as an opentrack UDP datagram.
///
/// Positions are scaled by [`TRANSLATION_SCALE`]; rotations are passed through
/// in degrees. Note that `pitch` is always `0.0` — the ET5's two eye origins
/// cannot express it (see the [crate docs](crate)).
pub fn to_opentrack_datagram(p: &HeadPose) -> [u8; DATAGRAM_LEN] {
    let values = [
        p.x_mm * TRANSLATION_SCALE,
        p.y_mm * TRANSLATION_SCALE,
        p.z_mm * TRANSLATION_SCALE,
        p.yaw_deg,
        p.pitch_deg,
        p.roll_deg,
    ];
    let mut out = [0u8; DATAGRAM_LEN];
    for (slot, value) in out.chunks_exact_mut(8).zip(values) {
        slot.copy_from_slice(&value.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read the six doubles back out of a datagram, little-endian.
    fn decode(buf: &[u8; DATAGRAM_LEN]) -> [f64; 6] {
        let mut out = [0.0; 6];
        for (i, slot) in out.iter_mut().enumerate() {
            let bytes: [u8; 8] = buf[i * 8..i * 8 + 8].try_into().expect("8-byte slice");
            *slot = f64::from_le_bytes(bytes);
        }
        out
    }

    fn sample_pose() -> HeadPose {
        HeadPose {
            x_mm: -12.5,
            y_mm: 33.25,
            z_mm: 681.0,
            yaw_deg: -7.5,
            pitch_deg: 0.0,
            roll_deg: 4.25,
        }
    }

    #[test]
    fn datagram_is_exactly_48_bytes() {
        assert_eq!(DATAGRAM_LEN, 48);
        assert_eq!(to_opentrack_datagram(&sample_pose()).len(), 48);
    }

    #[test]
    fn fields_round_trip_in_the_documented_order() {
        let p = sample_pose();
        let got = decode(&to_opentrack_datagram(&p));
        assert_eq!(got[0], p.x_mm * TRANSLATION_SCALE, "slot 0 must be x");
        assert_eq!(got[1], p.y_mm * TRANSLATION_SCALE, "slot 1 must be y");
        assert_eq!(got[2], p.z_mm * TRANSLATION_SCALE, "slot 2 must be z");
        assert_eq!(got[3], p.yaw_deg, "slot 3 must be yaw");
        assert_eq!(got[4], p.pitch_deg, "slot 4 must be pitch");
        assert_eq!(got[5], p.roll_deg, "slot 5 must be roll");
    }

    /// Each slot must be independent — a shared or aliased write would let one
    /// field leak into another, which round-tripping a single pose can hide.
    #[test]
    fn each_slot_carries_only_its_own_field() {
        /// A field name, a setter that writes 1.0 into it, and the datagram
        /// slot that field is expected to land in.
        type Probe = (&'static str, fn(&mut HeadPose), usize);

        let probes: [Probe; 6] = [
            ("x", |p| p.x_mm = 1.0, 0),
            ("y", |p| p.y_mm = 1.0, 1),
            ("z", |p| p.z_mm = 1.0, 2),
            ("yaw", |p| p.yaw_deg = 1.0, 3),
            ("pitch", |p| p.pitch_deg = 1.0, 4),
            ("roll", |p| p.roll_deg = 1.0, 5),
        ];
        for (name, set, index) in probes {
            let mut p = HeadPose::default();
            set(&mut p);
            let got = decode(&to_opentrack_datagram(&p));
            for (i, v) in got.iter().enumerate() {
                let expected = if i == index { 1.0 } else { 0.0 };
                assert_eq!(*v, expected, "setting {name} wrote slot {i} as {v}");
            }
        }
    }

    #[test]
    fn byte_layout_is_little_endian() {
        // 1.0f64 is 0x3FF0000000000000; little-endian that is 7 zero bytes
        // then 0xF0 0x3F.
        let p = HeadPose {
            x_mm: 1.0,
            ..HeadPose::default()
        };
        let buf = to_opentrack_datagram(&p);
        assert_eq!(
            &buf[0..8],
            &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x3f]
        );
        assert!(buf[8..].iter().all(|&b| b == 0), "unset slots must be zero");
    }

    #[test]
    fn a_zero_pose_encodes_as_all_zero_bytes() {
        assert_eq!(
            to_opentrack_datagram(&HeadPose::default()),
            [0u8; DATAGRAM_LEN]
        );
    }

    #[test]
    fn negative_and_fractional_values_survive_the_round_trip() {
        let p = HeadPose {
            x_mm: -0.000_123_45,
            y_mm: 1e6,
            z_mm: -1e-9,
            yaw_deg: 179.999_999,
            pitch_deg: -0.5,
            roll_deg: f64::MIN_POSITIVE,
        };
        let got = decode(&to_opentrack_datagram(&p));
        assert_eq!(got[0], p.x_mm);
        assert_eq!(got[1], p.y_mm);
        assert_eq!(got[2], p.z_mm);
        assert_eq!(got[3], p.yaw_deg);
        assert_eq!(got[4], p.pitch_deg);
        assert_eq!(got[5], p.roll_deg);
    }
}
