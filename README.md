# TobiiLinux

A native **Linux runtime and GUI for the Tobii Eye Tracker 5**, written in Rust —
a clean-room reimplementation of the device's USB protocol, with no Tobii
software required. It provides gaze streaming, display/eye-position setup, a live
on-screen gaze overlay, and a graphical configuration app inspired by the
original Tobii Experience UI.

> **Unofficial.** Not affiliated with, endorsed by, or supported by Tobii. The
> protocol was reverse-engineered clean-room (the [`tobiifree`](https://github.com/Aetherall/tobiifree)
> project is the reference). Use at your own risk.

## Features

- **Gaze streaming** over USB (TTP protocol): 2D gaze point, per-eye validity,
  eye position (trackbox), eye-origin, pupil.
- **GTK4 configuration app** (`tobii-gtk`): a styled hub with
  - live **eye-position** view (shaped to your monitor's aspect ratio),
  - **Set up display** — a fullscreen guided flow: drag two lines onto the
    tracker's ends to derive your screen geometry (with an optional advanced
    numeric form),
  - **Preview my gaze** — a translucent, click-through overlay dot that follows
    your gaze (Wayland `layer-shell`),
  - **Select eyes to detect** — both / left-only / right-only *(takes effect on
    calibration; see Status)*.
- **`tobii` CLI**: scriptable gaze stream, display setup, calibration, and the
  eye-selection property.
- **EDID monitor auto-detect** — pre-fills your screen's physical size from
  `/sys/class/drm`.
- Zero-config device access via a udev rule; the driver auto-applies your saved
  display configuration on every connect.

## Requirements

- **Rust** (stable, edition 2021) — e.g. via [rustup](https://rustup.rs).
- A **Tobii Eye Tracker 5** (USB `2104:0313`).
- System libraries: **GTK 4**, **`gtk4-layer-shell`** (for the gaze overlay),
  and **libusb 1.0** + **pkg-config**.
  - Arch / CachyOS: `sudo pacman -S --needed gtk4 gtk4-layer-shell libusb pkgconf`
  - Debian / Ubuntu: `sudo apt install libgtk-4-dev libgtk4-layer-shell-dev libusb-1.0-0-dev pkg-config`
- A **Wayland** session is recommended (the gaze overlay uses `layer-shell`).

## Build

```sh
cargo build --release
```

## Device access (one-time)

Install the udev rule so the tracker is usable without root:

```sh
sudo cp assets/99-tobii.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger
```

Then (re-)plug the Eye Tracker 5.

## Usage

### GUI (recommended)

```sh
cargo run --release -p tobii-gtk
```

- The **hub** shows connection status and your live eye position.
- **Set up display** → drag the two vertical lines onto the marks at the ends of
  your tracker; the screen size + horizontal offset are derived and applied.
  *Show advanced* reveals editable numeric fields (width/height/tilt/offsets +
  a live corner preview).
- **Preview my gaze** → toggles a dot that follows your gaze across the screen.
- **Select eyes to detect** → Both / Left only / Right only.

### CLI (`tobii`)

```sh
./target/release/tobii stream            # human-readable gaze
./target/release/tobii stream --json     # one JSON object per sample
./target/release/tobii setup             # interactive display setup (EDID-seeded)
./target/release/tobii display get        # read the device's current display area
./target/release/tobii display set        # re-apply the saved display area
./target/release/tobii enabled-eye [both|left|right]   # get/set eye selection
./target/release/tobii calibrate [--apply]             # calibration (experimental)
```

## Configuration

Stored under `$XDG_CONFIG_HOME/tobii-linux/` (default `~/.config/tobii-linux/`):
`config.toml` (display geometry), `calibration.bin`, and `enabled_eye`. The
display area is a planar model (width/height, tilt, offsets) validated against a
real working configuration — see `docs/superpowers/specs/`.

> **Note:** the ET5 wipes its display area every time it reboots (which it does
> on every session close), so the driver **re-applies your saved configuration
> on every connect** — without this the tracker reports no eyes.

## Architecture

A Cargo workspace of focused crates:

| Crate | Responsibility |
|---|---|
| `tobii-protocol` | Pure protocol codec, TTP framing, handshake, gaze decode (no I/O). |
| `tobii-usb`      | libusb (`rusb`) transport + connection driver. |
| `tobii-config`   | Display-setup geometry + EDID detection + TOML/blob persistence. |
| `tobii-cli`      | The `tobii` command-line tool. |
| `tobii-gtk`      | The GTK4 configuration GUI + gaze overlay. |
| `tobii-gui`      | An earlier egui prototype (superseded by `tobii-gtk`). |

## Status

Working: gaze streaming, display setup (CLI + guided GUI flow), live
eye-position view, and the gaze overlay. **Calibration** (follow-the-dot) is in
progress — "Select eyes to detect" is stored/persisted but only takes effect
once calibration lands (the tracker applies the eye selection when it
(re)calibrates). Head-pose output (for opentrack / Star Citizen) is a later
milestone. See `docs/superpowers/` for design specs and plans.

## Credits & license

Protocol reference: the [`tobiifree`](https://github.com/Aetherall/tobiifree)
project. Licensed **GPL-3.0-only**.
