//! Where the daemon's socket lives, and what to do when one is already there.

use std::io;
use std::path::PathBuf;

/// Directory name under the runtime dir.
const DIR_NAME: &str = "tobii-linux";

/// Socket file name.
const SOCK_NAME: &str = "tracker.sock";

/// The daemon's socket path.
///
/// `$XDG_RUNTIME_DIR/tobii-linux/tracker.sock`, falling back to
/// `/run/user/<uid>/…` and then `/tmp/tobii-linux-<uid>/…`.
///
/// The runtime directory rather than the config directory, because a socket is
/// not configuration: the runtime dir is per-user, on tmpfs, already mode 0700,
/// and cleared at logout. A socket under `$XDG_CONFIG_HOME` would survive
/// reboots as a stale file that every later start had to reason about.
pub fn socket_path() -> PathBuf {
    socket_dir().join(SOCK_NAME)
}

/// The directory holding the socket.
pub fn socket_dir() -> PathBuf {
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !rt.is_empty() {
            return PathBuf::from(rt).join(DIR_NAME);
        }
    }
    let uid = current_uid();
    let run_user = PathBuf::from(format!("/run/user/{uid}"));
    if run_user.is_dir() {
        return run_user.join(DIR_NAME);
    }
    // Last resort. Namespaced by uid because /tmp is shared, and created 0700
    // by `ensure_socket_dir` so another user cannot sit in the path.
    PathBuf::from(format!("/tmp/{DIR_NAME}-{uid}"))
}

/// This process's real user id, read without pulling in `libc`.
///
/// `/proc/self/status` is a stable Linux interface and this crate is
/// deliberately dependency-free. A failure here only affects the fallback
/// paths, so it degrades to uid 0's spelling rather than refusing to run.
fn current_uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|rest| rest.split_whitespace().next().map(str::to_string))
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Create the socket directory, owner-only.
pub fn ensure_socket_dir() -> io::Result<PathBuf> {
    let dir = socket_dir();
    std::fs::create_dir_all(&dir)?;
    set_owner_only(&dir)?;
    Ok(dir)
}

/// Restrict a directory to its owner (mode 0700).
fn set_owner_only(dir: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(dir)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(dir, perms)
}

/// Longest usable socket path.
///
/// `sockaddr_un.sun_path` is 108 bytes on Linux and must hold a terminating
/// NUL, so 107 characters is the real ceiling. Nothing in the default layout
/// comes close (`/run/user/1000/tobii-linux/tracker.sock` is 39), but an unusual
/// `XDG_RUNTIME_DIR` can, and the kernel's own complaint is
/// "path must be shorter than SUN_LEN" — which names neither the path nor the
/// limit nor which of several paths it meant.
pub const MAX_SOCKET_PATH: usize = 107;

/// Reject a socket path the kernel could not use, with an error that says why.
pub fn check_socket_path(path: &std::path::Path) -> io::Result<()> {
    let len = path.as_os_str().as_encoded_bytes().len();
    if len > MAX_SOCKET_PATH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path is {len} bytes, over the {MAX_SOCKET_PATH}-byte kernel limit: {} \
                 — set XDG_RUNTIME_DIR to something shorter",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// What to do about a socket path that already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindAction {
    /// Nothing in the way — bind it.
    Bind,
    /// A leftover file from a daemon that died; remove it and bind.
    Unlink,
    /// Something is listening. Do not touch it.
    AlreadyRunning,
}

/// Decide what to do when `bind` fails, given whether a probe connection to the
/// existing socket succeeded.
///
/// The distinction matters and cannot be made from the filesystem alone: a Unix
/// socket file outlives the process that made it, so "the file exists" says
/// nothing about whether anyone is listening. Only trying to connect
/// distinguishes a live daemon from a corpse — and deleting a *live* daemon's
/// socket would leave it running but unreachable, with every client silently
/// failing to find it.
///
/// Pure, so the truth table is testable without binding anything.
pub fn stale_socket_action(bind_err: io::ErrorKind, probe_connected: bool) -> BindAction {
    match bind_err {
        io::ErrorKind::AddrInUse if probe_connected => BindAction::AlreadyRunning,
        io::ErrorKind::AddrInUse => BindAction::Unlink,
        _ => BindAction::Bind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_lives_under_the_runtime_dir_when_one_is_set() {
        // The test process inherits a real XDG_RUNTIME_DIR on a normal session.
        if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
            if !rt.is_empty() {
                let p = socket_path();
                assert!(p.starts_with(PathBuf::from(rt)), "{p:?}");
                assert!(p.ends_with("tobii-linux/tracker.sock"), "{p:?}");
            }
        }
    }

    #[test]
    fn the_socket_is_always_named_consistently() {
        assert!(socket_path().ends_with("tobii-linux/tracker.sock"));
        assert_eq!(socket_path().parent(), Some(socket_dir().as_path()));
    }

    #[test]
    fn the_uid_is_readable_and_matches_the_runtime_dir_convention() {
        let uid = current_uid();
        // /run/user/<uid> is the standard spelling; if it exists it should be
        // the uid we just read.
        if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
            let rt = rt.to_string_lossy().to_string();
            if let Some(suffix) = rt.strip_prefix("/run/user/") {
                assert_eq!(suffix, uid.to_string(), "uid disagrees with {rt}");
            }
        }
    }

    /// A Unix socket file outlives its process, so existence alone proves
    /// nothing. Only the probe distinguishes a live daemon from a corpse.
    #[test]
    fn only_a_successful_probe_means_a_daemon_is_really_running() {
        assert_eq!(
            stale_socket_action(io::ErrorKind::AddrInUse, true),
            BindAction::AlreadyRunning
        );
        assert_eq!(
            stale_socket_action(io::ErrorKind::AddrInUse, false),
            BindAction::Unlink
        );
    }

    /// The kernel's own message names neither the path nor the limit, and a
    /// daemon that dies on it should say which path was too long.
    #[test]
    fn an_over_length_socket_path_is_rejected_with_an_actionable_message() {
        let long = std::path::PathBuf::from(format!("/tmp/{}/tracker.sock", "x".repeat(120)));
        let err = check_socket_path(&long).expect_err("must be rejected");
        let text = err.to_string();
        assert!(text.contains("107"), "must name the limit: {text}");
        assert!(
            text.contains("XDG_RUNTIME_DIR"),
            "must name the fix: {text}"
        );
    }

    /// The paths this actually produces must clear the limit comfortably, or
    /// the daemon would fail on a perfectly ordinary system.
    #[test]
    fn the_default_socket_path_is_well_within_the_limit() {
        assert!(check_socket_path(&socket_path()).is_ok());
        assert!(check_socket_path(std::path::Path::new(
            "/run/user/1000/tobii-linux/tracker.sock"
        ))
        .is_ok());
    }

    /// Any other bind failure is not about staleness. Unlinking on, say, a
    /// permission error would delete a file we had no business touching.
    #[test]
    fn a_bind_failure_that_is_not_addr_in_use_never_unlinks() {
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::NotFound,
            io::ErrorKind::Other,
        ] {
            for probed in [true, false] {
                assert_eq!(
                    stale_socket_action(kind, probed),
                    BindAction::Bind,
                    "{kind:?} probe={probed} must not unlink"
                );
            }
        }
    }
}
