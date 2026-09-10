//! The Wine-side half of game output.
//!
//! Linux holds the tracker; this runs inside the game's Wine prefix and presents
//! that data through the two interfaces Windows games already know:
//!
//! ```text
//!   Linux: tobii serve ──UDP 127.0.0.1:4243──► tobii-bridge.exe
//!                                                    │
//!                                              FT_SharedMem
//!                                                ▲        ▲
//!                                       NPClient64.dll   freetrackclient64.dll
//!                                            (TrackIR)        (FreeTrack)
//!                                                ▲
//!                                            the game
//! ```
//!
//! # Why UDP crosses the boundary
//!
//! Wine's winsock is a thin shim over host sockets, so a datagram from a Linux
//! process reaches a Wine process bound to the same loopback port with no
//! special support and no Linux-side code beyond a `UdpSocket`. It also stays
//! prefix-agnostic — Proton, Lutris, a flatpak'd runner, even a VM — and a
//! bridge that is not running costs nothing, which matters because "the game is
//! not open yet" is the ordinary case rather than an error.
//!
//! # Why Rust rather than C
//!
//! The DLLs have to be real PE files (a game calls `LoadLibrary` on an explicit
//! registry-derived path, so Wine's builtin `.dll.so` name resolution does not
//! apply), which rules out `winegcc` and demands a PE toolchain either way. Given
//! that, Rust lets this crate depend on `tobii-output` by path, so the FreeTrack
//! byte layout, the degrees-to-radians conversion and the frame decoder are the
//! **same tested code** as on the Linux side rather than an untested
//! re-implementation in the hardest place to debug.

pub mod shm;
pub mod winapi;

/// Where `tobii bridge install` puts the artifacts inside the prefix.
pub const INSTALL_DIR: &str = r"C:\tobii-bridge";

/// Registry key a TrackIR game reads to find its client DLL.
///
/// [CONFIRMED] this exact string is present in `StarCitizen.exe`.
pub const NP_KEY: &str = r"Software\NaturalPoint\NATURALPOINT\NPClient Location";

/// Registry key a FreeTrack game reads to find its client DLL.
pub const FT_KEY: &str = r"Software\Freetrack\FreeTrackClient";

/// The value name under both keys.
pub const PATH_VALUE: &str = "Path";

/// Point both registries at `dir`, so games load our DLLs from there.
///
/// Idempotent, and done by the provider at startup rather than only by the
/// installer: the keys then always describe wherever the DLLs actually are,
/// even if someone moved them by hand.
pub fn register(dir: &str) -> Result<(), String> {
    for key in [NP_KEY, FT_KEY] {
        winapi::set_hkcu_string(key, PATH_VALUE, dir)
            .map_err(|rc| format!("could not write HKCU\\{key}: error {rc}"))?;
    }
    Ok(())
}

/// Remove both registry keys.
pub fn unregister() {
    for key in [NP_KEY, FT_KEY] {
        winapi::delete_hkcu_key(key);
    }
}
