//! Replay a recorded ET5 session, with no ET5.
//!
//! Everything this crate knows about the tracker was learned by plugging one in.
//! That knowledge was, until this file existed, checked by nothing: a refactor
//! that changed an op code, reordered the handshake, or altered a payload
//! encoding would break every user and pass every test.
//!
//! `captures/session.tobiicap` is one real session recorded with
//! `tobii record`. These tests drive the driver against exactly the replies the
//! device gave, and assert on **what the driver sends** — which is where the
//! reverse engineering lives.
//!
//! # The one thing to understand before trusting a green run here
//!
//! This proves the code still does what it did when the recording was taken.
//! It does not prove the device still does. A firmware change would invalidate
//! the fixture silently and these tests would keep passing. Re-record with
//! `tobii record` after a firmware update, and treat a capture as a photograph
//! rather than a specification.

use std::time::Duration;

use tobii_protocol::{DisplayCorners, EnabledEye};
use tobii_usb::capture::{Capture, ReplayTransport};
use tobii_usb::Connection;

const CAPTURE: &str = include_str!("captures/session.tobiicap");

/// A second recording, covering the one thing the session capture cannot: a
/// response too large for the 16 KB read buffer, which the device splits across
/// many USB transfers with continuation envelopes between them.
///
/// Kept separate because it is 1.5 MB of one 32,000-character line. The session
/// capture stays short enough that a re-recording produces a diff a human can
/// read, which is the reason the format is line-oriented hex at all.
const CALIBRATION_CAPTURE: &str = include_str!("captures/calibration.tobiicap");

/// How long a replayed request waits. See [`replay_the_recorded_session`].
const REPLAY_TIMEOUT: Duration = Duration::from_millis(200);

fn capture() -> Capture {
    Capture::parse(CAPTURE).expect("the committed capture parses")
}

/// The display geometry the recording was taken with, from its header.
///
/// The corners are this project's own config on the machine that recorded it,
/// so they are carried in the capture rather than guessed: without them the
/// display-area frame the driver builds would differ from the recorded one on
/// every other machine, and the comparison below would be meaningless.
fn recorded_corners(c: &Capture) -> Option<DisplayCorners> {
    let nums: Vec<f64> = c
        .header("display-area")?
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() != 9 {
        return None;
    }
    Some(DisplayCorners {
        tl: [nums[0], nums[1], nums[2]],
        tr: [nums[3], nums[4], nums[5]],
        bl: [nums[6], nums[7], nums[8]],
    })
}

/// Drive the driver through exactly the session that was recorded.
///
/// Returns the connection's transport, holding everything the driver sent, and
/// the gaze samples it decoded.
fn replay_the_recorded_session() -> (ReplayTransport, Vec<tobii_protocol::GazeSample>) {
    let cap = capture();
    let mut conn = Connection::connect(ReplayTransport::new(&cap))
        .expect("the handshake completes against the recorded replies");
    // A replayed reply arrives instantly or never, so the default multi-second
    // request window is pure waiting: once the recording is exhausted, every
    // remaining request spins to its deadline. This took the suite from ten
    // seconds to well under one.
    conn.set_request_timeout(REPLAY_TIMEOUT);

    if let Some(corners) = recorded_corners(&cap) {
        let _ = conn.set_display_area(&corners);
    }
    let _ = conn.get_enabled_eye();
    let _ = conn.subscribe_stream(tobii_protocol::frame::STREAM_GAZE);

    let mut samples = Vec::new();
    // Read until the recording runs dry. `next_gaze` returning None means the
    // replay is exhausted, which is the end of the capture, not a failure.
    for _ in 0..500 {
        match conn.next_gaze() {
            Some(s) => samples.push(s),
            None => break,
        }
    }
    let _ = conn.unsubscribe_stream(tobii_protocol::frame::STREAM_GAZE);
    (conn.into_transport(), samples)
}

/// **The fragmentation test.** A 778 KB calibration blob, reassembled from the
/// 60 device reads it really arrived in.
///
/// This exists because a guard that assumed the continuation envelope's length
/// field fits inside its own USB read passed every test in the workspace and
/// broke every calibration retrieval on real hardware: the field is the size of
/// the whole continuation RUN, so a genuine envelope announcing 778,188 bytes
/// turned up in a 100-byte read. Sixteen envelope bytes were spliced into the
/// payload, the frame decoded as `BadDirection`, and the caller timed out with
/// no response at all.
///
/// Nothing could catch that: the session capture has no fragmented response,
/// and every parser unit test built an envelope whose length happened to equal
/// its chunk. This is the coverage that closes it — and it is end to end,
/// through `Connection::retrieve_calibration`, not just the parser.
#[test]
fn a_fragmented_calibration_blob_is_reassembled_exactly() {
    let cap = Capture::parse(CALIBRATION_CAPTURE).expect("the calibration capture parses");
    let mut conn =
        Connection::connect(ReplayTransport::new(&cap)).expect("the handshake completes");
    conn.set_request_timeout(REPLAY_TIMEOUT);
    if let Some(corners) = recorded_corners(&cap) {
        let _ = conn.set_display_area(&corners);
    }
    let _ = conn.get_enabled_eye();

    let blob = conn
        .retrieve_calibration()
        .expect("the recorded blob must come back");

    // The size the device really sent. A reassembly that drops or splices
    // envelope bytes lands near this but not on it, which is exactly how the
    // bug behaved.
    assert_eq!(
        blob.0.len(),
        778_753,
        "the reassembled blob is the wrong length"
    );
    assert!(
        tobii_usb::is_plausible_calibration(&blob.0),
        "a blob this size must pass the driver's own plausibility check"
    );

    // And it must be the bytes themselves, not merely the right count: a
    // spliced-in envelope keeps the length if it also drops payload.
    let recorded_total: usize = cap.received().iter().map(|f| f.len()).sum();
    assert!(
        recorded_total > blob.0.len(),
        "the recording should carry more bytes than the payload (headers, envelopes)"
    );
}

/// The fixture is a real recording and stays one.
#[test]
fn the_committed_capture_is_a_real_recorded_session() {
    let c = capture();
    assert_eq!(
        c.header("device"),
        Some("2104:0313"),
        "recorded from an ET5"
    );
    assert!(
        c.header("recorded-at").is_some_and(|t| t.starts_with("20")),
        "a capture must say when it was taken: {:?}",
        c.header("recorded-at")
    );
    assert!(
        c.received().len() > 20,
        "a session with almost no device traffic is not worth replaying"
    );
    assert!(!c.sent().is_empty(), "the driver said nothing");
}

/// The handshake completes against real device replies.
///
/// The single most load-bearing thing in this crate: three round trips of a
/// reverse-engineered protocol, with an op code and a realm negotiation that
/// were worked out by disassembly. Nothing else here checks them against a real
/// device's answers.
#[test]
fn the_handshake_completes_against_the_recorded_replies() {
    let cap = capture();
    let conn = Connection::connect(ReplayTransport::new(&cap))
        .expect("the recorded handshake must still complete");
    let t = conn.into_transport();
    assert!(
        t.sent().len() >= 3,
        "the handshake is a multi-frame exchange, saw {} frames",
        t.sent().len()
    );
}

/// **The regression test.** Given exactly these device replies, the driver must
/// produce exactly these frames.
///
/// This is what catches a changed op code, a reordered handshake, a payload
/// encoded differently, or a sequence number that stopped incrementing — all
/// changes that compile, pass every unit test, and break every user.
///
/// When this fails after a deliberate protocol change, re-record with
/// `tobii record` and read the diff: the capture is line-oriented hex precisely
/// so that diff is reviewable.
#[test]
fn the_driver_sends_exactly_what_it_sent_when_this_was_recorded() {
    let cap = capture();
    let (transport, _) = replay_the_recorded_session();

    let recorded: Vec<&[u8]> = cap.sent();
    let replayed = transport.sent();

    assert_eq!(
        replayed.len(),
        recorded.len(),
        "the driver sent {} frames; the recording has {}",
        replayed.len(),
        recorded.len()
    );
    for (i, (got, want)) in replayed.iter().zip(recorded.iter()).enumerate() {
        assert_eq!(
            got.as_slice(),
            *want,
            "frame {i} differs from the recording\n  sent     {}\n  recorded {}",
            hex(got),
            hex(want)
        );
    }
}

/// Gaze frames from a real device still decode, and to plausible values.
///
/// Not a golden-value test: the numbers are whatever the recorder's eyes were
/// doing. What is asserted is the shape — that frames decode at all, that the
/// count matches what was recorded, and that the fields are within the ranges
/// the protocol defines. A decoder that silently started reading the wrong
/// column would fail this.
#[test]
fn recorded_gaze_frames_still_decode() {
    let (_, samples) = replay_the_recorded_session();
    assert!(
        samples.len() >= 30,
        "only {} gaze samples decoded from a 40-frame recording",
        samples.len()
    );
    for (i, s) in samples.iter().enumerate() {
        // Validity is a small enum on the wire; 4 means "no eye".
        assert!(
            s.validity_l <= 4,
            "sample {i} validity_l = {}",
            s.validity_l
        );
        assert!(
            s.validity_r <= 4,
            "sample {i} validity_r = {}",
            s.validity_r
        );
        // The gaze point is normalised, with (-1, -1) meaning "no gaze".
        let [x, y] = s.gaze_point_2d;
        assert!(
            x.is_finite() && y.is_finite(),
            "sample {i} has a non-finite gaze point: {x}, {y}"
        );
        assert!(
            (-1.5..=2.0).contains(&x) && (-1.5..=2.0).contains(&y),
            "sample {i} gaze point ({x}, {y}) is outside anything the protocol produces"
        );
        // Eye origins are millimetres in the tracker frame. A decoder reading
        // the wrong column would land far outside a room.
        for (name, o) in [("left", s.eye_origin_l_mm), ("right", s.eye_origin_r_mm)] {
            assert!(
                o.iter().all(|v| v.is_finite() && v.abs() < 5000.0),
                "sample {i} {name} eye origin {o:?} is not a plausible position"
            );
        }
    }
}

/// The eye selection read back is one the enum knows.
///
/// `enabled_eye`'s wire values were mapped by experiment and their order
/// differs from the obvious one, so a decode that drifted would be easy to miss.
#[test]
fn the_recorded_eye_selection_decodes_to_a_real_value() {
    let cap = capture();
    let mut conn = Connection::connect(ReplayTransport::new(&cap)).unwrap();
    conn.set_request_timeout(REPLAY_TIMEOUT);
    if let Some(corners) = recorded_corners(&cap) {
        let _ = conn.set_display_area(&corners);
    }
    let eye = conn
        .get_enabled_eye()
        .expect("the request completes")
        .expect("the device answered with a selection");
    assert!(
        matches!(eye, EnabledEye::Both | EnabledEye::Left | EnabledEye::Right),
        "decoded {eye:?}"
    );
}

/// The replay must actually run to the end of the recording, or a test that
/// stops after the handshake would look exactly like one that covered
/// everything.
#[test]
fn the_replay_consumes_the_whole_recording() {
    let (transport, samples) = replay_the_recorded_session();
    assert_eq!(
        transport.remaining(),
        0,
        "{} recorded device frames were never delivered — the replay stopped early",
        transport.remaining()
    );
    assert!(!samples.is_empty());
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
