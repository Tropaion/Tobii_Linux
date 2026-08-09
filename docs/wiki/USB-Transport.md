# USB Transport

How TTP frames get onto and off of the wire. Source of truth:
`crates/tobii-usb/src/transport.rs`, `crates/tobii-protocol/src/frame.rs`,
`crates/tobii-protocol/src/parser.rs`. The descriptor facts below were read off
the physical device with `lsusb -d 2104:0313 -v` / `lsusb -t` on 2026-08-09.

## Device identity

| Property | Value | Confidence | Source |
|----------|-------|-----------|--------|
| USB Vendor ID | `0x2104` | **[CONFIRMED]** | `transport.rs` `VID` |
| USB Product ID | `0x0313` | **[CONFIRMED]** | `transport.rs` `PID` |
| Manufacturer / product strings | `Tobii AB` / `EyeChip` | **[CONFIRMED]** | `lsusb -v`, 2026-08-09 |
| USB version, speed | 2.00, 480 Mbit/s high-speed | **[CONFIRMED]** | `lsusb -v`, `lsusb -t`, 2026-08-09 |
| Configurations | 1, bus-powered, `MaxPower` 500 mA | **[CONFIRMED]** | `lsusb -v`, 2026-08-09 |
| Interfaces | **3** (`bNumInterfaces 3`) | **[CONFIRMED]** | `lsusb -v`, 2026-08-09 |

## Full interface and endpoint layout

The device is **not** the single-interface vendor device this project treats it
as. Its one configuration exposes three interfaces, each with exactly one
alternate setting (`bAlternateSetting 0` only — there are no others):

| Interface | `bInterfaceClass` | Subclass | `iInterface` | Endpoints | Used by this project | Confidence |
|-----------|-------------------|----------|--------------|-----------|----------------------|-----------|
| `0` | 255 Vendor Specific | 0 | — | `0x83` IN, `0x04` OUT, `0x05` OUT | **yes — all TTP traffic** (`transport.rs` `IFACE`) | **[CONFIRMED]** |
| `1` | 14 Video | 1 Video Control | `Tobii Video Control` | none | no | **[CONFIRMED]** |
| `2` | 14 Video | 2 Video Streaming | `Video Stream In` | `0x82` IN | no | **[CONFIRMED]** |

Every endpoint on the device is **bulk** (`bmAttributes 2`) with
`wMaxPacketSize 512` — including the video one, which is unusual for UVC (see
below). Endpoint detail:

| Endpoint | Dir | Type | `wMaxPacketSize` | Interface | Role | Confidence |
|----------|-----|------|------------------|-----------|------|-----------|
| `0x83` | IN | bulk | 512 | 0 | TTP device→host; `transport.rs` `EP_IN` | **[CONFIRMED]** |
| `0x05` | OUT | bulk | 512 | 0 | TTP host→device; `transport.rs` `EP_OUT` | **[CONFIRMED]** |
| `0x04` | OUT | bulk | 512 | 0 | **unknown — never used by this project** | **[CONFIRMED]** it exists; purpose **[UNCONFIRMED]** |
| `0x82` | IN | bulk | 512 | 2 | UVC video streaming; untouched | **[CONFIRMED]** |

**Endpoint `0x04` OUT has never been used by this driver.** `transport.rs`
defines only `EP_OUT = 0x05` and `EP_IN = 0x83`, and no code anywhere in
`crates/` references `0x04` as an endpoint. The full ET5 handshake, all
configuration ops and all streams work without ever writing to it, so it is not
required for anything we do. What it is *for* is unknown — a second command
channel, a firmware/DFU path and a bulk-out for the video interface's control
plane are all consistent with the descriptor, and we have tested none of them.
**[UNCONFIRMED]** — do not guess in code; probe it before using it.

Open procedure (`UsbTransport::open`): open by VID/PID, best-effort detach of any
kernel driver on interface 0, `claim_interface(0)`, then a **vendor control
transfer** to open the session (below). Bulk writes and reads use a 1000 ms
libusb timeout; reads of length 0 or a libusb timeout are treated as "no data
this call". The read buffer used by the driver is 16384 bytes
(`connection.rs` `READ_BUF`). **[CONFIRMED]**

## Vendor session control transfers

A session is bracketed by two vendor control transfers on the interface
recipient (`bmRequestType = Vendor | Host-to-Device | Interface`), zero-length
data:

| Name | `bRequest` | When | Confidence | Source |
|------|-----------|------|-----------|--------|
| `SESSION_OPEN` | `0x41` | right after `claim_interface`, before any bulk I/O | **[CONFIRMED]** | `transport.rs` `open()` |
| `SESSION_CLOSE` | `0x42` | on drop / disconnect | **[CODE-VERIFIED]** | `transport.rs` `SESSION_CLOSE`; memory note `et5-display-area-resets-on-reboot` (byte-identical to the tobiifree reference) |

Note: the current `UsbTransport` sends `SESSION_OPEN` on `open()`; the
`SESSION_CLOSE` (`0x42`) constant exists and matches the reference driver's
close-on-Drop. **[CODE-VERIFIED]**

## Outbound USB envelope (host → device)

Every frame the host sends is `[envelope:8][ttp header:24][payload]`:

```
byte 0        : 0x00     direction = OUT
bytes 1..3    : 0x00 * 3 padding
bytes 4..7    : len_LE (u32, little-endian) = length of the TTP part
                (24-byte header + payload), EXCLUDING these 8 envelope bytes
bytes 8..     : TTP header (24 bytes, big-endian) + payload
```

`build_out_frame` (`frame.rs`) builds exactly this. **[CONFIRMED]** — pinned by
`frame.rs::out_frame_layout` (e.g. a 3-byte payload gives `len_LE = 27` = 24+3).

## Inbound USB envelope (device → host)

Device-to-host bytes are also length-prefixed, but the length field is
**asymmetric**: it INCLUDES the 8-byte envelope.

```
byte 0        : 0x01     direction = IN  (rejected otherwise: BadDirection)
bytes 1..3    : 0x00 * 3
bytes 4..7    : len_LE (u32) = TOTAL bytes of this frame INCLUDING the 8-byte envelope
bytes 8..31   : TTP header (24 bytes, big-endian)
bytes 32..    : payload
```

**[CONFIRMED]** — `parser.rs` `drain_one` reads the header at
`ENVELOPE_SIZE + 20` and requires `len >= 8 + 24`; test `single_complete_frame`
and the real 200-byte fragmented capture test pin it.

## Reassembly across transfers

Large responses (calibration blobs, ~1.7 KB gaze frames, ~78 KB image frames)
are split across multiple USB bulk transfers. The `Parser` (`parser.rs`)
accumulates raw bytes and yields complete frames:

- The **first** transfer of a frame carries the full `[IN envelope][TTP
  header][partial payload]`.
- **Continuation** transfers each carry their *own* 8-byte envelope
  (`01 00 00 00` + a length) wrapping raw payload bytes. When the accumulator
  already holds a header and is still short of `plen`, and the next chunk begins
  with `01 00 00 00`, the parser strips that 8-byte continuation envelope before
  appending, so the accumulator holds one clean `[env][hdr][payload]`.
- The accumulator has a 2 MiB cap (`ACC_CAP`); overflow or a bad
  direction/length resets it and returns an error.

**[CONFIRMED]** — `parser.rs::fragmented_multi_envelope_response` reconstructs a
200-byte payload delivered in three chunks with intermediate continuation
envelopes.

## The device reboots on session close

When the last client detaches, the ET5 **re-enumerates on USB (reboots)**. This
is normal ET5 behavior, not a fault. It has a critical side effect: on every
reboot the device **wipes its display-area configuration** to a ~4 mm stub, and
until a valid display area is re-applied *in-session* it reports no eyes
(`validity = 4`, all eye-origin columns zero). Every tool that wants gaze data
must call `set_display_area` right after connecting. See [[Display-Area]].
**[CONFIRMED]** — memory note `et5-display-area-resets-on-reboot`, fix commit
`b7528b5`; `connection.rs::set_display_area` doc.

## The UVC video interfaces — a second, unexplored path

Interfaces 1 and 2 are a **standard USB Video Class (UVC 1.10) camera** that
this project has never touched. Nothing in `crates/` claims, configures or reads
them; every fact below is read straight off the device's own descriptors.

Interface 1 is the Video Control interface: a `Camera Sensor` input terminal
(`0x0201`), a `USB Streaming` output terminal (`0x0101`), a processing unit, and
one vendor extension unit `{158e1211-61ce-4d13-8170-47150a2a2e30}` with 5
control bits set. Interface 2 is the Video Streaming interface and declares a
single format with a single frame descriptor: **[CONFIRMED]**

| Descriptor field | Value | Meaning |
|------------------|-------|---------|
| format subtype | 16 `FORMAT_FRAME_BASED` | frame-based, not an uncompressed/MJPEG format |
| `guidFormat` | `{e39e1ba2-1599-3248-8728-e1b25923a611}` | **vendor-specific** — it lacks the standard UVC GUID suffix, so it is not YUY2/NV12/MJPG |
| `bBitsPerPixel` | 8 | 8-bit, consistent with the NIR grayscale sensors |
| `wWidth` × `wHeight` | **642 × 480** | the only frame size offered |
| `dwDefaultFrameInterval` | 416667 (×100 ns) | 41.67 ms → **24 fps** |
| `bNumFormats`, `bNumFrameDescriptors` | 1, 1 | no alternatives to negotiate |

Note the streaming endpoint `0x82` is **bulk**, and interface 2 has no
zero-bandwidth alternate setting. UVC cameras normally stream over isochronous
endpoints with alt settings to negotiate bandwidth; a bulk-only UVC device is
legal but uncommon, and it means there is no bandwidth reservation to fail.

### Nothing is bound to the video interfaces — [CONFIRMED]

`lsusb -t` reports `Driver=[none]` for interfaces 1 and 2 whatever the device is
doing, and sysfs agrees: neither `/sys/bus/usb/devices/<bus>-<port>:1.1` nor
`:1.2` has a `driver` symlink, and there is **no `/dev/video*` node on the
system at all** — even though the `uvcvideo` module *is* loaded (with a zero
refcount). So the kernel has the driver available and has simply not bound it to
this device. Why it does not bind is **[UNCONFIRMED]**.

Interface 0 must not be quoted as evidence here, because its binding is not a
property of the device: it reads `Driver=[none]` only while nothing holds it and
shows `Driver=usbfs` for as long as a client has claimed it, libusb being a
usbfs client. Both states were observed on this device on 2026-08-09, minutes
apart. **[CONFIRMED]**

### Can they actually stream? — UNVERIFIED

**We have never made these interfaces produce a single frame.** A descriptor is
a claim by the device, not proof it will honour it, and the vendor format GUID
means even a successful bind may yield a stream no standard consumer can decode.
Treat "the ET5 exposes a UVC camera" as *declared but unproven*.

To find out, force `uvcvideo` to try the device and then ask the resulting node
what it can really do:

```sh
# uvcvideo is loaded but bound to nothing — make it attempt this VID:PID
echo 2104 0313 | sudo tee /sys/bus/usb/drivers/uvcvideo/new_id

v4l2-ctl --list-devices                          # did a node appear at all?
v4l2-ctl -d /dev/videoN --list-formats-ext       # what does it really offer?
v4l2-ctl -d /dev/videoN --stream-mmap --stream-count=1 --stream-to=frame.raw
```

Untested interaction: whether this can be done while a `tobii` session holds
interface 0, and whether binding/unbinding trips the reboot described above.
Establish the result on a device with no session open first.

### Why this matters: head pose

If interface 2 streams, the eye cameras are reachable through plain **v4l2 with
none of the TTP machinery** — no session, no handshake, no display area, no
subscription. That is directly relevant to [[Head-Pose]], whose neural path
needs exactly one thing from the device: camera images to run a model over. A
v4l2 source would be a far simpler and more robust way to feed it than decoding
`0x501`/`0x50e` off the vendor endpoint, and it would sidestep the display-area
reset entirely. It also *declares* a different frame geometry than the TTP camera
stream delivers (see below), so it may be a different view of the sensors rather
than the same images by another route — but that is **[UNCONFIRMED]** until
someone captures a frame.

### On the "560×560" figure — unsupported hearsay

Two resolutions have real evidence behind them, and they disagree — note that
one is *declared* by a descriptor and the other is *measured* off actual frames:

| Source | Resolution | Kind of evidence | Confidence |
|--------|-----------|------------------|-----------|
| UVC frame descriptor, read off the device | 642 × 480, 8-bit | **declared** by the device; never streamed | **[CONFIRMED]** the descriptor says so — `lsusb -v`, 2026-08-09 |
| TTP camera stream `0x501`/`0x50e` | 280 × 280, 8-bit, ~78 KB/frame | **measured** from decoded frames | **[CONFIRMED]** — `camera.rs` module docs, 2026-07-23 |

A **560 × 560** figure circulates for the ET5 cameras. It originates from a
third-party tool that is **not public**, so it cannot be inspected or
reproduced, and it matches *neither* number we have measured ourselves. Record
it as **unsupported hearsay** and do not size any buffer or model input from it.
(560 is arithmetically 2 × 280; that coincidence is not evidence of anything and
should not be treated as a stereo-pair explanation without a capture proving it.)
