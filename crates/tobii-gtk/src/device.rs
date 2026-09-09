//! The device thread: owns the blocking `Connection`, publishes a `DeviceState`
//! snapshot for the UI, and applies `DeviceCommand`s. `device_tick` (one
//! iteration) is generic over `Transport` so it is unit-tested without hardware.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tobii_protocol::camera::decode_camera_frame;
use tobii_protocol::frame::OP_GAZE_NOTIFY;
use tobii_protocol::{CameraFrame, DisplayCorners, EnabledEye, GazeSample};
use tobii_usb::{Connection, Transport, UsbError, UsbTransport};

/// The eye-camera stream mirrored into the hub preview (one of the stereo pair).
const CAMERA_STREAM: u16 = 0x501;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum ConnStatus {
    /// Nothing needs the tracker, so it is not opened at all.
    ///
    /// The default, because a freshly started process has not yet been asked
    /// for anything. See [`Demand`].
    #[default]
    Idle,
    Connecting,
    Connected,
    Error(String),
}

/// Reference-counted reasons to keep the eye tracker running.
///
/// The tracker's infrared illuminators are on for as long as a USB session is
/// open, and a bar of IR LEDs glowing at you from under the monitor all day is
/// exactly the kind of thing that makes people unplug a device. So the session
/// is opened only while something actually wants data, and closed again when
/// nothing does.
///
/// Every consumer takes a [`DemandGuard`] and holds it for as long as it needs
/// frames: the hub while its window has focus, the gaze overlay while it is
/// shown, a calibration or setup flow while it runs. Dropping the last guard
/// closes the session, which the ET5 answers by rebooting — which is also why
/// the display area, eye selection and calibration are re-applied on every
/// connect.
///
/// A second, unplanned benefit: while nothing in the hub wants the tracker, the
/// USB device is free, so `tobii headpose` can claim it for a game without the
/// hub having to be closed first.
#[derive(Clone, Default)]
pub struct Demand {
    held: Arc<Mutex<BTreeMap<&'static str, usize>>>,
}

impl Demand {
    pub fn new() -> Demand {
        Demand::default()
    }

    /// Ask for the tracker, until the returned guard is dropped.
    ///
    /// `reason` is shown to the user when they ask why the tracker is on, so it
    /// should read as a phrase in a sentence: "head tracking", "the gaze
    /// preview".
    pub fn hold(&self, reason: &'static str) -> DemandGuard {
        *self.held.lock().unwrap().entry(reason).or_insert(0) += 1;
        DemandGuard {
            held: Arc::clone(&self.held),
            reason,
        }
    }

    /// Whether anything currently wants the tracker.
    pub fn active(&self) -> bool {
        !self.held.lock().unwrap().is_empty()
    }

    /// What is keeping the tracker on, for the user to read.
    pub fn reasons(&self) -> Vec<&'static str> {
        self.held.lock().unwrap().keys().copied().collect()
    }
}

/// One consumer's claim on the tracker. Releases it when dropped.
pub struct DemandGuard {
    held: Arc<Mutex<BTreeMap<&'static str, usize>>>,
    reason: &'static str,
}

impl Drop for DemandGuard {
    fn drop(&mut self) {
        let mut held = self.held.lock().unwrap();
        if let Some(n) = held.get_mut(self.reason) {
            *n -= 1;
            if *n == 0 {
                held.remove(self.reason);
            }
        }
    }
}

/// How long the session stays open after the last consumer lets go.
///
/// Closing costs more than it looks: the ET5 reboots on session close, so the
/// next connect has to re-apply the display area, the eye selection and the
/// calibration blob before any data flows. Without a linger, alt-tabbing away
/// from the hub and back would pay that twice, and clicking through the hub's
/// own dialogs — each of which briefly moves focus — would thrash it. Three
/// seconds is long enough to cover both and short enough that the LEDs go out
/// while the user is still looking at them.
const LINGER: Duration = Duration::from_secs(3);

/// Hands out process-unique calibration session tokens. The UI mints one per
/// `CalBegin` and only trusts a `CalPhase` that carries it back (see
/// [`CalPhase::token`]); starting at 1 keeps `CalPhase::default()`'s 0 a token
/// that can never match a real session.
static NEXT_CAL_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Mint a fresh calibration session token.
pub fn next_cal_token() -> u64 {
    NEXT_CAL_TOKEN.fetch_add(1, Ordering::Relaxed)
}

/// Progress of an in-flight calibration, published to the UI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalPhase {
    /// The session token this phase belongs to, echoed from `CalBegin`. Every
    /// other field is meaningless to the UI until this matches the token it
    /// minted: `active`/`collected`/`last_error`/`finished` all survive from
    /// the previous session until the device thread dequeues a command, so
    /// level-testing them races the queue. 0 = no session (see
    /// [`next_cal_token`]).
    pub token: u64,
    /// True between `CalBegin` being *dequeued* and `CalFinish`/`CalAbort`.
    /// Deliberately set before any USB traffic: the device thread's idle
    /// watchdog uses it to suppress disconnect detection for the whole session,
    /// including the seconds `start`/`clear` may take. It is NOT evidence that
    /// the session opened — see [`CalPhase::started`].
    pub active: bool,
    /// True once `start` AND `clear` have both been acked, i.e. the device is
    /// really in a calibration session and point collection may begin. The UI
    /// waits on this rather than `active`, which would only prove the command
    /// left the queue.
    pub started: bool,
    /// Points successfully collected so far this session.
    pub collected: usize,
    /// Set when the last `CalCollect` failed (per-point error to surface).
    pub last_error: Option<String>,
    /// Set once the finish path resolves: `Ok` on success, `Err(msg)` on failure.
    pub finished: Option<Result<(), String>>,
}

impl CalPhase {
    /// A fresh in-progress phase (0 points collected) tagged with `token`.
    pub fn begin(token: u64) -> Self {
        CalPhase {
            token,
            active: true,
            started: false,
            collected: 0,
            last_error: None,
            finished: None,
        }
    }
    /// Record that `start` + `clear` both acked: the session is really open.
    pub fn on_started(&mut self) {
        self.started = true;
    }
    /// Record a point-collection result: increment on success, else store error.
    pub fn on_collect(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.collected += 1;
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(e),
        }
    }
    /// Record the compute/finish outcome and leave calibration mode.
    pub fn on_finish(&mut self, result: Result<(), String>) {
        self.active = false;
        self.finished = Some(result);
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeviceState {
    pub status: ConnStatus,
    pub latest_gaze: Option<GazeSample>,
    /// Most recent decoded eye-camera frame ([`CAMERA_STREAM`]), for the hub
    /// preview. `None` until the camera stream is subscribed and a frame arrives.
    ///
    /// An `Arc` because the head-pose worker is handed the same frame, and a
    /// 78 KB copy per frame at 33 Hz to serve two readers is pure memcpy.
    pub latest_camera: Option<Arc<CameraFrame>>,
    /// Rolling per-eye extrapolation windows, smoothed depth and guidance
    /// damping (see `eyeview::EyeHistory`) — updated once per incoming gaze
    /// notification, here, not at display-consumption time. This placement is
    /// load-bearing twice over: consumption happens from several UI surfaces on
    /// independent redraw cadences, and both the extrapolation window and the
    /// text damping count in *frames*, so they need exactly one update per real
    /// incoming frame. It also matches the original software, which runs the
    /// same computation in its stream callback rather than its UI timer.
    pub(crate) eye_history: crate::eyeview::EyeHistory,
    /// The eye-position view for the CURRENT frame (gap-filled by
    /// extrapolation), or `None` before any gaze data has ever arrived.
    /// `widget::eye_view_for` reads this instead of recomputing from
    /// `latest_gaze` directly.
    pub eye_view: Option<crate::eyeview::EyeView>,
    pub enabled_eye: Option<EnabledEye>,
    pub calibration: CalPhase,
    /// Whether the device *may* be inside an open calibration realm. Set
    /// pessimistically before `start_calibration` is issued (a deadline error
    /// is not proof the device ignored the request) and cleared only once a
    /// stop is actually **acked**. Erring towards `true` costs at most one
    /// redundant stop; erring towards `false` strands the device in calibration
    /// mode. Deliberately separate from `calibration.active`, which `on_finish`
    /// clears on the start/clear failure path — exactly when a stop is needed.
    pub cal_session_open: bool,
    /// Progress and result of a head-pose pitch-zero measurement.
    pub pitch_cal: PitchCal,
    /// Most recent head pose, as the opentrack stream would see it: position
    /// from the eye origins, rotation from the model. `None` until a model is
    /// installed AND a frame it was confident about has arrived.
    pub head_pose: Option<tobii_headpose::HeadPose>,
    /// The model's confidence for that pose — lower is better; see
    /// `tobii_headpose::onnx::SIGMA_MAX`. `None` when the pose is geometric.
    pub head_sigma: Option<f32>,
}

/// A pitch-zero measurement in flight, or its result.
///
/// The neural model reports pitch in its own frame, offset from level by the
/// training set's convention plus how far this tracker is tilted up. That sum is
/// fixed per installation and cannot be derived, so it is measured: sit
/// square-on, hold still, take the median.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PitchCal {
    /// Identifies the run this state belongs to.
    ///
    /// Without it a cancel could not be told from the start of the next run:
    /// the device thread can sit in `read_notifications` for a second, so a
    /// user who presses Start then Stop has both happen before the command is
    /// even dequeued. The UI mints the token and the device thread refuses to
    /// touch state carrying a different one.
    pub token: u64,
    pub active: bool,
    /// Seconds remaining, including the settle-in countdown.
    pub secs_left: u64,
    /// Accepted frames so far — a run that ends with very few of these was not
    /// a measurement, whatever number it would otherwise produce.
    pub samples: usize,
    /// `Ok(offset_deg)` once saved, or a human-readable failure.
    pub result: Option<Result<f64, String>>,
}

pub enum DeviceCommand {
    SetDisplayArea(DisplayCorners),
    SetEnabledEye(EnabledEye),
    /// Begin calibration: set the eye (experiment), then start + clear.
    /// `token` identifies this session; it is echoed into `CalPhase::token` so
    /// the UI can tell a fresh phase from the previous session's leftovers.
    CalBegin {
        /// Whether to seed from the calibration already on the device.
        ///
        /// The original gates both the retrieve and the re-apply on
        /// `ShouldImproveCalibration` — true only for an explicit
        /// "improve" recalibration. Every other entry (new profile, guest,
        /// forced) starts clean. Seeding unconditionally, as this did, hands a
        /// user who is recalibrating *because the old model is bad* that same
        /// bad model as the starting point.
        improve: bool,
        token: u64,
    },
    /// Measure the head-pose pitch zero over `secs` of held-still frames, then
    /// save it. Runs inline on the device thread, publishing progress into
    /// [`DeviceState::pitch_cal`]; the hub is briefly not updating gaze while it
    /// does, which is the correct trade for a measurement the user is holding
    /// still for anyway.
    PitchCalibrate {
        secs: u64,
        token: u64,
    },
    /// Rebuild the head-pose worker: pick up a model that was just installed or
    /// removed, and re-read the pitch zero.
    ///
    /// The worker resolves both once, when it is created, and a connection can
    /// live for hours. Without this, a model fetched from the hub did nothing
    /// until a restart, and a pitch zero measured in the hub was written to disk
    /// and then ignored by the very model that had just been used to measure it.
    ReloadHeadModel,
    /// Compute and apply the model from the points collected so far, without
    /// ending the session.
    ///
    /// The original does this after **every** group — once for the centre
    /// point, once for each row of three — so the outer points are collected
    /// against a tracker that already has a partial model applied, rather than
    /// against the factory one. Only the last compute is followed by a stop.
    CalComputeGroup,
    /// Sample one stimulus point (both eyes).
    CalCollect {
        x: f64,
        y: f64,
    },
    /// "Redo" a point: tell the device to discard whatever it collected for
    /// (x, y), so a later `CalCollect` for the same coordinates starts fresh.
    /// Best-effort — `discard_calibration_point` is reverse-engineered and
    /// unverified on real hardware; a failure here does not abort the session.
    CalDiscard {
        x: f64,
        y: f64,
    },
    /// Compute + apply + stop + retrieve + persist. `mode` is the calibration
    /// mode's label ("quick"/"full"), recorded into the saved `CalMeta` — a
    /// plain `String` rather than `calibrate_flow::CalMode` so this
    /// generic/testable device layer takes no dependency on a GTK-flow-owned
    /// enum.
    CalFinish {
        mode: String,
    },
    /// Abort: stop (best-effort) and reset.
    CalAbort,
}

/// Apply one command from the UI.
///
/// Split out of [`device_tick`] so a command that arrived while the tracker was
/// off — when there was no connection to apply it to — can be applied once one
/// is open, by the same code and in the same order.
///
/// Returns whether the head-pose worker should be rebuilt.
fn apply_command<T: Transport>(
    conn: &mut Connection<T>,
    state: &Mutex<DeviceState>,
    cmd: DeviceCommand,
) -> bool {
    let mut reload_head = false;
    match cmd {
        DeviceCommand::SetDisplayArea(c) => {
            let _ = conn.set_display_area(&c);
        }
        DeviceCommand::PitchCalibrate { secs, token } => {
            calibrate_pitch(conn, state, secs, token);
        }
        DeviceCommand::ReloadHeadModel => reload_head = true,
        DeviceCommand::SetEnabledEye(e) => {
            let _ = conn.set_enabled_eye(e);
            let _ = tobii_config::save_enabled_eye(e);
            state.lock().unwrap().enabled_eye = Some(e);
        }
        DeviceCommand::CalBegin { improve, token } => {
            state.lock().unwrap().calibration = CalPhase::begin(token);
            // Deliberately does NOT touch enabled_eye. The original never
            // does during calibration: `CalibrationStart` is hardcoded to
            // both eyes and the configured selection is used only to pick
            // which eye's gaze drives dot focus, host-side. Writing it here
            // mutated persistent device state as a side effect of opening
            // a calibration.

            // Retrieve whatever calibration is currently active on the
            // device BEFORE clearing it, so a successful start+clear below
            // can be re-seeded with it. This is what makes "Improve
            // calibration" (the hub's manual recalibration path)
            // meaningfully different from a from-scratch calibration: new
            // points refine the existing calibration instead of replacing
            // it outright. An empty/failed retrieve just means there was
            // nothing to improve on (e.g. a true first-ever calibration) —
            // proceed as a plain fresh session in that case, exactly as
            // before this change.
            let previous_blob = improve
                .then(|| conn.retrieve_calibration().ok())
                .flatten()
                .filter(|b| tobii_usb::is_plausible_calibration(&b.0));

            // Pessimistic: a request fails on a wall-clock deadline, which
            // is NOT proof the device ignored it — it may have entered the
            // realm while the ack was lost or late. Record "possibly open"
            // *before* issuing `start`, so the abort path always tries to
            // close it. A stale `true` costs one redundant stop; a `false`
            // with an open realm strands the device in calibration mode.
            state.lock().unwrap().cal_session_open = true;
            let r = conn
                .start_calibration()
                .and_then(|()| conn.clear_calibration())
                .map_err(|e| e.to_string());

            // Seeding from the previous calibration is an optimisation, not
            // a precondition: failing to seed just makes this a from-scratch
            // session, which is a perfectly good calibration. Keeping it in
            // the chain above made an unusable stored blob abort the whole
            // flow — every attempt failing instantly with "can't detect your
            // eyes", which is not remotely what went wrong.
            if r.is_ok() {
                if let Some(blob) = &previous_blob {
                    if let Err(e) = conn.apply_calibration(&blob.0) {
                        tobii_diagnostics::log::warn(&format!(
                            "note: starting from scratch, not seeding ({e})"
                        ));
                    }
                }
            }
            match r {
                // Only now is the session really open for point collection.
                Ok(()) => state.lock().unwrap().calibration.on_started(),
                Err(e) => state.lock().unwrap().calibration.on_finish(Err(e)),
            }
        }
        DeviceCommand::CalCollect { x, y } => {
            let r = conn
                .add_calibration_point(x, y, tobii_protocol::calibration::CAL_EYE_BOTH)
                .map_err(|e| e.to_string());
            state.lock().unwrap().calibration.on_collect(r);
        }
        DeviceCommand::CalDiscard { x, y } => {
            if let Err(e) = conn.discard_calibration_point(x, y) {
                tobii_diagnostics::log::warn(&format!(
                    "warning: could not discard calibration point ({e})"
                ));
            }
        }
        DeviceCommand::CalComputeGroup => {
            // A failure here is fatal to the session: the model is now in
            // an unknown state and collecting more points onto it would
            // produce a calibration nobody can reason about.
            if let Err(e) = conn.compute_and_apply_calibration() {
                state
                    .lock()
                    .unwrap()
                    .calibration
                    .on_finish(Err(e.to_string()));
            }
        }
        DeviceCommand::CalFinish { mode } => {
            let (r, stop_acked) = finish_calibration(conn, &mode);
            let mut s = state.lock().unwrap();
            // Issuing a stop is not the same as the realm closing: only an
            // acked stop proves that. If the ack was lost, leave the flag
            // set so a later abort retries the stop.
            if stop_acked {
                s.cal_session_open = false;
            }
            s.calibration.on_finish(r);
        }
        DeviceCommand::CalAbort => {
            // Only stop a realm that is actually open: after a successful
            // finish (which already stopped) a second stop may go
            // unanswered and would burn a whole request deadline here.
            // `cal_session_open` — not `calibration.active` — is the honest
            // predicate: `active` is also false after CalBegin's start
            // succeeded but clear failed, exactly when a stop is required.
            if state.lock().unwrap().cal_session_open {
                // Clear the flag only on an acked stop (see `CalFinish`).
                if conn.stop_calibration().is_ok() {
                    state.lock().unwrap().cal_session_open = false;
                }
                // `CalBegin` issues a destructive `clear`, and nothing else
                // puts the calibration back — without this an aborted or
                // failed session would leave the tracker uncalibrated for
                // the rest of the USB session. Having no saved blob is
                // normal (first ever run), not an error.
                match tobii_config::load_calibration() {
                    Ok(Some((blob, _meta))) => {
                        if let Err(e) = conn.apply_calibration(&blob) {
                            tobii_diagnostics::log::warn(&format!(
                                "warning: could not restore calibration ({e})"
                            ));
                        }
                    }
                    Ok(None) => {}
                    Err(e) => tobii_diagnostics::log::warn(&format!(
                        "warning: could not load saved calibration ({e})"
                    )),
                }
            }
            state.lock().unwrap().calibration = CalPhase::default();
        }
    }
    reload_head
}

/// One iteration: apply any queued commands, then read one transport chunk.
///
/// See [`Tick`] for what the return value distinguishes. The thread loop uses
/// sustained silence — not merely a tick that published nothing — to detect a
/// stalled or unplugged device.
pub fn device_tick<T: Transport>(
    conn: &mut Connection<T>,
    state: &Mutex<DeviceState>,
    cmd_rx: &Receiver<DeviceCommand>,
    head: Option<&HeadWorker>,
) -> Tick {
    let mut reload_head = false;
    while let Ok(cmd) = cmd_rx.try_recv() {
        reload_head |= apply_command(conn, state, cmd);
    }
    // Read one transport chunk and publish every gaze + camera frame in it (the
    // camera stream co-occurs with gaze, so read them together rather than via
    // the gaze-only queue).
    let notifications = conn.read_notifications();
    let saw_traffic = notifications.saw_traffic;
    let mut got = false;
    for (op, payload) in notifications {
        match op {
            OP_GAZE_NOTIFY => {
                if let Some(g) = GazeSample::decode(&payload) {
                    // Without a model the eye origins are the only head pose
                    // there is — 5 DOF, no pitch. Publishing it here is what
                    // makes yaw and roll appear in the hub on a fresh install;
                    // before this the readout showed "—" for all three even
                    // though the CLI reported two of them from the same data.
                    let geometric = head
                        .is_none()
                        .then(|| tobii_headpose::pose_from_sample(&g))
                        .flatten();
                    let mut s = state.lock().unwrap();
                    s.eye_view = Some(s.eye_history.update(&g));
                    s.latest_gaze = Some(g);
                    s.status = ConnStatus::Connected;
                    if head.is_none() {
                        s.head_pose = geometric;
                        s.head_sigma = None;
                    }
                }
                got = true;
            }
            op if op == CAMERA_STREAM as u32 => {
                if let Some(f) = decode_camera_frame(&payload) {
                    let f = Arc::new(f);
                    if let Some(w) = head {
                        // Pair the image with the eye origins from the newest
                        // gaze sample: the model supplies rotation, the eyes
                        // supply metric position. A dropped frame here costs the
                        // preview one update and nothing else.
                        let eyes = {
                            let s = state.lock().unwrap();
                            s.latest_gaze
                                .as_ref()
                                .and_then(tobii_headpose::pose_from_sample)
                        };
                        w.offer(Arc::clone(&f), eyes);
                    }
                    state.lock().unwrap().latest_camera = Some(f);
                }
                got = true;
            }
            _ => {}
        }
    }
    Tick {
        published: got,
        saw_traffic,
        reload_head,
    }
}

/// Runs the head-pose model off the device thread.
///
/// Inference is ~12 ms a frame against a 33 ms frame interval. That fits, but
/// not with room to spare, and the device thread is also publishing gaze — the
/// path where a stall shows up as visible lag. So frames go to a worker over a
/// one-slot channel and a frame that arrives while the worker is busy is
/// **dropped**, not queued: the preview wants the newest pose, and a queue would
/// turn a momentary overrun into permanent latency.
pub struct HeadWorker {
    tx: std::sync::mpsc::SyncSender<(Arc<CameraFrame>, Option<tobii_headpose::HeadPose>)>,
}

impl HeadWorker {
    /// `None` when no model is installed, which is the ordinary case.
    ///
    /// No pre-check for the file: `from_store` already reads it, hashes it and
    /// returns `ModelMissing` when it is absent. Asking `installed()` first read
    /// and digested the same 13 MB a second time, on the device thread, every
    /// time the connection was re-established.
    fn spawn(state: Arc<Mutex<DeviceState>>) -> Option<HeadWorker> {
        use tobii_headpose::onnx::{fuse, OnnxPose, RotationSource};
        let mut model = match OnnxPose::from_store() {
            Ok(m) => m,
            Err(tobii_headpose::onnx::OnnxError::ModelMissing(_)) => return None,
            Err(e) => {
                tobii_diagnostics::log::warn(&format!("head pose: {e}"));
                return None;
            }
        };
        if let Some(off) = tobii_headpose::model_store::pitch_offset() {
            let mut signs = model.tracker().signs();
            signs.pitch_offset_deg = off;
            model.tracker().set_signs(signs);
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            while let Ok((frame, eyes)) = rx.recv() {
                let frame: Arc<CameraFrame> = frame;
                let pose = model.estimate_detailed(&frame);
                let mut s = state.lock().unwrap();
                match pose {
                    Some(m) => {
                        s.head_pose = fuse(eyes, Some(&m), RotationSource::Model);
                        s.head_sigma = Some(m.sigma);
                    }
                    None => {
                        // Hold the last rotation rather than snapping to zero,
                        // but stop claiming a confidence for it.
                        s.head_sigma = None;
                        if let Some(e) = eyes {
                            s.head_pose = Some(match s.head_pose {
                                Some(prev) => tobii_headpose::HeadPose {
                                    x_mm: e.x_mm,
                                    y_mm: e.y_mm,
                                    z_mm: e.z_mm,
                                    ..prev
                                },
                                None => e,
                            });
                        }
                    }
                }
            }
        });
        Some(HeadWorker { tx })
    }

    /// Hand a frame over if the worker is idle; otherwise drop it.
    ///
    /// Takes an `Arc` because the common case is a *refused* send — the comment
    /// above says overruns are expected — and cloning 78 KB at 33 Hz to throw it
    /// away was 2.6 MB/s of memcpy on the device thread.
    fn offer(&self, frame: Arc<CameraFrame>, eyes: Option<tobii_headpose::HeadPose>) -> bool {
        self.tx.try_send((frame, eyes)).is_ok()
    }
}

/// Measure and save the head-pose pitch zero.
///
/// Takes over the connection for the duration rather than threading a model
/// through every tick. The hub stops updating gaze while it runs, which is
/// acceptable precisely because the user is sitting still for it — and the
/// alternative, holding a 13 MB model and 12 ms of inference inside the state
/// mutex the UI redraws from, is not.
fn calibrate_pitch<T: Transport>(
    conn: &mut Connection<T>,
    state: &Mutex<DeviceState>,
    secs: u64,
    token: u64,
) {
    // The UI put `active` and `token` in place before sending the command. If
    // either has changed, this run was cancelled or superseded while it sat in
    // the queue, and going ahead would spend 13 s measuring a user who has
    // walked away and then save it over their good calibration.
    if !current(state, token) {
        return;
    }
    let finish = |r: Result<f64, String>| {
        let mut s = state.lock().unwrap();
        if s.pitch_cal.token != token {
            return; // a newer run owns the state now
        }
        // Only the three fields that change. The struct literal this
        // replaced assigned `samples` back to itself and `token` back to
        // the value it had just been compared equal to, so a reader had to
        // check all five to find the two that moved.
        s.pitch_cal.active = false;
        s.pitch_cal.secs_left = 0;
        s.pitch_cal.result = Some(r);
    };

    let mut model = match tobii_headpose::onnx::OnnxPose::from_store() {
        Ok(m) => m,
        Err(e) => return finish(Err(e.to_string())),
    };
    // Measure the model's RAW pitch. Applying the saved offset while measuring a
    // new one would make each run a correction of the last, not a measurement.
    let mut signs = model.tracker().signs();
    signs.pitch_offset_deg = 0.0;
    model.tracker().set_signs(signs);

    // A settle-in pause, so the frames from while the user was still reaching
    // for the mouse are not part of the measurement.
    const SETTLE: Duration = Duration::from_secs(3);
    let start = Instant::now() + SETTLE;
    let deadline = start + Duration::from_secs(secs);
    let mut samples: Vec<f64> = Vec::new();
    {
        let mut s = state.lock().unwrap();
        s.pitch_cal.secs_left = secs + SETTLE.as_secs();
        s.pitch_cal.samples = 0;
    }

    while Instant::now() < deadline {
        for (op, payload) in conn.read_notifications().iter() {
            if *op != CAMERA_STREAM as u32 {
                continue;
            }
            let Some(frame) = decode_camera_frame(payload) else {
                continue;
            };
            if let Some(p) = model.estimate_detailed(&frame) {
                if Instant::now() >= start {
                    samples.push(p.pitch_deg);
                }
            }
            state.lock().unwrap().latest_camera = Some(Arc::new(frame));
        }
        let mut s = state.lock().unwrap();
        if s.pitch_cal.token != token || !s.pitch_cal.active {
            return; // cancelled from the UI, or superseded by a newer run
        }
        s.pitch_cal.secs_left = deadline.saturating_duration_since(Instant::now()).as_secs();
        s.pitch_cal.samples = samples.len();
    }

    match tobii_headpose::pitch_offset_from(&mut samples) {
        Some((offset, spread)) => match tobii_config::save_pitch_offset(offset) {
            Ok(()) => {
                if spread > 8.0 {
                    finish(Err(format!(
                        "saved, but your head moved during the measurement \
                         ({spread:.1}° of spread) — run it again if pitch looks off"
                    )));
                } else {
                    finish(Ok(offset));
                }
            }
            Err(e) => finish(Err(format!("could not save: {e}"))),
        },
        None => finish(Err(format!(
            "only {} usable frames — was your face in view? Nothing was saved.",
            samples.len()
        ))),
    }
}

/// Whether `token` still owns the pitch-calibration state.
fn current(state: &Mutex<DeviceState>, token: u64) -> bool {
    let s = state.lock().unwrap();
    s.pitch_cal.token == token && s.pitch_cal.active
}

/// What one [`device_tick`] observed, for the reconnect watchdog.
///
/// `published` and `saw_traffic` differ exactly when a transport chunk ended
/// mid-frame: real traffic, no complete frame yet. Only a lack of *traffic*
/// says anything about the device still being there.
#[derive(Debug, Clone, Copy)]
pub struct Tick {
    pub published: bool,
    pub saw_traffic: bool,
    /// A [`DeviceCommand::ReloadHeadModel`] arrived; the loop owns the worker,
    /// so it does the rebuilding.
    pub reload_head: bool,
}

/// Spawn the device thread. It handshakes, then loops `device_tick`; on any
/// connection failure it records the error and retries after a short delay.
pub fn spawn() -> (Arc<Mutex<DeviceState>>, Sender<DeviceCommand>, Demand) {
    let state = Arc::new(Mutex::new(DeviceState::default()));
    let (tx, rx) = channel::<DeviceCommand>();
    let demand = Demand::new();
    let thread_state = Arc::clone(&state);
    let thread_demand = demand.clone();
    std::thread::spawn(move || {
        // Commands that arrived while the tracker was off. They are not
        // dropped: "select left eye only" typed into an idle hub has to take
        // effect, so a queued command is itself a reason to open the session.
        let mut pending: Vec<DeviceCommand> = Vec::new();
        loop {
            // Nothing wants the tracker: do not open it. This is the whole
            // point of `Demand` — an idle hub leaves the illuminators dark and
            // the USB device free for `tobii headpose`.
            while !thread_demand.active() && pending.is_empty() {
                while let Ok(cmd) = rx.try_recv() {
                    pending.push(cmd);
                }
                if !pending.is_empty() {
                    break;
                }
                {
                    let mut st = thread_state.lock().unwrap();
                    if !matches!(st.status, ConnStatus::Idle) {
                        *st = DeviceState {
                            status: ConnStatus::Idle,
                            ..DeviceState::default()
                        };
                    }
                }
                std::thread::sleep(Duration::from_millis(120));
            }
            device_session(&thread_state, &thread_demand, &rx, &mut pending);
        }
    });
    (state, tx, demand)
}

/// Tell whoever is waiting that a queued command will never run.
///
/// Only the commands that hand out a token need this: the rest are settings,
/// which are already on disk and get re-applied on the next connect. A watcher
/// polling `DeviceState` for its token has no other way to learn the command
/// died with the connection attempt.
fn fail_queued_command(state: &Mutex<DeviceState>, cmd: DeviceCommand, e: &UsbError) {
    match cmd {
        DeviceCommand::PitchCalibrate { token, .. } => {
            let mut s = state.lock().unwrap();
            s.pitch_cal = PitchCal {
                token,
                active: false,
                secs_left: 0,
                samples: 0,
                result: Some(Err(format!("the tracker could not be opened: {e}"))),
            };
        }
        DeviceCommand::CalBegin { token, .. } => {
            let mut s = state.lock().unwrap();
            s.calibration = CalPhase::begin(token);
            s.calibration
                .on_finish(Err(format!("the tracker could not be opened: {e}")));
        }
        // Settings and the head-model reload carry no token and are recovered
        // from disk on the next connect.
        _ => {}
    }
}

/// One connection, from open to close.
///
/// Returns as soon as the connection is gone — because it failed, because the
/// device went quiet, or because nothing wants the tracker any more.
fn device_session(
    thread_state: &Arc<Mutex<DeviceState>>,
    demand: &Demand,
    rx: &Receiver<DeviceCommand>,
    pending: &mut Vec<DeviceCommand>,
) {
    {
        thread_state.lock().unwrap().status = ConnStatus::Connecting;
        match UsbTransport::open().and_then(Connection::connect) {
            Ok(mut conn) => {
                // The ET5 resets its display area to a stub on every reboot (it
                // reboots on each session close), and emits no eye-tracking data
                // until a valid area is set. Re-apply the saved config in-session
                // on every (re)connect — without this the device never detects.
                if let Ok(Some(setup)) = tobii_config::load() {
                    let _ = conn.set_display_area(&setup.to_corners());
                }
                // Re-apply the saved eye selection (reboot-persistence is
                // unverified), then read the device's current value to seed the UI.
                if let Ok(Some(eye)) = tobii_config::load_enabled_eye() {
                    let _ = conn.set_enabled_eye(eye);
                }
                // The ET5 wipes calibration on reboot like the display area;
                // re-apply the saved blob so calibration persists across sessions.
                if let Ok(Some((blob, _meta))) = tobii_config::load_calibration() {
                    // OP_CAL_APPLY is disasm-derived and runs on every connect;
                    // don't let a rejection pass silently as bad tracking.
                    if let Err(e) = conn.apply_calibration(&blob) {
                        tobii_diagnostics::log::warn(&format!(
                            "warning: could not apply saved calibration ({e})"
                        ));
                    }
                }
                // Subscribe to one eye-camera for the hub preview (best-effort:
                // the preview is optional, gaze/calibration work without it).
                let _ = conn.subscribe_stream(CAMERA_STREAM);
                let cur_eye = conn.get_enabled_eye().ok().flatten();
                {
                    let mut s = thread_state.lock().unwrap();
                    s.enabled_eye = cur_eye;
                    s.status = ConnStatus::Connected;
                    // A brand-new connection is never inside a calibration
                    // realm, whatever the previous one was doing.
                    s.cal_session_open = false;
                }
                // Watchdog on the wall clock, not on a tick count. A tick that
                // publishes no frame is not evidence of a disconnect: a
                // transport chunk that ends mid-frame is ordinary traffic, and
                // sleeping on it stalls the gaze path for as long as the sleep
                // lasts while samples queue up behind it — a freeze, then a
                // jump when they all land at once. Back off only when the
                // transport itself went silent, and even then only briefly,
                // since `read_notifications` already blocks for up to a second.
                // One model, one worker, for the life of the connection.
                let mut head = HeadWorker::spawn(thread_state.clone());
                let mut quiet_since = Instant::now();
                // Anything queued while the tracker was off is applied now that
                // there is a connection to apply it to.
                for cmd in pending.drain(..) {
                    if apply_command(&mut conn, thread_state, cmd) {
                        head = HeadWorker::spawn(thread_state.clone());
                    }
                }
                // When the last consumer let go. `None` while something still
                // wants the tracker.
                let mut idle_since: Option<Instant> = None;
                loop {
                    if demand.active() {
                        idle_since = None;
                    } else {
                        let since = *idle_since.get_or_insert_with(Instant::now);
                        if since.elapsed() >= LINGER {
                            // Dropping `conn` closes the session, which the ET5
                            // answers by rebooting — and the illuminators go out.
                            break;
                        }
                    }
                    let tick = device_tick(&mut conn, thread_state, rx, head.as_ref());
                    if tick.reload_head {
                        // Dropping the old worker closes its channel, so its
                        // thread ends after the frame it is on.
                        head = HeadWorker::spawn(thread_state.clone());
                    }
                    let calibrating = thread_state.lock().unwrap().calibration.active;
                    if tick.published || calibrating {
                        quiet_since = Instant::now();
                    } else {
                        if !tick.saw_traffic {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        if quiet_since.elapsed() >= Duration::from_secs(2) {
                            break; // silent for 2s -> assume disconnect; outer loop reconnects
                        }
                    }
                }
            }
            Err(e) => {
                thread_state.lock().unwrap().status = ConnStatus::Error(e.to_string());
                // A command queued while the tracker was off is a reason to
                // OPEN the session, and this attempt to open it failed. Drop
                // the queue rather than carrying it: `pending` is only emptied
                // inside the success arm, so a command that can never be
                // applied — the commonest case being a setting changed with the
                // tracker unplugged — kept the thread out of its idle wait
                // forever, retrying `UsbTransport::open` every 750 ms and
                // pinning the status to "Disconnected". One such command
                // permanently defeated the demand gate.
                //
                // Dropping is right rather than merely convenient: a setting
                // is written to disk where it is CHOSEN, so it is re-applied
                // from saved config on the next successful connect. (That was
                // not true when this drop was introduced — the eye selection
                // was persisted inside `apply_command`, so dropping the command
                // lost it permanently. It is saved by the UI now.)
                //
                // What must NOT simply vanish is a command some part of the UI
                // is waiting on. A pitch calibration or a calibration session
                // hands out a token and then watches `DeviceState` for it; if
                // the command is dropped in silence that watcher waits forever
                // on a token that can never arrive.
                if !pending.is_empty() {
                    tobii_diagnostics::log::warn(&format!(
                        "warning: could not reach the tracker to apply {} queued command(s) ({e})",
                        pending.len()
                    ));
                    for cmd in pending.drain(..) {
                        fail_queued_command(thread_state, cmd, &e);
                    }
                }
                std::thread::sleep(Duration::from_millis(750));
            }
        }
    }
}

/// Compute + stop + retrieve + persist. Always attempts `stop` so the device is
/// not left in calibration mode even when compute fails.
/// Returns the outcome alongside whether the stop was *acked* — only an acked
/// stop proves the realm closed, so the caller must not clear
/// `cal_session_open` without it.
fn finish_calibration<T: Transport>(
    conn: &mut Connection<T>,
    mode: &str,
) -> (Result<(), String>, bool) {
    // Timed, because the duration is the one cheap tell that the commit op is
    // the right one. `0x42e` computes a model and takes well over a second;
    // `0x42f`, which this used to send, returns in ~230 ms having changed
    // nothing. Logging it means the next real calibration settles that on this
    // hardware instead of resting on a second-hand claim — see
    // `frame::OP_CAL_COMPUTE`.
    let started = Instant::now();
    let compute = conn
        .compute_and_apply_calibration()
        .map_err(|e| e.to_string());
    let compute_took = started.elapsed();
    // Stop unconditionally, even when compute failed.
    let stop_acked = conn.stop_calibration().is_ok();
    let outcome = compute.and_then(|()| {
        let blob = conn.retrieve_calibration().map_err(|e| e.to_string())?;
        let previous = tobii_config::load_calibration()
            .ok()
            .flatten()
            .map(|(b, _)| b.len());
        tobii_diagnostics::log::warn(&format!(
            "calibration: compute {:?}, blob {} bytes{}{}",
            compute_took,
            blob.0.len(),
            match previous {
                Some(n) if n == blob.0.len() => " (SAME SIZE as the stored one)",
                Some(_) => " (size changed)",
                None => "",
            },
            if compute_took < Duration::from_millis(500) {
                " -- suspiciously fast for a real computation"
            } else {
                ""
            }
        ));
        if blob.0.is_empty() {
            // Persisting an empty blob would re-apply nothing on every connect
            // and silently mask that the calibration was never stored.
            return Err("device returned an empty calibration".into());
        }
        let meta = tobii_config::CalMeta {
            monitor_id: active_monitor_id(),
            created_utc: now_unix_secs(),
            mode: mode.to_string(),
            display_fingerprint: tobii_config::load()
                .ok()
                .flatten()
                .map(|s| s.fingerprint())
                .unwrap_or(0),
        };
        tobii_config::save_calibration(&blob.0, &meta).map_err(|e| e.to_string())
    });
    (outcome, stop_acked)
}

/// Best-effort id of the screen the tracker is on. Prefers the monitor ID
/// chosen by the user during setup, but only if that monitor is still among
/// the currently detected ones — a stale sidecar value must NOT be trusted
/// unconditionally, since `decide()`'s "is the tracker on a different screen
/// now?" check compares this function's result against the calibration's own
/// saved `monitor_id` (itself written from this same function, at save time)
/// — if this always returned the static sidecar value regardless of what's
/// actually connected, that comparison could never detect a real screen
/// change (see `calibration_state::decide`'s `RecommendReason::OtherScreen`).
/// Falls back to the largest-by-area heuristic when the sidecar is stale/
/// missing, for installs from before the monitor picker existed, for
/// single-monitor systems where nothing needs picking, and for a picked
/// monitor that's since been disconnected/replaced.
pub(crate) fn active_monitor_id() -> Option<String> {
    let monitors = tobii_config::detect_monitors();

    // Try the monitor ID chosen by the user during setup first, but only if
    // it's still among the currently detected monitors.
    if let Ok(Some(id)) = tobii_config::load_setup_monitor_id() {
        if monitors
            .iter()
            .any(|m| m.id.as_deref() == Some(id.as_str()))
        {
            return Some(id);
        }
    }

    // Fall back to the largest-by-area heuristic.
    tobii_config::pick_monitor(&monitors).and_then(|m| m.id.clone())
}

/// Seconds since the Unix epoch, clamped to 0 on a clock before 1970 (never
/// actually observed; a saturating fallback beats propagating a `Result` for it).
fn now_unix_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {

    /// The whole point of `Demand`: nothing asked, nothing opened. If this ever
    /// reports active with no guards held, the tracker's illuminators come on
    /// at login and stay on until logout.
    #[test]
    fn nothing_wants_the_tracker_until_something_asks() {
        let d = Demand::new();
        assert!(!d.active(), "a fresh Demand must not open the tracker");
        assert!(d.reasons().is_empty());

        let g = d.hold("the hub window");
        assert!(d.active());
        assert_eq!(d.reasons(), vec!["the hub window"]);

        drop(g);
        assert!(!d.active(), "the last guard going must close the session");
        assert!(d.reasons().is_empty());
    }

    /// Guards nest. The hub holding one while a calibration flow holds another
    /// must not let the first one released close the tracker under the second.
    #[test]
    fn the_tracker_stays_on_until_every_consumer_has_let_go() {
        let d = Demand::new();
        let hub = d.hold("the hub window");
        let cal = d.hold("calibration");
        let overlay = d.hold("the gaze preview");
        assert_eq!(d.reasons().len(), 3);

        drop(hub);
        assert!(d.active(), "two consumers still want it");
        drop(cal);
        assert!(d.active(), "the overlay still wants it");
        drop(overlay);
        assert!(!d.active());
    }

    /// Two claims for the same reason — two calibration windows, or a flow
    /// reopened before the old one finished being torn down — must be counted,
    /// not collapsed. Collapsing them means the first close turns the tracker
    /// off under the second.
    #[test]
    fn two_claims_for_the_same_reason_are_counted_separately() {
        let d = Demand::new();
        let a = d.hold("calibration");
        let b = d.hold("calibration");
        assert_eq!(d.reasons(), vec!["calibration"], "shown once");

        drop(a);
        assert!(d.active(), "the second claim is still held");
        drop(b);
        assert!(!d.active());
    }

    /// Guards are handed to GTK closures that can be dropped on any thread the
    /// main loop happens to finalise them on, and the device thread reads
    /// `active()` continuously.
    #[test]
    fn demand_is_safe_to_hold_and_release_from_several_threads() {
        let d = Demand::new();
        let outer = d.hold("the hub window");
        let mut handles = Vec::new();
        for i in 0..8 {
            let d = d.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..500 {
                    let _g = d.hold(if i % 2 == 0 {
                        "calibration"
                    } else {
                        "display setup"
                    });
                    assert!(d.active(), "our own guard is held");
                }
            }));
        }
        // The device thread's view, running concurrently.
        for _ in 0..2_000 {
            assert!(d.active(), "the outer guard is never released");
            let _ = d.reasons();
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(d.reasons(), vec!["the hub window"], "all inner guards gone");
        drop(outer);
        assert!(!d.active());
    }

    use super::*;

    use std::collections::VecDeque;
    use std::sync::mpsc::channel;
    use std::sync::Mutex;
    use std::time::Duration;
    use tobii_protocol::frame::{ENVELOPE_SIZE, TTP_HDR_SIZE, TTP_MAGIC_NOTIFY, TTP_MAGIC_RSP};
    use tobii_protocol::tlv::{write_f64_q42, write_tag, write_u32, TAG_POINT2D, TAG_XDS_COLUMN};
    use tobii_usb::{Connection, Transport, UsbError};

    // Minimal inbound-frame + gaze-payload helpers (same wire shape the usb tests use).
    fn inbound(magic: u32, seq: u32, op: u32, payload: &[u8]) -> Vec<u8> {
        let total = (ENVELOPE_SIZE + TTP_HDR_SIZE + payload.len()) as u32;
        let mut v = vec![0x01, 0, 0, 0];
        v.extend_from_slice(&total.to_le_bytes());
        v.extend_from_slice(&magic.to_be_bytes());
        v.extend_from_slice(&seq.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&op.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }
    fn realm_type_zero() -> Vec<u8> {
        let mut p = vec![0x00, 0x00, 0x02, 0x00, 0x00, 0x04];
        p.extend_from_slice(&0u32.to_be_bytes());
        p
    }
    fn gaze_payload() -> Vec<u8> {
        let mut w = tobii_protocol::bytes::Writer::new();
        w.push_u8(0x00);
        w.push_u8(0x00);
        write_tag(&mut w, (2u32 << 16) | 0x0bb8);
        write_tag(&mut w, TAG_XDS_COLUMN);
        write_u32(&mut w, 0x01);
        w.push_u8(6);
        w.push_be32(8);
        w.push_be64(42i64 as u64);
        write_tag(&mut w, TAG_XDS_COLUMN);
        write_u32(&mut w, 0x1c);
        write_tag(&mut w, TAG_POINT2D);
        write_f64_q42(&mut w, 0.25);
        write_f64_q42(&mut w, 0.75);
        w.into_vec()
    }
    struct MockTransport {
        sent: Vec<Vec<u8>>,
        to_recv: VecDeque<Vec<u8>>,
    }
    impl Transport for MockTransport {
        fn send(&mut self, data: &[u8]) -> Result<(), UsbError> {
            self.sent.push(data.to_vec());
            Ok(())
        }
        fn recv(&mut self, buf: &mut [u8], _t: Duration) -> Option<usize> {
            let next = self.to_recv.pop_front()?;
            buf[..next.len()].copy_from_slice(&next);
            Some(next.len())
        }
    }
    fn connected(post: Vec<Vec<u8>>) -> Connection<MockTransport> {
        let mut to_recv = VecDeque::from(vec![
            inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
            inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
            inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
            Vec::new(),
        ]);
        to_recv.extend(post);
        Connection::connect(MockTransport {
            sent: Vec::new(),
            to_recv,
        })
        .expect("connect")
    }

    #[test]
    fn tick_publishes_latest_gaze() {
        let mut conn = connected(vec![inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &gaze_payload())]);
        let state = Mutex::new(DeviceState::default());
        let (_tx, rx) = channel::<DeviceCommand>();
        assert!(device_tick(&mut conn, &state, &rx, None).published);
        let g = state
            .lock()
            .unwrap()
            .latest_gaze
            .clone()
            .expect("gaze published");
        assert_eq!(g.timestamp_us, 42);
    }

    #[test]
    fn tick_returns_false_when_no_gaze() {
        let mut conn = connected(vec![]);
        let state = Mutex::new(DeviceState::default());
        let (_tx, rx) = channel::<DeviceCommand>();
        assert!(!device_tick(&mut conn, &state, &rx, None).published);
    }

    #[test]
    fn cal_phase_begin_is_active_and_empty() {
        let p = CalPhase::begin(7);
        assert!(p.active);
        assert_eq!(p.token, 7);
        assert_eq!(p.collected, 0);
        assert!(p.last_error.is_none());
        assert!(p.finished.is_none());
    }

    #[test]
    fn cal_tokens_are_unique_and_never_zero() {
        let a = next_cal_token();
        let b = next_cal_token();
        assert_ne!(a, b);
        assert_ne!(a, 0);
        assert_ne!(b, 0);
        // 0 is reserved for "no session", so a default phase matches nothing.
        assert_eq!(CalPhase::default().token, 0);
    }

    #[test]
    fn cal_phase_collect_increments_on_ok_and_records_error() {
        let mut p = CalPhase::begin(1);
        p.on_collect(Ok(()));
        p.on_collect(Ok(()));
        assert_eq!(p.collected, 2);
        p.on_collect(Err("nope".into()));
        assert_eq!(p.collected, 2);
        assert_eq!(p.last_error.as_deref(), Some("nope"));
        p.on_collect(Ok(()));
        assert_eq!(p.collected, 3);
        assert!(p.last_error.is_none());
    }

    #[test]
    fn cal_phase_finish_sets_outcome_and_clears_active() {
        let mut p = CalPhase::begin(1);
        p.on_finish(Ok(()));
        assert!(!p.active);
        assert_eq!(p.finished, Some(Ok(())));
    }

    #[test]
    fn tick_collects_a_calibration_point() {
        // 0x406 (CALIBRATE_POINT_ADD2D), not 0x408 — the device acks points
        // sent to 0x408 and discards them, so this responding at all is the
        // whole assertion.
        let mut conn = connected(vec![inbound(TTP_MAGIC_RSP, 5, 0x406, &[])]);
        let state = Mutex::new(DeviceState::default());
        let (tx, rx) = channel::<DeviceCommand>();
        tx.send(DeviceCommand::CalCollect { x: 0.5, y: 0.5 })
            .unwrap();
        device_tick(&mut conn, &state, &rx, None);
        assert_eq!(state.lock().unwrap().calibration.collected, 1);
    }

    /// A retrieve response of plausible size: 2 status bytes plus 8 KB of body.
    /// Also large enough that the frame crosses the transport's chunk boundary
    /// on the way back out, exercising the split send.
    fn big_blob() -> Vec<u8> {
        let mut v = vec![0x00, 0x00];
        v.extend((0..8192u32).map(|i| i as u8));
        v
    }

    /// Was a request frame for `op` ever sent? (op lives at bytes 20..24.)
    fn sent_op(conn: &Connection<MockTransport>, op: u32) -> bool {
        conn.transport()
            .sent
            .iter()
            .any(|f| f.len() >= 24 && f[20..24] == op.to_be_bytes())
    }

    #[test]
    fn cal_begin_echoes_the_token_and_marks_the_session_open() {
        // Post-handshake seqs run 5, 6, 7: retrieve (empty -> no previous
        // blob, so no reapply follows), start, clear. No set_enabled_eye —
        // the original never touches it during a calibration.
        let mut conn = connected(vec![
            inbound(TTP_MAGIC_RSP, 5, 0x44c, &[]),
            inbound(TTP_MAGIC_RSP, 6, 0x3f2, &[]),
            inbound(TTP_MAGIC_RSP, 7, 0x424, &[]),
        ]);
        let state = Mutex::new(DeviceState::default());
        let (tx, rx) = channel::<DeviceCommand>();
        tx.send(DeviceCommand::CalBegin {
            improve: true,
            token: 99,
        })
        .unwrap();
        device_tick(&mut conn, &state, &rx, None);
        let s = state.lock().unwrap();
        assert_eq!(s.calibration.token, 99, "UI's token is echoed back");
        assert!(s.calibration.active);
        assert!(s.cal_session_open);
        assert!(s.calibration.started, "start+clear both acked");
        assert!(
            !sent_op(&conn, 0x456),
            "no blob retrieved -> no CAL_APPLY reseed"
        );
    }

    #[test]
    fn cal_begin_reapplies_the_previously_retrieved_calibration_blob() {
        // A non-empty retrieve response must be re-applied (CAL_APPLY, 0x456)
        // after start+clear, before the session is reported as started — this
        // is what makes "Improve calibration" build on the old calibration
        // instead of behaving like a from-scratch one.
        let mut conn = connected(vec![
            // Two status bytes then a blob big enough to be believable: the
            // apply path now refuses anything too small to be a calibration.
            inbound(TTP_MAGIC_RSP, 5, 0x44c, &big_blob()),
            inbound(TTP_MAGIC_RSP, 6, 0x3f2, &[]),
            inbound(TTP_MAGIC_RSP, 7, 0x424, &[]),
            inbound(TTP_MAGIC_RSP, 8, 0x456, &[]),
        ]);
        let state = Mutex::new(DeviceState::default());
        let (tx, rx) = channel::<DeviceCommand>();
        tx.send(DeviceCommand::CalBegin {
            improve: true,
            token: 1,
        })
        .unwrap();
        device_tick(&mut conn, &state, &rx, None);
        assert!(sent_op(&conn, 0x456), "retrieved blob was re-applied");
        let s = state.lock().unwrap();
        assert!(s.calibration.started, "start+clear+reapply all acked");
        assert!(s.cal_session_open);
    }

    #[test]
    fn an_unusable_stored_calibration_does_not_abort_the_session() {
        // The blob saved before the calibration ops were corrected is a ~1.5KB
        // stub that `apply` now refuses. Seeding from it is an optimisation, so
        // its absence must leave a perfectly good from-scratch session — not
        // fail instantly with "can't detect your eyes", which is what shipping
        // this in the start/clear chain did.
        let mut conn = connected(vec![
            inbound(TTP_MAGIC_RSP, 5, 0x44c, &[0x00, 0x00, 0xDE, 0xAD]),
            inbound(TTP_MAGIC_RSP, 6, 0x3f2, &[]),
            inbound(TTP_MAGIC_RSP, 7, 0x424, &[]),
        ]);
        let state = Mutex::new(DeviceState::default());
        let (tx, rx) = channel::<DeviceCommand>();
        tx.send(DeviceCommand::CalBegin {
            improve: true,
            token: 1,
        })
        .unwrap();
        device_tick(&mut conn, &state, &rx, None);
        let s = state.lock().unwrap();
        assert!(s.calibration.started, "session must open anyway");
        assert!(
            s.calibration.finished.is_none(),
            "must not have failed: {:?}",
            s.calibration.finished
        );
        assert!(
            !sent_op(&conn, 0x456),
            "a stub blob must not even be offered"
        );
    }

    #[test]
    fn cal_begin_marks_session_open_even_when_clear_fails() {
        // start (seq 7) acks, clear (seq 8) gets no response: the realm IS open
        // and a later abort must still stop it, even though `active` is false.
        let mut conn = connected(vec![
            inbound(TTP_MAGIC_RSP, 5, 0x44c, &[]),
            inbound(TTP_MAGIC_RSP, 6, 0x3f2, &[]),
        ]);
        conn.set_request_timeout(Duration::from_millis(10));
        let state = Mutex::new(DeviceState::default());
        let (tx, rx) = channel::<DeviceCommand>();
        tx.send(DeviceCommand::CalBegin {
            improve: true,
            token: 5,
        })
        .unwrap();
        device_tick(&mut conn, &state, &rx, None);
        {
            let s = state.lock().unwrap();
            assert!(!s.calibration.active, "on_finish cleared active");
            assert!(matches!(s.calibration.finished, Some(Err(_))));
            assert!(s.cal_session_open, "realm is still open on the device");
        }
        // ...and the abort therefore actually stops it (the pre-fix `active`
        // guard skipped exactly this case).
        //
        // NOTE: the abort's restore path calls `tobii_config::load_calibration`,
        // which reads the real user config directory — so whether an apply is
        // even attempted here depends on the machine this runs on. The
        // assertions below deliberately do not depend on that, but the coupling
        // is real and this test would be better with a config seam.
        tx.send(DeviceCommand::CalAbort).unwrap();
        device_tick(&mut conn, &state, &rx, None);
        assert!(sent_op(&conn, 0x3fc), "CAL_STOP was sent");
        // The mock never acks that stop, so the realm may well still be open:
        // the flag must STAY set so a later abort retries it. Clearing on a
        // merely-issued stop is how a device gets stranded in calibration mode.
        assert!(
            state.lock().unwrap().cal_session_open,
            "an unacked stop must not be taken as proof the realm closed"
        );
    }

    #[test]
    fn cal_phase_started_is_separate_from_active() {
        // `active` is set the moment CalBegin is dequeued (the idle watchdog
        // depends on that covering the whole start/clear window); `started`
        // only once the session is really open, which is what the UI waits on.
        let mut p = CalPhase::begin(9);
        assert!(p.active, "in flight as soon as the command is dequeued");
        assert!(
            !p.started,
            "but the session is not open until start+clear ack"
        );
        p.on_started();
        assert!(p.started);
    }

    #[test]
    fn cal_abort_skips_the_stop_when_no_session_is_open() {
        let mut conn = connected(vec![]);
        let state = Mutex::new(DeviceState::default());
        let (tx, rx) = channel::<DeviceCommand>();
        // A finished session: already stopped, so a second stop would just burn
        // a request deadline.
        {
            let mut s = state.lock().unwrap();
            s.calibration = CalPhase::begin(3);
            s.cal_session_open = false;
        }
        tx.send(DeviceCommand::CalAbort).unwrap();
        device_tick(&mut conn, &state, &rx, None);
        assert!(!sent_op(&conn, 0x3fc), "no redundant CAL_STOP");
        assert_eq!(state.lock().unwrap().calibration, CalPhase::default());
    }

    #[test]
    fn tick_applies_a_set_display_area_command() {
        let mut conn = connected(vec![inbound(TTP_MAGIC_RSP, 5, 0x5a0, &[])]);
        let state = Mutex::new(DeviceState::default());
        let (tx, rx) = channel::<DeviceCommand>();
        tx.send(DeviceCommand::SetDisplayArea(
            tobii_protocol::DisplayCorners {
                tl: [-1.0, 1.0, 0.0],
                tr: [1.0, 1.0, 0.0],
                bl: [-1.0, -1.0, 0.0],
            },
        ))
        .unwrap();
        device_tick(&mut conn, &state, &rx, None);
        // A SET_DISPLAY_AREA (op 0x5a0) frame was sent (5th send after 4 handshake sends).
        assert_eq!(
            &conn.transport().sent.last().unwrap()[20..24],
            &[0, 0, 0x05, 0xa0]
        );
    }
}
