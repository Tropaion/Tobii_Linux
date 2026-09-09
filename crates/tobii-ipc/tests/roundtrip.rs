//! Server + client over real Unix sockets.
//!
//! The codec is unit-tested against byte literals; what needs a live socket is
//! the behaviour around it — that a broadcast reaches everyone who asked for it,
//! that a client which stops reading is dropped rather than allowed to block the
//! server, and that a client going away deregisters itself.
//!
//! Each test binds its own socket under the temp dir, so they neither collide
//! with each other nor with a real daemon.

use std::time::{Duration, Instant};

use tobii_ipc::codec::Msg;
use tobii_ipc::{subs, Client, Server, StatusCode};

/// A unique socket path for one test.
fn sock(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("tobii-ipc-test-{name}.sock"));
    let _ = std::fs::remove_file(&p);
    p
}

/// How long a condition gets before a test gives up on it.
///
/// Deliberately generous. These are polling loops that return the instant the
/// condition holds, so a large budget costs nothing when the machine is idle —
/// and the alternative is a suite that fails when something else on the box is
/// busy, which teaches everyone to ignore it.
const PATIENCE: Duration = Duration::from_secs(20);

/// Poll `f` until it returns `Some`, or give up after `limit`.
///
/// Threads make timing nondeterministic; polling a condition is how this repo
/// avoids the alternative, which is a fixed sleep long enough to be slow and
/// short enough to be flaky.
fn wait_for<T>(limit: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if let Some(v) = f() {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

/// Wait until `n` clients have actually said hello.
///
/// Registration and `Hello` are two separate events: the accept thread adds a
/// client the moment the socket connects, but its subscriptions and name only
/// exist once `poll` has processed its `Hello`. Waiting on `clients().len()`
/// alone therefore returns while `subs` is still 0 — a race that is invisible on
/// an idle machine and reliable under load.
///
/// Every test client passes a non-empty name, so a populated name is the signal
/// that the handshake completed.
fn wait_for_clients(server: &Server, n: usize) {
    wait_for(PATIENCE, || {
        server.poll();
        let greeted = server
            .clients()
            .iter()
            .filter(|c| !c.name.is_empty())
            .count();
        (greeted >= n).then_some(())
    })
    .expect("clients should register and say hello");
}

#[test]
fn a_broadcast_reaches_every_subscriber() {
    let path = sock("broadcast");
    let server = Server::bind_at(&path).expect("bind");

    let a = Client::connect_at(&path, subs::GAZE, "a").expect("connect a");
    let b = Client::connect_at(&path, subs::GAZE, "b").expect("connect b");
    wait_for_clients(&server, 2);

    let msg = Msg::Notify {
        op: 0x500,
        payload: vec![1, 2, 3],
    };
    server.broadcast(subs::GAZE, &msg);

    for (name, c) in [("a", &a), ("b", &b)] {
        let got = c
            .recv_timeout(PATIENCE)
            .unwrap_or_else(|| panic!("{name} received nothing"));
        assert_eq!(got, msg, "{name} got the wrong message");
    }
}

/// Subscriptions must actually filter. The camera stream is 2.6 MB/s, so
/// sending it to a client that did not ask would be expensive, not merely
/// untidy.
#[test]
fn a_broadcast_skips_clients_that_did_not_subscribe() {
    let path = sock("filter");
    let server = Server::bind_at(&path).expect("bind");

    let gaze_only = Client::connect_at(&path, subs::GAZE, "gaze").expect("connect");
    let camera = Client::connect_at(&path, subs::CAMERA, "camera").expect("connect");
    wait_for_clients(&server, 2);

    assert_eq!(server.subscriber_count(subs::CAMERA), 1);
    assert_eq!(server.subscriber_count(subs::GAZE), 1);

    let cam_msg = Msg::Notify {
        op: 0x501,
        payload: vec![9; 32],
    };
    server.broadcast(subs::CAMERA, &cam_msg);

    assert_eq!(
        camera.recv_timeout(PATIENCE),
        Some(cam_msg),
        "the subscriber must receive it"
    );
    assert_eq!(
        gaze_only.recv_timeout(Duration::from_millis(300)),
        None,
        "a non-subscriber must not"
    );
}

/// `Status` goes to everyone regardless of subscription: a client needs to know
/// the tracker is gone whether or not it asked for data.
#[test]
fn a_zero_subscription_broadcast_reaches_everyone() {
    let path = sock("everyone");
    let server = Server::bind_at(&path).expect("bind");
    let c = Client::connect_at(&path, 0, "silent").expect("connect");
    wait_for_clients(&server, 1);

    let msg = Msg::Status {
        code: StatusCode::Leased,
        text: "held by tobii-gtk".to_string(),
    };
    server.broadcast(0, &msg);
    assert_eq!(c.recv_timeout(PATIENCE), Some(msg));
}

#[test]
fn a_client_message_reaches_the_server_tagged_with_its_sender() {
    let path = sock("inbound");
    let server = Server::bind_at(&path).expect("bind");
    let mut c = Client::connect_at(&path, subs::POSE, "gtk").expect("connect");
    wait_for_clients(&server, 1);

    c.send(&Msg::Lease(tobii_ipc::LeaseAction::Acquire))
        .expect("send");

    let got = wait_for(PATIENCE, || {
        server
            .poll()
            .into_iter()
            .find(|i| matches!(i.msg, Msg::Lease(_)))
    })
    .expect("the server should receive the lease request");

    assert_eq!(got.msg, Msg::Lease(tobii_ipc::LeaseAction::Acquire));
    let info = server.clients();
    assert_eq!(info.len(), 1);
    assert_eq!(info[0].id, got.from, "the message names its sender");
    assert_eq!(info[0].name, "gtk", "hello populated the client's name");
    assert_eq!(info[0].subs, subs::POSE);
}

/// **The load-bearing test.** A client that stops reading must not be able to
/// slow the publisher down.
///
/// The proof is `dropped > 0`, not elapsed time. Once far more messages than the
/// queue depth have been offered to a client that never reads, a `try_send`
/// implementation *must* have dropped some — whereas a blocking `send` could
/// not have dropped any, and would still be waiting. That is a structural
/// property of the two implementations rather than a statement about how fast
/// this machine happens to be, so it holds under any load.
///
/// Reaching the assertion at all is the second half: a blocking send would never
/// return, and the harness would kill the run.
#[test]
fn a_client_that_stops_reading_never_blocks_the_server() {
    let path = sock("slowclient");
    let server = Server::bind_at(&path).expect("bind");

    // Connect, say hello, then never read again.
    let stalled = Client::connect_at(&path, subs::GAZE, "stalled").expect("connect");
    wait_for_clients(&server, 1);

    let msg = Msg::Notify {
        op: 0x500,
        payload: vec![7; 1024],
    };
    // Far more than the queue depth and the socket buffer can hold together.
    for _ in 0..5_000 {
        server.broadcast(subs::GAZE, &msg);
    }

    let dropped: u64 = server.clients().iter().map(|c| c.dropped).sum();
    assert!(
        dropped > 0,
        "5000 messages went to a client that never read and none was dropped — \
         that is only possible if the server blocked waiting for it"
    );

    drop(stalled);
}

/// A client going away must deregister, or the daemon would keep queueing for a
/// corpse and its camera-subscriber count would never fall back to zero.
#[test]
fn a_disconnected_client_is_reaped() {
    let path = sock("reap");
    let server = Server::bind_at(&path).expect("bind");

    let c = Client::connect_at(&path, subs::GAZE, "transient").expect("connect");
    wait_for_clients(&server, 1);
    assert_eq!(server.subscriber_count(subs::GAZE), 1);

    drop(c);

    let gone = wait_for(PATIENCE, || {
        server.poll();
        server.clients().is_empty().then_some(())
    });
    assert!(gone.is_some(), "the client should have been reaped");
    assert_eq!(server.subscriber_count(subs::GAZE), 0);
}

/// Two daemons must not both think they own the tracker. The second one has to
/// fail, and say where the first one is listening.
#[test]
fn a_second_server_on_the_same_path_refuses_to_start() {
    let path = sock("double");
    let _first = Server::bind_at(&path).expect("first binds");

    // Not `expect_err`: that would require `Server: Debug`, and a server owning
    // live threads and sockets has nothing useful to print.
    let err = match Server::bind_at(&path) {
        Ok(_) => panic!("a second server on the same path must fail"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    assert!(
        err.to_string().contains(&path.display().to_string()),
        "the error should name the socket: {err}"
    );
}

/// A socket file outlives the process that made it, so a daemon killed with
/// SIGKILL leaves one behind. The next start must clear it rather than refusing
/// to run forever.
#[test]
fn a_stale_socket_file_is_cleared_and_rebound() {
    let path = sock("stale");
    // A leftover file with nobody listening.
    std::fs::write(&path, b"not a socket").expect("write");
    assert!(path.exists());

    let server = Server::bind_at(&path).expect("a stale file must not block startup");
    let c = Client::connect_at(&path, subs::GAZE, "after-stale").expect("connect");
    wait_for_clients(&server, 1);
    drop(c);
}

/// Dropping the server removes its socket, so the next start does not even have
/// to reason about staleness.
#[test]
fn dropping_the_server_removes_its_socket() {
    let path = sock("cleanup");
    {
        let _server = Server::bind_at(&path).expect("bind");
        assert!(path.exists(), "the socket should exist while bound");
    }
    assert!(!path.exists(), "the socket should be gone after drop");
}
