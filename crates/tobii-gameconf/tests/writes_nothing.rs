//! Whether this crate writes, asked of a tree it has just been run over.
//!
//! This is the check the crate's promise rests on. It builds a directory
//! shaped like the real data, records what every path in it looks like, runs
//! every public entry point of the crate over it — the answers and the
//! refusals both — and records the tree again. The two records must be equal.
//!
//! What that catches is a write, however it is spelled. A grouped import, a
//! renamed one, a macro that assembles the call out of fragments, a helper in
//! a module nobody thought to scan, a name added to `std` after anybody here
//! stopped looking: none of them changes the fact that a file that was
//! modified has a different length, or a different modification time, or
//! different bytes. That is the whole reason this test exists beside the
//! name scan in `lib.rs`, which can only ever find what somebody spelled out
//! in advance.
//!
//! It is an integration test on purpose: from out here the only things
//! reachable are the ones a caller can reach, so what it runs is the crate
//! as somebody using it gets it. That the list below is *all* of them is a
//! separate matter, and not something this file can show — the compiler
//! stops it reaching further, and lets it reach less. `lib.rs`'s
//! `the_behavioural_test_calls_every_public_function` is what checks it.
//!
//! # What it does not cover
//!
//! Three things, said plainly because the point of this file is to be the
//! thing that does not promise more than it delivers.
//!
//! * **Paths the fixture does not contain.** The tree below is the shapes
//!   this crate's readers are known to meet. A write to somewhere else
//!   entirely — a log file in `$HOME`, a path built from an environment
//!   variable — is outside the snapshot and invisible here. The name scan is
//!   what looks for that, and it looks by spelling.
//! * **Code that runs before this test does.** A `build.rs` has already run
//!   by the time a test binary starts, so nothing here can see what it did.
//!   That one is the name scan's, which walks every file cargo compiles,
//!   `build.rs` included, and fails if one ships unscanned.
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

/// A directory of this process's own, named so that two of them cannot
/// collide even when the suite runs its tests at once.
///
/// No `$HOME` and no fixed path: CI runs this as root, and this crate's whole
/// subject is other people's files.
fn scratch(what: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "tobii-gameconf-writes-nothing-{}-{}-{what}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&path).expect("a scratch directory");
    path
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

    // A preset directory a game has written and this crate can read whole.
    let presets = dir(root.join("presets"));
    write(
        presets.join("StartPreset.4.start"),
        b"\xef\xbb\xbfCustom\r\nCustom\r\nNotOnDisk\r\n",
    );
    write(presets.join("StartPreset.start"), b"Older\n");
    write(presets.join("Custom.binds"), &preset("Custom", "1"));
    write(presets.join("Spare.binds"), &preset("Spare", "0"));
    write(presets.join("Empty.binds"), &preset("Custom", ""));
    // A directory whose name ends `.binds`, which is not a preset however
    // much it reads like one.
    dir(presets.join("Trap.binds"));

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

    // A directory a game ships and has never run in: presets, no start file.
    let unwritten = dir(root.join("unwritten-presets"));
    write(unwritten.join("Custom.binds"), &preset("Custom", "1"));

    // A start file naming nothing a reader can use.
    let nameless = dir(root.join("nameless-presets"));
    write(nameless.join("StartPreset.start"), b"  \n\t\n");
    write(nameless.join("Custom.binds"), &preset("Custom", "1"));

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
#[derive(Default)]
struct Seen {
    unwritten: usize,
    rejected: usize,
    read: usize,
    absent: usize,
    text: usize,
    refused: usize,
}

impl Seen {
    fn source<T>(&mut self, source: &Source<T>) {
        match source {
            Source::Unwritten(_) => self.unwritten += 1,
            Source::Rejected(_) => self.rejected += 1,
            Source::Read(_) => self.read += 1,
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

    // The directory readers, over every directory in the tree and over one
    // that is not there at all.
    for dir in [
        "presets",
        "broken-presets",
        "unwritten-presets",
        "nameless-presets",
        "sealed-presets",
        "directory.xml",
        "attributes.xml",
        "never-written",
    ] {
        for setting in ["HeadlookMode", "NotASetting"] {
            let found = binds::read(&root.join(dir), setting);
            seen.source(&found);
            if let Source::Read(bindings) = &found {
                for active in &bindings.presets {
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
    let root = scratch("tree");
    fixture(&root);

    let before = snapshot(&root);
    let seen = run_everything(&root);
    let after = snapshot(&root);

    // A snapshot that matched because nothing ran would be the same shape of
    // hole as every one found in the name scan: a check reading less than it
    // looks like it is reading, and passing quietly for it.
    assert!(
        seen.unwritten > 0 && seen.rejected > 0 && seen.read > 0,
        "the readers did not reach all three answers, so this tree did not \
         exercise what it is here to exercise"
    );
    assert!(
        seen.absent > 0 && seen.text > 0 && seen.refused > 0,
        "the parsers did not reach all three answers, so this tree did not \
         exercise what it is here to exercise"
    );

    let changed: Vec<String> = before
        .iter()
        .filter(|(path, entry)| after.get(*path) != Some(*entry))
        .map(|(path, entry)| format!("{path}: {entry:?} became {:?}", after.get(path)))
        .chain(
            after
                .keys()
                .filter(|path| !before.contains_key(*path))
                .map(|path| format!("{path}: appeared")),
        )
        .collect();

    // Unsealed before the assertion, so that a failure can still be cleaned
    // up and a passing run leaves nothing behind either way.
    let _ = std::fs::set_permissions(
        root.join("sealed-presets"),
        std::fs::Permissions::from_mode(0o700),
    );
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        changed.is_empty(),
        "this crate read a tree and the tree changed: {}",
        changed.join("; ")
    );
}
