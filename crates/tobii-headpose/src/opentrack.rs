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
/// **[CONFIRMED] `0.1` — opentrack's translation unit is centimetres.** This
/// was previously `1.0` with a note that it could not be confirmed from the
/// documentation. It is not in the documentation; it is in the source, and two
/// independent trackers agree:
///
/// ```text
/// tracker-neuralnet/ftnoir_tracker_neuralnet.cpp:765
///     // convert to cm
///     data[TX] = -tmp.t[2] * 0.1;
/// tracker-pt/ftnoir_tracker_pt.cpp:191
///     // convert to cm
///     data[TX] = (double)t[0] / 10;
/// ```
///
/// Both hold their own pose in millimetres and divide by ten on the way into
/// opentrack's `data[]`. The "UDP over network" input writes the datagram's
/// values into that same array with no scaling of its own
/// (`tracker-udp/ftnoir_tracker_udp.cpp:86`), so a datagram carrying
/// millimetres reads as ten times the real motion.
///
/// This is the single place the unit conversion happens. If in-game motion is
/// the wrong magnitude, the other knob is opentrack's per-axis mapping curves.
pub const TRANSLATION_SCALE: f64 = 0.1;

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
    // `as_chunks_mut` gives `&mut [u8; 8]` slots, so each field is written into
    // a fixed-size array rather than a slice that only happens to be 8 long.
    let (slots, rest) = out.as_chunks_mut::<8>();
    debug_assert!(rest.is_empty(), "the datagram is a whole number of doubles");
    for (slot, value) in slots.iter_mut().zip(values) {
        *slot = value.to_le_bytes();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// opentrack takes centimetres. Pinned as a value, not just as an
    /// expression, so that "simplifying" it back to a pass-through is a test
    /// failure rather than a silent tenfold error in every game.
    #[test]
    fn positions_go_on_the_wire_in_centimetres() {
        assert_eq!(TRANSLATION_SCALE, 0.1);
        let p = HeadPose {
            x_mm: 100.0,
            ..Default::default()
        };
        let got = decode(&to_opentrack_datagram(&p));
        assert_eq!(got[0], 10.0, "100 mm is 10 cm");
    }

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
        /// A field name, a setter that writes 1.0 into it, the datagram slot
        /// that field is expected to land in, and what should be there —
        /// positions are scaled to centimetres, rotations pass through.
        type Probe = (&'static str, fn(&mut HeadPose), usize, f64);

        let probes: [Probe; 6] = [
            ("x", |p| p.x_mm = 1.0, 0, TRANSLATION_SCALE),
            ("y", |p| p.y_mm = 1.0, 1, TRANSLATION_SCALE),
            ("z", |p| p.z_mm = 1.0, 2, TRANSLATION_SCALE),
            ("yaw", |p| p.yaw_deg = 1.0, 3, 1.0),
            ("pitch", |p| p.pitch_deg = 1.0, 4, 1.0),
            ("roll", |p| p.roll_deg = 1.0, 5, 1.0),
        ];
        for (name, set, index, want) in probes {
            let mut p = HeadPose::default();
            set(&mut p);
            let got = decode(&to_opentrack_datagram(&p));
            for (i, v) in got.iter().enumerate() {
                let expected = if i == index { want } else { 0.0 };
                assert_eq!(*v, expected, "setting {name} wrote slot {i} as {v}");
            }
        }
    }

    #[test]
    fn byte_layout_is_little_endian() {
        // 1.0f64 is 0x3FF0000000000000; little-endian that is 7 zero bytes
        // then 0xF0 0x3F. 10 mm is the 1.0 cm that lands on the wire.
        let p = HeadPose {
            x_mm: 10.0,
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
        assert_eq!(got[0], p.x_mm * TRANSLATION_SCALE);
        assert_eq!(got[1], p.y_mm * TRANSLATION_SCALE);
        assert_eq!(got[2], p.z_mm * TRANSLATION_SCALE);
        assert_eq!(got[3], p.yaw_deg);
        assert_eq!(got[4], p.pitch_deg);
        assert_eq!(got[5], p.roll_deg);
    }
}
