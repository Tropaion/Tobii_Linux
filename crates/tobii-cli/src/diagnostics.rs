//! `tobii debug` — everything an issue report needs, and nothing that
//! identifies you.
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

    let _ = writeln!(o, "\nconfiguration");
    let _ = writeln!(o, "  {:<16} {}", "display area", display_area());
    let _ = writeln!(o, "  {:<16} {}", "monitor", monitor());
    let _ = writeln!(o, "  {:<16} {}", "calibration", calibration());
    let _ = writeln!(o, "  {:<16} {}", "enabled eye", enabled_eye());
    let _ = writeln!(o, "  {:<16} {}", "pitch offset", pitch_offset());
    let _ = writeln!(o, "  {:<16} {}", "update check", update_check());
    let _ = writeln!(o, "  {:<16} {}", "head-pose model", head_model());

    let _ = writeln!(
        o,
        "\nredacted: username, home path, hostname, monitor serial (hashed above).\n\
         no calibration data is included — only when it was made and how."
    );
    o
}

/// How this copy was installed, which decides who should update it.
fn install_kind() -> String {
    let Ok(dir) = tobii_update::install::install_dir() else {
        return "unknown".into();
    };
    if let Some((mgr, pkg)) = tobii_update::install::package_owner(&dir.join("tobii")) {
        return format!("package `{pkg}` via {mgr}");
    }
    if tobii_update::install::is_build_tree(&dir) {
        return format!("build tree ({})", tilde(&dir.display().to_string()));
    }
    format!("unmanaged ({})", tilde(&dir.display().to_string()))
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
        (Some(p), Some(i)) => format!("{p} [{i}]"),
        (Some(p), None) => p,
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
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().last())
        .unwrap_or("unknown")
        .to_string()
}

/// Wayland or X11, and which desktop — the two things every GUI bug depends on.
fn session() -> String {
    let t = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into());
    let d = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".into());
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

fn udev_rule() -> String {
    for p in [
        "/etc/udev/rules.d/99-tobii.rules",
        "/usr/lib/udev/rules.d/99-tobii.rules",
        "/lib/udev/rules.d/99-tobii.rules",
    ] {
        if Path::new(p).exists() {
            return format!("installed ({p})");
        }
    }
    "NOT INSTALLED — the tracker needs root without it".into()
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
fn monitor() -> String {
    match tobii_config::load_setup_monitor_id() {
        Ok(Some(id)) => format!("id {}", short_hash(&id)),
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

/// A short, stable stand-in for a value that must not be published verbatim.
fn short_hash(s: &str) -> String {
    let full = tobii_config::sha256::hex_digest(s.as_bytes());
    format!("{}…", &full[..8])
}

/// Replace the home directory with `~`.
///
/// Both for privacy and because it makes two reports comparable: `~/.local/bin`
/// is the same fact on every machine, `/home/someone/.local/bin` is not.
fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => path.replacen(&h, "~", 1),
        _ => path.to_string(),
    }
}

fn first_line(path: &str) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.to_string()))
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property that matters: whatever this prints is going onto a public
    /// issue tracker, so it must not carry the things that identify a person.
    #[test]
    fn the_report_does_not_leak_who_you_are() {
        let r = report();

        if let Ok(home) = std::env::var("HOME") {
            assert!(
                !r.contains(&home),
                "the home directory is in the report:\n{r}"
            );
            if let Some(user) = home.rsplit('/').next().filter(|u| u.len() > 2) {
                assert!(
                    !r.contains(user),
                    "the username `{user}` is in the report:\n{r}"
                );
            }
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

    /// It has to be pasteable into a GitHub issue form, which is the only place
    /// it can actually be required. A wall of text is not.
    #[test]
    fn the_report_is_small_enough_to_paste() {
        let r = report();
        assert!(
            r.lines().count() < 45,
            "{} lines is too long to paste",
            r.lines().count()
        );
        assert!(r.len() < 4096, "{} bytes is too long to paste", r.len());
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

    #[test]
    fn a_home_relative_path_is_reported_relative_to_home() {
        if let Ok(home) = std::env::var("HOME") {
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
