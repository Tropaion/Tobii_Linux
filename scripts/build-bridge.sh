#!/usr/bin/env bash
# Build the Wine-side bridge (provider exe + client DLLs).
#
# Kept out of `cargo build` on purpose: `bridge/` is a separate workspace
# targeting x86_64-pc-windows-gnu, so the repo's `cargo build` and
# `cargo clippy --workspace` are unaffected and anyone without the mingw
# toolchain can still build everything else.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target=x86_64-pc-windows-gnu

missing=0
if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
    echo "error: x86_64-w64-mingw32-gcc not found." >&2
    echo "       Arch/CachyOS:   sudo pacman -S --needed mingw-w64-gcc" >&2
    echo "       Debian/Ubuntu:  sudo apt install gcc-mingw-w64-x86-64" >&2
    missing=1
fi
if ! rustup target list --installed 2>/dev/null | grep -qx "$target"; then
    echo "error: the $target Rust target is not installed." >&2
    echo "       rustup target add $target" >&2
    missing=1
fi
[ "$missing" -eq 0 ] || exit 1

out="$root/bridge/target/$target/release"

# Spike S2: a measurement build of NPClient64.dll that logs every call and
# serves no data. Built first so the real DLL overwrites it — running the spike
# is an explicit act (`--spike`), never something you get by accident.
if [ "${1:-}" = "--spike" ]; then
    echo "building the S2 measurement stub..."
    cargo build --release --target "$target" \
        --manifest-path "$root/bridge/Cargo.toml" -p NPClient64 --features spike-log
    echo
    echo "built the LOGGING STUB: $out/NPClient64.dll"
    echo "it serves no tracking data — it records what the game asks for, to"
    echo "C:\\tobii-bridge\\npclient.log inside the prefix."
    exit 0
fi

echo "building the Wine bridge for $target..."
cargo build --release --target "$target" --manifest-path "$root/bridge/Cargo.toml"
echo
echo "built:"
for f in tobii-bridge.exe freetrackclient64.dll NPClient64.dll; do
    if [ -f "$out/$f" ]; then
        printf '  %-24s %s bytes\n' "$f" "$(stat -c '%s' "$out/$f")"
    fi
done
echo
echo "install into a Wine prefix with:  tobii bridge install --prefix PATH"
