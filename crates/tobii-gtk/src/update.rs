//! The hub's update banner: check on launch, show the changelog, install.
//!
//! # The one automatic network request
//!
//! Everything else in this program asks first — the head-pose model store will
//! not so much as look at the network without an explicit click. This does,
//! once per launch, because knowing a fix exists is worthless if you have to
//! remember to go looking for it.
//!
//! What that buys is deliberately narrow: it fetches the release *listing*,
//! which is metadata. Nothing is downloaded and nothing on disk changes until
//! the user presses Update or Download.
//!
//! It can be switched off — in Settings, or with `TOBII_NO_UPDATE_CHECK=1` —
//! and then no request is made at all. A program that reaches out on its own
//! needs a way to say no that is not "stop using the program".
//!
//! # Which button, decided before it is shown
//!
//! An install the updater owns — the `.tar.gz` unpacked into `~/.local/bin` —
//! can be replaced in place, and the button says **Update**.
//!
//! An install a package manager owns cannot. `tobii_update::install` refuses to
//! write over a `dpkg`-, `rpm`- or `pacman`-owned file, because doing so leaves
//! the package database describing a file that is no longer there and the next
//! upgrade of the package silently reverts the change. That refusal used to
//! arrive *after* the user pressed Update and waited for the download. So the
//! ownership question is asked as part of the check, and for those copies the
//! button says **Download**: it fetches the file their own package manager can
//! install — the `.deb`, the `.rpm`, the prebuilt Arch `.pkg.tar.zst`, or, for a
//! release without one, the PKGBUILD and its hook — into a folder they pick, and
//! says what to run, with every path quoted so the command survives a paste.
//!
//! Two more cases, both found on 2026-09-11. A copy nobody owns but that sits
//! where this user cannot write — `sudo ./install.sh --system`, or one copied by
//! hand into a system directory — also gets **Download**: the archive, and the
//! one command that installs it for every user. And a copy that was replaced or
//! removed while it ran has nothing at its path to update, so it gets **Quit**:
//! relaunching would only hand off to this same process. The decision is
//! `tobii_update::install::action_for`, and nothing is written to reach it.
//!
//! # What pressing Update or Download trusts
//!
//! The checksums are published in the same release as the archive and fetched
//! over the same connection, so they catch a *corrupted download* and nothing
//! more. There is no signature, so installing an update trusts the project's
//! GitHub release exactly as much as downloading a binary from it by hand
//! would. The dialog says so before the button is pressed, rather than implying
//! a guarantee that does not exist. See `tobii_update::install`.
//!
//! The check runs on a worker thread and the banner stays hidden until it has
//! something to say, so a slow or absent network costs the hub nothing. The
//! download runs on one too: it is the slowest thing this window does.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Label, Orientation};

use tobii_update::install::Action;
use tobii_update::release::{Asset, Blocked, Channel, Check, Release};

/// How the banner reads for a release.
pub fn headline(r: &Release) -> String {
    format!("Version {} is available.", r.version)
}

/// The changelog, or an honest stand-in when a release has none.
pub fn changelog(r: &Release) -> String {
    let notes = r.notes.trim();
    if notes.is_empty() {
        format!(
            "{} was published without release notes.\n\nThe full list of changes is on the \
             releases page.",
            r.version
        )
    } else {
        notes.to_string()
    }
}

/// Progress wording while an install runs.
pub fn installing(step: &str) -> String {
    format!("Updating — {step}…")
}

/// Progress wording while a download runs.
pub fn downloading(step: &str) -> String {
    format!("Downloading — {step}…")
}

/// How the banner reads when a package manager owns this copy.
///
/// It names the manager and the package, because "this cannot be updated from
/// here" on its own reads as a defect in this program rather than the
/// deliberate refusal it is — and because the two names are what the user needs
/// to do it themselves.
pub fn packaged_headline(version: &str, manager: &str, package: &str) -> String {
    format!(
        "Version {version} is available. This copy belongs to {manager} as the package \
         {package}, so it has to be installed by your package manager rather than replaced \
         from here."
    )
}

/// The banner for a copy that was replaced or removed while it ran.
pub fn restart_headline(version: &str) -> String {
    format!(
        "Version {version} is available, but this copy of the program was replaced or \
         removed while it was running, so there is nothing here to update. Quit it and \
         start it again to run whichever version is installed now."
    )
}

/// The banner for a copy only an administrator can replace.
pub fn system_headline(version: &str, dir: &Path) -> String {
    format!(
        "Version {version} is available. This copy is in {}, which only an administrator \
         can change, so it cannot be replaced from here. Download fetches the release and \
         says the one command that installs it for every user.",
        dir.display()
    )
}

/// Where the archive went, and the command that installs it into `dir` for
/// every user. The counterpart of [`saved`] for a copy no package manager owns.
pub fn saved_for_system(dir: &Path, files: &[PathBuf]) -> String {
    let Some(first) = files.first() else {
        // Unreachable: a download that succeeded wrote at least one file.
        return "The download finished, but reported no files.".to_string();
    };
    let folder = first.parent().unwrap_or(Path::new("."));
    let name = first
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let stem = name.trim_end_matches(".tar.gz");
    format!(
        "Saved {name} to {}.\nInstall it for every user with:\n  cd {} && tar -xzf {} && cd {} \
         && sudo ./install.sh --system {}",
        folder.display(),
        sh_quote(&folder.to_string_lossy()),
        sh_quote(&name),
        sh_quote(stem),
        sh_quote(&dir.to_string_lossy()),
    )
}

/// `s` as one shell word: single-quoted, each embedded `'` closed, escaped and
/// reopened. The banner's text is a command meant to be copied out and run, and
/// a folder with a space in it split an unquoted one into two words.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Where a download went, and the one command that installs it.
///
/// Only reached for a copy a package manager owns, which is why every branch
/// hands back a package-manager command rather than telling somebody to unpack
/// something over their packaged install.
///
/// Every path in it, in every branch, goes through [`sh_quote`]: the banner
/// makes this text selectable for exactly one reason, which is to paste it.
pub fn saved(channel: Channel, files: &[PathBuf]) -> String {
    let Some(first) = files.first() else {
        // Unreachable: a download that succeeded wrote at least one file.
        // Still better than a banner that goes blank if it ever happens.
        return "The download finished, but reported no files.".to_string();
    };
    let dir = first
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let dir = sh_quote(&dir.to_string_lossy());
    let path = sh_quote(&first.to_string_lossy());
    let name = |p: &PathBuf| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    };
    let first_name = name(first);
    match channel {
        Channel::Deb => {
            format!("Saved {first_name} to {dir}.\nInstall it with:  sudo apt install {path}")
        }
        Channel::Rpm => format!(
            "Saved {first_name} to {dir}.\nInstall it with:  sudo dnf install {path}\n\
             (on openSUSE:  sudo zypper install {path})"
        ),
        // tobii-linux-bin conflicts with tobii-linux, the package the PKGBUILD
        // builds, so pacman asks before replacing a copy built that way. The
        // question reads like a warning; the answer is given in advance.
        Channel::Pacman => format!(
            "Saved {first_name} to {dir}.\nInstall it with:  sudo pacman -U {path}\n\
             (if pacman offers to remove tobii-linux, the copy built from source, answer y)"
        ),
        // Named individually rather than as "the PKGBUILD": makepkg reads the
        // hook from the working directory by name, so the second file being
        // there is the point.
        Channel::Pkgbuild => format!(
            "Saved {} to {dir}.\nBuild and install it with:  cd {dir} && makepkg -si",
            files.iter().map(name).collect::<Vec<_>>().join(" and ")
        ),
        // The fallback, so it says why it is the fallback. Unpacking this
        // installs into ~/.local/bin, which does not replace the packaged copy
        // — it shadows it, and a user who is not told that will wonder why the
        // version did not change after an upgrade.
        Channel::Archive => format!(
            "Saved {first_name} to {dir}.\nThis release publishes no package for your \
             system, so this is the plain archive: it installs into ~/.local/bin, beside \
             the copy your package manager owns rather than over it.\n  cd {dir} && tar \
             -xzf {} && cd {} && ./install.sh",
            sh_quote(&first_name),
            sh_quote(first_name.trim_end_matches(".tar.gz"))
        ),
    }
}

/// How a failed download reads.
///
/// It says what was left behind, because the answer is "nothing": a partly
/// written `.deb` in a Downloads folder is exactly the thing somebody installs
/// by hand a week later.
pub fn download_failed(why: &str) -> String {
    format!(
        "Download failed: {why}\nNothing from this download was kept. Try again, or \
         download it from the releases page."
    )
}

/// How the banner reads when a newer release cannot be installed from here.
///
/// The two reasons need different words. Telling somebody "no build for your
/// machine" when the build is right there and only the checksums are missing
/// sends them looking for something that exists.
pub fn blocked_headline(version: &str, why: Blocked) -> String {
    match why {
        Blocked::NoBuildForTarget => format!(
            "Version {version} is available, but not as a build for {}.",
            tobii_update::Target::triple()
        ),
        Blocked::NoChecksums => format!(
            "Version {version} is available, but it was published without checksums, \
             so it cannot be installed from here."
        ),
    }
}

/// What the user is agreeing to when they press Update.
///
/// Deliberately not reassuring. The checksum published with a release is
/// fetched from that release, so it proves the download arrived intact and
/// nothing about who wrote it.
pub fn trust_note() -> String {
    "Installing replaces this program's binaries with the ones published in this release. \
     The published checksums are used to confirm the download arrived intact; they are not a \
     signature, so this trusts the project's GitHub releases as much as downloading and \
     running a binary from them by hand would."
        .to_string()
}

/// Build the update banner. It is hidden until a check finds something.
pub fn banner() -> gtk::Box {
    let row = gtk::Box::new(Orientation::Horizontal, 12);
    row.add_css_class("banner");
    row.set_visible(false);

    let text = Label::new(None);
    text.add_css_class("banner-text");
    text.set_hexpand(true);
    text.set_xalign(0.0);
    text.set_wrap(true);
    // A finished download says where the file went and what to type, and both
    // are one long unbreakable "word". A word-wrapping label asks for a natural
    // width that fits the longest of them, and this row is inside a window
    // sized to its content — so without these two the hub would grow to the
    // width of the user's home directory path.
    text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    text.set_max_width_chars(64);
    text.set_valign(Align::Center);

    let notes_btn = crate::widget::button("What's new");
    notes_btn.add_css_class("quiet");
    let update_btn = crate::widget::button("Update");
    update_btn.add_css_class("primary");
    let dismiss = crate::widget::button("Later");
    dismiss.add_css_class("quiet");

    row.append(&text);
    row.append(&notes_btn);
    row.append(&update_btn);
    row.append(&dismiss);

    {
        let row = row.clone();
        dismiss.connect_clicked(move |_| row.set_visible(false));
    }

    // Nothing is asked of the network when the user has said not to.
    if !tobii_config::update_check_enabled() {
        return row;
    }

    // The check, off the UI thread. A hub that stalled on a DNS lookup at
    // startup would be a worse bug than the one this feature fixes.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let found = tobii_update::release::check();
        // Asked here, on this thread, and only when there is something to
        // offer. It runs up to three package managers as subprocesses — see
        // `package_owner`, which bounds each of them — and the answer decides
        // which button the user gets, so it has to be in hand before the banner
        // appears. Nearly every launch finds no update and pays nothing.
        let placed = match &found {
            Ok(Check::Newer(_)) => tobii_update::install::placement(),
            _ => None,
        };
        let _ = tx.send((found, placed));
    });

    let widgets = (row.clone(), text.clone(), notes_btn, update_btn, dismiss);
    glib::timeout_add_local(Duration::from_millis(400), move || {
        let (row, text, notes_btn, update_btn, dismiss) = &widgets;
        let (found, placed) = match rx.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
            // The worker went away without answering: nothing to show, and
            // nothing left to wait for.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return glib::ControlFlow::Break,
            Ok(both) => both,
        };
        match found {
            // Offline, rate-limited, or no curl. Not worth a banner: the user
            // did not ask for this check, so its failure is not their problem.
            Err(_) => {}
            Ok(Check::UpToDate) => {}
            // A newer release with no build for this machine. Saying so beats
            // an Update button that could only ever fail, and beats silence:
            // the release does exist and can be built from source.
            Ok(Check::CannotInstall { version, url, why }) => {
                text.set_text(&blocked_headline(&version.to_string(), why));
                update_btn.set_visible(false);
                // Checked before it is handed to the desktop's URI handler.
                // This string comes out of the same release document as every
                // other URL the crate refuses unless `is_trusted` passes, and
                // `launch_default_for_uri` will hand any scheme to whatever
                // claims it — so an unchecked one is a "click here" button
                // pointing wherever the document says.
                let url = url.clone();
                notes_btn.connect_clicked(move |btn| {
                    if tobii_update::net::is_trusted(&url) {
                        let _ = gtk::gio::AppInfo::launch_default_for_uri(
                            &url,
                            gtk::gio::AppLaunchContext::NONE,
                        );
                    } else {
                        tobii_diagnostics::log::warn(&format!(
                            "refusing to open a release URL that is not on the \
                             allowlist: {url}"
                        ));
                    }
                    let _ = btn;
                });
                crate::widget::set_button_text(notes_btn, "Releases");
                row.set_visible(true);
            }
            Ok(Check::Newer(release)) => {
                let banner = Banner {
                    text: text.clone(),
                    notes: notes_btn.clone(),
                    action: update_btn.clone(),
                    dismiss: dismiss.clone(),
                };
                // Decided in `tobii_update::install::action_for`, where every
                // case is tested. Logged, because "which button did it show,
                // and why" is the first question about any update report, and
                // until 2026-09-11 the log had nothing to answer it with.
                let action = placed
                    .as_ref()
                    .map(tobii_update::install::action_for)
                    .unwrap_or(Action::Update);
                tobii_diagnostics::log::info(&format!(
                    "update {} available; this copy: {placed:?}; offering {action:?}",
                    release.version
                ));
                match action {
                    // Nothing at this copy's path to update: it was replaced or
                    // removed while it ran. Relaunching would hand off to this
                    // same process, so the button quits it.
                    Action::Restart => {
                        text.set_text(&restart_headline(&release.version.to_string()));
                        crate::widget::set_button_text(update_btn, "Quit");
                        wire_notes(&banner, &release);
                        update_btn.connect_clicked(|_| {
                            if let Some(app) = gtk::gio::Application::default() {
                                app.activate_action("quit", None);
                            }
                        });
                    }
                    // The installer would refuse this one, and only after the
                    // user had pressed Update and waited for the download. Ask
                    // for less: fetch the file their package manager can
                    // install, and leave the installing to it.
                    Action::DownloadPackage { manager, package } => {
                        text.set_text(&packaged_headline(
                            &release.version.to_string(),
                            &manager,
                            &package,
                        ));
                        crate::widget::set_button_text(update_btn, "Download");
                        wire_download(&banner, *release, manager, None);
                    }
                    // Nobody owns it, but only an administrator can replace it:
                    // the archive, and the command that installs it for everyone.
                    Action::DownloadForSystem { dir } => {
                        text.set_text(&system_headline(&release.version.to_string(), &dir));
                        crate::widget::set_button_text(update_btn, "Download");
                        wire_download(&banner, *release, String::new(), Some(dir));
                    }
                    Action::Update => {
                        text.set_text(&headline(&release));
                        wire(&banner, *release);
                    }
                }
                row.set_visible(true);
            }
        }
        glib::ControlFlow::Break
    });
    row
}

/// The banner's four widgets, cloned together.
///
/// A click handler owns everything it touches, and past two levels of callback
/// — press, choose a folder, then poll a worker — passing them one by one is
/// four clones per level and a parameter list nothing can read.
#[derive(Clone)]
struct Banner {
    text: Label,
    notes: gtk::Button,
    /// Update, or Download. The click handlers rebuild this from the button
    /// they are handed rather than capturing it, so no closure holds the button
    /// that holds the closure.
    action: gtk::Button,
    dismiss: gtk::Button,
}

/// The "What's new" button, which both banner variants have and which does the
/// same thing in each.
fn wire_notes(b: &Banner, release: &Release) {
    let release = release.clone();
    b.notes.connect_clicked(move |btn| {
        let parent = btn.root().and_downcast::<gtk::Window>();
        changelog_dialog(parent.as_ref(), &release);
    });
}

/// Attach the two actions once a release is actually in hand.
fn wire(b: &Banner, release: Release) {
    wire_notes(b, &release);
    let (text, dismiss, notes_btn) = (b.text.clone(), b.dismiss.clone(), b.notes.clone());
    b.action.connect_clicked(move |btn| {
        btn.set_sensitive(false);
        notes_btn.set_sensitive(false);
        dismiss.set_sensitive(false);
        text.set_text(&installing("starting"));

        // Closing the window mid-install would otherwise end the process
        // between the two renames, leaving a new `tobii` beside an old
        // `tobii-gtk`. The hold is released when the install finishes, either
        // way. (`swap_in` also rolls back, but only for failures it is told
        // about — a process that simply exits tells it nothing.)
        let guard = gtk::gio::Application::default().map(|app| app.hold());

        // The install downloads, verifies and rewrites files. All of that is
        // off the UI thread; the worker reports through a channel and touches
        // no widgets.
        let (tx, rx) = std::sync::mpsc::channel();
        let (ptx, prx) = std::sync::mpsc::channel::<String>();
        let release = release.clone();
        std::thread::spawn(move || {
            let report = |s: &str| {
                let _ = ptx.send(s.to_string());
            };
            let _ = tx.send(tobii_update::install_release(&release, &report));
        });

        let (text, btn, dismiss) = (text.clone(), btn.clone(), dismiss.clone());
        // `hold()` hands back an RAII guard, so releasing it is a drop.
        let guard = std::cell::RefCell::new(guard);
        let release_hold = move || drop(guard.borrow_mut().take());
        glib::timeout_add_local(Duration::from_millis(150), move || {
            while let Ok(step) = prx.try_recv() {
                text.set_text(&installing(&step));
            }
            match rx.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Ok(Ok(done)) => {
                    release_hold();
                    tobii_diagnostics::log::info(&format!("updated to {}", done.version));
                    text.set_text(&format!("Updated to {}. Restart to run it.", done.version));
                    btn.set_visible(false);
                    dismiss.set_sensitive(true);
                    crate::widget::set_button_text(&dismiss, "Close");
                    glib::ControlFlow::Break
                }
                Ok(Err(e)) => {
                    release_hold();
                    tobii_diagnostics::log::warn(&format!("update failed: {e}"));
                    text.set_text(&format!("Update failed: {e}"));
                    btn.set_sensitive(true);
                    dismiss.set_sensitive(true);
                    glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    release_hold();
                    tobii_diagnostics::log::warn("update failed: the updater thread vanished");
                    text.set_text("Update failed: the updater stopped unexpectedly.");
                    btn.set_sensitive(true);
                    dismiss.set_sensitive(true);
                    glib::ControlFlow::Break
                }
            }
        });
    });
}

/// Attach the two actions for a copy a package manager owns.
///
/// Same banner and the same changelog; the second button fetches instead of
/// installing. `manager` is the tool that answered the ownership query — it
/// picks the file, because a `.deb` is no use to somebody running `pacman`.
///
/// `system_dir` is set instead for a copy nobody owns in a directory only an
/// administrator can write. `manager` is then empty, so the file is the plain
/// archive, and the saved message is the `--system` install for that directory.
fn wire_download(b: &Banner, release: Release, manager: String, system_dir: Option<PathBuf>) {
    wire_notes(b, &release);

    let Some(offer) = release.offer_for(&manager, &tobii_update::Target::triple()) else {
        // `Check::Newer` is only reached when the release has an archive for
        // this machine, so there is always at least the fallback to offer. If
        // that ever stops being true, no button beats one that can only fail.
        b.action.set_visible(false);
        return;
    };
    let channel = offer.channel;
    // Owned: the worker thread cannot borrow out of `release`.
    let files: Vec<Asset> = offer.files.into_iter().cloned().collect();

    let (text, notes, dismiss) = (b.text.clone(), b.notes.clone(), b.dismiss.clone());
    b.action.connect_clicked(move |btn| {
        let b = Banner {
            text: text.clone(),
            notes: notes.clone(),
            action: btn.clone(),
            dismiss: dismiss.clone(),
        };
        let dialog = gtk::FileDialog::new();
        dialog.set_title("Where to save it");
        dialog.set_modal(true);
        if let Some(d) = default_download_dir() {
            dialog.set_initial_folder(Some(&gtk::gio::File::for_path(d)));
        }
        let (release, files, system_dir) = (release.clone(), files.clone(), system_dir.clone());
        dialog.select_folder(
            btn.root().and_downcast::<gtk::Window>().as_ref(),
            gtk::gio::Cancellable::NONE,
            move |chosen| match chosen {
                Ok(folder) => match folder.path() {
                    Some(into) => start_download(&b, release, files, channel, into, system_dir),
                    // A location gio can name and the filesystem cannot: a
                    // remote share that is not mounted, or a trash URI.
                    None => b.text.set_text(
                        "Pick a folder on this computer — that one has no path a file can \
                         be written to.",
                    ),
                },
                // They changed their mind. The banner is exactly as it was and
                // the button still works, which is the whole response needed.
                Err(e)
                    if e.matches(gtk::DialogError::Dismissed)
                        || e.matches(gtk::DialogError::Cancelled) => {}
                // No portal, or no file chooser at all. Silence here would look
                // like a dead button, so say where else the file is.
                Err(e) => b.text.set_text(&format!(
                    "The folder chooser could not be opened ({e}). The releases page has \
                     the file — \"What's new\" links to it."
                )),
            },
        );
    });
}

/// Fetch what was chosen into `into`, off the UI thread.
fn start_download(
    b: &Banner,
    release: Release,
    files: Vec<Asset>,
    channel: Channel,
    into: PathBuf,
    system_dir: Option<PathBuf>,
) {
    b.action.set_sensitive(false);
    crate::widget::set_button_text(&b.action, "Downloading…");
    b.notes.set_sensitive(false);
    b.dismiss.set_sensitive(false);
    b.text.set_text(&downloading("starting"));

    // A release archive is tens of megabytes over whatever connection the user
    // has, so this is much the slowest thing the window does; on the UI thread
    // it would be a frozen hub for the duration. The worker touches no widgets
    // and reports through channels, as the install does.
    //
    // No `GApplication` hold, unlike the install: nothing here is half-written
    // into a directory the program runs from, and holding the process open past
    // its last window would leave an invisible program finishing a download the
    // user walked away from.
    let (tx, rx) = std::sync::mpsc::channel();
    let (ptx, prx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let report = |s: &str| {
            let _ = ptx.send(s.to_string());
        };
        let files: Vec<&Asset> = files.iter().collect();
        let _ = tx.send(tobii_update::install::download_release_files(
            &release, &files, &into, &report,
        ));
    });

    let b = b.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || {
        while let Ok(step) = prx.try_recv() {
            b.text.set_text(&downloading(&step));
        }
        match rx.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Ok(Ok(written)) => {
                b.text.set_text(&match &system_dir {
                    Some(dir) => saved_for_system(dir, &written),
                    None => saved(channel, &written),
                });
                // The message is now a command to run: selectable so it can be
                // copied out of the banner instead of retyped from it.
                b.text.set_selectable(true);
                b.action.set_visible(false);
                b.notes.set_sensitive(true);
                b.dismiss.set_sensitive(true);
                crate::widget::set_button_text(&b.dismiss, "Close");
                glib::ControlFlow::Break
            }
            Ok(Err(e)) => {
                tobii_diagnostics::log::warn(&format!("download failed: {e}"));
                b.text.set_text(&download_failed(&e.to_string()));
                ready_again(&b);
                glib::ControlFlow::Break
            }
            // The worker vanished mid-download, so its cleanup did not run:
            // this is the one failure that cannot promise an empty folder.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                tobii_diagnostics::log::warn("download failed: the downloader thread vanished");
                b.text.set_text(
                    "Download failed: the downloader stopped unexpectedly. Check the folder \
                     you chose for a part-written file.",
                );
                ready_again(&b);
                glib::ControlFlow::Break
            }
        }
    });
}

/// Put the banner back the way it was before the click.
fn ready_again(b: &Banner) {
    b.action.set_sensitive(true);
    crate::widget::set_button_text(&b.action, "Download");
    b.notes.set_sensitive(true);
    b.dismiss.set_sensitive(true);
}

/// Where the folder chooser opens.
///
/// The XDG Downloads directory, or the home directory, or wherever the chooser
/// would have opened anyway.
///
/// Each candidate has to exist before it is offered: `XDG_DOWNLOAD_DIR` names a
/// folder the user configured once and may have deleted since, and pointing a
/// file chooser at a directory that is not there is worse than not pointing it
/// anywhere. Neither is created — a program that makes folders in somebody's
/// home directory to open a dialog has overstepped.
fn default_download_dir() -> Option<PathBuf> {
    let usable = |p: PathBuf| p.is_dir().then_some(p);
    glib::user_special_dir(glib::UserDirectory::Downloads)
        .and_then(usable)
        .or_else(|| usable(glib::home_dir()))
}

/// A window showing the release notes.
///
/// Scrollable, unlike the licence dialog: a changelog has no length this program
/// controls, and one long enough to run off the screen is exactly the release
/// worth reading about.
fn changelog_dialog(parent: Option<&gtk::Window>, release: &Release) {
    let heading = Label::new(Some(&format!("What's new in {}", release.version)));
    heading.add_css_class("dialog-heading");
    heading.set_halign(Align::Start);
    heading.set_xalign(0.0);

    let notes = Label::new(Some(&changelog(release)));
    notes.add_css_class("dialog-terms");
    notes.set_wrap(true);
    notes.set_xalign(0.0);
    notes.set_halign(Align::Start);
    notes.set_max_width_chars(64);
    notes.set_selectable(false);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_child(Some(&notes));
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_vexpand(true);
    scroller.set_min_content_height(260);

    let trust = Label::new(Some(&trust_note()));
    trust.add_css_class("dialog-note");
    trust.set_wrap(true);
    trust.set_xalign(0.0);
    trust.set_halign(Align::Start);
    trust.set_max_width_chars(64);

    let link = Label::new(None);
    link.set_markup(&format!(
        "<a href=\"{url}\">{url}</a>",
        url = glib::markup_escape_text(&release.html_url)
    ));
    link.add_css_class("dialog-url");
    link.set_xalign(0.0);
    link.set_halign(Align::Start);
    link.set_wrap(true);
    link.set_wrap_mode(gtk::pango::WrapMode::Char);
    link.set_max_width_chars(64);

    let close = crate::widget::button("Close");
    let buttons = gtk::Box::new(Orientation::Horizontal, 10);
    buttons.set_halign(Align::End);
    buttons.append(&close);

    let content = gtk::Box::new(Orientation::Vertical, 12);
    content.set_margin_top(24);
    content.set_margin_bottom(20);
    content.set_margin_start(26);
    content.set_margin_end(26);
    content.append(&heading);
    content.append(&scroller);
    content.append(&trust);
    content.append(&link);
    content.append(&buttons);

    let win = gtk::Window::builder()
        .title("Release notes")
        .modal(true)
        .default_width(620)
        .default_height(480)
        .child(&content)
        .build();
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }
    let w = win.clone();
    close.connect_clicked(move |_| w.close());
    let keys = gtk::EventControllerKey::new();
    let w = win.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            w.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(keys);
    win.present();
    close.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tobii_update::Version;

    fn release(notes: &str) -> Release {
        Release {
            tag: "v0.3.0".into(),
            version: Version::parse("0.3.0").unwrap(),
            notes: notes.into(),
            assets: vec![],
            html_url: "https://example.invalid/r".into(),
        }
    }

    #[test]
    fn the_headline_names_the_version_on_offer() {
        assert_eq!(headline(&release("")), "Version 0.3.0 is available.");
    }

    /// A release with no notes must not open an empty window — it should say
    /// there are none and point somewhere that has the answer.
    #[test]
    fn a_release_without_notes_says_so_rather_than_showing_nothing() {
        let c = changelog(&release("   \n  "));
        assert!(c.contains("without release notes"), "{c}");
        assert!(c.contains("releases page"), "{c}");
    }

    #[test]
    fn notes_are_shown_as_written_minus_stray_whitespace() {
        let c = changelog(&release("\n## Fixed\n- the thing\n\n"));
        assert_eq!(c, "## Fixed\n- the thing");
    }

    #[test]
    fn progress_reads_as_a_sentence() {
        assert_eq!(installing("verifying"), "Updating — verifying…");
    }

    /// The banner must not offer an Update button that could only fail, and
    /// must still name the version, so the user knows the release exists.
    #[test]
    fn a_release_that_cannot_be_installed_says_which_reason_it_is() {
        let missing = blocked_headline("0.9.0", Blocked::NoBuildForTarget);
        assert!(missing.contains("0.9.0"), "{missing}");
        assert!(
            missing.contains(&tobii_update::Target::triple()),
            "{missing}"
        );

        let sums = blocked_headline("0.9.0", Blocked::NoChecksums);
        assert!(sums.contains("0.9.0"), "{sums}");
        assert!(sums.contains("checksums"), "{sums}");
        // The trap this split exists to avoid: telling somebody there is no
        // build for their machine when the build is right there.
        assert!(
            !sums.contains(&tobii_update::Target::triple()),
            "a missing SHA256SUMS is not a missing build: {sums}"
        );
    }

    #[test]
    fn download_progress_reads_as_a_sentence() {
        assert_eq!(downloading("the checksums"), "Downloading — the checksums…");
    }

    /// The banner has to explain why the button changed, or a packaged copy
    /// looks like a copy this program forgot how to update.
    #[test]
    fn a_packaged_copy_is_told_who_owns_it() {
        let h = packaged_headline("0.3.0", "dpkg", "tobii-linux");
        assert!(h.contains("0.3.0"), "{h}");
        assert!(h.contains("dpkg"), "{h}");
        assert!(h.contains("tobii-linux"), "{h}");
        assert!(h.contains("package manager"), "{h}");
    }

    fn at(dir: &str, names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|n| PathBuf::from(dir).join(n)).collect()
    }

    /// A file in a folder is not an answer on its own: the point of the whole
    /// flow is the command that installs it, and it must be the command for the
    /// format that was actually downloaded.
    #[test]
    fn each_format_is_saved_with_the_command_that_installs_it() {
        let deb = saved(
            Channel::Deb,
            &at(
                "/home/u/Downloads/tobii-linux-0.3.0",
                &["tobii-linux_0.3.0_amd64.deb"],
            ),
        );
        assert!(deb.contains("/home/u/Downloads/tobii-linux-0.3.0"), "{deb}");
        assert!(deb.contains("apt install"), "{deb}");
        assert!(
            !deb.contains("dnf"),
            "a deb is not installed with dnf: {deb}"
        );

        let rpm = saved(
            Channel::Rpm,
            &at(
                "/home/u/Downloads/tobii-linux-0.3.0",
                &["tobii-linux-0.3.0-1.x86_64.rpm"],
            ),
        );
        assert!(rpm.contains("dnf install"), "{rpm}");
        assert!(rpm.contains("zypper"), "openSUSE uses the same rpm: {rpm}");
        assert!(!rpm.contains("apt install"), "{rpm}");

        // Both halves named, because makepkg reads the second one by name from
        // the directory the first one is in.
        let arch = saved(
            Channel::Pkgbuild,
            &at(
                "/home/u/Downloads/tobii-linux-0.3.0",
                &["PKGBUILD", "tobii-linux.install"],
            ),
        );
        assert!(arch.contains("PKGBUILD"), "{arch}");
        assert!(arch.contains("tobii-linux.install"), "{arch}");
        assert!(arch.contains("makepkg -si"), "{arch}");
    }

    /// The fallback is the one that can mislead: unpacking the archive does not
    /// replace a packaged copy, it shadows it, and the banner has to say so.
    #[test]
    fn the_archive_fallback_admits_it_installs_beside_the_packaged_copy() {
        let a = saved(
            Channel::Archive,
            &at(
                "/home/u/Downloads/tobii-linux-0.3.0",
                &["tobii-linux-0.3.0-x86_64-unknown-linux-gnu.tar.gz"],
            ),
        );
        assert!(a.contains("no package for your system"), "{a}");
        assert!(a.contains("~/.local/bin"), "{a}");
        assert!(a.contains("beside"), "{a}");
        // The unpacked directory, not the archive, is what install.sh is in.
        assert!(
            a.contains("cd 'tobii-linux-0.3.0-x86_64-unknown-linux-gnu' && ./install.sh"),
            "{a}"
        );
    }

    /// The prebuilt Arch package is one command, and it says in advance what
    /// to answer when pacman offers to replace a copy built from the PKGBUILD.
    #[test]
    fn the_arch_package_is_saved_with_pacman_u() {
        let p = saved(
            Channel::Pacman,
            &at(
                "/home/u/Downloads/tobii-linux-0.3.0",
                &["tobii-linux-bin-0.3.0-1-x86_64.pkg.tar.zst"],
            ),
        );
        assert!(
            p.contains(
                "sudo pacman -U \
                 '/home/u/Downloads/tobii-linux-0.3.0/tobii-linux-bin-0.3.0-1-x86_64.pkg.tar.zst'"
            ),
            "{p}"
        );
        assert!(p.contains("remove tobii-linux"), "{p}");
        assert!(p.contains("answer y"), "{p}");
        assert!(!p.contains("makepkg"), "there is nothing to build: {p}");
    }

    /// A folder with a space or an apostrophe in it is still one argument, in
    /// every branch: the whole point of the text is to be pasted.
    #[test]
    fn every_printed_path_is_quoted_for_the_shell() {
        let dir = "/home/u/My Downloads/Bob's/tobii-linux-0.3.0";
        let q = |s: &str| sh_quote(s);
        let cases: [(Channel, &[&str], String); 5] = [
            (
                Channel::Deb,
                &["tobii-linux_0.3.0_amd64.deb"],
                format!(
                    "sudo apt install {}",
                    q(&format!("{dir}/tobii-linux_0.3.0_amd64.deb"))
                ),
            ),
            (
                Channel::Rpm,
                &["tobii-linux-0.3.0-1.x86_64.rpm"],
                format!(
                    "sudo zypper install {}",
                    q(&format!("{dir}/tobii-linux-0.3.0-1.x86_64.rpm"))
                ),
            ),
            (
                Channel::Pacman,
                &["tobii-linux-bin-0.3.0-1-x86_64.pkg.tar.zst"],
                format!(
                    "sudo pacman -U {}",
                    q(&format!("{dir}/tobii-linux-bin-0.3.0-1-x86_64.pkg.tar.zst"))
                ),
            ),
            (
                Channel::Pkgbuild,
                &["PKGBUILD", "tobii-linux.install"],
                format!("cd {} && makepkg -si", q(dir)),
            ),
            (
                Channel::Archive,
                &["tobii-linux-0.3.0-x86_64-unknown-linux-gnu.tar.gz"],
                format!(
                    "cd {} && tar -xzf 'tobii-linux-0.3.0-x86_64-unknown-linux-gnu.tar.gz' \
                     && cd 'tobii-linux-0.3.0-x86_64-unknown-linux-gnu' && ./install.sh",
                    q(dir)
                ),
            ),
        ];
        for (channel, names, command) in cases {
            let s = saved(channel, &at(dir, names));
            assert!(s.contains(&command), "{channel:?} lacks {command:?}: {s}");
            assert!(s.contains(&format!("to {}.", q(dir))), "{channel:?}: {s}");
            // Nowhere bare: an unquoted copy after a space is a broken command.
            assert!(!s.contains(&format!(" {dir}")), "{channel:?}: {s}");
            assert!(s.contains(r"Bob'\''s"), "the apostrophe survives: {s}");
        }
    }

    /// The quoting is right only if a real shell reads it back as the string
    /// that went in — including the characters a shell would otherwise act on.
    #[test]
    fn sh_quote_round_trips_through_a_real_shell() {
        for s in [
            "plain",
            "My Downloads",
            "Bob's",
            "''",
            "$HOME `id` $(id) \"x\" \\ ; & | * ~",
            "new\nline",
            "",
        ] {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("printf %s {}", sh_quote(s)))
                .output()
                .expect("sh runs");
            assert!(out.status.success(), "{s:?}: {out:?}");
            assert_eq!(String::from_utf8_lossy(&out.stdout), s, "{s:?}");
        }
    }

    /// Reporting a failed download without saying the file is gone invites
    /// somebody to go and install whatever is sitting in that folder.
    #[test]
    fn a_failed_download_says_nothing_was_kept() {
        let f = download_failed("x.deb does not match its published checksum");
        assert!(f.contains("does not match"), "{f}");
        assert!(f.contains("Nothing from this download was kept"), "{f}");
    }

    /// The wording shown before Update is pressed has to be accurate: the
    /// checksum is not a signature, and claiming otherwise is the one thing
    /// this dialog must not do.
    #[test]
    fn the_trust_note_does_not_claim_a_guarantee_it_cannot_give() {
        let t = trust_note();
        assert!(t.contains("not a signature"), "{t}");
        assert!(t.contains("arrived intact"), "{t}");
        for overclaim in ["verified", "safe", "secure", "trusted source"] {
            assert!(!t.contains(overclaim), "{overclaim:?} overstates it: {t}");
        }
    }
}

/// The texts the banner shows for the two cases `action_for` added.
#[cfg(test)]
mod banner_text_tests {
    use super::*;

    #[test]
    fn a_copy_replaced_while_running_is_told_to_quit_and_start_again() {
        let t = restart_headline("0.3.1");
        assert!(t.contains("0.3.1") && t.contains("Quit"), "{t}");
    }

    #[test]
    fn a_system_copy_gets_the_system_install_command_quoted() {
        let t = saved_for_system(
            Path::new("/usr/local/bin"),
            &[PathBuf::from(
                "/home/u/My Downloads/tobii-linux-0.3.1/tobii-linux-0.3.1-x86_64-unknown-linux-gnu.tar.gz",
            )],
        );
        assert!(
            t.contains("cd '/home/u/My Downloads/tobii-linux-0.3.1'"),
            "{t}"
        );
        assert!(
            t.contains("tar -xzf 'tobii-linux-0.3.1-x86_64-unknown-linux-gnu.tar.gz'"),
            "{t}"
        );
        assert!(
            t.contains("cd 'tobii-linux-0.3.1-x86_64-unknown-linux-gnu'"),
            "{t}"
        );
        assert!(
            t.contains("sudo ./install.sh --system '/usr/local/bin'"),
            "{t}"
        );
        assert!(
            !t.contains("package manager") && !t.contains(".local/bin"),
            "{t}"
        );
        let h = system_headline("0.3.1", Path::new("/usr/local/bin"));
        assert!(h.contains("/usr/local/bin") && h.contains("0.3.1"), "{h}");
    }

    #[test]
    fn a_quoted_word_survives_a_single_quote_inside_it() {
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote("/plain/path"), "'/plain/path'");
    }
}
