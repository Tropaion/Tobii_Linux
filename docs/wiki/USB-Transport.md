# USB Transport

How TTP frames get onto and off of the wire. Source of truth:
`crates/tobii-usb/src/transport.rs`, `crates/tobii-protocol/src/frame.rs`,
`crates/tobii-protocol/src/parser.rs`.

## Device identity and endpoints

| Property | Value | Confidence | Source |
|----------|-------|-----------|--------|
| USB Vendor ID | `0x2104` | **[CONFIRMED]** | `transport.rs` `VID` |
| USB Product ID | `0x0313` | **[CONFIRMED]** | `transport.rs` `PID` |
| Interface | `0` | **[CONFIRMED]** | `transport.rs` `IFACE` |
| Bulk OUT endpoint (host→device) | `0x05` | **[CONFIRMED]** | `transport.rs` `EP_OUT` |
| Bulk IN endpoint (device→host) | `0x83` | **[CONFIRMED]** | `transport.rs` `EP_IN` |

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
