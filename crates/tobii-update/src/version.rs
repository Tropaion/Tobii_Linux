//! Release versions, and the one comparison that matters: is theirs newer?

/// A `major.minor.patch` version, with an optional pre-release tail.
///
/// Deliberately not a full semver implementation. The only question asked of it
/// is whether a published release is newer than the running build, and the only
/// versions it will ever see are this project's own tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// Everything after a `-`, e.g. `rc1`. Empty for a normal release.
    pub pre: String,
}

impl Version {
    /// Parse `1.2.3`, `v1.2.3`, or `v1.2.3-rc1`. A missing minor or patch reads
    /// as zero, so a `v1` tag still compares sensibly.
    pub fn parse(text: &str) -> Option<Version> {
        let t = text.trim();
        let t = t
            .strip_prefix('v')
            .or_else(|| t.strip_prefix('V'))
            .unwrap_or(t);
        // Build metadata (`+abc`) is not part of precedence, so it is dropped
        // rather than folded into the pre-release tail: `1.0.0+a` and
        // `1.0.0+b` are the same release, and neither is a pre-release.
        let t = t.split_once('+').map_or(t, |(v, _)| v);
        let (nums, pre) = match t.split_once('-') {
            Some((n, p)) => (n, p.to_string()),
            None => (t, String::new()),
        };
        if nums.is_empty() {
            return None;
        }
        let mut it = nums.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next().map_or(Some(0), |s| s.parse().ok())?;
        let patch = it.next().map_or(Some(0), |s| s.parse().ok())?;
        if it.next().is_some() {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }

    /// This build's version, from Cargo.
    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("our own version parses")
    }

    /// Whether `self` is a release the user does not have yet.
    ///
    /// A pre-release loses to the same numbers without one, which is the semver
    /// rule and the one that matters here: `1.2.0-rc1` must not be offered to
    /// somebody already running `1.2.0`.
    pub fn is_newer_than(&self, other: &Version) -> bool {
        let mine = (self.major, self.minor, self.patch);
        let theirs = (other.major, other.minor, other.patch);
        if mine != theirs {
            return mine > theirs;
        }
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, false) => true,  // release beats pre-release
            (false, true) => false, // pre-release never beats a release
            _ => cmp_pre(&self.pre, &other.pre) == std::cmp::Ordering::Greater,
        }
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre)?;
        }
        Ok(())
    }
}

/// Compare two pre-release tails the way semver says to.
///
/// A plain string compare gets this wrong the moment a series reaches ten:
/// `"rc10" < "rc9"`, so `1.0.0-rc10` would never be offered to somebody running
/// `1.0.0-rc9`. The rule is to compare dot-separated identifiers one at a time,
/// numerically where both are numeric, and to treat a numeric identifier as
/// lower than an alphanumeric one. `rc.9` vs `rc.10` then orders correctly; so
/// does the `rc9`/`rc10` spelling this project actually uses, because the
/// trailing digits are split off and compared as numbers.
fn cmp_pre(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            // Fewer identifiers wins, when everything before them was equal.
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => match cmp_ident(x, y) {
                std::cmp::Ordering::Equal => continue,
                other => return other,
            },
        }
    }
}

/// Compare one identifier, splitting a trailing number off an alphabetic stem.
///
/// Semver proper would compare `rc9` and `rc10` as opaque strings, since
/// neither is wholly numeric. This project spells its pre-releases that way
/// though, so the stem is compared as text and the trailing digits as a number
/// — which agrees with semver wherever semver has an opinion and gets `rc10`
/// right where semver would not.
fn cmp_ident(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |s: &str| {
        let stem = s.trim_end_matches(|c: char| c.is_ascii_digit());
        let digits = &s[stem.len()..];
        (stem.to_string(), digits.parse::<u64>().ok())
    };
    let (a_stem, a_num) = split(a);
    let (b_stem, b_num) = split(b);
    if a_stem != b_stem {
        // A wholly numeric identifier ranks below an alphanumeric one.
        return match (a_stem.is_empty(), b_stem.is_empty()) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a_stem.cmp(&b_stem),
        };
    }
    match (a_num, b_num) {
        (Some(x), Some(y)) => x.cmp(&y),
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s} should parse"))
    }

    #[test]
    fn tags_parse_with_or_without_the_v() {
        assert_eq!(v("1.2.3"), v("v1.2.3"));
        assert_eq!(v("v0.1.0").to_string(), "0.1.0");
        assert_eq!(v("v2").to_string(), "2.0.0", "a short tag still compares");
        assert_eq!(v("v1.4").to_string(), "1.4.0");
        assert_eq!(v("v1.2.3-rc1").pre, "rc1");
    }

    #[test]
    fn nonsense_does_not_parse_into_a_version() {
        for bad in ["", "v", "latest", "1.2.3.4", "1.x.0", "-1.0.0", "main"] {
            assert!(Version::parse(bad).is_none(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn newer_is_ordered_by_number_not_by_string() {
        // The trap a string compare falls into: "10" sorts before "9".
        assert!(v("0.10.0").is_newer_than(&v("0.9.0")));
        assert!(v("1.0.0").is_newer_than(&v("0.99.99")));
        assert!(v("0.1.10").is_newer_than(&v("0.1.9")));
        assert!(!v("0.1.0").is_newer_than(&v("0.1.0")), "equal is not newer");
        assert!(!v("0.1.0").is_newer_than(&v("0.2.0")));
    }

    /// Nobody running 1.2.0 should be offered 1.2.0-rc1 as an update.
    #[test]
    fn a_prerelease_never_beats_the_release_it_precedes() {
        assert!(!v("1.2.0-rc1").is_newer_than(&v("1.2.0")));
        assert!(v("1.2.0").is_newer_than(&v("1.2.0-rc1")));
        assert!(v("1.2.0-rc2").is_newer_than(&v("1.2.0-rc1")));
        assert!(v("1.2.1-rc1").is_newer_than(&v("1.2.0")));
    }

    /// The bug a string compare hides until the tenth candidate: `"rc10"`
    /// sorts before `"rc9"`, so the release everyone was waiting for would
    /// never be offered to anyone running rc9.
    #[test]
    fn prerelease_numbers_compare_as_numbers() {
        assert!(v("1.0.0-rc10").is_newer_than(&v("1.0.0-rc9")));
        assert!(!v("1.0.0-rc9").is_newer_than(&v("1.0.0-rc10")));
        assert!(v("1.0.0-rc.10").is_newer_than(&v("1.0.0-rc.9")));
        assert!(v("1.0.0-beta2").is_newer_than(&v("1.0.0-beta1")));
        // Different stems still compare as text, alpha before beta before rc.
        assert!(v("1.0.0-beta1").is_newer_than(&v("1.0.0-alpha9")));
        assert!(v("1.0.0-rc1").is_newer_than(&v("1.0.0-beta9")));
        assert!(!v("1.0.0-rc1").is_newer_than(&v("1.0.0-rc1")));
    }

    /// Semver: a numeric identifier ranks below an alphanumeric one, and a
    /// shorter run of identifiers below a longer one that starts the same.
    #[test]
    fn prerelease_ordering_follows_semver_where_semver_has_an_opinion() {
        assert!(v("1.0.0-alpha").is_newer_than(&v("1.0.0-1")));
        assert!(v("1.0.0-alpha.1").is_newer_than(&v("1.0.0-alpha")));
        assert!(!v("1.0.0-alpha").is_newer_than(&v("1.0.0-alpha.1")));
    }

    /// Build metadata is not a pre-release and does not affect precedence.
    #[test]
    fn build_metadata_is_ignored_rather_than_read_as_a_prerelease() {
        assert_eq!(v("1.2.3+build7").pre, "", "+ is metadata, not a tail");
        assert!(!v("1.2.3+a").is_newer_than(&v("1.2.3+b")));
        assert!(!v("1.2.3").is_newer_than(&v("1.2.3+b")));
        assert!(v("1.2.3+a").is_newer_than(&v("1.2.3-rc1")));
    }

    #[test]
    fn this_build_reports_a_version() {
        let c = Version::current();
        assert!(
            c.major > 0 || c.minor > 0 || c.patch > 0,
            "0.0.0 means the crate version was not picked up"
        );
    }
}
