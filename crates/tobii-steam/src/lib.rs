//! Reading Steam's own files: which libraries exist, what is installed in
//! them, and where a title's Proton prefix is.
//!
//! This lives apart from `tobii-cli` for one reason: `tobii-cli` is a
//! `[[bin]]` with no `[lib]`, so everything in it is unreachable from the GTK
//! hub. The hub needs to offer the user a list of installed games, and that
//! list is built here.
//!
//! # What this does not do
//!
//! It does not parse VDF. `libraryfolders.vdf` and an `appmanifest_*.acf` are
//! both Valve's key-value format, and both are read by pulling out the three
//! or four keys that matter. A real parser would be a dependency and a
//! maintenance burden for three strings, and the strings have not changed in
//! the lifetime of this project.
//!
//! It also never writes. Nothing here creates, modifies or deletes anything —
//! a wrong answer here should cost the user a confusing list, never a file.

use std::path::{Path, PathBuf};

/// Where Steam installs itself. All four are checked because a machine can
/// have more than one and they are not interchangeable: `.steam/steam` and
/// `.steam/root` are usually symlinks to the same place, and the Flatpak keeps
/// its own.
const STEAM_ROOTS: [&str; 4] = [
    ".steam/steam",
    ".local/share/Steam",
    ".steam/root",
    ".var/app/com.valvesoftware.Steam/data/Steam",
];

/// One installed Steam application.
///
/// "Application", not "game": Proton builds and the Steam runtimes are
/// installed exactly like titles are, and Steam's own files do not distinguish
/// them. See [`looks_like_tool`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct App {
    pub appid: String,
    pub name: String,
    /// The build Steam last installed, if the manifest says. Worth keeping
    /// because a per-game setting recorded against a build is a setting whose
    /// staleness can be noticed later; nothing here uses it yet.
    pub buildid: Option<String>,
}

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
pub fn libraries(home: &Path) -> Vec<PathBuf> {
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

/// Everything installed, across every library.
pub fn apps(home: &Path) -> Vec<App> {
    let mut out = Vec::new();
    for lib in libraries(home) {
        let Ok(dir) = std::fs::read_dir(lib.join("steamapps")) else {
            continue;
        };
        for entry in dir.flatten() {
            let file = entry.file_name();
            let file = file.to_string_lossy();
            if !file.starts_with("appmanifest_") || !file.ends_with(".acf") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let field = |key: &str| text.lines().find_map(|l| vdf_value(l, key));
            if let (Some(appid), Some(name)) = (field("appid"), field("name")) {
                out.push(App {
                    appid: appid.to_string(),
                    name: name.to_string(),
                    buildid: field("buildid").map(str::to_string),
                });
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
/// launches. Its absence is the commonest reason this fails and is worth
/// saying out loud rather than reporting as "not found".
pub fn prefix(home: &Path, appid: &str) -> Option<PathBuf> {
    let libs = libraries(home);
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

/// Whether an installed application is Steam's own plumbing rather than
/// something a user would configure head tracking for.
///
/// **This is a heuristic on the name, and it is one because nothing better
/// exists.** Two things were checked against a real library of 28 applications
/// before settling for it:
///
/// * No manifest field distinguishes a tool. `SharedDepots` looked promising
///   and is not: it is present on some games, absent on others, and absent on
///   Proton.
/// * "Has its own Proton prefix" does not either — Proton 9.0 and Proton 10.0
///   both have one.
///
/// So a caller that offers the user a list should filter with this and still
/// let them reach everything, because a heuristic that hides the game somebody
/// wanted is worse than a list with Proton in it.
pub fn looks_like_tool(name: &str) -> bool {
    const MARKS: [&str; 4] = [
        "Proton",
        "Steam Linux Runtime",
        "Steamworks Common Redistributables",
        "Steam Runtime",
    ];
    // The mark has to end where a word ends. `starts_with` alone called
    // Protonaut — a real game — Steam plumbing, which is exactly the failure
    // this heuristic must not have: hiding the game somebody was looking for.
    MARKS.iter().any(|m| {
        name.strip_prefix(m)
            .is_some_and(|rest| rest.is_empty() || !rest.starts_with(|c: char| c.is_alphanumeric()))
    })
}

/// What `--steam <appid or name>` picked out.
///
/// The decision, separated from how any one caller words it. `tobii bridge`
/// prints these as errors with a list of installed games; the hub will show
/// them as a picker. Neither wording belongs in the deciding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// Nothing matched. Carries everything installed, because every caller
    /// wants to show the user what there was.
    None,
    /// Exactly one, by app id or by name.
    One(App),
    /// A name fragment matching several. Carries them in the order they were
    /// given, so a caller can list them.
    Many(Vec<App>),
}

/// Resolve `wanted` against an already-read list.
///
/// Pure, and takes the apps rather than reading them, so the decision is
/// testable without a Steam install and a caller that has already listed them
/// does not read the disk twice.
///
/// An all-digits `wanted` is an app id and is matched exactly; anything else
/// is matched case-insensitively as a substring, because nobody types
/// "Elite Dangerous" with the right capitalisation twice.
pub fn resolve(apps: &[App], wanted: &str) -> Match {
    if !wanted.is_empty() && wanted.chars().all(|c| c.is_ascii_digit()) {
        return match apps.iter().find(|a| a.appid == wanted) {
            Some(a) => Match::One(a.clone()),
            None => Match::None,
        };
    }
    let needle = wanted.to_lowercase();
    let hits: Vec<App> = apps
        .iter()
        .filter(|a| a.name.to_lowercase().contains(&needle))
        .cloned()
        .collect();
    match hits.len() {
        0 => Match::None,
        1 => Match::One(hits.into_iter().next().expect("one")),
        _ => Match::Many(hits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A throwaway directory that is this process's alone.
    ///
    /// No `$HOME` is read and no fixed path is used: CI runs these as root, and
    /// a test that reaches for the real home would find a different one there.
    fn scratch(what: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "tobii-steam-{}-{}-{what}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("scratch");
        p
    }

    /// Write a library: a `steamapps` directory, and whatever manifests.
    fn library(root: &Path, apps: &[(&str, &str, Option<&str>)]) {
        std::fs::create_dir_all(root.join("steamapps")).expect("steamapps");
        for (id, name, build) in apps {
            let build = build
                .map(|b| format!("\t\"buildid\"\t\t\"{b}\"\n"))
                .unwrap_or_default();
            std::fs::write(
                root.join(format!("steamapps/appmanifest_{id}.acf")),
                format!("\"AppState\"\n{{\n\t\"appid\"\t\t\"{id}\"\n\t\"name\"\t\t\"{name}\"\n{build}}}\n"),
            )
            .expect("manifest");
        }
    }

    fn app(id: &str, name: &str) -> App {
        App {
            appid: id.into(),
            name: name.into(),
            buildid: None,
        }
    }

    /// The bug this guards is in the comment on `libraries`: an early `continue`
    /// when `libraryfolders.vdf` could not be read skipped the root fallback
    /// below it, so a Steam install with a missing or unreadable file reported
    /// NO libraries at all — on a machine with games plainly installed.
    #[test]
    fn a_root_is_a_library_even_with_no_libraryfolders_file() {
        let home = scratch("novdf");
        library(&home.join(".steam/steam"), &[("1", "A Game", None)]);
        let libs = libraries(&home);
        assert_eq!(libs.len(), 1, "the root itself is a library: {libs:?}");
        assert_eq!(apps(&home), vec![app("1", "A Game")]);
    }

    /// `~/.steam/steam` and `~/.steam/root` are both symlinks to the same
    /// install on an ordinary machine. Reported three times, it was installed
    /// into three times.
    #[test]
    fn one_library_reached_by_three_names_is_listed_once() {
        let home = scratch("symlink");
        let real = home.join(".local/share/Steam");
        library(&real, &[("1", "A Game", None)]);
        std::fs::create_dir_all(home.join(".steam")).expect("dotsteam");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real, home.join(".steam/steam")).expect("link");
            std::os::unix::fs::symlink(&real, home.join(".steam/root")).expect("link");
        }
        assert_eq!(libraries(&home).len(), 1, "{:?}", libraries(&home));
    }

    #[test]
    fn a_library_named_by_the_vdf_is_found_and_its_apps_read() {
        let home = scratch("vdf");
        let root = home.join(".steam/steam");
        let other = home.join("games/library-two");
        library(&root, &[("1", "First", Some("111"))]);
        library(&other, &[("2", "Second", None)]);
        std::fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                other.display()
            ),
        )
        .expect("vdf");
        let found = apps(&home);
        assert_eq!(
            found,
            vec![
                App {
                    appid: "1".into(),
                    name: "First".into(),
                    buildid: Some("111".into())
                },
                app("2", "Second"),
            ],
            "both libraries, sorted, with the buildid the manifest gave"
        );
    }

    /// Steam's "Move Install Folder" does not move `compatdata`, so a moved
    /// game leaves its old prefix behind and gets a fresh one. Installing into
    /// the abandoned one succeeds, says so, and does nothing for the game — so
    /// the library holding the manifest is asked FIRST, not whichever library
    /// happens to come first.
    #[test]
    fn the_library_that_owns_the_manifest_is_asked_before_the_others() {
        let home = scratch("moved");
        let root = home.join(".steam/steam");
        let elsewhere = home.join("games/second");
        library(&root, &[]);
        library(&elsewhere, &[("42", "Moved Game", None)]);
        std::fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            format!("\t\"path\"\t\t\"{}\"\n", elsewhere.display()),
        )
        .expect("vdf");
        // The abandoned prefix, in the library that does NOT hold the manifest.
        for lib in [&root, &elsewhere] {
            std::fs::create_dir_all(lib.join("steamapps/compatdata/42/pfx/drive_c")).expect("pfx");
        }
        let got = prefix(&home, "42").expect("a prefix");
        assert!(
            got.starts_with(elsewhere.canonicalize().expect("real")),
            "the manifest's own library wins; got {got:?}"
        );
    }

    #[test]
    fn a_game_that_has_never_been_launched_has_no_prefix() {
        let home = scratch("neverrun");
        library(&home.join(".steam/steam"), &[("7", "Unlaunched", None)]);
        assert_eq!(prefix(&home, "7"), None, "Proton makes it on the first run");
    }

    #[test]
    fn a_manifest_without_the_keys_that_matter_is_skipped_not_guessed_at() {
        let home = scratch("broken");
        let root = home.join(".steam/steam");
        library(&root, &[("1", "Fine", None)]);
        std::fs::write(
            root.join("steamapps/appmanifest_2.acf"),
            "\"AppState\"\n{\n}\n",
        )
        .expect("headless manifest");
        std::fs::write(
            root.join("steamapps/appmanifest_3.acf"),
            &[0xff, 0xfe, 0x00][..],
        )
        .expect("not utf-8");
        assert_eq!(
            apps(&home),
            vec![app("1", "Fine")],
            "the readable one survives"
        );
    }

    #[test]
    fn a_value_is_read_out_of_valves_spacing_whatever_it_is() {
        assert_eq!(
            vdf_value("\t\"name\"\t\t\"A Game\"", "name"),
            Some("A Game")
        );
        assert_eq!(
            vdf_value("  \"name\"   \"A Game\"  ", "name"),
            Some("A Game")
        );
        assert_eq!(vdf_value("\"name\"\"A Game\"", "name"), Some("A Game"));
        assert_eq!(
            vdf_value("\t\"name\"\t\t\"\"", "name"),
            Some(""),
            "an empty value is a value"
        );
        // The key is matched whole: `nameid` is not `name`.
        assert_eq!(vdf_value("\t\"nameid\"\t\"7\"", "name"), None);
        assert_eq!(vdf_value("\t\"appid\"\t\"7\"", "name"), None);
        assert_eq!(vdf_value("not a vdf line at all", "name"), None);
    }

    #[test]
    fn an_app_id_is_matched_exactly_and_a_name_loosely() {
        let apps = vec![
            app("42", "Elite Dangerous"),
            app("7", "Elite Dangerous Odyssey"),
        ];
        assert_eq!(
            resolve(&apps, "42"),
            Match::One(apps[0].clone()),
            "digits are an app id"
        );
        assert_eq!(
            resolve(&apps, "99"),
            Match::None,
            "an app id that is not installed"
        );
        assert_eq!(
            resolve(&apps, "odyssey"),
            Match::One(apps[1].clone()),
            "nobody types the capitalisation twice"
        );
        assert_eq!(
            resolve(&apps, "elite"),
            Match::Many(apps.clone()),
            "ambiguous lists them"
        );
        assert_eq!(resolve(&apps, "no such game"), Match::None);
    }

    /// An empty `--steam` value is all-digits vacuously, and taking that branch
    /// would match an app whose id is the empty string — which is to say,
    /// whatever `find` happened on.
    #[test]
    fn an_empty_name_is_not_an_app_id() {
        let apps = vec![app("42", "Elite Dangerous")];
        assert_eq!(
            resolve(&apps, ""),
            Match::Many(apps.clone()).into_one_or(&apps)
        );
    }

    #[test]
    fn steams_own_plumbing_is_recognised_as_such() {
        for tool in [
            "Proton 10.0",
            "Proton Experimental",
            "Proton EasyAntiCheat Runtime",
            "Steam Linux Runtime 3.0 (sniper)",
            "Steamworks Common Redistributables",
        ] {
            assert!(looks_like_tool(tool), "{tool}");
        }
        for game in [
            "Elite Dangerous",
            "Warframe",
            "The First Descendant",
            "Protonaut",
        ] {
            assert!(!looks_like_tool(game), "{game}");
        }
    }

    impl Match {
        /// Test helper: an empty query matches everything, which is `Many`
        /// unless there is exactly one app installed.
        fn into_one_or(self, all: &[App]) -> Match {
            if all.len() == 1 {
                Match::One(all[0].clone())
            } else {
                self
            }
        }
    }
}
