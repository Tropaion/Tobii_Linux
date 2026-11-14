//! COSMIC's custom keyboard shortcuts, and the one this program puts there:
//! a key that runs `tobii recentre`.
//!
//! # Why a file and not the desktop's portal
//!
//! Under Wayland a program cannot see keys pressed while another window — the
//! game — has focus. The standard way to ask for one is the desktop portal's
//! `GlobalShortcuts` interface, which KDE and GNOME implement and COSMIC does
//! not (checked on COSMIC with cosmic-comp 1.0: no such interface on
//! `org.freedesktop.portal.Desktop`). COSMIC instead has user-defined
//! shortcuts that run a command, kept in a RON file the compositor watches,
//! and picks a change up without a restart.
//!
//! Here and not in the hub, which sets the key, because `tobii uninstall`
//! removes it: a shortcut left behind runs a command that is gone, and COSMIC
//! still takes the key from every other program.
//!
//! # Editing a file this program does not own
//!
//! The file belongs to COSMIC Settings, which also writes it. Only the one
//! entry whose description is [`DESCRIPTION`] is ever replaced or removed; every
//! other entry is carried over byte for byte. The file is split into top-level
//! map entries by bracket depth and string quoting rather than parsed into a
//! model and re-serialised, so nothing this module does not understand can be
//! normalised away. If the file is not a map at all, it is left alone and the
//! user is told.

use std::path::{Path, PathBuf};

/// The description that marks the entry as ours. Shown by COSMIC Settings
/// beside the binding, so it is written for a person.
pub const DESCRIPTION: &str = "TobiiLinux: recentre head tracking";

/// A key combination in COSMIC's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// `Super`, `Ctrl`, `Alt`, `Shift`, in that order, each at most once.
    pub modifiers: Vec<&'static str>,
    /// An xkb keysym name: `r`, `F9`, `Pause`, `KP_5`.
    pub key: String,
}

const MODIFIER_ORDER: [&str; 4] = ["Super", "Ctrl", "Alt", "Shift"];

impl Binding {
    /// Build one, normalising the modifier order and a letter key's case
    /// (COSMIC's own defaults spell letters lower-case).
    pub fn new(modifiers: &[&str], key: &str) -> Binding {
        let modifiers = MODIFIER_ORDER
            .iter()
            .copied()
            .filter(|m| modifiers.iter().any(|x| x.eq_ignore_ascii_case(m)))
            .collect();
        let key = if key.chars().count() == 1 {
            key.to_lowercase()
        } else {
            key.to_string()
        };
        Binding { modifiers, key }
    }

    /// `Ctrl+Shift+R`, for a label.
    pub fn display(&self) -> String {
        let key = if self.key.chars().count() == 1 {
            self.key.to_uppercase()
        } else {
            self.key.clone()
        };
        let mut parts: Vec<String> = self.modifiers.iter().map(|m| m.to_string()).collect();
        parts.push(key);
        parts.join("+")
    }

    /// The RON for this binding's key, as COSMIC writes it.
    fn to_ron(&self) -> String {
        format!(
            "(modifiers: [{}], key: \"{}\", description: Some(\"{}\"))",
            self.modifiers.join(", "),
            self.key,
            DESCRIPTION
        )
    }

    /// Whether binding this, with no modifier or with Shift alone, would take
    /// a key away from typing. Function keys, Pause, Scroll Lock, Print and
    /// media keys are fine that way; everything else is not.
    ///
    /// A list of the keys that are fine, not of the ones that type, because
    /// the second list has no end: GDK names most typing keys with more than
    /// one character — `minus`, `odiaeresis` (ö), `ssharp` (ß), `KP_5` — and
    /// AltGr is not one of the modifiers recorded, so AltGr+Q arrives here as
    /// a bare `at`. A deny-list missed every one of those, and bound globally
    /// each stops that character typing anywhere on the desktop.
    ///
    /// Shift counts as no modifier because it changes which character a key
    /// types, not whether it types: Shift+ö is Ö, and Shift+a is A.
    pub fn steals_typing(&self) -> bool {
        let k = self.key.as_str();
        let function_key = k
            .strip_prefix('F')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        let fine_bare = function_key
            || k.starts_with("XF86")
            || matches!(k, "Pause" | "Break" | "Scroll_Lock" | "Print" | "Sys_Req");
        self.modifiers.iter().all(|m| *m == "Shift") && !fine_bare
    }
}

/// Whether this session is COSMIC.
pub fn is_cosmic() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .map(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("COSMIC")))
        .unwrap_or(false)
}

const SHORTCUTS_DIR: &str = "cosmic/com.system76.CosmicSettings.Shortcuts/v1";

/// The user's custom-shortcut file.
pub fn custom_path() -> PathBuf {
    custom_path_in(&crate::paths::config_home())
}

/// [`custom_path`] under the configuration directory `config_home`.
pub fn custom_path_in(config_home: &Path) -> PathBuf {
    config_home.join(SHORTCUTS_DIR).join("custom")
}

/// The files holding the bindings COSMIC already has: its shipped defaults and
/// the user's overrides of them. Consulted for conflicts only.
fn taken_paths() -> [PathBuf; 2] {
    [
        PathBuf::from("/usr/share").join(SHORTCUTS_DIR),
        crate::paths::config_home().join(SHORTCUTS_DIR),
    ]
    .map(|d| d.join("defaults"))
}

/// The text between the outer `{` and `}` split into top-level entries, each
/// trimmed and without its separating comma. `None` if the text is not a map.
fn split_entries(text: &str) -> Option<Vec<&str>> {
    let body = text.trim();
    let body = body.strip_prefix('{')?.strip_suffix('}')?;
    let (kinds, closed) = lex(body);
    if !closed {
        return None;
    }
    let mut entries = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, (&c, &kind)) in body.as_bytes().iter().zip(&kinds).enumerate() {
        if kind != Lex::Code {
            continue;
        }
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                entries.push(body[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    if depth != 0 {
        return None;
    }
    entries.push(body[start..].trim());
    Some(entries.into_iter().filter(|e| !e.is_empty()).collect())
}

/// What one byte of a RON text is part of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lex {
    Code,
    /// A string, its quotes included.
    Str,
    /// A `//` or `/* */` comment, its delimiters included.
    Comment,
}

/// Every byte of `text`, classified, and whether the text ended outside any
/// string or block comment.
///
/// One scanner for every reader of comments in this file, so they cannot
/// disagree about where a comment is. They did: `value_span` knew only `//`,
/// took the one inside `/* see https://x.org */` for a line comment, and put
/// the separating comma inside the block comment. Block comments nest, as in
/// Rust; a file that only parses if they do not is left unclosed here, and so
/// refused rather than misread.
fn lex(text: &str) -> (Vec<Lex>, bool) {
    let b = text.as_bytes();
    let mut kinds = Vec::with_capacity(b.len());
    let (mut in_str, mut escaped, mut line) = (false, false, false);
    let mut block = 0u32;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let pair = |second: u8| b.get(i + 1) == Some(&second);
        if line {
            line = c != b'\n';
            kinds.push(Lex::Comment);
        } else if block > 0 || (!in_str && c == b'/' && pair(b'*')) {
            if c == b'/' && pair(b'*') {
                block += 1;
            } else if c == b'*' && pair(b'/') {
                block -= 1;
            } else {
                kinds.push(Lex::Comment);
                i += 1;
                continue;
            }
            kinds.extend([Lex::Comment; 2]);
            i += 2;
            continue;
        } else if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            kinds.push(Lex::Str);
        } else if c == b'/' && pair(b'/') {
            line = true;
            kinds.push(Lex::Comment);
        } else if c == b'"' {
            in_str = true;
            kinds.push(Lex::Str);
        } else {
            kinds.push(Lex::Code);
        }
        i += 1;
    }
    (kinds, !in_str && block == 0)
}

/// `entry` with its comments blanked out, for the readers that look for words
/// in it: what a comment says is not part of the entry. A comment swallows the
/// comma after it, so a commented-out copy of ours in front of one of the
/// user's own shortcuts is split off as one entry with it, and read with the
/// comment in, the user's shortcut looked like ours — and was deleted with it.
fn code_of(entry: &str) -> String {
    let (kinds, _) = lex(entry);
    entry
        .char_indices()
        .map(|(i, c)| if kinds[i] == Lex::Comment { ' ' } else { c })
        .collect()
}

/// The binding written in one entry's key, if it can be read.
fn entry_binding(entry: &str) -> Option<Binding> {
    let entry = &code_of(entry);
    let m = entry.find("modifiers:")?;
    let open = m + entry[m..].find('[')?;
    let close = open + entry[open..].find(']')?;
    let mods: Vec<&str> = entry[open + 1..close]
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let k = entry.find("key:")?;
    let q1 = k + entry[k..].find('"')? + 1;
    let q2 = q1 + entry[q1..].find('"')?;
    Some(Binding::new(&mods, &entry[q1..q2]))
}

fn is_ours(entry: &str) -> bool {
    code_of(entry).contains(&format!("\"{DESCRIPTION}\""))
}

/// `text` with the recentre entry replaced by `binding` running `command`, or
/// removed when `binding` is `None`. Every other entry is kept verbatim.
pub fn with_binding(
    text: &str,
    binding: Option<&Binding>,
    command: &str,
) -> Result<String, SetError> {
    let entries = if text.trim().is_empty() {
        Vec::new()
    } else {
        split_entries(text).ok_or_else(|| {
            SetError::File(
                "COSMIC's custom-shortcut file is not in a format this program can edit \
                 safely, so it was left alone — change the shortcut in COSMIC Settings instead"
                    .to_string(),
            )
        })?
    };
    let mut out: Vec<String> = Vec::new();
    for e in entries {
        if is_ours(e) {
            // Ours goes; the user's comments in front of it stay. A comment
            // after the previous entry's comma is split off with this entry,
            // and writing ours behind a trailing comment puts one exactly
            // there (see `value_span`).
            if let Some((start, _)) = value_span(e) {
                let lead = e[..start].trim();
                if !lead.is_empty() {
                    out.push(lead.to_string());
                }
            }
            continue;
        }
        if let (Some(b), Some(theirs)) = (binding, entry_binding(e)) {
            if *b == theirs {
                return Err(SetError::Key(format!(
                    "{} is already one of your custom shortcuts in COSMIC",
                    b.display()
                )));
            }
        }
        out.push(e.to_string());
    }
    if let Some(b) = binding {
        let cmd = command.replace('\\', "\\\\").replace('"', "\\\"");
        out.push(format!("{}: Spawn(\"{cmd}\")", b.to_ron()));
    }
    let mut s = String::from("{\n");
    for e in out {
        s.push_str("    ");
        match value_span(&e) {
            Some((_, end)) => {
                s.push_str(&e[..end]);
                s.push(',');
                s.push_str(&e[end..]);
            }
            // Only comments: there is no value to separate.
            None => s.push_str(&e),
        }
        s.push('\n');
    }
    s.push('}');
    Ok(s)
}

/// Where the value in `entry` starts and ends: its first character outside a
/// comment, and just past its last one. `None` for an entry that is nothing
/// but comments.
///
/// The separating comma goes at the end. Appended to the end of the entry
/// instead, it lands inside a trailing comment — `Spawn("cosmic-term") //
/// mine,` — and two entries are left with no separator between them, which is
/// not RON. [`split_entries`] keeps such a comment with the entry before it
/// whenever no comma follows the value, which in a hand-edited file is the
/// last entry.
fn value_span(entry: &str) -> Option<(usize, usize)> {
    let (kinds, _) = lex(entry);
    let mut span: Option<(usize, usize)> = None;
    for (i, (&c, &kind)) in entry.as_bytes().iter().zip(&kinds).enumerate() {
        let part_of_value = match kind {
            Lex::Str => true,
            Lex::Code => !c.is_ascii_whitespace(),
            Lex::Comment => false,
        };
        if part_of_value {
            span = Some((span.map_or(i, |(start, _)| start), i + 1));
        }
    }
    span
}

/// Whether COSMIC already uses `b` for one of its own actions.
fn taken_by_cosmic(b: &Binding) -> bool {
    taken_paths().iter().any(|p| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|t| {
                split_entries(&t).map(|es| {
                    es.into_iter()
                        .filter_map(entry_binding)
                        .any(|theirs| theirs == *b)
                })
            })
            .unwrap_or(false)
    })
}

/// The recentre shortcut as COSMIC's file has it.
#[derive(Debug, PartialEq, Eq)]
pub enum Current {
    /// Bound, to this.
    Key(Binding),
    /// Not bound: no file, or nothing of ours in it.
    Unbound,
    /// Not known: the file cannot be read or taken apart, or ours is in it
    /// with a key this program cannot read back. Said apart from `Unbound`,
    /// because "none" would be a claim, and a key may well be bound.
    Unknown,
}

/// The recentre shortcut on this machine.
pub fn current() -> Current {
    read_custom(&custom_path()).map_or(Current::Unknown, |text| current_of(&text))
}

/// Whether `text`, a custom-shortcut file, holds the recentre shortcut in a
/// form this module can take out of it — readable command or not.
pub fn holds_ours(text: &str) -> bool {
    split_entries(text).is_some_and(|es| es.iter().any(|e| is_ours(e)))
}

/// The command the recentre shortcut in `text`, a custom-shortcut file, runs:
/// what `Spawn("…")` holds, unescaped. `None` when there is no shortcut of
/// ours, or its value is not a `Spawn` of a plain string.
pub fn our_command(text: &str) -> Option<String> {
    let entry = split_entries(text)?.into_iter().find(|e| is_ours(e))?;
    let (start, end) = value_span(entry)?;
    // After the key: the first `Spawn(` there is, since the key's own fields
    // hold none and the command may.
    let code = &entry[start..end];
    let quoted = code[code.find("Spawn(")?..]
        .strip_prefix("Spawn(")?
        .strip_suffix(')')?
        .trim()
        .strip_prefix('"')?
        .strip_suffix('"')?;
    // The two escapes `with_binding` writes, and nothing it does not.
    let mut out = String::new();
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next()? {
                e @ ('\\' | '"') => out.push(e),
                _ => return None,
            },
            '"' => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

/// [`current`] for the file's text.
fn current_of(text: &str) -> Current {
    if text.trim().is_empty() {
        return Current::Unbound;
    }
    let Some(entries) = split_entries(text) else {
        return Current::Unknown;
    };
    match entries.into_iter().find(|e| is_ours(e)) {
        None => Current::Unbound,
        Some(e) => entry_binding(e).map_or(Current::Unknown, Current::Key),
    }
}

/// COSMIC's custom-shortcut file, or nothing when there is none yet.
fn read_custom(path: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        read => read,
    }
}

/// Why a key could not be set or removed, by what would help.
#[derive(Debug, PartialEq, Eq)]
pub enum SetError {
    /// This key: another one may well work.
    Key(String),
    /// COSMIC's file, or the way to it: every key would fail the same way.
    File(String),
}

impl std::fmt::Display for SetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetError::Key(s) | SetError::File(s) => f.write_str(s),
        }
    }
}

/// Bind the recentre shortcut to `b`, running `command`, in COSMIC's file.
pub fn set(b: &Binding, command: &str) -> Result<bool, SetError> {
    if b.steals_typing() {
        return Err(SetError::Key(format!(
            "{} would stop that key typing anywhere — add Ctrl, Alt or Super, \
             or use a key like F9 or Pause",
            b.display()
        )));
    }
    if taken_by_cosmic(b) {
        return Err(SetError::Key(format!(
            "{} is already a COSMIC shortcut",
            b.display()
        )));
    }
    write_into(&custom_path(), Some(b), command)
}

/// Take the recentre shortcut out of COSMIC's file.
///
/// `Ok(false)` when there was nothing to do: no shortcut of ours in the file,
/// which leaves the file alone — or does not create it.
pub fn remove() -> Result<bool, SetError> {
    remove_at(&custom_path())
}

/// [`remove`] for the custom-shortcut file at `path`.
pub fn remove_at(path: &Path) -> Result<bool, SetError> {
    write_into(path, None, "")
}

fn write_into(path: &Path, binding: Option<&Binding>, command: &str) -> Result<bool, SetError> {
    let text = read_custom(path)
        .map_err(|e| SetError::File(format!("could not read {}: {e}", path.display())))?;
    // After `with_binding`, so that a file this program cannot take apart is
    // refused rather than passed over as having nothing of ours in it.
    let new = with_binding(&text, binding, command)?;
    if binding.is_none() && current_of(&text) == Current::Unbound {
        return Ok(false);
    }
    crate::write_atomic(path, new.as_bytes())
        .map(|()| true)
        .map_err(|e| SetError::File(format!("could not write {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape COSMIC Settings writes: pretty RON, one struct per key,
    /// spread over several lines.
    const THEIRS: &str = r#"{
    (
        modifiers: [
            Super,
        ],
        key: "t",
        description: Some("Terminal, with a comma, and (parens)"),
    ): Spawn("cosmic-term"),
    (
        modifiers: [],
        key: "Print",
    ): Spawn("cosmic-screenshot"),
}"#;

    fn ctrl_shift_r() -> Binding {
        Binding::new(&["Shift", "Ctrl"], "R")
    }

    #[test]
    fn a_binding_normalises_order_and_letter_case() {
        let b = ctrl_shift_r();
        assert_eq!(b.modifiers, vec!["Ctrl", "Shift"]);
        assert_eq!(b.key, "r");
        assert_eq!(b.display(), "Ctrl+Shift+R");
        assert_eq!(Binding::new(&[], "F9").display(), "F9");
    }

    #[test]
    fn an_empty_or_missing_file_gets_just_our_entry() {
        let out = with_binding("", Some(&ctrl_shift_r()), "/x/tobii recentre").unwrap();
        assert_eq!(current_of(&out), Current::Key(ctrl_shift_r()));
        assert!(out.contains(r#"Spawn("/x/tobii recentre")"#), "{out}");
        assert_eq!(split_entries(&out).unwrap().len(), 1);
    }

    #[test]
    fn other_entries_survive_verbatim_and_ours_is_replaced_not_duplicated() {
        let once = with_binding(THEIRS, Some(&ctrl_shift_r()), "tobii recentre").unwrap();
        let f9 = Binding::new(&[], "F9");
        let twice = with_binding(&once, Some(&f9), "tobii recentre").unwrap();
        let entries = split_entries(&twice).unwrap();
        assert_eq!(entries.len(), 3, "{twice}");
        for theirs in split_entries(THEIRS).unwrap() {
            assert!(twice.contains(theirs), "lost or altered: {theirs}");
        }
        assert_eq!(current_of(&twice), Current::Key(f9));
    }

    #[test]
    fn clearing_removes_only_ours() {
        let set = with_binding(THEIRS, Some(&ctrl_shift_r()), "tobii recentre").unwrap();
        let cleared = with_binding(&set, None, "tobii recentre").unwrap();
        assert_eq!(current_of(&cleared), Current::Unbound);
        assert_eq!(split_entries(&cleared).unwrap().len(), 2);
    }

    #[test]
    fn a_combination_already_in_the_file_is_refused() {
        let super_t = Binding::new(&["Super"], "t");
        let err = with_binding(THEIRS, Some(&super_t), "tobii recentre").unwrap_err();
        assert!(
            matches!(&err, SetError::Key(m) if m.contains("Super+T")),
            "{err}"
        );
    }

    #[test]
    fn a_file_that_is_not_a_map_is_left_alone() {
        assert!(matches!(
            with_binding("garbage ( [", Some(&ctrl_shift_r()), "x"),
            Err(SetError::File(_))
        ));
    }

    #[test]
    fn existing_bindings_are_read_back() {
        let es = split_entries(THEIRS).unwrap();
        assert_eq!(entry_binding(es[0]), Some(Binding::new(&["Super"], "t")));
        assert_eq!(entry_binding(es[1]), Some(Binding::new(&[], "Print")));
    }

    #[test]
    fn bare_typing_keys_are_flagged_and_function_keys_are_not() {
        assert!(Binding::new(&[], "r").steals_typing());
        assert!(Binding::new(&[], "space").steals_typing());
        assert!(!Binding::new(&[], "F9").steals_typing());
        assert!(!ctrl_shift_r().steals_typing());
    }

    /// GDK's names for the typing keys of a German layout, the keypad, and
    /// AltGr+Q (AltGr is not a recorded modifier, so it arrives bare). Each
    /// one types; a deny-list of single characters let all of them through.
    #[test]
    fn bare_keys_with_long_names_that_type_are_flagged_too() {
        for key in [
            "odiaeresis",
            "adiaeresis",
            "udiaeresis",
            "ssharp",
            "minus",
            "comma",
            "period",
            "numbersign",
            "at",
            "KP_5",
            "dead_acute",
            "Up",
            "a",
        ] {
            assert!(
                Binding::new(&[], key).steals_typing(),
                "{key} alone types, so a global binding must be refused"
            );
            assert!(
                Binding::new(&["Shift"], key).steals_typing(),
                "Shift+{key} types too (the capital), so Shift alone is no modifier"
            );
            assert!(
                !Binding::new(&["Ctrl"], key).steals_typing(),
                "with a modifier, {key} is fine"
            );
            assert!(
                !Binding::new(&["Shift", "Ctrl"], key).steals_typing(),
                "Shift beside a real modifier is fine"
            );
        }
        for key in [
            "F1",
            "F12",
            "F24",
            "Pause",
            "Scroll_Lock",
            "Print",
            "XF86AudioPlay",
        ] {
            assert!(
                !Binding::new(&["Shift"], key).steals_typing(),
                "Shift+{key} types nothing either"
            );
            assert!(
                !Binding::new(&[], key).steals_typing(),
                "{key} types nothing, so it is fine on its own"
            );
        }
    }

    /// A hand-edited file whose last entry ends in a `//` comment. The comma
    /// after it used to be written after the comment, so inside it, and the
    /// new entry followed with no separator — not RON, and one entry where
    /// there were two.
    #[test]
    fn a_trailing_comment_keeps_its_entry_separated() {
        let theirs =
            "{\n    (modifiers: [Super], key: \"t\"): Spawn(\"cosmic-term\") // my terminal\n}";
        let out = with_binding(theirs, Some(&ctrl_shift_r()), "tobii recentre").unwrap();
        let es = split_entries(&out).expect("still a map");
        assert_eq!(es.len(), 2, "two entries, separately: {out}");
        assert_eq!(entry_binding(es[0]), Some(Binding::new(&["Super"], "t")));
        assert_eq!(current_of(&out), Current::Key(ctrl_shift_r()), "{out}");
        assert!(
            out.contains("Spawn(\"cosmic-term\"), // my terminal\n"),
            "the comma goes before the comment, which is kept: {out}"
        );

        // Removing ours again leaves theirs, comment and all, still readable:
        // the comment now sits in front of ours, and must not go with it.
        let back = with_binding(&out, None, "tobii recentre").unwrap();
        let es = split_entries(&back).expect("still a map");
        let bound: Vec<Binding> = es.iter().filter_map(|e| entry_binding(e)).collect();
        assert_eq!(bound, vec![Binding::new(&["Super"], "t")], "{back}");
        assert!(back.contains("// my terminal"), "{back}");
        let again = with_binding(&back, Some(&ctrl_shift_r()), "tobii recentre").unwrap();
        assert!(
            again.contains("// my terminal"),
            "a re-bind keeps it too: {again}"
        );
        assert_eq!(current_of(&again), Current::Key(ctrl_shift_r()), "{again}");

        // A trailing note with nothing after it gets no comma of its own.
        let note =
            "{\n    (modifiers: [Super], key: \"t\"): Spawn(\"cosmic-term\"),\n    // a note\n}";
        let out = with_binding(note, None, "tobii recentre").unwrap();
        assert!(
            !out.contains("// a note,") && !out.contains(", // a note"),
            "{out}"
        );
        assert_eq!(split_entries(&out).map(|e| e.len()), Some(2), "{out}");
    }

    /// Block comments, which the comma placement did not know: the `//` inside
    /// `/* see https://x.org */` was taken for a line comment and the comma
    /// went in after `https:`, inside the block comment, leaving the new entry
    /// unseparated. A comma or bracket inside one is not structure either.
    #[test]
    fn block_comments_are_comments_to_both_readers() {
        let theirs = "{\n    (modifiers: [Super], key: \"t\"): Spawn(\"cosmic-term\") /* see https://x.org */\n}";
        let out = with_binding(theirs, Some(&ctrl_shift_r()), "tobii recentre").unwrap();
        assert!(
            out.contains("Spawn(\"cosmic-term\"), /* see https://x.org */"),
            "the comma goes before the comment, which is kept whole: {out}"
        );
        let es = split_entries(&out).expect("still a map");
        assert_eq!(es.len(), 2, "two entries, separately: {out}");
        assert_eq!(current_of(&out), Current::Key(ctrl_shift_r()), "{out}");

        // A comma and brackets inside a block comment split nothing.
        let noted = "{\n    /* mine, (all) of [these] */ (modifiers: [Super], key: \"t\"): Spawn(\"cosmic-term\"),\n}";
        let es = split_entries(noted).expect("a map with a comment in it");
        assert_eq!(es.len(), 1, "{es:?}");
        assert_eq!(entry_binding(es[0]), Some(Binding::new(&["Super"], "t")));

        // And one in front of ours is kept when ours goes, like a `//` one.
        let out = with_binding(noted, Some(&ctrl_shift_r()), "tobii recentre").unwrap();
        let ours_noted = out.replacen("(modifiers: [Ctrl", "/* recentre */ (modifiers: [Ctrl", 1);
        let back = with_binding(&ours_noted, None, "tobii recentre").unwrap();
        assert!(back.contains("/* recentre */"), "{back}");
        assert_eq!(current_of(&back), Current::Unbound, "{back}");

        // Unclosed: not a format this can edit, so it is refused, not guessed.
        assert_eq!(split_entries("{ /* never closed }"), None);
    }

    /// `tobii uninstall` judges the shortcut by the program it runs, so the
    /// command has to come back exactly as it was written — a path with a
    /// space, a quote or a backslash in it included.
    #[test]
    fn the_command_reads_back_as_written() {
        let cmd = r#"'/home/a/My "Games"\x/tobii' recentre"#;
        let text = with_binding(THEIRS, Some(&ctrl_shift_r()), cmd).unwrap();
        assert_eq!(our_command(&text).as_deref(), Some(cmd));
        assert_eq!(our_command(THEIRS), None, "no shortcut of ours");
        // Ours, running something that is not a plain `Spawn` of a string:
        // there, and nothing to judge it by.
        let odd = format!(
            "{{\n    (modifiers: [Ctrl], key: \"F9\", description: Some(\"{DESCRIPTION}\")): \
             System(Terminal),\n}}"
        );
        assert!(holds_ours(&odd));
        assert_eq!(our_command(&odd), None);
    }

    #[test]
    fn a_write_round_trips_through_a_real_file() {
        let dir = std::env::temp_dir().join(format!("tobii-shortcut-{}", std::process::id()));
        let path = dir.join("v1").join("custom");
        // A failed earlier run with the same pid leaves its file behind.
        let _ = std::fs::remove_dir_all(&dir);
        // Removing what was never there writes nothing, not even the file.
        assert_eq!(write_into(&path, None, "tobii recentre"), Ok(false));
        assert!(!path.exists());
        assert_eq!(
            write_into(&path, Some(&ctrl_shift_r()), "tobii recentre"),
            Ok(true)
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(current_of(&text), Current::Key(ctrl_shift_r()));
        assert_eq!(write_into(&path, None, "tobii recentre"), Ok(true));
        assert_eq!(
            current_of(&std::fs::read_to_string(&path).unwrap()),
            Current::Unbound
        );
        assert_eq!(write_into(&path, None, "tobii recentre"), Ok(false));
        // A file this program cannot take apart is refused, not passed over as
        // having nothing of ours in it: it may well have.
        std::fs::write(&path, "{ not ron at all").unwrap();
        assert!(write_into(&path, None, "tobii recentre").is_err());
        // Ours, with a key this program cannot read back: there is no binding
        // to show, and there is still an entry to remove.
        let unreadable = format!(
            "{{\n    (key: \"r\", description: Some(\"{DESCRIPTION}\")): Spawn(\"x\"),\n}}"
        );
        std::fs::write(&path, &unreadable).unwrap();
        assert_eq!(current_of(&unreadable), Current::Unknown);
        assert_eq!(write_into(&path, None, "tobii recentre"), Ok(true));
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains(DESCRIPTION));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A commented-out copy of ours swallows the comma after it, so it is
    /// split off as one entry with the user's next shortcut. Neither removing
    /// ours nor setting a key may take the user's shortcut with it, and the
    /// key in the comment is not a binding.
    #[test]
    fn a_commented_out_copy_of_ours_is_not_ours() {
        let text = format!(
            "{{\n    // (modifiers: [Ctrl, Shift], key: \"r\", description: Some(\"{DESCRIPTION}\")): Spawn(\"tobii recentre\"),\n    (modifiers: [Super], key: \"t\"): Spawn(\"cosmic-term\"),\n}}"
        );
        assert_eq!(current_of(&text), Current::Unbound);
        let removed = with_binding(&text, None, "tobii recentre").unwrap();
        assert!(removed.contains("cosmic-term"), "{removed}");
        let set = with_binding(&text, Some(&ctrl_shift_r()), "tobii recentre").unwrap();
        assert!(set.contains("cosmic-term"), "{set}");
        assert_eq!(current_of(&set), Current::Key(ctrl_shift_r()));
        // And a comment that only mentions it is left where it is.
        let note = format!("{{\n    // \"{DESCRIPTION}\" was on F9\n}}");
        assert!(with_binding(&note, None, "tobii recentre")
            .unwrap()
            .contains("was on F9"));
        assert_eq!(current_of(&note), Current::Unbound);
        assert_eq!(current_of("{ not ron at all"), Current::Unknown);
    }
}
