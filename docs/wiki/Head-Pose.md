# Head Pose (open investigation)

The ET5 advertises 6-DOF head tracking, useful for opentrack / head-aimed games.
This page documents where head pose **is not**, what we ship as a fallback, and
exactly what capture would resolve the rest. Source of truth:
`crates/tobii-headpose/src/lib.rs`, memory note
`et5-headpose-not-in-gaze-stream`, `docs/session-handoff-windows-vm.md`.

## Finding: head pose is NOT in the gaze frame — [CONFIRMED]

Established live 2026-07-22 with `tobii columns` (dumps the full per-frame
inventory of all 39 columns, every data kind), captured across all six head axes
(translate x/y/z, yaw, pitch, roll) with a head present and moving one axis at a
time. Result: **no head orientation is anywhere in the `0x500` gaze frame.**

- **point3d columns are all eye geometry.** `0x02`/`0x08` eye origins,
  `0x17`/`0x18` raw origins, and the higher pair `0x22`/`0x24` (~45 mm above,
  ~15 mm behind the eyes) are all **positions** — they give clean position + yaw
  + roll but no pitch. `0x04`/`0x0a` are per-eye **gaze directions** (x tracks
  yaw, y tracks pitch — where you *look*, not head facing). `0x25`/`0x27` stay
  ~zero.
- **scalar columns carry no orientation.** Every unmapped u32
  (`0x15 0x16 0x1b 0x1d 0x1e 0x1f 0x21 0x23 0x26 0x28`) had range 0 across both
  rotations — constant flags. `0x06`/`0x0c` are pupil diameters, `0x01`
  timestamp, `0x14` frame counter.
- **No Euler angles, no quaternion.** Pitch is **not recoverable** from the point
  geometry: the `0x22`→eye vector tilted *more* on translation than on an actual
  nod (contaminated, not signal). Two eye points give only 5 DOF; pitch needs a
  second rigid head reference the stream does not provide.

The pinned regression `gaze.rs::unmapped_point3d_columns_are_eye_positions_not_head_pose`
asserts the unmapped point3d set is exactly `{0x22, 0x24, 0x25, 0x27}` so a
decode shift fails loudly. **tobiifree concurs** — it only ever subscribes to
`0x500`; its own "does direction correlate with eye_origin (head pose)?" probe
was an attempt to *derive* pose, not read a pose stream. **[CONFIRMED]**.

## Two remaining hypotheses

The 6-DOF feature is real, so Tobii's software gets pose *somehow*. Exactly one
of these holds, and only a capture of the original software distinguishes them:

1. **A separate stream/subscription we never found.** `build_subscribe` already
   accepts any stream id; we only ever send `0x500`. Probing `0x501..=0x520`
   found only eye-image (`0x501`/`0x50e`) and event (`0x504`) streams, no pose.
   The head-pose stream, if it exists, is outside that range or uses a subscribe
   variant we have not seen. **[HYPOTHESIS]**
2. **Host-side derivation** from the eye/gaze data. If so, Tobii sends nothing
   extra and our derivation approach is already correct (only clean pitch is
   missing). **[HYPOTHESIS]**

## What we ship: a 5-DOF eye-origin fallback

`crates/tobii-headpose` derives a pose from the two eye origins
(`pose_from_eyes`) **[CONFIRMED]** logic:

- **position** = midpoint of the two eye origins (tracker-space mm).
- **yaw** = interocular vector angle in the horizontal x–z plane.
- **roll** = interocular vector tilt in the frontal x–y plane.
- **pitch = 0.0, always.** Two eyes are a single line through the head; nodding
  rotates about that line and leaves both origins essentially fixed. There is no
  vertical reference (nose/chin/forehead) in the data.

Gating is strict: **both** eyes must report `validity == 0` *and* have origin
columns present (`pose_from_sample`) — the present bit alone is not enough (the
device sends zeroed origins with `validity == 4` when no eye is detected; see
[[Gaze-Stream]]). Streamed to opentrack over UDP by `tobii headpose`.
**[CONFIRMED]** — `tobii-headpose/src/lib.rs` and its tests.

**Unverified in the fallback [HYPOTHESIS]:** the yaw/roll **sign conventions**
(yaw>0 = turn to user's right; roll>0 = tilt to user's right) and the
`opentrack::TRANSLATION_SCALE` (mm vs opentrack unit). If in-game movement comes
out mirrored, negate the offending angle in `pose_from_eyes` — the only place
each sign is decided.

## What capture would resolve it

A **Windows-VM USB capture** of Tobii's own software while the head moves. Watch
for: a `subscribe` (`0x4c4`) to a stream id other than `0x500`, and a subsequent
notify op whose payload changes with head orientation (Euler angles or a
quaternion). If none appears, pose is host-derived and hypothesis (2) holds. See
[[Reverse-Engineering-Methodology]] and `docs/session-handoff-windows-vm.md`.
