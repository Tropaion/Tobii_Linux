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
        let (nums, pre) = match t.split_once(['-', '+']) {
            Some((n, p)) => (n, p.to_string()),
            None => (t, String::new()),
        };
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
            _ => self.pre > other.pre,
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

    #[test]
    fn this_build_reports_a_version() {
        let c = Version::current();
        assert!(
            c.major > 0 || c.minor > 0 || c.patch > 0,
            "0.0.0 means the crate version was not picked up"
        );
    }
}
