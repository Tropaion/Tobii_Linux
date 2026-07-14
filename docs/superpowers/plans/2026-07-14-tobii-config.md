# tobii-config (display setup) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user configure the eye tracker's display area from their physical monitor geometry — `tobii setup` (interactive), `tobii display get`, `tobii display set` — backed by a new pure `tobii-config` crate.

**Architecture:** A new pure crate `tobii-config` holds the planar display-setup math (Spike S3 "Model B": physical inputs ↔ the three tracker-space corners) and a hand-rolled TOML config store. `tobii-usb::Connection` gains a generic request/response method so post-handshake commands (get/set display area) can round-trip while gaze keeps streaming. `tobii-cli` wires three new subcommands through the existing `tobii-protocol` command builders + `tobii-usb`. The device wire encoding already exists in `tobii-protocol` (`build_set_display_area_corners`, `DisplayCorners::decode`) and is reused unchanged.

**Tech Stack:** Rust (edition 2021), `cargo test`/`clippy`. Builds on `tobii-protocol` (pure codec) and `tobii-usb` (rusb transport + Connection). Reference: `docs/superpowers/specs/2026-07-14-spike-s3-display-setup-math.md` (the validated math + golden vector). GPL-3.0.

**Scope note:** Plan 4 of the v1 roadmap. Delivers display-area configuration only. Head-pose + opentrack (Plan 5) and per-user gaze calibration (Phase 2) remain later plans. Live hardware validation is the final task.

## Global Constraints

- Rust **edition 2021**, license **GPL-3.0-only** (inherited via `edition.workspace`/`license.workspace`).
- **Zero new external dependencies.** The workspace's only external crate is `rusb` (pulled by `tobii-usb`). `tobii-config` must depend on nothing but `tobii-protocol`. TOML is hand-rolled (matching the project's hand-rolled MD5/TLV/arg-parsing ethos).
- All `cargo` commands: prefix with `export PATH="$HOME/.cargo/bin:$PATH"`.
- Every task ends **rustfmt-clean and clippy-clean**: `cargo fmt` and `cargo clippy --all-targets -- -D warnings`.
- **Display-setup math = Spike S3 Model B**, tracker-space millimetres, right-handed (+X right, +Y up, +Z backward). Tilt is exposed to the user as an **angle in degrees**; internally `tilt_mm = height·sin(θ)`; corners use the **`TL.z = cz + tilt_mm`** sign convention (NOT the shipped tobiifree `cz − tEff`). Level top edge assumed (no roll/yaw).
- Config file: **`$XDG_CONFIG_HOME/tobii-linux/config.toml`**, falling back to `$HOME/.config/tobii-linux/config.toml`.
- Commit messages end with:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

### Task 1: `Handshake::seq()` getter (tobii-protocol)

Post-handshake requests (`Connection::request`, Task 3) must continue the TTP sequence counter the handshake left off at. Expose it.

**Files:**
- Modify: `crates/tobii-protocol/src/handshake.rs` (add one method + one test)

**Interfaces:**
- Produces: `Handshake::seq(&self) -> u32` — the next sequence number the handshake would use (i.e. one past the last frame it sent).

- [ ] **Step 1: Write the failing test**

In `crates/tobii-protocol/src/handshake.rs`, inside `mod handshake_tests`, add:

```rust
    #[test]
    fn seq_advances_past_the_handshake_frames() {
        let mut hs = Handshake::new(0x500);
        assert_eq!(hs.seq(), 1); // fresh: next seq is 1
        // No-auth path sends 4 frames (hello, query, open, subscribe) → seq ends at 5.
        let responses = vec![prefixed(&[]), prefixed(&[u32_field(0)]), prefixed(&[])];
        let (_sent, term) = run(&mut hs, responses);
        assert!(matches!(term, HandshakeAction::Done));
        assert_eq!(hs.seq(), 5);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib handshake::handshake_tests::seq_advances_past_the_handshake_frames`
Expected: FAIL — `no method named seq found`.

- [ ] **Step 3: Add the getter**

In `crates/tobii-protocol/src/handshake.rs`, add this method inside `impl Handshake`, immediately after `next_seq` (around line 133):

```rust
    /// The next sequence number this handshake would use. After the handshake
    /// reaches `Done`, callers continue post-handshake requests from here.
    pub fn seq(&self) -> u32 {
        self.seq
    }
```

- [ ] **Step 4: Run it to verify it passes**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib handshake::handshake_tests::seq_advances_past_the_handshake_frames`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt
git add crates/tobii-protocol/src/handshake.rs
git commit -m "feat(protocol): expose Handshake::seq for post-handshake requests

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: payload-only display-area builder (tobii-protocol)

`Connection::request` (Task 3) frames requests itself from `(op, payload)`, so it needs the set-display-area **payload** without the frame. Extract it; the existing frame builder delegates to it (byte-identical output).

**Files:**
- Modify: `crates/tobii-protocol/src/commands.rs`

**Interfaces:**
- Consumes: `crate::frame::build_out_frame`, `crate::tlv::{write_point, write_tag, write_u32}`, `crate::bytes::Writer` (all already imported).
- Produces: `commands::set_display_area_corners_payload(tl_x, tl_y, tl_z, tr_x, tr_y, tr_z, bl_x, bl_y, bl_z: f64) -> Vec<u8>` (the 164-byte SET_DISPLAY_AREA payload).

- [ ] **Step 1: Write the failing test**

In `crates/tobii-protocol/src/commands.rs`, inside `mod tests`, add:

```rust
    #[test]
    fn set_display_area_corners_payload_is_164_bytes() {
        let p = set_display_area_corners_payload(
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        );
        // 2 prefix + 3*point(48) + tag(9) + u32(9) = 164 (frame - envelope(8) - header(24)).
        assert_eq!(p.len(), 164);
    }

    #[test]
    fn frame_builder_payload_matches_standalone_payload() {
        let f = build_set_display_area_corners(
            7, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0,
        );
        let p = set_display_area_corners_payload(
            1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0,
        );
        // The frame's payload (after 8-byte envelope + 24-byte header) equals the builder.
        assert_eq!(&f[32..], &p[..]);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib commands::tests::set_display_area_corners_payload`
Expected: FAIL — `cannot find function set_display_area_corners_payload`.

- [ ] **Step 3: Refactor to extract the payload builder**

In `crates/tobii-protocol/src/commands.rs`, replace the existing `build_set_display_area_corners` function (lines ~56-79) with the following two functions:

```rust
/// The SET_DISPLAY_AREA payload for three explicit corners (each tracker-relative,
/// mm). Order on the wire is TL, TR, BL; bottom-right is implied by the device.
#[allow(clippy::too_many_arguments)]
pub fn set_display_area_corners_payload(
    tl_x: f64,
    tl_y: f64,
    tl_z: f64,
    tr_x: f64,
    tr_y: f64,
    tr_z: f64,
    bl_x: f64,
    bl_y: f64,
    bl_z: f64,
) -> Vec<u8> {
    let mut pay = Writer::new();
    pay.push_u8(0x00);
    pay.push_u8(0x00);
    write_point(&mut pay, tl_x, tl_y, tl_z);
    write_point(&mut pay, tr_x, tr_y, tr_z);
    write_point(&mut pay, bl_x, bl_y, bl_z);
    write_tag(&mut pay, 0x10100);
    write_u32(&mut pay, 0x3039);
    pay.into_vec()
}

/// Set display area from explicit corners (each tracker-relative, mm).
#[allow(clippy::too_many_arguments)]
pub fn build_set_display_area_corners(
    seq: u32,
    tl_x: f64,
    tl_y: f64,
    tl_z: f64,
    tr_x: f64,
    tr_y: f64,
    tr_z: f64,
    bl_x: f64,
    bl_y: f64,
    bl_z: f64,
) -> Vec<u8> {
    let payload =
        set_display_area_corners_payload(tl_x, tl_y, tl_z, tr_x, tr_y, tr_z, bl_x, bl_y, bl_z);
    build_out_frame(seq, OP_SET_DISPLAY_AREA, &payload)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib commands::tests`
Expected: PASS (new tests + the existing `set_display_area_frame_structure`/`get_display_area_is_empty_payload` still pass — the frame builder's bytes are unchanged).

- [ ] **Step 5: Clippy + commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy -p tobii-protocol --all-targets -- -D warnings 2>&1 | tail -2
git add crates/tobii-protocol/src/commands.rs
git commit -m "refactor(protocol): expose set_display_area_corners_payload

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: `Connection::request` request/response path (tobii-usb)

The gaze-only `Connection` can't send a command and read its reply. Add a generic `request(op, payload)` that frames + sends the request (continuing the handshake's seq), then reads until the matching-`op` RSP arrives, queuing any gaze notifications for `next_gaze`. Mock-tested — no hardware.

**Files:**
- Modify: `crates/tobii-usb/src/connection.rs`

**Interfaces:**
- Consumes: `Handshake::seq()` (Task 1); `tobii_protocol::frame::build_out_frame`.
- Produces: `Connection::request(&mut self, op: u32, payload: &[u8]) -> Result<Option<Vec<u8>>, UsbError>` — `Ok(Some(payload))` on the first RSP whose op matches; `Ok(None)` if none arrives within the read window.

- [ ] **Step 1: Write the failing test**

In `crates/tobii-usb/src/connection.rs`, inside `mod tests`, add:

```rust
    #[test]
    fn request_returns_matching_response_and_queues_gaze() {
        let mut to_recv = VecDeque::from(vec![
            inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
            inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
            inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
        ]);
        // After connect: a stray gaze frame, then the get-display-area response.
        to_recv.push_back(inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &gaze_payload()));
        to_recv.push_back(inbound(TTP_MAGIC_RSP, 5, 0x596, &[0xAA, 0xBB]));
        let t = MockTransport {
            sent: Vec::new(),
            to_recv,
        };
        let mut conn = Connection::connect(t).expect("connect");
        let resp = conn
            .request(0x596, &[])
            .expect("io ok")
            .expect("a response");
        assert_eq!(resp, vec![0xAA, 0xBB]);
        // The gaze that arrived before the response was queued, not dropped.
        assert!(conn.next_gaze().is_some());
    }

    #[test]
    fn request_returns_none_when_no_matching_response() {
        let t = MockTransport {
            sent: Vec::new(),
            to_recv: VecDeque::from(vec![
                inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
                inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
                inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
            ]),
        };
        let mut conn = Connection::connect(t).expect("connect");
        // Nothing left to receive → the request window drains to None.
        assert!(conn.request(0x596, &[]).expect("io ok").is_none());
    }

    #[test]
    fn request_queues_gaze_that_trails_the_response_in_one_chunk() {
        // One transport read delivers the matching RSP followed by a gaze NOTIFY;
        // the trailing gaze must be queued, not dropped. The empty filler read is
        // absorbed by the extra drain `run_handshake` does after the subscribe
        // send, so the RSP+gaze chunk survives for `request` to read rather than
        // being consumed during `connect`.
        let mut to_recv = VecDeque::from(vec![
            inbound(TTP_MAGIC_RSP, 1, 0x3e8, &[]),
            inbound(TTP_MAGIC_RSP, 2, 0x640, &realm_type_zero()),
            inbound(TTP_MAGIC_RSP, 3, 0x76c, &[0x00, 0x00]),
            Vec::new(),
        ]);
        let mut chunk = inbound(TTP_MAGIC_RSP, 5, 0x596, &[0xAA, 0xBB]);
        chunk.extend_from_slice(&inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &gaze_payload()));
        to_recv.push_back(chunk);
        let t = MockTransport {
            sent: Vec::new(),
            to_recv,
        };
        let mut conn = Connection::connect(t).expect("connect");
        let resp = conn
            .request(0x596, &[])
            .expect("io ok")
            .expect("a response");
        assert_eq!(resp, vec![0xAA, 0xBB]);
        assert!(
            conn.next_gaze().is_some(),
            "trailing gaze must be queued, not dropped"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-usb --lib connection::tests::request`
Expected: FAIL — `no method named request found`.

- [ ] **Step 3: Add the `seq` field, initialise it from the handshake, and add `request`**

In `crates/tobii-usb/src/connection.rs`:

(a) Update the imports at the top to add `build_out_frame`:

```rust
use tobii_protocol::frame::{
    build_out_frame, OP_GAZE_NOTIFY, STREAM_GAZE, TTP_MAGIC_NOTIFY, TTP_MAGIC_RSP,
};
```

(b) Add a read-cap constant next to the other consts (after `HANDSHAKE_STEP_CAP`, ~line 14):

```rust
const REQUEST_READ_CAP: u32 = 100;
```

(c) Add a `seq` field to the struct (after `gaze_queue`, ~line 21):

```rust
    /// Next TTP sequence number for post-handshake requests.
    seq: u32,
```

(d) Initialise it in `connect` (the `Self { .. }` literal, ~line 28):

```rust
        let mut conn = Self {
            transport,
            parser: Parser::new(),
            gaze_queue: VecDeque::new(),
            seq: 1,
        };
```

(e) In `run_handshake`, capture the handshake's final seq. Change the `Done` arm (~line 54) from `HandshakeAction::Done => return Ok(()),` to:

```rust
                HandshakeAction::Done => {
                    self.seq = hs.seq();
                    return Ok(());
                }
```

(f) Add these two methods inside `impl<T: Transport> Connection<T>` (place them after `transport`, before `run_handshake`):

```rust
    fn next_seq(&mut self) -> u32 {
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1);
        if self.seq == 0 {
            self.seq = 1;
        }
        s
    }

    /// Send a request frame and return the payload of the first response frame
    /// whose op matches. Gaze notifications arriving in the meantime are queued
    /// for [`Connection::next_gaze`]. Returns `Ok(None)` if no matching response
    /// arrives within the read window.
    pub fn request(&mut self, op: u32, payload: &[u8]) -> Result<Option<Vec<u8>>, UsbError> {
        let seq = self.next_seq();
        self.transport.send(&build_out_frame(seq, op, payload))?;
        let mut buf = [0u8; READ_BUF];
        for _ in 0..REQUEST_READ_CAP {
            let Some(n) = self.transport.recv(&mut buf, RECV_TIMEOUT) else {
                continue;
            };
            let Ok(frames) = self.parser.feed(&buf[..n]) else {
                continue;
            };
            // Route EVERY frame in the batch (a single read can coalesce the
            // response with trailing gaze notifications); capture the first
            // matching RSP but only return after the batch is fully drained, so
            // gaze that follows the response in the same chunk is never dropped.
            let mut matched = None;
            for f in frames {
                if matched.is_none() && f.magic == TTP_MAGIC_RSP && f.op == op {
                    matched = Some(f.payload);
                } else {
                    self.route(f, None);
                }
            }
            if matched.is_some() {
                return Ok(matched);
            }
        }
        Ok(None)
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-usb --lib connection::tests`
Expected: PASS (the three new tests + the three existing ones).

- [ ] **Step 5: Clippy + commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy -p tobii-usb --all-targets -- -D warnings 2>&1 | tail -2
git add crates/tobii-usb/src/connection.rs
git commit -m "feat(usb): add Connection::request (send + await matching RSP)

Continues the handshake seq; buffers gaze notifications so a command
response can be read mid-stream. Mock-tested (no hardware).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: scaffold `tobii-config` + display-setup geometry

The pure crate + the planar Model B math (forward + inverse) validated against the Spike S3 golden vector. No I/O.

**Files:**
- Create: `crates/tobii-config/Cargo.toml`
- Create: `crates/tobii-config/src/lib.rs`
- Create: `crates/tobii-config/src/setup.rs`
- Modify: `Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: `tobii_protocol::DisplayCorners { tl: [f64; 3], tr: [f64; 3], bl: [f64; 3] }`.
- Produces:
  - `tobii_config::DisplaySetup { width_mm, height_mm, tilt_deg, offset_x_mm, offset_y_mm, offset_z_mm: f64 }` (all `pub`, derives `Debug, Clone, Copy, PartialEq`).
  - `DisplaySetup::to_corners(&self) -> tobii_protocol::DisplayCorners`
  - `DisplaySetup::from_corners(c: &tobii_protocol::DisplayCorners) -> DisplaySetup`

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`, set the members line to:

```toml
members = ["crates/tobii-protocol", "crates/tobii-usb", "crates/tobii-cli", "crates/tobii-config"]
```

- [ ] **Step 2: Create `crates/tobii-config/Cargo.toml`**

```toml
[package]
name = "tobii-config"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "Display-setup geometry + TOML config for the Tobii ET5 Linux runtime."

[dependencies]
tobii-protocol = { path = "../tobii-protocol" }
```

- [ ] **Step 3: Create `crates/tobii-config/src/lib.rs`**

```rust
//! Display-setup geometry and TOML config persistence for the Tobii ET5.
//!
//! [`DisplaySetup`] is the physical parametrization a user edits (monitor size,
//! screen tilt, tracker offsets); [`DisplaySetup::to_corners`] converts it to the
//! three tracker-space corners the device wants (Spike S3 "Model B"), and
//! [`DisplaySetup::from_corners`] inverts a device-reported area back to editable
//! params. No I/O beyond the config store (see `store`).

mod setup;

pub use setup::DisplaySetup;
```

- [ ] **Step 4: Write the failing test**

Create `crates/tobii-config/src/setup.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tobii_protocol::DisplayCorners;

    // Spike S3 golden vector (a real, working display area).
    const GOLDEN: DisplayCorners = DisplayCorners {
        tl: [-451.8, 413.6, 157.5],
        tr: [479.8, 413.6, 157.5],
        bl: [-451.8, 68.0, -11.0],
    };

    #[test]
    fn flat_untilted_rectangle() {
        let s = DisplaySetup {
            width_mm: 400.0,
            height_mm: 300.0,
            tilt_deg: 0.0,
            offset_x_mm: 0.0,
            offset_y_mm: 0.0,
            offset_z_mm: 0.0,
        };
        let c = s.to_corners();
        assert_eq!(c.bl, [-200.0, 0.0, 0.0]);
        assert_eq!(c.tl, [-200.0, 300.0, 0.0]);
        assert_eq!(c.tr, [200.0, 300.0, 0.0]);
    }

    #[test]
    fn tilt_preserves_edge_length_and_pushes_top_back() {
        let s = DisplaySetup {
            width_mm: 500.0,
            height_mm: 300.0,
            tilt_deg: 30.0,
            offset_x_mm: 10.0,
            offset_y_mm: 50.0,
            offset_z_mm: -5.0,
        };
        let c = s.to_corners();
        // Bottom edge is unaffected by tilt.
        assert!((c.bl[0] - (10.0 - 250.0)).abs() < 1e-9);
        assert!((c.bl[1] - 50.0).abs() < 1e-9);
        assert!((c.bl[2] - (-5.0)).abs() < 1e-9);
        // Side-edge length is preserved (== height).
        let dy = c.tl[1] - c.bl[1];
        let dz = c.tl[2] - c.bl[2];
        assert!(((dy * dy + dz * dz).sqrt() - 300.0).abs() < 1e-9);
        // z displacement == height * sin(tilt).
        assert!((dz - 300.0 * 30f64.to_radians().sin()).abs() < 1e-9);
        // Width is preserved and the top edge is level.
        assert!((c.tr[0] - c.tl[0] - 500.0).abs() < 1e-9);
        assert!((c.tl[1] - c.tr[1]).abs() < 1e-9);
        assert!((c.tl[2] - c.tr[2]).abs() < 1e-9);
    }

    #[test]
    fn from_corners_recovers_golden_params() {
        let s = DisplaySetup::from_corners(&GOLDEN);
        assert!((s.width_mm - 931.6).abs() < 0.05);
        assert!((s.height_mm - 384.489).abs() < 0.05);
        assert!((s.tilt_deg - 26.0).abs() < 0.05);
        assert!((s.offset_x_mm - 14.0).abs() < 0.05);
        assert!((s.offset_y_mm - 68.0).abs() < 1e-9);
        assert!((s.offset_z_mm - (-11.0)).abs() < 1e-9);
    }

    #[test]
    fn corners_setup_roundtrip_is_exact() {
        let s = DisplaySetup::from_corners(&GOLDEN);
        let c = s.to_corners();
        for i in 0..3 {
            assert!((c.tl[i] - GOLDEN.tl[i]).abs() < 1e-6);
            assert!((c.tr[i] - GOLDEN.tr[i]).abs() < 1e-6);
            assert!((c.bl[i] - GOLDEN.bl[i]).abs() < 1e-6);
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-config --lib setup::tests`
Expected: FAIL — `cannot find type DisplaySetup`.

- [ ] **Step 6: Write the implementation**

PREPEND to `crates/tobii-config/src/setup.rs` (above the tests):

```rust
//! Planar display-setup geometry (Spike S3 "Model B").
//!
//! Convention: tracker-space mm, right-handed (+X right, +Y up, +Z backward).
//! Tilt is a screen lean-back angle in degrees (+ = top edge toward +Z). The
//! top edge is level (no roll/yaw). See
//! `docs/superpowers/specs/2026-07-14-spike-s3-display-setup-math.md`.

use tobii_protocol::DisplayCorners;

/// Physical display-setup parameters a user edits. Lengths in millimetres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplaySetup {
    /// Active-area width (measured along tracker +X).
    pub width_mm: f64,
    /// Active-area height along the screen surface (the tilted side-edge length).
    pub height_mm: f64,
    /// Screen tilt back from vertical, degrees; + = top edge leans toward +Z.
    pub tilt_deg: f64,
    /// Horizontal offset of the screen centre from the tracker (usually 0).
    pub offset_x_mm: f64,
    /// Height of the screen's bottom edge above the tracker.
    pub offset_y_mm: f64,
    /// Depth of the screen's bottom edge from the tracker.
    pub offset_z_mm: f64,
}

impl DisplaySetup {
    /// Forward construction: parameters → the three tracker-space corners
    /// (TL, TR, BL). Bottom-right is implied by the device.
    pub fn to_corners(&self) -> DisplayCorners {
        let tilt = self.tilt_deg.to_radians();
        let tilt_mm = self.height_mm * tilt.sin();
        let dy = self.height_mm * tilt.cos();
        let half_w = self.width_mm / 2.0;
        let (cx, cy, cz) = (self.offset_x_mm, self.offset_y_mm, self.offset_z_mm);
        DisplayCorners {
            bl: [cx - half_w, cy, cz],
            tl: [cx - half_w, cy + dy, cz + tilt_mm],
            tr: [cx + half_w, cy + dy, cz + tilt_mm],
        }
    }

    /// Inverse: a device-reported (or edited) set of corners → editable params.
    pub fn from_corners(c: &DisplayCorners) -> DisplaySetup {
        let dy = c.tl[1] - c.bl[1];
        let dz = c.tl[2] - c.bl[2];
        DisplaySetup {
            width_mm: c.tr[0] - c.tl[0],
            height_mm: dy.hypot(dz),
            tilt_deg: dz.atan2(dy).to_degrees(),
            offset_x_mm: (c.tl[0] + c.tr[0]) / 2.0,
            offset_y_mm: c.bl[1],
            offset_z_mm: c.bl[2],
        }
    }
}
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-config --lib setup::tests`
Expected: PASS (4 tests).

- [ ] **Step 8: Clippy + commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy -p tobii-config --all-targets -- -D warnings 2>&1 | tail -2
git add Cargo.toml Cargo.lock crates/tobii-config
git commit -m "feat(config): add tobii-config crate + planar display-setup math

DisplaySetup <-> DisplayCorners (Spike S3 Model B); round-trips the
golden vector exactly. Zero external deps.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: hand-rolled TOML (de)serialization

`DisplaySetup` ↔ a small `[display]` TOML section. No `serde`/`toml` crate.

**Files:**
- Modify: `crates/tobii-config/src/setup.rs` (add `to_toml`/`from_toml` + tests)

**Interfaces:**
- Produces:
  - `DisplaySetup::to_toml(&self) -> String`
  - `DisplaySetup::from_toml(s: &str) -> Option<DisplaySetup>` (None if any of the six keys is missing or unparseable).

- [ ] **Step 1: Write the failing test**

In `crates/tobii-config/src/setup.rs` `mod tests`, add:

```rust
    #[test]
    fn toml_roundtrips() {
        let s = DisplaySetup {
            width_mm: 931.6,
            height_mm: 384.5,
            tilt_deg: 26.0,
            offset_x_mm: 14.0,
            offset_y_mm: 68.0,
            offset_z_mm: -11.0,
        };
        let text = s.to_toml();
        assert!(text.contains("[display]"));
        assert_eq!(DisplaySetup::from_toml(&text), Some(s));
    }

    #[test]
    fn from_toml_ignores_comments_blanks_and_inline_comments() {
        let text = "# my monitor\n\n\
                    [display]\n\
                    width_mm = 800.0   # active area\n\
                    height_mm = 335.0\n\
                    tilt_deg = 20.0\n\
                    offset_x_mm = 0.0\n\
                    offset_y_mm = 40.0\n\
                    offset_z_mm = -5.0\n";
        let s = DisplaySetup::from_toml(text).expect("parse");
        assert_eq!(s.width_mm, 800.0);
        assert_eq!(s.offset_z_mm, -5.0);
    }

    #[test]
    fn from_toml_missing_key_is_none() {
        let text = "[display]\nwidth_mm = 800.0\n";
        assert_eq!(DisplaySetup::from_toml(text), None);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-config --lib setup::tests::toml`
Expected: FAIL — `no method named to_toml`.

- [ ] **Step 3: Implement the (de)serializers**

Add these methods inside `impl DisplaySetup` in `crates/tobii-config/src/setup.rs` (after `from_corners`):

```rust
    /// Serialize to a `[display]` TOML section.
    pub fn to_toml(&self) -> String {
        format!(
            "# tobii-linux display setup — edit with `tobii setup`\n\
             [display]\n\
             width_mm = {}\n\
             height_mm = {}\n\
             tilt_deg = {}\n\
             offset_x_mm = {}\n\
             offset_y_mm = {}\n\
             offset_z_mm = {}\n",
            self.width_mm,
            self.height_mm,
            self.tilt_deg,
            self.offset_x_mm,
            self.offset_y_mm,
            self.offset_z_mm,
        )
    }

    /// Parse a `[display]` TOML section. Returns `None` unless all six keys are
    /// present and parse as `f64`. Ignores comments, blank lines, other sections.
    pub fn from_toml(s: &str) -> Option<DisplaySetup> {
        let mut in_display = false;
        let (mut w, mut h, mut t) = (None, None, None);
        let (mut ox, mut oy, mut oz) = (None, None, None);
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                in_display = line == "[display]";
                continue;
            }
            if !in_display {
                continue;
            }
            let Some((key, val)) = line.split_once('=') else {
                continue;
            };
            let val = val.split('#').next().unwrap_or("").trim();
            let Ok(v) = val.parse::<f64>() else {
                continue;
            };
            match key.trim() {
                "width_mm" => w = Some(v),
                "height_mm" => h = Some(v),
                "tilt_deg" => t = Some(v),
                "offset_x_mm" => ox = Some(v),
                "offset_y_mm" => oy = Some(v),
                "offset_z_mm" => oz = Some(v),
                _ => {}
            }
        }
        Some(DisplaySetup {
            width_mm: w?,
            height_mm: h?,
            tilt_deg: t?,
            offset_x_mm: ox?,
            offset_y_mm: oy?,
            offset_z_mm: oz?,
        })
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-config --lib setup::tests`
Expected: PASS (7 tests total).

- [ ] **Step 5: Clippy + commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy -p tobii-config --all-targets -- -D warnings 2>&1 | tail -2
git add crates/tobii-config/src/setup.rs
git commit -m "feat(config): hand-rolled TOML (de)serialization for DisplaySetup

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: config store (path + load/save)

Resolve the config path and read/write the file. Path resolution uses env; load/save take an explicit path so they're testable without mutating process-global env.

**Files:**
- Create: `crates/tobii-config/src/store.rs`
- Modify: `crates/tobii-config/src/lib.rs` (add `mod store;` + re-exports)

**Interfaces:**
- Consumes: `crate::DisplaySetup` (`to_toml`/`from_toml`).
- Produces:
  - `config_path() -> std::path::PathBuf` (`$XDG_CONFIG_HOME/tobii-linux/config.toml`, else `$HOME/.config/...`)
  - `save_to(path: &Path, setup: &DisplaySetup) -> std::io::Result<()>`
  - `load_from(path: &Path) -> std::io::Result<Option<DisplaySetup>>`
  - `save(setup: &DisplaySetup) -> std::io::Result<()>` / `load() -> std::io::Result<Option<DisplaySetup>>` (convenience over `config_path()`)

- [ ] **Step 1: Wire the module**

In `crates/tobii-config/src/lib.rs`, add `mod store;` after `mod setup;`, and extend the re-exports:

```rust
mod setup;
mod store;

pub use setup::DisplaySetup;
pub use store::{config_path, load, load_from, save, save_to};
```

- [ ] **Step 2: Write the failing test**

Create `crates/tobii-config/src/store.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DisplaySetup {
        DisplaySetup {
            width_mm: 800.0,
            height_mm: 335.0,
            tilt_deg: 20.0,
            offset_x_mm: 0.0,
            offset_y_mm: 40.0,
            offset_z_mm: -5.0,
        }
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = std::env::temp_dir().join("tobii-config-test-save-load");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");
        let s = sample();
        save_to(&path, &s).expect("save");
        let loaded = load_from(&path).expect("load io").expect("some");
        assert_eq!(loaded, s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_returns_none() {
        let path = std::env::temp_dir()
            .join("tobii-config-test-missing")
            .join("nope.toml");
        let _ = std::fs::remove_file(&path);
        assert!(load_from(&path).expect("io ok").is_none());
    }

    #[test]
    fn config_path_ends_with_expected_suffix() {
        let p = config_path();
        assert!(p.ends_with("tobii-linux/config.toml"));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-config --lib store::tests`
Expected: FAIL — `cannot find function save_to`.

- [ ] **Step 4: Implement the store**

PREPEND to `crates/tobii-config/src/store.rs` (above the tests):

```rust
//! Config-file persistence: `$XDG_CONFIG_HOME/tobii-linux/config.toml`.

use std::io;
use std::path::{Path, PathBuf};

use crate::DisplaySetup;

/// The default config file path: `$XDG_CONFIG_HOME/tobii-linux/config.toml`,
/// falling back to `$HOME/.config/tobii-linux/config.toml`.
pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".config")
        });
    base.join("tobii-linux").join("config.toml")
}

/// Write `setup` as TOML to `path`, creating parent directories as needed.
pub fn save_to(path: &Path, setup: &DisplaySetup) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, setup.to_toml())
}

/// Read a `DisplaySetup` from `path`. `Ok(None)` if the file does not exist or
/// does not parse.
pub fn load_from(path: &Path) -> io::Result<Option<DisplaySetup>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(DisplaySetup::from_toml(&s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Save to the default [`config_path`].
pub fn save(setup: &DisplaySetup) -> io::Result<()> {
    save_to(&config_path(), setup)
}

/// Load from the default [`config_path`].
pub fn load() -> io::Result<Option<DisplaySetup>> {
    load_from(&config_path())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-config`
Expected: PASS (all `tobii-config` tests: setup + store).

- [ ] **Step 6: Clippy + commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy -p tobii-config --all-targets -- -D warnings 2>&1 | tail -2
git add crates/tobii-config/src/lib.rs crates/tobii-config/src/store.rs
git commit -m "feat(config): TOML config store (XDG path, load/save)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: `tobii setup` + `tobii display get|set` (tobii-cli)

Wire the request/response path + config into the three CLI subcommands (`setup` interactive, `display get`, `display set`). Verified by build + clippy + a no-device `setup` smoke test (live behavior is Task 9).

**Files:**
- Modify: `crates/tobii-cli/Cargo.toml` (add `tobii-config` dep)
- Modify: `crates/tobii-cli/src/main.rs`

**Interfaces:**
- Consumes: `tobii_usb::{Connection, UsbTransport}`, `Connection::request` (Task 3), `tobii_protocol::commands::set_display_area_corners_payload` (Task 2), `tobii_protocol::frame::{OP_GET_DISPLAY_AREA, OP_SET_DISPLAY_AREA}`, `tobii_protocol::DisplayCorners`, `tobii_config::{DisplaySetup, load, config_path, save}`, `std::io::Write`.

- [ ] **Step 1: Add the dependency**

In `crates/tobii-cli/Cargo.toml`, under `[dependencies]`, add:

```toml
tobii-config = { path = "../tobii-config" }
```

- [ ] **Step 2: Replace `crates/tobii-cli/src/main.rs`**

Replace the whole file with (this keeps `stream` unchanged and adds the new commands + dispatch):

```rust
//! `tobii` CLI. Subcommands: `stream`, `setup`, `display get|set`.

use std::io::Write;
use std::process::ExitCode;

use tobii_config::DisplaySetup;
use tobii_protocol::commands::set_display_area_corners_payload;
use tobii_protocol::frame::{OP_GET_DISPLAY_AREA, OP_SET_DISPLAY_AREA};
use tobii_protocol::gaze::present;
use tobii_protocol::DisplayCorners;
use tobii_usb::{Connection, UsbTransport};

type CmdResult = Result<(), Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str);
    let arg2 = args.get(2).map(String::as_str);
    let result = match (sub, arg2) {
        (Some("stream"), _) => stream(args.iter().any(|a| a == "--json")),
        (Some("setup"), _) => setup(),
        (Some("display"), Some("get")) => display_get(),
        (Some("display"), Some("set")) => display_set(),
        _ => {
            eprintln!(
                "usage:\n  \
                 tobii stream [--json]\n  \
                 tobii setup\n  \
                 tobii display get\n  \
                 tobii display set"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn stream(json: bool) -> CmdResult {
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

fn print_corners(c: &DisplayCorners) {
    println!("  TL = ({:8.1}, {:8.1}, {:8.1})", c.tl[0], c.tl[1], c.tl[2]);
    println!("  TR = ({:8.1}, {:8.1}, {:8.1})", c.tr[0], c.tr[1], c.tr[2]);
    println!("  BL = ({:8.1}, {:8.1}, {:8.1})", c.bl[0], c.bl[1], c.bl[2]);
}

fn print_setup(s: &DisplaySetup) {
    println!(
        "  width={:.1}mm height={:.1}mm tilt={:.1}° offset=({:.1}, {:.1}, {:.1})mm",
        s.width_mm, s.height_mm, s.tilt_deg, s.offset_x_mm, s.offset_y_mm, s.offset_z_mm
    );
}

fn display_get() -> CmdResult {
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    match conn.request(OP_GET_DISPLAY_AREA, &[])? {
        Some(payload) => {
            let corners = DisplayCorners::decode(&payload)
                .ok_or("could not decode the display-area response")?;
            println!("display area (tracker-space mm):");
            print_corners(&corners);
            println!("derived setup:");
            print_setup(&DisplaySetup::from_corners(&corners));
            Ok(())
        }
        None => Err("no display-area response from device".into()),
    }
}

fn display_set() -> CmdResult {
    let setup = tobii_config::load()?.ok_or("no saved config — run `tobii setup` first")?;
    let c = setup.to_corners();
    let payload = set_display_area_corners_payload(
        c.tl[0], c.tl[1], c.tl[2], c.tr[0], c.tr[1], c.tr[2], c.bl[0], c.bl[1], c.bl[2],
    );
    let transport = UsbTransport::open()?;
    let mut conn = Connection::connect(transport)?;
    conn.request(OP_SET_DISPLAY_AREA, &payload)?;
    println!("display area applied to device:");
    print_corners(&c);
    Ok(())
}

fn prompt_f64(label: &str, default: f64) -> Result<f64, Box<dyn std::error::Error>> {
    print!("{label} [{default}]: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let t = line.trim();
    if t.is_empty() {
        Ok(default)
    } else {
        Ok(t.parse()?)
    }
}

fn setup() -> CmdResult {
    println!("Tobii display setup — enter your monitor geometry.");
    println!("(millimetres; tilt in degrees; press Enter to accept each default)\n");
    let s = DisplaySetup {
        width_mm: prompt_f64("Monitor active-area WIDTH (mm)", 600.0)?,
        height_mm: prompt_f64("Monitor active-area HEIGHT (mm)", 340.0)?,
        tilt_deg: prompt_f64("Screen tilt back from vertical (deg)", 20.0)?,
        offset_y_mm: prompt_f64("Height of screen BOTTOM edge above tracker (mm)", 10.0)?,
        offset_z_mm: prompt_f64("Depth of screen bottom from tracker (mm)", 0.0)?,
        offset_x_mm: prompt_f64("Horizontal offset of screen centre from tracker (mm)", 0.0)?,
    };
    let c = s.to_corners();
    println!("\ncomputed display-area corners (tracker-space mm):");
    print_corners(&c);

    let path = tobii_config::config_path();
    tobii_config::save(&s)?;
    println!("saved config to {}", path.display());

    match UsbTransport::open() {
        Ok(t) => {
            let mut conn = Connection::connect(t)?;
            let payload = set_display_area_corners_payload(
                c.tl[0], c.tl[1], c.tl[2], c.tr[0], c.tr[1], c.tr[2], c.bl[0], c.bl[1], c.bl[2],
            );
            conn.request(OP_SET_DISPLAY_AREA, &payload)?;
            println!("applied to the connected device.");
        }
        Err(e) => {
            eprintln!(
                "note: device not opened ({e}); config saved — run `tobii display set` when connected."
            );
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Build + clippy**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo build -p tobii-cli && cargo clippy -p tobii-cli --all-targets -- -D warnings 2>&1 | tail -3`
Expected: builds cleanly; clippy clean.

- [ ] **Step 4: Smoke-test `setup`'s non-device path (no hardware)**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; printf '800\n335\n20\n40\n0\n0\n' | XDG_CONFIG_HOME=/tmp/tobii-setup-smoke cargo run -q -p tobii-cli -- setup`
Expected: prints computed corners, `saved config to /tmp/tobii-setup-smoke/tobii-linux/config.toml`, then a `note: device not opened ...` line (no hardware). Verify: `cat /tmp/tobii-setup-smoke/tobii-linux/config.toml` shows the `[display]` section with the entered values. Clean up: `rm -rf /tmp/tobii-setup-smoke`.

- [ ] **Step 5: Commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt
git add crates/tobii-cli/Cargo.toml crates/tobii-cli/src/main.rs Cargo.lock
git commit -m "feat(cli): add tobii setup + display get|set

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: docs — README + spec status

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-06-14-tobii-et5-linux-driver-design.md` (§9 phasing status)

- [ ] **Step 1: Update the README**

In `README.md`, replace the `## Crates` list's `tobii-cli` bullet region and the `## Build & run` + `## Status` sections. Set `## Crates` to include:

```markdown
- `tobii-protocol` — pure protocol codec + handshake state machine (no I/O).
- `tobii-usb` — libusb (rusb) transport + connection driver.
- `tobii-config` — display-setup geometry + TOML config (no I/O beyond the config file).
- `tobii-cli` — the `tobii` command-line tool.
```

Replace `## Build & run` with:

```markdown
## Build & run

    cargo build --release
    ./target/release/tobii stream          # human-readable gaze
    ./target/release/tobii stream --json   # one JSON object per sample

### Display setup

Tell the tracker where your screen is (needed for accurate on-screen gaze):

    ./target/release/tobii setup           # interactive; writes config + applies
    ./target/release/tobii display get      # read the device's current area
    ./target/release/tobii display set      # re-apply the saved config

Config is stored at `$XDG_CONFIG_HOME/tobii-linux/config.toml` (default
`~/.config/tobii-linux/config.toml`). Inputs are your monitor's active-area
width/height (mm), how far its bottom edge sits above the tracker, the screen's
tilt angle, and any horizontal/depth offset — a planar model validated against a
real working configuration (see `docs/superpowers/specs/`).
```

Replace `## Status` with:

```markdown
## Status
v1 in progress: gaze streaming and display-area setup work; head-pose and
opentrack output are upcoming. See `docs/superpowers/`.
```

- [ ] **Step 2: Update the spec phasing note**

In `docs/superpowers/specs/2026-06-14-tobii-et5-linux-driver-design.md`, in §9, replace the `- **v1 (this spec):**` bullet with:

```markdown
- **v1 (this spec):** core driver (handshake, gaze stream ✅, display-area config
  ✅ via `tobii-config`/`tobii setup`) + `tobii-headpose` + opentrack output (next)
  → playable in Star Citizen and any head-tracking game.
```

- [ ] **Step 3: Commit**

```bash
git add README.md docs/superpowers/specs/2026-06-14-tobii-et5-linux-driver-design.md
git commit -m "docs: document tobii setup / display commands

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: LIVE smoke test (requires the ET5 plugged in)

Proves the display-area round-trip on real hardware and captures a real
`get_display_area` response as a regression golden vector. **Do not skip or
simulate.** Needs the physical Tobii Eye Tracker 5 connected.

**Files:**
- (Possibly) Modify: `crates/tobii-protocol/src/display.rs` (add a real-capture golden decode test)

- [ ] **Step 1: Confirm the device is present**

Run: `lsusb | grep -i 2104`
Expected: `2104:0313`. If absent, **ask the user to plug in the ET5** (and install the udev rule per the README) before continuing. If it shows `2104:0102` (bootloader), stop and report.

- [ ] **Step 2: Read the current display area**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo run --release -p tobii-cli -- display get`
Expected: prints three corners + a derived setup line. If it errors with `no display-area response`, capture stderr and **record whether the response op differs from `0x596`** (Connection matches RSP by op) — that would be a real-device finding to fix in `Connection::request`/`display_get`.

- [ ] **Step 3: Run interactive setup with your real monitor**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo run --release -p tobii-cli -- setup`
Enter your monitor's real active-area width/height, tilt, and offsets. Expected: computed corners printed, config saved, `applied to the connected device.`

- [ ] **Step 4: Verify the round-trip**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo run --release -p tobii-cli -- display get`
Expected: the corners now reflect what `setup` sent (within device rounding). Confirm `tobii stream` still produces sane 2D gaze that tracks where you look.

- [ ] **Step 5: Capture a real display-area response as a golden vector**

While a device is connected, capture one raw `get_display_area` response payload
(temporarily add `eprintln!("{:02x?}", payload)` in `display_get`'s `Some(payload)`
arm, run `display get`, copy the bytes, then revert the `eprintln!`). Add a test to
`crates/tobii-protocol/src/display.rs` `mod tests` that decodes the captured bytes
with `DisplayCorners::decode` and asserts the three corners are finite and the
implied width (`tr[0] - tl[0]`) is positive and within a sane range (say 100–2000 mm).
Commit:

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib display
git add crates/tobii-protocol/src/display.rs
git commit -m "test(protocol): add real captured display-area golden decode vector

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 6: Final verification**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test 2>&1 | grep -E "test result|Running"` and `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: all crates' tests pass; clippy clean.

---

## Roadmap (after this plan)

- **Plan 5 — `tobii-headpose` + opentrack** (Spike S1 pitch axis, Spike S2 opentrack UDP format; derive 6DOF from per-eye 3D positions; `tobii opentrack`; live Star Citizen test).
- **Phase 2 — per-user gaze calibration** (stimulus points, add-point, compute/apply, save/load blob).

## Plan self-review notes

- **Spec coverage:** `tobii-config` crate (display-setup math + TOML) → Tasks 4–6; `tobii setup` interactive + `tobii display get|set` → Task 7; request/response transport seam → Tasks 1–3; udev/README already exist, updated → Task 8; live validation → Task 9. Head-pose/opentrack and calibration remain out of scope (Plan 5 / Phase 2), per §9 phasing. The design decision "match the original *inputs*, equivalent validated math" is per the Spike S3 finding recorded in the spec §10.
- **Zero-dependency constraint:** `tobii-config` depends only on `tobii-protocol`; TOML is hand-rolled (Task 5). No `serde`/`toml`/`clap` added.
- **Type consistency:** `DisplaySetup` fields (`width_mm, height_mm, tilt_deg, offset_x_mm, offset_y_mm, offset_z_mm`) and methods (`to_corners`, `from_corners`, `to_toml`, `from_toml`) are used identically across Tasks 4–7. `Connection::request(op, &[u8]) -> Result<Option<Vec<u8>>, UsbError>` (Task 3) is consumed unchanged in Task 7. `set_display_area_corners_payload` (Task 2, 9 f64 args) and the ops `OP_GET_DISPLAY_AREA`/`OP_SET_DISPLAY_AREA` (existing in `frame.rs`) are used consistently. `DisplayCorners { tl, tr, bl: [f64;3] }` is the shared `tobii-protocol` type throughout.
- **Placeholder scan:** every code step is complete — no stubs. The only "fill-in" is the real captured bytes in Task 9 Step 5, intrinsic to a hardware-capture step.
- **Hardware boundary:** Tasks 1–8 are fully verifiable on the dev machine (unit tests + build + a no-device `setup` smoke test); only Task 9 needs the ET5.
