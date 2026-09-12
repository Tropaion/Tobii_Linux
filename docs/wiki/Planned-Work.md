# Planned work

What is queued, why it is worth doing, and what each item rests on. Nothing here
is needed for a release that has already shipped; this is the list a contributor
can pick from.

Each item says what to change, where, what it costs, and how it could go wrong.
Where a number appears, it is one this project measured — not one read from
another project.

**Three of the four numbered items have since been built** — 1, 2 and 4 — and
each keeps its original reasoning here with what actually shipped, what was
deliberately left out, and where the shipped behaviour differs from what this
page proposed. Item 3 is the one still open. None of the three has been run
against a tracker; what that leaves untested is listed in
[Quality-and-Risks](Quality-and-Risks.md) §11.3d.

## How this list came about

Most of it came from comparing this project against
[Ridoc/Tobii5-for-OpenTrack-Linux](https://github.com/Ridoc/Tobii5-for-OpenTrack-Linux)
in September 2026. That project is a GPL-3.0 fork of
[`Aetherall/tobiifree`](https://github.com/Aetherall/tobiifree), the reference
this project already credits, and its driver, SDK and udev rule are byte-for-byte
upstream — so it contributed **no protocol knowledge**. What it does have is a
head-pose *feel* layer, and the four items below are the ideas worth having from
it. None of its code is used here, and none was copied.

Two things from that comparison are settled and need no work:

- Their calibration sends points to op `0x408` and computes with `0x42F`, with
  the eye argument `0`. This project measured that combination as a silent no-op
  on 2026-08-15 — the device acknowledges the points and discards them
  ([Op-Catalog](Op-Catalog.md), `crates/tobii-protocol/src/calibration.rs`). It
  is upstream's error, not theirs, and there is nothing to adopt.
- Their idea of gating an eye as "tracked" on the 2D point being plausible rather
  than on origin validity gives the same answer as ours: decoding this project's
  own committed no-eyes frame
  (`crates/tobii-protocol/src/gaze.rs::real_frame_payload()`) shows the per-eye
  2D columns carrying exactly the `(0,0)` sentinel their predicate rejects.

**Do not read their `assets/extract_firmware.c` or `assets/flash_firmware.c`.**
Those extract firmware from Tobii's own Windows service binary and replay a
signed DFU header captured from a vendor update. That is a provenance this
project does not touch, and this project never flashes firmware at all.

## 1. A one-eye fallback in the head-pose path — shipped in `a804686`

**Was.** `pose_from_sample` (`crates/tobii-headpose/src/lib.rs`) required both
eye validities to be 0 and returned `None` otherwise, so one dropped eye
produced no pose for that frame. This project measured roughly **five per-eye
dropouts per second** as the residual "lag" in head tracking — not latency, and
not the algorithm — so that was not a corner case.

**Shipped.** `tobii-headpose::PairOffset`, a stateful path beside the stateless
one. It re-measures the right-minus-left offset on every two-eye frame and
places the missing eye at the surviving one plus that offset.
`RECONSTRUCTION_MAX_AGE` is **300 ms**, about ten frames at the measured
30.208 ms cadence and well inside the pipeline's 1 s `TRACKING_LOSS_RESET`;
`ONE_EYE_DEBOUNCE` is **2**, and one-directional, so an isolated invalid frame
never switches paths while a frame carrying two measured eyes is answered with
the two-eye pose at once. With both eyes tracked the result is bit for bit the
geometry that shipped before. `tobii-output`'s `FramePipeline` owns the state
beside `neutral` — per-session tracking state that must survive a settings
change — and drops it with the rest at `TRACKING_LOSS_RESET`.

**What a reconstructed pose actually claims.** Yaw and roll are read off the
interocular vector, and during an outage that vector *is* the stored offset. So
rotation is **held** at its last measurement and never extrapolated; only
translation keeps following the eye that is really there. `PoseSource` and
`SourcedPose` carry that distinction out of the crate, because the whole risk of
this path is a guess that reads exactly like a measurement.

**Left out.** The visibility this item asked for. The fallback is **counted, not
shown**: `FramePipeline::fallback_stats` reports the both-eye and reconstructed
frame counts and whether the last pose was reconstructed, and nothing reads it
yet — `tobii debug`, the hub and `tobii headpose --check` live in crates that
commit did not own. `tobii-headpose` has no logger of its own (it cross-compiles
for the Wine bridge), so counting was the only in-crate option.

**Different from what this item proposed.** This page read as "wire
`eyeview::PairOffset` in". What shipped is a second implementation rather than a
shared one: the eyeview version carries normalized trackbox positions for a
drawing, this one tracker-space millimetres for a pose, and this one ages in
**wall-clock** time so a stalled stream cannot make an old offset look fresh by
simply not arriving.

**Where it changes the output, and where it does not.** Only on frames where
nothing else produced a pose — in practice the 5-DOF path. Both front ends
compute their pose with the *stateless* `pose_from_sample` first
(`tobii-cli`'s headpose loop, `tobii-gtk`'s `device.rs`), and the pipeline uses
the reconstruction only when that comes up empty and no fresh model pose exists;
with one, `onnx::fuse` already falls back to the model's own position, so the
reconstruction is bypassed and not counted. The hub's own head-pose readout and
`--check`'s `eyes` line still show nothing on a one-eye frame.

## 2. Reject-and-hold for glitches in the pose filter — shipped in `2ea22cd`

**Was.** `crates/tobii-headpose/src/filter.rs` was a plain EMA that rejected
only non-finite input, so a sample jumping further in one frame than a head can
move was blended in over the following frames — a swing across the view rather
than a discarded sample. It is the pairing for item 1: when an eye drops and
returns, the eye-origin midpoint steps.

**Shipped.** A plausibility gate in front of the average. A finite sample whose
**position** has moved more than `DEFAULT_MAX_STEP_MM = 150.0` from the last
accepted **raw** sample is held, and the raw reference is held with it —
measuring against the smoothed output instead would let a glitch drag the
reference after it one blend at a time, until the view had swept there anyway.
The hold is bounded at `MAX_HELD_FRAMES = 3` (91 ms at the measured interval), a
chosen bound: unbounded, it locks up, because once the head really is elsewhere
every later sample is just as far from the held value. `reset()` drops the
reference with the state, so whoever comes back after a tracking loss is allowed
to be somewhere else. Non-finite handling is unchanged and stays ahead of the
gate, deliberately not charged to the hold budget.

**Where 150 mm comes from, and what it is not.** Not a recorded distribution:
**no committed recording in this repository contains a tracked eye**, which was
decoded rather than assumed (see [Quality-and-Risks](Quality-and-Risks.md)
§11.3d). What `session.tobiicap` does give is the frame interval, **30.208 ms**,
which turns a speed into a per-frame step: 150 mm is **4.97 m/s**, several times
the fastest a seated head translates. It rejects a teleport, not the few
millimetres a stale one-eye reconstruction contributes — a limit tight enough
for that would reject a genuine lunge, which is the risk this item named. It
also cannot go below 100 mm without changing `pipeline.rs` and `tobii-gtk`'s
`outputs.rs`, which each move a synthetic 100 mm between two consecutive frames
and assert the axis moves.

**Position only, and no angular limit at all.** The pose reaching the filter is
the composed one, so yaw and pitch already carry the gaze-driven Extended View
term. Measured on the panel the defaults were tuned for: gaze at the left edge
gives `ev_yaw −45.00°` and the right edge `+45.00°`, so a one-frame look across
the screen legitimately swings composed yaw by 90°. Any angular gate tight
enough to catch a rotation glitch would reject that. A bad frame that moves
position is still rejected whole, angles included.

**Left out.** The `games.toml` key this item offered. The only path from that
file to the filter is `PoseFilter::new(cfg.filter_alpha)` in `pipeline.rs`, a
crate that commit did not own, so an advertised key would be written into the
file, listed by `tobii games`, and read by nothing. `with_max_step_mm()` exists
so that wiring is a one-line change when it is wanted; nothing outside the
crate's own tests calls it today.

## 3. A rotation recentre — the one item on this page still open

**What.** There is none. `crates/tobii-output/src/pipeline.rs` says "Rotation is
never recentred: it is already referenced to facing the screen". Translation gets
an automatic neutral; rotation gets nothing.

**Why.** That reasoning holds for the Extended View term, which is measured
against screen centre, but head yaw and roll come from `pose_from_eyes` — an
`atan2` on the interocular vector, absolute in the tracker's frame. It is
referenced to the screen only by assuming the tracker is mounted square and the
user sits on axis. This project has a measured session with a head sitting
**17.4° off-axis**; such a user has a permanent in-game bias and no control that
removes it.

**Touches.** A `rot_ref` beside `neutral` in `crates/tobii-output/src/pipeline.rs`,
a `Recenter` action over `crates/tobii-ipc`, a button in `crates/tobii-gtk`, and
`tobii headpose --recenter`.

**Effort.** A day, most of it the IPC and GUI surface.

**Risks.** Three, each avoidable:
- A reference captured mid-turn is worse than none — average over a settle window
  of about a second rather than grabbing one frame.
- **Keep pitch out of it.** Pitch already has a measured zero from
  `tobii headpose --calibrate-pitch`; a second reference would fight it.
- Recentre the head term only, never after `fusion::compose` — recentring while
  the user looks at a screen edge would bake the Extended View offset into the
  reference. The same trap for translation is already documented in
  `pipeline.rs`; this one is worse, because nothing decays it.

## 4. Per-group retry in the calibration flow — shipped in `161198e`

**Was.** A point the device refused, or a group whose deadline ran out, ended
the whole 7-point run and offered "Try again", which restarts from the first
point. On a 49" panel one bad corner therefore cost every point already
collected, and nothing on the device side required it: this flow sends per-point
`CalCollect`/`CalDiscard` and already discards and re-dwells mid-collect.

**Shipped.** `MAX_GROUP_ATTEMPTS = 3` in `calibrate_flow.rs` — the first showing
plus two re-shows; only the last attempt still fails, with the same message it
always gave. "Try again" is untouched and still restarts a genuinely failed run
from the first point. A re-show keeps the points the group already captured
(`calibrated` is monotonic, and clearing a captured bit would panic the
`expect()` that reads `collected` against it), keeps `focused` so a collect that
acks late still has a point to be attributed to, sends `CalDiscard` for whatever
was in flight, and takes a fresh deadline and fade-in. The instruction line
reads "Let's try those dots again", because the progress count does not move
when a group comes back and the run would otherwise look stalled.

**"Too weak" is two signals, because there are only two.** The device answers
`add_calibration_point` with an ack or an error and nothing else — `tobii-usb`
drops the reply payload, and no per-point sample count has ever been decoded out
of it — so a weak group means the device refused a point (`CalPhase::last_error`,
read as an *edge* by gating on `requested`, or one error would empty the budget
on consecutive ticks) or the group's own deadline elapsed. The mid-collect
discard on lost focus is deliberately **not** one: it already recovers on its
own, and charging it an attempt would spend the budget on the one failure mode
this flow handles well.

**Three is a judgement bounded by arithmetic**, not a measured or decompiled
figure: an attempt is capped at `COLLECT_TIMEOUT_TICKS` per member (~33 s), so
~99 s for a three-point group — three attempts cap one group at ~5 minutes and a
whole run at ~11.5 minutes of worst case before the failure screen appears. What
is measured is only that a dropped eye is usually transient (item 1's five
per-eye dropouts a second), which is why a first failure is not treated as final.

**Different from what this item proposed.** This page said a partial re-show
needs the group's hit-zone radius recomputed. It does not, and recomputing would
be actively wrong: `group_zone_radii` derives one radius per group from that
group's own points and does not depend on how many are already captured, while
for a lone survivor `focus::zone_radius` returns `f64::MAX` — the whole screen
counts as "on" that point, which is the class of bug the per-group radius was
introduced to fix. The re-show reuses the radius `launch()` computed.

## Smaller candidates

- **Label the per-output validity columns** in [Gaze-Stream](Gaze-Stream.md).
  Upstream labels `0x1d` as combined-2D-valid, `0x1e`/`0x1f` as per-eye-2D-valid,
  `0x21` unfiltered-valid, and `0x23`/`0x26`/`0x28` display-space-valid. This
  project's decoder reads them as U32 and skips them by kind. Decoding the
  committed no-eyes frame shows all of them `0` while validity is `4`, which is
  consistent with those labels. Add them tagged as an unverified third-party
  reading. Ten minutes.
- **Add X4: Foundations to [Game-Output](Game-Output.md).** It reportedly has a
  built-in opentrack UDP listener (Options → Controls), which would make it a
  direct-listener case like X-Plane, needing no opentrack application. **Verify
  against X4's own release notes before claiming it.**
- **Drop the bootloader grant from the udev rule.** `assets/60-tobii.rules` grants
  `2104:0102` (bootloader mode) as well as `2104:0313` (runtime). Nothing in this
  codebase ever opens the bootloader device, and firmware flashing is out of
  scope, so the grant is surface with no user. One line, no behaviour change for
  anything this project does.
- **Reconnection after suspend.** `crates/tobii-usb/src/transport.rs` exposes
  `open()` and a `Drop`, with no re-open path; their project tells users to
  replug after suspend. Two independent implementations hitting the same stale
  endpoint suggests a real device-level problem, and neither has solved it.
- **The `0x1f42` element type.** `crates/tobii-protocol/src/frame.rs` describes
  the op-`0x460` reply as containing a 3-element `0x1f42` struct "whose first two
  values are the point's normalized x and y", without knowing the element type.
  Upstream's TLV header comments claim `0x021f43` point2d_f and `0x031f42`
  point3d_f are 16.16 fixed-point. Decode the stored 713-byte reply both ways and
  see which yields sane coordinates — offline, minutes. Note no code in their
  tree exercises those tags, so it is a comment, not a measurement.

## Verifying a device claim independently

Anything about the hardware gets re-derived here rather than cited from another
repository: `tobii record [--calibration] [FILE] [FRAMES]` for a session,
`tobii-recap <capture.pcap> --gaze-columns` for a capture, `tobii stream --eyes`
for live columns, and `tobii-gtk --accuracy` for the 39-target sweep. See
[Reverse-Engineering-Methodology](Reverse-Engineering-Methodology.md).
