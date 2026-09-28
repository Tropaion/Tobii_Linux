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
| **One process at a time may claim the USB interface.** | The GUI must release the device when it is not using it, *and* on request, or `tobii headpose` cannot run for a game. Releasing on idle alone stopped being enough once the hub gained holds that never end by themselves (`keep_awake`, the virtual joystick), so the hub honours a **lease**: a client asks, the hub drops its session and waits, whatever its own demand says (`must_wait`, `crates/tobii-gtk/src/device.rs:229`). |
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
             (EDID, geometry,      (update check: unprompted at launch,
              layer-shell overlay)  opt-out-able; head-pose model download:
                                    only on an explicit click)
                            │
                     ~/.config/tobii-linux/
                (geometry, calibration blob, eye selection, pitch offset,
                 update-check preference, hub text size, games.toml, models/)
```

**In scope:** speaking the ET5 protocol, deriving screen geometry, running
calibration, deriving head pose, presenting all of it, and keeping itself
updated.

**Explicitly out of scope:** being a gaze-input method (no cursor control, no
dwell clicking); supporting other Tobii devices; a *general-purpose* IPC API — the
socket the background session serves carries tracking data, leases and recentre
requests for this project's own clients, and is not a public service interface.

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

Thirteen crates, one acyclic dependency graph, 92,833 lines. Counts measured
with `find crates -name '*.rs' | xargs wc -l` on 2026-09-28 — every figure in
this diagram had gone stale before, which is what a hand-maintained count does.

```
                    tobii-protocol  (3,552)  no dependencies at all
                     ▲    ▲     ▲
        ┌────────────┘    │     └──────────────┐
   tobii-usb (2,346)  tobii-config (5,341)  tobii-recap (1,600)
        ▲                ▲     ▲
        │      ┌─────────┘     └──────────┐
        │  tobii-headpose (5,659)   tobii-update (5,956)
        │      ▲      ▲                   ▲
        │      │  tobii-output (7,010)    │
        │      │      ▲      ▲            │
        │      │      │   tobii-diagnostics (2,308)
        │      │      │            ▲
        └──────┴──────┴────────────┴──────┐
                          │                │
          tobii-cli (21,555)      tobii-gtk (30,129)
```

Three crates hang off the two top-level binaries with **no dependencies at
all**, internal or external, and are drawn separately because an edge from each
to its users would cross the whole graph:

```
   tobii-ipc      (1,917)  ──▶ tobii-cli, tobii-gtk
   tobii-steam    (1,107)  ──▶ tobii-cli, tobii-gtk
   tobii-gameconf (4,353)  ──▶ tobii-gtk
```

Dependency-freedom is load-bearing for the last two rather than incidental:
`tobii-steam` stays callable from the hub without dragging the CLI's world
along, and `tobii-gameconf` — the crate that reads other people's game files —
keeps a surface small enough that "it cannot write anything" is a claim a
reader can check rather than take on trust.

| Crate | Responsibility | Notable |
|---|---|---|
| `tobii-protocol` | TTP framing, TLV/Q42 codec, op catalog, handshake state machine, gaze/camera/display decoders. | No I/O, no clock, no `unsafe`. The handshake is a *pure state machine* — it produces frames and consumes payloads, and never touches a socket. |
| `tobii-usb` | libusb transport, the connection driver, and the record/replay harness. | `Transport` is the seam everything testable hangs from. |
| `tobii-config` | Display geometry (including curved panels), EDID, persistence, SHA-256. | Every write is atomic; a truncated `config.toml` presents as a tracker that has stopped working. |
| `tobii-headpose` | 5-DOF geometric pose, the ONNX 6-DOF backend, the model store, opentrack output. | The two paths are fused, not alternatives: position from the eyes, rotation from the model. A frame with one tracked eye is reconstructed from the last measured interocular offset (≤300 ms, rotation *held*), and the smoothing filter is an EMA behind a gate that holds a sample whose position teleports. |
| `tobii-update` | Release checking, download integrity, installation with rollback. | All network access funnels through `net.rs`. No signature — see [[Quality-and-Risks]]. |
| `tobii-diagnostics` | The `tobii debug` report, its redaction, and the log the report quotes. | Its own tests fail if a home path, hostname or raw monitor id reaches the output. |
| `tobii-cli` | The `tobii` binary: user commands *and* the protocol diagnostics used to do the reverse engineering. | Five modules since the Wine bridge and `tobii uninstall` landed (`main.rs`, `bridge.rs`, `userreg.rs`, `wineserver.rs`, `uninstall.rs`); hand-rolled argument matching throughout. |
| `tobii-gtk` | The hub, the guided flows, the game-setup window, the overlay, and the one device thread. | 32% of the workspace. |
| `tobii-recap` | Decodes a usbmon pcap into a TTP op catalog. | Offline tool; how the protocol was mapped in the first place. |
| `tobii-ipc` | The socket the hub serves tracking data on: its path, the framed codec, the server and the client. | No workspace dependencies at all, so both ends of the socket share one codec. |
| `tobii-output` | Game output: the `games.toml` settings, the compose pipeline (Extended View, neutral, recentre), the opentrack port watch, and the sinks — UDP, virtual joystick, TrackIR/FreeTrack bridge. | Cross-compiles to `x86_64-pc-windows-gnu` for the Wine bridge, which is why the `/proc` watch is behind `cfg(target_os = "linux")`. |
| `tobii-steam` | Reading Valve's own files: library folders, installed application manifests, and where a title's Proton prefix is. | No dependencies, internal or external. It reports what Steam's files say and knows nothing about any particular game. |
| `tobii-gameconf` | Two read-only readers for a game's own configuration: `binds` (a directory of Elite Dangerous-style preset documents) and `attrs` (a flat `<Attributes>` document). | **Never writes, and that is the whole promise.** No dependencies, no `unsafe`, and `tests/writes_nothing.rs` fails if any byte of a fixture tree moves. It knows two file formats and no games: which file and which setting is a profile's business, and this project ships no profiles. A value it cannot decode exactly is `Rejected`, never reported as absent. |

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

  socket thread (one, started by `device::spawn`)
  ──────────────────────────────────────────────
  serves $XDG_RUNTIME_DIR/tobii-linux/tracker.sock — subscriptions, leases,
  recentre requests — and rides `PortWatch` on its own timer
```

Four threads carry the device path, and the boundaries are deliberate. (The
IPC server adds an accept thread and a reader/writer pair per client —
`crates/tobii-ipc/src/server.rs:117`, `:128` — and the updater two more, §6.5.)
The device thread owns the
connection and never blocks the UI. The head-pose worker runs the neural model
off the device thread on a **one-slot channel that drops rather than queues** —
a backlog of stale frames is worse than a skipped one. The socket thread is
started unconditionally by `device::spawn` (`crates/tobii-gtk/src/outputs.rs`),
so it exists on every launch shape including `--background`; it starts no
thread at all when another hub already holds the socket, which is degradation
rather than failure. §5 below is what it serves.

`Demand` is the reference count on the USB session. Consumers take a
`DemandGuard`: the hub while its window has focus, the gaze overlay while
shown, a calibration or setup flow while it runs, the `--accuracy` diagnostic
for the life of its process (`crates/tobii-gtk/src/lib.rs:818`), and a
**socket client** for
as long as it stays subscribed to pose, gaze or camera
(`outputs::Holds::hello`) — which is all `tobii game` is. One more consumer
needs no client at all: `outputs::PortWatch` holds a guard while a socket is
bound where the opentrack sink sends (`wake_for_opentrack`, default on, with
game output enabled), asked once a second through
`tobii_output::listener::probe`.

Two more are the **hub's own standing claims**, held by `GameSide` on the
device thread and synced from the same once-a-second config read that runs in
the idle wait and inside a session: *the virtual joystick* (`wake_for_joystick`,
default on — taken while game output is on and the uinput device really
exists) and *standby turned off in the settings* (`keep_awake`, default off —
taken unconditionally). They are the first reasons the hub holds on its own
behalf that **never end by themselves**, which is exactly why neither may be in
`EXCLUSIVE`: an exclusive claim that never ends would permanently refuse every
lease, recentre and calibration, naming something the user cannot see. See
[[Game-Output]] for why the joystick cannot be watched instead.

The sink plumbing still takes no guard of its own: `GameOutput::from_config`
routes frames, it does not ask for the device. A queued command is the one
thing that opens a session WITHOUT taking a guard — the device thread's wait is
the named `must_wait(demand_active, pending_empty, lease_blocks)`, which is
`(!demand.active() && pending.is_empty()) || lease_blocks`, so "select left eye
only" typed into an idle hub still takes effect and a lease still takes the
device from a claim that would otherwise never let go. Three seconds after the
last guard drops, the session closes, and the device thread logs why. See
[[Runtime-View]] §6.2 for why the linger exists.

**`tobii-update`** — `net.rs` is the only place the crate touches the network;
`install.rs` does digest → unpack → probe → swap with all-or-nothing rollback.

---

*Continue to [[Runtime-View]].*
