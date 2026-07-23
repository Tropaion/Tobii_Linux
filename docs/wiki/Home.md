# Tobii Eye Tracker 5 — USB Protocol Reference

This wiki documents the USB protocol of the **Tobii Eye Tracker 5** (ET5, USB
`2104:0313`) as reverse-engineered by the [TobiiLinux](https://github.com/Tropaion/Tobii_Linux)
project — a clean-room Rust reimplementation. The device speaks a message
protocol Tobii calls **TTP** over USB **bulk** transfers, wrapped in a small
length-prefixed USB envelope. A host opens a session, performs a
hello → realm-auth → display-area → subscribe handshake, and then receives a
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
`crates/tobii-usb/src/` (libusb transport + connection driver), and
`crates/tobii-cli/` (the `tobii` command). File citations on each page are
relative to the repo root.

## Contents

| Page | Covers |
|------|--------|
| [[USB-Transport]] | Device id, bulk endpoints, the USB envelope, reassembly, session open/close & reboot behavior |
| [[TTP-Framing]] | The 24-byte TTP header, the three magics, seq echo, notify op==stream_id |
| [[Handshake]] | Connect sequence: hello, realm HMAC-MD5 auth, display-area apply, subscribe |
| [[Encoding]] | TLV codec, tags, Q42 fixed-point, XDS row/column framing, a worked byte-by-byte decode |
| [[Op-Catalog]] | Master table of every known op code |
| [[Streams]] | Subscription model, the known streams (gaze / eye images / state event), probing |
| [[Gaze-Stream]] | The `0x500` gaze notification: full 39-column table, present-bit vs validity gotcha |
| [[Display-Area]] | GET `0x596` / SET `0x5a0`, corner layout, geometry model, reboot wipe, curved monitors |
| [[Calibration]] | Follow-the-dot protocol: op sequence, payloads, point sets, blob persistence |
| [[Select-Eyes]] | `enabled_eye` GET `0xc62` / SET `0xc58`, wire enum, calibration-time semantics |
| [[Head-Pose]] | The open investigation: head pose is NOT in the gaze frame; evidence & next steps |
| [[Tools]] | The `tobii` CLI subcommands and the `tobii-recap` pcap decoder |
| [[Reverse-Engineering-Methodology]] | How the protocol was mapped, taking a usbmon capture, contributing findings |
