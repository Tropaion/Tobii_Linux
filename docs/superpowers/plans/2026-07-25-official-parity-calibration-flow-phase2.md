# Official-Parity Calibration Flow — Phase 2 (Step UX Parity)

## Context

Phase 1 of the official-parity calibration flow (branch `feat/official-calibration-flow`) fixed the underlying accuracy bug (EDID arc width, not chord) and added a `decide()` state machine that forces/recommends (re)calibration — but it deliberately left the actual setup/calibration *screens* as they were: a single non-stepped display-setup form, and a calibration flow with a Quick/Full mode chooser, a plain shrinking-dwell-ring dot, and generic wording.

The user provided 25 screenshots of the real Tobii Windows software's onboarding flow (`docs/TobiiSetupProcess/*.png`) and wants the Linux GTK app's flow, wording, and visual mechanics to closely match it: a screen picker, a posture step, an eye-preview/range-check step, a calibration-dot sequence that "explodes" on capture, and matching success/failure screens — all translated to English. This plan (Phase 2 / Component 4 of the design spec) implements that.

Every open design ambiguity below was resolved directly with the user (see each task's "Confirmed" note) — nothing here is a unilateral guess.

**Key resolved decisions, all confirmed with the user:**
- Calibration point layout switches from today's edge-flush `FULL_9`/`QUICK_5` to a single measured **7-point set** (center + inset top-row-of-3 + inset bottom-row-of-3), matching the captured screenshots.
- **The Quick/Full mode chooser is removed entirely.** The real product has no such picker anywhere in the captured flow — calibration always runs the same full 7-point sequence, both on first-time forced setup and on manual "Improve calibration" touch-ups from the hub. This deletes `Phase::Chooser`, `CalMode::Quick`, and `QUICK_5`.
- The new eye-preview + range-check step runs before **every** calibration (forced or manual), auto-advancing once a stable good position is held, with a "Continue anyway" fallback if eyes are never detected (so the user is never trapped).
- On a failed calibration point, keep today's whole-session-abort behavior (no per-point retry — that was the design spec's original, now-superseded idea), but restyle the failure screen to match the captured "Oops. Nothing found." screen (tips list, Try again / Cancel). "Try again" always restarts directly into point collection — there's no chooser to skip back to anymore.
- The success screen's footer (Tobii's own commercial SDK-licensing disclaimer in the original) is **omitted** — it describes Tobii's business terms, not this clean-room project, and reproducing or paraphrasing it would be misleading.
- The calibration-dot phase's existing **dynamic instruction text is kept** ("Follow the dot with your eyes" → "Hold still — keep looking at the dot", with "point X of Y" progress) rather than replaced by the captured static line — more useful UX, at the cost of exact wording parity for this one line.
- No "Back" button anywhere in the new setup wizard (none of the captured screens show one); Cancel/Esc still closes the whole flow.

All UI text ships in **English** (every German phrase below is a translation reference, not what gets shipped). No Tobii screenshot/icon assets are ever embedded — all new illustrations are original cairo line-art in the existing house style (`setup_flow.rs`'s `rgb`/`diagram_bg`/etc. helpers).

## Architecture

- `setup_flow.rs` gains a `Phase` enum (`ScreenPick → Align → Posture`), modeled on the existing `fine_tune.rs` `Phase`/`refresh_ui` pattern (today it has no phase machine at all — one screen shows everything). `ScreenPick` is skipped automatically when ≤1 monitor is detected (matches the screenshots, which show no picker for the common case).
- `calibrate_flow.rs` gains an `EyePreview` phase before the (now chooser-free) point sequence, the measured 7-point layout, and an explode/crossfade animation on the existing `DotView`/`draw_scene`.
- Two new small pure-logic modules in `tobii-gtk` (`screen_pick.rs`, `eye_preview.rs`) and one new pure module (`particles.rs`) keep the non-GTK logic unit-tested, following the precedent already set by `align.rs`/`eyeview.rs` (pure logic + `#[cfg(test)]`, sitting beside untested GTK/cairo wiring).
- The two-window "setup closes → `decide()` re-evaluates → calibration opens" boundary from Phase 1 (`lib.rs`'s `launch_forced`/`cal_evaluated`/`forced_flow_open`) is **untouched** — this phase only changes what's drawn *inside* each window, not how many windows exist or when they open/close.
- One new sidecar file (`setup_monitor_id`, mirroring the existing `enabled_eye` sidecar) persists the screen-picker's choice, rather than adding a field to `DisplaySetup` (which is `#[derive(Copy)]` and used by value throughout `fine_tune.rs`/`setup_flow.rs` — Phase 1 already declined to touch this for the same reason).

No new external dependencies. The particle burst uses a small deterministic in-file PRNG (same category as `DisplaySetup::fingerprint`'s `DefaultHasher` use), not a `rand` crate.

**Global constraints (still binding, from Phase 1):** GPL-3.0-only/clean-room; never embed `docs/TobiiSetupProcess/*.png` or copy Tobii's exact iconography; preserve verbatim calibration-blob replay (nothing here touches blob bytes); `cargo clippy --workspace --all-targets` and `cargo fmt --all -- --check` must stay clean throughout; do not reintroduce runtime gaze/curvature correction; `tobii_config::decide()` remains the sole branching authority for force/recommend — this phase is UI/UX only. GTK/cairo code is not unit-testable in this repo (confirmed precedent) — every task states whether it's pure-logic-with-tests or GTK-wiring-gated-by-build+clippy+manual-checklist.

Design spec: `docs/superpowers/specs/2026-07-24-official-parity-calibration-flow-design.md` ("Component 4"). Prior plan for context/format: `docs/superpowers/plans/2026-07-24-official-parity-calibration-flow-phase1.md`.

Execute with the `subagent-driven-development` skill: fresh implementer subagent per task, task-scoped review between tasks, final whole-branch review at the end, exactly as Phase 1 was executed.

---

## Task 1: Persist the setup-time chosen monitor id (sidecar)

**Files:** `crates/tobii-config/src/store.rs`, `crates/tobii-config/src/lib.rs`

Pure logic, fully unit-tested. Add `save_setup_monitor_id_to`/`load_setup_monitor_id_from` (path-parameterized) plus `save_setup_monitor_id`/`load_setup_monitor_id` (default-path wrappers), mirroring the existing `enabled_eye_path`/`save_enabled_eye`/`load_enabled_eye` sidecar pattern in the same file. Store as plain UTF-8 (no TOML), written atomically via the existing `write_atomic` helper; `None` removes the file rather than writing empty. Tests: round-trip, missing-file-is-`None`, saving-`None`-clears-the-file (3 tests, `std::env::temp_dir()` idiom matching existing tests in this file).

Export both default-path functions from `lib.rs`.

**Why a sidecar, not a `DisplaySetup` field:** `DisplaySetup` is `#[derive(Copy)]` and used by value throughout `fine_tune.rs`/`setup_flow.rs`/`device.rs`; adding a `String` field breaks `Copy` for every call site — the exact churn Phase 1's Task 6 declined for the same reason. A sidecar file (like `calibration.meta.toml` and `enabled_eye` already are) is strictly additive and touches no existing struct.

---

## Task 2: `active_monitor_id()` prefers the persisted setup choice

**Files:** `crates/tobii-gtk/src/device.rs`

GTK wiring (build + clippy gate; underlying store functions already tested in Task 1). `active_monitor_id()` (used by both `finish_calibration`'s `CalMeta` construction and `lib.rs`'s `decide()` evaluation) tries `tobii_config::load_setup_monitor_id()` first; falls back to today's `pick_monitor(detect_monitors())` largest-by-area heuristic when nothing was persisted (covers installs from before the picker existed, and the common single-monitor case where nothing needs picking).

---

## Task 3: Screen-picker pure logic (`screen_pick.rs`)

**Files:** new `crates/tobii-gtk/src/screen_pick.rs`, `crates/tobii-gtk/src/lib.rs` (`pub mod screen_pick;`)

Pure logic, fully unit-tested.

```rust
pub fn should_show_picker(monitors: &[MonitorInfo]) -> bool { monitors.len() > 1 }

pub fn monitor_label(m: &MonitorInfo) -> String {
    // model, else connector, else EDID id, else "Unknown display" — always something clickable.
}
```

Tests: zero/one monitor never shows a picker; two-or-more does; label preference order (model → connector → id → generic fallback).

---

## Task 4: Restructure `setup_flow.rs` into a step wizard (ScreenPick → Align → Posture)

**Files:** `crates/tobii-gtk/src/setup_flow.rs`

GTK wiring (build + clippy + manual checklist).

```rust
enum Phase { ScreenPick, Align, Posture }
```

- `ScreenPick`: shown only if `screen_pick::should_show_picker`; a vertical list of one button per monitor (`screen_pick::monitor_label`), heading "Which screen is your eye tracker connected to?". Picking one (or auto-resolving when ≤1 monitor exists) seeds `width_mm`/`height_mm` exactly as today's unconditional `pick_monitor` seeding does, then advances to `Align`.
- `Align`: today's existing drag-two-lines screen, reused as-is (including the "Show advanced" toggle, kept as a toggle *within* this step — it isn't in any captured screenshot, so it doesn't need its own wizard step). Instruction text: "Move the lines to the marks on your eye tracker." Button relabeled from "Apply & save" to "Done" — it now only advances to `Posture`, no device/config writes yet.
- `Posture` (new): instruction "Sit up straight in front of the screen.", an original static line-art illustration (seated-person silhouette + teal gaze-frustum lines to a monitor, built the same way as `setup_flow.rs`'s existing `diagram_*` cairo helpers, scaled to fill the screen). Its "Done" button does what Align's button used to: `tobii_config::save`, push `DeviceCommand::SetDisplayArea`, `tobii_config::save_setup_monitor_id(...)` (the id chosen/resolved in `ScreenPick`), then close. Same failed-save handling as today (stay open, show the warning).
- No Back button anywhere; a single persistent Cancel (+ existing Esc) closes the whole wizard from any step.

Manual checklist: single-monitor skips straight to Align; multi-monitor picker works and seeds the right geometry; Align→Posture→Done persists everything and closes; Cancel/Esc at any step writes nothing; "Show advanced" still works.

---

## Task 5: Eye-preview + range-check pure dwell/advance logic (`eye_preview.rs`)

**Files:** new `crates/tobii-gtk/src/eye_preview.rs`, `crates/tobii-gtk/src/lib.rs` (`pub mod eye_preview;`)

Pure logic, fully unit-tested. Consumes `crate::eyeview::Guidance`.

```rust
pub const EYE_TEXT_TICKS: u32 = 60;       // ~2s: "These are your eyes..." regardless of guidance
pub const CENTERED_DWELL_TICKS: u32 = 45; // ~1.5s of continuous Guidance::Centered to auto-advance
pub const STUCK_FALLBACK_TICKS: u32 = 450; // ~15s before offering "Continue anyway"

pub fn message(ticks: u32, guidance: Guidance) -> &'static str;
pub fn should_advance(centered_ticks: u32) -> bool;
pub fn should_offer_fallback(ticks: u32) -> bool;
```

`message`: for `ticks < EYE_TEXT_TICKS`, always "These are your eyes..."; after that, switches on `guidance` — `MoveCloser` → "Move closer.", `MoveBack` → "Move back a little.", `NoEyes` → "We can't find your eyes. Sit in front of the screen.", `OffCenter`/`Centered` → "Find out how much room you have to move." Tests cover each branch plus the dwell/fallback thresholds (4 tests).

(33 ms tick cadence, matching every other flow in this app; constants are reasonable estimates, not measured from the screenshots — cheap to retune later since each is a single named constant.)

---

## Task 6: Fold eye-preview into `calibrate_flow.rs`; remove the Quick/Full chooser

**Files:** `crates/tobii-gtk/src/calibrate_flow.rs`, `crates/tobii-gtk/src/lib.rs`

GTK wiring (build + clippy + manual checklist).

- Remove `Phase::Chooser` and its UI (the `chooser` box, Quick/Full buttons). `Phase` gains a leading variant:
  ```rust
  enum Phase {
      EyePreview { ticks: u32, centered_ticks: u32 },
      Starting { mode: CalMode, token: u64, ticks: u32 },
      Collecting { .. }, // unchanged
      Computing { .. },  // unchanged
      Done(Result<(), String>), // unchanged — no distinct "mode" needed once there's only one mode
  }
  ```
  `launch()` now **always** starts at `Phase::EyePreview { ticks: 0, centered_ticks: 0 }`; there is no entry-mode distinction to make (`launch`'s signature is unchanged: `launch(app, state, cmd_tx) -> ApplicationWindow`).
- Tick loop for `EyePreview`: increments `ticks` every frame; increments `centered_ticks` only while the live `EyeView::from_gaze(...).guidance == Guidance::Centered` holds (any other guidance resets it to 0 — a stable dwell, not a one-off reading). Renders via `eye_preview::message(ticks, guidance)` over `widget::draw_eye_view` in a small centered panel, on the existing dark-teal background `(0.08, 0.09, 0.11)` (the "fully black" background is new for the point-sequence phases only — Task 9).
- Advances (via one shared closure) to `Starting` — mints a token, sends `CalBegin`, exactly like today's chooser-click handler did — either when `eye_preview::should_advance(centered_ticks)` fires, or via a "Continue anyway" button that only becomes visible once `eye_preview::should_offer_fallback(ticks)` is true (absent for the first ~15s, matching the buttonless screenshots — this is the no-trap fallback).
- Update the 3 `lib.rs` call sites (`CalAction::ForceCalibration`, the hub's `b_cal` button, and the recommend-banner's "Recalibrate" button) — all three keep calling `calibrate_flow::launch(app, state.clone(), cmd_tx.clone())` unchanged (no new parameter needed, since there's no longer a forced/manual distinction to make inside this file).

Manual checklist: eye-preview shows on every entry (forced, manual, banner-recalibrate); a good stable position auto-advances after ~1.5s straight into point collection with no chooser ever appearing; covering the camera/standing out of range shows "Continue anyway" after ~15s and it works.

---

## Task 7: The measured 7-point layout; delete the Quick mode

**Files:** `crates/tobii-gtk/src/calibrate_flow.rs`

Pure constants + existing test pattern — fully unit-tested (same style as today's `point_sets_have_expected_counts_and_start_centered`/`all_points_are_within_unit_square`).

Now that nothing selects Quick (Task 6 removed the only UI that could), delete `CalMode::Quick` and `QUICK_5`, collapsing `CalMode` to a single variant, and rename/replace `FULL_9` with the measured layout:

```rust
/// The 7-point calibration layout (measured from the captured official
/// screenshots): center, then a row of 3 across the top inset from the
/// edges, then a row of 3 across the bottom inset from the edges. Order:
/// center first (matches the captured flow's first visible dot), then top
/// row left-to-right, then bottom row left-to-right.
pub const FULL_7: [(f64, f64); 7] = [
    (0.5, 0.5),
    (0.3, 0.1), (0.5, 0.1), (0.7, 0.1),
    (0.3, 0.9), (0.5, 0.9), (0.7, 0.9),
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CalMode { Full }
impl CalMode {
    pub fn points(self) -> &'static [(f64, f64)] { &FULL_7 }
    pub fn label(self) -> &'static str { "full" } // unchanged consumer: device.rs's CalMeta.mode
}
```

Update the existing point-set tests in place (count changes from 9→7; add an explicit top-row/bottom-row assertion). `CalMeta.mode`'s consumer in `device.rs` (`CalFinish { mode: String }`, from Phase 1's Task 6) is untouched — `CalMode::Full.label()` still produces `"full"`.

---

## Task 8: Particle-burst pure module (`particles.rs`)

**Files:** new `crates/tobii-gtk/src/particles.rs`, `crates/tobii-gtk/src/lib.rs` (`pub mod particles;`)

Pure logic, fully unit-tested. No new dependency — a small deterministic splitmix64-style step function (same category as `DisplaySetup::fingerprint`'s `DefaultHasher` use).

```rust
pub const PARTICLE_COUNT: usize = 10;
pub struct Particle { pub angle: f64, pub speed: f64, pub size: f64 }
pub fn burst(seed: u64) -> [Particle; PARTICLE_COUNT];
pub fn particle_pos(t: f64, p: Particle) -> (f64, f64, f64); // (dx, dy, alpha), t in [0,1]
```

`burst` is deterministic (same seed ⇒ identical output — needed for testability and so a retried point's burst still looks stable). `particle_pos` is pure geometry: outward radial motion scaled by `t`, alpha ramping 1→0. Tests: determinism, different seeds differ, particles stay within documented ranges, position starts at origin/full-alpha and ends faded, particles move monotonically outward (5 tests).

---

## Task 9: Wire the explode animation + crossfade into `DotView`/`draw_scene`

**Files:** `crates/tobii-gtk/src/calibrate_flow.rs`

GTK/cairo wiring (build + clippy + manual checklist; underlying particle math already tested in Task 8).

- `DotView` gains a `fade_in: f64` (new dot's own fade-in, ramping over the existing `SETTLE_TICKS`) and an `explosion: Option<Explosion>` (the *previous* point's still-animating burst, drawn concurrently with the new point fading in — this is the "brief overlap" visible in the captured screenshots, a soft crossfade rather than a hard cut). `Explosion { origin: (f64, f64), seed: u64, age_ticks: u32 }`; retired once `age_ticks >= EXPLODE_DURATION_TICKS` (`~18` ticks, ~0.6s).
- `draw_scene` gains a `black_bg: bool` parameter: `true` from `Starting` onward (matches "fully black background" for the dot phases), `false` for `EyePreview` (still the existing dark-teal). Draw order: background → burst particles (if any) → the dwell ring/dot (unchanged shrinking-ring mechanic from today, just multiplied by `fade_in` so it visibly grows in).
- On point-capture (existing "advance to next point" branch in the `Collecting` tick arm): spawn an `Explosion` at the just-captured point (`seed = token ^ index`), reset `fade_in`/`progress` for the new point to 0.
- **The existing dynamic instruction text ("Follow the dot with your eyes" → "Hold still — keep looking at the dot", with "point X of Y") is kept as-is** — confirmed with the user; do not replace it with static wording, even though it diverges from the captured screen's single fixed line.

Manual checklist: background is pure black from the first dot onward, dark-teal before it; capturing a point shows a visible burst that fades in well under a second; the next point visibly fades/grows in while the previous burst is still finishing; no crash/panic on the last point's capture (no "next point" to fade in).

---

## Task 10: Restyled failure screen (tips list, Try again / Cancel)

**Files:** `crates/tobii-gtk/src/calibrate_flow.rs`, `crates/tobii-gtk/src/lib.rs` (CSS)

GTK wiring (build + clippy + manual checklist).

- Failure body (English translation of the captured screen): heading **"Oops. Nothing found."**, body **"Sorry, the eye tracker can't find your eyes. Let's try again. Here are some tips:"**, tips list:
  - "Look at the point until it explodes."
  - "If you wear glasses, please clean them."
  - "Avoid bright, direct light for the tracker and your eyes."
  - "Relax, you're allowed to blink."

  Buttons stay "Retry"/"Cancel" (existing widgets). Add two small CSS classes to `lib.rs`'s `CSS` const for the heading/tips styling.
- **Retry always restarts directly into point collection** (mints a token, sends `CalBegin`, goes to `Phase::Starting`) — there's no chooser to skip back to anymore (Task 6/7 removed it), so this is simply the only behavior, not a conditional one.
- No per-point retry (design spec's original idea, superseded) — a single point failure still aborts the whole session, exactly as today; only the failure screen's presentation and the retry button's destination change.

Manual checklist: force a failure (e.g. cover the camera mid-collection); confirm heading/body/tips/buttons match; "Try again" goes straight back into point collection.

---

## Task 11: Restyled success screen

**Files:** `crates/tobii-gtk/src/calibrate_flow.rs`, `crates/tobii-gtk/src/lib.rs` (CSS)

GTK wiring (build + clippy + manual checklist).

Heading changes from "Calibration complete." to **"Calibration successful!"**, centered, larger/bold style, on the black background (already the case from Task 9 onward). **No footer text** — confirmed with the user: the captured footer is Tobii's own commercial SDK-licensing disclaimer, not applicable to this project, and is omitted rather than reproduced or paraphrased.

Manual checklist: complete a calibration successfully; confirm the black background + centered "Calibration successful!" wording, no footer.

---

## Task 12: Final wiring sanity pass + end-to-end manual checklist

**Files:** none new — verification only over Tasks 4-6.

Build + clippy + manual checklist only.

1. Re-confirm `lib.rs`'s `launch_forced`/`cal_evaluated`/`forced_flow_open` chaining (Phase 1, commit `e5e4759`) is untouched — both flows still return the same `ApplicationWindow` type and fire `connect_close_request` the same way; only their *internal* step count changed.
2. Full end-to-end manual run on a fresh profile (no `config.toml`, no `calibration.bin`):
   - `ForceSetup` opens: screen-pick (if multi-monitor) → align → posture → Done closes the window.
   - Next tick: `decide()` re-fires → `ForceCalibration` opens: eye-preview auto-advances → 7-point sequence with explode/crossfade → success screen → Done closes.
   - Reconnect: silent (no prompt).
   - "Improve calibration" from the hub: eye-preview → same 7-point sequence (no chooser) → success/failure as before.
   - Force a mid-run failure → restyled failure screen → "Try again" restarts directly into points.
   - **Accuracy**: gaze at the screen borders remains materially better than the pre-Phase-1 baseline (carried over from Phase 1's own unresolved hardware checklist — verify once here since this is the last touchpoint before the branch is considered done).
3. `cargo build --workspace && cargo clippy --workspace --all-targets && cargo test --workspace && cargo fmt --all -- --check` — all clean.

---

## Verification (applies across all tasks)

- Each pure-logic task (1, 3, 5, 7, 8) ships with real unit tests (`cargo test -p tobii-config` / `cargo test -p tobii-gtk`), run RED→GREEN per the repo's existing TDD convention.
- Each GTK-wiring task (2, 4, 6, 9, 10, 11) is gated by `cargo build -p tobii-gtk && cargo clippy -p tobii-gtk --all-targets`, plus the manual checklist listed in that task (GTK/cairo code has no automated test coverage in this repo — confirmed existing precedent).
- Task 12 closes with the full workspace gate (`build`, `clippy --workspace --all-targets`, `test --workspace`, `fmt --all -- --check`) and the end-to-end hardware checklist, matching how Phase 1 was closed out.
