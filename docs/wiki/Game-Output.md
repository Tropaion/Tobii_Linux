# Game Output

How head pose and gaze reach a game. Four routes, in order of how much they ask
of the user.

| Route | Needs installed | Reaches |
|---|---|---|
| **Virtual joystick** | nothing | native Linux games, Proton games, emulators — anything that binds an **absolute** view axis (see the rate trap below) |
| **opentrack UDP** | opentrack, *or nothing* | whatever opentrack drives — **and X-Plane 12 directly**, see below |
| **FreeTrack (Wine/Proton)** | one `tobii bridge install` | Windows games that speak FreeTrack |
| **TrackIR (Wine/Proton)** | that, plus a signed client DLL | Windows games that speak TrackIR |

All four are fed from the same [`FramePipeline`](../../crates/tobii-output/src/pipeline.rs)
and fanned out by the same `Router`, so no two of them can disagree about what a
blink does or when Extended View contributes.

## Two processes can feed those routes, and only one of them needs a wrapper

The pipeline and the `Router` are a library, and **two front ends drive them**:
the hub, and `tobii headpose`. Which one is running decides whether anything has
to wrap the game.

**`tobii headpose` is self-contained.** `crates/tobii-cli/src/main.rs` opens the
device itself (`UsbTransport::open`, then `Connection::connect`), builds a
`Router` with the opentrack sink — and the bridge sink and joystick, where
`games.toml` asks for them — and streams until it is stopped. The tracker is lit
for exactly as long as the command runs, because that process holds the USB
session. Where a hub is running it asks that hub to stand down first
(`lease_the_tracker`, `crates/tobii-cli/src/main.rs:2115`) rather than failing
under it, and gives the device back when it ends; with no hub it opens the
device directly and waits for nothing. For the **opentrack UDP** route this is
the whole story: opentrack's
"UDP over network" input, or a direct listener like X-Plane (below), receives
the datagram with no hub running and nothing wrapping the game.

**The hub is how the other three routes are fed, and how all four are fed at
once.** Only one process can claim the ET5 over USB, so a joystick, a Wine
bridge and an opentrack stream cannot be three programs; they are one, and the
hub is the one that also owns the calibration, the display area and the
settings.

What the hub cannot work out by itself is *when*. The USB session is
reference-counted (`Demand`, see [[Runtime-View]] §6.2), and the things that
take a count are the hub window while it has focus, the gaze overlay, a
calibration or setup flow, and **a socket client**
(`crates/tobii-gtk/src/outputs.rs`: `Holds::hello` takes a `DemandGuard` for a
client subscribed to pose, gaze or camera). A game cannot take a count itself:
it speaks opentrack or TrackIR or evdev, not this program's socket.
`GameOutput::from_config` opens sinks and takes no guard either, so **switching
game output on does not, by that route, ask for the device**.

That gap is the whole reason `tobii game` exists. It transports nothing: it
connects to the hub's socket, subscribes to pose for the lifetime of the child
process, and drops the connection when the child exits — the wrapper is a
`DemandGuard` with a launcher's sense of timing. Any program that holds that
subscription wakes the tracker the same way — but only `tobii game` also puts
`TOBII_BRIDGE_PORT` into the child's environment (`main.rs:201-203`), which is
the only way the Wine-side DLL learns a non-default `bridge_port`.

Which left, through v0.4.0, exactly one route that worked unattended — the
opentrack watch below — and three that did not. The two that close the rest of
the gap are not observations; they are the user's standing answer, and they are
held by `GameSide` on the device thread rather than by the socket thread
(`crates/tobii-gtk/src/device.rs`). That placement is deliberate:
`outputs::spawn` returns *before* it spawns its thread when `Server::bind()`
fails, so a hold parked there would be missing precisely on the second
a second hub, while `GameSide` already re-reads `games.toml` once a second in
both the idle wait and inside a session, and already owns the joystick handle.

* **`wake_for_joystick`** (default on) — `GameSide::sync_joystick` ends by
  syncing a guard labelled *the virtual joystick*, taken when
  `have_device && cfg.wake_for_joystick && enabled && joystick`. Gated on the
  handle actually existing, not on the settings alone: a `/dev/uinput` that
  refused leaves nothing for a game to bind, so it must cost no sessions.
* **`keep_awake`** (default off) — a guard labelled *standby turned off in the
  settings*, synced by `GameSide::sync_keep_awake` from the same once-a-second
  `apply`, and gated on nothing at all. It holds with game output off, with no
  sink configured, and it covers the gaze overlay too. It does **not** cover
  `tobii headpose`: that is a separate process holding its own USB session, and
  the relationship is the opposite of coverage — `keep_awake` is what makes the
  hub's claim permanent, which is why `tobii headpose` has to ask for the lease
  and take the device off it.

Neither reason is in `EXCLUSIVE`. These are the first claims the hub holds on
its own behalf that never end by themselves, and an exclusive one would
permanently refuse leases, recentres and calibration to exactly the users who
turned the setting on — with a "busy with" message naming something they cannot
see. The device thread's idle wait is now the named
`must_wait(demand_active, pending_empty, lease_blocks)`, so the lease override
is provably an override and not a weighing: a lease still takes the device from
a permanent claim, and a queued command still opens a session.

**One receiver announces itself, and the hub listens for that.** A socket bound
to the opentrack destination appears in `/proc/net/udp` (and `udp6`), so
`tobii_output::listener::probe` looks for one and `PortWatch` in
`crates/tobii-gtk/src/outputs.rs` holds a `DemandGuard` while it is there,
polling once a second rather than at the socket loop's 50 ms. Two sockets on
that port are skipped, because neither could receive our datagrams: one that
has `connect`ed to a peer, and one of our own sinks — our inode, bound to a
wildcard — which the kernel can hand the configured port whenever nothing else
holds it, and which would otherwise hold the tracker on for itself for ever.
`probe` answers `Yes`, `No` or `Unknown(why)`, and `Unknown` — a configured
address this host cannot see, or a platform without `/proc` — never takes a
hold, so it degrades to the old behaviour instead of guessing. The key is
`wake_for_opentrack`, default on, and it does nothing until game output is
`enabled` and an opentrack address is set. The trade is that a bound socket is
not a request — opentrack left open on a second monitor is indistinguishable
from opentrack feeding a game, so the illuminators stay lit until it is closed.

**The joystick has no equivalent question, and this was measured rather than
assumed.** The kernel exports no open count for a uinput node, and the
processes that hold one open are not a signal: a faithful replica of our device
was opened within 30 ms by `joystickwake`, by Chrome probing gamepads on
hotplug, and by `winedevice.exe` for the life of a Wine prefix; across 30
samples it never had zero openers, and a real reader would appear as one more
identical row in `/proc/*/fd`. Reader detection is therefore not implemented
and should not be re-proposed — the refutation is written into
`wants_joystick_wake`'s doc comment so the next person to think of it finds it
next to the code. What `wake_for_joystick` costs instead is stated plainly:
with it on, the illuminators stay lit for as long as game output is on, not for
as long as a game is running. `wake_for_opentrack`'s hold ends when another
program exits; this one ends when the user unticks something. The Wine bridge
gets neither — its listener is inside the prefix — which is the case
`keep_awake` exists for, and the case `tobii game` served all along.

## The composition order is load-bearing

These orderings inside the pipeline are not obvious and are the difference
between tracking that feels right and tracking that feels broken:

* **Compose, then filter once.** Smoothing the head pose and the gaze
  contribution separately leaves the two out of phase, so a fast look arrives
  before the head movement that accompanied it.
* **Hold through a blink.** Gaze vanishes for 100–400 ms every few seconds.
  Treating that as "look straight ahead" makes the camera lurch on every blink,
  so the last Extended View offset is held and decayed instead.
* **Hold a sample that teleports.** The filter is no longer a plain EMA: a
  finite sample whose *position* has moved more than **150 mm** from the last
  accepted raw sample is refused, at most three frames in a row. 150 mm at the
  measured 30.208 ms frame interval is 4.97 m/s, so it catches a teleport and
  not a lunge. Position only — the pose reaching the filter already carries the
  Extended View term, which moves at saccade speed, so any angular gate tight
  enough to catch a rotation glitch would reject a legitimate look across the
  screen. The 150 mm is the default of the `filter_max_step_mm` key, not a
  constant.
* **Recentre rotation *before* `compose`, translation *after* it.** The two
  references are taken at opposite ends of the pipeline on purpose. Translation's
  neutral goes out after Extended View, because Extended View measures gaze
  against the screen's corners in the tracker's frame and moving the head to the
  origin first collapses that angle to nothing. A rotation reference
  (`--recenter`, the hub's button, `Msg::Recentre`) is taken before, because
  after `compose` the angle also carries the gaze-driven Extended View term — and
  a reference caught during a glance at a screen edge would be permanent, since
  nothing decays it. Pitch is in neither: it has its own measured zero from
  `tobii headpose --calibrate-pitch`.

Tracking loss longer than a second discards the smoothing state entirely:
resuming from a second-old pose swings the camera across the room.

Where the pipeline is the one deriving the pose — the 5-DOF path, with no fresh
neural pose — a frame carrying only **one** tracked eye still produces one: the
missing eye is placed at the last measured interocular offset, for up to 300 ms.
Rotation is *held* at its last measurement there; only translation follows the
surviving eye. `FramePipeline::fallback_stats` counts the frames the tracker
delivered with one eye against those it delivered with two — the geometry is run
on every frame, so the ratio describes the device rather than which path won —
and `tobii headpose` prints it at the end of its rate line as `, one eye N%`,
`(now)` while the pose at that instant is a reconstruction. `--check` carries
the same fragment on its `eyes` line. Nothing else reads it: the hub and
`tobii debug` show nothing. See [[Head-Pose]] and [[Quality-and-Risks]] §11.3d —
none of this has been run against a tracker.

## Virtual joystick

`/dev/uinput`, eight absolute axes, no other software. Because it is an ordinary
evdev joystick, SDL reads it, the legacy `/dev/input/js*` interface reads it,
and Wine's `winebus` enumerates it — so a Wine or Proton game sees a normal
controller. Measured, rather than inferred from documentation:

| consumer | result |
|---|---|
| SDL2 2.32 | 8 axes, 2 buttons, correct name; `SDL_IsGameController` false — it is a joystick, not a gamepad, which is what flight and space sims read |
| joydev (`/dev/input/js*`) | every axis exactly `0` at rest |
| Wine 11.17 DirectInput8 | enumerated as `DI8DEVTYPE_JOYSTICK` |
| Wine 11.17 XInput | **nothing**, in all four slots, with and without the device present |

XInput's fixed two-stick layout has nowhere to put eight axes, so a game that
speaks only XInput cannot use this sink and needs the Wine bridge or opentrack
instead. A known-value pose survives the whole chain into SDL exactly, with the
response stage in place: composed yaw **+35°** — half of the 70°
`joystick_yaw_full_deg` default — arrives as `16384`, half deflection; pitch
**−17.5°** of the 35° default as `−16385`; gaze at the right-hand screen edge as
`32767`.

| Axis | Carries | Full scale |
|---|---|---|
| `ABS_X`, `ABS_Y`, `ABS_Z` | head displacement from where you sit | ±500 mm |
| `ABS_RX`, `ABS_RY`, `ABS_RZ` | yaw, pitch, roll | ±180°, ±90°, ±180° |
| `ABS_THROTTLE`, `ABS_RUDDER` | gaze on screen, x and y | the whole screen |

### How much head movement reaches full deflection

The axis *means* ±180°/±90°/±180°, because that is the physical range of the
quantity and what every head-tracking wire format assumes. A head does not cover
it. With Extended View at *Normal* and a 20° head turn, composed yaw reaches
about 65° — **36%** of the axis. Roll gets no Extended View contribution at all,
so a 15° head tilt is **8%**.

opentrack and TrackIR both have an amplification stage before the wire for
exactly this reason: opentrack's docs describe mapping 15° of physical yaw onto
90–180° of camera rotation, and NaturalPoint tell TrackIR users to shape the
motion curve rather than move their heads further. We had copied opentrack's
wire scale and not its curve.

```sh
tobii games set joystick_yaw_full_deg 70      # the default
tobii games set joystick_pitch_full_deg 35
tobii games set joystick_roll_full_deg 20
```

The number is the composed head angle that reaches the end stop. The defaults
put "look at the edge of the screen and turn your head slightly" at roughly full
deflection. **This is the first thing to tune with a real game in front of you.**

Erring hot is deliberate: every game with an axis-tuning panel can attenuate a
strong signal trivially, and several cannot amplify a weak one at all — Elite
Dangerous exposes a per-axis deadzone and nothing else.

The mapping is linear with a hard clamp, not an eased curve. Easing would
flatten the response either side of centre as well as at the ends, and a soft
centre is a deadzone by another name, sitting exactly where the user is looking.

**Only the joystick gets this stage.** The rule is: shape it where we are the
last stage, send it raw where something downstream will shape it. opentrack
receives us as a *tracker* and applies its own mapping curves, which its users
have already tuned — amplifying first would double-apply and silently break
their profiles. The Wine bridge stands in for the TrackIR software, which is
what shapes the signal before a game sees it, so it arguably wants this too; it
is deliberately left raw until a real game has consumed that path, rather than
changing its feel and bringing it up at the same time.

Translation full scale is deliberately the same ±500 mm the TrackIR encoder
saturates at, so a movement of a given size means the same thing on every output
this program has. opentrack's equivalent is ±1 m; matching *it* would have made
our two outputs disagree with each other.

The axis range is `0..65534` centred on `32767`, because `encode_axis` emits
`centre ± centre`: an odd span would declare a maximum the encoder can never
reach. It is **not** chosen for centring — the two consumers disagree about
that, each by a single step. joydev rescales about `(min + max) / 2` and reports
exactly `0` at rest for this span (the rejected pairing, max 65535 centred
32768, reports `1`); SDL maps onto its own asymmetric `[-32768, 32767]` and
reports `-1` at rest either way. One step in 32767 is about 0.005° of a ±180°
axis, far below any deadzone or the tracker's own noise.

### Why the device declares a key capability it never uses

opentrack's source says only *"do not remove next 3 lines or udev scripts won't
assign 0664 permissions"*. That is not a mechanism, so it was measured here —
four builds of the device, reading udev's verdict out of `/run/udev/data/`:

| built with | udev says |
|---|---|
| `EV_KEY` + `BTN_TRIGGER`/`BTN_THUMB` + all eight axes | `ID_INPUT_JOYSTICK=1` |
| `EV_KEY` declared, **no** key codes | `ID_INPUT_JOYSTICK=1` |
| `EV_KEY` + buttons, only `ABS_X`/`Y`/`Z` | `ID_INPUT_JOYSTICK=1` |
| **no `EV_KEY` at all** | `ID_INPUT_ACCELEROMETER=1`, `IIO_SENSOR_PROXY_TYPE=input-accel`, `SYSTEMD_WANTS=iio-sensor-proxy.service` |

So the load-bearing thing is the bare `EV_KEY` capability, not the button codes.
Without it, `input_id` reads `ABS_X`/`Y`/`Z` as an accelerometer and hands the
device to `iio-sensor-proxy` — the service that rotates laptop screens. Given
`EV_KEY`, either the button codes **or** `ABS_RX`/`RY`/`RZ` independently
satisfy the joystick test, and this device has both.

The buttons are kept anyway and never pressed: SDL skips anything not tagged
`ID_INPUT_JOYSTICK`, and Steam's container runtime has a fallback that
classifies straight from evdev capabilities when udev properties are
unavailable, which does want a code in the `BTN_JOYSTICK..BTN_GAMEPAD` range.
Cheap insurance for a case that cannot be reproduced outside a container.

Nobody should "tidy" the buttons away later on the grounds that the axes alone
classify correctly — they do, and the container path still wants the buttons.

### The rate trap: binding is not working

The most important thing to know before binding an axis. Many games' "look"
axis is a **rate** input — the camera keeps rotating for as long as the axis is
deflected — not a position. Bind a head tracker to one of those and the view
spins away and never comes back, faster the further you turn your head.

The bind takes. The axis moves in the game's test display. It still does not
work. Games where the joystick route is genuinely fine name it explicitly:
Elite Dangerous has *Headlook Axis Mode: **Direct***, which exists precisely
because its default is incremental. Where a game offers only a rate axis
(BeamNG's `evdev` binding, X-Plane's "View left/right", ETS2's UI-bindable
`j_look_lr`), the joystick sink cannot drive the view no matter how it is
tuned — those need the Wine bridge, a native protocol, or a config-file edit.

### Steam Input can take the device away, silently

Steam's controller layer enumerates anything that looks like a joystick. For a
device it does not recognise it either presents it to the game as a generic
Xbox pad — discarding every axis that does not fit that shape and adding a dead
zone — or hides it from the game with no replacement. Both look from inside the
game exactly like "the Tobii controller is not in the bind list", and both are
on the default path for a Steam-launched game.

The fix is per-game: **Properties → Controller → Disable Steam Input**. Failing
that, Settings → Controller → turn off generic-gamepad support, or launch the
game outside Steam.

This is also a second, independent reason the device follows the setting rather
than the tracking session: Steam does not reliably hot-plug uinput devices, so
the device has to exist before Steam looks.

### The device follows the setting, not the tracking session

Every other sink is a UDP socket, and a socket that comes and goes is invisible
because nothing enumerates it. A joystick is enumerated, by name, and a device
whose lifetime is one tracking session breaks in both directions:

* A game builds its bind list when it launches. One that does not watch for
  hot-plug never sees a device created afterwards — and the tracker is dark
  until something asks for it, so "afterwards" is the normal case.
* A game that *does* watch for hot-plug shows a **controller disconnected**
  prompt a few seconds after the user stops looking at the screen.

So the hub creates one device while game output and the joystick are both on,
and hands each tracking session a handle to it. Verified on the running hub:

```
before the hub starts          (no device)
hub running, tracker IDLE      event22 js0     <- the case that matters
tobii games set joystick false (no device)
tobii games set joystick true  event22 js0
after the hub exits            (no device)
```

The setting is re-read once a second while the hub is idle, so ticking the
checkbox makes the controller appear without waiting for anything to wake the
tracker.

`tobii headpose` is the exception and deliberately so: it is a foreground
command, so its device lives exactly as long as the command does — and it
declines to create one at all when the hub already has one, because both would
use the same name, vendor and product. Two identical entries in a bind list of
which only one moves is worse than one, especially since while `tobii headpose`
holds the tracker, the hub's is the frozen one.

### Permissions

Creating the device needs write access to `/dev/uinput`, which is root-only on
most distributions. The packaged `60-tobii.rules` grants it the same way Steam,
KDE Connect and Logitech's own rules do. That is a real capability — any program
running as the logged-in user can then synthesise keystrokes and pointer motion
— so the rule is one clearly-marked line that can be deleted, and everything
except the virtual joystick still works without it.

`tobii debug` separates the three ways this fails, because from inside a game
they are indistinguishable (the controller simply is not in the bind list):

```
uinput           writable — the virtual joystick can be created
uinput           /dev/uinput MISSING — the uinput module is not loaded …
uinput           /dev/uinput NOT WRITABLE — … install 60-tobii.rules and log out …
```

## X-Plane 11/12 — no extra sink needed

The X-Plane plugin Linux users actually run is
[`amyinorbit/headtrack`](https://github.com/amyinorbit/headtrack) (MIT), whose
own source calls itself "an implementation of a basic OpenTrack protocol
receiver". It takes that protocol on UDP port **4242** — a 48-byte payload of
six `double`s — then writes `sim/graphics/view/pilots_head_{x,y,z}` in
**centimetres** and `pilots_head_{psi,the,phi}` in **degrees**.

That is byte-for-byte what our opentrack sink already emits, on the port we
already default to. So install the plugin and run:

```sh
tobii headpose                    # sends to 127.0.0.1:4242 until you stop it
```

and it should work with no opentrack, no bridge, no hub and no new code — the
plugin documents that port and that payload, and the sink already emits both.
**This has not been observed here:** no X-Plane has been run against it, the
same gap §11.3e records for opentrack. The plugin binds the port and this
command sends to it; nothing else is in the path.

Through the hub instead — worth it when the same session should also drive a
joystick or the Wine bridge — the sink is on by default and only has to be
switched on:

```sh
tobii games set enabled true      # opentrack sink is on by default at 127.0.0.1:4242
```

That needs no launch option either, and with `wake_for_opentrack` on (the
default) it should need no wrapper: the hub lights the tracker while X-Plane
holds that socket and lets go within a second of X-Plane closing it, the tracker
going dark after the usual three-second linger. What is *known* is the port,
4242, which the plugin documents. The watch answers for either address that
matters on it — a wildcard bind, or the configured loopback address itself,
since a wildcard counts as a listener on that address, which is what
`listener.rs`'s `a_wildcard_bind_counts_as_listening_on_loopback` pins. Which of
those the plugin actually does has **not been observed here**: no real X-Plane
has been watched binding the socket, the same caveat §11.3e carries for
opentrack. Should it bind some third interface, `probe` never matches and this
route wants a wrapper after all. It wants one for certain where the watch cannot
answer at all: turned off, or an opentrack address on another machine, where
`probe` says `Unknown` and takes no hold — and where the alternative to the
wrapper is `keep_awake`. The standalone `tobii headpose` route
needs neither, because it holds the USB session itself — taking it from the hub
where there is one, by asking for the lease first (`lease_the_tracker`,
`crates/tobii-cli/src/main.rs:2115`).

This matters because the virtual joystick genuinely *cannot* reach X-Plane:
Laminar's own developer documentation is explicit that a joystick axis cannot be
bound to a dataref, and the built-in "view left/right" assignments pan while
deflected and recentre on release — a rate, not an angle. See the rate trap
above.

## The Wine bridge

```sh
tobii bridge games                      # what is installed, and what has a prefix
tobii bridge install --steam elite      # by name, or by app id
tobii bridge install --prefix /path/to/prefix   # anything not Steam
tobii bridge status --steam elite       # what is in that prefix right now
tobii bridge uninstall --steam elite    # take out what the install put in
```

`status` is the first thing to run when a game gets nothing: it names the
prefix and the wine it resolved, lists which of the three artifacts are in
`drive_c/tobii-bridge`, says what each discovery key holds and whether this
installer wrote it, and says whether a wineserver is serving the prefix. It
reports **what is registered, not whether a game will accept it** — the only
titles ever measured against NaturalPoint's signature check are Star Citizen
and Microsoft Flight Simulator 2024, and both stopped at it.

**It starts no process at all.** `wine reg query` is still `wine`, and wine
initialises or upgrades whatever prefix it is pointed at before it answers
anything: measured with wine 11.18 on throwaway prefixes, one `wine reg query`
created 5510 paths under a prefix that held only `drive_c`, and on a complete
prefix with a stale `.update-timestamp` — which is what a Proton prefix looks
like to the host's wine — it rewrote 2744 lines of `system.reg` and stamped the
prefix as its own. That is the upgrade the `--wine` warnings on this page are
about, and a command you run *because* something is already wrong must not be
the thing that changes it. So `status` reads the two discovery keys out of the
prefix's own `user.reg` instead, which also means the answer does not depend on
which `wine` it resolved.

The cost of that, which the report states where the values are: `user.reg` is
the registry as it was last written back, and a prefix writes its registry back
when the last process on it exits. While a game is running, a key registered
since it started is not in the file yet. The report says so whenever it finds a
live wineserver — and says nothing of the sort when it does not, because then
the file *is* the registry.

The report names the prefix as it is spelled on your machine — login name and
all — because the undo command it ends with is only any use spelled exactly.
`tobii debug`, the other half of what an issue wants, folds those paths away;
this one cannot. Read it through before you paste it.

**The two commands that can end without writing read the same file first.** An
`install` that refuses, and an `uninstall` with nothing of ours to remove, used
to reach for `wine reg query` to find that out — and so upgraded the prefix they
had just declined to touch (measured the same way: the refusal created no
directory and wrote no key, and still moved `.update-timestamp` and rewrote 2764
lines of `system.reg`). Both now decide from `user.reg` whenever the wineserver
lock says nothing is serving the prefix, which is the case where the file *is*
the registry; wine is started only once the run has decided it is going to write
— something it was going to do anyway (`settled_keys`,
`crates/tobii-cli/src/bridge.rs:1091-1107`). Measured on the merged tree, with a
`wine` that wrecks the prefix if it runs at all: a refused install and a no-op
uninstall leave the prefix byte-for-byte as it was and never spawn it
(`a_refused_install_runs_no_wine_and_leaves_the_prefix_as_it_was`,
`an_uninstall_with_nothing_to_remove_runs_no_wine`). An install that goes
through still runs wine to write the keys, and `run` is a wine process by
definition.

The file is not consulted where it could not answer safely: a `user.reg` that
cannot be read whole, or a prefix something is serving — where a wineserver is
holding registry changes in memory — falls through to wine, because a command
that *acts* cannot hand the lag to the reader the way the report does.

### Reading `user.reg` is a second implementation of wine's reader

`crates/tobii-cli/src/userreg.rs` is the whole of it: one function that finds
one value under one key in the bytes of a `user.reg`. It exists because the
accurate way to ask — `wine reg query` — costs the prefix, and it is a
reimplementation, so the rule it is written to is that **a value it cannot read
exactly is never reported as an absence**. "Nothing is registered here" is the
answer whose next move is to write; "there is something here I cannot read" is
the answer that makes `install` refuse and `uninstall` keep its hands off. Every
shape it does not fully decode comes back as the second.

What it agrees with wine about was measured against wine 11.18 one case at a
time, not inferred from the format:

* The first line must be exactly `WINE REGISTRY Version 2`. With that line
  missing, BOM'd, indented, lower-cased or renumbered, wine loads **no `HKCU`
  at all** and answers every query in that prefix with "key not found" — so a
  parser that skipped to the sections read a registration no process in that
  prefix could see (`userreg.rs:100-110`, checked in `lookup` at
  `userreg.rs:126-143`).
* Wine's loader applies the file top to bottom, so a key or a value spelled
  twice leaves the **last** one in memory. Measured four ways — section
  repeated, section re-cased, value repeated, value re-cased — all four answer
  the second value (`userreg.rs:146-153`).
* A section header written with one trailing `\` names the same key; a leading
  one, a doubled one or two trailing ones kill the wineserver outright, so
  nothing at all can be read out of such a prefix (`userreg.rs:264-284`).
* Wine reads indented headers and values, and tolerates whitespace on either
  side of the `=`, though its own writer produces neither
  (`userreg.rs:157-169`, `userreg.rs:296-310`).
* Names and values are unescaped the way wine writes them, including the rule
  that makes the escapes decodable at all: a `\x` escape is padded to four hex
  digits when the next character is itself a hex digit (`userreg.rs:54-61`).

One thing this reads that `wine reg query` could not: a path with characters
outside ASCII. `reg.exe` prints in the console's OEM codepage, so those bytes
were a guess and the old path refused them; `user.reg` names the code point, so
the character is decoded exactly and shown.

### Every subcommand refuses what it does not read

`tobii bridge install --help` used to ignore the flag and perform a real install
into `$WINEPREFIX` or `~/.wine`, writing both discovery keys. Each subcommand
now names the flags it reads, and anything else stops the command before a
prefix is resolved, a directory created or a key written (`reject_unknown_flags`
and `SUBS`, `crates/tobii-cli/src/bridge.rs:2969-2998` and `:3013-3031`).
Refused, checked on the built binary: an unknown `--flag`; `-h` and every other
single-dash token; `--prefix=PATH`, which would pass a name check and then be
ignored because every reader compares whole tokens; a flag with nothing after it; and **a bare
positional** — `tobii bridge install /games/pfx`, the most natural spelling of
all, which used to be dropped on the floor while the install went into
`$WINEPREFIX` or `~/.wine` and said so nowhere. A positional is refused rather
than read as a prefix path because the commonest way to produce one is not a
forgotten flag: `--steam Star Citizen` leaves `Citizen` standing alone.

Nothing has to be left running. The client DLL the game loads **receives the
tracking itself**, in a background thread inside the game's own process, and
publishes it into `FT_SharedMem` where the game reads it.

### Why the DLL feeds itself

The original shape was one `tobii-bridge.exe` per prefix, started by hand and
left running, with the DLLs as pure consumers. That works when you own the
prefix and can run things in it — and it does not work at all for a Steam game.

A Proton title runs under **its own wineserver**, with its own prefix, Proton
build and environment. A `tobii bridge run` started from a terminal with system
Wine is a different session: its `FT_SharedMem` is a different object in a
different server, and the game never sees it. Getting a second executable into
the game's session was long taken here to mean reproducing Proton's entire
launch environment.

(That last claim was this project's working assumption and has never been
measured from both sides. `run` now resolves the prefix's own Proton build
rather than the system wine, and the prefix's server directory is derived from
the prefix's inode, which a container shares — so *whether* a bridge started
from outside now joins the game's wineserver is genuinely open. It is written
up as unknown under [Ordering](#ordering-the-game-first-the-bridge-second).
What is no longer open is whether reproducing the environment is the only way
in: it is not, and
[A launcher that never fights the lock](#a-launcher-that-never-fights-the-lock)
below says what the alternative is. Nothing in this section depends on either
answer: the DLL feeding itself needs no second process at all.)

The DLL is already inside the game's process. So the receive loop lives there:
same wineserver by construction, no second process, no environment to
reproduce. Wine's winsock is a thin shim over host sockets, so a datagram from
the Linux hub reaches it directly.

Whoever binds the port first wins, and everyone else is a plain consumer. That
one rule covers a standalone provider already running, both of our DLLs loaded
into one game, and the ordinary single-DLL case.

`tobii-bridge.exe` is still installed, and is now a **diagnostic**: it gives you
a console that says what is arriving and what is being rejected, which is the
difference between "the game sees nothing" and "the game sees nothing *because
the frames never arrive*".

### [LIMITATION] 64-bit games only

We ship `freetrackclient64.dll` and `NPClient64.dll`. A 32-bit game asks for
`freetrackclient.dll` and `NPClient.dll` — without the `64` — and finds nothing,
so it gets no tracking while `tobii bridge install` reports success. opentrack
ships all four names side by side for exactly this reason.

That excludes a real part of the head-tracking audience: Falcon BMS, IL-2 1946,
and the FSX generation are 32-bit. Closing it means building the two client
crates for `i686-pc-windows-gnu` as well; the install directory and both
registry keys are shared, so nothing else about the design changes.

### [UNTESTED] Flatpak Steam — and Snap Steam is not looked for at all

`--steam` finds a Flatpak Steam prefix (`~/.var/app/com.valvesoftware.Steam/data/Steam`)
and will install into it. **Snap Steam it cannot find at all**: `STEAM_ROOTS`
(`crates/tobii-cli/src/bridge.rs`) is four paths — `.steam/steam`,
`.local/share/Steam`, `.steam/root` and the Flatpak one — and Snap's
`~/snap/steam/...` is none of them, so `--steam` never resolves a Snap library.
What is *not* verified is the other half of the Flatpak case: inside the
sandbox the host's `tobii` is not on `PATH`, and `$XDG_RUNTIME_DIR` is the
app's rather than the host's, so the `tobii game -- %command%` launch option
this prints may not resolve or may not reach the hub's socket. If you run
Flatpak Steam, `flatpak-spawn --host tobii game -- %command%` is the shape to
try first. Whether the sandboxed game sees the uinput device, and whether the
DLL's loopback bind lands in the host's namespace, are both unmeasured. For the
joystick route this matters less than it did: `wake_for_joystick` holds the
tracker from the host side, so a launch option that will not resolve inside the
sandbox no longer costs the tracker — the DLL and socket questions above are
unchanged.

### Which wine writes the registry matters

`install` uses the Proton build recorded in the prefix's own
`compatdata/<appid>/config_info`, not whatever `wine` is on `$PATH`. A prefix
records the version that built it, and a different wine touching it runs
`wineboot -u` and upgrades it — so reaching for the system wine to write two
registry values could rewrite a Proton prefix out from under the game that owns
it.

Since v0.5.0 `install` does not only prefer the right wine, it **refuses the
wrong one**. A `--steam` prefix was found through Steam's `compatdata`, so it
was made by a Proton build by construction and the host's wine is by
construction not that build: where which build made it cannot be read out of
it, `install` stops before anything is copied or any key is written and says
what would have happened (`refuse_unverified_wine_for_steam`,
`crates/tobii-cli/src/bridge.rs:1562`, called from `install` at `:1606`). Three
things get past it and nothing else does — `--wine <Proton>/files/bin/wine`, a
prefix whose own runner resolves, or `--force`. `run` and `uninstall` print the
same warning and go on: stopping an uninstall would strand somebody taking our
files back out, and `run` is a user asking in so many words to start wine there.

### The discovery keys are not ours to overwrite

`HKCU\Software\NaturalPoint\NATURALPOINT\NPClient Location` and
`HKCU\Software\Freetrack\FreeTrackClient` (`NP_KEY`/`FT_KEY`,
`crates/tobii-cli/src/bridge.rs:86,89`) are how *any* head-tracking client in a
prefix is found, ours included. So the installer treats them as shared state
rather than as its own:

* **`install` reads before it writes**, and **refuses** when a key holds
  something it cannot account for — `bridge.rs:1644-1663`, message built by
  `refusal` at `bridge.rs:1465`. The error names each key, what it holds and
  whose it looks like, and prints the exact
  `WINEPREFIX=… wine reg delete … /v Path /f` to clear it if it is stale. The
  refusal happens before anything is copied and before any key is touched, so a
  prefix comes out of it exactly as it went in. Reachable from an ordinary
  state: a Windows opentrack installed inside the prefix, or a v0.4.0 install
  whose Linux opentrack has since been removed.
* **What it will not refuse over** is a key that already holds, byte for byte,
  the value this run would write with nothing on record either way
  (`stops_install`, `bridge.rs:1168-1188`). That is precisely what v0.4.0 left behind on a
  machine with opentrack installed, and refusing there would refuse the upgrade
  path over a write that changes nothing, in a sentence blaming another program
  for a value this program wrote. The cost is stated in `is_ours`'
  **[LIMITATION]** (`bridge.rs:1138`, doc comment from 1109): such a key is then recorded as ours and
  comes out on the way out.
* **`--force` goes through and promises nothing.** The installer writes down
  only the values it wrote itself (`RECORD_FILE`, `registered.txt`), so there
  is no previous value to put back — the refusal text says so in those words.
  It prints what it replaced.
* **`uninstall` is the same rule from the other side** (`undo_keys`, `bridge.rs:2172`, deciding
  with `undo_for` at `bridge.rs:2093`): a key
  is deleted only while it still points at our own install directory or holds
  exactly what the record says we wrote; anything else is left in place and
  named. A blind `reg delete` here would take opentrack's own registration with
  it and leave the prefix with nothing registered at all — worse than before we
  touched it.

Every file `install` puts in the prefix — both DLLs, the exe and the record —
is written to a staging name beside the target and renamed into place
(`staging_name`, `bridge.rs:1275`, used at `1289` and `1688`). The name carries
the writing process's pid, `.<artifact>.<pid>.new`, so two installs into one
prefix cannot stage over each other, and a half-written record is never read
back as a record with a line missing.

### Verified

On a real Elite Dangerous Proton prefix, with **no provider running**: the game's
own load path (`LoadLibrary` on the registry-supplied directory, then
`GetProcAddress`) resolved all five FreeTrack exports, and a pose sent from Linux
as `yaw 7.5°, pitch −3.25°, roll 1.5°, (11, 22, 33) mm` read back through
`FTGetData` as `yaw=0.1309, pitch=-0.0567, roll=0.0262` radians and
`pos=(11.0, 22.0, 33.0)`, with `DataID` advancing.

## TrackIR and the signature

FreeTrack and TrackIR read the *same* shared memory block — `FT_SharedMem`,
guarded by a mutex named `FT_Mutext` (the typo is part of the protocol). What
differs is the client DLL the game loads and the units it hands back:
`freetrackclient64.dll` reports radians, `NPClient64.dll` reports degrees.

FreeTrack has no authentication, so our own `freetrackclient64.dll` works.

TrackIR does. `NP_GetSignature` fills two 200-byte buffers, `DllSignature` and
`AppSignature`, and a game compares what it gets against what it expects. The
established open implementations answer it from two byte tables XORed together
— NaturalPoint's own signature data, carried in obfuscated halves. That is why
scanning those DLLs for the string `NaturalPoint` finds nothing, and it is worth
stating plainly because the absence of that string looks at first like evidence
that the check is not real.

We do not ship it. `tobii bridge install` therefore points TrackIR at an
already-installed client DLL (opentrack's, if present); `--npclient ours`
overrides that for a game that does not check.

**That is the one configuration that still needs `tobii bridge run`.** A
third-party client is a pure consumer of `FT_SharedMem`. Our DLLs are what
create and feed that mapping, and a TrackIR-only game never loads ours — so
something has to fill it. `install` says so when it sets that up. FreeTrack
games need nothing running.

### Ordering: the game first, the bridge second

`tobii bridge run` is a `wine` process on the game's prefix, and that is enough
to stop the game from launching at all.

Steam launches a Proton title with the verb `waitforexitandrun`, and Proton's
launcher runs `wineserver -w` **before** it spawns the game executable.
`wineserver -w` is `fcntl(F_SETLKW)` on byte 0 of
`/tmp/.wine-<uid>/server-<dev>-<ino>/lock` (wine's `server/request.c`,
`wait_for_lock`), and a live wineserver holds that write lock for its whole
lifetime by design — `acquire_lock` takes it and deliberately never closes the
descriptor. So any wine process alive on that prefix means the game executable
is never reached. The launcher does not crash and does not complain; it sits
there. That is what gets reported as "the game freezes while the bridge is
running", and it is not specific to any title or to this project — opentrack
hits the same wall (opentrack#2211, an Arma 3 report).

Measured 2026-09-26: with one wine process holding a throwaway prefix,
`wineserver -w` timed out at 4 s (exit 124) and returned 0 the instant the
holder died.

**So the rule is: start the game, let it reach its menu, then start the
bridge.** Late is not too late. Measured the same day: opentrack's
`NPClient64.dll`, driven through the full handshake, polled `NP_GetData` 102
times with no `FT_SharedMem` present at all — all zeros — and then picked the
mapping up mid-run, reporting a correct pose, when a separate process created
and fed it. It does not cache the absence.

Two things enforce the rule rather than only documenting it
(`crates/tobii-cli/src/wineserver.rs`):

* **Before it starts**, `run` derives the lock path from a `stat` of the prefix
  and probes it with a non-blocking `F_GETLK`. Nothing holding it means the
  bridge is about to become the holder, and it says so in those words. Something
  already holding it is the supported order, and it says that instead.
* **While it runs**, `run` watches `/proc/locks` for a blocked waiter on that
  same lock file — the shape, measured, is a second line on the same inode with
  a `->` prefix naming the waiter's pid. On seeing one it stops, so the launch
  goes through, and says to start it again once the game is up. Matching that
  line needs the device `/proc/locks` prints, which is the *superblock's* and
  not the one `stat` reports: on btrfs `btrfs_getattr` hands `stat` the
  subvolume's anonymous device instead, and measured here `/`, `/home` and
  `/var/tmp` report `st_dev` 31, 53 and 57 while `/proc/locks` says `00:1d` for
  a lock on any of them. The device is therefore taken from
  `/proc/self/mountinfo` for the mount the lock file is on
  (`crates/tobii-cli/src/wineserver.rs:315-370`); comparing `st_dev` against
  that text missed every waiter on such a filesystem, silently, and the
  stand-down never fired.

Verified end to end on a throwaway prefix on 2026-09-26: with the bridge
running, a real `wineserver -w` blocked on the lock, the bridge saw the waiter,
stopped itself, and `wineserver -w` returned 0.

**[UNKNOWN]** Two things this does *not* establish, and no message in the code
claims either:

* Whether a `wine` started by `tobii bridge run` actually **joins** a
  containerised game's wineserver rather than merely contending with it. The
  cross-process proof used host wine on a host prefix. The Steam Linux Runtime
  shares the host `/tmp` (measured: same device and inode inside and out),
  which is why the lock contends at all — but the joining half is unconfirmed.
  If it turns out not to join, yielding still fixes the freeze and there is
  still no tracking from `bridge run`; starting the helper from inside the
  game's own session would then be the next thing to try.
* Whether any of this makes a game **use** the data. It only stops a second
  process from breaking the launch.

**And on the one configuration that needs it, what yielding leaves behind is an
empty mapping.** Our own client DLLs create and fill `FT_SharedMem` from their
own feeder thread and need nothing else running — that is the whole reason
`tobii-bridge.exe` stopped being a required artifact. The exception is the
configuration this section is about: TrackIR pointed at a third-party client,
which only *reads* the mapping, with our DLLs never loaded at all. There the
provider is the only thing that can fill it, and the provider is the process
that stands down — so from the moment `run` yields until somebody starts it
again by hand, that client is reading a mapping nobody is writing. The launch
is unblocked and the head tracking is absent. Those are two different outcomes
and this page does not merge them.

### A launcher that never fights the lock

The lock is not something to beat. A user's launcher for opentrack
(`https://github.com/markx86/opentrack-launcher`, GPL-3.0) arranges never to be
on the wrong side of it, and it is read here for mechanism only — nothing from
it is fetched, vendored or copied.

Its whole trick is ordering. It takes Steam's `%command%`, finds the trailing
`.exe`, and substitutes a three-line batch file that re-runs Steam's own
command otherwise unchanged:

```bat
start "" "Z:\...\helper.exe"
start /wait "" "Z:\...\game.exe" <args>
taskkill /IM helper.exe /F >nul 2>&1
```

Proton is then invoked exactly as Steam meant to invoke it: **one**
`waitforexitandrun`, therefore **one** `wineserver -w`, and both processes are
born after it has already returned. Neither of them can block it, because
neither of them exists when it runs. Three details carry the rest:

* `start /wait` keeps `cmd.exe` alive for exactly as long as the game, so
  Steam's own bookkeeping — playtime, the "Stop" button, the overlay — sees the
  process lifetime it expects.
* The helper is started *first* and unwaited, so it is up before the game asks
  for data.
* The `taskkill` is not tidiness. A surviving helper is precisely the wine
  process that would make the **next** launch's `wineserver -w` block, which is
  the freeze this whole page is about, one launch later.

**What this settles for us.** Our reading of the lock was right as far as it
went: `wineserver -w` runs before the game, it waits for every wine process on
the prefix, and a provider started beforehand is one. It was **incomplete as an
explanation**, because it was carried alongside the assumption that the only
way into the game's session is to reproduce Proton's launch environment. This
launcher reproduces nothing. It gets in by being inside the launch Steam was
already going to make.

**Reported, not verified here.** One user reports getting head tracking working
in Microsoft Flight Simulator 2024 by running opentrack's *Windows* build
inside the game's Proton prefix, sequenced with the game in a single Proton
launch, using that launcher. **Nobody on this project has run it**, with that
launcher or any other. It is recorded because it is the only account anywhere
in our notes of that title tracking at all — and because MSFS 2024 is one of
the two titles we have measured stopping dead at the signature check. It is not
a recommendation, it is not a supported route, and no part of this program will
set it up, fetch anything, or write anything into Steam.

**Still unmeasured, here, as of 2026-09-28:**

* That the reported configuration works — at all, or for the reason given. We
  have one user's account and no run of our own.
* Whether our provider, started this way, is seen by a client DLL in the same
  launch. The mechanism says it should be the same wineserver; nothing here has
  watched `FT_SharedMem` cross that boundary.
* Whether a game gated by the signature check then *uses* the data. An
  answering client and a filled mapping are two conditions, and both titles we
  measured failed at the first one with our own DLL.

### The provider no longer writes the registry unless asked

`tobii-bridge.exe` used to call `register()` on every start, writing both
discovery keys blind. That is wrong in exactly the configuration above: TrackIR
is pointed at a third-party client, the provider is started *because* of that,
and the write replaces that client's registration with `C:\tobii-bridge` —
taking away the thing the user set up, on every start or restart.

The default is now off; `--register` asks for it, as a repair for a prefix
whose keys were clobbered. `tobii bridge run` passes `--no-register` explicitly,
so that command cannot start a registering provider whichever way the default
ever moves. Note that an *older* `tobii-bridge.exe` already sitting in a prefix
ignores unknown flags and will still register — re-run `tobii bridge install`
to replace it.

**[UNKNOWN]** Two things about our own `NPClient64.dll` are unmeasured because
no game has yet consumed it:

* `NP_QueryVersion` reports `4.00`. The established implementation reports
  `5.00`, and TIR5 additionally requires a checksum computed over the head-pose
  data and relayed to the game, which we do not compute. Claiming 5.00 without
  the checksum may be worse than claiming 4.00; this has not been tested either
  way, so it is left alone.
* The per-axis **signs** of the TrackIR encoding, and whether a game ignores a
  frame whose `wPFrameSignature` did not change.

## Provenance

What the README claims about the **ET5's USB protocol** is that no Tobii code was
copied: the protocol was mapped from this project's own USB captures,
cross-checked against the third-party `tobiifree` project, with op *names* and
enum orderings read from a decompile of Tobii's software
([Reverse-Engineering-Methodology](Reverse-Engineering-Methodology.md) says which
source backs which claim). It does not cover the
TrackIR/FreeTrack ABIs or the evdev/uinput interface: those are public
interfaces, and interoperating with them means matching facts about them that
are not ours to invent. Where opentrack was read for such a fact, the file that
uses it says so and says which fact — see
[`sinks/uinput_joystick.rs`](../../crates/tobii-output/src/sinks/uinput_joystick.rs).
No opentrack code was copied.
