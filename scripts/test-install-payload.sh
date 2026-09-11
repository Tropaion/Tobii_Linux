#!/usr/bin/env bash
# Checks for scripts/install-payload.sh and the install.sh wrapper that
# scripts/release.sh writes into every release tarball.
#
# No root and nothing outside a temp directory: TOBII_TEST_EUID stands in for the
# uid and TOBII_SYSTEM_DATA_DIR for /usr/local/share, HOME and XDG_* point into
# the temp tree, and the udev step is always skipped.
#
#   bash scripts/test-install-payload.sh
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

fresh_home() {
    HOME="$tmp/home-$1"; mkdir -p "$HOME"
    XDG_DATA_HOME="$HOME/.local/share"; XDG_CONFIG_HOME="$HOME/.config"
    export HOME XDG_DATA_HOME XDG_CONFIG_HOME
}
payload_as() { local uid="$1"; shift; TOBII_TEST_EUID="$uid" bash "$payload" "$@" --no-udev </dev/null; }

entry=com.tobiilinux.Configuration.desktop

# --- an ordinary per-user install
fresh_home user
payload_as 1000 "$HOME/.local/bin" "$src" "$assets" >/dev/null 2>&1
check "user: both binaries installed" '[[ -x "$HOME/.local/bin/tobii" && -x "$HOME/.local/bin/tobii-gtk" ]]'
check "user: the menu entry runs the absolute path, quoted" 'grep -qx "Exec=\"$HOME/.local/bin/tobii-gtk\"" "$XDG_DATA_HOME/applications/$entry"'
check "user: the icon is installed" '[[ -f "$XDG_DATA_HOME/icons/hicolor/scalable/apps/com.tobiilinux.Configuration.svg" ]]'
check "user: the manifest names the bindir" 'grep -qx "bindir=$HOME/.local/bin" "$XDG_DATA_HOME/tobii-linux/installs"'
check "user: no icon cache is created" '[[ ! -e "$XDG_DATA_HOME/icons/hicolor/icon-theme.cache" ]]'
payload_as 1000 "$HOME/.local/bin" "$src" "$assets" >/dev/null 2>&1
check "user: installing again adds no second manifest line" '[[ $(grep -c . "$XDG_DATA_HOME/tobii-linux/installs") -eq 1 ]]'

# --- an icon cache that already exists is refreshed, not left stale
if command -v gtk4-update-icon-cache >/dev/null 2>&1 || command -v gtk-update-icon-cache >/dev/null 2>&1; then
    fresh_home cache
    mkdir -p "$XDG_DATA_HOME/icons/hicolor"; : > "$XDG_DATA_HOME/icons/hicolor/icon-theme.cache"
    payload_as 1000 "$HOME/.local/bin" "$src" "$assets" >/dev/null 2>&1
    check "cache: an existing icon cache is refreshed" '[[ -s "$XDG_DATA_HOME/icons/hicolor/icon-theme.cache" ]]'
else
    echo "skip  cache: no gtk-update-icon-cache here"
fi

# --- root without --system is refused, before anything is written
fresh_home root
if payload_as 0 "$HOME/.local/bin" "$src" "$assets" >/dev/null 2>&1; then bad "root: refused without --system"
else ok "root: refused without --system"; fi
check "root: nothing was written" '[[ ! -e "$HOME/.local" ]]'

# --- --system without root is refused
fresh_home notroot
if payload_as 1000 "$tmp/sysbin-x" "$src" "$assets" --system >/dev/null 2>&1; then bad "--system: refused without root"
else ok "--system: refused without root"; fi

# --- --system as root: everything under the system data dir, nothing in HOME
fresh_home sys
sysdata="$tmp/usr-local-share"; sysbin="$tmp/usr-local-bin"
TOBII_SYSTEM_DATA_DIR="$sysdata" payload_as 0 "$sysbin" "$src" "$assets" --system >/dev/null 2>&1
check "--system: binaries installed" '[[ -x "$sysbin/tobii-gtk" ]]'
check "--system: the menu entry is under the system data dir" '[[ -f "$sysdata/applications/$entry" ]]'
check "--system: the manifest is under the system data dir" 'grep -qx "bindir=$sysbin" "$sysdata/tobii-linux/installs"'
check "--system: nothing under HOME" '[[ ! -e "$HOME/.local" && ! -e "$HOME/.config" ]]'

# --- a login entry pointing at a binary that no longer exists is repaired
fresh_home auto
mkdir -p "$XDG_CONFIG_HOME/autostart"; a="$XDG_CONFIG_HOME/autostart/$entry"
printf '[Desktop Entry]\nType=Application\nExec="/nonexistent/tobii-gtk" --background\nX-GNOME-Autostart-enabled=true\n' > "$a"
payload_as 1000 "$HOME/.local/bin" "$src" "$assets" >/dev/null 2>&1
check "autostart: a dead Exec now runs this install" 'grep -qx "Exec=\"$HOME/.local/bin/tobii-gtk\" --background" "$a"'
check "autostart: its other keys are untouched" 'grep -qx "X-GNOME-Autostart-enabled=true" "$a"'

# --- a login entry pointing at another copy that still exists is left alone
fresh_home auto2
other="$tmp/other-bin"; mkdir -p "$other" "$XDG_CONFIG_HOME/autostart"; cp "$src/tobii-gtk" "$other/"
printf '[Desktop Entry]\nExec="%s/tobii-gtk" --background\n' "$other" > "$XDG_CONFIG_HOME/autostart/$entry"
out="$(payload_as 1000 "$HOME/.local/bin" "$src" "$assets" 2>&1)"
check "autostart: another existing copy is left in place" 'grep -qx "Exec=\"$other/tobii-gtk\" --background" "$XDG_CONFIG_HOME/autostart/$entry"'
check "autostart: and is named" '[[ "$out" == *"$other/tobii-gtk"* ]]'

# --- a hub running from another copy is named, with its pid
fresh_home running
mkdir -p "$tmp/elsewhere"; cp "$(command -v sleep)" "$tmp/elsewhere/tobii-gtk"
"$tmp/elsewhere/tobii-gtk" 60 & sleeper=$!
out="$(payload_as 1000 "$HOME/.local/bin" "$src" "$assets" 2>&1)"
check "running: another running copy is named with its pid" '[[ "$out" == *"$tmp/elsewhere/tobii-gtk (pid $sleeper)"* ]]'
kill "$sleeper"; wait "$sleeper" 2>/dev/null || true; sleeper=""

# --- a relative bindir comes out absolute, in the entry and in the manifest
fresh_home relative
( cd "$HOME" && payload_as 1000 "rel/bin" "$src" "$assets" >/dev/null 2>&1 )
check "relative: the menu entry names the absolute path" 'grep -qx "Exec=\"$HOME/rel/bin/tobii-gtk\"" "$XDG_DATA_HOME/applications/$entry"'
check "relative: so does the manifest" 'grep -qx "bindir=$HOME/rel/bin" "$XDG_DATA_HOME/tobii-linux/installs"'

# --- a --system run leaves the invoking user's login entry alone, dead or not
fresh_home sysauto
mkdir -p "$XDG_CONFIG_HOME/autostart"; a="$XDG_CONFIG_HOME/autostart/$entry"
printf '[Desktop Entry]\nExec="/nonexistent/tobii-gtk" --background\n' > "$a"; cp "$a" "$tmp/sysauto.before"
TOBII_SYSTEM_DATA_DIR="$tmp/sysauto-share" payload_as 0 "$tmp/sysauto-bin" "$src" "$assets" --system >/dev/null 2>&1
check "--system: the user's login entry is byte-identical" 'cmp -s "$a" "$tmp/sysauto.before"'

# --- installing over a running hub says it is still the previous version
fresh_home previous
mkdir -p "$HOME/.local/bin"; cp "$(command -v sleep)" "$HOME/.local/bin/tobii-gtk"
"$HOME/.local/bin/tobii-gtk" 60 & sleeper=$!
out="$(payload_as 1000 "$HOME/.local/bin" "$src" "$assets" 2>&1)"
check "previous: a hub running the replaced binary is named, with its pid" '[[ "$out" == *"still running the previous version"*"(pid $sleeper)"* ]]'
kill "$sleeper"; wait "$sleeper" 2>/dev/null || true; sleeper=""

# --- a directory with a space, &, | and % in its name
# (A failing install is not fatal here: it is what the checks below report, by name.)
fresh_home odd
odd="$HOME/odd dir &|%/bin"
payload_as 1000 "$odd" "$src" "$assets" >/dev/null 2>&1 || true
check "odd: the install completed (manifest written)" 'grep -qxF "bindir=$odd" "$XDG_DATA_HOME/tobii-linux/installs"'
check "odd: Exec is quoted, with % doubled" 'grep -qxF "Exec=\"$HOME/odd dir &|%%/bin/tobii-gtk\"" "$XDG_DATA_HOME/applications/$entry"'
mkdir -p "$XDG_CONFIG_HOME/autostart"; a="$XDG_CONFIG_HOME/autostart/$entry"
printf '[Desktop Entry]\nExec="/nonexistent/tobii-gtk" --background\n' > "$a"
payload_as 1000 "$odd" "$src" "$assets" >/dev/null 2>&1 || true
check "odd: the dead login entry is repaired to the quoted path" 'grep -qxF "Exec=\"$HOME/odd dir &|%%/bin/tobii-gtk\" --background" "$a"'

# --- a directory with a double quote gets no menu entry, but is installed
fresh_home quote
qd="$HOME/say \"hi\"/bin"
out="$(payload_as 1000 "$qd" "$src" "$assets" 2>&1)" || true
check "quote: no menu entry is written" '[[ ! -e "$XDG_DATA_HOME/applications/$entry" ]]'
check "quote: and it says why" '[[ "$out" == *"No menu entry"* ]]'
check "quote: the install completed (manifest written)" 'grep -qxF "bindir=$qd" "$XDG_DATA_HOME/tobii-linux/installs"'

# --- login entries the repair must leave alone
fresh_home keep
mkdir -p "$XDG_CONFIG_HOME/autostart" "$tmp/pathbin" "$tmp/pct%dir"; a="$XDG_CONFIG_HOME/autostart/$entry"
cp "$src/tobii-gtk" "$tmp/pathbin/"; cp "$src/tobii-gtk" "$tmp/pct%dir/"
printf '[Desktop Entry]\nExec=tobii-gtk --background\n' > "$a"
PATH="$tmp/pathbin:$PATH" payload_as 1000 "$HOME/.local/bin" "$src" "$assets" >/dev/null 2>&1
check "keep: a bare name found on PATH is left alone" 'grep -qx "Exec=tobii-gtk --background" "$a"'
printf '[Desktop Entry]\nExec=env GDK_BACKEND=x11 /nonexistent/tobii-gtk --background\n' > "$a"
payload_as 1000 "$HOME/.local/bin" "$src" "$assets" >/dev/null 2>&1
check "keep: an env wrapper is left alone" 'grep -qx "Exec=env GDK_BACKEND=x11 /nonexistent/tobii-gtk --background" "$a"'
printf '[Desktop Entry]\nExec="%s" --background\n' "$tmp/pct%%dir/tobii-gtk" > "$a"; cp "$a" "$tmp/keep.before"
payload_as 1000 "$HOME/.local/bin" "$src" "$assets" >/dev/null 2>&1
check "keep: a quoted path escaped as the hub writes it (%%) is read as the copy it is" 'cmp -s "$a" "$tmp/keep.before"'
rm -f "$a"; printf '[Desktop Entry]\nExec="/nonexistent/tobii-gtk" --background\n' > "$tmp/dotfile.desktop"
ln -s "$tmp/dotfile.desktop" "$a"
payload_as 1000 "$HOME/.local/bin" "$src" "$assets" >/dev/null 2>&1
check "keep: a symlinked login entry stays a symlink, its target unchanged" '[[ -L "$a" ]] && grep -qx "Exec=\"/nonexistent/tobii-gtk\" --background" "$tmp/dotfile.desktop"'

# --- a bindir reached through a symlink that is on PATH is on PATH
fresh_home linkpath
mkdir -p "$tmp/realbin"; ln -s "$tmp/realbin" "$HOME/linkbin"
out="$(PATH="$HOME/linkbin:$PATH" payload_as 1000 "$HOME/linkbin" "$src" "$assets" 2>&1)"
check "path: a symlinked PATH entry counts as on PATH" '[[ "$out" != *"is not in your PATH"* ]]'

# --- a relative XDG_DATA_HOME is ignored
fresh_home relxdg
( cd "$HOME" && XDG_DATA_HOME="rel-share" payload_as 1000 "$HOME/.local/bin" "$src" "$assets" >/dev/null 2>&1 )
check "xdg: a relative XDG_DATA_HOME is ignored" '[[ -f "$HOME/.local/share/applications/$entry" && ! -e "$HOME/rel-share" ]]'

# --- a mistyped flag is refused, not ignored
if TOBII_TEST_EUID=1000 bash "$payload" "$tmp/typo" "$src" "$assets" --no-udve </dev/null >/dev/null 2>&1; then
    bad "flags: a mistyped flag is refused"
else ok "flags: a mistyped flag is refused"; fi

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

echo
if [[ $fails -eq 0 ]]; then echo "all install-script checks passed"; else echo "$fails check(s) failed"; exit 1; fi
