//! Whether this crate writes, asked of a tree it has just been run over.
//!
//! This is the check the crate's promise rests on. It builds a directory
//! shaped like the real data, records what every path in it looks like, runs
//! every public entry point of the crate over it — the answers and the
//! refusals both — and records the tree again. The two records must be equal.
//!
//! What that catches is a write to a path it watches, however the write is
//! spelled. A grouped import, a renamed one, a type alias, a macro that
//! assembles the call out of fragments, a helper in a module nobody thought
//! to scan, a name added to `std` after anybody here stopped looking: none of
//! them changes the fact that a file that was modified has a different
//! length, or a different modification time, or different bytes. That is the
//! whole reason this test exists beside the name scan in `lib.rs`, which can
//! only ever find what somebody spelled out in advance. Which paths it
//! watches is the limit that matters, and it is listed below.
//!
//! It is an integration test on purpose: from out here the only things
//! reachable are the ones a caller can reach, so what it runs is the crate
//! as somebody using it gets it. That the list below is *all* of them is a
//! separate matter, and not something this file can show — the compiler
//! stops it reaching further, and lets it reach less. `lib.rs`'s
//! `the_behavioural_test_calls_every_public_function` is what checks it.
//!
//! # Running the code is not the same as reaching it
//!
//! A snapshot that comes back identical says nothing about a branch that
//! never ran. `binds::read` answers at its first lookup for a directory that
//! is not there at all, and a fixture made only of those would leave most of
//! the function — and any write planted in it — untouched while every path in
//! the tree stayed byte-identical.
//!
//! So the fixture holds one directory per shape the reader distinguishes, and
//! [`Seen`] counts the cases each entry point answered with: the three
//! `Source` cases, the three `Lookup` cases, and the three `binds::Found`
//! cases one level down. A run where any of the nine went unreached fails
//! here rather than passing on a tree that was never really read.
//!
//! # What it does not cover
//!
//! Said plainly, because the point of this file is to be the thing that does
//! not promise more than it delivers.
//!
//! * **Paths outside the scratch directory.** The fixture is a directory
//!   inside a scratch directory of its own, and it is the scratch directory
//!   that is snapshotted — so a write beside the fixture, or to its parent,
//!   or to a sibling built with `with_extension`, shows up as something that
//!   appeared. Two places outside it are watched one level deep, by
//!   [`watched_outside`] and [`shallow`]: the directory a test binary is
//!   started in, and the directory it was built into. What that catches is a
//!   file directly inside either that appeared or was rewritten; what it does
//!   not is anything further down. Anywhere else — `$HOME`, a path built from
//!   an environment variable, `/tmp` at large — is invisible here,
//!   and nothing else in this crate makes up for that. The name scan in
//!   `lib.rs` looks by spelling and has been walked past by ordinary Rust
//!   every round somebody has tried; the syscall shim that would answer
//!   about any path at all is run by hand and is not in this repository.
//! * **Code that runs before this test does.** A `build.rs` has already run
//!   by the time a test binary starts, so nothing here can see what it did.
//!   The name scan does not scan one either: it walks every directory cargo
//!   compiles from, and a `build.rs` is on neither of its lists, so the build
//!   fails until somebody decides what to do about it.
//! * **A write and a restore inside one call.** Length, modification time to
//!   the nanosecond and a hash of the bytes are what is compared; a write
//!   that put every one of them back would pass. Nothing in reach of this
//!   crate does that by accident, and a crate that did it on purpose is not
//!   what either check here is for.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tobii_gameconf::{attrs, binds, Lookup, Source};

/// What one path looks like from outside, in the terms a write would change.
///
/// No access time: reading changes that one, and this test reads constantly.
/// Everything else here moves only when something modifies the path — the
/// length and the hash when the bytes change, the modification time when they
/// are rewritten with the same bytes, the mode when only the permissions are
/// touched, and the kind when a file becomes a directory or a link.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    kind: &'static str,
    len: u64,
    modified_nanos: Option<u128>,
    mode: u32,
    /// [`None`] for anything whose bytes this test cannot read — a directory,
    /// a symbolic link, a file the permissions keep it out of.
    hash: Option<u64>,
}

/// FNV-1a, 64-bit, because a hash written out in four lines is cheaper than
/// this crate's first dependency and the thing being detected is a file that
/// changed, not an adversary choosing collisions.
fn hash_of(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Every path under `root`, `root` itself included, keyed by its path
/// relative to `root`.
///
/// Symbolic links are recorded and not followed, so that a link repointed at
/// something else shows up as the change it is. A directory this process
/// cannot list is recorded as one and not descended into — and a directory
/// this process cannot enter is one the code under test cannot enter either,
/// since both run as the same user.
fn snapshot(root: &Path) -> BTreeMap<String, Entry> {
    let mut found = BTreeMap::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(path) = todo.pop() {
        let meta = path
            .symlink_metadata()
            .expect("a path this test just built");
        let file_type = meta.file_type();
        let mut kind = if file_type.is_symlink() {
            "symlink"
        } else if file_type.is_dir() {
            "directory"
        } else {
            "file"
        };
        let mut hash = None;
        if kind == "directory" {
            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    for entry in entries {
                        todo.push(
                            entry
                                .expect("an entry of a directory this test built")
                                .path(),
                        );
                    }
                }
                Err(_) => kind = "unlistable directory",
            }
        } else if kind == "file" {
            hash = std::fs::read(&path).ok().map(|bytes| hash_of(&bytes));
        }
        let under = path
            .strip_prefix(root)
            .expect("a path found beneath the root")
            .to_string_lossy()
            .into_owned();
        let under = if under.is_empty() {
            ".".to_string()
        } else {
            under
        };
        found.insert(
            under,
            Entry {
                kind,
                len: meta.len(),
                modified_nanos: meta.modified().ok().and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_nanos())
                }),
                mode: meta.permissions().mode(),
                hash,
            },
        );
    }
    found
}

/// What sits directly inside `dir`, for a directory this test watches and
/// does not own.
///
/// One level down and no recursion: one of the directories this is asked
/// about holds cargo's whole output tree, which is enormous, forever in
/// motion, and none of this test's business. A file directly inside is
/// recorded the way
/// [`snapshot`] records one, so a file rewritten on every run is caught on
/// every run and not only the first. A directory or a link is recorded under
/// its kind and its permissions and nothing more: a directory's length and
/// modification time move whenever cargo puts anything anywhere inside it,
/// and this test is not here to report on cargo.
fn shallow(dir: &Path) -> BTreeMap<String, Entry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return BTreeMap::new();
    };
    let mut found = BTreeMap::new();
    for entry in entries.flatten() {
        let at = entry.path();
        let Ok(meta) = at.symlink_metadata() else {
            continue;
        };
        let file_type = meta.file_type();
        let record = if file_type.is_file() {
            Entry {
                kind: "file",
                len: meta.len(),
                modified_nanos: meta.modified().ok().and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_nanos())
                }),
                mode: meta.permissions().mode(),
                hash: std::fs::read(&at).ok().map(|bytes| hash_of(&bytes)),
            }
        } else {
            Entry {
                kind: if file_type.is_symlink() {
                    "symlink"
                } else {
                    "directory"
                },
                len: 0,
                modified_nanos: None,
                mode: meta.permissions().mode(),
                hash: None,
            }
        };
        found.insert(entry.file_name().to_string_lossy().into_owned(), record);
    }
    found
}

/// What two records of the same place disagree about, worded for the failure
/// message and prefixed with where they were taken.
fn differences(
    under: &str,
    before: &BTreeMap<String, Entry>,
    after: &BTreeMap<String, Entry>,
) -> Vec<String> {
    before
        .iter()
        .filter(|(path, entry)| after.get(*path) != Some(*entry))
        .map(|(path, entry)| format!("{under}{path}: {entry:?} became {:?}", after.get(path)))
        .chain(
            after
                .keys()
                .filter(|path| !before.contains_key(*path))
                .map(|path| format!("{under}{path}: appeared")),
        )
        .collect()
}

/// The directories watched one level down instead of being snapshotted
/// whole: the one a test binary is started in, and the one it was built into.
///
/// Neither is this test's to record path by path. The build directory is
/// cargo's and holds cargo's whole output tree; the working directory is the
/// crate root cargo starts a test binary in. So each is read by [`shallow`],
/// and that is the whole of what
/// they give: something directly inside either of them that appeared during
/// the run, or was rewritten during it.
///
/// The build directory is here because a write to it passed everything this
/// crate had. A helper called from a reader wrote a log of every directory
/// the crate had visited into the build directory, under a type alias for
/// [`std::fs::File`] that spelled none of the names the scan in `lib.rs`
/// looks for — and the scan read the file, found nothing to object to, and
/// the suite stayed green. Watching the build directory ends that on its own,
/// without the scan having to grow another spelling.
///
/// Recording the file rather than the name is the other half of it, and it is
/// not decoration. This crate's own unit tests run before this file does and
/// go through the same readers, so a log a reader rewrites on every call is
/// already sitting in the build directory by the time this test takes its
/// first record, and a check that asked only whether the name was new would
/// see nothing. Cargo runs one test binary at a time, so nothing else is
/// writing there while this one runs.
fn watched_outside() -> Vec<PathBuf> {
    let mut watched = vec![std::env::current_dir().expect("a working directory")];
    // Cargo sets this when it compiles an integration test, to `tmp` inside
    // the build directory — which is how the build directory is named here
    // without this test having to believe it is called `target`.
    if let Some(build) = Path::new(env!("CARGO_TARGET_TMPDIR")).parent() {
        watched.push(build.to_path_buf());
    }
    watched
}

/// A scratch directory of this process's own, removed when this value is
/// dropped.
///
/// The fixture goes in a directory *inside* this one, so that a write that
/// lands beside the fixture rather than in it — `root.with_extension(…)`,
/// `root.parent()` — is inside the snapshot instead of outside it.
struct Scratch(PathBuf);

impl Scratch {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // The fixture seals a directory against this process on purpose, and
        // a sealed directory cannot be removed while it is sealed.
        unseal(&self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Give this process back the right to enter every directory under `path`.
fn unseal(path: &Path) {
    let Ok(meta) = path.symlink_metadata() else {
        return;
    };
    if !meta.file_type().is_dir() {
        return;
    }
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        unseal(&entry.path());
    }
}

/// A directory of this process's own, named so that two of them cannot
/// collide even when the suite runs its tests at once.
///
/// No `$HOME` and no fixed path: CI runs this as root, and this crate's whole
/// subject is other people's files.
fn scratch(what: &str) -> Scratch {
    let path = std::env::temp_dir().join(format!(
        "tobii-gameconf-writes-nothing-{}-{}-{what}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&path).expect("a scratch directory");
    Scratch(path)
}

/// One Elite-style preset document, written the way the real files are: a
/// byte-order mark, CRLF throughout, tabs for indentation.
fn preset(name: &str, setting_value: &str) -> Vec<u8> {
    format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n\
         <Root PresetName=\"{name}\" MajorVersion=\"1\" MinorVersion=\"8\">\r\n\
         \t<HeadlookMode Value=\"{setting_value}\" />\r\n\
         \t<HeadLookPitchAxisRaw>\r\n\
         \t\t<Binding Device=\"{{NoDevice}}\" Key=\"\" />\r\n\
         \t</HeadLookPitchAxisRaw>\r\n\
         </Root>\r\n"
    )
    .into_bytes()
}

/// Every directory [`binds::read`] is run over, one per branch it can take.
///
/// They cannot share a directory. A directory where two files claim the name
/// the start file names answers `Rejected` for that name and never reaches
/// the branch that reads a preset; a directory whose start file cannot be
/// read answers before it looks at a preset at all; a directory holding a
/// `.binds` name this reader cannot read back is refused whole, before any
/// preset is looked up at all. One shape each is the only arrangement in
/// which every branch runs.
///
/// The last four are not preset directories: a directory sealed against this
/// process, a directory named like an attributes file, an attributes file
/// where a directory was expected, and a path that is not there.
///
/// Two of them are the two shapes with no start file, and they are two
/// entries and not one because they are two answers: `shipped-presets` holds
/// documents a game ships and is read, `unwritten-presets` holds none and is
/// the absence. A fixture with only the first would never reach the absence;
/// one with only the second would never reach the reading.
const DIRECTORIES: [&str; 15] = [
    "presets",
    "trap-presets",
    "read-presets",
    "broken-presets",
    "shipped-presets",
    "unwritten-presets",
    "nameless-presets",
    "unnamed-preset-presets",
    "two-start-presets",
    "unreadable-start-presets",
    "other-case-presets",
    "sealed-presets",
    "directory.xml",
    "attributes.xml",
    "never-written",
];

/// The tree every entry point below is run over.
///
/// Every shape this crate's readers are known to meet, answers and refusals
/// together: a preset directory a game has written, one whose active preset
/// is missing, one a game has never written, one nothing here can list, a
/// flat attribute list, documents that are malformed in each of the ways the
/// scanner refuses, a file that is not UTF-8, a symbolic link, and names that
/// look like the real ones while being the wrong kind of thing.
fn fixture(root: &Path) {
    let write = |path: PathBuf, bytes: &[u8]| {
        std::fs::write(&path, bytes).expect("a fixture file");
        path
    };
    let dir = |path: PathBuf| {
        std::fs::create_dir_all(&path).expect("a fixture directory");
        path
    };

    // A preset directory where two files claim the name the start file names,
    // which is the one thing this reader will not pick between — and, in the
    // same start file, a third name that no file here carries, which is the
    // only shape in the tree that reaches the branch for a preset that is
    // simply not on disk.
    let presets = dir(root.join("presets"));
    write(
        presets.join("StartPreset.4.start"),
        b"\xef\xbb\xbfCustom\r\nCustom\r\nNotOnDisk\r\n",
    );
    write(presets.join("StartPreset.start"), b"Older\n");
    write(presets.join("Custom.binds"), &preset("Custom", "1"));
    write(presets.join("Spare.binds"), &preset("Spare", "0"));
    write(presets.join("Empty.binds"), &preset("Custom", ""));
    // A directory whose name ends `.binds`. It is a preset name this reader
    // cannot read back, so it refuses the directory it sits in rather than
    // skipping it — which is why it has a directory of its own: in `presets`
    // above it would refuse that one before a single preset was looked up.
    let trap = dir(root.join("trap-presets"));
    write(trap.join("StartPreset.start"), b"Custom\n");
    write(trap.join("Custom.binds"), &preset("Custom", "1"));
    dir(trap.join("Trap.binds"));

    // The ordinary directory: a start file naming one preset, and one file
    // that calls itself that. This is the only shape that reaches the branch
    // where a preset is actually read, which is the branch the whole crate
    // exists for.
    let read = dir(root.join("read-presets"));
    write(read.join("StartPreset.4.start"), b"\xef\xbb\xbfOnly\r\n");
    write(read.join("StartPreset.start"), b"Only\n");
    write(read.join("Only.binds"), &preset("Only", "1"));
    write(read.join("Spare.binds"), &preset("Spare", "0"));

    // A preset directory holding a document the scanner refuses, so that
    // `binds::read` takes its rejection path over a real directory.
    let broken = dir(root.join("broken-presets"));
    write(broken.join("StartPreset.start"), b"Custom\n");
    write(broken.join("Custom.binds"), &preset("Custom", "1"));
    write(
        broken.join("Truncated.binds"),
        b"<Root PresetName=\"T\"><A ",
    );
    write(broken.join("NotText.binds"), &[0xff, 0xfe, 0x00, 0x01]);

    // A directory a game ships and has never run in: presets, no start
    // file. Every one of them is reported, and none is in use — the shape
    // Elite Dangerous' `ControlSchemes` has, and the one `binds::read`
    // answered as an absence until 2026-09-28.
    let shipped = dir(root.join("shipped-presets"));
    write(shipped.join("Custom.binds"), &preset("Custom", "1"));
    write(shipped.join("Spare.binds"), &preset("Spare", "0"));
    write(shipped.join("Help.txt"), b"not a preset");

    // A directory holding no control scheme at all, saved or shipped, which
    // is the only shape with a real directory in front of it that is an
    // absence. It is the `Source::Unwritten` branch reached over something
    // this reader could list, where `never-written` below is the branch
    // reached over a path that is not there.
    let unwritten = dir(root.join("unwritten-presets"));
    write(unwritten.join("DeviceMappings.xml"), b"<Root/>");

    // A start file naming nothing a reader can use.
    let nameless = dir(root.join("nameless-presets"));
    write(nameless.join("StartPreset.start"), b"  \n\t\n");
    write(nameless.join("Custom.binds"), &preset("Custom", "1"));

    // A document that parses and calls itself nothing. It may be the preset
    // in use, so the directory is refused rather than read around it.
    let unnamed = dir(root.join("unnamed-preset-presets"));
    write(unnamed.join("StartPreset.start"), b"Custom\n");
    write(unnamed.join("Custom.binds"), &preset("Custom", "1"));
    write(
        unnamed.join("Anonymous.binds"),
        b"<Root MajorVersion=\"1\" MinorVersion=\"8\"/>",
    );

    // Two start files at one schema, which is no ordering at all: `.04.` is
    // the same number as `.4.`.
    let two_starts = dir(root.join("two-start-presets"));
    write(two_starts.join("StartPreset.4.start"), b"Custom\n");
    write(two_starts.join("StartPreset.04.start"), b"Custom\n");
    write(two_starts.join("Custom.binds"), &preset("Custom", "1"));

    // A start file that is listed and cannot be read, which is not a game
    // that never wrote one.
    let unreadable_start = dir(root.join("unreadable-start-presets"));
    dir(unreadable_start.join("StartPreset.start"));
    write(
        unreadable_start.join("Custom.binds"),
        &preset("Custom", "1"),
    );

    // A preset spelled the way the start file spells it but for its case,
    // which is neither a match nor an absence.
    let other_case = dir(root.join("other-case-presets"));
    write(other_case.join("StartPreset.start"), b"Custom\n");
    write(other_case.join("Custom.binds"), &preset("CUSTOM", "1"));

    // A flat attribute list, and the documents an attribute reader refuses.
    write(
        root.join("attributes.xml"),
        b"<Attributes Version=\"35\">\r\n\
          \x20<Attr name=\"HeadtrackingSource\" value=\"1\"/>\r\n\
          \x20<Attr name=\"HeadtrackingInactivityTime\" value=\"2\"/>\r\n\
          \x20<Attr name=\"Blank\" value=\"\"/>\r\n\
          </Attributes>\r\n",
    );
    write(
        root.join("truncated.xml"),
        b"<Attributes Version=\"35\"><Attr",
    );
    write(
        root.join("wrong-root.xml"),
        b"<Options><Attr name=\"A\" value=\"1\"/></Options>",
    );
    write(root.join("empty.xml"), b"");
    write(root.join("not-text.xml"), &[0xff, 0xfe, 0x00, 0x01]);

    // A path that is there and is the wrong kind of thing, which is the one
    // failure a reader is most likely to meet by accident.
    dir(root.join("directory.xml"));

    std::os::unix::fs::symlink("attributes.xml", root.join("linked.xml")).expect("a symbolic link");

    // Sealed against this process — except as root, where nothing is sealed.
    // Either way the snapshot and the code under test see the same thing.
    let sealed_dir = dir(root.join("sealed-presets"));
    write(sealed_dir.join("StartPreset.start"), b"Custom\n");
    let sealed_file = write(root.join("sealed.xml"), b"<Attributes Version=\"35\"/>");
    std::fs::set_permissions(&sealed_file, std::fs::Permissions::from_mode(0o000))
        .expect("sealing a file");
    std::fs::set_permissions(&sealed_dir, std::fs::Permissions::from_mode(0o000))
        .expect("sealing a directory");
}

/// Which cases an entry point answered with, so that a run that quietly
/// stopped exercising something fails instead of passing.
///
/// Three levels, because a count that stopped at the outermost one is
/// satisfied by a reader that got as far as opening a directory: `Source`
/// says whether the directory was read at all, `Found` says what became of
/// each preset the start file named, and `Lookup` says what one document held.
///
/// `selected` and `unselected` split the outermost level's `read` once more.
/// A preset directory is read down two different paths depending on whether
/// anything in it chooses a preset, and a fixture that stopped holding one of
/// the two shapes would leave that path — and any write planted in it —
/// unrun while this count still said the reader had read something.
#[derive(Default)]
struct Seen {
    unwritten: usize,
    rejected: usize,
    read: usize,
    selected: usize,
    unselected: usize,
    absent: usize,
    text: usize,
    refused: usize,
    missing_preset: usize,
    unusable_preset: usize,
    read_preset: usize,
}

impl Seen {
    fn source<T>(&mut self, source: &Source<T>) {
        match source {
            Source::Unwritten(_) => self.unwritten += 1,
            Source::Rejected(_) => self.rejected += 1,
            Source::Read(_) => self.read += 1,
        }
    }

    fn found(&mut self, found: &binds::Found) {
        match found {
            binds::Found::Absent(_) => self.missing_preset += 1,
            binds::Found::Rejected(_) => self.unusable_preset += 1,
            binds::Found::Read { .. } => self.read_preset += 1,
        }
    }

    fn lookup(&mut self, lookup: &Lookup) {
        match lookup {
            Lookup::Absent => self.absent += 1,
            Lookup::Text(_) => self.text += 1,
            Lookup::Rejected(_) => self.refused += 1,
        }
    }
}

/// Every public entry point of this crate, run over `root` until each of them
/// has answered every way it can answer.
fn run_everything(root: &Path) -> Seen {
    let mut seen = Seen::default();

    // The directory reader, over every shape it distinguishes.
    for dir in DIRECTORIES {
        for setting in ["HeadlookMode", "NotASetting"] {
            let found = binds::read(&root.join(dir), setting);
            seen.source(&found);
            if let Source::Read(bindings) = &found {
                if bindings.start.is_some() {
                    seen.selected += 1;
                } else {
                    seen.unselected += 1;
                }
                for active in &bindings.presets {
                    seen.found(&active.found);
                    if let binds::Found::Read { preset, .. } = &active.found {
                        seen.lookup(&preset.name);
                        seen.lookup(&preset.setting);
                        seen.lookup(&preset.version);
                    }
                }
            }
        }
    }

    // The file reader, over every file in the tree and over one that is not
    // there at all.
    let mut files: Vec<PathBuf> = snapshot(root)
        .keys()
        .map(|under| root.join(under))
        .collect();
    files.push(root.join("never-written.xml"));
    for path in &files {
        for name in ["HeadtrackingSource", "Blank", "NotAnAttribute"] {
            let found = attrs::read(path, name);
            seen.source(&found);
            if let Source::Read(found) = &found {
                seen.lookup(&found.version);
                seen.lookup(&found.value);
            }
        }
    }

    // The byte-level readers, over the bytes of every file that can be read.
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        for name in ["HeadtrackingSource", "Blank", "NotAnAttribute"] {
            let found = attrs::parse(&bytes, name);
            seen.lookup(&found.version);
            seen.lookup(&found.value);
        }
        for setting in ["HeadlookMode", "NotASetting"] {
            let found = binds::parse(&bytes, setting);
            seen.lookup(&found.name);
            seen.lookup(&found.setting);
            seen.lookup(&found.version);
        }
        let _ = binds::start(&bytes);
    }

    seen
}

#[test]
fn every_entry_point_leaves_the_tree_byte_identical() {
    let scratch = scratch("tree");
    // Inside the scratch directory rather than being it, so that a write
    // aimed beside the fixture lands inside what is snapshotted.
    let root = scratch.path().join("tree");
    std::fs::create_dir_all(&root).expect("the fixture root");
    fixture(&root);

    let watched = watched_outside();
    let before = snapshot(scratch.path());
    let watched_before: Vec<BTreeMap<String, Entry>> = watched.iter().map(|d| shallow(d)).collect();
    let seen = run_everything(&root);
    let after = snapshot(scratch.path());
    let watched_after: Vec<BTreeMap<String, Entry>> = watched.iter().map(|d| shallow(d)).collect();

    // A snapshot that matched because nothing ran would be the same shape of
    // hole as every one found in the name scan: a check reading less than it
    // looks like it is reading, and passing quietly for it.
    assert!(
        seen.unwritten > 0 && seen.rejected > 0 && seen.read > 0,
        "the readers did not reach all three answers, so this tree did not \
         exercise what it is here to exercise"
    );
    assert!(
        seen.selected > 0 && seen.unselected > 0,
        "the directory reader did not reach both shapes of preset directory — \
         one a game has run in and one holding only the presets it ships — so \
         one of the two paths through it went unrun"
    );
    assert!(
        seen.missing_preset > 0 && seen.unusable_preset > 0 && seen.read_preset > 0,
        "the preset lookup did not reach all three answers, so the branch that \
         reads a preset — the one this crate exists for — never ran"
    );
    assert!(
        seen.absent > 0 && seen.text > 0 && seen.refused > 0,
        "the parsers did not reach all three answers, so this tree did not \
         exercise what it is here to exercise"
    );

    let mut changed = differences("", &before, &after);
    for ((dir, before), after) in watched.iter().zip(&watched_before).zip(&watched_after) {
        changed.extend(differences(&format!("{}/", dir.display()), before, after));
    }

    assert!(
        changed.is_empty(),
        "this crate read a tree and something changed: {}",
        changed.join("; ")
    );
}
