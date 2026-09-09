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
//! where it will be executed until it has been verified"), which would have led
//! the next maintainer to believe a boundary existed here that never did. Real
//! protection against a compromised release would need a signature over the
//! archive with a key pinned in this binary; there is none, and until there is,
//! everything below is about making TLS-to-GitHub the *only* way in — not about
//! making the download safe to distrust.
//!
//! So the rules here are narrow and absolute:
//!
//! * every URL must be `https://` on a GitHub host, checked before it is used;
//! * the URL is passed after `--`, never as a bare final argument;
//! * **every** redirect hop is checked the same way, on both backends;
//! * every request has a timeout, and every download a size cap.
//!
//! # curl is used when it exists; wget is not a fallback for failure
//!
//! `wget` is used **only when `curl` is not installed** — never because a curl
//! request failed. That distinction is load-bearing. Retrying a failed curl
//! request with wget quietly downgrades every guarantee curl was enforcing: a
//! download curl aborted for exceeding [`MAX_DOWNLOAD`] was re-fetched by wget,
//! which has no such option, so the cap curl applied was undone by the retry.
//!
//! # What the wget backend has to do by hand
//!
//! `wget --https-only` does **not** do what its name suggests. GNU wget's own
//! manual says "When in recursive mode, only HTTPS links are followed", and this
//! is not recursive mode — measured, it neither blocks a redirect off TLS nor
//! refuses a plain `http://` URL. So the wget path follows redirects itself,
//! one hop at a time with `--max-redirect=0`, and puts every `Location` through
//! the same [`is_trusted`] check as the original URL. That is stricter than
//! curl's `--proto-redir`, which only checks the scheme.

use std::io::Read;
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

/// How many redirects to follow. The same number on both backends.
const MAX_REDIRECTS: usize = 5;

/// Largest release asset this will download, in bytes.
///
/// The archive is read into memory whole to hash it, and the scratch directory
/// lives inside the install directory, so an implausible size is both a disk and
/// a memory problem. 512 MB is far above any real build here.
pub const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

/// curl's exit code for "exceeded the maximum allowed file size".
const CURL_TOO_LARGE: i32 = 63;

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
    /// More than [`MAX_REDIRECTS`] hops.
    TooManyRedirects,
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
            NetError::TooManyRedirects => {
                write!(
                    f,
                    "the download was redirected more than {MAX_REDIRECTS} times"
                )
            }
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
    // Hosts are case-insensitive; the allowlist is lower case.
    ALLOWED_HOSTS.iter().any(|h| host.eq_ignore_ascii_case(h))
}

fn check(url: &str) -> Result<(), NetError> {
    if is_trusted(url) {
        Ok(())
    } else {
        Err(NetError::Untrusted(url.to_string()))
    }
}

/// Arguments for a curl invocation.
///
/// `--proto =https` and `--proto-redir =https` matter: curl's default allows a
/// redirect from https to plain http, which would silently drop the only thing
/// protecting this exchange.
fn curl_args(url: &str, dest: Option<&Path>) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-sS".into(),
        "--fail".into(),
        "--location".into(),
        "--proto".into(),
        "=https".into(),
        "--proto-redir".into(),
        "=https".into(),
        "--max-redirs".into(),
        MAX_REDIRECTS.to_string(),
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
    ];
    if let Some(p) = dest {
        a.push("-o".into());
        a.push(p.display().to_string());
    }
    // Everything after this is an operand, never an option.
    a.push("--".into());
    a.push(url.into());
    a
}

/// Arguments for one wget hop.
///
/// `--max-redirect=0` on purpose: redirects are followed by [`wget_fetch`] so
/// that every hop can be checked against the allowlist, which wget cannot be
/// asked to do. `-S` prints the reply headers to stderr, which is where the
/// `Location` of a redirect is read from.
fn wget_args(url: &str, dest: Option<&Path>) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-q".into(),
        "-S".into(),
        "--max-redirect=0".into(),
        "--timeout".into(),
        CONNECT_TIMEOUT.into(),
        "--tries".into(),
        "2".into(),
        "--header".into(),
        "Accept: application/vnd.github+json".into(),
        "--user-agent".into(),
        "tobii-linux".into(),
        "-O".into(),
    ];
    match dest {
        Some(p) => a.push(p.display().to_string()),
        None => a.push("-".into()),
    }
    a.push("--".into());
    a.push(url.into());
    a
}

/// One finished process: what it wrote, and how it ended.
struct Ran {
    stdout: Vec<u8>,
    stderr: String,
    code: Option<i32>,
    ok: bool,
}

/// Run `prog`, capping how much of its stdout is kept.
///
/// The cap is applied while reading rather than afterwards, because
/// "afterwards" means the bytes are already resident: the point of a limit is
/// that a reply this program did not ask for cannot decide how much memory it
/// uses.
fn run(prog: &str, args: &[String], limit: u64) -> std::io::Result<Ran> {
    let mut child = std::process::Command::new(prog)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdout = Vec::new();
    if let Some(out) = child.stdout.take() {
        // `limit + 1` so a reply exactly at the cap stays distinguishable from
        // one that ran over it.
        out.take(limit.saturating_add(1)).read_to_end(&mut stdout)?;
    }
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let mut buf = Vec::new();
        let _ = err.read_to_end(&mut buf);
        stderr = String::from_utf8_lossy(&buf).trim().to_string();
    }
    let status = child.wait()?;
    Ok(Ran {
        stdout,
        stderr,
        code: status.code(),
        ok: status.success(),
    })
}

/// The reason a failed run should report, never empty.
///
/// `curl --fail` and `wget -q` both exit non-zero with nothing on stderr for an
/// ordinary HTTP error, which used to surface as "download failed: ".
fn reason(prog: &str, r: &Ran) -> String {
    if r.stderr.is_empty() {
        match r.code {
            Some(c) => format!("{prog} exited with status {c}"),
            None => format!("{prog} was killed by a signal"),
        }
    } else {
        r.stderr.clone()
    }
}

/// Fetch `url`, into `dest` if given and into memory otherwise.
fn fetch(url: &str, dest: Option<&Path>) -> Result<Vec<u8>, NetError> {
    check(url)?;
    match run("curl", &curl_args(url, dest), MAX_DOWNLOAD) {
        // curl is not installed. This is the ONLY reason to use wget.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => wget_fetch(url, dest),
        Err(e) => Err(NetError::Io(e)),
        Ok(r) if r.ok => {
            if r.stdout.len() as u64 > MAX_DOWNLOAD {
                return Err(NetError::TooLarge(r.stdout.len() as u64));
            }
            finish(dest, r.stdout)
        }
        Ok(r) => {
            if let Some(p) = dest {
                let _ = std::fs::remove_file(p);
            }
            // Curl aborting on the size cap is a size error, not a generic
            // failure — and emphatically not something to retry with a tool
            // that has no cap.
            if r.code == Some(CURL_TOO_LARGE) {
                return Err(NetError::TooLarge(MAX_DOWNLOAD));
            }
            Err(NetError::Failed(reason("curl", &r)))
        }
    }
}

/// Check what landed on disk, or hand back what was read into memory.
fn finish(dest: Option<&Path>, body: Vec<u8>) -> Result<Vec<u8>, NetError> {
    let Some(p) = dest else {
        return Ok(body);
    };
    let size = std::fs::metadata(p)?.len();
    if size > MAX_DOWNLOAD {
        let _ = std::fs::remove_file(p);
        return Err(NetError::TooLarge(size));
    }
    Ok(Vec::new())
}

/// Fetch with wget, following redirects one checked hop at a time.
fn wget_fetch(url: &str, dest: Option<&Path>) -> Result<Vec<u8>, NetError> {
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        check(&current)?;
        let r = match run("wget", &wget_args(&current, dest), MAX_DOWNLOAD) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(NetError::NoFetcher),
            Err(e) => return Err(NetError::Io(e)),
            Ok(r) => r,
        };
        // A redirect: wget refuses to follow it (`--max-redirect=0`) but still
        // prints the headers, so the next hop can be read out and checked.
        if let Some(next) = redirect_target(&r.stderr) {
            if let Some(p) = dest {
                // wget created the output file before it saw the 3xx.
                let _ = std::fs::remove_file(p);
            }
            current = next;
            continue;
        }
        if !r.ok {
            if let Some(p) = dest {
                let _ = std::fs::remove_file(p);
            }
            return Err(NetError::Failed(reason("wget", &r)));
        }
        if let Some(n) = content_length(&r.stderr) {
            if n > MAX_DOWNLOAD {
                if let Some(p) = dest {
                    let _ = std::fs::remove_file(p);
                }
                return Err(NetError::TooLarge(n));
            }
        }
        if r.stdout.len() as u64 > MAX_DOWNLOAD {
            return Err(NetError::TooLarge(r.stdout.len() as u64));
        }
        return finish(dest, r.stdout);
    }
    Err(NetError::TooManyRedirects)
}

/// The `Location` of a redirect, from wget's `-S` header dump.
///
/// Only a `3xx` reply is treated as a redirect: `Location` is meaningful on a
/// `201` too, and following that would fetch something the server did not send.
fn redirect_target(headers: &str) -> Option<String> {
    let mut is_redirect = false;
    let mut location = None;
    for line in headers.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("HTTP/") {
            // "HTTP/1.1 302 Found" — the code follows the version.
            let code = rest
                .split_whitespace()
                .nth(1)
                .and_then(|c| c.parse::<u16>().ok());
            // A new status line starts a new reply, so the previous one's
            // Location is forgotten: only the last reply in the dump counts.
            is_redirect = matches!(code, Some(300..=399));
            location = None;
        } else if let Some(v) = strip_header(line, "location") {
            location = Some(v.to_string());
        }
    }
    if is_redirect {
        location
    } else {
        None
    }
}

/// The `Content-Length` of the final reply, if it gave one.
fn content_length(headers: &str) -> Option<u64> {
    let mut last = None;
    for line in headers.lines() {
        if let Some(v) = strip_header(line.trim(), "content-length") {
            last = v.parse().ok();
        }
    }
    last
}

/// `name: value`, with a case-insensitive name.
fn strip_header<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let (k, v) = line.split_once(':')?;
    k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
}

/// Fetch `url` into memory.
pub fn get(url: &str) -> Result<Vec<u8>, NetError> {
    fetch(url, None)
}

/// Download `url` to `dest`.
pub fn download(url: &str, dest: &Path) -> Result<(), NetError> {
    fetch(url, Some(dest)).map(|_| ())
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
            // Hosts are case-insensitive.
            "https://GitHub.com/a/b",
            "https://API.GitHub.COM/x",
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
        let args = curl_args("https://github.com/x", None);
        let joined = args.join(" ");
        for flag in [
            "--proto =https",
            "--proto-redir =https",
            "--max-redirs",
            "--max-time",
            "--connect-timeout",
            "--max-filesize",
        ] {
            assert!(joined.contains(flag), "curl is missing {flag}: {joined}");
        }
        // The URL must come after `--`, so it can never be read as an option.
        let dashdash = args.iter().position(|s| s == "--").expect("a -- separator");
        assert_eq!(
            args.last().map(String::as_str),
            Some("https://github.com/x")
        );
        assert_eq!(dashdash, args.len() - 2, "-- must directly precede the URL");

        let w = wget_args("https://github.com/x", None);
        assert_eq!(w.iter().position(|s| s == "--"), Some(w.len() - 2));
        // wget must NOT be asked to follow redirects: it cannot check them, so
        // `wget_fetch` follows them itself and checks each hop.
        assert!(w.contains(&"--max-redirect=0".to_string()));
        assert!(
            w.contains(&"-S".to_string()),
            "headers are how a hop is read"
        );
        // `--https-only` must not sit here pretending to do something. GNU wget:
        // "When in recursive mode, only HTTPS links are followed" — this is not
        // recursive mode, and it neither blocks a redirect off TLS nor refuses a
        // plain http:// URL.
        assert!(
            !w.contains(&"--https-only".to_string()),
            "--https-only does nothing here and must not read as though it does"
        );
    }

    #[test]
    fn an_untrusted_url_is_rejected_before_any_process_is_spawned() {
        let e = get("http://evil.test/x").unwrap_err();
        assert!(matches!(e, NetError::Untrusted(_)), "{e}");
        let e = download("-K/tmp/x", Path::new("/tmp/should-not-exist")).unwrap_err();
        assert!(matches!(e, NetError::Untrusted(_)), "{e}");
        assert!(!Path::new("/tmp/should-not-exist").exists());
    }

    /// The wget backend's redirect handling, which exists because
    /// `--https-only` does not do it.
    #[test]
    fn a_redirect_is_read_from_the_headers_and_only_from_a_3xx() {
        let redirect = "  HTTP/1.1 302 Found\n  Server: x\n  Location: https://github.com/next\n";
        assert_eq!(
            redirect_target(redirect).as_deref(),
            Some("https://github.com/next")
        );
        // Case and spacing are the server's choice, not ours.
        assert_eq!(
            redirect_target("HTTP/2 301\nlocation:   https://github.com/a\n").as_deref(),
            Some("https://github.com/a")
        );
        // A Location on a non-redirect is not a redirect.
        assert_eq!(
            redirect_target("HTTP/1.1 201 Created\nLocation: https://github.com/made\n"),
            None
        );
        assert_eq!(
            redirect_target("HTTP/1.1 200 OK\nContent-Length: 5\n"),
            None
        );
        assert_eq!(redirect_target(""), None);
        // Only the last reply in the dump counts: the 200 that ends a chain
        // must not inherit the 302's Location.
        let chain = "HTTP/1.1 302 Found\nLocation: https://github.com/a\n\
                     HTTP/1.1 200 OK\nContent-Length: 3\n";
        assert_eq!(redirect_target(chain), None);
    }

    /// A redirect to somewhere off the allowlist must be refused, not followed.
    /// This is the check curl's `--proto-redir` cannot make and wget will not.
    #[test]
    fn a_redirect_off_the_allowlist_would_be_refused() {
        for hop in [
            "http://github.com/plain",
            "https://evil.test/x",
            "-K/tmp/evil",
        ] {
            let headers = format!("HTTP/1.1 302 Found\nLocation: {hop}\n");
            let next = redirect_target(&headers).expect("a location");
            assert!(!is_trusted(&next), "{hop} must not be followed");
        }
    }

    #[test]
    fn the_content_length_of_the_final_reply_is_what_is_checked() {
        assert_eq!(content_length("Content-Length: 4096\n"), Some(4096));
        assert_eq!(content_length("content-length:  17 \n"), Some(17));
        // Across a redirect chain, the last one wins.
        assert_eq!(
            content_length("Content-Length: 0\nHTTP/1.1 200 OK\nContent-Length: 99\n"),
            Some(99)
        );
        assert_eq!(content_length("Transfer-Encoding: chunked\n"), None);
        assert_eq!(content_length(""), None);
    }

    /// A failed curl request must NOT be retried with wget. Retrying undid
    /// every guarantee curl was enforcing — most concretely, a download curl
    /// aborted for exceeding the size cap was re-fetched by wget, which has no
    /// such option.
    #[test]
    fn curl_failing_is_reported_rather_than_retried_with_a_weaker_tool() {
        let src = include_str!("net.rs");
        let body = src
            .split("fn fetch(url: &str, dest: Option<&Path>)")
            .nth(1)
            .expect("fetch exists")
            .split("\n/// Check what landed")
            .next()
            .expect("fetch ends");
        assert!(
            body.contains("ErrorKind::NotFound => wget_fetch"),
            "wget must be reached only when curl is absent"
        );
        assert_eq!(
            body.matches("wget_fetch").count(),
            1,
            "a second wget_fetch call would be a fallback for failure"
        );
        assert!(
            body.contains("CURL_TOO_LARGE"),
            "curl's size-cap abort must be reported as a size error"
        );
    }
}
