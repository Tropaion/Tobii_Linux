//! The one-level XML scanner both readers share.
//!
//! # What it is for
//!
//! Both formats this crate reads are the same shape: a document element with
//! attributes, holding a flat list of child elements with attributes. Elite's
//! `.binds` nests further — a binding holds `<Primary>`, `<Deadzone>` and so
//! on — but nothing this crate is asked about lives below the first level, and
//! keeping the scan to that level is what stops a question about `Deadzone`
//! from matching the forty of them buried inside individual bindings.
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
//! * a document type declaration with an internal subset, because entity
//!   declarations in it would change what the rest of the document means;
//! * an entity reference other than the five XML defines and numeric
//!   character references, and any numeric reference that does not name a
//!   character.
//!
//! It tolerates a UTF-8 byte-order mark (Elite writes one on 28 of the 30
//! presets it ships, and not on the other two), CRLF line endings, single- or
//! double-quoted attribute values, comments, processing instructions and CDATA
//! sections.
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
            if i == 0 {
                check_encoding(&rest[..skip])?;
            }
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

/// Refuse a declaration that names an encoding this reader did not decode.
///
/// Only the first construct in the file can be the XML declaration, which is
/// why this is asked at offset zero and nowhere else.
fn check_encoding(declaration: &str) -> Result<(), String> {
    let Some(body) = declaration.strip_prefix("<?xml") else {
        return Ok(());
    };
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

/// Read a start tag: the element, whether it closes itself, and its length.
fn start_tag(rest: &str) -> Result<(Element<'_>, bool, usize), String> {
    let after = &rest[1..];
    let name_len = after
        .find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        .ok_or_else(|| "a tag that never ends".to_string())?;
    let name = &after[..name_len];
    if name.is_empty() || !name.starts_with(|c: char| c.is_alphabetic() || c == '_' || c == ':') {
        return Err("a tag whose name this reader cannot read".to_string());
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
fn numeric(name: &str) -> Option<char> {
    let digits = name.strip_prefix('#')?;
    let value = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
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

    /// The one that makes the depth limit worth having. Elite's `.binds` holds
    /// dozens of `<Deadzone>` elements nested inside individual bindings; a
    /// scanner that matched on name alone would find them when asked about a
    /// setting of the same name, and could not tell which was which.
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
}
