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
//! logs its arguments, and none of them serve data. That exists because three
//! things about this ABI are unmeasured — the `TRACKIRDATA` scaling, the six
//! axis signs, and whether the game requires `wPFrameSignature` to change for a
//! frame to count — and a wrong guess at any of them is *invisible*: the DLL
//! loads, the game asks for data, and the view simply does not move. The stub
//! turns each of those into an observation.
//!
//! # What an export may answer
//!
//! Only what is true. An export that returns `NP_OK` has done what was asked;
//! one that could not says which of the ABI's failures applies. A success code
//! over an out-parameter that was never written hands the caller its own
//! uninitialised memory and tells it to trust it, and it leaves the caller
//! nothing to conclude — see [`NP_GetSignature`] for the game that retries for
//! ever because of exactly that.

// Every name here is the ABI's, spelled as the header spells it.
#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;

mod trackir;
pub use trackir::TrackIrData;

#[cfg(feature = "spike-log")]
mod log;

/// The ABI's return codes, from NaturalPoint's `NPClient.h` — the TrackIR
/// Enhanced SDK header that publishes this interface.
///
/// Spelled out in full, the ones nothing here returns included, because
/// answering honestly means choosing from this set and the set is not
/// guessable from the outside.
#[allow(dead_code)]
mod np {
    pub const NP_OK: i32 = 0;
    pub const NP_ERR_DEVICE_NOT_PRESENT: i32 = 1;
    pub const NP_ERR_UNSUPPORTED_OS: i32 = 2;
    pub const NP_ERR_INVALID_ARG: i32 = 3;
    pub const NP_ERR_DLL_NOT_FOUND: i32 = 4;
    pub const NP_ERR_NO_DATA: i32 = 5;
    pub const NP_ERR_INTERNAL_DATA: i32 = 6;
}

use np::{NP_ERR_INTERNAL_DATA, NP_ERR_INVALID_ARG, NP_ERR_NO_DATA, NP_OK};

/// The storage `NP_GetSignature` is handed: `DllSignature[200]` followed by
/// `AppSignature[200]`, both fixed-size C strings.
const SIGNATURE_LEN: usize = 400;

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
/// `NP_ERR_NO_DATA` when nothing has been published — the ordinary state before
/// the Linux side starts sending, and the permanent state of the spike build.
/// `data` is left exactly as it arrived then, because a game polls every frame
/// into a struct it keeps: leaving the last good pose standing beats zeroing
/// it, and the return code is what says the pose is not new.
///
/// # Safety
/// `data` must be null or point to a writable [`TrackIrData`].
#[no_mangle]
pub unsafe extern "system" fn NP_GetData(data: *mut TrackIrData) -> i32 {
    trace!("NP_GetData(data={data:p})");
    if data.is_null() {
        return NP_ERR_INVALID_ARG;
    }
    #[cfg(not(feature = "spike-log"))]
    {
        if trackir::fill(&mut *data) {
            return NP_OK;
        }
    }
    NP_ERR_NO_DATA
}

/// NaturalPoint's anti-clone check, which this client cannot answer.
///
/// A real client fills two 200-byte buffers — `DllSignature` and
/// `AppSignature` — and the game compares them against what it expects. The
/// established open implementations produce them by XORing two byte tables
/// together, which is NaturalPoint's own signature data carried in obfuscated
/// halves; that is why scanning such a DLL for the string "NaturalPoint" finds
/// nothing, and why that absence is not evidence the check is fake.
///
/// We do not ship their blob and do not reconstruct it — it is an
/// authentication measure, and defeating it is not this project's to do. So the
/// answer is `NP_ERR_INTERNAL_DATA`, the ABI's way of saying the data a client
/// holds internally is not here: the device is present, the OS is supported and
/// the argument was fine, so every other code in the set would be a different
/// untruth. The buffer is zeroed first — 400 bytes of the caller's memory were
/// handed over, and a caller that ignores the return code has to find two empty
/// strings there rather than whatever its allocator last left, which with no
/// NUL among 400 bytes of heap residue is a `strlen` running off the end.
///
/// # What games do with it
///
/// Two titles measured, and they do not behave alike:
///
/// - **Star Citizen, 2026-08-15.** Called this, got nothing it recognised, and
///   never asked for data again. It rejects, and stops.
/// - **Microsoft Flight Simulator 2024** (Steam appid 2537590) under Proton
///   Experimental, **2026-09-28**, reported against v0.5.0's spike build: 104
///   calls to this and to *nothing else* in 1m45s — no
///   `NP_RegisterWindowHandle`, no `NP_RequestData`, no `NP_GetData` — at
///   intervals stretching from ~250 ms to several seconds, into a freshly
///   allocated buffer each time. It rejects and retries, for as long as the
///   game runs.
///
/// Both were measured while this returned `NP_OK` and wrote nothing, so a
/// backoff-retry is one thing "success" plus an unrecognised buffer buys.
/// Whether an explicit failure makes MSFS stop retrying is **unmeasured**: the
/// code above is chosen for being true, not for being known to end that loop.
///
/// `tobii bridge install` points TrackIR at an already-installed client for
/// exactly this reason; FreeTrack has no signature and works with our own DLL.
///
/// # Safety
/// `sig` must be null or point to writable storage for the ABI's signature
/// pair. All [`SIGNATURE_LEN`] bytes of it are written.
#[no_mangle]
pub unsafe extern "system" fn NP_GetSignature(sig: *mut c_void) -> i32 {
    trace!("NP_GetSignature(sig={sig:p})");
    if sig.is_null() {
        return NP_ERR_INVALID_ARG;
    }
    std::ptr::write_bytes(sig.cast::<u8>(), 0, SIGNATURE_LEN);
    NP_ERR_INTERNAL_DATA
}

/// # Safety
/// `version` must be null or point to a writable `u16`.
#[no_mangle]
pub unsafe extern "system" fn NP_QueryVersion(version: *mut u16) -> i32 {
    trace!("NP_QueryVersion(version={version:p})");
    if version.is_null() {
        return NP_ERR_INVALID_ARG;
    }
    // 4.00. The established open client reports 5.00, but TIR5 also requires a
    // checksum computed over the head-pose data and relayed to the game, which
    // this does not compute — claiming the newer version without it may be
    // worse than claiming the older one. Untested either way, so it is left
    // where it was rather than changed on a hunch.
    *version = 0x0400;
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

/// The host cannot link a crate that imports `kernel32`, so these run against
/// the Windows target with Wine as the runner:
///
/// ```text
/// WINEPREFIX=<a scratch prefix> \
/// CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine \
///   cargo test --target x86_64-pc-windows-gnu -p NPClient64
/// ```
#[cfg(test)]
mod tests {
    use super::*;

    /// A byte no signature and no version number would contain, so "written"
    /// and "left alone" are distinguishable.
    const POISON: u8 = 0xAA;

    /// Not the default port: the first data call starts the feeder, which binds
    /// it, and 4243 may belong to a hub running on this machine for real.
    const TEST_PORT: u16 = 47431;

    /// The caller allocates this buffer and does not initialise it — MSFS
    /// allocates a fresh one per call — so an unwritten buffer is that game's
    /// own heap residue, read back and compared against a signature.
    #[test]
    fn np_get_signature_zeroes_the_buffer_it_cannot_fill() {
        let mut buf = [POISON; SIGNATURE_LEN];
        let rc = unsafe { NP_GetSignature(buf.as_mut_ptr().cast()) };
        assert_eq!(rc, NP_ERR_INTERNAL_DATA, "must not claim success");
        assert!(
            buf.iter().all(|b| *b == 0),
            "left the caller reading its own uninitialised memory"
        );
    }

    #[test]
    fn np_get_signature_rejects_a_null_buffer() {
        let rc = unsafe { NP_GetSignature(std::ptr::null_mut()) };
        assert_eq!(rc, NP_ERR_INVALID_ARG);
    }

    #[test]
    fn np_query_version_rejects_a_null_pointer() {
        let rc = unsafe { NP_QueryVersion(std::ptr::null_mut()) };
        assert_eq!(rc, NP_ERR_INVALID_ARG);
    }

    #[test]
    fn np_query_version_reports_the_version_it_claims() {
        let mut v = u16::from(POISON);
        let rc = unsafe { NP_QueryVersion(&mut v) };
        assert_eq!((rc, v), (NP_OK, 0x0400));
    }

    #[test]
    fn np_get_data_rejects_a_null_pointer() {
        let rc = unsafe { NP_GetData(std::ptr::null_mut()) };
        assert_eq!(rc, NP_ERR_INVALID_ARG);
    }

    /// The two answers `NP_GetData` has to tell apart, in the order they
    /// happen: nothing published yet, then a frame arriving. One test rather
    /// than two because the feeder is process-wide — a second test could not
    /// know which side of that transition it was on.
    #[test]
    fn np_get_data_answers_no_data_until_a_frame_arrives() {
        std::env::set_var(tobii_bridge_core::feeder::PORT_ENV, TEST_PORT.to_string());

        let mut d = TrackIrData::default();
        assert_eq!(unsafe { NP_GetData(&mut d) }, NP_ERR_NO_DATA);
        assert_eq!(d.wPFrameSignature, 0, "signature advanced over no frame");

        // That first call started the feeder, so something is listening now.
        let frame = tobii_output::TrackingFrame {
            timestamp_us: 1,
            presence: tobii_output::Presence::BothEyes,
            ..Default::default()
        };
        let sock = std::net::UdpSocket::bind("127.0.0.1:0").expect("a socket to send from");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            sock.send_to(&frame.encode(), ("127.0.0.1", TEST_PORT))
                .expect("send a frame to the feeder");
            std::thread::sleep(std::time::Duration::from_millis(50));
            if unsafe { NP_GetData(&mut d) } == NP_OK {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "a published frame never reached NP_GetData"
            );
        }
        assert_ne!(
            d.wPFrameSignature, 0,
            "served a frame without a new signature"
        );
    }
}
