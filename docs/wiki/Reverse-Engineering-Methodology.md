# Reverse-Engineering Methodology

How this protocol was mapped, how to take your own capture, and how to add
findings. The guiding principle: **the device's own USB traffic is the source of
truth.** Verify against hardware or code, never against a comment — this codebase
has already shipped one confidently-worded false constant (`376.3`) and one
plausible-but-wrong correction (runtime curvature), and both cost real debugging
time.

## Sources of truth, in order

1. **Live USB observation of the device.** A usbmon capture of real traffic is
   the highest authority. Everything tagged **[CONFIRMED]** ultimately rests on
   a capture or a hardware round-trip.
2. **`ressources/tobiifree/`** — a local Zig/TS protocol reference. Useful for
   cross-checking framing and column semantics; note it only ever subscribes to
   `0x500` and has no geometry-fitting or curvature logic.
3. **The Tobii MSI decompile** — for op *names* and enum orderings, not wire
   truth. See below. Values from here are **[CODE-VERIFIED]** at best and must be
   confirmed live before being trusted.

Confidence tags (see [[Home]]) encode which source backs each claim. When a
claim can't be sourced, it is marked **[HYPOTHESIS]** rather than smoothed over.

## Taking a usbmon capture

Capture on the **Linux host**, even when observing Tobii's Windows software: run
Windows in a QEMU/virt-manager guest with the ET5 passed through, and QEMU does
the real USB I/O through libusb on the host, so host `usbmon` sees every URB — no
Windows capture tooling needed, and the pcap lands where `tobii-recap` runs.

```bash
# virt-manager -> guest -> Add Hardware -> USB Host Device -> 2104:0313 (for VM captures)
sudo modprobe usbmon
lsusb | grep 2104:0313            # note the bus number
sudo tcpdump -i usbmon<BUS> -w tobii-<action>.pcap
```

**One action per capture file**, with a note of what was done and rough
timestamps — segmenting one long mixed capture afterwards is the tedious part.
USBPcap + Wireshark inside the guest works as a cross-check. **[CONFIRMED]** —
`docs/session-handoff-windows-vm.md`.

## Decoding a capture

```
tobii-recap tobii-<action>.pcap [--limit N] [--gaze-columns]
```

It reassembles TTP frames, prints a REQ/RSP/NOTIFY timeline with op names, and
flags unmapped ops as `?unknown` — those are your next mapping targets. See
[[Tools]]. For live exploration on real hardware use `tobii probe-streams`,
`tobii probe-stream`, and `tobii columns` ([[Streams]], [[Head-Pose]]).

## Decompiling the Tobii MSI (for op names)

The offline-installer MSI bundles managed .NET assemblies (`Tobii.Configuration.*`,
`Tobii.Tech.NETCommon.*`) plus native `TetConfig.dll` / `tobii_stream_engine.dll`.
To decompile the managed ones to C# without root (memory note
`tobii-msi-decompiler`):

1. Install .NET 8 SDK user-local:
   `curl -fsSL https://dot.net/v1/dotnet-install.sh | bash -s -- --channel 8.0 --install-dir ~/.dotnet --no-path`
2. `dotnet tool install --global ilspycmd --version 8.2.0.7535` — **pin
   8.2.0.7535**; the 9.x default ships a broken package.
3. ilspycmd 8.2 targets net6 but only net8 is installed → run with
   `DOTNET_ROLL_FORWARD=Major`. Extract GUID-suffixed assemblies with
   `7z e <msi> '<Name>.dll.<GUID>'`.

Key lesson: the **display-setup math is native** (`TetConfig.dll`), not in the
managed layer — the driver reimplements a validated planar model instead. Op
codes and enum orderings from the disasm are **[CODE-VERIFIED]**; several
(enabled_eye `0xc62`/`0xc58` inversion, the calibration op family) were found
this way and only some were later confirmed live.

## What is still open (highest-value captures)

1. **`SET_DISPLAY_AREA` (`0x5a0`) on a real curved monitor** from Tobii's own
   software — ground truth for the corner triple, tilt model, and whether the
   original compensates for curvature (we believe it does not; see
   [[Display-Area]]).
2. **A full calibration with timings** — settles the `add_point`-blocks-vs-acks
   disagreement ([[Calibration]]) and whether the original reads
   `stimulus_points_get` (`0x460`).
3. **The head-pose stream**, if any — the decisive open question ([[Head-Pose]]).
4. **Select-eyes toggling** — capture what Left/Right/Both actually sends and
   whether a standard calibration applies it ([[Select-Eyes]]).

## Contributing a finding

- Add op names to the single table in `crates/tobii-recap/src/opnames.rs`
  (seeded from `tobii_protocol::frame` constants); an op returning `None` there
  is surfaced as `UNKNOWN` and is a mapping target.
- Add op-code constants to `crates/tobii-protocol/src/frame.rs`.
- Pin any real captured payload as a regression test (as `gaze.rs`,
  `display.rs`, and `calibration.rs` already do with captured device frames).
- Tag every new protocol claim with its confidence and cite the source, so the
  next reader can tell a hardware fact from an inference.
