# Tobii Experience calibration capture (ET5)

Captured 2026-07-23 from a Windows machine running **Tobii Experience** (Store app
`TobiiAB.TobiiEyeTrackingPortal 1.69.32600.0`) with an **Eye Tracker 5** after a
successful device setup + calibration.

**Source:**
`C:\ProgramData\Tobii\Tobii Platform Runtime\IS5LEYETRACKER5\IS5FF-100203330104\`
(`IS5FF-100203330104` = the tracker's serial; `IS5LEYETRACKER5` = ET5 platform module "castor".)

These are **the user's own calibration data** for their own device, kept here as reference
for improving the Linux-side calibration code. Note they contain personal / machine-specific
identifiers (see `displayinfo.setpm`) — scrub before any public distribution if that matters.

## Files

| File | Size | What it is |
|------|------|------------|
| `calibration.setpm`  | 796,076 B | The personalized eye calibration payload (per-eye model). |
| `displayinfo.setpm`  | 276 B     | Which monitor the calibration is bound to (Windows display path + EDID id). |
| `screenplane.setpm`  | 48 B      | The screen-plane geometry (3D corner points) in the tracker frame. |

SHA-256 (first 16 hex): calibration `36FF03BF66F51746`, displayinfo `B769D8F669618C3C`,
screenplane `DF43BDE4471183CD`.

## `.setpm` container format (observed)

All three share a 12-byte little-endian header:

```
u32 version   = 0x00000004
u32 count     = 0x00000001
u32 payload_len            # bytes following the header
... payload ...
```

- **`screenplane.setpm`** — payload_len `0x24` (36 B) = **9× float32** → three 3D points
  (screen-plane corners, tentatively mm, in the tracker coordinate frame). Decoded:
  - P1 ≈ (-596.50,  10.27,  -3.10)
  - P2 ≈ (-596.50, 325.56, 111.66)
  - P3 ≈ ( 596.50, 325.56, 111.66)
  Interpretation (which corner is which, exact units/axis signs) still to be confirmed
  against `docs/wiki/Display-Area.md` and `docs/superpowers/specs/*display-setup-math*`.

- **`displayinfo.setpm`** — payload is a plaintext string:
  `DISPLAY\SAM7463\5&21C6DF7E&0&UID4352##eId:4C2D6374414D52302523#or:1#v:1.0`
  (Windows display device instance path + EDID id `eId:…`, orientation `or:1`, version `v:1.0`).

- **`calibration.setpm`** — after the header + a small record containing the ASCII marker
  `human` (profile/model type), the bulk (~796 KB) is **entropy ≈ 8.0 bits/byte**, i.e.
  compressed or encrypted. Not yet decoded; compare with
  `crates/tobii-protocol/src/testdata/real-calibration.blob` and `docs/wiki/Calibration.md`.

## Related

- `docs/wiki/Calibration.md`, `docs/wiki/Display-Area.md`
- `crates/tobii-protocol/src/testdata/real-calibration.blob`
- `docs/windows-extract-tobii-headpose-model.md` (the head-pose model extraction plan)
