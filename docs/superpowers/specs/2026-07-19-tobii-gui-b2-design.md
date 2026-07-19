# Tobii GUI (B2) — Config Hub + Guided Flows (Design Spec)

**Date:** 2026-07-19
**Status:** Approved design (brainstormed 2026-07-19), pre-planning
**Author:** Fabian Plaimauer (with Claude Code)
**License:** GPL-3.0

## 1. Goal & context

A graphical configuration app for the Tobii ET5 on Linux — sub-project **B2** of the
functional-parity effort (A + B1 shipped: display-config + calibration backend, on
`master`). B2 delivers the **GUI shell**: a persistent hub window + the **display-setup**
and **eye-position** guided flows. The follow-the-dot **calibration** flow is **B3**
(the backend for it already exists from B1).

**Fidelity target:** *functional* equivalent of the original Tobii software (clean
native-Linux look, not a pixel copy).

### What the original actually is (reverse-engineered, verified)

Decompiling `Tobii.Configuration.*` established (adversarially verified against the
decompiled C#) that the original is **not** a wizard, dashboard, or in-app hub. It is a
**single-shot, CLI-arg-dispatched flow launcher**: one process runs exactly one
**fullscreen modal guided flow** (chosen by a startup arg — `-S` set up display,
`-C/-G/-Q` calibrate, `-N` new profile, `-P` eye position, `-R` auto-resolve) then
exits (`ApplicationConfiguration.cs`, `MainModel.cs`, `App.cs`: `ShutdownMode=
OnExplicitShutdown`, no `MainWindow`). Each flow is a **forward-only state machine**
with in-flow **Retry/Cancel** and no global Back (`CalibrationViewModel`,
`MonitorMappingViewModel`). The **menu/hub lives in a separate app** (Tobii Experience
portal — not in this binary). The only in-app orchestrator is `-R`
`ResolveConfiguration`: a **device-status-driven auto-chain** (unconfigured display →
setup → re-check → calibrate/new-profile), not a "first-run" flag.

### B2's chosen shape

**Faithful fullscreen guided flows + a richer persistent hub.** We build a single Linux
binary, so it must supply *both* the (external, in the original) hub role and the flows:

- a **persistent hub window** (status + launch buttons + a live eye-position mini-view),
- **fullscreen, forward-only guided flows** (display-setup, eye-position; calibration in
  B3), launched from the hub, each returning to the hub on done/cancel.

## 2. Non-goals (B2)

- The **calibration** flow (follow-the-dot stimulus) → **B3** (hub's *Calibrate* button
  is present but disabled until then).
- Head-pose / opentrack (**Plan 5**), independent.
- Profile management, guest calibration (`-G`), quick calibration (`-Q`), device
  settings — the original keeps these in its external portal; out of scope for B2.
- Draggable direct-manipulation of the screen plane — the original *may* have it
  (`DisplaySetupLineHandleView` exists) but the interaction is unconfirmed; B2 ships the
  guided form first, a draggable overlay is a later enhancement.

## 3. Architecture

- **New crate `tobii-gui`** (binary `tobii-gui`): `eframe`/`egui` + the workspace crates
  (`tobii-usb`, `tobii-config`, `tobii-protocol`). This is the project's **first external
  GUI dependency** — deliberately isolated in its own crate so the lean `tobii` CLI and
  the pure protocol/config crates stay dependency-light. (`rusb` remains their only dep.)
- **Device thread + channels.** A dedicated thread owns the blocking `Connection`:
  runs the handshake, then loops — polling gaze (short timeout) into a shared
  `DeviceState` (latest `GazeSample` + connection status), and draining a command channel
  (`mpsc`) for UI requests (set display area now; later: run calibration). On
  disconnect/error it retries `UsbTransport::open()` + reconnect. The **egui UI thread**
  renders at its own cadence, reads `DeviceState` (behind an `Arc<Mutex<…>>` or via a
  watch channel), and sends `DeviceCommand`s. Rationale: the blocking `rusb` driver stays
  on one thread; the UI never blocks. (Rejected: driving the blocking driver from the UI
  thread — stalls the frame loop; a tokio/async stack — overkill for blocking rusb.)
- **App model.** `TobiiApp` (the `eframe::App`) holds an `enum Screen { Hub, DisplaySetup(DisplaySetupFlow), EyePosition(EyePositionFlow) }`. Each flow is a small
  forward-only state machine (`enum …State`) with `Retry`/`Cancel`/`Done` transitions and
  **no global Back**. Flows render fullscreen (borderless maximized viewport / eframe
  fullscreen); the hub renders windowed. Returning `Done`/`Cancel` swaps `Screen` back to
  `Hub`.

## 4. Screens

### 4.1 Hub (persistent, windowed)
- **Status:** connected / not-found (with the udev-rule hint) / handshaking; live gaze
  validity; detected monitor (model + mm).
- **Launch buttons:** *Set up display*, *Position eyes*, *Calibrate* (disabled, "B3").
- **Live eye-position mini-view:** a small trackbox + both eyes (§4.4 visualization),
  updating from the device thread's latest gaze.
- **Onboarding nudge:** if no `config.toml` exists (or the device reports an unset display
  area), highlight *Set up display* — state-driven, à la the original's `-R`.

### 4.2 Display-setup flow (fullscreen; the original's `-S`)
Forward-only: **Detect monitor** (EDID; confirm/override model + mm) → **Geometry**
(tilt + tracker offsets; live corner preview using `DisplaySetup::to_corners`) →
**Apply + save** (send the display area to the device via a `DeviceCommand`; write
`config.toml`) → **Trackbox confirmation** (live eye-position view — "we can see you") →
**Done** (return to hub). Cancel at any step returns to the hub without saving. Reuses
Plan 4's `DisplaySetup` math + `tobii-config` EDID/TOML.

### 4.3 Eye-position flow (fullscreen; the original's `-P`)
A standalone fullscreen "position your eyes" screen: the trackbox visualization + guidance
("move closer / further", "center yourself"), user-closable (Esc/Done → hub). Shares the
visualization widget with the hub mini-view and §4.2's confirmation step.

### 4.4 Eye-position visualization (shared widget) + backend prerequisite
Draws a trackbox rectangle with both eyes plotted from **trackbox-normalized eye position**
(`[0,1]²`), colored by validity, with a distance readout (z) and centering guidance; shows
"no eyes detected" when validity says so.

**Backend prerequisite (in `tobii-protocol`):** decode gaze columns **`0x03`
(trackbox_eye_pos_L)** / **`0x09` (trackbox_eye_pos_R)** — Q42 `point3d`, normalized x/y
in the box + z = distance — into `GazeSample` with `present` flags. These are currently
undecoded (`gaze.rs` has eye-origin mm cols `0x02`/`0x08` and raw `0x17`/`0x18`, not the
normalized box cols). Golden-tested against a **real captured frame** (the ET5 is
connected — capture a valid one with eyes in view). **Reconcile** the present-mask bit
discrepancy flagged in the B1 spec §10 Q5 (crate uses bits 12/13 for raw origins; one
extraction reported 19/20) against the live `present_mask`.

## 5. Data flow

Device thread: `connect` (handshake) → loop { `next_gaze` (short timeout) → update
`DeviceState.latest_gaze`; drain `cmd_rx` → e.g. `SetDisplayArea(corners)` →
`Connection::request(OP_SET_DISPLAY_AREA, payload)`; on error → mark disconnected, retry
`connect` }. UI thread: each frame, read `DeviceState`; on *Apply* → `cmd_tx.send(SetDisplayArea)`; render the eye-position widget from `latest_gaze`.

## 6. Error handling

- **Device not found / permission denied:** the hub shows a friendly banner reusing the
  `UsbError` messages (incl. the udev-rule hint); the display-setup *Geometry* form stays
  usable offline (Apply disabled until connected). The device thread keeps retrying.
- **Handshake failure / disconnect mid-flow:** flows surface a non-fatal error state with
  Retry/Cancel (matching the original's in-flow retry); never panic.
- egui/eframe init failure (no display server) exits with a clear message.

## 7. Testing strategy

- **Unit (no hardware, no GUI):** the flow state machines (transitions: step→step,
  Retry/Cancel/Done); the eye-position mapping math (normalized `[0,1]` → widget pixel
  coords, distance/centering thresholds); the `0x03/0x09` gaze-decode **golden vector**
  (captured frame → expected normalized positions); the device-thread command/gaze
  plumbing via the existing `MockTransport`-backed `Connection`.
- **Manual/live:** egui rendering, the fullscreen flows, and the end-to-end setup +
  eye-position experience on the ET5 (the final task).
- Keep GUI-drawing code thin over testable pure logic (widgets read a plain data struct;
  the math + state machines are separate, pure, and unit-tested).

## 8. Persistence & integration

Reuses `tobii-config` unchanged (`DisplaySetup` TOML at
`$XDG_CONFIG_HOME/tobii-linux/config.toml`, EDID `detect_monitors`). The display-setup
flow reads/writes the same config the `tobii setup` CLI does — the two stay
interchangeable. No new config schema.

## 9. Risks & open questions

1. **egui dependency tree size** — accepted: pure-Rust, self-contained (bundles
   windowing + GPU/GL), no system GUI libraries; isolated in `tobii-gui`.
2. **Fullscreen flow mechanics in eframe** — confirm borderless-maximized vs true
   fullscreen viewport behavior on the user's compositor (Wayland/CachyOS) during the
   first GUI task; fall back to a maximized borderless window if fullscreen is flaky.
3. **`0x03/0x09` columns + present-mask bits** need live confirmation — the ET5 is here;
   capture + verify during the backend task (resolves B1 spec §10 Q5).
4. **Screen-plane draggable interaction** in the original is unconfirmed — B2 ships the
   guided geometry form; a draggable overlay is a deferred enhancement, not scoped here.
5. **Device thread ↔ UI shared state** — keep it a single small `Mutex`ed snapshot (or a
   watch channel); avoid lock contention by copying the latest gaze out per frame.

## 10. Phasing (informs the plan)

1. **Backend** — decode gaze cols `0x03/0x09` in `tobii-protocol` (+ golden vector,
   live-captured); reconcile present-mask bits.
2. **Crate + device thread + hub shell** — `tobii-gui` scaffold, the device thread +
   `DeviceState`/`DeviceCommand`, and the hub window (status + buttons + mini eye-view).
3. **Display-setup flow** (fullscreen) — the highest-value flow; reuses Plan 4 math.
4. **Eye-position flow** (fullscreen) + shared visualization polish.
5. **Live validation** on the ET5.

B3 (calibration flow) and Plan 5 (head-pose/opentrack) follow.
