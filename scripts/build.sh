#!/usr/bin/env bash
# Build TobiiLinux in release mode.
#
#   scripts/build.sh                 # check dependencies, build everything
#   scripts/build.sh --lean          # the CLI only, without the neural backend
#   scripts/build.sh --install       # build, install, add to the app menu
#   scripts/build.sh --install /usr/local/bin
#   scripts/build.sh --udev          # also install the udev rule (needs sudo)
#   scripts/build.sh --check         # only check dependencies, build nothing
#
# This is the script for building the program to use it. To build a release for
# other people to download, use scripts/release.sh, which produces the archive
# and SHA256SUMS the updater expects.
#
# The dependency check exists because a missing GTK development package does not
# fail with "GTK is missing" — it fails with several hundred lines of linker
# errors about undefined symbols, several minutes into the build. Finding out in
# the first second, by name, is the whole point.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

lean=0
do_install=0
install_dir="$HOME/.local/bin"
do_udev=0
check_only=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --lean)    lean=1 ;;
        --check)   check_only=1 ;;
        --udev)    do_udev=1 ;;
        --install)
            do_install=1
            # An optional directory may follow, but not another flag.
            if [[ ${2:-} && ${2:0:1} != "-" ]]; then
                install_dir="$2"
                shift
            fi
            ;;
        -h|--help)
            sed -n '2,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown option: $1  (try --help)" >&2
            exit 2
            ;;
    esac
    shift
done

bold=""; dim=""; red=""; green=""; reset=""
if [[ -t 1 ]]; then
    bold=$'\e[1m'; dim=$'\e[2m'; red=$'\e[31m'; green=$'\e[32m'; reset=$'\e[0m'
fi

# ---------------------------------------------------------------- dependencies

# How to install what is missing, per distribution family. Read from os-release
# rather than guessed, and with a generic fallback so an unlisted distro gets the
# package names rather than silence.
hint_for() {
    local id="" like=""
    if [[ -r /etc/os-release ]]; then
        # shellcheck disable=SC1091
        . /etc/os-release
        id="${ID:-}"; like="${ID_LIKE:-}"
    fi
    case "$id $like" in
        *arch*|*cachyos*|*manjaro*)
            echo "sudo pacman -S --needed gtk4 gtk4-layer-shell libusb pkgconf base-devel" ;;
        *debian*|*ubuntu*)
            echo "sudo apt install libgtk-4-dev libgtk4-layer-shell-dev libusb-1.0-0-dev pkg-config build-essential" ;;
        *fedora*|*rhel*)
            echo "sudo dnf install gtk4-devel gtk4-layer-shell-devel libusb1-devel pkgconf-pkg-config gcc" ;;
        *suse*)
            echo "sudo zypper install gtk4-devel gtk4-layer-shell-devel libusb-1_0-devel pkg-config gcc" ;;
        *)
            echo "install the development packages for: gtk4, gtk4-layer-shell, libusb-1.0, pkg-config" ;;
    esac
}

missing=()

need_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        missing+=("$1 ($2)")
        printf '  %s%-24s%s %s\n' "$red" "$1" "$reset" "not found"
    else
        printf '  %s%-24s%s %s\n' "$green" "$1" "$reset" "${dim}$(command -v "$1")${reset}"
    fi
}

# The pkg-config module name is not always the name people call the library.
# gtk4-layer-shell installs `gtk4-layer-shell-0.pc`, so probing for
# "gtk4-layer-shell" reports it missing on a machine where it is installed and
# the build works. Each entry is "what to probe|what to call it".
need_module() {
    local probe="${1%%|*}" label="${1##*|}"
    if pkg-config --exists "$probe" 2>/dev/null; then
        printf '  %s%-24s%s %s\n' "$green" "$label" "$reset" "${dim}$(pkg-config --modversion "$probe")${reset}"
    else
        missing+=("$label")
        printf '  %s%-24s%s %s\n' "$red" "$label" "$reset" "not found"
    fi
}

echo "${bold}Dependencies${reset}"
need_command cargo "Rust toolchain — https://rustup.rs"
need_command cc "a C compiler, for the -sys crates"
need_command pkg-config "used to locate the system libraries"

if command -v pkg-config >/dev/null 2>&1; then
    need_module "libusb-1.0|libusb-1.0"
    if [[ $lean -eq 0 ]]; then
        need_module "gtk4|gtk4"
        need_module "gtk4-layer-shell-0|gtk4-layer-shell"
    fi
fi

if [[ ${#missing[@]} -gt 0 ]]; then
    echo
    echo "${red}${bold}Missing:${reset} ${missing[*]}"
    echo
    echo "  $(hint_for)"
    if [[ $lean -eq 1 ]]; then
        echo
        echo "  ${dim}(--lean still needs libusb; only the GTK packages are skipped)${reset}"
    else
        echo
        echo "  ${dim}Only building the CLI? scripts/build.sh --lean needs neither GTK package.${reset}"
    fi
    exit 1
fi

if [[ $check_only -eq 1 ]]; then
    echo
    echo "${green}Everything needed is installed.${reset}"
    exit 0
fi

# --------------------------------------------------------------------- build

echo
if [[ $lean -eq 1 ]]; then
    echo "${bold}Building the CLI (no neural head-pose backend)${reset}"
    # --no-default-features drops `tract`, which is ~110 crates and most of the
    # build time. Head tracking still works; it reports 5 DOF instead of 6,
    # because pitch is the axis two eye origins cannot express.
    cargo build --release -p tobii-cli --no-default-features
    built=(tobii)
else
    echo "${bold}Building everything in release mode${reset}"
    echo "${dim}First build pulls in tract (~110 crates) for the head-pose model; expect a few minutes.${reset}"
    cargo build --release
    built=(tobii tobii-gtk)
fi

echo
echo "${bold}Built${reset}"
for b in "${built[@]}"; do
    p="target/release/$b"
    if [[ -x "$p" ]]; then
        size="$(du -h "$p" | cut -f1)"
        ver="$("$p" --version 2>/dev/null || echo "?")"
        printf '  %s%-12s%s %-6s %s%s%s\n' "$green" "$b" "$reset" "$size" "$dim" "$ver" "$reset"
    else
        echo "  ${red}$b${reset} was not produced — this should not happen"
        exit 1
    fi
done

# ------------------------------------------------------------------- install

if [[ $do_install -eq 1 ]]; then
    echo
    echo "${bold}Installing into $install_dir${reset}"
    mkdir -p "$install_dir"
    for b in "${built[@]}"; do
        # Not `cp` onto a running binary: that writes through the inode and can
        # kill a process executing from it. Write beside it and rename, which
        # replaces the directory entry and leaves the running process on the old
        # inode — the same thing the in-app updater does.
        tmp="$install_dir/.$b.new-$$"
        cp "target/release/$b" "$tmp"
        chmod 755 "$tmp"
        mv -f "$tmp" "$install_dir/$b"
        echo "  $install_dir/$b"
    done

    case ":${PATH}:" in
        *":$install_dir:"*) ;;
        *)
            echo
            echo "  ${bold}Note:${reset} $install_dir is not in your PATH."
            echo "  ${dim}fish:  fish_add_path $install_dir${reset}"
            echo "  ${dim}bash:  echo 'export PATH=\"$install_dir:\$PATH\"' >> ~/.bashrc${reset}"
            ;;
    esac

    # The application menu entry. Installed per-user under XDG_DATA_HOME so this
    # needs no root; only the GUI has anything to show, so it is skipped for a
    # --lean build.
    if [[ $lean -eq 0 ]]; then
        data="${XDG_DATA_HOME:-$HOME/.local/share}"
        apps="$data/applications"
        icons="$data/icons/hicolor/scalable/apps"
        mkdir -p "$apps" "$icons"
        cp assets/com.tobiilinux.Configuration.desktop "$apps/"
        cp assets/com.tobiilinux.Configuration.svg "$icons/"

        # Without this the entry can take minutes to appear, or not appear until
        # the next login — which reads as "the install did not work".
        if command -v update-desktop-database >/dev/null 2>&1; then
            update-desktop-database "$apps" 2>/dev/null || true
        fi
        if command -v gtk4-update-icon-cache >/dev/null 2>&1; then
            gtk4-update-icon-cache -qtf "$data/icons/hicolor" 2>/dev/null || true
        elif command -v gtk-update-icon-cache >/dev/null 2>&1; then
            gtk-update-icon-cache -qtf "$data/icons/hicolor" 2>/dev/null || true
        fi
        echo "  $apps/com.tobiilinux.Configuration.desktop"

        # The entry runs a bare `tobii-gtk`, so it only works if the launcher
        # can find it. A desktop launcher does not read the user's shell
        # profile, so ~/.local/bin being on an interactive PATH proves nothing.
        if [[ "$install_dir" != "/usr/bin" && "$install_dir" != "/usr/local/bin" ]]; then
            echo
            echo "  ${dim}The menu entry runs \`tobii-gtk\` from PATH. Desktop launchers do not"
            echo "  read your shell profile, but most honour ~/.config/environment.d and"
            echo "  systemd's user environment; if the entry does nothing, either install"
            echo "  to /usr/local/bin or run:${reset}"
            echo "  ${dim}systemctl --user import-environment PATH${reset}"
        fi
    fi
fi

# ---------------------------------------------------------------------- udev

if [[ $do_udev -eq 1 ]]; then
    echo
    echo "${bold}Installing the udev rule${reset}"
    echo "${dim}Needed once, so the tracker is usable without root.${reset}"
    sudo cp assets/99-tobii.rules /etc/udev/rules.d/
    sudo udevadm control --reload
    sudo udevadm trigger
    echo "  installed — re-plug the Eye Tracker 5 for it to take effect"
fi

# ----------------------------------------------------------------------- next

echo
echo "${bold}Next${reset}"
if [[ $do_install -eq 1 ]]; then
    run_gui="tobii-gtk"; run_cli="tobii"
else
    run_gui="./target/release/tobii-gtk"; run_cli="./target/release/tobii"
fi
# Padded to a common width so the comments line up whether or not --install
# was used, since the two cases have very different command lengths.
say() { printf '  %-*s %s%s%s\n' "$width" "$1" "$dim" "$2" "$reset"; }
width=0
for c in "$run_gui" "$run_cli stream" "$run_cli headpose --fetch-model"; do
    [[ ${#c} -gt $width ]] && width=${#c}
done
if [[ $lean -eq 0 ]]; then
    say "$run_gui" "# the hub"
fi
say "$run_cli stream" "# decoded gaze samples"
say "$run_cli headpose --fetch-model" "# optional: adds pitch (6 DOF)"
if [[ $do_udev -eq 0 && ! -e /etc/udev/rules.d/99-tobii.rules ]]; then
    echo
    echo "  ${bold}The udev rule is not installed${reset}, so the tracker needs root."
    echo "  ${dim}scripts/build.sh --udev${reset}   or   ${dim}sudo cp assets/99-tobii.rules /etc/udev/rules.d/${reset}"
fi
