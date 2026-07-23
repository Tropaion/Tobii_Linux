# TTP Framing

The TTP header sits inside the USB envelope (see [[USB-Transport]]). Source of
truth: `crates/tobii-protocol/src/frame.rs` (`build_out_frame`,
`TTP_HDR_SIZE`, the magics) and `crates/tobii-protocol/src/parser.rs`
(`drain_one`, which parses the inbound header).

## Header layout (24 bytes, big-endian)

| Offset | Size | Field | Notes |
|-------:|-----:|-------|-------|
| 0 | 4 | **magic** | `0x51` REQ / `0x52` RSP / `0x53` NOTIFY |
| 4 | 4 | **seq** | request sequence number; echoed back in the response |
| 8 | 4 | **flag** | always `0` on outbound requests |
| 12 | 4 | **op** | operation code (e.g. `0x3e8` hello). For notifications, this is the **stream id** |
| 16 | 4 | reserved | always `0` |
| 20 | 4 | **plen** | payload length in bytes (big-endian) |
| 24 | plen | payload | op-specific body |

All four-byte header fields are **big-endian**. (This is the opposite of the
little-endian `len` in the surrounding USB envelope — do not confuse them.)
**[CONFIRMED]** — `frame.rs::out_frame_layout`, `parser.rs` field offsets, and
every builder test in `commands.rs`/`realm.rs`.

## The three magics and their direction

| Magic | Const | Direction | Meaning |
|-------|-------|-----------|---------|
| `0x51` | `TTP_MAGIC_REQ` | host → device | request |
| `0x52` | `TTP_MAGIC_RSP` | device → host | response to a request |
| `0x53` | `TTP_MAGIC_NOTIFY` | device → host | asynchronous notification (stream data) |

**[CONFIRMED]** — `frame.rs` constants; `connection.rs::route` dispatches on
these exact values.

## Sequence numbers and response matching

- The host assigns a `seq` to each request. The handshake starts at `seq = 1`
  and increments per frame, skipping `0` on wraparound
  (`handshake.rs::next_seq`, `connection.rs::next_seq`). **[CONFIRMED]**
- **The device echoes the request `seq` in its response.** A response frame is
  matched to a request only when **both `op` AND `seq` match** — this is
  hardware-verified and is what lets a repeated op (e.g. `cal_add_point`, sent
  once per calibration point) never be confused with a stale/duplicate ack.
  **[CONFIRMED]** — `connection.rs::request_until`, tests
  `request_skips_wrong_seq_response`, `add_calibration_point_gets_ack`; memory
  note `et5-calibration-protocol`.

## Notifications carry op == stream_id

For a `NOTIFY` frame (`magic == 0x53`), the header **op field is the stream
id**, not a command. The gaze stream is `0x500`, so gaze frames arrive with
`op == 0x500`. Notification `seq` is not meaningful (observed `0`). The driver
routes a notify by op: `op == 0x500` → decode as gaze; other notify ops are
surfaced by the diagnostic readers. **[CONFIRMED]** — `frame.rs`
`STREAM_GAZE`/`OP_GAZE_NOTIFY` both `0x500`; `connection.rs::route`,
`read_notifications`.

## Worked header example (hello)

`build_hello(1)` produces a 79-byte frame (8 envelope + 24 header + 47 payload):

```
00 00 00 00        envelope dir=OUT + pad
47 00 00 00        len_LE = 0x47 = 71 = 24 + 47   (excludes envelope)
00 00 00 51        magic = REQ
00 00 00 01        seq = 1
00 00 00 00        flag
00 00 03 e8        op = 0x3e8 (hello)
00 00 00 00        reserved
00 00 00 2f        plen = 47
00 ...             payload (47 bytes)
```

**[CONFIRMED]** — `commands.rs::hello_frame_is_79_bytes`.
