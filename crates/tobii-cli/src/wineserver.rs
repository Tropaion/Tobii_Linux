//! Who else wants this Wine prefix, and what standing on it costs.
//!
//! # The trap
//!
//! Steam launches a Proton title with the verb `waitforexitandrun`, and
//! Proton's launcher runs `wineserver -w` **before** it spawns the game
//! executable. `wineserver -w` is `fcntl(F_SETLKW)` on byte 0 of
//! `/tmp/.wine-<uid>/server-<dev>-<ino>/lock` (wine's `server/request.c`,
//! `wait_for_lock`), and a live wineserver holds that write lock for its entire
//! lifetime by design — `acquire_lock` takes it and never closes the
//! descriptor, precisely so that it is held until the process exits.
//!
//! So *any* wine process alive on the game's prefix stops the game from ever
//! being spawned. Not a crash, not a refusal: a launcher that sits there. That
//! is what a user reports as "the game freezes or won't start while the bridge
//! is running", and it is neither the game disliking us nor anything specific
//! to one title — opentrack hits the identical wall.
//!
//! Measured on 2026-09-26: with one wine process holding a throwaway prefix,
//! `wineserver -w` timed out at 4 s (exit 124) and returned 0 the instant the
//! holder died.
//!
//! `tobii bridge run` is exactly such a process — it is a bare `wine` on the
//! game's prefix — which is why this module exists. It cannot remove the lock.
//! What it can do is name the trap at the moment somebody walks into it
//! ([`before_run`]), and get out of the way when a launch does start waiting
//! ([`blocked_waiter`]).
//!
//! # Why yielding is enough, rather than a startup race we must win
//!
//! The helper does not have to exist before the game. Measured on 2026-09-26:
//! opentrack's `NPClient64.dll`, driven through the full handshake, polled
//! `NP_GetData` 102 times with **no `FT_SharedMem` present at all** — all
//! zeros — and then picked the mapping up mid-run, reporting a correct pose,
//! when a separate process created and fed it. It does not cache the absence.
//!
//! "Eventually, in the same session" is therefore enough, and eventually is
//! exactly when no deadlock is possible. That is what turns an unwinnable
//! ordering problem into a rule a user can follow: game first, bridge second.
//!
//! # What is NOT established, and must not be implied anywhere below
//!
//! * **That a `wine` started from here joins a containerised game's
//!   wineserver.** The cross-process proof behind this module used host wine on
//!   a host prefix. The Steam Linux Runtime shares the host `/tmp` (measured:
//!   same device and inode on both sides), which is why the lock contends at
//!   all — but the joining half is unconfirmed. If it turns out not to join,
//!   yielding still fixes the freeze and the user still gets no tracking from
//!   `bridge run`.
//! * **That any of this makes a game use the data.** It only stops a second
//!   process from breaking the launch. No message in this module may suggest
//!   otherwise.

use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// What the prefix's server lock says right now.
///
/// Three cases and not a bool, because the two ways of answering "no holder"
/// need different words: nothing is serving this prefix, or we could not find
/// out. A probe that failed must not be reported as an empty prefix — the
/// warning that follows is the same either way, but claiming to have looked
/// when we could not is how a user stops believing the next message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lock {
    /// Nobody holds it. Starting a wine process now makes *us* the holder.
    Free,
    /// Held, by this pid — or by a holder the kernel would not name, which is
    /// what a lock taken through an open file description reports.
    Held(i32),
    /// The question could not be answered, and why.
    Unknown(String),
}

/// Where wine puts the lock for the prefix directory identified by `(dev, ino)`.
///
/// `init_server_dir` (wine's `dlls/ntdll/unix/server.c`) spells it
/// `/tmp/.wine-<uid>/server-<dev>-<ino>`, both numbers in lowercase hex with no
/// padding, from a `stat` of the prefix directory. Derived from the *inode*
/// rather than the path, which is the whole reason the conflict is per-prefix
/// and completely independent of which wine binary, which runner, or which
/// container started the process: two different spellings of one prefix, from
/// inside and outside a container, compute the same lock.
///
/// `uid` is a parameter rather than a `getuid()` call so that this stays a pure
/// function. The tests must not depend on who runs them, and CI runs them as
/// root.
///
/// Wine has two `sprintf` branches here, `%llx` and `%lx`, chosen by whether
/// `st_dev` survives a round trip through `unsigned long`. On the x86_64 Linux
/// this program targets, `unsigned long` is 64 bits, so both branches print the
/// same text and one format covers both.
pub fn lock_path(uid: u32, dev: u64, ino: u64) -> PathBuf {
    PathBuf::from(format!("/tmp/.wine-{uid}/server-{dev:x}-{ino:x}/lock"))
}

/// The lock file for a prefix on this machine, as this user.
///
/// The impure half of [`lock_path`]: one `stat` and one `getuid`.
pub fn lock_for(prefix: &Path) -> Result<PathBuf, String> {
    let md = std::fs::metadata(prefix)
        .map_err(|e| format!("could not stat {}: {e}", prefix.display()))?;
    // SAFETY: getuid cannot fail and touches nothing.
    let uid = unsafe { libc::getuid() };
    Ok(lock_path(uid, md.dev(), md.ino()))
}

/// Ask who holds the prefix lock, without ever waiting for it.
///
/// `F_GETLK`, never `F_SETLK` or `F_SETLKW`: the first would *take* the lock
/// we are asking about — briefly becoming the very holder that hangs a launch —
/// and the second is the blocking call this whole module exists to keep out of
/// the way of. `F_GETLK` reports what a lock attempt *would* hit and changes
/// nothing.
///
/// A missing file is [`Lock::Free`] and not an error: wineserver creates the
/// directory and the lock at startup, so nothing there means nothing has ever
/// served this prefix as this user.
pub fn probe(lock: &Path) -> Lock {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Lock::Free,
        Err(e) => return Lock::Unknown(format!("{}: {e}", lock.display())),
    };
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = libc::F_WRLCK as libc::c_short;
    fl.l_whence = libc::SEEK_SET as libc::c_short;
    // Byte 0 only — the same single byte wine locks.
    fl.l_start = 0;
    fl.l_len = 1;
    // SAFETY: `fd` is open for the life of the call and `fl` is a live,
    // fully initialised `flock`.
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut fl) };
    if rc == -1 {
        return Lock::Unknown(format!(
            "{}: {}",
            lock.display(),
            std::io::Error::last_os_error()
        ));
    }
    if fl.l_type == libc::F_UNLCK as libc::c_short {
        Lock::Free
    } else {
        Lock::Held(fl.l_pid)
    }
}

/// What to tell the user before `tobii bridge run` starts, given what the probe
/// found.
///
/// Pure, and returns the whole block rather than printing it, so the wording is
/// testable without a prefix, a wine, or a uid.
///
/// Both answers proceed. The refusal this implements is a refusal to let
/// somebody walk into the trap *unwarned*, not a refusal to run: a user who
/// wants the bridge up before the game may have a reason we do not know, and a
/// hard stop would only teach them a flag to silence it.
pub fn before_run(lock: &Lock) -> String {
    // The consequence and the way out, written once: every branch needs it, and
    // three drifting copies of the same paragraph is how the wrong one ends up
    // in front of the user who most needed the right one.
    let trap = "\
         Steam launches a Proton title with the verb `waitforexitandrun`, and Proton\n\
         runs `wineserver -w` before it spawns the game — a call that waits for every\n\
         wine process on this prefix to exit first. While this command runs, the next\n\
         launch of this game will sit there doing nothing.\n\
         \n\
         The supported order is the other way round: start the game, let it reach its\n\
         menu, then start this. Late is not too late — a client DLL polled with no\n\
         shared mapping present at all read a correct pose the moment one appeared.\n\
         If a launch does start waiting while this runs, this command stops itself and\n\
         says so, so the launch can go through.\n";
    match lock {
        Lock::Free => format!(
            "warning: nothing is serving this prefix yet, so the bridge is about to\n\
             become its wineserver — and that is what hangs a Steam launch.\n\
             \n\
             {trap}"
        ),
        Lock::Held(pid) => {
            let who = if *pid > 0 {
                format!(" (pid {pid})")
            } else {
                String::new()
            };
            format!(
                "a wineserver is already serving this prefix{who}. If that is the game,\n\
                 this is the supported order and starting now cannot hang a launch that\n\
                 has already happened.\n\
                 Whether a bridge started from outside the game's container actually joins\n\
                 that wineserver is unconfirmed — if the game sees no tracking, that is the\n\
                 thing to report.\n"
            )
        }
        Lock::Unknown(why) => format!(
            "note: could not tell whether anything is already serving this prefix\n\
             ({why}), so what follows may not apply.\n\
             \n\
             {trap}"
        ),
    }
}

/// What to say on the way out when a launch is waiting on us.
///
/// Deliberately says nothing about whether the game will then *use* the data.
/// Stopping unblocks a launch; that is the entire claim.
pub fn yielding(pid: i32) -> String {
    format!(
        "\na Proton launch is waiting for this prefix's wineserver to exit — pid {pid} is\n\
         blocked on the prefix lock, and this command is part of what it is waiting for.\n\
         Stopping so the launch can go through.\n\
         \n\
         Start it again once the game is up: a client DLL picks up a mapping that\n\
         appears after it. That does not make the game accept the data — it only stops\n\
         this command blocking the launch.\n"
    )
}

/// The major number `/proc/locks` prints for a device, in glibc's encoding.
fn dev_major(dev: u64) -> u32 {
    (((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff_u64)) as u32
}

/// The minor number `/proc/locks` prints for a device, in glibc's encoding.
fn dev_minor(dev: u64) -> u32 {
    ((dev & 0xff) | ((dev >> 12) & !0xff_u64)) as u32
}

/// The pid of a process *blocked* on the lock file identified by `(dev, ino)`,
/// read out of `/proc/locks` text.
///
/// Pure, and takes the text rather than reading the file, because the live
/// `/proc/locks` on a developer's machine is whatever happens to be locked
/// there this second — a test written against it asserts nothing and fails for
/// reasons that have nothing to do with this code.
///
/// The shape, measured on 2026-09-26 with a real `wineserver -w` blocked behind
/// a real holder — the holder first, then the waiter as a second line on the
/// same inode, marked with `->` and naming its own pid:
///
/// ```text
/// 1: POSIX  ADVISORY  WRITE 1140426 00:36:483507 0 0
/// 1: -> POSIX  ADVISORY  WRITE 1140518 00:36:483507 0 0
/// ```
///
/// Only the `->` lines are of interest. The holder's line is almost always ours
/// — acting on it would make the bridge quit the instant it started.
///
/// The device is compared as well as the inode, and numerically rather than as
/// the printed text. Inode numbers are unique only within a filesystem, so an
/// inode-only match can fire on a completely unrelated file on another mount;
/// and the kernel prints major and minor with `%02x`, a *minimum* width, so any
/// comparison against a fixed-width rendering of our own breaks the moment
/// either number needs three digits.
pub fn blocked_waiter(locks: &str, dev: u64, ino: u64) -> Option<i32> {
    let (major, minor) = (dev_major(dev), dev_minor(dev));
    locks.lines().find_map(|line| {
        let mut f = line.split_whitespace();
        // `1:` — the block id. Its presence is what says this is a lock line.
        if !f.next()?.ends_with(':') {
            return None;
        }
        if f.next()? != "->" {
            return None;
        }
        // class, mode, type — three words for every lock the kernel prints,
        // POSIX/FLOCK/OFDLCK and LEASE/DELEG alike.
        f.next()?;
        f.next()?;
        f.next()?;
        let pid: i32 = f.next()?.parse().ok()?;
        let mut ids = f.next()?.split(':');
        let maj = u32::from_str_radix(ids.next()?, 16).ok()?;
        let min = u32::from_str_radix(ids.next()?, 16).ok()?;
        let i: u64 = ids.next()?.parse().ok()?;
        (maj == major && min == minor && i == ino).then_some(pid)
    })
}

/// `(dev, ino)` of the lock file, once it exists.
///
/// `None` while it does not: a prefix nothing has ever served has no lock file,
/// and our own wine creates it moments after it starts — so the caller asks
/// again rather than deciding once.
pub fn lock_ids(lock: &Path) -> Option<(u64, u64)> {
    let md = std::fs::metadata(lock).ok()?;
    Some((md.dev(), md.ino()))
}

/// Read `/proc/locks` and report a launch blocked on this lock file.
pub fn waiting_launch(dev: u64, ino: u64) -> Option<i32> {
    let text = std::fs::read_to_string("/proc/locks").ok()?;
    blocked_waiter(&text, dev, ino)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one path the whole mechanism turns on, spelled the way wine spells
    /// it — lowercase hex, unpadded, both numbers, with the uid in the middle.
    ///
    /// The reference point is the path in opentrack issue #2211,
    /// `/tmp/.wine-1000/server-41-2ab19f/lock`: device 0x41, inode 0x2ab19f.
    #[test]
    fn the_lock_path_is_wines_own_spelling_of_the_prefixs_device_and_inode() {
        assert_eq!(
            lock_path(1000, 0x41, 0x2a_b19f),
            PathBuf::from("/tmp/.wine-1000/server-41-2ab19f/lock")
        );
    }

    /// Hex, not decimal, and unpadded. A decimal rendering of the same numbers
    /// names a directory that does not exist, so the probe would report an
    /// empty prefix for every prefix and the warning would never be right.
    #[test]
    fn the_numbers_are_unpadded_lowercase_hex() {
        assert_eq!(
            lock_path(0, 254, 4_886_718_345),
            PathBuf::from("/tmp/.wine-0/server-fe-123456789/lock")
        );
    }

    /// The uid comes from the caller, so a test says nothing about who runs it
    /// — CI runs these as root and a developer does not.
    #[test]
    fn the_uid_is_the_callers_and_not_this_processs() {
        assert_eq!(
            lock_path(65_534, 1, 2),
            PathBuf::from("/tmp/.wine-65534/server-1-2/lock")
        );
    }

    /// The measured shape, both lines, as `/proc/locks` really printed them.
    #[test]
    fn a_blocked_waiter_on_our_inode_is_found_and_named() {
        let locks = "\
1: POSIX  ADVISORY  WRITE 1140426 00:36:483507 0 0\n\
1: -> POSIX  ADVISORY  WRITE 1140518 00:36:483507 0 0\n";
        assert_eq!(blocked_waiter(locks, 54, 483_507), Some(1_140_518));
    }

    /// The holder's line is ours. Reading it as a waiter would make the bridge
    /// stop the moment it managed to start, every time, with a message blaming
    /// a launch that is not happening.
    #[test]
    fn the_holder_alone_is_not_a_waiter() {
        let locks = "1: POSIX  ADVISORY  WRITE 1140426 00:36:483507 0 0\n";
        assert_eq!(blocked_waiter(locks, 54, 483_507), None);
    }

    /// Inode numbers repeat across filesystems, so the device has to agree too
    /// — otherwise somebody else's blocked `flock` on an unrelated mount ends
    /// the user's tracking session and tells them a game is launching.
    #[test]
    fn a_waiter_on_another_filesystem_with_the_same_inode_is_not_ours() {
        let locks = "1: -> POSIX  ADVISORY  WRITE 1140518 08:02:483507 0 0\n";
        assert_eq!(blocked_waiter(locks, 54, 483_507), None);
    }

    /// And the inode has to agree: a blocked waiter on some other file of the
    /// same filesystem is somebody else's business entirely.
    #[test]
    fn a_waiter_on_another_file_of_the_same_device_is_not_ours() {
        let locks = "1: -> POSIX  ADVISORY  WRITE 1140518 00:36:999999 0 0\n";
        assert_eq!(blocked_waiter(locks, 54, 483_507), None);
    }

    /// `%02x` is a minimum width, not a fixed one. A device whose major needs
    /// three hex digits still has to match, which is why the numbers are
    /// compared as numbers.
    #[test]
    fn a_wide_major_still_matches() {
        // major 0x103, minor 0x07 — dev_t 0x10307 in glibc's encoding.
        let locks = "1: -> POSIX  ADVISORY  WRITE 4242 103:07:99 0 0\n";
        assert_eq!(blocked_waiter(locks, 0x1_0307, 99), Some(4242));
    }

    /// Lines the kernel prints that are not locks at all, and truncated ones,
    /// answer "no waiter" rather than panicking.
    #[test]
    fn nonsense_is_not_a_waiter() {
        for text in ["", "\n", "not a lock line\n", "1: -> POSIX\n", "1: ->\n"] {
            assert_eq!(blocked_waiter(text, 54, 483_507), None, "{text:?}");
        }
    }

    /// The `Free` warning has to name the thing the user is about to do to
    /// themselves and the order that avoids it. Without both it is noise.
    #[test]
    fn the_warning_names_the_launch_that_will_hang_and_the_order_that_avoids_it() {
        let w = before_run(&Lock::Free);
        assert!(w.starts_with("warning:"), "{w}");
        assert!(w.contains("wineserver -w"), "{w}");
        assert!(w.contains("start the game"), "{w}");
    }

    /// A held lock is the supported order, so it is not a warning — and it says
    /// what it does not know, because "proceed" must not read as "this works".
    #[test]
    fn an_already_served_prefix_is_not_warned_about_and_promises_nothing() {
        let w = before_run(&Lock::Held(4242));
        assert!(!w.contains("warning"), "{w}");
        assert!(w.contains("pid 4242"), "{w}");
        assert!(w.contains("unconfirmed"), "{w}");
    }

    /// A lock held through an open file description reports no pid. Printing
    /// `pid -1` at somebody is worse than saying nothing about who holds it.
    #[test]
    fn a_holder_the_kernel_will_not_name_is_not_given_a_pid() {
        let w = before_run(&Lock::Held(-1));
        assert!(!w.contains("pid"), "{w}");
    }

    /// A probe that failed must carry the reason and still warn: the trap is
    /// the same, and claiming to have looked when we could not is how the next
    /// message stops being believed.
    #[test]
    fn a_probe_that_could_not_answer_says_so_and_still_warns() {
        let w = before_run(&Lock::Unknown("/tmp/x: Permission denied".into()));
        assert!(w.contains("Permission denied"), "{w}");
        assert!(w.contains("wineserver -w"), "{w}");
    }

    /// The exit message may not imply the game will now use the data — that is
    /// a different question, and nothing here has answered it.
    #[test]
    fn the_yield_message_claims_only_that_the_launch_can_proceed() {
        let m = yielding(1_140_518);
        assert!(m.contains("pid 1140518"), "{m}");
        assert!(m.contains("does not make the game accept the data"), "{m}");
    }

    /// A prefix that does not exist cannot be stat'ed, and that is an error
    /// with the path in it rather than a lock path built from nothing.
    #[test]
    fn a_prefix_that_is_not_there_has_no_lock_path() {
        let missing = std::env::temp_dir().join("tobii-no-such-prefix-ever");
        let e = lock_for(&missing).expect_err("no prefix");
        assert!(e.contains("tobii-no-such-prefix-ever"), "{e}");
    }

    /// An unlocked file reads as free, and a missing one reads as free too —
    /// not as an error, because a prefix nothing has ever served has no lock
    /// file at all.
    #[test]
    fn an_unheld_or_absent_lock_reads_as_free() {
        let dir = std::env::temp_dir().join(format!("tobii-lock-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let f = dir.join("lock");
        std::fs::write(&f, b"").expect("lock file");
        assert_eq!(probe(&f), Lock::Free);
        assert_eq!(probe(&dir.join("absent")), Lock::Free);
        std::fs::remove_dir_all(&dir).ok();
    }
}
