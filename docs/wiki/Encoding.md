# Encoding: TLV, Q42, and XDS

The TTP payload body is a **TLV** (type-length-value) byte stream. Numbers are
either integers or **Q42** fixed-point. Stream frames (gaze, event) wrap their
TLV fields in an **XDS** row/column structure. Source of truth:
`crates/tobii-protocol/src/tlv.rs` and `bytes.rs`.

## TLV field header

Every field in a **request** payload (and inside stream frames) is:

```
[type : u8][size : u32 big-endian][body : `size` bytes]
```

Known types (`tlv.rs`):

| type | Rust reader | Body | Meaning | Status |
|-----:|-------------|------|---------|--------|
| `0x01` | `read_enum4` | 4 B BE | small enum (a third party calls it a boolean — see below) | **[CONFIRMED]** type, **[HYPOTHESIS]** meaning |
| `0x02` | `read_u32` | 4 B BE | unsigned 32-bit integer | **[CONFIRMED]** |
| `0x03` | `read_fixed16x16` | 4 B BE | signed 16.16 fixed-point → `i32 / 65536.0` | **[CONFIRMED]** |
| `0x04` | `read_fixed22x42` | 8 B BE | **Q42** fixed-point → `i64 / 2^42` | **[CONFIRMED]** |
| `0x05` | `read_prolog_tag` | 4 B BE | struct **prolog**: a 4-byte tag introducing a struct | **[CONFIRMED]** |
| `0x06` | `read_s64` | 8 B BE | signed 64-bit integer (used for timestamps) | **[CONFIRMED]** |
| `0x14` | `read_string` | `4 + n` B | ASCII text: a 4-byte BE count `n`, then `n` bytes | **[CONFIRMED]** |
| `0x15` | (`camera.rs`) | `4 + n` B | counted blob: same shape, used for the camera pixel buffer | **[CONFIRMED]** structure |

The **[CONFIRMED]** rows are backed by `tlv.rs` readers/writers plus round-trip
and reader tests, and by the live payloads worked through below.

### `0x01` — a 4-byte enum, probably not a boolean

The type itself is **[CONFIRMED]** live (2026-08-09): the whole 69-byte
`presence` payload (stream `0x504`) is

```
00 00
05 00000004 00020bb8      row, 2 columns
05 00000004 00020bb9      column prolog
02 00000004 00000001        column id 1
06 00000008 00000004ab0c6f90  timestamp
05 00000004 00020bb9      column prolog
02 00000004 00000002        column id 2
01 00000004 00000002        <-- type 0x01, body = 2
```

A third-party decoder calls this type a boolean. **That reading looks wrong.**
The only value we have ever seen is **2**, captured with nobody in front of the
tracker — which no plain true/false encoding produces. So `read_enum4` returns
the raw `u32` and lets the caller decide; a `bool` would have cheerfully
reported "present" for an empty room.

Sit in front of the tracker and run `tobii dump-stream 504 1` to get the other
value, and this becomes a named enum. **[HYPOTHESIS]** for the meaning.

### `0x14` — ASCII text

**[CONFIRMED]** live (2026-08-09), from bytes we hold verbatim rather than from
character-counting a transcript. The stream-catalog reply (op `0x4b0`) contains
nine of these, e.g.

```
14  00 00 00 08            type 0x14, TLV size = 8
    00 00 00 04            character count = 4
    67 61 7a 65            "gaze"
```

and the empty second string of each record is `14 00000004 00000000`, which pins
the `+ 4` exactly. `read_string` enforces `size == count + 4` and errors
otherwise. Stream `0x1772` (the device's own text log, see [[Gaze-Stream]]) uses
the same framing and decodes cleanly through `tobii log`.

An earlier revision of this page rated the body layout a hypothesis reconstructed
from one log line whose hex was never written down, and printed four invented
bytes as though they had been captured. The catalog reply settles the question
from real bytes; the invented example is gone.

Type `0x15` (camera pixel blob, `camera.rs`) has the same `4 + n` shape. Its
4-byte prefix was recorded as starting `00 01 …` on a 280 × 280 frame, and
`280 × 280 = 78 400 = 0x00013240` — consistent with the prefix being the same
big-endian count, though only two of the four bytes were written down.
**[HYPOTHESIS]** for the blob prefix specifically; the `4 + n` framing itself is
**[CONFIRMED]** by the pixel-count check in `decode_camera_frame`.

Treat that corroboration as weak: the only four prefix bytes recorded anywhere
in the repo are `00 01 00 00`, in the *synthetic* fixture
`camera.rs::synth_frame`. If those came from the capture rather than from a
placeholder, the prefix is **not** a pixel count (78 400 would be
`00 01 32 40`) and the `0x14` analogy collapses. Nobody has re-read the last two
bytes off hardware.

> Note the asymmetry with handshake **responses**, which use a 2-byte size and a
> pad byte (`[type][pad][size:u16 BE]`) — see [[Handshake]]. The TLV codec here
> is for requests and for the gaze/event stream payloads.

## Q42 fixed-point

`Q42` encodes a real value (in millimetres, or a normalized coordinate) as a
signed 64-bit integer scaled by `2^42`:

```
Q42_SCALE = 2^42 = 4_398_046_511_104
encode:  round(value * 2^42)   ->  i64      (write_f64_q42)
decode:  i64 / 2^42            ->  f64      (read_fixed22x42)
```

Reference values **[CONFIRMED]** (`tlv.rs::q42_matches_reference`):

| Value | Q42 integer (hex) |
|------:|-------------------|
| `200.0` | `0x0003_2000_0000_0000` (= 879 609 302 220 800) |
| `0.0` | `0x0000_0000_0000_0000` |
| `-200.0` | `-879 609 302 220 800` |
| `0.25` | `0x0000_0100_0000_0000` |
| `0.75` | `0x0000_0300_0000_0000` |

The body is written big-endian as a two's-complement `i64`.

## Struct prologs and tags

A `type = 0x05` field carries a 4-byte **tag** that names a struct that follows.

### The tag grammar

A tag is not an opaque constant. It decomposes as:

```
tag = (count << 16) | type_id

count   = tag >> 16      how many TLV elements follow the prolog
type_id = tag & 0xffff   what kind of container it is
```

`tlv::split_tag` is the one place this is spelled out. The row header was always
read this way (`read_xds_row` returns `tag >> 16` as the column count); the point
and column tags turn out to be the same rule with a *fixed* count, which is why
they could be hardcoded whole and never look like anything but constants.

Every tag we emit or parse obeys it, and in every case `count` equals the number
of TLV elements that actually follow. **[CONFIRMED]**:

| Tag | Const | `count` | `type_id` | Elements that follow |
|-----|-------|--------:|-----------|----------------------|
| `0x00021f40` | `TAG_POINT2D` | 2 | `0x1f40` | 2 × Q42 |
| `0x00031f41` | `TAG_POINT3D` | 3 | `0x1f41` | 3 × Q42 |
| `0x00020bb9` | `TAG_XDS_COLUMN` | 2 | `0x0bb9` | the `u32` column id + its value |
| `0x00010100` | (`commands.rs`, SET_DISPLAY_AREA) | 1 | `0x0100` | one `u32` (`0x3039`) |
| `0x00270bb8` | — (live gaze row) | 39 | `0x0bb8` | 39 columns |
| `0x00020bb8` | — (live `0x504` row) | 2 | `0x0bb8` | 2 columns |

`TAG_XDS_ROW_MASK = 0x0bb8` is the **`type_id` half only** — an XDS row's count
varies per frame, so only the low half can be a constant. It is not a
counter-example to the grammar; it is the grammar's other component named on its
own.

Evidence: `tlv.rs::container_tags_split_into_an_element_count_and_a_type_id`, the
`write_point`/`read_point3d` tests (`point_is_48_bytes`: a point3d is prolog(9) +
3×Q42(13) = 48 bytes), the live worked examples below, and `camera.rs`, which
independently builds its markers as `0x0002_0000 | 0x0bb9` and `0x0005_0000 |
0x0bb8`.

The `type_id`s cluster (`0x0bb8`/`0x0bb9` structural, `0x1f40`/`0x1f41`
geometric), which suggests a device-side type registry. We have no listing of it,
so **do not** infer an unseen tag's meaning from its neighbours —
`(count << 16)` is the only half we can compute rather than observe.

## XDS row/column framing

Stream payloads (gaze `0x500`, the `0x504` event) are an **XDS row** — a set of
labelled **columns**. Layout:

```
[00 00]                       2-byte payload prefix (skipped)
[type=5][size=4][row-tag]     xds_row prolog; column count = tag >> 16, type_id (tag & 0xffff) == 0x0bb8
repeated per column:
  [type=5][size=4][0x00020bb9]  xds_column prolog
  [type=2][size=4][col_id:u32]  the column id
  [ ...value... ]               a TLV field whose type depends on the column (u32 / s64 / fixed16x16 / point2d / point3d)
```

The column count in the row tag is a hint; decoders also stop at buffer end.
**[CONFIRMED]** — `tlv.rs::read_xds_row`/`read_xds_column`,
`gaze.rs::decode`/`column_inventory`.

## Worked example 1 — the opening of a real gaze frame `0x500`

Verbatim first bytes of a physical-device capture
(`gaze.rs::real_frame_payload`, a 1692-byte frame). **[CONFIRMED]**:

```
00 00                          payload prefix
05 00 00 00 04 00 27 0b b8     xds_row: tag=0x00270bb8 -> count = 0x27 = 39 columns
05 00 00 00 04 00 02 0b b9     xds_column prolog (tag 0x00020bb9)
02 00 00 00 04 00 00 00 01     column id = 0x01  (timestamp)
06 00 00 00 08 00 00 00 00 45 e1 3a 79   s64 value = 0x45e13a79 = 1 172 363 897 (timestamp, µs)
05 00 00 00 04 00 02 0b b9     next xds_column prolog
02 00 00 00 04 00 00 00 11     column id = 0x11
02 00 00 00 04 00 00 00 04     u32 value = 4
...                            (37 more columns)
```

So this frame declares 39 columns; the first is a `0x01` timestamp (s64), the
second is column `0x11` carrying a `u32` = 4. The full column set is in
[[Gaze-Stream]].

## Worked example 2 — the `0x504` event payload

The `0x504` state-change event is a small XDS row of **two** columns
(timestamp + one small value), a 69-byte payload that fires once on subscribe.
Its leading bytes decode as: **[CONFIRMED]** structure / **[HYPOTHESIS]** meaning:

```
00 00                          payload prefix
05 00 00 00 04 00 02 0b b8     xds_row: tag=0x00020bb8 -> count = 0x0002 = 2 columns
05 00 00 00 04 00 02 0b b9     xds_column prolog
02 00 00 00 04 00 00 00 01     column id = 0x01 (timestamp)
06 00 00 00 08 [8-byte s64]    timestamp value
05 00 00 00 04 00 02 0b b9     xds_column prolog
02 00 00 00 04 00 00 00 02     column id = 0x02
02 00 00 00 04 [4-byte u32]    a small u32 value
```

Total = 2 (prefix) + 9 (row) + [9 + 9 + 13] (timestamp col) + [9 + 9 + 9]
(second col) = **69 bytes**, matching the observed size. The second column's
meaning (a user-presence / tracking-state code) is **[HYPOTHESIS]** — see
[[Streams]]. Only the leading `00 00 05 00 00 00 04 00 02 0b b8` prefix+row-tag
was captured verbatim; the per-column bytes above are the standard XDS pattern
reconstructed to fit the 69-byte total.
