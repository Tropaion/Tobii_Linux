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

arch="$(uname -m)"
triple="${2:-${arch}-unknown-linux-gnu}"
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
# The udev rule is the whole practical advantage of packaging this: without it
# the tracker is only reachable as root, and a package can put it in place where
# a tarball cannot.
install -m 644 assets/99-tobii.rules "$payload/usr/lib/udev/rules.d/99-tobii.rules"

installed_kb="$(du -sk "$payload" | cut -f1)"

# --------------------------------------------------------------------- the deb

deb_root="$work/deb"
mkdir -p "$deb_root"
cp -a "$payload/." "$deb_root/"

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
Depends: libc6, libgtk-4-1, libgtk4-layer-shell0, libusb-1.0-0
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
( cd "$deb_root" && find . -type f -printf '%P\0' | sort -z \
    | xargs -0 md5sum > "$ctl/md5sums" ) 2>/dev/null || true

# A .deb is an ar archive of exactly these three members, in this order.
# `debian-binary` must come first or dpkg refuses the file.
printf '2.0\n' > "$work/debian-binary"
tar --owner=root --group=root --numeric-owner -czf "$work/control.tar.gz" -C "$ctl" .
tar --owner=root --group=root --numeric-owner -czf "$work/data.tar.gz" -C "$deb_root" .
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
    install -Dm644 assets/99-tobii.rules "\$pkgdir/usr/lib/udev/rules.d/99-tobii.rules"
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
Requires:       gtk4, gtk4-layer-shell, libusb1
AutoReqProv:    no

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
/usr/lib/udev/rules.d/99-tobii.rules
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
