#!/usr/bin/env bash
# Build a release archive in the layout `tobii update` expects.
#
# Two assets are required: the archive, named with the target triple so a build
# for the wrong machine is never offered, and SHA256SUMS beside it. The updater
# refuses a release without checksums, because it could not then tell a
# truncated download from a complete one.
#
#   scripts/release.sh 0.2.0
#
# This is for checking what a release WOULD contain. It is not how one is
# published: `git tag v0.2.0 && git push origin v0.2.0` fires release.yml, which
# runs this same script inside a Debian 13 container and drafts the release from
# that build. Publishing the dist/ this produces would ship the developer
# machine's glibc floor, which is the failure the container exists to prevent.
#
# The tag has to be a version — `Version::parse` ignores anything else, so a
# `nightly` tag is invisible to the updater rather than offered as an upgrade.
#
# NOTE ON TRUST: the checksums are published in the same release as the archive
# and fetched over the same connection, so they are an integrity check and not a
# signature. Anyone who can publish to this repository's releases can publish
# binaries that every check in `tobii-update` accepts. That is the same trust
# a user extends by downloading a binary from the releases page by hand, and it
# is stated plainly in the UI — but do not add wording anywhere that implies
# more.
#
# NOTE ON GLIBC: a dynamically linked build only runs on a glibc at least as new
# as the one it was built against. Building on a current rolling distribution
# produces binaries that will not start on Debian stable, and the updater has no
# way to know that before downloading — it runs the new binary once before
# installing it, so the failure is refused rather than fatal, but the release is
# still useless to those users. Build in an old-glibc container, or use musl:
#
#   rustup target add x86_64-unknown-linux-musl
#   scripts/release.sh 0.2.0 x86_64-unknown-linux-musl
set -euo pipefail

version="${1:-}"
if [[ -z "$version" ]]; then
    echo "usage: scripts/release.sh <version>   e.g. scripts/release.sh 0.2.0" >&2
    exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# The version in Cargo.toml is what the running binary reports and what the
# updater compares against the tag. If they disagree, an update installs itself
# and then offers itself again forever.
manifest_version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
if [[ "$manifest_version" != "$version" ]]; then
    echo "Cargo.toml says $manifest_version but you asked for $version." >&2
    echo "Set [workspace.package] version first, so the build reports the tag it ships as." >&2
    exit 1
fi

arch="$(uname -m)"
triple="${2:-${arch}-unknown-linux-gnu}"
name="tobii-linux-${version}-${triple}"

echo "building $name"

# Keep the build machine's absolute paths out of the published binaries.
# Panic locations carry the source path of whatever crate raised them, so a
# plain release build embeds ~500 paths under the builder's
# $CARGO_HOME/registry. Measured on this workspace: 495 in `tobii`, 532 in
# `tobii-gtk`. It is a privacy leak for anyone who builds and shares a binary,
# it is dead weight in something people download, and the remapped form is
# *better* in a bug report — a panic reads `crates/tobii-usb/src/lib.rs:123`
# rather than a path unique to one machine.
#
# Not `[profile.release] trim-paths`, which does exactly this and is the right
# answer the moment it is available: it is still unstable in the pinned Cargo
# 1.98 and refuses to build. Checked, not assumed.
#
# This changes RUSTFLAGS, so it invalidates the build cache — which is correct
# for a release: a release must not be assembled from objects compiled with
# different flags.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}/registry=/cargo/registry --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}/git=/cargo/git --remap-path-prefix=$root=."

if [[ "$triple" == "${arch}-unknown-linux-gnu" ]]; then
    cargo build --release --locked
    bindir="target/release"
else
    cargo build --release --locked --target "$triple"
    bindir="target/$triple/release"
fi

dist="$root/dist"
rm -rf "$dist"
mkdir -p "$dist/$name"
for bin in tobii tobii-gtk; do
    cp "$bindir/$bin" "$dist/$name/"
done

# The binaries have to run somewhere other than this machine. Report the oldest
# glibc they need, so a release that only works on the maintainer's laptop is
# noticed here rather than by the first person who installs it.
if command -v objdump >/dev/null 2>&1; then
    # BOTH binaries, and the highest of the two. tobii-gtk links the whole GTK4
    # stack, so its floor is far above the CLI's: measured on this repo, tobii
    # needs GLIBC_2.39 and tobii-gtk needs 2.44. Reporting only the CLI
    # understated the archive by five minor versions, which is worse than not
    # reporting at all — it is a number a maintainer would trust.
    need=""
    for bin in tobii tobii-gtk; do
        [[ -f "$dist/$name/$bin" ]] || continue
        this="$(objdump -T "$dist/$name/$bin" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' \
            | sort -V | tail -1 || true)"
        [[ -n "$this" ]] || continue
        echo "  $bin needs $this"
        # `sort -V` puts the newer version last, so the max of the two is the
        # tail. Version sort, not string sort: 2.10 is newer than 2.9.
        need="$(printf '%s\n%s\n' "$need" "$this" | sort -V | tail -1)"
    done
    if [[ -n "$need" ]]; then
        echo "  → this archive needs $need or newer; nobody on an older glibc can run it"
    fi
fi

# The updater runs `--version` on a downloaded binary before installing it. If
# that does not work here, it will not work there either.
for bin in tobii tobii-gtk; do
    if ! "$dist/$name/$bin" --version >/dev/null 2>&1; then
        echo "$bin does not answer --version; the updater's pre-install check would reject it." >&2
        exit 1
    fi
done
cp README.md LICENSE "$dist/$name/" 2>/dev/null || true

# Everything needed to actually install this, not just to run it. Without the
# udev rule in the archive a tarball user has no way to get one short of
# cloning the repository, and the tracker is root-only until they do; without
# the desktop entry there is no menu item. The archive's `install.sh` is a
# two-line wrapper around the same script `build.sh --install` runs.
mkdir -p "$dist/$name/assets"
cp assets/60-tobii.rules \
   assets/com.tobiilinux.Configuration.desktop \
   assets/com.tobiilinux.Configuration.svg "$dist/$name/assets/"
cp scripts/install-payload.sh "$dist/$name/assets/"
cat > "$dist/$name/install.sh" <<'INSTALLER'
#!/usr/bin/env bash
# Install this release. Everything lands in your home directory except the udev
# rule, which needs sudo and which the script asks about before using it.
#
#   ./install.sh                    # into ~/.local/bin
#   ./install.sh /usr/local/bin     # somewhere else (may need sudo)
#   ./install.sh --no-udev          # skip the one step that needs sudo
#   ./install.sh --lean             # the CLI only, no menu entry
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# A leading non-flag argument is the install directory; everything else is
# passed straight through. This used to forward only "$1", so every flag the
# payload script documents was silently dropped — including --no-udev, the one
# a user reaches for precisely because they do not want it running sudo.
bindir="$HOME/.local/bin"
if [ $# -gt 0 ] && [ "${1#--}" = "$1" ]; then
    bindir="$1"
    shift
fi
exec bash "$here/assets/install-payload.sh" "$bindir" "$here" "$here/assets" "$@"
INSTALLER
chmod 755 "$dist/$name/install.sh" "$dist/$name/assets/install-payload.sh"

tar -czf "$dist/$name.tar.gz" -C "$dist" "$name"
rm -rf "${dist:?}/$name"

# Names only, no paths: the updater matches by file name, and so does
# `sha256sum -c` when run from this directory.
(cd "$dist" && sha256sum "$name.tar.gz" > SHA256SUMS)

echo
echo "dist/:"
ls -1 "$dist"
echo
echo "next:"
echo "  git tag v$version && git push origin v$version"
echo
echo "  That is the whole procedure. release.yml rebuilds all of this in a"
echo "  Debian 13 container and drafts the release from THAT build."
echo
echo "  Do NOT publish the dist/ you just built. This machine's glibc is the"
echo "  floor of anything it produces, and it is newer than the floor CI"
echo "  enforces — a hand-published archive would fail to start for very"
echo "  nearly everyone who downloaded it."
