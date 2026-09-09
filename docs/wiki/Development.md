# Development

How to build, test, and change this project. Architecture background:
[[Architecture]] · [[Runtime-View]] · [[Architecture-Decisions]] ·
[[Quality-and-Risks]].

---

## Getting a build

```sh
scripts/build.sh            # checks dependencies first, then builds release
scripts/build.sh --check    # only check dependencies
scripts/build.sh --lean     # CLI only, no neural backend (992 KB vs 32 MB)
scripts/build.sh --install  # + install to ~/.local/bin and the app menu
scripts/build.sh --udev     # + install the device rule
```

The dependency check exists because a missing GTK development package does not
fail with "GTK is missing" — it fails several minutes in with hundreds of linker
errors about undefined symbols.

**Use rustup.** `rust-toolchain.toml` pins the compiler and its components, and
rustup honours it automatically, so your `clippy` and `rustfmt` are the ones CI
runs. Distribution Rust ignores the pin and may be too old to build this at all.

---

## The three checks

These are exactly what CI runs, and they pass on `main`:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

`-D warnings` on a floating toolchain would be a time bomb — a new clippy turns
every open pull request red with no code change. That is why the toolchain is
pinned; see [[Architecture-Decisions]] §18.

---

## Testing

### What exists

| Crate | Tests | Shape |
|---|---|---|
| `tobii-protocol` | 74 | Inline unit tests over captured real frames |
| `tobii-usb` | 47 | 41 unit + 6 replay (see below) |
| `tobii-config` | 53 | Unit; SHA-256 cross-checked against coreutils |
| `tobii-headpose` | 93 (+7 ignored) | The ignored ones need the 13 MB model |
| `tobii-update` | 59 | 51 unit + 8 install end-to-end |
| `tobii-cli` | 9 | Argument parsing and text helpers |
| `tobii-gtk` | 191 | Inline; pure logic split out from widget code |
| `tobii-recap` | 32 | 29 unit + 3 integration |
| **Total** | **558** | |

There are almost no integration-test directories: the convention is inline
`#[cfg(test)]` modules next to the code, with pure logic deliberately factored
out of widget and I/O code so it can be tested at all.

### Testing the protocol without a tracker

Most of what "needs an ET5" does not, once a session has been recorded.

```sh
tobii record                              # → crates/tobii-usb/tests/captures/session.tobiicap
cargo test -p tobii-usb --test replay
```

`tobii record` wraps the USB transport and writes every frame, both directions,
to a line-oriented hex file with headers. The replay tests drive the driver
against exactly the replies the device gave and assert **byte-for-byte on what
the driver sends** — which is where the reverse engineering lives.

It catches what it exists to catch: changing `OP_SUBSCRIBE` from `0x4c4` to
`0x4c5` fails with a hex diff naming the frame and the byte.

**A capture is a photograph, not a specification.** It proves the code still
does what it did when the recording was taken — not that the device still does.
Re-record after a firmware update. The header carries when it was taken and the
display geometry used, so the byte comparison reproduces on any machine.

Its limits are worth knowing before you trust a green run: the fixture covers 8
host frames and 48 device frames — handshake, subscribe, display area, enabled
eye, gaze, unsubscribe. No calibration, no camera stream, no error paths. And
because `RecordTransport` wraps the *outer* transport, replaying exercises none
of `UsbTransport` itself.

### What cannot be tested without hardware

The accuracy figure, whether the tracker physically detects eyes, whether the
illuminators actually go dark, and everything below the `Transport` seam. Those
were measured by hand — [[Quality-and-Risks]] §10 says which and with what
numbers. **If you change the protocol layer or the device thread, say in your
pull request what you tested against a real ET5.**

### Useful measurement techniques

Several claims in this project were settled by observation rather than argument,
and the techniques are reusable:

- **Is the tracker actually on?** `ls -l /proc/<pid>/fd | grep bus/usb`. A GSK
  renderer fd (`/dev/dri/renderD128`) appears only when a window is really
  mapped — useful for proving a "windowless" mode is windowless.
- **Is the device free?** Run `tobii stream` alongside. Only one process can
  claim the interface.
- **Does a test actually catch anything?** Break the thing it guards and watch it
  fail. Several tests in this repo were found vacuous exactly this way.
- **Protocol archaeology:** `tobii-recap` decodes a usbmon pcap — including
  captures of Tobii's own Windows software in a passed-through VM — into a
  directional op catalog. See [[Reverse-Engineering-Methodology]].

---

## Conventions

**Comments explain *why*, and cite the measurement.** This codebase is unusually
dense in them and that is deliberate: the protocol is undocumented, so the
reason a constant has its value is often the only record that exists. Do not
strip them for brevity.

**Mark your confidence.** [CONFIRMED] / [CODE-VERIFIED] / [HYPOTHESIS] on
protocol claims. An unmarked claim reads as unproven.

**Do not assert what you have not checked.** The project's worst bugs and its
worst documentation have the same cause. If you cannot measure something, say
so in the same sentence.

**Hand-rolled is often deliberate.** SHA-256, JSON, MD5 and the curl/wget fetch
are all local, to keep a driver installable from source. MD5 in particular is
*dictated by the device*, not a security choice — swapping it out breaks the
handshake.

---

## Traps

Collected from the source and from bugs that actually happened.

**Protocol**
- The USB envelope length field is **asymmetric**: outbound it excludes the
  8-byte envelope, inbound it includes it.
- Every payload begins with a **2-byte prefix** the caller must skip.
- **Three TLV framings** exist. Do not assume the one you learned first.
- `EnabledEye`'s Rust order (Both, Left, Right) is **not** the wire order
  (1=Left, 2=Right, 3=Both) — always go through `to_wire`/`from_wire`.
- For `enabled_eye`, **SET (`0xc58`) is numerically lower than GET (`0xc62`)**,
  unlike every other mapped pair.
- There is **no "0 means both"** for a calibration point's eye argument —
  passing 0 selects no eye, the device acks the point and drops it. That is the
  bug that made every calibration before 2026-08-15 a silent no-op.
- A **present bit is not validity.** Gate on both.

**Device**
- The ET5 **wipes display area and calibration on every reboot**, and reboots on
  every session close. Re-applying on connect is not an optimisation target.
- Without a valid display area the device reports **no eyes at all** — which
  presents as broken hardware, not as a config problem.

**GUI**
- `recv` returning `None` is a timeout, not an error and not a disconnect.
- Never touch a widget off the GTK main thread.
- A `DemandGuard` leaked anywhere means the illuminators never go out — in
  background mode, until logout.
- `GApplication` emits `activate` on launch whenever argv has nothing left to
  handle. `--background` is filtered out of argv, so it *does* fire.

---

## Releasing

Bump `[workspace.package] version`, commit, then `git tag v0.2.0 && git push
origin v0.2.0`. CI builds in a Debian 13 container, enforces the glibc floor,
verifies the checksums and publishes a **draft** — the release notes are the
changelog every user's updater shows them, so a human sees them first.

Never cut a release from a developer machine. The build host *is* the
compatibility floor; see [[Runtime-View]] §7.
