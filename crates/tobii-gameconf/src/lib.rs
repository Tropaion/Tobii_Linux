//! Reading a game's own configuration files, to report what the user still
//! has to change by hand.
//!
//! # This never writes, and that is a design decision, not an omission
//!
//! There are three places a game needs configuring for a head tracker: this
//! program's own settings, the wine-side bridge, and the game's own options.
//! The first two this program owns and changes. The third it only reads.
//!
//! A game's bindings file is the user's work — hours of it, in Elite
//! Dangerous' case — in a format its author can change in any patch. A wrong
//! write costs somebody their bindings; a wrong read costs a confusing
//! sentence. So nothing in this crate creates, opens for writing, renames or
//! deletes anything, and `never_writes` in the tests below holds that to the
//! source: it reads this crate's own non-test code and fails on the name of
//! any call that could modify a file.
//!
//! # A file that cannot be read is never a file that says nothing
//!
//! The rule is the one `tobii-cli`'s `user.reg` reader follows, and it is here
//! for the same reason: the two negative answers are not interchangeable. "The setting
//! is not in this file" and "this file is not one I can read" lead to
//! different sentences — the first says *the user has not set this*, and the
//! second says *go look yourself* — and only the second is safe to be wrong
//! about. So [`Lookup`] has three cases, and everything that is not decoded to
//! the last character comes back [`Lookup::Rejected`].
//!
//! Schema drift is the reason this matters more here than it does for a
//! registry. Elite's bindings root carries a major and a minor version that
//! have moved 3.0 → 4.0 → 4.1 in the lifetime of the game, and Star Citizen's
//! `attributes.xml` says `Version="35"` today. This crate does not check those
//! numbers against a list it believes in — it reports them, so that a reader
//! of the report can see what it was looking at — but it does refuse anything
//! whose *shape* it cannot read exactly, and a version it has never seen is
//! most likely to show up as exactly that.
//!
//! # It knows two file formats and no games
//!
//! There is no appid here, no game name, and no path to anybody's options
//! directory. [`binds`] reads a directory of Elite Dangerous-style preset
//! documents; [`attrs`] reads a flat `<Attr name= value=/>` list. Which
//! directory, and which setting inside it, is the caller's business — that
//! knowledge belongs in a per-game profile, and this project ships none.
//!
//! # Why there is no XML dependency
//!
//! Both formats are `<Element attribute="value"/>`, one level deep, read for
//! one attribute. What a real parser would add over [`xml`] here is namespaces,
//! DTDs, entity declarations and mixed content — none of which appears in
//! either file — and what it would cost is this project's first external
//! dependency, in the crate whose whole promise is that it cannot damage
//! anything. The scanner refuses what it does not understand (see [`xml`]),
//! which is the property that makes a small reader safe rather than merely
//! short. If either format ever grows something that needs a real parser, the
//! refusal is what will say so, out loud, instead of a wrong value.

use std::path::Path;

pub mod attrs;
pub mod binds;
pub(crate) mod xml;

/// What one setting in one file turned out to hold.
///
/// Three cases, for the reason the crate docs give: "not set" and "there and
/// unreadable" are different sentences in a report and only one of them is
/// safe to be wrong about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// The file was read, and this setting is not in it.
    Absent,
    /// The value, decoded to the last character.
    ///
    /// Possibly empty: an attribute that is present and holds nothing is a
    /// fact about the file, and turning it into an absence would be the
    /// conflation this whole type exists to prevent.
    Text(String),
    /// Something is there that this reader cannot read back exactly, with the
    /// one thing that can honestly be said about it.
    Rejected(String),
}

/// What a file — or a directory of them — turned out to be, before any
/// question about its contents.
///
/// [`Self::Unwritten`] is the answer for a game that has never saved its
/// options: the directory or the file is simply not there. It is separated
/// from [`Self::Rejected`] because it is the one negative answer with a
/// confident sentence behind it — *nothing has written this yet* — while a
/// directory that exists and cannot be read means *go look yourself*. A caller
/// that folded them together would tell a user who has never launched the game
/// that something is wrong with their installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source<T> {
    /// Nothing is there at all: no such file or directory.
    Unwritten(String),
    /// Something is there that cannot be read back exactly.
    Rejected(String),
    /// Read, for whatever it says.
    Read(T),
}

/// Read a whole file, separating "not there" from "there and unreadable".
///
/// [`std::io::ErrorKind::NotFound`] is the only error that becomes
/// [`Source::Unwritten`]. Everything else — a permission error, a directory
/// where a file was expected, an I/O error off a failing disk — is a file this
/// program could not read, which is not the same as a file that is not there,
/// and a Proton prefix on a drive that is not mounted produces exactly the
/// second while looking like the first to a careless reader.
pub(crate) fn read(path: &Path) -> Source<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Source::Read(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Source::Unwritten(format!("there is no file at {}", path.display()))
        }
        Err(e) => Source::Rejected(format!("{} could not be read: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A throwaway directory that is this process's alone.
    ///
    /// No `$HOME` is read and no fixed path is used: CI runs these as root,
    /// and a test that reached for the real home would find a different one
    /// there — and this crate's whole subject is other people's files.
    pub(crate) fn scratch(what: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "tobii-gameconf-{}-{}-{what}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("scratch");
        p
    }

    /// The crate's own source, with the tests cut off.
    ///
    /// Cut at the first `#[cfg(test)]`, because the tests themselves write —
    /// they build fixture directories, and they are the only code here that is
    /// allowed to.
    fn shipped_source() -> Vec<(&'static str, &'static str)> {
        [
            ("lib.rs", include_str!("lib.rs")),
            ("xml.rs", include_str!("xml.rs")),
            ("binds.rs", include_str!("binds.rs")),
            ("attrs.rs", include_str!("attrs.rs")),
        ]
        .into_iter()
        .map(|(name, src)| (name, src.split("#[cfg(test)]").next().unwrap_or(src)))
        .collect()
    }

    /// The promise the crate docs open with, held to the source rather than to
    /// a reviewer's memory.
    ///
    /// This is not a substitute for the measurement — a real prefix was
    /// snapshotted whole, path by path with sizes, nanosecond mtimes and
    /// content hashes, before and after a read, and came back identical — but
    /// a measurement proves one afternoon and this fails the build.
    ///
    /// Comment lines are dropped first, so that the rule can be *written down*
    /// in the file it governs. `//` only: there are no block comments here,
    /// and a checker that pretended to understand Rust lexically would be a
    /// worse liar than one that admits what it scans.
    #[test]
    fn never_writes() {
        // Every std entry point that can modify a file or a directory. A name
        // is enough: this crate has no `unsafe`, no `std::process`, and no
        // dependency that could hide one behind another spelling — which is
        // itself part of why it has no dependencies.
        const FORBIDDEN: [&str; 14] = [
            "fs::write",
            "File::create",
            "OpenOptions",
            "create_dir",
            "create_new",
            "remove_file",
            "remove_dir",
            "fs::copy",
            "fs::rename",
            "set_permissions",
            "set_len",
            "symlink",
            "hard_link",
            "Command",
        ];
        for (name, src) in shipped_source() {
            let code: String = src
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for bad in FORBIDDEN {
                assert!(
                    !code.contains(bad),
                    "{name} names `{bad}`: this crate only ever reads"
                );
            }
        }
    }
}
