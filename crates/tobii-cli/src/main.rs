//! `tobii` CLI. Subcommands: `stream`, `headpose`, `setup`, `display get|set`,
//! `calibrate`, `uninstall`.

use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use tobii_config::DisplaySetup;
use tobii_headpose::pose_from_sample;
use tobii_protocol::frame::{OP_GAZE_NOTIFY, OP_GET_DISPLAY_AREA};
use tobii_protocol::gaze::present;
use tobii_protocol::{DisplayCorners, EnabledEye};
use tobii_usb::{Connection, UsbTransport};

type CmdResult = Result<(), Box<dyn std::error::Error>>;

mod bridge;
mod proton;
mod uninstall;
mod userreg;
mod wineserver;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str);
    let arg2 = args.get(2).map(String::as_str);
    let result = match (sub, arg2) {
        // Answered before anything else is touched: no device, no config, no
        // network. The updater runs this on a freshly downloaded binary to
        // check it can actually execute here before it replaces the installed
        // one — a build made against a newer glibc dies at the dynamic linker,
        // and that has to be found while the old binary is still in place.
        (Some("--version" | "-V" | "version"), _) => {
            println!("tobii {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        (Some("stream"), _) => stream(
            args.iter().any(|a| a == "--json"),
            args.iter().any(|a| a == "--eyes"),
        ),
        (Some("update"), _) => update(&args),
        (Some("uninstall"), _) => uninstall::run(&args),
        // Returns its own exit code rather than a CmdResult: the whole point is
        // to be transparent to whatever launched it, and a launcher reads the
        // status of the thing it launched.
        (Some("game"), _) => return game(&args),
        (Some("bridge"), _) => bridge::bridge(&args),
        (Some("headpose"), Some("--model-status")) => model_status(),
        (Some("headpose"), Some("--fetch-model")) => fetch_model(&args),
        (Some("headpose"), Some("--check-update")) => check_model_update(),
        (Some("headpose"), Some("--remove-model")) => remove_model(),
        (Some("headpose"), Some("--install-model")) => install_model(&args),
        (Some("headpose"), _) => headpose(&args),
        (Some("columns"), _) => columns(),
        (Some("probe-streams"), _) => probe_streams(&args),
        (Some("streams"), _) => stream_catalog(),
        (Some("log"), _) => device_log(&args),
        (Some("probe-stream"), _) => probe_stream(&args),
        (Some("dump-stream"), _) => dump_stream(&args),
        (Some("debug"), _) => debug_report(&args),
        (Some("record"), _) => record_session(&args),
        (Some("camera"), Some("both")) => camera_both(&args),
        (Some("camera"), _) => camera(&args),
        (Some("setup"), _) => setup(),
        (Some("display"), Some("get")) => display_get(),
        (Some("display"), Some("set")) => display_set(),
        (Some("calibrate"), _) => calibrate(args.iter().any(|a| a == "--apply")),
        (Some("cal-probe"), _) => cal_probe(),
        (Some("cal-blob"), _) => cal_blob(),
        (Some("cal-points"), _) => cal_points(),
        (Some("enabled-eye"), arg) => enabled_eye_cmd(arg),
        (Some("games"), sub) => games_cmd(sub, &args),
        _ => {
            eprintln!(
                "usage:\n  \
                 tobii update [--install]\n  \
                 tobii uninstall [--dry-run] [--yes] [--purge] [--udev] [--system] [--bindir DIR]\n  \
                 tobii stream [--json] [--eyes]\n  \
                 tobii game -- <command> [args...]\n  \
                 tobii games [set KEY VALUE]\n  \
                 tobii games profile show|save|apply|forget [<app id or name>]\n  \
                 tobii games profile check where|add|remove <app id or name>\n  \
                 tobii bridge install --prefix PATH\n  \
                 tobii bridge status --prefix PATH\n  \
                 tobii bridge run --prefix PATH\n  \
                 tobii headpose [--udp ADDR] [--rate HZ] [--model auto|off|FILE] [--recenter]\n  \
                 tobii headpose --check [--calibrate-pitch [SECS]]\n  \
                 tobii headpose --model-status\n  \
                 tobii headpose --fetch-model [--agree]\n  \
                 tobii headpose --check-update\n  \
                 tobii record [--calibration] [FILE]\n  \
                 tobii debug [--file PATH]\n  \
                 tobii headpose --remove-model\n  \
                 tobii headpose --install-model <FILE>\n  \
                 tobii columns\n  \
                 tobii probe-streams [START] [END]\n  \
                 tobii streams\n  \
                 tobii log [SECS]\n  \
                 tobii probe-stream <ID> [SECS]\n  \
                 tobii dump-stream <ID> [COUNT]\n  \
                 tobii camera [ID] [COUNT]\n  \
                 tobii camera both [SECS]\n  \
                 tobii setup\n  \
                 tobii display get\n  \
                 tobii display set\n  \
                 tobii calibrate [--apply]\n  \
                 tobii cal-probe\n  \
                 tobii cal-blob\n  \
                 tobii cal-points\n  \
                 tobii enabled-eye [both|left|right]"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

/// Run a command with the tracker held on for as long as it lives.
///
/// ```text
/// tobii game -- %command%          # in Steam's launch options
/// tobii game -- ./MyGame.x86_64
/// ```
///
/// # Why this exists rather than a switch in the GUI
///
/// The tracker is only on while something asks for it, and a game cannot ask:
/// it speaks opentrack or TrackIR, not this program's socket. Something has to
/// hold the claim for the game's lifetime and let go afterwards, and the thing
/// that knows exactly how long a game runs is the process that started it.
///
/// Wrapping is also the one integration point every launcher already has.
/// Steam substitutes `%command%`, Lutris and Heroic have a wrapper field, and a
/// shell script needs no support at all — so this works without asking any of
/// them to know what a Tobii is.
///
/// # It never stops the game from starting
///
/// If the hub is not running there is no socket to connect to, and that is a
/// warning, not a failure. A user whose game refuses to launch because an eye
/// tracker daemon is down would rightly remove the wrapper and never put it
/// back; head tracking is worth less than the game starting.
///
/// # And, when it is a Proton launch, where the provider goes
///
/// A Proton title's head-tracking provider has to be inside the game's own
/// wineserver session, and cannot be started beside it — see [`crate::proton`]
/// for why, and for the rewrite that puts it there instead. That is strictly
/// the second job: every decision it makes can come back "no", and every "no"
/// runs the command exactly as it arrived.
fn game(args: &[String]) -> ExitCode {
    let Some(cmd) = command_after_separator(args) else {
        eprintln!(
            "usage: tobii game -- <command> [args...]\n\n\
             Runs the command with the eye tracker held on, and releases it when\n\
             the command exits. In Steam, set the launch options to:\n\n    \
             tobii game -- %command%"
        );
        return ExitCode::from(2);
    };

    // The name is what the hub shows when asked why the tracker is on, so it is
    // the program being run rather than "tobii game".
    let name = std::path::Path::new(&cmd[0])
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| cmd[0].clone());

    // Reaching the hub is not the same as the hub sending anything. With game
    // output off, this wrapper works perfectly — the tracker comes on, the hub
    // gets frames — and the game receives nothing, because nothing is routed
    // anywhere. That presents as "head tracking does not work" with a lit
    // tracker as evidence that it should, which is the worst combination to
    // debug. The hub's games row names this state; the wrapper has to as well.
    let games = tobii_output::games::load_output_config();
    if !games.enabled {
        eprintln!(
            "note: game output is off, so {name} will receive nothing even though \
             the tracker comes on. Turn it on in the hub, or: tobii games set enabled true"
        );
    }

    // Held for exactly as long as the child lives. Dropping it is what releases
    // the tracker, so it is deliberately still in scope below the wait.
    let client = match tobii_ipc::Client::connect(tobii_ipc::subs::POSE, &name) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!(
                "note: could not reach the Tobii hub ({e}); starting {name} anyway, \
                 without head tracking. Open the hub and relaunch to get it."
            );
            None
        }
    };

    // The provider belongs inside the game's own wineserver session, and
    // [`proton`] builds the batch that would put it there — but nothing calls
    // it, because pointing Proton at a batch file does not work and the way it
    // fails is the worst available.
    //
    // Measured on four Proton builds on this machine: Proton's `steam.exe`
    // helper runs an `.exe` target through `CreateProcessW` and waits, and a
    // `.bat` target through `ShellExecuteW`, which does not. So Proton returns
    // 0 about a second in, while `cmd.exe`, the provider and the game are all
    // still starting. Steam is told the game exited successfully; this wrapper
    // returns and drops the tracker out from under a game that is still
    // running; the batch file is deleted while `cmd.exe` is reading it, so the
    // game often never starts at all; and the next launch's `wineserver -w`
    // blocks on what is still alive — the freeze v0.5.0 removed, put back by
    // the thing meant to remove the need for it.
    //
    // The module stays. Its plan, its quoting and its batch are measured and
    // right, and the shape that can use them is an `.exe` of ours as the
    // target rather than a `.bat` — which is a Windows program to build, not a
    // line to change here. See `docs/wiki/Quality-and-Risks.md` §11.3l.

    let mut child = std::process::Command::new(&cmd[0]);
    child.args(&cmd[1..]);
    // Put into the game's environment, because the Wine-side DLL reads its port
    // from there and nothing else can tell it. The hub sends on `bridge_port`;
    // the DLL defaults to 4243. Change that setting and, without this, the two
    // sit on different ports with nothing to say so — the game just gets no
    // tracking. Spelled literally rather than imported: the constant lives in
    // `bridge/core/src/feeder.rs`, a separate workspace this crate does not
    // depend on.
    if let Some(port) = games.bridge_port {
        child.env("TOBII_BRIDGE_PORT", port.to_string());
    }
    let status = child.status();
    // Explicit, and after the wait: this is the line that puts the tracker out.
    drop(client);

    match status {
        Ok(s) => ExitCode::from(exit_code_of(s)),
        Err(e) => {
            eprintln!("error: could not run {}: {e}", cmd[0]);
            ExitCode::from(127)
        }
    }
}

/// The command after `--`, or `None` if there is not one.
///
/// A separator is required rather than taking the rest of the line, because
/// `tobii game --rate 60 thing` should be an error rather than an attempt to
/// execute `--rate`. Everything after the FIRST `--` is the command, including
/// any further `--`, which belong to the game.
fn command_after_separator(args: &[String]) -> Option<Vec<String>> {
    let at = args.iter().position(|a| a == "--")?;
    let rest = &args[at + 1..];
    (!rest.is_empty()).then(|| rest.to_vec())
}

/// A child's exit status as a process exit code.
///
/// A killed child has no exit code, and reporting 0 for one would tell a
/// launcher the game finished cleanly when it crashed. The shell's convention —
/// 128 plus the signal — is what every wrapper around it already produces.
fn exit_code_of(status: std::process::ExitStatus) -> u8 {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        return code as u8;
    }
    match status.signal() {
        Some(sig) => 128u8.saturating_add(sig as u8),
        None => 1,
    }
}

/// Check for a newer release, and install it when asked.
///
/// The check is the default and the install is opt-in, because replacing the
/// binaries a user is running is not something to do because they typed a bare
/// verb.
fn update(args: &[String]) -> CmdResult {
    use tobii_update::release::Check;
    let current = tobii_update::Version::current();
    println!("running {current}");
    match tobii_update::release::check()? {
        Check::UpToDate => {
            println!("up to date — nothing newer has been released.");
            Ok(())
        }
        Check::CannotInstall { version, url, why } => {
            match why {
                tobii_update::release::Blocked::NoBuildForTarget => println!(
                    "\n{version} is available, but it publishes no build for {}.",
                    tobii_update::Target::triple()
                ),
                tobii_update::release::Blocked::NoChecksums => println!(
                    "\n{version} is available and has a build for this machine, but it \
                     publishes no SHA256SUMS — so a truncated download could not be told \
                     from a complete one."
                ),
            }
            println!("Build it from source, or see {}", sanitize_notes(&url));
            Ok(())
        }
        Check::Newer(r) => {
            println!("\n{} is available.\n", r.version);
            if r.notes.trim().is_empty() {
                println!("(this release has no notes)");
            } else {
                println!("{}", sanitize_notes(&r.notes));
            }
            // Sanitized like the notes: it comes out of the same JSON document.
            println!("\n{}", sanitize_notes(&r.html_url));
            if !args.iter().any(|a| a == "--install") {
                println!("\nRun `tobii update --install` to download and install it.");
                return Ok(());
            }
            let dir = tobii_update::install::install_dir()?;
            if tobii_update::install::is_build_tree(&dir) {
                // Not a refusal: somebody may well want to drop a release build
                // into their checkout. But the next `cargo build` silently
                // reverts it, and finding that out later is worse than being
                // told now.
                println!(
                    "\nnote: {} is a Cargo build directory — the next `cargo build` will \
                     overwrite what is installed here.",
                    dir.display()
                );
            }
            // Said plainly before anything is downloaded. The checksum published
            // with a release is fetched from that same release, so it catches a
            // corrupted download and not a hostile one; installing an update
            // trusts the GitHub release as much as running a binary downloaded
            // by hand from it would.
            println!(
                "\nThis downloads and runs binaries published at {}.",
                tobii_update::releases_url()
            );
            println!();
            let done = tobii_update::install_release(&r, &|step| println!("  {step}"))?;
            // `done.version` is what the DOWNLOADED binary printed for
            // `--version`, so it is release-controlled like the notes and the
            // URL above it and goes through the same filter. The directory is
            // local and the replaced names are compile-time constants.
            println!(
                "\ninstalled {} into {} ({})",
                sanitize_notes(&done.version),
                done.dir.display(),
                done.replaced.join(", ")
            );
            println!("Restart anything that is still running the old build.");
            Ok(())
        }
    }
}

/// Release notes, made safe to print to a terminal.
///
/// The body is written by whoever published the release and was printed
/// verbatim. A terminal reads control characters in it as commands, so a
/// changelog could move the cursor, recolour the rest of the session, or clear
/// the screen — and `\x1b]` can set the window title. Tabs and newlines are the
/// only control characters a changelog needs.
fn sanitize_notes(notes: &str) -> String {
    notes
        .trim_end()
        .chars()
        // A carriage return is dropped rather than replaced: GitHub stores
        // release bodies with CRLF line endings, so replacing it put a U+FFFD
        // at the end of every single line of a real changelog.
        .filter(|c| *c != '\r')
        .map(|c| match c {
            '\n' | '\t' => c,
            // `is_control` covers C0 and DEL but NOT the C1 block, which some
            // terminals still act on, nor the bidi overrides that can reorder
            // text into something that reads as a different sentence.
            c if c.is_control()
                || ('\u{80}'..='\u{9f}').contains(&c)
                || ('\u{202a}'..='\u{202e}').contains(&c)
                || ('\u{2066}'..='\u{2069}').contains(&c) =>
            {
                '\u{fffd}'
            }
            c => c,
        })
        .collect()
}

/// Delete the installed model. Head tracking keeps working without it.
fn remove_model() -> CmdResult {
    use tobii_headpose::model_store;
    let path = model_store::path_of(&model_store::HEAD_POSE);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            println!("removed {}", path.display());
            println!("head tracking still works — without the up-and-down angle.");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("nothing to remove: no model at {}", path.display());
            Ok(())
        }
        Err(e) => Err(format!("could not remove {}: {e}", path.display()).into()),
    }
}

/// Ask whether opentrack has published a newer model than the one pinned here.
fn check_model_update() -> CmdResult {
    use tobii_headpose::model_store::{self, Update};
    let src = &model_store::HEAD_POSE;
    println!("pinned to opentrack commit {}", src.commit);
    match model_store::check_update(src) {
        Update::UpToDate => println!("up to date — nothing newer has touched {}", src.file),
        Update::Newer { sha, date } => {
            println!("upstream has a NEWER {} (commit {sha}, {date})", src.file);
            println!();
            println!("This is deliberately not downloadable from here. A different model is a");
            println!("different model: its rotation conventions, its pitch zero and the scale of");
            println!("its confidence output are all measured against the pinned one, and this");
            println!("driver's constants come from those measurements. Adopting a new model means");
            println!("re-measuring and shipping a new pin, not re-running the download.");
        }
        Update::Unknown(why) => println!("could not check: {why}"),
    }
    Ok(())
}

/// Report which head-pose models are installed and whether they verify.
fn model_status() -> CmdResult {
    use tobii_headpose::model_store::{self, Status};
    println!("model directory: {}", model_store::model_dir().display());
    for src in model_store::SOURCES {
        let state = match model_store::status(src) {
            Status::Ready => "installed and verified".to_string(),
            Status::Missing => "not installed".to_string(),
            Status::Corrupt { found } => format!("PRESENT BUT WRONG (sha256 {found})"),
        };
        println!("  {:32} {state}", src.name);
    }
    match model_store::pitch_offset() {
        Some(d) => println!("\npitch zero: {d:+.1}°"),
        None => println!("\npitch zero: not measured — run `tobii headpose --calibrate-pitch`"),
    }
    println!("Head tracking works without a model — just without pitch.");
    Ok(())
}

/// Download a model after showing its terms and taking an explicit decision.
///
/// The consent prompt is not a formality: these weights are non-commercial-use
/// only and this program is GPL-3.0-only, so fetching them can only ever be the
/// user's choice, never a default or a background step. `--agree` exists for
/// people scripting their own setup, and still prints the terms.
fn fetch_model(args: &[String]) -> CmdResult {
    use tobii_headpose::model_store;
    println!("{}\n", model_store::TERMS);
    let src = &model_store::HEAD_POSE;
    println!(
        "About to download {} ({:.1} MB)\n  from {}\n",
        src.file,
        src.bytes as f64 / 1e6,
        src.url
    );
    if !args.iter().any(|a| a == "--agree") {
        print!("Type 'agree' to accept those terms and download: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if line.trim() != "agree" {
            println!("Not downloaded. Head tracking still works without it, without pitch.");
            return Ok(());
        }
    }
    println!("downloading...");
    let path = model_store::fetch(src)?;
    println!("installed and verified: {}", path.display());
    Ok(())
}

/// Install a model the user downloaded themselves. Verified the same way.
fn install_model(args: &[String]) -> CmdResult {
    use tobii_headpose::model_store;
    let path = args
        .get(3)
        .ok_or("usage: tobii headpose --install-model <FILE>")?;
    let dest = model_store::install_from_file(&model_store::HEAD_POSE, std::path::Path::new(path))?;
    println!("installed and verified: {}", dest.display());
    Ok(())
}

/// Get (and optionally set) which eye(s) the tracker detects (Spike S4).
/// `which` = both|left|right sets it first; then reads it back.
fn enabled_eye_cmd(which: Option<&str>) -> CmdResult {
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    if let Some(w) = which {
        let eye = match w {
            "both" => EnabledEye::Both,
            "left" => EnabledEye::Left,
            "right" => EnabledEye::Right,
            _ => return Err("usage: tobii enabled-eye [both|left|right]".into()),
        };
        let acked = conn.set_enabled_eye(eye)?;
        println!("set enabled_eye = {w} (acknowledged: {acked})");
    }
    match conn.get_enabled_eye()? {
        Some(e) => println!("enabled_eye is now: {e:?}"),
        None => println!("no enabled_eye response (unsupported firmware?)"),
    }
    Ok(())
}

/// Diagnostic: probe the calibration session ops. Non-destructive — only
/// `start` then `stop` (NOT `clear`, which would wipe the calibration, and no
/// compute, so nothing is written). Useful for checking that a device still
/// accepts these ops standalone, independently of the GUI's calibration flow.
///
/// Also samples live gaze for a few seconds WHILE calibration mode is active,
/// to check whether `gaze_point_2d` (the device's own point-of-regard
/// estimate) stays valid and tracks where you're looking during a session.
/// This is the exact signal the real Windows software uses (decompiled from
/// `Tobii.Configuration.Common.dll`'s `CalibrationProcessViewModel.OnGazeData`)
/// to detect when the user's gaze has actually landed on a stimulus point,
/// instead of the fixed dwell timer this driver currently uses. If it comes
/// back valid and plausible here, gaze-verified point capture is feasible.
fn cal_probe() -> CmdResult {
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    // The device wipes its display area on reboot; re-apply so it is in a
    // normal working state before we exercise calibration.
    if let Ok(Some(setup)) = tobii_config::load() {
        let _ = conn.set_display_area(&setup.to_corners());
    }
    eprintln!("probing calibration session ops (start -> stop; non-destructive)...");
    match conn.start_calibration() {
        Ok(()) => println!("  calibration_start (0x3f2): ACK"),
        Err(e) => {
            println!("  calibration_start (0x3f2): FAILED ({e})");
            return Ok(());
        }
    }

    eprintln!(
        "sampling gaze_point_2d for 8s WHILE calibration mode is active — look around \
         the screen (corners, center) and watch whether the values track. Ctrl-C stops \
         early (calibration_stop below then will not run)."
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    let (mut total_frames, mut valid_frames) = (0u32, 0u32);
    while Instant::now() < deadline {
        let Some(s) = conn.next_gaze() else {
            continue;
        };
        total_frames += 1;
        let valid = s.has(present::GAZE_2D) && s.validity_l == 0 && s.validity_r == 0;
        valid_frames += valid as u32;
        println!(
            "  t={:>12}  gaze=({:.4}, {:.4})  valL={} valR={}{}",
            s.timestamp_us,
            s.gaze_point_2d[0],
            s.gaze_point_2d[1],
            s.validity_l,
            s.validity_r,
            if valid { "" } else { "  (invalid)" }
        );
    }
    println!(
        "  summary: {valid_frames}/{total_frames} frames had a valid gaze_point_2d \
         during calibration mode"
    );

    match conn.stop_calibration() {
        Ok(()) => println!("  calibration_stop  (0x3fc): ACK"),
        Err(e) => println!("  calibration_stop  (0x3fc): FAILED ({e})"),
    }
    Ok(())
}

/// Ask the device for the calibration stimulus points (op `0x460`).
///
/// If it answers, the point set is the device's own and settles whether our
/// hardcoded layout matches the original — no decompiling required. Tries both
/// outside and inside a calibration session, since a query like this may only be
/// meaningful once one is open.
fn cal_points() -> CmdResult {
    use tobii_protocol::frame::OP_CAL_STIMULUS_POINTS;
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    reapply_display_area(&mut conn);
    conn.set_request_timeout(Duration::from_secs(3));

    let ask = |label: &str, conn: &mut Connection<UsbTransport>| match conn
        .request(OP_CAL_STIMULUS_POINTS, &[0x00, 0x00])
    {
        Ok(Some(p)) => {
            println!("  {label:16} ANSWERED, {} bytes", p.len());
            println!("    hex: {}", hex(&p));
            decode_points(&p);
            true
        }
        Ok(None) => {
            println!("  {label:16} no response");
            false
        }
        Err(e) => {
            println!("  {label:16} error: {e}");
            false
        }
    };

    println!("asking the device for its calibration points (op 0x460):");
    let outside = ask("outside session", &mut conn);
    // A calibration session is not destructive by itself: start then stop, with
    // no clear and no compute, leaves the existing calibration alone.
    let inside = match conn.start_calibration() {
        Ok(()) => {
            let got = ask("inside session", &mut conn);
            if let Err(e) = conn.stop_calibration() {
                eprintln!("warning: failed to leave the calibration session ({e})");
            }
            got
        }
        Err(e) => {
            println!("  (could not open a session to ask inside it: {e})");
            false
        }
    };
    if !outside && !inside {
        println!("\nNo answer either way. 0x460 is likely not this op, or not a query.");
    }
    Ok(())
}

/// Try to read a stimulus-point list out of a reply: pairs of Q42 values in
/// `[0,1]` are what a normalized point set looks like on this wire.
fn decode_points(payload: &[u8]) {
    let mut r = tobii_protocol::tlv::Reader::new(payload);
    r.skip(2);
    let mut pts = Vec::new();
    while let Ok(p) = r.read_point2d() {
        pts.push(p);
    }
    if pts.is_empty() {
        println!("    (no point2d values decoded — the layout is something else)");
        return;
    }
    println!("    {} point(s):", pts.len());
    for (i, p) in pts.iter().enumerate() {
        println!("      {i}: ({:.4}, {:.4})", p[0], p[1]);
    }
}

/// Diagnose the calibration-blob round trip: retrieve it, then apply it both
/// with and without the response's 2-byte status prefix, printing what the
/// device answers each time.
///
/// Every TTP response payload starts with a 2-byte prefix that every other
/// decoder here skips. `retrieve_calibration` does not, and `apply_calibration`
/// prepends its own — so a re-applied blob goes out as `[00 00][00 00][data]`,
/// shifted by two bytes. Nothing checks the reply, so a rejection would look
/// exactly like success. This prints the reply so it cannot.
///
/// **Not** non-destructive: an earlier version of this comment claimed the two
/// forms are applied in an order chosen so the session ends in the better
/// state. They are applied in a fixed order, and nothing here reads the replies
/// to decide which was preferred — the point is to PRINT them so a human can
/// see which one the device accepted. Run it on a tracker you are willing to
/// recalibrate.
fn cal_blob() -> CmdResult {
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    reapply_display_area(&mut conn);

    let blob = conn.retrieve_calibration()?;
    let raw = &blob.0;
    println!("retrieved {} bytes", raw.len());
    println!("  first 16: {}", hex(&raw[..raw.len().min(16)]));
    if raw.len() >= 2 && raw[0] == 0 && raw[1] == 0 {
        println!("  starts with the 2-byte status prefix every other decoder skips");
    }
    let stripped: Vec<u8> = if raw.len() > 2 {
        raw[2..].to_vec()
    } else {
        raw.clone()
    };

    let try_apply = |label: &str, b: &[u8], conn: &mut Connection<UsbTransport>| match conn.request(
        tobii_protocol::frame::OP_CAL_APPLY,
        &tobii_protocol::calibration::cal_apply_payload(b),
    ) {
        Ok(Some(reply)) => println!(
            "  {label:9} ({:5} bytes): reply {} bytes, status {}",
            b.len(),
            reply.len(),
            if reply.len() >= 2 {
                hex(&reply[..2])
            } else {
                "(none)".into()
            }
        ),
        Ok(None) => println!("  {label:9} ({:5} bytes): NO REPLY", b.len()),
        Err(e) => println!("  {label:9} ({:5} bytes): error {e}", b.len()),
    };
    println!("\napplying both forms (the stripped one last, so it wins):");
    try_apply("as-is", raw, &mut conn);
    try_apply("stripped", &stripped, &mut conn);
    println!(
        "\nA differing status is the answer. Identical statuses mean the device\n\
         tolerates both, and the prefix is cosmetic rather than corrupting."
    );
    Ok(())
}

/// Collect notification ops seen over `dur`: op -> (count, last payload len).
/// Uses `read_notifications` so co-occurring streams are not undercounted.
fn collect_notif_ops(
    conn: &mut Connection<UsbTransport>,
    dur: Duration,
) -> std::collections::BTreeMap<u32, (u32, usize)> {
    use std::collections::BTreeMap;
    let mut seen: BTreeMap<u32, (u32, usize)> = BTreeMap::new();
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        for (op, payload) in conn.read_notifications() {
            let e = seen.entry(op).or_insert((0, 0));
            e.0 += 1;
            e.1 = payload.len();
        }
    }
    seen
}

/// Diagnostic deep-dive on ONE stream: subscribe to it on a fresh connection,
/// read for `secs`, and report its rate, payload size range, whether the payload
/// CHANGES frame-to-frame (live data vs static config), and a hex preview. Move
/// your head while this runs to see whether a small live stream is head pose.
/// `tobii probe-stream <id-hex> [secs]`.
fn probe_stream(args: &[String]) -> CmdResult {
    let id = args
        .get(2)
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or("usage: tobii probe-stream <stream-id-hex> [secs]")?;
    let secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(5);

    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    reapply_display_area(&mut conn);
    conn.set_request_timeout(Duration::from_millis(300));
    let acked = conn.subscribe_stream(id)?;
    eprintln!(
        "stream 0x{id:03x}: subscribe {}",
        if acked { "ACK" } else { "no ack" }
    );
    eprintln!("reading {secs}s — MOVE YOUR HEAD if hunting head pose (Ctrl-C to stop)...");

    let mut count = 0u32;
    let (mut min_sz, mut max_sz) = (usize::MAX, 0usize);
    let mut first: Option<Vec<u8>> = None;
    let mut changed = false;
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        for (op, payload) in conn.read_notifications() {
            if op != id as u32 {
                continue; // ignore the always-on gaze stream (0x500)
            }
            count += 1;
            min_sz = min_sz.min(payload.len());
            max_sz = max_sz.max(payload.len());
            match &first {
                None => {
                    let n = payload.len().min(64);
                    let hex: String = payload[..n].iter().map(|b| format!("{b:02x} ")).collect();
                    println!("first frame ({} bytes), first {n}:\n  {hex}", payload.len());
                    first = Some(payload);
                }
                Some(f) => {
                    if *f != payload {
                        changed = true;
                    }
                }
            }
        }
    }
    println!(
        "\nstream 0x{id:03x}: {count} notifs in {secs}s (~{:.0} Hz), payload {}..{} bytes, payload {}",
        count as f64 / secs as f64,
        if min_sz == usize::MAX { 0 } else { min_sz },
        max_sz,
        if changed {
            "CHANGES frame-to-frame (LIVE DATA)"
        } else if count > 1 {
            "is CONSTANT (static config, not live)"
        } else {
            "seen too rarely to judge"
        }
    );
    Ok(())
}

/// Diagnostic: hunt for undiscovered TTP streams (chiefly the head-pose stream).
/// We only ever subscribe to gaze (0x500); this baselines the gaze-only notify
/// ops, subscribes to a range of candidate stream ids, then watches which notify
/// ops newly appear. A newly-appearing op is a stream the device started sending
/// because we asked — a real find. Non-destructive (subscribe only, no writes).
/// Default range 0x501..=0x520 (adjacent to gaze); override with START END (hex).
fn probe_streams(args: &[String]) -> CmdResult {
    let parse_hex = |s: &String| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok();
    let start = args.get(2).and_then(parse_hex).unwrap_or(0x501);
    let end = args.get(3).and_then(parse_hex).unwrap_or(0x520);

    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    reapply_display_area(&mut conn);

    eprintln!("baseline (gaze only), 1.5s...");
    let base = collect_notif_ops(&mut conn, Duration::from_millis(1500));
    eprintln!(
        "  baseline notify ops: {}",
        base.keys()
            .map(|o| format!("0x{o:03x}"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Short window so silent (unsupported) stream ids fail fast instead of
    // burning the full 10s request deadline each.
    conn.set_request_timeout(Duration::from_millis(300));
    eprintln!("subscribing to stream ids 0x{start:03x}..=0x{end:03x}...");
    for id in start..=end {
        match conn.subscribe_stream(id) {
            Ok(true) => eprintln!("  0x{id:03x}: ACK"),
            Ok(false) => {}
            Err(e) => eprintln!("  0x{id:03x}: err {e}"),
        }
    }

    eprintln!("reading notifications for 5s...");
    let after = collect_notif_ops(&mut conn, Duration::from_secs(5));
    println!("=== notify ops after subscribing ===");
    for (op, (count, sz)) in &after {
        let novel = if base.contains_key(op) {
            ""
        } else {
            "   <== NEW STREAM (appeared only after subscribing)"
        };
        println!("  op 0x{op:03x}: {count} notifs, ~{sz} bytes{novel}");
    }
    if after.keys().all(|o| base.contains_key(o)) {
        println!(
            "\nNo new streams in 0x{start:03x}..=0x{end:03x}. Try a wider range, \
             e.g. `tobii probe-streams 0x400 0x600`, or the head pose is host-derived."
        );
    }

    // Leave the session as we found it. Without this, every id that acked keeps
    // streaming for the rest of the connection, so a probe permanently changes
    // the traffic pattern it was measuring — and anything run afterwards in the
    // same session sees a device that is busier than normal.
    let mut released = 0u32;
    for id in start..=end {
        if let Ok(true) = conn.unsubscribe_stream(id) {
            released += 1;
        }
    }
    eprintln!("released {released} subscription(s) (op 0x4ce, unverified — see frame.rs)");
    Ok(())
}

/// Ask the device to enumerate its own streams (op `0x4b0`).
///
/// The op number comes from a third-party middleware *emulator*'s canned reply,
/// not from a capture of real hardware, so this command exists mainly to settle
/// whether it is real at all. It prints the raw reply: we have no model for the
/// encoding, and inventing one from an emulator's fixture would be how a wrong
/// "fact" enters the wiki.
fn stream_catalog() -> CmdResult {
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    conn.set_request_timeout(Duration::from_secs(2));
    match conn.stream_catalog()? {
        Some(payload) => {
            let entries = tobii_protocol::commands::parse_stream_catalog(&payload);
            if entries.is_empty() {
                println!(
                    "op 0x4b0 answered {} bytes but did not parse as a",
                    payload.len()
                );
                println!("catalog — the layout may differ on this firmware. Raw:");
                println!("  {}", hex(&payload));
                return Ok(());
            }
            println!("the device reports {} streams:", entries.len());
            for e in entries {
                println!("  0x{:04x}  {}", e.id, e.name);
            }
        }
        None => println!(
            "op 0x4b0 got no response in 2s — most likely not a real op on this device. \
             The third-party source for it was an emulator, not hardware."
        ),
    }
    Ok(())
}

/// Print the device's own log stream (`0x1772`) until interrupted.
///
/// The tracker narrates its internal state — protocol events, USB power
/// transitions — as length-prefixed ASCII. Everything here we would otherwise
/// have to infer from the outside.
fn device_log(args: &[String]) -> CmdResult {
    let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    reapply_display_area(&mut conn);
    conn.set_request_timeout(Duration::from_millis(500));
    if !conn.subscribe_stream(0x1772)? {
        return Err("device refused the log subscription (0x1772)".into());
    }
    eprintln!("device log for {secs}s (Ctrl-C to stop)...");
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        for (op, payload) in conn.read_notifications() {
            if op != 0x1772 {
                continue;
            }
            // The text sits under two levels of nesting whose shape differs
            // between message kinds, and we have only a handful of examples.
            // Finding the string beats modelling a structure we do not yet
            // understand: `read_string` validates its own framing, so a false
            // positive on a stray 0x14 byte fails rather than prints garbage.
            let line = (0..payload.len()).find_map(|i| {
                if payload[i] != 0x14 {
                    return None;
                }
                tobii_protocol::tlv::Reader::new(&payload[i..])
                    .read_string()
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            });
            match line {
                Some(l) => println!("{}", l.trim_end()),
                None => eprintln!("(no text found in a {}-byte log payload)", payload.len()),
            }
        }
    }
    let _ = conn.unsubscribe_stream(0x1772);
    Ok(())
}

/// Lowercase hex, for dumping a payload we cannot yet decode.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Capture DECODED camera frames (0x501/0x50e) and save them as viewable PGM
/// images. Confirms the camera pipeline end-to-end and lets you eyeball the NIR
/// image the head-pose model will consume. `tobii camera [id-hex] [count]`
/// (default 0x501, 3 frames). PGM is dependency-free; open with any image viewer.
/// Subscribe BOTH eye-camera streams at once and answer the two questions the
/// head-pose work is blocked on.
///
/// 1. Are `0x501` and `0x50e` a stereo pair, or the same image twice? The
///    module doc for `camera.rs` asserts "two near-infrared cameras (a stereo
///    pair)", and that has never been checked with a face in view. It decides
///    whether a depth-from-stereo path exists at all, and whether the
///    `PoseModel` trait should take a second frame.
/// 2. Is a face legible in an ET5 NIR frame? Every measurement so far was taken
///    on an empty scene, where the frames are the illuminator's own vignette
///    (peak 30 of 255) and say nothing.
///
/// Matching is by the frames' own device timestamps, so the comparison is
/// between images captured at the same instant rather than merely adjacent.
fn camera_both(args: &[String]) -> CmdResult {
    use std::collections::HashMap;
    use tobii_protocol::camera::decode_camera_frame;
    let secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(6);

    let dir = private_dump_dir()?;
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    reapply_display_area(&mut conn);
    conn.set_request_timeout(Duration::from_millis(300));
    conn.subscribe_stream(0x501)?;
    conn.subscribe_stream(0x50e)?;
    eprintln!("SIT IN FRONT OF THE TRACKER. Capturing both cameras for {secs}s...");

    let mut left: HashMap<i64, Vec<u8>> = HashMap::new();
    let mut right: HashMap<i64, Vec<u8>> = HashMap::new();
    let (mut eye_frames, mut gaze_frames) = (0u32, 0u32);
    let mut best: Option<(u8, i64)> = None;
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        for (op, payload) in conn.read_notifications() {
            if op == tobii_protocol::frame::OP_GAZE_NOTIFY {
                if let Some(g) = tobii_protocol::GazeSample::decode(&payload) {
                    gaze_frames += 1;
                    eye_frames += u32::from(g.validity_l == 0 || g.validity_r == 0);
                }
                continue;
            }
            let Some(f) = decode_camera_frame(&payload) else {
                continue;
            };
            let peak = f.pixels.iter().copied().max().unwrap_or(0);
            if best.is_none_or(|(b, _)| peak > b) {
                best = Some((peak, f.timestamp_us));
            }
            let slot = if op == 0x501 { &mut left } else { &mut right };
            slot.insert(f.timestamp_us, f.pixels);
        }
    }
    let _ = conn.unsubscribe_stream(0x501);
    let _ = conn.unsubscribe_stream(0x50e);

    let mut matched = 0usize;
    let mut identical = 0usize;
    for (ts, l) in &left {
        if let Some(r) = right.get(ts) {
            matched += 1;
            identical += usize::from(l == r);
        }
    }
    println!(
        "\ncaptured {} left, {} right frames",
        left.len(),
        right.len()
    );
    println!(
        "gaze: {eye_frames}/{gaze_frames} frames with an eye detected{}",
        if eye_frames == 0 {
            "  <-- NOBODY IN VIEW; everything below is meaningless"
        } else {
            ""
        }
    );
    println!(
        "brightest pixel seen anywhere: {}",
        best.map_or(0, |(b, _)| b)
    );
    if matched == 0 {
        println!("no timestamp-matched pairs — the two streams are not aligned");
    } else {
        println!(
            "timestamp-matched pairs: {matched}, byte-identical: {identical} ({:.0}%)",
            100.0 * identical as f64 / matched as f64
        );
        println!(
            "  => {}",
            if identical == matched {
                "NOT a stereo pair — 0x50e is the same image as 0x501"
            } else if identical == 0 {
                "genuinely two different views"
            } else {
                "mixed; inconclusive, capture again"
            }
        );
    }
    // Keep the brightest frame so a human can look at whether it shows a face.
    if let Some((_, ts)) = best {
        if let Some(px) = left.get(&ts).or_else(|| right.get(&ts)) {
            let path = dir.join("brightest.pgm");
            let mut out = "P5\n280 280\n255\n".to_string().into_bytes();
            out.extend_from_slice(px);
            std::fs::write(&path, out)?;
            println!("brightest frame written to {}", path.display());
        }
    }
    Ok(())
}

/// Print everything an issue report needs, and nothing that identifies you.
///
/// Written to stdout by default because that is what can be pasted into a
/// GitHub issue form — which is the only place it can be made *required*, since
/// issue forms have no file-upload field at all. `--file` is for anyone who
/// would rather send a file.
fn debug_report(args: &[String]) -> CmdResult {
    let text = tobii_diagnostics::report();
    match flag_value(args, "--file") {
        Some(path) => {
            std::fs::write(path, &text)?;
            eprintln!("wrote {path}");
            eprintln!("Please read it before attaching it — it is plain text on purpose.");
        }
        None => {
            print!("{text}");
            eprintln!(
                "\nPaste the above into an issue at \
                 https://github.com/Tropaion/Tobii_Linux/issues/new/choose"
            );
        }
    }
    Ok(())
}

/// Record a real session to a capture file, for replay in tests.
///
/// The point is regression testing without hardware: everything this driver
/// knows about the ET5 was learned by watching a real one, and none of that
/// knowledge is checked by anything unless a tracker is plugged in. A recorded
/// session lets CI assert that the driver still sends the same frames, in the
/// same order, given the same replies.
///
/// The session recorded here is deliberately the *boring* one — connect,
/// re-apply the saved display area, read the eye selection, subscribe to gaze,
/// take some frames, unsubscribe. That is the path every single run of this
/// program takes, so it is the path worth pinning.
///
/// Re-record after a firmware update, or after any protocol change that is
/// meant to alter the conversation: a capture is a photograph of what happened
/// once, not a specification.
fn record_session(args: &[String]) -> CmdResult {
    // Two captures, not one, and the split is about reviewability.
    //
    // The everyday session is a few hundred short lines: a re-recording after a
    // firmware change produces a diff a human can read, which is the whole
    // reason the format is line-oriented hex. The calibration blob is 778 KB —
    // one 32,000-character line whose diff says nothing to anybody. Keeping
    // them apart means the file you actually read stays readable, and the
    // opaque one is opaque on purpose.
    // Parsed strictly, because the default output path is a COMMITTED TEST
    // FIXTURE. Every way this used to be lenient ended with one of them
    // overwritten:
    //
    //   * an unrecognised `--flag` was ignored, so `tobii record --calibraton`
    //     (one letter short) recorded a plain session straight over
    //     session.tobiicap and said nothing;
    //   * FILE was "the first argument not starting with --", so
    //     `tobii record --calibration 60` — the frame count, which
    //     `--calibration` does not take — wrote the capture to a file called
    //     `60`.
    let mut with_calibration = false;
    let mut positional: Vec<&str> = Vec::new();
    for a in args.iter().skip(2) {
        match a.as_str() {
            "--calibration" => with_calibration = true,
            f if f.starts_with("--") => {
                return Err(format!(
                    "unknown option `{f}`. Usage: tobii record [--calibration] [FILE] [FRAMES]"
                )
                .into())
            }
            other => positional.push(other),
        }
    }
    if with_calibration {
        if let Some(n) = positional.first() {
            if n.parse::<usize>().is_ok() {
                return Err(format!(
                    "`{n}` looks like a frame count, and --calibration records no gaze \
                     frames. Give a FILE, or drop the number."
                )
                .into());
            }
        }
        if positional.len() > 1 {
            return Err("--calibration takes at most a FILE".to_string().into());
        }
    }
    let default = if with_calibration {
        "crates/tobii-usb/tests/captures/calibration.tobiicap"
    } else {
        "crates/tobii-usb/tests/captures/session.tobiicap"
    };
    let path = std::path::PathBuf::from(*positional.first().unwrap_or(&default));
    let frames: usize = if with_calibration {
        0
    } else {
        match positional.get(1) {
            Some(n) => n
                .parse()
                .map_err(|_| format!("FRAMES must be a number, not `{n}`"))?,
            None => 40,
        }
    };

    eprintln!("recording a session to {} ...", path.display());
    let transport = tobii_usb::RecordTransport::new(UsbTransport::open()?)
        .with_note(if with_calibration {
            "connect, display area, enabled eye, calibration retrieve (fragmented)"
        } else {
            "connect, display area, enabled eye, gaze subscribe, gaze frames, unsubscribe"
        })
        .header("recorded-at", &now_rfc3339())
        .header("device", "2104:0313")
        .header("driver-version", env!("CARGO_PKG_VERSION"));

    let mut conn = Connection::connect(transport)?;
    // The same things the GUI's device thread does on every connect. The ET5
    // wipes its display area on reboot, so this is not an optional extra — it
    // is what makes the tracker work at all, and therefore the path most worth
    // pinning.
    //
    // The corners come from this machine's config, which a replay test cannot
    // know, so they are written into a header. The test reads them back and
    // drives the same call, and the recorded frame then compares byte for byte
    // on any machine.
    let mut corners_header = String::new();
    if let Ok(Some(setup)) = tobii_config::load() {
        let c = setup.to_corners();
        let _ = conn.set_display_area(&c);
        let nums: Vec<String> =
            c.tl.iter()
                .chain(c.tr.iter())
                .chain(c.bl.iter())
                .map(|v| format!("{v}"))
                .collect();
        corners_header = nums.join(" ");
    } else {
        eprintln!("  no display area configured — the recording will not cover applying one");
    }
    let eye = conn.get_enabled_eye()?;
    eprintln!("  enabled eye: {eye:?}");

    // Retrieve the calibration blob — the ONLY thing this driver does that
    // produces a fragmented response, and therefore the only way a capture can
    // exercise the parser's continuation-envelope handling.
    //
    // This is not hypothetical coverage: a guard that assumed the continuation
    // envelope's length field fits inside its own USB read passed every test
    // and broke every calibration retrieval on real hardware, because the field
    // is the size of the whole continuation RUN. A capture without a fragmented
    // response cannot catch that class at all.
    //
    // A read, never a write: nothing here changes what is on the device.
    if with_calibration {
        // Fatal, and nothing is written. This used to print a warning, save
        // anyway and exit 0 — so a failed retrieve replaced the committed
        // fixture with a capture that has no fragmented response in it, and the
        // test that exists to catch the continuation-guard bug would have gone
        // on passing against a recording that could no longer catch it.
        let blob = conn.retrieve_calibration().map_err(|e| {
            format!(
                "no calibration blob ({e}), so this recording would not cover fragmented \
                 reassembly — which is the only reason this capture exists. Nothing was \
                 written. Calibrate the tracker first."
            )
        })?;
        eprintln!("  calibration blob: {} bytes (fragmented)", blob.0.len());
    }

    if frames > 0 {
        conn.subscribe_stream(tobii_protocol::frame::STREAM_GAZE)?;
        eprintln!("  subscribed; collecting {frames} gaze frames (look at the screen)...");
    }
    let mut got = 0usize;
    if frames > 0 {
        let deadline = Instant::now() + Duration::from_secs(20);
        while got < frames && Instant::now() < deadline {
            if conn.next_gaze().is_some() {
                got += 1;
            }
        }
        eprintln!("  {got} gaze frames");
        let _ = conn.unsubscribe_stream(tobii_protocol::frame::STREAM_GAZE);
    }

    let mut capture = conn.into_transport().into_capture();
    if !corners_header.is_empty() {
        capture
            .headers
            .push(("display-area".to_string(), corners_header));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tobii_usb::capture::save(&capture, &path)?;
    if frames > 0 && got == 0 {
        eprintln!(
            "warning: no gaze frames were captured — the recording still covers the \
             handshake, but nothing in it exercises gaze decoding."
        );
    }
    Ok(())
}

/// The current time, as the subset of RFC 3339 a header needs.
///
/// The civil-time arithmetic is the log's: every log line is stamped with the
/// same six numbers, and hand-rolling it — the same trade the JSON and SHA-256
/// code in this workspace make — is worth doing once, not twice.
fn now_rfc3339() -> String {
    let (y, m, d, h, mi, s) = tobii_diagnostics::log::utc_now();
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn camera(args: &[String]) -> CmdResult {
    use tobii_protocol::camera::decode_camera_frame;
    let id = args
        .get(2)
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x501);
    let count: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);

    let dir = private_dump_dir()?;
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    reapply_display_area(&mut conn);
    conn.set_request_timeout(Duration::from_millis(300));
    conn.subscribe_stream(id)?;
    eprintln!(
        "capturing {count} camera frame(s) from 0x{id:03x} to {} (sit in view)...",
        dir.display()
    );

    let mut saved = 0u32;
    let deadline = Instant::now() + Duration::from_secs(20);
    // Whether the device has been seeing eyes during this capture, and when it
    // last did.
    //
    // Declared OUT here on purpose. It used to live inside the `while`, so it
    // was reset on every `read_notifications` chunk and could only ever be true
    // if a gaze notification arrived in the SAME chunk as the camera frame,
    // earlier in the list. That never happens: a 280x280 camera payload is
    // 78400 bytes against a 16 KB read buffer, so a completed camera frame
    // always emerges from its own transfer. Measured: 0 of 265 frames ever said
    // DETECTED, so the annotation this exists for could not fire, and the "dark
    // frame with no eyes" hint below fired on every dark frame regardless.
    let mut last_eyes: Option<Instant> = None;
    while saved < count && Instant::now() < deadline {
        for (op, payload) in conn.read_notifications() {
            // Track whether the device is actually seeing eyes while these
            // frames are captured. Without it a dark frame is ambiguous: nobody
            // in view, or illuminators off. Head-pose work needs to tell those
            // apart before blaming a model for finding no face.
            if op == tobii_protocol::frame::OP_GAZE_NOTIFY {
                if let Some(g) = tobii_protocol::GazeSample::decode(&payload) {
                    if g.validity_l == 0 || g.validity_r == 0 {
                        last_eyes = Some(Instant::now());
                    }
                }
            }
            if op != id as u32 {
                continue;
            }
            let Some(f) = decode_camera_frame(&payload) else {
                continue;
            };
            let mean =
                f.pixels.iter().map(|&b| b as u64).sum::<u64>() / f.pixels.len().max(1) as u64;
            let path = dir.join(format!("cam-{id:03x}-{saved}.pgm"));
            // PGM (P5) grayscale: header + raw bytes. No image-crate dependency.
            let mut out = format!("P5\n{} {}\n255\n", f.width, f.height).into_bytes();
            out.extend_from_slice(&f.pixels);
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.write_all(&out)?;
            let peak = f.pixels.iter().copied().max().unwrap_or(0);
            // Eyes seen recently enough to describe THIS frame. The gaze and
            // camera notifications arrive in separate transfers, so they are
            // never simultaneous; a second is generous for 30 Hz gaze and still
            // short enough that "detected" means during this capture.
            let eyes_seen = last_eyes.is_some_and(|t| t.elapsed() < Duration::from_secs(1));
            println!(
                "{}  ({}x{}, {}-bit, mean {mean}, peak {peak}, eyes {})",
                path.display(),
                f.width,
                f.height,
                f.bit_depth,
                if eyes_seen {
                    "DETECTED"
                } else {
                    "not detected"
                }
            );
            if !eyes_seen && peak < 64 {
                println!(
                    "    ^ dark frame with no eyes detected — nobody in view, so this \
                     says nothing about whether a face is legible to a model."
                );
            }
            saved += 1;
            if saved >= count {
                break;
            }
        }
    }
    if saved == 0 {
        println!("no camera frames from 0x{id:03x} (is it a camera stream? try 0x501 or 0x50e)");
    }
    Ok(())
}

/// Diagnostic: capture raw frames of ONE stream to /tmp for offline analysis.
/// Used to decode the eye-camera image streams (0x501/0x50e) — their pixel
/// format determines whether an off-the-shelf head-pose model can consume them.
/// `tobii dump-stream <id-hex> [count]` writes /tmp/stream-<id>-<n>.bin.
fn dump_stream(args: &[String]) -> CmdResult {
    let id = args
        .get(2)
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or("usage: tobii dump-stream <stream-id-hex> [count]")?;
    let count: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);

    // Write into a FRESH PRIVATE directory, not fixed /tmp paths. A predictable
    // world-writable path (`/tmp/stream-….bin`) lets a local attacker pre-plant a
    // symlink there so our write lands on one of the user's files instead; the
    // per-run 0700 dir + O_EXCL opens below refuse to follow a planted symlink.
    let dir = private_dump_dir()?;

    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    reapply_display_area(&mut conn);
    conn.set_request_timeout(Duration::from_millis(300));
    conn.subscribe_stream(id)?;
    eprintln!(
        "capturing {count} frame(s) of stream 0x{id:03x} to {} (sit in view)...",
        dir.display()
    );

    let mut saved = 0u32;
    let deadline = Instant::now() + Duration::from_secs(20);
    while saved < count && Instant::now() < deadline {
        for (op, payload) in conn.read_notifications() {
            if op != id as u32 {
                continue;
            }
            let path = dir.join(format!("stream-{id:03x}-{saved}.bin"));
            // create_new = O_CREAT|O_EXCL: fails rather than following a symlink.
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            f.write_all(&payload)?;
            println!("wrote {} ({} bytes)", path.display(), payload.len());
            saved += 1;
            if saved >= count {
                break;
            }
        }
    }
    if saved == 0 {
        println!("no frames captured for 0x{id:03x} (does it stream? try `probe-stream`)");
    }
    Ok(())
}

/// A fresh, private (0700) per-run directory under the system temp dir for
/// diagnostic captures. `create_dir` fails if the path already exists — including
/// a pre-planted symlink — so an attacker cannot redirect our writes.
fn private_dump_dir() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let uniq = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("tobii-dump-{}-{uniq}", std::process::id()));
    std::fs::create_dir(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

/// Diagnostic: stream the FULL column inventory of each gaze frame, including
/// the columns `stream`/`headpose` discard. Used to map the head-pose data:
/// move your head one axis at a time (translate, then yaw/pitch/roll) and watch
/// which columns track the motion. A first pass showed the point3d columns are
/// all eye positions (0x02/0x08/0x17/0x18/0x22/0x24) and per-eye gaze
/// directions (0x04/0x0a) — no clean 6DOF pose — so this now also prints the
/// fixed16x16 and integer columns, where explicit head-orientation angles would
/// live. Redirect to a file. Needs a valid display area or the device reports
/// no eyes.
fn columns() -> CmdResult {
    use tobii_protocol::gaze::{column_inventory, ColumnValue};
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    reapply_display_area(&mut conn);
    eprintln!(
        "streaming column inventory (~2/s) — move your head ONE axis at a time; \
         watch which columns change (Ctrl-C to stop)"
    );
    let mut last = std::time::Instant::now();
    loop {
        let Some(payload) = conn.next_gaze_payload() else {
            continue;
        };
        // Throttle to ~2 Hz so the output is readable and loggable.
        if last.elapsed().as_millis() < 500 {
            continue;
        }
        last = std::time::Instant::now();
        let inv = column_inventory(&payload);
        // Print EVERY column that carries a value that could plausibly move
        // with the head: point3d/point2d (positions/directions) AND the
        // fixed16x16 and s64/u32 columns, which is where an explicit head
        // orientation (Euler angles or a quaternion) would live. Only truly
        // constant sentinels are suppressed. Unmapped columns are flagged.
        let mapped3 = [0x02, 0x03, 0x04, 0x08, 0x09, 0x0a, 0x17, 0x18];
        let mapped_other = [0x01, 0x06, 0x0c, 0x07, 0x0d, 0x14, 0x1c];
        println!("--- {} columns ---", inv.len());
        for (col, v) in &inv {
            let (line, unmapped) = match v {
                ColumnValue::Point3d(p) if *p != [0.0; 3] => (
                    format!("({:.1}, {:.1}, {:.1})", p[0], p[1], p[2]),
                    !mapped3.contains(col),
                ),
                ColumnValue::Point2d(p) if *p != [-1.0, -1.0] && *p != [0.0, 0.0] => (
                    format!("({:.4}, {:.4})", p[0], p[1]),
                    !mapped_other.contains(col),
                ),
                ColumnValue::Fixed(f) if *f != -1.0 && *f != 0.0 => {
                    (format!("{f:.4}"), !mapped_other.contains(col))
                }
                ColumnValue::U32(u) if *u != 0 && *u != 4 => {
                    (format!("{u}"), !mapped_other.contains(col))
                }
                ColumnValue::S64(s) if *s != 0 => (format!("{s}"), !mapped_other.contains(col)),
                _ => continue,
            };
            let mark = if unmapped { "   <-- unmapped" } else { "" };
            println!("  0x{col:02x} = {line}{mark}");
        }
    }
}

/// Re-apply the saved display area to a freshly connected device.
///
/// The ET5 resets its display area to a ~4mm stub on every reboot (it reboots
/// on session close), and emits no eye-tracking data at all until a valid area
/// is set — so every command that wants gaze data must do this in-session right
/// after connecting, or the user sees a device that reports no eyes forever.
/// Failures are reported but not fatal: the device may already have a usable
/// area from another session.
fn reapply_display_area(conn: &mut Connection<UsbTransport>) {
    match tobii_config::load() {
        Ok(Some(setup)) => match conn.set_display_area(&setup.to_corners()) {
            Ok(true) => eprintln!(
                "display area applied ({:.0}x{:.0}mm)",
                setup.width_mm, setup.height_mm
            ),
            Ok(false) => eprintln!("warning: display area sent but not acknowledged"),
            Err(e) => eprintln!("warning: could not set display area ({e})"),
        },
        Ok(None) => {
            eprintln!("note: no saved display config — run `tobii setup` first, or eyes won't be detected")
        }
        Err(e) => eprintln!("warning: could not load config ({e})"),
    }
}

fn stream(json: bool, eyes: bool) -> CmdResult {
    eprintln!("opening Tobii ET5...");
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    reapply_display_area(&mut conn);

    eprintln!("connected — streaming gaze (Ctrl-C to stop)");
    loop {
        let Some(s) = conn.next_gaze() else {
            continue; // read timeout — keep waiting
        };
        if json {
            println!(
                "{{\"t\":{},\"valid\":{},\"x\":{:.5},\"y\":{:.5}}}",
                s.timestamp_us,
                s.has(present::GAZE_2D),
                s.gaze_point_2d[0],
                s.gaze_point_2d[1]
            );
        } else if s.has(present::GAZE_2D) {
            println!(
                "t={:>12}  gaze=({:.4}, {:.4})  valL={} valR={}",
                s.timestamp_us, s.gaze_point_2d[0], s.gaze_point_2d[1], s.validity_l, s.validity_r
            );
            // Diagnostic view of the raw eye geometry the GUI's eye-position
            // box is drawn from: trackbox is the device's own normalized
            // capture volume (independent of any display config), origin is
            // the eye position in tracker-space mm.
            if eyes {
                // Trackbox z is printed too: it is a NORMALIZED depth, and it is
                // what drives the eye-position dot size/brightness ladder (see
                // `tobii-gtk`'s `eyeview`), so validating that it actually
                // sweeps a useful part of [0,1] on real hardware needs it
                // visible. The raw origins (cols 0x17/0x18, pre-calibration
                // detection output) are printed alongside the filtered ones to
                // show whether they lead in phase.
                println!(
                    "                trackbox L=({:.3}, {:.3}, z={:.3})  R=({:.3}, {:.3}, z={:.3})",
                    s.trackbox_eye_l[0],
                    s.trackbox_eye_l[1],
                    s.trackbox_eye_l[2],
                    s.trackbox_eye_r[0],
                    s.trackbox_eye_r[1],
                    s.trackbox_eye_r[2],
                );
                println!(
                    "                origin L=({:.0}, {:.0}, {:.0})mm  R=({:.0}, {:.0}, {:.0})mm   \
                     raw L z={:.0} R z={:.0}",
                    s.eye_origin_l_mm[0],
                    s.eye_origin_l_mm[1],
                    s.eye_origin_l_mm[2],
                    s.eye_origin_r_mm[0],
                    s.eye_origin_r_mm[1],
                    s.eye_origin_r_mm[2],
                    s.eye_origin_raw_l_mm[2],
                    s.eye_origin_raw_r_mm[2],
                );
            }
        } else {
            println!("t={:>12}  (no 2D gaze this frame)", s.timestamp_us);
        }
    }
}

/// opentrack's default "UDP over network" endpoint, and the send rate when
/// `--rate` is not given.
///
/// Re-exported from `tobii-output` rather than declared again: both were
/// duplicated here with the same values, and a default that disagrees between
/// the command and the config file is a bug nobody would look for.
use tobii_output::games::DEFAULT_OPENTRACK_ADDR as DEFAULT_UDP_ADDR;
/// How often the human-readable status line is printed to stderr.
const STATUS_INTERVAL: Duration = Duration::from_secs(1);
/// How stale a measured (model) rotation may be before the command stops
/// handing it to `fuse_pose`.
///
/// The camera and the gaze stream both run at ~33 Hz but are not in lockstep,
/// so holding the last model pose bridges the gaps rather than dropping pitch
/// to zero on every gaze sample that arrived without an image. After a second
/// the hold is a guess, and the geometric pose — which tracks the eyes that are
/// really there — is the honest answer. Same rule and same value as the hub's
/// `outputs::HEAD_POSE_MAX_AGE`.
const HEAD_POSE_MAX_AGE: Duration = Duration::from_millis(1000);

/// Value of a `--flag VALUE` style option, if present.
fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).map(String::as_str)
}

/// Resolve the `--udp` argument to a single socket address.
fn parse_udp_addr(raw: &str) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    raw.to_socket_addrs()?
        .next()
        .ok_or_else(|| format!("`{raw}` did not resolve to any address").into())
}

/// Parse the `--rate` argument into a positive, finite frequency in Hz.
/// Slowest and fastest send rates this will accept.
///
/// A lower bound is not fussiness: `send_interval` is `1.0 / rate_hz`, and
/// `Duration::from_secs_f64` panics above about 1.8e19 seconds — so
/// `--rate 1e-300` passed the "positive and finite" test and then killed the
/// process on the next line. One sample per minute is already far slower than
/// anything usable for head tracking.
const MIN_RATE_HZ: f64 = 1.0 / 60.0;
const MAX_RATE_HZ: f64 = 10_000.0;

fn parse_rate(raw: &str) -> Result<f64, Box<dyn std::error::Error>> {
    match raw.parse::<f64>() {
        Ok(hz) if hz.is_finite() && (MIN_RATE_HZ..=MAX_RATE_HZ).contains(&hz) => Ok(hz),
        Ok(hz) if hz.is_finite() && hz > 0.0 => Err(format!(
            "--rate must be between {MIN_RATE_HZ:.4} and {MAX_RATE_HZ} Hz, got `{raw}`"
        )
        .into()),
        _ => Err(format!("--rate needs a positive number of Hz, got `{raw}`").into()),
    }
}

/// Stream a derived head pose to opentrack over UDP.
///
/// Pitch is always zero — it cannot be recovered from two eye positions. See
/// the `tobii-headpose` crate docs for the geometry and the (still unvalidated)
/// sign conventions.
/// Which head-pose model to run, from `--model`.
enum ModelChoice {
    /// Use the installed model if there is one; otherwise the geometric path.
    Auto,
    /// Never load a model, even if one is installed.
    Off,
    /// Load this exact file. Only read by the `onnx` build; the lean build
    /// still parses the flag so that a script passing it does not fail, it just
    /// has nothing to load it with.
    Path(#[cfg_attr(not(feature = "onnx"), allow(dead_code))] std::path::PathBuf),
}

fn model_choice(args: &[String]) -> ModelChoice {
    match flag_value(args, "--model") {
        None | Some("auto") => ModelChoice::Auto,
        Some("off") | Some("none") => ModelChoice::Off,
        Some(p) => ModelChoice::Path(p.into()),
    }
}

#[cfg(feature = "onnx")]
type Model = tobii_headpose::onnx::OnnxPose;
#[cfg(not(feature = "onnx"))]
type Model = std::convert::Infallible;

/// Load the neural backend, reporting what happened on stderr. A missing model
/// is not an error — it is the ordinary state of a fresh install.
#[cfg(feature = "onnx")]
fn open_model(choice: &ModelChoice) -> Option<Model> {
    use tobii_headpose::model::{ModelConfig, ModelKind};
    use tobii_headpose::model_store;
    use tobii_headpose::onnx::OnnxPose;
    let loaded = match choice {
        ModelChoice::Off => return None,
        ModelChoice::Auto => {
            if model_store::installed(&model_store::HEAD_POSE).is_none() {
                eprintln!(
                    "no head-pose model installed — reporting 5 DOF (no pitch). \
                     Run `tobii headpose --fetch-model` to add it."
                );
                return None;
            }
            OnnxPose::from_store()
        }
        ModelChoice::Path(p) => OnnxPose::load(&ModelConfig {
            kind: ModelKind::OpentrackOnnx,
            model_path: p.clone(),
        }),
    };
    match loaded {
        Ok(mut m) => {
            // The model reports pitch in its own frame, which is offset from
            // level by the training set's convention plus the tracker's upward
            // tilt. That offset is a per-installation measurement, not a
            // constant we can ship — see `--calibrate-pitch`.
            match tobii_headpose::model_store::pitch_offset() {
                Some(off) => {
                    let mut signs = m.tracker().signs();
                    signs.pitch_offset_deg = off;
                    m.tracker().set_signs(signs);
                    eprintln!("head-pose model loaded — 6 DOF, pitch zero {off:+.1}°");
                }
                None => eprintln!(
                    "head-pose model loaded — 6 DOF, but pitch has no zero yet. \
                     Run `tobii headpose --calibrate-pitch` once."
                ),
            }
            Some(m)
        }
        Err(e) => {
            eprintln!("could not load the head-pose model ({e}); reporting 5 DOF");
            None
        }
    }
}

#[cfg(not(feature = "onnx"))]
fn open_model(_choice: &ModelChoice) -> Option<Model> {
    None
}

/// The device's wide-angle NIR camera stream, which the model runs on.
const CAMERA_STREAM: u16 = 0x501;

#[cfg(feature = "onnx")]
type ModelPose = tobii_headpose::onnx::ModelPose;
#[cfg(not(feature = "onnx"))]
type ModelPose = std::convert::Infallible;

/// Decode a camera notification and run the model on it, keeping the result if
/// the model was confident.
#[cfg(feature = "onnx")]
fn run_model(model: &mut Option<Model>, payload: &[u8], out: &mut Option<(ModelPose, Instant)>) {
    let Some(m) = model.as_mut() else { return };
    let Some(frame) = tobii_protocol::camera::decode_camera_frame(payload) else {
        return;
    };
    if let Some(pose) = m.estimate_detailed(&frame) {
        *out = Some((pose, Instant::now()));
    }
}

#[cfg(not(feature = "onnx"))]
fn run_model(_m: &mut Option<Model>, _payload: &[u8], _out: &mut Option<(ModelPose, Instant)>) {}

/// Position from the eyes, rotation from the model. See
/// `tobii_headpose::onnx::fuse` for why that split.
#[cfg(feature = "onnx")]
fn fuse_pose(
    eyes: Option<tobii_headpose::HeadPose>,
    model: Option<&ModelPose>,
) -> Option<tobii_headpose::HeadPose> {
    tobii_headpose::onnx::fuse(eyes, model, tobii_headpose::onnx::RotationSource::Model)
}

#[cfg(not(feature = "onnx"))]
fn fuse_pose(
    eyes: Option<tobii_headpose::HeadPose>,
    _model: Option<&ModelPose>,
) -> Option<tobii_headpose::HeadPose> {
    eyes
}

/// `--check`: the model's rotation beside the geometry's.
///
/// This is the whole sign experiment. Turn your head one axis at a time; the two
/// yaw numbers must move together, and so must the two roll numbers. If a pair
/// moves in opposite directions, that sign is inverted in
/// `tobii_headpose::onnx::Signs` — which is the only place it is decided.
///
/// Pitch has no geometric counterpart to check against; what it needs instead is
/// a zero. Sit square-on to the screen: whatever pitch reads then is the offset
/// to cancel with `Signs::pitch_offset_deg`.
#[cfg(feature = "onnx")]
fn print_check_line(
    eyes: Option<tobii_headpose::HeadPose>,
    model: Option<&ModelPose>,
    rates: &str,
) {
    match (eyes, model) {
        (Some(e), Some(m)) => eprintln!(
            "model yaw={:>7.2}° pitch={:>7.2}° roll={:>7.2}° sigma={:.3} head=({:>5.1},{:>5.1})px\n\
             eyes  yaw={:>7.2}°   pitch=  n/a   roll={:>7.2}°   z={:.0}mm   {rates}",
            m.yaw_deg,
            m.pitch_deg,
            m.roll_deg,
            m.sigma,
            m.centre_px[0],
            m.centre_px[1],
            e.yaw_deg,
            e.roll_deg,
            e.z_mm,
        ),
        (Some(e), None) => eprintln!(
            "model  (no confident pose this second)\n\
             eyes  yaw={:>7.2}°   pitch=  n/a   roll={:>7.2}°   z={:.0}mm   {rates}",
            e.yaw_deg, e.roll_deg, e.z_mm
        ),
        (None, Some(m)) => eprintln!(
            "model yaw={:>7.2}° pitch={:>7.2}° roll={:>7.2}° sigma={:.3}\n\
             eyes   (both eyes must be in the trackbox)   {rates}",
            m.yaw_deg, m.pitch_deg, m.roll_deg, m.sigma
        ),
        (None, None) => eprintln!("no pose from either source   {rates}"),
    }
}

#[cfg(not(feature = "onnx"))]
fn print_check_line(
    eyes: Option<tobii_headpose::HeadPose>,
    _model: Option<&ModelPose>,
    rates: &str,
) {
    match eyes {
        Some(e) => eprintln!(
            "eyes  yaw={:>7.2}°   pitch=  n/a   roll={:>7.2}°   z={:.0}mm   {rates}",
            e.yaw_deg, e.roll_deg, e.z_mm
        ),
        None => eprintln!("no pose   {rates}"),
    }
}

/// Measure the pitch zero: sit square-on, hold still, average, save.
///
/// The model's pitch is offset from level by two constants that nothing in the
/// software can separate — the training set's own pose convention, and how far
/// the tracker is tilted up on this particular desk. Both are fixed for an
/// installation, so one measurement settles them together.
#[cfg(feature = "onnx")]
fn calibrate_pitch_zero(
    conn: &mut Connection<UsbTransport>,
    model: &mut Option<Model>,
    secs: u64,
) -> CmdResult {
    let Some(m) = model.as_mut() else {
        return Err("pitch calibration needs the model — `tobii headpose --fetch-model`".into());
    };
    // Measure the model's RAW pitch: applying the old offset while measuring the
    // new one would make each run a correction of the last, not a measurement.
    let mut signs = m.tracker().signs();
    signs.pitch_offset_deg = 0.0;
    m.tracker().set_signs(signs);

    eprintln!(
        "\nSit square-on to the screen, look at its centre, and hold still for {secs}s.\n\
         Starting in 3 seconds..."
    );
    let start = Instant::now() + Duration::from_secs(3);
    let deadline = start + Duration::from_secs(secs);
    let mut samples: Vec<f64> = Vec::new();
    let mut last_note = Instant::now();

    while Instant::now() < deadline {
        for (op, payload) in conn.read_notifications().iter() {
            if *op != u32::from(CAMERA_STREAM) {
                continue;
            }
            let Some(frame) = tobii_protocol::camera::decode_camera_frame(payload) else {
                continue;
            };
            if let Some(p) = m.estimate_detailed(&frame) {
                if Instant::now() >= start {
                    samples.push(p.pitch_deg);
                }
            }
        }
        if last_note.elapsed() >= STATUS_INTERVAL {
            let left = deadline.saturating_duration_since(Instant::now()).as_secs();
            eprintln!("  {left}s to go, {} samples", samples.len());
            last_note = Instant::now();
        }
    }

    let Some((offset, spread)) = tobii_headpose::pitch_offset_from(&mut samples) else {
        return Err(format!(
            "only {} usable frames — was your face in view? Nothing was saved.",
            samples.len()
        )
        .into());
    };
    let median = -offset;

    println!(
        "\nmeasured pitch while sitting square-on: {median:+.2}° (10-90% spread {spread:.2}°)"
    );
    if spread > 8.0 {
        println!("that is a wide spread — you may have moved. Consider re-running.");
    }
    tobii_config::save_pitch_offset(offset)?;
    println!(
        "saved pitch zero {offset:+.2}° to {}",
        tobii_config::pitch_offset_path().display()
    );
    println!("`tobii headpose` will now report 0° when you sit like that.");
    Ok(())
}

#[cfg(not(feature = "onnx"))]
fn calibrate_pitch_zero(
    _conn: &mut Connection<UsbTransport>,
    _model: &mut Option<Model>,
    _secs: u64,
) -> CmdResult {
    Err("this build has no head-pose model support".into())
}

/// Narrow the config to what the user actually asked for.
///
/// See the comment inside: `OutputConfig::default()` is right for a configured
/// machine and wrong as a silent upgrade of a shipped command.
fn apply_games_opt_in(cfg: &mut tobii_output::games::OutputConfig, args: &[String]) {
    let opted_in = cfg.enabled || args.iter().any(|a| a == "--extended-view");
    if !opted_in {
        cfg.extended_view.enabled = false;
        cfg.bridge_port = None;
        // Same reasoning, and louder: a virtual joystick is visible. Somebody
        // running the command they have always run would find a new controller
        // in every game's bind list, which is a stranger thing to happen than
        // an unused socket.
        cfg.joystick = false;
    }
    if args.iter().any(|a| a == "--no-extended-view") {
        cfg.extended_view.enabled = false;
    }
}

/// Whether this run should take a rotation reference.
///
/// Both spellings are accepted. `--recenter` is the documented one, because it
/// is the spelling in `docs/wiki/Planned-Work.md` and the one an English
/// keyboard produces by habit; every line of prose in this repository spells it
/// "recentre", and somebody typing what they just read should not be answered
/// with a usage message.
fn wants_recentre(args: &[String]) -> bool {
    args.iter().any(|a| a == "--recenter" || a == "--recentre")
}

/// How much of this session's head pose is a reconstruction, and whether the
/// pose right now is one.
///
/// A dropped eye used to cost the frame its pose outright; `PairOffset` now
/// rebuilds the missing eye from the last measured offset — a guess, which must
/// not be able to travel as a measurement. This is where it says so, and it is
/// a fragment appended to the status line rather than a line of its own because
/// that line is read while a game is running.
///
/// Empty while every pose came from two measured eyes, which is the ordinary
/// case and needs no word. `(now)` marks a pose that is a reconstruction at this
/// instant, so a tracker that cannot see one eye at all reads differently from
/// one that blinked twenty minutes ago.
fn fallback_note(stats: tobii_output::pipeline::FallbackStats) -> String {
    // Nothing to say until a frame has actually been reconstructed — which also
    // means `total` below is at least one, so the percentage cannot divide by
    // zero.
    if stats.reconstructed == 0 {
        return String::new();
    }
    let total = stats.both_eyes + stats.reconstructed;
    // A whole percent: this is a proportion read at a glance beside three
    // rates, not a number anybody computes with.
    let pct = stats.reconstructed as f64 * 100.0 / total as f64;
    format!(
        ", one eye {pct:.0}%{}",
        if stats.active { " (now)" } else { "" }
    )
}

/// Apply one `KEY VALUE` to a config, or say why it was refused.
///
/// Split from [`games_cmd`] so the decision can be tested without touching the
/// user's config file. It used to be inline, and the test that claimed to cover
/// it exercised only `tobii-output` — breaking this arm outright left
/// `cargo test -p tobii-cli` entirely green.
fn games_set(
    cfg: &mut tobii_output::games::OutputConfig,
    key: &str,
    value: &str,
) -> Result<(), String> {
    if cfg.apply_key(key, value) {
        return Ok(());
    }
    // `OutputConfig::keys()` is the one list. It is hand-written and pinned by
    // a test to exactly what `to_toml` emits — scraping a second list out of
    // `to_toml` here, which is what this did at first, was itself the
    // duplication it claimed to be avoiding.
    Err(format!(
        "{key} = {value:?} was not accepted.\nvalid keys: {}",
        tobii_output::games::OutputConfig::keys().join(", ")
    ))
}

/// `tobii games` — show the game-output settings; `tobii games set K V` — change one.
///
/// Writes through [`OutputConfig::apply_key`], the same function the file
/// parser uses, so the CLI cannot accept a spelling the file would reject or
/// vice versa. That is the whole reason this command is thin: the validation
/// lives with the config, not here.
fn games_cmd(sub: Option<&str>, args: &[String]) -> CmdResult {
    use tobii_output::games::{load_output_config, save_output_config};

    match sub {
        None => {
            print!("{}", load_output_config().to_toml());
            println!("\n# path: {}", tobii_output::games::games_path().display());
            Ok(())
        }
        Some("set") => {
            let (key, value) = match (args.get(3), args.get(4)) {
                (Some(k), Some(v)) => (k.as_str(), v.as_str()),
                _ => return Err("usage: tobii games set KEY VALUE".into()),
            };
            let mut cfg = load_output_config();
            games_set(&mut cfg, key, value)?;
            save_output_config(&cfg)?;
            println!("{key} = {value}");
            Ok(())
        }
        Some("profile") => profile_cmd(args),
        Some(other) => Err(format!(
            "unknown: tobii games {other}\nusage:\n  \
             tobii games\n  \
             tobii games set KEY VALUE\n\
             {PROFILE_USAGE}"
        )
        .into()),
    }
}

// ----------------------------------------------------- tobii games profile

// A per-game profile holds three unequal things, and every sentence these
// commands print exists to keep them from being read as one:
//
//   * this program's own settings, which it reads and writes, and can
//     therefore capture and put back;
//   * whether the game needs the Wine bridge, which is a claim about the game
//     that somebody has to have established — installing the bridge does not
//     establish it;
//   * what the game's own configuration files should say, which this program
//     reads and never writes, and cannot invent.
//
// `save` can honestly capture only the first. That is not a gap to be papered
// over with a plausible-looking guess at the other two: a profile naming a
// check nobody verified would send a user to change a setting on this
// program's authority, and this program has none.

/// The libraries `libraryfolders.vdf` names that are not on this machine, as
/// a block to print — empty when there are none.
///
/// Steam records a library's path, not whether its drive is plugged in, so a
/// title on an unplugged external drive is installed and in no list here.
/// Every answer below that would otherwise read as the whole picture of what
/// is installed carries this, so "this game is not installed" is never printed
/// over a drive nobody could look in.
///
/// `pub(crate)` because `bridge.rs` words these same answers and calls this
/// rather than keeping its own copy. The crate root is the one module both
/// can see, and one sentence about somebody's unplugged drive is worth more
/// than two that can drift.
///
/// Takes the [`tobii_steam::Steam`] rather than a home, so this sentence and
/// the list it is printed beside come out of one walk of the disk. Asked of a
/// home, it took a walk of its own, and every caller here already had one.
pub(crate) fn steam_libraries_missing(steam: &tobii_steam::Steam) -> String {
    let missing = steam.missing_libraries();
    if missing.is_empty() {
        return String::new();
    }
    let mut block = if missing.len() == 1 {
        "libraryfolders.vdf names a Steam library this machine does not have, \
         so anything installed there is in no list here:"
            .to_string()
    } else {
        format!(
            "libraryfolders.vdf names {} Steam libraries this machine does not \
             have, so anything installed there is in no list here:",
            missing.len()
        )
    };
    for path in missing {
        block.push_str(&format!("\n  {}", path.display()));
    }
    block
}

/// `head`, then the missing-library block under it if there is one.
pub(crate) fn with_missing(head: String, missing: &str) -> String {
    if missing.is_empty() {
        head
    } else {
        format!("{head}\n{missing}")
    }
}

/// Whether what the user typed is an app id rather than a name fragment.
///
/// The same rule [`tobii_steam::resolve`] applies internally, and it has to
/// stay the same rule: this is asked only about a value `resolve` has already
/// answered [`tobii_steam::Match::None`] for, to tell *an app id for a game
/// that is not installed here* from *a name that matches nothing*.
fn is_app_id(wanted: &str) -> bool {
    !wanted.is_empty() && wanted.chars().all(|c| c.is_ascii_digit())
}

/// The app-id column every list and heading here pads to, so that the lines
/// of one answer stand under each other.
///
/// A width, not a limit: an app id wider than this takes the room it needs and
/// is still followed by a space. Ten digits is `u32`, which is what a Steam app
/// id is, so nothing that reaches these commands is wider —
/// [`steam_appid_for`] refuses the rest.
const APPID_COL: usize = 10;

/// One game the user named, resolved as far as this machine can resolve it.
#[derive(Debug)]
struct GameRef {
    /// What names the profile file. The whole of a game's identity here, as it
    /// is in `tobii_steam`.
    appid: String,
    /// What Steam calls it, when Steam has it. Display only.
    name: Option<String>,
    /// Whether Steam has a Proton prefix for this app id.
    ///
    /// The same question `bridge.rs` answers with the same call, and it is
    /// here because the two commands were giving one app id two answers: a
    /// title that has been uninstalled, and a non-Steam shortcut added to
    /// Steam, both keep a `compatdata/<appid>/pfx` that no `appmanifest_*.acf`
    /// mentions. `bridge status --steam <that id>` resolves the prefix and
    /// works; a heading built from the installed list alone called the same id
    /// "not installed on this machine".
    has_prefix: bool,
    /// [`steam_libraries_missing`] for the home this was resolved in, carried
    /// so the heading can never print a confident "not installed".
    missing_libraries: String,
}

impl GameRef {
    /// The line every answer about this game starts with.
    fn heading(&self) -> String {
        let id = format!("{:<APPID_COL$}", self.appid);
        match &self.name {
            Some(n) => format!("{id} {n}"),
            // Steam has no manifest for it, but Proton has a prefix: the two
            // outlive each other in both directions, so this is not a game
            // this machine has never seen. Which possibilities that leaves
            // depends on whether every library could be looked in — naming
            // only the two below over an unplugged drive would rule out the
            // likeliest answer of the three.
            None if self.has_prefix && self.missing_libraries.is_empty() => format!(
                "{id} (not in Steam's installed list, but it has a Proton prefix —\n  \
                 an uninstalled title, or a shortcut added to Steam)"
            ),
            None if self.has_prefix => with_missing(
                format!(
                    "{id} (not in the Steam libraries this machine has, but it has a\n  \
                     Proton prefix — an uninstalled title, a shortcut added to Steam,\n  \
                     or a title on the library below)"
                ),
                &self.missing_libraries,
            ),
            // Not installed, and a library that could not be looked in: two
            // different sentences, because only one of them is a negative this
            // program is entitled to.
            None if self.missing_libraries.is_empty() => {
                format!("{id} (not installed on this machine, and it has no Proton prefix)")
            }
            None => with_missing(
                format!("{id} (not in the Steam libraries this machine has)"),
                &self.missing_libraries,
            ),
        }
    }
}

/// The rule [`tobii_config::profiles::is_appid`] applies, as a sentence.
///
/// That refusal names no way out of itself — "not a Steam app id" does not say
/// what one looks like — and it is the only error here a user can hit by
/// typing a plausible number.
const APPID_RULE: &str = "an app id here is one to ten digits and does not begin with a zero: it \
                          names a file,\nand `0999.toml` beside `999.toml` would be two profiles \
                          for one game.";

/// Turn `<app id or name>` into a game, the way `--steam` does elsewhere.
///
/// The decision is [`tobii_steam::resolve`]'s and the wording is this
/// function's, exactly as in `bridge.rs`: a name is a case-insensitive
/// substring, an ambiguous one lists what it matched rather than picking, and
/// nothing matching says what was looked at.
///
/// One case `bridge.rs` has no use for: an app id that resolves to nothing is
/// *not* an error here. A profile outlives the install it was written for, and
/// `show`, `forget` and `apply` all have honest answers for a game that has
/// since been uninstalled. A *name* that matches nothing stays an error —
/// there is no app id to be had from it.
///
/// One case `bridge.rs` has no use for the other way: every caller of this is
/// a profile verb, and a profile is a file named after the app id, so an id
/// that [`tobii_config::profiles::is_appid`] will not name a file after is
/// refused here rather than carried. It is carried no further because every
/// answer downstream was a confident one about a game that cannot exist:
/// `show 0999` printed a heading saying it is not installed and then failed
/// with a second, different reason.
fn steam_appid_for(home: &std::path::Path, wanted: &str) -> Result<GameRef, String> {
    // Asked before `resolve`, which cannot answer it: an empty needle is a
    // substring of every name, so it matches everything — and on a machine
    // with exactly one application that is `Match::One`, indistinguishable
    // from a fragment that picked it out. `tobii games profile save ""` would
    // then write a profile for whatever that one application happened to be.
    if wanted.is_empty() {
        return Err("name a game: an app id, or part of its name".to_string());
    }
    // One walk of this machine's Steam install, for all three of the questions
    // below. Each used to take its own: `apps`, the missing-library sentence
    // and the prefix stat re-read every root's `libraryfolders.vdf`, so every
    // `tobii games profile` command read the same four files three times over.
    let steam = tobii_steam::Steam::at(home);
    let apps = steam.apps();
    let missing_libraries = steam_libraries_missing(&steam);
    let has_prefix = |appid: &str| steam.prefix(appid).is_some();
    match tobii_steam::resolve(&apps, wanted) {
        tobii_steam::Match::One(app) => Ok(GameRef {
            has_prefix: has_prefix(&app.appid),
            appid: app.appid,
            name: Some(app.name),
            missing_libraries,
        }),
        tobii_steam::Match::Many(hits) => {
            let list: Vec<String> = hits
                .iter()
                .map(|a| format!("  {:<10} {}", a.appid, a.name))
                .collect();
            Err(format!(
                "{wanted:?} matches more than one game; pass the app id:\n{}",
                list.join("\n")
            ))
        }
        tobii_steam::Match::None if is_app_id(wanted) => {
            if !tobii_config::profiles::is_appid(wanted) {
                return Err(format!(
                    "{}\n{APPID_RULE}",
                    tobii_config::profiles::LoadError::NotAnAppId(wanted.to_string())
                ));
            }
            Ok(GameRef {
                appid: wanted.to_string(),
                name: None,
                has_prefix: has_prefix(wanted),
                missing_libraries,
            })
        }
        tobii_steam::Match::None => {
            // "No installed Steam game matches" is a claim about every game
            // installed, and there may be a library here that could not be
            // looked in. So the sentence says what was looked at.
            let mut msg = if missing_libraries.is_empty() {
                format!("no installed Steam game matches {wanted:?}")
            } else {
                format!("nothing in the Steam libraries this machine has matches {wanted:?}")
            };
            if apps.is_empty() {
                msg.push_str(" (no Steam libraries found)");
            } else {
                msg.push_str("\ninstalled:");
                for a in &apps {
                    msg.push_str(&format!("\n  {:<10} {}", a.appid, a.name));
                }
            }
            Err(with_missing(msg, &missing_libraries))
        }
    }
}

/// This program's own settings as `(key, value)` text, exactly as its config
/// file spells them.
///
/// Read back out of `OutputConfig::to_toml` rather than listed here. The keys
/// are `OutputConfig::keys()`, which a test in `tobii-output` pins to exactly
/// what `to_toml` writes; a second list in this file would be a third place to
/// forget a new setting, which is the mistake that list already carries a
/// comment about. What this owes in return is
/// `captured_settings_are_every_key_and_rebuild_the_config`, below: the pairs
/// must be every advertised key, and must reconstruct the config they came
/// from.
///
/// It takes the config rather than the text, so no caller can point it at a
/// file somebody hand-edited: its input is always this program's own writer.
fn captured_settings(cfg: &tobii_output::games::OutputConfig) -> Vec<(String, String)> {
    let toml = cfg.to_toml();
    let mut out = Vec::new();
    let mut in_games = false;
    for line in toml.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_games = line == "[games]";
            continue;
        }
        if !in_games {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        // `to_toml` quotes a string and leaves a scalar bare. A profile's
        // settings are text on their way to `apply_key`, which is handed the
        // unquoted string in both cases — the same thing `from_toml` does.
        let value = value
            .strip_prefix('"')
            .and_then(|r| r.strip_suffix('"'))
            .unwrap_or(value);
        out.push((key.trim().to_string(), value.to_string()));
    }
    out
}

/// The `key = value` pairs in `settings` that `OutputConfig::apply_key` will
/// not take, formatted one per line.
///
/// A profile is a hand-edited file, so `enabeld = true` is a thing that
/// happens. `apply_key` is the one place that decides what a key means, and
/// `tobii-output` depends on `tobii-config`, so `tobii-config` cannot ask it —
/// which makes this the first place that can.
fn unusable_settings(settings: &[(String, String)]) -> Vec<String> {
    let mut probe = tobii_output::games::OutputConfig::default();
    settings
        .iter()
        .filter(|(k, v)| !probe.apply_key(k, v))
        .map(|(k, v)| format!("  {k} = {v:?}"))
        .collect()
}

/// How a check's `path` is joined onto a prefix, as both report lines say it.
///
/// Until 2026-09-28 both said *"under the Proton prefix"*, which is a
/// containment claim `path`'s three rules do not make: no leading `/`, no
/// `..`, no backslash keep the path **relative**, so a profile written on one
/// machine names the same thing on another. Relative is not contained — see
/// [`leaves_the_prefix`].
const PATH_IS_RELATIVE: &str = "relative to the Proton prefix";

/// The sentence a `path` through `dosdevices/` earns, or `""` for every other
/// path.
///
/// A Wine prefix maps the machine into itself under `dosdevices/`: `z:` is
/// `/`, and a Steam prefix adds an `s:` at the library root. So
/// `dosdevices/s:/steamapps/common/<game>/...` is relative, has no `..` and
/// no backslash — and names the Steam library, outside the prefix. That
/// spelling is the one this program's own refusal of an absolute `path`
/// recommends (`check_path`, `crates/tobii-config/src/profiles.rs`), so an
/// author who took the advice was told on the very next line that what they
/// had written was inside the prefix. Both lines that echo a check back say
/// this instead, from here, so the two cannot come to disagree.
///
/// Returned as lines rather than a block because the two report lines indent
/// their continuations by different amounts, and the caller owns its own
/// margin.
fn leaves_the_prefix(path: &str) -> &'static [&'static str] {
    if path.split('/').next() == Some("dosdevices") {
        &[
            "through dosdevices/, so it leaves the prefix: a drive letter there",
            "names the machine — s: the Steam library, z: the filesystem root.",
        ]
    } else {
        &[]
    }
}

/// What a profile leaves for a person to do, as a block to print under it.
///
/// Shared by `show` and `apply` so the two cannot come to differ about what
/// this program did not do.
fn left_for_you(p: &tobii_config::profiles::Profile, appid: &str) -> String {
    use tobii_config::profiles::Bridge;
    let mut s = String::new();
    match p.bridge {
        Bridge::Required => s.push_str(&format!(
            "\nThe Wine bridge: this game reads head tracking through it.\n  \
             tobii bridge install --steam {appid}\n"
        )),
        Bridge::NotNeeded => {
            s.push_str("\nThe Wine bridge: the profile says this game does not need it.\n")
        }
        // Absent is not "no". Nobody wrote it down, and this program has not
        // worked it out — the bridge being installed in the prefix would say
        // that somebody installed it, not that the game reads it.
        Bridge::Unstated => s.push_str(
            "\nThe Wine bridge: the profile does not say whether this game needs it,\n  \
             and nothing here has established it either way.\n",
        ),
    }
    let unknown = p.unknown_formats();
    if !unknown.is_empty() {
        // Counted over the checks, not over the names: two checks in one
        // unreadable format are two checks that cannot be looked up.
        let n = p.checks.iter().filter(|c| !c.format.is_known()).count();
        s.push_str(&format!(
            "\n{n} check{} name{} a file format this build cannot read: {}.\n  \
             A newer tobii-linux may know {}.\n",
            if n == 1 { "" } else { "s" },
            if n == 1 { "s" } else { "" },
            unknown.join(", "),
            if unknown.len() == 1 { "it" } else { "them" },
        ));
    }
    if p.checks.is_empty() {
        s.push_str(
            "\nThe game's own configuration: the profile names nothing to check.\n  \
             This program never writes a game's configuration files, so anything\n  \
             that has to change in them is yours to change — and nobody has\n  \
             written down what that is for this game.\n",
        );
        return s;
    }
    s.push_str(&format!(
        "\nThe game's own configuration — {} thing{} to check, BY HAND. This\n  \
         program has not read any of these files: it never writes a game's\n  \
         configuration, and it does not read one from here either.\n",
        p.checks.len(),
        if p.checks.len() == 1 { "" } else { "s" },
    ));
    for (i, c) in p.checks.iter().enumerate() {
        // Numbered because `tobii games profile check remove` takes a number,
        // and the list a person reads has to be the list they can point at.
        s.push_str(&format!(
            "\n  {}. {} ({}, {PATH_IS_RELATIVE})\n",
            i + 1,
            c.path,
            c.format.as_str(),
        ));
        for line in leaves_the_prefix(&c.path) {
            s.push_str(&format!("     {line}\n"));
        }
        s.push_str(&format!(
            "     {} should be {:?}\n     {}\n",
            c.setting, c.wants, c.tell
        ));
    }
    s
}

/// `tobii games profile show` with no game: every profile there is.
///
/// Reads nothing but the profiles directory — not Steam, not `$HOME`. A user
/// whose external drive is unplugged still gets the list of what they wrote.
fn profile_list(out: &mut String, dir: &std::path::Path, builtin: &[(&str, &str)]) {
    let listing = tobii_config::profiles::list_from(dir, builtin);
    if listing.profiles.is_empty() {
        out.push_str("no game profiles.\n");
    } else {
        // Measured, not assumed. A fixed 28 columns for the name put the
        // counts of "Stronghold Crusader: Definitive Edition" ten characters
        // right of everyone else's, which is worse than a wide table: the
        // column stops being one exactly on the row a reader is looking at.
        // Counted in characters rather than bytes — a game name is whatever
        // Steam wrote in it.
        let w_id = listing
            .profiles
            .iter()
            .map(|(id, _)| id.chars().count())
            .max()
            .unwrap_or(0)
            .max(APPID_COL);
        let w_name = listing
            .profiles
            .iter()
            .map(|(_, l)| l.profile.name.as_deref().unwrap_or("").chars().count())
            .max()
            .unwrap_or(0);
        for (appid, loaded) in &listing.profiles {
            let p = &loaded.profile;
            out.push_str(&format!(
                "{:<w_id$} {:<w_name$} {} setting{}, {} check{}\n",
                appid,
                p.name.as_deref().unwrap_or(""),
                p.settings.len(),
                if p.settings.len() == 1 { "" } else { "s" },
                p.checks.len(),
                if p.checks.len() == 1 { "" } else { "s" },
            ));
            // Under the name, not under the app id: the origin is a path, and
            // the app id column is the one thing on this line that is the same
            // width on every row.
            out.push_str(&format!("{:w_id$} {}\n", "", loaded.origin));
        }
    }
    out.push_str(&format!("\ndirectory: {}\n", dir.display()));
    // Read off the table rather than written into the sentence: on the day a
    // profile is compiled in, a hardcoded "none ship" would be a lie nothing
    // tests.
    if builtin.is_empty() {
        out.push_str("this build ships no profile for any game.\n");
    }
    // A name in the directory that is not a profile is not a broken profile,
    // and `tobii uninstall --purge` will say the same of it. Separate
    // paragraphs, because folding them together is how one gets read as the
    // other.
    if !listing.problems.is_empty() {
        out.push_str("\ncould not be used:\n");
        for e in &listing.problems {
            out.push_str(&format!("  {e}\n"));
        }
    }
    if !listing.strays.is_empty() {
        out.push_str("\nin that directory and not a profile:\n");
        for s in &listing.strays {
            out.push_str(&format!("  {s}\n"));
        }
    }
    // Its own paragraph and not the one above, because it is the opposite
    // claim: a stray is somebody else's file, and this is one of ours. Saying
    // nothing at all about it was worse than either — `tobii uninstall
    // --purge` deletes these, so the listing and the uninstaller disagreed
    // about what is in the directory.
    if !listing.leftovers.is_empty() {
        out.push_str(
            "\nwritten by this program and not a profile — a save that was cut short\n\
             left it; `tobii uninstall --purge` removes it:\n",
        );
        for l in &listing.leftovers {
            out.push_str(&format!("  {l}\n"));
        }
    }
}

/// `tobii games profile show <game>`: what this program knows, and from where.
///
/// Knowledge, not measurement. Nothing here opens a file belonging to the
/// game, so nothing it prints may read as a result — see [`left_for_you`].
fn profile_show(
    out: &mut String,
    dir: &std::path::Path,
    builtin: &[(&str, &str)],
    game: &GameRef,
) -> Result<(), String> {
    use tobii_config::profiles;
    out.push_str(&format!("{}\n", game.heading()));
    let loaded = profiles::load_from(dir, builtin, &game.appid).map_err(|e| e.to_string())?;
    let Some(loaded) = loaded else {
        out.push_str(&format!(
            "\nno profile: nothing here knows anything about this game.\n\
             looked in: {}\n",
            profiles::path_in(dir, &game.appid).display()
        ));
        if builtin.is_empty() {
            out.push_str(
                "this build ships no profile for any game, so that is the whole answer.\n",
            );
        }
        return Ok(());
    };
    out.push_str(&format!("profile: {}\n", loaded.origin));
    let p = &loaded.profile;
    if let Some(n) = &p.name {
        out.push_str(&format!("the profile calls it: {n}\n"));
    }
    if p.settings.is_empty() {
        out.push_str("\nthis program's own settings: the profile sets none.\n");
    } else {
        out.push_str(&format!(
            "\nthis program's own settings, {} of them, to put into effect with\n  \
             tobii games profile apply {}\n",
            p.settings.len(),
            game.appid
        ));
        for (k, v) in &p.settings {
            out.push_str(&format!("  {k} = {v}\n"));
        }
        let bad = unusable_settings(&p.settings);
        if !bad.is_empty() {
            out.push_str(&format!(
                "\n{} of those would be refused — a key this program does not have,\n  \
                 or a value it cannot read. `apply` refuses the whole profile\n  \
                 rather than half of it:\n{}\n",
                bad.len(),
                bad.join("\n")
            ));
        }
    }
    out.push_str(&left_for_you(p, &game.appid));
    Ok(())
}

/// The refusal every command that writes a profile owes a profile it could not
/// read — and, for the one error that is not about a file, the refusal it does
/// not owe.
///
/// "Somebody wrote it: fix it, or move it aside" is a sentence about a file
/// that exists. [`profiles::LoadError::NotAnAppId`] is returned when the app id
/// could not name a file at all, so nothing was read, nothing is there, and
/// nobody wrote it — printing the same paragraph sent a user looking for a
/// `0999.toml` that has never existed on any machine.
fn refusing_to_overwrite(e: tobii_config::profiles::LoadError) -> String {
    use tobii_config::profiles::LoadError;
    match e {
        LoadError::NotAnAppId(_) => format!("{e}\n{APPID_RULE}\nNothing was written."),
        LoadError::Unreadable { .. } | LoadError::Malformed { .. } => format!(
            "{e}\nrefusing to overwrite a profile this program cannot read. Whatever\n  \
             it says, somebody wrote it: fix it, or move it aside, then run this\n  \
             again."
        ),
    }
}

/// Write `p` as the profile for `appid`, and hand back what the writer could
/// not carry across, as a paragraph to print.
///
/// [`tobii_config::profiles::save_to`] is the writer and the authority on both
/// halves: what the file ends up saying, and which comments it had nowhere to
/// put. Nothing here works either out for itself. A second copy of the
/// writer's rule in this crate is a second answer waiting to disagree with it,
/// which is exactly how this command once listed six comments as destroyed, in
/// full, while leaving every one of them in the file.
///
/// Its refusals also read as what they are. `save_to` puts the path in front
/// of the writer's own sentence, so a caller that adds "could not write
/// <path>" prints the path twice and calls a deliberate refusal that wrote
/// nothing an I/O failure. Only a real I/O error gets that wrapper.
fn save_profile_to(
    dir: &std::path::Path,
    appid: &str,
    p: &tobii_config::profiles::Profile,
) -> Result<String, String> {
    use tobii_config::profiles;
    // Asked before a path is built, as `save_to` asks it: `path_in` would
    // happily make a name out of `../../anything`.
    if !profiles::is_appid(appid) {
        return Err(format!(
            "{}\n{APPID_RULE}\nNothing was written.",
            profiles::LoadError::NotAnAppId(appid.to_string())
        ));
    }
    let orphans = profiles::save_to(dir, appid, p).map_err(|e| match e.kind() {
        // The writer's refusal, already carrying the file it is about.
        std::io::ErrorKind::InvalidInput => e.to_string(),
        _ => format!(
            "could not write {}: {e}",
            profiles::path_in(dir, appid).display()
        ),
    })?;
    Ok(orphan_report(&orphans))
}

/// The comments a write could not put back, printed back at the person whose
/// work they are — empty when there are none.
///
/// In full, never counted: a count of somebody's sentences is not their
/// sentences, and the terminal is the only copy left once the file is written.
/// Each one says where it was and what it was about, because "a comment was
/// lost" is not enough to put it back with.
fn orphan_report(orphans: &[tobii_config::profiles::Orphan]) -> String {
    if orphans.is_empty() {
        return String::new();
    }
    let mut s = format!(
        "\nNOT kept: {} comment line{} the profile being written has nowhere to\n  \
         put. A profile is rebuilt from what was parsed out of it, and a comment\n  \
         belongs to what it sat beside. Here {} in full — put back what you\n  \
         still want:\n",
        orphans.len(),
        if orphans.len() == 1 { "" } else { "s" },
        if orphans.len() == 1 {
            "it is"
        } else {
            "they are"
        },
    );
    for o in orphans {
        s.push_str(&format!("    {o}\n"));
    }
    s
}

/// `tobii games profile save <game>`: capture what is honestly capturable.
///
/// This program's own settings, as they are this second, and nothing else.
/// Everything else a profile can hold is a claim about the *game* — whether it
/// reads the bridge, what its own configuration files should say — and there
/// is nowhere here to read such a claim from. So an existing profile's checks,
/// bridge line and name are carried across untouched rather than dropped: they
/// are somebody's work, and this command has no better version of them.
///
/// A profile that is there and cannot be read stops this outright. Overwriting
/// it would destroy hand-written checks and replace them with a file that has
/// none, and the user would never learn what the old one said.
fn profile_save(
    out: &mut String,
    dir: &std::path::Path,
    builtin: &[(&str, &str)],
    game: &GameRef,
    cfg: &tobii_output::games::OutputConfig,
) -> Result<(), String> {
    use tobii_config::profiles::{self, Bridge, Profile};
    let existing = profiles::load_from(dir, builtin, &game.appid).map_err(refusing_to_overwrite)?;
    let mut p = Profile {
        settings: captured_settings(cfg),
        ..Profile::default()
    };
    if let Some(l) = &existing {
        p.name = l.profile.name.clone();
        p.bridge = l.profile.bridge;
        p.checks = l.profile.checks.clone();
    }
    // Steam's name only where the profile has none of its own: a name in the
    // file is somebody's choice, and a save is not the moment to overrule it.
    if p.name.is_none() {
        p.name = game.name.clone();
    }
    let path = profiles::path_in(dir, &game.appid);
    let lost = save_profile_to(dir, &game.appid, &p)?;
    out.push_str(&format!("wrote {}\n", path.display()));
    out.push_str(&format!(
        "\ncaptured: {} settings — every setting this program has, as it stands\n  \
         right now. Not a judgement about this game: it is this machine's\n  \
         current configuration, written down under this game's app id.\n",
        p.settings.len()
    ));
    if p.settings
        .iter()
        .any(|(k, v)| k == "enabled" && v == "false")
    {
        // The order that gets typed is "set the game up, then save the
        // profile", and this is the step of it that bites: `save` writes down
        // whatever is on disk, and `apply` puts it back. Captured with game
        // output off, this profile turns game output off.
        out.push_str(
            "\n  enabled = false — game output is OFF in this machine's settings right\n  \
             now, so that is what went into the profile, and `apply` will turn it\n  \
             off again. If this was meant to be a setup that works, run\n    \
             tobii games set enabled true\n  and save again.\n",
        );
    }
    if let Some(l) = &existing {
        out.push_str(&format!(
            "\nkept from the profile that was already there: {} check{}, the bridge\n  \
             line, and the name. Nothing here knows better than they do.\n",
            p.checks.len(),
            if p.checks.len() == 1 { "" } else { "s" },
        ));
        // Which profile was already there is the one thing about it worth
        // repeating, and only when the answer is surprising: the path is on
        // the `wrote` line above. A built-in profile has just been superseded
        // by a file, and nothing else in this report would say so.
        if l.origin == profiles::Origin::Builtin {
            out.push_str(
                "  They came from the profile compiled into tobii-linux, which this\n  \
                 file now replaces whole.\n",
            );
        }
    }
    out.push_str(&lost);
    if p.bridge == Bridge::Unstated {
        out.push_str(
            "\nnot captured: whether this game needs the Wine bridge. Nothing here\n  \
             can find that out — the bridge being installed in the prefix would\n  \
             say somebody installed it, not that the game reads it. Write\n  \
             `bridge = true` or `bridge = false` in the file once you know.\n",
        );
    }
    if p.checks.is_empty() {
        // The command, not the file format. "Add [[check]] blocks by hand"
        // sent a user to an editor in the round that gave them a command for
        // it, and `[[check]]` is a word they have nowhere else met.
        out.push_str(&format!(
            "\nnot captured: anything about this game's own configuration files.\n  \
             This program never writes them, and it cannot invent what they\n  \
             should say. Once you have established one, write it down:\n    \
             tobii games profile check add {} --format <format> --path <path>\n      \
             --setting <name> --wants <value> --tell \"<what to do about it>\"\n  \
             `tobii games profile check` on its own says what each of those is.\n",
            game.appid
        ));
    }
    Ok(())
}

/// `tobii games profile forget <game>`: remove the user's file, naming it.
///
/// Only ever the user's own file. A compiled-in profile is part of the
/// program, and answering "removed" for one would be a claim about a file that
/// was never there.
fn profile_forget(
    out: &mut String,
    dir: &std::path::Path,
    builtin: &[(&str, &str)],
    appid: &str,
) -> Result<(), String> {
    use tobii_config::profiles;
    if !profiles::is_appid(appid) {
        return Err(profiles::LoadError::NotAnAppId(appid.to_string()).to_string());
    }
    let compiled_in = builtin.iter().any(|(id, _)| *id == appid);
    let path = profiles::path_in(dir, appid);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            out.push_str(&format!("removed {}\n", path.display()));
            if compiled_in {
                out.push_str(
                    "a profile for this game is compiled into tobii-linux, and that is\n  \
                     what will be used from now on.\n",
                );
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            out.push_str(&format!(
                "nothing to remove: there is no file at {}.\n",
                path.display()
            ));
            if compiled_in {
                out.push_str(
                    "This game's profile is compiled into tobii-linux, which is part of\n  \
                     the program and not a file `forget` can take away.\n",
                );
            }
            Ok(())
        }
        Err(e) => Err(format!("could not remove {}: {e}", path.display())),
    }
}

/// `tobii games profile apply <game>`: put the profile's settings into effect.
///
/// It earns its place because it is the other half of `save`: settings
/// captured and never restorable would make `save` a diary. What it must not
/// do is sound like more than it is, so three things are true of it by
/// construction.
///
/// It applies **this program's** settings, which are global. A profile is per
/// game; `games.toml` is not. Applying one game's profile sets this program up
/// the way that game wants it, and the next game's profile will overwrite
/// that. The report says so rather than leaving it to be discovered.
///
/// It is **all or nothing**. A pair `apply_key` refuses stops the whole
/// profile and writes nothing. Applying the half it understood is exactly how
/// somebody ends up told to change a setting a newer profile had already
/// marked as not needed.
///
/// It **touches no file belonging to the game**, and it does not install the
/// bridge. Both are named as things still to do, with the command for the one
/// that has a command.
///
/// Returns whether `cfg` changed and is worth saving: a profile with no
/// settings must not be the reason a `games.toml` appears on disk.
fn profile_apply(
    out: &mut String,
    dir: &std::path::Path,
    builtin: &[(&str, &str)],
    game: &GameRef,
    cfg: &mut tobii_output::games::OutputConfig,
) -> Result<bool, String> {
    use tobii_config::profiles;
    out.push_str(&format!("{}\n", game.heading()));
    let loaded = profiles::load_from(dir, builtin, &game.appid).map_err(|e| e.to_string())?;
    let Some(loaded) = loaded else {
        return Err(format!(
            "no profile for {}, so there is nothing to apply.\nlooked in: {}",
            game.appid,
            profiles::path_in(dir, &game.appid).display()
        ));
    };
    out.push_str(&format!("profile: {}\n", loaded.origin));
    let p = &loaded.profile;
    let bad = unusable_settings(&p.settings);
    if !bad.is_empty() {
        return Err(format!(
            "{}\nhas {} setting{} this program cannot use:\n{}\nvalid keys: {}\n\
             Nothing was applied: half a profile is worse than none of it.",
            loaded.origin,
            bad.len(),
            if bad.len() == 1 { "" } else { "s" },
            bad.join("\n"),
            tobii_output::games::OutputConfig::keys().join(", ")
        ));
    }
    let changed = !p.settings.is_empty();
    if changed {
        for (k, v) in &p.settings {
            if !cfg.apply_key(k, v) {
                // Unreachable: `unusable_settings` put every pair through this
                // same function a moment ago and found none it refused. If
                // that ever stops being true, stop here — the caller saves
                // nothing on an error, so a config half-changed by a bug never
                // reaches the disk.
                return Err(format!(
                    "{k} = {v:?} was refused after being accepted a moment earlier; \
                     nothing was written"
                ));
            }
        }
        out.push_str(&format!(
            "\napplied {} setting{} to {}.\n  \
             These are this program's settings and there is one set of them: they\n  \
             are now what every game gets, not only this one.\n",
            p.settings.len(),
            if p.settings.len() == 1 { "" } else { "s" },
            tobii_output::games::games_path().display()
        ));
    } else {
        out.push_str(
            "\nthis profile sets none of this program's settings, so nothing was\n  \
             applied and nothing was written.\n",
        );
    }
    out.push_str("\nstill to do, and not done by this command:\n");
    out.push_str(&left_for_you(p, &game.appid));
    Ok(changed)
}

// ----------------------------------------------- tobii games profile check
//
// The one place a person can write down what they learned by playing a game.
//
// A profile's `[settings]` this program can capture, because they are its own.
// Its `[[check]]` blocks it cannot: each one is a claim about a *game* — that
// this setting, in this file, should say this — and there is nothing here to
// read such a claim from. Until this existed, the only way to make one was to
// know the file format and open an editor, and the format was written down
// nowhere a user would look. That made the whole feature unusable for the
// thing it was built for: playing a game once and capturing what it took.
//
// So these commands take the claim, verbatim, from the person making it. They
// invent nothing: `add` will not read the game's file to suggest a value, and
// `where` will not turn "the file is there" into "the setting is right". What
// the program contributes is that the file it writes is one it can read back —
// every check goes through the profile parser before anything is written.

/// The `check` verbs, as usage lines.
const CHECK_USAGE: &str = "  \
     tobii games profile check where <app id or name>\n  \
     tobii games profile check add <app id or name> --format <format> --path <path>\n      \
     --setting <name> --wants <value> --tell \"<what to do about it>\"\n  \
     tobii games profile check remove <app id or name> <number>";

/// What a `[[check]]` is and what each of its keys means.
///
/// Printed by `check` with no verb, because this is the only place in the
/// program a user meets the profile format, and a schema nobody can find is a
/// schema nobody writes to.
const CHECK_SCHEMA: &str = "\
A check is one thing somebody verified about one game: a setting in the game's
own configuration, and what it should say. It never changes one — this program
does not write a game's files. It reads, and it reports.

Each check is a block in <profiles dir>/<app id>.toml, and looks like this:

  [[check]]
  format  = \"binds-dir\"
  path    = \"drive_c/users/steamuser/Options/Bindings\"
  setting = \"HeadlookMode\"
  wants   = \"1\"
  tell    = \"Set head look to toggle in the game's controls.\"

  format   which reader answers this, and so what `path` names:
             binds-dir        a directory of preset documents; `path` is the
                              directory. A StartPreset file there names the
                              live preset and that one is read; with none,
                              every preset is read and the answer says none
                              is in use, so one check can give many rows
             attributes-xml   one flat <Attributes> document; `path` is the file
  path     where that sits, written relative to the Proton prefix — the
           directory holding drive_c. Never starting with `/`, written with
           `/` and never `\\`, and with no `..` component.
  setting  an element name for binds-dir, an attribute name for attributes-xml
  wants    the value the game should have, spelled the way the game spells it
  tell     the sentence shown to whoever has to go and change it by hand

Those three rules on `path` keep it relative, which is what makes a profile
portable between machines whose prefixes sit in different places. They are
NOT a containment rule, and a check is not confined to the prefix. A Wine
prefix maps the machine into itself under dosdevices/, and those are ordinary
directory entries, so both of these are relative paths this accepts:

  dosdevices/s:/steamapps/common/<game>/...   the game's own installed files
                                              (s: is the Steam library root,
                                              and Steam is what puts it there)
  dosdevices/z:/<absolute path>               anything else you can read
                                              (z: is / in every Wine prefix)

That is deliberate — a game's shipped files are not inside its prefix — and
it is why a `path` beginning dosdevices/ is worth reading twice in a profile
somebody handed you. What bounds a check is not the path but the reader: it
opens the one file the path names, looks up the one `setting`, and answers
with that one value or with a refusal.

Every one of those is yours to assert. Nothing here opens a game's
configuration to fill one in, and nothing here knows what any game wants.";

/// One required `--flag VALUE` of `check add`.
///
/// A value that begins with `--` is refused rather than taken: the mistake
/// this catches is a flag whose value was left out, and `--wants --tell` would
/// otherwise be written into the profile as the string `--tell` and read back
/// out at somebody as the value their game should have.
fn check_flag<'a>(args: &'a [String], name: &str) -> Result<&'a str, String> {
    let v = flag_value(args, name)
        .ok_or_else(|| format!("{name} <value> is required.\nusage:\n{CHECK_USAGE}"))?;
    if v.starts_with("--") {
        return Err(format!(
            "{name} was given {v:?}, which is another flag: its value is missing.\nusage:\n\
             {CHECK_USAGE}"
        ));
    }
    Ok(v)
}

/// `tobii games profile check add <game> …`: write down one thing this person
/// verified.
///
/// Every field comes from the command line and none is guessed at. In
/// particular there is deliberately no "read the current value and offer it as
/// `--wants`": the current value is what the machine happens to have, and a
/// check says what the game *should* have. Those are the same number often
/// enough that filling it in would be right most of the time and wrong
/// silently, which is this project's whole catalogue of bugs in one feature.
///
/// The check is written only if the profile that results parses back — see
/// [`tobii_config::profiles::parse`], which is the authority on what a `path`
/// may be, rather than a second copy of that rule here.
fn profile_check_add(
    out: &mut String,
    dir: &std::path::Path,
    builtin: &[(&str, &str)],
    game: &GameRef,
    args: &[String],
) -> Result<(), String> {
    use tobii_config::profiles::{self, Check, Format};
    let format = check_flag(args, "--format")?;
    let path_arg = check_flag(args, "--path")?;
    let setting = check_flag(args, "--setting")?;
    let wants = check_flag(args, "--wants")?;
    let tell = check_flag(args, "--tell")?;
    if tell.trim().is_empty() {
        return Err(
            "--tell is the sentence shown to somebody who has to change the setting \
                    by hand.\nA check whose failure cannot be acted on is worse than no \
                    check: it says something\nis wrong and not what to do about it."
                .to_string(),
        );
    }
    if setting.trim().is_empty() {
        return Err(
            "--setting names the setting inside the file, so it cannot be blank.".to_string(),
        );
    }

    let existing = profiles::load_from(dir, builtin, &game.appid).map_err(refusing_to_overwrite)?;
    let mut draft = existing
        .as_ref()
        .map(|l| l.profile.clone())
        .unwrap_or_default();
    draft.checks.push(Check {
        // Carried as the user spelled it. Which `Format` that name *is* — one
        // of the two this build reads, or a name it does not know — is
        // `profiles`' decision and nobody else's, and it gets made below when
        // the file is read back: `Format::Unknown(s)` writes out as `s`, so a
        // name goes through the one function that maps names to formats
        // instead of through a second copy of the list here.
        format: Format::Unknown(format.to_string()),
        path: path_arg.to_string(),
        setting: setting.to_string(),
        wants: wants.to_string(),
        tell: tell.to_string(),
    });

    // Asked of the parser, not of a copy of its rules kept here. What a `path`
    // may be, what a string may hold and which keys a `[[check]]` needs are
    // decided in one place, and this is how a command on the other side of the
    // workspace gets the same answer — and how it becomes impossible for this
    // command to leave behind a profile the next read refuses.
    let path = profiles::path_in(dir, &game.appid);
    let p = profiles::parse(&draft.to_toml()).map_err(|e| {
        format!(
            "{}\nthat is what the profile reader would say about the check you gave,\n  \
             so nothing was written to {}.",
            e.message,
            path.display()
        )
    })?;

    // After the parse, so that `--format binds-dir` is compared against an
    // existing `binds-dir` as the same format and not as two different
    // strings. Two checks on one setting report it twice, and disagree the day
    // one of their `wants` is edited.
    let new = p.checks.last().expect("the check just pushed");
    if let Some(i) = p.checks[..p.checks.len() - 1]
        .iter()
        .position(|c| c.format == new.format && c.path == new.path && c.setting == new.setting)
    {
        return Err(format!(
            "check {} already reads {} out of {}, in the same format.\n\
             Remove it first, and nothing is written in the meantime:\n  \
             tobii games profile check remove {} {}",
            i + 1,
            new.setting,
            new.path,
            game.appid,
            i + 1,
        ));
    }

    let lost = save_profile_to(dir, &game.appid, &p)?;
    let n = p.checks.len();
    out.push_str(&format!("wrote {}\n", path.display()));
    if existing.is_none() {
        out.push_str(
            "\nThere was no profile for this game, so that file is new and holds this\n  \
             check and nothing else. `tobii games profile save` adds this program's\n  \
             own settings to it.\n",
        );
    }
    let c = &p.checks[n - 1];
    out.push_str(&format!(
        "\ncheck {n}, as it will be read back:\n  {} ({}, {PATH_IS_RELATIVE})\n",
        c.path,
        c.format.as_str(),
    ));
    for line in leaves_the_prefix(&c.path) {
        out.push_str(&format!("    {line}\n"));
    }
    out.push_str(&format!(
        "    {} should be {:?}\n    {}\n",
        c.setting, c.wants, c.tell,
    ));
    if !c.format.is_known() {
        out.push_str(&format!(
            "\n  {:?} is not a format this build has a reader for, so nothing here will\n  \
             ever look this check up. It is kept exactly as you spelled it — a newer\n  \
             tobii-linux may know it — but if it was meant to be one of the two this\n  \
             build reads, it is a typo: binds-dir, attributes-xml.\n",
            c.format.as_str()
        ));
    }
    out.push_str(
        "\nNothing here verified any of that. This program did not open the game's\n  \
         configuration and could not have told you what the setting should be: the\n  \
         claim is yours, and the file is the record that you made it. A `#` line in\n  \
         the file is where to date it and say how you know.\n",
    );
    out.push_str(&lost);
    out.push_str(&format!(
        "\nsee where it looks on this machine:\n  \
         tobii games profile check where {}\n",
        game.appid
    ));
    Ok(())
}

/// `tobii games profile check remove <game> <n>`: take one back out.
///
/// It prints what it removed in full, because the number is not the check and
/// a person who removed the wrong one needs the text back, not a count.
fn profile_check_remove(
    out: &mut String,
    dir: &std::path::Path,
    builtin: &[(&str, &str)],
    game: &GameRef,
    which: Option<&str>,
) -> Result<(), String> {
    use tobii_config::profiles;
    let which = which.ok_or_else(|| {
        format!(
            "which check? `tobii games profile show {}` numbers them.\nusage:\n{CHECK_USAGE}",
            game.appid
        )
    })?;
    let n: usize = which
        .parse()
        .ok()
        .filter(|n| *n >= 1)
        .ok_or_else(|| format!("{which:?} is not a check number; they start at 1."))?;

    let loaded = profiles::load_from(dir, builtin, &game.appid).map_err(refusing_to_overwrite)?;
    let path = profiles::path_in(dir, &game.appid);
    let Some(loaded) = loaded else {
        return Err(format!(
            "no profile for {}, so it has no checks to remove.\nlooked in: {}",
            game.appid,
            path.display()
        ));
    };
    let mut p = loaded.profile.clone();
    if n > p.checks.len() {
        return Err(match p.checks.len() {
            0 => format!("{} has a profile, but it names no checks.", game.appid),
            k => format!("there is no check {n}: this profile has {k}, numbered 1 to {k}."),
        });
    }
    let gone = p.checks.remove(n - 1);
    let lost = save_profile_to(dir, &game.appid, &p)?;
    out.push_str(&format!("wrote {}\n", path.display()));
    out.push_str(&format!(
        "\nremoved check {n}. It said:\n  \
         --format {} --path {:?} --setting {:?} --wants {:?} --tell {:?}\n  \
         That line puts it back.\n",
        gone.format.as_str(),
        gone.path,
        gone.setting,
        gone.wants,
        gone.tell,
    ));
    if loaded.origin == profiles::Origin::Builtin {
        out.push_str(
            "\nThe profile it came out of is the one compiled into tobii-linux; what\n  \
             was just written is a file of your own, which replaces it whole.\n",
        );
    }
    out.push_str(&format!(
        "\n{} check{} left.\n",
        p.checks.len(),
        if p.checks.len() == 1 { "" } else { "s" },
    ));
    out.push_str(&lost);
    Ok(())
}

/// `tobii games profile check where <game>`: where each check looks on this
/// machine, and whether anything is there.
///
/// The half of "run the checks" this program can do, and it says which half
/// that is. It resolves the Proton prefix exactly as `tobii bridge` does,
/// joins each check's path onto it, and reports whether that path exists and
/// is the kind of thing the format needs — a directory for `binds-dir`, a file
/// for `attributes-xml`. That catches the mistake an author actually makes,
/// which is a path that names nothing.
///
/// What it does **not** do is open any of them and read the setting out. The
/// reader for these formats is `tobii-gameconf`, which this binary does not
/// carry; the hub's game-setup window does, and reads them there. A command
/// that printed "HeadlookMode is 0" without a reader would be inventing it,
/// which is the failure this whole feature is written against.
fn profile_check_where(
    out: &mut String,
    dir: &std::path::Path,
    builtin: &[(&str, &str)],
    game: &GameRef,
    home: &std::path::Path,
) -> Result<(), String> {
    use tobii_config::profiles::{self, Format};
    out.push_str(&format!("{}\n", game.heading()));
    let path = profiles::path_in(dir, &game.appid);
    let loaded = profiles::load_from(dir, builtin, &game.appid).map_err(|e| e.to_string())?;
    let Some(loaded) = loaded else {
        return Err(format!(
            "no profile for {}, so there are no checks to place.\nlooked in: {}",
            game.appid,
            path.display()
        ));
    };
    out.push_str(&format!("profile: {}\n", loaded.origin));
    let checks = &loaded.profile.checks;
    if checks.is_empty() {
        out.push_str(
            "\nthis profile names no checks. `tobii games profile check add` writes one.\n",
        );
        return Ok(());
    }
    let prefix = tobii_steam::prefix(home, &game.appid);
    match &prefix {
        Some(p) => out.push_str(&format!("prefix:  {}\n", p.display())),
        None => out.push_str(
            "prefix:  none. Proton makes one the first time the game runs, so until it\n  \
             has, every path below is a path with nothing to join it onto.\n",
        ),
    }
    for (i, c) in checks.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. {} ({})\n   {} should be {:?}\n",
            i + 1,
            c.path,
            c.format.as_str(),
            c.setting,
            c.wants,
        ));
        let Some(prefix) = &prefix else { continue };
        let full = c.path_under(prefix);
        out.push_str(&format!("   {}\n", full.display()));
        // `metadata`, so a symlink is reported as what it points at — which is
        // what the reader would open.
        let what = match std::fs::metadata(&full) {
            Ok(m) if m.is_dir() => "a directory",
            Ok(m) if m.is_file() => "a file",
            Ok(_) => "there, and neither a file nor a directory",
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                out.push_str("   nothing is there.\n");
                continue;
            }
            Err(e) => {
                out.push_str(&format!("   it cannot be looked at: {e}\n"));
                continue;
            }
        };
        let wanted = match c.format {
            Format::BindsDir => Some("a directory"),
            Format::AttributesXml => Some("a file"),
            Format::Unknown(_) => None,
        };
        match wanted {
            Some(w) if w == what => {
                out.push_str(&format!("   {what}, as {} needs.\n", c.format.as_str()))
            }
            Some(w) => out.push_str(&format!(
                "   {what}, but {} reads {w}. The path names the wrong thing.\n",
                c.format.as_str()
            )),
            None => out.push_str(&format!(
                "   {what}. This build has no reader for {:?}, so what it ought to be is\n   \
                 not something this can say.\n",
                c.format.as_str()
            )),
        }
    }
    out.push_str(
        "\nThis did not read any of those files. It found the paths; what a setting in\n  \
         one of them says is read by the hub's game-setup window, which carries the\n  \
         readers. Nothing here will tell you what a setting says.\n",
    );
    // The promise above is only as wide as the readers this build has, and
    // `check add` already refuses to make it for a format it does not know:
    // *"nothing here will ever look this check up"*. Closing with the wide
    // version told a user, minutes later, that the window reads a check the
    // window answers `UnknownFormat` about.
    let unknown = loaded.profile.unknown_formats();
    if !unknown.is_empty() {
        let n = checks.iter().filter(|c| !c.format.is_known()).count();
        let which = if n == checks.len() && n == 1 {
            "the one check above names".to_string()
        } else if n == checks.len() {
            format!("all {n} checks above name")
        } else if n == 1 {
            format!("1 of the {} checks above names", checks.len())
        } else {
            format!("{n} of the {} checks above name", checks.len())
        };
        out.push_str(&format!(
            "\n  Except that {which} a format this build has no reader for: {}.\n  \
             Nothing will ever look {} up — not this command, and not that window\n  \
             either, which has the readers this build has and no others. A newer\n  \
             tobii-linux may know {}.\n",
            unknown.join(", "),
            if n == 1 { "it" } else { "them" },
            if unknown.len() == 1 { "it" } else { "them" },
        ));
    }
    Ok(())
}

/// `tobii games profile check …` — dispatch.
fn profile_check_cmd(
    out: &mut String,
    dir: &std::path::Path,
    builtin: &[(&str, &str)],
    args: &[String],
) -> Result<(), String> {
    let verb = args.get(4).map(String::as_str);
    let wanted = args.get(5).map(String::as_str);
    match (verb, wanted) {
        (Some("add"), Some(w)) => steam_appid_for(&home_for_steam()?, w)
            .and_then(|g| profile_check_add(out, dir, builtin, &g, args)),
        (Some("remove"), Some(w)) => steam_appid_for(&home_for_steam()?, w).and_then(|g| {
            profile_check_remove(out, dir, builtin, &g, args.get(6).map(String::as_str))
        }),
        (Some("where"), Some(w)) => {
            let home = home_for_steam()?;
            steam_appid_for(&home, w)
                .and_then(|g| profile_check_where(out, dir, builtin, &g, &home))
        }
        (Some(v @ ("add" | "remove" | "where")), None) => Err(format!(
            "name a game: an app id, or part of its name.\nusage:\n  \
             tobii games profile check {v} <app id or name> …\n{CHECK_USAGE}"
        )),
        // Not an error. This is the only readable description of a `[[check]]`
        // outside a Rust doc comment, and a user who typed the words to reach
        // it asked for it deliberately: answering on stderr, behind `error:`,
        // and exiting 1 told them that asking was the mistake.
        (None, _) => {
            out.push_str(&format!("{CHECK_SCHEMA}\n\nusage:\n{CHECK_USAGE}\n"));
            Ok(())
        }
        (Some(other), _) => Err(format!(
            "unknown: tobii games profile check {other}\nusage:\n{CHECK_USAGE}"
        )),
    }
}

/// `$HOME`, for finding Steam's libraries.
fn home_for_steam() -> Result<std::path::PathBuf, String> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "HOME is not set, so Steam's libraries cannot be found".to_string())
}

/// The `tobii games profile` verbs, as usage lines — without a `usage:`
/// header, because the one place that lists them under a wider usage would
/// then print the word twice.
const PROFILE_USAGE: &str = "  \
     tobii games profile show [<app id or name>]\n  \
     tobii games profile save <app id or name>\n  \
     tobii games profile apply <app id or name>\n  \
     tobii games profile forget <app id or name>\n  \
     tobii games profile check where|add|remove <app id or name> …\n      \
     (`tobii games profile check` on its own says what a check is)";

/// `tobii games profile …` — dispatch, and the only place here that reads the
/// real directories.
///
/// Every verb above takes the profiles directory, the built-in table and an
/// already-resolved game, so each is testable against a fixture directory
/// without a config, a Steam install or a `$HOME` — which CI, running as root
/// with somebody else's home, does not have.
fn profile_cmd(args: &[String]) -> CmdResult {
    use tobii_config::profiles;
    let verb = args.get(3).map(String::as_str);
    let wanted = args.get(4).map(String::as_str);
    let dir = profiles::profiles_dir();
    let mut out = String::new();
    let result = match (verb, wanted) {
        // `(None, _)` rather than `(None, None)`: there is no argv in which
        // the fifth word exists and the fourth does not, and the compiler has
        // no way to know that.
        (None, _) | (Some("show"), None) => {
            profile_list(&mut out, &dir, profiles::BUILTIN);
            Ok(())
        }
        (Some("show"), Some(w)) => steam_appid_for(&home_for_steam()?, w)
            .and_then(|g| profile_show(&mut out, &dir, profiles::BUILTIN, &g)),
        (Some("save"), Some(w)) => steam_appid_for(&home_for_steam()?, w).and_then(|g| {
            profile_save(
                &mut out,
                &dir,
                profiles::BUILTIN,
                &g,
                &tobii_output::games::load_output_config(),
            )
        }),
        (Some("forget"), Some(w)) => steam_appid_for(&home_for_steam()?, w)
            .and_then(|g| profile_forget(&mut out, &dir, profiles::BUILTIN, &g.appid)),
        // Takes the whole argv rather than a resolved game: its verbs are one
        // word further along, and `add` reads five flags off the end of it.
        (Some("check"), _) => profile_check_cmd(&mut out, &dir, profiles::BUILTIN, args),
        (Some("apply"), Some(w)) => steam_appid_for(&home_for_steam()?, w).and_then(|g| {
            let mut cfg = tobii_output::games::load_output_config();
            if profile_apply(&mut out, &dir, profiles::BUILTIN, &g, &mut cfg)? {
                tobii_output::games::save_output_config(&cfg).map_err(|e| e.to_string())?;
            }
            Ok(())
        }),
        (Some(v @ ("save" | "apply" | "forget")), None) => {
            Err(format!("usage: tobii games profile {v} <app id or name>"))
        }
        (Some(other), _) => Err(format!(
            "unknown: tobii games profile {other}\nusage:\n{PROFILE_USAGE}"
        )),
    };
    // Printed before the error is returned: `save` and `apply` both say what
    // they did above the part saying what they could not, and an error that
    // swallowed the first half would leave the less useful half on its own.
    print!("{out}");
    result.map_err(Into::into)
}

/// How long to wait for the hub to answer a lease request.
///
/// The hub notices the request within its 50 ms socket tick, but it cannot
/// answer until the device thread has actually dropped the USB session — and
/// that thread can be parked in a `read_notifications` that blocks for up to a
/// second. Three seconds is well clear of that and still short enough that a
/// hub which is never going to answer does not read as a hang.
const LEASE_WAIT: Duration = Duration::from_secs(3);

/// What the hub said when asked to let go of the tracker.
#[derive(Debug, PartialEq, Eq)]
enum StoodDown {
    /// The hub has dropped its USB session; the device can be opened.
    Yes,
    /// The hub will not let go, and says why.
    No(String),
}

/// Pick the lease reply out of one message the hub sent.
///
/// Anything that is not a `LeaseReply` is skipped rather than counted as an
/// answer, and that is load-bearing rather than defensive: granting the lease
/// is exactly what puts the hub into standby, and a status change is broadcast
/// to every connected client — so the first message to arrive after the request
/// is quite often the hub saying "nothing is asking for the tracker" rather
/// than the reply. Treating that as no answer would fall through to an open
/// that races the hub's own release.
fn lease_answer(msg: &tobii_ipc::Msg) -> Option<StoodDown> {
    match msg {
        tobii_ipc::Msg::LeaseReply { ok: true, .. } => Some(StoodDown::Yes),
        tobii_ipc::Msg::LeaseReply { ok: false, text } => Some(StoodDown::No(text.clone())),
        _ => None,
    }
}

/// The hub standing down, for as long as this lives.
struct HubLease {
    /// `None` when there was no hub to ask, or it never answered. The device is
    /// opened either way — see [`lease_the_tracker`].
    client: Option<tobii_ipc::Client>,
}

impl Drop for HubLease {
    /// Give the tracker back.
    ///
    /// The hub takes a lease back from a client whose socket has gone anyway —
    /// a dead socket is the truth and needs no timeout — so for a command that
    /// holds the device until Ctrl-C this is the polite half rather than the
    /// necessary one. It is here because the necessary half stops being enough
    /// the moment anything holds one of these for less than the whole process:
    /// `--check` and `--calibrate-pitch` both return, and the hub should have
    /// its tracker back at that point rather than at exit.
    fn drop(&mut self) {
        if let Some(c) = self.client.as_mut() {
            let _ = c.send(&tobii_ipc::Msg::Lease(tobii_ipc::LeaseAction::Release));
        }
    }
}

/// Ask a running hub to let go of the tracker, for as long as the returned
/// value lives.
///
/// # Why this is not simply `UsbTransport::open`
///
/// libusb claims interface 0 exclusively, so exactly one process has the
/// tracker. The hub takes it whenever something in it asks — and two of those
/// asks never end by themselves: the virtual joystick's, and `keep_awake`.
/// Both belong to a user who has turned game output on, which is the same user
/// this command is for, so without this `tobii headpose` (and with it
/// `--check` and `--calibrate-pitch`, the documented way to tell a gaze fault
/// from a head-tracking one) could not be run at all while the hub was open.
/// The hub already has the mechanism for handing the whole device over and
/// already honours it; until now nothing asked.
///
/// # The three answers, and why each is what it is
///
/// * **No hub.** Connecting fails, which is the ordinary state for anyone who
///   has not opened one. Nothing else holds the device, so this opens it
///   directly: that is the standalone route and it has to keep working — a
///   head-tracking command that needed a GUI running would be a worse program
///   than the one that could not share.
/// * **The hub refuses.** It is mid-calibration or mid-display-setup, or
///   another client holds the lease already. All three mean something else has
///   the device *and* is in a stateful conversation with it that handing it
///   over would break, so this fails — with the hub's own sentence, which names
///   what to wait for. Trying anyway would produce `DeviceBusy`, which names
///   nothing.
/// * **The hub does not answer.** Open it anyway. A hub that cannot answer
///   within [`LEASE_WAIT`] is either wedged or too old to know the question,
///   and in the commonest form of the second case — a hub sitting in standby —
///   it holds no USB session at all and the open simply succeeds. Refusing here
///   would make this command *less* usable than it was before it learned to
///   ask.
fn lease_the_tracker() -> Result<HubLease, Box<dyn std::error::Error>> {
    lease_from(&tobii_ipc::socket_path())
}

/// [`lease_the_tracker`], against an explicit socket.
///
/// Split out for the same reason `outputs::PortWatch::poll` takes its inputs as
/// arguments: the interesting behaviour is the conversation — ask, ignore the
/// status broadcast that the grant itself causes, act on the reply — and a test
/// can drive all of it against a socket of its own, while the real path keeps
/// using the one the hub binds.
fn lease_from(path: &std::path::Path) -> Result<HubLease, Box<dyn std::error::Error>> {
    // Subscribed to nothing, deliberately. This wants the device, not the hub's
    // frames, and the hub reads an empty subscription as "not a reason to run
    // the tracker" — so asking for poses here would light the illuminators for
    // a client that is about to take the device away.
    let mut client = match tobii_ipc::Client::connect_at(path, 0, "tobii headpose") {
        Ok(c) => c,
        Err(_) => return Ok(HubLease { client: None }),
    };
    if client
        .send(&tobii_ipc::Msg::Lease(tobii_ipc::LeaseAction::Acquire))
        .is_err()
    {
        // The hub went away between the connect and the request: the same
        // situation as there never having been one.
        return Ok(HubLease { client: None });
    }
    let deadline = Instant::now() + LEASE_WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match client.recv_timeout(left).as_ref().and_then(lease_answer) {
            Some(StoodDown::Yes) => {
                eprintln!("the hub has let go of the tracker for this run");
                return Ok(HubLease {
                    client: Some(client),
                });
            }
            Some(StoodDown::No(why)) => return Err(why.into()),
            // Something else, or nothing. A hub that has gone away will never
            // answer, and waiting out the deadline for it would add three
            // seconds to a case that is already decided.
            None => {
                if !client.is_connected() {
                    break;
                }
            }
        }
    }
    eprintln!(
        "the hub did not answer the request for the tracker within {}s; opening it anyway",
        LEASE_WAIT.as_secs()
    );
    Ok(HubLease { client: None })
}

fn headpose(args: &[String]) -> CmdResult {
    // Settings come from games.toml; the flags override this run without
    // writing anything. That order matters: a user who has configured Extended
    // View and a bridge port should get them from `tobii headpose` too, not
    // only from the hub — this command used to ignore the file entirely and
    // send a bare opentrack datagram.
    let mut cfg = tobii_output::games::load_output_config();
    if let Some(raw) = flag_value(args, "--udp") {
        // Parsed here rather than trusted: the same validation the flag had
        // before, so an unparseable address is still refused up front.
        cfg.opentrack = Some(parse_udp_addr(raw)?.to_string());
    } else if cfg.opentrack.is_none() {
        cfg.opentrack = Some(DEFAULT_UDP_ADDR.to_string());
    }
    if let Some(raw) = flag_value(args, "--rate") {
        cfg.rate_hz = parse_rate(raw)?;
    }
    // WHAT v0.1.0 SHIPPED STAYS WHAT THIS DOES BY DEFAULT.
    //
    // `OutputConfig::default()` has Extended View on and a bridge port set,
    // which is right for a machine somebody has configured for games — but with
    // no games.toml on disk those defaults apply to everyone, and `tobii
    // headpose` shipped in v0.1.0 as plain head pose to opentrack. Adopting the
    // file's defaults wholesale would have silently added gaze steering to a
    // running game and bound a second UDP socket for a bridge that is not even
    // merged yet.
    //
    // So the richer path is opt-in: the games master switch turned on in the
    // config, or `--extended-view` for one run. `--no-extended-view` still
    // forces it off, which is the quickest way to tell a gaze problem from a
    // head-tracking one.
    apply_games_opt_in(&mut cfg, args);
    let addr = parse_udp_addr(cfg.opentrack.as_deref().unwrap_or(DEFAULT_UDP_ADDR))?;
    let rate_hz = cfg.rate_hz;
    // `--check` prints the model's rotation beside the geometry's instead of the
    // normal status line. It is the experiment that settles the sign
    // conventions: turn your head one axis at a time and the two must move
    // TOGETHER. If one is inverted, that sign is wrong.
    let check = args.iter().any(|a| a == "--check");
    // `--recenter` takes a rotation reference for this run: the head angle the
    // user is holding becomes straight ahead. It is the same settle window the
    // hub's button and the socket's `Recentre` message ask for, because all
    // three ask the same `FramePipeline`.
    let recentre = wants_recentre(args);
    // `--calibrate-pitch [SECS]` measures the pitch zero instead of streaming:
    // sit square-on to the screen and hold still.
    let calibrate_pitch = args.iter().position(|a| a == "--calibrate-pitch").map(|i| {
        args.get(i + 1)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(10)
    });

    // Every sink the config asks for, throttled by the Router rather than by a
    // hand-rolled interval here. `OpentrackUdp` binds the ephemeral local port
    // this used to bind itself; a configured `bridge_port` adds the FreeTrack
    // bridge sink, which this command could not reach at all before.
    let mut router = tobii_output::Router::new(cfg.rate_hz);
    router.add(Box::new(tobii_output::sinks::OpentrackUdp::new(addr)?));
    if let Some(port) = cfg.bridge_port {
        router.add(Box::new(tobii_output::sinks::BridgeUdp::new(port)?));
    }
    if cfg.joystick {
        // Named and skipped, not fatal — the same policy the hub states in
        // `outputs::GameOutput::from_config`. This started out as `?`, with a
        // comment arguing that a foreground command should fail loudly; that
        // was wrong, and in a way worth recording. `/dev/uinput` is root-only
        // on most distributions, so `?` here means that on any machine without
        // the udev rule, a config with `enabled = true` stops `tobii headpose`
        // from running at all — taking opentrack and the bridge down with it
        // over an optional third sink that neither of them needs.
        if tobii_output::sinks::uinput_joystick::already_present() {
            // Almost always the hub: it holds a device for as long as game
            // output is on, whether or not it currently has the tracker. A
            // second one under the same name would put two identical
            // controllers in the game's bind list with only one of them
            // moving — and while this command holds the device, the hub's is
            // the frozen one.
            eprintln!(
                "not presenting a virtual joystick: one with this name already \
                 exists — the hub owns it. Close the hub, or turn its game output \
                 off, to use this command's own joystick."
            );
        } else {
            match tobii_output::sinks::UinputJoystick::open() {
                Ok(mut s) => {
                    s.set_response(tobii_output::sinks::uinput_joystick::Response::from_config(
                        &cfg,
                    ));
                    router.add(Box::new(s));
                }
                Err(e) => eprintln!("not presenting a virtual joystick: {e}"),
            }
        }
    }

    let mut model = open_model(&model_choice(args));

    // Asked for here rather than at the top of the command, because a granted
    // lease puts the hub's tracker out: loading the model can take seconds, and
    // there is no reason for the illuminators to be dark through them. Held in
    // a binding that lives to the end of this function — including across the
    // early return into `calibrate_pitch_zero` — because dropping it is what
    // gives the device back.
    let _hub = lease_the_tracker()?;

    eprintln!("opening Tobii ET5...");
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    reapply_display_area(&mut conn);

    // The camera stream is only worth its bandwidth when something consumes it.
    if model.is_some() {
        conn.set_request_timeout(Duration::from_millis(500));
        if !conn.subscribe_stream(CAMERA_STREAM)? {
            eprintln!("the device refused the camera subscription; falling back to 5 DOF");
            model = None;
        }
    }

    if let Some(secs) = calibrate_pitch {
        return calibrate_pitch_zero(&mut conn, &mut model, secs);
    }

    eprintln!("sending head pose to {addr} at {rate_hz:.0} Hz (Ctrl-C to stop)");
    if cfg.extended_view.enabled {
        eprintln!("extended view is on — your gaze steers the view as well as your head");
    }
    if let Some(port) = cfg.bridge_port {
        eprintln!("also sending to the FreeTrack bridge on port {port}");
    }
    // From what the router actually holds, not from what the config asked for:
    // the joystick is the one sink here that can be configured on and still
    // fail to open, and announcing one that is not there sends the user looking
    // for it in their game.
    if router.sink_names().contains(&"joystick") {
        eprintln!(
            "also presenting a virtual joystick — bind its axes in any game that \
             has no head-tracking support"
        );
    }
    if model.is_none() {
        eprintln!(
            "note: pitch is always 0 — two eye positions cannot express it. \
             `tobii headpose --fetch-model` adds the model that can."
        );
    }
    if recentre {
        eprintln!(
            "recentring: sit the way you play, look at the centre of the screen and hold \
             still for a second — the head angle you are holding becomes straight ahead. \
             Pitch is not part of it; it has its own zero from `--calibrate-pitch`."
        );
    }

    // The compose sequence lives in tobii-output now, so this command and the
    // hub cannot disagree about when Extended View contributes, what a blink
    // does, or when the smoothing state is stale.
    let mut pipeline = tobii_output::pipeline::FramePipeline::new(&cfg);
    let corners = tobii_config::load().ok().flatten().map(|s| s.to_corners());
    let now = Instant::now();
    let mut last_status = now;
    // The most recent composed frame, for the status line. It replaces reading
    // the filter's internal state: what the status should report is what was
    // actually sent, which is the frame, not the smoother.
    let mut last_frame: Option<tobii_output::TrackingFrame> = None;
    // Whether the settle window has been started. Once only: the reference is
    // taken at the start of the run, and a second one would fight the first.
    let mut recentre_started = false;
    let mut samples_since_status = 0u32;
    let mut sends_since_status = 0u32;
    let mut frames_since_status = 0u32;
    // The model's pose is held between camera frames: the camera runs at ~33 Hz
    // and gaze at ~33 Hz, but they are not in lockstep, and dropping pitch to
    // zero on every gaze sample that arrived without a matching image would
    // shake the head in game. Held for at most `HEAD_POSE_MAX_AGE`.
    let mut last_model: Option<(ModelPose, Instant)> = None;

    loop {
        let notes = conn.read_notifications();
        for (op, payload) in notes.iter() {
            match *op {
                OP_GAZE_NOTIFY => {
                    let Some(sample) = tobii_protocol::gaze::GazeSample::decode(payload) else {
                        continue;
                    };
                    samples_since_status += 1;
                    let eyes = pose_from_sample(&sample);
                    let fresh = last_model
                        .as_ref()
                        .filter(|(_, at)| at.elapsed() < HEAD_POSE_MAX_AGE);
                    // The model's pose wins where there is one; the pipeline
                    // falls back to the geometric pose otherwise. Tracking loss
                    // stops the send rather than emitting a synthetic pose —
                    // `Router::offer` returns `TrackingLost` and writes nothing,
                    // because opentrack holding its last value is far less
                    // jarring in game than a snap to zero.
                    let at = Instant::now();
                    // Started on the first frame that actually produced a pose,
                    // not at startup: the window would otherwise spend part of
                    // its second on frames from before the tracker had found
                    // the user, and be refused for having too few poses in it.
                    if recentre
                        && !recentre_started
                        && last_frame.as_ref().is_some_and(|f| f.pose.is_some())
                    {
                        pipeline.begin_recentre(at);
                        recentre_started = true;
                    }
                    // The stamp goes with the pose, not just the pose: a
                    // rotation held across the gaze frames between two camera
                    // frames is ONE measurement, and a settle window that
                    // counted it once per frame would call a stalled model a
                    // second of perfect stillness. See
                    // `pipeline::SuppliedPose`.
                    let pose_in = fuse_pose(eyes, fresh.map(|(m, _)| m)).map(|pose| {
                        tobii_output::pipeline::SuppliedPose {
                            pose,
                            rotation_at: fresh.map_or(at, |(_, measured)| *measured),
                        }
                    });
                    let frame = pipeline.offer(&sample, pose_in, &cfg, corners, at);
                    // A second later, when the window closes — including when
                    // it refuses, which is the answer somebody sitting still is
                    // waiting for.
                    if let Some(outcome) = pipeline.take_recentre() {
                        eprintln!("{outcome}");
                    }
                    if matches!(router.offer(&frame, at), tobii_output::Emitted::Sent) {
                        sends_since_status += 1;
                    }
                    last_frame = Some(frame);
                }
                op if op == u32::from(CAMERA_STREAM) => {
                    frames_since_status += 1;
                    run_model(&mut model, payload, &mut last_model);
                }
                _ => {}
            }
        }

        if last_status.elapsed() >= STATUS_INTERVAL {
            let elapsed = last_status.elapsed().as_secs_f64();
            // The one-eye fallback rides on the end of the rates, so both this
            // command's status line and `--check`'s two-line form report it
            // without either growing a line. See `fallback_note`.
            let rates = format!(
                "{:.0} samples/s, {:.0} frames/s, {:.0} sent/s{}",
                f64::from(samples_since_status) / elapsed,
                f64::from(frames_since_status) / elapsed,
                f64::from(sends_since_status) / elapsed,
                fallback_note(pipeline.fallback_stats()),
            );
            if check {
                print_check_line(
                    last_frame.as_ref().and_then(|f| f.pose),
                    last_model.as_ref().map(|(m, _)| m),
                    &rates,
                );
            } else {
                match last_frame.as_ref().and_then(|f| f.pose) {
                    Some(p) => {
                        let pitch = match last_model {
                            Some(_) => format!("{:>6.1}°", p.pitch_deg),
                            None => "   n/a".to_string(),
                        };
                        eprintln!(
                            "pos=({:>7.1}, {:>7.1}, {:>7.1})mm  yaw={:>6.1}°  pitch={pitch}  \
                             roll={:>6.1}°   {rates}",
                            p.x_mm, p.y_mm, p.z_mm, p.yaw_deg, p.roll_deg,
                        )
                    }
                    None => eprintln!(
                        "NO HEAD DETECTED — both eyes must be in the trackbox  ({rates}, \
                         not sending)"
                    ),
                }
            }
            last_status = Instant::now();
            samples_since_status = 0;
            sends_since_status = 0;
            frames_since_status = 0;
        }
    }
}

fn print_corners(c: &DisplayCorners) {
    println!("  TL = ({:8.1}, {:8.1}, {:8.1})", c.tl[0], c.tl[1], c.tl[2]);
    println!("  TR = ({:8.1}, {:8.1}, {:8.1})", c.tr[0], c.tr[1], c.tr[2]);
    println!("  BL = ({:8.1}, {:8.1}, {:8.1})", c.bl[0], c.bl[1], c.bl[2]);
}

fn print_setup(s: &DisplaySetup) {
    println!(
        "  width={:.1}mm height={:.1}mm tilt={:.1}° offset=({:.1}, {:.1}, {:.1})mm",
        s.width_mm, s.height_mm, s.tilt_deg, s.offset_x_mm, s.offset_y_mm, s.offset_z_mm
    );
    // A plane carries no curvature, so `display get` (which derives the setup
    // from the device's three corners) always reports flat — only the saved
    // config knows the real radius.
    if s.curvature_radius_mm > 0.0 {
        println!(
            "  curve radius={:.0}mm (width above is the flat chord)",
            s.curvature_radius_mm
        );
    }
    print_coverage(s.width_mm);
}

/// State plainly whether this screen is wider than the tracker can cover.
///
/// The two limits can simply fail to overlap: a screen wide enough needs the
/// user further away than the tracker can still find them. When that happens no
/// software closes the gap, and saying so once is worth more than leaving it to
/// surface later as "the edges feel bad" — a symptom indistinguishable from a
/// dozen real bugs, and mistaken for several of them on this project.
fn print_coverage(width_mm: f64) {
    let c = tobii_config::tracking_coverage(width_mm);
    if c.usable_fraction >= 1.0 {
        println!("  gaze coverage: the whole screen is within reach of this tracker");
        return;
    }
    println!(
        "  gaze coverage: at best {:.0}% of the width, sitting {:.0}mm back",
        c.usable_fraction * 100.0,
        c.best_distance_mm
    );
    println!(
        "    the edges need {:.0}° of eye rotation; past ~{:.0}° this device's error \
         jumps and it starts returning no gaze at all.",
        c.edge_angle_deg,
        tobii_config::USABLE_GAZE_DEG
    );
    println!(
        "    {:.0}mm is already the far edge of its tracking volume, so the outer \
         {:.0}% cannot be fixed by moving — or by us.",
        tobii_config::TRACKING_FAR_MM,
        (1.0 - c.usable_fraction) * 100.0
    );
}

/// Apply corners to a connected device. Returns whether the device
/// acknowledged the set (a response frame arrived) vs. was sent without ack.
fn apply_to_device(
    t: UsbTransport,
    c: &DisplayCorners,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut conn = Connection::connect(t)?;
    Ok(conn.set_display_area(c)?)
}

fn report_applied(acked: bool) {
    if acked {
        println!("display area applied to device (acknowledged).");
    } else {
        println!("display area sent to device (no acknowledgement received).");
    }
}

fn display_get() -> CmdResult {
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    match conn.request(OP_GET_DISPLAY_AREA, &[])? {
        Some(payload) => {
            let corners = DisplayCorners::decode(&payload)
                .ok_or("could not decode the display-area response")?;
            println!("display area (tracker-space mm):");
            print_corners(&corners);
            println!("derived setup:");
            print_setup(&DisplaySetup::from_corners(&corners));
            Ok(())
        }
        None => Err("no display-area response from device".into()),
    }
}

fn display_set() -> CmdResult {
    let setup = tobii_config::load()?.ok_or("no saved config — run `tobii setup` first")?;
    let c = setup.to_corners();
    let acked = apply_to_device(UsbTransport::open()?, &c)?;
    print_corners(&c);
    report_applied(acked);
    Ok(())
}

/// Host-chosen stimulus points (normalized). Center then four corners, inset
/// from the edges. NOTE: headless — no dots are drawn, so this validates the
/// protocol, not gaze accuracy. For an accurate calibration use the GUI's
/// follow-the-dot flow (`tobii-gtk`), which shows the stimulus.
const CAL_POINTS: [(f64, f64); 5] = [(0.5, 0.5), (0.1, 0.1), (0.9, 0.1), (0.1, 0.9), (0.9, 0.9)];

fn calibrate(apply_saved: bool) -> CmdResult {
    if apply_saved {
        let (blob, _meta) = tobii_config::load_calibration()?
            .ok_or("no saved calibration — run `tobii calibrate` first")?;
        let transport = UsbTransport::open()?;
        let mut conn = Connection::connect(transport)?;
        conn.apply_calibration(&blob)?;
        println!("re-applied saved calibration ({} bytes).", blob.len());
        return Ok(());
    }

    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    eprintln!(
        "Protocol exercise only. No stimulus is drawn, so the points below are \
         positions nobody looked at — the result cannot be a usable calibration \
         and is deliberately NOT saved. Use `tobii-gtk` for a real one."
    );

    // start + clear before any point, stop after the compute. Without the
    // session the device acknowledges every point and discards it, and the
    // whole run reports success having changed nothing — which is exactly how
    // this driver shipped a calibration that never calibrated.
    conn.start_calibration()?;
    conn.clear_calibration()?;
    let before = conn.retrieve_calibration().map(|b| b.0.len()).unwrap_or(0);

    for (i, &(x, y)) in CAL_POINTS.iter().enumerate() {
        conn.add_calibration_point(x, y, tobii_protocol::calibration::CAL_EYE_BOTH)?;
        println!(
            "  point {}/{} at ({x:.2}, {y:.2}) sampled",
            i + 1,
            CAL_POINTS.len()
        );
    }

    // Time it: a real computation takes well over a second. Around 230 ms is
    // the signature of an op that accepted the request and did nothing.
    let t0 = Instant::now();
    let computed = conn.compute_and_apply_calibration();
    let elapsed = t0.elapsed();
    let stopped = conn.stop_calibration();
    computed?;
    println!("  compute took {:?}", elapsed);
    if elapsed < Duration::from_millis(500) {
        println!("  ^ suspiciously fast — a real computation takes over a second");
    }
    if let Err(e) = stopped {
        eprintln!("warning: calibration session may still be open ({e})");
    }

    let blob = conn.retrieve_calibration()?;
    println!(
        "  blob {} bytes (was {before}) — {}",
        blob.0.len(),
        if blob.0.len() == before {
            "UNCHANGED, so nothing was computed"
        } else {
            "changed"
        }
    );
    println!("nothing saved; this exercises the protocol, not your eyes.");
    Ok(())
}

fn prompt_f64(label: &str, default: f64) -> Result<f64, Box<dyn std::error::Error>> {
    loop {
        print!("{label} [{default}]: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            // EOF (e.g. piped input exhausted) — accept the default.
            return Ok(default);
        }
        let t = line.trim();
        if t.is_empty() {
            return Ok(default);
        }
        match t.parse::<f64>() {
            Ok(v) if v.is_finite() => return Ok(v),
            _ => eprintln!("  please enter a finite number (or press Enter for {default})"),
        }
    }
}

fn setup() -> CmdResult {
    println!("Tobii display setup — enter your monitor geometry.");
    println!("(millimetres; tilt in degrees; press Enter to accept each default)\n");

    let (mut w_def, mut h_def) = (600.0, 340.0);
    let monitors = tobii_config::detect_monitors();
    if let Some(m) = tobii_config::pick_monitor(&monitors) {
        println!(
            "detected monitor: {} ({:.0} x {:.0} mm)",
            m.model, m.width_mm, m.height_mm
        );
        w_def = tobii_config::plane_width_from_edid(m.width_mm);
        h_def = m.height_mm;
    }

    let s = DisplaySetup {
        width_mm: prompt_f64("Monitor active-area WIDTH (mm)", w_def)?,
        height_mm: prompt_f64("Monitor active-area HEIGHT (mm)", h_def)?,
        tilt_deg: prompt_f64("Screen tilt back from vertical (deg)", 20.0)?,
        offset_y_mm: prompt_f64("Height of screen BOTTOM edge above tracker (mm)", 10.0)?,
        offset_z_mm: prompt_f64("Depth of screen bottom from tracker (mm)", 0.0)?,
        offset_x_mm: prompt_f64("Horizontal offset of screen centre from tracker (mm)", 0.0)?,
        curvature_radius_mm: prompt_f64("Screen curve radius (mm; 1800 for 1800R, 0 = flat)", 0.0)?,
    };
    let c = s.to_corners();
    println!("\ncomputed display-area corners (tracker-space mm):");
    print_corners(&c);

    let path = tobii_config::config_path();
    tobii_config::save(&s)?;
    // BOTH files, as the GUI's own setup flow writes both. The hub's screen
    // card names the monitor from this id, so a CLI setup that wrote only the
    // geometry left the card saying it could not name the monitor — on a
    // machine whose monitor is perfectly nameable, for every session after.
    // A monitor with no usable EDID id saves `None`, which is the same thing
    // the flow saves and which the card has its own sentence for.
    let _ = tobii_config::save_setup_monitor_id(
        tobii_config::pick_monitor(&monitors).and_then(|m| m.id.as_deref()),
    );
    println!("saved config to {}", path.display());

    match UsbTransport::open() {
        Ok(t) => match apply_to_device(t, &c) {
            Ok(acked) => report_applied(acked),
            Err(e) => eprintln!(
                "note: config saved, but applying to the device failed ({e}); run `tobii display set` to retry."
            ),
        },
        Err(e) => eprintln!(
            "note: device not opened ({e}); config saved — run `tobii display set` when connected."
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // --- the lease ------------------------------------------------------
    //
    // `tobii headpose` and a running hub cannot both hold the tracker, and the
    // hub's standby claims (the virtual joystick's and `keep_awake`) never end
    // by themselves. The hub has always honoured a lease; until these, nothing
    // in the shipped tree ever asked for one.

    /// A hub stand-in that answers one lease request with `reply`.
    ///
    /// A `Status` is sent first, always, because the real hub sends one:
    /// granting the lease is what puts it into standby, and the status change
    /// is broadcast to every client. The reply is therefore NOT the first
    /// message to arrive, and a client that assumed it was would open the
    /// device while the hub was still letting go.
    fn fake_hub(tag: &str, reply: tobii_ipc::Msg) -> (std::path::PathBuf, HubThread) {
        let path = std::env::temp_dir().join(format!("tobii-lease-{tag}-{}.sock", unique()));
        let _ = std::fs::remove_file(&path);
        let server = tobii_ipc::Server::bind_at(&path).expect("bind the stand-in hub");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_stop = std::sync::Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline
                && !thread_stop.load(std::sync::atomic::Ordering::Relaxed)
            {
                for msg in server.poll() {
                    if matches!(msg.msg, tobii_ipc::Msg::Lease(_)) {
                        server.send_to(
                            msg.from,
                            &tobii_ipc::Msg::Status {
                                code: tobii_ipc::StatusCode::Idle,
                                text: "nothing is asking for the tracker".into(),
                            },
                        );
                        server.send_to(msg.from, &reply);
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = std::fs::remove_file(server.path());
        });
        (path, HubThread { stop, handle })
    }

    /// Keeps the stand-in hub alive for as long as the test needs it, and shuts
    /// it down afterwards rather than leaving a thread running under the rest
    /// of the suite.
    struct HubThread {
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        handle: std::thread::JoinHandle<()>,
    }

    impl HubThread {
        fn shut_down(self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = self.handle.join();
        }
    }

    /// Distinct per call, so two tests running at once cannot land on one
    /// socket path. The pid alone is not enough: the whole suite shares one.
    fn unique() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        (std::process::id() as u64) << 16 | N.fetch_add(1, Ordering::Relaxed)
    }

    /// The point of the whole thing: with a hub running, this command asks it
    /// to stand down and waits for it to say it has, instead of opening the
    /// device under it and failing with `DeviceBusy`.
    ///
    /// Without the request being sent, the hub answers nothing, the wait runs
    /// out and this returns a lease holding no client — which is what every
    /// `tobii headpose` did before, and what left the two standby tests in
    /// `device.rs` guarding a path no user reached.
    #[test]
    fn asking_a_running_hub_to_stand_down_gets_the_device() {
        let (path, hub) = fake_hub(
            "granted",
            tobii_ipc::Msg::LeaseReply {
                ok: true,
                text: String::new(),
            },
        );
        let lease = lease_from(&path).expect("a granted lease is not an error");
        assert!(
            lease.client.is_some(),
            "the hub said yes, so the lease is held — and held is what releases it on drop"
        );
        drop(lease);
        hub.shut_down();
    }

    /// The hub refuses while it is mid-calibration or mid-display-setup, and
    /// its sentence names what to wait for. Opening anyway would produce
    /// `DeviceBusy`, which names nothing — so the refusal is the error, text
    /// and all.
    #[test]
    fn a_hub_that_will_not_let_go_fails_the_command_in_its_own_words() {
        let why = "the hub is busy with calibration — try again when it has finished";
        let (path, hub) = fake_hub(
            "refused",
            tobii_ipc::Msg::LeaseReply {
                ok: false,
                text: why.into(),
            },
        );
        let e = lease_from(&path)
            .err()
            .expect("a refusal must fail the command");
        assert_eq!(e.to_string(), why);
        hub.shut_down();
    }

    /// The standalone route. No hub is the ordinary state for anyone who has
    /// not opened one, so it must not be an error: nothing else holds the
    /// device and the command opens it directly, exactly as it always did.
    #[test]
    fn with_no_hub_to_ask_the_command_still_gets_to_open_the_device() {
        let path = std::env::temp_dir().join(format!("tobii-lease-absent-{}.sock", unique()));
        let _ = std::fs::remove_file(&path);
        let lease = lease_from(&path).expect("no hub is not an error");
        assert!(
            lease.client.is_none(),
            "nothing was leased, so there is nothing to give back"
        );
    }

    /// The grant is what puts the hub into standby, so the status change it
    /// broadcasts can arrive before the reply. Counting that as the answer
    /// would have this open the device while the hub was still dropping its
    /// own session — the `DeviceBusy` the hub's Requested/Held split exists to
    /// prevent, arriving at the client that did everything right.
    #[test]
    fn a_status_broadcast_is_not_an_answer_to_a_lease_request() {
        let status = tobii_ipc::Msg::Status {
            code: tobii_ipc::StatusCode::Idle,
            text: "nothing is asking for the tracker".into(),
        };
        assert_eq!(lease_answer(&status), None);
        assert_eq!(lease_answer(&tobii_ipc::Msg::Recentre), None);
        assert_eq!(
            lease_answer(&tobii_ipc::Msg::LeaseReply {
                ok: true,
                text: String::new()
            }),
            Some(StoodDown::Yes)
        );
        assert_eq!(
            lease_answer(&tobii_ipc::Msg::LeaseReply {
                ok: false,
                text: "no".into()
            }),
            Some(StoodDown::No("no".into()))
        );
    }

    /// `--rate 1e-300` used to parse, then `1.0 / rate` overflowed the
    /// `Duration` on the very next line and killed the process — a panic
    /// reachable from a value the validator had just approved.
    #[test]
    fn a_rate_the_parser_accepts_can_always_be_turned_into_an_interval() {
        for ok in ["30", "0.5", "120", "1000"] {
            let hz = parse_rate(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
            // The operation that used to panic.
            let d = std::time::Duration::from_secs_f64(1.0 / hz);
            assert!(d.as_secs_f64() > 0.0, "{ok}");
        }
        for bad in ["1e-300", "0", "-5", "nan", "inf", "abc", "", "1e300"] {
            assert!(parse_rate(bad).is_err(), "{bad:?} must be refused");
        }
    }

    /// GitHub stores release bodies with CRLF line endings, so a naive
    /// "replace every control character" put a U+FFFD at the end of every line
    /// of every real changelog.
    #[test]
    fn a_changelog_survives_sanitising_with_its_text_intact() {
        let notes = "## Fixed\r\n- the thing\r\n- another\r\n";
        assert_eq!(sanitize_notes(notes), "## Fixed\n- the thing\n- another");
        assert_eq!(
            sanitize_notes("plain\nlines\n\ttabbed"),
            "plain\nlines\n\ttabbed"
        );
        assert_eq!(
            sanitize_notes("émoji ✨ and — dashes"),
            "émoji ✨ and — dashes"
        );
    }

    /// The body is written by whoever published the release and used to be
    /// printed verbatim. A terminal reads control characters in it as commands.
    #[test]
    fn control_sequences_in_a_release_body_cannot_reach_the_terminal() {
        // A CSI that would recolour the rest of the session, and an OSC that
        // would set the window title.
        let hostile = "ok\u{1b}[31mred\u{1b}]0;pwned\u{7}";
        let clean = sanitize_notes(hostile);
        assert!(!clean.contains('\u{1b}'), "{clean:?}");
        assert!(!clean.contains('\u{7}'), "{clean:?}");
        assert!(
            clean.contains("ok"),
            "the readable text survives: {clean:?}"
        );

        // C1 controls: not `is_control()` in Rust, still acted on by terminals.
        assert!(!sanitize_notes("a\u{9b}31m").contains('\u{9b}'));
        // Bidi overrides can reorder a line into a different sentence.
        assert!(!sanitize_notes("a\u{202e}b").contains('\u{202e}'));
    }

    /// A reconstructed pose is a guess, and the whole risk of the one-eye
    /// fallback is that it reads exactly like a measurement. This is the line
    /// that says otherwise, so it has to say it while it is happening and it
    /// has to stay short enough to sit beside three rates.
    #[test]
    fn the_status_line_says_when_a_pose_is_a_reconstruction() {
        use tobii_output::pipeline::FallbackStats;

        assert_eq!(
            fallback_note(FallbackStats::default()),
            "",
            "before any frame there is nothing to report"
        );
        assert_eq!(
            fallback_note(FallbackStats {
                both_eyes: 100,
                reconstructed: 0,
                active: false,
            }),
            "",
            "a session with two eyes throughout says nothing at all"
        );

        let live = fallback_note(FallbackStats {
            both_eyes: 75,
            reconstructed: 25,
            active: true,
        });
        assert!(live.contains("25%"), "the share of the session: {live}");
        assert!(
            live.contains("now"),
            "a pose that IS a reconstruction must be visible as one: {live}"
        );
        assert!(live.len() < 32, "it shares a line with the rates: {live:?}");

        let past = fallback_note(FallbackStats {
            both_eyes: 75,
            reconstructed: 25,
            active: false,
        });
        assert!(past.contains("25%"), "{past}");
        assert!(
            !past.contains("now"),
            "a pose measured from two eyes must not be marked as a guess: {past}"
        );
    }

    /// The flag is documented `--recenter`; the prose everywhere in this
    /// repository says "recentre". Both have to work.
    #[test]
    fn the_recentre_flag_is_accepted_in_either_spelling() {
        assert!(wants_recentre(&args(&["tobii", "headpose", "--recenter"])));
        assert!(wants_recentre(&args(&["tobii", "headpose", "--recentre"])));
        assert!(!wants_recentre(&args(&["tobii", "headpose"])));
        assert!(
            !wants_recentre(&args(&["tobii", "headpose", "--check"])),
            "an unrelated flag must not start a settle window"
        );
    }

    #[test]
    fn flag_value_reads_the_argument_after_the_flag() {
        let a = args(&[
            "tobii",
            "headpose",
            "--udp",
            "10.0.0.5:9999",
            "--rate",
            "30",
        ]);
        assert_eq!(flag_value(&a, "--udp"), Some("10.0.0.5:9999"));
        assert_eq!(flag_value(&a, "--rate"), Some("30"));
        assert_eq!(flag_value(&a, "--missing"), None);
    }

    #[test]
    fn flag_value_is_none_when_the_flag_is_last() {
        assert_eq!(
            flag_value(&args(&["tobii", "headpose", "--udp"]), "--udp"),
            None
        );
    }

    #[test]
    fn default_udp_address_is_opentracks_usual_port() {
        let addr = parse_udp_addr(DEFAULT_UDP_ADDR).expect("default address parses");
        assert_eq!(addr.port(), tobii_headpose::opentrack::DEFAULT_PORT);
        assert!(addr.ip().is_loopback());
    }

    #[test]
    fn udp_addresses_parse_and_bad_ones_are_rejected() {
        assert_eq!(
            parse_udp_addr("192.168.1.7:4242")
                .expect("host:port")
                .port(),
            4242
        );
        assert!(parse_udp_addr("192.168.1.7").is_err(), "missing port");
        assert!(parse_udp_addr("not an address").is_err());
    }

    #[test]
    fn rate_accepts_positive_frequencies_only() {
        assert_eq!(parse_rate("120").expect("integral Hz"), 120.0);
        assert_eq!(parse_rate("33.5").expect("fractional Hz"), 33.5);
        for bad in ["0", "-30", "nan", "inf", "", "fast"] {
            assert!(parse_rate(bad).is_err(), "`{bad}` must be rejected");
        }
    }

    /// A separator is required. `tobii game --rate 60 thing` must be a usage
    /// error, not an attempt to execute `--rate`.
    #[test]
    fn the_command_is_what_follows_the_separator() {
        assert_eq!(
            command_after_separator(&args(&["tobii", "game", "--", "prog", "-x"])),
            Some(vec!["prog".to_string(), "-x".to_string()])
        );
        // Further separators belong to the game, not to us.
        assert_eq!(
            command_after_separator(&args(&["tobii", "game", "--", "prog", "--", "-y"])),
            Some(vec!["prog".to_string(), "--".to_string(), "-y".to_string()])
        );
        // Nothing to run.
        assert_eq!(command_after_separator(&args(&["tobii", "game"])), None);
        assert_eq!(
            command_after_separator(&args(&["tobii", "game", "--"])),
            None
        );
        assert_eq!(
            command_after_separator(&args(&["tobii", "game", "prog"])),
            None,
            "without a separator there is no command"
        );
    }

    /// A killed game must not report success. Telling a launcher the game
    /// finished cleanly when it was killed hides every crash.
    #[test]
    fn a_killed_child_does_not_look_like_a_clean_exit() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(exit_code_of(std::process::ExitStatus::from_raw(0)), 0);
        // Raw wait status: low byte is the signal for a killed child.
        let killed = std::process::ExitStatus::from_raw(9);
        assert_ne!(exit_code_of(killed), 0, "SIGKILL must not read as success");
        assert_eq!(exit_code_of(killed), 137, "the shell's 128 + signal");
    }

    /// `tobii games set` has to accept what the file advertises and refuse what
    /// it does not — and this must exercise THIS crate's code.
    ///
    /// Its predecessor lived here and called only `tobii_output`, so breaking
    /// the `set` arm outright left `cargo test -p tobii-cli` green. The
    /// keys-match-the-file property it also asserted belongs with the config
    /// and now lives there, as
    /// `the_advertised_keys_are_exactly_the_keys_the_file_writes`.
    #[test]
    fn games_set_accepts_every_advertised_key_and_names_them_when_refusing() {
        use tobii_output::games::OutputConfig;
        let mut cfg = OutputConfig::default();
        for key in OutputConfig::keys() {
            let probe = match *key {
                "enabled" | "extended_view" | "joystick" | "wake_for_opentrack"
                | "wake_for_joystick" | "keep_awake" => "true",
                "opentrack" => "127.0.0.1:9999",
                "bridge_port" => "4243",
                "ev_hold_ms" => "150",
                "filter_alpha" => "0.3",
                k if k.ends_with("_curve") => "linear",
                _ => "3",
            };
            assert!(
                games_set(&mut cfg, key, probe).is_ok(),
                "advertised key `{key}` was refused by the command"
            );
        }

        let err = games_set(&mut cfg, "nonsense", "1").expect_err("must refuse");
        assert!(err.contains("nonsense"), "{err}");
        assert!(
            err.contains("joystick"),
            "a refusal has to list what IS accepted: {err}"
        );
        assert!(
            !err.contains('#'),
            "the file's comments are not settings: {err}"
        );

        // A refused value must leave the config untouched, since the caller
        // saves whatever comes back.
        let before = cfg.clone();
        assert!(games_set(&mut cfg, "rate_hz", "not-a-number").is_err());
        assert_eq!(cfg, before, "a refused set must change nothing");
    }

    /// `tobii headpose` shipped in v0.1.0 as plain head pose to opentrack. The
    /// config's defaults have Extended View on and a bridge port set, so
    /// reading the file without this narrowing would have added gaze steering
    /// to a running game and bound a second socket, for everyone, on upgrade.
    #[test]
    fn headpose_keeps_its_shipped_behaviour_unless_asked_otherwise() {
        use tobii_output::games::OutputConfig;
        let plain = args(&["tobii", "headpose"]);

        // No config, no flags: exactly what v0.1.0 did.
        let mut c = OutputConfig::default();
        assert!(
            c.extended_view.enabled && c.bridge_port.is_some(),
            "premise"
        );
        apply_games_opt_in(&mut c, &plain);
        assert!(!c.extended_view.enabled, "gaze must not steer by default");
        assert_eq!(c.bridge_port, None, "no second socket by default");
        assert!(!c.joystick, "no new controller appears in anyone's games");

        // Opted in for one run.
        let mut c = OutputConfig::default();
        apply_games_opt_in(&mut c, &args(&["tobii", "headpose", "--extended-view"]));
        assert!(c.extended_view.enabled);
        assert_eq!(
            c.bridge_port,
            Some(tobii_output::games::DEFAULT_BRIDGE_PORT)
        );

        // Opted in permanently, in the config.
        let mut c = OutputConfig {
            enabled: true,
            ..OutputConfig::default()
        };
        apply_games_opt_in(&mut c, &plain);
        assert!(c.extended_view.enabled);

        // And the escape hatch still wins over both.
        let mut c = OutputConfig {
            enabled: true,
            ..OutputConfig::default()
        };
        apply_games_opt_in(&mut c, &args(&["tobii", "headpose", "--no-extended-view"]));
        assert!(!c.extended_view.enabled, "--no-extended-view must win");
    }

    /// The send interval belongs to `Router` now, so what this command still
    /// owns is the default it hands over. A rate that produced an interval
    /// longer than the status line's own period would print "0 sent/s" while
    /// working correctly.
    #[test]
    fn the_default_rate_is_sane_for_the_router_to_throttle_at() {
        let rate = tobii_output::games::OutputConfig::default().rate_hz;
        assert!(rate > 0.0 && rate.is_finite(), "{rate}");
        let interval = Duration::from_secs_f64(1.0 / rate);
        assert!(interval > Duration::ZERO && interval < STATUS_INTERVAL);
    }

    // ------------------------------------------------- tobii games profile

    /// The scratch directories one thread has made, removed when that thread
    /// ends.
    ///
    /// The helpers below hand their directory back out of themselves, so a
    /// guard the caller holds would drop at the end of the helper and take the
    /// directory with it. `libtest` gives each test a thread, and a
    /// thread-local's destructor runs when that thread ends.
    struct ScratchDirs(Vec<std::path::PathBuf>);

    impl Drop for ScratchDirs {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }

    thread_local! {
        static SCRATCH: std::cell::RefCell<ScratchDirs> =
            const { std::cell::RefCell::new(ScratchDirs(Vec::new())) };
    }

    /// A throwaway directory, removed when the thread that asked for it ends.
    ///
    /// Its own prefix rather than the `tobii-steamfix-` one `bridge.rs`'s
    /// tests use: two suites sharing a directory name is two suites able to
    /// delete each other's fixtures.
    fn scratch(tag: &str) -> std::path::PathBuf {
        // `unique()` rather than the tag alone. This clears the directory
        // before handing it over, so two tests sharing a tag had the second
        // delete the first's fixture mid-run — which happened, and read as an
        // intermittent bug in `profile_list`. A per-call uniquifier makes that
        // impossible instead of detectable: the same helper's own doc, four
        // hundred lines up, already says why the pid alone is not enough.
        let dir = std::env::temp_dir().join(format!("tobii-cliprof-{tag}-{}", unique()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("scratch");
        SCRATCH.with_borrow_mut(|dirs| dirs.0.push(dir.clone()));
        dir
    }

    /// A throwaway `$HOME` with one Steam library in it, holding a manifest
    /// per `(appid, name)`.
    ///
    /// No environment variable is read and nothing under the real home is
    /// touched: CI runs these as root, where `$HOME` is somebody else's, and
    /// `steam_appid_for` takes the home to look in for exactly that reason.
    fn steam_home(tag: &str, apps: &[(&str, &str)]) -> std::path::PathBuf {
        let home = scratch(tag);
        let steamapps = home.join(".steam/steam/steamapps");
        std::fs::create_dir_all(&steamapps).expect("steamapps");
        for (id, name) in apps {
            std::fs::write(
                steamapps.join(format!("appmanifest_{id}.acf")),
                format!(
                    "\"AppState\"\n{{\n\t\"appid\"\t\t\"{id}\"\n\t\"name\"\t\t\"{name}\"\n}}\n"
                ),
            )
            .expect("manifest");
        }
        home
    }

    /// A game as a command would have resolved it, without a Steam install.
    ///
    /// No prefix: the tests that care which sentence a prefix produces build
    /// one on disk and go through [`steam_appid_for`], which is the only thing
    /// that may decide that.
    fn game(appid: &str, name: Option<&str>) -> GameRef {
        GameRef {
            appid: appid.to_string(),
            name: name.map(str::to_string),
            has_prefix: false,
            missing_libraries: String::new(),
        }
    }

    /// A config that differs from the default in every kind of value there is:
    /// a bool, an integer, a float, a socket address and a curve name.
    fn tuned_config() -> tobii_output::games::OutputConfig {
        let mut c = tobii_output::games::OutputConfig::default();
        for (k, v) in [
            ("enabled", "true"),
            ("rate_hz", "90"),
            ("filter_alpha", "0.5"),
            ("opentrack", "10.0.0.5:5555"),
            ("bridge_port", "4711"),
            ("joystick", "false"),
            ("ev_yaw_curve", "linear"),
            ("ev_pitch_clamp_deg", "44"),
        ] {
            assert!(c.apply_key(k, v), "the fixture's own `{k}` was refused");
        }
        assert_ne!(
            c,
            tobii_output::games::OutputConfig::default(),
            "a fixture equal to the default would make the round trip below vacuous"
        );
        c
    }

    /// One profile file, written by hand rather than by `Profile::to_toml`, so
    /// that what these cases feed the reader is what a user would type.
    fn write_profile(dir: &std::path::Path, appid: &str, text: &str) {
        std::fs::create_dir_all(dir).expect("profiles dir");
        std::fs::write(dir.join(format!("{appid}.toml")), text).expect("profile");
    }

    /// What `save` captures has to be *every* setting and has to be lossless:
    /// a profile that dropped one would silently leave that setting at
    /// whatever the machine it was applied to happened to have, and a profile
    /// that mangled one would be refused by `apply` on a file this program
    /// wrote itself.
    ///
    /// This is the debt `captured_settings` takes on by reading its pairs back
    /// out of `to_toml` instead of listing them.
    #[test]
    fn captured_settings_are_every_key_and_rebuild_the_config() {
        use tobii_output::games::OutputConfig;
        let tuned = tuned_config();
        let pairs = captured_settings(&tuned);

        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            OutputConfig::keys(),
            "the capture has to be exactly the settings this program advertises"
        );

        let mut back = OutputConfig::default();
        for (k, v) in &pairs {
            assert!(back.apply_key(k, v), "captured `{k} = {v:?}` was refused");
        }
        assert_eq!(
            back, tuned,
            "applying the capture back must rebuild the config it came from"
        );
    }

    /// The checks and the bridge line are somebody's knowledge about the game;
    /// the settings are this machine's current state. `save` has a new version
    /// of the second and none of the first, so overwriting the first would
    /// destroy hand-written work to replace it with nothing.
    #[test]
    fn save_keeps_the_checks_and_bridge_line_a_profile_already_had() {
        use tobii_config::profiles;
        let dir = scratch("save-keeps");
        write_profile(
            &dir,
            "359320",
            "version = 1\n\
             name = \"My Own Name\"\n\
             bridge = true\n\
             \n[settings]\n\
             rate_hz = 11.0\n\
             \n[[check]]\n\
             format = \"binds-dir\"\n\
             path = \"drive_c/Bindings\"\n\
             setting = \"HeadlookMode\"\n\
             wants = \"1\"\n\
             tell = \"Set Head Look to Toggle.\"\n",
        );

        let mut out = String::new();
        profile_save(
            &mut out,
            &dir,
            &[],
            &game("359320", Some("Elite Dangerous")),
            &tuned_config(),
        )
        .expect("save");

        let p = profiles::load_from(&dir, &[], "359320")
            .expect("readable")
            .expect("present")
            .profile;
        assert_eq!(p.bridge, profiles::Bridge::Required, "the bridge line");
        assert_eq!(p.checks.len(), 1, "the check");
        assert_eq!(p.checks[0].setting, "HeadlookMode");
        assert_eq!(p.checks[0].tell, "Set Head Look to Toggle.");
        assert_eq!(
            p.name.as_deref(),
            Some("My Own Name"),
            "a name in the file is somebody's choice; Steam's must not overrule it"
        );
        // And the settings ARE replaced — that is the half `save` can do.
        assert_eq!(
            p.settings,
            captured_settings(&tuned_config()),
            "the settings are the machine's current state, and are taken fresh"
        );
    }

    /// Overwriting a profile this program could not read would destroy
    /// hand-written checks and replace them with a file that has none, and the
    /// user would never learn what the old one said.
    #[test]
    fn save_refuses_to_overwrite_a_profile_it_cannot_read() {
        let dir = scratch("save-refuses");
        // A version this build does not read: there and legible, and not
        // something this build may interpret.
        let text = "version = 9\nname = \"From The Future\"\n";
        write_profile(&dir, "359320", text);

        let mut out = String::new();
        let err = profile_save(
            &mut out,
            &dir,
            &[],
            &game("359320", Some("Elite Dangerous")),
            &tuned_config(),
        )
        .expect_err("an unreadable profile is not something to write over");
        assert!(err.contains("version 9"), "{err}");
        assert!(err.contains("refusing to overwrite"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.join("359320.toml")).expect("still there"),
            text,
            "not one byte of it may have changed"
        );
        assert!(out.is_empty(), "nothing was done, so nothing is reported");
    }

    /// A profile that is absent and a profile that is there and unreadable are
    /// different answers. Reporting the second as the first is the exact bug
    /// shape this project keeps finding in itself.
    #[test]
    fn an_absent_profile_and_an_unreadable_one_are_different_answers() {
        let dir = scratch("absent-vs-unreadable");

        let mut out = String::new();
        profile_show(&mut out, &dir, &[], &game("359320", Some("Elite"))).expect("absent is fine");
        assert!(out.contains("no profile"), "{out}");
        assert!(
            out.contains(&dir.join("359320.toml").display().to_string()),
            "it has to name where it looked: {out}"
        );

        write_profile(&dir, "359320", "version = 1\nnonsense = 3\n");
        let mut out = String::new();
        let err = profile_show(&mut out, &dir, &[], &game("359320", Some("Elite")))
            .expect_err("a file it cannot read is not 'nothing configured'");
        assert!(
            err.contains("line 2"),
            "the line to open an editor at: {err}"
        );
    }

    /// Every check is a thing a person has to go and change by hand, so a
    /// report that dropped one would leave a game misconfigured and say
    /// nothing. All four of its fields are needed to act on it.
    #[test]
    fn show_lists_every_check_with_what_to_change_and_where() {
        let dir = scratch("show-checks");
        write_profile(
            &dir,
            "359320",
            "version = 1\n\
             \n[[check]]\n\
             format = \"binds-dir\"\n\
             path = \"drive_c/Bindings\"\n\
             setting = \"HeadlookMode\"\n\
             wants = \"1\"\n\
             tell = \"Set Head Look to Toggle.\"\n\
             \n[[check]]\n\
             format = \"attributes-xml\"\n\
             path = \"drive_c/attributes.xml\"\n\
             setting = \"FreeLook\"\n\
             wants = \"on\"\n\
             tell = \"Turn Free Look on.\"\n",
        );
        let mut out = String::new();
        profile_show(&mut out, &dir, &[], &game("359320", Some("Elite"))).expect("show");
        for needle in [
            "drive_c/Bindings",
            "HeadlookMode",
            "\"1\"",
            "Set Head Look to Toggle.",
            "drive_c/attributes.xml",
            "FreeLook",
            "\"on\"",
            "Turn Free Look on.",
        ] {
            assert!(out.contains(needle), "{needle:?} is missing from:\n{out}");
        }
    }

    /// A format this build cannot read costs that one check, not the file —
    /// and the user has to be told which checks it cost, or they will read the
    /// list as complete.
    #[test]
    fn a_check_in_an_unreadable_format_is_named_as_one_that_was_not_looked_up() {
        let dir = scratch("unknown-format");
        write_profile(
            &dir,
            "359320",
            "version = 1\n\
             \n[[check]]\n\
             format = \"something-newer\"\n\
             path = \"drive_c/x.cfg\"\n\
             setting = \"Thing\"\n\
             wants = \"on\"\n\
             tell = \"Turn it on.\"\n",
        );
        let mut out = String::new();
        profile_show(&mut out, &dir, &[], &game("359320", None)).expect("show");
        assert!(
            out.contains("something-newer"),
            "the format has to be named: {out}"
        );
        assert!(
            out.contains("cannot read"),
            "and said to be one this build cannot read: {out}"
        );
    }

    /// A pair `apply_key` refuses stops the whole profile. Applying the half
    /// it understood is how somebody ends up with a config nobody wrote.
    #[test]
    fn apply_refuses_a_profile_with_an_unusable_setting_and_changes_nothing() {
        use tobii_output::games::OutputConfig;
        let dir = scratch("apply-refuses");
        // A typo, above a perfectly good pair — so a per-pair applier would
        // get as far as the second and change the config.
        write_profile(
            &dir,
            "359320",
            "version = 1\n[settings]\nenabeld = true\nrate_hz = 90.0\n",
        );
        let mut cfg = OutputConfig::default();
        let before = cfg.clone();
        let mut out = String::new();
        let err = profile_apply(&mut out, &dir, &[], &game("359320", None), &mut cfg)
            .expect_err("a setting it cannot use stops the profile");
        assert!(err.contains("enabeld"), "name the pair at fault: {err}");
        assert!(
            err.contains("rate_hz"),
            "and list the keys that are valid: {err}"
        );
        assert_eq!(cfg, before, "nothing may have been applied");
    }

    /// `apply` is `save`'s other half and must actually work — and must say
    /// that what it changed is this program's one set of settings, not
    /// something per-game.
    #[test]
    fn apply_puts_every_setting_into_effect_and_asks_to_be_saved() {
        use tobii_output::games::OutputConfig;
        let dir = scratch("apply-works");
        let tuned = tuned_config();
        let mut p = tobii_config::profiles::Profile {
            settings: captured_settings(&tuned),
            ..Default::default()
        };
        p.name = Some("Elite".to_string());
        tobii_config::profiles::save_to(&dir, "359320", &p).expect("write the profile");

        let mut cfg = OutputConfig::default();
        let mut out = String::new();
        let changed = profile_apply(&mut out, &dir, &[], &game("359320", None), &mut cfg)
            .expect("every pair came from this program's own writer");
        assert!(changed, "a profile with settings is worth saving");
        assert_eq!(cfg, tuned, "the config the profile was captured from");
        assert!(
            out.contains(&tobii_output::games::games_path().display().to_string()),
            "it has to name the file it changed: {out}"
        );
        assert!(
            out.contains("every game"),
            "and that these settings are not per-game: {out}"
        );
    }

    /// A profile with no settings must not be the reason a `games.toml`
    /// appears on disk: `apply` would then have written a file of defaults
    /// nobody chose, and reported that it had applied a profile.
    #[test]
    fn apply_of_a_profile_with_no_settings_asks_for_nothing_to_be_written() {
        use tobii_output::games::OutputConfig;
        let dir = scratch("apply-empty");
        write_profile(&dir, "359320", "version = 1\nbridge = true\n");
        let mut cfg = OutputConfig::default();
        let before = cfg.clone();
        let mut out = String::new();
        let changed = profile_apply(&mut out, &dir, &[], &game("359320", None), &mut cfg)
            .expect("a checks-only profile is a fine profile");
        assert!(!changed, "there was nothing to apply, so nothing to save");
        assert_eq!(cfg, before);
        // And it still says what is left to do, which is the whole of what
        // this profile holds.
        assert!(out.contains("tobii bridge install --steam 359320"), "{out}");
    }

    /// `apply` on a game with no profile is a question with no answer, not a
    /// no-op: the user asked for something to be applied and nothing was.
    #[test]
    fn apply_without_a_profile_is_an_error_naming_where_it_looked() {
        use tobii_output::games::OutputConfig;
        let dir = scratch("apply-absent");
        let mut cfg = OutputConfig::default();
        let mut out = String::new();
        let err = profile_apply(&mut out, &dir, &[], &game("359320", None), &mut cfg)
            .expect_err("nothing to apply");
        assert!(
            err.contains(&dir.join("359320.toml").display().to_string()),
            "{err}"
        );
    }

    /// `forget` removes the user's file and names it. Twice in a row is not an
    /// error — but the second time must not claim to have removed anything.
    #[test]
    fn forget_removes_the_users_file_and_names_it_exactly_once() {
        let dir = scratch("forget");
        write_profile(&dir, "359320", "version = 1\n");
        let path = dir.join("359320.toml");

        let mut out = String::new();
        profile_forget(&mut out, &dir, &[], "359320").expect("removing a file that is there");
        assert!(
            out.contains("removed") && out.contains(&path.display().to_string()),
            "{out}"
        );
        assert!(!path.exists(), "the file is gone");

        let mut out = String::new();
        profile_forget(&mut out, &dir, &[], "359320").expect("removing nothing is not an error");
        assert!(
            !out.contains("removed"),
            "nothing was removed the second time: {out}"
        );
        assert!(
            out.contains(&path.display().to_string()),
            "and it still names the file it did not find: {out}"
        );
    }

    /// A compiled-in profile is part of the program. `forget` can take away
    /// the file that replaced it and must not claim to have taken away the
    /// profile itself.
    #[test]
    fn forget_does_not_claim_to_remove_a_compiled_in_profile() {
        let dir = scratch("forget-builtin");
        let builtin: &[(&str, &str)] = &[("359320", "version = 1\nname = \"Shipped\"\n")];

        let mut out = String::new();
        profile_forget(&mut out, &dir, builtin, "359320").expect("not an error");
        assert!(!out.contains("removed"), "there was no file: {out}");
        assert!(
            out.contains("compiled into tobii-linux"),
            "and it has to say why the profile is still there: {out}"
        );

        // With a file over the top, the file goes and the built-in one comes
        // back — which is also worth a sentence.
        write_profile(&dir, "359320", "version = 1\n");
        let mut out = String::new();
        profile_forget(&mut out, &dir, builtin, "359320").expect("removing the file");
        assert!(out.contains("removed"), "{out}");
        assert!(out.contains("compiled into tobii-linux"), "{out}");
    }

    /// A name that is not an app id has no profile file, and `forget` must say
    /// that rather than build a path out of it. `is_appid` is tighter than
    /// "all digits" for this reason: `0999.toml` and `999.toml` would be two
    /// files for one game.
    #[test]
    fn forget_refuses_a_name_that_could_not_be_a_profile_file() {
        let dir = scratch("forget-notanappid");
        let mut out = String::new();
        let err = profile_forget(&mut out, &dir, &[], "0999").expect_err("a leading zero");
        assert!(err.contains("0999"), "{err}");
        assert!(
            !dir.join("0999.toml").exists() && out.is_empty(),
            "nothing was touched"
        );
    }

    /// The four things `--steam` can answer are worded in `bridge.rs` and
    /// decided in `tobii_steam`, and the wording is what a user sees. These
    /// pin the two that this command shares with it, byte for byte, because
    /// the phrasing is settled and must not drift between commands.
    #[test]
    fn an_ambiguous_name_lists_what_it_matched_and_asks_for_the_app_id() {
        let home = steam_home(
            "many",
            &[("11", "Fixture Game One"), ("22", "Fixture Game Two")],
        );
        let err = steam_appid_for(&home, "fixture").expect_err("two matches is not an answer");
        assert_eq!(
            err,
            "\"fixture\" matches more than one game; pass the app id:\n  \
             11         Fixture Game One\n  \
             22         Fixture Game Two"
        );
    }

    #[test]
    fn a_name_that_matches_nothing_lists_everything_that_is_installed() {
        let home = steam_home(
            "none",
            &[("11", "Fixture Game One"), ("22", "Fixture Game Two")],
        );
        let err = steam_appid_for(&home, "nothing").expect_err("no match is not an answer");
        assert_eq!(
            err,
            "no installed Steam game matches \"nothing\"\ninstalled:\n  \
             11         Fixture Game One\n  \
             22         Fixture Game Two"
        );
    }

    /// A profile outlives the install it was written for. An app id that
    /// matches nothing is still an app id, and `show`, `forget` and `apply`
    /// all have honest answers for a game that has been uninstalled — so it
    /// must not be refused the way a name is.
    #[test]
    fn an_app_id_for_a_game_that_is_not_installed_is_still_a_game() {
        let home = steam_home("gone", &[("11", "Fixture Game One")]);
        let g = steam_appid_for(&home, "359320").expect("an app id is an app id");
        assert_eq!(g.appid, "359320");
        assert_eq!(g.name, None, "this machine has no name for it");
        assert!(
            g.heading().contains("not installed on this machine"),
            "{}",
            g.heading()
        );
        // A name, though, yields no app id at all, so it is still refused.
        assert!(steam_appid_for(&home, "elite").is_err());
    }

    /// Steam records a library's path, not whether its drive is plugged in.
    /// "Not installed on this machine" over an unplugged drive is the exact
    /// confident negative this project keeps removing.
    #[test]
    fn a_library_this_machine_cannot_see_stops_the_not_installed_claim() {
        let home = steam_home("unplugged", &[("11", "Fixture Game One")]);
        let gone = home.join("not-mounted");
        std::fs::write(
            home.join(".steam/steam/steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                gone.display()
            ),
        )
        .expect("libraryfolders");

        let g = steam_appid_for(&home, "359320").expect("an app id resolves");
        let heading = g.heading();
        assert!(
            !heading.contains("not installed on this machine"),
            "a drive nobody could look in is not grounds for that: {heading}"
        );
        assert!(
            heading.contains(&gone.display().to_string()),
            "and the user has to recognise their own drive in it: {heading}"
        );

        // The same for a name that matched nothing.
        let err = steam_appid_for(&home, "nothing").expect_err("no match");
        assert!(
            err.starts_with("nothing in the Steam libraries this machine has matches"),
            "{err}"
        );
        assert!(err.contains(&gone.display().to_string()), "{err}");
    }

    /// An empty needle is a substring of every name, so `resolve` matches
    /// everything with it — and on a machine with exactly one application that
    /// is `Match::One`, indistinguishable from a fragment that picked it out.
    /// `tobii games profile save ""` would then write a profile for whatever
    /// that one application happened to be.
    #[test]
    fn an_empty_game_argument_is_refused_before_it_can_match_everything() {
        let home = steam_home("empty-needle", &[("11", "The Only Game Here")]);
        assert!(
            matches!(
                tobii_steam::resolve(&tobii_steam::apps(&home), ""),
                tobii_steam::Match::One(_)
            ),
            "premise: with one app installed, the empty needle resolves to it"
        );
        let err = steam_appid_for(&home, "").expect_err("an empty argument names no game");
        assert!(err.contains("name a game"), "{err}");
    }

    /// A broken profile and a file that is not a profile are different things,
    /// and `tobii uninstall --purge` will say the same of the second. Folding
    /// them together is how one gets read as the other.
    #[test]
    fn the_listing_keeps_a_broken_profile_apart_from_something_that_is_not_one() {
        let dir = scratch("listing");
        write_profile(&dir, "11", "version = 1\nname = \"Good One\"\n");
        write_profile(&dir, "22", "version = 99\n");
        std::fs::write(dir.join("notes.txt"), "mine\n").expect("stray");

        let mut out = String::new();
        profile_list(&mut out, &dir, &[]);
        let broken = out.find("could not be used:").expect("the broken one");
        let stray = out
            .find("in that directory and not a profile:")
            .expect("stray");
        assert!(
            out[broken..stray].contains("22.toml"),
            "the version this build cannot read belongs under 'could not be used':\n{out}"
        );
        assert!(
            out[stray..].contains("notes.txt") && !out[stray..].contains("22.toml"),
            "and somebody's own file belongs under the other heading:\n{out}"
        );
        assert!(out.contains("Good One"), "the usable one is still listed");
        assert!(
            out.contains("this build ships no profile for any game"),
            "which is true of this build, and read off the table: {out}"
        );
    }

    /// With a profile compiled in, that sentence is false. It is read off the
    /// table for exactly this reason, so this is what proves it is.
    #[test]
    fn the_listing_does_not_claim_no_profiles_ship_when_one_does() {
        let dir = scratch("listing-builtin");
        let builtin: &[(&str, &str)] = &[("359320", "version = 1\nname = \"Shipped\"\n")];
        let mut out = String::new();
        profile_list(&mut out, &dir, builtin);
        assert!(
            !out.contains("ships no profile"),
            "one is compiled in: {out}"
        );
        assert!(out.contains("Shipped"), "and it is listed: {out}");
        assert!(
            out.contains("built into tobii-linux"),
            "with its origin: {out}"
        );
    }

    // ------------------------------------------------- one game, one answer

    /// `profile show` and `bridge status` were answering one app id two ways.
    /// Steam keeps `compatdata/<appid>/pfx` after a title is uninstalled, and
    /// a non-Steam shortcut gets one under an id no `appmanifest_*.acf` ever
    /// mentions — `bridge status --steam` resolves both, while a heading built
    /// from the installed list alone called them "not installed on this
    /// machine".
    #[test]
    fn an_app_id_with_a_prefix_and_no_manifest_is_not_called_not_installed() {
        let home = steam_home("prefix-no-manifest", &[("11", "Fixture Game One")]);
        std::fs::create_dir_all(home.join(".steam/steam/steamapps/compatdata/999999/pfx/drive_c"))
            .expect("prefix");
        // The other command's answer, from the other command's function.
        assert!(
            tobii_steam::prefix(&home, "999999").is_some(),
            "the fixture has to be one `bridge status --steam 999999` resolves"
        );
        let head = steam_appid_for(&home, "999999")
            .expect("an app id is an app id")
            .heading();
        assert!(!head.contains("not installed"), "{head}");
        assert!(head.contains("Proton prefix"), "{head}");

        // An app id with neither a manifest nor a prefix still gets the plain
        // negative: this is a third answer, not a softening of the second.
        let head = steam_appid_for(&home, "888888")
            .expect("an app id is an app id")
            .heading();
        assert!(head.contains("not installed on this machine"), "{head}");

        // And with a library nobody could look in, the prefix sentence must
        // not rule out the likeliest answer of the three — that it is
        // installed on the drive that is not plugged in.
        let gone = home.join("not-mounted");
        std::fs::write(
            home.join(".steam/steam/steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                gone.display()
            ),
        )
        .expect("libraryfolders");
        let head = steam_appid_for(&home, "999999")
            .expect("an app id is an app id")
            .heading();
        assert!(head.contains("Proton prefix"), "{head}");
        assert!(
            head.contains(&gone.display().to_string()),
            "the drive nobody could look in has to be named: {head}"
        );
    }

    /// Every verb here names a file after the app id, so one that cannot name
    /// a file is refused at the door. It used to be carried: `show 0999`
    /// printed a heading saying the game is not installed, and then failed
    /// with an unrelated second reason.
    #[test]
    fn an_app_id_that_could_not_name_a_profile_file_is_refused_not_answered() {
        let home = steam_home("bad-appid", &[("11", "Fixture Game One")]);
        for bad in ["0999", "12345678901"] {
            let e = steam_appid_for(&home, bad).expect_err("that cannot name a profile");
            assert!(e.contains("is not a Steam app id"), "{bad}: {e}");
            assert!(e.contains("does not begin with a zero"), "{bad}: {e}");
        }
        assert_eq!(
            steam_appid_for(&home, "999")
                .expect("a canonical app id still resolves")
                .appid,
            "999"
        );
    }

    /// The columns are measured against the rows, because a fixed 28 for the
    /// name put the counts of "Stronghold Crusader: Definitive Edition" ten
    /// characters to the right of everybody else's.
    #[test]
    fn the_listing_lines_up_its_counts_over_a_name_wider_than_the_column() {
        /// Where the count starts on one row: the space before it, plus one.
        fn counts_at(line: &str) -> usize {
            let at = line.find(" setting").expect("a counts column");
            line[..at].rfind(' ').expect("a space before the count") + 1
        }
        let dir = scratch("listing-wide");
        write_profile(&dir, "359320", "version = 1\nname = \"Elite Dangerous\"\n");
        write_profile(
            &dir,
            "1000130",
            "version = 1\nname = \"Stronghold Crusader: Definitive Edition\"\n",
        );
        let mut out = String::new();
        profile_list(&mut out, &dir, &[]);
        let rows: Vec<usize> = out
            .lines()
            .filter(|l| l.contains(" setting"))
            .map(counts_at)
            .collect();
        assert_eq!(rows.len(), 2, "{out}");
        assert_eq!(
            rows[0], rows[1],
            "the counts column moves with the name:\n{out}"
        );
    }

    // ------------------------------------------------- what save may claim

    /// The "somebody wrote it: fix it, or move it aside" refusal is about a
    /// file that exists. `0999` cannot name one, so nothing was read, nothing
    /// is there, and nobody wrote it — the same shape of bug as the rest of
    /// this session, pointing the other way.
    #[test]
    fn save_does_not_say_somebody_wrote_a_profile_that_could_not_exist() {
        let dir = scratch("save-not-an-appid");
        let mut out = String::new();
        let e = profile_save(&mut out, &dir, &[], &game("0999", None), &tuned_config())
            .expect_err("0999 cannot name a profile file");
        assert!(!e.contains("somebody wrote it"), "nobody did:\n{e}");
        assert!(e.contains("Nothing was written."), "{e}");
        assert!(e.contains("does not begin with a zero"), "{e}");
        assert!(!dir.join("0999.toml").exists(), "and none was created");
    }

    /// A comment is hand-written work and the only place in this format a
    /// verification date can live, which is exactly what a first verified
    /// profile is made of. The writer rebuilds the file from what was parsed,
    /// so a comment does not survive a `save` — and the report used to say
    /// "nothing here knows better than they do" over the top of that.
    ///
    /// The assertion is a biconditional rather than a list of the comments
    /// this fixture loses: the day `Profile::to_toml` carries comments, this
    /// must go on passing, and must still catch a report that cries wolf.
    #[test]
    fn the_report_names_every_comment_the_write_did_not_carry_and_no_others() {
        let dir = scratch("save-comments");
        let dated = "# measured 2026-10-01 by playing the game with the bridge running";
        let meaning = "# HeadlookMode 1 == Toggle; 0 == Hold. Verified on 4.0.";
        write_profile(
            &dir,
            "359320",
            &format!(
                "{dated}\nversion = 1\nbridge = true\n\n{meaning}\n[[check]]\n\
                 format = \"binds-dir\"\npath = \"drive_c/B\"\nsetting = \"HeadlookMode\"\n\
                 wants = \"1\"\ntell = \"Set it.\"\n"
            ),
        );
        let before = std::fs::read_to_string(dir.join("359320.toml")).expect("before");
        assert!(
            before.contains(dated) && before.contains(meaning),
            "the fixture"
        );

        let mut out = String::new();
        profile_save(
            &mut out,
            &dir,
            &[],
            &game("359320", Some("Elite")),
            &tuned_config(),
        )
        .expect("save");

        // Worked out here rather than asked of the code under test, which
        // would otherwise be marking its own paper.
        let after = std::fs::read_to_string(dir.join("359320.toml")).expect("after");
        let comments = |t: &str| -> Vec<String> {
            t.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| l.starts_with('#'))
                .collect()
        };
        let kept = comments(&after);
        let lost: Vec<String> = comments(&before)
            .into_iter()
            .filter(|c| !kept.contains(c))
            .collect();
        for c in &lost {
            assert!(out.contains(c), "{c:?} was destroyed silently:\n{out}");
        }
        assert_eq!(
            lost.is_empty(),
            !out.contains("NOT kept:"),
            "the report must say comments went exactly when they did.\nlost: {lost:?}\n{out}"
        );
    }

    /// The order people type is "set the game up, then save the profile", and
    /// this is the step of it that bites: `save` writes down whatever is on
    /// disk, and `apply` puts it back. Captured with game output off, the
    /// profile turns game output off.
    #[test]
    fn save_says_so_when_what_it_captured_has_game_output_switched_off() {
        let dir = scratch("save-disabled");
        let mut off = tobii_output::games::OutputConfig::default();
        assert!(off.apply_key("enabled", "false"));
        let mut out = String::new();
        profile_save(&mut out, &dir, &[], &game("359320", Some("Elite")), &off).expect("save");
        assert!(out.contains("enabled = false"), "{out}");
        assert!(out.contains("tobii games set enabled true"), "{out}");

        // And it is a report of what was captured, not a paragraph that is
        // always there.
        let mut on = tobii_output::games::OutputConfig::default();
        assert!(on.apply_key("enabled", "true"));
        let mut out = String::new();
        profile_save(&mut out, &dir, &[], &game("359320", Some("Elite")), &on).expect("save");
        assert!(!out.contains("enabled = false"), "{out}");
    }

    // ---------------------------------------------------- authoring a check

    /// The five flags of one check, as a command line.
    fn add_argv(appid: &str, fields: &[(&str, &str)]) -> Vec<String> {
        let mut v = vec![
            "tobii".to_string(),
            "games".to_string(),
            "profile".to_string(),
            "check".to_string(),
            "add".to_string(),
            appid.to_string(),
        ];
        for (flag, value) in fields {
            v.push((*flag).to_string());
            v.push((*value).to_string());
        }
        v
    }

    /// Every field of one whole check, for a fixture to vary one of.
    const CHECK_FIELDS: [(&str, &str); 5] = [
        ("--format", "binds-dir"),
        ("--path", "drive_c/users/steamuser/Options/Bindings"),
        ("--setting", "HeadlookMode"),
        ("--wants", "1"),
        ("--tell", "Set head look to toggle."),
    ];

    /// The whole point of the command: what somebody types is what the reader
    /// reads back, format name included — and `binds-dir` has to arrive as the
    /// format this build reads, not as a name it does not know.
    #[test]
    fn check_add_writes_a_check_the_profile_reader_reads_back_unchanged() {
        use tobii_config::profiles::{Check, Format};
        let dir = scratch("check-add");
        let mut out = String::new();
        profile_check_add(
            &mut out,
            &dir,
            &[],
            &game("359320", Some("Elite")),
            &add_argv("359320", &CHECK_FIELDS),
        )
        .expect("add");
        let loaded = tobii_config::profiles::load_from(&dir, &[], "359320")
            .expect("what this wrote has to be readable")
            .expect("and has to be there");
        assert_eq!(
            loaded.profile.checks,
            vec![Check {
                format: Format::BindsDir,
                path: "drive_c/users/steamuser/Options/Bindings".into(),
                setting: "HeadlookMode".into(),
                wants: "1".into(),
                tell: "Set head look to toggle.".into(),
            }]
        );
        // Written down as somebody's claim, and said to be one.
        assert!(out.contains("Nothing here verified"), "{out}");
    }

    /// What a `path` may be is the profile parser's rule, and this command
    /// asks it rather than keeping a copy — so a check it will not accept
    /// leaves the file exactly as it was, instead of the corrupt profile that
    /// a write-then-fail would have left.
    #[test]
    fn check_add_writes_nothing_when_the_reader_would_refuse_the_check() {
        let dir = scratch("check-add-refused");
        write_profile(&dir, "359320", "version = 1\nname = \"Elite\"\n");
        let before = std::fs::read_to_string(dir.join("359320.toml")).expect("before");
        for bad in ["/absolute", "a/../../etc", "drive_c\\Options", ""] {
            let mut fields = CHECK_FIELDS;
            fields[1].1 = bad;
            let mut out = String::new();
            let e = profile_check_add(
                &mut out,
                &dir,
                &[],
                &game("359320", Some("Elite")),
                &add_argv("359320", &fields),
            )
            .expect_err("the reader would refuse that path");
            assert!(e.contains("nothing was written"), "{bad:?}: {e}");
            assert_eq!(
                std::fs::read_to_string(dir.join("359320.toml")).expect("after"),
                before,
                "{bad:?} changed the file"
            );
        }
    }

    /// A check the author did not make is not one this program may make for
    /// them. Every field is required and none has a default, least of all
    /// `--wants`, which is the claim itself.
    #[test]
    fn check_add_refuses_rather_than_supplying_a_field_left_out() {
        let dir = scratch("check-add-missing");
        for drop in ["--format", "--path", "--setting", "--wants", "--tell"] {
            let fields: Vec<(&str, &str)> = CHECK_FIELDS
                .iter()
                .filter(|(f, _)| *f != drop)
                .copied()
                .collect();
            let mut out = String::new();
            let e = profile_check_add(
                &mut out,
                &dir,
                &[],
                &game("359320", None),
                &add_argv("359320", &fields),
            )
            .expect_err("a field left out is not one to invent");
            assert!(e.contains(drop), "{drop}: {e}");
            assert!(
                !dir.join("359320.toml").exists(),
                "{drop}: a profile was written anyway"
            );
        }
    }

    /// `--wants --tell "..."` is a value left out, not a game that wants the
    /// string `--tell`. Taken literally it would be read back at somebody as
    /// the value their game should have.
    #[test]
    fn check_add_will_not_take_the_next_flag_as_a_value() {
        let dir = scratch("check-add-flagvalue");
        let mut argv = add_argv("359320", &CHECK_FIELDS);
        let at = argv
            .iter()
            .position(|a| a == "1")
            .expect("the --wants value");
        argv.remove(at);
        let mut out = String::new();
        let e = profile_check_add(&mut out, &dir, &[], &game("359320", None), &argv)
            .expect_err("--wants has no value");
        assert!(e.contains("--wants"), "{e}");
        assert!(e.contains("its value is missing"), "{e}");
        assert!(!dir.join("359320.toml").exists(), "and nothing was written");
    }

    /// Two checks on one setting report it twice, and disagree the day one of
    /// their `wants` is edited. The comparison is made after the format name
    /// has been through the reader, so a `binds-dir` on the command line meets
    /// the `BindsDir` already in the file as the same format.
    #[test]
    fn check_add_refuses_a_second_check_on_a_setting_already_read() {
        let dir = scratch("check-add-dup");
        let mut out = String::new();
        profile_check_add(
            &mut out,
            &dir,
            &[],
            &game("359320", None),
            &add_argv("359320", &CHECK_FIELDS),
        )
        .expect("the first");
        let mut again = CHECK_FIELDS;
        again[3].1 = "0";
        let mut out = String::new();
        let e = profile_check_add(
            &mut out,
            &dir,
            &[],
            &game("359320", None),
            &add_argv("359320", &again),
        )
        .expect_err("the second reads the same setting");
        assert!(e.contains("already reads HeadlookMode"), "{e}");
        let p = tobii_config::profiles::load_from(&dir, &[], "359320")
            .expect("readable")
            .expect("there")
            .profile;
        assert_eq!(p.checks.len(), 1, "{:#?}", p.checks);
        assert_eq!(
            p.checks[0].wants, "1",
            "the one that was there is untouched"
        );
    }

    /// `remove` takes the check the number names and no other, and prints what
    /// it took in full: the number is not the check, and somebody who removed
    /// the wrong one needs the text back rather than a count.
    #[test]
    fn check_remove_takes_out_the_one_numbered_and_prints_it_back() {
        let dir = scratch("check-remove");
        for setting in ["First", "Second", "Third"] {
            let mut fields = CHECK_FIELDS;
            fields[2].1 = setting;
            let mut out = String::new();
            profile_check_add(
                &mut out,
                &dir,
                &[],
                &game("359320", None),
                &add_argv("359320", &fields),
            )
            .expect("add");
        }
        let mut out = String::new();
        profile_check_remove(&mut out, &dir, &[], &game("359320", None), Some("2"))
            .expect("remove");
        let p = tobii_config::profiles::load_from(&dir, &[], "359320")
            .expect("readable")
            .expect("there")
            .profile;
        let left: Vec<&str> = p.checks.iter().map(|c| c.setting.as_str()).collect();
        assert_eq!(left, ["First", "Third"]);
        assert!(out.contains("--setting \"Second\""), "{out}");
        assert!(out.contains("puts it back"), "{out}");
    }

    /// A number the profile does not have changes nothing, and says what the
    /// numbers actually are.
    #[test]
    fn check_remove_refuses_a_number_the_profile_does_not_have() {
        let dir = scratch("check-remove-range");
        let mut out = String::new();
        profile_check_add(
            &mut out,
            &dir,
            &[],
            &game("359320", None),
            &add_argv("359320", &CHECK_FIELDS),
        )
        .expect("add");
        let before = std::fs::read_to_string(dir.join("359320.toml")).expect("before");
        for bad in ["2", "0", "two", "-1"] {
            let mut out = String::new();
            profile_check_remove(&mut out, &dir, &[], &game("359320", None), Some(bad))
                .expect_err("no such check");
            assert_eq!(
                std::fs::read_to_string(dir.join("359320.toml")).expect("after"),
                before,
                "{bad:?} changed the file"
            );
        }
    }

    /// `where` is the half of "run the checks" this binary can do: it places
    /// each one on this machine and says whether anything is there and whether
    /// it is the kind of thing the format reads. It does **not** open them —
    /// the reader for these formats is `tobii-gameconf`, which this binary
    /// does not carry, and a value printed without a reader would be invented.
    #[test]
    fn check_where_places_each_check_and_does_not_read_what_is_inside() {
        let home = steam_home("check-where", &[("359320", "Elite")]);
        let drive_c = home.join(".steam/steam/steamapps/compatdata/359320/pfx/drive_c");
        std::fs::create_dir_all(drive_c.join("Bindings")).expect("a binds directory");
        std::fs::write(
            drive_c.join("attributes.xml"),
            "<Attr value=\"ZZ_SECRET\"/>",
        )
        .expect("an attributes file");
        let dir = home.join("profiles");
        write_profile(
            &dir,
            "359320",
            // Four checks, because there are four things to say: the path is
            // the kind the format reads; the path is a file this command
            // deliberately does not open; the path is there and is the wrong
            // kind; the path is not there at all.
            "version = 1\n\
             \n[[check]]\nformat = \"binds-dir\"\npath = \"drive_c/Bindings\"\n\
             setting = \"HeadlookMode\"\nwants = \"1\"\ntell = \"Set it.\"\n\
             \n[[check]]\nformat = \"attributes-xml\"\npath = \"drive_c/attributes.xml\"\n\
             setting = \"FreeLook\"\nwants = \"on\"\ntell = \"Turn it on.\"\n\
             \n[[check]]\nformat = \"attributes-xml\"\npath = \"drive_c/Bindings\"\n\
             setting = \"FreeLook\"\nwants = \"on\"\ntell = \"Turn it on.\"\n\
             \n[[check]]\nformat = \"attributes-xml\"\npath = \"drive_c/gone.xml\"\n\
             setting = \"FreeLook\"\nwants = \"on\"\ntell = \"Turn it on.\"\n",
        );
        let mut out = String::new();
        profile_check_where(&mut out, &dir, &[], &game("359320", Some("Elite")), &home)
            .expect("where");

        assert!(
            out.contains(&drive_c.join("Bindings").display().to_string()),
            "the path it would open:\n{out}"
        );
        assert!(out.contains("as binds-dir needs"), "{out}");
        assert!(out.contains("as attributes-xml needs"), "{out}");
        assert!(out.contains("nothing is there."), "{out}");
        assert!(out.contains("The path names the wrong thing."), "{out}");
        // Check 2 names that file, this command found it, and it still did not
        // look inside — which is the whole difference between reporting where
        // a check looks and inventing what it would say.
        assert!(
            !out.contains("ZZ_SECRET"),
            "it must not have opened a game's file:\n{out}"
        );
    }

    /// The one place a user can find out what a `[[check]]` is. It was
    /// documented only in a Rust module's doc comment, which made the feature
    /// unusable for the thing it exists for.
    ///
    /// And asking for it is not an error: it went to stderr behind `error:`
    /// and exited 1, which tells somebody who asked for help that asking was
    /// the mistake — and puts the schema where a pipe into a pager will not
    /// find it.
    #[test]
    fn asking_for_check_with_no_verb_says_what_every_key_of_one_means() {
        let dir = scratch("check-schema");
        let mut out = String::new();
        profile_check_cmd(
            &mut out,
            &dir,
            &[],
            &args(&["tobii", "games", "profile", "check"]),
        )
        .expect("asking what a check is is not an error");
        let e = out;
        for needle in [
            "[[check]]",
            "format",
            "path",
            "setting",
            "wants",
            "tell",
            "binds-dir",
            "attributes-xml",
            "Proton prefix",
        ] {
            assert!(e.contains(needle), "{needle:?} is missing from:\n{e}");
        }
    }

    /// [[Game-Profiles]] designates this the canonical spec and tells readers
    /// to prefer it over any copy, and the hub's help window sends people here
    /// too. It said `path` was "where that sits under the Proton prefix" and
    /// nothing else — while the same binary's refusal of an absolute `path`
    /// told the author to write `dosdevices/s:/steamapps/common/...`, which is
    /// the Steam library and not the prefix. A canonical spec that does not
    /// mention the spelling its own error messages recommend is not canonical.
    ///
    /// Compared against that refusal rather than spot-checked, so the day
    /// `check_path` changes its advice this fails instead of going quietly out
    /// of date.
    #[test]
    fn the_canonical_check_spec_teaches_the_path_its_own_refusal_recommends() {
        let refusal = tobii_config::profiles::parse(
            "version = 1\n\
             \n[[check]]\n\
             format = \"binds-dir\"\n\
             path = \"/steamapps/common/Elite Dangerous\"\n\
             setting = \"HeadlookMode\"\n\
             wants = \"1\"\n\
             tell = \"Set head look to toggle.\"\n",
        )
        .expect_err("an absolute path is refused")
        .message;
        let recommended = "dosdevices/s:/steamapps/common/";
        assert!(
            refusal.contains(recommended),
            "this test's premise — the refusal recommends it: {refusal}"
        );

        let dir = scratch("check-schema-canonical");
        let mut out = String::new();
        profile_check_cmd(
            &mut out,
            &dir,
            &[],
            &args(&["tobii", "games", "profile", "check"]),
        )
        .expect("asking what a check is is not an error");
        assert!(
            out.contains(recommended),
            "the canonical spec has to teach what the refusal recommends:\n{out}"
        );
        // And say what it is, not only how to spell it: an author who reads
        // only this must not come away thinking a check stays inside.
        for needle in ["dosdevices/z:/", "NOT a containment rule"] {
            assert!(out.contains(needle), "{needle:?} is missing from:\n{out}");
        }
    }

    /// The last line an author reads after writing a check is the check echoed
    /// back, and both places that echo one closed the parenthesis with "under
    /// the Proton prefix". Write the path this program's own refusal
    /// recommends — `dosdevices/s:/steamapps/common/…`, the Steam library —
    /// and that line called it a place inside the prefix.
    ///
    /// Both report lines are checked here because they are two separate format
    /// strings that have already drifted once, and the note has to be earned:
    /// a `drive_c/` path must not get it, or it says nothing.
    #[test]
    fn neither_report_line_calls_a_path_through_dosdevices_a_place_in_the_prefix() {
        const OUTSIDE: &str = "dosdevices/s:/steamapps/common/Elite Dangerous/ControlSchemes";
        let dir = scratch("dosdevices-report");
        write_profile(
            &dir,
            "359320",
            &format!(
                "version = 1\n\
                 \n[[check]]\n\
                 format = \"binds-dir\"\n\
                 path = \"drive_c/Bindings\"\n\
                 setting = \"HeadlookMode\"\n\
                 wants = \"1\"\n\
                 tell = \"Set Head Look to Toggle.\"\n\
                 \n[[check]]\n\
                 format = \"binds-dir\"\n\
                 path = \"{OUTSIDE}\"\n\
                 setting = \"HeadlookMode\"\n\
                 wants = \"1\"\n\
                 tell = \"Set Head Look to Toggle.\"\n"
            ),
        );
        let mut shown = String::new();
        profile_show(&mut shown, &dir, &[], &game("359320", Some("Elite"))).expect("show");

        let mut added = String::new();
        profile_check_add(
            &mut added,
            &dir,
            &[],
            &game("999999", Some("Other")),
            &add_argv(
                "999999",
                &[
                    ("--format", "binds-dir"),
                    ("--path", OUTSIDE),
                    ("--setting", "HeadlookMode"),
                    ("--wants", "1"),
                    ("--tell", "Set head look to toggle."),
                ],
            ),
        )
        .expect("add");

        for (what, text) in [("show", &shown), ("check add", &added)] {
            assert!(
                !text.contains("under the Proton prefix"),
                "{what} still calls it a place under the prefix:\n{text}"
            );
            assert!(
                text.contains(PATH_IS_RELATIVE),
                "{what} has to say how the path is joined:\n{text}"
            );
            assert!(
                text.contains("leaves the prefix"),
                "{what} has to say where a dosdevices/ path lands:\n{text}"
            );
        }

        // Earned, not unconditional: the first check in `show` is a plain
        // `drive_c/` path, and a note printed on every line says nothing.
        let (plain, _) = shown.split_once("2. ").expect("two checks are listed");
        assert!(
            plain.contains("drive_c/Bindings") && !plain.contains("leaves the prefix"),
            "a path that does not leave the prefix must not be told it does:\n{plain}"
        );
    }

    /// A profile with one check, a note written above it, and nothing else.
    ///
    /// Spelled out rather than built by `check add`, because the note is the
    /// point and no command writes one: `add` tells the user to, and this is
    /// the file that results.
    const NOTED_CHECK: &str = "version = 1\n\
         name = \"Elite Dangerous\"\n\
         \n# measured 2026-10-01 by playing the game with the bridge running\n\
         [[check]]\n\
         format = \"binds-dir\"\n\
         path = \"drive_c/users/steamuser/Options/Bindings\"\n\
         setting = \"HeadlookMode\"\n\
         wants = \"1\"\n\
         tell = \"Set head look to toggle.\"\n";

    /// The check this feature exists to produce is a check somebody wrote a
    /// `#` note above — `check add` asks for one. Removing it was impossible:
    /// the writer refuses a comment with nowhere to go, so the whole write was
    /// refused, and the only way out was to hand-edit the file that the
    /// authoring commands exist to save you from.
    ///
    /// The note goes with the thing it was about, and comes back in full on
    /// the terminal so a copy survives.
    #[test]
    fn check_remove_takes_the_note_written_about_the_check_out_with_it() {
        let dir = scratch("check-remove-noted");
        write_profile(&dir, "359320", NOTED_CHECK);
        let mut out = String::new();
        profile_check_remove(&mut out, &dir, &[], &game("359320", None), Some("1"))
            .expect("a commented check has to be removable");
        let p = tobii_config::profiles::load_from(&dir, &[], "359320")
            .expect("readable")
            .expect("there")
            .profile;
        assert!(p.checks.is_empty(), "the check is gone");
        assert_eq!(
            p.name.as_deref(),
            Some("Elite Dangerous"),
            "the rest is not"
        );
        let after = std::fs::read_to_string(dir.join("359320.toml")).expect("after");
        assert!(
            !after.contains("measured 2026-10-01"),
            "the note is about a check that is gone:\n{after}"
        );
        assert!(out.contains("NOT kept:"), "{out}");
        assert!(
            out.contains("# measured 2026-10-01 by playing the game with the bridge running"),
            "the note has to come back in full:\n{out}"
        );
    }

    /// A note above a check that stays is not the one being removed, and a
    /// removal that took every comment with it would be the same bug wearing
    /// the other hat.
    #[test]
    fn check_remove_keeps_the_note_written_about_a_check_that_stays() {
        let dir = scratch("check-remove-noted-keep");
        write_profile(
            &dir,
            "359320",
            "version = 1\n\
             \n# about the first\n[[check]]\nformat = \"binds-dir\"\n\
             path = \"drive_c/A\"\nsetting = \"One\"\nwants = \"1\"\ntell = \"Do it.\"\n\
             \n# about the second\n[[check]]\nformat = \"binds-dir\"\n\
             path = \"drive_c/B\"\nsetting = \"Two\"\nwants = \"2\"\ntell = \"Do it.\"\n",
        );
        let mut out = String::new();
        profile_check_remove(&mut out, &dir, &[], &game("359320", None), Some("2"))
            .expect("remove");
        let after = std::fs::read_to_string(dir.join("359320.toml")).expect("after");
        assert!(after.contains("# about the first"), "{after}");
        assert!(!after.contains("# about the second"), "{after}");
        assert!(out.contains("# about the second"), "it comes back:\n{out}");
    }

    /// A `#` written after a value on the removed check's own line cannot be
    /// lifted off without rewriting that line, so the write cannot carry it.
    /// It is handed back instead of refused: refusing made `check remove`
    /// impossible on the only kind of check this feature produces, since
    /// `check add` asks for a `#` line saying how you know.
    ///
    /// Handed back, not dropped. The terminal is the only copy left once the
    /// file is written, so the sentence, its line and what it was about all
    /// have to come back — "a comment was lost" is not enough to put one back
    /// with.
    #[test]
    fn a_comment_the_write_cannot_carry_comes_back_rather_than_stopping_it() {
        let dir = scratch("check-remove-trailing");
        write_profile(
            &dir,
            "359320",
            "version = 1\n\
             \n[[check]]\nformat = \"binds-dir\" # a note about the format\n\
             path = \"drive_c/A\"\nsetting = \"One\"\nwants = \"1\"\ntell = \"Do it.\"\n",
        );
        let mut out = String::new();
        profile_check_remove(&mut out, &dir, &[], &game("359320", None), Some("1"))
            .expect("the removal goes through");
        assert!(
            out.contains("# a note about the format"),
            "the comment itself comes back:\n{out}"
        );
        assert!(out.contains("line 4"), "where it was:\n{out}");
        assert!(out.contains("`format`"), "what it was about:\n{out}");
        let after = std::fs::read_to_string(dir.join("359320.toml")).expect("after");
        assert!(
            !after.contains("a note about the format"),
            "and it really is gone from the file, which is why it had to be \
             printed:\n{after}"
        );
    }

    /// `check add` refuses to promise anything for a format this build has no
    /// reader for. `where` closed with the promise unconditionally, for a
    /// check the hub's window answers `UnknownFormat` about — the two
    /// commands contradicting each other about one check, minutes apart.
    #[test]
    fn check_where_does_not_promise_the_hub_reads_a_format_this_build_cannot() {
        let home = steam_home("check-where-unknown", &[("359320", "Elite")]);
        let dir = home.join("profiles");
        write_profile(
            &dir,
            "359320",
            "version = 1\n\
             \n[[check]]\nformat = \"ini-file\"\npath = \"drive_c/x.ini\"\n\
             setting = \"One\"\nwants = \"1\"\ntell = \"Do it.\"\n",
        );
        let mut out = String::new();
        profile_check_where(&mut out, &dir, &[], &game("359320", Some("Elite")), &home)
            .expect("where");
        assert!(out.contains("no reader for: ini-file"), "{out}");
        assert!(
            out.contains("not that window"),
            "the promise has to be taken back for this check:\n{out}"
        );

        // And it is taken back only for the check it is true of: a profile
        // this build can read keeps the promise whole.
        write_profile(
            &dir,
            "359320",
            "version = 1\n\
             \n[[check]]\nformat = \"binds-dir\"\npath = \"drive_c/A\"\n\
             setting = \"One\"\nwants = \"1\"\ntell = \"Do it.\"\n",
        );
        let mut out = String::new();
        profile_check_where(&mut out, &dir, &[], &game("359320", Some("Elite")), &home)
            .expect("where");
        assert!(out.contains("which carries the"), "{out}");
        assert!(!out.contains("no reader for"), "{out}");
    }

    /// `tobii uninstall --purge` deletes `<appid>.toml.tmp`, so a listing that
    /// names nothing at all for it is the listing disagreeing with the
    /// uninstaller about what is in the directory. It was printed once, as a
    /// stray; moving it out of the strays dropped it from the report entirely.
    #[test]
    fn the_listing_names_the_temp_file_a_cut_short_save_left() {
        let dir = scratch("listing-leftover");
        std::fs::write(dir.join("359320.toml.tmp"), "half a write").expect("leftover");
        let mut out = String::new();
        profile_list(&mut out, &dir, &[]);
        assert!(out.contains("359320.toml.tmp"), "{out}");
        assert!(out.contains("--purge"), "{out}");
        // Not as somebody else's file: that is the sentence it was moved out
        // of the strays to stop being given.
        assert!(!out.contains("and not a profile:\n  359320"), "{out}");
    }

    /// `save` is where somebody lands with no checks, and it sent them to an
    /// editor in the round that gave them a command for it.
    #[test]
    fn save_names_the_command_that_writes_a_check_rather_than_the_file_format() {
        let dir = scratch("save-names-add");
        let mut out = String::new();
        profile_save(
            &mut out,
            &dir,
            &[],
            &game("359320", Some("Elite")),
            &tobii_output::games::OutputConfig::default(),
        )
        .expect("save");
        assert!(
            out.contains("tobii games profile check add 359320"),
            "{out}"
        );
        assert!(!out.contains("by hand."), "{out}");
    }

    /// Both columns here are widths, not limits: a value wider than the column
    /// still ends up with a space after it. A fixed `{:<28}` for the name and
    /// a `{:<10}` an eleven-digit argument outgrew both ran the next field
    /// straight into it.
    #[test]
    fn a_value_wider_than_its_column_is_still_followed_by_a_space() {
        let dir = scratch("listing-wide");
        write_profile(
            &dir,
            "359320",
            "version = 1\nname = \"Stronghold Crusader: Definitive Edition\"\n\
             [settings]\nenabled = \"true\"\n",
        );
        let mut out = String::new();
        profile_list(&mut out, &dir, &[]);
        assert!(
            out.contains("Stronghold Crusader: Definitive Edition 1 setting"),
            "{out}"
        );
        let wide = GameRef {
            appid: "12345678901".to_string(),
            name: Some("Something".to_string()),
            has_prefix: false,
            missing_libraries: String::new(),
        };
        assert!(
            wide.heading().contains("12345678901 Something"),
            "{}",
            wide.heading()
        );
    }
}
