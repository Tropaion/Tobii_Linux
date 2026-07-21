//! `tobii` CLI. Subcommands: `stream`, `setup`, `display get|set`, `calibrate`.

use std::io::Write;
use std::process::ExitCode;

use tobii_config::DisplaySetup;
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

fn stream(json: bool, eyes: bool) -> CmdResult {
    eprintln!("opening Tobii ET5...");
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;

    // The ET5 resets its display area to a ~4mm stub on every reboot (it reboots
    // on session close), and emits no eye-tracking data until a valid area is
    // set — so re-apply the saved config in-session right after connecting.
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
