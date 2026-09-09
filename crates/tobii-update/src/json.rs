//! A small, complete JSON reader.
//!
//! Hand-rolled for the same reason [`crate::sha256`]'s neighbour in
//! `tobii-headpose` is: the one thing this crate needs to parse is a GitHub
//! release reply, and a JSON crate would be the heaviest dependency in the lean
//! build for a job of this size.
//!
//! It is a *complete* parser rather than a field scraper, because a release's
//! `body` is the changelog and is shown to the user verbatim. Scraping it with
//! a regex would hand them `\n` and `—` as literal text, and would break
//! outright on a changelog that mentions a quote or a brace.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl Value {
    /// The string at `key`, if this is an object whose `key` holds a string.
    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The boolean at `key`. Absent reads as `false`, which is what the GitHub
    /// reply means by omitting `draft` or `prerelease`.
    pub fn flag(&self, key: &str) -> bool {
        matches!(self.get(key), Some(Value::Bool(true)))
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(m) => m.get(key),
            _ => None,
        }
    }

    /// The array at `key`, or an empty slice — callers iterate either way.
    pub fn array(&self, key: &str) -> &[Value] {
        self.get(key).map_or(&[], Value::as_array)
    }

    pub fn as_array(&self) -> &[Value] {
        match self {
            Value::Array(v) => v,
            _ => &[],
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct ParseError {
    pub at: usize,
    pub what: &'static str,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}", self.what, self.at)
    }
}

impl std::error::Error for ParseError {}

/// How deeply arrays and objects may nest.
///
/// Without a limit, `value` -> `array` -> `value` recurses once per opening
/// bracket, so a reply of nothing but `[[[[...` overflows the stack and aborts
/// the process — and this parser runs on the reply to an unauthenticated GET
/// made automatically at every launch. A few kilobytes of brackets was enough.
/// A GitHub releases listing nests four deep; 64 leaves room for a format
/// change and still fails long before the stack does.
pub const MAX_DEPTH: usize = 64;

/// Largest document this will parse, in bytes.
///
/// A releases page with twenty entries is a few hundred kilobytes.
///
/// This is a **parse** limit, not the memory limit — by the time `parse` is
/// called the text is already resident, and parsing would roughly triple it.
/// What actually bounds memory is applied while reading the reply, in
/// [`crate::net`], which stops at `MAX_DOWNLOAD` bytes rather than collecting a
/// whole body and measuring it afterwards. This one keeps a reply that slipped
/// under that from being expanded into a `Value` tree.
pub const MAX_LEN: usize = 8 * 1024 * 1024;

/// Parse one JSON document. Trailing whitespace is allowed, trailing data is not.
pub fn parse(text: &str) -> Result<Value, ParseError> {
    let b = text.as_bytes();
    if b.len() > MAX_LEN {
        return Err(ParseError {
            at: 0,
            what: "the document is too large to parse",
        });
    }
    let mut p = Parser { b, i: 0, depth: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != b.len() {
        return Err(p.err("trailing data"));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    /// How many arrays and objects are open at this point.
    depth: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, what: &'static str) -> ParseError {
        ParseError { at: self.i, what }
    }

    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn lit(&mut self, word: &str, v: Value) -> Result<Value, ParseError> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(self.err("unknown literal"))
        }
    }

    fn value(&mut self) -> Result<Value, ParseError> {
        match self.b.get(self.i) {
            None => Err(self.err("unexpected end")),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b't') => self.lit("true", Value::Bool(true)),
            Some(b'f') => self.lit("false", Value::Bool(false)),
            Some(b'n') => self.lit("null", Value::Null),
            _ => self.number(),
        }
    }

    /// Enter a nested container, refusing to go deeper than [`MAX_DEPTH`].
    fn push(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        Ok(())
    }

    fn object(&mut self) -> Result<Value, ParseError> {
        self.push()?;
        self.i += 1; // '{'
        let mut map = BTreeMap::new();
        self.ws();
        if self.eat(b'}') {
            self.depth -= 1;
            return Ok(Value::Object(map));
        }
        loop {
            self.ws();
            if self.b.get(self.i) != Some(&b'"') {
                return Err(self.err("expected a key"));
            }
            let key = self.string()?;
            self.ws();
            if !self.eat(b':') {
                return Err(self.err("expected ':'"));
            }
            self.ws();
            let v = self.value()?;
            map.insert(key, v);
            self.ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b'}') {
                self.depth -= 1;
                return Ok(Value::Object(map));
            }
            return Err(self.err("expected ',' or '}'"));
        }
    }

    fn array(&mut self) -> Result<Value, ParseError> {
        self.push()?;
        self.i += 1; // '['
        let mut out = Vec::new();
        self.ws();
        if self.eat(b']') {
            self.depth -= 1;
            return Ok(Value::Array(out));
        }
        loop {
            self.ws();
            out.push(self.value()?);
            self.ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b']') {
                self.depth -= 1;
                return Ok(Value::Array(out));
            }
            return Err(self.err("expected ',' or ']'"));
        }
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.i += 1; // '"'
        let mut s = String::new();
        loop {
            let Some(&c) = self.b.get(self.i) else {
                return Err(self.err("unterminated string"));
            };
            self.i += 1;
            match c {
                b'"' => return Ok(s),
                b'\\' => {
                    let Some(&e) = self.b.get(self.i) else {
                        return Err(self.err("unterminated escape"));
                    };
                    self.i += 1;
                    match e {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'/' => s.push('/'),
                        b'b' => s.push('\u{8}'),
                        b'f' => s.push('\u{c}'),
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'u' => s.push(self.unicode_escape()?),
                        _ => return Err(self.err("unknown escape")),
                    }
                }
                // Raw control characters are invalid JSON, but rejecting a
                // changelog over a stray tab would be worse than keeping it.
                _ => {
                    let start = self.i - 1;
                    while self.b.get(self.i).is_some_and(|&n| n & 0xc0 == 0x80) {
                        self.i += 1;
                    }
                    match std::str::from_utf8(&self.b[start..self.i]) {
                        Ok(part) => s.push_str(part),
                        Err(_) => return Err(self.err("invalid utf-8")),
                    }
                }
            }
        }
    }

    /// `\uXXXX`, joining a surrogate pair when one follows.
    fn unicode_escape(&mut self) -> Result<char, ParseError> {
        let hi = self.hex4()?;
        // Not a lead surrogate: a plain code point.
        if !(0xd800..0xdc00).contains(&hi) {
            return char::from_u32(hi).ok_or_else(|| self.err("invalid code point"));
        }
        if !(self.b.get(self.i) == Some(&b'\\') && self.b.get(self.i + 1) == Some(&b'u')) {
            return Err(self.err("lone surrogate"));
        }
        self.i += 2;
        let lo = self.hex4()?;
        if !(0xdc00..0xe000).contains(&lo) {
            return Err(self.err("bad surrogate pair"));
        }
        let c = 0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00);
        char::from_u32(c).ok_or_else(|| self.err("invalid code point"))
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let end = self.i + 4;
        if end > self.b.len() {
            return Err(self.err("short \\u escape"));
        }
        let text = std::str::from_utf8(&self.b[self.i..end]).map_err(|_| self.err("bad \\u"))?;
        let v = u32::from_str_radix(text, 16).map_err(|_| self.err("bad \\u"))?;
        self.i = end;
        Ok(v)
    }

    fn number(&mut self) -> Result<Value, ParseError> {
        let start = self.i;
        self.eat(b'-'); // an optional leading sign
        while self
            .b
            .get(self.i)
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-'))
        {
            self.i += 1;
        }
        if start == self.i {
            return Err(self.err("expected a value"));
        }
        std::str::from_utf8(&self.b[start..self.i])
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .map(Value::Number)
            .ok_or(ParseError {
                at: start,
                what: "malformed number",
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shapes_a_release_reply_is_made_of_all_parse() {
        let v = parse(
            r#"{"tag_name":"v1.2.3","draft":false,"id":42,
                "assets":[{"name":"a.tar.gz","size":10}],"body":null}"#,
        )
        .unwrap();
        assert_eq!(v.str("tag_name"), Some("v1.2.3"));
        assert!(!v.flag("draft"));
        assert!(!v.flag("prerelease"), "an absent flag reads as false");
        assert_eq!(v.array("assets").len(), 1);
        assert_eq!(v.array("assets")[0].str("name"), Some("a.tar.gz"));
        assert_eq!(v.str("body"), None, "null is not a string");
        assert_eq!(v.array("nope").len(), 0, "an absent array iterates empty");
    }

    /// The changelog is shown to the user verbatim, so its escapes have to come
    /// out as the characters they stand for — not as backslash-n.
    #[test]
    fn a_changelog_keeps_its_newlines_quotes_and_unicode() {
        // `r###`: the changelog holds `"##` — a markdown heading straight after
        // a quote — which closes an `r#` or `r##` literal mid-string.
        let v = parse(r###"{"body":"## Fixed\n- the \"big\" one — at last\n"}"###).unwrap();
        assert_eq!(
            v.str("body"),
            Some("## Fixed\n- the \"big\" one — at last\n")
        );
    }

    #[test]
    fn surrogate_pairs_become_one_character() {
        let v = parse(r#"{"s":"😀"}"#).unwrap();
        assert_eq!(v.str("s"), Some("😀"));
        assert!(
            parse(r#"{"s":"\ud83d"}"#).is_err(),
            "a lone surrogate is an error"
        );
    }

    #[test]
    fn multi_byte_utf8_survives_unescaped() {
        let v = parse("{\"s\":\"grün — 日本\"}").unwrap();
        assert_eq!(v.str("s"), Some("grün — 日本"));
    }

    #[test]
    fn nesting_and_empties_round_trip() {
        assert_eq!(parse("[]").unwrap(), Value::Array(vec![]));
        assert_eq!(parse("{}").unwrap().array("x").len(), 0);
        let v = parse(r#"[{"a":[1,2]},{"a":[]}]"#).unwrap();
        assert_eq!(v.as_array().len(), 2);
        assert_eq!(v.as_array()[0].array("a").len(), 2);
    }

    /// Garbage must be an error, never a wrong answer — a half-parsed release
    /// reply could otherwise offer the user an update that does not exist.
    #[test]
    fn malformed_input_is_rejected_rather_than_guessed_at() {
        for bad in [
            "",
            "{",
            "[",
            "{\"a\"}",
            "{\"a\":}",
            "[1,]",
            "{,}",
            "tru",
            "\"unterminated",
            "{\"a\":1}x",
            "[1 2]",
            "{\"a\":1,}",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    /// The one that could take the program down. `value` recurses into
    /// `array`, so before the depth limit a reply of nothing but opening
    /// brackets overflowed the stack — and stack overflow in Rust is an abort,
    /// not a panic, so it could not be caught. This runs on a reply fetched
    /// automatically at launch, so the crash needed no user action at all.
    #[test]
    fn a_deeply_nested_reply_is_refused_instead_of_overflowing_the_stack() {
        let deep = "[".repeat(200_000) + &"]".repeat(200_000);
        let e = parse(&deep).expect_err("200k deep must be refused");
        assert_eq!(e.what, "nested too deeply");

        // The limit is on nesting depth, not on length: a long flat document
        // and one nested right up to the limit both still parse.
        let flat = format!("[{}]", vec!["1"; 50_000].join(","));
        assert_eq!(parse(&flat).unwrap().as_array().len(), 50_000);
        let at_limit = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(parse(&at_limit).is_ok(), "{MAX_DEPTH} deep is allowed");
        let over = "[".repeat(MAX_DEPTH + 1) + &"]".repeat(MAX_DEPTH + 1);
        assert!(parse(&over).is_err(), "one deeper is not");
    }

    /// Depth is unwound on the way out, so a document that opens and closes
    /// many containers in sequence is not mistaken for a deep one.
    #[test]
    fn sibling_containers_do_not_accumulate_depth() {
        let siblings = format!("[{}]", vec!["[[1]]"; 1_000].join(","));
        assert!(parse(&siblings).is_ok(), "siblings nest 3 deep, not 3000");
    }

    #[test]
    fn an_oversized_document_is_refused_before_it_is_walked() {
        let huge = format!("\"{}\"", "x".repeat(MAX_LEN));
        assert!(parse(&huge).is_err());
    }

    #[test]
    fn numbers_cover_what_the_api_sends() {
        let v = parse(r#"{"a":0,"b":-3,"c":1.5,"d":1e3,"e":123456789}"#).unwrap();
        for (k, want) in [("a", 0.0), ("b", -3.0), ("c", 1.5), ("d", 1000.0)] {
            assert_eq!(v.get(k), Some(&Value::Number(want)), "{k}");
        }
    }
}
