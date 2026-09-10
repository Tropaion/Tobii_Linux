//! The provider: receive tracking frames from Linux, publish them to games.
//!
//! **Optional now, and mostly a diagnostic.** The client DLLs feed themselves —
//! see [`feeder`](tobii_bridge_core::feeder) — so nothing has to be left running
//! for a game to get tracking. What this still gives you is a console: it prints
//! what arrives and what is rejected, which is the difference between "the game
//! sees nothing" and "the game sees nothing *because the frames never arrive*".
//! It also writes the registry keys, so it doubles as a repair tool for a prefix
//! whose keys were clobbered.
//!
//! Whoever binds the port first wins; if this is running, the DLLs read the
//! mapping it creates instead of making their own.

use std::net::UdpSocket;

use tobii_bridge_core::{register, shm::Provider, INSTALL_DIR};
use tobii_output::TrackingFrame;

fn main() {
    let mut port = tobii_bridge_core::feeder::port();
    let mut dir = INSTALL_DIR.to_string();
    // A flag's value is consumed whether or not it parses, so `--port nonsense`
    // keeps the default rather than trying to read `nonsense` as the next flag.
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                if let Some(v) = args.next().and_then(|v| v.parse().ok()) {
                    port = v;
                }
            }
            "--dir" => {
                if let Some(v) = args.next() {
                    dir = v;
                }
            }
            "--help" | "-h" => {
                println!("usage: tobii-bridge.exe [--port PORT] [--dir 'C:\\tobii-bridge']");
                return;
            }
            _ => {}
        }
    }

    // Written at startup rather than only by the installer, so the keys always
    // describe wherever the DLLs actually are.
    match register(&dir) {
        Ok(()) => println!("registered TrackIR + FreeTrack client path: {dir}"),
        Err(e) => eprintln!("warning: {e}"),
    }

    let provider = match Provider::create() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    println!("FT_SharedMem created");

    let socket = match UdpSocket::bind(("127.0.0.1", port)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not bind 127.0.0.1:{port}: {e}");
            eprintln!("       is another tobii-bridge already running in this prefix?");
            std::process::exit(1);
        }
    };
    println!("listening on 127.0.0.1:{port} — Ctrl-C to stop");

    let mut buf = [0u8; 512];
    let mut received: u64 = 0;
    let mut rejected: u64 = 0;
    let mut warned = false;

    loop {
        let Ok((n, _from)) = socket.recv_from(&mut buf) else {
            continue;
        };
        match TrackingFrame::decode(&buf[..n]) {
            Ok(frame) => {
                received += 1;
                provider.publish(
                    &frame,
                    tobii_bridge_core::feeder::GAME_ID.load(std::sync::atomic::Ordering::Relaxed),
                );
                if received == 1 {
                    println!("first frame received ({n} bytes); publishing to FT_SharedMem");
                }
            }
            Err(e) => {
                rejected += 1;
                // Say it once. A version mismatch means every frame is wrong in
                // the same way, and a message per datagram at 60 Hz would bury
                // the one line that explains it.
                if !warned {
                    warned = true;
                    eprintln!("warning: {e}");
                    eprintln!("         ignoring further malformed datagrams");
                }
            }
        }
        if received > 0 && received % 600 == 0 {
            println!("{received} frames published, {rejected} rejected");
        }
    }
}
