//! Point the two readers at real files, which is the only way to find out
//! whether they read real files.
//!
//! ```text
//! cargo run -p tobii-gameconf --example read -- binds <directory> <Element>
//! cargo run -p tobii-gameconf --example read -- attrs <file> <name>
//! cargo run -p tobii-gameconf --example read -- presets <directory> <Element>
//! ```
//!
//! `presets` is the one that does not go through a `StartPreset` file: it
//! reads every `.binds` document in a directory on its own, which is what the
//! presets a game *ships* look like — there is no active one among them. It
//! also counts the writing habits `tobii_gameconf::binds` states a census of:
//! byte-order marks, bare line feeds, the spacing of the declaration and the
//! lines indented with spaces where the rest of the file uses tabs. `attrs`
//! counts what `tobii_gameconf::attrs` states about the file it was read off:
//! its size and how many `<Attr>` elements are in it. Those censuses are
//! measurements of somebody's install, and a measurement that cannot be taken
//! again is a number nobody can check.
//!
//! Nothing here writes, and nothing here knows a game: every path and every
//! setting name comes off the command line.

use std::path::{Path, PathBuf};
use tobii_gameconf::{attrs, binds, Lookup, Source};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [what, path, name] = args.as_slice() else {
        eprintln!("usage: read <binds|attrs|presets> <path> <setting>");
        std::process::exit(2);
    };
    let path = Path::new(path);
    match what.as_str() {
        "binds" => bindings(path, name),
        "attrs" => attributes(path, name),
        "presets" => presets(path, name),
        other => {
            eprintln!("no such reader: {other}");
            std::process::exit(2);
        }
    }
}

fn say(label: &str, lookup: &Lookup) {
    match lookup {
        Lookup::Absent => println!("  {label}: not in the file"),
        Lookup::Text(text) => println!("  {label}: {text}"),
        Lookup::Rejected(why) => println!("  {label}: could not be read — {why}"),
    }
}

fn bindings(dir: &Path, setting: &str) {
    match binds::read(dir, setting) {
        Source::Unwritten(why) => println!("nothing to read: {why}"),
        Source::Rejected(why) => println!("could not be read: {why}"),
        Source::Read(b) => {
            println!("start file: {}", b.start.display());
            println!("schema in its name: {:?}", b.schema);
            for older in &b.superseded {
                println!("superseded start file: {}", older.display());
            }
            for active in &b.presets {
                println!("preset {}", active.name);
                match &active.found {
                    binds::Found::Absent(why) => println!("  no file: {why}"),
                    binds::Found::Rejected(why) => {
                        println!("  could not be placed: {why}")
                    }
                    binds::Found::Read { file, preset } => {
                        println!("  file: {}", file.display());
                        say("version", &preset.version);
                        say(setting, &preset.setting);
                    }
                }
            }
        }
    }
}

fn attributes(file: &Path, name: &str) {
    match attrs::read(file, name) {
        Source::Unwritten(why) => println!("nothing to read: {why}"),
        Source::Rejected(why) => println!("could not be read: {why}"),
        Source::Read(a) => {
            println!("{}", file.display());
            say("version", &a.version);
            say(name, &a.value);
            census(file);
        }
    }
}

/// The two numbers `tobii_gameconf::attrs` states about the real file, taken
/// again off whatever file this was pointed at.
///
/// Counted off the bytes rather than asked of the reader, which is what makes
/// it a second opinion: `attrs` answers about one attribute and has no reason
/// to hand back a population. A start tag inside a comment would be counted
/// here and not by a parser — the file this was measured on holds no comment,
/// and a count that disagrees with the reader is a thing worth seeing rather
/// than a thing to hide.
fn census(file: &Path) {
    let Ok(bytes) = std::fs::read(file) else {
        return;
    };
    const PAIR: &[u8] = b"<Attr ";
    let elements = bytes.windows(PAIR.len()).filter(|w| *w == PAIR).count();
    println!("  {} bytes, {elements} <Attr> elements", bytes.len());
}

fn presets(dir: &Path, setting: &str) {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries.flatten().map(|e| e.path()).collect(),
        Err(e) => {
            println!("could not be listed: {e}");
            return;
        }
    };
    files.sort();
    let mut seen = 0;
    let mut marked = 0;
    let mut mixed = 0;
    let mut spaced = 0;
    let mut space_indented = 0;
    for file in files {
        if file
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case("binds"))
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(&file) else {
            println!("{}: could not be read", file.display());
            continue;
        };
        seen += 1;
        let body = match bytes.strip_prefix(b"\xef\xbb\xbf") {
            Some(body) => {
                marked += 1;
                body
            }
            None => &bytes,
        };
        // Bare line feeds among the carriage returns, which is what "mixed"
        // means: a file written by two things, or by one that changed.
        let bare = bytes
            .windows(2)
            .filter(|w| w[1] == b'\n' && w[0] != b'\r')
            .count()
            + usize::from(bytes.first() == Some(&b'\n'));
        if bare > 0 {
            mixed += 1;
        }
        // Lines indented with spaces in a file that is otherwise tabs. Counted
        // per line rather than per file, because what the census states is
        // both: which files have the habit, and how far it goes in them.
        let spaces = bytes
            .split(|b| *b == b'\n')
            .filter(|line| line.first() == Some(&b' '))
            .count();
        if spaces > 0 {
            space_indented += 1;
        }
        if body.starts_with(b"<?xml") {
            if let Some(end) = body.windows(2).position(|w| w == b"?>") {
                if body[..end].ends_with(b" ") {
                    spaced += 1;
                }
            }
        }
        let preset = binds::parse(&bytes, setting);
        println!("{}", file.display());
        say("name", &preset.name);
        say("version", &preset.version);
        say(setting, &preset.setting);
        if bare > 0 {
            println!("  {bare} bare line feeds");
        }
        if spaces > 0 {
            println!("  {spaces} lines indented with spaces");
        }
    }
    println!("{seen} preset documents");
    println!(
        "  {marked} with a byte-order mark, {} without",
        seen - marked
    );
    println!(
        "  {mixed} mixed line endings, {} CRLF throughout",
        seen - mixed
    );
    println!("  {spaced} writing a space before `?>`");
    println!(
        "  {space_indented} with lines indented with spaces, {} tabs throughout",
        seen - space_indented
    );
}
