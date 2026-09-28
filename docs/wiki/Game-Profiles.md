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
fresh install the hub's game-setup window has nothing to check and says so.
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
wants = "1"
tell = "Set head look to toggle in the game's controls."
```

Two things about that example before you copy it. `[settings]` is **abridged** —
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
| `path` | Where that sits **under the Proton prefix** — the directory holding `drive_c`. |
| `setting` | An element name for `binds-dir`, an attribute name for `attributes-xml`. |
| `wants` | The value the game should have, spelled the way the game spells it. |
| `tell` | The sentence shown to whoever has to go and change it by hand. Required. |

**Two formats have readers in this build**, and no others:

- **`binds-dir`** — a directory of Elite Dangerous-style preset documents, plus
  the file naming which of them is live. `path` is the **directory**.
- **`attributes-xml`** — one flat `<Attributes>` document. `path` is the
  **file**.

A `format` this build has no reader for is not an error. It parses, it is
stored, and every surface that would otherwise look it up says out loud that
nothing will: `check add` says so when you write it, `check where` takes back
its own promise for exactly those checks by name, and a newer build may know
the format.

### `path` cannot escape the prefix — and what that costs

`parse` refuses a leading `/`, a `..` component and a backslash, at the line
that held it (`check_path`, `crates/tobii-config/src/profiles.rs`). A profile
is a hand-edited file whose path is joined onto a prefix and then opened, so
`..` would let one point the reader anywhere on the machine. A backslash is
refused because it is an ordinary character in a Linux path: a Windows-style
path would not fail, it would silently name nothing.

**The prefix is the only root a check has.** That puts a game's *shipped* files
permanently out of reach, and the consequence is concrete rather than
theoretical:

- Elite Dangerous ships **30** `.binds` preset documents under
  `steamapps/common/Elite Dangerous/Products/elite-dangerous-odyssey-64/ControlSchemes/`.
  That is the **Steam library**, not the prefix, so **no check can ever name
  one of them.**
- All 30 of those presets say `Bindings_HeadlookModeAccumulate` — counted, not
  sampled, and re-measured on 2026-09-28 with:

  ```sh
  cargo run -p tobii-gameconf --example read -- presets <ControlSchemes> HeadlookMode
  ```

  So the setting a head tracker cares about is one every user starts on the
  wrong side of, and a check still cannot read the file that says so.

What a check *can* reach is whatever the game writes into its own prefix once
you have changed a setting and it has saved. That is the whole reason the
authoring walkthrough below begins with **play the game once**.

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
  note, and correcting a `wants` or a `tell` keeps it. Changing what the check
  *reads* makes it a different claim, and the note is handed back rather than
  quietly re-pointed at it.
- When a comment's anchor is gone, the write still happens and the comment is
  **printed back in full** in the command's output. **That printout is the only
  remaining copy** — it is in no file any more. Keep the terminal output, or
  copy the note out before you remove the thing it is about.
- The one loss that still refuses the whole write is a profile **this build
  cannot read at all**: there is nowhere to put a comment back in a file whose
  shape is unknown, and nothing in it can be handed back honestly either. Fix
  the file, or move it aside.

> **Known gap, as of 2026-09-28:** a comment written *after a value on that
> value's own line* — `format = "binds-dir" # a note` — is dropped by a removal
> **without appearing in the report.** The reporting path only inspects
> whole-line comments, so this one is lost silently. Until that is fixed, put
> provenance notes on **their own line**, which is the form every command here
> tells you to write and the form that is reported correctly. See
> [[Quality-and-Risks]] §11.3k.

## See also

- [[Game-Output]] — the bridge, the joystick and opentrack: what a profile's
  `bridge` line is about.
- [[Quality-and-Risks]] §11.3k — what this surface has and has not been
  measured against.
