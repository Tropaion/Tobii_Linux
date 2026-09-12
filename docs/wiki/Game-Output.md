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

## The composition order is load-bearing

Two orderings inside the pipeline are not obvious and are the difference between
tracking that feels right and tracking that feels broken:

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
  screen.

Tracking loss longer than a second discards the smoothing state entirely:
resuming from a second-old pose swings the camera across the room.

Where the pipeline is the one deriving the pose — the 5-DOF path, with no fresh
neural pose — a frame carrying only **one** tracked eye still produces one: the
missing eye is placed at the last measured interocular offset, for up to 300 ms.
Rotation is *held* at its last measurement there; only translation follows the
surviving eye. `FramePipeline::fallback_stats` counts how many poses a session
owes to that, though nothing displays it yet. See [[Head-Pose]] and
[[Quality-and-Risks]] §11.3d — none of this has been run against a tracker.

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

## X-Plane 11/12 — already works, no extra sink

The X-Plane plugin Linux users actually run is
[`amyinorbit/headtrack`](https://github.com/amyinorbit/headtrack) (MIT), whose
own source calls itself "an implementation of a basic OpenTrack protocol
receiver". It binds UDP `0.0.0.0:4242` and reads a 48-byte payload of six
`double`s, then writes `sim/graphics/view/pilots_head_{x,y,z}` in **centimetres**
and `pilots_head_{psi,the,phi}` in **degrees**.

That is byte-for-byte what our opentrack sink already emits, on the port we
already default to. So:

```sh
tobii games set enabled true      # opentrack sink is on by default at 127.0.0.1:4242
```

install the plugin, and it works — no opentrack, no bridge, no new code.

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
```

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
the game's session means reproducing Proton's entire launch environment.

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

### [UNTESTED] Flatpak and Snap Steam

`--steam` finds a Flatpak Steam prefix (`~/.var/app/com.valvesoftware.Steam`)
and will install into it. What is *not* verified is the other half: inside the
sandbox the host's `tobii` is not on `PATH`, and `$XDG_RUNTIME_DIR` is the
app's rather than the host's, so the `tobii game -- %command%` launch option
this prints may not resolve or may not reach the hub's socket. If you run
Flatpak Steam, `flatpak-spawn --host tobii game -- %command%` is the shape to
try first. Whether the sandboxed game sees the uinput device, and whether the
DLL's loopback bind lands in the host's namespace, are both unmeasured.

### Which wine writes the registry matters

`install` uses the Proton build recorded in the prefix's own
`compatdata/<appid>/config_info`, not whatever `wine` is on `$PATH`. A prefix
records the version that built it, and a different wine touching it runs
`wineboot -u` and upgrades it — so reaching for the system wine to write two
registry values could rewrite a Proton prefix out from under the game that owns
it.

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
