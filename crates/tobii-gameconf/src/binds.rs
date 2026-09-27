//! Elite Dangerous-style control presets: a directory of `.binds` documents
//! and the `StartPreset` file that says which of them is live.
//!
//! # The format, as the game's own files have it
//!
//! Measured on 2026-09-27 against the 30 presets Elite Dangerous ships, read
//! off this machine's install rather than inferred:
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
//! * The document element is `Root` and carries `PresetName`. Indentation is
//!   tabs, shown above with spaces only because a doc comment may not hold
//!   one — tabs throughout in 26 of the files, and in the other 4 with two
//!   lines apiece, both of them inside `<ChargeECM>`, indented with four
//!   spaces instead. Those same 26 begin with a UTF-8 byte-order mark and end
//!   every line with CRLF; the other 4 — `AdvancedPS3Controller`, `Empty`,
//!   `PS3Controller` and `PS3ControllerYaw` — have no mark, are mixed rather
//!   than CRLF throughout (96, 108, 103 and 103 bare line feeds among their
//!   carriage returns), and write their declaration as `encoding="UTF-8" ?>`
//!   where the 26 write `encoding="utf-8"?>`. The same four files carry all
//!   four habits, so this is one split in the set and not four.
//! * A setting is a direct child with a `Value` attribute. Bindings are direct
//!   children too, but they hold their own children instead — and those nest
//!   names like `Deadzone` and `Binding` that repeat dozens of times per file,
//!   which is why [`crate::xml`] only ever offers the first level.
//! * `MajorVersion` and `MinorVersion` appear on `Root` in 2 of the 30 shipped
//!   files (`SaitekX56` and `T16000MHOTAS`, both 1.8) and not in the other 28.
//!   1.8 is the only bindings schema this project has ever seen, and nobody
//!   here has watched it change — but a saved preset carries the schema of the
//!   version that wrote it, so the numbers are read and reported rather than
//!   checked against a list this crate would have had to invent.
//!
//! Every count above is of one install on one machine on the date named.
//! Nothing in CI has those files, so no test here can show the numbers are
//! still true — what a test can do, and what this crate's `read` example is
//! for, is take them again:
//!
//! ```text
//! cargo run -p tobii-gameconf --example read -- presets <ControlSchemes> HeadlookMode
//! ```
//!
//! Its `presets` mode reads every `.binds` document in a directory, without a
//! `StartPreset` file, and prints each count above beside the per-file numbers
//! it is made of. A number in this census that the example does not print is a
//! number nobody can check; that is a rule for whoever edits the census, and
//! not something a test can hold, because the files it would have to count are
//! on one maintainer's disk.
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
//! Elite Dangerous install on this machine **has no Frontier user directory
//! and no `Options` or `Bindings` directory anywhere in its Proton prefix**,
//! so there is nothing here to read the convention off. The prefix itself has
//! been run — `compatdata/359320/pfx` is there and populated — which is why
//! the claim is about what the game wrote and not about whether it started.
//! What that exercises — and it is the path a user most often hits — is
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
    /// read at all, which is the only case where one reason is the true
    /// answer to all three questions.
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
        //
        // Only the setting is refused by it. What the document element calls
        // itself and what version it carries are attributes this reader
        // decoded to the last character, and answering "could not be read"
        // about them would be the crate telling a user to go and look at a
        // file it had in fact read — the one thing a report like this cannot
        // afford to do. It also costs [`read`] the fact it needs most: a name
        // it can compare against the active one, to say whether this file is
        // the preset in use or a stranger in the directory.
        //
        // [`crate::attrs::parse`] answers a wrong document element the same
        // way: what was decoded off it is reported, and the refusal reaches
        // only the question the document cannot answer.
        return Preset {
            name: document.root.attribute("PresetName"),
            version: version(&document.root),
            setting: Lookup::Rejected(format!(
                "a file whose outermost element is <{}> and not <{ROOT}>",
                document.root.name
            )),
        };
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
/// preset — and a line holding a control character, a `/` or a `\` is refused:
/// those say this is not the file this reader thinks it is, or that something
/// here is a path rather than a name, and the answer to either is a sentence
/// and not a best guess at which line was meant.
///
/// Every other line is carried through exactly as the file spells it. `..` is
/// a preset name here and not a parent directory — a name off this file is
/// only ever compared against what the preset documents call themselves and
/// printed in a report, never joined onto a path — and a reader that decided
/// which strings the game accepts as a preset name would be enforcing a rule
/// nobody here has watched the game apply.
///
/// A list rather than one name: newer versions of the game write several lines
/// here, one per category of binding, and which line governs which setting is
/// not something this project has been able to observe. Reporting every
/// distinct name it holds is the honest shape — when they are all the same
/// preset, which is the ordinary case, the report has one row.
///
/// Distinct means exactly distinct. Two lines spelled the same are one name;
/// two that differ only in case are two, and are matched against the presets
/// separately. This is the rule [`crate::attrs`] states for the other format
/// and for the same reason: whether the game's own reader is case-sensitive is
/// not something this project has measured, and the confident answers are both
/// guesses about that — one of them collapsing two rows a user can see in
/// their own file into one, the other reporting a preset as missing when a
/// file here very nearly names it. So neither is given: the names are carried
/// through as the file spells them, and [`read`] says what it found for each.
/// The one place this module *is* case-insensitive is [`start_schema`], which
/// is a question about a name on a disk rather than about a file's contents.
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
        if !out.iter().any(|n| n == name) {
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
/// configured game as unconfigured. That is an argument about names on a
/// disk, and it is why it does not carry over to the contents of the files —
/// see [`start`].
///
/// A name, not a string: the bytes the host holds are what a filesystem
/// actually stores, and every part of this spelling is ASCII.
///
/// The digits are checked before they are parsed, for the reason
/// [`crate::xml`]'s character-reference reader gives: Rust's integer parsers
/// accept a leading `+`, and nothing that writes these files does. Letting one
/// through would not merely read an odd name: `StartPreset.+4.start` would be
/// schema 4 beside a real `StartPreset.4.start`, which is a tie, and a tie
/// takes the whole directory down. A backup, a sync conflict or a hand edit
/// left next to the live file must not do that.
fn start_schema(file_name: &[u8]) -> Option<Option<u32>> {
    let rest = strip_prefix_ignore_ascii_case(file_name, b"StartPreset")?;
    let rest = strip_suffix_ignore_ascii_case(rest, b".start")?;
    if rest.is_empty() {
        return Some(None);
    }
    let (dot, digits) = rest.split_first()?;
    if *dot != b'.' || digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits)
        .ok()?
        .parse::<u32>()
        .ok()
        .map(Some)
}

fn strip_prefix_ignore_ascii_case<'a>(s: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    let (head, rest) = s.split_at_checked(prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then_some(rest)
}

fn strip_suffix_ignore_ascii_case<'a>(s: &'a [u8], suffix: &[u8]) -> Option<&'a [u8]> {
    let (rest, tail) = s.split_at_checked(s.len().checked_sub(suffix.len())?)?;
    tail.eq_ignore_ascii_case(suffix).then_some(rest)
}

/// What to say about a directory that is not there.
///
/// `ENOENT` is one error covering two situations, and the sentence has to be
/// true of both. If everything above the directory is there, the filesystem
/// holding it is mounted and readable and the only thing missing is what the
/// game would have written: *nothing has saved a control scheme here* is a
/// fact. If something further up is missing too, the path may be leading onto
/// a drive nobody has mounted — `libraryfolders.vdf` on this machine still
/// names a Steam library under `/run/media`, and `/run/media/tropaion` is not
/// there — and the same sentence becomes a confident negative about files this
/// reader never got to look at. It is the worst thing this crate can say, and
/// it is reachable today.
///
/// Nothing in the error separates the two, and a mount table is a guess this
/// module will not make. So the missing part of the path is named, the
/// sentence stops short of a promise, and a user looking at it recognises
/// their own unmounted drive in one line.
fn nothing_at(dir: &Path) -> String {
    // Shallowest first. A relative path's ancestors end at the empty path,
    // which stats as nothing there and would be named as the missing part of
    // the path; the working directory is not what is missing.
    let mut ancestors: Vec<&Path> = dir
        .ancestors()
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    ancestors.reverse();
    match ancestors
        .into_iter()
        .find(|p| matches!(p.try_exists(), Ok(false)))
    {
        Some(gone) if gone == dir => format!(
            "there is no directory at {}: nothing has saved a control scheme here",
            dir.display()
        ),
        Some(gone) => format!(
            "there is no directory at {}, and no {} either: nothing has saved \
             a control scheme here, unless this path leads onto something this \
             machine has not mounted",
            dir.display(),
            gone.display()
        ),
        // Every ancestor answered with an error rather than a yes or a no — a
        // component that cannot be searched, say. That the directory is not
        // there is all this reader saw.
        None => format!(
            "there is no directory at {}, and this reader could not see how \
             much of the path above it exists: nothing has saved a control \
             scheme here, unless this path leads onto something this machine \
             has not mounted",
            dir.display()
        ),
    }
}

/// One preset the `StartPreset` file names, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    /// The name, spelled as the `StartPreset` file spells it.
    pub name: String,
    pub found: Found,
}

/// Whether the named preset turned out to be one readable file.
///
/// Three cases for the reason [`Lookup`] has three, one level up: a directory
/// that holds nothing of this name and a directory this reader will not choose
/// within are different sentences in a report, and only the second one means
/// *go look yourself*. Two of them in one variant would leave a caller
/// string-matching the reason to tell "you have not saved this preset" from
/// "two files here claim it".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// No file in the directory calls itself this, and none comes near it.
    Absent(String),
    /// Something here answers to the name and this reader will not pick among
    /// it: a preset spelled the same but for its case, or two files claiming
    /// the name outright.
    Rejected(String),
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
    ///
    /// Lower-numbered and not merely other: two start files carrying the same
    /// schema are no ordering at all, and [`read`] refuses the directory
    /// rather than let the sort break the tie on the spelling of a file name.
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
/// by the `PresetName` inside it rather than by what the file is called. That
/// makes one unreadable file a refusal for the whole directory: it may be the
/// one that calls itself the active preset, or a second file that also does,
/// and reporting a value from its neighbour would be reporting a preset this
/// reader cannot show is the one in use. The refusal names the file.
///
/// Every `.binds` *name*, not every `.binds` regular file. A directory called
/// `Custom.binds`, a symbolic link to one, a link to nothing at all — none of
/// them is a document, and every one of them is a refusal naming the path,
/// which is the answer the `StartPreset` names get as well. Skipping them
/// instead would leave the directory read whole and reported as holding no
/// preset of that name: a confident negative about a directory holding
/// something the user can see, named after the very preset they asked about.
///
/// A file name is never decoded, so a name that is not UTF-8 is not a file
/// this reader skips. These directories sit under a Proton prefix and are
/// written by a Windows program, and a byte sequence the host cannot read as
/// text is a name the host will still hand over intact — while `.binds` and
/// `StartPreset` are ASCII, so recognising either takes no decoding at all.
/// Skipping such a file would be silence about the one document that may hold
/// the active preset; the name is only ever compared and printed.
///
/// A document that parses and is not a preset still says what it calls itself,
/// and that is enough to tell whether it is the one in use — so it is reported
/// where it belongs, with the refusal on the setting it could not answer,
/// rather than taking the directory down with it.
///
/// A preset is matched to the name in the start file exactly, case included,
/// and a spelling that differs only in case is neither a match nor an absence.
/// See [`start`] for why: the case-insensitivity elsewhere in this module is
/// an argument about file *names*, and this is a question about contents.
pub fn read(dir: &Path, setting: &str) -> Source<Bindings> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Source::Unwritten(nothing_at(dir))
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
        // Bytes, not text: see the function docs. The two things asked of a
        // name here are ASCII, and a name that is not UTF-8 is a name this
        // reader can still answer them about.
        let Some(name) = path.file_name() else {
            continue;
        };
        let name = name.as_encoded_bytes();
        if let Some(schema) = start_schema(name) {
            starts.push((schema, path));
        } else if strip_suffix_ignore_ascii_case(name, b".binds").is_some() {
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
    // Two of them at one schema, which the sort has no way to order. `pop`
    // would pick whichever path sorts last and call the other superseded,
    // though neither supersedes anything — and it is reachable: this name is
    // matched case-insensitively, and `.04.` is the same number as `.4.` on
    // any filesystem. Picking by path bytes is the guess this reader does not
    // make, here as everywhere else it finds two files claiming one thing.
    if let Some((_, other)) = starts.last().filter(|(s, _)| *s == schema) {
        return Source::Rejected(format!(
            "{} and {} are both StartPreset files at the same schema, and this \
             reader cannot tell which the game loads",
            other.display(),
            start_file.display()
        ));
    }
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
                .filter(|(_, p)| matches!(&p.name, Lookup::Text(n) if *n == name))
                .collect();
            // Near misses, kept separately rather than folded in: whether the
            // game reads them as this preset is the thing nobody here has
            // measured, and silence about them would be a confident negative
            // about a directory that plainly holds something very like it.
            let other_case: Vec<&str> = read_presets
                .iter()
                .filter_map(|(_, p)| match &p.name {
                    Lookup::Text(n) if *n != name && n.eq_ignore_ascii_case(&name) => {
                        Some(n.as_str())
                    }
                    _ => None,
                })
                .collect();
            let found = match (matches.as_slice(), other_case.as_slice()) {
                ([], []) => Found::Absent(format!(
                    "no preset in {} calls itself {name}",
                    dir.display()
                )),
                ([], spellings) => Found::Rejected(format!(
                    "no preset in {} calls itself {name}, only {} — and whether \
                     the game reads a name in another case as the same preset \
                     is not something this project has measured",
                    dir.display(),
                    spellings.join(" and ")
                )),
                ([(file, preset)], _) => Found::Read {
                    file: file.clone(),
                    preset: preset.clone(),
                },
                (many, _) => Found::Rejected(format!(
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

    /// A document that parses and is not a preset cannot answer about the
    /// setting — but it can still say what it calls itself, and saying
    /// otherwise would send a user to look at a file this reader read.
    ///
    /// The distinction is not cosmetic at the directory level: [`read`] places
    /// a file by the name inside it, so a name reported as unreadable is a
    /// file it cannot rule out as the active preset, and every other preset in
    /// the directory goes unread with it.
    #[test]
    fn a_document_that_is_not_a_preset_still_says_what_it_calls_itself() {
        let p = parse(
            b"<Options PresetName=\"C\" MajorVersion=\"1\" MinorVersion=\"8\">\
              <A Value=\"1\"/></Options>",
            "A",
        );
        assert_eq!(p.name, Lookup::Text("C".into()));
        assert_eq!(p.version, Lookup::Text("1.8".into()));
        match p.setting {
            Lookup::Rejected(why) => assert!(why.contains("<Options>"), "{why}"),
            other => panic!("a file that is not a preset has no setting to report: {other:?}"),
        }
        // And the directory around it is still read, because this file's own
        // name is enough to place it.
        let dir = dir_with(
            "stranger",
            &[
                ("StartPreset.4.start", b"Custom\r\n"),
                ("Custom.binds", &preset_file("Custom", "")),
                ("Other.binds", &b"<Options PresetName=\"Stranger\"/>"[..]),
            ],
        );
        let Source::Read(b) = read(&dir, "HeadlookMode") else {
            panic!("a stranger this reader can place does not refuse the directory");
        };
        let Found::Read { preset, .. } = &b.presets[0].found else {
            panic!("should have matched: {:?}", b.presets[0]);
        };
        assert_eq!(
            preset.setting,
            Lookup::Text("Bindings_HeadlookModeAccumulate".into())
        );
    }

    #[test]
    fn start_files_are_recognised_by_name_and_schema() {
        assert_eq!(start_schema(b"StartPreset.start"), Some(None));
        assert_eq!(start_schema(b"StartPreset.4.start"), Some(Some(4)));
        assert_eq!(start_schema(b"startpreset.4.START"), Some(Some(4)));
        for no in [
            &b"StartPreset"[..],
            b"StartPreset.4",
            b"Custom.4.0.binds",
            b"StartPreset.x.start",
            b"NotStartPreset.start",
            b"Start.start",
            // A name the host holds and no decoder can read as text. It is
            // not a start file, and answering that takes no decoding.
            b"StartPreset.\xff.start",
            // Rust's integer parsers take a leading sign; nothing that writes
            // these files does. See below for what reading one costs.
            b"StartPreset.+4.start",
            b"StartPreset.-4.start",
            b"StartPreset. 4.start",
        ] {
            assert_eq!(start_schema(no), None, "{}", String::from_utf8_lossy(no));
        }
    }

    /// A signed schema beside the real one is not a tie.
    ///
    /// `StartPreset.+4.start` is the shape a backup, a sync conflict or a hand
    /// edit leaves behind. Read as schema 4 it collides with the live
    /// `StartPreset.4.start`, and the tie refusal — which is right about two
    /// files that really do claim one schema — then takes the whole directory
    /// down over a name no writer of these files produces.
    #[test]
    fn a_start_file_with_a_signed_schema_is_not_a_start_file() {
        let dir = dir_with(
            "signed",
            &[
                ("StartPreset.4.start", b"Custom\r\n"),
                ("StartPreset.+4.start", b"Stray\r\n"),
                ("Custom.binds", &preset_file("Custom", "")),
            ],
        );
        let Source::Read(b) = read(&dir, "HeadlookMode") else {
            panic!("a name no writer produces is not a second start file");
        };
        assert_eq!(b.schema, Some(4));
        assert_eq!(b.start, dir.join("StartPreset.4.start"));
        assert!(b.superseded.is_empty(), "{:?}", b.superseded);
        assert_eq!(b.presets[0].name, "Custom");
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
        // And what the refusal is not about. A separator and a control
        // character are the whole of it; `..` is a preset name here, only ever
        // compared against what the documents call themselves and printed.
        assert_eq!(
            start(b"Custom\r\n..\r\nC:\r\n").expect("reads"),
            ["Custom", "..", "C:"],
            "this reader does not decide which strings the game allows"
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

    /// A path whose ancestors are gone is `ENOENT` as well, and that is the
    /// shape of a Proton prefix on a drive nobody mounted.
    ///
    /// It is reachable on this machine today: `libraryfolders.vdf` still names
    /// a Steam library under `/run/media`, and `/run/media/tropaion` does not
    /// exist. *Nothing has saved a control scheme here* about that path is a
    /// confident negative about files this reader never got to look at — the
    /// one sentence the crate is built not to say — so it names the part of
    /// the path that is missing and stops short of the promise.
    #[test]
    fn a_path_whose_ancestors_are_gone_does_not_promise_the_game_saved_nothing() {
        const FLAT: &str = "nothing has saved a control scheme here";
        let root = scratch("unmounted");
        let unmounted = root.join("DatenSSD");
        match read(&unmounted.join("games/steam/Bindings"), "HeadlookMode") {
            Source::Unwritten(why) => {
                assert!(
                    why.contains(&unmounted.display().to_string()),
                    "the missing part of the path is what a user recognises \
                     their own unmounted drive by: {why}"
                );
                assert!(
                    !why.ends_with(FLAT),
                    "a path this reader could not follow is not a game that \
                     saved nothing: {why}"
                );
            }
            other => panic!("a path that is not there is not a reading: {other:?}"),
        }
        // And the ordinary case keeps the flat sentence. Everything above the
        // directory is there, so whatever holds it is mounted and readable and
        // the one thing missing is what the game would have written.
        std::fs::create_dir_all(root.join("Options")).expect("fixture");
        match read(&root.join("Options/Bindings"), "HeadlookMode") {
            Source::Unwritten(why) => assert!(why.ends_with(FLAT), "{why}"),
            other => panic!("a directory that is not there is not a reading: {other:?}"),
        }
    }

    /// A name ending `.binds` that is not a document this reader can open.
    ///
    /// Skipping it would leave the directory read whole and reported as
    /// holding no preset of the active name — a flat negative about a
    /// directory holding something the user can see, named after the very
    /// preset they are asking about. A `StartPreset` name one pattern away
    /// refuses loudly, and so does this.
    #[test]
    fn a_directory_called_binds_is_refused_and_not_reported_absent() {
        let dir = dir_with("trapdir", &[("StartPreset.4.start", b"Custom\r\n")]);
        std::fs::create_dir(dir.join("Custom.binds")).expect("fixture");
        match read(&dir, "HeadlookMode") {
            Source::Rejected(why) => assert!(why.contains("Custom.binds"), "{why}"),
            other => panic!("a directory is not a preset nobody saved: {other:?}"),
        }
    }

    /// The same, for a link that points at nothing.
    ///
    /// These directories sit under a Proton prefix and are synced, backed up
    /// and copied about; a link whose target went away is what that leaves.
    #[cfg(unix)]
    #[test]
    fn a_binds_link_to_nothing_is_refused_and_not_reported_absent() {
        let dir = dir_with("traplink", &[("StartPreset.4.start", b"Gone\r\n")]);
        std::os::unix::fs::symlink("nowhere.binds", dir.join("Gone.binds")).expect("fixture");
        match read(&dir, "HeadlookMode") {
            Source::Rejected(why) => assert!(why.contains("Gone.binds"), "{why}"),
            other => panic!("a link to nothing is not a preset nobody saved: {other:?}"),
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

    /// A name the host will hand over and no decoder can read as text.
    ///
    /// These directories sit under a Proton prefix, written by a Windows
    /// program, so this is not an exotic shape — and the answer that used to
    /// come back was the worst one available: the file was skipped without a
    /// word, and the directory reported that nothing in it calls itself the
    /// active preset. That sentence is a confident negative about a directory
    /// that plainly holds the preset, which is the one thing this crate is
    /// built not to say.
    #[cfg(unix)]
    #[test]
    fn a_preset_whose_file_name_is_not_text_is_still_read() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = dir_with("undecodable", &[("StartPreset.4.start", b"Custom\r\n")]);
        let name = dir.join(OsStr::from_bytes(b"Cust\xffom.binds"));
        std::fs::write(&name, preset_file("Custom", "")).expect("fixture");
        let Source::Read(b) = read(&dir, "HeadlookMode") else {
            panic!("a name this reader cannot decode is not a preset it cannot place");
        };
        let Found::Read { file, preset } = &b.presets[0].found else {
            panic!("should have matched: {:?}", b.presets[0]);
        };
        assert_eq!(file, &name);
        assert_eq!(
            preset.setting,
            Lookup::Text("Bindings_HeadlookModeAccumulate".into())
        );
    }

    /// The case rule, at the directory level: whether the game reads `custom`
    /// as `Custom` is unmeasured, so neither confident answer is available.
    ///
    /// Not the value — that would be this reader deciding a question it has
    /// never looked at — and not "no preset here calls itself that", which is
    /// a flat negative about a directory holding a file that very nearly does.
    /// So the near miss is named, and the user is sent to look.
    #[test]
    fn a_preset_name_in_another_case_is_neither_a_match_nor_an_absence() {
        let dir = dir_with(
            "case",
            &[
                ("StartPreset.4.start", b"Custom\r\n"),
                ("custom.binds", &preset_file("custom", "")),
            ],
        );
        let Source::Read(b) = read(&dir, "HeadlookMode") else {
            panic!("should have read it");
        };
        match &b.presets[0].found {
            Found::Rejected(why) => assert!(why.contains("only custom"), "{why}"),
            other => panic!("should have refused to choose: {other:?}"),
        }
        // And the start file itself: two spellings are two names, each asked
        // after on its own, because collapsing them would be the same guess
        // made one step earlier.
        assert_eq!(
            start(b"Custom\r\ncustom\r\nCustom\r\n").expect("reads"),
            ["Custom", "custom"],
            "the same spelling twice is one name; two spellings are two"
        );
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
    /// Neither is a value, and neither is silence — and they are not the same
    /// answer as each other either.
    ///
    /// A directory that simply does not hold the preset is the ordinary thing:
    /// the user has not saved it. Two files claiming it is this reader
    /// refusing to choose, which is what *go look yourself* means here. For a
    /// caller deciding which presets to offer, that is the difference between
    /// a name it can leave out and a problem it has to report — so it is in
    /// the type, where the difference can be matched on, and not only in the
    /// wording of a sentence.
    #[test]
    fn a_preset_that_is_not_there_and_one_that_is_there_twice_are_two_answers() {
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
            (Found::Absent(gone), Found::Rejected(twice)) => {
                assert!(gone.contains("no preset"), "{gone}");
                assert!(twice.contains("2 presets"), "{twice}");
            }
            other => panic!(
                "a preset nobody saved and one two files claim are not one \
                 answer: {other:?}"
            ),
        }
    }

    /// Two start files at one schema, which is not an ordering.
    ///
    /// `start_schema` compares the name case-insensitively, so a
    /// case-sensitive filesystem can hold both spellings; `.04.` is the same
    /// number as `.4.` anywhere. The sort then ties, and a pick falling to
    /// whichever path sorts last would name one of them active and the other
    /// superseded, of a pair where neither supersedes anything.
    #[test]
    fn two_start_files_at_one_schema_are_refused_rather_than_ordered_by_name() {
        for pair in [
            ["StartPreset.4.start", "startpreset.4.start"],
            ["StartPreset.4.start", "StartPreset.04.start"],
        ] {
            let dir = dir_with(
                "tied",
                &[
                    (pair[0], b"Newer\r\n"),
                    (pair[1], b"Older\r\n"),
                    ("Newer.binds", &preset_file("Newer", "")),
                    ("Older.binds", &preset_file("Older", "")),
                ],
            );
            match read(&dir, "HeadlookMode") {
                Source::Rejected(why) => {
                    assert!(why.contains(pair[0]) && why.contains(pair[1]), "{why}")
                }
                other => panic!("{pair:?} is a tie, not a reading: {other:?}"),
            }
        }
        // One of each is still an ordering, and still read.
        let dir = dir_with(
            "untied",
            &[
                ("StartPreset.3.start", b"Older\r\n"),
                ("StartPreset.4.start", b"Newer\r\n"),
                ("Newer.binds", &preset_file("Newer", "")),
                ("Older.binds", &preset_file("Older", "")),
            ],
        );
        let Source::Read(b) = read(&dir, "HeadlookMode") else {
            panic!("two schemas are an ordering");
        };
        assert_eq!(b.presets[0].name, "Newer");
    }
}
