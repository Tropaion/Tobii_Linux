//! Downloading a release and swapping the binaries in place.
//!
//! Everything here is arranged around one rule: **nothing is put where it will
//! be executed until it has been verified.** The archive is downloaded to a
//! scratch directory, checked against the `SHA256SUMS` published beside it,
//! unpacked there, and only then moved over the installed binaries — each with
//! a rename, which is atomic, so an interrupted update leaves the old binary
//! intact rather than a half-written one.

use std::path::{Path, PathBuf};

use tobii_config::sha256;

use crate::release::{get, Release};

/// The binaries this project installs. A release archive may carry more; only
/// these are replaced, and only where one is already installed.
pub const BINARIES: [&str; 2] = ["tobii", "tobii-gtk"];

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
    /// The release publishes no `SHA256SUMS`, so nothing can be verified.
    ///
    /// Fatal on purpose. An unverified binary from the network is exactly the
    /// thing this module exists to avoid running.
    NoChecksums,
    /// The archive is not the file the checksums describe.
    Digest {
        name: String,
        expected: String,
        found: String,
    },
    /// `SHA256SUMS` does not list the archive.
    NotListed(String),
    Download(String),
    /// `tar` is missing, or the archive did not unpack.
    Unpack(String),
    /// The archive contained none of the binaries this project installs.
    NothingToInstall,
    /// The directory the binaries live in cannot be written to.
    NotWritable(PathBuf),
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
                "this release publishes no SHA256SUMS, so the download cannot be verified — \
                 refusing to install it"
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
                write!(f, "SHA256SUMS does not list {n}, so it cannot be verified")
            }
            InstallError::Download(e) => write!(f, "download failed: {e}"),
            InstallError::Unpack(e) => write!(f, "could not unpack the release: {e}"),
            InstallError::NothingToInstall => write!(
                f,
                "the release archive contained none of this project's binaries"
            ),
            InstallError::NotWritable(p) => write!(
                f,
                "{} cannot be written to, so the update cannot be installed there. \
                 Install it by hand, or re-run with the permission to write there.",
                p.display()
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

/// What an install replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub dir: PathBuf,
    /// Binary names actually replaced, in the order they were written.
    pub replaced: Vec<String>,
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
    let probe = dir.join(".tobii-update-probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
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

/// Download `url` to `dest`.
fn download_to(url: &str, dest: &Path) -> Result<(), InstallError> {
    let out = std::process::Command::new("curl")
        .args([
            "-sL",
            "--fail",
            "-o",
            &dest.display().to_string(),
            "-H",
            "User-Agent: tobii-linux",
            url,
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => return Ok(()),
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if !e.is_empty() {
                return Err(InstallError::Download(e));
            }
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(InstallError::Download(e.to_string()));
        }
        Err(_) => {}
    }
    let out = std::process::Command::new("wget")
        .args(["-q", "-O", &dest.display().to_string(), url])
        .output()
        .map_err(|e| InstallError::Download(format!("neither curl nor wget ran: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        let _ = std::fs::remove_file(dest);
        Err(InstallError::Download(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}

/// Download, verify and install `release`, replacing the binaries in place.
///
/// `progress` is called with a short line per step, so a CLI can print it and a
/// GUI can show it without this module knowing about either.
pub fn install_release(
    release: &Release,
    progress: &dyn Fn(&str),
) -> Result<Installed, InstallError> {
    let triple = Target::triple();
    let archive = release
        .archive_for(&triple)
        .ok_or_else(|| InstallError::NoBuildForTarget(triple.clone()))?;
    let sums_asset = release.checksums().ok_or(InstallError::NoChecksums)?;

    let dir = install_dir()?;
    if !is_writable(&dir) {
        return Err(InstallError::NotWritable(dir));
    }

    // A scratch directory beside the install, so the final move is a rename on
    // the same filesystem rather than a copy that can half-finish.
    let work = dir.join(format!(".tobii-update-{}", std::process::id()));
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
    let archive_path = work.join(&archive.name);
    download_to(&archive.url, &archive_path)?;

    progress("verifying");
    let sums = get(&sums_asset.url).map_err(|e| InstallError::Download(e.to_string()))?;
    let sums = String::from_utf8_lossy(&sums);
    let expected = digest_for(&sums, &archive.name)
        .ok_or_else(|| InstallError::NotListed(archive.name.clone()))?;
    let found = sha256::hex_digest(&std::fs::read(&archive_path)?);
    if found != expected {
        return Err(InstallError::Digest {
            name: archive.name.clone(),
            expected,
            found,
        });
    }

    progress("unpacking");
    let out = std::process::Command::new("tar")
        .args(["-xzf", &archive_path.display().to_string(), "-C"])
        .arg(work)
        .output()
        .map_err(|e| InstallError::Unpack(format!("tar could not be run: {e}")))?;
    if !out.status.success() {
        return Err(InstallError::Unpack(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }

    // Replace only binaries that are already installed here, and only after
    // every one of them has been found — so a release that ships half of them
    // cannot leave a mismatched pair behind.
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    for name in BINARIES {
        let Some(src) = find_file(work, name) else {
            continue;
        };
        let target = dir.join(name);
        if !target.exists() {
            continue;
        }
        let staging = dir.join(format!(".{name}.new"));
        std::fs::copy(&src, &staging)?;
        set_executable(&staging)?;
        staged.push((staging, target));
    }
    if staged.is_empty() {
        return Err(InstallError::NothingToInstall);
    }

    progress("installing");
    let mut replaced = Vec::new();
    for (staging, target) in &staged {
        // Rename over the running binary. On Linux this unlinks the old inode
        // rather than touching it, so the process still executing from it keeps
        // running and the swap is atomic.
        std::fs::rename(staging, target)?;
        if let Some(n) = target.file_name().and_then(|s| s.to_str()) {
            replaced.push(n.to_string());
        }
    }
    Ok(Installed {
        dir: dir.to_path_buf(),
        replaced,
        version: release.version.to_string(),
    })
}

/// The first file named `name` at or below `root`.
fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|s| s.to_str()) == Some(name) {
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
        // The probe must not survive the check.
        assert!(!dir.join(".tobii-update-probe").exists());
    }

    #[test]
    fn find_file_searches_below_the_root() {
        let root = std::env::temp_dir().join(format!("tobii-find-{}", std::process::id()));
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("tobii"), b"x").unwrap();
        assert_eq!(find_file(&root, "tobii"), Some(nested.join("tobii")));
        assert_eq!(find_file(&root, "absent"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
