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
//!   print back in a form we can read — is refused, with nothing written and
//!   nothing created, and named without accusing anyone the record cannot
//!   name. `--force` goes ahead and promises *nothing* about putting the old
//!   value back.
//! * uninstall reads the key first too, and removes the `Path` value it added,
//!   only while the key still says what we wrote. Anything else is left exactly
//!   as it is, and named.
//! * the record holds only values this program computed itself, written down
//!   after the key took one and naming only the keys that took it. A value read
//!   out of a key is compared and then dropped: never stored, never written.
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
const INSTALL_WIN_DIR: &str = r"C:\tobii-bridge";

/// Registry key a TrackIR game reads to find its client DLL.
const NP_KEY: &str = r"HKCU\Software\NaturalPoint\NATURALPOINT\NPClient Location";

/// Registry key a FreeTrack game reads to find its client DLL.
const FT_KEY: &str = r"HKCU\Software\Freetrack\FreeTrackClient";

/// Both discovery keys: the name each is recorded under, the key, its ABI.
///
/// Paired in one place because everything that touches one touches both —
/// install reads and writes both, uninstall reads both and takes out only what
/// it put there — and because the short name is what ends up in
/// [`RECORD_FILE`], where a rename would silently orphan an existing prefix's
/// record.
const KEYS: [(&str, &str, &str); 2] = [("ft", FT_KEY, "FreeTrack"), ("np", NP_KEY, "TrackIR")];

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
    /// `NP_GetSignature` anti-clone check, which a clean-room DLL cannot answer:
    /// measured on 2026-08-15, Star Citizen calls it, gets nothing it
    /// recognises, and never asks for data again. An already-installed client
    /// can answer it, and reads the same `FT_SharedMem` our provider writes —
    /// so it is used through its published interface, with our data behind it.
    Installed(PathBuf),
}

/// Decide which NPClient DLL to register.
///
/// Prefers an installed third-party client when one is present, because that is
/// the only configuration in which a TrackIR game actually receives anything.
/// `--npclient ours` forces ours, which is the right choice for a game that does
/// not check the signature.
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
/// The one artifact without which there is no installation — also what
/// [`artifact_dir`] recognises a build directory by.
const REQUIRED_ARTIFACT: &str = "freetrackclient64.dll";

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
        let expected = launch_script.clone().or_else(|| runners.first().cloned());
        let origin = if expected.as_deref() == Some(w.as_path()) {
            WineOrigin::Prefix
        } else {
            WineOrigin::Unverified
        };
        let warning = match expected {
            Some(e) if e != w => Some(format!(
                "using {} but this prefix's own runner is {} — if the game sees no \
                 tracking, that mismatch is why (two wineservers, two namespaces)",
                w.display(),
                e.display()
            )),
            _ => None,
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
        Some(w) => Ok((
            w,
            WineOrigin::Unverified,
            Some(
                "no runner found inside the prefix, falling back to the system wine — \
                 if the game sees no tracking, pass --wine with the runner the game uses"
                    .to_string(),
            ),
        )),
        None => Err("no wine binary found; pass --wine PATH".into()),
    }
}

/// The wine binary a LUG-style `sc-launch.sh` selects, if there is one.
///
/// That script records the runner in `export wine_path="..."`, which is the most
/// authoritative answer available: it is literally what launches the game.
fn wine_from_launch_script(prefix: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(prefix.join("sc-launch.sh")).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let rest = line.strip_prefix("export wine_path=")?;
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

/// Steam's install roots, in the order they are worth trying.
///
/// `~/.steam/steam` is the symlink Steam maintains to wherever it actually
/// lives, so it wins; the others are the layouts it has used and the one
/// Flatpak uses.
const STEAM_ROOTS: [&str; 4] = [
    ".steam/steam",
    ".local/share/Steam",
    ".steam/root",
    ".var/app/com.valvesoftware.Steam/data/Steam",
];

/// Add `path` if it is a library and not already listed.
///
/// Canonicalised first: `~/.steam/steam` and `~/.steam/root` are both symlinks
/// to the real install, so a plain path comparison reports the same library
/// three times and would then install into it three times.
fn push_library(out: &mut Vec<PathBuf>, path: PathBuf) {
    if !path.join("steamapps").is_dir() {
        return;
    }
    let real = path.canonicalize().unwrap_or(path);
    if !out.contains(&real) {
        out.push(real);
    }
}

/// The value of a `"key"    "value"` line in Valve's key-value format.
///
/// `libraryfolders.vdf` and an `appmanifest_*.acf` are both that format, and
/// both are read by pulling out the two or three keys that matter rather than
/// by understanding VDF, because a real parser would be a dependency and a
/// maintenance burden for three strings.
///
/// The key is matched a piece at a time rather than against a quoted copy of
/// it, so running this over every line of every manifest allocates nothing.
fn vdf_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line
        .trim()
        .strip_prefix('"')?
        .strip_prefix(key)?
        .strip_prefix('"')?;
    rest.trim().trim_start_matches('"').split('"').next()
}

/// Every Steam library on this machine.
///
/// The libraries live in `libraryfolders.vdf`, out of which [`vdf_value`] pulls
/// the `"path"` lines.
pub fn steam_libraries(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in STEAM_ROOTS {
        let root = home.join(root);
        // Not `else { continue }`: that skipped the root fallback below, so a
        // Steam install whose libraryfolders.vdf is missing, unreadable or not
        // UTF-8 produced NO libraries at all — and every `--steam` path sources
        // its libraries here, so the whole surface then reported "no Steam
        // libraries found" on a machine with games plainly installed.
        let vdf = root.join("steamapps/libraryfolders.vdf");
        let text = std::fs::read_to_string(&vdf).unwrap_or_default();
        for line in text.lines() {
            if let Some(p) = vdf_value(line, "path") {
                push_library(&mut out, PathBuf::from(p.replace("\\\\", "/")));
            }
        }
        // The root is a library itself even when the file does not say so.
        push_library(&mut out, root);
    }
    out
}

/// The installed games, as `(appid, name)`.
pub fn steam_apps(home: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for lib in steam_libraries(home) {
        let Ok(dir) = std::fs::read_dir(lib.join("steamapps")) else {
            continue;
        };
        for entry in dir.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let field = |key: &str| text.lines().find_map(|l| vdf_value(l, key));
            if let (Some(id), Some(name)) = (field("appid"), field("name")) {
                out.push((id.to_string(), name.to_string()));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The Proton prefix for `appid`, if the game has ever been run.
///
/// Proton creates `steamapps/compatdata/<appid>/pfx` the first time a title
/// launches. Its absence is the commonest reason this fails and is worth saying
/// out loud rather than reporting as "not found".
pub fn steam_prefix(home: &Path, appid: &str) -> Option<PathBuf> {
    let libs = steam_libraries(home);
    // The library holding the manifest is asked first, not just whichever
    // library happens to come first. Steam's "Move Install Folder" does not
    // move `compatdata`, so a game moved between libraries leaves its old
    // prefix behind and gets a fresh one on the next run — and installing into
    // the abandoned one succeeds, prints the ordinary success text, and does
    // nothing at all for the game.
    let owner = libs
        .iter()
        .find(|lib| {
            lib.join(format!("steamapps/appmanifest_{appid}.acf"))
                .is_file()
        })
        .cloned();
    owner
        .into_iter()
        .chain(libs)
        .map(|lib| lib.join("steamapps/compatdata").join(appid).join("pfx"))
        .find(|p| p.join("drive_c").is_dir())
}

/// Turn `--steam <appid|name fragment>` into a prefix path.
///
/// A name is matched case-insensitively as a substring, because nobody types
/// "Elite Dangerous" with the right capitalisation twice. An ambiguous fragment
/// lists what it matched rather than picking one.
fn steam_prefix_for(home: &Path, wanted: &str) -> Result<PathBuf, String> {
    let apps = steam_apps(home);
    let appid = if wanted.chars().all(|c| c.is_ascii_digit()) {
        wanted.to_string()
    } else {
        let needle = wanted.to_lowercase();
        let hits: Vec<&(String, String)> = apps
            .iter()
            .filter(|(_, n)| n.to_lowercase().contains(&needle))
            .collect();
        match hits.as_slice() {
            [] => {
                let mut msg = format!("no installed Steam game matches {wanted:?}");
                if apps.is_empty() {
                    msg.push_str(" (no Steam libraries found)");
                } else {
                    msg.push_str("\ninstalled:");
                    for (id, n) in &apps {
                        msg.push_str(&format!("\n  {id:<10} {n}"));
                    }
                }
                return Err(msg);
            }
            [one] => one.0.clone(),
            many => {
                let list: Vec<String> = many
                    .iter()
                    .map(|(id, n)| format!("  {id:<10} {n}"))
                    .collect();
                return Err(format!(
                    "{wanted:?} matches more than one game; pass the app id:\n{}",
                    list.join("\n")
                ));
            }
        }
    };
    steam_prefix(home, &appid).ok_or_else(|| {
        let name = apps
            .iter()
            .find(|(id, _)| *id == appid)
            .map(|(_, n)| n.as_str())
            .unwrap_or("that app");
        format!(
            "no Proton prefix for {appid} ({name}). Proton creates it the first \
             time the game runs — start the game once, then run this again.\n\
             If it is set to run natively rather than through Proton, there is \
             no prefix and no Windows DLL to install into."
        )
    })
}

/// Resolve the prefix: `--steam`, else `--prefix`, else `$WINEPREFIX`, else
/// `~/.wine`.
fn resolve_prefix(args: &[String]) -> Result<PathBuf, String> {
    if let Some(wanted) = crate::flag_value(args, "--steam") {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is not set, so Steam's libraries cannot be found")?;
        let p = steam_prefix_for(&home, wanted)?;
        eprintln!("Steam prefix: {}", p.display());
        return Ok(p);
    }
    let raw = crate::flag_value(args, "--prefix")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("WINEPREFIX").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".wine")))
        .ok_or("could not determine a Wine prefix; pass --prefix PATH or --steam <game>")?;
    if !raw.join("drive_c").is_dir() {
        return Err(format!(
            "{} does not look like a Wine prefix (no drive_c)",
            raw.display()
        ));
    }
    Ok(raw)
}

/// Resolve the wine binary for `prefix`, reporting any warning to stderr.
///
/// The [`WineOrigin`] comes back with it because a caller that quotes the
/// binary at the user has to know whether it is the prefix's own — see the
/// enum for what goes wrong when that is assumed.
fn resolve_wine(prefix: &Path, args: &[String]) -> Result<(PathBuf, WineOrigin), String> {
    // Proton's own build first among the non-explicit sources: it is the only
    // one that is *recorded* as belonging to this prefix rather than inferred,
    // and using any other would upgrade the prefix.
    let (wine, origin, warning) = choose_wine(
        crate::flag_value(args, "--wine").map(PathBuf::from),
        wine_from_steam_config_info(prefix).or_else(|| wine_from_launch_script(prefix)),
        &wines_from_runners(prefix),
        on_path("wine"),
    )?;
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

/// Run a wine command against `prefix`, returning its exit status.
fn wine_run(
    wine: &Path,
    prefix: &Path,
    args: &[&str],
) -> std::io::Result<std::process::ExitStatus> {
    std::process::Command::new(wine)
        .args(args)
        .env("WINEPREFIX", prefix)
        // Wine is chatty and none of it is ours; the bridge's own output is
        // what the user needs to see.
        .env("WINEDEBUG", "-all")
        .status()
}

/// Run a wine command against `prefix` and capture what it printed.
///
/// Separate from [`wine_run`] because reading the registry needs the output,
/// and because `reg query`'s own chatter would otherwise land in the middle of
/// the installer's lines.
fn wine_output(wine: &Path, prefix: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    std::process::Command::new(wine)
        .args(args)
        .env("WINEPREFIX", prefix)
        .env("WINEDEBUG", "-all")
        .output()
}

/// A key every wine prefix has, used to tell "nothing is registered" apart
/// from "this wine cannot read this prefix at all".
const PROBE_KEY: &str = r"HKCU\Software";

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
    // Wine terminates every line it prints with CRLF, so a chunk that arrived
    // without its CR did not end where the split did: the break came from
    // inside the value itself. What we would hold is a fragment, and a fragment
    // must never be compared — it can equal our own computed path and then be
    // removed as ours. The last chunk is exempt: it is what follows the final
    // newline, and is empty on well-formed output.
    let chunks: Vec<&[u8]> = stdout.split(|b| *b == b'\n').collect();
    let last = chunks.len().saturating_sub(1);
    for (i, line) in chunks.iter().enumerate() {
        let line = *line;
        let whole_line = i == last || line.ends_with(b"\r");
        // Everything up to the value — the name, the whitespace, the type — is
        // ASCII whatever codepage wine chose, so the line is parsed as ASCII up
        // to the first byte that is not one, and only the value itself raises
        // the question of whether we can read it.
        let ascii = &line[..line
            .iter()
            .position(|b| !b.is_ascii())
            .unwrap_or(line.len())];
        let Ok(head) = std::str::from_utf8(ascii) else {
            continue;
        };
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
         read. For a Steam title the wine must be the Proton build the prefix \
         records; pass --wine explicitly if this one is wrong for it.",
        prefix.display(),
        wine.display()
    ))
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
        match out.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = wrote.to_string(),
            None => out.push((name.to_string(), wrote.to_string())),
        }
    }
    out
}

/// The record kept in `dir`, empty when there is none.
fn read_record(dir: &Path) -> Vec<(String, String)> {
    parse_record(&std::fs::read_to_string(dir.join(RECORD_FILE)).unwrap_or_default())
}

/// The name the record is staged under before the rename that publishes it.
///
/// Per process, not one fixed name. The rename is atomic; the write into the
/// staging file is not, so two installs into one prefix sharing a staging name
/// interleave their writes and then rename a torn record into place — the very
/// thing the rename is here to prevent.
fn staging_name(pid: u32) -> String {
    format!(".{RECORD_FILE}.{pid}.new")
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
    let staged = dir.join(staging_name(std::process::id()));
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
/// So it is refused before anything is created, rather than written and
/// regretted.
fn registrable_path(dir: &Path) -> Result<String, String> {
    let win = wine_path_for(dir);
    if !win.is_ascii() {
        return Err(format!(
            "{} cannot be registered: wine prints the registry in the console's own \
             codepage rather than UTF-8, so a path with a character outside ASCII \
             does not read back the way it was written — and a value this installer \
             cannot read back is one it could never tell from another program's.\n\
             Move the client somewhere spelled in ASCII, or pass `--npclient ours` \
             to register our own DLL instead.",
            dir.display()
        ));
    }
    Ok(win)
}

/// A path, quoted for the shell the user is going to paste it into.
fn shell_quoted(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
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
    /// see [`Taken::whose`].
    want: String,
    /// Whether this prefix's record says this installer wrote this key.
    recorded: bool,
}

impl Taken {
    /// Why this key is not ours to write — the same two answers [`undo_for`]
    /// gives on the way out, because they rest on the same two facts.
    ///
    /// Naming another program is a claim, and the record is the only evidence
    /// for it: it says this installer wrote something else here, so somebody
    /// changed the key since. With no record there is no such evidence. Every
    /// prefix installed before the record existed arrives here with none, and
    /// telling those users another program took their key is an accusation
    /// about a value this program itself wrote.
    fn whose(&self) -> &'static str {
        if self.recorded {
            "it no longer holds what this install wrote, so it is another program's now"
        } else {
            "nothing here records what this install wrote, so there is no telling whose it is"
        }
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
            t.whose()
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

/// `tobii bridge install` — copy the artifacts in and register them.
fn install(args: &[String]) -> CmdResult {
    let prefix = resolve_prefix(args)?;
    let (wine, wine_origin) = resolve_wine(&prefix, args)?;
    let src = artifact_dir(args)?;
    let dest = prefix.join(INSTALL_SUBDIR);

    // Which client TrackIR is pointed at decides what its key should say, so it
    // is settled before the registry is looked at rather than in the middle of
    // writing it — and refused here, before anything exists, if it is a path we
    // could not read back.
    let explicit_np = crate::flag_value(args, "--npclient");
    let np_source = choose_npclient(explicit_np, find_installed_npclient())?;
    // FreeTrack always gets our own DLL: that ABI has no signature check, so
    // nothing stands between it and our data.
    let np_target = match &np_source {
        NpSource::Ours => INSTALL_WIN_DIR.to_string(),
        NpSource::Installed(dir) => registrable_path(dir)?,
    };
    let want_for = |key: &str| {
        if key == NP_KEY {
            np_target.as_str()
        } else {
            INSTALL_WIN_DIR
        }
    };

    // Read before write, and before anything is copied. A prefix that already
    // has a working head-tracking setup has to come out of a refusal exactly as
    // it went in — no directory created, no key touched.
    //
    // The record is read first too, because what this installer wrote here last
    // time is part of reading the key honestly: without it our own previous
    // registration of a third-party client looks exactly like a stranger's.
    let record = read_record(&dest);
    let mut taken: Vec<Taken> = Vec::new();
    for (name, key, abi) in KEYS {
        let wrote = record
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, w)| w.as_str());
        let want = want_for(key);
        let current = read_key(&wine, &prefix, key)?;
        if current == Reading::Absent || is_ours(&current, wrote) {
            continue;
        }
        // Nothing here to refuse over: the key already holds, byte for byte,
        // what this run would put in it, and nothing records this installer
        // ever putting anything else there. That is precisely the state v0.4.0
        // leaves behind on a machine with opentrack installed — it registered
        // the third-party path and kept no record of doing so — so refusing
        // here refuses the upgrade path itself, over a write that would change
        // nothing, in a sentence blaming another program for a value this
        // program wrote.
        //
        // A record naming a *different* value is a different fact and is still
        // refused, identical value or not: that is positive evidence the key
        // changed hands since we wrote it, which makes the match a reason to
        // leave it alone rather than to proceed — somebody else put it there.
        if wrote.is_none() && matches!(&current, Reading::Plain(v) if v.eq_ignore_ascii_case(want))
        {
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
        let target = dest.join(name);
        let staged = dest.join(format!(".{name}.new"));
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
        let wrote = want_for(key).to_string();
        match kept.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = wrote,
            None => kept.push((name.to_string(), wrote)),
        }
    }
    if ft_landed || np_landed {
        let entries: Vec<(&str, &str)> =
            kept.iter().map(|(n, w)| (n.as_str(), w.as_str())).collect();
        write_record(&dest, &entries)?;
    }

    // Held rather than printed as we go: "registered X" is only true once every
    // write has landed, and printing it before the check put confident lines
    // above the failure that contradicted it.
    let registered;

    let mut third_party_np = false;
    match &np_source {
        NpSource::Ours => {
            registered = format!("registered {INSTALL_WIN_DIR} for TrackIR and FreeTrack");
            if explicit_np != Some("ours") {
                println!(
                    "\nnote: no third-party NPClient64.dll was found. Games that verify\n      \
                     NaturalPoint's signature — Star Citizen among them — will load our\n      \
                     DLL, reject it, and never ask for data again. Installing opentrack\n      \
                     provides a client that passes. FreeTrack has no signature check,\n      \
                     so a game that speaks FreeTrack works with ours today."
                );
            }
        }
        NpSource::Installed(_) => {
            let win = np_target.as_str();
            registered = format!(
                "registered {INSTALL_WIN_DIR} for FreeTrack\n\
                 registered {win} for TrackIR\n\n\
                 TrackIR points at the client already installed there, because games\n\
                 verify NaturalPoint's signature and a clean-room DLL cannot answer it.\n\
                 Nothing was copied."
            );
            // Load-bearing, and easy to miss: a third-party client is a pure
            // consumer of FT_SharedMem. Our DLLs create and feed that mapping,
            // and a TrackIR-only game never loads ours — so this is the one
            // configuration that still needs the provider running.
            third_party_np = true;
        }
    }

    // Nothing below here is true if the keys did not land, so it is not
    // printed. A game finds its client DLL through the registry and nowhere
    // else — copied files alone install nothing.
    if !all_written {
        return Err(format!(
            "the registry keys could not be written, so the DLLs are copied but \
             nothing will load them.\n\
             The wine used was {}. For a Steam title that must be the Proton \
             build the prefix records; pass --wine explicitly if this one is \
             wrong for it.",
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
    let status = wine_run(
        wine,
        prefix,
        &[
            "reg", "add", key, "/v", "Path", "/t", "REG_SZ", "/d", dir, "/f",
        ],
    )
    .map_err(|e| format!("{e}"))?;
    if !status.success() {
        eprintln!("warning: could not write {key}");
        return Ok(false);
    }
    Ok(true)
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
    let status = wine_run(wine, prefix, &["reg", "delete", key, "/v", "Path", "/f"])
        .map_err(|e| format!("{e}"))?;
    if status.success() {
        return Ok(true);
    }
    if matches!(read_key(wine, prefix, key), Ok(Reading::Absent)) {
        return Ok(true);
    }
    eprintln!("warning: could not remove Path from {key}");
    Ok(false)
}

/// `tobii bridge run` — run the provider in the foreground.
fn run(args: &[String]) -> CmdResult {
    let prefix = resolve_prefix(args)?;
    let (wine, _) = resolve_wine(&prefix, args)?;
    let exe = prefix.join(INSTALL_SUBDIR).join("tobii-bridge.exe");
    if !exe.is_file() {
        return Err(format!(
            "the bridge is not installed in {} — run `tobii bridge install` first",
            prefix.display()
        )
        .into());
    }
    let mut wine_args = vec![r"C:\tobii-bridge\tobii-bridge.exe"];
    if let Some(port) = crate::flag_value(args, "--port") {
        wine_args.push("--port");
        wine_args.push(port);
    }
    eprintln!(
        "running the bridge in {} (Ctrl-C to stop)",
        prefix.display()
    );
    let status = wine_run(&wine, &prefix, &wine_args)?;
    if !status.success() {
        return Err(format!("the bridge exited with {status}").into());
    }
    Ok(())
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
/// "Nothing was recorded" is asked before "it is not what we wrote", because
/// they are different facts and the second is a false account of the first.
/// Every prefix installed before this record existed arrives here with no
/// record at all, and telling those users a key "no longer holds what this
/// install wrote" claims a comparison that never happened. Their own pointer at
/// `C:\tobii-bridge` is still recognised — [`is_ours`] knows that directory
/// without any record — so what reaches this arm is a third-party client path
/// an old install merely pointed at, which may have been that program's own
/// registration all along.
fn undo_for(current: &Reading, wrote: Option<&str>) -> Undo {
    if *current == Reading::Absent {
        return Undo::Nothing;
    }
    if is_ours(current, wrote) {
        return Undo::Remove;
    }
    if wrote.is_none() {
        return Undo::Leave(
            "nothing here records what this install wrote, so there is no telling whose it is",
        );
    }
    Undo::Leave("it no longer holds what this install wrote, so it is another program's now")
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
    let mut outcomes: Vec<KeyOutcome> = Vec::new();
    for (name, key, abi) in KEYS {
        let wrote = record
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, w)| w.as_str());
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
    let prefix = resolve_prefix(args)?;
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
             Run `tobii bridge uninstall` again once wine can serve this prefix \
             (for a\nSteam title that means the Proton build it records; pass \
             --wine explicitly if\nthis one is wrong for it).",
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

/// Dispatch `tobii bridge ...`.
/// `tobii bridge games` — what is installed and which titles have a prefix.
fn list_steam_games() -> CmdResult {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set, so Steam's libraries cannot be found")?;
    let libs = steam_libraries(&home);
    if libs.is_empty() {
        return Err("no Steam libraries found".into());
    }
    for lib in &libs {
        println!("library: {}", lib.display());
    }
    let apps = steam_apps(&home);
    if apps.is_empty() {
        return Err("no installed Steam games found".into());
    }
    println!();
    for (id, name) in &apps {
        // A title with no prefix has never been run under Proton, which is the
        // one thing that stops `--steam` working — so it is shown, not hidden.
        let mark = if steam_prefix(&home, id).is_some() {
            "proton"
        } else {
            "  --  "
        };
        println!("  {mark}  {id:<10} {name}");
    }
    println!(
        "\ninstall into one with:  tobii bridge install --steam <app id or name>\n\
         titles marked `--` have no Proton prefix yet — run them once first."
    );
    Ok(())
}

pub fn bridge(args: &[String]) -> CmdResult {
    match args.get(2).map(String::as_str) {
        Some("games") => list_steam_games(),
        Some("install") => install(args),
        Some("run") => run(args),
        Some("uninstall") => uninstall(args),
        other => Err(format!(
            "usage: tobii bridge games|install|run|uninstall \n  \
             [--steam <app id or name> | --prefix PATH] [--wine PATH] \
             [--artifacts DIR] [--port PORT]{}",
            match other {
                Some(o) => format!("\nunknown argument `{o}`"),
                None => String::new(),
            }
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
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
        assert!(warn.expect("a warning").contains("--wine"));
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
        assert_eq!(required, vec![REQUIRED_ARTIFACT]);
        // `artifact_dir` recognises a build directory by this same file, so a
        // directory holding a complete installation is always found.
        assert!(ARTIFACTS
            .iter()
            .any(|(n, req)| *n == REQUIRED_ARTIFACT && *req));
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
    const FAKE_WINE: &str = r#"#!/bin/sh
root=$(dirname "$WINEPREFIX")
{ for a in "$@"; do printf '[%s]' "$a"; done; printf '\n'; } >> "$root/argv"
if [ -f "$root/fail-wine" ]; then exit 1; fi
if [ -f "$root/fail-after-delete" ] && [ -f "$root/deleted" ]; then exit 1; fi
case "$3" in
  *NaturalPoint*) reply="$root/np.reply" ;;
  *)              reply="$root/ft.reply" ;;
esac
if [ "$1" = reg ] && [ "$2" = query ]; then
  if [ "$3" = 'HKCU\Software' ]; then exit 0; fi
  if [ -f "$reply" ]; then
    cat "$reply"
    if [ -f "$root/vanish" ]; then rm -f "$reply"; fi
    exit 0
  fi
  echo 'reg: the specified registry key was not found' >&2
  exit 1
fi
if [ "$1" = reg ] && [ "$2" = add ]; then
  if [ -f "$root/fail-add" ]; then exit 1; fi
  printf '\r\nHKEY_CURRENT_USER\\Whatever\r\n    Path    %s    %s\r\n\r\n' "$7" "$9" > "$reply"
  exit 0
fi
if [ "$1" = reg ] && [ "$2" = delete ]; then
  if [ -f "$root/fail-add" ]; then exit 1; fi
  if [ ! -f "$reply" ]; then exit 1; fi
  rm -f "$reply"
  : > "$root/deleted"
  exit 0
fi
exit 0
"#;

    /// The one fake wine, written once and shared by every test.
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
    /// it. The finished file is then renamed into place under a stable name, by
    /// which time no write descriptor to it exists anywhere — so a second test
    /// process running at the same time can execute it safely, and reuses it
    /// instead of leaving a file of its own behind.
    fn fake_wine() -> &'static Path {
        static WINE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        WINE.get_or_init(|| {
            let shared = std::env::temp_dir().join("tobii-fake-wine.sh");
            if std::fs::read(&shared).is_ok_and(|b| b == FAKE_WINE.as_bytes()) {
                return shared;
            }
            let staged =
                std::env::temp_dir().join(format!("tobii-fake-wine-{}.sh", std::process::id()));
            std::fs::write(&staged, FAKE_WINE).expect("fake wine");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
                .expect("chmod +x");
            match std::fs::rename(&staged, &shared) {
                Ok(()) => shared,
                Err(_) => staged,
            }
        })
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
            Self { root }
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

        /// Make one key hold `value`, in wine's own output shape.
        fn registered(&self, which: &str, value: &str) {
            self.registered_as(which, "REG_SZ", value);
        }

        /// The same, for a value stored as some other type.
        fn registered_as(&self, which: &str, ty: &str, value: &str) {
            self.registered_bytes(
                which,
                format!(
                    "\r\nHKEY_CURRENT_USER\\Software\\Whatever\r\n    \
                     Path    {ty}    {value}\r\n\r\n"
                )
                .as_bytes(),
            );
        }

        /// The same again, byte for byte — for output that is not UTF-8, which
        /// is what wine prints for a path with a non-ASCII character in it.
        fn registered_bytes(&self, which: &str, bytes: &[u8]) {
            std::fs::write(self.reply(which), bytes).expect("canned reply");
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

        fn args(&self, sub: &str, extra: &[&str]) -> Vec<String> {
            let mut v: Vec<String> = ["tobii", "bridge", sub]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
            for (flag, value) in [
                ("--prefix", self.prefix().display().to_string()),
                ("--wine", fake_wine().display().to_string()),
                (
                    "--artifacts",
                    self.root.join("artifacts").display().to_string(),
                ),
            ] {
                v.push(flag.to_string());
                v.push(value);
            }
            // Pinned unless the test picks its own, so the outcome does not
            // depend on whether the machine running the test happens to have
            // opentrack installed.
            if !extra.contains(&"--npclient") {
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
    #[test]
    fn a_value_broken_across_lines_is_not_read_as_its_first_half() {
        let out = b"\r\n    Path    REG_SZ    C:\\tobii-bridge\nEVIL\r\n\r\n";
        match reg_query_path(out) {
            Reading::Other(why) => assert!(
                why.contains("line break"),
                "a fragment must say why it is unreadable, got {why:?}"
            ),
            other => panic!("a fragment must never be a value or an absence: {other:?}"),
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

    /// The rename that publishes the record is atomic; the write into the
    /// staging file is not. Two installs into one prefix sharing a staging name
    /// interleave their writes and rename a torn record into place.
    #[test]
    fn two_installers_do_not_stage_the_record_over_each_other() {
        assert_ne!(staging_name(11), staging_name(12));
        assert_ne!(staging_name(11), RECORD_FILE);
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
        assert!(argv.contains("[query]"), "must have read first: {argv}");
        assert!(!argv.contains("[add]"), "nothing may be written: {argv}");
        assert!(!w.dest().exists(), "and nothing copied either");
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
        w.registered_bytes("np", &reply);
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
    #[test]
    fn uninstall_without_a_record_leaves_a_foreign_key_alone() {
        let w = FakeWine::new("norecord");
        std::fs::create_dir_all(w.dest()).expect("install dir");
        std::fs::write(w.dest().join(REQUIRED_ARTIFACT), b"dll").expect("dll");
        w.registered("np", r"Z:\usr\libexec\opentrack");
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(!argv.contains("[delete]"), "nothing may be deleted: {argv}");
        assert!(!argv.contains("[add]"), "and nothing written: {argv}");
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
}
