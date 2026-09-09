//! Downloading a release and swapping the binaries in place.
//!
//! # What the checksum actually buys, and what it does not
//!
//! **There is no trust anchor here beyond TLS to GitHub.** `SHA256SUMS` is
//! fetched over the same connection, from the same release, by the same code as
//! the archive it describes. So it detects a *corrupted or truncated download*
//! and nothing else: anyone who can serve the release JSON can serve a hostile
//! archive together with its true digest, and every check in this file will
//! pass.
//!
//! This is worth stating plainly because an earlier version of this file
//! asserted the opposite — "nothing is put where it will be executed until it
//! has been verified", "an unverified binary from the network is exactly the
//! thing this module exists to avoid running" — which would lead the next
//! person to believe there is a boundary here that has never existed. Making
//! this safe against a compromised release account needs a signature over the
//! archive, checked against a public key compiled into this binary. There is no
//! such key, so **installing an update trusts the GitHub release exactly as
//! much as downloading it by hand and running it would.**
//!
//! What this file *does* provide is worth having on its own terms, and it is
//! all about failure rather than malice:
//!
//! * every fetch is over HTTPS to a GitHub host, bounded and timed out
//!   ([`crate::net`]);
//! * a truncated or corrupted download is caught before anything is unpacked;
//! * the new binaries are **run once** before they are installed, so a build
//!   made against a newer glibc fails while the old binaries are still in
//!   place, instead of leaving nothing on the machine that starts;
//! * the swap keeps a backup of each binary and **rolls every one of them back**
//!   if any step fails, so a partial update cannot leave a mismatched pair;
//! * each individual replacement is a rename, which is atomic.

use std::path::{Component, Path, PathBuf};

use tobii_config::sha256;

use crate::net;
use crate::release::Release;

/// The binaries this project installs. A release archive may carry more; only
/// these are replaced, and only where one is already installed.
pub const BINARIES: [&str; 2] = ["tobii", "tobii-gtk"];

/// How long a newly downloaded binary gets to answer `--version`.
///
/// It loads the dynamic linker and prints one line, so this is generous. The
/// point of the limit is that a hung probe must not hang the GUI's install
/// thread for the life of the process.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The target triple this build was made for.
///
/// Assembled from what the standard library knows rather than from a build
/// script: the release archives are named with the same triple, and the ABI is
/// the only part that has to be assumed.
pub struct Target;

impl Target {
    pub fn triple() -> String {
        let abi = if cfg!(target_env = "musl") {
            "musl"
        } else {
            "gnu"
        };
        format!(
            "{}-unknown-{}-{abi}",
            std::env::consts::ARCH,
            std::env::consts::OS
        )
    }
}

#[derive(Debug)]
pub enum InstallError {
    /// The release has no archive built for this machine.
    NoBuildForTarget(String),
    /// The release publishes no `SHA256SUMS`.
    ///
    /// Fatal on purpose — not because the checksum proves the archive is
    /// trustworthy (see the module docs; it does not), but because a release
    /// without one cannot be told apart from a truncated download, and this
    /// project's own `scripts/release.sh` always publishes it.
    NoChecksums,
    /// The archive is not the file the checksums describe.
    Digest {
        name: String,
        expected: String,
        found: String,
    },
    /// `SHA256SUMS` does not list the archive.
    NotListed(String),
    /// The asset name is not a plain file name.
    ///
    /// Asset names come out of the release JSON, and were used to build the
    /// download path directly. `Path::join` replaces the whole path when what
    /// it is given is absolute, so an asset named `/etc/cron.d/x` wrote there
    /// instead of into the scratch directory — before any checksum was
    /// computed, because the download has to happen first.
    UnsafeName(String),
    Download(String),
    /// `tar` is missing, or the archive did not unpack.
    Unpack(String),
    /// The archive contained none of the binaries this project installs.
    NothingToInstall,
    /// The archive is missing a binary that is installed here.
    ///
    /// Refused rather than partly applied: replacing one of a pair and leaving
    /// the other at the old version is the mismatch the whole rollback path
    /// exists to prevent, and it would otherwise be reported as a success.
    Incomplete {
        missing: String,
    },
    /// A binary from the archive would not run on this machine.
    ///
    /// Caught before the installed binaries are touched. The usual cause is a
    /// release built against a newer glibc than this machine has.
    WillNotRun {
        name: String,
        detail: String,
    },
    /// The directory the binaries live in cannot be written to.
    NotWritable(PathBuf),
    /// The swap failed and the previous binaries were put back.
    RolledBack {
        detail: String,
    },
    Io(std::io::Error),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::NoBuildForTarget(t) => {
                write!(f, "this release has no build for {t}")
            }
            InstallError::NoChecksums => write!(
                f,
                "this release publishes no SHA256SUMS, so a truncated download could not be \
                 told from a complete one — refusing to install it"
            ),
            InstallError::Digest {
                name,
                expected,
                found,
            } => write!(
                f,
                "{name} does not match its published checksum\n  expected {expected}\n  \
                 got      {found}\nThe download was corrupted, or the release was changed \
                 after it was published. Nothing was installed."
            ),
            InstallError::NotListed(n) => {
                write!(f, "SHA256SUMS does not list {n}, so it cannot be checked")
            }
            InstallError::UnsafeName(n) => write!(
                f,
                "the release lists an asset named {n:?}, which is not a plain file name — \
                 refusing to download it"
            ),
            InstallError::Download(e) => write!(f, "download failed: {e}"),
            InstallError::Unpack(e) => write!(f, "could not unpack the release: {e}"),
            InstallError::NothingToInstall => write!(
                f,
                "the release archive contained none of this project's binaries"
            ),
            InstallError::Incomplete { missing } => write!(
                f,
                "the release archive is missing {missing}, which is installed here — \
                 installing the rest would leave versions that do not match, so nothing \
                 was changed"
            ),
            InstallError::WillNotRun { name, detail } => write!(
                f,
                "the downloaded {name} does not run on this machine, so it was not \
                 installed and nothing was changed:\n  {detail}\nThis usually means the \
                 release was built against newer system libraries than this machine has. \
                 Building from source will work."
            ),
            InstallError::NotWritable(p) => write!(
                f,
                "{} cannot be written to, so the update cannot be installed there. \
                 Install it by hand, or re-run with the permission to write there.",
                p.display()
            ),
            InstallError::RolledBack { detail } => write!(
                f,
                "the update could not be completed and the previous version was put \
                 back:\n  {detail}"
            ),
            InstallError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<std::io::Error> for InstallError {
    fn from(e: std::io::Error) -> Self {
        InstallError::Io(e)
    }
}

impl From<net::NetError> for InstallError {
    fn from(e: net::NetError) -> Self {
        InstallError::Download(e.to_string())
    }
}

/// What an install replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub dir: PathBuf,
    /// Binary names actually replaced, in the order they were written.
    pub replaced: Vec<String>,
    /// The version the installed binaries now report.
    ///
    /// Read back from a binary rather than taken from the release tag: the tag
    /// is what the release *claims*, and the two have disagreed before (a tag
    /// pushed without bumping `Cargo.toml` leaves a build that offers itself
    /// its own update forever). This is what the user will actually get.
    pub version: String,
}

/// The directory the running binaries live in.
pub fn install_dir() -> Result<PathBuf, InstallError> {
    let exe = std::env::current_exe()?;
    Ok(exe.parent().unwrap_or(Path::new(".")).to_path_buf())
}

/// Whether `dir` looks like a Cargo build directory.
///
/// Worth saying out loud before replacing anything: in a source checkout the
/// next `cargo build` overwrites whatever is installed here, so the update
/// would silently disappear. It is not an error — somebody may genuinely want
/// to try a release build in place — but it should never be a surprise.
pub fn is_build_tree(dir: &Path) -> bool {
    let mut parts = dir.iter().rev();
    matches!(
        parts.next().and_then(|s| s.to_str()),
        Some("release" | "debug")
    ) && matches!(parts.next().and_then(|s| s.to_str()), Some("target"))
}

/// Whether a new file can be created in `dir`.
///
/// Tested by creating one, because the permission bits alone do not account for
/// a read-only mount or the directory being owned by root.
pub fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".tobii-update-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Whether `name` is a plain file name, safe to join onto a directory.
///
/// Asset names are attacker-controlled in the same sense the rest of the
/// release is: they come out of the JSON. `Path::join` with an absolute path
/// discards the directory entirely, and `..` climbs out of it, so this is
/// checked before the name is ever joined.
pub fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('\0')
        && Path::new(name)
            .components()
            .try_fold(0usize, |n, c| match c {
                Component::Normal(_) => Some(n + 1),
                _ => None,
            })
            == Some(1)
}

/// The digest `SHA256SUMS` lists for `name`.
///
/// The format is coreutils': `<hex>  <name>`, one per line. A leading `*`
/// (binary mode) is accepted, and paths are compared by file name so an entry
/// written as `./dist/x.tar.gz` still matches.
pub fn digest_for(sums: &str, name: &str) -> Option<String> {
    for line in sums.lines() {
        let mut it = line.split_whitespace();
        let (Some(hex), Some(file)) = (it.next(), it.next()) else {
            continue;
        };
        let file = file.strip_prefix('*').unwrap_or(file);
        let base = file.rsplit('/').next().unwrap_or(file);
        if base == name && hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hex.to_ascii_lowercase());
        }
    }
    None
}

/// Download, verify and install `release`, replacing the binaries in place.
///
/// `progress` is called with a short line per step, so a CLI can print it and a
/// GUI can show it without this module knowing about either.
///
/// See the module docs for what "verify" does and does not mean here.
pub fn install_release(
    release: &Release,
    progress: &dyn Fn(&str),
) -> Result<Installed, InstallError> {
    let triple = Target::triple();
    let archive = release
        .archive_for(&triple)
        .ok_or_else(|| InstallError::NoBuildForTarget(triple.clone()))?;
    let sums_asset = release.checksums().ok_or(InstallError::NoChecksums)?;
    if !is_plain_file_name(&archive.name) {
        return Err(InstallError::UnsafeName(archive.name.clone()));
    }

    let dir = install_dir()?;
    if !is_writable(&dir) {
        return Err(InstallError::NotWritable(dir));
    }

    // A scratch directory beside the install, so the final move is a rename on
    // the same filesystem rather than a copy that can half-finish.
    let work = dir.join(format!(".tobii-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let result = install_inner(release, archive, sums_asset, &dir, &work, progress);
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn install_inner(
    release: &Release,
    archive: &crate::release::Asset,
    sums_asset: &crate::release::Asset,
    dir: &Path,
    work: &Path,
    progress: &dyn Fn(&str),
) -> Result<Installed, InstallError> {
    progress(&format!("downloading {}", archive.name));
    // Safe to join: `install_release` checked the name is a single component.
    let archive_path = work.join(&archive.name);
    net::download(&archive.url, &archive_path)?;

    progress("checking the download");
    let sums = net::get(&sums_asset.url)?;
    let sums = String::from_utf8_lossy(&sums);
    let expected = digest_for(&sums, &archive.name)
        .ok_or_else(|| InstallError::NotListed(archive.name.clone()))?;

    let mut installed = install_verified_archive(&archive_path, &expected, dir, work, progress)?;
    if installed.version.is_empty() {
        installed.version = release.version.to_string();
    }
    Ok(installed)
}

/// Check a downloaded archive and install what is in it.
///
/// Split from [`install_inner`] so everything after the download — the digest
/// check, the unpack, the symlink refusal, the runnability probe, and the
/// rollback — can be exercised against a real archive built from real
/// binaries, without a network or a published release. That is most of the
/// risk in this file, and it had never once been run end to end.
///
/// `expected` is the lower-case hex digest the release published for this file.
pub fn install_verified_archive(
    archive_path: &Path,
    expected: &str,
    dir: &Path,
    work: &Path,
    progress: &dyn Fn(&str),
) -> Result<Installed, InstallError> {
    let name = archive_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("the archive")
        .to_string();
    let found = sha256::hex_digest(&std::fs::read(archive_path)?);
    if found != expected.to_ascii_lowercase() {
        return Err(InstallError::Digest {
            name,
            expected: expected.to_string(),
            found,
        });
    }

    progress("unpacking");
    let unpacked = work.join("unpacked");
    std::fs::create_dir_all(&unpacked)?;
    let out = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive_path)
        .arg("-C")
        .arg(&unpacked)
        // Ownership in the archive is the maintainer's, and means nothing here.
        .arg("--no-same-owner")
        .output()
        .map_err(|e| InstallError::Unpack(format!("tar could not be run: {e}")))?;
    if !out.status.success() {
        return Err(InstallError::Unpack(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }

    // Stage every binary first, and run each one, before anything installed is
    // touched — so a release that ships half of them, or one that cannot run
    // here at all, changes nothing.
    progress("checking the new binaries");
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    // Binaries installed here that the archive does not carry. Installing only
    // the ones it happens to carry leaves a new `tobii` beside an old
    // `tobii-gtk` — the exact mismatched pair the backups and rollback exist to
    // prevent, arriving by the front door and reported as success.
    let mut missing: Vec<&str> = Vec::new();
    let discard = |staged: &[(PathBuf, PathBuf)]| {
        for (s, _) in staged {
            let _ = std::fs::remove_file(s);
        }
    };
    for name in BINARIES {
        let target = dir.join(name);
        // Only replace what is already installed here.
        if !target.symlink_metadata().is_ok_and(|m| m.is_file()) {
            continue;
        }
        let Some(src) = find_regular_file(&unpacked, name) else {
            missing.push(name);
            continue;
        };
        let staging = dir.join(format!(".{name}.new-{}", std::process::id()));
        let _ = std::fs::remove_file(&staging);
        if let Err(e) = std::fs::copy(&src, &staging) {
            // A copy that failed part-way still created the file, and nothing
            // else would ever remove it: it lives in the install directory, not
            // the scratch directory that gets cleaned up.
            let _ = std::fs::remove_file(&staging);
            discard(&staged);
            return Err(e.into());
        }
        set_executable(&staging)?;
        if let Err(detail) = probe(&staging) {
            discard(&staged);
            let _ = std::fs::remove_file(&staging);
            return Err(InstallError::WillNotRun {
                name: name.to_string(),
                detail,
            });
        }
        staged.push((staging, target));
    }
    if staged.is_empty() {
        return Err(InstallError::NothingToInstall);
    }
    if !missing.is_empty() {
        discard(&staged);
        return Err(InstallError::Incomplete {
            missing: missing.join(", "),
        });
    }

    progress("installing");
    let replaced = swap_in(&staged)?;
    Ok(Installed {
        dir: dir.to_path_buf(),
        replaced,
        // Empty when the binaries do not report one; `install_inner` then falls
        // back to the release tag.
        version: installed_version(dir).unwrap_or_default(),
    })
}

/// Move every staged binary into place, or put everything back.
///
/// Two properties, and the second is why this is not just a loop of renames:
///
/// 1. **Each replacement is one atomic rename over the target.** The backup is
///    a *hard link*, made before the rename — not the target renamed aside — so
///    there is never a moment when the path does not exist. Renaming the target
///    away first left a window in which `tobii` was simply missing, while the
///    module docs claimed a single atomic rename over the running binary.
/// 2. **Either all of them land or none do.** Two renames are each atomic but
///    not atomic together, so a failure on the second would leave a new `tobii`
///    beside an old `tobii-gtk`. Every completed swap is undone if a later one
///    fails.
fn swap_in(staged: &[(PathBuf, PathBuf)]) -> Result<Vec<String>, InstallError> {
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new(); // (target, backup)

    for (staging, target) in staged {
        let backup = target.with_file_name(format!(
            ".{}.old-{}",
            target.file_name().and_then(|s| s.to_str()).unwrap_or("bin"),
            std::process::id()
        ));
        let _ = std::fs::remove_file(&backup);
        let step = std::fs::hard_link(target, &backup)
            // Rename over the running binary. On Linux this unlinks the old
            // directory entry rather than touching the inode, so a process
            // still executing from it keeps running — and because the backup is
            // a link to that same inode, the old binary is still reachable.
            .and_then(|_| std::fs::rename(staging, target));
        match step {
            Ok(()) => done.push((target.clone(), backup)),
            Err(e) => {
                // Undo the swaps that did land, newest first.
                let mut detail = e.to_string();
                // This entry needs no restore: the rename that would have
                // replaced `target` is precisely the one that failed, so it
                // still holds the old binary. Only the backup link is cleaned
                // up.
                //
                // It must NOT be `rename(backup, target)` here. The two are
                // hard links to the same inode, and POSIX says renaming one
                // onto the other "shall return successfully and perform no
                // other action" — so the rename reports success, changes
                // nothing, and leaves the backup behind forever. Caught by
                // `a_failed_swap_puts_every_binary_back`.
                let _ = std::fs::remove_file(&backup);

                // The ones that DID land are different: their target points at
                // the new inode and their backup at the old one, so a rename
                // genuinely restores. Newest first, and every failure reported
                // — this loop's error used to be the only one in the function
                // that was discarded.
                for (t, b) in done.iter().rev() {
                    if let Err(u) = std::fs::rename(b, t) {
                        detail = format!(
                            "{detail}; and {} could not be restored from {}: {u}",
                            t.display(),
                            b.display()
                        );
                    }
                }
                for (s, _) in staged {
                    let _ = std::fs::remove_file(s);
                }
                return Err(InstallError::RolledBack { detail });
            }
        }
    }

    // The names that landed, in the order they were written — `done` is that
    // order.
    let replaced = done
        .iter()
        .filter_map(|(t, _)| Some(t.file_name()?.to_str()?.to_string()))
        .collect();
    for (_, backup) in &done {
        let _ = std::fs::remove_file(backup);
    }
    Ok(replaced)
}

/// How many times to retry a spawn that failed with `ETXTBSY`.
///
/// See [`spawn_probe`]. Six tries with 20 ms between them covers a window that
/// is over as soon as the other thread's `execve` completes.
const ETXTBSY_TRIES: usize = 6;

/// Start the probe, retrying the one failure that is not the binary's fault.
///
/// A file that was *just written* cannot be executed while any process still
/// holds a write descriptor for it: Linux answers `execve` with `ETXTBSY`. The
/// descriptor here is our own, from `fs::copy` — and although `copy` closes it,
/// `fork`/`posix_spawn` on **another thread** duplicates every open descriptor
/// into the child, where it survives until that child's own `execve`. So a
/// concurrent spawn anywhere else in the process — GLib launching a helper, a
/// sibling test, the head-pose worker — pins our write fd open for a few
/// microseconds and our exec fails.
///
/// Measured before this retry existed: the seven end-to-end install tests, which
/// all copy-then-exec and run concurrently in one binary, failed 51 of 200 runs
/// (0 of 200 with `--test-threads=1`). And it failed in the worst possible way:
/// `probe` reported it as [`InstallError::WillNotRun`], whose message says the
/// release "was built against newer system libraries than this machine has" —
/// a confident wrong diagnosis, for exactly the failure these tests exist to
/// rule out.
///
/// Retrying is the right answer rather than serialising, because the race is not
/// confined to tests: `install_release` runs on a worker thread while the GTK
/// main loop is live and free to spawn whatever it likes.
fn spawn_probe(path: &Path) -> Result<std::process::Child, String> {
    let mut last = String::new();
    for attempt in 0..ETXTBSY_TRIES {
        match version_command(path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => return Ok(c),
            Err(e) if is_text_file_busy(&e) => {
                last = e.to_string();
                // Someone else's fork is holding our write descriptor. It goes
                // as soon as their execve does.
                std::thread::sleep(std::time::Duration::from_millis(20 * (attempt as u64 + 1)));
            }
            Err(e) => return Err(format!("it could not be started: {e}")),
        }
    }
    Err(format!("it could not be started: {last}"))
}

/// `<path> --version`, set up the way both callers below need it.
///
/// The display is taken out of the environment because a GUI binary must not
/// try to talk to the session's display just to say what version it is.
fn version_command(path: &Path) -> std::process::Command {
    let mut c = std::process::Command::new(path);
    c.arg("--version")
        .stdin(std::process::Stdio::null())
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");
    c
}

/// Whether an error is the kernel refusing to exec a file open for writing.
fn is_text_file_busy(e: &std::io::Error) -> bool {
    // `ExecutableFileBusy` is the named kind; the raw code is checked too
    // because the mapping has not always existed.
    e.kind() == std::io::ErrorKind::ExecutableFileBusy || e.raw_os_error() == Some(26)
}

/// Run a freshly downloaded binary to see whether it works on this machine.
///
/// `--version` is answered before either binary opens a device, reads config or
/// initialises GTK, so this is a test of the dynamic linker and nothing else —
/// which is the failure that matters. A release built against a newer glibc
/// exits at startup with "version `GLIBC_2.xx' not found", and without this
/// check that discovery happens *after* both binaries have been replaced, with
/// nothing left on the machine that starts.
///
/// It runs code that was just downloaded. That is not a new exposure: the same
/// bytes are about to be installed and run anyway, and this way it happens
/// while the old binary is still in place.
fn probe(path: &Path) -> Result<(), String> {
    let mut child = spawn_probe(path)?;

    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Err(e) => return Err(format!("it could not be run: {e}")),
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                // Read the pipe directly rather than `wait_with_output`, which
                // waits for the pipe to close — and a probed binary that
                // spawned a child holding it open would hang here, past the
                // deadline that was supposed to bound this whole function.
                let mut detail = String::new();
                if let Some(err) = child.stderr.take() {
                    use std::io::Read;
                    let mut buf = Vec::new();
                    let _ = err.take(8 * 1024).read_to_end(&mut buf);
                    detail = String::from_utf8_lossy(&buf).trim().to_string();
                }
                if detail.is_empty() {
                    detail = format!("it exited with {status}");
                }
                return Err(detail.lines().take(3).collect::<Vec<_>>().join("; "));
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("it did not answer --version and was stopped".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    }
}

/// The version the installed binaries report, asked of one of them.
fn installed_version(dir: &Path) -> Option<String> {
    for name in BINARIES {
        let p = dir.join(name);
        if !p.is_file() {
            continue;
        }
        let out = version_command(&p).output().ok()?;
        if !out.status.success() {
            continue;
        }
        // "<name> <version>"
        let line = String::from_utf8_lossy(&out.stdout);
        if let Some(v) = line.split_whitespace().nth(1) {
            return Some(v.to_string());
        }
    }
    None
}

/// The first *regular* file named `name` at or below `root`.
///
/// Symlinks are skipped, both as candidates and as directories to walk into.
/// `fs::copy` reads through a symlink, so an archive member that is a link to
/// `/etc/shadow` — or to any file outside the scratch directory — would have
/// been copied to the install directory and marked executable, installing the
/// contents of a file that was never in the archive.
fn find_regular_file(root: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            // `symlink_metadata` does not follow the link, so a symlinked
            // directory is not descended into and a symlinked file is not
            // treated as a candidate.
            let Ok(meta) = p.symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(p);
            } else if meta.is_file() && p.file_name().and_then(|s| s.to_str()) == Some(name) {
                return Some(p);
            }
        }
    }
    None
}

fn set_executable(p: &Path) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(p)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(p, perms)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory this test owns, removed on the way out.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let p = std::env::temp_dir().join(format!(
                "tobii-install-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_target_triple_is_the_one_release_archives_are_named_with() {
        let t = Target::triple();
        assert!(t.contains(std::env::consts::ARCH), "{t}");
        assert!(t.contains("linux"), "{t}");
        assert!(t.ends_with("-gnu") || t.ends_with("-musl"), "{t}");
    }

    #[test]
    fn checksums_are_read_in_the_coreutils_format() {
        let sums = "\
abc  ignored-too-short
0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  a.tar.gz
FEDCBA9876543210FEDCBA9876543210FEDCBA9876543210FEDCBA9876543210 *./dist/b.tar.gz
";
        assert_eq!(
            digest_for(sums, "a.tar.gz").as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        // Binary-mode marker, a path, and upper case all still match.
        assert_eq!(
            digest_for(sums, "b.tar.gz").as_deref(),
            Some("fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210")
        );
        assert_eq!(digest_for(sums, "missing.tar.gz"), None);
        assert_eq!(digest_for("", "a.tar.gz"), None);
        assert_eq!(digest_for("garbage", "a.tar.gz"), None);
    }

    /// A short or non-hex field must not be accepted as a digest: it would
    /// compare unequal and report corruption, hiding the real problem.
    #[test]
    fn a_malformed_digest_line_is_ignored_rather_than_used() {
        let sums = "zzzz567890abcdef0123456789abcdef0123456789abcdef0123456789abcdef  a.tar.gz";
        assert_eq!(digest_for(sums, "a.tar.gz"), None);
    }

    /// `Path::join` throws the directory away when handed an absolute path, so
    /// an asset name like `/etc/cron.d/x` used to be written there — and the
    /// download necessarily happens before the checksum can be computed.
    #[test]
    fn an_asset_name_that_is_not_a_plain_file_name_is_refused() {
        for bad in [
            "/etc/cron.d/tobii",
            "../../../home/u/.bashrc",
            "..",
            ".",
            "a/b.tar.gz",
            "./x.tar.gz",
            "",
        ] {
            assert!(!is_plain_file_name(bad), "{bad:?} must be refused");
        }
        for ok in [
            "tobii-linux-0.2.0-x86_64-unknown-linux-gnu.tar.gz",
            "SHA256SUMS",
            ".hidden",
        ] {
            assert!(is_plain_file_name(ok), "{ok:?} should be allowed");
        }
        // Why it matters, stated directly. This is the behaviour the guard
        // exists for, so clippy's warning about it is the point.
        #[allow(clippy::join_absolute_paths)]
        {
            let work = Path::new("/tmp/work");
            assert_eq!(work.join("/etc/passwd"), Path::new("/etc/passwd"));
        }
    }

    #[test]
    fn a_cargo_build_directory_is_recognised() {
        assert!(is_build_tree(Path::new("/home/u/proj/target/release")));
        assert!(is_build_tree(Path::new("/home/u/proj/target/debug")));
        assert!(!is_build_tree(Path::new("/usr/local/bin")));
        assert!(!is_build_tree(Path::new("/home/u/.local/bin")));
        assert!(!is_build_tree(Path::new("/home/u/target")));
    }

    #[test]
    fn writability_is_tested_by_writing_not_by_reading_permissions() {
        let dir = std::env::temp_dir();
        assert!(is_writable(&dir), "the temp dir should be writable");
        assert!(!is_writable(Path::new("/proc/self/nonexistent-subdir")));
        // The probe must not survive the check. Named for THIS process only:
        // scanning for any `.tobii-update-probe*` searched a directory shared
        // with every other program on the machine, so a leftover from an
        // unrelated run failed this test.
        let mine = dir.join(format!(".tobii-update-probe-{}", std::process::id()));
        assert!(!mine.exists(), "the probe file was left behind");
    }

    #[test]
    fn find_regular_file_searches_below_the_root() {
        let s = Scratch::new("find");
        let nested = s.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("tobii"), b"x").unwrap();
        assert_eq!(
            find_regular_file(s.path(), "tobii"),
            Some(nested.join("tobii"))
        );
        assert_eq!(find_regular_file(s.path(), "absent"), None);
    }

    /// The archive is unpacked with `tar`, which happily creates symlinks.
    /// `fs::copy` then reads *through* one — so a member named `tobii` pointing
    /// at a file outside the scratch directory would have installed that file's
    /// contents, mode 0755, having never been in the archive at all.
    #[test]
    fn a_symlinked_binary_in_the_archive_is_not_installed() {
        let s = Scratch::new("symlink");
        let outside = s.path().join("secret");
        std::fs::write(&outside, b"never in the archive").unwrap();
        let unpacked = s.path().join("unpacked");
        std::fs::create_dir_all(&unpacked).unwrap();
        std::os::unix::fs::symlink(&outside, unpacked.join("tobii")).unwrap();

        assert_eq!(
            find_regular_file(&unpacked, "tobii"),
            None,
            "a symlink must not be taken as the binary to install"
        );

        // A real file elsewhere in the tree is still found.
        let real = unpacked.join("d");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("tobii"), b"elf").unwrap();
        assert_eq!(
            find_regular_file(&unpacked, "tobii"),
            Some(real.join("tobii"))
        );
    }

    /// A symlinked *directory* must not be walked into either, or the same
    /// escape works one level down.
    #[test]
    fn the_walk_does_not_follow_symlinked_directories() {
        let s = Scratch::new("symdir");
        let outside = s.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("tobii"), b"x").unwrap();
        let unpacked = s.path().join("unpacked");
        std::fs::create_dir_all(&unpacked).unwrap();
        std::os::unix::fs::symlink(&outside, unpacked.join("d")).unwrap();
        assert_eq!(find_regular_file(&unpacked, "tobii"), None);
    }

    /// The reason the backups exist: two renames are each atomic, but not
    /// atomic together. A failure on the second must not leave a new `tobii`
    /// beside an old `tobii-gtk`.
    #[test]
    fn a_failed_swap_puts_every_binary_back() {
        let s = Scratch::new("rollback");
        let dir = s.path();
        std::fs::write(dir.join("tobii"), b"old-cli").unwrap();
        std::fs::write(dir.join("tobii-gtk"), b"old-gui").unwrap();

        let good = dir.join(".tobii.new-test");
        std::fs::write(&good, b"new-cli").unwrap();
        // The second staged file does not exist, so its rename fails — which is
        // what a full disk or a lost permission looks like at this point.
        let missing = dir.join(".tobii-gtk.new-missing");

        let staged = vec![(good, dir.join("tobii")), (missing, dir.join("tobii-gtk"))];
        let e = swap_in(&staged).expect_err("the second rename must fail");
        assert!(matches!(e, InstallError::RolledBack { .. }), "{e}");

        assert_eq!(std::fs::read(dir.join("tobii")).unwrap(), b"old-cli");
        assert_eq!(std::fs::read(dir.join("tobii-gtk")).unwrap(), b"old-gui");
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    /// The property the hard-link backup exists to give: the path a user (or a
    /// desktop launcher, or a running script) might exec is never absent, not
    /// even for the instant between taking a backup and putting the new binary
    /// in place. Renaming the target aside to make the backup left exactly that
    /// window open, while the module docs claimed a single atomic rename.
    #[test]
    fn the_binary_never_disappears_even_for_an_instant() {
        let s = Scratch::new("never-gone");
        let dir = s.path();
        let target = dir.join("tobii");
        std::fs::write(&target, b"old").unwrap();
        let staging = dir.join(".tobii.new-test");
        std::fs::write(&staging, b"new").unwrap();

        // Watch the path from another thread while the swap runs. Any single
        // observation of "not there" is a failure.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let missing = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let watcher = {
            let (stop, missing, target) = (stop.clone(), missing.clone(), target.clone());
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if !target.exists() {
                        missing.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            })
        };
        for _ in 0..200 {
            std::fs::write(&staging, b"new").unwrap();
            swap_in(&[(staging.clone(), target.clone())]).unwrap();
            std::fs::write(&target, b"old").unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        watcher.join().unwrap();

        assert_eq!(
            missing.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the binary was absent at least once during the swap"
        );
    }

    #[test]
    fn a_swap_that_succeeds_replaces_everything_and_keeps_no_backups() {
        let s = Scratch::new("swap");
        let dir = s.path();
        std::fs::write(dir.join("tobii"), b"old").unwrap();
        let staging = dir.join(".tobii.new-test");
        std::fs::write(&staging, b"new").unwrap();

        let replaced = swap_in(&[(staging, dir.join("tobii"))]).unwrap();
        assert_eq!(replaced, vec!["tobii".to_string()]);
        assert_eq!(std::fs::read(dir.join("tobii")).unwrap(), b"new");
        let hidden: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(hidden.is_empty(), "backup left behind: {hidden:?}");
    }

    /// The check that turns a bricked install into a refused one.
    #[test]
    fn a_binary_that_does_not_run_is_caught_before_anything_is_replaced() {
        let s = Scratch::new("probe");
        let broken = s.path().join("broken");
        // Not an ELF and not a valid interpreter line: exactly what a build for
        // the wrong architecture or a truncated copy looks like to execve.
        std::fs::write(&broken, b"\x7fELF-but-not-really").unwrap();
        set_executable(&broken).unwrap();
        assert!(probe(&broken).is_err(), "a non-executable file must fail");

        let script = s.path().join("ok");
        std::fs::write(&script, b"#!/bin/sh\necho 'tobii 9.9.9'\n").unwrap();
        set_executable(&script).unwrap();
        assert!(probe(&script).is_ok(), "a working binary must pass");

        let fails = s.path().join("exits-1");
        std::fs::write(&fails, b"#!/bin/sh\necho 'boom' >&2\nexit 1\n").unwrap();
        set_executable(&fails).unwrap();
        let e = probe(&fails).expect_err("a non-zero exit is a failure");
        assert!(e.contains("boom"), "the reason should be reported: {e}");
    }

    /// The version reported to the user comes from the binary that is now
    /// installed, not from the tag the release claimed.
    #[test]
    fn the_installed_version_is_read_back_from_the_binary() {
        let s = Scratch::new("version");
        let p = s.path().join("tobii");
        std::fs::write(&p, b"#!/bin/sh\necho 'tobii 1.2.3'\n").unwrap();
        set_executable(&p).unwrap();
        assert_eq!(installed_version(s.path()).as_deref(), Some("1.2.3"));

        let empty = Scratch::new("version-empty");
        assert_eq!(installed_version(empty.path()), None);
    }
}
