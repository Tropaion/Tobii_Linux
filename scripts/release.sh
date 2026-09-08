#!/usr/bin/env bash
# Build a release archive in the layout `tobii update` expects.
#
# The updater will only install what it can verify, so the two assets below are
# both required: the archive, named with the target triple so a build for the
# wrong machine is never offered, and SHA256SUMS beside it. A release without
# the checksums is refused rather than trusted.
#
#   scripts/release.sh 0.2.0
#
# Then attach dist/* to the GitHub release for tag v0.2.0. The tag has to be a
# version — `Version::parse` ignores anything else, so a `nightly` tag is
# invisible to the updater rather than offered as an upgrade.
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
triple="${arch}-unknown-linux-gnu"
name="tobii-linux-${version}-${triple}"

echo "building $name"
cargo build --release --locked

dist="$root/dist"
rm -rf "$dist"
mkdir -p "$dist/$name"
for bin in tobii tobii-gtk; do
    cp "target/release/$bin" "$dist/$name/"
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
