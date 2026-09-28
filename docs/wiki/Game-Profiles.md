# Game profiles

A **profile** is one file per game that records two different kinds of thing:
this program's own settings, and *checks* — settings in the **game's** own
configuration files that somebody has verified, written down so the program can
read them back and report on them.

> **This program never writes a game's own configuration.** A check is read and
> reported. `tell` is the sentence shown to the person who then has to go and
> change it by hand. There is no command that edits a game's files, and
> `crates/tobii-gameconf` — the crate holding both readers — has a test whose
> whole job is to fail if one appears.

**Zero profiles ship.** `profiles::BUILTIN` is empty on purpose, so on every
fresh install the hub's Games tab has nothing to check and says so.
Everything below is how you write the first one.

## Where they live

```
$XDG_CONFIG_HOME/tobii-linux/profiles/<appid>.toml
```

— or `~/.config/tobii-linux/profiles/` when `XDG_CONFIG_HOME` is unset
(`profiles::profiles_dir()`). **The app id is the file name and is not inside
the file.** `tobii games profile show` prints the directory it used at the
bottom of every listing.

An interrupted save can leave `<appid>.toml.tmp` beside it — every write here
goes through the same atomic write the rest of the program uses, so a profile
is never half-written. `tobii games profile show` lists any leftover `.tmp` in
a paragraph of its own, and `tobii uninstall --purge` removes both it and the
profile. Anything else you keep in that directory — a `359320.toml~` backup, a
`what-i-measured.md` — is reported and kept, never deleted.

## The commands

```
tobii games profile show [<app id or name>]
tobii games profile save <app id or name>
tobii games profile apply <app id or name>
tobii games profile forget <app id or name>
tobii games profile check where|add|remove <app id or name> …
```

`tobii games profile check` **with no verb** prints the whole `[[check]]`
schema on stdout and exits 0. It is the canonical spec — what each key means,
which formats have readers, and a worked block — so prefer running it over
trusting a copy of the format anywhere, this page included. (Note that
`tobii games profile --help` is *not* a form this program accepts; the bare
command prints the usage.)

## The file format

This is a profile **as this program writes one** — the header line, the bare
`version` and `bridge`, and every other value quoted:

```toml
# tobii-linux game profile
version = 1
name = "Elite Dangerous"
bridge = true

[settings]
enabled = "true"
rate_hz = "60"

# measured 2026-10-01, verified on Odyssey 4.0
[[check]]
format = "binds-dir"
path = "drive_c/users/steamuser/Options/Bindings"
setting = "HeadlookMode"
wants = "Bindings_HeadlookModeDirect"
tell = "Set head look to Direct in the game's controls."
```

Three things about that example before you copy it. `wants` is **the author's
claim, not this project's measurement**: what is measured is that all 30
presets Elite ships say `Bindings_HeadlookModeAccumulate` — the value, spelled
exactly like that, is what a `binds-dir` check compares against — and nobody
here has yet played the game to establish which value a head tracker wants.
That is the sort of thing a profile records once somebody has. `[settings]` is
**abridged** —
a real `save` writes every setting this program has, which is about two dozen
lines, not two. And the `path` is **the shape of a path, not a verified
location**: nobody has yet observed where Elite Dangerous keeps a user's
bindings inside a Proton prefix, because the one install this project has
never wrote one. Establishing that is step 1 of the walkthrough below, not
something this page can hand you.

- `version = 1` is **required and must be the first non-comment line**, and it
  is a **bare** number. A profile claiming a version this build does not read
  is refused whole, naming both numbers, rather than half-read.
- `bridge` is a **bare `true` or `false`** — not a string, and not the words
  "required" or "not needed". `true` means this game reads TrackIR or FreeTrack
  through the bridge; `false` means somebody established that it does not.
  Leaving it out is "nobody has said", which is not a "no".
- `name` is display only. Nothing matches on it — `tobii-steam` already has the
  name the machine itself gives.
- `[settings]` are *this program's* settings, and `tobii games profile apply`
  is what puts them back.
- Any number of `[[check]]` blocks follow.

**On quoting:** the writer puts quotes around every value except `version` and
`bridge`, so a file this program produced looks exactly like the one above. A
file *you* typed may leave a bare `true`, `false` or number where a string is
expected — that is accepted on read and comes back as the same text. The two
spellings are one format, not two; the example above is the written form
because that is what you will actually find on disk.

### The five keys of a check

| Key | What it is |
|---|---|
| `format` | Which reader answers this, and so what `path` names. `binds-dir` or `attributes-xml`. |
| `path` | Where that sits, written **relative to** the Proton prefix — the directory holding `drive_c`. Relative is not the same as contained: see below. |
| `setting` | An element name for `binds-dir`, an attribute name for `attributes-xml`. |
| `wants` | The value the game should have, spelled the way the game spells it. |
| `tell` | The sentence shown to whoever has to go and change it by hand. Required. |

**Two formats have readers in this build**, and no others:

- **`binds-dir`** — a directory of Elite Dangerous-style preset documents.
  `path` is the **directory**. If a `StartPreset` file there names a live
  preset, that one is read; if none does, every preset in the directory is
  read and the answer says none of them is in use. So one check can come back
  with many rows.
- **`attributes-xml`** — one flat `<Attributes>` document. `path` is the
  **file**.

A `format` this build has no reader for is not an error. It parses, it is
stored, and every surface that would otherwise look it up says out loud that
nothing will: `check add` says so when you write it, `check where` takes back
its own promise for exactly those checks by name, and a newer build may know
the format.

### `path` stays relative — which is not the same as staying inside

`parse` refuses a leading `/`, a `..` component and a backslash, at the line
that held it (`check_path`, `crates/tobii-config/src/profiles.rs`). What those
three buy is that the path stays **relative**: a profile written on one machine
names the same thing on another, whose prefix lives somewhere else entirely,
and `path_under` can join it onto a prefix without producing nonsense. A
backslash is refused because it is an ordinary character in a Linux path: a
Windows-style path would not fail, it would silently name nothing.

**They are not a security boundary.** Until 2026-09-28 this page said "the
prefix is the only root a check has" and drew a containment conclusion from it.
The root part is true; the conclusion was not, and the reason is what a Proton
prefix *is*:

```
steamapps/compatdata/359320/pfx/dosdevices/
  c: -> ../drive_c
  s: -> /home/you/Daten_2/games/steam      ← the Steam library root (Steam adds this)
  z: -> /                                  ← every wine prefix has this one
```

Those are ordinary directory entries under the prefix. `dosdevices/z:/etc/hostname`
has no `..`, no leading `/` and no backslash, so `check_path` accepts it and the
reader opens `/etc/hostname`. **A check reaches whatever the prefix reaches, and
a wine prefix reaches the machine.** Measured on this project's own Elite prefix;
pinned by `a_check_path_reaches_what_the_prefix_reaches` in `profiles.rs`.

### That is a capability, and it is how you name shipped files

It would be easy to close by refusing `dosdevices`, and that would be the wrong
trade, because the files on the other side of it are ones a profile author
actually wants:

- Elite Dangerous ships **30** `.binds` preset documents under
  `steamapps/common/Elite Dangerous/Products/elite-dangerous-odyssey-64/ControlSchemes/`.
  That is the **Steam library**, not the prefix — and a check names it as
  `dosdevices/s:/steamapps/common/Elite Dangerous/Products/elite-dangerous-odyssey-64/ControlSchemes`,
  which parses. Measured on 2026-09-28 on this project's own install:
  `tobii games profile check where` resolved that path onto the real
  directory and answered *a directory, as binds-dir needs.*
- All 30 of those presets say `Bindings_HeadlookModeAccumulate` — counted, not
  sampled, and re-measured on 2026-09-28 with:

  ```sh
  cargo run -p tobii-gameconf --example read -- presets <ControlSchemes> HeadlookMode
  ```

  So the setting a head tracker cares about is one every user starts on the
  wrong side of.

**Naming them and reading them are two different claims, and this page used to
run them together.** It said a check "has read the whole census through" and
"can read the file that says so". Neither was run. What was run is the command
in the code block above, and `presets` there is a **mode of that example**, not
a `format` a check can name — the format names are the ones
`tobii games profile check` lists.

That conflation had a second cost, and it has been paid. `binds-dir` used to
resolve only through the file that says which preset is **live**, so a
directory of *shipped* presets — which has never had one saved into it —
answered `nothing has saved a control scheme here` about thirty saved control
schemes. A `binds-dir` check now answers both shapes, and says which it found.
Run on this machine on 2026-09-28, on that same path:

```
$ cargo run -q -p tobii-gameconf --example read -- binds <ControlSchemes> HeadlookMode
no start file: nothing here selects a preset, so these are the presets the game
ships and none of them is in use
preset AdvancedControlPad
  HeadlookMode: Bindings_HeadlookModeAccumulate
… 30 rows, every one Accumulate …
```

A directory with a start file still answers with the one preset that file
names. A directory holding neither a start file nor a `.binds` document is
still an absence, and says so naming both. [[Quality-and-Risks]] §11.3k records
what was measured and what was not.

Two cautions that come with using it. `s:` is a Steam convention, not a wine
one, and it points at **one** library root — a game in a second library is not
under it. And a path through a drive letter is only as portable as the drive
letter: `z:` is universal, `s:` is present because Steam made it, and neither is
something the profile can verify before the reader tries.

### What a profile from a stranger can actually do

A profile is somebody's file. Shared in a forum, it is content you are trusting,
so here is the whole of what it can ask this program to do:

> **Read a file you can already read, and tell you what one named setting in it
> says.**

That is the real bound, and unlike the path rule it holds. `[[check]]` has no
grammar for writing — the two readers, `binds` and `attrs`, only ever read. It
has no grammar for listing a directory it did not name, and none for returning a
file's contents: a check names one `setting` and gets back one value, or a
refusal. It runs as you, so it reads nothing you could not `cat` yourself, and
it reports to you and nowhere else.

What it *can* do is point that one-value read at somewhere personal. A `path`
beginning `dosdevices/z:` or `dosdevices/s:` is reaching outside the prefix on
purpose — usually at `steamapps/common`, legitimately — so read those lines
before running a profile you did not write, the same way you would read any
hand-edited file from a stranger.

Everything else a check can reach is whatever the game writes into its own
prefix once you have changed a setting and it has saved. That is the whole
reason the authoring walkthrough below begins with **play the game once**.

## Authoring a profile

Nothing here guesses. Each step records something you established.

### 1. Play the game once, with the bridge running

Until the game has run and saved, there is nothing under the prefix to check —
and this is not a hypothetical: on the one Elite Dangerous install this project
has, the Proton prefix `compatdata/359320/pfx` exists and is populated, but
holds **no Frontier user directory and no `Options` or `Bindings` directory at
all**. A game that has saved nothing is reported as exactly that, with a
sentence, and never as a game whose settings are fine.

So: change the setting in the game's own menus, quit properly, and only then go
looking.

### 2. Find what changed

Take a listing of the prefix before and after, or just search it for something
you recognise. `crates/tobii-gameconf`'s example reads either format without
this program's help, and answers exactly what a check would answer:

```sh
cargo run -p tobii-gameconf --example read -- <binds|attrs|presets> <path> <setting>
```

`binds` resolves through the live-preset file the way a `binds-dir` check does,
`attrs` reads one `attributes-xml` document, and `presets` ignores the live
file and reports every preset document in a directory — which is how the
30-file census above was taken.

### 3. Write down this program's settings

```sh
tobii games profile save 359320
```

This captures **every setting this program has, as it stands right now** — it is
not a judgement about the game, it is this machine's configuration filed under
that app id. Two consequences worth knowing:

- If game output is off when you save, the profile records that, and `apply`
  will turn it off again. `save` warns about exactly this case.
- An existing profile's checks, `bridge` line and `name` are carried across
  untouched. `save` knows nothing better than they do.

### 4. Record the check

```sh
tobii games profile check add 359320 --format <format> --path <path> \
    --setting <name> --wants <value> --tell "<what to do about it>"
```

`check add` creates the profile if there is none, so steps 3 and 4 work in
either order. Confirm where the path lands:

```sh
tobii games profile check where 359320
```

`check where` prints the prefix, then each check's path joined onto it — and
deliberately **does not open the file.** If the game has never run, it says
there is no prefix yet and that every path below is a path with nothing to join
it onto.

To see what the check actually *answers*, open the hub, go to the **Games**
tab (Ctrl+Page Down, or the strip under the title), and pick the game. The
third block runs every check in its profile and prints one row each: the value it
found, or an honest refusal saying which. That is the only place a check is
run — there is no CLI verb that prints a check's answer, and `check where`
will not open a file to get one.

A `binds-dir` check can come back with **many** rows. Pointed at a directory
where a `StartPreset` file names a live preset, it reads that one. Pointed at a
directory of presets a game ships, with nothing selecting one, it reads them
all and says so first — thirty rows, for Elite's `ControlSchemes`.

### 5. Record *how you know* — this is the part with no field for it

A profile has `format`, `path`, `setting`, `wants` and `tell`, and **not one
place to say who verified it, when, or against which version of the game.**
`check add` says so itself when it writes one:

> Nothing here verified any of that. This program did not open the game's
> configuration and could not have told you what the setting should be: the
> claim is yours, and the file is the record that you made it. A `#` line in
> the file is where to date it and say how you know.

So open the profile and add one, directly above the `[[check]]` it is about.
This is the **one hand edit the flow asks for** — there is no `--note` flag,
and a comment is the only field-free space the format has:

```toml
# measured 2026-10-01, verified on Odyssey 4.0
[[check]]
format = "binds-dir"
```

Write it on **its own line**, not after a value — see the known gap below. A
check without a note is an assertion whose author and date are gone the moment
you close the terminal.

## What happens to your comments

Comments are preserved across the editing commands rather than rebuilt away —
a profile is reconstructed from what was parsed out of it, and a comment is not
part of that, so this is deliberate machinery rather than luck.

- A comment comes back **above** the key, table or `[[check]]` it sat above, at
  the **end of** that same line, or **below** it where it sat at the end of the
  file.
- **A check is tracked by what it reads** — `format` + `path` + `setting` — not
  by its position. So adding or removing *other* checks does not move your
  note, and correcting a `wants` or a `tell` in the file keeps it. Changing
  what the check *reads* makes it a different claim, and the note is handed
  back rather than quietly re-pointed at it.

  No command corrects a `wants` or a `tell` in place today: `check add`
  refuses a check that already reads the same `format`, `path` and `setting`
  and tells you to remove it first, and there is no `check edit`. So the
  property above is reached by editing the file yourself — which is exactly
  when it matters, because that is when your note is at risk.
- When a comment's anchor is gone, the write still happens and the comment is
  **printed back in full** in the command's output. **That printout is the only
  remaining copy** — it is in no file any more. Keep the terminal output, or
  copy the note out before you remove the thing it is about.
- The one loss that still refuses the whole write is a profile **this build
  cannot read at all**: there is nowhere to put a comment back in a file whose
  shape is unknown, and nothing in it can be handed back honestly either. Fix
  the file, or move it aside.

A comment written *after a value on that value's own line* —
`format = "binds-dir" # a note` — cannot be carried across a removal, because
lifting it off would mean rewriting a line somebody else wrote. It is handed
back like any other orphan, naming its line and what it sat beside. Notes on
**their own line** are still the form every command here tells you to write,
and the form that moves with what it is about.

## See also

- [[Game-Output]] — the bridge, the joystick and opentrack: what a profile's
  `bridge` line is about.
- [[Quality-and-Risks]] §11.3k — what this surface has and has not been
  measured against.
