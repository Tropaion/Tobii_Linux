# Streams

The ET5 pushes data as **notifications** (`magic 0x53`) whose op field is a
**stream id**. A host must **subscribe** to a stream before the device sends it.
Source of truth: `crates/tobii-protocol/src/commands.rs::subscribe_payload`,
`crates/tobii-usb/src/connection.rs` (`subscribe_stream`, `read_notifications`),
and `crates/tobii-cli/src/main.rs` (`probe_streams`, `probe_stream`).

## Subscription model

Subscribe with op **`0x4c4`**; the stream id goes at payload bytes 9..10
big-endian (see [[Handshake]]). The handshake subscribes to **only `0x500`**
(gaze); additional streams are opened with `Connection::subscribe_stream(id)`.
Every stream id in `0x501..=0x520` **ACKs** a subscribe, but most then send
nothing. **[CONFIRMED]** — live probe 2026-07-22/23.

## Known streams

| Stream | Payload size | Rate | What it is | Confidence |
|--------|-------------|------|------------|-----------|
| `0x500` | ~1692 B | ~33 Hz | **gaze** — 39 XDS columns (eye positions, gaze directions, pupils, validity, timestamps, flags). See [[Gaze-Stream]] | **[CONFIRMED]** |
| `0x501` | 78550 B | ~33 Hz | **NIR camera image** — 280×280, 8-bit grayscale, full-face wide-angle | **[CONFIRMED]** |
| `0x50e` | 78577 B | ~33 Hz | **NIR camera image** — the SAME image as `0x501`, not a second view | **[CONFIRMED]** |
| `0x504` | 69 B (2 cols) | once | state-change **event** — fires exactly once on subscribe, not again; XDS row with a timestamp + one small value. Likely user-presence / tracking-state change | one-shot **[CONFIRMED]**; meaning **[HYPOTHESIS]** |
| `0x502`, `0x503`, `0x505`–`0x50d`, `0x50f`–`0x520` | — | — | ACK the subscribe but produced no data in a 5 s window | ACK **[CONFIRMED]**; purpose unknown |

The two ~78 KB streams carry the **same** near-infrared image of the whole face,
wide-angle (the tracker sits below the monitor and looks up, so the face appears
low in frame with empty wall above it).

**They are NOT two viewpoints.** That was the reading here until it was measured,
twice: `tobii camera both` captured 199 timestamp-matched pairs with a face in
view and **199 were byte-identical**, then 166 pairs of a dark empty scene and
**166 were byte-identical**. The second run is the stronger one — two separate
sensors differ in their *noise*, not merely in what they are pointed at, so
identical bytes on an empty scene means one source, not two cameras that happen
to agree.

The device's own stream catalog says the same thing in its naming: `0x501` is
`image` and `0x50e` is `primary_camera_image` — one primary camera, exposed
twice. A third stream, `0x508 image_collection`, acks a subscribe and then
delivers **nothing** (5 s, 0 notifications), so if a second sensor exists it is
not reachable over any stream this device advertises.

Hence the head-pose model is monocular and takes depth from the gaze frame's
metric eye origins instead (see [[Head-Pose]]). Decoded live 2026-07-23: same XDS/TLV framing as
gaze, with columns `[timestamp(s64), bit_depth=8, width=280, height=280,
image-blob]`; the `0x05` blob is a 4-byte prefix (`00 01 ..`) followed by
`280×280 = 78400` 8-bit pixels. See `tobii-protocol/src/camera.rs`
(`decode_camera_frame`). These images are the **input to the host-side head-pose
model** (see [[Head-Pose]]). The `0x504` one-shot-on-subscribe behavior marks it
as an event notification, not a periodic sample. **[CONFIRMED]**.

## Head pose is NOT a known stream

None of `0x501..=0x520` delivered anything resembling head orientation, and head
pose is definitively **not** inside the `0x500` gaze frame either (see
[[Head-Pose]] for the six-axis evidence). If a dedicated head-pose stream exists
it is outside the probed range or uses a subscribe variant we have not observed.
The remaining way to settle it is a USB capture of Tobii's own Windows software
(see [[Reverse-Engineering-Methodology]]). **[HYPOTHESIS]**

## How to probe for more streams

Two CLI diagnostics (`tobii-cli`):

- **`tobii probe-streams [START] [END]`** — baselines the gaze-only notify ops,
  subscribes across a range of candidate ids (default `0x501..=0x520`), then
  reports which notify ops **newly appear**. A new op = a stream the device
  started because you asked. Uses `read_notifications` so co-occurring streams
  are not undercounted, and a short 300 ms request timeout so silent ids fail
  fast. **[CONFIRMED]** — `main.rs::probe_streams`.
- **`tobii probe-stream <ID_hex> [SECS]`** — deep-dive on ONE stream: subscribe,
  read for `SECS`, report rate, payload size range, whether the payload
  **changes frame-to-frame** (live data vs static config), and a hex preview.
  Move your head while it runs to test whether a small live stream is head pose.
  **[CONFIRMED]** — `main.rs::probe_stream`.

Driver primitives behind these: `subscribe_stream`, `next_notification` (first
notify in a chunk only — undercounts), `read_notifications` (every notify in a
chunk — use this to characterize concurrent streams), `next_gaze_payload` (raw
undecoded `0x500` payload). **[CONFIRMED]** — `connection.rs`.
