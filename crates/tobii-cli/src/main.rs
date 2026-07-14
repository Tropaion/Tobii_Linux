//! `tobii` CLI. Subcommands: `stream`, `setup`, `display get|set`.

use std::io::Write;
use std::process::ExitCode;

use tobii_config::DisplaySetup;
use tobii_protocol::commands::set_display_area_corners_payload;
use tobii_protocol::frame::{OP_GET_DISPLAY_AREA, OP_SET_DISPLAY_AREA};
use tobii_protocol::gaze::present;
use tobii_protocol::DisplayCorners;
use tobii_usb::{Connection, UsbTransport};

type CmdResult = Result<(), Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str);
    let arg2 = args.get(2).map(String::as_str);
    let result = match (sub, arg2) {
        (Some("stream"), _) => stream(args.iter().any(|a| a == "--json")),
        (Some("setup"), _) => setup(),
        (Some("display"), Some("get")) => display_get(),
        (Some("display"), Some("set")) => display_set(),
        _ => {
            eprintln!(
                "usage:\n  \
                 tobii stream [--json]\n  \
                 tobii setup\n  \
                 tobii display get\n  \
                 tobii display set"
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

fn stream(json: bool) -> CmdResult {
    eprintln!("opening Tobii ET5...");
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
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
    let payload = set_display_area_corners_payload(
        c.tl[0], c.tl[1], c.tl[2], c.tr[0], c.tr[1], c.tr[2], c.bl[0], c.bl[1], c.bl[2],
    );
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    conn.request(OP_SET_DISPLAY_AREA, &payload)?;
    println!("display area applied to device:");
    print_corners(&c);
    Ok(())
}

fn prompt_f64(label: &str, default: f64) -> Result<f64, Box<dyn std::error::Error>> {
    print!("{label} [{default}]: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let t = line.trim();
    if t.is_empty() {
        Ok(default)
    } else {
        Ok(t.parse()?)
    }
}

fn setup() -> CmdResult {
    println!("Tobii display setup — enter your monitor geometry.");
    println!("(millimetres; tilt in degrees; press Enter to accept each default)\n");
    let s = DisplaySetup {
        width_mm: prompt_f64("Monitor active-area WIDTH (mm)", 600.0)?,
        height_mm: prompt_f64("Monitor active-area HEIGHT (mm)", 340.0)?,
        tilt_deg: prompt_f64("Screen tilt back from vertical (deg)", 20.0)?,
        offset_y_mm: prompt_f64("Height of screen BOTTOM edge above tracker (mm)", 10.0)?,
        offset_z_mm: prompt_f64("Depth of screen bottom from tracker (mm)", 0.0)?,
        offset_x_mm: prompt_f64("Horizontal offset of screen centre from tracker (mm)", 0.0)?,
    };
    let c = s.to_corners();
    println!("\ncomputed display-area corners (tracker-space mm):");
    print_corners(&c);

    let path = tobii_config::config_path();
    tobii_config::save(&s)?;
    println!("saved config to {}", path.display());

    match UsbTransport::open() {
        Ok(t) => {
            let mut conn = Connection::connect(t)?;
            let payload = set_display_area_corners_payload(
                c.tl[0], c.tl[1], c.tl[2], c.tr[0], c.tr[1], c.tr[2], c.bl[0], c.bl[1], c.bl[2],
            );
            conn.request(OP_SET_DISPLAY_AREA, &payload)?;
            println!("applied to the connected device.");
        }
        Err(e) => {
            eprintln!(
                "note: device not opened ({e}); config saved — run `tobii display set` when connected."
            );
        }
    }
    Ok(())
}
