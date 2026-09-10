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

use tobii_ipc::{subs, ClientId, LeaseAction, Server, StatusCode};
use tobii_output::sinks::JoystickHandle;

use crate::device::{ConnStatus, Demand, DemandGuard, DeviceState, Lease};

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

/// Decide and act on one lease request.
///
/// The reply for a granted acquire is deliberately NOT sent here. See
/// [`Lease`]: the libusb interface is released when the device thread drops its
/// connection, and answering before that races the client into `DeviceBusy` on
/// an interface nobody has let go of yet.
fn handle_lease(
    server: &Server,
    demand: &Demand,
    lease: &Arc<Mutex<Lease>>,
    awaiting: &mut Vec<ClientId>,
    from: ClientId,
    name: &str,
    action: LeaseAction,
) {
    let mut l = lease.lock().unwrap();
    let holder = match &*l {
        Lease::Free => None,
        Lease::Requested { by, name } | Lease::Held { by, name } => Some((*by, name.as_str())),
    };
    match crate::device::lease_decision(holder, from, action) {
        crate::device::LeaseDecision::Grant => {
            // The hub can refuse. Calibration and display setup are long
            // stateful conversations with the device whose invariants live in
            // this crate; handing the device away mid-way through one would
            // leave both sides believing something different about it.
            let reasons = demand.reasons();
            if crate::device::wants_exclusive(&reasons) {
                drop(l);
                server.send_to(
                    from,
                    &tobii_ipc::codec::Msg::LeaseReply {
                        ok: false,
                        text: format!(
                            "the hub is busy with {} — try again when it has finished",
                            reasons.join(" and ")
                        ),
                    },
                );
                return;
            }
            *l = Lease::Requested {
                by: from,
                name: name.to_string(),
            };
            if !awaiting.contains(&from) {
                awaiting.push(from);
            }
        }
        crate::device::LeaseDecision::Refuse(why) => {
            drop(l);
            server.send_to(
                from,
                &tobii_ipc::codec::Msg::LeaseReply {
                    ok: false,
                    text: why,
                },
            );
        }
        crate::device::LeaseDecision::Released => {
            *l = Lease::Free;
            drop(l);
            server.send_to(
                from,
                &tobii_ipc::codec::Msg::LeaseReply {
                    ok: true,
                    text: String::new(),
                },
            );
        }
        // A release from a client that never held it must not take the device
        // away from the client that does.
        crate::device::LeaseDecision::NotHeld => {}
    }
}

/// How stale a measured rotation may be before game output stops trusting it.
///
/// The model returning nothing holds the previous rotation, which keeps the
/// hub's drawing steady through a moment where the head cannot be found. A game
/// camera is different: a held rotation is indistinguishable from a real one,
/// so after a second the pose is dropped and the pipeline falls back to the
/// geometric one, which at least tracks the eyes that are actually there.
const HEAD_POSE_MAX_AGE: Duration = Duration::from_secs(1);

/// Whether a measured rotation is recent enough for a game to act on.
pub(crate) fn pose_is_fresh(at: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    at.is_some_and(|t| now.saturating_duration_since(t) < HEAD_POSE_MAX_AGE)
}

/// The hub's own game output: compose a frame per gaze sample, route it to the
/// configured sinks.
///
/// Built per CONNECTION rather than once at startup. The tracker only opens when
/// something asks for it, so the next connect is the next moment anybody could
/// be watching — which makes toggling the games switch take effect without
/// restarting the hub, and re-reads the display corners Extended View needs
/// after a screen change.
pub struct GameOutput {
    cfg: tobii_output::games::OutputConfig,
    router: tobii_output::Router,
    pipeline: tobii_output::pipeline::FramePipeline,
    corners: Option<tobii_protocol::DisplayCorners>,
}

impl GameOutput {
    /// The output for this session, or `None` if game output is switched off.
    ///
    /// Off is the default, deliberately: a hub that started steering games the
    /// moment it was installed would be a surprise, and the illuminator rule
    /// says nothing about who is allowed to consume the data.
    pub fn for_session(joystick: Option<JoystickHandle>) -> Option<GameOutput> {
        let cfg = tobii_output::games::load_output_config();
        if !cfg.enabled {
            return None;
        }
        Self::from_config(cfg, joystick)
    }

    /// The same, from a config given rather than read.
    ///
    /// Split out so a test can point the sinks at a socket it owns: what is
    /// worth asserting is that a datagram actually leaves, and that cannot be
    /// checked against whatever `games.toml` happens to say on the machine
    /// running the test.
    fn from_config(
        cfg: tobii_output::games::OutputConfig,
        joystick: Option<JoystickHandle>,
    ) -> Option<GameOutput> {
        let mut router = tobii_output::Router::new(cfg.rate_hz);
        if let Some(spec) = &cfg.opentrack {
            match spec
                .parse()
                .map_err(|e| format!("{e}"))
                .and_then(|a| tobii_output::sinks::OpentrackUdp::new(a).map_err(|e| format!("{e}")))
            {
                Ok(s) => router.add(Box::new(s)),
                // A sink that cannot be opened is named and skipped. Refusing
                // the whole session because one endpoint is bad would take the
                // hub's tracking down with it.
                Err(e) => tobii_diagnostics::log::warn(&format!(
                    "game output: could not open the opentrack sink at {spec} ({e})"
                )),
            }
        }
        if let Some(port) = cfg.bridge_port {
            match tobii_output::sinks::BridgeUdp::new(port) {
                Ok(s) => router.add(Box::new(s)),
                Err(e) => tobii_diagnostics::log::warn(&format!(
                    "game output: could not open the bridge sink on port {port} ({e})"
                )),
            }
        }
        // Handed in, never created here: the device outlives any one session.
        // See `JoystickHandle`.
        if let Some(js) = joystick {
            router.add(Box::new(js));
        }
        if router.sink_count() == 0 {
            tobii_diagnostics::log::warn(
                "game output is on but no sink could be opened; nothing will receive it",
            );
            return None;
        }
        tobii_diagnostics::log::info(&format!(
            "game output on, sending to: {}",
            router.sink_names().join(", ")
        ));
        Some(GameOutput {
            pipeline: tobii_output::pipeline::FramePipeline::new(&cfg),
            corners: tobii_config::load().ok().flatten().map(|s| s.to_corners()),
            cfg,
            router,
        })
    }

    /// Compose and route one sample.
    pub fn offer(
        &mut self,
        sample: &tobii_protocol::GazeSample,
        pose: Option<tobii_headpose::HeadPose>,
        now: std::time::Instant,
    ) {
        let frame = self
            .pipeline
            .offer(sample, pose, &self.cfg, self.corners, now);
        self.router.offer(&frame, now);
    }
}

/// Serve the hub's socket for as long as the process lives.
///
/// Returns without starting anything if the socket cannot be bound. That is a
/// degradation, not a failure: a hub that refuses to open because another one
/// already has the socket would be a worse outcome than a hub with no game
/// output, and the second one is exactly today's behaviour.
pub(crate) fn spawn(demand: Demand, state: Arc<Mutex<DeviceState>>, lease: Arc<Mutex<Lease>>) {
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
        // Clients told "yes, in a moment" — answered once the device thread has
        // actually let go. See `Lease`.
        let mut awaiting: Vec<ClientId> = Vec::new();
        let mut last_sent: Option<StatusCode> = None;
        loop {
            tick(
                &server,
                &demand,
                &state,
                &lease,
                &mut holds,
                &mut awaiting,
                &mut last_sent,
            );
            std::thread::sleep(POLL);
        }
    });
}

/// One pass: drain the socket, reconcile the holds, publish a status change.
///
/// Split from the loop so a test can drive it against a real `Server` on a
/// temporary path. The pure `Holds` tests below cover the decisions; this is
/// what proves the decisions are reached from an actual client connecting.
#[allow(clippy::too_many_arguments)]
fn tick(
    server: &Server,
    demand: &Demand,
    state: &Arc<Mutex<DeviceState>>,
    lease: &Arc<Mutex<Lease>>,
    holds: &mut Holds,
    awaiting: &mut Vec<ClientId>,
    last_sent: &mut Option<StatusCode>,
) {
    for msg in server.poll() {
        match msg.msg {
            tobii_ipc::codec::Msg::Hello { subs: bits, .. } => {
                holds.hello(demand, msg.from, bits);
            }
            tobii_ipc::codec::Msg::Lease(action) => {
                let name = server
                    .clients()
                    .into_iter()
                    .find(|c| c.id == msg.from)
                    .map(|c| c.name)
                    .unwrap_or_default();
                handle_lease(server, demand, lease, awaiting, msg.from, &name, action);
            }
            _ => {}
        }
    }
    let live: Vec<ClientId> = server.clients().into_iter().map(|c| c.id).collect();
    holds.reap(&live);

    // A client that died holding the lease gets it taken back. This is the same
    // reasoning as reaping a hold: a socket that is gone is the truth, and it
    // needs no timeout.
    {
        let mut l = lease.lock().unwrap();
        if l.client().is_some_and(|id| !live.contains(&id)) {
            *l = Lease::Free;
        }
    }

    // The device thread has let go: answer everyone who was told to wait.
    if matches!(*lease.lock().unwrap(), Lease::Held { .. }) {
        for id in awaiting.drain(..) {
            server.send_to(
                id,
                &tobii_ipc::codec::Msg::LeaseReply {
                    ok: true,
                    text: String::new(),
                },
            );
        }
    }

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
        let lease = Arc::new(Mutex::new(Lease::Free));
        let mut awaiting = Vec::new();
        let mut last = None;

        tick(
            &server,
            &demand,
            &state,
            &lease,
            &mut holds,
            &mut awaiting,
            &mut last,
        );
        assert!(!demand.active(), "no clients, no reason");

        let client = Client::connect_at(&path, subs::POSE, "a game").expect("connect");
        // The hello arrives on the server's accept thread, so give it a moment
        // rather than assuming it is there on the first tick.
        let lit = (0..100).any(|_| {
            tick(
                &server,
                &demand,
                &state,
                &lease,
                &mut holds,
                &mut awaiting,
                &mut last,
            );
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
            tick(
                &server,
                &demand,
                &state,
                &lease,
                &mut holds,
                &mut awaiting,
                &mut last,
            );
            if !demand.active() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
            false
        });
        assert!(dark, "the tracker must go out when the client goes away");
        let _ = std::fs::remove_file(&path);
    }

    /// A rotation the model has merely been HOLDING must not reach a game.
    ///
    /// When the model cannot find a head it keeps the last rotation rather than
    /// snapping to zero — right for the hub's drawing, wrong for a camera,
    /// where a held angle is indistinguishable from a real one and the view
    /// simply stops responding while looking like it is working.
    #[test]
    fn a_held_rotation_stops_being_offered_to_games_after_a_second() {
        let now = std::time::Instant::now();
        assert!(!pose_is_fresh(None, now), "no measurement at all");
        assert!(pose_is_fresh(Some(now), now), "measured right now");
        assert!(
            pose_is_fresh(Some(now - Duration::from_millis(999)), now),
            "just inside the bound: a brief loss must not drop the pose"
        );
        assert!(
            !pose_is_fresh(Some(now - HEAD_POSE_MAX_AGE), now),
            "at the bound it is stale"
        );
        assert!(
            !pose_is_fresh(Some(now - Duration::from_secs(60)), now),
            "a minute-old rotation is not a head position"
        );
    }

    /// The ordering the middle state exists for: a client is not told it has
    /// the device until the hub has actually let go of it.
    ///
    /// Replying earlier races the client into `DeviceBusy` on an interface
    /// nobody has released — and that failure looks like the tracker being
    /// broken, not like a protocol mistake.
    #[test]
    fn a_lease_is_not_confirmed_until_the_device_is_released() {
        use tobii_ipc::codec::Msg;
        use tobii_ipc::Client;

        let path = std::env::temp_dir().join(format!("tobii-lease-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let server = Server::bind_at(&path).expect("bind");
        let demand = Demand::new();
        let state = Arc::new(Mutex::new(DeviceState::default()));
        let lease = Arc::new(Mutex::new(Lease::Free));
        let (mut holds, mut awaiting, mut last) = (Holds::default(), Vec::new(), None);

        let mut client = Client::connect_at(&path, subs::POSE, "a game").expect("connect");
        client
            .send(&Msg::Lease(tobii_ipc::LeaseAction::Acquire))
            .expect("the acquire is sent");

        // Pump until the request has been seen.
        let asked = (0..100).any(|_| {
            tick(
                &server,
                &demand,
                &state,
                &lease,
                &mut holds,
                &mut awaiting,
                &mut last,
            );
            if matches!(*lease.lock().unwrap(), Lease::Requested { .. }) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
            false
        });
        assert!(asked, "the acquire should have been recorded");
        assert_eq!(awaiting.len(), 1, "and the reply deferred, not sent");

        // The hub is still letting go, so no confirmation yet, however many
        // times the loop runs.
        for _ in 0..5 {
            tick(
                &server,
                &demand,
                &state,
                &lease,
                &mut holds,
                &mut awaiting,
                &mut last,
            );
        }
        assert_eq!(awaiting.len(), 1, "still not confirmed while Requested");

        // The device thread announces the release.
        *lease.lock().unwrap() = Lease::Held {
            by: awaiting[0],
            name: "a game".into(),
        };
        tick(
            &server,
            &demand,
            &state,
            &lease,
            &mut holds,
            &mut awaiting,
            &mut last,
        );
        assert!(awaiting.is_empty(), "now it is answered");

        let _ = std::fs::remove_file(&path);
    }

    /// A client that dies holding the lease must not keep the device forever.
    /// Same reasoning as reaping a hold: a socket that is gone is the truth.
    #[test]
    fn a_lease_held_by_a_dead_client_is_taken_back() {
        let path = std::env::temp_dir().join(format!("tobii-lease2-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let server = Server::bind_at(&path).expect("bind");
        let demand = Demand::new();
        let state = Arc::new(Mutex::new(DeviceState::default()));
        // A holder that never existed on this server stands in for one that
        // has gone: either way it is not in `clients()`.
        let lease = Arc::new(Mutex::new(Lease::Held {
            by: 4242,
            name: "a departed game".into(),
        }));
        let (mut holds, mut awaiting, mut last) = (Holds::default(), Vec::new(), None);

        tick(
            &server,
            &demand,
            &state,
            &lease,
            &mut holds,
            &mut awaiting,
            &mut last,
        );
        assert_eq!(
            *lease.lock().unwrap(),
            Lease::Free,
            "the device must come back when its holder is gone"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A datagram actually leaves.
    ///
    /// Everything else here asserts a decision; this asserts the consequence.
    /// The chain is long — config, Router, FramePipeline, the opentrack sink,
    /// a UDP socket — and each link is tested in its own crate, but nothing
    /// until now checked that the hub wires them into something a game receives.
    #[test]
    fn a_tracked_sample_reaches_a_real_socket_as_an_opentrack_datagram() {
        use std::net::UdpSocket;
        use tobii_output::games::OutputConfig;

        let listener = UdpSocket::bind("127.0.0.1:0").expect("bind a listener");
        listener
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let cfg = OutputConfig {
            enabled: true,
            opentrack: Some(addr.to_string()),
            // Only the one sink, so the assertion below is about this socket.
            bridge_port: None,
            ..OutputConfig::default()
        };
        let mut out = GameOutput::from_config(cfg, None).expect("a router with one sink");

        // A sample the tracker would produce with both eyes visible.
        let mut sample = tobii_protocol::GazeSample {
            timestamp_us: 1,
            validity_l: 0,
            validity_r: 0,
            eye_origin_l_mm: [-32.0, 0.0, 600.0],
            eye_origin_r_mm: [32.0, 0.0, 600.0],
            ..Default::default()
        };
        sample.present_mask = tobii_protocol::gaze::present::EYE_ORIGIN_L
            | tobii_protocol::gaze::present::EYE_ORIGIN_R
            | tobii_protocol::gaze::present::VALIDITY_L
            | tobii_protocol::gaze::present::VALIDITY_R;

        out.offer(&sample, None, std::time::Instant::now());

        let mut buf = [0u8; 128];
        let n = listener
            .recv(&mut buf)
            .expect("a datagram should have arrived");
        assert_eq!(n, 48, "an opentrack datagram is six f64");

        // And it carries the position the eye origins imply, not zeroes.
        let z = f64::from_le_bytes(buf[16..24].try_into().unwrap());
        assert!(
            z.abs() > 1.0,
            "the datagram should carry a real distance, got {z}"
        );
    }

    /// Game output is off unless it has been turned on. A hub that started
    /// steering games the moment it was installed would be a surprise, and the
    /// illuminator rule says nothing about who may consume the data.
    #[test]
    fn game_output_is_off_until_it_is_switched_on() {
        assert!(
            !tobii_output::games::OutputConfig::default().enabled,
            "the default must be off, or `for_session` opens sockets nobody asked for"
        );
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
