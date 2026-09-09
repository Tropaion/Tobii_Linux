//! The daemon side: accept clients, fan messages out, never block on one.
//!
//! # The load-bearing invariant
//!
//! **The device thread must never block on a client.** It is the thread reading
//! USB, so anything that stalls it stalls gaze for everyone — a client that has
//! been `SIGSTOP`ped, or is merely slow, would otherwise drag the whole
//! pipeline down with it.
//!
//! That is enforced structurally rather than by care: every client has a
//! bounded queue and the publishing side uses `try_send` **only**. A queue that
//! fills means that client is not keeping up, and its frames are dropped and
//! counted. Dropping data for one slow consumer is the correct trade — the next
//! frame is 16 ms away, and a stale one helps nobody.
//!
//! # Threads
//!
//! One accept thread, plus a reader and a writer per client. Two per client
//! because `std` cannot wait on a channel and a socket simultaneously, and
//! polling both with a timeout would put that timeout straight into gaze
//! latency — the one thing this pipeline cannot spend.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::{io, thread};

use crate::codec::{decode, encode, Decoded, Msg};
use crate::path::{ensure_socket_dir, socket_path, stale_socket_action, BindAction};

/// Outbound queue depth per client.
///
/// At 60 Hz this is roughly a second of frames — long enough to ride out a
/// scheduling hiccup, short enough that a client which has genuinely stopped
/// reading cannot accumulate meaningful memory or meaningful staleness.
const QUEUE_DEPTH: usize = 64;

/// Identifies one connected client for the lifetime of the daemon.
pub type ClientId = u64;

/// A message that arrived from a client, tagged with its sender.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    pub from: ClientId,
    pub msg: Msg,
}

/// A client that has connected and said hello.
struct Client {
    id: ClientId,
    name: String,
    subs: u32,
    tx: SyncSender<Vec<u8>>,
    alive: Arc<AtomicBool>,
    dropped: u64,
}

/// Accepts connections and fans messages out to them.
pub struct Server {
    clients: Arc<Mutex<Vec<Client>>>,
    incoming: Receiver<Incoming>,
    path: std::path::PathBuf,
}

/// A snapshot of one client, for status reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    pub id: ClientId,
    pub name: String,
    pub subs: u32,
    pub dropped: u64,
}

impl Server {
    /// Bind the daemon socket and start accepting.
    ///
    /// If the path is already taken, distinguishes a live daemon from a stale
    /// file by trying to connect to it — see
    /// [`stale_socket_action`](crate::path::stale_socket_action).
    pub fn bind() -> io::Result<Server> {
        ensure_socket_dir()?;
        Server::bind_at(&socket_path())
    }

    /// Bind an explicit path. Used by tests, which need their own socket.
    pub fn bind_at(path: &std::path::Path) -> io::Result<Server> {
        crate::path::check_socket_path(path)?;
        let listener = match UnixListener::bind(path) {
            Ok(l) => l,
            Err(e) => {
                let probed = UnixStream::connect(path).is_ok();
                match stale_socket_action(e.kind(), probed) {
                    BindAction::AlreadyRunning => {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            format!(
                                "another tobii serve is already running ({})",
                                path.display()
                            ),
                        ))
                    }
                    BindAction::Unlink => {
                        std::fs::remove_file(path)?;
                        UnixListener::bind(path)?
                    }
                    BindAction::Bind => return Err(e),
                }
            }
        };

        let clients: Arc<Mutex<Vec<Client>>> = Arc::new(Mutex::new(Vec::new()));
        let (in_tx, in_rx) = std::sync::mpsc::channel::<Incoming>();
        let accept_clients = Arc::clone(&clients);

        thread::spawn(move || {
            static NEXT_ID: AtomicU64 = AtomicU64::new(1);
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                let alive = Arc::new(AtomicBool::new(true));
                let (tx, rx) = sync_channel::<Vec<u8>>(QUEUE_DEPTH);

                let Ok(write_half) = stream.try_clone() else {
                    continue;
                };
                spawn_writer(write_half, rx, Arc::clone(&alive));
                spawn_reader(stream, id, in_tx.clone(), Arc::clone(&alive));

                accept_clients.lock().expect("clients").push(Client {
                    id,
                    name: String::new(),
                    subs: 0,
                    tx,
                    alive,
                    dropped: 0,
                });
            }
        });

        Ok(Server {
            clients,
            incoming: in_rx,
            path: path.to_path_buf(),
        })
    }

    /// The bound socket path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Take any messages clients have sent since the last call.
    ///
    /// Never blocks. Also reaps clients whose sockets have closed, so the
    /// caller's view of who is connected stays current without a separate
    /// sweep.
    pub fn poll(&self) -> Vec<Incoming> {
        let mut out = Vec::new();
        while let Ok(msg) = self.incoming.try_recv() {
            // A `Hello` is the one message the server acts on itself: it names
            // the client and says what it wants, which decides who later
            // broadcasts reach.
            if let Msg::Hello { subs, name, .. } = &msg.msg {
                let mut guard = self.clients.lock().expect("clients");
                if let Some(c) = guard.iter_mut().find(|c| c.id == msg.from) {
                    c.subs = *subs;
                    c.name = name.clone();
                }
            }
            out.push(msg);
        }
        self.reap();
        out
    }

    /// Drop clients whose connection has gone.
    fn reap(&self) {
        self.clients
            .lock()
            .expect("clients")
            .retain(|c| c.alive.load(Ordering::Relaxed));
    }

    /// Send to one client.
    pub fn send_to(&self, id: ClientId, msg: &Msg) {
        let bytes = encode(msg);
        let mut guard = self.clients.lock().expect("clients");
        if let Some(c) = guard.iter_mut().find(|c| c.id == id) {
            push(c, bytes);
        }
    }

    /// Send to every client that subscribed to any of `subs`.
    ///
    /// `subs` of `0` means "everyone", used for state messages like `Status`
    /// that a client needs whether or not it asked for data.
    pub fn broadcast(&self, subs: u32, msg: &Msg) {
        let bytes = encode(msg);
        let mut guard = self.clients.lock().expect("clients");
        for c in guard.iter_mut() {
            if subs == 0 || c.subs & subs != 0 {
                push(c, bytes.clone());
            }
        }
    }

    /// How many clients are subscribed to any of `subs`.
    ///
    /// The daemon uses this to subscribe the *device* to the eye-camera stream
    /// only while somebody is actually watching it — 2.6 MB/s of USB bandwidth
    /// is not worth spending on nobody.
    pub fn subscriber_count(&self, subs: u32) -> usize {
        self.clients
            .lock()
            .expect("clients")
            .iter()
            .filter(|c| c.subs & subs != 0)
            .count()
    }

    /// A snapshot of the connected clients.
    pub fn clients(&self) -> Vec<ClientInfo> {
        self.clients
            .lock()
            .expect("clients")
            .iter()
            .map(|c| ClientInfo {
                id: c.id,
                name: c.name.clone(),
                subs: c.subs,
                dropped: c.dropped,
            })
            .collect()
    }
}

impl Drop for Server {
    /// Remove the socket file, so the next start does not have to reason about
    /// whether it is stale.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Queue one encoded message for a client, dropping it if the client is behind.
///
/// This is the whole invariant in one function: `try_send`, never `send`.
fn push(c: &mut Client, bytes: Vec<u8>) {
    match c.tx.try_send(bytes) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => c.dropped = c.dropped.saturating_add(1),
        Err(TrySendError::Disconnected(_)) => c.alive.store(false, Ordering::Relaxed),
    }
}

/// Drain the outbound queue onto the socket.
fn spawn_writer(mut stream: UnixStream, rx: Receiver<Vec<u8>>, alive: Arc<AtomicBool>) {
    thread::spawn(move || {
        while let Ok(bytes) = rx.recv() {
            if stream.write_all(&bytes).is_err() {
                break;
            }
        }
        alive.store(false, Ordering::Relaxed);
        // Wake the reader, which is parked in a blocking read.
        let _ = stream.shutdown(std::net::Shutdown::Both);
    });
}

/// Read framed messages from a client until it goes away.
///
/// Marking the client dead is all this does on the way out; [`Server::poll`]
/// reaps it, which drops the `SyncSender` and lets the writer thread's blocking
/// `recv` return so it exits too.
fn spawn_reader(
    mut stream: UnixStream,
    id: ClientId,
    out: std::sync::mpsc::Sender<Incoming>,
    alive: Arc<AtomicBool>,
) {
    thread::spawn(move || {
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
                        if out.send(Incoming { from: id, msg }).is_err() {
                            return;
                        }
                    }
                    Ok(Decoded::Incomplete) => break,
                    // A desynchronised or hostile stream: drop this client
                    // rather than trying to resynchronise, which cannot be
                    // done reliably on a framed protocol with no sync marker.
                    Err(_) => {
                        alive.store(false, Ordering::Relaxed);
                        return;
                    }
                }
            }
        }
        alive.store(false, Ordering::Relaxed);
    });
}
