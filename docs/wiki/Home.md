# Tobii Eye Tracker 5 — USB Protocol Reference

This wiki documents the USB protocol of the **Tobii Eye Tracker 5** (ET5, USB
`2104:0313`) as reverse-engineered by the [TobiiLinux](https://github.com/Tropaion/Tobii_Linux)
project — a clean-room Rust reimplementation. The device speaks a message
protocol Tobii calls **TTP** over USB **bulk** transfers, wrapped in a small
length-prefixed USB envelope. A host opens a session, performs a
hello → query-realm → open-realm (HMAC-MD5 on the auth path) → subscribe
handshake — after which the display area, eye selection and calibration blob are
re-applied as ordinary requests, because the device wipes them on every reboot —
and then receives a
continuous ~33 Hz **gaze notification** stream (op `0x500`) carrying 39
tab-separated ("XDS") data columns encoded as a TLV byte stream with Q42
fixed-point numbers. Configuration (display area, calibration, selected eyes) is
done with request/response ops on the same session.

This is a **reverse-engineering reference**. A wrong "fact" here can waste days,
so every non-obvious protocol claim is tagged with a confidence level, and each
page cites the source file that backs it.

## Confidence legend

| Tag | Meaning |
|-----|---------|
| **[CONFIRMED]** | Verified live against physical hardware, or a value present in the code that also has a captured-frame / round-trip test. |
| **[CODE-VERIFIED]** | Found in the native disassembly / decompiled Tobii sources (or mirrored in this codebase) but not individually live-tested. |
| **[HYPOTHESIS]** | Inferred or partially observed; unproven. Treat as a lead, not a fact. |

Source-of-truth code lives under `crates/tobii-protocol/src/` (pure codec),
`crates/tobii-usb/src/` (libusb transport + connection driver),
`crates/tobii-headpose/src/` (head pose, geometric and neural) and
`crates/tobii-cli/` (the `tobii` command). File citations on each page are
relative to the repo root.

## Contents

### Architecture (arc42)

How the software is put together, for anyone changing it.

| Page | Covers |
|------|--------|
| [[Architecture]] | §1–5: goals, constraints, context, solution strategy, the eleven crates and the three threads |
| [[Runtime-View]] | §6–7: six scenarios traced through the real code, and how a release is built and delivered |
| [[Architecture-Decisions]] | §8–9: cross-cutting concepts, and the ~24 decisions that are expensive to reverse |
| [[Quality-and-Risks]] | §10–12: measured numbers with their sources, known defects, untested surfaces, glossary |
| [[Development]] | Build, the three CI checks, the test suite, testing the protocol with no tracker, conventions and traps |

### The protocol

The reverse-engineered ET5 USB protocol.

| Page | Covers |
|------|--------|
| [[USB-Transport]] | Device id, bulk endpoints, the USB envelope, reassembly, session open/close & reboot behavior |
| [[TTP-Framing]] | The 24-byte TTP header, the three magics, seq echo, notify op==stream_id |
| [[Handshake]] | Connect sequence: hello, query-realm, open-realm (HMAC-MD5 auth), subscribe. The display area is applied *after* it, not in it |
| [[Encoding]] | TLV codec, tags, Q42 fixed-point, XDS row/column framing, a worked byte-by-byte decode |
| [[Op-Catalog]] | Master table of every known op code |
| [[Streams]] | Subscription model, the known streams (gaze / eye images / state event), probing |
| [[Gaze-Stream]] | The `0x500` gaze notification: full 39-column table, present-bit vs validity gotcha |
| [[Display-Area]] | GET `0x596` / SET `0x5a0`, corner layout, geometry model, reboot wipe, curved monitors |
| [[Calibration]] | Follow-the-dot protocol: op sequence, payloads, point sets, blob persistence |
| [[Select-Eyes]] | `enabled_eye` GET `0xc62` / SET `0xc58`, wire enum, calibration-time semantics |
| [[Head-Pose]] | Head pose is NOT in the gaze frame — how it is derived host-side instead: the 5-DOF eye-origin fallback and the 6-DOF neural path |
| [[Game-Output]] | Getting tracking into games: the virtual joystick, opentrack, TrackIR/FreeTrack over Wine, and why the TrackIR signature is not answered |
| [[Tools]] | The `tobii` CLI subcommands and the `tobii-recap` pcap decoder |
| [[Reverse-Engineering-Methodology]] | How the protocol was mapped, taking a usbmon capture, contributing findings |
