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
/// install reads and writes both, uninstall puts both back — and because the
/// short name is what ends up in [`PRIOR_FILE`], where a rename would silently
/// orphan an existing prefix's record.
const KEYS: [(&str, &str, &str); 2] = [("ft", FT_KEY, "FreeTrack"), ("np", NP_KEY, "TrackIR")];

/// Where, inside the prefix, we remember what those keys said before we first
/// wrote to them.
///
/// Inside the prefix on purpose, under the directory the artifacts already go
/// in. The registration belongs to *that* prefix and to nothing else: a Steam
/// title whose compatdata is deleted takes the record with it, whereas a record
/// kept in the user's own config would outlive the prefix it describes and
/// promise to restore a value into something that no longer exists.
const PRIOR_FILE: &str = "prior-registry.txt";

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
) -> Result<(PathBuf, Option<String>), String> {
    if let Some(w) = explicit {
        // An explicit choice is honoured, but still checked against the prefix's
        // own so a mismatch is visible rather than mysterious.
        let expected = launch_script.clone().or_else(|| runners.first().cloned());
        let warning = match expected {
            Some(e) if e != w => Some(format!(
                "using {} but this prefix's own runner is {} — if the game sees no \
                 tracking, that mismatch is why (two wineservers, two namespaces)",
                w.display(),
                e.display()
            )),
            _ => None,
        };
        return Ok((w, warning));
    }
    if let Some(w) = launch_script {
        return Ok((w, None));
    }
    if let Some(w) = runners.first() {
        return Ok((w.clone(), None));
    }
    match on_path {
        Some(w) => Ok((
            w,
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
fn resolve_wine(prefix: &Path, args: &[String]) -> Result<PathBuf, String> {
    // Proton's own build first among the non-explicit sources: it is the only
    // one that is *recorded* as belonging to this prefix rather than inferred,
    // and using any other would upgrade the prefix.
    let (wine, warning) = choose_wine(
        crate::flag_value(args, "--wine").map(PathBuf::from),
        wine_from_steam_config_info(prefix).or_else(|| wine_from_launch_script(prefix)),
        &wines_from_runners(prefix),
        on_path("wine"),
    )?;
    if let Some(w) = warning {
        eprintln!("warning: {w}");
    }
    Ok(wine)
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

/// Pull the `Path` value out of `reg query <key> /v Path` output.
///
/// The line is `    Path    REG_SZ    C:\tobii-bridge`, and the value is the
/// whole remainder of the line rather than the next whitespace-separated word:
/// `C:\Program Files\...` is an entirely ordinary thing to find registered
/// here, and taking one word of it would have us compare a truncated path
/// against ours and call another program's registration foreign or our own
/// missing.
fn reg_query_path(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let rest = line.trim_end_matches('\r').trim_start();
        let Some(rest) = rest.strip_prefix("Path") else {
            continue;
        };
        // `Path` and not `PathX`: the name must end where we stopped reading.
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((_, value)) = rest.split_once("REG_SZ") else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        return Some(value.to_string());
    }
    None
}

/// The `Path` value under `key`, or `None` when nothing is registered there.
///
/// `reg query` exits non-zero when the key or the value is missing, which is
/// the ordinary "nothing here" answer. A prefix this wine cannot serve at all
/// fails the same way and so reads as "nothing here" too — which does not stay
/// hidden, because the write that follows fails as well and [`set_key`] reports
/// that as the failed install it is.
fn read_key(wine: &Path, prefix: &Path, key: &str) -> Result<Option<String>, String> {
    let out = wine_output(wine, prefix, &["reg", "query", key, "/v", "Path"])
        .map_err(|e| format!("{e}"))?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(reg_query_path(&String::from_utf8_lossy(&out.stdout)))
}

/// What one key said before this installer first wrote to it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Prior {
    /// The key had no `Path` value at all, so uninstall deletes it. That is
    /// only correct *because* it was recorded: deleting on a guess is the
    /// whole fault this record exists to prevent.
    Unset,
    /// The key named this directory, and uninstall puts it back.
    Value(String),
}

/// What a key already says, judged against the value this install would write.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Existing {
    /// Nothing registered.
    Absent,
    /// Our own install directory inside this prefix. Nothing but this
    /// installer ever names it, so there is nothing of anyone else's here.
    Ours,
    /// Already exactly what this install would write, but naming a directory
    /// outside the prefix — so it may just as well be that program's own
    /// registration of the same client DLL, written before we ever ran.
    /// Writing it again changes nothing; putting it back on the way out is the
    /// reading that cannot break anything.
    Same(String),
    /// Another program's registration, which is not ours to overwrite.
    Foreign(String),
}

impl Existing {
    /// What uninstall should put back, if this install writes over it.
    fn prior(&self) -> Prior {
        match self {
            Existing::Absent | Existing::Ours => Prior::Unset,
            Existing::Same(v) | Existing::Foreign(v) => Prior::Value(v.clone()),
        }
    }
}

/// Judge an existing `Path` value against the one we mean to write.
///
/// Compared case-insensitively because Windows paths are, and because a prefix
/// that spells the drive letter the other way round is not a different
/// registration — refusing over that would be a refusal nobody could act on.
fn classify(current: Option<&str>, want: &str) -> Existing {
    match current {
        // An empty `Path` registers nothing, so it is the same situation as no
        // value at all — and restoring an empty string would be a promise to
        // put back something that never worked.
        None => Existing::Absent,
        Some(v) if v.trim().is_empty() => Existing::Absent,
        Some(v) if v.eq_ignore_ascii_case(INSTALL_WIN_DIR) => Existing::Ours,
        Some(v) if v.eq_ignore_ascii_case(want) => Existing::Same(v.to_string()),
        Some(v) => Existing::Foreign(v.to_string()),
    }
}

/// Render the record: one line per key, `<name> unset` or `<name> value <path>`.
///
/// Two states and not one string, because "there was no value" and "there was
/// this value" need opposite things done on the way out, and a format that
/// cannot tell them apart restores the wrong one — the same class of mistake as
/// recording nothing at all. The value is the rest of the line and is never
/// quoted or escaped, so a path containing spaces survives a round trip.
fn render_prior(entries: &[(String, Prior)]) -> String {
    let mut out = String::from(
        "# What this prefix's TrackIR and FreeTrack registry said before\n\
         # `tobii bridge install` first wrote to it. `tobii bridge uninstall`\n\
         # puts it back. Lines are `<key> unset` or `<key> value <path>`.\n",
    );
    for (name, prior) in entries {
        match prior {
            Prior::Unset => out.push_str(&format!("{name} unset\n")),
            Prior::Value(v) => out.push_str(&format!("{name} value {v}\n")),
        }
    }
    out
}

/// Read back what [`render_prior`] wrote.
///
/// A line that is not understood is dropped rather than guessed at: a record
/// that cannot be read is the same situation as no record, and uninstall's
/// answer to that is to leave the key alone.
fn parse_prior(text: &str) -> Vec<(String, Prior)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let mut field = line.splitn(3, ' ');
        let (Some(name), Some(kind)) = (field.next(), field.next()) else {
            continue;
        };
        match (kind, field.next()) {
            ("unset", None) => out.push((name.to_string(), Prior::Unset)),
            ("value", Some(v)) if !v.is_empty() => {
                out.push((name.to_string(), Prior::Value(v.to_string())))
            }
            _ => {}
        }
    }
    out
}

/// The record kept in `dir`, empty when there is none.
fn read_prior(dir: &Path) -> Vec<(String, Prior)> {
    parse_prior(&std::fs::read_to_string(dir.join(PRIOR_FILE)).unwrap_or_default())
}

/// Add what was found to the record, and write it.
///
/// The record names whatever this installer took the key from. A re-install
/// finds our own value in the keys and must not overwrite the answer with it —
/// that would leave uninstall restoring a directory it is about to delete — but
/// a run that takes the key off another program has to be written down even if
/// there is already a record, or `--force` promises to put back something
/// uninstall then deletes.
fn record_prior(dir: &Path, found: &[(&str, Existing)]) -> Result<(), String> {
    let mut entries = read_prior(dir);
    for (name, existing) in found {
        match entries.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => {
                if matches!(existing, Existing::Foreign(_)) {
                    slot.1 = existing.prior();
                }
            }
            None => entries.push(((*name).to_string(), existing.prior())),
        }
    }
    std::fs::write(dir.join(PRIOR_FILE), render_prior(&entries)).map_err(|e| {
        format!(
            "could not record what the registry said in {} ({e}) — refusing to \
             overwrite keys we could then not put back",
            dir.join(PRIOR_FILE).display()
        )
    })
}

/// `tobii bridge install` — copy the artifacts in and register them.
fn install(args: &[String]) -> CmdResult {
    let prefix = resolve_prefix(args)?;
    let wine = resolve_wine(&prefix, args)?;
    let src = artifact_dir(args)?;
    let dest = prefix.join(INSTALL_SUBDIR);

    // Which client TrackIR is pointed at decides what its key should say, so it
    // is settled before the registry is looked at rather than in the middle of
    // writing it.
    let explicit_np = crate::flag_value(args, "--npclient");
    let np_source = choose_npclient(explicit_np, find_installed_npclient())?;
    // FreeTrack always gets our own DLL: that ABI has no signature check, so
    // nothing stands between it and our data.
    let np_target = match &np_source {
        NpSource::Ours => INSTALL_WIN_DIR.to_string(),
        NpSource::Installed(dir) => wine_path_for(dir),
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
    let mut found: Vec<(&str, Existing)> = Vec::new();
    for (name, key, _) in KEYS {
        found.push((
            name,
            classify(read_key(&wine, &prefix, key)?.as_deref(), want_for(key)),
        ));
    }
    let taken: Vec<String> = KEYS
        .iter()
        .zip(&found)
        .filter_map(|((_, _, abi), (_, e))| match e {
            Existing::Foreign(v) => Some(format!("  {abi:<9} {v}")),
            _ => None,
        })
        .collect();
    if !taken.is_empty() && !args.iter().any(|a| a == "--force") {
        return Err(format!(
            "another program has already registered a head-tracking client in \
             this prefix:\n{}\n\
             Overwriting that would break it, and for a game that checks \
             NaturalPoint's\nsignature the client already registered is the one \
             that WORKS — ours is the\none it rejects. Pass --force to replace \
             it anyway; the old value is recorded\nand `tobii bridge uninstall` \
             puts it back.",
            taken.join("\n")
        )
        .into());
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

    // Written before the keys, never after: a record of a value we then failed
    // to overwrite is harmless, while a key overwritten with nothing recorded
    // is precisely the fault this exists to fix.
    record_prior(&dest, &found)?;

    let mut all_written = set_key(&wine, &prefix, FT_KEY, INSTALL_WIN_DIR)?;
    // Held rather than printed as we go: "registered X" is only true once every
    // write has landed, and printing it before the check put confident lines
    // above the failure that contradicted it.
    let registered;

    let mut third_party_np = false;
    match &np_source {
        NpSource::Ours => {
            all_written &= set_key(&wine, &prefix, NP_KEY, INSTALL_WIN_DIR)?;
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
            all_written &= set_key(&wine, &prefix, NP_KEY, win)?;
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
    for ((_, _, abi), (_, e)) in KEYS.iter().zip(&found) {
        if let Existing::Foreign(v) = e {
            println!("replaced the {abi} registration {v} — uninstall puts it back");
        }
    }
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

/// Point one discovery key at `dir`.
/// Write one registry key, reporting whether it actually landed.
///
/// The return value is load-bearing. This used to warn and return `Ok(())`, so
/// a wine that ran but could not serve the prefix produced two warning lines
/// followed by "registered …", the whole "now launch the game" paragraph and
/// exit 0 — four confident lines burying the two that mattered. The registry
/// path IS the installation: without it a game never finds the DLL, so a failed
/// write is a failed install and has to be reported as one.
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

/// `tobii bridge run` — run the provider in the foreground.
fn run(args: &[String]) -> CmdResult {
    let prefix = resolve_prefix(args)?;
    let wine = resolve_wine(&prefix, args)?;
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

/// `tobii bridge uninstall` — put the keys back and remove the directory.
///
/// "Put back", not "delete". These two keys are how *any* head-tracking client
/// is found, not just ours, so deleting them on the way out leaves a prefix
/// with nothing registered at all — worse than it was before we touched it, and
/// for a prefix that had opentrack's client it silently throws away the one
/// registration a signature-checking game accepts. What install found is
/// recorded in [`PRIOR_FILE`]; this undoes exactly that.
fn uninstall(args: &[String]) -> CmdResult {
    let prefix = resolve_prefix(args)?;
    let wine = resolve_wine(&prefix, args)?;
    let dir = prefix.join(INSTALL_SUBDIR);
    // Read before the directory goes: the record lives inside it.
    let prior = read_prior(&dir);

    let mut restored: Vec<String> = Vec::new();
    let mut unregistered: Vec<&str> = Vec::new();
    let mut untouched: Vec<&str> = Vec::new();
    for (name, key, abi) in KEYS {
        match prior.iter().find(|(n, _)| n == name).map(|(_, p)| p) {
            Some(Prior::Value(v)) => {
                restored.push(if set_key(&wine, &prefix, key, v)? {
                    format!("restored the {abi} client path to {v}")
                } else {
                    format!(
                        "could not restore the {abi} client path to {v} — it still points at us"
                    )
                });
            }
            Some(Prior::Unset) => {
                let _ = wine_run(&wine, &prefix, &["reg", "delete", key, "/f"]);
                unregistered.push(abi);
            }
            // No record — an older install, or a prefix someone else set up.
            // Deleting another program's registration on a guess is the fault
            // being fixed, so the key is left exactly as it is and said out
            // loud rather than quietly skipped.
            None => untouched.push(key),
        }
    }

    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)?;
        println!("removed {}", dir.display());
    }
    for line in &restored {
        println!("{line}");
    }
    if unregistered.len() == KEYS.len() {
        println!("unregistered TrackIR and FreeTrack client paths");
    } else {
        for abi in &unregistered {
            println!("unregistered the {abi} client path");
        }
    }
    if !untouched.is_empty() {
        println!(
            "\nnothing recorded what these keys held before the bridge was installed \
             here,\nso they were left as they are rather than deleting a registration \
             that may\nnot be ours:"
        );
        for key in untouched {
            println!("  {key}");
        }
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
        let (w, warn) = choose_wine(
            None,
            Some(p("/games/sc/runners/tkg-11.7/bin/wine")),
            &[p("/games/sc/runners/tkg-11.5/bin/wine")],
            Some(p("/usr/bin/wine")),
        )
        .expect("resolves");
        assert_eq!(w, p("/games/sc/runners/tkg-11.7/bin/wine"));
        assert_eq!(warn, None);
    }

    #[test]
    fn a_bundled_runner_beats_the_system_wine() {
        let (w, warn) = choose_wine(
            None,
            None,
            &[p("/games/sc/runners/tkg-11.5/bin/wine")],
            Some(p("/usr/bin/wine")),
        )
        .expect("resolves");
        assert_eq!(w, p("/games/sc/runners/tkg-11.5/bin/wine"));
        assert_eq!(warn, None);
    }

    /// Falling back to the system wine is allowed but suspicious: it is exactly
    /// the case that produces a second wineserver and an invisible mapping.
    #[test]
    fn falling_back_to_the_system_wine_warns() {
        let (w, warn) = choose_wine(None, None, &[], Some(p("/usr/bin/wine"))).expect("resolves");
        assert_eq!(w, p("/usr/bin/wine"));
        assert!(warn.expect("a warning").contains("--wine"));
    }

    /// An explicit choice is honoured — but if it disagrees with the prefix's
    /// own runner, the user hears about it, because the resulting failure looks
    /// like success everywhere except in the game.
    #[test]
    fn an_explicit_wine_that_contradicts_the_prefix_is_used_but_flagged() {
        let (w, warn) = choose_wine(
            Some(p("/usr/bin/wine")),
            Some(p("/games/sc/runners/tkg-11.7/bin/wine")),
            &[],
            Some(p("/usr/bin/wine")),
        )
        .expect("resolves");
        assert_eq!(w, p("/usr/bin/wine"), "the explicit choice still wins");
        let warn = warn.expect("a warning");
        assert!(warn.contains("tkg-11.7"), "{warn}");
        assert!(warn.contains("wineserver"), "must explain why: {warn}");
    }

    #[test]
    fn an_explicit_wine_matching_the_prefix_says_nothing() {
        let (_, warn) = choose_wine(
            Some(p("/games/sc/runners/tkg-11.7/bin/wine")),
            Some(p("/games/sc/runners/tkg-11.7/bin/wine")),
            &[],
            None,
        )
        .expect("resolves");
        assert_eq!(warn, None);
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

    /// A fake `wine`: a shell script that records every argv it is handed,
    /// answers `reg query` from a file the test writes, and can be told to fail
    /// a write. `ROOT` is replaced with the temp directory before it is written.
    const FAKE_WINE: &str = r#"#!/bin/sh
{ for a in "$@"; do printf '[%s]' "$a"; done; printf '\n'; } >> 'ROOT/argv'
if [ "$1" = reg ] && [ "$2" = query ]; then
  case "$3" in
    *NaturalPoint*) reply='ROOT/np.reply' ;;
    *)              reply='ROOT/ft.reply' ;;
  esac
  [ -f "$reply" ] || exit 1
  cat "$reply"
  exit 0
fi
if [ "$1" = reg ] && [ "$2" = add ] && [ -f 'ROOT/fail-add' ]; then
  exit 1
fi
exit 0
"#;

    /// A prefix, an artifact directory and that fake wine, in one temp tree.
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
            let wine = root.join("wine");
            std::fs::write(
                &wine,
                FAKE_WINE.replace("ROOT", &root.display().to_string()),
            )
            .expect("fake wine");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&wine, std::fs::Permissions::from_mode(0o755))
                .expect("chmod +x");
            Self { root }
        }

        fn prefix(&self) -> PathBuf {
            self.root.join("prefix")
        }

        fn dest(&self) -> PathBuf {
            self.prefix().join(INSTALL_SUBDIR)
        }

        /// Make `reg query` answer for one key, in wine's own output shape.
        fn registered(&self, which: &str, value: &str) {
            std::fs::write(
                self.root.join(format!("{which}.reply")),
                format!(
                    "\r\nHKEY_CURRENT_USER\\Software\\Whatever\r\n    \
                     Path    REG_SZ    {value}\r\n\r\n"
                ),
            )
            .expect("canned reply");
        }

        /// Make every `reg add` fail, as a wine that cannot serve the prefix
        /// does.
        fn fail_writes(&self) {
            std::fs::write(self.root.join("fail-add"), b"").expect("switch");
        }

        fn argv(&self) -> String {
            std::fs::read_to_string(self.root.join("argv")).unwrap_or_default()
        }

        fn record(&self) -> String {
            std::fs::read_to_string(self.dest().join(PRIOR_FILE)).unwrap_or_default()
        }

        fn put_record(&self, text: &str) {
            std::fs::create_dir_all(self.dest()).expect("install dir");
            std::fs::write(self.dest().join(PRIOR_FILE), text).expect("record");
        }

        fn args(&self, sub: &str, extra: &[&str]) -> Vec<String> {
            let mut v: Vec<String> = ["tobii", "bridge", sub]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
            for (flag, value) in [
                ("--prefix", self.prefix().display().to_string()),
                ("--wine", self.root.join("wine").display().to_string()),
                (
                    "--artifacts",
                    self.root.join("artifacts").display().to_string(),
                ),
                // Pinned, so the outcome does not depend on whether the machine
                // running the test happens to have opentrack installed.
                ("--npclient", "ours".to_string()),
            ] {
                v.push(flag.to_string());
                v.push(value);
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
    #[test]
    fn the_registered_path_is_read_out_of_reg_query_whole() {
        assert_eq!(
            reg_query_path(
                "\r\nHKEY_CURRENT_USER\\Software\\Freetrack\\FreeTrackClient\r\n    \
                 Path    REG_SZ    C:\\Program Files\\opentrack\r\n\r\n"
            )
            .as_deref(),
            Some(r"C:\Program Files\opentrack")
        );
        // What wine prints when there is nothing there.
        assert_eq!(
            reg_query_path("ERROR: The system was unable to find the specified registry key\n"),
            None
        );
        // A different value whose name merely starts with `Path`.
        assert_eq!(reg_query_path("    PathOther    REG_SZ    x\n"), None);
        assert_eq!(reg_query_path(""), None);
    }

    /// The cases install has to tell apart, and what each leaves for uninstall
    /// to put back.
    #[test]
    fn a_value_that_is_neither_absent_nor_ours_is_someone_elses() {
        assert_eq!(classify(None, INSTALL_WIN_DIR), Existing::Absent);
        // An empty Path registers nothing, so it is the same as no value.
        assert_eq!(classify(Some("  "), INSTALL_WIN_DIR), Existing::Absent);
        // Windows paths are case-insensitive; refusing over a drive letter
        // would be a refusal nobody could act on.
        assert_eq!(
            classify(Some(r"c:\TOBII-BRIDGE"), INSTALL_WIN_DIR),
            Existing::Ours
        );
        assert_eq!(
            classify(Some(r"C:\opentrack"), INSTALL_WIN_DIR),
            Existing::Foreign(r"C:\opentrack".to_string())
        );
        // Only our own directory is deleted on the way out. A third-party path
        // equal to what we would write may be that program's own registration,
        // so it is put back rather than removed.
        assert_eq!(Existing::Ours.prior(), Prior::Unset);
        assert_eq!(Existing::Absent.prior(), Prior::Unset);
        assert_eq!(
            classify(
                Some(r"Z:\usr\libexec\opentrack"),
                r"Z:\usr\libexec\opentrack"
            )
            .prior(),
            Prior::Value(r"Z:\usr\libexec\opentrack".to_string())
        );
    }

    /// Restoring "there was no value" and "there was this value" are opposite
    /// actions, so a record that cannot tell them apart is the same bug again.
    #[test]
    fn the_record_tells_no_value_apart_from_a_value() {
        let entries = vec![
            (
                "np".to_string(),
                Prior::Value(r"C:\Program Files\x".to_string()),
            ),
            ("ft".to_string(), Prior::Unset),
        ];
        assert_eq!(parse_prior(&render_prior(&entries)), entries);
        // A damaged line is dropped, never guessed at: no record and an
        // unreadable one must both end in leaving the key alone.
        assert_eq!(
            parse_prior("np value\nft wat\n# a comment\n\nnp unset\n"),
            vec![("np".to_string(), Prior::Unset)]
        );
    }

    /// The bug this whole path exists for: a prefix that already has a working
    /// head-tracking setup must not be overwritten by a blind `reg add`.
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
        let argv = w.argv();
        assert!(argv.contains("[query]"), "must have read first: {argv}");
        assert!(!argv.contains("[add]"), "nothing may be written: {argv}");
        assert!(!w.dest().exists(), "and nothing copied either");
    }

    /// `--force` is the deliberate override, and the point of it is that it is
    /// still recoverable: what it replaced is written down.
    #[test]
    fn force_replaces_it_but_records_what_it_replaced() {
        let w = FakeWine::new("force");
        w.registered("np", r"Z:\usr\libexec\opentrack");
        w.registered("ft", r"C:\freetrack");
        install(&w.args("install", &["--force"])).expect("installs");
        let rec = w.record();
        assert!(rec.contains(r"np value Z:\usr\libexec\opentrack"), "{rec}");
        assert!(rec.contains(r"ft value C:\freetrack"), "{rec}");
        let argv = w.argv();
        assert!(
            argv.contains(
                r"[add][HKCU\Software\Freetrack\FreeTrackClient][/v][Path][/t][REG_SZ][/d][C:\tobii-bridge][/f]"
            ),
            "{argv}"
        );
    }

    /// The other half of the record: an untouched prefix held nothing, and
    /// uninstall may then delete — which is only right because it was recorded.
    #[test]
    fn an_unregistered_prefix_is_recorded_as_having_held_nothing() {
        let w = FakeWine::new("absent");
        install(&w.args("install", &[])).expect("installs");
        let rec = w.record();
        assert!(rec.contains("np unset"), "{rec}");
        assert!(rec.contains("ft unset"), "{rec}");
    }

    /// The record answers "what did this prefix say before `tobii` ever wrote
    /// to it", so re-installing — which finds our own value there — must not
    /// overwrite the answer with our own value, and must not refuse either.
    #[test]
    fn a_second_install_neither_refuses_nor_overwrites_the_first_record() {
        let w = FakeWine::new("twice");
        w.registered("np", r"Z:\usr\libexec\opentrack");
        install(&w.args("install", &["--force"])).expect("first install");
        w.registered("np", INSTALL_WIN_DIR);
        w.registered("ft", INSTALL_WIN_DIR);
        install(&w.args("install", &[])).expect("our own value is not someone else's");
        // Read back as entries and compared whole: a second entry for the same
        // key appended below the first would leave the text assertion happy
        // while the record had two answers to one question.
        assert_eq!(
            read_prior(&w.dest()),
            vec![
                ("ft".to_string(), Prior::Unset),
                (
                    "np".to_string(),
                    Prior::Value(r"Z:\usr\libexec\opentrack".to_string())
                ),
            ]
        );
    }

    /// The other side of that: a run that takes the key off another program
    /// must say so even though a record already exists, or `--force` promises
    /// to put back a value uninstall then deletes.
    #[test]
    fn a_later_run_records_whatever_it_most_recently_took() {
        let w = FakeWine::new("retake");
        install(&w.args("install", &[])).expect("first install");
        // Something else claims the TrackIR key after we were already here.
        w.registered("np", r"Z:\usr\libexec\opentrack");
        w.registered("ft", INSTALL_WIN_DIR);
        install(&w.args("install", &["--force"])).expect("second install");
        assert_eq!(
            read_prior(&w.dest()),
            vec![
                ("ft".to_string(), Prior::Unset),
                (
                    "np".to_string(),
                    Prior::Value(r"Z:\usr\libexec\opentrack".to_string())
                ),
            ]
        );
    }

    /// Deleting leaves the prefix with NO client registered, which is worse
    /// than it was before we touched it.
    #[test]
    fn uninstall_puts_back_what_was_there_instead_of_deleting_it() {
        let w = FakeWine::new("restore");
        w.put_record("np value Z:\\usr\\libexec\\opentrack\nft unset\n");
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(
            argv.contains(
                r"[add][HKCU\Software\NaturalPoint\NATURALPOINT\NPClient Location][/v][Path][/t][REG_SZ][/d][Z:\usr\libexec\opentrack][/f]"
            ),
            "the TrackIR key must be restored: {argv}"
        );
        assert!(
            !argv.contains(r"[delete][HKCU\Software\NaturalPoint"),
            "and not deleted: {argv}"
        );
        // Recorded as having held nothing, so deleting is the restoration.
        assert!(
            argv.contains(r"[delete][HKCU\Software\Freetrack\FreeTrackClient][/f]"),
            "{argv}"
        );
    }

    /// With no record there is nothing to say the key is ours, and deleting
    /// another program's registration on the way out is the fault being fixed.
    #[test]
    fn uninstall_without_a_record_leaves_both_keys_alone() {
        let w = FakeWine::new("norecord");
        std::fs::create_dir_all(w.dest()).expect("install dir");
        std::fs::write(w.dest().join(REQUIRED_ARTIFACT), b"dll").expect("dll");
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(!argv.contains("[delete]"), "nothing may be deleted: {argv}");
        assert!(!argv.contains("[add]"), "and nothing written: {argv}");
        assert!(!w.dest().exists(), "the directory still goes");
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
