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

## How Tobii actually does it — host-side neural inference [CONFIRMED]

Resolved 2026-07-23 from the Tobii MSI (`platformservice.exe` strings). Head
pose is **computed on the PC, not sent by the device**:

- The device streams **eye-camera images** (`0x501`/`0x50e`, ~78 KB @ 33 Hz —
  see [[Streams]]).
- `platformservice` runs an **OpenVINO** neural model on them —
  `bdtsdata/NN/model.vino.xml` + `model.vino.bin`, loaded via `ReadNetwork` /
  `LoadNetwork` (Intel `InferenceEngine`/`MKLDNNPlugin` DLLs ship alongside).
- The result is exposed as a **client-side** stream:
  `PRP_STREAM_ENUM_HEADPOSE` → `headpose.position` + `headpose.rotation` (also
  `PRP_STREAM_ENUM_LOW_FREQUENCY_HEAD_POSITION`/`_ROTATION`).

This is exactly why USB device-stream probing (`0x400`–`0x520`) never found a
pose stream: **there is none on the wire** — the pose is inferred host-side from
the camera images. Our earlier "host-derived" hypothesis was correct; the
mechanism is a neural net, not a geometric derivation.

### Replicating it

Feasible: subscribe to `0x501`/`0x50e` → run a head-pose model (Rust
`openvino` / `ort` / `tract`) → 6 DOF. Open questions being worked:
- **Camera-image format** — decode `0x501`/`0x50e` (`tobii dump-stream 0x501`).
  Face image vs eye crops decides which models can consume it.
- **Model source** — Tobii's `model.vino.*` is proprietary and **cannot be
  shipped in this GPL repo**; code would load a user-supplied model extracted
  from their own install (`bdtsdata/NN/`). Clean alternative: opentrack's free
  `head-pose-0.4-big-int8.onnx`, if the NIR images are face-like enough (a
  grayscale/NIR domain gap may need fine-tuning).

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

The 5-DOF eye-origin fallback remains useful as a no-model, always-available
baseline (position + yaw + roll); the neural path adds the pitch it cannot give.
