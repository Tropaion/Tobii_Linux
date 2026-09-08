//! Checking for, and installing, a newer release of this program.
//!
//! # What this does and does not do
//!
//! It asks GitHub for the project's latest release, compares its tag with the
//! running build's version, and — **only when the user asks** — downloads that
//! release's Linux archive, verifies it against the checksums published beside
//! it, and swaps the binaries in place.
//!
//! Nothing is downloaded by the check itself, and nothing is ever installed
//! without an explicit request. That is the same rule the head-pose model store
//! follows, for the same reason: this program does not make network requests or
//! change files on disk as a side effect of being started.
//!
//! # Release layout this expects
//!
//! A release must carry an archive whose name contains the target triple, and a
//! `SHA256SUMS` asset listing it:
//!
//! ```text
//! tobii-linux-0.2.0-x86_64-unknown-linux-gnu.tar.gz
//! SHA256SUMS
//! ```
//!
//! The archive holds the binaries at its top level or one directory down. See
//! [`install`].

pub mod install;
pub mod json;
pub mod release;
pub mod version;

pub use install::{install_release, InstallError, Installed, Target};
pub use release::{check, Asset, CheckError, Release};
pub use version::Version;

/// Where releases are published.
///
/// A constant rather than something read from the crate metadata: the updater
/// replaces the binaries on a user's machine, and the place it fetches them
/// from should be visible in the source, not assembled at runtime from a field
/// that happens to be editable.
pub const REPO_OWNER: &str = "Tropaion";
pub const REPO_NAME: &str = "Tobii_Linux";

/// The project's releases page, for a user who would rather do it by hand.
pub fn releases_url() -> String {
    format!("https://github.com/{REPO_OWNER}/{REPO_NAME}/releases")
}
