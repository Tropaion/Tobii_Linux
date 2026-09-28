//! The provider: receive tracking frames from Linux, publish them to games.
//!
//! **Optional now, and mostly a diagnostic.** The client DLLs feed themselves —
//! see [`feeder`](tobii_bridge_core::feeder) — so nothing has to be left running
//! for a game to get tracking. What this still gives you is a console: it prints
//! what arrives and what is rejected, which is the difference between "the game
//! sees nothing" and "the game sees nothing *because the frames never arrive*".
//! With `--register` it also writes the registry keys, so it doubles as a repair
//! tool for a prefix whose keys were clobbered — see below for why that is not
//! what it does by default.
//!
//! Whoever binds the port first wins; if this is running, the DLLs read the
//! mapping it creates instead of making their own.
//!
//! # Why it does not write the registry any more
//!
//! It used to, on every start, unconditionally — both discovery keys, blind.
//! That is wrong in precisely the configuration this program still exists for.
//! `tobii bridge run` starts it when TrackIR is pointed at a **third-party**
//! client DLL, because that client is a pure consumer of `FT_SharedMem` and
//! something has to fill the mapping; a blind write then replaces that client's
//! registration with `C:\tobii-bridge` and takes away the very thing the user
//! set up. Any restart, or anything that ever auto-starts this, does it again.
//!
//! It is also the one write in the project that answers to no rules. The
//! installer on the Linux side reads each key before it writes it and refuses
//! anything it cannot account for (`crates/tobii-cli/src/bridge.rs`); this had
//! the opposite habit on the same two keys, in a second binary, with no way to
//! read them back.
//!
//! So the default is now to leave the registry alone, and `--register` asks for
//! the old behaviour — still useful as a repair for a prefix whose keys were
//! clobbered, but only when somebody has decided that is what they want.
//! `--no-register` spells the default out, and `tobii bridge run` passes it, so
//! that command cannot start a registering provider whichever way this default
//! ever moves.

use std::net::UdpSocket;

use tobii_bridge_core::{register, shm::Provider, INSTALL_DIR};
use tobii_output::TrackingFrame;

fn main() {
    let mut port = tobii_bridge_core::feeder::port();
    let mut dir = INSTALL_DIR.to_string();
    // Off unless asked. A write here can only ever be a no-op (the installer
    // already put our own path in both keys) or destructive (it replaces a
    // third-party client's registration), and the destructive case is the one
    // this program is normally started for.
    let mut want_register = false;
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
            "--register" => want_register = true,
            "--no-register" => want_register = false,
            "--help" | "-h" => {
                println!("usage: tobii-bridge.exe [--port PORT] [--dir 'C:\\tobii-bridge']");
                println!("                        [--register | --no-register]");
                println!();
                println!("--register writes both discovery keys, pointing them at --dir.");
                println!("Off by default: it overwrites whatever is registered there,");
                println!("including a third-party TrackIR client this bridge exists to feed.");
                println!("--no-register spells the default out.");
                return;
            }
            _ => {}
        }
    }

    // Only on request. See the header: a blind write on every start is how a
    // third-party TrackIR registration disappears, and this program is started
    // mostly *for* that configuration.
    if want_register {
        match register(&dir) {
            Ok(()) => println!("registered TrackIR + FreeTrack client path: {dir}"),
            Err(e) => eprintln!("warning: {e}"),
        }
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
        if received > 0 && received.is_multiple_of(600) {
            println!("{received} frames published, {rejected} rejected");
        }
    }
}
