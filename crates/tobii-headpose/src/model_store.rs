//! Where the head-pose model lives, and the terms under which a user may get it.
//!
//! # Why this is not just a download
//!
//! opentrack's tracker *code* is free. Its model **weights are not**: the
//! training sets behind them carry CC BY-NC 4.0 and "Research Use of Data"
//! terms (see `TERMS`). GPL-3 section 10 forbids us imposing further
//! restrictions on what we distribute, so this project cannot ship those
//! weights, vendor them, or quietly pull them in the background — any of which
//! would hand every downstream packager a licence problem they did not agree to.
//!
//! What it can do is show the terms, take an explicit decision, and then fetch
//! on the user's behalf. That is the whole reason this module exists rather than
//! a `build.rs`.
//!
//! # Why the transfer shells out
//!
//! `curl`/`wget` rather than an HTTP crate. Adding a TLS stack costs ~28 crates
//! for one transfer that happens at most once per install, and this workspace
//! already turned down a faster inference runtime (`ort`) specifically because
//! it dragged a 105 MB binary download into the build. Integrity does **not**
//! depend on the fetcher: every byte is checked against a pinned SHA-256 by our
//! own [`crate::sha256`] before anything is installed.

use std::path::{Path, PathBuf};

use crate::sha256;

/// A model file the user may choose to fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSource {
    pub name: &'static str,
    pub file: &'static str,
    /// **Pinned to a commit, never a branch.** opentrack replaces these files in
    /// place — the current one arrived in a commit titled "Update models" — and a
    /// `master` URL would hand us a different model the next time that happens.
    /// A different model is not an upgrade: its pose conventions, its pitch zero
    /// and its confidence scale are all re-measured per model, and this driver's
    /// constants are calibrated against *this* one.
    pub url: &'static str,
    /// The opentrack commit `url` points into, and the path within that repo.
    /// Used by [`check_update`] to ask whether anything newer exists.
    pub commit: &'static str,
    pub repo_path: &'static str,
    /// Pinned digest. Verified 2026-08-26 against a fresh download, by both
    /// `sha256sum` and [`crate::sha256`].
    pub sha256: &'static str,
    pub bytes: u64,
}

/// opentrack's head-pose regressor: 129x129 single-channel input, quaternion out.
pub const HEAD_POSE: ModelSource = ModelSource {
    name: "opentrack head-pose (small)",
    file: "head-pose-0.5-small.onnx",
    url: "https://raw.githubusercontent.com/opentrack/opentrack/03a0e69b02a11c425e2f07c728686ff7f2d6517a/tracker-neuralnet/models/head-pose-0.5-small.onnx",
    commit: "03a0e69b02a11c425e2f07c728686ff7f2d6517a",
    repo_path: "tracker-neuralnet/models/head-pose-0.5-small.onnx",
    sha256: "7c14f84114fb9eca89759d8a36350c6faae2b4187258cae07afb77a93c2d7eec",
    bytes: 12_919_981,
};

/// opentrack's face localizer. Optional: with both eyes tracked the region of
/// interest comes from the gaze frame's own metric eye origins, which needs no
/// model at all. Only useful for the eyes-not-tracked case.
pub const HEAD_LOCALIZER: ModelSource = ModelSource {
    name: "opentrack head localizer",
    file: "head-localizer.onnx",
    url: "https://raw.githubusercontent.com/opentrack/opentrack/03a0e69b02a11c425e2f07c728686ff7f2d6517a/tracker-neuralnet/models/head-localizer.onnx",
    commit: "03a0e69b02a11c425e2f07c728686ff7f2d6517a",
    repo_path: "tracker-neuralnet/models/head-localizer.onnx",
    sha256: "f26679fe5e01a08dab0b3b9b586b613c68622775e0b8e14ddae53f30391f7402",
    bytes: 279_403,
};

/// Shown before any download, and required to be acknowledged.
pub const TERMS: &str = "\
This model is not part of this program. It belongs to the opentrack project and \
is downloaded from opentrack's repository only if you ask for it here.

The weights are non-commercial-use-only. opentrack's tracker code is free \
software, but its models were trained on data that is not: sets under CC BY-NC \
4.0, Microsoft's \"Research Use of Data\" terms, and a non-commercial face model.

This program is GPL-3.0-only, so it cannot pass that restriction on to you \
by shipping the file. Downloading it is your decision, under opentrack's terms:

  https://github.com/opentrack/neuralnet-tracker-traincode/blob/master/license.md

Decline and head tracking still works. You lose only the up-and-down angle.";

/// Where fetched models are kept: beside the rest of this app's configuration.
pub fn model_dir() -> PathBuf {
    tobii_config::config_path()
        .parent()
        .map(|d| d.join("models"))
        .unwrap_or_else(|| PathBuf::from("models"))
}

pub fn path_of(src: &ModelSource) -> PathBuf {
    model_dir().join(src.file)
}

/// What is on disk for a given model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Missing,
    Ready,
    /// Present but not the expected bytes. Never silently used: a model that is
    /// not the one whose behaviour was characterised is worse than none, because
    /// its output looks plausible.
    Corrupt {
        found: String,
    },
}

pub fn status(src: &ModelSource) -> Status {
    let path = path_of(src);
    match std::fs::read(&path) {
        Err(_) => Status::Missing,
        Ok(bytes) => {
            let found = sha256::hex_digest(&bytes);
            if found == src.sha256 {
                Status::Ready
            } else {
                Status::Corrupt { found }
            }
        }
    }
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    /// The bytes are not the pinned model.
    Digest {
        expected: String,
        found: String,
    },
    /// Neither `curl` nor `wget` is installed.
    NoFetcher,
    /// The fetcher ran and failed.
    Fetch(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "{e}"),
            StoreError::Digest { expected, found } => write!(
                f,
                "downloaded file does not match the expected model\n  expected {expected}\n  got      {found}\n\
                 Either the download was corrupted, or upstream replaced the file — in which case the \
                 pinned digest in model_store.rs needs updating deliberately, not worked around."
            ),
            StoreError::NoFetcher => write!(
                f,
                "neither curl nor wget was found. Download the file yourself and install it with \
                 `tobii headpose --install-model <path>`"
            ),
            StoreError::Fetch(e) => write!(f, "download failed: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}

/// Verify `bytes` against the pinned digest and install them.
///
/// Written to a temporary beside the target and renamed, so an interrupted
/// install can never leave a half-written model that `status` would then have to
/// call corrupt.
pub fn install(src: &ModelSource, bytes: &[u8]) -> Result<PathBuf, StoreError> {
    let found = sha256::hex_digest(bytes);
    if found != src.sha256 {
        return Err(StoreError::Digest {
            expected: src.sha256.to_string(),
            found,
        });
    }
    let dir = model_dir();
    std::fs::create_dir_all(&dir)?;
    let final_path = dir.join(src.file);
    let tmp = dir.join(format!("{}.partial", src.file));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &final_path)?;
    Ok(final_path)
}

/// Install a file the user downloaded themselves.
pub fn install_from_file(src: &ModelSource, path: &Path) -> Result<PathBuf, StoreError> {
    install(src, &std::fs::read(path)?)
}

/// Fetch to `dest` with `curl`, falling back to `wget`.
///
/// Separate from [`install`] so a caller can watch `dest` grow for progress
/// without this module having to model progress reporting.
pub fn download_to(src: &ModelSource, dest: &Path) -> Result<(), StoreError> {
    let attempts: [(&str, Vec<String>); 2] = [
        (
            "curl",
            vec![
                "-L".into(),
                "--fail".into(),
                "--silent".into(),
                "--show-error".into(),
                "-o".into(),
                dest.display().to_string(),
                src.url.into(),
            ],
        ),
        (
            "wget",
            vec![
                "-q".into(),
                "-O".into(),
                dest.display().to_string(),
                src.url.into(),
            ],
        ),
    ];
    let mut found_any = false;
    let mut last = String::new();
    for (prog, args) in attempts {
        match std::process::Command::new(prog).args(&args).output() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                found_any = true;
                last = e.to_string();
            }
            Ok(out) => {
                found_any = true;
                if out.status.success() {
                    return Ok(());
                }
                last = String::from_utf8_lossy(&out.stderr).trim().to_string();
            }
        }
    }
    if !found_any {
        return Err(StoreError::NoFetcher);
    }
    let _ = std::fs::remove_file(dest);
    Err(StoreError::Fetch(last))
}

/// The [`ModelConfig`] for an installed, verified model, or `None`.
///
/// The one place a front end should ask "can I run the neural path?", so that
/// the CLI and the GUI cannot drift on what counts as installed. A file that is
/// present but has the wrong digest reads as `None`: a model whose behaviour was
/// never characterised is worse than no model, because its output looks
/// plausible.
pub fn installed(src: &ModelSource) -> Option<crate::model::ModelConfig> {
    match status(src) {
        Status::Ready => Some(crate::model::ModelConfig {
            kind: crate::model::ModelKind::OpentrackOnnx,
            model_path: path_of(src),
        }),
        _ => None,
    }
}

/// Where [`fetch`] writes while the download is in flight.
///
/// Public so a GUI can size its progress against [`ModelSource::bytes`] by
/// stat-ing this path, rather than re-deriving the name and drifting from it.
pub fn download_path(src: &ModelSource) -> PathBuf {
    model_dir().join(format!("{}.download", src.file))
}

/// The saved head-pose pitch zero, in degrees, if it has been measured.
///
/// Lives here rather than in the caller so the CLI and the GUI cannot disagree
/// about where it is kept.
pub fn pitch_offset() -> Option<f64> {
    tobii_config::load_pitch_offset().ok().flatten()
}

/// What upstream has, relative to the commit this build is pinned to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// Nothing newer has touched the file.
    UpToDate,
    /// A newer commit changed it. Deliberately NOT actionable from here.
    Newer { sha: String, date: String },
    /// The question could not be answered (offline, rate-limited, API change).
    Unknown(String),
}

/// Ask GitHub whether a newer commit has touched the model file.
///
/// This is a *question*, not an upgrade path, and that is on purpose. A new
/// model is a different model: its rotation conventions, its absolute pitch zero
/// and the scale of its confidence output are all properties this driver has
/// measured against the pinned one. Pulling a replacement in automatically would
/// silently invalidate every one of those constants, and the symptom would be a
/// head pose that looks plausible and is wrong. So when this reports something
/// newer, the response is to re-measure and ship a new pin — not to re-download.
///
/// Never called on its own; it is a network request, and this program does not
/// make those unasked.
pub fn check_update(src: &ModelSource) -> Update {
    let url = format!(
        "https://api.github.com/repos/opentrack/opentrack/commits?path={}&per_page=1",
        src.repo_path
    );
    let out = match std::process::Command::new("curl")
        .args([
            "-sL",
            "--fail",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: tobii-linux",
            &url,
        ])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        Ok(o) => {
            return Update::Unknown(format!(
                "GitHub request failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ))
        }
        Err(e) => return Update::Unknown(format!("could not run curl: {e}")),
    };
    let body = String::from_utf8_lossy(&out);
    match (
        json_first_string(&body, "sha"),
        json_first_string(&body, "date"),
    ) {
        (Some(sha), date) => {
            if sha == src.commit {
                Update::UpToDate
            } else {
                Update::Newer {
                    sha,
                    date: date.unwrap_or_default(),
                }
            }
        }
        _ => Update::Unknown("could not read a commit sha from the reply".into()),
    }
}

/// The first `"key": "value"` string in a JSON document.
///
/// A whole JSON parser for two fields would be a dependency; this is enough for
/// a reply whose shape is fixed and whose failure mode is `Unknown`, which is
/// already a handled outcome.
fn json_first_string(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let rest = &body[body.find(&needle)? + needle.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Download, verify, install. The caller must have obtained consent first —
/// this deliberately takes no "yes" flag, so that decision lives in the UI that
/// showed [`TERMS`] rather than being defaulted here.
pub fn fetch(src: &ModelSource) -> Result<PathBuf, StoreError> {
    let dir = model_dir();
    std::fs::create_dir_all(&dir)?;
    let tmp = download_path(src);
    download_to(src, &tmp)?;
    let bytes = std::fs::read(&tmp)?;
    let _ = std::fs::remove_file(&tmp);
    install(src, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_file_is_refused_and_nothing_is_written() {
        let path = path_of(&HEAD_POSE);
        let before = path.exists();
        let err = install(&HEAD_POSE, b"not a model").unwrap_err();
        assert!(matches!(err, StoreError::Digest { .. }), "{err}");
        assert_eq!(
            path.exists(),
            before,
            "a rejected install must not touch disk"
        );
    }

    #[test]
    fn the_digest_error_shows_both_hashes() {
        // A mismatch is either corruption or upstream moving the file, and the
        // two need different responses — so the message has to carry enough to
        // tell them apart rather than just saying "failed".
        let e = install(&HEAD_POSE, b"x").unwrap_err().to_string();
        assert!(e.contains(HEAD_POSE.sha256), "{e}");
        assert!(e.contains(&sha256::hex_digest(b"x")), "{e}");
    }

    #[test]
    fn the_url_is_pinned_to_a_commit_not_a_branch() {
        // A branch URL would silently hand us a different model the next time
        // opentrack updates theirs — which they do; the current file arrived in
        // a commit titled "Update models".
        for src in [&HEAD_POSE, &HEAD_LOCALIZER] {
            assert!(
                src.url.contains(src.commit),
                "{} must be fetched from its pinned commit: {}",
                src.name,
                src.url
            );
            assert!(
                !src.url.contains("/master/"),
                "{} still points at a branch",
                src.name
            );
            assert_eq!(
                src.commit.len(),
                40,
                "{} commit is not a full sha",
                src.name
            );
            assert!(
                src.url.ends_with(src.repo_path),
                "{} url/path disagree",
                src.name
            );
        }
    }

    #[test]
    fn a_commit_reply_is_read_without_a_json_dependency() {
        let body = r#"[{"sha":"abc123","commit":{"committer":{"date":"2026-05-26T18:51:01Z"}}}]"#;
        assert_eq!(json_first_string(body, "sha").as_deref(), Some("abc123"));
        assert_eq!(
            json_first_string(body, "date").as_deref(),
            Some("2026-05-26T18:51:01Z")
        );
        assert_eq!(json_first_string(body, "nope"), None);
        // Garbage must be None, never a panic or a wrong answer.
        assert_eq!(json_first_string("", "sha"), None);
        assert_eq!(json_first_string(r#"{"sha"}"#, "sha"), None);
        assert_eq!(json_first_string(r#"{"sha": 12}"#, "sha"), None);
    }

    #[test]
    fn terms_fit_a_dialog_and_still_say_the_three_things_that_matter() {
        // Read on screen, not in a terminal: no paragraph may be a wall.
        // Currently 700. The bound is a budget, not a measurement: consent
        // text long enough to scroll past is consent text nobody reads, and
        // this has already been rewritten once for being a wall.
        assert!(
            TERMS.len() < 900,
            "the terms are {} chars; they are meant to be read, not skipped",
            TERMS.len()
        );
        assert!(TERMS.contains("CC BY-NC"), "the restriction");
        assert!(TERMS.contains("GPL-3.0-only"), "why we cannot ship it");
        assert!(
            TERMS.contains("Decline and head tracking still works"),
            "the way out"
        );
    }

    #[test]
    fn terms_name_the_restriction_and_the_way_out() {
        assert!(TERMS.contains("CC BY-NC"));
        assert!(TERMS.contains("GPL-3.0-only"));
        // A user who declines must be told what still works, not just refused.
        assert!(TERMS.contains("still works"));
    }

    #[test]
    fn models_live_beside_the_rest_of_the_config() {
        let d = model_dir();
        assert!(d.ends_with("models"), "{}", d.display());
        assert_eq!(
            d.parent(),
            tobii_config::config_path().parent(),
            "models must not land somewhere unrelated to the app's own config"
        );
    }
}
