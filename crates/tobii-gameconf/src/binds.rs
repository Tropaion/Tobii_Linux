//! Elite Dangerous-style control presets: a directory of `.binds` documents
//! and the `StartPreset` file that says which of them is live.
//!
//! # The format, as the game's own files have it
//!
//! Measured against the 30 presets Elite Dangerous ships, read off this
//! machine's install rather than inferred:
//!
//! ```text
//! <?xml version="1.0" encoding="utf-8"?>
//! <Root PresetName="KeyboardMouseOnly" SortOrder="0">
//!   <MouseHeadlook Value="1" />
//!   …
//!   <HeadlookMode Value="Bindings_HeadlookModeAccumulate" />
//!   <HeadLookPitchAxisRaw>
//!     <Binding Device="{NoDevice}" Key="" />
//!     <Deadzone Value="0.00000000" />
//!   </HeadLookPitchAxisRaw>
//! </Root>
//! ```
//!
//! * The document element is `Root` and carries `PresetName`. 26 of the 30
//!   files begin with a UTF-8 byte-order mark and 4 do not; all 30 use CRLF
//!   line endings and tab indentation, shown above with spaces only because a
//!   doc comment may not hold a tab. One writes its declaration as
//!   `encoding="UTF-8" ?>` where the rest write `encoding="utf-8"?>`.
//! * A setting is a direct child with a `Value` attribute. Bindings are direct
//!   children too, but they hold their own children instead — and those nest
//!   names like `Deadzone` and `Binding` that repeat dozens of times per file,
//!   which is why [`crate::xml`] only ever offers the first level.
//! * `MajorVersion` and `MinorVersion` appear on `Root` in 2 of the 30 shipped
//!   files (`SaitekX56` and `T16000MHOTAS`, both `1`.`8`) and not in the other
//!   28. The game's own saved presets carry the schema of the version that
//!   wrote them, which has moved over the game's life, so the numbers are read
//!   and reported rather than checked against anything.
//!
//! # Why this is worth reading at all
//!
//! `HeadlookMode` is the setting a head tracker turns on: with the tracker
//! feeding the joystick route, `Bindings_HeadlookModeAccumulate` makes the
//! view drift where `Bindings_HeadlookModeDirect` makes it turn. **All 30
//! presets the game ships say Accumulate** — checked on this machine, all 30,
//! not sampled — so this is not an unlikely corner: it is what every user
//! starts from, and the report exists to say so.
//!
//! Nothing in this module names that setting, or the game, or where its
//! options live. It is handed a directory and the name of a setting. Per-game
//! knowledge belongs in a profile, and this project ships none.
//!
//! # What was measured here and what was not
//!
//! The document format above was read off 30 real files. The directory
//! convention was not, and this is the honest limit of this module: the one
//! Elite Dangerous install on this machine **has never been launched**, so
//! there is no user `Options/Bindings` directory anywhere on it to read. What
//! that exercises — and it is the path a user most often hits — is
//! [`crate::Source::Unwritten`]: a game that has saved nothing must be
//! reported as exactly that, with a sentence, and not as a game whose settings
//! are fine.
//!
//! So [`start`] is written to refuse rather than to guess. It takes one preset
//! name per line and nothing else; a line it cannot read as a name is an
//! error, not a skipped line. And [`read`] does not guess a preset's *file
//! name* from its preset name — it opens every `.binds` file in the directory
//! and matches on the `PresetName` inside it, which is a fact that can be
//! checked against those 30 files today and does not depend on a naming
//! convention nobody here has seen.
//!
//! The one thing [`read`] does assume is stated where it is made: see
//! [`Bindings::superseded`].

use crate::{xml, Lookup, Source};
use std::path::{Path, PathBuf};

/// The document element every preset file has.
const ROOT: &str = "Root";

/// What one `.binds` document says.
///
/// Three [`Lookup`]s rather than three `Option`s: a preset file that cannot be
/// read is a different report from one that does not mention the setting, and
/// a document this reader refuses outright fills all three with the same
/// reason — which is the true answer to all three questions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    /// `PresetName` on the document element.
    pub name: Lookup,
    /// `MajorVersion`.`MinorVersion` on the document element, when it has
    /// them. Reported, never checked: see the module docs.
    pub version: Lookup,
    /// What the asked-for setting holds.
    pub setting: Lookup,
}

impl Preset {
    /// Every answer is the same refusal — for a document that could not be
    /// read at all.
    fn rejected(why: &str) -> Self {
        Preset {
            name: Lookup::Rejected(why.to_string()),
            version: Lookup::Rejected(why.to_string()),
            setting: Lookup::Rejected(why.to_string()),
        }
    }
}

/// Read one preset document, asking what `setting` holds in it.
///
/// `setting` is an element name: `HeadlookMode` names
/// `<HeadlookMode Value="…" />`.
pub fn parse(doc: &[u8], setting: &str) -> Preset {
    let document = match xml::parse(doc) {
        Ok(d) => d,
        Err(why) => return Preset::rejected(&why),
    };
    if document.root.name != ROOT {
        // Not a refusal about XML but about *which* document this is. A file
        // that parses and is not a preset would otherwise be reported as a
        // preset with nothing in it, which is the conflation this crate exists
        // to avoid.
        return Preset::rejected(&format!(
            "a file whose outermost element is <{}> and not <{ROOT}>",
            document.root.name
        ));
    }
    let found: Vec<&xml::Element> = document
        .children
        .iter()
        .filter(|e| e.name == setting)
        .collect();
    let value = match found.as_slice() {
        [] => Lookup::Absent,
        [one] => match one.attribute("Value") {
            // The element is there and says nothing this reader can use.
            // Absent would read as "the user has not set this", and the user
            // has: there is an element here with their setting in it.
            Lookup::Absent => Lookup::Rejected(format!("a <{setting}> with no Value attribute")),
            other => other,
        },
        many => Lookup::Rejected(format!(
            "<{setting}> {} times in one preset, and this reader cannot tell \
             which of them the game uses",
            many.len()
        )),
    };
    Preset {
        name: document.root.attribute("PresetName"),
        version: version(&document.root),
        setting: value,
    }
}

/// `MajorVersion`.`MinorVersion`, as the one string a report would print.
///
/// Joined rather than handed over as two numbers because nothing here compares
/// them: what a reader of the report needs is to see which schema they are
/// looking at when the next one arrives.
fn version(root: &xml::Element) -> Lookup {
    match (
        root.attribute("MajorVersion"),
        root.attribute("MinorVersion"),
    ) {
        (Lookup::Absent, Lookup::Absent) => Lookup::Absent,
        (Lookup::Text(major), Lookup::Text(minor)) => Lookup::Text(format!("{major}.{minor}")),
        // Half a version is not a version, and neither is one this reader
        // could not decode.
        (Lookup::Rejected(why), _) | (_, Lookup::Rejected(why)) => Lookup::Rejected(why),
        _ => Lookup::Rejected("a preset naming one half of a schema version".to_string()),
    }
}

/// The preset names a `StartPreset` file holds, in order, without repeats.
///
/// One name per line. Blank lines are skipped — a trailing newline is not a
/// preset — and everything else is refused: a line that is not a plain name
/// means this is not the file this reader thinks it is, and the answer to that
/// is a sentence, not a best guess at which line was meant.
///
/// A list rather than one name: newer versions of the game write several lines
/// here, one per category of binding, and which line governs which setting is
/// not something this project has been able to observe. Reporting every
/// distinct name it holds is the honest shape — when they are all the same
/// preset, which is the ordinary case, the report has one row.
pub fn start(text: &[u8]) -> Result<Vec<String>, String> {
    let text = text.strip_prefix(b"\xef\xbb\xbf").unwrap_or(text);
    let text = std::str::from_utf8(text)
        .map_err(|_| "a StartPreset file that is not UTF-8 text".to_string())?;
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let name = line.trim_matches(|c: char| c.is_ascii_whitespace());
        if name.is_empty() {
            continue;
        }
        if name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
        {
            return Err(format!(
                "a StartPreset file holding a line this reader cannot read \
                 as a preset name: {name:?}"
            ));
        }
        if !out.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            out.push(name.to_string());
        }
    }
    if out.is_empty() {
        return Err("a StartPreset file naming no preset at all".to_string());
    }
    Ok(out)
}

/// Whether `file_name` is a `StartPreset` file, and the schema in its name.
///
/// `StartPreset.4.start` is schema 4; `StartPreset.start` is the older
/// spelling with no schema in it at all, which is [`Some`]`(`[`None`]`)` —
/// "yes, and it does not say". Anything else is [`None`].
///
/// Compared ASCII-case-insensitively: this file is written by a Windows
/// program, under a prefix whose filesystem may or may not be case-sensitive,
/// and a reader that missed it because of a capital letter would report a
/// configured game as unconfigured.
fn start_schema(file_name: &str) -> Option<Option<u32>> {
    let rest = strip_prefix_ignore_ascii_case(file_name, "StartPreset")?;
    let rest = strip_suffix_ignore_ascii_case(rest, ".start")?;
    if rest.is_empty() {
        return Some(None);
    }
    rest.strip_prefix('.')?.parse::<u32>().ok().map(Some)
}

fn strip_prefix_ignore_ascii_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let (head, rest) = s.split_at_checked(prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then_some(rest)
}

fn strip_suffix_ignore_ascii_case<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    let (rest, tail) = s.split_at_checked(s.len().checked_sub(suffix.len())?)?;
    tail.eq_ignore_ascii_case(suffix).then_some(rest)
}

/// One preset the `StartPreset` file names, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    /// The name, spelled as the `StartPreset` file spells it.
    pub name: String,
    pub found: Found,
}

/// Whether the named preset turned out to be one readable file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// There is no one file in the directory that calls itself this.
    Unmatched(String),
    /// The file whose `PresetName` is this name, and what it says.
    Read { file: PathBuf, preset: Preset },
}

/// A directory of presets, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    /// The `StartPreset` file this reader took as the current one.
    pub start: PathBuf,
    /// The schema number in that file's name, when it has one.
    pub schema: Option<u32>,
    /// The other `StartPreset` files in the directory, lower-numbered, that
    /// were not read.
    ///
    /// Exposed because it is the one assumption this module makes that it
    /// could not measure: a game that has been upgraded leaves the start file
    /// of the older schema next to the new one, and the reader takes the
    /// highest number as the current one. The ordering is real — the number is
    /// the bindings schema, and it only goes up — but a user who has *rolled
    /// back* their game would be running the lower one. A report that lists
    /// this field lets them see that, instead of being told a version number
    /// with no way to tell it is the wrong one.
    pub superseded: Vec<PathBuf>,
    /// One per distinct name the start file holds, in its order.
    pub presets: Vec<Active>,
}

/// Read the presets in `dir`, asking each active one what `setting` holds.
///
/// The three answers are [`crate::Source`]'s and they mean what it says:
/// nothing has been written here, something here cannot be read, or here is
/// what the files say.
///
/// Every `.binds` file in the directory is opened, because a preset is matched
/// by the `PresetName` inside it. That makes one file that cannot be read a
/// refusal for the whole directory: an unreadable file may be the one that
/// calls itself the active preset, or a second file that also does, and
/// reporting a value from the readable one would be reporting a preset this
/// reader cannot show is the one in use. The refusal names the file.
pub fn read(dir: &Path, setting: &str) -> Source<Bindings> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Source::Unwritten(format!(
                "there is no directory at {}: nothing has saved a control scheme here",
                dir.display()
            ))
        }
        Err(e) => {
            return Source::Rejected(format!("{} could not be listed: {e}", dir.display()));
        }
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        match entry {
            // Sorted below rather than trusted: `read_dir` yields whatever
            // order the filesystem holds, and a report whose wording depends
            // on that is a report that cannot be reproduced.
            Ok(entry) => files.push(entry.path()),
            Err(e) => {
                return Source::Rejected(format!("{} could not be listed: {e}", dir.display()))
            }
        }
    }
    files.sort();
    let mut starts: Vec<(Option<u32>, PathBuf)> = Vec::new();
    let mut presets: Vec<PathBuf> = Vec::new();
    for path in files {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(schema) = start_schema(name) {
            starts.push((schema, path));
        } else if strip_suffix_ignore_ascii_case(name, ".binds").is_some() && path.is_file() {
            presets.push(path);
        }
    }
    // Highest schema last, and `None` — the spelling with no number — lowest,
    // which is what `Option`'s own ordering already says.
    starts.sort();
    let Some((schema, start_file)) = starts.pop() else {
        return Source::Unwritten(format!(
            "{} holds no StartPreset file: nothing has saved a control scheme here",
            dir.display()
        ));
    };
    let bytes = match crate::read(&start_file) {
        Source::Read(b) => b,
        // A file that was listed a moment ago and is gone now is not a game
        // that never wrote one.
        Source::Unwritten(why) | Source::Rejected(why) => return Source::Rejected(why),
    };
    let names = match start(&bytes) {
        Ok(n) => n,
        Err(why) => return Source::Rejected(format!("{}: {why}", start_file.display())),
    };
    let mut read_presets: Vec<(PathBuf, Preset)> = Vec::new();
    for path in presets {
        let bytes = match crate::read(&path) {
            Source::Read(b) => b,
            Source::Unwritten(why) | Source::Rejected(why) => return Source::Rejected(why),
        };
        let preset = parse(&bytes, setting);
        match &preset.name {
            Lookup::Text(_) => read_presets.push((path, preset)),
            // Both of these are files this reader cannot place. See the
            // function docs: one of them may be the active preset.
            Lookup::Absent => {
                return Source::Rejected(format!(
                    "{} is a preset with no PresetName, so this reader cannot \
                     tell whether it is the one in use",
                    path.display()
                ))
            }
            Lookup::Rejected(why) => {
                return Source::Rejected(format!("{} is {why}", path.display()))
            }
        }
    }
    let presets = names
        .into_iter()
        .map(|name| {
            let matches: Vec<&(PathBuf, Preset)> = read_presets
                .iter()
                .filter(
                    |(_, p)| matches!(&p.name, Lookup::Text(n) if n.eq_ignore_ascii_case(&name)),
                )
                .collect();
            let found = match matches.as_slice() {
                [] => Found::Unmatched(format!(
                    "no preset in {} calls itself {name}",
                    dir.display()
                )),
                [(file, preset)] => Found::Read {
                    file: file.clone(),
                    preset: preset.clone(),
                },
                many => Found::Unmatched(format!(
                    "{} presets in {} call themselves {name}, and this reader \
                     cannot tell which the game loads",
                    many.len(),
                    dir.display()
                )),
            };
            Active { name, found }
        })
        .collect();
    Source::Read(Bindings {
        start: start_file,
        schema,
        superseded: starts.into_iter().map(|(_, p)| p).collect(),
        presets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::scratch;

    /// A preset document in the shape the shipped files have: BOM, CRLF, tabs,
    /// a declaration, bindings nested one deeper than the settings.
    fn preset_file(name: &str, extra: &str) -> Vec<u8> {
        let mut out = b"\xef\xbb\xbf".to_vec();
        out.extend_from_slice(
            format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n\
                 <Root PresetName=\"{name}\" SortOrder=\"0\">\r\n\
                 \t<MouseHeadlook Value=\"1\" />\r\n\
                 \t<HeadlookMode Value=\"Bindings_HeadlookModeAccumulate\" />\r\n\
                 \t<HeadLookPitchAxisRaw>\r\n\
                 \t\t<Binding Device=\"{{NoDevice}}\" Key=\"\" />\r\n\
                 \t\t<Deadzone Value=\"0.00000000\" />\r\n\
                 \t</HeadLookPitchAxisRaw>\r\n\
                 {extra}</Root>\r\n"
            )
            .as_bytes(),
        );
        out
    }

    fn dir_with(what: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let dir = scratch(what);
        for (name, bytes) in files {
            std::fs::write(dir.join(name), bytes).expect("fixture");
        }
        dir
    }

    /// The reading the whole crate exists for, on a document shaped like the
    /// ones the game ships.
    #[test]
    fn reads_a_setting_off_a_preset() {
        let p = parse(&preset_file("Custom", ""), "HeadlookMode");
        assert_eq!(p.name, Lookup::Text("Custom".into()));
        assert_eq!(
            p.setting,
            Lookup::Text("Bindings_HeadlookModeAccumulate".into())
        );
        assert_eq!(p.version, Lookup::Absent, "28 of 30 shipped files say none");
    }

    /// `Deadzone` is a real element name in these files, nested inside every
    /// axis binding. Asking for it must not find those.
    #[test]
    fn a_name_that_only_occurs_nested_is_absent_not_found() {
        let p = parse(&preset_file("Custom", ""), "Deadzone");
        assert_eq!(p.setting, Lookup::Absent);
    }

    /// The version is reported, never checked — the schema moves, and a reader
    /// that refused an unfamiliar number would break on the next patch.
    #[test]
    fn a_schema_version_is_read_and_not_judged() {
        let doc =
            b"<Root PresetName=\"C\" MajorVersion=\"4\" MinorVersion=\"1\"><A Value=\"x\"/></Root>";
        let p = parse(doc, "A");
        assert_eq!(p.version, Lookup::Text("4.1".into()));
        assert_eq!(p.setting, Lookup::Text("x".into()));
        let half = b"<Root PresetName=\"C\" MajorVersion=\"4\"/>";
        assert!(matches!(parse(half, "A").version, Lookup::Rejected(_)));
    }

    /// The rule the crate is built on, at the level of one document: a file
    /// that cannot be read is never a file that says nothing.
    #[test]
    fn an_unreadable_document_is_refused_and_not_reported_empty() {
        for (bad, what) in [
            (&b"<Root PresetName=\"C\"><A Value=\"1\"/>"[..], "unclosed"),
            (
                &b"<Options PresetName=\"C\"><A Value=\"1\"/></Options>"[..],
                "not a preset",
            ),
            (&b"\xff\xfe<Root/>"[..], "not UTF-8"),
        ] {
            let p = parse(bad, "A");
            assert!(
                matches!(p.setting, Lookup::Rejected(_)) && matches!(p.name, Lookup::Rejected(_)),
                "{what} should be refused, got {p:?}"
            );
        }
        // Present, and holding nothing this reader can use: still not absent.
        let p = parse(b"<Root PresetName=\"C\"><A/></Root>", "A");
        assert!(matches!(p.setting, Lookup::Rejected(_)), "{p:?}");
        // Two of them, and no way to tell which the game reads.
        let p = parse(
            b"<Root PresetName=\"C\"><A Value=\"1\"/><A Value=\"2\"/></Root>",
            "A",
        );
        assert!(matches!(p.setting, Lookup::Rejected(_)), "{p:?}");
    }

    #[test]
    fn start_files_are_recognised_by_name_and_schema() {
        assert_eq!(start_schema("StartPreset.start"), Some(None));
        assert_eq!(start_schema("StartPreset.4.start"), Some(Some(4)));
        assert_eq!(start_schema("startpreset.4.START"), Some(Some(4)));
        for no in [
            "StartPreset",
            "StartPreset.4",
            "Custom.4.0.binds",
            "StartPreset.x.start",
            "NotStartPreset.start",
            "Start.start",
        ] {
            assert_eq!(start_schema(no), None, "{no}");
        }
    }

    #[test]
    fn a_start_file_holds_one_name_per_line() {
        assert_eq!(start(b"Custom\r\n").expect("reads"), ["Custom"]);
        assert_eq!(
            start(b"\xef\xbb\xbfCustom\r\nCustom\r\nOther\r\n\r\n").expect("reads"),
            ["Custom", "Other"],
            "repeats collapse, blank lines are not names"
        );
        assert!(start(b"\r\n\r\n").is_err(), "no name is not a name");
        // Next to a name this reader *can* read, so that what is being
        // guarded is the refusal and not the empty result behind it: a line
        // that is silently skipped leaves a file that still names a preset,
        // and a report built on it would be missing whatever that line said.
        assert!(
            start(b"Custom\r\n..\\..\\etc\r\n").is_err(),
            "a line that is not a preset name is not a line to skip"
        );
    }

    /// The path this machine can actually exercise: Elite Dangerous is
    /// installed here and has never been launched, so the options directory
    /// does not exist at all. It has to be its own answer, with a sentence.
    #[test]
    fn a_game_that_has_never_saved_anything_says_so() {
        let dir = scratch("never");
        let missing = dir.join("Bindings");
        match read(&missing, "HeadlookMode") {
            Source::Unwritten(why) => assert!(why.contains("Bindings"), "{why}"),
            other => panic!("a directory that is not there is not a reading: {other:?}"),
        }
        // The directory exists — a prefix can have it — and still holds no
        // control scheme. Same answer, different sentence.
        std::fs::create_dir_all(&missing).expect("fixture");
        std::fs::write(missing.join("Custom.4.0.binds"), preset_file("Custom", ""))
            .expect("fixture");
        match read(&missing, "HeadlookMode") {
            Source::Unwritten(why) => assert!(why.contains("StartPreset"), "{why}"),
            other => panic!("presets with no StartPreset name nothing active: {other:?}"),
        }
    }

    /// The whole directory reading, on the shape a launched game leaves.
    #[test]
    fn the_active_preset_is_the_one_the_start_file_names() {
        let dir = dir_with(
            "active",
            &[
                ("StartPreset.4.start", b"Custom\r\n"),
                ("Custom.4.0.binds", &preset_file("Custom", "")),
                ("Keyboard.4.0.binds", &preset_file("Keyboard", "")),
            ],
        );
        let Source::Read(b) = read(&dir, "HeadlookMode") else {
            panic!("should have read {}", dir.display());
        };
        assert_eq!(b.schema, Some(4));
        assert!(b.superseded.is_empty());
        assert_eq!(b.presets.len(), 1);
        assert_eq!(b.presets[0].name, "Custom");
        let Found::Read { file, preset } = &b.presets[0].found else {
            panic!("should have matched: {:?}", b.presets[0]);
        };
        assert_eq!(file, &dir.join("Custom.4.0.binds"));
        assert_eq!(
            preset.setting,
            Lookup::Text("Bindings_HeadlookModeAccumulate".into())
        );
    }

    /// A preset is matched on the `PresetName` inside the file, not on the
    /// file's name — the naming convention of a saved preset is something this
    /// project has never seen, and the attribute is something it has seen 30
    /// times.
    #[test]
    fn a_preset_is_matched_by_what_it_calls_itself() {
        let dir = dir_with(
            "byname",
            &[
                ("StartPreset.4.start", b"Custom\r\n"),
                ("whatever.binds", &preset_file("Custom", "")),
            ],
        );
        let Source::Read(b) = read(&dir, "HeadlookMode") else {
            panic!("should have read it");
        };
        let Found::Read { file, .. } = &b.presets[0].found else {
            panic!("should have matched: {:?}", b.presets[0]);
        };
        assert_eq!(file, &dir.join("whatever.binds"));
    }

    /// Two start files is what an upgraded game leaves behind. The higher
    /// schema is taken, and the other is *named* rather than silently dropped
    /// — see [`Bindings::superseded`].
    #[test]
    fn the_highest_schema_start_file_wins_and_the_rest_are_named() {
        let dir = dir_with(
            "upgraded",
            &[
                ("StartPreset.start", b"Old\r\n"),
                ("StartPreset.3.start", b"Older\r\n"),
                ("StartPreset.4.start", b"Custom\r\n"),
                ("Custom.binds", &preset_file("Custom", "")),
            ],
        );
        let Source::Read(b) = read(&dir, "HeadlookMode") else {
            panic!("should have read it");
        };
        assert_eq!(b.schema, Some(4));
        assert_eq!(b.presets[0].name, "Custom");
        assert_eq!(
            b.superseded,
            vec![
                dir.join("StartPreset.start"),
                dir.join("StartPreset.3.start")
            ],
            "lowest first, and the one with no number at all is the lowest"
        );
    }

    /// The directory-level half of the crate's rule. An unreadable preset file
    /// may be the active one, or a second file claiming the same name, so a
    /// value read out of its neighbour is not a value this reader can show is
    /// in use.
    #[test]
    fn one_unreadable_preset_refuses_the_whole_directory() {
        let dir = dir_with(
            "broken",
            &[
                ("StartPreset.4.start", b"Custom\r\n"),
                ("Custom.binds", &preset_file("Custom", "")),
                ("Broken.binds", b"<Root PresetName=\"Oops\">"),
            ],
        );
        match read(&dir, "HeadlookMode") {
            Source::Rejected(why) => assert!(why.contains("Broken.binds"), "{why}"),
            other => panic!("a preset that cannot be read is not nothing: {other:?}"),
        }
    }

    /// The start file names a preset that is not here, or that two files claim.
    /// Neither is a value, and neither is silence.
    #[test]
    fn a_preset_that_is_not_there_or_is_there_twice_is_unmatched() {
        let dir = dir_with(
            "unmatched",
            &[
                ("StartPreset.4.start", b"Custom\r\nTwice\r\n"),
                ("a.binds", &preset_file("Twice", "")),
                ("b.binds", &preset_file("Twice", "")),
            ],
        );
        let Source::Read(b) = read(&dir, "HeadlookMode") else {
            panic!("should have read it");
        };
        assert_eq!(b.presets.len(), 2);
        match (&b.presets[0].found, &b.presets[1].found) {
            (Found::Unmatched(gone), Found::Unmatched(twice)) => {
                assert!(gone.contains("no preset"), "{gone}");
                assert!(twice.contains("2 presets"), "{twice}");
            }
            other => panic!("neither should have matched one file: {other:?}"),
        }
    }
}
