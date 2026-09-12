# Quality, risks and glossary (arc42 §10–12)

What is measured, what is assumed, and what is known to be wrong. Background in
[[Architecture]]; the reasoning is in [[Architecture-Decisions]].

> Every number here is quoted with where it came from. **If a figure appears
> without a source, treat it as a defect in this page.**

---

## 10. Quality requirements

### 10.1 Accuracy — the top quality goal

| Property | Measured | Where |
|---|---|---|
| Gaze accuracy, 49" 32:9 panel, within ±28° | **12.1 mm** mean error (≈0.92° at 750 mm) | measured after the calibration op-code fix |
| Before the op-code fix | **80 mm** | same panel — every calibration before 2026-08-15 was a silent no-op |
| Head-pose yaw vs geometry | slope **+1.03**, r = **0.998** | `tobii headpose --check` on hardware |
| Head-pose roll vs geometry | slope **+0.96**, r = **0.990** | same |
| Pitch offset (mounting + model frame) | **−24.08°**, 10–90% spread 2.57° | `--calibrate-pitch` on the reporter's setup |

### 10.2 Latency and throughput

| Property | Measured | Notes |
|---|---|---|
| Gaze stream | ~33 Hz | the device's rate |
| ONNX inference | **12.4 ms/frame** single-threaded | the reason the worker is a separate thread |
| Eye-dot redraw | GTK frame clock | the 33 ms timer was dropping ~3 frames/sec |
| Head-pose output | 60 Hz default (`--rate`) | opentrack UDP |
| Camera frame | 78 KB at 33 Hz | `Arc`-shared, two readers |

### 10.3 Robustness

- **Every config write is atomic.** An unparseable file reads as *unset*, never
  as a wrong value.
- **Reconnect is invisible.** Unplug/replug re-runs the full re-application
  sequence; the watchdog notices 2 s of silence.
- **An update either lands completely or not at all**, and the binary is never
  absent mid-swap.
- **A new binary is run before it is installed**, so a build for the wrong glibc
  is refused while the working one is still in place.

### 10.4 Not surprising the user

- Exactly **one** unprompted network request, with an opt-out.
- The tracker runs only while something needs it.
- The autostart session opens **no window** and **no USB session** —
  measured: zero USB fds and zero GPU fds while idle.

---

## 11. Risks and technical debt

### 11.1 Known and accepted

**No signature on updates.** `SHA256SUMS` is an integrity check. Anyone able to
publish to the repository's releases could publish binaries every check accepts.
This is stated in the code, the README and the UI. *Closing it needs a key
pinned in the binary; there is none.*

**The head-pose focal length is assumed.** `DEFAULT_FOCAL_PX = 355` feeds a
perspective correction worth 12–16° of pitch. `focal_from_eye_origins` can
compute the real value from data already on the wire; it is not yet wired up.

**The head-pose model download has a second, unguarded fetcher.**
`model_store.rs` builds its own curl/wget invocation rather than going through
`tobii-update`'s `net.rs`, so it carries none of that module's flags — no
`--proto =https`, no `--proto-redir`, no size cap, no per-hop host check. It is
a one-off fetch of a model the user has explicitly agreed to, from an allowlisted
host, verified by SHA-256 afterwards — but ADR 15's rule about network access
governs the updater and not this. *Closing it means routing `download_to`
through `net::fetch`.*

**The replay fixture is a photograph.** It proves the code still does what it
did when recorded, not that the device still does. A firmware change would
invalidate it silently and the tests would stay green.

### 11.1a Packaging

The release notes point here as "the full list", and until v0.1.0 this section
had nothing about packaging at all.

**The `.rpm` is the least-tested artifact.** `package.sh` skips it where there
is no `rpmbuild`, which is most developer machines — so the spec is written on a
machine that cannot read the package it produces. That is not theoretical: the
spec carried `AutoReqProv: no` for its whole life, which switches off the
`libc.so.6(GLIBC_x.y)` and soname requirements rpm derives from the ELF, and
nothing local could have noticed. `release.yml` now opens both finished packages
and fails if those declarations are missing, which is the only check that runs
where the packages are actually built.

**Only the Arch package is installed by anything, and only in a container.**
From v0.3.1, `release.yml`'s `arch` job installs `tobii-linux-bin` with
`pacman -U` on a fresh `archlinux:base-devel` container. Both binaries must then
answer `--version` from `/usr/bin` with every library resolved. `publish` waits
for that job, so no release from v0.3.1 on is published unless its package
installed there. `--version` returns before any GTK or device code runs, so this
checks the package's files and libraries, not the program: no display, no
tracker, no upgrade over an earlier version. The job runs only on a `v*` tag, so
its first run is the first tag after v0.3.0. Nothing installs the other
packages. The `.deb` has been unpacked and read field by field, the source
PKGBUILD parses under real `makepkg --printsrcinfo`, and CI checks the `.deb`'s
and `.rpm`'s declared dependencies — but no `apt install` or `dnf install` has
been run on a clean machine of the distribution it targets.

**The `.deb`'s `Depends` is hand-written.** `dpkg-shlibdeps` is the tool for
this and is not used, so the list is a judgement about what GTK pulls in
transitively rather than a derivation from the binaries. The glibc floor is
derived (from `objdump`), the rest is not.

**The PKGBUILD's `sha256sums` is `SKIP`.** GitHub's generated source tarballs
have not been byte-stable across their own tooling changes, and a pinned digest
that breaks is worse than an absent one — but it does mean the source-build
channel verifies nothing about what it downloads. The prebuilt `tobii-linux-bin`
is the pinned alternative on Arch: its PKGBUILD pins the release tarball's sha256,
taken from the release's own `SHA256SUMS` — an integrity check, not a signature,
like every checksum in this project.

**The binaries ship unstripped**, about 9 MB of symbol table each — a 34 MB
binary strips to 25 MB (measured with `strip` on both, September 2026).
Deliberate:
release builds carry no debug info, so the symbol table is the only thing that
makes a panic in a bug report name a function rather than an address, and this
project asks people to paste panics.

### 11.2 Confirmed defects

Found by review and reproduced. Fixed ones are kept here with what they were,
because the reasoning is worth more than the tidiness.

**Fixed:**

- ~~The parser's continuation-envelope heuristic false-positived~~ — `feed`
  stripped 8 bytes whenever a frame was in flight and the chunk began
  `01 00 00 00`, which is also a legitimate type-0x01 TLV field header and turns
  up in camera pixel data. The envelope's own length field is checked now.
- ~~`parse_enabled_eye` did no structural parsing~~ — it read the last four
  bytes of any slice, so `[0,0,0,2]` returned `Some(Right)`. It goes through
  `Reader` now.
- ~~`write_atomic` did not `fsync`~~ — rename gives atomicity of *visibility*,
  not durability of *content*. The temp file is synced before the rename and the
  directory after it.
- ~~`Capture::parse` panicked on a non-ASCII line~~ — `split_at(1)` aborts when
  byte 1 is not a char boundary, so a hand-added note with an umlaut killed the
  process instead of returning `BadLine`.
- ~~A dropped queued command lost the eye selection permanently~~ — it was
  persisted inside `apply_command`, so choosing an eye with the tracker
  unplugged never reached disk. The UI saves it at the point of choice now, and
  a dropped command that a UI is waiting on publishes a failure rather than
  stranding it on a token that never arrives.

**Open:**

- **A failed query-realm parse is indistinguishable from "no authentication
  required."** `resp_first_u32_opt` cannot tell them apart, because the walker
  above finds no field either way, and 0 means skip auth. Making the unreadable case *fail* was
  tried and the replay harness caught it breaking every connection: the real
  reply uses the 5-byte TLV framing while `resp_fields` walks a 4-byte one, so
  it finds nothing and the 0 default is what makes the handshake work. The
  walker is the real defect; it is left alone because the only reply with
  content we have is that one, and changing an unvalidated parser on a single
  capture is how this project has hurt itself before.
- **Three parallel TLV decoders** live in `tobii-protocol` (`tlv.rs`,
  `handshake.rs`, `camera.rs`). A hardening applied to one does not reach the
  others — see the entry above for what that costs.
- **The stale calibration fixture.** `testdata/real-calibration.blob` predates
  the calibration op-code fix.
- **Stale confidence markers.** At least one op is rated [UNCONFIRMED] in
  `frame.rs` while another module describes its reply parser as confirmed.

### 11.3 Untested surfaces

- **`UsbTransport` itself has no test that touches libusb.** Open/claim/detach,
  the vendor control transfers, chunking, `soak_incoming`, and the `Drop`
  session close are exercised only by running the program.
- **The replay captures cover two slices, not everything.** `session.tobiicap`
  is the everyday path (handshake, display area, enabled eye, gaze, unsubscribe);
  `calibration.tobiicap` adds the one fragmented response this driver produces,
  a 778 KB blob arriving across 60 reads with continuation envelopes. Still no
  camera stream and no error paths.
- **The replay test hand-reproduces `tobii-cli`'s recording sequence.** The two
  live in different crates and can drift apart silently.
- **Past `SOAK_CAP` (1 MiB), `soak_incoming` drops IN bytes** — creating exactly
  the stream hole its own comment says buffering exists to avoid. Reachable if
  the camera stream is subscribed while a calibration blob is applied.
- **The head-pose worker's exit is asserted in a comment, not demonstrated.**
  A live thread-count sample rose monotonically across session cycles without
  falling back; GTK's own threads could not be separated out. Worth one clean
  measurement.

### 11.3a Game output (new in v0.2.0)

- **Nothing has been consumed by a real game, and no real head has driven the
  axes.** Every figure in the v0.2.0 notes came from synthetic poses fed through
  the shipped code and read back by SDL, joydev, DirectInput or the FreeTrack
  client. That proves the plumbing, not the feel — the amplification default,
  the auto-recentre, and the per-axis **signs** are all unvalidated by use.
- **Our own udev rule has never been shown to work in isolation.** `/dev/uinput`
  is writable on the development machine because Steam, KDE Connect and Logitech
  each ship a rule granting it. Every "the joystick works" result was therefore
  obtained on a borrowed grant. The packaged rule is believed correct and is
  unproven.
- **`tobii update` cannot deliver the udev rule**, since it replaces binaries
  only. Anyone upgrading into this feature has a rules file that looks installed
  and lacks the uinput line; `tobii debug` detects and names that state, which is
  a mitigation and not a fix.
- **Some games' "look" axis is a rate control**, so a successful bind is not
  evidence the sink works. There is no way to detect this from our side.
- **Steam Input can hide or remangle the device** for a Steam-launched game.
  Documented, not reproduced here.
- **The bridge is 64-bit only.** A 32-bit game finds no DLL while `install`
  reports success. Judged acceptable: of the head-tracking titles that run on
  Linux, Falcon BMS is the only active 32-bit holdout.
- **Flatpak/Snap Steam is half-supported** — the install finds the prefix, the
  launch wrapper probably cannot reach the hub from inside the sandbox.
  Unmeasured in both directions.
- **`GAME_ID` and the feeder's `Once` are per DLL image, not per process.** A
  game loading both client DLLs gets two of each, so the TrackIR profile id
  reaches the mapping only when that DLL also won the port. Harmless today
  because nothing reads `GameID` back.
- **The feeder thread's failure to start is never surfaced.** `Started` is
  discarded by the caller, so a DLL that is waiting on a port held by another
  wineserver session looks identical to one with nothing sending.

### 11.3b The hub's window, the tray and the download path (new in v0.3.0)

- **The tray has only ever been seen on KDE Plasma.** It is a
  `StatusNotifierItem` on D-Bus, the interface waybar, xfce4-panel, LXQt and
  GNOME's AppIndicator extension all implement — none of which has been tried.
  Verified on Plasma against the live `kded6` watcher: the item appears in
  `RegisteredStatusNotifierItems` and `ToolTip` reads back correctly.
- **"A watcher is up" is not "an icon is visible", and the hub cannot tell the
  difference.** It hides itself when something owns the watcher bus name; the
  reply to `RegisterStatusNotifierItem` is discarded, because there is nothing
  useful to do with a refusal. So a host that rejects the item, or a panel
  configured to hide unknown items, leaves the window hidden with no icon. The
  way back still exists — launching the app again presents the existing hub —
  but nothing on screen says so.
- **The Download path for package-managed copies has never been clicked
  through.** It is unit-tested, and its asset matching was checked against the
  real v0.2.0 release assets, but the branch is reachable only when a package
  manager owns the running binary *and* a newer release exists — which no
  development build can produce. v0.3.0 is the release that first makes it
  reachable, and its first users are its first test.
- **Only the pacman branch of the ownership query has run against a real
  package database.** `dpkg` and `rpm` are not installed on the development
  machine, so those two branches are unit-test-only — and they are what decides
  whether a Debian or Fedora user is offered Update or Download.
- **A failed re-download discards an earlier complete one.** The cleanup is
  scoped to the version directory, not to the files that call added, so
  retrying the same version into the same folder and failing part-way takes the
  previous copy with it.
- **The text-size ceiling is reachable and costs more than it looks.** At 160%
  the hub's own minimum is 1395x857 — measured — which is larger than a
  1366x768 screen, so the window opens clipped, and the control that reverses
  it is in a popover anchored to that oversized window. The recovery is to
  delete `~/.config/tobii-linux/text_scale`. Only `font-size` follows the
  setting: every fixed pixel dimension in the hub stays where it is, so
  *lowering* the text size does not make the window fit a small screen either.
- **The layout breakpoints are measured once, when the window is built.**
  Changing the text size during a session leaves the column thresholds
  belonging to the old scale; only the window's height is re-fitted.
- **The window auto-fit stops for good once the user resizes by hand.** That is
  deliberate — a hub that fights a size you chose is worse — but it means
  raising the text size after a manual resize scrolls the content instead of
  growing the window, which reads as the setting half-working.
- **The two- and one-column layouts are effectively unreachable by dragging on
  a floating desktop**, because the window's minimum width and the three-column
  breakpoint derive from the same measurement. A tiling compositor, or a screen
  narrower than the hub, still reaches them. Established from the code and from
  measuring each layout, not from a drag.
- **The fullscreen setup and calibration flows scale with the text-size setting
  and have not been looked at at either end of its range.** The setting rewrites
  the display-wide stylesheet and the process's font DPI, so it is not confined
  to the hub.

### 11.3c Update decisions, the installer and quitting from outside (new in v0.3.1)

- **The three new banners have never been shown by a real update.** *Download*
  for a system copy, *Download* for a home-directory copy whose files are root's,
  and *Quit* for a replaced one need a release newer than the running copy, so
  they are unit-tested, with each decision's branch broken once to prove its test
  bites, and not clicked. The *Quit* case is the reported one: a v0.1.0 hub whose
  package was removed while it ran. The root-owned home copy is the other
  reported one — `sudo ./install.sh` put it there.
- **The folder fixes, the refusal under sudo and the shared folder are
  unit-tested only.** `FixFolder` (a banner with a command and no button:
  `chmod u+w` for an unwritable folder of the user's own, `sudo chown` of one
  folder in the home that another account owns), `RunWithoutSudo`
  (`sudo tobii update --install` on another account's files) and the `--system`
  *Download* for another account's files in a shared or sticky folder have not
  run outside unit tests. The banner's state after an install attempt — *What's
  new* usable again, and *Quit* once the install finds this copy replaced while
  it ran — is covered by a display test that is ignored by default:
  `cargo test -p tobii-gtk --lib -- --ignored a_finished_install`.
- **A group member without sudo is sent to an administrator, and the command
  they are given is refused.** Another account's files in a group-writable
  folder the user does not own get the `--system` *Download*, where a plain
  `./install.sh` by a member of that group would have worked. Worse, the
  `sudo ./install.sh --system <dir>` it prints is refused by the installer,
  whose rule is that `--system` writes only where root alone can change things —
  so that banner leads nowhere but to the installer's own advice, a plain
  install into `~/.local/bin`. Accepted for v0.3.1: the folder is not theirs,
  what is in it may be what every user runs, and both the banner and the
  installer refuse to touch it. The README and Runtime-View say so.
- **The download folder is refused if another user could change it**
  (`UnsafeFolder`), because the command printed for it is run later, perhaps with
  `sudo`, on files that must still be the ones checked. The chosen folder and
  every folder above it, both as written and as resolved, must belong to the
  user, root or uid 65534 (how root's `/` and `/home` look inside a toolbox),
  and be writable by no one else. A sticky folder is exempt from the write rule,
  but only when the user, root or uid 65534 owns it, so `/tmp` is accepted —
  inside a toolbox too, where it belongs to the overflow uid — and another
  account's sticky folder is not. The refusal names who
  can write the folder. Checked with folders a test makes, never with a second
  real user.
- **A group write bit is harmless only when `/etc/passwd` and `/etc/group` show
  the group is the user's alone**, the rule `tobii uninstall` applies too. An
  LDAP or sssd account's private group is not in those files, so on such an
  account a download folder made group-writable by umask 002 is refused, and
  the user has to pick another. The version folder the download makes inside
  the chosen one is created 0755 whatever the umask, and removed again if the
  check refuses it.
- **A POSIX ACL can hide a writer from this rule.** Where a directory has an
  extended ACL, its group bits hold the ACL mask, so a `setfacl -m u:someone:rwx`
  on the download folder, or on a folder above it, reads as a group write bit
  and passes whenever the group is the user's own private group. The account
  that ACL names can then swap the checked files. Nothing here reads ACLs;
  `getfacl` would be needed. The same gap is in `tobii uninstall`'s rule.
- **The folder chooser skips a Downloads folder the download would refuse** and
  opens in the home folder instead. Seen on the development machine, whose
  Downloads folder a download service's group can write (0775). Chosen by hand
  it is still refused, with the reason.
- **`faccessat` answers for the directory, not the swap.** It asks for write and
  search permission (`W_OK | X_OK`), so read-only mounts, ACLs and supplementary
  groups count; `tobii uninstall` asks a user's question the same way. The
  ownership check covers `protected_hardlinks`. Anything
  else that fails the rename still reaches `install_release`'s real attempt and
  its rollback — the button can still fail, only no longer for the reasons
  known in advance.
- **A read-only mount the user owns gets the `chmod u+w` advice**, which cannot
  help there, and neither can `sudo`: root gets EROFS too. `tobii uninstall`
  gives the same advice in the same case.
- **The `sudo chown` advice gives back one folder.** Where v0.3.0's
  `sudo ./install.sh ~/.local/bin` also made `~/.local`, that stays root's, and
  if `~/.local/share` does not exist either, the plain install afterwards stops
  when it makes the menu entry's folder, after the binaries are in place. Rare,
  and not handled.
- **The tray's `IconThemePath` is read by Plasma** — its `GetAll` reply was seen
  carrying it — **but drawing from it in the first-install case is unmeasured.**
  This session's Plasma had already been restarted after `~/.local/share/icons`
  existed, so the case needs a fresh login to recreate. What is confirmed is
  the fallback the README gives: after `systemctl --user restart
  plasma-plasmashell.service` the reported placeholder became the real icon.
- **Any process of the same user can quit the hub**, through the `quit` action
  GApplication exports on the session bus. By design: that is what the
  uninstaller and scripts use, and a same-user process can already signal it.
- **A `quit` from outside closes an open calibration or setup flow first**, so
  the flow's own close handler runs and its tracker claim is released. It is the
  same `quit` action the cogwheel's *Quit*, `tobii uninstall` and the update
  banner use. The calibration's cancel is queued by that handler, but the
  process can exit before the device thread sends it; what that leaves on the
  device is unmeasured on hardware. The display tests for it
  (`crates/tobii-gtk/tests/quit_action.rs` and
  `crates/tobii-gtk/tests/flows_release_the_tracker.rs`) are ignored by default,
  and their negative controls — the flow left open on quit, the gaze-preview
  claim, the Quit row's wiring, and Cancel through `close_on_click` — have not
  been run.
- **`install.sh --system` has never installed as real root here.** The root
  refusal, `--system`, the manifest, the autostart repair and the running-copy
  warning are checked by `scripts/test-install-payload.sh` in CI through its test
  seams (`TOBII_TEST_EUID`, `TOBII_SYSTEM_DATA_DIR`), with each check broken once
  to prove it bites. The same script checks `--system`'s refusal of a target
  another account can change: the other-account branch with a directory
  `chown`ed to 65534 when the test runs as root, as in CI, and otherwise against
  an existing directory another account owns; the group branch with `chgrp` to
  a group that is not the caller's. Each is skipped where there is no such
  directory or group. `--system` sets umask 022, so what it writes is 644 and
  755 whatever the caller's umask (checked under 027). An end-to-end
  `sudo ./install.sh --system` has not run.
- **On a Debian system upgraded from before bullseye, `/usr/local` and the
  folders in it can be `root:staff` 2775.** There `install.sh --system` refuses
  its default target, `/usr/local/bin` (`--system /opt/…` works);
  `sudo tobii uninstall --system` refuses a `--system` install there, whether
  the manifest lists it or `--bindir` names it; and the user's own run leaves it
  as "in a directory others can write". Accepted: that group can change what
  every user runs.
- **The installer edits a file the user owns**: the login entry, and only its
  `Exec` line, only when that names an absolute path that no longer exists, and
  never through a symlink. It reads that line as GLib does, with both escaping
  levels undone, so the hub's own entry for a copy in a directory with `$`,
  `` ` ``, `"` or `\` names the copy it is, and is left alone while that copy
  exists. A bare name, an `env` wrapper or a symlinked entry is left alone. The
  wrapper and the symlink are always named; a bare name is named only when the
  installer's PATH does not find it. Only the `[Desktop Entry]` group's `Exec`
  is read, and an entry with two is named and left alone, as `tobii uninstall`
  leaves it; one with an unterminated quote or a single quote outside double
  quotes is left alone without a word.
- **`tobii uninstall` keeps a desktop entry whose `Exec` it cannot read**,
  rather than guess and switch start-at-login or a menu entry off for a copy
  that stays: one with no `Exec`, with two (GKeyFile and KConfig take the last,
  systemd's xdg-autostart generator the first), or with escaping and quoting the
  spec leaves undefined, which GLib and KDE read differently. Such an entry is
  not used to find an install either, so one reached only through it — a
  v0.3.0 menu entry, unquoted, for a directory with a reserved character in its
  name — needs `--bindir DIR`.
- **A menu entry needs a path the Desktop Entry spec can quote.** `install.sh`
  writes `Exec` in double quotes with `%` doubled; for a directory containing
  `"`, `` ` ``, `$` or `\` it writes no menu entry and says so, because those
  need escaping that launchers do not apply alike. Checked by the install-script
  tests with a directory containing a space, `&`, `|` and `%`, and one with `"`,
  and with the hub's own login entry for a directory with every character its
  escaping covers, for a copy that exists and for one that is gone.
- **The menu entry and the icon cannot stop an install.** The entry is written
  beside its place and renamed in, so one another account owns in the user's
  folder is replaced; and once the binaries are in place, an applications
  folder that cannot be written is said and skipped, and the install still
  writes the manifest, repairs the login entry and runs the udev step. Checked
  by the install-script tests with an entry, an icon and an applications folder
  the user cannot write (skipped as root, whom chmod does not bind).

### 11.3d The one-eye fallback, the filter's gate, the calibration re-show and the rotation recentre (new since v0.3.1)

- **No committed recording in this repository contains a tracked eye**, so
  neither head-pose number below is fitted to real motion. Decoded rather than
  assumed: `crates/tobii-usb/tests/captures/session.tobiicap` holds exactly 40
  gaze records, and every one is byte-identical to `tobii-protocol`'s committed
  no-eyes fixture (`gaze.rs::real_frame_payload()`) apart from its timestamp and
  frame counter — so all 40 carry validity `(4, 4)` with both eye-origin columns
  zeroed, which is the state that fixture's own test asserts.
  `calibration.tobiicap` carries no gaze records at all, and `tobii record
  --calibration` records zero frames by construction. What the session *does*
  give is the cadence: its 39 timestamp deltas are **30208 µs** (30211 max),
  i.e. 33.1 Hz, matching the ~33 Hz documented elsewhere. What follows from
  having the cadence and not the motion:
  - The filter's `DEFAULT_MAX_STEP_MM = 150.0` is **geometry, not a
    distribution** — 150 mm at 30.208 ms is **4.97 m/s**, picked to sit several
    times above a seated head's translation and clear of the synthetic 100 mm
    lean `tobii-output`'s and `tobii-gtk`'s existing tests assert on.
  - `RECONSTRUCTION_MAX_AGE = 300 ms` is a judgement anchored to the 2026-08-09
    session's notes — ordinary outages of ~95 ms (left eye) and ~120 ms (right),
    with two extremes at 480 ms and 1260 ms — and the distribution between them
    was never recorded.
  - `MAX_HELD_FRAMES = 3` (91 ms at that cadence) is a chosen bound as well;
    nothing recorded here says how long a glitch lasts.
  Closing this needs one `tobii record` with a head in the trackbox and the
  per-frame step distribution read off it.
- **A reconstructed pose holds rotation; it never extrapolates it.** Yaw and
  roll are read off the interocular vector, and during an outage that vector
  *is* the stored offset — so a one-eye frame reports the rotation the last
  two-eye frame measured, and only translation follows the eye that is really
  there. That is what the age bound is for, and it also means a user who turns
  their head during a dropout is reported as not having turned, for up to
  300 ms.
- **Where the fallback takes effect is narrower than it reads.** Both front ends
  derive their pose with the *stateless* `pose_from_sample` and hand it to the
  pipeline, which reconstructs only when that returned nothing and no fresh
  neural pose exists — with one, `onnx::fuse` supplies the model's own position
  instead, and the reconstruction is neither used nor counted. The hub's
  head-pose readout and `tobii headpose --check`'s `eyes` line still show
  nothing on a one-eye frame, so the feature is invisible in both places a user
  would look to confirm it.
- **The calibration re-show rests on `discard_calibration_point`**, which
  `tobii-usb`'s own doc marks as reverse-engineered from native disassembly and
  **unverified on real hardware** (`0x438` is [CODE-VERIFIED] in
  [[Op-Catalog]] — disassembly, never a hardware round trip). Every re-show
  sends one for the point that was in flight, and the call is best-effort: a
  failure is logged and the flow continues. If the device does not in fact
  discard, a re-shown point is collected on top of a partial sample rather than
  in place of it, and nothing here can detect that.
- **The fallback is counted, and displayed nowhere.**
  `FramePipeline::fallback_stats` reports the both-eye and reconstructed frame
  counts and whether the last pose was reconstructed; no crate in the workspace
  reads it. So a reconstructed pose currently reaches opentrack, the joystick
  and the Wine bridge looking exactly like a measured one — the failure mode the
  `PoseSource`/`SourcedPose` distinction exists to prevent.
- **The rotation recentre's two thresholds are chosen, not fitted.** Both come
  from the same shortage as the numbers above — no recording here contains a
  tracked eye, so there is no distribution of what a head *holding still*
  actually does over a second:
  - `RECENTRE_MAX_SPREAD_DEG = 8.0` is **borrowed, not measured for this
    purpose**: it is the spread at which `--calibrate-pitch` already tells a user
    their head moved. Same shape of measurement, different axes, and a different
    consequence — the pitch run reports a spread it dislikes and still hands over
    the number, while this one **refuses** and leaves the old reference standing.
    The nearest thing to evidence that the bar is passable is §10.1's pitch-zero
    run on real hardware: 10–90% spread **2.57°** while sitting still, a third of
    the limit — but that is one person, one setup, one axis, and not this code.
    If an ordinary user's yaw/roll spread is in fact larger, the failure mode is
    a button that refuses over and over with nothing to tune, since the limit is
    a constant and not a `games.toml` key.
  - `RECENTRE_MIN_POSES = 10` in the 1000 ms window is anchored to the
    2026-08-09 session's per-eye dropout rates (34% left, 18% right of 400
    frames), which are **not** a distribution of how many poses a one-second
    window yields — the arithmetic from one to the other was never done against a
    recording. `RECENTRE_WINDOW = 1000 ms` is this page's own original figure,
    and how long a user actually holds a pose after clicking is unmeasured.
  - `REQUEST_MAX_AGE = 2 s` and the hub's 6 s outcome message are chosen bounds
    around a consumption latency of one gaze frame; nothing measures either.
- **What a recentre is *for* has never been checked end to end.** The 17.4°
  off-axis session that motivates it was measured before this existed, and
  nothing since has confirmed that taking a reference from such a posture removes
  the in-game bias — that needs a tracker, a game and the same head. Nor is the
  reference shown anywhere once applied: the pipeline holds two degrees that no
  readout prints, so a wrong reference presents as "head tracking is turned a bit"
  with nothing to inspect.
- **One reference, two rotation sources.** `rot_ref` is subtracted from the head
  term whatever produced it, so a reference measured while the neural model was
  supplying rotation is still applied when the model drops out and the geometric
  `pose_from_eyes` takes over. §10.1 measured the two as closely correlated
  (yaw r = 0.998, roll r = 0.990) but a constant offset between them would ride
  straight through this, and no test or measurement rules one out.
- **None of the four has been run against a tracker.** All are unit-tested, each
  new test negative-controlled by undoing the change it covers, and the hardware
  halves are untestable here: that a one-eye frame's surviving origin behaves as
  the rigid-offset model assumes, that holding a sample is the right answer to a
  real glitch, that the device refuses a point the way the re-show assumes and
  then collects it on a second showing, and that a settle window of real head
  data passes its own spread test.

### 11.4 Environmental

- **Glyph clipping at fractional display scale.** Tops of tall glyphs appear
  shaved. CSS, the GSK renderer and widget-label theories have all been ruled
  out by measurement, and it is not reproducible offscreen. Remaining suspect:
  the compositor's own downscale. Setting an integer scale is the test.
- **`0x501` and `0x50e` are the same camera exposed twice**, not a stereo pair —
  199/199 byte-identical frames with a face in view, 166/166 on an empty scene.
  There is no stereo depth to be had from this device.
- **The GUI always pulls `tract`.** `tobii-gtk` has no `onnx` feature gate.

### 11.5 Process

- **Hardware claims cannot be re-verified by CI.** The accuracy figure, the sign
  conventions and the tracker-on behaviour were measured by hand. If they
  regress, nothing will say so.
- **Half of the release workflow has never run.** `release.yml` built and
  published v0.1.0 to v0.3.0 (four runs; the first, for v0.1.0, failed and was
  fixed). Its `arch` job, the tag's own install-script checks and the
  pre-release flag came after v0.3.0, and `aur.yml` has never run, so the v0.3.1
  tag is their first execution. Nothing records whether the updater's
  download-and-install path has run against a real release. `ci.yml` has run
  (and failed once, usefully, on a clippy lint the maintainer's toolchain was
  too old to see).
- **A review agent has twice modified the tree it was told to read.** One
  deleted a security guard and ran `rm -rf` on a tests directory; another left a
  stray git repository in the scratch directory that a later `git add -A` picked
  up. Every review prompt since carries an explicit read-only constraint, and
  the working tree is checked before a release is cut.

---

## 12. Glossary

| Term | Meaning |
|---|---|
| **TTP** | The device's framing: a 24-byte header (magic, seq, op) inside an 8-byte USB envelope. Note the envelope length field is asymmetric — outbound it excludes the envelope, inbound it includes it. |
| **TLV** | Type-length-value payload encoding. Three variants exist in this codebase; see §11.2. |
| **Q42** | Fixed-point: a signed 64-bit integer scaled by 2⁴². How the device sends reals. |
| **XDS row/column** | The tagged table structure a gaze frame is built from — 39 columns. |
| **Realm** | The device's authentication scope. Opening one may require an HMAC-MD5 response to a challenge; `realm_type == 0` means no auth. |
| **Trackbox** | The volume in which the device can see eyes. Eye position within it is normalised `[0,1]` in x, y **and z** — z is normalised depth, *not* millimetres. |
| **Display area** | The three tracker-space corners (TL, TR, BL) defining the screen. The fourth is implied. Wiped on every device reboot. |
| **Demand** | The reference count on the USB session. While it is zero the tracker is off and the device is free for another process. |
| **Linger** | The 3 s after the last `DemandGuard` drops before the session closes. |
| **Present bit vs validity** | A present bit means the column was sent, **not** that the data is good. A no-eyes frame carries eye origins present and set to `[0,0,0]` with validity 4. Gate on both. |
| **opentrack datagram** | **Six** little-endian `f64` (48 bytes): x, y, z in centimetres, then yaw, pitch, roll in degrees. |
| **Capture** | A recorded USB session (`tobii record`) replayed in tests. A photograph, not a specification. |

---

*Back to [[Architecture]] · developer guide: [[Development]]*
