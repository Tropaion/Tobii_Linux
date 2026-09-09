//! Asking GitHub what the latest release is.

use crate::json::{self, Value};
use crate::net;
use crate::version::Version;
use crate::{REPO_NAME, REPO_OWNER};

/// One downloadable file attached to a release.
#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// A published release, as much of it as this program cares about.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub tag: String,
    pub version: Version,
    /// The release notes, verbatim. Shown to the user as the changelog.
    pub notes: String,
    pub assets: Vec<Asset>,
    pub html_url: String,
}

impl Release {
    /// The archive built for `target`, if this release has one.
    pub fn archive_for(&self, target: &str) -> Option<&Asset> {
        self.assets
            .iter()
            .find(|a| a.name.contains(target) && a.name.ends_with(".tar.gz"))
    }

    /// The checksums file published beside the archives.
    pub fn checksums(&self) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == "SHA256SUMS")
    }
}

#[derive(Debug)]
pub enum CheckError {
    /// Neither `curl` nor `wget` is installed.
    NoFetcher,
    /// The request ran and failed — offline, rate-limited, or a 404.
    Fetch(String),
    /// The reply was not the shape a releases listing has.
    Malformed(String),
}

impl From<net::NetError> for CheckError {
    fn from(e: net::NetError) -> Self {
        match e {
            net::NetError::NoFetcher => CheckError::NoFetcher,
            other => CheckError::Fetch(other.to_string()),
        }
    }
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::NoFetcher => write!(
                f,
                "neither curl nor wget was found, so releases cannot be checked"
            ),
            CheckError::Fetch(e) => write!(f, "could not reach GitHub: {e}"),
            CheckError::Malformed(e) => write!(f, "unexpected reply from GitHub: {e}"),
        }
    }
}

impl std::error::Error for CheckError {}

/// The newest published release, or `None` when the project has none yet.
///
/// Drafts and pre-releases are skipped: a draft is not public and a
/// pre-release is not something to push at somebody who did not opt in.
pub fn latest() -> Result<Option<Release>, CheckError> {
    let url = format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases?per_page=20");
    let body = net::get(&url)?;
    let text = String::from_utf8_lossy(&body);
    parse_releases(&text)
}

/// The newest release in a GitHub `releases` listing.
///
/// Split from [`latest`] so the parsing is testable without a network.
pub fn parse_releases(text: &str) -> Result<Option<Release>, CheckError> {
    let doc = json::parse(text).map_err(|e| CheckError::Malformed(e.to_string()))?;
    let list = doc.as_array();
    if list.is_empty() && !matches!(doc, Value::Array(_)) {
        // A single object means an error reply, e.g. rate limiting; its
        // `message` is the useful part.
        let msg = doc.str("message").unwrap_or("not a releases listing");
        return Err(CheckError::Malformed(msg.to_string()));
    }
    let mut best: Option<Release> = None;
    for item in list {
        if item.flag("draft") || item.flag("prerelease") {
            continue;
        }
        let Some(tag) = item.str("tag_name") else {
            continue;
        };
        let Some(version) = Version::parse(tag) else {
            continue; // a tag that is not a version is not an update
        };
        let assets = item
            .array("assets")
            .iter()
            .filter_map(|a| {
                Some(Asset {
                    name: a.str("name")?.to_string(),
                    url: a.str("browser_download_url")?.to_string(),
                    size: match a.get("size") {
                        Some(Value::Number(n)) if *n >= 0.0 => *n as u64,
                        _ => 0,
                    },
                })
            })
            .collect();
        let candidate = Release {
            tag: tag.to_string(),
            version,
            notes: item.str("body").unwrap_or_default().to_string(),
            assets,
            html_url: item.str("html_url").unwrap_or_default().to_string(),
        };
        if best
            .as_ref()
            .is_none_or(|b| candidate.version.is_newer_than(&b.version))
        {
            best = Some(candidate);
        }
    }
    Ok(best)
}

/// Why a newer release cannot be installed from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// The release publishes no archive named for this target triple.
    NoBuildForTarget,
    /// There is a build, but no `SHA256SUMS` to tell a complete download from a
    /// truncated one.
    NoChecksums,
}

/// What a check found.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    /// Nothing published is newer than this build, or nothing installable is.
    UpToDate,
    /// A newer release exists, with a build for this machine.
    Newer(Box<Release>),
    /// A newer release exists but cannot be installed from here.
    ///
    /// Kept separate from `Newer` because the two need different words: an
    /// "Update" button that can only ever fail is worse than no button. `why`
    /// says which of the two reasons it is, because they send the user to
    /// different places — "no build for your machine" told to somebody whose
    /// build is right there, and only the checksums are missing, sends them
    /// looking for something that exists.
    CannotInstall {
        version: Version,
        url: String,
        why: Blocked,
    },
}

/// Compare the latest release with the running build.
///
/// A release is only offered when it actually carries something installable
/// here: an archive named for this target triple, and the checksums to tell a
/// complete download from a truncated one. Without that test, a project that
/// publishes only x86-64 builds showed an aarch64 user a banner at every launch
/// that could never do anything.
pub fn check() -> Result<Check, CheckError> {
    let running = Version::current();
    let triple = crate::install::Target::triple();
    Ok(decide(latest()?, &running, &triple))
}

/// The decision [`check`] makes, without the network.
///
/// Split out because `check` takes no arguments and reaches the network on its
/// own, so nothing could test the gate that is the whole point of it: neutering
/// the target-triple check left all 51 tests green. This is that gate, and it
/// is tested directly.
pub fn decide(latest: Option<Release>, running: &Version, triple: &str) -> Check {
    let Some(r) = latest else {
        return Check::UpToDate;
    };
    if !r.version.is_newer_than(running) {
        return Check::UpToDate;
    }
    // Both are required to install: the archive for this machine, and the
    // checksums without which a truncated download cannot be told from a
    // complete one. Missing either means the Update button could only fail.
    let why = if r.archive_for(triple).is_none() {
        Some(Blocked::NoBuildForTarget)
    } else if r.checksums().is_none() {
        Some(Blocked::NoChecksums)
    } else {
        None
    };
    if let Some(why) = why {
        return Check::CannotInstall {
            version: r.version.clone(),
            url: if r.html_url.is_empty() {
                crate::releases_url()
            } else {
                r.html_url.clone()
            },
            why,
        };
    }
    Check::Newer(Box::new(r))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed copy of the shape GitHub's releases endpoint returns.
    // `r###`: the notes hold `"##` — a markdown heading straight after a quote
    // — which closes an `r#` or `r##` literal mid-string.
    const LISTING: &str = r###"[
      {"tag_name":"v0.3.0","draft":false,"prerelease":false,
       "html_url":"https://example.invalid/r/0.3.0",
       "body":"## Added\n- head tracking\n",
       "assets":[
         {"name":"tobii-linux-0.3.0-x86_64-unknown-linux-gnu.tar.gz",
          "browser_download_url":"https://example.invalid/a.tar.gz","size":4096},
         {"name":"SHA256SUMS","browser_download_url":"https://example.invalid/s","size":120}]},
      {"tag_name":"v0.4.0","draft":true,"prerelease":false,"body":"","assets":[]},
      {"tag_name":"v0.5.0","draft":false,"prerelease":true,"body":"","assets":[]},
      {"tag_name":"v0.2.0","draft":false,"prerelease":false,"body":"older","assets":[]}
    ]"###;

    #[test]
    fn the_newest_published_release_wins() {
        let r = parse_releases(LISTING).unwrap().expect("a release");
        assert_eq!(r.tag, "v0.3.0");
        assert_eq!(r.version.to_string(), "0.3.0");
        assert_eq!(r.notes, "## Added\n- head tracking\n");
        assert_eq!(r.html_url, "https://example.invalid/r/0.3.0");
    }

    /// A draft is not public, and a pre-release is not something to push at
    /// somebody who did not ask for one — even though both sort higher.
    #[test]
    fn drafts_and_prereleases_are_skipped() {
        let r = parse_releases(LISTING).unwrap().unwrap();
        assert_eq!(r.tag, "v0.3.0", "0.4.0 is a draft and 0.5.0 a pre-release");
    }

    #[test]
    fn assets_are_matched_by_target_triple_and_by_name() {
        let r = parse_releases(LISTING).unwrap().unwrap();
        let a = r
            .archive_for("x86_64-unknown-linux-gnu")
            .expect("the linux archive");
        assert_eq!(a.size, 4096);
        assert!(a.url.ends_with(".tar.gz"));
        assert!(r.archive_for("aarch64-unknown-linux-gnu").is_none());
        assert_eq!(r.checksums().map(|a| a.name.as_str()), Some("SHA256SUMS"));
    }

    /// A repository with no releases is the normal state of a young project,
    /// not an error to show the user.
    #[test]
    fn an_empty_listing_is_not_an_error() {
        assert_eq!(parse_releases("[]").unwrap(), None);
    }

    /// GitHub answers rate limiting with an object, not a list. Reporting its
    /// message beats reporting "malformed".
    #[test]
    fn an_api_error_object_is_reported_with_its_message() {
        let e = parse_releases(r#"{"message":"API rate limit exceeded"}"#).unwrap_err();
        assert!(e.to_string().contains("rate limit"), "{e}");
    }

    #[test]
    fn a_tag_that_is_not_a_version_is_not_an_update() {
        let json = r#"[{"tag_name":"nightly","draft":false,"prerelease":false,
                        "body":"","assets":[]}]"#;
        assert_eq!(parse_releases(json).unwrap(), None);
    }

    /// An asset name is only a name — it must be a plain file name before it
    /// is joined onto a directory, and `archive_for` is where the name that
    /// gets downloaded is chosen.
    #[test]
    fn an_archive_is_matched_by_the_full_triple_not_a_prefix() {
        let r = parse_releases(LISTING).unwrap().unwrap();
        // `x86_64-unknown-linux-gnu` must not be satisfied by a musl archive.
        assert!(r.archive_for("x86_64-unknown-linux-musl").is_none());
        assert!(
            r.archive_for("").is_some(),
            "an empty triple matches anything"
        );
    }

    fn rel(tag: &str, assets: &[(&str, &str)]) -> Release {
        Release {
            tag: tag.to_string(),
            version: Version::parse(tag).unwrap(),
            notes: String::new(),
            assets: assets
                .iter()
                .map(|(n, u)| Asset {
                    name: n.to_string(),
                    url: u.to_string(),
                    size: 0,
                })
                .collect(),
            html_url: "https://example.invalid/r".into(),
        }
    }

    const TRIPLE: &str = "x86_64-unknown-linux-gnu";

    fn full(tag: &str) -> Release {
        rel(
            tag,
            &[
                (
                    &format!(
                        "tobii-linux-{}-{TRIPLE}.tar.gz",
                        tag.trim_start_matches('v')
                    ),
                    "https://github.com/a/b/x.tar.gz",
                ),
                ("SHA256SUMS", "https://github.com/a/b/s"),
            ],
        )
    }

    /// The gate `check` exists for. Before this test, changing the condition to
    /// `if false && (...)` left every other test in the crate green.
    #[test]
    fn an_update_is_only_offered_when_it_can_actually_be_installed() {
        let running = Version::parse("0.1.0").unwrap();

        // The ordinary case: newer, with a build for us and checksums.
        assert!(matches!(
            decide(Some(full("v0.2.0")), &running, TRIPLE),
            Check::Newer(_)
        ));

        // A build for a different machine is not an update we can offer.
        let other = decide(Some(full("v0.2.0")), &running, "aarch64-unknown-linux-gnu");
        match other {
            Check::CannotInstall { version, url, why } => {
                assert_eq!(version.to_string(), "0.2.0");
                assert!(!url.is_empty(), "the user needs somewhere to go");
                assert_eq!(why, Blocked::NoBuildForTarget);
            }
            o => panic!("expected NotForThisTarget, got {o:?}"),
        }

        // An archive for us but no checksums: also not installable, and it must
        // NOT be reported as "no build for this machine" — that sends the user
        // looking for a build that is right there.
        let no_sums = rel(
            "v0.2.0",
            &[(
                &format!("tobii-linux-0.2.0-{TRIPLE}.tar.gz"),
                "https://github.com/a/b/x.tar.gz",
            )],
        );
        // ...and it must be reported as MISSING CHECKSUMS, not as "no build
        // for your machine": the build is right there, and sending the user to
        // look for one that exists is the worse of the two wrong answers.
        assert!(matches!(
            decide(Some(no_sums), &running, TRIPLE),
            Check::CannotInstall {
                why: Blocked::NoChecksums,
                ..
            }
        ));
    }

    #[test]
    fn nothing_newer_is_up_to_date_whatever_it_ships() {
        let running = Version::parse("0.2.0").unwrap();
        assert_eq!(decide(None, &running, TRIPLE), Check::UpToDate);
        assert_eq!(
            decide(Some(full("v0.2.0")), &running, TRIPLE),
            Check::UpToDate
        );
        assert_eq!(
            decide(Some(full("v0.1.0")), &running, TRIPLE),
            Check::UpToDate
        );
        // Not even when the older release has no build for us: there is nothing
        // to tell the user about.
        assert_eq!(
            decide(Some(full("v0.1.0")), &running, "mips-unknown-none"),
            Check::UpToDate
        );
    }

    #[test]
    fn garbage_is_an_error_rather_than_a_silent_no_update() {
        assert!(parse_releases("not json").is_err());
        assert!(parse_releases("").is_err());
    }
}
