//! Star Citizen-style `attributes.xml`: one flat list of name/value pairs.
//!
//! # The format, as the file on this machine has it
//!
//! Read off a real `attributes.xml` — 6915 bytes, 226 attributes — rather than
//! inferred:
//!
//! ```text
//! <Attributes Version="35">
//!  <Attr name="ADSMouseSensitivity" value="1"/>
//!  <Attr name="HeadtrackingSource" value="1"/>
//!  <Attr name="HeadtrackingInactivityTime" value="2"/>
//! </Attributes>
//! ```
//!
//! No declaration, no byte-order mark, one space of indentation, every value a
//! quoted string whatever it holds. `Version` on the document element is `35`
//! today; it is read and reported, never checked, for the reason the crate
//! docs give about schema drift.
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
    /// Both answers are the same refusal — for a document that is not one of
    /// these files at all, which is the only case where one reason is the true
    /// answer to both questions.
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
/// user needs in order to go and look.
pub fn parse(doc: &[u8], name: &str) -> Attributes {
    let document = match xml::parse(doc) {
        Ok(d) => d,
        Err(why) => return Attributes::rejected(&why),
    };
    if document.root.name != ROOT {
        return Attributes::rejected(&format!(
            "a file whose outermost element is <{}> and not <{ROOT}>",
            document.root.name
        ));
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
        // A document that is not one of these files at all is the other case,
        // and there one reason really is the answer to both.
        let a = parse(
            b"<Options Version=\"35\"><Attr name=\"A\" value=\"1\"/></Options>",
            "A",
        );
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
