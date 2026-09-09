# TobiiLinux

A native **Linux runtime and GUI for the Tobii Eye Tracker 5**, written in Rust —
a clean-room reimplementation of the device's USB protocol, with no Tobii
software required. Gaze streaming, guided display setup, follow-the-dot
calibration, 6-DOF head tracking for games, and a graphical configuration app
inspired by the original Tobii Experience UI.

> **Unofficial.** Not affiliated with, endorsed by, or supported by Tobii. The
> protocol was reverse-engineered clean-room (the [`tobiifree`](https://github.com/Aetherall/tobiifree)
> project is the reference). Use at your own risk.

## What it does

### Eye tracking

- **Gaze streaming** over USB (TTP protocol): 2D gaze point, per-eye validity,
  eye position in the trackbox, eye origins in millimetres, pupil size.
- **Calibration** — the follow-the-dot flow: 7 points shown in three groups,
  computed and applied on the device after each group, then persisted and
  re-applied on every connect.
  Measured accuracy on a 49" 32:9 panel: **12.1 mm** mean error within the
  device's usable ±28°, or about 0.30° at 750 mm.
- **Display setup** — a fullscreen guided flow: drag two lines onto the marks at
  the ends of the tracker and the screen geometry is derived, seeded from your
  monitor's EDID. Curved panels are handled (arc→chord width plus a gaze
  correction; the device itself can only be told about a flat plane).
- **Select eyes to detect** — both, left only, or right only.

### Head tracking

- **5 DOF with no extra download**: position in millimetres plus yaw and roll,
  derived from the two eye origins.
- **6 DOF with an optional model**: adds **pitch**, which two eye origins cannot
  express — nodding rotates the head about the line through them and leaves both
  origins where they were. A neural model reads it from the tracker's own
  infrared camera at ~12 ms a frame. The model is **not shipped**: its weights
  are non-commercial-only, so the program shows the terms and fetches it only if
  you agree.
- **Output to games** over opentrack's UDP protocol.
- **Pitch zero calibration** — the model reports pitch in its own frame, offset
  by how your tracker is mounted. `tobii headpose --calibrate-pitch` measures it
  once.

### The app

- **GTK4 hub** (`tobii-gtk`): a live instrument panel — the trackbox with a
  graticule and the tolerance region drawn, your eyes as dots, the head drawn
  around them turning as you turn, a readout of position, distance, yaw, pitch
  and roll, and the infrared sensor view. Responsive: the columns stack on a
  narrow window.
- **Preview my gaze** — a translucent click-through dot that follows your gaze
  (Wayland `layer-shell`).
- **Accuracy diagnostic** (`tobii-gtk --accuracy`) — a 39-target sweep reporting
  error per target, per angle band and per eye, with a diagnosis.
- **The tracker runs only when something needs it.** Its infrared illuminators
  are on for exactly as long as a USB session is open, so the session is opened
  only while a consumer wants data — the hub *while its window has focus*,
  the gaze overlay while it is shown, a calibration or setup flow while it runs
  — and closed three seconds after the last one lets go. A hub left open in the
  background leaves the LEDs dark, and leaves the device free for
  `tobii headpose` to claim for a game.
- **Start menu entry and optional autostart.** `scripts/build.sh --install`
  adds a desktop entry; *Start when I log in* runs `tobii-gtk --background`,
  which keeps the tracker's configuration applied without opening a window and
  without turning the tracker on. Launching the app again raises the hub rather
  than starting a second copy.
- **Updates** — the hub checks for a new release at launch and can show the
  changelog and install it. Nothing is downloaded without a click. The check can
  be switched off. See [Updates](#updates) for what installing one trusts.

## Installation

There are no distribution packages yet, so this is built from source. The whole
process is four commands, and `scripts/build.sh` checks the dependencies before
it starts so a missing package is named in the first second rather than as a
wall of linker errors five minutes in.

<details open>
<summary><b>Arch, CachyOS, Manjaro, EndeavourOS</b></summary>

```sh
sudo pacman -S --needed base-devel git rustup gtk4 gtk4-layer-shell libusb pkgconf
rustup default stable            # skip if you already have a Rust toolchain
git clone https://github.com/Tropaion/Tobii_Linux.git && cd Tobii_Linux
scripts/build.sh --install --udev
```
</details>

<details>
<summary><b>Debian 13+, Ubuntu 24.10+, Pop!_OS, Linux Mint</b></summary>

```sh
sudo apt install build-essential git curl pkg-config \
     libgtk-4-dev libgtk4-layer-shell-dev libusb-1.0-0-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # if you have no Rust
git clone https://github.com/Tropaion/Tobii_Linux.git && cd Tobii_Linux
scripts/build.sh --install --udev
```

`libgtk4-layer-shell-dev` is only in Debian 13 (trixie) and Ubuntu 24.10 or
newer. On an older release, either build it from
[the upstream project](https://github.com/wmww/gtk4-layer-shell), or skip the
GUI and build just the command-line tool:

```sh
sudo apt install build-essential git pkg-config libusb-1.0-0-dev
scripts/build.sh --lean --install --udev
```

Ubuntu's `rustc` is often too old; the rustup line above is the reliable route.
</details>

<details>
<summary><b>Fedora, RHEL, Rocky, Alma</b></summary>

```sh
sudo dnf install gcc git rust cargo pkgconf-pkg-config \
     gtk4-devel gtk4-layer-shell-devel libusb1-devel
git clone https://github.com/Tropaion/Tobii_Linux.git && cd Tobii_Linux
scripts/build.sh --install --udev
```

On RHEL and its rebuilds, `gtk4-layer-shell-devel` comes from
[EPEL](https://docs.fedoraproject.org/en-US/epel/); without it, use
`scripts/build.sh --lean` for the command-line tool only.
</details>

<details>
<summary><b>openSUSE Tumbleweed / Leap</b></summary>

```sh
sudo zypper install gcc git rust cargo pkg-config \
     gtk4-devel gtk4-layer-shell-devel libusb-1_0-devel
git clone https://github.com/Tropaion/Tobii_Linux.git && cd Tobii_Linux
scripts/build.sh --install --udev
```
</details>

<details>
<summary><b>NixOS</b></summary>

```sh
nix-shell -p gcc pkg-config gtk4 gtk4-layer-shell libusb1 cargo rustc
git clone https://github.com/Tropaion/Tobii_Linux.git && cd Tobii_Linux
cargo build --release
```

The udev rule belongs in your configuration rather than being copied into
`/etc`, since NixOS manages that directory:

```nix
services.udev.extraRules = builtins.readFile ./assets/99-tobii.rules;
```
</details>

<details>
<summary><b>Any other distribution</b></summary>

You need a C compiler, `pkg-config`, a stable Rust toolchain
([rustup](https://rustup.rs)), and the **development** packages for **GTK 4**,
**gtk4-layer-shell** and **libusb 1.0**. Then:

```sh
scripts/build.sh --check     # names anything still missing
scripts/build.sh --install --udev
```

The GUI is the only thing that needs GTK; `scripts/build.sh --lean` builds the
command-line tool with neither GTK package.
</details>

`--install` puts `tobii` and `tobii-gtk` in `~/.local/bin`; pass a directory to
choose another (`scripts/build.sh --install /usr/local/bin`). `--udev` installs
the device rule described below, which needs `sudo`. Leave both off to just
build into `target/release/`.

`--install` also adds **Tobii Eye Tracker** to your application menu.

**After installing, re-plug the Eye Tracker 5** so the udev rule takes effect,
then run `tobii-gtk` or pick it from the menu.

## Requirements

- **Rust** (stable, edition 2021) — e.g. via [rustup](https://rustup.rs).
- A **Tobii Eye Tracker 5** (USB `2104:0313`).
- System libraries: **GTK 4**, **`gtk4-layer-shell`** (for the gaze overlay),
  and **libusb 1.0** + **pkg-config**. The GUI needs all of them; the CLI needs
  only libusb.
- `curl` or `wget`, and `tar`, for fetching the head-pose model and updates.
- A **Wayland** session is recommended (the gaze overlay uses `layer-shell`).

## Build

`scripts/build.sh` is the Installation section's build step on its own, and
takes the same options:

```sh
scripts/build.sh                    # check dependencies, build everything
scripts/build.sh --check            # only check dependencies, build nothing
scripts/build.sh --lean             # CLI only, without the neural backend
scripts/build.sh --install [DIR]    # build, then install (default ~/.local/bin)
scripts/build.sh --udev             # also install the udev rule
```

Plain `cargo build --release` works too. The neural head-pose backend is on by
default and brings a sizeable dependency tree (`tract`, ~110 crates); the
**CLI** can be built without it — 5 DOF head tracking, everything else
unchanged, and a 1 MB binary instead of 32 MB:

```sh
cargo build --release -p tobii-cli --no-default-features
```

The GUI currently always includes it; `tobii-gtk` has no matching feature gate
yet.

## Device access (one-time)

Already done if you ran `scripts/build.sh --udev`. By hand:

```sh
sudo cp assets/99-tobii.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger
```

Then (re-)plug the Eye Tracker 5. Without this the device is only reachable as
root, and the tracker reports no eyes at all.

## Usage

### GUI

```sh
cargo run --release -p tobii-gtk
```

Or pick **Tobii Eye Tracker** from your application menu, once
`scripts/build.sh --install` has added it.

The hub shows connection status, the live instrument panel, and the settings:
improve calibration, head tracking, preview my gaze, select eyes, change screen,
start at login, check for updates.

```sh
tobii-gtk --background   # no window; keeps the tracker configured (see below)
tobii-gtk --version
```

### Head tracking for games

```sh
./target/release/tobii headpose --fetch-model     # optional: adds pitch
./target/release/tobii headpose --calibrate-pitch # once, sitting normally
./target/release/tobii headpose                   # stream to opentrack on :4242
```

Point opentrack's **UDP over network** input at `127.0.0.1:4242`. Without the
model this still works — you get position, yaw and roll, and pitch reads zero.

`tobii headpose --check` prints the model's yaw and roll beside the geometry's,
which is how the sign conventions were confirmed on hardware.

### CLI

```sh
./target/release/tobii stream [--json] [--eyes]   # decoded gaze samples
./target/release/tobii setup                      # interactive display setup
./target/release/tobii display get|set            # read / re-apply display area
./target/release/tobii enabled-eye [both|left|right]
./target/release/tobii update [--install]         # check for a new release
./target/release/tobii --version                  # what this build reports
```

Run `tobii` with no arguments for the full list, including the protocol
diagnostics (`streams`, `log`, `dump-stream`, `camera`, `cal-blob`, `cal-points`).

## When the tracker is on

The ET5's infrared illuminators are lit for as long as a USB session is open, so
this program keeps one open only while something actually wants data:

| Holds the tracker on | For how long |
|---|---|
| The hub window | while it has **focus** |
| Preview my gaze | while the overlay is shown |
| Calibration, display setup, the accuracy diagnostic | while the flow is running |
| A queued setting (e.g. select eyes) | until it has been applied |

Three seconds after the last of those lets go, the session closes and the LEDs
go out. The linger is not arbitrary: closing the session makes the ET5 reboot,
and the next connect has to re-apply the display area, the eye selection and the
calibration blob, so alt-tabbing away and back should not pay for that twice.

The hub says **Tracker off** when nothing is asking for it. That is the normal
resting state, not a fault.

A useful consequence: while the hub is unfocused it holds no USB session, so
`tobii headpose` can claim the device for a game without closing the hub first.

### Start at login

*Start when I log in* in the hub writes an XDG autostart entry that runs
`tobii-gtk --background`: no window, and — because nothing is asking for data —
no tracker either. It exists so the tracker's saved display area and calibration
are re-applied as soon as anything wants them, which matters because **the ET5
wipes both every time it reboots**.

Launching the application again, from the menu or the command line, raises the
hub belonging to that background process rather than starting a second copy.

To turn it off: the same switch, or delete
`~/.config/autostart/com.tobiilinux.Configuration.desktop`. Turning it off in
GNOME Tweaks or KDE's Autostart page is also honoured — the switch reads their
`Hidden=true` / `X-GNOME-Autostart-enabled=false` rather than fighting it.

## Configuration

Stored under `$XDG_CONFIG_HOME/tobii-linux/` (default `~/.config/tobii-linux/`):
`config.toml` (display geometry), `calibration.bin`, `enabled_eye`,
`headpose_pitch_offset`, `update_check`, and `models/` (the fetched head-pose
model). The autostart entry, if enabled, is
`~/.config/autostart/com.tobiilinux.Configuration.desktop`; the menu entry and
icon go under `~/.local/share/`.

> **Note:** the ET5 wipes its display area *and* its calibration every time it
> reboots — which it does on every session close — so the driver **re-applies
> both on every connect**. Without this the tracker reports no eyes at all.

## Status

**Validated on hardware:** gaze streaming, display setup, calibration (the
12.1 mm figure above is measured, not estimated), the live views, the gaze
overlay, and head tracking. The head-pose model's yaw and roll signs were
confirmed against the geometric pose (slope +1.03 and +0.96, r = 0.998 and
0.990).

**Implemented, partly proven:** the **update mechanism**. Everything after the
download — checksum, unpack, symlink refusal, the runnability probe, the swap
and the rollback — is exercised end to end against real `tar.gz` archives built
from real executables (`crates/tobii-update/tests/install_end_to_end.rs`). The
network half has only run against GitHub's live releases endpoint returning an
empty list, because this repository has no tags yet, so the *first real release
is still the first real test of the download itself*.

The trust model is the honest limitation, not a missing test: the checksums are
an integrity check, not a signature. See [Updates](#updates).

**Known limitations:**

- The head-pose model's **focal length is assumed** (`DEFAULT_FOCAL_PX = 355`),
  which feeds a perspective correction worth 12–16° of pitch.
  `focal_from_eye_origins` can compute the real value from data already on the
  wire; it is not yet wired to do so automatically.
- On a display at **fractional scaling**, the tops of tall glyphs can appear
  shaved. CSS, the GSK renderer and widget-label theories have all been ruled
  out by measurement and it is not reproducible offscreen; the remaining suspect
  is the compositor's own downscale. Setting the display to an integer scale is
  the test.
- `0x501` and `0x50e` are the **same** camera exposed twice, not a stereo pair
  (199/199 byte-identical frames with a face in view, 166/166 on an empty
  scene). There is no stereo depth to be had from this device.

## Protocol documentation

The reverse-engineered ET5 USB protocol is documented in [`docs/wiki/`](docs/wiki/)
(mirrored to the GitHub project wiki): TTP framing, the handshake, the full op
catalog, the gaze-stream column layout, display-area / calibration /
select-eyes, the stream map, head pose, and the reverse-engineering methodology.
Every non-obvious claim is tagged CONFIRMED / CODE-VERIFIED / HYPOTHESIS.

## Architecture

A Cargo workspace of focused crates:

| Crate | Responsibility |
|---|---|
| `tobii-protocol` | Pure protocol codec: TTP framing, handshake, gaze and camera decode (no I/O). |
| `tobii-usb`      | libusb (`rusb`) transport + connection driver. |
| `tobii-config`   | Display geometry, EDID detection, persistence, SHA-256. |
| `tobii-headpose` | Head pose: the geometric fallback, the ONNX backend, the model store, opentrack output. |
| `tobii-update`   | Release checking, download integrity, and installation with rollback. |
| `tobii-cli`      | The `tobii` command-line tool. |
| `tobii-gtk`      | The GTK4 hub, guided flows and gaze overlay. |
| `tobii-recap`    | Decodes a usbmon pcap capture into a readable TTP op catalog. |

## Updates

The GUI asks GitHub for the latest release when its window opens — the one
thing this program does on the network without being asked — and shows a banner
only if there is something newer. `tobii update` does the same from the command
line. Nothing is downloaded until you choose to update.

**Turning the check off:** the switch in the hub under *Check for updates*, or
`TOBII_NO_UPDATE_CHECK=1` in the environment, which also wins over the switch.
The setting is stored in `~/.config/tobii-linux/update_check`. An explicit
`tobii update` still works; only the automatic check is affected.

### What installing an update trusts

**The checksums are not a signature.** `SHA256SUMS` is published in the same
release as the archive and fetched over the same connection by the same code, so
it proves the download arrived intact and nothing about who produced it. Anyone
able to publish to this repository's releases could publish binaries that every
check in `tobii-update` accepts.

So installing an update trusts this project's GitHub releases exactly as much as
downloading a binary from the releases page and running it by hand would. That
is a normal amount of trust for a program you already run, but it is not the
guarantee a checksum is often assumed to give, and the UI says so before the
button is pressed. Closing that gap needs a signature checked against a key
compiled into the binary; there isn't one.

### What it does protect against

- Every request is HTTPS to a GitHub host, with the URL passed as an operand so
  it can never be read as a `curl` option, redirects pinned to HTTPS, and a
  timeout and size cap on all of it.
- A truncated or corrupted download is caught before anything is unpacked.
- Archive members that are symlinks are ignored, so an archive cannot cause a
  file it never contained to be installed.
- The new binaries are **run once before they are installed**. A release built
  against newer system libraries than your machine has fails here, while the
  working binaries are still in place, instead of leaving you with two that
  don't start.
- If any part of the swap fails, **every binary is rolled back**, so an update
  can't leave a new `tobii` beside an old `tobii-gtk`.

### Publishing a release

`scripts/release.sh <version> [triple]` builds the archive and `SHA256SUMS` in
the layout the updater expects, refuses to build if `Cargo.toml` disagrees with
the tag, reports the oldest glibc the binaries need, and checks they answer
`--version` — the same probe the updater runs before installing.

A `gnu` build only runs on a glibc at least as new as the one it was built
against, so building on a rolling distribution produces binaries that will not
start on older ones. Build in an old-glibc container, or target musl:

```sh
rustup target add x86_64-unknown-linux-musl
scripts/release.sh 0.2.0 x86_64-unknown-linux-musl
```

## Credits & license

Protocol reference: the [`tobiifree`](https://github.com/Aetherall/tobiifree)
project. Head-pose model: the [opentrack](https://github.com/opentrack/opentrack)
project (fetched at your request, not bundled — its weights are
non-commercial-only). Licensed **GPL-3.0-only**.
