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
//! both Valve's key-value format, and both are read by pulling out the four
//! keys that matter: `path` from the one, and `appid`, `name` and `buildid`
//! from the other. A real parser would be a dependency and a maintenance
//! burden for four strings, and the strings have not changed in the lifetime
//! of this project.
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
///
/// # Identity
///
/// Two `App`s are the same application when their `appid` matches. Neither the
/// build a copy sits at nor the name a manifest gives it is part of who it is:
/// `Eq`, `Ord` and `Hash` compare the app id and nothing else.
///
/// That matters because a machine can hold one title in two libraries — a
/// stale `appmanifest` left behind by a manual copy, or native Steam and the
/// Flatpak each holding the title — and the two copies can disagree about
/// either of the other two fields: the build when one library is a patch
/// behind, the name when Steam has renamed the title since the stale copy was
/// written. An identity that counted either one made those copies two
/// applications: `tobii bridge games` printed the row twice, and [`resolve`]
/// answered [`Match::Many`] for a name that matches one game, so its caller
/// told the user to pass the app id above a list naming that one app id on
/// both of its lines.
///
/// Passing it did work — an all-digits `--steam` value is used verbatim and
/// never reaches `resolve` — so the damage was not a dead end. It was a
/// question the user could not answer: a message whose whole job is to say
/// which of these to name replied with one id twice.
///
/// The app id is also all the identity any other answer here needs. [`prefix`]
/// looks a prefix up by app id and never reads a name, so two rows sharing an
/// app id are one game for every purpose except being shown to somebody.
#[derive(Debug, Clone)]
pub struct App {
    pub appid: String,
    /// What to call this title: every distinct name this machine's manifests
    /// give for the app id, in alphabetical order, joined by ` / `.
    ///
    /// Which is to say the one name they all give, except when copies
    /// disagree. Then nothing here can say which name is the true one, any
    /// more than it can say which of two builds is — but unlike the build
    /// there is no honest way to answer "none", because a row has to be called
    /// something before a list or a picker can show it.
    ///
    /// So both are kept, and the two things that follow are the reason rather
    /// than the taste. [`resolve`] matches a name fragment as a substring, so
    /// a row carrying both names is still reachable by either; picking one
    /// would make a way of typing the game that worked before the copies were
    /// collapsed match nothing at all. And alphabetical order, not the order
    /// the libraries happened to be scanned in, is what makes the row read the
    /// same on two runs on a machine nobody touched.
    pub name: String,
    /// The build every copy of this title on the machine agrees it is at.
    ///
    /// `None` says no build can be named — either no manifest gave one, or two
    /// libraries hold the title at different builds and there is no honest
    /// single answer. The two are not told apart because nothing can act on
    /// the difference: the reason to keep the field at all is that a per-game
    /// setting recorded against a build is one whose staleness can be noticed
    /// later, and "unknown" and "ambiguous" both mean staleness cannot be
    /// judged. No caller reads it yet; the collapse in [`apps`] is the only
    /// code that touches it.
    pub buildid: Option<String>,
}

impl App {
    /// What the comparisons below agree to compare — see the identity section
    /// on [`App`] for why neither the name nor the build is in it.
    ///
    /// It is one function rather than three copies of `&self.appid` so that
    /// `Eq`, `Ord` and `Hash` cannot drift apart. `Hash` counting a field
    /// `Eq` does not is the listing bug one layer down — the same game in two
    /// buckets of a `HashSet` — and nothing would say a word about it.
    ///
    /// The collapses in [`apps`] do not go through here. They compare the
    /// name as well, on purpose: two manifests under one name are one row
    /// outright, and two under different names are one row carrying both.
    /// Identity is what every *other* caller inherits, and it is the app id.
    fn identity(&self) -> &str {
        &self.appid
    }
}

impl PartialEq for App {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl Eq for App {}

impl PartialOrd for App {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for App {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.identity().cmp(other.identity())
    }
}

/// `Hash` exists, and agrees with `Eq`.
///
/// Nothing outside this file's own tests hashes an `App`. It is written anyway
/// because the contract is that equal values hash equally: a derived `Hash`
/// would hash the name and the build too, so two copies of one title would
/// compare equal and land in different buckets — a `HashSet` holding the same
/// game twice, which is the listing bug again one layer down. Writing it out
/// rather than deriving it also makes a later `#[derive(Hash)]` a
/// duplicate-impl compile error.
impl std::hash::Hash for App {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.identity().hash(state);
    }
}

/// Add `path` if it is a library and not already listed, and answer whether
/// it is a library at all.
///
/// The answer is not "was it added": a library already listed is still a
/// library. [`scan`] sorts the paths `libraryfolders.vdf` names by it, and a
/// library named by two of the roots would otherwise be counted, the second
/// time, as one that is not there.
///
/// Canonicalised before it is compared with what is already listed:
/// `~/.steam/steam` and `~/.steam/root` are both symlinks to the real install,
/// so a plain path comparison reports the same library three times and would
/// then install into it three times.
fn push_library(out: &mut Vec<PathBuf>, path: PathBuf) -> bool {
    if !path.join("steamapps").is_dir() {
        return false;
    }
    let real = path.canonicalize().unwrap_or(path);
    if !out.contains(&real) {
        out.push(real);
    }
    true
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

/// Every Steam root's `libraryfolders.vdf`, read once, sorting what those
/// files name into the libraries that are on this machine and the ones that
/// are not.
///
/// One walk behind both [`libraries`] and [`missing_libraries`], so the two
/// can never disagree about a path. A message naming a library as absent,
/// printed beside a list that shows that library, is worse than either answer
/// given on its own.
fn scan(home: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut found = Vec::new();
    let mut missing = Vec::new();
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
                let path = PathBuf::from(p.replace("\\\\", "/"));
                // Kept exactly as the file spells it. There is nothing to
                // canonicalise against, and the path a user has to recognise
                // as their own unplugged drive is the one Steam wrote down.
                if !push_library(&mut found, path.clone()) && !missing.contains(&path) {
                    missing.push(path);
                }
            }
        }
        // The root is a library itself even when the file does not say so. A
        // root that is not there is not reported missing: three of these four
        // are absent on an ordinary machine, and naming them would bury the
        // one absence that means something.
        push_library(&mut found, root);
    }
    (found, missing)
}

/// This machine's Steam install: the libraries that are here, the ones
/// `libraryfolders.vdf` names and are not, and everything that can be asked
/// about either.
///
/// One value, because [`scan`] is one walk. Every question below is answered
/// out of the walk taken when this was built, so asking a second question
/// costs no disk at all. The free functions further down each take that walk
/// for themselves, which is right for a caller with exactly one question and
/// wrong for every caller with two: the hub's game picker asked [`prefix`] of
/// 29 installed titles, which came to 31 walks and 124 reads of a
/// `libraryfolders.vdf` to open one window, and is 2 and 8 now. One of the
/// maintainer's libraries is on a drive that is not plugged in, and 93 of
/// those 124 reads were followed by a stat of that path; 6 are now. Whether
/// such a stat is slow is not something this machine can say: that path's
/// parent does not exist either, so every one of them returns ENOENT at once.
/// The count fell; no latency was measured.
///
/// # What it is a snapshot of
///
/// The library layout, and only that. [`Self::apps`] re-reads the manifests
/// on each call, so a game installed while this value is alive appears in the
/// next list rather than being hidden by a stale one. What is fixed is which
/// libraries were looked in.
///
/// # The pairing
///
/// [`Self::apps`] is what could be read and [`Self::missing_libraries`] is
/// what could not, and a caller that shows the first without the second has
/// told the user a confident negative about a game installed on a drive
/// nobody has plugged in. Taking both off one value is what makes them unable
/// to name different libraries — a list that shows a library beside a
/// sentence calling that library absent is worse than either answer alone.
#[derive(Debug)]
pub struct Steam {
    libraries: Vec<PathBuf>,
    missing: Vec<PathBuf>,
}

impl Steam {
    /// Walk `home`'s Steam roots. The only disk read in here that is not
    /// repeatable on demand, and the reason this type exists.
    ///
    /// `home` is a parameter rather than `$HOME` because CI runs as root with
    /// no Steam install, and because one window should be one machine's worth
    /// of answers even when the environment changes under it.
    pub fn at(home: &Path) -> Self {
        let (libraries, missing) = scan(home);
        Self { libraries, missing }
    }

    /// Every Steam library on this machine, in the order the library files
    /// name them.
    ///
    /// Not every library Steam knows about. A path `libraryfolders.vdf` names
    /// that is not a library here — an external drive nobody has plugged in is
    /// the ordinary way — is left out, because there is nothing to read there.
    /// [`Self::missing_libraries`] names those, and a caller whose answer would
    /// otherwise read as the whole picture of what is installed owes the user
    /// that list.
    pub fn libraries(&self) -> &[PathBuf] {
        &self.libraries
    }

    /// The libraries `libraryfolders.vdf` names that are not on this machine,
    /// spelled as that file spells them.
    ///
    /// Steam records a library's path, not whether its drive is plugged in, so
    /// a title installed on an external drive stays in that file after the
    /// drive is gone. [`Self::apps`] cannot see it, [`resolve`] answers
    /// [`Match::None`] for its name, and a caller that reports that as "no such
    /// game is installed" has told the user a confident negative about a game
    /// that is installed. This is what such a caller shows instead of guessing.
    ///
    /// **It is not every reason a title can be absent from [`Self::apps`].** A
    /// library that IS here but whose `steamapps` directory cannot be read, and
    /// a manifest that cannot be read or is not UTF-8, are each skipped by
    /// [`Self::apps`] and neither appears here. This answers one question —
    /// which of the paths in `libraryfolders.vdf` are not there — and a caller
    /// should word it as that, rather than as a complete account of what was
    /// missed.
    pub fn missing_libraries(&self) -> &[PathBuf] {
        &self.missing
    }

    /// Everything installed, across every library.
    ///
    /// The manifests are read here and not at [`Self::at`]: a caller that only
    /// wants a prefix should not pay for a `read_dir` of every library, and a
    /// caller that wants the list twice wants the second one to be current.
    pub fn apps(&self) -> Vec<App> {
        apps_in(&self.libraries)
    }

    /// The Proton prefix for `appid`, if the game has ever been run.
    ///
    /// Proton creates `steamapps/compatdata/<appid>/pfx` the first time a
    /// title launches. Its absence is the commonest reason this fails and is
    /// worth saying out loud rather than reporting as "not found".
    ///
    /// The library holding the manifest is asked first, not just whichever
    /// library happens to come first. Steam's "Move Install Folder" does not
    /// move `compatdata`, so a game moved between libraries leaves its old
    /// prefix behind and gets a fresh one on the next run — and installing
    /// into the abandoned one succeeds, prints the ordinary success text, and
    /// does nothing at all for the game. [`Self::prefixes`] is how a caller
    /// sees that there were others.
    pub fn prefix(&self, appid: &str) -> Option<PathBuf> {
        self.prefix_candidates(appid)
            .find(|p| p.join("drive_c").is_dir())
    }

    /// Every Proton prefix this machine holds for `appid`, the one
    /// [`Self::prefix`] picks first and the abandoned ones after it.
    ///
    /// Nothing else this crate answers hints that a second one exists:
    /// [`Self::apps`] folds two copies of a title into one row, and
    /// [`Self::prefix`] names one path with no sign that it chose. A caller
    /// that offers to install into a prefix is the one caller that has to be
    /// able to say there is more than one, and it used to have to rebuild the
    /// `compatdata/<appid>/pfx` shape out of [`Self::libraries`] itself — a
    /// second copy of this path, in another crate, that no test here could
    /// hold to this one.
    ///
    /// Empty when the title has never been run, which is the same answer
    /// [`Self::prefix`] gives as [`None`].
    pub fn prefixes(&self, appid: &str) -> Vec<PathBuf> {
        self.prefix_candidates(appid)
            .filter(|p| p.join("drive_c").is_dir())
            .collect()
    }

    /// Where a prefix for `appid` could be, in the order a caller should
    /// prefer them: the library holding the manifest, then the rest in library
    /// order. Nothing is stat'd here — the two callers above differ only in
    /// how much of this they take.
    fn prefix_candidates<'a>(&'a self, appid: &'a str) -> impl Iterator<Item = PathBuf> + 'a {
        let owner = self.libraries.iter().position(|lib| {
            lib.join(format!("steamapps/appmanifest_{appid}.acf"))
                .is_file()
        });
        owner
            .into_iter()
            .chain((0..self.libraries.len()).filter(move |i| Some(*i) != owner))
            .map(move |i| {
                self.libraries[i]
                    .join("steamapps/compatdata")
                    .join(appid)
                    .join("pfx")
            })
    }
}

/// Every Steam library on this machine.
///
/// [`Steam::at`] plus one question. A caller that asks a second one should
/// hold the [`Steam`] instead: this walks every root's `libraryfolders.vdf`
/// again, and so does every other free function here.
pub fn libraries(home: &Path) -> Vec<PathBuf> {
    Steam::at(home).libraries
}

/// The libraries `libraryfolders.vdf` names that are not on this machine.
///
/// [`Steam::missing_libraries`], plus a walk of its own — and it is the half
/// of an answer that must not drift from the other half, so a caller that
/// also wants [`apps`] wants one [`Steam`] rather than these two.
pub fn missing_libraries(home: &Path) -> Vec<PathBuf> {
    Steam::at(home).missing
}

/// Everything installed, across every library.
///
/// [`Steam::at`] plus [`Steam::apps`]. See [`missing_libraries`] for why a
/// caller that prints this list usually wants the [`Steam`] rather than this.
pub fn apps(home: &Path) -> Vec<App> {
    Steam::at(home).apps()
}

/// The Proton prefix for `appid`, if the game has ever been run.
///
/// [`Steam::at`] plus [`Steam::prefix`]. Asking this of a list of titles is
/// what made the [`Steam`] value necessary: it is one whole scan per title.
pub fn prefix(home: &Path, appid: &str) -> Option<PathBuf> {
    Steam::at(home).prefix(appid)
}

/// Everything installed in these libraries, collapsed the way [`App`]'s
/// identity says it must be.
fn apps_in(libraries: &[PathBuf]) -> Vec<App> {
    let mut out = Vec::new();
    for lib in libraries {
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
    // Sorted by name as well as app id although `Ord` compares only the app
    // id: `sort` is stable, so it would leave the order inside a run of copies
    // to the order the roots are scanned in and then whatever `read_dir` handed
    // back. Both collapses below fold a run into its first row, and the joined
    // name is read by a human, so the run has to come out the same every time.
    //
    // Case-folded first, because the joined name claims to be alphabetical and
    // byte order is not: it puts every capital ahead of every lowercase
    // letter, so a manifest written in lower case sank below one that was not
    // and the row read `Elite Dangerous: Odyssey / elite dangerous`. The raw
    // name then breaks the tie, so copies whose names differ only in case come
    // out in one fixed order rather than the scan's.
    out.sort_by_cached_key(|a| (a.appid.clone(), a.name.to_lowercase(), a.name.clone()));
    // Copies that agree on the name are the same row already, and all that can
    // differ is the build. Not a plain `dedup`, which keeps the first of a run
    // and whatever build that copy was at: that copy is whichever library was
    // scanned first, so naming its build would be arbitrary and could change
    // between two runs on a machine nobody touched. When the copies disagree
    // there is no build to name, and the field says so.
    out.dedup_by(|dropped, kept| {
        if (&dropped.appid, &dropped.name) != (&kept.appid, &kept.name) {
            return false;
        }
        if dropped.buildid != kept.buildid {
            kept.buildid = None;
        }
        true
    });
    // What is left of a run is one app id under names that are now all
    // different and in ascending order, so joining them repeats nothing. This
    // pass is what keeps [`Match::Many`] from ever naming one app id twice; see
    // the `name` field for why both names are kept rather than one chosen.
    out.dedup_by(|dropped, kept| {
        if dropped.appid != kept.appid {
            return false;
        }
        if dropped.buildid != kept.buildid {
            kept.buildid = None;
        }
        kept.name = format!("{} / {}", kept.name, dropped.name);
        true
    });
    out
}

/// Whether an installed application is Steam's own plumbing rather than
/// something a user would configure head tracking for.
///
/// **This is a heuristic on the name, and it is one because nothing better
/// exists.** Two things were checked against a real library of 29 applications
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
    /// Nothing matched. Carries nothing: every caller wants to show the user
    /// what there was instead, and every caller already holds that — the list
    /// is what it passed in.
    None,
    /// Exactly one, by app id or by name.
    One(App),
    /// A name fragment matching several. Carries them in the order they were
    /// given, so a caller can list them.
    ///
    /// **Never the same app id twice.** Every caller words this as "pass the
    /// app id" above the list, so a list naming one id on two lines asks a
    /// question whose answer it has already thrown away. [`apps`] guarantees
    /// it by making the app id the whole of an [`App`]'s identity — two
    /// manifests for one id are one row, under both of their names. A caller
    /// that assembles a list of its own owes the same.
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
///
/// An empty `wanted` is not an app id — the all-digits test excludes it, so it
/// cannot come back with whatever [`App`] happens to carry the empty app id.
/// It is a name fragment like any other, and every name contains it, so the
/// answer is [`Match::Many`] over everything installed — except on a machine
/// with exactly one application, where matching everything is [`Match::One`]
/// naming that application, indistinguishable from a fragment that picked
/// that one application out.
///
/// **It is never refused.** No answer here means "you gave me nothing":
/// [`Match::None`] means the needle matched none of the names, which an empty
/// needle reaches only when there are no names — on a machine with nothing
/// installed, where every needle answers that. A caller for which an empty
/// value means "not given" has to say so before it asks, because nothing in
/// this function can tell the two apart.
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
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// The scratch directories one thread has made, removed when that thread
    /// ends.
    ///
    /// A guard the caller holds is the plain way to write this, and it does
    /// not fit: the helpers below build a home inside themselves and hand
    /// back what was read out of it, so a guard would drop at the end of the
    /// helper and take the home with it before the case had looked at
    /// anything. So the guard lives here instead.
    ///
    /// `libtest` gives each test a thread of its own, and a thread-local's
    /// destructor runs when that thread ends. Measured at nothing left
    /// behind, with `--test-threads=1` as well as in parallel — counted by
    /// the process id in the directory names, because other things on a
    /// developer's machine run these tests too and a count of the whole
    /// `/tmp` glob measures those runs as much as this one.
    ///
    /// What it rests on is that the tests do not run on the main thread,
    /// whose locals are not destroyed. Were a harness ever to run them there,
    /// this would leave behind what it used to: fifteen directories per run,
    /// and the temporary directory had over a thousand of them.
    struct Sweep(Vec<PathBuf>);

    impl Drop for Sweep {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }

    thread_local! {
        static SWEEP: RefCell<Sweep> = const { RefCell::new(Sweep(Vec::new())) };
    }

    /// A throwaway directory that is this process's alone, removed when the
    /// thread that asked for it ends.
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
        SWEEP.with_borrow_mut(|sweep| sweep.0.push(p.clone()));
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

    /// An `App` to feed a pure function that takes a list — never an expected
    /// value, for the reason on [`row`].
    fn app(id: &str, name: &str) -> App {
        App {
            appid: id.into(),
            name: name.into(),
            buildid: None,
        }
    }

    /// Every field of one row, as a tuple that compares as all three.
    ///
    /// An `App` equals another when the APP ID matches and nothing else,
    /// which is the whole point of the type — so `assert_eq!` on an `App`, on
    /// a `Vec<App>`, or on a [`Match`] carrying either, compares app ids. An
    /// expected name written out beside one is then read by whoever maintains
    /// the test and by nothing else, and the case can go on passing with the
    /// name read out of the manifest replaced by anything at all. Every
    /// answer below is compared as a row, so the name and the build a case
    /// spells out are the name and the build it checks.
    fn row(a: &App) -> (&str, &str, Option<&str>) {
        (&a.appid, &a.name, a.buildid.as_deref())
    }

    fn rows(apps: &[App]) -> Vec<(&str, &str, Option<&str>)> {
        apps.iter().map(row).collect()
    }

    /// The single app a [`Match::One`] named, or a panic naming what came back
    /// instead. A `Match` compares by app id too, so an answer is unwrapped
    /// and checked as a row rather than compared against a built one.
    fn only(m: Match) -> App {
        match m {
            Match::One(a) => a,
            other => panic!("expected exactly one match, got {other:?}"),
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
        assert_eq!(rows(&apps(&home)), vec![("1", "A Game", None)]);
    }

    /// `libraryfolders.vdf` outlives the drive: Steam records a library's
    /// path, not whether it is plugged in. The library then drops out of
    /// [`libraries`] and its titles out of [`apps`] without a word, so a
    /// caller saying "no installed Steam game matches" says it about a game
    /// that is installed. The maintainer's own file names a library under
    /// `/run/media` that is not there.
    #[test]
    fn a_library_the_vdf_names_and_the_machine_does_not_have_is_reported() {
        let home = scratch("unplugged");
        let root = home.join(".steam/steam");
        let here = home.join("games/plugged-in");
        // Bounded to this test's own scratch directory on purpose: a path
        // built by walking up from anywhere else is how a plant escapes.
        let gone = home.join("run/media/nobody/ExternalSSD/steam");
        library(&root, &[("1", "On The Internal Drive", None)]);
        library(&here, &[("2", "On The Other Drive", None)]);
        std::fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            format!(
                "\t\"path\"\t\t\"{}\"\n\t\"path\"\t\t\"{}\"\n",
                here.display(),
                gone.display()
            ),
        )
        .expect("vdf");

        assert_eq!(
            missing_libraries(&home),
            vec![gone.clone()],
            "the one path the file names that is not here, as the file spells it"
        );
        assert!(
            !libraries(&home).contains(&gone),
            "and not among the libraries, because there is nothing to read there"
        );
        // What IS readable still reads: this reports the gap, it does not
        // widen it.
        assert_eq!(
            rows(&apps(&home)),
            vec![
                ("1", "On The Internal Drive", None),
                ("2", "On The Other Drive", None)
            ]
        );
    }

    /// Nothing missing is nothing said. A Steam root that is not installed is
    /// not a missing library: three of the four are absent on an ordinary
    /// machine, and naming them would bury the absence that means something.
    #[test]
    fn a_machine_holding_every_library_its_vdf_names_reports_none_missing() {
        let home = scratch("allthere");
        let root = home.join(".steam/steam");
        let other = home.join("games/library-two");
        library(&root, &[("1", "First", None)]);
        library(&other, &[("2", "Second", None)]);
        std::fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            format!("\t\"path\"\t\t\"{}\"\n", other.display()),
        )
        .expect("vdf");
        assert_eq!(missing_libraries(&home), Vec::<PathBuf>::new());
    }

    /// One absent library named by two roots is one absence. `~/.steam/steam`
    /// and the Flatpak each keep their own `libraryfolders.vdf`, and an
    /// external drive is in both.
    #[test]
    fn one_absent_library_named_by_two_roots_is_reported_once() {
        let home = scratch("twiceabsent");
        let gone = home.join("run/media/nobody/ExternalSSD/steam");
        for root in [
            ".steam/steam",
            ".var/app/com.valvesoftware.Steam/data/Steam",
        ] {
            let root = home.join(root);
            library(&root, &[]);
            std::fs::write(
                root.join("steamapps/libraryfolders.vdf"),
                format!("\t\"path\"\t\t\"{}\"\n", gone.display()),
            )
            .expect("vdf");
        }
        assert_eq!(missing_libraries(&home), vec![gone]);
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
            rows(&found),
            vec![("1", "First", Some("111")), ("2", "Second", None)],
            "both libraries, sorted, under the names and builds their \
             manifests gave"
        );
    }

    /// One app id written into two libraries — a stale `appmanifest` left
    /// behind by a manual copy, or native Steam and the Flatpak each holding
    /// the title. Each copy gets its own name and build. Returns what `apps`
    /// makes of the pair.
    ///
    /// `second` is the copy that is met FIRST: `libraries` pushes the libraries
    /// the vdf names before the root that names them. That is why a case whose
    /// answer could be reached by keeping whichever copy came first is written
    /// out both ways round rather than once.
    fn one_appid_twice(
        what: &str,
        appid: &str,
        first: (&str, Option<&str>),
        second: (&str, Option<&str>),
    ) -> Vec<App> {
        let home = scratch(what);
        let root = home.join(".steam/steam");
        let other = home.join("games/library-two");
        library(&root, &[(appid, first.0, first.1)]);
        library(&other, &[(appid, second.0, second.1)]);
        std::fs::write(
            root.join("steamapps/libraryfolders.vdf"),
            format!("\t\"path\"\t\t\"{}\"\n", other.display()),
        )
        .expect("vdf");
        apps(&home)
    }

    /// The pair under one name, disagreeing only about the build.
    fn one_title_twice(what: &str, first: Option<&str>, second: Option<&str>) -> Vec<App> {
        one_appid_twice(
            what,
            "42",
            ("Elite Dangerous", first),
            ("Elite Dangerous", second),
        )
    }

    /// The same title in two libraries is one title. An identity that included
    /// the buildid listed it twice — and `resolve` then answered "matches more
    /// than one game; pass the app id" above a list carrying the same app id on
    /// both lines, which is a question about which of these the user has no way
    /// of answering.
    #[test]
    fn one_title_in_two_libraries_at_two_builds_is_one_row() {
        let found = one_title_twice("twobuilds", Some("111"), Some("222"));
        assert_eq!(
            rows(&found),
            vec![("42", "Elite Dangerous", None)],
            "one row, not two, and no build the copies disagree on"
        );
        let picked = only(resolve(&found, "elite"));
        assert_eq!(
            row(&picked),
            ("42", "Elite Dangerous", None),
            "one game, not a choice between two rows carrying one app id"
        );
    }

    /// Which build survives when the copies disagree: none does. Collapsing a
    /// run keeps its first member, and which manifest that is comes from the
    /// order the roots are scanned in and then whatever `read_dir` handed back
    /// — so a surviving build would be arbitrary, and could differ between two
    /// runs on a machine nobody touched.
    #[test]
    fn a_build_the_copies_disagree_on_is_reported_as_no_build_at_all() {
        let agree = one_title_twice("agree", Some("111"), Some("111"));
        assert_eq!(
            rows(&agree),
            vec![("42", "Elite Dangerous", Some("111"))],
            "both copies say 111, so 111 is the answer"
        );

        let disagree = one_title_twice("disagree", Some("111"), Some("222"));
        assert_eq!(
            rows(&disagree),
            vec![("42", "Elite Dangerous", None)],
            "two builds, so there is no build to name"
        );

        // Both ways round. The library the vdf names is scanned before the
        // root, so with the build-less copy in that library this case is what a
        // plain `dedup` — keep the first row of the run, build and all — also
        // answers, and it would pass with no merging at all. The mirror is the
        // one a plain `dedup` gets wrong.
        for (what, first, second) in [("half", Some("111"), None), ("mirror", None, Some("111"))] {
            assert_eq!(
                rows(&one_title_twice(what, first, second)),
                vec![("42", "Elite Dangerous", None)],
                "a copy naming no build disagrees with one that does ({what})"
            );
        }

        // Renamed AND repatched, which is the case the second collapse has to
        // answer alone: the first one folds copies that agree on the name, and
        // these do not, so the run reaches the name-joining pass still
        // carrying two builds. Dropping the merge there left the joined row at
        // whichever build was met first, and every other case still passed.
        assert_eq!(
            rows(&one_appid_twice(
                "renamedandrepatched",
                "578080",
                ("PUBG", Some("111")),
                ("PUBG: BATTLEGROUNDS", Some("222")),
            )),
            vec![("578080", "PUBG / PUBG: BATTLEGROUNDS", None)],
            "both names, and no build either copy can claim"
        );
    }

    /// The same defect reached by the other field: two manifests for one app
    /// id that disagree about the NAME, which is what a rename leaves behind.
    /// This listed the title twice, so `resolve` said "matches more than one
    /// game; pass the app id" above `578080 PUBG` and `578080 PUBG:
    /// BATTLEGROUNDS` — one id, asked for, on both lines.
    #[test]
    fn one_app_id_under_two_names_is_one_row_reachable_by_either_name() {
        let found = one_appid_twice(
            "renamed",
            "578080",
            ("PUBG", Some("111")),
            ("PUBG: BATTLEGROUNDS", Some("111")),
        );
        // One app id is one game, under both names. The copy met first is the
        // vdf'd library's, so scan order would put the longer name in front;
        // alphabetical order is what is asserted. Renamed, not repatched, so
        // the copies agree on the build.
        assert_eq!(
            rows(&found),
            vec![("578080", "PUBG / PUBG: BATTLEGROUNDS", Some("111"))],
            "both names, in an order that does not depend on the scan"
        );
        // Either way of typing it reaches the one row. Picking one name would
        // have left the other matching nothing.
        for typed in ["pubg", "battlegrounds", "PUBG: BATTLEGROUNDS"] {
            let picked = only(resolve(&found, typed));
            assert_eq!(
                row(&picked),
                ("578080", "PUBG / PUBG: BATTLEGROUNDS", Some("111")),
                "{typed:?} is this game, not a choice between two rows"
            );
        }
    }

    /// "Alphabetical" has to mean what a reader means by it. Byte order sorts
    /// every capital ahead of every lowercase letter, so a copy whose manifest
    /// was written in lower case sank below one that was not and the joined
    /// name came back `Elite Dangerous: Odyssey / elite dangerous`.
    #[test]
    fn names_that_differ_in_case_are_joined_in_alphabetical_order() {
        let found = one_appid_twice(
            "casefold",
            "99",
            ("elite dangerous", Some("111")),
            ("Elite Dangerous: Odyssey", Some("111")),
        );
        assert_eq!(
            rows(&found),
            vec![(
                "99",
                "elite dangerous / Elite Dangerous: Odyssey",
                Some("111")
            )],
            "a letter's case is not its place in the alphabet"
        );

        // Names differing ONLY in case fold to one key, so the name itself
        // breaks the tie. Without it the order is the scan's — the vdf'd
        // library is met first, which would put `pubg` in front.
        let tied = one_appid_twice("casetie", "98", ("PUBG", None), ("pubg", None));
        assert_eq!(
            rows(&tied),
            vec![("98", "PUBG / pubg", None)],
            "a tie in the fold is broken by the name, not by the scan"
        );
    }

    /// Neither the build nor the name is part of a title's identity, and
    /// `Hash` agrees with `Eq` about both.
    #[test]
    fn only_the_app_id_says_which_application_this_is() {
        let a = App {
            appid: "42".into(),
            name: "Elite Dangerous".into(),
            buildid: Some("111".into()),
        };
        let repatched = App {
            buildid: Some("222".into()),
            ..a.clone()
        };
        let renamed = App {
            name: "Elite Dangerous Odyssey".into(),
            ..a.clone()
        };
        for other in [&repatched, &renamed] {
            assert_eq!(&a, other, "one app id is one application: {other:?}");
            assert_eq!(
                a.cmp(other),
                std::cmp::Ordering::Equal,
                "and sorts in one place: {other:?}"
            );
        }
        let set: std::collections::HashSet<App> =
            [a.clone(), repatched, renamed].into_iter().collect();
        assert_eq!(set.len(), 1, "Hash agrees with Eq");
        let other_game = app("7", "Elite Dangerous");
        assert_ne!(a, other_game, "the app id is what tells them apart");
        assert!(a < other_game, "and what they sort on");
    }

    /// Steam's "Move Install Folder" does not move `compatdata`, so a moved
    /// game leaves its old prefix behind and gets a fresh one. Installing into
    /// the abandoned one succeeds, says so, and does nothing for the game — so
    /// the library holding the manifest is asked FIRST, not whichever library
    /// happens to come first.
    /// Two libraries, each holding a prefix for the same title, with the
    /// manifest in the one the scan reaches **last**.
    ///
    /// The order matters to the case and not only to the code: a fixture whose
    /// owning library is also the first one scanned passes whether the
    /// preference exists or not, and this fixture was that until it was
    /// checked by removing the preference and watching it go on passing. A
    /// root is pushed after everything its own `libraryfolders.vdf` names, so
    /// putting the manifest in the root is what makes the two orders differ.
    fn moved_game(what: &str) -> (PathBuf, PathBuf, PathBuf) {
        let home = scratch(what);
        let owner = home.join(".steam/steam");
        let abandoned = home.join("games/second");
        library(&owner, &[("42", "Moved Game", None)]);
        library(&abandoned, &[]);
        std::fs::write(
            owner.join("steamapps/libraryfolders.vdf"),
            format!("\t\"path\"\t\t\"{}\"\n", abandoned.display()),
        )
        .expect("vdf");
        for lib in [&owner, &abandoned] {
            std::fs::create_dir_all(lib.join("steamapps/compatdata/42/pfx/drive_c")).expect("pfx");
        }
        (home, owner, abandoned)
    }

    /// Steam's "Move Install Folder" does not move `compatdata`, so a moved
    /// game leaves its old prefix behind and gets a fresh one. Installing into
    /// the abandoned one succeeds, says so, and does nothing for the game — so
    /// the library holding the manifest is asked FIRST, not whichever library
    /// happens to come first.
    #[test]
    fn the_library_that_owns_the_manifest_is_asked_before_the_others() {
        let (home, owner, abandoned) = moved_game("moved");
        let libs = libraries(&home);
        assert_eq!(
            libs.first(),
            Some(&abandoned.canonicalize().expect("real")),
            "the fixture is only a test of the preference while the owning \
             library is not the first one scanned: {libs:?}"
        );
        let got = prefix(&home, "42").expect("a prefix");
        assert!(
            got.starts_with(owner.canonicalize().expect("real")),
            "the manifest's own library wins; got {got:?}"
        );
    }

    /// The point of [`Steam`]: the walk is taken once, at construction, and
    /// nothing asked of the value afterwards goes back to `libraryfolders.vdf`.
    ///
    /// Checked by taking the file away and asking again. A library named only
    /// by that file is the whole of what it can tell anyone, so a second walk
    /// would lose it — which is exactly what the free functions do here, and
    /// they are asserted alongside so the case fails if the two ever become
    /// the same thing.
    #[test]
    fn the_walk_is_taken_once_and_every_later_answer_comes_out_of_it() {
        let home = scratch("walkonce");
        let root = home.join(".steam/steam");
        let named_only_by_the_file = home.join("games/library-two");
        library(&root, &[]);
        library(
            &named_only_by_the_file,
            &[("42", "Second Library Game", None)],
        );
        std::fs::create_dir_all(named_only_by_the_file.join("steamapps/compatdata/42/pfx/drive_c"))
            .expect("pfx");
        let vdf = root.join("steamapps/libraryfolders.vdf");
        std::fs::write(
            &vdf,
            format!("\t\"path\"\t\t\"{}\"\n", named_only_by_the_file.display()),
        )
        .expect("vdf");

        let steam = Steam::at(&home);
        std::fs::remove_file(&vdf).expect("take the file away");

        assert_eq!(
            rows(&steam.apps()),
            vec![("42", "Second Library Game", None)],
            "the library the file named is still this value's, after the file is gone"
        );
        assert!(
            steam.prefix("42").is_some(),
            "and so is the prefix in it: asking again is not walking again"
        );
        assert!(
            apps(&home).is_empty() && prefix(&home, "42").is_none(),
            "while a free function walks afresh and now finds nothing — which is \
             what every one of these calls was doing on every question"
        );
    }

    /// A title can have a prefix in every library it has ever been installed
    /// in, and the hub is the one place that offers to install into one — so
    /// it has to be able to name the others. It used to rebuild
    /// `compatdata/<appid>/pfx` out of `libraries` itself to do it.
    #[test]
    fn every_prefix_a_title_has_is_listed_with_the_chosen_one_first() {
        let (home, owner, abandoned) = moved_game("allprefixes");
        let steam = Steam::at(&home);
        let all = steam.prefixes("42");
        assert_eq!(all.len(), 2, "both are on this machine: {all:?}");
        assert_eq!(
            all.first(),
            steam.prefix("42").as_ref(),
            "and the one an install writes into leads the list"
        );
        assert!(
            all[0].starts_with(owner.canonicalize().expect("real"))
                && all[1].starts_with(abandoned.canonicalize().expect("real")),
            "which is the manifest's own library, although the scan reaches the \
             abandoned one first: {all:?}"
        );
        assert!(
            steam.prefixes("7").is_empty(),
            "a title that has never been run has none, as `prefix` says with None"
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
            rows(&apps(&home)),
            vec![("1", "Fine", None)],
            "the readable one survives, and nothing was guessed for the rest"
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

    /// An empty `--steam` value is all-digits vacuously, so the digit branch
    /// excludes it on purpose: taking it would match an app whose id is the
    /// empty string — which is to say, whatever `find` happened on. What is
    /// left is a name fragment every name contains, and that is the hazard the
    /// callers have to know about: it matches everything, and on a machine
    /// with exactly one application everything is one game. The one machine it
    /// answers [`Match::None`] on is the one with nothing installed.
    #[test]
    fn an_empty_name_is_every_game_rather_than_an_app_id() {
        // If the digit branch took the empty string, `find` would hand back
        // the app whose id is the empty string and nothing else. It does not:
        // both of these are reached as name matches.
        let with_idless = vec![
            App {
                appid: String::new(),
                name: "Not What Was Asked For".into(),
                buildid: None,
            },
            app("42", "Elite Dangerous"),
        ];
        assert_eq!(
            resolve(&with_idless, ""),
            Match::Many(with_idless.clone()),
            "a name fragment, not a lookup of the empty app id"
        );

        let one = vec![app("42", "Elite Dangerous")];
        assert_eq!(
            resolve(&one, ""),
            Match::One(one[0].clone()),
            "with one application installed, matching everything matches it"
        );

        assert_eq!(
            resolve(&[], ""),
            Match::None,
            "the only way an empty fragment matches nothing: there is nothing"
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
}
