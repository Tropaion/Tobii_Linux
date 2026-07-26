# Official-Parity Calibration Flow — Phase 3 (Decompiled Ground-Truth Corrections)

## Context

Phase 2 (branch `feat/official-calibration-flow`) restyled the calibration flow's step UX to match 25 captured screenshots of the real Windows software, including a particle-burst "explode" animation on point capture. After merging Phase 2's implementation, live testing surfaced a real functional bug: **dots explode regardless of whether the user's eyes are actually on them.** Root-cause investigation (see conversation history) found that point capture has always been a pure elapsed-time timer (`SETTLE_TICKS` + `DWELL_TICKS`, ~1.53s), never gated on any gaze signal — Phase 2 only added the explosion *visual* on top of this pre-existing timer, making the missing verification far more conspicuous (a "countdown ring" doesn't imply confirmation; an "explosion" does).

This phase fixes that by using the real Windows software's own mechanism as ground truth, decompiled directly from its shipped assembly:

- **`Tobii.Configuration.Common.dll`** (from a Wine-installed copy of Tobii Eye Tracker 5's software already present on this machine at `/home/tropaion/.wine/drive_c/Program Files/Tobii/Tobii EyeX/`) was decompiled with `ilspycmd` (see `tobii-msi-decompiler` project memory for the toolchain). The decompiled C# lives at (session-scratch, not committed — re-decompile if needed for future work):
  `/tmp/claude-1000/.../scratchpad/decompile/out_common/` (path is session-specific; re-run the decompile if this doesn't exist).
- Relevant classes: `Tobii.Configuration.Common.Calibration.PopCalibration.{CalibrationStateManager,CalibrationAssessor,CalibrationPoint,ExponentialSmoothingFilter}`, `Tobii.Configuration.Common.Calibration.ViewModels.{CalibrationProcessViewModel,EyesPositioningViewModel,CalibrationViewModel,CalibrationParameters}`, `Tobii.Configuration.Common.EyePositioning.{EyesPositioningParametersCalculator,EyePositioningUtils}`, and the English strings in `Tobii.Configuration.Common.Localization.LanguageResources.resx`.
- **A live, non-destructive hardware probe** (`tobii cal-probe`, extended this session — see `crates/tobii-cli/src/main.rs`) confirmed the ET5 reports a usable `gaze_point_2d` (with `validity_l`/`validity_r`) during an active calibration session even before that session's own calibration completes: 171/265 sampled frames were valid and the values coherently tracked eye movement across the screen. This is the exact signal (`GazePointOnDisplayNormalizedXy`) the decompiled original consumes — our protocol layer already parses it (`tobii_protocol::gaze::GazeSample::gaze_point_2d`), just never used it during calibration.

Every design decision below was resolved directly with the user (recommended options in each case): fix everything except per-eye calibration (out of scope — its actual device-op mapping lives in an undecompiled assembly and needs separate native reverse-engineering, matching this repo's existing unresolved `et5-enabled-eye-op` memory item), and match the original's slower eye-preview timing exactly rather than keep the current faster feel.

## Global constraints (binding across all tasks, carried over from Phase 1/2 and still true)

- GPL-3.0-only/clean-room: the decompiled C# is a **reference for behavior/values only** — no code is copied verbatim; every Rust implementation is an original clean-room reimplementation of the *algorithm*, in this codebase's own idioms. Do not paste C# into Rust files or comments beyond short factual citations (a constant's value, a formula).
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` must stay clean throughout.
- `tobii_config::decide()` remains the sole branching authority for force/recommend — untouched by this phase.
- GTK/cairo code has no unit-test-coverage precedent in this repo — pure-logic modules get real unit tests; GTK-wiring tasks are gated by build+clippy+manual-checklist (this sandbox has no display/device access — every manual checklist item will be `NOT_RUN`, exactly as in Phases 1-2, and needs the user's own hardware pass).
- `calibrate_flow.rs`/`setup_flow.rs` have a documented history of double-mutable-borrow panics (commit `c8db2c7`; Phase 2's Task 6 fix, `begin_calibration_phase`). Every task touching `Rc<RefCell<Phase>>`/`Rc<RefCell<DotView>>` must be scrutinized for this class of bug, exactly as in Phase 2.
- The `discard_data_2d` (0x438) protocol op used by Task 2 is reverse-engineered from native disassembly (see `et5-calibration-protocol` memory: payload `00 00` + Q42(x) + Q42(y)) but **never live-tested** — treat it as best-effort (a failed discard should not corrupt the session or block progress; log and move on, matching this codebase's existing `enabled_eye` best-effort precedent).

Execute with the `subagent-driven-development` skill: fresh implementer subagent per task, task-scoped review after each, final whole-branch review at the end — same pattern as Phases 1 and 2.

---

## Task 1: Add the `discard_data_2d` (0x438) protocol op

**Files:** `crates/tobii-protocol/src/frame.rs`, `crates/tobii-protocol/src/calibration.rs`, `crates/tobii-usb/src/connection.rs`

Pure logic + protocol wiring, fully unit-tested (mirror the existing `cal_add_point_payload`/`add_calibration_point` pattern exactly).

- `frame.rs`: `pub const OP_CAL_DISCARD_POINT: u32 = 0x438;` beside the other `OP_CAL_*` constants.
- `calibration.rs`: `pub fn cal_discard_point_payload(x: f64, y: f64) -> Vec<u8>` — same `00 00` prefix + Q42(x) + Q42(y) encoding as `cal_add_point_payload`, but **no trailing `eye: u32` field** (per the reverse-engineered payload shape — confirm by reading `cal_add_point_payload`'s exact byte layout first and drop only the last 4 bytes). Unit test: matches the expected byte layout (mirror `cal_add_point_payload`'s existing test).
- `connection.rs`: `pub fn discard_calibration_point(&mut self, x: f64, y: f64) -> Result<(), UsbError>` — mirror `add_calibration_point`'s structure (`request_until` with `CAL_POINT_TIMEOUT`, since this is also a during-collection op), doc-commented as "redo a point" per the reverse-engineered op semantics and explicitly noting it's unverified on real hardware.

**Why:** the gaze-verified capture in Task 3 needs to *reject* a sample if the user's gaze left the target during collection, not just accept it — this is the original's exact behavior (`RemoveCalibrationPointAsync`/discard-and-retry in `CalibrationProcessViewModel.CalibratePointAsync`). Without this op, a lost-focus sample can only be silently accepted (today's behavior) or the whole session aborted (too harsh — a monentary glance away shouldn't fail the whole calibration).

---

## Task 2: Pure focus-detection logic (`focus.rs`)

**Files:** new `crates/tobii-gtk/src/focus.rs`, `crates/tobii-gtk/src/lib.rs` (`pub mod focus;`)

Pure logic, fully unit-tested. Reimplements (clean-room, not copied) the decompiled original's `CalibrationProcessViewModel.GetClosestIndex`/`ClosestDistance`/`ResolveFocusedCalibrationPoint`/`WouldItBeSuitableToChange` logic:

```rust
/// Zone-radius shrink factor (matches the decompiled original's
/// RadiusSizeModifier exactly): the proximity radius around each
/// not-yet-calibrated point is half the closest inter-point spacing,
/// shrunk by this factor so adjacent zones never overlap.
pub const RADIUS_SIZE_MODIFIER: f64 = 0.45;

/// Minimum time a new focus target must hold before it's accepted, to avoid
/// flicker as the gaze point noisily crosses zone boundaries (matches the
/// original's `WouldItBeSuitableToChange`, ~150ms).
pub const FOCUS_CHANGE_DEBOUNCE_TICKS: u32 = 5; // ~150ms at 33ms/tick

/// The proximity radius (normalized units, aspect-ratio-corrected so a
/// non-square screen's x/y distances compare fairly) within which a live
/// gaze point counts as "on" a calibration point.
pub fn zone_radius(points: &[(f64, f64)], aspect: f64) -> f64;

/// Index of the closest not-yet-calibrated point within `zone_radius` of
/// `gaze`, or None if no point qualifies (mirrors `GetClosestIndex`).
/// `calibrated[i]` marks points already captured (excluded from consideration).
pub fn closest_focused_point(
    gaze: (f64, f64),
    points: &[(f64, f64)],
    calibrated: &[bool],
    radius: f64,
    aspect: f64,
) -> Option<usize>;
```

Use aspect-ratio-corrected Euclidean distance (scale the x-delta by `aspect` — screen width/height — before squaring, so the radius is a fair physical-ish distance rather than distorted by a non-square screen) for both `zone_radius`'s internal closest-inter-point-distance computation and `closest_focused_point`'s point-to-gaze distance. Document this deviation from the original (which works in true pixel space) explicitly — an aspect-corrected normalized distance is a reasonable, much simpler approximation given no config change is worth introducing just for this.

Tests: zone radius shrinks correctly with `RADIUS_SIZE_MODIFIER`; `closest_focused_point` returns `None` when gaze is outside every zone; returns the correct index when inside exactly one zone; correctly excludes already-`calibrated` points even if gaze is closest to one of them (falls through to the next-closest uncalibrated point, or `None`); aspect-ratio correction actually changes the result for a non-square `aspect` (a concrete test proving the correction isn't a no-op).

---

## Task 3: Rewire `Collecting` for gaze-verified capture

**Files:** `crates/tobii-gtk/src/calibrate_flow.rs`

GTK wiring (build + clippy + manual checklist; underlying math already tested in Tasks 1-2). **This is the largest, most architecturally significant task in this phase** — read the whole file before starting (it's ~750 lines), and read this task's brief file (`task-3-brief.md`, generated via `scripts/task-brief`) plus the two prior tasks' actual committed code before writing anything.

Replace the pure-timer capture trigger (`SETTLE_TICKS`/`DWELL_TICKS`/`SAMPLE_AT_TICKS`, currently: wait a fixed ~1.53s then blindly send `CalCollect`) with gaze-verified capture, reimplementing (clean-room) the decompiled original's `CalibrationProcessViewModel` state machine:

- Every tick during `Phase::Collecting`, read the live gaze sample (same `state.lock().unwrap().latest_gaze` / `EyeView`-style access already used elsewhere in this file), extract a usable 2D gaze point when `s.has(present::GAZE_2D) && s.validity_l == 0 && s.validity_r == 0` (else treat as "no gaze this frame" — matches the probe's own validity check).
- Track a "focused index" using `focus::closest_focused_point` against the current point set (Task 4 corrects the actual point values used here), debounced via `FOCUS_CHANGE_DEBOUNCE_TICKS` before accepting a *change* of focus (matches `WouldItBeSuitableToChange`/`ResolveFocusedCalibrationPoint`) — the original's `currentFocusedIndex == newClosestIndex` short-circuit (no debounce needed to *re-confirm* the same target) should also carry over.
- When focus lands on the point currently being presented (not a different point — that's a genuine "user looked elsewhere," not a normal saccade toward the shown target): wait `SETTLE_TICKS` (keep as today, ~330ms, matches the original's ~200ms saccade-settle wait closely enough — do not invent a new constant here without reason) for the saccade to land, then send `CalCollect`.
- **After sending `CalCollect`, keep tracking focus while waiting for the device's ack (`cal.collected > index`).** If focus leaves the target point *before* the device acks the collect, send `discard_calibration_point` (Task 1) for that point's coordinates and restart the dwell for the *same* index (do not advance) — this is the original's `FocusChangedDuringCalibration` → `RemoveCalibrationPointAsync` reject-and-retry behavior, and is the actual fix for the reported bug (a point that was captured while the user wasn't looking is no longer silently accepted).
- If gaze is lost entirely (no valid sample) for a sustained period, reset the current point's dwell/focus state (a blink or brief eye closure should not fail the point, but should not let a stale "focused" state persist indefinitely either) — reuse a tick-based interval comparable to the original's 250ms `GazeLeftInterval`.
- Preserve every existing safety property this file already has: session-token gating (`cal.token != token`), the existing `COLLECT_TIMEOUT_TICKS`/`START_TIMEOUT_TICKS` ceilings (a stuck gaze-verification loop must still eventually time out and fail cleanly, exactly like today), and the borrow-safety discipline (every `dot`/`phase` borrow stays a short, non-overlapping scoped block — see Global Constraints).
- The explosion/`fade_in`/`black_bg` visuals from Phase 2's Task 9 are UNTOUCHED by this task — they still trigger on the same "point captured, advancing" transition, just now gated by real focus-verification instead of a blind timer.
- The dynamic instruction text ("Follow the dot..."/"Hold still...") stays as-is (confirmed decision from Phase 2, still binding) — this task changes *when* capture fires, not the displayed text.

Manual checklist: capture a full 7-point session while genuinely looking at each dot in turn — dots should explode only once you've actually looked at them for a moment; deliberately look away from a freshly-arrived dot and confirm it does NOT explode/capture while you're not looking at it, and that looking back at it eventually captures normally; deliberately look at the target then glance away *right as it would capture* and confirm the point retries rather than silently accepting a bad sample; a total gaze loss (close your eyes for a couple seconds) should not crash or silently fail the session.

---

## Task 4: Correct the calibration point layout to ground truth

**Files:** `crates/tobii-gtk/src/calibrate_flow.rs`

Pure constants + existing test pattern — fully unit-tested (same style as Phase 2 Task 7's `full_7_has_correct_layout`).

Replace `FULL_7`'s current values (an inset 0.3/0.5/0.7 grid, measured from screenshot pixel positions — an approximation) with the exact values verified byte-for-byte in the decompiled `CalibrationStateManager`'s constructor default:

```rust
pub const FULL_7: [(f64, f64); 7] = [
    (0.5, 0.5),
    (0.1, 0.9),
    (0.5, 0.1),
    (0.9, 0.9),
    (0.1, 0.1),
    (0.5, 0.9),
    (0.9, 0.1),
];
```

Update the doc comment (it currently says "measured from the captured official screenshots" — correct this to cite the decompiled `CalibrationStateManager` default instead) and the existing `full_7_has_correct_layout` test's expected values/point-order assertions (edge-flush corners + one mid-edge-ish point per row, not the previous inset-grid assertions — read the existing test before rewriting it, since its structure/assertion style should carry over, just with corrected expected values).

---

## Task 5: Match the original's eye-preview timing + debounce exactly

**Files:** `crates/tobii-gtk/src/eye_preview.rs`, `crates/tobii-gtk/src/calibrate_flow.rs` (tick-loop call sites only)

Pure logic (fully unit-tested) + a small GTK-wiring touch-up.

Per the decompiled `EyesPositioningViewModel`/`ProcessUserPositionData`, replace the current constants and add the missing debounce mechanics:

- `EYE_TEXT_TICKS` → the original's `InitialTime` = 6000ms ≈ **182 ticks** at 33ms/tick (was 60).
- `CENTERED_DWELL_TICKS` → the original's `PresenceTime` = 3000ms continuous ≈ **91 ticks** (was 45), but tolerate gaps up to the original's `AbsenceOffset` = 500ms ≈ **15 ticks** without resetting the streak to zero (today's `calibrate_flow.rs` resets `centered_ticks` to 0 on ANY non-`Centered` reading — this task should change that to only reset once the gap has exceeded ~15 ticks, matching `GetAccumulatedPresenceTimeInMs`'s gap-tolerant accumulation).
- Add message-hint debounce: once `Guidance::Centered` has been shown, dropping to any other guidance should require ~11 consecutive non-Centered frames before the message actually changes (matches `MaxCountOfInvalidGazeDataFrom2To3`); recovering the OTHER way (from a settled non-Centered message back toward the "can't track eyes" message) should require ~49 consecutive frames (matches `MaxCountOfInvalidGazeDataFrom2To1`/`...From3To1`). Keep this proportionate to this module's existing simplicity — a single small debounce counter alongside the existing `ticks`/`centered_ticks` state is enough; do not port the original's full multi-`CancellationTokenSource` staggered-message architecture verbatim (that's WPF-specific async plumbing, not a behavior worth reproducing exactly).
- `STUCK_FALLBACK_TICKS` (the "Continue anyway" fallback) is **not** part of the original's own mechanism (it has no analogous escape hatch for the interactive flow) — leave this constant and its behavior exactly as Phase 2 built it; this task does not touch it.

Update the existing tests in `eye_preview.rs` for the new constant values and the new gap-tolerance/debounce behavior (boundary-correct, matching this file's existing test style). Update `calibrate_flow.rs`'s tick-loop `EyePreview` arm only as much as needed to thread the new gap-tolerance counter through (the arm's overall shape — read `ev.guidance`, update a running counter, call into `eye_preview`'s pure functions — stays the same; this is not a rewrite of that arm's structure, just its inputs).

---

## Task 6: Fix the failure screen's wording to ground truth

**Files:** `crates/tobii-gtk/src/calibrate_flow.rs`

GTK wiring (build + clippy + manual checklist) — a small, low-risk, purely textual fix. Verified exact English strings, extracted directly from the decompiled `LanguageResources.resx` (straight ASCII apostrophes, not the resx's typographic ones, to match this file's existing convention):

- Heading: **"Oops. No detection."** (was: "Oops. Nothing found." — an inferred approximation from the screenshot; this is the verified real string, resource key `Calibration_FullScreenMessage_DetectionFailed`).
- Body: **"Sorry about this, but the eye tracker can't detect your eyes. Let's try again and maybe follow these tips:"** (was: "Sorry, the eye tracker can't find your eyes. Let's try again. Here are some tips:"; resource key `CalibrationFailed_Body_TipBody`).
- Tips (resource keys `CalibrationFailed_MultipleChoice_ScreenTip1..4`, in this exact order):
  1. "Keep looking at the dot until it explodes."
  2. "Bright and direct light is no friend of the eye tracker or your eyes. Try to avoid it."
  3. "Remember to relax, it's ok to blink."
  4. "If you're wearing glasses, give them a wipe."
- Button label: **"Try again"** (was: "Retry" — resource key `General_Button_Retry`). Note: the success screen's heading, **"Calibration successful!"**, was independently verified against `General_FullScreenMessage_Completed` and is **already exactly correct** — no change needed there.

Update only the string literals (and the `retry_btn`/`Button::with_label` call) — no layout/CSS/logic changes.

---

## Task 7: Implement real "Improve calibration" seeding

**Files:** `crates/tobii-gtk/src/device.rs`

GTK/device wiring (build + clippy + manual checklist).

Today, `DeviceCommand::CalBegin`'s handler does `start_calibration()` → `clear_calibration()` and nothing else — every calibration session (including ones triggered by the hub's "Improve calibration" button) is functionally a from-scratch calibration; the old blob is discarded with no attempt to seed the new session with it. The decompiled original's `TryStartCalibration` does, in this exact order: retrieve the device's current calibration blob (before touching anything) → `start` → `clear` → if a blob was retrieved, re-apply it (`SetCalibration`) before returning control to point collection.

Reimplement this order in `CalBegin`'s handler:

```rust
DeviceCommand::CalBegin { eye, token } => {
    state.lock().unwrap().calibration = CalPhase::begin(token);
    let _ = conn.set_enabled_eye(eye);

    // Retrieve whatever calibration is currently active BEFORE clearing it,
    // so a successful start+clear below can be re-seeded with it — this is
    // what makes "Improve calibration" (vs. a from-scratch calibration)
    // meaningfully different: new points refine the old calibration instead
    // of replacing it outright. An empty/failed retrieve just means there
    // was nothing to improve on (e.g. true first-ever calibration) — proceed
    // as a plain fresh session in that case, exactly as today.
    let previous_blob = conn.retrieve_calibration().ok().filter(|b| !b.0.is_empty());

    state.lock().unwrap().cal_session_open = true;
    let r = conn
        .start_calibration()
        .and_then(|()| conn.clear_calibration())
        .and_then(|()| match &previous_blob {
            Some(blob) => conn.apply_calibration(&blob.0),
            None => Ok(()),
        })
        .map_err(|e| e.to_string());
    match r {
        Ok(()) => state.lock().unwrap().calibration.on_started(),
        Err(e) => state.lock().unwrap().calibration.on_finish(Err(e)),
    }
}
```

Confirm `retrieve_calibration()` can be called on a fresh (non-calibration-mode) connection — check whether it's already proven to work outside an active session elsewhere in this file (`finish_calibration` calls it after `stop_calibration()`, i.e. also outside an active session, which is evidence this ordering is fine) before assuming; if you find a reason it can't run before `start_calibration()`, adapt the ordering and document why in your report rather than silently deviating.

No new UI/wording changes — this is a pure behavior fix; "Improve calibration" already has its own button/label from before this phase.

---

## Task 8: Remove `fine_tune.rs` entirely

**Files:** delete `crates/tobii-gtk/src/fine_tune.rs`; `crates/tobii-gtk/src/lib.rs` (remove its wiring: `pub mod fine_tune;`, whatever button/menu entry launches it, and any state/handles it owns)

Deletion + cleanup, gated by build + clippy + manual checklist.

The decompiled original has no equivalent per-user manual gaze-offset-correction tool — the only "drag things to align" screen it has (`ScreenPlaneSetup`) is the two-line drag-to-marks step already reimplemented correctly by `setup_flow.rs`'s existing `Align` phase (independently confirmed this session: near-identical wording, `"Move the lines to the marks on top of your eye tracker."` vs. our `"Move the lines to the marks on your eye tracker."`). `fine_tune.rs`'s whole reason for existing — compensating a systematic gaze offset caused by the device plane being built from chord width instead of the correct EDID arc width — was already fixed at the root by the arc-width correction (`docs/superpowers/specs/2026-07-24-official-parity-calibration-flow-design.md`, Component 1). It is now a maintenance-only surface with no reference-flow justification.

- Delete `crates/tobii-gtk/src/fine_tune.rs`.
- Remove `pub mod fine_tune;` from `lib.rs`.
- Find and remove whatever launches it (grep `fine_tune::launch` — likely a hub button/menu item) and any now-dead state that only existed to support it.
- Grep the whole `tobii-gtk` crate afterward for any remaining reference to confirm nothing is left dangling (a stale button pointing at a removed function would be a compile error, so `cargo build` itself is a strong signal here, but double-check for anything merely unused-but-still-compiling, like an orphaned CSS class or doc comment cross-reference).

Manual checklist: the hub no longer shows a "fine-tune"/"adjust alignment" entry; nothing else regresses (build+clippy+existing tests are the primary gate here, given this is a pure removal).

---

## Task 9: Final whole-branch review + workspace gate

**Files:** none new — verification only, matching Phases 1-2's closing task.

1. Full `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo fmt --all -- --check` — all clean.
2. Dispatch the final whole-branch code reviewer (opus, per `requesting-code-review`) against the range from this plan's first commit to HEAD, with the same global constraints as above plus explicit attention to: the gaze-verification rewrite's borrow-safety (Task 3, the highest-risk task this phase), the new protocol op's error handling (Task 1, unverified-on-hardware, must degrade gracefully), and confirming Task 8's removal left no dangling references.
3. End-to-end manual checklist (all `NOT_RUN` in this sandbox, for the user's own hardware pass): a full calibration session where dots only capture when actually looked at and correctly reject-and-retry on a glance-away; "Improve calibration" from the hub measurably builds on an existing calibration rather than behaving identically to a fresh one; the failure screen's wording matches this plan's Task 6 values; Fine-tune alignment is gone from the hub with no other regression.

---

## Verification (applies across all tasks)

- Tasks 1, 2, 4, 5 ship real unit tests, run RED→GREEN per this repo's TDD convention.
- Tasks 3, 6, 7, 8 are GTK/device wiring, gated by `cargo build -p tobii-gtk && cargo clippy -p tobii-gtk --all-targets -- -D warnings` plus each task's manual checklist (no automated coverage precedent for GTK/cairo code in this repo).
- Task 9 closes with the full workspace gate and the end-to-end hardware checklist, matching how Phases 1-2 were closed out.
