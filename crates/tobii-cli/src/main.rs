//! `tobii` CLI. Subcommands: `stream`, `headpose`, `setup`, `display get|set`,
//! `calibrate`.

use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use tobii_config::DisplaySetup;
use tobii_headpose::{opentrack, pose_from_sample, PoseFilter};
use tobii_protocol::frame::OP_GET_DISPLAY_AREA;
use tobii_protocol::gaze::present;
use tobii_protocol::{DisplayCorners, EnabledEye};
use tobii_usb::{Connection, UsbTransport};

type CmdResult = Result<(), Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str);
    let arg2 = args.get(2).map(String::as_str);
    let result = match (sub, arg2) {
        (Some("stream"), _) => stream(
            args.iter().any(|a| a == "--json"),
            args.iter().any(|a| a == "--eyes"),
        ),
        (Some("headpose"), _) => headpose(&args),
        (Some("columns"), _) => columns(),
        (Some("probe-streams"), _) => probe_streams(&args),
        (Some("probe-stream"), _) => probe_stream(&args),
        (Some("dump-stream"), _) => dump_stream(&args),
        (Some("setup"), _) => setup(),
        (Some("display"), Some("get")) => display_get(),
        (Some("display"), Some("set")) => display_set(),
        (Some("calibrate"), _) => calibrate(args.iter().any(|a| a == "--apply")),
        (Some("cal-probe"), _) => cal_probe(),
        (Some("enabled-eye"), arg) => enabled_eye_cmd(arg),
        _ => {
            eprintln!(
                "usage:\n  \
                 tobii stream [--json] [--eyes]\n  \
                 tobii headpose [--udp ADDR] [--rate HZ]\n  \
                 tobii columns\n  \
                 tobii probe-streams [START] [END]\n  \
                 tobii probe-stream <ID> [SECS]\n  \
                 tobii dump-stream <ID> [COUNT]\n  \
                 tobii setup\n  \
                 tobii display get\n  \
                 tobii display set\n  \
                 tobii calibrate [--apply]\n  \
                 tobii cal-probe\n  \
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
        Err(e) => println!("  calibration_start (0x3f2): FAILED ({e})"),
    }
    match conn.stop_calibration() {
        Ok(()) => println!("  calibration_stop  (0x3fc): ACK"),
        Err(e) => println!("  calibration_stop  (0x3fc): FAILED ({e})"),
    }
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

    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    reapply_display_area(&mut conn);
    conn.set_request_timeout(Duration::from_millis(300));
    conn.subscribe_stream(id)?;
    eprintln!("capturing {count} frame(s) of stream 0x{id:03x} to /tmp (sit in view)...");

    let mut saved = 0u32;
    let deadline = Instant::now() + Duration::from_secs(20);
    while saved < count && Instant::now() < deadline {
        for (op, payload) in conn.read_notifications() {
            if op != id as u32 {
                continue;
            }
            let path = format!("/tmp/stream-{id:03x}-{saved}.bin");
            std::fs::write(&path, &payload)?;
            println!("wrote {path} ({} bytes)", payload.len());
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
                println!(
                    "                trackbox L=({:.3}, {:.3})  R=({:.3}, {:.3})   \
                     origin L=({:.0}, {:.0}, {:.0})mm  R=({:.0}, {:.0}, {:.0})mm",
                    s.trackbox_eye_l[0],
                    s.trackbox_eye_l[1],
                    s.trackbox_eye_r[0],
                    s.trackbox_eye_r[1],
                    s.eye_origin_l_mm[0],
                    s.eye_origin_l_mm[1],
                    s.eye_origin_l_mm[2],
                    s.eye_origin_r_mm[0],
                    s.eye_origin_r_mm[1],
                    s.eye_origin_r_mm[2],
                );
            }
        } else {
            println!("t={:>12}  (no 2D gaze this frame)", s.timestamp_us);
        }
    }
}

/// opentrack's default "UDP over network" endpoint.
const DEFAULT_UDP_ADDR: &str = "127.0.0.1:4242";
/// Datagrams per second when `--rate` is not given.
const DEFAULT_RATE_HZ: f64 = 60.0;
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
fn parse_rate(raw: &str) -> Result<f64, Box<dyn std::error::Error>> {
    match raw.parse::<f64>() {
        Ok(hz) if hz.is_finite() && hz > 0.0 => Ok(hz),
        _ => Err(format!("--rate needs a positive number of Hz, got `{raw}`").into()),
    }
}

/// Stream a derived head pose to opentrack over UDP.
///
/// Pitch is always zero — it cannot be recovered from two eye positions. See
/// the `tobii-headpose` crate docs for the geometry and the (still unvalidated)
/// sign conventions.
fn headpose(args: &[String]) -> CmdResult {
    let addr = parse_udp_addr(flag_value(args, "--udp").unwrap_or(DEFAULT_UDP_ADDR))?;
    let rate_hz = match flag_value(args, "--rate") {
        Some(raw) => parse_rate(raw)?,
        None => DEFAULT_RATE_HZ,
    };
    let send_interval = Duration::from_secs_f64(1.0 / rate_hz);

    // Bind an ephemeral local port; opentrack only ever receives from us.
    let socket = UdpSocket::bind("0.0.0.0:0")?;

    eprintln!("opening Tobii ET5...");
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    reapply_display_area(&mut conn);

    eprintln!("sending head pose to {addr} at {rate_hz:.0} Hz (Ctrl-C to stop)");
    eprintln!(
        "note: pitch is always 0 — it is not derivable from two eye positions, \
         and the device's own head-pose stream is not mapped yet."
    );

    let mut filter = PoseFilter::default();
    let now = Instant::now();
    let (mut last_send, mut last_status) = (now, now);
    let mut last_tracked: Option<Instant> = None;
    let mut samples_since_status = 0u32;
    let mut sends_since_status = 0u32;

    loop {
        // Drain every sample the device offers and feed them all to the filter;
        // `--rate` throttles what goes on the wire, not what we smooth over.
        if let Some(sample) = conn.next_gaze() {
            samples_since_status += 1;
            match pose_from_sample(&sample) {
                Some(raw) => {
                    last_tracked = Some(Instant::now());
                    let pose = filter.update(raw);
                    if last_send.elapsed() >= send_interval {
                        socket.send_to(&opentrack::to_opentrack_datagram(&pose), addr)?;
                        last_send = Instant::now();
                        sends_since_status += 1;
                    }
                }
                None => {
                    // Tracking lost. Stop sending rather than emitting a
                    // synthetic pose: opentrack simply holds its last value,
                    // which is far less jarring in game than a snap to zero.
                    let lost_for = last_tracked.map(|t| t.elapsed());
                    if lost_for.is_none_or(|d| d >= TRACKING_LOSS_RESET) {
                        filter.reset();
                        last_tracked = None;
                    }
                }
            }
        }

        if last_status.elapsed() >= STATUS_INTERVAL {
            let elapsed = last_status.elapsed().as_secs_f64();
            match (last_tracked.is_some(), filter.current()) {
                (true, Some(p)) => eprintln!(
                    "pos=({:>7.1}, {:>7.1}, {:>7.1})mm  yaw={:>6.1}°  roll={:>6.1}°  \
                     pitch=n/a   {:.0} samples/s, {:.0} sent/s",
                    p.x_mm,
                    p.y_mm,
                    p.z_mm,
                    p.yaw_deg,
                    p.roll_deg,
                    f64::from(samples_since_status) / elapsed,
                    f64::from(sends_since_status) / elapsed,
                ),
                _ => eprintln!(
                    "NO HEAD DETECTED — both eyes must be in the trackbox  \
                     ({:.0} samples/s, not sending)",
                    f64::from(samples_since_status) / elapsed
                ),
            }
            last_status = Instant::now();
            samples_since_status = 0;
            sends_since_status = 0;
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
        let blob = tobii_config::load_calibration()?
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
        "NOTE: headless calibration — no stimulus is drawn, so this validates the \
         protocol only, not gaze accuracy."
    );
    for (i, &(x, y)) in CAL_POINTS.iter().enumerate() {
        conn.add_calibration_point(x, y, 0)?;
        println!(
            "  point {}/{} at ({x:.2}, {y:.2}) sampled",
            i + 1,
            CAL_POINTS.len()
        );
    }
    conn.compute_and_apply_calibration()?;
    let blob = conn.retrieve_calibration()?;
    tobii_config::save_calibration(&blob.0)?;
    println!(
        "calibration computed + applied; saved {} bytes.",
        blob.0.len()
    );
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
        w_def = m.width_mm;
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
        assert_eq!(addr.port(), opentrack::DEFAULT_PORT);
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

    #[test]
    fn default_rate_yields_a_sane_send_interval() {
        let interval = Duration::from_secs_f64(1.0 / DEFAULT_RATE_HZ);
        assert!(interval > Duration::ZERO && interval < STATUS_INTERVAL);
    }
}
