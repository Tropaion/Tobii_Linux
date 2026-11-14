#!/usr/bin/env bash
# Run tobii-gtk's display tests in a nested compositor.
#
# They are `#[ignore = "needs a display"]` because CI has none, and because
# running them on the session display steals focus and pops windows in front of
# whatever you are doing. A nested `kwin_wayland --virtual` gives them a
# compositor of their own, off screen, that supports `wlr-layer-shell` — which
# the gaze overlay needs and which Weston and Xvfb do not provide.
#
#     scripts/display-tests.sh            # all of them
#     scripts/display-tests.sh games_tab  # one file in tests/
#     scripts/display-tests.sh lib        # the ones inside src/
#
# Nothing here is run by CI. `.github/workflows/ci.yml` runs `cargo test
# --workspace --locked`, which compiles these and skips every one of them, so
# what they assert is verified by a person running this and by nothing else.
# `docs/wiki/Quality-and-Risks.md` records that as a known gap; this script
# exists so that closing it by hand is one command instead of a paragraph.
set -euo pipefail

cd "$(dirname "$0")/.."

# With no names, every ignored test in the crate: each one is a display test,
# so there is no list here to fall behind when another is added.
TARGETS=(--tests)
if [ $# -gt 0 ]; then
    TARGETS=()
    for t in "$@"; do
        if [ "$t" = lib ]; then TARGETS+=(--lib); else TARGETS+=(--test "$t"); fi
    done
fi

command -v kwin_wayland >/dev/null || {
    echo "kwin_wayland is not installed; it is what provides wlr-layer-shell here" >&2
    exit 1
}
for tool in dbus-run-session setsid; do
    command -v "$tool" >/dev/null || {
        echo "$tool is not installed; the nested KWin needs it to stay out of your session" >&2
        exit 1
    }
done
# GNU tail's `--pid`, which is what ends KWin with this script (see below).
tail --pid=$$ -n 0 /dev/null 2>/dev/null || {
    echo "this needs a tail with --pid (GNU coreutils)" >&2
    exit 1
}
: "${XDG_RUNTIME_DIR:?is not set; run this from a desktop session}"

SOCKET="wl-tobii-$$"
DISPLAY_SOCKET="$XDG_RUNTIME_DIR/$SOCKET"
LOG="${TMPDIR:-/tmp}/kwin-$SOCKET.log"
# A home, settings and a D-Bus session of KWin's own, and no lock screen.
# Started bare, a nested `kwin_wayland` reaches into the session it is nested
# in, in two ways:
#
# * It writes the real KDE settings: `~/.config/kwinrc` gained a `[Tiling]`
#   section per run, and `kwinoutputconfig.json` a `Virtual-0` output beside
#   the real monitors.
# * It locks its screen at once, which starts xdg-desktop-portal, which starts
#   a second document portal on the real `$XDG_RUNTIME_DIR/doc` mount. The
#   session's own portal dies of that, and Flatpak apps lose file access until
#   `systemctl --user start xdg-document-portal.service`. A private D-Bus did
#   not prevent it; `--no-lockscreen` does.
#
# The private D-Bus is for everything else: on the session's bus it would be a
# second KWin beside the one that already owns KWin's name and talks to the
# rest of the desktop.
#
# Not a runtime directory of its own, though: given one (2026-11-14), KWin
# first gave the hub's window 821px of the 876 it asked for in four runs of six,
# against none in six with the real one, and `games_tab` failed whenever that
# stuck. The tests themselves keep the real environment too: without this
# machine's saved display setup the hub holds a claim, and `games_tab`'s Tab
# walk fails on it.
KWIN_HOME=$(mktemp -d "${TMPDIR:-/tmp}/tobii-kwin.XXXXXX")
# `setsid`, so that the trap stops KWin and everything it started as one group:
# `$!` is `dbus-run-session`, and killing that alone leaves KWin running. And
# `--exit-with-session`, so that KWin also ends with this script when the trap
# never runs (a `kill -9`): it quits when its session program does, and that
# program waits for this script.
setsid env -u WAYLAND_DISPLAY -u DISPLAY \
    HOME="$KWIN_HOME" XDG_CONFIG_HOME="$KWIN_HOME/config" \
    XDG_DATA_HOME="$KWIN_HOME/data" XDG_STATE_HOME="$KWIN_HOME/state" \
    XDG_CACHE_HOME="$KWIN_HOME/cache" \
    dbus-run-session -- kwin_wayland --virtual --no-lockscreen \
        --no-global-shortcuts --width 1600 --height 1200 --socket "$SOCKET" \
        --exit-with-session "tail --pid=$$ -f /dev/null" \
    >"$LOG" 2>&1 &
KWIN=$!
stop_kwin() {
    kill -- -"$KWIN" 2>/dev/null || true
    # Gone before its directory is: KWin writes its settings on the way out,
    # and a file landing mid-`rm` fails the removal, and with it the run.
    for _ in $(seq 1 50); do
        kill -0 -- -"$KWIN" 2>/dev/null || break
        sleep 0.1
    done
    kill -9 -- -"$KWIN" 2>/dev/null && sleep 0.2
    # A KWin that had to be killed never unlinks its socket, and that lives in
    # the session's own runtime directory.
    rm -f "$DISPLAY_SOCKET" "$DISPLAY_SOCKET.lock" || true
    rm -rf "$KWIN_HOME" || true
}
trap stop_kwin EXIT
# The socket appears a moment after the process does, and a test that starts
# before it is there fails for a reason that has nothing to do with the test.
for _ in $(seq 1 50); do
    [ -e "$DISPLAY_SOCKET" ] && break
    sleep 0.1
done
[ -e "$DISPLAY_SOCKET" ] || {
    echo "kwin_wayland did not start; its log is $LOG:" >&2
    tail -n 20 "$LOG" >&2
    exit 1
}

# The geometry the tests print is this machine's font metrics, not a constant.
WAYLAND_DISPLAY="$SOCKET" GDK_BACKEND=wayland \
    cargo test -p tobii-gtk "${TARGETS[@]}" --no-fail-fast --offline --locked \
    -- --ignored --nocapture
