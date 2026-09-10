//! `NPClient64.dll` — the TrackIR side of the client interface.
//!
//! This is the path Star Citizen takes. [CONFIRMED] by binary inspection:
//! `StarCitizen.exe` contains `NPClient64.dll`, the registry key
//! `Software\NaturalPoint\NATURALPOINT\NPClient Location`,
//! `CNaturalPointInputTrackIR::Update`, and resolves exactly twelve `NP_*`
//! exports. It needs no `TrackIR.exe` process, and never reads FreeTrack.
//!
//! # Two builds
//!
//! Default is the real client, reading from `FT_SharedMem` exactly as the
//! FreeTrack DLL does — one provider, two consumer ABIs.
//!
//! `--features spike-log` builds the **measurement stub** instead: every export
//! logs its arguments and returns `NP_OK` without serving data. That exists
//! because three things about this ABI are unmeasured — the `TRACKIRDATA`
//! scaling, the six axis signs, and whether the game requires
//! `wPFrameSignature` to change for a frame to count — and a wrong guess at any
//! of them is *invisible*: the DLL loads, the game asks for data, and the view
//! simply does not move. The stub turns each of those into an observation.

#![allow(non_snake_case)]

use std::ffi::c_void;

mod trackir;
pub use trackir::TrackIrData;

#[cfg(feature = "spike-log")]
mod log;

/// The protocol's success code.
const NP_OK: i32 = 0;

/// Handle types the game passes in. Never dereferenced here.
type HWND = *mut c_void;

/// Log a call when built as the spike stub; compile to nothing otherwise.
///
/// The non-spike arm still *mentions* the arguments via `format_args!`, which
/// costs nothing at runtime but keeps them counted as used — otherwise every
/// export would warn about its own parameters in the shipping build.
macro_rules! trace {
    ($($arg:tt)*) => {
        #[cfg(feature = "spike-log")]
        crate::log::line(&format!($($arg)*));
        #[cfg(not(feature = "spike-log"))]
        let _ = format_args!($($arg)*);
    };
}

/// Fill `data` with the latest tracking values.
///
/// The one export that carries data, and the one whose encoding is unmeasured.
///
/// # Safety
/// `data` must point to a writable [`TrackIrData`].
#[no_mangle]
pub unsafe extern "system" fn NP_GetData(data: *mut TrackIrData) -> i32 {
    trace!("NP_GetData(data={data:p})");
    #[cfg(not(feature = "spike-log"))]
    {
        if data.is_null() {
            return NP_OK;
        }
        trackir::fill(&mut *data);
    }
    NP_OK
}

/// NaturalPoint's anti-clone check.
///
/// We do **not** ship NaturalPoint's signature blob. Whether Star Citizen gates
/// on it is unknown; the spike build answers that by logging whether the game
/// calls this and whether it stops asking for data afterwards.
///
/// # Safety
/// `sig` must point to writable storage for the signature pair.
#[no_mangle]
pub unsafe extern "system" fn NP_GetSignature(sig: *mut c_void) -> i32 {
    trace!("NP_GetSignature(sig={sig:p})");
    NP_OK
}

/// # Safety
/// `version` must point to a writable `u16`.
#[no_mangle]
pub unsafe extern "system" fn NP_QueryVersion(version: *mut u16) -> i32 {
    trace!("NP_QueryVersion(version={version:p})");
    if !version.is_null() {
        // 4.00, the version TrackIR clients report.
        *version = 0x0400;
    }
    NP_OK
}

/// # Safety
/// Called by the game through the published NPClient ABI.
#[no_mangle]
pub unsafe extern "system" fn NP_RegisterWindowHandle(hwnd: HWND) -> i32 {
    trace!("NP_RegisterWindowHandle(hwnd={hwnd:p})");
    NP_OK
}

/// # Safety
/// As [`NP_RegisterWindowHandle`].
#[no_mangle]
pub unsafe extern "system" fn NP_UnregisterWindowHandle() -> i32 {
    trace!("NP_UnregisterWindowHandle()");
    NP_OK
}

/// The game announcing which profile it wants.
///
/// The id it passes is one of the things the spike measures — it becomes
/// `FTHeap.GameID` so a consumer can tell which title is being served.
///
/// # Safety
/// As [`NP_RegisterWindowHandle`].
#[no_mangle]
pub unsafe extern "system" fn NP_RegisterProgramProfileID(id: u16) -> i32 {
    trace!("NP_RegisterProgramProfileID(id={id})");
    trackir::set_game_id(i32::from(id));
    NP_OK
}

/// # Safety
/// As [`NP_RegisterWindowHandle`].
#[no_mangle]
pub unsafe extern "system" fn NP_RequestData(data: u16) -> i32 {
    trace!("NP_RequestData(data={data:#06x})");
    NP_OK
}

/// # Safety
/// As [`NP_RegisterWindowHandle`].
#[no_mangle]
pub unsafe extern "system" fn NP_StartDataTransmission() -> i32 {
    trace!("NP_StartDataTransmission()");
    NP_OK
}

/// # Safety
/// As [`NP_RegisterWindowHandle`].
#[no_mangle]
pub unsafe extern "system" fn NP_StopDataTransmission() -> i32 {
    trace!("NP_StopDataTransmission()");
    NP_OK
}

/// # Safety
/// As [`NP_RegisterWindowHandle`].
#[no_mangle]
pub unsafe extern "system" fn NP_StartCursor() -> i32 {
    trace!("NP_StartCursor()");
    NP_OK
}

/// # Safety
/// As [`NP_RegisterWindowHandle`].
#[no_mangle]
pub unsafe extern "system" fn NP_StopCursor() -> i32 {
    trace!("NP_StopCursor()");
    NP_OK
}

/// Re-centre. The pose is already referenced to the screen centre upstream, so
/// there is nothing to zero here.
///
/// # Safety
/// As [`NP_RegisterWindowHandle`].
#[no_mangle]
pub unsafe extern "system" fn NP_ReCenter() -> i32 {
    trace!("NP_ReCenter()");
    NP_OK
}

// The twelve above are what Star Citizen resolves. The two below round out the
// set opentrack's build exports; a superset costs nothing and other titles may
// look for them.

/// # Safety
/// As [`NP_RegisterWindowHandle`].
#[no_mangle]
pub unsafe extern "system" fn NP_GetParameter(category: u16, index: u16) -> i32 {
    trace!("NP_GetParameter(category={category}, index={index})");
    NP_OK
}

/// # Safety
/// As [`NP_RegisterWindowHandle`].
#[no_mangle]
pub unsafe extern "system" fn NP_SetParameter(category: u16, index: u16, value: f32) -> i32 {
    trace!("NP_SetParameter(category={category}, index={index}, value={value})");
    NP_OK
}
