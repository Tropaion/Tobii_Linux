//! A global keyboard shortcut for "recentre the head-tracking view".
//!
//! # Why the hub does not grab the key itself
//!
//! Under Wayland a program cannot see keys pressed while another window — the
//! game — has focus. The standard way to ask for one is the desktop portal's
//! `GlobalShortcuts` interface, which KDE and GNOME implement and COSMIC does
//! not (checked on COSMIC with cosmic-comp 1.0: no such interface on
//! `org.freedesktop.portal.Desktop`). COSMIC instead has user-defined
//! shortcuts that run a command, kept in a RON file the compositor watches.
//!
//! So on COSMIC the hub writes one entry there — the chosen key combination
//! running `tobii recentre`, which asks this hub to recentre over its socket
//! exactly as the button does. COSMIC fires it whatever has focus, and picks a
//! change up without a restart. Everywhere else the hub says which command to
//! bind in the desktop's own keyboard settings.
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

    /// Whether a key alone, with no modifier, would take a key away from
    /// typing. Function keys, Pause, Scroll Lock and the like are fine bare;
    /// a letter, digit or punctuation key is not.
    pub fn steals_typing(&self) -> bool {
        self.modifiers.is_empty()
            && (self.key.chars().count() == 1
                || matches!(
                    self.key.as_str(),
                    "space" | "Return" | "Tab" | "BackSpace" | "Escape" | "Delete"
                ))
    }
}

/// Whether this session is COSMIC.
pub fn is_cosmic() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .map(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("COSMIC")))
        .unwrap_or(false)
}

fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

const SHORTCUTS_DIR: &str = "cosmic/com.system76.CosmicSettings.Shortcuts/v1";

/// The user's custom-shortcut file.
pub fn custom_path() -> Option<PathBuf> {
    config_home().map(|c| c.join(SHORTCUTS_DIR).join("custom"))
}

/// The files holding the bindings COSMIC already has: its shipped defaults and
/// the user's overrides of them. Consulted for conflicts only.
fn taken_paths() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("/usr/share")
        .join(SHORTCUTS_DIR)
        .join("defaults")];
    if let Some(c) = config_home() {
        v.push(c.join(SHORTCUTS_DIR).join("defaults"));
    }
    v
}

/// The command the shortcut runs: the `tobii` beside this hub's executable when
/// there is one — so a hub run from a build tree binds its own build — and the
/// one on `PATH` otherwise.
pub fn recentre_command() -> String {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("tobii")))
        .filter(|p| p.is_file());
    match beside {
        Some(p) => format!("{} recentre", p.display()),
        None => "tobii recentre".to_string(),
    }
}

/// The text between the outer `{` and `}` split into top-level entries, each
/// trimmed and without its separating comma. `None` if the text is not a map.
fn split_entries(text: &str) -> Option<Vec<&str>> {
    let body = text.trim();
    let body = body.strip_prefix('{')?.strip_suffix('}')?;
    let mut entries = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    let (mut in_str, mut escaped, mut in_comment) = (false, false, false);
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_comment {
            if c == b'\n' {
                in_comment = false;
            }
        } else if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'/' if bytes.get(i + 1) == Some(&b'/') => in_comment = true,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b',' if depth == 0 => {
                    entries.push(body[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
        if depth < 0 {
            return None;
        }
        i += 1;
    }
    if depth != 0 || in_str {
        return None;
    }
    entries.push(body[start..].trim());
    Some(entries.into_iter().filter(|e| !e.is_empty()).collect())
}

/// The binding written in one entry's key, if it can be read.
fn entry_binding(entry: &str) -> Option<Binding> {
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
    entry.contains(&format!("\"{DESCRIPTION}\""))
}

/// The recentre binding currently in `text`, if there is one.
pub fn current_in(text: &str) -> Option<Binding> {
    split_entries(text)?
        .into_iter()
        .find(|e| is_ours(e))
        .and_then(entry_binding)
}

/// `text` with the recentre entry replaced by `binding` running `command`, or
/// removed when `binding` is `None`. Every other entry is kept verbatim.
pub fn with_binding(
    text: &str,
    binding: Option<&Binding>,
    command: &str,
) -> Result<String, String> {
    let entries = if text.trim().is_empty() {
        Vec::new()
    } else {
        split_entries(text).ok_or_else(|| {
            "COSMIC's custom-shortcut file is not in a format this program can edit safely, \
             so it was left alone — set the shortcut in COSMIC Settings instead"
                .to_string()
        })?
    };
    let mut out: Vec<String> = Vec::new();
    for e in entries {
        if is_ours(e) {
            continue;
        }
        if let (Some(b), Some(theirs)) = (binding, entry_binding(e)) {
            if *b == theirs {
                return Err(format!(
                    "{} is already one of your custom shortcuts in COSMIC",
                    b.display()
                ));
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
        s.push_str(&e);
        s.push_str(",\n");
    }
    s.push('}');
    Ok(s)
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

/// The recentre binding on this machine, if one is set.
pub fn current() -> Option<Binding> {
    let text = std::fs::read_to_string(custom_path()?).ok()?;
    current_in(&text)
}

/// Write `binding` (or remove the shortcut, for `None`) into COSMIC's file.
pub fn set(binding: Option<&Binding>) -> Result<(), String> {
    let path = custom_path().ok_or("no home directory to find COSMIC's settings in")?;
    if let Some(b) = binding {
        if b.steals_typing() {
            return Err(format!(
                "{} on its own would stop that key typing anywhere — add Ctrl, Alt, \
                 Shift or Super, or use a key like F9 or Pause",
                b.display()
            ));
        }
        if taken_by_cosmic(b) {
            return Err(format!("{} is already a COSMIC shortcut", b.display()));
        }
    }
    write_into(&path, binding, &recentre_command())
}

fn write_into(path: &Path, binding: Option<&Binding>, command: &str) -> Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let new = with_binding(&text, binding, command)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    tobii_config::write_atomic(path, new.as_bytes())
        .map_err(|e| format!("could not write {}: {e}", path.display()))
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
        assert_eq!(current_in(&out), Some(ctrl_shift_r()));
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
        assert_eq!(current_in(&twice), Some(f9));
    }

    #[test]
    fn clearing_removes_only_ours() {
        let set = with_binding(THEIRS, Some(&ctrl_shift_r()), "tobii recentre").unwrap();
        let cleared = with_binding(&set, None, "tobii recentre").unwrap();
        assert_eq!(current_in(&cleared), None);
        assert_eq!(split_entries(&cleared).unwrap().len(), 2);
    }

    #[test]
    fn a_combination_already_in_the_file_is_refused() {
        let super_t = Binding::new(&["Super"], "t");
        let err = with_binding(THEIRS, Some(&super_t), "tobii recentre").unwrap_err();
        assert!(err.contains("Super+T"), "{err}");
    }

    #[test]
    fn a_file_that_is_not_a_map_is_left_alone() {
        assert!(with_binding("garbage ( [", Some(&ctrl_shift_r()), "x").is_err());
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

    #[test]
    fn a_write_round_trips_through_a_real_file() {
        let dir = std::env::temp_dir().join(format!("tobii-shortcut-{}", std::process::id()));
        let path = dir.join("v1").join("custom");
        write_into(&path, Some(&ctrl_shift_r()), "tobii recentre").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(current_in(&text), Some(ctrl_shift_r()));
        write_into(&path, None, "tobii recentre").unwrap();
        assert_eq!(current_in(&std::fs::read_to_string(&path).unwrap()), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
