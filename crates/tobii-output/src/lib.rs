//! Routing ET5 tracking data to whatever is consuming it — games, opentrack,
//! the Wine bridge.
//!
//! The split against the neighbouring crates is deliberate:
//!
//! * `tobii-headpose` owns *a pose*, and wire encodings **of a pose**.
//! * `tobii-output` owns *who gets it, when, and how it was composed*.
//!
//! So [`sinks::opentrack_udp`] calls `tobii_headpose::opentrack`'s encoder
//! rather than reimplementing it, and this crate stays free of any opinion
//! about how a pose is derived.
//!
//! Everything here is `std`-only and free of device access, so the whole
//! routing layer is unit-testable with literal numbers and no hardware.

pub mod frame;
pub mod freetrack;
pub mod fusion;
pub mod games;
pub mod listener;
pub mod pipeline;
pub mod sinks;
pub mod trackir;

pub use frame::TrackingFrame;
pub use fusion::{AxisResponse, Curve, ExtendedView};

use std::time::{Duration, Instant};

use tobii_headpose::HeadPose;

/// Whether the tracker can currently see the user.
///
/// Distinct from "is the pose valid": the geometric pose needs **both** eyes,
/// but one eye is still enough to know somebody is there, which is what a
/// game's presence/auto-pause feature wants to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Presence {
    /// Neither eye is tracked.
    #[default]
    None,
    /// Exactly one eye is tracked — present, but no derivable head pose.
    OneEye,
    /// Both eyes are tracked.
    BothEyes,
}

impl Presence {
    /// Classify from the two per-eye validity values (`0` means tracked).
    pub fn from_validity(left: u32, right: u32) -> Self {
        match (left == VALIDITY_TRACKED, right == VALIDITY_TRACKED) {
            (true, true) => Presence::BothEyes,
            (false, false) => Presence::None,
            _ => Presence::OneEye,
        }
    }

    /// True if the user is there at all.
    pub fn is_present(self) -> bool {
        self != Presence::None
    }
}

/// Validity value meaning "this eye is tracked"; anything else (in practice 4)
/// means the eye's columns are meaningless. Mirrors `tobii-headpose`.
const VALIDITY_TRACKED: u32 = 0;

/// Why a sink could not deliver a frame.
///
/// Sinks are best-effort by contract, so this exists to be *counted and
/// reported*, not to abort a run: a game that is not listening yet must not
/// take the tracker down with it.
#[derive(Debug)]
pub enum SinkError {
    /// The underlying transport failed.
    Io(std::io::Error),
}

impl std::fmt::Display for SinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SinkError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SinkError {}

impl From<std::io::Error> for SinkError {
    fn from(e: std::io::Error) -> Self {
        SinkError::Io(e)
    }
}

/// Somewhere a [`TrackingFrame`] can be delivered.
///
/// Implementations **must not block**. The device thread calls this inline
/// between USB reads, so a sink that waits on a slow peer would stall the gaze
/// pipeline for everything else. Datagram transports satisfy this naturally;
/// anything stream-based needs its own buffering behind the trait.
pub trait Sink {
    /// A short stable name, for status lines and error messages.
    fn name(&self) -> &'static str;

    /// Deliver one frame. Errors are reported to the caller, never fatal.
    fn emit(&mut self, frame: &TrackingFrame) -> Result<(), SinkError>;
}

/// What a [`Router::offer`] call did with the frame it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emitted {
    /// Delivered to every enabled sink.
    Sent,
    /// Held back by the send-rate throttle.
    Throttled,
    /// Withheld because tracking is lost — see [`Router::offer`].
    TrackingLost,
}

/// Fans frames out to sinks, owning the send rate and the tracking-loss policy.
///
/// Feed it **every** device sample. It decides what reaches the wire, so that
/// smoothing upstream sees the full sample rate while consumers see only what
/// they asked for.
pub struct Router {
    sinks: Vec<Box<dyn Sink>>,
    send_interval: Duration,
    last_send: Option<Instant>,
    /// Per-sink count of failed `emit` calls, parallel to `sinks`.
    failures: Vec<u64>,
}

impl Router {
    /// A router that emits at most `rate_hz` frames per second.
    ///
    /// A non-finite or non-positive rate is treated as "no throttle" rather
    /// than panicking or silently emitting nothing — a bad config value should
    /// degrade to more data, not to a pipeline that looks dead.
    pub fn new(rate_hz: f64) -> Self {
        let send_interval = if rate_hz.is_finite() && rate_hz > 0.0 {
            Duration::from_secs_f64(1.0 / rate_hz)
        } else {
            Duration::ZERO
        };
        Self {
            sinks: Vec::new(),
            send_interval,
            last_send: None,
            failures: Vec::new(),
        }
    }

    /// Add a sink. Order is preserved but carries no meaning.
    pub fn add(&mut self, sink: Box<dyn Sink>) {
        self.sinks.push(sink);
        self.failures.push(0);
    }

    /// How many sinks are attached.
    pub fn sink_count(&self) -> usize {
        self.sinks.len()
    }

    /// Names of the attached sinks, in insertion order.
    pub fn sink_names(&self) -> Vec<&'static str> {
        self.sinks.iter().map(|s| s.name()).collect()
    }

    /// Failed `emit` calls per sink, in insertion order.
    pub fn failures(&self) -> &[u64] {
        &self.failures
    }

    /// Offer one frame to the sinks.
    ///
    /// Two policies live here, both lifted from the original `tobii headpose`
    /// loop:
    ///
    /// * **Throttle.** At most one send per `1/rate_hz`, so callers can feed
    ///   every sample in (keeping filters at full rate) without flooding.
    /// * **Tracking loss stops sending entirely.** When the pose is `None` the
    ///   frame is dropped rather than a synthetic zero pose being emitted:
    ///   consumers hold their last value, which is far less jarring in game
    ///   than the view snapping to centre every time the user blinks.
    ///
    /// `now` is passed in rather than read from the clock so the policy is
    /// testable without sleeping.
    pub fn offer(&mut self, frame: &TrackingFrame, now: Instant) -> Emitted {
        if frame.pose.is_none() {
            return Emitted::TrackingLost;
        }
        if let Some(last) = self.last_send {
            if now.duration_since(last) < self.send_interval {
                return Emitted::Throttled;
            }
        }
        self.last_send = Some(now);
        // Every sink is offered the frame even if an earlier one failed: one
        // dead consumer must not silently cut off the others.
        for (i, sink) in self.sinks.iter_mut().enumerate() {
            if sink.emit(frame).is_err() {
                self.failures[i] = self.failures[i].saturating_add(1);
            }
        }
        Emitted::Sent
    }
}

/// Convenience: a frame carrying a pose and nothing else.
impl TrackingFrame {
    /// A frame for a tracked head with no gaze information.
    pub fn from_pose(timestamp_us: i64, pose: HeadPose) -> Self {
        TrackingFrame {
            timestamp_us,
            pose: Some(pose),
            gaze: None,
            presence: Presence::BothEyes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A sink that records what it was given, and can be told to fail.
    struct Spy {
        name: &'static str,
        seen: Rc<RefCell<Vec<i64>>>,
        fail: bool,
    }

    impl Sink for Spy {
        fn name(&self) -> &'static str {
            self.name
        }
        fn emit(&mut self, frame: &TrackingFrame) -> Result<(), SinkError> {
            self.seen.borrow_mut().push(frame.timestamp_us);
            if self.fail {
                return Err(SinkError::Io(std::io::Error::other("spy refuses")));
            }
            Ok(())
        }
    }

    fn spy(name: &'static str, fail: bool) -> (Box<dyn Sink>, Rc<RefCell<Vec<i64>>>) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        (
            Box::new(Spy {
                name,
                seen: Rc::clone(&seen),
                fail,
            }),
            seen,
        )
    }

    fn tracked(timestamp_us: i64) -> TrackingFrame {
        TrackingFrame::from_pose(timestamp_us, HeadPose::default())
    }

    fn lost(timestamp_us: i64) -> TrackingFrame {
        TrackingFrame {
            timestamp_us,
            pose: None,
            gaze: None,
            presence: Presence::None,
        }
    }

    #[test]
    fn presence_needs_both_eyes_to_read_as_both() {
        assert_eq!(Presence::from_validity(0, 0), Presence::BothEyes);
        assert_eq!(Presence::from_validity(0, 4), Presence::OneEye);
        assert_eq!(Presence::from_validity(4, 0), Presence::OneEye);
        assert_eq!(Presence::from_validity(4, 4), Presence::None);
    }

    /// One eye is enough to know somebody is there, even though it is not
    /// enough to derive a pose. Conflating the two would tell a game the user
    /// walked away every time they squint.
    #[test]
    fn one_eye_still_counts_as_present() {
        assert!(Presence::BothEyes.is_present());
        assert!(Presence::OneEye.is_present());
        assert!(!Presence::None.is_present());
    }

    #[test]
    fn a_frame_reaches_every_sink() {
        let (a, seen_a) = spy("a", false);
        let (b, seen_b) = spy("b", false);
        let mut r = Router::new(60.0);
        r.add(a);
        r.add(b);

        assert_eq!(r.offer(&tracked(7), Instant::now()), Emitted::Sent);
        assert_eq!(*seen_a.borrow(), vec![7]);
        assert_eq!(*seen_b.borrow(), vec![7]);
        assert_eq!(r.sink_names(), vec!["a", "b"]);
    }

    /// A game that is not listening yet must not cut off opentrack, or the user
    /// loses working output to a consumer they were not even using.
    #[test]
    fn a_failing_sink_does_not_stop_the_others() {
        let (bad, seen_bad) = spy("bad", true);
        let (good, seen_good) = spy("good", false);
        let mut r = Router::new(60.0);
        r.add(bad);
        r.add(good);

        assert_eq!(r.offer(&tracked(1), Instant::now()), Emitted::Sent);
        assert_eq!(*seen_bad.borrow(), vec![1], "the failing sink was tried");
        assert_eq!(*seen_good.borrow(), vec![1], "and the next one still ran");
        assert_eq!(r.failures(), &[1, 0], "only the failure is counted");
    }

    #[test]
    fn the_throttle_holds_frames_back_until_the_interval_elapses() {
        let (s, seen) = spy("s", false);
        let mut r = Router::new(100.0); // 10ms
        r.add(s);

        let t0 = Instant::now();
        assert_eq!(r.offer(&tracked(1), t0), Emitted::Sent);
        assert_eq!(
            r.offer(&tracked(2), t0 + Duration::from_millis(5)),
            Emitted::Throttled
        );
        assert_eq!(
            r.offer(&tracked(3), t0 + Duration::from_millis(10)),
            Emitted::Sent
        );
        assert_eq!(*seen.borrow(), vec![1, 3], "only unthrottled frames go out");
    }

    /// Withholding beats faking. A synthetic zero pose would snap the in-game
    /// view to centre on every blink; sending nothing leaves the consumer
    /// holding its last value, which the user reads as "steady".
    #[test]
    fn tracking_loss_sends_nothing_rather_than_a_zero_pose() {
        let (s, seen) = spy("s", false);
        let mut r = Router::new(1000.0);
        r.add(s);

        let t0 = Instant::now();
        assert_eq!(r.offer(&lost(1), t0), Emitted::TrackingLost);
        assert!(seen.borrow().is_empty(), "nothing may be sent");
        assert_eq!(
            r.offer(&tracked(2), t0 + Duration::from_millis(5)),
            Emitted::Sent,
            "a lost frame must not have consumed the throttle slot"
        );
    }

    /// A bad config value should mean more data, not a pipeline that looks
    /// dead — a silently-never-sending router is indistinguishable from a
    /// broken tracker.
    #[test]
    fn a_nonsense_rate_degrades_to_no_throttle() {
        for bad in [0.0, -30.0, f64::NAN, f64::INFINITY] {
            let (s, seen) = spy("s", false);
            let mut r = Router::new(bad);
            r.add(s);
            let t0 = Instant::now();
            r.offer(&tracked(1), t0);
            r.offer(&tracked(2), t0);
            assert_eq!(*seen.borrow(), vec![1, 2], "rate {bad} must not gag output");
        }
    }

    #[test]
    fn a_router_with_no_sinks_still_reports_what_it_would_have_done() {
        let mut r = Router::new(60.0);
        assert_eq!(r.sink_count(), 0);
        assert_eq!(r.offer(&tracked(1), Instant::now()), Emitted::Sent);
        assert_eq!(r.offer(&lost(2), Instant::now()), Emitted::TrackingLost);
    }
}
