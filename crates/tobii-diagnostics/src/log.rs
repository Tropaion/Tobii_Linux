//! A log the user can actually send you.
//!
//! # Why this exists at all
//!
//! Every warning in this program used to be an `eprintln!`, which is fine for
//! `tobii stream` in a terminal and useless for the GUI: a hub launched from the
//! application menu has its stderr wired to the session journal or to
//! `/dev/null`, so *"warning: could not apply saved calibration"* — the single
//! most useful sentence a user could quote — is written to somewhere they will
//! never look. A bug report then arrives saying "tracking is bad", with no way
//! to recover what the program already knew.
//!
//! So warnings go three places at once: stderr (unchanged, for people running
//! from a terminal), a small ring buffer in memory, and a capped file under
//! `$XDG_STATE_HOME`. [`crate::report`] prints the tail, which is what makes it
//! worth attaching to an issue.
//!
//! # Deliberately not a logging framework
//!
//! No `log`, no `tracing`, no levels beyond warn/info, no filtering, no
//! subscriber to configure. This exists to answer one question — "what did the
//! program complain about before it went wrong?" — and the same
//! dependency-avoidance reasoning as the hand-rolled SHA-256 and JSON applies:
//! a driver that must be installable from source pays for every crate in its
//! tree. If this ever needs spans or structured fields, that is the moment to
//! take the dependency, not before.
//!
//! # What is not written here
//!
//! The log ends up on a public issue tracker via [`crate::report`], so nothing
//! goes in that would not survive that. In particular the home path is folded to
//! `~` on the way *out*, not on the way in, so a local reader still sees real
//! paths in the file itself.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// How many lines the in-memory buffer keeps.
///
/// Enough to cover a connect, a failure and a retry; short enough that pasting
/// it into an issue stays reasonable.
pub const RING: usize = 60;

/// How large the log file may grow before the oldest half is dropped.
///
/// A cap rather than rotation: a second file to find is a second file nobody
/// sends. 128 KB is thousands of warnings and still trivial to open.
pub const MAX_FILE_BYTES: u64 = 128 * 1024;

/// The most recent lines, for [`crate::report`].
///
/// Kept in memory as well as on disk so a report is still useful when the state
/// directory is unwritable — which is exactly the kind of broken setup somebody
/// files an issue about.
static RECENT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// `$XDG_STATE_HOME/tobii-linux/tobii.log`, falling back to
/// `~/.local/state/tobii-linux/tobii.log`.
///
/// State, not config and not cache: the XDG spec puts logs in state, and it is
/// the one of the three that is neither backed up as settings nor deleted as
/// disposable.
pub fn log_path() -> PathBuf {
    // An explicit override wins. Useful to a packager pointing the log
    // somewhere else, to anyone debugging two instances at once — and it is
    // what lets this module's own tests use a file of their own instead of
    // flooding the real one.
    if let Some(p) = std::env::var_os("TOBII_LOG_FILE").filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    // Directory and name from `tobii_config::paths`, which `tobii uninstall
    // --purge` reads too; never relative, see `tobii_config::paths::xdg_dir`.
    tobii_config::paths::state_dir().join(tobii_config::paths::LOG_FILE)
}

/// Record a warning: stderr, memory, and the file.
pub fn warn(msg: &str) {
    write_line("WARN", msg);
}

/// Record something worth having in a bug report but not worth alarming anyone.
pub fn info(msg: &str) {
    write_line("INFO", msg);
}

fn write_line(level: &str, msg: &str) {
    // One line, however many the message has: a multi-line entry breaks the
    // tail count and the paste.
    // Runs of line breaks collapse to ONE space: `\r\n` is two characters and
    // would otherwise leave a double space in the middle of a sentence.
    // Every control character, not just the line breaks. The breaks are what
    // would split one entry into two and break the tail count; ESC and NUL are
    // what would let a warning's text repaint or truncate the report it ends up
    // in, since that report is read in a terminal and pasted into an issue. All
    // of them collapse to a single space, so `\r\n` does not leave a double
    // one in the middle of a sentence.
    let mut flat = String::with_capacity(msg.len());
    let mut last_was_control = false;
    for c in msg.chars() {
        if c.is_control() {
            if !last_was_control {
                flat.push(' ');
            }
            last_was_control = true;
        } else {
            flat.push(c);
            last_was_control = false;
        }
    }
    let line = format!("{} {level} {}", stamp(), flat.trim());

    // stderr first and unconditionally, so nothing that used to be visible in a
    // terminal stops being visible.
    eprintln!("{}", flat.trim());

    if let Ok(mut r) = RECENT.lock() {
        r.push(line.clone());
        let len = r.len();
        if len > RING {
            r.drain(..len - RING);
        }
    }

    // Best-effort: a program that cannot write its log must still run.
    let _ = append(&line);
}

fn append(line: &str) -> std::io::Result<()> {
    let path = log_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Trim before appending rather than after, so the file never exceeds the
    // cap even briefly.
    //
    // `read_to_string` used to guard this, which meant the cap stopped applying
    // the moment the file was not valid UTF-8 — and then it grew without limit,
    // silently, in the one case where something is already wrong. A log this
    // program wrote is always UTF-8, but the file is a plain path: an
    // interrupted write, a filesystem that lost a block, or a `TOBII_LOG_FILE`
    // pointed at something else all produce bytes that are not. So the
    // fallback is to truncate rather than to give up.
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let keep: Vec<&str> = text.lines().skip(text.lines().count() / 2).collect();
                let _ = std::fs::write(
                    &path,
                    format!("[older entries dropped]\n{}\n", keep.join("\n")),
                );
            }
            Err(_) => {
                let _ = std::fs::write(&path, "[log was not text; dropped]\n");
            }
        }
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(f, "{line}")
}

/// The most recent lines, oldest first.
pub fn recent(max: usize) -> Vec<String> {
    let Ok(r) = RECENT.lock() else {
        return Vec::new();
    };
    let start = r.len().saturating_sub(max);
    r[start..].to_vec()
}

/// The tail of the log FILE, for a process that did not write it.
///
/// The hub and the CLI are separate processes with separate ring buffers, so a
/// report produced by `tobii debug` would otherwise show nothing the GUI logged
/// — which is the case that matters most, since the GUI's warnings are the ones
/// a user cannot see.
pub fn tail_file(max: usize) -> Vec<String> {
    // Lossy rather than `read_to_string`, for the same reason `append` no
    // longer gives up on invalid UTF-8: a log that lost a block would
    // otherwise vanish from the report entirely, exactly when it is wanted.
    let path = log_path();
    // A regular file, or nothing. `fs::read` on a FIFO blocks until somebody
    // writes to the other end — forever, in practice — and this runs on the GTK
    // main thread when the settings popover's copy button is pressed. The path
    // is `TOBII_LOG_FILE`-settable, and /dev/stdin, a socket and a directory
    // are all reachable by accident as well as on purpose.
    if !std::fs::metadata(&path)
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

/// A UTC timestamp, `YYYY-MM-DD HH:MM:SS`.
fn stamp() -> String {
    let (y, m, d, h, mi, s) = utc_now();
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// The current UTC time as `(year, month, day, hour, minute, second)`.
///
/// Hand-rolled from the epoch rather than taking a date crate for one line, the
/// same trade the JSON and SHA-256 code makes. Civil-time conversion by Howard
/// Hinnant's `civil_from_days`.
///
/// Public because the CLI needs the same six numbers for the RFC 3339 header on
/// a capture file. Sixteen lines of leap-year and era arithmetic that nobody can
/// eyeball for correctness is exactly the thing not to keep two copies of — and
/// it had two, written the same day in two crates.
pub fn utc_now() -> (i64, i64, i64, i64, i64, i64) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi, s)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The log is process-global — one ring buffer and one file — so tests that
    /// write to it cannot run beside each other: one test's filler becomes
    /// another's missing line. This serialises them and points the file
    /// somewhere disposable.
    pub(crate) static LOG_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn with_own_log<T>(tag: &str, f: impl FnOnce() -> T) -> T {
        let _guard = LOG_TEST.lock().unwrap_or_else(|e| e.into_inner());
        let path = std::env::temp_dir().join(format!("tobii-log-test-{tag}.log"));
        let _ = std::fs::remove_file(&path);
        std::env::set_var("TOBII_LOG_FILE", &path);
        if let Ok(mut r) = RECENT.lock() {
            r.clear();
        }
        let out = f();
        std::env::remove_var("TOBII_LOG_FILE");
        let _ = std::fs::remove_file(&path);
        out
    }

    #[test]
    fn a_warning_is_kept_in_memory_and_shows_up_in_the_tail() {
        let r = with_own_log("kept", || {
            warn("test: the tracker could not be opened");
            recent(RING)
        });
        assert!(
            r.iter()
                .any(|l| l.contains("the tracker could not be opened")),
            "{r:?}"
        );
        assert!(r.last().unwrap().contains("WARN"));
    }

    /// A multi-line message would otherwise break the tail count and the paste.
    #[test]
    fn a_multiline_message_becomes_one_line() {
        let last = with_own_log("multiline", || {
            warn("test: first\nsecond\r\nthird");
            recent(1).pop().expect("a line")
        });
        assert!(!last.contains('\n'), "{last}");
        assert!(last.contains("first second third"), "{last}");
    }

    /// ESC and NUL reach a terminal and an issue tracker if they are not
    /// stopped here: the report is read in a terminal and pasted into a form,
    /// and an escape sequence in a warning can repaint or hide what is around
    /// it.
    #[test]
    fn control_characters_do_not_survive_into_a_log_line() {
        let last = with_own_log("control", || {
            warn("test: red\u{1b}[31m and \u{0}nul and \u{7}bell");
            recent(1).pop().expect("a line")
        });
        assert!(!last.contains('\u{1b}'), "{last:?}");
        assert!(!last.contains('\u{0}'), "{last:?}");
        assert!(!last.contains('\u{7}'), "{last:?}");
        assert!(last.contains("red"), "{last:?}");
        assert!(last.contains("nul"), "{last:?}");
    }

    /// The cap used to be guarded by `read_to_string`, so it stopped applying
    /// entirely the moment the file was not valid UTF-8 — and then grew without
    /// limit, silently, in the one case where something is already wrong.
    #[test]
    fn a_log_that_is_not_utf8_is_still_capped() {
        with_own_log("badutf8", || {
            let path = log_path();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Over the cap, and invalid UTF-8 throughout.
            std::fs::write(&path, vec![0xffu8; (MAX_FILE_BYTES + 4096) as usize]).unwrap();
            warn("test: after the bad bytes");
            let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            assert!(
                len < MAX_FILE_BYTES,
                "the file was not trimmed: {len} bytes"
            );
            // And the report can still read what is there.
            assert!(
                tail_file(5)
                    .iter()
                    .any(|l| l.contains("after the bad bytes")),
                "{:?}",
                tail_file(5)
            );
        });
    }

    #[test]
    fn the_ring_keeps_only_the_most_recent_lines() {
        let r = with_own_log("ring", || {
            for i in 0..RING * 2 {
                info(&format!("test: filler {i}"));
            }
            recent(RING * 2)
        });
        assert!(r.len() <= RING, "the ring grew to {}", r.len());
        assert!(
            r.last()
                .unwrap()
                .contains(&format!("filler {}", RING * 2 - 1)),
            "the newest line should survive"
        );
    }

    #[test]
    fn the_timestamp_looks_like_a_timestamp() {
        let s = stamp();
        assert_eq!(s.len(), 19, "{s}");
        assert!(s.starts_with("20"), "{s}");
        assert_eq!(s.chars().filter(|c| *c == '-').count(), 2, "{s}");
        assert_eq!(s.chars().filter(|c| *c == ':').count(), 2, "{s}");
    }

    #[test]
    fn the_log_path_follows_xdg_state() {
        // The override is process-global, so this takes the same lock as the
        // tests that set it.
        let _guard = LOG_TEST.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("TOBII_LOG_FILE");
        let p = log_path();
        assert!(p.ends_with("tobii-linux/tobii.log"), "{}", p.display());
        assert!(
            p.to_string_lossy().contains("state"),
            "logs belong in XDG_STATE_HOME: {}",
            p.display()
        );

        // And the override wins when it is set.
        std::env::set_var("TOBII_LOG_FILE", "/tmp/somewhere-else.log");
        assert_eq!(log_path(), PathBuf::from("/tmp/somewhere-else.log"));
        std::env::remove_var("TOBII_LOG_FILE");
    }
}
