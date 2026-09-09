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
# Then attach dist/* to the GitHub release for tag v0.2.0. The tag has to be a
# version — `Version::parse` ignores anything else, so a `nightly` tag is
# invisible to the updater rather than offered as an upgrade.
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
    need="$(objdump -T "$dist/$name/tobii" 2>/dev/null \
        | grep -o 'GLIBC_[0-9.]*' | sort -V | tail -1 || true)"
    if [[ -n "$need" ]]; then
        echo "  needs $need or newer — anyone on an older glibc cannot run this build"
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
echo "  gh release create v$version dist/* --title v$version --notes-file <changelog>"
