# Cross-cutting concepts and decisions (arc42 §8–9)

The decisions that are **expensive to reverse**, or that a newcomer would
otherwise helpfully undo. Each names what was decided, what it replaced, why,
and where the evidence is. Background in [[Architecture]].

---

## 8. Cross-cutting concepts

### 8.1 Evidence markers

Protocol claims carry their confidence in the source: **[CONFIRMED]** (verified
live against hardware, or a captured frame with a round-trip test),
**[CODE-VERIFIED]** (found in the disassembled Tobii sources but not live-tested),
**[HYPOTHESIS]** (inferred; a lead, not a fact). Treat an unmarked claim as
unproven. This is a working practice, not decoration — see §9.

### 8.2 Error handling

Two layers with deliberately different contracts. `Parser` and `tlv::Reader`
return `Result<_, ProtocolError>`; the high-level decoders (`GazeSample::decode`,
`DisplayCorners::decode`, `decode_camera_frame`) return `Option` and **never
fail on a bad column** — on truncation they return the partial sample, matching
the reference decoder. The cost is that "why did that frame not decode" has no
answer above the reader layer.

`recv` returning `None` means *nothing arrived within the timeout* — ordinary
polling, not an error and not a disconnect. There is no error channel on the
read path at all.

### 8.3 Persistence

Configuration under `$XDG_CONFIG_HOME/tobii-linux/`, every *config* write
atomic (temp + rename in the same directory), every read treating "unparseable"
as "unset" rather than as a wrong value. A corrupt pitch offset must mean *not
calibrated*, never a wrong angle.

Two things are deliberately not under that sentence. The **log** lives at
`$XDG_STATE_HOME/tobii-linux/tobii.log`, because the XDG spec puts logs in
state, and its writes are append-only and best-effort rather than atomic — a
program that cannot write its log must still run. The **autostart entry** is at
`$XDG_CONFIG_HOME/autostart/`, where the desktop specification requires it.

### 8.4 Threading

One device thread owning the connection; one optional head-pose worker on a
**one-slot channel that drops rather than queues**; the GTK main thread touching
neither except through `Arc<Mutex<DeviceState>>` and a command `Sender`. No
widget is ever touched off the main thread.

### 8.5 User consent

The program makes exactly **one** unprompted network request (the launch-time
release check) and it has a real opt-out — a switch in the hub and
`TOBII_NO_UPDATE_CHECK`. Everything else — the model download, the update
install — needs an explicit click, and the model download shows its
non-commercial licence terms first.

---

## 9. Architecture decisions

### Structure

**1. `tobii-protocol` is a pure codec: no dependencies, no I/O, no `unsafe`.**
Builders return `Vec<u8>`; a `Parser` consumes bytes. This is what makes the
reverse-engineered knowledge testable without hardware, and what lets the pcap
decoder and the live driver share one reassembler.
`crates/tobii-protocol/src/lib.rs`; `cargo tree` shows zero dependencies.

**2. The driver is generic over a two-method `Transport` trait.** libusb sits
behind it. That seam is why the driver can be driven by a mock, a recording, or
real hardware — and it is what later made record/replay possible without
touching the driver at all. Commits `0c82731`, `7f2e86d`.

**3. GTK4 over egui.** egui is immediate-mode and cannot do a Wayland
click-through layer-shell overlay (the gaze dot), and fights a polished consumer
look. Cost explicitly accepted: a system GTK dependency, "a deliberate break
from the prior rusb-only ethos, isolated to the GUI crate."
`docs/superpowers/specs/2026-07-19-tobii-gtk-redesign-design.md`.

**4. One version for the whole workspace.** It is what a release is tagged with
and what the updater compares, so per-crate versions make "which crate's version
am I running?" have several answers. `release.sh` refuses to build if the
manifest disagrees with the tag — a build reporting a different version than its
tag offers itself its own update forever.

### The device

**5. Re-apply display area, eye selection and calibration on *every* connect.**
The ET5 wipes its display area to a ~4 mm stub on every reboot, and reboots on
every session close. Until a valid area is set **in-session** the device emits no
eye data at all — validity stays 4 and every eye-origin column is zero.
Confirmed live by reading the raw pre-calibration origin columns, which were
also zero, proving it is upstream of gaze calibration. Commit `b7528b5`.
**This is the single most expensive thing in the repo to "helpfully" optimise
away.**

**6. The USB session is reference-counted, with a 3-second linger.** The IR
illuminators are lit for as long as a session is open. The linger is not
arbitrary: closing makes the device reboot, so the next connect must re-apply
everything in decision 5. Unplanned and now load-bearing benefit: while the hub
wants nothing the device is free, so `tobii headpose` can claim it for a game
without closing the hub. Commit `39c1547`.

**7. `Idle` is not an error state.** The hub renders it "Tracker off", not
"Disconnected" — a user who sees a fault where there is none goes looking for
one.

**8. Requests are matched on op **and** seq, with a wall-clock deadline.** Seq
matching means a stale ack for a repeated op — `cal_add_point` fires once per
point — can never satisfy the wrong call. The deadline must be wall-clock
because gaze notifications stream concurrently through the same read loop, so an
iteration cap would shrink the effective wait to nothing under normal traffic.
Commit `c57d79e`.

**9. The IN endpoint is drained, not discarded, while a large frame goes out.**
Sending is synchronous, so nothing reads IN for its duration while gaze arrives
at ~33 Hz; pushing a 324 KB calibration backs up the device's IN buffer and it
stops accepting OUT — observed failing on transfer 4 of 40, every time.
Buffering rather than discarding matters because the parser reassembles a byte
*stream*, and a hole desyncs framing worse than the stall did. **The commit is
explicit that this is a hypothesis, not proof.** Commit `7f83868`.

**10. Hand-rolled MD5/HMAC-MD5 — dictated, not chosen.** The algorithm is fixed
by the device's challenge/response. A newcomer reading "MD5" as a security smell
and swapping in SHA-256 breaks the handshake outright. The realm key is a
hardcoded 17-byte public constant, 16 ASCII characters **plus a trailing NUL
that is part of the HMAC input**.

### Head pose

**11. Sensor fusion: position from the eyes, rotation from the model.** Two eye
origins cannot express pitch — nodding rotates the head about the line through
them and leaves both origins where they were. Hence a neural backend for
rotation only.

**12. The model is fetched on the user's behalf, never shipped.** opentrack's
weights are non-commercial-only. The terms are shown and agreement is required.

**13. The head-pose worker uses a one-slot channel that drops.** A backlog of
stale frames is worse than a skipped one when the output drives a game camera.

### Updates

**14. No signature — and the docs say so out loud.** `SHA256SUMS` comes from the
same release, over the same connection, by the same code as the archive, so it
catches a corrupted download and nothing else. An earlier version of the file
asserted the opposite, which would lead the next person to believe in a boundary
that never existed. There is a test that the UI wording does not claim a
guarantee the checksums cannot give. Commits `43c1cb6`, `3288f62`.

**15. The updater's network access goes through one guarded module, and wget is
a fallback for curl's *absence*, never its *failure*.** An asset URL comes out of network
JSON, and curl reads a leading `-` there as an option — `-K/tmp/x` turned the
launch-time check into an arbitrary write, proven end to end before fixing.
Retrying a failed curl with wget quietly downgrades every guarantee curl was
enforcing: a download curl aborted for exceeding the size cap was re-fetched by
wget, which has no such option. And `wget --https-only` only applies in recursive
mode — a reviewer captured cleartext on port 80 after an https→http redirect, so
redirects are now followed one checked hop at a time. Commits `91c712f`,
`86171ca`.

*Scope, stated because the unqualified version of this sentence was false.*
`tobii-headpose`'s model download has its own fetcher in `model_store.rs`; it
does not route through `net.rs` and carries none of its flags. It did also fall
through to wget on a failed curl, which this decision says does not happen —
that is fixed, but the fetcher is still separate, and the honest reading is that
this decision governs the updater. See [[Quality-and-Risks]] §11.1.

**16. The install swap: hard-link backup, then one rename per binary,
all-or-nothing.** Two renames are each atomic but not atomic *together*, so a
failure on the second left a new `tobii` beside an old `tobii-gtk`. The backup
is a hard link made **before** the rename, so the binary is never absent — the
previous rename-aside approach left a window in which `tobii` simply did not
exist while the module docs claimed otherwise. New binaries are run once before
anything installed is touched. `ETXTBSY` is retried: measured 51 failures/200
runs before, 19/200 with the library fix alone, **0/300 with both**.

**17. Releases are built in a container, and the glibc floor is enforced.** The
build host *is* the compatibility floor. On the maintainer's machine both
binaries need `GLIBC_2.44`, against Ubuntu 24.04's 2.39 — such a release fails
to start for nearly everyone, and fails *loudly*, because the updater's probe
would correctly refuse it. CI fails the release if the floor rises.

**18. The toolchain is pinned.** `clippy -D warnings` and `fmt --check` on a
floating `stable` are time bombs. Not hypothetical: CI failed on its very first
run on a lint the maintainer's toolchain was one release too old to see.

### Dependencies

**19. Hand-roll small things rather than take a dependency.** SHA-256, JSON, MD5
and the HTTP fetch. The JSON parser is *complete* rather than a field scraper
because the release body is a changelog shown verbatim — scraping would hand the
user `\n` as literal text and break on a changelog containing a brace. SHA-256
lives in the lowest shared crate because two unrelated things verify downloads,
and is cross-checked against coreutils at 206 lengths covering the padding
boundaries.

**20. `tract` for ONNX, behind a default-on feature.** ~110 crates and most of
the build time; the CLI can be built without it for 5-DOF (992 KB instead of
32 MB). The GUI has no matching gate yet.

### Testing

**21. Record one real session, commit it, replay it — and treat the fixture as a
photograph, not a specification.** A refactor changing an op code or reordering
the handshake would compile, pass every unit test, and break every user. The two
directions are deliberately decoupled in `ReplayTransport`: coupling them would
assert a USB interleaving that depends on how many frames landed in one
transfer, which is not a property of the protocol. Verified to catch what it
exists to catch — changing `OP_SUBSCRIBE` by one digit fails with a hex diff
naming the frame. Commit `6ed0f6d`.

### Ported behaviour

**22. The eye-position screen is a faithful port, with every constant quoted
from the decompiled original.** `tobii_stream_engine.dll` was disassembled end
to end and confirmed to hold no smoothing, so the ported algorithm is the
*complete* original behaviour rather than an approximation. The file looks
over-specified because the magic numbers are ground truth, not tuning.

**23. `PairOffset` — reconstructing a momentarily-lost eye — is the one
deliberate divergence.** Measured on a live 400-frame capture: left eye invalid
in 34% of frames, right in 18%, *both* in only 9% — so for a quarter of all
frames one dot vanishes while the other sits there, ~5×/sec. Carrying the last
offset draws both dots in 91% of frames instead of 66%/82%, bounded by the
original's own patience so a reconstructed eye never outlives the original's
willingness to say "I can see you".

**24. Calibration area is sized by gaze angle, not by Tobii's 600 mm**, and
there is no runtime curvature correction — the calibration absorbs it, and the
plane is seeded with the EDID *arc* width. Fixing the calibration op codes and
widening the area took a 49" panel from 80 mm to 12 mm of mean error.

---

*Continue to [[Quality-and-Risks]].*
