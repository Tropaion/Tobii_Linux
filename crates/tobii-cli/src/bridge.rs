//! `tobii bridge` — put the Wine-side bridge into a game's prefix and run it.
//!
//! # The thing that is easy to get wrong
//!
//! **Which `wine` binary.** A Wine prefix is served by a `wineserver`, and named
//! kernel objects like `FT_SharedMem` live inside one server's session. Launch
//! the bridge with `/usr/bin/wine` against a prefix whose game runs under a
//! bundled tkg runner and you get a *second* wineserver: the bridge starts, says
//! everything worked, creates its mapping — and the game never sees it, because
//! it is looking at a different session's namespace.
//!
//! Nothing about that failure points at its cause. So the wine binary is
//! resolved from the prefix itself wherever possible, and a mismatch is a loud
//! warning rather than a silent success.

use std::path::{Path, PathBuf};

use crate::CmdResult;

/// Where the artifacts are installed inside the prefix.
const INSTALL_SUBDIR: &str = "drive_c/tobii-bridge";

/// The Windows spelling of the same directory.
const INSTALL_WIN_DIR: &str = r"C:\tobii-bridge";

/// Registry key a TrackIR game reads to find its client DLL.
const NP_KEY: &str = r"HKCU\Software\NaturalPoint\NATURALPOINT\NPClient Location";

/// Registry key a FreeTrack game reads to find its client DLL.
const FT_KEY: &str = r"HKCU\Software\Freetrack\FreeTrackClient";

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

/// Artifacts copied into the prefix. Missing optional ones are skipped, so the
/// bridge is usable before every DLL exists.
const ARTIFACTS: [(&str, bool); 3] = [
    ("tobii-bridge.exe", true),
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

/// Resolve the prefix: `--prefix`, else `$WINEPREFIX`, else `~/.wine`.
fn resolve_prefix(args: &[String]) -> Result<PathBuf, String> {
    let raw = crate::flag_value(args, "--prefix")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("WINEPREFIX").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".wine")))
        .ok_or("could not determine a Wine prefix; pass --prefix PATH")?;
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
    let (wine, warning) = choose_wine(
        crate::flag_value(args, "--wine").map(PathBuf::from),
        wine_from_launch_script(prefix),
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
    for c in &candidates {
        if c.join("tobii-bridge.exe").is_file() {
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

/// `tobii bridge install` — copy the artifacts in and register them.
fn install(args: &[String]) -> CmdResult {
    let prefix = resolve_prefix(args)?;
    let wine = resolve_wine(&prefix, args)?;
    let src = artifact_dir(args)?;
    let dest = prefix.join(INSTALL_SUBDIR);
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
        std::fs::copy(&from, dest.join(name))?;
        copied += 1;
        println!("  {name}");
    }
    println!("copied {copied} file(s) into {}", dest.display());

    // FreeTrack always gets our own DLL: that ABI has no signature check, so
    // nothing stands between it and our data.
    set_key(&wine, &prefix, FT_KEY, INSTALL_WIN_DIR)?;

    let explicit_np = crate::flag_value(args, "--npclient");
    match choose_npclient(explicit_np, find_installed_npclient())? {
        NpSource::Ours => {
            set_key(&wine, &prefix, NP_KEY, INSTALL_WIN_DIR)?;
            println!("registered {INSTALL_WIN_DIR} for TrackIR and FreeTrack");
            if explicit_np != Some("ours") {
                println!(
                    "\nnote: no third-party NPClient64.dll was found. Games that verify\n      \
                     NaturalPoint's signature — Star Citizen among them — will load our\n      \
                     DLL, reject it, and never ask for data again. Installing opentrack\n      \
                     provides a client that passes; our provider still supplies the data."
                );
            }
        }
        NpSource::Installed(dir) => {
            let win = wine_path_for(&dir);
            set_key(&wine, &prefix, NP_KEY, &win)?;
            println!("registered {INSTALL_WIN_DIR} for FreeTrack");
            println!("registered {win} for TrackIR");
            println!(
                "\nTrackIR points at the client already installed there, because games\n\
                 verify NaturalPoint's signature and a clean-room DLL cannot answer it.\n\
                 Nothing was copied; our provider still supplies the data behind it.\n\
                 Override with `--npclient ours`."
            );
        }
    }
    println!("\nnow run:  tobii bridge run --prefix {}", prefix.display());
    Ok(())
}

/// Point one discovery key at `dir`.
fn set_key(wine: &Path, prefix: &Path, key: &str, dir: &str) -> CmdResult {
    let status = wine_run(
        wine,
        prefix,
        &[
            "reg", "add", key, "/v", "Path", "/t", "REG_SZ", "/d", dir, "/f",
        ],
    )?;
    if !status.success() {
        eprintln!("warning: could not write {key}");
    }
    Ok(())
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

/// `tobii bridge uninstall` — remove the keys and the directory.
fn uninstall(args: &[String]) -> CmdResult {
    let prefix = resolve_prefix(args)?;
    let wine = resolve_wine(&prefix, args)?;
    for key in [NP_KEY, FT_KEY] {
        let _ = wine_run(&wine, &prefix, &["reg", "delete", key, "/f"]);
    }
    let dir = prefix.join(INSTALL_SUBDIR);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)?;
        println!("removed {}", dir.display());
    }
    println!("unregistered TrackIR and FreeTrack client paths");
    Ok(())
}

/// Dispatch `tobii bridge ...`.
pub fn bridge(args: &[String]) -> CmdResult {
    match args.get(2).map(String::as_str) {
        Some("install") => install(args),
        Some("run") => run(args),
        Some("uninstall") => uninstall(args),
        other => Err(format!(
            "usage: tobii bridge install|run|uninstall [--prefix PATH] [--wine PATH] \
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

    /// NPClient64.dll does not exist until spike S2 has run, and the bridge is
    /// useful without it — FreeTrack games work either way.
    #[test]
    fn only_the_provider_and_the_freetrack_dll_are_required() {
        let required: Vec<&str> = ARTIFACTS
            .iter()
            .filter(|(_, req)| *req)
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(required, vec!["tobii-bridge.exe", "freetrackclient64.dll"]);
        assert!(ARTIFACTS
            .iter()
            .any(|(n, req)| *n == "NPClient64.dll" && !req));
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
}
