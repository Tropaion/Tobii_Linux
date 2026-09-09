//! The canonical tracking frame and its 80-byte wire form.
//!
//! One encoding, two transports: the datagram the Wine bridge receives and the
//! `Pose` message the daemon publishes over its Unix socket are byte-identical.
//! That is the point — a single encoder with a single test suite, rather than
//! two formats that drift and disagree about which way pitch points.
//!
//! # Layout
//!
//! ```text
//! off  len  field
//!   0    4  magic       b"TBG1"
//!   4    2  version     u16 LE (currently 1)
//!   6    1  presence    u8    0 none, 1 one eye, 2 both eyes
//!   7    1  flags       u8    bit0 pose_valid, bit1 gaze_valid
//!   8    8  timestamp   i64 LE, microseconds, device clock
//!  16    8  yaw_deg     f64 LE
//!  24    8  pitch_deg   f64 LE
//!  32    8  roll_deg    f64 LE
//!  40    8  x_mm        f64 LE
//!  48    8  y_mm        f64 LE
//!  56    8  z_mm        f64 LE
//!  64    8  gaze_x      f64 LE, normalized display coords
//!  72    8  gaze_y      f64 LE, normalized, +y DOWN
//!  = 80
//! ```
//!
//! Unlike opentrack's bare six doubles, this one is self-describing: it carries
//! a magic and a version because it crosses a Wine boundary where a mismatched
//! build is a real possibility, and because a receiver silently misreading a
//! stale layout would present as "head tracking is subtly wrong" rather than as
//! an error. Rotations stay in **degrees** and positions in **millimetres** —
//! the units the rest of this codebase already speaks — so no conversion is
//! buried in the transport.
//!
//! **`gaze_y` points down**, matching the device's normalized display coords
//! (`overlay.rs` draws `gy * height` directly). Tracker space is +y *up*. The
//! frame keeps the device convention and leaves the flip to whoever converts
//! gaze into an angle.

use tobii_headpose::HeadPose;

use crate::Presence;

/// Total size of an encoded frame.
pub const FRAME_LEN: usize = 80;

/// Leading magic, so a receiver can reject anything that is not ours.
pub const MAGIC: [u8; 4] = *b"TBG1";

/// Current wire version.
pub const VERSION: u16 = 1;

/// `flags` bit: the pose fields are meaningful.
const FLAG_POSE_VALID: u8 = 1 << 0;
/// `flags` bit: the gaze fields are meaningful.
const FLAG_GAZE_VALID: u8 = 1 << 1;

/// One moment of tracking: where the head is, where the eyes are looking, and
/// whether anybody is there at all.
///
/// `pose` and `gaze` are independent options because they fail independently:
/// the geometric pose needs both eyes tracked, while gaze can be valid with a
/// single eye, and a game may want presence even when neither is available.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TrackingFrame {
    /// Device timestamp in microseconds.
    pub timestamp_us: i64,
    /// Head pose, or `None` while tracking is lost.
    pub pose: Option<HeadPose>,
    /// Gaze point in normalized display coordinates, `+y` down.
    pub gaze: Option<[f64; 2]>,
    /// Whether the tracker can see the user.
    pub presence: Presence,
}

/// Why a buffer could not be decoded as a [`TrackingFrame`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Fewer than [`FRAME_LEN`] bytes.
    TooShort(usize),
    /// The leading magic did not match [`MAGIC`].
    BadMagic([u8; 4]),
    /// A version this build does not understand.
    UnsupportedVersion(u16),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::TooShort(n) => {
                write!(f, "tracking frame is {n} bytes, need {FRAME_LEN}")
            }
            DecodeError::BadMagic(m) => write!(f, "not a tracking frame (magic {m:02x?})"),
            DecodeError::UnsupportedVersion(v) => {
                write!(f, "tracking frame version {v} is newer than this build (v{VERSION}) — rebuild the Wine bridge")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Write a little-endian `f64` at `off`.
fn put_f64(buf: &mut [u8; FRAME_LEN], off: usize, v: f64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

/// Read a little-endian `f64` at `off`.
fn get_f64(buf: &[u8], off: usize) -> f64 {
    f64::from_le_bytes(buf[off..off + 8].try_into().expect("8-byte slice"))
}

impl TrackingFrame {
    /// Encode to the 80-byte wire form.
    ///
    /// Invalid pose/gaze are written as zeros with their flag clear, rather
    /// than as NaN: a receiver that ignored the flags would then hold still
    /// instead of propagating NaN through a game's camera maths.
    pub fn encode(&self) -> [u8; FRAME_LEN] {
        let mut buf = [0u8; FRAME_LEN];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4..6].copy_from_slice(&VERSION.to_le_bytes());
        buf[6] = match self.presence {
            Presence::None => 0,
            Presence::OneEye => 1,
            Presence::BothEyes => 2,
        };
        let mut flags = 0u8;
        if self.pose.is_some() {
            flags |= FLAG_POSE_VALID;
        }
        if self.gaze.is_some() {
            flags |= FLAG_GAZE_VALID;
        }
        buf[7] = flags;
        buf[8..16].copy_from_slice(&self.timestamp_us.to_le_bytes());

        let p = self.pose.unwrap_or_default();
        put_f64(&mut buf, 16, p.yaw_deg);
        put_f64(&mut buf, 24, p.pitch_deg);
        put_f64(&mut buf, 32, p.roll_deg);
        put_f64(&mut buf, 40, p.x_mm);
        put_f64(&mut buf, 48, p.y_mm);
        put_f64(&mut buf, 56, p.z_mm);

        let g = self.gaze.unwrap_or([0.0, 0.0]);
        put_f64(&mut buf, 64, g[0]);
        put_f64(&mut buf, 72, g[1]);
        buf
    }

    /// Decode from the wire form.
    ///
    /// A **newer** version is rejected rather than best-effort parsed: this
    /// frame crosses a Wine boundary, and half-reading a layout we do not know
    /// would produce a plausible-looking wrong pose instead of a clear error.
    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < FRAME_LEN {
            return Err(DecodeError::TooShort(buf.len()));
        }
        let magic: [u8; 4] = buf[0..4].try_into().expect("4-byte magic");
        if magic != MAGIC {
            return Err(DecodeError::BadMagic(magic));
        }
        let version = u16::from_le_bytes(buf[4..6].try_into().expect("2-byte version"));
        if version > VERSION {
            return Err(DecodeError::UnsupportedVersion(version));
        }
        let presence = match buf[6] {
            2 => Presence::BothEyes,
            1 => Presence::OneEye,
            // Any unknown code reads as "not present": the safe direction, since
            // it only ever costs a presence-driven feature a false negative.
            _ => Presence::None,
        };
        let flags = buf[7];
        let timestamp_us = i64::from_le_bytes(buf[8..16].try_into().expect("8-byte timestamp"));

        let pose = (flags & FLAG_POSE_VALID != 0).then(|| HeadPose {
            yaw_deg: get_f64(buf, 16),
            pitch_deg: get_f64(buf, 24),
            roll_deg: get_f64(buf, 32),
            x_mm: get_f64(buf, 40),
            y_mm: get_f64(buf, 48),
            z_mm: get_f64(buf, 56),
        });
        let gaze = (flags & FLAG_GAZE_VALID != 0).then(|| [get_f64(buf, 64), get_f64(buf, 72)]);

        Ok(TrackingFrame {
            timestamp_us,
            pose,
            gaze,
            presence,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pose() -> HeadPose {
        HeadPose {
            x_mm: -12.5,
            y_mm: 33.25,
            z_mm: 681.0,
            yaw_deg: -7.5,
            pitch_deg: 3.25,
            roll_deg: 4.25,
        }
    }

    fn full() -> TrackingFrame {
        TrackingFrame {
            timestamp_us: 1_234_567_890,
            pose: Some(sample_pose()),
            gaze: Some([0.25, 0.75]),
            presence: Presence::BothEyes,
        }
    }

    #[test]
    fn a_frame_is_exactly_eighty_bytes() {
        assert_eq!(FRAME_LEN, 80);
        assert_eq!(full().encode().len(), 80);
    }

    #[test]
    fn a_full_frame_round_trips() {
        assert_eq!(TrackingFrame::decode(&full().encode()), Ok(full()));
    }

    /// Field-by-field, one at a time. A shared or aliased write would let one
    /// field leak into another, which round-tripping a single frame can hide —
    /// and a yaw that quietly carries pitch's value is invisible until the view
    /// moves the wrong way in game. Mirrors `opentrack.rs`'s probe table.
    #[test]
    fn each_field_occupies_only_its_own_slot() {
        type Probe = (&'static str, fn(&mut TrackingFrame), usize);
        let probes: [Probe; 8] = [
            ("yaw", |f| set_pose(f, |p| p.yaw_deg = 1.0), 16),
            ("pitch", |f| set_pose(f, |p| p.pitch_deg = 1.0), 24),
            ("roll", |f| set_pose(f, |p| p.roll_deg = 1.0), 32),
            ("x", |f| set_pose(f, |p| p.x_mm = 1.0), 40),
            ("y", |f| set_pose(f, |p| p.y_mm = 1.0), 48),
            ("z", |f| set_pose(f, |p| p.z_mm = 1.0), 56),
            ("gaze_x", |f| f.gaze = Some([1.0, 0.0]), 64),
            ("gaze_y", |f| f.gaze = Some([0.0, 1.0]), 72),
        ];

        fn set_pose(f: &mut TrackingFrame, edit: impl Fn(&mut HeadPose)) {
            let mut p = HeadPose::default();
            edit(&mut p);
            f.pose = Some(p);
        }

        for (name, apply, want_off) in probes {
            let mut f = TrackingFrame {
                pose: Some(HeadPose::default()),
                gaze: Some([0.0, 0.0]),
                ..TrackingFrame::default()
            };
            apply(&mut f);
            let buf = f.encode();
            for off in (16..FRAME_LEN).step_by(8) {
                let got = get_f64(&buf, off);
                let expect = if off == want_off { 1.0 } else { 0.0 };
                assert_eq!(got, expect, "setting {name} wrote offset {off} as {got}");
            }
        }
    }

    #[test]
    fn the_header_is_magic_version_presence_flags() {
        let buf = full().encode();
        assert_eq!(&buf[0..4], b"TBG1");
        assert_eq!(u16::from_le_bytes([buf[4], buf[5]]), VERSION);
        assert_eq!(buf[6], 2, "both eyes");
        assert_eq!(buf[7], FLAG_POSE_VALID | FLAG_GAZE_VALID);
    }

    #[test]
    fn presence_round_trips_through_its_byte() {
        for p in [Presence::None, Presence::OneEye, Presence::BothEyes] {
            let f = TrackingFrame {
                presence: p,
                ..TrackingFrame::default()
            };
            assert_eq!(
                TrackingFrame::decode(&f.encode()).expect("decode").presence,
                p
            );
        }
    }

    /// Absent is not the same as zero. A consumer must be able to tell "the
    /// head is at the origin" from "we do not know where the head is", or it
    /// will happily point the camera at a pose nobody measured.
    #[test]
    fn absent_pose_and_gaze_survive_as_absent_not_as_zero() {
        let f = TrackingFrame {
            timestamp_us: 99,
            pose: None,
            gaze: None,
            presence: Presence::OneEye,
        };
        let got = TrackingFrame::decode(&f.encode()).expect("decode");
        assert_eq!(got.pose, None);
        assert_eq!(got.gaze, None);
        assert_eq!(got.timestamp_us, 99);

        let zeroed = TrackingFrame {
            pose: Some(HeadPose::default()),
            gaze: Some([0.0, 0.0]),
            ..f
        };
        assert_ne!(
            f.encode(),
            zeroed.encode(),
            "an absent pose must not encode identically to a zero pose"
        );
    }

    /// Invalid fields go out as zeros, not NaN: a receiver that ignores the
    /// flags then holds still instead of poisoning a game's camera maths.
    #[test]
    fn an_absent_pose_writes_zeros_rather_than_nan() {
        let buf = TrackingFrame::default().encode();
        for off in (16..FRAME_LEN).step_by(8) {
            assert_eq!(get_f64(&buf, off), 0.0, "offset {off} must be zero");
        }
    }

    #[test]
    fn negative_timestamps_and_extreme_values_survive() {
        let f = TrackingFrame {
            timestamp_us: i64::MIN,
            pose: Some(HeadPose {
                x_mm: -0.000_123_45,
                y_mm: 1e6,
                z_mm: -1e-9,
                yaw_deg: 179.999_999,
                pitch_deg: -0.5,
                roll_deg: f64::MIN_POSITIVE,
            }),
            gaze: Some([-1.5, 2.5]),
            presence: Presence::OneEye,
        };
        assert_eq!(TrackingFrame::decode(&f.encode()), Ok(f));
    }

    #[test]
    fn a_short_buffer_is_rejected_with_its_length() {
        let buf = full().encode();
        assert_eq!(
            TrackingFrame::decode(&buf[..FRAME_LEN - 1]),
            Err(DecodeError::TooShort(FRAME_LEN - 1))
        );
        assert_eq!(TrackingFrame::decode(&[]), Err(DecodeError::TooShort(0)));
    }

    #[test]
    fn a_foreign_datagram_is_rejected_by_its_magic() {
        let mut buf = full().encode();
        buf[0] = b'X';
        assert!(matches!(
            TrackingFrame::decode(&buf),
            Err(DecodeError::BadMagic(_))
        ));
    }

    /// The bridge runs as a separate binary built by a separate script, so a
    /// stale one is a real possibility. Refusing a newer frame turns that into
    /// a message naming the fix, instead of subtly wrong head tracking.
    #[test]
    fn a_newer_version_is_refused_rather_than_guessed_at() {
        let mut buf = full().encode();
        buf[4..6].copy_from_slice(&(VERSION + 1).to_le_bytes());
        assert_eq!(
            TrackingFrame::decode(&buf),
            Err(DecodeError::UnsupportedVersion(VERSION + 1))
        );
        assert!(TrackingFrame::decode(&buf)
            .unwrap_err()
            .to_string()
            .contains("rebuild"));
    }

    /// Trailing bytes are ignored, so the frame can later be embedded in a
    /// larger message without every reader needing to know its length.
    #[test]
    fn trailing_bytes_are_ignored() {
        let mut buf = full().encode().to_vec();
        buf.extend_from_slice(&[0xAA; 16]);
        assert_eq!(TrackingFrame::decode(&buf), Ok(full()));
    }

    /// An unknown presence code must read as "not present": that only ever
    /// costs a presence-driven feature a false negative, whereas guessing
    /// "present" would keep a game from pausing when the user walks away.
    #[test]
    fn an_unknown_presence_code_reads_as_absent() {
        let mut buf = full().encode();
        buf[6] = 200;
        assert_eq!(
            TrackingFrame::decode(&buf).expect("decode").presence,
            Presence::None
        );
    }
}
