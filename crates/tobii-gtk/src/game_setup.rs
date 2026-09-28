//! The hub's **Games** tab: pick an installed game, and see the three things
//! that have to be configured for it.
//!
//! # The three things
//!
//! 1. **This program's own settings** — game output on, and a destination to
//!    send to. **Reported here and edited nowhere in this file.** The one
//!    editor of them is the games card on the Tracker tab: every control on
//!    that card is global — there is no such thing as "the joystick, for Elite
//!    Dangerous" — so a per-game page that also wrote them was one setting with
//!    two editors, which is what the modal this tab replaces was modal to
//!    prevent. The race is deleted rather than blocked.
//! 2. **The Wine bridge** — the TrackIR/FreeTrack registration inside the
//!    game's Proton prefix. Installed by shelling out to `tobii bridge install
//!    --steam <appid>` and showing that program's words verbatim. The one
//!    thing on this tab that changes anything.
//! 3. **The game's own configuration files** — read-only, always, and only
//!    where a profile says what to read. This program never writes them.
//!
//! # What this tab may not do
//!
//! It takes **no claim on the tracker**, and after the merge into the hub that
//! is a stronger statement than it was: a modal window took the focus off the
//! hub and dropped the hub's own claim as a side effect, where a tab does not.
//! So the hub holds `"the hub window"` only while the **Tracker** tab is
//! showing — see `crate::build_hub`'s tick — and nothing in here calls
//! `crate::hold_while_open` or takes a `DemandGuard`. The argument is
//! [`crate::help`]'s, word for word: lighting the illuminators to show
//! somebody a paragraph is precisely the behaviour the whole demand mechanism
//! exists to prevent. `tests/games_tab.rs` asserts it against the device
//! thread's own list of claim reasons.
//!
//! It never spawns `wine`, on any path, and it never passes `--force` to the
//! installer — see [`install_argv`].
//!
//! # The one thing it writes
//!
//! Its own list of games added by hand — a name and a Wine prefix each, in
//! this program's config directory, through [`tobii_config::custom_games`].
//! That is not a hole in the two promises above and it is worth saying why:
//! it is not a game's file, and it is not a setting with an editor on the
//! other tab. It is the list this page is drawn from, and the only way a game
//! Steam has never heard of can be on it. `tobii uninstall --purge` knows its
//! name.
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
//! Every sentence this tab shows is produced by a function that takes its
//! inputs and returns a `String`, so it can be asserted in CI, which has no
//! display. The widgets only show what those functions said. The one function
//! that touches the filesystem on its own account is [`scan`], whose `home` is
//! a parameter rather than a read of `$HOME` for the reason `bridge.rs` gives
//! about its own split: `$HOME` is process-global and these tests run in
//! parallel. No test in this file calls it.
//!
//! # Why there is no timer
//!
//! The `refresh` closure in [`build_with`] recomputes the whole detail pane
//! from disk and rewrites every label. It runs on a selection change and when
//! a subprocess this tab started finishes — and **not** on the 33 ms hub tick
//! that drives [`crate::games::GamesRow::refresh`]. That card has to follow a
//! file a terminal can edit under it. This tab stats prefixes and reads
//! profiles, which is not work to do thirty times a second for a pane that is
//! usually not even showing, and the installer it started reports itself.
//!
//! It does read one fact it does not own: what the device thread made of the
//! virtual joystick ([`crate::device::JoystickStatus`]). That is read on each
//! refresh and never polled, so a joystick that fails a second after this page
//! is drawn is reported on the next pass and not before. The games card on the
//! Tracker tab is the live account of what is happening, and it is the one on
//! the 33 ms tick.

use gtk::prelude::*;
use gtk::{glib, Align, Label, Orientation};

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tobii_config::{profiles, signature};
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
///
/// Only [`BRIDGE_ARTIFACT`] is `bridge.rs`' `REQUIRED_ARTIFACT`. The installer
/// prints `note: <name> not built yet — skipping` for either of the other two
/// and carries on, and `tobii bridge status` has a `missing (optional)` line
/// for that state — so a prefix holding one of the three is an ordinary
/// outcome and not a corruption. It is also why nothing here says "the
/// bridge's files" off a stat of one of them: what a stat supports is the set
/// it actually looked at, which is [`present_files`].
const BRIDGE_SUBDIR: &str = "drive_c/tobii-bridge";
const BRIDGE_FILES: [&str; 3] = [
    "tobii-bridge.exe",
    "freetrackclient64.dll",
    "NPClient64.dll",
];
const BRIDGE_ARTIFACT: &str = "freetrackclient64.dll";

/// The heading over the detail pane before a game has been picked.
///
/// It was the modal's window title, and it is the one line on this tab that
/// says what the tab is for to somebody who has just landed on it.
const HEADING: &str = "Set up a game";

/// The search box, so a test can find it without matching on text.
///
/// `pub(crate)` and not `pub`: these were exported for a display test that
/// went with the modal, and nothing outside this crate has referred to either
/// since. A name in a crate's public API on the strength of a caller that does
/// not exist is a promise nothing is holding — `tests/games_tab.rs` finds the
/// tab stack through [`crate::HUB_STACK_NAME`], which IS `pub` and IS used.
pub(crate) const SEARCH_NAME: &str = "gamesetup-search";
/// The list of games, likewise.
pub(crate) const LIST_NAME: &str = "gamesetup-games";

/// How wide the list of games asks to be, and how wide the detail pane does.
///
/// Neither is what they end up at — both panes expand — and neither may set
/// the hub's opening width. `crate::build_hub` derives `default_width` from the
/// control rack alone, so this tab cannot raise it; what these floors have to
/// do is stay comfortably *under* it, because GTK warns and clips when a
/// stack's non-visible child measures a minimum wider than the window it is
/// in. Their sum plus the gap is about 700px against the rack's 1152px.
const LIST_WIDTH: i32 = 300;
const DETAIL_WIDTH: i32 = 380;

// ------------------------------------------------------------------- the scan

/// Everything this window reads off the machine before it draws anything.
///
/// Injected rather than read inside [`build_with`] so the whole tab can be
/// opened in a test against a synthetic library: CI runs as root with no real
/// `$HOME` and no Steam install, and a window that read `$HOME` itself could
/// only ever be tested on somebody's laptop.
#[derive(Debug, Clone)]
pub struct Scan {
    /// Where per-game profiles are read from.
    pub profiles_dir: PathBuf,
    /// What Steam says is installed, in the order [`tobii_steam::apps`] gives.
    pub apps: Vec<App>,
    /// The walk `apps` and `missing` were taken off, kept so that every later
    /// prefix answer comes from the same one.
    ///
    /// Without it the window built a second `Steam` at open, so the two halves
    /// of the census came from one walk and every `no Proton prefix yet`,
    /// `bridge_state` and `other_prefixes` answer on the page came from
    /// another — the disagreement this type's own doc says taking them from
    /// one value prevents.
    pub steam: Rc<tobii_steam::Steam>,
    /// What [`tobii_steam::Steam::prefix`] answered for an app id, kept.
    ///
    /// `prefix` is NOT an in-memory lookup: it stats an `appmanifest_<id>.acf`
    /// in each present library and then walks the candidates again looking for
    /// a `drive_c`, so a title never launched under Proton pays every one and
    /// finds nothing. `Catalog::new` asks it of every installed title, and
    /// `read_catalog` runs on every `map` of the tab — so the answer was
    /// re-derived for 29 titles every time somebody clicked Games. The comment
    /// on `Catalog` already claimed this machine's prefixes were "a walk each
    /// and deliberately paid once"; this is what makes that true. A prefix
    /// appears when a game is first launched — which this program never does,
    /// but the user does, and is told to. [`Scan::forget_prefixes`] is what
    /// makes the cache a within-one-read memo rather than a claim for the life
    /// of the process.
    prefix_cache: RefCell<std::collections::HashMap<String, Option<PathBuf>>>,
    /// Where the hand-added games are kept.
    ///
    /// A path and not the list, because the list is re-read every time the tab
    /// comes into view and the tab can write it. Here for the reason
    /// `profiles_dir` is: a tab that read `$XDG_CONFIG_HOME` itself could only
    /// be tested on somebody's laptop, and this is the one file on this tab
    /// that is written as well as read.
    pub custom_games: PathBuf,
}

/// Read the machine.
///
/// `home` is a parameter, not `$HOME`: see the module docs.
///
/// The two Steam answers come off one [`tobii_steam::Steam`] rather than one
/// each. That is not only a walk saved: what is installed and which library
/// could not be looked in are two halves of one census, and taking them from
/// one value is what stops them naming different libraries.
pub fn scan(home: &Path, profiles_dir: &Path) -> Scan {
    let steam = Rc::new(tobii_steam::Steam::at(home));
    let apps = steam.apps();
    Scan::of(
        profiles_dir.to_path_buf(),
        apps,
        steam,
        tobii_config::custom_games::path(),
    )
}

impl Scan {
    /// A scan assembled from parts.
    ///
    /// The one way to build one, because [`Scan::prefix_cache`] is an
    /// implementation detail that has to start empty and a struct literal
    /// would make it somebody else's to get right — which is also why the
    /// field is private. `scan` uses this, and so does the display test that
    /// builds a synthetic machine.
    pub fn of(
        profiles_dir: PathBuf,
        apps: Vec<App>,
        steam: Rc<tobii_steam::Steam>,
        custom_games: PathBuf,
    ) -> Self {
        Self {
            profiles_dir,
            apps,
            steam,
            custom_games,
            prefix_cache: RefCell::default(),
        }
    }

    /// Forget every prefix answer, so the next read asks the disk again.
    ///
    /// Called at the top of `read_catalog`, which is the moment something may
    /// have changed — see there.
    pub fn forget_prefixes(&self) {
        self.prefix_cache.borrow_mut().clear();
    }

    /// The Proton prefix for an app id, asked of the disk once per read.
    ///
    /// See [`Scan::prefix_cache`] and [`Scan::forget_prefixes`].
    pub fn prefix(&self, appid: &str) -> Option<PathBuf> {
        if let Some(hit) = self.prefix_cache.borrow().get(appid) {
            return hit.clone();
        }
        let answer = self.steam.prefix(appid);
        self.prefix_cache
            .borrow_mut()
            .insert(appid.to_string(), answer.clone());
        answer
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

/// Which of the list's three sections a row is in.
///
/// The order is the order they are shown in, and it is `Ord` for that: what a
/// user came here to find out is *what have I set up*, so the answer is at the
/// top and the machine's whole catalogue is under it.
///
/// [`Elsewhere`] is the reason this tab is worth building. A profile for a game
/// on a drive that is not plugged in, or for one that has been uninstalled, is
/// invisible everywhere else in this program: Steam does not list the title, so
/// every list built from Steam's manifests leaves it out, and the file goes on
/// sitting in the profiles directory being applied to nothing. It is also the
/// one group whose rows cannot answer most of this page's questions, which is
/// what the detail pane keys off.
///
/// [`Elsewhere`]: Group::Elsewhere
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Group {
    /// A profile, and Steam lists the title as installed here.
    SetUp,
    /// Steam lists it and there is no profile.
    NotSetUp,
    /// A profile, and Steam does not list the title on this machine.
    Elsewhere,
    /// Not Steam's at all: a game somebody pointed at a Wine prefix by hand.
    ///
    /// Last, because it is the smallest group on every machine and the only
    /// one whose rows this program put there. See
    /// [`tobii_config::custom_games`].
    Custom,
}

impl Group {
    /// The heading the section sits under.
    ///
    /// Plain words and not a count: the count is the number of rows under it,
    /// which is on screen directly below, and a heading that carried one would
    /// be the same number twice and a second thing to get wrong when the search
    /// box narrows the list.
    pub(crate) fn heading(self) -> &'static str {
        match self {
            Group::SetUp => "Set up",
            Group::NotSetUp => "Not set up",
            Group::Elsewhere => "Set up, not installed here",
            Group::Custom => "Added by hand",
        }
    }
}

/// What a row of the *Set up* group can say about the bridge.
///
/// Three states and not a `bool`, because the third is the one a user acts on:
/// a game that has never been launched under Proton has nowhere to install a
/// bridge into, and "not installed" over it reads as something to go and press
/// when there is nothing to press yet. Block 2 makes the same distinction; this
/// is the one-word version of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowBridge {
    /// No Proton prefix at all.
    NeverLaunched,
    /// A prefix, holding the artifact a game has to load.
    Installed,
    /// A prefix, without it.
    NotInstalled,
}

impl RowBridge {
    /// Read it off the same value block 2 is written from, so a row and the
    /// page it opens cannot disagree about a prefix they both stat'd.
    fn of(state: &BridgeState) -> Self {
        match state {
            BridgeState::NoPrefix => RowBridge::NeverLaunched,
            BridgeState::Files { .. } => RowBridge::Installed,
            BridgeState::Absent { .. } => RowBridge::NotInstalled,
        }
    }

    /// The clause that follows "profile · " on a row.
    fn clause(self) -> &'static str {
        match self {
            RowBridge::NeverLaunched => "never launched",
            RowBridge::Installed => "bridge installed",
            RowBridge::NotInstalled => "bridge not installed",
        }
    }
}

/// What the action bar's buttons say when a pointer rests on them.
///
/// Consts, and `pub(crate)`, for [`crate::PREVIEW_HELP`]'s reason: GTK4 shows a
/// tooltip on pointer hover and on nothing else, so a fact that lives only in
/// one is unreachable by keyboard and by touch. The help window has to carry
/// the same facts, and `help::tests` asserts it over these very strings rather
/// than over a retyped copy — this tab is outside `tests/help_window.rs`'s
/// rack walk, which is scoped to the Tracker tab's cards and always was, so
/// this is the check that stands in for it and it runs in CI.
/// What the button that runs `tobii bridge status` is called.
///
/// It was "Details", then briefly "Check what's registered" — and neither is
/// right now that block 2 reads the registry itself. What this button adds is
/// the REST of that command's report: the wine build the prefix records,
/// whether a wineserver is holding it, whose the registration is judged to be
/// against a record file inside the prefix, and the caveats. It is the thing
/// to paste into an issue, and nothing on this page depends on pressing it.
///
/// The step it went through is worth leaving here: while block 2 said it could
/// not read those two values, this button was the only way to learn the one
/// fact that decides whether a game loads our client — and pressing it printed
/// a terminal report whose first half repeated the paragraph above it, which
/// is what somebody noticed and reported.
pub(crate) const DETAILS_CAPTION: &str = "Full report\u{2026}";

/// See [`ADD_GAME_CAPTION`].
pub(crate) const DETAILS_TIP: &str =
    "Run `tobii bridge status` for this game and show everything it prints — the wine build \
     this prefix records, whether anything is holding it, and whose the registration is \
     judged to be. What is registered is already above; this is the whole report, for \
     pasting into an issue. It starts nothing.";
/// See [`DETAILS_TIP`].
pub(crate) const UNINSTALL_TIP: &str =
    "Run `tobii bridge uninstall` for this game: take the bridge's files back out of the prefix \
     and unregister them. It does not touch the game or its saves.";
/// See [`DETAILS_TIP`].
pub(crate) const OTHER_CLIENT_TIP: &str =
    "Install a client DLL that is not ours into this prefix — pick the folder holding its \
     NPClient64.dll. For a game that refuses ours at the signature check. A client only \
     reads the shared mapping; something inside the game's own session has to be filling \
     it, and ours is what does. The paragraph above says what that costs.";
/// See [`DETAILS_TIP`].
pub(crate) const ADD_GAME_TIP: &str =
    "For a game Steam does not list — pick the Wine prefix it runs in, and this page can \
     install the bridge into it like any other. Nothing is written to the game.";
/// The three captions the help topic names in prose.
///
/// Shared for the reason the ten strings beside them are: the same `format!`
/// in `help.rs` already pulls `PROFILE_SAVE`, `TRACKER_TAB_POINTER` and the
/// four `Group` headings across this boundary rather than copying them. These
/// were the class left out — and the test that checks the topic mentions them
/// was checking it against a third copy of the same literal, so a rename would
/// have left the prose naming a button that no longer exists with every test
/// still green.
pub(crate) const ADD_GAME_CAPTION: &str = "Add a game by folder\u{2026}";
/// See [`ADD_GAME_CAPTION`].
pub(crate) const FORGET_CAPTION: &str = "Remove from this list";
/// See [`ADD_GAME_CAPTION`].
pub(crate) const OTHER_CLIENT_CAPTION: &str = "Install another client\u{2026}";

/// See [`DETAILS_TIP`].
pub(crate) const FORGET_TIP: &str =
    "Take this game out of the hub's list. It does not touch the prefix, the game, or a bridge \
     already installed in it.";

/// Every tooltip on this tab, for the one test that asks whether the help
/// window carries the same facts.
///
/// `cfg(test)` because nothing in the program reads it: the buttons each take
/// their own const. What it is for is the LENGTH — `help::tests` asserts it, so
/// a sixth tooltip on this tab cannot be added without that test being made to
/// look at the new one.
#[cfg(test)]
pub(crate) const TIPS: [&str; 5] = [
    DETAILS_TIP,
    UNINSTALL_TIP,
    OTHER_CLIENT_TIP,
    ADD_GAME_TIP,
    FORGET_TIP,
];

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
    /// Which section this row is in.
    pub group: Group,
    /// The Wine prefix, for a row of [`Group::Custom`] — the only kind whose
    /// target is a path rather than an app id.
    pub prefix: Option<PathBuf>,
}

impl PickRow {
    /// What the detail pane needs, and nothing else.
    ///
    /// The pane used to be handed a [`tobii_steam::App`] found by app id in the
    /// scan, which is exactly what a row of [`Group::Elsewhere`] has not got:
    /// its title is not in Steam's manifests on this machine, and a lookup that
    /// cannot fail became one that returns nothing and silently does not open
    /// the page.
    pub(crate) fn picked(&self) -> Picked {
        Picked {
            name: self.name.clone(),
            target: match &self.prefix {
                Some(p) => Target::Prefix(p.clone()),
                None => Target::Steam(self.appid.clone()),
            },
            group: self.group,
        }
    }
}

/// What `tobii bridge` is pointed at.
///
/// The CLI has taken `--prefix PATH` as well as `--steam <appid>` since it
/// shipped, so a game Steam has never heard of has never been out of reach from
/// a terminal — this is the hub catching up. The flag pair is built here rather
/// than at the three call sites so that adding a third kind of target is one
/// match arm and not three, and so that no caller can pair `--steam` with a
/// path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// A Steam title, by app id.
    Steam(String),
    /// A Wine prefix somebody named — see [`tobii_config::custom_games`].
    Prefix(PathBuf),
}

impl Target {
    fn push_flag(&self, v: &mut Vec<OsString>) {
        match self {
            Target::Steam(appid) => {
                v.push("--steam".into());
                v.push(appid.into());
            }
            Target::Prefix(p) => {
                v.push("--prefix".into());
                v.push(p.as_os_str().to_os_string());
            }
        }
    }

    /// What the running/outcome/report slots are keyed by.
    ///
    /// A string, and the same string for the life of a row: the slots outlive
    /// the selection — a job started for one game finishes while another is on
    /// screen — so they carry the key of the game they are about and the page
    /// reads only its own. An app id is unique among Steam titles and a prefix
    /// path is unique among everything, so the two cannot collide unless
    /// somebody names a prefix `359320`, which is not a path.
    pub(crate) fn key(&self) -> String {
        match self {
            Target::Steam(appid) => appid.clone(),
            Target::Prefix(p) => p.to_string_lossy().into_owned(),
        }
    }

    /// The app id, for the parts of the page that are keyed by one — a
    /// profile, and Steam's own answers about prefixes.
    pub(crate) fn appid(&self) -> Option<&str> {
        match self {
            Target::Steam(appid) => Some(appid),
            Target::Prefix(_) => None,
        }
    }
}

/// The game the detail pane is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Picked {
    pub name: String,
    /// What `tobii bridge` would be pointed at for this row.
    pub target: Target,
    /// Which section it came from, which is what decides the shape of the
    /// page — see [`blocks`].
    pub group: Group,
}

impl Picked {
    /// The app id, where there is one.
    pub(crate) fn appid(&self) -> Option<&str> {
        self.target.appid()
    }

    /// What the job slots are keyed by.
    pub(crate) fn key(&self) -> String {
        self.target.key()
    }

    /// The prefix, for a game added by hand.
    pub(crate) fn prefix(&self) -> Option<&PathBuf> {
        match &self.target {
            Target::Prefix(p) => Some(p),
            Target::Steam(_) => None,
        }
    }
}

/// Which of the three blocks have an answer for a row of this group.
///
/// Every block is suppressed somewhere, and each absence is a different
/// sentence rather than a blank space:
///
/// * A game Steam does not list here ([`Group::Elsewhere`]) has no prefix on
///   this machine and nothing to send tracking to from here, so the first two
///   go — see [`not_installed_text`] for what block 2 would otherwise say and
///   why it is not true.
/// * A game somebody added by hand ([`Group::Custom`]) has no app id, and a
///   profile is keyed by one. Block 3 reads a profile; with no profile
///   possible there is nothing for it to report, and `ProfileVerdict::None`'s
///   text would tell the reader to run `tobii games profile save <app id>`
///   for a game that has not got one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Blocks {
    pub settings: bool,
    pub bridge: bool,
    pub game: bool,
}

/// [`Blocks`] for a group.
pub(crate) fn blocks(group: Group) -> Blocks {
    match group {
        Group::SetUp | Group::NotSetUp => Blocks {
            settings: true,
            bridge: true,
            game: true,
        },
        Group::Elsewhere => Blocks {
            settings: false,
            bridge: false,
            game: true,
        },
        Group::Custom => Blocks {
            settings: true,
            bridge: true,
            game: false,
        },
    }
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
    /// [`directory_notes`], carried through unchanged. One line each, under
    /// the census, and usually none at all.
    ///
    /// Not filtered by the query, and that is deliberate: a file in the
    /// profiles directory that could not be read is not a search result, and a
    /// warning that disappeared when somebody typed would be a warning about
    /// their typing.
    pub notes: Vec<String>,
}

/// What is in the profiles directory and is not a profile this page can use.
///
/// Three claims, kept apart, because they are three different things and the
/// CLI's own listing keeps them apart for the same reason: a broken profile is
/// this program failing to read what it wrote, a stray is somebody else's file,
/// and a leftover is a save of ours that was cut short. Folding them into one
/// "some files could not be used" is how one gets read as another, and one of
/// them accuses the user of something.
///
/// Counts and not names. The CLI prints every name, and it has a terminal's
/// width and a scrollback to do it in; this is three quiet lines under a list,
/// and the names are one command away — which is the command the last line
/// gives. A label that grew a line per file would push the list off the bottom
/// of a window to report something that is almost always empty.
///
/// Empty on nearly every machine, and that is the point of it being a `Vec`:
/// nothing is shown at all until there is something to say.
pub(crate) fn directory_notes(l: &profiles::Listing) -> Vec<String> {
    let mut out = Vec::new();
    let n = l.problems.len();
    if n > 0 {
        out.push(format!(
            "{n} {thing} in the profiles directory could not be used.",
            thing = plural(n, "profile", "profiles"),
        ));
    }
    let n = l.strays.len();
    if n > 0 {
        out.push(format!(
            "{n} {thing} in that directory {is} not a profile and {was} not written by this \
             program.",
            thing = plural(n, "file", "files"),
            is = plural(n, "is", "are"),
            was = plural(n, "was", "were"),
        ));
    }
    let n = l.leftovers.len();
    if n > 0 {
        out.push(format!(
            "{n} {thing} there {was} written by this program and {is} not a profile — a save \
             that was cut short left {it} behind.",
            thing = plural(n, "file", "files"),
            was = plural(n, "was", "were"),
            is = plural(n, "is", "are"),
            it = plural(n, "it", "them"),
        ));
    }
    if !out.is_empty() {
        // The command that names them, always — and the one that deletes them
        // only where there is something for it to delete. `--purge` removes
        // what this program wrote, which is the leftovers and the profiles
        // themselves; a stray is somebody else's file and it does not touch
        // one. Offering it beside a line about strays alone would be this page
        // promising to tidy up something it will leave exactly where it is.
        let mut tail = "`tobii games profile list` names each of them".to_string();
        if !l.leftovers.is_empty() {
            tail.push_str(", and `tobii uninstall --purge` removes the ones this program wrote");
        }
        tail.push('.');
        out.push(tail);
    }
    out
}

/// Every row the pick page can show, in the order it shows them, with the
/// strings the search box compares against already folded.
///
/// Built once when the window opens, because nothing in it depends on what has
/// been typed. [`picker`] used to rebuild all of it on every keystroke: a sort
/// of the whole list, by a key closure that allocated two `String`s on every
/// comparison, and then a `to_lowercase` of every name again to filter by —
/// all of it to arrive at the same order every time, the order being no
/// function of the query. Typing "elite" did that six times. Measured on this
/// machine's 29 installed titles, a keystroke cost 37µs and now costs 6µs.
///
/// `has_prefix` is injected and asked here for the same reason: it is the one
/// question on this page that touches a disk, and its answer does not change
/// as somebody types. The window used to keep a `HashSet` of the app ids that
/// had a prefix to the same end — a cache beside the list it described, which
/// this is instead: one value, built once, that nothing can ask twice.
pub(crate) struct Catalog {
    /// The rows, ordered by group and then within it. Each carries the
    /// lowercased name beside it.
    rows: Vec<(PickRow, String)>,
    /// How many titles Steam lists as installed here.
    ///
    /// Not `rows.len()`, which it was: the third group is rows this machine
    /// has a profile for and Steam does **not** list, so counting the list
    /// told a user with one such profile that they had one more game
    /// installed than they have — in a sentence whose whole subject is what
    /// Steam's manifests on this machine say.
    installed: usize,
    /// What is in the profiles directory and is not a profile this page can
    /// use. Empty on almost every machine — see [`directory_notes`].
    notes: Vec<String>,
}

impl Catalog {
    /// A catalogue of nothing, for the moment before the tab has been looked
    /// at. See where it is used for why that moment exists.
    pub(crate) fn empty() -> Self {
        Self {
            rows: Vec::new(),
            installed: 0,
            notes: Vec::new(),
        }
    }

    /// Every row, in three groups.
    ///
    /// `profiles` is [`tobii_config::profiles::Listing::profiles`] — the app ids
    /// this machine has a profile for, whatever wrote it. Built-in and
    /// user-written are one answer here on purpose: what the page does with a
    /// profile does not depend on where it came from, and a group called *Set
    /// up* that left out the games this build configures for you would be
    /// telling somebody to set up what is already set up.
    ///
    /// **Two disk questions, asked of different rows**, and that is the whole
    /// reason they are two closures rather than one. `has_prefix` is a manifest
    /// lookup this scan has already paid for and is asked of everything Steam
    /// lists. `bridge` stats three files inside a prefix and is asked of the
    /// *Set up* group alone — usually a handful of rows, against a catalogue
    /// that is 29 on this machine and can be hundreds. A single closure
    /// answering both would quietly stat every installed title to write a
    /// subtitle that says "app id 12345".
    pub(crate) fn new(
        apps: &[App],
        listing: &profiles::Listing,
        custom: &[tobii_config::custom_games::CustomGame],
        has_prefix: &dyn Fn(&str) -> bool,
        bridge: &dyn Fn(&str) -> RowBridge,
    ) -> Self {
        let profiles = &listing.profiles;
        let mut sorted: Vec<&App> = apps.iter().collect();
        // Steam's own plumbing last, and still in the list. `looks_like_tool`
        // is a heuristic on the name and says so; hiding a row it gets wrong
        // would hide the game somebody was looking for, which is the one
        // failure it must not have.
        sorted.sort_by_cached_key(|a| {
            (
                tobii_steam::looks_like_tool(&a.name),
                a.name.to_lowercase(),
                a.appid.clone(),
            )
        });
        let has_profile = |id: &str| profiles.iter().any(|(p, _)| p == id);

        let mut rows: Vec<(PickRow, String)> = sorted
            .iter()
            .map(|a| {
                let group = if has_profile(&a.appid) {
                    Group::SetUp
                } else {
                    Group::NotSetUp
                };
                let subtitle = match group {
                    Group::SetUp => format!("profile · {}", bridge(&a.appid).clause()),
                    _ if has_prefix(&a.appid) => format!("app id {}", a.appid),
                    _ => format!("app id {} · no Proton prefix yet", a.appid),
                };
                let row = PickRow {
                    appid: a.appid.clone(),
                    name: a.name.clone(),
                    subtitle,
                    group,
                    prefix: None,
                };
                (row, a.name.to_lowercase())
            })
            .collect();

        // The third group: a profile whose title Steam does not list here.
        //
        // Named from the profile when it says a name, and by its app id when it
        // does not — which is the only name anything has for it, because the
        // one place that knows what a title is called is the manifest that is
        // not on this machine.
        let mut elsewhere: Vec<(PickRow, String)> = profiles
            .iter()
            .filter(|(id, _)| !apps.iter().any(|a| &a.appid == id))
            .map(|(id, loaded)| {
                let name = loaded
                    .profile
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("app id {id}"));
                let lower = name.to_lowercase();
                (
                    PickRow {
                        appid: id.clone(),
                        name,
                        subtitle: format!("app id {id} · not installed on this machine"),
                        group: Group::Elsewhere,
                        prefix: None,
                    },
                    lower,
                )
            })
            .collect();
        elsewhere.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.appid.cmp(&b.0.appid)));

        // The fourth group: what somebody pointed at a prefix by hand. In the
        // order the file gives, which is the order they were added — a list
        // this short is one somebody remembers adding to, and re-sorting it
        // would move a row out from under the pointer of the person who just
        // made it.
        let added: Vec<(PickRow, String)> = custom
            .iter()
            .map(|g| {
                let here = g.exists();
                (
                    PickRow {
                        // A row of this group is not keyed by an app id and has
                        // none; the path is what identifies it everywhere.
                        appid: String::new(),
                        name: g.name.clone(),
                        subtitle: if here {
                            g.prefix.display().to_string()
                        } else {
                            format!("{} · not on this machine right now", g.prefix.display())
                        },
                        group: Group::Custom,
                        prefix: Some(g.prefix.clone()),
                    },
                    g.name.to_lowercase(),
                )
            })
            .collect();

        // One stable sort by group, which leaves each group in the order it was
        // built with: the catalogue order for the first two, name order for the
        // third, and the file's own order for the fourth.
        rows.extend(elsewhere);
        rows.extend(added);
        rows.sort_by_key(|(r, _)| r.group);
        Self {
            rows,
            installed: sorted.len(),
            notes: directory_notes(listing),
        }
    }
}

/// Build the pick page from the catalogue and what has been typed.
///
/// Pure: everything that would touch a disk was asked when the [`Catalog`] was
/// built.
///
/// The filter is case-insensitive substring, over the name **and** the app id.
/// [`tobii_steam::resolve`] documents the name half and is deliberately not
/// called: the user picks a row rather than typing a string, so
/// [`tobii_steam::Match::Many`] cannot arise and needs no caller — and
/// `resolve`'s own documentation warns that an empty needle answers `Many`
/// over everything, which is what a search box holds before anybody types.
/// The app id half is a widening and never a narrowing: this box cannot hide a
/// row that the same text typed at a terminal would have found.
pub(crate) fn picker(catalog: &Catalog, missing: &[PathBuf], query: &str) -> Picker {
    let q = query.trim().to_lowercase();
    let rows: Vec<PickRow> = catalog
        .rows
        .iter()
        .filter(|(row, name)| {
            q.is_empty()
                || name.contains(&q)
                || (!row.appid.is_empty() && row.appid.contains(&q))
                // The fourth group's rows have no app id, and what identifies
                // one on screen is its path — so the path is what a query has
                // to be able to reach, or a list of six hand-added games can be
                // filtered down to nothing by typing part of the folder they
                // are all in.
                || row
                    .prefix
                    .as_ref()
                    .is_some_and(|p| p.to_string_lossy().to_lowercase().contains(&q))
        })
        .map(|(row, _)| row.clone())
        .collect();

    // The machine's whole count; a query narrows the `Vec` that `picker`
    // returns, never this one.
    let n = catalog.installed;
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
             id.\n\n\
             A game that is not Steam's is not in those manifests and never will be, and this \
             is where somebody with one keeps typing. Add it by hand instead: the button under \
             this list takes the Wine prefix the game runs in, and the bridge installs into \
             that exactly as it does for a Steam title.",
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
        notes: catalog.notes.clone(),
    }
}

/// What the detail pane says instead of the first two blocks, for a game this
/// machine has a profile for and Steam does not list.
///
/// The one genuinely new paragraph the three groups need, and it exists because
/// the honest answer to both of those blocks is *not from here*. Block 2 would
/// run `bridge_state` over no prefix and print `NoPrefix`'s text, which tells
/// somebody to launch the game once and come back — true for a game that is
/// installed and never started, and false for a drive that is not plugged in.
/// Block 1 reports settings that are global and would be word for word what
/// every other row says, which on this page reads as an answer about this game.
///
/// Block 3 is not suppressed and does not need to be: its rows answer
/// `Answer::NoPrefix` on their own, which is already the true thing to say
/// about a file nothing here can open.
pub(crate) fn not_installed_text(name: &str, appid: &str, missing: &[PathBuf]) -> String {
    let mut s = format!(
        "There is a profile for {name} (app id {appid}), and Steam does not list that title as \
         installed on this machine.\n\n\
         So the two things this page would otherwise report — where head tracking is being \
         sent, and whether the Wine bridge is in the game's Proton prefix — have no answer \
         from here. The profile is not lost: it is read whenever this program is asked about \
         that app id, and it applies again the moment Steam lists the title."
    );
    if let Some(note) = missing_note(missing) {
        s.push_str("\n\n");
        s.push_str(&note);
    }
    s
}

/// A first name for a prefix somebody has just picked.
///
/// The folder's own name, which for a Proton prefix is usually the app id and
/// for everything else is usually the game — `star-citizen/pfx` gives `pfx`,
/// which is useless, so a folder called `pfx` or `drive_c` takes its parent's
/// name instead. Whatever comes out is a starting point: the list is a plain
/// file and the name in it is display-only.
pub(crate) fn name_for_prefix(dir: &Path) -> String {
    let base = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let here = base(dir);
    let up = match here.as_str() {
        // The two names a prefix directory almost always has, neither of which
        // is a game.
        "pfx" | "drive_c" => dir.parent().map(base).unwrap_or_default(),
        _ => String::new(),
    };
    let chosen = if up.trim().is_empty() { here } else { up };
    if chosen.trim().is_empty() {
        dir.display().to_string()
    } else {
        chosen
    }
}

/// What stands in for block 3 on a game somebody added by hand.
///
/// Block 3 reads a profile, a profile is a file named `<app id>.toml`, and one
/// of these has no app id — so there is nothing to look up rather than nothing
/// found, and `ProfileVerdict::None`'s text would tell the reader to run
/// `tobii games profile save` with an app id they have not got.
///
/// The first two blocks are shown as usual: where head tracking is being sent
/// is a global answer and true of any game, and the bridge is the whole reason
/// this row exists.
pub(crate) fn added_by_hand_text(name: &str) -> String {
    format!(
        "{name} was added by hand, so this page knows the Wine prefix and nothing else about \
         it.\n\n\
         The bridge below is installed into that prefix exactly as it is for a Steam game. What \
         is missing is the third section: reading a game's own options needs a profile, a \
         profile is a file named after a Steam app id, and this game has not got one. Nothing \
         else on this page is affected."
    )
}

/// What the detail pane says when Steam has nothing installed at all.
///
/// It was a page of the modal's stack, and it was a page nothing could leave —
/// the window opened on it or never reached it. As a **state of the pane** it
/// is the same words doing the same job without the dead end, which is what it
/// always was: an empty list under a search box invites somebody to keep typing
/// at it, so the pane beside the list says why the list is empty.
pub(crate) fn nothing_text(missing: &[PathBuf]) -> String {
    let mut s = "No installed Steam games were found on this machine.\n\n\
                 Steam records what is installed in a manifest beside each game. If Steam is \
                 installed somewhere this program does not look, nothing here will see it, \
                 and the rest of this tab has nothing to work on."
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
                     created — {why}. Nothing is reaching it, so this page does not count \
                     it as a destination; the card on the Tracker tab says the same."
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

/// What every state of block 1 ends with, because block 1 has no controls.
///
/// It had one — a button that set `enabled` and `joystick` together — and that
/// button was the whole of the duplication the modal was modal to survive: two
/// live editors of one global setting. Deleting it leaves a block that reports
/// and never writes, and a block of prose about settings, with nothing on it to
/// press, has to say where the controls are or it reads as a page that has
/// forgotten its own buttons.
///
/// `pub(crate)` so the help topic and the tests quote it rather than retyping
/// it.
pub(crate) const TRACKER_TAB_POINTER: &str =
    "This page only reports these. The switch, the strength and the virtual joystick are on \
     the Tracker tab; an opentrack address or a bridge port is not — those come from \
     games.toml and `tobii games set` changes them.";

/// Block 1: this program's own settings, reported.
///
/// Three states rather than the two an `enabled && joystick` gate would give,
/// because the middle one is real: somebody who has already set up opentrack
/// has game output on and no joystick, and telling them to "turn on" a switch
/// that is on is the kind of small untruth this whole tab exists not to tell.
///
/// # Why there is no caption any more
///
/// There was a button, and it built its caption rather than stating one: it set
/// `enabled` **and** `joystick`, `enabled` is the switch every other
/// destination hangs off, and with opentrack and a bridge port already in the
/// file a fixed caption reading "Turn on and send to a virtual joystick"
/// promised one destination and produced three. The button is gone with the
/// modal — see [`TRACKER_TAB_POINTER`] — so what survives is the half of that
/// argument the body was already making: the destinations already in the file
/// are named **before** anything is touched, because the switch on the other
/// tab starts all of them and not just the one somebody has in mind.
pub(crate) fn settings_block(
    cfg: &OutputConfig,
    bridge: profiles::Bridge,
    joystick: &JoystickStatus,
) -> String {
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
            // Nothing wrong and nothing to do about it, said out loud: every
            // other state of this block ends by naming something that could be
            // changed, and silence here would read as the same list cut short.
            None => body.push_str(" Nothing to change here."),
            Some(t) => {
                body.push_str("\n\n");
                body.push_str(t);
            }
        }
        body.push_str("\n\n");
        body.push_str(TRACKER_TAB_POINTER);
        return body;
    }

    let mut body = if !cfg.enabled {
        match sinks.as_slice() {
            [] => "Head tracking for games is off.".to_string(),
            // The destinations already in the file, named before the switch is
            // touched. They are all started by the one switch on the Tracker
            // tab, and a page that mentioned them only once it was on told
            // somebody they were turning on a joystick and gave them three.
            _ => format!(
                "Head tracking for games is off. It is already configured to send to {list}, \
                 {strength} — turning it on, on the Tracker tab, starts all of that.",
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
    body.push_str("\n\n");
    body.push_str(TRACKER_TAB_POINTER);
    body
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
        "The profile for this game also asks for {n} of this program's own settings: {pairs}. \
         This window has not applied {them} — `tobii games set <key> <value>` in a terminal \
         does.",
        n = settings.len(),
        them = plural(settings.len(), "it", "them"),
    ))
}

/// Whether [`profile_bridge_note`] prints the signature gate for this answer.
///
/// One predicate, two readers, rather than one reader guessing from the other's
/// words. [`measured_note`] needs to know whether the gate has been said
/// already, and it used to find out by searching the note for the word
/// "signature" — a coupling nothing enforced, which a rewording of the gate
/// would have broken silently and which answered wrongly for
/// [`profiles::Bridge::NotNeeded`]: a measured game with such a profile printed
/// "this game does not need the bridge" and the whole gate underneath it.
pub(crate) fn bridge_says_the_gate(bridge: profiles::Bridge) -> bool {
    matches!(bridge, profiles::Bridge::Required)
}

/// What a profile says about the bridge, for block 2.
///
/// [`None`] for [`profiles::Bridge::Unstated`], and that is the whole point of
/// that variant: a profile that does not mention the bridge has not said the
/// game does without one, and printing "this game does not need the bridge"
/// from an absence would be a claim nobody made.
///
/// [`profiles::Bridge::Required`] carries the signature check with it, because
/// this is the one place the window knows a game wants TrackIR or FreeTrack —
/// and which of the two it is decides whether installing the bridge can
/// deliver anything at all. The rest of block 2 is about files, a directory
/// and two registry values, every word of which can be true while the game
/// receives nothing. It is asked of
/// [`tobii_config::signature::trackir_gate`] rather than written here: `tobii
/// bridge install` says the same thing in a terminal, and a window that
/// retyped it is how the two start disagreeing.
// receives nothing. It is asked of
/// [`tobii_config::signature::trackir_gate`] rather than written here: `tobii
/// bridge install` says the same thing in a terminal, and a window that
/// retyped it is how the two start disagreeing.
///
/// [`provider_note`] is appended after it, for the reason given there.
pub(crate) fn profile_bridge_note(bridge: profiles::Bridge) -> Option<String> {
    // `bridge_says_the_gate` is the predicate for "does this arm print the
    // gate", and this match has to agree with it — see there.
    match bridge {
        profiles::Bridge::Unstated => None,
        profiles::Bridge::Required => Some(format!(
            "The profile for this game says it reads TrackIR or FreeTrack, so the bridge is \
             what it needs — the virtual joystick above will not reach it.\n\n\
             Which of the two it speaks decides what installing the bridge can do for it. \
             {gate}\n\n\
             {note}",
            gate = signature::trackir_gate(),
            note = signature::provider_note(),
        )),
        profiles::Bridge::NotNeeded => Some(
            "The profile for this game says it does not need the bridge. Head tracking for it \
             goes through the settings above instead."
                .to_string(),
        ),
    }
}

/// What this project has watched THIS title do at the signature check.
///
/// Block 2 carried the gate only when a profile said `bridge = required`, and
/// nothing ships a profile — so the one page that could have told somebody
/// their game is known to refuse our client told them nothing, on exactly the
/// titles it has been measured on. The bar offers *Install another client…*
/// for those titles; this is the paragraph that says why it is there.
///
/// `already` is whether the profile note above has said it, so the two do not
/// print the gate twice on a game that has both a profile and a measurement.
pub(crate) fn measured_note(m: Option<&signature::Measured>, already: bool) -> Option<String> {
    let m = m?;
    // The gate ALREADY names every measurement, this one included — it is
    // built by interpolating each `clause()` — so printing the clause here and
    // the gate below it read the same sentence twice in adjacent paragraphs.
    // What this paragraph adds is that the measurement is about THIS game, so
    // it says that and lets the gate carry the wording.
    let mut s = format!(
        "This program has watched this very game meet that check: {}.",
        m.behaviour
    );
    s.push_str(
        " So installing our client into this prefix is very unlikely to give it head \
         tracking, however cleanly it installs \u{2014} which is what the button offering \
         somebody else's client is for.",
    );
    if !already {
        s = format!("{}\n\n{s}", signature::trackir_gate());
        s.push_str("\n\n");
        s.push_str(&signature::provider_note());
    }
    Some(s)
}

/// The two registry values that decide which client a game loads, read out of
/// the prefix's own `user.reg`.
///
/// # Why this is here now and was not before
///
/// Block 2 used to say, in as many words, that this window does not read these
/// — and the reason was never that reading them is hard. It is a plain-text
/// file and `tobii_config::userreg` does it with no dependencies and no wine.
/// The reader simply lived in `tobii-cli`, which is a `[[bin]]`, so the one
/// fact that decides whether a game loads our client at all was reachable only
/// by pressing a button that shelled out — and what came back was a terminal
/// report whose first half repeated what block 2 had already said.
///
/// # What it does NOT say
///
/// Whose the registration is. `tobii bridge status` judges that against a
/// record file it wrote inside the prefix, and a second opinion here — "the
/// path looks like ours" — would be a looser rule reaching a confident verdict
/// on the same question. So this reports the VALUE and names the button that
/// judges it. A user reading `Z:\usr\libexec\opentrack` has what they came
/// for either way.
///
/// `None` when there is no prefix, no `user.reg`, or it cannot be read — three
/// different nothings that all mean "this cannot be reported from here", and a
/// sentence claiming otherwise is what the block above is written against.
pub(crate) fn registered_clients(prefix: Option<&Path>) -> Option<String> {
    use tobii_config::userreg::{lookup, Lookup};
    let text = std::fs::read(prefix?.join(tobii_config::userreg::FILE)).ok()?;
    let say = |abi: &str, path: &str| match lookup(&text, path, tobii_config::userreg::PATH_VALUE) {
        Lookup::Absent => format!("{abi} has nothing registered"),
        Lookup::Text(v) => format!("{abi} is registered to {v}"),
        Lookup::Rejected(why) => format!("{abi} holds something this program cannot read — {why}"),
    };
    Some(format!(
        "Registered in this prefix, read from its own `{file}` with no wine run against it: \
         {ft}; {np}. That is what a game will load. Whether either of them is this program's \
         is a question about a record kept inside the prefix, which the button below answers.",
        file = tobii_config::userreg::FILE,
        ft = say("FreeTrack", tobii_config::userreg::FREETRACK_KEY),
        np = say("TrackIR", tobii_config::userreg::NPCLIENT_KEY),
    ))
}

// ------------------------------------------------------- block 2: the bridge

/// What a stat of the prefix found. Three cases, and the names say exactly how
/// much was looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BridgeState {
    /// [`tobii_steam::prefix`] found nothing.
    NoPrefix,
    /// A prefix with no [`BRIDGE_ARTIFACT`] in it. `present` is which of
    /// [`BRIDGE_FILES`] a stat *did* find, and it is here for the same reason
    /// it is on [`Self::Files`]: a directory holding two of the three is not a
    /// directory holding none, and "the bridge's files are not in it" over one
    /// is this window reporting a presence as an absence — the mirror of the
    /// bug [`present_files`] was written against, and just as wrong.
    Absent {
        prefix: PathBuf,
        present: Vec<&'static str>,
    },
    /// A prefix with the bridge's required artifact in it. `present` is which
    /// of [`BRIDGE_FILES`] a stat actually found — never assumed to be all
    /// three, because the installer skips an optional one it was not built
    /// with and says so.
    Files {
        prefix: PathBuf,
        dir: PathBuf,
        present: Vec<&'static str>,
    },
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
fn bridge_state(prefix: Option<&Path>) -> BridgeState {
    match prefix.map(Path::to_path_buf) {
        None => BridgeState::NoPrefix,
        Some(prefix) => {
            let dir = prefix.join(BRIDGE_SUBDIR);
            let present = present_files(&dir);
            if present.contains(&BRIDGE_ARTIFACT) {
                BridgeState::Files {
                    prefix,
                    dir,
                    present,
                }
            } else {
                BridgeState::Absent { prefix, present }
            }
        }
    }
}

/// Which of [`BRIDGE_FILES`] are in an install directory, in the order that
/// list gives them.
///
/// Three stats rather than one. The one decides whether a game can load
/// anything at all; all three decide what this window is allowed to say it
/// found, and those are not the same question. A prefix installed from a tree
/// with no `NPClient64.dll` holds one file, and "the bridge's files are in
/// this prefix" over it is this window reporting an absence as a presence —
/// which is the one bug shape this whole window is written against.
fn present_files(dir: &Path) -> Vec<&'static str> {
    BRIDGE_FILES
        .into_iter()
        .filter(|name| dir.join(name).is_file())
        .collect()
}

/// Which of [`BRIDGE_FILES`] are **not** in `present`, in the same order.
fn absent_files(present: &[&'static str]) -> Vec<&'static str> {
    BRIDGE_FILES
        .into_iter()
        .filter(|name| !present.contains(name))
        .collect()
}

/// The sentence naming what a stat of the install directory found, and what it
/// did not.
///
/// Shared by block 2 and by [`outcome_text`], so the paragraph offering the
/// page and the paragraph reporting an install cannot disagree about the same
/// set of stats.
fn present_sentence(dir: &Path, present: &[&'static str]) -> String {
    let missing = absent_files(present);
    if missing.is_empty() {
        return format!(
            "All {n} of the bridge's files are in this prefix, at {dir} — {list}.",
            n = BRIDGE_FILES.len(),
            dir = dir.display(),
            list = present.join(", "),
        );
    }
    format!(
        "{n} of the bridge's {total} files {is} in this prefix, at {dir} — {list}. {gone} \
         {isnt} not. The one a game has to load, {req}, is there; the rest are optional, and \
         the installer prints `not built yet — skipping` for one the build it ran did not \
         have.",
        n = present.len(),
        total = BRIDGE_FILES.len(),
        is = plural(present.len(), "is", "are"),
        dir = dir.display(),
        list = present.join(", "),
        gone = missing.join(" and "),
        isnt = plural(missing.len(), "is", "are"),
        req = BRIDGE_ARTIFACT,
    )
}

/// The sentence for a prefix with no [`BRIDGE_ARTIFACT`] in it.
///
/// Two sentences, not one, for the reason [`present_sentence`] has two: the
/// installer marks one of the three required and skips either of the others
/// when the build it ran did not have them, so a prefix can hold two files and
/// still be unable to load anything. Over that prefix the flat plural "the
/// bridge's files are not in it" names an absence that is not there, which is
/// [`present_files`]' own bug shape read backwards.
///
/// # What the empty branch is allowed to claim
///
/// `present` is [`present_files`]', and that is three `is_file()` calls. An
/// empty `present` therefore means *no regular file of those three names*,
/// which is not the same as *nothing of those names*: a directory called
/// `NPClient64.dll`, or a symlink pointing at nothing, is on disk and is not
/// a file. This branch used to read "none of the other 2 is there either" —
/// an absence asserted about something that is there, which is
/// [`present_files`]' own doc comment describing the bug it was written
/// against, one level up. It says what the stat established instead, and
/// names the gap, because a user staring at a `NPClient64.dll` in their file
/// manager while this window says there is none has been told the window is
/// wrong about everything else too.
fn absent_sentence(dir: &Path, present: &[&'static str]) -> String {
    if present.is_empty() {
        return format!(
            "The bridge's files are not in it: there is no {req} under {dir}, and no file \
             of the other {n} {name} either. All three are stats for a regular file, so a \
             name there that is a directory, or a link pointing at nothing, is counted \
             here as not present.",
            req = BRIDGE_ARTIFACT,
            dir = dir.display(),
            n = BRIDGE_FILES.len() - 1,
            name = plural(BRIDGE_FILES.len() - 1, "name", "names"),
        );
    }
    format!(
        "{n} of the bridge's {total} files {is} already in it, at {dir} — {list}. {req}, \
         the one a game has to load, is not, so as it stands a game looking for the bridge \
         finds nothing it can load.",
        n = present.len(),
        total = BRIDGE_FILES.len(),
        is = plural(present.len(), "is", "are"),
        dir = dir.display(),
        list = present.join(", "),
        req = BRIDGE_ARTIFACT,
    )
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
/// The order is [`tobii_steam::Steam::prefixes`]', which leads with the one an
/// install writes into; the chosen prefix is dropped from it by path rather
/// than by position, because which library holds the manifest is what decides
/// it and that is not a position.
///
/// This used to rebuild `compatdata/<appid>/pfx` out of the library list
/// itself, a second copy of a path shape `tobii-steam` already knew and that
/// nothing over there could hold this to.
fn other_prefixes(rest: Vec<PathBuf>) -> Vec<(PathBuf, bool)> {
    rest.into_iter()
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
///
/// `beside` is [`Search::beside`], and it only matters when `tobii` is
/// [`None`]: [`no_binary_text`] needs it to say whether the directory beside
/// this program was examined at all. It is carried through rather than worked
/// out here so that the sentence and the stat cannot be about different
/// directories.
pub(crate) fn bridge_block(
    state: &BridgeState,
    missing: &[PathBuf],
    others: &[(PathBuf, bool)],
    tobii: Option<&Path>,
    beside: Option<&Path>,
    target: &Target,
) -> String {
    // The caption is `bridge_action`'s, not this match's. The bar at the foot
    // of the pane asks the same question, and a paragraph and a button that
    // decided it separately is how a page ends up offering *Install* under a
    // sentence saying it is already installed.
    let action = bridge_action(state);
    let mut text = match state {
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
            s
        }
        BridgeState::Absent { prefix, present } => {
            format!(
                "The prefix is at {}.\n{found}\n\n\
                 The bridge is what lets a game see a TrackIR or FreeTrack device from inside \
                 Wine. Installing it copies up to {n} small {file} into the prefix — {list} — \
                 and sets two registry values. Only {req} is required, and a `tobii` built \
                 without one of the others copies what it has and says which it skipped. It \
                 does not touch the game or its saves.",
                prefix.display(),
                found = absent_sentence(&prefix.join(BRIDGE_SUBDIR), present),
                n = BRIDGE_FILES.len(),
                file = plural(BRIDGE_FILES.len(), "file", "files"),
                list = BRIDGE_FILES.join(", "),
                req = BRIDGE_ARTIFACT,
            )
        }
        BridgeState::Files { dir, present, .. } => {
            // The question, and no pointer at a button for the answer: the
            // answer is the line `registered_clients` adds under this one. It
            // used to say "which this window does not read" and then name the
            // button that did — a sentence that had to be made conditional on
            // there being a `tobii` to run, and whose whole existence was an
            // artefact of the reader living in a binary the hub cannot link.
            format!(
                "{}\n\n\
                 Whether the game will actually find {them} is a question about two registry \
                 values inside the prefix.",
                present_sentence(dir, present),
                them = plural(present.len(), "it", "them"),
            )
        }
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
        (None, Some(a)) => {
            text.push_str("\n\n");
            text.push_str(&no_binary_text(*a, beside, target));
        }
        (None, None) => {}
    }
    text
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
pub(crate) fn install_argv(tobii: &Path, target: &Target, with: &InstallWith) -> Vec<OsString> {
    let mut v = bridge_argv(tobii, Verb::Install, target);
    if let Some(w) = &with.wine {
        v.push("--wine".into());
        v.push(w.as_os_str().to_os_string());
    }
    if let Some(dir) = &with.npclient {
        v.push("--npclient".into());
        v.push(dir.as_os_str().to_os_string());
    }
    v
}

/// The optional halves of an install.
///
/// A struct rather than two positional `Option<&Path>`, because two of them
/// side by side is a call site where the wrong one silently installs the right
/// thing in the wrong place — and one of the two decides which client DLL a
/// game will load, which is the whole question this page exists around.
///
/// `npclient` is a DIRECTORY holding somebody else's `NPClient64.dll`;
/// opentrack ships one. Ours is the default and needs no flag. See
/// [`tobii_config::signature`] for what the difference decides and for the two
/// titles it has been measured on.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct InstallWith {
    pub wine: Option<PathBuf>,
    pub npclient: Option<PathBuf>,
}

/// The argv for `tobii bridge status`, which starts nothing and only reads —
/// which is why this window is allowed to run it behind a plain button.
pub(crate) fn status_argv(tobii: &Path, target: &Target) -> Vec<OsString> {
    bridge_argv(tobii, Verb::Status, target)
}

/// What the action bar can do for the game on screen, and what to say when it
/// can do nothing.
///
/// # Why this is a value
///
/// The three buttons used to be inside block 2, three paragraphs down a
/// scroller, and their visibility was four `if let` blocks in the middle of
/// `refresh` reading four different things. Two consequences, and both were
/// real: the one thing this tab can actually DO was below the fold on a short
/// window, and when it could do nothing the page said so only in prose that had
/// to be read to the end. A bar that is always on screen has to be able to say
/// "nothing, and here is why" in one line, and that is a sentence — so it is
/// decided here, where it can be tested, rather than by the absence of widgets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Actions {
    /// Install or Reinstall — one button, whose caption is the promise.
    pub primary: Option<Action>,
    /// Whether taking it back out is offered. Only ever over a prefix that
    /// holds the artifact: there is nothing to remove otherwise, and a button
    /// that fails is worse than no button.
    pub uninstall: bool,
    /// `tobii bridge status`, which reads and starts nothing.
    pub details: bool,
    /// The Proton-build picker, which [`JobView`] decides is earned.
    pub wine: bool,
    /// Whether to offer taking this row out of the hub's own list.
    ///
    /// In the value and not decided at the call site, which is where it was:
    /// the whole reason `Actions` exists is that the bar is decided somewhere
    /// testable "rather than by the absence of widgets", and Forget is on that
    /// bar. Left outside, it also falsified the invariant below — a hand-added
    /// game whose folder is gone returns a `blocked` reason AND shows a button.
    pub forget: bool,
    /// Whether to offer installing somebody else's client DLL instead of ours.
    ///
    /// Only for a title this project has MEASURED stopping at the signature
    /// check, which today is one Steam game. Everywhere else it would be a
    /// guess: our client is the right default, the paragraph above the bar
    /// already names the route for anybody whose game turns out to gate, and a
    /// button offering a third-party binary to every game on the machine would
    /// be this page recommending something nobody has watched work there.
    pub other_client: bool,
    /// Why there is nothing to press, when there is nothing. [`None`] when the
    /// bar has a button.
    pub blocked: Option<&'static str>,
}

/// Whether the place this row points at can be worked on at all, before
/// anything looks at what is in it.
///
/// Three answers and not a `bool`, because the two refusals are different facts
/// about different kinds of row and a user can act on one of them. A game Steam
/// does not list here is not coming back until Steam lists it; a folder
/// somebody named that is not there right now is a drive to plug in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    /// There is a place to work on.
    Reachable,
    /// [`Group::Elsewhere`]: Steam does not list the title on this machine.
    NotListedHere,
    /// [`Group::Custom`]: the folder named is not on this machine right now.
    FolderGone,
}

/// Decide the bar from what block 2 has just worked out.
///
/// The order of the refusals is the order they have to be answered in. A game
/// that is not here has no prefix either, and a machine with no `tobii` on its
/// PATH would still have nothing to install into — reporting a later one over
/// an earlier one tells somebody to fix the wrong thing.
pub(crate) fn actions(
    state: &BridgeState,
    tobii: Option<&Path>,
    job: &JobView,
    reach: Reach,
    measured: bool,
    group: Group,
) -> Actions {
    // Forget is not gated on anything the refusals below test: the row is in
    // this program's own list whether or not the folder it names is on the
    // machine, and taking it out is the answer to a folder that has gone.
    let forget = group == Group::Custom;
    let none = |why: &'static str| Actions {
        primary: None,
        uninstall: false,
        details: false,
        wine: false,
        other_client: false,
        forget,
        blocked: Some(why),
    };
    match reach {
        Reach::NotListedHere => {
            return none(
                "Steam does not list this game on this machine, so there is no prefix here to \
                 install into.",
            )
        }
        Reach::FolderGone => {
            return none(
                "The folder this game was added with is not on this machine right now — a drive \
                 that is not plugged in, or one that has moved. Nothing here can be installed \
                 into it until it is back.",
            )
        }
        Reach::Reachable => {}
    }
    if matches!(state, BridgeState::NoPrefix) {
        return none(
            "No Proton prefix yet. Run the game once under Proton and come back — a native \
             Linux game never gets one, and the virtual joystick is how it receives head \
             tracking.",
        );
    }
    if tobii.is_none() {
        return none(
            "The command-line program `tobii` is not on the PATH this window was started with, \
             so this page cannot run the installer. Block 2 prints what to type instead.",
        );
    }
    Actions {
        primary: bridge_action(state),
        uninstall: matches!(state, BridgeState::Files { .. }),
        details: true,
        wine: job.offer_wine,
        other_client: measured,
        forget,
        blocked: None,
    }
}

/// What the primary button promises, from the prefix alone.
///
/// Split out of [`bridge_block`], which used to be the only thing that decided
/// it — the bar needs the same answer and must not reach a different one, and
/// the paragraph and the button captioning themselves separately is exactly how
/// a page ends up offering *Install* under a paragraph that says it is already
/// installed.
pub(crate) fn bridge_action(state: &BridgeState) -> Option<Action> {
    match state {
        BridgeState::NoPrefix => None,
        BridgeState::Absent { .. } => Some(Action::Install),
        BridgeState::Files { .. } => Some(Action::Reinstall),
    }
}

/// The argv for `tobii bridge uninstall`.
///
/// Takes the artifacts back out of the prefix and unregisters them. Its own
/// function beside [`install_argv`] rather than a flag on it, because the two
/// are different verbs with different consequences and a boolean that chose
/// between them would be one character away from removing what somebody meant
/// to install.
pub(crate) fn uninstall_argv(tobii: &Path, target: &Target) -> Vec<OsString> {
    bridge_argv(tobii, Verb::Uninstall, target)
}

/// Which `tobii bridge` subcommand an argv is for.
///
/// A type and not a `&str`, because the doc below says the three stay three
/// functions so that nothing is "one character away from removing what somebody
/// meant to install" — and handing the verb over as a string put it back one
/// character away. `"uninstal"` is not a compile error; this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Install,
    Uninstall,
    Status,
}

impl Verb {
    fn word(self) -> &'static str {
        match self {
            Verb::Install => "install",
            Verb::Uninstall => "uninstall",
            Verb::Status => "status",
        }
    }
}

/// `tobii bridge <verb>` pointed at a target — the half the three verbs share.
///
/// The three stay three FUNCTIONS: `uninstall_argv`'s own doc says why, and a
/// boolean choosing between install and uninstall would be one character away
/// from removing what somebody meant to install. What is shared here is only
/// the preamble, which had drifted into three copies that all had to agree the
/// subcommand is `bridge`.
fn bridge_argv(tobii: &Path, verb: Verb, target: &Target) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec![
        tobii.as_os_str().to_os_string(),
        "bridge".into(),
        verb.word().into(),
    ];
    target.push_flag(&mut v);
    v
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
            // Not "The bridge is installed." full stop. The installer prints
            // `copied 1 file(s)` and a `not built yet — skipping` note over a
            // build missing an optional artifact, and a flat success sentence
            // above its own account of what it skipped is this window
            // contradicting the program it just ran, in the same paragraph.
            BridgeState::Files { dir, present, .. } => {
                let mut s = "The bridge is installed.".to_string();
                if !absent_files(present).is_empty() {
                    s.push(' ');
                    s.push_str(&present_sentence(dir, present));
                }
                s
            }
            BridgeState::Absent { prefix, present } => format!(
                "The installer reported no error, and a stat of the prefix afterwards says \
                 otherwise. {found} This window will not call that an install.\n\n\
                 An exit code is all a program hands back, and one that understood nothing \
                 and exited zero looks exactly like one that did the work. Press \
                 {DETAILS_CAPTION} to \
                 see what the prefix holds, and read what this run said below.",
                found = absent_sentence(&prefix.join(BRIDGE_SUBDIR), present),
            ),
            BridgeState::NoPrefix => "The installer reported no error, and there is no Proton \
                                      prefix here for it to have written into — so whatever \
                                      it did, it was not this game's prefix. This window will \
                                      not call that an install."
                .to_string(),
        }
    } else {
        match o.code {
            None => format!(
                "The installer stopped before it could report a result — it was killed, or it \
                 crashed. What it had done by then is not something this window can say; press \
                 {DETAILS_CAPTION} to read the prefix as it stands."
            ),
            Some(code) if said => format!("The installer stopped, exit status {code}. It said:"),
            // "It said:" with nothing under it is a colon pointing at a gap. A
            // child that exits non-zero having printed nothing at all is the
            // one case where the installer's own words are not the answer, so
            // the sentence has to stand on its own.
            Some(code) => format!(
                "The installer stopped, exit status {code}, and printed nothing at all — no \
                 error, and no word about what it had done. Press {DETAILS_CAPTION} to read \
                 the prefix \
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
             Closing this window does not stop it, and does not stop this page watching \
             either: the installer finishes and the result is here when you come back. \
             Quitting stops the watching; the installer still finishes. It copies each file \
             beside its target and renames it, so a half-finished copy cannot be loaded; \
             stopping it between the copy and the registry write is the one way to leave a \
             prefix inconsistent."
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
) -> Search {
    let beside = exe.and_then(Path::parent).map(Path::to_path_buf);
    if let Some(dir) = &beside {
        let candidate = dir.join("tobii");
        if exists(&candidate) {
            return Search {
                found: Some(candidate),
                beside,
            };
        }
    }
    Search {
        found: on_path("tobii"),
        beside,
    }
}

/// What [`tobii_binary`] found, **and where it looked**.
///
/// Two fields rather than the `Option<PathBuf>` this used to return, because
/// the paragraph printed when nothing is found opens by reporting on the
/// directory beside this program — and that directory does not always exist
/// to report on. `std::env::current_exe` reads `/proc/self/exe` on Linux and
/// can fail; a path it returns can have no parent. In either case
/// [`tobii_binary`] has nowhere to stand beside and skips that half of the
/// search entirely, and a sentence saying "`tobii` is not beside this one"
/// would then be naming a place nothing examined. [`beside`](Self::beside) is
/// [`None`] exactly in those cases, and [`no_binary_text`] branches on it.
///
/// It is returned by the search rather than worked out again by the caller
/// for the obvious reason: two computations of "the directory beside this
/// program" can disagree, and then the sentence is about a different place
/// from the stat.
pub(crate) struct Search {
    /// The `tobii` to run, or [`None`] when neither place had one.
    pub(crate) found: Option<PathBuf>,
    /// The directory that was stat'd for a `tobii` beside this program, or
    /// [`None`] when this program could not work out where its own file is
    /// and so never looked there.
    pub(crate) beside: Option<PathBuf>,
}

/// What to say when there is no `tobii` to run.
///
/// It takes the [`Action`] it is replacing, because it is replacing a button
/// and has to name the same job that button did. Written for [`Action::Install`]
/// and printed over a prefix that already holds the files, it read "the bridge
/// cannot be installed from here" under a paragraph saying the files are in
/// this prefix and over a button that said *Reinstall* — a page stating a
/// state and then denying it two paragraphs later.
///
/// `Details` is hidden by the same missing binary, so the reinstall wording
/// names `bridge status` as well: that is the one thing the page tells the
/// user to press for an answer it will not give itself.
///
/// # Whose `$PATH` this is about
///
/// [`on_path`] reads `std::env::var_os("PATH")` — **this process's**. A hub
/// started from its desktop entry gets the session's environment, not an
/// interactive shell's, and `~/.local/bin` is added by a shell rc far more
/// often than by the session: `scripts/release.sh`'s `install.sh` puts both
/// binaries there by default, so the ordinary user whose terminal has `tobii`
/// is exactly the user this window cannot find it for.
///
/// So this paragraph used to state "is not on your PATH" — settled fact about
/// the user's terminal — out of evidence covering only the GUI's environment,
/// and then, in the same breath, tell them to type `tobii` in that terminal.
/// It says what it actually looked at instead, and it says what it means when
/// the terminal cannot find it either: every install route this project has
/// puts `tobii` and this program in one directory, so the answer there is not
/// a `$PATH` line, it is a reinstall.
///
/// # And whose "beside this one" this is about
///
/// `beside` is [`Search::beside`] — the directory [`tobii_binary`] actually
/// stat'd, or [`None`] when it had none to stat. The head used to open "is
/// not beside this one" unconditionally, while [`tobii_binary`] skips the
/// beside-check whenever `current_exe` gives it nothing to stand beside: on
/// such a machine the first sentence the user read reported on a place that
/// was never examined. It is the same shape as the `$PATH` sentence above it,
/// one clause to the left.
pub(crate) fn no_binary_text(action: Action, beside: Option<&Path>, target: &Target) -> String {
    // What the reader would have to type, from the SAME value the buttons are
    // built from. It was an app id and a hardcoded `--steam`, and a game added
    // by hand has a path — so the one machine that cannot run the installer was
    // handed `tobii bridge install --steam /games/sc/pfx`, which is the exact
    // pairing `Target::push_flag` exists to make impossible.
    let flag = match target {
        Target::Steam(id) => format!("--steam {id}"),
        // Quoted, because this is a command somebody is about to paste: a
        // prefix under `My Games` is four shell words unquoted, and `--prefix`
        // gets the first of them. `install_argv` is unaffected — it builds an
        // `OsString` argv — so only the half the user acts on was wrong.
        Target::Prefix(p) => format!("--prefix {}", crate::sh_quote(&p.to_string_lossy())),
    };
    // The first clause is the one that used to be printed over a place
    // nothing had looked at. `tobii_binary` only stats the directory beside
    // this program when it knows which directory that is; when `current_exe`
    // gives it nothing, it goes straight to `$PATH`, and the sentence has to
    // say so rather than report a stat that never happened.
    let looked = match beside {
        Some(dir) => format!(
            "The command-line program `tobii` is not in {}, beside this one, and it is not \
             on the PATH this window was started with",
            dir.display()
        ),
        None => "The command-line program `tobii` is not on the PATH this window was \
                 started with, and this program could not work out where its own file is, \
                 so it has not looked beside itself either"
            .to_string(),
    };
    let head = format!(
        "{looked} — which is not necessarily the one your terminal has. This project's \
         installer puts both programs in `~/.local/bin` by default, and that directory is \
         usually added to the PATH by a shell rc, which a window started from the app menu \
         never reads. So try the command below in a terminal: it may simply work."
    );
    let tail = "If the terminal cannot find `tobii` either, it is not installed on this \
                machine. It is not a separate download — every install route this project \
                has puts it in the same directory as this program — so the fix is to \
                install the release again.";
    match action {
        Action::Install => format!(
            "{head}\n\n\
             Until then the bridge cannot be installed from here:\n\n\
             tobii bridge install {flag}\n\n\
             {tail}"
        ),
        Action::Reinstall => format!(
            "{head}\n\n\
             Until then nothing here can be run against this prefix — neither a reinstall \
             nor the whole report behind {DETAILS_CAPTION}, which is why that button is not \
             on the \
             page either:\n\n\
             tobii bridge status {flag}\n\
             tobii bridge install {flag}\n\n\
             {tail}"
        ),
    }
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

/// How to write the profile this window has just said does not exist.
///
/// The state every user of this build is in — `profiles::BUILTIN` is empty —
/// and the page used to end at *where* profiles go, which is a window naming a
/// directory and leaving the user to find the command that fills it. The
/// commands are `tobii games profile`'s own, spelled as its usage spells them;
/// `check add` writes the profile if there is none, so the two lines work in
/// either order and are given in the order somebody does them.
///
/// Named here in full rather than as "see the manual", for the reason block 2
/// names its binary: this window is the hub, and the thing it is telling
/// somebody to do lives in a different program.
pub(crate) fn write_a_profile_commands(appid: &str) -> String {
    format!(
        "To write one, in a terminal:\n\n\
         {save} {appid}\n\
         {add} {appid} {ADD_FLAGS}\n\n\
         `save` records this program's own settings as they stand under this game's app id. \
         `check add` is what fills this section: one setting in one of the game's own files, \
         and what it should say. Run `{PROFILE_CHECK}` on its own for what the fields mean \
         and which file formats this build reads, and `{where_} {appid}` to see which \
         directory the paths are taken as relative to.",
        save = PROFILE_SAVE,
        add = PROFILE_CHECK_ADD,
        where_ = PROFILE_CHECK_WHERE,
    )
}

/// The `tobii games profile` commands this tab tells somebody to type, and the
/// help window's "Games" topic types the same ones.
///
/// One spelling each, in one place, for the reason [`BRIDGE_FILES`] is one
/// list: two surfaces that both name a command and are edited apart end up
/// telling the user to run something that is not there. They are `tobii games
/// profile`'s own usage, as `crates/tobii-cli/src/main.rs` prints it — that
/// crate is a binary with no library target, so nothing here can link it and
/// the agreement is kept by hand.
pub(crate) const PROFILE_CHECK: &str = "tobii games profile check";
/// See [`PROFILE_CHECK`].
pub(crate) const PROFILE_SAVE: &str = "tobii games profile save";
/// See [`PROFILE_CHECK`].
pub(crate) const PROFILE_CHECK_ADD: &str = "tobii games profile check add";
/// See [`PROFILE_CHECK`].
pub(crate) const PROFILE_CHECK_WHERE: &str = "tobii games profile check where";
/// What `check add` needs after the game, as its usage spells it.
pub(crate) const ADD_FLAGS: &str = "--format <format> --path <path> --setting <name> \
                                    --wants <value> --tell \"<what to do about it>\"";

/// Which prefix the check rows below were read out of, when there is more than
/// one on this machine.
///
/// Block 2 carries [`other_prefixes_note`] and block 3 did not, so the page
/// warned that the prefix it names may be the abandoned one and then reported
/// *"there is no directory at …, nothing has saved a control scheme here"*
/// about that same prefix, as settled fact and one block lower. Both blocks
/// read the one path [`refresh`] chose, so both say so.
///
/// [`None`] unless a check actually read a prefix: with no other prefixes
/// there is nothing to warn about, and with no prefix at all the rows say that
/// themselves.
pub(crate) fn checked_prefix_note(
    checks: usize,
    prefix: Option<&Path>,
    others: &[(PathBuf, bool)],
) -> Option<String> {
    let prefix = prefix?;
    if checks == 0 || others.is_empty() {
        return None;
    }
    let n = others.len();
    Some(format!(
        "What follows was read under {p} — the same prefix block 2 names, and this title has \
         {n} other Proton {prefix_word} on this machine. If the game runs from {one}, every \
         line below is about the wrong copy of its settings, including the ones saying \
         nothing is there.",
        p = prefix.display(),
        prefix_word = plural(n, "prefix", "prefixes"),
        one = plural(n, "that one", "one of those"),
    ))
}

/// The paragraphs at the head of block 3, before any check rows.
///
/// Every one of them says the block is read-only, because every one of them is
/// a state somebody might act on.
pub(crate) fn profile_intro(
    v: &ProfileVerdict,
    game: &str,
    appid: &str,
    profiles_dir: &Path,
) -> String {
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
             Profiles go in {dir}, one file per game, named by app id. This build ships \
             {shipped}, so writing one is the only way this section ever says \
             anything.\n\n{cmds}",
            dir = profiles_dir.display(),
            // Asked, not asserted. Three screens used to carry their own
            // "ships none", so the day one ships they would all have gone on
            // saying otherwise with nothing failing.
            shipped = profiles::shipped_profiles(),
            cmds = write_a_profile_commands(appid),
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
/// Appended to every answer that read the game's files and did not confirm a
/// match, and that is the point of it: the commonest real case is not a wrong
/// value, it is a player who has never touched the setting, and that answer
/// used to read in full as
/// `HeadlookMode: not set.` — no statement of what was expected and no
/// instruction, on a page whose third block exists to say exactly those two
/// things. `tobii games profile show` prints both under every check it lists,
/// whatever the file turned out to hold, and two halves of one feature
/// disagreeing about whether the user is told what to do is worse than either
/// answer on its own.
///
/// Not appended to [`Answer::NoPrefix`] or [`Answer::UnknownFormat`]. Those
/// two say the check could not run — a remedy belongs on an answer that says
/// the setting is wrong, not on one that says nothing was looked at.
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
        // The two answers that are about this machine and this build rather
        // than about the game's setting, and so the two that take no
        // [`expectation`]: there is no file to go and change a value in, and
        // "set head look to Toggle" under "this game has no Proton prefix here
        // yet" is an instruction to edit something that does not exist.
        Answer::NoPrefix => {
            return format!(
                "{}: not checked — this game has no Proton prefix here yet, so its own \
                 settings have nowhere to be.",
                r.setting
            );
        }
        Answer::UnknownFormat(fmt) => {
            return format!(
                "{}: not checked — this build has no reader for the file format “{fmt}”.",
                r.setting
            );
        }
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
    if b.start.is_none() {
        // The tail is one choice rather than four, because "none of them" has
        // no singular that fits the same slot: a directory shipping exactly
        // one preset has to say "it is not in use", not "none of them is".
        parts.push(format!(
            "nothing in this directory selects a preset, so {}",
            plural(
                b.presets.len(),
                "the row below is the preset the game ships and it is not in use",
                "the rows below are the presets the game ships and none of them is in use",
            ),
        ));
    }
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
/// [`profiles::Check::path_under`], which pushes the profile's relative,
/// `..`-free path onto the prefix, so the result is *lexically* under it and
/// nothing here canonicalises.
///
/// **Lexically under is not inside, and this comment used to say it was.** A
/// Wine prefix maps the machine into itself under `dosdevices/`: `z:` is `/`,
/// and a Steam prefix adds `s:` pointing at the library root. A check whose
/// path begins `dosdevices/s:/steamapps/common/` therefore reaches a game's
/// shipped files, and one beginning `dosdevices/z:/` reaches any file this
/// user can read — through this function, by the readers below, exactly as a
/// path under `drive_c` would be.
/// This module's `a_check_read_through_a_drive_letter_lands_outside_the_prefix`
/// runs that against this function and reads the bytes back;
/// [`profiles::Check::path`] owns the rule and says why the capability is
/// wanted rather than merely tolerated.
///
/// What does bound this function is the other half of the promise: every arm
/// calls a `tobii-gameconf` reader, and those only read, and only the one
/// [`profiles::Check::setting`] they were handed. `tobii-gameconf`'s own
/// `tests/writes_nothing.rs` is what holds that — nothing in this file does.
///
/// The match on [`profiles::Format`] is exhaustive with no wildcard arm, on
/// purpose. A format added to `tobii-config` has to be answered here or this
/// file stops compiling, instead of falling into
/// [`profiles::Format::Unknown`]'s sentence and being reported as a check
/// this build cannot perform when in fact it can.
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
                // A guard, not a case: `binds::read` answers `Unwritten`
                // before it builds a `Bindings` with nothing in it, on both of
                // its paths. Kept so a change there cannot turn into a row-less
                // block here, and worded once — the two path-dependent
                // sentences this used to carry could drift with nothing
                // reaching them to notice.
                if b.presets.is_empty() {
                    return (
                        vec![row(
                            None,
                            Answer::Unwritten(format!("{} holds no preset", path.display())),
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

// -------------------------------------------------------------------- the tab

/// What the detail pane says before a game has been picked.
///
/// **It says "the one it can", and that number is the whole reason this
/// function is not the one the modal had.** The modal did two of the three: it
/// wrote this program's own settings from block 1's button, and it installed
/// the bridge. Block 1's button is gone with the modal — the settings have one
/// editor now, on the Tracker tab — so the count is one, and a lead still
/// offering two would be this tab promising a button that no longer exists
/// before the user has picked anything.
///
/// It is still the count that this branches on, for the same reason it always
/// was: without a `tobii` to run, [`bridge_block`] replaces the install button
/// with the command to type, and then the tab does **none** of the three.
/// Everything per-game is block 2's to say.
///
/// The `None` arm names only the `$PATH` half of the search. It used to open
/// "which is not beside this one", which [`tobii_binary`] has not necessarily
/// checked — see [`Search::beside`]. Block 2 has the directory and says which
/// of the two places were looked at; this line, printed before a game is
/// picked and with no room for the distinction, says the half that is true
/// either way and leaves the rest to the page that can be precise about it.
/// What the detail pane says before a game has been picked: a heading, a
/// paragraph, and — when there is a list to pick from — what to do next.
///
/// A function rather than three `if`s among the widgets, because this file's
/// rule is that every sentence it shows is produced by something CI can assert
/// without a display, and the pane that every new user lands on is a poor place
/// to start making exceptions. Zero profiles ship, so this IS the first screen
/// of the Games tab for everyone.
///
/// The `empty` arm is the modal's old third page, which was a page nothing
/// could leave. As a state of the pane it says exactly what it said before, and
/// it drops the instruction: there is nothing on the left to pick.
fn intro_text(
    tobii: Option<&Path>,
    empty: bool,
    missing: &[PathBuf],
) -> (&'static str, String, Option<&'static str>) {
    if empty {
        return ("No games found", nothing_text(missing), None);
    }
    (HEADING, lead(tobii).to_string(), Some(PICK_ONE))
}

/// The one instruction on the Games tab.
///
/// A master–detail pane with nothing selected has to say which half to use, or
/// it reads as a page that failed to load. It is the only sentence this stage
/// adds that was not somewhere in the modal already.
const PICK_ONE: &str = "Pick a game on the left.";

fn lead(tobii: Option<&Path>) -> &'static str {
    match tobii {
        Some(_) => {
            "Everything Steam says is installed on this machine. Pick one and this page will \
             show the three things that have to be configured for it, and do the one it can."
        }
        None => {
            "Everything Steam says is installed on this machine. Pick one and this page will \
             show the three things that have to be configured for it, and do none of them: \
             installing the Wine bridge is the one it could do, and that needs the \
             command-line program `tobii`, which \
             this window could not find — it is not on the PATH this window was started with, \
             and a terminal's is often not the same — so for that one the page says what to \
             type there instead."
        }
    }
}

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
/// of their own — and block 3 has none, on purpose. See [`build_with`].
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
/// **Nothing here kills the child, ever.** `bridge.rs`'s install stages each
/// DLL beside its target and renames it, exactly so a half-finished copy cannot
/// be loaded, and killing it between the copy and the registry write is the one
/// way to leave a prefix inconsistent. The poll only stops watching.
///
/// `alive` is **the hub's** lifetime and not this page's, which is what the tab
/// bought over the modal it replaces: closing the hub to the tray used to close
/// the modal, which set `alive` false and threw away a running install's
/// result. Now the poll keeps running while the hub is hidden and the outcome
/// lands in its slot, where [`JobView`] finds it — keyed by appid, which it
/// already was — the next time the page is drawn. Only the quit path sets this
/// false. See [`GamesTab::alive`].
///
/// `appid` is stored beside the command line and is what the page keys its
/// "Running now:" line to. One job at a time on this tab, whichever game it
/// was started for — the guard below is on `running` itself and not on a
/// per-game slot, because one child process is what this tab is prepared to
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
        // First line, before anything is touched: the hub may be gone.
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

/// The Games tab, and the one thing about it the hub has to be told.
///
/// Two fields, and they are deliberately not one struct the hub keeps: `root`
/// goes straight into the hub's `Stack`, which owns it, and `alive` is what the
/// hub's **quit** path sets. If the hub's close handler held this whole struct
/// it would hold `root`, which is a descendant of the window that holds the
/// handler — a strong reference from a window to its own subtree, which is the
/// sibling cycle GTK never breaks. So [`crate::build_hub`] takes the two apart
/// the moment it has them and only `alive` goes into a closure.
pub struct GamesTab {
    /// The widget the hub adds to its `Stack`.
    pub root: gtk::Box,
    /// Clear this and the tab stops watching whatever subprocess it started,
    /// because the program is ending.
    ///
    /// **Set false from the quit path and not from `unmap`.** The modal this
    /// tab replaces was closed by the hub's unmap handler — that is, by hiding
    /// to the tray — and closing it cleared this flag, so hiding the hub in the
    /// middle of a `tobii bridge install` threw the result away while the
    /// installer carried on. A tab is not closed by hiding, so the poll keeps
    /// running, the outcome lands in its per-appid slot, and the page shows it
    /// when the hub comes back. That is strictly better and it is why this is
    /// not wired to `unmap`.
    ///
    /// A bare flag and not a `shutdown(&self)` method, which is what this was:
    /// the caller cannot hold a `GamesTab` to call one on. [`crate::build_hub`]
    /// takes this struct apart the moment it has it precisely so that the
    /// close handler captures an `Rc<Cell<bool>>` and not `root`, which is one
    /// of the window's own descendants — so a method here had no reachable
    /// caller and the inline `set(false)` was never going to become one.
    pub alive: Rc<Cell<bool>>,
}

/// Read the profiles directory and build the list from it.
///
/// Separate from [`Catalog::new`], which is pure and tested as such: this is
/// the half that touches a disk, and it is called again every time the tab
/// comes into view.
///
/// The bridge closure is the expensive one — three stats inside a prefix — and
/// [`Catalog::new`] asks it only of the rows that have a profile. It is written
/// over [`bridge_state`], the same function block 2 is worded from, so a row
/// saying *bridge installed* and the page it opens cannot disagree.
fn read_catalog(scan: &Scan) -> Catalog {
    // The prefix answers go first, and that is not a tidy-up. A prefix appears
    // when a game is first launched under Proton, and this page's own words are
    // "run the game once under Proton and come back" — so the one moment the
    // cache must not survive is the one this function is called at. Kept, it
    // told a user who had done exactly that that their game had still never
    // been launched, while block 2 beside it (which asks `Steam` directly)
    // found the prefix and offered to install into it. The memoisation is for
    // the many asks WITHIN one read, which is where the cost was.
    scan.forget_prefixes();
    let listing = profiles::list_from(&scan.profiles_dir, profiles::BUILTIN);
    let custom = tobii_config::custom_games::list_from(&scan.custom_games);
    Catalog::new(
        &scan.apps,
        &listing,
        &custom,
        &|id| scan.prefix(id).is_some(),
        &|id| RowBridge::of(&bridge_state(scan.prefix(id).as_deref())),
    )
}

/// Build the Games tab.
///
/// Reads `$HOME` and this user's profile directory, and hands both to
/// [`build_with`]. Nothing in CI calls this.
///
/// The scan is a measured ~400 µs and 81 filesystem calls, and it is now paid
/// once when the hub is built rather than on the first press of a button. That
/// is the trade the tab makes: it used to be paid on every press of a door
/// that had already opened its window, which was worse — the modal checked the
/// registry before it read the disk for exactly that reason, and there is no
/// door to be on the wrong side of any more.
///
/// `joystick` is the device thread's own answer, the one the games card reports
/// from — see [`destinations`] for why the checkbox is not enough.
///
pub fn build(joystick: Arc<Mutex<JoystickStatus>>) -> GamesTab {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let dir = profiles::profiles_dir();
    build_with(scan(&home, &dir), joystick)
}

/// The tab, over a scan that was handed in.
///
/// **Master–detail, horizontal, and no pages.** The modal needed a page stack
/// and a Back button because it was 620px wide and could show one or the other;
/// a tab is over 1200px, both panes fit side by side, and a selection that
/// survives is how somebody compares two games. The list keeps its selection
/// while the tab is hidden.
///
/// It takes **no claim on the tracker**: [`crate::hold_while_open`] is not
/// called, no `DemandGuard` is taken, and nothing here is in
/// `device::EXCLUSIVE` or a candidate for it. See the module docs.
///
/// `joystick` is read, never written: this tab asks the device thread what
/// became of the tick, the same fact the games card reports.
pub fn build_with(scanned: Scan, joystick: Arc<Mutex<JoystickStatus>>) -> GamesTab {
    let scan = Rc::new(scanned);

    // This machine's Steam install, read once and asked everything: which
    // games have a prefix, where the one for the game on screen is, and which
    // other prefixes that title has. Every one of those was a fresh walk of
    // all four `libraryfolders.vdf` files before, and the first of them is
    // asked of every installed title — 29 walks to open one window, on a
    // machine one of whose libraries is on a drive that is not plugged in.
    //

    // The list, in the order the page shows it, with the prefix question
    // answered for each row. None of it depends on what gets typed.
    //
    // In a cell rather than built once, because one of its three inputs can
    // change while the hub is open and the other two cannot. Steam's manifests
    // and this machine's prefixes are read by `scan` and by `scan.steam`, which
    // are a walk each and are deliberately paid once. The profiles directory is
    // a `readdir` of a handful of small files, and `tobii games profile save`
    // in a terminal is the documented way to put something in it — so a list
    // that grouped by profile and never looked again would answer "Not set up"
    // about a game the user had just set up, on the page whose whole subject is
    // what is set up. It is re-read when the tab comes into view, which is the
    // first moment anybody could be looking at the answer.
    // EMPTY at build time, and filled when the tab is first shown. The hub adds
    // the Tracker page to the stack first, so this page is not mapped at
    // startup and `content.connect_map` re-reads the catalogue and rebuilds
    // every row before anybody can see one — so a catalogue read here was a
    // directory walk and a per-title prefix stat whose result was thrown away
    // on every launch of the program, including the launches that never open
    // this tab.
    let catalog = Rc::new(RefCell::new(Catalog::empty()));

    // Before the pick page, because the lead above the list says what this
    // window will do for a game and one of the two things it does needs this.
    let search = {
        let exists = |p: &Path| p.is_file();
        let path_lookup = on_path(&exists);
        tobii_binary(
            std::env::current_exe().ok().as_deref(),
            &exists,
            &path_lookup,
        )
    };
    // Both halves of one answer, kept together: `beside` is the directory
    // that search stat'd, and block 2's sentence about a missing `tobii` is
    // only allowed to name a place because this is it.
    let beside: Rc<Option<PathBuf>> = Rc::new(search.beside);
    let tobii: Rc<Option<PathBuf>> = Rc::new(search.found);

    // ---- the master pane: the search box, the list, the census
    //
    // The pick page's widgets in the same order, minus its heading and its
    // lead. Those two are not about the list — they are about what picking
    // something will do — so they belong to the pane that shows it, and that is
    // where the empty state below puts them.

    let pick = gtk::Box::new(Orientation::Vertical, 10);
    pick.set_size_request(LIST_WIDTH, -1);

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
    // Under the census and usually not there at all: what is in the profiles
    // directory and is not a profile. Hidden rather than empty, so it takes no
    // height on the machines — almost all of them — with nothing to say.
    let notes = small("");
    notes.add_css_class("section-warn");
    notes.set_visible(false);
    // What the last press of "Add a game by folder…" said, if it refused.
    //
    // Its own label and not `notes`: `rebuild` owns that one and rewrites it
    // on every keystroke, every tab switch and every successful add, so a
    // refusal put there was wiped by the next character typed — leaving the
    // dialog closed, no row added, and nothing on screen. The two also say
    // different kinds of thing: `notes` is a standing fact about the profiles
    // directory, this is the answer to something the user just did.
    let add_said = small("");
    add_said.add_css_class("section-warn");
    add_said.set_visible(false);
    // The one control on the list side, under it rather than beside the search
    // box: it is not a way of finding a game, it is a way of adding one that
    // searching can never find. `tobii bridge install --prefix PATH` has always
    // worked from a terminal; what was out of reach from the hub was the list
    // that remembers the path.
    let add_btn = crate::widget::button(ADD_GAME_CAPTION);
    add_btn.add_css_class("quiet");
    add_btn.set_halign(Align::Start);
    add_btn.set_tooltip_text(Some(ADD_GAME_TIP));
    pick.append(&search);
    pick.append(&list_scroll);
    pick.append(&census);
    pick.append(&notes);
    pick.append(&add_said);
    pick.append(&add_btn);

    // ---- the detail pane, state one: nothing picked yet
    //
    // Zero profiles ship, so this is where every new user lands, and the pane
    // is not empty even then: the list beside it has whatever Steam says is
    // installed, which is what they came for.
    //
    // `nothing_text` fills the pane instead when Steam lists nothing at all —
    // the modal's third page, demoted from somewhere you could never leave to a
    // state of the pane, which is what it always was.
    //
    // None of these three labels is `selectable`, unlike the blocks below, and
    // that is load-bearing rather than an oversight: this box is hidden the
    // moment a game is picked, and `help.rs`'s measurement is that a pane
    // folded away with the focus inside it holds its subtree for good. A label
    // that cannot take focus cannot be holding it when the box folds. The one
    // selectable label here is `nothing_body`, and it is only ever built on a
    // machine with no games, where nothing is ever picked and the box never
    // folds.
    let empty = scan.apps.is_empty();
    let (head, body, instruction) =
        intro_text(tobii.as_deref(), empty, scan.steam.missing_libraries());
    let intro = gtk::Box::new(Orientation::Vertical, 10);
    let intro_head = Label::new(Some(head));
    intro_head.set_halign(Align::Start);
    intro_head.set_xalign(0.0);
    intro_head.add_css_class("dialog-heading");
    intro.append(&intro_head);
    // `wrapped` when there is nothing to pick, because then this paragraph is
    // the whole of the pane and a path in it is worth being able to copy. A
    // plain label otherwise, so nothing in this box can hold the focus when it
    // folds away.
    let nothing_body = wrapped(&body);
    if empty {
        intro.append(&nothing_body);
    } else {
        let lead_label = Label::new(Some(&body));
        lead_label.set_halign(Align::Start);
        lead_label.set_xalign(0.0);
        lead_label.set_wrap(true);
        lead_label.set_max_width_chars(58);
        lead_label.add_css_class("dialog-lead");
        intro.append(&lead_label);
    }
    if let Some(line) = instruction {
        let pick_one = Label::new(Some(line));
        pick_one.set_halign(Align::Start);
        pick_one.set_xalign(0.0);
        pick_one.set_wrap(true);
        pick_one.set_max_width_chars(58);
        pick_one.add_css_class("section-desc");
        intro.append(&pick_one);
    }

    // ---- the detail pane, state two: one game

    let game = gtk::Box::new(Orientation::Vertical, 14);
    let g_title = Label::new(None);
    g_title.set_halign(Align::Start);
    g_title.set_xalign(0.0);
    g_title.set_wrap(true);
    g_title.add_css_class("dialog-heading");
    let g_sub = small("");
    game.append(&g_title);
    game.append(&g_sub);

    // What stands in for the first two blocks when Steam does not list the
    // title here — a whole group of the list, and the one case where those two
    // blocks have no true answer. See `not_installed_text`.
    //
    // A box of its own, with its own divider, so that showing it is one
    // `set_visible` and hiding it is one more: three widgets toggled instead of
    // five, and no chance of a divider left behind above nothing.
    let away = gtk::Box::new(Orientation::Vertical, 14);
    let away_body = wrapped("");
    away.append(&away_body);
    away.append(&divider());
    away.set_visible(false);
    game.append(&away);

    // No actions box, for the reason block 3 has never had one: this block
    // reports settings it does not own. The one editor of them is the games
    // card on the Tracker tab, and a second control here writing the same two
    // keys is the duplication the modal was modal to survive.
    let (b1, s_body) = block(1, "These settings");
    let d1 = divider();
    game.append(&b1);
    game.append(&d1);

    let (b2, br_body) = block(2, "The Wine bridge");
    // No actions box in the block any more. The buttons are on a bar at the
    // foot of the pane — see `action_bar` below for why.
    let b_report = small("");
    b2.append(&b_report);
    let d2 = divider();
    game.append(&b2);
    game.append(&d2);

    // No actions box either, and that is the decision rather than an oversight:
    // this program never writes a game's own configuration files, so block 3
    // has nothing to press. See the module docs.
    let (b3, p_body) = block(3, "The game's own settings");
    let rows_box = gtk::Box::new(Orientation::Vertical, 6);
    b3.append(&rows_box);
    b3.append(&small(READ_ONLY_FOOTER));
    game.append(&b3);

    // Both states of the detail pane, as siblings in one box rather than as a
    // second `Stack`: there is nothing to switch back to. `intro` is shown
    // until a game is picked and then hidden for good, which is why the walk
    // from `intro` to `game` below moves the focus first.
    game.set_visible(false);
    let detail = gtk::Box::new(Orientation::Vertical, 14);
    detail.append(&intro);
    detail.append(&game);

    let detail_scroll = gtk::ScrolledWindow::new();
    detail_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    detail_scroll.set_vexpand(true);
    detail_scroll.set_hexpand(true);
    detail_scroll.set_size_request(DETAIL_WIDTH, -1);
    detail_scroll.set_child(Some(&detail));

    // ---- the action bar, pinned under the detail pane
    //
    // Outside the scroller, and that is the whole point of it. These are the
    // only controls on this tab that change anything, and they were three
    // paragraphs down inside block 2: on a short window the one thing this page
    // can DO was below the fold, and a user who had not scrolled had no way of
    // knowing there was anything to press. Pinned, they are on screen for every
    // game and at every window height.
    //
    // It also has to be able to say NOTHING, out loud. Hiding every button for
    // a game with no prefix left a blank strip and a reason buried at the end of
    // a paragraph; `actions` returns that sentence and this shows it where the
    // buttons would have been.
    let action_bar = gtk::Box::new(Orientation::Horizontal, 10);
    action_bar.set_halign(Align::End);
    action_bar.set_valign(Align::Center);
    // NOT `wrapped`, which is selectable: this label sits in a row of buttons,
    // and a selectable label selects all of its text the moment focus reaches
    // it — which here is every time Tab passes through on the way to Install.
    // `help.rs` records the measurement; the three bodies in the pane above
    // pay for it because their text is worth copying, and a refusal is not.
    let blocked = Label::new(None);
    blocked.add_css_class("control-note");
    blocked.set_halign(Align::Start);
    blocked.set_xalign(0.0);
    blocked.set_wrap(true);
    blocked.set_hexpand(true);
    blocked.set_max_width_chars(64);
    blocked.set_visible(false);
    let bar = gtk::Box::new(Orientation::Horizontal, 12);
    bar.add_css_class("action-bar");
    bar.append(&blocked);
    bar.append(&action_bar);
    bar.set_visible(false);

    // ---- the two panes, side by side

    // `vexpand`, unlike the control rack this sits beside in the hub's stack,
    // and the difference is what each of them is made of. The rack is a fixed
    // set of cards: it wants its natural height and any spare window below it
    // is plain background, which is why `crate::build_hub` gives the grid
    // `vexpand(false)` and `valign(Start)`. This tab is two scrollers over
    // lists that have no natural length — 29 titles here, three on the next
    // machine — so without this the list gets its own small natural height and
    // scrolls inside a hundred pixels while four hundred sit empty underneath
    // it.
    //
    // It does not grow tab 1, and the reason is not the one it looks like.
    // Measured: `gtk_widget_compute_expand` on the hub's stack answers **true
    // on both tabs** — a `GtkStack` takes its expand from all of its pages, not
    // from the one that is showing — so this flag reaches the Tracker tab too.
    // It costs nothing there for two separate reasons, both of which have to
    // hold: `crate::build_hub` gives the control rack `vexpand(false)` and
    // `valign(Start)`, so it stays its own height at the top of whatever it is
    // given; and the hub's opening height comes from `root.measure`, which does
    // not consult expand at all. Measured either side of this line when it was
    // added: 1189 x 725 both times. The equality is the fact; the hub opens at
    // 1189 x 750 since, and `crate::build_hub` records what changed.
    let content = gtk::Box::new(Orientation::Horizontal, 16);
    content.set_vexpand(true);
    content.append(&pick);
    // The detail side is the scroller with the bar under it, so the bar stays
    // put while the page behind it scrolls.
    let right = gtk::Box::new(Orientation::Vertical, 10);
    right.set_hexpand(true);
    right.append(&detail_scroll);
    right.append(&bar);
    content.append(&right);

    // ---- the buttons, built once
    //
    // Once, and never inside `refresh`, because `refresh` is what their
    // handlers call: a button created by `refresh` would be held by a box that
    // `refresh` holds, and that is the cycle. Built here and only *described*
    // by `refresh`, which reaches them through `downgrade()`.
    let install_btn = crate::widget::button(Action::Install.caption());
    let details_btn = crate::widget::button(DETAILS_CAPTION);
    details_btn.add_css_class("quiet");
    details_btn.set_tooltip_text(Some(DETAILS_TIP));
    let wine_btn = crate::widget::button("Choose the Proton build…");
    // Only ever on a row of `Group::Custom`, and it removes the ROW rather than
    // anything on disk. Named for what it does to this program's own list: an
    // ambiguous caption beside *Uninstall* is one click from somebody expecting
    // the bridge to come out of their prefix and getting the entry deleted
    // instead.
    let forget_btn = crate::widget::button(FORGET_CAPTION);
    forget_btn.add_css_class("quiet");
    forget_btn.set_tooltip_text(Some(FORGET_TIP));
    // Offered only to a title this project has watched stop at the signature
    // check — see `Actions::other_client`. The caption says "another" rather
    // than naming opentrack: opentrack ships a client and so might something
    // else, and this page has not watched either of them deliver.
    let other_btn = crate::widget::button(OTHER_CLIENT_CAPTION);
    other_btn.add_css_class("quiet");
    other_btn.set_tooltip_text(Some(OTHER_CLIENT_TIP));
    let uninstall_btn = crate::widget::button("Uninstall");
    uninstall_btn.add_css_class("quiet");
    uninstall_btn.set_tooltip_text(Some(UNINSTALL_TIP));
    // The one that commits, first and accented; the rest quiet, in the order
    // somebody reaches for them.
    install_btn.add_css_class("primary");
    action_bar.append(&install_btn);
    action_bar.append(&uninstall_btn);
    action_bar.append(&details_btn);
    action_bar.append(&wine_btn);
    action_bar.append(&other_btn);
    action_bar.append(&forget_btn);

    // ---- the state the page draws itself from

    let sel: Rc<RefCell<Option<Picked>>> = Rc::default();
    // All three carry the app id their answer is about, and they carry it
    // because clearing them on a selection change was not enough. A job
    // outlives the selection it was started from: select a game, press *Install
    // the bridge*, pick another, and the running line and then the outcome
    // landed on whichever game was selected when the child finished — the
    // second game's page showing the first game's install and then its success.
    // `row_activated` can clear what is there; it cannot clear what has not
    // arrived yet. So the closure writes the id with the answer and the page
    // reads only its own.
    //
    // The tab makes that carry matter more, not less: the poll now survives the
    // hub being hidden to the tray, so an outcome can arrive while nothing is
    // on screen at all and be read minutes later.
    let running: Rc<RefCell<Option<(String, String)>>> = Rc::default();
    let outcome: Rc<RefCell<Option<(String, Outcome)>>> = Rc::default();
    let report: Rc<RefCell<Option<(String, String)>>> = Rc::default();

    // ---- refresh
    //
    // It holds labels and plain boxes strongly, and **every widget that carries
    // a handler through `downgrade()`**. A button's handler holds this closure;
    // if this closure held the button, or the box the button sits in, that
    // would be a cycle between two siblings, which GTK never breaks and which a
    // test that weak-refs only the window cannot see.
    //
    // The rule is unchanged and the stakes are higher: this tree used to die
    // every time the modal closed, which was often, so a cycle showed up
    // quickly. It now lives as long as the hub, and the hub closes once, at
    // quit — which is the exact shape of the v0.3.1 bug. `tests/games_tab.rs`
    // weak-refs the whole subtree for that reason.
    let refresh: Rc<dyn Fn()> = {
        let (scan, sel, running, outcome, report, tobii, beside, joystick) = (
            scan.clone(),
            sel.clone(),
            running.clone(),
            outcome.clone(),
            report.clone(),
            tobii.clone(),
            beside.clone(),
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
        // The four things a row of `Group::Elsewhere` turns off and the one it
        // turns on. **Weak, every one of them**, and one of the five is the
        // reason: `b2` contains `b_actions`, which contains the three buttons
        // whose `clicked` handlers hold this very closure. Held strongly here
        // that is `b2` -> `b_actions` -> `install_btn` -> handler -> this
        // closure -> `b2`, a cycle between two parts of one subtree, which GTK
        // never breaks — measured as 23 widgets and 45 event controllers still
        // alive after the program quit. The other four carry no handler and
        // could be strong; they are weak so that the rule at the top of this
        // closure is what the code looks like rather than something a reader
        // has to re-derive per widget.
        let (away, away_body, b1, d1, b2, d2, b3w) = (
            away.downgrade(),
            away_body.downgrade(),
            b1.downgrade(),
            d1.downgrade(),
            b2.downgrade(),
            d2.downgrade(),
            b3.downgrade(),
        );
        let action_bar_w = action_bar.downgrade();
        let (bar_w, blocked_w) = (bar.downgrade(), blocked.downgrade());
        let (install_w, uninstall_w, details_w, wine_w, other_w, forget_w) = (
            install_btn.downgrade(),
            uninstall_btn.downgrade(),
            details_btn.downgrade(),
            wine_btn.downgrade(),
            other_btn.downgrade(),
            forget_btn.downgrade(),
        );
        Rc::new(move || {
            let Some(app) = sel.borrow().clone() else {
                return;
            };
            g_title.set_text(&app.name);
            g_sub.set_text(&match &app.target {
                Target::Steam(appid) => format!("app id {appid}"),
                Target::Prefix(p) => p.display().to_string(),
            });

            // Which of the three blocks have an answer for this row, and the
            // sentence that stands in for the ones that have not. Every block
            // is suppressed somewhere and each absence is a different
            // sentence — see `blocks`.
            let shown = blocks(app.group);
            // `d1` sits under block 1 and `d2` under block 2, so each belongs
            // to the block BELOW it: a divider is a line between two things,
            // and one shown with nothing under it is a rule at the foot of the
            // pane. `d2` was tied to `shown.bridge`, which left exactly that on
            // every hand-added game — bridge shown, block 3 hidden.
            for (w, on) in [
                (&b1, shown.settings),
                (&d1, shown.settings && (shown.bridge || shown.game)),
                (&b2, shown.bridge),
                (&d2, shown.bridge && shown.game),
                (&b3w, shown.game),
            ] {
                if let Some(w) = w.upgrade() {
                    w.set_visible(on);
                }
            }
            let stand_in = match app.group {
                Group::Elsewhere => Some(not_installed_text(
                    &app.name,
                    app.appid().unwrap_or_default(),
                    scan.steam.missing_libraries(),
                )),
                Group::Custom => Some(added_by_hand_text(&app.name)),
                _ => None,
            };
            if let (Some(away), Some(body)) = (away.upgrade(), away_body.upgrade()) {
                match &stand_in {
                    Some(text) => {
                        body.set_text(text);
                        away.set_visible(true);
                    }
                    None => away.set_visible(false),
                }
            }

            // Read before the blocks are worded, because two of them have
            // something to say about what it holds. A profile that named
            // settings, or that answered the bridge question, and was then
            // silently ignored would be this page telling somebody nothing
            // needs doing while something does.
            //
            // `None` for a game added by hand: a profile is keyed by app id and
            // one of these has not got one, so there is nothing to look up
            // rather than nothing found.
            let verdict = match app.appid() {
                Some(appid) => profile_verdict(profiles::load_from(
                    &scan.profiles_dir,
                    profiles::BUILTIN,
                    appid,
                )),
                None => ProfileVerdict::None,
            };
            let profile = match &verdict {
                ProfileVerdict::Ready(loaded) => Some(&loaded.profile),
                _ => None,
            };

            // --- block 1
            let cfg = tobii_output::games::load_output_config();
            // A poisoned lock is the device thread having panicked, which this
            // page has nothing to say about and must not panic over: the
            // answer it gives then is the default, "nobody has asked for one".
            let joy = joystick
                .lock()
                .map(|g| g.clone())
                .unwrap_or_else(|e| e.into_inner().clone());
            let bridge = profile
                .map(|p| p.bridge)
                .unwrap_or(profiles::Bridge::Unstated);
            let mut text = settings_block(&cfg, bridge, &joy);
            if let Some(note) = profile.and_then(|p| profile_settings_note(&p.settings)) {
                text.push_str("\n\n");
                text.push_str(&note);
            }
            s_body.set_text(&text);

            // --- block 2
            //
            // One walk for both questions. `prefixes` leads with the one
            // `prefix` would pick, so taking the head and keeping the tail
            // answers "which prefix" and "what else is there" off a single
            // pass — where two calls re-stat every library's manifest and
            // every candidate `drive_c`, and the second then filtered out
            // exactly the element the first had returned.
            //
            // A game added by hand skips the walk entirely: its prefix is the
            // one somebody named, Steam has no manifest to consult about it,
            // and asking for "the other prefixes this title has" of a title
            // Steam does not have is a question with no meaning rather than one
            // with an empty answer.
            // Whether this project has watched this very title stop at the
            // signature check. Read once, above block 2, because two things
            // want it: the paragraph `measured_note` prints, and the button
            // `Actions::other_client` offers — and a page whose button and
            // whose prose disagreed about that would be offering a route with
            // no explanation, or an explanation with no route.
            //
            // BOTH lookups. `MEASURED[0]` is Star Citizen, which has no app id
            // because it is not sold on Steam, and it is the title the
            // hand-added list exists for.
            let measured_here = match &app.target {
                Target::Steam(appid) => signature::measured_steam(appid),
                Target::Prefix(_) => signature::measured_named(&app.name),
            };

            let (chosen, others) = match app.appid() {
                Some(appid) => {
                    let mut all = scan.steam.prefixes(appid);
                    let chosen = (!all.is_empty()).then(|| all.remove(0));
                    (chosen, other_prefixes(all))
                }
                None => (app.prefix().cloned(), Vec::new()),
            };
            let state = bridge_state(chosen.as_deref());
            let mut text = bridge_block(
                &state,
                scan.steam.missing_libraries(),
                &others,
                tobii.as_deref(),
                beside.as_deref(),
                &app.target,
            );
            // What is registered in the prefix, which block 2 used to say it
            // could not report. Above the profile and measurement notes,
            // because it is a fact about this machine and they are context.
            if let Some(note) = registered_clients(chosen.as_deref()) {
                text.push_str("\n\n");
                text.push_str(&note);
            }
            let profile_said = profile.and_then(|p| profile_bridge_note(p.bridge));
            let said_the_gate = profile.is_some_and(|p| bridge_says_the_gate(p.bridge));
            if let Some(note) = &profile_said {
                text.push_str("\n\n");
                text.push_str(note);
            }
            // And what was measured about THIS title, which is the paragraph
            // that explains the button the bar is about to show.
            if let Some(note) = measured_note(measured_here, said_the_gate) {
                text.push_str("\n\n");
                text.push_str(&note);
            }
            let job = job_view(
                &app.key(),
                running.borrow().as_ref(),
                outcome.borrow().as_ref(),
                report.borrow().as_ref(),
                &state,
            );
            text.push_str(&job.text);
            br_body.set_text(&text);
            // The bar, from one value. `action` above is what block 2's
            // paragraph was worded from, and `acts.primary` is the same
            // function — see `bridge_action`.
            // Where this row points, which is not the same question as what is
            // in it. A hand-added folder that is not on this machine right now
            // is a drive to plug in, and `bridge_state` over it answers
            // `Absent` — which would offer an install into a directory that is
            // not there.
            let reach = match app.group {
                Group::Elsewhere => Reach::NotListedHere,
                Group::Custom if !app.prefix().is_some_and(|p| p.is_dir()) => Reach::FolderGone,
                _ => Reach::Reachable,
            };
            // Whether this project has watched this very title stop at the
            // signature check — see `Actions::other_client` for why the offer
            // is not made to the rest, and `measured_note` for the paragraph
            // block 2 prints from the same answer.
            //
            // BOTH lookups, and the second is not a nicety: `MEASURED[0]` is
            // Star Citizen, which has no app id because it is not sold on
            // Steam, and it is the title the hand-added list exists for. Keyed
            // on the app id alone, the one route past the check was withheld
            // from the one measured game that can only be reached by folder —
            // while the help topic promised it appears "for a game this project
            // has watched refuse ours at the signature check, and for no
            // other".
            let acts = actions(
                &state,
                tobii.as_deref(),
                &job,
                reach,
                measured_here.is_some(),
                app.group,
            );
            if let Some(btn) = install_w.upgrade() {
                match acts.primary {
                    Some(a) => {
                        crate::widget::set_button_text(&btn, a.caption());
                        btn.set_visible(true);
                    }
                    None => btn.set_visible(false),
                }
            }
            // The same array loop the blocks above use. `install_w` keeps a
            // block of its own because it also sets its caption.
            for (w, on) in [
                (&uninstall_w, acts.uninstall),
                (&details_w, acts.details),
                (&wine_w, acts.wine),
                (&other_w, acts.other_client),
                (&forget_w, acts.forget),
            ] {
                if let Some(b) = w.upgrade() {
                    b.set_visible(on);
                }
            }
            if let Some(b) = action_bar_w.upgrade() {
                b.set_sensitive(job.enabled);
            }
            if let (Some(bar), Some(blocked)) = (bar_w.upgrade(), blocked_w.upgrade()) {
                match acts.blocked {
                    Some(why) => {
                        blocked.set_text(why);
                        blocked.set_visible(true);
                    }
                    None => blocked.set_visible(false),
                }
                // The bar itself appears the moment a game is picked and stays.
                bar.set_visible(true);
            }
            match &job.report {
                Some(r) => {
                    b_report.set_text(r);
                    b_report.set_visible(true);
                }
                None => b_report.set_visible(false),
            }

            // --- block 3
            let mut p_text = profile_intro(
                &verdict,
                &app.name,
                app.appid().unwrap_or_default(),
                &scan.profiles_dir,
            );
            // The same `chosen` block 2 has just warned about, said over the
            // rows that were read out of it.
            if let Some(n) = checked_prefix_note(
                profile.map(|p| p.checks.len()).unwrap_or(0),
                chosen.as_deref(),
                &others,
            ) {
                p_text.push_str("\n\n");
                p_text.push_str(&n);
            }
            p_body.set_text(&p_text);
            while let Some(c) = rows_box.first_child() {
                rows_box.remove(&c);
            }
            if let Some(loaded) = profile {
                for check in &loaded.checks {
                    let (rows, note) = run_check(check, chosen.as_deref());
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
    //
    // There is no handler for block 1. It had one — the button that wrote
    // `enabled` and `joystick` — and deleting it takes `games.toml` from five
    // writers to four. Nothing in this file writes that file any more.

    // The **hub's** lifetime, as one flag. Every poll reads it on its first
    // line and stops when it is false; only the quit path sets it. See
    // [`GamesTab::alive`] for why not `unmap`.
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
            let argv = install_argv(t, &app.target, &InstallWith::default());
            let (refresh2, outcome2, id) = (refresh.clone(), outcome.clone(), app.key());
            start_job(argv, &app.key(), &alive, &running, move |o| {
                *outcome2.borrow_mut() = Some((id.clone(), o));
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
        uninstall_btn.connect_clicked(move |_| {
            let (Some(t), Some(app)) = (tobii.as_ref(), sel.borrow().clone()) else {
                return;
            };
            // The same slot the install writes into, on purpose: one job at a
            // time, and whichever ran last is what the page reports. `Outcome`
            // is already keyed by app id, so an uninstall finishing after the
            // user has moved on lands on the right game or nowhere.
            *outcome.borrow_mut() = None;
            let argv = uninstall_argv(t, &app.target);
            let (refresh2, outcome2, id) = (refresh.clone(), outcome.clone(), app.key());
            start_job(argv, &app.key(), &alive, &running, move |o| {
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
            let argv = status_argv(t, &app.target);
            let (refresh2, report2, id, t2) =
                (refresh.clone(), report.clone(), app.key(), t.clone());
            start_job(argv, &app.key(), &alive, &running, move |o| {
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
        other_btn.connect_clicked(move |b| {
            let (Some(t), Some(app)) = (tobii.as_ref(), sel.borrow().clone()) else {
                return;
            };
            let parent = b.root().and_downcast::<gtk::Window>();
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Pick the folder holding the client's NPClient64.dll");
            dialog.set_modal(true);
            let (t, refresh, running, outcome, alive) = (
                t.clone(),
                refresh.clone(),
                running.clone(),
                outcome.clone(),
                alive.clone(),
            );
            // A folder, because that is what `--npclient` takes: the installer
            // decides which names inside it it needs, and a file picker here
            // would be this window having a second opinion about that.
            dialog.select_folder(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                let Some(dir) = res.ok().and_then(|f| f.path()) else {
                    return;
                };
                *outcome.borrow_mut() = None;
                let argv = install_argv(
                    &t,
                    &app.target,
                    &InstallWith {
                        npclient: Some(dir),
                        ..InstallWith::default()
                    },
                );
                let (refresh2, outcome2, id) = (refresh.clone(), outcome.clone(), app.key());
                start_job(argv, &app.key(), &alive, &running, move |o| {
                    *outcome2.borrow_mut() = Some((id.clone(), o));
                    refresh2();
                });
                refresh();
            });
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
                let argv = install_argv(
                    &t,
                    &app.target,
                    &InstallWith {
                        wine: Some(path),
                        ..InstallWith::default()
                    },
                );
                let (refresh2, outcome2, id) = (refresh.clone(), outcome.clone(), app.key());
                start_job(argv, &app.key(), &alive, &running, move |o| {
                    *outcome2.borrow_mut() = Some((id.clone(), o));
                    refresh2();
                });
                refresh();
            });
        });
    }

    // ---- the pick list

    // What each row of the list is, by position — and the list is rebuilt from
    // scratch on every keystroke, so this is rewritten with it.
    //
    // The whole row and not the app id it used to be, for two readers. The
    // section headings are drawn by `set_header_func`, which is handed a row
    // and has to say what group sits above it — headings as rows was the other
    // way, and it puts a selectable thing in the list that is not a game:
    // `row_activated` is keyed on `row.index()`, so every heading shifts every
    // id under it by one and a click opens the wrong game. And the detail pane
    // is opened from here, which used to mean finding the app id in the scan —
    // exactly what a row of `Group::Elsewhere` is not in.
    let ids: Rc<RefCell<Vec<PickRow>>> = Rc::default();
    {
        let ids = ids.clone();
        list.set_header_func(move |row, before| {
            let ids = ids.borrow();
            let group = |r: &gtk::ListBoxRow| ids.get(r.index().max(0) as usize).map(|p| p.group);
            let Some(mine) = group(row) else {
                return;
            };
            // Only where the group changes, which for the first row is always.
            if before.and_then(group) == Some(mine) {
                row.set_header(None::<&gtk::Widget>);
                return;
            }
            // A heading, and above every one but the first a rule. The four
            // sections were four lines of small grey caps in an unbroken column
            // of rows, so *set up* and *not set up* — which is the division the
            // whole list is sorted by — were told apart by reading, not by
            // looking. The rule is what makes the break a break.
            let h = gtk::Box::new(Orientation::Vertical, 0);
            if before.is_some() {
                let rule = gtk::Box::new(Orientation::Horizontal, 0);
                rule.add_css_class("hairline");
                rule.set_margin_bottom(12);
                h.append(&rule);
            }
            let label = Label::new(Some(mine.heading()));
            label.set_halign(Align::Start);
            label.set_xalign(0.0);
            label.add_css_class("group-heading");
            h.append(&label);
            row.set_header(Some(&h));
        });
    }
    let rebuild: Rc<dyn Fn(&str)> = {
        let (scan, catalog, ids, sel) = (scan.clone(), catalog.clone(), ids.clone(), sel.clone());
        let list_w = list.downgrade();
        let (placeholder, census, notes) = (placeholder.clone(), census.clone(), notes.clone());
        Rc::new(move |q: &str| {
            let p = picker(&catalog.borrow(), scan.steam.missing_libraries(), q);
            *ids.borrow_mut() = p.rows.clone();
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
                // The selection survives the rebuild, by app id. The list is
                // thrown away and built again on every keystroke and every time
                // the tab is shown, and the detail pane beside it goes on
                // showing the game that was picked — so without this the
                // highlight came off the row the pane is about, and typing one
                // letter made the page look like nothing was selected.
                if let Some(picked) = sel.borrow().as_ref() {
                    if let Some(i) = p.rows.iter().position(|r| r.picked() == *picked) {
                        if let Some(row) = list.row_at_index(i as i32) {
                            list.select_row(Some(&row));
                        }
                    }
                }
            }
            placeholder.set_text(p.no_match.as_deref().unwrap_or(""));
            census.set_text(&p.census);
            if p.census_warn {
                census.add_css_class("section-warn");
            } else {
                census.remove_css_class("section-warn");
            }
            // Newlines, not spaces. `directory_notes` returns three separate
            // claims — our bug, somebody else's file, our interrupted save —
            // and joining them with a space glued three deliberately distinct
            // accusations into one blur.
            notes.set_text(&p.notes.join("\n"));
            notes.set_visible(!p.notes.is_empty());
        })
    };
    // Not called here: the list is built on the first `map`, for the reason
    // `catalog` is empty above. 116 widgets on this machine's 29 titles, built
    // and destroyed before the window was drawn.
    {
        let rebuild = rebuild.clone();
        search.connect_search_changed(move |e| rebuild(&e.text()));
    }

    // ---- adding and forgetting a game of one's own
    //
    // The first thing on this tab that writes anything. The module docs promise
    // the tab never writes a game's files and never writes a setting the
    // Tracker tab owns, and both still hold: this writes a list of nicknames
    // that belongs to this program, in this program's own config directory, and
    // `tobii uninstall --purge` knows its name.
    {
        let (catalog, scan, rebuild, refresh) = (
            catalog.clone(),
            scan.clone(),
            rebuild.clone(),
            refresh.clone(),
        );
        let (search_w, notes_top) = (search.downgrade(), add_said.downgrade());
        add_btn.connect_clicked(move |b| {
            let parent = b.root().and_downcast::<gtk::Window>();
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Pick the game's Wine prefix");
            dialog.set_modal(true);
            let (catalog, scan, rebuild, refresh, search_w, notes_w) = (
                catalog.clone(),
                scan.clone(),
                rebuild.clone(),
                refresh.clone(),
                search_w.clone(),
                notes_top.clone(),
            );
            // A FOLDER, and the prefix itself rather than the game's directory:
            // that is what `tobii bridge install --prefix` takes, and it is the
            // directory holding `drive_c`. `name_for_prefix` says so when the
            // folder picked does not look like one — it does not refuse, because
            // a prefix this program does not recognise is still a prefix if the
            // user says so, and refusing would be this window overruling
            // somebody about their own disk.
            dialog.select_folder(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                let Some(dir) = res.ok().and_then(|f| f.path()) else {
                    return;
                };
                let mut games = tobii_config::custom_games::list_from(&scan.custom_games);
                let name = name_for_prefix(&dir);
                // Shown, not only logged. `AddError`'s whole `Display` impl
                // exists to name the entry a duplicate is already under, and
                // swallowing it left the dialog closing with no row added and
                // nothing on screen — indistinguishable from a broken button.
                let saved = tobii_config::custom_games::add(&mut games, &name, &dir)
                    .map_err(|e| e.to_string())
                    .and_then(|()| {
                        tobii_config::custom_games::save_to(&scan.custom_games, &games)
                            .map_err(|e| e.to_string())
                    });
                if let Err(why) = saved {
                    tobii_diagnostics::log::warn(&format!(
                        "could not add {}: {why}",
                        dir.display()
                    ));
                    if let Some(l) = notes_w.upgrade() {
                        l.set_text(&format!("Could not add that folder — {why}"));
                        l.set_visible(true);
                    }
                    return;
                }
                *catalog.borrow_mut() = read_catalog(&scan);
                let q = search_w.upgrade().map(|e| e.text().to_string());
                rebuild(q.as_deref().unwrap_or(""));
                refresh();
            });
        });
    }

    {
        let (catalog, scan, rebuild, sel) =
            (catalog.clone(), scan.clone(), rebuild.clone(), sel.clone());
        let search_w = search.downgrade();
        // The pane goes back to having nothing picked, so the two halves of it
        // swap back. Weak, both: they are siblings of the button this handler
        // is on, under a box it is also under.
        let (intro_w2, game_w2, bar_w2) = (intro.downgrade(), game.downgrade(), bar.downgrade());
        forget_btn.connect_clicked(move |_| {
            let Some(prefix) = sel.borrow().as_ref().and_then(|p| p.prefix().cloned()) else {
                return;
            };
            let mut games = tobii_config::custom_games::list_from(&scan.custom_games);
            if !tobii_config::custom_games::remove(&mut games, &prefix) {
                return;
            }
            if let Err(e) = tobii_config::custom_games::save_to(&scan.custom_games, &games) {
                tobii_diagnostics::log::warn(&format!("could not save the game list: {e}"));
                return;
            }
            // The row it was showing is gone, so the pane goes back to having
            // nothing picked rather than to a game that is no longer in the
            // list. Everything keyed by the old row — a running job, an
            // outcome, a report — is keyed by its prefix and simply stops being
            // read; `refresh` is not called because it returns on the first
            // line with nothing selected, and the widgets are what has to move.
            //
            // The intro comes back, which is the one place in this pane it
            // does. Safe for the reason it was folded carefully in the first
            // place: `help.rs` measured that a pane hidden with the FOCUS
            // inside it holds its subtree for good, and the focus is on this
            // button, which is on the bar and outside both.
            *sel.borrow_mut() = None;
            if let (Some(intro), Some(game), Some(bar)) =
                (intro_w2.upgrade(), game_w2.upgrade(), bar_w2.upgrade())
            {
                game.set_visible(false);
                intro.set_visible(true);
                bar.set_visible(false);
            }
            *catalog.borrow_mut() = read_catalog(&scan);
            let q = search_w.upgrade().map(|e| e.text().to_string());
            rebuild(q.as_deref().unwrap_or(""));
        });
    }

    {
        let (refresh, sel, ids, outcome, report) = (
            refresh.clone(),
            sel.clone(),
            ids.clone(),
            outcome.clone(),
            report.clone(),
        );
        // Weak, every widget: `search` carries a handler of its own that
        // reaches this very list, and `intro` and `game` are siblings of it
        // under a box this list is also under — so a strong reference here
        // would close a sibling cycle that nothing ever breaks, and the tree
        // this is in now lives as long as the hub.
        //
        // The three selectable bodies are here too, and they are re-cleared on
        // every selection change rather than once when a window opened: a
        // selectable `GtkLabel` selects all of its text the moment focus
        // reaches it, focus can pass *through* one on its way somewhere else,
        // and these bodies never fold away any more, so every pass has to be
        // undone. See below for the same call at build time.
        let (intro_w, game_w, detail_w) = (
            intro.downgrade(),
            game.downgrade(),
            detail_scroll.downgrade(),
        );
        let (s_w, br_w, p_w) = (s_body.downgrade(), br_body.downgrade(), p_body.downgrade());
        list.connect_row_activated(move |_, row| {
            let Some(picked) = ids
                .borrow()
                .get(row.index().max(0) as usize)
                .map(PickRow::picked)
            else {
                return;
            };
            *sel.borrow_mut() = Some(picked);
            // A different game's answers are not this game's — and clearing
            // them here is only half of that, because a job still running will
            // write its answer back after this line has run. The other half is
            // the app id each of these carries: see where they are declared.
            *outcome.borrow_mut() = None;
            *report.borrow_mut() = None;
            refresh();
            if let (Some(intro), Some(game), Some(detail)) =
                (intro_w.upgrade(), game_w.upgrade(), detail_w.upgrade())
            {
                // Focus leaves the intro BEFORE it folds away, onto the
                // scroller that holds both. `help.rs` records the measurement:
                // a pane folded away with the focus inside it permanently holds
                // its subtree. It only matters once — the intro never comes
                // back — and it costs one call, so it is made rather than
                // argued about.
                detail.grab_focus();
                intro.set_visible(false);
                game.set_visible(true);
            }
            for body in [&s_w, &br_w, &p_w] {
                if let Some(l) = body.upgrade() {
                    l.select_region(0, 0);
                }
            }
        });
    }

    // A selectable `GtkLabel` selects all of its text the moment focus reaches
    // it, and focus can pass through one on its way somewhere else — so the
    // pane used to open with a paragraph as a solid block of highlight. Taking
    // the focus away does not clear it; this does. The bug `help.rs` records.
    for body in [&s_body, &br_body, &p_body, &b_report, &g_sub, &nothing_body] {
        body.select_region(0, 0);
    }

    // Re-read when the tab comes into view, both halves of the page.
    //
    // **The detail pane**, because block 1 reports settings the other tab owns
    // and the sentence it adds — "turning it on, on the Tracker tab" — is an
    // instruction to go and change one of them. Without this, a user does
    // exactly what it says, comes back, and reads the same sentence, now false,
    // with nothing on screen looking wrong. The modal could not have this bug:
    // it blocked the card, which was the whole of its modality argument.
    // `refresh` re-reads `load_output_config` on every call, so only the
    // trigger was missing.
    //
    // **The list**, because `tobii games profile save <appid>` in a terminal is
    // the documented way to write a profile, and which group a game is in is
    // read off the profiles directory. A list built once would put a game the
    // user had just set up under "Not set up" until the hub was restarted.
    // Only the profile half is re-read — `read_catalog` does a `readdir` of a
    // handful of small files and stats a prefix per profile, not the Steam walk
    // behind it, which `scan` paid for once.
    //
    // The query is taken from the search box rather than reset, so coming back
    // to a filtered list finds it as it was left.
    //
    // Safe for the lifetime: both closures reach every handler-carrying widget
    // through `downgrade()`, so nothing under `content` holds `content` back.
    {
        let (refresh, rebuild, catalog, scan) = (
            refresh.clone(),
            rebuild.clone(),
            catalog.clone(),
            scan.clone(),
        );
        let search_w = search.downgrade();
        content.connect_map(move |_| {
            *catalog.borrow_mut() = read_catalog(&scan);
            let q = search_w.upgrade().map(|e| e.text().to_string());
            rebuild(q.as_deref().unwrap_or(""));
            refresh();
        });
    }

    GamesTab {
        root: content,
        alive,
    }
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

    /// A catalogue with no profiles at all — the two-argument `Catalog::new`
    /// this used to be, which is what most of these tests want.
    ///
    /// The bridge closure panics, and that is an assertion rather than a stub.
    /// `Catalog::new` promises to ask it only of the rows that have a profile,
    /// because it stats three files inside a prefix and the other two groups
    /// can be hundreds of rows long. With no profiles there are no such rows,
    /// so a change that asked it of everything installed fails here — loudly,
    /// and in every test in this module at once — instead of quietly costing a
    /// user a second every time they open the tab.
    fn catalog(apps: &[App], has_prefix: &dyn Fn(&str) -> bool) -> Catalog {
        Catalog::new(
            apps,
            &profiles::Listing::default(),
            &[],
            has_prefix,
            &|id| panic!("the bridge was stat'd for app id {id}, which has no profile"),
        )
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
    fn block2(state: &BridgeState, missing: &[PathBuf], others: &[(PathBuf, bool)]) -> String {
        bridge_block(
            state,
            missing,
            others,
            Some(Path::new("/usr/bin/tobii")),
            Some(Path::new("/opt/x")),
            &Target::Steam("359320".to_string()),
        )
    }

    /// A [`JobView`] with nothing running, for the tests that ask [`actions`]
    /// what the bar offers rather than what a job is doing to it.
    fn quiet_job() -> JobView {
        job_view("359320", None, None, None, &BridgeState::NoPrefix)
    }

    /// Block 1 for a game no profile has anything to say about, with a
    /// joystick nobody has asked for yet. The two arguments the tests that do
    /// not care about them would otherwise repeat.
    fn block1(cfg: &OutputConfig) -> String {
        settings_block(cfg, profiles::Bridge::Unstated, &JoystickStatus::Off)
    }

    // ----------------------------------------------------- the first screen

    /// The pane every new user lands on, and it is a pane and not a page.
    ///
    /// Zero profiles ship, so nobody arrives here with something already set
    /// up: what the Games tab is for has to be readable from a pane with
    /// nothing selected in it. Two states, and they differ in more than their
    /// wording — a machine with a list to pick from is told to pick, and a
    /// machine with no list is not sent looking for one.
    #[test]
    fn the_pane_with_nothing_picked_says_what_to_do_and_only_when_there_is_something_to_do() {
        let tobii = Path::new("/usr/bin/tobii");

        let (head, body, next) = intro_text(Some(tobii), false, &[]);
        assert_eq!(head, HEADING, "it says what the tab is for");
        assert_eq!(
            body,
            lead(Some(tobii)),
            "and the paragraph is the lead, verbatim: it already says what this page \
             will do for a game and what it cannot do without a `tobii`"
        );
        assert_eq!(
            next,
            Some(PICK_ONE),
            "a detail pane with nothing in it has to name the half that fills it, or \
             it reads as a page that failed to load"
        );

        // Nothing installed: the old dead-end page, as a state.
        let (head, body, next) = intro_text(Some(tobii), true, &[]);
        assert_eq!(head, "No games found");
        assert_eq!(body, nothing_text(&[]), "the same words, unchanged");
        assert_eq!(
            next, None,
            "there is nothing on the left to pick, and telling somebody to pick from \
             an empty list is the kind of instruction that reads as a fault in the \
             program"
        );

        // A library that is not plugged in still gets named, in both arms.
        let (_, absent, _) = intro_text(Some(tobii), true, &[PathBuf::from("/mnt/games2")]);
        assert!(
            absent.contains("/mnt/games2"),
            "an empty list with a missing library is not the same claim as an empty \
             list: {absent}"
        );
    }

    // ------------------------------------------------------- the job's watch

    /// What `alive` means now, asserted both ways round.
    ///
    /// It used to mean "the modal is open", and the modal was closed by the
    /// hub's **unmap** handler — that is, by hiding to the tray. So hiding the
    /// hub in the middle of a `tobii bridge install` set this flag, the poll
    /// broke out on its next tick, and the outcome of a child process that ran
    /// to completion was thrown away with nowhere to land. It now means "the
    /// hub is alive" and only the quit path sets it.
    ///
    /// Both halves are here because either one alone can be satisfied by a poll
    /// that does the wrong thing: a poll that never stops passes the first, a
    /// poll that never reports passes the second.
    ///
    /// No widgets and no display — [`start_job`] is a channel, a thread and a
    /// GLib timeout — so this runs in CI.
    ///
    /// It turns the **global default** `MainContext`, and it has to: measured,
    /// `glib::timeout_add_local` calls `g_timeout_add_full`, which attaches to
    /// that one and to no other, so a private context pushed as this thread's
    /// default is never given the source and the poll never runs. This is the
    /// only test in the crate that turns a main context, so nothing else here
    /// is competing for it.
    #[test]
    fn the_job_is_watched_until_the_hub_goes_and_then_not() {
        let ctx = glib::MainContext::default();
        // The context is turned by hand rather than by a `MainLoop`, and the
        // turning is bounded: a poll that never reports has to fail this test
        // by asserting, not by hanging the suite until somebody kills it.
        let turn = |until: &dyn Fn() -> bool, ms: u64| {
            let deadline = std::time::Instant::now() + Duration::from_millis(ms);
            while !until() && std::time::Instant::now() < deadline {
                ctx.iteration(false);
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let argv = || vec![OsString::from("/bin/echo"), OsString::from("ok")];

        // 1. A hub that is still there — which now includes one hidden in the
        //    tray — gets the answer.
        let alive: Rc<Cell<bool>> = Rc::new(Cell::new(true));
        let running: Rc<RefCell<Option<(String, String)>>> = Rc::default();
        let got: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        {
            let got = got.clone();
            start_job(argv(), "359320", &alive, &running, move |_| got.set(true));
        }
        turn(&|| got.get(), 5000);
        assert!(
            got.get(),
            "the installer finished and nothing reported it, so the page would sit \
             on 'Running now:' for ever"
        );
        assert!(
            running.borrow().is_none(),
            "and the slot is cleared, or the buttons stay greyed out"
        );

        // 2. A hub that has quit does not. The child still runs to completion —
        //    nothing here kills it — but there is nobody left to tell.
        let alive: Rc<Cell<bool>> = Rc::new(Cell::new(true));
        let running: Rc<RefCell<Option<(String, String)>>> = Rc::default();
        let got: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        {
            let got = got.clone();
            start_job(argv(), "359320", &alive, &running, move |_| got.set(true));
        }
        alive.set(false);
        turn(&|| false, 1500);
        assert!(
            !got.get(),
            "the poll reported into a hub that has quit, which is a closure holding \
             widgets of a window that is being torn down"
        );
    }

    // ------------------------------------------------------- the action bar

    /// The bar answers with a BUTTON or with a SENTENCE, and never with
    /// nothing.
    ///
    /// The buttons lived in block 2 and were simply hidden when there was
    /// nothing to press, so a game with no prefix — or a machine with no
    /// `tobii` — got a blank strip and a reason buried at the end of a
    /// paragraph. Pinned under the pane, the bar is the first thing a user
    /// looks at to find out whether this page can do anything, so an empty one
    /// has to say why it is empty.
    #[test]
    fn the_bar_always_answers_either_with_a_button_or_with_a_reason() {
        let tobii = Path::new("/usr/bin/tobii");
        let cases = [
            (
                "not installed here",
                present(),
                Some(tobii),
                Reach::NotListedHere,
            ),
            (
                "a folder that is gone",
                present(),
                Some(tobii),
                Reach::FolderGone,
            ),
            (
                "no prefix",
                BridgeState::NoPrefix,
                Some(tobii),
                Reach::Reachable,
            ),
            ("no tobii", present(), None, Reach::Reachable),
            (
                "a prefix without it",
                absent(),
                Some(tobii),
                Reach::Reachable,
            ),
            ("a prefix with it", present(), Some(tobii), Reach::Reachable),
        ];
        for (what, state, t, reach) in cases {
            let a = actions(&state, t, &quiet_job(), reach, false, Group::NotSetUp);
            // The BRIDGE buttons. `forget` is not one of them and is
            // deliberately outside this rule: it takes the row out of this
            // program's own list, which is the answer to a folder that has
            // gone rather than something the folder's absence forbids — so a
            // hand-added row can legitimately show it beside a reason.
            let has_button = a.primary.is_some() || a.uninstall || a.details || a.wine;
            assert_ne!(
                has_button,
                a.blocked.is_some(),
                "{what}: the bar has {} button(s) and {} a reason — it must have exactly \
                 one of the two: {a:?}",
                if has_button { "some" } else { "no" },
                if a.blocked.is_some() {
                    "has"
                } else {
                    "has not"
                },
            );
        }
    }

    /// The three refusals, in the order they have to be answered.
    ///
    /// A game Steam does not list here has no prefix either, and a machine
    /// with no `tobii` would still have nothing to install into — so answering
    /// the second or third over the first sends somebody to fix the wrong
    /// thing. Each case below is true of every refusal after it.
    #[test]
    fn the_bar_names_the_first_reason_and_not_a_later_one() {
        let no_tobii_no_prefix = actions(
            &BridgeState::NoPrefix,
            None,
            &quiet_job(),
            Reach::Reachable,
            false,
            Group::NotSetUp,
        );
        assert!(
            no_tobii_no_prefix
                .blocked
                .is_some_and(|w| w.contains("No Proton prefix")),
            "a missing prefix outranks a missing program: {no_tobii_no_prefix:?}"
        );
        let elsewhere = actions(
            &BridgeState::NoPrefix,
            None,
            &quiet_job(),
            Reach::NotListedHere,
            false,
            Group::Elsewhere,
        );
        assert!(
            elsewhere
                .blocked
                .is_some_and(|w| w.contains("does not list this game")),
            "and a game that is not on this machine outranks both: {elsewhere:?}"
        );
    }

    /// Uninstall is offered over a prefix that holds the artifact and nowhere
    /// else.
    ///
    /// There is nothing to take out of a prefix that has not got it, and a
    /// button that can only fail is the thing block 2 has always refused to
    /// show. The mirror matters as much: a prefix that HAS it must offer the
    /// way back out, or the only way to undo an install is a terminal.
    #[test]
    fn taking_the_bridge_back_out_is_offered_exactly_where_there_is_something_to_remove() {
        let tobii = Path::new("/usr/bin/tobii");
        assert!(
            actions(
                &present(),
                Some(tobii),
                &quiet_job(),
                Reach::Reachable,
                false,
                Group::NotSetUp,
            )
            .uninstall
        );
        assert!(
            !actions(
                &absent(),
                Some(tobii),
                &quiet_job(),
                Reach::Reachable,
                false,
                Group::NotSetUp,
            )
            .uninstall
        );
        assert!(
            !actions(
                &BridgeState::NoPrefix,
                Some(tobii),
                &quiet_job(),
                Reach::Reachable,
                false,
                Group::NotSetUp,
            )
            .uninstall
        );
        // Forget follows the group and nothing else — in particular it
        // survives every refusal, because removing a row is the one thing that
        // still works when the folder it names is gone.
        let tobii = Path::new("/usr/bin/tobii");
        for (reach, group, want) in [
            (Reach::Reachable, Group::Custom, true),
            (Reach::FolderGone, Group::Custom, true),
            (Reach::Reachable, Group::NotSetUp, false),
            (Reach::NotListedHere, Group::Elsewhere, false),
        ] {
            let a = actions(&present(), Some(tobii), &quiet_job(), reach, false, group);
            assert_eq!(a.forget, want, "{group:?} / {reach:?}: {a:?}");
        }

        // And it is a different verb from install, not a flag on it.
        let argv = uninstall_argv(tobii, &Target::Steam("359320".to_string()));
        assert_eq!(
            argv.iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>(),
            vec!["/usr/bin/tobii", "bridge", "uninstall", "--steam", "359320"],
        );
    }

    /// A game this project has measured at the check is told so, with no
    /// profile needed.
    ///
    /// This is the hole the button was opened over. Block 2 carried the
    /// signature gate only when a profile said `bridge = required`, and
    /// `profiles::BUILTIN` is empty — so on MSFS 2024, the one Steam title
    /// measured calling that check 104 times and nothing else, the page said
    /// nothing about it at all while the bar offered a button whose whole
    /// reason is that measurement.
    #[test]
    fn a_measured_game_is_told_what_was_measured_about_it_without_a_profile() {
        let msfs = signature::measured_steam("2537590").expect("measured");
        let note = measured_note(Some(msfs), false).expect("a measured title gets a paragraph");
        assert!(
            note.contains("watched this very game"),
            "about THIS title, not games in general: {note}"
        );
        assert!(note.contains("104 times"), "the measurement itself: {note}");
        assert!(
            note.contains(&signature::trackir_gate()),
            "and the gate, because no profile said it: {note}"
        );
        assert!(
            note.contains(&signature::provider_note()),
            "and the second wall, which the button does not get past: {note}"
        );

        // A profile that already printed the gate does not get it twice.
        let twice = measured_note(Some(msfs), true).expect("still a paragraph");
        assert!(!twice.contains(&signature::trackir_gate()), "{twice}");
        assert!(
            twice.contains("104 times"),
            "but still the measurement: {twice}"
        );

        // And a game nobody has put to the check gets no paragraph at all —
        // this page does not predict what an unmeasured game will do.
        assert_eq!(measured_note(None, false), None);
    }

    /// The one route measured to get past the signature check is offered to
    /// the titles measured to stop at it, and to nothing else.
    ///
    /// MSFS 2024 calls `NP_GetSignature` and nothing else, for as long as it
    /// runs; our client cannot answer it and the material that would is not
    /// ours. A separately installed client can, and this is the button that
    /// installs one — which is the difference between a user reading a
    /// paragraph about their problem and being able to do something about it.
    ///
    /// Not offered to every game, and that is the load-bearing half. Our
    /// client is the right default; block 2 already names the route for
    /// anybody whose game turns out to gate; and a button putting a
    /// third-party binary into every prefix on the machine would be this page
    /// recommending something nobody has watched work there — over a game that
    /// very likely never asks for a signature at all.
    #[test]
    fn another_client_is_offered_to_a_measured_title_and_to_no_other() {
        let tobii = Path::new("/usr/bin/tobii");
        let bar = |measured| {
            actions(
                &absent(),
                Some(tobii),
                &quiet_job(),
                Reach::Reachable,
                measured,
                Group::NotSetUp,
            )
        };
        assert!(
            bar(true).other_client,
            "a title we watched stop at the check"
        );
        assert!(!bar(false).other_client, "and not the rest of the machine");

        // And the flag it builds. A DIRECTORY, because that is what the
        // installer takes — which names inside it are needed is its business.
        let argv: Vec<String> = install_argv(
            tobii,
            &Target::Steam("2537590".to_string()),
            &InstallWith {
                npclient: Some(PathBuf::from("/opt/opentrack/win32")),
                ..InstallWith::default()
            },
        )
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
        assert_eq!(
            argv,
            vec![
                "/usr/bin/tobii",
                "bridge",
                "install",
                "--steam",
                "2537590",
                "--npclient",
                "/opt/opentrack/win32"
            ]
        );

        // Through the function the offer actually calls, not by walking the
        // list beside it: change how `measured_steam` matches and a hand-rolled
        // scan here would go on asserting the raw data and stay green while the
        // button's behaviour moved.
        assert!(
            signature::measured_steam("2537590").is_some(),
            "the offer keys on this lookup, so it has to answer for MSFS"
        );
        // And the half that has no app id. Star Citizen is measured, is not on
        // Steam, and is the title the hand-added list exists for — so the row
        // that can only be reached by folder is the one this button must not
        // be withheld from.
        assert!(
            signature::measured_named("Star Citizen").is_some(),
            "the measured title with no app id is reachable by the name a user typed"
        );
    }

    // --------------------------------------------------- games added by hand

    /// A game somebody pointed at a prefix is a row of its own group, keyed by
    /// the path and not by an app id.
    ///
    /// The whole point of the fourth group: `tobii bridge install --prefix
    /// PATH` has always worked from a terminal, and what the hub could not do
    /// was remember the path. So the row has to carry it — an app id it has not
    /// got cannot be what the page is keyed by.
    #[test]
    fn a_game_added_by_hand_is_keyed_by_its_prefix() {
        let custom = [tobii_config::custom_games::CustomGame {
            name: "Star Citizen".to_string(),
            prefix: PathBuf::from("/games/sc/pfx"),
        }];
        let c = Catalog::new(&[], &profiles::Listing::default(), &custom, &none, &|_| {
            RowBridge::Installed
        });
        let rows = picker(&c, &[], "").rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].group, Group::Custom);
        assert_eq!(Group::Custom.heading(), "Added by hand");

        let picked = rows[0].picked();
        assert_eq!(picked.appid(), None, "there is no app id to have");
        assert_eq!(picked.key(), "/games/sc/pfx", "so the path is the key");
        assert_eq!(picked.prefix(), Some(&PathBuf::from("/games/sc/pfx")));

        // And that is what the installer is pointed at — `--prefix`, never
        // `--steam` with a path after it.
        let argv: Vec<String> = install_argv(
            Path::new("/usr/bin/tobii"),
            &picked.target,
            &InstallWith::default(),
        )
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
        assert_eq!(
            argv,
            vec![
                "/usr/bin/tobii",
                "bridge",
                "install",
                "--prefix",
                "/games/sc/pfx"
            ]
        );
    }

    /// Each group shows the blocks that have an answer for it, and every
    /// suppression is a sentence rather than a gap.
    ///
    /// Two different shapes, and they are nearly opposites — which is the
    /// reason this is a function and not an `if` in the middle of `refresh`. A
    /// game Steam does not list here loses the first two (no prefix on this
    /// machine, nothing to send to it from here) and keeps the third, whose
    /// rows answer `NoPrefix` truthfully. A game added by hand keeps the first
    /// two and loses the third, because block 3 reads a profile and a profile
    /// is named after an app id it has not got.
    #[test]
    fn every_group_shows_the_blocks_that_have_an_answer_for_it() {
        for g in [Group::SetUp, Group::NotSetUp] {
            assert_eq!(
                blocks(g),
                Blocks {
                    settings: true,
                    bridge: true,
                    game: true
                },
                "{g:?}"
            );
        }
        assert_eq!(
            blocks(Group::Elsewhere),
            Blocks {
                settings: false,
                bridge: false,
                game: true
            }
        );
        assert_eq!(
            blocks(Group::Custom),
            Blocks {
                settings: true,
                bridge: true,
                game: false
            }
        );
        // The stand-in for the third block says why it is not there, and does
        // not send the reader off to write a profile they cannot name.
        let t = added_by_hand_text("Star Citizen");
        assert!(t.contains("Star Citizen"), "{t}");
        assert!(
            t.contains("named after a Steam app id"),
            "the reason, not just the absence: {t}"
        );
        assert!(
            !t.contains("tobii games profile save"),
            "that command takes an app id, and this game has not got one: {t}"
        );
    }

    /// A folder that is not on this machine right now is its own refusal.
    ///
    /// `bridge_state` over a prefix that is not there answers `Absent` — the
    /// same as a prefix that exists and has no bridge in it — so without this
    /// the bar offered *Install the bridge* into a directory that is not there,
    /// and the install would have failed in a subprocess.
    #[test]
    fn a_hand_added_folder_that_is_gone_is_not_offered_an_install() {
        let a = actions(
            &absent(),
            Some(Path::new("/usr/bin/tobii")),
            &quiet_job(),
            Reach::FolderGone,
            false,
            Group::Custom,
        );
        assert_eq!(a.primary, None);
        assert!(
            a.blocked
                .is_some_and(|w| w.contains("not on this machine right now")),
            "and it reads as a drive to plug in, not as a game that is gone: {a:?}"
        );
    }

    /// The name a picked folder starts with is the game's, not `pfx`.
    ///
    /// Every Proton prefix on a machine is called `pfx`, and a list of six
    /// games all called `pfx` is a list nobody can use. It is a starting point
    /// and nothing more — the file is plain text and the name in it is display
    /// only.
    #[test]
    fn a_picked_folder_is_named_after_the_game_and_not_after_the_prefix() {
        assert_eq!(
            name_for_prefix(Path::new("/games/star-citizen/pfx")),
            "star-citizen"
        );
        assert_eq!(
            name_for_prefix(Path::new("/games/star-citizen/pfx/drive_c")),
            "pfx"
        );
        assert_eq!(
            name_for_prefix(Path::new("/games/Elden Ring")),
            "Elden Ring"
        );
        // Nothing to take a name from at all still yields something to show.
        assert_eq!(name_for_prefix(Path::new("/")), "/");
    }

    /// The fourth group's rows are reachable by what identifies them.
    ///
    /// They have no app id, so a search that only matched a name and an app id
    /// could not reach one by the folder it is in — and the folder is the thing
    /// on screen under the name.
    #[test]
    fn a_hand_added_game_is_findable_by_its_folder() {
        let custom = [tobii_config::custom_games::CustomGame {
            name: "Star Citizen".to_string(),
            prefix: PathBuf::from("/mnt/big/sc/pfx"),
        }];
        let c = Catalog::new(&[], &profiles::Listing::default(), &custom, &none, &|_| {
            RowBridge::Installed
        });
        assert_eq!(picker(&c, &[], "mnt/big").rows.len(), 1, "by its folder");
        assert_eq!(picker(&c, &[], "citizen").rows.len(), 1, "and by its name");
        assert!(picker(&c, &[], "elden").rows.is_empty());
    }

    // ------------------------------------------------------------ the groups

    /// A `Listing` with a profile per app id given, each naming itself.
    fn listed(entries: &[(&str, Option<&str>)]) -> profiles::Listing {
        profiles::Listing {
            profiles: entries
                .iter()
                .map(|(id, name)| {
                    (
                        (*id).to_string(),
                        profiles::Loaded {
                            origin: profiles::Origin::File(PathBuf::from(format!("/p/{id}.toml"))),
                            profile: profiles::Profile {
                                name: name.map(str::to_string),
                                ..profiles::Profile::default()
                            },
                        },
                    )
                })
                .collect(),
            ..profiles::Listing::default()
        }
    }

    /// The whole point of the three groups, in one assertion: what a user came
    /// to this tab to find out is *what have I set up*, so it is at the top,
    /// the machine's catalogue is under it, and the profiles Steam knows
    /// nothing about are under that.
    ///
    /// The third group is the one that could not exist before. A profile for a
    /// game on a drive that is not plugged in, or for one that has been
    /// uninstalled, is in no list built from Steam's manifests — so the file
    /// sat in the profiles directory being applied to nothing, and no screen in
    /// this program said it was there.
    #[test]
    fn the_list_answers_what_is_set_up_before_it_answers_what_is_installed() {
        let apps = [app("1", "Bravo"), app("2", "Alpha"), app("3", "Charlie")];
        // A profile for one installed title, and one for a title that is not.
        let listing = listed(&[("2", None), ("77", Some("Gone Fishing"))]);
        let c = Catalog::new(&apps, &listing, &[], &none, &|_| RowBridge::Installed);
        let p = picker(&c, &[], "");
        let seen: Vec<(&str, Group)> = p.rows.iter().map(|r| (r.name.as_str(), r.group)).collect();
        assert_eq!(
            seen,
            vec![
                ("Alpha", Group::SetUp),
                ("Bravo", Group::NotSetUp),
                ("Charlie", Group::NotSetUp),
                ("Gone Fishing", Group::Elsewhere),
            ],
            "the groups, in order, with each group in its own order: {p:#?}"
        );
        // And the headings the sections are drawn under, which are the only
        // words on the list that say what any of this means.
        assert_eq!(
            [Group::SetUp, Group::NotSetUp, Group::Elsewhere].map(Group::heading),
            ["Set up", "Not set up", "Set up, not installed here"],
        );
    }

    /// The census counts what Steam says is installed, and the third group is
    /// by definition not that.
    ///
    /// It was `rows.len()`, which was the same number until there was a third
    /// group — and then a user with one profile for an uninstalled game was
    /// told they had one more title installed than they have, in the one
    /// sentence on the page whose whole subject is Steam's manifests.
    #[test]
    fn a_profile_for_a_game_that_is_not_here_is_not_counted_as_installed() {
        let apps = [app("1", "Bravo")];
        let c = Catalog::new(&apps, &listed(&[("77", None)]), &[], &none, &|_| {
            RowBridge::Installed
        });
        let p = picker(&c, &[], "");
        assert_eq!(p.rows.len(), 2, "both rows are shown: {p:#?}");
        assert!(
            p.census.starts_with("1 title installed"),
            "one is installed and the other is the reason this group exists: {}",
            p.census
        );
    }

    /// A row of the third group is named by the profile when the profile says a
    /// name, and by its app id when it does not.
    ///
    /// There is no other source. The one place that knows what a title is
    /// called is the manifest on the machine that has it, which is precisely
    /// the machine this is not — so a row with no name in its profile has to
    /// read as an app id rather than as a blank or as a guess.
    #[test]
    fn a_game_that_is_not_installed_here_is_named_by_its_profile_or_by_its_id() {
        let c = Catalog::new(
            &[],
            &listed(&[("77", Some("Gone Fishing")), ("88", None)]),
            &[],
            &none,
            &|_| RowBridge::Installed,
        );
        let rows = picker(&c, &[], "").rows;
        let named: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.name.as_str(), r.subtitle.as_str()))
            .collect();
        assert_eq!(
            named,
            // Name order, and "app id 88" is a name here: it is the only one
            // that title has on this machine, so it sorts with the others
            // rather than being pushed to an end of its own.
            vec![
                ("app id 88", "app id 88 · not installed on this machine"),
                ("Gone Fishing", "app id 77 · not installed on this machine"),
            ],
            "{rows:#?}"
        );
    }

    /// The *Set up* subtitle says what there is to do next, and the three
    /// states are three different answers.
    ///
    /// "Not installed" over a game that has never been launched reads as
    /// something to go and press, and there is nothing to press: there is no
    /// prefix to install into until Proton has made one. Block 2 draws the same
    /// distinction in a paragraph; this is the one-word version, and it is read
    /// off the same [`bridge_state`] so the two cannot disagree.
    #[test]
    fn a_set_up_row_says_which_of_the_three_bridge_states_it_is_in() {
        let apps = [app("1", "One"), app("2", "Two"), app("3", "Three")];
        let listing = listed(&[("1", None), ("2", None), ("3", None)]);
        let c = Catalog::new(&apps, &listing, &[], &none, &|id| match id {
            "1" => RowBridge::Installed,
            "2" => RowBridge::NotInstalled,
            _ => RowBridge::NeverLaunched,
        });
        let subs: Vec<String> = picker(&c, &[], "")
            .rows
            .iter()
            .map(|r| r.subtitle.clone())
            .collect();
        assert_eq!(
            subs,
            vec![
                "profile · bridge installed",
                "profile · never launched",
                "profile · bridge not installed",
            ],
            "in name order — One, Three, Two"
        );
    }

    /// The one-word answer on a row is read off the same value block 2's
    /// paragraph is written from.
    ///
    /// The row's own doc says "a row and the page it opens cannot disagree
    /// about a prefix they both stat'd", and nothing was holding it: every
    /// other test in this module injects a `RowBridge` variant straight into
    /// `Catalog::new`, so `RowBridge::of` could have been rewritten to return
    /// the wrong variant for every arm with the whole suite still green.
    ///
    /// All three arms, because the mapping is the claim. `Files` is
    /// "bridge installed" even when only the required artifact is there, and
    /// that is deliberate rather than a rounding: `present_sentence` says the
    /// same of the same state, and the other two files are ones a `tobii` built
    /// without them skips and says so.
    #[test]
    fn a_rows_one_word_answer_is_read_off_the_state_the_page_is_worded_from() {
        for (state, want) in [
            (BridgeState::NoPrefix, RowBridge::NeverLaunched),
            (absent(), RowBridge::NotInstalled),
            (present(), RowBridge::Installed),
            (files_with(vec![BRIDGE_ARTIFACT]), RowBridge::Installed),
        ] {
            assert_eq!(RowBridge::of(&state), want, "{state:?}");
        }
        // And the three clauses, which are what a reader sees.
        assert_eq!(
            [
                RowBridge::NeverLaunched,
                RowBridge::Installed,
                RowBridge::NotInstalled
            ]
            .map(RowBridge::clause),
            ["never launched", "bridge installed", "bridge not installed"],
        );
    }

    /// The expensive question is asked of the rows that need it and of nothing
    /// else.
    ///
    /// `bridge` stats three files inside a prefix. The *Set up* group is
    /// usually a handful of rows; the catalogue is 29 on this machine and can
    /// be hundreds, and asking all of them would put a per-title directory walk
    /// in the path of opening a tab. The count, not just "it was not asked of a
    /// row without a profile": once per row is also the promise, and a refresh
    /// that asked twice would still pass a check that only looked for zero.
    #[test]
    fn the_bridge_is_stat_ed_once_for_a_set_up_row_and_never_for_any_other() {
        let apps = [app("1", "One"), app("2", "Two"), app("3", "Three")];
        let asked = std::cell::RefCell::new(Vec::new());
        let c = Catalog::new(&apps, &listed(&[("2", None)]), &[], &none, &|id| {
            asked.borrow_mut().push(id.to_string());
            RowBridge::Installed
        });
        assert_eq!(
            *asked.borrow(),
            vec!["2".to_string()],
            "only the row with a profile, and only once"
        );
        drop(c);
    }

    /// What the pane says for a game this machine has a profile for and Steam
    /// does not list.
    ///
    /// The two blocks it replaces would both answer, and both answers would be
    /// wrong in the same way: block 2 runs `bridge_state` over no prefix and
    /// prints "launch it once and come back", which is true of an installed
    /// game that has never been started and false of a drive that is not
    /// plugged in.
    #[test]
    fn the_page_for_a_game_that_is_not_here_does_not_tell_you_to_launch_it() {
        let t = not_installed_text("Gone Fishing", "77", &[]);
        assert!(t.contains("Gone Fishing") && t.contains("77"), "{t}");
        assert!(
            t.contains("does not list that title as installed on this machine"),
            "it says the one thing that is true: {t}"
        );
        assert!(
            t.contains("The profile is not lost"),
            "and that the file still counts, which is the reason somebody is \
             looking at this row at all: {t}"
        );
        // The exact sentences block 2 would have printed. Not a class — a
        // paragraph that said "install it and come back" would sail through —
        // but these are the words a reader of this row would have been given,
        // and the reason this paragraph exists.
        for claim in ["run it once and come back", "No Proton prefix was found"] {
            assert!(
                !t.contains(claim),
                "{claim:?} is block 2's answer over an absent prefix, and it is not \
                 true of a drive that is not plugged in: {t}"
            );
        }
        // And the missing-library sentence when there is one, because a drive
        // that is not plugged in is the commonest way to land in this group.
        let with = not_installed_text("Gone Fishing", "77", &[PathBuf::from("/mnt/games2")]);
        assert!(with.contains("/mnt/games2"), "{with}");
    }

    // --------------------------------------------- the profiles directory

    /// Three claims, kept apart, because they are three different things and
    /// one of them accuses the user of something.
    ///
    /// A broken profile is this program failing to read what it wrote; a stray
    /// is somebody else's file, which this program will not touch; a leftover
    /// is a save of ours that was cut short. The CLI's own listing prints them
    /// as three paragraphs for exactly this reason, and folding them together
    /// here would be this page and that one disagreeing about what is in a
    /// directory they both read.
    #[test]
    fn the_directory_notes_keep_the_three_claims_apart() {
        assert!(
            directory_notes(&profiles::Listing::default()).is_empty(),
            "nothing to say on the machine almost everybody has"
        );

        let one = directory_notes(&profiles::Listing {
            problems: vec![profiles::LoadError::Unreadable {
                origin: profiles::Origin::File(PathBuf::from("/p/1.toml")),
                why: "Permission denied".to_string(),
            }],
            strays: vec!["notes.txt".to_string()],
            leftovers: vec!["2.toml.tmp".to_string()],
            ..profiles::Listing::default()
        });
        assert_eq!(
            one.len(),
            4,
            "three claims and where the names are: {one:#?}"
        );
        assert!(
            one[0].contains("1 profile in the profiles directory"),
            "{one:#?}"
        );
        assert!(
            one[1].contains("not a profile and was not written by this program"),
            "a stray is somebody else's file: {one:#?}"
        );
        assert!(
            one[2].contains("written by this program") && one[2].contains("cut short"),
            "and a leftover is ours: {one:#?}"
        );
        assert!(
            one[3].contains("tobii games profile list") && one[3].contains("--purge"),
            "the names are one command away, and with a leftover there is a second \
             command that removes it: {one:#?}"
        );

        // And `--purge` is offered only where there is something for it to
        // remove. A stray is somebody else's file and `--purge` leaves it
        // exactly where it is, so naming it beside a line about strays alone
        // would be this page promising a tidy-up that does not happen.
        let strays_only = directory_notes(&profiles::Listing {
            strays: vec!["notes.txt".to_string()],
            ..profiles::Listing::default()
        });
        assert_eq!(strays_only.len(), 2, "{strays_only:#?}");
        assert!(
            !strays_only[1].contains("--purge"),
            "`--purge` does not touch a file this program did not write: {strays_only:#?}"
        );

        // Plurals, because three sentences with four agreements between them is
        // where "1 files is not a profile" comes from.
        let many = directory_notes(&profiles::Listing {
            strays: vec!["a".to_string(), "b".to_string()],
            leftovers: vec!["c.toml.tmp".to_string(), "d.toml.tmp".to_string()],
            ..profiles::Listing::default()
        });
        assert!(
            many[0].contains("2 files in that directory are not a profile and were not"),
            "{many:#?}"
        );
        assert!(
            many[1].contains("2 files there were written by this program and are not"),
            "{many:#?}"
        );
    }

    // ------------------------------------------------------------ the picker

    /// The moment a user has typed the name of a game they own and got nothing
    /// back is the moment a silent list becomes a lie. The census line alone is
    /// not enough: it is above the fold and it is not what they are reading.
    #[test]
    fn a_search_that_finds_nothing_still_names_the_library_that_is_not_here() {
        let apps = [app("1", "Something Else")];
        let missing = [PathBuf::from("/mnt/games2")];
        let p = picker(&catalog(&apps, &none), &missing, "Elite");
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
        let p = picker(&catalog(&apps, &none), &[], "Elite");
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
        let p = picker(&catalog(&apps, &none), &[], "");
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
            picker(&catalog(&apps, &none), &[], q)
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
        let p = picker(&catalog(&apps, &|id| id == "1"), &[], "");
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

    /// Nothing that costs anything is redone as somebody types.
    ///
    /// `has_prefix` reaches a stat of a Proton prefix, and the search box asks
    /// [`picker`] for a new page on every keystroke — so asked there, typing
    /// "elite" over 29 installed titles would be 174 of them. It is asked once
    /// per title when the [`Catalog`] is built, and [`picker`] cannot ask it
    /// again because it is no longer given it.
    ///
    /// The order is pinned in the same case, because the other half of the
    /// same claim is that nothing about the order depends on the query: a
    /// filtered page has to be the unfiltered one with rows taken out, in
    /// place, or the sort could not have happened before the typing.
    #[test]
    fn the_prefix_is_stat_once_per_title_and_the_order_survives_every_keystroke() {
        // `Alpha Elite` is what makes the order half of this a test: it
        // matches "elite" and sorts ahead of the two titles that begin with
        // it, so any ranking by how well a row matches would move it and the
        // page would no longer be the same list with rows taken out.
        let apps = [
            app("1", "Elite Dangerous"),
            app("2", "Proton 9.0"),
            app("3", "Empyrion"),
            app("4", "elite squadron"),
            app("5", "Alpha Elite"),
        ];
        let asked = std::cell::RefCell::new(Vec::new());
        let catalog = catalog(&apps, &|id: &str| {
            asked.borrow_mut().push(id.to_string());
            id == "1"
        });
        let unfiltered: Vec<String> = picker(&catalog, &[], "")
            .rows
            .iter()
            .map(|r| r.appid.clone())
            .collect();
        for q in ["e", "el", "eli", "elit", "elite", "", "9", "danger"] {
            let rows: Vec<String> = picker(&catalog, &[], q)
                .rows
                .iter()
                .map(|r| r.appid.clone())
                .collect();
            let mut want = unfiltered.iter();
            assert!(
                rows.iter().all(|id| want.any(|u| u == id)),
                "{q:?} reordered the list rather than narrowing it: {rows:?} \
                 is not a run of {unfiltered:?}"
            );
        }
        assert_eq!(
            asked.borrow().len(),
            apps.len(),
            "one stat per installed title and not one per keystroke: {:?}",
            asked.borrow()
        );
    }

    // ---------------------------------------------------------- the settings

    #[test]
    fn a_config_that_needs_nothing_says_so() {
        let body = block1(&cfg(true, true, None));
        assert!(body.contains("Nothing to change here"), "{body}");
    }

    /// Block 1 has no controls, so every state of it has to end by saying where
    /// the controls are.
    ///
    /// All four: the two "on" states, the "off" state, and the one with a
    /// joystick that failed. A block of prose about settings, on a page with
    /// nothing on it to press, reads as a page that has lost its buttons — and
    /// this one really did lose one.
    #[test]
    fn every_state_of_block_one_points_at_the_tab_that_owns_the_settings() {
        let failed = JoystickStatus::Failed("/dev/uinput: Permission denied".to_string());
        for (what, body) in [
            ("on, with a joystick", block1(&cfg(true, true, None))),
            ("on, without one", block1(&cfg(true, false, None))),
            ("off", block1(&cfg(false, false, Some("127.0.0.1:4242")))),
            (
                "on, joystick refused",
                settings_block(&cfg(true, true, None), profiles::Bridge::Unstated, &failed),
            ),
        ] {
            assert!(
                body.contains(TRACKER_TAB_POINTER),
                "{what}: block 1 reports settings it cannot change and never says \
                 where they are changed: {body}"
            );
        }
    }

    /// The sentence that named the button it stood above.
    ///
    /// It said "turning it on starts all of that, not only what the button
    /// below adds", and there is no button below any more. What has to survive
    /// the rewording is the reason that clause existed: the switch on the other
    /// tab starts *every* destination already in the file, not the one the
    /// reader has in mind.
    #[test]
    fn the_off_state_names_the_switch_that_starts_everything_and_not_a_button() {
        let mut c = cfg(false, false, Some("127.0.0.1:4242"));
        c.bridge_port = Some(4243);
        let body = block1(&c);
        assert!(
            body.contains("on the Tracker tab, starts all of that"),
            "the one control that turns this on is named, and it is not here: {body}"
        );
        assert!(
            !body.contains("button below"),
            "there is no button below: {body}"
        );
        assert!(
            body.contains("127.0.0.1:4242") && body.contains("4243"),
            "and every destination the press will start is named before it is pressed: \
             {body}"
        );
    }

    /// The strength word comes from the presets, never from comparing degrees
    /// in this file.
    #[test]
    fn the_strength_word_comes_from_the_presets() {
        for (name, yaw, pitch) in STRENGTHS {
            let mut c = cfg(true, true, None);
            c.extended_view.yaw.output_max_deg = yaw;
            c.extended_view.pitch.output_max_deg = pitch;
            let body = block1(&c);
            assert!(body.contains(name), "{name} should be named: {body}");
        }
        let mut c = cfg(true, true, None);
        c.extended_view.yaw.output_max_deg = 33.3;
        let body = block1(&c);
        assert!(body.contains("hand-tuned"), "{body}");
        for (name, _, _) in STRENGTHS {
            assert!(
                !body.contains(name),
                "a hand-tuned config claims no preset: {body}"
            );
        }
    }

    /// Somebody who already set up opentrack has game output ON. Telling them
    /// it is off would be the small untruth this whole tab exists not to
    /// tell.
    #[test]
    fn game_output_on_with_another_sink_is_not_reported_as_off() {
        let body = block1(&cfg(true, false, Some("127.0.0.1:4242")));
        assert!(!body.contains("is off"), "{body}");
        assert!(
            body.contains("127.0.0.1:4242"),
            "it names where it is going: {body}"
        );

        let body = block1(&cfg(true, false, None));
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
        let text = block2(&BridgeState::NoPrefix, &missing, &[]);
        assert!(text.contains("/mnt/games2"), "{text}");
        assert!(text.contains("cannot rule out"), "{text}");
        assert_eq!(
            bridge_action(&BridgeState::NoPrefix),
            None,
            "there is nothing to install into"
        );
    }

    /// The moment somebody with a non-Steam game keeps typing is the moment
    /// to tell them the list cannot have it.
    ///
    /// The text explained the MATCH — title and app id, from Steam's manifests
    /// — and not the BOUNDARY, so a user with Star Citizen read an accurate
    /// sentence about how the search works and tried a different spelling. The
    /// way in exists and is one button below the list; this is where it is
    /// worth naming.
    #[test]
    fn a_search_that_finds_nothing_says_a_non_steam_game_never_will_be_found() {
        let apps = [app("1", "Something Else")];
        let text = picker(&catalog(&apps, &none), &[], "Star Citizen")
            .no_match
            .expect("nothing matched");
        assert!(
            text.contains("not in those manifests and never will be"),
            "the boundary, not just the match rule: {text}"
        );
        assert!(
            text.contains("Add it by hand"),
            "and the way in, which is a button on this very page: {text}"
        );
    }

    /// The other half. Without it the test above is satisfied by appending the
    /// sentence always, which would name a library on every machine.
    #[test]
    fn a_missing_prefix_with_every_library_present_invents_no_library() {
        let text = block2(&BridgeState::NoPrefix, &[], &[]);
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
        let text = block2(
            &BridgeState::Files {
                dir: prefix.join(BRIDGE_SUBDIR),
                prefix,
                present: BRIDGE_FILES.to_vec(),
            },
            &[],
            &[],
        );
        assert_eq!(bridge_action(&present()), Some(Action::Reinstall));
        assert_ne!(bridge_action(&present()), Some(Action::Install));
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
        let no_prefix = block2(&BridgeState::NoPrefix, &[], &[]);
        let absent = block2(
            &BridgeState::Absent {
                prefix: prefix.clone(),
                present: vec![],
            },
            &[],
            &[],
        );
        let files = block2(
            &BridgeState::Files {
                dir: prefix.join(BRIDGE_SUBDIR),
                prefix: prefix.clone(),
                present: BRIDGE_FILES.to_vec(),
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
        let argv = install_argv(
            Path::new("/usr/bin/tobii"),
            &Target::Steam("359320".to_string()),
            &InstallWith::default(),
        );
        assert!(
            !argv.iter().any(|a| a == "--force"),
            "--force overrides the refusal that stops the host's wine rewriting a game's \
             prefix; it is not a button's decision: {argv:?}"
        );
    }

    #[test]
    fn the_app_id_is_passed_and_the_name_is_not() {
        let argv = install_argv(
            Path::new("/usr/bin/tobii"),
            &Target::Steam("359320".to_string()),
            &InstallWith::default(),
        );
        assert_eq!(
            argv,
            ["/usr/bin/tobii", "bridge", "install", "--steam", "359320"]
                .map(OsString::from)
                .to_vec()
        );
    }

    #[test]
    fn wine_is_passed_only_when_it_was_chosen() {
        let bare = install_argv(
            Path::new("/usr/bin/tobii"),
            &Target::Steam("1".to_string()),
            &InstallWith::default(),
        );
        assert!(!bare.iter().any(|a| a == "--wine"), "{bare:?}");
        let chosen = install_argv(
            Path::new("/usr/bin/tobii"),
            &Target::Steam("1".to_string()),
            &InstallWith {
                wine: Some(PathBuf::from("/games/Proton 9.0/files/bin/wine")),
                ..InstallWith::default()
            },
        );
        let tail: Vec<&OsString> = chosen.iter().rev().take(2).collect();
        assert_eq!(tail[1], "--wine");
        assert_eq!(tail[0], "/games/Proton 9.0/files/bin/wine");
    }

    #[test]
    fn status_reads_the_same_game_the_install_would_have_written() {
        let s = status_argv(
            Path::new("/usr/bin/tobii"),
            &Target::Steam("359320".to_string()),
        );
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

    /// A prefix holding none of the three.
    fn absent() -> BridgeState {
        absent_with(vec![])
    }

    /// A prefix holding exactly `present` of them and not `BRIDGE_ARTIFACT` —
    /// the state `bridge_state` builds when the required file is missing,
    /// whatever else is there.
    fn absent_with(present: Vec<&'static str>) -> BridgeState {
        BridgeState::Absent {
            prefix: prefix(),
            present,
        }
    }

    /// A prefix holding all three files.
    fn present() -> BridgeState {
        files_with(BRIDGE_FILES.to_vec())
    }

    /// A prefix holding exactly `present` of them.
    fn files_with(present: Vec<&'static str>) -> BridgeState {
        BridgeState::Files {
            dir: prefix().join(BRIDGE_SUBDIR),
            prefix: prefix(),
            present,
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
        // Through `bridge_block`, because the claim is about the PAGE. Calling
        // `binary_line` and asserting on its return value tests that function
        // and nothing else: it stayed green with the line never appended to
        // any block.
        let here = bridge_block(
            &absent(),
            &[],
            &[],
            Some(Path::new("/home/x/.local/bin/tobii")),
            Some(Path::new("/opt/x")),
            &Target::Steam("359320".to_string()),
        );
        assert!(here.contains("/home/x/.local/bin/tobii"), "{here}");

        let there = bridge_block(
            &absent(),
            &[],
            &[],
            Some(Path::new("/usr/bin/tobii")),
            Some(Path::new("/opt/x")),
            &Target::Steam("359320".to_string()),
        );
        assert!(there.contains("/usr/bin/tobii"), "{there}");
        assert!(
            !there.contains("/home/x/.local/bin/tobii"),
            "the page names the one that was found, not a fixed path: {there}"
        );

        // And on every state that has a button, not only the one.
        for state in [absent(), present()] {
            let text = block2(&state, &[], &[]);
            assert!(bridge_action(&state).is_some(), "{state:?}");
            assert!(
                text.contains("/usr/bin/tobii"),
                "{state:?} carries a button and does not say what runs it: {text}"
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
        let s = tobii_binary(
            Some(Path::new("/opt/tobii/bin/tobii-gtk")),
            &exists,
            &on_path,
        );
        assert_eq!(s.found, Some(beside));
        assert_eq!(
            s.beside,
            Some(PathBuf::from("/opt/tobii/bin")),
            "the directory it stat'd, which the page is allowed to name"
        );
    }

    #[test]
    fn the_path_is_the_fallback_and_nothing_found_is_none_not_a_bare_name() {
        let exists = |_: &Path| false;
        let on_path = |_: &str| Some(PathBuf::from("/usr/bin/tobii"));
        assert_eq!(
            tobii_binary(Some(Path::new("/opt/x/tobii-gtk")), &exists, &on_path).found,
            Some(PathBuf::from("/usr/bin/tobii"))
        );

        let nothing = |_: &str| None;
        let found = tobii_binary(Some(Path::new("/opt/x/tobii-gtk")), &exists, &nothing).found;
        assert_eq!(found, None);
        assert_ne!(
            found,
            Some(PathBuf::from("tobii")),
            "a bare name would be a PATH lookup done by the child's environment at spawn \
             time, which is the thing this function exists to do beforehand"
        );

        // And the sentence that replaces the button says what to type.
        for a in [Action::Install, Action::Reinstall] {
            let text = no_binary_text(
                a,
                Some(Path::new("/opt/x")),
                &Target::Steam("359320".to_string()),
            );
            assert!(
                text.contains("tobii bridge install --steam 359320"),
                "{a:?}: {text}"
            );
        }
    }

    /// **`tobii_binary` does not always look beside this program, and the
    /// sentence printed when it finds nothing has to answer for that.**
    ///
    /// `std::env::current_exe` reads `/proc/self/exe` on Linux and can fail;
    /// `build_with` hands its `.ok()` straight through, so `exe` arrives as
    /// `None`. `tobii_binary` then has no directory to join `tobii` onto and
    /// goes straight to `$PATH` — while `no_binary_text`'s head opened "is
    /// not beside this one" whatever had happened, reporting a stat that was
    /// never performed as a finding. That is the same shape as the `$PATH`
    /// sentence the round before it, one clause to the left.
    ///
    /// What must break for this to fail: `tobii_binary` claiming a `beside`
    /// it did not stat, or `no_binary_text` going back to naming the
    /// beside-directory on the branch where there is none. Both halves are
    /// asserted, because the sentence is only honest if the flag under it is.
    #[test]
    fn with_no_current_exe_nothing_looked_beside_this_program_and_the_page_says_so() {
        // Half one: no exe, so no directory was stat'd. `exists` panics to
        // prove it was never called rather than merely answering `false`.
        let never = |p: &Path| panic!("nothing should have been stat'd, and {p:?} was");
        let on_path = |_: &str| None;
        let s = tobii_binary(None, &never, &on_path);
        assert_eq!(s.found, None);
        assert_eq!(s.beside, None, "there was nowhere to stand beside");

        // A path with no parent is the same case: `/` has none.
        assert_eq!(
            tobii_binary(Some(Path::new("/")), &never, &on_path).beside,
            None
        );

        // Half two: the sentence. It must not report on the place that was
        // not looked at, and it must say that it was not looked at.
        for a in [Action::Install, Action::Reinstall] {
            let text = no_binary_text(a, None, &Target::Steam("359320".to_string()));
            assert!(
                !text.contains("beside this one"),
                "{a:?} reported on a directory nothing stat'd: {text}"
            );
            assert!(
                text.contains("could not work out where its own file is"),
                "{a:?} has to say why it has no answer about that place: {text}"
            );
            assert!(
                text.contains("PATH this window was started with"),
                "the half it did look at is still named: {text}"
            );
        }

        // And with one, it names the directory it stat'd — not "this one",
        // which the user cannot check.
        let named = no_binary_text(
            Action::Install,
            Some(Path::new("/opt/x")),
            &Target::Steam("359320".to_string()),
        );
        assert!(named.contains("/opt/x"), "{named}");
        assert!(named.contains("beside this one"), "{named}");
        assert!(
            !named.contains("could not work out where"),
            "it did work it out: {named}"
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
        let text = profile_intro(&v, "A Game", "359320", Path::new("/cfg/profiles"));
        assert!(text.contains("/cfg/profiles/359320.toml"), "{text}");
        // Both numbers in the sentence that carries them, not loose anywhere
        // in the paragraph: `text.contains("2")` was satisfied by the 2 in
        // `359320.toml`, so a build that never told the user which version
        // their file claimed passed it.
        assert!(text.contains(&format!("profile format {newer}")), "{text}");
        assert!(
            text.contains(&format!("this build reads {}", profiles::VERSION)),
            "{text}"
        );
        assert!(text.contains("has not been used at all"), "{text}");
    }

    /// Block 2 warns that the prefix it names may be the abandoned one; block 3
    /// read the very same path and reported *"there is no directory at …,
    /// nothing has saved a control scheme here"* as settled fact, one block
    /// lower. One `chosen`, so one warning covering both.
    #[test]
    fn the_check_rows_say_which_prefix_they_read_when_there_is_more_than_one() {
        let here = PathBuf::from("/home/u/.steam/steam/steamapps/compatdata/359320/pfx");
        let there = PathBuf::from("/mnt/games2/steamapps/compatdata/359320/pfx");

        let note = checked_prefix_note(2, Some(&here), &[(there.clone(), false)])
            .expect("two prefixes and checks that read one of them");
        assert!(note.contains(&here.display().to_string()), "{note}");
        assert!(
            note.contains("wrong copy of its settings"),
            "including the rows reporting an absence: {note}"
        );
        assert!(note.contains("1 other Proton prefix"), "{note}");

        // One prefix is nothing to warn about, and appending this to every
        // page would invent a second prefix nobody has.
        assert_eq!(checked_prefix_note(2, Some(&here), &[]), None);
        // Nothing was read, so there is nothing to place.
        assert_eq!(
            checked_prefix_note(0, Some(&here), &[(there.clone(), false)]),
            None
        );
        // No prefix at all: the rows say that themselves.
        assert_eq!(checked_prefix_note(2, None, &[(there, false)]), None);
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
        let text = profile_intro(&v, "A Game", "359320", Path::new("/cfg/profiles"));
        assert!(text.contains("Permission denied"), "{text}");
        assert!(text.contains("not the same as there being none"), "{text}");

        let absent = profile_intro(
            &ProfileVerdict::None,
            "A Game",
            "359320",
            Path::new("/cfg/profiles"),
        );
        assert_ne!(text, absent, "the two negatives are two sentences");
    }

    /// Day one, and the intended behaviour rather than a gap.
    #[test]
    fn the_day_one_page_says_nobody_has_recorded_this_game() {
        // No `BUILTIN.is_empty()` assertion here any more. The paragraph asks
        // `shipped_profiles()` what this build ships rather than saying it, so
        // a profile shipping changes the sentence instead of falsifying it —
        // which is the whole reason that function exists.
        let text = profile_intro(
            &ProfileVerdict::None,
            "Elite Dangerous",
            "359320",
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
            text.contains(&format!("ships {}", profiles::shipped_profiles())),
            "it says what this build ships, from the one place that knows, \
             rather than reading as a fault: {text}"
        );
        // And it does not end at where profiles go. This is the state every
        // user of this build is in, and the paragraph named a directory and
        // stopped: the commands that fill it appeared nowhere a user could
        // see them, in this window or anywhere else.
        for cmd in [PROFILE_SAVE, PROFILE_CHECK_ADD, PROFILE_CHECK_WHERE] {
            assert!(
                text.contains(&format!("{cmd} 359320")),
                "`{cmd}` is not on the page, or not with this game's app id after it: {text}"
            );
        }
        assert!(
            text.contains(PROFILE_CHECK),
            "and the one that explains what a check is: {text}"
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

    /// The one fact that decides whether installing the bridge can deliver
    /// anything, on the page that offers to install it.
    ///
    /// Everything else block 2 says is about files, a directory and two
    /// registry values — all of which can be right while a TrackIR game
    /// receives nothing, because it checks a signature our DLL cannot answer.
    /// A terminal user is told that by `tobii bridge install`; before this
    /// test, somebody who installed from the window and got nothing had been
    /// told nothing at all.
    ///
    /// Asserted against [`tobii_config::signature::MEASURED`] rather than
    /// against sentences typed here: that is what makes it a test of the two
    /// surfaces sharing one source, and it fails if the note is ever forked
    /// back into a local copy that drifts.
    #[test]
    fn a_game_that_needs_the_bridge_is_told_about_the_signature_check() {
        let note =
            profile_bridge_note(profiles::Bridge::Required).expect("Required says something");
        assert!(
            note.contains("signature"),
            "the gate itself has to be named: {note}"
        );
        for m in signature::MEASURED {
            assert!(
                note.contains(m.title) && note.contains(m.date),
                "{} was measured against that check and the window does not say so: {note}",
                m.title,
            );
        }
        assert!(
            // The property, not the sentence: the count in it comes from
            // `signature::MEASURED`, so a literal here is a literal that goes
            // stale on the one edit the seam computes its way around.
            note.contains("not a rule about the rest"),
            "and it must not read as a rule about the user's own game: {note}"
        );
        assert!(
            note.contains("FreeTrack"),
            "the ungated route is the one that works today: {note}"
        );
        assert_eq!(
            note.matches("NaturalPoint").count(),
            1,
            "one copy of this, asked of tobii-config, never two: {note}"
        );
    }

    /// The window names the second wall, and names `tobii bridge run` only to
    /// say it is not the way over it.
    ///
    /// A third-party client answers the signature check and then *reads* the
    /// mapping; something inside the game's own Wine session has to be filling
    /// it. Until this commit the page said that thing was `tobii bridge run`,
    /// and `bridge/core/src/feeder.rs` records why that is wrong for nearly
    /// every reader: a Steam game under Proton has its own wineserver, and a
    /// provider started from a terminal with system Wine is a different session
    /// whose `FT_SharedMem` is a different object. The old sentence sent
    /// somebody to run a command that could not reach their game, and then to
    /// conclude the bridge was broken when it did not.
    ///
    /// So the note has to carry both halves: that a client only reads, and that
    /// the obvious way to feed it does not reach a Proton game. A window that
    /// said "installed" and stopped there would leave a user with an answered
    /// check, an empty mapping, and no reason to suspect either.
    #[test]
    fn the_window_says_a_separate_provider_cannot_reach_a_proton_game() {
        let note =
            profile_bridge_note(profiles::Bridge::Required).expect("Required says something");
        assert!(
            note.contains("has to be filling it"),
            "the wall a passing signature check does not clear has to be named: {note}"
        );
        assert!(
            note.contains("tobii bridge run"),
            "and the command a reader would otherwise reach for: {note}"
        );
        assert!(
            note.contains("that cannot be"),
            "named as what will NOT do it \u{2014} which is the whole correction: {note}"
        );
        assert!(
            note.contains("a different object") && note.contains("never sees it"),
            "with the reason, so it reads as a fact and not as a refusal: {note}"
        );
        assert!(
            !note.contains("started again once the game is up"),
            "and the instruction that was wrong is gone, not merely qualified: {note}"
        );
        // The other two arms are about a game that wants no bridge, or a
        // profile that never said — neither is a reason to explain `run`.
        assert!(
            !profile_bridge_note(profiles::Bridge::NotNeeded)
                .expect("NotNeeded says something")
                .contains("tobii bridge run"),
            "a game that does not need the bridge is not told how to feed it"
        );
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
    /// The note for a directory nothing selects a preset in — the shape the
    /// "thirty presets is not an absence" fix exists for, which had no test.
    ///
    /// Both counts, because the tail has no singular that fits the plural's
    /// slot: one preset "is not in use", several give "none of them is".
    #[test]
    fn a_directory_that_selects_no_preset_says_so_at_either_count() {
        let shipped = |n: usize| binds::Bindings {
            start: None,
            schema: None,
            superseded: Vec::new(),
            presets: (0..n)
                .map(|i| binds::Active {
                    name: format!("Preset{i}"),
                    found: binds::Found::Absent(String::new()),
                })
                .collect(),
        };

        let one = binds_note(&shipped(1)).expect("a note");
        assert!(
            one.contains("the row below is the preset the game ships and it is not in use"),
            "one preset reads as one: {one}"
        );

        let many = binds_note(&shipped(30)).expect("a note");
        assert!(
            many.contains(
                "the rows below are the presets the game ships and none of them is in use"
            ),
            "thirty read as thirty: {many}"
        );

        // A directory that DOES select one says none of this.
        let selected = binds_note(&binds::Bindings {
            start: Some(PathBuf::from("/p/StartPreset.start")),
            schema: None,
            superseded: Vec::new(),
            presets: Vec::new(),
        });
        assert!(
            !selected
                .unwrap_or_default()
                .contains("nothing in this directory selects a preset"),
            "a selected preset is not a shipped set"
        );
    }

    #[test]
    fn a_superseded_start_file_is_named_and_an_ordinary_one_says_nothing_extra() {
        let note = binds_note(&binds::Bindings {
            start: Some(PathBuf::from("/p/StartPreset.4.start")),
            schema: Some(4),
            superseded: vec![PathBuf::from("/p/StartPreset.3.start")],
            presets: Vec::new(),
        })
        .expect("a superseded start file is worth saying out loud");
        assert!(note.contains("StartPreset.3.start"), "{note}");
        assert!(note.contains("rolled back"), "{note}");

        let quiet = binds_note(&binds::Bindings {
            start: Some(PathBuf::from("/p/StartPreset.start")),
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
        let quiet = picker(&catalog(&apps, &none), &[], "");
        assert!(
            quiet.census.starts_with("2 titles"),
            "it counts every installed title, not the filtered rows: {}",
            quiet.census
        );
        assert!(!quiet.census_warn, "{}", quiet.census);

        // Filtered down to one row, and still a census of two.
        let filtered = picker(&catalog(&apps, &none), &[], "One");
        assert_eq!(filtered.rows.len(), 1);
        assert_eq!(
            filtered.census, quiet.census,
            "the census is of the machine, not of the search"
        );

        let one = picker(&catalog(&apps[..1], &none), &[], "");
        assert!(
            one.census.starts_with("1 title"),
            "singular: {}",
            one.census
        );

        let warned = picker(&catalog(&apps, &none), &[PathBuf::from("/mnt/games2")], "");
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

    /// What the detail pane says on an empty machine. The list beside it has no
    /// rows, so every sentence here has to carry itself.
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
    /// written to prevent". This tab reports the same fact from the same place.
    #[test]
    fn a_joystick_that_could_not_be_created_is_not_a_destination_here_either() {
        let failed = JoystickStatus::Failed("/dev/uinput: Permission denied".to_string());
        let c = cfg(true, true, None);

        let body = settings_block(&c, profiles::Bridge::Unstated, &failed);
        assert!(
            !body.contains("sending to a virtual joystick"),
            "the card on the Tracker tab says it could not be created: {body}"
        );
        assert!(
            !body.contains("Nothing to change here"),
            "something is very much wrong here: {body}"
        );
        assert!(body.contains("/dev/uinput: Permission denied"), "{body}");

        // The same configuration with a joystick the device thread made.
        let ok = settings_block(&c, profiles::Bridge::Unstated, &JoystickStatus::Present);
        assert!(ok.contains("sending to a virtual joystick"), "{ok}");
        assert!(ok.contains("Nothing to change here"), "{ok}");

        // And `Off` is not a failure: it means nobody has asked yet, which for
        // a page about what is *configured* is not a fault.
        let off = settings_block(&c, profiles::Bridge::Unstated, &JoystickStatus::Off);
        assert_eq!(off, ok, "not asked for yet is not could not be created");
    }

    // ------------------------------- what the switch on the other tab starts

    /// The doctrine that outlived the button: whoever turns game output on has
    /// to be told what that starts, and it is not one destination.
    ///
    /// The button used to carry this in its caption, built from
    /// [`destinations`] over the config the press would leave behind. The
    /// switch that does the same thing now is on the other tab and cannot carry
    /// a per-game caption at all — so the body has to name them, and it has to
    /// name **every** one, which is why the list is asked of `destinations`
    /// rather than typed out here.
    #[test]
    fn the_off_state_names_every_destination_the_switch_will_start() {
        let mut c = cfg(false, false, Some("127.0.0.1:4242"));
        c.bridge_port = Some(4243);
        let body = block1(&c);

        // What turning it on leaves behind, worded by the same function the
        // body is worded by.
        let mut after = c.clone();
        after.enabled = true;
        after.joystick = true;
        let (sinks, _) = destinations(&after, &JoystickStatus::Off);
        assert!(
            sinks.len() > 1,
            "this fixture exists to have more than one destination: {sinks:?}"
        );
        for sink in &sinks {
            // The joystick is the one the press adds rather than one the file
            // already names, and block 1 off-state speaks about the file. It is
            // named in the joystick paragraph below instead, which every
            // non-final state carries.
            if sink == "a virtual joystick" {
                continue;
            }
            assert!(
                body.contains(sink.as_str()),
                "turning it on starts {sink} and this page does not say so: {body}"
            );
        }
        assert!(
            body.contains("virtual joystick"),
            "and the destination that needs nothing else installed: {body}"
        );
    }

    /// The page cannot claim ignorance in block 1 that block 2 contradicts
    /// three paragraphs later from the same profile.
    #[test]
    fn a_profile_that_answers_the_bridge_question_is_not_also_reported_as_unanswered() {
        let c = cfg(false, false, None);
        let unstated = settings_block(&c, profiles::Bridge::Unstated, &JoystickStatus::Off);
        assert!(
            unstated.contains("not something this program knows"),
            "with nobody having said, this is the honest sentence: {unstated}"
        );

        let required = settings_block(&c, profiles::Bridge::Required, &JoystickStatus::Off);
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

        let not_needed = settings_block(&c, profiles::Bridge::NotNeeded, &JoystickStatus::Off);
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

        let alone = block2(
            &BridgeState::Absent {
                prefix: here.clone(),
                present: vec![],
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

        let two = block2(
            &BridgeState::Absent {
                prefix: here,
                present: vec![],
            },
            &[],
            &[(there.clone(), false)],
        );
        assert!(two.contains(&there.display().to_string()), "{two}");
        assert!(two.contains("Move Install Folder"), "{two}");
        assert_eq!(
            bridge_action(&absent()),
            Some(Action::Install),
            "the button stays — the named prefix is still the one an install writes"
        );

        // And when the OTHER one already holds the bridge, that is the fact
        // most worth knowing before pressing anything.
        let installed_there = block2(
            &BridgeState::Absent {
                prefix: PathBuf::from("/home/u/.steam/steam/steamapps/compatdata/359320/pfx"),
                present: vec![],
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
        let text = block2(
            &BridgeState::Absent {
                prefix: PathBuf::from("/p/pfx"),
                present: vec![],
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
        // The count in the promise and the file in the check are two numbers,
        // and the paragraph that gives the first has to give the second: this
        // window counted three when it promised and one when it looked.
        assert!(
            text.contains(&format!("there is no {BRIDGE_ARTIFACT} under")),
            "the stat this state rests on is one file, and the paragraph says which: {text}"
        );
        assert!(
            text.contains(&format!("Only {BRIDGE_ARTIFACT} is required")),
            "and which of the three an install has to produce: {text}"
        );
    }

    /// `bridge.rs` marks one of the three artifacts required and prints
    /// `note: <name> not built yet — skipping` for either of the others. Over
    /// such a prefix this window said "The bridge's files are in this prefix"
    /// and "The bridge is installed." directly above the installer's own
    /// account of what it had skipped — a plural over one file, and an absence
    /// reported as a presence.
    ///
    /// It is named for a **state**, not a prefix, because that is all it
    /// touches: it hands `BridgeState::Files` a `present` vector, so it is the
    /// wording it breaks, never the stat. Deleting `present_files` leaves this
    /// green, and it is meant to —
    /// `present_files_lists_what_is_there_and_nothing_else` is the one that
    /// stats a real directory, and it is the one that goes red. The old name
    /// said "a prefix holding one of the three", which promised the stat this
    /// test does not do.
    #[test]
    fn a_state_holding_one_of_the_three_is_not_worded_as_holding_all_three() {
        let one = files_with(vec![BRIDGE_ARTIFACT]);
        let text = block2(&one, &[], &[]);
        assert_eq!(bridge_action(&one), Some(Action::Reinstall));
        assert!(
            !text.contains("The bridge's files are in this prefix"),
            "one file is not \"the bridge's files\": {text}"
        );
        assert!(
            text.contains(&format!("1 of the bridge's {} files", BRIDGE_FILES.len())),
            "{text}"
        );
        for gone in absent_files(&[BRIDGE_ARTIFACT]) {
            assert!(
                text.contains(gone),
                "{gone} is not in the prefix and the page does not say so: {text}"
            );
        }
        assert!(
            text.contains("find it is a question"),
            "and the singular carries through the paragraph: {text}"
        );

        assert!(text.contains("The one a game has to load"), "{text}");

        // The full set reads as it always did — the qualification is earned by
        // a gap, not appended to every page.
        let all = block2(&present(), &[], &[]);
        assert!(
            all.contains(&format!("All {} of the bridge's files", BRIDGE_FILES.len())),
            "{all}"
        );
        assert!(
            !all.contains("The one a game has to load"),
            "nothing is missing, so there is nothing to explain away: {all}"
        );
        assert!(all.contains("find them is a question"), "{all}");

        // And the success sentence agrees with the same set, because it is the
        // one printed over `note: … skipping`.
        let said = outcome_text(&install_outcome(Some(0), "copied 1 file(s)\n", ""), &one);
        assert!(said.contains("The bridge is installed."), "{said}");
        for gone in absent_files(&[BRIDGE_ARTIFACT]) {
            assert!(
                said.contains(gone),
                "{gone} was skipped and the report does not say so: {said}"
            );
        }
        let whole = outcome_text(
            &install_outcome(Some(0), "copied 3 file(s)\n", ""),
            &present(),
        );
        assert!(
            !whole.contains("of the bridge's"),
            "all three arrived; the plain sentence is the whole truth: {whole}"
        );
    }

    /// The mirror, which was left unfixed. `present_files`' own doc comment
    /// states the rule for `Files` — "the bridge's files are in this prefix"
    /// over one file is an absence reported as a presence — and `Absent`
    /// carried no `present` at all, so it said flatly "The bridge's files are
    /// not in it" over a directory that may hold two of the three. A presence
    /// reported as an absence, out of a stat that had already seen them.
    #[test]
    fn a_prefix_missing_only_the_required_file_is_not_told_it_holds_none() {
        let some = absent_files(&[BRIDGE_ARTIFACT]);
        assert_eq!(
            some.len(),
            BRIDGE_FILES.len() - 1,
            "the fixture is the other two, whatever they are called"
        );

        let text = block2(&absent_with(some.clone()), &[], &[]);
        assert_eq!(
            bridge_action(&absent_with(some.clone())),
            Some(Action::Install),
            "the file a game loads is still missing, so the button stays: {text}"
        );
        assert!(
            !text.contains("The bridge's files are not in it"),
            "{n} of them are, and a stat had already seen them: {text}",
            n = some.len()
        );
        for f in &some {
            assert!(
                text.contains(f),
                "{f} is in the prefix and the page does not name it: {text}"
            );
        }
        assert!(
            text.contains(&format!(
                "{} of the bridge's {} files",
                some.len(),
                BRIDGE_FILES.len()
            )),
            "{text}"
        );
        let decides = format!("{BRIDGE_ARTIFACT}, the one a game has to load, is not");
        assert!(
            text.contains(&decides),
            "and which one of them decides that nothing loads: {text}"
        );

        // An empty prefix reads as it always did: the qualification is earned
        // by what the stat found, not appended to every page.
        let none = block2(&absent(), &[], &[]);
        assert!(none.contains("The bridge's files are not in it"), "{none}");
        assert!(
            none.contains(&format!("there is no {BRIDGE_ARTIFACT} under")),
            "{none}"
        );
        assert_ne!(none, text, "two stats, two sentences");

        // And the paragraph printed over an install that reported success and
        // produced nothing loadable agrees with the same stat — which is why
        // `outcome_text` takes the state at all.
        let said = outcome_text(
            &install_outcome(Some(0), "copied 2 file(s)\n", ""),
            &absent_with(some),
        );
        assert!(said.contains("will not call that an install"), "{said}");
        assert!(
            !said.contains("The bridge's files are not in it"),
            "the same two files are on disk here too: {said}"
        );
    }

    /// The gate is printed once, whatever the profile says.
    ///
    /// `measured_note` asked whether the profile note had already printed it by
    /// searching that note for the word "signature". It worked only while one
    /// sentence of the gate happened to contain the word — reword it and every
    /// profiled, measured game prints the whole gate twice with no test failing
    /// — and it answered NO for a profile saying the bridge is NOT needed, so
    /// such a game read "this game does not need the bridge" followed by the
    /// full gate. One predicate, two readers.
    #[test]
    fn a_measured_game_with_a_profile_is_not_told_the_gate_twice() {
        let msfs = signature::measured_steam("2537590").expect("measured");
        for (bridge, says) in [
            (profiles::Bridge::Required, true),
            (profiles::Bridge::NotNeeded, false),
            (profiles::Bridge::Unstated, false),
        ] {
            assert_eq!(bridge_says_the_gate(bridge), says, "{bridge:?}");
            // And the predicate matches what the note actually does.
            let note = profile_bridge_note(bridge).unwrap_or_default();
            assert_eq!(
                note.contains(&signature::trackir_gate()),
                says,
                "{bridge:?}: the predicate and the paragraph have to agree: {note}"
            );
            // Which is what keeps the page from printing it twice.
            let whole = format!(
                "{note}{}",
                measured_note(Some(msfs), bridge_says_the_gate(bridge)).unwrap_or_default()
            );
            assert_eq!(
                whole.matches(&signature::trackir_gate()).count(),
                1,
                "{bridge:?}: the gate is on this page exactly once: {whole}"
            );
        }
    }

    /// The registry line reports the VALUE and judges nothing.
    ///
    /// `tobii bridge status` decides whose a registration is against a record
    /// file it wrote inside the prefix. A second, looser opinion here — "the
    /// path looks like ours" — would be two surfaces reaching confident
    /// verdicts on one question by different rules, which is the drift
    /// `signature.rs` was created to stop. So this states what is there and
    /// names the button that judges it.
    #[test]
    fn the_registry_line_says_what_is_registered_and_not_whose_it_is() {
        let dir = scratch("userreg");
        std::fs::create_dir_all(&dir).expect("fixture");
        // Raw strings, and the backslashes are DOUBLED because that is how
        // wine writes them into the file — a fixture with single ones parses
        // as no such section and the line reads "nothing is registered",
        // which is the confident negative this whole page is written against.
        std::fs::write(
            dir.join(tobii_config::userreg::FILE),
            concat!(
                "WINE REGISTRY Version 2\n\n",
                r"[Software\\Freetrack\\FreeTrackClient] 1790444400",
                "\n",
                r#""Path"="C:\\tobii-bridge""#,
                "\n\n",
                r"[Software\\NaturalPoint\\NATURALPOINT\\NPClient Location] 1790444400",
                "\n",
                r#""Path"="Z:\\usr\\libexec\\opentrack""#,
                "\n",
            ),
        )
        .expect("fixture");

        let note = registered_clients(Some(&dir)).expect("a prefix with a user.reg");
        assert!(note.contains("FreeTrack is registered to"), "{note}");
        assert!(note.contains("TrackIR is registered to"), "{note}");
        assert!(
            note.contains("opentrack"),
            "the value is the whole point — this is the line that answers \"I installed it \
             and nothing happened\": {note}"
        );
        for verdict in [
            "registered by this installer",
            "not this installer's",
            "ours",
        ] {
            assert!(
                !note.contains(verdict),
                "{verdict:?} is `tobii bridge status`'s judgement, made against a record \
                 file this does not read: {note}"
            );
        }

        // Three different nothings, all of which mean "not from here".
        assert_eq!(registered_clients(None), None, "no prefix");
        assert_eq!(
            registered_clients(Some(&dir.join("nope"))),
            None,
            "a prefix with no user.reg in it"
        );
    }

    /// Block 2 no longer says it cannot read the registry, because it can.
    ///
    /// That sentence, and the one pointing at the button that could, were the
    /// shape of the whole problem: the hub shelled out for a plain-text file
    /// because the reader lived in a binary it cannot link. `userreg` moved to
    /// `tobii-config` and `registered_clients` reads it here, so the paragraph
    /// states the question and the answer lands under it — and neither half
    /// depends any more on whether there is a `tobii` to run.
    #[test]
    fn block_two_names_the_registry_question_whether_or_not_a_tobii_can_answer_it() {
        for (what, tobii) in [
            ("with a tobii", Some(Path::new("/usr/bin/tobii"))),
            ("without one", None),
        ] {
            let text = bridge_block(
                &present(),
                &[],
                &[],
                tobii,
                Some(Path::new("/opt/x")),
                &Target::Steam("359320".to_string()),
            );
            assert!(
                text.contains("two registry values inside the prefix"),
                "{what}: the question this block is about: {text}"
            );
            assert!(
                !text.contains("this window does not read"),
                "{what}: it reads them now, and a page saying otherwise sends the reader \
                 looking for a button it does not need: {text}"
            );
        }
    }

    #[test]
    fn what_this_window_says_about_a_tobii_it_could_not_find_is_about_its_own_path() {
        for text in [
            no_binary_text(
                Action::Install,
                Some(Path::new("/opt/x")),
                &Target::Steam("359320".to_string()),
            ),
            no_binary_text(
                Action::Reinstall,
                Some(Path::new("/opt/x")),
                &Target::Steam("359320".to_string()),
            ),
            lead(None).to_string(),
        ] {
            assert!(
                !text.contains("not on your PATH"),
                "the only PATH this window read is its own: {text}"
            );
            assert!(
                text.contains("PATH this window was started with"),
                "and it has to say whose, because it goes on to send the user to a terminal \
                 with a different one: {text}"
            );
        }

        // The two that print a command say where it would be, and what it
        // means when the terminal cannot find it either — which no arm said.
        for action in [Action::Install, Action::Reinstall] {
            let text = no_binary_text(
                action,
                Some(Path::new("/opt/x")),
                &Target::Steam("359320".to_string()),
            );
            assert!(
                text.contains("~/.local/bin"),
                "where this project's installer puts it: {text}"
            );
            assert!(
                text.contains("install the release again"),
                "and the one remedy that is not a PATH line: {text}"
            );
            assert!(
                text.contains(&format!("tobii bridge install --steam {}", "359320")),
                "the command is still there: {text}"
            );
        }

        // And block 3 is the page every user sees, since no profile ships: it
        // sends them to the same terminal, so the two must not disagree about
        // what is known about it.
        let write = write_a_profile_commands("359320");
        assert!(write.contains("in a terminal"), "{write}");
        assert!(
            !write.contains("not on your PATH"),
            "block 3 does not get to claim it either: {write}"
        );
    }

    /// The stat behind all of that, against a directory on disk. Three stats,
    /// not one: `BRIDGE_ARTIFACT` alone answers whether a game can load
    /// anything, and it is not the same question as what is in there.
    #[test]
    fn present_files_lists_what_is_there_and_nothing_else() {
        let dir = scratch("present-files").join(BRIDGE_SUBDIR);
        std::fs::create_dir_all(&dir).expect("fixture");
        assert!(present_files(&dir).is_empty());

        std::fs::write(dir.join(BRIDGE_ARTIFACT), b"x").expect("fixture");
        assert_eq!(present_files(&dir), vec![BRIDGE_ARTIFACT]);
        assert_eq!(
            absent_files(&present_files(&dir)).len(),
            BRIDGE_FILES.len() - 1
        );

        // A directory, not a file, is not a file.
        std::fs::create_dir(dir.join("NPClient64.dll")).expect("fixture");
        assert_eq!(present_files(&dir), vec![BRIDGE_ARTIFACT]);

        for f in BRIDGE_FILES {
            let p = dir.join(f);
            if !p.is_file() {
                let _ = std::fs::remove_dir_all(&p);
                std::fs::write(&p, b"x").expect("fixture");
            }
        }
        assert_eq!(present_files(&dir), BRIDGE_FILES.to_vec());
        assert!(absent_files(&present_files(&dir)).is_empty());

        let _ = std::fs::remove_dir_all(dir.parent().expect("drive_c"));
    }

    /// **An empty stat is not evidence that nothing of those names is there.**
    ///
    /// `present_files` is three `is_file()` calls, and `is_file()` is false
    /// for a directory of that name and false for a symlink pointing at
    /// nothing — both of which are on disk, and both of which the user can
    /// see in a file manager. `absent_sentence`'s empty branch read "and none
    /// of the other 2 is there either", asserted straight out of the empty
    /// vector, so over this prefix the window denied the existence of two
    /// entries it had just stat'd. `present_files`' own doc comment names
    /// this shape — "reporting an absence as a presence" — as the bug the
    /// whole window is written against; this is it in the mirror.
    ///
    /// What must break for this to fail: the sentence going back to claiming
    /// absence from `is_file()`, or dropping the clause that says what the
    /// three stats were.
    #[test]
    fn the_empty_prefix_sentence_does_not_deny_a_name_that_is_there_but_is_not_a_file() {
        let root = scratch("not-a-file");
        let dir = root.join(BRIDGE_SUBDIR);
        std::fs::create_dir_all(&dir).expect("fixture");
        // Two of the three names, on disk, neither of them a regular file.
        std::fs::create_dir(dir.join("NPClient64.dll")).expect("fixture");
        std::os::unix::fs::symlink("nowhere", dir.join("tobii-bridge.exe")).expect("fixture");
        assert!(
            dir.join("NPClient64.dll").exists(),
            "the fixture has to be a directory that is really there"
        );
        assert!(
            dir.join("tobii-bridge.exe").symlink_metadata().is_ok(),
            "and a link that is really there while its target is not"
        );

        let present = present_files(&dir);
        assert!(present.is_empty(), "neither is a regular file: {present:?}");

        let said = absent_sentence(&dir, &present);
        assert!(
            !said.contains("is there either") && !said.contains("are there either"),
            "two of those names are in the directory this sentence just named: {said}"
        );
        assert!(
            said.contains(&format!(
                "no file of the other {} names either",
                BRIDGE_FILES.len() - 1
            )),
            "what the stat did establish is still said: {said}"
        );
        assert!(
            said.contains("counted here as not present"),
            "and the gap between the two, because the user can see these entries: {said}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The sentence that replaces the button names the job the button did.
    /// Written for `Install` and printed over a prefix that already holds the
    /// files, it told the user the bridge "cannot be installed from here"
    /// under a paragraph saying the files are in this prefix, where the action
    /// it replaced was *Reinstall*.
    #[test]
    fn the_sentence_replacing_the_button_names_what_the_button_did() {
        let files = bridge_block(
            &present(),
            &[],
            &[],
            None,
            Some(Path::new("/opt/x")),
            &Target::Steam("359320".to_string()),
        );
        assert!(
            actions(
                &present(),
                None,
                &quiet_job(),
                Reach::Reachable,
                false,
                Group::NotSetUp
            )
            .primary
            .is_none(),
            "nothing to run: {files}"
        );
        assert!(
            !files.contains("cannot be installed from here"),
            "the files are in the prefix and the button said Reinstall: {files}"
        );
        assert!(files.contains("reinstall"), "{files}");
        assert!(
            files.contains("tobii bridge status --steam 359320"),
            "Details is gone for the same reason, so the page says what it ran: {files}"
        );

        let absent = bridge_block(
            &absent(),
            &[],
            &[],
            None,
            Some(Path::new("/opt/x")),
            &Target::Steam("359320".to_string()),
        );
        assert!(absent.contains("cannot be installed from here"), "{absent}");
        assert_ne!(
            no_binary_text(
                Action::Install,
                Some(Path::new("/opt/x")),
                &Target::Steam("359320".to_string())
            ),
            no_binary_text(
                Action::Reinstall,
                Some(Path::new("/opt/x")),
                &Target::Steam("359320".to_string())
            ),
            "two jobs, two sentences"
        );

        // No button at all, and so no sentence about one: a prefix that does
        // not exist is not a machine missing a program.
        let none = bridge_block(
            &BridgeState::NoPrefix,
            &[],
            &[],
            None,
            Some(Path::new("/opt/x")),
            &Target::Steam("359320".to_string()),
        );
        assert!(actions(
            &BridgeState::NoPrefix,
            None,
            &quiet_job(),
            Reach::Reachable,
            false,
            Group::NotSetUp,
        )
        .primary
        .is_none());
        // Asserted on a phrase both arms of `no_binary_text`'s head share.
        // The old assertion was `!none.contains("not beside this one")`,
        // which stopped being a substring of either arm the moment the head
        // learned to name the directory — a test that could no longer fail
        // for the reason it was written for.
        assert!(
            !none.contains("PATH this window was started with"),
            "there was no button here to replace, so no paragraph saying why it is gone: \
             {none}"
        );
    }

    /// The lead's count is a promise about buttons, and it has been wrong twice
    /// for the same reason: it was written once and not asked again.
    ///
    /// First it promised "the two it can" on a machine where block 2 has no
    /// button at all, and did not ask whether there was a `tobii` to run.
    /// Then block 1's button was deleted with the modal and the two became one
    /// — a lead still offering a settings button that does not exist anywhere
    /// on the page.
    ///
    /// So both halves are asserted: the count is **one**, and it is never two.
    #[test]
    fn the_lead_promises_exactly_the_buttons_this_machine_has() {
        let with = lead(Some(Path::new("/usr/bin/tobii")));
        assert!(
            with.contains("do the one it can"),
            "block 1 reports and never writes, so the bridge is the only thing this page \
             does: {with}"
        );
        assert!(
            !with.contains("do the two it can"),
            "the second of the two was block 1's button, and it is gone: {with}"
        );

        let without = lead(None);
        assert!(
            !without.contains("do the one it can"),
            "with no `tobii` the bridge block has no button either, so the page does \
             none of the three: {without}"
        );
        assert!(
            without.contains("three things"),
            "it still shows all three: {without}"
        );
        assert!(
            without.contains("`tobii`"),
            "and says which program is missing: {without}"
        );
        // And the tense, which is the half a count cannot carry. With no
        // `tobii` the page does nothing at all, and the sentence still read
        // "The one it can do is install the Wine bridge" — an offer, in the
        // present tense, above a block whose button is not built. A reader
        // looking for it found three paragraphs and no control.
        assert!(
            without.contains("do none of them"),
            "with no `tobii` the page does none of the three, and has to say so \
             before naming the one it would otherwise do: {without}"
        );
        assert!(
            !without.contains("The one it can do is"),
            "that is the offer, in the present tense, and there is no button under \
             it: {without}"
        );
    }

    /// Block 1 reports five settings, and only three of them have a control on
    /// the other tab.
    ///
    /// The sentence sent all five there — "These are the Tracker tab's own
    /// settings" — and two of them are not on it and never were: an opentrack
    /// address and a bridge port live in `games.toml` and are written by
    /// `tobii games set`. A reader who went looking for the address on the
    /// Tracker tab would find nothing, and the page that sent them is the one
    /// whose whole argument is that it does not tell small untruths.
    ///
    /// The lists are hand-written here, and that is the limit of what this
    /// catches: it notices the sentence being reworded back into a blanket
    /// claim, and it would not notice a sixth setting appearing in block 1
    /// with no route at all. What it pins is the split.
    #[test]
    fn the_pointer_sends_each_setting_to_the_place_that_can_change_it() {
        for on_the_tab in ["The switch", "the strength", "the virtual joystick"] {
            assert!(
                TRACKER_TAB_POINTER.contains(on_the_tab),
                "{on_the_tab} has a control on the Tracker tab and the pointer does \
                 not send anyone to it: {TRACKER_TAB_POINTER}"
            );
        }
        for elsewhere in ["opentrack", "bridge port"] {
            assert!(
                TRACKER_TAB_POINTER.contains(elsewhere),
                "{elsewhere} is reported by block 1 and the pointer says nothing \
                 about where it is changed: {TRACKER_TAB_POINTER}"
            );
        }
        assert!(
            TRACKER_TAB_POINTER.contains("games.toml")
                && TRACKER_TAB_POINTER.contains("tobii games set"),
            "and where those two really are, which is the half the Tracker tab \
             cannot answer: {TRACKER_TAB_POINTER}"
        );
        assert!(
            !TRACKER_TAB_POINTER.contains("the Tracker tab's own settings"),
            "that is the blanket claim: it puts all five on a tab that has three: \
             {TRACKER_TAB_POINTER}"
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

    /// The other half of the rule, and the half a remedy appended to *every*
    /// non-match broke: a row that says the check could not run must not then
    /// tell somebody to go and change the setting it did not look at. "This
    /// game has no Proton prefix here yet" followed by "In Controls, set Head
    /// Look to Toggle" sends a user into a directory that does not exist.
    #[test]
    fn a_check_that_could_not_run_is_not_given_a_remedy() {
        for a in [
            Answer::NoPrefix,
            Answer::UnknownFormat("binds-v2".to_string()),
        ] {
            let text = row_text(&row(a.clone()));
            assert!(text.contains("not checked"), "{a:?}: {text}");
            assert!(
                !text.contains("In Controls, set Head Look to Toggle."),
                "{a:?} sends the reader to a file this row just said was not read: {text}"
            );
            assert!(
                !text.contains("expects 1"),
                "{a:?} states an expectation about a value nothing here read: {text}"
            );
            // And the sentence ends where it ends: no orphaned space left by
            // dropping a clause off the end of it.
            assert_eq!(text.trim_end(), text, "{a:?}: {text:?}");
        }
    }

    /// The verb agrees with the count as well as the noun.
    #[test]
    fn one_superseded_start_file_is_one_file_that_was_not_read() {
        let one = binds_note(&binds::Bindings {
            start: Some(PathBuf::from("/p/StartPreset.4.start")),
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
            start: Some(PathBuf::from("/p/StartPreset.4.start")),
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
    /// path it names is joined onto a Proton prefix and opened. Three shapes
    /// of path, and this window owns what it says about each: a path that
    /// could climb out takes the whole file with it; a path the parser
    /// accepts is joined onto the prefix rather than onto whatever sits
    /// beside it; and a path through a `dosdevices` drive letter reads a file
    /// outside the prefix, which is not a bug —
    /// `a_check_read_through_a_drive_letter_lands_outside_the_prefix` below
    /// is that third one.
    ///
    /// # This test used to be named for a property the code has not got
    ///
    /// It was `this_window_opens_nothing_outside_the_prefix_it_was_given`,
    /// and every assertion in it was lexical: `path_under(&prefix)
    /// .starts_with(&prefix)` over three relative paths, plus a read that
    /// landed on the copy under the prefix. All of that is true and none of
    /// it is containment — a `dosdevices/z:` path passes both assertions and
    /// still resolves to `/`. A test whose name claims a property its body
    /// cannot see reads as evidence for the claim, which is worse than having
    /// no test, so the name now says what the body actually checks and the
    /// third test below states the property the old name denied.
    ///
    /// # What half one leans on, and who owns it
    ///
    /// The refusals are `check_path`'s, in
    /// `crates/tobii-config/src/profiles.rs` — not this crate's. This test
    /// pins its three answers because this window's own sentence is built on
    /// them, so if that guard's answer to any of these three changes, this
    /// test is the surface that has to change with it. It is a deliberate
    /// cross-crate pin, not an accident.
    ///
    /// The backslash case did not reach `check_path` at all until the literal
    /// was written with the escape TOML needs: `"drive_c\\users"` in Rust
    /// source is one `\` in the file, and one `\` before `u` is a `\u`
    /// escape, so the *string parser* rejected it for "`\u` takes four hex
    /// digits" several hundred lines earlier. Deleting `check_path`'s
    /// backslash guard left this test green. Hence the assertion below that
    /// the refusal is not that one.
    ///
    /// What must break for this to fail: `check_path` accepting any of the
    /// three shapes below, or this window reporting a refused profile as
    /// anything other than a file none of which was used.
    #[test]
    fn a_path_that_could_climb_out_takes_the_whole_profile_with_it() {
        // Nothing here touches the filesystem: the three shapes below never
        // get as far as a prefix, and a test that made one would be implying
        // they might.
        let profile = |path: &str| {
            format!(
                "version = 1\n\n[[check]]\nformat = \"attributes-xml\"\npath = \"{path}\"\n\
                 setting = \"S\"\nwants = \"1\"\ntell = \"t\"\n"
            )
        };

        // Half one: none of the three shapes `check_path` refuses survives
        // being parsed, and the whole file goes rather than the one check —
        // which is this window's sentence to write, not the parser's.
        for bad in ["../drive_c/here.xml", "/etc/passwd", "drive_c\\\\users"] {
            let error = profiles::parse(&profile(bad)).expect_err(
                "a path `check_path` refuses has to be refused when the file is read, not \
                 joined onto a prefix and opened",
            );
            assert!(
                !error.to_string().contains("hex digits"),
                "{bad} has to reach `check_path`; dying in the string-escape parser is this \
                 test passing for the wrong reason: {error}"
            );
            let v = profile_verdict(Err(profiles::LoadError::Malformed {
                origin: at("/cfg/profiles/359320.toml"),
                error,
            }));
            let body = profile_intro(&v, "A Game", "359320", Path::new("/cfg/profiles"));
            assert!(body.contains("/cfg/profiles/359320.toml"), "{bad}: {body}");
            assert!(body.contains("None of it has been used"), "{bad}: {body}");
        }
    }

    /// The other side of the parser's answer: `.` and empty segments are not
    /// `..`, so they pass `check_path` and arrive here — and the file that
    /// gets read is the one under the prefix this window was handed, not the
    /// same name sitting beside it.
    ///
    /// This is a join, not a containment check. `path_under` pushes
    /// components onto the prefix and canonicalises nothing, so the
    /// `starts_with` below is a statement about the string it built, and it
    /// is the only thing the assertion is allowed to mean — see
    /// `a_check_read_through_a_drive_letter_lands_outside_the_prefix`, where
    /// `starts_with` is true of a path that resolves to `/`.
    ///
    /// What must break for this to fail: `path_under` joining onto something
    /// other than the prefix it is given, or `run_check` reading a path it
    /// did not get from `path_under`. The `leaked` copy one level up is what
    /// turns either into a wrong value rather than a missing file.
    #[test]
    fn an_odd_but_relative_path_is_read_from_under_the_prefix_not_from_beside_it() {
        let root = scratch("joins-under");
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

        for odd in [
            "drive_c/here.xml",
            "./drive_c/./here.xml",
            "drive_c//here.xml",
        ] {
            let text = format!(
                "version = 1\n\n[[check]]\nformat = \"attributes-xml\"\npath = \"{odd}\"\n\
                 setting = \"S\"\nwants = \"1\"\ntell = \"t\"\n"
            );
            let p = profiles::parse(&text)
                .unwrap_or_else(|e| panic!("{odd} has no `..`, no leading `/`, no `\\`: {e}"));
            let c = p.checks.first().expect("one check").clone();
            assert!(
                c.path_under(&prefix).starts_with(&prefix),
                "the string `path_under` built is the prefix plus components, and that is \
                 all this asserts — {odd}: {:?}",
                c.path_under(&prefix)
            );
            let answer = only(run_check(&c, Some(&prefix)).0);
            assert_eq!(
                answer,
                Answer::Text("inside".to_string()),
                "{odd} was read from beside the prefix rather than under it"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// **`run_check` reads files outside the prefix, and that is the point.**
    ///
    /// Five places in this project said a check can only ever reach inside
    /// the prefix. Four were deleted in an earlier round; the fifth was the
    /// doc comment on `run_check` itself, directly above the only line in
    /// this window that hands a path to `tobii-gameconf`, and it survived
    /// because the test named for the property never looked at a path that
    /// could disprove it. This test is that path.
    ///
    /// Every Wine prefix maps the machine into itself under `dosdevices/`:
    /// `z:` is `/`, and Steam's Proton prefixes add `s:` pointing at the
    /// library root. `profiles::check_path` refuses `..`, a leading `/` and a
    /// backslash, and none of those three has anything to say about
    /// `dosdevices/z:/…` — so the path parses, joins lexically under the
    /// prefix, and opens a file that is not under it at all. Profile authors
    /// are told to use exactly this to reach a game's shipped files under
    /// `steamapps/common/`, so the capability has to stay.
    ///
    /// What must break for this to fail: the doc comment's claim coming back
    /// as code — `run_check` canonicalising and refusing, `path_under`
    /// resolving symlinks, or `check_path` learning to refuse a drive letter.
    /// Any of those takes the documented capability away, and this test is
    /// where that has to be argued rather than assumed.
    ///
    /// `tobii-config`'s `a_check_path_reaches_what_the_prefix_reaches` proves
    /// the same thing one layer down, about `path_under`. This one proves it
    /// about the function whose comment made the claim, through the readers
    /// the window actually calls.
    #[test]
    fn a_check_read_through_a_drive_letter_lands_outside_the_prefix() {
        let root = scratch("reaches-out");
        let prefix = root.join("pfx");
        std::fs::create_dir_all(prefix.join("drive_c")).expect("fixture");
        std::fs::create_dir_all(prefix.join("dosdevices")).expect("fixture");
        // Nothing of this name exists under the prefix, so a read that
        // answers with these bytes can only have come from out here.
        let outside = root.join("library");
        std::fs::create_dir_all(&outside).expect("fixture");
        std::fs::write(
            outside.join("shipped.xml"),
            b"<Attributes><Attr name=\"S\" value=\"outside\"/></Attributes>",
        )
        .expect("fixture");
        // Exactly what Steam puts in a Proton prefix, made inside this test's
        // own scratch directory, which is the only place it writes.
        std::os::unix::fs::symlink(&outside, prefix.join("dosdevices/s:")).expect("s:");

        let text = "version = 1\n\n[[check]]\nformat = \"attributes-xml\"\n\
                    path = \"dosdevices/s:/shipped.xml\"\n\
                    setting = \"S\"\nwants = \"1\"\ntell = \"t\"\n";
        let p = profiles::parse(text).expect("a drive letter is not a `..`");
        let c = p.checks.first().expect("one check").clone();

        // The lexical assertion the old test made is still true here — which
        // is the whole point: it never could have caught this.
        assert!(
            c.path_under(&prefix).starts_with(&prefix),
            "{:?}",
            c.path_under(&prefix)
        );
        // And the resolved path is not under the prefix.
        let real = std::fs::canonicalize(c.path_under(&prefix)).expect("the file out here");
        let real_prefix = std::fs::canonicalize(&prefix).expect("canonicalize prefix");
        assert!(
            !real.starts_with(&real_prefix),
            "{real:?} was expected to resolve outside {real_prefix:?}"
        );

        // The bytes, through `run_check` itself.
        assert_eq!(
            only(run_check(&c, Some(&prefix)).0),
            Answer::Text("outside".to_string()),
            "a check through `dosdevices/s:` has to read the file out there — that is what \
             a profile author is told to use it for"
        );

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
            present: vec![],
        };
        let text = bridge_block(
            &state,
            &[],
            &[],
            None,
            Some(Path::new("/opt/x")),
            &Target::Steam("359320".to_string()),
        );
        // The button half is `actions`, which is the one decider: with no
        // `tobii` there is nothing to run, whatever the prefix holds.
        let bar = actions(
            &state,
            None,
            &quiet_job(),
            Reach::Reachable,
            false,
            Group::NotSetUp,
        );
        assert_eq!(bar.primary, None, "nothing to run it with: {text}");
        assert!(
            bar.blocked
                .is_some_and(|w| w.contains("`tobii` is not on the PATH")),
            "and the bar says so where the buttons would be: {bar:?}"
        );
        assert!(
            text.contains("tobii bridge install --steam 359320"),
            "{text}"
        );

        let with = bridge_block(
            &state,
            &[],
            &[],
            Some(Path::new("/home/x/.local/bin/tobii")),
            Some(Path::new("/opt/x")),
            &Target::Steam("359320".to_string()),
        );
        let bar = actions(
            &state,
            Some(Path::new("/home/x/.local/bin/tobii")),
            &quiet_job(),
            Reach::Reachable,
            false,
            Group::NotSetUp,
        );
        assert_eq!(bar.primary, Some(Action::Install));
        assert_eq!(bar.blocked, None, "{bar:?}");
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

        let steam = tobii_steam::Steam::at(&home);
        let chosen = steam.prefix("359320").expect("both prefixes exist");
        assert!(
            chosen.starts_with(&two),
            "the library holding the manifest owns the prefix: {chosen:?}"
        );

        // The window's own shape: one walk, head is the chosen one, tail is
        // the rest. A `filter` against `chosen` would be dropping element 0.
        let mut all = steam.prefixes("359320");
        assert_eq!(all[0], chosen, "`prefixes` leads with what `prefix` picks");
        all.remove(0);
        let others = other_prefixes(all);
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
        // The same `Steam` answers this: what it caches is the library
        // layout, and `prefix` stats `drive_c` on every call — so a prefix
        // made inside a library the walk already found is one it sees. A
        // second value here would be dead work, and a comment saying it could
        // not see the new prefix would teach the opposite of that type's
        // contract.
        let mut one_only = steam.prefixes("220");
        assert_eq!(one_only.len(), 1, "one prefix: {one_only:?}");
        one_only.remove(0);
        assert!(other_prefixes(one_only).is_empty());

        let _ = std::fs::remove_dir_all(&home);
    }
}
