//! The install path, run against real archives.
//!
//! A review of this crate ended with: *"as a self-updater this is at roughly
//! the 'happy path on the author's own machine' stage … it has never been run
//! against a real release even once."* That was true — every test stopped at a
//! parsed fixture, and the half of the code that actually rewrites files on
//! somebody's machine had never executed.
//!
//! These tests build a real `tar.gz` with real executables in it, in the layout
//! `scripts/release.sh` produces, and run the whole post-download path over it:
//! digest check, unpack, symlink refusal, runnability probe, swap, rollback.
//! Only the network is absent.

use std::path::{Path, PathBuf};

use tobii_update::install::{install_verified_archive, InstallError};

/// A directory the test owns, removed on the way out.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "tobii-e2e-{tag}-{}-{:?}",
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

fn write_exe(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

/// Build a release archive the way `scripts/release.sh` does: one top-level
/// directory named for the release, with the binaries inside it.
fn build_archive(root: &Path, stem: &str, bodies: &[(&str, &str)]) -> (PathBuf, String) {
    let staging = root.join("build").join(stem);
    std::fs::create_dir_all(&staging).unwrap();
    for (name, body) in bodies {
        write_exe(&staging.join(name), body);
    }
    let archive = root.join(format!("{stem}.tar.gz"));
    let out = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(root.join("build"))
        .arg(stem)
        .output()
        .expect("tar should be installed");
    assert!(out.status.success(), "tar failed: {out:?}");
    let digest = tobii_config::sha256::hex_digest(&std::fs::read(&archive).unwrap());
    (archive, digest)
}

/// Ask a binary for its version, retrying the one failure that is not its fault.
///
/// A file that was just written cannot be exec'd while any process holds a write
/// descriptor for it — Linux answers `ETXTBSY`. `write_exe` closes its own
/// descriptor, but a `fork` on another thread duplicates every open descriptor
/// into the child, where it lives until that child execs. These tests run
/// concurrently and all of them write-then-exec, so they hand each other the
/// race: measured at 19 failures in 200 runs before this retry, all of them
/// here rather than in the library (which has its own retry, for the same
/// reason, in `spawn_probe`).
fn version_of(path: &Path) -> String {
    for attempt in 0..8 {
        match std::process::Command::new(path).arg("--version").output() {
            Ok(out) => return String::from_utf8_lossy(&out.stdout).to_string(),
            Err(e)
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                    || e.raw_os_error() == Some(26) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
            }
            Err(e) => panic!("{} could not be run: {e}", path.display()),
        }
    }
    panic!("{} stayed busy for every attempt", path.display())
}

/// A stand-in for an installed binary: it answers `--version` like the real
/// ones do, so the probe and the version read-back both work.
fn versioned(name: &str, version: &str) -> String {
    format!("#!/bin/sh\n[ \"$1\" = --version ] && echo '{name} {version}' && exit 0\nexit 3\n")
}

/// Somewhere for the binaries to be installed, with both already present —
/// `install_verified_archive` only replaces what is already there.
fn install_dir_with_both(root: &Path) -> PathBuf {
    let dir = root.join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    write_exe(&dir.join("tobii"), &versioned("tobii", "0.1.0"));
    write_exe(&dir.join("tobii-gtk"), &versioned("tobii-gtk", "0.1.0"));
    dir
}

/// Staging and backup files this crate creates are all dot-prefixed, so what is
/// left in the install directory after a run is the whole cleanup question.
fn hidden_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with('.'))
        .collect()
}

fn work_dir(root: &Path) -> PathBuf {
    let w = root.join("work");
    std::fs::create_dir_all(&w).unwrap();
    w
}

/// The whole point: a good archive replaces both binaries and reports the
/// version the *installed* binaries answer with, not the one the release
/// claimed.
#[test]
fn a_real_archive_installs_both_binaries() {
    let s = Scratch::new("happy");
    let (archive, digest) = build_archive(
        s.path(),
        "tobii-linux-0.2.0-x86_64-unknown-linux-gnu",
        &[
            ("tobii", &versioned("tobii", "0.2.0")),
            ("tobii-gtk", &versioned("tobii-gtk", "0.2.0")),
        ],
    );
    let dir = install_dir_with_both(s.path());
    let work = work_dir(s.path());

    let steps = std::cell::RefCell::new(Vec::new());
    let done = install_verified_archive(&archive, &digest, &dir, &work, &|st| {
        steps.borrow_mut().push(st.to_string())
    })
    .expect("a good archive should install");

    let mut replaced = done.replaced.clone();
    replaced.sort();
    assert_eq!(replaced, vec!["tobii".to_string(), "tobii-gtk".to_string()]);
    assert_eq!(done.version, "0.2.0", "read back from the binary");

    for name in ["tobii", "tobii-gtk"] {
        let said = version_of(&dir.join(name));
        assert!(said.contains("0.2.0"), "{name} still reports: {said}");
    }
    assert!(
        hidden_files(&dir).is_empty(),
        "leftovers: {:?}",
        hidden_files(&dir)
    );
    assert!(steps.borrow().iter().any(|s| s == "installing"));
}

/// A truncated or tampered download must be refused with nothing touched.
#[test]
fn a_wrong_digest_stops_before_anything_is_unpacked() {
    let s = Scratch::new("digest");
    let (archive, _) = build_archive(
        s.path(),
        "tobii-linux-0.2.0-x86_64-unknown-linux-gnu",
        &[("tobii", &versioned("tobii", "0.2.0"))],
    );
    let dir = install_dir_with_both(s.path());
    let work = work_dir(s.path());
    let wrong = "0".repeat(64);

    let e = install_verified_archive(&archive, &wrong, &dir, &work, &|_| {})
        .expect_err("a mismatched digest must be fatal");
    assert!(matches!(e, InstallError::Digest { .. }), "{e}");
    assert!(!work.join("unpacked").exists(), "it unpacked anyway");
    assert!(version_of(&dir.join("tobii")).contains("0.1.0"));
}

/// The failure that would otherwise brick the install: a release built against
/// libraries this machine does not have. It has to be caught while the old
/// binaries are still in place.
#[test]
fn a_binary_that_cannot_run_here_leaves_the_old_ones_alone() {
    let s = Scratch::new("willnotrun");
    // Exits non-zero however it is called — what a binary whose interpreter or
    // glibc is missing looks like from outside.
    let broken = "#!/bin/sh\necho \"version \\`GLIBC_2.44' not found\" >&2\nexit 1\n";
    let (archive, digest) = build_archive(
        s.path(),
        "tobii-linux-0.2.0-x86_64-unknown-linux-gnu",
        &[
            ("tobii", &versioned("tobii", "0.2.0")),
            ("tobii-gtk", broken),
        ],
    );
    let dir = install_dir_with_both(s.path());
    let work = work_dir(s.path());

    let e = install_verified_archive(&archive, &digest, &dir, &work, &|_| {})
        .expect_err("a binary that will not run must be refused");
    match &e {
        InstallError::WillNotRun { name, detail } => {
            assert_eq!(name, "tobii-gtk");
            assert!(
                detail.contains("GLIBC"),
                "the reason should survive: {detail}"
            );
        }
        other => panic!("expected WillNotRun, got {other}"),
    }

    // Both old binaries are untouched — including `tobii`, which passed its own
    // probe and was already staged when its sibling failed.
    for name in ["tobii", "tobii-gtk"] {
        assert!(
            version_of(&dir.join(name)).contains("0.1.0"),
            "{name} was replaced despite the failure"
        );
    }
    assert!(
        hidden_files(&dir).is_empty(),
        "staged files left behind: {:?}",
        hidden_files(&dir)
    );
}

/// `tar` will happily create a symlink, and `fs::copy` reads through one. A
/// member named `tobii` pointing outside the archive must not be installed.
#[test]
fn a_symlinked_member_is_not_installed() {
    let s = Scratch::new("symlink");
    let secret = s.path().join("secret");
    std::fs::write(&secret, b"this file was never in the archive").unwrap();

    let stem = "tobii-linux-0.2.0-x86_64-unknown-linux-gnu";
    let staging = s.path().join("build").join(stem);
    std::fs::create_dir_all(&staging).unwrap();
    std::os::unix::fs::symlink(&secret, staging.join("tobii")).unwrap();
    let archive = s.path().join(format!("{stem}.tar.gz"));
    let out = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(s.path().join("build"))
        .arg(stem)
        .output()
        .unwrap();
    assert!(out.status.success());
    let digest = tobii_config::sha256::hex_digest(&std::fs::read(&archive).unwrap());

    let dir = install_dir_with_both(s.path());
    let work = work_dir(s.path());
    let e = install_verified_archive(&archive, &digest, &dir, &work, &|_| {})
        .expect_err("a symlinked binary must not install");
    assert!(matches!(e, InstallError::NothingToInstall), "{e}");
    assert_ne!(
        std::fs::read(dir.join("tobii")).unwrap(),
        std::fs::read(&secret).unwrap(),
        "the symlink target was installed"
    );
}

/// GNU tar refuses `..` members outright. Worth pinning: the code relies on it
/// rather than parsing the archive itself, and a change in that behaviour would
/// be silent otherwise.
#[test]
fn tar_refuses_to_write_outside_the_directory_it_is_given() {
    let s = Scratch::new("traversal");
    let outside = s.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("target"), b"original").unwrap();

    // Build an archive containing `../outside/target`, which needs tar's own
    // escape hatch to create.
    let build = s.path().join("build");
    std::fs::create_dir_all(build.join("sub")).unwrap();
    std::fs::write(build.join("evil"), b"replaced").unwrap();
    let archive = s.path().join("evil.tar.gz");
    let made = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&build)
        .arg("--transform")
        .arg("s|^evil|../outside/target|")
        .arg("evil")
        .output()
        .unwrap();
    if !made.status.success() {
        // A tar without --transform: nothing to assert, and nothing at risk.
        return;
    }

    let into = s.path().join("into");
    std::fs::create_dir_all(&into).unwrap();
    let out = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&into)
        .arg("--no-same-owner")
        .output()
        .unwrap();
    assert!(!out.status.success(), "tar accepted a `..` member");
    assert_eq!(
        std::fs::read(outside.join("target")).unwrap(),
        b"original",
        "a file outside the unpack directory was written"
    );
}

/// A release archive that ships neither binary must say so rather than
/// reporting a successful update that changed nothing.
#[test]
fn an_archive_without_our_binaries_is_an_error() {
    let s = Scratch::new("empty");
    let (archive, digest) = build_archive(
        s.path(),
        "tobii-linux-0.2.0-x86_64-unknown-linux-gnu",
        &[("README.md", "not a binary")],
    );
    let dir = install_dir_with_both(s.path());
    let work = work_dir(s.path());
    let e = install_verified_archive(&archive, &digest, &dir, &work, &|_| {}).unwrap_err();
    assert!(matches!(e, InstallError::NothingToInstall), "{e}");
}

/// An archive missing one of the two installed binaries must be refused, not
/// half-applied. Installing the one it carries leaves a new `tobii` beside an
/// old `tobii-gtk` — the mismatched pair the whole rollback path exists to
/// prevent, arriving by the front door and reported as a success.
#[test]
fn an_archive_missing_an_installed_binary_changes_nothing() {
    let s = Scratch::new("incomplete");
    let (archive, digest) = build_archive(
        s.path(),
        "tobii-linux-0.2.0-x86_64-unknown-linux-gnu",
        &[("tobii", &versioned("tobii", "0.2.0"))],
    );
    // Both are installed here, but the archive carries only one.
    let dir = install_dir_with_both(s.path());
    let work = work_dir(s.path());

    let e = install_verified_archive(&archive, &digest, &dir, &work, &|_| {})
        .expect_err("a half release must be refused");
    match &e {
        InstallError::Incomplete { missing } => assert!(missing.contains("tobii-gtk"), "{missing}"),
        other => panic!("expected Incomplete, got {other}"),
    }
    for name in ["tobii", "tobii-gtk"] {
        assert!(
            version_of(&dir.join(name)).contains("0.1.0"),
            "{name} was changed by a refused install"
        );
    }
    assert!(
        hidden_files(&dir).is_empty(),
        "staged: {:?}",
        hidden_files(&dir)
    );
}

/// Only what is already installed is replaced. A machine with just the CLI must
/// not gain a GUI binary it never had.
#[test]
fn only_binaries_already_installed_are_replaced() {
    let s = Scratch::new("partial");
    let (archive, digest) = build_archive(
        s.path(),
        "tobii-linux-0.2.0-x86_64-unknown-linux-gnu",
        &[
            ("tobii", &versioned("tobii", "0.2.0")),
            ("tobii-gtk", &versioned("tobii-gtk", "0.2.0")),
        ],
    );
    let dir = s.path().join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    write_exe(&dir.join("tobii"), &versioned("tobii", "0.1.0"));
    let work = work_dir(s.path());

    let done = install_verified_archive(&archive, &digest, &dir, &work, &|_| {}).unwrap();
    assert_eq!(done.replaced, vec!["tobii".to_string()]);
    assert!(
        !dir.join("tobii-gtk").exists(),
        "a binary that was not installed must not appear"
    );
}
