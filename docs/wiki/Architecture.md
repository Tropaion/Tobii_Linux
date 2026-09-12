# Architecture (arc42 §1–5)

Introduction and goals, constraints, context, solution strategy, and the
building block view. The rest of the architecture documentation:

| Page | arc42 |
|---|---|
| [[Runtime-View]] | §6 runtime, §7 deployment |
| [[Architecture-Decisions]] | §8 cross-cutting concepts, §9 decisions |
| [[Quality-and-Risks]] | §10 quality, §11 risks and technical debt, §12 glossary |
| [[Development]] | how to build, test and contribute |

> Everything here was read out of the code at the commit this page was written
> against, and figures are quoted with their source. Where something is inferred
> rather than measured it says so. **A statement here that the code does not
> support is a bug in this page** — this project has been bitten before by
> documentation asserting guarantees that did not exist.

---

## 1. Introduction and goals

A native Linux runtime and GUI for the **Tobii Eye Tracker 5**, in Rust, with no
Tobii software installed. The USB protocol was reverse-engineered: mapped from
this project's own USB captures, cross-checked against the third-party
`tobiifree` project, and with op *names* and enum orderings read from a decompile
of Tobii's own software. No Tobii code was copied, and
[Reverse-Engineering-Methodology](Reverse-Engineering-Methodology.md) records
which source backs which claim.

**Top three goals, in order:**

1. **The tracker works.** Gaze streaming and calibration accurate enough to be
   used, not merely demonstrated. Measured: **12.1 mm** mean error on a 49" 32:9
   panel within the device's usable ±28°, ≈0.92° at 750 mm.
2. **It behaves like a well-made desktop application.** It does not surprise the
   user: no unexpected network traffic, no device left running, no window
   appearing uninvited, no claim in the UI that the code cannot back.
3. **The reverse engineering survives.** What was learned by watching real
   hardware must be written down and regression-tested, or it decays into
   folklore the moment the person who measured it stops working on it.

**Stakeholders:** ET5 owners on Linux (the product); head-tracking gamers
(opentrack output); contributors (who mostly will not own the hardware); and
anyone reverse-engineering this device (the [[Op-Catalog]] and friends are
written to be usable on their own).

---

## 2. Architecture constraints

Not negotiable, and most of them are the device's doing rather than ours.

| Constraint | Consequence |
|---|---|
| **The protocol is undocumented.** Everything is inferred from captures and disassembly. | Confidence markers ([CONFIRMED] / [CODE-VERIFIED] / [HYPOTHESIS]) are part of the source. A claim without evidence is treated as a defect. |
| **The ET5 reboots on session close, wiping its display area *and* its calibration.** | Both must be re-applied on **every** connect, or the tracker reports no eyes at all. This shapes the whole device thread. |
| **The IR illuminators are lit for as long as a USB session is open.** | The session is reference-counted (`Demand`) rather than held for the process lifetime. |
| **One process at a time may claim the USB interface.** | The GUI must release the device when it is not using it, or `tobii headpose` cannot run for a game. |
| **The head-pose model's weights are non-commercial-only** (opentrack). | Not shipped. Fetched only after the user is shown the terms and agrees. |
| **A dynamically linked binary needs a glibc at least as new as the one it was built against.** | Releases are built in a Debian 13 container, and the floor is enforced in CI. |
| **GPL-3.0-only.** | Dependencies must be compatible. |
| **Linux; Wayland strongly preferred.** | The gaze overlay uses `layer-shell`, which has no X11 equivalent. |
| **GTK 4** with `gtk4-layer-shell`. | The GUI needs a reasonably current distribution; the CLI does not. |

---

## 3. Context and scope

```
                        ┌──────────────────────┐
   Tobii Eye Tracker 5  │                      │   opentrack / games
   USB 2104:0313    ────┤     TobiiLinux       ├──── UDP 127.0.0.1:4242
   (bulk endpoints)     │                      │     (6-DOF datagram)
                        └───┬───────────┬──────┘
                            │           │
             X/Wayland display     GitHub releases API + raw.githubusercontent
             (EDID, geometry,      (update check; head-pose model download —
              layer-shell overlay)  both only when asked, and both opt-out-able)
                            │
                     ~/.config/tobii-linux/
                (geometry, calibration blob, eye selection, pitch offset,
                 update-check preference, hub text size, models/)
```

**In scope:** speaking the ET5 protocol, deriving screen geometry, running
calibration, deriving head pose, presenting all of it, and keeping itself
updated.

**Explicitly out of scope:** being a gaze-input method (no cursor control, no
dwell clicking); supporting other Tobii devices; a daemon with an IPC API — the
background session exists only to keep the device configured, not to serve other
processes.

---

## 4. Solution strategy

Five decisions that shape everything else. Each is expanded in
[[Architecture-Decisions]].

1. **A pure codec at the bottom, with no I/O.** `tobii-protocol` has zero
   dependencies, does no I/O, and holds no state beyond a reassembly buffer.
   Everything above it can be tested by handing it bytes.
2. **The device is behind a two-method trait.** `Transport` is `send` + `recv`.
   That seam is why the driver can be driven by a mock, by a recorded session,
   or by libusb, and why CI can regression-test a reverse-engineered protocol
   with no hardware.
3. **Hand-roll small things rather than take a dependency.** SHA-256, MD5, JSON
   and the HTTP fetch (via `curl`/`wget`) are all local. A driver that must be
   installable from source on any distribution pays a real price for a large
   dependency tree. Each is small, each is cross-checked against a reference
   implementation in its tests.
4. **The device is a resource, not a lifetime.** The USB session is
   reference-counted and closed when nothing needs it, because holding it lights
   the illuminators and locks out other processes.
5. **Say what is proven, and say what is not.** Confidence markers in the source,
   measured numbers in the docs, and a `Status` section that admits what has
   never been run. This is a strategy, not a style: the project's worst bugs
   have come from believing something that was never checked.

---

## 5. Building block view

### Level 1 — the workspace

Nine crates, one acyclic dependency graph, ~33,600 lines. Counts measured with
`find crates -name '*.rs' | xargs wc -l`, September 2026 — every figure in this
diagram had gone stale before, which is what a hand-maintained count does.

```
                    tobii-protocol  (3,437)  no dependencies at all
                     ▲    ▲     ▲
        ┌────────────┘    │     └──────────────┐
   tobii-usb (2,281)  tobii-config (1,856)  tobii-recap (1,600)
        ▲                ▲     ▲
        │      ┌─────────┘     └──────────┐
        │  tobii-headpose (4,559)   tobii-update (3,647)
        │      ▲                          ▲
        │      │              tobii-diagnostics (1,334)
        │      │                          ▲
        └──────┴──────────┬───────────────┘
                          │
          tobii-cli (2,272)   tobii-gtk (12,619)
```

| Crate | Responsibility | Notable |
|---|---|---|
| `tobii-protocol` | TTP framing, TLV/Q42 codec, op catalog, handshake state machine, gaze/camera/display decoders. | No I/O, no clock, no `unsafe`. The handshake is a *pure state machine* — it produces frames and consumes payloads, and never touches a socket. |
| `tobii-usb` | libusb transport, the connection driver, and the record/replay harness. | `Transport` is the seam everything testable hangs from. |
| `tobii-config` | Display geometry (including curved panels), EDID, persistence, SHA-256. | Every write is atomic; a truncated `config.toml` presents as a tracker that has stopped working. |
| `tobii-headpose` | 5-DOF geometric pose, the ONNX 6-DOF backend, the model store, opentrack output. | The two paths are fused, not alternatives: position from the eyes, rotation from the model. A frame with one tracked eye is reconstructed from the last measured interocular offset (≤300 ms, rotation *held*), and the smoothing filter is an EMA behind a gate that holds a sample whose position teleports. |
| `tobii-update` | Release checking, download integrity, installation with rollback. | All network access funnels through `net.rs`. No signature — see [[Quality-and-Risks]]. |
| `tobii-diagnostics` | The `tobii debug` report, its redaction, and the log the report quotes. | Its own tests fail if a home path, hostname or raw monitor id reaches the output. |
| `tobii-cli` | The `tobii` binary: user commands *and* the protocol diagnostics used to do the reverse engineering. | Single file, hand-rolled argument matching. |
| `tobii-gtk` | The hub, the guided flows, the overlay, and the one device thread. | 38% of the workspace. |
| `tobii-recap` | Decodes a usbmon pcap into a TTP op catalog. | Offline tool; how the protocol was mapped in the first place. |

### Level 2 — the pieces that carry the design

**`tobii-protocol`** — `frame.rs` holds the op catalog and `build_out_frame`,
the single funnel for every outbound frame. `tlv.rs` is the codec; note that
**three different TLV framings live in this crate** (`tlv.rs` uses
`[type:u8][size:u32 BE]`, `handshake.rs` a 4-byte variant, `camera.rs` its own
walk because a column id and a column value share a type byte). `parser.rs`
reassembles the inbound byte stream. `gaze.rs` decodes the 39-column gaze frame
and, importantly, **never fails** — on truncation it returns the partial sample,
matching the reference decoder.

**`tobii-usb`** — `Connection<T: Transport>` multiplexes one bulk stream into
request/response round trips (matched on op **and** seq), a queue of decoded
gaze samples, and raw notifications for diagnostics. `capture.rs` records a real
session and replays it; see [[Development]].

**`tobii-gtk`** — the shape that matters:

```
  GTK main thread                     device thread (one)
  ─────────────────                   ────────────────────
  hub window, flows, overlay          idle ⇄ connected
        │  Demand (Arc<Mutex<…>>)           │
        ├──── holds/releases ──────────────►│ opens/closes the USB session
        │  Sender<DeviceCommand>            │
        ├──── settings, calibration ───────►│
        │  Arc<Mutex<DeviceState>>          │
        │◄──── snapshot, 33 ms tick ────────┤
                                            │  SyncSender(1), drops when full
                                            └──► head-pose worker thread
                                                 (ONNX, ~12.4 ms/frame)
```

Three threads, and the boundaries are deliberate. The device thread owns the
connection and never blocks the UI. The head-pose worker runs the neural model
off the device thread on a **one-slot channel that drops rather than queues** —
a backlog of stale frames is worse than a skipped one.

`Demand` is the reference count on the USB session. Consumers take a
`DemandGuard`: the hub while its window has focus, the gaze overlay while shown,
a calibration or setup flow while it runs, and a **socket client** for as long as
it stays subscribed to pose, gaze or camera (`outputs::Holds::hello`) — which is
all `tobii game` is, and the only reason a game lights the tracker. Game output
itself takes no guard: the switch routes frames, it does not ask for the device.
A queued command is the one thing
that opens a session WITHOUT taking a guard — the device thread's wait is
`!demand.active() && pending.is_empty()`, so "select left eye only" typed into
an idle hub still takes effect. Three seconds after the last guard drops, the
session closes. See
[[Runtime-View]] §6.2 for why the linger exists.

**`tobii-update`** — `net.rs` is the only place the crate touches the network;
`install.rs` does digest → unpack → probe → swap with all-or-nothing rollback.

---

*Continue to [[Runtime-View]].*
