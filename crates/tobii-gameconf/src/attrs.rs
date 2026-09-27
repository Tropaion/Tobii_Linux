//! Star Citizen-style `attributes.xml`: one flat list of name/value pairs.
//!
//! # The format, as the file on this machine has it
//!
//! Read off a real `attributes.xml` — 6915 bytes, 112 `<Attr>` elements, no
//! two of them sharing a name — rather than inferred:
//!
//! ```text
//! <Attributes Version="35">
//!  <Attr name="ADSMouseSensitivity" value="1"/>
//!  <Attr name="HeadtrackingSource" value="1"/>
//!  <Attr name="HeadtrackingInactivityTime" value="2"/>
//! </Attributes>
//! ```
//!
//! No declaration, no byte-order mark, one space of indentation, every line
//! ending CRLF, every value a quoted string whatever it holds. `Version` on
//! the document element is `35` today; it is read and reported, never checked,
//! for the reason the crate docs give about schema drift.
//!
//! Both counts above are of one file on one machine, and the `attrs` mode of
//! this crate's `read` example takes them again:
//!
//! ```text
//! cargo run -p tobii-gameconf --example read -- attrs <attributes.xml> HeadtrackingSource
//! ```
//!
//! # What this reader will not tell you
//!
//! `HeadtrackingSource` is the reason anyone would point this module at that
//! file, and it holds a small number. **What that number means is not
//! something this project has measured.** The game's Options → Source control
//! is a list, the file stores an index into something, and nobody here has
//! watched that list change and the file change with it. So the report says
//! what the file holds and where the setting lives in the game's menus, and
//! stops there — which is what "read-only" means when it is taken seriously:
//! not just refusing to write the file, but refusing to invent the meaning of
//! what is in it.
//!
//! Like [`crate::binds`], this module names no game, no path and no setting.
//! It is handed a file and the name of an attribute.

use crate::{xml, Lookup, Source};
use std::path::Path;

/// The document element every one of these files has.
const ROOT: &str = "Attributes";

/// The element each pair is written as.
const PAIR: &str = "Attr";

/// What one `attributes.xml` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attributes {
    /// `Version` on the document element. Reported, never judged.
    pub version: Lookup,
    /// What the asked-for attribute holds.
    pub value: Lookup,
}

impl Attributes {
    /// Both answers are the same refusal — for a document this reader could
    /// not parse, which is the only case where one reason is the true answer
    /// to both questions, because nothing in it was decoded to hold an answer
    /// apart.
    fn rejected(why: &str) -> Self {
        Attributes {
            version: Lookup::Rejected(why.to_string()),
            value: Lookup::Rejected(why.to_string()),
        }
    }
}

/// Read one attributes document, asking what the attribute called `name`
/// holds.
///
/// Matching is exact. A file holding the same name in another case comes back
/// [`Lookup::Rejected`] rather than as a value or as an absence, because
/// whether the game's own reader is case-sensitive is not something this
/// project has measured — and both of the confident answers would be a guess
/// about that. The refusal says which spelling is in the file, which is what a
/// user needs in order to go and look. [`crate::binds`] reads the other format
/// by the same rule, and [`crate::binds::start`] gives the argument in full,
/// including why a *file name* is the one thing either module compares
/// case-insensitively.
pub fn parse(doc: &[u8], name: &str) -> Attributes {
    let document = match xml::parse(doc) {
        Ok(d) => d,
        Err(why) => return Attributes::rejected(&why),
    };
    if document.root.name != ROOT {
        // Not a refusal about XML but about *which* document this is, and it
        // reaches exactly as far as that: a file that parses and is not one of
        // these has no attribute list this reader can be asked about, and its
        // document element is still an element this reader decoded to the last
        // character. Answering "could not be read" about the number written
        // plainly on it would be the crate telling a user to go and look at
        // something it had in fact read. [`crate::binds::parse`] answers a
        // wrong document element the same way, for the same reason.
        return Attributes {
            version: document.root.attribute("Version"),
            value: Lookup::Rejected(format!(
                "a file whose outermost element is <{}> and not <{ROOT}>",
                document.root.name
            )),
        };
    }
    Attributes {
        // Two questions of two different parts of the document, so two
        // answers that fail for their own reasons. `Version` is on the
        // document element; whether some `<Attr>` further down carries a name
        // this reader can place says nothing about it, and reporting it as
        // unreadable because of one would be this crate sending a user to go
        // and look at a number it had in fact decoded.
        version: document.root.attribute("Version"),
        value: value(&document, name),
    }
}

/// What the attribute called `name` holds, in a document already known to be
/// one of these files.
fn value(document: &xml::Document<'_>, name: &str) -> Lookup {
    let mut exact: Vec<&xml::Element> = Vec::new();
    let mut other_case: Vec<String> = Vec::new();
    for element in document.children.iter().filter(|e| e.name == PAIR) {
        match element.attribute("name") {
            Lookup::Text(n) if n == name => exact.push(element),
            Lookup::Text(n) if n.eq_ignore_ascii_case(name) => other_case.push(n),
            Lookup::Text(_) => {}
            // An <Attr> with no name, or one this reader cannot decode, may be
            // the one being asked about — so the answer to the question is
            // that it could not be answered, not that the setting is unset.
            Lookup::Absent => {
                return Lookup::Rejected(format!("an <{PAIR}> with no name attribute"))
            }
            Lookup::Rejected(why) => {
                return Lookup::Rejected(format!("an <{PAIR}> whose name is {why}"))
            }
        }
    }
    match (exact.as_slice(), other_case.as_slice()) {
        ([], []) => Lookup::Absent,
        ([], spellings) => Lookup::Rejected(format!(
            "no attribute called {name}, but one called {}, and this reader \
             cannot tell whether the game reads it as the same setting",
            spellings.join(" and ")
        )),
        ([one], _) => match one.attribute("value") {
            Lookup::Absent => Lookup::Rejected(format!("an <{PAIR}> {name} with no value")),
            other => other,
        },
        (many, _) => Lookup::Rejected(format!(
            "{name} {} times in one file, and this reader cannot tell which of \
             them the game uses",
            many.len()
        )),
    }
}

/// Read the attributes file at `path`, asking what `name` holds.
///
/// [`Source::Unwritten`] means the file is not there — a game that has never
/// saved its options — and is separated from [`Source::Rejected`] for the
/// reason [`Source`] gives.
pub fn read(path: &Path, name: &str) -> Source<Attributes> {
    match crate::read(path) {
        Source::Read(bytes) => Source::Read(parse(&bytes, name)),
        Source::Unwritten(why) => {
            Source::Unwritten(format!("{why}: nothing has saved these settings here"))
        }
        Source::Rejected(why) => Source::Rejected(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::scratch;

    /// The shape the real file has, down to the one-space indentation and the
    /// missing declaration.
    const REAL: &[u8] = b"<Attributes Version=\"35\">\n \
        <Attr name=\"AudioMasterVolume\" value=\"1\"/>\n \
        <Attr name=\"HeadtrackingInactivityTime\" value=\"2\"/>\n \
        <Attr name=\"HeadtrackingSource\" value=\"1\"/>\n \
        <Attr name=\"Width\" value=\"2560\"/>\n\
        </Attributes>\n";

    #[test]
    fn reads_an_attribute_and_the_schema_version() {
        let a = parse(REAL, "HeadtrackingSource");
        assert_eq!(a.value, Lookup::Text("1".into()));
        assert_eq!(a.version, Lookup::Text("35".into()));
        assert_eq!(parse(REAL, "NotASetting").value, Lookup::Absent);
    }

    /// The rule, at the level of one file.
    #[test]
    fn an_unreadable_file_is_refused_and_not_reported_empty() {
        for bad in [
            &b"<Attributes Version=\"35\"><Attr name=\"A\" value=\"1\"/>"[..],
            &b"<Options><Attr name=\"A\" value=\"1\"/></Options>"[..],
            &b"<Attributes><Attr value=\"1\"/></Attributes>"[..],
        ] {
            let a = parse(bad, "A");
            assert!(matches!(a.value, Lookup::Rejected(_)), "{a:?}");
        }
        // There, and holding nothing this reader can use.
        let a = parse(b"<Attributes><Attr name=\"A\"/></Attributes>", "A");
        assert!(matches!(a.value, Lookup::Rejected(_)), "{a:?}");
        // Twice, with no way to tell which the game reads.
        let a = parse(
            b"<Attributes><Attr name=\"A\" value=\"1\"/><Attr name=\"A\" value=\"2\"/></Attributes>",
            "A",
        );
        assert!(matches!(a.value, Lookup::Rejected(_)), "{a:?}");
    }

    /// The schema version is a fact about the document element, and an
    /// `<Attr>` this reader cannot place is a fact about one line further
    /// down. Answering the first out of the second puts a sentence in a report
    /// that is simply untrue — the file plainly says `Version="35"` — and this
    /// crate is worth using only for being exact about what it did read.
    #[test]
    fn an_unplaceable_attr_does_not_make_the_version_unreadable() {
        let doc = b"<Attributes Version=\"35\"><Attr name=\"A\" value=\"1\"/>\
                    <Attr value=\"2\"/></Attributes>";
        let a = parse(doc, "A");
        assert_eq!(a.version, Lookup::Text("35".into()));
        assert!(matches!(a.value, Lookup::Rejected(_)), "{a:?}");
    }

    /// A document element this reader does not know is a fact about which
    /// document this is, and the refusal reaches exactly that far.
    ///
    /// The file is not one of these, so it has no attribute list to be asked
    /// about — but `Version` is written on the element this reader just
    /// decoded, and calling it unreadable would send a user to go and look at
    /// a number the crate had in fact read. [`crate::binds::parse`] answers a
    /// wrong document element the same way, and two modules answering one
    /// question in opposite directions is a crate arguing with itself.
    #[test]
    fn a_document_that_is_not_one_of_these_files_still_says_its_version() {
        let a = parse(
            b"<Options Version=\"35\"><Attr name=\"A\" value=\"1\"/></Options>",
            "A",
        );
        assert_eq!(a.version, Lookup::Text("35".into()));
        match a.value {
            Lookup::Rejected(why) => assert!(why.contains("<Options>"), "{why}"),
            other => panic!("a file that is not one of these has no value to report: {other:?}"),
        }
        // The other case is unchanged: nothing was decoded, so there is one
        // reason and it is the answer to both.
        let a = parse(b"<Attributes Version=\"35\"><Attr name=\"A\"", "A");
        assert!(matches!(a.version, Lookup::Rejected(_)), "{a:?}");
        assert!(matches!(a.value, Lookup::Rejected(_)), "{a:?}");
    }

    /// Whether the game's own reader is case-sensitive is unmeasured, so
    /// neither confident answer is available: not the value, and not "you have
    /// not set this".
    #[test]
    fn a_name_in_another_case_is_neither_a_value_nor_an_absence() {
        let doc = b"<Attributes><Attr name=\"headtrackingsource\" value=\"1\"/></Attributes>";
        match parse(doc, "HeadtrackingSource").value {
            Lookup::Rejected(why) => assert!(why.contains("headtrackingsource"), "{why}"),
            other => panic!("should have refused to choose: {other:?}"),
        }
    }

    /// Every number the header states about the real file is one the example
    /// takes again.
    ///
    /// The file is somebody's install and nothing in CI has it, so the numbers
    /// themselves cannot be held here. What can is the property that makes
    /// them worth writing down at all: a maintainer can point the example at
    /// the file and read the same two numbers back. A figure with no counter
    /// behind it is one nobody can check — which is how an element count that
    /// matched nothing in the file sat under a heading promising it had been
    /// measured — so the header states a size and a population, and the
    /// example prints a size and a population.
    #[test]
    fn every_number_in_the_header_is_counted_by_the_example() {
        // Without the code markers: whether a name is quoted in prose is a
        // typographic choice, and a rule that turned on it would be a rule
        // about typography.
        let header = include_str!("attrs.rs")
            .split_once("//! # What this reader will not tell you")
            .expect("the header runs down to the next heading")
            .0
            .replace('`', "");
        let example = include_str!("../examples/read.rs");
        for (stated, counted) in [("bytes", "bytes"), ("<Attr> elements", "<Attr> elements")] {
            assert!(
                header.contains(stated),
                "the header states no `{stated}` for the file it was read off"
            );
            assert!(
                example.contains(counted),
                "the header states `{stated}` and the example prints no \
                 `{counted}`: a number nobody can take again"
            );
        }
    }

    /// A game that has never saved its options has no file, and that is its
    /// own answer — not "the setting is unset", and not "something is wrong".
    #[test]
    fn a_file_that_is_not_there_says_so() {
        let dir = scratch("attrs");
        match read(&dir.join("attributes.xml"), "HeadtrackingSource") {
            Source::Unwritten(why) => assert!(why.contains("attributes.xml"), "{why}"),
            other => panic!("a file that is not there is not a reading: {other:?}"),
        }
        std::fs::write(dir.join("attributes.xml"), REAL).expect("fixture");
        let Source::Read(a) = read(&dir.join("attributes.xml"), "HeadtrackingSource") else {
            panic!("should have read it");
        };
        assert_eq!(a.value, Lookup::Text("1".into()));
    }
}
