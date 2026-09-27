//! The hub's game-setup window: pick an installed game, see the three things
//! that have to be configured for it, and do the two this program owns.
//!
//! # The three things
//!
//! 1. **This program's own settings** — game output on, and a destination to
//!    send to. Written here, through [`crate::games::edit_config`], into the
//!    same `games.toml` the card behind this window reads.
//! 2. **The Wine bridge** — the TrackIR/FreeTrack registration inside the
//!    game's Proton prefix. Installed by shelling out to `tobii bridge install
//!    --steam <appid>` and showing that program's words verbatim.
//! 3. **The game's own configuration files** — read-only, always, and only
//!    where a profile says what to read. This program never writes them.
//!
//! # What this window may not do
//!
//! It takes **no claim on the tracker**. It reads files and runs one
//! subprocess; it wants no frames, and [`crate::hold_while_open`] is called
//! from nowhere in here. The argument is [`crate::help`]'s, word for word:
//! lighting the illuminators to show somebody a paragraph is precisely the
//! behaviour the whole demand mechanism exists to prevent.
//! `tests/game_setup_window.rs` asserts it against the device thread's own
//! list of claim reasons.
//!
//! It never spawns `wine`, on any path, and it never passes `--force` to the
//! installer — see [`install_argv`].
//!
//! It never writes a game's own configuration file. The reading goes through
//! `tobii-gameconf`, which is the crate that promises not to write; the
//! promise this module has to keep is one layer up, and it is that **there is
//! no "fix it for me" button in the third block, ever**. A bindings file is
//! hours of somebody's work in a format its author can change in any patch: a
//! wrong read costs a confusing sentence, a wrong write costs the bindings.
//!
//! # The split
//!
//! Every sentence this window shows is produced by a function that takes its
//! inputs and returns a `String`, so it can be asserted in CI, which has no
//! display. The widgets only show what those functions said. The one function
//! that touches the filesystem on its own account is [`scan`], whose `home` is
//! a parameter rather than a read of `$HOME` for the reason `bridge.rs` gives
//! about its own split: `$HOME` is process-global and these tests run in
//! parallel. No test in this file calls it.
//!
//! # Why there is no timer
//!
//! The `refresh` closure in [`open_with`] recomputes the whole page from disk
//! and rewrites every label. It runs on entering the game page, after a write,
//! and when a subprocess this window started finishes — and **not** on the
//! 33 ms hub tick that drives [`crate::games::GamesRow::refresh`]. That card
//! has to follow a file a terminal can edit under it. This window is modal
//! over the hub, is itself the thing editing the file, and the one other thing
//! that can change while it is open is the installer it started, which reports
//! itself.

use gtk::prelude::*;
use gtk::{glib, Align, Application, Label, Orientation};

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use tobii_config::profiles;
use tobii_gameconf::{attrs, binds, Lookup, Source};
use tobii_output::games::OutputConfig;
use tobii_steam::App;

use crate::games::{strength_index, STRENGTHS};

/// Where `tobii bridge install` puts its files inside a prefix, and the one
/// file that decides whether anything can load at all.
///
/// Spelled here rather than imported: `tobii-cli` is a binary crate with no
/// library target, so nothing in the hub can link `bridge.rs`. The two names
/// are that file's `INSTALL_SUBDIR` and `REQUIRED_ARTIFACT`, and the wording
/// in [`bridge_block`] is careful to claim only what a stat of these two paths
/// can support — it never says "the bridge is installed", because two registry
/// values inside the prefix decide that and this window does not read them.
const BRIDGE_SUBDIR: &str = "drive_c/tobii-bridge";
const BRIDGE_ARTIFACT: &str = "freetrackclient64.dll";

/// The window's title, which is also how the display test finds it.
pub const TITLE: &str = "Set up a game";

/// The stack, so the display test can find it without matching on text.
pub const STACK_NAME: &str = "gamesetup-stack";
/// The search box, likewise.
pub const SEARCH_NAME: &str = "gamesetup-search";
/// The list of games, likewise.
pub const LIST_NAME: &str = "gamesetup-games";

/// The page with the game list on it.
pub const PAGE_PICK: &str = "pick";
/// The page for one game.
pub const PAGE_GAME: &str = "game";
/// The page for a machine with no installed games. Only ever the page the
/// window OPENS on: there is no path from a game back to it.
pub const PAGE_NOTHING: &str = "nothing";

// ------------------------------------------------------------------- the scan

/// Everything this window reads off the machine before it draws anything.
///
/// Injected rather than read inside [`open_with`] so the whole window can be
/// opened in a test against a synthetic library: CI runs as root with no real
/// `$HOME` and no Steam install, and a window that read `$HOME` itself could
/// only ever be tested on somebody's laptop.
#[derive(Debug, Clone, Default)]
pub struct Scan {
    /// The home the libraries were read from. Every later filesystem question
    /// about a game goes through this and never through `$HOME` a second time,
    /// so one window is one machine's worth of answers.
    pub home: PathBuf,
    /// Where per-game profiles are read from.
    pub profiles_dir: PathBuf,
    /// What Steam says is installed, in the order [`tobii_steam::apps`] gives.
    pub apps: Vec<App>,
    /// The libraries `libraryfolders.vdf` names that are not on this machine.
    ///
    /// Carried so that every sentence about something not being found can say
    /// this is one of the reasons it might not have been found. It is the
    /// shape of every genuinely user-visible bug this project has found in
    /// itself: an unmounted drive reported as "the game never saved this".
    pub missing: Vec<PathBuf>,
}

/// Read the machine.
///
/// `home` is a parameter, not `$HOME`: see the module docs.
pub fn scan(home: &Path, profiles_dir: &Path) -> Scan {
    Scan {
        home: home.to_path_buf(),
        profiles_dir: profiles_dir.to_path_buf(),
        apps: tobii_steam::apps(home),
        missing: tobii_steam::missing_libraries(home),
    }
}

/// A list of paths as a sentence can carry them.
fn list_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The singular or the plural, for a count.
fn plural(n: usize, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 {
        one
    } else {
        many
    }
}

/// What this program cannot rule out, whenever it is about to report that
/// something was not found.
///
/// [`tobii_steam::missing_libraries`] exists precisely so a caller can say
/// this, and its own documentation says what a caller who does not has done:
/// told the user a confident negative about a game that is installed. It also
/// says what it is **not** — it is not every reason a title can be absent —
/// so this is worded as "cannot rule out" rather than as "it is there".
///
/// [`None`] when every library named is present, which is the other half of
/// the same rule: a caller that appended this sentence unconditionally would
/// invent a library nobody has.
fn missing_note(missing: &[PathBuf]) -> Option<String> {
    if missing.is_empty() {
        return None;
    }
    let n = missing.len();
    Some(format!(
        "One thing this program cannot rule out: Steam lists {n} {lib} that {is} not on this \
         machine right now — {paths}. Anything installed on one of those looks from here \
         exactly like something that was never there.",
        lib = plural(n, "library", "libraries"),
        is = plural(n, "is", "are"),
        paths = list_paths(missing),
    ))
}

// ---------------------------------------------------------------- the picker

/// One row of the game list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PickRow {
    pub appid: String,
    pub name: String,
    /// The app id, and — when there is none — that there is no Proton prefix
    /// yet. Said here as well as in the bridge block, because this is where
    /// somebody is looking when they wonder why a game they just installed has
    /// nothing to install into.
    pub subtitle: String,
}

/// The pick page, as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Picker {
    pub rows: Vec<PickRow>,
    /// The line under the list, always present: what this list is a list of.
    pub census: String,
    /// Whether the census is a warning rather than a statement — true exactly
    /// when a library Steam names is not on this machine.
    pub census_warn: bool,
    /// What to show instead of rows when the query matched nothing.
    ///
    /// [`None`] when there are rows. It repeats the missing-library sentence
    /// on purpose: this is the moment a user has typed the name of a game they
    /// own and got nothing back, which is the exact moment a silent list
    /// becomes a lie.
    pub no_match: Option<String>,
}

/// Build the pick page from what was scanned and what has been typed.
///
/// `has_prefix` is injected so this is pure: it is the only question in here
/// that would otherwise touch a disk.
///
/// The filter is case-insensitive substring, over the name **and** the app id.
/// [`tobii_steam::resolve`] documents the name half and is deliberately not
/// called: the user picks a row rather than typing a string, so
/// [`tobii_steam::Match::Many`] cannot arise and needs no caller — and
/// `resolve`'s own documentation warns that an empty needle answers `Many`
/// over everything, which is what a search box holds before anybody types.
/// The app id half is a widening and never a narrowing: this box cannot hide a
/// row that the same text typed at a terminal would have found.
pub(crate) fn picker(
    apps: &[App],
    missing: &[PathBuf],
    has_prefix: &dyn Fn(&str) -> bool,
    query: &str,
) -> Picker {
    let mut sorted: Vec<&App> = apps.iter().collect();
    // Steam's own plumbing last, and still in the list. `looks_like_tool` is a
    // heuristic on the name and says so; hiding a row it gets wrong would hide
    // the game somebody was looking for, which is the one failure it must not
    // have.
    sorted.sort_by_key(|a| {
        (
            tobii_steam::looks_like_tool(&a.name),
            a.name.to_lowercase(),
            a.appid.clone(),
        )
    });

    let q = query.trim().to_lowercase();
    let rows: Vec<PickRow> = sorted
        .iter()
        .filter(|a| q.is_empty() || a.name.to_lowercase().contains(&q) || a.appid.contains(&q))
        .map(|a| PickRow {
            appid: a.appid.clone(),
            name: a.name.clone(),
            subtitle: if has_prefix(&a.appid) {
                format!("app id {}", a.appid)
            } else {
                format!("app id {} · no Proton prefix yet", a.appid)
            },
        })
        .collect();

    let n = apps.len();
    let mut census = format!(
        "{n} {thing} installed, from Steam's own manifests on this machine.",
        thing = plural(n, "title", "titles")
    );
    let census_warn = !missing.is_empty();
    if let Some(note) = missing_note(missing) {
        census.push(' ');
        census.push_str(&note);
    }

    let no_match = rows.is_empty().then(|| {
        let mut s = format!(
            "Nothing installed here is called “{}”, and no app id contains it. This list is \
             what Steam's manifests on this machine say, matched on the title and on the app \
             id.",
            query.trim()
        );
        if let Some(note) = missing_note(missing) {
            s.push_str("\n\n");
            s.push_str(&note);
        }
        s
    });

    Picker {
        rows,
        census,
        census_warn,
        no_match,
    }
}

/// The page shown when Steam has nothing installed at all.
///
/// Its own page rather than an empty list, because an empty list under a
/// search box invites somebody to keep typing at it.
pub(crate) fn nothing_text(missing: &[PathBuf]) -> String {
    let mut s = "No installed Steam games were found on this machine.\n\n\
                 Steam records what is installed in a manifest beside each game. If Steam is \
                 installed somewhere this program does not look, nothing here will see it, \
                 and the rest of this window has nothing to work on."
        .to_string();
    if let Some(note) = missing_note(missing) {
        s.push_str("\n\n");
        s.push_str(&note);
    }
    s
}

// --------------------------------------------------- block 1: these settings

/// Where this program's output is going, named from the settings alone.
///
/// Written here rather than taken from [`crate::games::status_text`], which is
/// deliberately not called from this window at all. That function takes
/// `tracker_on` and a `JoystickStatus` — two live facts this window has no
/// reader for — and feeding it `false` and `JoystickStatus::Off` to get a
/// sentence out of it would print "Ready to send to…" to somebody whose
/// tracker is running. The card answers *what is happening*; this window
/// answers *what is configured*. Two questions, two vocabularies.
///
/// It is prose and not a predicate, which is why duplicating it is safe: the
/// predicates this window shares with the card — [`strength_index`] and
/// [`STRENGTHS`] — are imported rather than re-derived.
fn destinations(cfg: &OutputConfig) -> Vec<String> {
    let mut out = Vec::new();
    if cfg.joystick {
        out.push("a virtual joystick".to_string());
    }
    if let Some(addr) = &cfg.opentrack {
        out.push(format!("opentrack at {addr}"));
    }
    if let Some(port) = cfg.bridge_port {
        out.push(format!("the Wine bridge on port {port}"));
    }
    out
}

/// The strength as a clause, from the presets and never from comparing
/// degrees here.
fn strength_clause(cfg: &OutputConfig) -> String {
    match strength_index(cfg) {
        Some(i) => format!("at {} strength", STRENGTHS[i].0),
        None => "at a hand-tuned strength".to_string(),
    }
}

/// Block 1: this program's own settings, and the one button that changes them.
///
/// Returns the body and the button's caption, [`None`] when there is nothing
/// to press. Three states rather than the two a `enabled && joystick` gate
/// would give, because the middle one is real: somebody who has already set up
/// opentrack has game output on and no joystick, and telling them to "turn on"
/// a switch that is on is the kind of small untruth this whole window exists
/// not to tell.
pub(crate) fn settings_block(cfg: &OutputConfig) -> (String, Option<&'static str>) {
    let sinks = destinations(cfg);
    if cfg.enabled && cfg.joystick {
        return (
            format!(
                "Head tracking for games is on, sending to {list}, {strength}. Nothing to \
                 change here.",
                list = join_and(&sinks),
                strength = strength_clause(cfg),
            ),
            None,
        );
    }

    let mut body = if !cfg.enabled {
        "Head tracking for games is off.".to_string()
    } else if sinks.is_empty() {
        "Head tracking for games is on, but nothing is set to receive it.".to_string()
    } else {
        format!(
            "Head tracking for games is on, sending to {list}, {strength} — but not to a \
             virtual joystick.",
            list = join_and(&sinks),
            strength = strength_clause(cfg),
        )
    };
    body.push_str(
        "\n\nA virtual joystick is the destination that needs nothing else installed — no \
         Wine, no opentrack — and it works in native and Proton games alike. Whether it is \
         the right one for this game is not something this program knows: a game that speaks \
         TrackIR or FreeTrack wants the Wine bridge below instead.",
    );

    let caption = if cfg.enabled {
        // It is already on. Saying "turn on" here would be a caption that
        // describes something that has already happened.
        "Send to a virtual joystick"
    } else {
        "Turn on and send to a virtual joystick"
    };
    (body, Some(caption))
}

/// `a`, `a and b`, `a, b and c` — or "nothing" for an empty list, which no
/// caller passes but which must not read as an empty gap in a sentence.
fn join_and(items: &[String]) -> String {
    match items {
        [] => "nothing".to_string(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What a profile says about this program's own settings.
///
/// **Reported, never applied.** `tobii-output`'s `OutputConfig::apply_key` is
/// the one place that decides what a key means and whether a value parses, and
/// applying a file's settings behind a button captioned "send to a virtual
/// joystick" would be a button doing more than it said. So the block names
/// them and names the command that applies them.
///
/// [`None`] when the profile asks for none, which is every profile today —
/// there are none. Saying nothing is the right answer there; inventing a
/// heading over an empty list is not.
pub(crate) fn profile_settings_note(settings: &[(String, String)]) -> Option<String> {
    if settings.is_empty() {
        return None;
    }
    let pairs = settings
        .iter()
        .map(|(k, v)| format!("{k} = {v}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "The profile for this game also asks for {n} of this program's own {s}: {pairs}. This \
         window has not applied {them} — `tobii games set <key> <value>` in a terminal does.",
        n = settings.len(),
        s = plural(settings.len(), "settings", "settings"),
        them = plural(settings.len(), "it", "them"),
    ))
}

/// What a profile says about the bridge, for block 2.
///
/// [`None`] for [`profiles::Bridge::Unstated`], and that is the whole point of
/// that variant: a profile that does not mention the bridge has not said the
/// game does without one, and printing "this game does not need the bridge"
/// from an absence would be a claim nobody made.
pub(crate) fn profile_bridge_note(bridge: profiles::Bridge) -> Option<String> {
    match bridge {
        profiles::Bridge::Unstated => None,
        profiles::Bridge::Required => Some(
            "The profile for this game says it reads TrackIR or FreeTrack, so the bridge is \
             what it needs — the virtual joystick above will not reach it."
                .to_string(),
        ),
        profiles::Bridge::NotNeeded => Some(
            "The profile for this game says it does not need the bridge. Head tracking for it \
             goes through the settings above instead."
                .to_string(),
        ),
    }
}

/// What is appended to block 1 after a write, because a modal window that
/// changes a control the user cannot see is otherwise a leap of faith.
///
/// True by construction: the hub tick calls [`crate::games::GamesRow::refresh`],
/// which re-reads the same file and compares before writing, so an equal write
/// cannot re-enter the save handlers listening to those very widgets.
fn wrote_line(path: &Path) -> String {
    format!(
        "Written to {}. The card behind this window reads the same file and will follow.",
        path.display()
    )
}

// ------------------------------------------------------- block 2: the bridge

/// What a stat of the prefix found. Three cases, and the names say exactly how
/// much was looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BridgeState {
    /// [`tobii_steam::prefix`] found nothing.
    NoPrefix,
    /// A prefix, with no bridge artifact in it.
    Absent { prefix: PathBuf },
    /// A prefix with the bridge's files in it.
    Files { prefix: PathBuf, dir: PathBuf },
}

/// What the bridge block's button does when it is pressed. Both run the same
/// command; they differ only in what the caption promises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Install,
    Reinstall,
}

impl Action {
    pub fn caption(self) -> &'static str {
        match self {
            Action::Install => "Install the bridge",
            Action::Reinstall => "Reinstall",
        }
    }
}

/// Stat the prefix. The only filesystem call in block 2.
fn bridge_state(home: &Path, appid: &str) -> BridgeState {
    match tobii_steam::prefix(home, appid) {
        None => BridgeState::NoPrefix,
        Some(prefix) => {
            let dir = prefix.join(BRIDGE_SUBDIR);
            if dir.join(BRIDGE_ARTIFACT).is_file() {
                BridgeState::Files { prefix, dir }
            } else {
                BridgeState::Absent { prefix }
            }
        }
    }
}

/// Block 2: the Wine bridge.
///
/// Note the wording discipline: this window stats two paths, so it claims two
/// paths. It never says "the bridge is installed" — a record file and two
/// registry values inside the prefix are what decide that, and they belong to
/// the program that owns them. `Details` is `tobii bridge status`, which reads
/// them.
pub(crate) fn bridge_block(state: &BridgeState, missing: &[PathBuf]) -> (String, Option<Action>) {
    match state {
        BridgeState::NoPrefix => {
            let mut s = "No Proton prefix was found for this game. Steam makes one the first \
                         time a title runs under Proton, so if this is a Windows game, run it \
                         once and come back.\n\n\
                         If it is a native Linux game it will never have one, and the Wine \
                         bridge is not how it gets head tracking — the virtual joystick above \
                         is."
            .to_string();
            if let Some(note) = missing_note(missing) {
                s.push_str("\n\n");
                s.push_str(&note);
            }
            // No button. There is nothing to install into, and a button that
            // fails is worse than no button.
            (s, None)
        }
        BridgeState::Absent { prefix } => (
            format!(
                "The prefix is at {}.\nThe bridge's files are not in it.\n\n\
                 The bridge is what lets a game see a TrackIR or FreeTrack device from inside \
                 Wine. Installing it copies two small files into the prefix and sets two \
                 registry values. It does not touch the game or its saves.",
                prefix.display()
            ),
            Some(Action::Install),
        ),
        BridgeState::Files { dir, .. } => (
            format!(
                "The bridge's files are in this prefix, at {}.\n\n\
                 Whether the game will actually find them is a question about two registry \
                 values inside the prefix, which this window does not read. Details below is \
                 `tobii bridge status`, which does.",
                dir.display()
            ),
            Some(Action::Reinstall),
        ),
    }
}

// ------------------------------------------------------- running the installer

/// The argv for an install. Pure, so the two rules that matter can be
/// asserted.
///
/// `--force` is NEVER here, and that is a rule rather than an omission:
/// `bridge.rs`'s `refuse_unverified_wine_for_steam` refuses a Proton prefix
/// whose own runner it could not identify, because running the host's wine
/// against it runs `wineboot -u` and rewrites the game's prefix out from under
/// it. `--force` is the flag that says do it anyway. That is a decision for
/// somebody at a terminal who has read the paragraph, not for a button in a
/// window.
///
/// The first element is the program; the rest are its arguments.
pub(crate) fn install_argv(tobii: &Path, appid: &str, wine: Option<&Path>) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec![
        tobii.as_os_str().to_os_string(),
        "bridge".into(),
        "install".into(),
        "--steam".into(),
        appid.into(),
    ];
    if let Some(w) = wine {
        v.push("--wine".into());
        v.push(w.as_os_str().to_os_string());
    }
    v
}

/// The argv for `tobii bridge status`, which starts nothing and only reads —
/// which is why this window is allowed to run it behind a plain button.
pub(crate) fn status_argv(tobii: &Path, appid: &str) -> Vec<OsString> {
    vec![
        tobii.as_os_str().to_os_string(),
        "bridge".into(),
        "status".into(),
        "--steam".into(),
        appid.into(),
    ]
}

/// What a finished subprocess said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub ok: bool,
    /// [`None`] when the process was killed by a signal rather than exiting.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Judge a finished subprocess. Exit zero and nothing else is success.
pub(crate) fn install_outcome(code: Option<i32>, stdout: &str, stderr: &str) -> Outcome {
    Outcome {
        ok: code == Some(0),
        code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}

/// The installer's result, in the installer's own words.
///
/// The failure text is printed verbatim and is never summarised or reworded.
/// `refuse_unverified_wine_for_steam` writes six lines that name the exact
/// command to run; re-wording them here would produce a second, worse account
/// of a decision this window did not make.
///
/// The success text keeps stderr too. The installer prints `warning:` there on
/// a *successful* install, and swallowing that is how a wine mismatch becomes
/// a silent no-tracking bug.
///
/// What it does not say, on a failure, is that nothing in the prefix was
/// changed. That would be a confident claim about a program this window only
/// watched: the refusals happen before anything is copied, but a failure part
/// way through a copy does not, and this window cannot tell the two apart.
pub(crate) fn outcome_text(o: &Outcome) -> String {
    let mut s = if o.ok {
        "The bridge is installed.".to_string()
    } else if o.code.is_none() {
        "The installer stopped before it could report a result — it was killed, or it \
         crashed. What it had done by then is not something this window can say; press \
         Details to read the prefix as it stands."
            .to_string()
    } else {
        format!(
            "The installer stopped, exit status {}. It said:",
            o.code.expect("some, by the branch above")
        )
    };
    for (lead, text) in [
        ("The installer said:", o.stdout.trim_end()),
        ("The installer also said:", o.stderr.trim_end()),
    ] {
        if text.is_empty() {
            continue;
        }
        s.push_str("\n\n");
        // A failure's text is the answer, not an aside, so it goes in bare.
        if o.ok {
            s.push_str(lead);
            s.push('\n');
        }
        s.push_str(text);
    }
    s
}

/// Whether the installer's refusal is one the user can answer by naming a
/// Proton build.
///
/// The refusal already tells them where to look; the button saves them
/// retyping it.
pub(crate) fn offers_wine_choice(o: &Outcome) -> bool {
    !o.ok && (o.stderr.contains("--wine") || o.stdout.contains("--wine"))
}

/// Where `tobii` is.
///
/// `tobii-gtk` and `tobii` are installed side by side — `scripts/
/// install-payload.sh` copies both into one bindir, and the .deb and .rpm put
/// both in `/usr/bin` — so the binary beside this one is the right answer and
/// `$PATH` is the fallback.
///
/// Both lookups are injected, `exists` included, so this is pure: CI runs as
/// root in a container with no real `$HOME` and no install, and a test that
/// had to create a file to see the first branch taken would be a test that
/// writes to a temporary directory to assert a two-line rule.
///
/// It never answers a bare `"tobii"`. That would be a `$PATH` lookup at spawn
/// time, done by whatever the child's environment turned out to be rather than
/// by this function, and the window would have no way to say beforehand that
/// there is nothing to run.
pub(crate) fn tobii_binary(
    exe: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
    on_path: &dyn Fn(&str) -> Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(beside) = exe.and_then(Path::parent).map(|d| d.join("tobii")) {
        if exists(&beside) {
            return Some(beside);
        }
    }
    on_path("tobii")
}

/// What to say when there is no `tobii` to run.
pub(crate) fn no_binary_text(appid: &str) -> String {
    format!(
        "The command-line program `tobii` is not beside this one and is not on your PATH, so \
         the bridge cannot be installed from here. Run this in a terminal instead:\n\n\
         tobii bridge install --steam {appid}"
    )
}

/// `$PATH`, searched with the same `exists` the caller used for the binary
/// beside this one.
fn on_path(exists: &dyn Fn(&Path) -> bool) -> impl Fn(&str) -> Option<PathBuf> + '_ {
    move |name| {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(name))
            .find(|p| exists(p))
    }
}

// ------------------------------------------------- block 3: the game's files

/// What became of the profile for this game.
///
/// Five cases, and the four that are not [`Self::Ready`] are four different
/// sentences. Rolling any two of them together is the mistake this whole
/// window is built against: *nobody has written one* and *yours could not be
/// read* lead somewhere different.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProfileVerdict {
    /// There is no profile for this game, anywhere. The shipped state.
    None,
    /// There is one, and it asks for a grammar this build does not have.
    TooNew { origin: String, found: u32 },
    /// There is one, and it is not a profile this build can read.
    Malformed { origin: String, why: String },
    /// There is one, and it could not be opened at all.
    Unreadable { origin: String, why: String },
    /// There is one and it was read.
    Ready(Box<profiles::Loaded>),
}

/// Sort what [`profiles::load_from`] answered into the sentence it deserves.
///
/// A profile asking for a newer grammar is [`ProfileVerdict::TooNew`] and not
/// `Malformed`, because the two lead to different advice: update this program,
/// against fix your file. `tobii-config`'s parser carries the version it could
/// not do in `ParseError::unknown_version` for exactly this.
pub(crate) fn profile_verdict(
    r: Result<Option<profiles::Loaded>, profiles::LoadError>,
) -> ProfileVerdict {
    match r {
        Ok(None) => ProfileVerdict::None,
        Ok(Some(loaded)) => ProfileVerdict::Ready(Box::new(loaded)),
        Err(profiles::LoadError::Unreadable { origin, why }) => ProfileVerdict::Unreadable {
            origin: origin.to_string(),
            why,
        },
        Err(profiles::LoadError::Malformed { origin, error }) => match error.unknown_version {
            Some(found) => ProfileVerdict::TooNew {
                origin: origin.to_string(),
                found,
            },
            None => ProfileVerdict::Malformed {
                origin: origin.to_string(),
                why: error.to_string(),
            },
        },
        // The app ids come from Steam's own manifests, so this is unreachable
        // from the picker. It is still worded rather than unwrapped: an
        // unreachable branch that panics is one bad manifest away from taking
        // the hub down.
        Err(e @ profiles::LoadError::NotAnAppId(_)) => ProfileVerdict::Malformed {
            origin: "this game".to_string(),
            why: e.to_string(),
        },
    }
}

/// The paragraphs at the head of block 3, before any check rows.
///
/// Every one of them says the block is read-only, because every one of them is
/// a state somebody might act on.
pub(crate) fn profile_intro(v: &ProfileVerdict, game: &str, profiles_dir: &Path) -> String {
    match v {
        ProfileVerdict::None => format!(
            "This program has no profile for {game}, so it does not know which of this game's \
             own settings to look at, or where they are kept. Nothing here has been checked, \
             and nothing has been guessed.\n\n\
             It never writes a game's own configuration files. A bindings file is hours of \
             somebody's work in a format its author can change in any patch: a wrong read \
             costs a confusing sentence, a wrong write costs the bindings. What a profile \
             buys is the reading — this window telling you which setting is wrong before you \
             go hunting.\n\n\
             Profiles go in {dir}, one file per game, named by app id. This build ships none.",
            dir = profiles_dir.display(),
        ),
        ProfileVerdict::TooNew { origin, found } => format!(
            "{origin} was written for a newer version of this program: it says profile format \
             {found}, and this build reads {ours}.\n\n\
             It has not been used at all — not even the parts that look familiar. Reading a \
             file by the wrong rules is how a setting that is fine gets reported as wrong, \
             and this section exists to not do that. Update this program, or move that file \
             aside.",
            ours = profiles::VERSION,
        ),
        ProfileVerdict::Malformed { origin, why } => format!(
            "{origin} could not be read as a profile: {why}.\n\n\
             None of it has been used. A profile half-read is a profile that reports settings \
             by rules its author did not write, so the whole file is refused and nothing here \
             has been checked. Nothing about the game itself has been touched.",
        ),
        ProfileVerdict::Unreadable { origin, why } => format!(
            "There is a profile at {origin} and it could not be opened: {why}.\n\n\
             That is not the same as there being none, and this window will not pretend it \
             is: nothing here has been checked. There is no fallback to a profile built into \
             this program either — your file outranks that one, and \"outranks\" must not \
             quietly become \"unless I dislike it\".",
        ),
        ProfileVerdict::Ready(loaded) => {
            let mut s = format!("Checked against the profile at {}.", loaded.origin);
            let unknown = loaded.profile.unknown_formats();
            if !unknown.is_empty() {
                s.push_str(&format!(
                    "\n\nIt names {n} file {fmt} this build has no reader for — {list}. Those \
                     checks are listed below as ones this program could not perform; the rest \
                     were performed.",
                    n = unknown.len(),
                    fmt = plural(unknown.len(), "format", "formats"),
                    list = unknown.join(", "),
                ));
            }
            if loaded.profile.checks.is_empty() {
                s.push_str(
                    "\n\nIt names nothing to check in the game's own files, so nothing here \
                     has been read.",
                );
            }
            s
        }
    }
}

/// The last line of block 3, in every state.
pub(crate) const READ_ONLY_FOOTER: &str =
    "Read only. This program has not changed any of these files.";

/// What one check turned out to hold.
///
/// Seven cases, and the negative ones are deliberately not four spellings of
/// one. This is `tobii-gameconf`'s thesis asserted at its consumer: *not set*
/// and *cannot be read* are different answers, and neither may be reported as
/// the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Answer {
    /// There is no prefix, so the game's files are not anywhere to be read.
    NoPrefix,
    /// The profile names a file format this build has no reader for.
    UnknownFormat(String),
    /// [`Source::Unwritten`] — nothing here holds a saved configuration.
    Unwritten(String),
    /// [`Source::Rejected`] — something is here and could not be read.
    Unreadable(String),
    /// Read, and the setting is not in it.
    Absent,
    /// Read, and the value could not be read back exactly.
    NotReadBack(String),
    /// Read, and here is what it says.
    Text(String),
}

/// One line of block 3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    /// What the profile calls the setting.
    pub setting: String,
    /// The preset this answer is about, for the format that has presets.
    pub preset: Option<String>,
    pub answer: Answer,
    /// What the profile says the game wants.
    pub wants: String,
    /// What to tell somebody who has to change it by hand.
    pub tell: String,
}

/// Whether a value is the one the profile asked for.
///
/// Three answers, not two. `tobii-config`'s `Check` deliberately offers no
/// `matches()` helper and says why: whether either game's own reader is
/// case-sensitive is not something this project has measured, so a two-way
/// answer would have to guess, and both guesses are wrong somewhere. The
/// caller owns the comparison rule, and this is it — an exact match is a
/// match, a match but for case is reported as exactly that, and the user
/// decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Matches,
    OnlyCase,
    Differs,
}

fn verdict(got: &str, wants: &str) -> Verdict {
    if got == wants {
        Verdict::Matches
    } else if got.eq_ignore_ascii_case(wants) {
        Verdict::OnlyCase
    } else {
        Verdict::Differs
    }
}

/// One check row, as a sentence.
pub(crate) fn row_text(r: &Row) -> String {
    let at = match &r.preset {
        Some(p) => format!(", in preset “{p}”"),
        None => String::new(),
    };
    match &r.answer {
        Answer::NoPrefix => format!(
            "{}: not checked — this game has no Proton prefix here yet, so its own settings \
             have nowhere to be.",
            r.setting
        ),
        Answer::UnknownFormat(fmt) => format!(
            "{}: not checked — this build has no reader for the file format “{fmt}”.",
            r.setting
        ),
        // The crate's own sentence, printed rather than reworded. It is the
        // one thing that knows which of the several ways of being absent this
        // was.
        Answer::Unwritten(why) => format!("{}: not found — {why}.", r.setting),
        Answer::Unreadable(why) => format!("{}: could not be read — {why}.", r.setting),
        Answer::Absent => format!("{}: not set{at}.", r.setting),
        Answer::NotReadBack(why) => {
            format!("{}: could not be read back exactly{at} — {why}.", r.setting)
        }
        Answer::Text(v) => {
            let head = format!("{}: is {v}{at}", r.setting);
            match verdict(v, &r.wants) {
                Verdict::Matches => format!("{head} — which is what this program expects."),
                Verdict::OnlyCase => format!(
                    "{head} — this program expects {wants}, which differs only in case. \
                     Whether this game's own reader cares is not something this program has \
                     measured.",
                    wants = r.wants
                ),
                Verdict::Differs => {
                    let mut s = format!("{head} — this program expects {}.", r.wants);
                    if !r.tell.trim().is_empty() {
                        s.push(' ');
                        s.push_str(r.tell.trim());
                    }
                    s
                }
            }
        }
    }
}

/// What a `binds` directory said about itself, beyond the setting asked for.
///
/// Both fields are shown because they are the one assumption that module could
/// not measure: a game that has been upgraded leaves the older schema's start
/// file next to the new one, and the reader takes the highest number as the
/// current one — which is wrong for somebody who has rolled their game back.
/// Nothing else in this window would say so.
pub(crate) fn binds_note(b: &binds::Bindings) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(n) = b.schema {
        parts.push(format!("read through the bindings schema {n} start file"));
    }
    if !b.superseded.is_empty() {
        parts.push(format!(
            "{n} older start {file} next to it were not read — {list}. If this game has been \
             rolled back, one of those is the live one and the reading above is of the wrong \
             file",
            n = b.superseded.len(),
            file = plural(b.superseded.len(), "file", "files"),
            list = list_paths(&b.superseded),
        ));
    }
    (!parts.is_empty()).then(|| {
        let mut s = parts.join("; ");
        s.push('.');
        s[..1].to_uppercase() + &s[1..]
    })
}

/// Turn one [`Lookup`] into an [`Answer`].
fn from_lookup(l: &Lookup) -> Answer {
    match l {
        Lookup::Absent => Answer::Absent,
        Lookup::Text(v) => Answer::Text(v.clone()),
        Lookup::Rejected(why) => Answer::NotReadBack(why.clone()),
    }
}

/// Run one check against a prefix.
///
/// The only place in this window that hands a path to `tobii-gameconf`, and
/// the only place that reads a game's own files at all. The path comes from
/// [`profiles::Check::path_under`], which can only ever land inside the prefix
/// because the parser refused `..`, a backslash and a leading `/` when the
/// file was read.
fn run_check(check: &profiles::Check, prefix: Option<&Path>) -> (Vec<Row>, Option<String>) {
    let row = |preset: Option<String>, answer: Answer| Row {
        setting: check.setting.clone(),
        preset,
        answer,
        wants: check.wants.clone(),
        tell: check.tell.clone(),
    };
    let Some(prefix) = prefix else {
        return (vec![row(None, Answer::NoPrefix)], None);
    };
    let path = check.path_under(prefix);
    match &check.format {
        profiles::Format::Unknown(fmt) => {
            (vec![row(None, Answer::UnknownFormat(fmt.clone()))], None)
        }
        profiles::Format::AttributesXml => {
            let answer = match attrs::read(&path, &check.setting) {
                Source::Unwritten(why) => Answer::Unwritten(why),
                Source::Rejected(why) => Answer::Unreadable(why),
                Source::Read(a) => from_lookup(&a.value),
            };
            (vec![row(None, answer)], None)
        }
        profiles::Format::BindsDir => match binds::read(&path, &check.setting) {
            Source::Unwritten(why) => (vec![row(None, Answer::Unwritten(why))], None),
            Source::Rejected(why) => (vec![row(None, Answer::Unreadable(why))], None),
            Source::Read(b) => {
                let note = binds_note(&b);
                if b.presets.is_empty() {
                    return (
                        vec![row(
                            None,
                            Answer::Unwritten(format!("{} names no preset", b.start.display())),
                        )],
                        note,
                    );
                }
                let rows = b
                    .presets
                    .iter()
                    .map(|active| {
                        let answer = match &active.found {
                            binds::Found::Absent(why) => Answer::Unwritten(why.clone()),
                            binds::Found::Rejected(why) => Answer::Unreadable(why.clone()),
                            binds::Found::Read { preset, .. } => from_lookup(&preset.setting),
                        };
                        row(Some(active.name.clone()), answer)
                    })
                    .collect();
                (rows, note)
            }
        },
    }
}

// ----------------------------------------------------------------- the window

thread_local! {
    /// The game-setup window, while one is open.
    ///
    /// A **weak** reference, which is the v0.3.1 rule written as code: a strong
    /// one here would keep the window alive after it was closed, and with it
    /// everything it holds. Nothing else in this module holds the window
    /// either — the Close button finds it from itself at click time
    /// ([`crate::close_on_click`]), the Esc controller holds a weak ref
    /// ([`crate::add_escape_to_close`]), and Back finds the stack as a weak
    /// reference to a descendant rather than walking up to the window.
    static OPEN: RefCell<glib::WeakRef<gtk::Window>> = RefCell::new(glib::WeakRef::new());
}

/// What the pick page says above the list.
const LEAD: &str = "Everything Steam says is installed on this machine. Pick one and this \
                    window will show the three things that have to be configured for it, and \
                    do the two it can.";

/// One numbered section of the game page: a title and a body.
///
/// The body is `selectable(true)` for the reason [`crate::help`]'s `card`
/// gives: a selectable `GtkLabel` is focusable, so Tab reaches it, a screen
/// reader reads it, and a prefix path or an installer's refusal can be copied
/// into a bug report. It is also what lets this whole window have no
/// tooltip-only fact in it, which is the rule the hub's cards had to learn the
/// hard way.
///
/// The actions, where a block has any, are appended by the caller into a box
/// of their own — and block 3 has none, on purpose. See [`open_with`].
fn block(n: u32, title: &str) -> (gtk::Box, Label) {
    let b = gtk::Box::new(Orientation::Vertical, 8);
    let t = Label::new(Some(&format!("{n}. {title}")));
    t.set_halign(Align::Start);
    t.set_xalign(0.0);
    t.add_css_class("section-title");
    let body = Label::new(None);
    body.set_halign(Align::Start);
    body.set_xalign(0.0);
    body.set_wrap(true);
    body.set_max_width_chars(58);
    body.set_selectable(true);
    b.append(&t);
    b.append(&body);
    (b, body)
}

/// The divider between two blocks.
///
/// Its own two lines rather than `lib.rs`'s `hairline`, which is private
/// there: the class is what matters, and `.hairline` states a minimum height
/// so an empty box is all it needs.
fn divider() -> gtk::Box {
    let h = gtk::Box::new(Orientation::Horizontal, 0);
    h.add_css_class("hairline");
    h
}

/// A paragraph of body text: left-aligned, wrapped to the same measure the
/// blocks use, and selectable for the reason [`block`] gives.
fn wrapped(text: &str) -> Label {
    let l = Label::new(Some(text));
    l.set_halign(Align::Start);
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_max_width_chars(58);
    l.set_selectable(true);
    l
}

/// The same, quieter: a line of block 3, a census, or an installer's report.
fn small(text: &str) -> Label {
    let l = wrapped(text);
    l.add_css_class("section-desc");
    l
}

/// An argv as a line somebody could retype.
fn pretty(argv: &[OsString]) -> String {
    argv.iter()
        .map(|a| {
            let s = a.to_string_lossy();
            if s.contains(' ') {
                format!("\"{s}\"")
            } else {
                s.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Run one subprocess on a worker thread and poll for its result.
///
/// The poll is `glib::timeout_add_local` at 150 ms, which is
/// `head_model::start_download`'s shape, with the `alive` guard from
/// `head_model::pitch_dialog` — whose comment records the measurement: without
/// it a cancelled run "left a 5 Hz timeout running for the life of the process,
/// holding every widget in this window alive with it".
///
/// **Closing the window mid-install does not kill the child.** The poll stops;
/// the process finishes. `bridge.rs`'s install stages each DLL beside its
/// target and renames it, exactly so a half-finished copy cannot be loaded, and
/// killing it between the copy and the registry write is the one way to leave a
/// prefix inconsistent.
fn start_job(
    argv: Vec<OsString>,
    alive: &Rc<Cell<bool>>,
    running: &Rc<RefCell<Option<String>>>,
    done: impl Fn(Outcome) + 'static,
) {
    if running.borrow().is_some() {
        return;
    }
    let line = pretty(&argv);
    *running.borrow_mut() = Some(line.clone());

    let (tx, rx) = std::sync::mpsc::channel();
    let spawn = argv.clone();
    std::thread::spawn(move || {
        let out = std::process::Command::new(&spawn[0])
            .args(&spawn[1..])
            .output();
        let _ = tx.send(out);
    });

    let alive = alive.clone();
    let running = running.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || {
        // First line, before anything is touched: the window may be gone.
        if !alive.get() {
            return glib::ControlFlow::Break;
        }
        let finish = |o: Outcome| {
            *running.borrow_mut() = None;
            done(o);
            glib::ControlFlow::Break
        };
        match rx.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Ok(Ok(out)) => finish(install_outcome(
                out.status.code(),
                &String::from_utf8_lossy(&out.stdout),
                &String::from_utf8_lossy(&out.stderr),
            )),
            Ok(Err(e)) => finish(install_outcome(
                None,
                "",
                &format!("{line} could not be run: {e}"),
            )),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => finish(install_outcome(
                None,
                "",
                "the thread running the installer ended without a result",
            )),
        }
    });
}

/// Open the game-setup window, or raise the one that is already open.
///
/// Reads `$HOME` and this user's profile directory, and hands both to
/// [`open_with`]. Nothing in CI calls this.
pub fn open(app: &Application, parent: &impl IsA<gtk::Window>) -> gtk::Window {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let dir = profiles::profiles_dir();
    open_with(app, parent, scan(&home, &dir))
}

/// Close the window if one is open.
///
/// Called when the hub hides itself to the tray. `destroy_with_parent` does not
/// cover *hiding* a parent, and a modal transient whose parent is hidden is
/// left to the compositor — on some it floats alone on an empty desktop with
/// nothing behind it to be modal over.
pub(crate) fn close() {
    if let Some(win) = OPEN.with(|c| c.borrow().upgrade()) {
        win.close();
    }
}

/// The window, over a scan that was handed in.
///
/// Modal and transient for the hub. Modal because the one control it writes —
/// game output and its destination — is also on the card behind it, and two
/// live editors of one setting side by side is a race the user can see.
///
/// It takes **no claim on the tracker**: [`crate::hold_while_open`] is not
/// called, no `DemandGuard` is taken, and this window is not in
/// `device::EXCLUSIVE` and is not a candidate for it. See the module docs.
pub fn open_with(app: &Application, parent: &impl IsA<gtk::Window>, scanned: Scan) -> gtk::Window {
    if let Some(win) = OPEN.with(|c| c.borrow().upgrade()) {
        win.present();
        return win;
    }
    let scan = Rc::new(scanned);

    // Which games have a prefix, answered once. `tobii_steam::prefix` re-reads
    // `libraryfolders.vdf` on every call, and the pick page asks this question
    // of every row on every keystroke.
    let with_prefix: Rc<std::collections::HashSet<String>> = Rc::new(
        scan.apps
            .iter()
            .filter(|a| tobii_steam::prefix(&scan.home, &a.appid).is_some())
            .map(|a| a.appid.clone())
            .collect(),
    );

    // ---- the pick page

    let pick = gtk::Box::new(Orientation::Vertical, 10);
    let pick_head = Label::new(Some("Which game?"));
    pick_head.set_halign(Align::Start);
    pick_head.set_xalign(0.0);
    pick_head.add_css_class("dialog-heading");
    let lead = Label::new(Some(LEAD));
    lead.set_halign(Align::Start);
    lead.set_xalign(0.0);
    lead.set_wrap(true);
    lead.set_max_width_chars(58);
    lead.add_css_class("dialog-lead");

    let search = gtk::SearchEntry::new();
    search.set_widget_name(SEARCH_NAME);
    search.add_css_class("topic-search");
    search.set_placeholder_text(Some("Search by title or app id"));

    let list = gtk::ListBox::new();
    list.set_widget_name(LIST_NAME);
    list.add_css_class("topic-list");
    list.set_selection_mode(gtk::SelectionMode::Single);
    let placeholder = wrapped("");
    list.set_placeholder(Some(&placeholder));
    let list_scroll = gtk::ScrolledWindow::new();
    list_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    list_scroll.set_vexpand(true);
    list_scroll.set_child(Some(&list));

    let census = small("");
    pick.append(&pick_head);
    pick.append(&lead);
    pick.append(&search);
    pick.append(&list_scroll);
    pick.append(&census);

    // ---- the nothing page

    let nothing = gtk::Box::new(Orientation::Vertical, 10);
    let nothing_head = Label::new(Some("No games found"));
    nothing_head.set_halign(Align::Start);
    nothing_head.set_xalign(0.0);
    nothing_head.add_css_class("dialog-heading");
    let nothing_body = wrapped(&nothing_text(&scan.missing));
    nothing.append(&nothing_head);
    nothing.append(&nothing_body);

    // ---- the game page

    let game = gtk::Box::new(Orientation::Vertical, 14);
    let g_title = Label::new(None);
    g_title.set_halign(Align::Start);
    g_title.set_xalign(0.0);
    g_title.set_wrap(true);
    g_title.add_css_class("dialog-heading");
    let g_sub = small("");
    game.append(&g_title);
    game.append(&g_sub);

    let (b1, s_body) = block(1, "These settings");
    let s_actions = gtk::Box::new(Orientation::Horizontal, 10);
    s_actions.set_halign(Align::Start);
    b1.append(&s_actions);
    game.append(&b1);
    game.append(&divider());

    let (b2, br_body) = block(2, "The Wine bridge");
    let b_actions = gtk::Box::new(Orientation::Horizontal, 10);
    b_actions.set_halign(Align::Start);
    b2.append(&b_actions);
    let b_report = small("");
    b2.append(&b_report);
    game.append(&b2);
    game.append(&divider());

    // No actions box, and that is the decision rather than an oversight: this
    // program never writes a game's own configuration files, so block 3 has
    // nothing to press. See the module docs.
    let (b3, p_body) = block(3, "The game's own settings");
    let rows_box = gtk::Box::new(Orientation::Vertical, 6);
    b3.append(&rows_box);
    b3.append(&small(READ_ONLY_FOOTER));
    game.append(&b3);

    let game_scroll = gtk::ScrolledWindow::new();
    game_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    game_scroll.set_vexpand(true);
    game_scroll.set_child(Some(&game));

    // ---- the stack and the footer

    let stack = gtk::Stack::new();
    stack.set_widget_name(STACK_NAME);
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
    stack.set_vexpand(true);
    stack.add_named(&pick, Some(PAGE_PICK));
    stack.add_named(&game_scroll, Some(PAGE_GAME));
    stack.add_named(&nothing, Some(PAGE_NOTHING));

    let back = crate::widget::button("Back");
    let close_btn = crate::widget::button("Close");
    crate::close_on_click(&close_btn);
    let footer = gtk::Box::new(Orientation::Horizontal, 10);
    footer.set_halign(Align::End);
    footer.set_margin_top(4);
    footer.append(&back);
    footer.append(&close_btn);

    let content = gtk::Box::new(Orientation::Vertical, 12);
    content.set_margin_top(24);
    content.set_margin_bottom(20);
    content.set_margin_start(26);
    content.set_margin_end(26);
    content.append(&stack);
    content.append(&footer);

    // ---- the buttons, built once
    //
    // Once, and never inside `refresh`, because `refresh` is what their
    // handlers call: a button created by `refresh` would be held by a box that
    // `refresh` holds, and that is the cycle. Built here and only *described*
    // by `refresh`, which reaches them through `downgrade()`.
    let settings_btn = crate::widget::button("Turn on and send to a virtual joystick");
    s_actions.append(&settings_btn);
    let install_btn = crate::widget::button(Action::Install.caption());
    let details_btn = crate::widget::button("Details");
    details_btn.add_css_class("quiet");
    details_btn.set_tooltip_text(Some(
        "Run `tobii bridge status` for this game and show what it prints. It reads the \
         prefix and starts nothing.",
    ));
    let wine_btn = crate::widget::button("Choose the Proton build…");
    b_actions.append(&install_btn);
    b_actions.append(&details_btn);
    b_actions.append(&wine_btn);

    // ---- the state the page draws itself from

    let sel: Rc<RefCell<Option<App>>> = Rc::default();
    let running: Rc<RefCell<Option<String>>> = Rc::default();
    let outcome: Rc<RefCell<Option<Outcome>>> = Rc::default();
    let report: Rc<RefCell<Option<String>>> = Rc::default();
    let wrote: Rc<RefCell<Option<String>>> = Rc::default();
    let tobii: Rc<Option<PathBuf>> = Rc::new({
        let exists = |p: &Path| p.is_file();
        let path_lookup = on_path(&exists);
        tobii_binary(
            std::env::current_exe().ok().as_deref(),
            &exists,
            &path_lookup,
        )
    });

    // ---- refresh
    //
    // It holds labels and plain boxes strongly, and **every widget that carries
    // a handler through `downgrade()`**. A button's handler holds this closure;
    // if this closure held the button, or the box the button sits in, that
    // would be a cycle between two siblings, which GTK never breaks and which a
    // test that weak-refs only the window cannot see.
    let refresh: Rc<dyn Fn()> = {
        let (scan, sel, running, outcome, report, wrote, tobii) = (
            scan.clone(),
            sel.clone(),
            running.clone(),
            outcome.clone(),
            report.clone(),
            wrote.clone(),
            tobii.clone(),
        );
        let (g_title, g_sub, s_body, br_body, b_report, p_body, rows_box) = (
            g_title.clone(),
            g_sub.clone(),
            s_body.clone(),
            br_body.clone(),
            b_report.clone(),
            p_body.clone(),
            rows_box.clone(),
        );
        let (s_actions, b_actions) = (s_actions.downgrade(), b_actions.downgrade());
        let (settings_w, install_w, details_w, wine_w) = (
            settings_btn.downgrade(),
            install_btn.downgrade(),
            details_btn.downgrade(),
            wine_btn.downgrade(),
        );
        Rc::new(move || {
            let Some(app) = sel.borrow().clone() else {
                return;
            };
            let busy = running.borrow().clone();
            g_title.set_text(&app.name);
            g_sub.set_text(&format!("app id {}", app.appid));

            // Read before the blocks are worded, because two of them have
            // something to say about what it holds. A profile that named
            // settings, or that answered the bridge question, and was then
            // silently ignored would be this window telling somebody nothing
            // needs doing while something does.
            let verdict = profile_verdict(profiles::load_from(
                &scan.profiles_dir,
                profiles::BUILTIN,
                &app.appid,
            ));
            let profile = match &verdict {
                ProfileVerdict::Ready(loaded) => Some(&loaded.profile),
                _ => None,
            };

            // --- block 1
            let cfg = tobii_output::games::load_output_config();
            let (mut text, caption) = settings_block(&cfg);
            if let Some(note) = profile.and_then(|p| profile_settings_note(&p.settings)) {
                text.push_str("\n\n");
                text.push_str(&note);
            }
            if let Some(w) = wrote.borrow().as_ref() {
                text.push_str("\n\n");
                text.push_str(w);
            }
            s_body.set_text(&text);
            if let Some(btn) = settings_w.upgrade() {
                match caption {
                    Some(c) => {
                        crate::widget::set_button_text(&btn, c);
                        btn.set_visible(true);
                    }
                    None => btn.set_visible(false),
                }
            }
            if let Some(b) = s_actions.upgrade() {
                // An insensitive ancestor makes its children insensitive, which
                // is how this greys a button out without holding one.
                b.set_sensitive(busy.is_none());
            }

            // --- block 2
            let state = bridge_state(&scan.home, &app.appid);
            let (mut text, action) = bridge_block(&state, &scan.missing);
            if let Some(note) = profile.and_then(|p| profile_bridge_note(p.bridge)) {
                text.push_str("\n\n");
                text.push_str(&note);
            }
            let runnable = tobii.is_some();
            if let Some(cmd) = &busy {
                text.push_str(&format!(
                    "\n\nRunning now:\n{cmd}\n\n\
                     Closing this window does not stop it — this window stops watching and \
                     the installer finishes. It copies each file beside its target and \
                     renames it, so a half-finished copy cannot be loaded; stopping it \
                     between the copy and the registry write is the one way to leave a \
                     prefix inconsistent."
                ));
            } else if let Some(o) = outcome.borrow().as_ref() {
                text.push_str("\n\n");
                text.push_str(&outcome_text(o));
            }
            if action.is_some() && !runnable {
                text.push_str("\n\n");
                text.push_str(&no_binary_text(&app.appid));
            }
            br_body.set_text(&text);
            if let Some(btn) = install_w.upgrade() {
                match (action, runnable) {
                    (Some(a), true) => {
                        crate::widget::set_button_text(&btn, a.caption());
                        btn.set_visible(true);
                    }
                    _ => btn.set_visible(false),
                }
            }
            if let Some(btn) = details_w.upgrade() {
                btn.set_visible(runnable && !matches!(state, BridgeState::NoPrefix));
            }
            if let Some(btn) = wine_w.upgrade() {
                let offer = outcome.borrow().as_ref().is_some_and(offers_wine_choice);
                btn.set_visible(runnable && offer);
            }
            if let Some(b) = b_actions.upgrade() {
                b.set_sensitive(busy.is_none());
            }
            match report.borrow().as_ref() {
                Some(r) => {
                    b_report.set_text(r);
                    b_report.set_visible(true);
                }
                None => b_report.set_visible(false),
            }

            // --- block 3
            p_body.set_text(&profile_intro(&verdict, &app.name, &scan.profiles_dir));
            while let Some(c) = rows_box.first_child() {
                rows_box.remove(&c);
            }
            if let Some(loaded) = profile {
                let prefix = match &state {
                    BridgeState::NoPrefix => None,
                    BridgeState::Absent { prefix } => Some(prefix.as_path()),
                    BridgeState::Files { prefix, .. } => Some(prefix.as_path()),
                };
                for check in &loaded.checks {
                    let (rows, note) = run_check(check, prefix);
                    for r in &rows {
                        rows_box.append(&small(&row_text(r)));
                    }
                    if let Some(n) = note {
                        rows_box.append(&small(&n));
                    }
                }
            }
        })
    };

    // ---- the handlers

    {
        let (refresh, wrote) = (refresh.clone(), wrote.clone());
        settings_btn.connect_clicked(move |_| {
            // Through `games::edit_config`, which re-reads the file first:
            // three editors now share `games.toml` — this window, the hub's
            // card, and `tobii games set` — and a held copy makes whichever
            // was touched second clobber the other.
            crate::games::edit_config("game-output settings", |cfg| {
                cfg.enabled = true;
                cfg.joystick = true;
            });
            *wrote.borrow_mut() = Some(wrote_line(&tobii_output::games::games_path()));
            refresh();
        });
    }

    // This window's lifetime, as one flag. Every poll reads it on its first
    // line and stops when it is false; `connect_close_request` sets it.
    let alive: Rc<Cell<bool>> = Rc::new(Cell::new(true));

    {
        let (refresh, sel, tobii, running, outcome, alive) = (
            refresh.clone(),
            sel.clone(),
            tobii.clone(),
            running.clone(),
            outcome.clone(),
            alive.clone(),
        );
        install_btn.connect_clicked(move |_| {
            let (Some(t), Some(app)) = (tobii.as_ref(), sel.borrow().clone()) else {
                return;
            };
            *outcome.borrow_mut() = None;
            let argv = install_argv(t, &app.appid, None);
            let (refresh2, outcome2) = (refresh.clone(), outcome.clone());
            start_job(argv, &alive, &running, move |o| {
                *outcome2.borrow_mut() = Some(o);
                refresh2();
            });
            refresh();
        });
    }

    {
        let (refresh, sel, tobii, running, report, alive) = (
            refresh.clone(),
            sel.clone(),
            tobii.clone(),
            running.clone(),
            report.clone(),
            alive.clone(),
        );
        details_btn.connect_clicked(move |_| {
            let (Some(t), Some(app)) = (tobii.as_ref(), sel.borrow().clone()) else {
                return;
            };
            let argv = status_argv(t, &app.appid);
            let (refresh2, report2) = (refresh.clone(), report.clone());
            start_job(argv, &alive, &running, move |o| {
                let mut s = o.stdout.trim_end().to_string();
                if !o.stderr.trim().is_empty() {
                    if !s.is_empty() {
                        s.push_str("\n\n");
                    }
                    s.push_str(o.stderr.trim_end());
                }
                *report2.borrow_mut() = Some(s);
                refresh2();
            });
            refresh();
        });
    }

    {
        let (refresh, sel, tobii, running, outcome, alive) = (
            refresh.clone(),
            sel.clone(),
            tobii.clone(),
            running.clone(),
            outcome.clone(),
            alive.clone(),
        );
        wine_btn.connect_clicked(move |b| {
            let (Some(t), Some(app)) = (tobii.as_ref(), sel.borrow().clone()) else {
                return;
            };
            let parent = b.root().and_downcast::<gtk::Window>();
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Choose the Proton build's wine");
            let (t, refresh, running, outcome, alive) = (
                t.clone(),
                refresh.clone(),
                running.clone(),
                outcome.clone(),
                alive.clone(),
            );
            dialog.open(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                let Some(path) = res.ok().and_then(|f| f.path()) else {
                    return;
                };
                *outcome.borrow_mut() = None;
                let argv = install_argv(&t, &app.appid, Some(&path));
                let (refresh2, outcome2) = (refresh.clone(), outcome.clone());
                start_job(argv, &alive, &running, move |o| {
                    *outcome2.borrow_mut() = Some(o);
                    refresh2();
                });
                refresh();
            });
        });
    }

    // ---- the pick list

    let ids: Rc<RefCell<Vec<String>>> = Rc::default();
    let rebuild: Rc<dyn Fn(&str)> = {
        let (scan, with_prefix, ids) = (scan.clone(), with_prefix.clone(), ids.clone());
        let list_w = list.downgrade();
        let (placeholder, census) = (placeholder.clone(), census.clone());
        Rc::new(move |q: &str| {
            let p = picker(&scan.apps, &scan.missing, &|id| with_prefix.contains(id), q);
            *ids.borrow_mut() = p.rows.iter().map(|r| r.appid.clone()).collect();
            if let Some(list) = list_w.upgrade() {
                while let Some(c) = list.first_child() {
                    list.remove(&c);
                }
                for r in &p.rows {
                    let b = gtk::Box::new(Orientation::Vertical, 2);
                    let n = Label::new(Some(&r.name));
                    n.set_halign(Align::Start);
                    n.set_xalign(0.0);
                    let s = Label::new(Some(&r.subtitle));
                    s.set_halign(Align::Start);
                    s.set_xalign(0.0);
                    s.add_css_class("section-desc");
                    b.append(&n);
                    b.append(&s);
                    list.append(&b);
                }
            }
            placeholder.set_text(p.no_match.as_deref().unwrap_or(""));
            census.set_text(&p.census);
            if p.census_warn {
                census.add_css_class("section-warn");
            } else {
                census.remove_css_class("section-warn");
            }
        })
    };
    rebuild("");
    {
        let rebuild = rebuild.clone();
        search.connect_search_changed(move |e| rebuild(&e.text()));
    }

    {
        let (refresh, sel, scan, ids, wrote, outcome, report) = (
            refresh.clone(),
            sel.clone(),
            scan.clone(),
            ids.clone(),
            wrote.clone(),
            outcome.clone(),
            report.clone(),
        );
        // Weak, all three: `back` and `search` each carry handlers of their
        // own, and `search`'s reaches this very list — so a strong reference
        // here would close a sibling cycle that nothing ever breaks.
        let (stack_w, back_w, game_scroll) =
            (stack.downgrade(), back.downgrade(), game_scroll.clone());
        list.connect_row_activated(move |_, row| {
            let Some(id) = ids.borrow().get(row.index().max(0) as usize).cloned() else {
                return;
            };
            let Some(app) = scan.apps.iter().find(|a| a.appid == id).cloned() else {
                return;
            };
            *sel.borrow_mut() = Some(app);
            // A different game's answers are not this game's.
            *wrote.borrow_mut() = None;
            *outcome.borrow_mut() = None;
            *report.borrow_mut() = None;
            refresh();
            if let (Some(stack), Some(back)) = (stack_w.upgrade(), back_w.upgrade()) {
                back.set_sensitive(true);
                // Focus leaves the outgoing page BEFORE it folds away, onto a
                // widget that is on neither page. `help.rs` records the
                // measurement: a pane folded away with the focus inside it
                // permanently holds its subtree, and a stack page is a pane.
                back.grab_focus();
                stack.set_visible_child_name(PAGE_GAME);
                game_scroll.grab_focus();
            }
        });
    }

    {
        let (stack_w, search_w, close_w) =
            (stack.downgrade(), search.downgrade(), close_btn.downgrade());
        back.connect_clicked(move |b| {
            let (Some(stack), Some(search)) = (stack_w.upgrade(), search_w.upgrade()) else {
                return;
            };
            if let Some(c) = close_w.upgrade() {
                c.grab_focus();
            }
            stack.set_visible_child_name(PAGE_PICK);
            search.grab_focus();
            b.set_sensitive(false);
        });
    }

    // ---- the window

    let win = gtk::Window::builder()
        .application(app)
        .transient_for(parent.as_ref())
        .modal(true)
        .destroy_with_parent(true)
        .title(TITLE)
        .default_width(620)
        .default_height(560)
        .resizable(true)
        .child(&content)
        .build();

    {
        let alive = alive.clone();
        win.connect_close_request(move |_| {
            // The poll's first line reads this. `connect_destroy` would be too
            // late and is not a teardown hook — see `crate::hold_while_open`.
            alive.set(false);
            // And the registry, here rather than left to the weak reference
            // going null on its own. A `WeakRef` reports the object, not the
            // window: GTK destroys a closed window's widgetry and drops it
            // from the application, but the GObject itself lives until the
            // last reference goes, and any of them can outlast the close by a
            // main-loop turn or two. Upgrading one of those and calling
            // `present` on it gives "A window is shown after it has been
            // destroyed", and the user gets nothing — measured, in
            // `tests/game_setup_window.rs`, where the second open found the
            // first window and showed a corpse.
            OPEN.with(|c| *c.borrow_mut() = glib::WeakRef::new());
            glib::Propagation::Proceed
        });
    }
    crate::add_escape_to_close(&win);

    let empty = scan.apps.is_empty();
    stack.set_visible_child_name(if empty { PAGE_NOTHING } else { PAGE_PICK });
    back.set_sensitive(false);

    win.present();
    if empty {
        close_btn.grab_focus();
    } else {
        search.grab_focus();
    }
    // A selectable GtkLabel selects all of its text the moment focus reaches
    // it, and focus can pass through one on its way somewhere else — so the
    // window opened with a paragraph as a solid block of highlight. Taking the
    // focus away does not clear it; this does. The bug `help.rs` records.
    for body in [&s_body, &br_body, &p_body, &b_report, &g_sub, &nothing_body] {
        body.select_region(0, 0);
    }

    OPEN.with(|c| *c.borrow_mut() = win.downgrade());
    win
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing in here reads `$HOME`, `~/.steam` or a real prefix, and nothing
    /// builds a widget: this whole module runs in CI, which has no display and
    /// no Steam install.
    fn app(appid: &str, name: &str) -> App {
        App {
            appid: appid.to_string(),
            name: name.to_string(),
            buildid: None,
        }
    }

    /// An `OutputConfig` with all four things this window reads spelled out.
    /// The defaults are deliberately not inherited: `joystick` is ON in them,
    /// which would make half of these cases unreachable.
    fn cfg(enabled: bool, joystick: bool, opentrack: Option<&str>) -> OutputConfig {
        OutputConfig {
            enabled,
            joystick,
            opentrack: opentrack.map(str::to_string),
            bridge_port: None,
            ..OutputConfig::default()
        }
    }

    fn none(_: &str) -> bool {
        false
    }

    // ------------------------------------------------------------ the picker

    /// The moment a user has typed the name of a game they own and got nothing
    /// back is the moment a silent list becomes a lie. The census line alone is
    /// not enough: it is above the fold and it is not what they are reading.
    #[test]
    fn a_search_that_finds_nothing_still_names_the_library_that_is_not_here() {
        let apps = [app("1", "Something Else")];
        let missing = [PathBuf::from("/mnt/games2")];
        let p = picker(&apps, &missing, &none, "Elite");
        let text = p
            .no_match
            .expect("nothing matched, so there is a no-match page");
        assert!(
            text.contains("/mnt/games2"),
            "the absent library has to be named where the user is looking: {text}"
        );
        assert!(
            text.contains("cannot rule out"),
            "and worded as something unruled-out rather than as a fact: {text}"
        );
    }

    /// The other half, without which the test above is satisfied by appending
    /// the sentence unconditionally — which would invent a library nobody has.
    #[test]
    fn a_search_that_finds_nothing_with_every_library_present_invents_none() {
        let apps = [app("1", "Something Else")];
        let p = picker(&apps, &[], &none, "Elite");
        let text = p.no_match.expect("nothing matched");
        assert!(!text.contains("cannot rule out"), "{text}");
        assert!(!text.contains("not on this machine"), "{text}");
        assert!(
            !p.census_warn,
            "no library is missing, so nothing to warn about"
        );
    }

    /// `looks_like_tool` is a heuristic and says so, so the list sorts by it
    /// and never filters by it — and `Protonaut`, a real game, is the
    /// regression that heuristic already has.
    #[test]
    fn a_tool_is_sorted_last_and_is_still_in_the_list() {
        let apps = [
            app("1", "Proton 9.0"),
            app("2", "Protonaut"),
            app("3", "Elite Dangerous"),
        ];
        let p = picker(&apps, &[], &none, "");
        let names: Vec<&str> = p.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["Elite Dangerous", "Protonaut", "Proton 9.0"],
            "the tool goes last and the game whose name starts with one does not"
        );
    }

    /// The rule `resolve` documents for the name, widened to the app id,
    /// because a search box narrows as it is typed and must not be able to hide
    /// a row that the same text at a terminal would have found.
    #[test]
    fn the_search_is_a_case_insensitive_substring_and_an_empty_box_is_not_a_search() {
        let apps = [app("359320", "Elite Dangerous"), app("220", "Half-Life 2")];
        let hits = |q: &str| -> Vec<String> {
            picker(&apps, &[], &none, q)
                .rows
                .iter()
                .map(|r| r.appid.clone())
                .collect()
        };
        assert_eq!(hits("").len(), 2, "an empty box must match everything");
        assert_eq!(hits("   ").len(), 2, "and so must whitespace");
        assert_eq!(hits("elite"), ["359320"], "case-insensitive on the name");
        assert_eq!(hits("DANGER"), ["359320"], "and anywhere inside it");
        assert_eq!(hits("359320"), ["359320"], "and the app id is reachable");
        assert!(hits("nothing at all").is_empty());
    }

    /// A game that has never been launched has no prefix, and the row says so
    /// where somebody is looking when they wonder why there is nothing to
    /// install into.
    #[test]
    fn a_row_says_whether_this_game_has_a_prefix_yet() {
        let apps = [app("1", "Launched"), app("2", "Never Launched")];
        let p = picker(&apps, &[], &|id| id == "1", "");
        let by_name = |n: &str| {
            p.rows
                .iter()
                .find(|r| r.name == n)
                .unwrap_or_else(|| panic!("no row {n}"))
                .subtitle
                .clone()
        };
        assert!(
            !by_name("Launched").contains("prefix"),
            "{}",
            by_name("Launched")
        );
        assert!(
            by_name("Never Launched").contains("no Proton prefix yet"),
            "{}",
            by_name("Never Launched")
        );
    }

    // ---------------------------------------------------------- the settings

    #[test]
    fn a_config_that_needs_nothing_offers_no_button() {
        let (body, caption) = settings_block(&cfg(true, true, None));
        assert_eq!(caption, None, "there is nothing left to press: {body}");
        assert!(body.contains("Nothing to change here"), "{body}");
    }

    #[test]
    fn the_button_says_what_it_will_change() {
        let (_, off) = settings_block(&cfg(false, false, None));
        let off = off.expect("something to press");
        assert!(off.contains("Turn on"), "the switch: {off}");
        assert!(off.contains("virtual joystick"), "and the sink: {off}");

        // Already on, sending somewhere else. Saying "turn on" here would be a
        // caption describing something that has already happened.
        let (_, on) = settings_block(&cfg(true, false, Some("127.0.0.1:4242")));
        let on = on.expect("something to press");
        assert!(on.contains("virtual joystick"), "{on}");
        assert!(!on.contains("Turn on"), "the switch is already on: {on}");
    }

    /// The strength word comes from the presets, never from comparing degrees
    /// in this file.
    #[test]
    fn the_strength_word_comes_from_the_presets() {
        for (name, yaw, pitch) in STRENGTHS {
            let mut c = cfg(true, true, None);
            c.extended_view.yaw.output_max_deg = yaw;
            c.extended_view.pitch.output_max_deg = pitch;
            let (body, _) = settings_block(&c);
            assert!(body.contains(name), "{name} should be named: {body}");
        }
        let mut c = cfg(true, true, None);
        c.extended_view.yaw.output_max_deg = 33.3;
        let (body, _) = settings_block(&c);
        assert!(body.contains("hand-tuned"), "{body}");
        for (name, _, _) in STRENGTHS {
            assert!(
                !body.contains(name),
                "a hand-tuned config claims no preset: {body}"
            );
        }
    }

    /// Somebody who already set up opentrack has game output ON. Telling them
    /// it is off would be the small untruth this whole window exists not to
    /// tell.
    #[test]
    fn game_output_on_with_another_sink_is_not_reported_as_off() {
        let (body, _) = settings_block(&cfg(true, false, Some("127.0.0.1:4242")));
        assert!(!body.contains("is off"), "{body}");
        assert!(
            body.contains("127.0.0.1:4242"),
            "it names where it is going: {body}"
        );

        let (body, _) = settings_block(&cfg(true, false, None));
        assert!(
            body.contains("nothing is set to receive it"),
            "on with no sink is its own sentence: {body}"
        );
    }

    // ------------------------------------------------------------ the bridge

    /// The shape of every genuinely user-visible bug this project has found in
    /// itself: an unmounted drive reported as "the game never saved this".
    #[test]
    fn a_missing_prefix_with_an_absent_library_does_not_read_as_never_launched() {
        let missing = [PathBuf::from("/mnt/games2")];
        let (text, action) = bridge_block(&BridgeState::NoPrefix, &missing);
        assert!(text.contains("/mnt/games2"), "{text}");
        assert!(text.contains("cannot rule out"), "{text}");
        assert_eq!(action, None, "there is nothing to install into");
    }

    /// The other half. Without it the test above is satisfied by appending the
    /// sentence always, which would name a library on every machine.
    #[test]
    fn a_missing_prefix_with_every_library_present_invents_no_library() {
        let (text, _) = bridge_block(&BridgeState::NoPrefix, &[]);
        assert!(!text.contains("cannot rule out"), "{text}");
        assert!(!text.contains("not on this machine"), "{text}");
        assert!(
            text.contains("native Linux game"),
            "and it still names the other innocent cause: {text}"
        );
    }

    #[test]
    fn a_prefix_that_already_has_the_files_is_not_offered_an_install() {
        let prefix = PathBuf::from("/games/compatdata/1/pfx");
        let (text, action) = bridge_block(
            &BridgeState::Files {
                dir: prefix.join(BRIDGE_SUBDIR),
                prefix,
            },
            &[],
        );
        assert_eq!(action, Some(Action::Reinstall));
        assert_ne!(action, Some(Action::Install));
        // It stats two paths, so it claims two paths — never that the bridge
        // "is installed", which two registry values decide.
        assert!(!text.contains("is installed"), "{text}");
        assert!(text.contains("registry"), "{text}");
    }

    /// Three states, three sentences, and each one says which state it is.
    ///
    /// Pairwise distinctness alone is too weak to be worth having: two states
    /// that merely quote different paths are already distinct strings while
    /// saying the same wrong thing. So each is also asked for the clause that
    /// only it may carry.
    #[test]
    fn the_three_bridge_states_read_as_three_sentences() {
        let prefix = PathBuf::from("/games/compatdata/1/pfx");
        let (no_prefix, _) = bridge_block(&BridgeState::NoPrefix, &[]);
        let (absent, _) = bridge_block(
            &BridgeState::Absent {
                prefix: prefix.clone(),
            },
            &[],
        );
        let (files, _) = bridge_block(
            &BridgeState::Files {
                dir: prefix.join(BRIDGE_SUBDIR),
                prefix: prefix.clone(),
            },
            &[],
        );

        assert!(
            no_prefix.contains("No Proton prefix was found"),
            "{no_prefix}"
        );
        assert!(
            !no_prefix.contains(&prefix.display().to_string()),
            "{no_prefix}"
        );
        assert!(
            absent.contains("files are not in it"),
            "a prefix without the bridge has to say the files are the thing missing, not \
             that the prefix is: {absent}"
        );
        assert!(absent.contains(&prefix.display().to_string()), "{absent}");
        assert!(files.contains(BRIDGE_SUBDIR), "{files}");

        let all = [no_prefix, absent, files];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two states read identically");
            }
        }
    }

    // ------------------------------------------------------------- the argv

    #[test]
    fn the_installer_is_never_forced() {
        let argv = install_argv(Path::new("/usr/bin/tobii"), "359320", None);
        assert!(
            !argv.iter().any(|a| a == "--force"),
            "--force overrides the refusal that stops the host's wine rewriting a game's \
             prefix; it is not a button's decision: {argv:?}"
        );
    }

    #[test]
    fn the_app_id_is_passed_and_the_name_is_not() {
        let argv = install_argv(Path::new("/usr/bin/tobii"), "359320", None);
        assert_eq!(
            argv,
            ["/usr/bin/tobii", "bridge", "install", "--steam", "359320"]
                .map(OsString::from)
                .to_vec()
        );
    }

    #[test]
    fn wine_is_passed_only_when_it_was_chosen() {
        let bare = install_argv(Path::new("/usr/bin/tobii"), "1", None);
        assert!(!bare.iter().any(|a| a == "--wine"), "{bare:?}");
        let chosen = install_argv(
            Path::new("/usr/bin/tobii"),
            "1",
            Some(Path::new("/games/Proton 9.0/files/bin/wine")),
        );
        let tail: Vec<&OsString> = chosen.iter().rev().take(2).collect();
        assert_eq!(tail[1], "--wine");
        assert_eq!(tail[0], "/games/Proton 9.0/files/bin/wine");
    }

    #[test]
    fn status_reads_the_same_game_the_install_would_have_written() {
        let s = status_argv(Path::new("/usr/bin/tobii"), "359320");
        assert_eq!(
            s,
            ["/usr/bin/tobii", "bridge", "status", "--steam", "359320"]
                .map(OsString::from)
                .to_vec()
        );
    }

    // ---------------------------------------------------------- the outcome

    /// `refuse_unverified_wine_for_steam` writes six lines naming the exact
    /// command to run. Re-wording them here would be a second, worse account of
    /// a decision this window did not make.
    const REFUSAL: &str = "cannot identify the Proton build this prefix belongs to\n\
                           run it with --wine pointing at that build's files/bin/wine, or\n\
                           tobii bridge status --steam 359320 runs nothing and names the prefix";

    #[test]
    fn a_refusal_is_reported_in_the_installers_own_words() {
        let o = install_outcome(Some(1), "", REFUSAL);
        assert!(!o.ok);
        let text = outcome_text(&o);
        assert!(
            text.contains(REFUSAL),
            "byte for byte, not summarised: {text}"
        );
        assert!(offers_wine_choice(&o), "and the button it earns is offered");
    }

    /// The installer prints `warning:` on a *successful* install, and
    /// swallowing that is how a wine mismatch becomes a silent no-tracking bug.
    #[test]
    fn a_warning_on_a_successful_install_is_not_swallowed() {
        let o = install_outcome(
            Some(0),
            "copied 3 file(s)",
            "warning: wine is 9.0, Proton is 10.0",
        );
        assert!(o.ok);
        let text = outcome_text(&o);
        assert!(text.contains("The bridge is installed."), "{text}");
        assert!(
            text.contains("warning: wine is 9.0, Proton is 10.0"),
            "{text}"
        );
        assert!(
            !offers_wine_choice(&o),
            "a success offers no refusal button"
        );
    }

    #[test]
    fn a_killed_installer_does_not_read_as_success() {
        let o = install_outcome(None, "", "");
        assert!(!o.ok);
        let text = outcome_text(&o);
        assert!(!text.contains("is installed"), "{text}");
        assert!(text.contains("killed"), "{text}");
    }

    /// A failure part way through a copy is not a failure that changed
    /// nothing, and this window only watched — so it claims neither.
    #[test]
    fn a_failed_install_does_not_claim_the_prefix_is_untouched() {
        for o in [
            install_outcome(Some(1), "", REFUSAL),
            install_outcome(None, "", ""),
        ] {
            let text = outcome_text(&o);
            assert!(
                !text.contains("Nothing in the prefix was changed"),
                "this window did not watch closely enough to say that: {text}"
            );
        }
    }

    // ---------------------------------------------------------- the binary

    #[test]
    fn the_binary_beside_this_one_wins_over_the_path() {
        let beside = PathBuf::from("/opt/tobii/bin/tobii");
        let b = beside.clone();
        let exists = move |p: &Path| p == b;
        let on_path = |_: &str| Some(PathBuf::from("/usr/bin/tobii"));
        assert_eq!(
            tobii_binary(
                Some(Path::new("/opt/tobii/bin/tobii-gtk")),
                &exists,
                &on_path
            ),
            Some(beside)
        );
    }

    #[test]
    fn the_path_is_the_fallback_and_nothing_found_is_none_not_a_bare_name() {
        let exists = |_: &Path| false;
        let on_path = |_: &str| Some(PathBuf::from("/usr/bin/tobii"));
        assert_eq!(
            tobii_binary(Some(Path::new("/opt/x/tobii-gtk")), &exists, &on_path),
            Some(PathBuf::from("/usr/bin/tobii"))
        );

        let nothing = |_: &str| None;
        let found = tobii_binary(Some(Path::new("/opt/x/tobii-gtk")), &exists, &nothing);
        assert_eq!(found, None);
        assert_ne!(
            found,
            Some(PathBuf::from("tobii")),
            "a bare name would be a PATH lookup done by the child's environment at spawn \
             time, which is the thing this function exists to do beforehand"
        );

        // And the sentence that replaces the button says what to type.
        let text = no_binary_text("359320");
        assert!(
            text.contains("tobii bridge install --steam 359320"),
            "{text}"
        );
    }

    // ---------------------------------------------------------- the profile

    fn at(path: &str) -> profiles::Origin {
        profiles::Origin::File(PathBuf::from(path))
    }

    /// Nothing partial, and both numbers named. Reading a file by the wrong
    /// rules is how a setting that is fine gets reported as wrong.
    #[test]
    fn a_profile_from_the_future_is_refused_whole() {
        let newer = profiles::VERSION + 1;
        let error = profiles::parse(&format!("version = {newer}\n"))
            .expect_err("a version this build does not have is refused");
        assert_eq!(error.unknown_version, Some(newer));

        let v = profile_verdict(Err(profiles::LoadError::Malformed {
            origin: at("/cfg/profiles/359320.toml"),
            error,
        }));
        assert!(
            matches!(v, ProfileVerdict::TooNew { .. }),
            "a newer grammar means update this program, not fix your file: {v:?}"
        );
        let text = profile_intro(&v, "A Game", Path::new("/cfg/profiles"));
        assert!(text.contains("/cfg/profiles/359320.toml"), "{text}");
        assert!(text.contains(&newer.to_string()), "{text}");
        assert!(text.contains(&profiles::VERSION.to_string()), "{text}");
        assert!(text.contains("has not been used at all"), "{text}");
    }

    /// A profile is a file a user may have been handed by a stranger. It must
    /// not be able to name anything outside the prefix it is about — and when
    /// it tries, the whole file goes, not the one check.
    #[test]
    fn a_profile_path_may_not_leave_the_prefix() {
        for bad in ["../../etc/passwd", "/etc/passwd", "drive_c\\users"] {
            let text = format!(
                "version = 1\n\n[[check]]\nformat = \"attributes-xml\"\npath = \"{bad}\"\n\
                 setting = \"s\"\nwants = \"1\"\ntell = \"t\"\n"
            );
            let error = profiles::parse(&text).expect_err(
                "a path that can leave the prefix has to be refused when the file is read, \
                 not joined onto a prefix and opened",
            );
            let v = profile_verdict(Err(profiles::LoadError::Malformed {
                origin: at("/cfg/profiles/359320.toml"),
                error,
            }));
            let body = profile_intro(&v, "A Game", Path::new("/cfg/profiles"));
            assert!(body.contains("/cfg/profiles/359320.toml"), "{bad}: {body}");
            assert!(body.contains("None of it has been used"), "{bad}: {body}");
        }
    }

    /// A profile that is there and could not be opened is not a game nobody has
    /// configured, and there is no falling back to a built-in one: "outranks"
    /// must not quietly become "unless I dislike it".
    #[test]
    fn an_unreadable_profile_does_not_read_as_no_profile() {
        let v = profile_verdict(Err(profiles::LoadError::Unreadable {
            origin: at("/cfg/profiles/359320.toml"),
            why: "Permission denied (os error 13)".to_string(),
        }));
        let text = profile_intro(&v, "A Game", Path::new("/cfg/profiles"));
        assert!(text.contains("Permission denied"), "{text}");
        assert!(text.contains("not the same as there being none"), "{text}");

        let absent = profile_intro(&ProfileVerdict::None, "A Game", Path::new("/cfg/profiles"));
        assert_ne!(text, absent, "the two negatives are two sentences");
    }

    /// Day one, and the intended behaviour rather than a gap.
    #[test]
    fn the_day_one_page_says_nobody_has_recorded_this_game() {
        assert!(
            profiles::BUILTIN.is_empty(),
            "the paragraph below says this build ships no profiles; the moment one ships, \
             that sentence stops being true and has to change with it"
        );
        let text = profile_intro(
            &ProfileVerdict::None,
            "Elite Dangerous",
            Path::new("/cfg/profiles"),
        );
        assert!(text.contains("Elite Dangerous"), "{text}");
        assert!(
            text.contains("/cfg/profiles"),
            "the user's own directory: {text}"
        );
        assert!(text.contains("nothing has been guessed"), "{text}");
        assert!(text.contains("never writes"), "{text}");
        assert!(
            text.contains("ships none"),
            "and it says so rather than reading as a fault: {text}"
        );
    }

    /// A profile that asks for settings must not be silently ignored — that is
    /// the window telling somebody nothing needs doing while something does.
    /// It must also not be silently *applied*: the button below says what it
    /// changes, and it does not say this.
    #[test]
    fn a_profile_that_names_settings_is_reported_and_not_applied() {
        let note = profile_settings_note(&[
            ("enabled".to_string(), "true".to_string()),
            ("rate_hz".to_string(), "60.0".to_string()),
        ])
        .expect("a profile that asks for settings has something to say");
        assert!(note.contains("enabled = true"), "{note}");
        assert!(note.contains("rate_hz = 60.0"), "{note}");
        assert!(
            note.contains("has not applied"),
            "it says it did not do it: {note}"
        );
        assert!(note.contains("tobii games set"), "and what does: {note}");

        assert_eq!(
            profile_settings_note(&[]),
            None,
            "a profile that asks for nothing gets no heading over an empty list"
        );
    }

    /// `Bridge` has three cases for the reason `Lookup` has three, and the
    /// third is the one that must stay silent.
    #[test]
    fn an_unstated_bridge_is_not_reported_as_a_no() {
        assert_eq!(
            profile_bridge_note(profiles::Bridge::Unstated),
            None,
            "a profile that does not mention the bridge has not said the game does without \
             one, and this window must not say it for them"
        );
        let required =
            profile_bridge_note(profiles::Bridge::Required).expect("Required says something");
        let not_needed =
            profile_bridge_note(profiles::Bridge::NotNeeded).expect("NotNeeded says something");
        assert!(required.contains("TrackIR"), "{required}");
        assert!(
            not_needed.contains("does not need the bridge"),
            "{not_needed}"
        );
        assert_ne!(required, not_needed);
    }

    // ------------------------------------------------------------ the rows

    fn row(answer: Answer) -> Row {
        Row {
            setting: "HeadlookMode".to_string(),
            preset: Some("Custom".to_string()),
            answer,
            wants: "1".to_string(),
            tell: "In Controls, set Head Look to Toggle.".to_string(),
        }
    }

    /// `tobii-gameconf`'s thesis, asserted at its consumer: "not set" and
    /// "cannot be read" are different answers, and neither may be reported as
    /// the other.
    #[test]
    fn the_four_negative_answers_are_four_different_sentences() {
        let all = [
            row_text(&row(Answer::Unwritten(
                "there is no directory at /p/Bindings".to_string(),
            ))),
            row_text(&row(Answer::Unreadable(
                "/p/Bindings could not be listed: Permission denied".to_string(),
            ))),
            row_text(&row(Answer::Absent)),
            row_text(&row(Answer::NotReadBack(
                "the value holds a character this reader cannot decode".to_string(),
            ))),
        ];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two answers read identically");
            }
        }
        assert!(
            !all[0].contains("not set"),
            "a file that was never written is not a setting that is unset: {}",
            all[0]
        );
        assert!(
            !all[1].contains("not set"),
            "a file that could not be read says nothing about the setting: {}",
            all[1]
        );
        assert!(all[2].contains("not set"), "{}", all[2]);
        assert!(
            all[2].contains("Custom"),
            "and names the preset it is not set in: {}",
            all[2]
        );
    }

    #[test]
    fn a_setting_that_is_right_carries_no_remedy_and_one_that_is_wrong_does() {
        let good = row_text(&Row {
            answer: Answer::Text("1".to_string()),
            ..row(Answer::Absent)
        });
        assert!(
            good.contains("which is what this program expects"),
            "{good}"
        );
        assert!(
            !good.contains("In Controls"),
            "a setting that is already right must not be given a remedy: {good}"
        );

        let bad = row_text(&Row {
            answer: Answer::Text("0".to_string()),
            ..row(Answer::Absent)
        });
        assert!(bad.contains("this program expects 1"), "{bad}");
        assert!(
            bad.contains("In Controls, set Head Look to Toggle."),
            "{bad}"
        );

        // Neither of the two confident answers, because nobody has measured
        // whether either game's own reader is case-sensitive.
        let cased = row_text(&Row {
            answer: Answer::Text("TOGGLE".to_string()),
            wants: "toggle".to_string(),
            ..row(Answer::Absent)
        });
        assert!(cased.contains("differs only in case"), "{cased}");
        assert!(
            cased.contains("not something this program has measured"),
            "{cased}"
        );
        assert_ne!(cased, good);
        assert_ne!(cased, bad);
    }

    /// A game with no prefix has nowhere for its own settings to be, and that
    /// is not the same as a setting being unset.
    #[test]
    fn a_game_with_no_prefix_reports_its_checks_as_unperformed() {
        let text = row_text(&row(Answer::NoPrefix));
        assert!(text.contains("not checked"), "{text}");
        assert!(!text.contains("not set"), "{text}");

        let unknown = row_text(&row(Answer::UnknownFormat("binds-v2".to_string())));
        assert!(unknown.contains("binds-v2"), "{unknown}");
        assert!(unknown.contains("no reader"), "{unknown}");
        assert_ne!(unknown, text);
    }

    /// The one assumption `binds` could not measure: a rolled-back game is read
    /// through the wrong start file and nothing else would say so.
    #[test]
    fn a_superseded_start_file_is_named_and_an_ordinary_one_says_nothing_extra() {
        let note = binds_note(&binds::Bindings {
            start: PathBuf::from("/p/StartPreset.4.start"),
            schema: Some(4),
            superseded: vec![PathBuf::from("/p/StartPreset.3.start")],
            presets: Vec::new(),
        })
        .expect("a superseded start file is worth saying out loud");
        assert!(note.contains("StartPreset.3.start"), "{note}");
        assert!(note.contains("rolled back"), "{note}");

        let quiet = binds_note(&binds::Bindings {
            start: PathBuf::from("/p/StartPreset.start"),
            schema: None,
            superseded: Vec::new(),
            presets: Vec::new(),
        });
        assert_eq!(quiet, None, "nothing unusual, nothing to say");
    }
}
