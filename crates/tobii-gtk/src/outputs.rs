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
//! # The other way something can ask
//!
//! Not everything that wants head tracking can connect to that socket.
//! opentrack's *UDP over network* input is a plain UDP listener that knows
//! nothing about us, and X-Plane is the same. Wrapping a game in `tobii game`
//! was the workaround — it carries no data at all, it subscribes so that the
//! count goes above zero — and telling everyone to wrap opentrack in it was
//! never anything but a way to spell "please turn the tracker on".
//!
//! So [`PortWatch`] asks the kernel instead: is a socket bound where we send?
//! The answer becomes an ordinary [`DemandGuard`], which is the point — the
//! second way of asking did not need a second notion of who wants the device.
//!
//! # What this does not do yet
//!
//! Nothing is published. No frames, no lease, no game output — this is the
//! demand seam alone. A shipped build behaves exactly as it does today, because
//! nothing in the tree connects to the socket; the only user-visible difference
//! is a line in the log when the socket was already taken.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tobii_ipc::{subs, ClientId, LeaseAction, Server, StatusCode};
use tobii_output::games::OutputConfig;
use tobii_output::listener::Listening;
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

/// Why the tracker is on when a program holds the opentrack port.
///
/// Named as its own cause rather than folded into the socket-client reason
/// above, because the two are answers to different questions when something
/// goes wrong: one is "a program connected to the hub and asked", the other is
/// "a program is sitting on the address we send to and never spoke to us at
/// all". A user staring at lit illuminators with no game running needs to be
/// able to tell those apart — and if it says the wrong one, the thing they go
/// looking for does not exist.
const LISTENER_REASON: &str = "a program listening on the opentrack port";

/// How often the opentrack address is checked for a listener.
///
/// Deliberately not [`POLL`]. The socket loop runs at 50 ms because it is
/// draining a socket and a client's disconnect should not sit unnoticed; this
/// is a scan of `/proc/net/udp`, which walks every UDP socket on the machine,
/// and the thing it is watching for is a human starting a program. Twenty scans
/// a second, forever, to notice that half a second sooner is not a trade worth
/// making — and the cost of the slower rate is bounded the same way in both
/// directions: up to a second before the tracker lights, up to a second before
/// it goes dark again.
const LISTENER_POLL: Duration = Duration::from_secs(1);

/// The address to watch, or `None` if nothing should be watched.
///
/// Three conditions, all of them settings the user set themselves: game output
/// is on, the watch is on, and there is an address to watch. The first is what
/// keeps this from being a surprise — with game output off, the hub is not
/// sending anywhere, and lighting the tracker for a socket it would ignore
/// would be lighting it for nothing.
pub(crate) fn watch_target(cfg: &OutputConfig) -> Option<SocketAddr> {
    if !cfg.enabled || !cfg.wake_for_opentrack {
        return None;
    }
    cfg.opentrack.as_deref()?.parse().ok()
}

/// A [`DemandGuard`] held for as long as something is bound at the opentrack
/// address.
///
/// This is the whole of "you do not need a wrapper for opentrack". `tobii game`
/// transports nothing — it subscribes over the hub's socket so that the demand
/// count goes above zero — and opentrack, which speaks plain UDP and knows
/// nothing about the hub, had no way to do the same. So the hub looks at the
/// place it would be sending instead: a socket bound there is a program that
/// asked for head tracking, in the only vocabulary it has.
///
/// Same machinery as every other consumer, on purpose. A second notion of who
/// wants the device — a flag, a timeout, a "game mode" — would be a second
/// place for the standby rule to be wrong.
#[derive(Default)]
pub(crate) struct PortWatch {
    held: Option<DemandGuard>,
    last_looked: Option<Instant>,
}

impl PortWatch {
    /// Look, at most once per [`LISTENER_POLL`], and hold or release.
    ///
    /// `target` and `look` are passed in rather than called directly for two
    /// reasons: a test can then drive every transition without a config file or
    /// a socket, and the throttle genuinely saves the work — on a tick that is
    /// not due, neither the file read nor the `/proc` scan happens.
    fn poll(
        &mut self,
        demand: &Demand,
        now: Instant,
        target: impl FnOnce() -> Option<SocketAddr>,
        look: impl FnOnce(SocketAddr) -> Listening,
    ) {
        if self
            .last_looked
            .is_some_and(|t| now.saturating_duration_since(t) < LISTENER_POLL)
        {
            return;
        }
        self.last_looked = Some(now);
        // Switched off, or nothing to watch: give the hold back. Read every
        // time rather than once at startup, because these are settings the user
        // can change while the hub runs — the same mistake `GameOutput` records
        // having made by freezing its config for the life of a session.
        let Some(addr) = target() else {
            self.held = None;
            return;
        };
        match look(addr) {
            // One listener is one guard, kept rather than re-taken. Assigning a
            // fresh `hold` every second would behave the same — but only
            // because assignment drops the old guard *after* taking the new
            // one, so the count never touches zero. That is a subtlety to not
            // depend on: the version of it that does the drop first closes and
            // reopens the USB session once a second, which on this device means
            // rebooting the tracker once a second.
            Listening::Yes => {
                self.held
                    .get_or_insert_with(|| demand.hold(LISTENER_REASON));
            }
            // `Unknown` releases too. It means the question could not be
            // answered — an address on another machine, no `/proc` — and a hold
            // taken on an answer we no longer have is a tracker that stays lit
            // for a reason nobody can check.
            Listening::No | Listening::Unknown(_) => self.held = None,
        }
    }

    /// Whether the tracker is currently being held on for a listener.
    #[cfg(test)]
    fn holding(&self) -> bool {
        self.held.is_some()
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

/// A recentre asked for from anywhere, and what came of it.
///
/// Three things can ask the hub for one — the button in the games row, a socket
/// client sending [`tobii_ipc::codec::Msg::Recentre`], and the hub's own tick —
/// while the only thing that can perform one is the `FramePipeline` inside
/// [`GameOutput`], which lives on the device thread and is rebuilt whenever the
/// settings change under it. So a request is *left here* rather than delivered,
/// and taken by whichever pipeline is composing frames when it next runs.
///
/// Shared exactly the way `JoystickStatus` is, and for the same reason: it is
/// state two threads have to agree about. Deliberately not a `DeviceCommand` —
/// those are applied to the tracker, and a recentre changes nothing on the
/// device at all; it changes how the pose is composed afterwards.
#[derive(Clone, Default)]
pub struct Recentring {
    inner: Arc<Mutex<RecentreState>>,
}

#[derive(Default)]
struct RecentreState {
    /// When a recentre was asked for and not yet taken.
    asked_at: Option<Instant>,
    /// The last outcome, and when it arrived.
    said: Option<(Instant, String)>,
}

/// How long an unclaimed request stays askable before it is thrown away.
///
/// Game output takes a request on its next gaze frame — 30.208 ms away at the
/// cadence measured in the committed capture — so anything still sitting here
/// two seconds later was asked for while nothing was composing frames at all:
/// game output switched off, or the tracker dark. Performing it later, when a
/// game finally starts, would be a recentre taken from a posture nobody was
/// holding at the time. A chosen bound, not a measured one: nearly two orders
/// of magnitude above the consumption latency, and far below the gap between
/// one session and the next.
const REQUEST_MAX_AGE: Duration = Duration::from_secs(2);

/// How long an outcome stays on the hub's status line.
///
/// The settle window is a second, so the answer arrives about a second after
/// the click; six seconds leaves it readable without leaving a stale sentence
/// where the live status belongs. Chosen.
const MESSAGE_SHOWN_FOR: Duration = Duration::from_secs(6);

impl Recentring {
    /// Ask for the head's current rotation to become straight ahead.
    pub fn request(&self, now: Instant) {
        self.inner.lock().unwrap().asked_at = Some(now);
    }

    /// Take a pending request, if there is a fresh one.
    ///
    /// Two requests arriving before either is taken collapse into one, which is
    /// what they mean: pressing a recentre button twice is "start from now",
    /// not "do it twice".
    pub fn take(&self, now: Instant) -> bool {
        let mut s = self.inner.lock().unwrap();
        match s.asked_at.take() {
            Some(at) => now.saturating_duration_since(at) <= REQUEST_MAX_AGE,
            None => false,
        }
    }

    /// Record what the recentre did, for the hub to show.
    pub fn report(&self, text: String, now: Instant) {
        self.inner.lock().unwrap().said = Some((now, text));
    }

    /// The outcome, while it is still worth showing.
    pub fn message(&self, now: Instant) -> Option<String> {
        let s = self.inner.lock().unwrap();
        s.said
            .as_ref()
            .filter(|(at, _)| now.saturating_duration_since(*at) < MESSAGE_SHOWN_FOR)
            .map(|(_, text)| text.clone())
    }
}

/// Whether a recentre can be taken right now, and what to tell the asker if not.
///
/// Both refusals are for things the asker cannot see:
///
/// * **A flow that wants the device exclusively.** `handle_lease` refuses a
///   lease for exactly these reasons, and this refuses for the same ones —
///   though not for the same cause. A recentre needs nothing exclusive; what
///   makes it wrong here is what the user's head is doing during one of those
///   flows. A calibration has them following a stimulus dot into the corners of
///   the screen, so the "posture" a settle window would average is whichever
///   corner the dot was in, and the result would be a permanent reference taken
///   from a moment of looking away. Silently re-referencing off that is
///   precisely the failure worth refusing for.
/// * **Nothing is tracking.** There is no head to measure, so the request would
///   sit unclaimed until it expired — a button that appeared to do nothing.
pub(crate) fn recentre_decision(reasons: &[&'static str], tracking: bool) -> Result<(), String> {
    if crate::device::wants_exclusive(reasons) {
        return Err(format!(
            "the hub is busy with {} — try again when it has finished",
            reasons.join(" and ")
        ));
    }
    if !tracking {
        return Err(
            "the tracker is not running, so there is no head pose to recentre — start what \
             you are recentring first"
                .to_string(),
        );
    }
    Ok(())
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
/// Built per connection, and **reconfigured in place** whenever the settings
/// change under it — see [`GameOutput::reconfigure`] and `device::GameSide`.
///
/// Per connection alone was not enough, and this comment used to say it was:
/// "the next connect is the next moment anybody could be watching, which makes
/// toggling the games switch take effect without restarting the hub". A session
/// lasts as long as anything holds the tracker — the whole time the hub has
/// focus, the whole life of a game started with `tobii game` — so the settings
/// were frozen for exactly as long as somebody was there to change them.
pub struct GameOutput {
    cfg: tobii_output::games::OutputConfig,
    router: tobii_output::Router,
    pipeline: tobii_output::pipeline::FramePipeline,
    corners: Option<tobii_protocol::DisplayCorners>,
    /// Where a recentre asked for elsewhere is picked up, and where the answer
    /// is left. Cloned into every rebuild of this struct, so a request cannot
    /// be lost by the settings changing underneath it.
    recentring: Recentring,
}

impl GameOutput {
    /// The output for this session, or `None` if game output is switched off.
    ///
    /// Off is the default, deliberately: a hub that started steering games the
    /// moment it was installed would be a surprise, and the illuminator rule
    /// says nothing about who is allowed to consume the data.
    pub fn for_session(
        joystick: Option<JoystickHandle>,
        recentring: Recentring,
    ) -> Option<GameOutput> {
        let cfg = tobii_output::games::load_output_config();
        if !cfg.enabled {
            return None;
        }
        Self::from_config(cfg, joystick, recentring)
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
        recentring: Recentring,
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
            recentring,
        })
    }

    /// Take a settings change without disturbing the tracking state.
    ///
    /// # Why this is not just a rebuild
    ///
    /// Rebuilding `GameOutput` is what the device thread used to do on any
    /// settings change, and it constructs a fresh [`FramePipeline`] — which
    /// throws away the **neutral**, the head position that reads as zero
    /// displacement. Re-taking it mid-game means re-taking it from wherever the
    /// user's head happens to be at that instant. Change the Extended View
    /// strength while leaning forward and "centre" becomes the leaning pose,
    /// permanently, until tracking is lost for a full second.
    ///
    /// The pipeline has nothing to do with which sinks are attached, so it is
    /// simply kept. Only the smoothing strength can require touching it, and
    /// only when it actually changed.
    pub fn reconfigure(&mut self, joystick: Option<JoystickHandle>) -> bool {
        let Some(fresh) = GameOutput::for_session(joystick, self.recentring.clone()) else {
            return false;
        };
        // Both of the filter's settings, because rebuilding it from one of them
        // resets the other: `set_filter` takes the whole config for exactly
        // that reason. The pipeline is otherwise kept — see the doc above.
        if (fresh.cfg.filter_alpha - self.cfg.filter_alpha).abs() > f64::EPSILON
            || (fresh.cfg.filter_max_step_mm - self.cfg.filter_max_step_mm).abs() > f64::EPSILON
        {
            self.pipeline.set_filter(&fresh.cfg);
        }
        self.cfg = fresh.cfg;
        self.router = fresh.router;
        self.corners = fresh.corners;
        true
    }

    /// Compose and route one sample.
    pub fn offer(
        &mut self,
        sample: &tobii_protocol::GazeSample,
        pose: Option<tobii_headpose::HeadPose>,
        now: std::time::Instant,
    ) {
        // Asked for by the hub's button or over the socket, performed here:
        // this is where the pipeline that composes the pose actually lives, and
        // it is the only thing that can average a settle window against real
        // frames.
        if self.recentring.take(now) {
            self.pipeline.begin_recentre(now);
        }
        let frame = self
            .pipeline
            .offer(sample, pose, &self.cfg, self.corners, now);
        // A second or so later, when the window closes. Reported twice on
        // purpose: to the log, which is where a support thread looks, and back
        // to whoever asked, which is what stops a refused recentre from being
        // indistinguishable from one that worked.
        if let Some(outcome) = self.pipeline.take_recentre() {
            let text = outcome.to_string();
            match outcome {
                tobii_output::pipeline::RecentreOutcome::Applied { .. } => {
                    tobii_diagnostics::log::info(&format!("head tracking: {text}"));
                }
                _ => tobii_diagnostics::log::warn(&format!("head tracking: {text}")),
            }
            self.recentring.report(text, now);
        }
        self.router.offer(&frame, now);
    }
}

/// Serve the hub's socket for as long as the process lives.
///
/// Returns without starting anything if the socket cannot be bound. That is a
/// degradation, not a failure: a hub that refuses to open because another one
/// already has the socket would be a worse outcome than a hub with no game
/// output, and the second one is exactly today's behaviour.
pub(crate) fn spawn(
    demand: Demand,
    state: Arc<Mutex<DeviceState>>,
    lease: Arc<Mutex<Lease>>,
    recentring: Recentring,
) {
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
        // Riding this thread rather than one of its own: it already wakes on a
        // timer and already owns a `Demand`, and the watch's own rate limit is
        // what keeps the two cadences apart.
        let mut watch = PortWatch::default();
        loop {
            tick(
                &server,
                &demand,
                &state,
                &lease,
                &mut holds,
                &mut awaiting,
                &mut last_sent,
                &recentring,
            );
            watch.poll(
                &demand,
                Instant::now(),
                || watch_target(&tobii_output::games::load_output_config()),
                tobii_output::listener::probe,
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
    recentring: &Recentring,
) {
    for msg in server.poll() {
        match msg.msg {
            tobii_ipc::codec::Msg::Hello { subs: bits, .. } => {
                holds.hello(demand, msg.from, bits);
            }
            // Answered immediately, unlike a lease: there is nothing to wait
            // for, because nothing is being handed over. `ok` says the request
            // was taken, not that the reference moved — the settle window has
            // not run yet, and it can still refuse a head that will not hold
            // still. The hub reports that part through its own status line.
            tobii_ipc::codec::Msg::Recentre => {
                let tracking = matches!(state.lock().unwrap().status, ConnStatus::Connected);
                let reply = match recentre_decision(&demand.reasons(), tracking) {
                    Ok(()) => {
                        recentring.request(Instant::now());
                        tobii_ipc::codec::Msg::RecentreReply {
                            ok: true,
                            text: String::new(),
                        }
                    }
                    Err(why) => tobii_ipc::codec::Msg::RecentreReply {
                        ok: false,
                        text: why,
                    },
                };
                server.send_to(msg.from, &reply);
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
        let recentring = Recentring::default();

        tick(
            &server,
            &demand,
            &state,
            &lease,
            &mut holds,
            &mut awaiting,
            &mut last,
            &recentring,
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
                &recentring,
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
                &recentring,
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
        let recentring = Recentring::default();

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
                &recentring,
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
                &recentring,
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
            &recentring,
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
        let recentring = Recentring::default();

        tick(
            &server,
            &demand,
            &state,
            &lease,
            &mut holds,
            &mut awaiting,
            &mut last,
            &recentring,
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
        let mut out = GameOutput::from_config(cfg, None, Recentring::default())
            .expect("a router with one sink");

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

        // The first sample establishes the neutral, so it is zero displacement
        // by construction — that is the point of the recentring, not a fault.
        // The rate throttle means only one datagram is sent per 1/rate_hz, so
        // the leaned sample is offered far enough ahead to be let through.
        let t0 = std::time::Instant::now();
        out.offer(&sample, None, t0);
        sample.eye_origin_l_mm[2] = 500.0;
        sample.eye_origin_r_mm[2] = 500.0;
        out.offer(&sample, None, t0 + Duration::from_millis(500));

        let mut buf = [0u8; 128];
        let n = listener
            .recv(&mut buf)
            .expect("a datagram should have arrived");
        assert_eq!(n, 48, "an opentrack datagram is six f64");

        // Drain to the most recent datagram: the first carries the neutral.
        let mut last = buf;
        listener
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        while let Ok(n) = listener.recv(&mut buf) {
            if n == 48 {
                last = buf;
            }
        }

        // And it carries the 100 mm lean the eye origins imply, not zeroes.
        // Smoothing means it arrives partway there, so this asserts the sign
        // and that something real moved rather than an exact figure.
        let z = f64::from_le_bytes(last[16..24].try_into().unwrap());
        assert!(
            z < -1.0,
            "leaning 100 mm closer should send a real negative displacement, \
             got {z}"
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

    /// A recentre must be refused for the same reasons a lease is, and say so.
    ///
    /// Not because it needs the device — it does not — but because of what the
    /// user's eyes are doing during those flows: a calibration has them
    /// following a dot into the corners of the screen, and a reference averaged
    /// off that is permanent and invisible. The refusal names the flow, so the
    /// asker knows to try again rather than pressing a button that appears dead.
    #[test]
    fn a_recentre_is_refused_while_a_flow_owns_the_device() {
        for flow in ["calibration", "display setup"] {
            let why = recentre_decision(&[flow], true)
                .expect_err("a recentre during an exclusive flow must be refused");
            assert!(why.contains(flow), "the refusal must name the flow: {why}");
        }
        // Watching is not a reason to refuse: the hub having focus, or the gaze
        // preview being open, says nothing about where the user is looking.
        assert!(recentre_decision(&["the hub window"], true).is_ok());
        assert!(recentre_decision(&[], true).is_ok());
    }

    /// With nothing tracking there is no head to measure, so the request would
    /// sit unclaimed until it expired — a control that appears to do nothing.
    #[test]
    fn a_recentre_with_the_tracker_dark_is_refused_rather_than_queued() {
        let why = recentre_decision(&[], false).expect_err("refused");
        assert!(
            why.contains("not running"),
            "the refusal must say why it did nothing: {why}"
        );
    }

    /// A request is for the posture the user is holding *now*. One left behind
    /// by a hub with no game output running must not be performed when a game
    /// starts half an hour later.
    #[test]
    fn a_request_nothing_consumed_goes_stale_rather_than_waiting() {
        let now = Instant::now();
        let r = Recentring::default();
        assert!(!r.take(now), "nothing asked for yet");

        r.request(now);
        assert!(r.take(now + REQUEST_MAX_AGE), "still fresh at the bound");

        r.request(now);
        assert!(
            !r.take(now + REQUEST_MAX_AGE + Duration::from_millis(1)),
            "a request older than the bound must not be performed"
        );
        assert!(!r.take(now), "and it is gone either way, not left pending");
    }

    /// Two presses of a button mean "start from now", not "do it twice".
    #[test]
    fn two_requests_before_either_is_taken_are_one_recentre() {
        let now = Instant::now();
        let r = Recentring::default();
        r.request(now);
        r.request(now + Duration::from_millis(50));
        assert!(r.take(now + Duration::from_millis(60)));
        assert!(!r.take(now + Duration::from_millis(70)), "only one");
    }

    /// The outcome has to reach the user, and then stop being the news.
    #[test]
    fn an_outcome_is_shown_and_then_makes_way_for_the_live_status() {
        let now = Instant::now();
        let r = Recentring::default();
        assert_eq!(r.message(now), None);
        r.report("recentred".to_string(), now);
        assert_eq!(r.message(now).as_deref(), Some("recentred"));
        assert_eq!(
            r.message(now + MESSAGE_SHOWN_FOR),
            None,
            "a stale sentence must not sit where the live status belongs"
        );
    }

    /// The whole socket path, through a real client: a recentre arriving while
    /// the hub is calibrating is refused and told why, and nothing is left
    /// pending for the pipeline to pick up afterwards.
    #[test]
    fn a_recentre_over_the_socket_is_answered_and_refused_during_calibration() {
        use tobii_ipc::codec::Msg;
        use tobii_ipc::Client;

        let path = std::env::temp_dir().join(format!("tobii-recentre-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let server = Server::bind_at(&path).expect("bind");
        let demand = Demand::new();
        let state = Arc::new(Mutex::new(DeviceState {
            status: ConnStatus::Connected,
            ..DeviceState::default()
        }));
        let lease = Arc::new(Mutex::new(Lease::Free));
        let (mut holds, mut awaiting, mut last) = (Holds::default(), Vec::new(), None);
        let recentring = Recentring::default();
        // A calibration running in the hub, exactly as the flow takes it.
        let _calibrating = demand.hold("calibration");

        let mut client = Client::connect_at(&path, subs::POSE, "a game").expect("connect");
        client.send(&Msg::Recentre).expect("the request is sent");

        let reply = wait_for_reply(
            &server,
            &demand,
            &state,
            &lease,
            &mut holds,
            &mut awaiting,
            &mut last,
            &recentring,
            &client,
        );
        match reply {
            Msg::RecentreReply { ok, text } => {
                assert!(!ok, "a recentre mid-calibration must be refused");
                assert!(text.contains("calibration"), "{text}");
            }
            other => panic!("expected a recentre reply, got {other:?}"),
        }
        assert!(
            !recentring.take(Instant::now()),
            "a refused request must not still be waiting for the pipeline"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// And the accepting half: with nothing exclusive running and the tracker
    /// connected, the request is taken and left for game output to perform.
    #[test]
    fn a_recentre_over_the_socket_is_accepted_and_left_for_the_pipeline() {
        use tobii_ipc::codec::Msg;
        use tobii_ipc::Client;

        let path =
            std::env::temp_dir().join(format!("tobii-recentre-ok-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let server = Server::bind_at(&path).expect("bind");
        let demand = Demand::new();
        let state = Arc::new(Mutex::new(DeviceState {
            status: ConnStatus::Connected,
            ..DeviceState::default()
        }));
        let lease = Arc::new(Mutex::new(Lease::Free));
        let (mut holds, mut awaiting, mut last) = (Holds::default(), Vec::new(), None);
        let recentring = Recentring::default();

        let mut client = Client::connect_at(&path, subs::POSE, "a game").expect("connect");
        client.send(&Msg::Recentre).expect("the request is sent");

        let reply = wait_for_reply(
            &server,
            &demand,
            &state,
            &lease,
            &mut holds,
            &mut awaiting,
            &mut last,
            &recentring,
            &client,
        );
        assert!(
            matches!(reply, Msg::RecentreReply { ok: true, .. }),
            "expected the request to be taken, got {reply:?}"
        );
        assert!(
            recentring.take(Instant::now()),
            "the request must be waiting for whichever pipeline runs next"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Pump `tick` until the client has an answer.
    ///
    /// The hello and the request arrive on the server's accept thread, so a
    /// single tick proves nothing about timing on a loaded machine — the same
    /// reason the lease tests above poll rather than sleep.
    #[allow(clippy::too_many_arguments)]
    fn wait_for_reply(
        server: &Server,
        demand: &Demand,
        state: &Arc<Mutex<DeviceState>>,
        lease: &Arc<Mutex<Lease>>,
        holds: &mut Holds,
        awaiting: &mut Vec<ClientId>,
        last: &mut Option<StatusCode>,
        recentring: &Recentring,
        client: &tobii_ipc::Client,
    ) -> tobii_ipc::codec::Msg {
        for _ in 0..200 {
            tick(
                server, demand, state, lease, holds, awaiting, last, recentring,
            );
            if let Some(m) = client
                .poll()
                .into_iter()
                .find(|m| matches!(m, tobii_ipc::codec::Msg::RecentreReply { .. }))
            {
                return m;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("no recentre reply arrived");
    }
    /// A config with game output on, watching one address.
    fn watching(addr: &str) -> OutputConfig {
        OutputConfig {
            enabled: true,
            opentrack: Some(addr.to_string()),
            ..OutputConfig::default()
        }
    }

    /// The claim the feature rests on, with the probe stubbed: a socket bound
    /// where we send is a `DemandGuard`, and it goes away with the socket.
    #[test]
    fn a_bound_socket_lights_the_tracker_and_closing_it_puts_it_out() {
        let demand = Demand::new();
        let mut watch = PortWatch::default();
        let t0 = Instant::now();
        let target = || watch_target(&watching("127.0.0.1:4242"));

        watch.poll(&demand, t0, target, |_| Listening::No);
        assert!(!demand.active(), "nothing bound there yet");

        watch.poll(&demand, t0 + LISTENER_POLL, target, |_| Listening::Yes);
        assert!(demand.active(), "opentrack started; the tracker comes on");
        assert_eq!(
            demand.reasons(),
            vec![LISTENER_REASON],
            "and says which cause it is"
        );

        watch.poll(&demand, t0 + 2 * LISTENER_POLL, target, |_| Listening::No);
        assert!(!demand.active(), "opentrack closed; the tracker goes dark");
        assert!(!watch.holding());
    }

    /// A listener that stays put is one hold that stays put — polling it does
    /// not accumulate anything that has to be released one at a time.
    ///
    /// An invariant pin rather than a behaviour gate, and worth saying so: the
    /// obvious alternative spelling (assign a fresh `hold` each poll) passes
    /// this too, because assignment drops the old guard after taking the new
    /// one. What it would catch is a version that keeps a guard per poll.
    #[test]
    fn a_listener_that_stays_is_still_only_one_hold() {
        let demand = Demand::new();
        let mut watch = PortWatch::default();
        let t0 = Instant::now();
        let target = || watch_target(&watching("127.0.0.1:4242"));

        for i in 0..10u32 {
            watch.poll(&demand, t0 + i * LISTENER_POLL, target, |_| Listening::Yes);
        }
        assert_eq!(demand.reasons().len(), 1);

        watch.poll(&demand, t0 + 10 * LISTENER_POLL, target, |_| Listening::No);
        assert!(
            !demand.active(),
            "ten polls must not need ten releases to go dark"
        );
    }

    /// `Unknown` is not `No`, and it is certainly not `Yes`: an address on
    /// another machine cannot be seen from here, so nothing may be claimed
    /// about it in either direction.
    #[test]
    fn an_unanswerable_probe_never_holds_the_tracker() {
        let demand = Demand::new();
        let mut watch = PortWatch::default();
        let t0 = Instant::now();
        let target = || watch_target(&watching("192.168.1.7:4242"));

        watch.poll(&demand, t0, target, |_| {
            Listening::Unknown(tobii_output::listener::NOT_LOCAL)
        });
        assert!(!demand.active());

        // And a real probe of a remote address answers exactly that, so the
        // stub above is the case that actually occurs.
        assert!(matches!(
            tobii_output::listener::probe("192.168.1.7:4242".parse().unwrap()),
            Listening::Unknown(_)
        ));
    }

    /// Nothing is watched that the user did not ask for. In particular game
    /// output being off means the hub is not sending anywhere, and lighting an
    /// infrared lamp for a socket we would ignore is the exact thing the
    /// standby rule forbids.
    #[test]
    fn nothing_is_watched_unless_the_user_asked_for_it() {
        assert_eq!(
            watch_target(&OutputConfig::default()),
            None,
            "game output is off out of the box, so a fresh install watches nothing"
        );
        assert_eq!(
            watch_target(&OutputConfig {
                wake_for_opentrack: false,
                ..watching("127.0.0.1:4242")
            }),
            None,
            "and the user can turn the watch itself off"
        );
        assert_eq!(
            watch_target(&OutputConfig {
                opentrack: None,
                ..watching("127.0.0.1:4242")
            }),
            None,
            "no destination, nothing to watch"
        );
        assert_eq!(
            watch_target(&watching("not an address")),
            None,
            "an unparseable address is watched as nothing, not panicked on"
        );
        assert_eq!(
            watch_target(&watching("127.0.0.1:4242")),
            Some("127.0.0.1:4242".parse().unwrap())
        );
    }

    /// Turning the watch off while a listener is up gives the tracker back,
    /// rather than holding it until the other program happens to close.
    #[test]
    fn switching_the_watch_off_releases_a_hold_it_is_already_holding() {
        let demand = Demand::new();
        let mut watch = PortWatch::default();
        let t0 = Instant::now();

        watch.poll(
            &demand,
            t0,
            || watch_target(&watching("127.0.0.1:4242")),
            |_| Listening::Yes,
        );
        assert!(demand.active());

        watch.poll(&demand, t0 + LISTENER_POLL, || None, |_| Listening::Yes);
        assert!(!demand.active(), "switched off means switched off");
    }

    /// The scan walks every UDP socket on the machine, so it runs at its own
    /// rate rather than the socket loop's 50 ms — and the config file is not
    /// read on the ticks in between either.
    #[test]
    fn the_port_is_checked_about_once_a_second_not_at_the_socket_cadence() {
        assert!(
            LISTENER_POLL >= 20 * POLL,
            "a /proc scan per socket tick would be 20 a second"
        );

        let demand = Demand::new();
        let mut watch = PortWatch::default();
        let t0 = Instant::now();
        let looked = std::cell::Cell::new(0);
        let asked = std::cell::Cell::new(0);

        // One second of the socket loop's ticks.
        for i in 0..20u32 {
            watch.poll(
                &demand,
                t0 + POLL * i,
                || {
                    asked.set(asked.get() + 1);
                    watch_target(&watching("127.0.0.1:4242"))
                },
                |_| {
                    looked.set(looked.get() + 1);
                    Listening::No
                },
            );
        }
        assert_eq!(looked.get(), 1, "one scan in a second, not twenty");
        assert_eq!(asked.get(), 1, "and one config read, not twenty");

        watch.poll(
            &demand,
            t0 + LISTENER_POLL,
            || {
                asked.set(asked.get() + 1);
                watch_target(&watching("127.0.0.1:4242"))
            },
            |_| {
                looked.set(looked.get() + 1);
                Listening::No
            },
        );
        assert_eq!(looked.get(), 2, "and it does look again once it is due");
    }

    /// Through the real kernel, with no wrapper anywhere in it: something binds
    /// the address the hub sends to, and the tracker comes on.
    ///
    /// This is the whole point of the feature, so it is worth one test that
    /// stubs nothing — the port is OS-assigned rather than 4242 so it neither
    /// collides with a real opentrack nor depends on the uid of whoever runs it.
    #[test]
    fn a_real_socket_on_the_opentrack_address_is_enough_to_light_the_tracker() {
        use std::net::UdpSocket;

        let sock = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let addr = sock.local_addr().expect("local addr");
        let cfg = watching(&addr.to_string());

        let demand = Demand::new();
        let mut watch = PortWatch::default();
        let t0 = Instant::now();
        watch.poll(
            &demand,
            t0,
            || watch_target(&cfg),
            tobii_output::listener::probe,
        );
        assert!(
            demand.active(),
            "a socket bound at {addr} should have lit the tracker"
        );

        drop(sock);
        watch.poll(
            &demand,
            t0 + LISTENER_POLL,
            || watch_target(&cfg),
            tobii_output::listener::probe,
        );
        assert!(!demand.active(), "and closing it should put it out");
    }

    /// Two different causes must read as two different sentences, or the user
    /// who asks why the tracker is on is sent looking for the wrong program.
    #[test]
    fn the_listener_reason_is_its_own_cause_and_reads_as_a_phrase() {
        assert_ne!(Some(LISTENER_REASON), reason_for(subs::POSE));
        assert!(LISTENER_REASON.contains("opentrack"));
        assert!(LISTENER_REASON
            .chars()
            .next()
            .is_some_and(|c| c.is_lowercase()));
        assert!(
            !crate::device::wants_exclusive(&[LISTENER_REASON]),
            "a listener is watching, not a stateful conversation with the device"
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
