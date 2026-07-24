<!-- Auto-generated 2026-07-23 by a multi-agent recon workflow, then reviewed. DRAFT: many I/O facts are pending an elevated read of the ACL-locked model files. -->

# Tobii ET5 head-pose — Windows→Linux integration map (DRAFT, pending elevated model read)

> **Status: DRAFT.** This document reconciles three repo analyses (head-pose backend contract, calibration/display geometry, protocol/head-pose docs) with the Windows reverse-engineering facts extracted from `platform_runtime_IS5LEYETRACKER5_service.exe` (Tobii "ViNNIE" engine over Intel OpenVINO, module codename "castor"). Several I/O facts remain **UNKNOWN** until a pending *elevated* read of the NTFS-ACL-locked model files (`NN/model.vino.xml`, `Head Tracker.cfg`, `pr_log0.txt`). Every coordinate-frame / unit / rotation-representation claim about Tobii's *real* output is flagged; do not treat any of them as validated until that read lands. Anything stated as fact here is either a decoded on-wire fact or a repo design choice, and is labelled as such.

---

## 1. Pipeline overview (Windows) mapped to the Linux backend trait

### 1.1 The Windows pipeline (from platform-service strings)

Tobii runs a **multi-stage neural + classical cascade** entirely host-side on the two 280×280 NIR camera images. There is **no head-pose stream on the USB wire** — this is now [CONFIRMED] both by the repo (`docs/wiki/Head-Pose.md:37-54`) and by the Windows strings, which are the *same source* (dated 2026-07-23), so the two agree rather than merely coincide.

Stage order (Windows):

1. **Face finder** — `FF/vnn_nir/ff.vino`
2. **Detector** — `NN/vnn_nir/d1.vino`
3. **Landmarks** — LBF cascade (`fa.lbf`, `fc.lbf`, `LBF/lv`, `is.bin`, `init_shape.bin`) producing landmark groups: eyebrows, mouth, pupils, eyelids, nose, contour, ears, plus `screen_space_gaze`
4. **Regressor** — `NN/fr.vino`
5. **Main model** — `NN/model.vino` (head pose, per the extraction plan)

Supporting assets: a 3D face model + MPEG-4-style FDP files (`jk_300.wfm`, `jk_300_rigid.fdp`), which imply head pose may come from **landmarks + rigid-model PnP**, and/or from the `model.vino` regressor. **Which of the two produces the final pose is UNKNOWN.**

### 1.2 The Linux backend trait

The Linux side collapses all of the above behind a single object-safe trait — `crates/tobii-headpose/src/model.rs:67-70`:

```rust
pub trait PoseModel {
    fn estimate(&mut self, left: &CameraFrame, right: Option<&CameraFrame>) -> Option<HeadPose>;
    fn kind(&self) -> ModelKind;
}
```

**Mapping decision (INFERRED):** the entire Windows cascade (FF → detector → landmarks → regressor → model) lives *inside* one `PoseModel` implementor — the `ModelKind::TobiiVino` adapter (`model.rs:32-50`). `estimate()` takes the two decoded `CameraFrame`s and is expected to internally run whatever sub-stages Tobii's pipeline needs, returning one `HeadPose` (or `None` on no-face / low-confidence / inference error, in which case the caller holds the previous pose — `model.rs:61-66`). Nothing downstream of `estimate()` changes, because both the neural path and the existing geometric fallback converge on `HeadPose → PoseFilter → to_opentrack_datagram`.

**Consequence for the shared preprocessor:** the Linux `preprocess::face_bbox` brightness heuristic (`mean + k·stddev`, `k≈1.5`, `preprocess.rs:60-99`) is a *placeholder standing in for Windows `ff.vino` + `d1.vino`*. If the Tobii backend must reproduce Tobii's own face localisation, the neural face-finder replaces that heuristic; the brightness bbox is only a stopgap for the open ONNX/6DRepNet backends. (KNOWN that the heuristic exists; INFERRED that the Tobii adapter will bypass it in favour of `ff.vino`.)

**Backend redistributability (KNOWN, repo):** `ModelKind::is_redistributable()` returns `false` only for `TobiiVino` (`model.rs:42-50`); the `.vino` files are user-supplied, load-only, never shipped (`model.rs:17`). This matches the Windows fact that the files are NTFS-ACL-locked to SYSTEM/Admin.

---

## 2. Per-model roles + KNOWN vs UNKNOWN I/O

| Windows asset | Role | Input (KNOWN / UNKNOWN) | Output (KNOWN / UNKNOWN) |
|---|---|---|---|
| `FF/vnn_nir/ff.vino` | Face finder | Input: 280×280 NIR (KNOWN it operates on the NIR frame). Exact tensor shape/dtype **UNKNOWN**. | Face ROI / candidate box — **UNKNOWN** exact format. |
| `NN/vnn_nir/d1.vino` | Detector | Cropped face region (INFERRED). Shape **UNKNOWN**. | Detection/confidence — **UNKNOWN**. |
| LBF cascade (`fa.lbf`, `fc.lbf`, `LBF/lv`, `is.bin`, `init_shape.bin`) | Landmark regression | Face crop + `init_shape.bin` initial shape (KNOWN files exist). | Landmark groups incl. `screen_space_gaze` — group names KNOWN, coordinate space **UNKNOWN**. |
| `NN/fr.vino` | Regressor | **UNKNOWN**. | **UNKNOWN** (feature regression feeding the main model, INFERRED). |
| `NN/model.vino` (+`.bin`) | Main model — head pose per plan | **UNKNOWN** — C=1 mono vs stereo-stacked, input size, dtype all pending `model.vino.xml`. | **UNKNOWN** — Euler vs quaternion vs matrix; this is the crux of the elevated read. |
| `jk_300.wfm`, `jk_300_rigid.fdp` | 3D face model / rigid FDP | Used for PnP against landmarks (INFERRED). | Rigid head pose (candidate producer, competing with `model.vino`) — **UNKNOWN which wins**. |

**Engine (KNOWN):** Tobii "ViNNIE" wrapper over Intel OpenVINO `InferenceEngine` / `MKLDNNPlugin`, `ReadNetwork`/`LoadNetwork` (`Head-Pose.md:47-49`). The Linux implication: `ModelKind::TobiiVino` will need the `openvino` crate added to `crates/tobii-headpose/Cargo.toml` (today its only dependency is `tobii-protocol` — no `ort`/`openvino`/`onnxruntime`/`tract` is a dependency of any crate yet; the deferral is stated at `model.rs:23-24`).

**File-readability (KNOWN unknown):** `model.vino.xml` is ~14 KB, so *likely plaintext OpenVINO IR* — but whether it is plaintext or ciphertext is **UNKNOWN** until the elevated read. If ciphertext, the whole "load `.vino.xml`+`.bin` via the `openvino` crate" plan (`model.rs:53-59` `ModelConfig{kind, model_path}`) is blocked and a decrypt/repack step is required.

---

## 3. Preprocessing (what's known)

**Windows (KNOWN facts, exact constants UNKNOWN):**
- Preprocessing uses **Tobii VisionSDK**: `vs::Mat`, `vs::resize` (cubic/linear), `CroppingAdjustment`.
- Named parameters observed: `max_face_scale`, `min_face_scale`, an ROI crop.
- The **exact input tensor shape and dtype are UNKNOWN** until `model.vino.xml` is read.

**Camera frames feeding preprocessing (KNOWN, decoded, [CONFIRMED]):**
- `crates/tobii-protocol/src/camera.rs:22-31` — `CameraFrame { timestamp_us, width, height, bit_depth, pixels: Vec<u8> }`.
- Layout: 8-bit grayscale, one byte/pixel, row-major, top-left origin; **280×280**, ~78 KB/frame @ ~33 Hz.
- Decoded by `decode_camera_frame(payload) -> Option<CameraFrame>` (`camera.rs:59`) from the `0x501` (left) / `0x50e` (right) stereo NIR streams; the 4-byte blob prefix is stripped (`camera.rs:35, 92`). Camera streams [CONFIRMED] at `docs/wiki/Streams.md:22-23,27-35`.

**Linux shared preprocessor (KNOWN, but constants are placeholders):** `crates/tobii-headpose/src/preprocess.rs`
- `preprocess(frame, size, channels, norm, pad_frac) -> Option<(Vec<f32>, BBox)>` (`preprocess.rs:143-163`) returns a **CHW `Vec<f32>`** tensor; grayscale replicated across channels for RGB models (`preprocess.rs:157-161`).
- `face_bbox` brightness heuristic `k≈1.5` (`preprocess.rs:60-99`); nearest-neighbour `crop_resize` (`preprocess.rs:104-115`), bilinear notably addable.
- `Normalize`: `Unit` (`p/255`), `SignedUnit` (`p/127.5−1`), `MeanStd{mean,std}` (`preprocess.rs:118-137`).

**Reconciliation (what the elevated read must overwrite):** the Linux `size`, `channels`, `pad_frac`, and `Normalize` are heuristic defaults (`preprocess.rs:9-10` explicitly says Tobii's crop-padding/normalization come "from the extracted spec"). Windows `vs::resize` cubic ≠ Linux nearest-neighbour, and `min/max_face_scale` + `CroppingAdjustment` are the real `pad_frac`/crop analogue. **None of these constants are known yet.**

---

## 4. Output → 6-DOF mapping, per-claim confidence

The Linux target type is `HeadPose` — `crates/tobii-headpose/src/lib.rs:70-78`:

```rust
pub struct HeadPose { x_mm, y_mm, z_mm, yaw_deg, pitch_deg, roll_deg }  // Euler degrees, NOT a quaternion
```

Emitted as the opentrack UDP datagram — `crates/tobii-headpose/src/opentrack.rs`: **48-byte payload = 6× `f64` little-endian, order `x, y, z, yaw, pitch, roll`, no header/checksum, default port 4242** (`opentrack.rs:8, 19, 42-50`).

| # | Claim | Confidence | Basis |
|---|---|---|---|
| 1 | Tobii emits `headpose.position` (xyz) and `headpose.rotation` as distinct fields on the client stream `PRP_STREAM_ENUM_HEADPOSE`. | **KNOWN** | Windows strings + `Head-Pose.md:47-49`. |
| 2 | The Linux wire/output shape is 6×f64 LE `x,y,z,yaw,pitch,roll`, port 4242, `TRANSLATION_SCALE=1.0`. | **KNOWN** (repo design) | `opentrack.rs:1-16, 35, 42-50`. |
| 3 | Tobii's rotation representation is Euler angles in degrees. | **UNKNOWN** | Could be Euler (any order), quaternion, or 3×3 matrix — listed open in `docs/windows-extract-tobii-headpose-model.md:127-138`. The Linux Euler struct is the *fallback's* choice, not Tobii's. |
| 4 | Tobii's position units are millimetres. | **UNKNOWN** | mm vs m unresolved; `TRANSLATION_SCALE=1.0` is flagged **UNVERIFIED** (`opentrack.rs:22-35`). This is the single place unit conversion happens. |
| 5 | The rotation→Euler conversion happens in exactly one place (constructing `HeadPose`). | **KNOWN** (repo design) | `lib.rs:70-78`; if Tobii emits quat/matrix, that conversion is encoded here — the only edit point. |
| 6 | Head pose comes from `model.vino` (a regressor). | **INFERRED** | Competing hypothesis: landmarks + `jk_300_rigid.fdp` PnP. Windows evidence supports *either*; **which is UNKNOWN**. |
| 7 | The neural path will supply real pitch (the fallback cannot). | **KNOWN limitation → INFERRED fix** | Fallback `pitch_deg` is always `0.0` (5-DOF from two eye origins; `lib.rs:29-36,122-124`, test `lib.rs:326-330`). The neural backend is the intended fix, contingent on claim 3. |
| 8 | yaw>0 = user turns right; roll>0 = user tilts right. | **UNKNOWN / HYPOTHESIS** | Explicitly flagged unvalidated (`lib.rs:38-50`). These are fallback assumptions; Tobii's signs are unknown. Fix point if mirrored: negate in `pose_from_eyes` (`lib.rs:48-49`). |
| 9 | Optional EMA smoothing applies equally to the neural output. | **KNOWN** | `PoseFilter` (`filter.rs`), `alpha=0.25`, discards non-finite, `reset()` on tracking loss. CLI already routes the fallback through it. |

---

## 5. Coordinate frame + units — state of knowledge

**Linux fallback frame (KNOWN as a repo design choice, NOT validated against hardware):** tracker-space millimetres, origin at the IR sensor array — `+x` user's right, `+y` up, `+z` toward the user (`z≈680` ⇒ head ~680 mm in front) (`lib.rs:4-13`).

**Note on axis-sign consistency across the two subsystems (KNOWN, worth flagging):** the head-pose fallback documents `+z` **toward the user** (`lib.rs:4-13`), while the *display-geometry* subsystem documents the tracker frame as **+X right, +Y up, +Z backward / away from user** (`display.rs`, `docs/wiki/Display-Area.md`; §6 below). These are opposite `+z` conventions living in the same repo. This is a **latent inconsistency to resolve** once Tobii's real HEADPOSE frame is known — do not assume the two subsystems already agree.

**Tobii's real HEADPOSE frame (UNKNOWN):** origin, axis directions, and handedness of `headpose.position`/`headpose.rotation` are all unresolved (`docs/windows-extract-tobii-headpose-model.md:145-176,214-221`). The Windows strings give field *names* only, no frame or units.

**Five Windows stream enums, reconciled against the repo:**

| Windows enum | Repo status | Reconciliation |
|---|---|---|
| `PRP_STREAM_ENUM_HEADPOSE` (`headpose.position`, `headpose.rotation`) | Documented (`Head-Pose.md:47-49`) | Full agreement. The head-orientation stream. |
| `PRP_STREAM_ENUM_LOW_FREQUENCY_HEAD_POSITION` | Partially documented | Repo folds two enums into one line (`Head-Pose.md:49`); Windows shows **two distinct** enums — split the names. |
| `PRP_STREAM_ENUM_LOW_FREQUENCY_HEAD_ROTATION` | Partially documented | Same — the second half. |
| `PRP_STREAM_ENUM_EYE_POSITION_NORMALIZED` | **Data decoded on wire, enum NOT mapped** | Already on the `0x500` gaze wire: `0x03`/`0x09` → `trackbox_eye_l/r`, "x/y normalized in trackbox [0,1], z=distance mm" (`docs/wiki/Gaze-Stream.md:44-45`, `gaze.rs:52-54`). This is **user positioning, not head orientation.** |
| `PRP_COMPOUND_STREAM_ENUM_USER_POSITION_GUIDE_XYZ` | **Not documented** | New. The "position-your-head-in-the-box" guide; almost certainly derived from trackbox/eye-origin data the repo already decodes (`gaze.rs` `0x02`/`0x08` eye origins, `0x03`/`0x09` trackbox). **Not a head-orientation stream** — flag so it is not re-investigated as one. |

**Takeaway:** two of the five enums are user-positioning data already on the gaze wire; only `PRP_STREAM_ENUM_HEADPOSE` (+ its low-frequency variants) is the neural head-orientation output. A regression anchor pins this distinction: `gaze.rs:479-493` (`unmapped_point3d_columns_are_eye_positions_not_head_pose`).

---

## 6. Calibration `.setpm` mapping to Linux types

Three captured Windows files (`docs/TobiiSetupProcess/calibration/{screenplane,displayinfo,calibration}.setpm`) map onto Linux types as follows.

### 6.1 `screenplane.setpm` → `DisplayCorners` / `DisplaySetup` — CLEAN, fully mappable (KNOWN)

Decoded as 9× float32 LE (numerically verified):

| Captured point | Value (mm) | Linux corner |
|---|---|---|
| P1 (−596.50, 10.27, −3.10) | low y, −z, left x | `bl` |
| P2 (−596.50, 325.56, 111.66) | high y, +z, left x | `tl` |
| P3 (596.50, 325.56, 111.66) | high y, +z, right x | `tr` |

- `.setpm` corner order is **BL, TL, TR**; the Linux wire order is **TL, TR, BL** (`display.rs:9` `DisplayCorners{tl,tr,bl}`, decoded via Q42 fixed-point big-endian, `tlv.rs:11,46,174`). Same three corners, different serialization; same tracker frame (**+X right, +Y up, +Z backward**).
- Fed into `DisplaySetup::from_corners` (`crates/tobii-config/src/setup.rs:87`) yields: **width_mm=1193.0, height_mm=335.5, tilt_deg=20.0°, offset_x=0.0, offset_y=10.27, offset_z=−3.10**. Directly loadable as a `[display]` TOML section (`setup.rs:102,127`) or `to_corners()` → `set_display_area` (op `0x5a0`).
- **width = arc, not chord (KNOWN risk):** 1193 is the EDID *arc* width of this 49" 1800R panel; Tobii's Windows setup did **not** apply `chord_from_arc` (`setup.rs:45,308`). To reproduce the exact plane the captured calibration was computed against, Linux must use **1193**, not the chord-corrected 1171.

### 6.2 `displayinfo.setpm` → (partly) `MonitorInfo` — NO Linux home for the binding (KNOWN gap)

Plaintext: `DISPLAY\SAM7463\5&21C6DF7E&0&UID4352##eId:4C2D6374414D52302523#or:1#v:1.0`. `SAM7463` = Samsung PnP monitor id; `UID4352` = port; `eId` = EDID-derived id; `or:1` = landscape; `v:1.0` = format version. Linux has `MonitorInfo{model,width_mm,height_mm}` from EDID (`edid.rs`, fixture `odyssey-g93sc.edid`) but **no concept of binding a calibration to a monitor/port** — this file has no target type today.

- **Identity mismatch (KNOWN risk):** captured code `SAM7463` vs committed Linux EDID fixture `SAM 0x7454` ("Odyssey G93SC", DTD 1193×336 mm). Dimensions match exactly; product-code digits differ — confirm same unit before trusting the binding.

### 6.3 `calibration.setpm` → `CalibrationBlob` — SAME FAMILY, NOT a drop-in (KNOWN + risk)

- Linux treats per-user calibration as an **opaque blob**: `CalibrationBlob(Vec<u8>)` (`calibration.rs:12`), the verbatim `retrieve` (op `0x44c`) response, stored to `calibration.bin` (`store.rs:52,88,93`) and re-applied byte-for-byte on every connect via op `0x456` (`connection.rs:138-186`). Only assertion: non-empty, ≤4096 B (`calibration.rs:97`).
- The `.setpm` is the **same serialization family** — both share the inner record signature (LE triple `6e 00 00 00 54 00 00 00 01 00 00 00` = 110,84,1) followed by ASCII tag **`human`**. The `.setpm` wraps it in a 12-byte container (`version=4, count=1, payload_len=0xC25A0`) with chunk magic `a5 73 4e 29`.
- **But it is NOT directly applyable (biggest risk):** the device blob is ≤4096 B; this file is **796 KB** (entropy ≈8), the host-side full per-eye "human" gaze model. Whether a device-applyable sub-blob can be extracted, or whether the ET5 firmware even wants one (vs. the host runtime doing gaze math from the model), is **UNKNOWN**. The current Linux driver only knows the ≤4 KB firmware-side tier.

### 6.4 No parser / import path exists (KNOWN)

No `.setpm` container parser and no Windows-calibration import command exist anywhere in `crates/` (grep = 0 code hits). Bringing any of this in is new code (§7).

---

## 7. Concrete Linux integration points

**Head-pose neural backend (the Tobii model plugs in here):**
1. Add the inference-engine dependency to `crates/tobii-headpose/Cargo.toml` — the `openvino` crate for `ModelKind::TobiiVino` (currently only `tobii-protocol`).
2. Create a struct implementing `PoseModel` (`model.rs:67-70`): load `model.vino.xml`+`.bin` from `ModelConfig.model_path` (`model.rs:53-59`); run the cascade (FF→detector→landmarks→regressor→model, §1.1) internally; map output → `HeadPose` (§4 claim 5); `kind()` → `ModelKind::TobiiVino`.
3. Fill real preprocessing constants into `preprocess::preprocess` params (`preprocess.rs:143-163`) — Tobii's crop/resize/normalization, replacing the `k=1.5` bbox and default `Normalize`/`pad_frac` placeholders (`preprocess.rs:9-10,60,117-163`).
4. Wire into the CLI `headpose` loop (`crates/tobii-cli/src/main.rs:567-622`), which today uses **only** the geometric fallback (`pose_from_sample`) and never subscribes to cameras. The plumbing is proven end-to-end by the `camera` subcommand (`main.rs:261-316`): `conn.subscribe_stream(0x501)` → `read_notifications()` → `decode_camera_frame(&payload)`. The `headpose` loop subscribes to `0x501`/`0x50e`, decodes both, calls `model.estimate(&left, Some(&right))`, feeds the result through the existing `PoseFilter`, and emits via the unchanged `opentrack::to_opentrack_datagram` (`main.rs:604-608`). On `None`, hold/reset as the fallback's tracking-loss branch already does (`main.rs:611-620`).

Because both paths converge on `HeadPose → PoseFilter → to_opentrack_datagram`, **nothing downstream of `estimate()` changes.**

**Doc-status upgrades the Windows facts license (wire layer — no op/stream to add; host-computed, confirmed no wire op):**
- `docs/wiki/Streams.md:37-44` — flip head-pose-not-a-stream from [HYPOTHESIS] to [CONFIRMED], point at `Head-Pose.md`.
- `docs/wiki/Op-Catalog.md:57,65-66` — mark `0x501`/`0x50e` [CONFIRMED]; delete "head-pose op, if one exists", replace with "none — host-computed".
- `docs/wiki/Reverse-Engineering-Methodology.md:85` — restate the open item as "extract the model + its I/O spec", not "the head-pose stream".
- `docs/wiki/Gaze-Stream.md:44-45` — cross-reference `EYE_POSITION_NORMALIZED` and note `USER_POSITION_GUIDE_XYZ` against the trackbox columns.

**Calibration / display import (all new code):**
- New `.setpm` reader: 12-byte header (`version, count, payload_len`) + payload.
- `screenplane → DisplaySetup` mapper: trivial, math verified in §6.1; target `crates/tobii-config/src/setup.rs` (`from_corners:87` / `[display]` TOML `from_toml:127`).
- Calibration tier decision (§6.3) **before** any calibration import is meaningful — the 796 KB host model is not the ≤4 KB device blob `apply_calibration` (op `0x456`, `connection.rs:138-186`, `calibration.rs`) expects.
- If per-monitor binding is wanted: a new persisted `EDID-id → calibration.bin` binding — does not exist today (`edid.rs` has `MonitorInfo` but no binding).

---

## 8. OPEN QUESTIONS — for the pending elevated read (`model.vino.xml`, `Head Tracker.cfg`, `pr_log0.txt`)

1. **Is `model.vino.xml` plaintext OpenVINO IR or ciphertext?** (~14 KB suggests plaintext IR.) If encrypted, the entire `openvino`-crate load plan (`ModelConfig`, `model.rs:53-59`) is blocked and a decrypt step is required.
2. **What is `model.vino`'s input tensor?** C=1 mono vs stereo-stacked (2-channel / side-by-side), H×W, and dtype. This decides whether `PoseModel::estimate(left, right)` (`model.rs:68`) passes `right: None` or both frames, and sets the true `size`/`channels` for `preprocess`.
3. **What is `model.vino`'s output layer(s)?** Euler (which order?) vs quaternion vs 3×3 matrix; number of outputs; whether position and rotation are separate heads. Determines the single conversion into `HeadPose` (`lib.rs:70-78`) — §4 claim 3.
4. **Does head pose come from `model.vino` (regressor) or from landmarks + `jk_300_rigid.fdp` PnP** (or a fusion)? §2 / §4 claim 6. If PnP, the Linux backend must also run the LBF landmark cascade, not just the `.vino` model.
5. **Position units — mm or m?** Sets `opentrack::TRANSLATION_SCALE` (`opentrack.rs:35`), currently `1.0` and UNVERIFIED — §4 claim 4.
6. **Coordinate frame — origin, axis directions, handedness, and rotation signs** of `headpose.position`/`headpose.rotation`. Must reconcile against the fallback's `+z`-toward-user (`lib.rs:4-13`) *and* the display subsystem's `+z`-backward (`display.rs`) — the latent inconsistency in §5. Fix points: `lib.rs:38-50` (signs), `lib.rs:4-13` (frame).
7. **Real preprocessing constants** from `Head Tracker.cfg` / VisionSDK: `min_face_scale`, `max_face_scale`, `CroppingAdjustment`, resize kernel (cubic vs Linux nearest-neighbour), normalization — replace the `k=1.5` / default `Normalize` / `pad_frac` placeholders (`preprocess.rs:9-10,60,117-163`).
8. **Confirm the two low-frequency enums** are distinct (`PRP_STREAM_ENUM_LOW_FREQUENCY_HEAD_POSITION` and `…_ROTATION`) and whether they differ in frame/units from the full-rate `HEADPOSE` — §5, `Head-Pose.md:49`.
9. **Calibration tier:** does the ET5 firmware accept only the ≤4 KB device blob, or can/should a sub-blob be extracted from the 796 KB `calibration.setpm` "human" model? Can `pr_log0.txt` reveal the runtime's own apply path (host gaze math vs firmware apply)? — §6.3, the biggest calibration unknown.
10. **Monitor-identity format:** does Windows format the Samsung product code as `SAM7463` for the same panel the Linux fixture records as `0x7454`? — §6.2, before trusting any per-monitor binding.
