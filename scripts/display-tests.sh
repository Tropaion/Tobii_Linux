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
#     scripts/display-tests.sh games_tab  # one
#
# Nothing here is run by CI. `.github/workflows/ci.yml` runs `cargo test
# --workspace --locked`, which compiles these and skips every one of them, so
# what they assert is verified by a person running this and by nothing else.
# `docs/wiki/Quality-and-Risks.md` records that as a known gap; this script
# exists so that closing it by hand is one command instead of a paragraph.
set -euo pipefail

cd "$(dirname "$0")/.."

TESTS=(games_tab games_tab_refreshes hub_lifetime help_window quit_action
       flows_release_the_tracker keep_awake_switch)
if [ $# -gt 0 ]; then
    TESTS=("$@")
fi

command -v kwin_wayland >/dev/null || {
    echo "kwin_wayland is not installed; it is what provides wlr-layer-shell here" >&2
    exit 1
}

SOCKET="wl-tobii-$$"
kwin_wayland --virtual --width 1600 --height 1200 --socket "$SOCKET" \
    >/tmp/kwin-"$SOCKET".log 2>&1 &
KWIN=$!
# The socket appears a moment after the process does, and a test that starts
# before it is there fails for a reason that has nothing to do with the test.
for _ in $(seq 1 50); do
    [ -e "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/$SOCKET" ] && break
    sleep 0.1
done
trap 'kill "$KWIN" 2>/dev/null || true' EXIT

# Text scale pinned inside the tests themselves; the geometry they print is at
# 1.0 and is this machine's font metrics, not a constant.
fail=0
for t in "${TESTS[@]}"; do
    echo "--- $t ---"
    WAYLAND_DISPLAY="$SOCKET" GDK_BACKEND=wayland \
        cargo test -p tobii-gtk --test "$t" --offline --locked -- --ignored --nocapture \
        || fail=1
done
exit "$fail"
