//! A global keyboard shortcut for "recentre the head-tracking view".
//!
//! On COSMIC the hub writes one entry into COSMIC's custom shortcuts — the
//! chosen key combination running [`recentre_command`], which asks this hub to
//! recentre over its socket exactly as the button does. COSMIC fires it
//! whatever has focus. Everywhere else the hub cannot set the key, so its Copy
//! button puts the same command on the clipboard, for the user to bind in the
//! desktop's own keyboard settings. Why, and how a file this program does not
//! own is edited: [`tobii_config::cosmic_shortcut`].

use std::path::Path;

pub use tobii_config::cosmic_shortcut::{
    current, custom_path, is_cosmic, remove, Binding, Current, SetError,
};

/// Bind the recentre shortcut to `b` in COSMIC's file, running
/// [`recentre_command`].
pub fn set(b: &Binding) -> Result<bool, SetError> {
    tobii_config::cosmic_shortcut::set(b, &recentre_command())
}

/// The command the shortcut runs: the `tobii` beside this hub's executable when
/// there is one — so a hub run from a build tree binds its own build — and the
/// one on `PATH` otherwise.
///
/// Shell-quoted ([`crate::sh_quote`]), because it is pasted into a desktop's
/// shortcut settings by hand (the hub's Copy button) as well as written into
/// COSMIC's file, and a space in the path split it in two.
pub fn recentre_command() -> String {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("tobii")))
        .filter(|p| p.is_file());
    command_for(beside.as_deref())
}

/// [`recentre_command`] for the `tobii` at `tobii`, or for the one on `PATH`.
fn command_for(tobii: Option<&Path>) -> String {
    match tobii {
        Some(p) => format!("{} recentre", crate::sh_quote(&p.to_string_lossy())),
        None => "tobii recentre".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The command is pasted into a desktop's shortcut settings and run by
    /// COSMIC through `sh -c`, so a path with a space in it must stay one word,
    /// and an ordinary one is left as it is.
    #[test]
    fn the_recentre_command_is_one_shell_word_per_path() {
        assert_eq!(
            command_for(Some(Path::new("/home/a/.local/bin/tobii"))),
            "/home/a/.local/bin/tobii recentre"
        );
        assert_eq!(
            command_for(Some(Path::new("/home/a/My Games/tobii"))),
            "'/home/a/My Games/tobii' recentre"
        );
        assert_eq!(command_for(None), "tobii recentre");
    }
}
