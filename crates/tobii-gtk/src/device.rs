//! The device thread: owns the blocking `Connection`, publishes a `DeviceState`
//! snapshot for the UI, and applies `DeviceCommand`s. `device_tick` (one
//! iteration) is generic over `Transport` so it is unit-tested without hardware.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tobii_protocol::{DisplayCorners, EnabledEye, GazeSample};
use tobii_usb::{Connection, Transport, UsbError, UsbTransport};

#[derive(Debug, Clone, PartialEq, Default)]
pub enum ConnStatus {
    #[default]
    Connecting,
    Connected,
    Error(String),
}

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
    /// True between `CalBegin` and `CalFinish`/`CalAbort`.
    pub active: bool,
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
            collected: 0,
            last_error: None,
            finished: None,
        }
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
    pub enabled_eye: Option<EnabledEye>,
    pub calibration: CalPhase,
    /// Whether the *device* is believed to be inside an open calibration realm:
    /// set once `start_calibration` returns `Ok`, cleared only once a stop has
    /// actually been issued. Deliberately separate from `calibration.active`,
    /// which `on_finish` also clears on the start/clear failure path — where the
    /// realm may well still be open and still needs an explicit stop.
    pub cal_session_open: bool,
}

pub enum DeviceCommand {
    SetDisplayArea(DisplayCorners),
    SetEnabledEye(EnabledEye),
    /// Begin calibration: set the eye (experiment), then start + clear.
    /// `token` identifies this session; it is echoed into `CalPhase::token` so
    /// the UI can tell a fresh phase from the previous session's leftovers.
    CalBegin {
        eye: EnabledEye,
        token: u64,
    },
    /// Sample one stimulus point (both eyes).
    CalCollect {
        x: f64,
        y: f64,
    },
    /// Compute + apply + stop + retrieve + persist.
    CalFinish,
    /// Abort: stop (best-effort) and reset.
    CalAbort,
}

/// One iteration: apply any queued commands, then poll one gaze sample.
/// Returns `true` if a gaze sample was received this tick — the thread loop
/// uses sustained `false` to detect a stalled/unplugged device (a healthy
/// device streams gaze continuously).
pub fn device_tick<T: Transport>(
    conn: &mut Connection<T>,
    state: &Mutex<DeviceState>,
    cmd_rx: &Receiver<DeviceCommand>,
) -> bool {
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            DeviceCommand::SetDisplayArea(c) => {
                let _ = conn.set_display_area(&c);
            }
            DeviceCommand::SetEnabledEye(e) => {
                let _ = conn.set_enabled_eye(e);
                let _ = tobii_config::save_enabled_eye(e);
                state.lock().unwrap().enabled_eye = Some(e);
            }
            DeviceCommand::CalBegin { eye, token } => {
                state.lock().unwrap().calibration = CalPhase::begin(token);
                let _ = conn.set_enabled_eye(eye); // best-effort select-eyes experiment

                // The realm is open the moment `start` acks, even if `clear`
                // then fails — record that before clearing so the abort path
                // below still knows it has to stop the session.
                let r = match conn.start_calibration() {
                    Ok(()) => {
                        state.lock().unwrap().cal_session_open = true;
                        conn.clear_calibration().map_err(|e| e.to_string())
                    }
                    Err(e) => Err(e.to_string()),
                };
                if let Err(e) = r {
                    state.lock().unwrap().calibration.on_finish(Err(e));
                }
            }
            DeviceCommand::CalCollect { x, y } => {
                let r = conn
                    .add_calibration_point(x, y, 0)
                    .map_err(|e| e.to_string());
                state.lock().unwrap().calibration.on_collect(r);
            }
            DeviceCommand::CalFinish => {
                let r = finish_calibration(conn);
                // `finish_calibration` always issues a stop, success or not.
                let mut s = state.lock().unwrap();
                s.cal_session_open = false;
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
                    let _ = conn.stop_calibration();
                    state.lock().unwrap().cal_session_open = false;
                }
                state.lock().unwrap().calibration = CalPhase::default();
            }
        }
    }
    if let Some(g) = conn.next_gaze() {
        let mut s = state.lock().unwrap();
        s.latest_gaze = Some(g);
        s.status = ConnStatus::Connected;
        true
    } else {
        false
    }
}

/// Spawn the device thread. It handshakes, then loops `device_tick`; on any
/// connection failure it records the error and retries after a short delay.
pub fn spawn() -> (Arc<Mutex<DeviceState>>, Sender<DeviceCommand>) {
    let state = Arc::new(Mutex::new(DeviceState::default()));
    let (tx, rx) = channel::<DeviceCommand>();
    let thread_state = Arc::clone(&state);
    std::thread::spawn(move || loop {
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
                if let Ok(Some(blob)) = tobii_config::load_calibration() {
                    let _ = conn.apply_calibration(&blob);
                }
                let cur_eye = conn.get_enabled_eye().ok().flatten();
                {
                    let mut s = thread_state.lock().unwrap();
                    s.enabled_eye = cur_eye;
                    s.status = ConnStatus::Connected;
                    // A brand-new connection is never inside a calibration
                    // realm, whatever the previous one was doing.
                    s.cal_session_open = false;
                }
                let mut idle_ticks = 0u32;
                loop {
                    let got = device_tick(&mut conn, &thread_state, &rx);
                    let calibrating = thread_state.lock().unwrap().calibration.active;
                    if got || calibrating {
                        idle_ticks = 0;
                    } else {
                        idle_ticks += 1;
                        std::thread::sleep(Duration::from_millis(100));
                        if idle_ticks >= 20 {
                            break; // ~2s without gaze -> assume disconnect; outer loop reconnects
                        }
                    }
                }
            }
            Err(e) => {
                set_error(&thread_state, &e);
                std::thread::sleep(Duration::from_millis(750));
            }
        }
    });
    (state, tx)
}

fn set_error(state: &Mutex<DeviceState>, e: &UsbError) {
    state.lock().unwrap().status = ConnStatus::Error(e.to_string());
}

/// Compute + stop + retrieve + persist. Always attempts `stop` so the device is
/// not left in calibration mode even when compute fails.
fn finish_calibration<T: Transport>(conn: &mut Connection<T>) -> Result<(), String> {
    let compute = conn
        .compute_and_apply_calibration()
        .map_err(|e| e.to_string());
    let _ = conn.stop_calibration();
    compute?;
    let blob = conn.retrieve_calibration().map_err(|e| e.to_string())?;
    if blob.0.is_empty() {
        // Persisting an empty blob would re-apply nothing on every connect and
        // silently mask the fact that the calibration was never stored.
        return Err("device returned an empty calibration".into());
    }
    tobii_config::save_calibration(&blob.0).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
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
        assert!(device_tick(&mut conn, &state, &rx));
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
        assert!(!device_tick(&mut conn, &state, &rx));
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
        let mut conn = connected(vec![inbound(TTP_MAGIC_RSP, 5, 0x408, &[])]);
        let state = Mutex::new(DeviceState::default());
        let (tx, rx) = channel::<DeviceCommand>();
        tx.send(DeviceCommand::CalCollect { x: 0.5, y: 0.5 })
            .unwrap();
        device_tick(&mut conn, &state, &rx);
        assert_eq!(state.lock().unwrap().calibration.collected, 1);
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
        // Post-handshake seqs run 5, 6, 7: set_enabled_eye, start, clear.
        let mut conn = connected(vec![
            inbound(TTP_MAGIC_RSP, 5, 0xc58, &[]),
            inbound(TTP_MAGIC_RSP, 6, 0x3f2, &[]),
            inbound(TTP_MAGIC_RSP, 7, 0x424, &[]),
        ]);
        let state = Mutex::new(DeviceState::default());
        let (tx, rx) = channel::<DeviceCommand>();
        tx.send(DeviceCommand::CalBegin {
            eye: EnabledEye::Both,
            token: 99,
        })
        .unwrap();
        device_tick(&mut conn, &state, &rx);
        let s = state.lock().unwrap();
        assert_eq!(s.calibration.token, 99, "UI's token is echoed back");
        assert!(s.calibration.active);
        assert!(s.cal_session_open);
    }

    #[test]
    fn cal_begin_marks_session_open_even_when_clear_fails() {
        // start (seq 6) acks, clear (seq 7) gets no response: the realm IS open
        // and a later abort must still stop it, even though `active` is false.
        let mut conn = connected(vec![
            inbound(TTP_MAGIC_RSP, 5, 0xc58, &[]),
            inbound(TTP_MAGIC_RSP, 6, 0x3f2, &[]),
        ]);
        conn.set_request_timeout(Duration::from_millis(10));
        let state = Mutex::new(DeviceState::default());
        let (tx, rx) = channel::<DeviceCommand>();
        tx.send(DeviceCommand::CalBegin {
            eye: EnabledEye::Both,
            token: 5,
        })
        .unwrap();
        device_tick(&mut conn, &state, &rx);
        {
            let s = state.lock().unwrap();
            assert!(!s.calibration.active, "on_finish cleared active");
            assert!(matches!(s.calibration.finished, Some(Err(_))));
            assert!(s.cal_session_open, "realm is still open on the device");
        }
        // ...and the abort therefore actually stops it (the pre-fix `active`
        // guard skipped exactly this case).
        tx.send(DeviceCommand::CalAbort).unwrap();
        device_tick(&mut conn, &state, &rx);
        assert!(sent_op(&conn, 0x3fc), "CAL_STOP was sent");
        assert!(!state.lock().unwrap().cal_session_open);
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
        device_tick(&mut conn, &state, &rx);
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
        device_tick(&mut conn, &state, &rx);
        // A SET_DISPLAY_AREA (op 0x5a0) frame was sent (5th send after 4 handshake sends).
        assert_eq!(
            &conn.transport().sent.last().unwrap()[20..24],
            &[0, 0, 0x05, 0xa0]
        );
    }
}
