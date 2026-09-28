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
- The tracker runs only while something needs it — with two settings that let
  the user answer that question themselves, and a badge and a diagnostics line
  so an answer left switched on is visible (§11.3f).
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

**The binaries ship unstripped**, about 10 MB of symbol table each — `tobii`
is 37.1 MB and strips to 27.1 MB, `tobii-gtk` 37.8 MB to 27.4 MB (measured with
`strip` on both, September 2026; decimal MB, which is why a file manager
showing MiB says 35 and 36).
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

### 11.3d The one-eye fallback, the filter's gate, the calibration re-show and the rotation recentre (new in v0.4.0)

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
    was never recorded. The 300 ms is measured against **two clocks**, because
    neither bounds what the other does: the host's `Instant` refuses a *stalled*
    stream, which produces no frames to age an offset with, and the frame's own
    `timestamp_us` refuses a *drained backlog* — `tobii-usb` soaks incoming
    transfers while a large frame goes out, and both callers stamp
    `Instant::now()` per sample inside the drain, so one host time covers a
    whole burst however far apart the frames were recorded. A frame without the
    timestamp column, or one whose timestamp went backwards, falls through to
    the host bound alone.
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
- **Where the fallback takes effect is narrower than where it is counted.** Both
  front ends derive their pose with the *stateless* `pose_from_sample` and hand
  it to the pipeline, which *uses* the reconstruction only when that returned
  nothing and no fresh neural pose exists — with one, `onnx::fuse` supplies the
  model's own position instead and the reconstruction goes nowhere. It is still
  counted there: the pipeline runs the geometry on every frame whoever ends up
  supplying the pose, which is what makes the ratio in the bullet below a
  statement about the tracker rather than about which path won. The hub's
  head-pose readout, and the yaw/pitch/roll fields of the `eyes` line in
  `tobii headpose --check`, still carry no marker of their own on a one-eye
  frame — the rate fragment at the end of that line is the only place it is
  named.
- **The calibration re-show rests on `discard_calibration_point`**, which
  `tobii-usb`'s own doc marks as reverse-engineered from native disassembly and
  **unverified on real hardware** (`0x438` is [CODE-VERIFIED] in
  [[Op-Catalog]] — disassembly, never a hardware round trip). A re-show after a
  *refused* point sends one for the point that was in flight, and the call is
  best-effort: a failure is logged and the flow continues. A re-show at the
  group's **deadline** sends none, deliberately — `CalCollect` and `CalDiscard`
  share one FIFO device queue, so a discard sent there would run *after* the
  collect it means to cancel, and the device would ack the sample (raising a
  count no discard decrements) before throwing it away — which the next tick
  reads as a point captured, fitting the group without it. What that path accepts
  instead: the collect it left running can ack late, and `focused` is carried
  across the re-show so the ack lands on the point it belongs to — unless the
  user has since settled on another point and a new collect is in flight, in
  which case it is attributed to that one. If the device does not in fact
  discard, a re-shown point is collected on top of a partial sample rather than
  in place of it, and nothing here can detect that.
- **The fallback is counted, and named on one status line.**
  `FramePipeline::fallback_stats` reports the both-eye and reconstructed frame
  counts and whether the pose that went out was reconstructed. `tobii-cli`'s
  `fallback_note` is its only reader: it appends `, one eye N%` — plus `(now)`
  while the pose at that instant is one — to the rates that `tobii headpose`
  prints and that `--check` carries at the end of its `eyes` line. The hub,
  `tobii debug` and the sinks do not read it, so a reconstructed pose still
  reaches opentrack, the joystick and the Wine bridge looking exactly like a
  measured one — the failure mode the `PoseSource`/`SourcedPose` distinction
  exists to prevent. What the ratio measures is the **tracker**: both counters
  are raised from the geometry the pipeline runs on every frame, so
  `reconstructed / (both_eyes + reconstructed)` is the share of frames the
  device delivered with one eye, and a ratio climbing towards parity is a
  tracker that cannot see an eye rather than a defect in this code. `active` is
  the one field about the outgoing pose.
- **The rotation recentre's two thresholds are chosen, not fitted.** Both come
  from the same shortage as the numbers above — no recording here contains a
  tracked eye, so there is no distribution of what a head *holding still*
  actually does over a second:
  - `RECENTRE_MAX_SPREAD_DEG` is `tobii_headpose::MOVED_SPREAD_DEG`, **8.0**,
    and it is **borrowed, not measured for this purpose**: it is the spread at
    which `--calibrate-pitch` already tells a user their head moved, named once
    as a shared constant rather than asserted alike in two places. Same shape of
    measurement, different axes, and a different consequence — the pitch run
    reports a spread it dislikes and still hands over the number, while this one
    **refuses** and leaves the old reference standing.
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
    recording. The ten are **measured rotations**, not poses, on both paths. A
    reconstructed frame is dropped because its yaw and roll are the last two-eye
    frame's bit for bit; a supplied pose is dropped when it carries a
    `SuppliedPose::rotation_at` stamp the window has already counted, which is
    how a held model pose — the front ends re-offer one for up to
    `HEAD_POSE_MAX_AGE`, about 33 frames — is counted once rather than every
    ~30 ms. Identity, not freshness: the two streams run at ~33 Hz and are not
    in lockstep, so "measured on this frame" is false for nearly every good
    frame, and only the pipeline knows what it has already counted. What is
    unmeasured is how much harder this makes the floor to reach on exactly the
    tracker those dropout rates describe.
    `RECENTRE_WINDOW = 1000 ms` is this page's own original figure, and how long
    a user actually holds a pose after clicking is unmeasured.
  - `REQUEST_MAX_AGE = 2 s` and the hub's 6 s outcome message are chosen bounds
    around a consumption latency of one gaze frame; nothing measures either.
- **The recentre's "nothing is composing" refusal reads the settings, not the
  device.** `outputs::composing` asks what `GameOutput::for_session` answers by
  returning `None` — game output switched on, and at least one destination
  configured — so a request made with the switch off, or with nothing to send
  to, is refused instead of expiring unanswered. It therefore still accepts one
  where a sink exists on paper and not in fact: a `/dev/uinput` the kernel
  refused, or an opentrack socket that would not open. Those are named by the
  standing status line, which is what a refusal falls back to.
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
  new test negative-controlled by undoing the change it covers — a control that
  proves the test sees its own line, not that the suite catches every mutation.
  A deliberate copy-paste of the `Seen::Left` arithmetic into the
  reconstruction's `Seen::Right` arm moved the head centre 63 mm and survived
  the whole suite — a mutation introduced to measure the suite, not a defect the
  code ever shipped — and it is why `a_lost_eye_does_not_move_the_head_centre`
  was extended to exercise both arms.
  The hardware halves are untestable here: that a one-eye frame's surviving
  origin behaves as the rigid-offset model assumes, that holding a sample is the
  right answer to a real glitch, that the device refuses a point the way the
  re-show assumes and then collects it on a second showing, and that a settle
  window of real head data passes its own spread test.

### 11.3e The opentrack port watch (new in v0.4.0)

- **A bound socket is not a request, and this cannot tell the difference.**
  opentrack left open on a second monitor looks exactly like opentrack feeding a
  game, so the illuminators stay lit for as long as it is open. `tobii games set
  wake_for_opentrack false` turns the watch off; the wrapper remains exact about
  when a game starts and stops.
- **Two kinds of socket do not count at all**, because neither can receive what
  we send: one that has `connect`ed — the kernel delivers it only its peer's
  datagrams, which is most of the outbound UDP on a desktop — and one of our own
  sinks, recognised as our inode (from `/proc/self/fd`) bound to a wildcard. The
  second closes a latch: `OpentrackUdp` and `BridgeUdp` bind `0.0.0.0:0`, the
  kernel draws that port from `ip_local_port_range` (32768–60999 on a stock
  Linux), and it is free to draw the configured opentrack port precisely when
  nothing else is bound there — which is the case the watch is asked about. The
  hold would then keep the session, the session the sink, and the sink the
  answer, and the tracker would never return to standby. The collision needs a
  configured port inside that range, which the shipped default (4242) is not.
  What the rule does **not** cover: a *foreign* unconnected sender that
  transiently holds the watched port still reads as a listener, for up to one
  poll interval.
- **It sees only this host, in this network namespace.** A configured opentrack
  address that is not local answers `Unknown`, never `No`, and `Unknown` takes no
  hold — so sending to another machine, or to a receiver in its own namespace,
  behaves exactly as it did before and still needs a wrapper, a focused hub, or
  one of §11.3f's two standing holds (`keep_awake`, or `wake_for_joystick` with
  the joystick sink on).
- **The decode assumes a little-endian host.** `/proc/net/udp` prints the address
  as a `__be32` in host order; on a big-endian machine the addresses would come
  out byte-swapped and simply never match, so the watch would be inert rather
  than wrong. Untested there — no such machine was available.
- **What is measured:** a background hub held 0 USB file descriptors with nothing
  listening, 1 within four seconds of a socket binding the configured address,
  and 0 again after it closed. The parser is tested against captured
  `/proc/net/udp` and `udp6` text, and three tests bind real sockets: one on an
  OS-assigned port, one that binds the wildcard on the watched port to prove our
  own sender is skipped while an unfiltered look still finds it, and one that
  `connect`s to prove a connected socket stops counting. What is **not** measured
  is a real opentrack or X-Plane doing the binding — both were inferred from the
  port they document.

### 11.3f Waking the tracker for the joystick, and switching standby off (new in v0.4.1)

Reported as [issue #2]: game output on, the virtual joystick and the Wine
bridge as sinks, and the tracker going dark three to five seconds after the hub
window lost focus. It was not a fault. It was the standby model meeting the one
configuration it could not serve, and every visible surface described it as a
fault.

- **The two new holds are bounded only by the user's memory, and that is a
  deliberate weakening of standby.** With `keep_awake` on, the illuminators are
  lit for as long as the hub runs. With `wake_for_joystick` on (the default),
  they are lit for as long as game output is on — including overnight, if it is
  forgotten. Every other wake path ends on its own: a focused window loses
  focus, a wrapped game exits, `wake_for_opentrack`'s hold ends when the
  program holding that port does. These two end when a human ends them. The
  visibility that pays for it — the header's **ALWAYS ON** badge, the switch's
  off-state sentence, the `can wake it now` line in `tobii debug`, the close
  line in the log — is therefore not decoration, and removing any of it
  re-opens the bargain.
- **What is measured.** The linger is **3.00 s**,
  timed, which is the "3 to 5 seconds" of the report. The joystick's uinput
  node **persists while the tracker is dark**: `GameSide::poll` →
  `sync_joystick` runs from the device thread's idle loop as well as from a
  session, so a game saw axes frozen at centre rather than a disappearing
  controller — which is why gating a hold on the handle really existing cannot
  deadlock. Reader detection was measured and **rejected**: a faithful replica
  of our device was held open by `joystickwake`, by Chrome probing gamepads on
  hotplug and by `winedevice.exe` within 30 ms, never had zero openers across
  30 samples, and the kernel exports no open count, so a real reader is one
  more identical row in `/proc/*/fd`. The hold's placement was decided by
  reading `outputs::spawn`, which returns *before* `std::thread::spawn` when
  `Server::bind()` fails — a waker parked on the socket thread would be missing
  on precisely the second hub — so it lives on `GameSide`, which
  already polls at 1 Hz in both loops.
- **What is not measured: nobody has run
  a tracker lit for eight hours.** There is no soak test of either hold, no
  thermal measurement, and nothing is known about what continuous illumination
  does to an ET5 over a night beyond the obvious. The "overnight" case in every
  warning above is reasoned, not observed.
- **No real game has been watched
  reading unfrozen axes.** The hold is proven by unit tests over `GameSide`
  (the claim exists exactly when the handle does, released on the setting, on
  losing the device, and on game output going off) and the lease behaviour by
  tests over `must_wait` and `EXCLUSIVE`. What has not been done is: turn it
  on, start a game, alt-tab, and watch the view still follow.
- **The close
  line's emission site is untested.** `standby_notice`'s wording has a test;
  the `log::info` call that emits it does not, because `device_session` opens
  `UsbTransport` directly and is not generic over `Transport` the way
  `device_tick` is. Making it generic was judged a larger change than the bug
  needed.
- **`keep_awake` lives in `games.toml`, which is the wrong-sounding
  file for it.** It is there because that is the only config the hub re-reads
  once a second, in the idle wait *and* in a session, so the GUI and the CLI
  both take effect within a second in either state; `config.toml` is read once
  at connect and is rewritten wholesale by `tobii setup`. The cost is that the
  file's name undersells one of its keys, paid down in the file's header
  comment and in the field's docs. The failure direction is at least the safe
  one: a `games.toml` that cannot be read at all falls back to
  `OutputConfig::default()`, where `keep_awake` is off — a corrupt file cannot
  leave somebody's illuminators lit.
- **The GUI half is tested on both sides
  of the display line**, which it was not when it briefly had a store of its
  own. Headless, in CI: `the_switch_writes_the_bit_the_device_thread_reads`
  (`crates/tobii-gtk/src/lib.rs`) writes through the switch's own save path and
  reads back through `load_output_config_from` — the function `GameSide::poll`
  reads with — checks the other settings survived the load-modify-save, and
  asserts the config directory holds `games.toml` and no second file. The GTK
  wiring itself, which is still unreachable without a display, is covered by
  `crates/tobii-gtk/tests/keep_awake_switch.rs`, `#[ignore]`d and run with
  `cargo test -p tobii-gtk --test keep_awake_switch -- --ignored`: it builds a
  real hub, rewrites `games.toml` underneath it the way `tobii games set
  keep_awake true` does, and checks that the switch and the ALWAYS ON badge
  follow within the hub's tick; that the file is **byte identical** afterwards,
  which is what proves a refresh does not re-enter the save handler and write
  the hub's stale idea back; and that switching it off in the hub lands in the
  field `sync_keep_awake` takes the hold from. It needs no tracker. The header
  geometry is still the older, separate measurement: `.measure()` on a replica
  header, 40 px with the badge hidden and 40 px shown — no added height — and a
  clean `load_css()`.
- **The settings popover now clips sooner.** Its natural
  height went from 814 px to 942 px with the new row, and v0.5.0's Help row and
  its hairline (`help_row`, `crates/tobii-gtk/src/lib.rs:2518`, appended at
  `lib.rs:2455`) have since added a further row on top of that, unmeasured. It
  has no `ScrolledWindow`. At the hub's text size multiplier that already cut off the
  bottom row on a 1080p screen at about 1.3×; this makes a pre-existing bug
  roughly 128 px worse. Left alone rather than redesigned inside a bug fix.
- **The two permanent holds made `tobii headpose` unrunnable, and the fix ends
  in a three-second guess against an older hub.** `wake_for_joystick` ships on,
  so a user with game output on, **and a joystick device the hub could actually
  create**, has the hub holding libusb interface 0 for its whole lifetime — and
  `tobii headpose`, `--check` and `--calibrate-pitch`, the documented way to
  tell a gaze fault from a head-tracking one, failed with *already claimed by
  another process* for exactly those users. The CLI now asks for the lease the
  hub has always honoured (`must_wait`'s third term is an `||` for precisely
  this); it was the protocol's first client, since nothing outside the tests
  had ever sent `Msg::Lease`. **Measured**, on the merged tree with a real ET5:
  with `keep_awake` on the hub held one USB file descriptor; `tobii headpose
  --check` printed *the hub has let go of the tracker for this run*, opened the
  device, and the hub's count went to 0; about three seconds after the command
  exited it was back to 1. With no hub running, the same command opened the
  device directly and waited for nothing. **Not measured:** the mixed-version
  case. A hub older than this build does not know the message, so the CLI waits
  out `LEASE_WAIT = 3 s` and opens anyway — which succeeds where that hub is in
  standby and fails with the same `DeviceBusy` as before where it is not. No
  older hub has been run against this CLI. Also not watched: what a **focused**
  hub shows while the lease is out. The path is decided rather than observed —
  the lease term of `must_wait` is an override and not a weighing, the idle
  wait sets `ConnStatus::Idle`, and the eye-position panel renders a closed
  session as **tracker off** rather than *not detected* — but it was read out
  of the code, not seen on a screen.
- **Three interactions between the lease
  and the hub's own flows are known and are NOT fixed in this release.** All
  three were found by reading the merged code in the pre-release check; none
  was found by use, and none has been run on hardware. They are written down
  rather than fixed because each wants a design answer (which flows are
  exclusive, and what a blocked hub says) rather than a patch inside a release.
  - *A pitch calibration holds the device thread for about 13 s and declares no
  exclusive reason, so the hub grants a lease it cannot honour in time.*
  `calibrate_pitch` (`crates/tobii-gtk/src/device.rs:888`) runs a 3 s settle
  plus the 10 s the dialog asks for (`SECS`,
  `crates/tobii-gtk/src/head_model.rs:329`), and its loop (`device.rs:937-958`)
  tests only its own token, never `hub_must_not_open`. It is applied either
  from `device_tick`'s own command drain (`device.rs:709-711`), which is where
  a Start pressed at a hub that has the tracker lands, or from the queue drain
  at the top of `device_session` (`device.rs:1515`) for a command queued while
  the tracker was off. Both run the measurement to completion before the loop's
  lease check (`device.rs:1556`) comes round again. The Pitch-zero dialog takes
  no `Demand` hold at all — `hold_while_open` is called with "display setup",
  "the gaze preview" and "calibration" only
  (`crates/tobii-gtk/src/lib.rs:1118`, `1143`, `1246`, `1350`, `1873`, `1896`)
  — so `wants_exclusive` is false, the refusal branch at
  `crates/tobii-gtk/src/outputs.rs:309-319` is skipped and the lease is granted
  at `outputs.rs:320-326`. Press **Start** in Pitch zero and run `tobii
  headpose` within the next 13 s: the hub sets `Lease::Requested` and cannot
  reach `announce_release`, the CLI's `LEASE_WAIT` of 3 s
  (`crates/tobii-cli/src/main.rs:2029`) expires, and it prints *the hub did not
  answer the request for the tracker within 3s; opening it anyway*
  (`main.rs:2167-2170`) — a sentence whose premise is a wedged or older hub,
  which this one is not — before failing with the same *already claimed by
  another process* the lease was added to remove. - *A command queued while a
  lease is out now waits silently instead of failing.* `stand_by` no longer
  leaves the wait for a non-empty queue — `must_wait`'s third term is an `||`
  (`device.rs:229-231`), which is the intended change, and it took with it the
  only path that answered a queued command's token: the break used to reach
  `device_session`, `UsbTransport::open()` returned `DeviceBusy`, and
  `fail_queued_command` (`device.rs:1405-1416`, called at `device.rs:1608`) put
  the error in `pitch_cal.result`. Now: run `tobii headpose`, then open Pitch
  zero in the hub — the Start button has no connection gate — and press Start.
  The command sits in `pending`, and the dialog's 200 ms poll replaces *Get
  comfortable — starting in 3 seconds…* with *Hold still… 13s*
  (`pitch_progress`, `head_model.rs:61-67`, off the `secs_left = SECS + 3` the
  dialog set itself at `head_model.rs:429`) and then leaves that number frozen
  for as long as the CLI runs — only `calibrate_pitch` ever counts it down —
  with nothing saying why. It is not worse than that: Cancel works, and
  `calibrate_pitch`'s `current(state, token)` check discards the stale command
  rather than running it unobserved when the lease ends. - *With the hub
  standing down, its virtual joystick stays present and the CLI's own is
  suppressed.* The configuration the lease exists for is game output on with
  `joystick = true` and `wake_for_joystick = true` — the one that gives the hub
  its standing claim. `sync_joystick` creates the uinput device from
  `wants_joystick(cfg)` alone (`device.rs:1317-1319`) and `GameSide::poll` runs
  inside the idle wait (`device.rs:1047`, `1067`), so the hub keeps its
  controller while it is in standby. `tobii headpose` therefore finds
  `already_present()` and prints *not presenting a virtual joystick: one with
  this name already exists — the hub owns it*
  (`crates/tobii-cli/src/main.rs:2241-2251`), one line among the command's
  other notes. The command now runs where it used to fail, but for that user
  the joystick sink is dead on both sides: the hub composes no frames while it
  holds no session, so a game bound to the hub's controller sees its axes
  frozen. Only the opentrack and bridge routes carry head pose in that state.

[issue #2]: https://github.com/Tropaion/Tobii_Linux/issues/2

### 11.3g The Wine bridge's discovery keys, and the flags that reach them (new in v0.4.1)

`tobii bridge install` used to write both head-tracking discovery keys blind —
`reg add … /f`, with no read first — into a prefix whose registrations belong to
whoever got there first. The six commits that fixed that (`2d394ac`, then
`2e4dc20`, `1729622`, `e0b4723`, `95259c0` and `b9d0487`) are the bulk of this
release: the non-test half of `crates/tobii-cli/src/bridge.rs` went from 781
lines at v0.4.0 to 1821 at v0.4.1 (3050 at v0.5.0). The design is written up in [[Game-Output]];
what belongs here is how little of it has been run against a prefix that was not
built for the purpose.

- **The three properties the design rests on**, so that a later change can be
  told from a later mistake. `install` reads both keys before it writes, copies
  or creates anything, and refuses rather than clobbering
  (`crates/tobii-cli/src/bridge.rs:1644-1663`) — a refused prefix comes out
  with nothing of ours in it: no directory created, no key touched. That is a
  promise about this installer's writes and not about wine's: reading the keys
  runs wine against the prefix, and wine will bootstrap a directory that is not
  a prefix yet (`system.reg`, `user.reg`, `dosdevices/`, `.update-timestamp`)
  before it can answer at all. `uninstall` deletes a key's `Path` value only
  while it still holds what the record says this program put there, and
  otherwise leaves it alone and names it (`undo_for`, `bridge.rs:2093-2101`,
  driven from `undo_keys`, `bridge.rs:2172-2197`) — or our own install
  directory, which counts as ours whatever the record says, the limitation two
  bullets below. And the record holds **only values this program computed**: a
  value read out of the registry is compared and dropped, never stored and
  never written back (`render_record`'s own header says so in the file it
  writes, `bridge.rs:1195-1203`). The third is what the other two are built on,
  and it is the one that is easy to undo by accident.
- **It took five rounds
  of review to get there, and two of the fixes recreated the fault they were
  sent after.** The first version (`2d394ac`) read before writing *and*
  remembered the value it displaced so it could put it back; the first review
  (`2e4dc20`) found that the restore path committed the same fault on the way
  out — `uninstall` acted on the record alone and fired `reg delete` at a key
  it had never looked at — and its repair kept the mechanism. The second review
  found the same class again in the same mechanism (a value read through wine's
  console codepage written back as the "restored" one; a stale prior
  resurrected over a newer registration), and the answer was to **delete the
  feature rather than repair it a third time** (`1729622`: `Prior`, `Existing`,
  `classify`, the `/t` type round-trip, `Undo::Restore` and every message
  promising to put something back — 284 lines). `--force` now says in those
  words that it promises nothing about the old value. The three rounds after
  that (`e0b4723`, `95259c0`, `b9d0487`) found no defect in that property and
  four, two and four defects in the reading, the recording order and the
  line-splitting underneath it.
- **What was exercised.** 57 unit tests in `bridge.rs` at v0.4.1 (94 at
  v0.5.0), 23 of them driving a *stateful* fake wine (`FAKE_WINE`,
  `bridge.rs:3698-3756`): a shell script that keeps the two `Path` values in
  files and applies `reg add` and `reg delete` to them, so a test can install,
  let another program claim a key, and uninstall — the faults here all live in
  the gap between what a key said at install and what it says at uninstall, and
  canned answers cannot reach them. Two shapes in it are copied from live wine
  11.18 rather than invented, because code depends on both: `reg delete` of an
  absent value exits 1, and `reg query` of an absent key exits 1 while the
  probe key still answers. Beyond the fakes, live wine 11.18 in throwaway
  prefixes settled the byte-level questions and one end-to-end ordering bug —
  the record written *before* the keys, so an install whose `reg add` failed
  still recorded `np wrote <path>` and a later uninstall deleted a registration
  another program had since made there (`e0b4723`). And, twice, by accident:
  the maintainer's own `~/.wine`, which is the prefix `resolve_prefix` falls
  back to when none is named (`bridge.rs:636`), and which is how the missing
  flag gate was found at all.
- **What was NOT exercised: a Proton prefix
  belonging to a running game.** Every live run was a throwaway prefix made for
  the run. No real third-party registration — a Windows opentrack installed
  inside a game's prefix — has been through the refusal, the `--force` path or
  the uninstall rule; the closest is the fake wine holding a foreign value,
  which proves the branch and not the registry it would meet. The staged-rename
  argument for the DLLs (`staging_name`, `bridge.rs:1260-1277`) is likewise
  reasoned from `fs::rename` being atomic within a directory, not measured
  against a game that has the DLL mapped.
- **A key that already holds our own
  path, with nothing recorded, is treated as ours — including when it is not.**
  `is_ours` (`bridge.rs:1138-1143`) answers yes for `C:\tobii-bridge` whatever
  the record says, and `install` skips a key it calls ours before it reaches
  the identical-value test (`stops_install`, `bridge.rs:1168-1188`). The
  [LIMITATION] block on `is_ours` (`bridge.rs:1124-1137`) states the cost in both directions: a
  third-party client that registers itself at the path we pointed a key at,
  *after* we pointed it there, is indistinguishable from our own work and comes
  out on our way out; and a registration another program made at our install
  directory in a prefix we had never touched is adopted, recorded as ours, and
  deleted by `uninstall`. Refusing instead would refuse every upgrade from
  v0.4.0 — which wrote that value and recorded nothing — on every machine with
  opentrack installed. Neither answer can tell the two apart, and this one
  fails towards a prefix the user can re-register in one command.
- **The flag
  gate was added because the install had already written into a real prefix,
  and three spellings still got past it afterwards.** `tobii bridge install
  --help` did not print help: it ignored the flag and performed a real install
  into the default Wine prefix, writing both discovery keys (`c3eaa4c`). After
  the gate went in, the pre-release check found three ways through it, each
  ending in that same install into the prefix the user did not name, and each
  one keystroke from the bug the gate was closing: `-h` and every other
  single-dash token, because the gate stripped only `--`; `--prefix=/path`,
  which passed the name check and was then ignored, because every reader is
  `flag_value` and that compares the whole token; and `--prefix` with nothing
  after it, which falls back the same way. All three are refused at HEAD
  (`reject_unknown_flags`, `bridge.rs:2969-2998`), checked on the built binary
  against a throwaway prefix. The point for this register is not the fix: it is
  that **all four holes — the original and these three — were found by
  reviewers, and the first of them only by a reviewer running the command on a
  real prefix; no user reported any of them**, on the one command in this
  project that writes into somebody else's Wine prefix.

### 11.3h The shortened cards and the F1 help window (new in v0.5.0)

**Two halves of the help window's display test depend on the session, and say
so rather than failing when it will not oblige.** The Tab walk needs the window
to be ACTIVE — `child_focus` is GTK's own focus walk, and in a window the
compositor never brought to the front it reports that it moved and leaves the
focus where it was — and the narrow-layout block needs the 520px breakpoint to be
asked, which is not the same as a granted resize: `relayout` reads
`default-width`, which `set_default_size` sets whether or not a compositor obeys
it, so a refused resize still folds the sidebar. A bare Xwayland therefore runs
that block; what stops it is a window manager holding the window maximised or
fullscreen, where GTK freezes `default-width`. The Tab walk is the one that
needs the window to be at the front. Both blocks then
print a SKIPPED line naming what went unchecked. **Run on 2026-09-27 on the
maintainer's own KDE session, all five display-gated tests passed with zero
skips**, so both halves did run there, including the fold.

Two controls, which say different things and were at one point confused for
each other. Removing the focus handoff inside `toggle.connect_toggled` — the fix
for the leak — makes the test FAIL, but at the precondition above the census,
not at the census: *"the fold must happen with the focus inside the pane it is
about to hide, or the census below is taken across a sequence that cannot leak
and passes for that reason."* That is the vacuous pass closed, and nothing more:
with the precondition relaxed the same run leaves the census at (0, 0).

**The census itself is a working detector, and that is measured.** Stashing a
strong reference to the split box from inside `toggle.connect_toggled`'s
`is_active` branch — a fold-scoped hold, the shape of the original bug — fails
the run at the narrow census with *82 of 87 widgets and 127 of 143 event
controllers outlived*, the magnitude the assertion advertises, while the wide
census 3100ms earlier stays green because the hold does not exist until the
fold. Two runs out of two.


Three card descriptions were shortened and two were removed from the card
altogether — five of the six cards; "Head tracking for games" was already one
line at v0.4.1 and was not touched. One of the two removals is a correction
rather than a cut: "Preview my gaze" said "Shows you a visual trail of your
gaze", and `overlay.rs` draws one circle at the current gaze point per frame
with no history at all. A help window was added to hold what the rest gave
up. The window is the load-bearing part: **GTK4 shows a tooltip on pointer
hover and on nothing else** — there is no focus trigger and no touch trigger —
so a fact that lives only in a tooltip cannot be read with a keyboard or on a
touchscreen, and two cards' guidance is now in exactly that position.

- **The window has to stay discoverable.** F1 is not discovery. The "?" button
  beside the cogwheel is the only door a touch user has, and the cogwheel's
  Help row is the one people look for. Dropping either as clutter would put the
  eyes-card and preview-card guidance out of reach in practice, which is the
  silent fact-dropping this change exists to avoid.
- **F1 is scoped to the hub window** — a controller, deliberately not an
  application accel, because an accel fires inside the fullscreen calibration
  too and a window over the stimulus dot spoils every sample while still
  reporting success. The cost is real: F1 does nothing in the setup and
  calibration flows, which is where a confused user often is. Those flows need
  their own in-flow text, not this window over the top of them.
- **The help window is not in the `REFIT` path.** Changing the text size from
  the cogwheel re-fits the hub and not this; its labels do grow (same CSS
  classes) so nothing clips, but the window scrolls more instead of growing.
  Someone will report that as a bug.
- **A transient window over a hidden parent is the compositor's guess.**
  `destroy_with_parent` does not cover a parent that is merely hidden, which is
  what closing to the tray does, so the hub closes the help window from its own
  unmap handler — tested with an asserted window count, but not against the
  several compositors where this project's other layout surprises came from.
  Minimising deliberately leaves it open, since a minimised hub is one click
  away.
- **The description budget is this machine's.** "About 48 characters" was
  measured at the default font and text scale 1.0; a user at 150% wraps
  earlier. Nothing clips — the window measures its content and re-fits — the
  card simply grows a line. The test states the claim in a form that travels
  (no description may be taller at the width the window opens at than with room
  to spare) rather than as a pixel count that does not.
- **Two cards are now titled "Head tracking" and "Head tracking for games"**,
  and the pair is told apart by their descriptions as much as by their titles:
  "Sends position and angle to games and apps, over opentrack."
  (`crates/tobii-gtk/src/lib.rs:1373-1381`) against "Sends head tracking and
  gaze to a game." (`lib.rs:1403-1410`). Both were deliberately kept — the
  first carries a comment saying why — and a later pass that cut *either* of
  them for another 26px would leave two cards whose titles differ only by "for
  games". The test that would notice is `tests/help_window.rs:1858-1874`, which
  requires the set of cards with no description to be exactly
  `["Select eyes to detect", "Preview my gaze"]`.
- **The window is a topic list, a search box and one topic at a time**, not
  the 2464px scroll it started as. The search is the load-bearing half: it
  opens focused, reads every heading and every body, and requires every word
  typed to appear in the same topic, so a second word narrows
  (`crates/tobii-gtk/src/help.rs:338-373`). The risk it carries is that this
  page is built from the hub's own strings, so it answers for the words those
  strings happen to contain and for no others — a promise nothing enforces.
- **One control's own word is known not to find it.** The three strength
  presets are captioned `Subtle`, `Normal` and `Strong` (`games::STRENGTHS`,
  `crates/tobii-gtk/src/games.rs:41-45`), their row carries no caption, and
  none of the three words is in any topic — so typing `Subtle` or `Strong`
  lands on the no-match page, and `Normal` matches only by accident, through
  the word "normally" in a topic about something else. It is written down in
  `help.rs:1189-1196` as a known gap rather than asserted, because closing it
  means naming the presets FROM that constant instead of retyping them. The
  test beside it does hold the line for the words the window advertises: the
  four the "Keyboard" topic tells a user to try, which since `a7186cc` are the
  same four words printed on the hub, each of which must open a topic that
  really contains it.
- **The folding sidebar leaked the entire window, twice.** Below 520px the
  topic list folds behind a "Topics" button; pressing it and picking a topic
  left 80 of 87 widgets and 127 of 143 event controllers alive after the window
  closed, accumulating linearly for as long as the hub ran — and the retainer
  was not a reference cycle in the handlers but GTK's focus bookkeeping over a
  pane hidden, shown and hidden again with the focus never put back into it.
  The fix is to hand the focus to whichever pane has just come on screen. What
  makes this a standing risk rather than a closed one is that **the guard has
  to be taken through the sequence that leaks**: the first leak census in this
  file ran over a window that was only ever wide and reported zero. There are
  now two, and the narrow one asserts it weak-ref'd at least 40 widgets before
  it asserts none survived (`tests/help_window.rs:1396-1418`).
- **What is measured:** the whole window, end to end on the real `build_hub`,
  748px natural height before and 692px after at an unchanged natural width of
  1241px; cards 142/142/96/194/94/204 against 180/161/141/213/120/204. A
  headless test asserts every tooltip-sourced constant is in `help::topics`,
  and a display test walks the real hub's six cards and requires every tooltip
  it finds, paragraph by paragraph, in the help text; it also drives the real
  window's focus-on-open, its Tab chain, the arrow walk down the topic list,
  Ctrl+F, the breakpoint at 520px and both leak censuses, with GTK's own
  `child_focus` and `move-cursor` rather than with a sentence about them.
  Sixteen control runs reverted each behaviour in turn and watched the test
  fail.
- **What is NOT measured: any of that, in CI.** Every one of those window
  assertions lives in a test marked `#[ignore = "needs a display"]` — five of
  them in `tobii-gtk`, four being whole integration tests — and `ci.yml` runs
  `cargo test --workspace` on a machine with no display and no `xvfb`, so it
  runs none of them. What runs everywhere is the headless half: every
  tooltip-sourced constant is somewhere in `help::topics`, which can see the
  model and never the window built from it. The display test also needs the
  compositor to make its window ACTIVE, which it checks and says so about
  rather than blaming Tab.

### 11.3i The wineserver lock, and what yielding to it does not prove (new in v0.5.0)

`tobii bridge run` now probes the prefix's wineserver lock before it starts and
watches `/proc/locks` for a blocked launch while it runs
(`crates/tobii-cli/src/wineserver.rs`). The mechanism it is built on is
documented in wine's and Proton's own sources and was reproduced here; what it
means for a real game is not.

- **What was measured, on this machine, 2026-09-26.** With one wine process
  holding a throwaway prefix, `wineserver -w` timed out at 4 s (exit 124) and
  returned 0 the instant the holder died. The lock path derived from a `stat`
  of a prefix (`dev 53, ino 15664805` → `/tmp/.wine-1000/server-35-ef06a5/lock`)
  named the file wine had actually created. With the bridge running on that
  prefix, a real `wineserver -w` blocked, the bridge read the waiter out of
  `/proc/locks`, stopped itself, and `wineserver -w` returned 0. The
  already-held branch was checked against a persistent wineserver and named the
  same pid `/proc/locks` did.
- **What is NOT established, and is stated as unknown in the code and in
  [[Game-Output]] rather than assumed.** Whether a `wine` started by `tobii
  bridge run` **joins** a containerised game's wineserver rather than merely
  contending with it: the cross-process proof used host wine on a host prefix.
  The Steam Linux Runtime shares the host `/tmp` (measured: same device and
  inode on both sides), which is why the lock contends at all — but the joining
  half is unconfirmed. If it does not join, yielding still fixes the freeze and
  the user still gets no tracking from `bridge run`.
- **And it does not make a game accept the data.** None of the lock work has
  been run against a real title at all; the two titles that *have* been put in
  front of our `NPClient64.dll` — Star Citizen and MSFS 2024 — both stopped at
  the signature check, which is the whole of this tree's evidence that a game
  reads `HKCU\…\NPClient Location`. The change stops a second process breaking
  the launch. That is its entire claim, and the messages are worded to claim no
  more.
- **What the yield leaves behind is a mapping nobody fills.** On the one
  configuration that needs `bridge run` — TrackIR pointed at a third-party
  client, which is a pure consumer — the provider is the process that stands
  down, and our own DLLs are never loaded because the game loaded the other
  client. So between the stand-down and a manual restart the game has an
  answered signature and an empty `FT_SharedMem`. **"The freeze is fixed" is
  not "the game works"**, and a review of this project found the two had been
  read as the same thing. Every surface that describes this fix
  ([[Game-Output]], the README, the risk register, the hub's setup window) is
  worded to keep them apart.
- **The device in `/proc/locks` is not the device `stat` reports, and getting
  that wrong disabled the whole stand-down on this machine's filesystem.**
  `/proc/locks` prints the *superblock's* device; `btrfs_getattr` hands `stat`
  the subvolume's anonymous device instead. Measured here: `/`, `/home` and
  `/var/tmp` are three subvolumes of one btrfs reporting `st_dev` 31, 53 and
  57, while `/proc/locks` says `00:1d` — device 29 — for a lock on any of them.
  The comparison is now made against the device
  `/proc/self/mountinfo` gives for the mount the lock file is on
  (`crates/tobii-cli/src/wineserver.rs:315-370`). The failure it fixes is the
  worst shape available to this feature: silent, filesystem-dependent, and a
  launch left sitting there having been promised in so many words that it would
  not be.
- **A `SIGTERM` aimed at `tobii` alone orphans the wine child.** `run` installs
  no signal handler; the only thing that kills the child is the yield path
  (`stop`, `crates/tobii-cli/src/bridge.rs:1965-1980`). The child is
  deliberately left in this process's group so that a Ctrl-C at the terminal
  reaches wine too, which is the case a user is actually in — but a `kill
  <tobii pid>`, or a supervisor that signals one pid, leaves a wine process
  alive on the prefix holding exactly the lock this feature exists to get out
  of the way of. Read out of the code, not measured.
- **The watch loop's own I/O is untested.** `blocked_waiter` is a pure function
  checked against the measured `/proc/locks` text and five negatives, and
  `supervise` is checked against a real child with an injected waiter — but the
  wiring between them (the lock file's inode reaching the parser, on a real
  prefix, under a real launch) is covered only by the manual run above, not by
  a test. What a test *does* cover is the inode reaching the parser against text
  the kernel really printed: `stage_a_blocked_waiter`
  (`crates/tobii-cli/src/wineserver.rs:816`) blocks a real waiter with two open
  file descriptions and `F_OFD_SETLKW` on a thread — no fork, which a threaded
  test binary could not do safely — and
  `the_device_compared_is_the_one_the_kernel_prints_for_a_real_waiter`
  (`wineserver.rs:725`) asserts `blocked_waiter` names that waiter on the
  temporary directory, `/var/tmp` and the build directory in turn. What is still
  missing is the rest of the wiring: a real prefix, a real wine launch, and the
  lock path `lock_for` derives from it.
- **The provider's flag parsing has no automated test at all.** `bridge/` is a
  separate workspace that cross-compiles to `x86_64-pc-windows-gnu` and has no
  host test runner — its crates call `advapi32`/`kernel32` directly, so a host
  build does not link. `--register`, `--no-register` and the default were
  checked by hand under wine 11.18 against a throwaway prefix: a seeded
  third-party registration survived a default start and a `--no-register`
  start, and `--register` overwrote it. What *is* tested from the root
  workspace is that `tobii bridge run` passes `--no-register`
  (`bridge.rs`, `run_starts_the_provider_with_the_registry_write_turned_off`).

### 11.3j `bridge status`, and the reads that no longer run wine (new in v0.5.0)

`tobii bridge status` is new, and with it a rule the other subcommands now keep
too: **a `tobii bridge` command that ends up writing nothing must not have
started `wine` to find that out.** `wine reg query` is `wine`, and wine
initialises or upgrades whatever prefix it is pointed at before it answers —
measured with wine 11.18 on throwaway prefixes: 5510 paths created under a
directory holding only `drive_c`, and on a complete prefix whose
`.update-timestamp` was stale (what a Proton prefix looks like to the host's
wine) 2744 lines of `system.reg` rewritten and the stamp overwritten. So
`status` reads the prefix's own `user.reg`
(`crates/tobii-cli/src/userreg.rs`), and an install that refuses or an
uninstall with nothing of ours to remove decides from the same file whenever
the wineserver lock says nothing is serving the prefix (`settled_keys`,
`crates/tobii-cli/src/bridge.rs:1091-1107`). The design is in [[Game-Output]];
what belongs here is how much of it is measured and how much is reasoning.

- **What is measured, and how.** Three tests hand the command a `wine` that
  *wrecks* the prefix if it runs at all — `.update-timestamp` and `system.reg`
  overwritten, `drive_c/windows` deleted (`WRECKING_WINE`,
  `bridge.rs:3769-3774`) — and compare the whole prefix tree before and after,
  every path with its size, its mtime to the nanosecond and a digest of its
  bytes (`status_runs_no_wine_and_leaves_the_prefix_byte_for_byte_as_it_was`,
  `a_refused_install_runs_no_wine_and_leaves_the_prefix_as_it_was`,
  `an_uninstall_with_nothing_to_remove_runs_no_wine`). Each then runs the
  wrecking wine itself and asserts the prefix *did* change, so a script that
  could not be executed at all cannot pass the test by silence. All three were
  re-run against throwaway prefixes on the binary built from this tree while
  this section was written: the prefix hashes identically before and after and
  the fake wine's log stays empty. The figures 2744/5510, and the refusal's own
  2764, came from live wine 11.18 on throwaway prefixes rather than from the
  fakes.
- **What is measured about the reader.** `userreg.rs` is a second
  implementation of wine's own registry reader, and every rule it keeps was
  checked against wine 11.18 one case at a time rather than inferred: the
  header line wine refuses a file over, last-match-wins for a duplicated key or
  value, a header with one trailing separator naming the same key (and the
  three sharper separators that kill the wineserver instead), indented lines,
  whitespace around the `=`, and the `\x` padding rule that makes an escape
  decodable. 22 tests in that file, 94 in `bridge.rs`, 25 in `wineserver.rs`.
- **The reimplementation is the risk, and it has already bitten seven times.**
  Everything above is agreement with **one build on one machine**. Nothing
  re-checks it when wine changes, and the whole surface exists to answer a
  question the authoritative reader would answer for us. **Seven shapes where
  the parser and wine disagreed were found after it was written, every one of
  them by review rather than by use**: three in `f6a0e4f` (an indented header,
  an indented value line, an escaped value name) and four in `be75f1d` (the
  header line wine refuses the whole file over, last-match-wins, a trailing key
  separator, whitespace around the `=`). Most of them had the parser reporting
  "nothing is registered here" about a value wine hands the game — the one
  answer whose next move is to write. Two went the other way and are worth
  telling apart: the missing header made `status` print a registration no
  process in that prefix could see, and last-match-wins made it print the wrong
  one of two. The design contains the damage rather than preventing it: **a
  value that cannot be read exactly is never reported as absent**, so the
  failure mode this surface fails towards is a refusal to act on a key, not a
  clobbered registration.
- **The lag is real and disclosed, not removed.** `user.reg` is the registry as
  last written back; a wineserver holds changes in memory until the last
  process on the prefix exits. `status` says so whenever it finds a live
  wineserver and says nothing of the sort when it does not, since the file is
  then the registry (`staleness`, `bridge.rs:2503-2533`). The commands that
  *act* do not get that luxury and fall through to wine in the same case. The
  window between the lock probe and the write is not closed by any of this —
  it exists for the wine read too, and nothing here holds the prefix.
- **What is NOT exercised: a Proton prefix belonging to a running game.** Every
  live run was a throwaway prefix made for the run, and no real third-party
  registration — a Windows opentrack installed inside a game's prefix — has
  been through the refusal, `--force`, the uninstall rule or this report. The
  stateful fake wine holds a foreign value and proves the branch, not the
  registry it would meet. §11.3g's `is_ours` limitation is unchanged by any of
  this work.
- **And none of it says a game will use the data.** The report is worded to
  describe the prefix and never to predict the title, and a test forbids the
  words "ready", "working", "will work" and "you are all set" in its output
  (`status_never_predicts_what_the_game_will_do`). The reason is two
  measurements, and both are refusals (`NpSource::Installed`,
  `bridge.rs:127-142`): on 2026-08-15 Star Citizen called `NP_GetSignature`,
  got nothing it recognised from our clean-room `NPClient64.dll`, and never
  asked for data again; on 2026-09-27 Microsoft Flight Simulator 2024 (Steam
  app id 2537590, Proton Experimental) called it 104 times in 1m45s, called
  nothing else at all, and went on retrying for as long as it ran. Those are
  the only two titles ever measured against NaturalPoint's check, and neither
  got past it; whether any game accepts our NPClient is unknown, two titles are
  not a rule about the rest, and nothing in `status`, in the refusals or in the
  `--wine` warnings may be read as evidence either way. The verified route is
  still FreeTrack, on a real Elite Dangerous Proton prefix — see
  [[Game-Output]].
- **The flag gate now also refuses a bare positional**, before anything is
  resolved or written (`reject_unknown_flags`, `bridge.rs:2969-2998`, driven
  from the one `SUBS` table at `bridge.rs:3013-3031` so a subcommand cannot be
  checked against one list of flags and run by another). `tobii bridge install
  /games/pfx` used to drop the path and install into `$WINEPREFIX` or
  `~/.wine`, saying so nowhere. This is the fifth hole found in that one gate
  — after `--help` itself, `-h`, `--prefix=PATH` and a valueless `--prefix` —
  and, like the other four, it was found by a reviewer rather than reported by
  a user, on the one command in this project that writes into somebody else's
  Wine prefix. Checked on the built binary: every spelling above is refused,
  for every subcommand, and `games`, which reads no flags at all, refuses them
  too.

### 11.3k Per-game setup: the window, the profile format, and the game nobody has played yet (new in v0.5.0)

The hub gained **Set up a game…**, a window that lists installed Steam titles
and, for one of them, the three things that have to be configured: this
program's settings, the Wine bridge in that game's Proton prefix, and the
game's own configuration files. The third is answered from a **profile** —
`$XDG_CONFIG_HOME/tobii-linux/profiles/<appid>.toml` — authored by
`tobii games profile save|apply|show|forget` and
`tobii games profile check where|add|remove`. The format and the authoring
walkthrough are in [[Game-Profiles]]; what belongs here is the distance between
what is tested and what is known.

- **The headline: no profile has ever been verified against a running game.**
  Not one. `profiles::BUILTIN` is empty, every user is on day one, and this
  project's first per-game fact does not exist yet. Everything below about
  parsing, writing, anchoring and reporting is machinery exercised on
  **fixtures and hand-made files**, never on a claim somebody confirmed by
  playing. The plan — play Elite Dangerous once with the bridge running and
  write down what changed — has not been carried out.
- **What is measured about the readers.** `tobii-gameconf` is 52 tests, and its
  `binds` format was read off **30 real `.binds` documents** on one machine:
  26 with a byte-order mark and 4 without, 4 with mixed line endings, 2 naming
  a schema version and 28 naming none. All 30 answer
  `Bindings_HeadlookModeAccumulate` for `HeadlookMode` — counted, not sampled,
  and re-measured on 2026-09-28 with
  `cargo run -p tobii-gameconf --example read -- presets <ControlSchemes> HeadlookMode`.
  Nothing in CI has those files, so no test holds those numbers; that example
  is what re-takes them.
- **What is NOT measured about the readers: the directory convention.** This is
  the honest limit. The one Elite Dangerous install on this machine has a
  populated Proton prefix (`compatdata/359320/pfx`) and **no Frontier user
  directory, and no `Options` or `Bindings` directory anywhere inside it** —
  confirmed again on 2026-09-28. So the path a real check would name has never
  been observed, and the example path printed by `tobii games profile check` is
  an **illustration of the shape, not a verified location.** What the empty
  prefix does exercise is the case a user most often hits first: a game that
  has saved nothing must be reported as exactly that, with a sentence, and
  never as a game whose settings are fine.
- **A containment property this project claimed and did not have — found by
  review, before release.** Four surfaces said, in four wordings, that a check
  can only ever reach inside the prefix: the module docs and `Check::path` in
  `profiles.rs`, **two `ParseError` messages a user reads**, [[Game-Profiles]]
  under a heading "`path` cannot escape the prefix", and this bullet, which
  called it "a deliberate containment choice". The premise was right —
  `check_path` does refuse a leading `/`, a `..` component and a backslash, and
  the prefix is the only root. The conclusion was wrong, because **a prefix is
  a wine prefix**: `dosdevices/` maps drive letters onto the machine, `z:` is
  `/` in every prefix wine makes, and Steam adds `s:` pointing at the library
  root. Verified on this machine's Elite prefix on 2026-09-28 — both links are
  there. `dosdevices/z:/etc/hostname` has no `..`, no leading `/` and no
  backslash; the parser accepts it and the reader opens the file. Two things
  were demonstrated: `check where` resolved and stat'd outside the prefix, and
  the `attrs` reader parsed a file at an arbitrary absolute path through such a
  check.
- **A third demonstration was written down here and never performed.** This
  bullet claimed that *"a `binds-dir` check on
  `dosdevices/s:/steamapps/common/Elite Dangerous/…/ControlSchemes` read the
  full 30-document census"*. It did not. What was run is
  `cargo run -p tobii-gameconf --example read -- presets <ControlSchemes> …` —
  a **mode of an example**, pointed straight at the directory, and not a
  `profiles::Format` a check can name at all. At the time that was written, a
  `binds-dir` check's own reader answered that directory with *"holds no
  StartPreset file: nothing has saved a control scheme here"* — an absence
  stated over thirty saved control schemes — because it resolved only through
  the live-preset file and shipped presets have none. That was fixed in the
  same review: a `binds-dir` check now answers a directory with no start file
  with one row per shipped preset, saying so first. Measured on 2026-09-28,
  after the fix, on the real directory and again through `dosdevices/s:`:
  **30 rows, all `Bindings_HeadlookModeAccumulate`, 2 naming schema 1.8, 0
  refusals** — the same counts the `presets` mode reports on the same files.
  Naming is not reading, and this file said it was; now both are true and both
  have been run. A demonstration
  recorded in the risk register that nobody performed is the most expensive
  wrong sentence this project can hold — every later reader takes it as the
  thing they no longer have to check.
- **The audit that closed it counted four surfaces; there were ten.** Six more
  still said it when the bullet above declared the matter closed, found by
  grep on 2026-09-28: `tobii games profile check`'s schema — the text
  [[Game-Profiles]] designates **canonical** and tells readers to prefer over
  any copy — both report lines that echo a check back (`profile show` and
  `check add`, each closing with *"under the Proton prefix"* on the last line
  an author reads), [[Game-Profiles]]'s five-key table, and two places in
  `crates/tobii-gtk/src/game_setup.rs` (`run_check`'s doc comment, *"can only
  ever land inside the prefix"*, and the doc of the test that pins it, *"opened
  under the prefix and nowhere else"*). The first four are corrected in the
  same change as this bullet; the two in `game_setup.rs` are another file's,
  and are named here so that the count is the count. The lesson is about the
  count, not the wording: a claim that had been made in four places had been
  made in ten, and "audited, four surfaces, closed" read as a finished job.
- **It was closed by deleting the claim, not the capability.** Refusing
  `dosdevices` would have made the sentence true and removed something a
  profile author legitimately wants: Elite's 30 shipped presets live under
  `steamapps/common/`, this project counted them itself (with the example —
  see above), and `s:` is how a check **names** them — and, since the reader
  was fixed in the same review, reads them too. All ten surfaces now say what
  is true — **a check reaches what the prefix reaches** —
  and the three guards are documented as
  what they are, a rule that keeps the path *relative* so a profile is portable
  between machines and `path_under` has something well-defined to join. The
  canonical schema now teaches the two spellings that leave the prefix
  (`dosdevices/s:/steamapps/common/…`, `dosdevices/z:/…`) — the same ones
  `check_path`'s refusal recommends, and a test compares the two texts rather
  than asserting each alone — and both report lines say *relative to* the
  Proton prefix and add a line, for a `dosdevices/` path only, saying it leaves
  it. The
  regression test is `a_check_path_reaches_what_the_prefix_reaches`
  (`profiles.rs`), which builds a fixture prefix with `s:` and `z:` links and
  fails if either the parser or `path_under` takes the capability back.
- **What actually bounds a profile, and it is the real guarantee.** The readers
  only read; a check names one file and one `setting` and gets back one value
  or a refusal; there is no grammar for writing, for listing, or for returning
  a file's contents. So a profile from a stranger can make this program **read
  a file you could already read and tell you what one attribute of it says** —
  no more, and no less, which is why a `path` starting `dosdevices/z:` is worth
  looking at before running somebody else's profile. That bound is stated on
  [[Game-Profiles]] where a profile author will meet it.
- **Still untested about all of this:** no `tobii-config` test opens a real
  Proton prefix, and no test covers what `check where` prints for a path that
  resolves outside one — the fixture above stands in for both. The cost of the
  old claim is unchanged in one respect: the 30 presets that prove the
  Accumulate default are still not read by anything in CI, because CI has no
  copy of them.
- **The bridge-install path inside this window has never run against a real
  game launch.** The window can install the bridge into a prefix, and block 2's
  reporting was corrected this round so that the page and the install report
  count the same files (only `freetrackclient64.dll` is required; the other two
  are optional and the installer says `skipping`). But no game has been started
  from this window afterwards to see whether it loads anything. §11.3j's
  conclusion stands unchanged and applies here: **both titles ever measured
  against NaturalPoint's signature check stop at it** — Star Citizen rejects
  our `NPClient64.dll` and never asks for data again, and Microsoft Flight
  Simulator 2024 retries the check for as long as it runs — and nothing in
  this window may be read as evidence that any game accepts it. The verified
  route remains FreeTrack.
- **The window is display-tested for lifecycle, not for the correctness of what
  it says.** `tests/game_setup_window.rs` opens it, closes it, and asserts it
  frees itself and takes no claim on the tracker — run under a nested
  `kwin_wayland --virtual`, and `#[ignore]`d because CI has no display. Every
  claim about the *text* is a unit test over the string-building functions.
  That is a real seam: a sentence can be correct in `bridge_block` and never
  reach a user because the widget wiring changed.
- **Comment provenance is the format's only memory, and it is newly
  load-bearing.** A `[[check]]` has no field for who verified it, when, or
  against which build of the game. The one place that can live is a `#` line,
  so the editing commands preserve comments across a rewrite and anchor a
  check's note to **what the check reads** (`format` + `path` + `setting`)
  rather than to its position. When an anchor is gone the write proceeds and
  the comment is printed back in full — **that printout is then the only copy
  in existence.** A profile this build cannot read is the one remaining
  refusal.
- **A defect in exactly that mechanism, found and closed in this review.** A
  comment written *after a value on that value's own line* —
  `format = "binds-dir" # a note` — was dropped by `check remove` **and did not
  appear in the report**, because the reporting path derived the loss itself by
  scanning whole-line comments while the writer orphaned end-of-line ones too.
  Two answers to one question, and the wrong one was the one printed. The CLI
  now takes the orphans `save_to` returns rather than working them out again,
  so there is one answer. Measured on 2026-09-28 on the merged tree: the
  removal goes through and the report names the line, the comment and what it
  sat beside. Provenance notes are safest on
  **their own line**, which is the form every command here tells you to write.
- **Work that merges cleanly and does not compose — seven times in this
  review.** Twice on this feature. Once the CLI grew a loop matching on
  `profiles::CommentLoss` while the config crate deleted that type and made the
  same write succeed-and-report instead; merged, `tobii-cli` did not compile,
  and resolving the error mechanically failed a test pinning a refusal that was
  deliberately no longer a refusal. Once the reader changed
  `binds::Bindings::start` to an `Option` and the window, rewritten in the same
  round, did not take the handoff — five type errors, no git conflict. Both are
  fixed here. The pattern is the finding: every author's gates were green, no
  merge reported a conflict, and in each case one side had reasoned about what
  another file does instead of calling it. The rule that came out of it is in
  §11.5 — a fix that depends on behaviour in a file you do not own must call
  it, and a handoff must be applied by whoever owns the destination, in the
  same round.
- **What the fix reports themselves list as not done.** `Orphans` is not
  `#[must_use]`, which is the marking that would make silently dropping a
  comment impossible; `save_to`'s `Unreadable` branch has no end-to-end
  coverage through the CLI, because all three callers refuse earlier;
  `profiles::shipped_profiles()` exists so that the "this build ships …"
  sentence has a single owner, and the two GTK surfaces now ask it
  (`game_setup.rs:1648`, `help.rs:295`) — but `tobii-cli` still hand-types
  "this build ships no profile for any game" in two places
  (`main.rs:2495`, `main.rs:2550`), so the count is hard-coded wherever a
  terminal user reads it.

### 11.3l The launcher that answers the lock, and what it does not answer (2026-09-28)

§11.3i established that `wineserver -w` is what blocks a Proton launch, and
made `tobii bridge run` yield to it. That diagnosis was right and **incomplete
as an explanation**, and what showed it up is a user's own solution to the same
problem: `https://github.com/markx86/opentrack-launcher` (GPL-3.0). It was read
here for mechanism only. Nothing from it has been fetched, vendored or copied,
and none of what follows was run by anybody on this project.

- **What it does.** It takes Steam's `%command%`, finds the trailing `.exe`,
  and substitutes a three-line batch file that re-runs Steam's own command
  otherwise unchanged: `start "" helper.exe`, then
  `start /wait "" game.exe <args>`, then `taskkill /IM helper.exe /F`.
- **Why the ordering works.** Steam still invokes Proton exactly once, with the
  verb it always used, so there is exactly **one** `waitforexitandrun` and
  therefore exactly **one** `wineserver -w`. Both processes are created after
  it has returned. They do not beat the lock; they are never on the wrong side
  of it. `start /wait` holds `cmd.exe` open for the game's lifetime, so Steam's
  playtime, Stop button and overlay see what they expect, and the `taskkill` is
  load-bearing rather than tidy: a surviving helper is exactly the wine process
  that would make the **next** launch's `wineserver -w` block.
- **What it confirms about our diagnosis.** That `wineserver -w` runs before
  the game, waits for every wine process on the prefix, and that a provider
  started beforehand is one of them. An independent implementation built on the
  same mechanism is the strongest corroboration this project has for §11.3i.
- **Where our diagnosis fell short.** It was carried alongside the assumption —
  stated as an assumption in [[Game-Output]], never measured — that getting a
  second executable into a Steam title's wineserver session means reproducing
  Proton's entire launch environment. This launcher reproduces none of it. The
  assumption was not wrong about what it would take to *re-create* the session;
  it was the wrong question, because the session can be joined from inside the
  launch Steam was already going to make. **"Start the game, then start the
  bridge" was a workaround for a problem that has an ordering fix, and this
  register did not say so.**
- **One user reports MSFS 2024 tracking through this route** — opentrack's
  Windows build run inside the game's Proton prefix, sequenced with the game in
  a single Proton launch, by that launcher. **Nobody here has run it.** It is
  recorded because MSFS 2024 is also one of the two titles measured stopping
  dead at the signature check (§11.3j), so the account and our own measurement
  are about the same title and do not contradict each other — ours says what
  that title does with *our* DLL, theirs says what it did with opentrack's. Why
  their configuration worked is not established here; the obvious reading, that
  a third-party client answered the check and the sequencing filled the mapping
  behind it, is a reading and not a measurement. It is not a recommendation and
  not a supported route.
- **Unmeasured here, as of 2026-09-28.** That the reported configuration works,
  or works for the reason given. That our provider started this way is seen by
  a client DLL in the same launch — the mechanism says one wineserver, but
  nothing here has watched `FT_SharedMem` cross that boundary. That a
  signature-gated title then *uses* the data: an answering client and a filled
  mapping are two conditions and we have measured neither together.
- **This entry describes a launcher we read, not code we ship.** The wrapper
  group is building this project's own sequencing in the same round as this
  entry; at the time of writing their result is not in front of me, so nothing
  here claims anything about it.
- **It was built, and here is the line that entry asked for. It does not
  ship — read the two bullets below before this one.**
  `crates/tobii-cli/src/proton.rs` *was written to* have
  `tobii game -- %command%` recognise a Proton launch by Proton's own argument
  pair — a program named `proton` followed by the verb `waitforexitandrun` —
  and, **only** when `tobii-bridge.exe` was already in that launch's
  `$STEAM_COMPAT_DATA_PATH/pfx`, replace Proton's target with a batch file of
  ours: `start /b` the provider, `start /wait` the game, `taskkill` the
  provider, return the game's saved exit code. Every other case runs the
  command exactly as Steam wrote it.
- **What was run.** Against real wine 11.18 with stub executables: the provider
  starts before the game, the batch blocks for the game's lifetime, the
  provider is reaped, and the game's exit code survives. Each of those four
  broken in turn and watched to fail — and with the `taskkill` removed,
  `wineserver -w` timed out at twelve seconds, which is the next launch's
  freeze reproduced on demand. Fifteen hostile arguments and eight hostile game
  paths arrive byte-identical through `cmd.exe`; removing the `%`-doubling
  collapses five arguments into three, and removing the trailing-backslash
  doubling swallows the closing quote. A path this program cannot spell safely
  — a quote, a control character, a non-ASCII character — declines the rewrite
  and the game still launches.
- **It does not work, and it was the shape that was wrong.** Measured after
  the above, against real Proton rather than plain wine — four builds on this
  machine, `proton-cachyos-11.0-slr`, Experimental, 10.0 and 9.0 (Beta), each
  with a control. Proton's `steam.exe` helper runs an `.exe` target through
  `CreateProcessW` and **waits**; it runs a `.bat` target through
  `ShellExecuteW`, which **does not**. Pointing `waitforexitandrun` at our
  batch therefore has Proton report success about a second in, on all four
  builds, while `cmd.exe`, the provider and the game are still starting:
  `proton_rc=0` after ~1 s where the `.exe` control returns 7 after the full
  run. Four consequences, each measured: Steam records the game as exited
  successfully; `tobii game` returns and drops the tracker while the game is
  still running; the launch file is deleted while `cmd.exe` is reading it, so
  end to end on a cold wineserver **the game never started at all**, three runs
  of three; and the next launch's `wineserver -w` blocked at the 20 s timeout
  on every build — the freeze §11.3i is about, reintroduced by the work meant
  to make it unnecessary.
- **So nothing calls it.** `crates/tobii-cli/src/proton.rs` is kept, uncalled,
  with that written at the top of it: the plan, the quoting, the ordering, the
  reap and the exit code are right and were measured, and what they need is a
  target Proton waits on — an `.exe` of ours that starts the provider and then
  the game. That is a Windows program to write, not a line to change.
  `tobii game -- %command%` does what it did before this work: it holds the
  tracker for the process's lifetime and rewrites nothing.
- **The lesson is the one this register keeps writing down.** The ordering was
  measured against plain wine, and plain wine is not the thing that runs the
  target — Proton's own helper is, and it was never in the test. Two steps were
  listed as unmeasured and one of them was the step that decided the outcome.
- **What it delivered is not known.** No game has been launched through it.
  Two steps between the measured ordering and a game receiving anything are
  unwatched: that Proton accepts a batch file as its target at all (only plain
  wine was tested, not the Steam Linux Runtime's container), and that the
  provider then shares the wineserver session the game is in. The wrapper's own
  note says what it did and not what it achieved, and this entry says the same.
  **A demonstration is still owed**, and the shape of it is one Proton title
  launched this way with `tobii bridge status` and a spike log either side.

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
  pre-release flag came after v0.3.0 and have run on every tag from v0.3.1 on
  (v0.3.1, v0.4.0, v0.4.1, v0.5.0). `aur.yml` fires only when a draft release is
  published by hand; whether it has ever run is not recorded here. Nothing
  records whether the updater's
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
