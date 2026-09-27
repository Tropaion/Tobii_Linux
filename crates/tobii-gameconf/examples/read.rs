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
//! 30 presets a game *ships* look like — there is no active one among them.
//! It exists because that is the census the crate docs make a claim about.
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
                    binds::Found::Unmatched(why) => println!("  no file: {why}"),
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
        }
    }
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
        let preset = binds::parse(&bytes, setting);
        println!("{}", file.display());
        say("name", &preset.name);
        say("version", &preset.version);
        say(setting, &preset.setting);
    }
    println!("{seen} preset documents");
}
