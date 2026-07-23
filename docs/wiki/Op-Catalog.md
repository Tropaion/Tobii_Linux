# Op Catalog

Master table of every known TTP op code. Op constants live in
`crates/tobii-protocol/src/frame.rs`; the name table is
`crates/tobii-recap/src/opnames.rs`. Payloads all begin with the universal
2-byte `00 00` prefix unless noted. "Dir" is who initiates.

## Connection / session

| Op (hex) | Dec | Name | Dir | Request payload | Response | Conf | Source |
|---------|----:|------|-----|-----------------|----------|------|--------|
| `0x3e8` | 1000 | hello | host→dev | fixed 47-byte blob | echoed/empty ack | **[CONFIRMED]** | `commands.rs` `HELLO_PAYLOAD` |
| `0x640` | 1600 | query_realm | host→dev | `00 00` | realm_type (first u32 field) | **[CODE-VERIFIED]** | `realm.rs` |
| `0x76c` | 1900 | open_realm | host→dev | `00 00` + u32(realm_type) + `00` | realm_id, field_210, challenge | **[CODE-VERIFIED]** | `realm.rs` |
| `0x776` | 1910 | realm_response | host→dev | `00 00` + u32(realm_id) + u32(field_210) + 16-byte HMAC-MD5 | ack | **[CODE-VERIFIED]** | `realm.rs` |
| `0x77b` | 1915 | close_realm | host→dev | `00 00` + u32(realm_id) | ack | **[CODE-VERIFIED]** | `realm.rs::build_close_realm` |
| `0x4c4` | 1220 | subscribe | host→dev | 20 B, stream_id BE at payload[9..10] | ack | **[CONFIRMED]** | `commands.rs::subscribe_payload` |

## Display area (platmod property, msg-class `0x51`)

| Op (hex) | Dec | Name | Dir | Request payload | Response | Conf | Source |
|---------|----:|------|-----|-----------------|----------|------|--------|
| `0x596` | 1430 | get_display_area | host→dev | empty (no `00 00`) | `00 00` + TL/TR/BL point3d + trailer | **[CONFIRMED]** | `commands.rs::build_get_display_area`, `display.rs` real-frame test |
| `0x5a0` | 1440 | set_display_area | host→dev | `00 00` + TL/TR/BL point3d + tag `0x10100` + u32 `0x3039` | ack | **[CONFIRMED]** | `commands.rs::set_display_area_corners_payload` |

## Calibration (Phase 2)

| Op (hex) | Dec | Name | Dir | Request payload | Response | Conf | Source |
|---------|----:|------|-----|-----------------|----------|------|--------|
| `0x3f2` | 1010 | cal_start | host→dev | `00 00` | ack | **[CONFIRMED]** live | `frame.rs`, `calibration.rs`; memory `et5-calibration-protocol` |
| `0x3fc` | 1020 | cal_stop | host→dev | `00 00` | ack | **[CONFIRMED]** live | same |
| `0x424` | 1060 | cal_clear | host→dev | `00 00` (destructive) | ack | **[CODE-VERIFIED]** | `frame.rs` `OP_CAL_CLEAR` |
| `0x408` | 1032 | cal_add_point | host→dev | `00 00` + Q42(x) + Q42(y) + u32(eye) | ack | **[CONFIRMED]** | `calibration.rs::cal_add_point_payload` |
| `0x42f` | 1071 | cal_compute (compute **and** apply) | host→dev | `00 00` | ack | **[CODE-VERIFIED]** | `frame.rs` `OP_CAL_COMPUTE` |
| `0x44c` | 1100 | cal_retrieve | host→dev | `00 00` | opaque blob | **[CONFIRMED]** | `connection.rs`, real blob testdata |
| `0x456` | 1110 | cal_apply | host→dev | `00 00` + raw blob | ack | **[CODE-VERIFIED]** | `calibration.rs::cal_apply_payload` |
| `0x438` | 1080 | cal_discard_point (discard_data_2d) | host→dev | `00 00` + Q42(x) + Q42(y) | ack | **[CODE-VERIFIED]** | memory `et5-calibration-protocol` (not in `frame.rs` consts) |
| `0x42e` | 1070 | cal_compute_and_apply_per_eye | host→dev | per-eye path; returns collected_eyes | — | **[HYPOTHESIS]** | memory `et5-calibration-protocol` (inferred, unverified) |
| `0x460` | 1120 | cal_stimulus_points_get | host→dev | quality: per-point L/R precision+bias | — | **[CODE-VERIFIED]** | memory `et5-calibration-protocol` |

## Enabled eye ("Select eyes to detect", platmod property)

| Op (hex) | Dec | Name | Dir | Request payload | Response | Conf | Source |
|---------|----:|------|-----|-----------------|----------|------|--------|
| `0xc62` | 3170 | get_enabled_eye | host→dev | empty | `00 00 02 00 00 00 04 00 00 00 0N` (N = 1 L / 2 R / 3 both) | **[CONFIRMED]** | `commands.rs`, `connection.rs` test |
| `0xc58` | 3160 | set_enabled_eye | host→dev | `00 00 02 00 00 00 04` + BE32(value) | ack | **[CONFIRMED]** | `commands.rs::set_enabled_eye_payload` |

> **SET (`0xc58`) is a LOWER op code than GET (`0xc62`)** — the inverse of the
> display-area/calibration convention. This is real; do not "correct" it.
> **[CODE-VERIFIED]** — memory `et5-enabled-eye-op`.

## Notifications (device → host, op == stream id)

| Op (hex) | Name | Payload | Conf | Source |
|---------|------|---------|------|--------|
| `0x500` | gaze_notify | ~1692 B, 39 XDS columns, ~33 Hz | **[CONFIRMED]** | `gaze.rs`; see [[Gaze-Stream]] |
| `0x501` / `0x50e` | eye-camera image (probable) | ~78 KB, ~33 Hz | **[HYPOTHESIS]** | live probe 2026-07-22; see [[Streams]] |
| `0x504` | state-change event (probable) | 69 B, 2 columns, one-shot on subscribe | one-shot **[CONFIRMED]**, meaning **[HYPOTHESIS]** | live probe |

## Unmapped / targets

Any op not above is a **mapping target**. `tobii-recap` prints these as
`?unknown` in its timeline (`opnames.rs::op_label`). Known open targets:

- The **head-pose** subscription/notify op, if one exists — never observed. See
  [[Head-Pose]]. **[HYPOTHESIS]**
- Streams `0x502`, `0x503`, `0x505`..`0x50d`, `0x50f`..`0x520`: all **ACK** a
  subscribe but streamed no data in a 5 s window. **[CONFIRMED]** ack, purpose
  unknown. See [[Streams]].
