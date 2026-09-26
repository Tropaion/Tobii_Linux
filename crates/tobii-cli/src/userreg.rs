//! Reading a prefix's `HKEY_CURRENT_USER` keys out of `user.reg`, without
//! running wine.
//!
//! # Why this exists at all
//!
//! Asking wine what a key holds costs the prefix. `wine reg query` is still
//! `wine`, and wine initialises or upgrades whatever prefix it is pointed at
//! before it runs anything: measured against wine 11.18 on throwaway prefixes,
//! one `reg query` against a directory holding only `drive_c` created 5510
//! paths under it — `system.reg`, `user.reg`, `dosdevices/` and the whole
//! `drive_c/windows` tree — and against a *complete* prefix whose
//! `.update-timestamp` was stale, which is what a Proton prefix looks like to
//! the host's wine, it rewrote 2744 lines of `system.reg` and stamped the
//! prefix as its own. That is exactly the upgrade [`crate::bridge::WineOrigin`]
//! exists to warn about, and a command whose whole job is to report what is
//! wrong must not be the thing that changes it.
//!
//! [`crate::bridge`]'s two discovery keys are both under `HKCU`, and a prefix
//! keeps `HKCU` in one plain-text file. Reading it needs no wine, so the answer
//! does not depend on which build happens to be on `$PATH` either.
//!
//! **The trade, stated because the report has to state it too:** `user.reg` is
//! the registry as it was last *written back*. A live wineserver holds changes
//! in memory and flushes them when the last process on the prefix exits, so a
//! value written by something still running is not in this file yet.
//! [`crate::wineserver::probe`] is what says whether that is the case, and the
//! caller says so in the report. The old path was accurate and destructive;
//! this one is safe and can lag.
//!
//! # The format, as wine 11.18 actually writes it
//!
//! Checked against a real prefix rather than inferred, one value at a time:
//!
//! ```text
//! WINE REGISTRY Version 2
//! ;; All keys relative to \\User\\S-1-5-21-0-0-0-1000
//!
//! #arch=win64
//!
//! [Software\\Freetrack\\FreeTrackClient] 1790444400
//! #time=1dd4dde0e4c32d6
//! "Path"="C:\\tobii-bridge"
//! ```
//!
//! * Section headers hold the key path with every backslash doubled, `[` and
//!   `]` escaped, then `]`, a space and a decimal time. There is no `HKCU\`
//!   prefix: the whole file is relative to it, which is the line about
//!   `;; All keys relative to` above.
//! * A `REG_SZ` value is `"Name"="value"`. Any other type is spelled
//!   `"Name"=str(2):"…"` (`REG_EXPAND_SZ`), `"Name"=dword:00000001`,
//!   `"Name"=hex(b):05,00,…` (`REG_QWORD`) or `"Name"=hex:61,00,…`
//!   (`REG_BINARY`) — the number in each is hexadecimal, wine's `str(%x)`.
//! * Inside a string, `\\` is a backslash and `\"` is a quote. Control
//!   characters come back as the C escapes `\a \b \t \n \v \f \r \e`, or as
//!   `\0` and octal. Anything above U+007F is `\x` and one to four hex digits,
//!   **padded to four when the next character is itself a hex digit** so the
//!   escape cannot run on into the text: `C:\üab` is written `C:\\\x00fcab`
//!   while `C:\ü` alone is `C:\\\xfc`. That padding is the only thing that
//!   makes the escape decodable at all, and it is why this parser reads hex
//!   digits greedily to four and no further.
//!
//! # What is read and what is refused
//!
//! The rule this file inherits from [`crate::bridge`] is that a value which
//! cannot be read *exactly* is never mistaken for an absence: the caller's two
//! negative answers are not interchangeable, and only one of them is safe to be
//! wrong about. So every shape below that is not a string decoded to the last
//! character comes back as [`Lookup::Rejected`], never [`Lookup::Absent`].
//!
//! One thing is read here that `wine reg query` could not read: a value with
//! characters outside ASCII. That was refused on the old path because wine's
//! `reg.exe` prints in the console's OEM codepage, so the bytes were a guess —
//! `ü` arrives as a bare `0x81` under CP850 and no locale setting changes it.
//! `user.reg` has no codepage: the character is a hex escape naming its
//! Unicode code point, and decoding it is exact. So it is decoded and shown.
//! A caller comparing it against one of its own paths still finds no match —
//! `tobii bridge install` refuses to register a non-ASCII path in the first
//! place — so the classification is the same either way, and the user gets to
//! see the path that is actually there instead of a sentence about codepages.

/// What one value in one key turned out to hold.
///
/// Three cases for the reason [`crate::bridge::Reading`] has three: "nothing is
/// registered here" and "something is, and it is not readable" lead to
/// different words and, in the caller's other commands, to different actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// No such section, or the section has no such value.
    Absent,
    /// A `REG_SZ` string, decoded to the last character.
    Text(String),
    /// Something is there that this program cannot read back exactly, with the
    /// one thing that can honestly be said about it.
    Rejected(String),
}

/// The name of the file this module reads, inside a prefix.
pub const FILE: &str = "user.reg";

/// The first line wine requires of a `user.reg`, exactly.
///
/// Measured against wine 11.18 rather than assumed. With the line missing, a
/// UTF-8 BOM in front of it, a blank line before it, one leading space, one
/// trailing space, `Version 1` in place of `Version 2`, or the same words in
/// lower case, wine prints `user.reg is not a valid registry file`, loads no
/// `HKCU` at all, and answers every query in that prefix with "key not found".
/// A zero-byte file is refused the same way. Only a trailing `\r` is tolerated,
/// and only because a file copied off a Windows filesystem ends every line
/// that way. Wine does not rewrite the bad file either, so the state persists.
const FILE_HEADER: &str = "WINE REGISTRY Version 2";

/// Find `value_name` under `key_path` in the bytes of a `user.reg`.
///
/// `key_path` is spelled the way the file does — relative to `HKCU`, with
/// single backslashes, e.g. `Software\Freetrack\FreeTrackClient`. Matching is
/// ASCII-case-insensitive for both the key and the value name, because the
/// Windows registry is: a game calling `RegQueryValueEx(…, "Path", …)` finds a
/// value another installer stored as `path`, and a report that missed it would
/// tell the user a key is empty while their game reads something out of it.
///
/// Taken as bytes rather than as a `&str`. Wine escapes everything above ASCII,
/// so a line it wrote is always ASCII — but nothing stops a hand-edited file
/// from holding raw UTF-8, or from holding bytes that are not UTF-8 at all, and
/// decoding the file lossily first would replace those bytes with U+FFFD and
/// then hand the caller a "value" equal to nothing that is in the registry.
pub fn lookup(text: &[u8], key_path: &str, value_name: &str) -> Lookup {
    // Wine reads this one line before it reads anything else, and a file whose
    // first line is not exactly [`FILE_HEADER`] is not half-loaded: no `HKCU`
    // is loaded at all, so every key in the prefix is gone and not just this
    // one. Asked here, because a parser that skipped to the sections read such
    // a file as authoritative — `status` printed the path and "registered by
    // this installer" for a BOM'd copy of a real registration that no process
    // in that prefix could see.
    //
    // Refused and not called absent, for the reason the module header gives:
    // what is wrong here is the whole file, and "nothing is registered here" is
    // the answer whose next move is to write.
    let first = text.split(|b| *b == b'\n').next().unwrap_or_default();
    if first.strip_suffix(b"\r").unwrap_or(first) != FILE_HEADER.as_bytes() {
        return Lookup::Rejected(format!(
            "a {FILE} wine itself will not load, \
             because its first line is not `{FILE_HEADER}`"
        ));
    }
    let mut inside = false;
    // Wine's loader applies the file top to bottom into one tree, so a key or a
    // value spelled twice leaves the LAST one in memory. Measured on wine 11.18
    // four ways — the section repeated, the section respelled in another case,
    // the value repeated, the value respelled in another case — and all four
    // answer `C:\SECOND` where this parser, returning at the first match,
    // answered `C:\FIRST`. A path that is not the one the game reads is a wrong
    // ownership verdict in either direction, so the whole file is scanned and
    // the last match kept.
    let mut found: Option<Lookup> = None;
    for raw in text.split(|b| *b == b'\n') {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        // Wine writes headers and values hard against the left margin, but it
        // READS them with leading space allowed — and a file may have been
        // hand-edited or written by something else. Skipping an indented line
        // reported "nothing is registered here" about a value wine hands the
        // game, which is the one answer this parser must never give wrongly.
        let line = {
            let at = line
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .unwrap_or(line.len());
            &line[at..]
        };
        // A section header ends the previous section whatever else is true of
        // it. Asked on the bytes and before anything else, because a header
        // this parser cannot read is still a header: skipping it would leave
        // `inside` set and read the next section's values as if they were ours.
        if line.first() == Some(&b'[') {
            inside = std::str::from_utf8(line)
                .ok()
                .is_some_and(|l| header_names(l, key_path));
            continue;
        }
        if !inside || line.first() != Some(&b'"') {
            continue;
        }
        // The name first, on bytes, so that a line naming some *other* value is
        // skipped before its contents are judged. Only the line that really is
        // ours has to be readable.
        let Some((name, rest)) = split_quoted(&line[1..]) else {
            continue;
        };
        // The name is escaped the same way a value is — wine reads
        // `"P\x0061th"` as `Path` — so it is decoded before it is compared.
        // A name in OUR section that cannot be decoded is refused rather than
        // skipped: skipping it would report the value absent, and an
        // undecodable name may well be the one being asked about. Still a hard
        // return under the last-match rule below, and for the same reason: a
        // later readable assignment does not tell us this one was not ours.
        let Ok(name_text) = std::str::from_utf8(name) else {
            return Lookup::Rejected(
                "a value whose name is not text this installer can read".to_string(),
            );
        };
        let name_text = match unescape(name_text) {
            Unescaped::Ok(t) => t,
            Unescaped::Bad(why) => return Lookup::Rejected(format!("a value whose name {why}")),
        };
        if !name_text.eq_ignore_ascii_case(value_name) {
            continue;
        }
        let Ok(rest) = std::str::from_utf8(rest) else {
            return Lookup::Rejected(
                "a value whose line is not text this installer can read".to_string(),
            );
        };
        found = Some(read_assignment(rest));
    }
    found.unwrap_or(Lookup::Absent)
}

/// Split `"name"…` — with the opening quote already eaten — into the raw name
/// and whatever follows the closing quote.
///
/// Escape-aware: a `\"` inside the name is part of it, not its end. Returns
/// `None` for a name that never closes.
fn split_quoted(after_quote: &[u8]) -> Option<(&[u8], &[u8])> {
    let i = end_of_unescaped(after_quote, b'"')?;
    Some((&after_quote[..i], &after_quote[i + 1..]))
}

/// Where `end` first appears in `b` without a backslash in front of it.
///
/// The one escape-skipping rule in this file, written once: a name, a value and
/// a section header all end at the first *unescaped* terminator, and this is
/// the rule the parser's whole agreement with wine turns on. Stating it twice
/// is how the two readers drift apart.
fn end_of_unescaped(b: &[u8], end: u8) -> Option<usize> {
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            // Skip the escaped byte whatever it is: only an *unescaped*
            // terminator ends the run, and `\\` must not leave its second
            // backslash to be read as the start of a new escape.
            b'\\' => i += 2,
            c if c == end => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// Does this section header name `key_path`?
fn header_names(line: &str, key_path: &str) -> bool {
    let Some(body) = line.strip_prefix('[') else {
        return false;
    };
    let Some((name, _)) = split_bracket(body) else {
        return false;
    };
    match unescape(name) {
        // A key path is compared, not shown, so a header this parser cannot
        // decode simply is not the one we are looking for — ours decodes.
        Unescaped::Bad(_) => false,
        Unescaped::Ok(k) => key_names_match(&k, key_path),
    }
}

/// Do these two spellings name the same key, the way wine's loader decides it?
///
/// Wine splits a key name on backslashes and ignores a single empty component
/// at the end, so `Software\Freetrack\FreeTrackClient\` is the same key as
/// `Software\Freetrack\FreeTrackClient`. Measured on wine 11.18: a header
/// written with that trailing separator serves `Path` to `reg query` exactly as
/// the plain spelling does, while a whole-string compare called it absent —
/// "nothing is registered here" about a value wine was handing the game.
///
/// One trailing separator and nothing more forgiving. A leading separator, a
/// doubled one, or two trailing ones do not name the key more loosely: all
/// three kill the wineserver outright (`wine client error:0: recvmsg:
/// connection reset`, each measured), so nothing at all can be read out of such
/// a prefix and a parser that matched them would be answering for a registry
/// no process can open.
fn key_names_match(header_key: &str, key_path: &str) -> bool {
    fn trim(k: &str) -> &str {
        k.strip_suffix('\\').unwrap_or(k)
    }
    trim(header_key).eq_ignore_ascii_case(trim(key_path))
}

/// Split a section header body at its first unescaped `]`.
///
/// The index comes back a character boundary even when an escape step lands
/// mid-character: `]` is ASCII, so it can never be a UTF-8 continuation byte.
fn split_bracket(body: &str) -> Option<(&str, &str)> {
    let i = end_of_unescaped(body.as_bytes(), b']')?;
    Some((&body[..i], &body[i + 1..]))
}

/// Read `=<something>` — everything after the value's name — into an answer.
fn read_assignment(rest: &str) -> Lookup {
    // Wine tolerates whitespace on either side of the `=` and this parser did
    // not. Measured on wine 11.18: `"Path" = "C:\spaced"`, a space on one side
    // alone, and tabs on both sides are all read back as the path, while this
    // answered "a value line this installer cannot read" — about a key that may
    // hold, byte for byte, this installer's own directory, and that `uninstall`
    // reads perfectly well because it goes through wine. The direction was
    // safe (never an absence) but the report was wrong and it contradicted the
    // other command.
    fn trim(s: &str) -> &str {
        s.trim_start_matches(|c: char| c.is_ascii_whitespace())
    }
    let Some(rest) = trim(rest).strip_prefix('=') else {
        return Lookup::Rejected("a value line this installer cannot read".to_string());
    };
    let rest = trim(rest);
    // `"…"` is wine's spelling for REG_SZ, and `str(1):"…"` is the same type
    // written the long way — legal input even though wine's own writer never
    // produces it.
    if let Some(body) = rest.strip_prefix('"') {
        return read_string(body);
    }
    if let Some(body) = rest.strip_prefix("str(") {
        let Some((num, after)) = body.split_once("):") else {
            return Lookup::Rejected("a value line this installer cannot read".to_string());
        };
        let Ok(ty) = u32::from_str_radix(num, 16) else {
            return Lookup::Rejected("a value line this installer cannot read".to_string());
        };
        return match (ty, after.strip_prefix('"')) {
            (1, Some(body)) => read_string(body),
            _ => Lookup::Rejected(not_ours(ty)),
        };
    }
    if rest.starts_with("dword:") {
        return Lookup::Rejected(not_ours(4));
    }
    if rest.starts_with("hex:") {
        return Lookup::Rejected(not_ours(3));
    }
    if let Some(body) = rest.strip_prefix("hex(") {
        return match body
            .split_once("):")
            .map(|(n, _)| u32::from_str_radix(n, 16))
        {
            Some(Ok(ty)) => Lookup::Rejected(not_ours(ty)),
            _ => Lookup::Rejected("a value line this installer cannot read".to_string()),
        };
    }
    Lookup::Rejected("a value stored in a form this installer does not recognise".to_string())
}

/// Read a quoted string body — the opening quote already eaten — and say what
/// it holds.
fn read_string(body: &str) -> Lookup {
    let Some((raw, after)) = split_quoted(body.as_bytes()) else {
        return Lookup::Rejected("a value whose text never ends".to_string());
    };
    if !after.iter().all(u8::is_ascii_whitespace) {
        // Wine writes the value and then the line ends. Anything after the
        // closing quote means this line is not the shape this parser proved
        // against wine, and guessing which half is the value is how a value
        // gets read as its own first fragment.
        //
        // Anything but whitespace, that is: wine does not mind a tail of it.
        // Measured on wine 11.18, `"Path"="C:\x"` followed by spaces and the
        // same followed by a tab both read back as the path, while this refused
        // them. A value ending in a space is still decoded to its last
        // character, because the space that matters is INSIDE the quotes and
        // `split_quoted` has already ended the string before this runs.
        return Lookup::Rejected(
            "a value followed by something this installer cannot read".to_string(),
        );
    }
    // Infallible: `raw` came out of a `&str` at character boundaries — the
    // escape scan only ever steps over ASCII backslashes and whole bytes of the
    // text it walked.
    let Ok(raw) = std::str::from_utf8(raw) else {
        return Lookup::Rejected("a value whose text this installer cannot read".to_string());
    };
    let value = match unescape(raw) {
        Unescaped::Ok(v) => v,
        Unescaped::Bad(why) => return Lookup::Rejected(why.to_string()),
    };
    if value.is_empty() {
        return Lookup::Rejected(
            "a value holding no path at all, which this installer never writes".to_string(),
        );
    }
    // The wording for a line break is the one install's own reader gives it,
    // because it is the same hazard read out of a different file: a value split
    // across lines has a first half that can be byte-for-byte our own computed
    // path, and comparing a fragment is how a registration this program never
    // made gets deleted.
    if value.contains('\n') || value.contains('\r') {
        return Lookup::Rejected(
            "a value with a line break in it, which this installer never writes \
             and cannot read back whole"
                .to_string(),
        );
    }
    if value.chars().any(|c| c.is_control()) {
        return Lookup::Rejected(
            "a value with a control character in it, which this installer never \
             writes and cannot show whole"
                .to_string(),
        );
    }
    Lookup::Text(value)
}

/// Wine's own name for a registry type, in the sentence install's reader uses
/// for the same finding — one wording for "this is not a type we write",
/// wherever it was read.
fn not_ours(ty: u32) -> String {
    let name = match ty {
        1 => "REG_SZ".to_string(),
        2 => "REG_EXPAND_SZ".to_string(),
        3 => "REG_BINARY".to_string(),
        4 => "REG_DWORD".to_string(),
        5 => "REG_DWORD_BIG_ENDIAN".to_string(),
        6 => "REG_LINK".to_string(),
        7 => "REG_MULTI_SZ".to_string(),
        8 => "REG_RESOURCE_LIST".to_string(),
        11 => "REG_QWORD".to_string(),
        other => format!("registry type {other}"),
    };
    format!("a value stored as {name}, which this installer never writes")
}

/// A decoded string, or the one thing that can be said about why it is not one.
enum Unescaped {
    Ok(String),
    Bad(&'static str),
}

/// Undo wine's escaping.
///
/// Every escape wine's writer can produce and nothing else. An escape this does
/// not know is [`Unescaped::Bad`] rather than a backslash followed by a letter:
/// a wrong guess here becomes a value the caller compares against its own
/// paths, and the whole point of the three-way answer is that a value we cannot
/// read is never quietly turned into one we can.
fn unescape(s: &str) -> Unescaped {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(e) = it.next() else {
            return Unescaped::Bad("a value that ends in the middle of an escape");
        };
        let plain = match e {
            '\\' => Some('\\'),
            '"' => Some('"'),
            '[' => Some('['),
            ']' => Some(']'),
            'a' => Some('\u{7}'),
            'b' => Some('\u{8}'),
            't' => Some('\t'),
            'n' => Some('\n'),
            'v' => Some('\u{b}'),
            'f' => Some('\u{c}'),
            'r' => Some('\r'),
            'e' => Some('\u{1b}'),
            _ => None,
        };
        if let Some(p) = plain {
            out.push(p);
            continue;
        }
        // `\x` names a UTF-16 code unit in one to four hex digits; an escape
        // that begins with an octal digit names one in up to three octal
        // digits, which is wine's spelling for a control character it has no
        // letter for. The same five steps over different numbers, so one
        // reader, with the branch reduced to picking the numbers.
        let numeric = match e {
            'x' => Some((16, 4, None)),
            '0'..='7' => Some((8, 3, Some(e))),
            _ => None,
        };
        if let Some((radix, max, seed)) = numeric {
            match escape_digits(&mut it, radix, max, seed) {
                Ok(ch) => out.push(ch),
                Err(why) => return Unescaped::Bad(why),
            }
            continue;
        }
        return Unescaped::Bad("a value holding an escape this installer cannot read");
    }
    Unescaped::Ok(out)
}

/// Read the digits of a numeric escape and turn them into a character.
///
/// `seed` is the digit the escape has already named — octal spells its first
/// digit in the escape itself and `\x` does not — `max` is how many digits the
/// escape can hold, and `radix` says which characters count as digits.
fn escape_digits(
    it: &mut std::iter::Peekable<std::str::Chars<'_>>,
    radix: u32,
    max: usize,
    seed: Option<char>,
) -> Result<char, &'static str> {
    // Greedily, to `max` and no further — wine pads to four exactly when
    // the character after the escape is itself a hex digit, so reading
    // four whenever four are there is what puts `C:\\\x00fcab` back
    // together as `C:\üab` instead of `C:\ずab`.
    let mut digits = String::new();
    digits.extend(seed);
    while digits.len() < max && it.peek().is_some_and(|c| c.is_digit(radix)) {
        digits.push(it.next().unwrap_or_default());
    }
    if digits.is_empty() {
        return Err("a value holding an escape this installer cannot read");
    }
    // Infallible: at most four hex digits is at most 0xffff.
    let Ok(unit) = u32::from_str_radix(&digits, radix) else {
        return Err("a value holding an escape this installer cannot read");
    };
    // A UTF-16 code unit, so a lone surrogate is possible and is not a
    // character. Wine stores keys as UTF-16 and a surrogate pair would
    // arrive as two escapes; putting one back together is guesswork
    // this file does not do, and a half of one is certainly not a path.
    char::from_u32(unit).ok_or("a value holding a character this installer cannot read back")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FT: &str = r"Software\Freetrack\FreeTrackClient";
    const NP: &str = r"Software\NaturalPoint\NATURALPOINT\NPClient Location";

    /// Copied byte for byte out of a `user.reg` written by wine 11.18 on this
    /// machine, from a prefix booted with `wineboot -i` and then given each
    /// value with `wine reg add`. Nothing here is invented, which is the point:
    /// a parser proved only against its author's idea of the format is a parser
    /// proved against nothing.
    const REAL: &str = concat!(
        "WINE REGISTRY Version 2\n",
        ";; All keys relative to \\\\User\\\\S-1-5-21-0-0-0-1000\n",
        "\n",
        "#arch=win64\n",
        "\n",
        "[Software\\\\Freetrack\\\\FreeTrackClient] 1790444400\n",
        "#time=1dd4dde0e4c32d6\n",
        "\"Path\"=\"C:\\\\tobii-bridge\"\n",
        "\n",
        "[Software\\\\NaturalPoint\\\\NATURALPOINT\\\\NPClient Location] 1790444400\n",
        "#time=1dd4dde0e5bceee\n",
        "\"Path\"=\"C:\\\\Program Files\\\\M\\xfcller opentrack\"\n",
        "\n",
        "[Software\\\\T\\\\dword] 1790444401\n",
        "\"Path\"=dword:00000001\n",
        "\n",
        "[Software\\\\T\\\\empty] 1790444401\n",
        "\"Path\"=\"\"\n",
        "\n",
        "[Software\\\\T\\\\expand] 1790444401\n",
        "\"Path\"=str(2):\"%ProgramFiles%\\\\opentrack\"\n",
        "\n",
        "[Software\\\\T\\\\lower] 1790444401\n",
        "\"path\"=\"C:\\\\lower\"\n",
        "\n",
        "[Software\\\\T\\\\multi] 1790444401\n",
        "\"Path\"=str(7):\"a\\0b\\0\"\n",
        "\n",
        "[Software\\\\T\\\\nopathval] 1790444401\n",
        "\"Zed\"=\"z\"\n",
        "\n",
        "[Software\\\\T\\\\quote] 1790444401\n",
        "\"Path\"=\"C:\\\\a\\\"b\\\\c\"\n",
        "\n",
        "[Software\\\\T\\\\trailsp] 1790444401\n",
        "\"Path\"=\"C:\\\\x \"\n",
        "\n",
        "[Software\\\\T\\\\amb] 1790444434\n",
        "\"Path\"=\"C:\\\\\\x00fcab\"\n",
        "\n",
        "[Software\\\\T\\\\cjk] 1790444434\n",
        "\"Path\"=\"C:\\\\\\x65e5\\x672c\"\n",
        "\n",
        "[Software\\\\T\\\\nl] 1790444434\n",
        "\"Path\"=\"C:\\\\a\\nb\"\n",
        "\n",
        "[Software\\\\T\\\\bin] 1790444501\n",
        "\"Path\"=hex:61,00,62,00,63,00\n",
        "\n",
        "[Software\\\\T\\\\qword] 1790444501\n",
        "\"Path\"=hex(b):05,00,00,00,00,00,00,00\n",
        "\n",
        "[Software\\\\T\\\\br\\[a\\]ck] 1790444501\n",
        "\"Path\"=\"C:\\\\b\"\n",
        "\n",
    );

    fn look(key: &str) -> Lookup {
        lookup(REAL.as_bytes(), key, "Path")
    }

    /// A scratch `user.reg`: the one line wine insists on, then these bytes.
    ///
    /// Every fixture that means to exercise the parser goes through this,
    /// because a file without that line is one wine refuses whole — which is a
    /// different fact, tested on its own in
    /// [`a_file_wine_will_not_load_is_not_a_registration`].
    fn file(rest: &[u8]) -> Vec<u8> {
        let mut out = format!("{FILE_HEADER}\n\n").into_bytes();
        out.extend_from_slice(rest);
        out
    }

    /// The same, with a FreeTrack section header in front of `rest`, built from
    /// the one spelling of that header rather than from a fresh hand-escaping
    /// each time. Several tests below assert [`Lookup::Absent`], and `Absent`
    /// is also what a mis-escaped header produces, so a typo in one copy of
    /// that header would make its test pass for the wrong reason.
    fn ft_file(rest: &str) -> Vec<u8> {
        ft_bytes(rest.as_bytes())
    }

    /// The same, for the fixtures whose body is deliberately not UTF-8.
    fn ft_bytes(rest: &[u8]) -> Vec<u8> {
        let mut out = format!("[{}] 1790444400\n", FT.replace('\\', r"\\")).into_bytes();
        out.extend_from_slice(rest);
        file(&out)
    }

    /// The whole reason this module exists: a real prefix's real file, read
    /// without a wine anywhere near it.
    /// Wine writes its own file hard against the left margin, but it READS a
    /// leading space fine — and a `user.reg` may have been hand-edited or
    /// written by something else. Reported by a reviewer who put real wine and
    /// this parser side by side on the same file: wine answered
    /// `Path REG_SZ C:\\tobii-bridge`, this said nothing was registered.
    #[test]
    fn an_indented_line_is_read_the_way_wine_reads_it() {
        let indented = file(
            b"  [Software\\\\Freetrack\\\\FreeTrackClient]\n  \"Path\"=\"C:\\\\tobii-bridge\"\n",
        );
        assert_eq!(
            lookup(&indented, FT, "Path"),
            Lookup::Text("C:\\tobii-bridge".to_string()),
            "an indented header and value are still a header and a value"
        );
    }

    /// A value's NAME carries the same escaping its contents do, so wine reads
    /// `"P\x0061th"` as `Path`. Comparing the raw bytes called that absent —
    /// and "absent" is the one answer that must never be wrong here, because
    /// it is the answer that says nobody else has claimed the key.
    #[test]
    fn an_escaped_value_name_is_decoded_before_it_is_compared() {
        let escaped = ft_file("\"P\\x0061th\"=\"C:\\\\tobii-bridge\"\n");
        assert_eq!(
            lookup(&escaped, FT, "Path"),
            Lookup::Text("C:\\tobii-bridge".to_string()),
            "wine decodes the name; so must this"
        );

        // And a name this parser cannot decode is refused, never skipped: an
        // undecodable name in our own section may be the one being asked for.
        let bad = ft_file("\"P\\q\"=\"C:\\\\x\"\n");
        assert!(
            matches!(lookup(&bad, FT, "Path"), Lookup::Rejected(_)),
            "a name that cannot be read is not proof the value is absent"
        );
    }

    #[test]
    fn both_discovery_keys_are_read_out_of_a_real_user_reg() {
        assert_eq!(look(FT), Lookup::Text(r"C:\tobii-bridge".to_string()));
        assert_eq!(
            look(NP),
            Lookup::Text(r"C:\Program Files\Müller opentrack".to_string())
        );
    }

    /// The doubled backslashes of the section header are the format, not a
    /// typo: a parser matching the key with single ones finds nothing and
    /// reports every prefix as empty.
    #[test]
    fn a_key_spelled_with_single_backslashes_matches_the_doubled_header() {
        assert!(header_names(
            r"[Software\\Freetrack\\FreeTrackClient] 1790444400",
            FT
        ));
        assert!(!header_names(
            r"[Software\\Freetrack\\FreeTrackClientX] 1",
            FT
        ));
        // And the escaped brackets wine writes inside a key name.
        assert!(header_names(
            r"[Software\\T\\br\[a\]ck] 1",
            r"Software\T\br[a]ck"
        ));
    }

    /// Registry keys and value names are case-insensitive on Windows, so a
    /// report that reads them case-sensitively tells a user a key is empty
    /// while their game is reading a path out of it.
    #[test]
    fn the_key_and_the_value_name_are_matched_the_way_windows_matches_them() {
        assert_eq!(
            lookup(
                REAL.as_bytes(),
                r"software\freetrack\freetrackclient",
                "PATH"
            ),
            Lookup::Text(r"C:\tobii-bridge".to_string())
        );
        assert_eq!(
            lookup(REAL.as_bytes(), r"Software\T\lower", "Path"),
            Lookup::Text(r"C:\lower".to_string())
        );
    }

    /// Absent means absent: no section, or a section with no such value.
    #[test]
    fn a_key_that_is_not_there_and_a_value_that_is_not_there_are_both_absent() {
        assert_eq!(look(r"Software\T\nothing"), Lookup::Absent);
        assert_eq!(look(r"Software\T\nopathval"), Lookup::Absent);
    }

    /// The rule the caller rests on: nothing unreadable is ever an absence.
    /// Each of these is a real shape wine wrote, and each has to come back as
    /// something the caller will refuse to touch.
    #[test]
    fn every_shape_this_parser_cannot_read_is_refused_and_never_called_absent() {
        for (key, expect) in [
            (r"Software\T\expand", "REG_EXPAND_SZ"),
            (r"Software\T\dword", "REG_DWORD"),
            (r"Software\T\multi", "REG_MULTI_SZ"),
            (r"Software\T\bin", "REG_BINARY"),
            (r"Software\T\qword", "REG_QWORD"),
        ] {
            match look(key) {
                Lookup::Rejected(why) => assert!(why.contains(expect), "{key}: {why}"),
                other => panic!("{key} read as {other:?}"),
            }
        }
        for key in [r"Software\T\empty", r"Software\T\nl"] {
            assert!(
                matches!(look(key), Lookup::Rejected(_)),
                "{key} was not refused"
            );
        }
    }

    /// A value ending in a space and a value holding a quote both survive
    /// intact. The first is the one that matters: install can register a client
    /// directory whose path ends in a space, and a reader that trimmed it would
    /// produce a registration this program could never recognise as its own
    /// again.
    #[test]
    fn a_value_is_decoded_to_its_last_character() {
        assert_eq!(
            look(r"Software\T\trailsp"),
            Lookup::Text("C:\\x ".to_string())
        );
        assert_eq!(
            look(r"Software\T\quote"),
            Lookup::Text("C:\\a\"b\\c".to_string())
        );
    }

    /// Wine pads a hex escape to four digits exactly when the next character is
    /// itself a hex digit, and that padding is the only thing that keeps the
    /// escape from swallowing the text after it. Reading fewer than four digits
    /// turns `C:\üab` into `C:\ü` plus nothing, and reading them non-greedily
    /// when four are meant turns `C:\日本` into something else again.
    #[test]
    fn a_hex_escape_stops_where_wine_meant_it_to() {
        assert_eq!(look(r"Software\T\amb"), Lookup::Text("C:\\üab".to_string()));
        assert_eq!(
            look(r"Software\T\cjk"),
            Lookup::Text("C:\\日本".to_string())
        );
    }

    /// A header the parser cannot read still *ends* the section before it.
    /// Otherwise a value belonging to some other program's key is read as the
    /// answer for ours — the one mistake in this file that could delete a
    /// registration we never made.
    #[test]
    fn an_unreadable_header_does_not_leave_the_previous_section_open() {
        let text = ft_bytes(b"[\xff\xfe] 2\n\"Path\"=\"EVIL\"\n");
        assert_eq!(lookup(&text, FT, "Path"), Lookup::Absent);
    }

    /// The same, for a header whose escaping this parser refuses.
    #[test]
    fn a_header_with_an_escape_we_do_not_know_closes_the_section_too() {
        let text = ft_file("[Software\\\\T\\q] 2\n\"Path\"=\"EVIL\"\n");
        assert_eq!(lookup(&text, FT, "Path"), Lookup::Absent);
    }

    /// A value name is matched whole. `PathX` is not `Path`, and a name
    /// carrying an escaped quote does not end where that quote is.
    #[test]
    fn a_value_name_is_matched_whole() {
        let text = ft_file(concat!(
            "\"PathX\"=\"NO\"\n",
            "\"Pa\\\"th\"=\"NO\"\n",
            "\"Path\"=\"YES\"\n"
        ));
        assert_eq!(lookup(&text, FT, "Path"), Lookup::Text("YES".to_string()));
    }

    /// Bytes that are not text cannot be turned into a value, and must not be
    /// turned into an absence either.
    #[test]
    fn a_value_line_that_is_not_text_is_refused() {
        let text = ft_bytes(b"\"Path\"=\"\xff\xfe\"\n");
        assert!(matches!(lookup(&text, FT, "Path"), Lookup::Rejected(_)));
    }

    /// An escape this parser does not know is refused rather than guessed at.
    /// Guessing produces a string the caller then compares against its own
    /// paths, which is the one thing the three-way answer exists to prevent.
    #[test]
    fn an_unknown_escape_is_refused() {
        for body in [r"C:\q", r"C:\", r"C:\x"] {
            let text = ft_file(&format!("\"Path\"=\"{body}\"\n"));
            assert!(
                matches!(lookup(&text, FT, "Path"), Lookup::Rejected(_)),
                "{body} was not refused"
            );
        }
    }

    /// `str(1):` is REG_SZ written the long way, and is read; every other
    /// `str(N)` is a type this installer never writes.
    #[test]
    fn a_string_written_the_long_way_is_still_a_string() {
        let text = ft_file("\"Path\"=str(1):\"C:\\\\x\"\n");
        assert_eq!(lookup(&text, FT, "Path"), Lookup::Text(r"C:\x".to_string()));
    }

    /// Anything after the closing quote means the line is not the shape this
    /// parser proved against wine, and which half is the value is then a guess.
    #[test]
    fn a_line_with_something_after_the_value_is_refused() {
        let text = ft_file("\"Path\"=\"C:\\\\x\" junk\n");
        assert!(matches!(lookup(&text, FT, "Path"), Lookup::Rejected(_)));
    }

    /// CRLF line ends, which a file copied off a Windows filesystem has.
    #[test]
    fn carriage_returns_at_the_ends_of_lines_are_not_part_of_anything() {
        let text = format!(
            "{FILE_HEADER}\r\n[Software\\\\Freetrack\\\\FreeTrackClient] 1\r\n\"Path\"=\"C:\\\\x\"\r\n"
        );
        assert_eq!(
            lookup(text.as_bytes(), FT, "Path"),
            Lookup::Text(r"C:\x".to_string())
        );
    }

    /// A prefix whose `user.reg` holds only its header is an absence and not a
    /// failure: a booted prefix nothing has registered in really does hold
    /// nothing under these keys. Measured — wine answers "key not found" for
    /// this file and says nothing about the file itself.
    ///
    /// An empty file is NOT that case, which is why it moved out of this test:
    /// wine 11.18 prints `user.reg is not a valid registry file` for a
    /// zero-byte one and loads no `HKCU` at all.
    #[test]
    fn a_file_with_no_sections_is_an_absence() {
        assert_eq!(
            lookup(b"WINE REGISTRY Version 2\n\n#arch=win64\n", FT, "Path"),
            Lookup::Absent
        );
        // The same file with nothing after the header at all, not even a
        // newline — wine reads that one too.
        assert_eq!(lookup(FILE_HEADER.as_bytes(), FT, "Path"), Lookup::Absent);
    }

    /// Wine refuses a `user.reg` whose first line is not exactly
    /// `WINE REGISTRY Version 2`: it loads no `HKCU`, so every key in the
    /// prefix is gone, and `reg query` answers "key not found" for a
    /// registration that is sitting right there in the file.
    ///
    /// Each of these is a file real wine 11.18 was pointed at, on a throwaway
    /// prefix holding a genuine `[Software\\Freetrack\\FreeTrackClient]` /
    /// `"Path"="C:\\tobii-bridge"` — every one printed `user.reg is not a
    /// valid registry file` and exited 1, and wine did not rewrite any of them.
    /// Reading straight past the line made `tobii bridge status` print the path
    /// and "registered by this installer" for all of them.
    #[test]
    fn a_file_wine_will_not_load_is_not_a_registration() {
        let good = format!(
            "{FILE_HEADER}\n\n[{}] 1\n\"Path\"=\"C:\\\\tobii-bridge\"\n",
            FT.replace('\\', r"\\")
        );
        // The control: the same bytes, read.
        assert_eq!(
            lookup(good.as_bytes(), FT, "Path"),
            Lookup::Text(r"C:\tobii-bridge".to_string())
        );
        let body = good.split_once('\n').expect("a second line").1;
        for (what, text) in [
            (
                "a UTF-8 BOM in front of the line",
                format!("\u{feff}{good}"),
            ),
            ("no header line at all", body.to_string()),
            ("a blank line before it", format!("\n{good}")),
            ("one leading space", format!(" {good}")),
            ("one trailing space", format!("{FILE_HEADER} \n{body}")),
            (
                "the wrong version",
                format!("WINE REGISTRY Version 1\n{body}"),
            ),
            (
                "the words in lower case",
                format!("wine registry version 2\n{body}"),
            ),
            ("no bytes at all", String::new()),
        ] {
            match lookup(text.as_bytes(), FT, "Path") {
                Lookup::Rejected(why) => assert!(
                    why.contains(FILE_HEADER),
                    "{what}: the refusal has to name the line — {why}"
                ),
                other => panic!("{what} was read as {other:?}"),
            }
        }
    }

    /// Wine ignores ONE empty component at the end of a key name, so a header
    /// spelled with a trailing key separator names the same key. Measured: with
    /// `[Software\\Freetrack\\FreeTrackClient\\]` in a real prefix's user.reg,
    /// `wine reg query HKCU\Software\Freetrack\FreeTrackClient /v Path`
    /// answered `C:\sep`, while a whole-string compare called it absent — the
    /// one answer this module must never get wrong.
    #[test]
    fn a_header_with_a_trailing_key_separator_names_the_same_key() {
        let text = file(
            b"[Software\\\\Freetrack\\\\FreeTrackClient\\\\] 1790444400\n\"Path\"=\"C:\\\\sep\"\n",
        );
        assert_eq!(
            lookup(&text, FT, "Path"),
            Lookup::Text(r"C:\sep".to_string())
        );

        // And nothing more forgiving than that one. A leading separator, a
        // doubled one and two trailing ones each killed the wineserver outright
        // when they were put in a real prefix (`wine client error:0: recvmsg:
        // connection reset`), so nothing can be read out of such a file at all
        // and a parser that matched them would be answering for a registry no
        // process can open.
        for header in [
            r"[\\Software\\Freetrack\\FreeTrackClient] 1",
            r"[Software\\\\Freetrack\\FreeTrackClient] 1",
            r"[Software\\Freetrack\\FreeTrackClient\\\\] 1",
        ] {
            let text = file(format!("{header}\n\"Path\"=\"C:\\\\no\"\n").as_bytes());
            assert_eq!(lookup(&text, FT, "Path"), Lookup::Absent, "{header}");
        }
    }

    /// Wine's loader applies the file top to bottom into one tree, so the LAST
    /// spelling of a key or a value is the one left in memory. All four of
    /// these were put in a real prefix and answered `C:\SECOND`; this parser,
    /// returning at its first match, answered `C:\FIRST` — a path the game
    /// never sees, and an ownership verdict drawn from the wrong one.
    #[test]
    fn a_key_or_a_value_written_twice_reads_the_way_wine_reads_it() {
        let hdr = format!("[{}] 1", FT.replace('\\', r"\\"));
        for (what, body) in [
            (
                "the section repeated",
                format!("{hdr}\n\"Path\"=\"C:\\\\FIRST\"\n\n{hdr}\n\"Path\"=\"C:\\\\SECOND\"\n"),
            ),
            (
                "the section respelled in another case",
                format!(
                    "{hdr}\n\"Path\"=\"C:\\\\FIRST\"\n\n\
                     [SOFTWARE\\\\FREETRACK\\\\FREETRACKCLIENT] 2\n\"Path\"=\"C:\\\\SECOND\"\n"
                ),
            ),
            (
                "the value repeated",
                format!("{hdr}\n\"Path\"=\"C:\\\\FIRST\"\n\"Path\"=\"C:\\\\SECOND\"\n"),
            ),
            (
                "the value respelled in another case",
                format!("{hdr}\n\"Path\"=\"C:\\\\FIRST\"\n\"PATH\"=\"C:\\\\SECOND\"\n"),
            ),
        ] {
            assert_eq!(
                lookup(&file(body.as_bytes()), FT, "Path"),
                Lookup::Text(r"C:\SECOND".to_string()),
                "{what}"
            );
        }
    }

    /// Wine tolerates whitespace around the `=` and after the closing quote,
    /// and this parser did not. Every one of these six was read back as its
    /// path by real wine 11.18, while this answered "a value line this
    /// installer cannot read" — about a key that may hold, byte for byte, this
    /// installer's own directory, and that `uninstall` reads perfectly well
    /// because it goes through wine.
    #[test]
    fn whitespace_around_the_assignment_is_read_the_way_wine_reads_it() {
        for (line, want) in [
            ("\"Path\" = \"C:\\\\spaced\"\n", r"C:\spaced"),
            ("\"Path\"= \"C:\\\\y\"\n", r"C:\y"),
            ("\"Path\" =\"C:\\\\z\"\n", r"C:\z"),
            ("\"Path\"\t=\t\"C:\\\\tabbed\"\n", r"C:\tabbed"),
            ("\"Path\"=\"C:\\\\x\"   \n", r"C:\x"),
            ("\"Path\"=\"C:\\\\w\"\t\n", r"C:\w"),
        ] {
            assert_eq!(
                lookup(&ft_file(line), FT, "Path"),
                Lookup::Text(want.to_string()),
                "{line:?}"
            );
        }

        // The refusal this keeps is the one its comment is actually about:
        // something that is not whitespace after the closing quote, where which
        // half is the value would be a guess. Covered by
        // `a_line_with_something_after_the_value_is_refused`; asserted here too
        // because the whitespace rule is what could erode it.
        assert!(matches!(
            lookup(&ft_file("\"Path\"=\"C:\\\\x\" junk\n"), FT, "Path"),
            Lookup::Rejected(_)
        ));
    }
}
