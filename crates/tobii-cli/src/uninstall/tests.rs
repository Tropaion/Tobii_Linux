//! `plan()` and `execute()` against temporary trees only. Nothing here runs a
//! binary, asks a package manager or signals anything: ownership,
//! identification and the process list are all injected. One test reads this
//! test process's own `/proc` entry, to check how a process is told apart from
//! a later one with the same pid.

use super::*;

/// Directories made 0755 whatever the umask: under umask 002 every directory
/// of a test tree would otherwise be group-writable, which [`Fs::vet`] refuses.
fn mkdirs(p: &Path) {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(p)
        .unwrap();
}

/// This test process's effective uid: the owner of every file a test makes.
fn real_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").unwrap().uid()
}

/// The uid the plans here are made for: this process's, so the files a test
/// makes are the user's own. As root, 1000 — root may not plan for itself
/// without --system, and root-owned files are trusted anyway.
fn me() -> u32 {
    match real_uid() {
        0 => 1000,
        u => u,
    }
}

/// A temporary filesystem root, removed on the way out.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Tree {
        Tree::under(&std::env::temp_dir(), tag)
    }
    fn under(base: &Path, tag: &str) -> Tree {
        let root = base.join(format!(
            "tobii-uninstall-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        mkdirs(&root);
        Tree { root }
    }
    fn real(&self, p: &str) -> PathBuf {
        self.root.join(p.trim_start_matches('/'))
    }
    fn put(&self, p: &str, content: &str) {
        let real = self.real(p);
        mkdirs(real.parent().unwrap());
        std::fs::write(real, content).unwrap();
    }
    /// A fake program: an ELF header line, then what it answers to
    /// `--version` (see [`by_content`]), mode 755.
    fn bin(&self, p: &str, answer: &str) {
        self.put(p, &format!("\x7fELF\n{answer}\n"));
        self.chmod(p, 0o755);
    }
    fn chmod(&self, p: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(self.real(p), std::fs::Permissions::from_mode(mode)).unwrap();
    }
    /// `link` -> `target`, both inside the tree.
    fn symlink(&self, target: &str, link: &str) {
        let real = self.real(link);
        mkdirs(real.parent().unwrap());
        std::os::unix::fs::symlink(self.real(target), real).unwrap();
    }
    fn mkdir(&self, p: &str) {
        mkdirs(&self.real(p));
    }
    fn has(&self, p: &str) -> bool {
        self.real(p).symlink_metadata().is_ok()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const BIN: &str = "/home/u/.local/bin";
const ENTRY: &str = "/home/u/.local/share/applications/com.tobiilinux.Configuration.desktop";
const ICON: &str =
    "/home/u/.local/share/icons/hicolor/scalable/apps/com.tobiilinux.Configuration.svg";
const AUTOSTART: &str = "/home/u/.config/autostart/com.tobiilinux.Configuration.desktop";
const MANIFEST: &str = "/home/u/.local/share/tobii-linux/installs";
/// What Cargo writes at the top of its output directory, word for word.
const CARGO_TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n\
     # This file is a cache directory tag created by cargo.\n\
     # For information about cache directory tags see https://bford.info/cachedir/\n";

fn user() -> Env {
    Env {
        home: Some("/home/u".into()),
        euid: me(),
        pid: 4242,
        ppid: 4241,
        ..Env::default()
    }
}

fn menu_entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\n\
         # scripts/install-payload.sh rewrites the Exec line below\n\
         Exec={exec}\nIcon=com.tobiilinux.Configuration\n"
    )
}

/// What `install.sh` from v0.3.0 leaves: two binaries, a menu entry with an
/// absolute Exec, the icon — and no manifest, which did not exist yet.
fn v030_install(t: &Tree) {
    t.bin(&format!("{BIN}/tobii"), "tobii 0.3.0");
    t.bin(&format!("{BIN}/tobii-gtk"), "tobii-gtk 0.3.0");
    t.put(ENTRY, &menu_entry("/home/u/.local/bin/tobii-gtk"));
    t.put(ICON, "<svg/>");
}

/// A fake binary "answers --version" with its first line — or, after an ELF
/// header line, its second.
fn by_content(p: &Path) -> Option<String> {
    let bytes = std::fs::read(p).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    let first = lines.next()?;
    if first == "\x7fELF" {
        lines.next().map(str::to_string)
    } else {
        Some(first.to_string())
    }
}

fn unowned(_: &Path) -> Ownership {
    Ownership::None
}

fn pacman_owns_usr_bin(d: &Path) -> Ownership {
    if d.ends_with("usr/bin") {
        Ownership::Package {
            manager: "pacman".into(),
            package: "tobii-linux".into(),
        }
    } else {
        Ownership::None
    }
}

fn plan_in(t: &Tree, env: &Env, opts: &Options, owner: &dyn Fn(&Path) -> Ownership) -> Plan {
    plan_procs(t, env, opts, owner, &[])
}

fn plan_procs(
    t: &Tree,
    env: &Env,
    opts: &Options,
    owner: &dyn Fn(&Path) -> Ownership,
    procs: &[Proc],
) -> Plan {
    plan_full(t, env, opts, owner, &|_| true, &by_content, procs)
}

fn plan_full(
    t: &Tree,
    env: &Env,
    opts: &Options,
    owner: &dyn Fn(&Path) -> Ownership,
    writable: &dyn Fn(&Path) -> bool,
    identify: &dyn Fn(&Path) -> Option<String>,
    procs: &[Proc],
) -> Plan {
    let probes = Probes {
        owner,
        identify,
        writable,
        private_group: &|_| false,
    };
    plan(env, opts, &t.root, &probes, procs)
}

/// Probes that answer as a plain home install would: no package, writable,
/// no private group, and `--version` answered by [`by_content`].
fn home_probes<'a>(identify: &'a dyn Fn(&Path) -> Option<String>) -> Probes<'a> {
    Probes {
        owner: &unowned,
        identify,
        writable: &|_| true,
        private_group: &|_| false,
    }
}

/// An `identify` that records every file it is asked to run, and answers
/// with [`by_content`]: the marker for "was this ever run".
struct Runs(std::cell::RefCell<Vec<PathBuf>>);

impl Runs {
    fn new() -> Runs {
        Runs(std::cell::RefCell::new(Vec::new()))
    }
    fn identify(&self) -> impl Fn(&Path) -> Option<String> + '_ {
        |p: &Path| {
            self.0.borrow_mut().push(p.to_path_buf());
            by_content(p)
        }
    }
    /// Whether anything whose real path ends with `p` was run.
    fn ran(&self, t: &Tree, p: &str) -> bool {
        let want = t.real(p);
        self.0
            .borrow()
            .iter()
            .any(|r| r == &want || r.ends_with(p.trim_start_matches('/')))
    }
}

/// Every path the plan would delete, the running binary included.
fn removals(p: &Plan) -> Vec<String> {
    p.remove
        .iter()
        .map(|r| r.path.display().to_string())
        .chain(p.self_exe.iter().map(|s| s.display().to_string()))
        .collect()
}

fn kept(p: &Plan) -> Vec<String> {
    p.kept
        .iter()
        .map(|k| k.path.display().to_string())
        .collect()
}

fn why_kept<'a>(p: &'a Plan, path: &str) -> &'a str {
    &p.kept
        .iter()
        .find(|k| k.path == Path::new(path))
        .unwrap_or_else(|| panic!("{path} is not kept: {:#?}", p.kept))
        .why
}

fn why_removed<'a>(p: &'a Plan, path: &str) -> &'a str {
    &p.remove
        .iter()
        .find(|r| r.path == Path::new(path))
        .unwrap_or_else(|| panic!("{path} is not removed: {:#?}", p.remove))
        .what
}

fn location<'a>(p: &'a Plan, dir: &str) -> &'a Location {
    p.locations
        .iter()
        .find(|l| l.dir == Path::new(dir))
        .unwrap_or_else(|| panic!("{dir} was not looked at: {:#?}", p.locations))
}

fn proc(pid: u32, uid: u32, exe: &str, args: &[&str]) -> Proc {
    Proc {
        pid,
        uid,
        exe: exe.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        start_time: None,
    }
}

/// Every path `program_check` opened on this thread since the last call.
fn checked() -> Vec<PathBuf> {
    PROGRAM_CHECKED.with(|c| c.take())
}

/// Who owns a file, as a test tree pretends: everything under `/opt` is
/// root's, as a system install is.
fn opt_is_roots(p: &Path, m: &std::fs::Metadata) -> u32 {
    if p.components().any(|c| c.as_os_str() == "opt") {
        0
    } else {
        owner_of(p, m)
    }
}

#[test]
fn a_v030_install_with_no_manifest_is_found_through_the_menu_entry() {
    let t = Tree::new("v030");
    v030_install(&t);
    t.put(
        AUTOSTART,
        &autostart::entry_text("/home/u/.local/bin/tobii-gtk"),
    );
    // The hub's start-at-login switch writes through a temporary; one left by
    // an interrupted write goes, and names that only look like it stay.
    let scratch = format!("{AUTOSTART}.new-777");
    t.put(&scratch, "x");
    t.put(&format!("{AUTOSTART}.new-"), "x");
    t.put(&format!("{AUTOSTART}.new-7a"), "x");

    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert!(p.refusal.is_none(), "{:?}", p.refusal);
    let r = removals(&p);
    for want in [
        format!("{BIN}/tobii"),
        format!("{BIN}/tobii-gtk"),
        ENTRY.to_string(),
        ICON.to_string(),
        AUTOSTART.to_string(),
        scratch.clone(),
    ] {
        assert!(r.contains(&want), "{want} is missing from {r:#?}");
    }
    assert!(!r.contains(&format!("{AUTOSTART}.new-")), "{r:#?}");
    assert!(!r.contains(&format!("{AUTOSTART}.new-7a")), "{r:#?}");
    let loc = location(&p, BIN);
    assert!(loc.via.contains(&Via::MenuEntry), "{:?}", loc.via);
    assert_eq!(loc.verdict, Verdict::Remove);
    // Identified, not trusted: both answered with their own name.
    assert!(loc
        .binaries
        .iter()
        .all(|b| b.planned && matches!(b.ident, Ident::Answered(Some(_)))));
    assert!(p.manifests.is_empty(), "there is no manifest to edit");

    // And the plan says what the identification is worth.
    let text = render(&p, &Options::default());
    assert!(text.contains("it does not verify them"), "{text}");
}

/// This machine's real state: the hub's start-at-login switch was turned on
/// while a package was installed, and the package is gone.
#[test]
fn an_autostart_entry_for_a_deleted_usr_bin_hub_is_removed() {
    let t = Tree::new("stale-autostart");
    v030_install(&t);
    t.put(AUTOSTART, &autostart::entry_text("/usr/bin/tobii-gtk"));

    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert!(why_removed(&p, AUTOSTART).contains("no longer exists"));
    // /usr/bin was looked at through the entry and holds nothing of ours.
    assert_eq!(location(&p, "/usr/bin").verdict, Verdict::NothingThere);
}

/// Removing an entry that a remaining install uses would silently switch its
/// start-at-login off.
#[test]
fn an_autostart_entry_for_a_copy_that_stays_is_kept() {
    let t = Tree::new("keep-autostart");
    v030_install(&t);
    t.bin("/usr/bin/tobii-gtk", "tobii-gtk 0.3.0");
    t.put(AUTOSTART, &autostart::entry_text("/usr/bin/tobii-gtk"));

    let p = plan_in(&t, &user(), &Options::default(), &pacman_owns_usr_bin);
    assert!(!removals(&p).contains(&AUTOSTART.to_string()), "{p:#?}");
    assert!(kept(&p).contains(&AUTOSTART.to_string()));
    // The home copy still goes.
    assert!(removals(&p).contains(&format!("{BIN}/tobii-gtk")));

    // The same for a copy that is kept because it would not identify itself.
    let t = Tree::new("keep-autostart-unidentified");
    t.bin("/opt/other/tobii-gtk", "something else entirely");
    t.put(AUTOSTART, &autostart::entry_text("/opt/other/tobii-gtk"));
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert!(kept(&p).contains(&AUTOSTART.to_string()), "{p:#?}");
    assert!(kept(&p).contains(&"/opt/other/tobii-gtk".to_string()));
    assert!(removals(&p).is_empty(), "{:#?}", removals(&p));
}

/// A hand-edited entry, `Exec=env VAR=value /path …`, runs the path, not
/// `env`; a bare name runs whatever PATH finds. Either is judged by the
/// program it runs, and one that cannot be judged stays.
#[test]
fn an_entry_run_through_env_or_by_a_bare_name_is_judged_by_the_program_it_runs() {
    let t = Tree::new("env-bare");
    v030_install(&t);
    t.bin("/usr/bin/tobii-gtk", "tobii-gtk 0.3.0");
    let autostart_says = |exec: &str| t.put(AUTOSTART, &format!("[Desktop Entry]\nExec={exec}\n"));
    let plan_with_path = |path: &[&str]| {
        let env = Env {
            path: path.iter().map(PathBuf::from).collect(),
            ..user()
        };
        plan_in(&t, &env, &Options::default(), &pacman_owns_usr_bin)
    };

    // env, and a program that stays.
    autostart_says("env GDK_BACKEND=x11 /usr/bin/tobii-gtk --background");
    let p = plan_with_path(&[]);
    let why = why_kept(&p, AUTOSTART);
    assert!(
        why.contains("/usr/bin/tobii-gtk") && why.contains("not being removed"),
        "{why}"
    );
    // env, and a program that is being removed.
    autostart_says("env GDK_BACKEND=x11 LANG=C /home/u/.local/bin/tobii-gtk --background");
    let p = plan_with_path(&[]);
    assert!(why_removed(&p, AUTOSTART).contains("being removed"));
    // /usr/bin/env is env too.
    autostart_says("/usr/bin/env A=1 /usr/bin/tobii-gtk");
    assert!(kept(&plan_with_path(&[])).contains(&AUTOSTART.to_string()));

    // A bare name. With the home copy first on PATH and the packaged one
    // after it, the entry runs the packaged one once this run is done.
    autostart_says("tobii-gtk --background");
    let p = plan_with_path(&[BIN, "/usr/bin"]);
    assert!(why_kept(&p, AUTOSTART).contains("/usr/bin/tobii-gtk"));
    // Found only where it is being removed: it goes.
    let p = plan_with_path(&[BIN]);
    assert!(why_removed(&p, AUTOSTART).contains("PATH finds only"));
    // Not on this PATH at all: that says nothing about the session's.
    let p = plan_with_path(&[]);
    assert!(why_kept(&p, AUTOSTART).contains("not on this PATH"));

    // What cannot be judged stays, and says why.
    for (exec, says) in [
        (
            "env -u DISPLAY /home/u/.local/bin/tobii-gtk",
            "env's options",
        ),
        ("bin/tobii-gtk", "relative path"),
        ("\"/home/u/.local/bin/tobii-gtk", "no Exec line"),
    ] {
        autostart_says(exec);
        let p = plan_with_path(&[]);
        let why = why_kept(&p, AUTOSTART);
        assert!(
            why.contains(says) && why.contains("left as it is"),
            "{exec}: {why}"
        );
    }
    t.put(AUTOSTART, "[Desktop Entry]\nName=no exec\n");
    assert!(why_kept(&plan_with_path(&[]), AUTOSTART).contains("no Exec line"));
}

/// A build tree or an unpacked archive is not an install, however it was
/// reached — the autostart Exec that `cargo run` plus start-at-login writes,
/// the menu entry's, --bindir — and the entries that point there stay, since
/// they run a copy that stays.
#[test]
fn entries_into_a_build_tree_or_an_unpacked_archive_keep_them_and_it() {
    let tarball = "/home/u/Downloads/tobii-linux-0.3.0-x86_64-unknown-linux-gnu";
    for (tag, dir, extra) in [
        (
            "entry-build",
            "/home/u/src/TobiiLinux/target/release",
            vec![],
        ),
        (
            "entry-cross",
            "/home/u/src/TobiiLinux/target/x86_64-unknown-linux-gnu/release",
            vec![(
                "/home/u/src/TobiiLinux/target/CACHEDIR.TAG".to_string(),
                CARGO_TAG,
            )],
        ),
        (
            "entry-tarball",
            tarball,
            vec![
                (format!("{tarball}/install.sh"), "#!/bin/sh\n"),
                (
                    format!("{tarball}/assets/install-payload.sh"),
                    "#!/bin/sh\n",
                ),
            ],
        ),
    ] {
        let t = Tree::new(tag);
        t.bin(&format!("{dir}/tobii"), "tobii 0.3.0");
        t.bin(&format!("{dir}/tobii-gtk"), "tobii-gtk 0.3.0");
        for (path, content) in &extra {
            t.put(path, content);
        }
        t.put(ENTRY, &menu_entry(&format!("{dir}/tobii-gtk")));
        t.put(ICON, "<svg/>");
        t.put(
            AUTOSTART,
            &autostart::entry_text(&format!("{dir}/tobii-gtk")),
        );
        for opts in [
            Options::default(),
            Options {
                bindirs: vec![dir.into()],
                ..Options::default()
            },
        ] {
            let p = plan_in(&t, &user(), &opts, &unowned);
            assert!(removals(&p).is_empty(), "{tag}: {:#?}", removals(&p));
            let loc = location(&p, dir);
            assert!(
                matches!(loc.verdict, Verdict::NotAnInstall(_)),
                "{tag}: {loc:?}"
            );
            assert!(loc.via.contains(&Via::Autostart) && loc.via.contains(&Via::MenuEntry));
            for e in [ENTRY, AUTOSTART, ICON] {
                assert!(
                    kept(&p).contains(&e.to_string()),
                    "{tag}: {e} {:#?}",
                    p.kept
                );
            }
            assert!(why_kept(&p, AUTOSTART).contains("not being removed"));
        }

        // The one thing that overrides it: this mode's own manifest says an
        // install was made there.
        t.put(MANIFEST, &format!("bindir={dir}\n"));
        let p = plan_in(&t, &user(), &Options::default(), &unowned);
        assert_eq!(location(&p, dir).verdict, Verdict::Remove, "{tag}");
        assert!(removals(&p).contains(&AUTOSTART.to_string()), "{tag}");
    }
}

/// `~/.local/bin/tobii-gtk -> /usr/bin/tobii-gtk`: removing the link leaves
/// the packaged hub in place, so an entry that runs it stays, and a hub
/// running it is not stopped.
#[test]
fn a_symlinked_binary_loses_only_the_link_and_what_it_points_at_stays() {
    let t = Tree::new("symlink");
    t.bin("/usr/bin/tobii-gtk", "tobii-gtk 0.3.0");
    t.bin(&format!("{BIN}/tobii"), "tobii 0.3.0");
    t.symlink("/usr/bin/tobii-gtk", &format!("{BIN}/tobii-gtk"));
    t.put(ENTRY, &menu_entry(&format!("{BIN}/tobii-gtk")));
    t.put(AUTOSTART, &autostart::entry_text("/usr/bin/tobii-gtk"));
    let procs = [
        // The kernel reports the resolved path: the packaged hub.
        proc(
            20,
            me(),
            "/usr/bin/tobii-gtk",
            &["tobii-gtk", "--background"],
        ),
        proc(21, me(), &format!("{BIN}/tobii"), &["tobii", "stream"]),
    ];
    let p = plan_procs(
        &t,
        &user(),
        &Options::default(),
        &pacman_owns_usr_bin,
        &procs,
    );
    let r = removals(&p);
    assert!(r.contains(&format!("{BIN}/tobii-gtk")), "{r:#?}");
    assert!(r.contains(&format!("{BIN}/tobii")), "{r:#?}");
    assert!(!r.iter().any(|x| x.starts_with("/usr")), "{r:#?}");
    // The menu entry runs the link, which goes; the autostart entry runs the
    // packaged hub, which stays.
    assert!(why_removed(&p, ENTRY).contains("being removed"));
    assert!(why_kept(&p, AUTOSTART).contains("/usr/bin/tobii-gtk"));
    assert_eq!(p.stop.iter().map(|x| x.pid).collect::<Vec<_>>(), [21]);
    assert_eq!(p.other_hubs.iter().map(|x| x.pid).collect::<Vec<_>>(), [20]);

    let out = execute(&p);
    assert!(out.failed.is_empty(), "{:?}", out.failed);
    assert!(!t.has(&format!("{BIN}/tobii-gtk")));
    assert!(t.has("/usr/bin/tobii-gtk"), "the link's target stays");

    // The other way round: an entry runs a link whose TARGET is being
    // removed, so it runs nothing afterwards and goes. (The link's own
    // directory is not writable here, so the link itself stays.)
    let t = Tree::new("symlink-to-removed");
    v030_install(&t);
    t.symlink(&format!("{BIN}/tobii-gtk"), "/opt/links/tobii-gtk");
    t.put(AUTOSTART, &autostart::entry_text("/opt/links/tobii-gtk"));
    let writable = |d: &Path| !d.ends_with("opt/links");
    let p = plan_full(
        &t,
        &user(),
        &Options::default(),
        &unowned,
        &writable,
        &by_content,
        &[],
    );
    assert!(!removals(&p).contains(&"/opt/links/tobii-gtk".to_string()));
    assert!(why_removed(&p, AUTOSTART).contains("being removed"));
}

fn populate_config_and_state(t: &Tree, cfg: &str, state: &str) {
    for f in [
        "config.toml",
        "calibration.bin",
        "calibration.meta.toml",
        "games.toml",
        "enabled_eye",
        "report_salt",
        "accuracy.csv",
        // Hand-made, by a person, on this machine. No code writes these.
        "config.toml.bak-offsetz",
        "calibration.bin.baseline-20260810",
    ] {
        t.put(&format!("{cfg}/{f}"), "x");
    }
    t.put(&format!("{cfg}/models/head-pose-0.5-small.onnx"), "m");
    t.put(&format!("{cfg}/models/head-localizer.onnx.partial"), "m");
    t.put(&format!("{cfg}/models/my-own.onnx"), "m");
    t.put(&format!("{state}/tobii.log"), "log");
    t.put(&format!("{state}/diagnostics.txt"), "old report");
}

#[test]
fn the_default_plan_leaves_settings_and_state_alone() {
    let t = Tree::new("default-no-purge");
    v030_install(&t);
    let env = Env {
        xdg_config_home: Some("/home/u/cfg".into()),
        xdg_state_home: Some("/home/u/st".into()),
        ..user()
    };
    populate_config_and_state(&t, "/home/u/cfg/tobii-linux", "/home/u/st/tobii-linux");

    let p = plan_in(&t, &env, &Options::default(), &unowned);
    assert!(!removals(&p).is_empty());
    for r in removals(&p)
        .into_iter()
        .chain(p.rmdirs.iter().map(|d| d.display().to_string()))
    {
        assert!(
            !r.starts_with("/home/u/cfg/tobii-linux") && !r.starts_with("/home/u/st/tobii-linux"),
            "{r} must not be touched without --purge"
        );
    }
}

#[test]
fn purge_removes_known_names_only_and_reports_the_rest() {
    let t = Tree::new("purge");
    let cfg = "/home/u/.config/tobii-linux";
    let state = "/home/u/.local/state/tobii-linux";
    populate_config_and_state(&t, cfg, state);
    let opts = Options {
        purge: true,
        ..Options::default()
    };

    let p = plan_in(&t, &user(), &opts, &unowned);
    let r = removals(&p);
    for want in [
        "calibration.bin",
        "calibration.meta.toml",
        "games.toml",
        "config.toml",
        "report_salt",
        "accuracy.csv",
        "models/head-pose-0.5-small.onnx",
        "models/head-localizer.onnx.partial",
    ] {
        assert!(
            r.contains(&format!("{cfg}/{want}")),
            "{want} missing: {r:#?}"
        );
    }
    assert!(r.contains(&format!("{state}/tobii.log")));
    assert!(r.contains(&format!("{state}/diagnostics.txt")));

    for keep in [
        "config.toml.bak-offsetz",
        "calibration.bin.baseline-20260810",
        "models/my-own.onnx",
    ] {
        let path = format!("{cfg}/{keep}");
        assert!(!r.contains(&path), "{keep} must survive --purge");
        assert!(kept(&p).contains(&path), "{keep} must be reported");
    }
    // The models directory is purged on its own, so the config directory's
    // pass does not report it as something of the user's.
    assert!(
        !kept(&p).contains(&format!("{cfg}/models")),
        "{:#?}",
        p.kept
    );
    // Directories go only if empty, and never as a tree.
    let dirs: Vec<String> = p.rmdirs.iter().map(|d| d.display().to_string()).collect();
    let models = dirs.iter().position(|d| d == &format!("{cfg}/models"));
    let config = dirs.iter().position(|d| d == cfg);
    assert!(models < config && models.is_some(), "{dirs:?}");
    assert!(
        p.remove.iter().all(|r| !r.tree),
        "no tree removal in a purge"
    );
    // Told what it costs before it happens.
    assert!(p.warnings.iter().any(|w| w.contains("calibration.bin")));
}

#[test]
fn a_package_managed_copy_is_left_to_its_package_manager() {
    let t = Tree::new("package");
    v030_install(&t);
    for (manager, command) in [
        ("pacman", "pacman -R tobii-linux"),
        ("dpkg", "apt remove tobii-linux"),
        ("rpm", "dnf remove tobii-linux"),
    ] {
        let owner = |_: &Path| Ownership::Package {
            manager: manager.into(),
            package: "tobii-linux".into(),
        };
        let p = plan_in(&t, &user(), &Options::default(), &owner);
        assert!(p.refusal.is_none());
        for r in removals(&p) {
            assert!(!r.starts_with(BIN), "{r} belongs to {manager}");
        }
        let hint = p
            .hints
            .iter()
            .find(|h| h.contains(BIN))
            .expect("a hint for the package");
        assert!(hint.contains(manager), "{hint}");
        assert!(hint.contains(command), "{hint}");
        assert!(
            p.hints.iter().all(|h| !h.contains("sudo rm")),
            "{:?}",
            p.hints
        );
        // The menu entry still runs the packaged copy, so it stays — and so
        // does the icon it shows.
        assert!(kept(&p).contains(&ENTRY.to_string()));
        assert!(kept(&p).contains(&ICON.to_string()), "{:#?}", p.kept);
        assert!(!removals(&p).contains(&ICON.to_string()));
    }
}

#[test]
fn an_ownership_question_that_could_not_be_answered_refuses_everything() {
    let t = Tree::new("unknown");
    v030_install(&t);
    let owner = |_: &Path| Ownership::Unknown {
        manager: "dpkg".into(),
        why: "it exited with 2".into(),
    };
    let p = plan_in(&t, &user(), &Options::default(), &owner);
    let why = p.refusal.as_deref().expect("refused");
    assert!(why.contains("dpkg"), "{why}");
    assert!(p.remove.is_empty() && p.self_exe.is_none() && p.manifests.is_empty());
}

#[test]
fn icon_and_mime_caches_are_never_removed_and_only_an_existing_cache_is_refreshed() {
    let t = Tree::new("caches");
    v030_install(&t);
    t.put("/home/u/.local/share/icons/hicolor/icon-theme.cache", "c");
    t.put("/home/u/.local/share/applications/mimeinfo.cache", "c");
    let opts = Options {
        purge: true,
        ..Options::default()
    };
    let p = plan_in(&t, &user(), &opts, &unowned);
    for r in removals(&p) {
        assert!(
            !r.ends_with("icon-theme.cache") && !r.ends_with("mimeinfo.cache"),
            "{r}"
        );
    }
    assert_eq!(
        p.icon_caches,
        vec![PathBuf::from("/home/u/.local/share/icons/hicolor")]
    );

    // No cache: nothing to refresh, and so nothing that could create one.
    let t = Tree::new("no-cache");
    v030_install(&t);
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert!(p.icon_caches.is_empty());
}

/// The guard itself: whatever names one of the two caches, it is not planned.
#[test]
fn the_caches_other_programs_share_can_never_be_named_for_removal() {
    let mut p = Plan::default();
    for cache in NEVER_REMOVE {
        for dir in [
            "/home/u/.local/share/icons/hicolor",
            "/home/u/.local/share/applications",
        ] {
            p.push_remove(PathBuf::from(format!("{dir}/{cache}")), "a bug", false);
        }
    }
    assert!(p.remove.is_empty(), "{:#?}", p.remove);
    p.push_remove(PathBuf::from("/home/u/other"), "not a cache", false);
    assert_eq!(p.remove.len(), 1);
}

#[test]
fn updater_scratch_goes_and_an_updated_binary_is_still_removed() {
    let t = Tree::new("scratch");
    // Listed in the manifest, and replaced by the updater since: different
    // bytes, a different version, even a binary that no longer answers at all.
    // The manifest is trusted; nothing is compared against what was installed.
    t.put(
        MANIFEST,
        "# written by install-payload.sh\nbindir=/home/u/.local/bin\n",
    );
    t.bin(&format!("{BIN}/tobii"), "tobii 0.4.0");
    t.bin(&format!("{BIN}/tobii-gtk"), "");
    t.mkdir(&format!("{BIN}/.tobii-update-123"));
    t.put(&format!("{BIN}/.tobii-update-123/archive.tar.gz"), "a");
    for f in [
        ".tobii-update-probe-123",
        ".tobii.new-55",
        ".tobii-gtk.new-56",
        ".tobii.old-77",
        ".tobii-gtk.old-78",
        // Not ours, by shape.
        ".tobii-update-notes",
        ".tobii.new-",
        ".tobii.new-5x",
        "other-tool",
    ] {
        t.put(&format!("{BIN}/{f}"), "s");
    }
    // The installer's manifest temporary, left by an interrupted install.
    let data = "/home/u/.local/share/tobii-linux";
    for f in ["installs.new-4321", "installs.new-", "installs.new-12x"] {
        t.put(&format!("{data}/{f}"), "bindir=/x\n");
    }

    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    let r = removals(&p);
    assert!(r.contains(&format!("{BIN}/tobii")));
    assert!(r.contains(&format!("{BIN}/tobii-gtk")));
    // Trusted by the manifest, not run.
    assert!(location(&p, BIN)
        .binaries
        .iter()
        .all(|b| b.ident == Ident::Listed));
    for s in [
        ".tobii-update-probe-123",
        ".tobii.new-55",
        ".tobii-gtk.new-56",
        ".tobii.old-77",
        ".tobii-gtk.old-78",
    ] {
        assert!(r.contains(&format!("{BIN}/{s}")), "{s} missing: {r:#?}");
    }
    let work = p
        .remove
        .iter()
        .find(|x| x.path == Path::new(&format!("{BIN}/.tobii-update-123")))
        .expect("the work dir");
    assert!(
        work.tree,
        "the updater's work dir is removed with its contents"
    );
    for s in [
        ".tobii-update-notes",
        ".tobii.new-",
        ".tobii.new-5x",
        "other-tool",
    ] {
        assert!(!r.contains(&format!("{BIN}/{s}")), "{s} is not ours");
    }
    // Everything the manifest pointed at is going, so the manifest goes too,
    // and the temporary that would keep its directory.
    assert_eq!(p.manifests.len(), 1);
    assert!(p.manifests[0].then_empty);
    assert!(p.rmdirs.contains(&PathBuf::from(data)));
    assert!(r.contains(&format!("{data}/installs.new-4321")), "{r:#?}");
    assert!(!r.contains(&format!("{data}/installs.new-")));
    assert!(!r.contains(&format!("{data}/installs.new-12x")));
    let text = render(&p, &Options::default());
    assert!(
        text.contains("listed in the install manifest; not identified"),
        "{text}"
    );

    // An INFERRED copy that was updated is found and removed the same way —
    // it is identified by name, not by version.
    let t = Tree::new("scratch-inferred");
    v030_install(&t);
    t.bin(&format!("{BIN}/tobii"), "tobii 0.9.9");
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert!(removals(&p).contains(&format!("{BIN}/tobii")));
}

/// The manifest says where to look. A name there that is not a program is
/// not something the installer put there.
#[test]
fn a_manifest_listed_name_that_is_not_a_program_is_kept() {
    let t = Tree::new("not-a-program");
    t.put(MANIFEST, "bindir=/home/u/.local/bin\n");
    t.put(&format!("{BIN}/tobii"), "tobii 0.4.0\n"); // mode 644, text
    t.bin(&format!("{BIN}/tobii-gtk"), "tobii-gtk 0.4.0");
    let never = |p: &Path| -> Option<String> { panic!("{p:?} is listed and must not be run") };
    let p = plan_full(
        &t,
        &user(),
        &Options::default(),
        &unowned,
        &|_| true,
        &never,
        &[],
    );
    let r = removals(&p);
    assert!(r.contains(&format!("{BIN}/tobii-gtk")), "{r:#?}");
    assert!(!r.contains(&format!("{BIN}/tobii")), "{r:#?}");
    assert!(why_kept(&p, &format!("{BIN}/tobii")).contains("not executable"));
    // Its line stays: that directory is not empty of this program.
    assert!(p.manifests.is_empty(), "{:#?}", p.manifests);

    // Executable, but a script rather than a program: kept too.
    t.put(&format!("{BIN}/tobii"), "#!/bin/sh\necho tobii 0.4.0\n");
    t.chmod(&format!("{BIN}/tobii"), 0o755);
    let p = plan_full(
        &t,
        &user(),
        &Options::default(),
        &unowned,
        &|_| true,
        &never,
        &[],
    );
    assert!(why_kept(&p, &format!("{BIN}/tobii")).contains("a script"));
    let text = render(&p, &Options::default());
    assert!(
        text.contains("listed in the install manifest, but not an executable program"),
        "{text}"
    );
}

#[test]
fn root_is_refused_and_roots_leftovers_are_only_ever_a_hint() {
    let t = Tree::new("root");
    v030_install(&t);
    let as_root = Env {
        euid: 0,
        home: Some("/root".into()),
        sudo_user: Some("u".into()),
        ..user()
    };
    let p = plan_in(&t, &as_root, &Options::default(), &unowned);
    let why = p.refusal.as_deref().expect("refused under sudo");
    assert!(why.contains("/root") && why.contains("--system"), "{why}");
    assert!(p.remove.is_empty());
    // Printed once, by the caller: the plan text does not repeat it.
    let text = render(&p, &Options::default());
    assert!(!text.contains("root's home"), "{text}");

    // --system is for root only.
    let sys = Options {
        system: true,
        ..Options::default()
    };
    assert!(plan_in(&t, &user(), &sys, &unowned).refusal.is_some());

    // A past `sudo ./install.sh` left a copy in /root.
    t.bin("/root/.local/bin/tobii", "tobii 0.3.0");
    t.bin("/root/.local/bin/tobii-gtk", "tobii-gtk 0.3.0");
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    for r in removals(&p) {
        assert!(!r.starts_with("/root"), "{r} is not the user's to remove");
    }
    let hint = p
        .hints
        .iter()
        .find(|h| h.contains("/root"))
        .expect("a hint");
    assert!(
        hint.contains(
            "sudo rm -f /root/.local/bin/tobii /root/.local/bin/tobii-gtk \
             /root/.local/share/applications/com.tobiilinux.Configuration.desktop \
             /root/.local/share/icons/hicolor/scalable/apps/com.tobiilinux.Configuration.svg"
        ),
        "{hint}"
    );
}

#[test]
fn a_system_install_is_planned_only_with_system_and_only_from_its_own_places() {
    let t = Tree::new("system");
    v030_install(&t); // a home install that --system must not touch
    t.put(
        "/usr/local/share/tobii-linux/installs",
        "bindir=/usr/local/bin\n",
    );
    t.bin("/usr/local/bin/tobii", "tobii 0.4.0");
    t.bin("/usr/local/bin/tobii-gtk", "tobii-gtk 0.4.0");
    t.put(
        "/usr/local/share/applications/com.tobiilinux.Configuration.desktop",
        "[Desktop Entry]\nExec=/usr/local/bin/tobii-gtk\n",
    );
    t.put(
        "/usr/local/share/icons/hicolor/scalable/apps/com.tobiilinux.Configuration.svg",
        "<svg/>",
    );

    // Seen from the user's own run: a hint, never a plan.
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert!(removals(&p).iter().all(|r| !r.starts_with("/usr/local")));
    assert!(p
        .hints
        .iter()
        .any(|h| h.contains("uninstall --system --bindir /usr/local/bin")));

    let root = Env {
        euid: 0,
        home: Some("/root".into()),
        ..user()
    };
    let sys = Options {
        system: true,
        ..Options::default()
    };
    // What `sudo ./install.sh --system` makes is root's. (A listed binary
    // that is not root's is another user's, and is kept.)
    let fs = Fs {
        root: &t.root,
        owner: |_, _| 0,
    };
    let p = plan_with_fs(&root, &sys, fs, &home_probes(&by_content), &[]);
    assert!(p.refusal.is_none(), "{:?}", p.refusal);
    let r = removals(&p);
    for want in [
        "/usr/local/bin/tobii",
        "/usr/local/bin/tobii-gtk",
        "/usr/local/share/applications/com.tobiilinux.Configuration.desktop",
        "/usr/local/share/icons/hicolor/scalable/apps/com.tobiilinux.Configuration.svg",
    ] {
        assert!(r.contains(&want.to_string()), "{want}: {r:#?}");
    }
    assert!(r.iter().all(|x| !x.starts_with("/home")), "{r:#?}");
    assert!(p.manifests[0].then_empty);
}

/// As root, identifying a binary means running it as root. A directory that
/// someone other than root can write to decides what that runs — and
/// nothing in it is so much as opened: the script there is not read to find
/// out it is one.
#[test]
fn as_root_a_bindir_someone_else_can_write_to_is_never_run() {
    let t = Tree::new("root-bindir");
    t.bin("/opt/t/tobii", "tobii 0.3.0");
    t.put("/opt/t/tobii-gtk", "#!/bin/sh\necho tobii-gtk 0.3.0\n");
    t.chmod("/opt/t/tobii-gtk", 0o755);
    t.chmod("/opt/t", 0o777);
    checked();
    let root = Env {
        euid: 0,
        home: Some("/root".into()),
        ..user()
    };
    let opts = Options {
        system: true,
        bindirs: vec!["/opt/t".into()],
        ..Options::default()
    };
    let never = |p: &Path| -> Option<String> { panic!("{p:?} was run as root") };
    let p = plan_full(&t, &root, &opts, &unowned, &|_| true, &never, &[]);
    assert!(p.refusal.is_none(), "{:?}", p.refusal);
    assert!(removals(&p).is_empty(), "{:#?}", removals(&p));
    let loc = location(&p, "/opt/t");
    let Verdict::NotRunAsRoot(why) = &loc.verdict else {
        panic!("{loc:?}");
    };
    assert!(
        why.contains("/opt/t") && why.contains("not run as root"),
        "{why}"
    );
    assert!(kept(&p).contains(&"/opt/t/tobii".to_string()));
    assert!(
        loc.binaries.iter().all(|b| b.ident == Ident::NotRun),
        "{:?}",
        loc.binaries
    );
    assert!(checked().is_empty(), "opened as root");
}

#[test]
fn the_udev_step_names_only_the_rules_in_etc() {
    let t = Tree::new("udev");
    for f in [
        "/etc/udev/rules.d/60-tobii.rules",
        "/etc/udev/rules.d/99-tobii.rules",
        "/etc/udev/rules.d/70-someone-else.rules",
        "/usr/lib/udev/rules.d/60-tobii.rules",
    ] {
        t.put(f, "rule");
    }
    let opts = Options {
        udev: true,
        ..Options::default()
    };
    let p = plan_in(&t, &user(), &opts, &unowned);
    assert_eq!(
        p.udev,
        vec![
            PathBuf::from("/etc/udev/rules.d/60-tobii.rules"),
            PathBuf::from("/etc/udev/rules.d/99-tobii.rules"),
        ]
    );
    let cmds = udev_commands(&p, 1000);
    let all = cmds
        .iter()
        .map(|c| c.join(" "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!all.contains("/usr/lib"), "{all}");
    assert!(!all.contains("70-someone-else"), "{all}");
    assert_eq!(
        cmds[0].join(" "),
        "sudo rm -f /etc/udev/rules.d/60-tobii.rules /etc/udev/rules.d/99-tobii.rules"
    );
    assert!(all.contains("sudo udevadm trigger --subsystem-match=usb"));
    assert!(all.contains("sudo udevadm trigger --subsystem-match=misc"));
    // The package's rule is what takes over, and the user is told so.
    assert!(p.udev_notes.iter().any(|n| n.contains(PACKAGE_UDEV_RULE)));
    // Nothing in the removal list: the udev step is its own, separate step.
    assert!(removals(&p).iter().all(|r| !r.contains("udev")));
}

#[test]
fn a_build_tree_or_an_unpacked_archive_is_not_an_install() {
    for (tag, dir, extra) in [
        ("build", "/home/u/src/TobiiLinux/target/release", None),
        (
            "tarball",
            "/home/u/Downloads/tobii-linux-0.3.0-x86_64-unknown-linux-gnu",
            Some("install.sh"),
        ),
    ] {
        let t = Tree::new(tag);
        t.bin(&format!("{dir}/tobii"), "tobii 0.3.0");
        t.bin(&format!("{dir}/tobii-gtk"), "tobii-gtk 0.3.0");
        if let Some(e) = extra {
            t.put(&format!("{dir}/{e}"), "#!/bin/sh\n");
            t.put(&format!("{dir}/assets/install-payload.sh"), "#!/bin/sh\n");
        }
        let env = Env {
            current_exe: Some(format!("{dir}/tobii").into()),
            ..user()
        };
        let p = plan_in(&t, &env, &Options::default(), &unowned);
        assert!(removals(&p).is_empty(), "{tag}: {:#?}", removals(&p));
        assert!(
            p.locations
                .iter()
                .any(|l| matches!(l.verdict, Verdict::NotAnInstall(_))),
            "{tag}: {:#?}",
            p.locations
        );
    }
}

/// Every `--lean` install from v0.1.0 to v0.3.0: two binaries at most, no
/// manifest, no menu entry. Run from the release archive's `uninstall.sh` —
/// an unpacked archive, which is not an install — it must still be found.
#[test]
fn a_lean_install_is_found_from_an_unpacked_archive() {
    let t = Tree::new("lean");
    t.bin(&format!("{BIN}/tobii"), "tobii 0.3.0");
    let archive = "/home/u/Downloads/tobii-linux-0.4.0-x86_64-unknown-linux-gnu";
    t.bin(&format!("{archive}/tobii"), "tobii 0.4.0");
    t.put(&format!("{archive}/install.sh"), "#!/bin/sh\n");
    t.put(&format!("{archive}/uninstall.sh"), "#!/bin/sh\n");
    t.put(
        &format!("{archive}/assets/install-payload.sh"),
        "#!/bin/sh\n",
    );
    let env = Env {
        current_exe: Some(format!("{archive}/tobii").into()),
        ..user()
    };
    let p = plan_in(&t, &env, &Options::default(), &unowned);
    assert_eq!(removals(&p), [format!("{BIN}/tobii")]);
    let loc = location(&p, BIN);
    assert_eq!(loc.via, [Via::DefaultDir]);
    assert!(matches!(
        location(&p, archive).verdict,
        Verdict::NotAnInstall(_)
    ));
}

/// Only Cargo's own output directories are build trees: told by the text of
/// the CACHEDIR.TAG Cargo writes one or two levels above its binaries.
/// Something merely kept under a target directory — a HOME made up for a
/// test, as the tarball-route check does — is looked at like anywhere else.
#[test]
fn only_cargos_output_directories_count_as_a_build_tree() {
    let t = Tree::new("under-target");
    let home = "/work/target/uninstall-test/home";
    t.put("/work/target/CACHEDIR.TAG", CARGO_TAG);
    t.bin(&format!("{home}/.local/bin/tobii"), "tobii 0.3.0");
    let built = [
        "/work/target/profiling",
        "/work/target/x86_64-unknown-linux-gnu/release",
    ];
    for dir in built {
        t.bin(&format!("{dir}/tobii"), "tobii 0.3.0");
    }
    let env = Env {
        home: Some(home.into()),
        ..user()
    };
    let opts = Options {
        bindirs: built.iter().map(PathBuf::from).collect(),
        ..Options::default()
    };
    let p = plan_in(&t, &env, &opts, &unowned);
    assert_eq!(removals(&p), [format!("{home}/.local/bin/tobii")]);
    for dir in built {
        assert_eq!(
            location(&p, dir).verdict,
            Verdict::NotAnInstall("a Cargo build directory"),
            "{dir}"
        );
    }
}

#[test]
fn the_running_binary_is_removed_last() {
    let t = Tree::new("self");
    v030_install(&t);
    let env = Env {
        current_exe: Some(format!("{BIN}/tobii").into()),
        ..user()
    };
    let p = plan_in(&t, &env, &Options::default(), &unowned);
    assert_eq!(p.self_exe, Some(PathBuf::from(format!("{BIN}/tobii"))));
    assert!(p
        .remove
        .iter()
        .all(|r| r.path != Path::new(&format!("{BIN}/tobii"))));
    let loc = location(&p, BIN);
    assert!(loc.via.contains(&Via::ThisProgram) && loc.via.contains(&Via::MenuEntry));
}

#[test]
fn the_process_scan_skips_itself_and_its_parent_and_sees_deleted_binaries() {
    let t = Tree::new("procs");
    v030_install(&t);
    t.bin("/usr/bin/tobii-gtk", "tobii-gtk 0.3.0");
    let procs = [
        proc(4242, me(), &format!("{BIN}/tobii"), &["tobii", "uninstall"]),
        proc(4241, me(), &format!("{BIN}/tobii-gtk"), &["tobii-gtk"]),
        // Started before the updater replaced the file.
        proc(
            10,
            me(),
            &format!("{BIN}/tobii-gtk (deleted)"),
            &["tobii-gtk", "--background"],
        ),
        proc(
            11,
            me(),
            &format!("{BIN}/tobii"),
            &["tobii", "game", "--", "game.exe"],
        ),
        proc(12, me() + 1, &format!("{BIN}/tobii-gtk"), &["tobii-gtk"]),
        proc(13, me(), "/usr/bin/tobii-gtk", &["tobii-gtk"]),
        proc(14, me(), "/usr/bin/bash", &["bash"]),
    ];
    let p = plan_procs(
        &t,
        &user(),
        &Options::default(),
        &pacman_owns_usr_bin,
        &procs,
    );
    let pids: Vec<u32> = p.stop.iter().map(|x| x.pid).collect();
    assert_eq!(pids, vec![10, 11]);
    // Another hub is running from somewhere that stays: D-Bus is not asked.
    assert_eq!(p.other_hubs.iter().map(|x| x.pid).collect::<Vec<_>>(), [13]);
    assert!(p.orphan_hubs.is_empty());
    let game = describe_proc(&p.stop[1]);
    assert!(game.contains("running game's head tracking"), "{game}");
    // What agreeing to --yes means is said where it applies.
    let text = render(&p, &Options::default());
    assert!(text.contains("SIGTERM only if you agree"), "{text}");
    let yes = Options {
        yes: true,
        ..Options::default()
    };
    assert!(render(&p, &yes).contains("--yes agrees to that"));
    assert!(USAGE.contains("--yes also stops running copies with SIGTERM if they do not quit"));
}

/// A package removed while its hub ran leaves a hub running from a file that
/// is gone. Nothing of it is left to remove, but it holds the tracker and the
/// hub's name, so it is shown and offered to be stopped — not called "a hub
/// from a copy that stays".
#[test]
fn a_hub_whose_program_was_deleted_is_an_orphan_offered_to_be_stopped() {
    let t = Tree::new("orphan");
    v030_install(&t);
    t.put(AUTOSTART, &autostart::entry_text("/usr/bin/tobii-gtk"));
    let orphan = proc(
        30,
        me(),
        "/usr/bin/tobii-gtk (deleted)",
        &["/usr/bin/tobii-gtk", "--background"],
    );
    let p = plan_procs(
        &t,
        &user(),
        &Options::default(),
        &unowned,
        std::slice::from_ref(&orphan),
    );
    assert_eq!(p.orphan_hubs, std::slice::from_ref(&orphan));
    assert!(p.other_hubs.is_empty() && p.stop.is_empty());
    let text = render(&p, &Options::default());
    for want in [
        "pid 30",
        "/usr/bin/tobii-gtk (deleted)",
        "/usr/bin/tobii-gtk --background",
        "whichever hub owns the name",
    ] {
        assert!(text.contains(want), "{want}: {text}");
    }

    // One of our copies runs too: still an orphan, and D-Bus may be asked.
    let ours = proc(31, me(), &format!("{BIN}/tobii-gtk"), &["tobii-gtk"]);
    let p = plan_procs(
        &t,
        &user(),
        &Options::default(),
        &unowned,
        &[orphan.clone(), ours],
    );
    assert_eq!(p.stop.iter().map(|x| x.pid).collect::<Vec<_>>(), [31]);
    assert_eq!(
        p.orphan_hubs.iter().map(|x| x.pid).collect::<Vec<_>>(),
        [30]
    );
    let text = render(&p, &Options::default());
    assert!(
        !text.contains("a copy that stays is also running"),
        "{text}"
    );

    // The same path with a program there again (a package upgrade replaced
    // it): a hub from a copy that stays, as before.
    t.bin("/usr/bin/tobii-gtk", "tobii-gtk 0.4.0");
    let p = plan_procs(
        &t,
        &user(),
        &Options::default(),
        &pacman_owns_usr_bin,
        std::slice::from_ref(&orphan),
    );
    assert!(p.orphan_hubs.is_empty());
    assert_eq!(p.other_hubs, [orphan]);
}

/// Checked right before a signal: a pid the kernel has given to something
/// else since the scan must not be signalled.
#[test]
fn a_process_is_told_apart_from_a_later_one_with_the_same_pid() {
    // The command name may hold spaces and parentheses of its own.
    let stat = "1234 (tobii (gtk) x) S 1 1234 1234 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 4 0 \
                987654 123456 45 18446744073709551615";
    assert_eq!(parse_start_time(stat), Some(987654));
    assert_eq!(parse_start_time("1234 (x S 1"), None);

    let pid = std::process::id();
    let exe = std::fs::read_link("/proc/self/exe")
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let me = Proc {
        pid,
        uid: 0,
        exe,
        args: Vec::new(),
        start_time: start_time(pid),
    };
    assert!(me.start_time.is_some());
    assert!(safe_to_signal(&me));
    let later = Proc {
        start_time: me.start_time.map(|t| t + 1),
        ..me.clone()
    };
    assert!(!still_running(&later) && !safe_to_signal(&later));
    let other_program = Proc {
        exe: "/usr/bin/something-else".into(),
        ..me.clone()
    };
    assert!(!safe_to_signal(&other_program));
    // No start time from the scan: cannot be proved the same, so no signal.
    let unknown = Proc {
        start_time: None,
        ..me
    };
    assert!(!safe_to_signal(&unknown));
}

#[test]
fn the_bridge_is_found_in_steam_prefixes_wineprefix_and_dot_wine() {
    let t = Tree::new("wine");
    v030_install(&t);
    t.mkdir("/home/u/.wine/drive_c/tobii-bridge");
    t.mkdir("/home/u/.steam/steam/steamapps/compatdata/359320/pfx/drive_c/tobii-bridge");
    t.mkdir("/home/u/.steam/steam/steamapps/compatdata/400/pfx/drive_c");
    t.mkdir("/home/u/games/wp/drive_c/tobii-bridge");
    t.mkdir("/home/u/other/drive_c");
    let env = Env {
        wineprefix: Some("/home/u/games/wp".into()),
        ..user()
    };
    let p = plan_in(&t, &env, &Options::default(), &unowned);
    assert_eq!(
        p.wine_prefixes,
        [
            PathBuf::from("/home/u/.steam/steam/steamapps/compatdata/359320/pfx"),
            PathBuf::from("/home/u/.wine"),
            PathBuf::from("/home/u/games/wp"),
        ]
    );
    // Without WINEPREFIX, ~/.wine is still found.
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert_eq!(p.wine_prefixes.len(), 2, "{:?}", p.wine_prefixes);

    // Removing the bridge needs a tobii. When this run removes the one
    // running it, that is said in the plan, asked before anything is
    // removed, and repeated at the end.
    let installed = Env {
        current_exe: Some(format!("{BIN}/tobii").into()),
        ..user()
    };
    let p = plan_in(&t, &installed, &Options::default(), &unowned);
    assert!(p.self_exe.is_some());
    let text = render(&p, &Options::default());
    assert!(
        text.contains("this run removes `tobii` — so run these first"),
        "{text}"
    );
    let q = bridge_question(&p).expect("asked");
    assert!(
        q.contains("2 Wine prefixes") && q.contains("Stop here"),
        "{q}"
    );
    let out = execute(&p);
    let s = summary(&p, &out);
    assert!(
        s.contains("./tobii bridge uninstall --prefix /home/u/.wine"),
        "{s}"
    );
    assert!(s.contains("release archive"), "{s}");

    // From the release archive's uninstall.sh, the running tobii is the
    // archive's, and it stays: it can remove the bridge before or after, so
    // nothing is asked and nothing says to do it first.
    let t = Tree::new("wine-archive");
    v030_install(&t);
    t.mkdir("/home/u/.wine/drive_c/tobii-bridge");
    let archive = "/home/u/Downloads/tobii-linux-0.4.0-x86_64-unknown-linux-gnu";
    t.bin(&format!("{archive}/tobii"), "tobii 0.4.0");
    t.put(&format!("{archive}/install.sh"), "#!/bin/sh\n");
    t.put(
        &format!("{archive}/assets/install-payload.sh"),
        "#!/bin/sh\n",
    );
    let from_archive = Env {
        current_exe: Some(format!("{archive}/tobii").into()),
        ..user()
    };
    let p = plan_in(&t, &from_archive, &Options::default(), &unowned);
    assert!(p.self_exe.is_none());
    assert!(removals(&p).contains(&format!("{BIN}/tobii")));
    assert!(bridge_question(&p).is_none());
    let command = format!("{archive}/tobii bridge uninstall --prefix /home/u/.wine");
    let text = render(&p, &Options::default());
    assert!(!text.contains("run these first"), "{text}");
    assert!(text.contains("before or after"), "{text}");
    assert!(text.contains(&command), "{text}");
    let out = execute(&p);
    let s = summary(&p, &out);
    assert!(s.contains(&command), "{s}");
    assert!(!s.contains("./tobii bridge"), "{s}");
    assert!(t.has(&format!("{archive}/tobii")));

    // Nothing to remove the bridge with is being removed: nothing to ask.
    let t = Tree::new("wine-no-tobii");
    t.mkdir("/home/u/.wine/drive_c/tobii-bridge");
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert!(bridge_question(&p).is_none());
}

#[test]
fn options_parse_and_a_relative_bindir_is_refused() {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let o = parse_options(&args(&[
        "--dry-run",
        "--purge",
        "--bindir",
        "/opt/t",
        "--bindir=/x",
    ]))
    .unwrap()
    .unwrap();
    assert!(o.dry_run && o.purge && !o.yes && !o.udev && !o.system);
    assert_eq!(
        o.bindirs,
        vec![PathBuf::from("/opt/t"), PathBuf::from("/x")]
    );
    assert!(parse_options(&args(&["--bindir", "rel"])).is_err());
    assert!(parse_options(&args(&["--bindir"])).is_err());
    assert!(parse_options(&args(&["--frobnicate"])).is_err());
    assert_eq!(parse_options(&args(&["--help"])), Ok(None));
}

#[test]
fn the_manifest_is_a_hint_with_comments_and_unknown_keys_ignored() {
    let text = "# comment\n\nbindir=/a/bin\nfuture_key=1\nbindir=relative/bin\n  bindir=/b  \n";
    assert_eq!(
        manifest_bindirs(text),
        vec![PathBuf::from("/a/bin"), PathBuf::from("/b")]
    );
}

/// `execute` against a temp tree: what is removed, what the manifest keeps,
/// and a directory that is not empty stays and is listed.
#[test]
fn executing_a_plan_removes_what_it_said_and_rewrites_the_manifest() {
    let t = Tree::new("execute");
    v030_install(&t);
    t.put(
        MANIFEST,
        "# written by install-payload.sh\nbindir=/home/u/.local/bin\nbindir=/usr/bin\n",
    );
    t.bin("/usr/bin/tobii", "tobii 0.3.0");
    t.put(AUTOSTART, &autostart::entry_text("/usr/bin/tobii-gtk"));
    t.put("/home/u/.config/tobii-linux/calibration.bin", "cal");
    t.put(
        "/home/u/.config/tobii-linux/calibration.bin.baseline-20260810",
        "mine",
    );
    let opts = Options {
        purge: true,
        ..Options::default()
    };
    let p = plan_in(&t, &user(), &opts, &pacman_owns_usr_bin);
    assert!(p.refusal.is_none());
    let out = execute(&p);
    assert!(out.failed.is_empty(), "{:?}", out.failed);

    for gone in [
        &format!("{BIN}/tobii"),
        &format!("{BIN}/tobii-gtk"),
        ENTRY,
        ICON,
        AUTOSTART,
        "/home/u/.config/tobii-linux/calibration.bin",
    ] {
        assert!(!t.has(gone), "{gone} should be gone");
    }
    assert!(t.has("/usr/bin/tobii"), "the packaged copy is untouched");
    assert!(t.has("/home/u/.config/tobii-linux/calibration.bin.baseline-20260810"));
    // The package's line stays; the removed location's goes; the comment stays.
    let manifest = std::fs::read_to_string(t.real(MANIFEST)).unwrap();
    assert_eq!(
        manifest,
        "# written by install-payload.sh\nbindir=/usr/bin\n"
    );
    // The config directory held a backup, so it was not removed, and says so.
    assert!(out
        .not_empty
        .iter()
        .any(|(d, left)| d.ends_with("tobii-linux")
            && left.contains(&"calibration.bin.baseline-20260810".to_string())));
}

/// The manifest is decided again when it is edited: a binary that could not
/// be removed keeps its directory's line, so the next run still looks there.
#[test]
fn a_binary_that_could_not_be_removed_keeps_its_manifest_line() {
    let t = Tree::new("execute-fail");
    t.put(MANIFEST, "bindir=/home/u/.local/bin\n");
    t.bin(&format!("{BIN}/tobii"), "tobii 0.4.0");
    t.bin(&format!("{BIN}/tobii-gtk"), "tobii-gtk 0.4.0");
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert_eq!(p.manifests.len(), 1, "planned to go");
    // Between the plan and the removal, the hub's name became a directory
    // with something in it.
    std::fs::remove_file(t.real(&format!("{BIN}/tobii-gtk"))).unwrap();
    t.put(&format!("{BIN}/tobii-gtk/something"), "x");

    let out = execute(&p);
    assert_eq!(
        out.failed
            .iter()
            .map(|(f, _)| f.clone())
            .collect::<Vec<_>>(),
        [PathBuf::from(format!("{BIN}/tobii-gtk"))]
    );
    assert!(!t.has(&format!("{BIN}/tobii")));
    assert_eq!(
        std::fs::read_to_string(t.real(MANIFEST)).unwrap(),
        "bindir=/home/u/.local/bin\n"
    );
}

/// The running tobii goes last, and only if nothing else failed: a failure
/// leaves a tobii to run this again with, and the manifest line that finds it.
#[test]
fn the_running_tobii_is_kept_with_its_line_when_anything_else_fails() {
    let setup = |tag: &str| {
        let t = Tree::new(tag);
        t.put(MANIFEST, "bindir=/home/u/.local/bin\n");
        t.bin(&format!("{BIN}/tobii"), "tobii 0.4.0");
        t.bin(&format!("{BIN}/tobii-gtk"), "tobii-gtk 0.4.0");
        let env = Env {
            current_exe: Some(format!("{BIN}/tobii").into()),
            ..user()
        };
        let p = plan_in(&t, &env, &Options::default(), &unowned);
        assert_eq!(p.self_exe, Some(PathBuf::from(format!("{BIN}/tobii"))));
        (t, p)
    };

    let (t, p) = setup("self-kept");
    std::fs::remove_file(t.real(&format!("{BIN}/tobii-gtk"))).unwrap();
    t.put(&format!("{BIN}/tobii-gtk/something"), "x");
    let out = execute(&p);
    assert!(!out.failed.is_empty());
    assert!(t.has(&format!("{BIN}/tobii")), "the running tobii stays");
    assert_eq!(out.self_kept, Some(PathBuf::from(format!("{BIN}/tobii"))));
    assert_eq!(
        std::fs::read_to_string(t.real(MANIFEST)).unwrap(),
        "bindir=/home/u/.local/bin\n"
    );
    let s = summary(&p, &out);
    assert!(
        s.contains("run `/home/u/.local/bin/tobii uninstall` again"),
        "{s}"
    );

    // Nothing failing: it goes, after the manifest and its directory.
    let (t, p) = setup("self-goes");
    let out = execute(&p);
    assert!(out.failed.is_empty(), "{:?}", out.failed);
    assert!(!t.has(&format!("{BIN}/tobii")) && !t.has(MANIFEST));
    assert!(!t.has("/home/u/.local/share/tobii-linux"));
    assert_eq!(
        out.removed.last(),
        Some(&PathBuf::from(format!("{BIN}/tobii")))
    );
    assert!(out.self_kept.is_none());

    // A failure outside the install directory counts the same: here the
    // start-at-login entry sits in a directory this user cannot write. Root
    // writes there anyway, so this part cannot fail as root.
    if real_uid() == 0 {
        eprintln!("skipped the read-only autostart case: root can write anywhere");
        return;
    }
    let t = Tree::new("self-kept-outside");
    t.put(MANIFEST, "bindir=/home/u/.local/bin\n");
    t.bin(&format!("{BIN}/tobii"), "tobii 0.4.0");
    t.bin(&format!("{BIN}/tobii-gtk"), "tobii-gtk 0.4.0");
    t.put(
        AUTOSTART,
        &autostart::entry_text(&format!("{BIN}/tobii-gtk")),
    );
    let env = Env {
        current_exe: Some(format!("{BIN}/tobii").into()),
        ..user()
    };
    let p = plan_in(&t, &env, &Options::default(), &unowned);
    assert_eq!(p.self_exe, Some(PathBuf::from(format!("{BIN}/tobii"))));
    assert!(removals(&p).contains(&AUTOSTART.to_string()));
    let autostart_dir = "/home/u/.config/autostart";
    t.chmod(autostart_dir, 0o555);
    let out = execute(&p);
    t.chmod(autostart_dir, 0o755);
    assert_eq!(
        out.failed
            .iter()
            .map(|(f, _)| f.clone())
            .collect::<Vec<_>>(),
        [PathBuf::from(AUTOSTART)]
    );
    assert!(t.has(&format!("{BIN}/tobii")), "the running tobii stays");
    assert!(!t.has(&format!("{BIN}/tobii-gtk")));
    assert_eq!(out.self_kept, Some(PathBuf::from(format!("{BIN}/tobii"))));
    assert_eq!(
        std::fs::read_to_string(t.real(MANIFEST)).unwrap(),
        "bindir=/home/u/.local/bin\n"
    );
    assert!(t.has("/home/u/.local/share/tobii-linux"));
}

/// An entry that cannot be read cannot be judged, and one that cannot be
/// judged stays: a guess of "gone" is the one that switches something off.
#[test]
fn an_entry_that_cannot_be_read_is_kept() {
    if real_uid() == 0 {
        eprintln!("skipped: root reads a mode-000 file");
        return;
    }
    let t = Tree::new("unreadable-entry");
    v030_install(&t);
    t.put(
        AUTOSTART,
        &autostart::entry_text(&format!("{BIN}/tobii-gtk")),
    );
    t.chmod(AUTOSTART, 0o000);
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    let why = why_kept(&p, AUTOSTART);
    assert!(
        why.contains("could not be read") && why.contains("left as it is"),
        "{why}"
    );
    assert!(!removals(&p).contains(&AUTOSTART.to_string()));
    // The install itself still goes.
    assert!(removals(&p).contains(&format!("{BIN}/tobii-gtk")));
}

// ------------------------------------------------------- where it looks

/// PATH is not a place to look. Scanning it reached users' own wrappers,
/// other users' copies in shared directories and cargo's, and ran them to
/// ask what they were. --bindir still reaches such a directory.
#[test]
fn copies_found_only_on_path_are_not_looked_at() {
    let t = Tree::new("path-only");
    t.bin("/home/u/opt/bin/tobii", "tobii 0.3.0");
    t.bin("/home/u/opt/bin/tobii-gtk", "tobii-gtk 0.3.0");
    let env = Env {
        path: vec!["/home/u/opt/bin".into(), BIN.into()],
        ..user()
    };
    let runs = Runs::new();
    let identify = runs.identify();
    let p = plan(
        &env,
        &Options::default(),
        &t.root,
        &home_probes(&identify),
        &[],
    );
    assert!(removals(&p).is_empty(), "{:#?}", removals(&p));
    assert!(p.locations.is_empty(), "{:#?}", p.locations);
    assert!(!runs.ran(&t, "/home/u/opt/bin/tobii"));

    // An entry's bare-name Exec is judged by what PATH finds — here a copy
    // that stays, so the entry stays — but adds no place to look.
    t.put(AUTOSTART, "[Desktop Entry]\nExec=tobii-gtk --background\n");
    let only_opt = Env {
        path: vec!["/home/u/opt/bin".into()],
        ..user()
    };
    let p = plan(
        &only_opt,
        &Options::default(),
        &t.root,
        &home_probes(&identify),
        &[],
    );
    assert!(p.locations.is_empty(), "{:#?}", p.locations);
    assert!(runs.0.borrow().is_empty(), "{:?}", runs.0.borrow());
    assert!(why_kept(&p, AUTOSTART).contains("/home/u/opt/bin/tobii-gtk"));
    assert!(removals(&p).is_empty(), "{:#?}", removals(&p));
    std::fs::remove_file(t.real(AUTOSTART)).unwrap();

    let opts = Options {
        bindirs: vec!["/home/u/opt/bin".into()],
        ..Options::default()
    };
    let p = plan(&env, &opts, &t.root, &home_probes(&identify), &[]);
    assert_eq!(location(&p, "/home/u/opt/bin").via, [Via::Bindir]);
    assert_eq!(removals(&p).len(), 2, "{:#?}", removals(&p));
}

/// A script of the user's in ~/.local/bin that answers as the hub — a
/// wrapper that sets something and execs the real one, say — is not an
/// install, and is not run to find that out. It would have answered
/// "tobii-gtk 0.3.0".
#[test]
fn a_wrapper_script_that_answers_as_the_hub_is_kept_and_never_run() {
    let t = Tree::new("wrapper");
    let gtk = format!("{BIN}/tobii-gtk");
    t.bin(&format!("{BIN}/tobii"), "tobii 0.3.0");
    t.put(
        &gtk,
        "#!/bin/sh\necho tobii-gtk 0.3.0\nexec /opt/tobii/tobii-gtk \"$@\"\n",
    );
    t.chmod(&gtk, 0o755);
    t.put(ENTRY, &menu_entry(&gtk));
    let ran = std::cell::RefCell::new(Vec::<PathBuf>::new());
    let answers_as_named = |p: &Path| {
        ran.borrow_mut().push(p.to_path_buf());
        let name = p.file_name()?.to_str()?;
        Some(format!("{name} 0.3.0"))
    };
    let p = plan(
        &user(),
        &Options::default(),
        &t.root,
        &home_probes(&answers_as_named),
        &[],
    );
    assert!(
        !ran.borrow().iter().any(|r| r.ends_with("tobii-gtk")),
        "the wrapper was run: {:?}",
        ran.borrow()
    );
    let why = why_kept(&p, &gtk);
    assert!(
        why.contains("a script") && why.contains("a wrapper of yours?"),
        "{why}"
    );
    assert!(!removals(&p).contains(&gtk));
    // The real program beside it still goes; the menu entry that runs the
    // wrapper stays with it.
    assert!(removals(&p).contains(&format!("{BIN}/tobii")));
    assert!(kept(&p).contains(&ENTRY.to_string()));
    let loc = location(&p, BIN);
    assert!(loc
        .binaries
        .iter()
        .any(|b| b.name == "tobii-gtk" && b.ident == Ident::Refused("a script".into())));
    assert!(render(&p, &Options::default()).contains("not run: a script"));
}

/// Who owns a file, as a test tree pretends: `a/tobii` is another user's,
/// `b/tobii` is a link of theirs, `c/tobii` a link of ours to a file of
/// theirs, and the directory `e` is theirs. A file owned by another uid
/// cannot be made without root.
fn someone_elses(p: &Path, m: &std::fs::Metadata) -> u32 {
    let theirs = p.ends_with("home/u/a/tobii")
        || (p.ends_with("home/u/b/tobii") && m.file_type().is_symlink())
        || p.ends_with("home/u/c-real/tobii")
        || p.ends_with("home/u/e");
    if theirs {
        4321
    } else {
        owner_of(p, m)
    }
}

/// Another user's copy is theirs to remove, and running it runs their
/// program. Checked on the name itself (a symlink's own owner) and on the
/// file it resolves to.
#[test]
fn a_binary_someone_else_owns_is_neither_run_nor_removed() {
    let t = Tree::new("owner");
    t.bin("/home/u/a/tobii", "tobii 0.3.0");
    t.bin("/home/u/b-real/tobii", "tobii 0.3.0");
    t.symlink("/home/u/b-real/tobii", "/home/u/b/tobii");
    t.bin("/home/u/c-real/tobii", "tobii 0.3.0");
    t.symlink("/home/u/c-real/tobii", "/home/u/c/tobii");
    t.bin("/home/u/d/tobii", "tobii 0.3.0");
    // The user's own program, in a directory another user owns: its mode
    // bits are clean, but its owner can swap what is in it.
    t.bin("/home/u/e/tobii", "tobii 0.3.0");
    let opts = Options {
        bindirs: ["a", "b", "c", "d", "e"]
            .iter()
            .map(|d| PathBuf::from(format!("/home/u/{d}")))
            .collect(),
        ..Options::default()
    };
    let runs = Runs::new();
    let identify = runs.identify();
    let fs = Fs {
        root: &t.root,
        owner: someone_elses,
    };
    let p = plan_with_fs(&user(), &opts, fs, &home_probes(&identify), &[]);
    for (d, owned) in [
        ("a", "/home/u/a/tobii"),
        ("b", "/home/u/b/tobii"),
        ("c", "/home/u/c-real/tobii"),
    ] {
        let bin = format!("/home/u/{d}/tobii");
        let why = why_kept(&p, &bin);
        assert!(
            why.contains(&format!("{owned} is owned by uid 4321"))
                && why.contains("they can run tobii uninstall themselves"),
            "{d}: {why}"
        );
        assert_eq!(
            location(&p, &format!("/home/u/{d}")).verdict,
            Verdict::Unidentified
        );
    }
    let why = why_kept(&p, "/home/u/e/tobii");
    assert!(
        why.contains("/home/u/e belongs to uid 4321") && why.contains("in a directory"),
        "{why}"
    );
    for r in [
        "/home/u/a/tobii",
        "/home/u/b-real/tobii",
        "/home/u/c-real/tobii",
        "/home/u/e/tobii",
    ] {
        assert!(!runs.ran(&t, r), "{r} was run");
    }
    // The one that is all the user's own is run, and goes.
    assert!(runs.ran(&t, "/home/u/d/tobii"));
    assert_eq!(removals(&p), ["/home/u/d/tobii".to_string()]);
}

/// Somewhere others can write, what is there is not the user's alone to
/// choose: they could swap it between the question and the removal, or
/// have put it there. The file, the directory its name is in, and the
/// directory of the file a symlink leads to all count; a group write bit
/// does not when the group is the user's own private group, and a sticky
/// directory does not (only an entry's owner can replace it there).
#[test]
fn a_binary_somewhere_others_can_write_is_neither_run_nor_removed() {
    let gtk = format!("{BIN}/tobii-gtk");
    let case = |tag: &str, chmod: &[(&str, u32)], private: bool| {
        let t = Tree::new(tag);
        v030_install(&t);
        for (p, mode) in chmod {
            t.chmod(p, *mode);
        }
        let runs = Runs::new();
        let identify = runs.identify();
        let private_group = move |_: u32| private;
        let probes = Probes {
            private_group: &private_group,
            ..home_probes(&identify)
        };
        let p = plan(&user(), &Options::default(), &t.root, &probes, &[]);
        let ran = runs.ran(&t, &gtk);
        (p, ran)
    };

    for (tag, chmod, private, says) in [
        ("dir-777", vec![(BIN, 0o777)], false, "anyone"),
        ("dir-775", vec![(BIN, 0o775)], false, "its group"),
        ("dir-775-private-777", vec![(BIN, 0o777)], true, "anyone"),
        (
            "file-777",
            vec![(gtk.as_str(), 0o777)],
            false,
            "a file others can change",
        ),
    ] {
        let (p, ran) = case(tag, &chmod, private);
        assert!(!ran, "{tag}: it was run");
        assert!(!removals(&p).contains(&gtk), "{tag}");
        let why = why_kept(&p, &gtk);
        assert!(why.contains(says), "{tag}: {why}");
        if tag != "file-777" {
            assert!(
                why.contains("in a directory others can write"),
                "{tag}: {why}"
            );
        }
    }
    // The user's own private group, or a sticky directory: still theirs.
    for (tag, mode, private) in [
        ("dir-775-private", 0o775, true),
        ("dir-1777", 0o1777, false),
    ] {
        let (p, ran) = case(tag, &[(BIN, mode)], private);
        assert!(ran, "{tag}");
        assert!(removals(&p).contains(&gtk), "{tag}: {:#?}", p.kept);
    }

    // A link of the user's, in the user's own directory, to a file in a
    // directory anyone can write: that directory decides what the link runs.
    let t = Tree::new("link-into-777");
    t.bin("/home/u/shared/tobii-gtk", "tobii-gtk 0.3.0");
    t.chmod("/home/u/shared", 0o777);
    t.symlink("/home/u/shared/tobii-gtk", &gtk);
    let runs = Runs::new();
    let identify = runs.identify();
    let p = plan(
        &user(),
        &Options::default(),
        &t.root,
        &home_probes(&identify),
        &[],
    );
    assert!(!runs.ran(&t, "/home/u/shared/tobii-gtk"));
    let why = why_kept(&p, &gtk);
    assert!(
        why.contains("in a directory others can write") && why.contains("/home/u/shared can"),
        "{why}"
    );

    // The directory the NAME is in counts on its own: a link in a shared
    // ~/.local/bin to the user's own program, in the user's own directory.
    for (tag, mode, says) in [
        ("link-in-777", 0o777, "anyone"),
        ("link-in-775", 0o775, "its group"),
    ] {
        let t = Tree::new(tag);
        t.bin("/home/u/opt/tobii-gtk", "tobii-gtk 0.3.0");
        t.symlink("/home/u/opt/tobii-gtk", &gtk);
        t.chmod(BIN, mode);
        let runs = Runs::new();
        let identify = runs.identify();
        let p = plan(
            &user(),
            &Options::default(),
            &t.root,
            &home_probes(&identify),
            &[],
        );
        assert!(!runs.ran(&t, "/home/u/opt/tobii-gtk"), "{tag}");
        let why = why_kept(&p, &gtk);
        assert!(
            why.contains("in a directory others can write")
                && why.contains(&format!("{BIN} can be written by {says}")),
            "{tag}: {why}"
        );
    }

    // A link on the way, in a directory anyone can write: whoever can write
    // there can re-point it between the check and the run.
    let t = Tree::new("link-through-777");
    t.bin("/home/u/opt/tobii-gtk", "tobii-gtk 0.3.0");
    t.symlink("/home/u/opt/tobii-gtk", "/home/u/shared/tobii-gtk");
    t.chmod("/home/u/shared", 0o777);
    t.symlink("/home/u/shared/tobii-gtk", &gtk);
    let runs = Runs::new();
    let identify = runs.identify();
    let p = plan(
        &user(),
        &Options::default(),
        &t.root,
        &home_probes(&identify),
        &[],
    );
    assert!(!runs.ran(&t, "/home/u/opt/tobii-gtk"));
    let why = why_kept(&p, &gtk);
    assert!(
        why.contains("in a directory others can write") && why.contains("/home/u/shared can"),
        "{why}"
    );
}

#[test]
fn a_private_group_is_the_users_own_with_no_one_else_in_it() {
    let passwd = "root:x:0:0::/root:/bin/bash\nu:x:1000:1000::/home/u:/bin/bash\n\
                  v:x:1001:100::/home/v:/bin/bash\n";
    assert!(private_group_in(passwd, "u:x:1000:\n", 1000, 1000));
    assert!(private_group_in(passwd, "u:x:1000:u\n", 1000, 1000));
    // Someone else listed in it.
    assert!(!private_group_in(passwd, "u:x:1000:u,v\n", 1000, 1000));
    // A second line for the same gid, with someone in it.
    assert!(!private_group_in(
        passwd,
        "u:x:1000:\nalias:x:1000:v\n",
        1000,
        1000
    ));
    // Not in /etc/group at all: LDAP or sssd, which cannot be seen from here.
    assert!(!private_group_in(passwd, "users:x:100:\n", 1000, 1000));
    // Not the user's primary group, however empty.
    assert!(!private_group_in(passwd, "users:x:100:\n", 100, 1000));
    // Another account's primary group too.
    let shared = format!("{passwd}w:x:1002:1000::/home/w:/bin/sh\n");
    assert!(!private_group_in(&shared, "u:x:1000:\n", 1000, 1000));
    // Asked for another user.
    assert!(!private_group_in(passwd, "u:x:1000:\n", 1000, 1001));
}

/// A release archive has install.sh AND assets/install-payload.sh. An
/// install.sh of the user's own in ~/.local/bin is only that.
#[test]
fn an_install_sh_of_your_own_does_not_hide_an_install() {
    let t = Tree::new("own-install-sh");
    v030_install(&t);
    t.put(&format!("{BIN}/install.sh"), "#!/bin/sh\n# my own\n");
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert_eq!(location(&p, BIN).verdict, Verdict::Remove);
    for b in ["tobii", "tobii-gtk"] {
        assert!(removals(&p).contains(&format!("{BIN}/{b}")), "{b}");
    }
    assert!(!removals(&p).contains(&format!("{BIN}/install.sh")));

    t.put(&format!("{BIN}/assets/install-payload.sh"), "#!/bin/sh\n");
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert!(matches!(
        location(&p, BIN).verdict,
        Verdict::NotAnInstall(_)
    ));
}

/// CARGO_TARGET_DIR or build.target-dir can call Cargo's output directory
/// anything. Its CACHEDIR.TAG says what it is.
#[test]
fn a_cargo_target_dir_of_any_name_is_not_an_install() {
    for (tag, dir) in [
        ("custom-target", "/home/u/build/out/release"),
        (
            "custom-target-triple",
            "/home/u/build/out/x86_64-unknown-linux-gnu/release",
        ),
    ] {
        let t = Tree::new(tag);
        t.bin(&format!("{dir}/tobii-gtk"), "tobii-gtk 0.4.0");
        t.put("/home/u/build/out/CACHEDIR.TAG", CARGO_TAG);
        t.put(
            AUTOSTART,
            &autostart::entry_text(&format!("{dir}/tobii-gtk")),
        );
        let p = plan_in(&t, &user(), &Options::default(), &unowned);
        let loc = location(&p, dir);
        assert_eq!(
            loc.verdict,
            Verdict::NotAnInstall("a Cargo build directory"),
            "{tag}"
        );
        assert_eq!(loc.via, [Via::Autostart]);
        assert!(removals(&p).is_empty(), "{tag}: {:#?}", removals(&p));
        assert!(why_kept(&p, AUTOSTART).contains("not being removed"));
    }

    // A cache tag some other program wrote says nothing about Cargo.
    let t = Tree::new("other-cachedir");
    let dir = "/home/u/build/out/release";
    t.bin(&format!("{dir}/tobii-gtk"), "tobii-gtk 0.4.0");
    t.put(
        "/home/u/build/out/CACHEDIR.TAG",
        "Signature: 8a477f597d28d172789f06886806bc55\n# created by a backup tool\n",
    );
    t.put(
        AUTOSTART,
        &autostart::entry_text(&format!("{dir}/tobii-gtk")),
    );
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert_eq!(location(&p, dir).verdict, Verdict::Remove);
}

/// What `cargo install` writes to `.crates2.json`, for `packages` (name,
/// binary) installed from this checkout.
fn crates2_json_for(packages: &[(&str, &str)]) -> String {
    let entries: Vec<String> = packages
        .iter()
        .map(|(package, bin)| {
            format!(
                "\"{package} 0.3.0 (path+file:///home/u/src/TobiiLinux/crates/{package})\":\
                 {{\"version_req\":null,\"bins\":[\"{bin}\"],\"features\":[],\
                 \"all_features\":false,\"no_default_features\":false,\"profile\":\"release\",\
                 \"target\":\"x86_64-unknown-linux-gnu\",\
                 \"rustc\":\"rustc 1.89.0 (29483883e 2025-08-04)\\nbinary: rustc\\n\"}}"
            )
        })
        .collect();
    format!("{{\"installs\":{{{}}}}}\n", entries.join(","))
}

/// `cargo install --path crates/tobii-gtk` puts the hub in $CARGO_HOME/bin
/// and records it beside that. It is Cargo's to remove, and nothing there is
/// run.
#[test]
fn a_cargo_install_found_through_an_entry_is_left_to_cargo() {
    let never = |p: &Path| -> Option<String> { panic!("{p:?} is cargo's and must not be run") };
    let bin = "/home/u/.cargo/bin";

    let t = Tree::new("cargo-home");
    t.bin(&format!("{bin}/tobii"), "tobii 0.3.0");
    t.bin(&format!("{bin}/tobii-gtk"), "tobii-gtk 0.3.0");
    t.put(
        "/home/u/.cargo/.crates2.json",
        &crates2_json_for(&[("tobii-cli", "tobii"), ("tobii-gtk", "tobii-gtk")]),
    );
    t.put(ENTRY, &menu_entry(&format!("{bin}/tobii-gtk")));
    let udev = Options {
        udev: true,
        ..Options::default()
    };
    let p = plan_full(&t, &user(), &udev, &unowned, &|_| true, &never, &[]);
    // cargo install ships no udev rule: nothing to say one takes over.
    assert!(
        !p.udev_notes
            .iter()
            .any(|n| n.contains("a package install is still here")),
        "{:?}",
        p.udev_notes
    );
    assert_eq!(
        location(&p, bin).verdict,
        Verdict::Package {
            manager: "cargo".into(),
            package: "tobii-cli tobii-gtk".into()
        }
    );
    let hint = p.hints.iter().find(|h| h.contains(bin)).expect("a hint");
    assert!(
        hint.contains("cargo uninstall tobii-cli tobii-gtk"),
        "{hint}"
    );
    assert!(removals(&p).is_empty(), "{:#?}", removals(&p));
    assert!(kept(&p).contains(&ENTRY.to_string()));

    // The older record, and only the hub there. $CARGO_HOME is where cargo
    // uninstall looks without --root, wherever that is.
    let bin = "/home/u/cargo-x/bin";
    let t = Tree::new("cargo-home-toml");
    t.bin(&format!("{bin}/tobii-gtk"), "tobii-gtk 0.3.0");
    t.put(
        "/home/u/cargo-x/.crates.toml",
        "[v1]\n\"tobii-gtk 0.3.0 (path+file:///home/u/src/TobiiLinux/crates/tobii-gtk)\" = \
         [\"tobii-gtk\"]\n",
    );
    t.put(
        AUTOSTART,
        &autostart::entry_text(&format!("{bin}/tobii-gtk")),
    );
    let env = Env {
        cargo_home: Some("/home/u/cargo-x".into()),
        ..user()
    };
    let p = plan_full(
        &t,
        &env,
        &Options::default(),
        &unowned,
        &|_| true,
        &never,
        &[],
    );
    assert!(
        p.hints
            .iter()
            .any(|h| h.ends_with("\n    cargo uninstall tobii-gtk")),
        "{:?}",
        p.hints
    );
    assert!(kept(&p).contains(&AUTOSTART.to_string()));
}

/// A record beside a directory says `cargo install --root` was pointed there
/// once — at anything. Only what it lists is Cargo's.
#[test]
fn a_cargo_record_leaves_to_cargo_only_what_it_lists() {
    let ripgrep = "\"ripgrep 14.1.1 (registry+https://github.com/rust-lang/crates.io-index)\"";
    let ripgrep_toml = format!("[v1]\n{ripgrep} = [\"rg\"]\n");
    let ripgrep_json = format!(
        "{{\"installs\":{{{ripgrep}:{{\"version_req\":null,\"bins\":[\"rg\"],\"features\":[],\
         \"all_features\":false,\"no_default_features\":false,\"profile\":\"release\"}}}}}}\n"
    );

    // `cargo install --root ~/.local ripgrep`, then a v0.3.0 install.sh
    // install in ~/.local/bin: still found, identified, and planned.
    let t = Tree::new("cargo-record-other");
    v030_install(&t);
    t.bin(&format!("{BIN}/rg"), "ripgrep 14.1.1");
    t.put("/home/u/.local/.crates.toml", &ripgrep_toml);
    t.put("/home/u/.local/.crates2.json", &ripgrep_json);
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert_eq!(location(&p, BIN).verdict, Verdict::Remove);
    for b in ["tobii", "tobii-gtk"] {
        assert!(removals(&p).contains(&format!("{BIN}/{b}")), "{b}");
    }
    assert!(!removals(&p).contains(&format!("{BIN}/rg")));
    assert!(
        !p.hints.iter().any(|h| h.contains("cargo")),
        "{:?}",
        p.hints
    );
    assert!(removals(&p).contains(&ENTRY.to_string()));

    // A record that lists tobii-cli: only tobii is Cargo's, and the command
    // names this --root, since it is not $CARGO_HOME.
    let t = Tree::new("cargo-record-cli");
    v030_install(&t);
    t.put(
        "/home/u/.local/.crates.toml",
        &format!(
            "{ripgrep_toml}\"tobii-cli 0.3.0 (path+file:///home/u/src/TobiiLinux/crates/\
             tobii-cli)\" = [\"tobii\"]\n"
        ),
    );
    let runs = Runs::new();
    let identify = runs.identify();
    let p = plan(
        &user(),
        &Options::default(),
        &t.root,
        &home_probes(&identify),
        &[],
    );
    assert!(!runs.ran(&t, &format!("{BIN}/tobii")));
    assert!(why_kept(&p, &format!("{BIN}/tobii")).contains("Cargo's record lists it"));
    assert!(removals(&p).contains(&format!("{BIN}/tobii-gtk")));
    assert!(!removals(&p).contains(&format!("{BIN}/tobii")));
    let loc = location(&p, BIN);
    assert_eq!(loc.verdict, Verdict::Remove);
    assert!(loc
        .binaries
        .iter()
        .any(|b| b.name == "tobii" && b.ident == Ident::Refused("cargo install's".into())));
    let hint = p
        .hints
        .iter()
        .find(|h| h.contains("cargo"))
        .expect("a cargo hint");
    assert!(
        hint.ends_with("\n    cargo uninstall --root /home/u/.local tobii-cli"),
        "{hint}"
    );
    // Not the whole directory: the hub there is not Cargo's.
    assert!(!hint.contains("tobii-gtk"), "{hint}");

    // The same from the newer record alone, for the hub.
    let t = Tree::new("cargo-record-gtk-json");
    v030_install(&t);
    t.put(
        "/home/u/.local/.crates2.json",
        &crates2_json_for(&[("tobii-gtk", "tobii-gtk")]),
    );
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert!(removals(&p).contains(&format!("{BIN}/tobii")));
    assert!(kept(&p).contains(&format!("{BIN}/tobii-gtk")));

    // The reading itself: a package whose name only begins with ours is not
    // ours; TOML arrays may span lines and hold comments; JSON escapes.
    let toml = "# c\n[v1]\n\"tobii-cli-extra 1.0 (x)\" = [\"tobii\"]\n\
                \"tobii-gtk 1.0 (path+file:///a%20b)\" = [\n  \"tobii-gtk\", # the hub\n]\n\
                [v2]\n\"tobii-cli 1.0 (x)\" = [\"tobii\"]\n";
    assert_eq!(
        crates_toml(toml),
        [
            (
                "tobii-cli-extra 1.0 (x)".to_string(),
                vec!["tobii".to_string()]
            ),
            (
                "tobii-gtk 1.0 (path+file:///a%20b)".to_string(),
                vec!["tobii-gtk".to_string()]
            ),
        ]
    );
    let json = r#"{"installs":{"tobii-cli 1.0 (path+file:///a\"b\\c\u0041)":{"bins":["tobii"],"x":[1,{"y":null}]}}}"#;
    assert_eq!(
        crates2_json(json),
        [(
            "tobii-cli 1.0 (path+file:///a\"b\\cA)".to_string(),
            vec!["tobii".to_string()]
        )]
    );
    assert!(crates2_json("{\"installs\":").is_empty());
    let t = Tree::new("cargo-record-prefix");
    t.put("/r/.crates.toml", toml);
    t.bin("/r/bin/tobii", "tobii 0.3.0");
    assert!(cargo_listed(&Fs::new(&t.root), Path::new("/r/bin"), &["tobii"]).is_empty());
}

/// A directory this user cannot write is asked as this user — after the
/// checks — and is a system-wide install only if what is there answers as
/// this program. Anything else there is only something called tobii, and
/// advice to run this as root there would be wrong.
#[test]
fn a_directory_you_cannot_write_is_a_system_install_only_if_it_answers() {
    let t = Tree::new("not-writable");
    t.bin("/opt/sys/tobii", "tobii 0.3.0");
    t.bin("/opt/sys/tobii-gtk", "tobii-gtk 0.3.0");
    t.bin("/opt/other/tobii", "tobii-the-other-program 2.0");
    t.put("/opt/script/tobii", "#!/bin/sh\necho tobii 0.3.0\n");
    t.chmod("/opt/script/tobii", 0o755);
    let opts = Options {
        bindirs: ["/opt/sys", "/opt/other", "/opt/script"]
            .iter()
            .map(PathBuf::from)
            .collect(),
        ..Options::default()
    };
    let runs = Runs::new();
    let identify = runs.identify();
    let probes = Probes {
        writable: &|_| false,
        ..home_probes(&identify)
    };
    // /opt is root's; the home directory below is the user's own.
    t.bin("/home/u/ro/tobii", "tobii 0.3.0");
    let opts = Options {
        bindirs: [opts.bindirs, vec!["/home/u/ro".into()]].concat(),
        ..opts
    };
    let fs = Fs {
        root: &t.root,
        owner: opt_is_roots,
    };
    let p = plan_with_fs(&user(), &opts, fs, &probes, &[]);
    assert!(removals(&p).is_empty(), "{:#?}", removals(&p));

    assert!(runs.ran(&t, "/opt/sys/tobii"));
    assert_eq!(location(&p, "/opt/sys").verdict, Verdict::SystemInstall);
    assert!(p
        .hints
        .iter()
        .any(|h| h.contains("uninstall --system --bindir /opt/sys")));

    // The user's own directory, only not writable: chmod, not sudo — root
    // would refuse to run what is in a directory that is not root's, and
    // send them back here.
    assert!(runs.ran(&t, "/home/u/ro/tobii"));
    assert_eq!(location(&p, "/home/u/ro").verdict, Verdict::NotWritable);
    let hint = p
        .hints
        .iter()
        .find(|h| h.contains("/home/u/ro"))
        .expect("a hint");
    assert!(hint.ends_with("\n    chmod u+w /home/u/ro"), "{hint}");
    assert!(!hint.contains("sudo tobii"), "{hint}");
    let s = summary(&p, &Outcome::default());
    assert!(s.contains("chmod u+w /home/u/ro"), "{s}");
    assert!(!s.contains("--bindir /home/u/ro"), "{s}");

    for (dir, says) in [
        ("/opt/other", "did not answer"),
        ("/opt/script", "a script"),
    ] {
        assert_eq!(location(&p, dir).verdict, Verdict::Unidentified, "{dir}");
        assert!(
            !p.hints.iter().any(|h| h.contains(dir)),
            "{dir}: {:?}",
            p.hints
        );
        assert!(
            why_kept(&p, &format!("{dir}/tobii")).contains(says),
            "{dir}"
        );
    }
    assert!(!runs.ran(&t, "/opt/script/tobii"));
    let s = summary(&p, &Outcome::default());
    assert!(!s.contains("--bindir /opt/other"), "{s}");
}

/// What is run to ask a binary what it is, is the file the checks were made
/// on — its resolved path — never its name, whose symlinks would be followed
/// again and could have changed in between. The same call serves root
/// under --system, where root_may_run's approved path is what is run.
#[test]
fn the_file_run_to_identify_a_binary_is_the_one_that_was_checked() {
    let t = Tree::new("run-the-checked");
    let real_gtk = "/home/u/opt/tobii-linux/tobii-gtk";
    t.bin(real_gtk, "tobii-gtk 0.3.0");
    t.symlink(real_gtk, &format!("{BIN}/tobii-gtk"));
    t.bin(&format!("{BIN}/tobii"), "tobii 0.3.0");
    let runs = Runs::new();
    let identify = runs.identify();
    checked();
    let p = plan(
        &user(),
        &Options::default(),
        &t.root,
        &home_probes(&identify),
        &[],
    );
    let canonical = |p: &str| t.real(p).canonicalize().unwrap();
    assert_eq!(
        *runs.0.borrow(),
        [canonical(&format!("{BIN}/tobii")), canonical(real_gtk)]
    );
    // And what program_check opened is exactly that: not the link's name,
    // whose symlink would be followed again.
    assert_eq!(checked(), *runs.0.borrow());
    // The link is what goes; what it points at stays.
    assert!(removals(&p).contains(&format!("{BIN}/tobii-gtk")));
    assert!(!removals(&p).contains(&real_gtk.to_string()));
}

/// The pid is checked again at the moment of the signal, not only when the
/// list was made. A process whose start time differs is a later one that got
/// the same pid, and is never signalled; nor is one whose start time was
/// never known.
#[test]
fn a_process_that_is_no_longer_the_one_listed_is_never_signalled() {
    let pid = std::process::id();
    let now = Proc {
        pid,
        uid: 0,
        exe: std::fs::read_link("/proc/self/exe")
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        args: Vec::new(),
        start_time: start_time(pid),
    };
    assert!(now.start_time.is_some());
    let later = Proc {
        start_time: now.start_time.map(|t| t + 1),
        ..now.clone()
    };
    let unknown = Proc {
        start_time: None,
        ..now.clone()
    };
    let mut sent: Vec<Option<u64>> = Vec::new();
    let signalled = signal_each(&[later, unknown, now.clone()], &mut |p| {
        sent.push(p.start_time);
        Ok(())
    });
    assert_eq!(sent, [now.start_time]);
    assert_eq!(signalled, std::slice::from_ref(&now));
    // A signal that could not be sent is not counted as sent.
    let signalled = signal_each(std::slice::from_ref(&now), &mut |_| Err("no".into()));
    assert!(signalled.is_empty());
}

/// Package-managed and system-wide copies were printed under whatever
/// heading came last — "Removed (0):" when nothing was kept.
#[test]
fn what_this_run_does_not_touch_has_its_own_heading_in_the_summary() {
    let t = Tree::new("summary-untouched");
    t.bin(&format!("{BIN}/tobii"), "tobii 0.3.0");
    t.bin("/opt/sys/tobii", "tobii 0.3.0");
    let owner = |d: &Path| {
        if d.ends_with(".local/bin") {
            Ownership::Package {
                manager: "pacman".into(),
                package: "tobii-linux".into(),
            }
        } else {
            Ownership::None
        }
    };
    let opts = Options {
        bindirs: vec!["/opt/sys".into()],
        ..Options::default()
    };
    let probes = Probes {
        owner: &owner,
        writable: &|d| !d.ends_with("opt/sys"),
        ..home_probes(&by_content)
    };
    let fs = Fs {
        root: &t.root,
        owner: opt_is_roots,
    };
    let p = plan_with_fs(&user(), &opts, fs, &probes, &[]);
    assert!(p.kept.is_empty() && p.is_empty(), "{p:#?}");
    let s = summary(&p, &execute(&p));
    let at = |needle: &str| {
        s.find(needle)
            .unwrap_or_else(|| panic!("{needle} is missing: {s}"))
    };
    let heading = at("Not touched:");
    assert!(at("Removed (0):") < heading, "{s}");
    assert!(heading < at(&format!("{BIN} — owned by pacman")), "{s}");
    assert!(heading < at("/opt/sys — a system-wide install"), "{s}");
}

// ------------------------------------------ what is opened, and as whom

/// A CACHEDIR.TAG sits somewhere anyone may have made. It is opened without
/// following a symlink at its end and without waiting on a FIFO, and read
/// only if it is a regular file.
#[test]
fn a_cachedir_tag_that_is_a_fifo_or_a_symlink_is_not_read() {
    use std::os::unix::ffi::OsStringExt;
    let dir = "/home/u/build/out/release";
    let tag = "/home/u/build/out/CACHEDIR.TAG";
    let opts = Options {
        bindirs: vec![dir.into()],
        ..Options::default()
    };

    let t = Tree::new("tag-fifo");
    t.bin(&format!("{dir}/tobii-gtk"), "tobii-gtk 0.4.0");
    let fifo = std::ffi::CString::new(t.real(tag).into_os_string().into_vec()).unwrap();
    // SAFETY: a valid NUL-terminated path; mkfifo only creates a node there.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    let (tx, rx) = std::sync::mpsc::channel();
    let (root, o) = (t.root.clone(), opts.clone());
    std::thread::spawn(move || {
        let p = plan(&user(), &o, &root, &home_probes(&by_content), &[]);
        let _ = tx.send(location(&p, dir).verdict.clone());
    });
    let verdict = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the plan is waiting on a FIFO named CACHEDIR.TAG");
    assert_eq!(verdict, Verdict::Remove);

    let t = Tree::new("tag-symlink");
    t.bin(&format!("{dir}/tobii-gtk"), "tobii-gtk 0.4.0");
    t.put("/home/u/elsewhere/CACHEDIR.TAG", CARGO_TAG);
    t.symlink("/home/u/elsewhere/CACHEDIR.TAG", tag);
    let p = plan_in(&t, &user(), &opts, &unowned);
    assert_eq!(location(&p, dir).verdict, Verdict::Remove);
    // The same text in a file of its own there is read, as before.
    std::fs::remove_file(t.real(tag)).unwrap();
    t.put(tag, CARGO_TAG);
    let p = plan_in(&t, &user(), &opts, &unowned);
    assert_eq!(
        location(&p, dir).verdict,
        Verdict::NotAnInstall("a Cargo build directory")
    );
}

/// As root, nothing in a directory is opened until root_may_run has found
/// that only root can change what is there: not the CACHEDIR.TAG above a
/// user's build directory, not a Cargo record beside it, not the binary.
/// It is left, and not run as root.
#[test]
fn as_root_nothing_in_a_directory_others_control_is_opened() {
    let t = Tree::new("root-tag");
    let dir = "/home/u/build/out/release";
    t.bin(&format!("{dir}/tobii-gtk"), "tobii-gtk 0.4.0");
    t.put("/home/u/build/out/CACHEDIR.TAG", CARGO_TAG);
    t.put(
        "/home/u/build/out/.crates.toml",
        "[v1]\n\"tobii-gtk 0.4.0 (x)\" = [\"tobii-gtk\"]\n",
    );
    let root = Env {
        euid: 0,
        home: Some("/root".into()),
        ..user()
    };
    let opts = Options {
        system: true,
        bindirs: vec![dir.into()],
        ..Options::default()
    };
    let never = |p: &Path| -> Option<String> { panic!("{p:?} was run as root") };
    checked();
    let p = plan_full(&t, &root, &opts, &unowned, &|_| true, &never, &[]);
    let loc = location(&p, dir);
    assert!(matches!(loc.verdict, Verdict::NotRunAsRoot(_)), "{loc:?}");
    assert!(checked().is_empty(), "opened as root");
    assert!(
        !p.hints.iter().any(|h| h.contains("cargo")),
        "{:?}",
        p.hints
    );
}

/// D7 under --system: what root runs to identify a binary is exactly the
/// path root_may_run approved, and the one program_check opened. Root's
/// check needs every directory up to / to be root's and writable by no one
/// else, so the tree is under target/ — not /tmp, whose 1777 it refuses —
/// and the owner hook says root owns all of it. No root needed.
#[test]
fn as_root_the_file_run_is_the_one_root_may_run_approved() {
    use std::os::unix::fs::MetadataExt;
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/uninstall-test");
    mkdirs(&base);
    let base = base.canonicalize().unwrap();
    let open = base
        .ancestors()
        .find(|a| std::fs::metadata(a).is_ok_and(|m| m.mode() & 0o022 != 0));
    if let Some(open) = open {
        eprintln!(
            "skipped: {} can be written by its group or others, which root_may_run refuses",
            open.display()
        );
        return;
    }
    let t = Tree::under(&base, "root-d7");
    let real_gtk = "/opt/tobii-linux/tobii-gtk";
    t.bin(real_gtk, "tobii-gtk 0.4.0");
    t.symlink(real_gtk, "/usr/local/bin/tobii-gtk");
    t.bin("/usr/local/bin/tobii", "tobii 0.4.0");
    let root = Env {
        euid: 0,
        home: Some("/root".into()),
        ..user()
    };
    let opts = Options {
        system: true,
        bindirs: vec!["/usr/local/bin".into()],
        ..Options::default()
    };
    let runs = Runs::new();
    let identify = runs.identify();
    let fs = Fs {
        root: &t.root,
        owner: |_, _| 0,
    };
    checked();
    let p = plan_with_fs(&root, &opts, fs, &home_probes(&identify), &[]);
    let canonical = |p: &str| t.real(p).canonicalize().unwrap();
    assert_eq!(
        *runs.0.borrow(),
        [canonical("/usr/local/bin/tobii"), canonical(real_gtk)]
    );
    assert_eq!(checked(), *runs.0.borrow());
    let r = removals(&p);
    for want in ["/usr/local/bin/tobii", "/usr/local/bin/tobii-gtk"] {
        assert!(r.contains(&want.to_string()), "{want}: {r:#?}");
    }
    assert!(!r.contains(&real_gtk.to_string()), "{r:#?}");
}

type Owner = fn(&Path, &std::fs::Metadata) -> u32;

fn home_is_overflow(p: &Path, m: &std::fs::Metadata) -> u32 {
    if p.ends_with("home") {
        OVERFLOW_UID
    } else {
        owner_of(p, m)
    }
}

fn home_is_theirs(p: &Path, m: &std::fs::Metadata) -> u32 {
    if p.ends_with("home") {
        4321
    } else {
        owner_of(p, m)
    }
}

/// Above the directories that decide which file runs: whoever can write one
/// can rename what is under it, so those count too — but for writers, and
/// with the overflow uid as an owner, since inside a toolbox or `unshare -c`
/// root's `/` and `/home` belong to it.
#[test]
fn a_directory_above_an_install_that_others_can_write_refuses_it() {
    let gtk = format!("{BIN}/tobii-gtk");
    let case = |tag: &str, chmod: &[(&str, u32)], private: bool, owner: Owner| {
        let t = Tree::new(tag);
        v030_install(&t);
        for (p, mode) in chmod {
            t.chmod(p, *mode);
        }
        let runs = Runs::new();
        let identify = runs.identify();
        let private_group = move |_: u32| private;
        let probes = Probes {
            private_group: &private_group,
            ..home_probes(&identify)
        };
        let fs = Fs {
            root: &t.root,
            owner,
        };
        let p = plan_with_fs(&user(), &Options::default(), fs, &probes, &[]);
        let ran = runs.ran(&t, &gtk);
        (p, ran)
    };
    for (tag, chmod, owner, says) in [
        (
            "above-777",
            vec![("/home/u/.local", 0o777)],
            owner_of as Owner,
            "/home/u/.local can be written by anyone",
        ),
        (
            "above-775",
            vec![("/home/u", 0o775)],
            owner_of as Owner,
            "/home/u can be written by its group",
        ),
        (
            "above-theirs",
            vec![],
            home_is_theirs as Owner,
            "/home belongs to uid 4321",
        ),
    ] {
        let (p, ran) = case(tag, &chmod, false, owner);
        assert!(!ran, "{tag}: it was run");
        assert!(!removals(&p).contains(&gtk), "{tag}");
        let why = why_kept(&p, &gtk);
        assert!(
            why.contains("in a directory others can write") && why.contains(says),
            "{tag}: {why}"
        );
    }
    for (tag, chmod, private, owner) in [
        (
            "above-775-private",
            vec![("/home/u", 0o775)],
            true,
            owner_of as Owner,
        ),
        (
            "above-1777",
            vec![("/home/u", 0o1777)],
            false,
            owner_of as Owner,
        ),
        ("above-overflow", vec![], false, home_is_overflow as Owner),
    ] {
        let (p, ran) = case(tag, &chmod, private, owner);
        assert!(ran, "{tag}");
        assert!(removals(&p).contains(&gtk), "{tag}: {:#?}", p.kept);
    }
}

/// `~/.local/bin/tobii` another user's, and `~/.local/bin/tobii-gtk` a link
/// of theirs.
fn listed_but_theirs(p: &Path, m: &std::fs::Metadata) -> u32 {
    let theirs = p.ends_with("home/u/.local/bin/tobii")
        || (p.ends_with("home/u/.local/bin/tobii-gtk") && m.file_type().is_symlink());
    if theirs {
        4321
    } else {
        owner_of(p, m)
    }
}

/// The file `~/.local/bin/tobii-gtk` leads to is another user's.
fn listed_target_theirs(p: &Path, m: &std::fs::Metadata) -> u32 {
    if p.ends_with("home/u/opt/tobii-gtk") {
        4321
    } else {
        owner_of(p, m)
    }
}

/// A name in a directory the manifest lists is removed without being run —
/// but not another user's, however the directory came to be listed: it is
/// theirs to remove. The name (a link's own owner) and the file it leads to
/// both count.
#[test]
fn a_manifest_listed_binary_someone_else_owns_is_kept() {
    let never = |p: &Path| -> Option<String> { panic!("{p:?} is listed and must not be run") };
    let setup = |tag: &str| {
        let t = Tree::new(tag);
        t.put(MANIFEST, "bindir=/home/u/.local/bin\n");
        t.bin(&format!("{BIN}/tobii"), "tobii 0.4.0");
        t.bin("/home/u/opt/tobii-gtk", "tobii-gtk 0.4.0");
        t.symlink("/home/u/opt/tobii-gtk", &format!("{BIN}/tobii-gtk"));
        t
    };

    let t = setup("listed-theirs");
    let fs = Fs {
        root: &t.root,
        owner: listed_but_theirs,
    };
    let p = plan_with_fs(&user(), &Options::default(), fs, &home_probes(&never), &[]);
    for b in [format!("{BIN}/tobii"), format!("{BIN}/tobii-gtk")] {
        let why = why_kept(&p, &b);
        assert!(
            why.contains("listed in the install manifest")
                && why.contains(&format!("{b} is owned by uid 4321"))
                && why.contains("they can run tobii uninstall themselves"),
            "{why}"
        );
    }
    assert!(removals(&p).is_empty(), "{:#?}", removals(&p));
    assert!(p.manifests.is_empty(), "{:#?}", p.manifests);
    assert!(location(&p, BIN)
        .binaries
        .iter()
        .all(|b| b.ident == Ident::Refused("owned by uid 4321".into())));

    let t = setup("listed-target-theirs");
    let fs = Fs {
        root: &t.root,
        owner: listed_target_theirs,
    };
    let p = plan_with_fs(&user(), &Options::default(), fs, &home_probes(&never), &[]);
    let why = why_kept(&p, &format!("{BIN}/tobii-gtk"));
    assert!(
        why.contains("/home/u/opt/tobii-gtk is owned by uid 4321"),
        "{why}"
    );
    assert_eq!(removals(&p), [format!("{BIN}/tobii")]);
}

/// Commands printed for a person to paste are quoted for the shell where a
/// path needs it: a space or a quote in a Wine prefix, a --bindir, a cargo
/// --root or the running program's path would otherwise split the word or
/// end it.
#[test]
fn paths_in_printed_commands_are_quoted_for_the_shell() {
    assert_eq!(sh_quote("/home/u/.wine"), "/home/u/.wine");
    assert_eq!(
        sh_quote("/home/u/it's a prefix"),
        r"'/home/u/it'\''s a prefix'"
    );
    assert_eq!(sh_quote(""), "''");

    let t = Tree::new("quoting");
    let prefix = "/home/u/Games/it's mine";
    t.mkdir(&format!("{prefix}/drive_c/tobii-bridge"));
    let sys = "/opt/Tobii's dir";
    t.bin(&format!("{sys}/tobii"), "tobii 0.3.0");
    let apps = "/home/u/my apps";
    t.bin(&format!("{apps}/bin/tobii"), "tobii 0.3.0");
    t.put(
        &format!("{apps}/.crates.toml"),
        "[v1]\n\"tobii-cli 0.3.0 (path+file:///x)\" = [\"tobii\"]\n",
    );
    let archive = "/home/u/Down loads/tobii-linux-0.4.0";
    t.bin(&format!("{archive}/tobii"), "tobii 0.4.0");
    t.put(&format!("{archive}/install.sh"), "#!/bin/sh\n");
    t.put(
        &format!("{archive}/assets/install-payload.sh"),
        "#!/bin/sh\n",
    );
    let env = Env {
        wineprefix: Some(prefix.into()),
        current_exe: Some(format!("{archive}/tobii").into()),
        ..user()
    };
    let opts = Options {
        bindirs: vec![sys.into(), format!("{apps}/bin").into()],
        ..Options::default()
    };
    let probes = Probes {
        writable: &|d: &Path| !d.ends_with("Tobii's dir"),
        ..home_probes(&by_content)
    };
    let fs = Fs {
        root: &t.root,
        owner: opt_is_roots,
    };
    let p = plan_with_fs(&env, &opts, fs, &probes, &[]);
    assert_eq!(location(&p, sys).verdict, Verdict::SystemInstall);

    let running = "'/home/u/Down loads/tobii-linux-0.4.0/tobii'";
    let text = render(&p, &Options::default());
    let bridge = format!(r"{running} bridge uninstall --prefix '/home/u/Games/it'\''s mine'");
    assert!(text.contains(&bridge), "{text}");
    let system = format!(r"sudo {running} uninstall --system --bindir '/opt/Tobii'\''s dir'");
    assert!(
        p.hints.iter().any(|h| h.contains(&system)),
        "{:#?}",
        p.hints
    );
    let cargo = "cargo uninstall --root '/home/u/my apps' tobii-cli";
    assert!(p.hints.iter().any(|h| h.ends_with(cargo)), "{:#?}", p.hints);
    let s = summary(&p, &Outcome::default());
    assert!(
        s.contains(r"sudo tobii uninstall --system --bindir '/opt/Tobii'\''s dir'"),
        "{s}"
    );
    assert!(s.contains(cargo), "{s}");
}
