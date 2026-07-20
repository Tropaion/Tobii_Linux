# B3 — Follow-the-dot Calibration Flow (Design Spec)

**Date:** 2026-07-20
**Status:** Approved (design); ready for implementation plan
**Supersedes/extends:** the GTK4 redesign spec (2026-07-19); reuses that hub + `setup_flow.rs` patterns.

## Goal

Add a guided **follow-the-dot gaze calibration** to the GTK4 app: the user
follows a moving dot across the screen, the device builds a calibration, and it
is saved + re-applied on every connect. This unblocks the hub's disabled
*"Improve my calibration"* button and improves gaze accuracy.

## Background & evidence

Protocol facts are code-verified (native disasm) and partly hardware-verified
(B3 spike, commit `9588212`). See memory `et5-calibration-protocol`.

Op map (all session ops carry the bare `00 00` payload):

| Command | Op | Notes |
|---|---|---|
| `calibration_start` | `0x3f2` | enter calibration mode; **no eye arg on the wire** |
| `calibration_clear` | `0x424` | discard collected/active data (destructive) |
| `calibration_stop`  | `0x3fc` | leave calibration mode |
| `collect_data_2d` (add point) | `0x408` | `00 00` + Q42(x) + Q42(y) + u32(eye); eye 0=both |
| `compute_and_apply` | `0x42f` | computes AND applies |
| `retrieve`          | `0x44c` | returns the opaque blob to persist |
| `apply`             | `0x456` | re-apply a saved blob |

**Managed sequence:** `start → clear → per point{ animate dot, wait ~200 ms
saccade, add_point } → compute → stop → retrieve → save`. Note: compute happens
BEFORE stop; retrieve AFTER stop.

**Hardware-verified (spike):** `start` + `stop` both ACK standalone and the
device keeps streaming afterward. `clear` + the full point sequence are
validated live during this flow (recalibration is the intent).

**Per-point mechanics:** the host draws/animates the dot, waits for the saccade,
then calls `add_point` **once**; the **device** does the time-windowed sampling
internally and the call **blocks until it has gathered enough** — there is no
host-side sample loop.

**Select-eyes caveat & experiment:** `calibration_start` drops the eye argument
and the original hardcodes both eyes, so a standard calibration is not known to
enable "Select eyes to detect". This flow performs a **cheap experiment**: it
SETs the saved `enabled_eye` (op `0xc58`) *before* `start`, then calibrates both
eyes. If the tracker then respects the selection, select-eyes works for free; if
not, we have lost nothing (the calibration is valid either way) and know the
per-eye path (`0x42e`) is required. The per-eye path is out of scope here.

## Architecture

### Threading model

Calibration is stateful and `add_point` **blocks** the device thread while the
tracker samples, so the whole sequence runs on the **device thread** (which owns
the `Connection`). The UI drives pacing (dot animation, saccade settle) and
communicates through the existing primitives:

- **UI → device:** new `DeviceCommand` variants (sent over the existing
  `cmd_tx`).
- **device → UI:** a new `calibration: CalPhase` field on `DeviceState`, read by
  the hub/flow's 33 ms `glib` tick (same pattern as gaze + status).

While `add_point` blocks, the device thread does not process other commands or
publish gaze — acceptable during calibration; the UI shows "hold still".

### Data types (`crates/tobii-gtk/src/device.rs`)

```rust
/// Progress of an in-flight calibration, published to the UI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalPhase {
    /// True between CalBegin and CalFinish/CalAbort.
    pub active: bool,
    /// Points successfully collected so far this session.
    pub collected: usize,
    /// Set when the last CalCollect failed (per-point error to surface).
    pub last_error: Option<String>,
    /// Set once CalFinish resolves: Ok(()) on success, Err(msg) on failure.
    pub finished: Option<Result<(), String>>,
}
```

`DeviceState` gains `pub calibration: CalPhase` (added to its `Default`).

New `DeviceCommand` variants:

```rust
CalBegin { eye: EnabledEye },   // set_enabled_eye(eye) + start + clear
CalCollect { x: f64, y: f64 },  // add_calibration_point(x, y, 0)
CalFinish,                      // compute + stop + retrieve + save_calibration
CalAbort,                       // stop (best-effort); reset CalPhase
```

### Device-thread handling (`device.rs` `device_tick` / command drain)

- `CalBegin{eye}`: reset `CalPhase` to `{active:true, ..default}`; best-effort
  `conn.set_enabled_eye(eye)` (log on failure, do not abort); `conn.start_calibration()`;
  `conn.clear_calibration()`. On a hard error, set `finished = Some(Err(..))`.
- `CalCollect{x,y}`: `conn.add_calibration_point(x, y, 0)`. On Ok, `collected += 1`
  and clear `last_error`. On Err, set `last_error = Some(..)` (do NOT increment).
- `CalFinish`: `conn.compute_and_apply_calibration()` → `conn.stop_calibration()`
  → `conn.retrieve_calibration()` → `tobii_config::save_calibration(blob)`; set
  `finished = Some(Ok(()))`. Any step's error → `finished = Some(Err(..))` (still
  attempt `stop` so the device is not left in calibration mode).
- `CalAbort`: best-effort `conn.stop_calibration()`; reset `CalPhase` to default.

### Connect-time re-apply (`device.rs` spawn/connect path)

After applying the saved display area + enabled_eye on connect, also:

```rust
if let Ok(Some(blob)) = tobii_config::load_calibration() {
    let _ = conn.apply_calibration(&blob); // best-effort; log on failure
}
```

The ET5 wipes calibration on reboot (it reboots on session close) exactly like
the display area, so this keeps calibration persistent.

### UI flow (`crates/tobii-gtk/src/calibrate_flow.rs`, new)

Public entry: `pub fn launch(app: &Application, state: <shared>, cmd_tx: Sender<DeviceCommand>)`
where `<shared>` is the `Arc<Mutex<DeviceState>>` returned by `device::spawn`
(the same handle `overlay::show` receives), and `cmd_tx` is the sender
`setup_flow::launch` already receives. The flow needs both: `state` to read
`CalPhase`/`enabled_eye`, `cmd_tx` to send commands.

A fullscreen `ApplicationWindow` (dark background) with an internal state machine
driven by a per-flow 33 ms tick reading `state.lock().calibration`:

1. **Chooser:** two buttons — *Quick (5 points)* / *Full (9 points)* — plus
   *Cancel*. Selecting one records the chosen point array and sends
   `CalBegin{eye}` where `eye` is read from `state.calibration`'s sibling
   `DeviceState.enabled_eye` at launch (the value already seeded from the device
   / set by the hub radios), defaulting to `EnabledEye::Both` when unknown.
2. **Collecting:** for point index `i`, draw a pulsing dot (a ring converging on
   a small fixation dot) at `points[i]` on a fullscreen `DrawingArea`. After a
   ~200 ms settle (dot has arrived + saccade), send `CalCollect{points[i]}` once
   and enter "sampling" for `i`. Show a subtle "hold still" hint. Wait until
   `calibration.collected == i+1`, then advance to `i+1`.
   - Guard against double-send: track "collect requested for index i" locally so
     the tick sends `CalCollect` exactly once per point.
   - **Timeout/error:** if `collected` has not advanced within a bounded time
     (e.g. ~6 s) or `last_error` is set, show a brief "Couldn't read that point —
     hold still and look at the dot" and re-send `CalCollect` for the same index
     (bounded retries, e.g. 3), else fail to the result screen with an error.
3. **Computing:** after the last point, send `CalFinish`; show "Computing…";
   wait for `calibration.finished`.
4. **Result:** on `Ok`, "Calibration complete" + a *Done* button (closes the
   window). On `Err(msg)`, show the message + *Retry* (back to chooser) and
   *Cancel*.
5. **Cancel** (any phase): send `CalAbort`, close the window.

Point arrays (module constants, `[0,1]` top-left, center-first):

```rust
const QUICK_5: [(f64, f64); 5] =
    [(0.5, 0.5), (0.1, 0.9), (0.5, 0.1), (0.9, 0.9), (0.5, 0.5)];
const FULL_9: [(f64, f64); 9] = [
    (0.5, 0.5), (0.1, 0.9), (0.5, 0.1), (0.9, 0.9),
    (0.1, 0.1), (0.5, 0.9), (0.9, 0.1), (0.1, 0.5), (0.9, 0.5),
];
```

### Hub wiring (`crates/tobii-gtk/src/lib.rs`)

The *"Improve my calibration"* button (`b_cal`) becomes sensitive and its
click launches `calibrate_flow::launch(&app, state.clone(), cmd_tx.clone())`.
Its tooltip loses the "coming in B3" text. The hub already holds `state` and
`cmd_tx`; pass clones as `setup_flow` does.

## Coordinate model

`add_point` x/y are normalized display coords in `[0,1]`, top-left origin — the
same space in which the dot is drawn on the fullscreen area, so the on-screen dot
position and the sampled point are identical. No display-geometry math is
involved (that is the separate display-area setup).

## Error handling

- Any calibration op returning `NoResponse`/error is caught on the device thread
  and reported via `CalPhase` (`last_error` per point, `finished = Err` for the
  compute/finish path). The device thread always attempts `stop` on the finish
  path so the device is never left in calibration mode.
- The UI never blocks the main loop: all blocking work is on the device thread;
  the UI only polls `CalPhase`.
- `CalAbort` on window close/Cancel guarantees `stop` is sent.

## Testing

**Unit (pure, no device):**
- `device.rs`: `CalPhase` default is inactive/zero; a helper that maps a
  `Result` from each cal step into `CalPhase` transitions (collect increments
  only on Ok; finish sets `finished`). Extract the transition logic into pure
  functions so it is testable without a `Connection`.
- `tobii-protocol`/`tobii-usb`: session-op payloads + ack (already added in the
  spike: `session_payload_is_prefix_only`, `session_ops_send_correct_ops_and_ack`).
- `calibrate_flow.rs`: pure point-array + flow-state helpers (e.g. "next index",
  "all collected?", chooser → array selection) unit-tested; cairo drawing is
  live-validated.

**Live validation (hardware, manual):**
1. `clear` + full sequence complete without error; `retrieve` returns a non-empty
   blob; it is saved.
2. Whether `add_point` blocks until "enough data" vs returns immediately (tunes
   the UI settle/timeout) — record the observed behavior.
3. Re-apply on connect: after a reboot, gaze uses the saved calibration.
4. **Select-eyes experiment:** SET Left/Right, calibrate, then observe per-eye
   validity in the stream — does detection change? Record the result (this
   decides whether the per-eye path is needed later).

## Out of scope (YAGNI)

- Per-point quality readout (`stimulus_points_get` 0x460) — later enhancement.
- The per-eye calibration path (`collect_data_per_eye_2d` / `0x42e`).
- Head-pose output (Plan 5) — independent milestone.

## Files

| File | Change |
|---|---|
| `crates/tobii-protocol/src/frame.rs` | ✅ ops added (spike) |
| `crates/tobii-protocol/src/calibration.rs` | ✅ `cal_session_payload` (spike) |
| `crates/tobii-usb/src/connection.rs` | ✅ `start/stop/clear_calibration` (spike) |
| `crates/tobii-cli/src/main.rs` | ✅ `cal-probe` (spike) |
| `crates/tobii-gtk/src/device.rs` | `CalPhase`, `DeviceState.calibration`, 4 `DeviceCommand`s, handlers, connect re-apply |
| `crates/tobii-gtk/src/calibrate_flow.rs` | **new** — fullscreen flow |
| `crates/tobii-gtk/src/lib.rs` | enable `b_cal`, launch flow, `mod calibrate_flow` |
