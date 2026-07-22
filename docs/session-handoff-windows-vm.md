# Session handoff — Windows VM protocol capture

Paste this as the opening message of the new session.

---

## Project

**TobiiLinux** — a native Linux runtime + GTK4 GUI for the Tobii Eye Tracker 5
(USB `2104:0313`), a clean-room Rust reimplementation of the ET5 USB (TTP)
protocol. GPL-3.0-only. Repo: <https://github.com/Tropaion/Tobii_Linux>,
working copy `~/Dokumente/Git/TobiiLinux`, branch `main` @ `8b21938`.
132 tests, `clippy --workspace --all-targets` clean, `fmt` clean.
`ressources/tobiifree/` is a local protocol reference (Zig/TS); it has **no**
curvature or geometry-fitting logic — it sets a plane and reads gaze.

Crates: `tobii-protocol` (pure codec), `tobii-usb` (libusb transport +
connection driver), `tobii-config` (geometry, EDID, persistence), `tobii-cli`
(the `tobii` command), `tobii-gtk` (the GUI).

## The problem this session exists to solve

Gaze accuracy is **good at screen centre and degrades toward the sides,
completely wrong at the monitor borders.** The error grows with horizontal
gaze angle.

The monitor is a **Samsung Odyssey G93SC**: 49", 5120×1440, **1800R curved**.
EDID reports 1193 × 336 mm — that 1193 is the **arc**; the straight-line
**chord** is 1171 mm; the sagitta (centre deeper than edges) is 98 mm.

### Already ruled out — do not re-litigate

- **A constant offset.** Fixed with the in-app fine-tune helper: `offset_y_mm`
  went 10 → 43.7 (an independent ruler estimate predicted ~40). Centre is now
  accurate, which is exactly what a translation fix should achieve.
- **A runtime curvature correction.** Implemented, then **disproved on
  hardware and removed** (commit `8b21938`). It made the sides *worse*.
  Mechanism: a per-user calibration **already absorbs curvature**, because the
  stimulus dots are drawn at normalized positions on the physical curved screen
  but reported to the device as plain normalized coordinates, which it converts
  against its flat plane. The device therefore learns to emit those coordinates
  for those physical points. Correcting again at runtime double-counts it —
  zero error at centre, growing outward. **Do not reintroduce this.**
- **Width being the arc rather than the chord.** `width_mm` is now 1171.3.

### Remaining suspects, best first

1. **`offset_z_mm = 0`, never measured.** A depth error produces error
   ∝ `tan θ` — 0 mm at centre, ~6 mm at 25% out, ~26 mm at the far edge per
   30 mm of depth error. That is precisely the observed shape.
2. **`tilt_deg = 20`, never measured** (it is a default, not a measurement).
3. **The calibration itself**, computed under earlier wrong geometry.
4. **`DisplaySetup::to_corners()` being wrong in a way that only manifests at
   angle** — which is what the capture below settles definitively.

### Current config (`~/.config/tobii-linux/config.toml`)

```toml
width_mm            = 1171.283987269172   # chord, converted from the 1193 arc
height_mm           = 336
tilt_deg            = 20                  # UNMEASURED default
offset_x_mm         = 5.830292961134139   # from fine-tune
offset_y_mm         = 43.748652156549554  # from fine-tune
offset_z_mm         = 0                   # UNMEASURED
curvature_radius_mm = 1800
```

## Do this FIRST, on Linux, no VM needed

**Map the head-pose stream with our own tools.** The device's 6DOF head pose is
almost certainly already on our wire: the gaze notification (op `0x500`) carries
**39 columns**, of which the decoder models ~18 and silently discards the rest.
`column_inventory` (`tobii-protocol/src/gaze.rs`) dumps them all; in a captured
**no-eyes** frame the four unmapped **point3d** columns **`0x22`, `0x24`,
`0x25`, `0x27`** read `[0,0,0]` and are the strongest position/rotation-vector
candidates (also unmapped: point2d `0x19`/`0x1a`/`0x20`, fixed `0x29`/`0x2b`,
plus many u32 flags).

Run `tobii columns` (streams the inventory ~2/s, flags unmapped point3d
columns) with a head present and MOVING ONE AXIS AT A TIME — translate
left/right, up/down, forward/back, then yaw, pitch, roll. The columns that go
non-zero and track each motion are the pose; redirect to a file per axis
(`tobii columns > yaw.log`) for a record. No VM, no original software.

`crates/tobii-headpose` already derives a 5-DOF fallback (position + yaw + roll,
pitch stubbed at 0) from the two eye origins; the real 6DOF stream replaces that
derivation once these columns are identified — and gives us the pitch two eye
points can't.

## What the VM is actually for — observing the ORIGINAL software

The VM only earns its keep where we need to see what *Tobii's own software*
sends or computes, which we cannot derive from our side. In priority order:

1. **Display setup on this exact monitor** → the `SET_DISPLAY_AREA` (op
   `0x5a0`) payload. Ground truth for the corner triple Tobii computes for this
   screen and tracker placement — directly validates or corrects `to_corners()`,
   the tilt model, and the meaning of `offset_y`/`offset_z`, and answers whether
   the original compensates for curvature at all. **Highest value — this alone
   may solve the remaining side-error.**
2. **A full calibration** — the real op sequence with timings. Confirms whether
   `add_calibration_point` really acks immediately, what per-point dwell the
   original uses, and whether it reads `stimulus_points_get` (`0x460`) for
   per-point bias.
3. **Select-eyes** — toggle Left / Right / Both and capture what it sends.
   Resolves a genuinely open question (see protocol facts below).

Also worth dumping: `HKCU\Software\Tobii\...\EyeXConfig\UserProfiles\` — the
original stores `TrackedEyes` and geometry host-side in the registry.

## Recommended capture setup

Capture on the **host**, not in the guest: QEMU does the real USB I/O through
libusb on the Linux host, so `usbmon` sees every URB, no Windows capture tooling
is needed, and the pcap lands where the decoder already lives.

```bash
# virt-manager → Windows 10/11 guest → Add Hardware → USB Host Device → 2104:0313
sudo modprobe usbmon
lsusb | grep 2104:0313            # note the bus number
sudo tcpdump -i usbmon<BUS> -w tobii-<action>.pcap
```

USBPcap + Wireshark inside the guest works as a cross-check if the host capture
looks wrong.

**One action per capture file**, with a note of what was done and rough
timestamps. Segmenting one long mixed capture is the tedious part; separate
files make it trivial.

## Protocol facts already established — do not rediscover

- **Display area**: GET `0x596`, SET `0x5a0`. Payload is `00 00` + TL/TR/BL
  point3d + trailer tag `0x10100` + u32 `0x3039`. Three corners = a **plane**;
  the device cannot be told about curvature.
- **The ET5 wipes its display area on every reboot** (and it reboots on every
  session close). The driver must re-apply the saved area on every connect or
  the device reports no eyes at all (`validity = 4`).
- **Calibration ops**: start `0x3f2`, stop `0x3fc`, clear `0x424` (destructive),
  add_point `0x408` (`00 00` + Q42 x + Q42 y + u32 eye, eye 0 = both), compute+
  apply `0x42f`, retrieve `0x44c`, apply `0x456`, discard_point `0x438`,
  stimulus_points_get `0x460`. Order: `start → clear → points → compute → stop
  → retrieve`; compute comes **before** stop.
- **`add_calibration_point` acks almost immediately** — it does *not* block
  while the device samples, contrary to what the decompiled managed layer
  implies. The fixation dwell must be enforced host-side (~1.2 s per point).
- **`enabled_eye`**: GET `0xc62`, SET `0xc58` (SET is *lower* than GET). Wire
  enum `1 = LEFT, 2 = RIGHT, 3 = BOTH`. A standalone SET is acked and persists
  across reboot, but does **not** change live detection — both eyes keep
  reporting `validity = 0`. Believed to be a calibration-time setting; whether
  a standard calibration is enough is **unresolved** (capture item 3).
- **Request windows must be time-capped, not iteration-capped.** Gaze
  notifications are routed from inside the same read loop, so a fixed iteration
  budget gets starved by normal gaze traffic and every calibration point fails.
- **`EYE_TRACKER_WIDTH_MM = 184.0`** — the span between the tracker's reference
  marks, *measured on the physical device*. A previous 376.3 was wrong by 2.04×
  (and exceeded the ET5's 285 mm total length) while carrying a doc comment
  falsely claiming it had been verified against the decompiled source.
- **Curvature is not in EDID.** Verified on this monitor: the DisplayID
  extension carries only a Type II Timing block, and the value 1800 appears
  nowhere in the 384-byte EDID. It must be entered by hand.
- Eye-origin columns are **present but all-zero** when `validity = 4`, so eye
  presence must be gated on validity, never on the present bit alone.

## Working style that has paid off here

Verify claims against the device or the code rather than against comments —
this codebase has already shipped one confidently-worded false comment
(`376.3`) and one plausible-but-wrong correction (curvature), and both cost
real debugging time. When a reviewer and a refuter disagree about a race or a
sign, the refuter has been the one that was wrong. Prefer a hardware capture
or a closed-loop test over a derivation.
