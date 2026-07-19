# Tobii GUI (B2) — Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the `tobii-gui` app foundation — a persistent hub window that shows live connection/eye-position status and launches (stub) flows — on top of a device thread and a new gaze-column decode.

**Architecture:** New `tobii-gui` binary crate (egui/eframe). A dedicated **device thread** owns the blocking `Connection` (handshake, gaze stream, command channel) and publishes a `DeviceState` snapshot; the egui UI thread renders the hub from that snapshot and sends commands. Pure logic (gaze-column decode, eye-position mapping, the device tick) is extracted and unit-tested; egui drawing is thin and build/clippy/live-verified.

**Tech Stack:** Rust (edition 2021), `eframe`/`egui` 0.28, `cargo test`/`clippy`. Builds on `tobii-protocol`, `tobii-usb`, `tobii-config` (all on `master`). Reference: `docs/superpowers/specs/2026-07-19-tobii-gui-b2-design.md`. GPL-3.0.

**Scope note:** This is B2 **phase 1 of 2**. It delivers the backend decode + crate + device thread + hub. The **display-setup** and **eye-position fullscreen flows** are **Plan B2.2** (the hub's launch buttons are stubs here). Calibration flow = B3; head-pose/opentrack = Plan 5.

## Global Constraints

- Rust **edition 2021**, license **GPL-3.0-only** (inherited via `.workspace`).
- **`tobii-gui` is the only crate allowed external GUI deps** (`eframe`/`egui`). The protocol/usb/config crates stay as they are (no new deps there). `eframe = "0.28"`.
- All `cargo` commands: prefix `export PATH="$HOME/.cargo/bin:$PATH"`.
- Every task ends **rustfmt-clean** (`cargo fmt`) and **clippy-clean** for the crates it touches (`cargo clippy -p <crate> --all-targets -- -D warnings`).
- **Gaze trackbox columns:** `0x03` = left, `0x09` = right, each a Q42 `point3d` (normalized x/y in the trackbox `[0,1]`, z = distance). `present_mask` bits are the crate's OWN internal flags (not wire values) — add new bits freely.
- egui draw code is thin over pure, tested logic. GUI tasks verify by build + clippy; live behaviour is the final task.
- Commit messages end with:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

## File Structure

- `crates/tobii-protocol/src/gaze.rs` — add trackbox fields + present flags + decode arms (modify).
- `crates/tobii-gui/Cargo.toml` — **new** (eframe dep; lib + bin).
- `crates/tobii-gui/src/lib.rs` — **new** (the app + module declarations; `pub fn run`).
- `crates/tobii-gui/src/main.rs` — **new** (thin binary entry → `tobii_gui::run()`).
- `crates/tobii-gui/src/eyeview.rs` — **new** (pure eye-position mapping).
- `crates/tobii-gui/src/device.rs` — **new** (device thread + state/commands + testable tick).
- `crates/tobii-gui/src/hub.rs` — **new** (the hub egui screen + trackbox widget).
- `Cargo.toml` — workspace members (modify).

---

### Task 1: decode trackbox eye-position columns 0x03/0x09 (tobii-protocol)

**Files:**
- Modify: `crates/tobii-protocol/src/gaze.rs`

**Interfaces:**
- Produces on `GazeSample`: `pub trackbox_eye_l: [f64; 3]`, `pub trackbox_eye_r: [f64; 3]`; `present::{TRACKBOX_L, TRACKBOX_R}`. `0x03`→`trackbox_eye_l`+`TRACKBOX_L`; `0x09`→`trackbox_eye_r`+`TRACKBOX_R`.

- [ ] **Step 1: Write the failing test**

In `crates/tobii-protocol/src/gaze.rs` `mod tests`, add (mirrors the existing synthetic-frame tests; `TAG_XDS_COLUMN`/`write_tag`/`write_u32`/`write_f64_q42`/`write_point`/`Writer` are already used by neighbouring tests):

```rust
    #[test]
    fn decodes_trackbox_eye_positions() {
        use crate::bytes::Writer;
        use crate::tlv::{write_f64_q42, write_point, write_tag, write_u32, TAG_XDS_COLUMN};
        let mut w = Writer::new();
        w.push_u8(0x00);
        w.push_u8(0x00);
        write_tag(&mut w, (2u32 << 16) | 0x0bb8); // xds_row, 2 columns
        // col 0x03 = trackbox left (point3d)
        write_tag(&mut w, TAG_XDS_COLUMN);
        write_u32(&mut w, 0x03);
        write_point(&mut w, 0.25, 0.75, 500.0);
        // col 0x09 = trackbox right (point3d)
        write_tag(&mut w, TAG_XDS_COLUMN);
        write_u32(&mut w, 0x09);
        write_point(&mut w, 0.30, 0.70, 510.0);

        let s = GazeSample::decode(&w.into_vec()).expect("decode");
        assert!(s.has(present::TRACKBOX_L) && s.has(present::TRACKBOX_R));
        assert!((s.trackbox_eye_l[0] - 0.25).abs() < 1e-9);
        assert!((s.trackbox_eye_l[2] - 500.0).abs() < 1e-6);
        assert!((s.trackbox_eye_r[1] - 0.70).abs() < 1e-9);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib gaze::tests::decodes_trackbox_eye_positions`
Expected: FAIL — `no field trackbox_eye_l` / `no associated item TRACKBOX_L`.

- [ ] **Step 3: Add present flags**

In `crates/tobii-protocol/src/gaze.rs`, in the `pub mod present` block, after `GAZE_3D_R` (the `1 << 15` line), add:

```rust
    pub const TRACKBOX_L: u32 = 1 << 16;
    pub const TRACKBOX_R: u32 = 1 << 17;
```

- [ ] **Step 4: Add the struct fields**

In `struct GazeSample`, after `pub gaze_point_3d_r_mm: [f64; 3],`, add:

```rust
    /// Left/right eye position normalized in the trackbox ([0,1] x/y; z = distance mm).
    pub trackbox_eye_l: [f64; 3],
    pub trackbox_eye_r: [f64; 3],
```

- [ ] **Step 5: Add the decode arms**

In `GazeSample::decode`, in the `match col` block, add these arms next to the `0x02`/`0x08` arms (they use the existing `set3` helper):

```rust
                0x03 => {
                    if !set3(
                        &mut r,
                        &mut s.trackbox_eye_l,
                        &mut s.present_mask,
                        present::TRACKBOX_L,
                    ) {
                        return Some(s);
                    }
                }
                0x09 => {
                    if !set3(
                        &mut r,
                        &mut s.trackbox_eye_r,
                        &mut s.present_mask,
                        present::TRACKBOX_R,
                    ) {
                        return Some(s);
                    }
                }
```

(Cols `0x03`/`0x09` were previously swallowed by the `other => column_kind(...)` skip arm — now they are stored.)

- [ ] **Step 6: Run tests + clippy**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib gaze:: && cargo clippy -p tobii-protocol --all-targets -- -D warnings 2>&1 | tail -2`
Expected: the new test + all existing gaze tests pass; clippy clean.

- [ ] **Step 7: Commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt
git add crates/tobii-protocol/src/gaze.rs
git commit -m "feat(protocol): decode trackbox eye-position columns 0x03/0x09

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: scaffold `tobii-gui` crate + minimal eframe window

**Files:**
- Modify: `Cargo.toml` (workspace members)
- Create: `crates/tobii-gui/Cargo.toml`
- Create: `crates/tobii-gui/src/main.rs`

**Interfaces:**
- Produces: a runnable `tobii-gui` binary that opens a window. Later tasks add `mod eyeview; mod device; mod hub;`.

- [ ] **Step 1: Add to the workspace**

In the root `Cargo.toml` `members`, append `"crates/tobii-gui"`.

- [ ] **Step 2: Create `crates/tobii-gui/Cargo.toml`**

```toml
[package]
name = "tobii-gui"
version = "0.1.0"
edition.workspace = true
license.workspace = true
description = "Graphical config hub for the Tobii ET5 Linux runtime (egui/eframe)."

[lib]
name = "tobii_gui"
path = "src/lib.rs"

[[bin]]
name = "tobii-gui"
path = "src/main.rs"

[dependencies]
tobii-protocol = { path = "../tobii-protocol" }
tobii-usb = { path = "../tobii-usb" }
tobii-config = { path = "../tobii-config" }
eframe = "0.28"
```

> **Why lib + bin:** the modules below (`eyeview`, `device`, `hub`) expose `pub`
> items used by unit tests before the binary's own code uses them. In a bin-only
> crate those read as `dead_code` and fail `clippy -D warnings`; in a **lib** they
> are the crate's API and are never flagged. `main.rs` stays a thin entry that
> calls `tobii_gui::run()`.

- [ ] **Step 3: Create `crates/tobii-gui/src/lib.rs`** (the app; modules are added by later tasks)

```rust
//! `tobii-gui` — graphical configuration hub for the Tobii ET5.
//!
//! A persistent hub window (status + live eye position + flow launchers) over a
//! device thread that owns the blocking USB connection. (Flows are added in B2.2.)
//! `main.rs` is a thin entry point; the app + modules live here so their `pub`
//! items are library API rather than dead code in a binary.

/// Run the app: open the window and start the egui event loop.
pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([720.0, 480.0])
            .with_title("Tobii Configuration"),
        ..Default::default()
    };
    eframe::run_native(
        "tobii-gui",
        options,
        Box::new(|_cc| Ok(Box::<TobiiApp>::default())),
    )
}

#[derive(Default)]
struct TobiiApp {}

impl eframe::App for TobiiApp {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        eframe::egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Tobii Configuration");
            ui.label("(foundation — hub UI arrives in a later task)");
        });
    }
}
```

- [ ] **Step 3b: Create the thin `crates/tobii-gui/src/main.rs`**

```rust
//! Binary entry point — see the `tobii_gui` library crate for the app.

fn main() -> eframe::Result<()> {
    tobii_gui::run()
}
```

- [ ] **Step 4: Build (downloads eframe; needs a display only to RUN, not to build)**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo build -p tobii-gui 2>&1 | tail -5`
Expected: eframe + egui compile and the crate builds. **If the `run_native` closure signature differs in the resolved `eframe` 0.28.x** (the creator closure must return `Result<Box<dyn App>, Box<dyn Error + Send + Sync>>`), reconcile against `cargo doc -p eframe --open` / the version's changelog — this is the one spot where the exact eframe API matters. Do not proceed until it builds.

- [ ] **Step 5: Clippy + commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy -p tobii-gui --all-targets -- -D warnings 2>&1 | tail -2
git add Cargo.toml Cargo.lock crates/tobii-gui
git commit -m "feat(gui): scaffold tobii-gui crate + minimal eframe window

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: eye-position mapping (pure, tested) — `eyeview.rs`

The pure logic the trackbox widget draws: a `GazeSample` → renderable eye positions + guidance. No egui here.

**Files:**
- Create: `crates/tobii-gui/src/eyeview.rs`
- Modify: `crates/tobii-gui/src/main.rs` (add `mod eyeview;`)

**Interfaces:**
- Consumes: `tobii_protocol::{GazeSample, gaze::present}`.
- Produces:
  - `eyeview::EyeView { pub left: Option<[f32; 2]>, pub right: Option<[f32; 2]>, pub distance_mm: Option<f32>, pub guidance: Guidance }`
  - `eyeview::Guidance { NoEyes, MoveCloser, MoveBack, Centered, OffCenter }`
  - `eyeview::EyeView::from_gaze(s: &GazeSample) -> EyeView`

- [ ] **Step 1: Write the failing tests**

Create `crates/tobii-gui/src/eyeview.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tobii_protocol::gaze::present;
    use tobii_protocol::GazeSample;

    fn sample_with_trackbox(l: [f64; 3], r: [f64; 3], valid: bool) -> GazeSample {
        let mut s = GazeSample::default();
        s.trackbox_eye_l = l;
        s.trackbox_eye_r = r;
        s.present_mask |= present::TRACKBOX_L | present::TRACKBOX_R;
        s.present_mask |= present::VALIDITY_L | present::VALIDITY_R;
        let v = if valid { 0 } else { 4 };
        s.validity_l = v;
        s.validity_r = v;
        s
    }

    #[test]
    fn no_trackbox_columns_means_no_eyes() {
        let v = EyeView::from_gaze(&GazeSample::default());
        assert!(matches!(v.guidance, Guidance::NoEyes));
        assert!(v.left.is_none() && v.right.is_none());
    }

    #[test]
    fn centered_eyes_map_to_box_and_report_centered() {
        let v = EyeView::from_gaze(&sample_with_trackbox([0.45, 0.5, 550.0], [0.55, 0.5, 550.0], true));
        assert!(v.left.is_some() && v.right.is_some());
        // x normalized [0,1] -> passed through as f32 for the widget to scale.
        assert!((v.left.unwrap()[0] - 0.45).abs() < 1e-6);
        assert!(matches!(v.guidance, Guidance::Centered));
        assert!((v.distance_mm.unwrap() - 550.0).abs() < 1e-3);
    }

    #[test]
    fn too_close_and_too_far_are_flagged() {
        let close = EyeView::from_gaze(&sample_with_trackbox([0.5, 0.5, 300.0], [0.5, 0.5, 300.0], true));
        assert!(matches!(close.guidance, Guidance::MoveBack));
        let far = EyeView::from_gaze(&sample_with_trackbox([0.5, 0.5, 900.0], [0.5, 0.5, 900.0], true));
        assert!(matches!(far.guidance, Guidance::MoveCloser));
    }

    #[test]
    fn off_center_eyes_are_flagged() {
        let v = EyeView::from_gaze(&sample_with_trackbox([0.1, 0.5, 550.0], [0.2, 0.5, 550.0], true));
        assert!(matches!(v.guidance, Guidance::OffCenter));
    }

    #[test]
    fn invalid_validity_means_no_eyes() {
        let v = EyeView::from_gaze(&sample_with_trackbox([0.5, 0.5, 550.0], [0.5, 0.5, 550.0], false));
        assert!(matches!(v.guidance, Guidance::NoEyes));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-gui --lib eyeview::tests`
Expected: FAIL — `cannot find type EyeView`.

- [ ] **Step 3: Write the implementation**

PREPEND to `crates/tobii-gui/src/eyeview.rs`:

```rust
//! Pure mapping from a decoded gaze sample to a renderable eye-position view.
//! No egui — the widget (hub/flows) draws from this.

use tobii_protocol::gaze::present;
use tobii_protocol::GazeSample;

/// Comfortable operating-distance window (mm) and centre tolerance for guidance.
const DIST_MIN_MM: f32 = 450.0;
const DIST_MAX_MM: f32 = 750.0;
const CENTRE_TOL: f32 = 0.18; // max |mid - 0.5| on each axis to count as centred

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Guidance {
    NoEyes,
    MoveCloser,
    MoveBack,
    Centered,
    OffCenter,
}

/// A renderable eye-position snapshot. `left`/`right` are normalized `[0,1]`
/// trackbox coordinates (the widget scales them into its rectangle).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EyeView {
    pub left: Option<[f32; 2]>,
    pub right: Option<[f32; 2]>,
    pub distance_mm: Option<f32>,
    pub guidance: Guidance,
}

impl EyeView {
    pub fn from_gaze(s: &GazeSample) -> EyeView {
        let eyes_valid = s.has(present::TRACKBOX_L)
            && s.has(present::TRACKBOX_R)
            && s.validity_l == 0
            && s.validity_r == 0;
        if !eyes_valid {
            return EyeView { left: None, right: None, distance_mm: None, guidance: Guidance::NoEyes };
        }
        let left = [s.trackbox_eye_l[0] as f32, s.trackbox_eye_l[1] as f32];
        let right = [s.trackbox_eye_r[0] as f32, s.trackbox_eye_r[1] as f32];
        let distance = ((s.trackbox_eye_l[2] + s.trackbox_eye_r[2]) / 2.0) as f32;
        let mid_x = (left[0] + right[0]) / 2.0;
        let mid_y = (left[1] + right[1]) / 2.0;

        let guidance = if distance < DIST_MIN_MM {
            Guidance::MoveBack
        } else if distance > DIST_MAX_MM {
            Guidance::MoveCloser
        } else if (mid_x - 0.5).abs() > CENTRE_TOL || (mid_y - 0.5).abs() > CENTRE_TOL {
            Guidance::OffCenter
        } else {
            Guidance::Centered
        };

        EyeView { left: Some(left), right: Some(right), distance_mm: Some(distance), guidance }
    }
}
```

Add `pub mod eyeview;` to `crates/tobii-gui/src/lib.rs` (below the doc comment, above `pub fn run`).

- [ ] **Step 4: Run tests + clippy**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-gui --lib eyeview::tests && cargo clippy -p tobii-gui --all-targets -- -D warnings 2>&1 | tail -2`
Expected: 5 tests pass; clippy clean. (`EyeView`/`Guidance` are used by tests now and by the hub in Task 5 — no dead-code warning under `--all-targets`.)

- [ ] **Step 5: Commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt
git add crates/tobii-gui/src/eyeview.rs crates/tobii-gui/src/lib.rs
git commit -m "feat(gui): pure eye-position mapping (trackbox -> renderable + guidance)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: device thread + shared state/commands — `device.rs`

Owns the blocking `Connection` on its own thread; publishes a `DeviceState` snapshot and consumes `DeviceCommand`s. The per-iteration logic (`device_tick`) is generic over `Transport` and unit-tested with `MockTransport`.

**Files:**
- Create: `crates/tobii-gui/src/device.rs`
- Modify: `crates/tobii-gui/src/main.rs` (add `mod device;`)

**Interfaces:**
- Consumes: `tobii_usb::{Connection, UsbTransport, Transport}`, `tobii_protocol::{GazeSample, commands::set_display_area_corners_payload, frame::OP_SET_DISPLAY_AREA, DisplayCorners}`.
- Produces:
  - `device::ConnStatus { Connecting, Connected, Error(String) }`
  - `device::DeviceState { pub status: ConnStatus, pub latest_gaze: Option<GazeSample> }`
  - `device::DeviceCommand::SetDisplayArea(DisplayCorners)`
  - `device::device_tick<T: Transport>(conn: &mut Connection<T>, state: &Mutex<DeviceState>, cmd_rx: &Receiver<DeviceCommand>)` — one iteration.
  - `device::spawn() -> (Arc<Mutex<DeviceState>>, Sender<DeviceCommand>)`

- [ ] **Step 1: Write the failing tests**

Create `crates/tobii-gui/src/device.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::mpsc::channel;
    use std::sync::Mutex;
    use std::time::Duration;
    use tobii_protocol::frame::{
        ENVELOPE_SIZE, TTP_HDR_SIZE, TTP_MAGIC_NOTIFY, TTP_MAGIC_RSP,
    };
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
        Connection::connect(MockTransport { sent: Vec::new(), to_recv }).expect("connect")
    }

    #[test]
    fn tick_publishes_latest_gaze() {
        let mut conn = connected(vec![inbound(TTP_MAGIC_NOTIFY, 0, 0x500, &gaze_payload())]);
        let state = Mutex::new(DeviceState::default());
        let (_tx, rx) = channel::<DeviceCommand>();
        device_tick(&mut conn, &state, &rx);
        let g = state.lock().unwrap().latest_gaze.clone().expect("gaze published");
        assert_eq!(g.timestamp_us, 42);
    }

    #[test]
    fn tick_applies_a_set_display_area_command() {
        let mut conn = connected(vec![inbound(TTP_MAGIC_RSP, 5, 0x5a0, &[])]);
        let state = Mutex::new(DeviceState::default());
        let (tx, rx) = channel::<DeviceCommand>();
        tx.send(DeviceCommand::SetDisplayArea(tobii_protocol::DisplayCorners {
            tl: [-1.0, 1.0, 0.0],
            tr: [1.0, 1.0, 0.0],
            bl: [-1.0, -1.0, 0.0],
        }))
        .unwrap();
        device_tick(&mut conn, &state, &rx);
        // A SET_DISPLAY_AREA (op 0x5a0) frame was sent (5th send after 4 handshake sends).
        assert_eq!(&conn.transport().sent.last().unwrap()[20..24], &[0, 0, 0x05, 0xa0]);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-gui --lib device::tests`
Expected: FAIL — `cannot find type DeviceState`.

- [ ] **Step 3: Write the implementation**

PREPEND to `crates/tobii-gui/src/device.rs`:

```rust
//! The device thread: owns the blocking `Connection`, publishes a `DeviceState`
//! snapshot for the UI, and applies `DeviceCommand`s. `device_tick` (one
//! iteration) is generic over `Transport` so it is unit-tested without hardware.

use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tobii_protocol::commands::set_display_area_corners_payload;
use tobii_protocol::frame::OP_SET_DISPLAY_AREA;
use tobii_protocol::{DisplayCorners, GazeSample};
use tobii_usb::{Connection, Transport, UsbError, UsbTransport};

#[derive(Debug, Clone, PartialEq, Default)]
pub enum ConnStatus {
    #[default]
    Connecting,
    Connected,
    Error(String),
}

#[derive(Debug, Clone, Default)]
pub struct DeviceState {
    pub status: ConnStatus,
    pub latest_gaze: Option<GazeSample>,
}

pub enum DeviceCommand {
    SetDisplayArea(DisplayCorners),
}

/// One iteration: apply any queued commands, then poll one gaze sample.
pub fn device_tick<T: Transport>(
    conn: &mut Connection<T>,
    state: &Mutex<DeviceState>,
    cmd_rx: &Receiver<DeviceCommand>,
) {
    loop {
        match cmd_rx.try_recv() {
            Ok(DeviceCommand::SetDisplayArea(c)) => {
                let payload = set_display_area_corners_payload(
                    c.tl[0], c.tl[1], c.tl[2], c.tr[0], c.tr[1], c.tr[2], c.bl[0], c.bl[1], c.bl[2],
                );
                let _ = conn.request(OP_SET_DISPLAY_AREA, &payload);
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
        }
    }
    if let Some(g) = conn.next_gaze() {
        let mut s = state.lock().unwrap();
        s.latest_gaze = Some(g);
        s.status = ConnStatus::Connected;
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
                thread_state.lock().unwrap().status = ConnStatus::Connected;
                loop {
                    device_tick(&mut conn, &thread_state, &rx);
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
```

Add `pub mod device;` to `crates/tobii-gui/src/lib.rs`.

> **Note:** `device_tick` never terminates the inner `spawn` loop on its own (gaze polling has its own read timeout); a lost device surfaces only when a `request`/read errors. That is acceptable for the foundation; a future task can add explicit disconnect detection.

- [ ] **Step 4: Run tests + clippy**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-gui --lib device::tests && cargo clippy -p tobii-gui --all-targets -- -D warnings 2>&1 | tail -2`
Expected: 2 tests pass; clippy clean.

- [ ] **Step 5: Commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt
git add crates/tobii-gui/src/device.rs crates/tobii-gui/src/lib.rs
git commit -m "feat(gui): device thread + shared state/commands (tick mock-tested)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: hub window — `hub.rs`

The persistent hub: connection status, launch buttons (flows are stubs for B2.2), and a live trackbox mini-view drawn from `EyeView`. Build/clippy-verified; live behaviour is Task 6.

**Files:**
- Create: `crates/tobii-gui/src/hub.rs`
- Modify: `crates/tobii-gui/src/main.rs` (wire the device thread + render the hub)

**Interfaces:**
- Consumes: `crate::device::{spawn, DeviceState, ConnStatus}`, `crate::eyeview::{EyeView, Guidance}`, `eframe::egui`.
- Produces: `hub::draw(ui: &mut egui::Ui, state: &DeviceState)`.

- [ ] **Step 1: Write `hub.rs`**

Create `crates/tobii-gui/src/hub.rs`:

```rust
//! The persistent hub screen: status, flow launchers (stubs in the foundation),
//! and a live trackbox mini-view.

use eframe::egui;

use crate::device::{ConnStatus, DeviceState};
use crate::eyeview::{EyeView, Guidance};

pub fn draw(ui: &mut egui::Ui, state: &DeviceState) {
    ui.heading("Tobii Configuration");
    ui.add_space(6.0);

    match &state.status {
        ConnStatus::Connecting => {
            ui.colored_label(egui::Color32::YELLOW, "Connecting to the eye tracker…");
        }
        ConnStatus::Connected => {
            ui.colored_label(egui::Color32::GREEN, "Eye tracker connected");
        }
        ConnStatus::Error(e) => {
            ui.colored_label(egui::Color32::LIGHT_RED, format!("Not connected: {e}"));
        }
    }
    ui.add_space(10.0);

    ui.horizontal(|ui| {
        // Flows arrive in B2.2 — buttons are placeholders here.
        let _ = ui.button("Set up display…");
        let _ = ui.button("Position eyes…");
        ui.add_enabled(false, egui::Button::new("Calibrate… (B3)"));
    });
    ui.add_space(12.0);

    ui.label("Eye position:");
    let view = state
        .latest_gaze
        .as_ref()
        .map(EyeView::from_gaze)
        .unwrap_or(EyeView { left: None, right: None, distance_mm: None, guidance: Guidance::NoEyes });
    draw_trackbox(ui, &view, egui::vec2(320.0, 200.0));

    let msg = match view.guidance {
        Guidance::NoEyes => "No eyes detected — sit in front of the tracker.".to_string(),
        Guidance::MoveCloser => "Move a little closer.".to_string(),
        Guidance::MoveBack => "Move back a little.".to_string(),
        Guidance::OffCenter => "Center yourself in front of the screen.".to_string(),
        Guidance::Centered => match view.distance_mm {
            Some(d) => format!("Good position ({d:.0} mm)."),
            None => "Good position.".to_string(),
        },
    };
    ui.label(msg);
}

/// Draw the trackbox rectangle with the two eyes at their normalized positions.
fn draw_trackbox(ui: &mut egui::Ui, view: &EyeView, size: egui::Vec2) {
    let (resp, painter) = ui.allocate_painter(size, egui::Sense::hover());
    let rect = resp.rect;
    painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.5, egui::Color32::GRAY));
    let plot = |p: [f32; 2]| egui::pos2(rect.left() + p[0] * rect.width(), rect.top() + p[1] * rect.height());
    let color = if matches!(view.guidance, Guidance::Centered) {
        egui::Color32::GREEN
    } else {
        egui::Color32::YELLOW
    };
    for eye in [view.left, view.right].into_iter().flatten() {
        painter.circle_filled(plot(eye), 8.0, color);
    }
}
```

- [ ] **Step 2: Wire the hub into `lib.rs`**

In `crates/tobii-gui/src/lib.rs`: add `pub mod hub;` alongside the existing `pub mod eyeview;` / `pub mod device;`, and replace the placeholder `run`/`TobiiApp` with the real wiring:

```rust
/// Run the app: open the window, start the device thread, render the hub.
pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([720.0, 480.0])
            .with_title("Tobii Configuration"),
        ..Default::default()
    };
    eframe::run_native("tobii-gui", options, Box::new(|_cc| Ok(Box::new(TobiiApp::new()))))
}

struct TobiiApp {
    state: std::sync::Arc<std::sync::Mutex<device::DeviceState>>,
    _cmd_tx: std::sync::mpsc::Sender<device::DeviceCommand>,
}

impl TobiiApp {
    fn new() -> Self {
        let (state, cmd_tx) = device::spawn();
        Self { state, _cmd_tx: cmd_tx }
    }
}

impl eframe::App for TobiiApp {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        let snapshot = self.state.lock().unwrap().clone();
        eframe::egui::CentralPanel::default().show(ctx, |ui| hub::draw(ui, &snapshot));
        ctx.request_repaint_after(std::time::Duration::from_millis(33)); // ~30fps live view
    }
}
```

`main.rs` stays the thin `fn main() -> eframe::Result<()> { tobii_gui::run() }` from Task 2 — no change. `hub.rs` refers to sibling modules via `crate::device` / `crate::eyeview` (they resolve because both are `pub mod` in the lib crate root).

- [ ] **Step 3: Build + clippy**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo build -p tobii-gui && cargo clippy -p tobii-gui --all-targets -- -D warnings 2>&1 | tail -3`
Expected: builds cleanly; clippy clean. (Reconcile any egui 0.28.x painter/label API differences against `cargo doc -p egui` if a method signature differs — e.g. `rect_stroke` may take a `StrokeKind` arg in some 0.28.x; adjust minimally.)

- [ ] **Step 4: Commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt
git add crates/tobii-gui/src/hub.rs crates/tobii-gui/src/lib.rs
git commit -m "feat(gui): hub window — status, flow launchers, live trackbox mini-view

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: LIVE validation (requires the ET5 + a display)

Runs the hub on real hardware, confirms the live eye view, and captures a real trackbox frame as a golden decode vector. Needs the ET5 (`2104:0313`) connected + the udev rule, and a graphical session.

**Files:**
- (Possibly) Modify: `crates/tobii-protocol/src/gaze.rs` (add a real-capture trackbox golden test)

- [ ] **Step 1: Confirm device present**

Run: `lsusb | grep -i 2104` → expect `2104:0313`. If absent, ask the user to connect it.

- [ ] **Step 2: Run the hub**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo run --release -p tobii-gui`
Expected: a window titled "Tobii Configuration" showing "Eye tracker connected", and — when you sit in front of the tracker — two dots moving in the trackbox with live guidance ("Good position (…mm)" / "Move closer" / "Center yourself"). If it stays "No eyes detected" with your eyes clearly in view, capture a raw `0x500` payload (Step 3) and check whether cols `0x03`/`0x09` are actually present on this firmware (they may be gated differently — record the finding in the design spec §9).

- [ ] **Step 3: Capture a real trackbox frame as a golden vector**

With eyes tracked, capture one raw `0x500` payload that contains cols `0x03`/`0x09` (temporarily write `f.payload` to `/tmp/tb.bin` in `Connection::route`'s gaze branch, run the hub briefly, then revert). Add a test to `crates/tobii-protocol/src/gaze.rs` `mod tests` that `include_bytes!`-loads it, decodes it, and asserts `has(present::TRACKBOX_L)` / `has(present::TRACKBOX_R)` and that both eyes' x/y are within `[0,1]` and z within a sane distance range (say 200–1200 mm). Commit:

```bash
export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p tobii-protocol --lib gaze
git add crates/tobii-protocol/src/gaze.rs crates/tobii-protocol/src/testdata/*.bin docs/superpowers/specs/2026-07-19-tobii-gui-b2-design.md
git commit -m "test(protocol): real trackbox gaze frame golden vector; record live finding

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 4: Final verification**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test --workspace 2>&1 | grep -E "test result: ok"` and `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -2`
Expected: all tests pass; clippy clean.

---

## Roadmap (after this Foundation)

- **Plan B2.2 — flows:** the fullscreen **display-setup** flow (detect → geometry → apply → trackbox confirm; reuses Plan 4 math) and the **eye-position** flow; wire the hub's launch buttons to them.
- **B3** — the follow-the-dot calibration flow (backend already on `master`).
- **Plan 5** — `tobii-headpose` + opentrack (Star Citizen), independent.

## Plan self-review notes

- **Spec coverage:** gaze `0x03/0x09` decode (spec §4.4 backend) → Task 1; `tobii-gui` crate + egui (spec §3) → Task 2; eye-position mapping (spec §4.4) → Task 3; device thread + state/commands (spec §3/§5) → Task 4; hub window w/ status + launchers + mini-view (spec §4.1) → Task 5; live validation + real golden vector (spec §7/§9/§10) → Task 6. The **fullscreen flows** (spec §4.2/§4.3) are explicitly deferred to Plan B2.2 — the hub launch buttons are stubs. The §10-Q5 "present-mask discrepancy" is resolved by noting present bits are internal (Global Constraints), so no wire reconciliation is needed.
- **Dependency constraint:** only `tobii-gui` gains `eframe`/`egui`; protocol/usb/config crates are untouched dependency-wise (Task 1 modifies gaze.rs code only, no new deps).
- **Type consistency:** `GazeSample.trackbox_eye_l/r` + `present::TRACKBOX_L/R` (Task 1) are consumed by `EyeView::from_gaze` (Task 3), which the hub (Task 5) renders. `DeviceState`/`ConnStatus`/`DeviceCommand`/`device_tick`/`spawn` (Task 4) are consumed by `main.rs` + `hub.rs` (Task 5). `EyeView`/`Guidance` (Task 3) used by `hub.rs` (Task 5). `set_display_area_corners_payload` + `OP_SET_DISPLAY_AREA` + `DisplayCorners` (existing) used in Task 4.
- **GUI-API caveat (called out honestly):** the exact `eframe`/`egui` 0.28.x API for `run_native`'s creator closure and a couple of painter/label calls may need minor reconciliation against the resolved patch version — flagged at Tasks 2 Step 4 and 5 Step 3 with the reconciliation method. All non-GUI logic (Tasks 1, 3, 4) is exact and TDD'd.
- **Hardware boundary:** Tasks 1, 3, 4 are fully unit-tested without hardware; Tasks 2, 5 are build/clippy-verified; only Task 6 needs the ET5 + a display.
