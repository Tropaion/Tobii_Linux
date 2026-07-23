# Gaze Stream (0x500)

The always-on data stream. Subscribed during the handshake; arrives as `NOTIFY`
frames with `op == 0x500`, payload ~1692 bytes, ~33 Hz, carrying **39 XDS
columns**. Source of truth: `crates/tobii-protocol/src/gaze.rs`
(`column_kind`, `GazeSample::decode`, `column_inventory`, and the captured
real-frame tests).

## Framing

The payload is one XDS row (see [[Encoding]]): a 2-byte prefix, an `xds_row`
prolog whose tag encodes the column count (`0x27` = 39), then 39 columns. Each
column is `xds_column prolog + u32(col_id) + a typed value`. The decoder reads
columns it models and skips the rest by their kind; it stops at the first
truncated or truly unknown column, returning the partial sample gathered so far.
**[CONFIRMED]** — `gaze.rs::decode`, `real_frame_payload` (1692 B) decodes.

## Column kinds

`column_kind()` maps each id to a TLV kind so unmodeled columns can be skipped:

| Kind | Column ids |
|------|-----------|
| s64 | `0x01` |
| point3d | `0x02 0x03 0x04 0x08 0x09 0x0a 0x17 0x18 0x22 0x24 0x25 0x27` |
| point2d | `0x05 0x0b 0x19 0x1a 0x1c 0x20` |
| fixed16x16 | `0x06 0x0c 0x29 0x2b` |
| u32 | `0x07 0x0d 0x0e 0x11 0x14 0x15 0x16 0x1b 0x1d 0x1e 0x1f 0x21 0x23 0x26 0x28 0x2a 0x2c` |

**[CONFIRMED]** — `gaze.rs::column_kind`.

## Complete column table

Meanings for the **modeled** columns are [CONFIRMED] (decoder + captured-frame
test). The remaining columns' kinds are [CONFIRMED]; their **semantics** come
from a live six-axis capture (2026-07-22, memory note
`et5-headpose-not-in-gaze-stream`) and are marked accordingly.

| Id | Kind | Field / meaning | Conf |
|----|------|-----------------|------|
| `0x01` | s64 | **timestamp**, microseconds | **[CONFIRMED]** |
| `0x02` | point3d | **eye origin L**, tracker-space mm | **[CONFIRMED]** |
| `0x08` | point3d | **eye origin R**, tracker-space mm | **[CONFIRMED]** |
| `0x03` | point3d | **trackbox eye L** — x/y normalized in the trackbox `[0,1]`, z = distance mm | **[CONFIRMED]** |
| `0x09` | point3d | **trackbox eye R** | **[CONFIRMED]** |
| `0x04` | point3d | **per-eye gaze direction L** (`gaze_point_3d_l`); x tracks yaw, y tracks pitch — where the eye *looks* | **[CONFIRMED]** decoder; direction semantics **[CONFIRMED]** live |
| `0x0a` | point3d | **per-eye gaze direction R** | **[CONFIRMED]** |
| `0x17` | point3d | **raw eye origin L** (pre-calibration detection output), mm | **[CONFIRMED]** |
| `0x18` | point3d | **raw eye origin R**, mm | **[CONFIRMED]** |
| `0x05` | point2d | **gaze point 2D L** (per-eye normalized on display area) | **[CONFIRMED]** |
| `0x0b` | point2d | **gaze point 2D R** | **[CONFIRMED]** |
| `0x1c` | point2d | **gaze point 2D** (combined, filtered) — normalized `[0,1]²`; `(-1,-1)` sentinel when invalid | **[CONFIRMED]** |
| `0x20` | point2d | **gaze point 2D unfiltered** | **[CONFIRMED]** decoder; label **[CODE-VERIFIED]** |
| `0x06` | fixed16x16 | **pupil diameter L**, mm | **[CONFIRMED]** |
| `0x0c` | fixed16x16 | **pupil diameter R**, mm | **[CONFIRMED]** |
| `0x07` | u32 | **validity L** (`0` = tracked; `4` = not detected) | **[CONFIRMED]** |
| `0x0d` | u32 | **validity R** | **[CONFIRMED]** |
| `0x14` | u32 | **frame counter** | **[CONFIRMED]** |
| `0x22` | point3d | eye/head **position** point-pair, ~45 mm above + ~15 mm behind the eye origins (moves with the head) — investigated as head-pose, ruled out | position **[CONFIRMED]** live; not head orientation |
| `0x24` | point3d | second of the higher position pair (with `0x22`) | **[CONFIRMED]** live |
| `0x25` | point3d | stays ~zero in captures | **[CONFIRMED]** live (value); meaning unknown **[HYPOTHESIS]** |
| `0x27` | point3d | stays ~zero in captures | **[CONFIRMED]** live (value); meaning unknown **[HYPOTHESIS]** |
| `0x19` `0x1a` | point2d | present; `(-1,-1)` / `(0,0)` sentinels in the no-eyes capture — likely more per-eye 2D gaze | **[HYPOTHESIS]** |
| `0x29` `0x2b` | fixed16x16 | present with `-1.0` sentinel in captures — likely more per-eye scalars (pupil/quality) | **[HYPOTHESIS]** |
| `0x0e 0x11 0x15 0x16 0x1b 0x1d 0x1e 0x1f 0x21 0x23 0x26 0x28 0x2a 0x2c` | u32 | **constant flags** — every one had range 0 across all six head axes; no orientation data | constant across motion **[CONFIRMED]** live; exact meaning **[HYPOTHESIS]** |

## Present-bit vs validity — the critical gotcha

`GazeSample` exposes a `present_mask` (see `gaze::present`) with a bit per
modeled field. **A set present bit means only "the column was in the frame", not
"this eye is being tracked".** The device sends the eye-origin, trackbox and
gaze columns on **every** frame and simply **zeroes** them when no eye is
detected (`validity == 4`).

> **Rule: gate eye/head presence on `validity == 0`, never on the present bit
> alone.** A present-bit-only check reports a head sitting exactly on the
> tracker's sensor.

**[CONFIRMED]** — `gaze.rs::decodes_real_device_gaze_frame_2026_07_15` (a
no-eyes frame: both validities `4`, both eye origins `[0,0,0]`, `gaze_point_2d`
`(-1,-1)`, yet all present bits set); `tobii-headpose::pose_from_sample` gates on
`validity == 0` for exactly this reason.

## No head pose here

A full six-axis live capture confirmed there are **no Euler angles and no
quaternion** anywhere in the frame: point3d columns are all eye/position
geometry, `0x04`/`0x0a` are gaze *directions*, and every unmapped integer column
is a constant flag. Pitch is not even recoverable from the point geometry. See
[[Head-Pose]]. **[CONFIRMED]** — memory `et5-headpose-not-in-gaze-stream`,
`gaze.rs::unmapped_point3d_columns_are_eye_positions_not_head_pose`.
