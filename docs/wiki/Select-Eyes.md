# Select Eyes to Detect (enabled_eye)

"Select eyes to detect" (Both / Left only / Right only) is a **genuine,
persistent device-level property** — the same platmod-property family as the
display area — not merely a calibration input. Source of truth:
`crates/tobii-protocol/src/frame.rs` (op codes),
`crates/tobii-protocol/src/commands.rs` (`EnabledEye`, payload/parse),
`crates/tobii-usb/src/connection.rs` (`get/set_enabled_eye`), memory note
`et5-enabled-eye-op`.

## Ops

| Op | Dec | Name | Request payload | Response |
|----|----:|------|-----------------|----------|
| `0xc62` | 3170 | get_enabled_eye | **empty** | `00 00 02 00 00 00 04 00 00 00 0N` |
| `0xc58` | 3160 | set_enabled_eye | `00 00 02 00 00 00 04` + BE32(value) | ack |

> **SET (`0xc58`) is a LOWER op code than GET (`0xc62`)** — the inverse of the
> display-area and calibration convention. This is real; do not "correct" it.

Msg-class/tag `0x51` like display-area, 3 s timeout. Both are **version-gated**
on `[dev+0x1a0] >= 0x10007`; older firmware returns NOT_SUPPORTED (err 2), and
`get_enabled_eye` then returns `None`. **[CODE-VERIFIED]** op codes / gating
(native disasm); **[CONFIRMED]** live that both ACK and round-trip.

## Wire enum (1-based)

```
1 = LEFT      2 = RIGHT      3 = BOTH        (0 folds to BOTH)
```

This is the C-API `tobii_enabled_eye_t {LEFT=0, RIGHT=1, BOTH=2}` **plus 1** —
the response integer is NOT the C-API value; remap with `C-API = wire − 1`.
`EnabledEye::{Left,Right,Both}` encodes exactly `1/2/3`. **[CONFIRMED]** —
`commands.rs::to_wire`/`from_wire`, `enabled_eye_wire_round_trips`.

The hardware-captured **BOTH** GET response is:

```
00 00 02 00 00 00 04 00 00 00 03
                        ^^^^^^^^^^ BE32 value = 3 (BOTH)
```

`parse_enabled_eye` reads the trailing BE32. **[CONFIRMED]** —
`commands.rs::enabled_eye_payload_and_parse_match_device`,
`connection.rs::get_enabled_eye_parses_device_response`.

> **This used to warn of an enum landmine. There isn't one — that warning was
> the bug.** `cal_add_point` is `0x406`, not `0x408`, and its eye argument uses
> **the same** encoding as this property: `1=L, 2=R, 3=both`, as a mask. There is
> no `0=both`; passing 0 selects no eye, and the device acks the point and
> discards it. Sharing a constant between the two is correct, and
> `calibration::CAL_EYE_*` exists for that. See [[Calibration]] and the
> correction note at the top of [[Op-Catalog]].
>
> This also reopens a question: `et5-calibration-protocol` concluded that a
> standard calibration cannot honour the eye selection because per-eye needed
> `0x42e`. That premise is gone — `0x42e` is the ordinary compute — so whether a
> standard calibration applies the eye selection is **unknown again**.

## Semantics: persists, but takes effect only on (re)calibration

Hardware-verified 2026-07-20 **[CONFIRMED]** (memory `et5-enabled-eye-op`):

- A standalone **SET is acked and PERSISTS across reboot** — a fresh-session GET
  returns the value that was set.
- **BUT a standalone SET does NOT change live detection.** Both eyes keep
  reporting `validity == 0` in the gaze stream regardless of the setting.
- The property only takes effect when the tracking model is rebuilt, i.e. via
  `tobii_calibration_start(enabled_eye)`, which happens on **(re)calibration**.
  The vendor app never SETs it standalone; it persists the choice host-side per
  profile and re-applies by recalibrating.

Consequence: setting `enabled_eye` alone stores a preference but the tracker
keeps detecting both eyes until a calibration is run. Whether a *standard*
calibration (which drops the eye arg on the wire, [[Calibration]]) is enough to
apply the selection is **unresolved** — a Windows-VM capture of toggling
Left/Right/Both settles it. **[HYPOTHESIS]**.

## API / CLI

`Connection::get_enabled_eye() -> Option<EnabledEye>`,
`set_enabled_eye(eye) -> bool` (acked?). CLI:
`tobii enabled-eye [both|left|right]` — sets (if an arg is given) then reads
back. **[CONFIRMED]** — `connection.rs`, `main.rs::enabled_eye_cmd`.
