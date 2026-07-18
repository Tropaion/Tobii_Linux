# Tobii Eye Tracker 5 — Linux Runtime (Design Spec)

**Date:** 2026-06-14
**Status:** Approved design, pre-planning
**Author:** Fabian Plaimauer (with Claude Code)
**License:** GPL-3.0 (derives protocol knowledge from the GPL-3.0 `tobiifree` project)

## 1. Goal

A general-purpose, game-agnostic Linux runtime for the **Tobii Eye Tracker 5 (ET5)** —
the open equivalent of what the Windows Tobii driver provides, ported to Linux
(CachyOS / Arch). It reverse-engineers the device so it works natively on Linux.

The **first integration and test target is Star Citizen** (running under Proton),
driven via **opentrack** — but the runtime itself is not SC-specific. It is a
reusable Tobii driver that any head-tracking-capable game on Linux can consume
through opentrack's output protocols.

### Non-goals (v1)

- Per-user gaze calibration (the 9-point stimulus flow) — deferred to the
  eye-tracking phase that actually needs it (see §9).
- Full Tobii Game Integration API emulation (gaze / "Extended View" for all
  games) — later phase.
- Mouse/cursor control via uinput — later phase.

## 2. Background & Key Findings

- **The ET5 wire protocol is already fully decoded** by the `tobiifree` project
  (`ressources/tobiifree/`, GPL-3.0), by observing USB bulk transfers — *not*
  from the Windows MSI. It documents, at byte level: the USB envelope + TTP
  framing (24-byte big-endian header), the TLV codec (Q42 fixed-point), the
  handshake (hello → realm auth → display area → subscribe), realm
  authentication (HMAC-MD5), the calibration ops, and the gaze sample format
  (every column ID annotated). This is our authoritative protocol spec.
- **The MSI is not needed for the protocol.** It *is* useful for the
  display-setup math: it contains .NET assemblies (`Tobii.Configuration.*`,
  `TetConfig.dll`, `Tobii.Experience.Streaming.*`) that decompile to near-source
  C#, letting us match the original driver's display-area configuration exactly.
- **Star Citizen on Linux head tracking already works via opentrack.** The
  `the-sane/opentrack-StarCitizen` fork outputs through Freetrack or its Wine
  output, injecting the NaturalPoint/`NPClient` (TrackIR) registry key into SC's
  Proton prefix; SC reads it as a TrackIR/SmartNav device. This is **6DOF head
  tracking only**. SC's native Tobii eye/gaze support uses a Windows-only Tobii
  Game Integration component with **no Linux path** today.
- The missing Linux piece — and what this project provides — is a **high-quality
  ET5 head-pose source feeding opentrack**, which then performs the proven
  SC/Wine bridge.

### Hardware / environment (probed 2026-06-14)

| Item | State |
|---|---|
| ET5 device (`2104:0313` runtime, `2104:0102` bootloader) | Not currently plugged in; needed only at live-validation milestone |
| `libusb-1.0` | Present (1.0.30) |
| Rust toolchain | **Not installed** — setup step (rustup or pacman `rust`) |
| udev rule | Reuse `tobiifree/assets/99-tobii.rules` (`uaccess` + `0666`) |
| User groups | In `wheel` (sudo available) |
| MSI extraction | `7z`, `bsdtar` present; `msitools` optional |
| .NET decompiler | `ilspycmd` to install (dotnet tool / AUR) |

## 3. Architecture

A Cargo **workspace** of focused crates. The split keeps the pure protocol logic
I/O-free and unit-testable, isolates the USB transport, and leaves clean seams
for the deferred calibration / Tobii-API phases.

```
                 ┌──────────────┐
   ET5 ──usb──▶  │  tobii-usb   │  rusb/libusb transport
                 │ (transport)  │  open 2104:0313, bulk IN/OUT
                 └──────┬───────┘
                        │ raw bytes
                 ┌──────▼────────┐
                 │ tobii-protocol│  TTP framing, TLV/Q42, handshake SM,
                 │   (pure I/O-  │  realm HMAC-MD5, gaze decode,
                 │    free core) │  display-area encode/decode
                 └──────┬────────┘
                        │ GazeSample / responses
        ┌───────────────┼──────────────────┐
        │               │                  │
 ┌──────▼──────┐ ┌──────▼───────┐  ┌────────▼────────┐
 │tobii-headpose│ │ tobii-config │  │   tobii-cli     │
 │ 6DOF derive  │ │ display-setup│  │ orchestrates +  │
 │ from eye pos │ │ (RE'd math)  │  │ opentrack UDP   │
 └──────┬───────┘ └──────────────┘  └────────┬────────┘
        │ 6DOF                                │
        └──────────────▶ opentrack UDP input ◀┘ ──▶ Wine/TrackIR bridge ──▶ games
```

### Crates

- **`tobii-protocol`** *(pure, no I/O, no allocator dependence beyond std)*
  - TTP frame builder/parser (24-byte BE header; `0x51` req / `0x52` rsp /
    `0x53` notify).
  - USB envelope (de)framing including the asymmetric IN/OUT length convention
    and multi-transfer reassembly for large responses.
  - TLV codec: tag, u32, Q42 fixed-point f64 (`round(v * 2^42)`), 3D point,
    raw blob; plus the XDS row/column readers for gaze payloads.
  - Handshake state machine: hello (op `0x3e8`) → query/open realm
    (`0x640`/`0x76c`) → HMAC-MD5 challenge response (`0x776`) → set/get display
    area (`0x5a0`/`0x596`) → subscribe (`0x4c4`, stream `0x500`).
  - Gaze sample decode: `0x500` notification → typed `GazeSample` (timestamp,
    per-eye validity, pupil, 2D gaze (filtered/unfiltered), per-eye 3D eye
    origins (raw + calibrated), track-box positions, 3D ray-plane hits).
  - Returns `Result<_, ProtocolError>` everywhere; never panics on malformed
    device data.
- **`tobii-usb`**
  - `rusb` (libusb) device discovery for `2104:0313`; detach kernel driver if
    attached; claim interface; locate bulk IN/OUT endpoints.
  - `send(&[u8]) -> Result<()>`, `recv(&mut [u8]) -> Result<usize>` with a
    non-blocking `try_recv` variant; drives the protocol core's handshake +
    poll loop.
  - Clear diagnostics: not-found, permission-denied (→ udev rule hint),
    claim-failure (→ kernel detach), timeout.
- **`tobii-config`**
  - Display-setup model with the **same inputs as the original driver** (monitor
    width/height in mm, tracker mounting position & vertical offset, screen
    tilt), reverse-engineered from the decompiled `Tobii.Configuration` /
    `TetConfig` assemblies, producing the 3-corner display area sent via op
    `0x5a0`.
  - Persistence: our own **TOML** config file (user chose to match the *inputs
    and math*, not Tobii's on-disk format). Default location:
    `$XDG_CONFIG_HOME/tobii-linux/config.toml`.
- **`tobii-headpose`**
  - Derives 6DOF head pose from per-eye 3D positions:
    - **translation (x/y/z)** = midpoint of the two eye origins,
    - **yaw** = atan2 of the inter-eye vector in the horizontal plane,
    - **roll** = atan2 of the inter-eye vector in the vertical plane,
    - **pitch** = the weak axis (see Risk R1 / Spike S1): determined by an early
      research spike — either a head/face field exposed by the protocol, or an
      estimator; documented honestly with its limitations.
  - Output units/axes normalized to opentrack's convention.
- **`tobii-cli`** — the binary the user runs:
  - `tobii setup` — interactive display-setup config (matches original inputs),
    writes TOML, sends display area to device.
  - `tobii display get|set` — read/write the device display area.
  - `tobii stream [--json]` — print gaze / head-pose samples (debug/inspection).
  - `tobii opentrack [--host H --port P]` — connect, handshake, stream **6DOF
    head pose to opentrack's UDP input**. The primary game-integration command.

## 4. Data Flow

**Inbound:** USB bulk-IN → `tobii-usb.recv` → `tobii-protocol` reassembler →
frame dispatch → `GazeSample` → `tobii-headpose` (6DOF) → opentrack UDP packet
(or `tobii-cli` stdout).

**Outbound (config/commands):** `tobii-cli` command → `tobii-protocol` frame
builder → `tobii-usb.send` → USB bulk-OUT; responses captured by request-id
correlation in the protocol core.

## 5. Integration Boundary (opentrack)

- Target: opentrack's **"UDP over network"** tracker input. Expected packet is
  6 little-endian `f64`: `x, y, z, yaw, pitch, roll`. **Exact format, units
  (cm vs mm, degrees), and axis signs to be confirmed in Spike S2** against the
  opentrack source before finalizing `tobii-headpose` output.
- opentrack is then configured (out of scope for our code, documented in README)
  with the SC fork's Wine/Freetrack output to bridge into Star Citizen's Proton
  prefix. Because opentrack supports many output protocols, the same head-pose
  source works for any head-tracking-capable game on Linux.
- Neutral-pose centering, dead-zone, and per-axis sensitivity curves are handled
  **in opentrack**, not in our runtime.

## 6. Error Handling

- USB: device-not-found, permission-denied (hint: install udev rule), interface
  claim failure (hint: kernel driver detach), transfer timeout / disconnect
  (graceful reconnect attempt in the `opentrack`/`stream` loops).
- Protocol: handshake timeout, realm-auth failure, frame parse errors — surfaced
  as typed errors with context; the core never panics on bad device bytes.
- CLI: actionable messages and non-zero exit codes; `--json` mode keeps stdout
  clean (diagnostics to stderr).

## 7. Testing Strategy

- **`tobii-protocol` unit tests (no hardware):** golden byte vectors — e.g.
  `build_hello` must equal the known 79-byte frame; decode a captured `0x500`
  gaze payload into the expected `GazeSample`; Q42 encode/decode round-trips;
  display-area encode/decode round-trip; multi-transfer reassembly. Vectors
  sourced from `tobiifree`'s embedded payloads and any captures.
- **`tobii-config` tests:** golden values cross-checked against the math read
  from the decompiled original (same inputs → same corners).
- **`tobii-headpose` tests:** synthetic eye-position inputs → expected 6DOF
  outputs; documented pitch behavior.
- **Live hardware validation (one milestone, ET5 plugged in):** handshake
  completes; gaze streams; display area round-trips; head pose feeds opentrack;
  confirmed controlling the camera in Star Citizen.

## 8. Reverse-Engineering Workflow

- **Protocol:** use `tobiifree` as the authoritative decoded spec; write a clean
  Rust reimplementation. (GPL-3.0 obligations → this project is GPL-3.0.)
- **MSI / display-setup math:** extract `drivers.cab` (`7z`), de-mangle the
  installer file names, decompile the `Tobii.Configuration` / `TetConfig` .NET
  assemblies (`ilspycmd`), locate the function mapping setup inputs → display
  area, document and reimplement it in `tobii-config`.

## 9. Scope & Phasing

- **v1 (this spec):** core driver (handshake, gaze stream ✅, display-area config
  ✅ via `tobii-config`/`tobii setup`) + `tobii-headpose` + opentrack output (next)
  → playable in Star Citizen and any head-tracking game.
- **Phase 2 (later):** per-user gaze calibration (stimulus points, add-point,
  compute/apply, save/load blob).
- **Phase 3 (later):** Tobii Game Integration API emulation (Wine shim) for true
  gaze / Extended View across games.
- **Phase 4 (later):** cursor control via uinput.

## 10. Risks & Early Spikes

- **R1 / Spike S1 — Pitch axis.** Per-eye 3D positions give yaw/roll/translation
  cleanly but pitch is weak. Spike: inspect the protocol for head/face-model
  data; if absent, design and document a pitch estimator and its limits. Do this
  before committing the `tobii-headpose` design.
- **Spike S2 — opentrack UDP format.** Confirm exact packet layout, units, and
  axis signs from opentrack source before finalizing head-pose output.
- **Spike S3 — display-setup math location. RESOLVED 2026-07-14** (see
  `specs/2026-07-14-spike-s3-display-setup-math.md`). The forward math is **native**
  (`TetConfig.dll`, x86-64), *not* in the decompilable managed assemblies — those
  only collect inputs → registry and read the finished corners back. Decision: do not
  native-RE it; `tobii-config` implements a validated planar-rectangle model (from the
  `tobiifree` reference + first principles) that reproduces a real working display
  area to < 0.1 mm, exposing the spec's intended inputs (monitor W/H mm, mounting &
  vertical offset, screen tilt angle). This refines §3/§8's "match the original math
  exactly" to "match the original *inputs*; equivalent validated math."
- **R2 — Device prerequisite for streaming. RESOLVED 2026-07-15 (live, on a
  physical ET5 `2104:0313`).** Gaze streams immediately after the handshake
  (hello → query/open realm → HMAC-MD5 realm auth → subscribe) with **no prior
  user calibration** — the realm-auth path was exercised for real and the stream
  began at ~33 Hz. When no eyes are in the trackbox the device reports its invalid
  sentinel (`gaze=(-1,-1)`, `validity=4`), which is expected, not a decode error.
  No extra device-side step was needed. Also confirmed live: the `get_display_area`
  response echoes op `0x596`, `set_display_area` (op `0x5a0`) **is acknowledged**
  with a response frame, and a `setup`→`display set`→`display get` round-trip is
  byte-exact. The device shipped with an uninitialised 4×4 mm placeholder display
  area (never configured by real Tobii software). Real captured gaze + display-area
  frames are now golden decode vectors (`tobii-protocol` `gaze.rs`/`display.rs`).
- **R3 — Firmware mode.** Device may appear in bootloader mode (`2104:0102`);
  reuse `tobiifree`'s DFU tooling/notes if a flash is ever required (not expected
  for v1).
