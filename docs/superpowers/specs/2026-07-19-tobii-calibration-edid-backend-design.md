# Tobii Config Backend — Calibration Protocol + EDID Auto-Detect (Design Spec)

**Date:** 2026-07-19
**Status:** Implemented + live-validated 2026-07-19 (branch `feat/tobii-calibration`)
**Author:** Fabian Plaimauer (with Claude Code)
**License:** GPL-3.0 (derives protocol knowledge from the GPL-3.0 `tobiifree` project)

> **Shipped-API note (supersedes the design sketch below).** Two names in this
> design differ from what shipped: (1) the CLI is **`tobii calibrate [--apply]`**
> (the default run is the headless op sequence; `--apply` re-applies the saved
> blob) — not `--smoke`; (2) the protocol layer ships **payload** builders
> `cal_add_point_payload` / `cal_compute_payload` / `cal_retrieve_payload` /
> `cal_apply_payload` (the op + seq are applied by `Connection::request`), not the
> `build_cal_*(seq, …)` full-frame builders sketched in §5. The wire facts (§4) and
> the resolved live findings (§10) are accurate as shipped.

## 1. Goal & context

Deliver the **backend foundations** for full *functional* parity with the original
Tobii software's configuration + calibration experience on Linux: (A) auto-detect
the monitor, and (B1) the per-user **gaze-calibration protocol** — the wire ops,
persistence, and a headless validation path — with **no GUI**.

This is the first of several sub-projects toward "do exactly what the original
does" (functional equivalent, native-Linux look — not a pixel copy of Tobii's
proprietary UI):

| Sub-project | Scope | Status |
|---|---|---|
| **A — EDID auto-detect** | monitor mm size + model from sysfs → seed setup | **this spec** |
| **B1 — calibration protocol** | wire ops + blob persistence + headless smoke | **this spec** |
| B2 — GUI shell + display-setup + eye-position view | graphical app | later spec |
| B3 — gaze calibration UI | fullscreen follow-the-dot stimulus | later spec |

**Sequencing rationale:** backend-first, so the protocol and persistence are proven
on real hardware before the GUI's complexity (a windowing/rendering stack — the
project's first) is introduced. It reuses the existing runtime (handshake, TTP/TLV/
Q42 codec, `Connection::request`) and adds **zero external dependencies**.

**Primary end goal reminder:** the project's original target (6DOF head tracking →
opentrack → Star Citizen, "Plan 5") is independent of this parity work and remains
a separate track; this spec does not address it.

## 2. Scope

**In scope**
- **A:** read `/sys/class/drm/*/edid`, parse physical size (mm) + model + preferred
  resolution, and use it to pre-fill `tobii setup` (and, later, the GUI).
- **B1:** four new calibration TTP ops (`add_point`, `compute`, `retrieve`, `apply`)
  built on the existing realm ops for enter/leave; opaque calibration-blob
  persistence + auto-apply; a `tobii calibrate --smoke` headless validation path.

**Out of scope** (explicitly deferred)
- Any GUI, fullscreen stimulus rendering, or **gaze-accuracy** validation → B2/B3.
- Head-pose / opentrack (Plan 5).
- The `cal_stimulus 0x460` op — dead in the reference; the host picks stimulus points.
- Multi-profile calibration management (single active profile for now).

## 3. Architecture (component split)

All new code follows the established pattern — pure builders/parsers in
`tobii-protocol`, thin request/response in `tobii-usb`, orchestration in the caller.

- **`tobii-protocol`** — new `calibration` module: op-code constants + pure payload
  builders (`build_cal_add_point`, `build_cal_compute`, `build_cal_retrieve`,
  `build_cal_apply`) + a `CalibrationBlob(Vec<u8>)` newtype. Reuses `bytes::Writer`,
  `tlv::{write_f64_q42, write_u32}`, `frame::build_out_frame`. The realm ops
  (`0x640/0x76c/0x776/0x77b`) and HMAC-MD5 already exist (`realm.rs`, `md5.rs`,
  `handshake.rs`) and are reused for enter/leave. Golden-vector tested.
- **`tobii-usb`** — thin `Connection` calibration methods driving the ops via the
  existing `request()`; mock-tested with `MockTransport`.
- **`tobii-config`** — (A) `edid` module (pure, std-only); (B) calibration-blob
  persistence (`calibration.bin` beside `config.toml`) + an "auto-apply on connect"
  flag in the TOML.
- **`tobii-cli`** — `tobii calibrate --smoke` (headless op-sequence validation) and
  EDID wired into `tobii setup`.

## 4. Calibration protocol reference (verified against `tobiifree`)

Sourced from `tobiifree`'s Zig core (`driver/src/tobiifree_core.zig`, the wire
authority) and cross-verified against its own unit tests by an adversarial pass
(all op codes + payload sizes confirmed). Every request is
`build_out_frame(seq, op, payload)`; **every calibration/realm request payload
begins with the 2-byte prefix `00 00`**. TLV field encodings (already in our
`tlv.rs`): `u32` = `[0x02][00 00 00 04][u32 BE]` (9 B); `Q42 f64` =
`[0x04][00 00 00 08][i64 BE = round(v·2⁴²)]` (13 B); raw bytes = no header.

### 4.1 Op table

| Op | TTP code | Request payload | Response |
|---|---|---|---|
| `query_realm` (enter 1) | `0x640` *(reuse)* | `00 00` | first u32 field = `realm_type` |
| `open_realm` (enter 2) | `0x76c` *(reuse)* | `00 00` + u32(`realm_type`) + raw `0x00` | `realm_type==0`→`realm_id`, done; else `realm_id`,`field_210`,challenge |
| `realm_response` (enter 3, if `realm_type≠0`) | `0x776` *(reuse)* | `00 00` + u32(`realm_id`) + u32(`field_210`) + **16-byte raw** HMAC-MD5 digest | any response → unlocked |
| **`cal_add_point`** | **`0x408`** | `00 00` + Q42(x) + Q42(y) + u32(eye) | ack by seq; body ignored |
| **`cal_compute`** (compute *and* apply) | **`0x42f`** | `00 00` | **first response = done** (no status field, no poll) |
| **`cal_retrieve`** | **`0x44c`** | `00 00` | **response payload = opaque blob, verbatim** (cap 4096 B) |
| **`cal_apply`** (restore) | **`0x456`** | `00 00` + **raw blob bytes** (no TLV header) | first response = done |
| `close_realm` (leave) | `0x77b` *(reuse)* | `00 00` + u32(`realm_id`) | any response → closed |

- `x,y` are **normalized display coords `[0,1]`**; `eye` is `0=both / 1=L / 2=R`
  (TLV u32). **`add_point` uses two bare Q42 fields — no point2d prolog.**
- HMAC-MD5 key `REALM_KEY = "IS2LJC6GIRBBEK2K\x00"` (**17 bytes, incl. the trailing
  NUL**) — already in `realm.rs`.
- The `00 00`-prefixed `compute`/`retrieve` payloads (plen=2) differ from
  `build_get_display_area` (plen=0) — keep the prefix.

### 4.2 Flow

**Calibrate (measure → apply → save):**
1. **Enter** — realm unlock (`query→open→[response]`), retain `realm_id`.
2. **Per stimulus point** (host-chosen `(x,y)`; center + corners typical): render a
   dot, then `cal_add_point(x, y, eye=0)`; await the per-point ack (matched by op+seq).
3. **Compute + apply** — `cal_compute`; treat the first response as done (generous
   read window).
4. **Retrieve** — `cal_retrieve`; store the whole response payload verbatim as the blob.
5. **Leave** — `close_realm(realm_id)`.

**Restore (apply a saved blob):** enter → `cal_apply(blob)` (first response = done) → leave.

Response matching throughout: `Frame.op == <op>` **and** `Frame.seq == request seq`
(same contract as `Connection::request` today).

### 4.3 Calibration blob

Opaque byte buffer, unparsed. Retrieve = the response payload byte-for-byte
(≤4096 B); apply = `00 00` + those bytes. Persisted verbatim to
`$XDG_CONFIG_HOME/tobii-linux/calibration.bin`. Large blobs spanning multiple USB
transfers reassemble via the existing `Parser`.

### 4.4 Eye-position data (informs B2, not built here)

The "position your eyes" view will use the `0x500` gaze stream's **trackbox-
normalized eye position** (cols `0x03`/`0x09`, `[0,1]`, mislabeled "gaze direction"
in the wire but actually normalized box position; `z` = head distance), gated on
`validity_L`/`validity_R` (`0`=valid, `4`=not detected). **Gap noted for B2:** our
`GazeSample` does not decode cols `0x03`/`0x09` yet; add them then. Before any
calibration exists, use the raw pre-calibration origins (cols `0x17`/`0x18`).

## 5. Rust API surface (to add)

```rust
// tobii-protocol/src/frame.rs — new constants
pub const OP_CAL_ADD_POINT: u32 = 0x408;
pub const OP_CAL_COMPUTE:   u32 = 0x42f; // compute AND apply
pub const OP_CAL_RETRIEVE:  u32 = 0x44c;
pub const OP_CAL_APPLY:     u32 = 0x456;
// reuse: OP_QUERY_REALM 0x640, OP_OPEN_REALM 0x76c, OP_REALM_RESPONSE 0x776, OP_CLOSE_REALM 0x77b

// tobii-protocol/src/calibration.rs — pure builders (00 00 prefix, existing TLV helpers)
pub fn build_cal_add_point(seq: u32, x: f64, y: f64, eye: u32) -> Vec<u8>; // Q42(x),Q42(y),u32(eye)
pub fn build_cal_compute(seq: u32)  -> Vec<u8>;   // payload = [0,0]
pub fn build_cal_retrieve(seq: u32) -> Vec<u8>;   // payload = [0,0]
pub fn build_cal_apply(seq: u32, blob: &[u8]) -> Vec<u8>; // [0,0] + raw blob
pub struct CalibrationBlob(pub Vec<u8>);

// tobii-usb — thin Connection methods (reuse request(); enter/leave via realm ops)
impl<T: Transport> Connection<T> {
    pub fn add_calibration_point(&mut self, x: f64, y: f64, eye: u32) -> Result<(), UsbError>;
    pub fn compute_and_apply_calibration(&mut self) -> Result<(), UsbError>;
    pub fn retrieve_calibration(&mut self) -> Result<CalibrationBlob, UsbError>;
    pub fn apply_calibration(&mut self, blob: &[u8]) -> Result<(), UsbError>;
    // enter/leave: reuse realm unlock/close (see §10 open question #1 — may already be open)
}

// tobii-config — EDID + blob persistence
pub struct MonitorInfo { pub model: String, pub width_mm: f64, pub height_mm: f64, /* + native res */ }
pub fn detect_monitors() -> Vec<MonitorInfo>;         // parse /sys/class/drm/*/edid
pub fn save_calibration(blob: &[u8]) -> std::io::Result<()>;
pub fn load_calibration() -> std::io::Result<Option<Vec<u8>>>;
```

`write_point2d` is **not** needed (add_point uses bare Q42).

## 6. Data flow

- **Calibration** (driven by the `--smoke` path now, the GUI later): `Connection`
  enter → `add_calibration_point` per host-chosen point → `compute_and_apply` →
  `retrieve_calibration` → `tobii_config::save_calibration`. On a later connect,
  `load_calibration` → `apply_calibration` re-applies it (persistent, like the
  original).
- **EDID:** `tobii setup` calls `detect_monitors()` → pre-fills width/height/model;
  the user confirms or overrides.

## 7. Error handling

- Calibration ops return `Result<_, UsbError>`; a missing/mismatched response
  (`request` → `Ok(None)`) is surfaced as an error for `retrieve` (we need the blob)
  and as a warning for fire-and-forget-ish steps. Compute failure (device returns an
  error frame) is surfaced with context.
- EDID: missing/garbage `edid` files degrade gracefully to manual entry — never panic.
- Never panic on device bytes (existing crate invariant).

## 8. Testing strategy

- **Unit (no hardware):** golden byte-vector tests for each calibration builder
  (assert exact op code + payload bytes, cross-checked against §4.1 sizes:
  add_point frame = 69 B, compute/retrieve = 34 B); EDID parse against a captured
  `edid` blob (a real monitor's, committed as a fixture); blob save/load round-trip.
- **Mock (`tobii-usb`):** `MockTransport` scripts for the `Connection` methods —
  including a scripted `retrieve` returning a multi-frame blob, and gaze interleaving
  (reuse the display-area/request test patterns).
- **Headless hardware smoke** (`tobii calibrate --smoke`): enter → add 2–3 fixed
  points → compute → retrieve → apply → leave; confirm the device accepts each op
  and returns a sane (non-empty, ≤4096 B) blob. **Gaze accuracy is NOT asserted here**
  (no one is following dots) — that waits for B3. Capture the real blob + one op
  response as golden regression vectors.

## 9. Persistence

`$XDG_CONFIG_HOME/tobii-linux/calibration.bin` (raw blob) alongside `config.toml`.
A `[calibration] auto_apply = true` key (hand-rolled TOML, matching `DisplaySetup`)
controls whether a saved blob is re-applied when a connection is opened.

## 10. Risks & open questions

**All four backend questions RESOLVED 2026-07-19 (live, physical ET5 `2104:0313`,
Samsung Odyssey G93SC).** `tobii calibrate` ran the full sequence and
`tobii calibrate --apply` round-tripped the saved blob — no code change needed.

1. **Does calibration need its own realm unlock? → NO.** The five `cal_add_point`
   ops, `cal_compute`, and `cal_retrieve` all succeeded on the connection's
   already-open (handshake) realm with **no re-unlock**. The backend's assumption
   holds; no explicit enter/leave is required.
2. **Blob prefix round-trip? → ROUND-TRIPS CLEANLY.** A 1480-byte blob was retrieved
   verbatim and re-applied via `cal_apply` with the device acknowledging — the feared
   doubled `00 00` prefix either does not occur or is tolerated. **No stripping
   transform needed.** (Captured as a golden fixture: `tobii-protocol` `real-calibration.blob`.)
3. **Compute-done semantics? → CONFIRMED.** `cal_compute` returned promptly with a
   single response; treating the first response as "done" is correct.
4. **Per-point ack cadence? → CONFIRMED.** Each of the five `cal_add_point` ops got
   exactly one acknowledging response (no hang); `expect_response` per point is right.

5. **Eye-position present-mask bits** (B2 concern, NOT tested here): our crate marks
   raw eye-origin presence at bits 12/13 while one extraction reported 19/20 —
   reconcile against the live `present_mask` before the eye-position view relies on it.

## 11. What comes next (after this backend lands)

- **B2** — GUI shell (toolkit chosen then; egui/eframe is the leading candidate for a
  self-contained fullscreen-capable app) + display-setup screen (EDID-seeded) +
  eye-position view (needs cols `0x03`/`0x09` decode).
- **B3** — fullscreen follow-the-dot stimulus wired to this backend, then compute →
  save → auto-apply. This is where gaze accuracy is finally validated end-to-end.
