# Quality, risks and glossary (arc42 §10–12)

What is measured, what is assumed, and what is known to be wrong. Background in
[[Architecture]]; the reasoning is in [[Architecture-Decisions]].

> Every number here is quoted with where it came from. **If a figure appears
> without a source, treat it as a defect in this page.**

---

## 10. Quality requirements

### 10.1 Accuracy — the top quality goal

| Property | Measured | Where |
|---|---|---|
| Gaze accuracy, 49" 32:9 panel, within ±28° | **12.1 mm** mean error (≈0.30° at 750 mm) | measured after the calibration op-code fix |
| Before the op-code fix | **80 mm** | same panel — every calibration before 2026-08-15 was a silent no-op |
| Head-pose yaw vs geometry | slope **+1.03**, r = **0.998** | `tobii headpose --check` on hardware |
| Head-pose roll vs geometry | slope **+0.96**, r = **0.990** | same |
| Pitch offset (mounting + model frame) | **+24.08°**, 10–90% spread 2.57° | `--calibrate-pitch` on the reporter's setup |

### 10.2 Latency and throughput

| Property | Measured | Notes |
|---|---|---|
| Gaze stream | ~33 Hz | the device's rate |
| ONNX inference | **12.4 ms/frame** single-threaded | the reason the worker is a separate thread |
| Eye-dot redraw | GTK frame clock | the 33 ms timer was dropping ~3 frames/sec |
| Head-pose output | 60 Hz default (`--rate`) | opentrack UDP |
| Camera frame | 78 KB at 33 Hz | `Arc`-shared, two readers |

### 10.3 Robustness

- **Every config write is atomic.** An unparseable file reads as *unset*, never
  as a wrong value.
- **Reconnect is invisible.** Unplug/replug re-runs the full re-application
  sequence; the watchdog notices 2 s of silence.
- **An update either lands completely or not at all**, and the binary is never
  absent mid-swap.
- **A new binary is run before it is installed**, so a build for the wrong glibc
  is refused while the working one is still in place.

### 10.4 Not surprising the user

- Exactly **one** unprompted network request, with an opt-out.
- The tracker runs only while something needs it.
- The autostart session opens **no window** and **no USB session** —
  measured: zero USB fds and zero GPU fds while idle.

---

## 11. Risks and technical debt

### 11.1 Known and accepted

**No signature on updates.** `SHA256SUMS` is an integrity check. Anyone able to
publish to the repository's releases could publish binaries every check accepts.
This is stated in the code, the README and the UI. *Closing it needs a key
pinned in the binary; there is none.*

**The head-pose focal length is assumed.** `DEFAULT_FOCAL_PX = 355` feeds a
perspective correction worth 12–16° of pitch. `focal_from_eye_origins` can
compute the real value from data already on the wire; it is not yet wired up.

**The replay fixture is a photograph.** It proves the code still does what it
did when recorded, not that the device still does. A firmware change would
invalidate it silently and the tests would stay green.

### 11.2 Confirmed defects

Found by review and reproduced. Fixed ones are kept here with what they were,
because the reasoning is worth more than the tidiness.

**Fixed:**

- ~~The parser's continuation-envelope heuristic false-positived~~ — `feed`
  stripped 8 bytes whenever a frame was in flight and the chunk began
  `01 00 00 00`, which is also a legitimate type-0x01 TLV field header and turns
  up in camera pixel data. The envelope's own length field is checked now.
- ~~`parse_enabled_eye` did no structural parsing~~ — it read the last four
  bytes of any slice, so `[0,0,0,2]` returned `Some(Right)`. It goes through
  `Reader` now.
- ~~`write_atomic` did not `fsync`~~ — rename gives atomicity of *visibility*,
  not durability of *content*. The temp file is synced before the rename and the
  directory after it.
- ~~`Capture::parse` panicked on a non-ASCII line~~ — `split_at(1)` aborts when
  byte 1 is not a char boundary, so a hand-added note with an umlaut killed the
  process instead of returning `BadLine`.
- ~~A dropped queued command lost the eye selection permanently~~ — it was
  persisted inside `apply_command`, so choosing an eye with the tracker
  unplugged never reached disk. The UI saves it at the point of choice now, and
  a dropped command that a UI is waiting on publishes a failure rather than
  stranding it on a token that never arrives.

**Open:**

- **A failed query-realm parse is indistinguishable from "no authentication
  required."** `resp_first_u32_opt` cannot tell them apart, because the walker
  above finds no field either way, and 0 means skip auth. Making the unreadable case *fail* was
  tried and the replay harness caught it breaking every connection: the real
  reply uses the 5-byte TLV framing while `resp_fields` walks a 4-byte one, so
  it finds nothing and the 0 default is what makes the handshake work. The
  walker is the real defect; it is left alone because the only reply with
  content we have is that one, and changing an unvalidated parser on a single
  capture is how this project has hurt itself before.
- **Three parallel TLV decoders** live in `tobii-protocol` (`tlv.rs`,
  `handshake.rs`, `camera.rs`). A hardening applied to one does not reach the
  others — see the entry above for what that costs.
- **The stale calibration fixture.** `testdata/real-calibration.blob` predates
  the calibration op-code fix.
- **Stale confidence markers.** At least one op is rated [UNCONFIRMED] in
  `frame.rs` while another module describes its reply parser as confirmed.

### 11.3 Untested surfaces

- **`UsbTransport` itself has no test that touches libusb.** Open/claim/detach,
  the vendor control transfers, chunking, `soak_incoming`, and the `Drop`
  session close are exercised only by running the program.
- **The replay captures cover two slices, not everything.** `session.tobiicap`
  is the everyday path (handshake, display area, enabled eye, gaze, unsubscribe);
  `calibration.tobiicap` adds the one fragmented response this driver produces,
  a 778 KB blob arriving across 60 reads with continuation envelopes. Still no
  camera stream and no error paths.
- **The replay test hand-reproduces `tobii-cli`'s recording sequence.** The two
  live in different crates and can drift apart silently.
- **Past `SOAK_CAP` (1 MiB), `soak_incoming` drops IN bytes** — creating exactly
  the stream hole its own comment says buffering exists to avoid. Reachable if
  the camera stream is subscribed while a calibration blob is applied.
- **The head-pose worker's exit is asserted in a comment, not demonstrated.**
  A live thread-count sample rose monotonically across session cycles without
  falling back; GTK's own threads could not be separated out. Worth one clean
  measurement.

### 11.4 Environmental

- **Glyph clipping at fractional display scale.** Tops of tall glyphs appear
  shaved. CSS, the GSK renderer and widget-label theories have all been ruled
  out by measurement, and it is not reproducible offscreen. Remaining suspect:
  the compositor's own downscale. Setting an integer scale is the test.
- **`0x501` and `0x50e` are the same camera exposed twice**, not a stereo pair —
  199/199 byte-identical frames with a face in view, 166/166 on an empty scene.
  There is no stereo depth to be had from this device.
- **The GUI always pulls `tract`.** `tobii-gtk` has no `onnx` feature gate.

### 11.5 Process

- **Hardware claims cannot be re-verified by CI.** The accuracy figure, the sign
  conventions and the tracker-on behaviour were measured by hand. If they
  regress, nothing will say so.
- **Neither the release workflow nor the release path has ever run.** The
  repository has no tags, so the first tag will be the first execution of
  `release.yml` itself *and* of the updater's download-and-install path against
  a real release. `ci.yml` has run (and failed once, usefully, on a clippy lint
  the maintainer's toolchain was too old to see).

---

## 12. Glossary

| Term | Meaning |
|---|---|
| **TTP** | The device's framing: a 24-byte header (magic, seq, op) inside an 8-byte USB envelope. Note the envelope length field is asymmetric — outbound it excludes the envelope, inbound it includes it. |
| **TLV** | Type-length-value payload encoding. Three variants exist in this codebase; see §11.2. |
| **Q42** | Fixed-point: a signed 64-bit integer scaled by 2⁴². How the device sends reals. |
| **XDS row/column** | The tagged table structure a gaze frame is built from — 39 columns. |
| **Realm** | The device's authentication scope. Opening one may require an HMAC-MD5 response to a challenge; `realm_type == 0` means no auth. |
| **Trackbox** | The volume in which the device can see eyes. Eye position within it is normalised `[0,1]` in x, y **and z** — z is normalised depth, *not* millimetres. |
| **Display area** | The three tracker-space corners (TL, TR, BL) defining the screen. The fourth is implied. Wiped on every device reboot. |
| **Demand** | The reference count on the USB session. While it is zero the tracker is off and the device is free for another process. |
| **Linger** | The 3 s after the last `DemandGuard` drops before the session closes. |
| **Present bit vs validity** | A present bit means the column was sent, **not** that the data is good. A no-eyes frame carries eye origins present and set to `[0,0,0]` with validity 4. Gate on both. |
| **opentrack datagram** | **Six** little-endian `f64` (48 bytes): x, y, z in centimetres, then yaw, pitch, roll in degrees. |
| **Capture** | A recorded USB session (`tobii record`) replayed in tests. A photograph, not a specification. |

---

*Back to [[Architecture]] · developer guide: [[Development]]*
