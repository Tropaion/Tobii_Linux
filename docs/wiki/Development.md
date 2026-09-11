# Development

How to build, test, and change this project. Architecture background:
[[Architecture]] · [[Runtime-View]] · [[Architecture-Decisions]] ·
[[Quality-and-Risks]].

---

## Getting a build

```sh
scripts/build.sh            # checks dependencies first, then builds release
scripts/build.sh --check    # only check dependencies
scripts/build.sh --lean     # CLI only, no neural backend (1.2 MB vs 35 MB)
scripts/build.sh --install  # + install to ~/.local/bin and the app menu
scripts/build.sh --udev     # + install the device rule
```

`--install` and `--udev` both hand off to `scripts/install-payload.sh`, which is
also what the `install.sh` inside a release tarball runs. One definition of what
"installed" means, so a source install and a release install cannot drift — the
archive used to ship no installer at all, and the notes told people to unpack it
into `~/.local/bin`, which unpacks a versioned directory rather than binaries.

The dependency check exists because a missing GTK development package does not
fail with "GTK is missing" — it fails several minutes in with hundreds of linker
errors about undefined symbols.

**Use rustup.** `rust-toolchain.toml` pins the compiler and its components, and
rustup honours it automatically, so your `clippy` and `rustfmt` are the ones CI
runs. Distribution Rust ignores the pin and may be too old to build this at all.

---

## The three checks

These are exactly what CI runs, and they pass on `main`:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

`-D warnings` on a floating toolchain would be a time bomb — a new clippy turns
every open pull request red with no code change. That is why the toolchain is
pinned; see [[Architecture-Decisions]] §18.

---

## Testing

### What exists

| Crate | Tests | Shape |
|---|---|---|
| `tobii-protocol` | 80 | Inline unit tests over captured real frames |
| `tobii-usb` | 51 | 43 unit + 8 replay (see below) |
| `tobii-config` | 53 | Unit; SHA-256 cross-checked against coreutils |
| `tobii-headpose` | 93 (+7 ignored) | The ignored ones need the 13 MB model |
| `tobii-update` | 67 | 59 unit + 8 install end-to-end |
| `tobii-diagnostics` | 17 | The report and the log; a test fails if it leaks a home path |
| `tobii-cli` | 9 | Argument parsing and text helpers |
| `tobii-gtk` | 191 | Inline; pure logic split out from widget code |
| `tobii-recap` | 32 | 29 unit + 3 integration |
| **Total** | **593** | |

Counted with `cargo test -p <crate>`, not from memory: this table said "41 unit
+ 6 replay" for `tobii-usb` long after both numbers had moved, which is the
failure mode of every hand-maintained count.

There are almost no integration-test directories: the convention is inline
`#[cfg(test)]` modules next to the code, with pure logic deliberately factored
out of widget and I/O code so it can be tested at all.

### Testing the protocol without a tracker

Most of what "needs an ET5" does not, once a session has been recorded.

```sh
tobii record                 # the everyday session
tobii record --calibration   # + the one fragmented response there is
cargo test -p tobii-usb --test replay
```

Two captures, because they want different things. `session.tobiicap` is 62 lines — short
enough that a re-recording produces a diff a human can read, which is why the
format is line-oriented hex at all, though the longest line is still 3,450
characters. `calibration.tobiicap` is 1.5 MB
whose payload is a handful of enormous lines, and is opaque on purpose: it
exists to exercise reassembly of a response larger than the 16 KB read buffer.

**A capture you record is your own eye model.** `--calibration` retrieves the
calibration blob the tracker computed from your eyes, and the header carries the
display geometry it was recorded with. The committed fixture is the
maintainer's. If you re-record and open a pull request, that is what you are
publishing — which is fine, and worth knowing first. `tobii debug` deliberately
contains no calibration data for the same reason.

`tobii record` refuses rather than damaging a fixture: an unknown `--flag` is an
error (a typo'd `--calibraton` used to record a plain session straight over
`session.tobiicap`), a frame count passed to `--calibration` is an error, and a
failed calibration retrieve writes nothing at all.

`tobii record` wraps the USB transport and writes every frame, both directions,
to a line-oriented hex file with headers. The replay tests drive the driver
against exactly the replies the device gave and assert **byte-for-byte on what
the driver sends** — which is where the reverse engineering lives.

It catches what it exists to catch: changing `OP_SUBSCRIBE` from `0x4c4` to
`0x4c5` fails with a hex diff naming the frame and the byte.

**A capture is a photograph, not a specification.** It proves the code still
does what it did when the recording was taken — not that the device still does.
Re-record after a firmware update. The header carries when it was taken and the
display geometry used, so the byte comparison reproduces on any machine.

Its limits are worth knowing before you trust a green run: no camera stream and
no error paths, and because `RecordTransport` wraps the *outer* transport,
replaying exercises none of `UsbTransport` itself.

**Why the calibration capture exists.** A guard added to the parser assumed a
continuation envelope's length field fits inside the USB read carrying it. It
passed every test in the workspace and broke every calibration retrieval on real
hardware, because the field is the size of the whole continuation *run* — a
genuine envelope announcing 778,188 bytes arrives in a 100-byte read. Nothing
could catch it: no capture had a fragmented response, and every parser unit test
built an envelope whose length happened to equal its chunk. Re-introducing that
clause now fails `a_fragmented_calibration_blob_is_reassembled_exactly` with the
same `NoResponse { op: 0x44c }` the hardware gave.

### What cannot be tested without hardware

The accuracy figure, whether the tracker physically detects eyes, whether the
illuminators actually go dark, and everything below the `Transport` seam. Those
were measured by hand — [[Quality-and-Risks]] §10 says which and with what
numbers. **If you change the protocol layer or the device thread, say in your
pull request what you tested against a real ET5.**

### Useful measurement techniques

Several claims in this project were settled by observation rather than argument,
and the techniques are reusable:

- **Is the tracker actually on?** `ls -l /proc/<pid>/fd | grep bus/usb`. A GSK
  renderer fd (`/dev/dri/renderD128`) appears only when a window is really
  mapped — useful for proving a "windowless" mode is windowless.
- **Is the device free?** Run `tobii stream` alongside. Only one process can
  claim the interface.
- **Does a test actually catch anything?** Break the thing it guards and watch it
  fail. Several tests in this repo were found vacuous exactly this way.
- **Protocol archaeology:** `tobii-recap` decodes a usbmon pcap — including
  captures of Tobii's own Windows software in a passed-through VM — into a
  directional op catalog. See [[Reverse-Engineering-Methodology]].

---

## Reporting a bug

`tobii debug` prints the report the issue form asks for. It answers, in one
paste, most of what a triager would otherwise have to ask for over three
round trips.

It is written to be read before it is sent: no calibration data, the monitor id
hashed (it is derived from the EDID serial), and no username, home path or
hostname. There is a test that fails if any of those appear in the output — and
it earned its keep: it caught `tilde()` folding a home path only at the *start*
of a string, so a path in the middle of a log line went into the report intact.

### The log

There is no logging framework here — no `log`, no `tracing` — for the same
reason SHA-256 and JSON are hand-rolled: a driver installable from source pays
for every crate in its tree. `tobii_diagnostics::log::warn` writes three places
at once: stderr (so nothing that was visible in a terminal stops being), a
60-line ring in memory, and a 128 KB-capped file at
`$XDG_STATE_HOME/tobii-linux/tobii.log`.

The reason it exists: **a hub launched from the application menu has its stderr
wired to the journal or to `/dev/null`.** Every warning the GUI produced —
"could not apply saved calibration" being the one that matters most — was
written somewhere the user would never look, so a bug report arrived saying
"tracking is bad" with no way to recover what the program already knew.

`TOBII_LOG_FILE` overrides the path, which is also how this crate's own tests
avoid flooding the real log.

Issue forms are under `.github/ISSUE_TEMPLATE/`. Blank issues are disabled so
the bug form cannot be bypassed by accident, with links out to Discussions for
anything that is not a bug — forcing a question through a bug form gets a worse
answer. Note the constraint that shaped this: GitHub issue forms **can** mark a
field required, but have **no file-upload field type**, so a pasted block is the
only thing that can actually be enforced.

## Conventions

**Comments explain *why*, and cite the measurement.** This codebase is unusually
dense in them and that is deliberate: the protocol is undocumented, so the
reason a constant has its value is often the only record that exists. Do not
strip them for brevity.

**Mark your confidence.** [CONFIRMED] / [CODE-VERIFIED] / [HYPOTHESIS] on
protocol claims. An unmarked claim reads as unproven.

**Do not assert what you have not checked.** The project's worst bugs and its
worst documentation have the same cause. If you cannot measure something, say
so in the same sentence.

**Hand-rolled is often deliberate.** SHA-256, JSON, MD5 and the curl/wget fetch
are all local, to keep a driver installable from source. MD5 in particular is
*dictated by the device*, not a security choice — swapping it out breaks the
handshake.

---

## Traps

Collected from the source and from bugs that actually happened.

**Protocol**
- The USB envelope length field is **asymmetric**: outbound it excludes the
  8-byte envelope, inbound it includes it.
- Every payload begins with a **2-byte prefix** the caller must skip.
- **Three TLV framings** exist. Do not assume the one you learned first.
- `EnabledEye`'s Rust order (Both, Left, Right) is **not** the wire order
  (1=Left, 2=Right, 3=Both) — always go through `to_wire`/`from_wire`.
- For `enabled_eye`, **SET (`0xc58`) is numerically lower than GET (`0xc62`)**,
  unlike every other mapped pair.
- There is **no "0 means both"** for a calibration point's eye argument —
  passing 0 selects no eye, the device acks the point and drops it. That is the
  bug that made every calibration before 2026-08-15 a silent no-op.
- A **present bit is not validity.** Gate on both.

**Device**
- The ET5 **wipes display area and calibration on every reboot**, and reboots on
  every session close. Re-applying on connect is not an optimisation target.
- Without a valid display area the device reports **no eyes at all** — which
  presents as broken hardware, not as a config problem.

**GUI**
- `recv` returning `None` is a timeout, not an error and not a disconnect.
- Never touch a widget off the GTK main thread.
- A `DemandGuard` leaked anywhere means the illuminators never go out — in
  background mode, until logout.
- `GApplication` emits `activate` on launch whenever argv has nothing left to
  handle. `--background` is filtered out of argv, so it *does* fire.

---

## Releasing

Four things go in the release commit; the tag build fails without the first two.
`release.sh` refuses when `Cargo.toml` disagrees with the tag, and every gate
runs `--locked`, so a bumped manifest with a stale lock fails before it builds:

1. `[workspace.package] version` in **both** `Cargo.toml` and `bridge/Cargo.toml`.
2. **Both** lock files — a separate step: `cargo update -w --offline`, then the
   same with `--manifest-path bridge/Cargo.toml`.
3. `docs/releases/vX.Y.Z.md`. This is the published changelog and the in-app
   *What's new*; without it CI falls back to raw commit subjects.
4. A `docs/wiki/Quality-and-Risks.md` section for whatever new surface ships.

Then `git push origin main` (the tag must point at a pushed commit) and
`git tag vX.Y.Z && git push origin vX.Y.Z`. CI builds in a Debian 13 container,
enforces the glibc floor,
checks that the `.deb` and `.rpm` actually *declare* it, verifies the checksums
and publishes a **draft** — the release notes are the changelog every user's
updater shows them, so a human sees them first.

**Pre-release tags** must look like `v1.0.0-rc1`: the suffix starts with a
letter, and `release.yml` rejects anything else. Each format has its own
spelling of it: the PKGBUILDs strip the `-` (`1.0.0rc1`, which `vercmp` sorts
below `1.0.0`), and the `.deb` and `.rpm` use `~` (`1.0.0~rc1`, their own "sorts
before" marker; an rpm Version may not contain `-` at all). A numeric suffix
such as `-1` would become `1.0.01`, which `vercmp` sorts *above* `1.0.0`. File
names keep the tag's `-`, because GitHub rewrites `~` in an asset's name to `.`
and `SHA256SUMS` would then list a file the release does not have.

**The Arch package.** After `build`, the `arch` job repackages the same tarball
in an `archlinux:base-devel` container. `package.sh` has already written
`aur/tobii-linux-bin/PKGBUILD` (`scripts/aur-bin.sh`, which derives `depends`
from the binaries' NEEDED entries and refuses a soname it has no Arch package
for). `makepkg` builds it as an unprivileged user from the tarball in its start
directory, checking it against the pin, since the draft's URL would 404.
`pacman -U` installs it on the clean container. Both binaries must then answer
`--version` from `/usr/bin` with every library resolved, and the package must
declare `glibc>=`. The `.pkg.tar.zst` joins `dist/`, `SHA256SUMS` is rewritten
over every file, and every file must be listed and named the way a hub's
matcher looks for it. `publish` uploads that set. The tested PKGBUILD and its
`.SRCINFO` are kept as the `aur-bin` artifact.

**The AUR.** `.github/workflows/aur.yml` runs when a release becomes a full
release — published as one, or a pre-release promoted later — or by hand: *Actions → AUR → Run workflow*, with a
tag. It downloads the published tarball and `SHA256SUMS`, checks one against the
other, regenerates the PKGBUILD, compares its pin with the release's
`SHA256SUMS` entry, lets `makepkg --verifysource` download from the published
URL and check the pin, writes `.SRCINFO`, and pushes to
`ssh://aur@aur.archlinux.org/tobii-linux-bin.git`. The pin is an integrity check
taken from the same release, not a signature. It proves an AUR user got what was
published, and nothing about who published it.

One-time setup, before the first push:

1. An AUR account: <https://aur.archlinux.org/register>.
2. A key used for nothing else:
   `ssh-keygen -t ed25519 -N '' -C tobii-linux-bin -f aur_key`. Paste
   `aur_key.pub` into *My Account → SSH Public Key* on the AUR.
3. The private key, `aur_key`, as the repository secret `AUR_SSH_KEY`
   (*Settings → Secrets and variables → Actions*). Then delete the local copy,
   or keep it somewhere you would keep a password.
4. Run the workflow by hand for the first release that ships `tobii-linux-bin`
   — v0.3.1 or later, not v0.3.0, whose hub does not know the prebuilt package
   and whose `tobii` has no `uninstall`. **The first push creates the package**
   under your account; there is nothing to register beforehand.

To re-publish a release after a packaging fix, run it by hand with a higher
`pkgrel`: AUR helpers upgrade only when `pkgver-pkgrel` changes, so a fix pushed
under the same pkgrel reaches new installs only.

Until the secret exists, the job does everything except the push, prints what it
would have pushed, and ends green with a notice. The AUR's SSH host keys are
pinned in `aur.yml`. If the AUR ever rotates them, the push fails closed:
compare the new fingerprints with the ones listed on <https://aur.archlinux.org>
before replacing the lines.

The `.deb`/`.rpm` check is in CI rather than in `package.sh` because most developer
machines have no `rpmbuild`, so the spec's dependencies cannot be read where
they are written. That is exactly how `AutoReqProv: no` sat in the spec silently
declaring no glibc requirement at all.

`release.sh` builds with `--remap-path-prefix`, so no build machine's absolute
paths reach the published binaries (there were ~500 per binary) and a panic in a
bug report names `crates/…` instead. `[profile.release] trim-paths` is the
proper answer and should replace it — it is still unstable in Cargo 1.98.

Never cut a release from a developer machine. The build host *is* the
compatibility floor; see [[Runtime-View]] §7.
