# Game Output

How head pose and gaze reach a game. Four routes, in order of how much they ask
of the user.

| Route | Needs installed | Reaches |
|---|---|---|
| **Virtual joystick** | nothing | native Linux games, Proton games, emulators — anything that binds an axis |
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
and Wine's `winebus` enumerates it — so a Proton game sees a normal controller.

| Axis | Carries | Full scale |
|---|---|---|
| `ABS_X`, `ABS_Y`, `ABS_Z` | head position | ±500 mm |
| `ABS_RX`, `ABS_RY`, `ABS_RZ` | yaw, pitch, roll | ±180°, ±90°, ±180° |
| `ABS_THROTTLE`, `ABS_RUDDER` | gaze on screen, x and y | the whole screen |

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

### Why the device declares buttons it never presses

`BTN_TRIGGER` and `BTN_THUMB` are declared purely to be classified. opentrack's
source says only *"do not remove next 3 lines or udev scripts won't assign 0664
permissions"*, so the mechanism was measured here rather than assumed: dropping
the two key bits and creating the device again produced

```
ID_INPUT=1 ID_INPUT_ACCELEROMETER=1
```

with no `ID_INPUT_JOYSTICK` at all. systemd's `input_id` builtin reads absolute
axes with no buttons as an accelerometer, and SDL — which is what Proton and
most Linux games enumerate through — skips anything not tagged
`ID_INPUT_JOYSTICK`. With the buttons declared:

```
ID_INPUT_JOYSTICK=1  TAGS=:seat:uaccess:
```

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
command, so its device lives exactly as long as the command does.

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
