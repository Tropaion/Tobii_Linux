//! The report `tobii debug` prints and the hub's **Copy diagnostics** button
//! copies — everything an issue report needs, and nothing that identifies you.
//!
//! Shared by both binaries on purpose: a GUI that produced a different report
//! from the CLI would mean triage depending on which one the reporter happened
//! to run.
//!
//! # Why this is text you paste, not a file you attach
//!
//! GitHub issue forms can mark a field **required**, but they have no
//! file-upload field type at all: there is no way to make an attachment
//! mandatory. A required `textarea` is the only thing that can actually be
//! enforced, so this is built to be pasted — compact, plain, and readable by
//! the person pasting it. `--file` exists for anyone who would rather send a
//! file, but the issue form asks for the text.
//!
//! # What is deliberately left out
//!
//! Whatever this prints ends up on a public issue tracker, so it is written on
//! the assumption that the reader is a stranger:
//!
//! * **No calibration data.** A calibration blob is a fitted model of one
//!   person's eyes. Its metadata — when, which mode, how it scored — is useful
//!   and safe; the blob is neither.
//! * **No monitor serial.** `setup_monitor_id` is derived from the EDID's
//!   manufacturer, product *and serial number*. It is reported as a short hash
//!   so two reports from the same screen can still be recognised as such,
//!   without publishing the serial.
//! * **No username, home directory or hostname.** Paths are printed relative to
//!   `~`, which is also what makes two reports comparable.
//!
//! The report ends with a line saying all of this, so the person pasting it can
//! see what they are agreeing to rather than having to trust the tool.

pub mod log;

use std::fmt::Write as _;
use std::path::Path;

/// The device this project is for.
const VID: &str = "2104";
const PID: &str = "0313";

/// Build the report.
pub fn report() -> String {
    let mut o = String::with_capacity(2048);
    let _ = writeln!(o, "tobii-linux diagnostics");
    let _ = writeln!(o, "=======================");
    let _ = writeln!(o, "version    {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(o, "install    {}", install_kind());
    let _ = writeln!(o, "target     {}", tobii_update::Target::triple());

    let _ = writeln!(o, "\nsystem");
    let _ = writeln!(o, "  {:<8} {}", "distro", distro());
    let _ = writeln!(
        o,
        "  {:<8} {}",
        "kernel",
        first_line("/proc/sys/kernel/osrelease")
    );
    let _ = writeln!(o, "  {:<8} {}", "glibc", glibc_version());
    let _ = writeln!(o, "  {:<8} {}", "session", session());

    let _ = writeln!(o, "\nlibraries");
    for m in ["gtk4", "gtk4-layer-shell-0", "libusb-1.0"] {
        let _ = writeln!(o, "  {:<18} {}", m, pkg_config_version(m));
    }
    for t in ["curl", "wget", "tar"] {
        let _ = writeln!(
            o,
            "  {:<18} {}",
            t,
            if have(t) { "yes" } else { "NOT FOUND" }
        );
    }

    let _ = writeln!(o, "\ndevice");
    let _ = writeln!(o, "  {:<16} {}", format!("usb {VID}:{PID}"), usb_present());
    let _ = writeln!(o, "  {:<16} {}", "udev rule", udev_rule());

    // Under sudo this whole block would be a lie, so it does not get printed.
    //
    // `config_path()` resolves $XDG_CONFIG_HOME then $HOME, and sudo's
    // `env_reset` drops the XDG variables and sets HOME=/root — so every loader
    // looks in /root, finds nothing, and the report states that absence as a
    // confident diagnosis. Measured:
    //
    //   $ env -u XDG_CONFIG_HOME HOME=/root SUDO_USER=me tobii debug
    //     display area     NOT CONFIGURED — the tracker reports no eyes without it
    //     calibration      none saved
    //     head-pose model  not installed …
    //
    // — six false statements, on a machine where all six are configured. And
    // this is a flow the tool invites: the same report tells people the tracker
    // "needs root" without the udev rule. Reading the invoking user's config
    // instead would make the report describe a configuration this process is
    // not using, which is a different kind of wrong; saying nothing is the only
    // honest answer.
    let _ = writeln!(o, "\nconfiguration");
    let elevated = invoking_user();
    let salted = if let Some(user) = &elevated {
        let _ = writeln!(
            o,
            "  NOT READ — running under sudo as {user}, so every loader looked in {}\n  \
             instead of that user's home. Every line here would read \"not configured\":\n  \
             truthfully about that home, falsely about the machine. Run `tobii debug`\n  \
             WITHOUT sudo to get this section.",
            // Folded: sudo usually leaves HOME=/root, which names nobody, but
            // `sudo -u other` does not — and this report is pasted in public.
            tilde(&std::env::var("HOME").unwrap_or_else(|_| "/root".into()))
        );
        let _ = writeln!(o, "  {:<16} {}", "update check", update_check());
        false
    } else {
        let _ = writeln!(o, "  {:<16} {}", "display area", display_area());
        // Established once and used twice: the monitor line marks itself when
        // the hash is unsalted, and the footer must not promise a redaction
        // that did not happen.
        let salted = !report_salt().is_empty();
        let _ = writeln!(o, "  {:<16} {}", "monitor", monitor(salted));
        let _ = writeln!(o, "  {:<16} {}", "calibration", calibration());
        let _ = writeln!(o, "  {:<16} {}", "enabled eye", enabled_eye());
        let _ = writeln!(o, "  {:<16} {}", "pitch offset", pitch_offset());
        let _ = writeln!(o, "  {:<16} {}", "update check", update_check());
        let _ = writeln!(o, "  {:<16} {}", "head-pose model", head_model());
        salted
    };

    let tail = recent_log();
    // Say when the log is not the program's own. `TOBII_LOG_FILE` points the
    // log anywhere, which is what lets the tests use a file of their own — but
    // it also means these lines can be any file's, and a reader who assumes
    // otherwise is being misled by the report rather than by the person who
    // pasted it.
    let source = if std::env::var_os("TOBII_LOG_FILE").is_some() {
        " — from TOBII_LOG_FILE, not the usual log"
    } else {
        ""
    };
    if tail.is_empty() {
        let _ = writeln!(
            o,
            "\nrecent log     (empty — nothing has been logged yet{source})"
        );
    } else {
        let _ = writeln!(
            o,
            "\nrecent log ({} lines, newest last{source})",
            tail.len()
        );
        for line in &tail {
            let _ = writeln!(o, "  {line}");
        }
    }

    let _ = writeln!(
        o,
        "\nredacted: username, home path, hostname, monitor serial ({}).\n\
         no calibration data is included — only when it was made and how.",
        if elevated.is_some() {
            "no monitor id was read at all"
        } else if salted {
            "salted hash above"
        } else if matches!(tobii_config::load_setup_monitor_id(), Ok(Some(_))) {
            // There IS a monitor line, and it carries the UNSALTED marker.
            "hashed above, but see the warning on that line"
        } else {
            // No monitor was recorded, so no hash was printed and there is no
            // warning to point at. The footer used to send the reader looking
            // for one.
            "no monitor id was recorded"
        }
    );
    o
}

/// The last few log lines, from this process and from the file.
///
/// Both, because they answer different questions: the ring buffer has what THIS
/// process just did, and the file has what the *hub* did — which is the half a
/// user cannot see, since a GUI launched from the menu writes its stderr to the
/// journal or to nothing.
///
/// Paths are folded to `~` here rather than when written, so a local reader
/// still sees real paths in the file itself.
fn recent_log() -> Vec<String> {
    const KEEP: usize = 15;
    let mut lines = log::tail_file(KEEP);
    for l in log::recent(KEEP) {
        if !lines.contains(&l) {
            lines.push(l);
        }
    }
    let start = lines.len().saturating_sub(KEEP);
    // `sane` as well as `tilde`: lines written by THIS program are already
    // one printable line each, but the file is a plain path and these are read
    // in a terminal and pasted into a form.
    lines[start..].iter().map(|l| sane(&tilde(l))).collect()
}

/// How this copy was installed, which decides who should update it.
fn install_kind() -> String {
    let Ok(dir) = tobii_update::install::install_dir() else {
        return "unknown".into();
    };
    use tobii_update::install::Ownership;
    // Asked once per process. Three subprocess spawns dominate the cost of
    // building this report — measured at ~120 ms, on the GTK main thread when
    // the settings popover's save or copy button is pressed — and the answer
    // cannot change while the program runs: it is a property of how this binary
    // was installed, and installing over a running binary is what the updater
    // refuses to do.
    static OWNER: std::sync::OnceLock<Ownership> = std::sync::OnceLock::new();
    match OWNER.get_or_init(|| tobii_update::install::package_owner(&dir.join("tobii"))) {
        Ownership::Package { manager, package } => {
            return format!("package `{package}` via {manager}")
        }
        // Worth printing rather than hiding: it is also why the updater will
        // refuse, so a report that omitted it would not explain the refusal
        // the user is filing an issue about.
        Ownership::Unknown { manager, why } => {
            return format!("unknown — {manager} could not be asked ({why})")
        }
        Ownership::None => {}
    }
    if tobii_update::install::is_build_tree(&dir) {
        return format!("build tree ({})", safe_path(&dir));
    }
    format!("unmanaged ({})", safe_path(&dir))
}

/// A path that cannot carry a username, whatever `$HOME` happens to be.
///
/// [`tilde`] folds the home directory, which is the whole redaction as long as
/// the home the program is running under is the home the binary sits in. Under
/// `sudo` it is not: `env_reset` sets `HOME=/root` while the binary is still
/// under the real user's home, and this very report tells people the tracker
/// "needs root" without the udev rule — so `sudo tobii debug` is a flow the
/// tool itself invites. Measured before this existed:
///
/// ```text
/// $ env HOME=/root tobii debug
/// install    build tree (/home/tropaion/Dokumente/Git/TobiiLinux/target/release)
/// redacted: username, home path, hostname, monitor serial (…)
/// ```
///
/// The footer was making a promise the line above it had already broken.
/// [`home_spellings`] now resolves the invoking user's home too, which covers
/// sudo properly and keeps the *useful* answer (`~/.local/bin`). This is the
/// backstop for everything that resolution cannot reach: a home somewhere no
/// convention names, `su - other`, a container with a rewritten passwd. If the
/// path did not fold and is not under a directory that belongs to the system,
/// only its last two components go out — enough to recognise `bin/release` or
/// `.local/bin`, never enough to name anybody.
fn safe_path(dir: &std::path::Path) -> String {
    let folded = tilde(&dir.display().to_string());
    // Folded, or somewhere every machine has the same: publish it as it is.
    const SYSTEM: [&str; 6] = ["/usr/", "/opt/", "/bin/", "/sbin/", "/snap/", "/nix/"];
    if folded.starts_with('~') || SYSTEM.iter().any(|p| folded.starts_with(p)) {
        return folded;
    }
    let tail: Vec<&std::ffi::OsStr> = dir
        .iter()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let joined = tail
        .iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    format!("…/{joined}")
}

fn distro() -> String {
    let Ok(s) = std::fs::read_to_string("/etc/os-release") else {
        return "unknown".into();
    };
    let field = |k: &str| {
        s.lines()
            .find_map(|l| l.strip_prefix(k))
            .map(|v| v.trim_matches('"').to_string())
    };
    match (field("PRETTY_NAME="), field("ID=")) {
        (Some(p), Some(i)) => format!("{} [{}]", sane(&p), sane(&i)),
        (Some(p), None) => sane(&p),
        _ => "unknown".into(),
    }
}

/// The runtime glibc, which is what decides whether a release binary starts.
fn glibc_version() -> String {
    // `ldd --version` is the portable way to ask, and it is what the release
    // workflow's floor is compared against.
    let Ok(out) = std::process::Command::new("ldd").arg("--version").output() else {
        return "unknown".into();
    };
    sane(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().last())
            .unwrap_or("unknown"),
    )
}

/// Wayland or X11, and which desktop — the two things every GUI bug depends on.
fn session() -> String {
    let t = sane(&std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into()));
    let d = sane(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".into()));
    let w = std::env::var("WAYLAND_DISPLAY").is_ok();
    format!("{t} / {d}{}", if w { "" } else { " (no WAYLAND_DISPLAY)" })
}

fn pkg_config_version(module: &str) -> String {
    let Ok(out) = std::process::Command::new("pkg-config")
        .args(["--modversion", module])
        .output()
    else {
        return "pkg-config missing".into();
    };
    if out.status.success() {
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        "NOT FOUND".into()
    }
}

fn have(prog: &str) -> bool {
    std::process::Command::new(prog)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Whether the tracker is on the bus, read from sysfs.
///
/// Deliberately NOT by opening it: a diagnostic that claimed the USB interface
/// would take the device away from a running hub or game — and "the tracker is
/// busy" is exactly the state somebody is most likely to be reporting.
fn usb_present() -> String {
    let Ok(entries) = std::fs::read_dir("/sys/bus/usb/devices") else {
        return "unknown (no sysfs)".into();
    };
    for e in entries.flatten() {
        let vid = std::fs::read_to_string(e.path().join("idVendor"));
        let pid = std::fs::read_to_string(e.path().join("idProduct"));
        if let (Ok(v), Ok(p)) = (vid, pid) {
            if v.trim() == VID && p.trim() == PID {
                return "present".into();
            }
        }
    }
    "NOT FOUND on the bus".into()
}

/// Whether the udev rule is installed — and whether it is the one that works.
///
/// Both names are looked for. The rule shipped before v0.1.0 was called
/// `99-tobii.rules`, and the number is not cosmetic: udev reads rule files in
/// lexical order, and `73-seat-late.rules` is what turns `TAG+="uaccess"` into
/// an ACL, so a tag set at 99 arrives after the only rule that reads it.
/// Measured on a machine with the old file installed and the tracker plugged
/// in: `CURRENT_TAGS=:uaccess:` and no ACL on the device node at all. The rule
/// "worked" only because it also said `MODE="0666"`, which is a different and
/// much broader grant. A leftover copy of the old file still wins on mode, so
/// it is worth naming in the report rather than passing as installed.
fn udev_rule() -> String {
    const DIRS: [&str; 3] = [
        "/etc/udev/rules.d",
        "/usr/lib/udev/rules.d",
        "/lib/udev/rules.d",
    ];
    let found = |name: &str| {
        DIRS.iter()
            .map(|d| format!("{d}/{name}"))
            .find(|p| Path::new(p).exists())
    };
    match (found("60-tobii.rules"), found("99-tobii.rules")) {
        (Some(new), Some(old)) => format!(
            "installed ({new}) — but {old} is still there and overrides its mode; delete it"
        ),
        (Some(new), None) => format!("installed ({new})"),
        (None, Some(old)) => format!(
            "installed ({old}), the OLD rule — its uaccess tag is set too late to \
             have any effect; replace it with 60-tobii.rules"
        ),
        (None, None) => "NOT INSTALLED — the tracker needs root without it".into(),
    }
}

fn display_area() -> String {
    match tobii_config::load() {
        Ok(Some(s)) => {
            let c = s.to_corners();
            let w = (c.tr[0] - c.tl[0]).abs().round();
            let h = (c.tl[1] - c.bl[1]).abs().round();
            format!("configured, about {w:.0}x{h:.0} mm")
        }
        Ok(None) => "NOT CONFIGURED — the tracker reports no eyes without it".into(),
        Err(e) => format!("unreadable ({e})"),
    }
}

/// The monitor identity, hashed.
///
/// The raw value contains the EDID serial number. Two reports from the same
/// screen still hash the same, which is what makes it useful, without putting a
/// serial on a public issue.
fn monitor(salted: bool) -> String {
    match tobii_config::load_setup_monitor_id() {
        Ok(Some(id)) => {
            // Say so when the salt could not be established: an unsalted digest
            // of a monitor id is recoverable in well under a second, and the
            // user is about to paste this somewhere public.
            let warn = if salted {
                ""
            } else {
                "  (UNSALTED — this hash is reversible; delete the line if it matters to you)"
            };
            format!("id {}{warn}", short_hash(&id))
        }
        Ok(None) => "not recorded".into(),
        Err(e) => format!("unreadable ({e})"),
    }
}

fn calibration() -> String {
    match tobii_config::load_calibration() {
        // The blob's LENGTH is diagnostic (a short one is a failed
        // calibration); its contents are a model of somebody's eyes.
        Ok(Some((blob, meta))) => {
            let age = meta
                .as_ref()
                .map(|m| {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    let days = (now - m.created_utc) / 86_400;
                    format!(", {days} day(s) old")
                })
                .unwrap_or_default();
            let mode = meta
                .as_ref()
                .map(|m| format!(", mode {}", m.mode))
                .unwrap_or_default();
            format!("present, {} bytes{mode}{age}", blob.len())
        }
        Ok(None) => "none saved".into(),
        Err(e) => format!("unreadable ({e})"),
    }
}

fn enabled_eye() -> String {
    match tobii_config::load_enabled_eye() {
        Ok(Some(e)) => format!("{e:?}"),
        Ok(None) => "not set (both)".into(),
        Err(e) => format!("unreadable ({e})"),
    }
}

fn pitch_offset() -> String {
    match tobii_config::load_pitch_offset() {
        Ok(Some(v)) => format!("{v:+.2}°"),
        Ok(None) => "not calibrated — pitch has no zero".into(),
        Err(e) => format!("unreadable ({e})"),
    }
}

fn update_check() -> String {
    let forced_off = std::env::var_os("TOBII_NO_UPDATE_CHECK").is_some_and(|v| v != "0");
    match (tobii_config::update_check_enabled(), forced_off) {
        (_, true) => "off (TOBII_NO_UPDATE_CHECK)".into(),
        (true, _) => "on".into(),
        (false, _) => "off".into(),
    }
}

fn head_model() -> String {
    use tobii_headpose::model_store::{status_quick, Status, HEAD_POSE};
    // `status_quick`, not `status`: the full check hashes 13 MB, and a
    // diagnostic should not make the user wait to find out what is installed.
    match status_quick(&HEAD_POSE) {
        Status::Ready => "installed".into(),
        Status::Missing => "not installed — head tracking is 5 DOF, pitch reads 0".into(),
        Status::Corrupt { .. } => "present but NOT the expected bytes".into(),
    }
}

/// One line, printable, and short.
///
/// Everything in this report is read by a person on an issue tracker, and some
/// of it comes from outside the program: environment variables, /etc/os-release,
/// the kernel version, log lines. A value with a newline in it can add a whole
/// convincing section to the report —
/// `XDG_CURRENT_DESKTOP="KDE\n\nudev rule       installed"` — and one with an
/// ANSI escape can hide what is already there from anybody reading it in a
/// terminal. Neither is an attack worth much on its own; both make the report
/// less trustworthy than it claims to be, which is the only thing it has.
///
/// So: control characters out, and a length cap. 200 characters is longer than
/// any real value here and short enough that a runaway one cannot push the rest
/// of the report out of view.
fn sane(v: &str) -> String {
    const MAX: usize = 200;
    let mut out: String = v
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    out = out.trim().to_string();
    if out.chars().count() > MAX {
        out = out.chars().take(MAX).collect::<String>() + "…";
    }
    out
}

/// A short, stable stand-in for a value that must not be published verbatim.
///
/// **Salted, per install.** An unsalted hash of a monitor id is not a redaction
/// — it is an encoding. `edid_monitor_id` is `PNP(3 letters) + product(4 hex) +
/// "-" + serial`, and real serials are short, structured and sequential within a
/// batch, so the search space is around 2^20. Measured: the maintainer's own id
/// was recovered from its 8-hex-character digest in **0.07 seconds** on one
/// core, uniquely. And for this repository in particular no search was needed at
/// all — `SAM7454-HNTY900001` is committed as a test fixture in six places, so
/// `grep` reversed it.
///
/// The salt is 32 random bytes generated once per installation and kept in the
/// config directory. It preserves the only property this hash was ever for —
/// two reports from the same machine carry the same id, so a triager can see
/// they are the same screen — while making the value meaningless to anybody
/// else. A salt compiled into the binary would not help: the binary is public.
fn short_hash(s: &str) -> String {
    let mut input = report_salt();
    input.extend_from_slice(s.as_bytes());
    let full = tobii_config::sha256::hex_digest(&input);
    format!("{}…", &full[..8])
}

/// The per-install salt, created on first use.
///
/// Falls back to a fixed value only if the salt can neither be read nor written
/// — in which case the hash is no better than before, so the report says so
/// rather than pretending. See [`monitor`].
fn report_salt() -> Vec<u8> {
    let path = tobii_config::config_path().with_file_name("report_salt");
    if let Ok(existing) = std::fs::read(&path) {
        if existing.len() >= 16 {
            return existing;
        }
    }
    // 32 bytes from the kernel. `read_exact` on a bounded buffer, NOT
    // `fs::read` — /dev/urandom is an endless stream, so reading it to EOF
    // never returns, and a diagnostics button that hangs the hub forever would
    // be a worse bug than the one this salt fixes.
    let mut fresh = [0u8; 32];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut fresh))
        .is_ok()
    {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&path, fresh).is_ok() {
            // Readable only by its owner: it is the only thing standing between
            // a published report and the serial behind it.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
            return fresh.to_vec();
        }
    }
    Vec::new()
}

/// Replace the home directory with `~`, wherever it appears and however it is
/// spelled.
///
/// Both for privacy and because it makes two reports comparable: `~/.local/bin`
/// is the same fact on every machine, `/home/someone/.local/bin` is not.
///
/// **Every occurrence, and every spelling.** Two separate bugs were found here,
/// and the second is the one that survived the fix for the first:
///
/// 1. It folded only a *prefix*, so a home path in the middle of a log line —
///    `… WARN could not write /home/someone/.config/…` — went through intact.
/// 2. It compared against the raw `$HOME` only. `install_dir` comes from
///    `current_exe()`, which reads `/proc/self/exe` and is **fully
///    symlink-resolved**, so on any system where the home is reached through a
///    link the two are spelled differently and nothing matched. That is not
///    exotic: ostree distributions ship `/home -> var/home` by default —
///    Silverblue, Kinoite, Bluefin and Bazzite, the last of which is a gaming
///    image and squarely this project's audience. `sudo`/`pkexec` does it too,
///    setting `HOME=/root` while the binary is still under the real user's home,
///    and this very report tells people the tracker "needs root" without the
///    udev rule.
///
/// So both the raw and the canonical form are folded. The crate's own redaction
/// test could not catch (2), because it compared the report against the same
/// `$HOME` the code folded with — the test and the bug shared an assumption.
fn tilde(path: &str) -> String {
    let mut out = path.to_string();
    for home in home_spellings() {
        out = out.replace(&home, "~");
    }
    fold_home_shaped(&out)
}

/// Fold anything *shaped* like a home directory, whoever it belongs to.
///
/// [`home_spellings`] can only fold homes it can name, and there are cases
/// where it can name none of them: `su - other` leaves `HOME=/root` with no
/// `SUDO_USER` to resolve, a container can have a passwd file that does not
/// describe the host, and a log line written by an earlier run carries whatever
/// the home was *then*.
///
/// That is not hypothetical, and it is not confined to install paths. The log
/// tail goes into the report verbatim, and a warning reads
/// `could not write /home/someone/.config/tobii-linux/x`. Found by a test that
/// points `$HOME` at /root and asserts the real home does not come out: it
/// failed intermittently, depending on whether the machine's real log happened
/// to contain a path at that moment. [`safe_path`] covers the install line and
/// nothing else, because a log line is free text with no last-two-components
/// to fall back to.
///
/// So this folds by shape: `/home/<name>` and `/var/home/<name>` (the ostree
/// layout) become `~`, whoever `<name>` is. Folding another user's home to `~`
/// is not strictly accurate — but the report's purpose is to name nobody, and
/// an inaccurate `~` names nobody while an accurate `/home/someone` does.
fn fold_home_shaped(text: &str) -> String {
    // Longest first, so a tie at the same offset takes the more specific shape.
    //
    // The media shapes are here because udisks2 — what mounts removable media
    // on KDE and GNOME — mounts at `/run/media/<login>/<label>`, and the older
    // Debian convention is `/media/<login>/<label>`. Saving the diagnostics
    // report to a USB stick is a natural thing to do on a machine whose GUI is
    // broken, and without these the login name goes into the log and then into
    // the next report.
    const PREFIXES: [&str; 4] = ["/var/home/", "/run/media/", "/media/", "/home/"];
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        // The EARLIEST match in the string, not the first prefix that matches
        // somewhere in it. Taking the first prefix meant a line holding both
        // shapes folded only the later one:
        //   "/home/alice/a and /var/home/bob/b"
        //     -> "/home/alice/a and ~/b"
        // because "/var/home/" was tried first and found a match, so "/home/"
        // was never tried at all — leaving a login name in a report whose
        // footer says there is none.
        let Some((prefix, at)) = PREFIXES
            .iter()
            .filter_map(|p| rest.find(p).map(|at| (*p, at)))
            .min_by_key(|&(p, at)| (at, std::cmp::Reverse(p.len())))
        else {
            out.push_str(rest);
            break;
        };
        let after = &rest[at + prefix.len()..];
        // The name runs to the next separator. A path that is exactly the
        // prefix with nothing after it names nobody.
        let end = after
            .find(|c: char| c == '/' || c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(after.len());
        if end == 0 {
            out.push_str(&rest[..at + prefix.len()]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..at]);
        out.push('~');
        rest = &after[end..];
    }
    out
}

/// Every way this machine's home directory can be written.
///
/// The raw `$HOME`, and its canonical form when they differ. Longest first, so
/// folding one cannot leave a fragment of another behind.
fn home_spellings() -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    // `$HOME`, and the home of whoever invoked `sudo`. The second is not a
    // nicety: sudo's `env_reset` sets HOME=/root, so under sudo the first one
    // names a directory the binary is not in and folds nothing — while the
    // real user's home sits in the path, in a report built to be pasted on a
    // public tracker.
    let invoking = std::env::var("SUDO_USER").ok().and_then(passwd_home);
    for raw in [std::env::var("HOME").ok(), invoking].into_iter().flatten() {
        let raw = raw.trim_end_matches('/').to_string();
        // "" or "/" names no user directory, and folding "/" would replace
        // every separator in every path — "~~.local~bin".
        if raw.len() <= 1 {
            continue;
        }
        if let Ok(canon) = std::fs::canonicalize(&raw) {
            let canon = canon
                .display()
                .to_string()
                .trim_end_matches('/')
                .to_string();
            if canon.len() > 1 {
                all.push(canon);
            }
        }
        all.push(raw);
    }
    all.sort_by_key(|s| std::cmp::Reverse(s.len()));
    all.dedup();
    all
}

/// The user who invoked `sudo`/`pkexec`, when this is running elevated AND the
/// home directory moved with it.
///
/// The point is not "am I root" but "is `$HOME` somebody else's" — that is what
/// makes every configuration lookup answer about the wrong home. `sudo -E`
/// keeps the caller's environment, so the loaders look in the right place and
/// the section is worth printing; a real root login is not somebody else's home
/// either.
fn invoking_user() -> Option<String> {
    let user = std::env::var("SUDO_USER")
        .ok()
        .filter(|u| !u.is_empty() && u != "root")?;
    let home = std::env::var("HOME").unwrap_or_default();
    (home != passwd_home(user.clone()).unwrap_or_default()).then_some(user)
}

/// A user's home directory, read out of `/etc/passwd`.
///
/// `getpwnam` would be the right call and would also see LDAP and SSSD users,
/// but it needs libc bindings this crate does not have — and the case that
/// matters here, a desktop user running `sudo tobii debug`, is in the local
/// file. Failing to resolve costs a fold, not correctness: [`safe_path`]
/// truncates whatever `tilde` could not fold.
fn passwd_home(user: String) -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == user)
            .then(|| f.nth(4))?
            .filter(|h| !h.is_empty())
            .map(str::to_string)
    })
}

fn first_line(path: &str) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.lines().next().map(sane))
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A home directory worth testing against, normalised.
    ///
    /// `None` when `HOME` names no user directory — empty, `/`, or a bare
    /// `/home` in a container. Trailing slashes are trimmed so a test does not
    /// build "/home//.config" and then blame `tilde` for the doubled separator.
    /// The lock every test in this module that touches `$HOME` must hold.
    ///
    /// One of them *writes* it — `a_home_that_is_not_where_the_binary_lives…`
    /// points HOME at /root — and `$HOME` is process-global, so a sibling
    /// reading it concurrently gets /root and fails for a reason that has
    /// nothing to do with what it is testing. It is the log tests' lock rather
    /// than a second one: two locks over the same global is a deadlock waiting
    /// for somebody to take them in the other order. A `std::sync::Mutex` is
    /// NOT reentrant, so a test that also calls `log::tests::with_own_log` —
    /// which takes this same lock — must not call this as well.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        log::tests::LOG_TEST
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn test_home() -> Option<String> {
        let h = std::env::var("HOME").ok()?;
        let h = h.trim_end_matches('/');
        (h.len() > 1).then(|| h.to_string())
    }

    /// The leak the sibling test could not see, because that test and the bug
    /// shared an assumption.
    ///
    /// `the_report_does_not_leak_who_you_are` compares the report against the
    /// same `$HOME` the code folds with, so it passes by construction whenever
    /// the two agree — and the interesting case is exactly when they do not.
    /// `sudo` sets `HOME=/root` while the binary is still under the real
    /// user's home, and this report tells people the tracker "needs root"
    /// without the udev rule, so `sudo tobii debug` is a flow the tool itself
    /// invites. Measured before the fix, with the shipped binary:
    ///
    /// ```text
    /// $ env HOME=/root tobii debug
    /// install    build tree (/home/tropaion/Dokumente/Git/TobiiLinux/target/release)
    /// redacted: username, home path, hostname, …
    /// ```
    ///
    /// So this one points `$HOME` somewhere the install is definitely NOT and
    /// asserts the real path still does not come out.
    #[test]
    fn a_home_that_is_not_where_the_binary_lives_still_does_not_leak() {
        let _env = env_lock();
        let Some(real) = test_home() else {
            return; // no home to leak
        };

        let restore = std::env::var_os("HOME");
        let sudo = std::env::var_os("SUDO_USER");
        std::env::set_var("HOME", "/root");
        std::env::remove_var("SUDO_USER");
        let r = report();
        match restore {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        if let Some(u) = sudo {
            std::env::set_var("SUDO_USER", u);
        }

        assert!(
            !r.contains(&real),
            "with HOME=/root the real home reached the report:\n{r}"
        );
        // And the report must still say something useful about the install —
        // a redaction that erased the line would pass the assertion above and
        // be a worse report.
        assert!(
            r.lines()
                .any(|l| l.starts_with("install") && l.contains('(') && !l.contains("()")),
            "the install line went missing — redacting it away is not the fix:\n{r}"
        );
    }

    /// The property that matters: whatever this prints is going onto a public
    /// issue tracker, so it must not carry the things that identify a person.
    #[test]
    fn the_report_does_not_leak_who_you_are() {
        let _env = env_lock();
        let r = report();

        // The home path, which is the leak that actually happens: it reaches
        // the report through install paths and through log lines. Checked as a
        // whole string, which `tilde` is responsible for folding everywhere.
        //
        // There is NO separate "is the username in the report" probe, and that
        // is deliberate — one was tried twice and produced only false alarms.
        // Searching for the last path segment of HOME matches ordinary English
        // once HOME is `/home` in a container (`home` matched this test's own
        // redaction footer, which is what broke CI), and searching for it as a
        // path component matches unrelated paths once the username is short
        // (`x/` matches `TobiiLinux/`). A test that cries wolf teaches people
        // to ignore it, which costs more than the case it might have caught —
        // and nothing in the report prints a bare username anyway: every route
        // a username could take is *through a path*, which the check below
        // covers.
        if let Some(home) = test_home() {
            assert!(
                !r.contains(&home),
                "the home directory is in the report:\n{r}"
            );
        }
        let host = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
        let host = host.trim();
        if host.len() > 2 {
            assert!(!r.contains(host), "the hostname is in the report:\n{r}");
        }
        // The monitor id is hashed, never printed raw.
        if let Ok(Some(id)) = tobii_config::load_setup_monitor_id() {
            assert!(
                !r.contains(&id),
                "the raw monitor id is in the report:\n{r}"
            );
        }
    }

    /// The point of the whole logging module: a warning the HUB wrote must
    /// reach a report the CLI produces, because they are separate processes and
    /// the hub's warnings are the ones a user cannot otherwise see — a GUI
    /// launched from the application menu writes stderr to the journal or to
    /// nothing.
    #[test]
    fn a_warning_from_another_process_reaches_the_report() {
        // The ring buffer is per-process, so this covers the in-process half;
        // the file half is what carries it across, and `recent_log` reads both.
        let r = log::tests::with_own_log("report", || {
            log::warn("test: a warning that must show up in the report");
            report()
        });
        assert!(
            r.contains("a warning that must show up in the report"),
            "the log tail is missing from the report:\n{r}"
        );
        assert!(r.contains("recent log"), "{r}");
    }

    /// Log lines go into the report, so they get the same redaction as
    /// everything else — a warning naming a path must not publish a home
    /// directory.
    #[test]
    fn a_log_line_containing_a_home_path_is_folded_in_the_report() {
        // `$HOME` is read INSIDE the closure, not before it. `with_own_log`
        // is what takes the env lock, so reading it out here races the sibling
        // test that points HOME at /root — which made this fail about one run
        // in four, with a log line naming /root and an assertion blaming the
        // fold. No `env_lock()` of its own: it is the same mutex, and
        // std::sync::Mutex is not reentrant.
        let Some((home, r)) = log::tests::with_own_log("redact", || {
            let home = test_home()?;
            log::warn(&format!(
                "test: could not write {home}/.config/tobii-linux/x"
            ));
            Some((home, report()))
        }) else {
            return;
        };
        assert!(
            !r.contains(&home),
            "the home path leaked through the log:\n{r}"
        );
        assert!(r.contains("~/.config/tobii-linux/x"), "{r}");
    }

    /// It has to be pasteable into a GitHub issue form, which is the only place
    /// it can actually be required. A wall of text is not.
    #[test]
    fn the_report_is_small_enough_to_paste() {
        let r = report();
        // The fixed part is ~35 lines; the log tail adds at most KEEP more.
        // Both together still paste into an issue form without scrolling being
        // a problem, which is the actual requirement.
        assert!(
            r.lines().count() < 60,
            "{} lines is too long to paste",
            r.lines().count()
        );
        assert!(r.len() < 8192, "{} bytes is too long to paste", r.len());
    }

    /// Every section a triager needs, so a report can be read at a glance and
    /// nobody has to ask a follow-up question that this could have answered.
    #[test]
    fn the_report_answers_the_questions_a_triager_would_ask() {
        let r = report();
        for expected in [
            "version",
            "install",
            "target",
            "distro",
            "kernel",
            "glibc",
            "session",
            "gtk4",
            "libusb",
            "usb 2104:0313",
            "udev rule",
            "display area",
            "calibration",
            "enabled eye",
            "pitch offset",
            "head-pose model",
        ] {
            assert!(
                r.contains(expected),
                "the report never mentions {expected:?}"
            );
        }
        // And it says what it left out, so the person pasting it can see.
        assert!(r.contains("redacted"), "{r}");
        assert!(r.contains("no calibration data"), "{r}");
    }

    /// The mechanism the redaction actually rests on, tested directly rather
    /// than inferred from the whole report. Every occurrence, not just a
    /// prefix — a log line carries the home path in the middle.
    #[test]
    fn every_occurrence_of_the_home_path_is_folded() {
        let _env = env_lock();
        let Some(home) = test_home() else {
            return; // no meaningful home to fold — see `tilde`
        };
        let line = format!("WARN could not write {home}/.config/x, retrying {home}/.config/x");
        let folded = tilde(&line);
        assert!(!folded.contains(&home), "{folded}");
        assert_eq!(folded.matches("~/.config/x").count(), 2, "{folded}");
    }

    /// The fold that does not need to know whose home it is.
    ///
    /// `home_spellings` can only fold homes it can name, and `su - other`,
    /// a container with a foreign passwd file, and a log line written by an
    /// earlier run all defeat it.
    #[test]
    fn a_home_shaped_path_is_folded_whoever_it_belongs_to() {
        let _env = env_lock();
        assert_eq!(tilde("/home/someone-else/.config/x"), "~/.config/x");
        // The ostree layout, which Silverblue, Kinoite, Bluefin and Bazzite
        // all ship.
        assert_eq!(tilde("/var/home/someone/.local/bin"), "~/.local/bin");
        // In the middle of a sentence, which is where log lines put it.
        assert_eq!(
            tilde("WARN could not write /home/bob/.config/tobii-linux/x — giving up"),
            "WARN could not write ~/.config/tobii-linux/x — giving up"
        );
        // Twice in one line.
        assert_eq!(tilde("/home/a/x -> /home/b/y"), "~/x -> ~/y");
        // Nothing that is not a home.
        assert_eq!(tilde("/usr/bin"), "/usr/bin");
        assert_eq!(tilde("/homework/notes"), "/homework/notes");
        // A name with no path after it still folds.
        assert_eq!(tilde("/home/bob"), "~");
        // udisks2 mounts removable media at /run/media/<login>/<label>; the
        // older Debian convention is /media/<login>/<label>. Saving the report
        // to a USB stick used to write the login name into the log, and the
        // log tail goes into the next report.
        assert_eq!(
            tilde("/run/media/bob/STICK/report.txt"),
            "~/STICK/report.txt"
        );
        assert_eq!(tilde("/media/bob/STICK/report.txt"), "~/STICK/report.txt");
        // And "/var/home/x" must not be matched as the shorter "/home/x".
        assert_eq!(tilde("/var/home/bob/x"), "~/x");
        // Both shapes on one line, the /home/ one FIRST. Taking the first
        // prefix that matched anywhere left this one unfolded.
        assert_eq!(tilde("/home/alice/a and /var/home/bob/b"), "~/a and ~/b");
        assert_eq!(tilde("/media/bob/x and /home/bob/y"), "~/x and ~/y");
        // Degenerate input must terminate and not eat the string.
        assert_eq!(tilde("/home/"), "/home/");
        assert_eq!(tilde(""), "");
    }

    #[test]
    fn a_home_relative_path_is_reported_relative_to_home() {
        let _env = env_lock();
        if let Some(home) = test_home() {
            assert_eq!(tilde(&format!("{home}/.local/bin")), "~/.local/bin");
        }
        assert_eq!(tilde("/usr/bin"), "/usr/bin");
    }

    #[test]
    fn the_monitor_hash_is_stable_and_short() {
        let a = short_hash("MON-12345-SERIAL");
        assert_eq!(a, short_hash("MON-12345-SERIAL"), "must be stable");
        assert_ne!(a, short_hash("MON-99999-SERIAL"));
        assert_eq!(a.chars().count(), 9, "8 hex plus the ellipsis: {a}");
    }
}
