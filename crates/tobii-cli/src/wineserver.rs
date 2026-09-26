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

use std::ffi::OsString;
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
         If a launch — or anything else — does start waiting for this prefix while this\n\
         runs, this command stops itself and says so, so the wait can end.\n";
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

/// What to say on the way out when something is waiting on us.
///
/// Deliberately says nothing about whether the game will then *use* the data.
/// Stopping unblocks whatever is waiting; that is the entire claim.
///
/// It says nothing about *who* is waiting either, because nothing here has
/// asked. [`blocked_waiter`] matches a blocked request on our lock file and
/// never looks at the process that made it, and a Proton pre-launch
/// `wineserver -w` is indistinguishable from winetricks, `wineboot`, a
/// launcher between operations, or a user running `wineserver -w` by hand:
/// measured, all of them produce the identical `->` line. Standing down is
/// right for every one of them — telling the user a game is launching when one
/// is not is not.
pub fn yielding(pid: i32) -> String {
    // A request queued through an open file description has no pid the kernel
    // will name and reports `-1`; printing `pid -1` at somebody is worse than
    // saying nothing about who is waiting. Same rule [`before_run`] keeps for
    // the holder, for the same reason.
    let who = if pid > 0 {
        format!(" (pid {pid})")
    } else {
        String::new()
    };
    format!(
        "\nsomething is waiting for this prefix's wineserver to exit{who}, and this\n\
         command is part of what it is waiting for. A Proton launch does this before it\n\
         spawns the game, and so do winetricks and other prefix tools.\n\
         Stopping so it can go through.\n\
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

/// The inverse: a `major:minor` pair back into one `dev_t`, so a device read
/// out of `/proc/self/mountinfo` can be carried in the same `u64` a `stat`
/// would have produced and decoded again by the two functions above.
fn makedev(major: u32, minor: u32) -> u64 {
    let (major, minor) = (u64::from(major), u64::from(minor));
    ((major & 0xfff) << 8) | (minor & 0xff) | ((major & !0xfff) << 32) | ((minor & !0xff) << 12)
}

/// A mount point as `/proc/self/mountinfo` spells it, unescaped.
///
/// The kernel escapes space, tab, newline and backslash in that field as
/// `\\040`, `\\011`, `\\012` and `\\134` (`show_mountinfo` in `fs/proc_namespace.c`),
/// so a mount point with a space in it does not match the path under it until
/// the escapes are undone. Bytes rather than `char`s: a mount point is an
/// arbitrary byte string, and decoding one octal escape at a time into a
/// `String` would mangle every non-ASCII name.
fn unescaped(point: &str) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    let raw = point.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let octal = (raw[i] == b'\\')
            .then(|| raw.get(i + 1..i + 4))
            .flatten()
            .and_then(|d| std::str::from_utf8(d).ok())
            .filter(|d| d.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|d| u8::from_str_radix(d, 8).ok());
        match octal {
            Some(byte) => {
                out.push(byte);
                i += 4;
            }
            None => {
                out.push(raw[i]);
                i += 1;
            }
        }
    }
    OsString::from_vec(out)
}

/// How many components of `mount_point` cover `path`, or `None` if it does not
/// cover it at all.
///
/// Component-wise, never as a string prefix: `/tmpfoo` starts with the text of
/// `/tmp` without being anywhere underneath it, and picking that mount would
/// answer with another filesystem's device.
fn covers(mount_point: &Path, path: &Path) -> Option<usize> {
    let mut under = path.components();
    let mut depth = 0;
    for c in mount_point.components() {
        if under.next() != Some(c) {
            return None;
        }
        depth += 1;
    }
    Some(depth)
}

/// The device `/proc/locks` will print for a file — which is *not* the device
/// `stat` reports for it.
///
/// `/proc/locks` prints `MAJOR(inode->i_sb->s_dev):MINOR(...)`: the
/// *superblock's* device, one per mounted filesystem. `stat` is free to answer
/// something else, and on btrfs it does — `btrfs_getattr` overwrites `stat->dev`
/// with the subvolume's anonymous device, so every subvolume of one filesystem
/// reports a different `st_dev` while `/proc/locks` prints the one superblock
/// device for all of them. Measured here: `/`, `/home` and `/var/tmp` are three
/// subvolumes of one btrfs with `st_dev` 31, 53 and 57, and `/proc/locks` says
/// `00:1d` — device 29 — for a lock on any of them.
///
/// Comparing a `stat` device against that text therefore misses every waiter on
/// such a filesystem, silently: the stand-down never fires and the user's launch
/// sits there forever, having been promised in so many words that it would not.
/// And the lock file lives in `/tmp`, which is a directory on the root
/// filesystem on every install without a tmpfs `/tmp` — not an exotic case, just
/// whichever way `/tmp` happens to be set up.
///
/// Field 3 of `/proc/self/mountinfo` *is* that superblock device, in the same
/// `major:minor` notation, so the mount carrying the file is the thing to ask.
/// Pure, and takes the text rather than reading the file, for the reason
/// [`blocked_waiter`] does.
///
/// `path` must already be absolute and symlink-free; a mount point only covers
/// the paths under it once both are resolved.
///
/// None of this applies to [`lock_path`], which is built from `st_dev` and must
/// stay that way: wine names the server directory from its own `stat` of the
/// prefix, so the subvolume's anonymous device is the right answer *there*.
fn locks_dev(mountinfo: &str, path: &Path) -> Option<u64> {
    let mut best: Option<(usize, u64)> = None;
    for line in mountinfo.lines() {
        // `36 35 98:0 /mnt1 /mnt2 rw,noatime - ext3 /dev/root rw`: mount id,
        // parent id, major:minor, root within the filesystem, mount point.
        let mut f = line.split(' ');
        let (Some(ids), Some(point)) = (f.nth(2), f.nth(1)) else {
            continue;
        };
        let Some((maj, min)) = ids.split_once(':') else {
            continue;
        };
        let (Ok(maj), Ok(min)) = (maj.parse::<u32>(), min.parse::<u32>()) else {
            continue;
        };
        let Some(depth) = covers(Path::new(&unescaped(point)), path) else {
            continue;
        };
        // `>=`, so a later line wins a tie: mountinfo is in mount order, and a
        // mount stacked on another mount's point hides the one underneath.
        if best.is_none_or(|(d, _)| depth >= d) {
            best = Some((depth, makedev(maj, min)));
        }
    }
    best.map(|(_, dev)| dev)
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

/// `(dev, ino)` of the lock file as `/proc/locks` will name it, once it exists.
///
/// `None` while it does not: a prefix nothing has ever served has no lock file,
/// and our own wine creates it moments after it starts — so the caller asks
/// again rather than deciding once. `None` too if the mount cannot be found,
/// which is the honest answer: a device we had to guess at would either miss
/// every waiter or match somebody else's file.
///
/// The inode comes from `stat`; the device deliberately does not. See
/// [`locks_dev`] for why the two disagree, and what it costs when the wrong one
/// is compared.
pub fn lock_ids(lock: &Path) -> Option<(u64, u64)> {
    let md = std::fs::metadata(lock).ok()?;
    let resolved = std::fs::canonicalize(lock).ok()?;
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    Some((locks_dev(&mountinfo, &resolved)?, md.ino()))
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

    /// The same rule the holder gets: a waiter the kernel will not name is not
    /// given a pid. `blocked_waiter` really does return `-1` for a request
    /// queued through an open file description — the staged test below is one
    /// — so this is the value the message is handed, not a hypothetical.
    #[test]
    fn a_waiter_the_kernel_will_not_name_is_not_given_a_pid() {
        let m = yielding(-1);
        assert!(!m.contains("pid"), "{m}");
        assert!(m.contains("something is waiting"), "{m}");
    }

    /// Nothing here looked at the waiting process, so the message may not name
    /// it. A bare `wineserver -w`, winetricks or a launcher between operations
    /// produces the identical `->` line a Proton pre-launch does; standing down
    /// is right for all of them, and announcing a game launch is right only for
    /// the one we cannot pick out.
    #[test]
    fn the_yield_does_not_claim_to_know_a_game_is_launching() {
        let m = yielding(4242);
        assert!(!m.contains("a Proton launch is waiting"), "{m}");
        assert!(m.contains("winetricks"), "{m}");
        // The warning printed before the run makes the same promise in
        // advance, so it may not narrow the stand-down to a launch either.
        let w = before_run(&Lock::Free);
        assert!(w.contains("or anything else"), "{w}");
    }

    /// `/proc/locks` prints the *superblock's* device and `stat` need not
    /// agree. This is the btrfs shape measured on this machine, written out:
    /// one filesystem, three subvolumes, `st_dev` 31, 53 and 57 — and one
    /// device, 0:29, for every lock on any of them. A comparison built from
    /// `stat` matches none of those lines.
    #[test]
    fn the_device_comes_from_the_mount_and_not_from_stat() {
        let mountinfo = "\
32 2 0:29 /@ / rw,relatime shared:1 - btrfs /dev/nvme0n1p2 rw,subvolid=256\n\
57 32 0:29 /@home /home rw,relatime shared:47 - btrfs /dev/nvme0n1p2 rw,subvolid=257\n\
210 32 0:29 /@var-tmp /var/tmp rw,relatime shared:2 - btrfs /dev/nvme0n1p2 rw,subvolid=259\n\
99 32 0:54 / /tmp rw,nosuid,nodev shared:52 - tmpfs tmpfs rw,size=16221880k\n";
        let dev = |p| locks_dev(mountinfo, Path::new(p));
        assert_eq!(dev("/home/u/.wine"), Some(makedev(0, 29)));
        assert_eq!(dev("/var/tmp/pfx/lock"), Some(makedev(0, 29)));
        assert_eq!(dev("/srv/x"), Some(makedev(0, 29)));
        // The one place it is a tmpfs, which is why this bug hid: here the two
        // answers happen to be the same number.
        assert_eq!(
            dev("/tmp/.wine-1000/server-36-c4/lock"),
            Some(makedev(0, 54))
        );
    }

    /// The deepest mount that covers the path wins, and a name that merely
    /// starts with a mount point's text is not under it — `/tmpfoo` is not in
    /// `/tmp`, and answering with the tmpfs device for it would compare against
    /// an unrelated filesystem's locks.
    #[test]
    fn the_deepest_covering_mount_wins_and_a_text_prefix_is_not_one() {
        let mountinfo = "\
32 2 0:29 / / rw - btrfs /dev/sda1 rw\n\
99 32 0:54 / /tmp rw - tmpfs tmpfs rw\n\
120 99 0:77 / /tmp/nested rw - tmpfs tmpfs rw\n";
        let dev = |p| locks_dev(mountinfo, Path::new(p));
        assert_eq!(dev("/tmp/nested/deep/lock"), Some(makedev(0, 77)));
        assert_eq!(dev("/tmp/x/lock"), Some(makedev(0, 54)));
        assert_eq!(dev("/tmpfoo/lock"), Some(makedev(0, 29)));
        assert_eq!(dev("/"), Some(makedev(0, 29)));
    }

    /// A mount stacked on another mount's point hides the one underneath, and
    /// mountinfo is in mount order — so at equal depth the later line is the
    /// one whose device the kernel will print.
    #[test]
    fn a_mount_stacked_on_another_shadows_it() {
        let mountinfo = "\
32 2 0:29 / / rw - btrfs /dev/sda1 rw\n\
99 32 0:54 / /tmp rw - tmpfs tmpfs rw\n\
150 32 0:88 / /tmp rw - tmpfs tmpfs rw\n";
        assert_eq!(
            locks_dev(mountinfo, Path::new("/tmp/lock")),
            Some(makedev(0, 88))
        );
    }

    /// The kernel escapes space, tab, newline and backslash in the mount point
    /// field. Left escaped, a mount point with a space in it covers nothing,
    /// and the answer silently becomes the filesystem above it.
    #[test]
    fn an_escaped_mount_point_still_covers_its_own_files() {
        let mountinfo = "\
32 2 0:29 / / rw - btrfs /dev/sda1 rw\n\
77 32 0:44 / /mnt/my\\040disk\\011x rw - ext4 /dev/sdb1 rw\n";
        assert_eq!(
            locks_dev(mountinfo, Path::new("/mnt/my disk\tx/pfx/lock")),
            Some(makedev(0, 44))
        );
    }

    /// Lines that are not mountinfo lines, and a path on no mount at all,
    /// answer "no device" rather than panicking or guessing one.
    #[test]
    fn unreadable_mountinfo_has_no_device_rather_than_a_wrong_one() {
        assert_eq!(locks_dev("", Path::new("/tmp/lock")), None);
        assert_eq!(locks_dev("garbage\n\n1 2\n", Path::new("/tmp/lock")), None);
        assert_eq!(
            locks_dev(
                "99 32 nope / /tmp rw - tmpfs tmpfs rw\n",
                Path::new("/tmp/x")
            ),
            None
        );
    }

    /// `makedev` is the inverse of the two decoders, including for numbers too
    /// wide for the low bits — the encoding splits both of them in two, and
    /// getting that wrong would turn a correct device into a near miss.
    #[test]
    fn a_device_survives_the_trip_through_major_and_minor() {
        for (maj, min) in [(0, 29), (0, 54), (8, 2), (0x103, 7), (259, 1), (0, 0xfffff)] {
            let dev = makedev(maj, min);
            assert_eq!((dev_major(dev), dev_minor(dev)), (maj, min), "{maj}:{min}");
        }
        // And the two decoders already agreed with `stat` on the way in: 54 is
        // the tmpfs `st_dev` behind the measured `00:36` lines above.
        assert_eq!(makedev(0, 54), 54);
    }

    /// The device this module compares is the one `/proc/locks` really prints,
    /// checked against a real blocked waiter on every filesystem this machine
    /// will hand us — not against a fixture, because the fault this guards
    /// against is exactly a fixture agreeing with the code that wrote it.
    ///
    /// The lock file wine uses lives in `/tmp`, which is a tmpfs on this
    /// developer's machine and a directory on the root filesystem on plenty of
    /// others; on a tmpfs the two devices happen to be equal, which is why a
    /// run against `/tmp` alone proves nothing. So every directory here is
    /// staged in turn: the temporary directory, `/var/tmp`, and the one cargo
    /// just wrote this test binary into. Where any of them is a btrfs — three
    /// subvolumes of one here — a `stat`-derived device fails this test.
    #[test]
    fn the_device_compared_is_the_one_the_kernel_prints_for_a_real_waiter() {
        let build_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let candidates = [
            Some(std::env::temp_dir()),
            Some(PathBuf::from("/var/tmp")),
            build_dir,
        ];
        let mut checked = Vec::new();
        for dir in candidates.into_iter().flatten() {
            let Some(staged) = stage_a_blocked_waiter(&dir) else {
                continue;
            };
            let Staged {
                locks,
                dev,
                ino,
                stat_dev,
            } = staged;
            let (maj, min, pid) = kernel_waiter(&locks, ino).unwrap_or_else(|| {
                panic!("no blocked waiter for inode {ino} in /proc/locks:\n{locks}")
            });
            let dev = dev
                .unwrap_or_else(|| panic!("no device at all for a lock file in {}", dir.display()));
            assert_eq!(
                (dev_major(dev), dev_minor(dev)),
                (maj, min),
                "in {}: we compare {:x}:{:x}, the kernel prints {maj:x}:{min:x} \
                 (stat says {:x}:{:x})",
                dir.display(),
                dev_major(dev),
                dev_minor(dev),
                dev_major(stat_dev),
                dev_minor(stat_dev)
            );
            assert_eq!(
                blocked_waiter(&locks, dev, ino),
                Some(pid),
                "in {}, against:\n{locks}",
                dir.display()
            );
            checked.push(dir);
        }
        assert!(
            !checked.is_empty(),
            "no filesystem could be staged, so this test checked nothing"
        );
    }

    /// What a staged waiter left behind, gathered before anything is unlocked
    /// or removed.
    struct Staged {
        /// `/proc/locks` as it read while the waiter was blocked.
        locks: String,
        /// What this module would compare against it — `None` if it could not
        /// say, which is a failure and not a reason to skip the filesystem.
        dev: Option<u64>,
        ino: u64,
        /// What `stat` says, for the failure message: on a filesystem where
        /// the two agree this test cannot fail for its own reason, and the
        /// message should be able to say so.
        stat_dev: u64,
    }

    /// One `F_OFD_*` call on byte 0 — the single byte wine locks.
    fn ofd(fd: i32, cmd: i32, ty: i32) -> i32 {
        let mut fl: libc::flock = unsafe { std::mem::zeroed() };
        fl.l_type = ty as libc::c_short;
        fl.l_whence = libc::SEEK_SET as libc::c_short;
        fl.l_start = 0;
        fl.l_len = 1;
        // SAFETY: `fd` is open for the whole call and `fl` is a live, fully
        // initialised `flock`. `l_pid` is zero, which `F_OFD_*` requires.
        unsafe { libc::fcntl(fd, cmd, &fl) }
    }

    /// Put a genuinely blocked waiter on a lock file in `dir` and read what the
    /// kernel says about it. `None` if `dir` cannot hold one.
    ///
    /// Two open file descriptions of one file, in this process. An OFD lock
    /// belongs to the *description* rather than to the process, so the second
    /// request really does queue behind the first — which is how a blocked
    /// waiter is staged at all from inside a threaded test binary, where a
    /// `fork` is not safe and a second POSIX lock from the same process would
    /// simply be granted. The kernel prints such a waiter with no pid, `-1`,
    /// which is the same `-1` the message test above refuses to print.
    ///
    /// Everything is unlocked, joined and removed *before* the caller asserts
    /// anything: a panic between the block and the unlock would leave a thread
    /// stuck in `F_OFD_SETLKW` for the life of the test binary.
    fn stage_a_blocked_waiter(dir: &Path) -> Option<Staged> {
        let dir = dir.join(format!("tobii-wineserver-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("lock");
        let open = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
        };
        let staged = (|| {
            let holder = open().ok()?;
            if ofd(holder.as_raw_fd(), libc::F_OFD_SETLK, libc::F_WRLCK) != 0 {
                return None;
            }
            let waiter = open().ok()?;
            let ino = std::fs::metadata(&path).ok()?.ino();
            let fd = waiter.as_raw_fd();
            let blocked = std::thread::spawn(move || ofd(fd, libc::F_OFD_SETLKW, libc::F_WRLCK));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let locks = loop {
                let locks = std::fs::read_to_string("/proc/locks").unwrap_or_default();
                if kernel_waiter(&locks, ino).is_some() || std::time::Instant::now() > deadline {
                    break locks;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            let staged = Staged {
                locks,
                dev: lock_ids(&path).map(|(dev, _)| dev),
                ino,
                stat_dev: std::fs::metadata(&path).ok()?.dev(),
            };
            // Let the waiter through and take its thread with us, so nothing
            // is left blocked in the kernel when this test binary moves on.
            ofd(holder.as_raw_fd(), libc::F_OFD_SETLK, libc::F_UNLCK);
            blocked.join().ok()?;
            drop(waiter);
            Some(staged)
        })();
        std::fs::remove_dir_all(&dir).ok();
        staged
    }

    /// The `major`, `minor` and pid the kernel itself printed for a blocked
    /// request on `ino` — the ground truth the module's own parse is measured
    /// against, read out of the text independently of [`blocked_waiter`].
    fn kernel_waiter(locks: &str, ino: u64) -> Option<(u32, u32, i32)> {
        locks.lines().find_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let [_, "->", _, _, _, pid, ids, ..] = f[..] else {
                return None;
            };
            let mut ids = ids.split(':');
            let maj = u32::from_str_radix(ids.next()?, 16).ok()?;
            let min = u32::from_str_radix(ids.next()?, 16).ok()?;
            (ids.next()?.parse::<u64>().ok()? == ino).then_some((maj, min, pid.parse().ok()?))
        })
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
