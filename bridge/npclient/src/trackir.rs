//! Reading `TRACKIRDATA` out of the shared mapping.
//!
//! The conversion itself — the scaling, the field order, the radians-to-degrees
//! step — lives in `tobii_output::trackir`, where it is unit-tested on the host
//! alongside the FreeTrack layout it converts from. Those are the parts with
//! real bugs in them; this file is the Windows plumbing around them.

use std::sync::atomic::{AtomicI32, Ordering};

pub use tobii_output::trackir::TrackIrData;

// The spike stub logs calls instead of serving data, so everything below that
// reads the mapping is compiled out along with `fill`.
#[cfg(not(feature = "spike-log"))]
use {
    std::sync::atomic::AtomicU16, std::sync::OnceLock, tobii_bridge_core::shm::Consumer,
    tobii_output::trackir::from_ft_heap,
};

/// Advances on every fill, because a game may ignore a repeated signature.
#[cfg(not(feature = "spike-log"))]
static FRAME_SIGNATURE: AtomicU16 = AtomicU16::new(0);

/// The profile id the game announced, if any.
static GAME_ID: AtomicI32 = AtomicI32::new(0);

/// Record the profile id from `NP_RegisterProgramProfileID`.
pub fn set_game_id(id: i32) {
    GAME_ID.store(id, Ordering::Relaxed);
}

/// The profile id the game announced.
///
/// Not yet delivered anywhere, and that is a real gap rather than an oversight:
/// `GameID` lives in the shared mapping, the **provider** owns that mapping, and
/// this DLL is a different process holding it read-only. Closing the loop would
/// mean either a second channel back to the provider or a read-write mapping
/// with two writers — neither worth building until something needs the value.
///
/// Spike S2 records the id Star Citizen passes, which is the evidence for
/// whether anything does.
#[allow(dead_code)]
pub fn game_id() -> i32 {
    GAME_ID.load(Ordering::Relaxed)
}

/// Fill `out` from the provider's mapping, if one is running.
///
/// The mapping is opened once and reused: a game polls `NP_GetData` every
/// frame, so re-opening it per call would be a syscall per frame for nothing.
#[cfg(not(feature = "spike-log"))]
pub fn fill(out: &mut TrackIrData) {
    static SHM: OnceLock<Option<Consumer>> = OnceLock::new();
    let Some(shm) = SHM.get_or_init(Consumer::open).as_ref() else {
        return;
    };
    let raw = shm.read();
    let sig = FRAME_SIGNATURE.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    from_ft_heap(&raw, sig, out);
}
