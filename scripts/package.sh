#!/usr/bin/env bash
# Build distribution packages from an already-built dist/ tree.
#
#   scripts/release.sh 0.1.0      # builds dist/tobii-linux-0.1.0-<triple>.tar.gz
#   scripts/package.sh  0.1.0     # adds the .deb, the PKGBUILD and the .rpm
#
# # Two channels, and they are not the same channel
#
# The **tar.gz** is what `tobii-update` installs: it unpacks into whatever
# directory the running binary lives in, typically ~/.local/bin, and the updater
# owns it end to end.
#
# The **packages** are for people who would rather their system owned it. A
# package installs into /usr/bin, which the updater deliberately refuses to
# touch — `install.rs::package_owner` asks dpkg/rpm/pacman whether they own the
# binary and, if one does, tells the user to update with that instead. Writing
# over a package-managed file would leave the package database describing a file
# that is no longer there, and the next upgrade would silently revert the
# update anyway.
#
# So: to test the updater, install the tar.gz. To install it properly, use a
# package. Both ship in every release.
#
# # Why the .deb is built with `ar` rather than `dpkg-deb`
#
# A .deb *is* an ar archive of three members in a fixed order. Building it by
# hand means this script runs anywhere — including on the maintainer's Arch
# machine, where `dpkg-deb` does not exist — so the packaging can be tested
# before CI ever sees it, rather than being debugged through a CI log.
set -euo pipefail

version="${1:-}"
if [[ -z "$version" ]]; then
    echo "usage: scripts/package.sh <version>   e.g. scripts/package.sh 0.1.0" >&2
    exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

triple="${2:-$(uname -m)-unknown-linux-gnu}"
# The architecture comes from the TRIPLE, not from `uname -m`. They are the
# same thing only when this runs on the machine it is packaging for — and the
# second argument exists precisely so it does not have to. Cross-packaging
# aarch64 on an x86_64 host used to label the .deb `amd64` and the .rpm
# `x86_64`, which apt and dnf both believe.
arch="${triple%%-*}"
name="tobii-linux-${version}-${triple}"
dist="$root/dist"
tarball="$dist/$name.tar.gz"

if [[ ! -f "$tarball" ]]; then
    echo "no $tarball — run scripts/release.sh $version first." >&2
    exit 1
fi

# Unpack the release archive rather than reaching into target/: the packages
# must contain exactly the binaries the tarball ships, not a different build.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
tar -xzf "$tarball" -C "$work"
bindir="$work/$name"
for b in tobii tobii-gtk; do
    [[ -x "$bindir/$b" ]] || { echo "$b missing from $tarball" >&2; exit 1; }
done

# Debian's name for x86_64, and the RPM/Arch names, differ from the Rust triple.
case "$arch" in
    x86_64)  deb_arch=amd64 ;;
    aarch64) deb_arch=arm64 ;;
    *)       deb_arch="$arch" ;;
esac

echo "packaging $name"

# ----------------------------------------------------------------- the payload

# One tree, shared by every package format, so they cannot disagree about what
# is installed or where.
payload="$work/payload"
mkdir -p "$payload/usr/bin" \
         "$payload/usr/share/applications" \
         "$payload/usr/share/icons/hicolor/scalable/apps" \
         "$payload/usr/share/doc/tobii-linux" \
         "$payload/usr/lib/udev/rules.d"

install -m 755 "$bindir/tobii"     "$payload/usr/bin/tobii"
install -m 755 "$bindir/tobii-gtk" "$payload/usr/bin/tobii-gtk"
install -m 644 assets/com.tobiilinux.Configuration.desktop \
               "$payload/usr/share/applications/"
install -m 644 assets/com.tobiilinux.Configuration.svg \
               "$payload/usr/share/icons/hicolor/scalable/apps/"
install -m 644 README.md LICENSE "$payload/usr/share/doc/tobii-linux/" 2>/dev/null || true

# Debian Policy 12.5 makes /usr/share/doc/<pkg>/copyright a *must*, and lintian
# reports its absence as an error — a package with none is one an archive would
# reject. Machine-readable DEP-5, and it points at
# /usr/share/common-licenses/GPL-3 rather than embedding the text: Policy
# requires the reference for a license Debian already ships, and the full text
# is beside it in LICENSE anyway. rpm's %doc glob picks the file up too, which
# costs nothing and keeps the two payloads identical.
cat > "$payload/usr/share/doc/tobii-linux/copyright" <<'COPYRIGHT'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: tobii-linux
Upstream-Contact: https://github.com/Tropaion/Tobii_Linux/issues
Source: https://github.com/Tropaion/Tobii_Linux

Files: *
Copyright: 2026 Fabian Plaimauer
License: GPL-3.0-only

License: GPL-3.0-only
 This program is free software: you can redistribute it and/or modify it under
 the terms of the GNU General Public License version 3 as published by the Free
 Software Foundation.
 .
 This program is distributed in the hope that it will be useful, but WITHOUT ANY
 WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
 PARTICULAR PURPOSE.  See the GNU General Public License for more details.
 .
 On Debian systems the full text of the GNU General Public License version 3 can
 be found in /usr/share/common-licenses/GPL-3; a copy also ships beside this file
 as LICENSE.
COPYRIGHT
chmod 644 "$payload/usr/share/doc/tobii-linux/copyright"
# Without the udev rule the tracker is only reachable as root. A package puts it
# in place as part of the install; the tarball ships the same rule and an
# install.sh that offers to copy it, so neither channel leaves a user stuck.
install -m 644 assets/60-tobii.rules "$payload/usr/lib/udev/rules.d/60-tobii.rules"

installed_kb="$(du -sk "$payload" | cut -f1)"

# The glibc floor, from the binaries themselves, so the packages can DECLARE it.
#
# Without this the package manager has nothing to refuse on: an unversioned
# `libc6` dependency installs happily on a system whose glibc is too old, and
# the user's first experience is apt saying yes and the program saying
# "version `GLIBC_2.41' not found". release.yml enforces the floor at build
# time; this is what carries it to the person installing.
glibc_req=""
# rpm tags a symbol-version requirement with the ELF's bitness, and gets that
# from the file itself rather than from a list of architecture names. So does
# this: a hard-coded "(64bit)" is a second definition of the same fact, and the
# one that is wrong the first time somebody builds for a 32-bit target.
rpm_bits=""
if command -v objdump >/dev/null 2>&1; then
    for bin in tobii tobii-gtk; do
        this="$(objdump -T "$payload/usr/bin/$bin" 2>/dev/null \
            | grep -o 'GLIBC_[0-9.]*' | sort -V | tail -1 || true)"
        [[ -n "$this" ]] || continue
        glibc_req="$(printf '%s\n%s\n' "$glibc_req" "${this#GLIBC_}" | sort -V | tail -1)"
        if [[ -z "$rpm_bits" ]] && objdump -f "$payload/usr/bin/$bin" 2>/dev/null \
            | grep -q 'file format elf64'; then
            rpm_bits="(64bit)"
        fi
    done
fi
if [[ -n "$glibc_req" ]]; then
    echo "  glibc floor: $glibc_req"
else
    echo "  WARNING: could not determine the glibc floor; packages will not declare one" >&2
fi

# --------------------------------------------------------------------- the deb


ctl="$work/control-dir"
mkdir -p "$ctl"
cat > "$ctl/control" <<EOF
Package: tobii-linux
Version: ${version}
Section: utils
Priority: optional
Architecture: ${deb_arch}
Maintainer: Fabian Plaimauer <noreply@github.com>
Installed-Size: ${installed_kb}
Depends: libc6${glibc_req:+ (>= ${glibc_req})}, libgtk-4-1, libgtk4-layer-shell0, libusb-1.0-0
Recommends: curl | wget
Homepage: https://github.com/Tropaion/Tobii_Linux
Description: Linux runtime and GUI for the Tobii Eye Tracker 5
 A clean-room reimplementation of the Tobii Eye Tracker 5's USB protocol, with
 no Tobii software required: gaze streaming, guided display setup,
 follow-the-dot calibration, 6-DOF head tracking for games, and a GTK4
 configuration app.
 .
 This package installs into /usr/bin, so the built-in updater will decline to
 modify it and refer you to your package manager instead. To use the built-in
 updater, install the tar.gz release into your home directory instead.
EOF

# Reload udev so the tracker is usable without a reboot; failing that is not a
# reason to fail the installation, so every command is guarded.
cat > "$ctl/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
    if command -v udevadm >/dev/null 2>&1; then
        udevadm control --reload >/dev/null 2>&1 || true
        udevadm trigger >/dev/null 2>&1 || true
    fi
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q /usr/share/applications >/dev/null 2>&1 || true
    fi
    echo "Re-plug the Eye Tracker 5 so the udev rule takes effect."
fi
EOF
chmod 755 "$ctl/postinst"

# `md5sums` is what `dpkg -V` verifies against. Paths are relative to /.
#
# Not silenced. This used to end in `2>/dev/null || true`, which turns every
# way it can go wrong — no `md5sum`, an unreadable file, xargs splitting the
# list — into an empty or partial manifest that dpkg accepts without complaint,
# so `dpkg -V` would verify nothing and say so by saying nothing.
( cd "$payload" && find . -type f -printf '%P\0' | sort -z \
    | xargs -0 md5sum > "$ctl/md5sums" )
if [[ ! -s "$ctl/md5sums" ]] \
   || [[ "$(wc -l < "$ctl/md5sums")" -ne "$(find "$payload" -type f | wc -l)" ]]; then
    echo "md5sums does not cover every file in the package" >&2
    exit 1
fi

# A .deb is an ar archive of exactly these three members, in this order.
# `debian-binary` must come first or dpkg refuses the file.
printf '2.0\n' > "$work/debian-binary"
tar --owner=root --group=root --numeric-owner -czf "$work/control.tar.gz" -C "$ctl" .
tar --owner=root --group=root --numeric-owner -czf "$work/data.tar.gz" -C "$payload" .
deb="$dist/tobii-linux_${version}_${deb_arch}.deb"
rm -f "$deb"
( cd "$work" && ar rc "$deb" debian-binary control.tar.gz data.tar.gz )
echo "  $(basename "$deb")"

# ---------------------------------------------------------------- the PKGBUILD

# Source-based on purpose: Arch users expect to build from a PKGBUILD, and it
# means the package is compiled against the system's own GTK rather than
# whatever the release container had.
pkgbuild="$dist/PKGBUILD"
cat > "$pkgbuild" <<EOF
# Maintainer: Fabian Plaimauer
pkgname=tobii-linux
pkgver=${version}
pkgrel=1
pkgdesc="Linux runtime and GUI for the Tobii Eye Tracker 5 (clean-room)"
arch=('x86_64' 'aarch64')
url="https://github.com/Tropaion/Tobii_Linux"
license=('GPL-3.0-only')
depends=('gtk4' 'gtk4-layer-shell' 'libusb')
makedepends=('rust' 'pkgconf')
optdepends=('curl: fetching the head-pose model and updates')
source=("\$pkgname-\$pkgver.tar.gz::\$url/archive/refs/tags/v\$pkgver.tar.gz")
# SKIP, deliberately. GitHub generates this tarball on demand and its bytes
# have not been stable across GitHub's own tooling changes, so a pinned digest
# breaks the PKGBUILD for everyone the day that happens — which is worse than an
# absent one, because it looks like a compromised download. The trade is
# recorded in docs/wiki/Quality-and-Risks.md section 11.1a: this channel verifies
# nothing about what it downloads. Use the .tar.gz release if you want a
# checksum; it ships SHA256SUMS.
sha256sums=('SKIP')

build() {
    cd "Tobii_Linux-\$pkgver"
    cargo build --release --locked
}

check() {
    cd "Tobii_Linux-\$pkgver"
    cargo test --workspace --locked
}

package() {
    cd "Tobii_Linux-\$pkgver"
    install -Dm755 target/release/tobii     "\$pkgdir/usr/bin/tobii"
    install -Dm755 target/release/tobii-gtk "\$pkgdir/usr/bin/tobii-gtk"
    install -Dm644 assets/com.tobiilinux.Configuration.desktop \\
        "\$pkgdir/usr/share/applications/com.tobiilinux.Configuration.desktop"
    install -Dm644 assets/com.tobiilinux.Configuration.svg \\
        "\$pkgdir/usr/share/icons/hicolor/scalable/apps/com.tobiilinux.Configuration.svg"
    install -Dm644 assets/60-tobii.rules "\$pkgdir/usr/lib/udev/rules.d/60-tobii.rules"
    install -Dm644 README.md "\$pkgdir/usr/share/doc/\$pkgname/README.md"
    install -Dm644 LICENSE   "\$pkgdir/usr/share/licenses/\$pkgname/LICENSE"
}
EOF
echo "  PKGBUILD"

# --------------------------------------------------------------------- the rpm

# Only where rpmbuild exists. Skipped rather than faked: an untested package is
# worse than an absent one, because it looks like a supported channel.
if command -v rpmbuild >/dev/null 2>&1; then
    rpmtop="$work/rpm"
    mkdir -p "$rpmtop"/{BUILD,RPMS,SOURCES,SPECS,BUILDROOT}
    cat > "$rpmtop/SPECS/tobii-linux.spec" <<EOF
Name:           tobii-linux
Version:        ${version}
Release:        1
Summary:        Linux runtime and GUI for the Tobii Eye Tracker 5
License:        GPL-3.0-only
URL:            https://github.com/Tropaion/Tobii_Linux
BuildArch:      ${arch}
Requires:       gtk4, gtk4-layer-shell, libusb1${glibc_req:+, libc.so.6(GLIBC_${glibc_req})${rpm_bits}}
# Automatic dependency generation left ON deliberately (it was `AutoReqProv:
# no`). Turning it off also turns off the `libc.so.6(GLIBC_x.y)` requirement rpm
# derives from the ELF — exactly the check that stops this installing on a
# system too old to run it. The explicit Requires above are a floor, not a
# replacement for it: this script has no rpmbuild on most developer machines, so
# release.yml re-reads both finished packages and fails if the floor is missing.

%description
A clean-room reimplementation of the Tobii Eye Tracker 5's USB protocol, with no
Tobii software required. Installs into /usr/bin, so the built-in updater defers
to your package manager.

%install
cp -a %{_sourcedir}/payload/. %{buildroot}/

%files
/usr/bin/tobii
/usr/bin/tobii-gtk
/usr/share/applications/com.tobiilinux.Configuration.desktop
/usr/share/icons/hicolor/scalable/apps/com.tobiilinux.Configuration.svg
/usr/lib/udev/rules.d/60-tobii.rules
%doc /usr/share/doc/tobii-linux/*

%post
udevadm control --reload >/dev/null 2>&1 || :
udevadm trigger >/dev/null 2>&1 || :
EOF
    cp -a "$payload" "$rpmtop/SOURCES/payload"
    rpmbuild --define "_topdir $rpmtop" -bb "$rpmtop/SPECS/tobii-linux.spec" >/dev/null
    find "$rpmtop/RPMS" -name '*.rpm' -exec cp {} "$dist/" \;
    echo "  $(cd "$dist" && ls *.rpm 2>/dev/null | tail -1)"
else
    echo "  (no rpmbuild — skipping the .rpm)"
fi

# ------------------------------------------------------------------- checksums

# Rewritten to cover everything now in dist/, so the updater's archive and every
# package are all listed by the one file the release publishes.
( cd "$dist" && rm -f SHA256SUMS && sha256sum ./* 2>/dev/null \
    | sed 's|\./||' | grep -v 'SHA256SUMS' > SHA256SUMS )

echo
echo "dist/:"
ls -1 "$dist"
