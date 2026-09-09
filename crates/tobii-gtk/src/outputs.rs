//! A socket subscriber is a reason to keep the tracker on.
//!
//! # The one idea
//!
//! Only one process can claim the ET5 over USB, so anything that wants tracking
//! data while the hub is running has to get it *from* the hub. That could have
//! been a second daemon owning the device, with the hub as one of its clients —
//! and it was, on an older branch. This is the other way round, and the reason
//! is [`Demand`].
//!
//! The hub already answers "should the tracker be on?" with a reference count:
//! the window while it has focus, the gaze overlay while it is shown, a flow
//! while it runs. A connected socket client simply takes one of those counts.
//! There is no second notion of who wants the device, no timeout inferring it,
//! and no clock to get wrong — a client that connects lights the tracker, a
//! client that dies closes its socket and the guard drops with it. The kernel
//! is the liveness check.
//!
//! That is why the answer to "why are the illuminators on?" is still one line
//! in one file, `device.rs`'s `while !thread_demand.active()`, and this module
//! did not have to change it.
//!
//! # What this does not do yet
//!
//! Nothing is published. No frames, no lease, no game output — this is the
//! demand seam alone. A shipped build behaves exactly as it does today, because
//! nothing in the tree connects to the socket; the only user-visible difference
//! is a line in the log when the socket was already taken.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tobii_ipc::{subs, ClientId, Server, StatusCode};

use crate::device::{ConnStatus, Demand, DemandGuard, DeviceState};

/// How often the socket is drained and the client list reconciled.
///
/// A disconnect is noticed within this, so it is also how long the tracker can
/// stay lit after the last client dies. Short enough not to be noticed, long
/// enough that an idle hub is not polling a socket at frame rate.
const POLL: Duration = Duration::from_millis(50);

/// Why the tracker is on, for a client with these subscriptions.
///
/// `Demand::hold` takes a `&'static str` because the reason is shown to the
/// user — so these are literals rather than anything built from the client's
/// own name, which is untrusted text arriving over a socket.
///
/// `None` for a client subscribed to nothing: connecting is not by itself a
/// reason to run the tracker, and a client that has said "send me nothing"
/// should not be lighting an infrared lamp. That is also how a client lowers
/// its own demand without disconnecting — the server acts on every `Hello`, so
/// re-sending one with no bits gives the device back.
fn reason_for(bits: u32) -> Option<&'static str> {
    if bits & subs::POSE != 0 {
        Some("head tracking for another program")
    } else if bits & subs::GAZE != 0 {
        Some("another program watching your gaze")
    } else if bits & subs::CAMERA != 0 {
        Some("another program watching the eye camera")
    } else {
        None
    }
}

/// What a client should be told the tracker is doing.
fn status_of(state: &DeviceState) -> (StatusCode, String) {
    match &state.status {
        // `Idle` is not `Connecting`, and the difference is the whole standby
        // model: a client that cannot tell "off on purpose" from "trying"
        // renders "Connecting…" forever while the tracker sits dark by design.
        ConnStatus::Idle => (StatusCode::Idle, "nothing is asking for the tracker".into()),
        ConnStatus::Connecting => (StatusCode::Connecting, String::new()),
        ConnStatus::Connected => (StatusCode::Connected, String::new()),
        ConnStatus::Error(e) => (StatusCode::Error, e.clone()),
    }
}

/// The guards held on behalf of connected clients, keyed by client.
///
/// Separated from the thread so it can be driven directly by a test: the
/// interesting behaviour is entirely in when a guard is taken and when it is
/// dropped, and none of that needs a socket to exercise.
#[derive(Default)]
pub(crate) struct Holds {
    held: HashMap<ClientId, DemandGuard>,
}

impl Holds {
    /// A client said hello, or said it again with different subscriptions.
    ///
    /// Replacing rather than adding: a second `Hello` from the same client
    /// changes what it wants, it does not want the tracker twice. The old guard
    /// is dropped by the insert, and dropping it before taking the new one
    /// would let the demand hit zero and close the session between the two.
    fn hello(&mut self, demand: &Demand, id: ClientId, bits: u32) {
        match reason_for(bits) {
            Some(reason) => {
                let fresh = demand.hold(reason);
                self.held.insert(id, fresh);
            }
            None => {
                self.held.remove(&id);
            }
        }
    }

    /// Drop the guards of clients that are no longer connected.
    ///
    /// Reap-by-reconciliation rather than by a disconnect event: a socket can
    /// die in ways that produce no message at all — the peer is killed, the
    /// machine suspends — and the client list is the truth either way. There is
    /// no timeout here because there is nothing to time out: the kernel already
    /// knows the socket is gone.
    fn reap(&mut self, live: &[ClientId]) {
        self.held.retain(|id, _| live.contains(id));
    }

    /// How many clients currently hold the tracker.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.held.len()
    }
}

/// Serve the hub's socket for as long as the process lives.
///
/// Returns without starting anything if the socket cannot be bound. That is a
/// degradation, not a failure: a hub that refuses to open because another one
/// already has the socket would be a worse outcome than a hub with no game
/// output, and the second one is exactly today's behaviour.
pub(crate) fn spawn(demand: Demand, state: Arc<Mutex<DeviceState>>) {
    let server = match Server::bind() {
        Ok(s) => s,
        Err(e) => {
            tobii_diagnostics::log::warn(&format!(
                "could not open the tracking socket ({e}); other programs will not \
                 be able to get tracking data from this hub"
            ));
            return;
        }
    };
    std::thread::spawn(move || {
        let mut holds = Holds::default();
        let mut last_sent: Option<StatusCode> = None;
        loop {
            tick(&server, &demand, &state, &mut holds, &mut last_sent);
            std::thread::sleep(POLL);
        }
    });
}

/// One pass: drain the socket, reconcile the holds, publish a status change.
///
/// Split from the loop so a test can drive it against a real `Server` on a
/// temporary path. The pure `Holds` tests below cover the decisions; this is
/// what proves the decisions are reached from an actual client connecting.
fn tick(
    server: &Server,
    demand: &Demand,
    state: &Arc<Mutex<DeviceState>>,
    holds: &mut Holds,
    last_sent: &mut Option<StatusCode>,
) {
    for msg in server.poll() {
        if let tobii_ipc::codec::Msg::Hello { subs: bits, .. } = msg.msg {
            holds.hello(demand, msg.from, bits);
        }
    }
    let live: Vec<ClientId> = server.clients().into_iter().map(|c| c.id).collect();
    holds.reap(&live);

    // Only on a change. A status broadcast every 50 ms would be a wake-up per
    // client per tick for information that changes a few times a minute.
    let (code, text) = {
        let s = state.lock().unwrap();
        status_of(&s)
    };
    if *last_sent != Some(code) {
        server.broadcast(0, &tobii_ipc::codec::Msg::Status { code, text });
        *last_sent = Some(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The claim the whole design rests on: a subscriber is a `DemandGuard`,
    /// so the standby gate in `device.rs` needs no second notion of who wants
    /// the tracker.
    #[test]
    fn a_subscriber_lights_the_tracker_and_leaving_puts_it_out() {
        let demand = Demand::new();
        let mut holds = Holds::default();
        assert!(!demand.active(), "nothing wants it yet");

        holds.hello(&demand, 1, subs::POSE);
        assert!(demand.active(), "a subscribed client is a reason");
        assert!(
            demand.reasons().iter().any(|r| r.contains("head tracking")),
            "the user should be told why: {:?}",
            demand.reasons()
        );

        holds.reap(&[]);
        assert!(
            !demand.active(),
            "the last client left; the tracker goes dark"
        );
    }

    /// Two clients, one hold each: the tracker stays up until both are gone.
    /// This is the property a timeout-based design has to approximate.
    #[test]
    fn clients_refcount_rather_than_racing_each_other() {
        let demand = Demand::new();
        let mut holds = Holds::default();
        holds.hello(&demand, 1, subs::POSE);
        holds.hello(&demand, 2, subs::GAZE);
        assert_eq!(holds.len(), 2);

        holds.reap(&[2]);
        assert!(demand.active(), "one client left, so the tracker stays on");
        holds.reap(&[]);
        assert!(!demand.active());
    }

    /// A client can give the tracker back without disconnecting, by saying
    /// hello again with nothing subscribed. The server acts on every `Hello`.
    #[test]
    fn resubscribing_to_nothing_gives_the_tracker_back() {
        let demand = Demand::new();
        let mut holds = Holds::default();
        holds.hello(&demand, 1, subs::POSE);
        assert!(demand.active());

        holds.hello(&demand, 1, 0);
        assert!(!demand.active(), "subscribed to nothing is not a reason");
        assert_eq!(holds.len(), 0);
    }

    /// Changing subscriptions must not let the demand hit zero in between, or
    /// the session closes and reopens — three seconds of the tracker going out
    /// and coming back for what should be a no-op.
    #[test]
    fn changing_subscriptions_never_drops_the_demand_to_zero() {
        let demand = Demand::new();
        let mut holds = Holds::default();
        holds.hello(&demand, 1, subs::GAZE);
        assert!(demand.active());

        holds.hello(&demand, 1, subs::POSE);
        assert!(
            demand.active(),
            "the tracker must not blink between the two"
        );
        assert_eq!(holds.len(), 1, "one client is one hold, not two");
    }

    /// Connecting is not by itself a reason to run an infrared lamp.
    #[test]
    fn a_client_that_wants_nothing_is_not_a_reason() {
        let demand = Demand::new();
        let mut holds = Holds::default();
        holds.hello(&demand, 7, 0);
        assert!(!demand.active());
        assert_eq!(reason_for(0), None);
    }

    #[test]
    fn every_subscription_has_a_reason_a_user_can_read() {
        for bits in [subs::POSE, subs::GAZE, subs::CAMERA] {
            let r = reason_for(bits).expect("a reason");
            assert!(
                r.chars().next().is_some_and(|c| c.is_lowercase()),
                "reads as a phrase in a sentence: {r:?}"
            );
        }
    }

    /// "Off on purpose" and "trying to open" must not be the same status, or a
    /// client shows "Connecting…" forever while the tracker is deliberately
    /// dark.
    #[test]
    fn idle_is_distinct_from_connecting() {
        let idle = status_of(&DeviceState {
            status: ConnStatus::Idle,
            ..DeviceState::default()
        });
        let connecting = status_of(&DeviceState {
            status: ConnStatus::Connecting,
            ..DeviceState::default()
        });
        assert_eq!(idle.0, StatusCode::Idle);
        assert_eq!(connecting.0, StatusCode::Connecting);
        assert!(!idle.1.is_empty(), "idle should say why it is idle");
    }

    /// The whole seam, through a real socket: a client that connects lights the
    /// tracker, and a client that goes away puts it out.
    ///
    /// The tests above are pure and prove the decisions; this one proves they
    /// are reached from an actual connection, which is the part a design
    /// document cannot assert.
    #[test]
    fn a_real_client_connecting_is_what_lights_the_tracker() {
        use tobii_ipc::Client;

        let path = std::env::temp_dir().join(format!("tobii-seam-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let server = Server::bind_at(&path).expect("bind");
        let demand = Demand::new();
        let state = Arc::new(Mutex::new(DeviceState::default()));
        let mut holds = Holds::default();
        let mut last = None;

        tick(&server, &demand, &state, &mut holds, &mut last);
        assert!(!demand.active(), "no clients, no reason");

        let client = Client::connect_at(&path, subs::POSE, "a game").expect("connect");
        // The hello arrives on the server's accept thread, so give it a moment
        // rather than assuming it is there on the first tick.
        let lit = (0..100).any(|_| {
            tick(&server, &demand, &state, &mut holds, &mut last);
            if demand.active() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
            false
        });
        assert!(lit, "a connected subscriber should light the tracker");
        assert!(
            demand.reasons().iter().any(|r| r.contains("head tracking")),
            "{:?}",
            demand.reasons()
        );

        drop(client);
        let dark = (0..100).any(|_| {
            tick(&server, &demand, &state, &mut holds, &mut last);
            if !demand.active() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
            false
        });
        assert!(dark, "the tracker must go out when the client goes away");
        let _ = std::fs::remove_file(&path);
    }

    /// The reason text is a literal, never the client's own name — that name
    /// is untrusted text from a socket and it is shown to the user.
    #[test]
    fn the_reason_never_comes_from_the_client() {
        let demand = Demand::new();
        let mut holds = Holds::default();
        holds.hello(&demand, 1, subs::POSE);
        for r in demand.reasons() {
            assert!(
                !r.contains("evil"),
                "the reason must not be attacker-controlled: {r}"
            );
        }
    }
}
