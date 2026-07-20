# B3 — Follow-the-dot Calibration Flow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a guided follow-the-dot gaze calibration to the GTK4 app, saved and re-applied on every connect, unblocking the hub's "Improve my calibration" button.

**Architecture:** Calibration runs on the device thread (it owns the blocking `Connection`); the UI drives pacing and polls a new `CalPhase` on `DeviceState`. A new fullscreen `calibrate_flow.rs` mirrors `setup_flow.rs`.

**Tech Stack:** Rust, gtk4-rs 0.11 (feature v4_12), cairo, existing `tobii-protocol`/`tobii-usb`/`tobii-config` crates.

## Global Constraints

- GPL-3.0-only; edition 2021; follow existing crate patterns.
- Protocol ops are fixed and code-verified: start `0x3f2`, stop `0x3fc`, clear `0x424`, add_point `0x408` (eye arg `0`=both), compute `0x42f`, retrieve `0x44c`, apply `0x456`. **These are already implemented in `tobii-protocol`/`tobii-usb` (spike commit `9588212`) — do NOT re-add them.**
- Managed sequence: `start → clear → per point{ settle, add_point } → compute → stop → retrieve → save`. Compute BEFORE stop; retrieve AFTER stop.
- Point sets (normalized, top-left origin, center-first), verbatim:
  - Quick (5): `(.5,.5) (.1,.9) (.5,.1) (.9,.9) (.5,.5)`
  - Full (9): `(.5,.5) (.1,.9) (.5,.1) (.9,.9) (.1,.1) (.5,.9) (.9,.1) (.1,.5) (.9,.5)`
- `add_point` **blocks** on the device thread until the device gathers enough samples — one call per point, no host sample loop.
- Select-eyes experiment: `CalBegin` SETs the saved `enabled_eye` before `start`, then calibrates both eyes (eye arg `0`). The per-eye path (`0x42e`) is OUT OF SCOPE.
- Out of scope (YAGNI): per-point quality (`0x460`).
- Spec: `docs/superpowers/specs/2026-07-20-b3-calibration-flow-design.md`.

---

## File Structure

- `crates/tobii-gtk/src/device.rs` (modify) — `CalPhase` + transitions, `DeviceState.calibration`, 4 `DeviceCommand`s, `device_tick` handlers + `finish_calibration` helper, connect-time blob re-apply, keep-alive-during-calibration guard.
- `crates/tobii-gtk/src/calibrate_flow.rs` (create) — point sets + `CalMode` (pure, tested), then the fullscreen UI (`launch`, cairo dot, tick state machine).
- `crates/tobii-gtk/src/lib.rs` (modify) — `mod calibrate_flow;`, enable `b_cal`, launch the flow.

---

## Task 1: Device-thread calibration state, commands, and persistence

**Files:**
- Modify: `crates/tobii-gtk/src/device.rs`
- Test: `crates/tobii-gtk/src/device.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Connection::{start_calibration, stop_calibration, clear_calibration, add_calibration_point, compute_and_apply_calibration, retrieve_calibration, apply_calibration, set_enabled_eye}` (tobii-usb, existing); `tobii_config::{save_calibration, load_calibration}` (existing).
- Produces: `CalPhase` (pub struct with `active: bool`, `collected: usize`, `last_error: Option<String>`, `finished: Option<Result<(), String>>`, and methods `begin()`, `on_collect(Result<(),String>)`, `on_finish(Result<(),String>)`); `DeviceState.calibration: CalPhase`; `DeviceCommand::{CalBegin{eye: EnabledEye}, CalCollect{x: f64, y: f64}, CalFinish, CalAbort}`. Task 2/3 consume these.

- [ ] **Step 1: Write the failing tests** (append to `mod tests` in `device.rs`)

```rust
    #[test]
    fn cal_phase_begin_is_active_and_empty() {
        let p = CalPhase::begin();
        assert!(p.active);
        assert_eq!(p.collected, 0);
        assert!(p.last_error.is_none());
        assert!(p.finished.is_none());
    }

    #[test]
    fn cal_phase_collect_increments_on_ok_and_records_error() {
        let mut p = CalPhase::begin();
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
        let mut p = CalPhase::begin();
        p.on_finish(Ok(()));
        assert!(!p.active);
        assert_eq!(p.finished, Some(Ok(())));
    }

    #[test]
    fn tick_collects_a_calibration_point() {
        let mut conn = connected(vec![inbound(TTP_MAGIC_RSP, 5, 0x408, &[])]);
        let state = Mutex::new(DeviceState::default());
        let (tx, rx) = channel::<DeviceCommand>();
        tx.send(DeviceCommand::CalCollect { x: 0.5, y: 0.5 }).unwrap();
        device_tick(&mut conn, &state, &rx);
        assert_eq!(state.lock().unwrap().calibration.collected, 1);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p tobii-gtk cal_phase 2>&1 | tail -20`
Expected: FAIL — `CalPhase` / `DeviceCommand::CalCollect` not found (compile error).

- [ ] **Step 3: Add `CalPhase` and the `DeviceState` field**

In `device.rs`, after the `ConnStatus` enum and before `DeviceState`, add:

```rust
/// Progress of an in-flight calibration, published to the UI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalPhase {
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
    /// A fresh in-progress phase (0 points collected).
    pub fn begin() -> Self {
        CalPhase { active: true, collected: 0, last_error: None, finished: None }
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
```

Add the field to `DeviceState`:

```rust
#[derive(Debug, Clone, Default)]
pub struct DeviceState {
    pub status: ConnStatus,
    pub latest_gaze: Option<GazeSample>,
    pub enabled_eye: Option<EnabledEye>,
    pub calibration: CalPhase,
}
```

- [ ] **Step 4: Add the `DeviceCommand` variants**

```rust
pub enum DeviceCommand {
    SetDisplayArea(DisplayCorners),
    SetEnabledEye(EnabledEye),
    /// Begin calibration: set the eye (experiment), then start + clear.
    CalBegin { eye: EnabledEye },
    /// Sample one stimulus point (both eyes).
    CalCollect { x: f64, y: f64 },
    /// Compute + apply + stop + retrieve + persist.
    CalFinish,
    /// Abort: stop (best-effort) and reset.
    CalAbort,
}
```

- [ ] **Step 5: Add the `finish_calibration` helper and command handlers**

Add this free function near `set_error` in `device.rs`:

```rust
/// Compute + stop + retrieve + persist. Always attempts `stop` so the device is
/// not left in calibration mode even when compute fails.
fn finish_calibration<T: Transport>(conn: &mut Connection<T>) -> Result<(), String> {
    let compute = conn
        .compute_and_apply_calibration()
        .map_err(|e| e.to_string());
    let _ = conn.stop_calibration();
    compute?;
    let blob = conn.retrieve_calibration().map_err(|e| e.to_string())?;
    tobii_config::save_calibration(&blob.0).map_err(|e| e.to_string())?;
    Ok(())
}
```

Extend the `match cmd` in `device_tick` with:

```rust
            DeviceCommand::CalBegin { eye } => {
                state.lock().unwrap().calibration = CalPhase::begin();
                let _ = conn.set_enabled_eye(eye); // best-effort select-eyes experiment
                let r = conn
                    .start_calibration()
                    .and_then(|()| conn.clear_calibration())
                    .map_err(|e| e.to_string());
                if let Err(e) = r {
                    state.lock().unwrap().calibration.on_finish(Err(e));
                }
            }
            DeviceCommand::CalCollect { x, y } => {
                let r = conn.add_calibration_point(x, y, 0).map_err(|e| e.to_string());
                state.lock().unwrap().calibration.on_collect(r);
            }
            DeviceCommand::CalFinish => {
                let r = finish_calibration(conn);
                state.lock().unwrap().calibration.on_finish(r);
            }
            DeviceCommand::CalAbort => {
                let _ = conn.stop_calibration();
                state.lock().unwrap().calibration = CalPhase::default();
            }
```

- [ ] **Step 6: Re-apply the saved calibration on connect + keep-alive during calibration**

In `spawn`, inside the `Ok(mut conn)` arm, after the `set_enabled_eye` re-apply block and before reading `cur_eye`, add:

```rust
                // The ET5 wipes calibration on reboot like the display area;
                // re-apply the saved blob so calibration persists across sessions.
                if let Ok(Some(blob)) = tobii_config::load_calibration() {
                    let _ = conn.apply_calibration(&blob);
                }
```

Replace the inner `loop { ... }` body's idle logic so an active calibration does not trip the disconnect detector (gaze pauses during calibration):

```rust
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
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p tobii-gtk 2>&1 | tail -20`
Expected: PASS — all four new tests plus existing device tests pass.

- [ ] **Step 8: Verify the crate builds**

Run: `cargo build -p tobii-gtk 2>&1 | tail -5`
Expected: `Finished` (no errors).

- [ ] **Step 9: Commit**

```bash
git add crates/tobii-gtk/src/device.rs
git commit -m "feat(gtk): device-thread calibration commands + CalPhase + blob re-apply

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: Calibration point sets (pure core)

**Files:**
- Create: `crates/tobii-gtk/src/calibrate_flow.rs`
- Modify: `crates/tobii-gtk/src/lib.rs` (add `mod calibrate_flow;`)
- Test: `crates/tobii-gtk/src/calibrate_flow.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `CalMode` (`Quick`/`Full`, `#[derive(Clone, Copy, PartialEq, Debug)]`) with `pub fn points(self) -> &'static [(f64, f64)]`; consts `QUICK_5`, `FULL_9`. Task 3 consumes these.

- [ ] **Step 1: Create the file with the failing tests**

Create `crates/tobii-gtk/src/calibrate_flow.rs`:

```rust
//! Fullscreen follow-the-dot calibration flow. The user picks Quick/Full, then
//! follows a pulsing dot; each point is sampled by the device thread (see
//! `device::DeviceCommand::Cal*`). The point sets + `CalMode` are unit-tested;
//! the GTK window + cairo dot (added next) are live-validated.

/// Calibration point sets (normalized, top-left origin, center-first — the
/// original's Guest (5) and recalibration (9) sets, verbatim).
pub const QUICK_5: [(f64, f64); 5] =
    [(0.5, 0.5), (0.1, 0.9), (0.5, 0.1), (0.9, 0.9), (0.5, 0.5)];
pub const FULL_9: [(f64, f64); 9] = [
    (0.5, 0.5), (0.1, 0.9), (0.5, 0.1), (0.9, 0.9),
    (0.1, 0.1), (0.5, 0.9), (0.9, 0.1), (0.1, 0.5), (0.9, 0.5),
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CalMode {
    Quick,
    Full,
}

impl CalMode {
    /// The stimulus points for this mode.
    pub fn points(self) -> &'static [(f64, f64)] {
        match self {
            CalMode::Quick => &QUICK_5,
            CalMode::Full => &FULL_9,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_sets_have_expected_counts_and_start_centered() {
        assert_eq!(CalMode::Quick.points().len(), 5);
        assert_eq!(CalMode::Full.points().len(), 9);
        assert_eq!(CalMode::Quick.points()[0], (0.5, 0.5));
        assert_eq!(CalMode::Full.points()[0], (0.5, 0.5));
    }

    #[test]
    fn all_points_are_within_unit_square() {
        for m in [CalMode::Quick, CalMode::Full] {
            for &(x, y) in m.points() {
                assert!((0.0..=1.0).contains(&x), "x in range: {x}");
                assert!((0.0..=1.0).contains(&y), "y in range: {y}");
            }
        }
    }
}
```

- [ ] **Step 2: Declare the module**

In `crates/tobii-gtk/src/lib.rs`, add to the module list (near `pub mod setup_flow;`):

```rust
pub mod calibrate_flow;
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p tobii-gtk calibrate_flow 2>&1 | tail -15`
Expected: PASS — both tests pass; crate compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/tobii-gtk/src/calibrate_flow.rs crates/tobii-gtk/src/lib.rs
git commit -m "feat(gtk): calibration point sets + CalMode (pure core)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: Calibration flow UI (fullscreen window + dot + state machine)

**Files:**
- Modify: `crates/tobii-gtk/src/calibrate_flow.rs`

**Interfaces:**
- Consumes: `CalMode`/`points()` (Task 2); `device::{DeviceCommand, DeviceState, CalPhase}` (Task 1); `tobii_protocol::EnabledEye`.
- Produces: `pub fn launch(app: &Application, state: Arc<Mutex<DeviceState>>, cmd_tx: Sender<DeviceCommand>)`. Task 4 calls it.

**Note:** No new unit tests — the UI is live-validated (same as `widget.rs`/`setup_flow.rs`). The deliverable is a clean build; Task 2's tests must still pass.

- [ ] **Step 1: Add imports at the top of `calibrate_flow.rs`** (below the module doc comment, above `QUICK_5`)

```rust
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{cairo, Align, Application, Button, DrawingArea, Label, Orientation, Overlay};

use crate::device::{CalPhase, DeviceCommand, DeviceState};
use tobii_protocol::EnabledEye;
```

- [ ] **Step 2: Add the phase enum, dot view, timing constants, and helpers**

Add after the `CalMode` impl (before `#[cfg(test)]`):

```rust
// Tick cadence is 33 ms (~30 fps), matching the hub.
const SETTLE_TICKS: u32 = 8; // ~260 ms saccade settle before sampling a point
const COLLECT_TIMEOUT_TICKS: u32 = 300; // ~10 s per point before giving up
const COMPUTE_TIMEOUT_TICKS: u32 = 450; // ~15 s for compute+retrieve

/// UI-side flow state (distinct from the device's `CalPhase`).
#[derive(Clone)]
enum Phase {
    Chooser,
    Collecting { mode: CalMode, index: usize, requested: bool, ticks: u32 },
    Computing { ticks: u32 },
    Done(Result<(), String>),
}

/// What the cairo surface draws this frame.
struct DotView {
    point: Option<(f64, f64)>,
    pulse: f64,
}

/// Primary monitor height (px), to place the header a bit above the middle.
fn screen_height() -> i32 {
    gtk::gdk::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_then(|o| o.downcast::<gtk::gdk::Monitor>().ok())
        .map(|m| m.geometry().height())
        .filter(|h| *h > 0)
        .unwrap_or(1080)
}

/// Dark background + the pulsing fixation dot (a ring converging on a dot).
fn draw_scene(cr: &cairo::Context, w: i32, h: i32, dot: &DotView) {
    let (w, h) = (w as f64, h as f64);
    cr.set_source_rgb(0.08, 0.09, 0.11);
    let _ = cr.paint();
    if let Some((nx, ny)) = dot.point {
        let (cx, cy) = (nx * w, ny * h);
        let ring = 26.0 + (1.0 - dot.pulse) * 22.0;
        cr.set_source_rgba(0.30, 0.85, 0.85, 0.6 * dot.pulse.max(0.15));
        cr.set_line_width(3.0);
        cr.arc(cx, cy, ring, 0.0, std::f64::consts::TAU);
        let _ = cr.stroke();
        cr.set_source_rgb(0.30, 0.85, 0.85);
        cr.arc(cx, cy, 7.0, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    }
}

/// Reflect the current phase in the instruction text + visible controls.
fn update_ui(
    phase: &Phase,
    instr: &Label,
    chooser: &gtk::Box,
    done_box: &gtk::Box,
    cancel: &Button,
) {
    match phase {
        Phase::Chooser => {
            instr.set_text(
                "Choose a calibration. Sit comfortably, about an arm's length from the screen.",
            );
            chooser.set_visible(true);
            done_box.set_visible(false);
            cancel.set_visible(true);
        }
        Phase::Collecting { index, mode, .. } => {
            instr.set_text(&format!(
                "Follow the dot with your eyes  ·  point {} of {}",
                index + 1,
                mode.points().len()
            ));
            chooser.set_visible(false);
            done_box.set_visible(false);
            cancel.set_visible(true);
        }
        Phase::Computing { .. } => {
            instr.set_text("Computing your calibration…");
            chooser.set_visible(false);
            done_box.set_visible(false);
            cancel.set_visible(false);
        }
        Phase::Done(res) => {
            match res {
                Ok(()) => instr.set_text("Calibration complete."),
                Err(e) => instr.set_text(e),
            }
            chooser.set_visible(false);
            done_box.set_visible(true);
            cancel.set_visible(false);
        }
    }
}
```

- [ ] **Step 3: Add the `launch` function**

Add after the helpers (before `#[cfg(test)]`):

```rust
/// Open the fullscreen follow-the-dot calibration flow.
pub fn launch(app: &Application, state: Arc<Mutex<DeviceState>>, cmd_tx: Sender<DeviceCommand>) {
    // Eye to calibrate: the device's current selection, defaulting to Both.
    let eye = state.lock().unwrap().enabled_eye.unwrap_or(EnabledEye::Both);

    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Calibration")
        .build();
    win.set_modal(true);
    win.fullscreen();

    let phase = Rc::new(RefCell::new(Phase::Chooser));
    let dot = Rc::new(RefCell::new(DotView { point: None, pulse: 0.0 }));

    let area = DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    {
        let dot = dot.clone();
        area.set_draw_func(move |_, cr, w, h| draw_scene(cr, w, h, &dot.borrow()));
    }

    let instr = Label::new(Some("Calibrate your eye tracker"));
    instr.add_css_class("app-title");
    instr.set_halign(Align::Center);
    instr.set_justify(gtk::Justification::Center);
    instr.set_wrap(true);

    let quick = Button::with_label("Quick (5 points)");
    let full = Button::with_label("Full (9 points)");
    let chooser = gtk::Box::new(Orientation::Horizontal, 10);
    chooser.set_halign(Align::Center);
    chooser.append(&quick);
    chooser.append(&full);

    let done_btn = Button::with_label("Done");
    let retry_btn = Button::with_label("Retry");
    let done_box = gtk::Box::new(Orientation::Horizontal, 10);
    done_box.set_halign(Align::Center);
    done_box.append(&retry_btn);
    done_box.append(&done_btn);
    done_box.set_visible(false);

    let cancel = Button::with_label("Cancel");
    cancel.set_halign(Align::Center);

    let header = gtk::Box::new(Orientation::Vertical, 16);
    header.set_halign(Align::Center);
    header.set_valign(Align::Start);
    header.set_margin_top((screen_height() as f64 * 0.30) as i32);
    header.append(&instr);
    header.append(&chooser);
    header.append(&done_box);
    header.append(&cancel);

    let overlay = Overlay::new();
    overlay.set_child(Some(&area));
    overlay.add_overlay(&header);
    win.set_child(Some(&overlay));

    // Chooser -> begin calibration for the chosen mode.
    let start_mode: Rc<dyn Fn(CalMode)> = {
        let phase = phase.clone();
        let cmd_tx = cmd_tx.clone();
        Rc::new(move |mode: CalMode| {
            let _ = cmd_tx.send(DeviceCommand::CalBegin { eye });
            *phase.borrow_mut() = Phase::Collecting { mode, index: 0, requested: false, ticks: 0 };
        })
    };
    {
        let s = start_mode.clone();
        quick.connect_clicked(move |_| s(CalMode::Quick));
    }
    {
        let s = start_mode.clone();
        full.connect_clicked(move |_| s(CalMode::Full));
    }
    {
        let phase = phase.clone();
        retry_btn.connect_clicked(move |_| *phase.borrow_mut() = Phase::Chooser);
    }
    {
        let win = win.clone();
        let cmd_tx = cmd_tx.clone();
        done_btn.connect_clicked(move |_| {
            let _ = cmd_tx.send(DeviceCommand::CalAbort);
            win.close();
        });
    }
    {
        let win = win.clone();
        let cmd_tx = cmd_tx.clone();
        cancel.connect_clicked(move |_| {
            let _ = cmd_tx.send(DeviceCommand::CalAbort);
            win.close();
        });
    }

    // Esc cancels.
    let keys = gtk::EventControllerKey::new();
    {
        let win = win.clone();
        let cmd_tx = cmd_tx.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                let _ = cmd_tx.send(DeviceCommand::CalAbort);
                win.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    win.add_controller(keys);

    // ~30 fps state machine: read the device's CalPhase, advance the UI phase.
    let tick_cmd = cmd_tx.clone();
    glib::timeout_add_local(Duration::from_millis(33), move || {
        let cal: CalPhase = state.lock().unwrap().calibration.clone();
        let mut ph = phase.borrow_mut();
        let mut next: Option<Phase> = None;
        match &*ph {
            Phase::Chooser => {
                dot.borrow_mut().point = None;
            }
            Phase::Collecting { mode, index, requested, ticks } => {
                let (mode, index, requested, ticks) = (*mode, *index, *requested, *ticks);
                if let Some(Err(e)) = &cal.finished {
                    next = Some(Phase::Done(Err(e.clone())));
                } else if let Some(e) = &cal.last_error {
                    let _ = tick_cmd.send(DeviceCommand::CalAbort);
                    next = Some(Phase::Done(Err(format!(
                        "Couldn't read a point: {e}. Make sure you're seated and looking at the dots."
                    ))));
                } else if cal.collected > index {
                    let pts = mode.points();
                    if index + 1 >= pts.len() {
                        let _ = tick_cmd.send(DeviceCommand::CalFinish);
                        next = Some(Phase::Computing { ticks: 0 });
                    } else {
                        next = Some(Phase::Collecting {
                            mode,
                            index: index + 1,
                            requested: false,
                            ticks: 0,
                        });
                    }
                } else {
                    let (px, py) = mode.points()[index];
                    {
                        let mut d = dot.borrow_mut();
                        d.point = Some((px, py));
                        d.pulse = (d.pulse + 0.06) % 1.0;
                    }
                    let t = ticks + 1;
                    if !requested && t >= SETTLE_TICKS {
                        let _ = tick_cmd.send(DeviceCommand::CalCollect { x: px, y: py });
                        next = Some(Phase::Collecting { mode, index, requested: true, ticks: 0 });
                    } else if requested && t >= COLLECT_TIMEOUT_TICKS {
                        let _ = tick_cmd.send(DeviceCommand::CalAbort);
                        next = Some(Phase::Done(Err("Timed out reading a point.".into())));
                    } else {
                        next = Some(Phase::Collecting { mode, index, requested, ticks: t });
                    }
                }
            }
            Phase::Computing { ticks } => {
                dot.borrow_mut().point = None;
                if let Some(res) = &cal.finished {
                    next = Some(Phase::Done(res.clone()));
                } else {
                    let t = ticks + 1;
                    if t >= COMPUTE_TIMEOUT_TICKS {
                        next = Some(Phase::Done(Err("Calibration computation timed out.".into())));
                    } else {
                        next = Some(Phase::Computing { ticks: t });
                    }
                }
            }
            Phase::Done(_) => {}
        }
        if let Some(n) = next {
            *ph = n;
        }
        update_ui(&ph, &instr, &chooser, &done_box, &cancel);
        area.queue_draw();
        glib::ControlFlow::Continue
    });

    win.present();
}
```

- [ ] **Step 4: Build and confirm no warnings/errors**

Run: `cargo build -p tobii-gtk 2>&1 | tail -8`
Expected: `Finished` with no errors. (An unused-`state`/`Mutex` warning must NOT appear — both are used in the tick.)

- [ ] **Step 5: Run the crate tests (Task 2's still pass)**

Run: `cargo test -p tobii-gtk 2>&1 | tail -12`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/tobii-gtk/src/calibrate_flow.rs
git commit -m "feat(gtk): follow-the-dot calibration flow UI (window + dot + state machine)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 4: Hub wiring — enable "Improve my calibration"

**Files:**
- Modify: `crates/tobii-gtk/src/lib.rs`

**Interfaces:**
- Consumes: `calibrate_flow::launch` (Task 3); `state`, `cmd_tx`, `app` (already in `build_ui`).

- [ ] **Step 1: Replace the disabled `b_cal` button wiring**

In `build_ui` (`lib.rs`), replace:

```rust
    let b_cal = Button::with_label("Improve calibration");
    b_cal.set_sensitive(false);
    b_cal.set_tooltip_text(Some("Calibration — coming in B3"));
```

with:

```rust
    let b_cal = Button::with_label("Improve calibration");
    {
        let app = app.clone();
        let state = state.clone();
        let cmd_tx = cmd_tx.clone();
        b_cal.connect_clicked(move |_| {
            calibrate_flow::launch(&app, state.clone(), cmd_tx.clone())
        });
    }
```

(Placement: this is before the `glib::timeout_add_local` tick that moves `state`, so `state.clone()` here is valid.)

- [ ] **Step 2: Build the whole workspace**

Run: `cargo build 2>&1 | tail -8`
Expected: `Finished` — no errors.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test 2>&1 | tail -20`
Expected: PASS — all crates.

- [ ] **Step 4: Commit**

```bash
git add crates/tobii-gtk/src/lib.rs
git commit -m "feat(gtk): launch calibration flow from 'Improve my calibration'

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Live validation (manual, after Task 4) — NOT a subagent step

Run `cargo run --release -p tobii-gtk` and, with the tracker connected:

1. Click **Improve my calibration** → chooser appears fullscreen. Pick **Quick**.
2. Follow the dot through all 5 points; confirm each advances after you fixate, and "Computing…" then "Calibration complete." appear. Click **Done**.
3. Confirm the hub's live eye-position view works afterward (device streams again).
4. Note whether points advance promptly (does `add_point` block until enough data, or return fast?) — record for tuning `SETTLE_TICKS`/timeout.
5. **Reboot persistence:** unplug/replug (or close+reopen) → gaze should use the saved calibration (blob re-applied on connect).
6. **Select-eyes experiment:** in the hub pick *Left eye only*, run a calibration, then watch the gaze stream (`tobii stream`) — does `valR` go to 4 (right no longer detected)? Record the result; it decides whether the per-eye path is needed later.

Then: update `README.md` status (calibration now working) and memory `et5-calibration-protocol` with the live findings; run the final whole-branch review and use `superpowers:finishing-a-development-branch`.

## Self-Review notes

- Spec coverage: threading (Task 1), commands + persistence (Task 1), point sets (Task 2), fullscreen flow + dot + result + cancel (Task 3), hub wiring (Task 4), select-eyes experiment (`CalBegin` sets eye; validated manually). ✓
- Types consistent across tasks: `CalPhase`, `DeviceCommand::Cal*`, `CalMode::points()`, `launch(app, state, cmd_tx)`. ✓
- No placeholders; every code step is complete. ✓
