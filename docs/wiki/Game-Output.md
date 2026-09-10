# Game Output

How head pose and gaze reach a game. Four routes, in order of how much they ask
of the user.

| Route | Needs installed | Reaches |
|---|---|---|
| **Virtual joystick** | nothing | native Linux games, Proton games, emulators — anything that binds an **absolute** view axis (see the rate trap below) |
| **opentrack UDP** | opentrack | whatever opentrack is configured to drive |
| **FreeTrack (Wine)** | a Wine prefix + our bridge | Windows games that speak FreeTrack |
| **TrackIR (Wine)** | a Wine prefix + a signed client DLL | Windows games that speak TrackIR |

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

Tracking loss longer than a second discards the smoothing state entirely:
resuming from a second-old pose swings the camera across the room.

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
instead. A known-value pose survived the whole chain into SDL exactly: yaw +90°
of ±180 arrived as `16384` (half scale), pitch −45° of ±90 as `−16385`, gaze at
the right-hand screen edge as `32767`.

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

The axis range is `0..65534` centred on `32767` — an even span, so the two
halves are exactly equal. An axis whose halves differ by one step reads as a
permanent fractional offset in anything that normalises to `[-1, 1]`, which
presents as slow drift with nothing to point at as the cause. Confirmed through
`joydev`, which rescales to `[-32767, 32767]` and reported every axis at exactly
`0` at rest.

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
already-installed client DLL (opentrack's, if present) and supplies the data
behind it from our own provider; `--npclient ours` overrides that for a game
that does not check.

**[UNKNOWN]** Two things about our own `NPClient64.dll` are unmeasured because
no game has yet consumed it:

* `NP_QueryVersion` reports `4.00`. The established implementation reports
  `5.00`, and TIR5 additionally requires a checksum computed over the head-pose
  data and relayed to the game, which we do not compute. Claiming 5.00 without
  the checksum may be worse than claiming 4.00; this has not been tested either
  way, so it is left alone.
* The per-axis **signs** of the TrackIR encoding, and whether a game ignores a
  frame whose `wPFrameSignature` did not change.

## Clean room

The clean-room claim in the README covers the **ET5's USB protocol**, which was
derived from captured traffic and nothing else. It does not cover the
TrackIR/FreeTrack ABIs or the evdev/uinput interface: those are public
interfaces, and interoperating with them means matching facts about them that
are not ours to invent. Where opentrack was read for such a fact, the file that
uses it says so and says which fact — see
[`sinks/uinput_joystick.rs`](../../crates/tobii-output/src/sinks/uinput_joystick.rs).
No opentrack code was copied.
