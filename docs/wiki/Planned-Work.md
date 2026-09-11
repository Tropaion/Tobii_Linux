# Planned work

What is queued, why it is worth doing, and what each item rests on. Nothing here
is needed for a release that has already shipped; this is the list a contributor
can pick from.

Each item says what to change, where, what it costs, and how it could go wrong.
Where a number appears, it is one this project measured — not one read from
another project.

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

## 1. A one-eye fallback in the head-pose path

**What.** `pose_from_sample` (`crates/tobii-headpose/src/lib.rs`) requires both
eye validities to be 0 and returns `None` otherwise, so one dropped eye produces
no pose for that frame.

**Why.** This project measured roughly **five per-eye dropouts per second** as
the residual "lag" in head tracking — not latency, and not the algorithm. A
fallback that keeps producing a pose from the surviving eye is the structural fix.

**The algorithm is already here.** `crates/tobii-gtk/src/eyeview.rs`'s
`PairOffset` re-measures the right-minus-left delta while both eyes are real,
reconstructs the missing one when exactly one is, and ages the offset out so it
never draws a ghost. It is not wired into `tobii-headpose`.

**Touches.** `crates/tobii-headpose/src/lib.rs` for a variant of
`pose_from_sample` that carries reconstruction state, and
`crates/tobii-output/src/pipeline.rs` to own that state beside `neutral`.

**Effort.** Half a day with tests. Pure logic, unit-testable with recorded frames.

**Risks.** A reconstructed pose is a guess: keep `PairOffset`'s aging discipline
so a stale offset cannot confidently report a head that is not there, and make
the fallback visible in `tobii debug`. Note the present cost is a stutter, not a
yank — `pipeline.rs` only recentres after `TRACKING_LOSS_RESET`, and the
opentrack sink drops pose-less frames rather than sending zeros, so the receiver
holds its last value.

## 2. Reject-and-hold for glitches in the pose filter

**What.** `crates/tobii-headpose/src/filter.rs` is a plain EMA and says so; it
rejects only non-finite input. A sample that jumps implausibly far in one frame
is blended in over the following frames.

**Why.** This is the pairing for item 1: when an eye drops and returns, the
eye-origin midpoint steps, and an EMA turns that step into a visible swing. A
per-frame jump limit that holds the last accepted value instead is a handful of
lines.

**Touches.** `crates/tobii-headpose/src/filter.rs`, and one key in
`crates/tobii-output/src/games.rs::keys()` if it should be tunable.

**Effort.** A few hours.

**Risks.** Too tight a threshold rejects a genuine fast turn. The limit is
per frame precisely so a real turn spans many frames and each stays under it.
**Derive the threshold from recorded sessions here, not from another project's
constant** — theirs ships with an open high-severity corner overshoot.

## 3. A rotation recentre

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

## 4. Per-group retry in the calibration flow

**What.** A weak point fails the whole run: `crates/tobii-gtk/src/calibrate_flow.rs`
offers "Try again", which restarts from the first point.

**Why.** On a 49" panel one bad corner costs the entire run. The device side
already supports the narrower operation — this flow sends per-point
`CalCollect`/`CalDiscard` and already discards and retries mid-collect.

**Effort.** Half a day. No protocol change.

**Risks.** The flow shows three points per group and lets gaze choose which is
focused, so "retry this point" is not well defined: it has to be "retry this
group". The group's hit-zone radius is computed from that group's own points, so
a partial re-show needs the radius recomputed, or the whole group re-shown.

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
