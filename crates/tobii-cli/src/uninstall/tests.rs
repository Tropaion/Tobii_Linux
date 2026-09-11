//! `plan()` and `execute()` against temporary trees only. Nothing here runs a
//! binary, asks a package manager or signals anything: ownership,
//! identification and the process list are all injected. One test reads this
//! test process's own `/proc` entry, to check how a process is told apart from
//! a later one with the same pid.

use super::*;

/// A temporary filesystem root, removed on the way out.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = std::env::temp_dir().join(format!(
            "tobii-uninstall-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Tree { root }
    }
    fn real(&self, p: &str) -> PathBuf {
        self.root.join(p.trim_start_matches('/'))
    }
    fn put(&self, p: &str, content: &str) {
        let real = self.real(p);
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
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
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(self.real(target), real).unwrap();
    }
    fn mkdir(&self, p: &str) {
        std::fs::create_dir_all(self.real(p)).unwrap();
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

fn user() -> Env {
    Env {
        home: Some("/home/u".into()),
        euid: 1000,
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
    };
    plan(env, opts, &t.root, &probes, procs)
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
    for (tag, dir, extra) in [
        ("entry-build", "/home/u/src/TobiiLinux/target/release", None),
        (
            "entry-cross",
            "/home/u/src/TobiiLinux/target/x86_64-unknown-linux-gnu/release",
            Some("/home/u/src/TobiiLinux/target/CACHEDIR.TAG"),
        ),
        (
            "entry-tarball",
            "/home/u/Downloads/tobii-linux-0.3.0-x86_64-unknown-linux-gnu",
            Some("/home/u/Downloads/tobii-linux-0.3.0-x86_64-unknown-linux-gnu/install.sh"),
        ),
    ] {
        let t = Tree::new(tag);
        t.bin(&format!("{dir}/tobii"), "tobii 0.3.0");
        t.bin(&format!("{dir}/tobii-gtk"), "tobii-gtk 0.3.0");
        if let Some(e) = extra {
            t.put(e, "x");
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
            1000,
            "/usr/bin/tobii-gtk",
            &["tobii-gtk", "--background"],
        ),
        proc(21, 1000, &format!("{BIN}/tobii"), &["tobii", "stream"]),
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
    assert!(why_kept(&p, &format!("{BIN}/tobii")).contains("not an ELF program"));
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
    let p = plan_in(&t, &root, &sys, &unowned);
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
/// someone other than root can write to decides what that runs.
#[test]
fn as_root_a_bindir_someone_else_can_write_to_is_never_run() {
    let t = Tree::new("root-bindir");
    t.bin("/opt/t/tobii", "tobii 0.3.0");
    t.bin("/opt/t/tobii-gtk", "tobii-gtk 0.3.0");
    t.chmod("/opt/t", 0o777);
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
    assert!(loc.binaries.iter().all(|b| b.ident == Ident::NotRun));
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

    // An install somewhere else on PATH, found only there — and an unrelated
    // `tobii` on PATH, which says so and stays.
    let t = Tree::new("path-only");
    t.bin("/home/u/opt/bin/tobii", "tobii 0.3.0");
    t.bin("/home/u/other/bin/tobii", "tobii-the-other-program 2.0");
    let env = Env {
        path: vec![
            "/home/u/opt/bin".into(),
            "/home/u/other/bin".into(),
            "/home/u/nothing".into(),
        ],
        ..user()
    };
    let p = plan_in(&t, &env, &Options::default(), &unowned);
    assert_eq!(removals(&p), ["/home/u/opt/bin/tobii".to_string()]);
    assert_eq!(location(&p, "/home/u/opt/bin").via, [Via::Path]);
    assert_eq!(
        location(&p, "/home/u/other/bin").verdict,
        Verdict::Unidentified
    );
    assert!(!p.locations.iter().any(|l| l.dir.ends_with("nothing")));
}

/// Only Cargo's own output directories are build trees: `target/<profile>`
/// and `target/<triple>/<profile>`. Something merely kept under a target
/// directory — a HOME made up for a test, as the tarball-route check does —
/// is looked at like anywhere else.
#[test]
fn only_cargos_output_directories_count_as_a_build_tree() {
    let t = Tree::new("under-target");
    let home = "/work/target/uninstall-test/home";
    t.put(
        "/work/target/CACHEDIR.TAG",
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    );
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
        path: built.iter().map(PathBuf::from).collect(),
        ..user()
    };
    let p = plan_in(&t, &env, &Options::default(), &unowned);
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
        proc(4242, 1000, &format!("{BIN}/tobii"), &["tobii", "uninstall"]),
        proc(4241, 1000, &format!("{BIN}/tobii-gtk"), &["tobii-gtk"]),
        // Started before the updater replaced the file.
        proc(
            10,
            1000,
            &format!("{BIN}/tobii-gtk (deleted)"),
            &["tobii-gtk", "--background"],
        ),
        proc(
            11,
            1000,
            &format!("{BIN}/tobii"),
            &["tobii", "game", "--", "game.exe"],
        ),
        proc(12, 1001, &format!("{BIN}/tobii-gtk"), &["tobii-gtk"]),
        proc(13, 1000, "/usr/bin/tobii-gtk", &["tobii-gtk"]),
        proc(14, 1000, "/usr/bin/bash", &["bash"]),
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
        1000,
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
    let ours = proc(31, 1000, &format!("{BIN}/tobii-gtk"), &["tobii-gtk"]);
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

    // Removing the bridge needs tobii, and this run removes tobii: said in
    // the plan, asked before anything is removed, and repeated at the end.
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
}
