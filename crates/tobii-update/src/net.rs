//! The one place this crate talks to the network.
//!
//! # What is actually trusted, and what is not
//!
//! **The trust anchor is TLS to GitHub, and nothing else.** The release
//! metadata, the archive and the `SHA256SUMS` all come from the same place over
//! the same connection, so the checksum gate catches a truncated or corrupted
//! download — it does **not** make a hostile release safe. Whoever can serve the
//! JSON can serve an archive and a matching digest for it.
//!
//! An earlier version of this crate documented the opposite ("nothing is put
//! where it will be executed until it has been verified", "an unverified binary
//! from the network is exactly the thing this module exists to avoid running"),
//! which would have led the next maintainer to believe a boundary existed here
//! that never did. Real protection against a compromised release would need a
//! signature over the archive with a key pinned in this binary; there is none,
//! and until there is, everything below is about making TLS-to-GitHub the
//! *only* way in — not about making the download safe to distrust.
//!
//! So the rules here are narrow and absolute:
//!
//! * every URL must be `https://` on a GitHub host, checked before it is used;
//! * the URL is passed after `--`, never as a bare final argument;
//! * redirects may not leave https, and are bounded;
//! * every request has a timeout, and every download a size cap.

use std::path::Path;

/// Hosts a release asset or API reply may come from.
///
/// An allowlist rather than a scheme check alone: the URLs are read out of a
/// document fetched over the network, so "it starts with https" only says the
/// attacker chose https.
const ALLOWED_HOSTS: [&str; 4] = [
    "api.github.com",
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// Seconds before a stalled request is abandoned.
///
/// There was no timeout of any kind. A connection that opens and then goes
/// quiet left the hub's launch-time check thread — and, once Update was pressed,
/// the banner with every button disabled — waiting for the life of the process.
const CONNECT_TIMEOUT: &str = "10";
const MAX_TIME: &str = "600";

/// Largest release asset this will download, in bytes.
///
/// The archive is read into memory whole to hash it, and the scratch directory
/// lives inside the install directory, so an implausible size is both a disk and
/// a memory problem. 512 MB is far above any real build here.
pub const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

#[derive(Debug)]
pub enum NetError {
    /// The URL is not https on a GitHub host.
    Untrusted(String),
    /// Neither `curl` nor `wget` is installed.
    NoFetcher,
    /// The request ran and failed.
    Failed(String),
    /// The reply was larger than [`MAX_DOWNLOAD`].
    TooLarge(u64),
    Io(std::io::Error),
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetError::Untrusted(u) => write!(
                f,
                "refusing to fetch {u}: release assets must be https on a GitHub host"
            ),
            NetError::NoFetcher => write!(f, "neither curl nor wget was found"),
            NetError::Failed(e) => write!(f, "{e}"),
            NetError::TooLarge(n) => write!(f, "the download is {n} bytes, which is too large"),
            NetError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<std::io::Error> for NetError {
    fn from(e: std::io::Error) -> Self {
        NetError::Io(e)
    }
}

/// Whether `url` is one this crate will fetch.
///
/// Rejects anything that is not `https://` on an allowlisted host. In
/// particular it rejects a URL beginning with `-`, which `curl` and `wget` would
/// otherwise read as an option: a value like `-K/tmp/x` makes curl take its
/// real URL and its output path from a file on disk, which turns "download this
/// release" into "write anything anywhere".
pub fn is_trusted(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    // Authority ends at the first '/', '?' or '#'. Userinfo before an '@' can
    // hide the real host (`https://api.github.com@evil.test/`), so anything
    // with one is refused rather than parsed.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') || authority.is_empty() {
        return false;
    }
    let host = authority.split(':').next().unwrap_or_default();
    ALLOWED_HOSTS.contains(&host)
}

fn check(url: &str) -> Result<(), NetError> {
    if is_trusted(url) {
        Ok(())
    } else {
        Err(NetError::Untrusted(url.to_string()))
    }
}

/// Arguments common to every curl invocation.
///
/// `--proto =https` and `--proto-redir =https` matter: curl's default allows a
/// redirect from https to plain http, which would silently drop the only thing
/// protecting this exchange.
fn curl_args(url: &str) -> Vec<String> {
    vec![
        "-sS".into(),
        "--fail".into(),
        "--location".into(),
        "--proto".into(),
        "=https".into(),
        "--proto-redir".into(),
        "=https".into(),
        "--max-redirs".into(),
        "5".into(),
        "--connect-timeout".into(),
        CONNECT_TIMEOUT.into(),
        "--max-time".into(),
        MAX_TIME.into(),
        "--max-filesize".into(),
        MAX_DOWNLOAD.to_string(),
        "-H".into(),
        "Accept: application/vnd.github+json".into(),
        "-H".into(),
        "User-Agent: tobii-linux".into(),
        // Everything after this is an operand, never an option.
        "--".into(),
        url.into(),
    ]
}

fn wget_args(url: &str) -> Vec<String> {
    vec![
        "-q".into(),
        "--https-only".into(),
        "--max-redirect".into(),
        "5".into(),
        "--timeout".into(),
        CONNECT_TIMEOUT.into(),
        "--tries".into(),
        "2".into(),
        "-O".into(),
        "-".into(),
        "--".into(),
        url.into(),
    ]
}

fn run(prog: &str, args: &[String]) -> Result<Result<Vec<u8>, String>, std::io::Error> {
    let out = std::process::Command::new(prog).args(args).output()?;
    if out.status.success() {
        return Ok(Ok(out.stdout));
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    // Never return an empty reason: `curl --fail` and `wget -q` both exit
    // non-zero with nothing on stderr for an ordinary HTTP error, which used to
    // surface to the user as "download failed: ".
    Ok(Err(if err.is_empty() {
        format!("{prog} exited with {}", out.status)
    } else {
        err
    }))
}

/// Fetch `url` into memory.
pub fn get(url: &str) -> Result<Vec<u8>, NetError> {
    check(url)?;
    let mut last: Option<String> = None;
    for (prog, args) in [("curl", curl_args(url)), ("wget", wget_args(url))] {
        match run(prog, &args) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => last = Some(e.to_string()),
            Ok(Ok(body)) => {
                if body.len() as u64 > MAX_DOWNLOAD {
                    return Err(NetError::TooLarge(body.len() as u64));
                }
                return Ok(body);
            }
            Ok(Err(e)) => last = Some(e),
        }
    }
    Err(last.map_or(NetError::NoFetcher, NetError::Failed))
}

/// Download `url` to `dest`.
pub fn download(url: &str, dest: &Path) -> Result<(), NetError> {
    check(url)?;
    let target = dest.display().to_string();
    let mut curl = curl_args(url);
    // Insert before the trailing `--`/url pair.
    let at = curl.len() - 2;
    curl.splice(at..at, ["-o".to_string(), target.clone()]);
    let mut wget = wget_args(url);
    let at = wget.len() - 2;
    wget.splice(at..at, ["-O".to_string(), target.clone()]);
    // `wget_args` writes to stdout by default; drop that pair.
    if let Some(i) = wget.iter().position(|a| a == "-O") {
        if wget.get(i + 1).map(String::as_str) == Some("-") {
            wget.drain(i..i + 2);
        }
    }

    let mut last: Option<String> = None;
    for (prog, args) in [("curl", curl), ("wget", wget)] {
        match run(prog, &args) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => last = Some(e.to_string()),
            Ok(Ok(_)) => {
                let size = std::fs::metadata(dest)?.len();
                if size > MAX_DOWNLOAD {
                    let _ = std::fs::remove_file(dest);
                    return Err(NetError::TooLarge(size));
                }
                return Ok(());
            }
            Ok(Err(e)) => {
                let _ = std::fs::remove_file(dest);
                last = Some(e);
            }
        }
    }
    Err(last.map_or(NetError::NoFetcher, NetError::Failed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one that mattered: a URL beginning with `-` is read by curl and wget
    /// as an option. `-K<file>` makes curl take its URL and its output path from
    /// a file on disk, turning a download into an arbitrary write.
    #[test]
    fn a_url_that_is_really_an_option_is_refused() {
        for hostile in [
            "-K/tmp/evil.conf",
            "--output-dir=/tmp",
            "-O/tmp/pwned",
            "-",
            "--config=/tmp/x",
        ] {
            assert!(!is_trusted(hostile), "{hostile} must not be fetched");
        }
    }

    #[test]
    fn only_https_on_a_github_host_is_fetched() {
        for ok in [
            "https://api.github.com/repos/a/b/releases",
            "https://github.com/a/b/releases/download/v1/x.tar.gz",
            "https://objects.githubusercontent.com/x",
            "https://release-assets.githubusercontent.com/y",
        ] {
            assert!(is_trusted(ok), "{ok} should be allowed");
        }
        for bad in [
            "http://github.com/a",                    // not TLS
            "https://evil.test/x",                    // wrong host
            "https://github.com.evil.test/x",         // suffix trick
            "https://api.github.com@evil.test/x",     // userinfo hides the host
            "https://evil.test/https://github.com/x", // path only looks right
            "ftp://github.com/x",
            "file:///etc/passwd",
            "",
        ] {
            assert!(!is_trusted(bad), "{bad} must be refused");
        }
    }

    /// A port is allowed to be present but must not change which host matched.
    #[test]
    fn a_port_does_not_smuggle_a_different_host() {
        assert!(is_trusted("https://github.com:443/a/b"));
        assert!(!is_trusted("https://evil.test:443/a/b"));
    }

    #[test]
    fn every_request_is_bounded_and_pinned_to_https() {
        let a = curl_args("https://github.com/x").join(" ");
        for flag in [
            "--proto =https",
            "--proto-redir =https",
            "--max-redirs",
            "--max-time",
            "--connect-timeout",
            "--max-filesize",
        ] {
            assert!(a.contains(flag), "curl is missing {flag}: {a}");
        }
        // The URL must come after `--`, so it can never be read as an option.
        let args = curl_args("https://github.com/x");
        let dashdash = args.iter().position(|s| s == "--").expect("a -- separator");
        assert_eq!(
            args.last().map(String::as_str),
            Some("https://github.com/x")
        );
        assert_eq!(dashdash, args.len() - 2, "-- must directly precede the URL");

        let w = wget_args("https://github.com/x");
        assert!(w.contains(&"--https-only".to_string()));
        assert_eq!(w.iter().position(|s| s == "--"), Some(w.len() - 2));
    }

    #[test]
    fn an_untrusted_url_is_rejected_before_any_process_is_spawned() {
        let e = get("http://evil.test/x").unwrap_err();
        assert!(matches!(e, NetError::Untrusted(_)), "{e}");
        let e = download("-K/tmp/x", Path::new("/tmp/should-not-exist")).unwrap_err();
        assert!(matches!(e, NetError::Untrusted(_)), "{e}");
        assert!(!Path::new("/tmp/should-not-exist").exists());
    }
}
