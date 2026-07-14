# Spike S3 — Display-Setup Math (findings)

**Date:** 2026-07-14
**Status:** Complete. Feeds Plan 4 (`tobii-config`).
**Method:** Decompiled the offline installer MSI's managed assemblies to C# with
`ilspycmd` and cross-referenced the `tobiifree` reference + our `tobii-protocol`
crate. Findings were produced by a 6-way parallel read of the decompiled source,
synthesized, and adversarially verified (the verifier re-implemented the math and
numerically reproduced the golden vector; all load-bearing claims independently
re-confirmed here).

## TL;DR

**The authoritative forward setup math — `(monitor size, mounting, offset, tilt) →
3 tracker-space corners` — is NOT in the decompilable managed code. It lives in the
native `TetConfig.dll` (PE32+ x86-64).** The managed layer only *collects* setup
inputs and writes them to the Windows registry (`HKLM\Software\Tobii\EyeXConfig`),
then *reads the finished corners back* from the device. There is no `SetDisplayArea`
in managed code at all.

Therefore we **cannot lift Tobii's exact math** without native reverse-engineering
of `TetConfig.dll`. Instead we reconstruct an equivalent **planar-rectangle model**
(recovered from the `tobiifree` reference and first principles) that reproduces a
real, working display area — the "golden vector" — to **< 0.1 mm**. This is the
recommended basis for `tobii-config`.

### Evidence (independently re-verified)

| Claim | Check | Result |
|---|---|---|
| No tilt/angle math in managed code | `grep -rin tilt <decompiled>` | **0 hits** |
| Managed code never *writes* a display area | `grep -rn SetDisplayArea` | **0 hits** |
| Managed code only *reads* it back | `grep -rn GetDisplayArea` | 2 reads (`AdvancedGazeSingletonSubscription.cs:54,177`) |
| `Parallelogram3` is only parsed, never computed | `grep -rn 'new Parallelogram3('` | only `Empty` (`Parallelogram3.cs:13`) + wire-parse (`ParallelogramHelper.cs:20`) |

## 1. Data model (recovered from managed code)

- **`Parallelogram3 { Vector3 TopLeft, TopRight, BottomLeft }`** — the display area is
  exactly three tracker-space corners in **mm** (doubles); **BottomRight is never
  stored**, implied by `BR = TR + BL − TL`
  (`Tobii.Tech.NETCommon.Mathematics/Parallelogram3.cs:7-27`). Wire tags map
  `UL→TopLeft, UR→TopRight, LL→BottomLeft`
  (`…TrackingDevice/Utility/ParallelogramHelper.cs:17-20`).
- **`Vector3`** — right-handed; `Up=(0,1,0)`, `Right=(1,0,0)`, `Backward=(0,0,1)`,
  `Forward=(0,0,-1)` (`Mathematics/Vector3.cs:19,23,27-29`).
- Physical monitor mm comes from **EDID** (`Displays/EdidDataInfoReader.cs:133-146`),
  not a user-typed field; it can fall back to a 96-dpi guesstimate.
- **`Mounting`** enum (`Peripheral / InternalDisplay / InternalBase / ExternalDisplay`)
  and **`MonitorRotation`** (discrete OS orientation `Identity/90/180/270`, *not* a
  continuous tilt) are pushed to the registry and consumed **natively**
  (`…TrackingDevice/Config/ConfigurationManager.cs:459-520,748-755`).

The only vector geometry in managed code is the **inverse** consumption of a
finished area — `CreateTransformationMatrixFromDisplayArea`
(`AdvancedGazeSingletonSubscription.cs:~193-242`), i.e. corners → the tracker→screen
gaze transform:

```
C = (TR + BL)/2        R = (TR − TL)/2        U = (TL − BL)/2       N = R × U
screenArea = (|TR−TL|, |BL−TL|)          # (width, height) in mm
# tracker→normalized-display matrix = Translate(−C) · inverse([R̂; Û; N̂])
```

Run backwards this confirms `TL = C − R + U`, `TR = C + R + U`, `BL = C − R − U`,
but **no setup input ever enters** — the corners are its input, not its output. We
reuse this relationship for the inverse direction (`tobii display get`).

## 2. The model we will implement (planar rectangle, "Model B")

Recovered from `tobiifree`'s `applyRectToCorners`
(`applications/tobiifree-demo/src/main.ts:677-701`) plus the golden vector. The
display area is a **flat rectangle** with a **level top edge** (no roll/yaw),
parametrized by width, height, a bottom-edge reference point, and a tilt:

```
# forward: params → corners
tilt_mm = height_mm · sin(tilt_deg)         # z-displacement of top edge over bottom
dy      = height_mm · cos(tilt_deg)         # == sqrt(height² − tilt_mm²)
half_w  = width_mm / 2
BL = ( cx − half_w,  cy,        cz )
TL = ( cx − half_w,  cy + dy,   cz + tilt_mm )
TR = ( cx + half_w,  cy + dy,   cz + tilt_mm )
# BR implied = TR + BL − TL

# inverse: corners → params
width_mm  = TR.x − TL.x
height_mm = hypot(TL.y − BL.y, TL.z − BL.z)
tilt_deg  = atan2(TL.z − BL.z, TL.y − BL.y)
cx = (TL.x + TR.x)/2     cy = BL.y     cz = BL.z
```

**Sign/convention decisions (locked in):**
- `tilt` is expressed to the **user as an angle in degrees** (spec's "screen tilt"),
  internally as the mm z-displacement `tilt_mm = h·sin(θ)`. Positive tilt = top edge
  at higher `+z` than the bottom (top leans toward `Backward`).
- We adopt `TL.z = cz + tilt_mm` (the tobiifree **doc-comment** convention, line 628),
  which is the exact negation of the shipped code's `cz − tEff` (line 697). Either
  produces identical corners; we pick the `+` form so forward/inverse round-trip with
  a positive angle for the golden vector. **Do not copy the shipped code's sign
  verbatim.**
- `tEff` is clamped to `[−h, +h]` so the tilted edge length stays exactly `h`
  (equivalently, `|tilt_deg| ≤ 90°`).

## 3. Golden-vector validation

Source: `ressources/tobiifree/calibrations/manual-2026-04-06.json` — a *"manually
nudged calibration — works very well"* for an Acer Predator X34P (34" curved
ultrawide). Corners (ground truth):
`TL=(−451.8, 413.6, 157.5)  TR=(479.8, 413.6, 157.5)  BL=(−451.8, 68.0, −11.0)`.

Inverse → our params: **w = 931.6, h = 384.49, tilt = 26.0°, offset (cx,cy,cz) =
(14.0, 68.0, −11.0)**. Forward from those params reproduces the corners to
**< 0.1 mm** (max residual 0.25 mm vs the JSON's `screen_rect`, fully explained by
its 0.5-step hand-authored slider rounding — the corners, not the rect, are the
source of truth).

## 4. Wire encoding (already implemented in `tobii-protocol`)

`SET_DISPLAY_AREA` (op `0x5a0`) payload = `00 00` + point(TL) + point(TR) + point(BL)
+ tag `0x10100` + u32 `0x3039`. Each point = tag `0x031f41` + 3 × **Q42** big-endian
signed `i64` (`round(mm · 2⁴²)`). Encode/decode already live in
`crates/tobii-protocol/src/{commands.rs:58-79, display.rs, tlv.rs}` — `tobii-config`
only produces corners and hands them to `build_set_display_area_corners`.

## 5. Open questions / risks

1. **Tobii's true forward math is native (`TetConfig.dll`) and unobserved.** We are
   *replacing* it with Model B, validated on **one** golden vector. Confirm against
   more real configs and/or a live device.
2. **"Monitor size" input ≠ raw physical EDID size.** The golden's 931.6 × 384.5 mm
   is ~15–17 % larger than the Acer's physical ~794 × 340 mm — the hand-nudged area
   deviates from the panel to improve accuracy (curve, parallax, calibration). Model
   B validates the *geometry*, not a physical→area mapping. For a first setup we seed
   the params from physical monitor mm; true fit is refined later (nudge / Phase-2
   calibration). Document this honestly in `tobii setup`.
3. **Level top edge assumed** (no roll/yaw): `TL` and `TR` share y and z. True for
   all observed cases; a rolled screen would need the general parallelogram.
4. **Mounting type & non-Identity rotation** effects are native-only (invisible in
   managed code). Portrait/flipped setups are unverified — out of scope for v1.
5. **Undocumented constants** in the wire payload: the leading `00 00` and trailing
   `tag 0x10100 + u32 0x3039 (=12345)` are unexplained in both our crate and
   tobiifree. Left as-is (they work).

## 6. Impact on the design spec

Supersedes the §3 / §8 assumption that `tobii-config` would "match the original
driver's math exactly." The exact math is native and not recoverable without native
RE (deemed not worth it for v1). `tobii-config` implements the validated planar
Model B, exposing the spec's intended **inputs** (monitor W/H mm, mounting offset &
vertical offset, screen tilt angle) and persisting them to our own TOML.
