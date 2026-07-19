# Tobii GTK4 GUI Redesign — Design Spec

**Date:** 2026-07-19
**Status:** Approved design (brainstormed 2026-07-19), pre-planning
**Author:** Fabian Plaimauer (with Claude Code)
**License:** GPL-3.0-only
**Supersedes:** the GUI portions of `2026-07-14`/`2026-07-19` egui-era B2 design. The **backend/protocol**
decisions there (display-area planar model, gaze decode, calibration ops, EDID) remain valid and are reused.

## 1. Goal & context

Rebuild the Tobii ET5 Linux configuration GUI in **GTK4** to be an *original-inspired, modern* equivalent of the
Windows Tobii Experience software — polished dark theme, guided illustrated flows, a live gaze overlay, and the
settings the original exposes. The eye-tracking **core is already built and hardware-validated** (`tobii-protocol`,
`tobii-usb`, `tobii-config`: USB transport, TTP handshake, gaze/trackbox decode, display-area get/set, calibration
ops, EDID). The prior **egui** GUI (`tobii-gui`) proved the device-thread + eye-position-mapping architecture and
is now retired in favor of GTK4 (egui cannot do a Wayland click-through overlay and fights a polished consumer look).

**Fidelity target (chosen):** *original-inspired modern* — same flows, features, structure, and wording as the
original, restyled with a clean modern dark/teal GTK theme. Not a pixel-perfect clone.

### Why GTK4 (decision record)
egui is immediate-mode, ideal for live-data/tool UIs but weak for (a) **Wayland layer-shell overlays** (the
"Preview my gaze" dot) and (b) polished, illustrated, animated consumer UIs. GTK4 is the native Linux toolkit:
mature Wayland support incl. **`gtk4-layer-shell`** overlays (the tobiifree reference uses exactly this for the
ET5 gaze dot), **CSS theming** that can match the original, and SVG illustrations via librsvg. Cost accepted: a
system GTK dependency (a deliberate break from the prior rusb-only ethos, isolated to the GUI crate) and more
verbose binding code. The portable core (protocol/usb/config + the device thread + `eyeview` mapping) carries over.

### What the original is (reverse-engineered + verified this session)
- The `-S` **Set up display** flow computes the screen plane from a **two-line visual alignment** ("move the lines
  to the marks on top of your eye tracker"): from normalized line positions `L,R` and the ET5's fixed physical
  width (≈376.3 mm), it derives `monitor_width_mm = 376.3/(R−L)`, height from the monitor aspect ratio, and
  `offset_x_mm = ((L+R)/2 − 0.5)·width`, then composes the **same TL/TR/BL corner geometry we already send via
  `SET_DISPLAY_AREA 0x5a0`**. No new geometry protocol. Flow order: Loading → line-alignment → Save (SetDisplayArea
  [+ optional SetDisplayId]) → trackbox confirmation → Done.
- **Select eyes to detect** (Both / Left only / Right only) is a genuine persistent **device property**
  (`platmod_property_enabled_eye`, same family as display-area), but the original only ever pushes it via
  `calibration_start(enabled_eye)` and stores the choice host-side per profile. Its TTP op code is **unmapped**;
  standalone SET is untested; enum ordering differs (stream-engine `L=0/R=1/both=2` vs our cal arg `both=0/L=1/R=2`).
  → handled by **Spike S4** below.
- **Preview my gaze** is a host-side gaze-dot overlay of the live 2D gaze point (col `0x1c`) — no device command.
- **Settings sections** (from Tobii Experience): Improve calibration (→ B3), Preview my gaze, Select eyes to detect,
  Set up display, Change user profile (host-side, out of scope now).

## 2. Non-goals (this redesign)
- **Improve/redo calibration** (follow-the-dot) → **B3** (backend ops `cal_add_point`/`compute`/`retrieve`/`apply`
  exist; still need `calibration_start/stop/clear`). The hub shows *Improve calibration* **disabled** until B3.
- **User-profile management** (`-N`) — host-side registry concept; out of scope.
- **Multi-monitor / duplicated-display selection** in Set-up-display — single primary display first; multi-monitor
  is a later enhancement.
- Pixel-perfect cloning of the original's exact assets/animations.

## 3. Architecture

### 3.1 Crates
- **Unchanged:** `tobii-protocol`, `tobii-usb`, `tobii-config` (+ Spike S4 adds `enabled_eye` ops to `tobii-usb`).
- **New: `tobii-gtk`** (binary `tobii-gtk`) — GTK4 app. Depends on `gtk4` (gtk4-rs), `glib`, `gio`, `cairo`,
  optionally `gtk4-layer-shell` (overlay) + `librsvg`/`rsvg` (illustrations), plus the workspace core crates.
- **Retired:** `tobii-gui` (egui) — removed once `tobii-gtk` reaches parity; its `device.rs` (device thread) and
  `eyeview.rs` (pure mapping) are **ported** to `tobii-gtk`, not rewritten.

### 3.2 Threading (GTK is main-thread-only)
The blocking `Connection` stays on a dedicated **device thread** (ported from `tobii-gui/src/device.rs`): it runs
the handshake, re-applies the saved display area on connect (the ET5-resets-on-reboot fix), polls gaze into an
`Arc<Mutex<DeviceState>>`, drains a `DeviceCommand` mpsc, and reconnects on error. The **GTK UI thread** reads the
snapshot on a `glib::timeout_add_local(~33 ms)` tick (mirrors egui's 30 fps repaint), updates widgets, and
`queue_draw()`s the eye-view `DrawingArea`s. UI → device requests go through the existing `Sender<DeviceCommand>`
(e.g. `SetDisplayArea(corners)`, later `SetEnabledEye(eye)`). Rationale: identical proven model to the egui build;
no async runtime; the UI never blocks on USB.

### 3.3 App structure
- `TobiiApp` = a `gtk::Application`. The **main window** is the hub (a styled settings panel). Guided flows open as
  **fullscreen `gtk::Window`s** (borderless, maximized/fullscreen), each returning to the hub on Done/Cancel/Esc.
- The **gaze overlay** is a separate `gtk4-layer-shell` window (layer=Overlay, all-anchors, keyboard-mode=None for
  click-through), toggled from the hub.
- **Pure logic reused:** `eyeview` mapping (trackbox → renderable + guidance) and the display-setup planar math
  (`DisplaySetup::to_corners` in `tobii-config`) are unchanged and unit-tested independent of GTK.

## 4. Screens & components

### 4.1 Hub (main window, styled panel)
Sectioned like the original's settings panel:
- **Status**: connected / not-found (udev hint) / reconnecting, from `DeviceState.status`.
- **Live eye-position mini-view**: a `DrawingArea` (cairo) trackbox with both eyes, from `eyeview`.
- **Set up display** — launches §4.2.
- **Position eyes** — launches §4.3.
- **Preview my gaze** — a toggle that shows/hides the §4.5 overlay.
- **Select eyes to detect** — Both / Left only / Right only radios (§4.6; enabled once Spike S4 maps the op).
- **Improve calibration** — present but **disabled** ("B3").

### 4.2 Set-up-display flow (fullscreen; the original's `-S`)
State machine (forward-only, in-flow Cancel/Esc): **DetectMonitor** (confirm auto-detected monitor via EDID;
aspect ratio + a size hint) → **Align** (fullscreen `DrawingArea`: a tracker illustration at the bottom + **two
draggable vertical lines**; drag them to the marks on the physical tracker; live-derive `width_mm`, `offset_x_mm`
per §1 math; clamp lines to `[0.02,0.98]`, min gap `0.05`) → **Save** (compose corners via `DisplaySetup::to_corners`,
send `SetDisplayArea` command, write `config.toml`) → **Confirm** (live trackbox view — "we can see you") → **Done**.
Replaces the egui numeric form; the numeric offset/width become derived, not typed. Mounting angle/tilt uses the
device's fixed geometry (defaults; advanced override optional later).

### 4.3 Eye-position flow (fullscreen; the original's `-P`)
A large trackbox visualization + guidance ("move closer/back", "center yourself"), Done/Esc → hub. Shares the
`DrawingArea` eye-view widget with the hub mini-view and §4.2 Confirm.

### 4.4 Eye-position visualization (shared widget)
A cairo `DrawingArea` drawing the trackbox rectangle + both eyes at normalized `[0,1]` positions, colored by
validity, with a distance readout + centering guidance; "no eyes"/"reconnecting" states from `DeviceState.status`
(so a disconnect never shows stale gaze). Backed by the ported pure `eyeview::EyeView::from_gaze` + `guidance`.

### 4.5 Preview-my-gaze overlay (`gtk4-layer-shell`)
A transparent, click-through, always-on-top layer-shell window spanning the screen; a cairo dot follows the live
2D gaze point (`gaze_point_2d`, col `0x1c`). Toggled from the hub. Mirrors `tobiifree-overlay`. **Prerequisite:**
system `gtk4-layer-shell` + the Rust binding. If unavailable, the toggle is disabled with a clear hint.

### 4.6 Select-eyes control
Radios Both/Left/Right bound to Spike S4's `enabled_eye` op: on connect, GET the current value to seed the control;
on change, SET it and persist host-side (new `tobii-config` field), re-applying on every connect (like display area,
since reboot-persistence is unproven). Enum ordering fixed per S4's finding.

## 5. Spike S4 — map the `enabled_eye` device op (backend prerequisite)
Separate, hardware-driven, in `tobii-usb` (toolkit-agnostic):
1. **GET + subscribe (read-only, safe first):** add `Connection::get_enabled_eye()` using the `enabled_eye`
   property; confirm the **op code** (derive by analogy to display-area GET `0x596`/SET `0x5a0` — likely an adjacent
   platmod-property op — or probe candidates read-only) and the **on-wire enum order**, live on the ET5.
2. **SET (cautious):** add `Connection::set_enabled_eye(eye)`; verify on hardware it actually changes detection
   (validity/trackbox for the disabled eye). If the device rejects standalone SET (only honors it via a calibration
   session), **record the finding and fall the feature back to the B3 calibration route** — do not force it.
3. Treat reboot-persistence like display area: persist host-side, re-apply on connect.
Deliverable: `Connection::{get,set}_enabled_eye` + confirmed op/enum, or a documented "calibration-only" outcome.

## 6. Feature feasibility summary (from verified research)
| Feature | Protocol | Notes |
|---|---|---|
| Set-up-display + visual line-alignment | **existing** `SET 0x5a0` / `GET 0x596` | lines are UI over width+offset_x we already model |
| Preview my gaze | **existing** gaze `0x4c4`/`0x500`, 2D `0x1c` | host-side overlay only |
| Richer hub | **existing** | UI restructure; Improve-calibration gated to B3 |
| Select eyes to detect | **new op** `enabled_eye` (unmapped) | Spike S4; fallback = B3 calibration route |

## 7. Theming
A GTK **CSS** stylesheet (dark background, teal accent, generous spacing, modern typography) loaded via
`gtk::CssProvider` + `style_context_add_provider_for_display`. Tracker/illustration assets as embedded SVG
(librsvg) or cairo-drawn. Aim for the original's feel, not its exact pixels.

## 8. Data flow
Device thread: `connect` → apply saved display area → loop { poll `next_gaze` → update `DeviceState`; drain
`cmd_rx` → `SetDisplayArea`/`SetEnabledEye` via `Connection`; on error → status=Error, reconnect }. UI thread: a
`glib` tick reads `DeviceState`, updates status + `queue_draw()`; flows send `DeviceCommand`s; the overlay reads
the same snapshot.

## 9. Error handling
- Device not found / permission denied → hub banner reusing `UsbError` text (incl. udev hint); the Set-up-display
  geometry stays usable offline (Save disabled until connected); device thread keeps retrying.
- Disconnect mid-flow → flows show a "reconnecting"/error state (never stale gaze); never panic.
- Config save failure → surfaced honestly (mirrors the CLI's `7cba602` honesty fix); never claim "applied" falsely.
- Missing `gtk4-layer-shell` → Preview-my-gaze disabled with a hint; rest of the app unaffected.
- GTK/display init failure → clear stderr message + non-zero exit.

## 10. Testing strategy
- **Unit (no GTK, no hardware):** the flow state machines (transitions, Cancel/Esc/Done); the `eyeview` mapping
  math; the line-alignment ↔ (width, offset_x) conversion (both directions) + clamps; the device-thread
  command/gaze plumbing via the existing `MockTransport`-backed `Connection`; S4's `enabled_eye` payload/enum.
- **Manual/live (ET5):** GTK rendering, fullscreen flows, the drag alignment, the overlay, and end-to-end
  setup + eye-position + select-eyes; capture a golden trackbox frame (deferred B2.1 item).
- Keep GTK draw code thin over the pure, testable logic (widgets read plain structs; math/state machines separate).

## 11. Phasing (informs the plan)
1. **Spike S4** — map `enabled_eye` (GET/subscribe → SET) in `tobii-usb`; live on the ET5. Gates §4.6.
2. **`tobii-gtk` scaffold + theme + device-thread port** — crate, `gtk::Application`, CSS dark/teal, port the
   device thread + `eyeview`; a minimal hub window showing status + live eye-view (glib tick + cairo `DrawingArea`).
3. **Hub sections** — the full styled settings panel + launch buttons + Preview/Select-eyes controls (wired in 5/6).
4. **Set-up-display flow** — DetectMonitor → drag line-alignment → Save → Confirm → Done.
5. **Eye-position flow** + shared eye-view widget polish.
6. **Preview-my-gaze overlay** (`gtk4-layer-shell`).
7. **Select-eyes wiring** to S4's op (+ host persistence).
8. **Retire `tobii-gui`** (remove the egui crate) once parity is reached.
9. **Live validation** on the ET5.

## 12. Risks & open questions
1. **`gtk4-rs` vs GTK 4.22.4** — pick a `gtk4` crate version whose `v4_x` feature matches ≤4.22; confirm at scaffold.
2. **`gtk4-layer-shell` Rust binding** — confirm the crate + that it links the just-installed system lib; the
   overlay is isolated so a problem here doesn't block the app.
3. **`enabled_eye` op** — code unmapped, standalone SET untested, enum order unknown (Spike S4 resolves; safe
   read-only-first; documented fallback to the B3 calibration route).
4. **Wayland fullscreen + layer-shell** behavior on CachyOS — validate during the flow/overlay tasks; fall back to
   borderless-maximized if true-fullscreen misbehaves.
5. **Device↔UI threading** — keep the single `Arc<Mutex<DeviceState>>` snapshot read per tick; no lock across draw.
6. **Scope** — this is large; the phasing keeps each phase independently shippable and testable.
