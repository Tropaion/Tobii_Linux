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
//! deletes anything.
//!
//! Two measurements are what that promise rests on, and they are different in
//! kind from the test that guards it. A real Proton prefix was snapshotted
//! whole — every path under it, with size, nanosecond mtime and content hash
//! — before and after this crate read it, and came back identical. Then this
//! crate's readers were run again under a shim that aborts the process on any
//! of eighteen mutating syscalls, and not one of them fired. Those two say
//! the code as it stands does not write.
//!
//! `never_writes` in the tests below is a smaller thing: a tripwire, not a
//! proof. It reads the crate's shipping source and fails the build on the
//! name of a call that could modify a file, so that the day somebody adds one
//! the build says so instead of a maintainer remembering to re-measure. It
//! leans on two facts the test beside it asserts rather than assumes — no
//! dependencies, so `std` is the whole of what this crate can call, and no
//! `unsafe`, so it cannot reach past `std` under a name that is in no Rust
//! source at all — but those bound its vocabulary without completing its
//! list. What a list of spellings cannot catch is said plainly where the list
//! is.
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
    use std::path::{Path, PathBuf};
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

    /// Every file this crate ships, as the scanners below read it.
    ///
    /// The example is in here with the modules. It is code this crate ships,
    /// it is the thing a maintainer points at real files to check something,
    /// and a guarantee that covered the library but not the program that
    /// exercises it would be a guarantee about the half nobody runs by hand.
    ///
    /// Whether this is all of them is not taken on trust:
    /// [`nothing_can_be_called_that_is_not_in_std`] walks the directories
    /// these files live in and fails on any `.rs` file that is not named
    /// here.
    fn shipped_source() -> Vec<(&'static str, String)> {
        [
            ("lib.rs", include_str!("lib.rs")),
            ("xml.rs", include_str!("xml.rs")),
            ("binds.rs", include_str!("binds.rs")),
            ("attrs.rs", include_str!("attrs.rs")),
            ("examples/read.rs", include_str!("../examples/read.rs")),
        ]
        .into_iter()
        .map(|(name, src)| (name, shipped_code(src)))
        .collect()
    }

    /// One file's shipping code: the comments taken out first, then
    /// everything from the first `#[cfg(test)]` onwards.
    ///
    /// The tests are cut off because they themselves write — they build
    /// fixture directories, and they are the only code here that is allowed
    /// to.
    ///
    /// The order of the two steps is the whole of this function. Cutting the
    /// raw source at that literal cuts it wherever the literal appears, and a
    /// `//` line is the easiest place in a Rust file for it to appear: one
    /// sentence about how the tests are gated, written in good faith above a
    /// function, ended the scan for everything below it in that file. A
    /// comment cannot end the scan if there are no comments left by the time
    /// the cut is made.
    fn shipped_code(src: &str) -> String {
        let code = code_of(src);
        code.split("#[cfg(test)]")
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// Source with the comments taken out, so that a rule can be *written
    /// down* in the file it governs.
    ///
    /// `//` only: there are no block comments here, and a checker that
    /// pretended to understand Rust lexically would be a worse liar than one
    /// that admits what it scans. Where a line has a quote before its `//`
    /// the line is kept whole, because that `//` may be inside a string
    /// literal and the two mistakes are not worth the same: keeping a comment
    /// can at worst make the scan complain about a word somebody wrote in
    /// prose, while cutting a line of code hides whatever else was on it.
    fn code_of(src: &str) -> String {
        src.lines()
            .map(|line| match line.find("//") {
                Some(at) if !line[..at].contains('"') => &line[..at],
                _ => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every identifier a `use` declaration in this code brings into scope,
    /// read one statement at a time so that a declaration split over several
    /// lines is read whole.
    ///
    /// A glob comes back as `*` and a rename as `as`. Neither is a name to
    /// look up; both are things to refuse, because each is a way for a call
    /// to arrive under a spelling that appears nowhere for a scan to read.
    fn imported_names(code: &str) -> Vec<String> {
        let lines: Vec<&str> = code.lines().collect();
        let mut names = Vec::new();
        let mut at = 0;
        while at < lines.len() {
            let head = lines[at].trim_start();
            let head = head
                .strip_prefix("pub(crate) ")
                .or_else(|| head.strip_prefix("pub "))
                .unwrap_or(head);
            if !head.starts_with("use ") {
                at += 1;
                continue;
            }
            let mut statement = String::new();
            while at < lines.len() {
                statement.push_str(lines[at]);
                let ends_here = lines[at].contains(';');
                at += 1;
                if ends_here {
                    break;
                }
            }
            names.extend(
                statement
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '*')
                    .filter(|token| !token.is_empty())
                    .map(str::to_string),
            );
        }
        names
    }

    /// Everything in one file's shipping code that says this crate might
    /// write, worded for the failure message.
    fn forbidden_in(code: &str) -> Vec<String> {
        // Every `std` entry point that can create, modify, remove or relink
        // something on disk, each spelled the shortest way that still cannot
        // match anything else. Sub-spellings are covered by their prefixes:
        // `File::create` catches `create_new`, `create_dir` catches
        // `create_dir_all`, `remove_dir` catches `remove_dir_all`, and `chown`
        // catches `lchown` and `fchown`.
        //
        // The list is written against the surface and not against the habits:
        // three of these are spellings that reach the filesystem while naming
        // none of the obvious ones. `File::options` opens for writing without
        // the word `OpenOptions`, `DirBuilder` creates a directory without the
        // word `create_dir`, and `soft_link` makes a symbolic link without the
        // word `symlink`.
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
        // `fs::write`, `fs::copy` and `fs::rename` are the only entries above
        // that carry a path, and a grouped import walks straight past all
        // three: `use std::fs::{copy, metadata};` puts `copy` in scope, and
        // the call site then spells no path at all. Their bare names cannot go
        // in the list above — `write` is also `write!` and `fmt::Write`, and
        // `copy` is also `copy_from_slice` — so they are refused where they
        // enter instead. A `use` that pulls one of them into this crate is
        // itself the thing to refuse.
        //
        // The rest of the list needs no entry here. `create_dir` and its kind
        // are already unambiguous as bare words, so the scan above finds them
        // in a `use` line as readily as at a call; and `File::create` and
        // `File::options` are associated functions, which no `use` can name.
        const MUTATING_IMPORTS: [&str; 3] = ["write", "copy", "rename"];

        let mut found = Vec::new();
        for bad in FORBIDDEN {
            if code.contains(bad) {
                found.push(format!("names `{bad}`"));
            }
        }
        for name in imported_names(code) {
            if name == "*" {
                found.push(
                    "imports a module whole with a glob, which puts names in scope that \
                     appear nowhere for this scan to read"
                        .to_string(),
                );
            } else if name == "as" {
                found.push(
                    "renames an import, which is how a call reaches the filesystem under a \
                     spelling this scan was never written against"
                        .to_string(),
                );
            } else if MUTATING_IMPORTS.contains(&name.as_str()) {
                found.push(format!(
                    "imports `{name}`, which modifies a file when it is called under no \
                     path at all"
                ));
            }
        }
        found
    }

    /// The dependency a manifest declares, if it declares one.
    ///
    /// Cargo spells a dependency table three ways and only two of them end in
    /// the word. `[dependencies]` and `[target.'cfg(unix)'.dependencies]` hold
    /// a line per dependency; `[dependencies.quick-xml]` names its dependency
    /// in the header and holds that one dependency's keys. A check that
    /// recognised only the first kind read the third as a table about
    /// something other than dependencies, and then read every key under it as
    /// a line belonging to no table at all.
    fn dependency_in(manifest: &str) -> Option<String> {
        let mut under_dependencies = false;
        for line in manifest.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            if let Some(table) = line.strip_prefix('[') {
                let name = table.trim_end_matches(']').trim_start_matches('[');
                let segments: Vec<&str> = name.split('.').map(str::trim).collect();
                let at = segments.iter().position(|s| s.ends_with("dependencies"));
                under_dependencies = at.is_some();
                if let Some(at) = at {
                    let named = segments[at + 1..].join(".");
                    if !named.is_empty() {
                        return Some(named);
                    }
                }
                continue;
            }
            if under_dependencies && !line.is_empty() {
                return Some(line.to_string());
            }
        }
        None
    }

    /// Every `.rs` file under a directory, named the way [`shipped_source`]
    /// names them.
    ///
    /// It goes down, because `src/xml/scratchpad.rs` is a module of this
    /// crate exactly as much as `src/xml.rs` is.
    fn rust_files(root: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut todo = vec![root.to_path_buf()];
        while let Some(dir) = todo.pop() {
            let entries = std::fs::read_dir(&dir).expect("a directory of this crate's own source");
            for entry in entries {
                let path = entry.expect("an entry").path();
                if path.is_dir() {
                    todo.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let under = path
                    .strip_prefix(root)
                    .expect("a path under the root it was found beneath");
                found.push(under.to_string_lossy().into_owned());
            }
        }
        found
    }

    /// The `.rs` files a crate root ships that a list of scanned names does
    /// not cover.
    ///
    /// The question is asked of the directories, not of what any one file
    /// declares. A module or a program that ships here and is not scanned is
    /// one nobody checked, and the way that happens is somebody adding one —
    /// to `xml.rs` as readily as to `lib.rs`.
    fn unscanned_files(scanned: &[&str], crate_root: &Path) -> Vec<String> {
        let mut missing = Vec::new();
        for (dir, prefix) in [("src", ""), ("examples", "examples/")] {
            for name in rust_files(&crate_root.join(dir)) {
                let shipped = format!("{prefix}{name}");
                if !scanned.contains(&shipped.as_str()) {
                    missing.push(shipped);
                }
            }
        }
        missing
    }

    /// A tripwire on the crate's shipping source, for the day somebody adds a
    /// write by hand.
    ///
    /// This is not the proof that the crate does not write — the crate docs
    /// say what is, and why a measurement and a test are different kinds of
    /// thing. What this one has over a measurement is that it fails the
    /// build, every build, and that is worth having on its own terms. What it
    /// does not have is completeness.
    ///
    /// It checks spellings, so what it cannot catch is a call that reaches
    /// the filesystem under a spelling nobody wrote into the list. Two ways
    /// that has already happened are closed below, and neither was sabotage:
    /// a `//` line naming the test gate used to end the scan for the rest of
    /// its file, and a grouped `use` of `std::fs` used to put a mutating call
    /// in scope under a bare name the list does not carry. Closing those does
    /// not finish the list, and the class stays open — a `std` entry point
    /// added after this was written, a macro that assembles the path out of
    /// fragments, a `#[path]` or an `include!` naming a file outside the
    /// directories walked here. Certainty comes from re-running the
    /// measurements the crate docs describe. This is what notices in between.
    #[test]
    fn never_writes() {
        let complaints: Vec<String> = shipped_source()
            .into_iter()
            .flat_map(|(name, code)| {
                forbidden_in(&code)
                    .into_iter()
                    .map(move |what| format!("{name} {what}"))
            })
            .collect();
        assert!(
            complaints.is_empty(),
            "this crate only ever reads: {}",
            complaints.join("; ")
        );
    }

    /// The preconditions [`never_writes`] rests on.
    ///
    /// Each check here closes a way for code to run in this crate under a
    /// name the scan has never read. A dependency brings its own vocabulary,
    /// and no list written against `std` can cover it. `unsafe` reaches libc
    /// directly, where the name on the call need not be a Rust name at all.
    /// And a file the scan does not open is simply a file nobody checked,
    /// which is why the question is asked of the directories: a module
    /// declared in `xml.rs` ships exactly as much as one declared in
    /// `lib.rs`, and reading only `lib.rs`'s declarations found neither the
    /// file nor the hole.
    ///
    /// A file that scans to nothing is the last of them, and the cheapest to
    /// miss: every hole found in this machinery so far has had the same
    /// shape, a scan reading less than it looks like it is reading and
    /// passing quietly for it. A file opened and cut down to nothing passes
    /// [`never_writes`] exactly as a clean one does, so it is asked about
    /// here instead.
    #[test]
    fn nothing_can_be_called_that_is_not_in_std() {
        let dependency = dependency_in(include_str!("../Cargo.toml"));
        assert!(
            dependency.is_none(),
            "a dependency — `{}` — puts names under this crate that `never_writes` \
             was never written against",
            dependency.unwrap_or_default()
        );
        let shipped = shipped_source();
        let scanned: Vec<&str> = shipped.iter().map(|(name, _)| *name).collect();
        let missing = unscanned_files(&scanned, Path::new(env!("CARGO_MANIFEST_DIR")));
        assert!(
            missing.is_empty(),
            "these ship and are not scanned by `never_writes`: {}",
            missing.join(", ")
        );
        for (name, code) in &shipped {
            assert!(
                !code.trim().is_empty(),
                "{name} scanned down to nothing, so `never_writes` read none of it"
            );
            assert!(
                !code.contains("unsafe"),
                "{name} names `unsafe`: past `std` there is no name to scan for"
            );
        }
    }

    /// A comment cannot end the scan — which it could, for as long as the cut
    /// was made before the comments were taken out.
    #[test]
    fn a_comment_naming_the_test_gate_does_not_end_the_scan() {
        for src in [
            "// The tests below are gated on #[cfg(test)], as usual.\n\
             fn stash(p: &Path) {\n    let _ = std::fs::write(p, b\"\");\n}\n",
            "fn stash(p: &Path) { // gated on #[cfg(test)]\n    \
             let _ = std::fs::write(p, b\"\");\n}\n",
        ] {
            assert!(
                !forbidden_in(&shipped_code(src)).is_empty(),
                "a comment naming the gate turned the scan off below it: {src}"
            );
        }
        let gated = "fn read_it() {}\n#[cfg(test)]\nmod tests {\n    \
                     fn fixture() { std::fs::remove_file(\"x\"); }\n}\n";
        assert!(
            forbidden_in(&shipped_code(gated)).is_empty(),
            "the gate itself no longer cuts the tests off"
        );
    }

    /// A mutating call can arrive under a bare name, and the `use` that put
    /// it there is ordinary Rust rather than an evasion.
    #[test]
    fn a_use_declaration_cannot_walk_a_mutating_name_past_the_list() {
        for src in [
            "use std::fs::{copy, metadata};\nfn back_up(p: &Path) { let _ = copy(p, p); }\n",
            "use std::fs::{metadata, rename};\nfn move_it(p: &Path) { let _ = rename(p, p); }\n",
            "use std::fs::{\n    metadata,\n    write,\n};\n",
            "use std::fs::*;\n",
            "use std::fs::File as Handle;\n",
        ] {
            assert!(
                !forbidden_in(&shipped_code(src)).is_empty(),
                "a `use` put a mutating name in scope and the scan read past it: {src}"
            );
        }
        let reading_only = "use std::path::{Path, PathBuf};\nuse crate::{xml, Lookup, Source};\n";
        assert!(
            forbidden_in(&shipped_code(reading_only)).is_empty(),
            "an ordinary import of names that read nothing"
        );
    }

    /// Cargo's spellings of a dependency table, one of which used to read as
    /// no dependency table at all.
    #[test]
    fn a_dependency_is_found_however_its_table_is_spelled() {
        assert_eq!(
            dependency_in("[dependencies]\nquick-xml = \"x\"\n").as_deref(),
            Some("quick-xml = \"x\"")
        );
        assert_eq!(
            dependency_in("[dependencies.quick-xml]\npath = \"../quick-xml\"\n").as_deref(),
            Some("quick-xml")
        );
        assert_eq!(
            dependency_in("[dev-dependencies.tempfile]\nversion = \"x\"\n").as_deref(),
            Some("tempfile")
        );
        assert_eq!(
            dependency_in("[target.'cfg(unix)'.dependencies]\nlibc = \"x\"\n").as_deref(),
            Some("libc = \"x\"")
        );
        assert_eq!(
            dependency_in("[package]\nname = \"x\"\n\n[[bin]]\nname = \"y\"\n"),
            None
        );
        assert_eq!(dependency_in(include_str!("../Cargo.toml")), None);
    }

    /// A module declared inside a module is a file that ships, so the walk
    /// that looks for unscanned files has to go down.
    #[test]
    fn a_module_nested_under_another_is_still_asked_about() {
        let root = scratch("nested-module");
        std::fs::create_dir_all(root.join("src/xml")).expect("a nested module directory");
        std::fs::create_dir_all(root.join("examples")).expect("an examples directory");
        std::fs::write(root.join("src/lib.rs"), "pub mod xml;").expect("lib.rs");
        std::fs::write(root.join("src/xml.rs"), "mod scratchpad;").expect("xml.rs");
        std::fs::write(root.join("src/xml/scratchpad.rs"), "").expect("the nested module");
        std::fs::write(root.join("examples/read.rs"), "").expect("the example");
        assert_eq!(
            unscanned_files(&["lib.rs", "xml.rs", "examples/read.rs"], &root),
            ["xml/scratchpad.rs"]
        );
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
