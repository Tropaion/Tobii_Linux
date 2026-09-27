//! The one-level XML scanner both readers share.
//!
//! # What it is for
//!
//! Both formats this crate reads are the same shape: a document element with
//! attributes, holding a flat list of child elements with attributes. Elite's
//! `.binds` nests further — a binding holds `<Primary>`, `<Deadzone>` and so
//! on — but nothing this crate is asked about lives below the first level, and
//! keeping the scan to that level is what stops a question about `Deadzone`
//! from matching the dozens of them buried inside individual bindings.
//!
//! # What it refuses
//!
//! Everything it was not proved against. The point of a small reader is not
//! that it is short, it is that the things it does not understand come back as
//! [`Err`] instead of as a value — so the failure mode of a format that moves
//! is a sentence saying so, not a wrong answer. It refuses:
//!
//! * bytes that are not UTF-8, and a declaration naming an encoding other than
//!   UTF-8 or US-ASCII. A UTF-16 file fails the first test anyway; a file that
//!   *says* it is CP1252 and happens to be ASCII would decode identically, but
//!   saying so is a promise this reader has not earned for the byte where it
//!   stops being true;
//! * a tag that never ends, an element that is never closed, a closing tag
//!   naming something other than what is open, and more than one document
//!   element;
//! * an attribute without a quoted value, an attribute name spelled twice in
//!   one element, and a `<` inside an attribute value;
//! * a tag or an attribute whose name holds an ASCII character XML does not
//!   allow in a name. Characters above ASCII are let through without the
//!   Unicode table a conforming parser carries, which is the one place this
//!   reader is knowingly the more permissive of the two — see [`is_name`] for
//!   what that costs and what it buys;
//! * a `<!…>` construct that is none of the three it knows: a comment, a CDATA
//!   section or a document type declaration;
//! * a document type declaration with an internal subset, because entity
//!   declarations in it would change what the rest of the document means;
//! * an entity reference other than the five XML defines and numeric
//!   character references, and any numeric reference that does not name a
//!   character.
//!
//! It tolerates a UTF-8 byte-order mark, CRLF and bare line-feed endings,
//! single- or double-quoted attribute values, comments, processing
//! instructions and CDATA sections — every one of them a habit of the program
//! that wrote the file rather than content. Which of Elite's shipped presets
//! has which habit is counted in [`crate::binds`], next to the files it was
//! counted off and next to the example that counts them again; a second copy
//! of a count here would be a second answer.
//!
//! # What it deliberately does not do
//!
//! It does not perform attribute-value normalisation. XML says a tab or a
//! newline inside an attribute value is to be replaced by a space, and a
//! conforming parser would hand the caller the normalised string — but what
//! matters here is what the *game's* parser does with its own file, and that
//! is not something this project has measured. So a value holding a control
//! character is [`crate::Lookup::Rejected`] rather than quietly rewritten:
//! neither format puts one in the values this crate is asked about, and a
//! reader that invents one interpretation of a file it is only reporting on is
//! doing the one thing this crate promises not to do.

use crate::Lookup;

/// One element: its name, and the attributes on its start tag.
pub(crate) struct Element<'a> {
    pub name: &'a str,
    /// In document order, names and values exactly as the file spells them.
    attrs: Vec<(&'a str, &'a str)>,
}

impl<'a> Element<'a> {
    /// What `name` holds on this element.
    ///
    /// Case-sensitive, because XML is and because both formats are written by
    /// a program: Elite spells it `Value` and `PresetName`, Star Citizen
    /// spells it `name` and `value`, and neither varies.
    pub fn attribute(&self, name: &str) -> Lookup {
        match self.attrs.iter().find(|(n, _)| *n == name) {
            None => Lookup::Absent,
            Some((_, raw)) => match decode(raw) {
                Ok(text) => Lookup::Text(text),
                Err(why) => Lookup::Rejected(why),
            },
        }
    }
}

/// A document read as far as this crate needs it.
pub(crate) struct Document<'a> {
    /// The document element itself.
    pub root: Element<'a>,
    /// Its direct children, in document order. What is nested deeper is
    /// scanned — it has to be, to find the end of it — and dropped.
    pub children: Vec<Element<'a>>,
}

/// Scan `bytes` into a document element and its direct children.
///
/// The [`Err`] is one sentence, phrased to be shown to a user: it ends up in
/// a report next to the name of the file it is about.
pub(crate) fn parse(bytes: &[u8]) -> Result<Document<'_>, String> {
    // A UTF-8 byte-order mark is not part of the document, and it is in front
    // of most of the files this reader is pointed at.
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| {
        "a file that is not UTF-8 text, which this reader cannot decode".to_string()
    })?;
    let b = text.as_bytes();
    let mut i = 0usize;
    // The names of the elements that are open, innermost last. Names and not a
    // depth counter: a closing tag that does not match what is open means the
    // scan has lost its place, and a counter cannot tell.
    let mut open: Vec<&str> = Vec::new();
    let mut root: Option<Element> = None;
    let mut children: Vec<Element> = Vec::new();
    let mut closed_root = false;
    while i < b.len() {
        if b[i] != b'<' {
            // Character data. Ignored inside the document element — neither
            // format uses it — but outside it, anything that is not whitespace
            // means this is not one XML document and the scan is somewhere it
            // does not understand.
            if open.is_empty() && !b[i].is_ascii_whitespace() {
                return Err("a file with text outside its outermost element".to_string());
            }
            i += 1;
            continue;
        }
        let rest = &text[i..];
        if let Some(skip) = skipped(rest)? {
            // A declaration, a comment, a CDATA section or a doctype: not an
            // element, but its extent has to be known exactly, or a `<` inside
            // one would be read as a tag.
            check_encoding(&rest[..skip])?;
            i += skip;
            continue;
        }
        if let Some(after) = rest.strip_prefix("</") {
            let end = after
                .find('>')
                .ok_or_else(|| "a closing tag that never ends".to_string())?;
            let name = after[..end].trim_end_matches(|c: char| c.is_ascii_whitespace());
            match open.pop() {
                Some(o) if o == name => {}
                Some(o) => return Err(format!("a </{name}> where <{o}> was open")),
                None => return Err(format!("a </{name}> that closes nothing")),
            }
            if open.is_empty() {
                closed_root = true;
            }
            i += 2 + end + 1;
            continue;
        }
        let (element, self_closing, len) = start_tag(rest)?;
        // Taken before the element is moved into one of the arms below.
        let name = element.name;
        match open.len() {
            0 if root.is_some() || closed_root => {
                return Err("a file holding more than one outermost element".to_string())
            }
            0 => root = Some(element),
            1 => children.push(element),
            // Deeper than this crate reads. Scanned for its extent only.
            _ => {}
        }
        if !self_closing {
            open.push(name);
        } else if open.is_empty() {
            closed_root = true;
        }
        i += len;
    }
    if let Some(name) = open.last() {
        return Err(format!("an element <{name}> that is never closed"));
    }
    let root = root.ok_or_else(|| "a file holding no XML element at all".to_string())?;
    Ok(Document { root, children })
}

/// How long the non-element construct at the start of `rest` is, if it is one.
///
/// Processing instructions and the XML declaration (`<?…?>`), comments
/// (`<!--…-->`), CDATA sections (`<![CDATA[…]]>`) and document type
/// declarations (`<!…>`). Each is skipped whole, because each can hold a `<`
/// or a `>` that is not a tag.
fn skipped(rest: &str) -> Result<Option<usize>, String> {
    for (open, close, what) in [
        ("<?", "?>", "a processing instruction"),
        ("<!--", "-->", "a comment"),
        ("<![CDATA[", "]]>", "a CDATA section"),
    ] {
        if let Some(after) = rest.strip_prefix(open) {
            let end = after
                .find(close)
                .ok_or_else(|| format!("{what} that never ends"))?;
            return Ok(Some(open.len() + end + close.len()));
        }
    }
    let Some(after) = rest.strip_prefix("<!") else {
        return Ok(None);
    };
    // Comments and CDATA sections were taken above, so a document type
    // declaration is the only `<!…>` construct left that this reader knows.
    // Anything else spelled that way gets skipped whole if it is let through,
    // and whatever sits inside it goes with it — which is how an element this
    // crate was asked about turns into [`Lookup::Absent`], *the user has not
    // set this*, on a document no parser would have read.
    if !after
        .strip_prefix("DOCTYPE")
        .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_whitespace()) || r.starts_with('>'))
    {
        return Err("a <!…> construct this reader does not know".to_string());
    }
    let end = after
        .find('>')
        .ok_or_else(|| "a document type declaration that never ends".to_string())?;
    if after[..end].contains('[') {
        // An internal subset can declare entities, which changes what every
        // `&name;` in the rest of the document means. Neither format has one;
        // a file that grew one is a file this reader no longer understands.
        return Err("a document type declaration holding an internal subset".to_string());
    }
    Ok(Some(2 + end + 1))
}

/// Refuse a construct that names an encoding this reader did not decode.
///
/// Only the first construct in a file can be the XML declaration proper, and
/// a well-formed document may not spell `<?xml …?>` anywhere else — but a
/// document that does is one whose author meant those bytes to be read as
/// something other than what this reader decoded, and that is the single shape
/// where a tolerated construct turns into a wrong value rather than a missing
/// one. So the question is asked of every construct that is skipped, and where
/// it sits is not part of the answer.
fn check_encoding(declaration: &str) -> Result<(), String> {
    let Some(body) = declaration.strip_prefix("<?xml") else {
        return Ok(());
    };
    // `<?xml-stylesheet …?>` is a processing instruction aimed at something
    // else and says nothing about how these bytes are encoded; the
    // declaration's target is `xml` and nothing longer.
    if !body.starts_with(|c: char| c.is_ascii_whitespace()) {
        return Ok(());
    }
    let Some(at) = body.find("encoding") else {
        return Ok(());
    };
    let quoted = &body[at + "encoding".len()..];
    let Some((_, rest)) = quoted.split_once(['"', '\'']) else {
        return Ok(());
    };
    let name = rest.split(['"', '\'']).next().unwrap_or_default();
    if ["utf-8", "utf8", "us-ascii", "ascii"]
        .iter()
        .any(|ok| name.eq_ignore_ascii_case(ok))
    {
        return Ok(());
    }
    Err(format!(
        "a file that says it is {name}, which this reader cannot decode"
    ))
}

/// Whether `name` is spelled the way XML spells a name, as far as here.
///
/// XML's `Name` production is a table of Unicode ranges, and this reader does
/// not carry one: it checks the ASCII half exactly and lets every character
/// above ASCII through. That is knowingly the more permissive of the two, and
/// the trade is deliberate. Carrying the table is the weight that argues for a
/// real parser, which this crate has an argument against; and what the ASCII
/// half buys is the whole of what either format spells, since Elite and Star
/// Citizen both write every element and attribute name in ASCII letters. So an
/// ASCII character XML does not allow in a name means the scan is holding
/// something that is not the name it thinks it is — and the honest answer to
/// that is a refusal. The alternative is worse than it looks: a lookup against
/// a name nobody could have written comes back [`Lookup::Absent`], which says
/// *the user has not set this* about a document no parser would have read at
/// all.
fn is_name(name: &str) -> bool {
    fn first(c: char) -> bool {
        c.is_ascii_alphabetic() || c == '_' || c == ':' || !c.is_ascii()
    }
    fn later(c: char) -> bool {
        first(c) || c.is_ascii_digit() || c == '-' || c == '.'
    }
    let mut chars = name.chars();
    chars.next().is_some_and(first) && chars.all(later)
}

/// Read a start tag: the element, whether it closes itself, and its length.
fn start_tag(rest: &str) -> Result<(Element<'_>, bool, usize), String> {
    let after = &rest[1..];
    let name_len = after
        .find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        .ok_or_else(|| "a tag that never ends".to_string())?;
    let name = &after[..name_len];
    if !is_name(name) {
        return Err(format!(
            "a tag whose name this reader cannot read: `{name}`"
        ));
    }
    // The end of the tag, found with quoting in mind: a `>` inside an
    // attribute value is legal XML and ends nothing.
    let body = &after[name_len..];
    let mut quote: Option<u8> = None;
    let mut end = None;
    for (at, byte) in body.bytes().enumerate() {
        match (quote, byte) {
            (Some(q), b) if b == q => quote = None,
            (Some(_), _) => {}
            (None, b @ (b'"' | b'\'')) => quote = Some(b),
            (None, b'>') => {
                end = Some(at);
                break;
            }
            (None, _) => {}
        }
    }
    let end = end.ok_or_else(|| format!("a <{name}> tag that never ends"))?;
    let (attrs, self_closing) = match body[..end].strip_suffix('/') {
        Some(a) => (a, true),
        None => (&body[..end], false),
    };
    let attrs = attributes(attrs).map_err(|why| format!("<{name}> has {why}"))?;
    Ok((
        Element { name, attrs },
        self_closing,
        1 + name_len + end + 1,
    ))
}

/// Split the text between an element's name and the end of its tag.
fn attributes(mut raw: &str) -> Result<Vec<(&str, &str)>, String> {
    let mut out: Vec<(&str, &str)> = Vec::new();
    loop {
        raw = raw.trim_start_matches(|c: char| c.is_ascii_whitespace());
        if raw.is_empty() {
            return Ok(out);
        }
        let name_len = raw
            .find(|c: char| c.is_ascii_whitespace() || c == '=')
            .ok_or_else(|| "an attribute with no value".to_string())?;
        let (name, after) = raw.split_at(name_len);
        if name.is_empty() {
            return Err("an attribute with no name".to_string());
        }
        if !is_name(name) {
            return Err(format!(
                "an attribute whose name this reader cannot read: `{name}`"
            ));
        }
        let after = after
            .trim_start_matches(|c: char| c.is_ascii_whitespace())
            .strip_prefix('=')
            .ok_or_else(|| format!("an attribute `{name}` with no value"))?
            .trim_start_matches(|c: char| c.is_ascii_whitespace());
        let q = match after.as_bytes().first() {
            Some(b'"') => '"',
            Some(b'\'') => '\'',
            _ => return Err(format!("an attribute `{name}` whose value is not quoted")),
        };
        let (value, rest) = after[1..]
            .split_once(q)
            .ok_or_else(|| format!("an attribute `{name}` whose value never ends"))?;
        if value.contains('<') {
            return Err(format!("an attribute `{name}` holding a `<`"));
        }
        if out.iter().any(|(n, _)| *n == name) {
            // Illegal XML, and the one shape where a reader that took the
            // first or the last would be answering for a document no parser
            // agrees about.
            return Err(format!("the attribute `{name}` twice"));
        }
        out.push((name, value));
        raw = rest;
    }
}

/// Resolve the references in an attribute value.
///
/// The five entities XML defines and numeric character references, and nothing
/// else — see the module docs on why an unknown one is an error rather than a
/// literal ampersand.
fn decode(raw: &str) -> Result<String, String> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        let end = after.find(';').ok_or_else(|| {
            "a value holding a `&` that starts nothing this reader knows".to_string()
        })?;
        let name = &after[1..end];
        let ch = match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => numeric(name).ok_or_else(|| {
                format!("a value holding an entity `&{name};` this reader cannot resolve")
            })?,
        };
        out.push(ch);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    if out.chars().any(char::is_control) {
        // Including a tab and a newline, which XML would normalise to a space.
        // See the module docs: normalising is a claim about the game's parser,
        // and this crate does not make claims it has not measured.
        return Err(
            "a value with a control character in it, which this reader cannot show whole"
                .to_string(),
        );
    }
    Ok(out)
}

/// `#1234` or `#x04d2`, as a character.
///
/// The digits are checked before they are parsed, because Rust's integer
/// parsers are the more permissive of the two: both accept a leading `+`, and
/// XML's `CharRef` production has no sign in it. `x` is likewise the whole of
/// what marks a hexadecimal reference — an uppercase `X` is a spelling XML
/// does not define. Letting either through would answer `&#+65;` with an `A`,
/// which is this reader handing back a value for a document no parser would
/// have read.
fn numeric(name: &str) -> Option<char> {
    let digits = name.strip_prefix('#')?;
    let value = match digits.strip_prefix('x') {
        Some(hex) if hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
            u32::from_str_radix(hex, 16).ok()?
        }
        None if digits.bytes().all(|b| b.is_ascii_digit()) => digits.parse::<u32>().ok()?,
        _ => return None,
    };
    char::from_u32(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Result<Document<'_>, String> {
        parse(text.as_bytes())
    }

    /// The shape both formats actually have, read the way both readers read
    /// it: the document element's own attributes and its direct children.
    #[test]
    fn reads_a_document_element_and_its_children() {
        let d = doc(r#"<Root PresetName="Custom"><A Value="1" /><B Value="2"/></Root>"#)
            .expect("parses");
        assert_eq!(d.root.name, "Root");
        assert_eq!(
            d.root.attribute("PresetName"),
            Lookup::Text("Custom".into())
        );
        assert_eq!(d.root.attribute("SortOrder"), Lookup::Absent);
        assert_eq!(
            d.children.iter().map(|e| e.name).collect::<Vec<_>>(),
            ["A", "B"]
        );
        assert_eq!(d.children[1].attribute("Value"), Lookup::Text("2".into()));
    }

    /// The one that makes the depth limit worth having. A preset document
    /// repeats names like `Deadzone` inside its individual bindings — the
    /// module docs say where that comes from — so a scanner that matched on
    /// name alone would find those when asked about a setting of the same
    /// name, and could not tell which was which.
    #[test]
    fn nested_elements_are_not_children() {
        let d = doc("<Root><Axis><Deadzone Value=\"0\"/></Axis><Deadzone Value=\"9\"/></Root>")
            .expect("parses");
        let named: Vec<_> = d.children.iter().filter(|e| e.name == "Deadzone").collect();
        assert_eq!(named.len(), 1, "only the first-level one");
        assert_eq!(named[0].attribute("Value"), Lookup::Text("9".into()));
    }

    /// Both real formats arrive with a byte-order mark, CRLF line endings and
    /// tabs. All three are file-writing habits, not content.
    #[test]
    fn a_byte_order_mark_and_crlf_are_not_content() {
        let mut bytes = b"\xef\xbb\xbf".to_vec();
        bytes.extend_from_slice(b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<Root>\r\n\t<A Value=\"1\" />\r\n</Root>\r\n");
        let d = parse(&bytes).expect("parses");
        assert_eq!(d.root.name, "Root");
        assert_eq!(d.children[0].attribute("Value"), Lookup::Text("1".into()));
    }

    #[test]
    fn comments_declarations_and_cdata_are_skipped_whole() {
        let d =
            doc("<!-- <Root Value=\"wrong\"/> --><Root><A Value=\"1\"/><![CDATA[ <B/> ]]></Root>")
                .expect("parses");
        assert_eq!(d.root.name, "Root");
        assert_eq!(d.children.len(), 1, "the CDATA is not an element");
    }

    /// A `>` inside an attribute value ends nothing. Getting this wrong
    /// desynchronises the whole scan, which is the failure that produces
    /// confident wrong values rather than an error.
    #[test]
    fn a_greater_than_inside_a_value_does_not_end_the_tag() {
        let d = doc(r#"<Root><A Value="a>b" Other="c"/></Root>"#).expect("parses");
        assert_eq!(d.children.len(), 1);
        assert_eq!(d.children[0].attribute("Value"), Lookup::Text("a>b".into()));
        assert_eq!(d.children[0].attribute("Other"), Lookup::Text("c".into()));
    }

    #[test]
    fn entities_are_resolved_and_unknown_ones_refused() {
        let d = doc(r#"<Root><A V="&lt;&amp;&gt;&quot;&apos;&#65;&#x42;"/><B V="&nbsp;"/></Root>"#)
            .expect("parses");
        assert_eq!(
            d.children[0].attribute("V"),
            Lookup::Text("<&>\"'AB".into())
        );
        match d.children[1].attribute("V") {
            Lookup::Rejected(why) => assert!(why.contains("&nbsp;"), "{why}"),
            other => panic!("an entity this reader cannot resolve is not a value: {other:?}"),
        }
    }

    /// A character reference XML does not define is not a character.
    ///
    /// Rust's integer parsers take a leading `+`, and an uppercase `X` is a
    /// spelling XML has no production for. Either one let through answers with
    /// a confident `A` off a document no parser would have read, which is the
    /// shape the whole module is written against.
    #[test]
    fn a_character_reference_spelled_a_way_xml_does_not_define_is_refused() {
        for bad in [
            r#"<Root V="&#+65;"/>"#,
            r#"<Root V="&#x+41;"/>"#,
            r#"<Root V="&#X41;"/>"#,
            r#"<Root V="&#-65;"/>"#,
            r#"<Root V="&#6 5;"/>"#,
        ] {
            match doc(bad).expect("parses").root.attribute("V") {
                Lookup::Rejected(why) => assert!(why.contains("cannot resolve"), "{why}"),
                other => panic!("{bad} is not a value: {other:?}"),
            }
        }
        // The two spellings XML does define still read, hexadecimal digits in
        // either case.
        let d = doc(r#"<Root V="&#65;&#x4a;&#x4A;"/>"#).expect("parses");
        assert_eq!(d.root.attribute("V"), Lookup::Text("AJJ".into()));
    }

    /// Every shape the module docs promise to refuse, refused. The assertion
    /// that matters is `is_err`: what is being guarded is that none of these
    /// comes back as a document with a value in it.
    #[test]
    fn malformed_documents_are_refused() {
        for bad in [
            "<Root><A Value=\"1\"/>",
            "<Root><A Value=\"1\"/></Other>",
            "<Root/><Other/>",
            "<Root><A Value=1/></Root>",
            "<Root><A Value/></Root>",
            "<Root><A V=\"1\" V=\"2\"/></Root>",
            "<Root><A V=\"1\"",
            "<!DOCTYPE Root [ <!ENTITY x \"y\"> ]><Root/>",
            "not xml at all",
            "",
            "<?xml version=\"1.0\" encoding=\"windows-1252\"?><Root/>",
        ] {
            assert!(doc(bad).is_err(), "should have been refused: {bad:?}");
        }
        assert!(
            parse(b"<Root V=\"\xff\xfe\"/>").is_err(),
            "bytes that are not UTF-8 are not a document"
        );
    }

    /// An attribute that is there and holds nothing is not an attribute that
    /// is not there — the distinction the whole [`Lookup`] type exists for.
    #[test]
    fn an_empty_value_is_a_value() {
        let d = doc(r#"<Root V=""/>"#).expect("parses");
        assert_eq!(d.root.attribute("V"), Lookup::Text(String::new()));
        assert_eq!(d.root.attribute("W"), Lookup::Absent);
    }

    /// XML would normalise this to a space. This reader does not pretend to
    /// know whether the game's own parser does.
    #[test]
    fn a_control_character_in_a_value_is_refused() {
        let d = doc("<Root V=\"a\nb\"/>").expect("parses");
        assert!(matches!(d.root.attribute("V"), Lookup::Rejected(_)));
    }

    /// The declaration is refused for what it says, not for where it sits.
    ///
    /// A document may not spell `<?xml …?>` after anything else, so each of
    /// these is already a file no parser would read — but the bytes still
    /// claim an encoding this reader did not decode, and answering out of them
    /// is the one way a tolerated construct turns into a wrong value rather
    /// than a missing one.
    #[test]
    fn an_encoding_this_reader_did_not_decode_is_refused_wherever_it_is_named() {
        for bad in [
            "<?xml version=\"1.0\" encoding=\"windows-1252\"?><Root V=\"1\"/>",
            " <?xml version=\"1.0\" encoding=\"windows-1252\"?><Root V=\"1\"/>",
            "<!-- first --><?xml version=\"1.0\" encoding=\"windows-1252\"?><Root V=\"1\"/>",
            "<Root V=\"1\"/><?xml version=\"1.0\" encoding=\"iso-8859-1\"?>",
        ] {
            match doc(bad) {
                Err(why) => assert!(why.contains("which this reader cannot decode"), "{why}"),
                Ok(_) => panic!("read as though the encoding were not there: {bad:?}"),
            }
        }
        // A processing instruction aimed at something else is not the
        // declaration, whatever it happens to hold.
        let d = doc("<?xml-stylesheet href=\"a.xsl\" encoding=\"windows-1252\"?><Root V=\"1\"/>")
            .expect("parses");
        assert_eq!(d.root.attribute("V"), Lookup::Text("1".into()));
    }

    /// A name no parser would accept is a document this reader has lost its
    /// place in, and [`Lookup::Absent`] — *the user has not set this* — is the
    /// wrong thing to say about one.
    #[test]
    fn a_name_spelled_with_a_character_xml_forbids_is_refused() {
        for bad in [
            "<Ro=ot V=\"1\"/>",
            "<Ro\"ot V=\"1\"/>",
            "<Root V=\"1\"><A'B V=\"2\"/></Root>",
            "<Root V(x)=\"1\"/>",
            "<Root a<b=\"1\"/>",
            // Not a name at all: a <!…> construct that is none of the three
            // this reader knows, skipped whole and taking its contents with it
            // if it were let through.
            "<Root><! V=\"1\"/></Root>",
            "<Root><!Z V=\"1\"/></Root>",
        ] {
            assert!(doc(bad).is_err(), "should have been refused: {bad:?}");
        }
        // The one it does know still reads.
        let d = doc("<!DOCTYPE Root><Root V=\"1\"/>").expect("parses");
        assert_eq!(d.root.attribute("V"), Lookup::Text("1".into()));
        // The ASCII half only. Above it this reader has no table and does not
        // pretend to: a name it cannot judge is let through rather than
        // refused on a guess.
        let d = doc("<R\u{f6}ot V\u{e4}=\"1\"/>").expect("parses");
        assert_eq!(d.root.attribute("V\u{e4}"), Lookup::Text("1".into()));
        // And everything the two real formats spell still reads.
        let d =
            doc("<Root><HeadLookPitchAxisRaw.2 xml:id=\"a-b\" _v=\"1\"/></Root>").expect("parses");
        assert_eq!(d.children[0].attribute("_v"), Lookup::Text("1".into()));
    }

    /// This module states no census of Elite's shipped files.
    ///
    /// Prose cannot be unit-tested, but a second copy of a measurement can be:
    /// two modules that count the same files are two counts, and a reader
    /// believes whichever one they happen to open. The count lives in
    /// [`crate::binds`], beside the files it was taken off and beside the
    /// example that takes it again, and this fails if a copy grows here.
    ///
    /// What it holds is the rule and not a list of wordings. A census is a
    /// number about those files, so it is caught wherever it is written: the
    /// module docs carry no digits at all beyond the encodings they have to
    /// name, and no sentence anywhere in this file's prose puts a number,
    /// digits or spelled out, next to a word for the files Elite ships. A list
    /// of phrases would only ever catch the phrase somebody already corrected.
    ///
    /// Anywhere means the whole file, these tests included. Cutting it at the
    /// literal text `#[cfg(test)]` would be a rule any ordinary comment naming
    /// the gate could switch off for everything under it, and it would leave
    /// the tests free to restate a census the module above them may not. What
    /// makes the whole file safe to scan is that this reads comments and not
    /// code: the needles below are string literals, and a check that read
    /// those would only ever find itself.
    #[test]
    fn the_shipped_preset_census_is_stated_somewhere_else() {
        /// Names for the set a census would be of.
        const THE_SET: [&str; 5] = ["elite", "preset", "shipped", "ships", ".binds"];
        /// Numbers a census can be written with, past the digits.
        ///
        /// Cardinals only: an ordinal counts nothing — "the first level", "a
        /// second copy" — and this file's prose is full of them.
        const CARDINALS: [&str; 28] = [
            "one",
            "two",
            "three",
            "four",
            "five",
            "six",
            "seven",
            "eight",
            "nine",
            "ten",
            "eleven",
            "twelve",
            "thirteen",
            "fourteen",
            "fifteen",
            "sixteen",
            "seventeen",
            "eighteen",
            "nineteen",
            "twenty",
            "thirty",
            "forty",
            "fifty",
            "sixty",
            "seventy",
            "eighty",
            "ninety",
            "hundred",
        ];
        /// The only numbers with a reason to be in the module docs: the names
        /// of encodings, which are names and not counts.
        const ENCODINGS: [&str; 3] = ["UTF-8", "UTF-16", "CP1252"];

        let src = include_str!("xml.rs");

        for line in src.lines() {
            let Some(doc) = line.trim_start().strip_prefix("//!") else {
                continue;
            };
            let mut rest = doc.to_string();
            for encoding in ENCODINGS {
                rest = rest.replace(encoding, "");
            }
            assert!(
                !rest.contains(|c: char| c.is_ascii_digit()),
                "a number in xml.rs's module docs, which count nothing: {doc}"
            );
        }

        // Sentences, because a census is a claim and a claim can be spread
        // over as many lines as the wrapping takes. Each run of comment lines
        // is its own text: joining across items would invent sentences neither
        // of them says.
        for sentence in prose_sentences(src) {
            let lower = sentence.to_ascii_lowercase();
            if !THE_SET.iter().any(|word| lower.contains(word)) {
                continue;
            }
            let number = lower
                .split(|c: char| !c.is_ascii_alphanumeric())
                .find(|word| {
                    word.contains(|c: char| c.is_ascii_digit()) || CARDINALS.contains(word)
                });
            assert!(
                number.is_none(),
                "xml.rs counts Elite's shipped files — `{}` — and the census \
                 belongs to binds.rs, where it was measured: {sentence}",
                number.unwrap_or_default()
            );
        }
    }

    /// Every sentence of every comment in `src`, one run of comment lines at a
    /// time.
    ///
    /// Ordinary `//` comments as well as doc comments: a census restated in
    /// one is a second answer just the same, and a rule about a file's prose
    /// that stopped at the third slash would be a rule about syntax.
    fn prose_sentences(src: &str) -> Vec<String> {
        let mut blocks: Vec<String> = Vec::new();
        let mut open = false;
        for line in src.lines() {
            let line = line.trim_start();
            let doc = line
                .strip_prefix("//!")
                .or_else(|| line.strip_prefix("///"))
                .or_else(|| line.strip_prefix("//"));
            match doc {
                Some(text) => {
                    if !open {
                        blocks.push(String::new());
                        open = true;
                    }
                    let block = blocks.last_mut().expect("just pushed");
                    block.push(' ');
                    block.push_str(text.trim());
                }
                None => open = false,
            }
        }
        blocks
            .iter()
            .flat_map(|block| block.split(". "))
            .map(|sentence| sentence.trim().to_string())
            .filter(|sentence| !sentence.is_empty())
            .collect()
    }
}
