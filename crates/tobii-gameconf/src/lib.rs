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
//! ## What holds that up, and what each part of it is worth
//!
//! **The test that runs the code.** This is what the promise rests on.
//! `writes_nothing`, in this crate's `tests` directory, builds a tree shaped
//! like the real data — a preset directory, a flat attribute list, documents
//! malformed in each way the scanner refuses, a file that is not text, a
//! symbolic link, a path sealed against the process — records every path in
//! it with its length, its modification time to the nanosecond and a hash of
//! its bytes, runs every public entry point of this crate over it, and
//! records the tree again. The two records have to be equal. That catches a
//! write whatever the write is called: a file that was modified looks
//! different afterwards no matter which name modified it, so no import
//! style, macro, comment or submodule gets past it. Its limits are stated in
//! that file, and they are real — it sees the paths its fixture holds and
//! nothing else, and it cannot see code that had already run by the time it
//! started. That it runs *every* entry point is checked from in here rather
//! than promised there.
//!
//! **The tripwire.** `never_writes` in the tests below reads this crate's
//! shipping source and fails the build on the name of a call that could
//! modify a file. It is secondary and it is not a proof: a list of spellings
//! is exactly as complete as the spellings somebody thought of, and this one
//! has been walked past repeatedly — by a comment, by a grouped import, by a
//! module nobody had listed — almost every time by ordinary Rust rather than
//! by anything trying to. What it is for is code the behavioural test cannot
//! run. This crate has no `build.rs`; a `build.rs` would run on every build
//! of every program that depends on it and would have finished before any
//! test here started, and it is scanned if it ever appears — along with the
//! example, which is a program a maintainer points at real files by hand,
//! and every other target cargo compiles. The tripwire also leans on two
//! facts the test beside it asserts rather than assumes — no dependencies,
//! so `std` is the whole of what this crate can call, and no `unsafe`, so it
//! cannot reach past `std` under a name that is in no Rust source at all —
//! and those bound its vocabulary without completing it.
//!
//! **Two measurements, run once, by hand.** A real Proton prefix was
//! snapshotted whole before and after this crate read it and came back
//! identical, and this crate's readers were run under a shim that aborts on
//! any mutating syscall, which did not fire. Neither can be reproduced from
//! anything in this repository: no fixture here is that prefix and no shim
//! here is that shim. They are a person's report of what happened on one
//! machine on one day. They are worth what that is worth, which is something
//! — and they are not what any of the above rests on, because a `build.rs`
//! added the day after either of them would have gone unremarked beside both.
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

    /// Every file this crate ships that runs for somebody who is not running
    /// its tests, as the scanners below read it.
    ///
    /// The example is in here with the modules. It is code this crate ships,
    /// it is the thing a maintainer points at real files to check something,
    /// and a guarantee that covered the library but not the program that
    /// exercises it would be a guarantee about the half nobody runs by hand.
    /// A `build.rs` would belong here too, and there is none — which is a
    /// fact [`nothing_can_be_called_that_is_not_in_std`] checks rather than
    /// one this list asserts.
    ///
    /// Whether this is all of them is not taken on trust: that same test
    /// walks every directory cargo compiles and fails on any `.rs` file that
    /// is named neither here nor in [`TEST_ONLY`].
    fn shipped_source() -> Vec<(&'static str, Result<String, String>)> {
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

    /// The files cargo compiles that no consumer of this crate ever runs.
    ///
    /// They are walked and named, and not scanned. Test and benchmark
    /// targets are separate crates: nothing in `src` can call into one, so a
    /// write here cannot become a write in a program somebody installed.
    /// They are listed rather than waved through as a directory so that
    /// adding one is a decision somebody makes on purpose. The two ways
    /// shipping code could reach a file outside its own directories —
    /// `#[path]` and `include!` — are refused by [`forbidden_in`].
    const TEST_ONLY: [&str; 1] = ["tests/writes_nothing.rs"];

    /// One file's shipping code: the comments taken out, then everything from
    /// the gated test module onwards cut off.
    ///
    /// The tests are cut off because they themselves write — they build
    /// fixture directories, and they are the only code here that is allowed
    /// to. Cutting is the dangerous half of this whole tripwire: a cut made
    /// early reads less than the caller thinks it read, and passes quietly
    /// for it. Four separate holes found in this machinery were that literal
    /// appearing somewhere nobody expected — in a `//` comment, in a block
    /// comment, in a string constant, and in a trailing comment after a
    /// quote, which `code_of` keeps whole on purpose — and a fifth was a
    /// gated helper sitting beside the code it exercises, which is the most
    /// ordinary Rust of the five.
    ///
    /// So the cut refuses rather than guesses. The first occurrence in the
    /// comment-stripped source must be the whole of a column-zero line,
    /// followed by the column-zero line `mod tests {`, with nothing at column
    /// zero after it but the brace that closes that module — which, under
    /// `cargo fmt`, is what "the gated tests are the last item in the file"
    /// looks like. An occurrence anywhere else is an error naming itself, not
    /// a shorter scan. A file with no occurrence at all is scanned whole.
    ///
    /// What this does not check is the text between those braces: a string
    /// literal holding a line at column zero would be read as an item and
    /// refused. That is the direction to be wrong in, and indenting it is the
    /// fix.
    fn shipped_code(src: &str) -> Result<String, String> {
        const GATE: &str = "#[cfg(test)]";
        let code = code_of(src);
        let Some(at) = code.find(GATE) else {
            return Ok(code);
        };
        let line_start = code[..at].rfind('\n').map_or(0, |nl| nl + 1);
        let (before, rest) = code.split_at(line_start);
        let mut lines = rest.lines();
        if lines.next().map(str::trim_end) != Some(GATE) {
            return Err(format!(
                "names `{GATE}` somewhere that is not the attribute on the gated \
                 test module, so the scan cannot tell where this file's shipping \
                 code ends"
            ));
        }
        if lines.next().map(str::trim_end) != Some("mod tests {") {
            return Err(format!(
                "has a `{GATE}` that is not the gate on the final `mod tests`, and \
                 gated code outside that module is code this scan would not read"
            ));
        }
        let after: Vec<&str> = lines
            .filter(|line| !line.is_empty() && !line.starts_with([' ', '\t']))
            .collect();
        if after != ["}"] {
            return Err(format!(
                "has items after its gated test module, which the scan stops at: {}",
                after.join(" ")
            ));
        }
        Ok(before.to_string())
    }

    /// Source with the comments taken out, so that a rule can be *written
    /// down* in the file it governs.
    ///
    /// `//` only: block comments are not handled, and a checker that
    /// pretended to understand Rust lexically would be a worse liar than one
    /// that admits what it scans. Where a line has a quote before its `//`
    /// the line is kept whole, because that `//` may be inside a string
    /// literal and cutting a line of code hides whatever else was on it.
    ///
    /// Both of those leave comment text in the scanned code, and neither can
    /// shorten the scan any more: [`shipped_code`] refuses a gate literal it
    /// does not find in attribute position wherever it came from. What they
    /// can still do is make the scan complain about a word somebody wrote in
    /// prose, which is a loud failure with an obvious fix.
    fn code_of(src: &str) -> String {
        src.lines()
            .map(|line| match line.find("//") {
                Some(at) if !line[..at].contains('"') => &line[..at],
                _ => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every identifier a `use` declaration in this code brings into scope.
    ///
    /// The word is looked for as a token rather than at the start of a line,
    /// because every way of writing a visibility puts something in front of
    /// it — `pub use`, `pub(crate) use`, `pub(super) use`, `pub(in
    /// crate::xml) use` — and a `use` inside a function body can have a whole
    /// statement in front of it on the same line. Matching the shapes was
    /// tried, and `pub(super)` walked past a check that knew two of them.
    /// From there the statement is read to its semicolon, so that a
    /// declaration split over several lines is read whole.
    ///
    /// A glob comes back as `*` and a rename as `as`. Neither is a name to
    /// look up; both are things to refuse, because each is a way for a call
    /// to arrive under a spelling that appears nowhere for a scan to read.
    fn imported_names(code: &str) -> Vec<String> {
        let mut names = Vec::new();
        let mut at = 0;
        while let Some(found) = code[at..].find("use ") {
            let start = at + found;
            at = start + "use ".len();
            // `use` has to be its own word: `reuse `, and any identifier
            // ending in those three letters, is not a declaration.
            if code[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                continue;
            }
            let end = code[at..].find(';').map_or(code.len(), |i| at + i);
            names.extend(
                code[at..end]
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '*')
                    .filter(|token| !token.is_empty())
                    .map(str::to_string),
            );
            at = end;
        }
        names
    }

    /// Everything in one file's shipping code that says this crate might
    /// write, worded for the failure message.
    fn forbidden_in(code: &str) -> Vec<String> {
        // Every `std` entry point that can create, modify, remove or relink
        // something on disk — plus, at the end, the two ways a file the walk
        // never saw becomes part of one it did — each spelled the shortest
        // way that still cannot match anything else. Sub-spellings are
        // covered by their prefixes:
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
        const FORBIDDEN: [&str; 23] = [
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
            // Not writes: the two ways a file outside the walked directories
            // becomes part of a scanned one. `#[path]` names a module
            // anywhere on disk and `include!` pastes a file in whole, and
            // either would put code in this crate that the walk above reports
            // as nothing to scan. `include_str!` is a different word and is
            // not matched by this one; it reads a file at compile time and
            // produces a string.
            "#[path",
            "include!",
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

    /// Every `.rs` file under a directory, relative to it.
    ///
    /// It goes down, because `src/xml/scratchpad.rs` is a module of this
    /// crate exactly as much as `src/xml.rs` is. A directory that is not
    /// there holds nothing: this crate has no `tests` directory of unit
    /// fixtures and no benchmarks today, and the point of asking is that
    /// either could appear.
    fn rust_files(root: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut todo = vec![root.to_path_buf()];
        while let Some(dir) = todo.pop() {
            // Not there is not the same as could not be read, here for the
            // same reason it is everywhere else in this crate: the first is
            // "this crate has no benchmarks", and the second is a walk that
            // found nothing because it saw nothing, which would pass this
            // check by reading none of the files it is about.
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => panic!("{} could not be listed: {e}", dir.display()),
            };
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

    /// Every `.rs` file cargo compiles from this crate root, named the way
    /// [`shipped_source`] and [`TEST_ONLY`] name them.
    ///
    /// All five targets, because a walk that covered some of them is a walk
    /// whose result reads like a walk that covered all of them. `build.rs` is
    /// the one that matters most and is easiest to leave out: it is a single
    /// file rather than a directory, it runs on every build of every program
    /// that depends on this crate, and it runs before any test here can look
    /// at anything.
    fn compiled_files(crate_root: &Path) -> Vec<String> {
        let mut found = Vec::new();
        if crate_root.join("build.rs").is_file() {
            found.push("build.rs".to_string());
        }
        for (dir, prefix) in [
            ("src", ""),
            ("examples", "examples/"),
            ("tests", "tests/"),
            ("benches", "benches/"),
        ] {
            for name in rust_files(&crate_root.join(dir)) {
                found.push(format!("{prefix}{name}"));
            }
        }
        found
    }

    /// The `.rs` files cargo compiles that a list of names does not account
    /// for.
    ///
    /// The question is asked of the directories, not of what any one file
    /// declares. A module or a program that compiles here and is on neither
    /// list is one nobody looked at, and the way that happens is somebody
    /// adding one — to `xml.rs` as readily as to `lib.rs`.
    fn unscanned_files(named: &[&str], crate_root: &Path) -> Vec<String> {
        compiled_files(crate_root)
            .into_iter()
            .filter(|file| !named.contains(&file.as_str()))
            .collect()
    }

    /// A tripwire on the crate's shipping source, for the day somebody adds a
    /// write by hand.
    ///
    /// This is not what the crate's promise rests on. `tests/`'s
    /// `writes_nothing` is: it runs the crate over a tree and looks at the
    /// tree afterwards, which catches a write whatever it is called. This one
    /// reads the source and looks for names, and a list of names is exactly
    /// as complete as the spellings somebody thought of. It has been walked
    /// past repeatedly, almost always by ordinary Rust rather than by
    /// anything trying to.
    ///
    /// What it is for is the one thing running the code cannot do: it reads
    /// files that never run inside a test. A `build.rs` runs on every build
    /// of every program that depends on this crate, and has already run by
    /// the time a test binary starts; an example is a program a maintainer
    /// points at real files by hand. Both are scanned here. It also fails on
    /// the spot, in the diff that introduced the call, which is cheaper than
    /// finding out from a behavioural test which tree got written to.
    ///
    /// So: a tripwire against drift, useful where it is the only thing
    /// looking, and not a guarantee. What it cannot catch is a call that
    /// reaches the filesystem under a spelling nobody wrote into the list —
    /// a `std` entry point added after this was written, a macro that
    /// assembles the call out of fragments, a name that arrives through a
    /// dependency. The checks in
    /// [`nothing_can_be_called_that_is_not_in_std`] are what bound that
    /// vocabulary; they do not complete it.
    #[test]
    fn never_writes() {
        let complaints: Vec<String> = shipped_source()
            .into_iter()
            .flat_map(|(name, code)| {
                let found = match code {
                    Ok(code) => forbidden_in(&code),
                    Err(why) => vec![why],
                };
                found.into_iter().map(move |what| format!("{name} {what}"))
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
        let named: Vec<&str> = shipped
            .iter()
            .map(|(name, _)| *name)
            .chain(TEST_ONLY)
            .collect();
        let missing = unscanned_files(&named, Path::new(env!("CARGO_MANIFEST_DIR")));
        assert!(
            missing.is_empty(),
            "cargo compiles these and nobody has said what they are: {}",
            missing.join(", ")
        );
        for (name, code) in &shipped {
            let code = match code {
                Ok(code) => code,
                // `never_writes` is where this is reported as the failure it
                // is; here it would only be reported twice.
                Err(_) => continue,
            };
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

    /// One file's worth of what [`never_writes`] would say about it: the
    /// refusals from the cut and the names from the scan, together, because
    /// together is how the test reads them.
    fn complaints_about(src: &str) -> Vec<String> {
        match shipped_code(src) {
            Ok(code) => forbidden_in(&code),
            Err(why) => vec![why],
        }
    }

    /// The cut fails closed: the gate literal anywhere but in attribute
    /// position on the final `mod tests` is an error, not a shorter scan.
    ///
    /// Every entry below is a way this scan has actually been walked past,
    /// and only the last of them was anybody trying to. They were four
    /// separate holes while the cut took the first occurrence wherever it
    /// found it; they are one rule now.
    #[test]
    fn a_gate_literal_out_of_place_is_refused_rather_than_obeyed() {
        let writes = "fn stash(p: &Path) { let _ = std::fs::write(p, b\"\"); }\n";
        for (what, src) in [
            (
                "a line comment naming the gate",
                format!("// The tests below are gated on #[cfg(test)], as usual.\n{writes}"),
            ),
            (
                "a block comment naming the gate",
                format!("/* everything below is #[cfg(test)] */\n{writes}"),
            ),
            (
                "a string constant holding the gate",
                format!("const GATE: &str = \"#[cfg(test)]\";\n{writes}"),
            ),
            (
                "a trailing comment after a quote",
                format!("const KEY: &str = \"X\"; // gated on #[cfg(test)]\n{writes}"),
            ),
            (
                "a gated helper beside the code it exercises",
                format!(
                    "#[cfg(test)]\nfn sample() -> &'static str {{ \"x\" }}\n{writes}\
                     #[cfg(test)]\nmod tests {{\n}}\n"
                ),
            ),
            (
                "a gated helper inside a module",
                format!("mod xml {{\n    #[cfg(test)]\n    fn sample() {{}}\n}}\n{writes}"),
            ),
            (
                "an item after the gated module",
                format!("#[cfg(test)]\nmod tests {{\n    fn fixture() {{}}\n}}\n{writes}"),
            ),
        ] {
            assert!(
                !complaints_about(&src).is_empty(),
                "{what} left the write below it unscanned: {src}"
            );
        }

        let gated = "fn read_it() {}\n#[cfg(test)]\nmod tests {\n    \
                     fn fixture() { std::fs::remove_file(\"x\"); }\n}\n";
        assert!(
            complaints_about(gated).is_empty(),
            "the gate in its one legitimate place no longer cuts the tests off"
        );
        assert!(
            !complaints_about(writes).is_empty(),
            "a file with no gate in it at all is scanned whole"
        );
    }

    /// A mutating call can arrive under a bare name, and the `use` that put
    /// it there is ordinary Rust rather than an evasion.
    ///
    /// Three of the spellings below walked past a check that looked for
    /// `use` at the start of a line under one of two visibilities. There are
    /// more than two visibilities, and a `use` inside a function body is not
    /// at the start of anything.
    #[test]
    fn a_use_declaration_cannot_walk_a_mutating_name_past_the_list() {
        for src in [
            "use std::fs::{copy, metadata};\nfn back_up(p: &Path) { let _ = copy(p, p); }\n",
            "use std::fs::{metadata, rename};\nfn move_it(p: &Path) { let _ = rename(p, p); }\n",
            "use std::fs::{\n    metadata,\n    write,\n};\n",
            "use std::fs::*;\n",
            "use std::fs::File as Handle;\n",
            "pub use std::fs::{copy, metadata};\n",
            "pub(crate) use std::fs::{copy, metadata};\n",
            "pub(super) use std::fs::{copy, metadata};\n",
            "pub(in crate::xml) use std::fs::{metadata, rename};\n",
            "fn save(p: &Path) { use std::fs::{metadata, write}; let _ = write(p, b\"\"); }\n",
        ] {
            assert!(
                !complaints_about(src).is_empty(),
                "a `use` put a mutating name in scope and the scan read past it: {src}"
            );
        }
        for reading_only in [
            "use std::path::{Path, PathBuf};\nuse crate::{xml, Lookup, Source};\n",
            // `use` has to be its own word, or every identifier ending in
            // those letters starts a declaration that runs to the next
            // semicolon and reads whatever is in it.
            "fn clause() { let clause = write; }\n",
        ] {
            assert!(
                complaints_about(reading_only).is_empty(),
                "an ordinary line refused: {reading_only}"
            );
        }
    }

    /// The behavioural test calls every function a caller of this crate can
    /// call.
    ///
    /// It says it does, in a file that cannot be made to prove it: an
    /// integration test is limited to the public surface, but nothing in it
    /// forces it to touch all of that surface, and a function added to
    /// [`binds`] tomorrow would sit there uncovered while the sentence above
    /// it went on claiming otherwise. So the claim is checked from in here,
    /// where the public modules and their functions can be read off the
    /// source.
    ///
    /// This is a name scan like [`never_writes`], and worth the same: it
    /// catches drift and it is not a proof. What it is checking, though, is
    /// a list of five names in one file, rather than every way a call can be
    /// spelled.
    #[test]
    fn the_behavioural_test_calls_every_public_function() {
        let behavioural = include_str!("../tests/writes_nothing.rs");
        let shipped = shipped_source();
        let source_of = |file: &str| {
            shipped
                .iter()
                .find(|(name, _)| *name == file)
                .and_then(|(_, code)| code.as_ref().ok())
                .expect("a module this crate ships")
                .clone()
        };
        let mut missing = Vec::new();
        let mut checked = 0;
        for line in source_of("lib.rs").lines() {
            let Some(module) = line.trim_end().strip_prefix("pub mod ") else {
                continue;
            };
            let module = module.trim_end_matches(';');
            for line in source_of(&format!("{module}.rs")).lines() {
                let Some(rest) = line.strip_prefix("pub fn ") else {
                    continue;
                };
                let call = format!("{module}::{}(", rest.split('(').next().unwrap_or_default());
                checked += 1;
                if !behavioural.contains(&call) {
                    missing.push(call);
                }
            }
        }
        assert!(
            missing.is_empty(),
            "`tests/writes_nothing` says it runs every public entry point and \
             does not call: {}",
            missing.join(", ")
        );
        assert!(
            checked > 0,
            "no public function was found to check, so this test checked nothing"
        );
    }

    /// Everything cargo compiles is walked, not only the two directories
    /// that hold the library.
    ///
    /// `build.rs` is the one this used to miss, and the expensive one to
    /// miss: it runs on every build of everything that depends on this
    /// crate, and it has finished running before any test here starts.
    #[test]
    fn every_target_cargo_compiles_is_asked_about() {
        let root = scratch("compiled-targets");
        for dir in ["src", "examples", "tests", "benches"] {
            std::fs::create_dir_all(root.join(dir)).expect("a target directory");
        }
        for file in [
            "build.rs",
            "src/lib.rs",
            "examples/read.rs",
            "tests/writes_nothing.rs",
            "benches/parse.rs",
        ] {
            std::fs::write(root.join(file), "").expect("a target file");
        }
        let mut missing = unscanned_files(&["lib.rs"], &root);
        missing.sort();
        assert_eq!(
            missing,
            [
                "benches/parse.rs",
                "build.rs",
                "examples/read.rs",
                "tests/writes_nothing.rs",
            ]
        );
        assert!(
            unscanned_files(
                &[
                    "build.rs",
                    "lib.rs",
                    "examples/read.rs",
                    "tests/writes_nothing.rs",
                    "benches/parse.rs",
                ],
                &root
            )
            .is_empty(),
            "a crate whose every compiled file is named still reports some"
        );
        let _ = std::fs::remove_dir_all(&root);
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
