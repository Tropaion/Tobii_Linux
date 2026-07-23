# Display Area

The display area tells the tracker where the screen is in **tracker-space
millimetres**, so it can intersect the gaze ray with the screen plane and emit
normalized 2D gaze. Source of truth:
`crates/tobii-protocol/src/display.rs` (decode),
`crates/tobii-protocol/src/commands.rs` (`set_display_area*`),
`crates/tobii-config/src/setup.rs` (the geometry model),
`crates/tobii-usb/src/connection.rs::set_display_area`.

## Ops

| Op | Name | Payload | Response |
|----|------|---------|----------|
| `0x596` | get_display_area | **empty** (no `00 00` prefix) | `00 00` + TL/TR/BL point3d + trailer |
| `0x5a0` | set_display_area | `00 00` + TL/TR/BL point3d + tag `0x10100` + u32 `0x3039` | ack |

**[CONFIRMED]** — `commands.rs` (`build_get_display_area` sends an empty
payload; `set_display_area_corners_payload` is 164 bytes), `display.rs`
real-device decode test.

## Wire layout

Three corners, each a `point3d` (prolog `0x00031f41` + 3 × Q42 mm). Order on the
wire is **TL, TR, BL**; the bottom-right corner is **implied** by the device (a
plane is fully determined by three corners). The SET payload appends a trailer:
`write_tag(0x10100)` then `write_u32(0x3039)`.

```
00 00                              payload prefix
[point3d TL]   (48 bytes)          top-left    corner, tracker-space mm
[point3d TR]   (48 bytes)          top-right   corner
[point3d BL]   (48 bytes)          bottom-left corner
05 00 00 00 04 00 01 01 00         trailer tag  0x10100     (SET only)
02 00 00 00 04 00 00 30 39         trailer u32  0x3039      (SET only)
```

Total SET payload = 2 + 3×48 + 9 + 9 = **164 bytes**. The GET **response** has
the same three-corner shape (`DisplayCorners::decode` skips the 2-byte prefix and
reads three point3d). **[CONFIRMED]** —
`commands.rs::set_display_area_corners_payload_is_164_bytes`,
`display.rs::decodes_real_device_display_area_2026_07_15`.

## Corner geometry model

`DisplayCorners { tl, tr, bl : [f64; 3] }` in tracker-space, right-handed:
**+X** right, **+Y** up, **+Z** backward (away from the user). The editable
`DisplaySetup` converts to/from corners (`setup.rs`):

```
to_corners():
  tilt   = tilt_deg -> radians
  tilt_mm = height_mm * sin(tilt)      # how far the top edge leans in +Z
  dy      = height_mm * cos(tilt)      # vertical rise of the top edge
  half_w  = width_mm / 2
  bl = [cx - half_w, cy,      cz]
  tl = [cx - half_w, cy + dy, cz + tilt_mm]
  tr = [cx + half_w, cy + dy, cz + tilt_mm]
    where (cx, cy, cz) = (offset_x_mm, offset_y_mm, offset_z_mm)
```

Parameters: `width_mm` (chord width along +X), `height_mm` (tilted side-edge
length), `tilt_deg` (lean-back from vertical, + = top edge toward +Z),
`offset_x/y/z_mm` (screen bottom-edge position relative to the tracker). The top
edge is level (no roll/yaw). `from_corners` inverts this.
**[CONFIRMED]** — `setup.rs::to_corners`/`from_corners`, golden round-trip tests.

Worked real example (`display.rs`, captured from hardware after configuring a
600×335 mm screen tilted 15° with its bottom edge 10 mm above the tracker):
decodes to `tl.x ≈ -300`, `tr.x ≈ 300` (600 mm width), `bl.y ≈ 10`,
`tl.y ≈ 333.6`, `tl.z ≈ 86.7` (top edge tilted into +Z), `bl.z ≈ 0`, level top
edge. **[CONFIRMED]**.

## Reboot wipe — apply on EVERY connect

The ET5 **resets its stored display area to a ~4 mm stub every time it reboots**,
and it reboots on **every session close** (USB re-enumeration when the last
client detaches — normal behavior). **Until a valid display area is set
in-session, the device emits no eye-tracking data at all**: `validity` stays
`4`, and every eye-origin (`0x02`/`0x08`), raw eye-origin (`0x17`/`0x18`) and
trackbox (`0x03`/`0x09`) column is zero — even with a face in view. This is
upstream of gaze calibration (the raw `0x17`/`0x18` columns are zero too).

**Rule: every streaming/tracking session must call
`Connection::set_display_area(&saved_corners)` right after connect (and after
every reconnect).** The vendor stack does exactly this. `tobii display set` on
its own cannot fix it (it applies, then closes → reboot → reset again); the apply
must share the streaming session. **[CONFIRMED]** — memory
`et5-display-area-resets-on-reboot` (fix `b7528b5`), `connection.rs` doc.

## Curved monitors: arc → chord, no runtime correction

The device only accepts a **flat plane** (three corners); it cannot be told
about curvature. Handling for a curved panel:

- A curved screen is a flat sheet bent into an arc, so EDID reports the **arc**
  width. Convert to the straight **chord** with
  `chord_from_arc(arc, radius) = 2R·sin(arc / 2R)` (`setup.rs`). Feed the chord
  as `width_mm`. **[CONFIRMED]** — `setup.rs::chord_from_arc`.
- **No runtime gaze curvature correction is applied.** A per-user calibration
  already absorbs curvature (stimulus dots are drawn on the physical curved
  screen but reported as plain normalized coordinates, so the device learns the
  mapping). Correcting again at runtime double-counts it — this was implemented,
  disproved on hardware, and removed (commit `8b21938`). Do not reintroduce it.
  **[CONFIRMED]** — `docs/session-handoff-windows-vm.md`.
- **Curvature is not in EDID.** On the tested 1800R panel the radius appears
  nowhere in the 384-byte EDID; it must be entered by hand. **[CONFIRMED]**.

## Tracker reference-mark span

`EYE_TRACKER_WIDTH_MM = 184.0` — the measured span between the tracker's two
reference marks, used by the GTK alignment helper to convert normalized marker
positions to millimetres. A prior value of `376.3` was wrong by ~2× (and
exceeded the ET5's 285 mm total length). **[CONFIRMED]** — `tobii-gtk/src/align.rs`,
memory / handoff doc.
