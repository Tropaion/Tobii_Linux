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
//!
//! # `--launch`: being the process Proton waits on
//!
//! Steam launches a Proton title with the verb `waitforexitandrun`, and Proton
//! runs `wineserver -w` — a wait for every wine process on the prefix — *before*
//! it spawns the game. Anything of ours started beforehand does not delay that
//! launch, it prevents it. `tobii bridge run` answers by standing down, which
//! stops the freeze and delivers nothing: for a TrackIR title pointed at a
//! third-party client DLL, nothing then fills `FT_SharedMem`.
//!
//! The way out is not to win that race but never to be in it. Steam hands the
//! wrapper the whole command it meant to run, ending in the game's `.exe`. If
//! Proton's target is instead **this program**, with `--launch` and the game
//! after it, Proton is invoked exactly as Steam meant it to be — one
//! `waitforexitandrun`, therefore one `wineserver -w` — and both the provider
//! and the game are born after the lock is taken, inside the session the game
//! itself is in.
//!
//! The shape is [markx86/opentrack-launcher]'s, which is where this project
//! learned the ordering; the mechanism was read, and no code, binary or file of
//! it is used here. It substitutes a `.bat`, and `crates/tobii-cli/src/proton.rs`
//! was written to do the same until that was measured on four Proton builds:
//! `steam.exe` runs an `.exe` target through `CreateProcessW` and **waits**, and
//! a `.bat` target through `ShellExecuteW`, which returns at once. A batch
//! target therefore has Steam record the game as exited about a second in. That
//! is survivable for a launcher whose only job is to start and reap another
//! program — the batch's own `taskkill` still runs when the game ends — and it
//! is not survivable here, because `tobii game` releases the tracker when it
//! returns, so the tracker would go dark a second into every session.
//!
//! Being an `.exe` is the whole of the difference. Three things follow from it:
//!
//! * **Steam's bookkeeping stays correct.** Playtime, the Stop button and the
//!   overlay see a process that ends when the game does.
//! * **The reap is process exit.** A batch needs `taskkill /IM` to remove the
//!   helper, and a surviving helper is exactly the wine process that makes the
//!   *next* launch's `wineserver -w` block. Here the provider is a thread of
//!   the process Proton is waiting on, so it cannot outlive the game.
//! * **The game's command line is an argv, not a line of batch.** A batch file
//!   cannot carry a non-ASCII path — measured — and needs `%` doubled and
//!   quotes counted. None of that applies to a child process.
//!
//! [markx86/opentrack-launcher]: https://github.com/markx86/opentrack-launcher
//!
//! ## What it will not do
//!
//! Stop the game from starting. Every failure on the provider's side — the port
//! already bound, the mapping refused, anything — is a warning, and the game is
//! launched regardless. A wrapper that can turn a head-tracking problem into a
//! game that will not start is worse than no wrapper, and this is the one place
//! in the program with the game's launch in its hands.
//!
//! It also does not establish that a game then *receives* tracking. It gets a
//! provider running in the right session at the right time; whether a given
//! title reads `FT_SharedMem` afterwards is not something this can check, and
//! nothing it prints may suggest it did.

use std::net::UdpSocket;
use std::process::Command;

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
    // Everything after `--launch` is the game, verbatim, and the flag is
    // terminal for that reason: a game's own arguments are not ours to read,
    // and one of them being spelled `--port` must not reach the parser above.
    let mut launch: Vec<String> = Vec::new();
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
            "--launch" => {
                launch.extend(args.by_ref());
                break;
            }
            "--register" => want_register = true,
            "--no-register" => want_register = false,
            "--help" | "-h" => {
                println!("usage: tobii-bridge.exe [--port PORT] [--dir 'C:\\tobii-bridge']");
                println!("                        [--register | --no-register]");
                println!("                        [--launch GAME.EXE [ARGS...]]");
                println!();
                println!("--register writes both discovery keys, pointing them at --dir.");
                println!("Off by default: it overwrites whatever is registered there,");
                println!("including a third-party TrackIR client this bridge exists to feed.");
                println!("--no-register spells the default out.");
                println!();
                println!("--launch starts the provider, then runs GAME.EXE and waits for it,");
                println!("then exits with its exit code. Everything after --launch belongs to");
                println!("the game. Meant to be what Proton is pointed at, so that the");
                println!("provider and the game share one wineserver session; see the module");
                println!("docs. A provider that cannot start never stops the game starting.");
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

    // Opened before the game is started, not after: the client DLL a game
    // loads takes the port if nothing else has it, and the ordering this
    // program exists to fix is only fixed if the provider is up first.
    let launching = !launch.is_empty();
    let served = match open(port, launching) {
        Ok(pair) => Some(pair),
        Err(e) => {
            eprintln!("error: {e}");
            // Fatal when starting the provider is the whole of the job, and a
            // warning when a game's launch is waiting behind it. See the
            // module docs: this must never be why a game did not start.
            if !launching {
                std::process::exit(1);
            }
            eprintln!("warning: starting the game anyway — it will get no tracking from us");
            None
        }
    };

    if launching {
        if let Some((provider, socket)) = served {
            std::thread::spawn(move || serve(provider, socket));
        }
        std::process::exit(run_game(&launch));
    }

    if let Some((provider, socket)) = served {
        serve(provider, socket);
    }
}

/// The mapping and the socket, or why not.
///
/// Both, or neither: a provider holding the port without a mapping to publish
/// into would take the port from a client DLL that could have served itself.
fn open(port: u16, launching: bool) -> Result<(Provider, UdpSocket), String> {
    let provider = Provider::create()?;
    println!("FT_SharedMem created");
    let socket = UdpSocket::bind(("127.0.0.1", port)).map_err(|e| {
        format!(
            "could not bind 127.0.0.1:{port}: {e}\n       \
             is another tobii-bridge already running in this prefix?"
        )
    })?;
    // Ctrl-C is the answer when somebody started this themselves. Under
    // `--launch` there is no console to press it in and the game's exit is what
    // ends this process.
    let stop = if launching {
        "until the game exits"
    } else {
        "\u{2014} Ctrl-C to stop"
    };
    println!("listening on 127.0.0.1:{port} {stop}");
    Ok((provider, socket))
}

/// Run the game and wait for it, and report what it exited with.
///
/// Its exit code is this process's, because Steam reads it: a wrapper that
/// swallowed a crash would have the library show a clean exit for a game that
/// fell over.
fn run_game(cmd: &[String]) -> i32 {
    let Some((exe, args)) = cmd.split_first() else {
        eprintln!("error: --launch was given nothing to run");
        return 1;
    };
    println!("starting {exe}");
    match Command::new(exe).args(args).status() {
        Ok(status) => status.code().unwrap_or(0),
        Err(e) => {
            eprintln!("error: could not start {exe}: {e}");
            1
        }
    }
}

/// Publish what arrives, forever.
fn serve(provider: Provider, socket: UdpSocket) {
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
