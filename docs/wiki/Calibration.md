# Calibration (follow-the-dot)

> **Corrected 2026-08-15 — calibration ops.** `0x408` and `0x42f` were wrong and
> are kept in the table below, renamed, so nobody re-picks them. `0x408` is
> `CALIBRATE_POINT_ADD_EYE` and `0x42f` is `CALIBRATE_EYE_APPLY`: the device acks
> points sent to `0x408` with eye `0` and then discards them, and `0x42f` leaves
> the model untouched — so a whole calibration ran clean and changed nothing, for
> months. The eye argument is a **mask** (1=L, 2=R, 3=both); there is no
> zero-means-both. Found via `ChrisVeigl/tobiifree` commit `623c696`, corroborated
> here by a stored blob that was byte-identical across a full recalibration.
>
> The old `[CONFIRMED]` on `0x408` cited our own source file, which is circular and
> against this project's own rule that `[CONFIRMED]` needs a capture or a hardware
> round-trip. The replacements are **[HYPOTHESIS]** until a run on this hardware
> shows a compute over one second and a blob that changes.

Per-user gaze calibration improves accuracy by sampling where the user looks at
known on-screen stimulus dots and computing a personal model. Source of truth:
`crates/tobii-protocol/src/calibration.rs` (payload builders),
`crates/tobii-protocol/src/frame.rs` (op codes),
`crates/tobii-usb/src/connection.rs` (the driver methods), and memory note
`et5-calibration-protocol`.

## Op sequence

```
start (0x3f2)                          enter calibration mode
  → clear (0x424)                      discard any collected/active data (destructive)
  → for each stimulus point:
        (host draws + animates the dot, waits for fixation)
        add_point (0x406, x, y, eyemask) sample this point
        [on a bad point: discard_point (0x438, x, y), then re-add]
  → compute (0x42e)                    compute AND apply the new calibration
  → stop (0x3fc)                       leave calibration mode
  → retrieve (0x44c)                   read back the opaque blob to persist
```

**Ordering matters: `compute` comes BEFORE `stop`; `retrieve` comes AFTER
`stop`.** All session-control ops (`start`/`stop`/`clear`/`compute`/`retrieve`)
carry the bare `00 00` prefix only. **[CODE-VERIFIED]** — memory
`et5-calibration-protocol`; **`start` + `stop` are [CONFIRMED] live** (both ACK
standalone and the device keeps streaming afterward).

## Payloads

| Op | Builder | Payload |
|----|---------|---------|
| `0x3f2` start | `cal_session_payload` | `00 00` (no eye arg — see below) |
| `0x3fc` stop | `cal_session_payload` | `00 00` |
| `0x424` clear | `cal_session_payload` | `00 00` |
| `0x406` add_point | `cal_add_point_payload(x, y, eye)` | `00 00` + Q42(x) + Q42(y) + u32(eye **mask**: 1=L, 2=R, 3=both) |
| `0x42e` compute | `cal_compute_payload` | `00 00` |
| `0x44c` retrieve | `cal_retrieve_payload` | `00 00` → response is the blob |
| `0x456` apply | `cal_apply_payload(blob)` | `00 00` + raw blob bytes (no TLV header) |
| `0x438` discard_point | `cal_discard_point_payload(x, y)` | `00 00` + Q42(x) + Q42(y) — no eye arg |

`x`/`y` are normalized display coordinates in `[0,1]`. `eye` is
**`0 = both, 1 = left, 2 = right`** (NB: this is a *different* enum from the
`enabled_eye` property — see [[Select-Eyes]]). The `add_point` payload is **two
bare Q42 fields, not a point2d prolog**. **[CONFIRMED]** —
`calibration.rs::add_point_payload_is_exact` (a `(0.25, 0.75, 0)` payload is
exactly 37 bytes → a 69-byte frame).

`start` carries **no eye argument**: the native `tobii_calibration_start` drops
its `enabled_eye` argument on the wire and the app hardcodes both eyes. A
standard calibration therefore does not by itself enable single-eye detection.
**[CODE-VERIFIED]** — memory `et5-calibration-protocol`.

## Point sets

Normalized, top-left origin `[0,1]`, center-first order. The GTK follow-the-dot
flow uses a single **7-point** layout — `CalMode::Full`/`FULL_7` in
`calibrate_flow.rs` — verified byte-for-byte against the decompiled real Windows
software's `CalibrationStateManager` constructor default: `(.5,.5) (.1,.9)
(.5,.1) (.9,.9) (.1,.1) (.5,.9) (.9,.1)`. **[CODE-VERIFIED]**.

> Historical note: earlier revisions of this flow had a Quick/Full mode chooser
> (a 5-point set and a 9-point set). Both were removed — the real product has no
> such picker in its captured flow; it always runs the same 7-point sequence,
> for both first-time setup and manual recalibration.

The headless CLI (`tobii calibrate`) uses its own 5-point set
`(.5,.5) (.1,.1) (.9,.1) (.1,.9) (.9,.9)` and draws no dots — it validates the
protocol, not accuracy. The accurate flow is the GTK follow-the-dot UI.
**[CONFIRMED]** — `main.rs::CAL_POINTS`.

## Per-point timing — gaze-verified, not a blind timer

`add_point` **acks almost immediately** — it does **not** block while the device
gathers samples, contrary to what the decompiled managed layer (`Task.Delay(200)`
then a "blocking" collect) implied. **[CONFIRMED]** live 2026-07-21 (commit
`6837d24`): an early follow-the-dot run flew through all five points in ~1.5 s
and every sample was taken mid-saccade, producing a garbage calibration — proof
the device was not waiting.

The GTK flow's first response to this (a fixed host-side dwell — settle, then
hold for a set duration regardless of gaze, then sample) shipped initially but
turned out to have the same underlying flaw as the "blocking" assumption it
replaced: neither actually confirms the user is looking at the dot when the
sample is taken. **This was replaced** with genuine gaze-verified capture,
matching the decompiled original's real mechanism (`CalibrationProcessViewModel`):
a live `gaze_point_2d` reading (already streamed by the device, no personal
calibration required first) is checked against a proximity zone around the
current point (`focus::zone_radius`/`closest_focused_point`); `add_point` is
only sent once gaze has been continuously confirmed in that zone for
`SETTLE_TICKS` (~330 ms, tolerating brief gaps up to `GAZE_GAP_TOLERANCE_TICKS`
so a blink doesn't reset progress); if gaze leaves the zone again while waiting
for the device's ack, `discard_point` (`0x438`) is sent and the same point
retries from scratch rather than silently keeping a bad sample. `CAL_POINT_TIMEOUT`
= 30 s in `connection.rs` remains a defensive upper bound (rarely reached), not
evidence of device-side blocking. See `crates/tobii-gtk/src/calibrate_flow.rs`'s
`Phase::Collecting` and `crates/tobii-gtk/src/focus.rs`.

> Historical note: the memory note `et5-calibration-protocol` and the
> `add_calibration_point` doc originally said the call *blocks* — that was the
> pre-hardware hypothesis from the decompile, disproven by the live run above.
> A later revision then assumed a fixed host-side dwell was sufficient in its
> place — that too was superseded, by the gaze-verified mechanism described here.

## Session / realm

No special realm: all calibration ops ride the handshake's already-open no-auth
session. `start` itself puts the device into calibration mode; headless
add/compute/retrieve is proven. **[CONFIRMED]** — memory,
`calibration.rs::real_device_calibration_blob_is_sane`.

## Two hard-won design lessons (do not regress)

1. **Session token is mandatory in the UI.** After a Retry the device thread may
   still be blocked in a previous `add_calibration_point` (up to 30 s) while the
   UI already timed out; reading `collected`/`active` then is stale and the flow
   would compute a **zero-sample calibration over the user's good one**. Mint a
   per-start token, and read nothing from calibration state until it matches.
2. **`Connection::request` must be time-capped, not iteration-capped.** Gaze
   notifications route through the same read loop; a fixed iteration budget gets
   starved by ~33 Hz gaze traffic and every calibration point fails. Uses an
   `Instant` deadline (`DEFAULT_REQUEST_TIMEOUT` 10 s, `CAL_POINT_TIMEOUT` 30 s).

**[CONFIRMED]** — memory `et5-calibration-protocol`, `connection.rs`.

## Blob persistence

`retrieve` (`0x44c`) returns an **opaque** blob (verbatim response payload,
`CalibrationBlob`). Persist it and re-apply with `apply` (`0x456`,
`00 00` + raw blob). A real captured blob round-trips through `apply` unmodified
and is ≤ 4096 bytes. **[CONFIRMED]** —
`calibration.rs::real_device_calibration_blob_is_sane`,
`connection.rs::apply_calibration_sends_prefixed_blob_and_acks`. CLI:
`tobii calibrate` (run) / `tobii calibrate --apply` (re-apply saved).
