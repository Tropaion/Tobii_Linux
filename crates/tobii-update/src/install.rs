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
    /// The running binary was deleted or replaced after it started, so there is
    /// nothing at its path for an update to replace.
    ReplacedWhileRunning,
    /// This copy was installed by a package manager, which owns it.
    ///
    /// Not a permissions problem, and emphatically not something to solve by
    /// re-running as root: writing over a `dpkg`- or `pacman`-owned file leaves
    /// the package database describing a file that is no longer there, and the
    /// next upgrade of the package silently reverts the update anyway.
    PackageManaged {
        manager: String,
        package: String,
    },
    /// A package manager could not be asked whether it owns this copy.
    ///
    /// Deliberately fatal. The alternative — treating a failed query as "nobody
    /// owns it" — reaches the swap through the check *failing*, which is the
    /// one way [`InstallError::PackageManaged`] can be bypassed by accident.
    OwnerUnknown {
        manager: String,
        why: String,
    },
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
            InstallError::PackageManaged { manager, package } => write!(
                f,
                "this copy was installed by {manager}, as the package {package}. \
                 Update it with {} rather than from here — overwriting a \
                 package-managed file leaves the package database wrong, and the next \
                 upgrade would revert the change.",
                // The tool that ANSWERED is not the tool a user updates with:
                // `dpkg -S` is how you ask, `apt` is how you upgrade. Telling
                // somebody to "update it with dpkg" is advice they cannot act
                // on.
                updater_for(manager)
            ),
            InstallError::OwnerUnknown { manager, why } => write!(
                f,
                "{manager} could not be asked whether it owns this copy ({why}), so the \
                 update was not installed. Overwriting a package-managed file leaves the \
                 package database wrong, and this cannot rule that out. Fix {manager}, or \
                 update through your package manager."
            ),
            // Not "re-run with the permission to write there", which it said
            // until 2026-09-11: in the hub that means running a GUI as root,
            // and under sudo HOME is /root, so every follow-on step goes wrong.
            InstallError::NotWritable(p) => write!(
                f,
                "{} is not writable by you, so the update cannot be installed there. \
                 Download the release archive from the releases page, unpack it, and run \
                 `sudo ./install.sh --system {}` in it.",
                p.display(),
                p.display()
            ),
            InstallError::ReplacedWhileRunning => write!(
                f,
                "this copy was replaced or removed while it was running, so there is \
                 nothing here to update. Quit it and start it again to run whichever \
                 version is installed now."
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

/// Which package manager owns a file, if any — or that we could not tell.
///
/// # Why the third answer exists
///
/// This used to return `Option`, and treated any non-zero exit as "nobody owns
/// it". That is one of three things a non-zero exit can mean. Reproduced with a
/// stub `dpkg` behaving like a broken database (`exit 2`, "unable to open
/// database"): `package_owner` returned `None`, which is the answer that lets
/// [`install_release`] fall through and overwrite a packaged binary — the exact
/// outcome [`InstallError::PackageManaged`] exists to prevent, arrived at
/// because the check failed rather than because it passed.
///
/// So a failure the manager did not choose is [`Ownership::Unknown`], and the
/// caller refuses. Refusing costs a message; proceeding costs a package
/// database that describes files which are no longer there.
///
/// # Why there is a deadline
///
/// `Command::output()` waits forever. Measured with a stub `dpkg` that sleeps
/// an hour: the call never returned and had to be killed from outside. Both
/// callers make that fatal — [`install_release`] runs on a thread holding a
/// `GApplication` hold that is only released on a terminal result, and
/// `tobii-diagnostics` calls this from the GTK main thread when the settings
/// popover's save or copy button is pressed. A package manager blocked on an
/// NFS stall or a stale lock would freeze the hub for the life of the process.
#[derive(Debug, Clone, PartialEq)]
pub enum Ownership {
    /// A package manager claims the file.
    Package { manager: String, package: String },
    /// Every manager that is installed ran and disclaimed it.
    None,
    /// At least one manager could not be asked, or failed in a way it does not
    /// use for "no match". Treated as ownership, because the alternative is
    /// overwriting a packaged file on the strength of a broken query.
    Unknown { manager: String, why: String },
}

/// How long any one package-manager query may take.
///
/// These are local database lookups that normally answer in tens of
/// milliseconds — measured here at 114 ms for a miss, worst of the three. Two
/// seconds is far outside that and still short enough not to be felt.
const OWNER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

pub fn package_owner(path: &Path) -> Ownership {
    let Some(p) = path.to_str() else {
        return Ownership::None;
    };
    /// A package manager, the query that asks it who owns a file, how to read
    /// the package name out of what it prints, and the exit code it uses for
    /// "no package owns this".
    struct Query<'a> {
        prog: &'static str,
        args: Vec<&'a str>,
        name_from: fn(&str) -> Option<String>,
        no_match: i32,
    }

    let queries = [
        // dpkg prints "tobii-linux: /usr/bin/tobii". It can also print
        // "diversion by <pkg> from: /usr/bin/tobii", where the first
        // colon-separated field is the words "diversion by <pkg> from" — which
        // the naive split takes for a package name. Exit 1 is "no path found",
        // exit 2 is a real error.
        Query {
            prog: "dpkg",
            args: vec!["-S", p],
            name_from: |o| {
                let line = o.lines().find(|l| !l.starts_with("diversion "))?;
                Some(line.split(':').next()?.trim().to_string())
            },
            no_match: 1,
        },
        // `--queryformat`, so this is the package NAME. Plain `rpm -qf` prints
        // the whole NEVRA — "tobii-linux-0.1.0-1.x86_64" — which is not what
        // you type at dnf and reads like a filename in the refusal message.
        // Exit 1 is "not owned by any package".
        Query {
            prog: "rpm",
            args: vec!["-qf", "--queryformat", "%{NAME}\\n", p],
            name_from: |o| Some(o.lines().next()?.trim().to_string()),
            no_match: 1,
        },
        // `-Qoq` prints the bare package name and nothing else. NOT `-Qo`,
        // whose output is a prose sentence — and a LOCALIZED one: on a German
        // system it reads "/usr/bin/ls ist in coreutils 9.11-2.1 enthalten",
        // where the first whitespace token is the path, not the package. Any
        // parse of that sentence is a parse of the user's language settings.
        // Verified on this machine: a miss exits 1.
        Query {
            prog: "pacman",
            args: vec!["-Qoq", p],
            name_from: |o| Some(o.lines().next()?.trim().to_string()),
            no_match: 1,
        },
    ];
    for q in queries {
        let spawned = std::process::Command::new(q.prog)
            .args(&q.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        let Ok(child) = spawned else {
            continue; // that package manager is not installed here
        };
        let out = match wait_bounded(child, OWNER_TIMEOUT) {
            Ok(out) => out,
            Err(why) => {
                return Ownership::Unknown {
                    manager: q.prog.to_string(),
                    why,
                }
            }
        };
        match out.status.code() {
            Some(0) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if let Some(pkg) = (q.name_from)(&stdout).filter(|s| !s.is_empty()) {
                    return Ownership::Package {
                        manager: q.prog.to_string(),
                        package: pkg,
                    };
                }
                // Exit 0 and nothing parseable: it answered, and we cannot read
                // the answer. That is not "nobody owns it".
                return Ownership::Unknown {
                    manager: q.prog.to_string(),
                    why: "it succeeded but printed nothing we could read".into(),
                };
            }
            Some(c) if c == q.no_match => continue, // it ran and disclaims the file
            Some(c) => {
                return Ownership::Unknown {
                    manager: q.prog.to_string(),
                    why: format!("it exited with {c}"),
                }
            }
            // Killed by a signal.
            None => {
                return Ownership::Unknown {
                    manager: q.prog.to_string(),
                    why: "it was killed".into(),
                }
            }
        }
    }
    Ownership::None
}

/// Who owns the binaries an install into `dir` would replace.
///
/// BOTH binaries, not just the first. A package can own one and not the other —
/// a half-replaced install, a distribution that splits the CLI from the GUI —
/// and overwriting the owned one is the damage this gate exists to prevent,
/// whichever of the two it is. So the first answer that is not
/// [`Ownership::None`] wins.
///
/// Public so a caller can ask *before* it offers a button that
/// [`install_release`] would only refuse. Nothing about the refusal rests on
/// that: `install_release` asks again, on its own thread, and refuses on its
/// own answer.
pub fn ownership_of(dir: &Path) -> Ownership {
    for name in BINARIES {
        match package_owner(&dir.join(name)) {
            Ownership::None => {}
            owned => return owned,
        }
    }
    Ownership::None
}

/// The command a person actually updates with, given the one that answered.
fn updater_for(query_tool: &str) -> &str {
    match query_tool {
        "dpkg" => "apt",
        "rpm" => "dnf",
        other => other, // pacman asks and upgrades with the same command
    }
}

/// Most of a child's output this will read, per pipe.
///
/// Bounded: a package manager — or a downloaded binary — that printed a
/// gigabyte would otherwise be read into memory in full.
const MAX_CHILD_OUTPUT: u64 = 256 * 1024;

/// Drain a finished child's pipe, up to [`MAX_CHILD_OUTPUT`] bytes.
fn read_capped<R: std::io::Read>(pipe: Option<R>) -> Vec<u8> {
    use std::io::Read;
    let mut buf = Vec::new();
    if let Some(r) = pipe {
        let _ = r.take(MAX_CHILD_OUTPUT).read_to_end(&mut buf);
    }
    buf
}

/// Wait for a child, killing it if it outstays `limit`.
///
/// The 25 ms `try_wait` loop every subprocess in this file needs, written once
/// so all three callers are bounded by construction rather than by remembering
/// to be. It used to say exactly that while `probe` kept its own copy.
///
/// The pipes are read *after* the child has exited, rather than with
/// `wait_with_output`, which waits for the pipe to close — a probed binary that
/// spawned a child holding it open would hang there, past the deadline that was
/// supposed to bound this.
fn wait_bounded(
    mut child: std::process::Child,
    limit: std::time::Duration,
) -> Result<std::process::Output, String> {
    let deadline = std::time::Instant::now() + limit;
    loop {
        match child.try_wait() {
            Err(e) => return Err(format!("it could not be run: {e}")),
            Ok(Some(status)) => {
                return Ok(std::process::Output {
                    status,
                    stdout: read_capped(child.stdout.take()),
                    stderr: read_capped(child.stderr.take()),
                });
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("it did not answer within {limit:?}"));
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    }
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

/// Where the running copy lives, and what can be done to it.
///
/// Read by the hub's update banner before it offers anything, so that the
/// button it shows is one that can work. Everything here is *read*: the hub
/// promises that nothing on disk changes until a button is pressed, and
/// [`is_writable`] proves writability by creating a file, so it is not used
/// here. [`install_release`] still makes that real attempt, and refuses on its
/// own answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    /// The directory the running binary was started from.
    pub dir: PathBuf,
    /// Who owns the binaries in it.
    pub owner: Ownership,
    /// Whether this user can replace them in place: the directory is writable
    /// by them AND every binary in it is theirs. The second half is not
    /// redundant. With `fs.protected_hardlinks` on — the kernel default, and 1
    /// on the machine this was found on — the swap's hard-linked backup of a
    /// file you do not own fails with EPERM even in a directory you can write,
    /// so a root-owned binary in ~/.local/bin got an Update that could only roll
    /// back.
    pub replaceable: bool,
    /// The running binary was deleted or replaced after it started.
    pub exe_gone: bool,
}

/// What the update banner offers for a [`Placement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Replace the binaries in place: the tarball install in ~/.local/bin.
    Update,
    /// A package manager owns this copy, so fetch the file it installs.
    DownloadPackage { manager: String, package: String },
    /// Nobody owns it, but it is somewhere this user cannot replace — a
    /// `sudo ./install.sh --system` install, or one copied by hand into a system
    /// directory. Fetch the archive and hand over the one command that installs
    /// it there.
    DownloadForSystem { dir: PathBuf },
    /// This copy was deleted or replaced while it ran. There is nothing here to
    /// update: quitting and starting again runs whatever is installed now.
    Restart,
}

/// The decision, kept apart from gathering its inputs so every case is tested.
///
/// Reported on 2026-09-11: a hub whose package had been removed while it kept
/// running offered Update, and after the click said "/usr/bin cannot be written
/// to … re-run with the permission to write there". Both halves were dead ends.
/// Nothing owned the deleted files any more, so the package check passed; and a
/// GUI cannot sensibly be re-run as root. The first case now asks for a restart
/// and the second offers a download, before anything is clicked.
pub fn action_for(p: &Placement) -> Action {
    if p.exe_gone {
        return Action::Restart;
    }
    match &p.owner {
        Ownership::Package { manager, package } => Action::DownloadPackage {
            manager: manager.clone(),
            package: package.clone(),
        },
        Ownership::None if !p.replaceable => Action::DownloadForSystem { dir: p.dir.clone() },
        // `Unknown` keeps Update on purpose: a package manager could not be
        // *asked*, which is not the same as one owning this copy, and
        // `install_release` refuses with the reason when it is pressed.
        Ownership::None | Ownership::Unknown { .. } => Action::Update,
    }
}

/// Gather a [`Placement`] for the running program.
///
/// `None` when its own path cannot be read — and then the banner keeps Update,
/// whose click reports that same failure.
pub fn placement() -> Option<Placement> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    Some(Placement {
        owner: ownership_of(&dir),
        replaceable: can_replace_in_place(&dir),
        exe_gone: exe_replaced_or_removed(&exe),
        dir,
    })
}

/// Whether `/proc/self/exe` says the running binary is no longer at its path.
///
/// The kernel appends " (deleted)" to the link once the file it names is
/// unlinked — by a package removal, or by any install that renames a new file
/// over it, which is how both `install.sh` and this updater replace a binary.
/// `current_exe` returns the link as it reads. Measured on this machine with a
/// copy of `sleep` deleted while it ran.
pub fn exe_replaced_or_removed(exe: &Path) -> bool {
    std::os::unix::ffi::OsStrExt::as_bytes(exe.as_os_str()).ends_with(b" (deleted)")
}

/// [`Placement::replaceable`], without writing anything.
fn can_replace_in_place(dir: &Path) -> bool {
    // SAFETY: geteuid takes no arguments and cannot fail.
    let euid = unsafe { libc::geteuid() };
    writable_by_me(dir)
        && BINARIES
            .iter()
            .all(|b| match std::fs::symlink_metadata(dir.join(b)) {
                Ok(m) => std::os::unix::fs::MetadataExt::uid(&m) == euid,
                // Not installed here — a --lean install has no tobii-gtk — so
                // nothing to replace and nothing to refuse.
                Err(_) => true,
            })
}

/// The kernel's own answer to "may I write here", with the effective ids, so it
/// counts read-only mounts and ACLs — and creates nothing.
fn writable_by_me(dir: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(dir.as_os_str()))
    else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated path that outlives the call, and
    // faccessat only reads it.
    unsafe { libc::faccessat(libc::AT_FDCWD, c.as_ptr(), libc::W_OK, libc::AT_EACCESS) == 0 }
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

/// Fetch `files` from `release` into a folder under `into`, checking each one
/// against the release's `SHA256SUMS`. Nothing is installed.
///
/// This is the path for a copy this program must not update itself: a
/// package-managed install has to be updated by its package manager, and a
/// package manager needs the file on disk. The download and the checksum check
/// are the installer's — see the module docs for what that check is worth — and
/// the last step, the one that writes to the install directory, is simply not
/// taken.
///
/// The files land in `<into>/tobii-linux-<version>/` rather than straight into
/// the folder the user picked. Two of the names this can fetch are `PKGBUILD`
/// and `tobii-linux.install`, generic enough that writing them into a folder an
/// Arch user keeps their own packaging in would overwrite theirs; the
/// subdirectory also keeps that pair together in one directory, which is what
/// `makepkg` needs.
///
/// Nothing this call wrote survives a failure, the file whose digest did not
/// match included. A `.deb` left in a Downloads folder is something a person
/// installs by hand later, and half a PKGBUILD pair does not build at all —
/// both are worse than an empty folder and a message.
pub fn download_release_files(
    release: &Release,
    files: &[&crate::release::Asset],
    into: &Path,
    progress: &dyn Fn(&str),
) -> Result<Vec<PathBuf>, InstallError> {
    if files.is_empty() {
        return Err(InstallError::NoBuildForTarget(Target::triple()));
    }
    for a in files {
        if !is_plain_file_name(&a.name) {
            return Err(InstallError::UnsafeName(a.name.clone()));
        }
    }
    let sums_asset = release.checksums().ok_or(InstallError::NoChecksums)?;

    progress("the checksums");
    let sums = net::get(&sums_asset.url)?;
    let sums = String::from_utf8_lossy(&sums).into_owned();
    // Every digest is looked up before anything is written, so a release that
    // lists one of these files and not another fails while the user's folder is
    // still untouched.
    for a in files {
        if digest_for(&sums, &a.name).is_none() {
            return Err(InstallError::NotListed(a.name.clone()));
        }
    }

    let dir = into.join(format!("tobii-linux-{}", release.version));
    std::fs::create_dir_all(&dir)?;
    let mut written = Vec::new();
    match fetch_each(
        files,
        &dir,
        &sums,
        progress,
        &|url, dest| net::download(url, dest),
        &mut written,
    ) {
        Ok(()) => Ok(written),
        Err(e) => {
            discard(&written, &dir);
            Err(e)
        }
    }
}
/// Undo a failed download: remove what this call wrote, and the directory if
/// it is now empty.
///
/// `remove_dir`, not `remove_dir_all`, so a folder the user chose cannot be
/// emptied of anything this call did not put there.
///
/// It does NOT protect an earlier, completed download of the same release: a
/// second attempt writes the same file names into the same
/// `tobii-linux-<version>` directory, so a failure part-way through deletes the
/// copies that were already there. Re-downloading is the only thing that
/// triggers it, and re-downloading is what a user does when the first attempt
/// looked wrong — so the honest statement is that this cleans up after itself,
/// not that it is safe around your files.
fn discard(written: &[PathBuf], dir: &Path) {
    for p in written {
        let _ = std::fs::remove_file(p);
    }
    let _ = std::fs::remove_dir(dir);
}

/// Fetch each file into `dir` and check it, recording every path written.
///
/// `fetch` is a parameter so the checking and the cleanup can be exercised
/// against known bytes without a network — that is the half of this worth
/// testing, and the half a real download cannot be made to fail on demand. The
/// only caller passes [`net::download`], which is what puts every URL through
/// [`net::is_trusted`].
///
/// `written` is an out-parameter rather than a return value because the caller
/// has to delete what was written *on the failure path*, when there is no
/// return value to read it out of.
fn fetch_each(
    files: &[&crate::release::Asset],
    dir: &Path,
    sums: &str,
    progress: &dyn Fn(&str),
    fetch: &dyn Fn(&str, &Path) -> Result<(), net::NetError>,
    written: &mut Vec<PathBuf>,
) -> Result<(), InstallError> {
    for a in files {
        progress(&a.name);
        // Safe to join: the caller checked every name is a single component.
        let dest = dir.join(&a.name);
        fetch(&a.url, &dest)?;
        written.push(dest.clone());
        let expected = digest_for(sums, &a.name).ok_or_else(|| {
            // Unreachable through `download_release_files`, which looks every
            // digest up before the first byte is fetched.
            InstallError::NotListed(a.name.clone())
        })?;
        let found = sha256::hex_digest(&std::fs::read(&dest)?);
        if found != expected {
            return Err(InstallError::Digest {
                name: a.name.clone(),
                expected,
                found,
            });
        }
    }
    Ok(())
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
    // A copy deleted or replaced while it ran has nothing at its path to update,
    // and pressing on reports whatever the next check trips over — "not
    // writable", for the orphaned package copy this was found with.
    if std::env::current_exe().is_ok_and(|e| exe_replaced_or_removed(&e)) {
        return Err(InstallError::ReplacedWhileRunning);
    }
    // Asked before writability, because the two failures need opposite advice:
    // "you need permission" invites `sudo`, which is exactly the wrong thing to
    // do to a package-managed file.
    match ownership_of(&dir) {
        Ownership::Package { manager, package } => {
            return Err(InstallError::PackageManaged { manager, package })
        }
        // Could not tell. Refuse: the alternative is overwriting a packaged
        // file on the strength of a query that failed.
        Ownership::Unknown { manager, why } => {
            return Err(InstallError::OwnerUnknown { manager, why })
        }
        Ownership::None => {}
    }
    if !is_writable(&dir) {
        return Err(InstallError::NotWritable(dir));
    }

    // Anything an earlier run left behind, first.
    //
    // Every scratch name carries the pid that made it, so a run that was killed
    // or panicked between staging and the swap leaves its files under a name no
    // later run ever revisits: the archive, the unpacked tree and an executable
    // copy of each binary — up to 73 MB — sitting in the install directory
    // forever. `remove_dir_all(&work)` below only ever cleans THIS run's.
    sweep_stale_scratch(&dir);

    // A scratch directory beside the install, so the final move is a rename on
    // the same filesystem rather than a copy that can half-finish.
    let work = dir.join(format!(".tobii-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let result = install_inner(release, archive, sums_asset, &dir, &work, progress);
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Delete scratch files left by a run that did not finish.
///
/// Matched by name, not by age: `.tobii-update-<pid>`,
/// `.tobii-update-probe-<pid>` and `.<binary>.new-<pid>` are this program's own
/// shapes, and nothing else in an install directory looks like them. The
/// current process's own files are skipped, since a second updater running
/// concurrently would otherwise delete the first's staging out from under it —
/// and a live pid is a poor signal here, because pids are reused.
fn sweep_stale_scratch(dir: &Path) {
    let me = format!("{}", std::process::id());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let Some(pid) = name
            .strip_prefix(".tobii-update-probe-")
            .or_else(|| name.strip_prefix(".tobii-update-"))
            .or_else(|| {
                BINARIES
                    .iter()
                    .find_map(|b| name.strip_prefix(&format!(".{b}.new-")))
            })
        else {
            continue;
        };
        if pid == me || pid.is_empty() || !pid.bytes().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let path = e.path();
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
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
/// See [`spawn_probe`]. Six tries, backing off 20 ms at a time — 20, 40, 60,
/// 80, 100, 120 ms — cover a window that is over as soon as the other thread's
/// `execve` completes. The figure is the measured budget for the race this
/// exists to survive, so a doc that said "20 ms between them" was inviting
/// somebody to tune the constant against the wrong number.
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
    let out = wait_bounded(spawn_probe(path)?, PROBE_TIMEOUT)?;
    if out.status.success() {
        return Ok(());
    }
    let mut detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if detail.is_empty() {
        detail = format!("it exited with {}", out.status);
    }
    Err(detail.lines().take(3).collect::<Vec<_>>().join("; "))
}

/// The version the installed binaries report, asked of one of them.
fn installed_version(dir: &Path) -> Option<String> {
    for name in BINARIES {
        let p = dir.join(name);
        if !p.is_file() {
            continue;
        }
        // Bounded, like every other subprocess in this crate. This ran the
        // just-installed, network-supplied binary with a bare `.output()` — no
        // deadline and no cap on what it reads — which is the one subprocess
        // here that was neither. A binary that answers `--version` during
        // `probe` (a staged copy) and then hangs afterwards would wedge the
        // install thread for the life of the process, holding a GApplication
        // hold, after the swap has already happened.
        let Ok(child) = version_command(&p)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            continue;
        };
        let Ok(out) = wait_bounded(child, PROBE_TIMEOUT) else {
            continue;
        };
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

    /// A release carrying just these assets; nothing else here reads the rest.
    fn release_with(assets: Vec<crate::release::Asset>) -> crate::release::Release {
        crate::release::Release {
            tag: "v9.9.9".into(),
            version: crate::version::Version::parse("v9.9.9").expect("a version"),
            notes: String::new(),
            assets,
            html_url: String::new(),
        }
    }

    /// The path-traversal guard runs before anything is fetched, and nothing
    /// else tests it — `download_release_files` had no test at all, so the
    /// check could be deleted with the whole suite still green.
    ///
    /// Offline by construction: the guard is ahead of `release.checksums()` and
    /// ahead of the first `net::get`, so this needs no network and no seam.
    #[test]
    fn a_download_refuses_an_asset_whose_name_is_a_path() {
        use crate::release::Asset;
        let dir = std::env::temp_dir().join(format!("tobii-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");

        for bad in ["../evil", "a/b", "/etc/passwd", ".."] {
            let asset = Asset {
                name: bad.to_string(),
                url: "https://example.invalid/x".to_string(),
            };
            let release = release_with(vec![asset.clone()]);
            let err = download_release_files(&release, &[&asset], &dir, &|_| {})
                .expect_err("a name that is a path must be refused");
            assert!(
                matches!(err, InstallError::UnsafeName(ref n) if n == bad),
                "{bad:?} gave {err:?}"
            );
        }
        assert!(
            std::fs::read_dir(&dir).expect("dir").next().is_none(),
            "nothing may be written before the name is checked"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Nothing to download is its own answer, not an empty success — a caller
    /// that got `Ok(vec![])` would report a finished download of no files.
    #[test]
    fn a_download_with_no_assets_says_there_is_no_build() {
        let dir = std::env::temp_dir().join(format!("tobii-dl-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let err = download_release_files(&release_with(Vec::new()), &[], &dir, &|_| {})
            .expect_err("an empty set must be refused");
        assert!(matches!(err, InstallError::NoBuildForTarget(_)), "{err:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
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

    /// The distinction that decides what advice the user gets. A
    /// package-managed install must NOT be told it needs permission, because
    /// that invites `sudo`, and writing over a dpkg- or pacman-owned file
    /// leaves the package database describing a file that is no longer there.
    #[test]
    fn a_package_managed_install_is_told_to_use_its_package_manager() {
        let e = InstallError::PackageManaged {
            manager: "pacman".into(),
            package: "tobii-linux".into(),
        };
        let msg = e.to_string();
        assert!(msg.contains("pacman"), "{msg}");
        assert!(msg.contains("tobii-linux"), "{msg}");
        // The one thing it must never suggest.
        assert!(!msg.to_lowercase().contains("sudo"), "{msg}");
        assert!(!msg.contains("permission"), "{msg}");

        // And the permissions message stays about permissions.
        let w = InstallError::NotWritable(PathBuf::from("/usr/bin")).to_string();
        assert!(!w.contains("re-run with the permission"), "{w}");
        assert!(w.contains("sudo ./install.sh --system /usr/bin"), "{w}");
    }

    /// A killed or panicking install left up to 73 MB in the install directory
    /// under a pid-stamped name no later run ever revisited.
    #[test]
    fn scratch_from_a_dead_run_is_swept_and_this_run_s_is_not() {
        let s = Scratch::new("sweep");
        let dir = s.path();
        let me = std::process::id();

        // A dead run's leavings: all three shapes.
        std::fs::create_dir_all(dir.join(".tobii-update-999999")).unwrap();
        std::fs::write(dir.join(".tobii-update-999999/archive.tar.gz"), b"x").unwrap();
        std::fs::write(dir.join(".tobii-update-probe-999999"), b"x").unwrap();
        std::fs::write(dir.join(".tobii.new-999999"), b"x").unwrap();
        std::fs::write(dir.join(".tobii-gtk.new-999999"), b"x").unwrap();
        // This run's, which a concurrent updater must not have deleted.
        std::fs::create_dir_all(dir.join(format!(".tobii-update-{me}"))).unwrap();
        // And things that merely look similar.
        std::fs::write(dir.join("tobii"), b"x").unwrap();
        std::fs::write(dir.join(".tobii-update-notes"), b"x").unwrap();

        sweep_stale_scratch(dir);

        assert!(!dir.join(".tobii-update-999999").exists(), "dead work dir");
        assert!(
            !dir.join(".tobii-update-probe-999999").exists(),
            "dead probe"
        );
        assert!(!dir.join(".tobii.new-999999").exists(), "dead staging");
        assert!(!dir.join(".tobii-gtk.new-999999").exists(), "dead staging");
        assert!(
            dir.join(format!(".tobii-update-{me}")).exists(),
            "own work dir"
        );
        assert!(dir.join("tobii").exists(), "a real binary");
        assert!(
            dir.join(".tobii-update-notes").exists(),
            "a non-numeric suffix is not a pid and must be left alone"
        );
    }

    /// Asked of the package managers, not guessed from the path. A file no
    /// package owns — a tarball install, a build tree — must come back `None`,
    /// or the updater would refuse to update the copies it exists for.
    #[test]
    fn a_file_no_package_owns_is_not_reported_as_package_managed() {
        // Takes the PATH lock: the stub tests below repoint `PATH` at a fake
        // dpkg, and this one asks the REAL package managers. Without the lock
        // it intermittently gets a stub's answer.
        let _guard = PATH_TEST.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("owner");
        let loose = s.path().join("tobii");
        std::fs::write(&loose, b"not from a package").unwrap();
        assert_eq!(package_owner(&loose), Ownership::None);
        assert_eq!(
            package_owner(&s.path().join("does-not-exist")),
            Ownership::None
        );
    }

    /// `package_owner` against stub package managers.
    ///
    /// Two of the three output parsers had never parsed anything, and the
    /// refusal path had never run: every existing test stayed on the "nobody
    /// owns it" branch or built an `InstallError` by hand. These put a fake
    /// `dpkg`/`rpm`/`pacman` first on `PATH` and drive the real function.
    ///
    /// `PATH` is process-global, so they serialise on one lock.
    static PATH_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_stub<T>(prog: &str, script: &str, f: impl FnOnce() -> T) -> T {
        let guard = PATH_TEST.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("tobii-stub-{prog}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(prog);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let old = std::env::var_os("PATH");
        // ONLY the stub directory: the real dpkg/rpm/pacman must not be
        // reachable, or the machine running the test decides the outcome.
        std::env::set_var("PATH", &dir);
        let out = f();
        match old {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        drop(guard);
        out
    }

    #[test]
    fn dpkg_naming_the_package_is_a_refusal() {
        let o = with_stub("dpkg", "echo 'tobii-linux: /usr/bin/tobii'; exit 0", || {
            package_owner(Path::new("/usr/bin/tobii"))
        });
        assert_eq!(
            o,
            Ownership::Package {
                manager: "dpkg".into(),
                package: "tobii-linux".into()
            }
        );
    }

    /// dpkg answers a diverted path with a line whose first colon-field is the
    /// words "diversion by <pkg> from" — which the naive `split(':').next()`
    /// took for a package name, so the refusal named a package called
    /// "diversion by tobii-linux from".
    #[test]
    fn a_dpkg_diversion_line_is_not_read_as_a_package_name() {
        let o = with_stub(
            "dpkg",
            "echo 'diversion by other-pkg from: /usr/bin/tobii'; \
             echo 'tobii-linux: /usr/bin/tobii'; exit 0",
            || package_owner(Path::new("/usr/bin/tobii")),
        );
        assert_eq!(
            o,
            Ownership::Package {
                manager: "dpkg".into(),
                package: "tobii-linux".into()
            }
        );
    }

    /// A broken database exits non-zero for a reason that is not "no match".
    /// Reading that as "nobody owns it" is how a packaged binary gets
    /// overwritten by a check that FAILED rather than passed.
    #[test]
    fn a_package_manager_that_errors_is_not_read_as_nobody_owns_it() {
        let o = with_stub(
            "dpkg",
            "echo 'dpkg-query: error: unable to open database' >&2; exit 2",
            || package_owner(Path::new("/usr/bin/tobii")),
        );
        assert!(
            matches!(o, Ownership::Unknown { ref manager, .. } if manager == "dpkg"),
            "{o:?}"
        );
    }

    /// Its own no-match code still means no match.
    #[test]
    fn dpkgs_no_match_exit_code_means_nobody_owns_it() {
        let o = with_stub("dpkg", "exit 1", || {
            package_owner(Path::new("/usr/bin/tobii"))
        });
        assert_eq!(o, Ownership::None);
    }

    /// `Command::output()` waits forever, and both callers make that fatal —
    /// the GUI's install thread holds a `GApplication` hold, and the
    /// diagnostics report is built on the GTK main thread.
    #[test]
    fn a_package_manager_that_hangs_is_killed_rather_than_waited_for() {
        let started = std::time::Instant::now();
        let o = with_stub("dpkg", "sleep 3600", || {
            package_owner(Path::new("/usr/bin/tobii"))
        });
        assert!(
            matches!(o, Ownership::Unknown { .. }),
            "a hung manager must not read as unowned: {o:?}"
        );
        assert!(
            started.elapsed() < OWNER_TIMEOUT * 3,
            "it waited {:?}",
            started.elapsed()
        );
    }

    /// The refusal has to be actionable: `dpkg -S` is how you ask, `apt` is how
    /// you upgrade, and "update it with dpkg" is advice nobody can follow.
    #[test]
    fn the_refusal_names_the_command_that_updates_not_the_one_that_answered() {
        let msg = InstallError::PackageManaged {
            manager: "dpkg".into(),
            package: "tobii-linux".into(),
        }
        .to_string();
        assert!(msg.contains("apt"), "{msg}");
        let msg = InstallError::PackageManaged {
            manager: "rpm".into(),
            package: "tobii-linux".into(),
        }
        .to_string();
        assert!(msg.contains("dnf"), "{msg}");
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

    fn asset(name: &str) -> crate::release::Asset {
        crate::release::Asset {
            name: name.to_string(),
            url: format!("https://github.com/a/b/{name}"),
        }
    }

    /// A download that hands back exactly the bytes it is told to, so the
    /// checking and the cleanup can be run without a network.
    fn canned(
        bytes: &'static [(&'static str, &'static [u8])],
    ) -> impl Fn(&str, &Path) -> Result<(), net::NetError> {
        move |_url, dest| {
            let name = dest.file_name().unwrap().to_string_lossy().to_string();
            let body = bytes
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, b)| *b)
                .unwrap_or(b"unexpected");
            std::fs::write(dest, body).unwrap();
            Ok(())
        }
    }

    /// The whole reason a download is checked at all: a file that is not what
    /// the release published must not be left sitting in somebody's Downloads
    /// folder, where they would install it by hand tomorrow.
    #[test]
    fn a_download_that_does_not_match_its_checksum_is_deleted_and_reported() {
        let s = Scratch::new("download-bad");
        let good = sha256::hex_digest(b"the real thing");
        let sums = format!("{good}  PKGBUILD\n{good}  tobii-linux.install\n");
        let files = [asset("PKGBUILD"), asset("tobii-linux.install")];
        let files: Vec<&crate::release::Asset> = files.iter().collect();

        // The second file arrives corrupted.
        let mut written = Vec::new();
        let e = fetch_each(
            &files,
            s.path(),
            &sums,
            &|_| {},
            &canned(&[
                ("PKGBUILD", b"the real thing"),
                ("tobii-linux.install", b"truncated"),
            ]),
            &mut written,
        )
        .expect_err("a wrong digest must fail");
        match &e {
            InstallError::Digest { name, .. } => assert_eq!(name, "tobii-linux.install"),
            other => panic!("expected a digest failure, got {other:?}"),
        }
        // Both are reported as written, because both are on disk and the
        // caller has to remove them — including the bad one.
        assert_eq!(written.len(), 2, "the bad file must be reported as written");
        for p in &written {
            assert!(p.exists(), "fetch_each does not delete; its caller does");
        }
        assert!(e.to_string().contains("tobii-linux.install"), "{e}");
    }

    /// The happy path, and the property the Arch download depends on: both
    /// halves of the pair land in one directory, under their own names.
    #[test]
    fn a_download_that_matches_is_kept_under_its_published_name() {
        let s = Scratch::new("download-good");
        let a = sha256::hex_digest(b"pkgbuild bytes");
        let b = sha256::hex_digest(b"hook bytes");
        let sums = format!("{a}  PKGBUILD\n{b}  tobii-linux.install\n");
        let files = [asset("PKGBUILD"), asset("tobii-linux.install")];
        let files: Vec<&crate::release::Asset> = files.iter().collect();

        // `progress` is an `Fn`, so what it collects into has to be shared
        // rather than borrowed mutably.
        let steps = std::cell::RefCell::new(Vec::new());
        let mut written = Vec::new();
        fetch_each(
            &files,
            s.path(),
            &sums,
            &|step| steps.borrow_mut().push(step.to_string()),
            &canned(&[
                ("PKGBUILD", b"pkgbuild bytes"),
                ("tobii-linux.install", b"hook bytes"),
            ]),
            &mut written,
        )
        .expect("matching digests download cleanly");
        assert_eq!(
            written,
            vec![
                s.path().join("PKGBUILD"),
                s.path().join("tobii-linux.install")
            ]
        );
        assert_eq!(
            std::fs::read(s.path().join("PKGBUILD")).unwrap(),
            b"pkgbuild bytes"
        );
        assert_eq!(
            steps.into_inner(),
            vec!["PKGBUILD", "tobii-linux.install"],
            "each file should be named as it is fetched"
        );
    }

    /// A package can own one binary and not the other — a split package, or an
    /// install half-replaced by hand. Asking only about the first is how an
    /// owned file gets overwritten while the check appears to have run.
    #[test]
    fn ownership_is_asked_about_every_binary_not_just_the_first() {
        let owns_the_gui = "case \"$*\" in \
             *tobii-gtk) echo 'tobii-linux: /usr/bin/tobii-gtk'; exit 0;; \
             esac; exit 1";
        let o = with_stub("dpkg", owns_the_gui, || ownership_of(Path::new("/usr/bin")));
        assert_eq!(
            o,
            Ownership::Package {
                manager: "dpkg".into(),
                package: "tobii-linux".into()
            },
            "the second binary being owned is still ownership"
        );

        // Nobody owns either: the one answer that lets an install proceed.
        let none = with_stub("dpkg", "exit 1", || ownership_of(Path::new("/usr/bin")));
        assert_eq!(none, Ownership::None);
    }

    /// What the caller does with the failure: the corrupted file goes, and so
    /// does the folder — unless the folder holds something this download did
    /// not put there.
    #[test]
    fn a_failed_download_leaves_nothing_of_its_own_behind() {
        let s = Scratch::new("discard");
        let dir = s.path().join("tobii-linux-0.3.0");
        std::fs::create_dir_all(&dir).unwrap();
        let written = vec![dir.join("PKGBUILD"), dir.join("tobii-linux.install")];
        for p in &written {
            std::fs::write(p, b"half a download").unwrap();
        }
        discard(&written, &dir);
        assert!(!dir.exists(), "an emptied download folder should go too");

        // The same failure in a folder that already held something else: the
        // download's own files go, the stranger stays, and so does the folder.
        std::fs::create_dir_all(&dir).unwrap();
        let theirs = dir.join("notes.txt");
        std::fs::write(&theirs, b"mine").unwrap();
        for p in &written {
            std::fs::write(p, b"half a download").unwrap();
        }
        discard(&written, &dir);
        for p in &written {
            assert!(!p.exists(), "{} should have been removed", p.display());
        }
        assert!(
            theirs.exists(),
            "a file this download did not write must stay"
        );
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

/// The update banner's decision — see [`action_for`] and [`placement`].
#[cfg(test)]
mod placement_tests {
    use super::*;

    fn placed(owner: Ownership, replaceable: bool, exe_gone: bool) -> Placement {
        Placement {
            dir: PathBuf::from("/usr/local/bin"),
            owner,
            replaceable,
            exe_gone,
        }
    }

    /// The reported case first: a copy whose package was removed while it ran.
    #[test]
    fn a_copy_replaced_or_removed_while_running_is_told_to_restart_whoever_owns_it() {
        assert_eq!(
            action_for(&placed(Ownership::None, false, true)),
            Action::Restart
        );
        let pkg = Ownership::Package {
            manager: "pacman".into(),
            package: "tobii-linux".into(),
        };
        assert_eq!(action_for(&placed(pkg, false, true)), Action::Restart);
    }

    #[test]
    fn an_unowned_copy_this_user_cannot_replace_gets_a_download_not_a_dead_update() {
        assert_eq!(
            action_for(&placed(Ownership::None, false, false)),
            Action::DownloadForSystem {
                dir: PathBuf::from("/usr/local/bin")
            }
        );
    }

    #[test]
    fn the_ordinary_home_install_still_gets_update() {
        assert_eq!(
            action_for(&placed(Ownership::None, true, false)),
            Action::Update
        );
    }

    #[test]
    fn a_packaged_copy_gets_its_package_and_an_unknown_owner_keeps_update() {
        let pkg = Ownership::Package {
            manager: "dpkg".into(),
            package: "tobii-linux".into(),
        };
        assert_eq!(
            action_for(&placed(pkg, false, false)),
            Action::DownloadPackage {
                manager: "dpkg".into(),
                package: "tobii-linux".into()
            }
        );
        let unknown = Ownership::Unknown {
            manager: "rpm".into(),
            why: "timed out".into(),
        };
        assert_eq!(action_for(&placed(unknown, true, false)), Action::Update);
    }

    /// The suffix the kernel uses, measured with a copy of `sleep` deleted while
    /// it ran.
    #[test]
    fn a_deleted_binary_is_recognised_by_the_kernels_suffix() {
        assert!(exe_replaced_or_removed(Path::new(
            "/usr/bin/tobii-gtk (deleted)"
        )));
        assert!(!exe_replaced_or_removed(Path::new("/usr/bin/tobii-gtk")));
        assert!(!exe_replaced_or_removed(Path::new(
            "/home/u/(deleted)/tobii-gtk"
        )));
    }

    #[test]
    fn writability_is_asked_of_the_kernel_without_creating_anything() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tobii-placement-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(writable_by_me(&dir));
        assert!(
            can_replace_in_place(&dir),
            "an empty directory has nothing to refuse"
        );
        std::fs::write(dir.join("tobii"), b"x").unwrap();
        assert!(can_replace_in_place(&dir), "a binary this user owns");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "asking created nothing"
        );
        // Read-only, as a system directory is to a user. Root ignores modes, so
        // the assertion only means something when this does not run as root.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // SAFETY: geteuid takes no arguments and cannot fail.
        if unsafe { libc::geteuid() } != 0 {
            assert!(!writable_by_me(&dir));
            assert!(!can_replace_in_place(&dir));
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!writable_by_me(Path::new(
            "/nonexistent-tobii-placement-dir"
        )));
    }

    #[test]
    fn the_running_program_has_a_placement() {
        let p = placement().expect("current_exe is readable");
        assert!(!p.exe_gone);
        assert!(p.dir.is_dir());
    }
}
