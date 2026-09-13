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
`openvino` / `ort` / `tract`) → 6 DOF. Both former open questions are now
settled by measurement:
- **Camera-image format** — **[CONFIRMED]** `0x501` and `0x50e` are the *same*
  wide face image, not eye crops and not two viewpoints: 199/199 timestamp-matched
  pairs byte-identical with a face in view, and 166/166 on an empty scene, where
  two separate sensors would differ in noise alone (`tobii camera both`). The
  device names them `image` and `primary_camera_image`; `0x508 image_collection`
  acks but delivers nothing. The raw
  pixels sit in a narrow 8..42-of-255 band lit from below, with blown-out
  corneal glints at 255, but the face is plainly legible after a percentile
  stretch — face-like enough for a face model (see
  `crates/tobii-headpose/src/preprocess.rs`, which equalises before inference).
- **Model source** — **settled**: Tobii's `model.vino.*` is AES-encrypted as
  well as proprietary (`docs/windows-headpose-findings.md`), so that path is
  closed, not merely unshippable. We use opentrack's `head-pose-0.5-small.onnx`,
  fetched by the user rather than bundled — see *Getting the neural model*
  below.

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
**[CONFIRMED]** — `tobii-headpose/src/lib.rs` and its tests. That gate is still
the whole of the *stateless* function; a one-eye frame is now handled beside it,
see below.

**Unverified in the fallback [HYPOTHESIS]:** the yaw/roll **sign conventions**
(yaw>0 = turn to user's right; roll>0 = tilt to user's right) and the
`opentrack::TRANSLATION_SCALE` (mm vs opentrack unit). If in-game movement comes
out mirrored, negate the offending angle in `pose_from_eyes` — the only place
each sign is decided.

The 5-DOF eye-origin fallback remains useful as a no-model, always-available
baseline (position + yaw + roll); the neural path adds the pitch it cannot give.

### One eye is enough — for 300 ms, and only for translation

Since `a804686` a dropped eye no longer costs the frame its pose.
`tobii-headpose::PairOffset` is the stateful counterpart of `pose_from_sample`:
it re-measures the right-minus-left offset on every two-eye frame and, when
exactly one eye is tracked, places the missing one at the surviving one plus
that offset. The reason it exists is measured — roughly **five per-eye dropouts
per second**, with one eye lost far more often than both, because a head off the
tracker's optical axis loses the two independently.

- **Rotation is held, not extrapolated.** `pose_from_eyes` reads yaw and roll
  off the interocular vector, and during an outage that vector *is* the stored
  offset — so a reconstructed frame repeats the last measured rotation and only
  translation follows the eye that is really there.
- **Bounds.** `RECONSTRUCTION_MAX_AGE = 300 ms` (~10 frames at the measured
  30.208 ms cadence, and inside `tobii-output`'s 1 s `TRACKING_LOSS_RESET`),
  measured against the host's clock *and* the frame's own `timestamp_us`,
  because neither bounds what the other does: the host clock refuses a stalled
  stream that produces no frames to age an offset with, the device clock refuses
  a transport backlog drained in one burst, where every sample is stamped with
  the same host time however far apart the frames were recorded;
  `ONE_EYE_DEBOUNCE = 2`, one-directional, so one isolated invalid frame never
  switches paths while two measured eyes are answered with the two-eye pose at
  once. With both eyes tracked the output is bit for bit what it was.
- **Not the same code as the eye-position screen's.** `eyeview::PairOffset`
  carries normalized trackbox positions for a drawing and ages by counting
  frames; this one carries tracker-space millimetres and ages against the two
  clocks above.
- **Where it applies.** The pipeline uses it only when the stateless path
  returned nothing *and* there is no fresh model pose — with one, `onnx::fuse`
  takes the model's own position. It is *counted* on every frame either way, so
  `FramePipeline::fallback_stats` measures the tracker's own one-eye rate rather
  than how often the reconstruction won; `tobii headpose` and `--check` print it
  as `, one eye N%` at the end of the rate line. The angle fields themselves
  differ between the two readouts. The hub's are stateless: `device.rs` derives
  them with `pose_from_sample` from the frame in hand, so a one-eye frame blanks
  them. `tobii headpose`'s and `--check`'s are the pipeline's composed pose —
  this reconstruction wherever it won, printed with no marker of its own, so on
  a one-eye frame inside `RECONSTRUCTION_MAX_AGE` the line keeps showing a yaw
  and a roll instead of "NO HEAD DETECTED". The rate fragment is the only place
  a held frame is named.

**[HYPOTHESIS]** — unit-tested only; never run against a tracker, and the 300 ms
bound is a judgement anchored to session notes rather than a fitted
distribution. See [[Quality-and-Risks]] §11.3d.

## What we ship: the neural 6-DOF path

`crates/tobii-headpose/src/onnx.rs` runs opentrack's `head-pose-0.5-small.onnx`
on `tract` (pure Rust — no runtime to download, no system library). **12.4
ms/frame single-threaded** against a 33 Hz camera, plus ~50 ms once to optimise
the graph. Enabled with the crate's `onnx` feature, which is on by default;
`--no-default-features` gives the lean 5-DOF build.

```text
CameraFrame 280x280 u8
  -> Roi (sub-pixel, square)      seeded from the corneal glints
  -> sample_patch  129x129 u8     bilinear, replicate border
  -> normalize     129x129 f32    opentrack's adaptive brightness gain
  -> tract                        pos_size, quat, box, *_scales
  -> next_roi = box               the ROI for the NEXT frame
  -> image->world, perspective correction, Euler
```

### Three parts that look optional and are not

Each was measured on a real ET5 frame, not reasoned about.

| part | what happens without it |
|---|---|
| **box-feedback loop** (the model's `box` becomes the next ROI) | Pitch is confounded with crop *scale*: a 0.70–1.50x zoom sweep moves pitch **19.6°** while yaw moves under 2°. A one-shot crop from `preprocess()` is **8° off in pitch — and passes the confidence gate while being so.** |
| **sub-pixel ROI** (`Roi { cx: f32, cy: f32, side: f32 }`, bilinear, replicate border) | The crate's integer `BBox` + nearest-neighbour `crop_resize` injects **4.06° peak-to-peak pitch jitter on a frozen image**, purely from ROI quantisation, against 0.30° for bilinear. |
| **updating the ROI on rejected frames** | From a whole-frame start, sigma runs 0.50 → 0.94 → 0.80 → 0.88 → 0.75 → 0.51 → 0.20 before locking at 0.08. Gating the ROI update on sigma freezes it there and re-acquisition can never happen. Sigma decides whether a pose is *emitted*, never whether the ROI *moves*. |

### The contract [CONFIRMED]

Input `x [1,1,129,129]` f32. Outputs `pos_size[1,3]`, `quat[1,4]`, `box[1,4]`,
`pos_size_scales[1,3]`, `rotaxis_scales_tril[1,3,3]`, resolved **by name** at
load (labels survive tract's optimiser; outlet ids do not).

- Normalisation is opentrack's `normalize_brightness`: put the patch's 90th
  percentile at mid-scale, `alpha = 0.45 / max(5, q)` for `q < 127` else
  `1/255`, then `p * alpha - 0.5`. It works on ET5 NIR despite the corneal
  glints saturating. `Normalize::SignedUnit` is the trap — it gives yaw −75°
  and sigma 0.81, i.e. garbage.
- `pos_size` and `box` are normalised to the **patch half-extent**, not `[0,1]`
  and not the 129 grid. `+py` is down. `box` is corner form.
- `quat` is `(x, y, z, w)` — **real part last.** Reading it w-first gives a
  plausible-looking upside-down head, which is why there is a test pinning
  exactly that mistake.
- `rotaxis_scales_tril` is always exactly `sigma * I`, by construction: the
  training head predicts one scalar and writes literal zeros off the diagonal.

### Confidence gate

`sigma = rotaxis_scales_tril[0][0]`, reject at `>= 0.15`:

| input | sigma |
|---|---|
| real face, well cropped | 0.066 – 0.075 |
| real face, worst perturbation tried | 0.135 |
| real face, eyes occluded | 0.173 |
| best garbage of any kind | 0.423 |
| real empty-room capture | 0.90 – 1.06 |

Nothing was ever observed between 0.18 and 0.42 — a 5.4x empty band, so the
threshold is not delicate. What it does **not** catch is a badly *scaled* crop
(0.093 passes, carrying 8° of pitch error), which is what the feedback loop is
for.

### Fusion

Position from the **eye origins** — a hardware measurement in real millimetres,
and the one advantage this device has over a webcam, which must assume a fixed
head size. Rotation from the **model**, all three angles together so the
rotation stays self-consistent. `tobii_headpose::onnx::fuse`.

### What is still unverified, and how to settle it

Everything about the model's *numerics* is measured. What is not is how its
rotation sits against the physical world — and a mirrored pose looks perfectly
plausible, which makes it the hardest class of error to spot from a number. All
of it is collected in `onnx::Signs` so a fix is a constant, not a rewrite:

| unknown | how to settle it |
|---|---|
| yaw sign, roll sign | `tobii headpose --check` prints the model's yaw and roll beside `pose_from_eyes`'s. Turn your head one axis at a time: they must move **together**. If a pair anti-correlates, flip that sign. |
| absolute pitch zero | Sit square-on to the screen with `--check` running. Whatever pitch reads is the offset — cancel it with `Signs::pitch_offset_deg`. Two constants are folded in here that cannot be separated from one frame: the training set's own convention, and the ET5's physical camera tilt (its mounting rotation is **−20.00°**, measured by the gaze pipeline). |
| `DEFAULT_FOCAL_PX = 355` | The perspective correction is worth 12–16° of pitch, and omitting it makes pitch a function of where the head sits in frame (~28° swing). `focal_from_eye_origins` computes the real value from the glint separation and the metric eye origins — both already on the wire. Cross-check: report pitch with the head high and low in frame; with the right `f` they agree. |
| whether the 0.15 gate generalises | It was calibrated on one face. Log sigma over 30 s with glasses, one eye occluded, gaze far off-axis, and a reflective object behind the head; check the 0.18–0.42 band is still empty. |

## Getting the neural model

The weights are **not in this repository and are never fetched automatically.**
opentrack's tracker code is free software, but its head-pose models are trained
on data that is not: CC BY-NC 4.0 (non-commercial) sets, Microsoft's "Research
Use of Data" terms, and a non-commercial face model. That is incompatible with
redistribution under GPL-3.0-only, so the model is the user's to obtain:

| | |
|---|---|
| model | `head-pose-0.5-small.onnx`, 12,919,981 bytes |
| source | `https://raw.githubusercontent.com/opentrack/opentrack/master/tracker-neuralnet/models/head-pose-0.5-small.onnx` |
| sha256 | `7c14f84114fb9eca89759d8a36350c6faae2b4187258cae07afb77a93c2d7eec` |
| installs to | `$XDG_CONFIG_HOME/tobii-linux/models/` (default `~/.config/…`) |

The digest is **pinned and enforced** (`crates/tobii-headpose/src/sha256.rs`, a
dependency-free streaming SHA-256 checked against the NIST vectors and against
`sha256sum` on the real file). A model whose bytes do not match is refused by
`install` and reported as `Corrupt` by `status` — never quietly used, because a
different model's output is plausible-looking and wrong.

Two front ends, one code path (`crates/tobii-headpose/src/model_store.rs`):

- **CLI** — `tobii headpose --fetch-model` prints [`TERMS`], asks for consent,
  then downloads (via `curl`, falling back to `wget`; no HTTP crate is linked
  in), verifies, and installs. `--model-status` reports what is installed.
- **GUI** — the hub's **Head tracking** section
  (`crates/tobii-gtk/src/head_model.rs`) shows the same status line, and its
  button opens a dialog with the same terms. Agreeing runs the fetch on a
  worker thread with a progress line driven by the part-file's size; Escape and
  the default button both mean "no".

`model_store::fetch` deliberately takes **no** "yes" argument, so consent cannot
be defaulted by a caller — it must be taken in the UI that showed the terms.

**[CONFIRMED]** — end-to-end run against a sandbox config dir on 2026-08-26:
download → digest match → install → `--model-status` reads
"installed and verified", with the `.download` part-file removed.

The localizer model is defined in the store but is **not** fetched: the face ROI
comes from the tracker's own metric eye origins, which beat a detector's guess
and cost nothing.
