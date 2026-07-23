# Windows session plan — extract the Tobii head-pose model + spec

**Paste this whole file as the opening message of a parallel Claude Code session
running on the Windows machine that has Tobii Experience / Eye Tracker 5
installed and working.** It is self-contained.

---

## Who you are and what you're doing

You are helping reverse-engineer the Tobii Eye Tracker 5 (ET5) so its **head
tracking** can be reproduced on Linux (project: TobiiLinux, a clean-room GPL
reimplementation; repo <https://github.com/Tropaion/Tobii_Linux>). The Linux
side is done up to the point where it needs Tobii's neural model.

**What the Linux session already established (facts — you can rely on these):**
- Head pose is **not** on the USB wire. The device only streams gaze + two
  near-infrared **camera images**. Head pose is computed **on the PC** by Tobii's
  service (`platformservice.exe`) using an **Intel OpenVINO** model, then exposed
  through the stream-engine client API as `PRP_STREAM_ENUM_HEADPOSE`
  (`headpose.position` + `headpose.rotation`).
- The model is referenced in `platformservice.exe` strings as
  **`bdtsdata/NN/model.vino.xml`** + **`bdtsdata/NN/model.vino.bin`** (OpenVINO
  IR format), loaded via `ReadNetwork` / `LoadNetwork`.
- The two cameras are a **stereo pair**, each **280 × 280, 8-bit grayscale**,
  full-face wide-angle, ~33 Hz (Linux decodes these already).

**Your goal:** get the model file(s) **and** everything needed to run inference
identically on Linux — the exact input preprocessing, the output meaning
(what numbers = position/rotation, in what units and coordinate frame), and, if
possible, a few ground-truth (image → pose) pairs to validate against.

This model is **Tobii's proprietary IP**. Everything here is for the **user's own
personal use** with their own licensed device. Do **not** publish or redistribute
the model or weights. The Linux code will *load* a user-supplied model, never
ship one.

## Prerequisites to confirm first

- Tobii Experience (or Tobii Eye Tracker 5 software) installed, ET5 plugged in,
  head tracking working (verify in the Tobii settings/preview that it tracks your
  head).
- Windows PowerShell (Admin).
- Python 3 available (for model inspection). You'll `pip install openvino onnx`.
- Optional (for the hard parts): [Ghidra](https://ghidra-sre.org/) or IDA for the
  native decompile; the Tobii Stream Engine SDK for the validation capture.

Create a working folder to collect deliverables: `C:\tobii-extract\`.

---

## Task 1 — Locate and copy the model files (MUST)

Search the whole system for OpenVINO / ONNX model files and the `bdtsdata\NN`
directory. In an **Admin PowerShell**:

```powershell
$dst = "C:\tobii-extract"; New-Item -ItemType Directory -Force $dst | Out-Null

# 1a. Find every model-ish file anywhere (may take a few minutes)
Get-ChildItem -Path C:\ -Recurse -ErrorAction SilentlyContinue `
  -Include *.vino.xml,*.vino.bin,*.onnx,*.blob,model.xml,model.bin |
  Select-Object FullName,Length | Tee-Object "$dst\model-files.txt"

# 1b. Find the bdtsdata/NN directory specifically
Get-ChildItem -Path C:\ -Recurse -Directory -ErrorAction SilentlyContinue `
  -Filter "NN" | Where-Object { $_.FullName -match "bdtsdata" } |
  Select-Object FullName | Tee-Object "$dst\nn-dirs.txt"
```

Common roots if the recursive search is slow: `C:\Program Files\Tobii`,
`C:\Program Files (x86)\Tobii`, `C:\ProgramData\Tobii`, `$env:LOCALAPPDATA\Tobii`,
`$env:APPDATA\Tobii`, `$env:ProgramData\Tobii\Tobii Eye Tracking`.

**Copy the ENTIRE `bdtsdata\NN\` folder** (there may be several models — head
pose, a face/eye detector, a low-frequency head model, plus config/label files —
grab all of them):

```powershell
# adjust the source path to the one found above
$nn = "<PATH FROM nn-dirs.txt>\bdtsdata\NN"
Copy-Item -Recurse $nn "$dst\NN"
Get-ChildItem -Recurse "$dst\NN" | Select-Object FullName,Length | Tee-Object "$dst\NN-listing.txt"
```

Also copy any config/metadata sitting next to the models (`*.json`, `*.xml`,
`*.ini`, `*.txt`, `*.yaml`, label files). These often hold the input
normalization and class/output definitions.

**Deliverable 1:** `C:\tobii-extract\NN\` with every model + sibling file, and the
three `*.txt` listings.

---

## Task 2 — Dump each model's input/output spec (MUST)

For **every** `.xml`/`.onnx` you found, record its inputs and outputs. The `.xml`
is plaintext OpenVINO IR — but also load it programmatically for the exact
shapes, types, and layouts.

```powershell
pip install --quiet openvino onnx numpy
```

```python
# save as C:\tobii-extract\inspect.py and run: python C:\tobii-extract\inspect.py
import glob, os
from openvino.runtime import Core
core = Core()
for path in glob.glob(r"C:\tobii-extract\NN\**\*.xml", recursive=True) + \
            glob.glob(r"C:\tobii-extract\NN\**\*.onnx", recursive=True):
    print("="*70); print(path)
    try:
        m = core.read_model(path)
        for p in m.inputs:
            print(f"  INPUT  {p.get_any_name():30} shape={p.get_partial_shape()} "
                  f"type={p.get_element_type()}")
        for p in m.outputs:
            print(f"  OUTPUT {p.get_any_name():30} shape={p.get_partial_shape()} "
                  f"type={p.get_element_type()}")
    except Exception as e:
        print("  (openvino read failed:", e, "- open the .xml as text instead)")
```

Save its full output to `C:\tobii-extract\model-io.txt`.

**What we need to learn from this (call it out explicitly in your notes):**
- **Input:** shape `[N, C, H, W]`. Is `C` = 1 (one grayscale camera) or 2/6
  (stereo / stacked)? Is `H×W` = `280×280`, or a crop/resize like `224×224` or
  `128×128`? Element type (u8 / fp16 / fp32)?
- **Output(s):** how many values and their shape. Is it one vector of 6 (pose),
  separate `position`(3) + `rotation`(3 or 4)? Is rotation Euler (3) or a
  quaternion (4) or a 3×3 matrix (9)? Any auxiliary outputs (landmarks,
  confidence)?
- Open the `.xml` head/tail in a text editor and paste the first `<layer>`
  (input/Parameter) and the last few layers (outputs/Result) into the notes —
  layer names sometimes reveal semantics (e.g. `head_translation`, `euler`,
  `quat`).

**Deliverable 2:** `model-io.txt` + a short notes paragraph answering the bullets
above.

---

## Task 3 — Find the preprocessing + output meaning (BEST-EFFORT, high value)

The model input/output shapes don't reveal *how* the 280×280 camera image is
turned into the input tensor, or how the output numbers map to a real head pose.
That logic is in `platformservice.exe` (native C++). In priority order:

1. **Look for a config/metadata file** shipped with the model (Task 1 siblings).
   Many OpenVINO pipelines carry a JSON/YAML with `mean`, `scale`, `input_size`,
   `crop`, `reverse_input_channels`, etc. If present, copy it — that may be the
   whole answer.

2. **Strings pass** on `platformservice.exe` and any `*headpose*` / `*platform*`
   DLLs — look for hints near the model path:
   ```powershell
   # if you have sysinternals 'strings', else use the python below
   Select-String -Path "C:\Program Files\Tobii\**\platformservice.exe" -Encoding Byte `
     -Pattern "mean|scale|normali|crop|resize|input|width|height|pose|euler|quat|radian|degree|mm" `
     -ErrorAction SilentlyContinue | Select-Object Line -First 200
   ```

3. **Decompile (if you have Ghidra/IDA):** import `platformservice.exe`, find the
   calls to OpenVINO `ReadNetwork`/`LoadNetwork`/`infer`/`SetBlob`/`GetBlob`, and
   read the code that fills the input blob (preprocessing) and reads the output
   blob (pose mapping). Capture: crop rectangle, resize, channel handling,
   normalization constants, and how output floats become position (mm?) +
   rotation (Euler radians/degrees? quaternion? which axis order?), and the
   coordinate frame (origin, +x/+y/+z direction).

**Deliverable 3 (best-effort):** any config file found, plus notes on
preprocessing (crop/resize/normalization) and output mapping (units, rotation
representation, coordinate frame, signs). Even partial findings help a lot; the
Linux side can fill gaps by experimentation once it has the model.

---

## Task 4 — Capture ground-truth image→pose pairs (BEST-EFFORT, for validation)

So the Linux inference can be verified to match Tobii's, capture the head pose
Tobii reports *while you move your head in known ways*.

Easiest route — the **Tobii Stream Engine SDK** (`tobii_stream_engine.dll`,
already on the machine). It has a documented head-pose API. Write a tiny program
(C or a Python `ctypes` wrapper) that:
- connects to the device (`tobii_device_create`),
- subscribes to head pose (`tobii_head_pose_subscribe`),
- prints, ~30×/s, the timestamp + position (x,y,z) + rotation for a minute.

Run it and, on camera, perform each motion in isolation for a few seconds while
narrating to a log: **look straight → turn left → right → nod up → down → tilt
left → right → lean in → out → move left → right → up → down.** Save the printed
stream to `C:\tobii-extract\headpose-log.txt` with your motion annotations.

If the SDK program is too much, a fallback: open the Tobii settings/preview that
shows head position and screen-record it while moving; we can read approximate
values from the video. Any signal on **which output changes for which motion,
and its sign/units** is what we need.

**Deliverable 4 (best-effort):** `headpose-log.txt` (+ the tiny program source if
you wrote one), annotated with the motions.

---

## What to bring back to the Linux session

Zip `C:\tobii-extract\` and hand it over. Minimum viable = **Task 1 + Task 2**
(the model + its I/O spec) — with those, the Linux side can start running the
model and iterate on preprocessing. Tasks 3 and 4 make it exact and are worth the
effort if the tools are available.

Summarize your findings in `C:\tobii-extract\FINDINGS.md`:
- exact paths + sizes of the model files,
- model input shape/type and output shape(s)/meaning,
- preprocessing (crop/resize/normalization) if found,
- output convention (units, rotation representation, coordinate frame, signs) if
  found,
- anything surprising (multiple models and what each seems to do, versioning,
  encryption, etc.).

## Reference — the exact Linux-side context you're feeding

The Linux code will build a **model-agnostic head-pose adapter** with three
backends (Tobii `model.vino`, opentrack's free `head-pose-*.onnx`, and 6DRepNet),
each defined by: input preprocessing (from the 280×280 stereo NIR frames) →
inference (`ort`/`openvino` crate) → output mapping to 6-DOF → opentrack UDP.
Tobii's model is backend #1, so the precise input/output spec above is exactly
what its adapter needs. The camera decode, the opentrack output, and a 5-DOF
eye-origin fallback already exist on the Linux side.
