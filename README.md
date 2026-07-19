# TobiiLinux

A native Linux runtime for the Tobii Eye Tracker 5, written in Rust. Clean-room
reimplementation of the device's USB protocol (GPL-3.0; the `tobiifree` project
is the protocol reference).

## Crates
- `tobii-protocol` — pure protocol codec + handshake state machine (no I/O).
- `tobii-usb` — libusb (rusb) transport + connection driver.
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

## Status
v1 in progress: gaze streaming works; display-area config, calibration,
head-pose, and opentrack output are upcoming. See `docs/superpowers/`.

License: GPL-3.0-only.
