# tobii-usb Transport + `tobii stream` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Connect to a physical Tobii Eye Tracker 5 over USB (libusb via `rusb`), drive the handshake to completion, and stream live decoded gaze samples through a `tobii stream` CLI — the first end-to-end, on-hardware milestone.

**Architecture:** A `Transport` trait abstracts byte I/O. `UsbTransport` implements it over `rusb` (open `2104:0313`, detach kernel driver, claim interface 0, vendor control `0x41`/`0x42` session open/close, bulk OUT `0x05` / IN `0x83`). A generic `Connection<T: Transport>` wires the existing `tobii_protocol::Handshake` + `Parser` + `GazeSample` into a connect-then-stream loop — and is fully unit-tested against a scripted `MockTransport`, so all the driver logic is verified without hardware. Only the thin `rusb` glue and the CLI need the device, and only at the final live-smoke task.

**Tech Stack:** Rust (edition 2021), `rusb` 0.9 (libusb 1.0 bindings; system `libusb-1.0` is present), `cargo test`. Builds on the complete `tobii-protocol` crate. Reference for USB specifics: `ressources/tobiifree/driver/src/libusb_transport.zig`. GPL-3.0.

**Scope note:** Plan 3 of the v1 roadmap. Includes the review follow-ups: the handshake `Recv`-path test, the `RSP`-only routing contract (so gaze notifications never corrupt the handshake), the live capture of a real gaze frame as a regression golden vector, and **Spike R2** (does the gaze stream require realm auth?). Calibration, display-area config, head-pose, and opentrack output remain later plans.

---

### Task 1: Follow-up — handshake `Recv`-path test (tobii-protocol)

The existing handshake tests always feed a response immediately, so `HandshakeAction::Recv` (the "no response yet, read more" branch) is never exercised. Lock it in.

**Files:**
- Modify: `crates/tobii-protocol/src/handshake.rs` (add one test to `handshake_tests`)

- [ ] **Step 1: Add the failing test**

In `crates/tobii-protocol/src/handshake.rs`, inside `mod handshake_tests`, add:

```rust
    #[test]
    fn poll_asks_to_recv_before_any_response() {
        let mut hs = Handshake::new(0x500);
        // First poll builds + sends hello.
        assert!(matches!(hs.poll(), HandshakeAction::Send(_)));
        // With no response fed yet, the next poll must ask the transport to read.
        assert!(matches!(hs.poll(), HandshakeAction::Recv));
        // Still Recv on a repeat call (idempotent until a response arrives).
        assert!(matches!(hs.poll(), HandshakeAction::Recv));
    }
```

- [ ] **Step 2: Run it (it should already PASS — this is a characterization test)**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib handshake::handshake_tests::poll_asks_to_recv_before_any_response`
Expected: PASS. (The behavior already exists; this test documents and guards it. If it unexpectedly FAILS, stop and report — the state machine diverged from intent.)

- [ ] **Step 3: Commit**

```bash
git add crates/tobii-protocol/src/handshake.rs
git commit -m "test(protocol): cover handshake Recv path before first response

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Scaffold `tobii-usb` crate + track Cargo.lock

**Files:**
- Create: `crates/tobii-usb/Cargo.toml`
- Create: `crates/tobii-usb/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)
- Modify: `.gitignore` (stop ignoring `Cargo.lock`)

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`, change the `members` line to:

```toml
members = ["crates/tobii-protocol", "crates/tobii-usb"]
```

- [ ] **Step 2: Track `Cargo.lock`**

The workspace now has crates that will become a binary; the lockfile should be committed. In `.gitignore`, **remove the `Cargo.lock` line** (keep `target/`). The Rust section should read:

```gitignore
# Rust
target/
```

- [ ] **Step 3: Create `crates/tobii-usb/Cargo.toml`**

```toml
[package]
name = "tobii-usb"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "libusb (rusb) transport + connection driver for the Tobii Eye Tracker 5."

[dependencies]
tobii-protocol = { path = "../tobii-protocol" }
rusb = "0.9"
```

- [ ] **Step 4: Create a minimal `crates/tobii-usb/src/lib.rs`**

```rust
//! USB transport and connection driver for the Tobii Eye Tracker 5.
//!
//! `UsbTransport` moves bytes over libusb; `Connection` drives the protocol
//! handshake and decodes the live gaze stream. The driver logic is generic
//! over the `Transport` trait so it can be tested without hardware.
//!
//! (Public re-exports are added by later tasks as the items appear.)

mod transport;
```

- [ ] **Step 5: Verify it builds (downloads rusb + links system libusb)**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; touch crates/tobii-usb/src/transport.rs && cargo build -p tobii-usb`
Expected: rusb compiles and links against system `libusb-1.0`; build finishes. An empty `transport.rs` (an empty private module) compiles fine. If rusb fails to find libusb, confirm `pkg-config --modversion libusb-1.0` works (it should: 1.0.30).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml .gitignore Cargo.lock crates/tobii-usb
git commit -m "feat(usb): scaffold tobii-usb crate (rusb dependency)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: `Transport` trait + `UsbError`

**Files:**
- Create/replace: `crates/tobii-usb/src/transport.rs`
- Test: inline in `transport.rs`

- [ ] **Step 1: Write the failing test**

Put this in `crates/tobii-usb/src/transport.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_error_displays() {
        assert!(UsbError::DeviceNotFound.to_string().contains("not found"));
        assert!(UsbError::ShortWrite { wrote: 1, expected: 8 }
            .to_string()
            .contains("short"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-usb usb_error_displays`
Expected: FAIL — `cannot find type UsbError`.

- [ ] **Step 3: Write the implementation**

PREPEND to `transport.rs` (above the tests):

```rust
//! Byte-transport abstraction and its libusb implementation.

use std::time::Duration;

/// Errors from opening or talking to the device.
#[derive(Debug)]
pub enum UsbError {
    /// The Tobii ET5 (2104:0313) was not found on the bus.
    DeviceNotFound,
    /// A libusb operation failed.
    Usb(rusb::Error),
    /// A bulk write transferred fewer bytes than requested.
    ShortWrite { wrote: usize, expected: usize },
    /// The protocol handshake did not complete.
    Handshake,
}

impl std::fmt::Display for UsbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsbError::DeviceNotFound => write!(
                f,
                "Tobii ET5 (2104:0313) not found — is it plugged in, and is the udev rule installed?"
            ),
            UsbError::Usb(e) => write!(f, "libusb error: {e}"),
            UsbError::ShortWrite { wrote, expected } => {
                write!(f, "short bulk write: {wrote}/{expected} bytes")
            }
            UsbError::Handshake => write!(f, "handshake failed"),
        }
    }
}

impl std::error::Error for UsbError {}

impl From<rusb::Error> for UsbError {
    fn from(e: rusb::Error) -> Self {
        UsbError::Usb(e)
    }
}

/// A bidirectional byte transport. Implemented by [`UsbTransport`] for real
/// hardware and by mocks in tests.
pub trait Transport {
    /// Send all bytes of `data`. Errors if not all bytes were transferred.
    fn send(&mut self, data: &[u8]) -> Result<(), UsbError>;
    /// Read available bytes into `buf`, waiting up to `timeout`. Returns the
    /// number of bytes read, or `None` on timeout / no data.
    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> Option<usize>;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-usb usb_error_displays`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/tobii-usb/src/transport.rs
git commit -m "feat(usb): add Transport trait and UsbError

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: `UsbTransport` (rusb implementation)

This is the hardware glue — verified by **compilation** here; exercised live in Task 8. It mirrors `libusb_transport.zig` exactly: VID/PID `2104:0313`, interface 0, EP_OUT `0x05`, EP_IN `0x83`, vendor control `0x41` (session open) / `0x42` (session close).

**Files:**
- Modify: `crates/tobii-usb/src/transport.rs` (add `UsbTransport`)

- [ ] **Step 1: Append the implementation**

Add to `crates/tobii-usb/src/transport.rs`, AFTER the `Transport` trait and BEFORE the `#[cfg(test)]` module:

```rust
use rusb::{Direction, GlobalContext, Recipient, RequestType};

const VID: u16 = 0x2104;
const PID: u16 = 0x0313;
const IFACE: u8 = 0;
const EP_OUT: u8 = 0x05;
const EP_IN: u8 = 0x83;
const SESSION_OPEN: u8 = 0x41;
const SESSION_CLOSE: u8 = 0x42;

/// libusb-backed [`Transport`] for the Tobii ET5.
pub struct UsbTransport {
    handle: rusb::DeviceHandle<GlobalContext>,
}

impl UsbTransport {
    /// Open the device, detach any kernel driver, claim interface 0, and send
    /// the vendor session-open control transfer.
    pub fn open() -> Result<Self, UsbError> {
        let handle = rusb::open_device_with_vid_pid(VID, PID).ok_or(UsbError::DeviceNotFound)?;

        // Best-effort kernel driver detach (ignored if not attached / unsupported).
        if handle.kernel_driver_active(IFACE).unwrap_or(false) {
            let _ = handle.detach_kernel_driver(IFACE);
        }
        handle.claim_interface(IFACE)?;

        // Vendor session-open: bmRequestType = vendor | host-to-device | interface.
        let req_type = rusb::request_type(Direction::Out, RequestType::Vendor, Recipient::Interface);
        handle.write_control(req_type, SESSION_OPEN, 0, 0, &[], Duration::from_millis(1000))?;

        Ok(Self { handle })
    }
}

impl Transport for UsbTransport {
    fn send(&mut self, data: &[u8]) -> Result<(), UsbError> {
        let wrote = self.handle.write_bulk(EP_OUT, data, Duration::from_millis(1000))?;
        if wrote != data.len() {
            return Err(UsbError::ShortWrite { wrote, expected: data.len() });
        }
        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> Option<usize> {
        match self.handle.read_bulk(EP_IN, buf, timeout) {
            Ok(n) if n > 0 => Some(n),
            // Timeout (expected for polling) or zero-length: nothing this call.
            _ => None,
        }
    }
}

impl Drop for UsbTransport {
    fn drop(&mut self) {
        // Vendor session-close, then release the interface (best effort).
        let req_type = rusb::request_type(Direction::Out, RequestType::Vendor, Recipient::Interface);
        let _ = self
            .handle
            .write_control(req_type, SESSION_CLOSE, 0, 0, &[], Duration::from_millis(500));
        let _ = self.handle.release_interface(IFACE);
    }
}
```

- [ ] **Step 2: Re-export `UsbTransport`**

In `crates/tobii-usb/src/lib.rs`, update the re-export line to:

```rust
pub use transport::{Transport, UsbError, UsbTransport};
```

- [ ] **Step 3: Verify it builds (no hardware needed to compile)**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo build -p tobii-usb && cargo test -p tobii-usb 2>&1 | tail -3`
Expected: builds cleanly; the `usb_error_displays` test still passes. (No test opens a device.)

- [ ] **Step 4: Clippy**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo clippy -p tobii-usb --all-targets -- -D warnings 2>&1 | tail -2`
Expected: clean. Fix any warning minimally without changing the USB constants or logic.

- [ ] **Step 5: Commit**

```bash
git add crates/tobii-usb/src/transport.rs crates/tobii-usb/src/lib.rs
git commit -m "feat(usb): add UsbTransport (rusb open/claim/session/bulk)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: `Connection<T>` driver (handshake + gaze stream) — mock-tested

This is the heart of the crate and is **fully unit-tested without hardware** via a scripted `MockTransport`. It enforces the review follow-up: only `TTP_MAGIC_RSP` frames reach the handshake; `NOTIFY` gaze frames are buffered separately and never corrupt the handshake.

**Files:**
- Create: `crates/tobii-usb/src/connection.rs`
- Modify: `crates/tobii-usb/src/lib.rs` (add `mod connection;` + re-export)
- Test: inline in `connection.rs`

- [ ] **Step 1: Wire the module**

In `crates/tobii-usb/src/lib.rs`, add `mod connection;` after `mod transport;`, and add `Connection` to the re-export:

```rust
mod connection;
mod transport;

pub use connection::Connection;
pub use transport::{Transport, UsbError, UsbTransport};
```

- [ ] **Step 2: Write the failing tests**

Create `crates/tobii-usb/src/connection.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::Duration;
    use tobii_protocol::frame::{ENVELOPE_SIZE, TTP_HDR_SIZE, TTP_MAGIC_NOTIFY, TTP_MAGIC_RSP};
    use tobii_protocol::tlv::{write_f64_q42, write_tag, write_u32, TAG_POINT2D, TAG_XDS_COLUMN};

    /// Build an inbound USB frame: [01 00 00 00][len_LE][ttp hdr][payload].
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

    // A query-realm response payload selecting realm_type = 0 (no auth).
    fn realm_type_zero() -> Vec<u8> {
        let mut p = vec![0x00, 0x00, 0x02, 0x00, 0x00, 0x04];
        p.extend_from_slice(&0u32.to_be_bytes());
        p
    }

    // A minimal 0x500 gaze payload: timestamp + gaze_point_2d (0.25, 0.75).
    fn gaze_payload() -> Vec<u8> {
        let mut w = Vec::new();
        w.extend_from_slice(&[0x00, 0x00]);
        // xds_row count=2: tag = (2<<16)|0x0bb8, encoded via the tlv prolog.
        let mut buf = tobii_protocol::bytes::Writer::new();
        write_tag(&mut buf, (2u32 << 16) | 0x0bb8);
        write_tag(&mut buf, TAG_XDS_COLUMN);
        write_u32(&mut buf, 0x01); // timestamp column
        buf.push_u8(6);
        buf.push_be32(8);
        buf.push_be64(42i64 as u64);
        write_tag(&mut buf, TAG_XDS_COLUMN);
        write_u32(&mut buf, 0x1c); // gaze_point_2d
        write_tag(&mut buf, TAG_POINT2D);
        write_f64_q42(&mut buf, 0.25);
        write_f64_q42(&mut buf, 0.75);
        w.extend_from_slice(&buf.into_vec());
        w
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

    #[test]
    fn connects_via_no_auth_handshake() {
        let t = MockTransport {
            sent: Vec::new(),
            to_recv: VecDeque::from(vec![
                inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),           // hello reply
                inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()), // query → realm_type 0
                inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]), // open reply
            ]),
        };
        let conn = Connection::connect(t).expect("handshake should complete");
        // 4 frames sent: hello, query_realm, open_realm, subscribe.
        assert_eq!(conn.transport().sent.len(), 4);
    }

    #[test]
    fn streams_a_gaze_sample_after_connect() {
        let mut to_recv = VecDeque::from(vec![
            inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
            inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
            inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
        ]);
        // A gaze notification arrives after the handshake.
        to_recv.push_back(inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &gaze_payload()));
        let t = MockTransport { sent: Vec::new(), to_recv };

        let mut conn = Connection::connect(t).expect("connect");
        let s = conn.next_gaze().expect("a gaze sample");
        assert!(s.has(tobii_protocol::gaze::present::TIMESTAMP));
        assert_eq!(s.timestamp_us, 42);
        assert!((s.gaze_point_2d[0] - 0.25).abs() < 1e-9);
    }

    #[test]
    fn gaze_notification_during_handshake_does_not_break_it() {
        // A stray gaze NOTIFY frame interleaved with handshake responses must be
        // buffered, not fed to the handshake (which only consumes RSP frames).
        let t = MockTransport {
            sent: Vec::new(),
            to_recv: VecDeque::from(vec![
                inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
                inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &gaze_payload()), // stray gaze
                inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
                inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
            ]),
        };
        let mut conn = Connection::connect(t).expect("handshake survives stray gaze");
        // The buffered gaze frame is still available afterwards.
        assert!(conn.next_gaze().is_some());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-usb --lib connection::tests`
Expected: FAIL — `cannot find type Connection`.

- [ ] **Step 4: Write the implementation**

PREPEND to `connection.rs` (above the tests):

```rust
//! Connection driver: runs the handshake, then yields decoded gaze samples.

use std::collections::VecDeque;
use std::time::Duration;

use tobii_protocol::frame::{OP_GAZE_NOTIFY, STREAM_GAZE, TTP_MAGIC_NOTIFY, TTP_MAGIC_RSP};
use tobii_protocol::{Frame, GazeSample, Handshake, HandshakeAction, Parser};

use crate::transport::{Transport, UsbError};

const READ_BUF: usize = 16384;
const RECV_TIMEOUT: Duration = Duration::from_millis(100);
const GAZE_TIMEOUT: Duration = Duration::from_millis(1000);
const HANDSHAKE_STEP_CAP: u32 = 400;

/// A live connection to the eye tracker. Generic over [`Transport`] so the
/// driver logic is testable without hardware.
pub struct Connection<T: Transport> {
    transport: T,
    parser: Parser,
    /// Gaze samples decoded while draining (e.g. arriving during the handshake).
    gaze_queue: VecDeque<GazeSample>,
}

impl<T: Transport> Connection<T> {
    /// Open a connection: run the handshake to completion, leaving the device
    /// subscribed and streaming gaze.
    pub fn connect(transport: T) -> Result<Self, UsbError> {
        let mut conn = Self {
            transport,
            parser: Parser::new(),
            gaze_queue: VecDeque::new(),
        };
        conn.run_handshake()?;
        Ok(conn)
    }

    /// Access the underlying transport (used in tests).
    pub fn transport(&self) -> &T {
        &self.transport
    }

    fn run_handshake(&mut self) -> Result<(), UsbError> {
        let mut hs = Handshake::new(STREAM_GAZE);
        let mut buf = [0u8; READ_BUF];
        for _ in 0..HANDSHAKE_STEP_CAP {
            match hs.poll() {
                HandshakeAction::Send(bytes) => {
                    self.transport.send(&bytes)?;
                    self.drain(&mut buf, Some(&mut hs));
                }
                HandshakeAction::Recv => {
                    self.drain(&mut buf, Some(&mut hs));
                }
                HandshakeAction::Done => return Ok(()),
                HandshakeAction::Failed => return Err(UsbError::Handshake),
            }
        }
        Err(UsbError::Handshake)
    }

    /// Read one transport chunk, parse frames, and route them. Response frames
    /// go to the handshake (if any); gaze notifications are queued. Other
    /// frames are ignored.
    fn drain(&mut self, buf: &mut [u8], mut hs: Option<&mut Handshake>) {
        if let Some(n) = self.transport.recv(buf, RECV_TIMEOUT) {
            if let Ok(frames) = self.parser.feed(&buf[..n]) {
                for f in frames {
                    self.route(f, hs.as_deref_mut());
                }
            }
        }
    }

    fn route(&mut self, f: Frame, hs: Option<&mut Handshake>) {
        if f.magic == TTP_MAGIC_RSP {
            // Only response frames advance the handshake (seq-safety: gaze
            // notifications must never be mistaken for handshake responses).
            if let Some(hs) = hs {
                hs.on_response(&f.payload);
            }
        } else if f.magic == TTP_MAGIC_NOTIFY && f.op == OP_GAZE_NOTIFY {
            if let Some(s) = GazeSample::decode(&f.payload) {
                self.gaze_queue.push_back(s);
            }
        }
    }

    /// Return the next gaze sample, reading from the device if none is queued.
    /// Returns `None` if no gaze arrives within the read timeout.
    pub fn next_gaze(&mut self) -> Option<GazeSample> {
        if let Some(s) = self.gaze_queue.pop_front() {
            return Some(s);
        }
        let mut buf = [0u8; READ_BUF];
        if let Some(n) = self.transport.recv(&mut buf, GAZE_TIMEOUT) {
            if let Ok(frames) = self.parser.feed(&buf[..n]) {
                for f in frames {
                    self.route(f, None);
                }
            }
        }
        self.gaze_queue.pop_front()
    }
}
```

> **Note on the `connects_via_no_auth_handshake` test:** the handshake sends hello → query → open → subscribe. After each `Send`, `drain` reads exactly one queued frame. The subscribe `Send` is followed by a `drain` that finds the queue empty (`recv` → `None`), then the next `poll` returns `Done`. So 3 scripted responses drive 4 sent frames. This matches the `MockTransport` script.

- [ ] **Step 5: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-usb --lib connection::tests`
Expected: PASS (3 tests). If `tobii_protocol::bytes::Writer` / `tobii_protocol::tlv::*` / `tobii_protocol::frame::*` are not accessible, confirm those modules are `pub` in `tobii-protocol/src/lib.rs` (they are — `pub mod bytes; pub mod tlv; pub mod frame;`).

- [ ] **Step 6: Clippy + commit**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo clippy -p tobii-usb --all-targets -- -D warnings 2>&1 | tail -2`
Expected: clean.

```bash
git add crates/tobii-usb/src/connection.rs crates/tobii-usb/src/lib.rs
git commit -m "feat(usb): add Connection driver (handshake + gaze stream), mock-tested

Routes only RSP frames to the handshake; buffers NOTIFY gaze separately so a
stray gaze frame can't corrupt the handshake. Verified with a scripted
MockTransport (no hardware).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: `tobii-cli` — the `tobii stream` binary

**Files:**
- Create: `crates/tobii-cli/Cargo.toml`
- Create: `crates/tobii-cli/src/main.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Add to the workspace**

In the root `Cargo.toml`, set:

```toml
members = ["crates/tobii-protocol", "crates/tobii-usb", "crates/tobii-cli"]
```

- [ ] **Step 2: Create `crates/tobii-cli/Cargo.toml`**

```toml
[package]
name = "tobii-cli"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "Command-line interface for the Tobii ET5 Linux runtime."

[[bin]]
name = "tobii"
path = "src/main.rs"

[dependencies]
tobii-usb = { path = "../tobii-usb" }
tobii-protocol = { path = "../tobii-protocol" }
```

- [ ] **Step 3: Create `crates/tobii-cli/src/main.rs`**

```rust
//! `tobii` CLI. v1 subcommand: `stream` — print live gaze samples.

use std::process::ExitCode;

use tobii_protocol::gaze::present;
use tobii_usb::{Connection, UsbTransport};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("stream") => {
            let json = args.iter().any(|a| a == "--json");
            match stream(json) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(1)
                }
            }
        }
        _ => {
            eprintln!("usage: tobii stream [--json]");
            ExitCode::from(2)
        }
    }
}

fn stream(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("opening Tobii ET5...");
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    eprintln!("connected — streaming gaze (Ctrl-C to stop)");

    loop {
        let Some(s) = conn.next_gaze() else {
            continue; // read timeout — keep waiting
        };
        if json {
            println!(
                "{{\"t\":{},\"valid\":{},\"x\":{:.5},\"y\":{:.5}}}",
                s.timestamp_us,
                s.has(present::GAZE_2D),
                s.gaze_point_2d[0],
                s.gaze_point_2d[1]
            );
        } else if s.has(present::GAZE_2D) {
            println!(
                "t={:>12}  gaze=({:.4}, {:.4})  valL={} valR={}",
                s.timestamp_us, s.gaze_point_2d[0], s.gaze_point_2d[1], s.validity_l, s.validity_r
            );
        } else {
            println!("t={:>12}  (no 2D gaze this frame)", s.timestamp_us);
        }
    }
}
```

- [ ] **Step 4: Verify it builds**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo build -p tobii-cli && cargo clippy -p tobii-cli --all-targets -- -D warnings 2>&1 | tail -2`
Expected: builds cleanly; clippy clean. (Running it needs hardware — that's Task 8.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/tobii-cli
git commit -m "feat(cli): add tobii stream command

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: udev rule + run instructions

**Files:**
- Create: `assets/99-tobii.rules`
- Create: `README.md` (repo root)

- [ ] **Step 1: Create the udev rule**

Create `assets/99-tobii.rules` (same as the reference — grants non-root access via `uaccess`/`0666`):

```
# Tobii Eye Tracker 5 (EyeChip) — non-root access
# Bootloader / DFU mode
SUBSYSTEM=="usb", ATTR{idVendor}=="2104", ATTR{idProduct}=="0102", MODE="0666", TAG+="uaccess"
# Runtime mode
SUBSYSTEM=="usb", ATTR{idVendor}=="2104", ATTR{idProduct}=="0313", MODE="0666", TAG+="uaccess"
```

- [ ] **Step 2: Create the repo README**

Create `README.md`:

```markdown
# TobiiLinux

A native Linux runtime for the Tobii Eye Tracker 5, written in Rust. Clean-room
reimplementation of the device's USB protocol (GPL-3.0; the `tobiifree` project
is the protocol reference).

## Crates
- `tobii-protocol` — pure protocol codec + handshake state machine (no I/O).
- `tobii-usb` — libusb (rusb) transport + connection driver.
- `tobii-cli` — the `tobii` command-line tool.

## Setup (once)
Install the udev rule so the device is accessible without root:
```sh
sudo cp assets/99-tobii.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger
```
Then plug in (or re-plug) the Eye Tracker 5.

## Build & run
```sh
cargo build --release
./target/release/tobii stream          # human-readable gaze
./target/release/tobii stream --json   # one JSON object per sample
```

## Status
v1 in progress: gaze streaming works; display-area config, calibration,
head-pose, and opentrack output are upcoming. See `docs/superpowers/`.

License: GPL-3.0-only.
```

- [ ] **Step 3: Commit**

```bash
git add assets/99-tobii.rules README.md
git commit -m "docs: add udev rule and project README

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: LIVE SMOKE TEST (requires the ET5 plugged in)

This is the proof. It needs the physical Tobii Eye Tracker 5 connected. It also answers **Spike R2** and captures a real gaze frame as a regression vector (the deferred review follow-up). **Do not skip or simulate any step.**

**Files:**
- (Possibly) Modify: `crates/tobii-protocol/src/gaze.rs` (add a real-capture golden test)

- [ ] **Step 1: Confirm the device is present**

Run: `lsusb | grep -i 2104`
Expected: a line showing `2104:0313` (runtime) — if it shows `2104:0102`, the device is in bootloader mode; stop and report (DFU is out of scope for v1). If nothing appears, the ET5 is not plugged in — **ask the user to plug it in before continuing.**

- [ ] **Step 2: Install the udev rule (if not already)**

```bash
sudo cp assets/99-tobii.rules /etc/udev/rules.d/ && sudo udevadm control --reload && sudo udevadm trigger
```
Then re-plug the device so the rule applies. (This step needs the user's sudo password; if it cannot run non-interactively, ask the user to run it.)

- [ ] **Step 3: Run the stream**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo run --release -p tobii-cli -- stream`
Expected: `opening...` then `connected — streaming gaze`, then a flow of `t=... gaze=(x, y)...` lines that change as you move your eyes. Let it run ~5 seconds, then Ctrl-C.

**If it fails:**
- `not found` → re-check Step 1/2 (device + udev rule + re-plug).
- `libusb error: Access denied` → udev rule not applied; re-plug or re-run Step 2.
- `handshake failed` → capture stderr and **record the finding for Spike R2** (see Step 4); the realm path may differ from the reference.

- [ ] **Step 4: Record Spike R2 finding**

Add a one-paragraph note to `docs/superpowers/specs/2026-06-14-tobii-et5-linux-driver-design.md` under §10, stating what `realm_type` the gaze stream reported (auth required vs `0`) and whether the handshake completed unmodified. Commit:

```bash
git add docs/superpowers/specs/2026-06-14-tobii-et5-linux-driver-design.md
git commit -m "docs: record Spike R2 (gaze stream realm/auth) finding from live test

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 5: Capture a real gaze frame as a regression golden vector**

While streaming works, capture one real raw `0x500` payload (add a temporary `eprintln!("{:02x?}", f.payload)` in `Connection::route`'s gaze branch, run briefly, copy one payload's bytes, then revert the `eprintln!`). Add a test to `crates/tobii-protocol/src/gaze.rs` `mod tests` that decodes the captured bytes and asserts the fields are sane (e.g. `decode` returns `Some`, `present` includes the columns the device actually sent, 2D gaze within `[-0.5, 1.5]`). This replaces the synthetic-only coverage with a real-device vector. Commit:

```bash
git add crates/tobii-protocol/src/gaze.rs
git commit -m "test(protocol): add real captured gaze frame as golden decode vector

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 6: Final verification**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test 2>&1 | grep -E "test result|Running"`
Expected: all crates' tests pass.

---

## Roadmap (after this plan)

With live gaze proven, the remaining v1 plans:
- **Plan 4 — `tobii-config`** (Spike S3: decompile the MSI `Tobii.Configuration`/`TetConfig`; reimplement display-setup math; `tobii setup` + `tobii display get/set`).
- **Plan 5 — `tobii-headpose` + opentrack** (Spike S1 pitch axis, Spike S2 opentrack UDP packet format; derive 6DOF; `tobii opentrack`; live Star Citizen test). Add the deferred gaze columns then (see memory `gaze-decode-deferred-columns`).

## Plan self-review notes

- **Spec coverage:** USB transport (open/claim/session/bulk, the spec's `tobii-usb` crate) → Tasks 2–4; driver loop wiring Handshake+Parser+GazeSample → Task 5; `tobii stream` CLI → Task 6; udev rule → Task 7; live validation + Spike R2 → Task 8. Display-area/config, head-pose, opentrack remain explicitly out of scope (Plans 4–5), per the spec's phasing.
- **Follow-ups included:** handshake `Recv`-path test (Task 1); RSP-only routing / seq-safety (Task 5, `route`); real captured gaze golden vector (Task 8 Step 5); Spike R2 (Task 8 Step 4).
- **Placeholder scan:** every code step is complete; the only "fill-in" is the *real captured bytes* in Task 8 Step 5, which is intrinsic to a hardware-capture step, not a spec gap.
- **Type consistency:** `Transport::{send, recv}`, `UsbError` variants, `UsbTransport::open`, `Connection::{connect, next_gaze, transport}` are used identically across Tasks 3–6. `Connection` routes on `TTP_MAGIC_RSP`/`TTP_MAGIC_NOTIFY`/`OP_GAZE_NOTIFY`/`STREAM_GAZE` — all exist in `tobii-protocol::frame` from Plan 1. `GazeSample`, `Handshake`, `HandshakeAction`, `Parser`, `Frame` are the Plan 1/2 re-exports.
- **Hardware boundary:** Tasks 1–7 are fully verifiable on the dev machine (unit tests + builds); only Task 8 needs the ET5.
