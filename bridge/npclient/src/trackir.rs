//! Reading `TRACKIRDATA` out of the shared mapping.
//!
//! The conversion itself — the scaling, the field order, the radians-to-degrees
//! step — lives in `tobii_output::trackir`, where it is unit-tested on the host
//! alongside the FreeTrack layout it converts from. Those are the parts with
//! real bugs in them; this file is the Windows plumbing around them.

use std::sync::atomic::Ordering;

pub use tobii_output::trackir::TrackIrData;

// The spike stub logs calls instead of serving data, so everything below that
// reads the mapping is compiled out along with `fill`.
#[cfg(not(feature = "spike-log"))]
use {
    std::sync::atomic::AtomicU16, std::sync::OnceLock, tobii_bridge_core::feeder,
    tobii_bridge_core::shm::Consumer, tobii_output::trackir::from_ft_heap,
};

/// Advances on every fill, because a game may ignore a repeated signature.
#[cfg(not(feature = "spike-log"))]
static FRAME_SIGNATURE: AtomicU16 = AtomicU16::new(0);

/// Record the profile id from `NP_RegisterProgramProfileID`.
///
/// It reaches consumers now. This used to be stored in a local atomic and go
/// nowhere, with a comment explaining why that was unavoidable: `GameID` lives
/// in the shared mapping, the provider owned the mapping, and this DLL was a
/// separate process holding it read-only. Since the feeder moved into the DLL
/// (see [`feeder`](tobii_bridge_core::feeder)), the process that learns the id
/// is the process that writes the mapping, so the value is simply handed over.
pub fn set_game_id(id: i32) {
    feeder::GAME_ID.store(id, Ordering::Relaxed);
}

/// The profile id the game announced.
#[allow(dead_code)]
pub fn game_id() -> i32 {
    feeder::GAME_ID.load(Ordering::Relaxed)
}

/// Fill `out` from the mapping.
///
/// The mapping is opened once and reused: a game polls `NP_GetData` every
/// frame, so re-opening it per call would be a syscall per frame for nothing.
#[cfg(not(feature = "spike-log"))]
pub fn fill(out: &mut TrackIrData) {
    static SHM: OnceLock<Option<Consumer>> = OnceLock::new();
    // Ordered, not incidental: the feeder is what creates the mapping, and
    // `SHM` caches its answer for the life of the process. Opening first would
    // cache `None` and report "no data" for ever.
    feeder::ensure_started();
    let Some(shm) = SHM.get_or_init(Consumer::open).as_ref() else {
        return;
    };
    let raw = shm.read();
    // As in `ftclient`: a never-written mapping is an all-zero frame, and
    // advancing the signature over it would present a live tracker frozen at
    // dead centre. `DataID == 0` is reserved for "nothing published yet".
    if u32::from_le_bytes(raw[0..4].try_into().expect("4-byte slice")) == 0 {
        return;
    }
    let sig = FRAME_SIGNATURE.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    from_ft_heap(&raw, sig, out);
}
