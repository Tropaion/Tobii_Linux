//! What this program knows about setting up one game.
//!
//! # Three things need configuring, and a profile speaks about all three
//!
//! Setting a game up for a head tracker means three separate things: this
//! program's own settings (game output on, which sink, how hard gaze steers),
//! the wine-side TrackIR/FreeTrack bridge inside the game's Proton prefix, and
//! the game's own options. A profile says what one game wants of each —
//! `Profile::settings`, `Profile::bridge`, `Profile::checks`.
//!
//! The first two this program owns and changes. The third it **only reads**.
//! A `Check` names a file, a setting inside it and the value the game wants,
//! plus `Check::tell` — the sentence to show a person who has to go and
//! change it by hand. There is deliberately no field that would make this
//! program write into a game's own configuration: a bindings file is hours of
//! somebody's work in a format its author can change in any patch, and the
//! crate that reads those files, `tobii-gameconf`, has as its whole promise
//! that it never writes. A
//! profile cannot ask for something the reader it names is unable to do.
//!
//! # This ships zero profiles, and that is the intended state
//!
//! `BUILTIN` is empty. Nobody has yet played a game with this program's
//! bridge running and written down what it took, so there are no verified
//! per-game facts to compile in, and a profile invented from a forum post
//! would be exactly the confident wrong answer this project keeps finding in
//! its own bugs. The machinery is here; the knowledge is not, and until it is,
//! the wizard detects games, detects prefixes, sets this program's settings
//! and installs the bridge, and checks a game's own options only where a
//! profile says what to check.
//!
//! # The app id is the file name, and it is not in the file
//!
//! A profile lives at `<profiles dir>/<appid>.toml` and holds no app id of its
//! own. That is what makes "two profiles claiming one app id" not a case this
//! module has to arbitrate: within a directory the filesystem allows exactly
//! one file of that name, and a file cannot disagree with its own name about
//! which game it is for.
//!
//! What can collide is the two tiers — a compiled-in profile and a file the
//! user wrote for the same game — and there the user's file wins **whole**,
//! not field by field. A merge would leave a profile nobody wrote: a user who
//! found a shipped check wrong could not remove it, only add to it, and the
//! report would then carry a sentence with no author. Replacing the whole
//! thing keeps every profile something one person is responsible for, and
//! `Loaded::origin` says which one is in force so a report can name it.
//!
//! # Refusing a version, tolerating a format
//!
//! Forward compatibility runs on two axes, because two different things grow.
//!
//! `version` is the *grammar*. This build reads `VERSION` and nothing else.
//! A file that says `version = 2` is not broken and is not guessed at: it
//! comes back as a `ParseError` carrying `ParseError::unknown_version`, so
//! the sentence a user sees is *this profile was written for a newer
//! tobii-linux* and not *your profile is malformed*. Every key this build does
//! not know is a hard error for the same reason — a build that silently
//! ignored a field it had never heard of would apply the half of a profile it
//! understood, which is how you end up telling somebody to change a setting a
//! newer profile had already marked as not needed. Growing a field means
//! bumping the version, and an old build then declines the file by name.
//!
//! `Check::format` is an *open vocabulary*, and is handled the other way. A
//! format name this build cannot read parses fine and arrives as
//! `Format::Unknown`; the caller reports that one check as unreadable and
//! goes on with the rest. A third file format added later must not invalidate
//! a profile's checks against the two that already worked, and a version bump
//! would be far too blunt an instrument for it.
//!
//! # A profile may be partial
//!
//! `version` is the only key a profile must have. Settings without checks,
//! checks without settings, a bridge answer and nothing else are all valid —
//! and on day one, settings-only is what an honest profile looks like, since
//! the checks are the part that needs somebody to have verified a game.
//! `Profile::is_empty` is there for the caller that wants to say *this
//! profile asks for nothing*.
//!
//! # Why the parser is here and hand-written
//!
//! These files are hand-edited, so a refusal has to say which line and what it
//! wanted; that is the whole reason this is not a `split('=')` loop like
//! `DisplaySetup::from_toml`, which may quietly skip a line because the field
//! it feeds has a sane default. A profile has no sane default — silently
//! dropping a check means not telling somebody about a setting that is wrong.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::paths;

/// The profile grammar this build reads and writes.
///
/// A file naming any other version is refused, never guessed at: see the
/// module docs.
pub const VERSION: u32 = 1;

/// What every profile file is called: `<appid>` and this.
pub const FILE_SUFFIX: &str = ".toml";

/// The profiles compiled into this program, as `(appid, TOML text)`.
///
/// **Empty, on purpose.** See the module docs. It is a table rather than
/// nothing at all because the resolution order it takes part in
/// ([`load_from`]) is the thing that has to be right before the first entry
/// lands, and a code path that has never run is not one to trust with the
/// first real per-game facts this project produces.
///
/// A duplicate app id here would be a bug in this program rather than in
/// anybody's file; [`load_from`] takes the first and does not pretend to
/// choose.
pub const BUILTIN: &[(&str, &str)] = &[];

// ------------------------------------------------------------------- the type

/// Which file format a [`Check`] reads, and therefore which reader answers it.
///
/// The names are the format's, not any game's: this crate ships no game
/// knowledge, and a profile for a game nobody here has heard of should be
/// able to name a format without the format having to be renamed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Format {
    /// A directory of preset documents plus the `StartPreset` file naming
    /// which of them is live — `tobii_gameconf::binds`.
    ///
    /// [`Check::path`] names the **directory**, not a file in it. Which
    /// preset is in force is the reader's problem, not the profile's.
    BindsDir,
    /// One flat `<Attributes><Attr name= value=/></Attributes>` document —
    /// `tobii_gameconf::attrs`.
    ///
    /// [`Check::path`] names the **file**.
    AttributesXml,
    /// A format this build cannot read, carried through as the profile spells
    /// it.
    ///
    /// Not an error: see the module docs. A caller reports the check as one it
    /// cannot perform, naming the string, and performs the others.
    Unknown(String),
}

impl Format {
    /// The name as a profile spells it. Round-trips, [`Format::Unknown`]
    /// included — a file this build could not fully understand still comes
    /// back out the way it went in.
    pub fn as_str(&self) -> &str {
        match self {
            Format::BindsDir => "binds-dir",
            Format::AttributesXml => "attributes-xml",
            Format::Unknown(s) => s,
        }
    }

    /// Whether a reader in this build answers this format.
    pub fn is_known(&self) -> bool {
        !matches!(self, Format::Unknown(_))
    }

    fn from_name(s: &str) -> Format {
        match s {
            "binds-dir" => Format::BindsDir,
            "attributes-xml" => Format::AttributesXml,
            other => Format::Unknown(other.to_string()),
        }
    }
}

/// One setting in the game's own configuration, and what it should say.
///
/// Read and report. Nothing here asks this program to change the file: see
/// the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Which reader answers this, and so what [`Self::path`] means.
    pub format: Format,
    /// Where the file (or, for [`Format::BindsDir`], the directory) sits
    /// **relative to the Proton prefix** — the directory holding `drive_c`.
    ///
    /// Always relative, always `/`-separated, and never with a `..` component:
    /// [`parse`] refuses the rest at the line that held it. A profile is a
    /// hand-edited file whose path is joined onto a prefix and then opened,
    /// and `..` would let one point the reader anywhere on the machine. A
    /// backslash is refused too — it would be a literal character in a Linux
    /// path, so a Windows-style path would not fail, it would silently name
    /// nothing.
    pub path: String,
    /// What to ask for inside it: an element name for [`Format::BindsDir`]
    /// (`HeadlookMode` names `<HeadlookMode Value="…"/>`), an attribute name
    /// for [`Format::AttributesXml`].
    pub setting: String,
    /// The value the game wants, exactly as the profile spells it.
    ///
    /// This module does not compare it to anything, and deliberately offers no
    /// helper that would: whether either game's own reader is case-sensitive
    /// is not something this project has measured, so a `matches()` here would
    /// be a guess wearing an authoritative name. The caller compares, and owns
    /// the rule it used.
    pub wants: String,
    /// The sentence to show somebody who has to change it by hand.
    ///
    /// Required, because a check whose failure cannot be acted on is worse
    /// than no check: it tells a user something is wrong and not what to do.
    pub tell: String,
}

impl Check {
    /// This check's path under a Proton prefix.
    ///
    /// Safe to join because [`Self::path`] is relative and has no `..`; that
    /// is established when the profile is parsed, so it cannot be re-litigated
    /// at every call site.
    pub fn path_under(&self, prefix: &Path) -> PathBuf {
        let mut p = prefix.to_path_buf();
        for part in self.path.split('/').filter(|s| !s.is_empty()) {
            p.push(part);
        }
        p
    }
}

/// Whether this game needs the wine-side bridge.
///
/// Three cases rather than a `bool`, for the reason `tobii_gameconf::Lookup`
/// has three: a profile that does not mention the bridge has not said the game
/// does without one. *Nobody wrote this down* and *this game does not need it*
/// lead to different sentences, and only one of them is a claim somebody made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Bridge {
    /// The profile does not say. Not a "no".
    #[default]
    Unstated,
    /// This game reads TrackIR or FreeTrack through the bridge.
    Required,
    /// Somebody established that this game does not need it.
    NotNeeded,
}

/// Everything this program knows about setting up one game.
///
/// No app id: that is the file's name. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Profile {
    /// What to call the game in a report, if the profile bothered to say.
    /// Display only — nothing matches on it, and `tobii-steam` has the name
    /// the machine itself gives.
    pub name: Option<String>,
    /// Whether the wine-side bridge is needed.
    pub bridge: Bridge,
    /// This program's own settings, in the order the file gives them, as
    /// `(key, value)` text.
    ///
    /// Text, and unvalidated here. The keys belong to `tobii-output`'s
    /// `OutputConfig::apply_key`, which is the one place that decides what a
    /// key means and whether a value parses — and `tobii-output` depends on
    /// this crate, so this crate cannot ask it. A caller applies these through
    /// `apply_key` and reports the pairs it rejected; a second opinion here
    /// would be a second list to drift out of step with the first, which is
    /// the mistake `OutputConfig::keys` already carries a comment about.
    pub settings: Vec<(String, String)>,
    /// What to read in the game's own configuration, and what it should say.
    pub checks: Vec<Check>,
}

impl Profile {
    /// Whether this profile asks for nothing at all: no settings, no checks,
    /// and no answer about the bridge.
    ///
    /// [`Self::name`] does not count — a file that only names the game still
    /// asks for nothing.
    pub fn is_empty(&self) -> bool {
        self.settings.is_empty() && self.checks.is_empty() && self.bridge == Bridge::Unstated
    }

    /// The format names in this profile that no reader in this build answers,
    /// in order, without repeats.
    ///
    /// For the one sentence a report owes a user whose profile is newer than
    /// their build.
    pub fn unknown_formats(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for c in &self.checks {
            if let Format::Unknown(s) = &c.format {
                if !out.contains(&s.as_str()) {
                    out.push(s);
                }
            }
        }
        out
    }

    /// This profile as a profile file.
    ///
    /// Round-trips: [`parse`] of this text gives back an equal `Profile`, for
    /// every string any field can hold — see `a_profile_survives_a_round_trip`
    /// in the tests below. That is the property the GUI needs, since it will
    /// read a file a user wrote, change one field and write it back.
    ///
    /// Every value goes out quoted, including a setting that reads as a number
    /// or a boolean. A hand-written `enabled = true` is accepted — see
    /// [`parse`] — but what a setting *is* is text on its way to
    /// `OutputConfig::apply_key`, and a writer that sometimes quoted and
    /// sometimes did not would have to decide which, on a value it is not
    /// entitled to have an opinion about.
    pub fn to_toml(&self) -> String {
        let mut s = String::from("# tobii-linux game profile\n");
        s.push_str(&format!("version = {VERSION}\n"));
        if let Some(n) = &self.name {
            s.push_str(&format!("name = {}\n", quote(n)));
        }
        match self.bridge {
            Bridge::Unstated => {}
            Bridge::Required => s.push_str("bridge = true\n"),
            Bridge::NotNeeded => s.push_str("bridge = false\n"),
        }
        if !self.settings.is_empty() {
            s.push_str("\n[settings]\n");
            for (k, v) in &self.settings {
                s.push_str(&format!("{k} = {}\n", quote(v)));
            }
        }
        for c in &self.checks {
            s.push_str("\n[[check]]\n");
            s.push_str(&format!("format = {}\n", quote(c.format.as_str())));
            s.push_str(&format!("path = {}\n", quote(&c.path)));
            s.push_str(&format!("setting = {}\n", quote(&c.setting)));
            s.push_str(&format!("wants = {}\n", quote(&c.wants)));
            s.push_str(&format!("tell = {}\n", quote(&c.tell)));
        }
        s
    }
}

// ------------------------------------------------------------------- errors

/// Why a profile file could not be read as one.
///
/// The message is written to be printed at a user who is about to open the
/// file in an editor, which is why it carries the line and says what the line
/// would have had to hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// The 1-based line, when one line is at fault. [`None`] when the file as
    /// a whole is (an empty file, a file with no `version`, a `[[check]]`
    /// missing a key is reported at its header line, so that one is `Some`).
    pub line: Option<usize>,
    /// The whole sentence, written to be shown.
    pub message: String,
    /// The version the file asked for, when that — and only that — is what
    /// this build could not do.
    ///
    /// A caller that wants to say *update tobii-linux* rather than *fix your
    /// file* matches on this. See the module docs.
    pub unknown_version: Option<u32>,
}

impl ParseError {
    fn at(line: usize, message: impl Into<String>) -> ParseError {
        ParseError {
            line: Some(line),
            message: message.into(),
            unknown_version: None,
        }
    }

    fn whole(message: impl Into<String>) -> ParseError {
        ParseError {
            line: None,
            message: message.into(),
            unknown_version: None,
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(n) => write!(f, "line {n}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ParseError {}

/// Where a profile came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Compiled into this program. [`BUILTIN`] is empty, so nothing produces
    /// this today.
    Builtin,
    /// A file the user has, which outranks any built-in one for the same game.
    File(PathBuf),
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Builtin => f.write_str("built into tobii-linux"),
            Origin::File(p) => write!(f, "{}", p.display()),
        }
    }
}

/// A profile and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub origin: Origin,
    pub profile: Profile,
}

/// Why a profile that is *there* could not be used.
///
/// A profile that is simply absent is not an error and never appears here:
/// [`load_from`] answers `Ok(None)` for that and for nothing else. The two
/// negative answers are not interchangeable — reporting a file this program
/// could not open as a game nobody has configured is the exact shape of bug
/// this project keeps finding in itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// It is there and could not be read: a permission error, a directory
    /// where the file should be, a failing disk.
    Unreadable { origin: Origin, why: String },
    /// It was read and is not a profile this build can use.
    Malformed { origin: Origin, error: ParseError },
    /// The caller asked about something that is not an app id, so no profile
    /// could exist for it. An error rather than "no profile": answering
    /// *nothing configured* to a question that was never askable would be a
    /// lie about the disk.
    NotAnAppId(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Unreadable { origin, why } => write!(f, "{origin}: {why}"),
            LoadError::Malformed { origin, error } => write!(f, "{origin}: {error}"),
            LoadError::NotAnAppId(s) => {
                write!(f, "{s:?} is not a Steam app id, so it has no profile")
            }
        }
    }
}

impl std::error::Error for LoadError {}

// -------------------------------------------------------------------- naming

/// Whether `s` is an app id this module will name a file after: one to ten
/// ASCII digits, with no leading zero.
///
/// Deliberately tighter than `tobii_steam::resolve`'s "is this all digits",
/// which is a question about what a user typed. This is a question about a
/// file name, and a file name has to be canonical: `0999.toml` and `999.toml`
/// would be two profiles for one game, of which this program would only ever
/// write and find the second. Ten digits is `u32`, which is what a Steam app
/// id is.
pub fn is_appid(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_digit() && c != '0')
        && cs.all(|c| c.is_ascii_digit())
        && s.len() <= 10
}

/// Whether `name` is a file this program writes into [`profiles_dir`]:
/// `<appid>.toml`, or the temporary an interrupted [`crate::write_atomic`]
/// leaves beside it.
///
/// `paths` names every file this program writes so that `tobii uninstall
/// --purge` can delete those and report everything else. A profile directory
/// cannot be a list of names — the names are one per game the user has — so
/// what it contributes to that rule is this predicate, and the caller builds
/// its list by filtering the directory through it. Whatever else a user keeps
/// in there is not written by this program and is reported, exactly as a
/// `config.toml.bak` beside the config is.
pub fn is_profile_file(name: &str) -> bool {
    let base = name
        .strip_suffix(paths::ATOMIC_TMP_SUFFIX)
        .unwrap_or(name)
        .strip_suffix(FILE_SUFFIX);
    base.is_some_and(is_appid)
}

/// `<config dir>/profiles`.
pub fn profiles_dir() -> PathBuf {
    paths::config_dir().join(paths::PROFILES_DIR)
}

/// Where a game's profile goes, under a given profiles directory.
///
/// The app id is not checked here — this is a name, and [`load_from`] and
/// [`save_to`] are where a bad one is refused rather than written.
pub fn path_in(dir: &Path, appid: &str) -> PathBuf {
    dir.join(format!("{appid}{FILE_SUFFIX}"))
}

/// Where a game's profile goes in this user's config.
pub fn profile_path(appid: &str) -> PathBuf {
    path_in(&profiles_dir(), appid)
}

// ------------------------------------------------------------------- reading

/// Read one profile, user file first, then `builtin`.
///
/// `Ok(None)` means *there is no profile for this game*, and it means only
/// that: a file that is there and unreadable is [`LoadError::Unreadable`] and
/// does **not** fall through to a built-in profile. Falling through would
/// answer a question about one file with the contents of another, which is
/// how a user ends up being told their own edit did nothing.
pub fn load_from(
    dir: &Path,
    builtin: &[(&str, &str)],
    appid: &str,
) -> Result<Option<Loaded>, LoadError> {
    if !is_appid(appid) {
        return Err(LoadError::NotAnAppId(appid.to_string()));
    }
    let path = path_in(dir, appid);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let origin = Origin::File(path);
            return match parse(&text) {
                Ok(profile) => Ok(Some(Loaded { origin, profile })),
                Err(error) => Err(LoadError::Malformed { origin, error }),
            };
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(LoadError::Unreadable {
                origin: Origin::File(path),
                why: e.to_string(),
            })
        }
    }
    match builtin.iter().find(|(id, _)| *id == appid) {
        Some((_, text)) => match parse(text) {
            Ok(profile) => Ok(Some(Loaded {
                origin: Origin::Builtin,
                profile,
            })),
            Err(error) => Err(LoadError::Malformed {
                origin: Origin::Builtin,
                error,
            }),
        },
        None => Ok(None),
    }
}

/// Read one profile from this user's config, then [`BUILTIN`].
pub fn load(appid: &str) -> Result<Option<Loaded>, LoadError> {
    load_from(&profiles_dir(), BUILTIN, appid)
}

/// Every profile there is, and everything in the directory that is not one.
///
/// Both halves, because a user whose profile is not being applied needs to see
/// why far more than they need a tidy list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// `(appid, profile)`, in app id order.
    pub profiles: Vec<(String, Loaded)>,
    /// What was in the directory and is not a usable profile, in the order the
    /// directory was read.
    pub problems: Vec<LoadError>,
    /// Names in the directory that are not profile files at all, sorted.
    ///
    /// Separate from [`Self::problems`]: an editor's `<appid>.toml~` is not a
    /// broken profile, it is somebody's backup, and `--purge` will say the
    /// same thing about it. A caller may reasonably print these more quietly,
    /// or not at all.
    pub strays: Vec<String>,
}

/// List every profile under `dir`, with `builtin` behind it.
///
/// A directory that is not there is not a problem — it is a user who has
/// written no profiles — but one that cannot be listed is.
pub fn list_from(dir: &Path, builtin: &[(&str, &str)]) -> Listing {
    let mut out = Listing::default();
    for (appid, text) in builtin {
        match parse(text) {
            Ok(profile) => out.profiles.push((
                (*appid).to_string(),
                Loaded {
                    origin: Origin::Builtin,
                    profile,
                },
            )),
            Err(error) => out.problems.push(LoadError::Malformed {
                origin: Origin::Builtin,
                error,
            }),
        }
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return sorted(out),
        Err(e) => {
            out.problems.push(LoadError::Unreadable {
                origin: Origin::File(dir.to_path_buf()),
                why: e.to_string(),
            });
            return sorted(out);
        }
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        let Some(appid) = name.strip_suffix(FILE_SUFFIX).filter(|s| is_appid(s)) else {
            out.strays.push(name);
            continue;
        };
        match load_from(dir, &[], appid) {
            // A user file replaces the built-in one for the same game, whole.
            Ok(Some(loaded)) => match out.profiles.iter_mut().find(|(id, _)| id == appid) {
                Some(slot) => slot.1 = loaded,
                None => out.profiles.push((appid.to_string(), loaded)),
            },
            // Unreachable: the name came off this directory a moment ago. If
            // it has gone since, it is not a profile any more, and saying
            // nothing about it is the true report.
            Ok(None) => {}
            Err(e) => out.problems.push(e),
        }
    }
    sorted(out)
}

/// App id order, which for these strings is numeric order: [`is_appid`]
/// forbids a leading zero, so a shorter digit string is always the smaller
/// number.
fn sorted(mut l: Listing) -> Listing {
    l.profiles
        .sort_by(|a, b| a.0.len().cmp(&b.0.len()).then(a.0.cmp(&b.0)));
    l.strays.sort();
    l
}

/// List this user's profiles, with [`BUILTIN`] behind them.
pub fn list() -> Listing {
    list_from(&profiles_dir(), BUILTIN)
}

// ------------------------------------------------------------------- writing

/// Write a profile into `dir`, creating it if need be.
///
/// Atomic, like every other file this program writes: a half-written profile
/// would be refused on the next read, and the user would be told their own
/// file is malformed by a program that malformed it.
pub fn save_to(dir: &Path, appid: &str, profile: &Profile) -> io::Result<()> {
    if !is_appid(appid) {
        // Before the path is built, not after: `path_in` would happily make a
        // name out of `../../anything`.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{appid:?} is not a Steam app id"),
        ));
    }
    crate::write_atomic(&path_in(dir, appid), profile.to_toml().as_bytes())
}

/// Write a profile into this user's config.
pub fn save(appid: &str, profile: &Profile) -> io::Result<()> {
    save_to(&profiles_dir(), appid, profile)
}

// ------------------------------------------------------------------- parsing

/// Read a profile file.
///
/// Every refusal names the line and what that line would have had to hold.
/// Nothing is skipped and nothing falls back to a default: see the module
/// docs.
pub fn parse(text: &str) -> Result<Profile, ParseError> {
    // A byte-order mark is not whitespace, so without this a file some editors
    // write would be refused at a line one that says `version = 1` and looks
    // exactly right — the most baffling refusal this parser could produce.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut version: Option<u32> = None;
    let mut name: Option<(String, usize)> = None;
    let mut bridge: Option<(Bridge, usize)> = None;
    let mut settings: Vec<(String, String, usize)> = Vec::new();
    let mut settings_at: Option<usize> = None;
    let mut checks: Vec<Draft> = Vec::new();
    let mut table = Table::Top;

    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let t = raw.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }

        // The version gates the grammar, so it is read before anything the
        // grammar governs. Anywhere else and this reader would have to
        // interpret a file's body to find out whether it was allowed to.
        if version.is_none() {
            let Some(rest) = t.strip_prefix("version") else {
                return Err(ParseError::at(
                    line,
                    "a profile starts with `version = 1`, before anything else",
                ));
            };
            let Some(rest) = rest.trim_start().strip_prefix('=') else {
                return Err(ParseError::at(line, "`version = 1`"));
            };
            // Whatever is wrong with the value, the sentence is about
            // `version`: this is line one of a file somebody is about to open
            // in an editor, and "only `true`, `false` and numbers go bare" is
            // not what they need to be told there.
            let whole = || ParseError::at(line, "`version` is a whole number, as in `version = 1`");
            let Ok((v, Kind::Bare)) = value(rest, line) else {
                return Err(whole());
            };
            let Ok(v) = v.parse::<u32>() else {
                return Err(whole());
            };
            if v != VERSION {
                return Err(ParseError {
                    line: Some(line),
                    message: format!(
                        "this profile is version {v}; this tobii-linux reads version {VERSION}. \
                         Update tobii-linux, or write the profile for version {VERSION}."
                    ),
                    unknown_version: Some(v),
                });
            }
            version = Some(v);
            continue;
        }

        if let Some(header) = table_header(t, line)? {
            match header {
                Table::Settings => {
                    if let Some(first) = settings_at {
                        return Err(ParseError::at(
                            line,
                            format!("`[settings]` is already open from line {first}"),
                        ));
                    }
                    settings_at = Some(line);
                }
                Table::Check => checks.push(Draft::new(line)),
                Table::Top => {}
            }
            table = header;
            continue;
        }

        let (key, rest) = split_key(t, line)?;
        match table {
            Table::Top => match key {
                "version" => {
                    return Err(ParseError::at(line, "`version` is already set"));
                }
                "name" => {
                    let (v, _) = value(rest, line)?;
                    if let Some((_, first)) = name {
                        return Err(ParseError::at(
                            line,
                            format!("`name` is already set on line {first}"),
                        ));
                    }
                    name = Some((v, line));
                }
                "bridge" => {
                    if let Some((_, first)) = bridge {
                        return Err(ParseError::at(
                            line,
                            format!("`bridge` is already set on line {first}"),
                        ));
                    }
                    // Same reasoning as `version`: there are two right answers
                    // here and a wrong one is told which they are, rather than
                    // being told the general rule about bare values.
                    let b = match value(rest, line) {
                        Ok((v, Kind::Bare)) if v == "true" => Bridge::Required,
                        Ok((v, Kind::Bare)) if v == "false" => Bridge::NotNeeded,
                        _ => return Err(ParseError::at(line, "`bridge` is `true` or `false`")),
                    };
                    bridge = Some((b, line));
                }
                other => {
                    return Err(ParseError::at(
                        line,
                        format!(
                            "`{other}` is not a key this build knows. Before any table a profile \
                             holds `version`, `name` and `bridge`; this program's own settings go \
                             under `[settings]` and a check under `[[check]]`."
                        ),
                    ))
                }
            },
            Table::Settings => {
                if let Some((_, _, first)) = settings.iter().find(|(k, _, _)| k == key) {
                    return Err(ParseError::at(
                        line,
                        format!("`{key}` is already set on line {first}"),
                    ));
                }
                let (v, _) = value(rest, line)?;
                settings.push((key.to_string(), v, line));
            }
            Table::Check => {
                let draft = checks.last_mut().expect("a [[check]] opened this table");
                let (v, _) = value(rest, line)?;
                let slot = match key {
                    "format" => &mut draft.format,
                    "path" => &mut draft.path,
                    "setting" => &mut draft.setting,
                    "wants" => &mut draft.wants,
                    "tell" => &mut draft.tell,
                    other => {
                        return Err(ParseError::at(
                            line,
                            format!(
                                "`{other}` is not a key of `[[check]]`, which holds `format`, \
                                 `path`, `setting`, `wants` and `tell`. A check is read and \
                                 reported; there is no key that changes the game's file."
                            ),
                        ))
                    }
                };
                if let Some((_, first)) = slot {
                    return Err(ParseError::at(
                        line,
                        format!("`{key}` is already set on line {first}"),
                    ));
                }
                if key == "path" {
                    check_path(&v, line)?;
                }
                if key == "format" && v.is_empty() {
                    return Err(ParseError::at(line, "`format` names a file format"));
                }
                *slot = Some((v, line));
            }
        }
    }

    if version.is_none() {
        return Err(ParseError::whole(
            "this file has no `version` key, so it is not a profile. A profile starts with \
             `version = 1`.",
        ));
    }

    Ok(Profile {
        name: name.map(|(v, _)| v),
        bridge: bridge.map(|(b, _)| b).unwrap_or_default(),
        settings: settings.into_iter().map(|(k, v, _)| (k, v)).collect(),
        checks: checks
            .into_iter()
            .map(Draft::finish)
            .collect::<Result<_, _>>()?,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Table {
    Top,
    Settings,
    Check,
}

/// A `[[check]]` being read, before it is known to have every key.
struct Draft {
    at: usize,
    format: Option<(String, usize)>,
    path: Option<(String, usize)>,
    setting: Option<(String, usize)>,
    wants: Option<(String, usize)>,
    tell: Option<(String, usize)>,
}

impl Draft {
    fn new(at: usize) -> Draft {
        Draft {
            at,
            format: None,
            path: None,
            setting: None,
            wants: None,
            tell: None,
        }
    }

    fn finish(self) -> Result<Check, ParseError> {
        let at = self.at;
        let need = |slot: Option<(String, usize)>, key: &str| match slot {
            Some((v, _)) => Ok(v),
            // Reported at the header, which is where a reader looking for the
            // missing key will look, rather than at the blank line after it.
            None => Err(ParseError::at(
                at,
                format!("this `[[check]]` has no `{key}` key"),
            )),
        };
        Ok(Check {
            format: Format::from_name(&need(self.format, "format")?),
            path: need(self.path, "path")?,
            setting: need(self.setting, "setting")?,
            wants: need(self.wants, "wants")?,
            tell: need(self.tell, "tell")?,
        })
    }
}

/// A table header, if this line is one.
fn table_header(t: &str, line: usize) -> Result<Option<Table>, ParseError> {
    if !t.starts_with('[') {
        return Ok(None);
    }
    let close = if t.starts_with("[[") { "]]" } else { "]" };
    let Some(i) = t.find(close) else {
        return Err(ParseError::at(line, "a closing `]`"));
    };
    let head = &t[..i + close.len()];
    let after = t[head.len()..].trim_start();
    if !after.is_empty() && !after.starts_with('#') {
        return Err(ParseError::at(line, "nothing but a comment after a table"));
    }
    match head {
        "[settings]" => Ok(Some(Table::Settings)),
        "[[check]]" => Ok(Some(Table::Check)),
        other => Err(ParseError::at(
            line,
            format!(
                "`{other}` is not a table of a profile, which has `[settings]` and `[[check]]`"
            ),
        )),
    }
}

/// The key on the left of `=`, and everything after the `=`.
fn split_key(t: &str, line: usize) -> Result<(&str, &str), ParseError> {
    let Some((key, rest)) = t.split_once('=') else {
        return Err(ParseError::at(line, "`key = value`"));
    };
    let key = key.trim();
    if key.is_empty() {
        return Err(ParseError::at(line, "a key before the `=`"));
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(ParseError::at(
            line,
            format!("`{key}` is not a key: a key here is letters, digits, `_` and `-`"),
        ));
    }
    Ok((key, rest))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Quoted,
    Bare,
}

/// One value after a `=`, with whatever follows it checked to be a comment.
///
/// Every value comes back as text, whichever way it was written. A bare value
/// has to be something TOML would read as a scalar — `true`, `false` or a
/// number — so that a profile is a TOML file and not a lookalike, and it is
/// carried through as the digits the file actually spells: `rate_hz = 60.0`
/// reaches `apply_key` as `"60.0"`, never as a float this program formatted
/// back.
fn value(rest: &str, line: usize) -> Result<(String, Kind), ParseError> {
    let s = rest.trim_start();
    if let Some(body) = s.strip_prefix('"') {
        let (text, used) = unquote(body, line)?;
        let after = body[used..].trim_start();
        if !after.is_empty() && !after.starts_with('#') {
            return Err(ParseError::at(line, "nothing but a comment after a value"));
        }
        return Ok((text, Kind::Quoted));
    }
    let tok: &str = s
        .split(|c: char| c.is_whitespace() || c == '#')
        .next()
        .unwrap_or("");
    if tok.is_empty() {
        return Err(ParseError::at(line, "a value after the `=`"));
    }
    // Before the trailing check, so that `name = A Game` is told to quote it
    // rather than told there is something after the value — which is true, and
    // is not the mistake.
    if !is_scalar(tok) {
        return Err(ParseError::at(
            line,
            format!("`{tok}` has to be quoted: only `true`, `false` and numbers go bare"),
        ));
    }
    let after = s[tok.len()..].trim_start();
    if !after.is_empty() && !after.starts_with('#') {
        return Err(ParseError::at(line, "nothing but a comment after a value"));
    }
    Ok((tok.to_string(), Kind::Bare))
}

fn is_scalar(tok: &str) -> bool {
    tok == "true"
        || tok == "false"
        || tok.parse::<i64>().is_ok()
        || tok.parse::<f64>().is_ok_and(|v| v.is_finite())
}

/// The contents of a basic string, given everything after its opening quote,
/// with how many bytes of that it consumed including the closing quote.
fn unquote(body: &str, line: usize) -> Result<(String, usize), ParseError> {
    let unterminated = || ParseError::at(line, "a closing `\"`");
    let mut out = String::new();
    let mut i = 0;
    while i < body.len() {
        // Indexing by byte, and every byte compared against here is ASCII, so
        // `i` only ever lands on a character boundary.
        let c = body[i..].chars().next().expect("a character at a boundary");
        match c {
            '"' => return Ok((out, i + 1)),
            '\\' => {
                let Some(e) = body[i + 1..].chars().next() else {
                    return Err(unterminated());
                };
                i += 1 + e.len_utf8();
                match e {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'u' => {
                        // `get` rather than slicing: the four bytes may run off
                        // the end of the line or into the middle of a
                        // character, and both are the same refusal.
                        let hex = body.get(i..i + 4).unwrap_or("");
                        // `from_str_radix` accepts a leading `+` and a sign is
                        // not a hex digit; checking the digits first is the
                        // rule `tobii-gameconf`'s XML reader states for the
                        // same parser habit.
                        let ok = hex.len() == 4 && hex.bytes().all(|b| b.is_ascii_hexdigit());
                        let ch = u32::from_str_radix(hex, 16)
                            .ok()
                            .filter(|_| ok)
                            .and_then(char::from_u32);
                        let Some(ch) = ch else {
                            return Err(ParseError::at(
                                line,
                                "`\\u` takes four hex digits naming a character",
                            ));
                        };
                        out.push(ch);
                        i += 4;
                    }
                    other => {
                        return Err(ParseError::at(
                            line,
                            format!(
                                "`\\{other}` is not an escape this reader knows: `\\\"`, `\\\\`, \
                                 `\\n`, `\\t`, `\\r` and `\\uXXXX`"
                            ),
                        ))
                    }
                }
            }
            // TOML's rule, and a useful one: a control character in a file
            // meant to be hand-edited is a paste accident, and a sentence with
            // one in it would print as something the user cannot see.
            c if c.is_control() && c != '\t' => {
                return Err(ParseError::at(
                    line,
                    "a control character in a string; write it as `\\uXXXX`",
                ))
            }
            c => {
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    Err(unterminated())
}

/// A check's path: relative, `/`-separated, no `..`. See [`Check::path`].
fn check_path(p: &str, line: usize) -> Result<(), ParseError> {
    if p.is_empty() {
        return Err(ParseError::at(
            line,
            "`path` names a place under the prefix",
        ));
    }
    if p.starts_with('/') {
        return Err(ParseError::at(
            line,
            "`path` is relative to the Proton prefix, so it does not start with `/`",
        ));
    }
    if p.contains('\\') {
        return Err(ParseError::at(
            line,
            "`path` uses `/`, not `\\`: a backslash is an ordinary character in a Linux path, so \
             a Windows-style path would not fail, it would name nothing",
        ));
    }
    if p.split('/').any(|part| part == "..") {
        return Err(ParseError::at(
            line,
            "`path` may not have a `..` component: it is joined onto a prefix and then opened",
        ));
    }
    Ok(())
}

/// A string as a profile file spells it.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not any game: these tests are about the shape of a profile, and this
    /// crate ships no per-game knowledge to test against.
    const FAKE: &str = "1234567";
    const OTHER: &str = "7654321";

    /// A fresh, empty directory of this test's own, by the rule `store`'s
    /// tests use: the pid keeps overlapping runs apart, the tag keeps the
    /// tests of one run apart.
    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tobii-profiles-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        d
    }

    fn full() -> Profile {
        Profile {
            name: Some("A Game".into()),
            bridge: Bridge::Required,
            settings: vec![
                ("enabled".into(), "true".into()),
                ("rate_hz".into(), "60.0".into()),
            ],
            checks: vec![Check {
                format: Format::BindsDir,
                path: "drive_c/users/steamuser/Options/Bindings".into(),
                setting: "HeadlookMode".into(),
                wants: "1".into(),
                tell: "Set head look to toggle.".into(),
            }],
        }
    }

    // ------------------------------------------------------------ round trip

    /// Everything this program writes it must read back identically — the GUI
    /// will read a file, change one field and write it back, and a value that
    /// did not survive that would be a user's sentence silently rewritten.
    ///
    /// The strings are the ones escaping exists for: a quote, a backslash, a
    /// newline, a tab, a `#` that is not a comment, a `=` that is not a
    /// separator, a control character with no named escape, and a character
    /// outside ASCII.
    #[test]
    fn a_profile_survives_a_round_trip() {
        let nasty = "a \"quote\", a \\ backslash,\na tab\there, # not a comment, = not a key, \
                     \u{7}bell, and é";
        let p = Profile {
            name: Some(nasty.into()),
            bridge: Bridge::NotNeeded,
            settings: vec![("opentrack".into(), nasty.into())],
            checks: vec![Check {
                format: Format::Unknown("something-newer".into()),
                path: "a dir/with spaces".into(),
                setting: nasty.into(),
                wants: nasty.into(),
                tell: nasty.into(),
            }],
        };
        let text = p.to_toml();
        assert_eq!(parse(&text), Ok(p), "{text}");

        // And the ordinary one, and the emptiest one a profile can be.
        assert_eq!(parse(&full().to_toml()), Ok(full()));
        let bare = Profile::default();
        assert!(bare.is_empty());
        assert_eq!(parse(&bare.to_toml()), Ok(bare));
    }

    /// Settings keep the file's own spelling of a number, because they are
    /// handed to `apply_key` as text: a profile asking for `60.0` must not
    /// arrive as `60` because something here parsed and reprinted it.
    #[test]
    fn a_bare_number_keeps_the_spelling_the_file_gave_it() {
        let p = parse("version = 1\n[settings]\nrate_hz = 60.0\nbridge_port = 4242\n").expect("ok");
        assert_eq!(
            p.settings,
            vec![
                ("rate_hz".to_string(), "60.0".to_string()),
                ("bridge_port".to_string(), "4242".to_string()),
            ]
        );
    }

    // --------------------------------------------------------------- parsing

    #[test]
    fn a_whole_profile_parses() {
        let text = "# a profile\n\
                    version = 1\n\
                    name = \"A Game\"  # display only\n\
                    bridge = true\n\
                    \n\
                    [settings]\n\
                    enabled = true\n\
                    rate_hz = 60.0\n\
                    \n\
                    [[check]]\n\
                    format = \"attributes-xml\"\n\
                    path = \"drive_c/Roaming/attributes.xml\"\n\
                    setting = \"HeadTracking\"\n\
                    wants = \"1\"\n\
                    tell = \"Turn head tracking on.\"\n";
        let p = parse(text).expect("parses");
        assert_eq!(p.name.as_deref(), Some("A Game"));
        assert_eq!(p.bridge, Bridge::Required);
        assert_eq!(p.settings.len(), 2);
        assert_eq!(p.checks[0].format, Format::AttributesXml);
        assert_eq!(p.checks[0].setting, "HeadTracking");
        assert!(!p.is_empty());
    }

    /// A `#` inside a quoted string is part of the sentence. The old
    /// `DisplaySetup` parser cuts at the first `#` on the line, which is fine
    /// for numbers and would eat half of every `tell` written here.
    #[test]
    fn a_hash_inside_a_string_is_not_a_comment() {
        let p = parse("version = 1\nname = \"A game # 2\" # but this is\n").expect("parses");
        assert_eq!(p.name.as_deref(), Some("A game # 2"));
    }

    /// A profile written by an editor that marks its files, with the line
    /// endings that editor is likely to have used, is still a profile.
    #[test]
    fn a_file_from_a_windows_editor_parses() {
        let p = parse("\u{feff}# a profile\r\nversion = 1\r\nname = \"A Game\"\r\n")
            .expect("a mark and CRLF");
        assert_eq!(p.name.as_deref(), Some("A Game"));
    }

    /// The file says which line and what it wanted — the whole reason this is
    /// not a loop that skips what it does not understand.
    #[test]
    fn a_refusal_names_the_line_and_what_it_wanted() {
        for (text, line, wanted) in [
            ("version = 1\nname = A Game\n", 2, "quoted"),
            ("version = 1\nname = \"unterminated\n", 2, "closing"),
            ("version = 1\nwat = 1\n", 2, "not a key this build knows"),
            ("version = 1\n[nope]\n", 2, "not a table of a profile"),
            (
                "version = 1\n[settings]\nx = 1\n[settings]\ny = 2\n",
                4,
                "already open",
            ),
            (
                "version = 1\nname = \"a\"\nname = \"b\"\n",
                3,
                "already set on line 2",
            ),
            (
                "version = 1\n[[check]]\nnope = \"x\"\n",
                3,
                "not a key of `[[check]]`",
            ),
            ("version = 1\nbridge = yes\n", 2, "`true` or `false`"),
            (
                "version = 1\nname = \"a\" oops\n",
                2,
                "nothing but a comment",
            ),
            ("version = 1\nname\n", 2, "`key = value`"),
            ("version = 1\nname = \n", 2, "a value after the `=`"),
            ("version = 1\n\"name\" = \"a\"\n", 2, "is not a key"),
            ("version = 1\nname = \"\\q\"\n", 2, "is not an escape"),
            ("version = 1\nname = \"\\uZZZZ\"\n", 2, "four hex digits"),
            ("version = 1\nname = \"a\u{1}b\"\n", 2, "control character"),
            ("version = 1\n[settings\n", 2, "a closing `]`"),
        ] {
            let e = parse(text).expect_err(text);
            assert_eq!(e.line, Some(line), "{text:?} -> {e}");
            assert!(e.message.contains(wanted), "{text:?} -> {e}");
            assert_eq!(e.unknown_version, None, "{text:?}");
            assert!(format!("{e}").starts_with(&format!("line {line}: ")), "{e}");
        }
    }

    /// A `[[check]]` missing a key is refused at its own header, which is
    /// where somebody looking for the missing key will look.
    #[test]
    fn an_incomplete_check_is_refused_at_its_header() {
        let text = "version = 1\n\
                    \n\
                    [[check]]\n\
                    format = \"binds-dir\"\n\
                    path = \"a/b\"\n\
                    setting = \"X\"\n\
                    wants = \"1\"\n";
        let e = parse(text).expect_err("no tell");
        assert_eq!(e.line, Some(3));
        assert!(e.message.contains("`tell`"), "{e}");
    }

    /// A profile may be partial: nobody has verified a game yet, so a profile
    /// with settings and no checks is the shape day one actually has.
    #[test]
    fn a_partial_profile_is_valid() {
        let settings_only = parse("version = 1\n[settings]\nenabled = true\n").expect("ok");
        assert!(settings_only.checks.is_empty());
        assert_eq!(settings_only.bridge, Bridge::Unstated);
        assert!(!settings_only.is_empty());

        let checks_only = parse(
            "version = 1\n[[check]]\nformat = \"binds-dir\"\npath = \"a\"\n\
             setting = \"X\"\nwants = \"1\"\ntell = \"do it\"\n",
        )
        .expect("ok");
        assert!(checks_only.settings.is_empty());
        assert_eq!(checks_only.name, None);

        let nothing = parse("version = 1\n").expect("ok");
        assert!(nothing.is_empty());
    }

    /// A profile that does not mention the bridge has not said the game does
    /// without one — `Unstated` is not `NotNeeded`.
    #[test]
    fn an_unmentioned_bridge_is_not_a_no() {
        assert_eq!(parse("version = 1\n").unwrap().bridge, Bridge::Unstated);
        assert_eq!(
            parse("version = 1\nbridge = false\n").unwrap().bridge,
            Bridge::NotNeeded
        );
        assert_ne!(Bridge::Unstated, Bridge::NotNeeded);
    }

    // ------------------------------------------------------- the two axes

    /// A version this build does not read is refused, and is refused as a
    /// *version* — so the sentence can be "update tobii-linux" rather than
    /// "your file is broken".
    #[test]
    fn a_newer_version_is_refused_as_a_version() {
        let e =
            parse("version = 2\nwhatever_a_version_2_profile_holds = 1\n").expect_err("version 2");
        assert_eq!(e.unknown_version, Some(2));
        assert_eq!(e.line, Some(1));
        assert!(e.message.contains("version 2"), "{e}");

        for bad in ["version = 0\n", "version = 99\n"] {
            assert!(
                parse(bad).expect_err(bad).unknown_version.is_some(),
                "{bad}"
            );
        }
        // Not a version at all is a different refusal: nothing to update to.
        for bad in ["version = \"1\"\n", "version = x\n", "version = 1.5\n"] {
            let e = parse(bad).expect_err(bad);
            assert_eq!(e.unknown_version, None, "{bad}");
            assert!(e.message.contains("whole number"), "{bad} -> {e}");
        }
    }

    /// The version is read before the body, so a newer profile is declined by
    /// version even when its body is full of keys this build would refuse.
    #[test]
    fn the_version_is_read_before_anything_it_governs() {
        let e = parse("[settings]\nenabled = true\nversion = 1\n").expect_err("late version");
        assert_eq!(e.line, Some(1));
        assert!(e.message.contains("starts with `version = 1`"), "{e}");

        let e = parse("# just a comment\n").expect_err("no version");
        assert_eq!(e.line, None);
        assert!(e.message.contains("no `version` key"), "{e}");
        assert_eq!(format!("{e}"), e.message);
    }

    /// A format this build cannot read is not an error: it parses, it is
    /// carried through by name, and every other check in the file still works.
    /// A third format added later must not invalidate a profile's checks
    /// against the two that already worked.
    #[test]
    fn an_unknown_format_is_carried_not_refused() {
        let text = "version = 1\n\
                    [[check]]\nformat = \"from-a-newer-build\"\npath = \"a\"\n\
                    setting = \"X\"\nwants = \"1\"\ntell = \"go look\"\n\
                    [[check]]\nformat = \"binds-dir\"\npath = \"b\"\n\
                    setting = \"Y\"\nwants = \"2\"\ntell = \"and this\"\n";
        let p = parse(text).expect("parses");
        assert_eq!(
            p.checks[0].format,
            Format::Unknown("from-a-newer-build".into())
        );
        assert!(!p.checks[0].format.is_known());
        assert!(p.checks[1].format.is_known());
        assert_eq!(p.unknown_formats(), vec!["from-a-newer-build"]);
        // And it survives being written back out by a build that never knew it.
        assert_eq!(parse(&p.to_toml()), Ok(p));
    }

    // ------------------------------------------------------------ check paths

    /// A check's path is joined onto a prefix and then opened, so the parser
    /// is where `..` and the rest stop.
    #[test]
    fn a_check_path_may_not_leave_the_prefix() {
        let one = |path: &str| {
            format!(
                "version = 1\n[[check]]\nformat = \"binds-dir\"\npath = {path}\n\
                 setting = \"X\"\nwants = \"1\"\ntell = \"t\"\n"
            )
        };
        for (path, wanted) in [
            ("\"/etc/passwd\"", "does not start with `/`"),
            ("\"a/../../../etc\"", "`..`"),
            ("\"..\"", "`..`"),
            ("\"drive_c\\\\users\"", "uses `/`"),
            ("\"\"", "names a place"),
        ] {
            let e = parse(&one(path)).expect_err(path);
            assert_eq!(e.line, Some(4), "{path}");
            assert!(e.message.contains(wanted), "{path} -> {e}");
        }
        // `..` as part of a name is not a parent directory.
        let ok = parse(&one("\"a/..b/c\"")).expect("a name that starts with dots");
        assert_eq!(ok.checks[0].path, "a/..b/c");
    }

    #[test]
    fn a_check_path_joins_under_the_prefix() {
        let c = &parse(
            "version = 1\n[[check]]\nformat = \"binds-dir\"\npath = \"a/b c/d\"\n\
             setting = \"X\"\nwants = \"1\"\ntell = \"t\"\n",
        )
        .unwrap()
        .checks[0];
        assert_eq!(
            c.path_under(Path::new("/pfx")),
            PathBuf::from("/pfx/a/b c/d")
        );
    }

    // ---------------------------------------------------------------- naming

    #[test]
    fn an_appid_is_a_canonical_number() {
        for good in ["1", "999", "1234567", "4294967295"] {
            assert!(is_appid(good), "{good}");
            assert!(is_profile_file(&format!("{good}.toml")), "{good}");
            assert!(is_profile_file(&format!("{good}.toml.tmp")), "{good}");
        }
        for bad in [
            "",
            "0",
            "0999",
            "12345678901",
            "48a",
            " 999",
            "999 ",
            "-999",
            "4.8",
            "../999",
        ] {
            assert!(!is_appid(bad), "{bad}");
        }
        for bad in ["999", "999.toml~", "notes.txt", ".toml", "999.TOML", "toml"] {
            assert!(!is_profile_file(bad), "{bad}");
        }
    }

    #[test]
    fn a_profile_is_named_for_its_game_and_nothing_else() {
        let p = path_in(Path::new("/p"), FAKE);
        assert_eq!(p, PathBuf::from("/p/1234567.toml"));
        // The app id is the name, not a field: it is in no profile this crate
        // writes, so a file cannot disagree with itself about which game it is.
        assert!(!full().to_toml().contains(FAKE));
    }

    // --------------------------------------------------------------- loading

    #[test]
    fn a_user_profile_outranks_a_builtin_one_whole() {
        let dir = scratch("tiers");
        let builtin: &[(&str, &str)] = &[(
            FAKE,
            "version = 1\nname = \"built in\"\n[settings]\nenabled = true\n",
        )];

        let got = load_from(&dir, builtin, FAKE).expect("ok").expect("found");
        assert_eq!(got.origin, Origin::Builtin);
        assert_eq!(got.profile.name.as_deref(), Some("built in"));

        // The user's file replaces it whole: the built-in profile's settings
        // are gone, not merged, so a user can take away a wrong one.
        std::fs::write(path_in(&dir, FAKE), "version = 1\nname = \"mine\"\n").expect("write");
        let got = load_from(&dir, builtin, FAKE).expect("ok").expect("found");
        assert_eq!(got.origin, Origin::File(path_in(&dir, FAKE)));
        assert_eq!(got.profile.name.as_deref(), Some("mine"));
        assert!(
            got.profile.settings.is_empty(),
            "merged instead of replaced"
        );

        let listing = list_from(&dir, builtin);
        assert_eq!(listing.profiles.len(), 1, "one game, one profile");
        assert_eq!(listing.profiles[0].0, FAKE);
        assert_eq!(listing.profiles[0].1.profile.name.as_deref(), Some("mine"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Absent and unreadable are different answers. A profile that is there
    /// and cannot be read must never fall through to the built-in one, or a
    /// user's own edit silently does nothing.
    #[test]
    fn a_profile_that_cannot_be_read_is_not_a_profile_that_is_absent() {
        let dir = scratch("unreadable");
        let builtin: &[(&str, &str)] = &[(FAKE, "version = 1\nname = \"built in\"\n")];

        assert_eq!(load_from(&dir, &[], FAKE), Ok(None), "absent is Ok(None)");

        // A directory where the file should be. Chosen because CI runs these
        // as root, where a mode-000 file is still readable and would prove
        // nothing.
        std::fs::create_dir(path_in(&dir, FAKE)).expect("mkdir");
        match load_from(&dir, builtin, FAKE) {
            Err(LoadError::Unreadable { origin, .. }) => {
                assert_eq!(origin, Origin::File(path_in(&dir, FAKE)))
            }
            other => panic!("{other:?}"),
        }

        // And a malformed one is its own answer, also not a fall-through.
        std::fs::remove_dir(path_in(&dir, FAKE)).expect("rmdir");
        std::fs::write(path_in(&dir, FAKE), "version = 1\nnope = 1\n").expect("write");
        match load_from(&dir, builtin, FAKE) {
            Err(LoadError::Malformed { error, .. }) => assert_eq!(error.line, Some(2)),
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn asking_about_something_that_is_not_an_appid_is_an_error() {
        let dir = scratch("notanappid");
        for bad in ["", "Elite Dangerous", "../config"] {
            assert_eq!(
                load_from(&dir, &[], bad),
                Err(LoadError::NotAnAppId(bad.to_string())),
                "{bad}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A listing says what it could not use and what was not a profile, in
    /// app id order — which for these names is numeric order.
    #[test]
    fn a_listing_reports_what_it_skipped() {
        let dir = scratch("listing");
        std::fs::write(path_in(&dir, "900"), "version = 1\n").expect("write");
        std::fs::write(path_in(&dir, OTHER), "version = 1\n").expect("write");
        std::fs::write(path_in(&dir, FAKE), "version = 1\nbroken\n").expect("write");
        std::fs::write(dir.join("notes.txt"), "hello").expect("write");
        std::fs::write(dir.join("999.toml~"), "an editor backup").expect("write");

        let l = list_from(&dir, &[]);
        assert_eq!(
            l.profiles
                .iter()
                .map(|(a, _)| a.as_str())
                .collect::<Vec<_>>(),
            vec!["900", OTHER],
            "numeric order, and the broken one is not a profile"
        );
        assert_eq!(l.problems.len(), 1);
        assert!(
            matches!(&l.problems[0], LoadError::Malformed { error, .. } if error.line == Some(2)),
            "{:?}",
            l.problems[0]
        );
        assert_eq!(
            l.strays,
            vec!["999.toml~".to_string(), "notes.txt".to_string()]
        );

        // A directory nobody has written profiles into is not a problem.
        let empty = scratch("listing-absent");
        std::fs::remove_dir_all(&empty).ok();
        assert_eq!(list_from(&empty, &[]), Listing::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// This build ships no per-game knowledge — and the loader that will one
    /// day carry some still has to answer for a table that is wrong.
    #[test]
    fn nothing_is_shipped_and_a_bad_builtin_is_still_reported() {
        assert!(BUILTIN.is_empty(), "this build ships no game profiles");

        let dir = scratch("builtin");
        let bad: &[(&str, &str)] = &[(FAKE, "version = 1\nnot_a_key = 1\n")];
        match load_from(&dir, bad, FAKE) {
            Err(LoadError::Malformed { origin, error }) => {
                assert_eq!(origin, Origin::Builtin);
                assert_eq!(error.line, Some(2));
                let shown = format!("{}", LoadError::Malformed { origin, error });
                assert!(
                    shown.starts_with("built into tobii-linux: line 2: "),
                    "{shown}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(list_from(&dir, bad).problems.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    // --------------------------------------------------------------- writing

    #[test]
    fn what_is_written_is_read_back() {
        let dir = scratch("save");
        save_to(&dir, FAKE, &full()).expect("save");
        let got = load_from(&dir, &[], FAKE).expect("ok").expect("found");
        assert_eq!(got.profile, full());
        assert_eq!(got.origin, Origin::File(path_in(&dir, FAKE)));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An app id that is not one never becomes a file name: `path_in` would
    /// happily build `<dir>/../../anything.toml` out of it.
    #[test]
    fn saving_under_something_that_is_not_an_appid_writes_nothing() {
        let dir = scratch("save-bad");
        let e = save_to(&dir, "../escape", &full()).expect_err("refused");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read_dir(&dir).expect("list").count(),
            0,
            "nothing was written"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
