//! Receiving frames from Linux and publishing them into `FT_SharedMem`.
//!
//! # Why this is not only the provider exe's job
//!
//! The original shape was one `tobii-bridge.exe` per prefix, started by hand and
//! left running, with the client DLLs as pure consumers of the mapping it
//! created. That works when you own the prefix and can run things in it. It does
//! not work for the case most users are actually in.
//!
//! A Steam/Proton game runs under **its own wineserver**, with its own prefix
//! path, its own Proton build and its own environment. A `tobii bridge run`
//! started from a terminal with system Wine is a different session entirely: its
//! `FT_SharedMem` is a different object in a different server, and the game
//! never sees it. Getting a second executable into the game's session means
//! reproducing Proton's whole launch environment, which is the part
//! `crates/tobii-cli/src/bridge.rs` already calls "the thing that is easy to get
//! wrong".
//!
//! The DLL, though, is *already inside the game's process* — the game loaded it
//! from the registry path. So the receive loop runs there instead: same
//! wineserver by construction, no second process, nothing to keep running, and
//! no environment to reproduce. Wine's winsock is a thin shim over host sockets,
//! so a datagram from the Linux hub reaches it directly.
//!
//! # Who wins the port
//!
//! Whoever binds it first, and everyone else is a plain consumer. That falls out
//! of the existing design rather than needing arbitration: the loser simply does
//! what the DLLs already did, which is read the mapping somebody else writes. It
//! covers all three cases with one rule — a standalone provider already running,
//! both of our DLLs loaded into one game, or the ordinary single DLL feeding
//! itself.

use std::net::UdpSocket;
use std::sync::mpsc;
use std::sync::Once;
use std::sync::OnceLock;
use std::time::Duration;

use tobii_output::freetrack::FT_HEAP_LEN;
use tobii_output::TrackingFrame;

use crate::shm::{Consumer, Provider};

/// Default loopback port, matching `tobii-output`'s bridge sink.
pub const DEFAULT_PORT: u16 = 4243;

/// Environment variable that overrides [`DEFAULT_PORT`].
///
/// Read rather than configured through a file because the only process that can
/// set it is the one launching the game, which is exactly who knows: Steam
/// launch options and Lutris/Heroic environment settings both pass it through to
/// the game process, and therefore to us.
pub const PORT_ENV: &str = "TOBII_BRIDGE_PORT";

/// The profile id the game announced through `NP_RegisterProgramProfileID`.
///
/// Zero means "unknown", which is what the FreeTrack protocol expects before a
/// game has announced anything.
///
/// This lives here, rather than only in the TrackIR DLL that receives it,
/// because publishing it needs write access to the mapping — and until the
/// feeder moved into the DLL, the DLL was a read-only consumer of a mapping
/// another process owned. `npclient::trackir` recorded the id and said so:
/// "closing the loop would mean either a second channel back to the provider or
/// a read-write mapping with two writers". Neither is needed now that the
/// process holding the id is the process writing the mapping.
pub static GAME_ID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// The port to listen on, from the environment or the default.
pub fn port() -> u16 {
    std::env::var(PORT_ENV)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|p| *p != 0)
        .unwrap_or(DEFAULT_PORT)
}

/// What one attempt to become the feeder did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    /// This process owns the port and is publishing.
    Feeding,
    /// Somebody in **this** wineserver session owns it, proven by their mapping
    /// being openable — so reading theirs is right.
    AlreadyTaken,
    /// The port is held from another session, whose mapping this one cannot
    /// see. Still retrying; the game has nothing to read until it frees up.
    Waiting,
    /// The mapping could not be created at all.
    Failed,
}

/// How often a port held by another wineserver session is retried.
const RETRY: Duration = Duration::from_secs(1);

/// Receive frames on `port` forever, publishing each into the mapping.
///
/// `ready` is signalled once the mapping exists and the socket is bound, so a
/// caller can wait for the mapping before trying to open it as a consumer.
fn serve(port: u16, ready: mpsc::Sender<Started>) {
    // Bind BEFORE creating the mapping, and never create one we might drop.
    //
    // The first version did the opposite, and it was a real bug rather than an
    // ordering preference. The UDP port is **host-wide**; `FT_SharedMem` is
    // **per-wineserver**. So "somebody already has the port" does not imply
    // "somebody in this session has a mapping" — a leftover `tobii bridge run`,
    // or a game in another prefix, holds the port from a different session
    // entirely. Creating a provider and then dropping it on a failed bind
    // destroyed the only mapping in the game's own session, and the caller's
    // immediate `Consumer::open` usually won the race against that drop and
    // pinned the doomed section: the game then read `FTGetData == true` with an
    // all-zero frame and a ticking counter — a live-looking tracker frozen at
    // dead centre — for the life of the process, because `START` is a `Once`.
    //
    // Ceding is only correct when the holder is in THIS session, and an
    // openable mapping is exactly what proves that. Otherwise the port is
    // watched until it frees up.
    let socket = loop {
        match UdpSocket::bind(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(_) => {
                if Consumer::open().is_some() {
                    let _ = ready.send(Started::AlreadyTaken);
                    return;
                }
                // A holder in another session. Keep waiting rather than
                // leaving the game blind: when it exits, this takes over.
                let _ = ready.send(Started::Waiting);
                std::thread::sleep(RETRY);
            }
        }
    };
    let provider = match Provider::create() {
        Ok(p) => p,
        Err(_) => {
            let _ = ready.send(Started::Failed);
            return;
        }
    };
    let _ = ready.send(Started::Feeding);

    let mut buf = [0u8; 512];
    loop {
        let Ok((n, _from)) = socket.recv_from(&mut buf) else {
            continue;
        };
        // A malformed datagram is dropped in silence. This runs inside a game
        // with no console, and a version mismatch would otherwise mean sixty
        // identical complaints a second into nowhere.
        if let Ok(frame) = TrackingFrame::decode(&buf[..n]) {
            provider.publish(&frame, GAME_ID.load(std::sync::atomic::Ordering::Relaxed));
        }
    }
}

static START: Once = Once::new();

/// Start feeding in a background thread, at most once per process.
///
/// # Why this must not be called from `DllMain`
///
/// It creates a thread. `DllMain` runs under the loader lock, and starting a
/// thread there is the textbook way to deadlock a Windows process — the new
/// thread's own attach notification cannot run until the lock is released, and
/// the lock is not released until `DllMain` returns. So the DLLs call this
/// lazily from the first data call the game makes, which is well outside the
/// loader.
///
/// Blocks until the mapping exists, up to a short deadline, because the caller's
/// very next act is to open that mapping as a consumer — and the consumer handle
/// is cached for the life of the process, so losing that race once would mean
/// reporting "no data" forever.
pub fn ensure_started() {
    START.call_once(|| {
        let (tx, rx) = mpsc::channel();
        let p = port();
        if std::thread::Builder::new()
            .name("tobii-bridge-feeder".into())
            .spawn(move || serve(p, tx))
            .is_err()
        {
            return;
        }
        // Creating a mapping and binding a loopback socket are immediate; the
        // deadline is only here so a wedged call can never hang a game's frame
        // loop for longer than a hitch.
        let _ = rx.recv_timeout(Duration::from_secs(1));
    });
}

/// The frame the mapping currently holds, or `None` when there is nothing a
/// game should be shown.
///
/// Both client DLLs read through this, and the order inside it is load-bearing
/// rather than incidental: the feeder is what creates the mapping, and the
/// consumer handle is cached for the life of the process, so opening first
/// would cache `None` and report "no data" for ever.
///
/// `None` also covers a mapping that exists but has never been written. That
/// reads as a perfectly valid all-zero frame, which in a game is a live tracker
/// sitting at dead centre — worse than no tracker, because nothing looks wrong.
/// Since a DLL creates the mapping itself now, that state is ordinary whenever
/// nothing is sending yet, so `DataID == 0` is reserved to mean exactly it (see
/// [`tobii_output::freetrack::next_data_id`]).
///
/// The mapping is opened once and reused: a game polls for data every frame, so
/// re-opening it per call would be a syscall per frame for nothing.
pub fn published_frame() -> Option<[u8; FT_HEAP_LEN]> {
    static SHM: OnceLock<Option<Consumer>> = OnceLock::new();
    ensure_started();
    let raw = SHM.get_or_init(Consumer::open).as_ref()?.read();
    if u32::from_le_bytes(raw[0..4].try_into().expect("4-byte slice")) == 0 {
        return None;
    }
    Some(raw)
}
