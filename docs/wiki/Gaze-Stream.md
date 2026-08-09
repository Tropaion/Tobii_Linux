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
| `0x03` | point3d | **trackbox eye L** — x/y **and z** all normalized in the trackbox `[0,1]`; z is *not* mm (use `0x02`/`0x08` for a real distance) | **[CONFIRMED]** live |
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
| `0x22` | point3d | **eye origin L, display-space mm** — `0x02` rotated **exactly −20.00° about x** and translated | **[CONFIRMED]** live (see below) |
| `0x24` | point3d | **eye origin R, display-space mm** (same transform from `0x08`) | **[CONFIRMED]** live |
| `0x25` | point3d | **trackbox eye L, display-space**, normalized | **[CONFIRMED]** live |
| `0x27` | point3d | **trackbox eye R, display-space** | **[CONFIRMED]** live |
| `0x15` | u32 | **eye-present R** — `1` exactly when `0x0d == 0` (400/400 frames). Note the **crossed** ordering: `0x15` tracks the *right* eye | **[CONFIRMED]** live |
| `0x16` | u32 | **eye-present L** — `1` exactly when `0x07 == 0` (400/400) | **[CONFIRMED]** live |
| `0x1b` | u32 | **binocular flag** — `{0,1}` | **[CONFIRMED]** live (values); label **[HYPOTHESIS]** |
| `0x1d` `0x1e` `0x1f` | u32 | **gaze validity** combined / L / R (`1` = valid). Independent of eye validity `0x07`/`0x0d` — they disagree on ~9% of frames | **[CONFIRMED]** live |
| `0x21` | u32 | **unfiltered-gaze validity** — identical to `0x1d` in 400/400 frames | **[CONFIRMED]** live |
| `0x23` `0x26` `0x28` | u32 | validity companions to the display-space columns, `{0,1}` | **[CONFIRMED]** live (values) |
| `0x11` | u32 | constant `4` in every frame captured | **[CONFIRMED]** live |
| `0x2a` `0x2c` | u32 | constant `0` in every frame captured | **[CONFIRMED]** live |
| `0x19` `0x1a` | point2d | present; `(-1,-1)` / `(0,0)` sentinels in the no-eyes capture — likely more per-eye 2D gaze | **[HYPOTHESIS]** |
| `0x29` `0x2b` | fixed16x16 | present with `-1.0` sentinel in captures — likely more per-eye scalars (pupil/quality) | **[HYPOTHESIS]** |
| `0x0e` | u32 | not observed carrying a non-zero value | **[HYPOTHESIS]** |

> **Correction (2026-08-09).** This table previously called `0x25`/`0x27`
> "stays ~zero" and grouped `0x15 0x16 0x1b 0x1d 0x1e 0x1f 0x21 0x23 0x26 0x28`
> as "constant flags — range 0 across all six head axes". Both were artefacts of
> reading a capture in which **no eyes were ever detected**: the device zeroes
> every eye-derived column when `validity == 4`, so the whole block reads
> constant. A 400-frame capture with a user in view shows all of them varying.
> Cross-checked against the independent decoder in `njmill/tobii-linux`
> (`tools/probes/tobii-ttp-mux.c`), which names the same ids — except that it
> labels `0x15`/`0x16` left/right, which the live data shows is **swapped**.

## Display-space columns and the tracker's 20° tilt

The ET5 emits every eye position **twice**: once in tracker-space and once in a
display-space frame. Fitting the two against 589 live samples gives an exact
rigid transform (max residual **0.000 mm**):

```text
x' = x                            - 4.85 mm
y' =  cos20°·y + sin20°·z       - 212.71 mm
z' = -sin20°·y + cos20°·z        + 14.81 mm
```

The rotation is exactly **−20.00°** about x: the ET5's fixed mounting tilt (the
camera looks upward past the bottom bezel). The translation is the display-area
offset. This makes `0x22`/`0x24` genuinely useful — they answer "where are the
eyes relative to the *screen*" without the caller having to know the tilt.

The **normalized** display-space trackbox (`0x25`/`0x27`) is *not* re-centred by
this: it tracks `0x03`/`0x09` to within ~0.003 in x and y. There is no
better-centred position signal hiding in these columns. **[CONFIRMED]** live.

## Sibling streams

| Id | What | Status |
|----|------|--------|
| `0x0500` | gaze — this document | subscribed by us |
| `0x0501` | eye-camera image (secondary) | decoded, `camera.rs` |
| `0x050e` | eye-camera image (primary) | decoded, `camera.rs` |
| `0x0508` | named "image collection" by `njmill/tobii-linux` | **[UNCONFIRMED]** — subscribing it produced no frames here |
| `0x1770` | **algodbg** — 38-byte payload, one u32 column, constant `0` at ~133 Hz | **[CONFIRMED]** live — carries no data |
| `0x1771` | **sync** — 73-byte payload, two s64 columns: `0x01` device timestamp µs, `0x02` a second, consistently *earlier* timestamp | **[CONFIRMED]** live |
| `0x1772` | **log** — the device's own text log | **[CONFIRMED]** live |
| `0x1774` | named "custom" by the third-party catalog | **[UNCONFIRMED]** |

These ids came from `njmill/tobii-linux`; our own sweep only ever covered
`0x501..=0x520`, so they had never been probed here. Verified by
`tobii probe-streams 1770 1780` — `0x1770`, `0x1771` and `0x1772` all deliver.

`0x1770` runs at **~133 Hz**, four times the gaze rate, which made it a
promising home for internal per-eye state. It is not: the single column reads
`0` in every frame. Do not spend time here again.

`0x1772` is a genuine find — the device streams its **own log** as
length-prefixed ASCII (TLV type `0x14`), with uptime, severity and subsystem:

```text
[ 17340.014877] (I) PROT: Client 0 qid 260 subscribing for 'log' stream (id 6002)
[ 17382.143495] (I) USB pow: Requesting low power
```

Worth wiring into diagnostics. The power-state lines in particular are the
device narrating something we otherwise have to infer.

The `0x1771` pair is a device↔host clock reference: the two stamps sat
**10.7 ms and 11.0 ms** apart across consecutive frames. That bounds transport
latency and is the tool to reach for before blaming the display for "lag".
We do not subscribe it today.

Op `0x04ce` is used as unsubscribe/stream-disable by `njmill/tobii-linux`; we
have no equivalent and have not exercised it. **[UNCONFIRMED]** here.

## No separate user-position stream

The eye-position display is driven by the trackbox columns above. There is **no
distinct device stream** carrying head or user position: an independent
implementation (`njmill/tobii-linux`) subscribes only `0x0500` (gaze),
`0x0501`/`0x0508`/`0x050e` (images) and `0x1771` (sync). The Windows SDK's
`UserPositionGuide` is computed host-side from these same columns, not
subscribed from the device. **[CONFIRMED]** — cross-implementation agreement.

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

The zeroing is **total**, so no alternative gate can recover data the validity
gate rejects: across 400 live frames there were **0** frames in which `0x07`/
`0x0d` said "not detected" yet the matching trackbox column held a non-zero
value. Every candidate gate (`0x1e`/`0x1f`, `0x26`/`0x28`, non-zero trackbox,
non-zero display trackbox) admits the same 91% of frames. If a dot is missing,
the fix is upstream of the wire — aim, lighting, occlusion — or reconstruction
from the *other* eye (`eyeview::PairOffset`), never a looser gate.
**[CONFIRMED]** live.

## No head pose here

A full six-axis live capture confirmed there are **no Euler angles and no
quaternion** anywhere in the frame: point3d columns are all eye/position
geometry, `0x04`/`0x0a` are gaze *directions*, and every unmapped integer column
is a constant flag. Pitch is not even recoverable from the point geometry. See
[[Head-Pose]]. **[CONFIRMED]** — memory `et5-headpose-not-in-gaze-stream`,
`gaze.rs::unmapped_point3d_columns_are_eye_positions_not_head_pose`.
