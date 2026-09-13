# Planned work

What is queued, why it is worth doing, and what each item rests on. Nothing here
is needed for a release that has already shipped; this is the list a contributor
can pick from.

Each item says what to change, where, what it costs, and how it could go wrong.
Where a number appears, it is one this project measured — not one read from
another project.

**All four numbered items have since been built**, and each keeps its original
reasoning here with what actually shipped, what was deliberately left out, and
where the shipped behaviour differs from what this page proposed. A fifth item,
the opentrack port watch, has shipped since. What is still queued is under
[Smaller candidates](#smaller-candidates). None of the four numbered items has
been run against a tracker — the port watch has, end to end — and what that
leaves untested is listed in [Quality-and-Risks](Quality-and-Risks.md) §11.3d.

## How this list came about

Most of it came from comparing this project against
[Ridoc/Tobii5-for-OpenTrack-Linux](https://github.com/Ridoc/Tobii5-for-OpenTrack-Linux)
in September 2026. That project is a GPL-3.0 fork of
[`Aetherall/tobiifree`](https://github.com/Aetherall/tobiifree), the reference
this project already credits, and its driver, SDK and udev rule are byte-for-byte
upstream — so it contributed **no protocol knowledge**. What it does have is a
head-pose *feel* layer, and the four items below are the ideas worth having from
it. None of its code is used here, and none was copied.

Two things from that comparison are settled and need no work:

- Their calibration sends points to op `0x408` and computes with `0x42F`, with
  the eye argument `0`. This project measured that combination as a silent no-op
  on 2026-08-15 — the device acknowledges the points and discards them
  ([Op-Catalog](Op-Catalog.md), `crates/tobii-protocol/src/calibration.rs`). It
  is upstream's error, not theirs, and there is nothing to adopt.
- Their idea of gating an eye as "tracked" on the 2D point being plausible rather
  than on origin validity gives the same answer as ours: decoding this project's
  own committed no-eyes frame
  (`crates/tobii-protocol/src/gaze.rs::real_frame_payload()`) shows the per-eye
  2D columns carrying exactly the `(0,0)` sentinel their predicate rejects.

**Do not read their `assets/extract_firmware.c` or `assets/flash_firmware.c`.**
Those extract firmware from Tobii's own Windows service binary and replay a
signed DFU header captured from a vendor update. That is a provenance this
project does not touch, and this project never flashes firmware at all.

## 1. A one-eye fallback in the head-pose path — shipped in `a804686`

**Was.** `pose_from_sample` (`crates/tobii-headpose/src/lib.rs`) required both
eye validities to be 0 and returned `None` otherwise, so one dropped eye
produced no pose for that frame. This project measured roughly **five per-eye
dropouts per second** as the residual "lag" in head tracking — not latency, and
not the algorithm — so that was not a corner case.

**Shipped.** `tobii-headpose::PairOffset`, a stateful path beside the stateless
one. It re-measures the right-minus-left offset on every two-eye frame and
places the missing eye at the surviving one plus that offset.
`RECONSTRUCTION_MAX_AGE` is **300 ms**, about ten frames at the measured
30.208 ms cadence and well inside the pipeline's 1 s `TRACKING_LOSS_RESET`;
`ONE_EYE_DEBOUNCE` is **2**, and one-directional, so an isolated invalid frame
never switches paths while a frame carrying two measured eyes is answered with
the two-eye pose at once. With both eyes tracked the result is bit for bit the
geometry that shipped before. `tobii-output`'s `FramePipeline` owns the state
beside `neutral` — per-session tracking state that must survive a settings
change — and drops it with the rest at `TRACKING_LOSS_RESET`.

**What a reconstructed pose actually claims.** Yaw and roll are read off the
interocular vector, and during an outage that vector *is* the stored offset. So
rotation is **held** at its last measurement and never extrapolated; only
translation keeps following the eye that is really there. `PoseSource` and
`SourcedPose` carry that distinction out of the crate, because the whole risk of
this path is a guess that reads exactly like a measurement.

**Left out, then half closed.** The visibility this item asked for. The fallback
shipped **counted, not shown**: `FramePipeline::fallback_stats` reports the
both-eye and reconstructed frame counts and whether the pose that went out was
reconstructed, and nothing in that commit read it — `tobii debug`, the hub and
`tobii headpose --check` live in crates it did not own. `tobii-headpose` has no
logger of its own (it cross-compiles for the Wine bridge), so counting was the
only in-crate option. `f25a2ea` then added `fallback_note` in `tobii-cli`, which
puts `, one eye N%` on the rate line of `tobii headpose` and of `--check`. The
hub and `tobii debug` still show nothing.

**Different from what this item proposed.** This page read as "wire
`eyeview::PairOffset` in". What shipped is a second implementation rather than a
shared one: the eyeview version carries normalized trackbox positions for a
drawing, this one tracker-space millimetres for a pose, and this one ages
against **two clocks**: the host's, so a stalled stream cannot make an old
offset look fresh by simply not arriving, and the frame's own `timestamp_us`, so
a transport backlog drained in one burst — every sample stamped with the same
host time — cannot either.

**Where it changes the output, and where it does not.** Only on frames where
nothing else produced a pose — in practice the 5-DOF path. Both front ends
compute their pose with the *stateless* `pose_from_sample` first
(`tobii-cli`'s headpose loop, `tobii-gtk`'s `device.rs`), and the pipeline uses
the reconstruction only when that comes up empty and no fresh model pose exists;
with one, `onnx::fuse` already falls back to the model's own position, so the
reconstruction is bypassed. It is still counted there — the pipeline runs the
geometry on every frame, which is what makes the ratio a statement about the
tracker rather than about which path won. The hub's own head-pose readout, and
the angles on `--check`'s `eyes` line, still say nothing about a one-eye frame;
the `, one eye N%` fragment at the end of that line is where it shows.

## 2. Reject-and-hold for glitches in the pose filter — shipped in `2ea22cd`

**Was.** `crates/tobii-headpose/src/filter.rs` was a plain EMA that rejected
only non-finite input, so a sample jumping further in one frame than a head can
move was blended in over the following frames — a swing across the view rather
than a discarded sample. It is the pairing for item 1: when an eye drops and
returns, the eye-origin midpoint steps.

**Shipped.** A plausibility gate in front of the average. A finite sample whose
**position** has moved more than `DEFAULT_MAX_STEP_MM = 150.0` from the last
accepted **raw** sample is held, and the raw reference is held with it —
measuring against the smoothed output instead would let a glitch drag the
reference after it one blend at a time, until the view had swept there anyway.
The hold is bounded at `MAX_HELD_FRAMES = 3` (91 ms at the measured interval), a
chosen bound: unbounded, it locks up, because once the head really is elsewhere
every later sample is just as far from the held value. `reset()` drops the
reference with the state, so whoever comes back after a tracking loss is allowed
to be somewhere else. Non-finite handling is unchanged and stays ahead of the
gate, deliberately not charged to the hold budget.

**Where 150 mm comes from, and what it is not.** Not a recorded distribution:
**no committed recording in this repository contains a tracked eye**, which was
decoded rather than assumed (see [Quality-and-Risks](Quality-and-Risks.md)
§11.3d). What `session.tobiicap` does give is the frame interval, **30.208 ms**,
which turns a speed into a per-frame step: 150 mm is **4.97 m/s**, several times
the fastest a seated head translates. It rejects a teleport, not the few
millimetres a stale one-eye reconstruction contributes — a limit tight enough
for that would reject a genuine lunge, which is the risk this item named. It
also cannot go below 100 mm without changing `pipeline.rs` and `tobii-gtk`'s
`outputs.rs`, which each move a synthetic 100 mm between two consecutive frames
and assert the axis moves.

**Position only, and no angular limit at all.** The pose reaching the filter is
the composed one, so yaw and pitch already carry the gaze-driven Extended View
term. Measured on the panel the defaults were tuned for: gaze at the left edge
gives `ev_yaw −45.00°` and the right edge `+45.00°`, so a one-frame look across
the screen legitimately swings composed yaw by 90°. Any angular gate tight
enough to catch a rotation glitch would reject that. A bad frame that moves
position is still rejected whole, angles included.

**Left out.** The `games.toml` key this item offered. The only path from that
file to the filter is `PoseFilter::new(cfg.filter_alpha)` in `pipeline.rs`, a
crate that commit did not own, so an advertised key would be written into the
file, listed by `tobii games`, and read by nothing. `with_max_step_mm()` exists
so that wiring is a one-line change when it is wanted; nothing outside the
crate's own tests calls it today.

## 3. A rotation recentre — shipped in `f25a2ea`

**Was.** There was none. `crates/tobii-output/src/pipeline.rs` said "Rotation is
never recentred: it is already referenced to facing the screen" — true of the
Extended View term, which is built against the screen's own corners, and not of
the head term, which is an `atan2` on the interocular vector (`pose_from_eyes`)
and so absolute in the *tracker's* frame. It is referenced to the screen only by
assuming the tracker is mounted square and the user sits on its axis. This
project has a measured session with a head sitting **17.4° off-axis**; that user
had a permanent in-game bias and no control that removed it.

**Shipped.** `rot_ref: Option<[f64; 2]>` beside `neutral` in `pipeline.rs`, two
degrees subtracted from the head term, and a settle window that decides what
they are. `begin_recentre` opens one; `RECENTRE_WINDOW` is **1000 ms**, about 33
frames at the 30.208 ms cadence measured in `session.tobiicap`, and short enough
that the user is still holding the pose they pressed the button in. The window
is reduced the way `tobii_headpose::pitch_offset_from` already reduces the other
measured zero in this project — **median and 10–90% spread**, so the frames
before the user settled cannot move the answer. Non-finite angles are dropped
rather than sorted (a degenerate model quaternion normalises to NaN, and
`partial_cmp` answers `None` for it), which costs the run those frames and so
shows up as a window with too few poses, which is the truth.

**Three ways a window is refused, and all of them leave the old reference
standing.** `RECENTRE_MIN_POSES = 10` of the ~33 frames a second holds: a chosen
floor bounded by measurement in both directions — the 2026-08-09 session lost
its left eye in 34% of 400 frames and its right in 18%, so demanding most of the
window would refuse an ordinary session, while a window that yielded fewer than
ten poses means the tracker was mostly not seeing the user. The ten are
*measured rotations*, not poses: a reconstructed frame's yaw and roll are the
last two-eye frame's bit for bit — `PairOffset` holds the interocular vector and
`pose_from_eyes` reads the rotation off precisely that vector — so the window
drops them rather than counting one measurement up to nine times, which would
pad the floor and pull the spread towards zero with duplicates taken while the
head was free to move through the outage. A supplied pose is dropped the same
way, by the `SuppliedPose::rotation_at` stamp it carries: a held model pose,
which the front ends re-offer for up to a second, is counted once rather than
on every frame of the window.

`RECENTRE_MAX_SPREAD_DEG` is not a new number either: it *is*
`tobii_headpose::MOVED_SPREAD_DEG`, **8.0**, the spread at which
`--calibrate-pitch` already tells the user their head moved during the
measurement — the same shape of measurement reduced by the same
`median_and_spread`, so one constant rather than the same figure written twice.
It is **the worse of yaw and roll, not their average** — a head that held its
yaw while swinging in roll was still moving — and it refuses rather than merely
flagging, because a pitch run prints the number it measured while a rotation
reference is invisible once applied. The bias this exists to remove is twice the
bar (17.4° against 8°), so an off-axis user is refused for *moving*, never for
being off-axis.

The third is the window the user walks out of. A run abandoned by the
tracking-loss reset is reported as `Interrupted`, separately from the `NoHead`
the floor produces: it can be holding any number of perfectly good poses, and
saying "found you in only 20 frames" against a floor of ten sends the user to
look at their tracker instead of at the measurement they left. All three are
reported — `RecentreOutcome` names what went wrong, because a recentre that
quietly did not take is indistinguishable from nothing happening.

**Before `compose`, never after.** The exact opposite of the rule for
translation, for the opposite reason: after `compose` the angle also carries the
gaze-driven Extended View term, so a reference taken while the user glanced at a
screen edge would write that glance in permanently — and unlike the neutral,
nothing decays it. A window that completes on a frame is applied to *that* frame,
so the recentre takes effect on the first frame it possibly can.

**Pitch is untouched**, as this item asked: it already has a measured zero from
`tobii headpose --calibrate-pitch`, saved to disk and applied inside the model,
and the geometric path has no pitch to reference at all. The reference also
**survives a tracking loss**, unlike the neutral beside it — walking away is not
a request to undo something the user asked for — though a settle window *in
flight* when the head is gone for a second is abandoned and reported as
`Interrupted` rather than averaged across the gap.

**Three ways in, one place they are decided.** The hub's "Recentre view" button
in the games row, `tobii headpose --recenter` (`--recentre` is accepted too,
because every line of prose here spells it that way), and `Msg::Recentre` over
the socket, answered with `RecentreReply`. The message kind is additive, which is
what the codec's `Unknown` arm exists to make safe for an older client. Only the
`FramePipeline` on the device thread can perform one, so a request is *left* in
`Recentring` (`crates/tobii-gtk/src/outputs.rs`) and taken by whichever pipeline
is composing frames — it is not a `DeviceCommand`, because a recentre changes
nothing on the tracker.

**Refused before it is taken, in the three states the asker cannot see.**
`recentre_decision` refuses while a flow holds the device exclusively
(`EXCLUSIVE = ["calibration", "display setup"]`) — a calibration has the user
following a stimulus dot into the corners, so the "posture" a window would
average is whichever corner the dot was in — when nothing is tracking, which
would otherwise be a control that appears to do nothing, and when nothing is
composing a pose to recentre: the pipeline that performs one lives inside a
`GameOutput`, and there is none with game output switched off or with no
destination set, which is the state a fresh install is in. That last question
is asked of the settings (`outputs::composing`, the same condition
`GameOutput::for_session` answers by returning `None`), so it cannot see a sink
that exists in the settings and failed to open. The hub's button is insensitive
exactly when that function would refuse. A request nobody consumes goes stale
after `REQUEST_MAX_AGE = 2 s`: game output takes one on its next gaze frame,
30.208 ms away, so anything still waiting two seconds later has nothing taking
it — the settings that refusal reads can be true and the composing session end
between the ask and the take — and performing it when a game finally starts
would take a reference from a posture nobody was holding. Two presses before
either is taken collapse into one, which is what they mean.

**Two things rode along**, because they need files this change already owns:

- **`filter_max_step_mm`** — item 2's "left out" is no longer left out. The
  filter's jump limit is a `games.toml` key, read at both places a pipeline is
  built (`FramePipeline::new` and `reconfigure`), so the constant that shipped
  with the gate is tunable without a rebuild.
- **The one-eye fallback is visible.** `fallback_note` appends
  `, one eye N%` to the rates that `tobii headpose` and `--check` already print,
  with `(now)` while the pose at that instant is a reconstruction — so a tracker
  that cannot see one eye at all reads differently from one that blinked twenty
  minutes ago. The share is of the frames the tracker delivered, counted whoever
  supplied the pose for them, so a session running on the neural model reports
  the device's dropouts rather than 0%. Item 1's "left out" is half closed: the
  hub and `tobii debug` still show nothing.

**Different from what this item proposed.** This page said "average over a settle
window of about a second". What shipped takes a **median**, and can **refuse** —
an average always produces a number, and a number produced from a head that was
moving is exactly the permanent bad reference this item was written to avoid.
The refusal states (a flow that owns the device, nothing tracking, a request gone
stale) are not on this page at all; they came out of the three entry points
existing at once. The spelling is the other difference: this page said
`--recenter`, the code accepts it and spells everything else "recentre".

## 4. Per-group retry in the calibration flow — shipped in `161198e`

**Was.** A point the device refused, or a group whose deadline ran out, ended
the whole 7-point run and offered "Try again", which restarts from the first
point. On a 49" panel one bad corner therefore cost every point already
collected, and nothing on the device side required it: this flow sends per-point
`CalCollect`/`CalDiscard` and already discards and re-dwells mid-collect.

**Shipped.** `MAX_GROUP_ATTEMPTS = 3` in `calibrate_flow.rs` — the first
showing plus two re-shows; only the last attempt still fails, with the same
message it always gave. "Try again" is untouched and still restarts a genuinely
failed run from the first point. A re-show keeps the points the group already
captured (`calibrated` is monotonic, and clearing a captured bit would panic
the `expect()` that reads `collected` against it), keeps `focused` so a collect
that acks late still has a point to be attributed to, and takes a fresh
deadline and fade-in. A re-show after a *refused* point sends `CalDiscard` for
the point that was in flight; a re-show at the group's **deadline** sends none,
because the two commands share one FIFO device queue — a discard sent there
would run after the collect it means to cancel, and the device would ack the
sample, raising a count no discard decrements, before throwing it away. The
next tick would read that advance, mark the point captured and fit the group
without it: a run reported complete that is a point short. The instruction line
reads "Let's try those dots again" until gaze settles on a dot again, because
the progress count does not move when a group comes back and the run would
otherwise look stalled.

**"Too weak" is two signals, because there are only two.** The device answers
`add_calibration_point` with an ack or an error and nothing else — `tobii-usb`
drops the reply payload, and no per-point sample count has ever been decoded out
of it — so a weak group means the device refused a point
(`CalPhase::last_error`, which is a level the flow **takes** under the lock on
every `Collecting` tick and charges only while a collect is in flight — left in
place, one refusal would be charged again to the re-shown group's first sample
and empty the budget) or the group's own deadline elapsed. The mid-collect
discard on lost focus is deliberately **not** one: it already recovers on its
own, and charging it an attempt would spend the budget on the one failure mode
this flow handles well.

**Three is a judgement bounded by arithmetic**, not a measured or decompiled
figure: an attempt is capped at `COLLECT_TIMEOUT_TICKS` per member (~33 s), so
~99 s for a three-point group — three attempts cap one group at ~5 minutes and a
whole run at ~11.5 minutes of worst case before the failure screen appears. What
is measured is only that a dropped eye is usually transient (item 1's five
per-eye dropouts a second), which is why a first failure is not treated as final.

**Different from what this item proposed.** This page said a partial re-show
needs the group's hit-zone radius recomputed. It does not, and recomputing would
be actively wrong: `group_zone_radii` derives one radius per group from that
group's own points and does not depend on how many are already captured, while
for a lone survivor `focus::zone_radius` returns `f64::MAX` — the whole screen
counts as "on" that point, which is the class of bug the per-group radius was
introduced to fix. The re-show reuses the radius `launch()` computed.

## 5. The hub lights the tracker for a listener on the opentrack port — shipped in `8c2537f`

Written down here because it changes the answer to a question the README used to
answer badly: the wrapper was never needed for the opentrack route.

**What.** The hub reads `/proc/net/udp` (and `/proc/net/udp6`), looks for a
socket bound to the configured opentrack destination — `127.0.0.1:4242` by
default — and, while one exists, holds a `DemandGuard` for it, exactly as a
socket client does today. opentrack running, or X-Plane with the `headtrack`
plugin loaded, then lights the tracker by itself.

**Why.** Because the wrapper is a workaround for a signal the hub could not
otherwise get. `tobii game` transports nothing: it exists only to tell the hub
that something wants data (see [Game-Output](Game-Output.md)). For the opentrack
route, the receiver is already announcing itself — it has bound the port, and the
kernel will say so — so the hub can hold its own demand and the launch option is
no longer needed. The joystick and the Wine bridge keep needing the wrapper,
because neither receiver binds anything the hub can see.

**The trade, stated plainly.** A bound socket is not a request:

- **An idle listener keeps the illuminators lit.** opentrack left open on a
  second monitor is indistinguishable, through `/proc/net/udp`, from opentrack
  feeding a game — same socket, same state. The tracker would be on all
  afternoon, which is exactly what the reference-counted session exists to
  prevent. The wrapper is precise about when a game starts and stops; this is
  not, and cannot be made so from this interface.
- **It only sees this machine.** `/proc/net/udp` lists this host's sockets in
  this network namespace. Sending to another machine's opentrack — a
  configuration `tobii games set opentrack HOST:PORT` allows — finds nothing to
  detect, and so must keep working through a wrapper or through a hub window
  that has focus. A receiver in a network namespace of its own is the same case.

**Consequently the detection is not the only way in**, and it is not silent: the
hub names this cause like any other ("a program listening on the opentrack
port"), and the wrapper stays for the routes and the machines this cannot reach.

**Shipped.** `tobii_output::listener::probe` answers `Yes` / `No` /
`Unknown(why)`; `Unknown` never takes a hold, so a configured address this host
cannot see keeps the old behaviour rather than guessing. Two sockets on the
watched port do not count: one that has `connect`ed, which the kernel serves
only its peer's datagrams, and one of our own sinks — our inode, wildcard-bound
— which the kernel can hand the configured port whenever nothing else holds it,
and which would then hold the tracker on for itself with no way out. The /proc
path is behind `cfg(target_os = "linux")` because this crate cross-compiles to
`x86_64-pc-windows-gnu` for the Wine bridge. The hub polls once a second, not at
the socket loop's 50 ms. The key is `wake_for_opentrack`, default **on**, and it
does nothing until game output is `enabled` (off out of the box) and an
opentrack address is set — so a fresh install behaves as it always did, and the
watch only runs for somebody who already turned game output on.

**Measured, not inferred.** A hub started with `--background` (no window, so
nothing else could be holding the device) against a config with game output on:
**0** USB file descriptors with nothing listening, **1** within four seconds of
a socket binding `127.0.0.1:4242`, and **0** again after it closed and the three
second linger elapsed. Its IPC socket was already owned by another hub at the
time, which the log said — so the watch does not depend on the socket server
binding.

## Smaller candidates

- **Label the per-output validity columns** in [Gaze-Stream](Gaze-Stream.md).
  Upstream labels `0x1d` as combined-2D-valid, `0x1e`/`0x1f` as per-eye-2D-valid,
  `0x21` unfiltered-valid, and `0x23`/`0x26`/`0x28` display-space-valid. This
  project's decoder reads them as U32 and skips them by kind. Decoding the
  committed no-eyes frame shows all of them `0` while validity is `4`, which is
  consistent with those labels. Add them tagged as an unverified third-party
  reading. Ten minutes.
- **Add X4: Foundations to [Game-Output](Game-Output.md).** It reportedly has a
  built-in opentrack UDP listener (Options → Controls), which would make it a
  direct-listener case like X-Plane, needing no opentrack application. **Verify
  against X4's own release notes before claiming it.**
- **Drop the bootloader grant from the udev rule.** `assets/60-tobii.rules` grants
  `2104:0102` (bootloader mode) as well as `2104:0313` (runtime). Nothing in this
  codebase ever opens the bootloader device, and firmware flashing is out of
  scope, so the grant is surface with no user. One line, no behaviour change for
  anything this project does.
- **Reconnection after suspend.** `crates/tobii-usb/src/transport.rs` exposes
  `open()` and a `Drop`, with no re-open path; their project tells users to
  replug after suspend. Two independent implementations hitting the same stale
  endpoint suggests a real device-level problem, and neither has solved it.
- **The `0x1f42` element type.** `crates/tobii-protocol/src/frame.rs` describes
  the op-`0x460` reply as containing a 3-element `0x1f42` struct "whose first two
  values are the point's normalized x and y", without knowing the element type.
  Upstream's TLV header comments claim `0x021f43` point2d_f and `0x031f42`
  point3d_f are 16.16 fixed-point. Decode the stored 713-byte reply both ways and
  see which yields sane coordinates — offline, minutes. Note no code in their
  tree exercises those tags, so it is a comment, not a measurement.

## Verifying a device claim independently

Anything about the hardware gets re-derived here rather than cited from another
repository: `tobii record [--calibration] [FILE] [FRAMES]` for a session,
`tobii-recap <capture.pcap> --gaze-columns` for a capture, `tobii stream --eyes`
for live columns, and `tobii-gtk --accuracy` for the 39-target sweep. See
[Reverse-Engineering-Methodology](Reverse-Engineering-Methodology.md).
