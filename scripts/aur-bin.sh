#!/usr/bin/env bash
# Write the PKGBUILD for tobii-linux-bin: the release tarball's prebuilt
# binaries, repackaged for pacman, so an Arch user needs no Rust toolchain.
#
#   scripts/aur-bin.sh <version> <tarball> <outdir>
#   scripts/aur-bin.sh 0.3.0 dist/tobii-linux-0.3.0-x86_64-unknown-linux-gnu.tar.gz aur/tobii-linux-bin
#
# Writes <outdir>/PKGBUILD and <outdir>/tobii-linux.install and nothing else.
# `.SRCINFO` is left to the caller (`makepkg --printsrcinfo > .SRCINFO`),
# because that needs makepkg, and the Debian release container has none.
#
# # Who runs it
#
# - scripts/package.sh, once the release tarball exists, into aur/ — which
#   release.yml's `arch` job builds, installs and tests in an Arch container,
#   and whose .pkg.tar.zst it publishes with the release.
# - .github/workflows/aur.yml, when a release is published, against the tarball
#   downloaded from it — and pushes the result to the AUR.
#
# # What the pin is
#
# `sha256sums_x86_64` is the digest of <tarball>, so the PKGBUILD refuses any
# download that is not byte-for-byte the file this was generated from: a
# truncated transfer, or an asset replaced after the fact. It is an integrity
# check taken from the same release, NOT a signature. Whoever can publish a
# release can publish a tarball and a PKGBUILD pinned to it; see the NOTE ON
# TRUST in scripts/release.sh, and do not write anything that implies more.
#
# # Where `depends` comes from
#
# The binaries, never a hand-written list. Every NEEDED soname of both goes
# through the table below, and one the table does not know stops the script —
# the same guarantee package.sh gives the rpm, where a hand-written list would
# be a judgement about what GTK happens to pull in. The glibc floor is the
# highest GLIBC_x.y either binary needs, from scripts/elf-deps.sh, which
# package.sh uses for the .deb and the .rpm: three packages, one floor.
set -euo pipefail

usage() {
    echo "usage: scripts/aur-bin.sh <version> <tarball> <outdir>" >&2
    echo "  e.g. scripts/aur-bin.sh 0.3.0 dist/tobii-linux-0.3.0-x86_64-unknown-linux-gnu.tar.gz aur/tobii-linux-bin" >&2
    exit 2
}
die() {
    echo "aur-bin.sh: $*" >&2
    exit 1
}

[[ $# -eq 3 ]] || usage
version="$1"
tarball="$2"
outdir="$3"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The version is written into a PKGBUILD, which is shell that every AUR helper
# sources on the user's machine. Only what release.yml accepts as a tag gets in,
# and that is also the rule that makes the pkgver mapping below sort correctly.
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z][A-Za-z0-9.]*)?$ ]] \
    || die "'$version' is not MAJOR.MINOR.PATCH with an optional -letter… suffix"

# x86_64 only: it is the one architecture the release builds.
dir="tobii-linux-${version}-x86_64-unknown-linux-gnu"
[[ -f "$tarball" ]] || die "no such file: $tarball"
# The PKGBUILD downloads the asset by this name, and makepkg looks for it by
# this name in its start directory. A tarball called anything else is the
# wrong version or the wrong architecture.
[[ "$(basename "$tarball")" == "$dir.tar.gz" ]] \
    || die "$tarball is not $dir.tar.gz"
command -v objdump >/dev/null 2>&1 \
    || die "objdump (binutils) is needed to read the binaries' dependencies"

# shellcheck source=scripts/elf-deps.sh
. "$root/scripts/elf-deps.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
tar -xzf "$tarball" -C "$work"
top="$work/$dir"

# Everything package() below installs, checked here, where a missing file is a
# clear message rather than an `install: cannot stat` on somebody's machine.
for f in tobii tobii-gtk; do
    [[ -f "$top/$f" && ! -L "$top/$f" && -x "$top/$f" ]] \
        || die "$f is not an executable file in $tarball"
done
for f in assets/com.tobiilinux.Configuration.desktop \
         assets/com.tobiilinux.Configuration.svg \
         assets/60-tobii.rules README.md LICENSE; do
    [[ -f "$top/$f" ]] || die "$f is missing from $tarball, and package() installs it"
done

glibc="$(elf_glibc_floor "$top/tobii" "$top/tobii-gtk")"
[[ -n "$glibc" ]] || die "could not read a glibc symbol version from the binaries"

sonames="$(elf_needed "$top/tobii" "$top/tobii-gtk")"
# A dynamically linked program with no NEEDED entries is objdump failing, not a
# program with no dependencies; an empty depends= would install anywhere.
[[ -n "$sonames" ]] || die "objdump listed no NEEDED entries for the binaries"

# soname → the Arch package that ships it (`pacman -Qoq /usr/lib/<soname>`).
#
# Exact sonames, not globs: a soname bump is an ABI change, and the table should
# stop the release until somebody has looked, not wave it through.
# libgcc_s is `libgcc`, not `gcc-libs`: Arch split gcc-libs, which is now a
# meta-package that also drags in libgfortran, libasan and nine more.
deps=()
unmapped=()
add() {
    local d
    for d in "${deps[@]}"; do [[ "$d" == "$1" ]] && return 0; done
    deps+=("$1")
}
while read -r so; do
    case "$so" in
        libgtk-4.so.1)            add gtk4 ;;
        libgtk4-layer-shell.so.0) add gtk4-layer-shell ;;
        libusb-1.0.so.0)          add libusb ;;
        libcairo.so.2)            add cairo ;;
        libglib-2.0.so.0|libgobject-2.0.so.0|libgio-2.0.so.0)
                                  add glib2 ;;
        libgcc_s.so.1)            add libgcc ;;
        # glibc carries the floor: an unversioned glibc would install on a
        # system too old to start the binaries.
        libc.so.6|libm.so.6|ld-linux-x86-64.so.2)
                                  add "glibc>=$glibc" ;;
        *)                        unmapped+=("$so") ;;
    esac
done <<<"$sonames"
if (( ${#unmapped[@]} )); then
    die "no Arch package known for: ${unmapped[*]} — add it to the table in scripts/aur-bin.sh (pacman -Qoq /usr/lib/<soname> names it)"
fi
# Sorted, so the same binaries always produce the same PKGBUILD.
mapfile -t deps < <(printf '%s\n' "${deps[@]}" | LC_ALL=C sort)
depends_line="$(printf "'%s' " "${deps[@]}")"
depends_line="${depends_line% }"

sha="$(sha256sum "$tarball" | cut -d' ' -f1)"
[[ "$sha" =~ ^[0-9a-f]{64}$ ]] || die "could not hash $tarball"

mkdir -p "$outdir"
# A .SRCINFO left from an earlier run would describe the previous version, and
# the AUR would be sent a pair that disagree.
rm -f "$outdir/.SRCINFO"
install -m 644 "$root/scripts/tobii-linux.install" "$outdir/tobii-linux.install"

cat > "$outdir/PKGBUILD" <<EOF
# Maintainer: Fabian Plaimauer
#
# Generated by scripts/aur-bin.sh in https://github.com/Tropaion/Tobii_Linux —
# change the script, not this file: every release regenerates it.
#
# Prebuilt: this repackages the binaries the release ships, built in a Debian 13
# container, so there is nothing to compile here. To build from source instead,
# use the PKGBUILD published with each release (package tobii-linux; the two
# conflict, and either replaces the other).
pkgname=tobii-linux-bin
_ver=${version}
# makepkg forbids '-' in pkgver, so 1.0.0-rc1 becomes 1.0.0rc1 — which vercmp
# sorts below 1.0.0, as a pre-release should.
pkgver=\${_ver//-/}
pkgrel=1
pkgdesc="Linux runtime and GUI for the Tobii Eye Tracker 5 (clean-room, prebuilt)"
arch=('x86_64')
url="https://github.com/Tropaion/Tobii_Linux"
license=('GPL-3.0-only')
# Every NEEDED soname of both binaries, mapped by scripts/aur-bin.sh; glibc
# carries the oldest version the binaries can start on.
depends=(${depends_line})
optdepends=('curl: head-pose model download and update checks')
provides=("tobii-linux=\${pkgver}")
conflicts=('tobii-linux')
# Unstripped on purpose, as the release ships them: a panic backtrace names its
# function. Spelled out rather than left to makepkg.conf, so the release's CI
# build and an AUR helper's build produce the same package: Arch's stock
# makepkg.conf turns strip and debug ON. (autodeps cannot be listed here —
# makepkg 7.1 rejects '!autodeps' in a PKGBUILD as an unknown option. It is off
# in the stock makepkg.conf, and depends above is derived from the binaries.)
options=('!strip' '!debug')
install=tobii-linux.install
_dir="tobii-linux-\${_ver}-x86_64-unknown-linux-gnu"
source_x86_64=("\${_dir}.tar.gz::\${url}/releases/download/v\${_ver}/\${_dir}.tar.gz")
# The digest of the release tarball this file was generated from, which is the
# value the release's SHA256SUMS lists. It catches a corrupted or replaced
# download. It is not a signature, and says nothing about who built the file.
sha256sums_x86_64=('${sha}')

package() {
    cd "\$srcdir/\$_dir"
    install -Dm755 tobii     "\$pkgdir/usr/bin/tobii"
    install -Dm755 tobii-gtk "\$pkgdir/usr/bin/tobii-gtk"
    install -Dm644 assets/com.tobiilinux.Configuration.desktop \\
        "\$pkgdir/usr/share/applications/com.tobiilinux.Configuration.desktop"
    install -Dm644 assets/com.tobiilinux.Configuration.svg \\
        "\$pkgdir/usr/share/icons/hicolor/scalable/apps/com.tobiilinux.Configuration.svg"
    install -Dm644 assets/60-tobii.rules "\$pkgdir/usr/lib/udev/rules.d/60-tobii.rules"
    install -Dm644 README.md "\$pkgdir/usr/share/doc/\$pkgname/README.md"
    install -Dm644 LICENSE   "\$pkgdir/usr/share/licenses/\$pkgname/LICENSE"
}
EOF

echo "aur-bin.sh: wrote $outdir/PKGBUILD (tobii-linux-bin ${version//-/}-1)"
echo "  depends: ${deps[*]}"
echo "  sha256:  $sha"
