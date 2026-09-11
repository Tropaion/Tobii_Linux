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

    /// What somebody whose copy belongs to `manager` should be handed.
    ///
    /// `manager` is the tool that *answered* the ownership question — `dpkg`,
    /// `rpm` or `pacman`, the three [`crate::install::package_owner`] asks —
    /// not the command a person upgrades with.
    ///
    /// The channels are not interchangeable. A packaged install lives in
    /// `/usr/bin` and is recorded in a package database; the `.tar.gz` unpacks
    /// into `~/.local/bin` and is recorded nowhere. Handing the archive to
    /// somebody who installed the `.deb` gives them a second copy shadowing the
    /// first, which is why the manager decides the file rather than the file
    /// deciding itself.
    ///
    /// `pacman` has two formats, tried in order: the prebuilt `.pkg.tar.zst`
    /// for this machine, which is one file and needs no toolchain; then the
    /// `PKGBUILD` pair, which compiles the whole workspace. A release from
    /// before the prebuilt package existed, or one that only built it for
    /// another architecture, still has the pair.
    ///
    /// Falls back to the archive when this release publishes no package for
    /// that manager *and* this machine: an archive is at least installable by
    /// hand, where a `.deb` built for another architecture is not.
    pub fn offer_for(&self, manager: &str, triple: &str) -> Option<Offer<'_>> {
        let arch = triple.split('-').next().unwrap_or_default();
        let ending = |suffix: String| -> Vec<&Asset> {
            self.assets
                .iter()
                .filter(|a| a.name.ends_with(&suffix))
                .collect()
        };
        let (channel, files) = match manager {
            "dpkg" => (Channel::Deb, ending(format!("_{}.deb", deb_arch(arch)))),
            "rpm" => (Channel::Rpm, ending(format!(".{arch}.rpm"))),
            "pacman" => match self.pacman_package(arch) {
                Some(p) => (Channel::Pacman, vec![p]),
                None => (Channel::Pkgbuild, self.pkgbuild_pair()),
            },
            _ => (Channel::Archive, Vec::new()),
        };
        if !files.is_empty() {
            return Some(Offer { channel, files });
        }
        self.archive_for(triple).map(|a| Offer {
            channel: Channel::Archive,
            files: vec![a],
        })
    }

    /// The prebuilt Arch package for `arch`, if this release has one.
    ///
    /// makepkg names it `tobii-linux-bin-<pkgver>-<pkgrel>-<arch>.pkg.tar.zst`
    /// (release.yml's `arch` job builds it from `scripts/aur-bin.sh`'s
    /// PKGBUILD), so the architecture is the whole segment before the
    /// extension. Never `tobii-linux-bin-debug-…`: makepkg splits detached
    /// debug symbols into that package, and it installs no program. The job
    /// builds with `!debug` and fails if one appears; this is the other half.
    fn pacman_package(&self, arch: &str) -> Option<&Asset> {
        if arch.is_empty() {
            return None;
        }
        let ending = format!("-{arch}.pkg.tar.zst");
        self.assets.iter().find(|a| {
            a.name.starts_with(PACMAN_PREFIX)
                && !a.name.starts_with(PACMAN_DEBUG_PREFIX)
                && a.name.ends_with(&ending)
        })
    }

    /// `PKGBUILD` and the hook `makepkg` reads from beside it.
    ///
    /// Both or neither: the PKGBUILD's `install=` names the second file, and
    /// `makepkg` stops with "install scriptlet not found" when it is missing.
    /// Half the pair looks like a download that worked and is not one.
    fn pkgbuild_pair(&self) -> Vec<&Asset> {
        let named = |n: &str| self.assets.iter().find(|a| a.name == n);
        match (named(PKGBUILD), named(PKGBUILD_INSTALL)) {
            (Some(p), Some(i)) => vec![p, i],
            _ => Vec::new(),
        }
    }
}

/// The PKGBUILD `scripts/package.sh` publishes, and its install hook.
const PKGBUILD: &str = "PKGBUILD";
const PKGBUILD_INSTALL: &str = "tobii-linux.install";

/// The prebuilt Arch package's file name starts with its package name, and
/// makepkg's split debug package with that name plus `-debug`.
const PACMAN_PREFIX: &str = "tobii-linux-bin-";
const PACMAN_DEBUG_PREFIX: &str = "tobii-linux-bin-debug-";

/// Debian's name for a machine architecture.
///
/// `.deb` files are labelled `amd64` and `arm64` where a target triple says
/// `x86_64` and `aarch64` — `scripts/package.sh` does this same mapping when it
/// names the file — so matching an asset name against the triple's own word
/// finds nothing at all on the one architecture this project actually ships.
fn deb_arch(arch: &str) -> &str {
    match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Which channel a set of assets came from, and so how it is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// A `.deb`, for `apt` and friends.
    Deb,
    /// An `.rpm`, for `dnf` or `zypper`.
    Rpm,
    /// A prebuilt `.pkg.tar.zst` (`tobii-linux-bin`), installed with
    /// `pacman -U`.
    Pacman,
    /// A `PKGBUILD` and its install hook, built with `makepkg`.
    Pkgbuild,
    /// The release archive, unpacked and installed by hand.
    Archive,
}

/// The files to hand a user, and what they are.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer<'a> {
    pub channel: Channel,
    /// Never empty, in the order they should be downloaded.
    pub files: Vec<&'a Asset>,
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
                // `size` is deliberately not read. GitHub sends it and it
                // looks useful, but nothing bounds a download by it: the cap is
                // `net::MAX_DOWNLOAD`, enforced by curl's `--max-filesize`, by
                // `run()`'s capped read, and by `finish()`'s metadata check —
                // none of which trust a number the server chose. A field that is
                // parsed and stored but never read reads like a guard that
                // exists.
                Some(Asset {
                    name: a.str("name")?.to_string(),
                    url: a.str("browser_download_url")?.to_string(),
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
        Blocked::NoBuildForTarget
    } else if r.checksums().is_none() {
        Blocked::NoChecksums
    } else {
        return Check::Newer(Box::new(r));
    };
    Check::CannotInstall {
        version: r.version,
        url: if r.html_url.is_empty() {
            crate::releases_url()
        } else {
            r.html_url
        },
        why,
    }
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

    /// `archive_for` matches the whole triple, so a musl build is not handed
    /// the gnu archive.
    #[test]
    fn an_archive_is_matched_by_the_full_triple_not_a_prefix() {
        let r = parse_releases(LISTING).unwrap().unwrap();
        // The listing's only archive is gnu; a musl build must not get it.
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
            o => panic!("expected CannotInstall(NoBuildForTarget), got {o:?}"),
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

    /// Everything `scripts/release.sh` and `scripts/package.sh` publish, plus
    /// the packages for the *other* architecture, which a release built on two
    /// runners carries.
    fn packaged() -> Release {
        rel(
            "v0.3.0",
            &[
                (
                    &format!("tobii-linux-0.3.0-{TRIPLE}.tar.gz"),
                    "https://github.com/a/b/t.tar.gz",
                ),
                ("tobii-linux_0.3.0_amd64.deb", "https://github.com/a/b/d"),
                ("tobii-linux_0.3.0_arm64.deb", "https://github.com/a/b/d64"),
                ("tobii-linux-0.3.0-1.x86_64.rpm", "https://github.com/a/b/r"),
                (
                    "tobii-linux-0.3.0-1.aarch64.rpm",
                    "https://github.com/a/b/r64",
                ),
                // aarch64 first, so a match on "any pkg.tar.zst" would pick
                // the wrong one on the x86_64 machine the tests pretend to be.
                (
                    "tobii-linux-bin-0.3.0-1-aarch64.pkg.tar.zst",
                    "https://github.com/a/b/z64",
                ),
                (
                    "tobii-linux-bin-0.3.0-1-x86_64.pkg.tar.zst",
                    "https://github.com/a/b/z",
                ),
                ("PKGBUILD", "https://github.com/a/b/p"),
                ("tobii-linux.install", "https://github.com/a/b/i"),
                ("SHA256SUMS", "https://github.com/a/b/s"),
            ],
        )
    }

    fn without(mut r: Release, gone: &[&str]) -> Release {
        r.assets.retain(|a| !gone.contains(&a.name.as_str()));
        r
    }

    const PKG_X86: &str = "tobii-linux-bin-0.3.0-1-x86_64.pkg.tar.zst";
    const PKG_ARM: &str = "tobii-linux-bin-0.3.0-1-aarch64.pkg.tar.zst";

    fn names<'a>(o: &Offer<'a>) -> Vec<&'a str> {
        o.files.iter().map(|a| a.name.as_str()).collect()
    }

    /// The whole point of the split: the file offered is decided by the package
    /// manager that owns the copy, and by the architecture — a `.deb` for the
    /// wrong machine is not an answer.
    #[test]
    fn each_package_manager_is_offered_its_own_format_for_this_machine() {
        let r = packaged();

        let deb = r.offer_for("dpkg", TRIPLE).expect("a deb");
        assert_eq!(deb.channel, Channel::Deb);
        assert_eq!(names(&deb), ["tobii-linux_0.3.0_amd64.deb"]);

        let rpm = r.offer_for("rpm", TRIPLE).expect("an rpm");
        assert_eq!(rpm.channel, Channel::Rpm);
        assert_eq!(names(&rpm), ["tobii-linux-0.3.0-1.x86_64.rpm"]);

        // Same release, other machine: the arm64/aarch64 packages, not the
        // x86_64 ones that happen to be listed first.
        let arm = "aarch64-unknown-linux-gnu";
        assert_eq!(
            names(&r.offer_for("dpkg", arm).unwrap()),
            ["tobii-linux_0.3.0_arm64.deb"]
        );
        assert_eq!(
            names(&r.offer_for("rpm", arm).unwrap()),
            ["tobii-linux-0.3.0-1.aarch64.rpm"]
        );
    }

    /// `makepkg` reads the install hook by name from beside the PKGBUILD, so
    /// the pair travels together or not at all. (Without the prebuilt
    /// packages, which would otherwise be offered first.)
    #[test]
    fn arch_gets_the_pkgbuild_and_its_install_hook_together() {
        let all = without(packaged(), &[PKG_X86, PKG_ARM]);
        let o = all.offer_for("pacman", TRIPLE).expect("a PKGBUILD");
        assert_eq!(o.channel, Channel::Pkgbuild);
        assert_eq!(names(&o), ["PKGBUILD", "tobii-linux.install"]);

        // Half of it is not a smaller version of it: without the hook the
        // PKGBUILD does not build, so this falls back to the archive.
        let half = without(all, &["tobii-linux.install"]);
        let o = half.offer_for("pacman", TRIPLE).expect("the archive");
        assert_eq!(o.channel, Channel::Archive);
        assert_eq!(names(&o), [format!("tobii-linux-0.3.0-{TRIPLE}.tar.gz")]);
    }

    /// The prebuilt package is one file and needs no Rust toolchain, so it is
    /// what pacman is offered whenever there is one for this machine — and
    /// only the one for this machine.
    #[test]
    fn arch_gets_the_prebuilt_package_for_its_own_machine() {
        let r = packaged();

        let o = r.offer_for("pacman", TRIPLE).expect("a package");
        assert_eq!(o.channel, Channel::Pacman);
        assert_eq!(names(&o), [PKG_X86]);

        let o = r
            .offer_for("pacman", "aarch64-unknown-linux-gnu")
            .expect("a package");
        assert_eq!(o.channel, Channel::Pacman);
        assert_eq!(names(&o), [PKG_ARM]);
    }

    /// makepkg splits detached debug symbols into `tobii-linux-bin-debug-…`,
    /// which installs no program. It is never the file handed over — not when
    /// it is listed first, and not when it is the only one for this machine.
    #[test]
    fn a_debug_package_is_never_offered() {
        let debug = "tobii-linux-bin-debug-0.3.0-1-x86_64.pkg.tar.zst";
        let mut r = packaged();
        r.assets.insert(
            0,
            Asset {
                name: debug.into(),
                url: "https://github.com/a/b/dbg".into(),
            },
        );
        assert_eq!(names(&r.offer_for("pacman", TRIPLE).unwrap()), [PKG_X86]);

        let only_debug = without(r, &[PKG_X86]);
        let o = only_debug.offer_for("pacman", TRIPLE).unwrap();
        assert_eq!(o.channel, Channel::Pkgbuild, "{:?}", names(&o));
    }

    /// A package built for another machine is not an answer. Without one for
    /// this machine pacman gets the PKGBUILD pair, and without that the
    /// archive — the same order as a release that never had a package.
    #[test]
    fn another_machines_package_falls_back_to_the_pkgbuild_then_the_archive() {
        let arm_only = without(packaged(), &[PKG_X86]);
        let o = arm_only.offer_for("pacman", TRIPLE).unwrap();
        assert_eq!(o.channel, Channel::Pkgbuild);
        assert_eq!(names(&o), ["PKGBUILD", "tobii-linux.install"]);

        let no_pair = without(arm_only, &["PKGBUILD", "tobii-linux.install"]);
        let o = no_pair.offer_for("pacman", TRIPLE).unwrap();
        assert_eq!(o.channel, Channel::Archive);
        assert_eq!(names(&o), [format!("tobii-linux-0.3.0-{TRIPLE}.tar.gz")]);
    }

    /// Only pacman is handed a `.pkg.tar.zst`: the deb, rpm and archive
    /// matchers must not be satisfied by one, on either machine.
    #[test]
    fn no_other_channel_is_handed_an_arch_package() {
        let r = packaged();
        for triple in [TRIPLE, "aarch64-unknown-linux-gnu"] {
            for manager in ["dpkg", "rpm", "nix", ""] {
                if let Some(o) = r.offer_for(manager, triple) {
                    assert!(
                        o.files.iter().all(|a| !a.name.ends_with(".pkg.tar.zst")),
                        "{manager} on {triple} was offered {:?}",
                        names(&o)
                    );
                }
            }
            if let Some(a) = r.archive_for(triple) {
                assert!(!a.name.ends_with(".pkg.tar.zst"), "{}", a.name);
            }
        }
    }

    /// A manager this project has never packaged for, and a release that
    /// carries no packages at all, both land on the archive rather than on
    /// nothing.
    #[test]
    fn anything_unpackaged_falls_back_to_the_archive_for_this_machine() {
        let archive = format!("tobii-linux-0.3.0-{TRIPLE}.tar.gz");

        let all = packaged();
        let o = all.offer_for("nix", TRIPLE).expect("the archive");
        assert_eq!(o.channel, Channel::Archive);
        assert_eq!(names(&o), [archive.as_str()]);

        let tarball_only = full("v0.3.0");
        for manager in ["dpkg", "rpm", "pacman", ""] {
            let o = tarball_only
                .offer_for(manager, TRIPLE)
                .unwrap_or_else(|| panic!("{manager} should still get the archive"));
            assert_eq!(o.channel, Channel::Archive);
            assert_eq!(names(&o), [archive.as_str()]);
        }

        // Nothing for this machine at all: no offer, rather than a file from
        // somebody else's architecture.
        assert_eq!(all.offer_for("dpkg", "mips-unknown-none"), None);
    }

    #[test]
    fn garbage_is_an_error_rather_than_a_silent_no_update() {
        assert!(parse_releases("not json").is_err());
        assert!(parse_releases("").is_err());
    }
}
