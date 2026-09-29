//! Put our provider on the right side of the wineserver lock.
//!
//! # What Proton is pointed at, and why it is not a batch file
//!
//! This module used to build a batch file and point Proton's
//! `waitforexitandrun` at it, which is the shape the studied launcher uses.
//! **Proton does not wait for a batch file.** Its `steam.exe` helper runs an
//! `.exe` target through `CreateProcessW` and waits on it, and a `.bat` target
//! through `ShellExecuteW`, which returns at once — confirmed on four Proton
//! builds on the machine this was written on, against a control with an `.exe`
//! target that waits correctly. Proton then reports success about a second in,
//! while `cmd.exe`, the provider and the game are all still starting: Steam
//! records the game as exited and [`crate::game`] drops the tracker out from
//! under a game that is still running.
//!
//! That is survivable for a launcher whose whole job is to start and reap
//! another program, which is why the studied one ships it and works. It is not
//! survivable here.
//!
//! So Proton is pointed at **the provider**, which is an `.exe` and already in
//! the prefix — `tobii bridge install` puts it there, and this module declines
//! unless it is. The game follows it after `--launch`, and the provider starts
//! itself, runs the game, waits, and exits with the game's code. See
//! `bridge/provider/src/main.rs`, which is where the sequencing now lives.
//!
//! Three things fall out of the target being an `.exe` rather than a batch:
//! Steam's bookkeeping stays correct, the reap is process exit rather than a
//! `taskkill` that can be missed, and the game's command line travels as argv.
//! The last one deleted the most code here — a batch file cannot carry a quote,
//! a control character or anything non-ASCII, all measured, so a game under a
//! path with an umlaut in it could not be wrapped at all.
//!
//! `docs/wiki/Quality-and-Risks.md` §11.3l has the measurements that killed the
//! batch, and §11.3o those for this: against real Proton, pointing
//! `waitforexitandrun` at the provider returns the game's exit code after the
//! full run, the game's arguments arrive byte-identical, and the provider's
//! mapping and port come up in the host's own network namespace. What is still
//! unwatched is a game reading the mapping — that is a title, not a mechanism.
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

// `arrange` is the entry point and `tobii game` calls it; the rest is reachable
// only through it or through tests, which is what this allows.
#![allow(dead_code)]

use std::path::Path;

/// Proton's verb for "run this and wait for it", the one Steam launches a title
/// with and the only one this module treats as a game launch.
///
/// The other verbs are tooling — `runinprefix`, `getcompatpath` and friends run
/// something *in* the prefix without being the game — and rewriting one of
/// those would substitute a game launch for a lookup.
const VERB: &str = "waitforexitandrun";

/// The provider, inside the prefix.
///
/// The provider, taken from the module that installs it rather than spelled
/// again here — as the directory beside it already is.
const PROVIDER: &str = crate::bridge::PROVIDER_EXE;

/// `$STEAM_COMPAT_DATA_PATH`, as this process received it.
///
/// Read here rather than at the call site so that the one place that knows what
/// a Proton launch looks like is also the one place that reads Steam's word for
/// where it is. Steam sets it for a Proton launch and for nothing else, which
/// is why [`plan`] declines rather than guessing when it is absent.
pub(crate) fn compat_data_path() -> Option<std::path::PathBuf> {
    std::env::var_os("STEAM_COMPAT_DATA_PATH").map(std::path::PathBuf::from)
}

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
    /// Put the provider in front of the game: Proton runs `provider --launch
    /// <game> <args…>` instead of the game.
    Rewrite {
        /// Index in the command of the game executable Proton was told to run.
        /// The provider goes in front of it; the game and its arguments stay
        /// where they are.
        target: usize,
        /// The provider inside this game's prefix, as Proton will be given it.
        provider: String,
        /// The game, in the Windows spelling the provider will hand to
        /// `CreateProcess`. `cmd[target]` is a Unix path, which is right for
        /// Proton and wrong once we are inside the prefix.
        game: String,
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
    // A command line is `String`s here, and a path spelled lossily names a
    // different file — so a prefix whose bytes are not UTF-8 declines rather
    // than being approximated into the launch.
    let exe = dir.join(PROVIDER);
    let Some(provider) = exe.to_str().map(str::to_owned) else {
        return Plan::Declined(format!(
            "{} cannot be named on a command line, so the bridge provider is not being \
             started — running the game unchanged",
            exe.display()
        ));
    };
    Plan::Rewrite {
        target,
        provider,
        game: crate::bridge::wine_path_for(Path::new(&cmd[target])),
    }
}

/// Put the provider in front of the game.
///
/// The game's arguments stay exactly where they are, and that is the whole
/// advantage of an `.exe` over a batch file: they travel as argv, so a quote, a
/// percent sign, a trailing backslash or a non-ASCII path needs nothing done to
/// it. The batch form had to refuse all four, and a refusal meant the provider
/// did not start for that game at all.
///
/// The game's path is replaced with its Windows spelling, because the process
/// reading it is inside the prefix. Proton gets the provider's Unix path, which
/// is what it gets for a game.
pub(crate) fn point_at(cmd: &mut Vec<String>, target: usize, provider: String, game: String) {
    cmd[target] = game;
    cmd.insert(target, "--launch".to_string());
    cmd.insert(target, provider);
}

pub(crate) fn arrange(mut cmd: Vec<String>, compat: Option<&Path>) -> Vec<String> {
    let (target, provider, game) = match plan(&cmd, compat) {
        Plan::AsGiven => return cmd,
        Plan::Declined(why) => {
            eprintln!("note: {why}");
            return cmd;
        }
        Plan::Rewrite {
            target,
            provider,
            game,
        } => (target, provider, game),
    };
    // What this says is what was arranged, and no more. The ordering, the reap
    // and the exit code are the provider's own and are tested there; that
    // Proton runs what it is pointed at, and that the provider then shares the
    // wineserver session the game is in, are the two steps nobody here has
    // watched happen. Saying "will be started inside this game's Proton
    // session" would state the second as fact and disclaim only the outcome
    // after it, which is the shape this project keeps having to correct.
    eprintln!(
        "note: this launch is wrapped — the provider starts first, the game runs, and both \
         end together. Whether the game then reads tracking from it is not checked here. If \
         the game does not start at all, take `tobii game -- ` back out of the launch options \
         and it launches exactly as before."
    );
    point_at(&mut cmd, target, provider, game);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        // The real chain spells the runtime's verb `--verb=waitforexitandrun`,
        // which can never equal `VERB` — so that fixture alone exercises the
        // string comparison and nothing else, and passes with the `proton`
        // check deleted outright. What the check is for is a *bare*
        // `waitforexitandrun` earlier in the line, which the separated
        // spelling of that same flag produces.
        let mut cmd = steam_command("/games/common/Thing/Thing.exe");
        let entry = cmd
            .iter()
            .position(|a| a == "--verb=waitforexitandrun")
            .expect("the entry point's verb is in the fixture");
        cmd.splice(entry..=entry, args(&["--verb", "waitforexitandrun"]));

        // Proton's own pair is still the anchor, so the game is still the game.
        let at = proton_target(&cmd).expect("a target");
        assert_eq!(cmd[at], "/games/common/Thing/Thing.exe", "{cmd:?}");
        assert_eq!(cmd[at - 1], VERB);
        assert!(
            Path::new(&cmd[at - 2])
                .file_name()
                .is_some_and(|n| n == "proton"),
            "the anchor is Proton's own pair: {cmd:?}"
        );
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

    // --- the batch ------------------------------------------------------

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

        /// Where the bridge's files go, and so where the provider is.
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
            Plan::Rewrite {
                target,
                provider,
                game,
            } => {
                assert_eq!(cmd[target], "/games/common/Thing/Thing.exe");
                // Proton gets a Unix path, because that is what Proton takes.
                assert_eq!(provider, scratch.dir().join(PROVIDER).to_string_lossy());
                // The provider gets the Windows spelling, because the process
                // that reads it is inside the prefix.
                assert_eq!(game, r"Z:\games\common\Thing\Thing.exe");
            }
            other => panic!("{other:?}"),
        }
    }

    /// A path a batch file could not carry is carried.
    ///
    /// The batch form had to refuse a quote, a control character, a `%`, a
    /// trailing backslash and anything non-ASCII, and a refusal meant the
    /// provider did not start for that game at all — on a machine whose games
    /// live under a name with an umlaut in it, for every game. An argv has no
    /// such limits, and this is the test that says the limits are gone rather
    /// than merely untested.
    #[test]
    fn a_path_no_batch_file_could_carry_is_carried() {
        let scratch = Scratch::new("awkward", true);
        let awkward = "/games/Über spiele/100% Orange/Thing.exe";
        match plan(&steam_command(awkward), Some(scratch.compat())) {
            Plan::Rewrite { game, .. } => {
                assert_eq!(game, r"Z:\games\Über spiele\100% Orange\Thing.exe");
            }
            other => panic!("a path with a space, a percent and non-ASCII: {other:?}"),
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
    fn the_games_arguments_stay_on_the_command_line_after_the_game() {
        let mut cmd = steam_command("/games/Thing/Thing.exe");
        let target = proton_target(&cmd).expect("a target");
        point_at(
            &mut cmd,
            target,
            "/pfx/drive_c/tobii-bridge/tobii-bridge.exe".into(),
            r"Z:\games\Thing\Thing.exe".into(),
        );
        assert_eq!(
            &cmd[target..],
            &[
                "/pfx/drive_c/tobii-bridge/tobii-bridge.exe".to_string(),
                "--launch".to_string(),
                r"Z:\games\Thing\Thing.exe".to_string(),
                "-steam".to_string(),
            ],
            "the provider goes in FRONT of the game, and the game keeps its own \
             arguments — under the batch they had to come off with it: {cmd:?}"
        );
        // Exactly one `-steam`: under the batch the game's arguments had to come
        // off the command line, because a copy left behind would have been
        // handed to `cmd.exe` as arguments to the batch file. They stay now, and
        // a second copy would reach the game twice.
        assert_eq!(cmd.iter().filter(|a| *a == "-steam").count(), 1, "{cmd:?}");
        assert_eq!(cmd[target - 1], VERB);
    }

    // --- arranging it ---------------------------------------------------

    #[test]
    fn arranging_a_proton_launch_points_it_at_the_provider() {
        let scratch = Scratch::new("arrange", true);
        let cmd = arrange(
            steam_command("/games/Thing/Thing.exe"),
            Some(scratch.compat()),
        );
        let provider = scratch.dir().join(PROVIDER);
        assert_eq!(
            &cmd[cmd.len() - 4..],
            &[
                provider.to_string_lossy().into_owned(),
                "--launch".to_string(),
                crate::bridge::wine_path_for(Path::new("/games/Thing/Thing.exe")),
                "-steam".to_string(),
            ],
            "Proton is pointed at the provider, with the game after `--launch`: {cmd:?}"
        );
        assert!(
            provider.is_file(),
            "and at a provider that is actually there — nothing is written for the launch, \
             which is why there is no file to clean up"
        );
    }

    #[test]
    fn arranging_anything_else_hands_the_command_straight_back() {
        for command in [
            args(&["./MyGame.x86_64", "-w"]),
            steam_command("/games/Thing/Thing.exe"),
        ] {
            assert_eq!(arrange(command.clone(), None), command);
        }
    }

    // --- the batch, actually run ----------------------------------------

    /// The real provider, under real wine, starts a game and ends with it.
    ///
    /// Ignored and gated because it needs three things this repository does not
    /// ship: `wine`, a built `tobii-bridge.exe` (`scripts/build-bridge.sh`), and
    /// a stub `game.exe` that writes its own `argv` one bracketed value per line
    /// to `%ARGVDUMP_OUT%` and exits 7. Point `TOBII_PROTON_E2E` at the
    /// directory holding the stub.
    ///
    /// What it is for: the sequencing moved out of this crate and into the
    /// provider when Proton's target became an `.exe` rather than a batch file,
    /// and `bridge/` is a separate workspace that cross-compiles to Windows —
    /// nothing in it can run a test on this machine. This is the only place the
    /// mechanism can be exercised end to end.
    ///
    /// The argument battery is the one the batch form needed rules for, plus
    /// the two it had to refuse outright. Under `--launch` they are argv, so the
    /// expectation is that every one of them arrives unchanged.
    #[test]
    #[ignore = "needs wine, a built tobii-bridge.exe and a stub game (TOBII_PROTON_E2E)"]
    fn e2e_the_provider_runs_the_game_passes_its_arguments_and_ends_with_it() {
        let stubs = PathBuf::from(
            std::env::var("TOBII_PROTON_E2E").expect("TOBII_PROTON_E2E names the stub directory"),
        );
        let provider = PathBuf::from("bridge/target/x86_64-pc-windows-gnu/release").join(PROVIDER);
        assert!(
            provider.is_file(),
            "{} is not built — run scripts/build-bridge.sh",
            provider.display()
        );
        let scratch = Scratch::new("e2e", false);
        let prefix = scratch.compat().join("pfx");
        let argv_out = scratch.compat().join("argv.txt");

        // Every one of these had a rule in the batch form, and the last three
        // could not be carried at all: a quote, a non-ASCII path and a newline.
        let battery = args(&[
            "-w indowed",
            "100%",
            "a^b",
            "c&d",
            "(p)",
            "!bang!",
            r"C:\trailing\",
            "",
            "say \"hello\"",
            "Über",
        ]);

        let status = std::process::Command::new("wine")
            .env("WINEPREFIX", &prefix)
            .env("WINEDEBUG", "-all")
            .env("ARGVDUMP_OUT", crate::bridge::wine_path_for(&argv_out))
            .arg(&provider)
            .arg("--launch")
            .arg(crate::bridge::wine_path_for(&stubs.join("game.exe")))
            .args(&battery)
            .status()
            .expect("wine runs");

        let argv = std::fs::read_to_string(&argv_out).unwrap_or_default();
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
        // Before the tree goes: a wineserver still serving a directory being
        // deleted is how a scratch prefix outlives its test.
        let _ = std::process::Command::new("wineserver")
            .env("WINEPREFIX", &prefix)
            .arg("-k")
            .status();

        assert_eq!(
            argv.lines().collect::<Vec<_>>(),
            battery.iter().map(|a| format!("[{a}]")).collect::<Vec<_>>(),
            "the game did not receive its arguments intact — the last three are the ones a \
             batch file could not carry at all"
        );
        assert_eq!(
            status.code(),
            Some(7),
            "the game's exit code did not come back through the provider"
        );
        assert!(
            reaped,
            "the provider outlived the game and still holds the prefix — the next launch's \
             `wineserver -w` is what would block"
        );
    }
}
