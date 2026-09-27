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
//! over the hub, is itself the thing editing the file, and the installer it
//! started reports itself.
//!
//! It does read one fact it does not own: what the device thread made of the
//! virtual joystick ([`crate::device::JoystickStatus`]). That is read on each
//! refresh and never polled, so a joystick that fails a second after this page
//! is drawn is reported on the next pass and not before. The card behind this
//! window is the live account of what is happening, and it is the one on the
//! 33 ms tick.

use gtk::prelude::*;
use gtk::{glib, Align, Application, Label, Orientation};

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tobii_config::profiles;
use tobii_gameconf::{attrs, binds, Lookup, Source};
use tobii_output::games::OutputConfig;
use tobii_steam::App;

use crate::device::JoystickStatus;
use crate::games::{strength_index, STRENGTHS};

/// Where `tobii bridge install` puts its files inside a prefix, what it puts
/// there, and the one file that decides whether anything can load at all.
///
/// Spelled here rather than imported: `tobii-cli` is a binary crate with no
/// library target, so nothing in the hub can link `bridge.rs`. The names are
/// that file's `INSTALL_SUBDIR`, its `ARTIFACTS` and its `REQUIRED_ARTIFACT`,
/// and the wording in [`bridge_block`] is careful to claim only what a stat of
/// these paths can support — it never says "the bridge is installed" from a
/// stat alone, because two registry values inside the prefix decide that and
/// this window does not read them.
///
/// [`BRIDGE_FILES`] is a list and never a count written out in a sentence: the
/// paragraph offering the install used to say "two small files" while the
/// installer copied three and printed `copied 3 file(s)` underneath it.
const BRIDGE_SUBDIR: &str = "drive_c/tobii-bridge";
const BRIDGE_FILES: [&str; 3] = [
    "tobii-bridge.exe",
    "freetrackclient64.dll",
    "NPClient64.dll",
];
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

/// Where this program's output is going, named from the settings alone — and
/// what is wrong with one of them.
///
/// Written here rather than taken from [`crate::games::status_text`], which is
/// deliberately not called from this window at all. That function also takes
/// `tracker_on`, a live fact this window has no reader for, and feeding it
/// `false` to get a sentence out of it would print "Ready to send to…" to
/// somebody whose tracker is running. The card answers *what is happening*;
/// this window answers *what is configured*. Two questions, two vocabularies.
///
/// The joystick is the one place the two questions touch, and it takes the
/// card's answer rather than the checkbox's. `games.rs` states the rule where
/// it enforces it: the tick is a request, `/dev/uinput` can refuse it, and "a
/// line that says 'sending to a virtual joystick' when none exists is exactly
/// the support thread this row was written to prevent". A window reading
/// `cfg.joystick` alone printed that line over a card saying the joystick
/// could not be created.
///
/// Only [`JoystickStatus::Failed`] changes the answer. [`JoystickStatus::Off`]
/// means *nobody has asked for one yet*, which for a window about what is
/// configured is not a fault — the card says "(starting)" there because the
/// card is about what is happening.
///
/// It is prose and not a predicate, which is why duplicating it is safe: the
/// predicates this window shares with the card — [`strength_index`] and
/// [`STRENGTHS`] — are imported rather than re-derived.
fn destinations(cfg: &OutputConfig, joystick: &JoystickStatus) -> (Vec<String>, Option<String>) {
    let mut out = Vec::new();
    let mut trouble = None;
    if cfg.joystick {
        match joystick {
            JoystickStatus::Failed(why) => {
                trouble = Some(format!(
                    "The virtual joystick is asked for in the settings and could not be \
                     created — {why}. Nothing is reaching it, so this window does not count \
                     it as a destination; the card behind this window says the same."
                ))
            }
            JoystickStatus::Off | JoystickStatus::Present => {
                out.push("a virtual joystick".to_string())
            }
        }
    }
    if let Some(addr) = &cfg.opentrack {
        out.push(format!("opentrack at {addr}"));
    }
    if let Some(port) = cfg.bridge_port {
        out.push(format!("the Wine bridge on port {port}"));
    }
    (out, trouble)
}

/// The strength as a clause, from the presets and never from comparing
/// degrees here.
fn strength_clause(cfg: &OutputConfig) -> String {
    match strength_index(cfg) {
        Some(i) => format!("at {} strength", STRENGTHS[i].0),
        None => "at a hand-tuned strength".to_string(),
    }
}

/// The paragraph under block 1 about what a virtual joystick is for.
///
/// The last sentence is the profile's to answer wherever the profile has
/// answered it. Saying "whether it is the right one for this game is not
/// something this program knows" above a block 2 that says, from the same
/// profile, that the game reads TrackIR and "the virtual joystick above will
/// not reach it" is one page holding two opinions about what it knows.
fn joystick_paragraph(bridge: profiles::Bridge) -> String {
    let mut s = "A virtual joystick is the destination that needs nothing else installed — \
                 no Wine, no opentrack — and it works in native and Proton games alike. "
        .to_string();
    s.push_str(match bridge {
        profiles::Bridge::Unstated => {
            "Whether it is the right one for this game is not something this program knows: a \
             game that speaks TrackIR or FreeTrack wants the Wine bridge below instead."
        }
        profiles::Bridge::Required => {
            "It is not the one for this game. The profile says this game reads TrackIR or \
             FreeTrack, which a joystick does not speak, so the Wine bridge below is what it \
             needs — turning game output on is still what feeds the bridge."
        }
        profiles::Bridge::NotNeeded => {
            "The profile for this game says it does not need the Wine bridge, so this is the \
             destination for it."
        }
    });
    s
}

/// Block 1: this program's own settings, and the one button that changes them.
///
/// Returns the body and the button's caption, [`None`] when there is nothing
/// to press. Three states rather than the two an `enabled && joystick` gate
/// would give, because the middle one is real: somebody who has already set up
/// opentrack has game output on and no joystick, and telling them to "turn on"
/// a switch that is on is the kind of small untruth this whole window exists
/// not to tell.
///
/// # Why the caption is built rather than written
///
/// The button sets `enabled` **and** `joystick`, and `enabled` is the switch
/// every other destination hangs off. With opentrack and a bridge port already
/// in the file, a caption reading "Turn on and send to a virtual joystick"
/// promised one destination and produced three. This module's own doctrine is
/// that a button must not do more than its caption says, so the caption names
/// the destinations the press will leave running, worded by the same
/// [`destinations`] the body is worded by.
pub(crate) fn settings_block(
    cfg: &OutputConfig,
    bridge: profiles::Bridge,
    joystick: &JoystickStatus,
) -> (String, Option<String>) {
    let (sinks, trouble) = destinations(cfg, joystick);

    if cfg.enabled && cfg.joystick {
        // Everything this button would set is set. Whether that is enough is a
        // separate question, and `trouble` is the case where it is not.
        let mut body = match sinks.as_slice() {
            [] => "Head tracking for games is on, and nothing is receiving it.".to_string(),
            _ => format!(
                "Head tracking for games is on, sending to {list}, {strength}.",
                list = join_and(&sinks),
                strength = strength_clause(cfg),
            ),
        };
        match &trouble {
            // Nothing wrong and nothing left to press, said out loud rather
            // than left as a block with no button under it.
            None => body.push_str(" Nothing to change here."),
            Some(t) => {
                body.push_str("\n\n");
                body.push_str(t);
            }
        }
        return (body, None);
    }

    let mut body = if !cfg.enabled {
        match sinks.as_slice() {
            [] => "Head tracking for games is off.".to_string(),
            // The destinations already in the file, named before the switch is
            // touched. They are what the one button below will also start, and
            // a page that mentioned them only once the switch was on told
            // somebody they were turning on a joystick and gave them three.
            _ => format!(
                "Head tracking for games is off. It is already configured to send to {list}, \
                 {strength} — turning it on starts all of that, not only what the button \
                 below adds.",
                list = join_and(&sinks),
                strength = strength_clause(cfg),
            ),
        }
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
    if let Some(t) = &trouble {
        body.push_str("\n\n");
        body.push_str(t);
    }
    body.push_str("\n\n");
    body.push_str(&joystick_paragraph(bridge));

    let caption = if cfg.enabled {
        // It is already on. Saying "turn on" here would be a caption that
        // describes something that has already happened, and the press adds
        // the joystick and nothing else.
        "Send to a virtual joystick".to_string()
    } else {
        // What the file will say once the press has landed.
        let mut after = cfg.clone();
        after.enabled = true;
        after.joystick = true;
        let (mut list, _) = destinations(&after, joystick);
        if list.is_empty() {
            // Only reachable with a joystick that has already failed. The
            // press still asks for one, so the caption says it asks for one.
            list.push("a virtual joystick".to_string());
        }
        format!("Turn on and send to {}", join_and(&list))
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

/// The Proton prefixes for this game that [`bridge_state`] did **not** pick,
/// each with whether the bridge's files are in it.
///
/// [`tobii_steam::prefix`] answers with one, and its own documentation says
/// what the others are: Steam's "Move Install Folder" does not move
/// `compatdata`, so a title moved between libraries leaves its old prefix
/// behind and gets a fresh one on the next run — and "installing into the
/// abandoned one succeeds, prints the ordinary success text, and does nothing
/// at all for the game".
///
/// This window is the one place in the project that names a prefix to somebody
/// and offers to install into it, so it is the one place that has to be able
/// to say there is more than one. Nothing else on the page hints at it:
/// [`tobii_steam::apps`] collapses two copies of a title into one row and
/// blanks the build id when they disagree, which removes the last sign that
/// there were two.
///
/// The order is [`tobii_steam::libraries`]', which is the order the library
/// file lists them in; the chosen prefix is dropped from it by path rather
/// than by position, because which library holds the manifest is what decides
/// it and that is not a position.
fn other_prefixes(home: &Path, appid: &str, chosen: Option<&Path>) -> Vec<(PathBuf, bool)> {
    tobii_steam::libraries(home)
        .into_iter()
        .map(|lib| lib.join("steamapps/compatdata").join(appid).join("pfx"))
        .filter(|p| p.join("drive_c").is_dir())
        .filter(|p| Some(p.as_path()) != chosen)
        .map(|p| {
            let has = p.join(BRIDGE_SUBDIR).join(BRIDGE_ARTIFACT).is_file();
            (p, has)
        })
        .collect()
}

/// What to say when a title has more than one Proton prefix on this machine.
///
/// [`None`] for the ordinary case, which is the same rule [`missing_note`]
/// follows: a note appended unconditionally would tell every user about a
/// second prefix they do not have.
pub(crate) fn other_prefixes_note(others: &[(PathBuf, bool)]) -> Option<String> {
    if others.is_empty() {
        return None;
    }
    let n = others.len();
    let mut s = format!(
        "This title has {n} other Proton {prefix} on this machine — {list}. The one named \
         above is the one an install writes into, because it is the one in the library \
         holding the game's manifest. Steam's Move Install Folder does not move a prefix, so \
         a title moved between libraries leaves its old one behind; installing into the \
         abandoned one succeeds, prints the ordinary success text, and does nothing at all \
         for the game.",
        prefix = plural(n, "prefix", "prefixes"),
        list = list_paths(&others.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>()),
    );
    let with: Vec<PathBuf> = others
        .iter()
        .filter(|(_, has)| *has)
        .map(|(p, _)| p.clone())
        .collect();
    if !with.is_empty() {
        s.push_str(&format!(
            " The bridge's files are already in {list} — so if the game runs from {that}, it \
             has been set up and what this block says above is about the wrong prefix.",
            list = list_paths(&with),
            that = plural(with.len(), "that one", "one of those"),
        ));
    }
    Some(s)
}

/// Block 2: the Wine bridge.
///
/// Note the wording discipline: this window stats paths, so it claims paths.
/// It never says "the bridge is installed" from a stat — a record file and two
/// registry values inside the prefix are what decide that, and they belong to
/// the program that owns them. `Details` is `tobii bridge status`, which reads
/// them.
///
/// `tobii` is which program the buttons would run, [`None`] when there is
/// none to run. It decides the button as well as the sentence: an install
/// button over a machine with no `tobii` is a button that can only fail, and
/// the page says what to type instead. It is named on the page because this
/// window is the hub and `tobii` is a separate program on its own version —
/// the premise of everything else block 2 reports.
pub(crate) fn bridge_block(
    state: &BridgeState,
    missing: &[PathBuf],
    others: &[(PathBuf, bool)],
    tobii: Option<&Path>,
    appid: &str,
) -> (String, Option<Action>) {
    let (mut text, action) = match state {
        BridgeState::NoPrefix => {
            let s = "No Proton prefix was found for this game. Steam makes one the first \
                     time a title runs under Proton, so if this is a Windows game, run it \
                     once and come back.\n\n\
                     If it is a native Linux game it will never have one, and the Wine \
                     bridge is not how it gets head tracking — the virtual joystick above \
                     is."
            .to_string();
            // No button. There is nothing to install into, and a button that
            // fails is worse than no button.
            (s, None)
        }
        BridgeState::Absent { prefix } => (
            format!(
                "The prefix is at {}.\nThe bridge's files are not in it.\n\n\
                 The bridge is what lets a game see a TrackIR or FreeTrack device from inside \
                 Wine. Installing it copies {n} small {file} into the prefix — {list} — and \
                 sets two registry values. It does not touch the game or its saves.",
                prefix.display(),
                n = BRIDGE_FILES.len(),
                file = plural(BRIDGE_FILES.len(), "file", "files"),
                list = BRIDGE_FILES.join(", "),
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
    };
    // Both notes hang off every state, because both are reasons the sentence
    // above them may be about the wrong place on this machine.
    for note in [other_prefixes_note(others), missing_note(missing)]
        .into_iter()
        .flatten()
    {
        text.push_str("\n\n");
        text.push_str(&note);
    }
    match (tobii, &action) {
        (Some(t), _) => {
            text.push_str("\n\n");
            text.push_str(&binary_line(t));
        }
        (None, Some(_)) => {
            text.push_str("\n\n");
            text.push_str(&no_binary_text(appid));
            return (text, None);
        }
        (None, None) => {}
    }
    (text, action)
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

/// Judge a finished subprocess on what it handed back, which is an exit code
/// and two streams and nothing else.
///
/// [`Outcome::ok`] is "it exited zero", and that is all it may be taken to
/// mean. It is **not** "the work was done": see [`outcome_text`], which asks
/// the prefix instead.
pub(crate) fn install_outcome(code: Option<i32>, stdout: &str, stderr: &str) -> Outcome {
    Outcome {
        ok: code == Some(0),
        code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}

impl Outcome {
    /// Both streams as one haystack, for the questions that do not care which
    /// one a program chose.
    fn said(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

/// Whether what ran was a `tobii` that has no `bridge <sub>` at all.
///
/// Neither the exit code nor an empty stderr answers this. The `tobii` this
/// machine has on its `$PATH` today is 0.3.0, and it answers `tobii bridge
/// status --steam 359320` with its usage line, ``unknown argument `status` ``
/// — and **exit code 0**. An unknown subcommand is therefore indistinguishable
/// from a clean success by everything except what the program said.
///
/// Two readings, because a program may report it either way: the argument
/// named back as unknown, or a usage line for `bridge` whose list of
/// subcommands does not have this one in it.
///
/// This is a quotation of another program's output and it is allowed to go
/// stale — `tobii-cli` is a binary crate with no library target, so nothing
/// here can link its strings, the same limitation [`BRIDGE_SUBDIR`] records.
/// It fails closed: a wording this does not recognise leaves the ordinary
/// answer standing, which for an install is [`outcome_text`]'s stat of the
/// prefix and not a claim of success.
pub(crate) fn too_old_for(sub: &str, o: &Outcome) -> bool {
    let said = o.said();
    said.contains(&format!("unknown argument `{sub}`"))
        || (said.contains("usage: tobii bridge") && !said.contains(sub))
}

/// What to print above a subcommand's output when the program that ran does
/// not have that subcommand.
///
/// Its own answer, rather than a success or a wall of usage text with no
/// explanation over it.
pub(crate) fn too_old_note(sub: &str, tobii: &Path) -> String {
    format!(
        "{} has no `bridge {sub}`: it printed its usage and stopped, and it stopped with a \
         success code doing it. That is an older `tobii` than this window — the two are \
         separate programs, installed and updated separately, and this window runs whichever \
         one it found. Its own words follow.",
        tobii.display()
    )
}

/// Which `tobii` this window runs, named where the user can see it.
///
/// [`tobii_binary`] falls back to `$PATH`, so the program behind these buttons
/// need not be the one this build shipped beside — and on the machine this was
/// written on it is not: the hub was built from this tree and the `tobii` on
/// `$PATH` is 0.3.0. A page that never named it left the user to work that out
/// from the installer's own error text.
pub(crate) fn binary_line(tobii: &Path) -> String {
    format!(
        "The buttons here run {}. It is a separate program with its own version, so this \
         window judges it by what the prefix holds afterwards rather than by what it \
         reported.",
        tobii.display()
    )
}

/// The installer's result, cross-checked against the prefix as it stands now.
///
/// # Why `after` is a parameter
///
/// An exit code is a claim, and this window's job is not to repeat claims. A
/// `tobii` that never understood the command exits zero — 0.3.0 does exactly
/// that — so "exit 0" printed as "The bridge is installed." put that sentence
/// directly above the program's own error, on a page whose block 2, recomputed
/// in the same pass, still said the files were not in the prefix. Two readings
/// of one prefix in one paragraph, disagreeing. So the claim is checked: the
/// caller passes the [`BridgeState`] it has just recomputed, and a success
/// with no [`BRIDGE_ARTIFACT`] under the prefix is not reported as an install.
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
pub(crate) fn outcome_text(o: &Outcome, after: &BridgeState) -> String {
    let said = !o.stdout.trim().is_empty() || !o.stderr.trim().is_empty();
    let mut s = if o.ok && too_old_for("install", o) {
        "Nothing was installed: the program that ran has no such command. It printed its \
         usage and stopped — with a success code, which is why this had to be read out of \
         what it said rather than out of what it returned."
            .to_string()
    } else if o.ok {
        match after {
            BridgeState::Files { .. } => "The bridge is installed.".to_string(),
            BridgeState::Absent { prefix } => format!(
                "The installer reported no error, and the bridge's files are still not in \
                 the prefix: there is no {BRIDGE_ARTIFACT} under {}. This window will not \
                 call that an install.\n\n\
                 An exit code is all a program hands back, and one that understood nothing \
                 and exited zero looks exactly like one that did the work. Press Details to \
                 see what the prefix holds, and read what this run said below.",
                prefix.join(BRIDGE_SUBDIR).display()
            ),
            BridgeState::NoPrefix => "The installer reported no error, and there is no Proton \
                                      prefix here for it to have written into — so whatever \
                                      it did, it was not this game's prefix. This window will \
                                      not call that an install."
                .to_string(),
        }
    } else {
        match o.code {
            None => "The installer stopped before it could report a result — it was killed, \
                     or it crashed. What it had done by then is not something this window \
                     can say; press Details to read the prefix as it stands."
                .to_string(),
            Some(code) if said => format!("The installer stopped, exit status {code}. It said:"),
            // "It said:" with nothing under it is a colon pointing at a gap. A
            // child that exits non-zero having printed nothing at all is the
            // one case where the installer's own words are not the answer, so
            // the sentence has to stand on its own.
            Some(code) => format!(
                "The installer stopped, exit status {code}, and printed nothing at all — no \
                 error, and no word about what it had done. Press Details to read the prefix \
                 as it stands."
            ),
        }
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

/// `bridge.rs`'s own sentence for the one refusal a Proton build answers.
///
/// Quoted for the reason [`BRIDGE_SUBDIR`] is quoted, and with the same
/// consequence if it drifts: the button stops being offered, and the refusal
/// itself — which names the exact command, verbatim — is still on the page.
const WINE_REFUSAL_MARK: &str = "Refusing to run that one";

/// Whether the installer's refusal is one the user can answer by naming a
/// Proton build.
///
/// The refusal already tells them where to look; the button saves them
/// retyping it.
///
/// It is not a search for `--wine`. Every usage block this program prints
/// lists every flag the command takes, `[--wine PATH]` among them, so a
/// substring test offered "Choose the Proton build…" for a plain mistyped
/// command — and pressing it re-ran the same failing command with `--wine`
/// added to it. What earns the button is the refusal that asks for a build,
/// not the flag's name appearing somewhere.
pub(crate) fn offers_wine_choice(o: &Outcome) -> bool {
    !o.ok && {
        let said = o.said();
        said.contains(WINE_REFUSAL_MARK) && said.contains("--wine")
    }
}

/// What the page shows about the subprocess this window has run.
///
/// One function, because "whose answer is this?" has to be asked once. The
/// three slots it reads are filled by closures that outlive the page they were
/// started from: a job started for one game finishes after the user has gone
/// Back and picked another, and the page was then showing the first game's
/// "Running now:" line and then its success over the second game's name.
/// Clearing the slots on a page change cannot fix that — the answer had not
/// arrived yet — so each slot carries the app id it belongs to and this
/// decides what the page showing `appid` may say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobView {
    /// Appended to block 2. Empty when there is nothing to report.
    pub text: String,
    /// Whether the blocks' buttons may be pressed. False while ANY job runs,
    /// this game's or another's: one child process at a time is what this
    /// window is prepared to wait for.
    pub enabled: bool,
    /// Whether the Proton-build button is earned.
    pub offer_wine: bool,
    /// What `Details` printed, for this game.
    pub report: Option<String>,
}

/// Decide all of that for the page showing `appid`, from the slots as they
/// stand and the [`BridgeState`] the caller has just recomputed.
pub(crate) fn job_view(
    appid: &str,
    running: Option<&(String, String)>,
    outcome: Option<&(String, Outcome)>,
    report: Option<&(String, String)>,
    state: &BridgeState,
) -> JobView {
    let mine = |slot: Option<&(String, String)>| {
        slot.filter(|(id, _)| id == appid).map(|(_, v)| v.clone())
    };
    let mut text = String::new();
    let mut offer_wine = false;

    if let Some(cmd) = mine(running) {
        text.push_str(&format!(
            "\n\nRunning now:\n{cmd}\n\n\
             Closing this window does not stop it — this window stops watching and the \
             installer finishes. It copies each file beside its target and renames it, so a \
             half-finished copy cannot be loaded; stopping it between the copy and the \
             registry write is the one way to leave a prefix inconsistent."
        ));
    } else if let Some((_, o)) = outcome.filter(|(id, _)| id == appid) {
        text.push_str("\n\n");
        // Judged against the prefix as the caller has just stated it, never
        // against the exit code alone.
        text.push_str(&outcome_text(o, state));
        offer_wine = offers_wine_choice(o);
    }

    // A job for another game greys the buttons here, so it is named: a control
    // that is unavailable with nothing on the page saying why is the one thing
    // this window's own blocks are written not to be.
    if let Some((id, cmd)) = running.filter(|(id, _)| id != appid) {
        text.push_str(&format!(
            "\n\nThe buttons here are unavailable while a job started for app id {id} is \
             running:\n{cmd}"
        ));
    }

    JobView {
        text,
        enabled: running.is_none(),
        offer_wine,
        report: mine(report),
    }
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

/// [`Row::tell`] as a clause to append, empty when the profile left it blank.
///
/// The grammar requires the field, so the guard is against whitespace rather
/// than against a profile that left it out.
fn tell_clause(r: &Row) -> String {
    match r.tell.trim() {
        "" => String::new(),
        t => format!(" {t}"),
    }
}

/// What the profile expects and what to do about it, for every answer that has
/// not already said the first half.
///
/// Appended to every answer but a confirmed match, and that is the point of
/// it: the commonest real case is not a wrong value, it is a player who has
/// never touched the setting, and that answer used to read in full as
/// `HeadlookMode: not set.` — no statement of what was expected and no
/// instruction, on a page whose third block exists to say exactly those two
/// things. `tobii games profile show` prints both under every check it lists,
/// whatever the file turned out to hold, and two halves of one feature
/// disagreeing about whether the user is told what to do is worse than either
/// answer on its own.
fn expectation(r: &Row) -> String {
    format!(" This program expects {}.{}", r.wants, tell_clause(r))
}

/// One check row, as a sentence.
pub(crate) fn row_text(r: &Row) -> String {
    let at = match &r.preset {
        Some(p) => format!(", in preset “{p}”"),
        None => String::new(),
    };
    let mut s = match &r.answer {
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
        // The three answers that have read a value already name it, so they
        // word the expectation themselves rather than taking [`expectation`].
        Answer::Text(v) => {
            let head = format!("{}: is {v}{at}", r.setting);
            return match verdict(v, &r.wants) {
                // The one answer that earns no remedy at all: it is already
                // right, and telling somebody to set what is set is how a
                // report teaches people to stop reading it.
                Verdict::Matches => format!("{head} — which is what this program expects."),
                Verdict::OnlyCase => format!(
                    "{head} — this program expects {wants}, which differs only in case. \
                     Whether this game's own reader cares is not something this program has \
                     measured.{tell}",
                    wants = r.wants,
                    tell = tell_clause(r),
                ),
                Verdict::Differs => format!(
                    "{head} — this program expects {wants}.{tell}",
                    wants = r.wants,
                    tell = tell_clause(r),
                ),
            };
        }
    };
    s.push_str(&expectation(r));
    s
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
            "{n} older start {file} next to it {was} not read — {list}. If this game has been \
             rolled back, {that} the live one and the reading above is of the wrong file",
            n = b.superseded.len(),
            file = plural(b.superseded.len(), "file", "files"),
            // The verb agrees with the count as well as the noun, in both
            // halves of the sentence. A directory with one older start file
            // read "1 older start file next to it were not read … one of those
            // is the live one".
            was = plural(b.superseded.len(), "was", "were"),
            that = plural(b.superseded.len(), "that one is", "one of those is"),
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
///
/// `appid` is stored beside the command line and is what the page keys its
/// "Running now:" line to. One job at a time in this window, whichever game it
/// was started for — the guard below is on `running` itself and not on a
/// per-game slot, because one child process is what this window is prepared to
/// wait for and a second `tobii bridge install` running beside the first is
/// not an improvement.
fn start_job(
    argv: Vec<OsString>,
    appid: &str,
    alive: &Rc<Cell<bool>>,
    running: &Rc<RefCell<Option<(String, String)>>>,
    done: impl Fn(Outcome) + 'static,
) {
    if running.borrow().is_some() {
        return;
    }
    let line = pretty(&argv);
    *running.borrow_mut() = Some((appid.to_string(), line.clone()));

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
///
/// `joystick` is the device thread's own answer, the one the hub's card
/// reports from — see [`destinations`] for why the checkbox is not enough. It
/// is an `Arc<Mutex<_>>` and holds no widget, so carrying it into the button
/// handler that opens this window cannot be half of a GTK reference cycle.
pub fn open(
    app: &Application,
    parent: &impl IsA<gtk::Window>,
    joystick: Arc<Mutex<JoystickStatus>>,
) -> gtk::Window {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let dir = profiles::profiles_dir();
    open_with(app, parent, scan(&home, &dir), joystick)
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
///
/// `joystick` is read, never written: this window asks the device thread what
/// became of the tick, the same fact the card behind it reports.
pub fn open_with(
    app: &Application,
    parent: &impl IsA<gtk::Window>,
    scanned: Scan,
    joystick: Arc<Mutex<JoystickStatus>>,
) -> gtk::Window {
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
    // All four carry the app id their answer is about, and they carry it
    // because clearing them on a page change was not enough. A job
    // outlives the page it was started from: select a game, press *Install the
    // bridge*, press *Back* and pick another, and the running line and then
    // the outcome landed on whichever game was selected when the child
    // finished — the second game's page showing the first game's install and
    // then its success. `row_activated` can clear what is there; it cannot
    // clear what has not arrived yet. So the closure writes the id with the
    // answer and the page reads only its own.
    let running: Rc<RefCell<Option<(String, String)>>> = Rc::default();
    let outcome: Rc<RefCell<Option<(String, Outcome)>>> = Rc::default();
    let report: Rc<RefCell<Option<(String, String)>>> = Rc::default();
    let wrote: Rc<RefCell<Option<(String, String)>>> = Rc::default();
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
        let (scan, sel, running, outcome, report, wrote, tobii, joystick) = (
            scan.clone(),
            sel.clone(),
            running.clone(),
            outcome.clone(),
            report.clone(),
            wrote.clone(),
            tobii.clone(),
            joystick.clone(),
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
            // A poisoned lock is the device thread having panicked, which this
            // window has nothing to say about and must not panic over: the
            // answer it gives then is the default, "nobody has asked for one".
            let joy = joystick
                .lock()
                .map(|g| g.clone())
                .unwrap_or_else(|e| e.into_inner().clone());
            let bridge = profile
                .map(|p| p.bridge)
                .unwrap_or(profiles::Bridge::Unstated);
            let (mut text, caption) = settings_block(&cfg, bridge, &joy);
            if let Some(note) = profile.and_then(|p| profile_settings_note(&p.settings)) {
                text.push_str("\n\n");
                text.push_str(&note);
            }
            if let Some((_, w)) = wrote.borrow().as_ref().filter(|(id, _)| *id == app.appid) {
                text.push_str("\n\n");
                text.push_str(w);
            }
            s_body.set_text(&text);
            if let Some(btn) = settings_w.upgrade() {
                match &caption {
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
                b.set_sensitive(running.borrow().is_none());
            }

            // --- block 2
            let state = bridge_state(&scan.home, &app.appid);
            let chosen = match &state {
                BridgeState::NoPrefix => None,
                BridgeState::Absent { prefix } | BridgeState::Files { prefix, .. } => {
                    Some(prefix.as_path())
                }
            };
            let others = other_prefixes(&scan.home, &app.appid, chosen);
            let (mut text, action) =
                bridge_block(&state, &scan.missing, &others, tobii.as_deref(), &app.appid);
            if let Some(note) = profile.and_then(|p| profile_bridge_note(p.bridge)) {
                text.push_str("\n\n");
                text.push_str(&note);
            }
            let job = job_view(
                &app.appid,
                running.borrow().as_ref(),
                outcome.borrow().as_ref(),
                report.borrow().as_ref(),
                &state,
            );
            text.push_str(&job.text);
            br_body.set_text(&text);
            if let Some(btn) = install_w.upgrade() {
                match action {
                    Some(a) => {
                        crate::widget::set_button_text(&btn, a.caption());
                        btn.set_visible(true);
                    }
                    None => btn.set_visible(false),
                }
            }
            if let Some(btn) = details_w.upgrade() {
                btn.set_visible(tobii.is_some() && !matches!(state, BridgeState::NoPrefix));
            }
            if let Some(btn) = wine_w.upgrade() {
                btn.set_visible(tobii.is_some() && job.offer_wine);
            }
            if let Some(b) = b_actions.upgrade() {
                b.set_sensitive(job.enabled);
            }
            match &job.report {
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
                for check in &loaded.checks {
                    let (rows, note) = run_check(check, chosen);
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
        let (refresh, wrote, sel) = (refresh.clone(), wrote.clone(), sel.clone());
        settings_btn.connect_clicked(move |_| {
            let Some(app) = sel.borrow().clone() else {
                return;
            };
            // Through `games::edit_config`, which re-reads the file first:
            // three editors now share `games.toml` — this window, the hub's
            // card, and `tobii games set` — and a held copy makes whichever
            // was touched second clobber the other.
            crate::games::edit_config("game-output settings", |cfg| {
                cfg.enabled = true;
                cfg.joystick = true;
            });
            *wrote.borrow_mut() = Some((app.appid, wrote_line(&tobii_output::games::games_path())));
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
            let (refresh2, outcome2, id) = (refresh.clone(), outcome.clone(), app.appid.clone());
            start_job(argv, &app.appid, &alive, &running, move |o| {
                *outcome2.borrow_mut() = Some((id.clone(), o));
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
            let (refresh2, report2, id, t2) = (
                refresh.clone(),
                report.clone(),
                app.appid.clone(),
                t.clone(),
            );
            start_job(argv, &app.appid, &alive, &running, move |o| {
                let mut s = String::new();
                // A `tobii` with no `bridge status` answers with its usage and
                // exits zero. Its words are still printed — they are the only
                // evidence — but not on their own, as a wall of text with
                // nothing above it saying what happened.
                if too_old_for("status", &o) {
                    s.push_str(&too_old_note("status", &t2));
                    s.push_str("\n\n");
                }
                s.push_str(o.stdout.trim_end());
                if !o.stderr.trim().is_empty() {
                    if !s.is_empty() && !s.ends_with('\n') {
                        s.push_str("\n\n");
                    }
                    s.push_str(o.stderr.trim_end());
                }
                *report2.borrow_mut() = Some((id.clone(), s));
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
                let (refresh2, outcome2, id) =
                    (refresh.clone(), outcome.clone(), app.appid.clone());
                start_job(argv, &app.appid, &alive, &running, move |o| {
                    *outcome2.borrow_mut() = Some((id.clone(), o));
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
            // A different game's answers are not this game's — and clearing
            // them here is only half of that, because a job still running will
            // write its answer back after this line has run. The other half is
            // the app id each of these carries: see where they are declared.
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

    /// Block 2 on a machine that has a `tobii` to run. The two arguments the
    /// tests that are not about the binary would otherwise repeat.
    fn block2(
        state: &BridgeState,
        missing: &[PathBuf],
        others: &[(PathBuf, bool)],
    ) -> (String, Option<Action>) {
        bridge_block(
            state,
            missing,
            others,
            Some(Path::new("/usr/bin/tobii")),
            "359320",
        )
    }

    /// Block 1 for a game no profile has anything to say about, with a
    /// joystick nobody has asked for yet. The two arguments the tests that do
    /// not care about them would otherwise repeat.
    fn block1(cfg: &OutputConfig) -> (String, Option<String>) {
        settings_block(cfg, profiles::Bridge::Unstated, &JoystickStatus::Off)
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
        let (body, caption) = block1(&cfg(true, true, None));
        assert_eq!(caption, None, "there is nothing left to press: {body}");
        assert!(body.contains("Nothing to change here"), "{body}");
    }

    #[test]
    fn the_button_says_what_it_will_change() {
        let (_, off) = block1(&cfg(false, false, None));
        let off = off.expect("something to press");
        assert!(off.contains("Turn on"), "the switch: {off}");
        assert!(off.contains("virtual joystick"), "and the sink: {off}");

        // Already on, sending somewhere else. Saying "turn on" here would be a
        // caption describing something that has already happened.
        let (_, on) = block1(&cfg(true, false, Some("127.0.0.1:4242")));
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
            let (body, _) = block1(&c);
            assert!(body.contains(name), "{name} should be named: {body}");
        }
        let mut c = cfg(true, true, None);
        c.extended_view.yaw.output_max_deg = 33.3;
        let (body, _) = block1(&c);
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
        let (body, _) = block1(&cfg(true, false, Some("127.0.0.1:4242")));
        assert!(!body.contains("is off"), "{body}");
        assert!(
            body.contains("127.0.0.1:4242"),
            "it names where it is going: {body}"
        );

        let (body, _) = block1(&cfg(true, false, None));
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
        let (text, action) = block2(&BridgeState::NoPrefix, &missing, &[]);
        assert!(text.contains("/mnt/games2"), "{text}");
        assert!(text.contains("cannot rule out"), "{text}");
        assert_eq!(action, None, "there is nothing to install into");
    }

    /// The other half. Without it the test above is satisfied by appending the
    /// sentence always, which would name a library on every machine.
    #[test]
    fn a_missing_prefix_with_every_library_present_invents_no_library() {
        let (text, _) = block2(&BridgeState::NoPrefix, &[], &[]);
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
        let (text, action) = block2(
            &BridgeState::Files {
                dir: prefix.join(BRIDGE_SUBDIR),
                prefix,
            },
            &[],
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
        let (no_prefix, _) = block2(&BridgeState::NoPrefix, &[], &[]);
        let (absent, _) = block2(
            &BridgeState::Absent {
                prefix: prefix.clone(),
            },
            &[],
            &[],
        );
        let (files, _) = block2(
            &BridgeState::Files {
                dir: prefix.join(BRIDGE_SUBDIR),
                prefix: prefix.clone(),
            },
            &[],
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

    /// `refuse_unverified_wine_for_steam`'s own words, as it writes them.
    /// Re-wording them here would be a second, worse account of a decision
    /// this window did not make.
    const REFUSAL: &str =
        "this is a Proton prefix — Steam found it for `--steam \"Elite Dangerous\"` — but \
         which Proton build made it could not be read out of it, so the only wine left is \
         /usr/bin/wine.\n\
         Refusing to run that one. A wine that is not the build a prefix was made with runs \
         `wineboot -u` against it before it does anything else and upgrades it.\n\
         Name the build the game uses:\n  \
         tobii bridge install --steam \"Elite Dangerous\" --wine <Proton>/files/bin/wine\n\
         `tobii bridge status --steam \"Elite Dangerous\"` runs nothing and names the prefix.";

    /// What the `tobii` on this machine's `$PATH` — 0.3.0 — prints for a
    /// subcommand it does not have, byte for byte, **and it exits zero**.
    /// Measured by running it.
    const OLD_USAGE: &str = "error: usage: tobii bridge games|install|run|uninstall \n  \
                             [--steam <app id or name> | --prefix PATH] [--wine PATH] \
                             [--artifacts DIR] [--port PORT]\nunknown argument `status`\n";

    fn prefix() -> PathBuf {
        PathBuf::from("/games/compatdata/359320/pfx")
    }

    fn absent() -> BridgeState {
        BridgeState::Absent { prefix: prefix() }
    }

    fn present() -> BridgeState {
        BridgeState::Files {
            dir: prefix().join(BRIDGE_SUBDIR),
            prefix: prefix(),
        }
    }

    #[test]
    fn a_refusal_is_reported_in_the_installers_own_words() {
        let o = install_outcome(Some(1), "", REFUSAL);
        assert!(!o.ok);
        let text = outcome_text(&o, &absent());
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
        let text = outcome_text(&o, &present());
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

    /// An exit code is a claim. The one thing this window can check it against
    /// is the prefix it has just re-stated for the paragraph above, and when
    /// the two disagree the page must not say both.
    ///
    /// Reachable today: the `tobii` on this machine's `$PATH` is 0.3.0 and
    /// exits **zero** on an argument it does not know.
    #[test]
    fn an_exit_of_zero_over_an_empty_prefix_is_not_reported_as_an_install() {
        let o = install_outcome(Some(0), "", "");
        assert!(o.ok, "it did exit zero, and this window still says no");
        let text = outcome_text(&o, &absent());
        assert!(
            !text.contains("The bridge is installed"),
            "the files are not in the prefix that was just stated to be empty: {text}"
        );
        assert!(
            text.contains(BRIDGE_ARTIFACT),
            "and it names the file it looked for: {text}"
        );
        assert!(
            text.contains(&prefix().join(BRIDGE_SUBDIR).display().to_string()),
            "and where: {text}"
        );

        // The same code over a prefix that does hold the files is the ordinary
        // success, which is what keeps the test above from being satisfied by
        // never saying it.
        let good = outcome_text(&o, &present());
        assert!(good.contains("The bridge is installed."), "{good}");

        // And a prefix that vanished is neither of those two sentences.
        let gone = outcome_text(&o, &BridgeState::NoPrefix);
        assert!(!gone.contains("The bridge is installed"), "{gone}");
        assert_ne!(gone, text);
    }

    /// A `tobii` with no such command is its own answer, not a success and not
    /// a wall of usage text with nothing above it.
    #[test]
    fn a_tobii_too_old_for_the_command_is_neither_a_success_nor_a_mystery() {
        // Exactly what 0.3.0 does: exit zero, with its usage on stderr.
        let o = install_outcome(Some(0), "", OLD_USAGE);
        assert!(o.ok);
        assert!(
            too_old_for("status", &o),
            "the subcommand is named back as unknown"
        );
        assert!(
            !too_old_for("install", &o),
            "that version does have `bridge install`, and its usage line lists it"
        );

        let note = too_old_note("status", Path::new("/home/x/.local/bin/tobii"));
        assert!(
            note.contains("/home/x/.local/bin/tobii"),
            "it names which program: {note}"
        );
        assert!(note.contains("no `bridge status`"), "{note}");

        // An install that exits zero having printed that usage says so, rather
        // than being reported through the prefix stat as a mystery.
        let installish = install_outcome(
            Some(0),
            "",
            "error: usage: tobii bridge games|run|uninstall\nunknown argument `install`\n",
        );
        let text = outcome_text(&installish, &absent());
        assert!(!text.contains("The bridge is installed"), "{text}");
        assert!(text.contains("no such command"), "{text}");
        assert!(
            text.contains("unknown argument `install`"),
            "and the evidence is still printed: {text}"
        );
    }

    /// The button re-runs the command with `--wine` added. A usage block lists
    /// every flag the command takes, so a substring test offered it for every
    /// mistyped command — and pressing it re-ran the same failure.
    #[test]
    fn a_usage_block_that_merely_lists_the_wine_flag_offers_no_proton_button() {
        assert!(
            OLD_USAGE.contains("[--wine PATH]"),
            "the fixture has to carry the flag, or this test asserts nothing"
        );
        let usage = install_outcome(Some(1), "", OLD_USAGE);
        assert!(!usage.ok);
        assert!(
            !offers_wine_choice(&usage),
            "naming the flag is not asking for a Proton build: {OLD_USAGE}"
        );

        let refused = install_outcome(Some(1), "", REFUSAL);
        assert!(
            offers_wine_choice(&refused),
            "and the refusal that does ask for one still earns the button"
        );
    }

    #[test]
    fn a_killed_installer_does_not_read_as_success() {
        let o = install_outcome(None, "", "");
        assert!(!o.ok);
        let text = outcome_text(&o, &absent());
        assert!(!text.contains("is installed"), "{text}");
        assert!(text.contains("killed"), "{text}");
    }

    /// "It said:" is a colon pointing at a gap when the child printed nothing.
    #[test]
    fn a_failure_with_nothing_to_quote_does_not_announce_a_quotation() {
        let o = install_outcome(Some(1), "", "");
        let text = outcome_text(&o, &absent());
        assert!(text.contains("exit status 1"), "{text}");
        assert!(
            !text.trim_end().ends_with("It said:"),
            "nothing follows the colon, so there is no colon: {text}"
        );
        assert!(text.contains("printed nothing"), "{text}");

        // And a failure that did print something still announces it.
        let spoke = install_outcome(Some(1), "", "could not open the prefix");
        let text = outcome_text(&spoke, &absent());
        assert!(text.contains("It said:"), "{text}");
        assert!(text.contains("could not open the prefix"), "{text}");
    }

    /// A failure part way through a copy is not a failure that changed
    /// nothing, and this window only watched — so it claims neither.
    #[test]
    fn a_failed_install_does_not_claim_the_prefix_is_untouched() {
        for o in [
            install_outcome(Some(1), "", REFUSAL),
            install_outcome(None, "", ""),
        ] {
            let text = outcome_text(&o, &absent());
            assert!(
                !text.contains("Nothing in the prefix was changed"),
                "this window did not watch closely enough to say that: {text}"
            );
        }
    }

    /// The hub and `tobii` are separate programs on separate versions, and
    /// which one these buttons found is the premise of everything block 2
    /// reports.
    #[test]
    fn the_page_names_the_program_its_buttons_run() {
        let line = binary_line(Path::new("/home/x/.local/bin/tobii"));
        assert!(line.contains("/home/x/.local/bin/tobii"), "{line}");
        assert_ne!(
            line,
            binary_line(Path::new("/usr/bin/tobii")),
            "a line that named no path would be the same for both"
        );
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

    // ------------------------------------------------------------ the census

    /// The line under the list is the only place this window says what the
    /// list is a list of, and it is also where an unmounted drive is named.
    #[test]
    fn the_census_counts_the_titles_and_warns_only_when_a_library_is_gone() {
        let apps = [app("1", "One"), app("2", "Two")];
        let quiet = picker(&apps, &[], &none, "");
        assert!(
            quiet.census.starts_with("2 titles"),
            "it counts every installed title, not the filtered rows: {}",
            quiet.census
        );
        assert!(!quiet.census_warn, "{}", quiet.census);

        // Filtered down to one row, and still a census of two.
        let filtered = picker(&apps, &[], &none, "One");
        assert_eq!(filtered.rows.len(), 1);
        assert_eq!(
            filtered.census, quiet.census,
            "the census is of the machine, not of the search"
        );

        let one = picker(&apps[..1], &[], &none, "");
        assert!(
            one.census.starts_with("1 title"),
            "singular: {}",
            one.census
        );

        let warned = picker(&apps, &[PathBuf::from("/mnt/games2")], &none, "");
        assert!(
            warned.census_warn,
            "a library Steam names and this machine does not have is what the warning is \
             for: {}",
            warned.census
        );
        assert!(warned.census.contains("/mnt/games2"), "{}", warned.census);
        assert!(
            warned.census.contains("2 titles"),
            "and it still counts: {}",
            warned.census
        );
    }

    /// The page an empty machine opens on. It has no list, no search box and
    /// no rows, so every sentence on it has to carry itself.
    #[test]
    fn the_empty_machine_page_says_why_and_names_a_library_that_is_gone() {
        let quiet = nothing_text(&[]);
        assert!(
            quiet.contains("No installed Steam games were found"),
            "{quiet}"
        );
        assert!(
            quiet.contains("manifest"),
            "and why this program might not see them: {quiet}"
        );
        assert!(
            !quiet.contains("cannot rule out"),
            "no library is missing, so none is invented: {quiet}"
        );

        let warned = nothing_text(&[PathBuf::from("/mnt/games2")]);
        assert!(warned.contains("/mnt/games2"), "{warned}");
        assert!(warned.contains("cannot rule out"), "{warned}");
        assert_ne!(quiet, warned);
    }

    // ------------------------------------------------- the joystick's reality

    /// `games.rs` reports the joystick "from what the device thread actually
    /// did, never from the checkbox… a line that says 'sending to a virtual
    /// joystick' when none exists is exactly the support thread this row was
    /// written to prevent". This window sits in front of that card.
    #[test]
    fn a_joystick_that_could_not_be_created_is_not_a_destination_here_either() {
        let failed = JoystickStatus::Failed("/dev/uinput: Permission denied".to_string());
        let c = cfg(true, true, None);

        let (body, caption) = settings_block(&c, profiles::Bridge::Unstated, &failed);
        assert!(
            !body.contains("sending to a virtual joystick"),
            "the card behind this window says it could not be created: {body}"
        );
        assert!(
            !body.contains("Nothing to change here"),
            "something is very much wrong here: {body}"
        );
        assert!(body.contains("/dev/uinput: Permission denied"), "{body}");
        assert_eq!(
            caption, None,
            "and there is still no button that fixes /dev/uinput: {body}"
        );

        // The same configuration with a joystick the device thread made.
        let (ok, _) = settings_block(&c, profiles::Bridge::Unstated, &JoystickStatus::Present);
        assert!(ok.contains("sending to a virtual joystick"), "{ok}");
        assert!(ok.contains("Nothing to change here"), "{ok}");

        // And `Off` is not a failure: it means nobody has asked yet, which for
        // a window about what is *configured* is not a fault.
        let (off, _) = settings_block(&c, profiles::Bridge::Unstated, &JoystickStatus::Off);
        assert_eq!(off, ok, "not asked for yet is not could not be created");
    }

    // -------------------------------------------- the button and its caption

    /// The module's own doctrine: a button must not do more than its caption
    /// says. This one sets `enabled`, and `enabled` is the switch every other
    /// destination hangs off.
    #[test]
    fn turning_it_on_names_every_destination_the_press_will_start() {
        let mut c = cfg(false, false, Some("127.0.0.1:4242"));
        c.bridge_port = Some(4243);

        let (body, caption) = block1(&c);
        assert!(
            body.contains("127.0.0.1:4242") && body.contains("4243"),
            "the destinations already in the file are named before the switch is touched: \
             {body}"
        );
        let caption = caption.expect("there is a switch to turn on");
        assert!(caption.contains("virtual joystick"), "{caption}");
        assert!(
            caption.contains("127.0.0.1:4242"),
            "pressing this starts opentrack too, so the caption says so: {caption}"
        );
        assert!(caption.contains("4243"), "and the Wine bridge: {caption}");

        // What the press leaves behind, worded by the same function: the
        // caption is that list and nothing less.
        let mut after = c.clone();
        after.enabled = true;
        after.joystick = true;
        let (sinks, _) = destinations(&after, &JoystickStatus::Off);
        for sink in &sinks {
            assert!(
                caption.contains(sink.as_str()),
                "the press starts {sink} and the caption does not say so: {caption}"
            );
        }

        // A machine with nothing else configured reads exactly as it did.
        let (_, plain) = block1(&cfg(false, false, None));
        assert_eq!(
            plain.as_deref(),
            Some("Turn on and send to a virtual joystick")
        );
    }

    /// The page cannot claim ignorance in block 1 that block 2 contradicts
    /// three paragraphs later from the same profile.
    #[test]
    fn a_profile_that_answers_the_bridge_question_is_not_also_reported_as_unanswered() {
        let c = cfg(false, false, None);
        let (unstated, _) = settings_block(&c, profiles::Bridge::Unstated, &JoystickStatus::Off);
        assert!(
            unstated.contains("not something this program knows"),
            "with nobody having said, this is the honest sentence: {unstated}"
        );

        let (required, _) = settings_block(&c, profiles::Bridge::Required, &JoystickStatus::Off);
        assert!(
            !required.contains("not something this program knows"),
            "block 2 is about to say, from this very profile, that the joystick will not \
             reach this game: {required}"
        );
        assert!(
            required.contains("TrackIR"),
            "and it says what the profile said: {required}"
        );
        // The two blocks are reading one profile, so they must not disagree.
        let note = profile_bridge_note(profiles::Bridge::Required).expect("Required says so");
        assert!(note.contains("will not reach it"), "{note}");

        let (not_needed, _) = settings_block(&c, profiles::Bridge::NotNeeded, &JoystickStatus::Off);
        assert!(
            !not_needed.contains("not something this program knows"),
            "{not_needed}"
        );
        assert_ne!(required, not_needed);
        assert_ne!(required, unstated);
    }

    // ------------------------------------------------------ the other prefix

    /// `tobii_steam::prefix`'s own warning, made visible: "installing into the
    /// abandoned one succeeds, prints the ordinary success text, and does
    /// nothing at all for the game". Nothing else on the page hints there are
    /// two — `apps` collapses the rows and blanks the build id.
    #[test]
    fn a_title_with_two_prefixes_is_not_offered_one_of_them_without_saying_so() {
        let here = PathBuf::from("/home/u/.steam/steam/steamapps/compatdata/359320/pfx");
        let there = PathBuf::from("/mnt/games2/steamapps/compatdata/359320/pfx");

        let (alone, _) = block2(
            &BridgeState::Absent {
                prefix: here.clone(),
            },
            &[],
            &[],
        );
        assert!(
            !alone.contains("/mnt/games2"),
            "one prefix invents no second one: {alone}"
        );
        assert!(
            !alone.contains("other Proton"),
            "and says nothing about others: {alone}"
        );

        let (two, action) = block2(
            &BridgeState::Absent { prefix: here },
            &[],
            &[(there.clone(), false)],
        );
        assert!(two.contains(&there.display().to_string()), "{two}");
        assert!(two.contains("Move Install Folder"), "{two}");
        assert_eq!(
            action,
            Some(Action::Install),
            "the button stays — the named prefix is still the one an install writes"
        );

        // And when the OTHER one already holds the bridge, that is the fact
        // most worth knowing before pressing anything.
        let (installed_there, _) = block2(
            &BridgeState::Absent {
                prefix: PathBuf::from("/home/u/.steam/steam/steamapps/compatdata/359320/pfx"),
            },
            &[],
            &[(there.clone(), true)],
        );
        assert!(installed_there.contains("already in"), "{installed_there}");
        assert_ne!(installed_there, two);
    }

    /// The count in the sentence and the files the installer copies are one
    /// fact, so the sentence takes it from the list.
    #[test]
    fn the_install_paragraph_counts_the_files_it_will_copy() {
        let (text, _) = block2(
            &BridgeState::Absent {
                prefix: PathBuf::from("/p/pfx"),
            },
            &[],
            &[],
        );
        assert!(
            BRIDGE_FILES.contains(&BRIDGE_ARTIFACT),
            "the file block 2 stats has to be one of the files an install copies"
        );
        assert!(
            text.contains(&format!("{} small files", BRIDGE_FILES.len())),
            "the number comes from the list: {text}"
        );
        for f in BRIDGE_FILES {
            assert!(text.contains(f), "{f} is copied and is not named: {text}");
        }
        assert!(
            !text.contains("two small files"),
            "the installer copies three and prints `copied 3 file(s)`: {text}"
        );
    }

    // ---------------------------------------------------------- the remedies

    /// The commonest real case is a player who has never touched the setting,
    /// and that answer used to read in full as `HeadlookMode: not set.` —
    /// nothing expected, nothing to do. `tobii games profile show` prints both
    /// under every check, whatever the file held.
    #[test]
    fn every_answer_but_a_match_says_what_is_expected_and_what_to_do() {
        let answers = [
            Answer::NoPrefix,
            Answer::UnknownFormat("binds-v2".to_string()),
            Answer::Unwritten("there is no directory at /p/Bindings".to_string()),
            Answer::Unreadable("/p/Bindings could not be listed".to_string()),
            Answer::Absent,
            Answer::NotReadBack("a character this reader cannot decode".to_string()),
            Answer::Text("0".to_string()),
        ];
        for a in answers {
            let text = row_text(&row(a.clone()));
            assert!(
                text.contains("expects 1"),
                "{a:?} leaves the reader without the value: {text}"
            );
            assert!(
                text.contains("In Controls, set Head Look to Toggle."),
                "{a:?} leaves the reader without an instruction: {text}"
            );
        }

        // The one answer that earns neither: it is already right.
        let good = row_text(&Row {
            answer: Answer::Text("1".to_string()),
            ..row(Answer::Absent)
        });
        assert!(
            !good.contains("In Controls"),
            "telling somebody to set what is set is how a report stops being read: {good}"
        );

        // A profile with a blank instruction still states the expectation, and
        // leaves no dangling space behind it.
        let blank = row_text(&Row {
            tell: "   ".to_string(),
            ..row(Answer::Absent)
        });
        assert!(blank.contains("expects 1"), "{blank}");
        assert_eq!(blank.trim_end(), blank, "no trailing gap: {blank:?}");
    }

    /// The verb agrees with the count as well as the noun.
    #[test]
    fn one_superseded_start_file_is_one_file_that_was_not_read() {
        let one = binds_note(&binds::Bindings {
            start: PathBuf::from("/p/StartPreset.4.start"),
            schema: Some(4),
            superseded: vec![PathBuf::from("/p/StartPreset.3.start")],
            presets: Vec::new(),
        })
        .expect("a superseded start file is worth saying out loud");
        assert!(
            one.contains("1 older start file next to it was not read"),
            "{one}"
        );
        assert!(
            one.contains("that one is the live one"),
            "the other half of the same sentence counts too: {one}"
        );

        let two = binds_note(&binds::Bindings {
            start: PathBuf::from("/p/StartPreset.4.start"),
            schema: Some(4),
            superseded: vec![
                PathBuf::from("/p/StartPreset.3.start"),
                PathBuf::from("/p/StartPreset.2.start"),
            ],
            presets: Vec::new(),
        })
        .expect("two of them likewise");
        assert!(
            two.contains("2 older start files next to it were not read"),
            "{two}"
        );
        assert!(two.contains("one of those is the live one"), "{two}");
    }

    // ------------------------------------------------------------ run_check

    /// A directory this test owns. Nothing here touches `$HOME`, `~/.steam` or
    /// a real prefix — see the note at the head of this module. The name
    /// carries the thread id because these run in parallel.
    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "tobii-gamesetup-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("scratch");
        p
    }

    fn check(format: profiles::Format, path: &str, setting: &str) -> profiles::Check {
        profiles::Check {
            format,
            path: path.to_string(),
            setting: setting.to_string(),
            wants: "1".to_string(),
            tell: "In Options, turn head tracking on.".to_string(),
        }
    }

    fn only(rows: Vec<Row>) -> Answer {
        assert_eq!(rows.len(), 1, "one row was expected: {rows:?}");
        rows.into_iter().next().expect("checked").answer
    }

    /// The only place `tobii-gameconf`'s three-way answers reach a screen.
    ///
    /// Each of the three is produced from a real file on a real disk, because
    /// the distinction this asserts is exactly the one a reader loses by
    /// looking at the code: *nothing was ever written here*, *something is
    /// here and cannot be read*, and *it was read and here is what it says*
    /// are three different sentences, and folding any two together is this
    /// project's signature bug.
    #[test]
    fn run_check_keeps_never_written_apart_from_cannot_be_read() {
        let prefix = scratch("run-check-attrs");
        let dir = prefix.join("drive_c/Options");
        std::fs::create_dir_all(&dir).expect("fixture");
        std::fs::write(
            dir.join("attributes.xml"),
            b"<Attributes Version=\"35\">\n \
              <Attr name=\"HeadtrackingSource\" value=\"1\"/>\n \
              <Attr name=\"Broken\" value=\"&nbsp;\"/>\n\
              </Attributes>\n",
        )
        .expect("fixture");

        let at = |path: &str, setting: &str| {
            only(
                run_check(
                    &check(profiles::Format::AttributesXml, path, setting),
                    Some(&prefix),
                )
                .0,
            )
        };

        // Read, and the value is there.
        assert_eq!(
            at("drive_c/Options/attributes.xml", "HeadtrackingSource"),
            Answer::Text("1".to_string())
        );

        // Read, and this document does not hold that name.
        assert_eq!(
            at("drive_c/Options/attributes.xml", "NotInHere"),
            Answer::Absent
        );

        // Read, and the value cannot be handed back as it stands.
        let back = at("drive_c/Options/attributes.xml", "Broken");
        match &back {
            Answer::NotReadBack(why) => assert!(why.contains("&nbsp;"), "{why}"),
            other => panic!("a value this reader will not decode is not {other:?}"),
        }

        // Nothing has ever been written here.
        let unwritten = at("drive_c/Options/never.xml", "HeadtrackingSource");
        match &unwritten {
            Answer::Unwritten(why) => assert!(why.contains("never.xml"), "{why}"),
            other => panic!("a file that was never written is not {other:?}"),
        }

        // Something is here and this program could not read it. A directory
        // where a document should be, because CI runs as root and a mode of
        // 000 is no obstacle to root.
        std::fs::create_dir_all(dir.join("adirectory.xml")).expect("fixture");
        let unreadable = at("drive_c/Options/adirectory.xml", "HeadtrackingSource");
        match &unreadable {
            Answer::Unreadable(why) => assert!(why.contains("adirectory.xml"), "{why}"),
            other => panic!("a path this program could not read is not {other:?}"),
        }

        // The two negatives are two sentences, on the screen as well as in the
        // enum — which is the whole claim.
        assert_ne!(unwritten, unreadable);
        let (a, b) = (row_text(&row(unwritten)), row_text(&row(unreadable)));
        assert!(a.contains("not found"), "{a}");
        assert!(b.contains("could not be read"), "{b}");
        assert!(
            !b.contains("not found"),
            "a path that could not be read says nothing about whether it holds a setting: {b}"
        );

        let _ = std::fs::remove_dir_all(&prefix);
    }

    /// The other two ways a check can end without any file being opened, and
    /// the directory format, which answers with one row per active preset.
    #[test]
    fn run_check_answers_a_preset_directory_and_refuses_to_guess_without_a_prefix() {
        // No prefix: not a setting that is unset, and no file is opened at all
        // — the path this would have built does not exist anywhere.
        assert_eq!(
            only(
                run_check(
                    &check(profiles::Format::AttributesXml, "drive_c/x.xml", "S"),
                    None
                )
                .0
            ),
            Answer::NoPrefix
        );

        // A format this build has no reader for is named and not attempted.
        assert_eq!(
            only(
                run_check(
                    &check(
                        profiles::Format::Unknown("binds-v2".to_string()),
                        "drive_c/x",
                        "S"
                    ),
                    Some(Path::new("/nonexistent"))
                )
                .0
            ),
            Answer::UnknownFormat("binds-v2".to_string())
        );

        let prefix = scratch("run-check-binds");
        let binds_dir = prefix.join("drive_c/Bindings");
        std::fs::create_dir_all(&binds_dir).expect("fixture");
        let preset = |name: &str, value: &str| {
            let mut out = b"\xef\xbb\xbf".to_vec();
            out.extend_from_slice(
                format!(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n\
                     <Root PresetName=\"{name}\" SortOrder=\"0\">\r\n\
                     \t<HeadlookMode Value=\"{value}\" />\r\n\
                     </Root>\r\n"
                )
                .as_bytes(),
            );
            out
        };
        std::fs::write(binds_dir.join("StartPreset.4.start"), b"Custom\r\n").expect("fixture");
        std::fs::write(binds_dir.join("StartPreset.3.start"), b"Old\r\n").expect("fixture");
        std::fs::write(binds_dir.join("Custom.binds"), preset("Custom", "1")).expect("fixture");

        let (rows, note) = run_check(
            &check(
                profiles::Format::BindsDir,
                "drive_c/Bindings",
                "HeadlookMode",
            ),
            Some(&prefix),
        );
        assert_eq!(rows.len(), 1, "one active preset, one row: {rows:?}");
        assert_eq!(rows[0].answer, Answer::Text("1".to_string()));
        assert_eq!(
            rows[0].preset.as_deref(),
            Some("Custom"),
            "the row names which preset it read"
        );
        let note = note.expect("an older start file next to the live one is worth saying");
        assert!(note.contains("StartPreset.3.start"), "{note}");
        assert!(note.contains("rolled back"), "{note}");

        // A directory that has never been written is not a setting that is
        // unset — the same rule as the file format, one level up.
        let missing = only(
            run_check(
                &check(profiles::Format::BindsDir, "drive_c/None", "HeadlookMode"),
                Some(&prefix),
            )
            .0,
        );
        match &missing {
            Answer::Unwritten(why) => assert!(why.contains("drive_c/None"), "{why}"),
            other => panic!("a directory nothing wrote is not {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&prefix);
    }

    /// A profile is a file a user may have been handed by a stranger, and the
    /// path it names is joined onto a Proton prefix and opened. Two halves,
    /// and this window owns both: the whole file is refused when a path could
    /// climb out, and what the parser does accept is opened under the prefix
    /// and nowhere else.
    #[test]
    fn this_window_opens_nothing_outside_the_prefix_it_was_given() {
        let root = scratch("stays-inside");
        let prefix = root.join("pfx");
        std::fs::create_dir_all(prefix.join("drive_c")).expect("fixture");
        std::fs::write(
            prefix.join("drive_c/here.xml"),
            b"<Attributes><Attr name=\"S\" value=\"inside\"/></Attributes>",
        )
        .expect("fixture");
        // The same name one level up: what a path built by walking out of the
        // prefix would find instead. Inside this test's own directory, which
        // is the only place it writes.
        std::fs::create_dir_all(root.join("drive_c")).expect("fixture");
        std::fs::write(
            root.join("drive_c/here.xml"),
            b"<Attributes><Attr name=\"S\" value=\"leaked\"/></Attributes>",
        )
        .expect("fixture");

        let profile = |path: &str| {
            format!(
                "version = 1\n\n[[check]]\nformat = \"attributes-xml\"\npath = \"{path}\"\n\
                 setting = \"S\"\nwants = \"1\"\ntell = \"t\"\n"
            )
        };

        // Half one: nothing that could climb out survives being parsed, and
        // the whole file goes rather than the one check — which is this
        // window's sentence to write, not the parser's.
        for bad in ["../drive_c/here.xml", "/etc/passwd", "drive_c\\users"] {
            let error = profiles::parse(&profile(bad)).expect_err(
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

        // Half two: what the parser accepts, this window opens under the
        // prefix it was given. `.` and empty segments are not `..`, so they
        // pass the parser and land here — and the answer has to come out of
        // the copy inside, never the one beside it.
        for odd in [
            "drive_c/here.xml",
            "./drive_c/./here.xml",
            "drive_c//here.xml",
        ] {
            let p = profiles::parse(&profile(odd))
                .unwrap_or_else(|e| panic!("{odd} has no `..`, no leading `/`, no `\\`: {e}"));
            let c = p.checks.first().expect("one check").clone();
            assert!(
                c.path_under(&prefix).starts_with(&prefix),
                "{odd}: {:?}",
                c.path_under(&prefix)
            );
            let answer = only(run_check(&c, Some(&prefix)).0);
            assert_eq!(
                answer,
                Answer::Text("inside".to_string()),
                "{odd} was read somewhere other than under the prefix"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    // ------------------------------------------------------- whose answer

    fn ran(appid: &str) -> (String, String) {
        (
            appid.to_string(),
            format!("/usr/bin/tobii bridge install --steam {appid}"),
        )
    }

    /// A job outlives the page it was started from. Press *Install the
    /// bridge*, press Back, pick another game: the install finishes while the
    /// second game is showing, and its "Running now:" line and then its
    /// success landed on the second game's page. Clearing the slots on a page
    /// change cannot fix that — the answer had not arrived yet.
    #[test]
    fn an_install_reports_itself_on_the_game_it_was_started_for_and_no_other() {
        let running = ran("359320");
        let here = job_view("359320", Some(&running), None, None, &absent());
        assert!(here.text.contains("Running now:"), "{}", here.text);
        assert!(here.text.contains("--steam 359320"), "{}", here.text);

        let there = job_view("220", Some(&running), None, None, &absent());
        assert!(
            !there.text.contains("Running now:"),
            "this is not this game's install: {}",
            there.text
        );
        assert!(
            there.text.contains("359320"),
            "and the grey buttons are explained rather than left a mystery: {}",
            there.text
        );
        assert!(
            !there.enabled && !here.enabled,
            "one child at a time, whichever game it was started for"
        );

        // The same for the answer that comes back.
        let done = ("359320".to_string(), install_outcome(Some(0), "", ""));
        let mine = job_view("359320", None, Some(&done), None, &present());
        assert!(
            mine.text.contains("The bridge is installed."),
            "{}",
            mine.text
        );
        let theirs = job_view("220", None, Some(&done), None, &present());
        assert!(
            !theirs.text.contains("The bridge is installed"),
            "another game's install is not this game's: {}",
            theirs.text
        );
        assert!(theirs.enabled, "nothing is running any more");

        // And for what Details printed.
        let report = ("359320".to_string(), "the prefix is at /p/pfx".to_string());
        assert_eq!(
            job_view("359320", None, None, Some(&report), &present())
                .report
                .as_deref(),
            Some("the prefix is at /p/pfx")
        );
        assert_eq!(
            job_view("220", None, None, Some(&report), &present()).report,
            None,
            "a report about another game's prefix is not this game's"
        );

        // The Proton-build button is earned by a refusal, and by this game's.
        let refused = ("359320".to_string(), install_outcome(Some(1), "", REFUSAL));
        assert!(job_view("359320", None, Some(&refused), None, &absent()).offer_wine);
        assert!(
            !job_view("220", None, Some(&refused), None, &absent()).offer_wine,
            "another game's refusal does not put a button on this page"
        );

        // Nothing at all to say is nothing at all said.
        let idle = job_view("359320", None, None, None, &absent());
        assert_eq!(idle.text, "");
        assert!(idle.enabled);
    }

    /// The premise of block 2: there has to be something to run. A button that
    /// can only fail is worse than a sentence saying what to type.
    #[test]
    fn a_machine_with_no_tobii_gets_no_button_and_a_command_to_type() {
        let state = BridgeState::Absent {
            prefix: PathBuf::from("/p/pfx"),
        };
        let (text, action) = bridge_block(&state, &[], &[], None, "359320");
        assert_eq!(action, None, "nothing to run it with: {text}");
        assert!(
            text.contains("tobii bridge install --steam 359320"),
            "{text}"
        );

        let (with, action) = bridge_block(
            &state,
            &[],
            &[],
            Some(Path::new("/home/x/.local/bin/tobii")),
            "359320",
        );
        assert_eq!(action, Some(Action::Install));
        assert!(
            with.contains("/home/x/.local/bin/tobii"),
            "which program these buttons run is the premise of the rest: {with}"
        );
    }

    /// `other_prefixes` off a Steam tree this test builds: two libraries, one
    /// title, a prefix in each. The manifest decides which one an install
    /// writes to, and the other is what nothing on the page would otherwise
    /// mention.
    #[test]
    fn a_second_library_with_a_second_prefix_is_found_and_the_manifest_picks_the_first() {
        let home = scratch("two-libraries");
        let one = home.join(".steam/steam");
        let two = home.join("games2");
        for lib in [&one, &two] {
            std::fs::create_dir_all(lib.join("steamapps/compatdata/359320/pfx/drive_c"))
                .expect("fixture");
        }
        std::fs::write(
            one.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n\
                 \t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                one.display(),
                two.display()
            ),
        )
        .expect("fixture");
        // The manifest lives in the SECOND library, so that is the one
        // `tobii_steam::prefix` picks — and the first is the abandoned one.
        std::fs::write(
            two.join("steamapps/appmanifest_359320.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"359320\"\n\t\"name\"\t\t\"Elite Dangerous\"\n}\n",
        )
        .expect("fixture");
        // And the bridge is installed in the abandoned one, which is exactly
        // the trap `tobii_steam::prefix` documents.
        let stale = one.join("steamapps/compatdata/359320/pfx");
        std::fs::create_dir_all(stale.join(BRIDGE_SUBDIR)).expect("fixture");
        std::fs::write(stale.join(BRIDGE_SUBDIR).join(BRIDGE_ARTIFACT), b"x").expect("fixture");

        let chosen = tobii_steam::prefix(&home, "359320").expect("both prefixes exist");
        assert!(
            chosen.starts_with(&two),
            "the library holding the manifest owns the prefix: {chosen:?}"
        );

        let others = other_prefixes(&home, "359320", Some(&chosen));
        assert_eq!(others.len(), 1, "the other library's prefix: {others:?}");
        assert!(others[0].0.starts_with(&one), "{others:?}");
        assert!(
            others[0].1,
            "and this window knows the bridge is in that one: {others:?}"
        );
        assert!(
            !others.iter().any(|(p, _)| *p == chosen),
            "the chosen prefix is not one of the others"
        );

        // A title with one prefix has no others, which is what keeps the note
        // from being appended to every page.
        std::fs::create_dir_all(two.join("steamapps/compatdata/220/pfx/drive_c")).expect("fixture");
        let only_one = tobii_steam::prefix(&home, "220").expect("one prefix");
        assert!(other_prefixes(&home, "220", Some(&only_one)).is_empty());

        let _ = std::fs::remove_dir_all(&home);
    }
}
