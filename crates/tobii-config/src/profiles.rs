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
//! # A check is resolved under the prefix — which is not the same as contained
//!
//! [`Check::path`] is relative to the prefix — the directory holding
//! `drive_c` — and [`parse`] refuses a leading `/`, a `..` component and a
//! backslash. Those three keep the path *relative*: that is what makes a
//! profile portable between two machines whose prefixes sit in different
//! places, and it is what lets [`Check::path_under`] join without producing
//! nonsense. They are a shape rule, and this module used to describe them as
//! a containment boundary. **They are not one, and the difference is the
//! whole of this section.**
//!
//! A prefix is a *Wine* prefix, and every Wine prefix ships the way out.
//! `dosdevices/` maps drive letters onto the rest of the machine: `z:` is `/`
//! in every prefix wine ever made, and Steam adds an `s:` pointing at the
//! Steam library root. Those are ordinary directory entries under the prefix,
//! so `dosdevices/z:/etc/hostname` has no `..`, no leading `/` and no
//! backslash, [`parse`] accepts it, and the reader it names really does open
//! that file. Measured on this project's own Elite prefix, not reasoned
//! about; `a_check_path_reaches_what_the_prefix_reaches` in this file pins it
//! against a fixture.
//!
//! So the honest sentence is: **a check reaches what the prefix reaches**,
//! and a Wine prefix reaches the machine. That is deliberate rather than
//! merely tolerated — a game's *shipped* files are under the Steam library,
//! in `steamapps/common/<game>/`, and naming them is a thing a profile author
//! legitimately wants. Elite Dangerous keeps its thirty stock control schemes
//! there, in `Products/elite-dangerous-odyssey-64/ControlSchemes/`, and
//! refusing `dosdevices` would remove that capability to preserve a sentence.
//!
//! Be exact about which half of that has been run, because an earlier draft of
//! this paragraph was not. **What this module does was measured**: on
//! 2026-09-28, on the maintainer's install, a check with
//! `path = "dosdevices/s:/steamapps/common/Elite Dangerous/Products/elite-dangerous-odyssey-64/ControlSchemes"`
//! parsed, and `tobii games profile check where` resolved it onto that real
//! directory and reported *a directory, as binds-dir needs*. **Whether a
//! reader then answers out of it is `tobii_gameconf`'s question, not this
//! module's**, and the thirty-document count this paragraph cites was taken by
//! that crate's `read` example in its `presets` mode, pointed straight at the
//! directory — not by a check, and not through a profile. This module's claim
//! stops at the path.
//!
//! # What actually bounds what a profile can do
//!
//! Not the path. The readers.
//!
//! A `[[check]]` names a `format`, and the only two formats this build has
//! readers for — `tobii_gameconf`'s `binds` and `attrs` — **only ever read**,
//! open one file the check names, look up the one `setting` the check names,
//! and answer with that one value or with a refusal. There is no grammar for
//! writing, no grammar for listing a directory the check did not name, and no
//! path by which a check returns a file's contents.
//!
//! State that plainly to whoever is about to trust a profile from a forum:
//! **a profile from a stranger can make this program read a file you can
//! already read and tell you what one attribute of it says.** Not more than
//! that — and not less, so a profile naming `dosdevices/z:/` somewhere
//! personal is worth a second look before you run it, the same second look
//! any hand-edited file from a stranger is worth.
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
//! More than one screen in this program says that sentence to a user. Each
//! of them asks [`ships_profiles`] rather than carrying a `false` somebody
//! typed: the day the first profile lands, a hardcoded sentence becomes
//! untrue and nothing fails.
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
//! # Writing a profile back does not lose what somebody wrote in it
//!
//! A profile is hand-edited, and the useful half of a hand-edited file is
//! often the half a parser throws away. *Measured 2026-10-01, verified on
//! Odyssey 4.0* is a comment or it is nowhere — there is no field for it,
//! and a field would be the wrong shape for it if there were.
//!
//! So [`save_to`] does not write [`Profile::to_toml`] over a file that is
//! already there. It reads that file first and writes
//! [`Profile::to_toml_over`], which puts every `#` line back beside the
//! thing it was beside — above the key, table or check it sat above, at the
//! end of that same line, or below it, which is where the comments at the
//! end of a file are.
//!
//! What a comment is beside is that *thing*, and not the slot it sat in. A
//! check is found again by what it names — its `format`, `path` and
//! `setting`, the same three fields `tobii games profile check add` already
//! treats as one check being the same as another. That is what a removal
//! turns on. Anchored by position, every note below a check that went would
//! come back one check higher: still in the file, still reading as somebody's
//! provenance, now above a check they never looked at, and never reported,
//! because a slot that is still occupied looks like a comment that found its
//! home. Anchored by what it names, each note either finds its own check or
//! finds nothing.
//!
//! Finding nothing is the honest case, and it does not stop the write.
//! Removing a check is what `tobii games profile check remove` is for, and
//! the checks worth removing are exactly the ones somebody wrote a comment
//! above. Such a comment comes back out of [`save_to`] as an [`Orphan`] — its
//! line, its text, and what it sat beside — and the caller shows it: the file
//! does not hold that sentence any more, so the report is the only place it
//! now exists. What is never done is putting it somewhere else in the file.
//!
//! A file this build cannot *read* is the one case that still stops the write
//! whole, as [`Unreadable`]. Nothing in it can be located, so nothing in it
//! can be put back — and this build cannot say what any of it was about
//! either, so it cannot even hand it over honestly.
//!
//! Blank lines are the one thing that is not kept. A comment comes back
//! attached to its anchor, not to the spacing around it.
//!
//! # What is in the profiles directory, in one answer
//!
//! [`is_profile_file`] is the only predicate over a name in
//! [`profiles_dir`], and it answers one question: **did this program write
//! it?** Two names pass — `<appid>.toml`, a profile, and `<appid>.toml.tmp`,
//! what an interrupted [`crate::write_atomic`] leaves beside one. `tobii
//! uninstall --purge` deletes exactly what it accepts, which is why the
//! temporary has to be in it: a leftover of this program's own write is this
//! program's to clean up, not somebody's file to report and leave.
//!
//! [`list_from`] asks that same predicate, so the two cannot disagree about
//! one name. A temporary is not a profile and is not a stray either; it is
//! [`Listing::leftovers`]. Filing it under strays told a user that a file
//! this program wrote was *not written by this program* — the same untruth
//! `--purge` exists to avoid, one level down.
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

/// Whether this build has a profile for any game compiled in.
///
/// The one place that answers it. Several screens tell a user that this
/// build ships none; each of them asks here rather than carrying the answer
/// as a `false` in its own sentence, because the day [`BUILTIN`] grows an
/// entry every one of those sentences is wrong at once and a hardcoded one
/// would go on being printed.
pub const fn ships_profiles() -> bool {
    !BUILTIN.is_empty()
}

/// What this build ships, as the object of the sentence "this build ships …".
///
/// The one phrase, so that the several screens which say it say the same
/// thing and change together. Each asks for it rather than typing "no profile
/// for any game", because the day [`BUILTIN`] grows an entry every typed copy
/// is wrong at once, and a typed copy goes on being printed.
pub fn shipped_profiles() -> String {
    shipped_phrase(BUILTIN.len())
}

/// [`shipped_profiles`] over a count, so that the wording for the counts this
/// build cannot yet have is still something a test can reach.
fn shipped_phrase(n: usize) -> String {
    match n {
        0 => "no profile for any game".to_string(),
        1 => "a profile for one game".to_string(),
        n => format!("profiles for {n} games"),
    }
}

// ------------------------------------------------------------------- the type

/// Which file format a [`Check`] reads, and therefore which reader answers it.
///
/// The names are the format's, not any game's: this crate ships no game
/// knowledge, and a profile for a game nobody here has heard of should be
/// able to name a format without the format having to be renamed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Format {
    /// A directory of preset documents — `tobii_gameconf::binds`.
    ///
    /// A `StartPreset` file there names the live preset and that one is read.
    /// With no such file the directory holds presets a game ships and nothing
    /// has selected one, so every document is read and the answer says so.
    /// Which shape a directory is depends on whether the person running the
    /// profile has ever launched the game, which its author cannot know — so
    /// one format answers both rather than making the author guess.
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
    /// Always relative, always `/`-separated, and never with a `..`
    /// component: [`parse`] refuses the rest at the line that held it. Those
    /// three keep the path relative, which is what makes a profile portable
    /// between machines whose prefixes live in different places and what lets
    /// [`Self::path_under`] join it onto a prefix at all. A backslash is
    /// refused for a plainer reason still — it is a literal character in a
    /// Linux path, so a Windows-style path would not fail, it would silently
    /// name nothing.
    ///
    /// **They are not a containment boundary, and this doc comment used to
    /// say they were.** The prefix is the only *root*, but a Wine prefix maps
    /// the rest of the machine into itself under `dosdevices/` — `z:` is `/`
    /// and, on a Steam prefix, `s:` is the library root — so a path with no
    /// `..` in it can still name a game's shipped files under
    /// `steamapps/common/`, or anything else this user can read. That is a
    /// capability profile authors want; see the module docs for what does
    /// bound a check, which is that its reader only ever reads, and only the
    /// one [`Self::setting`] it was asked for.
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
    /// Well-defined to join because [`Self::path`] is relative and has no
    /// `..`; that is established when the profile is parsed, so it cannot be
    /// re-litigated at every call site.
    ///
    /// The result is lexically under `prefix` — it is `prefix` with relative
    /// components pushed onto it, and nothing here canonicalises. Where it
    /// *resolves* is another question: a Proton prefix contains
    /// `dosdevices/z: -> /`, so a caller opening this path may well open a
    /// file outside the prefix. That is intended; see the module docs.
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

    /// This profile as a profile file, with no comments in it.
    ///
    /// Round-trips: [`parse`] of this text gives back an equal `Profile`, for
    /// every string any field can hold — see `a_profile_survives_a_round_trip`
    /// in the tests below.
    ///
    /// This is what to write where there is no file yet. Over a file somebody
    /// already has, use [`Self::to_toml_over`]: this one carries no comments,
    /// so writing it over a file that has some destroys them.
    ///
    /// Every value goes out quoted, including a setting that reads as a number
    /// or a boolean. A hand-written `enabled = true` is accepted — see
    /// [`parse`] — but what a setting *is* is text on its way to
    /// `OutputConfig::apply_key`, and a writer that sometimes quoted and
    /// sometimes did not would have to decide which, on a value it is not
    /// entitled to have an opinion about.
    ///
    /// So the two spellings are not two formats and there is nothing to choose
    /// between them: `enabled = true` is what a hand-written file may say,
    /// `enabled = "true"` is what this writes, and both read back as the text
    /// `true`. An example of a profile *as this program writes one* quotes
    /// every value; an example of one somebody typed need not.
    pub fn to_toml(&self) -> String {
        self.render(&Comments::default()).0
    }

    /// This profile as a profile file, keeping the comments of `existing` —
    /// the text of the file it is about to replace.
    ///
    /// Every `#` line of `existing` comes out again beside the thing it was
    /// beside: above the key, table or `[[check]]` it sat above, at the end of
    /// that same line, or below it. Nothing here reorders a profile, and a
    /// `[[check]]` is matched by what it names rather than by where it sat, so
    /// a check that keeps its `format`, `path` and `setting` keeps its
    /// comments however the checks around it change.
    ///
    /// Blank lines are not kept: a comment comes back attached to its anchor,
    /// not to the spacing around it.
    ///
    /// A comment whose anchor this profile does not have is **not** in the
    /// text, and this call does not say which one it was: it is the text and
    /// nothing else. To write a profile, go through [`save_to`], which hands
    /// back every comment it could not put back — the only copy of them
    /// left. [`Err`] is the one case where there is nothing honest to hand
    /// back at all: see [`Unreadable`].
    pub fn to_toml_over(&self, existing: &str) -> Result<String, Unreadable> {
        Ok(self.rewrite_over(existing)?.0)
    }

    /// [`Self::to_toml_over`] with the comments it had nowhere to put.
    ///
    /// The one place a profile is written over another, and so the one place
    /// that can say what writing it costs.
    fn rewrite_over(&self, existing: &str) -> Result<(String, Vec<Orphan>), Unreadable> {
        let Ok(was) = parse(existing) else {
            // Nothing in a file this build cannot read can be located, so no
            // comment in it can be put back. Refuse if there is one to lose.
            for (i, raw) in existing.lines().enumerate() {
                if let (_, Some(c)) = split_comment(raw) {
                    return Err(Unreadable {
                        line: i + 1,
                        comment: c.trim_end().to_string(),
                    });
                }
            }
            return Ok((self.to_toml(), Vec::new()));
        };
        // `was` is `existing`'s own parse, which is what lets a comment above
        // the n-th `[[check]]` be recorded as being about the check that is
        // there rather than about the n-th slot.
        Ok(self.render(&comments_of(existing, &was.checks)))
    }

    /// Every line this profile is, each with the thing a comment could be
    /// attached to. The order is the file's order, and is the whole reason a
    /// comment's position is stable.
    fn lines(&self) -> Vec<(Anchor, String)> {
        let mut v = vec![(
            Anchor::Top("version".into()),
            format!("version = {VERSION}"),
        )];
        if let Some(n) = &self.name {
            v.push((Anchor::Top("name".into()), format!("name = {}", quote(n))));
        }
        match self.bridge {
            Bridge::Unstated => {}
            Bridge::Required => v.push((Anchor::Top("bridge".into()), "bridge = true".into())),
            Bridge::NotNeeded => v.push((Anchor::Top("bridge".into()), "bridge = false".into())),
        }
        if !self.settings.is_empty() {
            v.push((Anchor::Settings, "[settings]".into()));
            for (k, val) in &self.settings {
                v.push((Anchor::Setting(k.clone()), format!("{k} = {}", quote(val))));
            }
        }
        for c in &self.checks {
            let id = CheckId::of(c);
            v.push((Anchor::Check(id.clone()), "[[check]]".into()));
            for (k, val) in [
                ("format", c.format.as_str()),
                ("path", c.path.as_str()),
                ("setting", c.setting.as_str()),
                ("wants", c.wants.as_str()),
                ("tell", c.tell.as_str()),
            ] {
                v.push((
                    Anchor::CheckKey(id.clone(), k.into()),
                    format!("{k} = {}", quote(val)),
                ));
            }
        }
        v
    }

    /// This profile written out with `kept` put back where it came from, and
    /// whatever `kept` held that this profile has nowhere to put.
    fn render(&self, kept: &Comments) -> (String, Vec<Orphan>) {
        let mut used = vec![false; kept.at.len()];
        let mut s = String::from(HEADER);
        s.push('\n');
        for (anchor, line) in self.lines() {
            // The blank line goes before the comment block, so a note written
            // above a `[[check]]` comes back above it and not above the gap.
            if matches!(anchor, Anchor::Settings | Anchor::Check(_)) {
                s.push('\n');
            }
            // The first match not already spoken for. Nothing in the grammar
            // stops a hand-written file naming one thing in two `[[check]]`
            // blocks — only `check add` does — and then the second one's
            // comments are its own, not a second copy of the first one's.
            let hit = kept
                .at
                .iter()
                .enumerate()
                .find(|(i, a)| !used[*i] && a.anchor == anchor)
                .map(|(i, _)| i);
            if let Some(i) = hit {
                used[i] = true;
                for (_, c) in &kept.at[i].leading {
                    // The header is written above, so a file this program
                    // wrote does not grow a second one on every save.
                    if c != HEADER {
                        s.push_str(c);
                        s.push('\n');
                    }
                }
            }
            s.push_str(&line);
            if let Some(t) = hit.and_then(|i| kept.at[i].trailing.as_deref()) {
                s.push(' ');
                s.push_str(t);
            }
            s.push('\n');
            if let Some(i) = hit {
                for (_, c) in &kept.at[i].following {
                    s.push_str(c);
                    s.push('\n');
                }
            }
        }
        // In file order: `at` is in file order, and within one anchor what was
        // above it comes before what was on its line, which comes before what
        // was below it.
        let mut orphans = Vec::new();
        for (i, a) in kept.at.iter().enumerate() {
            if used[i] {
                continue;
            }
            let about = a.anchor.describe();
            let mut lost = |line: usize, comment: &str| {
                orphans.push(Orphan {
                    line,
                    comment: comment.to_string(),
                    about: about.clone(),
                });
            };
            for (line, c) in &a.leading {
                lost(*line, c);
            }
            if let Some(c) = &a.trailing {
                lost(a.line, c);
            }
            for (line, c) in &a.following {
                lost(*line, c);
            }
        }
        (s, orphans)
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
    /// Names in the directory that this program did not write and that are
    /// not profiles, sorted.
    ///
    /// Separate from [`Self::problems`]: an editor's `<appid>.toml~` is not a
    /// broken profile, it is somebody's backup, and `--purge` will say the
    /// same thing about it. A caller may reasonably print these more quietly,
    /// or not at all.
    ///
    /// Judged by [`is_profile_file`], the same predicate `--purge` deletes
    /// by, so nothing this program writes can land here. What this program
    /// wrote and is not a profile goes in [`Self::leftovers`].
    pub strays: Vec<String>,
    /// `<appid>.toml.tmp`: what an interrupted [`crate::write_atomic`] left
    /// behind, sorted.
    ///
    /// Not a profile, and not somebody else's file either — this program
    /// wrote it, `--purge` deletes it, and a report that called it a stray
    /// would be telling a user the opposite. Usually empty; a name here means
    /// a save was cut short.
    pub leftovers: Vec<String>,
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
            // One predicate decides who wrote a name here, and it is the one
            // `--purge` deletes by. A `.tmp` of ours is not a profile, but
            // calling it a stray would say this program did not write it.
            if is_profile_file(&name) {
                out.leftovers.push(name);
            } else {
                out.strays.push(name);
            }
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
    l.leftovers.sort();
    l
}

/// List this user's profiles, with [`BUILTIN`] behind them.
pub fn list() -> Listing {
    list_from(&profiles_dir(), BUILTIN)
}

// ------------------------------------------------------------------ comments

/// The line every profile this program writes starts with.
const HEADER: &str = "# tobii-linux game profile";

/// What a comment in a profile file sits beside.
///
/// An identity, not a position. Nothing here reorders a profile, so for the
/// keys a name is enough; for a `[[check]]` it is not, because a removal
/// renumbers every check below it and a note anchored to a number would come
/// back above whichever check inherited the number.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Anchor {
    /// A key before any table: `version`, `name`, `bridge`.
    Top(String),
    /// The `[settings]` header.
    Settings,
    /// A key under `[settings]`.
    Setting(String),
    /// A `[[check]]` header.
    Check(CheckId),
    /// A key of a `[[check]]`.
    CheckKey(CheckId, String),
}

impl Anchor {
    /// What to call this in the sentence a person reads when their comment
    /// has nowhere to go.
    fn describe(&self) -> String {
        match self {
            Anchor::Top(k) => format!("`{k}`"),
            Anchor::Settings => "`[settings]`".to_string(),
            Anchor::Setting(k) => format!("the setting `{k}`"),
            Anchor::Check(id) => id.describe(),
            Anchor::CheckKey(id, k) => format!("`{k}` of {}", id.describe()),
        }
    }
}

/// Which `[[check]]` a comment is about: the three fields that say what a
/// check reads, and so which check it is.
///
/// Not `wants` and not `tell`. Correcting the value a game should have, or
/// the sentence shown to whoever has to set it, leaves it the same check, and
/// a note above it is still a note about it. Changing what it reads makes it
/// a different claim, and a note above it stops being about the check that is
/// there now — which is a comment to hand back, not one to keep silently.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CheckId {
    format: String,
    path: String,
    setting: String,
}

impl CheckId {
    fn of(c: &Check) -> CheckId {
        CheckId {
            format: c.format.as_str().to_string(),
            path: c.path.clone(),
            setting: c.setting.clone(),
        }
    }

    /// What to call this check in the sentence a person reads. What it reads,
    /// not where it sat: a comment is handed back because a check went, and
    /// after that neither file's numbering is the one they are looking at.
    fn describe(&self) -> String {
        format!(
            "the check that reads `{}` out of `{}`",
            self.setting, self.path
        )
    }
}

/// One thing in a file that has a comment attached to it.
///
/// Only things that do: a profile is mostly lines nobody wrote a note about,
/// and carrying those would make "which comments went nowhere" a search
/// rather than a look.
#[derive(Debug)]
struct Attached {
    anchor: Anchor,
    /// The 1-based line the anchor's own text is on.
    line: usize,
    /// Whole-line comments immediately above it, with their lines.
    leading: Vec<(usize, String)>,
    /// The comment at the end of the anchor's own line.
    trailing: Option<String>,
    /// Whole-line comments below it with nothing but the end of the file
    /// after them, with their lines. Only the last anchor in a file can have
    /// any: a comment with another line under it is that line's `leading`.
    following: Vec<(usize, String)>,
}

/// Every comment in one profile file, by what it sits beside.
#[derive(Debug, Default)]
struct Comments {
    /// In file order, which is why orphans come back in file order too.
    at: Vec<Attached>,
}

/// One line split into what it says and the comment on the end of it.
///
/// The `#` has to be found outside the strings — `tell = "press # twice"` is
/// one value and no comment. The parser settles that a value at a time, in
/// [`unquote`], which reports where a string ended; this asks it of a whole
/// line at once, for a line whose value it does not otherwise need.
///
/// Byte-wise is safe: every byte compared against is ASCII, and no byte of a
/// multi-byte character can equal one, so a `#` this stops on is always a
/// character boundary.
fn split_comment(raw: &str) -> (&str, Option<&str>) {
    let b = raw.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    while i < b.len() {
        match b[i] {
            b'"' => {
                in_string = !in_string;
                i += 1;
            }
            b'\\' if in_string => i += 2,
            b'#' if !in_string => return (&raw[..i], Some(&raw[i..])),
            _ => i += 1,
        }
    }
    (raw, None)
}

/// Every comment in `text`, attached to what it is about.
///
/// `text` must be something [`parse`] accepted and `checks` must be what it
/// parsed to: this walks the same shapes and takes each line to be a table
/// header or a `key = value`, which is what having parsed guarantees, and it
/// names the n-th `[[check]]` by what the n-th check holds.
fn comments_of(text: &str, checks: &[Check]) -> Comments {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = Comments::default();
    let mut pending: Vec<(usize, String)> = Vec::new();
    let mut table = Table::Top;
    let mut seen = 0usize;
    let mut id = CheckId::default();
    let mut last: Option<(Anchor, usize)> = None;
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let (content, comment) = split_comment(raw);
        let comment = comment.map(|c| c.trim_end().to_string());
        let t = content.trim();
        if t.is_empty() {
            if let Some(c) = comment {
                pending.push((line, c));
            }
            continue;
        }
        let anchor = if t.starts_with("[[") {
            table = Table::Check;
            // `checks` is this text's own parse, so there is one entry per
            // header. The default is reachable only by breaking that, and it
            // is an id no profile can write out — every check has a `format`
            // — so its comments would come back as orphans rather than land
            // on some other check.
            id = checks.get(seen).map(CheckId::of).unwrap_or_default();
            seen += 1;
            Anchor::Check(id.clone())
        } else if t.starts_with('[') {
            table = Table::Settings;
            Anchor::Settings
        } else {
            let key = t.split_once('=').map_or(t, |(k, _)| k.trim()).to_string();
            match table {
                Table::Top => Anchor::Top(key),
                Table::Settings => Anchor::Setting(key),
                // `id` is set: a parsed file cannot be inside a `[[check]]`
                // without having had its header.
                Table::Check => Anchor::CheckKey(id.clone(), key),
            }
        };
        let leading = std::mem::take(&mut pending);
        if !leading.is_empty() || comment.is_some() {
            out.at.push(Attached {
                anchor: anchor.clone(),
                line,
                leading,
                trailing: comment,
                following: Vec::new(),
            });
        }
        last = Some((anchor, line));
    }
    // What is left is the comments at the end of the file, and they are about
    // the last line of it — the thing they sit under. Kept as a list of their
    // own they came back at the end of whatever the *new* file turned out to
    // be, so a note under the last check moved to under a check added after
    // it: silently, and never as an orphan, because a file always has an end.
    //
    // `text` has parsed, so it has a `version` line and `last` is set.
    if let (false, Some((anchor, line))) = (pending.is_empty(), last) {
        match out.at.last_mut() {
            Some(a) if a.line == line => a.following = pending,
            _ => out.at.push(Attached {
                anchor,
                line,
                leading: Vec::new(),
                trailing: None,
                following: pending,
            }),
        }
    }
    out
}

/// A comment in the file being replaced that the new profile has nowhere to
/// put, because the thing it sat beside is not in it.
///
/// Not a refusal: the write happens, and this is the comment itself, handed
/// back for the caller to show. See the module docs for why that is the
/// answer and putting it elsewhere in the file is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orphan {
    /// The 1-based line it was on, in the file being replaced.
    pub line: usize,
    /// The comment as it was written, `#` and all, without trailing space.
    pub comment: String,
    /// What it sat beside, as a fragment to put after "a comment about": *the
    /// setting `x`*, *the check that reads `X` out of `y`*.
    pub about: String,
}

impl fmt::Display for Orphan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Orphan {
            line,
            comment,
            about,
        } = self;
        write!(
            f,
            "line {line} — {comment} — was a comment about {about}, which this profile does not \
             have"
        )
    }
}

/// The comments a write could not put back.
///
/// Nothing else holds these now: a caller that writes the profile and drops
/// this has lost somebody's sentence with nobody told, which is the one
/// outcome the whole comment mechanism exists to prevent. So a caller of
/// [`save_to`] reports them — every one of them, in full, since a count of
/// comments is not a comment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Orphans(Vec<Orphan>);

impl std::ops::Deref for Orphans {
    type Target = [Orphan];

    fn deref(&self) -> &[Orphan] {
        &self.0
    }
}

/// A file that is there and is not one this build can read as a profile, so
/// the write over it did not happen at all.
///
/// The one loss that is still a refusal. Nothing in such a file can be
/// located, so nothing in it can be put back — and nothing in it can be
/// handed back honestly either, because this build cannot say what any of it
/// was about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    /// The 1-based line of the first comment that would be lost.
    pub line: usize,
    /// That comment, `#` and all.
    pub comment: String,
}

impl fmt::Display for Unreadable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Unreadable { line, comment } = self;
        write!(
            f,
            "line {line} is a comment — {comment} — in a file this build cannot read as a \
             profile, so there is nowhere to put it back. Nothing was written: fix the file, or \
             move it aside."
        )
    }
}

impl std::error::Error for Unreadable {}

// ------------------------------------------------------------------- writing

/// Write a profile into `dir`, creating it if need be.
///
/// Atomic, like every other file this program writes: a half-written profile
/// would be refused on the next read, and the user would be told their own
/// file is malformed by a program that malformed it.
///
/// Over a file that is already there this is a read-modify-write, so that the
/// comments in it survive — see [`Profile::to_toml_over`]. The ones it had
/// nowhere to put come back as [`Orphans`], which the caller must show: they
/// are in no file now. A file that is there and cannot be read is the one
/// case that stops the write, as an [`io::ErrorKind::InvalidInput`] carrying
/// [`Unreadable`]'s sentence, rather than that file being overwritten by a
/// program that could not say what was in it.
pub fn save_to(dir: &Path, appid: &str, profile: &Profile) -> io::Result<Orphans> {
    if !is_appid(appid) {
        // Before the path is built, not after: `path_in` would happily make a
        // name out of `../../anything`.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{appid:?} is not a Steam app id"),
        ));
    }
    let path = path_in(dir, appid);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let (body, orphans) = match &text {
        Some(t) => {
            let (text, orphans) = profile.rewrite_over(t).map_err(|loss| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{}: {loss}", path.display()),
                )
            })?;
            (text, Orphans(orphans))
        }
        None => (profile.to_toml(), Orphans::default()),
    };
    crate::write_atomic(&path, body.as_bytes())?;
    Ok(orphans)
}

/// Write a profile into this user's config.
pub fn save(appid: &str, profile: &Profile) -> io::Result<Orphans> {
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
            "`path` is relative to the Proton prefix, so it does not start with `/` — that is \
             what keeps a profile portable between machines. To reach outside the prefix, name \
             a drive under `dosdevices/`: `dosdevices/s:/steamapps/common/...` for the game's \
             own install, `dosdevices/z:/` for an absolute path.",
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
            "`path` may not have a `..` component: it is joined onto a prefix and then opened, \
             and a path that climbs out of it is no longer portable between machines. To reach \
             outside the prefix, name a drive under `dosdevices/`: \
             `dosdevices/s:/steamapps/common/...` for the game's own install, `dosdevices/z:/` \
             for an absolute path.",
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
    ///
    /// This is a *portability* rule, not a containment one — see
    /// `a_check_path_reaches_what_the_prefix_reaches` for what a prefix
    /// actually reaches. What must break for this to fail: `check_path`
    /// accepting one of these five shapes, or blaming the wrong line.
    #[test]
    fn a_check_path_must_stay_relative() {
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

    /// A `path` with no `..`, no leading `/` and no backslash still reaches
    /// outside the prefix, because every Wine prefix maps the machine into
    /// itself under `dosdevices/`: `z:` is `/`, and a Steam prefix adds `s:`
    /// pointing at the library root. Measured on this project's own Elite
    /// prefix; reproduced here against a fixture.
    ///
    /// This is the test the four "the prefix is the only place a check can
    /// reach" sentences did not have. It exists so that claim cannot come
    /// back without something going red — and because reaching
    /// `steamapps/common/` through `s:` is a capability a profile author is
    /// meant to have.
    ///
    /// What must break for this to fail: `check_path` starting to refuse
    /// `dosdevices` or a drive-letter segment, or `path_under` resolving
    /// symlinks instead of joining — either of which takes that capability
    /// away.
    #[test]
    fn a_check_path_reaches_what_the_prefix_reaches() {
        let d = scratch("reaches");
        let prefix = d.join("pfx");
        std::fs::create_dir_all(prefix.join("drive_c")).expect("drive_c");
        std::fs::create_dir_all(prefix.join("dosdevices")).expect("dosdevices");
        let outside = d.join("library");
        std::fs::create_dir_all(&outside).expect("library");
        let body = "<Attributes Headlook=\"1\"/>";
        std::fs::write(outside.join("shipped.xml"), body).expect("write");
        // Exactly what Steam puts in a Proton prefix, and what wine puts in
        // every prefix it makes.
        std::os::unix::fs::symlink(&outside, prefix.join("dosdevices").join("s:")).expect("s:");
        std::os::unix::fs::symlink("/", prefix.join("dosdevices").join("z:")).expect("z:");

        let one = |path: &str| {
            format!(
                "version = 1\n[[check]]\nformat = \"attributes-xml\"\npath = \"{path}\"\n\
                 setting = \"Headlook\"\nwants = \"1\"\ntell = \"t\"\n"
            )
        };

        // Half one: the parser accepts it. No `..`, no leading `/`, no
        // backslash, so the three guards have nothing to say about it.
        let via_s = "dosdevices/s:/shipped.xml";
        let c = parse(&one(via_s))
            .expect("a drive letter is not a `..`")
            .checks[0]
            .clone();

        // Half two: it opens the file outside the prefix. Reading it here is
        // the same act `tobii-gameconf` performs on the path this crate hands
        // it, and the assertion is on the bytes, not on the shape of a path.
        let joined = c.path_under(&prefix);
        assert_eq!(
            std::fs::read_to_string(&joined).expect("the file outside the prefix"),
            body,
            "{via_s} did not reach the fixture standing in for steamapps/common"
        );
        let real = std::fs::canonicalize(&joined).expect("canonicalize");
        let real_prefix = std::fs::canonicalize(&prefix).expect("canonicalize prefix");
        assert!(
            !real.starts_with(&real_prefix),
            "{real:?} was expected to resolve outside {real_prefix:?}"
        );

        // And `z:` is the general form: any absolute path this user can read,
        // spelled without the leading `/`.
        let abs = outside.join("shipped.xml");
        let via_z = format!("dosdevices/z:{}", abs.to_str().expect("utf-8 temp path"));
        let c = parse(&one(&via_z))
            .expect("`z:` is not a `..` either")
            .checks[0]
            .clone();
        assert_eq!(
            std::fs::read_to_string(c.path_under(&prefix)).expect("through z:"),
            body,
            "{via_z} did not reach an absolute path"
        );

        std::fs::remove_dir_all(&d).ok();
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
        assert!(
            !ships_profiles(),
            "the answer every sentence about this asks for"
        );
        assert_eq!(shipped_profiles(), "no profile for any game");

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

    /// The phrase the screens that say what this build ships ask for, at the
    /// counts this build cannot have yet. It is one sentence fragment in one
    /// place so that the day `BUILTIN` grows an entry, every screen carrying
    /// it changes with it instead of going on saying "none".
    #[test]
    fn what_this_build_ships_reads_as_a_sentence_at_every_count() {
        assert_eq!(shipped_phrase(0), "no profile for any game");
        assert_eq!(shipped_phrase(1), "a profile for one game");
        assert_eq!(shipped_phrase(4), "profiles for 4 games");
        for n in 0..3 {
            assert!(
                format!("this build ships {}.", shipped_phrase(n)).starts_with("this build ships "),
                "it is the object of that sentence and nothing else"
            );
        }
    }

    /// A `<appid>.toml.tmp` is what an interrupted write of ours leaves. It
    /// is not a profile and it is not somebody else's file, and the two
    /// places that decide have to agree: `--purge` deletes by the predicate,
    /// so a listing that called it a stray would be printing "not written by
    /// this program" about a file this program wrote.
    #[test]
    fn an_interrupted_write_of_ours_is_not_somebody_elses_file() {
        let dir = scratch("leftover");
        std::fs::write(path_in(&dir, FAKE), "version = 1\n").expect("write");
        let tmp = format!("{FAKE}{FILE_SUFFIX}{}", paths::ATOMIC_TMP_SUFFIX);
        std::fs::write(dir.join(&tmp), "version = 1\n").expect("write");
        std::fs::write(dir.join("notes.txt"), "hello").expect("write");
        std::fs::write(dir.join(format!("{FAKE}{FILE_SUFFIX}~")), "backup").expect("write");

        assert!(is_profile_file(&tmp), "the predicate `--purge` deletes by");
        let l = list_from(&dir, &[]);
        assert_eq!(l.leftovers, vec![tmp], "written by this program");
        assert_eq!(
            l.strays,
            vec![format!("{FAKE}{FILE_SUFFIX}~"), "notes.txt".to_string()],
            "and nothing this program wrote is among what it did not write"
        );
        assert_eq!(l.profiles.len(), 1);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        std::fs::remove_dir_all(&dir).ok();
    }

    // --------------------------------------------------------------- writing

    #[test]
    fn what_is_written_is_read_back() {
        let dir = scratch("save");
        assert!(
            save_to(&dir, FAKE, &full()).expect("save").is_empty(),
            "there was no file, so there was nothing in one to lose"
        );
        let got = load_from(&dir, &[], FAKE).expect("ok").expect("found");
        assert_eq!(got.profile, full());
        assert_eq!(got.origin, Origin::File(path_in(&dir, FAKE)));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The whole reason to preserve comments: the only place a fact somebody
    /// measured can live in a profile is a `#` line, and a save has to be
    /// survivable or nobody will write one down.
    ///
    /// Written as the whole file, not as a search for fragments: where each
    /// comment lands is the property, and a `contains` would pass with every
    /// one of them dumped at the bottom.
    #[test]
    fn a_save_keeps_the_comments_it_found() {
        let dir = scratch("save-comments");
        let before = "\
# tobii-linux game profile
# measured 2026-10-01, verified on Odyssey 4.0
version = 1
name = \"A Game\" # the launcher calls it this
bridge = true

[settings]
# gaze steering was too strong at 1.0
enabled = \"true\"

[[check]]
# the preset directory, not a file in it
format = \"binds-dir\"
path = \"drive_c/x\"
setting = \"HeadlookMode\"
wants = \"1\"
tell = \"Set head look to toggle.\"
# nothing below this anchors it
";
        let after = "\
# tobii-linux game profile
# measured 2026-10-01, verified on Odyssey 4.0
version = 1
name = \"A Game\" # the launcher calls it this
bridge = true

[settings]
# gaze steering was too strong at 1.0
enabled = \"true\"
rate_hz = \"60\"

[[check]]
# the preset directory, not a file in it
format = \"binds-dir\"
path = \"drive_c/x\"
setting = \"HeadlookMode\"
wants = \"1\"
tell = \"Set head look to toggle.\"
# nothing below this anchors it
";
        std::fs::write(path_in(&dir, FAKE), before).expect("write");
        let mut p = parse(before).expect("parses");
        p.settings.push(("rate_hz".into(), "60".into()));

        assert!(save_to(&dir, FAKE, &p).expect("save").is_empty());
        let back = std::fs::read_to_string(path_in(&dir, FAKE)).expect("read");
        assert_eq!(back, after);

        // And a save that changes nothing changes nothing — the header a
        // profile of ours starts with is not a comment to preserve on top of
        // the one being written.
        assert!(save_to(&dir, FAKE, &p).expect("save again").is_empty());
        assert_eq!(
            std::fs::read_to_string(path_in(&dir, FAKE)).expect("read"),
            after
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The other half of the promise. A comment about a check that is being
    /// removed has no place in the file, so it comes back out to the caller —
    /// and the write happens, because `tobii games profile check remove` is
    /// impossible otherwise and a commented check is the only kind this
    /// feature exists to produce.
    #[test]
    fn a_comment_about_a_removed_check_comes_back_and_the_write_happens() {
        let dir = scratch("save-orphan");
        let before = "version = 1\n\n[[check]]\n# measured on Odyssey 4.0\n                      format = \"binds-dir\"\npath = \"a\"\nsetting = \"X\"\n                      wants = \"1\"\ntell = \"t\"\n";
        std::fs::write(path_in(&dir, FAKE), before).expect("write");
        let mut p = parse(before).expect("parses");
        p.checks.clear();

        let lost = save_to(&dir, FAKE, &p).expect("the removal goes through");
        assert_eq!(lost.len(), 1, "{lost:?}");
        assert_eq!(lost[0].line, 4);
        assert_eq!(lost[0].comment, "# measured on Odyssey 4.0");
        assert_eq!(
            lost[0].about,
            "`format` of the check that reads `X` out of `a`"
        );
        let shown = lost[0].to_string();
        assert!(shown.contains("# measured on Odyssey 4.0"), "{shown}");
        assert!(shown.contains("line 4"), "{shown}");

        let back = std::fs::read_to_string(path_in(&dir, FAKE)).expect("read");
        assert_eq!(back, format!("{HEADER}\nversion = 1\n"), "{back}");
        assert!(
            !back.contains("Odyssey"),
            "and the sentence it handed back is not also still in the file"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The comment at the end of a file is about the last thing in the file,
    /// so it comes back under that thing — not under whatever ends the file
    /// after a check is added. Anything else moves somebody's *measured on
    /// Odyssey 4.0* onto a check they never looked at, without a word.
    #[test]
    fn a_note_at_the_end_of_a_file_stays_under_what_it_was_under() {
        let before = "\
version = 1

[[check]]
format = \"binds-dir\"
path = \"a\"
setting = \"X\"
wants = \"1\"
tell = \"t\"
# measured on Odyssey 4.0
";
        let mut p = parse(before).expect("parses");
        p.checks.push(Check {
            format: Format::AttributesXml,
            path: "b".into(),
            setting: "Y".into(),
            wants: "2".into(),
            tell: "u".into(),
        });
        let (text, orphans) = p.rewrite_over(before).expect("readable");
        assert!(orphans.is_empty(), "{orphans:?}");

        let lines: Vec<&str> = text.lines().collect();
        let note = lines
            .iter()
            .position(|l| *l == "# measured on Odyssey 4.0")
            .expect("the note is still there");
        assert_eq!(
            lines[note - 1],
            "tell = \"t\"",
            "under the check it was written under: {text}"
        );
        assert_ne!(
            lines.last(),
            Some(&"# measured on Odyssey 4.0"),
            "and not at the end of a file that now ends with another check: {text}"
        );
    }

    /// A check is found again by what it reads, not by its number. Take the
    /// first of two out and the second one's numbering changes; a note above
    /// the one that went must not come back above the one that stayed, which
    /// is the same untruth as re-emitting it at the end of the file and is
    /// harder to see, because nothing about the result looks wrong.
    #[test]
    fn a_note_above_a_removed_check_does_not_slide_onto_the_next_one() {
        let before = "\
version = 1

# X is what the HUD calls head look
[[check]]
format = \"binds-dir\"
path = \"a\"
setting = \"X\"
wants = \"1\"
tell = \"t\"

[[check]]
format = \"attributes-xml\"
path = \"b\"
setting = \"Y\"
wants = \"2\"
tell = \"u\"
";
        let mut p = parse(before).expect("parses");
        p.checks.remove(0);
        let (text, orphans) = p.rewrite_over(before).expect("readable");

        assert!(
            !text.contains("HUD"),
            "the note is about a check that is gone: {text}"
        );
        assert_eq!(orphans.len(), 1, "{orphans:?}");
        assert_eq!(orphans[0].line, 3);
        assert_eq!(
            orphans[0].about, "the check that reads `X` out of `a`",
            "named by what it read, since after a removal neither numbering is theirs"
        );
        assert_eq!(parse(&text).expect("reads back").checks.len(), 1);
    }

    /// The other side of that: a check whose value or sentence is corrected
    /// is the same check, and keeps its comment. `check add` already treats
    /// format, path and setting as what makes two checks the same one.
    #[test]
    fn correcting_what_a_check_wants_keeps_the_note_above_it() {
        let before = "\
version = 1

# measured on Odyssey 4.0
[[check]]
format = \"binds-dir\"
path = \"a\"
setting = \"X\"
wants = \"1\"
tell = \"t\"
";
        let mut p = parse(before).expect("parses");
        p.checks[0].wants = "2".into();
        p.checks[0].tell = "set it to 2".into();
        let (text, orphans) = p.rewrite_over(before).expect("readable");
        assert!(orphans.is_empty(), "{orphans:?}");
        let lines: Vec<&str> = text.lines().collect();
        let note = lines
            .iter()
            .position(|l| *l == "# measured on Odyssey 4.0")
            .expect("kept");
        assert_eq!(lines[note + 1], "[[check]]", "{text}");
    }

    /// A `#` inside a value is part of the value, not a comment to move.
    #[test]
    fn a_hash_in_a_value_is_not_a_comment_a_save_relocates() {
        let p = Profile {
            name: Some("# not a comment".into()),
            settings: vec![("tell".into(), "press # twice".into())],
            ..Profile::default()
        };
        let text = p.to_toml();
        assert_eq!(
            p.to_toml_over(&text).expect("nothing to place"),
            text,
            "a file of ours written over itself is itself"
        );
        assert_eq!(parse(&text), Ok(p));
    }

    /// A file this build cannot read is not overwritten by a build that
    /// cannot say what was in it — the same rule `load_from` follows for
    /// reading, on the writing side.
    #[test]
    fn a_file_that_cannot_be_read_is_not_written_over() {
        let dir = scratch("save-unreadable");
        let before = "# somebody wrote this\nversion = 9\n";
        std::fs::write(path_in(&dir, FAKE), before).expect("write");
        let e = save_to(&dir, FAKE, &full()).expect_err("refused");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(e.to_string().contains("# somebody wrote this"), "{e}");
        assert_eq!(
            std::fs::read_to_string(path_in(&dir, FAKE)).expect("read"),
            before,
            "nothing was written"
        );
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
