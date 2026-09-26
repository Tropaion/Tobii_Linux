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

/// A key every wine prefix has, used to tell "nothing is registered" apart
/// from "this wine cannot read this prefix at all".
const PROBE_KEY: &str = r"HKCU\Software";

/// A registry value type this installer can read and write back unchanged.
///
/// Only these two. A `Path` stored as anything else is a value we could not
/// put back the way we found it, and writing back something we cannot spell is
/// how a working registration gets destroyed while the destruction is reported
/// as a successful restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegTy {
    Sz,
    ExpandSz,
}

impl RegTy {
    fn parse(token: &str) -> Option<Self> {
        match token {
            "REG_SZ" => Some(RegTy::Sz),
            "REG_EXPAND_SZ" => Some(RegTy::ExpandSz),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            RegTy::Sz => "REG_SZ",
            RegTy::ExpandSz => "REG_EXPAND_SZ",
        }
    }
}

/// A `Path` value as it is stored: the string, and the type it is stored as.
///
/// The type travels with the value because `%ProgramFiles%\opentrack` put back
/// as `REG_SZ` is not the value that was there — the game would then look for
/// a directory spelled with literal percent signs, and find nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RegVal {
    ty: RegTy,
    value: String,
}

impl RegVal {
    /// The plain string form this installer writes for its own paths.
    fn sz(value: &str) -> Self {
        RegVal {
            ty: RegTy::Sz,
            value: value.to_string(),
        }
    }
}

/// What `reg query <key> /v Path` said.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reading {
    /// No key, no `Path` value, or an empty one — nothing is registered.
    Absent,
    /// A value we can compare, record, and write back exactly.
    Value(RegVal),
    /// Something IS registered and this installer cannot read it faithfully.
    /// Carries the reason, because only the user can act on it.
    Unreadable(String),
}

/// Pull the `Path` value out of `reg query <key> /v Path` output.
///
/// Parsed as bytes, deliberately not as text. Wine's `reg.exe` prints in the
/// console's OEM codepage and not UTF-8: checked against wine 11.18 here,
/// `C:\Program Files\Müller opentrack` comes back with a bare `0x81` (CP850's
/// `ü`), which is not valid UTF-8, and forcing `LC_ALL=C.UTF-8` does not change
/// it. `from_utf8_lossy` would turn that into U+FFFD, and the mangled string
/// would then be recorded and later written back as the "restored" value —
/// destroying a working registration and reporting it as a success. ASCII is
/// the one region every codepage wine picks agrees on, so a value that is pure
/// ASCII is read and one that is not is refused by name.
///
/// The line is `    Path    REG_SZ    C:\tobii-bridge`. The type is read as the
/// field it is: `REG_SZ` is NOT a substring of `REG_EXPAND_SZ` (the letter
/// after `REG_` differs), so matching it as one reads an expandable value —
/// `%ProgramFiles%\opentrack`, the natural spelling for an installer script —
/// as no value at all, and clobbers it. The value is the whole remainder of
/// the line, since `C:\Program Files\...` is an entirely ordinary thing to
/// find registered here and one word of it would be compared against ours.
fn reg_query_path(stdout: &[u8]) -> Reading {
    for line in stdout.split(|b| *b == b'\n') {
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
        let head = head.trim_end_matches('\r').trim_start();
        let Some(rest) = head.strip_prefix("Path") else {
            continue;
        };
        // `Path` and not `PathX`: the name must end where we stopped reading.
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        if ascii.len() != line.len() {
            return Reading::Unreadable(
                "its value has characters outside ASCII, which wine does not print \
                 in UTF-8"
                    .to_string(),
            );
        }
        let rest = rest.trim_start();
        let (ty, value) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let Some(ty) = RegTy::parse(ty) else {
            return Reading::Unreadable(format!(
                "it is stored as {ty}, which this installer cannot write back"
            ));
        };
        let value = value.trim();
        // An empty `Path` registers nothing, so it is the same situation as no
        // value at all — and restoring an empty string would be a promise to
        // put back something that never worked.
        if value.is_empty() {
            return Reading::Absent;
        }
        return Reading::Value(RegVal {
            ty,
            value: value.to_string(),
        });
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
/// actually there and recording that there was nothing.
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

/// What one key said before this installer first wrote to it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Prior {
    /// The key had no `Path` value at all, so uninstall removes the one we
    /// added. That is only correct *because* it was recorded: deleting on a
    /// guess is the whole fault this record exists to prevent.
    Unset,
    /// The key named this value, and uninstall puts it back.
    Value(RegVal),
}

/// What a key already says, judged against the value this install would write
/// and against what this installer last wrote there itself.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Existing {
    /// Nothing registered.
    Absent,
    /// Ours: either our own install directory inside this prefix, which
    /// nothing but this installer ever names, or the exact value our own last
    /// install wrote and wrote down. Both are values we put there, so there is
    /// nothing of anyone else's to protect — and refusing over one of them
    /// would be refusing over our own handiwork, which no user can act on.
    Ours,
    /// Already exactly what this install would write, but naming a directory
    /// outside the prefix — so it may just as well be that program's own
    /// registration of the same client DLL, written before we ever ran.
    /// Writing it again changes nothing; putting it back on the way out is the
    /// reading that cannot break anything.
    Same(RegVal),
    /// Another program's registration, which is not ours to overwrite.
    Foreign(RegVal),
    /// Something is registered that we could not read faithfully. Not absent,
    /// not ours, and not a value we could promise to restore.
    Unreadable(String),
}

impl Existing {
    /// What uninstall should put back, if this install writes over it —
    /// `None` when the answer is not known and must not be invented.
    fn prior(&self) -> Option<Prior> {
        match self {
            Existing::Absent => Some(Prior::Unset),
            // Our own value says nothing about what was here before us: it is
            // here *because* we wrote it. If nothing was recorded at the time,
            // the honest record is still no record — "there was nothing" is a
            // claim about a key whose earlier value is genuinely unknown. Same
            // for a value we could not read: we have no value to write down.
            Existing::Ours | Existing::Unreadable(_) => None,
            Existing::Same(v) | Existing::Foreign(v) => Some(Prior::Value(v.clone())),
        }
    }
}

/// Judge an existing `Path` value against the one we mean to write, and
/// against the one this installer last wrote here.
///
/// Compared case-insensitively because Windows paths are, and because a prefix
/// that spells the drive letter the other way round is not a different
/// registration — refusing over that would be a refusal nobody could act on.
///
/// `wrote` is what the record says this installer last put in this key, and it
/// is what stops a re-install refusing about its own work: an install that
/// pointed TrackIR at opentrack's client leaves a value that is neither ours
/// nor what a later `--npclient ours` run would write, and without the record
/// that run would report "another program has already registered…" about a
/// value it wrote itself — and, forced, would record that value as the prior
/// to restore, so uninstall would CREATE a registration in a prefix that never
/// had one.
fn classify(current: &Reading, want: &str, wrote: Option<&str>) -> Existing {
    match current {
        Reading::Absent => Existing::Absent,
        Reading::Unreadable(why) => Existing::Unreadable(why.clone()),
        Reading::Value(v) => {
            // Only a plain string can be one of ours: we have never written
            // anything else, so an expandable value that happens to read the
            // same is somebody else's doing and is left alone.
            let plain = v.ty == RegTy::Sz;
            if plain
                && (v.value.eq_ignore_ascii_case(INSTALL_WIN_DIR)
                    || wrote.is_some_and(|w| v.value.eq_ignore_ascii_case(w)))
            {
                Existing::Ours
            } else if plain && v.value.eq_ignore_ascii_case(want) {
                Existing::Same(v.clone())
            } else {
                Existing::Foreign(v.clone())
            }
        }
    }
}

/// One key's place in the record: what it held before this installer first
/// wrote to it, and what this installer last put there.
///
/// `wrote` is the second half of "read before you write". Uninstall compares
/// it against what the key says *now* and touches the key only while they
/// still match: without it, a key some other program claimed in the weeks
/// after our install is indistinguishable from the one we left, and putting
/// the recorded value "back" — or deleting, for a key recorded as having held
/// nothing — destroys that program's registration. That is the very fault this
/// record exists to prevent, committed on the way out instead of on the way in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Entry {
    /// `None` when nothing is known about what came before, which is not the
    /// same as knowing there was nothing.
    prior: Option<Prior>,
    wrote: Option<String>,
}

/// Render the record: one line per fact, `<key> prior …` and `<key> wrote …`.
///
/// Two spellings for the prior and not one possibly-empty string, because
/// "there was no value" and "there was this value" need opposite things done
/// on the way out, and a format that cannot tell them apart restores the wrong
/// one — the same class of mistake as recording nothing at all. A fact that is
/// not known has no line, so it cannot be read back as a fact that is. The
/// value is the rest of the line and is never quoted or escaped, so a path
/// containing spaces survives a round trip.
fn render_prior(entries: &[(String, Entry)]) -> String {
    let mut out = String::from(
        "# What this prefix's TrackIR and FreeTrack registry said before\n\
         # `tobii bridge install` first wrote to it, and what it then wrote.\n\
         # `tobii bridge uninstall` reads those keys and puts the old values\n\
         # back only where they still say what we wrote. Lines are\n\
         #   <key> prior unset | <key> prior value <TYPE> <path> | <key> wrote <path>\n",
    );
    for (name, entry) in entries {
        match &entry.prior {
            None => {}
            Some(Prior::Unset) => out.push_str(&format!("{name} prior unset\n")),
            Some(Prior::Value(v)) => out.push_str(&format!(
                "{name} prior value {} {}\n",
                v.ty.as_str(),
                v.value
            )),
        }
        if let Some(w) = &entry.wrote {
            out.push_str(&format!("{name} wrote {w}\n"));
        }
    }
    out
}

/// Read back what [`render_prior`] wrote.
///
/// A line that is not understood is dropped rather than guessed at: a record
/// that cannot be read is the same situation as no record, and the answer to
/// that is to leave the key alone.
fn parse_prior(text: &str) -> Vec<(String, Entry)> {
    enum Fact {
        Prior(Prior),
        Wrote(String),
    }
    let mut out: Vec<(String, Entry)> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let mut field = line.splitn(3, ' ');
        let (Some(name), Some(kind), Some(rest)) = (field.next(), field.next(), field.next())
        else {
            continue;
        };
        let fact = match kind {
            "prior" if rest == "unset" => Some(Fact::Prior(Prior::Unset)),
            "prior" => rest
                .strip_prefix("value ")
                .and_then(|r| r.split_once(' '))
                .and_then(|(ty, value)| {
                    Some(RegVal {
                        ty: RegTy::parse(ty)?,
                        value: value.to_string(),
                    })
                })
                .filter(|v| !v.value.is_empty())
                .map(|v| Fact::Prior(Prior::Value(v))),
            "wrote" if !rest.is_empty() => Some(Fact::Wrote(rest.to_string())),
            _ => None,
        };
        let (Some(fact), false) = (fact, name.is_empty()) else {
            continue;
        };
        let slot = slot_for(&mut out, name);
        match fact {
            Fact::Prior(p) => slot.prior = Some(p),
            Fact::Wrote(w) => slot.wrote = Some(w),
        }
    }
    out
}

/// The entry for `name`, appended in first-seen order if it is new.
fn slot_for<'a>(entries: &'a mut Vec<(String, Entry)>, name: &str) -> &'a mut Entry {
    match entries.iter().position(|(n, _)| n == name) {
        Some(i) => &mut entries[i].1,
        None => {
            entries.push((name.to_string(), Entry::default()));
            &mut entries.last_mut().expect("just pushed").1
        }
    }
}

/// The record kept in `dir`, empty when there is none.
fn read_prior(dir: &Path) -> Vec<(String, Entry)> {
    parse_prior(&std::fs::read_to_string(dir.join(PRIOR_FILE)).unwrap_or_default())
}

/// Add what was found, and what we are about to write, to the record.
///
/// The prior answers "what did this prefix say before `tobii` ever wrote to
/// it". A re-install finds our own value in the keys and must not overwrite
/// that answer with it — that would leave uninstall restoring a directory it
/// is about to delete — but a run that takes a live value over, whether it is
/// another program's (`Foreign`) or the very client we are about to register
/// (`Same`, which may equally be that program's own registration), has to be
/// written down even when a record already exists, or uninstall deletes a
/// registration it promised to put back.
///
/// Written `.new`-then-renamed, like the DLLs and for the same reason: a
/// half-written record is read back as a record with a line missing, and the
/// next install would then fill that line in from a registry it had already
/// written to itself.
fn record_prior(dir: &Path, found: &[(&str, Existing, String)]) -> Result<(), String> {
    let mut entries = read_prior(dir);
    for (name, existing, writing) in found {
        let slot = slot_for(&mut entries, name);
        match existing {
            Existing::Same(_) | Existing::Foreign(_) => slot.prior = existing.prior(),
            Existing::Absent if slot.prior.is_none() => slot.prior = Some(Prior::Unset),
            _ => {}
        }
        slot.wrote = Some(writing.clone());
    }
    let path = dir.join(PRIOR_FILE);
    let staged = dir.join(format!(".{PRIOR_FILE}.new"));
    std::fs::write(&staged, render_prior(&entries))
        .and_then(|()| std::fs::rename(&staged, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&staged);
            format!(
                "could not record what the registry said in {} ({e}) — refusing to \
                 overwrite keys we could then not put back",
                path.display()
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
    //
    // The record is read first too, because what this installer wrote here
    // last time is part of reading the key honestly: without it our own
    // previous registration looks exactly like a stranger's.
    let record = read_prior(&dest);
    let wrote_for = |name: &str| {
        record
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, e)| e.wrote.clone())
    };
    let mut found: Vec<(&str, Existing, String)> = Vec::new();
    for (name, key, _) in KEYS {
        let current = read_key(&wine, &prefix, key)?;
        let want = want_for(key).to_string();
        found.push((
            name,
            classify(&current, &want, wrote_for(name).as_deref()),
            want,
        ));
    }
    // A value we could not read is refused whatever the flags say. --force is
    // "replace it, the old value is recorded and uninstall puts it back", and
    // here we have no old value to record: forcing would destroy a working
    // registration and promise a restoration we could not perform.
    let unreadable: Vec<String> = KEYS
        .iter()
        .zip(&found)
        .filter_map(|((_, key, abi), (_, e, _))| match e {
            Existing::Unreadable(why) => Some(format!("  {abi:<9} {key}\n            {why}")),
            _ => None,
        })
        .collect();
    if !unreadable.is_empty() {
        return Err(format!(
            "something is registered in this prefix that this installer cannot \
             read:\n{}\n\
             Wine prints the registry in the console's own codepage rather than \
             UTF-8, so a\nvalue like that cannot be read back exactly — and a \
             value we cannot read is one we\ncould not put back. Overwriting it \
             would destroy a working registration, and\n--force would only \
             promise a restoration we cannot perform. If the value is stale,\n\
             clear it yourself — `wine reg delete \"<key>\" /v Path /f` — and run \
             this again.",
            unreadable.join("\n")
        )
        .into());
    }
    let taken: Vec<String> = KEYS
        .iter()
        .zip(&found)
        .filter_map(|((_, _, abi), (_, e, _))| match e {
            Existing::Foreign(v) => Some(format!("  {abi:<9} {}", v.value)),
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

    let mut all_written = set_key(&wine, &prefix, FT_KEY, &RegVal::sz(INSTALL_WIN_DIR))?;
    // Held rather than printed as we go: "registered X" is only true once every
    // write has landed, and printing it before the check put confident lines
    // above the failure that contradicted it.
    let registered;

    let mut third_party_np = false;
    match &np_source {
        NpSource::Ours => {
            all_written &= set_key(&wine, &prefix, NP_KEY, &RegVal::sz(INSTALL_WIN_DIR))?;
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
            all_written &= set_key(&wine, &prefix, NP_KEY, &RegVal::sz(win))?;
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
    for ((_, _, abi), (_, e, _)) in KEYS.iter().zip(&found) {
        if let Existing::Foreign(v) = e {
            println!(
                "replaced the {abi} registration {} — uninstall puts it back",
                v.value
            );
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
fn set_key(wine: &Path, prefix: &Path, key: &str, val: &RegVal) -> Result<bool, String> {
    let status = wine_run(
        wine,
        prefix,
        &[
            "reg",
            "add",
            key,
            "/v",
            "Path",
            "/t",
            val.ty.as_str(),
            "/d",
            &val.value,
            "/f",
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
fn del_value(wine: &Path, prefix: &Path, key: &str) -> Result<bool, String> {
    let status = wine_run(wine, prefix, &["reg", "delete", key, "/v", "Path", "/f"])
        .map_err(|e| format!("{e}"))?;
    if !status.success() {
        eprintln!("warning: could not remove Path from {key}");
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

/// What uninstall should do to one key, having read what it says now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Undo {
    /// The key still holds what we wrote, and something was there before us.
    Restore(RegVal),
    /// The key still holds what we wrote, and nothing was there before us.
    Remove,
    /// Not ours to touch, and why — which the user is told, because residue
    /// left deliberately and residue left by a bug look identical otherwise.
    Leave(&'static str),
}

/// Decide from what the key says *now*, never from the record alone.
///
/// Install reads before it writes; so must this. The record says what was
/// there before us and what we left behind, but between then and now the user
/// may have installed opentrack, switched trackers, or cleared the key by
/// hand — and then a `reg delete` aimed at the value we left deletes whatever
/// replaced it, and a `reg add` of the recorded prior overwrites it. A key
/// that no longer says what we wrote belongs to somebody else, and the only
/// safe thing to do with it is nothing.
fn undo_for(current: &Reading, entry: Option<&Entry>) -> Undo {
    let Reading::Value(v) = current else {
        return Undo::Leave(match current {
            Reading::Absent => "nothing is registered there any more",
            _ => "it holds a value this installer cannot read, so it is not ours",
        });
    };
    let wrote = entry.and_then(|e| e.wrote.as_deref());
    let ours = v.ty == RegTy::Sz
        && (v.value.eq_ignore_ascii_case(INSTALL_WIN_DIR)
            || wrote.is_some_and(|w| v.value.eq_ignore_ascii_case(w)));
    if !ours {
        return Undo::Leave(
            "it no longer holds what this install wrote, so it is another program's now",
        );
    }
    match entry.and_then(|e| e.prior.clone()) {
        Some(Prior::Value(p)) => Undo::Restore(p),
        Some(Prior::Unset) => Undo::Remove,
        // Nothing recorded what came before. `C:\tobii-bridge` is a directory
        // nothing but this installer ever names, and it is about to stop
        // existing, so taking the pointer to it out is right even with no
        // record — which is every prefix installed before this record existed,
        // and the one the full-uninstall instructions send people through. A
        // third-party path we merely pointed at is a different matter: it may
        // have been that program's own registration all along.
        None if v.value.eq_ignore_ascii_case(INSTALL_WIN_DIR) => Undo::Remove,
        None => Undo::Leave("nothing recorded what it held before the bridge pointed it here"),
    }
}

/// `tobii bridge uninstall` — put the keys back and remove the directory.
///
/// "Put back", not "delete". These two keys are how *any* head-tracking client
/// is found, not just ours, so deleting them on the way out leaves a prefix
/// with nothing registered at all — worse than it was before we touched it, and
/// for a prefix that had opentrack's client it silently throws away the one
/// registration a signature-checking game accepts. What install found is
/// recorded in [`PRIOR_FILE`]; this undoes exactly that, and only where the
/// keys still say what install left.
///
/// The directory goes last and only if the registry came out right: it holds
/// the record, and a record deleted after a failed restore leaves the prefix
/// pointing at a directory that no longer exists with the value to put back
/// gone for good.
fn uninstall(args: &[String]) -> CmdResult {
    let prefix = resolve_prefix(args)?;
    let wine = resolve_wine(&prefix, args)?;
    let dir = prefix.join(INSTALL_SUBDIR);
    // Read before the directory goes: the record lives inside it.
    let record = read_prior(&dir);

    let mut said: Vec<String> = Vec::new();
    let mut unregistered: Vec<&str> = Vec::new();
    let mut untouched: Vec<(&str, &str)> = Vec::new();
    let mut failed = false;
    for (name, key, abi) in KEYS {
        let entry = record.iter().find(|(n, _)| n == name).map(|(_, e)| e);
        let current = read_key(&wine, &prefix, key)?;
        match undo_for(&current, entry) {
            Undo::Restore(v) => said.push(if set_key(&wine, &prefix, key, &v)? {
                format!("restored the {abi} client path to {}", v.value)
            } else {
                failed = true;
                format!("could not restore the {abi} client path to {}", v.value)
            }),
            Undo::Remove => {
                if del_value(&wine, &prefix, key)? {
                    unregistered.push(abi);
                } else {
                    failed = true;
                    said.push(format!("could not unregister the {abi} client path"));
                }
            }
            Undo::Leave(why) => untouched.push((key, why)),
        }
    }

    for line in &said {
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
            "\nthese keys were left exactly as they are, because deleting or \
             overwriting a\nregistration that may not be ours is the fault this \
             record exists to prevent:"
        );
        for (key, why) in untouched {
            println!("  {key}\n    {why}");
        }
    }

    // Nothing below here is true if the registry did not come out right, and
    // the directory holds the only copy of what is left to put back.
    if failed {
        let record = dir.join(PRIOR_FILE);
        return Err(format!(
            "the registry could not be put back, so {} is still there{}\n\
             Run `tobii bridge uninstall` again once wine can serve this prefix \
             (for a\nSteam title that means the Proton build it records; pass \
             --wine explicitly if\nthis one is wrong for it).",
            dir.display(),
            // Said only when it is true: with no record there is nothing in
            // there to keep, only the artifacts and the keys still pointing at
            // them.
            if record.is_file() {
                format!(
                    " —\n{} still holds what these keys said before the bridge \
                     was installed.",
                    record.display()
                )
            } else {
                ".".to_string()
            }
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

    /// A fake `wine`: a shell script that records every argv it is handed and
    /// keeps the two `Path` values in files, answering `reg query` from them in
    /// wine's real output shape and applying `reg add` and `reg delete` to
    /// them. Stateful on purpose — the faults this file exists to prevent all
    /// live in the gap between what a key said when we installed and what it
    /// says when we uninstall, and a harness with canned answers only cannot
    /// reach them.
    ///
    /// It takes its state directory from `WINEPREFIX`, which the code under
    /// test sets, so one script serves every test. That is not tidiness: see
    /// [`fake_wine`].
    const FAKE_WINE: &str = r#"#!/bin/sh
root=$(dirname "$WINEPREFIX")
{ for a in "$@"; do printf '[%s]' "$a"; done; printf '\n'; } >> "$root/argv"
if [ -f "$root/fail-wine" ]; then exit 1; fi
case "$3" in
  *NaturalPoint*) reply="$root/np.reply" ;;
  *)              reply="$root/ft.reply" ;;
esac
if [ "$1" = reg ] && [ "$2" = query ]; then
  if [ "$3" = 'HKCU\Software' ]; then exit 0; fi
  if [ -f "$reply" ]; then cat "$reply"; exit 0; fi
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
  rm -f "$reply"
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

        /// Make wine fail outright, as one pointed at a prefix it cannot use
        /// does — including the `reg query` that would otherwise read as
        /// "nothing is registered here".
        fn fail_wine(&self) {
            std::fs::write(self.root.join("fail-wine"), b"").expect("switch");
        }

        /// A directory holding a third-party client, for `--npclient DIR`.
        fn npclient_dir(&self) -> PathBuf {
            let dir = self.root.join("opentrack");
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
    #[test]
    fn the_registered_path_is_read_out_of_reg_query_whole() {
        assert_eq!(
            reg_query_path(
                b"\r\nHKEY_CURRENT_USER\\Software\\Freetrack\\FreeTrackClient\r\n    \
                  Path    REG_SZ    C:\\Program Files\\opentrack\r\n\r\n"
            ),
            Reading::Value(RegVal::sz(r"C:\Program Files\opentrack"))
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
        // An empty `Path` registers nothing, so it is the same as no value.
        assert_eq!(
            reg_query_path(b"    Path    REG_SZ    \r\n"),
            Reading::Absent
        );
    }

    /// `REG_SZ` is not a substring of `REG_EXPAND_SZ` — the letter after `REG_`
    /// differs — so a type matched as a substring reads `%ProgramFiles%\...`,
    /// which is the natural spelling for an installer script and legal for
    /// these keys, as *no value at all*: clobbered, recorded as having held
    /// nothing, and deleted on the way out.
    #[test]
    fn an_expandable_path_is_a_value_and_not_an_absence() {
        assert_eq!(
            reg_query_path(b"    Path    REG_EXPAND_SZ    %ProgramFiles%\\opentrack\r\n"),
            Reading::Value(RegVal {
                ty: RegTy::ExpandSz,
                value: r"%ProgramFiles%\opentrack".to_string(),
            })
        );
        // A type we could not write back unchanged is not an absence either.
        let multi = reg_query_path(b"    Path    REG_MULTI_SZ    a\\0b\r\n");
        assert!(
            matches!(&multi, Reading::Unreadable(why) if why.contains("REG_MULTI_SZ")),
            "{multi:?}"
        );
    }

    /// Wine's `reg.exe` prints the console's OEM codepage, not UTF-8: checked
    /// against wine 11.18 here, `C:\Program Files\Müller opentrack` comes back
    /// with a bare 0x81 and no locale setting changes it. Read lossily, that
    /// becomes U+FFFD — a string that is then recorded, written back, and
    /// announced as the restored value while the registration it replaced is
    /// destroyed.
    #[test]
    fn a_value_wine_cannot_print_faithfully_is_refused_not_mangled() {
        let mut out: Vec<u8> = b"    Path    REG_SZ    C:\\Program Files\\M".to_vec();
        out.push(0x81);
        out.extend_from_slice(b"ller opentrack\r\n");
        let reading = reg_query_path(&out);
        assert!(
            matches!(&reading, Reading::Unreadable(why) if why.contains("ASCII")),
            "{reading:?}"
        );
        // And it is emphatically not absent: something IS registered there.
        assert_ne!(reading, Reading::Absent);
    }

    /// The cases install has to tell apart, and what each leaves for uninstall
    /// to put back.
    #[test]
    fn a_value_that_is_neither_absent_nor_ours_is_someone_elses() {
        let sz = |v: &str| Reading::Value(RegVal::sz(v));
        assert_eq!(
            classify(&Reading::Absent, INSTALL_WIN_DIR, None),
            Existing::Absent
        );
        // Windows paths are case-insensitive; refusing over a drive letter
        // would be a refusal nobody could act on.
        assert_eq!(
            classify(&sz(r"c:\TOBII-BRIDGE"), INSTALL_WIN_DIR, None),
            Existing::Ours
        );
        assert_eq!(
            classify(&sz(r"C:\opentrack"), INSTALL_WIN_DIR, None),
            Existing::Foreign(RegVal::sz(r"C:\opentrack"))
        );
        // The value our own last install wrote is ours, however little it
        // looks like our directory: a run that pointed TrackIR at opentrack's
        // client and a run that pointed it at ours must not accuse each other.
        assert_eq!(
            classify(
                &sz(r"Z:\usr\libexec\opentrack"),
                INSTALL_WIN_DIR,
                Some(r"Z:\usr\libexec\opentrack")
            ),
            Existing::Ours
        );
        // Only our own directory is removed on the way out. A third-party path
        // equal to what we would write may be that program's own registration,
        // so it is put back rather than removed.
        assert_eq!(Existing::Ours.prior(), None);
        assert_eq!(Existing::Absent.prior(), Some(Prior::Unset));
        assert_eq!(
            classify(
                &sz(r"Z:\usr\libexec\opentrack"),
                r"Z:\usr\libexec\opentrack",
                None
            )
            .prior(),
            Some(Prior::Value(RegVal::sz(r"Z:\usr\libexec\opentrack")))
        );
        // A value we could not read is neither absent nor ours.
        let unreadable = Reading::Unreadable("because".to_string());
        assert!(matches!(
            classify(&unreadable, INSTALL_WIN_DIR, None),
            Existing::Unreadable(_)
        ));
        assert_eq!(classify(&unreadable, INSTALL_WIN_DIR, None).prior(), None);
    }

    /// Uninstall decides from what the key says now, not from the record
    /// alone — the record says what we left there, not what is there.
    #[test]
    fn a_key_that_no_longer_says_what_we_wrote_is_left_alone() {
        let ours = Reading::Value(RegVal::sz(INSTALL_WIN_DIR));
        let unset = Entry {
            prior: Some(Prior::Unset),
            wrote: Some(INSTALL_WIN_DIR.to_string()),
        };
        assert_eq!(undo_for(&ours, Some(&unset)), Undo::Remove);
        // Someone else's now: neither deleted nor overwritten.
        let theirs = Reading::Value(RegVal::sz(r"Z:\usr\libexec\opentrack"));
        assert!(matches!(undo_for(&theirs, Some(&unset)), Undo::Leave(_)));
        let had = Entry {
            prior: Some(Prior::Value(RegVal::sz(r"C:\freetrack"))),
            wrote: Some(INSTALL_WIN_DIR.to_string()),
        };
        assert!(matches!(undo_for(&theirs, Some(&had)), Undo::Leave(_)));
        assert_eq!(
            undo_for(&ours, Some(&had)),
            Undo::Restore(RegVal::sz(r"C:\freetrack"))
        );
        // A prefix installed before this record existed still has our own
        // directory in the key, and that directory is about to go.
        assert_eq!(undo_for(&ours, None), Undo::Remove);
        // But a third-party path we merely pointed at, with nothing recorded,
        // may have been that program's own registration all along.
        let pointed = Entry {
            prior: None,
            wrote: Some(r"Z:\usr\libexec\opentrack".to_string()),
        };
        assert!(matches!(undo_for(&theirs, Some(&pointed)), Undo::Leave(_)));
        assert!(matches!(
            undo_for(&Reading::Absent, Some(&unset)),
            Undo::Leave(_)
        ));
    }

    /// Restoring "there was no value" and "there was this value" are opposite
    /// actions, so a record that cannot tell them apart is the same bug again —
    /// and a fact that is not known has to have no line at all, or it is read
    /// back as a fact that is.
    #[test]
    fn the_record_tells_no_value_apart_from_a_value() {
        let entries = vec![
            (
                "np".to_string(),
                Entry {
                    prior: Some(Prior::Value(RegVal::sz(r"C:\Program Files\x"))),
                    wrote: Some(INSTALL_WIN_DIR.to_string()),
                },
            ),
            (
                "ft".to_string(),
                Entry {
                    prior: Some(Prior::Unset),
                    wrote: Some(INSTALL_WIN_DIR.to_string()),
                },
            ),
            (
                "xx".to_string(),
                Entry {
                    prior: Some(Prior::Value(RegVal {
                        ty: RegTy::ExpandSz,
                        value: r"%ProgramFiles%\opentrack".to_string(),
                    })),
                    wrote: None,
                },
            ),
        ];
        assert_eq!(parse_prior(&render_prior(&entries)), entries);
        // A damaged line is dropped, never guessed at: no record and an
        // unreadable one must both end in leaving the key alone.
        assert_eq!(
            parse_prior(
                "np prior value\nnp prior value REG_WAT x\nft wat\n# a comment\n\nnp prior unset\n"
            ),
            vec![(
                "np".to_string(),
                Entry {
                    prior: Some(Prior::Unset),
                    wrote: None
                }
            )]
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
    /// still recoverable: what it replaced is written down, with the type it
    /// was stored as.
    #[test]
    fn force_replaces_it_but_records_what_it_replaced() {
        let w = FakeWine::new("force");
        w.registered("np", r"Z:\usr\libexec\opentrack");
        w.registered("ft", r"C:\freetrack");
        install(&w.args("install", &["--force"])).expect("installs");
        let rec = w.record();
        assert!(
            rec.contains(r"np prior value REG_SZ Z:\usr\libexec\opentrack"),
            "{rec}"
        );
        assert!(rec.contains(r"ft prior value REG_SZ C:\freetrack"), "{rec}");
        // And what we put there, which is how uninstall knows the key is still
        // the one we left.
        assert!(rec.contains(r"np wrote C:\tobii-bridge"), "{rec}");
        let argv = w.argv();
        assert!(
            argv.contains(
                r"[add][HKCU\Software\Freetrack\FreeTrackClient][/v][Path][/t][REG_SZ][/d][C:\tobii-bridge][/f]"
            ),
            "{argv}"
        );
    }

    /// The other half of the record: an untouched prefix held nothing, and
    /// uninstall may then remove — which is only right because it was recorded.
    #[test]
    fn an_unregistered_prefix_is_recorded_as_having_held_nothing() {
        let w = FakeWine::new("absent");
        install(&w.args("install", &[])).expect("installs");
        let rec = w.record();
        assert!(rec.contains("np prior unset"), "{rec}");
        assert!(rec.contains("ft prior unset"), "{rec}");
    }

    /// The record answers "what did this prefix say before `tobii` ever wrote
    /// to it", so re-installing — which finds our own value there — must not
    /// overwrite the answer with our own value, and must not refuse either.
    #[test]
    fn a_second_install_neither_refuses_nor_overwrites_the_first_record() {
        let w = FakeWine::new("twice");
        w.registered("np", r"Z:\usr\libexec\opentrack");
        install(&w.args("install", &["--force"])).expect("first install");
        install(&w.args("install", &[])).expect("our own value is not someone else's");
        // Read back as entries and compared whole: a second entry for the same
        // key appended below the first would leave the text assertion happy
        // while the record had two answers to one question.
        assert_eq!(
            read_prior(&w.dest()),
            vec![
                (
                    "ft".to_string(),
                    Entry {
                        prior: Some(Prior::Unset),
                        wrote: Some(INSTALL_WIN_DIR.to_string()),
                    }
                ),
                (
                    "np".to_string(),
                    Entry {
                        prior: Some(Prior::Value(RegVal::sz(r"Z:\usr\libexec\opentrack"))),
                        wrote: Some(INSTALL_WIN_DIR.to_string()),
                    }
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
        install(&w.args("install", &["--force"])).expect("second install");
        assert_eq!(
            read_prior(&w.dest()).into_iter().find(|(n, _)| n == "np"),
            Some((
                "np".to_string(),
                Entry {
                    prior: Some(Prior::Value(RegVal::sz(r"Z:\usr\libexec\opentrack"))),
                    wrote: Some(INSTALL_WIN_DIR.to_string()),
                }
            ))
        );
    }

    /// A re-install that finds the very client it is about to register is the
    /// same situation as finding a stranger's: that value was there before this
    /// run, and if it is not written down, uninstall deletes a registration
    /// that was never ours — reached with no `--force` and no warning.
    #[test]
    fn reinstalling_while_another_program_owns_the_key_records_what_it_found() {
        let w = FakeWine::new("same");
        let dir = w.npclient_dir();
        let win = wine_path_for(&dir);
        install(&w.args("install", &[])).expect("first install");
        // opentrack is installed afterwards and registers its own client.
        w.registered("np", &win);
        // This run points TrackIR at exactly that client, so the key needs no
        // change — but what it held is still not ours.
        install(&w.args("install", &["--npclient", &dir.display().to_string()]))
            .expect("our own value is not someone else's");
        let rec = w.record();
        assert!(
            rec.contains(&format!("np prior value REG_SZ {win}")),
            "the value found must be recorded: {rec}"
        );
        w.forget_argv();
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(
            !argv.contains(r"[delete][HKCU\Software\NaturalPoint"),
            "opentrack's registration must survive: {argv}"
        );
        assert!(
            w.current("np").contains(&win),
            "and still be there afterwards: {}",
            w.current("np")
        );
    }

    /// An install that pointed TrackIR at a third-party client leaves a value
    /// that is neither our directory nor what the next run would write. Without
    /// the record it reads as another program's, and the installer accuses
    /// itself — then, forced, records its own value as the prior to restore, so
    /// uninstall CREATES a registration in a prefix that never had one.
    #[test]
    fn our_own_third_party_registration_is_not_another_programs() {
        let w = FakeWine::new("ourown");
        let dir = w.npclient_dir();
        let win = wine_path_for(&dir);
        install(&w.args("install", &["--npclient", &dir.display().to_string()]))
            .expect("first install");
        assert!(w.current("np").contains(&win), "{}", w.current("np"));
        // The same installer, now asked for its own DLL.
        install(&w.args("install", &["--npclient", "ours"]))
            .expect("must not refuse about a value it wrote itself");
        let rec = w.record();
        assert!(
            rec.contains("np prior unset"),
            "the prefix still held nothing before us: {rec}"
        );
        w.forget_argv();
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(
            !argv.contains("[add]"),
            "nothing may be created in a prefix that had nothing: {argv}"
        );
        assert!(
            argv.contains(
                r"[delete][HKCU\Software\NaturalPoint\NATURALPOINT\NPClient Location][/v][Path][/f]"
            ),
            "{argv}"
        );
    }

    /// Deleting leaves the prefix with NO client registered, which is worse
    /// than it was before we touched it. And the undo is the `Path` value we
    /// added, not the whole key, which may hold another program's settings.
    #[test]
    fn uninstall_puts_back_what_was_there_instead_of_deleting_it() {
        let w = FakeWine::new("restore");
        w.put_record(
            "np prior value REG_SZ Z:\\usr\\libexec\\opentrack\n\
             np wrote C:\\tobii-bridge\n\
             ft prior unset\n\
             ft wrote C:\\tobii-bridge\n",
        );
        w.registered("np", INSTALL_WIN_DIR);
        w.registered("ft", INSTALL_WIN_DIR);
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
        // Recorded as having held nothing, so removing our value is the
        // restoration — the value, not the key.
        assert!(
            argv.contains(r"[delete][HKCU\Software\Freetrack\FreeTrackClient][/v][Path][/f]"),
            "{argv}"
        );
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

    /// And the restoring arm of the same fault: putting the recorded value
    /// back over whatever holds the key today overwrites a registration made
    /// after ours just as blindly as deleting it.
    #[test]
    fn uninstall_does_not_overwrite_a_value_changed_since_our_install() {
        let w = FakeWine::new("changed");
        w.registered("np", r"Z:\usr\libexec\opentrack");
        install(&w.args("install", &["--force"])).expect("installs");
        // The user switches trackers; the new one claims the key.
        w.registered("np", r"C:\brand-new-tracker");
        w.forget_argv();
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(
            !argv.contains("[add]"),
            "the stale recorded value must not be written back: {argv}"
        );
        assert!(
            w.current("np").contains(r"C:\brand-new-tracker"),
            "{}",
            w.current("np")
        );
    }

    /// A value stored as `REG_EXPAND_SZ` is a registration like any other: it
    /// must be seen, refused over, and — once forced — put back as the type it
    /// was, since the same string as `REG_SZ` sends the game looking for a
    /// directory spelled with literal percent signs.
    #[test]
    fn an_expandable_registration_is_refused_and_restored_as_itself() {
        let w = FakeWine::new("expand");
        w.registered_as("np", "REG_EXPAND_SZ", r"%ProgramFiles%\opentrack");
        let err = install(&w.args("install", &[]))
            .expect_err("must refuse")
            .to_string();
        assert!(err.contains(r"%ProgramFiles%\opentrack"), "{err}");
        install(&w.args("install", &["--force"])).expect("installs");
        let rec = w.record();
        assert!(
            rec.contains(r"np prior value REG_EXPAND_SZ %ProgramFiles%\opentrack"),
            "{rec}"
        );
        w.forget_argv();
        uninstall(&w.args("uninstall", &[])).expect("uninstalls");
        let argv = w.argv();
        assert!(
            argv.contains(
                r"[add][HKCU\Software\NaturalPoint\NATURALPOINT\NPClient Location][/v][Path][/t][REG_EXPAND_SZ][/d][%ProgramFiles%\opentrack][/f]"
            ),
            "{argv}"
        );
    }

    /// Wine prints the registry in the console's OEM codepage. A value we
    /// cannot read is not one we can promise to put back, so it is refused and
    /// said out loud — `--force` included, because forcing would destroy a
    /// working registration and record a mangled string in its place.
    #[test]
    fn a_value_that_cannot_be_read_is_refused_even_with_force() {
        let w = FakeWine::new("codepage");
        let mut reply: Vec<u8> =
            b"\r\nHKEY_CURRENT_USER\\Whatever\r\n    Path    REG_SZ    C:\\Program Files\\M"
                .to_vec();
        reply.push(0x81);
        reply.extend_from_slice(b"ller opentrack\r\n\r\n");
        w.registered_bytes("np", &reply);
        for extra in [vec![], vec!["--force"]] {
            let err = install(&w.args("install", &extra))
                .expect_err("must refuse")
                .to_string();
            assert!(err.contains("cannot read"), "{err}");
            assert!(
                err.contains(r"HKCU\Software\NaturalPoint"),
                "must name the key: {err}"
            );
        }
        assert!(!w.argv().contains("[add]"), "{}", w.argv());
        assert!(!w.dest().exists(), "and nothing copied");
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

    /// The record is the only copy of what is left to put back, and it lives
    /// in the directory being removed. A restore that did not land must leave
    /// both where they are, and must not exit 0 — a script that runs
    /// `tobii bridge uninstall && rm -rf "$prefix"` would otherwise proceed.
    #[test]
    fn a_failed_restore_keeps_the_record_and_fails_the_command() {
        let w = FakeWine::new("failrestore");
        w.put_record(
            "np prior value REG_SZ Z:\\usr\\libexec\\opentrack\n\
             np wrote C:\\tobii-bridge\n",
        );
        w.registered("np", INSTALL_WIN_DIR);
        w.fail_writes();
        let err = uninstall(&w.args("uninstall", &[]))
            .expect_err("a failed restore is a failed uninstall")
            .to_string();
        assert!(err.contains(PRIOR_FILE), "must name the record: {err}");
        assert!(w.dest().exists(), "the directory must stay");
        assert_eq!(
            read_prior(&w.dest())
                .into_iter()
                .find(|(n, _)| n == "np")
                .and_then(|(_, e)| e.prior),
            Some(Prior::Value(RegVal::sz(r"Z:\usr\libexec\opentrack"))),
            "and the value to put back must still be readable"
        );
    }

    /// A record that cannot be read says nothing about what the keys held —
    /// and the next install must not fill it in from a registry it wrote
    /// itself, which is how "there was nothing" gets asserted about a key whose
    /// earlier value is genuinely unknown, and deleted on the way out.
    #[test]
    fn a_damaged_record_is_not_rewritten_as_there_was_nothing() {
        let w = FakeWine::new("damaged");
        install(&w.args("install", &[])).expect("first install");
        // A crash mid-write leaves a record with the prior lines gone.
        w.put_record("np wrote C:\\tobii-bridge\n");
        install(&w.args("install", &[])).expect("second install");
        // Read back as entries, because the file's own header names the
        // spellings and would answer a text assertion for them.
        assert!(
            read_prior(&w.dest()).iter().all(|(_, e)| e.prior.is_none()),
            "what these keys held is unknown, and must not be invented: {}",
            w.record()
        );
    }

    /// `reg query` fails both when nothing is registered and when wine cannot
    /// serve the prefix at all, and its message is localized. Reading the
    /// second as the first walks straight into overwriting whatever is there
    /// and recording that there was nothing.
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
