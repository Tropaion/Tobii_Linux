//! The client side: connect to the daemon, send, receive.
//!
//! Connecting is expected to fail — the daemon may simply not be running, which
//! is the normal state for anyone who has not opted into it. So [`Client::connect`]
//! returns an ordinary error and callers fall back to opening the device
//! directly. [`backoff`] shapes the retry so a missing daemon costs a poll every
//! couple of seconds rather than a spin.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;
use std::{io, thread};

use crate::codec::{decode, encode, Decoded, Msg};
use crate::path::socket_path;

/// First retry delay.
pub const BACKOFF_MIN: Duration = Duration::from_millis(250);

/// Longest retry delay.
pub const BACKOFF_MAX: Duration = Duration::from_millis(2000);

/// The next reconnect delay after `prev`, doubling up to [`BACKOFF_MAX`].
///
/// Bounded rather than unbounded because a daemon that starts later should be
/// picked up promptly — an exponential backoff with no ceiling would leave a
/// GUI ignoring a running daemon for minutes.
pub fn backoff(prev: Option<Duration>) -> Duration {
    match prev {
        None => BACKOFF_MIN,
        Some(d) => (d * 2).min(BACKOFF_MAX),
    }
}

/// A connection to the daemon.
pub struct Client {
    stream: UnixStream,
    rx: Receiver<Msg>,
    alive: Arc<AtomicBool>,
}

impl Client {
    /// Connect to the default socket path.
    pub fn connect(subs: u32, name: &str) -> io::Result<Client> {
        Client::connect_at(&socket_path(), subs, name)
    }

    /// Connect to an explicit path, and say hello.
    pub fn connect_at(path: &std::path::Path, subs: u32, name: &str) -> io::Result<Client> {
        crate::path::check_socket_path(path)?;
        let stream = UnixStream::connect(path)?;
        let read_half = stream.try_clone()?;
        let alive = Arc::new(AtomicBool::new(true));
        let (tx, rx) = channel::<Msg>();
        let reader_alive = Arc::clone(&alive);

        thread::spawn(move || {
            let mut stream = read_half;
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
                loop {
                    match decode(&buf) {
                        Ok(Decoded::Message(msg, used)) => {
                            buf.drain(..used);
                            if tx.send(msg).is_err() {
                                return;
                            }
                        }
                        Ok(Decoded::Incomplete) => break,
                        Err(_) => {
                            reader_alive.store(false, Ordering::Relaxed);
                            return;
                        }
                    }
                }
            }
            reader_alive.store(false, Ordering::Relaxed);
        });

        let mut client = Client { stream, rx, alive };
        client.send(&Msg::Hello {
            version: crate::PROTO_VERSION,
            subs,
            name: name.to_string(),
        })?;
        Ok(client)
    }

    /// Send one message.
    pub fn send(&mut self, msg: &Msg) -> io::Result<()> {
        self.stream.write_all(&encode(msg))
    }

    /// Take whatever has arrived, without blocking.
    pub fn poll(&self) -> Vec<Msg> {
        let mut out = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            out.push(m);
        }
        out
    }

    /// Block for the next message, up to `timeout`.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Msg> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// False once the daemon has gone away.
    pub fn is_connected(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

impl Drop for Client {
    /// Actually disconnect.
    ///
    /// The reader thread holds a `try_clone`'d descriptor for the same socket
    /// and is parked in a blocking `read`, so dropping the `UnixStream` alone
    /// closes nothing: the kernel keeps the connection open for the surviving
    /// descriptor, the daemon never sees EOF, and it goes on queueing frames
    /// for a client that is gone. An explicit shutdown wakes the reader and
    /// tears the connection down for real.
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_starts_small_doubles_and_stops_at_the_ceiling() {
        let mut d = backoff(None);
        assert_eq!(d, BACKOFF_MIN);
        let mut seen = vec![d];
        for _ in 0..10 {
            let next = backoff(Some(d));
            assert!(next >= d, "backoff must not shrink");
            d = next;
            seen.push(d);
        }
        assert_eq!(d, BACKOFF_MAX, "must settle at the ceiling");
        assert!(seen.windows(2).all(|w| w[1] <= BACKOFF_MAX));
    }

    /// A daemon started after the client must be picked up promptly. An
    /// unbounded exponential backoff would leave the GUI ignoring a running
    /// daemon for minutes.
    #[test]
    fn the_backoff_ceiling_is_short_enough_to_notice_a_late_daemon() {
        assert!(BACKOFF_MAX <= Duration::from_secs(2));
    }

    /// The daemon not running is the normal state for anyone who has not opted
    /// in, so it must be an ordinary error the caller can fall back from.
    #[test]
    fn connecting_with_no_daemon_is_an_ordinary_error() {
        let path = std::env::temp_dir().join("tobii-ipc-test-absent.sock");
        let _ = std::fs::remove_file(&path);
        assert!(Client::connect_at(&path, 0, "test").is_err());
    }
}
