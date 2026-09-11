//! `tobii uninstall`: remove an install made by `install.sh` or
//! `scripts/build.sh --install` — and, only when asked, what it wrote.
//!
//! # Why this is a subcommand and not a script beside `install.sh`
//!
//! The in-app updater replaces the binaries and nothing else. A script dropped
//! at install time would stay at the version that was installed while the
//! program it removes moved on, and would be the one part of an install that
//! never learned about a new file. It would also have to re-implement the
//! question that decides whether anything may be deleted at all — does a
//! package manager own this copy? — which `tobii_update::install::ownership_of`
//! already answers, with the failure modes (a broken `dpkg`, a hung query, a
//! localized `pacman`) found and tested.
//!
//! # Shape
//!
//! [`plan`] decides everything. It reads the filesystem (under a root it is
//! given, so tests can point it at a temporary tree) and runs nothing: package
//! ownership, `--version` identification, writability and the process list are
//! all handed to it. [`execute`] performs a plan. What sits between them — the
//! questions, stopping running copies, the udev step — is in [`run`].
//!
//! # What it never does
//!
//! * Delete anything but fixed, known names. The install manifest says where to
//!   *look*; it is never a list of things to delete.
//! * Remove a directory tree it did not create. Config and state directories
//!   are emptied of known names and then `remove_dir`'d, so a hand-made backup
//!   survives and is reported.
//! * Touch a package-managed copy, `/usr/lib/udev/rules.d`, or a Wine prefix.
//! * Create an icon cache, or name `icon-theme.cache` or `mimeinfo.cache` for
//!   removal. An `icon-theme.cache` that exists is refreshed, and the refresh
//!   tool itself deletes the cache when no icons are left under it — which
//!   leaves the directory as it was before the install. See
//!   [`refresh_icon_cache`].
//! * Run a program as root that anyone but root could have put where it is.
//! * Run or remove a binary it found by inference unless it is an ELF program,
//!   owned by this user (or root), in directories no one else can write — see
//!   [`Fs::vet`]. A wrapper script of the user's, another user's copy, or one
//!   in a shared directory is left, with the reason, and never run.
//! * Look along `PATH`. It looks in exactly these places: this mode's install
//!   manifest, the menu entry's and the autostart entry's `Exec`,
//!   `~/.local/bin`, where the running program is, and `--bindir`. A `PATH`
//!   scan reached users' own wrappers, other users' copies in shared
//!   directories and `cargo install`ed copies, and ran them to ask what they
//!   were.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tobii_config::{autostart, paths};
use tobii_update::install::{is_build_tree, Ownership, BINARIES};

use crate::{bridge, CmdResult};

pub const USAGE: &str =
    "usage: tobii uninstall [--dry-run] [--yes] [--purge] [--udev] [--system] [--bindir DIR]

  --dry-run    print the plan and change nothing (do this first)
  --yes        do not ask; required when there is no terminal to ask on.
               --yes also stops running copies with SIGTERM if they do not quit
  --purge      also delete your settings, calibration, models and log
  --udev       also remove the udev rule from /etc/udev/rules.d (uses sudo)
  --system     remove a `sudo ./install.sh --system` install (run with sudo)
  --bindir DIR also look in DIR for an install (absolute path; repeatable)";

/// The rules `install-payload.sh` writes (60-) and used to write (99-).
const UDEV_RULES: [&str; 2] = [
    "/etc/udev/rules.d/60-tobii.rules",
    "/etc/udev/rules.d/99-tobii.rules",
];

/// Where a package puts its own rule. Never touched; only looked at.
const PACKAGE_UDEV_RULE: &str = "/usr/lib/udev/rules.d/60-tobii.rules";

/// Caches that list other programs' files too. This program never names one
/// for removal, whatever a bug elsewhere in this file might say. (Refreshing
/// an existing `icon-theme.cache` can still end with the cache tool deleting
/// it; see [`refresh_icon_cache`].)
const NEVER_REMOVE: [&str; 2] = ["icon-theme.cache", "mimeinfo.cache"];

// ------------------------------------------------------------------- inputs

/// What to do, from the command line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Options {
    pub dry_run: bool,
    pub yes: bool,
    pub purge: bool,
    pub udev: bool,
    pub system: bool,
    pub bindirs: Vec<PathBuf>,
}

/// Parse the arguments after `uninstall`. `Ok(None)` means help was asked for.
pub fn parse_options(args: &[String]) -> Result<Option<Options>, String> {
    let mut o = Options::default();
    let mut it = args.iter();
    let absolute = |d: &str| {
        let p = PathBuf::from(d);
        if p.is_absolute() {
            Ok(p)
        } else {
            Err(format!("--bindir needs an absolute path, not `{d}`"))
        }
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" | "-n" => o.dry_run = true,
            "--yes" | "-y" => o.yes = true,
            "--purge" => o.purge = true,
            "--udev" => o.udev = true,
            "--system" => o.system = true,
            "--bindir" => {
                let d = it.next().ok_or("--bindir needs a directory")?;
                o.bindirs.push(absolute(d)?);
            }
            "-h" | "--help" => return Ok(None),
            other => match other.strip_prefix("--bindir=") {
                Some(d) => o.bindirs.push(absolute(d)?),
                None => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
            },
        }
    }
    Ok(Some(o))
}

/// The parts of the environment a plan depends on.
///
/// Paths here are as the user sees them; [`plan`] resolves them under the root
/// it is given.
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub xdg_runtime_dir: Option<PathBuf>,
    /// `TOBII_LOG_FILE`, which moves the log somewhere the user chose.
    pub log_file: Option<PathBuf>,
    /// `PATH`, split, absolute entries only: an empty or relative entry means
    /// "the working directory", which says nothing about where an install is.
    ///
    /// Used for one thing: judging a menu or autostart entry whose `Exec`
    /// names a bare program, by what `PATH` finds. It never adds a place to
    /// look for an install.
    pub path: Vec<PathBuf>,
    /// `WINEPREFIX`, which `tobii bridge install` uses when given no prefix.
    pub wineprefix: Option<PathBuf>,
    pub euid: u32,
    pub sudo_user: Option<String>,
    pub current_exe: Option<PathBuf>,
    pub pid: u32,
    pub ppid: u32,
}

impl Env {
    /// This process's environment.
    ///
    /// Fails rather than guessing when the effective uid cannot be read: the
    /// one thing it guards is running as root by accident, and a guess of
    /// "not root" is the answer that defeats it.
    pub fn from_process() -> Result<Env, String> {
        let status = std::fs::read_to_string("/proc/self/status").map_err(|e| {
            format!("could not read /proc/self/status ({e}), so whether this is root is unknown")
        })?;
        let field = |key: &str| {
            status
                .lines()
                .find_map(|l| l.strip_prefix(key))
                .map(str::trim)
        };
        let euid = field("Uid:")
            .and_then(|v| v.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .ok_or("/proc/self/status has no readable Uid line")?;
        let ppid = field("PPid:").and_then(|v| v.parse().ok()).unwrap_or(0);
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Ok(Env {
            home: var("HOME"),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            xdg_data_home: var("XDG_DATA_HOME"),
            xdg_state_home: var("XDG_STATE_HOME"),
            xdg_runtime_dir: var("XDG_RUNTIME_DIR"),
            log_file: var("TOBII_LOG_FILE"),
            path: std::env::var_os("PATH")
                .map(|v| {
                    std::env::split_paths(&v)
                        .filter(|d| d.is_absolute())
                        .collect()
                })
                .unwrap_or_default(),
            wineprefix: var("WINEPREFIX"),
            euid,
            sudo_user: std::env::var("SUDO_USER").ok().filter(|s| !s.is_empty()),
            current_exe: std::env::current_exe().ok(),
            pid: std::process::id(),
            ppid,
        })
    }

    fn xdg(&self, var: &Option<PathBuf>, fallback: &str) -> PathBuf {
        paths::xdg_dir(
            var.as_deref().map(Path::as_os_str),
            self.home.as_deref().map(Path::as_os_str),
            fallback,
        )
    }
    fn config_home(&self) -> PathBuf {
        self.xdg(&self.xdg_config_home, ".config")
    }
    fn data_home(&self) -> PathBuf {
        self.xdg(&self.xdg_data_home, ".local/share")
    }
    fn state_home(&self) -> PathBuf {
        self.xdg(&self.xdg_state_home, ".local/state")
    }
}

/// A running process, as `/proc` describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub uid: u32,
    /// `readlink /proc/<pid>/exe`, which ends in ` (deleted)` when the file the
    /// process was started from has since been unlinked or replaced.
    pub exe: String,
    pub args: Vec<String>,
    /// Field 22 of `/proc/<pid>/stat`: when the process started, in clock
    /// ticks since boot. With `exe`, what tells this process apart from a
    /// later one the kernel gave the same pid. `None` if it could not be read.
    pub start_time: Option<u64>,
}

/// The questions [`plan`] needs answered about the real system.
///
/// Every path given to these is already resolved under the plan's root.
pub struct Probes<'a> {
    /// Which package manager owns the binaries in a directory.
    pub owner: &'a dyn Fn(&Path) -> Ownership,
    /// The first line a binary prints for `--version`, if it answers.
    pub identify: &'a dyn Fn(&Path) -> Option<String>,
    /// Whether entries in a directory can be created and removed by this user.
    pub writable: &'a dyn Fn(&Path) -> bool,
    /// Whether a group id is this user's own private group, with no one else
    /// in it: a directory that group can write is still only this user's.
    pub private_group: &'a dyn Fn(u32) -> bool,
}

// -------------------------------------------------------------------- plan

/// How an install location came to be looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The install manifest this mode owns. The only source trusted without
    /// asking the binaries what they are.
    Manifest,
    /// The system manifest, seen from a per-user run.
    SystemManifest,
    MenuEntry,
    Autostart,
    ThisProgram,
    /// `~/.local/bin`, where `install.sh` and `build.sh --install` put it when
    /// told nothing else.
    DefaultDir,
    Bindir,
}

impl Via {
    fn label(self) -> &'static str {
        match self {
            Via::Manifest => "the install manifest",
            Via::SystemManifest => "the system install manifest",
            Via::MenuEntry => "the menu entry's Exec",
            Via::Autostart => "the autostart entry's Exec",
            Via::ThisProgram => "where this program runs from",
            Via::DefaultDir => "the default install directory",
            Via::Bindir => "--bindir",
        }
    }
}

/// What was decided about one install location.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Remove,
    /// Neither binary is there.
    NothingThere,
    Package {
        manager: String,
        package: String,
    },
    /// Listed in the system manifest, or not writable by this user and holding
    /// a binary that answered as this program when this user ran it.
    SystemInstall,
    /// This user's own directory, holding a binary that answered as this
    /// program, but not writable by them. `chmod u+w` is the way, not sudo:
    /// root will not run what is in a directory that is not root's.
    NotWritable,
    /// Something may be there — present, or in a directory that cannot be
    /// looked at — but nothing there could be identified as this program.
    Unidentified,
    /// A build tree or an unpacked archive: somewhere a copy runs from, not an
    /// install.
    NotAnInstall(&'static str),
    /// Root would have had to run a program there to identify it, and someone
    /// other than root can change what that program is.
    NotRunAsRoot(String),
}

/// How a binary came to be planned, or not.
#[derive(Debug, Clone, PartialEq)]
pub enum Ident {
    /// The running program itself: what it would answer is known.
    Me(String),
    /// Listed in this mode's own manifest, and an executable ELF file: trusted
    /// without being run.
    Listed,
    /// Listed in the manifest, but not an executable program.
    ListedNotProgram,
    /// Asked with `--version`; what it printed, if it answered.
    Answered(Option<String>),
    /// Not run: this is root, and someone else could have put it there.
    NotRun,
    /// Neither run nor planned — found by inference and failed [`Fs::vet`],
    /// listed in the manifest but another user's, or listed in Cargo's
    /// record: what it is instead, in a few words.
    Refused(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Binary {
    pub name: &'static str,
    pub ident: Ident,
    pub planned: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    pub dir: PathBuf,
    pub via: Vec<Via>,
    pub verdict: Verdict,
    pub binaries: Vec<Binary>,
}

/// One thing to delete.
#[derive(Debug, Clone, PartialEq)]
pub struct Removal {
    pub path: PathBuf,
    pub what: String,
    /// A scratch directory of the updater's own, removed with its contents.
    /// Nothing else is ever removed as a tree.
    pub tree: bool,
}

/// Something left in place, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Kept {
    pub path: PathBuf,
    pub why: String,
}

/// Lines to drop from an install manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct ManifestEdit {
    pub file: PathBuf,
    /// Install directories whose `bindir=` lines go.
    pub drop: Vec<PathBuf>,
    /// Nothing but blanks and comments would be left, so the file goes too.
    pub then_empty: bool,
}

/// Everything an uninstall would do, decided before anything is done.
#[derive(Debug, Default)]
pub struct Plan {
    /// Paths are resolved under this. `/` except in tests.
    pub root: PathBuf,
    /// Who the plan is for: the udev commands need `sudo` unless this is 0.
    pub euid: u32,
    /// Set when nothing may be done at all.
    pub refusal: Option<String>,
    pub locations: Vec<Location>,
    /// Running copies of binaries that are about to be removed.
    pub stop: Vec<Proc>,
    /// Hubs running from somewhere this run is NOT removing. The hub is
    /// single-instance, so asking "the" hub to quit over D-Bus could reach one
    /// of these instead of ours.
    pub other_hubs: Vec<Proc>,
    /// Hubs whose program has been deleted — a package removed while its hub
    /// ran. Nothing of them is left to remove, but they hold the tracker and
    /// the hub's D-Bus name until they quit, so this run offers to stop them.
    pub orphan_hubs: Vec<Proc>,
    pub remove: Vec<Removal>,
    /// The running `tobii`, when it is being removed. Deleted after everything
    /// else, and only if nothing else failed: unlinking a running executable
    /// is fine on Linux, and keeping it until the end means any failure
    /// leaves a `tobii` to run this again with.
    pub self_exe: Option<PathBuf>,
    /// The running `tobii`, symlinks resolved, whether or not it is being
    /// removed. Run from an unpacked release archive it stays, and it is what
    /// removes the bridge from a Wine prefix.
    pub running: Option<PathBuf>,
    pub manifests: Vec<ManifestEdit>,
    /// `remove_dir`, in order, each only if empty by then.
    pub rmdirs: Vec<PathBuf>,
    /// `hicolor` directories whose existing icon cache needs refreshing.
    pub icon_caches: Vec<PathBuf>,
    pub kept: Vec<Kept>,
    /// Commands and facts for the user: things this run does not do itself.
    pub hints: Vec<String>,
    pub warnings: Vec<String>,
    /// Rule files for the udev step.
    pub udev: Vec<PathBuf>,
    pub udev_notes: Vec<String>,
    pub wine_prefixes: Vec<PathBuf>,
}

impl Plan {
    /// Nothing to remove, rewrite or stop.
    pub fn is_empty(&self) -> bool {
        self.remove.is_empty()
            && self.self_exe.is_none()
            && self.manifests.is_empty()
            && self.rmdirs.is_empty()
    }

    fn push_remove(&mut self, path: PathBuf, what: impl Into<String>, tree: bool) {
        // Belt and braces for the two caches that also index other programs'
        // files. No code path above names them; this makes sure none can.
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| NEVER_REMOVE.contains(&n))
        {
            return;
        }
        if self.remove.iter().any(|r| r.path == path) {
            return;
        }
        self.remove.push(Removal {
            path,
            what: what.into(),
            tree,
        });
    }

    fn keep(&mut self, path: PathBuf, why: impl Into<String>) {
        self.kept.push(Kept {
            path,
            why: why.into(),
        });
    }

    /// Carry out an [`entry_decision`] about the desktop entry `entry`, shown
    /// as `label`. `true` when it stays.
    fn settle_entry(&mut self, entry: PathBuf, label: &str, d: Decision) -> bool {
        match d {
            Decision::Absent => false,
            Decision::Remove(why) => {
                self.push_remove(entry, format!("{label} — {why}"), false);
                false
            }
            Decision::Keep(why) => {
                self.keep(entry, why);
                true
            }
        }
    }
}

/// Whether a path leads anywhere.
enum Presence {
    There,
    /// It, or a directory on the way, does not exist — a dangling symlink
    /// included.
    Gone,
    /// It cannot be told (no permission, a symlink loop): the error.
    Unknown(String),
}

/// Whether a failed lookup says the path is not there: it, or a directory on
/// the way, does not exist. Any other error says nothing about what is there.
fn is_gone(e: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
}

/// Filesystem access under a root.
struct Fs<'a> {
    root: &'a Path,
    /// Who owns a file, given its real path and metadata: the metadata's uid.
    /// A test replaces it, because a file owned by another user cannot be
    /// made without root.
    owner: fn(&Path, &std::fs::Metadata) -> u32,
}

fn owner_of(_: &Path, m: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    m.uid()
}

/// The overflow uid: what a file whose owner has no mapping in this user
/// namespace shows up as. Inside a toolbox or `unshare -c`, root's `/` and
/// `/home` are owned by it.
const OVERFLOW_UID: u32 = 65534;

#[cfg(test)]
thread_local! {
    /// Every path [`Fs::program_check`] opened, for the tests that check it is
    /// the path then run.
    static PROGRAM_CHECKED: std::cell::RefCell<Vec<PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Every path [`open_regular`] actually called open() on, for the test that
    /// checks a device or FIFO is refused by looking, before any open.
    static OPENED: std::cell::RefCell<Vec<PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Open `real` to read it, only if it is a regular file.
///
/// Whatever is opened with this sits somewhere another user may have put
/// something: a `CACHEDIR.TAG`, Cargo's record, a binary. So a symlink at its
/// last component is not followed (`O_NOFOLLOW`), a FIFO does not wait for a
/// writer (`O_NONBLOCK`), and what was opened is `fstat`ed and refused unless
/// it is a regular file — a device, FIFO or socket is never read.
fn open_regular(real: &Path) -> Result<(std::fs::File, std::fs::Metadata), String> {
    use std::os::unix::fs::OpenOptionsExt;
    // Looked at before it is opened, not only after. For a device, opening is
    // itself an action — some arm a watchdog, some rewind a tape, some block —
    // and as root this is asked of paths inside directories another user
    // controls. lstat refuses a device, FIFO, socket or symlink without touching
    // it. The fstat below still checks what was actually opened, because the path
    // can change in between: a regular file swapped for a symlink is stopped by
    // O_NOFOLLOW, a FIFO by O_NONBLOCK, and a user cannot make a device node to
    // swap in.
    let before =
        std::fs::symlink_metadata(real).map_err(|e| format!("it cannot be looked at ({e})"))?;
    if !before.file_type().is_file() {
        return Err("it is not a regular file".into());
    }
    #[cfg(test)]
    OPENED.with(|o| o.borrow_mut().push(real.to_path_buf()));
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(real)
        .map_err(|e| format!("it cannot be opened ({e})"))?;
    let m = f
        .metadata()
        .map_err(|e| format!("it cannot be looked at ({e})"))?;
    if !m.is_file() {
        return Err("it is not a regular file".into());
    }
    Ok((f, m))
}

/// Why a binary found by inference is neither run nor planned.
enum Unvetted {
    /// Not a program, someone else's, or somewhere others can change it: in a
    /// few words, and in full.
    Refused { short: String, why: String },
    /// This is root, and someone other than root can change what it is.
    NotAsRoot(String),
}

/// Who other than its owner can write a file or directory, if anyone.
///
/// A group write bit counts unless the group is the user's own private group.
/// A directory with the sticky bit is left out: in one, only an entry's owner
/// (or the directory's, or root) can rename or delete that entry, and the
/// entry below it is checked on its own.
fn foreign_writer(m: &std::fs::Metadata, private_group: &dyn Fn(u32) -> bool) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let mode = m.mode();
    if m.is_dir() && mode & 0o1000 != 0 {
        return None;
    }
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

impl<'a> Fs<'a> {
    fn new(root: &'a Path) -> Fs<'a> {
        Fs {
            root,
            owner: owner_of,
        }
    }
    /// Who owns `real` (a path under the root). `lstat` when `link`, so a
    /// symlink's own owner; otherwise the file it resolves to.
    fn uid(&self, real: &Path, link: bool) -> Result<u32, String> {
        let m = if link {
            real.symlink_metadata()
        } else {
            std::fs::metadata(real)
        };
        m.map(|m| (self.owner)(real, &m))
            .map_err(|e| format!("{} cannot be looked at ({e})", self.logical(real).display()))
    }
    /// Where `p` really is.
    fn at(&self, p: &Path) -> PathBuf {
        self.root.join(p.strip_prefix("/").unwrap_or(p))
    }
    fn exists(&self, p: &Path) -> bool {
        self.at(p).symlink_metadata().is_ok()
    }
    fn is_file_or_link(&self, p: &Path) -> bool {
        self.at(p)
            .symlink_metadata()
            .is_ok_and(|m| m.is_file() || m.file_type().is_symlink())
    }
    fn is_real_dir(&self, p: &Path) -> bool {
        self.at(p).symlink_metadata().is_ok_and(|m| m.is_dir())
    }
    /// A regular file, after symlinks, with an execute bit: what a `PATH`
    /// lookup would run.
    fn is_executable_file(&self, p: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(self.at(p))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    fn presence(&self, p: &Path) -> Presence {
        match self.at(p).canonicalize() {
            Ok(_) => Presence::There,
            Err(e) if is_gone(&e) => Presence::Gone,
            Err(e) => Presence::Unknown(e.to_string()),
        }
    }
    /// A real path (under the root) as the user sees it.
    fn logical(&self, real: &Path) -> PathBuf {
        let canon_root = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.to_path_buf());
        for root in [canon_root.as_path(), self.root] {
            if let Ok(rel) = real.strip_prefix(root) {
                return Path::new("/").join(rel);
            }
        }
        real.to_path_buf()
    }
    /// `p` with every symlink resolved, as the user sees it. `None` if it does
    /// not exist.
    fn canon(&self, p: &Path) -> Option<PathBuf> {
        let real = self.at(p).canonicalize().ok()?;
        Some(self.logical(&real))
    }
    fn canon_or(&self, p: &Path) -> PathBuf {
        self.canon(p).unwrap_or_else(|| p.to_path_buf())
    }
    /// `p` with its directory resolved and its last component left alone: the
    /// name a removal of `p` unlinks. For a symlink that is the link, never
    /// what it points at.
    fn unlinked_name(&self, p: &Path) -> PathBuf {
        match (p.parent(), p.file_name()) {
            (Some(d), Some(n)) => self.canon(d).map_or_else(|| p.to_path_buf(), |d| d.join(n)),
            _ => p.to_path_buf(),
        }
    }
    /// Every name `p` passes through on the way to the file it runs: `p`, then
    /// each symlink's target in turn, all as [`Fs::unlinked_name`]s. Removing
    /// any one of them leaves `p` running nothing.
    fn link_chain(&self, p: &Path) -> Vec<PathBuf> {
        let mut chain = vec![self.unlinked_name(p)];
        // 40 is the kernel's own limit on symlinks followed in one lookup.
        while chain.len() <= 40 {
            let real = self.at(chain.last().expect("the chain is never empty"));
            let Ok(target) = std::fs::read_link(&real) else {
                break;
            };
            // An absolute target replaces the directory in `join`.
            let next = match real.parent() {
                Some(d) => d.join(target),
                None => target,
            };
            let next = self.unlinked_name(&self.logical(&next));
            if chain.contains(&next) {
                break;
            }
            chain.push(next);
        }
        chain
    }
    /// `Ok` if `real` — a resolved path under the root, the one that will be
    /// run — is a program: a regular file with an execute bit and an ELF
    /// header, what every build and release of this program is. Opened with
    /// [`open_regular`], so a symlink put there since it was resolved is not
    /// followed. `Err` says what it is instead.
    fn program_check(&self, real: &Path) -> Result<(), String> {
        use std::io::Read;
        use std::os::unix::fs::PermissionsExt;
        #[cfg(test)]
        PROGRAM_CHECKED.with(|c| c.borrow_mut().push(real.to_path_buf()));
        let (mut f, m) = open_regular(real)?;
        if m.permissions().mode() & 0o111 == 0 {
            return Err("it is not executable".into());
        }
        let mut magic = [0u8; 4];
        f.read_exact(&mut magic)
            .map_err(|e| format!("it cannot be read ({e})"))?;
        if &magic != b"\x7fELF" {
            if magic.starts_with(b"#!") {
                return Err(
                    "it is a script, not a build of this program — a wrapper of yours?".into(),
                );
            }
            return Err("it is not an ELF program".into());
        }
        Ok(())
    }
    /// `Err` unless `real` (`lstat` when `link`, so a symlink's own owner) is
    /// this user's or root's: in a few words, and in full. Another user's
    /// copy is theirs to remove.
    fn owned_by_us(&self, real: &Path, link: bool, euid: u32) -> Result<(), (String, String)> {
        let uid = self
            .uid(real, link)
            .map_err(|e| ("not looked at".to_string(), e))?;
        if uid == euid || uid == 0 {
            return Ok(());
        }
        Err((
            format!("owned by uid {uid}"),
            format!(
                "{} is owned by uid {uid} — they can run tobii uninstall themselves",
                self.logical(real).display()
            ),
        ))
    }
    /// Whether a binary found by inference may be run to ask what it is, and
    /// then removed. `Ok` is the file it resolves to, under the root: the file
    /// every check was made on — [`Fs::program_check`] included — and so the
    /// only path that may be run. Running the name instead would follow its
    /// symlinks again.
    ///
    /// It must be a program: a script in `~/.local/bin` that prints
    /// "tobii-gtk 0.3.0" is a wrapper of the user's, not an install. As root,
    /// [`Fs::root_may_run`] is asked first, before anything there is opened.
    /// Otherwise every name on the way from `file` to the file it runs
    /// ([`Fs::link_chain`], each `lstat`ed) and that file must be this user's
    /// or root's, and neither that file, nor any directory one of those names
    /// is in, nor the file's own directory, may be owned or writable by anyone
    /// else (see [`foreign_writer`]). Those directories decide which file the
    /// name leads to: whoever can write one can re-point a link in it between
    /// the check and the run, and so choose what runs.
    ///
    /// Every directory above those is checked for writers too — whoever can
    /// write one can rename what is under it — but its owner may also be
    /// [`OVERFLOW_UID`], or every install inside a toolbox or `unshare -c`
    /// would be refused. Root's own check is stricter: all of it root's.
    fn vet(
        &self,
        file: &Path,
        euid: u32,
        private_group: &dyn Fn(u32) -> bool,
    ) -> Result<PathBuf, Unvetted> {
        let refused = |short: &str, why: String| Unvetted::Refused {
            short: short.to_string(),
            why,
        };
        let not_a_program = |why: String| {
            let short = if why.contains("a script") {
                "a script"
            } else {
                "not a program"
            };
            refused(short, why)
        };
        if euid == 0 {
            let real = self.root_may_run(file).map_err(Unvetted::NotAsRoot)?;
            self.program_check(&real).map_err(not_a_program)?;
            return Ok(real);
        }
        let real = self
            .at(file)
            .canonicalize()
            .map_err(|e| refused("not resolved", format!("it cannot be resolved ({e})")))?;
        self.program_check(&real).map_err(not_a_program)?;
        let ours = |uid: u32| uid == euid || uid == 0;
        let chain: Vec<PathBuf> = self.link_chain(file).iter().map(|n| self.at(n)).collect();
        let names = chain.iter().map(|n| (n, true));
        for (p, link) in names.chain([(&real, false)]) {
            self.owned_by_us(p, link, euid)
                .map_err(|(short, why)| refused(&short, why))?;
        }
        let others = |m: &std::fs::Metadata, p: &Path, what: &str| {
            foreign_writer(m, private_group).map(|who| {
                refused(
                    what,
                    format!(
                        "{what}: {} can be written by {who}, so what runs there is not yours \
                         alone to choose",
                        self.logical(p).display()
                    ),
                )
            })
        };
        let m = std::fs::metadata(&real)
            .map_err(|e| refused("not looked at", format!("it cannot be looked at ({e})")))?;
        if let Some(r) = others(&m, &real, "a file others can change") {
            return Err(r);
        }
        let dir = "in a directory others can write";
        let look = |d: &Path| {
            std::fs::metadata(d).map_err(|e| {
                refused(
                    "not looked at",
                    format!("{} cannot be looked at ({e})", self.logical(d).display()),
                )
            })
        };
        let belongs = |d: &Path, uid: u32| {
            refused(
                dir,
                format!(
                    "{dir}: {} belongs to uid {uid}, so what runs there is not yours alone to \
                     choose",
                    self.logical(d).display()
                ),
            )
        };
        // Each directory a name on the way is in and the file's own, then
        // every directory above them, whose owner may also be OVERFLOW_UID.
        let near: Vec<&Path> = chain
            .iter()
            .filter_map(|n| n.parent())
            .chain(real.parent())
            .collect();
        let above = near.iter().flat_map(|d| d.ancestors().skip(1));
        let mut seen: Vec<&Path> = Vec::new();
        for (d, is_near) in near
            .iter()
            .map(|d| (*d, true))
            .chain(above.map(|a| (a, false)))
        {
            if seen.contains(&d) {
                continue;
            }
            seen.push(d);
            let m = look(d)?;
            let uid = (self.owner)(d, &m);
            if !ours(uid) && (is_near || uid != OVERFLOW_UID) {
                return Err(belongs(d, uid));
            }
            if let Some(r) = others(&m, d, dir) {
                return Err(r);
            }
        }
        Ok(real)
    }
    /// Whether root may run `file` to ask what it is: only if no one other
    /// than root can change what it is. Each directory a name on the way to
    /// the file is in ([`Fs::link_chain`]) and every directory above, and the
    /// file it resolves to and every directory above that, must all be root's
    /// and writable by no one else. `Ok` is the file it resolves to — what was
    /// checked, and so what may be run — and `Err` says why not. Asked of a
    /// directory, it says whether anything in it may be opened as root.
    fn root_may_run(&self, file: &Path) -> Result<PathBuf, String> {
        use std::os::unix::fs::MetadataExt;
        let real = self
            .at(file)
            .canonicalize()
            .map_err(|e| format!("{} cannot be resolved ({e})", file.display()))?;
        let name_dirs: Vec<PathBuf> = self
            .link_chain(file)
            .iter()
            .filter_map(|n| n.parent())
            .map(|d| self.at(d))
            .collect();
        let above_names = name_dirs.iter().flat_map(|d| d.ancestors());
        for p in above_names.chain(real.ancestors()) {
            let shown = self.logical(p);
            let Ok(m) = std::fs::metadata(p) else {
                return Err(format!("{} cannot be looked at", shown.display()));
            };
            let uid = (self.owner)(p, &m);
            if uid != 0 {
                return Err(format!(
                    "{} is owned by uid {uid}, not by root",
                    shown.display()
                ));
            }
            if m.mode() & 0o022 != 0 {
                return Err(format!(
                    "{} can be written by its group or by anyone",
                    shown.display()
                ));
            }
        }
        Ok(real)
    }
    /// The first `limit` bytes of a regular file, as text, opened with
    /// [`open_regular`]. `None` if it cannot be read, or is not one.
    fn read_regular(&self, p: &Path, limit: u64) -> Option<String> {
        use std::io::Read;
        let mut buf = Vec::new();
        open_regular(&self.at(p))
            .ok()?
            .0
            .take(limit)
            .read_to_end(&mut buf)
            .ok()?;
        Some(String::from_utf8_lossy(&buf).into_owned())
    }
    fn read(&self, p: &Path) -> Option<String> {
        std::fs::read_to_string(self.at(p)).ok()
    }
    /// File names in a directory, sorted. `None` if it cannot be read.
    fn list(&self, dir: &Path) -> Option<Vec<String>> {
        let mut names: Vec<String> = std::fs::read_dir(self.at(dir))
            .ok()?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        Some(names)
    }
    /// The absolute `bindir=` values in a manifest. Blank lines, comments,
    /// unknown keys and relative paths are ignored.
    fn manifest_bindirs(&self, file: &Path) -> Vec<PathBuf> {
        self.read(file)
            .map(|t| manifest_bindirs(&t))
            .unwrap_or_default()
    }
}

fn manifest_bindirs(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("bindir="))
        .map(|v| PathBuf::from(v.trim()))
        .filter(|p| p.is_absolute())
        .collect()
}

/// Whether a manifest line still says anything once `drop` is gone.
fn manifest_line_survives(line: &str, dropped: &dyn Fn(&Path) -> bool) -> bool {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return false;
    }
    match t.strip_prefix("bindir=") {
        Some(v) => !dropped(Path::new(v.trim())),
        None => true,
    }
}

fn is_pid(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// An updater or installer temporary in an install directory.
///
/// `.tobii-update-<pid>` (the updater's work directory),
/// `.tobii-update-probe-<pid>`, `.<bin>.new-<pid>` (staged by the updater and
/// by `install-payload.sh`) and `.<bin>.old-<pid>` (the updater's rollback
/// backup, left if it was killed mid-swap).
fn is_install_scratch(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix(".tobii-update-probe-") {
        return is_pid(rest);
    }
    if let Some(rest) = name.strip_prefix(".tobii-update-") {
        return is_pid(rest);
    }
    BINARIES.iter().any(|b| {
        [".new-", ".old-"].iter().any(|kind| {
            name.strip_prefix(&format!(".{b}{kind}"))
                .is_some_and(is_pid)
        })
    })
}

/// The temporary `install-payload.sh` writes the manifest to before renaming
/// it into place: `installs.new-<pid>`.
fn is_manifest_scratch(name: &str) -> bool {
    name.strip_prefix(paths::MANIFEST_FILE)
        .and_then(|r| r.strip_prefix(".new-"))
        .is_some_and(is_pid)
}

/// A build tree or an unpacked release archive: somewhere a copy of this
/// program runs from, which no install made.
///
/// A Cargo output directory is told by the `CACHEDIR.TAG` Cargo writes at the
/// top of it, whose text says it was "created by cargo" — whatever that
/// directory is called (`CARGO_TARGET_DIR` and `build.target-dir` can call it
/// anything). Binaries sit one level below it (`<dir>/release`) or two
/// (`<dir>/<triple>/release`, which is how scripts/release.sh builds), and no
/// deeper, so only those two are looked at: looking further would call
/// anything that merely sits under a target directory a build tree — a test
/// HOME, for one. `is_build_tree` (`target/release`, `target/debug`, by name)
/// is a second signal, for a tree whose tag is gone.
///
/// An unpacked archive is told by the release layout, `install.sh` AND
/// `assets/install-payload.sh` beside the binary, which every archive since
/// v0.1.0 has. An `install.sh` alone is not enough: a user may keep one of
/// their own in `~/.local/bin`, and that must not hide an install there.
///
/// `may_read` false — root, and a directory [`Fs::root_may_run`] refused —
/// skips the tags: nothing someone else controls is opened as root. The name
/// and the archive layout are only looked at, never opened.
fn not_an_install(fs: &Fs, dir: &Path, may_read: bool) -> Option<&'static str> {
    let tagged = |a: &Path| {
        may_read
            && fs
                .read_regular(&a.join("CACHEDIR.TAG"), 1024)
                .is_some_and(|t| t.contains("created by cargo"))
    };
    if is_build_tree(dir) || dir.ancestors().skip(1).take(2).any(tagged) {
        Some("a Cargo build directory")
    } else if fs.is_file_or_link(&dir.join("install.sh"))
        && fs.is_file_or_link(&dir.join("assets/install-payload.sh"))
    {
        Some("an unpacked release archive (install.sh and assets/install-payload.sh are beside it)")
    } else {
        None
    }
}

/// This program's packages, as `cargo install` names them.
const CARGO_PACKAGES: [&str; 2] = ["tobii-cli", "tobii-gtk"];

/// The most of Cargo's record that is read.
const RECORD_LIMIT: u64 = 4 << 20;

/// The binaries in `present` that Cargo's record beside `dir` lists as
/// `cargo install`ed there from this program's packages, each with its
/// package — Cargo's to remove, since a copy removed behind its back leaves
/// that record wrong.
///
/// The record is `.crates.toml` (under `[v1]`, keys `"<package> <version>
/// (<source>)"`, each set to its binaries) and `.crates2.json` (the same keys
/// under `installs`, each with its `bins`). That one exists says only that
/// `cargo install` was once pointed here, at anything: `cargo install --root
/// ~/.local ripgrep` writes one beside `~/.local/bin`. So only what it lists
/// is Cargo's; everything else — everything, when neither can be read — is
/// looked at like anywhere else.
fn cargo_listed(
    fs: &Fs,
    dir: &Path,
    present: &[&'static str],
) -> Vec<(&'static str, &'static str)> {
    // `cargo install` writes only to <root>/bin. A record beside a directory with
    // any other name was written for the `bin` next to it, and reading it as this
    // directory's would keep a copy Cargo never installed — and print a
    // `cargo uninstall --root` that removes a different one.
    if dir.file_name() != Some(std::ffi::OsStr::new("bin")) {
        return Vec::new();
    }
    let Some(root) = dir.parent() else {
        return Vec::new();
    };
    let read = |name: &str| fs.read_regular(&root.join(name), RECORD_LIMIT);
    let entries: Vec<_> = read(".crates.toml")
        .map(|t| crates_toml(&t))
        .unwrap_or_default()
        .into_iter()
        .chain(
            read(".crates2.json")
                .map(|t| crates2_json(&t))
                .unwrap_or_default(),
        )
        .collect();
    // Each binary here with the package of the first of this program's
    // entries that lists it.
    present
        .iter()
        .filter_map(|b| {
            entries.iter().find_map(|(key, bins)| {
                let package = CARGO_PACKAGES
                    .iter()
                    .find(|p| key.strip_prefix(**p).is_some_and(|r| r.starts_with(' ')))?;
                bins.iter().any(|x| x == b).then_some((*b, *package))
            })
        })
        .collect()
}

/// A value in Cargo's record: just enough JSON — and TOML's quoted keys and
/// arrays of strings — to read which binaries it lists.
enum Value {
    Str(String),
    List(Vec<Value>),
    Map(Vec<(String, Value)>),
    Other,
}

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        let hit = self.peek() == Some(c);
        self.i += usize::from(hit);
        hit
    }
    /// Whitespace, line ends, and TOML's `#` comments.
    fn blank(&mut self) {
        while let Some(c) = self.peek() {
            match c {
                b' ' | b'\t' | b'\r' | b'\n' => self.i += 1,
                b'#' => self.line(),
                _ => return,
            }
        }
    }
    /// To the end of the line.
    fn line(&mut self) {
        while self.peek().is_some_and(|c| c != b'\n') {
            self.i += 1;
        }
    }
    fn string(&mut self) -> Option<String> {
        if !self.eat(b'"') {
            return None;
        }
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.peek().is_some_and(|c| c != b'"' && c != b'\\') {
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.s[start..self.i]).ok()?);
            if self.eat(b'"') {
                return Some(out);
            }
            self.i += 1; // the backslash
            let e = self.peek()?;
            self.i += 1;
            match e {
                b'n' => out.push('\n'),
                b't' => out.push('\t'),
                b'r' => out.push('\r'),
                b'b' => out.push('\u{8}'),
                b'f' => out.push('\u{c}'),
                b'u' => {
                    let hex = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
                    self.i += 4;
                    let c = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
                    out.push(c.unwrap_or('\u{fffd}'));
                }
                c => out.push(char::from(c)), // \" \\ \/
            }
        }
    }
    fn value(&mut self, depth: u8) -> Option<Value> {
        self.blank();
        if depth > 32 {
            return None;
        }
        match self.peek()? {
            b'"' => self.string().map(Value::Str),
            open @ (b'[' | b'{') => {
                self.i += 1;
                let close = if open == b'[' { b']' } else { b'}' };
                let (mut list, mut map) = (Vec::new(), Vec::new());
                loop {
                    self.blank();
                    if self.eat(close) {
                        break;
                    }
                    if open == b'[' {
                        list.push(self.value(depth + 1)?);
                    } else {
                        let k = self.string()?;
                        self.blank();
                        if !self.eat(b':') {
                            return None;
                        }
                        map.push((k, self.value(depth + 1)?));
                    }
                    self.blank();
                    if !self.eat(b',') {
                        self.blank();
                        if !self.eat(close) {
                            return None;
                        }
                        break;
                    }
                }
                Some(if open == b'[' {
                    Value::List(list)
                } else {
                    Value::Map(map)
                })
            }
            _ => {
                let start = self.i;
                while self
                    .peek()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || b"+-.".contains(&c))
                {
                    self.i += 1;
                }
                (self.i > start).then_some(Value::Other)
            }
        }
    }
}

fn strings(v: Vec<Value>) -> Vec<String> {
    v.into_iter()
        .filter_map(|x| match x {
            Value::Str(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// `(key, binaries)` for each entry under `[v1]` in `.crates.toml`.
fn crates_toml(text: &str) -> Vec<(String, Vec<String>)> {
    let mut r = Reader {
        s: text.as_bytes(),
        i: 0,
    };
    let mut out = Vec::new();
    let mut in_v1 = false;
    loop {
        r.blank();
        match r.peek() {
            None => break,
            Some(b'[') => {
                let start = r.i;
                r.line();
                let header = String::from_utf8_lossy(&r.s[start..r.i]);
                in_v1 = header.split('#').next().map(str::trim) == Some("[v1]");
            }
            Some(b'"') if in_v1 => {
                let Some(key) = r.string() else {
                    break;
                };
                r.blank();
                if !r.eat(b'=') {
                    break;
                }
                match r.value(0) {
                    Some(Value::List(bins)) => out.push((key, strings(bins))),
                    Some(_) => {}
                    None => break,
                }
            }
            Some(_) => r.line(),
        }
    }
    out
}

/// `(key, bins)` for each entry under `installs` in `.crates2.json`.
fn crates2_json(text: &str) -> Vec<(String, Vec<String>)> {
    let mut r = Reader {
        s: text.as_bytes(),
        i: 0,
    };
    let Some(Value::Map(top)) = r.value(0) else {
        return Vec::new();
    };
    let installs = top.into_iter().find_map(|(k, v)| match v {
        Value::Map(m) if k == "installs" => Some(m),
        _ => None,
    });
    installs
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, v)| {
            let Value::Map(fields) = v else {
                return None;
            };
            let bins = fields.into_iter().find_map(|(k, v)| match v {
                Value::List(b) if k == "bins" => Some(b),
                _ => None,
            })?;
            Some((key, strings(bins)))
        })
        .collect()
}

/// The packages in a [`cargo_listed`] list, each once.
fn cargo_package_list(listed: &[(&str, &str)]) -> String {
    let mut packages: Vec<&str> = Vec::new();
    for (_, p) in listed {
        if !packages.contains(p) {
            packages.push(p);
        }
    }
    packages.join(" ")
}

/// `cargo uninstall` for `packages` installed in `dir`, always with `--root`.
/// Cargo resolves its root from `--root`, then `CARGO_INSTALL_ROOT`, then
/// `install.root` in its config, and only then `$CARGO_HOME` — so a command
/// that leaves `--root` out for a copy in `$CARGO_HOME/bin` removes a
/// different copy whenever either of the first two is set. With it, the
/// command is right in every case.
fn cargo_uninstall(dir: &Path, packages: &str) -> String {
    // Only a `bin` with a parent can be Cargo's (cargo_listed), so `dir` has one.
    let root = dir.parent().unwrap_or(dir);
    format!("cargo uninstall --root {} {packages}", q(root))
}

fn cargo_hint(dir: &Path, listed: &[(&str, &str)]) -> String {
    let bins: Vec<&str> = listed.iter().map(|(b, _)| *b).collect();
    let (it, it_is) = if bins.len() == 1 {
        ("it", "it is")
    } else {
        ("them", "they are")
    };
    format!(
        "{} in {} came from `cargo install`, and Cargo's record beside it lists {it}, so \
         {it_is} left to Cargo — removed behind its back, that record would be wrong. Remove \
         {it} with:\n    {}",
        bins.join(" and "),
        dir.display(),
        cargo_uninstall(dir, &cargo_package_list(listed))
    )
}

/// What a desktop entry runs, as far as it can be told from here.
enum ExecTarget {
    /// An absolute path.
    Path(PathBuf),
    /// A bare name, and every place `PATH` finds it, in `PATH` order.
    Bare { name: String, hits: Vec<PathBuf> },
    /// It cannot be told, and why.
    Unknown(String),
}

/// What a desktop entry's `Exec` runs, read past `env NAME=value …`, with a
/// bare name looked up on `path`.
///
/// `path` is this process's PATH, which is not necessarily the one the
/// session starts entries with; a name it does not find is reported as
/// unknown rather than as gone.
fn exec_target(fs: &Fs, text: &str, path: &[PathBuf]) -> ExecTarget {
    let Some(args) = autostart::exec_arguments(text) else {
        return ExecTarget::Unknown("it has no Exec line that names a program".into());
    };
    let mut words = args.iter().map(String::as_str);
    let mut prog = words.next().unwrap_or_default();
    if Path::new(prog).file_name().is_some_and(|n| n == "env") {
        // `env NAME=value … program …`. An option to env (`-u NAME`, `-i`,
        // `-S`, `--`) changes how the rest is read, so that is not guessed at.
        match words.find(|w| w.starts_with('-') || !w.contains('=')) {
            Some(w) if w.starts_with('-') => {
                return ExecTarget::Unknown(format!(
                    "it runs `env {} …`, and env's options are not read here",
                    sanitize(w)
                ))
            }
            Some(w) => prog = w,
            None => return ExecTarget::Unknown("it runs `env` with no program after it".into()),
        }
    }
    let p = Path::new(prog);
    if p.is_absolute() {
        return ExecTarget::Path(p.to_path_buf());
    }
    if prog.contains('/') {
        return ExecTarget::Unknown(format!(
            "it runs `{}`, a relative path, which depends on where it is started from",
            sanitize(prog)
        ));
    }
    let mut hits: Vec<PathBuf> = Vec::new();
    for d in path.iter().filter(|d| d.is_absolute()) {
        let c = d.join(prog);
        if fs.is_executable_file(&c) {
            let c = fs.unlinked_name(&c);
            if !hits.contains(&c) {
                hits.push(c);
            }
        }
    }
    ExecTarget::Bare {
        name: prog.to_string(),
        hits,
    }
}

enum Decision {
    Absent,
    Remove(String),
    Keep(String),
}

/// Whether a desktop entry of ours goes, judged by the program it runs.
///
/// It goes when that program is being removed or no longer exists. It stays
/// when it runs a copy that is staying — a package's, a system install's, a
/// build tree's — because deleting it would silently switch that copy's menu
/// entry or start-at-login off, with nothing to say it had happened. And it
/// stays when what it runs cannot be told, for the same reason: a guess of
/// "gone" is the one that switches something off.
fn entry_decision(
    fs: &Fs,
    entry: &Path,
    removing: &dyn Fn(&Path) -> bool,
    path: &[PathBuf],
    what: &str,
) -> Decision {
    if !fs.exists(entry) {
        return Decision::Absent;
    }
    let unknown = |why: &str| {
        Decision::Keep(format!(
            "{why}, so whether it runs a copy that stays cannot be told — left as it is"
        ))
    };
    let Some(text) = fs.read(entry) else {
        return unknown("it could not be read");
    };
    let stays = |prog: &Path| {
        Decision::Keep(format!(
            "it runs {}, which is not being removed — deleting it would silently switch {what} \
             off for that copy",
            prog.display()
        ))
    };
    match exec_target(fs, &text, path) {
        ExecTarget::Unknown(why) => unknown(&why),
        ExecTarget::Path(prog) => match fs.presence(&prog) {
            Presence::Gone => Decision::Remove(format!(
                "it runs {}, which no longer exists",
                prog.display()
            )),
            Presence::Unknown(e) => unknown(&format!(
                "it runs {}, which cannot be looked at ({e})",
                prog.display()
            )),
            Presence::There if removing(&prog) => Decision::Remove(format!(
                "it runs {}, which is being removed",
                prog.display()
            )),
            Presence::There => stays(&prog),
        },
        ExecTarget::Bare { name, hits } => {
            let name = sanitize(&name);
            if let Some(stay) = hits.iter().find(|h| !removing(h)) {
                // The first copy PATH finds after this run is what it will
                // run, and that one stays.
                stays(stay)
            } else if hits.is_empty() {
                unknown(&format!(
                    "it runs a bare `{name}`, which is not on this PATH (the session that starts \
                     it may have another)"
                ))
            } else {
                let list: Vec<String> = hits.iter().map(|h| h.display().to_string()).collect();
                Decision::Remove(format!(
                    "it runs `{name}`, which PATH finds only at {} — being removed",
                    list.join(" and ")
                ))
            }
        }
    }
}

/// The removal command for a package, in the tool a person removes with.
fn package_remove_command(manager: &str, package: &str) -> String {
    match manager {
        "pacman" => format!("sudo pacman -R {package}"),
        "dpkg" => format!("sudo apt remove {package}"),
        "rpm" => format!("sudo dnf remove {package}    (openSUSE: sudo zypper remove {package})"),
        other => format!("remove the package {package} with {other}"),
    }
}

fn package_hint(dir: &Path, manager: &str, package: &str) -> String {
    format!(
        "{} belongs to {manager}, as the package {package}, so nothing is removed there. \
         Remove it with:\n    {}\n  That removes the packaged copy only. Copies in your home \
         directory are `tobii uninstall`'s job — this run includes any it found.",
        dir.display(),
        package_remove_command(manager, package)
    )
}

fn system_hint(dir: &Path, exe: &str, self_removed: bool) -> String {
    let mut s = format!(
        "{} is a system-wide install (you cannot write to it, no package owns it, and what is \
         there is this program). Remove it with:\n    sudo {} uninstall --system --bindir {}",
        dir.display(),
        sh_quote(exe),
        q(dir)
    );
    if self_removed {
        s.push_str(&format!(
            "\n  Do that first, or with a copy of tobii that stays: this run removes {exe}."
        ));
    }
    s
}

/// For a directory of this user's own that they cannot write to. Not sudo:
/// root will not run what is in a directory that is not root's
/// ([`Fs::root_may_run`]), and would send them straight back here.
fn chmod_hint(dir: &Path) -> String {
    format!(
        "{} holds this program and is yours, but you cannot write to it, so nothing there is \
         removed. It is not root's, so sudo would not help. Make it writable, then run this \
         again:\n    chmod u+w {}",
        dir.display(),
        q(dir)
    )
}

fn root_refusal(env: &Env) -> String {
    let who = env
        .sudo_user
        .as_deref()
        .map(|u| format!(" through sudo (as {u})"))
        .unwrap_or_default();
    format!(
        "this is running as root{who}. Under sudo HOME is {} — root's home, not yours — so it \
         would look for root's install and miss yours entirely.\n  Run `tobii uninstall` \
         without sudo: it removes files in your home and needs no root.\n  For an install made \
         with `sudo ./install.sh --system`, run `sudo tobii uninstall --system`.",
        env.home
            .as_deref()
            .map(|h| h.display().to_string())
            .unwrap_or_else(|| "unset".into())
    )
}

/// The four files a `sudo ./install.sh` from before `--system` existed left in
/// root's home.
fn root_leftovers() -> [PathBuf; 4] {
    let bin = Path::new("/root/.local/bin");
    let data = Path::new("/root/.local/share");
    [
        bin.join("tobii"),
        bin.join("tobii-gtk"),
        paths::desktop_entry_in(data),
        paths::icon_in(data),
    ]
}

fn root_hint(fs: &Fs) -> Option<String> {
    let files = root_leftovers();
    let mut seen = false;
    let mut denied = false;
    for b in &files[..2] {
        match fs.at(b).symlink_metadata() {
            Ok(_) => seen = true,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => denied = true,
            Err(_) => {}
        }
    }
    let list = files.iter().map(|p| q(p)).collect::<Vec<_>>().join(" ");
    if seen {
        Some(format!(
            "A past `sudo ./install.sh` left a copy in root's home, which this run does not \
             touch. Remove exactly those four files with:\n    sudo rm -f {list}"
        ))
    } else if denied {
        Some(format!(
            "/root cannot be read from here. A past `sudo ./install.sh` may have left \
             these four files there: {list}"
        ))
    } else {
        None
    }
}

/// Wine prefixes the bridge is installed in, as far as they can be found.
///
/// Every Steam library's `compatdata/*/pfx`, scanned directly rather than only
/// for installed games — Steam keeps a prefix after the game is uninstalled —
/// plus the prefixes `tobii bridge install` falls back to when given none
/// (`resolve_prefix` in bridge.rs): `$WINEPREFIX`, then `~/.wine`. Both of
/// those are checked, not only the first: the WINEPREFIX of this shell is not
/// necessarily the one the bridge was installed with.
fn wine_prefixes(fs: &Fs, home: &Path, wineprefix: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for lib in bridge::steam_libraries(&fs.at(home)) {
        let Ok(entries) = std::fs::read_dir(lib.join("steamapps/compatdata")) else {
            continue;
        };
        for e in entries.flatten() {
            let pfx = e.path().join("pfx");
            if pfx.join(bridge::INSTALL_SUBDIR).is_dir() {
                out.push(fs.logical(&pfx));
            }
        }
    }
    let defaults = wineprefix
        .filter(|w| w.is_absolute())
        .map(Path::to_path_buf)
        .into_iter()
        .chain([home.join(".wine")]);
    for w in defaults {
        if fs.is_real_dir(&w.join(bridge::INSTALL_SUBDIR)) {
            out.push(fs.canon_or(&w));
        }
    }
    out.sort();
    out.dedup();
    out
}

const CALIBRATION_WARNING: &str = "calibration.bin is the tracker's calibration. Everything \
     else --purge deletes is quick to set up again; redoing the calibration is the costly part \
     — a full follow-the-dot run at this screen.";

struct Candidate {
    dir: PathBuf,
    via: Vec<Via>,
}

/// Decide what an uninstall would do. Reads the filesystem under `root`; runs
/// nothing itself — `probes.identify` is the one thing that runs a program.
pub fn plan(env: &Env, opts: &Options, root: &Path, probes: &Probes, procs: &[Proc]) -> Plan {
    plan_with_fs(env, opts, Fs::new(root), probes, procs)
}

fn plan_with_fs(env: &Env, opts: &Options, fs: Fs, probes: &Probes, procs: &[Proc]) -> Plan {
    let root = fs.root;
    let mut p = Plan {
        root: root.to_path_buf(),
        euid: env.euid,
        ..Plan::default()
    };

    // ---------------------------------------------------------------- guards
    if opts.system {
        if env.euid != 0 {
            p.refusal = Some(
                "--system removes a system-wide install and needs root: \
                 sudo tobii uninstall --system"
                    .into(),
            );
            return p;
        }
        if opts.purge {
            p.refusal = Some(
                "--purge deletes one person's settings and calibration, which live in their \
                 home — run it as that person, without sudo and without --system"
                    .into(),
            );
            return p;
        }
    } else if env.euid == 0 {
        p.refusal = Some(root_refusal(env));
        return p;
    }
    let home = env.home.clone().filter(|h| h.is_absolute());
    if !opts.system && home.is_none() {
        p.refusal = Some("HOME is not set, so there is no home install to find".into());
        return p;
    }

    let data = if opts.system {
        PathBuf::from(paths::SYSTEM_DATA_DIR)
    } else {
        env.data_home()
    };
    let manifest = paths::manifest_in(&data);
    let system_manifest = paths::manifest_in(Path::new(paths::SYSTEM_DATA_DIR));
    let me = env.current_exe.as_deref().map(|e| fs.canon_or(e));
    p.running = me.clone();
    let exe_display = me
        .as_deref()
        .map(|e| e.display().to_string())
        .unwrap_or_else(|| "tobii".into());

    // ------------------------------------------------------------- discovery
    let mut found: Vec<Candidate> = Vec::new();
    let mut add = |dir: &Path, via: Via| {
        let dir = fs.canon_or(dir);
        match found.iter_mut().find(|c| c.dir == dir) {
            Some(c) if !c.via.contains(&via) => c.via.push(via),
            Some(_) => {}
            None => found.push(Candidate {
                dir,
                via: vec![via],
            }),
        }
    };
    let holds_ours = |d: &Path| BINARIES.iter().any(|b| fs.is_file_or_link(&d.join(b)));
    let manifest_dirs = fs.manifest_bindirs(&manifest);
    for d in &manifest_dirs {
        add(d, Via::Manifest);
    }
    let system_listed: Vec<PathBuf> = if opts.system {
        Vec::new()
    } else {
        fs.manifest_bindirs(&system_manifest)
            .iter()
            .map(|d| fs.canon_or(d))
            .collect()
    };
    let config_home = env.config_home();
    let autostart_entry = autostart::dir_in(&config_home).join(autostart::ENTRY_NAME);
    if !opts.system {
        for d in &system_listed {
            add(d, Via::SystemManifest);
        }
        for (entry, via) in [
            (paths::desktop_entry_in(&data), Via::MenuEntry),
            (autostart_entry.clone(), Via::Autostart),
        ] {
            let Some(text) = fs.read(&entry) else {
                continue;
            };
            // Only an absolute Exec says where to look. A bare name is judged
            // by what PATH finds (entry_decision), but PATH is never a place
            // to look: it reached wrappers, other users' copies and cargo's.
            if let ExecTarget::Path(prog) = exec_target(&fs, &text, &env.path) {
                if let Some(dir) = prog.parent() {
                    add(dir, via);
                }
            }
        }
        if let Some(dir) = env.current_exe.as_deref().and_then(Path::parent) {
            add(dir, Via::ThisProgram);
        }
        // An install with no manifest and no menu entry — every `--lean`
        // install from v0.1.0 to v0.3.0, or one whose menu entry is gone — is
        // otherwise found only if this program happens to run from it.
        // ~/.local/bin is where install.sh and build.sh --install put it when
        // told nothing else. Inferred like everything above: vetted, then
        // identified by --version, never trusted.
        if let Some(home) = &home {
            let default = home.join(".local/bin");
            if holds_ours(&default) {
                add(&default, Via::DefaultDir);
            }
        }
    }
    for d in &opts.bindirs {
        add(d, Via::Bindir);
    }

    // -------------------------------------------------------- classification
    let mut removing: Vec<PathBuf> = Vec::new();
    // Directories whose manifest lines can go: emptied by this run, or already
    // empty of this program.
    let mut cleared: Vec<PathBuf> = Vec::new();
    let mut system_dirs: Vec<PathBuf> = Vec::new();
    for c in found {
        let mut loc = Location {
            dir: c.dir.clone(),
            via: c.via.clone(),
            verdict: Verdict::NothingThere,
            binaries: Vec::new(),
        };
        let listed = c.via.contains(&Via::Manifest);
        // As root, nothing in or above a directory is opened — no
        // CACHEDIR.TAG, no Cargo record — unless no one but root can change
        // what is there. Only root_may_run's own checks are made first.
        let may_read = env.euid != 0 || fs.root_may_run(&c.dir).is_ok();
        // A build tree or an unpacked archive is where a copy runs from, not
        // an install, however it was reached: `cargo run -p tobii-gtk` and
        // then switching start-at-login on puts target/debug into the
        // autostart Exec. Only this mode's own manifest says otherwise —
        // install-payload.sh wrote that line, so an install was made there.
        if !listed {
            if let Some(why) = not_an_install(&fs, &c.dir, may_read) {
                loc.verdict = Verdict::NotAnInstall(why);
                p.locations.push(loc);
                continue;
            }
        }
        let present: Vec<&'static str> = BINARIES
            .iter()
            .copied()
            .filter(|b| fs.is_file_or_link(&c.dir.join(b)))
            .collect();
        if present.is_empty() {
            // Neither name answered lstat. That means "gone" only when the
            // error says so. A directory on the way that cannot be searched
            // (no permission, a loop) says nothing about what is there, and
            // dropping its manifest line on that guess would lose the only
            // record of a --lean install.
            let unknown =
                BINARIES
                    .iter()
                    .find_map(|b| match fs.at(&c.dir.join(b)).symlink_metadata() {
                        Err(e) if !is_gone(&e) => Some(e),
                        _ => None,
                    });
            if let Some(e) = unknown {
                p.keep(
                    c.dir.clone(),
                    format!(
                        "it cannot be looked at ({e}), so whether this program is there cannot \
                         be told — left as it is"
                    ),
                );
                loc.verdict = Verdict::Unidentified;
            } else {
                cleared.push(c.dir.clone());
            }
            p.locations.push(loc);
            continue;
        }
        // A system install is root's to remove — but only where root will:
        // a listed directory root_may_run refuses (a user's own ~/bin, say)
        // is refused by `sudo tobii uninstall --system` too, which sends its
        // user here. So it is looked at like anywhere else.
        let root_would = || {
            present
                .iter()
                .all(|b| fs.root_may_run(&c.dir.join(b)).is_ok())
        };
        if !opts.system && system_listed.contains(&c.dir) && root_would() {
            loc.verdict = Verdict::SystemInstall;
            system_dirs.push(c.dir.clone());
            p.locations.push(loc);
            continue;
        }
        // `cargo install`'s: left to `cargo uninstall`, like a package's copy
        // is left to its package manager, and for the same reason — removed
        // behind its back, Cargo's record of what it installed is wrong.
        // Only what that record lists, and none of it is run; the rest is
        // looked at below. A manifest line overrides it, as for a build tree:
        // install-payload.sh wrote that line, so it installed there.
        let cargo = if listed || !may_read {
            Vec::new()
        } else {
            cargo_listed(&fs, &c.dir, &present)
        };
        if !cargo.is_empty() {
            p.hints.push(cargo_hint(&c.dir, &cargo));
            if cargo.len() == present.len() {
                loc.verdict = Verdict::Package {
                    manager: "cargo".into(),
                    package: cargo_package_list(&cargo),
                };
                p.locations.push(loc);
                continue;
            }
        }
        // Asked before writability, as the updater does, because the two need
        // opposite advice: "you need permission" invites sudo, which is exactly
        // wrong for a package-managed file.
        match (probes.owner)(&fs.at(&c.dir)) {
            Ownership::Package { manager, package } => {
                p.hints.push(package_hint(&c.dir, &manager, &package));
                loc.verdict = Verdict::Package { manager, package };
                p.locations.push(loc);
                continue;
            }
            Ownership::Unknown { manager, why } => {
                p.refusal = Some(format!(
                    "{manager} could not be asked whether it owns the copy in {} ({why}), so \
                     nothing was changed. A package-managed file has to be removed by its \
                     package manager, and this cannot rule that out. Fix {manager} and run \
                     this again.",
                    c.dir.display()
                ));
                p.locations.push(loc);
                return p;
            }
            Ownership::None => {}
        }
        // Trusted without asking only when this mode's own manifest lists the
        // directory AND the name there is a program. Anything inferred is
        // vetted first — it is neither run nor planned unless it passes — and
        // must then say what it is.
        let mut not_run: Option<String> = None;
        let mut judged: Vec<(Binary, Option<String>)> = Vec::new();
        for b in present {
            let path = c.dir.join(b);
            if let Some((_, package)) = cargo.iter().find(|(x, _)| *x == b) {
                let why = format!(
                    "`cargo install` put it here from {package}, and Cargo's record lists it — \
                     left to Cargo (the command is below)"
                );
                let bin = Binary {
                    name: b,
                    ident: Ident::Refused("cargo install's".into()),
                    planned: false,
                };
                judged.push((bin, Some(why)));
                continue;
            }
            let is_me = me
                .as_ref()
                .is_some_and(|m| fs.canon(&path).as_ref() == Some(m));
            let me_answer = || Ident::Me(format!("tobii {}", env!("CARGO_PKG_VERSION")));
            let (ident, why_kept) = if listed {
                match listed_check(&fs, &path, env.euid) {
                    Ok(()) if is_me => (me_answer(), None),
                    Ok(()) => (Ident::Listed, None),
                    // Root's refusal, said as for a directory found by
                    // inference. That user's own run can remove it: it does
                    // not call a directory root refuses a system install.
                    Err((Ident::NotRun, why)) => {
                        let why = format!(
                            "{why}. If it is an install of this program, remove it as that \
                             user: tobii uninstall --bindir {}",
                            q(&c.dir)
                        );
                        not_run.get_or_insert_with(|| why.clone());
                        (Ident::NotRun, Some(why))
                    }
                    Err((ident, why)) => (ident, Some(why)),
                }
            } else {
                match fs.vet(&path, env.euid, probes.private_group) {
                    Err(Unvetted::Refused { short, why }) => (
                        Ident::Refused(short),
                        Some(format!("found by inference, and {why}")),
                    ),
                    Err(Unvetted::NotAsRoot(why)) => {
                        let why = format!(
                            "not run as root to ask what it is: {why}, so whoever that is \
                             chooses what it would run. If it is an install of this program, \
                             remove it as that user: tobii uninstall --bindir {}",
                            q(&c.dir)
                        );
                        not_run.get_or_insert_with(|| why.clone());
                        (Ident::NotRun, Some(why))
                    }
                    Ok(_) if is_me => (me_answer(), None),
                    // Run by the file that was vetted, never by its name.
                    Ok(vetted) => {
                        let answer = (probes.identify)(&vetted);
                        let answers = answer
                            .as_deref()
                            .is_some_and(|a| a.starts_with(&format!("{b} ")));
                        let why = (!answers).then(|| {
                            format!(
                                "found by inference, and it did not answer --version with \
                                 \"{b} …\" ({}) — it may not be this program's",
                                match &answer {
                                    Some(a) => format!("it said {:?}", sanitize(a)),
                                    None => "it gave no answer".into(),
                                }
                            )
                        });
                        (Ident::Answered(answer), why)
                    }
                }
            };
            let bin = Binary {
                name: b,
                ident,
                planned: false,
            };
            judged.push((bin, why_kept));
        }
        // Nothing is removed from a directory this user cannot write. It is a
        // system-wide install only if something there answered as this
        // program when this user ran it (after the checks above); otherwise
        // it is merely something called tobii, and advice to run this as
        // root there would be wrong.
        let writable = (probes.writable)(&fs.at(&c.dir));
        let identified = judged.iter().any(|(_, why)| why.is_none());
        for (mut bin, why) in judged {
            let path = c.dir.join(bin.name);
            match why {
                Some(why) => p.keep(path, why),
                None if writable => {
                    bin.planned = true;
                    removing.push(path);
                }
                None => {}
            }
            loc.binaries.push(bin);
        }
        if !writable && identified {
            // The user's own directory, only not writable: the advice is
            // chmod. Under sudo, root_may_run would refuse a directory that
            // is not root's and send them back to this same hint.
            if fs.uid(&fs.at(&c.dir), false).is_ok_and(|u| u == env.euid) {
                loc.verdict = Verdict::NotWritable;
                p.hints.push(chmod_hint(&c.dir));
            } else {
                loc.verdict = Verdict::SystemInstall;
                system_dirs.push(c.dir.clone());
            }
            p.locations.push(loc);
            continue;
        }
        if !loc.binaries.iter().any(|b| b.planned) {
            loc.verdict = match not_run {
                Some(why) => Verdict::NotRunAsRoot(why),
                None => Verdict::Unidentified,
            };
            p.locations.push(loc);
            continue;
        }
        loc.verdict = Verdict::Remove;
        if loc.binaries.iter().all(|b| b.planned) {
            cleared.push(c.dir.clone());
        }
        for name in fs.list(&c.dir).unwrap_or_default() {
            if is_install_scratch(&name) {
                let path = c.dir.join(&name);
                let tree = fs.is_real_dir(&path);
                p.push_remove(path, "a temporary left by the updater or installer", tree);
            }
        }
        p.locations.push(loc);
    }

    // The running binary goes last; every other binary first. Compared
    // without resolving the planned name's last component: a symlink to the
    // running program is only a link, removing it leaves the program where it
    // is, and so it goes with the others.
    let mut binaries: Vec<Removal> = Vec::new();
    for b in &removing {
        if me.as_ref() == Some(b) {
            p.self_exe = Some(b.clone());
        } else {
            binaries.push(Removal {
                path: b.clone(),
                what: "binary".into(),
                tree: false,
            });
        }
    }
    p.remove.splice(0..0, binaries);
    for d in &system_dirs {
        p.hints
            .push(system_hint(d, &exe_display, p.self_exe.is_some()));
    }

    // A planned name is `canonical directory + file name`, and a removal
    // unlinks exactly that name. So a path is removed when a name on its way
    // to the file it runs is planned — never merely because it resolves to
    // the same file as a planned symlink, whose target stays.
    let is_removed = |x: &Path| fs.link_chain(x).iter().any(|n| removing.contains(n));

    // ------------------------------------------------------------ data files
    let entry = paths::desktop_entry_in(&data);
    let d = entry_decision(&fs, &entry, &is_removed, &env.path, "the menu entry");
    let entry_kept = p.settle_entry(entry, "the menu entry", d);
    let icon = paths::icon_in(&data);
    if fs.exists(&icon) {
        if entry_kept {
            p.keep(icon, "the menu entry that stays shows it");
        } else {
            p.push_remove(icon, "the menu entry's icon", false);
            let hicolor = paths::hicolor_in(&data);
            // Refreshed only if it already exists. Never created: a cache in
            // a user's hicolor directory hides icons other programs add there
            // later, until something refreshes it again.
            if fs.is_file_or_link(&hicolor.join("icon-theme.cache")) {
                p.icon_caches.push(hicolor);
            }
        }
    }
    if !opts.system {
        let d = entry_decision(
            &fs,
            &autostart_entry,
            &is_removed,
            &env.path,
            "start-at-login",
        );
        p.settle_entry(autostart_entry, "the start-at-login entry", d);
        let adir = autostart::dir_in(&config_home);
        for name in fs.list(&adir).unwrap_or_default() {
            if autostart::is_scratch_name(&name) {
                p.push_remove(
                    adir.join(name),
                    "a temporary left by the hub's start-at-login switch",
                    false,
                );
            }
        }
    }

    // --------------------------------------------------------------- manifest
    let drop: Vec<PathBuf> = manifest_dirs
        .iter()
        .filter(|d| cleared.contains(&fs.canon_or(d)))
        .cloned()
        .collect();
    if !drop.is_empty() {
        let text = fs.read(&manifest).unwrap_or_default();
        let dropped = |d: &Path| drop.iter().any(|x| x == d);
        let then_empty = !text.lines().any(|l| manifest_line_survives(l, &dropped));
        if then_empty {
            if let Some(dir) = manifest.parent() {
                // The installer writes the manifest to installs.new-<pid> and
                // renames it into place; one stopped in between leaves that
                // behind, and it would keep this directory from going.
                for name in fs.list(dir).unwrap_or_default() {
                    if is_manifest_scratch(&name) {
                        p.push_remove(
                            dir.join(name),
                            "a temporary left by the installer's manifest write",
                            false,
                        );
                    }
                }
                p.rmdirs.push(dir.to_path_buf());
            }
        }
        p.manifests.push(ManifestEdit {
            file: manifest.clone(),
            drop,
            then_empty,
        });
    }

    // ------------------------------------------------------------------ purge
    if opts.purge {
        let cfg = config_home.join(paths::APP_DIR);
        let models = cfg.join(paths::MODELS_DIR);
        let state = env.state_home().join(paths::APP_DIR);
        let cfg_known: Vec<String> = paths::CONFIG_FILES
            .iter()
            .flat_map(|n| [n.to_string(), format!("{n}{}", paths::ATOMIC_TMP_SUFFIX)])
            .collect();
        let model_known = tobii_headpose::model_store::file_names();
        let state_known: Vec<String> = paths::STATE_FILES.iter().map(|s| s.to_string()).collect();
        // The models directory first, so the config directory can be empty by
        // the time its own remove_dir comes round.
        purge_dir(&mut p, &fs, &models, &model_known, None);
        purge_dir(&mut p, &fs, &cfg, &cfg_known, Some(paths::MODELS_DIR));
        purge_dir(&mut p, &fs, &state, &state_known, None);
        if p.remove
            .iter()
            .any(|r| r.path == cfg.join(paths::CALIBRATION_BIN))
        {
            p.warnings.push(CALIBRATION_WARNING.into());
        }
        if let Some(log) = &env.log_file {
            p.keep(
                log.clone(),
                "TOBII_LOG_FILE sends the log here — a path you chose, so it is not deleted",
            );
        }
    }
    if let Some(rt) = &env.xdg_runtime_dir {
        let sock = rt.join(paths::APP_DIR);
        if !opts.system && fs.exists(&sock) {
            p.keep(
                sock,
                "the hub's socket directory, on a tmpfs that is cleared when you log out",
            );
        }
    }

    // ------------------------------------------------------------------- udev
    if opts.udev {
        for r in UDEV_RULES {
            if fs.exists(Path::new(r)) {
                p.udev.push(PathBuf::from(r));
            }
        }
        if p.udev.is_empty() {
            p.udev_notes
                .push("there is no udev rule of this program's in /etc/udev/rules.d".into());
        }
        // `cargo install` ships no rule; only a real package does.
        let package = p
            .locations
            .iter()
            .any(|l| matches!(&l.verdict, Verdict::Package { manager, .. } if manager != "cargo"));
        if package || fs.exists(Path::new(PACKAGE_UDEV_RULE)) {
            p.udev_notes.push(format!(
                "a package install is still here, and it ships its own rule at \
                 {PACKAGE_UDEV_RULE}, which the /etc copy was overriding. Removing the /etc \
                 copy puts the package's rule in charge; that one is not touched."
            ));
        } else if !p.udev.is_empty() {
            p.udev_notes.push(
                "without a udev rule the tracker can only be opened as root, by any copy of \
                 this program that remains"
                    .into(),
            );
        }
    }

    // ------------------------------------------------------ outside this run
    if let Some(h) = root_hint(&fs) {
        p.hints.push(h);
    }
    if !opts.system {
        if let Some(home) = &home {
            p.wine_prefixes = wine_prefixes(&fs, home, env.wineprefix.as_deref());
        }
    }

    // -------------------------------------------------------------- processes
    //
    // Last, because it needs the final list of binaries. Matched by the path
    // the kernel reports, including `<path> (deleted)` — which is what a copy
    // started before the updater replaced its binary looks like. The kernel's
    // path has every symlink resolved, so a process running the target of a
    // planned symlink does not match the link's name: that target stays.
    let targets: Vec<String> = removing.iter().map(|b| b.display().to_string()).collect();
    for pr in procs {
        if pr.pid == env.pid || pr.pid == env.ppid {
            continue;
        }
        if !opts.system && pr.uid != env.euid {
            continue;
        }
        let (exe, deleted) = match pr.exe.strip_suffix(" (deleted)") {
            Some(e) => (e, true),
            None => (pr.exe.as_str(), false),
        };
        if targets.iter().any(|t| t == exe) {
            p.stop.push(pr.clone());
        } else if Path::new(exe).file_name().is_some_and(|n| n == "tobii-gtk") {
            // Its program deleted and nothing at that path any more: a
            // package removed while its hub ran. Under --system that would be
            // another user's hub, which is not this run's to stop.
            let gone = matches!(fs.presence(Path::new(exe)), Presence::Gone);
            if deleted && gone {
                if !opts.system {
                    p.orphan_hubs.push(pr.clone());
                }
            } else {
                p.other_hubs.push(pr.clone());
            }
        }
    }
    p
}

/// Whether a name in a directory this mode's manifest lists may be removed
/// without being run: the file it resolves to is a program, and that file
/// and the name (`lstat`) are this user's or root's. `Err` is how it is
/// shown, and why it stays.
///
/// As root, [`Fs::root_may_run`] is asked first, as for anything found by
/// inference, and nothing is opened unless it passes. Being listed says an
/// install was made there, not who can change it since: `sudo ./install.sh
/// --system ~/bin` writes a line for a directory its user owns, and that
/// user can swap it for a link to `/usr/bin` while root waits at the
/// question — root's unlink of `~/bin/tobii` would then remove a package's
/// copy. A directory only root can change cannot be swapped.
fn listed_check(fs: &Fs, path: &Path, euid: u32) -> Result<(), (Ident, String)> {
    let listed_but = |why: String| {
        format!(
            "its directory is listed in the install manifest, but {why} — the manifest says \
             where to look, and this is not a program that was installed there"
        )
    };
    let real = if euid == 0 {
        fs.root_may_run(path).map_err(|why| {
            let why = format!(
                "its directory is listed in the install manifest, but nothing there is opened \
                 or removed as root: {why}, so whoever that is chooses what is there"
            );
            (Ident::NotRun, why)
        })?
    } else {
        fs.at(path).canonicalize().map_err(|e| {
            let why = format!("it cannot be resolved ({e})");
            (Ident::ListedNotProgram, listed_but(why))
        })?
    };
    fs.program_check(&real)
        .map_err(|why| (Ident::ListedNotProgram, listed_but(why)))?;
    let named = fs.at(&fs.unlinked_name(path));
    for (p, link) in [(&named, true), (&real, false)] {
        fs.owned_by_us(p, link, euid).map_err(|(short, why)| {
            let why = format!("its directory is listed in the install manifest, but {why}");
            (Ident::Refused(short), why)
        })?;
    }
    Ok(())
}

/// Plan the removal of known names from `dir`, report everything else, and
/// plan a `remove_dir` of it.
fn purge_dir(p: &mut Plan, fs: &Fs, dir: &Path, known: &[String], subdir: Option<&str>) {
    let Some(names) = fs.list(dir) else {
        return;
    };
    for name in names {
        let path = dir.join(&name);
        if subdir == Some(name.as_str()) && fs.is_real_dir(&path) {
            continue; // purged on its own
        }
        if known.contains(&name) && fs.is_file_or_link(&path) {
            p.push_remove(path, "--purge", false);
        } else {
            p.keep(path, "not written by this program, so --purge leaves it");
        }
    }
    p.rmdirs.push(dir.to_path_buf());
}

// ----------------------------------------------------------------- execute

/// What executing a plan did.
#[derive(Debug, Default)]
pub struct Outcome {
    pub removed: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
    /// Directories `remove_dir` found not empty, with what is still in them.
    pub not_empty: Vec<(PathBuf, Vec<String>)>,
    /// The running `tobii`, kept because something else could not be removed.
    pub self_kept: Option<PathBuf>,
    pub notes: Vec<String>,
}

/// Perform a plan's filesystem changes. Stopping processes and the udev step
/// are not part of this; see [`run`].
pub fn execute(plan: &Plan) -> Outcome {
    let fs = Fs::new(&plan.root);
    let mut out = Outcome::default();
    let remove = |r: &Removal, out: &mut Outcome| {
        let real = fs.at(&r.path);
        let result = match real.symlink_metadata() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => Err(e),
            Ok(m) if m.is_dir() && r.tree => std::fs::remove_dir_all(&real),
            Ok(m) if m.is_dir() => Err(std::io::Error::other("it is a directory")),
            Ok(_) => std::fs::remove_file(&real),
        };
        match result {
            Ok(()) => out.removed.push(r.path.clone()),
            Err(e) => out.failed.push((r.path.clone(), e.to_string())),
        }
    };
    for r in &plan.remove {
        remove(r, &mut out);
    }
    // The running tobii goes last, and only if nothing has failed — so a
    // failure leaves a tobii to run this again with, and its manifest line
    // with it. Whether it is going is decided here, before the manifest is
    // edited, so that its line can go if it is.
    let self_going = plan.self_exe.as_deref().filter(|_| out.failed.is_empty());
    for m in &plan.manifests {
        apply_manifest(&fs, m, self_going, &mut out);
    }
    for d in &plan.rmdirs {
        match std::fs::remove_dir(fs.at(d)) {
            Ok(()) => out.removed.push(d.clone()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                let left = fs.list(d).unwrap_or_default();
                out.not_empty.push((d.clone(), left));
            }
        }
    }
    for h in &plan.icon_caches {
        refresh_icon_cache(&fs.at(h), &mut out);
    }
    if let Some(me) = &plan.self_exe {
        if out.failed.is_empty() {
            let r = Removal {
                path: me.clone(),
                what: "this program".into(),
                tree: false,
            };
            remove(&r, &mut out);
            if out.failed.iter().any(|(p, _)| p == me) {
                // Its manifest line went already, on the understanding that
                // it would. It is still found: it is the program you run.
                out.notes.push(format!(
                    "{} could not remove itself; run `{} uninstall` again once the reason above \
                     is fixed",
                    me.display(),
                    q(me)
                ));
            }
        } else {
            out.self_kept = Some(me.clone());
        }
    }
    out
}

/// Drop the manifest lines for directories this run emptied.
///
/// Decided again here, against what is really left: a line only goes when
/// neither binary remains in that directory, so a removal that failed keeps the
/// directory discoverable for the next attempt. `self_going` is the running
/// program when it is about to be removed; it does not count as remaining.
fn apply_manifest(fs: &Fs, m: &ManifestEdit, self_going: Option<&Path>, out: &mut Outcome) {
    let Some(text) = fs.read(&m.file) else {
        return;
    };
    // lstat, so a dangling link still counts as there; and a name that cannot
    // be looked at counts as there too, since it may be.
    let there = |f: &Path| !matches!(fs.at(f).symlink_metadata(), Err(e) if is_gone(&e));
    let remains = |d: &Path| {
        BINARIES.iter().any(|b| {
            let f = d.join(b);
            there(&f) && self_going != Some(fs.unlinked_name(&f).as_path())
        })
    };
    let emptied = |d: &Path| m.drop.iter().any(|x| x == d) && !remains(d);
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| match l.trim().strip_prefix("bindir=") {
            Some(v) => !emptied(Path::new(v.trim())),
            None => true,
        })
        .collect();
    let meaningful = kept
        .iter()
        .any(|l| manifest_line_survives(l, &|_: &Path| false));
    let real = fs.at(&m.file);
    if meaningful {
        let mut body = kept.join("\n");
        body.push('\n');
        match tobii_config::write_atomic(&real, body.as_bytes()) {
            Ok(()) => out.notes.push(format!("updated {}", m.file.display())),
            Err(e) => out.failed.push((m.file.clone(), e.to_string())),
        }
    } else {
        match std::fs::remove_file(&real) {
            Ok(()) => out.removed.push(m.file.clone()),
            Err(e) => out.failed.push((m.file.clone(), e.to_string())),
        }
    }
}

/// Refresh an icon cache that exists. Never creates one — but the tool this
/// runs deletes the cache when no icons are left under it; see below.
fn refresh_icon_cache(hicolor: &Path, out: &mut Outcome) {
    if !hicolor.join("icon-theme.cache").is_file() {
        return;
    }
    for tool in ["gtk4-update-icon-cache", "gtk-update-icon-cache"] {
        let mut c = Command::new(tool);
        c.arg("-qtf").arg(hicolor);
        match run_bounded(c, Duration::from_secs(20)) {
            Err(e) if e.starts_with("not found") => continue,
            Err(e) => {
                out.notes
                    .push(format!("could not refresh {}: {e}", hicolor.display()));
                return;
            }
            // Both tools DELETE an existing cache when the theme directory has
            // no icons left in it — measured with gtk4-update-icon-cache 4.x
            // and gtk-update-icon-cache 3.x, on a cache install-payload.sh made
            // while this program's icon was the only one. That puts the
            // directory back as it was before the install, which is the right
            // outcome: an empty cache would hide icons other programs add
            // later. It is still the tool's doing, not this program's, and the
            // summary says so rather than claiming a refresh.
            Ok(o) if o.status.success() => {
                out.notes
                    .push(if hicolor.join("icon-theme.cache").exists() {
                        format!("refreshed the icon cache in {}", hicolor.display())
                    } else {
                        format!(
                            "{tool} removed the icon cache in {}: no icons are left under it, \
                         and an empty theme needs none",
                            hicolor.display()
                        )
                    });
                return;
            }
            Ok(o) => {
                out.notes.push(format!(
                    "{tool} failed on {} ({})",
                    hicolor.display(),
                    o.status
                ));
                return;
            }
        }
    }
    out.notes.push(format!(
        "no icon-cache tool is installed, so the cache in {} still lists the removed icon until \
         something refreshes it",
        hicolor.display()
    ));
}

/// Run a command with a deadline, killing it if it outstays it.
fn run_bounded(mut c: Command, limit: Duration) -> Result<std::process::Output, String> {
    let mut child = c
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "not found".to_string()
            } else {
                e.to_string()
            }
        })?;
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Err(e) => return Err(e.to_string()),
            Ok(Some(status)) => {
                use std::io::Read;
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(o) = child.stdout.take() {
                    let _ = o.take(64 * 1024).read_to_end(&mut stdout);
                }
                if let Some(e) = child.stderr.take() {
                    let _ = e.take(64 * 1024).read_to_end(&mut stderr);
                }
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("it did not finish within {limit:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

// ------------------------------------------------------------ real probes

/// Every process on the machine this user can see the executable of.
pub fn scan_processes() -> Vec<Proc> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in dir.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let base = e.path();
        // Another user's process answers EACCES here, which is the filter.
        let Ok(exe) = std::fs::read_link(base.join("exe")) else {
            continue;
        };
        let uid = std::fs::read_to_string(base.join("status"))
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("Uid:"))
                    .and_then(|v| v.split_whitespace().next())
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(u32::MAX);
        let args = std::fs::read(base.join("cmdline"))
            .map(|b| {
                b.split(|c| *c == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect()
            })
            .unwrap_or_default();
        out.push(Proc {
            pid,
            uid,
            exe: exe.to_string_lossy().into_owned(),
            args,
            start_time: start_time(pid),
        });
    }
    out
}

/// When a process started, from `/proc/<pid>/stat`.
fn start_time(pid: u32) -> Option<u64> {
    parse_start_time(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// Field 22 of a `/proc/<pid>/stat` line. Field 2 is the command name in
/// parentheses, which may itself hold spaces and parentheses, so the fields
/// are counted from the LAST `)`.
fn parse_start_time(stat: &str) -> Option<u64> {
    let (_, rest) = stat.rsplit_once(')')?;
    // `rest` begins at field 3, the state, so field 22 is its 20th word.
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// Whether a process from the plan is still the process it was.
///
/// Compared by executable and start time as well as pid, so a pid the kernel
/// has since given to something else does not read as "still running".
fn still_running(p: &Proc) -> bool {
    let strip = |s: &str| s.strip_suffix(" (deleted)").unwrap_or(s).to_string();
    std::fs::read_link(format!("/proc/{}/exe", p.pid))
        .is_ok_and(|e| strip(&e.to_string_lossy()) == strip(&p.exe))
        && (p.start_time.is_none() || start_time(p.pid) == p.start_time)
}

/// Whether a signal may go to `p`'s pid: it is provably still that process.
/// Without a start time from the scan it cannot be proved, so no.
fn safe_to_signal(p: &Proc) -> bool {
    p.start_time.is_some() && still_running(p)
}

/// The first line a binary prints for `--version`.
///
/// This identifies a binary; it does not verify one. Anything can print
/// "tobii 0.3.0". What it rules out is deleting an unrelated program that
/// happens to be called `tobii` in a directory found by inference.
fn identify_binary(path: &Path) -> Option<String> {
    let mut c = Command::new(path);
    c.arg("--version")
        // A GUI binary must not reach for the session's display just to say
        // what version it is; tobii-gtk answers before GTK starts anyway.
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");
    let out = run_bounded(c, Duration::from_secs(10)).ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|l| l.trim().to_string())
}

/// Whether this user can create and remove entries in `dir`.
///
/// Read from the mode bits rather than by creating a probe file, so a dry run
/// writes nothing. It does not see a read-only mount; a removal there fails and
/// is reported as failed.
fn dir_writable(dir: &Path, euid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = std::fs::metadata(dir) else {
        return false;
    };
    if euid == 0 {
        return true;
    }
    let groups: Vec<u32> = std::fs::read_to_string("/proc/self/status")
        .ok()
        .map(|s| {
            let mut g: Vec<u32> = s
                .lines()
                .find_map(|l| l.strip_prefix("Groups:"))
                .map(|v| {
                    v.split_whitespace()
                        .filter_map(|x| x.parse().ok())
                        .collect()
                })
                .unwrap_or_default();
            if let Some(egid) = s
                .lines()
                .find_map(|l| l.strip_prefix("Gid:"))
                .and_then(|v| v.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
            {
                g.push(egid);
            }
            g
        })
        .unwrap_or_default();
    let need = if m.uid() == euid {
        0o300
    } else if groups.contains(&m.gid()) {
        0o030
    } else {
        0o003
    };
    m.mode() & need == need
}

/// Whether `gid` is the private group of the user `euid`: that user's primary
/// group, with no other account in it.
///
/// Distributions that give each user a group of their own often set umask
/// 002 (Fedora's /etc/bashrc does), so `mkdir -p ~/.local/bin` makes it
/// group-writable — by that group, which holds only the user. Read from
/// /etc/passwd and /etc/group only: a group they do not describe (LDAP, sssd)
/// cannot be seen to be private, so it is not taken to be.
fn is_private_group(gid: u32, euid: u32) -> bool {
    match (
        std::fs::read_to_string("/etc/passwd"),
        std::fs::read_to_string("/etc/group"),
    ) {
        (Ok(passwd), Ok(group)) => private_group_in(&passwd, &group, gid, euid),
        _ => false,
    }
}

fn private_group_in(passwd: &str, group: &str, gid: u32, euid: u32) -> bool {
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

// -------------------------------------------------------------- the command

/// Terminal-safe: a version string or a process's arguments are not ours to
/// let move the cursor.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// A word of a command printed for a person to paste, as a POSIX shell
/// reads it back: as it is when it holds only `[A-Za-z0-9_./-]`, otherwise
/// in single quotes, each `'` in it written `'\''`.
fn sh_quote(s: &str) -> String {
    let plain = |b: u8| b.is_ascii_alphanumeric() || b"_./-".contains(&b);
    if !s.is_empty() && s.bytes().all(plain) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A path, as [`sh_quote`] prints it.
fn q(p: &Path) -> String {
    sh_quote(&p.to_string_lossy())
}

fn describe_proc(p: &Proc) -> String {
    let args = sanitize(&p.args.join(" "));
    let args = if args.chars().count() > 140 {
        format!("{}…", args.chars().take(140).collect::<String>())
    } else {
        args
    };
    let mut s = format!("pid {:<7} {}\n        {args}", p.pid, sanitize(&p.exe));
    if p.args.get(1).map(String::as_str) == Some("game") {
        s.push_str("\n        (this `tobii game` is a running game's head tracking)");
    }
    s
}

fn is_hub(p: &Proc) -> bool {
    p.exe.contains("/tobii-gtk")
}

/// The plan, as the person about to approve it reads it.
///
/// A refused plan shows what was looked at and stops; the refusal itself is
/// the caller's to print, once.
pub fn render(plan: &Plan, opts: &Options) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let _ = writeln!(
        o,
        "tobii uninstall{}{}\n",
        if opts.system { " --system" } else { "" },
        if opts.dry_run {
            " — dry run: nothing will be changed"
        } else {
            ""
        }
    );

    if !plan.locations.is_empty() {
        let _ = writeln!(o, "Install locations");
        for l in &plan.locations {
            let via: Vec<&str> = l.via.iter().map(|v| v.label()).collect();
            let _ = writeln!(o, "  {}   (found via {})", l.dir.display(), via.join(", "));
            for b in &l.binaries {
                let what = match &b.ident {
                    Ident::Me(a) => format!("answers {:?} (this program)", sanitize(a)),
                    Ident::Listed => "listed in the install manifest; not identified".into(),
                    Ident::ListedNotProgram => {
                        "listed in the install manifest, but not an executable program".into()
                    }
                    Ident::Answered(Some(a)) => format!("answers {:?}", sanitize(a)),
                    Ident::Answered(None) => "no answer to --version".into(),
                    Ident::NotRun => "not run as root to ask what it is".into(),
                    Ident::Refused(short) => format!("not run: {short}"),
                };
                let _ = writeln!(o, "      {:<10} {what}", b.name);
            }
            let verdict = match &l.verdict {
                Verdict::Remove => {
                    "removed. Binaries found by inference are identified by what they print \
                     for --version — that identifies them; it does not verify them."
                        .to_string()
                }
                Verdict::NothingThere => "nothing of this program's is there".into(),
                Verdict::Package { manager, package } => {
                    format!("owned by {manager} ({package}) — left to the package manager")
                }
                Verdict::SystemInstall => "a system-wide install — see below".into(),
                Verdict::NotWritable => "yours, but you cannot write to it — see below".into(),
                Verdict::Unidentified => {
                    "left: nothing there could be identified as this program's".into()
                }
                Verdict::NotAnInstall(why) => format!("not an install: {why}"),
                Verdict::NotRunAsRoot(why) => format!("left: {why}"),
            };
            let _ = writeln!(o, "      → {verdict}");
        }
        let _ = writeln!(o);
    }

    if plan.refusal.is_some() {
        return o;
    }

    let sigterm = if opts.yes {
        "SIGTERM if they have not quit 5 seconds later — --yes agrees to that"
    } else {
        "SIGTERM only if you agree"
    };
    if !plan.stop.is_empty() {
        // Only a hub can be asked, and only when no hub from a copy that
        // stays could take the question instead (see stop_running). A running
        // `tobii` is given five seconds to be quit by hand, then the question.
        let asked = if plan.stop.iter().any(is_hub) && plan.other_hubs.is_empty() {
            "the hub is asked to quit, then "
        } else {
            ""
        };
        let _ = writeln!(o, "Running copies — stopped first ({asked}{sigterm})");
        for p in &plan.stop {
            let _ = writeln!(o, "  {}", describe_proc(p));
        }
        if !plan.other_hubs.is_empty() {
            let _ = writeln!(
                o,
                "  A hub from a copy that stays is also running, so the running copies above \
                 are not asked over D-Bus (that could reach the other hub instead)."
            );
        }
        let _ = writeln!(o);
    }

    if !plan.orphan_hubs.is_empty() {
        let _ = writeln!(o, "A hub whose program has been deleted is still running");
        for p in &plan.orphan_hubs {
            let _ = writeln!(o, "  {}", describe_proc(p));
        }
        let how = if plan.other_hubs.is_empty() {
            format!(
                "it is asked to quit over D-Bus first — which reaches whichever hub owns the \
                 name, this one or a copy being removed — then {sigterm}"
            )
        } else {
            format!(
                "with {sigterm} — not over D-Bus, which could reach the hub from a copy that \
                 stays instead"
            )
        };
        let _ = writeln!(
            o,
            "  Its program is gone — most likely a package was removed while it ran — so \
             there is nothing of it to remove. But it still holds the tracker, and every start \
             of the hub is handed to it until it quits. This run offers to stop it: {how}.\n"
        );
    }

    if !plan.is_empty() {
        let _ = writeln!(o, "Will remove");
        for r in &plan.remove {
            let _ = writeln!(o, "  {}   ({})", r.path.display(), r.what);
        }
        if let Some(me) = &plan.self_exe {
            let _ = writeln!(
                o,
                "  {}   (this program — removed last, and only if nothing else failed)",
                me.display()
            );
        }
        for m in &plan.manifests {
            for d in &m.drop {
                let _ = writeln!(
                    o,
                    "  the line bindir={} in {}",
                    d.display(),
                    m.file.display()
                );
            }
            if m.then_empty {
                let _ = writeln!(o, "  {}   (nothing left in it)", m.file.display());
            }
        }
        for d in &plan.rmdirs {
            let _ = writeln!(o, "  {}/   (only if empty by then)", d.display());
        }
        for h in &plan.icon_caches {
            let _ = writeln!(
                o,
                "  (then refresh the existing icon cache in {}; the cache tool deletes it if no \
                 icons are left there)",
                h.display()
            );
        }
        let _ = writeln!(o);
    }

    if !plan.kept.is_empty() {
        let _ = writeln!(o, "Will keep");
        for k in &plan.kept {
            let _ = writeln!(o, "  {}\n      {}", k.path.display(), k.why);
        }
        let _ = writeln!(o);
    }

    if opts.udev {
        let _ = writeln!(o, "The udev rule (--udev)");
        for c in udev_commands(plan) {
            let _ = writeln!(o, "  {}", c.join(" "));
        }
        for n in &plan.udev_notes {
            let _ = writeln!(o, "  Note: {n}");
        }
        let _ = writeln!(o);
    }

    if !plan.hints.is_empty() || !plan.wine_prefixes.is_empty() || !opts.system {
        let _ = writeln!(o, "Not done by this run");
        for h in &plan.hints {
            let _ = writeln!(o, "  {h}");
        }
        if !opts.system {
            if plan.wine_prefixes.is_empty() {
                let _ = writeln!(
                    o,
                    "  No Wine prefix with the bridge was found in Steam, $WINEPREFIX or ~/.wine."
                );
            } else {
                // Removing the bridge takes a tobii. Only when this run removes
                // the one running it is there a reason to do that first; run
                // from an unpacked archive, the archive's tobii stays and does
                // it just as well afterwards.
                let (how, tobii) = match (&plan.self_exe, &plan.running) {
                    (Some(_), _) => (
                        "Removing it takes `tobii`, and this run removes `tobii` — so run \
                         these first:"
                            .to_string(),
                        "tobii".to_string(),
                    ),
                    (None, Some(running)) => (
                        format!(
                            "The tobii running this, {}, is not removed by this run, so it can \
                             remove the bridge before or after it:",
                            running.display()
                        ),
                        q(running),
                    ),
                    (None, None) => ("Remove it with:".to_string(), "tobii".to_string()),
                };
                let _ = writeln!(
                    o,
                    "  The TrackIR/FreeTrack bridge is installed in these Wine prefixes. {how}"
                );
                for w in &plan.wine_prefixes {
                    let _ = writeln!(o, "    {tobii} bridge uninstall --prefix {}", q(w));
                }
            }
            let _ = writeln!(
                o,
                "  Prefixes outside Steam (Lutris, Heroic, Bottles, a WINEPREFIX not set here) \
                 cannot be found from here; for any you installed the bridge into, run \
                 `tobii bridge uninstall --prefix PATH`."
            );
            if !opts.udev {
                let _ = writeln!(o, "  The udev rule stays unless you pass --udev.");
            }
            if !opts.purge {
                let _ = writeln!(
                    o,
                    "  Your settings, calibration and log stay unless you pass --purge."
                );
            }
        }
        let _ = writeln!(o);
    }

    for w in &plan.warnings {
        let _ = writeln!(o, "Warning: {w}\n");
    }
    o
}

/// The udev commands, the same ones `install-payload.sh` runs. Without `sudo`
/// when this is already root.
fn udev_commands(plan: &Plan) -> Vec<Vec<String>> {
    if plan.udev.is_empty() {
        return Vec::new();
    }
    let sudo: Vec<String> = if plan.euid == 0 {
        Vec::new()
    } else {
        vec!["sudo".into()]
    };
    let with = |rest: &[&str]| {
        let mut c = sudo.clone();
        c.extend(rest.iter().map(|s| s.to_string()));
        c
    };
    let mut rm = with(&["rm", "-f"]);
    rm.extend(plan.udev.iter().map(|p| p.display().to_string()));
    vec![
        rm,
        with(&["udevadm", "control", "--reload"]),
        with(&["udevadm", "trigger", "--subsystem-match=usb"]),
        with(&["udevadm", "trigger", "--subsystem-match=misc"]),
    ]
}

fn ask(question: &str, default_yes: bool) -> bool {
    print!(
        "{question} {} ",
        if default_yes { "[Y/n]" } else { "[y/N]" }
    );
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        // End of input answers nothing, and nothing is not a yes.
        Ok(0) | Err(_) => false,
        Ok(_) => {
            let a = line.trim();
            if a.is_empty() {
                default_yes
            } else {
                a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes")
            }
        }
    }
}

/// Ask the hub to quit through the `quit` action it exports on the session
/// bus. Hubs from v0.1.0–v0.3.0 have no such action, and this fails for them.
fn ask_hub_to_quit() -> Result<(), String> {
    let mut c = Command::new("gdbus");
    c.args([
        "call",
        "--session",
        "--timeout",
        "5",
        "--dest",
        paths::APP_ID,
        "--object-path",
        &paths::app_object_path(),
        "--method",
        "org.freedesktop.Application.ActivateAction",
        "quit",
        "[]",
        "{}",
    ]);
    let out = run_bounded(c, Duration::from_secs(10))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(sanitize(err.lines().next().unwrap_or("it failed")))
    }
}

/// What is said when [`ask_hub_to_quit`] fails: the likely cause, and only
/// the one that fits. `--system` runs only as root (`plan` refuses it
/// otherwise), and root has no session bus that reaches a user's hub; in a
/// user's own run, the hub may be one from before the quit action.
fn quit_refused_note(e: &str, system: bool) -> String {
    let why = if system {
        "as root there is no session bus that reaches a user's hub"
    } else {
        "a hub from v0.3.0 or before has no quit action to ask"
    };
    format!("  the hub could not be asked to quit ({e}) — {why}")
}

/// Send `signal` to each process that is provably still the one listed, and
/// skip the rest. Returns those signalled.
///
/// Checked again right before each signal, not only when the list was made:
/// the question before this waited for a person, for as long as they took,
/// and a pid freed meanwhile can belong to anything by now.
fn signal_each(procs: &[Proc], signal: &mut dyn FnMut(&Proc) -> Result<(), String>) -> Vec<Proc> {
    let mut signalled = Vec::new();
    for p in procs {
        if !safe_to_signal(p) {
            println!(
                "  pid {} is no longer the process listed above — not signalled",
                p.pid
            );
            continue;
        }
        match signal(p) {
            Ok(()) => signalled.push(p.clone()),
            Err(e) => println!("  could not signal {}: {e}", p.pid),
        }
    }
    signalled
}

fn wait_gone(procs: &[Proc], limit: Duration) -> Vec<Proc> {
    let deadline = Instant::now() + limit;
    loop {
        let left: Vec<Proc> = procs.iter().filter(|p| still_running(p)).cloned().collect();
        if left.is_empty() || Instant::now() >= deadline {
            return left;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Stop the running copies before anything is removed, and offer to stop a
/// hub whose program was deleted.
///
/// There is no other way to stop a primary hub from receiving every launch:
/// `GApplication` is single-instance, so while one runs, starting any copy
/// hands off to it. `Err` means one of our copies is still running and
/// nothing has been removed. A hub whose program was deleted is not ours to
/// insist on: declining to stop it leaves it running and goes on.
fn stop_running(plan: &Plan, opts: &Options, interactive: bool) -> Result<(), String> {
    let ours = &plan.stop;
    let orphans = &plan.orphan_hubs;
    if ours.is_empty() && orphans.is_empty() {
        return Ok(());
    }
    let stop_msg = "Nothing was removed. Quit them first — the hub's Quit is in its cogwheel \
                    menu — and run this again.";
    let orphan_left = "  left running: the hub whose program was deleted. Its Quit is in its \
                       cogwheel menu.";
    let question = match (ours.is_empty(), orphans.is_empty()) {
        (false, true) => "Ask the running copies to quit now?",
        (true, _) => "Ask the hub whose program was deleted to quit now?",
        (false, false) => {
            "Ask the running copies, and the hub whose program was deleted, to quit now?"
        }
    };
    if !opts.yes && !(interactive && ask(question, true)) {
        if ours.is_empty() {
            println!("{orphan_left}");
            return Ok(());
        }
        return Err(stop_msg.into());
    }
    let all: Vec<Proc> = ours.iter().chain(orphans).cloned().collect();
    if all.iter().any(is_hub) && plan.other_hubs.is_empty() {
        match ask_hub_to_quit() {
            Ok(()) => println!(
                "  asked the hub to quit (over D-Bus, which reaches whichever hub owns the name)"
            ),
            Err(e) => println!("{}", quit_refused_note(&e, opts.system)),
        }
    }
    let left = wait_gone(&all, Duration::from_secs(5));
    if left.is_empty() {
        return Ok(());
    }
    println!("Still running:");
    for p in &left {
        println!("  {}", describe_proc(p));
    }
    let ours_left = |set: &[Proc]| set.iter().any(|p| ours.contains(p));
    if !opts.yes && !(interactive && ask("Send them SIGTERM?", false)) {
        if ours_left(&left) {
            return Err(stop_msg.into());
        }
        println!("{orphan_left}");
        return Ok(());
    }
    let signalled = signal_each(&left, &mut |p| {
        let mut c = Command::new("kill");
        c.args(["-TERM", &p.pid.to_string()]);
        match run_bounded(c, Duration::from_secs(5)) {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => Err(format!("kill failed ({})", o.status)),
            Err(e) => Err(e),
        }
    });
    let still = wait_gone(&left, Duration::from_secs(3));
    if signalled.iter().any(is_hub) {
        println!(
            "A hub was stopped with SIGTERM. If the tracker's lights stay on, unplug it and \
             plug it back in. (Whether a hub stopped this way leaves the ET5 lit has not been \
             measured.)"
        );
    }
    let pids = |set: &[Proc]| {
        set.iter()
            .map(|p| p.pid.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if ours_left(&still) {
        return Err(format!(
            "still running after SIGTERM: {}. Nothing was removed.",
            pids(&still)
        ));
    }
    if !still.is_empty() {
        println!(
            "  still running after SIGTERM, and left: {} (its program is already gone, so \
             nothing here depends on it)",
            pids(&still)
        );
    }
    Ok(())
}

fn run_udev(plan: &Plan, interactive: bool, yes: bool) {
    let cmds = udev_commands(plan);
    if cmds.is_empty() {
        for n in &plan.udev_notes {
            println!("  {n}");
        }
        return;
    }
    if !interactive {
        println!("\nNo terminal to run sudo on, so here are the udev commands to run yourself:");
        for c in &cmds {
            println!("  {}", c.join(" "));
        }
        return;
    }
    let question = if plan.euid == 0 {
        "Remove the udev rule now?"
    } else {
        "Remove the udev rule now? This uses sudo."
    };
    if !yes && !ask(question, false) {
        println!("  skipped — the commands are listed above");
        return;
    }
    for c in &cmds {
        println!("  {}", c.join(" "));
        let status = Command::new(&c[0]).args(&c[1..]).status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                println!("  that failed ({s}); the rest is not run");
                return;
            }
            Err(e) => {
                println!("  could not run it ({e}); the rest is not run");
                return;
            }
        }
    }
}

fn summary(plan: &Plan, out: &Outcome) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let _ = writeln!(o, "\nSummary");
    let _ = writeln!(o, "  Removed ({}):", out.removed.len());
    for r in &out.removed {
        let _ = writeln!(o, "    {}", r.display());
    }
    if !out.failed.is_empty() {
        let _ = writeln!(o, "  Could not remove:");
        for (p, e) in &out.failed {
            let _ = writeln!(o, "    {} — {e}", p.display());
        }
    }
    if let Some(me) = &out.self_kept {
        let _ = writeln!(
            o,
            "  Kept {} — this program — because something above could not be removed. Its \
             line stays in the install manifest. Fix what failed, then run `{} uninstall` \
             again.",
            me.display(),
            q(me)
        );
    }
    let left = !plan.kept.is_empty() || !out.not_empty.is_empty();
    if left {
        let _ = writeln!(o, "  Left in place:");
        for k in &plan.kept {
            let _ = writeln!(o, "    {} — {}", k.path.display(), k.why);
        }
        for (d, names) in &out.not_empty {
            let _ = writeln!(
                o,
                "    {}/ — still holds {}",
                d.display(),
                if names.is_empty() {
                    "something".to_string()
                } else {
                    names.join(", ")
                }
            );
        }
    }
    // Its own heading: under "Left in place" it read as something this run
    // had meant to remove, and with nothing kept it landed under "Removed".
    let untouched: Vec<String> = plan
        .locations
        .iter()
        .filter_map(|l| match &l.verdict {
            Verdict::Package { manager, package } if manager == "cargo" => Some(format!(
                "{} — `cargo install`'s ({package}): {}",
                l.dir.display(),
                cargo_uninstall(&l.dir, package)
            )),
            Verdict::Package { manager, package } => Some(format!(
                "{} — owned by {manager} ({package}): {}",
                l.dir.display(),
                package_remove_command(manager, package)
            )),
            Verdict::SystemInstall => Some(format!(
                "{} — a system-wide install: sudo tobii uninstall --system --bindir {}",
                l.dir.display(),
                q(&l.dir)
            )),
            Verdict::NotWritable => Some(format!(
                "{} — yours, but you cannot write to it: chmod u+w {}, then run this again",
                l.dir.display(),
                q(&l.dir)
            )),
            _ => None,
        })
        .collect();
    if !untouched.is_empty() {
        let _ = writeln!(o, "  Not touched:");
        for u in &untouched {
            let _ = writeln!(o, "    {u}");
        }
    }
    let tobii_gone = out
        .removed
        .iter()
        .any(|p| p.file_name().is_some_and(|n| n == "tobii"));
    if tobii_gone && !plan.wine_prefixes.is_empty() {
        let running_stays = plan
            .running
            .as_ref()
            .filter(|r| !out.removed.contains(r) && plan.self_exe.as_ref() != Some(r));
        match running_stays {
            Some(running) => {
                let _ = writeln!(
                    o,
                    "  The TrackIR/FreeTrack bridge is still in these Wine prefixes. The tobii \
                     that ran this stays, and removes it:"
                );
                for w in &plan.wine_prefixes {
                    let _ = writeln!(o, "    {} bridge uninstall --prefix {}", q(running), q(w));
                }
            }
            None => {
                let _ = writeln!(
                    o,
                    "  The TrackIR/FreeTrack bridge is still in these Wine prefixes. `tobii` is \
                     removed now, but the `tobii` in a release archive runs these just as well \
                     — from the unpacked archive's folder:"
                );
                for w in &plan.wine_prefixes {
                    let _ = writeln!(o, "    ./tobii bridge uninstall --prefix {}", q(w));
                }
            }
        }
    }
    for n in &out.notes {
        let _ = writeln!(o, "  {n}");
    }
    o
}

/// The question asked before anything is removed when the bridge is still in
/// a Wine prefix and this run removes the `tobii` running it — the one sure
/// to be at hand to remove the bridge with. Run from an unpacked archive the
/// running `tobii` stays, so there is nothing to stop for.
fn bridge_question(plan: &Plan) -> Option<String> {
    if plan.wine_prefixes.is_empty() || plan.self_exe.is_none() {
        return None;
    }
    let n = plan.wine_prefixes.len();
    Some(format!(
        "The bridge is still in {n} Wine prefix{}, and removing it needs this program. Stop here \
         so you can remove it first?",
        if n == 1 { "" } else { "es" }
    ))
}

/// `tobii uninstall ...`
pub fn run(args: &[String]) -> CmdResult {
    let Some(opts) = parse_options(args.get(2..).unwrap_or(&[]))? else {
        println!("{USAGE}");
        return Ok(());
    };
    let env = Env::from_process()?;
    let procs = scan_processes();
    let euid = env.euid;
    let writable = |d: &Path| dir_writable(d, euid);
    let private_group = |gid: u32| is_private_group(gid, euid);
    let probes = Probes {
        owner: &tobii_update::install::ownership_of,
        identify: &identify_binary,
        writable: &writable,
        private_group: &private_group,
    };
    let plan = plan(&env, &opts, Path::new("/"), &probes, &procs);
    print!("{}", render(&plan, &opts));
    if let Some(r) = &plan.refusal {
        // Printed once, as the caller's `error:` line.
        return Err(r.clone().into());
    }
    if opts.dry_run {
        println!("Dry run: nothing was changed.");
        return Ok(());
    }
    let interactive = std::io::stdin().is_terminal();
    if plan.is_empty() {
        println!("Nothing to remove.");
        stop_running(&plan, &opts, interactive)?;
        if opts.udev {
            run_udev(&plan, interactive, opts.yes);
        }
        return Ok(());
    }
    if !opts.yes {
        if !interactive {
            return Err(
                "there is no terminal to ask on, so nothing was changed. Read the \
                        plan above, then re-run with --yes to go ahead."
                    .into(),
            );
        }
        if let Some(q) = bridge_question(&plan) {
            if ask(&q, true) {
                println!(
                    "Nothing was changed. Run the `tobii bridge uninstall` commands listed \
                     above, then run this again."
                );
                return Ok(());
            }
        }
        if !ask("Remove everything listed under “Will remove”?", false) {
            println!("Nothing was changed.");
            return Ok(());
        }
    }
    stop_running(&plan, &opts, interactive)?;
    let out = execute(&plan);
    if opts.udev {
        run_udev(&plan, interactive, opts.yes);
    }
    print!("{}", summary(&plan, &out));
    if out.failed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} item(s) could not be removed", out.failed.len()).into())
    }
}

#[cfg(test)]
mod tests;
