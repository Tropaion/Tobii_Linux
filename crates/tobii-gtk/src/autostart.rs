//! Starting the hub when the user logs in.
//!
//! Implemented with the XDG autostart spec — a `.desktop` file in
//! `$XDG_CONFIG_HOME/autostart/` — because that is the one mechanism every
//! desktop here honours: GNOME, KDE Plasma, Xfce, Cinnamon, MATE, LXQt and
//! sway/Hyprland via `dex` all read that directory. A systemd user unit would
//! be tidier on paper and would not work on the desktops that do not run one.
//!
//! Where the entry lives and what it says are defined in
//! [`tobii_config::autostart`], not here: `tobii uninstall` has to find the same
//! file and read the same `Exec` back, and two spellings of either would let
//! the two programs disagree about which entry is whose.
//!
//! # Why the autostarted process has no window
//!
//! The entry runs `tobii-gtk --background`, which starts the device thread and
//! nothing else. Throwing a settings window at somebody every time they log in
//! would be a bad trade for the little this buys. Launching `tobii-gtk` again —
//! from the menu, the dock, anywhere — hands off to the running instance and
//! raises the hub, because `GApplication` is single-instance.
//!
//! # What it does NOT do, despite what this used to say
//!
//! This module claimed that running at login is "what makes the tracker work
//! before anything asks it to", because the ET5 wipes its display area and
//! calibration on every reboot and something must re-apply them. That was true
//! before the standby behaviour landed and is false now, and the two cannot
//! both hold: `--background` opens no window, every `Demand` hold in this crate
//! belongs to a window, and the device thread refuses to open a session while
//! the demand is empty. So the background process connects to nothing and
//! applies nothing.
//!
//! Nor does a game need it to: `tobii headpose` opens the device itself and
//! re-applies the display area immediately after connecting. What is left is
//! worth having but smaller — the program is resident, so the hub opens
//! instantly and a second launch raises it.

use std::io;

use tobii_config::autostart::{dir, entry_path, entry_text, scratch_name};

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
    let exec = entry_exec(&std::env::current_exe()?)?.display().to_string();
    std::fs::create_dir_all(dir())?;
    // Written whole and renamed: a login that reads a half-written entry would
    // silently not start, and the failure would look like the setting never
    // took.
    let tmp = dir().join(scratch_name(std::process::id()));
    std::fs::write(&tmp, entry_text(&exec))?;
    std::fs::rename(&tmp, &path)
}

/// The path the entry should run, from `exe` as [`std::env::current_exe`]
/// reported it.
///
/// That is `/proc/self/exe`, and the kernel appends ` (deleted)` to it once the
/// file is unlinked — which `install.sh`, the updater and a package upgrade all
/// do, by renaming a new copy over the running one. Written as it reads, the
/// entry named a file that cannot exist: the switch said on, and the next login
/// started nothing. So a running copy that was REPLACED writes the path it was
/// replaced at, where the new copy now is and where the next login should find
/// it; one that was REMOVED is refused, since any entry written then would be
/// the same dead one.
///
/// The path is taken at its word when it exists, so a directory or file that
/// really is called `(deleted)` is not second-guessed.
fn entry_exec(exe: &std::path::Path) -> io::Result<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    if exe.is_file() {
        return Ok(exe.to_path_buf());
    }
    let bytes = exe.as_os_str().as_bytes();
    let Some(stem) = bytes.strip_suffix(b" (deleted)") else {
        return Ok(exe.to_path_buf());
    };
    let replaced = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(stem));
    if replaced.is_file() {
        return Ok(replaced);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "this program was removed from {} while it ran; start the copy that is \
             installed now and turn this on from there",
            replaced.display()
        ),
    ))
}

/// Whether this process was started for the background session.
pub fn background_mode() -> bool {
    std::env::args().any(|a| a == "--background")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tobii_config::autostart::ENTRY_NAME;

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

    /// The reported failure: start-at-login switched on from a hub that had
    /// been updated in place wrote `Exec=".../tobii-gtk (deleted)"`, an entry
    /// that reads as on and never starts anything.
    #[test]
    fn a_copy_replaced_while_it_ran_writes_the_path_it_was_replaced_at() {
        let dir = scratch("replaced");
        let bin = dir.join("tobii-gtk");
        std::fs::write(&bin, b"the new copy").unwrap();
        let reported = dir.join("tobii-gtk (deleted)");

        let exec = entry_exec(&reported).expect("the new copy is there");
        assert_eq!(exec, bin);
        // And the entry built from it runs that file, read back the way
        // `tobii uninstall` and `install.sh` read it.
        let text = entry_text(&exec.display().to_string());
        assert_eq!(
            tobii_config::autostart::exec_arguments(&text)
                .and_then(|args| args.into_iter().next())
                .as_deref(),
            Some(bin.to_str().unwrap())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_copy_removed_while_it_ran_writes_no_entry() {
        let dir = scratch("removed");
        let err = entry_exec(&dir.join("tobii-gtk (deleted)")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(
            err.to_string()
                .contains(&dir.join("tobii-gtk").display().to_string()),
            "the message names where it was: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only the kernel's suffix is special: a path that exists is used as it
    /// is, whatever it is called.
    #[test]
    fn a_path_that_exists_is_used_as_it_is() {
        let dir = scratch("as-is");
        let odd = dir.join("(deleted)");
        std::fs::create_dir_all(&odd).unwrap();
        let bin = odd.join("tobii-gtk");
        std::fs::write(&bin, b"x").unwrap();
        assert_eq!(entry_exec(&bin).unwrap(), bin);

        let literal = dir.join("tobii-gtk (deleted)");
        std::fs::write(&literal, b"x").unwrap();
        std::fs::write(dir.join("tobii-gtk"), b"another copy").unwrap();
        assert_eq!(entry_exec(&literal).unwrap(), literal);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
