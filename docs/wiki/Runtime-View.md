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
   the original does.
5. **Computing** → compute → retrieve → persist to `calibration.bin`.

### 6.4 `tobii headpose` to a game

Its own process, its own connection — which is only possible because the hub
releases the device when unfocused.

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
   they were.
7. Encode nine `f64` little-endian into the opentrack datagram and send.
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
3. On **Update**: `app.hold()` (so closing the window cannot kill the process
   between two renames), then an install thread.
4. `net::download` → digest against `SHA256SUMS` → `tar -xzf` → find the
   binaries **skipping symlinks** → **run each one with `--version`** → swap.
5. The swap: hard-link the old binary aside as a backup, then a single atomic
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
   scripts/build.sh                      ci.yml     — fmt, clippy, 558 tests
     ├── checks deps, names what's         (debian:trixie container,
     │   missing per distro                 pinned toolchain 1.98.0)
     ├── cargo build --release
     └── --install → ~/.local/bin        release.yml — on a v* tag
         + desktop entry + icon            ├── builds in debian:trixie
                                           ├── ENFORCES the glibc floor
   User machine                            ├── verifies SHA256SUMS
   ────────────                            └── publishes a DRAFT release
   ~/.local/bin/{tobii,tobii-gtk}
   ~/.config/tobii-linux/…                        │
   ~/.config/autostart/…  (optional)              │  tobii update --install
   /etc/udev/rules.d/99-tobii.rules  ◄────────────┘  or the hub's banner
```

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
