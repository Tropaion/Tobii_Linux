# Tools

Two binaries ship with the project: the `tobii` CLI (`crates/tobii-cli`) for
talking to live hardware, and `tobii-recap` (`crates/tobii-recap`) for decoding
a captured USB trace offline.

## `tobii` CLI

Source: `crates/tobii-cli/src/main.rs`. All device-touching commands open the
ET5, run the handshake, and — where gaze data is needed — re-apply the saved
display area first (the device wipes it on reboot; see [[Display-Area]]).

| Subcommand | Purpose |
|-----------|---------|
| `tobii stream [--json] [--eyes]` | Connect and print decoded gaze samples (timestamp, `gaze_point_2d`, validities). `--eyes` also prints trackbox + eye-origin geometry; `--json` emits one JSON object per frame. |
| `tobii headpose [--udp ADDR] [--rate HZ]` | Derive a 5-DOF head pose from the two eye origins and stream it to opentrack over UDP (default `127.0.0.1:4242`, 60 Hz). **Pitch is always 0.** See [[Head-Pose]]. |
| `tobii columns` | Diagnostic: stream the FULL column inventory of each gaze frame (~2 Hz), including columns `stream`/`headpose` discard. Move your head one axis at a time to see which columns track motion. Flags unmapped columns. Needs a valid display area. |
| `tobii probe-streams [START] [END]` | Hunt for undiscovered streams: baseline gaze-only notify ops, subscribe across `START..=END` (default `0x501..=0x520`), report which notify ops newly appear. See [[Streams]]. |
| `tobii probe-stream <ID_hex> [SECS]` | Deep-dive on ONE stream: subscribe, read `SECS` (default 5), report rate, payload size range, whether the payload changes frame-to-frame (live vs static), and a hex preview. |
| `tobii setup` | Interactive display-geometry wizard: detect the monitor, prompt for width/height/tilt/offsets/curvature, compute corners, save config, and apply to the device. |
| `tobii display get` | Read the device's current display area (`0x596`), print the three corners and the derived setup. |
| `tobii display set` | Apply the saved config's corners to the device (`0x5a0`). |
| `tobii calibrate [--apply]` | Run a **headless** 5-point calibration (no dots drawn — validates the protocol, not accuracy), compute+apply, retrieve and save the blob. `--apply` re-applies the saved blob. For accurate calibration use the GTK follow-the-dot flow. |
| `tobii cal-probe` | Non-destructive calibration-session probe: `start` then `stop` only (no `clear`, no `compute`), to check the device still accepts these ops. |
| `tobii enabled-eye [both\|left\|right]` | Get (and optionally set) "Select eyes to detect" (`0xc62`/`0xc58`). See [[Select-Eyes]]. |

The GUI (`crates/tobii-gtk`) provides the follow-the-dot calibration flow
(Quick-5 / Full-9), the display-setup UI, and an eye-position/gaze overlay.

## `tobii-recap` — pcap → TTP op catalog

Source: `crates/tobii-recap/`. Decodes a **usbmon pcap** capture of ET5 traffic
into a human-readable TTP timeline and op catalog — the offline counterpart to
the live probes, and the tool used to analyze a Windows-VM capture of Tobii's own
software.

```
tobii-recap <capture.pcap> [--limit N] [--gaze-columns]
  --limit N          cap the number of timeline lines printed
  --gaze-columns     dump the column inventory of each gaze notify (0x500)
```

It reassembles TTP frames from the usbmon URBs, labels each with its magic
(REQ/RSP/NOTIFY), direction, and op name (via the shared `opnames` table —
`op_label` prints `?unknown` for unmapped ops, which are exactly the
reverse-engineering targets), and can expand gaze frames into their column
inventory using the same `tobii-protocol::gaze::column_inventory` the live tools
use. **[CONFIRMED]** — `tobii-recap/src/main.rs`, `opnames.rs`. Modules:
`pcap.rs`/`usbmon.rs` (capture parsing), `decode.rs` (framing), `catalog.rs`
(op catalog), `out_parser.rs`, `opnames.rs`.

See [[Reverse-Engineering-Methodology]] for how to produce the capture.
