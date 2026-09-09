//! Recording a real session, and replaying it without the device.
//!
//! # Why
//!
//! Everything this crate does was verified by plugging in an ET5 and watching
//! it work. That is the only way to learn a reverse-engineered protocol, and it
//! is a bad way to keep one working: the knowledge lives in whoever ran the
//! experiment, CI cannot run it, and a refactor that changes an op code or
//! reorders the handshake breaks silently for everyone without a tracker.
//!
//! So: record one real session to a file, commit the file, and replay it in
//! tests. What that turns into a regression test is the *driver's half* of the
//! conversation — which frames it sends, in which order, with which payloads,
//! given exactly the replies the device gave. That is where the reverse
//! engineering is encoded, and it is what a refactor breaks.
//!
//! # What this does and does not prove
//!
//! It proves the code still does what it did when the recording was taken. It
//! does **not** prove the device still does: a firmware change would invalidate
//! the fixture silently, and the replay would keep passing. So a capture is
//! stamped with when and against what it was taken, and re-recording is one
//! command. Treat a fixture like a photograph, not like a specification.
//!
//! # Format
//!
//! Line-oriented text, so a diff of a re-recorded capture is readable and a
//! reviewer can see what changed:
//!
//! ```text
//! # tobii-capture 1
//! # recorded-at 2026-09-09T10:22:31Z
//! # note connect + display area + gaze
//! > 5454500000000001...      (host -> device)
//! < 5454501000000001...      (device -> host)
//! ```
//!
//! Hex rather than base64 or binary: TTP frames are read by eye during protocol
//! work, and the whole point of this file is to be inspectable.

use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use crate::transport::{Transport, UsbError};

/// The format version written into every capture.
pub const FORMAT_VERSION: u32 = 1;

/// One direction of one transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Host to device.
    Sent(Vec<u8>),
    /// Device to host.
    Received(Vec<u8>),
}

/// A recorded session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capture {
    /// Free text describing what was recorded and when.
    pub headers: Vec<(String, String)>,
    pub events: Vec<Event>,
}

impl Capture {
    /// Everything the host sent, in order.
    pub fn sent(&self) -> Vec<&[u8]> {
        self.events
            .iter()
            .filter_map(|e| match e {
                Event::Sent(b) => Some(b.as_slice()),
                _ => None,
            })
            .collect()
    }

    /// Everything the device sent, in order.
    pub fn received(&self) -> Vec<&[u8]> {
        self.events
            .iter()
            .filter_map(|e| match e {
                Event::Received(b) => Some(b.as_slice()),
                _ => None,
            })
            .collect()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Render in the format above.
    pub fn to_text(&self) -> String {
        let mut s = format!("# tobii-capture {FORMAT_VERSION}\n");
        for (k, v) in &self.headers {
            s.push_str(&format!("# {k} {v}\n"));
        }
        for e in &self.events {
            let (marker, bytes) = match e {
                Event::Sent(b) => ('>', b),
                Event::Received(b) => ('<', b),
            };
            s.push(marker);
            s.push(' ');
            s.push_str(&to_hex(bytes));
            s.push('\n');
        }
        s
    }

    /// Parse the format above.
    ///
    /// Unknown `#` lines are kept as headers rather than rejected, so a future
    /// version can add metadata without old code refusing to read the file.
    pub fn parse(text: &str) -> Result<Capture, CaptureError> {
        let mut out = Capture::default();
        // The magic is checked before anything else is parsed. Otherwise a file
        // that is not a capture at all is reported by whichever line happens to
        // fail first — "line 1 is not hex" for a text file, which sends the
        // reader looking for a corrupt frame rather than a wrong file.
        let first = text.lines().map(str::trim).find(|l| !l.is_empty());
        if !first.is_some_and(|l| l.starts_with("# tobii-capture")) {
            return Err(CaptureError::NotACapture);
        }
        let mut saw_magic = false;
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix('#') {
                let rest = rest.trim();
                let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
                if key == "tobii-capture" {
                    saw_magic = true;
                    let v: u32 = value.trim().parse().unwrap_or(0);
                    if v != FORMAT_VERSION {
                        return Err(CaptureError::Version(v));
                    }
                    continue;
                }
                out.headers
                    .push((key.to_string(), value.trim().to_string()));
                continue;
            }
            let (marker, hex) = line.split_at(1);
            let bytes = from_hex(hex.trim()).ok_or(CaptureError::BadHex { line: i + 1 })?;
            match marker {
                ">" => out.events.push(Event::Sent(bytes)),
                "<" => out.events.push(Event::Received(bytes)),
                _ => return Err(CaptureError::BadLine { line: i + 1 }),
            }
        }
        if !saw_magic {
            return Err(CaptureError::NotACapture);
        }
        Ok(out)
    }

    pub fn read_file(path: &Path) -> Result<Capture, CaptureError> {
        let text = std::fs::read_to_string(path).map_err(CaptureError::Io)?;
        Capture::parse(&text)
    }

    pub fn write_file(&self, path: &Path) -> Result<(), CaptureError> {
        std::fs::write(path, self.to_text()).map_err(CaptureError::Io)
    }
}

#[derive(Debug)]
pub enum CaptureError {
    NotACapture,
    Version(u32),
    BadHex { line: usize },
    BadLine { line: usize },
    Io(std::io::Error),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::NotACapture => {
                write!(f, "not a tobii capture (no `# tobii-capture` line)")
            }
            CaptureError::Version(v) => {
                write!(
                    f,
                    "capture format version {v}, this build reads {FORMAT_VERSION}"
                )
            }
            CaptureError::BadHex { line } => write!(f, "line {line} is not hex"),
            CaptureError::BadLine { line } => write!(f, "line {line} starts with neither > nor <"),
            CaptureError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CaptureError {}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in b.as_chunks::<2>().0 {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

/// Wraps a real transport and writes everything through it to a [`Capture`].
///
/// Recording at the [`Transport`] boundary rather than with `usbmon` on purpose:
/// usbmon needs root and produces URBs that then have to be reassembled, while
/// this sees exactly the byte stream the driver sees. What it therefore cannot
/// see is anything below the transport — the USB control transfers in
/// `UsbTransport::open`, and chunking — which is the right trade, since those
/// are libusb's business rather than this protocol's.
pub struct RecordTransport<T: Transport> {
    inner: T,
    capture: Capture,
}

impl<T: Transport> RecordTransport<T> {
    pub fn new(inner: T) -> Self {
        RecordTransport {
            inner,
            capture: Capture::default(),
        }
    }

    pub fn with_note(mut self, note: &str) -> Self {
        self.capture
            .headers
            .push(("note".to_string(), note.to_string()));
        self
    }

    pub fn header(mut self, key: &str, value: &str) -> Self {
        self.capture
            .headers
            .push((key.to_string(), value.to_string()));
        self
    }

    pub fn capture(&self) -> &Capture {
        &self.capture
    }

    pub fn into_capture(self) -> Capture {
        self.capture
    }
}

impl<T: Transport> Transport for RecordTransport<T> {
    fn send(&mut self, data: &[u8]) -> Result<(), UsbError> {
        // Recorded before the call, so a frame that fails to transmit is still
        // in the capture — a failure is exactly the thing worth having a record
        // of.
        self.capture.events.push(Event::Sent(data.to_vec()));
        self.inner.send(data)
    }

    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> Option<usize> {
        let n = self.inner.recv(buf, timeout)?;
        self.capture.events.push(Event::Received(buf[..n].to_vec()));
        Some(n)
    }
}

/// Replays a [`Capture`] in place of the device.
///
/// The two directions are deliberately **decoupled**: `recv` hands back the
/// recorded device replies in order, and `send` records what the driver wrote
/// without checking it against the capture as it goes.
///
/// That is not laziness. Coupling them would make the replay assert that the
/// driver reads and writes in exactly the interleaving that happened to occur
/// on the day — which depends on USB timing, on how many frames arrived in one
/// transfer, and on how long a `recv` blocked. Those are not properties of the
/// protocol, and a test that enforces them fails for reasons nobody can act on.
///
/// What the protocol *does* determine is the sequence of frames the driver
/// sends given those replies. So the test asserts on [`ReplayTransport::sent`]
/// afterwards, which is a stable, meaningful comparison.
pub struct ReplayTransport {
    to_deliver: VecDeque<Vec<u8>>,
    sent: Vec<Vec<u8>>,
    /// Reads attempted after the recording ran out.
    exhausted_reads: usize,
}

impl ReplayTransport {
    pub fn new(capture: &Capture) -> Self {
        ReplayTransport {
            to_deliver: capture.received().into_iter().map(|b| b.to_vec()).collect(),
            sent: Vec::new(),
            exhausted_reads: 0,
        }
    }

    /// Everything the driver sent during the replay.
    pub fn sent(&self) -> &[Vec<u8>] {
        &self.sent
    }

    /// Recorded replies not yet delivered.
    pub fn remaining(&self) -> usize {
        self.to_deliver.len()
    }

    /// Reads made after the recording was exhausted.
    ///
    /// Not an error: a driver polling for gaze frames will ask for more than
    /// the recording holds, and that is what the end of a capture looks like.
    /// Worth reporting so a test can tell "ran to the end" from "gave up early".
    pub fn exhausted_reads(&self) -> usize {
        self.exhausted_reads
    }
}

impl Transport for ReplayTransport {
    fn send(&mut self, data: &[u8]) -> Result<(), UsbError> {
        self.sent.push(data.to_vec());
        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8], _timeout: Duration) -> Option<usize> {
        let Some(next) = self.to_deliver.pop_front() else {
            self.exhausted_reads += 1;
            // The device having nothing to say is a timeout, not an error.
            return None;
        };
        // A recorded frame longer than the caller's buffer would be silently
        // truncated, which would look like a decode bug rather than a replay
        // bug. The recording was taken through the same code paths, so this
        // means the buffer shrank since.
        let n = next.len().min(buf.len());
        buf[..n].copy_from_slice(&next[..n]);
        Some(n)
    }
}

/// Write `capture` to `path`, reporting what was written.
pub fn save(capture: &Capture, path: &Path) -> Result<(), CaptureError> {
    capture.write_file(path)?;
    let mut out = std::io::stdout();
    let _ = writeln!(
        out,
        "wrote {} ({} frames from the host, {} from the device)",
        path.display(),
        capture.sent().len(),
        capture.received().len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_round_trips_through_its_text_form() {
        let mut c = Capture::default();
        c.headers.push(("note".into(), "connect + gaze".into()));
        c.headers
            .push(("recorded-at".into(), "2026-09-09T10:00:00Z".into()));
        c.events.push(Event::Sent(vec![0x54, 0x54, 0x50, 0x00]));
        c.events.push(Event::Received(vec![0xff, 0x00, 0x10]));

        let text = c.to_text();
        assert!(text.starts_with("# tobii-capture 1\n"));
        assert!(text.contains("# note connect + gaze\n"));
        assert!(text.contains("> 54545000\n"));
        assert!(text.contains("< ff0010\n"));

        let back = Capture::parse(&text).expect("round trip");
        assert_eq!(back, c);
    }

    #[test]
    fn an_empty_frame_survives_the_round_trip() {
        let mut c = Capture::default();
        c.events.push(Event::Received(Vec::new()));
        let back = Capture::parse(&c.to_text()).unwrap();
        assert_eq!(back.events, vec![Event::Received(Vec::new())]);
    }

    #[test]
    fn nonsense_is_refused_rather_than_read_as_an_empty_session() {
        assert!(matches!(
            Capture::parse("just some text"),
            Err(CaptureError::NotACapture)
        ));
        assert!(matches!(
            Capture::parse("# tobii-capture 99\n"),
            Err(CaptureError::Version(99))
        ));
        assert!(matches!(
            Capture::parse("# tobii-capture 1\n> zz\n"),
            Err(CaptureError::BadHex { line: 2 })
        ));
        assert!(matches!(
            Capture::parse("# tobii-capture 1\n> abc\n"),
            Err(CaptureError::BadHex { line: 2 })
        ));
        assert!(matches!(
            Capture::parse("# tobii-capture 1\n? 00\n"),
            Err(CaptureError::BadLine { line: 2 })
        ));
    }

    /// A future version may add headers this build does not know. Refusing to
    /// read the file over that would make every old build reject every new
    /// capture, for no benefit.
    #[test]
    fn unknown_headers_are_kept_rather_than_refused() {
        let c = Capture::parse("# tobii-capture 1\n# something-new yes\n< 00\n").unwrap();
        assert_eq!(c.header("something-new"), Some("yes"));
        assert_eq!(c.received().len(), 1);
    }

    #[test]
    fn replay_hands_back_the_recorded_replies_in_order() {
        let mut c = Capture::default();
        c.events.push(Event::Sent(vec![1]));
        c.events.push(Event::Received(vec![0xaa, 0xbb]));
        c.events.push(Event::Received(vec![0xcc]));

        let mut r = ReplayTransport::new(&c);
        let mut buf = [0u8; 8];

        assert_eq!(r.recv(&mut buf, Duration::from_millis(1)), Some(2));
        assert_eq!(&buf[..2], &[0xaa, 0xbb]);
        assert_eq!(r.recv(&mut buf, Duration::from_millis(1)), Some(1));
        assert_eq!(&buf[..1], &[0xcc]);

        // Past the end is a timeout, not an error — that is what a driver
        // polling for the next gaze frame sees when the recording runs out.
        assert_eq!(r.recv(&mut buf, Duration::from_millis(1)), None);
        assert_eq!(r.exhausted_reads(), 1);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn replay_collects_what_the_driver_sent() {
        let c = Capture::default();
        let mut r = ReplayTransport::new(&c);
        r.send(&[1, 2, 3]).unwrap();
        r.send(&[4]).unwrap();
        assert_eq!(r.sent(), &[vec![1, 2, 3], vec![4]]);
    }

    /// Recording must not change what the driver sees, or the capture would be
    /// of a session that never happened.
    #[test]
    fn recording_is_transparent_to_the_driver() {
        let mut c = Capture::default();
        c.events.push(Event::Received(vec![9, 8, 7]));
        let inner = ReplayTransport::new(&c);
        let mut rec = RecordTransport::new(inner).with_note("test");

        rec.send(&[1, 2]).unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(rec.recv(&mut buf, Duration::from_millis(1)), Some(3));
        assert_eq!(&buf[..3], &[9, 8, 7]);

        let out = rec.into_capture();
        assert_eq!(out.sent(), vec![&[1u8, 2][..]]);
        assert_eq!(out.received(), vec![&[9u8, 8, 7][..]]);
        assert_eq!(out.header("note"), Some("test"));
    }

    #[test]
    fn hex_is_lower_case_and_round_trips_every_byte() {
        let all: Vec<u8> = (0..=255u8).collect();
        let hex = to_hex(&all);
        assert_eq!(hex.len(), 512);
        assert!(hex
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
        assert_eq!(from_hex(&hex).unwrap(), all);
        // Upper case is accepted on the way in: captures get hand-edited.
        assert_eq!(from_hex("FF00Aa").unwrap(), vec![0xff, 0x00, 0xaa]);
        assert_eq!(from_hex("f"), None);
        assert_eq!(from_hex("gg"), None);
        assert_eq!(from_hex("").unwrap(), Vec::<u8>::new());
    }
}
