#!/usr/bin/env bash
# Checks for scripts/install-payload.sh, the install.sh wrapper that
# scripts/release.sh writes into every release tarball, and the install front
# end of scripts/build.sh.
#
# No root and nothing outside a temp directory: TOBII_TEST_EUID stands in for the
# uid and TOBII_SYSTEM_DATA_DIR for /usr/local/share, HOME and XDG_* point into
# the temp tree, and the udev step is always skipped. build.sh runs from a copy
# of the scripts, with stand-ins for cargo, the compiler and sudo, so nothing is
# built and nothing is written to this checkout.
#
#   bash scripts/test-install-payload.sh
#
# Every check's condition is a string `check` evals, so shellcheck sees its
# expansions as single-quoted and the variables only it reads as unused.
# shellcheck disable=SC2016,SC2034
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
payload="$root/scripts/install-payload.sh"
assets="$root/assets"
tmp="$(mktemp -d)"
sleeper=""
cleanup() { if [[ -n "$sleeper" ]]; then kill "$sleeper" 2>/dev/null || true; fi; rm -rf "$tmp"; }
trap cleanup EXIT
fails=0
ok()  { echo "ok    $1"; }
bad() { echo "FAIL  $1"; fails=$((fails + 1)); }
check() { if eval "$2"; then ok "$1"; else bad "$1"; fi; }

# Stand-ins for the binaries: enough to be copied and to answer --version.
src="$tmp/src"; mkdir -p "$src"
for b in tobii tobii-gtk; do
    printf '#!/bin/sh\necho "%s 9.9.9"\n' "$b" > "$src/$b"; chmod 755 "$src/$b"
done

entry=com.tobiilinux.Configuration.desktop

fresh_home() {
    HOME="$tmp/home-$1"; mkdir -p "$HOME"
    XDG_DATA_HOME="$HOME/.local/share"; XDG_CONFIG_HOME="$HOME/.config"
    export HOME XDG_DATA_HOME XDG_CONFIG_HOME
    a="$XDG_CONFIG_HOME/autostart/$entry"
}
# Every run installs the stand-in binaries and the real assets, into <dir>.
payload_as() { local uid="$1" dir="$2"; shift 2; TOBII_TEST_EUID="$uid" bash "$payload" "$dir" "$src" "$assets" "$@" --no-udev </dev/null; }
# The current home's login entry: a [Desktop Entry] header, then one line per argument.
login_entry() { mkdir -p "${a%/*}"; { echo '[Desktop Entry]'; printf '%s\n' "$@"; } > "$a"; }
# The login entry byte for byte as the hub writes it (autostart::entry_text in
# tobii-config), for an Exec argument already quoted and escaped the hub's way.
hub_entry() {
    mkdir -p "${a%/*}"
    printf '[Desktop Entry]\nType=Application\nVersion=1.5\nName=Tobii Eye Tracker\nComment=Keep the Tobii hub ready in the background\nExec=%s --background\nIcon=com.tobiilinux.Configuration\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n' "$1" > "$a"
}

# --- an ordinary per-user install
fresh_home user
payload_as 1000 "$HOME/.local/bin" >/dev/null 2>&1
check "user: both binaries installed" '[[ -x "$HOME/.local/bin/tobii" && -x "$HOME/.local/bin/tobii-gtk" ]]'
check "user: the menu entry runs the absolute path, quoted" 'grep -qx "Exec=\"$HOME/.local/bin/tobii-gtk\"" "$XDG_DATA_HOME/applications/$entry"'
check "user: the icon is installed" '[[ -f "$XDG_DATA_HOME/icons/hicolor/scalable/apps/com.tobiilinux.Configuration.svg" ]]'
check "user: the manifest names the bindir" 'grep -qx "bindir=$HOME/.local/bin" "$XDG_DATA_HOME/tobii-linux/installs"'
check "user: no icon cache is created" '[[ ! -e "$XDG_DATA_HOME/icons/hicolor/icon-theme.cache" ]]'
payload_as 1000 "$HOME/.local/bin" >/dev/null 2>&1
check "user: installing again adds no second manifest line" '[[ $(grep -c . "$XDG_DATA_HOME/tobii-linux/installs") -eq 1 ]]'

# --- an icon cache that already exists is refreshed, not left stale
if command -v gtk4-update-icon-cache >/dev/null 2>&1 || command -v gtk-update-icon-cache >/dev/null 2>&1; then
    fresh_home cache
    mkdir -p "$XDG_DATA_HOME/icons/hicolor"; : > "$XDG_DATA_HOME/icons/hicolor/icon-theme.cache"
    payload_as 1000 "$HOME/.local/bin" >/dev/null 2>&1
    check "cache: an existing icon cache is refreshed" '[[ -s "$XDG_DATA_HOME/icons/hicolor/icon-theme.cache" ]]'
else
    echo "skip  cache: no gtk-update-icon-cache here"
fi

# --- root without --system is refused, before anything is written
fresh_home root
check "root: refused without --system" '! payload_as 0 "$HOME/.local/bin" >/dev/null 2>&1'
check "root: nothing was written" '[[ ! -e "$HOME/.local" ]]'

# --- --system without root is refused, before anything is written
fresh_home notroot
check "--system: refused without root" '! TOBII_SYSTEM_DATA_DIR="$tmp/notroot-share" payload_as 1000 "$tmp/sysbin-x" --system >/dev/null 2>&1'
check "--system: and nothing was written" '[[ ! -e "$tmp/sysbin-x" && ! -e "$tmp/notroot-share" ]]'

# --- --system as root: everything under the system data dir, nothing in HOME
fresh_home sys
sysdata="$tmp/usr-local-share"; sysbin="$tmp/usr-local-bin"
TOBII_SYSTEM_DATA_DIR="$sysdata" payload_as 0 "$sysbin" --system >/dev/null 2>&1
check "--system: binaries installed" '[[ -x "$sysbin/tobii-gtk" ]]'
check "--system: the menu entry is under the system data dir" '[[ -f "$sysdata/applications/$entry" ]]'
check "--system: the manifest is under the system data dir" 'grep -qx "bindir=$sysbin" "$sysdata/tobii-linux/installs"'
check "--system: nothing under HOME" '[[ ! -e "$HOME/.local" && ! -e "$HOME/.config" ]]'

# --- --system writes what every account can read, whatever the caller's umask
fresh_home umask
( umask 027; TOBII_SYSTEM_DATA_DIR="$tmp/um-share" payload_as 0 "$tmp/um/opt/bin" --system >/dev/null 2>&1 ) || true
check "--system: under umask 027 the menu entry and icon are 644" '[[ $(stat -c %a "$tmp/um-share/applications/$entry" "$tmp/um-share/icons/hicolor/scalable/apps/com.tobiilinux.Configuration.svg" | sort -u) == 644 ]]'
check "--system: and the directories it made are 755" '[[ $(stat -c %a "$tmp/um" "$tmp/um/opt" "$tmp/um/opt/bin" "$tmp/um-share/applications" | sort -u) == 755 ]]'

# --- --system refuses a directory another account can change
fresh_home sysshared
mkdir -p "$tmp/g"; chmod 777 "$tmp/g"
out="$(TOBII_SYSTEM_DATA_DIR="$tmp/g-share" payload_as 0 "$tmp/g/bin" --system 2>&1)" && rc=0 || rc=$?
check "--system: under a directory anyone can write, refused" '[[ $rc -ne 0 && "$out" == *"only root can change, and $tmp/g is not"* ]]'
check "--system: and nothing was written" '[[ ! -e "$tmp/g/bin" && ! -e "$tmp/g-share" ]]'
mkdir -p "$tmp/st"; chmod 1777 "$tmp/st"
check "--system: a shared sticky directory as the target is refused" '! TOBII_SYSTEM_DATA_DIR="$tmp/st-share" payload_as 0 "$tmp/st" --system >/dev/null 2>&1'
mkdir -p "$tmp/st/own"; chmod 755 "$tmp/st/own"
check "--system: a shared sticky directory above the target is allowed" 'TOBII_SYSTEM_DATA_DIR="$tmp/st-share" payload_as 0 "$tmp/st/own/bin" --system >/dev/null 2>&1'
# Another account's directory: made with chown under root (CI), otherwise one
# that already exists here, and not writable by this user even without the check.
if [[ $(id -u) -eq 0 ]]; then
    mkdir -p "$tmp/theirs"; chown 65534 "$tmp/theirs"; theirs="$tmp/theirs"
else
    theirs="$(find / -maxdepth 3 \( -path /proc -o -path /sys -o -path /dev -o -path /run \) -prune \
        -o -type d ! -uid 0 ! -uid "$(id -u)" ! -perm /022 -print -quit 2>/dev/null || true)"
fi
if [[ -n "$theirs" ]]; then
    out="$(TOBII_SYSTEM_DATA_DIR="$tmp/theirs-share" payload_as 0 "$theirs/tobii-test-bin" --system 2>&1)" || true
    check "--system: inside another account's directory, refused" '[[ "$out" == *"only root can change, and $theirs is not"* ]]'
else
    echo "skip  --system: no directory of another account here"
fi
# Group-writable by a group that is not the caller's.
if [[ $(id -u) -eq 0 ]]; then other_gid=65534
else other_gid="$(id -G | tr ' ' '\n' | grep -vx "$(id -g)" | head -n1 || true)"; fi
if [[ -n "$other_gid" ]]; then
    mkdir -p "$tmp/grp"; chgrp "$other_gid" "$tmp/grp"; chmod 775 "$tmp/grp"
    check "--system: under a directory another group can write, refused" '! TOBII_SYSTEM_DATA_DIR="$tmp/grp-share" payload_as 0 "$tmp/grp/bin" --system >/dev/null 2>&1'
else
    echo "skip  --system: this user is in no second group"
fi

# --- a login entry pointing at a binary that no longer exists is repaired
fresh_home auto
login_entry Type=Application 'Exec="/nonexistent/tobii-gtk" --background' X-GNOME-Autostart-enabled=true
payload_as 1000 "$HOME/.local/bin" >/dev/null 2>&1
check "autostart: a dead Exec now runs this install" 'grep -qx "Exec=\"$HOME/.local/bin/tobii-gtk\" --background" "$a"'
check "autostart: its other keys are untouched" 'grep -qx "X-GNOME-Autostart-enabled=true" "$a"'

# --- a login entry pointing at another copy that still exists is left alone
fresh_home auto2
other="$tmp/other-bin"; mkdir -p "$other"; cp "$src/tobii-gtk" "$other/"
login_entry "Exec=\"$other/tobii-gtk\" --background"
out="$(payload_as 1000 "$HOME/.local/bin" 2>&1)"
check "autostart: another existing copy is left in place" 'grep -qx "Exec=\"$other/tobii-gtk\" --background" "$a"'
check "autostart: and is named" '[[ "$out" == *"$other/tobii-gtk"* ]]'

# --- the hub's login entry for a directory with every character its escaping
# touches: a space, %, $, `, " and \. The Exec argument is the bytes entry_text
# writes: `%` doubled, `$` `` ` `` `"` behind two backslashes, `\` as four.
fresh_home escaped
odd_name='sp ace%c$d`e"f\g'
odd_esc='sp ace%%c\\$d\\`e\\"f\\\\g'
mkdir -p "$tmp/$odd_name"; cp "$src/tobii-gtk" "$tmp/$odd_name/"
hub_entry "\"$tmp/$odd_esc/tobii-gtk\""; cp "$a" "$tmp/escaped.before"
out="$(payload_as 1000 "$HOME/.local/bin" 2>&1)" || true
check "escaped: the hub's entry for a copy that exists is left alone" 'cmp -s "$a" "$tmp/escaped.before"'
check "escaped: and that copy is named as it is" '[[ "$out" == *"runs $tmp/$odd_name/tobii-gtk, not this copy"* ]]'
hub_entry "\"$tmp/gone-$odd_esc/tobii-gtk\""
out="$(payload_as 1000 "$HOME/.local/bin" 2>&1)" || true
check "escaped: the hub's entry for a copy that is gone is repaired" 'grep -qx "Exec=\"$HOME/.local/bin/tobii-gtk\" --background" "$a"'
check "escaped: and the gone copy is named as it was" '[[ "$out" == *"it ran $tmp/gone-$odd_name/tobii-gtk, which no longer exists"* ]]'

# --- a hub running from another copy is named, with its pid
fresh_home running
mkdir -p "$tmp/elsewhere"; cp "$(command -v sleep)" "$tmp/elsewhere/tobii-gtk"
"$tmp/elsewhere/tobii-gtk" 60 & sleeper=$!
out="$(payload_as 1000 "$HOME/.local/bin" 2>&1)"
check "running: another running copy is named with its pid" '[[ "$out" == *"$tmp/elsewhere/tobii-gtk (pid $sleeper)"* ]]'
kill "$sleeper"; wait "$sleeper" 2>/dev/null || true; sleeper=""

# --- a relative bindir comes out absolute, in the entry and in the manifest
fresh_home relative
( cd "$HOME" && payload_as 1000 "rel/bin" >/dev/null 2>&1 )
check "relative: the menu entry names the absolute path" 'grep -qx "Exec=\"$HOME/rel/bin/tobii-gtk\"" "$XDG_DATA_HOME/applications/$entry"'
check "relative: so does the manifest" 'grep -qx "bindir=$HOME/rel/bin" "$XDG_DATA_HOME/tobii-linux/installs"'

# --- a --system run leaves the invoking user's login entry alone, dead or not
fresh_home sysauto
login_entry 'Exec="/nonexistent/tobii-gtk" --background'; cp "$a" "$tmp/sysauto.before"
TOBII_SYSTEM_DATA_DIR="$tmp/sysauto-share" payload_as 0 "$tmp/sysauto-bin" --system >/dev/null 2>&1
check "--system: the user's login entry is byte-identical" 'cmp -s "$a" "$tmp/sysauto.before"'

# --- installing over a running hub says it is still the previous version
fresh_home previous
mkdir -p "$HOME/.local/bin"; cp "$(command -v sleep)" "$HOME/.local/bin/tobii-gtk"
"$HOME/.local/bin/tobii-gtk" 60 & sleeper=$!
out="$(payload_as 1000 "$HOME/.local/bin" 2>&1)"
check "previous: a hub running the replaced binary is named, with its pid" '[[ "$out" == *"still running the previous version"*"(pid $sleeper)"* ]]'
kill "$sleeper"; wait "$sleeper" 2>/dev/null || true; sleeper=""

# --- a directory with a space, &, | and % in its name
# (A failing install is not fatal here: it is what the checks below report, by name.)
fresh_home odd
odd="$HOME/odd dir &|%/bin"
payload_as 1000 "$odd" >/dev/null 2>&1 || true
check "odd: the install completed (manifest written)" 'grep -qxF "bindir=$odd" "$XDG_DATA_HOME/tobii-linux/installs"'
check "odd: Exec is quoted, with % doubled" 'grep -qxF "Exec=\"$HOME/odd dir &|%%/bin/tobii-gtk\"" "$XDG_DATA_HOME/applications/$entry"'
login_entry 'Exec="/nonexistent/tobii-gtk" --background'
payload_as 1000 "$odd" >/dev/null 2>&1 || true
check "odd: the dead login entry is repaired to the quoted path" 'grep -qxF "Exec=\"$HOME/odd dir &|%%/bin/tobii-gtk\" --background" "$a"'

# --- a directory with a double quote gets no menu entry, but is installed
fresh_home quote
qd="$HOME/say \"hi\"/bin"
out="$(payload_as 1000 "$qd" 2>&1)" || true
check "quote: no menu entry is written" '[[ ! -e "$XDG_DATA_HOME/applications/$entry" ]]'
check "quote: and it says why" '[[ "$out" == *"No menu entry"* ]]'
check "quote: the install completed (manifest written)" 'grep -qxF "bindir=$qd" "$XDG_DATA_HOME/tobii-linux/installs"'

# --- a menu entry and icon this user cannot write do not stop the install.
# Skipped as root, whom chmod does not bind (CI's container runs as root).
if [[ $(id -u) -ne 0 ]]; then
    fresh_home readonly
    ro_apps="$XDG_DATA_HOME/applications"; ro_icons="$XDG_DATA_HOME/icons/hicolor/scalable/apps"
    mkdir -p "$ro_apps" "$ro_icons"
    printf '[Desktop Entry]\nExec=/old/tobii-gtk\n' > "$ro_apps/$entry"
    echo '<svg/>' > "$ro_icons/com.tobiilinux.Configuration.svg"
    chmod 444 "$ro_apps/$entry" "$ro_icons/com.tobiilinux.Configuration.svg"
    login_entry 'Exec="/nonexistent/tobii-gtk" --background'
    payload_as 1000 "$HOME/.local/bin" >/dev/null 2>&1 && rc=0 || rc=$?
    check "readonly: an entry and icon this user cannot write do not stop the install" '[[ $rc -eq 0 ]]'
    check "readonly: the entry is replaced, by rename" 'grep -qx "Exec=\"$HOME/.local/bin/tobii-gtk\"" "$ro_apps/$entry"'
    check "readonly: so is the icon" 'cmp -s "$assets/com.tobiilinux.Configuration.svg" "$ro_icons/com.tobiilinux.Configuration.svg"'
    check "readonly: the manifest is still written" 'grep -qx "bindir=$HOME/.local/bin" "$XDG_DATA_HOME/tobii-linux/installs"'
    check "readonly: and the dead login entry still repaired" 'grep -qx "Exec=\"$HOME/.local/bin/tobii-gtk\" --background" "$a"'
    chmod 555 "$ro_apps"
    out="$(payload_as 1000 "$HOME/.local/bin" 2>&1)" && rc=0 || rc=$?
    chmod 755 "$ro_apps"
    check "readonly: an applications folder this user cannot write is said, not fatal" '[[ $rc -eq 0 && "$out" == *"menu entry was not updated"* && "$out" != *"  $ro_apps/$entry"* ]]'
else
    echo "skip  readonly: running as root, whom chmod does not bind"
fi

# --- login entries the repair must leave alone
fresh_home keep
mkdir -p "$tmp/pathbin" "$tmp/pct%dir" "$tmp/mid q"
cp "$src/tobii-gtk" "$tmp/pathbin/"; cp "$src/tobii-gtk" "$tmp/pct%dir/"; cp "$src/tobii-gtk" "$tmp/mid q/"
login_entry 'Exec=tobii-gtk --background'
PATH="$tmp/pathbin:$PATH" payload_as 1000 "$HOME/.local/bin" >/dev/null 2>&1
check "keep: a bare name found on PATH is left alone" 'grep -qx "Exec=tobii-gtk --background" "$a"'
login_entry 'Exec=env GDK_BACKEND=x11 /nonexistent/tobii-gtk --background'
payload_as 1000 "$HOME/.local/bin" >/dev/null 2>&1
check "keep: an env wrapper is left alone" 'grep -qx "Exec=env GDK_BACKEND=x11 /nonexistent/tobii-gtk --background" "$a"'
login_entry "Exec=\"$tmp/pct%%dir/tobii-gtk\" --background"; cp "$a" "$tmp/keep.before"
payload_as 1000 "$HOME/.local/bin" >/dev/null 2>&1
check "keep: a quoted path escaped as the hub writes it (%%) is read as the copy it is" 'cmp -s "$a" "$tmp/keep.before"'
login_entry "Exec=$tmp/mid\" q\"/tobii-gtk --background"; cp "$a" "$tmp/keep.before"
payload_as 1000 "$HOME/.local/bin" >/dev/null 2>&1
check "keep: quotes inside the program's path are read as a launcher reads them" 'cmp -s "$a" "$tmp/keep.before"'
# Two Exec keys: a dead one first, a live one second. A reader that took the
# first would "repair" an entry GKeyFile starts the live copy from.
login_entry 'Exec="/nonexistent/tobii-gtk" --background' "Exec=\"$tmp/pathbin/tobii-gtk\" --background"
cp "$a" "$tmp/keep.before"
out="$(payload_as 1000 "$HOME/.local/bin" 2>&1)"
check "keep: an entry with two Exec keys is left alone, as tobii uninstall leaves it" 'cmp -s "$a" "$tmp/keep.before"'
check "keep: and it says why" '[[ "$out" == *"two Exec lines"* ]]'
# An Exec in another group comes first: only [Desktop Entry]'s is read, and
# only that line is rewritten.
mkdir -p "${a%/*}"
printf '[Desktop Action other]\nExec=%s\n[Desktop Entry]\n  Exec = "/nonexistent/tobii-gtk" --background\n' "$tmp/pathbin/tobii-gtk" > "$a"
payload_as 1000 "$HOME/.local/bin" >/dev/null 2>&1
check "repair: only the [Desktop Entry] group's Exec is read and rewritten" '[[ "$(sed -n 2p "$a")" == "Exec=$tmp/pathbin/tobii-gtk" && "$(sed -n 4p "$a")" == "Exec=\"$HOME/.local/bin/tobii-gtk\" --background" ]]'
rm -f "$a"; printf '[Desktop Entry]\nExec="/nonexistent/tobii-gtk" --background\n' > "$tmp/dotfile.desktop"
ln -s "$tmp/dotfile.desktop" "$a"
payload_as 1000 "$HOME/.local/bin" >/dev/null 2>&1
check "keep: a symlinked login entry stays a symlink, its target unchanged" '[[ -L "$a" ]] && grep -qx "Exec=\"/nonexistent/tobii-gtk\" --background" "$tmp/dotfile.desktop"'

# --- a bindir reached through a symlink that is on PATH is on PATH
fresh_home linkpath
mkdir -p "$tmp/realbin"; ln -s "$tmp/realbin" "$HOME/linkbin"
out="$(PATH="$HOME/linkbin:$PATH" payload_as 1000 "$HOME/linkbin" 2>&1)"
check "path: a symlinked PATH entry counts as on PATH" '[[ "$out" != *"is not in your PATH"* ]]'

# --- a relative XDG_DATA_HOME is ignored
fresh_home relxdg
( cd "$HOME" && XDG_DATA_HOME="rel-share" payload_as 1000 "$HOME/.local/bin" >/dev/null 2>&1 )
check "xdg: a relative XDG_DATA_HOME is ignored" '[[ -f "$HOME/.local/share/applications/$entry" && ! -e "$HOME/rel-share" ]]'

# --- a mistyped flag is refused, not ignored
check "flags: a mistyped flag is refused" '! payload_as 1000 "$tmp/typo" --no-udve >/dev/null 2>&1'

# --- the tarball's install.sh, with the payload replaced by one that prints its arguments
w="$tmp/wrapper"; mkdir -p "$w/assets"
awk "/<<'INSTALLER'\$/{f=1;next} /^INSTALLER\$/{f=0} f" "$root/scripts/release.sh" > "$w/install.sh"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$@"\n' > "$w/assets/install-payload.sh"
got="$(bash "$w/install.sh" --no-udev --system)"
check "wrapper: --system anywhere installs into /usr/local/bin" '[[ "$(sed -n 1p <<<"$got")" == /usr/local/bin ]]'
check "wrapper: --system is passed on" 'grep -qx -- --system <<<"$got"'
check "wrapper: other flags are passed on" 'grep -qx -- --no-udev <<<"$got"'
got="$(HOME=/home/u bash "$w/install.sh")"
check "wrapper: the default is ~/.local/bin" '[[ "$(sed -n 1p <<<"$got")" == /home/u/.local/bin ]]'
got="$(bash "$w/install.sh" /opt/tobii --lean)"
check "wrapper: an explicit directory wins" '[[ "$(sed -n 1p <<<"$got")" == /opt/tobii ]]'

# --- build.sh's install front end, run from $tmp/cwd. Its copy of the scripts
# has its own target/, which the stand-in cargo fills with the stand-in
# binaries; the stand-in sudo runs the payload through the root test seam.
br="$tmp/build-repo"; fake="$tmp/fakebin"
mkdir -p "$br/scripts" "$fake" "$tmp/cwd"
cp "$root/scripts/build.sh" "$payload" "$br/scripts/"; cp -R "$assets" "$br/assets"
printf '#!/bin/sh\nmkdir -p target/release && cp "%s/tobii" "%s/tobii-gtk" target/release/\n' "$src" "$src" > "$fake/cargo"
printf '#!/bin/sh\n[ "$1" = --modversion ] && echo 0\nexit 0\n' > "$fake/pkg-config"
printf '#!/bin/sh\n' > "$fake/cc"
printf '#!/bin/sh\nexec env TOBII_TEST_EUID=0 "$@"\n' > "$fake/sudo"
chmod 755 "$fake"/*
build_as() { ( cd "$tmp/cwd" && PATH="$fake:$PATH" TOBII_TEST_EUID=1000 bash "$br/scripts/build.sh" "$@" </dev/null ); }
fresh_home build
out="$(build_as --system 2>&1)" && rc=0 || rc=$?
check "build: --system alone is refused, not ignored" '[[ $rc -eq 2 && "$out" == *"--install --system"* ]]'
check "build: and refused before building" '[[ ! -e "$br/target" ]]'
build_as --install rel/bin >/dev/null 2>&1 || true
check "build: a relative --install DIR is where build.sh was run" '[[ -x "$tmp/cwd/rel/bin/tobii-gtk" && ! -e "$br/rel" ]]'
check "build: and the manifest names it there" 'grep -qx "bindir=$tmp/cwd/rel/bin" "$XDG_DATA_HOME/tobii-linux/installs"'
build_as --install --lean >/dev/null 2>&1 || true
check "build: --install with no DIR installs into ~/.local/bin" '[[ -x "$HOME/.local/bin/tobii" ]]'
TOBII_SYSTEM_DATA_DIR="$tmp/bsys-share" build_as --install sysrel --system >/dev/null 2>&1 || true
check "build: --install DIR --system installs there, for every user" '[[ -x "$tmp/cwd/sysrel/tobii-gtk" ]] && grep -qx "bindir=$tmp/cwd/sysrel" "$tmp/bsys-share/tobii-linux/installs"'

echo
if [[ $fails -eq 0 ]]; then echo "all install-script checks passed"; else echo "$fails check(s) failed"; exit 1; fi
