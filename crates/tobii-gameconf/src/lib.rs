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
//! source: it reads every line of this crate that ships, the modules and the
//! example alike, and fails on the name of any call that could modify a file.
//! Reading for names is only worth anything because of two other facts, which
//! the tests beside it assert rather than assume: this crate has no
//! dependencies, so `std` is the whole of what it can call, and it has no
//! `unsafe`, so it cannot reach past `std` under a name that is in no Rust
//! source at all.
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
//! registry. Both formats carry a schema version on their document element,
//! and what this project has seen of either is one number, counted off the
//! files on one machine and written down in the module that counted it — see
//! [`binds`] and [`attrs`]. Nobody here has watched either number move. That
//! is exactly why this crate does not check them against a list it believes
//! in: it reports them, so a reader of the report can see what it was looking
//! at. What it does refuse is anything whose *shape* it cannot read exactly,
//! and a schema this reader has never met is most likely to arrive as exactly
//! that.
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
/// options. It is separated from [`Self::Rejected`] because it is the one
/// negative answer with a confident sentence behind it — *nothing has written
/// this yet* — while a directory that exists and cannot be read means *go look
/// yourself*. A caller that folded them together would tell a user who has
/// never launched the game that something is wrong with their installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source<T> {
    /// Nothing has written this yet.
    ///
    /// Usually there is no such file or directory. It is also the answer for a
    /// directory that is there and holds nothing that names a saved
    /// configuration — [`binds::read`] gives it for a preset directory with no
    /// `StartPreset` file in it, however many `.binds` documents are sitting
    /// next to the missing one, because a game ships those and a game that has
    /// run writes the other. The string says which of the two it was, and it
    /// is the sentence a report should print: the cases share an answer, not a
    /// wording.
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

    /// Every line of this crate that ships, with the tests cut off.
    ///
    /// Cut at the first `#[cfg(test)]`, because the tests themselves write —
    /// they build fixture directories, and they are the only code here that is
    /// allowed to.
    ///
    /// The example is in here with the modules. It is code this crate ships,
    /// it is the thing a maintainer points at real files to check something,
    /// and a guarantee that covered the library but not the program that
    /// exercises it would be a guarantee about the half nobody runs by hand.
    fn shipped_source() -> Vec<(&'static str, &'static str)> {
        [
            ("lib.rs", include_str!("lib.rs")),
            ("xml.rs", include_str!("xml.rs")),
            ("binds.rs", include_str!("binds.rs")),
            ("attrs.rs", include_str!("attrs.rs")),
            ("examples/read.rs", include_str!("../examples/read.rs")),
        ]
        .into_iter()
        .map(|(name, src)| (name, src.split("#[cfg(test)]").next().unwrap_or(src)))
        .collect()
    }

    /// Source with the comment lines dropped, so that a rule can be *written
    /// down* in the file it governs.
    ///
    /// `//` only: there are no block comments here, and a checker that
    /// pretended to understand Rust lexically would be a worse liar than one
    /// that admits what it scans.
    fn code_of(src: &str) -> String {
        src.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The promise the crate docs open with, held to the source rather than to
    /// a reviewer's memory.
    ///
    /// This is not a substitute for the measurement — a real prefix was
    /// snapshotted whole, path by path with sizes, nanosecond mtimes and
    /// content hashes, before and after a read, and came back identical — but
    /// a measurement proves one afternoon and this fails the build.
    ///
    /// Scanning for names is a blunt instrument, and blunt in a particular
    /// way: the check is exactly as complete as the list, and a list is a
    /// thing somebody wrote from memory. What makes it sound here is not the
    /// list, it is that the list can be *finished* —
    /// [`nothing_can_be_called_that_is_not_in_std`] holds the two facts that
    /// make it finite. `std` is the whole of what this crate can call, and
    /// `std`'s file-modifying surface is a page of documentation somebody can
    /// read to the end once and enumerate. Take those away and this becomes
    /// theatre; so they are asserted next to it rather than remembered.
    ///
    /// The list must therefore be written against the surface and not against
    /// the habits: three of the entries below are spellings that reach the
    /// filesystem while naming none of the obvious ones. `File::options` opens
    /// for writing without the word `OpenOptions` appearing, `DirBuilder`
    /// creates a directory without the word `create_dir`, and `soft_link`
    /// makes a symbolic link without the word `symlink`.
    #[test]
    fn never_writes() {
        // Every `std` entry point that can create, modify, remove or relink
        // something on disk, each spelled the shortest way that still cannot
        // match anything else. Sub-spellings are covered by their prefixes:
        // `File::create` catches `create_new`, `create_dir` catches
        // `create_dir_all`, `remove_dir` catches `remove_dir_all`, and `chown`
        // catches `lchown` and `fchown`.
        const FORBIDDEN: [&str; 21] = [
            "fs::write",
            "File::create",
            "File::options",
            "OpenOptions",
            "create_new",
            "create_dir",
            "DirBuilder",
            "remove_file",
            "remove_dir",
            "fs::copy",
            "fs::rename",
            "hard_link",
            "soft_link",
            "symlink",
            "set_permissions",
            "set_len",
            "set_modified",
            "set_times",
            "FileTimes",
            "chown",
            "Command",
        ];
        for (name, src) in shipped_source() {
            let code = code_of(src);
            for bad in FORBIDDEN {
                assert!(
                    !code.contains(bad),
                    "{name} names `{bad}`: this crate only ever reads"
                );
            }
        }
    }

    /// The preconditions that make [`never_writes`] a proof and not a gesture.
    ///
    /// Each check here closes a way for code to run in this crate under a
    /// name the scan has never read. A dependency brings its own vocabulary,
    /// and no list written against `std` can cover it. `unsafe` reaches libc
    /// directly, where the name on the call need not be a Rust name at all. A
    /// module or an example the scan does not open is simply a file nobody
    /// checked — which is why neither is listed twice: the modules are matched
    /// against what `lib.rs` declares, and the examples against the directory
    /// they live in.
    #[test]
    fn nothing_can_be_called_that_is_not_in_std() {
        let manifest = include_str!("../Cargo.toml");
        let mut under_dependencies = false;
        for line in manifest.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            if let Some(table) = line.strip_prefix('[') {
                under_dependencies = table.trim_end_matches(']').ends_with("dependencies");
                continue;
            }
            assert!(
                !under_dependencies || line.is_empty(),
                "a dependency — `{line}` — puts names under this crate that \
                 `never_writes` was never written against"
            );
        }
        let scanned: Vec<&str> = shipped_source().iter().map(|(name, _)| *name).collect();
        let (lib, _) = shipped_source()
            .into_iter()
            .find(|(name, _)| *name == "lib.rs")
            .expect("lib.rs");
        for line in code_of(lib).lines() {
            let line = line.trim();
            let Some(rest) = line
                .strip_prefix("pub mod ")
                .or_else(|| line.strip_prefix("pub(crate) mod "))
                .or_else(|| line.strip_prefix("mod "))
            else {
                continue;
            };
            let Some(module) = rest.strip_suffix(';') else {
                continue;
            };
            assert!(
                scanned.contains(&format!("{module}.rs").as_str()),
                "module {module} is declared here and not scanned by `never_writes`"
            );
        }
        // The same question of the examples, asked of the directory rather
        // than of a second list: a program that ships here and is not scanned
        // is a program nobody checked, and the way that happens is somebody
        // adding one.
        let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
        for entry in std::fs::read_dir(&examples).expect("the examples directory") {
            let path = entry.expect("an entry").path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).expect("a name");
            assert!(
                scanned.contains(&format!("examples/{name}").as_str()),
                "examples/{name} ships and is not scanned by `never_writes`"
            );
        }
        for (name, src) in shipped_source() {
            assert!(
                !code_of(src).contains("unsafe"),
                "{name} names `unsafe`: past `std` there is no name to scan for"
            );
        }
    }

    /// [`Source::Unwritten`] answers for more than a path that is not there,
    /// and the type is where a caller finds out which cases it covers.
    ///
    /// The sentence each case carries is right; a type whose doc names only
    /// one of them sends a reader to the wrong conclusion about the other.
    #[test]
    fn unwritten_names_every_case_that_answers_it() {
        let src = include_str!("lib.rs");
        let (before, _) = src
            .split_once("Unwritten(String),")
            .expect("the variant is declared");
        let doc: Vec<&str> = before
            .lines()
            .rev()
            .take_while(|l| {
                let l = l.trim_start();
                // The variant line itself is cut mid-way by the split, and
                // what is left of it is its indentation.
                l.starts_with("///") || l.is_empty()
            })
            .collect();
        let doc = doc.join("\n");
        assert!(
            doc.contains("StartPreset"),
            "`binds::read` answers Unwritten for a directory that is there and \
             holds no StartPreset file, and this doc does not say so: {doc}"
        );
    }

    /// The crate docs make the argument; the modules hold the measurements.
    ///
    /// Every number in this crate is a count of real files somebody opened,
    /// and it belongs beside the reader it was counted with — [`binds`] for
    /// Elite's presets, [`attrs`] for the one `attributes.xml` on this
    /// machine. Restated up here it becomes a second copy that no measurement
    /// keeps honest, and the cheapest rule that holds the line is that the
    /// docs in this file carry no digits at all.
    #[test]
    fn the_crate_docs_count_nothing_themselves() {
        for line in include_str!("lib.rs").lines() {
            let doc = line.trim_start();
            if !doc.starts_with("//!") && !doc.starts_with("///") {
                continue;
            }
            assert!(
                !doc.contains(|c: char| c.is_ascii_digit()),
                "a measurement in the crate docs, where nothing is measured: {doc}"
            );
        }
    }
}
