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
  device's usable ±28° — about 0.92° at a 750 mm viewing distance.
- **Display setup** — a fullscreen guided flow: drag two lines onto the marks at
  the ends of the tracker and the screen geometry is derived, seeded from your
  monitor's EDID. Curved panels: the device is told the EDID **arc** width — it
  only accepts a flat plane, and sending the chord makes that plane too narrow
  and compresses gaze toward the edges — and the per-user calibration absorbs
  the curve. No runtime gaze correction is applied; one was written and then
  disproved on hardware.
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
- **Output to games** as a virtual joystick (nothing else needed), over
  opentrack's UDP protocol, or to TrackIR/FreeTrack games through a Wine bridge.
- **Pitch zero calibration** — the model reports pitch in its own frame, offset
  by how your tracker is mounted. `tobii headpose --calibrate-pitch` measures it
  once.

### The app

- **GTK4 hub** (`tobii-gtk`): a live instrument panel — the trackbox with a
  graticule and the tolerance region drawn, your eyes as dots, the head drawn
  around them turning as you turn, a readout of position, distance, yaw, pitch
  and roll, and the infrared sensor view, across the top; six setting cards in
  three columns beneath. The layout has three column counts and drops to two,
  then one, as the window narrows — though on a floating desktop the window's
  own minimum width is the three-column width, so reaching the narrower ones
  takes a tiling compositor or a screen smaller than the hub.
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
  which keeps the program resident with no window, so the hub opens instantly
  and launching the app again raises it rather than starting a second copy. It
  does **not** turn the tracker on, and does not apply anything to it: nothing
  asks for data until you open a window, which is the point of the standby
  behaviour below.
- **Updates** — the hub checks for a new release at launch and can show the
  changelog and install it. Nothing is downloaded without a click. The check can
  be switched off. See [Updates](#updates) for what installing one trusts.

## Installation

Two ways, and the choice is really one question: **do you want the built-in
updater, or do you want your system to own this?**

| | Get it | Updated by |
|---|---|---|
| **A release** — `.tar.gz`, `.deb`, `.rpm`, an Arch package, or a `PKGBUILD` | [Releases](https://github.com/Tropaion/Tobii_Linux/releases) | the built-in updater (`.tar.gz` only) or your package manager |
| **From source** | the steps below | `git pull` and rebuild |

The prebuilt binaries are built in a Debian 13 container, which caps the glibc
they can need at **2.41**. What they actually need is measured from the binaries
when they are built — **2.39** for v0.3.0 — and the `.deb`, the `.rpm` and the
Arch package declare that measured floor, so your package manager refuses rather
than installing something that cannot start. On an older distribution, build
from source.

<details>
<summary><b>Installing a release</b></summary>

```sh
# the tar.gz — the only one the built-in updater can update
tar -xzf tobii-linux-*.tar.gz
cd tobii-linux-*/ && ./install.sh          # ~/.local/bin, menu entry, udev rule
sudo ./install.sh --system                 # the same for every user, /usr/local/bin
./uninstall.sh --dry-run                   # removing it again — see Uninstalling

sudo apt install ./tobii-linux_*.deb       # Debian, Ubuntu, Mint, Pop!_OS
sudo dnf install ./tobii-linux-*.rpm       # Fedora, RHEL (gtk4-layer-shell is in EPEL)
sudo zypper install ./tobii-linux-*.rpm    # openSUSE
sudo pacman -U ./tobii-linux-bin-*-x86_64.pkg.tar.zst   # Arch — see below
```

**On Arch, CachyOS, Manjaro or EndeavourOS**, use the prebuilt package. It needs
no Rust toolchain and has nothing to compile. Every release from v0.3.1 on has
one; v0.3.0 has only the `PKGBUILD` described below.

```sh
curl -fLO https://github.com/Tropaion/Tobii_Linux/releases/download/vX.Y.Z/tobii-linux-bin-X.Y.Z-1-x86_64.pkg.tar.zst
curl -fLO https://github.com/Tropaion/Tobii_Linux/releases/download/vX.Y.Z/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
sudo pacman -U ./tobii-linux-bin-X.Y.Z-1-x86_64.pkg.tar.zst
```

For a pre-release, drop the dash from the version in the package's name only:
`v1.0.0-rc1` ships `tobii-linux-bin-1.0.0rc1-1-x86_64.pkg.tar.zst`, because a
pacman version cannot contain one. The tag in the URL keeps it.

Once `tobii-linux-bin` is on the AUR, `paru -S tobii-linux-bin` (or `yay -S`)
does the same and updates it along with the rest of your system.

Download the file first and install it from disk. `pacman -U` with the URL
refuses this package: pacman checks a remote file at `RemoteFileSigLevel`, which
defaults to `SigLevel`, and Arch's stock `pacman.conf` sets that to `Required`.
The package is not signed. A file on disk is checked at `LocalFileSigLevel`,
which the same `pacman.conf` sets to `Optional`. The `sha256sum -c` step checks
the download against the release's `SHA256SUMS`. That proves the file arrived
intact and says nothing about who built it; see
[What installing an update trusts](#what-installing-an-update-trusts).

**To build it yourself** instead, download `PKGBUILD` *and*
`tobii-linux.install` from the release into one directory and run `makepkg -si`.
That compiles the program against your own GTK (it needs `rust`) and installs a
package called `tobii-linux`. It and `tobii-linux-bin` conflict, so installing
either one offers to remove the other.

`install.sh` copies the two binaries, adds the application-menu entry, and asks
before using `sudo` for the udev rule. The packages do all of that as part of
installing, into `/usr/bin` — which is also why the built-in updater will not
replace them. It offers to **download** the file your package manager wants
instead; see [Updates](#updates).

Do not run `install.sh` with plain `sudo`: under sudo your home directory is
`/root`, so it would install for the root account. It refuses, and names the two
commands that are meant — `./install.sh` as yourself, or
`sudo ./install.sh --system` for every user.

**Re-plug the Eye Tracker 5 afterwards** so the udev rule takes effect.
</details>

### From source

Works on any distribution and builds against your own GTK. The whole process is
four commands, and `scripts/build.sh` checks the dependencies before it starts
so a missing package is named in the first second rather than as a wall of
linker errors five minutes in.

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
sudo dnf install gcc git pkgconf-pkg-config \
     gtk4-devel gtk4-layer-shell-devel libusb1-devel
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # if you have no Rust
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
sudo zypper install gcc git pkg-config \
     gtk4-devel gtk4-layer-shell-devel libusb-1_0-devel
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # if you have no Rust
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
services.udev.extraRules = builtins.readFile ./assets/60-tobii.rules;
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
choose another (a relative one is taken from where you ran `build.sh`), or add
`--system` for `/usr/local/bin` and every user's menu
(`scripts/build.sh --install --system`). Every user's menu entry runs what is in
that directory, so `--system` refuses one that another account could change, and
names the folder that stops it; [Development](docs/wiki/Development.md) has the
rule. `--udev` installs the device rule described below. Run it as yourself, not
with `sudo` — it refuses, and asks for `sudo` itself for the steps that need it.
Leave both off to just build into `target/release/`.

`--install` also adds **Tobii Eye Tracker** to your application menu.

**After installing, re-plug the Eye Tracker 5** so the udev rule takes effect,
then run `tobii-gtk` or pick it from the menu.

## Uninstalling

**Installed with `install.sh` or `scripts/build.sh --install`:** `tobii` removes
itself. Look at the plan first:

```sh
tobii uninstall --dry-run     # prints what it would do; changes nothing
tobii uninstall               # asks, then does it
```

If that `tobii` is gone already, the release archive runs its own copy for you:
`./uninstall.sh --dry-run`, then `./uninstall.sh`, from the unpacked folder.

It looks in these places, and only these: the install manifest the installer
writes (`~/.local/share/tobii-linux/installs`), the directory named in the menu
entry's `Exec=` and in the start-at-login entry's, `~/.local/bin`, the
directory of the `tobii` you run it with, and any `--bindir DIR` you give it.
It also reads the system-wide manifest,
`/usr/local/share/tobii-linux/installs`. A directory listed there is a
`--system` install, which it leaves and prints the command for — unless
`sudo tobii uninstall --system` would refuse that directory too (a `~/bin` of
yours, say), and then it is checked like any other place. With
`--system` it looks only in that manifest and in `--bindir`. It does not search
your `PATH`. An install somewhere else that none of these lead to — one made
into a directory of your choice by v0.3.0 or earlier, with no menu entry — is
found with `--bindir DIR`.

It asks before stopping a hub that is still running, then removes the
binaries, the menu entry, its icon and the start-at-login entry. It deletes
only names it knows. The only directories it removes with everything in them
are the `.tobii-update-<pid>` work folders an interrupted update left beside
the binaries. Every other directory it touches (the install manifest's, and
with `--purge` the settings, models and log directories) is removed only once
it is empty.

**What it will not touch, and why.** A copy found any way but the manifest is
checked before it is run or removed. It is left where it is, never run, and
listed with the reason, if:

- it is not a program. A script called `tobii-gtk` in `~/.local/bin` is a
  wrapper of yours, not a build of this program.
- it belongs to a user other than you or root. It is theirs to remove, with
  their own `tobii uninstall`. That holds for a copy the manifest lists too.
  Root's files pass this check, so a copy an older `sudo ./install.sh` left in
  a folder of yours is treated as yours.
- others can write where it is: the file, or a directory that decides which
  file its name leads to — the one it is in, the one of each symlink on the
  way, and every directory above those — belongs to a user other than you or
  root, or can be written by another user or by a group other than your own
  private group (the group of your own that Fedora and others give each user is
  fine; so is a sticky directory such as `/tmp`). Whoever can write there
  chooses what would run.

A copy that passes must still name itself when asked `--version`. It also
leaves alone:

- a Cargo build directory, recognised by the `CACHEDIR.TAG` Cargo writes
  whatever the directory is called, and an unpacked release archive
  (`install.sh` with `assets/install-payload.sh` beside it). Copies run from
  there, but no install made them.
- a copy `cargo install` put there. Cargo keeps a record beside the directory
  (`.crates.toml`, `.crates2.json`). What that record lists as installed from
  `tobii-cli` or `tobii-gtk` is left, and it prints `cargo uninstall` for
  exactly those packages, always with `--root` naming where they are — Cargo's
  own default can be moved by `CARGO_INSTALL_ROOT` or `install.root`, so a
  command without it could remove a different copy. Only a directory named
  `bin` is Cargo's, because that is the only one `cargo install` writes. A record
  that lists only other programs — `cargo install --root ~/.local ripgrep`
  writes one beside `~/.local/bin` — hides nothing.
- a directory you cannot write to. If what is there answers as this program
  and the directory is yours, it prints `chmod u+w` for it, to run this again
  after. Otherwise it is a system-wide install, and the command for that is
  printed (see below).
- a menu or start-at-login entry that runs a copy which stays — a package's, a
  build directory's — and one whose program it cannot tell. That includes an
  entry whose `Exec` line cannot be read here: there is none, there are two, or
  it is quoted in a way launchers read differently, as a v0.3.0 menu entry is
  for a directory whose name holds one of `' \ & | ; $ ~ # * ? < >` or a
  backtick. Such an entry is not used to find an install either; `--bindir DIR`
  finds one it would have led to.

`--yes` goes ahead without asking, which is what a run with no terminal needs.
It does not stop for the bridge question below; it goes on. It stops running
copies: a hub is first asked to quit over D-Bus (except when a hub from a copy
that stays is also running, because D-Bus could reach that hub instead), and
whatever is still running 5 seconds later gets `SIGTERM`. That includes a
`tobii game` wrapper, which is a running game's head tracking and is never
asked first. A hub still running from a program that has been deleted (a
package removed while it ran) is shown and stopped the same way. Without
`--yes`, stopping it is only offered, and declining leaves it running.

If an `icon-theme.cache` exists in the `hicolor` directory the icon is removed
from (`~/.local/share/icons/hicolor`, or `/usr/local/share/icons/hicolor` with
`--system`), it is refreshed after the icon is removed. When no icons are left
under that directory, the cache tool deletes the cache, which puts the
directory back as it was before the install. No cache is ever created.

It leaves:

- **your settings, calibration, head-pose model and log.** Add `--purge` to
  remove those too. Only the files this program writes are deleted, and
  anything else it finds there — a backup of your own — stays and is listed.
  `calibration.bin` is the part that is costly to redo.
- **the udev rule.** Add `--udev`. That step asks first, and uses `sudo` unless
  it already runs as root (`--system`); `--yes` runs it without asking. With no
  terminal it only prints the commands, for you to run.
- **the TrackIR/FreeTrack bridge in Wine prefixes.** It lists the prefixes it
  finds the bridge in — Steam's, `$WINEPREFIX` and `~/.wine` — with the command
  for each. Removing the bridge needs a `tobii`. When the `tobii` you run is
  one this removes, it offers, before removing anything, to stop and let you
  run those first. With `--yes` it goes on, and repeats the commands at the
  end: the `tobii` in a release archive runs them just as well. Run from the
  archive's `./uninstall.sh`, that `tobii` stays, so it names it for the
  commands, which work before or after, and does not stop.
- **a copy your package manager installed.** It prints the command instead:

```sh
sudo pacman -R tobii-linux-bin # Arch (the prebuilt package, or from the AUR)
sudo pacman -R tobii-linux     # Arch (the PKGBUILD)
sudo apt remove tobii-linux    # Debian, Ubuntu, Mint, Pop!_OS
sudo dnf remove tobii-linux    # Fedora, RHEL   (openSUSE: sudo zypper remove tobii-linux)
```

A system-wide install (`sudo ./install.sh --system`) comes out with
`sudo tobii uninstall --system`. Without `--system`, `tobii uninstall` refuses to
run as root. Under `sudo`, HOME is `/root`, so it would search root's home
instead of yours. As root it neither runs nor opens anything in a directory
that someone other than root could have changed — a directory the system
manifest lists included. One it refuses is left, with the reason and the
command for whoever controls it: `tobii uninstall --bindir DIR`, run as that
user. Its line in the system manifest stays until a later
`sudo tobii uninstall --system` finds the directory empty.

<details>
<summary><b>By hand — for v0.1.0 to v0.3.0, whose <code>tobii</code> has no <code>uninstall</code></b></summary>

Every path an install writes, and every path the program writes while it runs.
The directory is `~/.local/bin` unless you gave `install.sh` or `--install`
another one; the `Exec=` line of the menu entry names it. If you set
`$XDG_DATA_HOME`, `$XDG_CONFIG_HOME` or `$XDG_STATE_HOME`, those replace
`~/.local/share`, `~/.config` and `~/.local/state` below.

```sh
# the program
rm -f ~/.local/bin/tobii ~/.local/bin/tobii-gtk
# temporaries an interrupted update can leave beside them
rm -rf ~/.local/bin/.tobii-update-[0-9]*
rm -f ~/.local/bin/.tobii-update-probe-* \
      ~/.local/bin/.tobii.new-* ~/.local/bin/.tobii-gtk.new-* \
      ~/.local/bin/.tobii.old-* ~/.local/bin/.tobii-gtk.old-*

# the menu entry, its icon, and start-at-login
rm -f ~/.local/share/applications/com.tobiilinux.Configuration.desktop
rm -f ~/.local/share/icons/hicolor/scalable/apps/com.tobiilinux.Configuration.svg
rm -f ~/.config/autostart/com.tobiilinux.Configuration.desktop
# only if this file already exists — do not create one:
#   gtk4-update-icon-cache -qtf ~/.local/share/icons/hicolor
#   (~/.local/share/icons/hicolor/icon-theme.cache; with no icons left under
#   hicolor, that command deletes the cache — as it was before the install)

# optional: settings, calibration, models and log — the names this program
# writes, so anything of your own in these directories stays. A save that was
# interrupted can also leave one of these names with .tmp added.
rm -f ~/.config/tobii-linux/config.toml ~/.config/tobii-linux/calibration.bin \
      ~/.config/tobii-linux/calibration.meta.toml ~/.config/tobii-linux/enabled_eye \
      ~/.config/tobii-linux/update_check ~/.config/tobii-linux/text_scale \
      ~/.config/tobii-linux/headpose_pitch_offset \
      ~/.config/tobii-linux/setup_monitor_id ~/.config/tobii-linux/games.toml \
      ~/.config/tobii-linux/accuracy.csv ~/.config/tobii-linux/report_salt
rm -f ~/.config/tobii-linux/models/head-pose-0.5-small.onnx \
      ~/.config/tobii-linux/models/head-pose-0.5-small.onnx.partial \
      ~/.config/tobii-linux/models/head-pose-0.5-small.onnx.download \
      ~/.config/tobii-linux/models/head-localizer.onnx \
      ~/.config/tobii-linux/models/head-localizer.onnx.partial \
      ~/.config/tobii-linux/models/head-localizer.onnx.download
rm -f ~/.local/state/tobii-linux/tobii.log ~/.local/state/tobii-linux/diagnostics.txt
# then the directories: rmdir refuses one that still holds something, and says so
rmdir ~/.config/tobii-linux/models ~/.config/tobii-linux ~/.local/state/tobii-linux

# or instead, the directories with EVERYTHING in them — which also deletes any
# backup of your own kept there, such as a copy of calibration.bin:
#   rm -r ~/.config/tobii-linux ~/.local/state/tobii-linux

# optional: the udev rule
sudo rm -f /etc/udev/rules.d/60-tobii.rules /etc/udev/rules.d/99-tobii.rules
sudo udevadm control --reload
sudo udevadm trigger --subsystem-match=usb
sudo udevadm trigger --subsystem-match=misc
```

If you ever ran `sudo ./install.sh`, it installed a second copy into root's
home:

```sh
sudo rm -f /root/.local/bin/tobii /root/.local/bin/tobii-gtk \
     /root/.local/share/applications/com.tobiilinux.Configuration.desktop \
     /root/.local/share/icons/hicolor/scalable/apps/com.tobiilinux.Configuration.svg
```

The bridge, if you installed it into a Wine prefix, is removed with
`tobii bridge uninstall --prefix PATH`. Run that before removing `tobii`.
</details>

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
scripts/build.sh --install --system # the same for every user, /usr/local/bin
scripts/build.sh --udev             # also install the udev rule
```

Plain `cargo build --release` works too. The neural head-pose backend is on by
default and brings a sizeable dependency tree (`tract`, ~110 crates); the
**CLI** can be built without it — 5 DOF head tracking, everything else
unchanged, and a 1.2 MB binary instead of 35 MB:

```sh
cargo build --release -p tobii-cli --no-default-features
```

The GUI currently always includes it; `tobii-gtk` has no matching feature gate
yet.

## Device access (one-time)

Already done if you ran `scripts/build.sh --udev`. By hand:

```sh
sudo cp assets/60-tobii.rules /etc/udev/rules.d/
sudo rm -f /etc/udev/rules.d/99-tobii.rules   # if you installed a pre-v0.1.0 copy
sudo udevadm control --reload && sudo udevadm trigger
```

Then (re-)plug the Eye Tracker 5. Without this the device is only reachable as
root, and the tracker reports no eyes at all.

The rule grants access through `uaccess`: systemd-logind puts an ACL on the
device for whoever is logged in at the seat. **The `60-` in the name is
load-bearing** — udev reads rule files in lexical order and
`73-seat-late.rules` is what acts on the `uaccess` tag, so the pre-v0.1.0
`99-tobii.rules` set it too late to have any effect and granted access only
through a world-readable `MODE="0666"`. A leftover copy still overrides the new
rule's mode, which is why the line above deletes it; `tobii debug` says so if
one is present.

### If the tracker still needs root

`uaccess` needs systemd-logind. Without it there is no ACL and `MODE="0660"`
leaves the device to root. Give a group access instead — pick one your user is
already in (`id -nG`), often `plugdev` on Debian-family systems:

```sh
# Addressed to the tracker lines only — see the warning below.
sudo sed -i '/idVendor/s/MODE="0660"/MODE="0660", GROUP="plugdev"/' \
    /etc/udev/rules.d/60-tobii.rules
sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=usb
```

> **Do not drop the `/idVendor/` address.** The file also carries a
> `MODE="0660"` line for `/dev/uinput` (the virtual joystick). An unaddressed
> `sed` would put `GROUP="plugdev"` on that one too, and every member of the
> group — logged in or not — could then synthesise keystrokes into any local
> session. That is a much broader grant than the `uaccess` one this section is
> replacing, which is scoped to whoever is actually sitting at the machine.

Without logind the **virtual joystick** needs its own group grant as well, and a
second one: udev's `uaccess` is also what makes the device it creates readable,
so add your group to `/dev/uinput` and to the created node:

```sh
echo 'KERNEL=="uinput", SUBSYSTEM=="misc", MODE="0660", GROUP="plugdev", OPTIONS+="static_node=uinput"
SUBSYSTEM=="input", ENV{ID_INPUT_JOYSTICK}=="?*", MODE="0660", GROUP="plugdev"' \
  | sudo tee /etc/udev/rules.d/61-tobii-nologind.rules
sudo udevadm control --reload
```

## Usage

### GUI

```sh
cargo run --release -p tobii-gtk
```

Or pick **Tobii Eye Tracker** from your application menu, once
`scripts/build.sh --install` has added it.

The hub shows connection status, the live instrument panel, and the tracking
settings: improve calibration, head tracking, preview my gaze, select eyes,
change screen. The cogwheel beside the connection status holds the rest — start
at login, check for updates, **text size** (80–160%, for the whole program), and
saving or copying the diagnostics report.

```sh
tobii-gtk --background   # resident, no window, tracker off — but an icon in
                         # the status area, where there is one (see below)
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
./target/release/tobii debug                      # the report an issue asks for
./target/release/tobii record [--calibration]     # re-record a replay fixture
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

### Head tracking in a game

The tracker is only on while something asks for it, and a game cannot ask — it
speaks opentrack or TrackIR, not this program's socket. So wrap the game:

```sh
tobii game -- %command%        # Steam: paste this into Launch Options
tobii game -- ./MyGame.x86_64  # or anywhere else
```

The hub must be running (it is, if you closed its window rather than quitting).
The tracker comes on when the game starts and goes dark a few seconds after it
exits — measured on the wrapper above: **0 USB file descriptors held before, 1
while the game runs, 0 again afterwards.**

`tobii game` is transparent to whatever launched it: it exits with the game's own
exit code, reports a killed game as 128 + the signal rather than as success, and
**never stops the game starting**. With no hub running it prints a note and runs
the game anyway — head tracking is worth less than the game launching.

Steam's `%command%`, Lutris's and Heroic's wrapper fields, and a plain shell
script all work with no further support, which is why this is a wrapper rather
than a setting.

### Games with no head-tracking support (virtual joystick)

Most games have never heard of head tracking, but nearly all of them can bind a
joystick axis. So game output also presents one:

```sh
tobii games set joystick true    # on by default
```

It appears as **Tobii Eye Tracker 5 head tracking**, an eight-axis controller:

| Axis | Carries | Full scale |
|---|---|---|
| X, Y, Z | head displacement from where you sit | ±500 mm |
| RX, RY, RZ | yaw, pitch, roll | ±180°, ±90°, ±180° |
| Throttle, Rudder | gaze on screen, left→right and top→bottom | the whole screen |

Nothing else has to be installed — no opentrack, no Wine. Because it is an
ordinary evdev joystick it is read by SDL, by the legacy `/dev/input/js*`
interface, and by Wine's `winebus`, so **Proton games see it as a DirectInput
joystick** too (not XInput — measured under Wine 11.17; XInput's two-stick
layout has nowhere to put eight axes). The last two axes are gaze, which no
other head tracker offers; bind them to a free-look axis and the camera follows
your eyes.

If the axes feel like they barely move, turn up the amplification — this is the
first thing to tune with a game in front of you:

```sh
tobii games set joystick_yaw_full_deg 45   # head angle for a full swing (default 70)
```

Two things to know before you bind it, both covered in
[`docs/wiki/Game-Output.md`](docs/wiki/Game-Output.md): some games' "look" axis
is a **rate** control rather than a position, and binding a head tracker to one
of those makes the view spin away rather than follow your head; and **Steam
Input** can quietly take the device away from a Steam-launched game, which is
fixed per-game with Properties → Controller → Disable Steam Input.

This needs write access to `/dev/uinput`, which the packaged udev rule grants —
`tobii debug` reports whether it is there. The rule is one line and the file
says how to remove it if you would rather not grant it; everything else keeps
working without it.

### Games that speak TrackIR or FreeTrack (Wine)

opentrack is not the only protocol. Windows games under Wine ask for TrackIR or
FreeTrack, which means a DLL inside the prefix and a shared-memory block — so
there is a small Windows-side bridge:

```sh
scripts/build-bridge.sh                  # needs mingw-w64 + the rust win target
tobii bridge games                       # what is installed, and what has a prefix
tobii bridge install --steam elite       # by name, or by app id
tobii bridge install --prefix /path/to/prefix   # anything not Steam
```

**Nothing has to be left running.** The client DLL the game loads receives the
tracking itself, in a background thread inside the game's own process, and
publishes it into `FT_SharedMem` where the game reads it. A second executable
would have to run inside the game's own wineserver session, which for a Steam
game means reproducing Proton's entire launch environment; the DLL is already
in there.

**64-bit games only.** A 32-bit game asks for `freetrackclient.dll` without the
`64` and finds nothing — see
[`docs/wiki/Game-Output.md`](docs/wiki/Game-Output.md).

`install --steam` finds the prefix from Steam's own library files and writes the
registry with **the Proton build the prefix records**, not whatever `wine` is on
your `$PATH` — a foreign wine would upgrade the prefix out from under the game.

**TrackIR is pointed at an already-installed client** (opentrack's, if you have
it) rather than at ours, because games verify NaturalPoint's signature and a
clean-room DLL cannot answer it. That is not a guess: the established
implementations answer `NP_GetSignature` from two 200-byte tables XORed
together, which is NaturalPoint's own signature data carried in obfuscated
halves — which is also why scanning those DLLs for the string "NaturalPoint"
finds nothing. Shipping it would mean redistributing their blob, so we do not.
Nothing is copied — our provider still supplies the data behind it.
`--npclient ours` overrides that for a game that does not check, and FreeTrack
has no signature at all, so a game that speaks FreeTrack works with our own DLL
today.

Verified end to end on a real Elite Dangerous Proton prefix with **no provider
running**: the game's own load path (`LoadLibrary` on the registry-supplied
directory, then `GetProcAddress`) resolved all five FreeTrack exports, and a pose
sent from Linux as `yaw 7.5°, pitch −3.25°, roll 1.5°, (11, 22, 33) mm` read back
through `FTGetData` as `yaw=0.1309, pitch=-0.0567, roll=0.0262` radians and
`pos=(11.0, 22.0, 33.0)`, with `DataID` advancing.

`tobii bridge uninstall --prefix PATH` removes it again.

### Closing the window does not quit

Pressing **X** puts the hub in the background and leaves it running. That is
deliberate: only one process can claim the tracker over USB, so the program that
owns the device has to be the same one that feeds a game — and configuring it
means opening this window. A hub that died when you dismissed it would take the
game's head tracking with it.

**Where it goes depends on your desktop.** If something on your session bus owns
`org.kde.StatusNotifierWatcher` — KDE Plasma, and the panels and bars that
implement the same interface — the window is *hidden* and a tray icon is your
way back. With no such host, stock GNOME included, the window is *minimised*
exactly as it was before, and Alt-Tab or the overview is the way back. Either
way, launching the app again raises the hub you already have.

The tray icon has no menu on purpose: left-, right- and middle-click all just
raise the hub, and *Quit* lives in the cogwheel inside it. Plasma looks for
icons only in folders that existed when it started, so right after a first
install the tray and the *application menu* can show a generic icon. The hub
hands the tray its own copy of the picture (`IconThemePath`) for exactly that
case; Plasma reads it, but that it then draws from it has not been confirmed. If
the placeholder stays, `systemctl --user restart plasma-plasmashell.service`, or
your next login, fixes both.

The tracker still goes dark. Hiding releases the claim as it hides; minimising
drops it within a frame, because the claim is polled from whether the window is
active. Measured on a **minimised** hub: **no USB file descriptors and no GPU
file descriptors held**. If you left *Preview my gaze* on, that has its own claim
and the tracker stays on, which is correct — something is asking for data.

Launching the app again — from the menu, the dock, or `tobii-gtk` — raises the
existing window rather than starting a second copy.

**To exit for real**, use *Quit* in the cogwheel menu. From a script, or when the
window is somewhere you cannot reach, the same thing is an action on the session
bus:

```sh
gdbus call --session --dest com.tobiilinux.Configuration \
  --object-path /com/tobiilinux/Configuration \
  --method org.freedesktop.Application.ActivateAction quit '[]' '{}'
```

### Start at login

*Start when I log in* — behind the cogwheel in the hub's header, with the other
settings that are about the program rather than the tracker — writes an XDG
autostart entry that runs
`tobii-gtk --background`: no window, and — because nothing is asking for data —
no tracker either. Where your desktop has a status area there is an icon in it,
which is the way to open the hub; otherwise the application menu is. It exists so
the hub is already running when you want it, and so that launching the app hands
off to the one process that can claim the tracker rather than starting a second.

The entry runs whichever copy switched it on, by its full path. Switched on from
a copy that was replaced while it ran — by `install.sh`, the updater or a
package upgrade — it names the path the new copy is at. From a copy that was
removed while it ran, the switch refuses, and its tooltip says why. If that copy
is later removed or moved, the entry points at nothing and the next login starts
nothing; `install.sh` repairs it when that is so, and when it points at a
different copy that still exists it leaves it alone and tells you, because which
copy starts at login is your choice. It reads the entry's `Exec` line as GLib
does, and leaves alone one it cannot read for certain: two `Exec` lines, an
unterminated quote, or a single quote outside double quotes.

Launching the application again, from the menu or the command line, raises the
hub belonging to that background process rather than starting a second copy.

To turn it off: the same switch, or delete
`~/.config/autostart/com.tobiilinux.Configuration.desktop`. Turning it off in
GNOME Tweaks or KDE's Autostart page is also honoured — the switch reads their
`Hidden=true` / `X-GNOME-Autostart-enabled=false` rather than fighting it.

## Configuration

Stored under `$XDG_CONFIG_HOME/tobii-linux/` (default `~/.config/tobii-linux/`):
`config.toml` (display geometry), `calibration.bin` and `calibration.meta.toml`,
`enabled_eye`, `headpose_pitch_offset`, `update_check`, `setup_monitor_id`,
`text_scale` (the hub's text size, as a bare number — delete it to get back to
100%),
`report_salt` (32 random bytes, mode 0600, which is what makes the monitor id in
a diagnostics report meaningless to anyone else), and `models/` (the fetched
head-pose model).

The **log** is not there: it is at `$XDG_STATE_HOME/tobii-linux/tobii.log`
(default `~/.local/state/…`), because the XDG spec puts logs in state. The
autostart entry, if enabled, is
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
listing and asset-selection halves have now run against two real published
releases. What is still unproven is the *install* half against a release
downloaded from GitHub rather than built locally, and the **Download** path for
package-managed copies, which no one has clicked through against a real release
— v0.3.0 is the first release that makes it reachable.

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

[`docs/wiki/`](docs/wiki/) (mirrored to the GitHub project wiki) holds both the
protocol documentation and the architecture documentation.

**The protocol:** TTP framing, the handshake, the full op catalog, the
gaze-stream column layout, display-area / calibration / select-eyes, the stream
map, head pose, and the reverse-engineering methodology. Every non-obvious claim
is tagged CONFIRMED / CODE-VERIFIED / HYPOTHESIS.

**The architecture**, as arc42: [Architecture](docs/wiki/Architecture.md) (§1–5),
[Runtime-View](docs/wiki/Runtime-View.md) (§6–7),
[Architecture-Decisions](docs/wiki/Architecture-Decisions.md) (§8–9),
[Quality-and-Risks](docs/wiki/Quality-and-Risks.md) (§10–12), and
[Development](docs/wiki/Development.md).

## Architecture

A Cargo workspace of focused crates:

| Crate | Responsibility |
|---|---|
| `tobii-protocol` | Pure protocol codec: TTP framing, handshake, gaze and camera decode (no I/O). |
| `tobii-usb`      | libusb (`rusb`) transport + connection driver. |
| `tobii-config`   | Display geometry, EDID detection, persistence, SHA-256. |
| `tobii-headpose` | Head pose: the geometric fallback, the ONNX backend, the model store, opentrack output. |
| `tobii-update`   | Release checking, download integrity, and installation with rollback. |
| `tobii-diagnostics` | The `tobii debug` report and the log it quotes. |
| `tobii-cli`      | The `tobii` command-line tool. |
| `tobii-gtk`      | The GTK4 hub, guided flows and gaze overlay. |
| `tobii-output`   | Game output: the virtual joystick, opentrack/TrackIR encoding, the frame pipeline. |
| `tobii-ipc`      | The local socket other programs read tracking from. |
| `tobii-recap`    | Decodes a usbmon pcap capture into a readable TTP op catalog. |

## Updates

The GUI asks GitHub for the latest release when its window opens — the one
thing this program does on the network without being asked — and shows a banner
only if there is something newer. `tobii update` does the same from the command
line. Nothing is downloaded until you choose to update.

**If a package manager owns your copy**, the button says *Download* rather than
*Update*, because overwriting a packaged file behind `dpkg`/`rpm`/`pacman`'s
back is how a package database comes to describe files that are no longer there.
It asks where to put it, fetches the artifact that matches your manager — the
`.deb`, the `.rpm`, or the prebuilt Arch package (for a release without one, the
`PKGBUILD` and its install hook together) — into a
version-named folder there, and prints the one command that installs it. It
never installs anything itself. This is the hub only; `tobii update` on the
command line still just refuses and tells you why.

Four more cases get something other than *Update*, all decided before the
banner appears, so the button you see is one that can work:

- **A copy in a folder only an administrator can change** — one installed with
  `sudo ./install.sh --system`, copied into a system folder by hand, or another
  account's files in a shared folder that is not yours either — also gets
  *Download*: the release archive, and the one command that installs it for every
  user, `sudo ./install.sh --system <that folder>`.
- **A copy in your own folder whose files are someone else's** — what an older
  `sudo ./install.sh ~/.local/bin` left behind — gets *Download* as well, with a
  plain `./install.sh <that folder>`, no `sudo`. That replaces root's files
  with your own, which an update in place cannot: its backup step hard-links
  the old file, and the kernel refuses that for a file you do not own.
  `tobii update --install` refuses the same copy and says so.
- **A copy in a folder you cannot write but can fix yourself** — your own
  folder with its write permission off, or a folder in your home that another
  account owns, which that same older `sudo ./install.sh ~/.local/bin` made
  when the folder did not exist yet — gets no button. The banner shows the one
  command, `chmod u+w <that folder>` or
  `sudo chown <your uid>:<your gid> <that folder>` (that folder alone, not what
  is in it), selectable so you can copy it, and says to reopen the app
  afterwards. `tobii update --install` prints the same command and says to run
  it again.
- **A copy that was replaced or removed while it was running** — its package
  uninstalled, or a newer version installed over it — gets *Quit*. There is
  nothing at its path to update, and starting the app again would only hand off
  to the same old process, so quitting it is the step that helps.

`sudo tobii update --install` on a copy whose files are an ordinary account's
refuses, and says to run it again as that account, without `sudo`: as root the
update would work, and leave root's files behind.

The decision and any failure are written to the log, which `tobii debug` quotes.

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

Four things go in the release commit, and the tag build fails without the first
two. `release.sh` refuses when `Cargo.toml` disagrees with the tag, and every
gate runs `--locked`, so a bumped manifest with a stale lock fails CI:

```sh
# 1. both workspaces' [workspace.package] version -> the new number
# 2. both lock files, which is a separate step:
cargo update -w --offline
cargo update -w --offline --manifest-path bridge/Cargo.toml
# 3. docs/releases/vX.Y.Z.md — this is what the published changelog and the
#    in-app "What's new" show. Without it CI falls back to raw commit subjects.
# 4. docs/wiki/Quality-and-Risks.md — a section for whatever new surface ships.

git commit -am "release: vX.Y.Z"
git push origin main          # the tag must point at a pushed commit
git tag vX.Y.Z && git push origin vX.Y.Z
```

CI drafts the release; publishing it is a human click.

**Releases are built by CI, not on a developer's machine, and that is the
point.** The machine a binary is compiled on *is* its compatibility floor: on
the maintainer's current system both binaries come out needing `GLIBC_2.44`,
which almost nobody has, so such a release would fail to start for nearly
everyone who downloaded it. `.github/workflows/release.yml` builds in a
Debian 13 container instead, fixing the floor at glibc 2.41, and **fails the
build if the floor ever rises above that** rather than shipping a narrower
release quietly.

It publishes a **draft** — the notes are the changelog every user's updater
shows them, so a human sees them before anyone does.

`scripts/release.sh <version> [triple]` is what CI runs and works locally too:
it refuses to build if `Cargo.toml` disagrees with the tag, reports the oldest
glibc each binary needs, and checks both answer `--version` — the same probe the
updater runs before installing.

## Contributing

Pull requests run these checks, and they pass on `main`:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
bash scripts/test-install-payload.sh   # the install scripts; needs no root
```

`rust-toolchain.toml` pins the compiler, and **rustup** honours it
automatically, so your `clippy` and `rustfmt` are the ones CI runs. A
distribution Rust ignores the pin entirely — the install blocks above use rustup
for exactly that reason, and `dnf install rust cargo` or `zypper install rust
cargo` would give you a compiler these checks do not describe.

**Most of what "needs an ET5" does not** — `tobii record` captures a real session
and `cargo test -p tobii-usb --test replay` regression-tests the protocol against
it with no tracker attached.

Everything else a contributor needs is in the wiki:
[Development](docs/wiki/Development.md) for the test suite, conventions and the
protocol traps; [Architecture](docs/wiki/Architecture.md) for how the eight
crates and three threads fit together;
[Quality-and-Risks](docs/wiki/Quality-and-Risks.md) for the measured numbers and
the known defects.

## Credits & license

Protocol reference: the [`tobiifree`](https://github.com/Aetherall/tobiifree)
project. Head-pose model: the [opentrack](https://github.com/opentrack/opentrack)
project (fetched at your request, not bundled — its weights are
non-commercial-only). Licensed **GPL-3.0-only**.
