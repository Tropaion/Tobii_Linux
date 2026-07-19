# TobiiLinux

A native Linux runtime for the Tobii Eye Tracker 5, written in Rust. Clean-room
reimplementation of the device's USB protocol (GPL-3.0; the `tobiifree` project
is the protocol reference).

## Crates
- `tobii-protocol` — pure protocol codec + handshake state machine (no I/O).
- `tobii-usb` — libusb (rusb) transport + connection driver.
- `tobii-config` — display-setup geometry + TOML config (no I/O beyond the config file).
- `tobii-cli` — the `tobii` command-line tool.

## Setup (once)
Install the udev rule so the device is accessible without root:

    sudo cp assets/99-tobii.rules /etc/udev/rules.d/
    sudo udevadm control --reload && sudo udevadm trigger

Then plug in (or re-plug) the Eye Tracker 5.

## Build & run

    cargo build --release
    ./target/release/tobii stream          # human-readable gaze
    ./target/release/tobii stream --json   # one JSON object per sample

### Display setup

Tell the tracker where your screen is (needed for accurate on-screen gaze):

    ./target/release/tobii setup           # interactive; writes config + applies
    ./target/release/tobii display get      # read the device's current area
    ./target/release/tobii display set      # re-apply the saved config

`tobii setup` auto-detects your monitor's size from its EDID (via
`/sys/class/drm`), pre-filling the width/height — just confirm or override.

### Gaze calibration (experimental)

    ./target/release/tobii calibrate          # run + save a calibration
    ./target/release/tobii calibrate --apply   # re-apply the saved calibration

Calibration is stored at `$XDG_CONFIG_HOME/tobii-linux/calibration.bin`.
The current `calibrate` is **headless** (no on-screen stimulus yet), so it
exercises the device protocol but does not itself produce an accurate
per-user calibration — the follow-the-dot stimulus UI is a later milestone.

Config is stored at `$XDG_CONFIG_HOME/tobii-linux/config.toml` (default
`~/.config/tobii-linux/config.toml`). Inputs are your monitor's active-area
width/height (mm), how far its bottom edge sits above the tracker, the screen's
tilt angle, and any horizontal/depth offset — a planar model validated against a
real working configuration (see `docs/superpowers/specs/`).

## Status
v1 in progress: gaze streaming and display-area setup work; head-pose and
opentrack output are upcoming. See `docs/superpowers/`.

License: GPL-3.0-only.
