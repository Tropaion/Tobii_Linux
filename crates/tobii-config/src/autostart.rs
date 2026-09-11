//! The XDG autostart entry: where it lives, what it says, and how to read the
//! program back out of it.
//!
//! Here rather than in the hub because two programs need the same answers. The
//! hub writes the entry; `tobii uninstall` has to find it and decide whether it
//! belongs to an install that is being removed. If the two each spelled the
//! path, or each had their own idea of how `Exec` is quoted, an uninstall could
//! miss the entry — or remove one that a remaining install still uses, which
//! silently turns off start-at-login.

use std::path::{Path, PathBuf};

use crate::paths;

/// The autostart entry's file name — the application id, like the menu entry.
pub const ENTRY_NAME: &str = paths::DESKTOP_ENTRY;

/// `<config home>/autostart`.
pub fn dir_in(config_home: &Path) -> PathBuf {
    config_home.join("autostart")
}

/// `$XDG_CONFIG_HOME/autostart`, falling back to `~/.config/autostart`.
pub fn dir() -> PathBuf {
    dir_in(&paths::config_home())
}

/// Where the autostart entry lives.
pub fn entry_path() -> PathBuf {
    dir().join(ENTRY_NAME)
}

/// The temporary the entry is written to before it is renamed into place.
pub fn scratch_name(pid: u32) -> String {
    format!("{ENTRY_NAME}.new-{pid}")
}

/// Whether `name` is a temporary left by an interrupted [`scratch_name`] write.
pub fn is_scratch_name(name: &str) -> bool {
    name.strip_prefix(ENTRY_NAME)
        .and_then(|r| r.strip_prefix(".new-"))
        .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// The text of the autostart entry.
///
/// `exec` is written as an absolute path rather than a bare `tobii-gtk`. The
/// session that runs autostart entries does not necessarily have the same PATH
/// as an interactive shell — `~/.local/bin` in particular is added by the
/// user's shell profile, which a display manager never sources — so a bare name
/// is exactly the kind of thing that works when tested and silently does
/// nothing at the next login.
pub fn entry_text(exec: &str) -> String {
    let exec = quote_exec(exec);
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.5\n\
         Name=Tobii Eye Tracker\n\
         Comment=Keep the Tobii hub ready in the background\n\
         Exec={exec} --background\n\
         Icon=com.tobiilinux.Configuration\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// One argument of an `Exec` value, quoted as the Desktop Entry spec requires.
///
/// A launcher splits `Exec` on unquoted whitespace, so a path containing a
/// space becomes two arguments and the program it tries to run is the first
/// fragment. Nothing warns: `desktop-file-validate` accepts the file, the write
/// succeeds, and reading it back says it is enabled — it simply never starts,
/// once, at the next login.
///
/// A `%` is doubled because the spec gives it meaning (field codes like `%U`),
/// and inside a quoted argument the reserved characters `"`, `` ` ``, `$` and
/// `\` are escaped with a backslash — which, since this is also a desktop-entry
/// *value*, has to be written as an escaped backslash.
fn quote_exec(exec: &str) -> String {
    // Always quoted, not only when the path contains a space: a path that
    // acquires one later should not change whether this is correct.
    let mut out = String::with_capacity(exec.len() + 2);
    out.push('"');
    for c in exec.chars() {
        match c {
            // A literal backslash is escaped TWICE over: once for the quoted
            // argument (`\` -> `\\`) and then once more because this is also a
            // desktop-entry *value*, where each of those backslashes is itself
            // written `\\`. Four characters, not three. Writing three produced
            // a value that unescapes to `\"` — an escaped quote — so a path
            // containing a backslash silently ended the argument early.
            '\\' => out.push_str("\\\\\\"),
            // Reserved inside a quoted argument. Written as an escaped
            // backslash because this is also a desktop-entry *value*.
            '"' | '`' | '$' => out.push_str("\\\\"),
            // `%` starts a field code, so a literal one is doubled.
            '%' => out.push('%'),
            _ => {}
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The program a desktop entry runs: the first argument of its `Exec` line,
/// unescaped.
///
/// Reads any desktop entry, not only the autostart one — the menu entry that
/// `install-payload.sh` writes has an unquoted absolute path, the autostart
/// entry above a quoted one, and both undo the same two layers: the
/// desktop-entry value escapes (`\\`, `\s`, …) and then the `Exec` quoting.
/// Only the `[Desktop Entry]` group is read, as the spec says.
pub fn exec_program(entry: &str) -> Option<String> {
    let mut in_main = false;
    for line in entry.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "Exec" {
            continue;
        }
        return first_argument(&unescape_value(value.trim()));
    }
    None
}

/// Undo the desktop-entry *string* escapes: `\s \n \t \r \\`.
fn unescape_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The first argument of an unescaped `Exec` value, with its quoting removed.
fn first_argument(v: &str) -> Option<String> {
    let v = v.trim_start();
    let arg = if let Some(rest) = v.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        loop {
            match chars.next()? {
                // Inside quotes a backslash escapes the next character.
                '\\' => out.push(chars.next()?),
                '"' => break out,
                c => out.push(c),
            }
        }
    } else {
        v.split_whitespace().next()?.to_string()
    };
    let arg = arg.replace("%%", "%");
    (!arg.is_empty()).then_some(arg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_is_a_valid_desktop_file_that_runs_the_background_session() {
        let text = entry_text("/home/u/.local/bin/tobii-gtk");
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("Type=Application\n"));
        // The point of the whole feature: no window at login.
        assert!(text.contains("Exec=\"/home/u/.local/bin/tobii-gtk\" --background\n"));
        // It is not an application to show in the menu — the installed
        // com.tobiilinux.Configuration.desktop is.
        assert!(text.contains("NoDisplay=true\n"));
        assert!(text.contains("X-GNOME-Autostart-enabled=true\n"));
        assert!(text.ends_with('\n'), "desktop files end with a newline");
    }

    /// An absolute path, because the login session's PATH is not the shell's.
    #[test]
    fn the_entry_names_the_binary_by_absolute_path() {
        let text = entry_text("/opt/tobii/tobii-gtk");
        let exec = text
            .lines()
            .find_map(|l| l.strip_prefix("Exec="))
            .expect("an Exec line");
        assert!(
            exec.starts_with("\"/"),
            "Exec must be an absolute path, quoted: {exec}"
        );
    }

    /// A launcher splits `Exec` on unquoted whitespace, so an unquoted path
    /// with a space in it runs the first fragment — and nothing warns.
    #[test]
    fn a_path_with_a_space_still_produces_a_launchable_entry() {
        let text = entry_text("/home/u/My Projects/tobii-gtk");
        let exec = text
            .lines()
            .find_map(|l| l.strip_prefix("Exec="))
            .expect("an Exec line");
        assert_eq!(exec, "\"/home/u/My Projects/tobii-gtk\" --background");
    }

    /// `%` is a field code in an Exec value, so a literal one must be doubled,
    /// and the spec's reserved characters escaped inside the quotes.
    #[test]
    fn reserved_characters_in_the_path_are_escaped() {
        let text = entry_text("/home/u/100%/tobii-gtk");
        assert!(
            text.contains("Exec=\"/home/u/100%%/tobii-gtk\" --background"),
            "{text}"
        );
        let dollar = entry_text("/home/u/$HOME/tobii-gtk");
        assert!(dollar.contains("\\\\$HOME"), "{dollar}");
    }

    /// The reader undoes exactly what the writer does, for every character the
    /// writer treats specially. An uninstall that misread the path would either
    /// miss the entry or delete one a remaining install still uses.
    #[test]
    fn the_program_read_back_is_the_program_written() {
        for exec in [
            "/home/u/.local/bin/tobii-gtk",
            "/home/u/My Projects/tobii-gtk",
            "/home/u/100%/tobii-gtk",
            "/home/u/$HOME/`x`/\"q\"/tobii-gtk",
            "/home/u/back\\slash/tobii-gtk",
        ] {
            assert_eq!(
                exec_program(&entry_text(exec)).as_deref(),
                Some(exec),
                "{exec}"
            );
        }
    }

    /// The menu entry `install-payload.sh` writes: an unquoted absolute path,
    /// and comments above it that mention `Exec` in prose.
    #[test]
    fn the_menu_entrys_unquoted_exec_is_read() {
        let entry = "[Desktop Entry]\nType=Application\n# rewrites this line\n\
                     Exec=/home/u/.local/bin/tobii-gtk\nIcon=x\n";
        assert_eq!(
            exec_program(entry).as_deref(),
            Some("/home/u/.local/bin/tobii-gtk")
        );
        // The shipped, unrewritten entry says a bare name.
        assert_eq!(
            exec_program("[Desktop Entry]\nExec=tobii-gtk\n").as_deref(),
            Some("tobii-gtk")
        );
        // An action group's Exec is not the application's.
        assert_eq!(
            exec_program("[Desktop Action x]\nExec=/other\n[Desktop Entry]\nName=y\n"),
            None
        );
    }

    #[test]
    fn the_write_temporary_is_recognised_and_nothing_else_is() {
        assert!(is_scratch_name(&scratch_name(1234)));
        for other in [
            ENTRY_NAME,
            "com.tobiilinux.Configuration.desktop.new-",
            "com.tobiilinux.Configuration.desktop.new-12a",
            "other.desktop.new-12",
        ] {
            assert!(!is_scratch_name(other), "{other}");
        }
    }

    #[test]
    fn the_autostart_directory_follows_xdg() {
        assert_eq!(
            dir_in(Path::new("/x/cfg")),
            PathBuf::from("/x/cfg/autostart")
        );
        assert!(entry_path().ends_with(format!("autostart/{ENTRY_NAME}")));
    }
}
