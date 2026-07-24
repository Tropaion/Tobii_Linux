# Official-parity calibration flow — design

Date: 2026-07-24
Status: draft for review
Scope owner: TobiiLinux (clean-room Linux driver + GTK4 GUI for the Tobii ET5)

## Context

Gaze accuracy on Linux is good at screen centre but **degrades toward the sides and is
wrong at the borders**, the error growing with horizontal gaze angle
(`docs/session-handoff-windows-vm.md:23-25`). Separately, the calibration *experience* is
nothing like Tobii's: the GUI has no calibration-state awareness — every flow is a manual
button and **nothing is triggered on connect** (`crates/tobii-gtk/src/lib.rs:228-344`,
`:405-443`). The user wants the Linux experience to match the official software "as far as
possible, even the UX flow": force a full calibration when none exists, and recommend
recalibration when the stored calibration doesn't match the active screen.

Two things now make this achievable without any Windows dependency:

1. **The calibration model is computed on the ET5 device** (op `0x42f`), driven entirely
   from Linux already (`device.rs` start→collect→compute→retrieve→save). So a Linux-only
   calibration is inherently possible — the device produces the same per-user model whether
   driven by Windows or Linux.
2. **We captured Tobii's own display-area geometry for this exact monitor** — the
   `SET_DISPLAY_AREA` corner triple the handoff called the "highest value … this alone may
   solve the remaining side-error" ground truth (`session-handoff-windows-vm.md:114-119`).
   It is decoded and committed here as a golden test vector.

**Intended outcome:** a Linux calibration flow that (a) produces gaze accuracy matching the
official software, and (b) mirrors the official *behaviour* — force vs recommend — and, in a
second phase, the *step UX*.

## Root cause of the accuracy problem (settled by ground truth)

The device projects gaze onto the flat plane the host sends via `set_display_area` (op
`0x5a0`). For a curved panel the plane width must be the EDID **arc** width, not the chord:

- Tobii's captured plane (`screenplane.setpm`) decodes to width **1193 mm** (= EDID arc),
  height 335.5, tilt 20°, offset_x 0, offset_y 10.27, offset_z −3.10.
- The GTK setup path instead converts arc→chord and sends **1171.3 mm**
  (`crates/tobii-gtk/src/setup_flow.rs:446-450` via `chord_from_arc`). A plane ~22 mm too
  narrow compresses gaze ~1.9 % horizontally → ~11 mm error at the far edge, ~0 at centre —
  exactly the reported symptom.
- `DisplaySetup::to_corners()` math is **correct**; only its width *input* is wrong. The
  fine-tuned `offset_x=5.83`/`offset_y=43.75` were compensating for the width error (they
  recentre but cannot fix edge-growth).
- The `session-handoff` note listing "arc … ruled out" (`:44`) was a guess made **without**
  this ground truth; Tobii's own plane uses the arc. This does **not** reintroduce the
  removed runtime curvature correction (`overlay.rs` / `session-handoff:36-43`) — the device
  still gets a flat plane; we only size it correctly and let calibration absorb the residual.
- **The CLI is already correct**: `crates/tobii-cli/src/main.rs:789` seeds width from
  `MonitorInfo.width_mm` (arc) with no chord conversion. The fix aligns GTK to the CLI.

## Goals / non-goals

**Goals**
- Send the geometrically-correct display plane (arc width) so device-side calibration is
  accurate to the edges — validated against Tobii's captured corners.
- Add calibration-state awareness: bind a calibration to the monitor it was made for, and on
  connect **force** calibration when missing / **recommend** it when the monitor differs.
- Match the official step UX (screen pick → tracker align → posture → eye preview →
  calibration dots that "explode" on capture with retry → success/error).

**Non-goals (YAGNI / later)**
- No runtime gaze/curvature post-correction (stays removed).
- No per-point calibration-quality readout via `0x460` (not implemented anywhere today;
  optional future).
- No multi-tracker / multi-device support.
- No decryption of Tobii's neural models (unrelated; see `docs/windows-headpose-findings.md`).

## Current state (anchored)

- **Triggers:** none automatic. Hub buttons launch `setup_flow::launch` (`lib.rs:228-233`),
  `calibrate_flow::launch` (`lib.rs:296-320`), `fine_tune::launch` (`lib.rs:322-344`).
- **Calibration persistence:** raw blob only, **no metadata**, `calibration.bin` beside
  `config.toml` (`crates/tobii-config/src/store.rs:52-95`); re-applied on every connect
  (`device.rs:275-283`) because the ET5 wipes it on reboot; saved by `finish_calibration`
  (`device.rs:329-345`).
- **Monitor identity:** `MonitorInfo { model, width_mm, height_mm }` only
  (`crates/tobii-config/src/edid.rs:10-15`); **no stable id/serial**, DRM connector name is
  discarded (`edid.rs:83`); `pick_monitor` = largest-by-area (`edid.rs:69-74`); no "this
  screen" picker; **no calibration↔monitor binding anywhere**.
- **Geometry seeding:** GTK `chord_from_arc` at `setup_flow.rs:448` (+ `on_curve` recompute
  `:766`); `default_setup` `setup_flow.rs:39-49`; CLI defaults `main.rs:782-801` (uses arc).
- **Calibrate flow:** single pass, `QUICK_5`/`FULL_9` (`calibrate_flow.rs:29-40`), dwell
  ~1.2 s (`:58-86`), one `CalCollect`→`add_calibration_point(x,y,0)` per point
  (`:465-466`, `device.rs:179-184`); **any per-point error aborts the whole session**
  (`calibrate_flow.rs:434-438`); only a manual whole-session Retry (`:319`); `0x460` unused.
- **Config:** fixed `[display]` TOML, **no version field**; only soft-default is
  `curvature_radius_mm` (`setup.rs:123-171`).
- **Tests:** inline `#[cfg(test)]`; fixtures via `include_bytes!("testdata/…")` beside the
  source (`edid.rs:99`, `calibration.rs:101`); dir `crates/<crate>/src/testdata/`.

## Design

### Component 1 — Geometry: send the arc width (tobii-config + tobii-gtk)

- Redefine `DisplaySetup.width_mm` doc as "active-area (arc/plane) width the device projects
  gaze across; for a flat panel arc == chord" (`setup.rs:14-23`). Math in `to_corners` /
  `from_corners` is unchanged.
- GTK: seed `initial.width_mm = m.width_mm` (arc) directly at `setup_flow.rs:448`; drop the
  `chord_from_arc` call there and the chord recompute in `on_curve` (`:766`). Curvature
  radius becomes metadata only (it already never reaches the device).
- Keep `chord_from_arc`/`arc_from_chord` (public, tested) but they are no longer applied to
  the device plane; update the `odyssey_g93sc_arc_to_chord` test (`setup.rs:308-316`) to
  assert the *plane* uses the arc (1193) for this panel while retaining the helper-math
  round-trip tests.
- After any geometry change the stored calibration is stale → the state machine (Component 3)
  treats a geometry change as invalidating and recommends recalibration.

### Component 2 — Monitor identity + calibration binding (tobii-config)

- Extend EDID parsing to expose a **stable monitor id** = manufacturer (bytes 8-9, PnP
  3-letter) + product code (bytes 10-11) + serial (bytes 12-15, and/or 0xFF descriptor),
  formatted like Tobii's `eId` (e.g. `SAM7454-<serial>`). Add `id: MonitorId` to
  `MonitorInfo`; also capture the DRM connector name (`edid.rs:83`) as a fallback key.
- Persist calibration metadata beside the blob: new `calibration.meta.toml` (keeps
  `calibration.bin` byte-verbatim for replay) holding `{ monitor_id, created_utc,
  mode(quick|full), display_fingerprint }`, where `display_fingerprint` is a hash of the
  `[display]` geometry in force at calibration time (so a later geometry edit is detectable).
- `save_calibration` gains a metadata argument; `load_calibration` returns blob + optional
  meta (missing meta ⇒ legacy/unbound). Store API in `store.rs`.

### Component 3 — Calibration state machine + triggers (tobii-config, pure)

A pure decision function (unit-testable, no GTK):

```
fn decide(display_configured: bool,
          cal: Option<CalMeta>,
          active_monitor: Option<MonitorId>,
          current_display_fingerprint: u64) -> CalAction
```

| Situation | `CalAction` | GUI behaviour |
|---|---|---|
| no display setup | `ForceSetup` | full-screen setup, non-dismissible |
| display set, no calibration (or no meta) | `ForceCalibration` | full-screen calibration, non-dismissible |
| calibration exists, `monitor_id` ≠ active | `RecommendCalibration{reason: OtherScreen}` | dismissible banner + highlighted button; existing blob still applied |
| calibration exists, `display_fingerprint` ≠ current | `RecommendCalibration{reason: GeometryChanged}` | dismissible banner |
| calibration valid for active monitor & geometry | `None` | apply silently (already done on connect) |

Wiring (GTK, minimal in Phase 1): compute `active_monitor` from the tracker's monitor (chosen
at setup; stored as `monitor_id` in `[display]`), evaluate `decide` when the device connects
(`device.rs` connect block / a new hub event), and drive the hub: `Force*` launches the flow
modally on the target screen and blocks hub use until done; `Recommend*` shows a dismissible
banner. Silent case keeps the current `device.rs:275-283` re-apply.

### Component 4 — Step UX parity (Phase 2, tobii-gtk)

Match the captured screenshots (`docs/TobiiSetupProcess/`):
- **Screen pick** ("Diesen Bildschirm") — associate tracker with a monitor, store its
  `monitor_id`. (New; today `pick_monitor` is largest-by-area.)
- **Tracker-line align** — existing `setup_flow`/`align.rs` (`Bewegen Sie die Linien…`).
- **Posture guide** ("Sitzen Sie aufrecht") — new static step.
- **Eye preview** ("Das sind Ihre Augen") — existing `eyeview`.
- **Calibration dots** — animate/"explode" on capture; **per-point retry on "no eyes"**
  instead of aborting the session: change `calibrate_flow.rs:434-438` + `device.rs:179-184`
  to re-collect the failed point up to N times, matching "Ups. Nichts gefunden → Erneut
  versuchen." Keep `QUICK_5`/`FULL_9` for now.
- **Success/error** screens matching wording.

### Component 5 — Ground-truth golden test (tobii-config)

- Commit `crates/tobii-config/src/testdata/screenplane.setpm` (48 B — the user's own monitor
  geometry, not Tobii IP).
- Add a `.setpm` decoder: header `{u32 version, u32 count, u32 payload_len}` + 9× `f32` →
  three corners → `DisplayCorners` (BL, TL, TR by geometry). Small fn in `setup.rs` (or
  `setpm.rs`).
- Golden test (inline in `setup.rs`): `from_corners(decoded) ≈ {1193.0, 335.5, 20.0, 0.0,
  10.27, −3.10}` (tight tol); and the GTK seeding for the Odyssey EDID (arc width + current
  defaults) yields corners within ~5 mm / 0.5° of the decoded Tobii corners — proving the
  width fix lands on Tobii's plane and blocking a regression to chord.

## Data model / interfaces

- `MonitorId` (new, `tobii-config`): opaque, `Display`/`Eq`/`Hash`, derived from EDID.
- `CalMeta { monitor_id: MonitorId, created_utc: i64, mode: CalMode, display_fingerprint: u64 }`.
- `CalAction { None | ForceSetup | ForceCalibration | RecommendCalibration{reason} }`.
- Store: `save_calibration(blob, &CalMeta)`, `load_calibration() -> Option<(Vec<u8>, Option<CalMeta>)>`.
- `[display]` gains `monitor_id` (string) — soft-defaulted absent (legacy configs still load,
  like `curvature_radius_mm`).

## Error handling

- Missing/garbled `calibration.meta.toml` ⇒ treat as unbound (legacy) ⇒ `RecommendCalibration`
  (never crash; the raw blob still applies).
- EDID unreadable / no stable id ⇒ fall back to DRM connector name, then to
  largest-by-area; if still ambiguous, `monitor_id` is `Unknown` and mismatch checks are
  skipped (never force spuriously).
- Per-point calibration failure ⇒ retry up to N (Phase 2); exhausted ⇒ existing abort path.

## Phasing

- **Phase 1 (brain, mostly `tobii-config`, heavily unit-tested):** Component 1 (arc width),
  Component 2 (monitor id + binding), Component 3 (`decide` + minimal GTK wiring to
  force/recommend), Component 5 (golden test). Delivers the accuracy fix **and** the
  force/recommend behaviour the user described.
- **Phase 2 (polish, `tobii-gtk`):** Component 4 (full step/visual parity, per-point retry).

## Migration

No config version exists. Existing installs have a chord `width_mm` (1171.3), fine-tuned
offsets, and an unbound `calibration.bin`. On upgrade: the absent `calibration.meta.toml`
makes `decide` return `RecommendCalibration` (or `ForceCalibration` if we also detect the
legacy chord width via a one-time `[display] monitor_id` absence), prompting the user through
setup (which now seeds the arc width) + recalibration. No silent geometry rewrite; the user
re-runs the corrected flow. A `monitor_id` key absence is the migration hook.

## Testing

- Pure unit tests for `decide` across the full state table (Component 3).
- EDID id extraction against the existing `odyssey-g93sc.edid` fixture (`edid.rs`), asserting
  a stable id.
- Golden `.setpm` corner test (Component 5).
- Store round-trip: blob + meta save/load, legacy (meta-absent) load.
- Geometry: arc width seeding for a curved panel; flat panel unchanged (arc == chord).
- On-hardware (manual, per the repo's "verify on HW" rule): after arc-width + recalibrate,
  confirm border accuracy improves; optionally quantify residual-vs-angle with a gaze-vs-
  target capture (sibling of `C:\tobii-extract\capture_headpose.py`).

## Risks / open questions

- **Reverses a prior deliberate decision** (chord). Mitigated by Tobii's ground truth + the
  golden test + on-HW verification. Recommend confirming edge accuracy on hardware before
  removing the chord path entirely.
- **Monitor id stability** across cable/port changes and identical-model multi-monitor
  setups: serial (0xFF) improves uniqueness but isn't always populated; connector-name
  fallback is port-specific. Acceptable for the single-tracker case; documented.
- **"Active screen" definition** in multi-monitor Linux: Phase 1 uses the tracker's setup
  monitor; revisit if users run the GUI across monitors.
- **`correct_gaze_x`** is referenced in comments but does not exist
  (`lib.rs:9-12`/`calibrate_flow.rs:22-28`); unrelated to this work — leave as a separate
  doc-cleanup.
