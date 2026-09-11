//! `plan()` against temporary trees only. Nothing here runs a binary, asks a
//! package manager, reads the real `/proc` or signals anything: ownership,
//! identification and the process list are all injected.

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

/// What `install.sh` from v0.3.0 leaves: two binaries, a menu entry with an
/// absolute Exec, the icon — and no manifest, which did not exist yet.
fn v030_install(t: &Tree) {
    t.put(&format!("{BIN}/tobii"), "tobii 0.3.0\n");
    t.put(&format!("{BIN}/tobii-gtk"), "tobii-gtk 0.3.0\n");
    t.put(
        ENTRY,
        "[Desktop Entry]\nType=Application\n\
         # scripts/install-payload.sh rewrites the Exec line below\n\
         Exec=/home/u/.local/bin/tobii-gtk\nIcon=com.tobiilinux.Configuration\n",
    );
    t.put(ICON, "<svg/>");
}

/// A fake binary "answers --version" with the first line of its contents.
fn by_content(p: &Path) -> Option<String> {
    std::fs::read_to_string(p)
        .ok()?
        .lines()
        .next()
        .map(str::to_string)
}

fn unowned(_: &Path) -> Ownership {
    Ownership::None
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
    let writable = |_: &Path| true;
    let probes = Probes {
        owner,
        identify: &by_content,
        writable: &writable,
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

#[test]
fn a_v030_install_with_no_manifest_is_found_through_the_menu_entry() {
    let t = Tree::new("v030");
    v030_install(&t);
    t.put(
        AUTOSTART,
        &autostart::entry_text("/home/u/.local/bin/tobii-gtk"),
    );

    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert!(p.refusal.is_none(), "{:?}", p.refusal);
    let r = removals(&p);
    for want in [
        format!("{BIN}/tobii"),
        format!("{BIN}/tobii-gtk"),
        ENTRY.to_string(),
        ICON.to_string(),
        AUTOSTART.to_string(),
    ] {
        assert!(r.contains(&want), "{want} is missing from {r:#?}");
    }
    let loc = p
        .locations
        .iter()
        .find(|l| l.dir == Path::new(BIN))
        .expect("the bin dir was found");
    assert!(loc.via.contains(&Via::MenuEntry), "{:?}", loc.via);
    assert_eq!(loc.verdict, Verdict::Remove);
    // Identified, not trusted: both answered with their own name.
    assert!(loc.binaries.iter().all(|b| b.planned && b.answer.is_some()));
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
    assert!(removals(&p).contains(&AUTOSTART.to_string()), "{p:#?}");
    let why = &p
        .remove
        .iter()
        .find(|r| r.path == Path::new(AUTOSTART))
        .unwrap()
        .what;
    assert!(why.contains("no longer exists"), "{why}");
    // /usr/bin was looked at through the entry and holds nothing of ours.
    let usr = p
        .locations
        .iter()
        .find(|l| l.dir == Path::new("/usr/bin"))
        .expect("looked at");
    assert_eq!(usr.verdict, Verdict::NothingThere);
}

/// Removing an entry that a remaining install uses would silently switch its
/// start-at-login off.
#[test]
fn an_autostart_entry_for_a_copy_that_stays_is_kept() {
    let t = Tree::new("keep-autostart");
    v030_install(&t);
    t.put("/usr/bin/tobii-gtk", "tobii-gtk 0.3.0\n");
    t.put(AUTOSTART, &autostart::entry_text("/usr/bin/tobii-gtk"));
    let pacman = |d: &Path| {
        if d.ends_with("usr/bin") {
            Ownership::Package {
                manager: "pacman".into(),
                package: "tobii-linux".into(),
            }
        } else {
            Ownership::None
        }
    };

    let p = plan_in(&t, &user(), &Options::default(), &pacman);
    assert!(!removals(&p).contains(&AUTOSTART.to_string()), "{p:#?}");
    assert!(kept(&p).contains(&AUTOSTART.to_string()));
    // The home copy still goes.
    assert!(removals(&p).contains(&format!("{BIN}/tobii-gtk")));

    // The same for a copy that is kept because it would not identify itself.
    let t = Tree::new("keep-autostart-unidentified");
    t.put("/opt/other/tobii-gtk", "something else entirely\n");
    t.put(AUTOSTART, &autostart::entry_text("/opt/other/tobii-gtk"));
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert!(kept(&p).contains(&AUTOSTART.to_string()), "{p:#?}");
    assert!(kept(&p).contains(&"/opt/other/tobii-gtk".to_string()));
    assert!(removals(&p).is_empty(), "{:#?}", removals(&p));
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
        // The menu entry still runs the packaged copy, so it stays.
        assert!(kept(&p).contains(&ENTRY.to_string()));
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
    t.put(&format!("{BIN}/tobii"), "tobii 0.4.0\n");
    t.put(&format!("{BIN}/tobii-gtk"), "\x7fELF not answering\n");
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

    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    let r = removals(&p);
    assert!(r.contains(&format!("{BIN}/tobii")));
    assert!(r.contains(&format!("{BIN}/tobii-gtk")));
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
    // Everything the manifest pointed at is going, so the manifest goes too.
    assert_eq!(p.manifests.len(), 1);
    assert!(p.manifests[0].then_empty);
    assert!(p
        .rmdirs
        .contains(&PathBuf::from("/home/u/.local/share/tobii-linux")));

    // An INFERRED copy that was updated is found and removed the same way —
    // it is identified by name, not by version.
    let t = Tree::new("scratch-inferred");
    v030_install(&t);
    t.put(&format!("{BIN}/tobii"), "tobii 0.9.9\n");
    let p = plan_in(&t, &user(), &Options::default(), &unowned);
    assert!(removals(&p).contains(&format!("{BIN}/tobii")));
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

    // --system is for root only.
    let sys = Options {
        system: true,
        ..Options::default()
    };
    assert!(plan_in(&t, &user(), &sys, &unowned).refusal.is_some());

    // A past `sudo ./install.sh` left a copy in /root.
    t.put("/root/.local/bin/tobii", "tobii 0.3.0\n");
    t.put("/root/.local/bin/tobii-gtk", "tobii-gtk 0.3.0\n");
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
    t.put("/usr/local/bin/tobii", "tobii 0.4.0\n");
    t.put("/usr/local/bin/tobii-gtk", "tobii-gtk 0.4.0\n");
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
        t.put(&format!("{dir}/tobii"), "tobii 0.3.0\n");
        t.put(&format!("{dir}/tobii-gtk"), "tobii-gtk 0.3.0\n");
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
    let loc = p
        .locations
        .iter()
        .find(|l| l.dir == Path::new(BIN))
        .unwrap();
    assert!(loc.via.contains(&Via::ThisProgram) && loc.via.contains(&Via::MenuEntry));
}

#[test]
fn the_process_scan_skips_itself_and_its_parent_and_sees_deleted_binaries() {
    let t = Tree::new("procs");
    v030_install(&t);
    let proc = |pid, uid, exe: &str, args: &[&str]| Proc {
        pid,
        uid,
        exe: exe.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
    };
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
    let p = plan_procs(&t, &user(), &Options::default(), &unowned, &procs);
    let pids: Vec<u32> = p.stop.iter().map(|x| x.pid).collect();
    assert_eq!(pids, vec![10, 11]);
    // Another hub is running from somewhere that stays: D-Bus is not asked.
    assert_eq!(p.other_hubs.iter().map(|x| x.pid).collect::<Vec<_>>(), [13]);
    let game = describe_proc(&p.stop[1]);
    assert!(game.contains("running game's head tracking"), "{game}");
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
    t.put("/usr/bin/tobii", "tobii 0.3.0\n");
    t.put(AUTOSTART, &autostart::entry_text("/usr/bin/tobii-gtk"));
    t.put("/home/u/.config/tobii-linux/calibration.bin", "cal");
    t.put(
        "/home/u/.config/tobii-linux/calibration.bin.baseline-20260810",
        "mine",
    );
    let owner = |d: &Path| {
        if d.ends_with("usr/bin") {
            Ownership::Package {
                manager: "pacman".into(),
                package: "tobii-linux".into(),
            }
        } else {
            Ownership::None
        }
    };
    let opts = Options {
        purge: true,
        ..Options::default()
    };
    let p = plan_in(&t, &user(), &opts, &owner);
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
