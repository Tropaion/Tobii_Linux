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

/// A temporary the updater, or `install-payload.sh`, leaves in an install
/// directory, each named with the pid that made it: `.tobii-update-<pid>` (the
/// updater's work directory), `.tobii-update-probe-<pid>`, `.<bin>.new-<pid>`
/// (staged by the updater and by `install-payload.sh`) and `.<bin>.old-<pid>`
/// (the updater's rollback link, left only if it was killed mid-swap).
///
/// Parsed here, beside the code that writes them, so `tobii uninstall` — which
/// removes every one of them with the install — cannot fall behind a new shape
/// and leave it holding the directory open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScratchKind {
    Work,
    Probe,
    Staged,
    Backup,
}

/// Which [`ScratchKind`] `name` is, and the pid that made it; `None` for
/// anything else, a real binary and `.tobii-update-notes` included.
pub fn scratch_of(name: &str) -> Option<(ScratchKind, &str)> {
    let found = if let Some(pid) = name.strip_prefix(".tobii-update-probe-") {
        (ScratchKind::Probe, pid)
    } else if let Some(pid) = name.strip_prefix(".tobii-update-") {
        (ScratchKind::Work, pid)
    } else {
        // `.tobii.new-1` cannot be misread as a `tobii-gtk` name, or the
        // reverse: what follows the binary's name must be `.new-` or `.old-`.
        BINARIES.iter().find_map(|b| {
            let rest = name.strip_prefix('.')?.strip_prefix(b)?;
            rest.strip_prefix(".new-")
                .map(|pid| (ScratchKind::Staged, pid))
                .or_else(|| {
                    rest.strip_prefix(".old-")
                        .map(|pid| (ScratchKind::Backup, pid))
                })
        })?
    };
    let pid = found.1;
    (!pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())).then_some(found)
}

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
    /// Only an administrator can replace the binaries in this directory: it
    /// cannot be written to and is not this user's to fix, or it is shared and
    /// the binaries in it are another account's.
    NotWritable(PathBuf),
    /// The directory cannot be written to, and one command of this user's own
    /// fixes that — see [`FolderFix`]. Nothing needs downloading by hand.
    FolderNotWritable {
        dir: PathBuf,
        fix: FolderFix,
    },
    /// The directory is this user's, but the binaries in it belong to another
    /// account, so the swap's backup of them cannot be made.
    NotYours(PathBuf),
    /// Running as root on binaries that belong to another account: `sudo tobii
    /// update --install` on an ordinary home install. Refused, since as root the
    /// swap would work and leave root's files there — the state
    /// [`InstallError::NotYours`] exists to undo — and the advice is only to
    /// drop the sudo.
    RunWithoutSudo(PathBuf),
    /// The folder a download would go into can be changed by someone other than
    /// this user, so what was checked could be swapped before it is installed.
    /// `why` says who, and through which folder.
    UnsafeFolder {
        path: PathBuf,
        why: String,
    },
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
            // Not "re-run with the permission to write there": in the hub that
            // means running a GUI as root, and under sudo HOME is /root, so
            // every follow-on step goes wrong.
            InstallError::NotWritable(p) => write!(
                f,
                "only an administrator can replace the program's files in {}, so the update \
                 cannot be installed there. {}.",
                p.display(),
                by_hand(&format!(
                    "sudo ./install.sh --system {}",
                    shell_word(&p.to_string_lossy())
                ))
            ),
            InstallError::FolderNotWritable { dir, fix } => {
                let (state, remedy) = match fix {
                    FolderFix::MakeWritable => (
                        "is yours but you cannot write to it",
                        "It is not root's, so sudo would not help. Make it writable",
                    ),
                    FolderFix::TakeBack => (
                        "is in your home folder but belongs to another account — made with \
                         sudo, probably",
                        "Give that one folder back to yourself, not what is in it",
                    ),
                };
                write!(
                    f,
                    "{} {state}, so the update cannot be installed there. Nothing needs \
                     downloading. {remedy}: `{}`, then run `tobii update --install` again.",
                    dir.display(),
                    fix.command(dir)
                )
            }
            InstallError::NotYours(p) => write!(
                f,
                "the program's files in {} belong to another account — installed with sudo, \
                 probably — so they cannot be replaced in place. {} WITHOUT sudo: that \
                 replaces them with files you own.",
                p.display(),
                by_hand(&format!(
                    "./install.sh {}",
                    shell_word(&p.to_string_lossy())
                ))
            ),
            InstallError::RunWithoutSudo(p) => write!(
                f,
                "this is running as root — under sudo, probably — and the program's files in \
                 {} belong to another account, so updating them as root would leave them \
                 root's. Nothing was changed. Run `tobii update --install` again as the \
                 account that owns them, without sudo.",
                p.display()
            ),
            InstallError::UnsafeFolder { path, why } => write!(
                f,
                "{} can be changed by someone other than you ({why}), so a download there \
                 could be swapped after its checksum was checked and before you install it. \
                 Pick a folder only you can write to.",
                path.display()
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
/// The 25 ms `try_wait` loop every subprocess here and in `tobii uninstall`
/// needs, written once so each caller is bounded by construction rather than by
/// remembering to be. Public for that second caller: keep one copy, not two.
/// Each pipe is read up to 256 KiB.
///
/// The pipes are read *after* the child has exited, rather than with
/// `wait_with_output`, which waits for the pipe to close — a probed binary that
/// spawned a child holding it open would hang there, past the deadline that was
/// supposed to bound this.
pub fn wait_bounded(
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
    /// Whether this user may write the directory: faccessat(W_OK | X_OK), which counts
    /// read-only mounts and ACLs. One they cannot write is one only an
    /// administrator can change — [`Action::DownloadForSystem`] — unless it is
    /// their own ([`Placement::dir_mine`]) or in their home
    /// ([`Placement::dir_in_home`]).
    pub dir_writable: bool,
    /// Whether every binary in it belongs to this user. Not implied by the first,
    /// and not the same case: with `fs.protected_hardlinks` on — the kernel
    /// default, and 1 on the machine this was found on — the swap's hard-linked
    /// backup of a file you do not own fails with EPERM even in a directory you
    /// can write. That is what an install into `~/.local/bin` run with sudo
    /// leaves, and it needs no administrator to fix — [`Action::DownloadForHome`].
    pub binaries_mine: bool,
    /// Whether the directory itself belongs to this user: `stat`, following a
    /// symlink as the faccessat behind [`Placement::dir_writable`] does.
    ///
    /// It decides two things. An unwritable directory of their own needs
    /// `chmod u+w`, not sudo: `sudo ./install.sh --system` there would put
    /// root's files into it — the [`Action::DownloadForHome`] state — and a menu
    /// entry for every user pointing into one user's folder. And the plain
    /// install that replaces another account's files is offered only here: in a
    /// sticky folder someone else owns, rename(2) over a file refuses anyone but
    /// the file's owner or the folder's, so `./install.sh` stops at its first
    /// `mv`; and a group-writable system folder (/usr/local/bin as root:staff
    /// 2775) holds files every user runs, which a per-user install should not
    /// take over.
    pub dir_mine: bool,
    /// Whether the directory is strictly inside this user's home, as `$HOME`
    /// names it once symlinks are resolved (`current_exe` is the path the kernel
    /// resolved, and /home may be a link to /var/home). False when `$HOME` is
    /// unset, relative, or cannot be resolved.
    ///
    /// An unwritable folder in the home that is not the user's is what v0.3.0's
    /// `sudo ./install.sh ~/.local/bin` left when ~/.local/bin did not exist yet:
    /// root made it. A non-recursive chown gives it back — see
    /// [`FolderFix::TakeBack`].
    pub dir_in_home: bool,
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
    /// Nobody owns it, but it is somewhere only an administrator can replace it
    /// — a `sudo ./install.sh --system` install, one copied by hand into a
    /// system directory, or another account's files in a folder that is not
    /// this user's either. Fetch the archive and hand over the one command that
    /// installs it there.
    DownloadForSystem { dir: PathBuf },
    /// Nobody owns it and the directory is this user's own, but the binaries in
    /// it belong to another account. Fetch the archive, and hand over the plain
    /// `./install.sh` — no sudo, which is what left them there — that replaces
    /// them with files this user owns: rename(2) in a directory you own does not
    /// care who owns the file it replaces, sticky bit or not.
    DownloadForHome { dir: PathBuf },
    /// Nobody owns it, and the directory cannot be written, but one command of
    /// this user's fixes that — see [`FolderFix`]. There is nothing to download:
    /// the banner shows the command, and the next start decides again.
    FixFolder { dir: PathBuf, fix: FolderFix },
    /// This copy was deleted or replaced while it ran. There is nothing here to
    /// update: quitting and starting again runs whatever is installed now.
    Restart,
}

/// What makes an unwritable install directory writable again, when that is the
/// user's own to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderFix {
    /// The folder is theirs and only its mode is wrong (0555): `chmod u+w`. Not
    /// sudo — it would change nothing a chmod cannot, and `--system` would put
    /// root's files into it.
    MakeWritable,
    /// The folder is in their home and another account owns it: a chown of that
    /// one folder, not of what is in it. Afterwards the binaries in it are
    /// still root's, and the next start offers [`Action::DownloadForHome`] for
    /// them.
    TakeBack,
}

impl FolderFix {
    /// The command, for `dir`, with this process's own ids as numbers:
    /// `$(id -u)` is not read by fish before 3.4, and the command is pasted
    /// into whatever shell the user has. The only place either command is
    /// written, for the CLI's refusal and the hub's banner alike.
    pub fn command(self, dir: &Path) -> String {
        // SAFETY: geteuid and getegid take no arguments and cannot fail.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        self.command_as(dir, uid, gid)
    }

    fn command_as(self, dir: &Path, uid: u32, gid: u32) -> String {
        let dir = shell_word(&dir.to_string_lossy());
        match self {
            FolderFix::MakeWritable => format!("chmod u+w {dir}"),
            FolderFix::TakeBack => format!("sudo chown {uid}:{gid} {dir}"),
        }
    }
}

/// The decision, kept apart from gathering its inputs so every case is tested.
///
/// Reported on 2026-09-11: a hub whose package had been removed while it kept
/// running offered Update, and after the click said "/usr/bin cannot be written
/// to … re-run with the permission to write there". Both halves were dead ends.
/// Nothing owned the deleted files any more, so the package check passed; and a
/// GUI cannot sensibly be re-run as root. The first case now asks for a restart
/// and the second offers a download, before anything is clicked.
///
/// "Only an administrator can change it" is the last answer for a folder this
/// user cannot write, not the first: one of their own needs a chmod, and one in
/// their home that root made needs a chown. Handing either the `--system`
/// command would put root's files, and a menu entry for every user, into one
/// user's folder.
pub fn action_for(p: &Placement) -> Action {
    if p.exe_gone {
        return Action::Restart;
    }
    let dir = p.dir.clone();
    match &p.owner {
        Ownership::Package { manager, package } => Action::DownloadPackage {
            manager: manager.clone(),
            package: package.clone(),
        },
        // `Unknown` keeps Update on purpose: a package manager could not be
        // *asked*, which is not the same as one owning this copy, and
        // `install_release` refuses with the reason when it is pressed.
        Ownership::Unknown { .. } => Action::Update,
        Ownership::None if !p.dir_writable && p.dir_mine => Action::FixFolder {
            dir,
            fix: FolderFix::MakeWritable,
        },
        Ownership::None if !p.dir_writable && p.dir_in_home => Action::FixFolder {
            dir,
            fix: FolderFix::TakeBack,
        },
        Ownership::None if !p.dir_writable => Action::DownloadForSystem { dir },
        Ownership::None if !p.binaries_mine && p.dir_mine => Action::DownloadForHome { dir },
        // Another account's files in a folder that is not this user's either —
        // a shared or sticky one — so only an administrator can replace them.
        Ownership::None if !p.binaries_mine => Action::DownloadForSystem { dir },
        Ownership::None => Action::Update,
    }
}

/// What [`refuse_before_download`] may ask, each only when its answer decides
/// something.
///
/// References to closures so a test can answer them, and so a probe that is no
/// longer needed never runs: the write probe creates a file, and a directory a
/// package manager owns must get none.
#[derive(Clone, Copy)]
struct Probes<'a> {
    /// The running binary was deleted or replaced after it started.
    exe_gone: bool,
    /// Who owns the binaries: [`ownership_of`].
    owner: &'a dyn Fn() -> Ownership,
    /// Whether a file can be created in the directory: [`is_writable`].
    writable: &'a dyn Fn() -> bool,
    /// [`Placement::binaries_mine`].
    binaries_mine: &'a dyn Fn() -> bool,
    /// [`Placement::dir_mine`].
    dir_mine: &'a dyn Fn() -> bool,
    /// [`Placement::dir_in_home`].
    dir_in_home: &'a dyn Fn() -> bool,
    /// Whether this process runs as root.
    as_root: &'a dyn Fn() -> bool,
}

/// The refusals [`install_release`] makes before it downloads anything, in the
/// order it makes them — the facts [`placement`] gathers for the banner, asked
/// again because `tobii update --install` has no banner in front of it. The
/// same decision as [`action_for`], ending in a refusal where that ends in a
/// banner.
fn refuse_before_download(dir: &Path, p: Probes) -> Result<(), InstallError> {
    // A copy deleted or replaced while it ran has nothing at its path to update,
    // and pressing on reports whatever the next check trips over — "not
    // writable", for the orphaned package copy this was found with.
    if p.exe_gone {
        return Err(InstallError::ReplacedWhileRunning);
    }
    // Asked before writability, because the two failures need opposite advice:
    // "you need permission" invites `sudo`, which is exactly the wrong thing to
    // do to a package-managed file.
    match (p.owner)() {
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
    let dir = dir.to_path_buf();
    if !(p.writable)() {
        return Err(if (p.dir_mine)() {
            InstallError::FolderNotWritable {
                dir,
                fix: FolderFix::MakeWritable,
            }
        } else if (p.dir_in_home)() {
            InstallError::FolderNotWritable {
                dir,
                fix: FolderFix::TakeBack,
            }
        } else {
            InstallError::NotWritable(dir)
        });
    }
    // Not the same as not writable, and it needs the opposite advice — see
    // `Placement::binaries_mine`; unasked, the swap fails after the whole
    // download. `tobii update --install` is the only updater a --lean install
    // has, so it is asked here and not only by the banner.
    if !(p.binaries_mine)() {
        return Err(if (p.as_root)() {
            // As root the same answer means the opposite: the files are an
            // ordinary user's, and the sudo on this command is the problem,
            // not a past install.
            InstallError::RunWithoutSudo(dir)
        } else if (p.dir_mine)() {
            InstallError::NotYours(dir)
        } else {
            InstallError::NotWritable(dir)
        });
    }
    Ok(())
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
        dir_writable: writable_by_me(&dir),
        binaries_mine: binaries_mine(&dir),
        dir_mine: dir_is_mine(&dir),
        dir_in_home: inside_home(&dir, std::env::var_os("HOME").as_deref()),
        exe_gone: exe_replaced_or_removed(&exe),
        dir,
    })
}

/// [`Placement::dir_mine`]. Also asked by [`install_release`].
fn dir_is_mine(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid takes no arguments and cannot fail.
    let euid = unsafe { libc::geteuid() };
    std::fs::metadata(dir).is_ok_and(|m| m.uid() == euid)
}

/// [`Placement::dir_in_home`], with `$HOME` passed in so a test can point it
/// somewhere. Also asked by [`install_release`].
fn inside_home(dir: &Path, home: Option<&std::ffi::OsStr>) -> bool {
    let Some(home) = home.map(Path::new).filter(|h| h.is_absolute()) else {
        return false;
    };
    let Ok(home) = std::fs::canonicalize(home) else {
        return false;
    };
    dir != home && dir.starts_with(&home)
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

/// [`Placement::binaries_mine`]: every binary here that this program would
/// replace belongs to this user. Also asked by [`install_release`].
pub(crate) fn binaries_mine(dir: &Path) -> bool {
    // SAFETY: geteuid takes no arguments and cannot fail.
    let euid = unsafe { libc::geteuid() };
    BINARIES
        .iter()
        .all(|b| match std::fs::symlink_metadata(dir.join(b)) {
            Ok(m) => std::os::unix::fs::MetadataExt::uid(&m) == euid,
            // Not installed here — a --lean install has no tobii-gtk — so
            // nothing to replace and nothing to refuse.
            Err(_) => true,
        })
}

/// A word of a command printed for a person to paste, as a POSIX shell reads it
/// back: as it is when it holds only `[A-Za-z0-9_./-]`, otherwise in single
/// quotes, each `'` in it written `'\''`. Advice that ends in a command is
/// copied and run, and a path with a space in it splits in two.
pub fn shell_word(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The by-hand route the refusals end in: fetch, check, unpack, run `cmd`.
///
/// One copy of it, because the checksum step has one right wording: an
/// integrity check of the download, and nothing more.
fn by_hand(cmd: &str) -> String {
    format!(
        "Download the release archive and SHA256SUMS from the releases page, check them \
         with `sha256sum -c --ignore-missing SHA256SUMS`, unpack the archive, and run \
         `{cmd}` in it"
    )
}

/// The overflow uid: what a file whose owner has no mapping in this user
/// namespace shows up as. Inside a toolbox or `unshare -c`, root's `/` and
/// `/home` are owned by it. `tobii uninstall` takes it from here.
pub const OVERFLOW_UID: u32 = 65534;

/// Whether a folder above a download may belong to `owner`, for the user `me`:
/// see [`chosen_folder`].
fn may_own(owner: u32, me: u32) -> bool {
    owner == me || owner == 0 || owner == OVERFLOW_UID
}

/// Who other than its owner can write a file or directory, if anyone.
///
/// A group write bit counts unless the group is the user's own private group.
/// A sticky directory is NOT left out here: whether one may be, and whose it
/// must be, is the caller's to decide — in one, only an entry's owner or the
/// directory's owner can rename or delete that entry.
pub fn foreign_writer(
    m: &std::fs::Metadata,
    private_group: &dyn Fn(u32) -> bool,
) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let mode = m.mode();
    if mode & 0o002 != 0 {
        Some("anyone".into())
    } else if mode & 0o020 != 0 && !private_group(m.gid()) {
        Some(format!(
            "its group (gid {}), which is not yours alone",
            m.gid()
        ))
    } else {
        None
    }
}

/// Whether `gid` is the private group of the user `euid`: that user's primary
/// group, with no other account in it.
///
/// Distributions that give each user a group of their own often set umask
/// 002 (Fedora's /etc/bashrc does), so `mkdir -p ~/.local/bin` makes it
/// group-writable — by that group, which holds only the user. Read from
/// /etc/passwd and /etc/group only: a group they do not describe (LDAP, sssd)
/// cannot be seen to be private, so it is not taken to be.
pub fn is_private_group(gid: u32, euid: u32) -> bool {
    match (
        std::fs::read_to_string("/etc/passwd"),
        std::fs::read_to_string("/etc/group"),
    ) {
        (Ok(passwd), Ok(group)) => private_group_in(&passwd, &group, gid, euid),
        _ => false,
    }
}

/// [`is_private_group`], over the text of /etc/passwd and /etc/group.
pub fn private_group_in(passwd: &str, group: &str, gid: u32, euid: u32) -> bool {
    // (name, uid, primary gid)
    let accounts: Vec<(&str, u32, u32)> = passwd
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            Some((
                *f.first()?,
                f.get(2)?.parse().ok()?,
                f.get(3)?.parse().ok()?,
            ))
        })
        .collect();
    let mine: Vec<&str> = accounts
        .iter()
        .filter(|a| a.1 == euid)
        .map(|a| a.0)
        .collect();
    // Its primary group, and no one else's.
    if !accounts.iter().any(|a| a.1 == euid && a.2 == gid)
        || accounts.iter().any(|a| a.2 == gid && a.1 != euid)
    {
        return false;
    }
    // Listed in /etc/group, with no member but this user. Every line with
    // that gid counts: a gid may appear twice.
    let member_lists: Vec<&str> = group
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() >= 4 && f[2].parse::<u32>().ok() == Some(gid)).then(|| f[3])
        })
        .collect();
    !member_lists.is_empty()
        && member_lists.iter().all(|members| {
            members
                .split(',')
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .all(|m| mine.contains(&m))
        })
}

/// Whether a download into `into` would be let through — so a folder chooser
/// can start somewhere that works. The rule is [`chosen_folder`]'s.
pub fn download_folder_ok(into: &Path) -> bool {
    // SAFETY: geteuid takes no arguments and cannot fail.
    let euid = unsafe { libc::geteuid() };
    chosen_folder(into, euid, &|g| is_private_group(g, euid)).is_ok()
}

/// The folder a user chose for a download, and every folder above it, are
/// such that nobody but this user or root can rename an entry out of the way
/// and put their own in its place — see [`download_release_files`].
///
/// A directory's owner can always do that, sticky bit or not (rename(2)), so
/// each one must be this user's or root's — or the overflow uid, which is how
/// root's `/`, `/home` and `/tmp` look inside a toolbox or `unshare -c`.
/// Anyone else can only through a write bit, and not in a sticky directory,
/// where only an entry's owner or the directory's can move it. So the chosen
/// folder may be /tmp, but not a sticky folder another account owns.
///
/// The folders above are walked both as written and as resolved: the banner
/// prints the path as written, so the folder holding a symlink on it counts,
/// and the files land where it resolves to, so those folders count too.
///
/// `me` is this process's euid, and `private_group` answers
/// [`is_private_group`] — both passed in, so a test can make its own folders
/// look like someone else's.
fn chosen_folder(
    into: &Path,
    me: u32,
    private_group: &dyn Fn(u32) -> bool,
) -> Result<(), InstallError> {
    use std::os::unix::fs::MetadataExt;
    let unsafe_folder = |path: &Path, why: String| InstallError::UnsafeFolder {
        path: path.to_path_buf(),
        why,
    };
    let chosen = std::fs::metadata(into)?;
    if chosen.mode() & 0o1000 != 0 && !may_own(chosen.uid(), me) {
        return Err(unsafe_folder(
            into,
            format!(
                "it is shared, and its owner, uid {}, can move anything in it",
                chosen.uid()
            ),
        ));
    }
    let resolved = std::fs::canonicalize(into)?;
    let mut seen: Vec<&Path> = Vec::new();
    for folder in into.ancestors().chain(resolved.ancestors()) {
        if folder.as_os_str().is_empty() || seen.contains(&folder) {
            continue;
        }
        seen.push(folder);
        let m = std::fs::metadata(folder)?;
        let owner = m.uid();
        if !may_own(owner, me) {
            return Err(unsafe_folder(
                folder,
                format!("it belongs to uid {owner}, who can move anything in it"),
            ));
        }
        if m.mode() & 0o1000 == 0 {
            if let Some(who) = foreign_writer(&m, private_group) {
                return Err(unsafe_folder(folder, format!("{who} can write to it")));
            }
        }
    }
    Ok(())
}

/// The folder a download goes into, and the folder chosen for it, are this
/// user's alone — see [`download_release_files`].
///
/// The chosen folder and those above it follow [`chosen_folder`]. The version
/// folder, when it exists, must be a real directory — not a symlink — owned by
/// this user and writable by nobody else: not by anyone, and not by a group
/// unless it is this user's private group. No sticky exemption there, because
/// the files in it are what gets checked.
fn private_folder(
    into: &Path,
    dir: &Path,
    me: u32,
    private_group: &dyn Fn(u32) -> bool,
) -> Result<(), InstallError> {
    use std::os::unix::fs::MetadataExt;
    chosen_folder(into, me, private_group)?;
    let why = match std::fs::symlink_metadata(dir) {
        // Not there yet: it will be made, and checked again once it is.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
        Ok(m) if !m.is_dir() => "it is already there, and is not a folder".to_string(),
        Ok(m) if m.uid() != me => format!("it is already there, made by uid {}", m.uid()),
        Ok(m) => match foreign_writer(&m, private_group) {
            Some(who) => format!("{who} can write to it"),
            None => return Ok(()),
        },
    };
    Err(InstallError::UnsafeFolder {
        path: dir.to_path_buf(),
        why,
    })
}

/// The kernel's own answer to "may I create, rename and remove entries here":
/// write AND search permission, with the effective ids, so it counts read-only
/// mounts, ACLs and supplementary groups — and creates nothing. Search too,
/// because a directory that can be written but not searched (0o600) refuses
/// every rename into it. `tobii uninstall` asks the same question of a user's
/// install directories.
pub fn writable_by_me(dir: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(dir.as_os_str()))
    else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated path that outlives the call, and
    // faccessat only reads it.
    unsafe {
        libc::faccessat(
            libc::AT_FDCWD,
            c.as_ptr(),
            libc::W_OK | libc::X_OK,
            libc::AT_EACCESS,
        ) == 0
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
    // The folder the files go into is checked BEFORE anything is fetched, and
    // again once it exists. The banner ends by telling the user to run a command
    // on these files (`sudo pacman -U`, `sudo ./install.sh --system`, or a plain
    // `./install.sh`), so nobody else may be able to change them between the
    // checksum check and that command. A /tmp/tobii-linux-<version> made in
    // advance by another account, which create_dir_all silently reused, was the
    // way in.
    let dir = into.join(format!("tobii-linux-{}", release.version));
    // SAFETY: geteuid takes no arguments and cannot fail.
    let euid = unsafe { libc::geteuid() };
    let private_group = |g| is_private_group(g, euid);
    private_folder(into, &dir, euid, &private_group)?;
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

    make_version_folder(into, &dir, euid, &private_group)?;
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
/// Make the version folder `dir` in `into`, or take the one already there, and
/// check it again now that it exists: made by someone else between the first
/// check and here, it is theirs.
///
/// 0o755, not the default 0o777 less the umask: under umask 002 — or in a
/// chosen folder whose default ACL grants a group, or a setgid one whose group
/// is not this user's — the default is group-writable, which the check refuses
/// unless the group is this user's private one. umask can only clear bits, so
/// this is never more. A folder this call made and the check then refused is
/// removed, or every retry would find it there and refuse it again.
fn make_version_folder(
    into: &Path,
    dir: &Path,
    me: u32,
    private_group: &dyn Fn(u32) -> bool,
) -> Result<(), InstallError> {
    use std::os::unix::fs::DirBuilderExt;
    let made = match std::fs::DirBuilder::new().mode(0o755).create(dir) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(e.into()),
    };
    private_folder(into, dir, me, private_group).inspect_err(|_| {
        if made {
            let _ = std::fs::remove_dir(dir);
        }
    })
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
    refuse_before_download(
        &dir,
        Probes {
            exe_gone: std::env::current_exe().is_ok_and(|e| exe_replaced_or_removed(&e)),
            owner: &|| ownership_of(&dir),
            writable: &|| is_writable(&dir),
            binaries_mine: &|| binaries_mine(&dir),
            dir_mine: &|| dir_is_mine(&dir),
            dir_in_home: &|| inside_home(&dir, std::env::var_os("HOME").as_deref()),
            // SAFETY: geteuid takes no arguments and cannot fail.
            as_root: &|| unsafe { libc::geteuid() } == 0,
        },
    )?;

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
/// Matched by name, not by age: the [`ScratchKind`] shapes are this program's
/// own, and nothing else in an install directory looks like them. The current
/// process's own files are skipped, since a second updater running
/// concurrently would otherwise delete the first's staging out from under it —
/// and a live pid is a poor signal here, because pids are reused. A `.old-`
/// rollback link is not swept: a swap killed after its rename may have left the
/// previous binary reachable only through it, and `tobii uninstall` removes it
/// with the install.
fn sweep_stale_scratch(dir: &Path) {
    let me = format!("{}", std::process::id());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let Some((kind, pid)) = scratch_of(&name) else {
            continue;
        };
        if kind == ScratchKind::Backup || pid == me {
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
        // Recorded before the copy: a copy that fails part-way still creates the
        // file, in the install directory rather than the scratch directory.
        staged.push((staging.clone(), target));
        let ready = std::fs::copy(&src, &staging)
            .map_err(InstallError::from)
            .and_then(|_| set_executable(&staging))
            .and_then(|()| {
                probe(&staging).map_err(|detail| InstallError::WillNotRun {
                    name: name.to_string(),
                    detail,
                })
            });
        if let Err(e) = ready {
            remove_staged(&staged);
            return Err(e);
        }
    }
    if staged.is_empty() {
        return Err(InstallError::NothingToInstall);
    }
    if !missing.is_empty() {
        remove_staged(&staged);
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

/// Remove every staged copy: the cleanup for a refusal before the swap, and for
/// a rollback.
fn remove_staged(staged: &[(PathBuf, PathBuf)]) {
    for (s, _) in staged {
        let _ = std::fs::remove_file(s);
    }
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
                remove_staged(staged);
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

/// `<path> --version`, with stdin closed and no display: how this program asks
/// any of its binaries what version it is. Pipes and the deadline are the
/// caller's — [`wait_bounded`] — and `tobii uninstall` asks the same way.
///
/// The display is taken out of the environment because a GUI binary must not
/// try to talk to the session's display just to say what version it is.
pub fn version_command(path: &Path) -> std::process::Command {
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
    use super::*;

    fn pkg(manager: &str, package: &str) -> Ownership {
        Ownership::Package {
            manager: manager.into(),
            package: package.into(),
        }
    }

    fn unknown(manager: &str) -> Ownership {
        Ownership::Unknown {
            manager: manager.into(),
            why: "timed out".into(),
        }
    }

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
        let s = Scratch::new("dl-unsafe-name");
        let dir = s.path();

        for bad in ["../evil", "a/b", "/etc/passwd", ".."] {
            let asset = Asset {
                name: bad.to_string(),
                url: "https://example.invalid/x".to_string(),
            };
            let release = release_with(vec![asset.clone()]);
            let err = download_release_files(&release, &[&asset], dir, &|_| {})
                .expect_err("a name that is a path must be refused");
            assert!(
                matches!(err, InstallError::UnsafeName(ref n) if n == bad),
                "{bad:?} gave {err:?}"
            );
        }
        assert!(
            std::fs::read_dir(dir).expect("dir").next().is_none(),
            "nothing may be written before the name is checked"
        );
    }

    /// Nothing to download is its own answer, not an empty success — a caller
    /// that got `Ok(vec![])` would report a finished download of no files.
    #[test]
    fn a_download_with_no_assets_says_there_is_no_build() {
        let s = Scratch::new("dl-empty");
        let err = download_release_files(&release_with(Vec::new()), &[], s.path(), &|_| {})
            .expect_err("an empty set must be refused");
        assert!(matches!(err, InstallError::NoBuildForTarget(_)), "{err:?}");
    }

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
        // True for a folder that is not writable, and for a shared one that is.
        assert!(w.starts_with("only an administrator can replace"), "{w}");
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
        // A rollback link a swap killed half-way left, possibly the previous
        // binary's last name: it stays.
        std::fs::write(dir.join(".tobii.old-999999"), b"x").unwrap();
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
        assert!(dir.join(".tobii.old-999999").exists(), "a rollback link");
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
        assert_eq!(o, pkg("dpkg", "tobii-linux"));
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
        assert_eq!(o, pkg("dpkg", "tobii-linux"));
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
        // `sleep` by its absolute path: with_stub leaves only the stub on PATH,
        // so a bare `sleep` would exit 127 at once, and the test would pass on
        // that exit code without the deadline ever being reached.
        let sleep = ["/usr/bin/sleep", "/bin/sleep"]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .expect("a sleep binary");
        let started = std::time::Instant::now();
        let o = with_stub("dpkg", &format!("exec {sleep} 3600"), || {
            package_owner(Path::new("/usr/bin/tobii"))
        });
        assert!(
            matches!(&o, Ownership::Unknown { why, .. } if why.contains("did not answer")),
            "a hung manager must be killed at the deadline, not read as unowned: {o:?}"
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
            pkg("dpkg", "tobii-linux"),
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

    // The update banner's decision — see `action_for` and `placement` — and the
    // refusals `install_release` makes from the same facts.

    /// Every probe unasked: each panics if it runs. A case overrides the ones
    /// its answer needs, so a probe asked out of order fails the test.
    fn unasked() -> Probes<'static> {
        Probes {
            exe_gone: false,
            owner: &|| panic!("the package managers were asked"),
            writable: &|| panic!("the write probe ran"),
            binaries_mine: &|| panic!("the files' owner was asked"),
            dir_mine: &|| panic!("the folder's owner was asked"),
            dir_in_home: &|| panic!("$HOME was asked"),
            as_root: &|| panic!("the euid was asked"),
        }
    }

    /// `install_release`'s own refusals, in order. A probe must not run once an
    /// earlier answer has refused: the write probe creates a file, and a
    /// package's directory must get none.
    #[test]
    fn install_release_refuses_before_downloading_in_the_order_the_advice_needs() {
        let dir = Path::new("/home/u/.local/bin");
        let refuse = |p| refuse_before_download(dir, p);
        let yes: &dyn Fn() -> bool = &|| true;
        let no: &dyn Fn() -> bool = &|| false;
        let nobody: &dyn Fn() -> Ownership = &|| Ownership::None;
        let base = unasked();

        let gone = Probes {
            exe_gone: true,
            ..base
        };
        assert!(matches!(
            refuse(gone),
            Err(InstallError::ReplacedWhileRunning)
        ));
        let pacman = || pkg("pacman", "tobii-linux-bin");
        assert!(matches!(
            refuse(Probes {
                owner: &pacman,
                ..base
            }),
            Err(InstallError::PackageManaged { .. })
        ));
        assert!(matches!(
            refuse(Probes {
                owner: &|| unknown("rpm"),
                ..base
            }),
            Err(InstallError::OwnerUnknown { .. })
        ));

        // Not writable. Whose the folder is decides the advice; the files are
        // not asked about, since nothing can be installed there either way.
        let unwritable = Probes {
            owner: nobody,
            writable: no,
            ..base
        };
        assert!(matches!(
            refuse(Probes { dir_mine: yes, ..unwritable }),
            Err(InstallError::FolderNotWritable { dir: p, fix: FolderFix::MakeWritable }) if p == dir
        ));
        assert!(matches!(
            refuse(Probes { dir_mine: no, dir_in_home: yes, ..unwritable }),
            Err(InstallError::FolderNotWritable { dir: p, fix: FolderFix::TakeBack }) if p == dir
        ));
        assert!(matches!(
            refuse(Probes { dir_mine: no, dir_in_home: no, ..unwritable }),
            Err(InstallError::NotWritable(p)) if p == dir
        ));

        // Writable, and someone else's files in it. $HOME is not asked.
        let not_mine = Probes {
            owner: nobody,
            writable: yes,
            binaries_mine: no,
            ..base
        };
        assert!(matches!(
            refuse(Probes { as_root: yes, ..not_mine }),
            Err(InstallError::RunWithoutSudo(p)) if p == dir
        ));
        assert!(matches!(
            refuse(Probes { as_root: no, dir_mine: yes, ..not_mine }),
            Err(InstallError::NotYours(p)) if p == dir
        ));
        // A shared or sticky folder that is not this user's: the plain install
        // would stop at its first `mv`, or take over files everyone runs.
        assert!(matches!(
            refuse(Probes { as_root: no, dir_mine: no, ..not_mine }),
            Err(InstallError::NotWritable(p)) if p == dir
        ));

        // The ordinary home install asks nothing past the files' owner.
        assert!(refuse(Probes {
            owner: nobody,
            writable: yes,
            binaries_mine: yes,
            ..base
        })
        .is_ok());
    }

    /// A placement in `/usr/local/bin`, with the facts `action_for` reads.
    struct Facts {
        dir_writable: bool,
        binaries_mine: bool,
        dir_mine: bool,
        dir_in_home: bool,
    }

    fn placed(owner: Ownership, f: Facts, exe_gone: bool) -> Placement {
        Placement {
            dir: PathBuf::from("/usr/local/bin"),
            owner,
            dir_writable: f.dir_writable,
            binaries_mine: f.binaries_mine,
            dir_mine: f.dir_mine,
            dir_in_home: f.dir_in_home,
            exe_gone,
        }
    }

    /// Every combination of the four facts, for the cases that ignore them.
    fn every_fact() -> impl Iterator<Item = Facts> {
        (0..16u8).map(|b| Facts {
            dir_writable: b & 1 != 0,
            binaries_mine: b & 2 != 0,
            dir_mine: b & 4 != 0,
            dir_in_home: b & 8 != 0,
        })
    }

    fn at_usr_local() -> PathBuf {
        PathBuf::from("/usr/local/bin")
    }

    /// The reported case first: a copy whose package was removed while it ran.
    #[test]
    fn a_copy_replaced_or_removed_while_running_is_told_to_restart_whoever_owns_it() {
        for f in every_fact() {
            assert_eq!(
                action_for(&placed(Ownership::None, f, true)),
                Action::Restart
            );
        }
        for f in every_fact() {
            assert_eq!(
                action_for(&placed(pkg("pacman", "tobii-linux"), f, true)),
                Action::Restart
            );
        }
    }

    #[test]
    fn an_unowned_copy_in_a_directory_this_user_cannot_write_gets_the_system_download() {
        for binaries_mine in [true, false] {
            let f = Facts {
                dir_writable: false,
                binaries_mine,
                dir_mine: false,
                dir_in_home: false,
            };
            assert_eq!(
                action_for(&placed(Ownership::None, f, false)),
                Action::DownloadForSystem {
                    dir: at_usr_local()
                }
            );
        }
    }

    /// A folder of the user's own that they cannot write (0555): `chmod u+w`.
    /// The `--system` install there would put root's files into it.
    #[test]
    fn an_unwritable_folder_of_the_users_own_gets_chmod_not_sudo() {
        for (binaries_mine, dir_in_home) in
            [(true, true), (true, false), (false, true), (false, false)]
        {
            let f = Facts {
                dir_writable: false,
                binaries_mine,
                dir_mine: true,
                dir_in_home,
            };
            assert_eq!(
                action_for(&placed(Ownership::None, f, false)),
                Action::FixFolder {
                    dir: at_usr_local(),
                    fix: FolderFix::MakeWritable
                }
            );
        }
    }

    /// Root's folder in the user's home — what v0.3.0's `sudo ./install.sh
    /// ~/.local/bin` left when that folder did not exist yet: a chown of that
    /// one folder, not `--system` into a home directory.
    #[test]
    fn root_s_folder_in_the_home_gets_chown_not_a_system_install() {
        for binaries_mine in [true, false] {
            let f = Facts {
                dir_writable: false,
                binaries_mine,
                dir_mine: false,
                dir_in_home: true,
            };
            assert_eq!(
                action_for(&placed(Ownership::None, f, false)),
                Action::FixFolder {
                    dir: at_usr_local(),
                    fix: FolderFix::TakeBack
                }
            );
        }
    }

    /// Root's files in a folder of the user's own: not a system install, and no
    /// sudo needed to fix it.
    #[test]
    fn someone_elses_files_in_a_directory_this_user_can_write_get_the_home_download() {
        let f = Facts {
            dir_writable: true,
            binaries_mine: false,
            dir_mine: true,
            dir_in_home: false,
        };
        assert_eq!(
            action_for(&placed(Ownership::None, f, false)),
            Action::DownloadForHome {
                dir: at_usr_local()
            }
        );
    }

    /// Writable is not the same as the user's: in a sticky folder another
    /// account owns, `./install.sh` stops at its first `mv`, and in a
    /// group-writable system folder it would take over files everyone runs.
    #[test]
    fn someone_elses_files_in_a_shared_folder_get_the_system_download() {
        for dir_in_home in [true, false] {
            let f = Facts {
                dir_writable: true,
                binaries_mine: false,
                dir_mine: false,
                dir_in_home,
            };
            assert_eq!(
                action_for(&placed(Ownership::None, f, false)),
                Action::DownloadForSystem {
                    dir: at_usr_local()
                }
            );
        }
    }

    #[test]
    fn the_ordinary_home_install_still_gets_update() {
        for (dir_mine, dir_in_home) in [(true, true), (true, false), (false, true), (false, false)]
        {
            let f = Facts {
                dir_writable: true,
                binaries_mine: true,
                dir_mine,
                dir_in_home,
            };
            assert_eq!(
                action_for(&placed(Ownership::None, f, false)),
                Action::Update
            );
        }
    }

    #[test]
    fn a_packaged_copy_gets_its_package_and_an_unknown_owner_keeps_update() {
        for f in every_fact() {
            assert_eq!(
                action_for(&placed(pkg("dpkg", "tobii-linux"), f, false)),
                Action::DownloadPackage {
                    manager: "dpkg".into(),
                    package: "tobii-linux".into()
                }
            );
        }
        for f in every_fact() {
            assert_eq!(
                action_for(&placed(unknown("rpm"), f, false)),
                Action::Update
            );
        }
    }

    /// The commands, as pasted into any shell: the ids are numbers, not
    /// `$(id -u)`, which older fish rejects; the folder is one word.
    #[test]
    fn the_folder_fixes_are_one_command_with_numeric_ids() {
        let spaced = Path::new("/home/u/My Apps");
        assert_eq!(
            FolderFix::MakeWritable.command_as(spaced, 1000, 1000),
            "chmod u+w '/home/u/My Apps'"
        );
        assert_eq!(
            FolderFix::TakeBack.command_as(spaced, 1000, 100),
            "sudo chown 1000:100 '/home/u/My Apps'"
        );
        // SAFETY: geteuid and getegid take no arguments and cannot fail.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        assert_eq!(
            FolderFix::TakeBack.command(spaced),
            format!("sudo chown {uid}:{gid} '/home/u/My Apps'")
        );

        let yours = InstallError::FolderNotWritable {
            dir: spaced.to_path_buf(),
            fix: FolderFix::MakeWritable,
        }
        .to_string();
        assert!(yours.contains("`chmod u+w '/home/u/My Apps'`"), "{yours}");
        // It may say sudo would not help; it must not hand over a sudo command.
        assert!(
            !yours.contains("`sudo") && !yours.contains("--system"),
            "{yours}"
        );
        assert!(yours.contains("tobii update --install"), "{yours}");
        let back = InstallError::FolderNotWritable {
            dir: spaced.to_path_buf(),
            fix: FolderFix::TakeBack,
        }
        .to_string();
        assert!(
            back.contains(&format!("`sudo chown {uid}:{gid} '/home/u/My Apps'`")),
            "{back}"
        );
        assert!(!back.contains("$(") && !back.contains("--system"), "{back}");
    }

    /// `sudo tobii update --install` on an ordinary install: the fix is to drop
    /// the sudo, not the by-hand install `NotYours` describes.
    #[test]
    fn root_updating_someone_elses_files_is_told_to_drop_the_sudo() {
        let t = InstallError::RunWithoutSudo(PathBuf::from("/home/u/My Apps")).to_string();
        assert!(
            t.contains("without sudo") && t.contains("/home/u/My Apps"),
            "{t}"
        );
        assert!(
            !t.contains("sudo ./install.sh") && !t.contains("sudo tobii"),
            "{t}"
        );
        assert!(!t.contains("SHA256SUMS"), "nothing needs downloading: {t}");
    }

    /// `$HOME` as the kernel sees it: through a symlink, and strictly inside.
    #[test]
    fn inside_home_resolves_home_and_wants_a_folder_strictly_below_it() {
        let s = Scratch::new("home");
        let real = s.path().join("real");
        let bin = real.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let link = s.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let real = std::fs::canonicalize(&real).unwrap();
        let bin = real.join(".local/bin");

        assert!(
            inside_home(&bin, Some(link.as_os_str())),
            "HOME through a link"
        );
        assert!(inside_home(&bin, Some(real.as_os_str())));
        assert!(
            !inside_home(&real, Some(real.as_os_str())),
            "the home itself"
        );
        assert!(!inside_home(
            Path::new("/usr/local/bin"),
            Some(real.as_os_str())
        ));
        assert!(!inside_home(&bin, None), "no HOME");
        assert!(
            !inside_home(&bin, Some("relative/home".as_ref())),
            "a relative HOME"
        );
        assert!(
            !inside_home(&bin, Some(s.path().join("gone").as_os_str())),
            "a HOME that does not resolve"
        );
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
    fn writability_and_ownership_are_asked_without_creating_anything() {
        use std::os::unix::fs::PermissionsExt;
        let s = Scratch::new("placement");
        let dir = s.path();
        assert!(writable_by_me(dir));
        assert!(dir_is_mine(dir), "a folder this user made");
        assert!(
            binaries_mine(dir),
            "an empty directory has nothing to refuse"
        );
        std::fs::write(dir.join("tobii"), b"x").unwrap();
        assert!(binaries_mine(dir), "a binary this user owns");
        assert_eq!(
            std::fs::read_dir(dir).unwrap().count(),
            1,
            "asking created nothing"
        );
        // Read-only, as a system directory is to a user. Root ignores modes, so
        // the assertion only means something when this does not run as root.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // SAFETY: geteuid takes no arguments and cannot fail.
        let root = unsafe { libc::geteuid() } == 0;
        if !root {
            assert!(!writable_by_me(dir));
        }
        assert!(dir_is_mine(dir), "still this user's, unwritable or not");
        // Writable but not searchable: nothing can be renamed into it, so it is
        // not writable in the sense every caller means.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o600)).unwrap();
        if !root {
            assert!(!writable_by_me(dir), "0o600 cannot take a rename");
        }
        // Drop's remove_dir_all needs to write it.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!dir_is_mine(Path::new("/nonexistent-tobii-placement-dir")));
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

    /// The two refusals that end in a command say which command, quote the
    /// directory, and check the download first. The one for someone else's files
    /// must not tell the user to reach for sudo: sudo is what caused it.
    #[test]
    fn the_refusals_hand_over_a_checked_quoted_command() {
        let spaced = PathBuf::from("/home/u/My Apps");
        let w = InstallError::NotWritable(spaced.clone()).to_string();
        assert!(
            w.contains("sha256sum -c --ignore-missing SHA256SUMS"),
            "{w}"
        );
        assert!(
            w.contains("sudo ./install.sh --system '/home/u/My Apps'"),
            "{w}"
        );
        let y = InstallError::NotYours(spaced).to_string();
        assert!(
            y.contains("sha256sum -c --ignore-missing SHA256SUMS"),
            "{y}"
        );
        assert!(y.contains("`./install.sh '/home/u/My Apps'`"), "{y}");
        assert!(!y.contains("sudo ./install.sh"), "{y}");
        assert_eq!(shell_word("/usr/local/bin"), "/usr/local/bin");
        assert_eq!(shell_word("it's"), r"'it'\''s'");

        // The download-folder refusal says who can write it, and no "as root":
        // for the home download and the archive fallback the next command is a
        // plain ./install.sh.
        let u = InstallError::UnsafeFolder {
            path: PathBuf::from("/home/u/Downloads"),
            why: "its group (gid 958), which is not yours alone, can write to it".into(),
        }
        .to_string();
        assert!(
            u.contains("/home/u/Downloads") && u.contains("gid 958"),
            "{u}"
        );
        assert!(u.contains("before you install it."), "{u}");
        assert!(!u.contains("as root"), "{u}");
    }

    fn mode(p: &Path, m: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap()
    }

    fn me() -> u32 {
        // SAFETY: geteuid takes no arguments and cannot fail.
        unsafe { libc::geteuid() }
    }

    /// Which folder a refusal names.
    fn refused_at(r: Result<(), InstallError>) -> Option<PathBuf> {
        match r {
            Err(InstallError::UnsafeFolder { path, .. }) => Some(path),
            Ok(()) => None,
            Err(e) => panic!("expected UnsafeFolder, got {e:?}"),
        }
    }

    /// A folder for download tests, 0755 whatever the umask: under 002 the
    /// scratch folder itself would be group-writable, and refused as an
    /// ancestor by a test that says the group is not private.
    fn download_scratch(tag: &str) -> Scratch {
        let s = Scratch::new(tag);
        mode(s.path(), 0o755);
        s
    }

    /// The download folder: a version folder someone else made in advance, or a
    /// chosen folder others can rename things in, is refused before anything is
    /// fetched. A sticky shared folder with no version folder yet is fine.
    #[test]
    fn a_download_folder_someone_else_can_change_is_refused() {
        let s = download_scratch("dlfolder");
        let base = s.path();
        let private = |_| true;
        let check = |into: &Path, dir: &Path| refused_at(private_folder(into, dir, me(), &private));

        let into = base.join("mine");
        std::fs::create_dir(&into).unwrap();
        mode(&into, 0o755);
        let dir = into.join("tobii-linux-9.9.9");
        assert_eq!(check(&into, &dir), None, "nothing there yet");
        std::fs::create_dir(&dir).unwrap();
        mode(&dir, 0o755);
        assert_eq!(check(&into, &dir), None, "made by this user, private");
        mode(&dir, 0o777);
        assert_eq!(check(&into, &dir), Some(dir.clone()));
        // No sticky exemption for the version folder: its files are what is
        // checked, and in it this user's are the only entries that matter.
        mode(&dir, 0o1777);
        assert_eq!(check(&into, &dir), Some(dir.clone()));
        mode(&dir, 0o755);
        std::fs::remove_dir(&dir).unwrap();
        std::os::unix::fs::symlink(base, &dir).unwrap();
        assert_eq!(
            check(&into, &dir),
            Some(dir.clone()),
            "a symlink is not a folder of this user's"
        );

        let shared = base.join("shared");
        std::fs::create_dir(&shared).unwrap();
        mode(&shared, 0o777);
        let there = shared.join("tobii-linux-9.9.9");
        assert_eq!(check(&shared, &there), Some(shared.clone()));
        assert!(!download_folder_ok(&shared));
        mode(&shared, 0o1777);
        assert_eq!(
            check(&shared, &there),
            None,
            "sticky and this user's: only an entry's owner can rename it"
        );
        assert!(download_folder_ok(&shared));
        assert!(download_folder_ok(&into));
    }

    /// A folder above the chosen one that others can write lets them rename the
    /// chosen folder away and put their own in its place, with the same path.
    #[test]
    fn a_folder_above_the_chosen_one_is_checked_too() {
        let s = download_scratch("dlabove");
        let private = |_| true;
        let open = s.path().join("open");
        let into = open.join("mine");
        std::fs::create_dir_all(&into).unwrap();
        mode(&into, 0o755);
        mode(&open, 0o777);
        let dir = into.join("tobii-linux-9.9.9");
        assert_eq!(
            refused_at(private_folder(&into, &dir, me(), &private)),
            Some(open.clone())
        );
        assert!(!download_folder_ok(&into));
        // Sticky, and this user's: nobody else can move `mine`.
        mode(&open, 0o1777);
        assert_eq!(
            refused_at(private_folder(&into, &dir, me(), &private)),
            None
        );

        // The folder holding a symlink on the way counts, as the path is
        // printed with the link in it — even when the link's target is fine.
        mode(&open, 0o777);
        let safe = s.path().join("safe");
        std::fs::create_dir(&safe).unwrap();
        mode(&safe, 0o755);
        let link = open.join("link");
        std::os::unix::fs::symlink(&safe, &link).unwrap();
        assert_eq!(
            refused_at(private_folder(&link, &link.join("v"), me(), &private)),
            Some(open.clone())
        );
        assert_eq!(
            refused_at(private_folder(&safe, &safe.join("v"), me(), &private)),
            None,
            "the target, reached directly, is fine"
        );
        mode(&open, 0o755);
    }

    /// A group that can write counts as this user's only when it is their
    /// private group — asked, not assumed from the gid.
    #[test]
    fn a_group_writable_download_folder_is_refused_unless_the_group_is_private() {
        let s = download_scratch("dlgroup");
        let into = s.path().join("into");
        std::fs::create_dir(&into).unwrap();
        mode(&into, 0o775);
        let dir = into.join("tobii-linux-9.9.9");
        assert_eq!(
            refused_at(private_folder(&into, &dir, me(), &|_| false)),
            Some(into.clone())
        );
        assert_eq!(
            refused_at(private_folder(&into, &dir, me(), &|_| true)),
            None
        );

        mode(&into, 0o755);
        std::fs::create_dir(&dir).unwrap();
        mode(&dir, 0o775);
        assert_eq!(
            refused_at(private_folder(&into, &dir, me(), &|_| false)),
            Some(dir.clone())
        );
        assert_eq!(
            refused_at(private_folder(&into, &dir, me(), &|_| true)),
            None
        );

        // The shared rule has no sticky exemption: that is each caller's.
        let m = std::fs::metadata(&dir).unwrap();
        assert_eq!(
            foreign_writer(&m, &|_| false)
                .as_deref()
                .map(|w| w.contains("gid")),
            Some(true)
        );
        mode(&dir, 0o1777);
        let m = std::fs::metadata(&dir).unwrap();
        assert_eq!(foreign_writer(&m, &|_| true).as_deref(), Some("anyone"));
        mode(&dir, 0o755);
    }

    /// Whose a folder is, told apart with a stand-in euid — making one that
    /// belongs to another real account needs root. To a process that is not
    /// this test's user, every folder this test made is someone else's.
    #[test]
    fn a_folder_another_account_owns_is_refused_sticky_or_not() {
        let s = download_scratch("dlowner");
        let into = s.path().join("into");
        std::fs::create_dir(&into).unwrap();
        mode(&into, 0o755);
        let dir = into.join("tobii-linux-9.9.9");
        let private = |_| true;
        // Mine: taken first, because as root the folder is given away below and
        // does not come back.
        assert_eq!(
            refused_at(private_folder(&into, &dir, me(), &private)),
            None
        );
        // Someone else's. As root every folder this test made is root's, and
        // root may own any of them, so it is given to a spare uid and asked as
        // a third account. CI runs the tests as root; this must hold there too.
        let stranger = if me() == 0 {
            // Root: give the folder away, since root may own any folder and
            // would not be refused. In a user namespace without a mapping for
            // 4242 the kernel refuses that, and then no folder here can belong
            // to another account, so there is nothing left to check.
            if std::os::unix::fs::chown(&into, Some(4242), None).is_err() {
                return;
            }
            4243
        } else {
            me().wrapping_add(4242)
        };
        // Its owner can rename anything in it, so the chosen folder — the
        // first on the way up — is the one refused.
        assert_eq!(
            refused_at(private_folder(&into, &dir, stranger, &private)),
            Some(into.clone())
        );
        // A sticky shared folder is fine only when its owner is this user, root
        // or the overflow uid: the owner can move entries in it, sticky or not.
        mode(&into, 0o1777);
        let why = match chosen_folder(&into, stranger, &private) {
            Err(InstallError::UnsafeFolder { path, why }) => {
                assert_eq!(path, into);
                why
            }
            other => panic!("{other:?}"),
        };
        assert!(why.contains("its owner"), "{why}");
        // The overflow uid owns /tmp and everything above it inside a toolbox
        // or `unshare -c`, so a sticky folder of its is not refused. Only root
        // can make one, so only a root run checks it.
        if me() == 0 {
            std::os::unix::fs::chown(&into, Some(OVERFLOW_UID), None).unwrap();
            assert!(chosen_folder(&into, stranger, &private).is_ok());
            std::os::unix::fs::chown(&into, Some(0), None).unwrap();
        }
        mode(&into, 0o755);
    }

    /// Who may own a folder on the way to a download: this user, root, and the
    /// overflow uid root's `/` and `/home` show up as inside a toolbox or
    /// `unshare -c` — and nobody else.
    #[test]
    fn root_and_the_overflow_uid_may_own_the_folders_above_a_download() {
        assert!(may_own(1000, 1000));
        assert!(may_own(0, 1000));
        assert!(may_own(65534, 1000));
        assert!(!may_own(1001, 1000));
        assert!(!may_own(1000, 1001));
    }

    /// The folder the download makes is 0755 whatever the umask, so it passes
    /// its own check; and one it made and then refused is not left behind for
    /// every retry to be refused on.
    #[test]
    fn the_version_folder_is_made_private_and_not_left_behind_when_refused() {
        let s = download_scratch("dlmake");
        let into = s.path().join("into");
        std::fs::create_dir(&into).unwrap();
        mode(&into, 0o755);
        let dir = into.join("tobii-linux-9.9.9");
        make_version_folder(&into, &dir, me(), &|_| false).expect("made and kept");
        let m = std::fs::symlink_metadata(&dir).unwrap();
        assert_eq!(
            std::os::unix::fs::MetadataExt::mode(&m) & 0o022,
            0,
            "nobody else may write the folder the download makes"
        );
        // Taken as it is when it is already there and fine.
        make_version_folder(&into, &dir, me(), &|_| false).expect("reused");
        std::fs::remove_dir(&dir).unwrap();

        // Refused after it was made — here, by a stand-in euid it does not
        // belong to — and so removed again.
        let stranger = me().wrapping_add(4242);
        assert!(make_version_folder(&into, &dir, stranger, &|_| true).is_err());
        assert!(!dir.exists(), "the refused folder this call made is gone");
        // One this call did not make stays: it is not ours to remove.
        std::fs::create_dir(&dir).unwrap();
        mode(&dir, 0o777);
        assert!(make_version_folder(&into, &dir, me(), &|_| true).is_err());
        assert!(dir.exists(), "a folder already there is left as it was");
        mode(&dir, 0o755);
    }

    #[test]
    fn a_private_group_is_the_users_own_with_no_one_else_in_it() {
        let passwd = "root:x:0:0::/root:/bin/bash\nu:x:1000:1000::/home/u:/bin/bash\n\
                      v:x:1001:100::/home/v:/bin/bash\n";
        assert!(private_group_in(passwd, "u:x:1000:\n", 1000, 1000));
        assert!(private_group_in(passwd, "u:x:1000:u\n", 1000, 1000));
        // Someone else listed in it.
        assert!(!private_group_in(passwd, "u:x:1000:u,v\n", 1000, 1000));
        // A second line for the same gid, with someone in it.
        assert!(!private_group_in(
            passwd,
            "u:x:1000:\nalias:x:1000:v\n",
            1000,
            1000
        ));
        // Not in /etc/group at all: LDAP or sssd, which cannot be seen from here.
        assert!(!private_group_in(passwd, "users:x:100:\n", 1000, 1000));
        // Not the user's primary group, however empty.
        assert!(!private_group_in(passwd, "users:x:100:\n", 100, 1000));
        // Another account's primary group too.
        let shared = format!("{passwd}w:x:1002:1000::/home/w:/bin/sh\n");
        assert!(!private_group_in(&shared, "u:x:1000:\n", 1000, 1000));
        // Asked for another user.
        assert!(!private_group_in(passwd, "u:x:1000:\n", 1000, 1001));
    }
}
