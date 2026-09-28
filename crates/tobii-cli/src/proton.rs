//! Put our provider on the right side of the wineserver lock.
//!
//! # The ordering problem, in one paragraph
//!
//! Steam launches a Proton title with the verb `waitforexitandrun`, and Proton
//! runs `wineserver -w` **before** it spawns the game — a wait for every wine
//! process on that prefix to exit (see [`crate::wineserver`]). Anything of ours
//! started beforehand is such a process, so it does not delay the launch, it
//! prevents it. `tobii bridge run` answers that by standing down, which stops
//! the freeze and delivers nothing: the provider is then not running, and for a
//! TrackIR title pointed at a third-party client DLL nothing else fills
//! `FT_SharedMem`.
//!
//! The way out is not to win that race but never to be in it. Steam hands the
//! wrapper the whole command it meant to run, ending in the game's `.exe`. If
//! the thing Proton is told to run is instead a batch file that starts our
//! provider and then the game, Proton is invoked exactly as Steam meant it to
//! be — **one** `waitforexitandrun`, therefore **one** `wineserver -w` — and
//! both processes are born after the lock is taken, inside the session the game
//! itself is in. `start /wait` keeps `cmd.exe` alive for exactly as long as the
//! game — measured, below — which is what the studied launcher relies on to
//! leave Steam's own bookkeeping and overlay seeing a process that ends when
//! the game does.
//!
//! The shape is [markx86/opentrack-launcher]'s; the mechanism was read, and no
//! code, binary or file of it is used here.
//!
//! [markx86/opentrack-launcher]: https://github.com/markx86/opentrack-launcher
//!
//! # Why a title that already works is not disturbed
//!
//! The client DLLs feed themselves, so a FreeTrack title needs nothing running
//! and now gets a provider anyway. That is safe by the feeder's own design
//! rather than by luck: `tobii_bridge_core::feeder` cedes the port only when
//! the mapping is openable, which is what proves the holder is in *this*
//! wineserver session — and being in that session is precisely what this module
//! arranges. One of the two feeds, and the other reads what it publishes.
//!
//! # What this module will not do
//!
//! Every decision here is allowed to answer "no", and answering no means the
//! command runs exactly as Steam wrote it. That is the whole safety argument: a
//! wrapper that declines costs the user the provider, and a wrapper that
//! launches the wrong thing costs them the game.
//!
//! So it declines unless it can name Proton's own verb in the command, the
//! prefix comes from the environment of *this* launch rather than being
//! inferred, our provider is already sitting in that prefix, and every
//! character of the game's path and arguments is one a batch file can carry.
//!
//! # What a batch file can carry, measured
//!
//! Measured 2026-09-28 against wine 11.18 on a throwaway prefix, running a
//! generated batch whose "game" printed its own `argv` back:
//!
//! * A path or argument holding `%`, `^`, `&`, `(`, `)`, `!`, a space, a
//!   trailing `\`, or nothing at all arrives intact, given [`batch_arg`]'s
//!   quoting — `%` doubled, trailing backslashes doubled, the whole thing in
//!   double quotes.
//! * A **non-ASCII** path does not. `cmd.exe` reads a batch file in the
//!   prefix's OEM codepage: the same launch worked when the file was written as
//!   CP850 and failed as UTF-8, as UTF-8 with a BOM, as UTF-16LE with a BOM,
//!   and with `chcp 65001` on the first line — "file not found" each time.
//!   Which OEM codepage a given Proton prefix reads is not knowable from here,
//!   and writing the wrong one spells a different path, so this declines
//!   instead of guessing. (Passing the path through an environment variable and
//!   spelling `%VAR%` in the batch *was* measured to carry non-ASCII and CJK
//!   intact; it is not used, because it trades a limitation that declines for
//!   one that would stop the game launching if the variable ever failed to
//!   reach `cmd.exe`.)
//! * The game's exit code comes back out: `start /wait`, saved to a variable
//!   before the `taskkill` overwrites it, then `exit /b`. Measured 0, 3 and 7.
//!
//! # What none of this establishes
//!
//! That a game then *receives* tracking. This module gets a process started in
//! the right order; whether a given title reads `FT_SharedMem` afterwards is
//! not something anything here can check, and no message it prints may suggest
//! it did.
//!
//! Nor that a real Proton launch behaves the way the measurements above do.
//! Every one of them ran `wine` directly on a throwaway prefix. The container's
//! share — that Steam and Proton accept a `.bat` as the launch target, and that
//! the `cmd.exe` running it is inside the wineserver session Proton opened for
//! the game — is reasoned from the shape of the launch and from the studied
//! launcher working for its users. It was not measured here, and nothing on
//! this machine could measure it.

use std::path::{Path, PathBuf};

/// Proton's verb for "run this and wait for it", the one Steam launches a title
/// with and the only one this module treats as a game launch.
///
/// The other verbs are tooling — `runinprefix`, `getcompatpath` and friends run
/// something *in* the prefix without being the game — and rewriting one of
/// those would substitute a game launch for a lookup.
const VERB: &str = "waitforexitandrun";

/// The provider, inside the prefix.
///
/// The directory it sits in is [`crate::bridge::INSTALL_WIN_DIR`], taken from
/// the module that put it there rather than spelled again here.
const PROVIDER: &str = "tobii-bridge.exe";

/// What the wrapper decided to do with the command it was handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Nothing recognisable as a Proton game launch. Run the command as given,
    /// silently: this is every native game, every non-Steam program, and every
    /// Proton invocation that is not the title itself.
    AsGiven,
    /// A Proton game launch, and a reason this one is not being rewritten. Run
    /// the command as given and say this, once.
    Declined(String),
    /// Replace the command from `target` onwards with a batch file holding
    /// `batch`, written into `dir`.
    Rewrite {
        /// Index in the command of the game executable Proton was told to run.
        /// Everything from here on moves into the batch file.
        target: usize,
        /// Where the batch file goes: our own directory inside the prefix.
        dir: PathBuf,
        /// The batch file's whole content.
        batch: String,
    },
}

/// Where the game executable sits in a Proton launch, if this is one.
///
/// The anchor is Proton's own argument pair — a program whose file name is
/// `proton`, followed by [`VERB`] — and the game is what comes after it. Steam
/// expands `%command%` to a chain that ends that way; measured here on a live
/// launch, the whole of it is
///
/// ```text
/// …/reaper SteamLaunch AppId=2074920 -- …/_v2-entry-point --verb=waitforexitandrun
///   -- …/proton waitforexitandrun /…/TheGame.exe -steam
/// ```
///
/// which is why the verb alone will not do: the runtime's entry point carries
/// the same word in `--verb=waitforexitandrun`, a few arguments earlier, and
/// anchoring on it would treat the entry point's own `--` as the game.
///
/// The **first** match wins, not the last. Everything after the real anchor
/// belongs to the game, so an argument that happened to spell the anchor could
/// only ever appear after it — taking the last match would hand that argument
/// the launch.
pub(crate) fn proton_target(cmd: &[String]) -> Option<usize> {
    let at = cmd.iter().enumerate().find_map(|(i, a)| {
        (i > 0
            && a == VERB
            && Path::new(&cmd[i - 1])
                .file_name()
                .is_some_and(|n| n == "proton"))
        .then_some(i + 1)
    })?;
    // A verb with nothing after it is not a launch, and neither is one whose
    // target is not a Windows executable — Proton would not be running it.
    let target = cmd.get(at)?;
    is_exe(target).then_some(at)
}

/// Whether a path names a Windows executable.
///
/// Case-insensitively, because Steam prints back whatever the game's manifest
/// says and plenty of them say `.EXE`.
fn is_exe(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

/// One value, ready to appear inside a batch file, or `None` if it cannot.
///
/// Three characters are refused rather than escaped:
///
/// * `"` — `cmd.exe` toggles quoting on every one of them and has no escape a
///   program's own `CommandLineToArgvW` would also agree with, so an embedded
///   quote cannot mean the same thing on both sides of the line.
/// * any ASCII control character, a newline above all: a batch file is parsed a
///   line at a time, so a value containing one is a value that ends the command.
/// * anything non-ASCII — see the module header for what was measured.
///
/// What is escaped:
///
/// * `%` is doubled. Inside a batch file `%FOO%` expands and a lone `%` is
///   unpredictable; `%%` is the one spelling that means a literal one. It
///   applies inside double quotes too, which is why quoting alone is not
///   enough.
/// * A run of backslashes at the very end is doubled. Quoting is for
///   `cmd.exe`, but the program on the other side splits its own command line
///   by the C runtime's rules, where `\"` is a literal quote — so `"C:\dir\"`
///   would reach it as an unterminated argument beginning with `"`. Doubling
///   only the trailing run is enough because every other backslash is followed
///   by something that is not the closing quote.
pub(crate) fn batch_arg(value: &str) -> Option<String> {
    if !value.is_ascii() || value.chars().any(|c| c.is_ascii_control() || c == '"') {
        return None;
    }
    let mut out = value.replace('%', "%%");
    let trailing = out.len() - out.trim_end_matches('\\').len();
    out.push_str(&"\\".repeat(trailing));
    Some(format!("\"{out}\""))
}

/// The batch file that starts the provider, then the game, then reaps.
///
/// `target` is the game in its Windows spelling and `args` are its arguments,
/// exactly as Proton would have passed them.
///
/// The order is the whole point: the provider first so that it is up by the
/// time the game looks, the game under `start /wait` so this `cmd.exe` lives
/// exactly as long as it does, and the reap last.
///
/// **`start /b`** for the provider, so it does not open a console window over a
/// game that is about to go fullscreen, and so what it prints lands in the same
/// output Steam already collects.
///
/// **`--no-register`, always**, for the reason `tobii bridge run` passes it:
/// `install` settled both discovery keys under a read-before-write contract
/// that the provider's own blind write knows nothing about, and in the one
/// configuration this exists for — TrackIR pointed at a third-party client —
/// that write replaces the registration the user came for.
///
/// **No `--port`.** The provider and the client DLL both take it from
/// `TOBII_BRIDGE_PORT` in the environment, and they have to agree: naming it
/// here would make the two disagree in exactly the case where naming it helped,
/// an environment that did not arrive.
///
/// **The reap is by image name**, which the studied launcher is criticised for
/// — and here it is the right instrument rather than a blunt one. `taskkill`
/// reaches only this prefix's wineserver session, and the name is our own
/// program's, so the only thing it can take besides this launch's provider is
/// another copy of *our* provider in *this* game's prefix. That copy is
/// precisely the wine process that would make the next launch's `wineserver -w`
/// hang, so reaping it is the service, not the collateral.
pub(crate) fn batch(target: &str, args: &[String]) -> Option<String> {
    let mut game = batch_arg(target)?;
    for a in args {
        game.push(' ');
        game.push_str(&batch_arg(a)?);
    }
    let provider = format!(r"{}\{PROVIDER}", crate::bridge::INSTALL_WIN_DIR);
    let lines = [
        "@echo off".to_string(),
        "rem written by `tobii game` for one launch, and deleted after it".to_string(),
        format!("start /b \"\" \"{provider}\" --no-register"),
        format!("start /wait \"\" {game}"),
        // Saved before the taskkill, which would otherwise be the exit code
        // Steam is told the game finished with.
        "set TOBII_GAME_RC=%ERRORLEVEL%".to_string(),
        format!("taskkill /f /im {PROVIDER} >nul 2>&1"),
        "exit /b %TOBII_GAME_RC%".to_string(),
    ];
    // CRLF: what `cmd.exe` is written for, and what every batch file it has
    // ever been handed uses.
    Some(lines.join("\r\n") + "\r\n")
}

/// Decide what to do with the command Steam handed the wrapper.
///
/// `compat` is `$STEAM_COMPAT_DATA_PATH` as this process received it. That, and
/// not an app id resolved back through Steam's libraries, is what decides the
/// prefix: Steam sets it in the environment of the very launch being wrapped,
/// so it is the answer rather than a reconstruction of it. A game moved between
/// libraries, a shortcut with its own compat path, a second Steam install — all
/// of those make the inferred answer wrong and leave this one right.
///
/// Declining when it is absent rather than falling back to `tobii-steam` is
/// deliberate: without it there is no evidence this is the Proton launch it
/// looks like, and the fallback's failure mode is starting a provider in a
/// prefix the game is not in, which looks exactly like success.
pub(crate) fn plan(cmd: &[String], compat: Option<&Path>) -> Plan {
    let Some(target) = proton_target(cmd) else {
        return Plan::AsGiven;
    };
    let Some(compat) = compat else {
        return Plan::Declined(
            "this looks like a Proton launch, but STEAM_COMPAT_DATA_PATH is not set, so \
             which prefix it uses is not established — running it unchanged"
                .into(),
        );
    };
    let prefix = compat.join("pfx");
    let dir = prefix.join(crate::bridge::INSTALL_SUBDIR);
    if !dir.join(PROVIDER).is_file() {
        return Plan::Declined(format!(
            "the bridge provider is not in {} — run `tobii bridge install` for this \
             game to have it started alongside",
            prefix.display()
        ));
    }
    let Some(batch) = batch(
        &crate::bridge::wine_path_for(Path::new(&cmd[target])),
        &cmd[target + 1..],
    ) else {
        return Plan::Declined(
            "the game's path or one of its arguments holds a character a batch file \
             cannot carry (a quote, a control character, or anything non-ASCII), so the \
             bridge provider is not being started — running the game unchanged"
                .into(),
        );
    };
    Plan::Rewrite { target, dir, batch }
}

/// Point Proton at the batch file instead of at the game.
///
/// The game's own arguments come off the command line **with** it. They are
/// inside the batch now, and a copy left behind would be handed to `cmd.exe` as
/// arguments to the batch file — where `%1` and friends mean something, and
/// where nothing would pass them to the game a second time.
pub(crate) fn point_at(cmd: &mut Vec<String>, target: usize, batch: String) {
    cmd.truncate(target + 1);
    cmd[target] = batch;
}

/// A batch file that exists for one launch and is removed when it ends.
///
/// The studied launcher caches one per app id forever, with the executable's
/// path and arguments baked in, so a game moved to another library goes on
/// launching from the old path until somebody works out why. Nothing here is
/// cached: the file is written from *this* launch's command and deleted when it
/// returns, so there is no version of it that can be stale.
pub(crate) struct Once(PathBuf);

impl Once {
    /// Write the batch into `dir`, named for this process.
    ///
    /// The pid is in the name so that two launches sharing a prefix cannot
    /// write over each other's file while `cmd.exe` is still reading it.
    pub(crate) fn write(dir: &Path, batch: &str) -> std::io::Result<Self> {
        let path = dir.join(format!("launch-{}.bat", std::process::id()));
        std::fs::write(&path, batch)?;
        Ok(Self(path))
    }

    /// Where it was written, for the command line that has to name it.
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Once {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Carry out [`plan`]: the command to run, and the file that has to outlive it.
///
/// Every path that is not the rewrite hands the command straight back, so a
/// caller cannot get this wrong by forgetting a branch — there is nothing to
/// forget. The notes go to stderr from here because the reason for each one
/// lives here; the caller has no way to say anything truer about it.
///
/// Hold the returned [`Once`] until the command has exited. Dropping it is what
/// takes the batch file away, and dropping it early takes it out from under the
/// `cmd.exe` still reading it.
pub(crate) fn arrange(mut cmd: Vec<String>, compat: Option<&Path>) -> (Vec<String>, Option<Once>) {
    let (target, dir, batch) = match plan(&cmd, compat) {
        Plan::AsGiven => return (cmd, None),
        Plan::Declined(why) => {
            eprintln!("note: {why}");
            return (cmd, None);
        }
        Plan::Rewrite { target, dir, batch } => (target, dir, batch),
    };
    let file = match Once::write(&dir, &batch) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "note: could not write the launch file in {} ({e}), so the bridge provider is \
                 not being started — running the game unchanged.",
                dir.display()
            );
            return (cmd, None);
        }
    };
    // A command line is `String`s here, and a path spelled lossily names a
    // different file — so a prefix whose bytes are not UTF-8 declines rather
    // than being approximated into the launch.
    let Some(path) = file.path().to_str().map(str::to_owned) else {
        eprintln!(
            "note: {} cannot be named on a command line, so the bridge provider is not being \
             started — running the game unchanged.",
            file.path().display()
        );
        return (cmd, None);
    };
    eprintln!(
        "note: the bridge provider will be started inside this game's Proton session, before \
         the game. That is the ordering; whether the game then reads tracking data from it is \
         not checked here."
    );
    point_at(&mut cmd, target, path);
    (cmd, Some(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The command as Steam actually expands it, read off a live launch on this
    /// machine on 2026-09-28.
    fn steam_command(game: &str) -> Vec<String> {
        args(&[
            "/home/u/.local/share/Steam/ubuntu12_32/reaper",
            "SteamLaunch",
            "AppId=2074920",
            "--",
            "/games/steamapps/common/SteamLinuxRuntime_4/_v2-entry-point",
            "--verb=waitforexitandrun",
            "--",
            "/usr/share/steam/compatibilitytools.d/proton-cachyos-slr/proton",
            "waitforexitandrun",
            game,
            "-steam",
        ])
    }

    #[test]
    fn the_game_is_the_argument_after_protons_own_verb() {
        let cmd = steam_command("/games/common/The First Descendant/TheFirstDescendant.exe");
        assert_eq!(proton_target(&cmd), Some(9));
        assert!(cmd[9].ends_with("TheFirstDescendant.exe"));
    }

    /// The runtime's entry point spells the same word four arguments earlier.
    /// Anchoring on the word alone makes `--` the game.
    #[test]
    fn the_runtimes_own_verb_is_not_mistaken_for_protons() {
        let cmd = steam_command("/games/common/Thing/Thing.exe");
        let entry = cmd
            .iter()
            .position(|a| a == "--verb=waitforexitandrun")
            .expect("the entry point's verb is in the fixture");
        assert!(proton_target(&cmd).expect("a target") > entry + 1);
    }

    /// Everything after the real anchor is the game's, so a game argument that
    /// spells the anchor comes later — and a rule that took the last match
    /// would launch that argument instead.
    #[test]
    fn a_game_argument_cannot_impersonate_the_anchor() {
        let mut cmd = steam_command("/games/Real/Real.exe");
        cmd.push("/opt/proton".into());
        cmd.push("waitforexitandrun".into());
        cmd.push("/games/Fake/Fake.exe".into());
        let at = proton_target(&cmd).expect("a target");
        assert_eq!(cmd[at], "/games/Real/Real.exe");
    }

    #[test]
    fn a_native_command_is_not_a_proton_launch() {
        assert_eq!(
            proton_target(&args(&["./MyGame.x86_64", "-windowed"])),
            None
        );
        assert_eq!(proton_target(&args(&["/usr/bin/lutris"])), None);
    }

    /// `runinprefix` and the `get…path` verbs are how Proton is asked to do
    /// something that is not the game; rewriting one substitutes a launch for a
    /// lookup.
    #[test]
    fn only_protons_launching_verb_anchors_a_rewrite() {
        for verb in ["runinprefix", "getcompatpath", "run"] {
            let cmd = args(&["/opt/proton", verb, "/games/Thing/Thing.exe"]);
            assert_eq!(proton_target(&cmd), None, "{verb} should not anchor");
        }
    }

    #[test]
    fn a_verb_with_nothing_after_it_is_not_a_launch() {
        assert_eq!(proton_target(&args(&["/opt/proton", VERB])), None);
    }

    #[test]
    fn a_target_that_is_not_an_executable_is_not_a_game() {
        let cmd = args(&["/opt/proton", VERB, "/games/Thing/thing.txt"]);
        assert_eq!(proton_target(&cmd), None);
        let upper = args(&["/opt/proton", VERB, "/games/Thing/THING.EXE"]);
        assert_eq!(proton_target(&upper), Some(2));
    }

    // --- quoting --------------------------------------------------------
    //
    // Every expectation below was run through wine 11.18 on 2026-09-28: a
    // generated batch launched a stub that wrote its own `argv` to a file, and
    // the file held exactly the values named here.

    #[test]
    fn a_percent_is_doubled_so_cmd_does_not_expand_it() {
        // `"100%"` in a batch file is not 100 per cent; `%%` is.
        assert_eq!(batch_arg("100% off").as_deref(), Some(r#""100%% off""#));
        assert_eq!(batch_arg("%PATH%").as_deref(), Some(r#""%%PATH%%""#));
    }

    /// `cmd.exe` would end the argument at the closing quote, but the program's
    /// own `CommandLineToArgvW` reads `\"` as a literal quote and runs on.
    #[test]
    fn a_trailing_backslash_is_doubled_so_it_cannot_escape_the_closing_quote() {
        assert_eq!(batch_arg(r"C:\dir\").as_deref(), Some(r#""C:\dir\\""#));
        assert_eq!(batch_arg(r"C:\a\\").as_deref(), Some(r#""C:\a\\\\""#));
        // Only the trailing run: an interior backslash precedes something that
        // is not the closing quote, so it is already literal.
        assert_eq!(batch_arg(r"C:\a\b").as_deref(), Some(r#""C:\a\b""#));
    }

    #[test]
    fn spaces_and_shell_metacharacters_survive_inside_the_quotes() {
        for v in ["a b", "a^b", "c&d", "(p)", "!bang!", "x|y", "a>b", ""] {
            assert_eq!(
                batch_arg(v).as_deref(),
                Some(format!("\"{v}\"").as_str()),
                "{v:?}"
            );
        }
    }

    #[test]
    fn a_quote_cannot_be_carried_and_stops_the_rewrite() {
        assert_eq!(batch_arg(r#"say "hi""#), None);
    }

    #[test]
    fn a_newline_cannot_be_carried_and_stops_the_rewrite() {
        assert_eq!(batch_arg("a\nb"), None);
        assert_eq!(batch_arg("a\rb"), None);
        assert_eq!(batch_arg("a\tb"), None);
    }

    /// Measured: the identical launch worked from a CP850 batch and failed from
    /// UTF-8, UTF-8 with a BOM, UTF-16LE and `chcp 65001`. Which codepage a
    /// given prefix reads is not knowable here, so this declines.
    #[test]
    fn a_non_ascii_path_cannot_be_carried_and_stops_the_rewrite() {
        assert_eq!(batch_arg("/games/Wéird/game.exe"), None);
        assert_eq!(batch_arg("日本語"), None);
    }

    // --- the batch ------------------------------------------------------

    #[test]
    fn the_provider_starts_before_the_game_and_is_reaped_after_it() {
        let text = batch(r"Z:\games\Thing\Thing.exe", &args(&["-steam"])).expect("a batch");
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle:?} is absent"))
        };
        assert!(at("start /b") < at("start /wait"), "{text}");
        assert!(at("start /wait") < at("taskkill"), "{text}");
        assert!(
            text.contains(r#"start /b "" "C:\tobii-bridge\tobii-bridge.exe""#),
            "{text}"
        );
    }

    #[test]
    fn the_provider_is_started_with_no_register() {
        let text = batch(r"Z:\g\g.exe", &[]).expect("a batch");
        let line = text
            .lines()
            .find(|l| l.contains("tobii-bridge.exe") && l.starts_with("start"))
            .expect("the provider line");
        assert!(line.contains("--no-register"), "{line}");
    }

    /// Without saving it first, the exit code Steam is told the game finished
    /// with is `taskkill`'s.
    #[test]
    fn the_games_exit_code_is_saved_before_the_reap_and_returned_after_it() {
        let text = batch(r"Z:\g\g.exe", &[]).expect("a batch");
        let lines: Vec<&str> = text.lines().collect();
        let save = lines
            .iter()
            .position(|l| l.starts_with("set TOBII_GAME_RC="))
            .expect("the exit code is saved");
        let kill = lines
            .iter()
            .position(|l| l.starts_with("taskkill"))
            .expect("the reap");
        assert!(save < kill, "{text}");
        assert_eq!(lines.last().copied(), Some("exit /b %TOBII_GAME_RC%"));
    }

    #[test]
    fn the_batch_uses_the_line_endings_cmd_is_written_for() {
        let text = batch(r"Z:\g\g.exe", &[]).expect("a batch");
        assert!(text.ends_with("\r\n"));
        assert!(!text.contains("\n\n"), "no bare LF between lines");
    }

    #[test]
    fn one_uncarryable_argument_stops_the_whole_batch() {
        assert_eq!(batch(r"Z:\g\g.exe", &args(&["-ok", "a\"b"])), None);
    }

    // --- the decision ---------------------------------------------------

    /// A compat directory of the shape Steam names, holding a prefix that may
    /// or may not have our provider in it.
    ///
    /// A guard rather than a bare path, following `bridge`'s own `FakeWine`: a
    /// test that fails before its cleanup line otherwise leaves the tree
    /// behind, and these run in CI as well as here.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str, with_provider: bool) -> Self {
            let compat =
                std::env::temp_dir().join(format!("tobii-proton-{tag}-{}", std::process::id()));
            std::fs::remove_dir_all(&compat).ok();
            let dir = compat.join("pfx").join(crate::bridge::INSTALL_SUBDIR);
            std::fs::create_dir_all(&dir).expect("a scratch prefix");
            if with_provider {
                std::fs::write(dir.join(PROVIDER), b"not really an exe").expect("a provider");
            }
            Self(compat)
        }

        /// The value `$STEAM_COMPAT_DATA_PATH` would hold.
        fn compat(&self) -> &Path {
            &self.0
        }

        /// Where the batch file goes.
        fn dir(&self) -> PathBuf {
            self.0.join("pfx").join(crate::bridge::INSTALL_SUBDIR)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn a_non_steam_program_is_run_exactly_as_given() {
        let scratch = Scratch::new("native", true);
        let plan = plan(&args(&["./MyGame.x86_64", "-w"]), Some(scratch.compat()));
        assert_eq!(plan, Plan::AsGiven);
    }

    #[test]
    fn a_proton_launch_with_the_provider_installed_is_rewritten() {
        let scratch = Scratch::new("rewrite", true);
        let cmd = steam_command("/games/common/Thing/Thing.exe");
        match plan(&cmd, Some(scratch.compat())) {
            Plan::Rewrite { target, dir, batch } => {
                assert_eq!(cmd[target], "/games/common/Thing/Thing.exe");
                assert_eq!(dir, scratch.dir());
                assert!(
                    batch.contains(r"Z:\games\common\Thing\Thing.exe"),
                    "{batch}"
                );
                // The game's own arguments move into the batch, because they
                // come off Proton's command line with it.
                assert!(batch.contains("\"-steam\""), "{batch}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The conservative rule: without our own artifact in that prefix there is
    /// nothing to start, and the launch is left alone.
    #[test]
    fn a_prefix_without_the_provider_is_declined_and_says_where_it_looked() {
        let scratch = Scratch::new("bare", false);
        match plan(
            &steam_command("/games/Thing/Thing.exe"),
            Some(scratch.compat()),
        ) {
            Plan::Declined(why) => {
                assert!(
                    why.contains(&scratch.compat().join("pfx").display().to_string()),
                    "{why}"
                );
                assert!(why.contains("tobii bridge install"), "{why}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// Without it there is no evidence about which prefix this launch uses, and
    /// an inferred one would start a provider somewhere the game is not.
    #[test]
    fn a_proton_launch_with_no_compat_path_is_declined_rather_than_guessed() {
        match plan(&steam_command("/games/Thing/Thing.exe"), None) {
            Plan::Declined(why) => assert!(why.contains("STEAM_COMPAT_DATA_PATH"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_game_path_a_batch_cannot_spell_is_declined_rather_than_mangled() {
        let scratch = Scratch::new("unicode", true);
        match plan(
            &steam_command("/games/Wéird/Thing.exe"),
            Some(scratch.compat()),
        ) {
            Plan::Declined(why) => assert!(why.contains("non-ASCII"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_games_arguments_come_off_the_command_line_with_the_game() {
        let mut cmd = steam_command("/games/Thing/Thing.exe");
        let target = proton_target(&cmd).expect("a target");
        point_at(
            &mut cmd,
            target,
            "/pfx/drive_c/tobii-bridge/launch-1.bat".into(),
        );
        assert_eq!(
            cmd.last().map(String::as_str),
            Some("/pfx/drive_c/tobii-bridge/launch-1.bat")
        );
        // `-steam` went into the batch; a copy here would be an argument to the
        // batch file instead.
        assert!(!cmd.iter().any(|a| a == "-steam"), "{cmd:?}");
        assert_eq!(cmd[target - 1], VERB);
    }

    // --- the file -------------------------------------------------------

    #[test]
    fn the_batch_is_written_for_one_launch_and_removed_with_it() {
        let scratch = Scratch::new("once", false);
        let path = {
            let once = Once::write(&scratch.dir(), "@echo off\r\n").expect("written");
            let written = std::fs::read_to_string(once.path()).expect("the batch is on disk");
            assert_eq!(written, "@echo off\r\n");
            once.path().to_path_buf()
        };
        assert!(!path.exists(), "{} outlived the launch", path.display());
    }

    // --- arranging it ---------------------------------------------------

    #[test]
    fn arranging_a_proton_launch_points_it_at_the_batch_and_keeps_the_file_alive() {
        let scratch = Scratch::new("arrange", true);
        let (cmd, file) = arrange(
            steam_command("/games/Thing/Thing.exe"),
            Some(scratch.compat()),
        );
        let file = file.expect("the batch file is handed back to be held");
        assert_eq!(cmd.last().map(PathBuf::from).as_deref(), Some(file.path()));
        assert!(file.path().is_file(), "it must exist while the game runs");
    }

    /// The disk can refuse, and the answer to that is the launch Steam asked
    /// for — not a command line pointing at a file that was never written.
    #[test]
    fn a_batch_that_cannot_be_written_leaves_the_launch_exactly_as_it_was() {
        let scratch = Scratch::new("unwritable", true);
        // A directory where the file goes: `write` fails with `EISDIR` for
        // root as much as for anybody, which a permission bit would not.
        let blocked = scratch
            .dir()
            .join(format!("launch-{}.bat", std::process::id()));
        std::fs::create_dir_all(&blocked).expect("something in the file's way");
        let original = steam_command("/games/Thing/Thing.exe");
        let (cmd, file) = arrange(original.clone(), Some(scratch.compat()));
        assert_eq!(cmd, original);
        assert!(file.is_none());
    }

    #[test]
    fn arranging_anything_else_hands_the_command_straight_back() {
        for command in [
            args(&["./MyGame.x86_64", "-w"]),
            steam_command("/games/Thing/Thing.exe"),
        ] {
            let (cmd, file) = arrange(command.clone(), None);
            assert_eq!(cmd, command);
            assert!(file.is_none());
        }
    }

    // --- the batch, actually run ----------------------------------------

    /// Everything above asserts about a string. This runs it.
    ///
    /// Ignored because it needs `wine` and two Windows executables this crate
    /// cannot build (CI has neither). `$TOBII_PROTON_E2E` names a directory
    /// holding both:
    ///
    /// * `tobii-bridge.exe` — writes the file `%HELPER_OUT%` names, then runs
    ///   until something kills it.
    /// * `game.exe` — writes one `[argument]` per line to the file
    ///   `%ARGVDUMP_OUT%` names, then exits with code 7.
    ///
    /// Run it with
    /// `TOBII_PROTON_E2E=<dir> cargo test -p tobii-cli -- --ignored e2e`.
    #[test]
    #[ignore = "needs wine and two Windows stubs (TOBII_PROTON_E2E)"]
    fn e2e_the_generated_batch_starts_the_provider_runs_the_game_and_reaps_it() {
        let stubs = PathBuf::from(
            std::env::var("TOBII_PROTON_E2E").expect("TOBII_PROTON_E2E names the stub directory"),
        );
        let scratch = Scratch::new("e2e", false);
        let prefix = scratch.compat().join("pfx");
        let dir = scratch.dir();
        std::fs::copy(stubs.join(PROVIDER), dir.join(PROVIDER)).expect("the provider stub");

        let helper_out = scratch.compat().join("helper.txt");
        let argv_out = scratch.compat().join("argv.txt");
        // The battery from the module header, each one a character `cmd.exe`
        // would otherwise act on.
        let battery = args(&[
            "-w indowed",
            "100%",
            "a^b",
            "c&d",
            "(p)",
            "!bang!",
            r"C:\trailing\",
            "",
        ]);
        let batch = batch(
            &crate::bridge::wine_path_for(&stubs.join("game.exe")),
            &battery,
        )
        .expect("the battery is carryable");
        let file = Once::write(&dir, &batch).expect("the batch is written");

        let status = std::process::Command::new("wine")
            .env("WINEPREFIX", &prefix)
            .env("WINEDEBUG", "-all")
            .env("HELPER_OUT", crate::bridge::wine_path_for(&helper_out))
            .env("ARGVDUMP_OUT", crate::bridge::wine_path_for(&argv_out))
            .arg(file.path())
            .status()
            .expect("wine runs");

        let argv = std::fs::read_to_string(&argv_out).unwrap_or_default();
        let helper = helper_out.is_file();
        // `wineserver -w` is what the next Steam launch does, and it returns
        // only once every wine process on this prefix is gone: if the provider
        // outlived the game, this is the launch that would hang.
        let reaped = {
            let mut w = std::process::Command::new("wineserver")
                .env("WINEPREFIX", &prefix)
                .arg("-w")
                .spawn()
                .expect("wineserver runs");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                match w.try_wait() {
                    Ok(Some(_)) => break true,
                    _ if std::time::Instant::now() > deadline => {
                        let _ = w.kill();
                        let _ = w.wait();
                        break false;
                    }
                    _ => std::thread::sleep(std::time::Duration::from_millis(200)),
                }
            }
        };
        drop(file);
        // Before the tree goes: a wineserver still serving a directory being
        // deleted is how a scratch prefix outlives its test.
        let _ = std::process::Command::new("wineserver")
            .env("WINEPREFIX", &prefix)
            .arg("-k")
            .status();

        assert!(helper, "the provider never started");
        assert_eq!(
            argv.lines().collect::<Vec<_>>(),
            battery.iter().map(|a| format!("[{a}]")).collect::<Vec<_>>(),
            "the game did not receive its arguments intact"
        );
        assert_eq!(
            status.code(),
            Some(7),
            "the game's exit code did not come back"
        );
        assert!(
            reaped,
            "the provider outlived the game and still holds the prefix"
        );
    }
}
