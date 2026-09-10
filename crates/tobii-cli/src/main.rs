//! `tobii` CLI. Subcommands: `stream`, `headpose`, `setup`, `display get|set`,
//! `calibrate`.

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
                 tobii stream [--json] [--eyes]\n  \
                 tobii game -- <command> [args...]\n  \
                 tobii games [set KEY VALUE]\n  \
                 tobii bridge install --prefix PATH\n  \
                 tobii bridge run --prefix PATH\n  \
                 tobii headpose [--udp ADDR] [--rate HZ] [--model auto|off|FILE]\n  \
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

    let status = std::process::Command::new(&cmd[0]).args(&cmd[1..]).status();
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
    for src in [&model_store::HEAD_POSE, &model_store::HEAD_LOCALIZER] {
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
/// How long tracking must stay lost before the smoothing filter is reset. A
/// blink drops a handful of frames and should not cause a visible snap when the
/// eyes come back; a genuine absence should not drag a stale pose back in.
const TRACKING_LOSS_RESET: Duration = Duration::from_millis(1000);

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
            if !cfg.apply_key(key, value) {
                // The valid spellings are exactly the keys the file writes, so
                // they are listed from a serialized config rather than from a
                // second hand-maintained list that could drift from it.
                let doc = cfg.to_toml();
                let keys: Vec<&str> = doc
                    .lines()
                    .filter(|l| !l.trim_start().starts_with('#'))
                    .filter_map(|l| l.split_once(" = ").map(|(k, _)| k.trim()))
                    .collect();
                return Err(format!(
                    "{key} = {value:?} was not accepted.\nvalid keys: {}",
                    keys.join(", ")
                )
                .into());
            }
            save_output_config(&cfg)?;
            println!("{key} = {value}");
            Ok(())
        }
        Some(other) => {
            Err(format!("unknown: tobii games {other} (try: tobii games set KEY VALUE)").into())
        }
    }
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
        match tobii_output::sinks::UinputJoystick::open() {
            Ok(s) => router.add(Box::new(s)),
            Err(e) => eprintln!("not presenting a virtual joystick: {e}"),
        }
    }

    let mut model = open_model(&model_choice(args));

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
    let mut samples_since_status = 0u32;
    let mut sends_since_status = 0u32;
    let mut frames_since_status = 0u32;
    // The model's pose is held between camera frames: the camera runs at ~33 Hz
    // and gaze at ~33 Hz, but they are not in lockstep, and dropping pitch to
    // zero on every gaze sample that arrived without a matching image would
    // shake the head in game. Held for at most `TRACKING_LOSS_RESET`.
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
                        .filter(|(_, at)| at.elapsed() < TRACKING_LOSS_RESET)
                        .map(|(m, _)| m);
                    // The model's pose wins where there is one; the pipeline
                    // falls back to the geometric pose otherwise. Tracking loss
                    // stops the send rather than emitting a synthetic pose —
                    // `Router::offer` returns `TrackingLost` and writes nothing,
                    // because opentrack holding its last value is far less
                    // jarring in game than a snap to zero.
                    let at = Instant::now();
                    let frame = pipeline.offer(&sample, fuse_pose(eyes, fresh), &cfg, corners, at);
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
            let rates = format!(
                "{:.0} samples/s, {:.0} frames/s, {:.0} sent/s",
                f64::from(samples_since_status) / elapsed,
                f64::from(frames_since_status) / elapsed,
                f64::from(sends_since_status) / elapsed,
            );
            if check {
                print_check_line(
                    last_frame.as_ref().and_then(|f| f.pose),
                    last_model.as_ref().map(|(m, _)| m),
                    &rates,
                );
            } else {
                match (
                    last_frame.as_ref().is_some_and(|f| f.pose.is_some()),
                    last_frame.as_ref().and_then(|f| f.pose),
                ) {
                    (true, Some(p)) => {
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
                    _ => eprintln!(
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

    /// The keys `tobii games set` accepts must be exactly the keys the file
    /// writes. They are derived from a serialized config rather than listed by
    /// hand precisely so they cannot drift apart — but the derivation has to
    /// skip the file's comment lines, two of which contain `" = "`.
    #[test]
    fn the_settable_keys_are_the_keys_the_file_writes() {
        let doc = tobii_output::games::OutputConfig::default().to_toml();
        let keys: Vec<&str> = doc
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| l.split_once(" = ").map(|(k, _)| k.trim()))
            .collect();
        assert!(keys.contains(&"joystick"), "{keys:?}");
        assert!(
            !keys.iter().any(|k| k.starts_with('#')),
            "the file's comments are not settings: {keys:?}"
        );
        let mut cfg = tobii_output::games::OutputConfig::default();
        for k in &keys {
            // Every listed key must actually be settable, or the error message
            // that lists them is lying about what it accepts.
            let probe = match *k {
                "opentrack" => "127.0.0.1:1",
                "ev_yaw_curve" | "ev_pitch_curve" => "linear",
                _ if k.ends_with("_deg") || *k == "rate_hz" => "1",
                "filter_alpha" => "0.5",
                "ev_hold_ms" => "1",
                "bridge_port" => "1",
                _ => "true",
            };
            assert!(cfg.apply_key(k, probe), "{k} is listed but not settable");
        }
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
}
