# TobiiLinux

A native **Linux runtime and GUI for the Tobii Eye Tracker 5**, written in Rust —
a clean-room reimplementation of the device's USB protocol, with no Tobii
software required. Gaze streaming, guided display setup, follow-the-dot
calibration, 6-DOF head tracking for games, and a graphical configuration app
inspired by the original Tobii Experience UI.

> **Unofficial.** Not affiliated with, endorsed by, or supported by Tobii. The
> protocol was reverse-engineered clean-room (the [`tobiifree`](https://github.com/Aetherall/tobiifree)
> project is the reference). Use at your own risk.

## What it does

### Eye tracking

- **Gaze streaming** over USB (TTP protocol): 2D gaze point, per-eye validity,
  eye position in the trackbox, eye origins in millimetres, pupil size.
- **Calibration** — the follow-the-dot flow: 7 points shown in three groups,
  computed and applied on the device after each group, then persisted and
  re-applied on every connect.
  Measured accuracy on a 49" 32:9 panel: **12.1 mm** mean error within the
  device's usable ±28°, or about 0.30° at 750 mm.
- **Display setup** — a fullscreen guided flow: drag two lines onto the marks at
  the ends of the tracker and the screen geometry is derived, seeded from your
  monitor's EDID. Curved panels are handled (arc→chord width plus a gaze
  correction; the device itself can only be told about a flat plane).
- **Select eyes to detect** — both, left only, or right only.

### Head tracking

- **5 DOF with no extra download**: position in millimetres plus yaw and roll,
  derived from the two eye origins.
- **6 DOF with an optional model**: adds **pitch**, which two eye origins cannot
  express — nodding rotates the head about the line through them and leaves both
  origins where they were. A neural model reads it from the tracker's own
  infrared camera at ~12 ms a frame. The model is **not shipped**: its weights
  are non-commercial-only, so the program shows the terms and fetches it only if
  you agree.
- **Output to games** over opentrack's UDP protocol.
- **Pitch zero calibration** — the model reports pitch in its own frame, offset
  by how your tracker is mounted. `tobii headpose --calibrate-pitch` measures it
  once.

### The app

- **GTK4 hub** (`tobii-gtk`): a live instrument panel — the trackbox with a
  graticule and the tolerance region drawn, your eyes as dots, the head drawn
  around them turning as you turn, a readout of position, distance, yaw, pitch
  and roll, and the infrared sensor view. Responsive: the columns stack on a
  narrow window.
- **Preview my gaze** — a translucent click-through dot that follows your gaze
  (Wayland `layer-shell`).
- **Accuracy diagnostic** (`tobii-gtk --accuracy`) — a 39-target sweep reporting
  error per target, per angle band and per eye, with a diagnosis.
- **Updates** — the hub checks for a new release at launch and can show the
  changelog and install it. Nothing is downloaded without a click, and nothing is
  installed that does not match the checksums published with the release.

## Requirements

- **Rust** (stable, edition 2021) — e.g. via [rustup](https://rustup.rs).
- A **Tobii Eye Tracker 5** (USB `2104:0313`).
- System libraries: **GTK 4**, **`gtk4-layer-shell`** (for the gaze overlay),
  and **libusb 1.0** + **pkg-config**.
  - Arch / CachyOS: `sudo pacman -S --needed gtk4 gtk4-layer-shell libusb pkgconf`
  - Debian / Ubuntu: `sudo apt install libgtk-4-dev libgtk4-layer-shell-dev libusb-1.0-0-dev pkg-config`
- `curl` or `wget`, and `tar`, for fetching the head-pose model and updates.
- A **Wayland** session is recommended (the gaze overlay uses `layer-shell`).

## Build

```sh
cargo build --release
```

The neural head-pose backend is on by default and brings a sizeable dependency
tree (`tract`, ~110 crates). The **CLI** can be built without it — 5 DOF head
tracking, everything else unchanged:

```sh
cargo build --release -p tobii-cli --no-default-features
```

The GUI currently always includes it; `tobii-gtk` has no matching feature gate
yet.

## Device access (one-time)

Install the udev rule so the tracker is usable without root:

```sh
sudo cp assets/99-tobii.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger
```

Then (re-)plug the Eye Tracker 5.

## Usage

### GUI

```sh
cargo run --release -p tobii-gtk
```

The hub shows connection status, the live instrument panel, and the settings:
improve calibration, head tracking, preview my gaze, select eyes, change screen.

### Head tracking for games

```sh
./target/release/tobii headpose --fetch-model     # optional: adds pitch
./target/release/tobii headpose --calibrate-pitch # once, sitting normally
./target/release/tobii headpose                   # stream to opentrack on :4242
```

Point opentrack's **UDP over network** input at `127.0.0.1:4242`. Without the
model this still works — you get position, yaw and roll, and pitch reads zero.

`tobii headpose --check` prints the model's yaw and roll beside the geometry's,
which is how the sign conventions were confirmed on hardware.

### CLI

```sh
./target/release/tobii stream [--json] [--eyes]   # decoded gaze samples
./target/release/tobii setup                      # interactive display setup
./target/release/tobii display get|set            # read / re-apply display area
./target/release/tobii enabled-eye [both|left|right]
./target/release/tobii update [--install]         # check for a new release
```

Run `tobii` with no arguments for the full list, including the protocol
diagnostics (`streams`, `log`, `dump-stream`, `camera`, `cal-blob`, `cal-points`).

## Configuration

Stored under `$XDG_CONFIG_HOME/tobii-linux/` (default `~/.config/tobii-linux/`):
`config.toml` (display geometry), `calibration.bin`, `enabled_eye`,
`headpose_pitch_offset`, and `models/` (the fetched head-pose model).

> **Note:** the ET5 wipes its display area *and* its calibration every time it
> reboots — which it does on every session close — so the driver **re-applies
> both on every connect**. Without this the tracker reports no eyes at all.

## Status

**Validated on hardware:** gaze streaming, display setup, calibration (the
12.1 mm figure above is measured, not estimated), the live views, the gaze
overlay, and head tracking. The head-pose model's yaw and roll signs were
confirmed against the geometric pose (slope +1.03 and +0.96, r = 0.998 and
0.990).

**Implemented but unproven:** the **update mechanism** has never run against a
real release — this repository has no tags yet, so the download/verify/install
path has only been exercised against fixtures.

**Known limitations:**

- The head-pose model's **focal length is assumed** (`DEFAULT_FOCAL_PX = 355`),
  which feeds a perspective correction worth 12–16° of pitch.
  `focal_from_eye_origins` can compute the real value from data already on the
  wire; it is not yet wired to do so automatically.
- On a display at **fractional scaling**, the tops of tall glyphs can appear
  shaved. CSS, the GSK renderer and widget-label theories have all been ruled
  out by measurement and it is not reproducible offscreen; the remaining suspect
  is the compositor's own downscale. Setting the display to an integer scale is
  the test.
- `0x501` and `0x50e` are the **same** camera exposed twice, not a stereo pair
  (199/199 byte-identical frames with a face in view, 166/166 on an empty
  scene). There is no stereo depth to be had from this device.

## Protocol documentation

The reverse-engineered ET5 USB protocol is documented in [`docs/wiki/`](docs/wiki/)
(mirrored to the GitHub project wiki): TTP framing, the handshake, the full op
catalog, the gaze-stream column layout, display-area / calibration /
select-eyes, the stream map, head pose, and the reverse-engineering methodology.
Every non-obvious claim is tagged CONFIRMED / CODE-VERIFIED / HYPOTHESIS.

## Architecture

A Cargo workspace of focused crates:

| Crate | Responsibility |
|---|---|
| `tobii-protocol` | Pure protocol codec: TTP framing, handshake, gaze and camera decode (no I/O). |
| `tobii-usb`      | libusb (`rusb`) transport + connection driver. |
| `tobii-config`   | Display geometry, EDID detection, persistence, SHA-256. |
| `tobii-headpose` | Head pose: the geometric fallback, the ONNX backend, the model store, opentrack output. |
| `tobii-update`   | Release checking, verification and installation. |
| `tobii-cli`      | The `tobii` command-line tool. |
| `tobii-gtk`      | The GTK4 hub, guided flows and gaze overlay. |
| `tobii-recap`    | Decodes a usbmon pcap capture into a readable TTP op catalog. |

## Releases

`scripts/release.sh <version>` builds the archive and `SHA256SUMS` in the layout
the updater expects. A release without checksums is refused by the updater
rather than trusted.

## Credits & license

Protocol reference: the [`tobiifree`](https://github.com/Aetherall/tobiifree)
project. Head-pose model: the [opentrack](https://github.com/opentrack/opentrack)
project (fetched at your request, not bundled — its weights are
non-commercial-only). Licensed **GPL-3.0-only**.
