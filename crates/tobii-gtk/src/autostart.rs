//! Starting the hub when the user logs in.
//!
//! Implemented with the XDG autostart spec — a `.desktop` file in
//! `$XDG_CONFIG_HOME/autostart/` — because that is the one mechanism every
//! desktop here honours: GNOME, KDE Plasma, Xfce, Cinnamon, MATE, LXQt and
//! sway/Hyprland via `dex` all read that directory. A systemd user unit would
//! be tidier on paper and would not work on the desktops that do not run one.
//!
//! # Why the autostarted process has no window
//!
//! The entry runs `tobii-gtk --background`, which starts the device thread and
//! nothing else. That is not a cosmetic choice: the ET5 **wipes its display
//! area and its calibration every time it reboots**, which it does whenever the
//! session closes, so something has to re-apply them on connect or the tracker
//! reports no eyes at all. Running at login is what makes the tracker work
//! before anything asks it to.
//!
//! Throwing a settings window at somebody every time they log in would be a bad
//! trade for that. Launching `tobii-gtk` again — from the menu, the dock,
//! anywhere — hands off to the running instance and raises the hub, because
//! `GApplication` is single-instance.

use std::io;
use std::path::PathBuf;

/// The autostart entry's file name.
///
/// Named for the application id, which is the convention, and which also means
/// a desktop that matches autostart entries against installed applications
/// finds the right one.
pub const ENTRY_NAME: &str = "com.tobiilinux.Configuration.desktop";

/// `$XDG_CONFIG_HOME/autostart`, falling back to `~/.config/autostart`.
pub fn autostart_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(".config")
        });
    base.join("autostart")
}

/// Where the autostart entry lives.
pub fn entry_path() -> PathBuf {
    autostart_dir().join(ENTRY_NAME)
}

/// Whether the hub is set to start at login.
pub fn is_enabled() -> bool {
    is_enabled_at(&entry_path())
}

/// [`is_enabled`] against a given path.
///
/// A file that exists but says `Hidden=true` or `X-GNOME-Autostart-enabled=false`
/// is how GNOME's own Tweaks and KDE's Autostart page record "off" — they edit
/// the entry rather than delete it. Reading only for the file's existence would
/// show the switch on for a user who had turned it off in their desktop's own
/// settings, and then fight with them over it.
pub fn is_enabled_at(path: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Hidden" if value.eq_ignore_ascii_case("true") => return false,
            "X-GNOME-Autostart-enabled" if value.eq_ignore_ascii_case("false") => return false,
            _ => {}
        }
    }
    true
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
         Comment=Keep the eye tracker configured from login\n\
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
/// fragment. Nothing warns: `desktop-file-validate` accepts the file,
/// `set_enabled` returns `Ok`, the switch stays on, and reading the file back
/// says it is enabled — it simply never starts, once, at the next login.
///
/// A `%` is doubled because the spec gives it meaning (field codes like `%U`),
/// and inside a quoted argument the reserved characters `"`, `` ` ``, `$` and
/// `\` are escaped with a backslash — which, since this is also a desktop-entry
/// *value*, has to be written as an escaped backslash.
fn quote_exec(exec: &str) -> String {
    // Always quoted, not only when the path contains a space: a path that
    // acquires one later should not change whether this is correct.
    //
    // One pass into one String. The `flat_map` this replaced allocated a Vec
    // per character of the path to yield one to three chars.
    let mut out = String::with_capacity(exec.len() + 2);
    out.push('"');
    for c in exec.chars() {
        match c {
            // Reserved inside a quoted argument. Written as an escaped
            // backslash because this is also a desktop-entry *value*.
            '"' | '`' | '$' | '\\' => out.push_str("\\\\"),
            // `%` starts a field code, so a literal one is doubled.
            '%' => out.push('%'),
            _ => {}
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Turn start-at-login on or off.
pub fn set_enabled(on: bool) -> io::Result<()> {
    let path = entry_path();
    if !on {
        // Remove the file rather than writing `Hidden=true`: this program wrote
        // it, so it can take it away, and leaving a disabled entry behind means
        // a user who removes the program still has one referring to it.
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        };
    }
    // The running binary's own path, so enabling autostart from a build tree
    // autostarts that build and enabling it from an installed copy autostarts
    // that one. `current_exe` resolves symlinks, which is what we want here:
    // the entry should survive the symlink being repointed.
    let exec = std::env::current_exe()?.display().to_string();
    std::fs::create_dir_all(autostart_dir())?;
    // Written whole and renamed: a login that reads a half-written entry would
    // silently not start, and the failure would look like the setting never
    // took.
    let tmp = path.with_extension(format!("desktop.new-{}", std::process::id()));
    std::fs::write(&tmp, entry_text(&exec))?;
    std::fs::rename(&tmp, &path)
}

/// Whether this process was started for the background session.
pub fn background_mode() -> bool {
    std::env::args().any(|a| a == "--background")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "tobii-autostart-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

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
    /// with a space in it runs the first fragment — and nothing warns:
    /// `desktop-file-validate` accepts it, the write succeeds, and reading it
    /// back says enabled. It just silently never starts.
    #[test]
    fn a_path_with_a_space_still_produces_a_launchable_entry() {
        let text = entry_text("/home/u/My Projects/tobii-gtk");
        let exec = text
            .lines()
            .find_map(|l| l.strip_prefix("Exec="))
            .expect("an Exec line");
        assert_eq!(exec, "\"/home/u/My Projects/tobii-gtk\" --background");
        // The path is one argument, not two.
        assert!(exec.starts_with('"'));
        let end = exec[1..].find('"').expect("a closing quote") + 1;
        assert_eq!(&exec[1..end], "/home/u/My Projects/tobii-gtk");
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

    #[test]
    fn enabling_writes_an_entry_and_disabling_removes_it() {
        let dir = scratch("toggle");
        let path = dir.join(ENTRY_NAME);

        assert!(!is_enabled_at(&path), "absent means off");
        std::fs::write(&path, entry_text("/bin/true")).unwrap();
        assert!(is_enabled_at(&path), "present and not disabled means on");
        std::fs::remove_file(&path).unwrap();
        assert!(!is_enabled_at(&path));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GNOME Tweaks and KDE's Autostart page turn an entry off by editing it,
    /// not by deleting it. Reading only for existence would show the switch on
    /// for somebody who had already turned it off, and then fight them for it.
    #[test]
    fn an_entry_disabled_by_the_desktop_itself_reads_as_off() {
        let dir = scratch("disabled");

        let gnome = dir.join("gnome.desktop");
        std::fs::write(
            &gnome,
            "[Desktop Entry]\nType=Application\nExec=x\nX-GNOME-Autostart-enabled=false\n",
        )
        .unwrap();
        assert!(!is_enabled_at(&gnome));

        let kde = dir.join("kde.desktop");
        std::fs::write(
            &kde,
            "[Desktop Entry]\nType=Application\nExec=x\nHidden=true\n",
        )
        .unwrap();
        assert!(!is_enabled_at(&kde));

        // Case and stray whitespace are how these get hand-edited.
        let messy = dir.join("messy.desktop");
        std::fs::write(&messy, "[Desktop Entry]\n Hidden = TRUE \n").unwrap();
        assert!(!is_enabled_at(&messy));

        // And the values that mean "still on".
        let on = dir.join("on.desktop");
        std::fs::write(
            &on,
            "[Desktop Entry]\nHidden=false\nX-GNOME-Autostart-enabled=true\n",
        )
        .unwrap();
        assert!(is_enabled_at(&on));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_autostart_directory_follows_xdg() {
        let d = autostart_dir();
        assert!(d.ends_with("autostart"), "{}", d.display());
        assert!(entry_path().ends_with(ENTRY_NAME));
    }
}
