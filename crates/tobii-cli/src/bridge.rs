//! `tobii bridge` — put the Wine-side bridge into a game's prefix.
//!
//! # The thing that was easy to get wrong, and how it stopped mattering
//!
//! A Wine prefix is served by a `wineserver`, and named kernel objects like
//! `FT_SharedMem` live inside one server's session. The bridge used to be a
//! separate `tobii-bridge.exe` you left running, and launching it with
//! `/usr/bin/wine` against a prefix whose game runs under a bundled runner gave
//! you a *second* wineserver: the bridge started, said everything worked,
//! created its mapping — and the game never saw it, because it was looking at a
//! different session's namespace. Nothing about that failure pointed at its
//! cause, and for a Steam/Proton title it was not a mistake but the only
//! possible outcome, short of reproducing Proton's whole launch environment.
//!
//! The receive loop lives in the client DLL now, inside the game's own process,
//! so the sessions match by construction and there is nothing to leave running.
//! See `tobii_bridge_core::feeder`.
//!
//! **Which `wine` binary still matters here**, for two narrower reasons:
//!
//! * Writing the registry. A prefix records the version that built it, and a
//!   different wine touching it runs `wineboot -u` and upgrades it — so using
//!   the system wine to write two values could rewrite a Proton prefix out from
//!   under the game that owns it. Steam records the answer in the prefix's own
//!   `config_info`, and that is what [`wine_from_steam_config_info`] reads.
//! * The one remaining case that needs `tobii bridge run`: a TrackIR game
//!   pointed at a third-party client DLL. That DLL is a pure consumer of
//!   `FT_SharedMem`, and a TrackIR-only game never loads ours — so something
//!   must fill the mapping, in the game's own session.
//!
//! # What this command promises about the registry, and what it refuses to
//!
//! [`NP_KEY`] and [`FT_KEY`] are how *any* head-tracking client is found —
//! opentrack's as much as ours — so writing one blind takes another program's
//! registration away, and deleting one on the way out leaves the prefix with no
//! client registered at all. The rule that makes both impossible is narrow:
//!
//! * install reads the key first. Nothing registered there, or something this
//!   installer can account for — our own directory inside this prefix, what
//!   [`RECORD_FILE`] says we wrote there, or, where no record says anything at
//!   all, exactly the value this run would write — and it is written. Anything
//!   else — another program's path, a type we never write, bytes wine did not
//!   print back in a form we can read — is refused, with nothing written,
//!   nothing created and nothing started, and named without accusing anyone the
//!   record cannot name. `--force` goes ahead and promises *nothing* about
//!   putting the old value back.
//! * uninstall reads the key first too, and removes the `Path` value it added,
//!   only while the key still says what we wrote. Anything else is left exactly
//!   as it is, and named.
//! * the record holds only values this program computed itself, written down
//!   after the key took one and naming only the keys that took it. A value read
//!   out of a key is compared and then dropped: never stored, never written.
//!
//! **Where that first read comes from.** `wine reg query` is the accurate
//! answer and an expensive one: wine initialises or upgrades whatever prefix it
//! is pointed at before it answers anything, so a refusal that reached for it
//! rewrote the prefix it had just declined to touch — measured on a
//! Proton-shaped throwaway prefix, `.update-timestamp` moved and 2764 lines of
//! `system.reg` rewritten by an install that created no directory and wrote no
//! key. The refusal above, and an uninstall with nothing of ours to remove, now
//! decide from the prefix's own `user.reg`, which costs no process at all; wine
//! is started only once this run has decided it is going to write, which is
//! something it was going to do anyway. [`settled_keys`] holds the two cases
//! where the file cannot settle it and wine is asked after all.
//!
//! That last line is what the other two rest on. Two earlier versions of this
//! module remembered the old value and put it back on the way out, and every
//! defect that followed lived in that one mechanism — a value mangled by wine's
//! console codepage written back as the "restored" one, a stale prior
//! resurrected over a newer registration, a delete that failed because the
//! value was already gone. None of them can be expressed now: the only writes
//! are paths this program computed, and a comparison that goes wrong answers
//! "not ours", whose action is to leave the key alone.

use std::path::{Path, PathBuf};

use crate::CmdResult;

/// Where the artifacts are installed inside the prefix.
pub(crate) const INSTALL_SUBDIR: &str = "drive_c/tobii-bridge";

/// The Windows spelling of the same directory.
///
/// Shared with [`crate::proton`], which spells the provider's path into a batch
/// file the game's own `cmd.exe` runs: a second copy of this string would let
/// the two drift, and the launch would start nothing while `install` went on
/// reporting a directory it had filled.
pub(crate) const INSTALL_WIN_DIR: &str = r"C:\tobii-bridge";

/// Registry key a TrackIR game reads to find its client DLL.
const NP_KEY: &str = r"HKCU\Software\NaturalPoint\NATURALPOINT\NPClient Location";

/// Registry key a FreeTrack game reads to find its client DLL.
const FT_KEY: &str = r"HKCU\Software\Freetrack\FreeTrackClient";

/// One entry of [`KEYS`]: the name it is recorded under, the key, its ABI.
type KeyEntry = (&'static str, &'static str, &'static str);

/// Both discovery keys: the name each is recorded under, the key, its ABI.
///
/// Paired in one place because everything that touches one touches both —
/// install reads and writes both, uninstall reads both and takes out only what
/// it put there — and because the short name is what ends up in
/// [`RECORD_FILE`], where a rename would silently orphan an existing prefix's
/// record.
const KEYS: [KeyEntry; 2] = [("ft", FT_KEY, "FreeTrack"), ("np", NP_KEY, "TrackIR")];

/// Where, inside the prefix, this installer writes down what it put in those
/// keys.
///
/// ONLY what we wrote — paths this program computed itself — and never a value
/// read out of the registry. That is the whole contract described in the module
/// header: a value found in a key is compared and then forgotten, so there is
/// no stored copy of anyone else's registration to corrupt, resurrect, or write
/// back over whatever holds the key by then.
///
/// Inside the prefix on purpose, under the directory the artifacts already go
/// in. What was written belongs to *that* prefix and to nothing else: a Steam
/// title whose compatdata is deleted takes the record with it, whereas a record
/// kept in the user's own config would outlive the prefix it describes.
const RECORD_FILE: &str = "registered.txt";

/// Directories an installed opentrack keeps its client DLLs in.
const OPENTRACK_DIRS: [&str; 5] = [
    "/usr/libexec/opentrack",
    "/usr/lib/opentrack",
    "/usr/lib64/opentrack",
    "/usr/local/libexec/opentrack",
    "/usr/local/lib/opentrack",
];

/// Which `NPClient64.dll` a TrackIR game should be pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NpSource {
    /// Ours, installed alongside the provider.
    Ours,
    /// A separately-installed one, named by its containing directory.
    ///
    /// Needed because TrackIR clients are gated by NaturalPoint's
    /// `NP_GetSignature` anti-clone check, which a clean-room DLL cannot
    /// answer. Both titles measured against it stop there, by different
    /// routes: Star Citizen (2026-08-15) calls it once, gets nothing it
    /// recognises, and never asks for data again; Microsoft Flight Simulator
    /// 2024 (Steam appid 2537590, Proton Experimental, 2026-09-27) calls it
    /// 104 times in 1m45s, calls nothing else at all, and never proceeds — so
    /// one title loses its tracking and the other loses its tracking and
    /// retries the check for as long as it runs. An already-installed client
    /// can answer it, and reads the same `FT_SharedMem` our provider writes —
    /// so it is used through its published interface, with our data behind it.
    Installed(PathBuf),
}

/// Decide which NPClient DLL to register.
///
/// Prefers an installed third-party client when one is present, because that is
/// the only configuration in which a TrackIR game actually receives anything.
/// `--npclient ours` forces ours, which is the right choice for a game that does
/// not check the signature — a FreeTrack title, or a TrackIR one that never
/// asks.
///
/// Honoured rather than second-guessed, because nothing here can tell those
/// games from the gated ones. What says the choice has a cost is
/// [`ours_for_trackir`], which `install` prints whenever the TrackIR key ends
/// up pointing at our own DLL — including when the flag asked for it.
pub fn choose_npclient(
    explicit: Option<&str>,
    installed: Option<PathBuf>,
) -> Result<NpSource, String> {
    match explicit {
        Some("ours") => Ok(NpSource::Ours),
        Some("auto") | None => Ok(match installed {
            Some(dir) => NpSource::Installed(dir),
            None => NpSource::Ours,
        }),
        Some(path) => {
            let dir = PathBuf::from(path);
            if !dir.join("NPClient64.dll").is_file() {
                return Err(format!("no NPClient64.dll in {}", dir.display()));
            }
            Ok(NpSource::Installed(dir))
        }
    }
}

/// The Wine spelling of a Linux path, via the `Z:` drive.
///
/// Lets a game load a DLL from anywhere on the host without anything being
/// copied into the prefix — which keeps a third-party client exactly where its
/// own package manager put it.
pub fn wine_path_for(linux: &Path) -> String {
    format!("Z:{}", linux.display().to_string().replace('/', "\\"))
}

/// Find an installed opentrack's client directory, if there is one.
fn find_installed_npclient() -> Option<PathBuf> {
    OPENTRACK_DIRS
        .iter()
        .map(PathBuf::from)
        .find(|d| d.join("NPClient64.dll").is_file())
}

/// Everything `install` says about what it registered, in one string.
///
/// One function because the two halves have to be printed together and were
/// not: the "registered …" lines were held in a local and the note about
/// `--npclient ours` went out through a `println!` of its own, several lines
/// earlier. Nothing could assert that `install` said both, so a guard put back
/// around that `println!` left the whole suite green while reintroducing the
/// silence that cost a user an evening.
///
/// `explicit_ours` is whether the user typed `--npclient ours` rather than
/// having it fall out of `auto`, and `installed` is what
/// [`find_installed_npclient`] found — both are [`ours_for_trackir`]'s to
/// interpret, not this function's.
fn registered_text(
    np: &NpSource,
    np_target: &str,
    explicit_ours: bool,
    installed: Option<&Path>,
) -> String {
    match np {
        NpSource::Ours => format!(
            "registered {INSTALL_WIN_DIR} for TrackIR and FreeTrack\n\n{}",
            ours_for_trackir(explicit_ours, installed)
        ),
        NpSource::Installed(_) => format!(
            "registered {INSTALL_WIN_DIR} for FreeTrack\n\
             registered {np_target} for TrackIR\n\n\
             TrackIR points at the client already installed there, because games\n\
             verify NaturalPoint's signature and a clean-room DLL cannot answer it.\n\
             Nothing was copied."
        ),
    }
}

/// What `install` says when the TrackIR key is about to point at our own DLL.
///
/// Printed whichever way the run got there, the run that asked for it by name
/// included. It used to be withheld from exactly that run, on the reading that
/// somebody passing `--npclient ours` had chosen it already — but the flag
/// picks a DLL, it does not say the cost of that DLL is understood, and a user
/// who reached for it on advice got the losing configuration with nothing said
/// about it. It still does not refuse: a FreeTrack title, or a TrackIR one
/// that never checks, is a real case and ours is right for it.
///
/// What it may say is what was measured. Two titles are two titles and not a
/// rule about TrackIR games, and nothing here can tell which kind a user's
/// game is — so it names them, dates them, and stops.
fn ours_for_trackir(explicit: bool, installed: Option<&Path>) -> String {
    let opening = match (explicit, installed) {
        (true, Some(dir)) => format!(
            "--npclient ours registers our own DLL for TrackIR, and there is a\n      \
             third-party one installed in {}\n      \
             — which is the one that can answer the check.",
            dir.display()
        ),
        (true, None) => "--npclient ours registers our own DLL for TrackIR. No \
             third-party\n      client was found here either, so it was the only one \
             available."
            .to_string(),
        (false, _) => "no third-party NPClient64.dll was found, so our own is \
             registered\n      for TrackIR."
            .to_string(),
    };
    let body = [
        tobii_config::signature::trackir_gate(),
        tobii_config::signature::provider_note(),
        // This command's own, and deliberately not the seam's: the seam is
        // shared with a window that has no flags, and a flag named there would
        // be a flag the hub cannot offer. `install` is where the spelling
        // belongs.
        "`--npclient auto` is what registers an installed client instead of ours.".to_string(),
    ]
    .join(" ");
    format!("note: {opening}\n{}", wrapped(&body, "      ", 78))
}

/// `text` folded to `width` columns with every line under `indent`.
///
/// The seam this borrows its sentences from renders them unwrapped on purpose:
/// a GTK label wraps itself, and a terminal knows a width the label does not.
/// This is the terminal knowing it. Splitting on spaces is enough because the
/// text it is given is prose — there is nothing in it that must not be broken,
/// and a word longer than the width takes its own line rather than being cut.
fn wrapped(text: &str, indent: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && indent.len() + line.len() + 1 + word.len() > width {
            out.push_str(indent);
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    out.push_str(indent);
    out.push_str(&line);
    out
}

/// The one artifact without which there is no installation — also what
/// [`artifact_dir`] recognises a build directory by.
const REQUIRED_ARTIFACT: &str = "freetrackclient64.dll";

/// What `install` copies, and whether its absence is fatal.
///
/// The DLLs are the product. `tobii-bridge.exe` used to be required, because it
/// was the thing that received frames and created `FT_SharedMem`; the DLLs feed
/// themselves now, so it is a diagnostic — a console that says what is arriving
/// — and a prefix without it works.
///
/// **[LIMITATION] 64-bit games only.** A 32-bit game calls
/// `LoadLibrary("freetrackclient.dll")` — no `64` — and finds nothing here, so
/// it gets no tracking while `install` reports success. That is a real slice of
/// the head-tracking audience (Falcon BMS, IL-2 1946, the FSX generation), and
/// opentrack ships all four names side by side for exactly this reason.
/// Closing it means building the two client crates for `i686-pc-windows-gnu` as
/// well and adding them here; the registry key and install directory are shared,
/// so nothing else changes.
const ARTIFACTS: [(&str, bool); 3] = [
    ("tobii-bridge.exe", false),
    ("freetrackclient64.dll", true),
    ("NPClient64.dll", false),
];

/// Where the wine binary came from, and so what may be said about it.
///
/// The refusal install prints tells the user to clear the key with *this
/// prefix's own wine*, and the reason it insists is that a different build runs
/// `wineboot -u` against a prefix it does not own and upgrades it — for a
/// Proton prefix, out from under the game. That sentence is only true when the
/// binary actually came from the prefix, and [`choose_wine`] falls back to
/// whatever `wine` is on `$PATH` when nothing in the prefix names one. Handing
/// *that* one over under that sentence is the very upgrade the sentence warns
/// against, with this module's own authority behind it — so the claim travels
/// with the evidence for it instead of being asserted wherever it reads well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WineOrigin {
    /// Recorded in, or bundled with, the prefix itself — so it is the build
    /// this prefix belongs to.
    Prefix,
    /// Whatever `wine` is on `$PATH` because nothing in the prefix named one,
    /// or a binary the user named with `--wine` that the prefix does not
    /// corroborate. Usable, but nothing here established whose prefix it owns.
    Unverified,
}

/// Choose which `wine` to use, from what was found.
///
/// Pure, so the preference order is testable without a prefix on disk. The order
/// is deliberate: an explicit choice, then whatever the prefix's own launch
/// script uses (the LUG Star Citizen layout records it), then a runner bundled
/// in the prefix, and only then whatever is on `$PATH`.
pub fn choose_wine(
    explicit: Option<PathBuf>,
    launch_script: Option<PathBuf>,
    runners: &[PathBuf],
    on_path: Option<PathBuf>,
) -> Result<(PathBuf, WineOrigin, Option<String>), String> {
    if let Some(w) = explicit {
        // An explicit choice is honoured, but still checked against the prefix's
        // own so a mismatch is visible rather than mysterious. That same check
        // is the only thing that can call an explicit binary the prefix's own:
        // it is corroborated when it IS what the prefix records, and merely
        // used when the prefix records nothing or records something else.
        let expected = launch_script.or_else(|| runners.first().cloned());
        let (origin, warning) = match expected {
            Some(e) if e == w => (WineOrigin::Prefix, None),
            Some(e) => (
                WineOrigin::Unverified,
                Some(format!(
                    "using {} but this prefix's own runner is {} — if the game sees no \
                     tracking, that mismatch is why (two wineservers, two namespaces)",
                    w.display(),
                    e.display()
                )),
            ),
            None => (WineOrigin::Unverified, None),
        };
        return Ok((w, origin, warning));
    }
    if let Some(w) = launch_script {
        return Ok((w, WineOrigin::Prefix, None));
    }
    if let Some(w) = runners.first() {
        return Ok((w.clone(), WineOrigin::Prefix, None));
    }
    match on_path {
        Some(w) => {
            // The consequence this names is the one that actually happens, and
            // it took a measurement to notice it was the wrong one. This said
            // "if the game sees no tracking, pass --wine…", which is a warning
            // about an outcome the user can undo by trying again. What a
            // fallback to the system wine really costs is in this module's own
            // header and in [`refusal`]: wine upgrades a prefix it does not own
            // before it does anything else. Measured on a Proton-shaped
            // throwaway prefix — `.update-timestamp` taken over, thousands of
            // lines of `system.reg` rewritten, `wineboot`, `explorer`,
            // `rundll32` and `control` spawned — by an `install` whose only
            // printed warning was about tracking.
            // Broken into lines because [`render_status`] prints this one
            // indented under the `wine` heading, a line at a time.
            let warning = format!(
                "no runner found inside the prefix, falling back to the system\n\
                 wine ({}). A wine that is not the build a prefix was made with\n\
                 runs `wineboot -u` against it before it does anything else and\n\
                 upgrades it: the prefix's stamp taken over and its `system.reg`\n\
                 rewritten. For a game's own prefix that is a rewrite out from\n\
                 under the game, and nothing here puts it back. Pass --wine with\n\
                 the runner the game uses.",
                w.display()
            );
            Ok((w, WineOrigin::Unverified, Some(warning)))
        }
        None => Err("no wine binary found; pass --wine PATH".into()),
    }
}

/// The wine binary a LUG-style `sc-launch.sh` selects, if there is one.
///
/// That script records the runner in `export wine_path="..."`, which is the most
/// authoritative answer available: it is literally what launches the game.
///
/// Every line that is not that assignment is skipped, not treated as the end of
/// the file. Giving up at the first one used to end the scan on line 1 of every
/// real script: `sc-launch.sh` opens with a shebang, a blank line is enough on
/// its own, and the upstream helper carries a couple of dozen other statements
/// before this assignment. So the tier that exists for the LUG layout never
/// fired, and resolution fell through to the system wine — which then runs
/// `wineboot -u` against a prefix it does not own, the upgrade this module is
/// organised around preventing.
fn wine_from_launch_script(prefix: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(prefix.join("sc-launch.sh")).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("export wine_path=") else {
            continue;
        };
        let dir = rest.trim().trim_matches('"').trim_matches('\'');
        let candidate = PathBuf::from(dir).join("wine");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Wine binaries from runners bundled in the prefix, newest name last.
fn wines_from_runners(prefix: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let Ok(entries) = std::fs::read_dir(prefix.join("runners")) else {
        return found;
    };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    // Reversed so the highest-versioned runner name is preferred, which is the
    // one a user who installed a newer runner almost certainly means.
    for d in dirs.into_iter().rev() {
        let w = d.join("bin/wine");
        if w.is_file() {
            found.push(w);
        }
    }
    found
}

/// Look a bare command up on `$PATH`.
fn on_path(cmd: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join(cmd))
        .find(|p| p.is_file())
}

/// The Proton build a Steam prefix was made with, from its `config_info`.
///
/// # Why this matters more than a convenience
///
/// Writing the registry with the *wrong* wine is not a neutral act. A prefix
/// records the version that built it, and a different wine touching it runs
/// `wineboot -u` and upgrades it — so reaching for the system wine to write two
/// registry values could rewrite a Proton prefix out from under the game that
/// owns it.
///
/// Steam records the answer next to the prefix. `compatdata/<appid>/config_info`
/// is a plain list of lines whose second and third name paths inside the Proton
/// build; the build root is the directory containing `files/`, and its wine is
/// `files/bin/wine`. Older Proton laid this out as `dist/` instead, so both are
/// tried.
pub fn wine_from_steam_config_info(prefix: &Path) -> Option<PathBuf> {
    // `prefix` is `<compatdata>/<appid>/pfx`; the file sits beside it.
    let info = prefix.parent()?.join("config_info");
    let text = std::fs::read_to_string(info).ok()?;
    for line in text.lines() {
        let line = line.trim();
        for marker in ["/files/", "/dist/"] {
            if let Some(cut) = line.find(marker) {
                let root = Path::new(&line[..cut]);
                for sub in ["files/bin/wine", "dist/bin/wine"] {
                    let candidate = root.join(sub);
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    None
}

// Reading Steam's own files — the libraries, what is installed in them, and
// where a title's Proton prefix is — is `tobii_steam`'s job, not this
// module's, and every caller below goes there directly for it.
//
// It lives there so the GTK hub can reach it: `tobii-cli` is a `[[bin]]` with
// no `[lib]`, so nothing declared in here is callable from anywhere else in
// the workspace. What stays here is the wording — which errors this command
// prints and how — because a picker in the hub and a line on a terminal want
// the same decision phrased two different ways.

/// Whether `--steam <wanted>` names an app id rather than a title.
///
/// An app id is used verbatim and is never checked against what is installed,
/// because `compatdata` outlives the manifest in both of the directions that
/// matter: Steam keeps a prefix after the title is uninstalled — which is why
/// `uninstall` scans `compatdata/*` rather than the installed list — and a
/// non-Steam shortcut added to Steam gets a prefix under an id that no
/// `appmanifest_*.acf` ever mentions. Both of those resolve today, and both
/// would stop resolving if the id went through [`tobii_steam::resolve`], which
/// answers `None` for an id it cannot find installed and would turn each of
/// them into "no installed Steam game matches".
///
/// Named for the decision it makes, not for the question it looks like: there
/// is an `is_app_id` in `main.rs` too, and it answers the *other* question —
/// whether a string is a well-formed app id — for which the empty string is
/// plainly not one. Two functions of one name giving one input opposite
/// answers in one binary is a trap, and this is the one that is not about
/// well-formedness.
///
/// **The empty string passes this vacuously, and that is deliberate.** It is
/// therefore the empty app id, which nothing is installed under and nothing
/// has a prefix for, so `--steam ""` reaches the prefix paragraph.
///
/// [`tobii_steam::resolve`] would answer better, and it refuses nothing: it
/// excludes the empty string from its app-id branch and treats it as a name
/// fragment, which every name contains. So it matches everything installed —
/// [`tobii_steam::Match::Many`] on a machine with several games, which would
/// print the ambiguous list, and [`tobii_steam::Match::One`] on a machine
/// holding exactly one, which would silently succeed against that game's
/// prefix. Better, and different enough to be somebody's decision rather than
/// a side effect of this one: what a bare `--steam ""` should do is a question
/// about this command's wording, and the wording is what this module owns.
fn used_verbatim_as_app_id(wanted: &str) -> bool {
    wanted.chars().all(|c| c.is_ascii_digit())
}

/// Turn `--steam <appid|name fragment>` into a prefix path.
///
/// What a name fragment picked out is [`tobii_steam::resolve`]'s decision; the
/// wording of every answer is this function's. A name is matched
/// case-insensitively as a substring, because nobody types "Elite Dangerous"
/// with the right capitalisation twice. An ambiguous fragment lists what it
/// matched rather than picking one.
fn steam_prefix_for(home: &Path, wanted: &str) -> Result<PathBuf, String> {
    // One walk, for the list, the absent-library sentence and the prefix
    // alike. Each of the three used to read every root's `libraryfolders.vdf`
    // for itself.
    let steam = tobii_steam::Steam::at(home);
    let apps = steam.apps();
    let appid = if used_verbatim_as_app_id(wanted) {
        wanted.to_string()
    } else {
        match tobii_steam::resolve(&apps, wanted) {
            tobii_steam::Match::One(app) => app.appid,
            tobii_steam::Match::Many(hits) => {
                let list: Vec<String> = hits
                    .iter()
                    .map(|a| format!("  {:<10} {}", a.appid, a.name))
                    .collect();
                return Err(format!(
                    "{wanted:?} matches more than one game; pass the app id:\n{}",
                    list.join("\n")
                ));
            }
            tobii_steam::Match::None => {
                let missing = crate::steam_libraries_missing(&steam);
                // "No installed Steam game matches" is a claim about every
                // game installed, and there is a library here that could not
                // be looked in. So the sentence says what was looked at.
                let mut msg = if missing.is_empty() {
                    format!("no installed Steam game matches {wanted:?}")
                } else {
                    format!("nothing in the Steam libraries this machine has matches {wanted:?}")
                };
                if apps.is_empty() {
                    msg.push_str(" (no Steam libraries found)");
                } else {
                    msg.push_str("\ninstalled:");
                    for a in &apps {
                        msg.push_str(&format!("\n  {:<10} {}", a.appid, a.name));
                    }
                }
                return Err(crate::with_missing(msg, &missing));
            }
        }
    };
    steam.prefix(&appid).ok_or_else(|| {
        let name = apps
            .iter()
            .find(|a| a.appid == appid)
            .map(|a| a.name.as_str())
            .unwrap_or("that app");
        format!(
            "no Proton prefix for {appid} ({name}). Proton creates it the first \
             time the game runs — start the game once, then run this again.\n\
             If it is set to run natively rather than through Proton, there is \
             no prefix and no Windows DLL to install into."
        )
    })
}

/// How the prefix in hand was named.
///
/// Carried out of [`resolve_prefix`] rather than worked out a second time by
/// the one command that prints it. "Which prefix, and how" has exactly one
/// right answer, and the precedence that produces it lives in one place: a
/// second copy is a bug waiting for the day the first one changes, and it
/// would make `tobii bridge status` name a prefix it did not look in — the one
/// failure a diagnostic must never have.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PrefixSource {
    /// `--steam <app id or name>`, resolved through Steam's own libraries.
    Steam(String),
    /// Named directly with `--prefix`.
    Given,
    /// Taken from `$WINEPREFIX`, which named one when nothing else did.
    Environment,
    /// Nothing named one at all, so wine's default under `$HOME`.
    Default,
}

impl PrefixSource {
    /// How this prefix came to be the one in hand, as a line under it.
    fn describe(&self) -> String {
        match self {
            PrefixSource::Steam(w) => format!("Steam, from `--steam {}`", quoted(w)),
            PrefixSource::Given => "given with --prefix".to_string(),
            PrefixSource::Environment => "from $WINEPREFIX".to_string(),
            PrefixSource::Default => {
                "wine's default prefix — nothing named one, so this is ~/.wine".to_string()
            }
        }
    }
}

/// Resolve the prefix: `--steam`, else `--prefix`, else `$WINEPREFIX`, else
/// `~/.wine`.
///
/// Returns *how* it was resolved along with it, because a report about a prefix
/// has to be able to say which one it read and why that one — see
/// [`PrefixSource`].
fn resolve_prefix(args: &[String]) -> Result<(PathBuf, PrefixSource), String> {
    if let Some(wanted) = crate::flag_value(args, "--steam") {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is not set, so Steam's libraries cannot be found")?;
        let p = steam_prefix_for(&home, wanted)?;
        eprintln!("Steam prefix: {}", p.display());
        return Ok((p, PrefixSource::Steam(wanted.to_string())));
    }
    let (raw, source) = crate::flag_value(args, "--prefix")
        .map(|p| (PathBuf::from(p), PrefixSource::Given))
        .or_else(|| {
            std::env::var_os("WINEPREFIX").map(|p| (PathBuf::from(p), PrefixSource::Environment))
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| (PathBuf::from(h).join(".wine"), PrefixSource::Default))
        })
        .ok_or("could not determine a Wine prefix; pass --prefix PATH or --steam <game>")?;
    if !raw.join("drive_c").is_dir() {
        return Err(format!(
            "{} does not look like a Wine prefix (no drive_c)",
            raw.display()
        ));
    }
    Ok((raw, source))
}

/// Resolve the wine binary for `prefix`, and hand the caller any warning
/// rather than printing it.
///
/// The [`WineOrigin`] comes back with it because a caller that quotes the
/// binary at the user has to know whether it is the prefix's own — see the
/// enum for what goes wrong when that is assumed.
///
/// The warning is returned rather than printed because of where it ends up.
/// `choose_wine`'s mismatch sentence — *using X but this prefix's own runner is
/// Y; if the game sees no tracking, that mismatch is why* — is the single most
/// diagnostic line this module produces, and on stderr it is absent from a
/// report the user redirected to a file and pasted into an issue. [`status`]
/// puts it in the report; every other command still prints it to stderr, where
/// it belongs among their progress lines.
fn resolve_wine_reporting(
    prefix: &Path,
    args: &[String],
) -> Result<(PathBuf, WineOrigin, Option<String>), String> {
    // Proton's own build first among the non-explicit sources: it is the only
    // one that is *recorded* as belonging to this prefix rather than inferred,
    // and using any other would upgrade the prefix.
    choose_wine(
        crate::flag_value(args, "--wine").map(PathBuf::from),
        wine_from_steam_config_info(prefix).or_else(|| wine_from_launch_script(prefix)),
        &wines_from_runners(prefix),
        on_path("wine"),
    )
}

/// The same, for the commands whose output is a running commentary: the
/// warning goes to stderr.
fn resolve_wine(prefix: &Path, args: &[String]) -> Result<(PathBuf, WineOrigin), String> {
    let (wine, origin, warning) = resolve_wine_reporting(prefix, args)?;
    if let Some(w) = warning {
        eprintln!("warning: {w}");
    }
    Ok((wine, origin))
}

/// Where the cross-compiled artifacts were built.
fn artifact_dir(args: &[String]) -> Result<PathBuf, String> {
    if let Some(d) = crate::flag_value(args, "--artifacts") {
        return Ok(PathBuf::from(d));
    }
    let rel = Path::new("bridge/target/x86_64-pc-windows-gnu/release");
    let mut candidates = vec![rel.to_path_buf()];
    // Beside the running binary, for a built-and-copied layout.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("bridge"));
            if let Some(up) = dir.parent().and_then(|p| p.parent()) {
                candidates.push(up.join(rel));
            }
        }
    }
    candidates.push(PathBuf::from("/usr/lib/tobii-linux/bridge"));
    // Recognised by the DLL, not by `tobii-bridge.exe`. The exe used to be
    // required, so probing for it was the same question; it is optional now, and
    // a directory holding only the client DLLs — which is a complete
    // installation — would otherwise not be found at all.
    for c in &candidates {
        if c.join(REQUIRED_ARTIFACT).is_file() {
            return Ok(c.clone());
        }
    }
    Err(
        "could not find the built bridge — run `scripts/build-bridge.sh`, or pass \
         --artifacts DIR"
            .into(),
    )
}

/// A wine invocation against `prefix`, built but not started.
///
/// One builder for both ways this module starts wine — [`wine_output`] and the
/// provider spawn in [`run`] — because the environment is the part that must
/// not drift. `WINEPREFIX` is which prefix gets touched, and a call that forgot
/// it would reach `~/.wine` instead.
fn wine_cmd(wine: &Path, prefix: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(wine);
    cmd.env("WINEPREFIX", prefix)
        // Wine is chatty and none of it is ours; the bridge's own output is
        // what the user needs to see.
        .env("WINEDEBUG", "-all");
    cmd
}

/// Run a wine command against `prefix` and capture what it printed.
///
/// Every `reg` command goes through here, reads and writes alike. There used to
/// be a second one that inherited stdio, and the two writes used it: `reg
/// query`'s chatter was kept out of the installer's lines while `reg add`'s and
/// `reg delete`'s landed in the middle of them — wine's own localised success
/// message, twice, between "copied 3 file(s)" and "registered …". What a failed
/// write said is printed by [`wine_said`], where it is the only thing that
/// explains the failure.
fn wine_output(wine: &Path, prefix: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    wine_cmd(wine, prefix).args(args).output()
}

/// A key every wine prefix has, used to tell "nothing is registered" apart
/// from "this wine cannot read this prefix at all".
const PROBE_KEY: &str = r"HKCU\Software";

/// The sentence every failure that blames the wine ends with.
///
/// Three errors carry it — the unreadable registry, the failed install write
/// and the failed uninstall removal — and all three are one situation: this run
/// used a wine that cannot serve this prefix, and for a Steam title the one
/// that can is recorded inside the prefix itself. Written once so the three
/// cannot drift into saying different things about the same fix.
///
/// It carries its own subject, because only two of the three sites supply one.
/// `install` and `uninstall` precede it with a sentence naming the wine ("The
/// wine used was X."), and folding this hint into a shared const while it still
/// opened with a bare "that" left [`read_key`]'s copy — whose previous sentence
/// is "Refusing to touch keys whose current value could not be read." — with
/// nothing for the pronoun to attach to but "a Steam title".
const WRONG_WINE_HINT: &str = "For a Steam title the wine must be the Proton build the \
     prefix records; pass --wine explicitly if this one is wrong for it.";

/// What `reg query <key> /v Path` said.
///
/// Three cases and not a string, because the only question ever asked of a
/// reading is "is this the value this installer wrote?", and the two ways of
/// answering no need different words from us: nothing is registered here at
/// all, or something is and it is not ours.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reading {
    /// No key and no `Path` value — nothing is registered here.
    ///
    /// The one answer whose action is to *write*, which is why nothing that
    /// found a value may ever end up here: see [`reg_query_path`].
    Absent,
    /// A plain `REG_SZ` string, read whole. The only shape this installer
    /// writes, so the only shape a value of ours can have.
    Plain(String),
    /// Something is registered that this installer would never have written: a
    /// value of another type, bytes wine did not print in a form we can read,
    /// or one that names no path at all. Carries the one thing we can honestly
    /// say about it, because only the user can act on it — and because a
    /// refusal that guessed at the reason would send them looking for a problem
    /// they do not have.
    Other(String),
}

impl Reading {
    /// How to name what is registered here, inside a sentence about it.
    fn describe(&self) -> &str {
        match self {
            Reading::Absent => "nothing",
            Reading::Plain(v) => v,
            Reading::Other(why) => why,
        }
    }
}

/// Pull the `Path` value out of `reg query <key> /v Path` output.
///
/// Parsed as bytes, deliberately not as text. Wine's `reg.exe` prints in the
/// console's OEM codepage and not UTF-8: checked against wine 11.18 here,
/// `C:\Program Files\Müller opentrack` comes back with a bare `0x81` (CP850's
/// `ü`), which is not valid UTF-8, and forcing `LC_ALL=C.UTF-8` does not change
/// it. `from_utf8_lossy` would turn that into U+FFFD — a string equal to
/// neither what the registry holds nor anything we wrote, and comparing against
/// it is guessing. ASCII is the one region every codepage wine might pick
/// agrees on, so a value that is pure ASCII is read and one that is not is
/// reported as unreadable, by name. Install refuses to register a client path
/// that is not ASCII for the same reason, so a value of *ours* is always one
/// that can be read back — see [`registrable_path`].
///
/// The line is `    Path    REG_SZ    C:\tobii-bridge`. The type is read as the
/// field it is and has to be exactly `REG_SZ`: `REG_SZ` is NOT a substring of
/// `REG_EXPAND_SZ` (the letter after `REG_` differs), so matching it as one
/// reads an expandable value — `%ProgramFiles%\opentrack`, the natural spelling
/// for an installer script and legal for both keys — as no value at all, and
/// overwrites it. The value is the whole remainder of the line, since
/// `C:\Program Files\...` is an entirely ordinary thing to find registered here
/// and one word of it would be compared against ours.
///
/// **Only the separator is eaten, never the value's own bytes.** Wine's four
/// spaces between the type and the value are fixed and it pads nothing after
/// it — checked against wine 11.18: `/d 'C:\x '` prints back as `…REG_SZ
/// C:\x ` with that trailing space intact. `trim()` here used to take it off
/// anyway, which broke both halves of this function's job at once: a client
/// directory whose path ends in a space produced a registration this installer
/// could never recognise as its own again, and a value that is *nothing but*
/// whitespace came back as [`Reading::Absent`] — the one answer that leads to a
/// write, manufactured out of a value somebody else had put there. A value that
/// is present but names no path is [`Reading::Other`], like every other thing
/// we did not write: the two negative answers are not interchangeable, and only
/// one of them is safe to be wrong about.
fn reg_query_path(stdout: &[u8]) -> Reading {
    // Wine prints the value's line and then one blank line, and stops. So the
    // `Path` line of a value we can read whole is the last line of the output,
    // and whatever follows it is the remainder of a value that carried a line
    // break of its own. That is what is checked, rather than the shape of the
    // matched chunk alone.
    //
    // The chunk alone is not enough. A chunk that arrived without its CR is
    // certainly a fragment — the break came from inside the value — but a chunk
    // that ends WITH a CR can be one too, because the break inside the value
    // can itself be a CRLF: a `Path` holding `C:\tobii-bridge<CR><LF>EVIL`
    // splits into a first chunk ending in CR that is byte for byte our own
    // computed path. A fragment must never be compared. That one would have
    // read as ours twice over — install would take the key as its own and
    // overwrite a value it never wrote, and uninstall would delete a
    // registration this program never made.
    //
    // Checked against wine 11.18, the three tails are distinguishable and only
    // here: a clean value leaves exactly `\r\n` behind its line, a value with an
    // embedded CRLF leaves `EVIL\r\n\r\n`, and one *ending* in CRLF leaves
    // `\r\n\r\n` — that last one also a fragment, and also caught.
    let chunks: Vec<&[u8]> = stdout.split(|b| *b == b'\n').collect();
    let last = chunks.len().saturating_sub(1);
    // How far this chunk and the newline that ended it reach, so the tail can
    // be looked at whole instead of chunk by chunk. The last chunk ends no
    // newline: it is what followed the final one.
    let mut after = 0usize;
    for (i, line) in chunks.iter().enumerate() {
        let line = *line;
        after += line.len() + usize::from(i != last);
        let rest = &stdout[after..];
        // Empty as well as `\r\n`, because output captured without its closing
        // blank line is still output whose value line ended where the split did.
        let whole_line = rest.is_empty() || rest == b"\r\n";
        // Everything up to the value — the name, the whitespace, the type — is
        // ASCII whatever codepage wine chose, so the line is parsed as ASCII up
        // to the first byte that is not one, and only the value itself raises
        // the question of whether we can read it.
        let ascii = &line[..line
            .iter()
            .position(|b| !b.is_ascii())
            .unwrap_or(line.len())];
        // Infallible, and written as a total decode to say so: `ascii` stops at
        // the first byte that is not ASCII, and every ASCII byte is valid UTF-8.
        // A fallible decode here spent a dead arm on a `continue`, and this
        // function's safety argument is read off its control flow — every
        // `continue` in it has to mean "not the Path line", or a reader has to
        // prove an unreachable branch cannot manufacture an `Absent`.
        let head = std::str::from_utf8(ascii).unwrap_or_default();
        // Exactly one CR, which is wine's. `trim_end_matches` took every one
        // of them, so a value whose own last byte is a CR came back a byte
        // short and then failed to match itself.
        let head = head.strip_suffix('\r').unwrap_or(head).trim_start();
        let Some(rest) = head.strip_prefix("Path") else {
            continue;
        };
        // `Path` and not `PathX`: the name must end where we stopped reading.
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        if !whole_line {
            return Reading::Other(
                "a value with a line break in it, which this installer never writes \
                 and cannot read back whole"
                    .to_string(),
            );
        }
        if ascii.len() != line.len() {
            return Reading::Other(
                "a value with characters outside ASCII, which wine does not print \
                 in UTF-8"
                    .to_string(),
            );
        }
        let rest = rest.trim_start();
        let (ty, value) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if ty != "REG_SZ" {
            return Reading::Other(format!(
                "a value stored as {ty}, which this installer never writes"
            ));
        }
        // Leading whitespace is wine's separator, so it goes; whatever is
        // left is the value, to the last byte.
        let value = value.trim_start();
        if value.is_empty() {
            return Reading::Other(
                "a value holding no path at all, which this installer never writes".to_string(),
            );
        }
        return Reading::Plain(value.to_string());
    }
    Reading::Absent
}

/// What the `Path` value under `key` says.
///
/// `reg query` exits non-zero both when there is nothing there and when wine
/// cannot serve this prefix at all, and the message it prints is localized
/// (`reg: Der angegebene Schlüssel wurde nicht gefunden` on this machine), so
/// the text cannot tell them apart either. A second query, of a key every
/// prefix has, can: if that one answers, the first key really is empty; if it
/// does not, the registry was not read at all, and calling that "nothing is
/// registered" would send the installer straight into overwriting whatever is
/// actually there.
fn read_key(wine: &Path, prefix: &Path, key: &str) -> Result<Reading, String> {
    let out = wine_output(wine, prefix, &["reg", "query", key, "/v", "Path"])
        .map_err(|e| format!("{e}"))?;
    if out.status.success() {
        return Ok(reg_query_path(&out.stdout));
    }
    let probe =
        wine_output(wine, prefix, &["reg", "query", PROBE_KEY]).map_err(|e| format!("{e}"))?;
    if probe.status.success() {
        return Ok(Reading::Absent);
    }
    Err(format!(
        "could not read the registry of {} with {}: `reg query {PROBE_KEY}` failed \
         too, so this is not \"nothing is registered\" but a prefix this wine \
         cannot serve.\nRefusing to touch keys whose current value could not be \
         read. {WRONG_WINE_HINT}",
        prefix.display(),
        wine.display()
    ))
}

/// Strip the root off one of [`KEYS`], for a reader that works inside one hive.
///
/// Both of them are `HKCU`, which is the whole reason [`read_keys_read_only`]
/// can exist: one hive, one file. A key that was not would come back `None`
/// rather than be looked for in the wrong file and reported absent — see
/// `both_discovery_keys_live_in_user_reg`, which is the test that stops a
/// future `HKLM` key from silently reading as "nothing is registered here".
fn hkcu_path(key: &str) -> Option<&str> {
    key.strip_prefix(r"HKCU\")
}

/// Everything [`KEYS`] holds, read out of the prefix's own `user.reg`.
///
/// **This runs no wine, and that is the point.** `wine reg query` is still
/// `wine`: it initialises the prefix it is pointed at before it runs anything,
/// and against a prefix whose `.update-timestamp` is stale — which is what a
/// Proton prefix looks like to the host's wine — it runs the `wineboot -u` that
/// [`WineOrigin`] exists to warn about, rewriting thousands of lines of
/// `system.reg` and stamping the prefix as its own. Measured: 2744 lines, and
/// on a prefix holding only `drive_c`, 5510 paths created. A command a user
/// runs to find out what is wrong must not be the thing that changes it, and
/// `status` is the command we are about to tell a user with a Proton flight-sim
/// prefix to run first.
///
/// Both keys are `HKCU` and a prefix keeps `HKCU` in `user.reg`, so the answer
/// is in one plain-text file — which also makes it independent of which wine
/// [`resolve_wine`] happened to pick.
///
/// **What it costs:** `user.reg` is the registry as last written back, and a
/// live wineserver holds changes in memory until the last process on the prefix
/// exits. So this can lag, and only when something is serving the prefix. The
/// report says so — see [`staleness`] — because a stale reading presented as
/// the current one would be a new way to mislead, which is the fault this
/// change is here to remove and not to relocate.
///
/// One `Err` for the whole file rather than one per key: there is one file, and
/// a failure to read it is a failure to answer either question. The keys that
/// *were* answered are still returned alongside it — nothing here is
/// `Reading::Absent` that was not read as absent.
fn read_keys_read_only(prefix: &Path) -> (Vec<(KeyEntry, Reading)>, Option<String>) {
    let file = prefix.join(crate::userreg::FILE);
    let text = match std::fs::read(&file) {
        Ok(t) => t,
        Err(e) => {
            return (
                Vec::new(),
                Some(format!(
                    "could not read {}: {e}\n\
                     That file is where a prefix keeps its HKEY_CURRENT_USER keys, so\n\
                     without it this report cannot say what these two hold — which is\n\
                     not the same as nothing being registered in them. A prefix that\n\
                     has never been started has no {} yet.",
                    file.display(),
                    crate::userreg::FILE
                )),
            );
        }
    };
    let mut out = Vec::new();
    for entry in KEYS {
        let (_, key, _) = entry;
        let Some(path) = hkcu_path(key) else {
            // Unreachable while both keys are HKCU, and a refusal rather than a
            // guess if one ever is not: the answer this must never invent is
            // "nothing is registered here".
            return (
                out,
                Some(format!(
                    "{key} is not under HKCU, so it is not in {} and this report\n\
                     cannot say what it holds.",
                    file.display()
                )),
            );
        };
        out.push((
            entry,
            match crate::userreg::lookup(&text, path, "Path") {
                crate::userreg::Lookup::Absent => Reading::Absent,
                crate::userreg::Lookup::Text(v) => Reading::Plain(v),
                crate::userreg::Lookup::Rejected(why) => Reading::Other(why),
            },
        ));
    }
    (out, None)
}

/// What this prefix's wineserver lock says, from the answer to where the lock
/// file is.
///
/// The Err-to-[`crate::wineserver::Lock::Unknown`] step in one place. Three
/// callers made it — [`settled_keys`], [`run`] and [`gather_status`] — and
/// they must not disagree about it: `Unknown` is the answer that sends
/// `settled_keys` to wine and that puts "could not tell" in the report, so a
/// caller that mapped a failed lookup to `Free` instead would have this
/// installer act on a registry a live wineserver is still holding changes to.
fn lock_state(found: &Result<PathBuf, String>) -> crate::wineserver::Lock {
    match found {
        Ok(path) => crate::wineserver::probe(path),
        Err(why) => crate::wineserver::Lock::Unknown(why.clone()),
    }
}

/// What [`KEYS`] hold according to the prefix's own files, when those files
/// settle it well enough to *act* on. `None` means "ask wine".
///
/// [`read_keys_read_only`] is what lets `status` report a prefix without
/// changing it. `install` and `uninstall` need the same read for a narrower
/// job: finding out, before any wine starts, whether this run is going to write
/// at all. A run that turns out to write nothing must not have booted the
/// prefix to discover that — `wine reg query` is still `wine`, and against a
/// prefix whose `.update-timestamp` is stale, which is what a Proton prefix
/// looks like to the host's wine, it runs the `wineboot -u` that [`WineOrigin`]
/// exists to warn about. Measured: an install that printed the refusal, created
/// no directory and wrote no key still moved the stamp and rewrote 2764 lines
/// of `system.reg`; so did an uninstall that removed nothing. The refusal text
/// warns the user that pasting that very wine binary would upgrade the prefix,
/// having just done it.
///
/// **`None` in the two cases where the file is not an answer this may act on:**
///
/// * It could not be read whole. A prefix that has never been started has no
///   `user.reg` at all, and an install is exactly the thing that would create
///   one.
/// * Something is serving the prefix, or whether anything is could not be
///   determined. A wineserver holds registry changes in memory until the last
///   process on the prefix exits, so while one is alive the file lags it —
///   [`staleness`] is where `status` discloses that and leaves it to the
///   reader. A command that *acts* cannot leave it to the reader, because both
///   directions of the lag do harm: a key the file calls free may hold a live
///   registration this run would then overwrite, and one it calls taken may
///   already be gone.
///
/// Which is the whole reason the check is here and not inside
/// [`read_keys_read_only`]: with the lock free, nothing has the prefix open, so
/// `user.reg` *is* the registry and a decision taken from it is the decision
/// wine would have given — at no cost to the prefix. Anything else falls
/// through to wine, which is accurate and which the caller is about to start
/// anyway.
///
/// **[LIMITATION]** A wineserver that starts between this probe and the write
/// is not seen. That window exists today with the wine read too — nothing here
/// holds the prefix — and it is not made worse by asking the lock first.
fn settled_keys(prefix: &Path) -> Option<Vec<(KeyEntry, Reading)>> {
    if lock_state(&crate::wineserver::lock_for(prefix)) != crate::wineserver::Lock::Free {
        return None;
    }
    let (readings, unreadable) = read_keys_read_only(prefix);
    // Every key or none: a partial answer is one this may not act on either,
    // and `read_keys_read_only` returns what it managed alongside the reason it
    // stopped.
    if unreadable.is_some() || readings.len() != KEYS.len() {
        return None;
    }
    Some(readings)
}

/// Is this the value this installer put in the key?
///
/// Two ways to be ours and no third. `C:\tobii-bridge` names a directory inside
/// this very prefix that nothing but this installer ever creates, so a key
/// pointing at it is ours whatever the record says — which is the only thing
/// that recognises a prefix installed before the record existed. The other is
/// the value [`RECORD_FILE`] says we wrote, which is how a TrackIR key we
/// pointed at a third-party client is told from that program's own registration
/// of the same DLL.
///
/// Compared case-insensitively, because Windows paths are and a prefix that
/// spells the drive letter the other way round is not a different registration.
/// Anything that is not a plain string is not ours: this installer has never
/// written one.
///
/// **[LIMITATION]** A third-party client that registers itself at exactly the
/// path we pointed the key at, *after* we pointed it there, is indistinguishable
/// from our own work and comes out on the way out. A value-equality test cannot
/// see the difference; the alternative — remembering the value that was there
/// before — is what this module stopped doing, and for far worse failures.
///
/// The same blindness reaches one step earlier, in [`install`]: a key that
/// already holds exactly the path this run would write, with no record saying
/// anything either way, is not refused over — and is then recorded as ours, so
/// a registration opentrack made for itself in a prefix we had never touched
/// comes out on our way out. Refusing instead would refuse every upgrade from
/// v0.4.0, which wrote that value and recorded nothing, on every machine with
/// opentrack installed. Neither answer can tell the two apart; this one at
/// least fails towards a prefix the user can re-register in one command.
fn is_ours(current: &Reading, wrote: Option<&str>) -> bool {
    let Reading::Plain(v) = current else {
        return false;
    };
    v.eq_ignore_ascii_case(INSTALL_WIN_DIR) || wrote.is_some_and(|w| v.eq_ignore_ascii_case(w))
}

/// **Does `install` stop on this key?**
///
/// The question itself, in one function, because two commands act on the
/// answer: `install`'s own loop, which refuses over every key this says yes
/// to, and `status`, whose closing line tells the user whether `install` will
/// go through. They used to ask it separately — `install` here, `status` by
/// reading its own [`KeyState`] classification — and the two drifted apart at
/// the one state the classification cannot see.
///
/// That state is the third arm below. A key holding, byte for byte, what this
/// run would write, with nothing recorded either way, is not refused over; and
/// what "what this run would write" *is* depends on `--npclient`, which the
/// report had no way to know. So a prefix where opentrack had registered its
/// own client — the configuration this project recommends for TrackIR, and the
/// state v0.4.0 leaves behind — was told "`install` will not put it there
/// while these hold something this installer did not write", over an install
/// that then walked straight past the same key. The report and the command
/// disagreed about the same prefix, and the report is the half a user pastes
/// into an issue.
///
/// Sharing the function is what stops that recurring: there is no second copy
/// to fall behind, and `status` now reads `--npclient` so it can supply the
/// same `want`.
fn stops_install(current: &Reading, wrote: Option<&str>, want: &str) -> bool {
    if *current == Reading::Absent || is_ours(current, wrote) {
        return false;
    }
    // Nothing here to refuse over: the key already holds, byte for byte, what
    // this run would put in it, and nothing records this installer ever
    // putting anything else there. That is precisely the state v0.4.0 leaves
    // behind on a machine with opentrack installed — it registered the
    // third-party path and kept no record of doing so — so refusing here
    // refuses the upgrade path itself, over a write that would change nothing,
    // in a sentence blaming another program for a value this program wrote.
    //
    // A record naming a *different* value is a different fact and is still
    // refused, identical value or not: that is positive evidence the key
    // changed hands since we wrote it, which makes the match a reason to leave
    // it alone rather than to proceed — somebody else put it there.
    if wrote.is_none() && matches!(current, Reading::Plain(v) if v.eq_ignore_ascii_case(want)) {
        return false;
    }
    true
}

/// Render the record: one `<key> wrote <path>` line per key.
///
/// The value is the rest of the line and is never quoted or escaped, so a path
/// containing spaces survives a round trip. A key with no line is a key we have
/// no claim on, which is the safe reading and so the right default.
fn render_record(entries: &[(&str, &str)]) -> String {
    let mut out = String::from(
        "# What `tobii bridge install` last wrote into this prefix's FreeTrack\n\
         # and TrackIR discovery keys. `tobii bridge uninstall` removes those\n\
         # values only while the keys still say exactly this, and leaves\n\
         # anything else exactly as it is. Nothing here was read out of the\n\
         # registry: every line is a path this installer computed itself.\n\
         # Lines are `<key> wrote <path>`.\n",
    );
    for (name, wrote) in entries {
        out.push_str(&format!("{name} wrote {wrote}\n"));
    }
    out
}

/// Read back what [`render_record`] wrote.
///
/// A line that is not understood is dropped rather than guessed at: a record
/// that cannot be read is the same situation as no record, and the answer to
/// that is to leave the key alone.
fn parse_record(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with('#') {
            continue;
        }
        let mut field = line.splitn(3, ' ');
        let (Some(name), Some("wrote"), Some(wrote)) = (field.next(), field.next(), field.next())
        else {
            continue;
        };
        if name.is_empty() || wrote.is_empty() {
            continue;
        }
        upsert(&mut out, name, wrote.to_string());
    }
    out
}

/// What the record says this installer wrote into the key called `name`.
fn recorded<'a>(record: &'a [(String, String)], name: &str) -> Option<&'a str> {
    record
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, w)| w.as_str())
}

/// Put `value` in the record's line for `name`, adding the line if there is
/// none.
///
/// The last line wins rather than the first, which is what makes a record with
/// a key named twice read the same as one written afresh.
fn upsert(record: &mut Vec<(String, String)>, name: &str, value: String) {
    match record.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = value,
        None => record.push((name.to_string(), value)),
    }
}

/// The record kept in `dir`, empty when there is none.
fn read_record(dir: &Path) -> Vec<(String, String)> {
    parse_record(&std::fs::read_to_string(dir.join(RECORD_FILE)).unwrap_or_default())
}

/// The name a file is staged under before the rename that publishes it.
///
/// Per process, not one fixed name per file. The rename is atomic; the write
/// into the staging file is not, so two installs into one prefix sharing a
/// staging name interleave their writes and then rename a torn file into place
/// — the very thing the rename is here to prevent.
///
/// The record and the DLLs both go through here. The DLLs used to stage under
/// one fixed `.<name>.new` each, and the hazard was worse there than for the
/// record: two `tobii bridge install --prefix P` runs with different
/// `--artifacts` directories (a rebuild in one terminal, a packaged binary in
/// the other) both `fs::copy` into the same staging file, one renames the
/// mixture onto the DLL the game loads — and the loser, whose descriptor now
/// points at the published file, goes on writing into the live DLL with no
/// staging left between it and the target.
fn staging_name(name: &str, pid: u32) -> String {
    format!(".{name}.{pid}.new")
}

/// Write down what this run put in the keys.
///
/// Rewritten whole, from the merge of what was already recorded with what this
/// run actually wrote — see the call site for why a key this run failed to
/// write keeps whatever an earlier run recorded for it. Staged and renamed,
/// like the DLLs and for the same reason: a half-written record is read back as
/// a record with a line missing, and a missing line means "nothing here says we
/// wrote that key", whose action on the way out is to leave it alone.
fn write_record(dir: &Path, entries: &[(&str, &str)]) -> Result<(), String> {
    let path = dir.join(RECORD_FILE);
    let staged = dir.join(staging_name(RECORD_FILE, std::process::id()));
    std::fs::write(&staged, render_record(entries))
        .and_then(|()| std::fs::rename(&staged, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&staged);
            format!(
                "could not write {} ({e}) — the keys were registered, so the install \
                 itself works, but nothing in this prefix now records what was put \
                 there. `tobii bridge uninstall` takes out only the values it can \
                 still recognise as its own, and a TrackIR key pointed at a \
                 third-party client is not one of them. Fix that path and install \
                 again.",
                path.display()
            )
        })
}

/// The Wine spelling of a client directory, refused when we could not read it
/// back out of the registry afterwards.
///
/// This is the symmetry the refusals rest on: what we decline to read from
/// another program, we decline to write ourselves. Wine's `reg.exe` prints the
/// registry in the console's OEM codepage, so a `Path` holding a character
/// outside ASCII comes back mangled — and a value we cannot read back is one we
/// can never recognise as ours again. Confirmed against wine 11.18: a client
/// under `…/ゲーム/opentrack` is stored correctly and reads back as
/// `Z:\…\???\opentrack`, which is pure ASCII and so looks exactly like another
/// program's registration. Install would then refuse over its own work, and
/// uninstall would leave it behind for good.
///
/// A line break is the second such character and it arrived later. A Linux
/// directory name may hold one, `is_ascii()` accepts it because LF *is* ASCII,
/// and wine stores and prints it back raw — so [`reg_query_path`] splits the
/// output on it and answers [`Reading::Other`], "a value with a line break in
/// it". From then on the key is unrecognisable in exactly the way above: every
/// later install refuses over its own work, and uninstall orphans it for good.
///
/// A CR goes with it, though a lone one does read back whole today. This guard
/// is deliberately wider than the demonstrated failure, exactly as the ASCII
/// half is — most non-ASCII paths would survive a round trip too, and it
/// refuses them all. A CR is the byte wine terminates its lines with, so a rule
/// that admitted one would have to reason about which half of a break arrived,
/// and reasoning about which half arrived is what was wrong with the reader's
/// own guard before this. There is no cost: no real client directory is spelled
/// with either, and the refusal names the flag that gets past it.
///
/// So it is refused before anything is created, rather than written and
/// regretted.
fn registrable_path(dir: &Path) -> Result<String, String> {
    let win = wine_path_for(dir);
    if !win.is_ascii() || win.contains(['\r', '\n']) {
        return Err(format!(
            "{} cannot be registered: wine prints the registry in the console's own \
             codepage rather than UTF-8, so a path with a character outside ASCII — \
             or one carrying a line break, which wine prints raw and this installer \
             then cannot read back whole — does not read back the way it was written, \
             and a value this installer cannot read back is one it could never tell \
             from another program's.\n\
             Move the client somewhere spelled in plain ASCII on one line, or pass \
             `--npclient ours` to register our own DLL instead.",
            dir.display()
        ));
    }
    Ok(win)
}

/// Which TrackIR client this run would register, and the value that puts in
/// [`NP_KEY`].
///
/// Settled from the arguments alone, before the registry is looked at, so that
/// a path we could not read back is refused before anything exists.
///
/// Asked by `install`, which writes it, and by `status`, which reports whether
/// `install` would go through — and that report is only true of the install
/// the same flags describe. Which is why `status` reads `--npclient` at all:
/// without it the report had to guess what the TrackIR key would be compared
/// against, and guessed the one value that makes the answer wrong.
fn np_target_for(args: &[String]) -> Result<(NpSource, String), String> {
    let np_source = choose_npclient(
        crate::flag_value(args, "--npclient"),
        find_installed_npclient(),
    )?;
    let target = match &np_source {
        // FreeTrack always gets our own DLL: that ABI has no signature check,
        // so nothing stands between it and our data.
        NpSource::Ours => INSTALL_WIN_DIR.to_string(),
        NpSource::Installed(dir) => registrable_path(dir)?,
    };
    Ok((np_source, target))
}

/// What this run would write into `key`, given the TrackIR client it settled
/// on.
///
/// One key varies and one does not, and both halves live here rather than
/// inlined at each caller: it is the value [`stops_install`] compares against,
/// so a second spelling of it is a second answer to the question that defect
/// was about.
fn want_for<'a>(key: &str, np_target: &'a str) -> &'a str {
    if key == NP_KEY {
        np_target
    } else {
        INSTALL_WIN_DIR
    }
}

/// A path, quoted for the shell the user is going to paste it into.
fn shell_quoted(p: &Path) -> String {
    quoted(&p.display().to_string())
}

/// Anything else that goes into a command line we hand the user to paste.
///
/// Every argument of such a command, not just the paths. `--steam "Microsoft
/// Flight Simulator 2024"` was interpolated bare, so the undo line came out as
/// `tobii bridge uninstall --steam Microsoft Flight Simulator 2024 …` — which
/// pasted back answers `"Microsoft" matches more than one game`, the trailing
/// words having been silently dropped as positionals. The one line in the
/// report whose entire job is to survive a round trip through the clipboard has
/// to survive a title with a space in it, and Steam titles have spaces.
fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// One key install will not write into, and everything the refusal is allowed
/// to say about it.
struct Taken {
    /// Whose key it is, in the name the user knows it by.
    abi: &'static str,
    key: &'static str,
    /// What is there now — as much of it as could be read.
    what: String,
    /// What this run would have put there. Not always different from `what`:
    /// see [`replaced`].
    want: String,
    /// Whether this prefix's record says this installer wrote this key.
    recorded: bool,
}

/// Whose a key holding something we did not write is — said once, because
/// install on the way in and [`undo_for`] on the way out give the same two
/// answers and rest on the same two facts.
///
/// Naming another program is a claim, and the record is the only evidence for
/// it: it says this installer wrote something else here, so somebody changed
/// the key since. With no record there is no such evidence. Every prefix
/// installed before the record existed arrives here with none, and telling
/// those users another program took their key is an accusation about a value
/// this program itself wrote.
fn whose(recorded: bool) -> &'static str {
    if recorded {
        "it no longer holds what this install wrote, so it is another program's now"
    } else {
        "nothing here records what this install wrote, so there is no telling whose it is"
    }
}

/// The refusal install answers with when a key holds something this run did not
/// write.
///
/// Says what is there — as much of it as could be read, and no cause it has not
/// established — names the flag that goes ahead anyway and what that flag does
/// *not* promise, and spells the clearing command with this prefix and the wine
/// this run resolved already in it. That last part is not politeness: a bare
/// `wine` reaches `~/.wine` rather than the prefix in question, and a different
/// wine build touching a prefix runs `wineboot -u` and upgrades it — for a
/// Proton prefix, out from under the game that owns it. Resolving the right wine
/// is most of what this module does; handing the user `wine reg delete …` as the
/// way out would undo it in one paste.
///
/// Which is exactly why `origin` is a parameter and not an assumption. This
/// used to call the quoted binary "this prefix's own wine" unconditionally,
/// while [`choose_wine`] falls back to `$PATH` when the prefix names none — so
/// for a Proton prefix whose `config_info` could not be read, the sentence
/// warning against the upgrade was printed over the command that performs it.
/// The claim is made only where [`WineOrigin::Prefix`] says it was established.
fn refusal(prefix: &Path, wine: &Path, origin: WineOrigin, taken: &[Taken]) -> String {
    let mut msg = String::from(
        "these head-tracking discovery keys hold something this install did not \
         write:\n",
    );
    for t in taken {
        msg.push_str(&format!(
            "  {:<9} {}\n            {}\n            {}\n",
            t.abi,
            t.key,
            t.what,
            whose(t.recorded)
        ));
    }
    msg.push_str(
        "Overwriting that would break whatever is using it, and for a game that\n\
         checks NaturalPoint's signature the client registered there may be the one\n\
         that WORKS — ours is the one it rejects.\n\
         Pass --force to register ours anyway. --force promises nothing about putting\n\
         that back: this installer writes down only the values it wrote itself, so\n\
         what is there now is gone for good.\n\
         If it is stale, clear it yourself:\n",
    );
    for t in taken {
        msg.push_str(&format!(
            "  WINEPREFIX={} {} reg delete '{}' /v Path /f\n",
            shell_quoted(prefix),
            shell_quoted(wine),
            t.key
        ));
    }
    msg.push_str(match origin {
        WineOrigin::Prefix => {
            "That wine is the one this prefix itself records, and it is the one to use:\n\
             a different build runs `wineboot -u` against a prefix it does not own and\n\
             upgrades it out from under the game."
        }
        WineOrigin::Unverified => {
            "Check that wine before you paste it. It is the one this run used, not one\n\
             this prefix records — nothing here established that it is the build this\n\
             prefix belongs to, and a different build runs `wineboot -u` against a\n\
             prefix it does not own and upgrades it out from under the game. For a\n\
             Steam title the right one is the Proton build the prefix records."
        }
    });
    msg
}

/// What `--force` replaced, in the words `--force` was given in: what was
/// there is gone, and nothing here will bring it back.
///
/// Only what was actually replaced. A key that already held, byte for byte, the
/// value this run then wrote lost nothing — and "nothing puts that back" about
/// a value still sitting in the key sends the user looking for a loss that
/// never happened.
fn replaced(taken: &[Taken]) -> String {
    let mut out = String::new();
    for t in taken {
        if t.what.eq_ignore_ascii_case(&t.want) {
            continue;
        }
        out.push_str(&format!(
            "replaced what {} had registered: {} — nothing puts that back\n",
            t.abi, t.what
        ));
    }
    out
}

/// Whether [`refuse_unverified_wine_for_steam`] refuses, as a question rather
/// than as a refusal.
///
/// Split out for the reason [`stops_install`] is split out: `status` has to
/// say what `install` will do with this prefix, and the only way to say it
/// without keeping a second answer that can drift from the first is to ask the
/// same function. The report modelled the per-key refusal that way and this
/// one not at all, so it handed back an install command that stops before it
/// reads a key — with this refusal's own text pointing the user back at the
/// report that printed it.
///
/// The two exemptions arrive as facts rather than as an `args` slice because
/// `status` has no `--force` to pass: it is not one of its flags — see
/// [`SUBS`] — so the report asks with `forced: false` and gets the answer for
/// the command it is about to print.
fn steam_wine_refused(
    source: &PrefixSource,
    origin: WineOrigin,
    wine_named: bool,
    forced: bool,
) -> bool {
    matches!(source, PrefixSource::Steam(_))
        && origin != WineOrigin::Prefix
        && !wine_named
        && !forced
}

/// A `--steam` prefix whose own runner could not be resolved, about to be
/// written by whatever `wine` is on `$PATH`: refuse, and say what would have
/// happened.
///
/// # Why this one is refused and not merely warned about
///
/// [`choose_wine`]'s last tier is the system wine, and for a `--steam` prefix
/// that tier is not a fallback but a known-wrong answer. The prefix was found
/// through Steam's `compatdata`, so it was made by a Proton build by
/// construction, and the host's wine is by construction not that build.
/// Running it is not the risk that the game may see no tracking; it is
/// `wineboot -u` rewriting the game's prefix — measured: the stamp taken over,
/// `system.reg` replaced, four Windows processes spawned, and nine minutes of
/// silence before the run was killed. Nothing puts that back, and what the
/// command wanted was to write two registry values.
///
/// # Why only here
///
/// Refusing an `install` costs the user nothing: nothing of ours is in the
/// prefix yet, so stopping leaves it exactly as it was, and both ways past are
/// one flag. The same refusal on `uninstall` would strand somebody trying to
/// take our files back *out* of their own prefix, which is the one direction
/// where stopping leaves it worse than proceeding; and `run` is a user asking
/// in so many words to start wine on that prefix. Those two print
/// [`choose_wine`]'s warning, which now names this same damage, and go on.
///
/// An explicit `--wine` is never refused: the user named the binary, and this
/// function knows nothing they do not.
fn refuse_unverified_wine_for_steam(
    source: &PrefixSource,
    origin: WineOrigin,
    wine: &Path,
    args: &[String],
) -> Result<(), String> {
    let PrefixSource::Steam(title) = source else {
        return Ok(());
    };
    if !steam_wine_refused(
        source,
        origin,
        crate::flag_value(args, "--wine").is_some(),
        args.iter().any(|a| a == "--force"),
    ) {
        return Ok(());
    }
    Err(format!(
        "this is a Proton prefix — Steam found it for `--steam {t}` — but which \
         Proton build made it could not be read out of it, so the only wine left \
         is {w}.\n\
         Refusing to run that one. A wine that is not the build a prefix was made \
         with runs `wineboot -u` against it before it does anything else and \
         upgrades it: the prefix's stamp taken over and its `system.reg` rewritten, \
         out from under the game that owns it. Nothing here puts that back, and the \
         two registry values this command came to write are not worth it.\n\
         Name the build the game uses:\n  \
         tobii bridge install --steam {t} --wine <Proton>/files/bin/wine\n\
         `tobii bridge status --steam {t}` runs nothing and names the prefix; the \
         Proton build is the one Steam lists under the title's compatibility \
         setting, under `steamapps/common`.\n\
         Or pass --force to go ahead with {w} anyway, knowing it will upgrade the \
         prefix first.",
        t = quoted(title),
        w = wine.display()
    ))
}

/// `tobii bridge install` — copy the artifacts in and register them.
fn install(args: &[String]) -> CmdResult {
    let (prefix, source) = resolve_prefix(args)?;
    // [`resolve_wine_reporting`] rather than [`resolve_wine`], only so the
    // refusal below can come *instead of* the warning rather than after it:
    // both say the same thing about the same binary, and printing the milder
    // one first teaches the reader to skip the one that stopped the command.
    let (wine, wine_origin, wine_warning) = resolve_wine_reporting(&prefix, args)?;
    refuse_unverified_wine_for_steam(&source, wine_origin, &wine, args)?;
    if let Some(w) = wine_warning {
        eprintln!("warning: {w}");
    }
    let src = artifact_dir(args)?;
    let dest = prefix.join(INSTALL_SUBDIR);

    // Which client TrackIR is pointed at decides what its key should say, so it
    // is settled before the registry is looked at rather than in the middle of
    // writing it — and refused here, before anything exists, if it is a path we
    // could not read back.
    let explicit_np = crate::flag_value(args, "--npclient");
    let (np_source, np_target) = np_target_for(args)?;

    // Read before write, and before anything is copied. A prefix that already
    // has a working head-tracking setup has to come out of a refusal exactly as
    // it went in — no directory created, no key touched, and nothing started
    // that would boot it.
    //
    // The record is read first too, because what this installer wrote here last
    // time is part of reading the key honestly: without it our own previous
    // registration of a third-party client looks exactly like a stranger's.
    let record = read_record(&dest);
    // The prefix's own `user.reg` where it can settle this, and wine only where
    // it cannot — because a refusal that ran wine rewrote the prefix it was
    // refusing to touch. See [`settled_keys`]; the wine read below is the
    // accurate one and is reached only on the way to writing.
    let readings = match settled_keys(&prefix) {
        Some(readings) => readings,
        None => {
            let mut v = Vec::with_capacity(KEYS.len());
            for entry in KEYS {
                v.push((entry, read_key(&wine, &prefix, entry.1)?));
            }
            v
        }
    };
    let mut taken: Vec<Taken> = Vec::new();
    for (entry, current) in &readings {
        let (name, key, abi) = *entry;
        let wrote = recorded(&record, name);
        let want = want_for(key, &np_target);
        // The one question, asked in the one place — see [`stops_install`].
        // `status` asks that same function about the same key, which is what
        // stops the report and this loop disagreeing again.
        if !stops_install(current, wrote, want) {
            continue;
        }
        taken.push(Taken {
            abi,
            key,
            what: current.describe().to_string(),
            want: want.to_string(),
            recorded: wrote.is_some(),
        });
    }
    if !taken.is_empty() && !args.iter().any(|a| a == "--force") {
        return Err(refusal(&prefix, &wine, wine_origin, &taken).into());
    }

    std::fs::create_dir_all(&dest)?;

    let mut copied = 0;
    for (name, required) in ARTIFACTS {
        let from = src.join(name);
        if !from.is_file() {
            if required {
                return Err(format!("{} is missing from {}", name, src.display()).into());
            }
            eprintln!("note: {name} not built yet — skipping");
            continue;
        }
        // Written beside the target and renamed, never copied over in place.
        // A game that is running has the DLL mapped; `fs::copy` onto it either
        // fails outright or, if the loader lets it through, leaves a torn file
        // that the next launch loads. `rename` within one directory is atomic,
        // so the old DLL stays whole until the instant the new one replaces it.
        //
        // Under a per-process name, for the reason [`staging_name`] gives: a
        // fixed one puts two concurrent installs in the same staging file and
        // hands the game the mixture.
        let target = dest.join(name);
        let staged = dest.join(staging_name(name, std::process::id()));
        std::fs::copy(&from, &staged)?;
        if let Err(e) = std::fs::rename(&staged, &target) {
            let _ = std::fs::remove_file(&staged);
            return Err(format!("could not replace {}: {e}", target.display()).into());
        }
        copied += 1;
        println!("  {name}");
    }
    println!("copied {copied} file(s) into {}", dest.display());
    // Said at install time, not only in the docs: a 32-bit game's failure to
    // find a DLL is indistinguishable from every other "no tracking" cause.
    println!("  (64-bit games only — a 32-bit game looks for freetrackclient.dll)");

    let ft_landed = set_key(&wine, &prefix, FT_KEY, INSTALL_WIN_DIR)?;
    let np_landed = set_key(&wine, &prefix, NP_KEY, &np_target)?;
    let all_written = ft_landed && np_landed;

    // Written AFTER the keys, and naming only what actually landed in one.
    //
    // It used to go first, on the reasoning that a record of a write that then
    // failed is harmless because the key does not hold that value. That stops
    // being true the moment something else writes it. An install whose
    // `reg add` failed still left `np wrote <third-party path>` behind;
    // opentrack then registered that same path for itself — the directory
    // `--npclient` picks by default is opentrack's own — and `tobii bridge
    // uninstall` read the record, found the key holding "what we wrote", and
    // deleted somebody else's registration. No --force, and no write of ours
    // ever landed. A record is a record of what happened, not of what was
    // meant to.
    //
    // Merged into what was already there rather than written whole: a key this
    // run could not write may still hold what an earlier run put there, and
    // dropping that line would abandon a claim that is still true.
    let landed = |key: &str| if key == NP_KEY { np_landed } else { ft_landed };
    let mut kept = record;
    for (name, key, _) in KEYS {
        if !landed(key) {
            continue;
        }
        upsert(&mut kept, name, want_for(key, &np_target).to_string());
    }
    if ft_landed || np_landed {
        let entries: Vec<(&str, &str)> =
            kept.iter().map(|(n, w)| (n.as_str(), w.as_str())).collect();
        write_record(&dest, &entries)?;
    }

    // Held rather than printed as we go: "registered X" is only true once every
    // write has landed, and printing it before the check put confident lines
    // above the failure that contradicted it.
    let registered = registered_text(
        &np_source,
        np_target.as_str(),
        explicit_np == Some("ours"),
        find_installed_npclient().as_deref(),
    );
    // Load-bearing, and easy to miss: a third-party client is a pure consumer
    // of FT_SharedMem. Our DLLs create and feed that mapping, and a
    // TrackIR-only game never loads ours — so this is the one configuration
    // that still needs the provider running.
    let third_party_np = matches!(np_source, NpSource::Installed(_));

    // Nothing below here is true if the keys did not land, so it is not
    // printed. A game finds its client DLL through the registry and nowhere
    // else — copied files alone install nothing.
    if !all_written {
        return Err(format!(
            "the registry keys could not be written, so the DLLs are copied but \
             nothing will load them.\n\
             The wine used was {}. {WRONG_WINE_HINT}",
            wine.display()
        )
        .into());
    }

    println!("{registered}");
    // Only reached with --force.
    print!("{}", replaced(&taken));
    println!();
    if third_party_np {
        println!(
            "For TrackIR through that third-party client you must also run:\n  \
             tobii bridge run --prefix {}\n\
             It is a plain consumer of the shared memory our own DLLs create, and a\n\
             TrackIR-only game never loads ours — so something has to fill it.\n\
             \n\
             Start the game FIRST and that command second. While it runs it is a\n\
             wineserver on this prefix, and Steam waits for every wineserver on a\n\
             prefix to exit before it spawns the game — so a bridge started first\n\
             leaves the launch sitting there. The command says this before it starts,\n\
             and stops itself if a launch starts waiting behind it.\n\
             FreeTrack games need nothing running.",
            prefix.display()
        );
    } else {
        println!(
            "Nothing else to run. The DLL receives tracking itself, inside the game's\n\
             own process — no second program, no prefix to match.\n\n\
             Turn game output on (the hub's switch, or `tobii games set enabled true`),\n\
             then launch the game through the wrapper so the tracker comes on:\n  \
             tobii game -- %command%      (Steam: paste into Launch Options)"
        );
    }
    Ok(())
}

/// Write one discovery key, reporting whether it actually landed.
///
/// The return value is load-bearing. This used to warn and return `Ok(())`, so
/// a wine that ran but could not serve the prefix produced two warning lines
/// followed by "registered …", the whole "now launch the game" paragraph and
/// exit 0 — four confident lines burying the two that mattered. The registry
/// path IS the installation: without it a game never finds the DLL, so a failed
/// write is a failed install and has to be reported as one.
///
/// Always `REG_SZ`, because the only thing ever written here is a path this
/// program computed. Nothing found in a key is ever written back, so no other
/// type has to be spellable.
fn set_key(wine: &Path, prefix: &Path, key: &str, dir: &str) -> Result<bool, String> {
    match reg_write(
        wine,
        prefix,
        &[
            "reg", "add", key, "/v", "Path", "/t", "REG_SZ", "/d", dir, "/f",
        ],
    )? {
        None => Ok(true),
        Some(said) => {
            eprintln!("warning: could not write {key}");
            eprint!("{said}");
            Ok(false)
        }
    }
}

/// Run one `reg` write, and say in wine's own words why it did not land.
///
/// `None` is a write that landed and had nothing to report. `Some(said)` is
/// one that did not, carrying whatever wine printed.
///
/// Both writes go through here for the reason [`wine_output`] already existed
/// for on the read side. They used to inherit this process's stdio, so a real
/// install printed `WARNING: radv is not a conformant Vulkan implementation`
/// and two copies of `reg: Der Vorgang wurde erfolgreich abgeschlossen` —
/// wine's own success line, in the user's locale — in between "copied 3
/// file(s) into …" and "registered C:\tobii-bridge for TrackIR and FreeTrack".
/// This output is written to be pasted into an issue; untranslated noise
/// between two of our own lines is noise a triager has to learn to skip.
///
/// Kept, rather than dropped, for the failure: then it is the only thing that
/// says why, and the caller prints it under its own warning.
fn reg_write(wine: &Path, prefix: &Path, args: &[&str]) -> Result<Option<String>, String> {
    let out = wine_output(wine, prefix, args).map_err(|e| format!("{e}"))?;
    if out.status.success() {
        return Ok(None);
    }
    Ok(Some(wine_said(&out)))
}

/// Everything a wine command printed, on both streams, indented.
///
/// Indented so it cannot be mistaken for one of our own lines, and blank lines
/// dropped, because `reg.exe` frames its one sentence in them.
fn wine_said(out: &std::process::Output) -> String {
    let mut said = String::new();
    for stream in [&out.stdout, &out.stderr] {
        for line in String::from_utf8_lossy(stream).lines() {
            let line = line.trim_end();
            if !line.trim().is_empty() {
                said.push_str(&format!("  {line}\n"));
            }
        }
    }
    said
}

/// Remove the `Path` value this installer added, and only that.
///
/// `/v Path`, not `reg delete <key> /f`: the second removes the key and every
/// value under it, and install may well have added `Path` to a key another
/// program created and keeps its own settings in. Checked against wine 11.18 —
/// deleting the value leaves the key's other values in place.
///
/// A value that is already gone is success. Wine's `reg delete` exits 1 when
/// there is no `Path` to delete (checked against the same build), so a
/// concurrent uninstall, or a user who cleared the key by hand between the read
/// above and this write, would otherwise fail the command and keep the install
/// directory — for a prefix that is already in exactly the state being asked
/// for. Re-read rather than assume: a delete that failed for any other reason
/// still has to be reported.
fn del_value(wine: &Path, prefix: &Path, key: &str) -> Result<bool, String> {
    let Some(said) = reg_write(wine, prefix, &["reg", "delete", key, "/v", "Path", "/f"])? else {
        return Ok(true);
    };
    if matches!(read_key(wine, prefix, key), Ok(Reading::Absent)) {
        return Ok(true);
    }
    eprintln!("warning: could not remove Path from {key}");
    eprint!("{said}");
    Ok(false)
}

/// How a supervised child ended.
#[derive(Debug)]
enum Supervised {
    /// It finished on its own, with this status.
    Exited(std::process::ExitStatus),
    /// We stopped it because a launch was blocked behind it, by this pid.
    Yielded(i32),
}

/// How often the watch loop looks for a blocked launch.
///
/// The thing being waited on is a `wineserver -w` that waits forever, so
/// latency here is only how long the user stares at a launcher that has not
/// started yet. A quarter of a second is short enough not to be noticed and
/// long enough that reading one small proc file costs nothing.
const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Run `child` to completion, unless a launch starts waiting on the prefix.
///
/// `waiter` is a parameter rather than a direct `/proc/locks` read so that the
/// yield path — the part that kills a live process — is testable against a real
/// child without needing a real blocked `fcntl` waiter, which cannot be staged
/// from inside a threaded test binary.
fn supervise(
    child: &mut std::process::Child,
    mut waiter: impl FnMut() -> Option<i32>,
) -> std::io::Result<Supervised> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Supervised::Exited(status));
        }
        if let Some(pid) = waiter() {
            stop(child);
            return Ok(Supervised::Yielded(pid));
        }
        std::thread::sleep(WATCH_INTERVAL);
    }
}

/// Ask the child to go, then insist.
///
/// `SIGTERM` first because wine turns it into an ordinary process termination:
/// the Windows process exits, wine detaches from the wineserver, and the
/// wineserver — which holds the lock for exactly as long as it has clients —
/// goes with it. `SIGKILL` straight away would get there too, but only after
/// wine's own cleanup did not happen, and the point of this path is to leave
/// the prefix in the state a launch is about to walk into.
///
/// The child is deliberately NOT put in a process group of its own. It shares
/// ours, so a Ctrl-C at the terminal still reaches wine; a child in its own
/// group would survive the interrupt that kills this process and go on holding
/// the lock with nothing left to stop it — the exact failure this whole module
/// is here to prevent, made permanent.
fn stop(child: &mut std::process::Child) {
    // SAFETY: a pid this process owns and has not reaped — only `try_wait` and
    // `wait` reap it, and both are here. A child that died a moment ago is
    // still a zombie holding its pid, so the signal lands on nothing rather
    // than on somebody else's process.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(_) => return,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// `tobii bridge run` — run the provider in the foreground.
///
/// This command is a wine process on the game's prefix, which makes it a
/// wineserver holder by construction, which makes it able to hang the next
/// Steam launch of that game — see [`crate::wineserver`] for the mechanism and
/// for what is measured versus assumed about it. Two things follow from that
/// and they are the only reason this is not three lines:
///
/// * it says, before it starts, what starting it now will cost;
/// * it gets out of the way when a launch does start waiting behind it.
///
/// Neither makes a game *accept* the tracking data. Both stop a second process
/// from breaking the launch, and nothing said here may claim more.
fn run(args: &[String]) -> CmdResult {
    let (prefix, _) = resolve_prefix(args)?;
    let (wine, _) = resolve_wine(&prefix, args)?;
    let exe = prefix.join(INSTALL_SUBDIR).join("tobii-bridge.exe");
    if !exe.is_file() {
        return Err(format!(
            "the bridge is not installed in {} — run `tobii bridge install` first",
            prefix.display()
        )
        .into());
    }
    // `--no-register` always, never conditionally. `install` settled both
    // discovery keys under the read-before-write contract this module's header
    // describes, and the provider's own blind write knows none of it: in the
    // one configuration that needs this command at all — TrackIR pointed at a
    // third-party client — a write here replaces that client's registration
    // with ours and quietly takes away the thing the user came for.
    let mut wine_args = vec![r"C:\tobii-bridge\tobii-bridge.exe", "--no-register"];
    if let Some(port) = crate::flag_value(args, "--port") {
        wine_args.push("--port");
        wine_args.push(port);
    }

    let lock = crate::wineserver::lock_for(&prefix);
    eprint!("{}", crate::wineserver::before_run(&lock_state(&lock)));
    eprintln!(
        "\nrunning the bridge in {} (Ctrl-C to stop)",
        prefix.display()
    );

    let mut child = wine_cmd(&wine, &prefix).args(&wine_args).spawn()?;

    // Asked for again on every pass rather than once, because the usual case is
    // that the lock file does not exist yet: nothing has served this prefix, so
    // our own wine is about to create it, moments from now.
    let lock = lock.ok();
    let mut ids = lock.as_deref().and_then(crate::wineserver::lock_ids);
    let outcome = supervise(&mut child, || {
        if ids.is_none() {
            ids = lock.as_deref().and_then(crate::wineserver::lock_ids);
        }
        let (dev, ino) = ids?;
        crate::wineserver::waiting_launch(dev, ino)
    })?;

    match outcome {
        Supervised::Yielded(pid) => {
            eprint!("{}", crate::wineserver::yielding(pid));
            Ok(())
        }
        Supervised::Exited(status) if !status.success() => {
            Err(format!("the bridge exited with {status}").into())
        }
        Supervised::Exited(_) => Ok(()),
    }
}

/// What uninstall should do to one key, having read what it says now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Undo {
    /// The key still holds what this installer wrote, so what it added comes
    /// out — the `Path` value, not the key.
    Remove,
    /// Nothing is registered there at all: nothing to do, and nothing to warn
    /// anybody about.
    Nothing,
    /// Not ours to touch, and why — which the user is told, because residue
    /// left deliberately and residue left by a bug look identical otherwise.
    Leave(&'static str),
}

/// Decide from what the key says *now*, never from the record alone.
///
/// Install reads before it writes; so does this. The record says what we left
/// behind, but between then and now the user may have installed opentrack,
/// switched trackers, or cleared the key by hand — and a `reg delete` aimed at
/// the value we left then deletes whatever replaced it. A key that no longer
/// says what we wrote belongs to somebody else, and the only safe thing to do
/// with it is nothing.
///
/// Which of the two [`whose`] answers a left key gets turns on whether there is
/// a record at all, and that matters most here: every prefix installed before
/// this record existed arrives with none, and telling those users a key "no
/// longer holds what this install wrote" claims a comparison that never
/// happened. Their own pointer at `C:\tobii-bridge` is still recognised —
/// [`is_ours`] knows that directory without any record — so what reaches the
/// no-record answer is a third-party client path an old install merely pointed
/// at, which may have been that program's own registration all along.
fn undo_for(current: &Reading, wrote: Option<&str>) -> Undo {
    if *current == Reading::Absent {
        return Undo::Nothing;
    }
    if is_ours(current, wrote) {
        return Undo::Remove;
    }
    Undo::Leave(whose(wrote.is_some()))
}

/// What uninstall did to one key.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Removed,
    Failed,
    Nothing,
    Left(&'static str),
}

/// What uninstall says it did, as the lines it prints.
///
/// Separate from the doing so the words can be tested without a registry, and
/// because they only make sense together. "Left exactly as they are" is a
/// warning about somebody else's registration; a key with nothing in it must
/// not be collected under it, or a prefix that never had the bridge is told in
/// four alarming lines that something it does not have was spared.
fn report(outcomes: &[(&str, &str, Outcome)]) -> String {
    let mut out = String::new();
    let mut removed: Vec<&str> = Vec::new();
    let mut nothing: Vec<&str> = Vec::new();
    let mut left: Vec<(&str, &str)> = Vec::new();
    for (abi, key, outcome) in outcomes {
        match outcome {
            Outcome::Removed => removed.push(abi),
            Outcome::Failed => {
                out.push_str(&format!("could not unregister the {abi} client path\n"));
            }
            Outcome::Nothing => nothing.push(abi),
            Outcome::Left(why) => left.push((key, why)),
        }
    }
    if removed.len() == KEYS.len() {
        out.push_str("unregistered TrackIR and FreeTrack client paths\n");
    } else {
        for abi in &removed {
            out.push_str(&format!("unregistered the {abi} client path\n"));
        }
    }
    if nothing.len() == KEYS.len() {
        out.push_str("no head-tracking client was registered in this prefix\n");
    } else {
        for abi in &nothing {
            out.push_str(&format!("nothing was registered for {abi}\n"));
        }
    }
    if !left.is_empty() {
        out.push_str(
            "\nthese keys hold something this install did not write, so they were left\n\
             exactly as they are:\n",
        );
        for (key, why) in left {
            out.push_str(&format!("  {key}\n    {why}\n"));
        }
    }
    out
}

/// What uninstall did to one key: the ABI it belongs to, the key, the outcome.
type KeyOutcome = (&'static str, &'static str, Outcome);

/// Read each key and do what it says, stopping at the first read that fails.
///
/// Split out of [`uninstall`] so that a run which stops half way can be tested
/// for what it *reports* as well as for what it did. A failed read used to
/// return straight out of the loop, so a wine that died between the two keys
/// printed nothing but "Refusing to touch keys whose current value could not be
/// read" — over a prefix whose FreeTrack value this run had already removed.
/// Recovery was never the problem: running it again is idempotent. Being told
/// nothing happened when something did is.
fn undo_keys(
    wine: &Path,
    prefix: &Path,
    record: &[(String, String)],
) -> Result<(Vec<KeyOutcome>, Option<String>), String> {
    // An uninstall that takes nothing out writes nothing, and must not have run
    // wine against the prefix to establish that: it upgraded prefixes it then
    // reported as untouched — see [`settled_keys`]. `collect` into an `Option`
    // is the whole test: the first key that IS ours to remove makes this run a
    // write, and a write reads through wine, because a `reg delete` aimed at a
    // reading that has gone stale deletes whatever replaced it.
    if let Some(readings) = settled_keys(prefix) {
        let settled: Option<Vec<KeyOutcome>> = readings
            .iter()
            .map(
                |((name, key, abi), current)| match undo_for(current, recorded(record, name)) {
                    Undo::Nothing => Some((*abi, *key, Outcome::Nothing)),
                    Undo::Leave(why) => Some((*abi, *key, Outcome::Left(why))),
                    Undo::Remove => None,
                },
            )
            .collect();
        if let Some(outcomes) = settled {
            return Ok((outcomes, None));
        }
    }
    let mut outcomes: Vec<KeyOutcome> = Vec::new();
    for (name, key, abi) in KEYS {
        let wrote = recorded(record, name);
        let current = match read_key(wine, prefix, key) {
            Ok(c) => c,
            Err(e) => return Ok((outcomes, Some(e))),
        };
        let outcome = match undo_for(&current, wrote) {
            Undo::Nothing => Outcome::Nothing,
            Undo::Leave(why) => Outcome::Left(why),
            Undo::Remove => {
                if del_value(wine, prefix, key)? {
                    Outcome::Removed
                } else {
                    Outcome::Failed
                }
            }
        };
        outcomes.push((abi, key, outcome));
    }
    Ok((outcomes, None))
}

/// `tobii bridge uninstall` — take out what this install put in, and nothing
/// else.
///
/// These two keys are how *any* head-tracking client is found, not just ours,
/// so a blind `reg delete` on the way out takes opentrack's registration with it
/// and leaves the prefix with nothing registered at all — worse than it was
/// before we touched it. What this installer wrote is in [`RECORD_FILE`], and a
/// key is touched only while it still says exactly that; anything else is left
/// alone and named. Nothing is ever written here: the values another program
/// holds were never recorded, so there is nothing of theirs to put back and
/// nothing of theirs to destroy.
///
/// The directory goes last, and only once the registry came out right: while a
/// key still points into it, removing it leaves the prefix pointing at
/// something that does not exist.
fn uninstall(args: &[String]) -> CmdResult {
    let (prefix, _) = resolve_prefix(args)?;
    let (wine, _) = resolve_wine(&prefix, args)?;
    let dir = prefix.join(INSTALL_SUBDIR);
    // Read before the directory goes: the record lives inside it.
    let record = read_record(&dir);

    let (outcomes, unreadable) = undo_keys(&wine, &prefix, &record)?;
    print!("{}", report(&outcomes));
    if let Some(e) = unreadable {
        return Err(e.into());
    }
    let failed = outcomes.iter().any(|(_, _, o)| *o == Outcome::Failed);

    // Nothing below here is true while a key still points into the directory.
    if failed {
        return Err(format!(
            "the registry still points at {}, so it was left in place.\n\
             Run `tobii bridge uninstall` again once wine can serve this prefix. \
             {WRONG_WINE_HINT}",
            dir.display()
        )
        .into());
    }
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)?;
        println!("removed {}", dir.display());
    }
    Ok(())
}

/// What one discovery key holds right now, as `status` reports it.
///
/// The same answers [`install`] and [`uninstall`] act on, named rather than
/// acted on. [`is_ours`] draws the line between the first two and [`Reading`]
/// draws it between the last two; nothing new is decided here, and that is the
/// point — a status that classified a key by rules of its own could tell the
/// user "ours" about a key uninstall would then refuse to touch.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyState {
    /// A value [`is_ours`] recognises: this install's own directory, or the
    /// path [`RECORD_FILE`] says we registered.
    Ours(String),
    /// A value we did not write, with the sentence [`whose`] gives for it.
    Theirs(String, &'static str),
    /// Nothing is registered there.
    Absent,
    /// Something is, and it is not a value this program can read — why, in
    /// [`Reading::Other`]'s own words.
    Unreadable(String),
}

/// Classify one key's reading exactly the way install and uninstall do.
fn key_state(current: &Reading, wrote: Option<&str>) -> KeyState {
    match current {
        Reading::Absent => KeyState::Absent,
        Reading::Other(why) => KeyState::Unreadable(why.clone()),
        Reading::Plain(v) if is_ours(current, wrote) => KeyState::Ours(v.clone()),
        Reading::Plain(v) => KeyState::Theirs(v.clone(), whose(wrote.is_some())),
    }
}

/// One discovery key as [`gather_status`] found it.
///
/// The three raw facts rather than a verdict, because the report asks two
/// different questions of them and both have to be the ones the commands ask.
/// [`key_state`] is the classification `uninstall` acts on, and
/// [`stops_install`] is what `install` acts on — and the second needs `want`,
/// which the first throws away. A `Status` holding only the classification had
/// to re-derive the refusal from it, could not, and so predicted one over a
/// key `install` walks straight past.
#[derive(Debug)]
struct KeyReport {
    /// Whose key it is, in the name the user knows it by.
    abi: &'static str,
    key: &'static str,
    /// What the key holds.
    current: Reading,
    /// What [`RECORD_FILE`] says this installer wrote here.
    wrote: Option<String>,
    /// What `install` would write here on a run spelled like this one — see
    /// [`want_for`], and [`Status::npclient_given`] for why the report carries
    /// the flag that decides it.
    want: String,
}

impl KeyReport {
    /// What it holds, in the terms `uninstall` uses.
    fn state(&self) -> KeyState {
        key_state(&self.current, self.wrote.as_deref())
    }

    /// Whether `install` would refuse over it — by the function `install`'s
    /// own loop asks, not a paraphrase of it.
    fn stops_install(&self) -> bool {
        stops_install(&self.current, self.wrote.as_deref(), &self.want)
    }
}

/// Whether something is where it should be, when "could not tell" is a real
/// third answer.
///
/// Three cases because `Path::is_file` has two and answers the wrong one:
/// it folds *every* error into `false`, so an install directory this user
/// cannot read reports all three artifacts as absent. Measured on a
/// `drive_c/tobii-bridge` holding all three with its mode set to `000`:
/// `freetrackclient64.dll  MISSING — nothing can load without it`, with the
/// file sitting right there. A report that asserts three files are gone when
/// they are present is the one failure class this command exists to prevent,
/// and it sends the user to `install` — which would then refuse, for a reason
/// nothing in the report named.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Presence {
    Yes,
    /// Nothing of that name is there. `ENOENT` and nothing else: the only
    /// error that actually means absence.
    No,
    /// The question could not be answered, in the words the answer came back
    /// in.
    Unknown(String),
}

/// Ask whether `p` is there and is the kind of thing `wanted` names.
fn presence(p: &Path, ok: fn(&std::fs::Metadata) -> bool, wanted: &str) -> Presence {
    match std::fs::metadata(p) {
        Ok(m) if ok(&m) => Presence::Yes,
        Ok(_) => Presence::Unknown(format!("there is something here that is not {wanted}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Presence::No,
        Err(e) => Presence::Unknown(format!("{e}")),
    }
}

/// Everything `tobii bridge status` found, before any of it is worded.
///
/// Gathered and rendered in two halves so that the wording is testable without
/// a prefix, a wine or a registry — and so the gathering half has no
/// opportunity to phrase anything.
struct Status {
    prefix: PathBuf,
    source: PrefixSource,
    wine: PathBuf,
    origin: WineOrigin,
    /// Whether the user named that wine themselves.
    ///
    /// Carried only so the undo command can be spelled the way this run was:
    /// a user who had to pass `--wine` did so because the automatic choice was
    /// wrong for this prefix, and `uninstall` makes the same automatic choice
    /// — so handing them a command without it hands them the failure they
    /// already worked around.
    wine_given: bool,
    /// [`choose_wine`]'s own warning about that choice, when it had one.
    ///
    /// In the report rather than on stderr: it is the sentence that most often
    /// explains "the game sees nothing", and a user who redirected this report
    /// to a file to paste it would otherwise send us everything except the
    /// answer.
    wine_warning: Option<String>,
    server: crate::wineserver::Lock,
    /// The file the registry half was read out of, named in the report because
    /// the report's honesty depends on it: it says what was last *written
    /// back*, not what a running wineserver holds.
    registry_file: PathBuf,
    dir: PathBuf,
    /// Whether the install directory is there at all.
    ///
    /// Asked separately from the artifacts, because "no directory" and "a
    /// directory whose required DLL is gone" are different states with
    /// different next steps, and reading the second off an empty artifact list
    /// reported the second as the first — which sends the user to `install`
    /// instead of showing them the one file whose absence explains the
    /// silence.
    dir_present: Presence,
    /// One entry per [`ARTIFACTS`] name: the name, whether it is required,
    /// whether it is there.
    artifacts: Vec<(&'static str, bool, Presence)>,
    /// Why [`RECORD_FILE`] could not be read, when it is there and could not
    /// be.
    ///
    /// [`read_record`] answers "no record" to a record it failed to read, which
    /// is the safe answer for install and uninstall — the action it leads to is
    /// to leave the key alone. For a report it is a lie with a consequence:
    /// without the record, a key this installer pointed at a third-party client
    /// is indistinguishable from that program's own registration, and the
    /// report below calls it a stranger's.
    record_unreadable: Option<String>,
    /// One entry per [`KEYS`] entry, in that order.
    keys: Vec<KeyReport>,
    /// `--npclient`, as this run was spelled, when it was given.
    ///
    /// Carried for the same reason [`Status::wine_given`] is: the install
    /// command this report hands back has to be the install this report
    /// modelled. `--npclient` decides what the TrackIR key would be compared
    /// against, so a report that answered for one value and printed a command
    /// that would use another is answering about a different command.
    ///
    /// On the install lines only. `uninstall` does not read it, and a pasted
    /// command carrying a flag the gate refuses is worse than no command.
    npclient_given: Option<String>,
    /// Whether game output is switched on for this machine.
    ///
    /// Not about this prefix at all, and in the report because the commonest
    /// reason to run this command is "I installed it and the game does not
    /// track" — for which this is the first thing to check and the one the
    /// report used to leave out. Read from the same file `tobii games` writes;
    /// a missing or damaged one reads as the default, which is off.
    game_output_enabled: bool,
    /// Why the registry could not be read at all, when it could not.
    unreadable: Option<String>,
}

/// The sentences that say what this report is *not*.
///
/// Whether a game accepts what is registered is unknown to this project. Two
/// titles have ever been measured against NaturalPoint's signature check — see
/// [`NpSource::Installed`] — and both stop at it, by routes different enough
/// that neither predicts the other: one rejects our DLL and gives up, the
/// other never stops asking. Two measurements are not a rule, and nothing here
/// knows what a third title does. So this report says what is registered,
/// names what was measured with the dates on it, and stops. A "ready" or
/// "working" line would be a claim nobody has earned, and a user pasting this
/// into an issue would be pasting our guess back at us as though it were a
/// measurement.
const STATUS_CAVEAT: &str = "\
     This says what is installed and registered in this prefix. It does not say\n\
     whether a game will use it. Two titles have been measured against\n\
     NaturalPoint's signature check, with our own DLL registered for TrackIR, and\n\
     both stop at it: Star Citizen (2026-08-15) rejects it and never asks for data\n\
     again; Microsoft Flight Simulator 2024 (Steam appid 2537590, Proton\n\
     Experimental, 2026-09-27) calls the check over and over and never gets past\n\
     it. Two titles are not a rule about the rest, and nothing here knows what any\n\
     other title does.\n";

/// What the prefix's wineserver lock says, in one entry.
///
/// Says only what the lock proves — that a wine process is alive on this
/// prefix, or is not — and never who it is. A holder may be the game, the
/// provider, or a `wineboot` that has not finished; [`crate::wineserver`] has
/// the mechanism, and the one consequence worth stating here is that a Proton
/// launch waits for every one of them.
fn server_line(lock: &crate::wineserver::Lock) -> String {
    match lock {
        crate::wineserver::Lock::Free => "nothing is serving this prefix right now".to_string(),
        crate::wineserver::Lock::Held(pid) => {
            let who = if *pid > 0 {
                format!("pid {pid}")
            } else {
                "the kernel would not name the holder".to_string()
            };
            format!(
                "a wine process is alive on this prefix ({who})\n           \
                 a Proton launch waits for every one of them to exit first, so the\n           \
                 supported order is the game first and anything of ours second"
            )
        }
        crate::wineserver::Lock::Unknown(why) => format!("could not tell ({why})"),
    }
}

/// What the same lock says about how current the registry half of this report
/// is, when it says anything.
///
/// The one trade this change makes, stated where the user reads the values it
/// applies to. The registry is read out of `user.reg` rather than by running
/// `wine reg query`, because running wine against a prefix initialises or
/// upgrades it — and `user.reg` is the registry as it was last written back. A
/// wineserver holds changes in memory and flushes them when the last process on
/// the prefix exits, so while one is alive a value written since it started is
/// not in the file yet.
///
/// Said only when it can be true. [`crate::wineserver::Lock::Free`] means
/// nothing has the prefix open, so the file is what the registry is, and a
/// caveat printed there would teach the user to discount a report that is
/// exactly right.
fn staleness(lock: &crate::wineserver::Lock) -> Option<&'static str> {
    match lock {
        crate::wineserver::Lock::Free => None,
        crate::wineserver::Lock::Held(_) => Some(
            "  a wine process is alive on this prefix, and a prefix's registry is\n  \
             written back when the last one exits — so anything registered since\n  \
             that process started is not in the file yet, and is not below.\n",
        ),
        crate::wineserver::Lock::Unknown(_) => Some(
            "  whether anything is serving this prefix could not be determined (see\n  \
             above), and a prefix's registry is written back only when the last\n  \
             process on it exits — so whether this is current could not be\n  \
             determined either.\n",
        ),
    }
}

/// How one artifact's [`Presence`] reads in the report.
///
/// The third answer never wears the second's words. "missing" is a claim about
/// the prefix; "could not tell" is a claim about this program's own reach, and
/// the next step differs — the first is fixed by `install`, the second by
/// looking at why the file could not be reached.
fn artifact_line(required: bool, present: &Presence) -> String {
    match (present, required) {
        (Presence::Yes, _) => "present".to_string(),
        (Presence::No, true) => "MISSING — nothing can load without it".to_string(),
        (Presence::No, false) => "missing (optional)".to_string(),
        (Presence::Unknown(why), _) => format!("could not tell whether it is here — {why}"),
    }
}

/// Word what [`gather_status`] found.
///
/// Pure. Every sentence that could be wrong about a user's prefix is in here,
/// where a test can read it without a wine on the machine.
fn render_status(s: &Status) -> String {
    let mut o = String::from("tobii bridge status\n===================\n");
    o.push_str(&format!("prefix     {}\n", s.prefix.display()));
    o.push_str(&format!("           {}\n", s.source.describe()));
    o.push_str(&format!("wine       {}\n", s.wine.display()));
    o.push_str(&format!(
        "           {}\n",
        match s.origin {
            WineOrigin::Prefix => "the build this prefix itself records",
            WineOrigin::Unverified =>
                "not corroborated by this prefix — nothing here established that\n           \
                 it is the build this prefix belongs to",
        }
    ));
    // Said plainly, because it is the difference between this report and the
    // one before it: nothing here ran that binary. It is named because
    // `install` and `uninstall` will run it against this prefix, and because
    // which build that is decides whether they upgrade the prefix.
    o.push_str(
        "           nothing here ran it — it is named because `install` and\n           \
         `uninstall` would use it on this prefix\n",
    );
    if let Some(w) = &s.wine_warning {
        for line in w.lines() {
            o.push_str(&format!("           {line}\n"));
        }
    }
    o.push_str(&format!("wineserver {}\n", server_line(&s.server)));

    o.push_str(&format!("\nfiles      {}\n", s.dir.display()));
    match &s.dir_present {
        Presence::No => {
            o.push_str("  nothing of ours is installed here — there is no such directory\n");
        }
        Presence::Unknown(why) => {
            o.push_str(&format!(
                "  could not tell what is installed here — {why}\n  \
                 so this report says nothing about these files, which is not the\n  \
                 same as their being absent\n"
            ));
        }
        Presence::Yes => {
            for (name, required, present) in &s.artifacts {
                o.push_str(&format!(
                    "  {name:<22} {}\n",
                    artifact_line(*required, present)
                ));
            }
            if let Some(why) = &s.record_unreadable {
                o.push_str(&format!(
                    "  the note of what this installer registered here could not be read\n  \
                     ({why}), so a key it pointed at a third-party client is reported\n  \
                     below as a stranger's\n"
                ));
            }
        }
    }

    // Named, because what follows is the registry as last written back rather
    // than the registry as some running process has it — and because naming the
    // file is how the report says it did not run wine to find out.
    o.push_str(&format!(
        "\nregistry   read from {}, with no wine run against this prefix\n",
        s.registry_file.display()
    ));
    if let Some(caveat) = staleness(&s.server) {
        o.push_str(caveat);
    }
    for k in &s.keys {
        o.push_str(&format!("  {:<10} {}\n", k.abi, k.key));
        match &k.state() {
            KeyState::Ours(v) => {
                o.push_str(&format!("             {v}\n"));
                o.push_str("             registered by this installer\n");
            }
            // The value on one line and [`whose`]'s sentence on the next, the
            // shape [`refusal`] prints it in — and the sentence itself
            // unaltered, because it is calibrated: with no record it says only
            // that there is no telling whose it is, which is all that was
            // established.
            KeyState::Theirs(v, why) => {
                o.push_str(&format!("             {v}\n"));
                o.push_str("             not this installer's\n");
                o.push_str(&format!("             {why}\n"));
            }
            KeyState::Absent => o.push_str("             nothing is registered here\n"),
            // Not "leaves it alone", which is only half of it and the
            // reassuring half: `uninstall` does leave it alone, but `is_ours`
            // is false for everything that is not a plain string, so `install`
            // stops the whole command over it rather than writing past it.
            KeyState::Unreadable(why) => {
                o.push_str(&format!("             {why}\n"));
                o.push_str(
                    "             so `uninstall` leaves it alone and `install` refuses\n\
                     \x20            over it rather than overwrite what it could not read\n",
                );
            }
        }
        // The one line here that tells a user in the losing configuration that
        // they are in it, while there is still something to do about it. Read
        // off the value rather than off the classification: what decides it is
        // that the TrackIR key names our own DLL, which is as true of a key
        // somebody else pointed there as of one we wrote.
        if k.key == NP_KEY && matches!(&k.current, Reading::Plain(v) if v == INSTALL_WIN_DIR) {
            o.push_str(
                "             that is our own DLL, which cannot answer NaturalPoint's\n\
                 \x20            signature check — see the note below\n",
            );
        }
    }
    // Said under the heading, before the reason, because an empty `registry`
    // section followed by a paragraph reads as "there is nothing in these
    // keys" — the one thing this answer is not. Both keys, and not "the
    // remaining one": there is one file and either it was read or it was not,
    // so there is no half-answer left to name.
    if let Some(why) = &s.unreadable {
        o.push_str(
            "  neither key could be read, so this report says nothing about what\n  \
             they hold — which is not the same as their holding nothing:\n\n",
        );
        for line in why.lines() {
            o.push_str(&format!("  {line}\n"));
        }
    }

    o.push_str(&format!("\n{STATUS_CAVEAT}"));
    // Which of the two commands this report hands back, and whether it may
    // promise the one it names will go through.
    //
    // Three cases and not two, for the reason every other answer in this file
    // is three-valued. This used to ask `anything_of_ours` alone, which folded
    // `Theirs` and `Unreadable` into "nothing": a prefix whose FreeTrack key
    // holds a stranger's path was told "Nothing of ours is in this prefix. To
    // put it there: tobii bridge install …" — the one line in the report whose
    // whole job is to be pasted back — over a command that then refuses,
    // because of the very key this report had just classified two paragraphs
    // above. The classification was right; the line that acted on it was not.
    let anything_of_ours = s.dir_present != Presence::No
        || s.keys
            .iter()
            .any(|k| matches!(k.state(), KeyState::Ours(_)));
    // Not a list of states this report believes `install` stops on — the
    // states `install` itself stops on, from [`stops_install`], the function
    // `install`'s own loop calls. The paraphrase it replaces (`Theirs` or
    // `Unreadable`) was right about three of the four cases and wrong about
    // the fourth, which is the one a machine with opentrack installed is in.
    let in_the_way: Vec<&str> = s
        .keys
        .iter()
        .filter(|k| k.stops_install())
        .map(|k| k.key)
        .collect();
    // Spelled with this run's own flags, and quoted: it is the one line in the
    // report whose entire job is to be pasted back.
    let how = format!(
        "{}{}",
        match &s.source {
            PrefixSource::Steam(w) => format!(" --steam {}", quoted(w)),
            _ => format!(" --prefix {}", shell_quoted(&s.prefix)),
        },
        if s.wine_given {
            format!(" --wine {}", shell_quoted(&s.wine))
        } else {
            String::new()
        }
    );
    // Only on the install lines: see [`Status::npclient_given`]. It is what
    // `in_the_way` was decided against, so an install line without it would be
    // a different command from the one this report just answered for.
    let np = match &s.npclient_given {
        Some(v) => format!(" --npclient {}", quoted(v)),
        None => String::new(),
    };
    // The whole-command refusal `install` makes before it reads a key at all,
    // asked of the function `install` asks — see [`steam_wine_refused`]. The
    // report used to model [`stops_install`] and stop there, which left it
    // handing back a bare `install` line for a Proton prefix whose build could
    // not be read: a command that exits without touching either key, over a
    // refusal nothing in the report had named.
    let proton_unknown = steam_wine_refused(&s.source, s.origin, s.wine_given, false);
    // So every install line this report prints carries the flag that gets it
    // past that refusal. Spelled `<Proton>` because only the user can fill it
    // in, in the same words the refusal itself uses.
    let proton = if proton_unknown {
        " --wine <Proton>/files/bin/wine"
    } else {
        ""
    };
    if anything_of_ours {
        // The state a bug reporter is actually in, and the report used to
        // answer it with one command: `uninstall`. Somebody whose game gets
        // nothing was handed the way to remove what they have and nothing at
        // all about how to proceed — while `install` prints both of the
        // remaining conditions at the end of every successful run, where a
        // user who installed last week never sees them again.
        //
        // Neither is about this prefix, which is why they are stated as what
        // has to be true *as well* rather than folded in among the registry
        // findings above.
        o.push_str(
            "\nThis prefix is only half of it. Two things outside it decide whether a\n\
             game receives anything:\n",
        );
        o.push_str(if s.game_output_enabled {
            "  game output is ON\n"
        } else {
            "  game output is OFF — nothing is sent to any game until it is on:\n    \
             tobii games set enabled true        (or the hub's own switch)\n"
        });
        o.push_str(
            "  the game has to be launched through the wrapper, so the tracker comes on:\n    \
             tobii game -- %command%             (Steam: paste into Launch Options)\n",
        );
        o.push_str(&format!(
            "\nTo undo everything this installer put here:\n  \
             tobii bridge uninstall{how}\n\
             It takes out only the values it still recognises as its own, and names\n\
             anything it leaves alone.\n"
        ));
    } else if !in_the_way.is_empty() {
        o.push_str(
            "\nNothing of ours is in this prefix, and `install` will not put it there\n\
             while these hold something this installer did not write:\n",
        );
        for key in &in_the_way {
            o.push_str(&format!("  {key}\n"));
        }
        o.push_str(&format!(
            "It refuses over them rather than overwrite them — the same rule this\n\
             report classified them by — and spells out the command that clears one,\n\
             if it is stale. Or go ahead anyway with:\n  \
             tobii bridge install{how}{np}{proton} --force\n\
             which promises nothing about putting back what is there now.\n"
        ));
    } else if s.unreadable.is_some() {
        // Not "nothing is in this prefix": the section above has just said
        // neither key could be read, and that this is not the same as their
        // holding nothing. Three lines later asserting it anyway made one
        // report contradict itself.
        o.push_str(&format!(
            "\nNothing of ours was found in this prefix — though neither key could be\n\
             read, so that is not the same as nothing being in them. To put ours\n\
             there:\n  \
             tobii bridge install{how}{np}{proton}\n\
             It reads both keys itself before writing either, and stops if one holds\n\
             something it did not write.\n\
             To take it out again afterwards:\n  \
             tobii bridge uninstall{how}\n"
        ));
    } else {
        o.push_str(&format!(
            "\nNothing of ours is in this prefix. To put it there:\n  \
             tobii bridge install{how}{np}{proton}\n\
             To take it out again afterwards:\n  \
             tobii bridge uninstall{how}\n"
        ));
    }
    // What `<Proton>` is, said once, under whichever of the three branches
    // above printed an install line. Not under the fourth: that one hands back
    // `uninstall`, which is never refused over this — see
    // [`refuse_unverified_wine_for_steam`]'s "why only here" — so naming a
    // refusal there would describe a command this report does not print.
    //
    // The last sentence is here because the branch above may have said
    // `install` "spells out the command that clears one": it does, once it
    // gets as far as the keys, and this refusal is the thing that stops it
    // first.
    if proton_unknown && !anything_of_ours {
        o.push_str(&format!(
            "\n`<Proton>` above is not a placeholder this report can fill in. Steam made\n\
             this prefix with some Proton build, and which build could not be read out\n\
             of it, so the only wine left here is {w} — which `install` refuses to run\n\
             against a Proton prefix, because a wine that is not the build a prefix was\n\
             made with upgrades the prefix out from under the game that owns it. Steam\n\
             lists the build under the title's compatibility setting, under\n\
             `steamapps/common`.\n\
             That refusal comes before either key is read, so without `--wine` the\n\
             command stops having said nothing about what they hold.\n",
            w = s.wine.display()
        ));
    }
    // Said because the sentence above it tells the user to post this publicly,
    // and because the other half of that sentence promises the opposite:
    // `tobii debug` spends two documented bug fixes folding the home path,
    // login name and hostname away and signs off saying so. This report cannot
    // do the same — the prefix it read and the command it hands back are only
    // any use spelled exactly — so it says which of the two they are pasting.
    o.push_str(
        "\n\
         If something is wrong, paste this report and the output of `tobii debug`\n\
         into https://github.com/Tropaion/Tobii_Linux/issues — between them they say\n\
         what state the prefix and the tracker are actually in. Read it through\n\
         first: unlike `tobii debug`, which folds your home path away, this report\n\
         names paths as they are spelled on your machine, login name and all.\n",
    );
    o
}

/// Read a prefix and say what is in it. Runs nothing, writes nothing, creates
/// nothing.
///
/// **No process is spawned here at all.** That is not tidiness: `wine reg
/// query` was how this read the registry, and wine initialises or upgrades
/// whatever prefix it is pointed at before it runs anything — 5510 paths
/// created under a prefix holding only `drive_c`, and 2744 lines of
/// `system.reg` rewritten plus `.update-timestamp` overwritten on a complete
/// prefix whose stamp was stale, which is what a Proton prefix looks like to
/// the host's wine. All three reads are now plain file reads:
/// [`read_keys_read_only`] for the registry, [`read_record`] for the record,
/// and `F_GETLK` for the lock, which reports what a lock attempt *would* hit
/// and takes nothing. A user running this to find out what is wrong must not
/// change what is wrong, and must not find that asking the question upgraded
/// the prefix the answer was about.
///
/// The lock is asked first and stays first, so that a holder this report names
/// can never be one of ours — there is now nothing of ours that could become
/// one, and the ordering keeps it that way if that ever changes.
fn gather_status(args: &[String]) -> Result<Status, String> {
    let (prefix, source) = resolve_prefix(args)?;
    let (wine, origin, wine_warning) = resolve_wine_reporting(&prefix, args)?;
    let dir = prefix.join(INSTALL_SUBDIR);

    let server = lock_state(&crate::wineserver::lock_for(&prefix));

    let artifacts = ARTIFACTS
        .iter()
        .map(|(name, required)| {
            (
                *name,
                *required,
                presence(&dir.join(name), std::fs::Metadata::is_file, "a file"),
            )
        })
        .collect();

    // The record first, for the reason install reads it first: without it our
    // own registration of a third-party client is indistinguishable from that
    // program's own, and this report would call it a stranger's. Read here
    // rather than through `read_record`, which folds a failure into "no
    // record": that is the right answer for a command whose next move is to
    // leave a key alone, and the wrong one for a report, which would then state
    // the consequence as a fact about somebody else's prefix.
    let (record, record_unreadable) = match std::fs::read_to_string(dir.join(RECORD_FILE)) {
        Ok(t) => (parse_record(&t), None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
        Err(e) => (Vec::new(), Some(format!("{e}"))),
    };
    let (readings, unreadable) = read_keys_read_only(&prefix);
    // The same answer `install` would settle on, from the same flags — and it
    // fails here exactly where `install` would fail, with the same words, over
    // an `--npclient` that names no client or a path that cannot be registered.
    let (_, np_target) = np_target_for(args)?;
    let keys = readings
        .iter()
        .map(|((name, key, abi), current)| KeyReport {
            abi,
            key,
            current: current.clone(),
            wrote: recorded(&record, name).map(str::to_string),
            want: want_for(key, &np_target).to_string(),
        })
        .collect();

    Ok(Status {
        prefix: prefix.clone(),
        source,
        wine,
        origin,
        wine_given: crate::flag_value(args, "--wine").is_some(),
        wine_warning,
        server,
        registry_file: prefix.join(crate::userreg::FILE),
        dir_present: presence(&dir, std::fs::Metadata::is_dir, "a directory"),
        dir,
        artifacts,
        record_unreadable,
        keys,
        npclient_given: crate::flag_value(args, "--npclient").map(str::to_string),
        game_output_enabled: tobii_output::games::load_output_config().enabled,
        unreadable,
    })
}

/// `tobii bridge status` — what is in a prefix right now, and nothing else.
fn status(args: &[String]) -> CmdResult {
    print!("{}", render_status(&gather_status(args)?));
    Ok(())
}

/// Dispatch `tobii bridge ...`.
/// `tobii bridge games` — what is installed and which titles have a prefix.
fn list_steam_games() -> CmdResult {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set, so Steam's libraries cannot be found")?;
    list_steam_games_in(&home)
}

/// The body of [`list_steam_games`], with the home named rather than read out
/// of the environment, so that both of its refusals can be put to a test:
/// `$HOME` is process-global and these tests run in parallel.
fn list_steam_games_in(home: &Path) -> CmdResult {
    // The libraries, the ones that are not here, the applications and a prefix
    // stat per row, off one walk. The two halves of the census came from two
    // walks before, which is two chances to disagree about one library.
    let steam = tobii_steam::Steam::at(home);
    let libs = steam.libraries();
    let missing = crate::steam_libraries_missing(&steam);
    if libs.is_empty() {
        return Err(crate::with_missing("no Steam libraries found".to_string(), &missing).into());
    }
    for lib in libs {
        println!("library: {}", lib.display());
    }
    // With the libraries, not at the end: this is part of the same census, and
    // a reader counting the lines above should see it while counting them.
    if !missing.is_empty() {
        println!("{missing}");
    }
    let apps = steam.apps();
    if apps.is_empty() {
        // Bare, unlike the refusal above it: that one returns before the block
        // is printed, this one after, and naming the absent library twice
        // reads as two different libraries.
        return Err("no installed Steam games found".into());
    }
    println!();
    for app in &apps {
        // A title with no prefix has never been run under Proton, which is the
        // one thing that stops `--steam` working — so it is shown, not hidden.
        //
        // Proton itself and the Steam runtimes are shown too, which
        // `tobii_steam::looks_like_tool` could now filter out. Left alone: this
        // list is what it was before the move, and what it should be is the
        // maintainer's call, not a rewiring's.
        let mark = if steam.prefix(&app.appid).is_some() {
            "proton"
        } else {
            "  --  "
        };
        println!("  {mark}  {id:<10} {name}", id = app.appid, name = app.name);
    }
    println!(
        "\ninstall into one with:  tobii bridge install --steam <app id or name>\n\
         titles marked `--` have no Proton prefix yet — run them once first."
    );
    Ok(())
}

/// Every flag any `tobii bridge` subcommand reads, and how it is spelled in a
/// usage line. One table so a usage line cannot advertise a flag the command
/// refuses, which is what one shared string did.
const FLAGS: [(&str, &str); 7] = [
    ("steam", "--steam <app id or name>"),
    ("prefix", "--prefix PATH"),
    ("wine", "--wine PATH"),
    ("artifacts", "--artifacts DIR"),
    ("npclient", "--npclient ours|DIR"),
    ("force", "--force"),
    ("port", "--port PORT"),
];

/// `--force` is the only one that stands alone; every other flag names a value.
fn takes_value(name: &str) -> bool {
    name != "force"
}

fn usage_for(sub: &str, known: &[&str]) -> String {
    let flags: Vec<&str> = FLAGS
        .iter()
        .filter(|(n, _)| known.contains(n))
        .map(|(_, hint)| *hint)
        .collect();
    format!("Usage: tobii bridge {sub} {}", flags.join(" "))
}

/// Reject anything `tobii bridge <sub>` does not read, before it resolves,
/// creates or writes a thing.
///
/// Parsed strictly for the same reason `tobii record` is, and with more at
/// stake: an unrecognised flag used to be IGNORED, so `tobii bridge install
/// --help` did not print help — it performed a real install into the default
/// Wine prefix and wrote both discovery keys. Three spellings reach that same
/// outcome and all three are refused here:
///
///   * `-h`, and any other single-dash token. A gate that only looked at `--`
///     left this one open, which is the whole bug one keystroke away.
///   * `--prefix=PATH`. It would pass a name check and then be ignored, because
///     every reader is `flag_value`, which compares the whole token — so the
///     command would install into a prefix the user did not name.
///   * `--prefix` with nothing after it, which falls back to the same default.
///   * a bare path — `tobii bridge install /games/pfx`, the most natural
///     spelling of all. Nothing downstream reads a positional: `resolve_prefix`
///     looks at `--steam`, `--prefix`, `$WINEPREFIX` and then `~/.wine`, so the
///     typed path was dropped and the install went into the default prefix —
///     the maintainer's own `~/.wine` when nothing else names one — and said
///     so nowhere. It is refused rather than read as `--prefix` because the
///     commonest way to produce one is not a forgotten flag at all: `--steam
///     Star Citizen` leaves `Citizen` standing alone, and quietly treating that
///     as a prefix path is the same class of mistake one layer down. A refusal
///     names both.
///
/// A flag's VALUE is not inspected: `--wine /opt/-odd/wine` is a real path, and
/// refusing it for its leading dash would refuse a correct command.
fn reject_unknown_flags(args: &[String], sub: &str, known: &[&str]) -> Result<(), String> {
    let usage = usage_for(sub, known);
    let mut it = args.iter().skip(3);
    while let Some(a) = it.next() {
        // A bare `-` is not a flag either, and this command reads no stdin, so
        // it is a positional like any other.
        if a == "-" || !a.starts_with('-') {
            return Err(format!(
                "`{a}` stands on its own, and this command reads no positional \
                 arguments. A `--steam` title with spaces in it has to be quoted \
                 as one argument — `--steam 'Star Citizen'`, not `--steam Star \
                 Citizen`, which leaves `Citizen` standing here. A prefix is named \
                 with `--prefix PATH`. {usage}"
            ));
        }
        if let Some((name, _)) = a.split_once('=') {
            return Err(format!(
                "`{a}` is not a spelling this command reads — write `{name} VALUE`. {usage}"
            ));
        }
        let name = a.trim_start_matches('-');
        if !known.contains(&name) {
            return Err(format!("unknown option `{a}`. {usage}"));
        }
        if takes_value(name) && it.next().is_none() {
            return Err(format!("`{a}` needs a value after it. {usage}"));
        }
    }
    Ok(())
}

/// Every `tobii bridge` subcommand: its name, every flag it reads, and what
/// runs it.
///
/// One table, walked once — gate, then dispatch — so a subcommand cannot be
/// checked against one list of flags and then run by another, and so the usage
/// line a bare `tobii bridge` prints names exactly the subcommands that exist
/// rather than a hand-typed copy of them.
///
/// Each list is spelled out in full rather than composed from a shared base.
/// `--artifacts` is why: the flags are nearly the same four every time, and a
/// base plus extras reads as though every omission were an oversight — which
/// is how three of these five came to advertise a flag none of them reads.
#[allow(clippy::type_complexity)]
const SUBS: [(&str, &[&str], fn(&[String]) -> CmdResult); 5] = [
    ("games", &[], |_| list_steam_games()),
    (
        "install",
        &["steam", "prefix", "wine", "artifacts", "npclient", "force"],
        install,
    ),
    // `--artifacts` names the *build* directory an install copies from, and
    // [`artifact_dir`] is called from `install` and nowhere else. The three
    // below never look there: `run` and `uninstall` derive their directory as
    // `prefix.join(INSTALL_SUBDIR)`, and `status` reports what is in the
    // prefix. They listed it anyway, inherited from one shared flag string, so
    // `uninstall --artifacts /does/not/exist` was accepted and discarded and
    // the usage line advertised it — which is how a user ends up believing a
    // command read a directory it never opened.
    ("run", &["steam", "prefix", "wine", "port"], run),
    ("status", &["steam", "prefix", "wine", "npclient"], status),
    ("uninstall", &["steam", "prefix", "wine"], uninstall),
];

pub fn bridge(args: &[String]) -> CmdResult {
    let sub = args.get(2).map(String::as_str);
    if let Some((name, known, handler)) = SUBS.iter().find(|(n, _, _)| Some(*n) == sub) {
        reject_unknown_flags(args, name, known)?;
        return handler(args);
    }
    let names: Vec<&str> = SUBS.iter().map(|(n, _, _)| *n).collect();
    Err(format!(
        "usage: tobii bridge {}{}",
        names.join("|"),
        match sub {
            Some(o) => format!("\nunknown argument `{o}`"),
            None => String::new(),
        }
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// `--help` is the first thing anybody types at an unfamiliar command, and
    /// it used to be ignored — so it installed into the default Wine prefix and
    /// wrote both discovery keys. A reviewer reached that by typing exactly
    /// that. An unknown flag must stop the command before it touches anything.
    #[test]
    fn an_unknown_flag_stops_the_command_before_it_touches_a_prefix() {
        let args: Vec<String> = ["tobii", "bridge", "install", "--help"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let err = reject_unknown_flags(&args, "install", &["prefix", "force"])
            .expect_err("--help is not a flag install reads");
        assert!(err.contains("--help"), "name the flag: {err}");
    }

    /// Four spellings that reached a real install into the default Wine
    /// prefix, all found by checks of the gate that was supposed to stop
    /// exactly this. `-h` is the one that matters most among the flags: a gate
    /// looking only at `--` leaves the bug one keystroke away from where it
    /// was. The bare path is the one most likely to be typed at all.
    #[test]
    fn every_spelling_that_reached_an_install_is_refused() {
        let run = |tail: &[&str]| {
            let mut v = vec!["tobii".to_string(), "bridge".into(), "install".into()];
            v.extend(tail.iter().map(|s| s.to_string()));
            reject_unknown_flags(&v, "install", &["prefix", "force"])
        };
        for tail in [
            vec!["-h"],
            vec!["-help"],
            vec!["--prefix=/tmp/x"],
            vec!["--prefix"],
            // The fourth: a bare path, which nothing downstream reads.
            vec!["/games/pfx"],
            // And a bare dash, which is a positional like any other here —
            // this command reads no stdin.
            vec!["-"],
        ] {
            let err = run(&tail).expect_err(&format!("{tail:?} must be refused"));
            assert!(
                err.contains(tail[0].trim_end_matches("=/tmp/x")),
                "the message must name what was rejected: {err}"
            );
        }
        // A flag's value may begin with a dash, and refusing it for that would
        // refuse a correct command.
        run(&["--prefix", "-odd-path"]).expect("a value that starts with a dash is still a value");
    }

    /// The most natural spelling of all, and the one that shipped: a bare path
    /// after the subcommand.
    ///
    /// `resolve_prefix` reads `--steam`, `--prefix`, `$WINEPREFIX` and then
    /// `~/.wine`, and `flag_value` compares whole tokens — so no positional
    /// ever reached a reader. `tobii bridge install /games/pfx` left that
    /// prefix byte for byte untouched, installed into the default one instead,
    /// wrote both discovery keys there and exited 0. With nothing naming a
    /// prefix, the default is the user's own `~/.wine`.
    ///
    /// Refused rather than read as `--prefix`, because the commonest way to
    /// produce a positional is not a forgotten flag: `--steam Star Citizen`
    /// leaves `Citizen` standing alone, and silently treating that as a prefix
    /// path is the same mistake one layer down.
    #[test]
    fn a_bare_path_is_not_quietly_installed_somewhere_else() {
        let w = FakeWine::new("positional");
        let typed = w.root.join("the-one-the-user-meant");
        std::fs::create_dir_all(typed.join("drive_c")).expect("other prefix");
        let mut args = w.args("install", &[]);
        args.push(typed.display().to_string());

        let err = bridge(&args)
            .expect_err("a path this command cannot read must stop it")
            .to_string();
        assert!(
            err.contains(&typed.display().to_string()),
            "the message must name the argument it refused: {err}"
        );
        assert!(
            err.contains("--prefix PATH"),
            "and how to name a prefix: {err}"
        );
        // The one remedy that must not be followed. `--steam Star Citizen`
        // leaves `Citizen` standing alone, and the message used to open by
        // offering `--prefix Citizen` — which is the same mistake one layer
        // down, and the commit that added the refusal says so. The quoting
        // hint is the real fix, so it leads.
        assert!(
            !err.contains(&format!("--prefix {}", typed.display())),
            "the refused token must not come back as a suggested value: {err}"
        );
        let quoting = err.find("quoted").expect("the fix that works");
        let flag = err.find("--prefix PATH").expect("the generic form");
        assert!(quoting < flag, "the real fix leads: {err}");
        // Neither prefix: not the one that was typed, and not the one that
        // would have been used instead of it.
        assert!(!typed.join(INSTALL_SUBDIR).exists(), "{}", typed.display());
        assert!(!w.dest().exists(), "{}", w.dest().display());
        assert!(w.argv().is_empty(), "and no wine ran at all: {}", w.argv());
    }

    /// [`SUBS`]'s own doc says each list is every flag the subcommand reads.
    /// For `--artifacts` that was false for three of the five.
    ///
    /// [`artifact_dir`] is called from `install` and nowhere else: `run` and
    /// `uninstall` derive their directory as `prefix.join(INSTALL_SUBDIR)`.
    /// Both listed the flag anyway, so `uninstall --artifacts
    /// /does/not/exist` was accepted and the value dropped, and the usage line
    /// offered it — a user then believes a command read a directory it never
    /// opened.
    #[test]
    fn only_install_advertises_the_build_directory_it_copies_from() {
        for (name, known, _) in SUBS {
            let reads_it = name == "install";
            assert_eq!(
                known.contains(&"artifacts"),
                reads_it,
                "`{name}` and `--artifacts`"
            );
            assert_eq!(
                usage_for(name, known).contains("--artifacts"),
                reads_it,
                "`{name}`'s usage line and `--artifacts`"
            );
        }
        // Driven through [`bridge`] and not through the gate directly, because
        // what can be wrong is the list the dispatch hands over.
        let w = FakeWine::new("artifacts-gate");
        for sub in ["uninstall", "run"] {
            let mut args = w.args(sub, &[]);
            args.push("--artifacts".to_string());
            args.push(w.root.join("artifacts").display().to_string());
            let err = bridge(&args)
                .expect_err(&format!("`{sub}` does not read it, so it must refuse it"))
                .to_string();
            assert!(err.contains("--artifacts"), "{err}");
            assert!(
                !err.split_once(&format!("Usage: tobii bridge {sub}"))
                    .expect("its own usage line")
                    .1
                    .contains("--artifacts"),
                "and must not go on to advertise it: {err}"
            );
        }
    }

    /// [`WRONG_WINE_HINT`] is pasted after three different sentences, and two
    /// of them end by naming the wine ("The wine used was X."). Folding
    /// `read_key`'s own copy into the shared const while it still opened with a
    /// bare "that" left the third site — whose previous sentence is "Refusing
    /// to touch keys whose current value could not be read." — with nothing for
    /// the pronoun but "a Steam title".
    ///
    /// Read at the site, not asserted about the const, because the const is not
    /// what was wrong: the sentence it lands after is.
    #[test]
    fn the_wrong_wine_hint_carries_its_own_subject() {
        let w = FakeWine::new("hint-subject");
        w.fail_wine();
        let err = read_key(fake_wine(), &w.prefix(), FT_KEY)
            .expect_err("a wine that cannot serve the prefix must not read as an absence");
        assert!(
            err.contains("could not be read. For a Steam title the wine must be the Proton build"),
            "the clause has to make sense after the sentence it follows: {err}"
        );
        assert!(
            !err.contains("For a Steam title that must be"),
            "a bare `that` attaches to `a Steam title` here: {err}"
        );
    }

    /// Wine's own chatter is captured, not inherited into the installer's
    /// output — and kept for the one case where it is the only explanation.
    ///
    /// A real install printed `WARNING: radv is not a conformant Vulkan
    /// implementation` and two copies of `reg: Der Vorgang wurde erfolgreich
    /// abgeschlossen`, wine's success line in the user's locale, between
    /// "copied 3 file(s) into …" and "registered C:\tobii-bridge …". Only the
    /// read path went through [`wine_output`]; both writes ran with this
    /// process's stdio, so the one output written to be pasted into an issue
    /// carried untranslated noise between two of its own lines.
    #[test]
    fn a_registry_write_keeps_wines_chatter_and_prints_it_only_when_it_failed() {
        let w = FakeWine::new("write-chatter");
        let add = [
            "reg",
            "add",
            FT_KEY,
            "/v",
            "Path",
            "/t",
            "REG_SZ",
            "/d",
            INSTALL_WIN_DIR,
            "/f",
        ];

        // The failure: wine's reason comes back, indented so it cannot be read
        // as one of our own lines.
        w.fail_writes();
        let said = reg_write(fake_wine(), &w.prefix(), &add)
            .expect("wine ran")
            .expect("the write failed, so there is something to say");
        assert!(said.contains("Zugriff verweigert"), "{said:?}");
        assert!(
            said.lines().all(|l| l.starts_with("  ")),
            "indented, so it is visibly wine's and not ours: {said:?}"
        );

        // The success: the fake prints wine's own success line, exactly as the
        // real one does, and nothing of it reaches the caller.
        w.allow_writes();
        assert_eq!(
            reg_write(fake_wine(), &w.prefix(), &add).expect("wine ran"),
            None,
            "a write that landed has nothing to say, least of all in German"
        );
        assert!(
            set_key(fake_wine(), &w.prefix(), FT_KEY, INSTALL_WIN_DIR).expect("wine ran"),
            "and it really did write the key"
        );
    }

    /// A usage line that lists the flag it has just called unknown tells the
    /// reader the flag both is and is not accepted.
    #[test]
    fn the_usage_line_names_only_this_subcommands_flags() {
        let u = usage_for("uninstall", &["steam", "prefix", "wine"]);
        assert!(u.contains("--prefix"), "{u}");
        assert!(!u.contains("--npclient"), "uninstall does not read it: {u}");
        assert!(!u.contains("--force"), "uninstall does not read it: {u}");
    }

    /// A flag's VALUE is not a flag. `--steam Elite Dangerous` puts two bare
    /// words after it, and a path may begin with a dash after `--`; mistaking
    /// either for an option would refuse a command that is perfectly correct.
    #[test]
    fn a_flags_value_is_skipped_and_force_takes_none() {
        let ok: Vec<String> = [
            "tobii",
            "bridge",
            "install",
            "--prefix",
            "--odd-path",
            "--force",
            "--npclient",
            "ours",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        reject_unknown_flags(&ok, "install", &["prefix", "force", "npclient"])
            .expect("a value is not an option, and --force takes no value");

        // `--force` consuming a value would swallow the flag after it, so a
        // typo behind it would go unnoticed.
        let typo: Vec<String> = ["tobii", "bridge", "install", "--force", "--prefx", "/tmp/x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(
            reject_unknown_flags(&typo, "install", &["prefix", "force"]).is_err(),
            "a typo after --force must still be caught"
        );
    }

    /// The prefix's own launch script is the most authoritative answer — it is
    /// literally what starts the game — so it outranks everything but an
    /// explicit choice.
    #[test]
    fn the_prefixs_own_launch_script_beats_a_bundled_runner_and_the_path() {
        let (w, origin, warn) = choose_wine(
            None,
            Some(p("/games/sc/runners/tkg-11.7/bin/wine")),
            &[p("/games/sc/runners/tkg-11.5/bin/wine")],
            Some(p("/usr/bin/wine")),
        )
        .expect("resolves");
        assert_eq!(w, p("/games/sc/runners/tkg-11.7/bin/wine"));
        assert_eq!(origin, WineOrigin::Prefix);
        assert_eq!(warn, None);
    }

    #[test]
    fn a_bundled_runner_beats_the_system_wine() {
        let (w, origin, warn) = choose_wine(
            None,
            None,
            &[p("/games/sc/runners/tkg-11.5/bin/wine")],
            Some(p("/usr/bin/wine")),
        )
        .expect("resolves");
        assert_eq!(w, p("/games/sc/runners/tkg-11.5/bin/wine"));
        assert_eq!(origin, WineOrigin::Prefix);
        assert_eq!(warn, None);
    }

    /// Falling back to the system wine is allowed but suspicious: it is exactly
    /// the case that produces a second wineserver and an invisible mapping.
    ///
    /// And it must never come back as the prefix's own. The refusal quotes this
    /// binary inside a `reg delete` and tells the user to use *that* wine and
    /// not the one on their PATH — which is this one, whose paste would run the
    /// `wineboot -u` upgrade the sentence warns about.
    #[test]
    fn falling_back_to_the_system_wine_warns() {
        let (w, origin, warn) =
            choose_wine(None, None, &[], Some(p("/usr/bin/wine"))).expect("resolves");
        assert_eq!(w, p("/usr/bin/wine"));
        assert_eq!(origin, WineOrigin::Unverified);
        let warn = warn.expect("a warning");
        assert!(warn.contains("--wine"), "{warn}");
        assert!(warn.contains("/usr/bin/wine"), "and which binary: {warn}");
        // The damage, not a different one. This used to say "if the game sees
        // no tracking, pass --wine …" — a warning about something the user can
        // retry — over a run that instead rewrites the prefix. Measured on a
        // Proton-shaped throwaway: `wineboot -u`, the stamp taken over,
        // `system.reg` replaced, and nothing put back.
        assert!(
            warn.contains("wineboot -u") && warn.contains("upgrades it"),
            "the warning has to name what actually happens: {warn}"
        );
        assert!(
            !warn.contains("sees no tracking"),
            "a tracking failure is not what this costs: {warn}"
        );
    }

    /// And for a `--steam` prefix that fallback is not a risk to warn about but
    /// a known-wrong answer, so `install` refuses it outright.
    ///
    /// The prefix came out of Steam's `compatdata`, so a Proton build made it;
    /// the host's wine is by construction not that build. Measured: `install
    /// --steam 8888` with no runner resolvable ran /usr/bin/wine, which moved
    /// the prefix's `.update-timestamp`, spawned `wineboot`, `explorer`,
    /// `rundll32` and `control`, and printed nothing for the nine minutes
    /// before it was killed — to write two registry values.
    #[test]
    fn a_steam_prefix_whose_runner_is_unknown_is_not_written_by_the_system_wine() {
        let args: Vec<String> = ["tobii", "bridge", "install", "--steam", "8888"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let steam = PrefixSource::Steam("8888".to_string());
        let sys = p("/usr/bin/wine");

        let err = refuse_unverified_wine_for_steam(&steam, WineOrigin::Unverified, &sys, &args)
            .expect_err("the system wine must not be turned loose on a Proton prefix");
        assert!(
            err.contains("wineboot -u") && err.contains("upgrades it"),
            "it has to name the damage: {err}"
        );
        assert!(
            !err.contains("sees no tracking"),
            "which is not a tracking failure: {err}"
        );
        assert!(err.contains("--wine"), "and the way past it: {err}");
        assert!(err.contains("--force"), "and the other one: {err}");
        assert!(
            err.contains("'8888'"),
            "spelled with this run's own title: {err}"
        );

        // Never refused where the answer was established, or where the user
        // supplied one: this function knows nothing they do not.
        refuse_unverified_wine_for_steam(
            &steam,
            WineOrigin::Prefix,
            &p("/games/Proton/files/bin/wine"),
            &args,
        )
        .expect("the build the prefix records is the whole point");
        for extra in [vec!["--wine", "/usr/bin/wine"], vec!["--force"]] {
            let mut chosen = args.clone();
            chosen.extend(extra.iter().map(|x| x.to_string()));
            refuse_unverified_wine_for_steam(&steam, WineOrigin::Unverified, &sys, &chosen)
                .expect("an explicit choice is the user's to make");
        }
        // And a prefix nobody called Steam's is not this rule's business: a
        // plain `WINEPREFIX` is very often the system wine's own.
        for source in [
            PrefixSource::Given,
            PrefixSource::Environment,
            PrefixSource::Default,
        ] {
            refuse_unverified_wine_for_steam(&source, WineOrigin::Unverified, &sys, &args)
                .expect("only a Proton prefix is known-wrong for the system wine");
        }
    }

    /// An explicit choice is honoured — but if it disagrees with the prefix's
    /// own runner, the user hears about it, because the resulting failure looks
    /// like success everywhere except in the game.
    #[test]
    fn an_explicit_wine_that_contradicts_the_prefix_is_used_but_flagged() {
        let (w, origin, warn) = choose_wine(
            Some(p("/usr/bin/wine")),
            Some(p("/games/sc/runners/tkg-11.7/bin/wine")),
            &[],
            Some(p("/usr/bin/wine")),
        )
        .expect("resolves");
        assert_eq!(w, p("/usr/bin/wine"), "the explicit choice still wins");
        assert_eq!(
            origin,
            WineOrigin::Unverified,
            "a choice the prefix contradicts is not the prefix's own"
        );
        let warn = warn.expect("a warning");
        assert!(warn.contains("tkg-11.7"), "{warn}");
        assert!(warn.contains("wineserver"), "must explain why: {warn}");
    }

    #[test]
    fn an_explicit_wine_matching_the_prefix_says_nothing() {
        let (_, origin, warn) = choose_wine(
            Some(p("/games/sc/runners/tkg-11.7/bin/wine")),
            Some(p("/games/sc/runners/tkg-11.7/bin/wine")),
            &[],
            None,
        )
        .expect("resolves");
        assert_eq!(warn, None);
        assert_eq!(
            origin,
            WineOrigin::Prefix,
            "corroborated by the prefix, so it may be called the prefix's own"
        );
    }

    /// The prefix may name no runner at all — a plain `WINEPREFIX` with no
    /// launch script and no bundled runners. Then there is nothing to
    /// corroborate `--wine` against, and nothing to contradict it either: it is
    /// used, it is not called the prefix's own, and the user is told nothing,
    /// because there is nothing established to tell them.
    #[test]
    fn an_explicit_wine_the_prefix_says_nothing_about_is_used_without_comment() {
        let (w, origin, warn) =
            choose_wine(Some(p("/usr/bin/wine")), None, &[], None).expect("resolves");
        assert_eq!(w, p("/usr/bin/wine"));
        assert_eq!(
            origin,
            WineOrigin::Unverified,
            "nothing here established whose prefix it owns"
        );
        assert_eq!(warn, None, "a mismatch warning needs something to mismatch");
    }

    /// The tier that exists for the LUG Star Citizen layout has to survive the
    /// shape of a real script: a shebang, blank lines, and other statements
    /// before the one it wants. Giving up at the first line that was not the
    /// assignment gave up on line 1 of every one of them, so the most
    /// authoritative answer available — literally what launches the game — was
    /// never consulted, and resolution fell through towards the system wine.
    #[test]
    fn the_launch_script_is_read_past_its_shebang_and_blank_lines() {
        let root = std::env::temp_dir().join(format!("tobii-launch-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let bin = root.join("runners/tkg-11.7/bin");
        std::fs::create_dir_all(&bin).expect("runner dir");
        std::fs::write(bin.join("wine"), b"#!/bin/sh\n").expect("wine");
        std::fs::write(
            root.join("sc-launch.sh"),
            format!(
                "#!/usr/bin/env bash\n\
                 \n\
                 # Configure the prefix\n\
                 export WINEPREFIX=\"$HOME/Games/star-citizen\"\n\
                 export wine_path=\"{}\"\n",
                bin.display()
            ),
        )
        .expect("launch script");
        assert_eq!(
            wine_from_launch_script(&root),
            Some(bin.join("wine")),
            "the assignment is never the first line of a real script"
        );
        // A script that names no runner is still no answer, not a wrong one.
        std::fs::write(root.join("sc-launch.sh"), "#!/bin/sh\n\necho hello\n").expect("script");
        assert_eq!(wine_from_launch_script(&root), None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_wine_anywhere_is_an_error_naming_the_flag() {
        let err = choose_wine(None, None, &[], None).expect_err("must fail");
        assert!(err.contains("--wine"), "{err}");
    }

    /// Only the FreeTrack DLL is required, and that is the whole architecture
    /// in one assertion.
    ///
    /// `tobii-bridge.exe` used to be required, because it was the thing that
    /// received frames and created `FT_SharedMem`. The DLLs feed themselves
    /// now, so a prefix without the exe works and the exe is a diagnostic. If
    /// this ever goes back to requiring it, the self-feeding path has been
    /// broken and nobody would otherwise notice until a game saw nothing.
    #[test]
    fn only_the_freetrack_dll_is_required() {
        let required: Vec<&str> = ARTIFACTS
            .iter()
            .filter(|(_, req)| *req)
            .map(|(n, _)| *n)
            .collect();
        // Exactly one entry is required and it is that name — which is also
        // what `artifact_dir` recognises a build directory by, so a directory
        // holding a complete installation is always found.
        assert_eq!(required, vec![REQUIRED_ARTIFACT]);
        for optional in ["tobii-bridge.exe", "NPClient64.dll"] {
            assert!(
                ARTIFACTS.iter().any(|(n, req)| *n == optional && !req),
                "{optional} must be optional"
            );
        }
    }

    /// Steam records the Proton build a prefix belongs to, and using any other
    /// wine on it runs `wineboot -u` and upgrades the prefix — so this must
    /// read the recorded one rather than fall back to `$PATH`.
    #[test]
    fn the_proton_build_is_read_from_the_prefix_it_belongs_to() {
        let tmp = std::env::temp_dir().join(format!("tobii-cfginfo-{}", std::process::id()));
        let proton = tmp.join("common/Proton - Experimental");
        let pfx = tmp.join("compatdata/359320/pfx");
        std::fs::create_dir_all(proton.join("files/bin")).expect("proton dir");
        std::fs::create_dir_all(&pfx).expect("prefix dir");
        std::fs::write(proton.join("files/bin/wine"), b"#!/bin/sh\n").expect("wine");
        std::fs::write(
            pfx.parent().expect("compatdata entry").join("config_info"),
            format!(
                "11.0-100\n{}/files/share/fonts/\n{}/files/lib/\n",
                proton.display(),
                proton.display()
            ),
        )
        .expect("config_info");

        assert_eq!(
            wine_from_steam_config_info(&pfx),
            Some(proton.join("files/bin/wine")),
            "the Proton build named in config_info must win"
        );

        // A prefix that is not a Steam one has no config_info and must simply
        // decline, leaving the existing sources to answer.
        assert_eq!(
            wine_from_steam_config_info(&tmp.join("not-a-steam-prefix")),
            None
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The whole point of the interop route: when a client that can answer
    /// NaturalPoint's signature check is present, TrackIR games must be sent to
    /// it, because ours is measurably rejected.
    #[test]
    fn an_installed_npclient_is_preferred_when_one_exists() {
        assert_eq!(
            choose_npclient(None, Some(p("/usr/libexec/opentrack"))).expect("resolves"),
            NpSource::Installed(p("/usr/libexec/opentrack"))
        );
        assert_eq!(
            choose_npclient(Some("auto"), Some(p("/usr/lib/opentrack"))).expect("resolves"),
            NpSource::Installed(p("/usr/lib/opentrack"))
        );
    }

    /// With nothing installed there is no better option than ours, even though a
    /// signature-checking game will reject it — the install path says so rather
    /// than leaving the key unset.
    #[test]
    fn ours_is_used_when_nothing_else_is_installed() {
        assert_eq!(
            choose_npclient(None, None).expect("resolves"),
            NpSource::Ours
        );
    }

    /// A game that does not check the signature works fine with ours, so the
    /// preference must be overridable.
    #[test]
    fn ours_can_be_forced_even_when_another_client_exists() {
        assert_eq!(
            choose_npclient(Some("ours"), Some(p("/usr/libexec/opentrack"))).expect("resolves"),
            NpSource::Ours
        );
    }

    /// What `install` prints about the client it registered, asked of the
    /// thing `install` actually prints.
    ///
    /// Its sibling below asks [`ours_for_trackir`] directly, which is a
    /// weaker question: the defect a user hit was not a wrong note, it was a
    /// correct note behind a gate, printed nowhere. Re-introducing that gate
    /// has to fail something, and this is the something.
    #[test]
    fn what_install_says_it_registered_carries_the_note_about_our_own_client() {
        let ours = registered_text(&NpSource::Ours, "ignored", true, None);
        assert!(
            ours.contains(&format!("registered {INSTALL_WIN_DIR} for TrackIR")),
            "it still says what it registered:\n{ours}"
        );
        assert!(
            ours.contains("NaturalPoint"),
            "and what that costs, in the same breath:\n{ours}"
        );

        // The other source says nothing about our own DLL, because it did not
        // register it for TrackIR — a note about its limits there would be
        // about a configuration this install did not make.
        let theirs = registered_text(
            &NpSource::Installed(PathBuf::from("/opt/opentrack")),
            r"C:\opentrack",
            false,
            None,
        );
        assert!(
            !theirs.contains("NaturalPoint's signature check"),
            "not in the installed-client case:\n{theirs}"
        );
        assert!(
            theirs.contains(r"C:\opentrack"),
            "which names where TrackIR points instead:\n{theirs}"
        );
    }

    /// The flag says which DLL to register. It does not say the person who
    /// passed it knows what that DLL cannot do — and the note used to be
    /// withheld from exactly the run that named it, so the one install that
    /// chose ours deliberately was the one install told nothing about the
    /// check. A user who passed it on advice spent an evening there.
    #[test]
    fn asking_for_ours_by_name_is_still_told_what_it_costs() {
        for explicit in [true, false] {
            let note = ours_for_trackir(explicit, None);
            assert!(note.contains("NaturalPoint"), "explicit={explicit}: {note}");
            // Over the measurements themselves, not over three strings typed
            // here: a third title has to reach this output, and a test that
            // names two would not notice it missing. Wrapping is stripped
            // because this renders folded to a terminal width.
            let flat = note.split_whitespace().collect::<Vec<_>>().join(" ");
            for m in tobii_config::signature::MEASURED {
                assert!(
                    flat.contains(m.title) && flat.contains(m.date),
                    "explicit={explicit}: {} is missing from:\n{note}",
                    m.title
                );
            }
            // It says what was measured, not what this user's game will do.
            assert!(
                note.contains("nothing here knows what your"),
                "explicit={explicit}: {note}"
            );
        }
    }

    /// When the flag turns down a client that is sitting right there, the note
    /// names that client and the flag value that would use it — and still does
    /// not refuse, because a FreeTrack title is a real reason to have asked.
    #[test]
    fn the_note_names_the_client_that_ours_was_chosen_over() {
        let note = ours_for_trackir(true, Some(Path::new("/usr/libexec/opentrack")));
        assert!(note.contains("/usr/libexec/opentrack"), "{note}");
        assert!(note.contains("--npclient auto"), "{note}");

        // Auto reaches ours only because there is nothing else, so there is
        // nothing to have been chosen over, and naming one would invent it.
        let auto = ours_for_trackir(false, None);
        assert!(
            !auto.contains("over the client"),
            "nothing was turned down here: {auto}"
        );
        assert!(
            auto.contains("no third-party NPClient64.dll was found"),
            "{auto}"
        );
    }

    #[test]
    fn an_explicit_directory_without_the_dll_is_refused() {
        let err = choose_npclient(Some("/nonexistent/dir"), None).expect_err("must fail");
        assert!(err.contains("NPClient64.dll"), "{err}");
    }

    /// Registering a host path through `Z:` is what lets a third-party client
    /// stay where its package manager put it, with nothing copied or
    /// redistributed.
    #[test]
    fn host_paths_are_registered_through_the_z_drive() {
        assert_eq!(
            wine_path_for(Path::new("/usr/libexec/opentrack")),
            r"Z:\usr\libexec\opentrack"
        );
        assert_eq!(wine_path_for(Path::new("/")), r"Z:\");
    }

    /// Both client ABIs are discovered through the registry, and the TrackIR key
    /// string is one this project confirmed inside StarCitizen.exe — a typo
    /// would mean the game silently never finds us.
    #[test]
    fn the_registry_keys_are_the_published_discovery_paths() {
        assert!(NP_KEY.contains(r"NaturalPoint\NATURALPOINT\NPClient Location"));
        assert!(FT_KEY.contains(r"Freetrack\FreeTrackClient"));
    }

    /// A fake `wine`: a shell script that records every argv it is handed and
    /// keeps the two `Path` values in files, answering `reg query` from them in
    /// wine's real output shape and applying `reg add` and `reg delete` to
    /// them. Stateful on purpose — the faults this file exists to prevent all
    /// live in the gap between what a key said when we installed and what it
    /// says when we uninstall, and a harness with canned answers only cannot
    /// reach them.
    ///
    /// Two details are copied from wine 11.18 rather than invented, because
    /// code here depends on both: `reg delete` of a value that is not there
    /// exits 1, and `reg query` of a key that is not there exits 1 while the
    /// probe key still answers.
    ///
    /// It takes its state directory from `WINEPREFIX`, which the code under
    /// test sets, so one script serves every test. That is not tidiness: see
    /// [`fake_wine`].
    /// It keeps the prefix's `user.reg` in step with those two values, in
    /// wine's own on-disk spelling, because that is what wine does and because
    /// [`status`] now reads the file instead of asking wine — see
    /// [`read_keys_read_only`]. A fake that wrote only its canned query output
    /// would leave `install` and `status` testable only in separate halves,
    /// which is precisely where a discrepancy between the writer and the reader
    /// would hide.
    const FAKE_WINE: &str = r#"#!/bin/sh
root=$(dirname "$WINEPREFIX")
{ for a in "$@"; do printf '[%s]' "$a"; done; printf '\n'; } >> "$root/argv"
if [ -f "$root/fail-wine" ]; then exit 1; fi
if [ -f "$root/fail-after-delete" ] && [ -f "$root/deleted" ]; then exit 1; fi
case "$3" in
  *NaturalPoint*) reply="$root/np.reply"; section='Software\\NaturalPoint\\NATURALPOINT\\NPClient Location' ;;
  *)              reply="$root/ft.reply"; section='Software\\Freetrack\\FreeTrackClient' ;;
esac
write_user_reg() {
  {
    printf 'WINE REGISTRY Version 2\n'
    printf ';; All keys relative to \\\\User\\\\S-1-5-21-0-0-0-1000\n\n#arch=win64\n\n'
    if [ -f "$root/ft.userreg" ]; then
      printf '[Software\\\\Freetrack\\\\FreeTrackClient] 1790444400\n#time=1dd4dde0e4c32d6\n'
      cat "$root/ft.userreg"
      printf '\n'
    fi
    if [ -f "$root/np.userreg" ]; then
      printf '[Software\\\\NaturalPoint\\\\NATURALPOINT\\\\NPClient Location] 1790444400\n#time=1dd4dde0e5bceee\n'
      cat "$root/np.userreg"
      printf '\n'
    fi
  } > "$WINEPREFIX/user.reg"
}
case "$section" in
  *NaturalPoint*) userreg="$root/np.userreg" ;;
  *)              userreg="$root/ft.userreg" ;;
esac
if [ "$1" = reg ] && [ "$2" = query ]; then
  if [ "$3" = 'HKCU\Software' ]; then exit 0; fi
  if [ -f "$reply" ]; then
    cat "$reply"
    if [ -f "$root/vanish" ]; then rm -f "$reply"; rm -f "$userreg"; write_user_reg; fi
    exit 0
  fi
  echo 'reg: the specified registry key was not found' >&2
  exit 1
fi
if [ "$1" = reg ] && [ "$2" = add ]; then
  if [ -f "$root/fail-add" ]; then echo 'reg: Zugriff verweigert' >&2; exit 1; fi
  echo 'reg: Der Vorgang wurde erfolgreich abgeschlossen'
  printf '\r\nHKEY_CURRENT_USER\\Whatever\r\n    Path    %s    %s\r\n\r\n' "$7" "$9" > "$reply"
  printf '"Path"="%s"\n' "$(printf '%s' "$9" | sed 's/\\/\\\\/g; s/"/\\"/g')" > "$userreg"
  write_user_reg
  exit 0
fi
if [ "$1" = reg ] && [ "$2" = delete ]; then
  if [ -f "$root/fail-add" ]; then echo 'reg: Zugriff verweigert' >&2; exit 1; fi
  if [ ! -f "$reply" ]; then exit 1; fi
  echo 'reg: Der Vorgang wurde erfolgreich abgeschlossen'
  rm -f "$reply"
  rm -f "$userreg"
  write_user_reg
  : > "$root/deleted"
  exit 0
fi
exit 0
"#;

    /// A "wine" that wrecks the prefix it is pointed at.
    ///
    /// Handed to a command that must not run wine at all, so that running it
    /// IS the failure. A fake that merely declines to write can only prove
    /// that this particular fake wrote nothing — which is all every install
    /// test in this file proved, and why a refusal that upgraded real prefixes
    /// passed every one of them.
    ///
    /// Modelled on what the host's wine does to a Proton-shaped prefix before
    /// it answers anything: `.update-timestamp` overwritten, `system.reg`
    /// rewritten, files under `drive_c` created and removed.
    const WRECKING_WINE: &str = "#!/bin/sh\n\
         rm -rf \"$WINEPREFIX/drive_c/windows\"\n\
         echo wrecked > \"$WINEPREFIX/system.reg\"\n\
         echo wrecked > \"$WINEPREFIX/.update-timestamp\"\n\
         echo wrecked > \"$WINEPREFIX/user.reg\"\n\
         exit 0\n";

    /// Both test scripts, written once and shared by every test.
    ///
    /// Once, and not once per test, for a reason that cost an afternoon: a
    /// script written by one thread and executed moments later fails with
    /// `ETXTBSY` if any other thread happened to fork in between, because the
    /// child inherits the still-open write descriptor until its own `execve`.
    /// The test binary runs its tests in parallel threads, each of them
    /// spawning wine, so a per-test script made roughly one run in fifteen fail
    /// in a different random test each time — which reads exactly like a flaky
    /// fix and is nothing of the kind. Measured on this machine at 14 failures
    /// in 400 exec-after-write attempts with four threads spawning alongside.
    ///
    /// `OnceLock` closes that window: the write happens while every other test
    /// is blocked on this very lock, so nothing in this process can fork during
    /// it. One lock for both scripts rather than one each, because that
    /// guarantee is "every test that will spawn anything is blocked here", and
    /// two locks would let a test past the first one fork during the second's
    /// write.
    fn scripts() -> &'static (PathBuf, PathBuf) {
        static SCRIPTS: std::sync::OnceLock<(PathBuf, PathBuf)> = std::sync::OnceLock::new();
        SCRIPTS.get_or_init(|| {
            (
                shared_script("tobii-fake-wine", FAKE_WINE),
                shared_script("tobii-wrecking-wine", WRECKING_WINE),
            )
        })
    }

    /// Put `body` at a stable path under a name of its own, executable.
    ///
    /// The finished file is renamed into place, by which time no write
    /// descriptor to it exists anywhere — so a second test process running at
    /// the same time can execute it safely, and reuses it instead of leaving a
    /// file of its own behind.
    fn shared_script(name: &str, body: &str) -> PathBuf {
        let shared = std::env::temp_dir().join(format!("{name}.sh"));
        if std::fs::read(&shared).is_ok_and(|b| b == body.as_bytes()) {
            return shared;
        }
        let staged = std::env::temp_dir().join(format!("{name}-{}.sh", std::process::id()));
        std::fs::write(&staged, body).expect("script");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x");
        match std::fs::rename(&staged, &shared) {
            Ok(()) => shared,
            Err(_) => staged,
        }
    }

    /// The one fake wine: a stateful registry, in wine's own output shape.
    fn fake_wine() -> &'static Path {
        &scripts().0
    }

    /// The one wrecking wine. See [`WRECKING_WINE`].
    fn wrecking_wine() -> &'static Path {
        &scripts().1
    }

    /// A prefix and an artifact directory, in one temp tree, served by that
    /// fake wine.
    ///
    /// The registry *is* the installation, so nothing install and uninstall do
    /// to it can be observed without a wine to talk to — which is why this
    /// exists rather than more pure-function tests. It needs no real Wine, no
    /// real prefix, no `$HOME` and no particular uid, so it runs unchanged in
    /// CI's root container.
    struct FakeWine {
        root: PathBuf,
    }

    impl FakeWine {
        fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("tobii-bridge-{tag}-{}", std::process::id()));
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(root.join("prefix/drive_c")).expect("prefix");
            std::fs::create_dir_all(root.join("artifacts")).expect("artifact dir");
            std::fs::write(root.join("artifacts").join(REQUIRED_ARTIFACT), b"dll").expect("dll");
            let fw = Self { root };
            // A booted prefix with nothing of ours in it, which is the state
            // every one of these tests starts from — and a booted prefix HAS a
            // `user.reg`. Without it "nothing is registered here" would be
            // reached through the file-missing path, and the tests would never
            // exercise the one that actually reads a registry.
            fw.write_user_reg();
            fw
        }

        /// Where the prefix keeps its `HKEY_CURRENT_USER` keys.
        fn user_reg(&self) -> PathBuf {
            self.prefix().join(crate::userreg::FILE)
        }

        /// Rebuild `user.reg` from whatever the two keys hold, the way wine
        /// writes it out.
        fn write_user_reg(&self) {
            let mut out = String::from(
                "WINE REGISTRY Version 2\n\
                 ;; All keys relative to \\\\User\\\\S-1-5-21-0-0-0-1000\n\n#arch=win64\n\n",
            );
            for (name, key, _) in KEYS {
                let Ok(assignment) =
                    std::fs::read_to_string(self.root.join(format!("{name}.userreg")))
                else {
                    continue;
                };
                let path = hkcu_path(key).expect("both discovery keys are HKCU");
                out.push_str(&format!(
                    "[{}] 1790444400\n#time=1dd4dde0e4c32d6\n{assignment}\n",
                    path.replace('\\', r"\\")
                ));
            }
            std::fs::write(self.user_reg(), out).expect("user.reg");
        }

        /// Put a `REG_SZ` in one key, on disk, escaped the way wine escapes it.
        fn registered_in_file(&self, which: &str, value: &str) {
            self.assigned_in_file(
                which,
                &format!(
                    "\"Path\"=\"{}\"",
                    value.replace('\\', r"\\").replace('"', "\\\"")
                ),
            );
        }

        /// The same, for a whole assignment written by hand — the only way to
        /// put a type or an escaping into the file that this installer would
        /// never write itself.
        fn assigned_in_file(&self, which: &str, assignment: &str) {
            std::fs::write(self.root.join(format!("{which}.userreg")), assignment).expect("seed");
            self.write_user_reg();
        }

        fn prefix(&self) -> PathBuf {
            self.root.join("prefix")
        }

        fn dest(&self) -> PathBuf {
            self.prefix().join(INSTALL_SUBDIR)
        }

        fn reply(&self, which: &str) -> PathBuf {
            self.root.join(format!("{which}.reply"))
        }

        /// Make one key hold `value`: in wine's own output shape AND in the
        /// prefix's `user.reg`, which is where real wine would also have put
        /// it.
        ///
        /// Both halves, because both are read. `install` and `uninstall`
        /// decide from the file when nothing is serving the prefix and from
        /// `reg query` only when the file cannot answer — see
        /// [`settled_keys`]. A key seeded into one half alone puts the prefix
        /// in a state no real prefix can be in, and a test set up that way
        /// proves nothing about either reader: it was how nine of these tests
        /// went on passing while install still ran wine to reach its refusal.
        fn registered(&self, which: &str, value: &str) {
            self.registered_as(which, "REG_SZ", value);
        }

        /// The same, for a value stored as some other type.
        fn registered_as(&self, which: &str, ty: &str, value: &str) {
            self.wine_reply_bytes(
                which,
                format!(
                    "\r\nHKEY_CURRENT_USER\\Software\\Whatever\r\n    \
                     Path    {ty}    {value}\r\n\r\n"
                )
                .as_bytes(),
            );
            let escaped = value.replace('\\', r"\\").replace('"', "\\\"");
            // Wine spells `REG_SZ` bare and every other string type with its
            // numeric type in front — `str(2)` is `REG_EXPAND_SZ`. A type this
            // does not know how to spell is one the file half cannot express,
            // and guessing would seed a line wine never writes.
            self.assigned_in_file(
                which,
                &match ty {
                    "REG_SZ" => format!("\"Path\"=\"{escaped}\""),
                    "REG_EXPAND_SZ" => format!("\"Path\"=str(2):\"{escaped}\""),
                    other => panic!("no user.reg spelling here for {other}"),
                },
            );
        }

        /// Wine's canned answer alone, byte for byte — for output that is not
        /// UTF-8, which is what wine prints for a path with a non-ASCII
        /// character in it.
        ///
        /// One half only, and named so it cannot be mistaken for the other:
        /// `user.reg` has no codepage, so a non-ASCII value has no equivalent
        /// there to seed. A test using this reaches wine's reader only from a
        /// prefix the file cannot answer for — see [`FakeWine::without_user_reg`].
        fn wine_reply_bytes(&self, which: &str, bytes: &[u8]) {
            std::fs::write(self.reply(which), bytes).expect("canned reply");
        }

        /// Take the prefix's `user.reg` away, so the command under test falls
        /// through to the wine reader.
        ///
        /// Not a contrivance: [`settled_keys`] hands `install` and `uninstall`
        /// the file where it can answer and wine where it cannot, and a prefix
        /// that has never been started has no `user.reg` at all. A test about
        /// wine's reader has to be in the state that reaches it.
        fn without_user_reg(&self) {
            std::fs::remove_file(self.user_reg()).expect("user.reg");
        }

        /// What the key holds now, as the fake wine would print it.
        fn current(&self, which: &str) -> String {
            std::fs::read_to_string(self.reply(which)).unwrap_or_default()
        }

        /// Make every `reg add` and `reg delete` fail, as a wine that cannot
        /// serve the prefix does.
        fn fail_writes(&self) {
            std::fs::write(self.root.join("fail-add"), b"").expect("switch");
        }

        /// Let them work again, for a test that has to get past a failed write
        /// to see what the failure left behind.
        fn allow_writes(&self) {
            std::fs::remove_file(self.root.join("fail-add")).expect("switch");
        }

        /// Make wine die from the first successful `reg delete` onwards — a
        /// build that stops serving the prefix in the middle of an uninstall,
        /// after one key's value is already gone.
        fn fail_after_delete(&self) {
            std::fs::write(self.root.join("fail-after-delete"), b"").expect("switch");
        }

        /// Make wine fail outright, as one pointed at a prefix it cannot use
        /// does — including the `reg query` that would otherwise read as
        /// "nothing is registered here".
        fn fail_wine(&self) {
            std::fs::write(self.root.join("fail-wine"), b"").expect("switch");
        }

        /// Make every value disappear the moment it has been read, which is
        /// what a second uninstall running alongside this one looks like from
        /// in here.
        fn vanish_after_read(&self) {
            std::fs::write(self.root.join("vanish"), b"").expect("switch");
        }

        /// Make the prefix look lived-in, so a wine run has something to
        /// damage — and give it the stale `.update-timestamp` a Proton prefix
        /// wears, which is what makes the host's wine upgrade it rather than
        /// leave it alone.
        fn lived_in(&self) {
            std::fs::create_dir_all(self.prefix().join("drive_c/windows/system32"))
                .expect("windows");
            std::fs::write(
                self.prefix().join("system.reg"),
                "WINE REGISTRY Version 2\n",
            )
            .expect("hklm");
            std::fs::write(self.prefix().join(".update-timestamp"), "0\n").expect("stamp");
        }

        /// A directory holding a third-party client, for `--npclient DIR`.
        fn npclient_dir(&self) -> PathBuf {
            self.npclient_dir_named("opentrack")
        }

        fn npclient_dir_named(&self, name: &str) -> PathBuf {
            let dir = self.root.join(name);
            std::fs::create_dir_all(&dir).expect("client dir");
            std::fs::write(dir.join("NPClient64.dll"), b"dll").expect("client dll");
            dir
        }

        fn argv(&self) -> String {
            std::fs::read_to_string(self.root.join("argv")).unwrap_or_default()
        }

        /// Forget the argv logged so far, so an assertion about what the next
        /// command did is not answered by what an earlier one did.
        fn forget_argv(&self) {
            std::fs::remove_file(self.root.join("argv")).ok();
        }

        fn record(&self) -> String {
            std::fs::read_to_string(self.dest().join(RECORD_FILE)).unwrap_or_default()
        }

        fn put_record(&self, text: &str) {
            std::fs::create_dir_all(self.dest()).expect("install dir");
            std::fs::write(self.dest().join(RECORD_FILE), text).expect("record");
        }

        fn run_args(&self, extra: &[&str]) -> Vec<String> {
            self.args("run", extra)
        }

        fn status_args(&self, extra: &[&str]) -> Vec<String> {
            self.args("status", extra)
        }

        fn args(&self, sub: &str, extra: &[&str]) -> Vec<String> {
            self.args_with_wine(sub, fake_wine(), extra)
        }

        /// The argv a test hands `tobii bridge <sub>`, spelled the way that
        /// subcommand reads it — with a wine of the test's own, for the
        /// commands that must not start one ([`wrecking_wine`]).
        ///
        /// One builder and not one per subcommand. Which flags a subcommand
        /// takes is knowledge that belongs next to [`SUBS`], which is where
        /// the gate reads it from; three hand-written copies of this body drift
        /// from each other, and did — the copy that pinned no `--npclient`
        /// tested a different report on a machine with opentrack installed
        /// from the one it tested on CI.
        fn args_with_wine(&self, sub: &str, wine: &Path, extra: &[&str]) -> Vec<String> {
            let mut v: Vec<String> = ["tobii", "bridge", sub]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
            for (flag, value) in [
                ("--prefix", self.prefix().display().to_string()),
                ("--wine", wine.display().to_string()),
            ] {
                v.push(flag.to_string());
                v.push(value);
            }
            // `install` is the only subcommand that reads it — see [`SUBS`] —
            // so a helper that put it on every argv would be building command
            // lines the gate refuses.
            if sub == "install" {
                v.push("--artifacts".to_string());
                v.push(self.root.join("artifacts").display().to_string());
            }
            // Pinned unless the test picks its own, so the outcome does not
            // depend on whether the machine running the test happens to have
            // opentrack installed. `status` reads it too — it is what the
            // report's refusal prediction is decided against — and, like
            // `--artifacts` above, it goes only on the subcommands that read
            // it: `run` and `uninstall` would be refused over it.
            if matches!(sub, "install" | "status") && !extra.contains(&"--npclient") {
                v.push("--npclient".to_string());
                v.push("ours".to_string());
            }
            v.extend(extra.iter().map(|s| (*s).to_string()));
            v
        }
    }

    impl Drop for FakeWine {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    /// A registered path may contain spaces, so the value is the rest of the
    /// line — one word of `C:\Program Files\opentrack` would be compared
    /// against ours and called someone else's.
    /// A value carrying a newline arrives split across two chunks, so what the
    /// first one holds is a fragment of it. The danger is precise: a fragment
    /// that happens to equal our own computed path would be read as ours and
    /// removed by uninstall, destroying a registration we never made. Neither
    /// half may be returned as a value.
    /// Both terminators, because the fragment a CRLF leaves is the dangerous
    /// one: it ends in the CR that wine ends its own lines with, so a guard
    /// that only asks whether the chunk kept its CR reads
    /// `C:\tobii-bridge<CR><LF>EVIL` as `C:\tobii-bridge` — byte for byte our
    /// own computed path, and `is_ours` then says yes. Install would overwrite
    /// a value it never wrote without the `--force` it never asked for, and
    /// uninstall would `reg delete` a registration this program never made:
    /// both safety properties failing in one read.
    ///
    /// The third case is a value that merely *ends* in CRLF. Nothing follows it
    /// but wine's own blank line, so the tail looks almost right — and what
    /// would be returned is still a fragment.
    ///
    /// Byte shapes checked against wine 11.18 rather than invented: `reg add
    /// /d $'C:\tobii-bridge\r\nEVIL'` prints its value back raw, breaks and all.
    #[test]
    fn a_value_broken_across_lines_is_not_read_as_its_first_half() {
        for out in [
            &b"\r\n    Path    REG_SZ    C:\\tobii-bridge\nEVIL\r\n\r\n"[..],
            &b"\r\n    Path    REG_SZ    C:\\tobii-bridge\r\nEVIL\r\n\r\n"[..],
            &b"\r\n    Path    REG_SZ    C:\\tobii-bridge\r\n\r\n\r\n"[..],
        ] {
            match reg_query_path(out) {
                Reading::Other(why) => assert!(
                    why.contains("line break"),
                    "a fragment must say why it is unreadable, got {why:?}"
                ),
                other => panic!("a fragment must never be a value or an absence: {other:?}"),
            }
            // And the whole point of refusing it: it must not pass for ours.
            assert!(!is_ours(&reg_query_path(out), Some(INSTALL_WIN_DIR)));
            assert_eq!(
                undo_for(&reg_query_path(out), None),
                Undo::Leave(whose(false))
            );
        }
    }

    /// The CR wine puts at the end of a line and a CR the value itself ends
    /// with are the same byte. Eating every one of them took the value's, so a
    /// path registered that way never matched itself again.
    #[test]
    fn a_value_ending_in_a_carriage_return_keeps_it() {
        let out = b"\r\n    Path    REG_SZ    C:\\odd\r\r\n\r\n";
        assert_eq!(
            reg_query_path(out),
            Reading::Plain("C:\\odd\r".to_string()),
            "wine's CR comes off, the value's own stays on"
        );
    }

    #[test]
    fn the_registered_path_is_read_out_of_reg_query_whole() {
        assert_eq!(
            reg_query_path(
                b"\r\nHKEY_CURRENT_USER\\Software\\Freetrack\\FreeTrackClient\r\n    \
                  Path    REG_SZ    C:\\Program Files\\opentrack\r\n\r\n"
            ),
            Reading::Plain(r"C:\Program Files\opentrack".to_string())
        );
        // What wine prints when there is nothing there.
        assert_eq!(
            reg_query_path(b"ERROR: The system was unable to find the specified registry key\n"),
            Reading::Absent
        );
        // A different value whose name merely starts with `Path`.
        assert_eq!(
            reg_query_path(b"    PathOther    REG_SZ    x\n"),
            Reading::Absent
        );
        assert_eq!(reg_query_path(b""), Reading::Absent);
        // Wine's separator is four fixed spaces and it pads nothing after the
        // value: `/d 'C:\x '` prints back with that trailing space, and it is
        // part of the value. Checked against wine 11.18.
        assert_eq!(
            reg_query_path(b"    Path    REG_SZ    C:\\x \r\n"),
            Reading::Plain("C:\\x ".to_string())
        );
    }

    /// `Absent` is the one answer whose action is to WRITE, so nothing that
    /// found a value may ever produce it. `value.trim()` used to: a `Path`
    /// holding nothing but spaces came back as "nothing is registered here",
    /// and install wrote straight over a value somebody else had put there
    /// without a word — the one live counterexample to "a comparison that goes
    /// wrong leaves the key alone".
    ///
    /// The same `trim()` was the other half of a worse one: a client directory
    /// whose path ends in a space registered a value this installer could never
    /// recognise as its own again, so every later install refused over its own
    /// work and uninstall left it behind for good.
    #[test]
    fn a_present_value_never_reads_as_an_absence() {
        for out in [
            // Nothing but whitespace — wine prints its four-space separator and
            // then the spaces that are the value, indistinguishable from each
            // other and so unusable either way.
            &b"    Path    REG_SZ       \r\n"[..],
            // An empty value: present, and naming no path.
            &b"    Path    REG_SZ    \r\n"[..],
        ] {
            let reading = reg_query_path(out);
            assert_ne!(
                reading,
                Reading::Absent,
                "something IS registered there: {reading:?}"
            );
            assert!(matches!(reading, Reading::Other(_)), "{reading:?}");
            assert!(!is_ours(&reading, Some(INSTALL_WIN_DIR)));
        }
    }

    /// `REG_SZ` is not a substring of `REG_EXPAND_SZ` — the letter after `REG_`
    /// differs — so a type matched as a substring reads `%ProgramFiles%\...`,
    /// which is the natural spelling for an installer script and legal for
    /// these keys, as *no value at all*, and overwrites it.
    #[test]
    fn a_type_this_installer_never_writes_is_a_registration_and_not_an_absence() {
        for out in [
            &b"    Path    REG_EXPAND_SZ    %ProgramFiles%\\opentrack\r\n"[..],
            &b"    Path    REG_MULTI_SZ    a\\0b\r\n"[..],
            &b"    Path    REG_DWORD    0x1\r\n"[..],
        ] {
            let reading = reg_query_path(out);
            assert!(matches!(reading, Reading::Other(_)), "{reading:?}");
            assert_ne!(reading, Reading::Absent, "something IS registered there");
            assert!(!is_ours(&reading, Some(INSTALL_WIN_DIR)));
        }
    }

    /// Wine's `reg.exe` prints the console's OEM codepage, not UTF-8: checked
    /// against wine 11.18 here, `C:\Program Files\Müller opentrack` comes back
    /// with a bare 0x81 and no locale setting changes it. Read lossily, that
    /// becomes U+FFFD — a string equal to neither what the registry holds nor
    /// anything we wrote, and every comparison against it is a guess.
    #[test]
    fn a_value_wine_cannot_print_faithfully_is_not_mistaken_for_ours() {
        let mut out: Vec<u8> = b"    Path    REG_SZ    C:\\Program Files\\M".to_vec();
        out.push(0x81);
        out.extend_from_slice(b"ller opentrack\r\n");
        let reading = reg_query_path(&out);
        assert!(
            matches!(&reading, Reading::Other(why) if why.contains("ASCII")),
            "{reading:?}"
        );
        // And it is emphatically not absent: something IS registered there.
        assert_ne!(reading, Reading::Absent);
        assert!(!is_ours(&reading, Some(INSTALL_WIN_DIR)));
    }

    /// The one comparison this module makes. Everything else follows from it:
    /// "ours" is written over and taken back out, and anything else is refused
    /// and left alone.
    #[test]
    fn only_a_value_this_installer_wrote_is_ours() {
        let sz = |v: &str| Reading::Plain(v.to_string());
        // Our own directory inside this prefix, which nothing else ever names —
        // recognised with no record at all, which is every install made before
        // the record existed. Windows paths are case-insensitive, and refusing
        // over a drive letter would be a refusal nobody could act on.
        assert!(is_ours(&sz(r"c:\TOBII-BRIDGE"), None));
        // The value our own last install wrote, however little it looks like
        // our directory: a run that pointed TrackIR at opentrack's client and a
        // run that pointed it at ours must not accuse each other.
        assert!(is_ours(
            &sz(r"Z:\usr\libexec\opentrack"),
            Some(r"Z:\usr\libexec\opentrack")
        ));
        // Someone else's, with or without a record.
        assert!(!is_ours(&sz(r"C:\opentrack"), None));
        assert!(!is_ours(&sz(r"C:\opentrack"), Some(INSTALL_WIN_DIR)));
        // An absence is not a value of ours either.
        assert!(!is_ours(&Reading::Absent, Some(INSTALL_WIN_DIR)));
    }

    /// Uninstall decides from what the key says now, not from the record
    /// alone — the record says what we left there, not what is there.
    #[test]
    fn a_key_that_no_longer_says_what_we_wrote_is_left_alone() {
        let ours = Reading::Plain(INSTALL_WIN_DIR.to_string());
        assert_eq!(undo_for(&ours, Some(INSTALL_WIN_DIR)), Undo::Remove);
        // Someone else's now: neither deleted nor overwritten.
        let theirs = Reading::Plain(r"Z:\usr\libexec\opentrack".to_string());
        let left = undo_for(&theirs, Some(INSTALL_WIN_DIR));
        assert!(
            matches!(left, Undo::Leave(why) if why.contains("no longer holds")),
            "{left:?}"
        );
        // A prefix installed before this record existed still has our own
        // directory in the key, and that directory is about to go.
        assert_eq!(undo_for(&ours, None), Undo::Remove);
        // But a third-party path an old install merely pointed at, with nothing
        // recorded, is not "no longer what we wrote" — nothing ever wrote down
        // what we wrote. Saying otherwise claims a comparison that never
        // happened, on the one path every pre-record install reaches.
        let unknown = undo_for(&theirs, None);
        assert!(
            matches!(unknown, Undo::Leave(why) if why.contains("nothing here records")),
            "{unknown:?}"
        );
        // Nothing registered is nothing to do, and nothing to warn about.
        assert_eq!(
            undo_for(&Reading::Absent, Some(INSTALL_WIN_DIR)),
            Undo::Nothing
        );
        // A value we cannot read is not one we wrote: install refuses to
        // register a path it could not read back.
        assert!(matches!(
            undo_for(
                &Reading::Other("because".to_string()),
                Some(INSTALL_WIN_DIR)
            ),
            Undo::Leave(_)
        ));
    }

    /// The record holds one fact per key and only one: the path this installer
    /// computed and wrote. Nothing read out of the registry is ever kept, so
    /// there is no stored copy of anyone else's value to write back.
    #[test]
    fn the_record_says_only_what_this_install_wrote() {
        let entries = [("ft", INSTALL_WIN_DIR), ("np", r"Z:\usr\libexec\opentrack")];
        let text = render_record(&entries);
        assert_eq!(
            parse_record(&text),
            vec![
                ("ft".to_string(), INSTALL_WIN_DIR.to_string()),
                ("np".to_string(), r"Z:\usr\libexec\opentrack".to_string()),
            ]
        );
        // A path with spaces is the rest of the line, not the first word.
        assert_eq!(
            parse_record(&render_record(&[("np", r"C:\Program Files\opentrack")])),
            vec![("np".to_string(), r"C:\Program Files\opentrack".to_string())]
        );
        // A damaged or truncated line is dropped, never guessed at: no record
        // and an unreadable one must both end in leaving the key alone.
        assert_eq!(
            parse_record("np wrote\nft\n# a comment\n\nnp held C:\\x\nnp wrote C:\\y\n"),
            vec![("np".to_string(), r"C:\y".to_string())]
        );
    }

    /// The rename that publishes a file is atomic; the write into the staging
    /// file is not. Two installs into one prefix sharing a staging name
    /// interleave their writes and rename a torn file into place.
    ///
    /// The DLLs are in here as well as the record, and they are the worse half:
    /// the record staged per process while `freetrackclient64.dll` still staged
    /// under one fixed `.<name>.new`, so two installs with different
    /// `--artifacts` directories handed the game a DLL made of both — and the
    /// loser, holding a descriptor the winner's rename turned into the
    /// published file, went on writing into the live DLL with nothing staged
    /// between it and the target.
    #[test]
    fn two_installers_do_not_stage_over_each_other() {
        assert_ne!(staging_name(RECORD_FILE, 11), staging_name(RECORD_FILE, 12));
        assert_ne!(staging_name(RECORD_FILE, 11), RECORD_FILE);
        for (name, _) in ARTIFACTS {
            assert_ne!(
                staging_name(name, 11),
                staging_name(name, 12),
                "{name} must stage per process, like the record"
            );
            assert_ne!(staging_name(name, 11), name, "{name}");
        }
    }

    /// And the copy loop must actually stage under that name, which is the half
    /// the record's fix left behind.
    ///
    /// Planted here is what another install has open mid-copy: a staging file
    /// under the fixed `.<name>.new` the DLLs used. Writing through it is the
    /// whole failure — both runs `fs::copy` into the one file, one renames the
    /// mixture onto the DLL the game loads, and the other is left writing into
    /// the published file itself, its staging gone out from under it.
    #[test]
    fn another_installs_staging_file_is_not_written_through() {
        let w = FakeWine::new("staging");
        std::fs::create_dir_all(w.dest()).expect("install dir");
        let theirs = w.dest().join(format!(".{REQUIRED_ARTIFACT}.new"));
        std::fs::write(&theirs, b"half of another install's DLL").expect("their staging file");
        install(&w.args("install", &[])).expect("installs");
        assert_eq!(
            std::fs::read(&theirs).ok().as_deref(),
            Some(&b"half of another install's DLL"[..]),
            "their staging file was copied into, and then renamed away as ours"
        );
        assert_eq!(
            std::fs::read(w.dest().join(REQUIRED_ARTIFACT))
                .ok()
                .as_deref(),
            Some(&b"dll"[..]),
            "and this install's own DLL still landed"
        );
    }

    /// The symmetry the refusals rest on: a value we could not read back is one
    /// we must not write either. Wine prints `…/ゲーム/…` back as `…\???\…`,
    /// which is pure ASCII and looks exactly like another program's path — so
    /// the next install refuses over its own work and uninstall never
    /// recognises it.
    #[test]
    fn a_client_path_that_cannot_be_read_back_is_refused() {
        let err = registrable_path(Path::new("/home/x/Spiele/öpentrack"))
            .expect_err("must refuse a path we could not read back");
        assert!(err.contains("ASCII"), "{err}");
        assert!(
            err.contains("--npclient ours"),
            "must say what to do: {err}"
        );
    }

    /// The same symmetry, one input class later. A Linux directory name may
    /// hold a newline, and `is_ascii()` accepts it because LF *is* ASCII — so
    /// `--npclient` pointing at one registered a value that
    /// [`reg_query_path`] then answered "a value with a line break in it"
    /// forever after: every later install refused over its own work and
    /// uninstall left the key behind for good, the exact outcome this refusal
    /// exists to prevent.
    #[test]
    fn a_client_path_carrying_a_line_break_is_refused() {
        for dir in ["/opt/my\nclient", "/opt/client\n", "/opt/my\rclient"] {
            let win = wine_path_for(Path::new(dir));
            assert!(
                win.is_ascii(),
                "the point of this one is that the ASCII guard lets it through: {win:?}"
            );
            let err = registrable_path(Path::new(dir))
                .expect_err("must refuse a path we could not read back");
            assert!(err.contains("line break"), "{err}");
            assert!(
                err.contains("--npclient ours"),
                "must say what to do: {err}"
            );
        }
        // The LF is the one that demonstrably cannot be read back: wine prints
        // it raw, and what comes back is a value with a break in it.
        let win = wine_path_for(Path::new("/opt/my\nclient"));
        assert!(
            matches!(
                reg_query_path(format!("\r\n    Path    REG_SZ    {win}\r\n\r\n").as_bytes()),
                Reading::Other(why) if why.contains("line break")
            ),
            "a value we could never read back: {win:?}"
        );
        // And an ordinary client directory still registers.
        assert_eq!(
            registrable_path(Path::new("/usr/libexec/opentrack")).as_deref(),
            Ok(r"Z:\usr\libexec\opentrack")
        );
    }

    /// "Left exactly as they are" is a warning about another program's
    /// registration. A prefix that never had the bridge has nothing in those
    /// keys at all, and must not be handed that paragraph about them.
    #[test]
    fn a_prefix_that_never_had_the_bridge_is_not_warned_about() {
        let empty = report(&[
            ("FreeTrack", FT_KEY, Outcome::Nothing),
            ("TrackIR", NP_KEY, Outcome::Nothing),
        ]);
        assert_eq!(
            empty,
            "no head-tracking client was registered in this prefix\n"
        );
        // The ordinary install-then-uninstall case, unchanged.
        assert_eq!(
            report(&[
                ("FreeTrack", FT_KEY, Outcome::Removed),
                ("TrackIR", NP_KEY, Outcome::Removed),
            ]),
            "unregistered TrackIR and FreeTrack client paths\n"
        );
        // And the warning is still there for what it is for.
        let foreign = report(&[
            ("FreeTrack", FT_KEY, Outcome::Removed),
            ("TrackIR", NP_KEY, Outcome::Left("because")),
        ]);
        assert!(
            foreign.contains("unregistered the FreeTrack client path"),
            "{foreign}"
        );
        assert!(foreign.contains("left\nexactly as they are"), "{foreign}");
        assert!(foreign.contains(NP_KEY), "must name the key: {foreign}");
        assert!(foreign.contains("because"), "and the reason: {foreign}");
    }

    /// The bug this whole path exists for: a prefix that already has a working
    /// head-tracking setup must not be overwritten by a blind `reg add`. And
    /// the way out must not be a command that damages a different prefix: a
    /// bare `wine` acts on `~/.wine`, and a foreign wine build runs
    /// `wineboot -u` against a Proton prefix and upgrades it out from under the
    /// game — which is the reason this module resolves a wine at all.
    ///
    /// Including by running it itself, which is what this used to do: the
    /// refusal read the keys with `wine reg query`, so the sentence warning
    /// against the upgrade was printed by a run that had just performed one.
    /// The argv assertion below is the whole of that fix — nothing may be
    /// spawned on the way to saying no.
    #[test]
    fn install_refuses_to_clobber_another_programs_registration() {
        let w = FakeWine::new("refuse");
        w.registered("np", r"Z:\usr\libexec\opentrack");
        w.registered("ft", r"Z:\usr\libexec\opentrack");
        let err = install(&w.args("install", &[]))
            .expect_err("must refuse")
            .to_string();
        assert!(
            err.contains(r"Z:\usr\libexec\opentrack"),
            "must say what is registered: {err}"
        );
        assert!(err.contains("--force"), "must name the way through: {err}");
        assert!(
            err.contains(&format!("WINEPREFIX={}", shell_quoted(&w.prefix()))),
            "the remedy must name the prefix it is about: {err}"
        );
        assert!(
            err.contains(&shell_quoted(fake_wine())),
            "and the wine that serves it: {err}"
        );
        let argv = w.argv();
        assert!(
            argv.is_empty(),
            "nothing may be written, and nothing spawned either — a wine started \
             here boots the prefix this run is refusing to touch: {argv}"
        );
        assert!(!w.dest().exists(), "and nothing copied either");
        // It did read, out of the prefix's own file: a run that answered
        // nothing would also have started nothing.
        assert!(
            w.user_reg().is_file(),
            "the read has to have come from somewhere"
        );
    }

    /// The refusal may say only what this run established. Two claims used to
    /// be made unconditionally, and both were reachable while false: that
    /// another program registered the value (with no record, the value may well
    /// be one of ours, from a release that kept none), and that the quoted
    /// binary is "this prefix's own wine" (it is whatever `wine` is on `$PATH`
    /// whenever the prefix names none — so the sentence warning against
    /// `wineboot -u` was printed over the command that performs it).
    #[test]
    fn the_refusal_claims_no_owner_and_no_wine_it_did_not_establish() {
        let taken = |recorded| {
            vec![Taken {
                abi: "TrackIR",
                key: NP_KEY,
                what: r"Z:\usr\libexec\opentrack".to_string(),
                want: INSTALL_WIN_DIR.to_string(),
                recorded,
            }]
        };
        // Nothing recorded: nothing to base an accusation on.
        let msg = refusal(
            Path::new("/games/pfx"),
            Path::new("/usr/bin/wine"),
            WineOrigin::Unverified,
            &taken(false),
        );
        assert!(msg.contains("no telling whose it is"), "{msg}");
        assert!(
            !msg.contains("another program has already registered"),
            "no program may be named that nothing names: {msg}"
        );
        assert!(
            !msg.contains("this prefix's own wine"),
            "a wine off $PATH is not the prefix's own: {msg}"
        );
        assert!(
            msg.contains("nothing here established"),
            "and it must say so: {msg}"
        );
        assert!(
            msg.contains("wineboot -u"),
            "the risk is still named: {msg}"
        );
        assert!(
            msg.contains("--force"),
            "the way through is still named: {msg}"
        );

        // A record naming a different value IS the evidence, and a wine the
        // prefix itself records may be called that.
        let msg = refusal(
            Path::new("/games/pfx"),
            Path::new("/games/pfx/runners/tkg/bin/wine"),
            WineOrigin::Prefix,
            &taken(true),
        );
        assert!(msg.contains("another program's now"), "{msg}");
        assert!(msg.contains("this prefix itself records"), "{msg}");
        assert!(!msg.contains("nothing here established"), "{msg}");

        // And on the way through with --force: a key that already held what we
        // then wrote lost nothing, so nothing is mourned.
        assert_eq!(
            replaced(&[Taken {
                abi: "TrackIR",
                key: NP_KEY,
                what: r"Z:\usr\libexec\opentrack".to_string(),
                want: r"z:\usr\libexec\opentrack".to_string(),
                recorded: true,
            }]),
            ""
        );
        assert!(replaced(&taken(true)).contains("nothing puts that back"));
    }

    /// The upgrade path off v0.4.0, which is the path every machine with
    /// opentrack installed takes automatically: that release registered the
    /// third-party client and kept no record of doing so. Reading the key it
    /// left and calling it another program's refuses the upgrade — over a write
    /// that would not change a byte — and accuses the user's own install.
    #[test]
    fn a_key_already_holding_what_we_would_write_with_nothing_recorded_installs() {
        let w = FakeWine::new("upgrade");
        let dir = w.npclient_dir();
        let win = wine_path_for(&dir);
        // Exactly what v0.4.0 leaves behind: both keys written, no record.
        std::fs::create_dir_all(w.dest()).expect("install dir");
        std::fs::write(w.dest().join(REQUIRED_ARTIFACT), b"dll").expect("dll");
        w.registered("ft", INSTALL_WIN_DIR);
        w.registered("np", &win);
        assert_eq!(w.record(), "", "v0.4.0 wrote no record");

        install(&w.args("install", &["--npclient", &dir.display().to_string()]))
            .expect("must not refuse over a value identical to the one it would write");
        let rec = w.record();
        assert!(
            rec.contains(&format!("np wrote {win}")),
            "and it is recorded now, so uninstall can take it back out: {rec}"
        );
        // A record naming something else is different evidence, and still
        // refused: that says the key changed hands since we wrote it.
        w.put_record(&format!("np wrote {INSTALL_WIN_DIR}\n"));
        let err = install(&w.args("install", &["--npclient", &dir.display().to_string()]))
            .expect_err("a key the record says we wrote something else into is not ours")
            .to_string();
        assert!(err.contains("another program's now"), "{err}");
    }

    /// A client path that ends in a space is still ours. Wine stores and prints
    /// it exactly (checked against 11.18), so trimming the value on the way in
    /// made the installer unable to recognise its own registration: the second
    /// install refused over its own work, permanently, and uninstall left it
    /// behind while calling it another program's.
    #[test]
    fn a_registered_path_ending_in_a_space_is_still_recognised_as_ours() {
        let w = FakeWine::new("trailspace");
        let dir = w.npclient_dir_named("opentrack ");
        let win = wine_path_for(&dir);
        assert!(win.ends_with(' '), "{win}");
        install(&w.args("install", &["--npclient", &dir.display().to_string()]))
            .expect("first install");
        install(&w.args("install", &["--npclient", &dir.display().to_string()]))
            .expect("must recognise the value it wrote itself");
        w.forget_argv();
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        assert!(
            w.argv().contains(r"[delete][HKCU\Software\NaturalPoint"),
            "and takes its own registration back out: {}",
            w.argv()
        );
    }

    /// A whitespace-only registration is somebody else's value, not an empty
    /// key: it must be refused, never silently overwritten.
    #[test]
    fn a_registration_of_nothing_but_spaces_is_not_an_empty_key() {
        let w = FakeWine::new("spaces");
        w.registered("np", "   ");
        // About `reg query`'s reader, which is reached from a prefix the file
        // cannot answer for. `user.reg` has its own wording for this shape and
        // its own test, in `crate::userreg`.
        w.without_user_reg();
        let err = install(&w.args("install", &[]))
            .expect_err("must refuse")
            .to_string();
        assert!(err.contains("no path at all"), "{err}");
        assert!(!w.argv().contains("[add]"), "{}", w.argv());
        assert!(!w.dest().exists(), "and nothing created");
    }

    /// The record said what this run MEANT to write, which stops being harmless
    /// the moment something else writes that same value. Reproduced end to end:
    /// an install whose `reg add` failed still recorded `np wrote <path>`,
    /// opentrack — whose own directory is the one `--npclient` picks — then
    /// registered that path for itself, and uninstall deleted it. No `--force`,
    /// and not one write of ours had landed.
    #[test]
    fn a_key_this_run_could_not_write_is_not_recorded_as_written() {
        let w = FakeWine::new("intent");
        let dir = w.npclient_dir();
        let win = wine_path_for(&dir);
        w.fail_writes();
        install(&w.args("install", &["--npclient", &dir.display().to_string()]))
            .expect_err("a registry that could not be written is a failed install");
        let rec = w.record();
        assert!(
            !rec.contains(&win),
            "nothing landed in a key, so nothing may claim it did: {rec}"
        );

        // opentrack now registers that very path for itself.
        w.allow_writes();
        w.registered("np", &win);
        w.forget_argv();
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        assert!(
            !w.argv().contains(r"[delete][HKCU\Software\NaturalPoint"),
            "a registration we never made is not ours to delete: {}",
            w.argv()
        );
        assert!(
            w.current("np").contains(&win),
            "it must still be registered: {}",
            w.current("np")
        );
    }

    /// A key an earlier install wrote and this one could not keeps its line: it
    /// still holds what that install put there, and dropping the claim would
    /// abandon a value only we can safely remove.
    #[test]
    fn a_failed_write_does_not_drop_what_an_earlier_install_recorded() {
        let w = FakeWine::new("keepline");
        let dir = w.npclient_dir();
        let win = wine_path_for(&dir);
        install(&w.args("install", &["--npclient", &dir.display().to_string()]))
            .expect("first install");
        w.fail_writes();
        install(&w.args("install", &["--npclient", "ours"])).expect_err("the writes fail");
        let rec = w.record();
        assert!(
            rec.contains(&format!("np wrote {win}")),
            "the earlier claim is still true and still needed: {rec}"
        );
    }

    /// Recovery from a wine that dies half way through an uninstall was always
    /// correct — running it again is idempotent. Being told nothing was touched
    /// when a value is already gone was not.
    #[test]
    fn uninstall_reports_what_it_already_did_when_the_next_read_fails() {
        let w = FakeWine::new("halfway");
        install(&w.args("install", &[])).expect("installs");
        let record = read_record(&w.dest());
        w.fail_after_delete();
        let (outcomes, unreadable) =
            undo_keys(fake_wine(), &w.prefix(), &record).expect("no io failure");
        assert!(
            unreadable.is_some(),
            "the second read must fail: {outcomes:?}"
        );
        assert!(
            outcomes.iter().any(|(_, _, o)| *o == Outcome::Removed),
            "what is already gone must still be reported: {outcomes:?}"
        );
        assert!(
            report(&outcomes).contains("unregistered the FreeTrack client path"),
            "{}",
            report(&outcomes)
        );
    }

    /// `--force` is the deliberate override, and it promises nothing: what it
    /// replaced is not written down anywhere, because a value this installer
    /// did not compute is a value it will never write.
    #[test]
    fn force_replaces_it_and_records_only_what_it_wrote() {
        let w = FakeWine::new("force");
        w.registered("np", r"Z:\usr\libexec\opentrack");
        w.registered("ft", r"C:\freetrack");
        install(&w.args("install", &["--force"])).expect("installs");
        let rec = w.record();
        assert!(rec.contains(r"np wrote C:\tobii-bridge"), "{rec}");
        assert!(rec.contains(r"ft wrote C:\tobii-bridge"), "{rec}");
        assert!(
            !rec.contains("opentrack") && !rec.contains(r"C:\freetrack"),
            "no copy of what was there may be kept: {rec}"
        );
        let argv = w.argv();
        assert!(
            argv.contains(
                r"[add][HKCU\Software\Freetrack\FreeTrackClient][/v][Path][/t][REG_SZ][/d][C:\tobii-bridge][/f]"
            ),
            "{argv}"
        );
        // And on the way out, nothing is put back: the keys we wrote are
        // cleared and nothing is written at all.
        w.forget_argv();
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(
            !argv.contains("[add]"),
            "nothing is ever written back: {argv}"
        );
        assert!(
            argv.contains(r"[delete][HKCU\Software\Freetrack\FreeTrackClient][/v][Path][/f]"),
            "{argv}"
        );
    }

    /// An install that pointed TrackIR at a third-party client leaves a value
    /// that is neither our directory nor what the next run would write. Without
    /// the record it reads as another program's and the installer accuses
    /// itself — which no user can act on.
    #[test]
    fn our_own_third_party_registration_is_not_another_programs() {
        let w = FakeWine::new("ourown");
        let dir = w.npclient_dir();
        let win = wine_path_for(&dir);
        install(&w.args("install", &["--npclient", &dir.display().to_string()]))
            .expect("first install");
        assert!(w.current("np").contains(&win), "{}", w.current("np"));
        let rec = w.record();
        assert!(rec.contains(&format!("np wrote {win}")), "{rec}");
        // The same installer, now asked for its own DLL.
        install(&w.args("install", &["--npclient", "ours"]))
            .expect("must not refuse about a value it wrote itself");
        w.forget_argv();
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(
            !argv.contains("[add]"),
            "nothing may be written into a prefix on the way out: {argv}"
        );
        assert!(
            argv.contains(
                r"[delete][HKCU\Software\NaturalPoint\NATURALPOINT\NPClient Location][/v][Path][/f]"
            ),
            "{argv}"
        );
    }

    /// A key another program claimed in the weeks since our install is not ours
    /// to overwrite, even when the value we would write is the very one it
    /// holds — that value is there because *they* put it there.
    #[test]
    fn a_key_another_program_claimed_since_our_install_is_refused() {
        let w = FakeWine::new("claimedsince");
        install(&w.args("install", &[])).expect("first install");
        let dir = w.npclient_dir();
        let win = wine_path_for(&dir);
        // opentrack is installed afterwards and registers its own client.
        w.registered("np", &win);
        let err = install(&w.args("install", &["--npclient", &dir.display().to_string()]))
            .expect_err("must refuse")
            .to_string();
        assert!(err.contains(&win), "{err}");
        // And the record still says only what we wrote, unchanged by the run
        // that refused.
        let rec = w.record();
        assert!(rec.contains(r"np wrote C:\tobii-bridge"), "{rec}");
        assert!(
            !rec.contains(&win),
            "nothing of theirs is written down: {rec}"
        );
    }

    /// A value stored as a type this installer never writes is a registration
    /// like any other: it must be seen and refused, never read as an absence
    /// and overwritten — and once forced, no copy of it is kept.
    #[test]
    fn an_expandable_registration_is_refused_and_never_written_back() {
        let w = FakeWine::new("expand");
        w.registered_as("np", "REG_EXPAND_SZ", r"%ProgramFiles%\opentrack");
        let err = install(&w.args("install", &[]))
            .expect_err("must refuse")
            .to_string();
        assert!(err.contains("REG_EXPAND_SZ"), "{err}");
        assert!(!w.dest().exists(), "and nothing created");
        install(&w.args("install", &["--force"])).expect("installs");
        let rec = w.record();
        assert!(!rec.contains("ProgramFiles"), "no copy is kept: {rec}");
        w.forget_argv();
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        assert!(!w.argv().contains("[add]"), "{}", w.argv());
    }

    /// Wine prints the registry in the console's OEM codepage, so a value with
    /// a character outside ASCII cannot be read back exactly. That is a reason
    /// to refuse, and — since nothing is ever written back — no reason to
    /// refuse `--force`: what --force promises is the same nothing either way.
    #[test]
    fn a_value_that_cannot_be_read_is_refused_but_force_may_proceed() {
        let w = FakeWine::new("codepage");
        let mut reply: Vec<u8> =
            b"\r\nHKEY_CURRENT_USER\\Whatever\r\n    Path    REG_SZ    C:\\Program Files\\M"
                .to_vec();
        reply.push(0x81);
        reply.extend_from_slice(b"ller opentrack\r\n\r\n");
        w.wine_reply_bytes("np", &reply);
        // Wine's reader is the one that cannot read these bytes: `user.reg`
        // has no codepage and decodes the character exactly, so there is
        // nothing to seed there and nothing for it to refuse.
        w.without_user_reg();
        let err = install(&w.args("install", &[]))
            .expect_err("must refuse")
            .to_string();
        assert!(err.contains("outside ASCII"), "{err}");
        assert!(
            err.contains(r"HKCU\Software\NaturalPoint"),
            "must name the key: {err}"
        );
        assert!(
            !err.contains("codepage rather than UTF-8"),
            "the refusal must not assert a cause it has not established: {err}"
        );
        assert!(!w.argv().contains("[add]"), "{}", w.argv());
        assert!(!w.dest().exists(), "and nothing copied");
        install(&w.args("install", &["--force"])).expect("--force goes ahead");
        let rec = w.record();
        assert!(
            rec.is_ascii(),
            "nothing unreadable reaches the record: {rec}"
        );
        assert!(!rec.contains('\u{fffd}'), "{rec}");
    }

    /// The same symmetry from the other side: a client path we could not read
    /// back out of the registry is refused before anything exists, rather than
    /// written and then unrecognisable for ever.
    #[test]
    fn a_non_ascii_client_path_is_refused_before_anything_is_created() {
        let w = FakeWine::new("nonascii");
        let dir = w.npclient_dir_named("öpentrack");
        let err = install(&w.args("install", &["--npclient", &dir.display().to_string()]))
            .expect_err("must refuse")
            .to_string();
        assert!(err.contains("ASCII"), "{err}");
        assert!(!w.dest().exists(), "nothing may be created");
        assert!(
            w.argv().is_empty(),
            "and the registry not even read: {}",
            w.argv()
        );
    }

    /// The undo is the `Path` value we added, not the whole key: install may
    /// well have added `Path` to a key another program created and keeps its
    /// own settings under.
    #[test]
    fn uninstall_removes_only_the_path_value_it_wrote() {
        let w = FakeWine::new("removevalue");
        install(&w.args("install", &[])).expect("installs");
        w.forget_argv();
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(
            argv.contains(
                r"[delete][HKCU\Software\NaturalPoint\NATURALPOINT\NPClient Location][/v][Path][/f]"
            ),
            "{argv}"
        );
        assert!(
            argv.contains(r"[delete][HKCU\Software\Freetrack\FreeTrackClient][/v][Path][/f]"),
            "{argv}"
        );
        assert!(!argv.contains("[add]"), "nothing is written back: {argv}");
        assert!(!w.dest().exists(), "the directory goes last, but it goes");
    }

    /// The fault, mirrored on the way out: the record says what the key held
    /// when we installed, and the user has had weeks to install opentrack since.
    #[test]
    fn uninstall_leaves_a_key_another_program_claimed_after_our_install() {
        let w = FakeWine::new("claimed");
        install(&w.args("install", &[])).expect("installs");
        // opentrack is installed later and registers itself.
        w.registered("np", r"Z:\usr\libexec\opentrack");
        w.forget_argv();
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(
            !argv.contains(r"[delete][HKCU\Software\NaturalPoint"),
            "opentrack's registration is not ours to delete: {argv}"
        );
        assert!(
            w.current("np").contains(r"Z:\usr\libexec\opentrack"),
            "it must still be registered: {}",
            w.current("np")
        );
        // Ours is still ours, and still goes.
        assert!(
            argv.contains(r"[delete][HKCU\Software\Freetrack\FreeTrackClient][/v][Path][/f]"),
            "{argv}"
        );
    }

    /// With no record there is nothing to say the key is ours, and deleting
    /// another program's registration on the way out is the fault being fixed.
    ///
    /// And an uninstall that takes nothing out writes nothing, so it has no
    /// business starting wine to find that out: it used to, and moved a
    /// Proton-shaped prefix's `.update-timestamp` while reporting that it had
    /// left everything alone.
    #[test]
    fn uninstall_without_a_record_leaves_a_foreign_key_alone() {
        let w = FakeWine::new("norecord");
        std::fs::create_dir_all(w.dest()).expect("install dir");
        std::fs::write(w.dest().join(REQUIRED_ARTIFACT), b"dll").expect("dll");
        w.registered("np", r"Z:\usr\libexec\opentrack");
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(
            argv.is_empty(),
            "nothing comes out, so nothing may be deleted, written, or even \
             spawned: {argv}"
        );
        assert!(!w.dest().exists(), "the directory still goes");
    }

    /// But a key still pointing at the directory we are about to delete IS
    /// ours — nothing else ever names it — and leaving it behind is residue in
    /// every prefix installed before this record existed, which is exactly the
    /// path the full-uninstall instructions send people down.
    #[test]
    fn uninstall_removes_our_own_pointer_even_with_no_record() {
        let w = FakeWine::new("older");
        std::fs::create_dir_all(w.dest()).expect("install dir");
        std::fs::write(w.dest().join(REQUIRED_ARTIFACT), b"dll").expect("dll");
        w.registered("np", INSTALL_WIN_DIR);
        w.registered("ft", INSTALL_WIN_DIR);
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(
            argv.contains(r"[delete][HKCU\Software\Freetrack\FreeTrackClient][/v][Path][/f]"),
            "{argv}"
        );
        assert!(w.current("np").is_empty(), "{}", w.current("np"));
    }

    /// A record that lost lines says nothing about what we wrote, and a key we
    /// cannot recognise is left alone — the direction a damaged record has to
    /// fail in.
    #[test]
    fn a_damaged_record_leaves_the_key_alone() {
        let w = FakeWine::new("damaged");
        let dir = w.npclient_dir();
        let win = wine_path_for(&dir);
        install(&w.args("install", &["--npclient", &dir.display().to_string()])).expect("installs");
        // A crash mid-write leaves the record with the TrackIR line gone.
        w.put_record("ft wrote C:\\tobii-bridge\n");
        w.forget_argv();
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(
            !argv.contains(r"[delete][HKCU\Software\NaturalPoint"),
            "a key nothing claims is left alone: {argv}"
        );
        assert!(w.current("np").contains(&win), "{}", w.current("np"));
    }

    /// `reg delete` exits 1 when the value is already gone (wine 11.18). A
    /// second uninstall running alongside this one, or a user who cleared the
    /// key by hand, leaves the prefix in exactly the state being asked for —
    /// reporting that as a failure keeps the install directory for a prefix
    /// that is already done.
    #[test]
    fn a_value_that_is_already_gone_is_not_a_failure() {
        let w = FakeWine::new("vanish");
        install(&w.args("install", &[])).expect("installs");
        w.vanish_after_read();
        w.forget_argv();
        uninstall(&w.args("uninstall", &[]))
            .expect("a prefix already in the wanted state is not a failure");
        assert!(!w.dest().exists(), "and the directory goes");
    }

    /// While a key still points into the directory, removing it leaves the
    /// prefix pointing at something that does not exist — and a script running
    /// `tobii bridge uninstall && rm -rf "$prefix"` must not proceed.
    #[test]
    fn a_failed_removal_keeps_the_directory_and_fails_the_command() {
        let w = FakeWine::new("failremove");
        install(&w.args("install", &[])).expect("installs");
        w.fail_writes();
        let err = uninstall(&w.args("uninstall", &[]))
            .expect_err("a failed removal is a failed uninstall")
            .to_string();
        assert!(err.contains("still points at"), "{err}");
        assert!(w.dest().exists(), "the directory must stay");
    }

    /// `reg query` fails both when nothing is registered and when wine cannot
    /// serve the prefix at all, and its message is localized. Reading the
    /// second as the first walks straight into overwriting whatever is there.
    #[test]
    fn a_registry_that_cannot_be_read_is_not_nothing_registered() {
        let w = FakeWine::new("blind");
        w.fail_wine();
        // No file to fall back on, so the question really does go to wine —
        // which is the state this test is about. With a readable `user.reg`
        // the registry CAN be read, and reading it is the right answer.
        w.without_user_reg();
        let err = install(&w.args("install", &[]))
            .expect_err("must not guess")
            .to_string();
        assert!(err.contains("could not read the registry"), "{err}");
        assert!(!w.argv().contains("[add]"), "{}", w.argv());
        assert!(!w.dest().exists(), "and nothing copied");
    }

    /// The registry path IS the installation, so a write that did not land is
    /// a failed install however many files were copied.
    #[test]
    fn a_registry_write_that_fails_still_fails_the_install() {
        let w = FakeWine::new("failwrite");
        w.fail_writes();
        let err = install(&w.args("install", &[]))
            .expect_err("must fail")
            .to_string();
        assert!(err.contains("registry keys could not be written"), "{err}");
    }

    /// `tobii bridge run` exists for the configuration where TrackIR is pointed
    /// at somebody else's client DLL — and the provider it starts used to write
    /// both discovery keys at its own directory on every single start, which
    /// takes that registration away and leaves the user with our unsigned
    /// client instead of the one they installed.
    ///
    /// Passed always, not only when a third-party client is registered: this
    /// side cannot read the keys without spending another wine invocation on
    /// it, and the flag costs nothing in the case where our own path is
    /// registered — the write it suppresses would have rewritten the value it
    /// already holds.
    #[test]
    fn run_starts_the_provider_with_the_registry_write_turned_off() {
        let w = FakeWine::new("runnoreg");
        std::fs::create_dir_all(w.dest()).expect("install dir");
        std::fs::write(w.dest().join("tobii-bridge.exe"), b"exe").expect("exe");
        run(&w.run_args(&[])).expect("run");
        let argv = w.argv();
        assert!(
            argv.contains("[C:\\tobii-bridge\\tobii-bridge.exe][--no-register]"),
            "{argv}"
        );
    }

    /// The flag the user passes still reaches the provider, behind the one this
    /// command adds. A `--port` swallowed by the new argument would be a silent
    /// downgrade to the default port, which looks exactly like "no frames
    /// arrive".
    #[test]
    fn a_port_still_reaches_the_provider() {
        let w = FakeWine::new("runport");
        std::fs::create_dir_all(w.dest()).expect("install dir");
        std::fs::write(w.dest().join("tobii-bridge.exe"), b"exe").expect("exe");
        run(&w.run_args(&["--port", "4999"])).expect("run");
        let argv = w.argv();
        assert!(argv.contains("[--no-register][--port][4999]"), "{argv}");
    }

    /// A child that is asked to stand down while a launch waits behind it is
    /// actually stopped, and the pid of the waiter comes back so the message
    /// can name it.
    ///
    /// The waiter is injected rather than staged as a real blocked `fcntl`,
    /// which would need a forked process inside a threaded test binary. What
    /// this covers is the half that acts: a live child, killed, reaped, and the
    /// loop returning rather than waiting for a process that is never going to
    /// exit on its own.
    #[test]
    fn a_waiting_launch_stops_the_child_and_names_the_waiter() {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exec sleep 30")
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let outcome = supervise(&mut child, || Some(1_140_518)).expect("supervise");
        assert!(
            matches!(outcome, Supervised::Yielded(1_140_518)),
            "{outcome:?}"
        );
        // Reaped, not merely signalled: a second wait would block forever on a
        // child still running, and `/proc/<pid>` outliving us is how a
        // "stopped" bridge goes on holding the lock.
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}/task")).exists(),
            "pid {pid} still alive"
        );
    }

    /// With nothing waiting, the child runs to completion and its status is
    /// what comes back — the ordinary case, which the watch loop must not
    /// change.
    #[test]
    fn a_child_nobody_is_waiting_for_runs_to_completion() {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 3")
            .spawn()
            .expect("spawn");
        let outcome = supervise(&mut child, || None).expect("supervise");
        match outcome {
            Supervised::Exited(status) => assert_eq!(status.code(), Some(3)),
            other => panic!("{other:?}"),
        }
    }

    /// The report's whole job is to answer "what state is this prefix in", and
    /// the first half of that answer is *which* prefix and *which* wine — a
    /// report that leaves either out is one nobody can act on, because every
    /// later line is about a prefix the reader has to guess at.
    ///
    /// The origin travels with the wine for the reason [`WineOrigin`] gives:
    /// nothing here established that a `$PATH` wine belongs to this prefix, and
    /// a report that called it the prefix's own would be the claim install's
    /// refusal was taught not to make.
    #[test]
    fn status_says_which_prefix_and_which_wine_and_where_each_came_from() {
        let fw = FakeWine::new("status-heading");
        let s = gather_status(&fw.status_args(&[])).expect("status");
        let out = render_status(&s);
        assert!(
            out.contains(&fw.prefix().display().to_string()),
            "must name the prefix it read: {out}"
        );
        assert!(
            out.contains("given with --prefix"),
            "must say how that prefix was chosen: {out}"
        );
        assert!(
            out.contains(&fake_wine().display().to_string()),
            "must name the wine it used: {out}"
        );
        // `--wine` with nothing in the prefix corroborating it is exactly
        // `Unverified`, and the words must not promise more.
        assert_eq!(s.origin, WineOrigin::Unverified);
        assert!(
            out.contains("not corroborated by this prefix"),
            "an unverified wine must be named as one: {out}"
        );
        assert!(
            !out.contains("the build this prefix itself records"),
            "must not claim a prefix corroborated it: {out}"
        );
        assert!(
            out.lines().any(|l| l.starts_with("wineserver ")),
            "must report whether anything is serving the prefix: {out}"
        );
    }

    /// State one of the three: a prefix nobody has installed into. Every line
    /// has to say so plainly, because this is the state a user is in when the
    /// install they thought they ran went somewhere else.
    #[test]
    fn status_reports_a_prefix_with_nothing_installed() {
        let fw = FakeWine::new("status-empty");
        let out = render_status(&gather_status(&fw.status_args(&[])).expect("status"));
        assert!(out.contains("nothing of ours is installed here"), "{out}");
        assert_eq!(
            out.matches("nothing is registered here").count(),
            2,
            "both keys are empty and both must say so: {out}"
        );
        assert!(
            !out.contains("registered by this installer"),
            "nothing was registered, so nothing may be claimed: {out}"
        );
    }

    /// State two: our own install. The artifacts are listed by name — which is
    /// how a user whose `NPClient64.dll` never got built finds that out — and
    /// both keys read back as ours.
    #[test]
    fn status_reports_our_own_installation_as_ours() {
        let fw = FakeWine::new("status-ours");
        // `freetrackclient64.dll` is all `FakeWine::new` puts in the artifact
        // directory, so this install copies one file of the three and the
        // report has to distinguish the two that are missing.
        install(&fw.args("install", &[])).expect("install");
        let s = gather_status(&fw.status_args(&[])).expect("status");
        let out = render_status(&s);
        assert!(out.contains("freetrackclient64.dll  present"), "{out}");
        assert!(
            out.contains("NPClient64.dll         missing (optional)"),
            "{out}"
        );
        assert!(
            out.contains("tobii-bridge.exe       missing (optional)"),
            "{out}"
        );
        assert_eq!(
            out.matches("registered by this installer").count(),
            2,
            "install wrote both keys, so both must read as ours: {out}"
        );
        for k in &s.keys {
            assert_eq!(
                k.state(),
                KeyState::Ours(INSTALL_WIN_DIR.to_string()),
                "{k:?}"
            );
        }
    }

    /// The required artifact's absence is not a footnote: without
    /// `freetrackclient64.dll` there is nothing in the prefix for a game to
    /// load, however good the registry looks, and a report that filed it under
    /// "missing (optional)" would hide the one fact that explains the silence.
    #[test]
    fn a_missing_required_artifact_is_not_reported_as_optional() {
        let fw = FakeWine::new("status-missing-required");
        install(&fw.args("install", &[])).expect("install");
        std::fs::remove_file(fw.dest().join(REQUIRED_ARTIFACT)).expect("remove");
        let out = render_status(&gather_status(&fw.status_args(&[])).expect("status"));
        assert!(
            out.contains("freetrackclient64.dll  MISSING — nothing can load without it"),
            "{out}"
        );
        // The directory is still there, with the record in it, so this is not
        // the never-installed state and must not be reported as one: that
        // sentence sends the user to `install` with the keys already written.
        assert!(
            !out.contains("nothing of ours is installed here"),
            "an installed prefix missing one file is not an empty one: {out}"
        );
    }

    /// State three: somebody else's client is registered. That is the state
    /// install refuses over, and the report has to name it the same way — with
    /// [`whose`]'s own sentence, which says only what a missing record lets it
    /// say rather than accusing a program nothing here can name.
    #[test]
    fn status_reports_a_third_party_registration_as_not_ours() {
        let fw = FakeWine::new("status-theirs");
        fw.registered_in_file("np", r"Z:\usr\libexec\opentrack");
        let s = gather_status(&fw.status_args(&[])).expect("status");
        let out = render_status(&s);
        assert!(
            s.keys.iter().any(|k| k.abi == "TrackIR"
                && k.state()
                    == KeyState::Theirs(r"Z:\usr\libexec\opentrack".to_string(), whose(false))),
            "{:?}",
            s.keys
        );
        assert!(out.contains(r"Z:\usr\libexec\opentrack"), "{out}");
        assert!(out.contains("not this installer's"), "{out}");
        assert!(
            out.contains("there is no telling whose it is"),
            "with no record, the report may not say whose it is: {out}"
        );
        // And the other key, which nobody touched, is still an absence — the
        // two answers are not interchangeable.
        assert!(out.contains("nothing is registered here"), "{out}");
    }

    /// A TrackIR key we pointed at a third-party client is the one registration
    /// that cannot be recognised by its value alone, and [`RECORD_FILE`] is the
    /// only thing that tells it from that program's own. The report reads the
    /// record for exactly that reason, and the second half of this test is the
    /// control: take the record away and the same key becomes a stranger's.
    #[test]
    fn a_third_party_path_this_installer_registered_reads_as_ours() {
        let fw = FakeWine::new("status-recorded");
        let dir = fw.npclient_dir();
        install(&fw.args("install", &["--npclient", &dir.display().to_string()])).expect("install");
        let want = wine_path_for(&dir);
        let s = gather_status(&fw.status_args(&[])).expect("status");
        assert!(
            s.keys
                .iter()
                .any(|k| k.abi == "TrackIR" && k.state() == KeyState::Ours(want.clone())),
            "the record is what makes this one ours: {:?}",
            s.keys
        );
        // Control: the very same registry, with nothing recording that we
        // wrote it, is not ours — and the report says so in the words that
        // admit it cannot tell.
        std::fs::remove_file(fw.dest().join(RECORD_FILE)).expect("record");
        let s = gather_status(&fw.status_args(&[])).expect("status");
        assert!(
            s.keys
                .iter()
                .any(|k| k.abi == "TrackIR"
                    && k.state() == KeyState::Theirs(want.clone(), whose(false))),
            "without the record there is nothing to recognise it by: {:?}",
            s.keys
        );
    }

    /// A value this program cannot read whole is neither ours nor an absence,
    /// and the report must not round it to either: "nothing is registered" over
    /// a key that holds something is the sentence that sends a user to
    /// `install`, which would then be writing over a value it never wrote.
    #[test]
    fn status_reports_a_value_it_cannot_read_as_unreadable() {
        let fw = FakeWine::new("status-unreadable");
        // `%ProgramFiles%\opentrack` is the natural spelling for an installer
        // script and legal for this key, and wine stores it as `str(2)`.
        fw.assigned_in_file("ft", r#""Path"=str(2):"%ProgramFiles%\\opentrack""#);
        let s = gather_status(&fw.status_args(&[])).expect("status");
        let out = render_status(&s);
        assert!(
            s.keys.iter().any(|k| k.abi == "FreeTrack"
                && matches!(k.state(), KeyState::Unreadable(why) if why.contains("REG_EXPAND_SZ"))),
            "{:?}",
            s.keys
        );
        assert!(
            !out.contains("nothing is registered here\n             registered"),
            "{out}"
        );
        assert!(out.contains("`uninstall` leaves it alone"), "{out}");
        // And not the half-truth it used to print here: `install` does not
        // leave this key alone, it stops the whole command over it.
        assert!(out.contains("`install` refuses"), "{out}");
    }

    /// The one line in the report whose whole job is to be pasted back must
    /// not be a command that then refuses.
    ///
    /// `docs/wiki/Tools.md` promises the report decides "by exactly the rules
    /// `install` and `uninstall` act on". The classification did; this line did
    /// not — it asked only whether anything was ours, which folds a stranger's
    /// key and an unreadable one into "nothing". So a prefix whose FreeTrack
    /// key holds somebody else's path was told, three paragraphs under the
    /// report naming that very key, "Nothing of ours is in this prefix. To put
    /// it there: tobii bridge install …", over a command that answers "these
    /// head-tracking discovery keys hold something this install did not write".
    #[test]
    fn status_does_not_hand_back_an_install_that_would_refuse() {
        let fw = FakeWine::new("status-blocked");
        fw.registered_in_file("ft", r"C:\Program Files\opentrack");
        let out = render_status(&gather_status(&fw.status_args(&[])).expect("status"));
        assert!(
            !out.contains("Nothing of ours is in this prefix. To put it there"),
            "{out}"
        );
        assert!(out.contains("will not put it there"), "{out}");
        assert!(
            out.contains(FT_KEY),
            "and must name what is in the way: {out}"
        );
        // And the report is right about that: the command it withheld is the
        // one that refuses. Asserted here rather than taken on trust, because
        // the whole defect was a line that had stopped agreeing with it.
        install(&fw.args("install", &[])).expect_err("install refuses over that key");
    }

    /// The same guard in the other direction, which is the one it could not
    /// see.
    ///
    /// `install` has a fourth state the report's [`KeyState`] classification
    /// cannot express: a key holding, byte for byte, what this run would
    /// write, with nothing recorded either way, is installed straight past.
    /// What "what this run would write" *is* depends on `--npclient` — so a
    /// prefix where opentrack had registered its own client, which is the
    /// configuration this project recommends for TrackIR, was told `install`
    /// would refuse over that key, and then `install` did not.
    ///
    /// The previous test could never catch it: [`FakeWine::args`] pins
    /// `--npclient ours`, and with that value the TrackIR key's want is
    /// `INSTALL_WIN_DIR`, which `is_ours` already recognises — so the fourth
    /// state is unreachable. This one supplies the value that reaches it.
    #[test]
    fn the_report_and_install_agree_about_a_client_already_registered_at_our_target() {
        let fw = FakeWine::new("status-agrees-np");
        let dir = fw.npclient_dir();
        let np = dir.display().to_string();
        // Exactly what opentrack leaves behind on a prefix we have never
        // touched: its own path in the TrackIR key, and no record of ours.
        fw.registered_in_file("np", &wine_path_for(&dir));
        assert_eq!(fw.record(), "", "nothing of ours has been here");

        let out =
            render_status(&gather_status(&fw.status_args(&["--npclient", &np])).expect("status"))
                .to_string();
        assert!(
            !out.contains("will not put it there"),
            "install walks straight past this key, so the report may not \
             promise a refusal over it: {out}"
        );
        assert!(
            out.contains("Nothing of ours is in this prefix. To put it there"),
            "{out}"
        );
        // The command it hands back has to be the command it answered for: the
        // whole question turns on `--npclient`, so an install line without it
        // would describe a different run.
        assert!(
            out.contains(&format!(
                "tobii bridge install --prefix {} --wine {} --npclient {}",
                shell_quoted(&fw.prefix()),
                shell_quoted(fake_wine()),
                quoted(&np)
            )),
            "{out}"
        );
        // And it is right: that command goes through.
        install(&fw.args("install", &["--npclient", &np])).expect("install goes through");

        // The control, on the same prefix and the same key: pointed at OUR
        // DLL instead, the value is not what this run would write, and both
        // halves say so.
        let fw = FakeWine::new("status-agrees-np-control");
        fw.registered_in_file("np", &wine_path_for(&fw.npclient_dir()));
        let out = render_status(&gather_status(&fw.status_args(&[])).expect("status"));
        assert!(out.contains("will not put it there"), "{out}");
        assert!(out.contains(NP_KEY), "{out}");
        install(&fw.args("install", &[])).expect_err("and that one does refuse");
    }

    /// The other direction, and the same line: a report that has just said
    /// neither key could be read must not assert four lines later that there
    /// is nothing in them. Only one of those two sentences can be true, and
    /// the section above is the one that knows.
    #[test]
    fn status_does_not_contradict_itself_about_keys_it_could_not_read() {
        let fw = FakeWine::new("status-blocked-unreadable");
        // A directory where the file should be: `read` fails with EISDIR for
        // everyone, including CI's root.
        std::fs::remove_file(fw.user_reg()).expect("remove");
        std::fs::create_dir(fw.user_reg()).expect("dir in its place");
        let out = render_status(&gather_status(&fw.status_args(&[])).expect("status"));
        assert!(out.contains("neither key could be read"), "{out}");
        assert!(!out.contains("Nothing of ours is in this prefix."), "{out}");
        // It still hands back `install`, which is the right command here —
        // what it may not do is claim to know the keys are empty.
        assert!(out.contains("tobii bridge install"), "{out}");
    }

    /// A prefix whose `user.reg` cannot be read is not an empty prefix, and
    /// saying so must not cost the user the rest of the report: the prefix
    /// line, the wine line and the files above it are precisely what somebody
    /// answering the issue needs, and they are all still known.
    #[test]
    fn a_registry_that_cannot_be_read_is_reported_without_losing_the_report() {
        let fw = FakeWine::new("status-unreadable-file");
        // A directory where the file should be: `read` fails with EISDIR for
        // everyone, including CI's root, where a mode has no effect at all.
        std::fs::remove_file(fw.user_reg()).expect("remove");
        std::fs::create_dir(fw.user_reg()).expect("dir in its place");
        let s = gather_status(&fw.status_args(&[])).expect("a failed read is not a failed report");
        let out = render_status(&s);
        assert!(s.unreadable.is_some(), "{out}");
        assert!(
            out.contains(&fw.prefix().display().to_string()),
            "the prefix must still be named: {out}"
        );
        assert!(
            out.contains(&fake_wine().display().to_string()),
            "the wine uninstall would use must still be named: {out}"
        );
        assert!(
            out.contains("not the same as nothing being registered in them"),
            "the two answers must be kept apart in so many words: {out}"
        );
        assert!(
            !out.contains("nothing is registered here"),
            "a registry we could not read is not an empty registry: {out}"
        );
        // And the section must say so where the keys would have been: a
        // `registry` heading with nothing under it reads as an empty registry,
        // which is exactly the answer this one is not.
        assert!(
            out.contains("  neither key could be read"),
            "the empty section must be accounted for: {out}"
        );
    }

    /// The whole read-without-wine path rests on one fact about these two
    /// keys: they are both under `HKCU`, and a prefix keeps `HKCU` in one file.
    /// A third key added under `HKLM` would be looked for in `user.reg`, not
    /// found, and reported as "nothing is registered here" — an absence
    /// manufactured out of looking in the wrong place, which is the one answer
    /// this module is not allowed to invent.
    #[test]
    fn both_discovery_keys_live_in_user_reg() {
        for (_, key, abi) in KEYS {
            assert!(
                hkcu_path(key).is_some(),
                "{abi} ({key}) is not under HKCU, so it is not in user.reg"
            );
        }
    }

    /// An artifact this program cannot reach is not an artifact that is gone.
    ///
    /// `Path::is_file` folds every error into `false`, so an install directory
    /// the user cannot read reported all three files as missing — measured with
    /// mode `000` on a directory holding all three. "MISSING — nothing can load
    /// without it" about a file that is right there is the one failure class
    /// this command exists to prevent, and it sends the user to `install`.
    ///
    /// Reached here through a symlink that points at itself, which fails with
    /// `ELOOP` for every user including CI's root — a mode would not, since
    /// root ignores them.
    #[test]
    fn an_artifact_that_cannot_be_reached_is_not_reported_as_missing() {
        let fw = FakeWine::new("status-artifact-unreachable");
        install(&fw.args("install", &[])).expect("install");
        let stuck = fw.dest().join(REQUIRED_ARTIFACT);
        std::fs::remove_file(&stuck).expect("remove");
        std::os::unix::fs::symlink(REQUIRED_ARTIFACT, &stuck).expect("loop");
        let s = gather_status(&fw.status_args(&[])).expect("status");
        let out = render_status(&s);
        assert!(
            matches!(
                s.artifacts
                    .iter()
                    .find(|(n, _, _)| *n == REQUIRED_ARTIFACT)
                    .map(|(_, _, p)| p),
                Some(Presence::Unknown(_))
            ),
            "{:?}",
            s.artifacts
        );
        assert!(out.contains("could not tell whether it is here"), "{out}");
        assert!(
            !out.contains("MISSING — nothing can load without it"),
            "a file this program could not reach was reported as absent: {out}"
        );
    }

    /// A record that is there and unreadable is not "no record".
    ///
    /// [`read_record`] answers "no record" to a read that failed, which is the
    /// right answer for install and uninstall — it leads them to leave the key
    /// alone. For a report it changes what is said about somebody else's
    /// prefix: with no record, a TrackIR key this installer pointed at a
    /// third-party client is indistinguishable from that program's own, and the
    /// report calls it a stranger's. The report has to say which of the two
    /// happened.
    #[test]
    fn a_record_that_cannot_be_read_is_not_reported_as_no_record() {
        let fw = FakeWine::new("status-record-unreadable");
        let dir = fw.npclient_dir();
        install(&fw.args("install", &["--npclient", &dir.display().to_string()])).expect("install");
        let record = fw.dest().join(RECORD_FILE);
        std::fs::remove_file(&record).expect("remove");
        std::fs::create_dir(&record).expect("dir in its place");
        let s = gather_status(&fw.status_args(&[])).expect("status");
        let out = render_status(&s);
        assert!(s.record_unreadable.is_some(), "{out}");
        assert!(
            out.contains("could not be read"),
            "the report must say the record was unreadable: {out}"
        );
        // And it must say what that costs, because the line below it now calls
        // our own registration a stranger's.
        assert!(
            out.contains("is reported\n  below as a stranger's"),
            "{out}"
        );
    }

    /// The cost of reading `user.reg` instead of running wine, said out loud.
    ///
    /// A prefix's registry is written back when the last process on it exits,
    /// so while a wineserver is alive the file can lag. That is a real trade
    /// against the old path, which was accurate and destructive, and a stale
    /// reading printed as the current one would be a new way to mislead — which
    /// is the fault this change removes, not one it may relocate. Said only
    /// when it can be true: with nothing serving the prefix the file *is* the
    /// registry, and a caveat there teaches the user to discount a report that
    /// is exactly right.
    #[test]
    fn a_live_wineserver_makes_the_report_say_the_registry_may_lag() {
        assert_eq!(staleness(&crate::wineserver::Lock::Free), None);
        let held = staleness(&crate::wineserver::Lock::Held(4242)).unwrap_or_default();
        assert!(
            held.contains("a wine process is alive on this prefix"),
            "{held}"
        );
        assert!(held.contains("not in the file yet"), "{held}");
        let unknown =
            staleness(&crate::wineserver::Lock::Unknown("EACCES".into())).unwrap_or_default();
        assert!(unknown.contains("could not be\n  determined"), "{unknown}");

        let mut s = steam_status("2537590");
        s.server = crate::wineserver::Lock::Held(4242);
        let out = render_status(&s);
        assert!(out.contains("not in the file yet"), "{out}");
        // Under the registry heading, where the values it applies to are — not
        // in a footnote after them.
        let reg = out.find("\nregistry").expect("a registry heading");
        let caveat = out.find("not in the file yet").expect("the caveat");
        let first_key = out.find(FT_KEY).expect("the first key");
        assert!(reg < caveat && caveat < first_key, "{out}");

        s.server = crate::wineserver::Lock::Free;
        assert!(
            !render_status(&s).contains("not in the file yet"),
            "nothing is serving the prefix, so the file is the registry"
        );
    }

    /// `choose_wine`'s mismatch sentence is the line that most often explains
    /// "the game sees nothing", and it went to stderr — so a user who ran
    /// `tobii bridge status … > report.txt` and pasted the file sent us
    /// everything except the answer. It belongs in the report, which is the
    /// thing designed to be pasted.
    #[test]
    fn the_wine_mismatch_warning_is_in_the_report_and_not_only_on_stderr() {
        // The warning `choose_wine` actually produces, rather than one invented
        // here: an explicit `--wine` over a prefix that records its own.
        let (_, origin, warning) = choose_wine(
            Some(PathBuf::from("/usr/bin/wine")),
            Some(PathBuf::from("/games/Proton/files/bin/wine")),
            &[],
            None,
        )
        .expect("a choice");
        assert_eq!(origin, WineOrigin::Unverified);
        let warning = warning.expect("choose_wine warns about this one");

        let mut s = steam_status("2537590");
        s.wine_warning = Some(warning.clone());
        let out = render_status(&s);
        for line in warning.lines() {
            assert!(out.contains(line.trim()), "missing `{line}` from {out}");
        }
        // And it sits with the wine it is about, above the files and the
        // registry, not appended somewhere after them.
        let first = warning.lines().next().expect("a warning has a line").trim();
        let at = out.find(first).expect("the warning is in the report");
        let files = out.find("\nfiles").expect("a files section");
        assert!(at < files, "{out}");
    }

    /// **The fault this command shipped with.** It claimed to read and change
    /// nothing, and it read the registry by running `wine reg query` — and wine
    /// initialises or upgrades whatever prefix it is pointed at. Measured on
    /// throwaway prefixes with wine 11.18: 5510 paths created under a prefix
    /// holding only `drive_c`, and on a complete prefix with a stale
    /// `.update-timestamp` — which is what a Proton prefix looks like to the
    /// host's wine — 2744 lines of `system.reg` rewritten and the stamp
    /// overwritten. That is the prefix upgrade [`WineOrigin`] exists to warn
    /// about, performed by the command we tell a user to run first.
    ///
    /// **The old test could not see it, and this one has to.** It asserted
    /// against a fake wine that is a shell script doing nothing, so it was a
    /// true statement about our own code and a false one about the command.
    /// Two things make this one able to fail for the real reason:
    ///
    /// * the whole prefix tree is compared before and after — every path, its
    ///   size, its mtime to the nanosecond and a hash of its bytes — so
    ///   anything wine would have done to it shows up whether or not this file
    ///   was the thing that did it;
    /// * the `wine` handed to the command is a script that *wrecks* the prefix
    ///   when it runs. Running it is the failure. A fake that merely declines
    ///   to write can only prove that this particular fake wrote nothing.
    #[test]
    fn status_runs_no_wine_and_leaves_the_prefix_byte_for_byte_as_it_was() {
        let fw = FakeWine::new("status-readonly");
        fw.registered_in_file("np", r"Z:\usr\libexec\opentrack");
        fw.lived_in();
        let wrecker = wrecking_wine();

        let before = snapshot(&fw.prefix());
        // Through the shared builder, not a hand-written argv: this was the
        // one status test that passed no `--npclient`, so the report it
        // rendered on a machine with opentrack installed was not the report it
        // rendered on CI, and all four assertions below hold either way.
        let args = fw.args_with_wine("status", wrecker, &[]);
        let out = render_status(&gather_status(&args).expect("status"));

        assert_eq!(
            snapshot(&fw.prefix()),
            before,
            "the prefix changed while being asked what was in it"
        );
        assert!(
            !fw.dest().exists(),
            "asking the question created {}",
            fw.dest().display()
        );
        // And it did read the registry, out of the file: a command that
        // answered nothing would also leave the prefix alone.
        assert!(out.contains(r"Z:\usr\libexec\opentrack"), "{out}");
        assert!(
            out.contains(&fw.user_reg().display().to_string()),
            "the report must name the file it read: {out}"
        );
        assert_wrecker_wrecks(&fw, before);
    }

    /// The install half of the same test, and for the same reason: every
    /// install test in this file drives a fake wine that is a shell script
    /// doing nothing, so "the refusal wrote nothing" was a true statement
    /// about this code and a false one about the command.
    ///
    /// `wine reg query` is `wine`. Against a prefix whose `.update-timestamp`
    /// is stale — which is what a Proton prefix looks like to the host's wine,
    /// and the ordinary case, since the command announces the fallback itself
    /// ("no runner found inside the prefix, falling back to the system wine")
    /// — it runs `wineboot -u` before it answers anything. Measured with real
    /// wine 11.18 on a throwaway prefix: an install that printed the refusal,
    /// created no directory and wrote no key still moved the stamp and rewrote
    /// 2764 lines of `system.reg`, having just warned the user in that very
    /// refusal that a foreign wine "upgrades it out from under the game".
    #[test]
    fn a_refused_install_runs_no_wine_and_leaves_the_prefix_as_it_was() {
        let w = FakeWine::new("refuse-readonly");
        // A stranger's registration, put there the way a stranger would: in
        // the prefix's own file, with no wine of ours involved.
        w.registered_in_file("np", r"Z:\opt\SomeoneElse\tracker");
        w.lived_in();

        let before = snapshot(&w.prefix());
        let err = install(&w.args_with_wine("install", wrecking_wine(), &[]))
            .expect_err("a registration this install did not write must be refused")
            .to_string();

        assert!(err.contains(r"Z:\opt\SomeoneElse\tracker"), "{err}");
        assert_eq!(
            snapshot(&w.prefix()),
            before,
            "the prefix changed while being refused"
        );
        assert!(!w.dest().exists(), "and nothing was created");
        assert_wrecker_wrecks(&w, before);
    }

    /// And the same for an uninstall with nothing to take out, which is the
    /// other command that reaches a decision without writing. Measured the
    /// same way: `tobii bridge uninstall` on a prefix holding neither key
    /// printed "no head-tracking client was registered in this prefix", exited
    /// 0, removed nothing — and moved `.update-timestamp` from 0 to the host
    /// wine's own, rewriting 2764 lines of `system.reg` on the way.
    #[test]
    fn an_uninstall_with_nothing_to_remove_runs_no_wine() {
        let w = FakeWine::new("undo-readonly");
        w.lived_in();

        let before = snapshot(&w.prefix());
        uninstall(&w.args_with_wine("uninstall", wrecking_wine(), &[]))
            .expect("a prefix with nothing of ours in it is not a failure");

        assert_eq!(
            snapshot(&w.prefix()),
            before,
            "the prefix changed while being told there was nothing to undo"
        );
        assert_wrecker_wrecks(&w, before);
    }

    /// Prove the wrecking wine can wreck, in the test that just relied on it
    /// not having run.
    ///
    /// Without this, a script that could not be executed at all — a failed
    /// `chmod`, a `noexec` mount, the `ETXTBSY` window [`scripts`] describes —
    /// would satisfy every assertion above it. That is the exact shape of test
    /// this file has shipped three of, and the one thing that stops it is
    /// making the same test show the failure it claims to be watching for.
    fn assert_wrecker_wrecks(w: &FakeWine, before: String) {
        wine_output(wrecking_wine(), &w.prefix(), &["reg", "query", FT_KEY])
            .expect("the wrecking wine has to be runnable");
        assert_ne!(
            snapshot(&w.prefix()),
            before,
            "the wrecking wine did not wreck anything, so the assertion above \
             proved nothing about whether it ran"
        );
    }

    /// Every path under `root`, with everything about it that a wine run would
    /// disturb: its type, its size, its mtime to the nanosecond, and a digest
    /// of its bytes.
    ///
    /// Sorted, so two snapshots compare as text. The mtime matters as much as
    /// the content: wine rewrites `system.reg` with the same first lines, and a
    /// comparison of sizes alone called that untouched.
    fn snapshot(root: &Path) -> String {
        fn walk(dir: &Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                let Ok(md) = std::fs::symlink_metadata(&p) else {
                    continue;
                };
                use std::os::unix::fs::MetadataExt;
                let digest = std::fs::read(&p).map(|b| {
                    // Enough to catch a rewrite: length, and a rolling sum that
                    // order matters to.
                    b.iter()
                        .fold(1469598103934665603u64, |h, x| {
                            (h ^ u64::from(*x)).wrapping_mul(1099511628211)
                        })
                        .to_string()
                });
                out.push(format!(
                    "{} dir={} len={} mtime={}.{} mode={:o} {}",
                    p.display(),
                    md.is_dir(),
                    md.len(),
                    md.mtime(),
                    md.mtime_nsec(),
                    md.mode(),
                    digest.unwrap_or_default()
                ));
                if md.is_dir() {
                    walk(&p, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(root, &mut out);
        out.sort();
        out.join("\n")
    }

    /// Whether a game accepts our DLL is unknown to this project — two titles
    /// have ever been measured against NaturalPoint's signature check — so the
    /// report may describe the prefix and must never predict the game. A
    /// "ready" line here would come back to us in an issue as though it were
    /// evidence.
    #[test]
    fn status_never_predicts_what_the_game_will_do() {
        let fw = FakeWine::new("status-no-promises");
        install(&fw.args("install", &[])).expect("install");
        let out = render_status(&gather_status(&fw.status_args(&[])).expect("status"));
        let lower = out.to_lowercase();
        for claim in ["ready", "working", "will work", "you are all set"] {
            assert!(
                !lower.contains(claim),
                "the report promises `{claim}`: {out}"
            );
        }
        assert!(
            out.contains("It does not say"),
            "it must say what it is not saying: {out}"
        );
    }

    /// Two titles have been measured against the signature check now, and the
    /// second does not behave like the first: one rejects our DLL and gives
    /// up, the other asks again forever. A report that still said "Star
    /// Citizen aside" would be withholding the measurement that explains the
    /// symptom the second user actually had.
    #[test]
    fn status_names_both_titles_measured_against_the_signature_check() {
        let fw = FakeWine::new("status-two-measurements");
        install(&fw.args("install", &[])).expect("install");
        let out = render_status(&gather_status(&fw.status_args(&[])).expect("status"));
        for measured in [
            "Star Citizen (2026-08-15)",
            "Microsoft Flight Simulator 2024",
            "2026-09-27",
        ] {
            assert!(out.contains(measured), "{out}");
        }
        assert!(
            !out.contains("Star Citizen aside"),
            "one measurement was all there was, and it is not all there is: {out}"
        );
    }

    /// [`FakeWine::args`] pins `--npclient ours`, so this prefix is in exactly
    /// the configuration a TrackIR game gets nothing out of: the key pointing
    /// at a DLL that cannot answer the check. The report is the last place
    /// that can say so before the game is launched, and it costs one line.
    #[test]
    fn status_says_when_the_trackir_key_points_at_our_own_dll() {
        let fw = FakeWine::new("status-trackir-is-ours");
        install(&fw.args("install", &[])).expect("install");
        let out = render_status(&gather_status(&fw.status_args(&[])).expect("status"));
        assert!(out.contains("that is our own DLL"), "{out}");
        // Under the one key it is true of. FreeTrack is registered to the same
        // directory and has no signature check to fail.
        assert_eq!(out.matches("that is our own DLL").count(), 1, "{out}");

        // The control, on the same command with the other client: a TrackIR
        // key pointing at a third-party DLL is not that configuration and must
        // not be described as it.
        let other = FakeWine::new("status-trackir-is-theirs");
        let np = other.npclient_dir().display().to_string();
        install(&other.args("install", &["--npclient", &np])).expect("install");
        let out = render_status(
            &gather_status(&other.status_args(&["--npclient", &np])).expect("status"),
        );
        assert!(!out.contains("that is our own DLL"), "{out}");
        assert!(out.contains(NP_KEY), "{out}");
    }

    /// The report is written to be pasted into an issue, so it has to end with
    /// the two things the maintainer will ask for anyway — the way to undo the
    /// install, spelled with this prefix in it, and the other report.
    #[test]
    fn status_ends_with_the_way_out_and_the_other_report_to_send() {
        let fw = FakeWine::new("status-what-next");
        install(&fw.args("install", &[])).expect("install");
        let out = render_status(&gather_status(&fw.status_args(&[])).expect("status"));
        assert!(
            out.contains(&format!(
                "tobii bridge uninstall --prefix {} --wine {}",
                shell_quoted(&fw.prefix()),
                shell_quoted(fake_wine())
            )),
            "the undo command must be the one that works here — this run needed \
             --wine, and uninstall resolves wine the same way: {out}"
        );
        assert!(out.contains("tobii debug"), "{out}");
        // Control on the other half: with no `--wine` of the user's own, the
        // command must not carry one — a flag they never passed is one more
        // path to check before pasting.
        let s = Status {
            wine_given: false,
            ..gather_status(&fw.status_args(&[])).expect("status")
        };
        assert!(
            !render_status(&s).contains("--wine"),
            "nothing named a wine, so the undo command must not pin one"
        );
    }

    /// The state a bug reporter is actually in — "I installed it and the game
    /// does not track" — and the report's only actionable line used to be the
    /// one that undoes the install.
    ///
    /// Two things outside this prefix still have to be true, `install` prints
    /// both at the end of a successful run, and a user who installed last week
    /// never sees that paragraph again. `status` is the command we tell them
    /// to run instead, so it is the one that has to say it.
    #[test]
    fn the_report_says_what_still_has_to_be_true_for_a_game_to_receive_anything() {
        let fw = FakeWine::new("status-next-steps");
        install(&fw.args("install", &[])).expect("install");
        let gathered = gather_status(&fw.status_args(&[])).expect("status");

        let out = render_status(&Status {
            game_output_enabled: false,
            ..gathered
        });
        assert!(
            out.contains("tobii games set enabled true"),
            "game output is off, so the first thing to do is turn it on: {out}"
        );
        assert!(
            out.contains("tobii game -- %command%"),
            "and the game has to be launched through the wrapper: {out}"
        );
        let forward = out
            .find("tobii game -- %command%")
            .expect("the way forward");
        let out_again = out
            .find("tobii bridge uninstall")
            .expect("the way out is still offered");
        assert!(
            forward < out_again,
            "the way forward comes before the way out, not after it: {out}"
        );

        // With output already on, it says so and does not hand back a command
        // that would change nothing — a report that tells a user to switch on
        // what is already on sends them looking in the wrong place.
        let out = render_status(&Status {
            game_output_enabled: true,
            ..gather_status(&fw.status_args(&[])).expect("status")
        });
        assert!(out.contains("game output is ON"), "{out}");
        assert!(!out.contains("tobii games set enabled true"), "{out}");
        assert!(out.contains("tobii game -- %command%"), "{out}");
    }

    /// `reject_unknown_flags` gates every subcommand, and a new one wired into
    /// the dispatch without being wired into the gate is a command where
    /// `--help` performs the command. `status` reads no `--artifacts`, so the
    /// gate must refuse that too rather than have the usage line advertise a
    /// directory the command never looks in.
    ///
    /// `--npclient` is on the other side of that same line now: the report's
    /// closing sentence is decided against what `install` would write into the
    /// TrackIR key, and that is what `--npclient` picks. A `status` that
    /// refused the flag could only answer for one spelling of the command
    /// while printing another.
    ///
    /// Driven through [`bridge`] and not through [`reject_unknown_flags`]
    /// directly, because the thing that can be wrong is the *list the dispatch
    /// passes*: a test that hands the gate its own list agrees with itself
    /// whatever `bridge` does. Every argv here also names the throwaway prefix
    /// and the fake wine, so a gate that let one through reaches that prefix
    /// and never the user's own.
    #[test]
    fn status_is_gated_like_every_other_subcommand() {
        let fw = FakeWine::new("status-gate");
        for bad in [
            vec!["--help"],
            vec!["-h"],
            vec!["--prefix=/x"],
            vec!["--artifacts", "/x"],
            vec!["--force"],
        ] {
            let err = bridge(&fw.status_args(&bad))
                .expect_err(&format!("`{bad:?}` must not reach the command"));
            let err = err.to_string();
            let usage = err
                .split_once("Usage: tobii bridge status")
                .unwrap_or_else(|| panic!("the refusal must show status's own usage: {err}"))
                .1;
            assert!(
                !usage.contains("--artifacts") && !usage.contains("--force"),
                "the usage line may not advertise a flag status ignores: {usage}"
            );
            assert!(
                usage.contains("--npclient"),
                "and must advertise the ones it reads: {usage}"
            );
        }
        // And the flags it does read get all the way through to a report.
        bridge(&fw.status_args(&[])).expect("its own flags are accepted");
    }

    /// The subcommand list a bare `tobii bridge` prints is the only place a
    /// user learns this command exists.
    #[test]
    fn a_bare_bridge_names_status_among_the_subcommands() {
        let args: Vec<String> = ["tobii", "bridge"].iter().map(|s| s.to_string()).collect();
        let err = bridge(&args).expect_err("no subcommand is an error");
        assert!(err.to_string().contains("status"), "{err}");
    }

    /// The scratch homes one thread has made, removed when that thread ends.
    ///
    /// `steam_home` hands its home back out of itself, so a guard the caller
    /// holds would drop at the end of the helper and take the home with it.
    /// `libtest` gives each test a thread, and a thread-local's destructor
    /// runs when that thread ends. What it rests on is that the tests do not
    /// run on the main thread, whose locals are not destroyed; until this
    /// existed they were left behind on every run, one per case per run.
    struct SteamHomes(Vec<PathBuf>);

    impl Drop for SteamHomes {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }

    thread_local! {
        static STEAM_HOMES: std::cell::RefCell<SteamHomes> =
            const { std::cell::RefCell::new(SteamHomes(Vec::new())) };
    }

    /// A throwaway `$HOME` with one Steam library in it, holding a manifest
    /// per `(appid, name)`, removed when the thread that asked for it ends.
    ///
    /// No environment variable is read and nothing under the real home is
    /// touched: CI runs these as root, where `$HOME` is somebody else's, and
    /// `steam_prefix_for` takes the home to look in for exactly that reason.
    fn steam_home(tag: &str, apps: &[(&str, &str)]) -> PathBuf {
        let home =
            std::env::temp_dir().join(format!("tobii-steamfix-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        STEAM_HOMES.with_borrow_mut(|homes| homes.0.push(home.clone()));
        let steamapps = home.join(".steam/steam/steamapps");
        std::fs::create_dir_all(&steamapps).expect("steamapps");
        for (id, name) in apps {
            std::fs::write(
                steamapps.join(format!("appmanifest_{id}.acf")),
                format!(
                    "\"AppState\"\n{{\n\t\"appid\"\t\t\"{id}\"\n\t\"name\"\t\t\"{name}\"\n}}\n"
                ),
            )
            .expect("manifest");
        }
        home
    }

    /// What Proton leaves behind the first time a title is launched.
    fn launched(home: &Path, appid: &str) -> PathBuf {
        let pfx = home
            .join(".steam/steam/steamapps/compatdata")
            .join(appid)
            .join("pfx");
        std::fs::create_dir_all(pfx.join("drive_c")).expect("pfx");
        pfx
    }

    /// The four things `--steam` can answer are worded here and decided in
    /// `tobii_steam`, and the wording is what a user sees. These pin it
    /// byte-for-byte, because the move onto that crate was allowed to change
    /// where the answer comes from and nothing about what it says.
    #[test]
    fn an_ambiguous_name_lists_what_it_matched_and_asks_for_the_app_id() {
        let home = steam_home(
            "many",
            &[("11", "Fixture Game One"), ("22", "Fixture Game Two")],
        );
        let err = steam_prefix_for(&home, "fixture").expect_err("two matches is not an answer");
        assert_eq!(
            err,
            "\"fixture\" matches more than one game; pass the app id:\n  \
             11         Fixture Game One\n  \
             22         Fixture Game Two"
        );
    }

    #[test]
    fn a_name_that_matches_nothing_lists_everything_that_is_installed() {
        let home = steam_home(
            "none",
            &[("11", "Fixture Game One"), ("22", "Fixture Game Two")],
        );
        let err = steam_prefix_for(&home, "nothing").expect_err("no match is not an answer");
        assert_eq!(
            err,
            "no installed Steam game matches \"nothing\"\ninstalled:\n  \
             11         Fixture Game One\n  \
             22         Fixture Game Two"
        );
    }

    /// A title on a drive nobody has plugged in is installed and in no list
    /// here, so "no installed Steam game matches" is a confident negative this
    /// command has no grounds for — the same answer `tobii-gameconf` spent two
    /// rounds removing. `libraryfolders.vdf` outlives the drive, and the
    /// maintainer's own file names a library under `/run/media` that is gone.
    #[test]
    fn a_name_matching_nothing_says_so_when_a_library_could_not_be_looked_in() {
        let home = steam_home("unplugged", &[("11", "Fixture Game One")]);
        // Under this test's own scratch home on purpose: a path reached by
        // walking up out of it is how a fixture escapes into real files.
        let gone = home.join("run/media/nobody/ExternalSSD/steam");
        std::fs::write(
            home.join(".steam/steam/steamapps/libraryfolders.vdf"),
            format!("\t\"path\"\t\t\"{}\"\n", gone.display()),
        )
        .expect("vdf");
        let err = steam_prefix_for(&home, "nothing").expect_err("no match is not an answer");
        assert_eq!(
            err,
            format!(
                "nothing in the Steam libraries this machine has matches \"nothing\"\n\
                 installed:\n  11         Fixture Game One\n\
                 libraryfolders.vdf names a Steam library this machine does not \
                 have, so anything installed there is in no list here:\n  {}",
                gone.display()
            )
        );
    }

    /// `games` prints the absent library with the library list and then
    /// refuses, so the refusal must not carry it a second time: the same path
    /// under two sentences reads as two drives that are gone, and the count is
    /// the whole point of the block.
    ///
    /// The refusal above it in the function returns *before* that block is
    /// printed and so does carry it — which is why these two cannot share a
    /// sentence, and why this is worth pinning.
    #[test]
    fn games_names_an_absent_library_once_when_it_has_already_printed_it() {
        let home = steam_home("gamesdup", &[]);
        // Under this test's own scratch home: a path reached by walking up out
        // of it is how a fixture escapes into real files.
        let gone = home.join("run/media/nobody/ExternalSSD/steam");
        std::fs::write(
            home.join(".steam/steam/steamapps/libraryfolders.vdf"),
            format!("\t\"path\"\t\t\"{}\"\n", gone.display()),
        )
        .expect("vdf");
        let err = list_steam_games_in(&home).expect_err("nothing is installed");
        assert_eq!(err.to_string(), "no installed Steam games found");
    }

    /// An empty list is its own sentence: "installed:" followed by nothing
    /// would read as a list that failed to print.
    #[test]
    fn a_name_matching_nothing_with_no_steam_at_all_says_there_are_no_libraries() {
        let home = steam_home("nosteam", &[]);
        std::fs::remove_dir_all(home.join(".steam")).expect("no library at all");
        let err = steam_prefix_for(&home, "elite").expect_err("nothing is installed");
        assert_eq!(
            err,
            "no installed Steam game matches \"elite\" (no Steam libraries found)"
        );
    }

    /// An app id is used verbatim, and this is the case that proves it is not
    /// checked against the installed list first: `compatdata` outlives the
    /// manifest, so a non-Steam shortcut and an uninstalled title both keep a
    /// prefix that `--steam <id>` still reaches. Asking `tobii_steam::resolve`
    /// about the id instead would answer `None` here and turn a prefix that
    /// resolves today into "no installed Steam game matches".
    #[test]
    fn an_app_id_with_a_prefix_but_no_manifest_still_resolves() {
        let home = steam_home("orphan", &[("44", "Listed Fixture")]);
        let pfx = launched(&home, "3512345678");
        assert_eq!(
            steam_prefix_for(&home, "3512345678").expect("the prefix is there"),
            pfx.canonicalize().expect("real")
        );
    }

    /// The other half of the same rule: an app id nothing is installed under
    /// and nothing has a prefix for is answered by the prefix paragraph — "that
    /// app", because no manifest names it — and not by the installed list.
    #[test]
    fn an_app_id_with_neither_manifest_nor_prefix_gets_the_prefix_paragraph() {
        let home = steam_home("strangeid", &[("11", "Fixture Game One")]);
        let err = steam_prefix_for(&home, "99").expect_err("no prefix under 99");
        assert_eq!(
            err,
            "no Proton prefix for 99 (that app). Proton creates it the first \
             time the game runs — start the game once, then run this again.\n\
             If it is set to run natively rather than through Proton, there is \
             no prefix and no Windows DLL to install into."
        );
    }

    /// An installed title that has never been launched is the commonest way to
    /// reach that paragraph, and there the manifest does name it.
    #[test]
    fn a_game_that_has_never_been_launched_is_named_in_the_prefix_paragraph() {
        let home = steam_home("unlaunched", &[("11", "Fixture Game One")]);
        let err = steam_prefix_for(&home, "game one").expect_err("never launched, no prefix");
        assert!(
            err.starts_with("no Proton prefix for 11 (Fixture Game One). Proton creates it"),
            "{err}"
        );
    }

    /// `--steam ""` is all-digits vacuously, so it is an app id — the empty
    /// one, which nothing is installed under and nothing has a prefix for. The
    /// answer is nonsense and it is the answer this command has always given;
    /// see [`used_verbatim_as_app_id`] for why the move onto `tobii_steam`, whose `resolve`
    /// calls the empty string a name matching everything, did not take that
    /// better answer along with it.
    #[test]
    fn an_empty_steam_value_answers_exactly_as_it_did_before() {
        let home = steam_home(
            "empty",
            &[("11", "Fixture Game One"), ("22", "Fixture Game Two")],
        );
        let err = steam_prefix_for(&home, "").expect_err("the empty app id");
        assert_eq!(
            err,
            "no Proton prefix for  (that app). Proton creates it the first \
             time the game runs — start the game once, then run this again.\n\
             If it is set to run natively rather than through Proton, there is \
             no prefix and no Windows DLL to install into."
        );
    }

    /// A name that matches exactly one is that one, whatever its capitalisation
    /// — and the prefix it resolves to is the library's, not the first library
    /// that happens to have a `compatdata`.
    #[test]
    fn one_name_match_resolves_to_that_titles_prefix() {
        let home = steam_home(
            "one",
            &[("11", "Fixture Game One"), ("22", "Fixture Game Two")],
        );
        let pfx = launched(&home, "22");
        assert_eq!(
            steam_prefix_for(&home, "GAME TWO").expect("launched once"),
            pfx.canonicalize().expect("real")
        );
    }

    /// A Steam prefix is one the user never typed the path of — they named a
    /// title — so the report has to say which app id it resolved and hand the
    /// undo command back in the same spelling. Handing them
    /// `--prefix /…/compatdata/2537590/pfx` instead is a path they would have
    /// to check before pasting, over a prefix they never chose by name.
    #[test]
    fn a_steam_prefix_is_reported_and_undone_by_the_name_the_user_gave() {
        let mut s = steam_status("2537590");
        s.dir_present = Presence::Yes;
        let out = render_status(&s);
        assert!(out.contains("Steam, from `--steam '2537590'`"), "{out}");
        assert!(
            out.contains("tobii bridge uninstall --steam '2537590'"),
            "{out}"
        );
        assert!(
            !out.contains("uninstall --prefix"),
            "a prefix the user never named must not come back as one they must type: {out}"
        );
        // The wine this prefix records IS its own, and only that case may say
        // so — the other half of the claim `refusal` was taught not to make.
        assert!(
            out.contains("the build this prefix itself records"),
            "{out}"
        );
    }

    /// The undo line's entire job is to survive a round trip through the
    /// clipboard, and a Steam title is a name with spaces in it.
    ///
    /// Unquoted, `--steam "Microsoft Flight Simulator 2024"` came out as
    /// `tobii bridge uninstall --steam Microsoft Flight Simulator 2024 …`,
    /// which pasted back answers `"Microsoft" matches more than one game` —
    /// the trailing words silently dropped, because the flag gate ignores
    /// positionals by design. The app-id case could never catch it: `2537590`
    /// is one token.
    ///
    /// Checked by splitting the printed line the way a shell would rather than
    /// by looking for quotes, because what is being asserted is that the name
    /// arrives at `uninstall` as one argument.
    #[test]
    fn the_undo_line_survives_a_steam_title_with_spaces_in_it() {
        let title = "Microsoft Flight Simulator 2024";
        let mut s = steam_status(title);
        s.dir_present = Presence::Yes;
        let out = render_status(&s);
        let line = out
            .lines()
            .find(|l| l.contains("tobii bridge uninstall"))
            .unwrap_or_else(|| panic!("no undo line in {out}"))
            .trim();
        assert_eq!(
            shell_words(line),
            vec!["tobii", "bridge", "uninstall", "--steam", title],
            "the undo line does not paste back as the command it names: {line}"
        );
        // And the prose above it names the same title the same way.
        assert!(out.contains(&format!("`--steam '{title}'`")), "{out}");
    }

    /// A prefix with nothing of ours in it is handed `install` first.
    ///
    /// That state is the one that most often brings somebody to this command —
    /// a game gets nothing because the bridge was never put in its prefix — and
    /// a report whose first command undoes an installation that does not exist
    /// names the wrong half of the pair.
    ///
    /// The undo line follows it rather than being absent: this branch always
    /// promised one, in prose ("the same line with `uninstall`") until that
    /// prose started describing a paste the gate refuses. What matters is the
    /// order, which is what the wrong half of the pair means here.
    #[test]
    fn an_empty_prefix_is_handed_the_command_that_fills_it() {
        let out = render_status(&steam_status("2537590"));
        let fills = out
            .find("tobii bridge install --steam '2537590'")
            .unwrap_or_else(|| panic!("no install line in {out}"));
        assert!(
            out.find("tobii bridge uninstall --steam")
                .is_none_or(|undo| fills < undo),
            "the undo command comes before the one that fills the prefix: {out}"
        );
    }

    /// One report per state whose closing block hands a command back, each in
    /// both wine states — the Proton build read out of the prefix, and not.
    ///
    /// A list rather than a test per state, because the rule the two tests
    /// below check is about every command this report can print, and the way
    /// this defect shipped twice was one state at a time: the per-key refusal
    /// was fixed for the branch it was found in, and the branch next to it
    /// grew a second one.
    fn reports_that_hand_a_command_back() -> Vec<(String, Status)> {
        let mut out = Vec::new();
        for (wine, origin) in [
            ("the prefix's own Proton", WineOrigin::Prefix),
            (
                "a wine the prefix does not corroborate",
                WineOrigin::Unverified,
            ),
        ] {
            for np in [None, Some("ours")] {
                let base = || {
                    let mut s = steam_status("Star Citizen");
                    s.origin = origin;
                    s.npclient_given = np.map(str::to_string);
                    s
                };
                let np_said = match np {
                    Some(v) => format!(", --npclient {v}"),
                    None => String::new(),
                };
                let say = |what: &str| format!("{what} ({wine}{np_said})");

                out.push((say("nothing of ours in the prefix"), base()));

                let mut ours = base();
                ours.dir_present = Presence::Yes;
                out.push((say("ours installed"), ours));

                let mut theirs = base();
                theirs.keys[0].current = Reading::Plain(r"C:\opentrack".to_string());
                out.push((say("a stranger's path in the way"), theirs));

                let mut unreadable = base();
                unreadable.unreadable = Some("Permission denied".to_string());
                out.push((say("neither key could be read"), unreadable));

                // The one shape that is not a `--steam` prefix: `--prefix`
                // with a wine the user named, which is the spelling the undo
                // line has to carry back — and the state in which no Proton
                // refusal exists to model.
                let mut given = base();
                given.source = PrefixSource::Given;
                given.wine_given = true;
                out.push((say("a --prefix run that named its own wine"), given));
            }
        }
        out
    }

    /// No line this report prints for pasting is one the command it names
    /// refuses.
    ///
    /// The rule stated once, over every command every branch prints, rather
    /// than asserted case by case — because case by case is how it was got
    /// wrong twice in one commit. The report used to hand a prefix holding a
    /// stranger's key an `install` its own key loop refuses; that was fixed by
    /// sharing [`stops_install`], and the same commit added a second refusal
    /// ([`steam_wine_refused`]) that fires earlier and that nothing in the
    /// report consulted, so the bare `install` line came back for a Proton
    /// prefix whose build could not be read.
    ///
    /// Both gates the command itself applies, in the order it applies them:
    /// [`reject_unknown_flags`], which is what `tobii bridge` runs before it
    /// dispatches, and then the whole-command refusal. What is deliberately
    /// not checked is whether the command would then succeed — an `install`
    /// that reads the keys and refuses over one is doing its job, and the
    /// report says so.
    #[test]
    fn every_command_the_report_hands_back_is_one_that_command_accepts() {
        for (what, s) in reports_that_hand_a_command_back() {
            let out = render_status(&s);
            let mut checked = 0;
            for line in out.lines().map(str::trim) {
                if !line.starts_with("tobii bridge ") {
                    continue;
                }
                let argv = shell_words(line);
                let sub = argv[2].clone();
                let (_, known, _) = SUBS
                    .iter()
                    .find(|(n, _, _)| *n == sub)
                    .unwrap_or_else(|| panic!("{what}: `{line}` names no subcommand that exists"));
                if let Err(why) = reject_unknown_flags(&argv, &sub, known) {
                    panic!("{what}: the report prints `{line}`, which that command rejects: {why}");
                }
                if sub == "install" {
                    assert!(
                        !steam_wine_refused(
                            &s.source,
                            s.origin,
                            crate::flag_value(&argv, "--wine").is_some(),
                            argv.iter().any(|a| a == "--force"),
                        ),
                        "{what}: the report prints `{line}`, which `install` refuses \
                         outright before it reads a key"
                    );
                }
                checked += 1;
            }
            assert!(
                checked > 0,
                "{what}: the report hands back no command at all:\n{out}"
            );
        }
    }

    /// The undo command is spelled out, not described.
    ///
    /// It used to read "the same line with `uninstall`" — and the line above
    /// it carries `--npclient` when this run did, which `uninstall` does not
    /// read, so the sentence described a paste that exits 1 on the flag gate.
    /// [`Status::npclient_given`]'s own doc already says a command carrying a
    /// flag the gate refuses is worse than no command; prose that reassembles
    /// one is the same thing said out of reach of the test above.
    #[test]
    fn the_undo_command_is_spelled_out_rather_than_described() {
        for (what, s) in reports_that_hand_a_command_back() {
            let out = render_status(&s);
            assert!(
                !out.contains("the same line with"),
                "{what}: the report describes a command instead of printing it:\n{out}"
            );
            if !out.contains("To take it out again") {
                continue;
            }
            let line = out
                .lines()
                .map(str::trim)
                .find(|l| l.starts_with("tobii bridge uninstall"))
                .unwrap_or_else(|| panic!("{what}: promised an undo it never spelled out:\n{out}"));
            assert!(
                !shell_words(line).iter().any(|a| a == "--npclient"),
                "{what}: `{line}` carries a flag `uninstall` does not read"
            );
        }
    }

    /// A Steam prefix whose Proton build could not be read is told what
    /// `install` wants before it is handed an `install`.
    ///
    /// The half of the state the report used to leave out. `install`'s refusal
    /// answers "which build?" with "run `tobii bridge status`" — so a status
    /// report that hands back the command that refuses closes the loop the
    /// refusal was trying to open.
    #[test]
    fn a_proton_prefix_whose_build_is_unknown_is_told_what_install_wants() {
        let mut s = steam_status("Star Citizen");
        s.origin = WineOrigin::Unverified;
        let out = render_status(&s);
        assert!(
            out.contains(
                "tobii bridge install --steam 'Star Citizen' --wine <Proton>/files/bin/wine"
            ),
            "{out}"
        );
        assert!(out.contains("`steamapps/common`"), "{out}");
        // And the reason, so that `<Proton>` is not a shape to be guessed at.
        assert!(
            out.contains("upgrades the prefix out from under the game"),
            "{out}"
        );
        // A prefix that does corroborate its wine gets no such paragraph, and
        // the plain line back.
        let plain = render_status(&steam_status("Star Citizen"));
        assert!(
            plain.contains("tobii bridge install --steam 'Star Citizen'\n"),
            "{plain}"
        );
        assert!(!plain.contains("<Proton>"), "{plain}");
    }

    /// Split a command line the way a POSIX shell would, for asserting that a
    /// line we print pastes back as the arguments it means. Single quotes and
    /// spaces only: that is all [`quoted`] produces.
    fn shell_words(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut started = false;
        let mut quoting = false;
        for c in line.chars() {
            match c {
                '\'' => {
                    quoting = !quoting;
                    started = true;
                }
                c if c.is_whitespace() && !quoting => {
                    if started {
                        out.push(std::mem::take(&mut cur));
                        started = false;
                    }
                }
                c => {
                    cur.push(c);
                    started = true;
                }
            }
        }
        if started {
            out.push(cur);
        }
        out
    }

    /// A report about a Steam prefix named `wanted`, with nothing installed in
    /// it — the starting point every rendering test varies one field of.
    fn steam_status(wanted: &str) -> Status {
        let prefix = PathBuf::from("/games/steamapps/compatdata/2537590/pfx");
        Status {
            dir: prefix.join(INSTALL_SUBDIR),
            registry_file: prefix.join(crate::userreg::FILE),
            prefix,
            source: PrefixSource::Steam(wanted.to_string()),
            wine: PathBuf::from("/games/Proton/files/bin/wine"),
            origin: WineOrigin::Prefix,
            wine_given: false,
            wine_warning: None,
            server: crate::wineserver::Lock::Free,
            dir_present: Presence::No,
            artifacts: ARTIFACTS
                .iter()
                .map(|(name, required)| (*name, *required, Presence::No))
                .collect(),
            record_unreadable: None,
            keys: vec![KeyReport {
                abi: "FreeTrack",
                key: FT_KEY,
                current: Reading::Absent,
                wrote: None,
                want: INSTALL_WIN_DIR.to_string(),
            }],
            npclient_given: None,
            game_output_enabled: true,
            unreadable: None,
        }
    }

    /// The lock says a wine process is alive on this prefix, and that is all it
    /// says. Naming the holder as the game, or as ours, would be a guess — and
    /// the useful half is the consequence, which is the same whoever it is.
    #[test]
    fn the_wineserver_line_says_what_the_lock_proves_and_no_more() {
        assert_eq!(
            server_line(&crate::wineserver::Lock::Free),
            "nothing is serving this prefix right now"
        );
        let held = server_line(&crate::wineserver::Lock::Held(4242));
        assert!(held.contains("pid 4242"), "{held}");
        assert!(held.contains("the game first"), "{held}");
        assert!(
            !held.contains("the game is running"),
            "the lock does not say who holds it: {held}"
        );
        // A lock taken through an open file description reports no pid at all,
        // and inventing one — pid 0, or "the game" — would be a fact nobody
        // measured.
        let anon = server_line(&crate::wineserver::Lock::Held(0));
        assert!(anon.contains("would not name the holder"), "{anon}");
        assert!(!anon.contains("pid 0"), "{anon}");
        let unknown = server_line(&crate::wineserver::Lock::Unknown("EACCES".into()));
        assert!(unknown.contains("could not tell"), "{unknown}");
        assert!(unknown.contains("EACCES"), "{unknown}");
    }
}
