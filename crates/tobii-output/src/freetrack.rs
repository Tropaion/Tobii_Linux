//! The FreeTrack shared-memory layout.
//!
//! This is the ecosystem's native data channel on Windows. A *provider* creates
//! a file mapping named `FT_SharedMem` guarded by a mutex named `FT_Mutext`,
//! and consumer DLLs — `freetrackclient64.dll` for FreeTrack games and
//! `NPClient64.dll` for TrackIR ones — read it. One provider feeds both ABIs,
//! so our Wine-side bridge only has to fill this one structure.
//!
//! `FT_Mutext` **is** the real name. The typo is part of the published
//! protocol and reproducing it exactly is mandatory; a correctly-spelled mutex
//! would simply never be found.
//!
//! This module lives on the Linux side, and is compiled into the Wine-side
//! bridge too, so the byte layout is written and tested once rather than
//! re-implemented in whatever language the bridge happens to be.
//!
//! # Layout
//!
//! ```text
//! FTData  (92 bytes)
//!    0  DataID     u32     bumped on every write; consumers poll it
//!    4  CamWidth   i32
//!    8  CamHeight  i32
//!   12  Yaw        f32     radians
//!   16  Pitch      f32     radians
//!   20  Roll       f32     radians
//!   24  X          f32     millimetres
//!   28  Y          f32     millimetres
//!   32  Z          f32     millimetres
//!   36  RawYaw     f32     } the unfiltered equivalents; we have no separate
//!   40  RawPitch   f32     } raw path, so these mirror the filtered values
//!   44  RawRoll    f32     }
//!   48  RawX       f32     }
//!   52  RawY       f32     }
//!   56  RawZ       f32     }
//!   60  X1 Y1 X2 Y2 X3 Y3 X4 Y4   f32 × 8 — IR blob positions, unused
//!
//! FTHeap  (108 bytes)
//!    0  data       FTData
//!   92  GameID     i32
//!   96  table[8]   u8
//!  104  GameID2    i32
//! ```
//!
//! Note the units differ from everything else in this crate: **radians and
//! millimetres**, where our [`HeadPose`](tobii_headpose::HeadPose) carries
//! degrees. The conversion happens here, in one tested place.

use crate::TrackingFrame;

/// Size of the `FTData` structure.
pub const FT_DATA_LEN: usize = 92;

/// Size of the `FTHeap` structure — what the file mapping holds.
pub const FT_HEAP_LEN: usize = 108;

/// Windows name of the shared file mapping.
pub const FT_SHARED_MEM_NAME: &str = "FT_SharedMem";

/// Windows name of the guarding mutex. The typo is part of the protocol.
pub const FT_MUTEX_NAME: &str = "FT_Mutext";

/// `DataID` wraps here rather than at `u32::MAX`.
pub const DATA_ID_WRAP: u32 = 1 << 29;

/// Byte offsets within `FTHeap`, so the encoder and its tests agree by
/// construction rather than by two people counting the same way.
pub mod offset {
    pub const DATA_ID: usize = 0;
    pub const CAM_WIDTH: usize = 4;
    pub const CAM_HEIGHT: usize = 8;
    pub const YAW: usize = 12;
    pub const PITCH: usize = 16;
    pub const ROLL: usize = 20;
    pub const X: usize = 24;
    pub const Y: usize = 28;
    pub const Z: usize = 32;
    pub const RAW_YAW: usize = 36;
    pub const RAW_PITCH: usize = 40;
    pub const RAW_ROLL: usize = 44;
    pub const RAW_X: usize = 48;
    pub const RAW_Y: usize = 52;
    pub const RAW_Z: usize = 56;
    pub const POINTS: usize = 60;
    pub const GAME_ID: usize = 92;
    pub const TABLE: usize = 96;
    pub const GAME_ID2: usize = 104;
}

/// The next `DataID`, wrapping at [`DATA_ID_WRAP`].
///
/// Consumers detect new data by watching this change, so it must advance on
/// every write and must never repeat consecutively — a counter that stuck would
/// read as "the tracker stopped" even while frames kept arriving.
pub fn next_data_id(prev: u32) -> u32 {
    (prev + 1) % DATA_ID_WRAP
}

/// Narrow to `f32`, mapping anything non-finite to zero.
///
/// A NaN reaching the mapping would be read by a game as a rotation and tends
/// to take the whole view with it; a zero is merely wrong for one frame.
fn f32_of(v: f64) -> f32 {
    let n = v as f32;
    if n.is_finite() {
        n
    } else {
        0.0
    }
}

/// Write a little-endian `f32`.
fn put_f32(buf: &mut [u8; FT_HEAP_LEN], off: usize, v: f32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

/// Encode a frame into the `FTHeap` bytes a provider copies into the mapping.
///
/// Rotations are converted from degrees to radians and positions pass through
/// as millimetres. A frame with no pose encodes as a zero pose: unlike the UDP
/// path, a provider cannot decline to write — the mapping always holds
/// *something* — so callers that want "hold the last value" must simply not
/// call this, which is exactly what [`Router`](crate::Router) arranges by
/// withholding pose-less frames.
pub fn ft_heap_bytes(frame: &TrackingFrame, data_id: u32, game_id: i32) -> [u8; FT_HEAP_LEN] {
    let mut buf = [0u8; FT_HEAP_LEN];
    let p = frame.pose.unwrap_or_default();

    let yaw = f32_of(p.yaw_deg.to_radians());
    let pitch = f32_of(p.pitch_deg.to_radians());
    let roll = f32_of(p.roll_deg.to_radians());
    let (x, y, z) = (f32_of(p.x_mm), f32_of(p.y_mm), f32_of(p.z_mm));

    buf[offset::DATA_ID..offset::DATA_ID + 4].copy_from_slice(&data_id.to_le_bytes());
    put_f32(&mut buf, offset::YAW, yaw);
    put_f32(&mut buf, offset::PITCH, pitch);
    put_f32(&mut buf, offset::ROLL, roll);
    put_f32(&mut buf, offset::X, x);
    put_f32(&mut buf, offset::Y, y);
    put_f32(&mut buf, offset::Z, z);
    // We have no separate unfiltered path, so Raw* mirror the filtered values
    // rather than being left at zero — a consumer preferring Raw* would
    // otherwise see a permanently motionless head.
    put_f32(&mut buf, offset::RAW_YAW, yaw);
    put_f32(&mut buf, offset::RAW_PITCH, pitch);
    put_f32(&mut buf, offset::RAW_ROLL, roll);
    put_f32(&mut buf, offset::RAW_X, x);
    put_f32(&mut buf, offset::RAW_Y, y);
    put_f32(&mut buf, offset::RAW_Z, z);

    buf[offset::GAME_ID..offset::GAME_ID + 4].copy_from_slice(&game_id.to_le_bytes());
    buf[offset::GAME_ID2..offset::GAME_ID2 + 4].copy_from_slice(&game_id.to_le_bytes());
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use tobii_headpose::HeadPose;

    fn get_f32(buf: &[u8; FT_HEAP_LEN], off: usize) -> f32 {
        f32::from_le_bytes(buf[off..off + 4].try_into().expect("4-byte slice"))
    }

    fn frame(p: HeadPose) -> TrackingFrame {
        TrackingFrame::from_pose(0, p)
    }

    #[test]
    fn the_structures_are_the_documented_sizes() {
        assert_eq!(FT_DATA_LEN, 92);
        assert_eq!(FT_HEAP_LEN, 108);
        assert_eq!(offset::GAME_ID, FT_DATA_LEN);
        assert_eq!(ft_heap_bytes(&TrackingFrame::default(), 0, 0).len(), 108);
    }

    /// The mutex name's typo is part of the published protocol. Spelling it
    /// correctly would create a mutex nothing ever looks for.
    #[test]
    fn the_protocol_names_are_verbatim_including_the_typo() {
        assert_eq!(FT_SHARED_MEM_NAME, "FT_SharedMem");
        assert_eq!(FT_MUTEX_NAME, "FT_Mutext");
    }

    /// One field at a time, so a write that bled into a neighbouring offset
    /// cannot hide behind a round trip. A pitch that lands in roll's slot is
    /// invisible until the view tilts when the user nods.
    #[test]
    fn each_field_lands_only_at_its_documented_offset() {
        type Probe = (&'static str, fn(&mut HeadPose), &'static [usize]);
        // Raw* mirror the filtered values, so each pose field owns two offsets.
        let probes: [Probe; 6] = [
            ("yaw", |p| p.yaw_deg = 90.0, &[offset::YAW, offset::RAW_YAW]),
            (
                "pitch",
                |p| p.pitch_deg = 90.0,
                &[offset::PITCH, offset::RAW_PITCH],
            ),
            (
                "roll",
                |p| p.roll_deg = 90.0,
                &[offset::ROLL, offset::RAW_ROLL],
            ),
            ("x", |p| p.x_mm = 5.0, &[offset::X, offset::RAW_X]),
            ("y", |p| p.y_mm = 5.0, &[offset::Y, offset::RAW_Y]),
            ("z", |p| p.z_mm = 5.0, &[offset::Z, offset::RAW_Z]),
        ];
        let all: [usize; 12] = [
            offset::YAW,
            offset::PITCH,
            offset::ROLL,
            offset::X,
            offset::Y,
            offset::Z,
            offset::RAW_YAW,
            offset::RAW_PITCH,
            offset::RAW_ROLL,
            offset::RAW_X,
            offset::RAW_Y,
            offset::RAW_Z,
        ];

        for (name, set, owned) in probes {
            let mut p = HeadPose::default();
            set(&mut p);
            let buf = ft_heap_bytes(&frame(p), 0, 0);
            for off in all {
                let got = get_f32(&buf, off);
                if owned.contains(&off) {
                    assert_ne!(got, 0.0, "{name} should have written offset {off}");
                } else {
                    assert_eq!(got, 0.0, "{name} leaked into offset {off} as {got}");
                }
            }
        }
    }

    /// Everything else in this crate speaks degrees; the mapping speaks
    /// radians. Getting this wrong is a factor of 57 — a head turn would peg
    /// the view instantly.
    #[test]
    fn rotations_are_converted_from_degrees_to_radians() {
        let cases = [
            (0.0_f64, 0.0_f32),
            (90.0, std::f32::consts::FRAC_PI_2),
            (180.0, std::f32::consts::PI),
            (-90.0, -std::f32::consts::FRAC_PI_2),
        ];
        for (deg, want) in cases {
            let buf = ft_heap_bytes(
                &frame(HeadPose {
                    yaw_deg: deg,
                    ..HeadPose::default()
                }),
                0,
                0,
            );
            let got = get_f32(&buf, offset::YAW);
            assert!((got - want).abs() < 1e-6, "{deg}° gave {got}, want {want}");
        }
    }

    /// Positions must NOT be converted — the mapping already wants millimetres,
    /// which is what a head pose carries.
    #[test]
    fn positions_pass_through_as_millimetres() {
        let buf = ft_heap_bytes(
            &frame(HeadPose {
                x_mm: -12.5,
                y_mm: 33.25,
                z_mm: 681.0,
                ..HeadPose::default()
            }),
            0,
            0,
        );
        assert_eq!(get_f32(&buf, offset::X), -12.5);
        assert_eq!(get_f32(&buf, offset::Y), 33.25);
        assert_eq!(get_f32(&buf, offset::Z), 681.0);
    }

    #[test]
    fn raw_fields_mirror_the_filtered_ones() {
        let buf = ft_heap_bytes(
            &frame(HeadPose {
                x_mm: -12.5,
                y_mm: 33.25,
                z_mm: 681.0,
                yaw_deg: -7.5,
                pitch_deg: 3.25,
                roll_deg: 4.25,
            }),
            0,
            0,
        );
        for (filtered, raw) in [
            (offset::YAW, offset::RAW_YAW),
            (offset::PITCH, offset::RAW_PITCH),
            (offset::ROLL, offset::RAW_ROLL),
            (offset::X, offset::RAW_X),
            (offset::Y, offset::RAW_Y),
            (offset::Z, offset::RAW_Z),
        ] {
            assert_eq!(get_f32(&buf, filtered), get_f32(&buf, raw));
        }
    }

    #[test]
    fn the_data_id_and_game_ids_land_where_documented() {
        let buf = ft_heap_bytes(&TrackingFrame::default(), 0xABCD, 0x1234_5678);
        let id = u32::from_le_bytes(buf[0..4].try_into().expect("4"));
        let g1 = i32::from_le_bytes(buf[92..96].try_into().expect("4"));
        let g2 = i32::from_le_bytes(buf[104..108].try_into().expect("4"));
        assert_eq!(id, 0xABCD);
        assert_eq!(g1, 0x1234_5678);
        assert_eq!(g2, 0x1234_5678, "GameID2 mirrors GameID");
        assert_eq!(&buf[96..104], &[0u8; 8], "the table stays zeroed");
    }

    /// A stuck counter reads to a game as "the tracker stopped", even while
    /// frames keep arriving, so it must advance every single time.
    #[test]
    fn the_data_id_advances_every_write_and_wraps_without_repeating() {
        assert_eq!(next_data_id(0), 1);
        assert_eq!(next_data_id(DATA_ID_WRAP - 2), DATA_ID_WRAP - 1);
        assert_eq!(next_data_id(DATA_ID_WRAP - 1), 0, "wraps at 1<<29");

        let mut id = DATA_ID_WRAP - 3;
        for _ in 0..6 {
            let next = next_data_id(id);
            assert_ne!(next, id, "the counter must never repeat consecutively");
            assert!(next < DATA_ID_WRAP);
            id = next;
        }
    }

    /// NaN reaching the mapping is read as a rotation and tends to take the
    /// whole in-game view with it; zero is merely wrong for one frame.
    #[test]
    fn non_finite_and_overflowing_values_narrow_to_something_finite() {
        let buf = ft_heap_bytes(
            &frame(HeadPose {
                x_mm: f64::NAN,
                y_mm: f64::INFINITY,
                z_mm: -f64::INFINITY,
                yaw_deg: 1e300,
                pitch_deg: f64::NAN,
                roll_deg: -1e300,
            }),
            0,
            0,
        );
        for off in [
            offset::YAW,
            offset::PITCH,
            offset::ROLL,
            offset::X,
            offset::Y,
            offset::Z,
        ] {
            let v = get_f32(&buf, off);
            assert!(v.is_finite(), "offset {off} is {v}");
            assert_eq!(v, 0.0, "offset {off} should be zeroed, got {v}");
        }
    }

    #[test]
    fn a_pose_less_frame_encodes_as_a_zero_pose() {
        let buf = ft_heap_bytes(&TrackingFrame::default(), 7, 0);
        for off in [offset::YAW, offset::X, offset::RAW_Z] {
            assert_eq!(get_f32(&buf, off), 0.0);
        }
        assert_eq!(
            u32::from_le_bytes(buf[0..4].try_into().expect("4")),
            7,
            "the counter still advances so a consumer can tell frames apart"
        );
    }
}
