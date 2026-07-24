# Tobii ET5 head-pose — definitive findings (Windows extraction, 2026-07-23/24)

Reverse-engineered from a fully-installed **Tobii Experience** (Store app
`TobiiAB.TobiiEyeTrackingPortal 1.69`) + **Eye Tracker 5**, platform runtime
`IS5LEYETRACKER5` v1.37.4 (algobox `f2b4e724c9-…-WingedMacaw`), module codename **castor**.
Files extracted to `C:\tobii-extract\platform-runtime\` (not committed — Tobii assets).

Confidence tags: **[CONFIRMED]** = directly observed in a file; **[INFERRED]** = strong
deduction; **[UNKNOWN]** = not yet determined.

## 1. Is the model encrypted? — YES [CONFIRMED]

Empirically verified on the actual files (not just inferred from the binary this time):

| Asset | Entropy | Verdict |
|---|---|---|
| `NN/model.vino.{xml,bin}` | 7.99 / 8.0 | **encrypted** |
| `NN/fr.vino.{xml,bin}`, `NN/vnn_nir/d1.vino.{xml,bin}`, `FF/vnn_nir/ff.vino.{xml,bin}` | ≈8.0 | **encrypted** |
| `LBF/lv`, `NN/fa.lbf`, `NN/fc.lbf`, `NN/is.bin` | 7.2–8.0 | **encrypted** (small plaintext header + ciphertext body) |
| all four `*.vino.xml` | — | share identical first 16 bytes `e2 65 f2 ab 00 64 7d aa 87 25 7b b0 43 45 06 0b` → common **AES** header/IV |

The `.vino.xml` are **not** plaintext OpenVINO IR — they are ciphertext. Decryption is done
in-memory by `VNN::Crypto::decryptBuffer` (AES via OpenSSL + Windows `BCryptDecrypt`) before
OpenVINO `ReadNetwork`. In addition, the files carry a restrictive **NTFS ACL** that denies
read to Administrators (bypassed only via `robocopy /ZB` backup privilege). So: two layers —
ACL + AES. The models never touch disk in plaintext.

**But the license check is separate:** the runtime validates an **RSA-signed license**
(`licensekey_check_public_key`, `pr_log0.txt:43`), which is not the model cipher.

## 2. What is plaintext and readable [CONFIRMED]

The whole classical pipeline *around* the neural nets is plaintext:

- **`Head Tracker.cfg`, Facial Features/Face Detector cfgs, `NeuralNet.cfg`** — all tuning params.
- **`jk_300.wfm`** — a **357-vertex 3D face mesh** (`# VERTEX LIST:`), i.e. a **CANDIDE-3** variant.
- **`jk_300_rigid.fdp`** — FDP landmark map, header `1.0 candide3.fdp` → the reference geometry
  is **CANDIDE-3** (Ahlberg, Linköping — a *public, academic* 3D face model, **not** Tobii IP).
- **`landmarks.bin`** (entropy 1.76), `init_shape.bin` — plaintext index/shape data.

## 3. How head pose is actually computed — the key finding [CONFIRMED + INFERRED]

Head pose is **not** a direct NN regression. It is a **3D morphable-model fit** (CANDIDE-style):

```
NIR frame(s) ─▶ [encrypted NNs: ff/d1 detect, fr + LBF cascade regress LANDMARKS]
             ─▶ fit CANDIDE-3 (jk_300.wfm) to the landmarks  (pose_fitting_*)
             ─▶ 6-DOF head pose  +  Action Units (expression)  +  Shape Units (identity)
```

Evidence (`Head Tracker.cfg`): `pose_fitting_model jk_300.wfm`, `pose_fitting_fdp jk_300_rigid.fdp`,
and the pose vector is explicitly **`#rx ry rz tx ty tz`**.

**⇒ Only the *landmark detector* is the encrypted secret. The pose itself is a classical
optimization over a *public* face model.** Any landmark source can drive the same fit.

### Exact conventions [CONFIRMED unless noted]
- **Rotation: Euler angles in radians**, order `rx, ry, rz`. Limits: `rx ∈ [-π/2, π/2]`,
  `ry ∈ [1.05, 5.24]` (yaw, offset origin), `rz ∈ [-3.2, 3.2]≈±π` (roll). Axis→(pitch/yaw/roll)
  mapping and signs: [INFERRED] rx=pitch, ry=yaw, rz=roll.
- **Translation: `tx ∈ [-4,4], ty ∈ [-3,3], tz ∈ [0,11]` in CANDIDE model units** — the mm scale
  factor is [UNKNOWN]; needs one ground-truth capture to calibrate (see §5).
- **`camera_focus`** = 2.6 (head tracker) / 3.0 (facial-features) — pinhole focal used in the fit.
- Face search `min/max_face_scale = 0.15/1.0`, `face_detector_sensitivity 0.6`.
- 23 Action Units (`au_names` list: jaw_drop, brow raisers, eye_closed, rotate_eyes…) + 40 Shape Units.
- Landmark groups: **eyebrows, mouth, pupils, eyelids, nose, contour, screen_space_gaze, ears**.
- Output stream rate ~**33 Hz** (`pr_log0.txt:27`).

Answers to the earlier open questions: pose representation (Q3) = Euler radians; source (Q6) =
model-fit, not NN regression; preprocessing constants (Q7) = the cfg values above; units (Q5) =
model units, mm-scale still open. The `.vino` tensor shapes (Q2) remain [UNKNOWN] (encrypted),
but are no longer on the critical path — see §4.

## 4. Recommended Linux approach — no decryption needed

Because the pose is a public-model fit, **you do not need Tobii's encrypted weights** to get
compatible head pose:

1. **Landmarks** from an open detector on the 280×280 NIR frames (or the existing preprocessing /
   an open face-landmark ONNX), **then**
2. **Reimplement the CANDIDE-3 rigid fit** using this config (`jk_300` geometry, the pose
   sensitivities/limits above) → 6-DOF in Tobii's convention. CANDIDE-3 is freely available.
3. Feed the result through the existing `PoseFilter → to_opentrack_datagram` path
   (`crates/tobii-headpose/`, see `docs/windows-headpose-integration-map.md`).

Even simpler, already-scaffolded: use **6DRepNet / opentrack ONNX** (backends #2/#3) for direct
head-pose regression; only pursue the CANDIDE fit if you want to match Tobii numerically.

## 5. On decrypting Tobii's model (the EU-law question)

Decrypting the `.vino` files **is** circumventing a technical protection measure (AES). The EU
Software Directive (2009/24/EC) Art. 6 interop exception and Art. 5(3) are genuinely more
permissive than US law, and EULA anti-RE clauses are void here (Art. 8) — but Art. 6's
**"necessary for interoperability"** prong is hard to satisfy now that §4 shows interop is
achievable **without** decryption. So decryption is both the **legally weakest** step and
**technically unnecessary**.

Recommendation: **don't decrypt.** If exact-fidelity to Tobii's landmarks is ever required for
personal use on your own device, the least-fraught route is observing your *own* licensed
process (the runtime already decrypts to memory) rather than attacking the cipher — but that is
out of scope here and not needed for a working Linux head-pose backend. **Never redistribute the
weights** regardless (the repo already ships none — `ModelKind::is_redistributable()==false`).

## 6. Where the files are
`C:\tobii-extract\platform-runtime\dependencies\` — encrypted `bdtsdata/`, plaintext cfgs,
`jk_300.wfm`, `jk_300_rigid.fdp`. Runtime log: `…\pr_log0.txt`. Calibration (user's own):
committed at `docs/TobiiSetupProcess/calibration/`. Integration contract:
`docs/windows-headpose-integration-map.md`.
