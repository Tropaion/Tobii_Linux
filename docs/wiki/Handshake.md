# Handshake

The connect sequence that takes the device from "just opened" to "subscribed and
streaming gaze". Source of truth: `crates/tobii-protocol/src/handshake.rs`
(the state machine), `crates/tobii-protocol/src/commands.rs` (`build_hello`,
`build_subscribe`), `crates/tobii-protocol/src/realm.rs` (auth builders),
`crates/tobii-protocol/src/md5.rs` (`hmac_md5`), and
`crates/tobii-usb/src/connection.rs::run_handshake` (the driver).

## Sequence overview

```
hello (0x3e8)
  → query_realm (0x640)          -> realm_type
  → open_realm (0x76c, realm_type)
        if realm_type == 0:  no auth, skip to subscribe
        else:                -> realm_id, field_210, challenge
  → realm_response (0x776, HMAC-MD5(REALM_KEY, challenge))    [auth path only]
  → subscribe (0x4c4, stream_id = 0x500)
  → Done  (device now streams gaze notifications)
```

Each step sends one request and waits for its response before advancing. The
driver feeds only **response** frames (`magic == 0x52`) into the handshake; gaze
notifications that arrive mid-handshake are queued, never mistaken for a
handshake reply. **[CONFIRMED]** — `handshake.rs::State`, `connection.rs::route`,
test `gaze_notification_during_handshake_does_not_break_it`.

## Step-by-step

### 1. hello — op `0x3e8`
A fixed captured 47-byte payload. **[CONFIRMED]** (`commands.rs` `HELLO_PAYLOAD`):

```
00 00 17 00 00 00 28 00 00 00 09 00 01 00 00 00
01 00 01 00 01 00 02 00 01 00 03 00 01 00 04 00
01 00 05 00 01 00 06 00 01 00 07 00 01 00 08
```

### 2. query_realm — op `0x640`
Payload `00 00`. The response's first `size==4` field is the **realm_type**
(0 = no authentication required). **[CODE-VERIFIED]** — `realm.rs::build_query_realm`,
`handshake.rs::resp_first_u32`.

### 3. open_realm — op `0x76c`
Payload `00 00` + `u32(realm_type)` (TLV type 2) + a single raw `0x00` choice
byte. If `realm_type == 0` the handshake jumps straight to subscribe. Otherwise
the response yields `realm_id` (field 0), `field_210` (field 1) and a **challenge**
(the first field longer than 4 bytes). A reply under 12 bytes on the auth path
fails the handshake. **[CODE-VERIFIED]** — `realm.rs::build_open_realm`,
`handshake.rs` `AwaitOpenRealm`.

### 4. realm_response — op `0x776` (auth path only)
Payload `00 00` + `u32(realm_id)` + `u32(field_210)` + the 16-byte HMAC-MD5
digest (raw, no TLV header). The digest is
`HMAC-MD5(REALM_KEY, challenge)` where:

```
REALM_KEY = "IS2LJC6GIRBBEK2K\x00"   (16 ASCII chars + trailing NUL, 17 bytes)
```

**[CODE-VERIFIED]** — `realm.rs::REALM_KEY`, `build_realm_response`;
`handshake.rs::auth_path_sends_correct_digest_and_reaches_done` verifies the
digest lands at frame offset 52..68. On the ET5 the realm has been observed to
be **no-auth** (`realm_type == 0`), so the HMAC path, while implemented and
unit-tested, has not been exercised live on this hardware. **[HYPOTHESIS]** that
any ET5 firmware returns a non-zero realm_type.

### 5. subscribe — op `0x4c4`
The handshake subscribes to **only `0x500`** (gaze). Payload is 20 bytes with the
stream id at bytes 9..10 big-endian:

```
00 00 02 00 00 00 04 00 [id_hi id_lo] 17 00 00 00 04 00 00 00 00
                          ^^^^^^^^^^^  stream id (BE) at payload[9..10]
```

For `0x500`: `id_hi = 0x05`, `id_lo = 0x00`. **[CONFIRMED]** —
`commands.rs::subscribe_payload`, test `subscribe_frame_carries_stream_id`.
After the subscribe is sent the state machine reports `Done`; the device begins
emitting `0x500` notifications. See [[Streams]] for subscribing to more streams.

## Sequence numbering across the handshake

The no-auth path sends 4 frames (hello, query, open, subscribe) → `seq` ends at
5; the auth path sends 5 → `seq` ends at 6. The connection continues
post-handshake requests from `Handshake::seq()`. **[CONFIRMED]** —
`handshake.rs::seq_advances_past_the_handshake_frames`,
`connection.rs::run_handshake` (`self.seq = hs.seq()`).

## Response field format (looser than requests)

Handshake **responses** are walked with a looser TLV: each field is
`[type:u8][pad:u8][size:u16 BE][body]` (a 2-byte size, unlike the 4-byte size in
the request codec). The first 2 payload bytes are a prefix and skipped.
**[CODE-VERIFIED]** — `handshake.rs::resp_fields`. This asymmetry (u16 sizes in
responses vs u32 in requests) is worth remembering when hand-decoding captures.
