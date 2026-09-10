//! `freetrackclient64.dll` — the FreeTrack side of the client interface.
//!
//! Built as a `cdylib` so it is a **real PE**: a game resolves its client DLL
//! from a registry-supplied path and calls `LoadLibrary` on it, which Wine's
//! builtin `.dll.so` name resolution does not cover.
//!
//! A pure consumer: it opens the mapping the provider exe created and copies out
//! whatever is there. If no provider is running it reports no data and returns
//! cleanly, because a game started before the bridge is ordinary, not an error.
//!
//! The export set matches what the published protocol defines and what
//! opentrack's own build exports, established by reading its export table with
//! `winedump -j export` — an interface fact, not implementation.

use std::sync::OnceLock;

use tobii_output::freetrack::{offset, FT_DATA_LEN, FT_HEAP_LEN};

use tobii_bridge_core::shm::Consumer;

/// The `FTData` structure a game passes in for us to fill.
///
/// Field order and offsets are the published FreeTrack layout; see
/// `tobii_output::freetrack`, which owns and tests the byte layout.
#[repr(C)]
pub struct FtData {
    pub data_id: u32,
    pub cam_width: i32,
    pub cam_height: i32,
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub raw_yaw: f32,
    pub raw_pitch: f32,
    pub raw_roll: f32,
    pub raw_x: f32,
    pub raw_y: f32,
    pub raw_z: f32,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub x3: f32,
    pub y3: f32,
    pub x4: f32,
    pub y4: f32,
}

// The layout this crate writes and the struct games read must agree, or every
// field would be off by some amount nobody could see from in-game symptoms.
const _: () = assert!(std::mem::size_of::<FtData>() == FT_DATA_LEN);

/// The provider's mapping, opened once and reused.
///
/// A game polls `FTGetData` every frame, so re-opening the mapping each call
/// would be a syscall per frame for no benefit.
static SHM: OnceLock<Option<Consumer>> = OnceLock::new();

fn shm() -> Option<&'static Consumer> {
    SHM.get_or_init(Consumer::open).as_ref()
}

/// Read a little-endian `f32` out of the raw heap bytes.
fn f32_at(buf: &[u8; FT_HEAP_LEN], off: usize) -> f32 {
    f32::from_le_bytes(buf[off..off + 4].try_into().expect("4-byte slice"))
}

/// Fill `data` from the shared mapping. Returns false when no data is available.
///
/// # Safety
/// `data` must point to a writable `FTData`. Called by the game through the
/// published FreeTrack ABI.
#[no_mangle]
pub unsafe extern "system" fn FTGetData(data: *mut FtData) -> bool {
    if data.is_null() {
        return false;
    }
    let Some(shm) = shm() else {
        return false;
    };
    let raw = shm.read();
    let out = &mut *data;
    out.data_id = u32::from_le_bytes(raw[0..4].try_into().expect("4-byte slice"));
    out.cam_width = i32::from_le_bytes(raw[4..8].try_into().expect("4-byte slice"));
    out.cam_height = i32::from_le_bytes(raw[8..12].try_into().expect("4-byte slice"));
    out.yaw = f32_at(&raw, offset::YAW);
    out.pitch = f32_at(&raw, offset::PITCH);
    out.roll = f32_at(&raw, offset::ROLL);
    out.x = f32_at(&raw, offset::X);
    out.y = f32_at(&raw, offset::Y);
    out.z = f32_at(&raw, offset::Z);
    out.raw_yaw = f32_at(&raw, offset::RAW_YAW);
    out.raw_pitch = f32_at(&raw, offset::RAW_PITCH);
    out.raw_roll = f32_at(&raw, offset::RAW_ROLL);
    out.raw_x = f32_at(&raw, offset::RAW_X);
    out.raw_y = f32_at(&raw, offset::RAW_Y);
    out.raw_z = f32_at(&raw, offset::RAW_Z);
    true
}

/// Version string, in the shape the protocol expects.
///
/// # Safety
/// The returned pointer is a `'static` NUL-terminated string; the caller must
/// not free it.
#[no_mangle]
pub unsafe extern "system" fn FTGetDllVersion() -> *const u8 {
    b"1.0.0.0\0".as_ptr()
}

/// Who is providing the data.
///
/// # Safety
/// As [`FTGetDllVersion`].
#[no_mangle]
pub unsafe extern "system" fn FTProvider() -> *const u8 {
    b"TobiiLinux\0".as_ptr()
}

/// A game announcing its profile id. Accepted and ignored.
///
/// The provider owns `GameID` in the mapping; a consumer writing it would be
/// the second writer to a structure with one owner.
///
/// # Safety
/// Called by the game through the published FreeTrack ABI.
#[no_mangle]
pub unsafe extern "system" fn FTReportID(_id: i32) {}

/// A game announcing its name. Accepted and ignored.
///
/// # Safety
/// `name` may be null or any NUL-terminated string; it is not dereferenced.
#[no_mangle]
pub unsafe extern "system" fn FTReportName(_name: *const u8) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The struct games read and the layout this project writes must agree.
    /// A mismatch would put every field at the wrong offset, which from inside
    /// a game looks like "head tracking is nonsense" and nothing more specific.
    #[test]
    fn the_struct_matches_the_published_layout() {
        assert_eq!(std::mem::size_of::<FtData>(), FT_DATA_LEN);
        assert_eq!(std::mem::size_of::<FtData>(), 92);
    }
}
