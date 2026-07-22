//! `tobii-recap` — decode a usbmon pcap capture of the Tobii ET5 into a
//! human-readable TTP op catalog.
//!
//! Usage: `tobii-recap <capture.pcap> [--limit N] [--gaze-columns]`

use std::process::ExitCode;

use tobii_protocol::frame::OP_GAZE_NOTIFY;
use tobii_protocol::gaze::column_inventory;

use tobii_recap::catalog;
use tobii_recap::decode::{decode, TimelineFrame};
use tobii_recap::opnames::op_label;

struct Args {
    path: String,
    limit: Option<usize>,
    gaze_columns: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut path: Option<String> = None;
    let mut limit: Option<usize> = None;
    let mut gaze_columns = false;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--gaze-columns" => gaze_columns = true,
            "--limit" => {
                let v = it
                    .next()
                    .ok_or_else(|| "--limit requires a number".to_string())?;
                limit = Some(
                    v.parse::<usize>()
                        .map_err(|_| format!("invalid --limit value: {v}"))?,
                );
            }
            "-h" | "--help" => return Err("help".to_string()),
            other if other.starts_with('-') => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if path.replace(other.to_string()).is_some() {
                    return Err("only one capture file may be given".to_string());
                }
            }
        }
    }

    Ok(Args {
        path: path.ok_or_else(|| "missing <capture.pcap>".to_string())?,
        limit,
        gaze_columns,
    })
}

fn usage() {
    eprintln!("Usage: tobii-recap <capture.pcap> [--limit N] [--gaze-columns]");
    eprintln!();
    eprintln!("  --limit N        cap the number of timeline lines printed");
    eprintln!("  --gaze-columns   dump the column inventory of each gaze notify (0x500)");
}

fn dir_arrow(dir_in: bool) -> char {
    if dir_in {
        '<'
    } else {
        '>'
    }
}

fn magic_label(magic: u32) -> String {
    match magic {
        0x51 => "REQ".to_string(),
        0x52 => "RSP".to_string(),
        0x53 => "NOTIFY".to_string(),
        other => format!("0x{other:x}"),
    }
}

fn print_timeline_line(tf: &TimelineFrame, gaze_columns: bool) {
    let f = &tf.frame;
    print!(
        "[+{:.6}] {} {} op=0x{:x} {} seq={} len={}",
        tf.ts_rel,
        dir_arrow(tf.dir_in),
        magic_label(f.magic),
        f.op,
        op_label(f.op),
        f.seq,
        f.payload.len(),
    );
    if f.op == OP_GAZE_NOTIFY {
        let inv = column_inventory(&f.payload);
        print!(" columns={}", inv.len());
        if gaze_columns {
            let cols: Vec<String> = inv
                .iter()
                .map(|(c, v)| format!("0x{c:02x}={v:?}"))
                .collect();
            print!(" [{}]", cols.join(", "));
        }
    }
    println!();
}

fn run() -> Result<(), String> {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if e != "help" {
                eprintln!("error: {e}");
            }
            usage();
            return Err(String::new());
        }
    };

    let bytes = std::fs::read(&args.path).map_err(|e| format!("cannot read {}: {e}", args.path))?;
    let result = decode(&bytes).map_err(|e| e.to_string())?;

    // 1. One-line summary.
    println!(
        "capture: linktype={} packets={} bulk-in={} bytes bulk-out={} bytes frames={}",
        result.linktype,
        result.packet_count,
        result.bulk_in_bytes,
        result.bulk_out_bytes,
        result.frames.len(),
    );

    // 2. Timeline.
    println!("\n== timeline ==");
    let shown = match args.limit {
        Some(n) => n.min(result.frames.len()),
        None => result.frames.len(),
    };
    for tf in result.frames.iter().take(shown) {
        print_timeline_line(tf, args.gaze_columns);
    }
    if shown < result.frames.len() {
        println!(
            "... {} more frames (raise --limit)",
            result.frames.len() - shown
        );
    }

    // 3. Op catalog.
    println!("\n== op catalog (unknown ops first) ==");
    println!(
        "{:>3}  {:<8} {:>6}  {:>7} {:>7}  name",
        "dir", "op", "count", "minlen", "maxlen"
    );
    for s in catalog::build(&result.frames) {
        println!(
            "{:>3}  0x{:<6x} {:>6}  {:>7} {:>7}  {}",
            s.dir_marker(),
            s.op,
            s.count,
            s.min_len,
            s.max_len,
            s.name.unwrap_or("UNKNOWN"),
        );
    }

    // Report any non-fatal problems last, so they are not lost above the fold.
    if !result.errors.is_empty() {
        eprintln!("\n== {} decode warning(s) ==", result.errors.len());
        for e in &result.errors {
            eprintln!("  {e}");
        }
    }

    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("error: {msg}");
            }
            ExitCode::FAILURE
        }
    }
}
