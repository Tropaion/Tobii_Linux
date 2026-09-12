# Runtime and deployment view (arc42 §6–7)

Six scenarios traced through the real code, then how the thing is delivered.
Background in [[Architecture]]; the reasoning behind these shapes is in
[[Architecture-Decisions]].

---

## 6. Runtime view

### 6.1 Cold start of the GUI — launch to first live gaze frame

`main.rs` is two lines; everything is in `lib.rs:run`.

1. **`--version` is answered before GTK exists.** Deliberately first, so the
   binary is probe-able on a machine with no display — the updater runs exactly
   this on a freshly downloaded binary before installing it (§6.5).
2. `Application` is built with `APP_ID = "com.tobiilinux.Configuration"`.
   `--accuracy` sets `NON_UNIQUE`, because `GApplication` is single-instance and
   a second launch would otherwise hand off to a process whose argv has no
   `--accuracy`, so the diagnostic would silently never run.
3. **argv is filtered** (`is_our_flag`) before `run_with_args`: GTK aborts on
   options it does not recognise.
4. **`activate` fires** — and this is where the two launch shapes diverge.

**Normal launch:** `connect_activate` builds the hub via `build_hub`.

**Background launch (`--background`, what the autostart entry runs):**
`connect_startup` takes `app.hold()` (nothing else keeps a main loop alive with
no window) and calls `device::spawn()`. `connect_activate` then **returns
immediately for the first activation only**.

> That guard is the whole feature, and it is easy to lose. `GApplication`
> emits `activate` from `run()` whenever argv has nothing left to handle — and
> `--background` is filtered out of argv, so argc is 1 and activate fires.
> Without the guard, `--background` built and presented the hub, which took the
> focus claim, which opened a USB session: the settings window popping up and
> the illuminators coming on at *every login*. Only the first activation is
> suppressed; later ones arrive from a second launch handing off, and must raise
> the hub.

**The device thread** (`device::spawn`) returns immediately and parks in:

```rust
while !demand.active() && pending.is_empty() { /* … 120 ms … */ }
```

`DeviceState::status` is `Idle`, which the hub renders as **"Tracker off"** —
deliberately not "Disconnected", because the resting state is not a fault.

**When the session actually opens.** The hub's claim is synced from
`window.is_active()` on the 33 ms tick:

```rust
match (window.is_active(), hold.is_some()) {
    (true, false) => *hold = Some(demand.hold("the hub window")),
    (false, true) => *hold = None,
    _ => {}
}
```

Polled rather than signal-driven, and that is a correction, not laziness: it was
an unconditional claim at build time plus a `notify::is-active` handler to
release it, and where a compositor never grants focus that signal never fires —
so the claim was never released and the tracker stayed lit for the life of the
process. Polling a boolean 30 times a second is self-correcting; a one-shot
signal is not.

So: activate → device thread (idle) → widgets built → `present()` → focus
granted → tick takes the claim → device thread notices within ≤120 ms.

**Opening the session** (`device_session`): `UsbTransport::open` enumerates by
hand rather than using `open_device_with_vid_pid`, so a permission failure stays
distinguishable from an absent tracker (`Access → PermissionDenied`,
`Busy → DeviceBusy`, `NoDevice → DeviceNotFound`) — the difference between two
very different things to tell the user. Then `Connection::connect` drives the
pure state machine in `tobii-protocol/handshake.rs`:
`Hello → QueryRealm → OpenRealm → [RealmAuth] → Subscribe(0x500)`. Realm auth is
`hmac_md5(REALM_KEY, challenge)`; it is skipped when `realm_type == 0`. Only
`TTP_MAGIC_RSP` frames advance the handshake, so a gaze notification arriving
mid-handshake cannot be mistaken for a response — there is a test for exactly
that.

**Then the re-application, which is load-bearing** (see [[Architecture]] §2):
display area → eye selection → calibration blob → camera subscribe → read back
the eye selection to seed the UI → `status = Connected`.

**First frame.** `device_tick` drains commands, then `read_notifications()`.
`0x500` → `GazeSample::decode` → under one lock: `eye_history.update(&g)` (which
owns the per-eye extrapolation and must run exactly once per real frame),
`latest_gaze`, `status`. `0x501` → `decode_camera_frame` → `Arc`-wrapped (78 KB
at 33 Hz, two readers) and offered to the head-pose worker.

**The frame reaches the screen on the GTK frame clock, not the 33 ms timer.**
The drawing area uses `add_tick_callback`; the timer was slightly slower than
the ~33 Hz gaze stream and was silently dropping ~3 frames a second.

### 6.2 Turning the tracker off — and why three seconds

1. The last `DemandGuard` drops (hub loses focus, a flow window is destroyed, the
   overlay closes).
2. `Demand::active()` becomes false. The device thread's inner loop checks demand
   **before** each tick and starts a `LINGER` countdown.
3. After **3 s**, `break` → `conn` drops → `UsbTransport::drop` sends vendor
   control `SESSION_CLOSE = 0x42` and releases interface 0. **The ET5 answers a
   session close by rebooting, and the illuminators go out.**
4. The outer loop finds demand still false, resets state to `Idle`, and parks.

The linger is not arbitrary. Closing costs a device reboot, so the next connect
must re-apply display area, eye selection and calibration before any data flows.
Without it, alt-tabbing away and back would pay that twice, and every dialog —
each of which briefly moves focus — would thrash the device.

### 6.3 A calibration run

A tick-driven state machine in `calibrate_flow.rs` over the device thread:

1. **EyePreview** — live eye box; advance when centred.
2. **Starting** — waits for `state.calibration` to carry *our* token **and**
   `started == true`. The token exists because the previous session's fields
   survive until the device thread dequeues the command, and the flow ticks at
   33 ms while the device thread can sit in a 30 s USB request. Gating on
   `started` rather than `active` is what proves `start` **and** `clear` were
   both acked.
3. On the device thread: optionally retrieve the existing blob (filtered by
   `is_plausible_calibration`, ≥4096 bytes) **before** clearing; set
   `cal_session_open` *pessimistically* before issuing anything; `start` then
   `clear`; then re-apply the previous blob as a non-fatal seed — which is what
   makes "improve calibration" different from starting over.
4. **Collecting** — gaze must be continuously in-zone for `SETTLE_TICKS = 30`
   (~990 ms) with `GAZE_GAP_TOLERANCE_TICKS = 8` of blink tolerance. A lost
   streak mid-collect sends a discard. Each completed group is computed and
   applied so the next is collected against a partially fitted model, exactly as
   the original does. A group whose point the device refuses, or whose deadline
   elapses, is **re-shown** rather than ending the run — three attempts in all,
   captured points staying captured — and only the third failure reaches the
   failure screen, whose "Try again" still restarts from the first point.
5. **Computing** → compute → retrieve → persist to `calibration.bin`.

### 6.4 `tobii headpose` to a game

Its own process, its own connection — which is only possible because the hub
releases the device when unfocused. Nothing below goes through the hub or its
socket: this path holds the USB session itself, so the tracker is lit for
exactly as long as the command runs, and the `tobii game` wrapper has nothing to
do here. That wrapper is a socket client and nothing else — it exists for the
hub's route, where game output itself takes no `DemandGuard` and so cannot light
the tracker for a game that never connects (§6.2, and
[[Game-Output]]).

1. Resolve `--udp` (default `127.0.0.1:4242`) and `--rate` (default 60 Hz).
2. Bind an ephemeral local socket; opentrack only ever receives.
3. Load the ONNX model if installed and apply the saved pitch zero. Without a
   model this prints a 5-DOF notice and continues — pitch reads zero.
4. Connect, **re-apply the display area** (the ET5 reports no eyes without one).
5. Subscribe `0x501` for the NIR camera; a refusal downgrades to 5 DOF rather
   than failing.
6. Per frame: **position from the eye origins, rotation from the model.** The
   fusion is the point — two eye origins cannot express pitch, because nodding
   rotates the head about the line through them and leaves both origins where
   they were. Without a fresh model pose, a frame with only one tracked eye
   still yields one: the pipeline places the missing eye at the last measured
   interocular offset for up to 300 ms, holding rotation at its last
   measurement. With a model pose that path is bypassed — `onnx::fuse` takes the
   model's own position when the eye geometry is missing.
7. Encode six `f64` little-endian into the opentrack datagram and send — x, y, z,
   yaw, pitch, roll, which is 48 bytes, as `datagram_is_exactly_48_bytes` pins.
   `TRANSLATION_SCALE = 0.1` converts millimetres to the centimetres opentrack's
   `data[]` expects.

### 6.5 An update

Threads, in order: **launch-check thread** → GTK main → **install thread**.

1. At launch, *if the check is enabled*, a worker thread calls
   `release::check()`. The banner stays hidden until it has something; a slow or
   absent network costs the hub nothing, and a failure is silent because the
   user did not ask.
2. `decide()` offers the release only if it carries an archive for this target
   triple **and** checksums. Otherwise the banner says which of those is missing
   — "no build for your machine" told to somebody whose build is right there
   sends them looking for something that exists.
3. Still on the worker: **where is this copy, and what can be done to it?**
   `install::placement()` gathers it without writing anything — the hub promises
   nothing on disk changes before a click, which is why the probe-file
   `is_writable` is not used here:
   - `ownership_of()`, which asks `dpkg`, `rpm` and `pacman` in turn about each
     installed binary — up to three subprocess calls, each with a deadline;
   - whether this user can replace the binaries in place — two answers, because
     their failures need opposite advice: `faccessat(W_OK | X_OK)` on the
     directory (search as well as write, because a directory that cannot be
     searched refuses every rename into it),
     and whether every installed binary is owned by this user (with
     `fs.protected_hardlinks` on, the swap's hard-linked backup of a file you do
     not own fails even in a writable directory);
   - whose the directory is (`stat`), and whether it is inside the user's home
     once symlinks are resolved — which decide the advice when either answer
     above is no;
   - whether `/proc/self/exe` ends in ` (deleted)` — replaced or removed while
     it ran.

   `install::action_for()` turns that into the banner, and the choice is logged:
   - **Replaced or removed while running** → *Quit*, through the hub's `quit`
     action, the one the cogwheel's *Quit* and `tobii uninstall` use as well.
     Nothing is at the path to update, and relaunching would hand off to this
     same process.
   - **Unowned, the directory writable and the binaries this user's** →
     *Update*, the path below.
   - **Unowned, not writable, and the directory this user's** (its write bit
     off) → no download (`FixFolder`): the banner shows `chmod u+w <dir>`,
     selectable, and its button quits, since the banner is decided once per
     launch and closing the window only hides the hub. Not sudo:
     `--system` there would put root's files, and a menu entry for every user,
     into one user's folder.
   - **Unowned, not writable, inside the home and another account's** (what
     v0.3.0's `sudo ./install.sh ~/.local/bin` made when that folder did not
     exist) → the same, with `sudo chown <uid>:<gid> <dir>` of that one folder,
     not of what is in it. The next start finds root's binaries in a folder of
     the user's, and offers the plain-install *Download* below.
   - **Unowned, but only an administrator can change the directory** →
     *Download* of the archive, and the `sudo ./install.sh --system <dir>` that
     installs it.
   - **Unowned, the directory this user's but the binaries someone else's** (a
     `sudo ./install.sh` into a home directory) → *Download* of the archive,
     and a plain `./install.sh <dir>`: the installer renames its files into
     place, and a rename in a directory you own ignores the old file's owner.
   - **Unowned, another account's binaries in a folder that is not this user's
     either** (a shared or sticky one) → the `--system` *Download*. In a sticky
     folder someone else owns, rename(2) refuses anyone but the file's owner or
     the folder's, so a plain install would stop at its first `mv`; and a
     group-writable system folder holds files every user runs, which a per-user
     install should not take over. The printed `sudo ./install.sh --system
     <dir>` is then refused by the installer itself, whose own rule is that
     `--system` writes only where root alone can change things; it names the
     folder and offers a plain install into `~/.local/bin` instead. Known, and
     recorded in Quality-and-Risks 11.3c.
   - **Owner unknown** (a package manager could not be asked) → *Update*, which
     `install_release` then refuses, saying which query failed. Overwriting a
     packaged file on the strength of a query that failed is the one outcome
     worth refusing outright.
   - **A package manager owns it** → *Download*. A folder picker, then
     `download_release_files` fetches the artifact matching that manager (the
     `.deb`, the `.rpm`, or the prebuilt Arch package — for a release without
     one, the `PKGBUILD` **and** its install hook) into a version-named
     subdirectory, digest-checked against `SHA256SUMS` exactly as the archive
     would be — and stops.

   Every *Download*, the two archive ones included, installs nothing, so it
   deliberately takes **no** `app.hold()`: there is no window of half-written
   binaries to protect. Before fetching anything it refuses a download folder,
   or an existing version folder, that another user could change
   (`UnsafeFolder`): the command it prints is run later, perhaps with `sudo`, on
   files that must still be the ones it checked.
4. On **Update**: `app.hold()` (so closing the window cannot kill the process
   between two renames), then an install thread. `install_release` asks step 3's
   questions again itself — `tobii update --install` has no banner in front of
   it — and refuses with the matching reason: for a folder the user can fix,
   the same command, then "run `tobii update --install` again". Run as root on
   binaries another account owns, it refuses and says to run it again as that
   account, without sudo (`RunWithoutSudo`): as root the swap would work, and
   leave root's files there.
5. `net::download` → digest against `SHA256SUMS` → `tar -xzf` → find the
   binaries **skipping symlinks** → **run each one with `--version`** → swap.
6. The swap: hard-link the old binary aside as a backup, then a single atomic
   rename over the target — so the path is never absent — and roll **all** of
   them back if any step fails.

### 6.6 Reconnect after unplug

No special path: the device thread's watchdog notices the transport has gone
quiet for 2 s, breaks the inner loop, and the outer loop retries `open` every
750 ms while demand persists. Every reconnect re-runs the full re-application
sequence from §6.1, which is why an unplug/replug is invisible to the user.

---

## 7. Deployment view

```
   Developer machine                     GitHub Actions
   ─────────────────                     ──────────────
   scripts/build.sh                      ci.yml     — fmt, clippy, tests,
     ├── checks deps, names what's         install-script tests,
     │   missing per distro                rc pre-release flag
     ├── cargo build --release             (debian:trixie container,
     │                                      pinned toolchain 1.98.0)
     └── --install → ~/.local/bin        release.yml — on a v* tag
         + desktop entry + icon            ├── the checks, on the tagged commit
                                           ├── builds in debian:trixie
                                           ├── ENFORCES the glibc floor
                                           ├── verifies SHA256SUMS
                                           ├── builds + installs the Arch pkg
                                           └── publishes a DRAFT release
                                         aur.yml — on a full release,
   User machine                            pushes tobii-linux-bin to the AUR
   ────────────
   ~/.local/bin/{tobii,tobii-gtk}    ◄────────────┐  tobii update --install
     (/usr/local/bin with --system)               │  or the hub's banner
   ~/.config/tobii-linux/…                        │  (BINARIES ONLY — the
   ~/.local/share/tobii-linux/installs            │   updater never touches
   ~/.local/state/tobii-linux/tobii.log           │   config or the rule)
   $XDG_RUNTIME_DIR/tobii-linux/icons             │
   ~/.config/autostart/…  (optional)              │
   /etc/udev/rules.d/60-tobii.rules  ─────────────┘
```

`installs` is the installer's manifest — the directories `install.sh` put
binaries in, `/usr/local/share/tobii-linux/installs` for `--system`. It is a
hint for `tobii uninstall`, which checks every binary it names before
touching it. A directory it lists that cannot be looked at (no permission to
search it, say) keeps its line and is reported as `cannot be looked at`, not
taken for empty: that line may be the only record of a `--lean` install.
`icons` is the tray's private copy of its icon, rewritten only when it differs,
and gone with the session.

**Why the container matters.** The machine a binary is compiled on *is* its
compatibility floor. Built on a current rolling distribution both binaries
require `GLIBC_2.44`; Ubuntu 24.04 has 2.39 and Debian 13 has 2.41 — such a
release would fail to start for nearly everyone. Worse, it would fail *loudly*:
the updater's pre-install probe would get a dynamic-linker error and report
"built against newer system libraries than this machine has". CI builds in
Debian 13 and **fails the release** if the floor rises above `MAX_GLIBC`.

**Runtime requirements.** GTK 4 + `gtk4-layer-shell` (GUI only), libusb 1.0,
`curl` or `wget` and `tar` (model and update downloads), a udev rule for
non-root device access, and a Wayland session for the gaze overlay.

---

*Continue to [[Architecture-Decisions]].*
