# tobii-protocol

Pure, dependency-free Rust codec for the Tobii Eye Tracker 5 USB wire protocol
(TTP framing + TLV/Q42), reverse-engineered with the GPL-3.0 `tobiifree` project
as the protocol reference.

This crate does **no I/O**. It builds outbound command frames, reassembles the
inbound USB byte stream into frames, and decodes gaze + display-area payloads.
The USB transport and handshake state machine live in `tobii-usb` (separate
crate).

## Modules
- `frame` — TTP framing + outbound USB envelope, op/magic constants.
- `tlv` — TLV encoders + `Reader` decoders, Q42 fixed-point.
- `commands` — hello, subscribe, get/set display area.
- `realm` — query/open/response/close realm + the realm key.
- `handshake` — `Handshake` connection state machine (hello → realm auth → subscribe).
- `md5` — MD5 + HMAC-MD5 for realm auth.
- `parser` — inbound `Parser` → `Frame`s (handles multi-transfer reassembly).
- `gaze` — `GazeSample::decode` for 0x500 notifications.
- `display` — `DisplayCorners::decode`.

License: GPL-3.0-only.
