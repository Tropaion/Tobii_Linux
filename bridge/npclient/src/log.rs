//! Spike S2's logger: append every call to a file the game can write.
//!
//! Only compiled with `--features spike-log`. A game gives no console, so the
//! only way to see what it asks for is to write it down.
//!
//! Deliberately crude — open, append, close, per line. A DLL loaded into a game
//! has no safe place to keep a buffered writer alive across an unknown teardown
//! order, and a lost tail is exactly the part that matters when the question is
//! "what did it do last before it stopped".

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

/// Where the log goes. Inside the install directory, so it sits beside the DLL
/// being investigated.
const LOG_PATH: &str = r"C:\tobii-bridge\npclient.log";

/// Call counter, so the order and rate are both visible.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Append one line.
pub fn line(msg: &str) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    // A monotonic tick rather than a wall clock: the question is call *order*
    // and *rate*, and this avoids dragging in time formatting.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_PATH)
    {
        let _ = writeln!(f, "{n:6} {now} {msg}");
    }
}
