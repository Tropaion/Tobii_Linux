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

| type | Rust reader | Body | Meaning |
|-----:|-------------|------|---------|
| `0x02` | `read_u32` | 4 B BE | unsigned 32-bit integer |
| `0x03` | `read_fixed16x16` | 4 B BE | signed 16.16 fixed-point → `i32 / 65536.0` |
| `0x04` | `read_fixed22x42` | 8 B BE | **Q42** fixed-point → `i64 / 2^42` |
| `0x05` | `read_prolog_tag` | 4 B BE | struct **prolog**: a 4-byte tag introducing a struct |
| `0x06` | `read_s64` | 8 B BE | signed 64-bit integer (used for timestamps) |

**[CONFIRMED]** — `tlv.rs` readers/writers, round-trip and reader tests.

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
Tags used on the wire (`tlv.rs`):

| Tag | Const | Introduces |
|-----|-------|-----------|
| `0x00021f40` | `TAG_POINT2D` | a 2D point: 2 × Q42 |
| `0x00031f41` | `TAG_POINT3D` | a 3D point: 3 × Q42 |
| `0x00020bb9` | `TAG_XDS_COLUMN` | an XDS column header (followed by a `u32` column id) |
| `…0bb8` (low 16 bits) | `TAG_XDS_ROW_MASK` | an XDS row header; the column count is packed in the high bits |

**[CONFIRMED]** — `tlv.rs` constants and `write_point`/`read_point3d` tests
(`point_is_48_bytes`: a point3d is prolog(9) + 3×Q42(13) = 48 bytes).

## XDS row/column framing

Stream payloads (gaze `0x500`, the `0x504` event) are an **XDS row** — a set of
labelled **columns**. Layout:

```
[00 00]                       2-byte payload prefix (skipped)
[type=5][size=4][row-tag]     xds_row prolog; column count = (tag >> 16) & 0xfff, low 16 bits == 0x0bb8
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
