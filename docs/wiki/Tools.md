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
| `tobii debug [--file PATH]` | Print everything an issue report needs and nothing that identifies you, **including the tail of the log**: versions, the distro/kernel/glibc, session type, library versions, whether the tracker is on the bus (read from sysfs — it never claims the device, so it works while the hub is running), whether the udev rule is installed, and what is configured. **No calibration data**; the monitor id is hashed because it is derived from the EDID serial; the home path, username and hostname are left out. Small enough to paste, which matters: GitHub issue forms can require a pasted field but have no file-upload field at all. |
| `tobii record [--calibration] [FILE] [FRAMES]` | Record a real session — handshake, display-area apply, eye selection, gaze subscribe, N gaze frames, unsubscribe — to a replayable capture. Writes `crates/tobii-usb/tests/captures/session.tobiicap` by default. The capture is line-oriented hex with a header carrying when it was taken and the display geometry used, so `cargo test -p tobii-usb --test replay` reproduces the same frames on a machine with no tracker. Re-record after a firmware update: a capture is a photograph of one session, not a specification. |
| `tobii update [--install]` | Check GitHub for a newer release and print its changelog. `--install` downloads the archive for this machine, checks it against the release's `SHA256SUMS`, runs the new binaries once to confirm they work here, and then replaces the installed ones — rolling every one of them back if any step fails. The checksums are published in the same release as the archive, so they catch a corrupted download, not a hostile one; see [Updates](../../README.md#updates). |
| `tobii stream [--json] [--eyes]` | Connect and print decoded gaze samples (timestamp, `gaze_point_2d`, validities). `--eyes` also prints trackbox + eye-origin geometry; `--json` emits one JSON object per frame. |
| `tobii headpose [--udp ADDR] [--rate HZ] [--model auto\|off\|FILE] [--check]` | Stream head pose to opentrack over UDP (default `127.0.0.1:4242`, 60 Hz). With a model installed this is **6 DOF** — position from the eye origins, rotation from the neural model; without one it is the 5-DOF geometry and **pitch is 0**. `--check` prints the model's yaw/roll beside the geometry's, which is the experiment that settles the sign conventions. See [[Head-Pose]]. |
| `tobii headpose --model-status` | Report which head-pose models are installed in `~/.config/tobii-linux/models` and whether their sha256 matches the pinned one. |
| `tobii headpose --fetch-model [--agree]` | Show the model's licence terms, ask for explicit consent, then download + verify + install it. `--agree` answers the prompt (for scripts); nothing is fetched without one or the other. Same flow as the GUI's **Head tracking** section. |
| `tobii headpose --install-model <FILE>` | Install a model you already have. Refused unless its sha256 is the pinned one — a different file is worse than none, because its output looks plausible. |
| `tobii columns` | Diagnostic: stream the FULL column inventory of each gaze frame (~2 Hz), including columns `stream`/`headpose` discard. Move your head one axis at a time to see which columns track motion. Flags unmapped columns. Needs a valid display area. |
| `tobii probe-streams [START] [END]` | Hunt for undiscovered streams: baseline gaze-only notify ops, subscribe across `START..=END` (default `0x501..=0x520`), report which notify ops newly appear. See [[Streams]]. |
| `tobii probe-stream <ID_hex> [SECS]` | Deep-dive on ONE stream: subscribe, read `SECS` (default 5), report rate, payload size range, whether the payload changes frame-to-frame (live vs static), and a hex preview. |
| `tobii streams` | Ask the device for its OWN stream catalog (`0x4b0`) and print every stream id it advertises, with names. See [[Streams]]. |
| `tobii log [SECS]` | Subscribe to the device's ASCII log stream (`0x1772`) and print it for `SECS` (default 30). The firmware's own view of what it is doing. |
| `tobii dump-stream <ID_hex> [COUNT]` | Dump `COUNT` (default 3) raw payloads of one stream to files in a fresh private directory. |
| `tobii camera [ID] [COUNT]` | Capture camera frames from one image stream (default `0x501`) and write them as PGM. |
| `tobii camera both [SECS]` | Capture `0x501` and `0x50e` together for `SECS` (default 6), pair them by timestamp, and report whether the two cameras differ. On this ET5 they are byte-identical, on a face and on an empty scene alike — `0x50e` is the **same** camera as `0x501`, not a second view. |
| `tobii cal-blob` | Retrieve the stored calibration (`0x44c`) and report its size, the 2-byte status prefix, and a preview — the quickest way to tell a real blob from a stub. See [[Calibration]]. |
| `tobii cal-points` | Ask the device for its own stimulus point set (`0x460`) and decode the reply. |
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
