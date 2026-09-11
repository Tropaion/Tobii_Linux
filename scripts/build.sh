#!/usr/bin/env bash
# Build TobiiLinux in release mode.
#
#   scripts/build.sh                 # check dependencies, build everything
#   scripts/build.sh --lean          # the CLI only, without the neural backend
#   scripts/build.sh --install       # build, install, add to the app menu
#   scripts/build.sh --install --system   # for every user, /usr/local/bin (uses sudo)
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
caller_pwd="$PWD"
cd "$root"

# Not as root. cargo run under sudo leaves a root-owned target/ that the next
# plain build cannot write, and an install under sudo lands in /root. This asks
# for sudo itself, for exactly the steps that need it.
if [[ ${TOBII_TEST_EUID:-$(id -u)} -eq 0 ]]; then
    echo "Run scripts/build.sh as yourself; it asks for sudo where it needs it." >&2
    echo "For an install every user gets:  scripts/build.sh --install --system" >&2
    exit 1
fi

lean=0
do_install=0
install_dir=""
system=0
do_udev=0
check_only=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --lean)    lean=1 ;;
        --check)   check_only=1 ;;
        --udev)    do_udev=1 ;;
        --system)  system=1 ;;
        --install)
            do_install=1
            # An optional directory may follow, but not another flag. Relative
            # to where build.sh was run, not the repository it cd's into.
            if [[ ${2:-} && ${2:0:1} != "-" ]]; then
                install_dir="$2"
                if [[ "$install_dir" != /* ]]; then install_dir="$caller_pwd/$install_dir"; fi
                shift
            fi
            ;;
        -h|--help)
            # The header comment, however long it is: a stored line range goes
            # stale the first time a line is added above it.
            awk 'NR==1{next} !/^#/{exit} {sub(/^# ?/,""); print}' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        *)
            echo "unknown option: $1  (try --help)" >&2
            exit 2
            ;;
    esac
    shift
done

# --system says who an install is for, so on its own it has nothing to act on.
# Refused here, before the build, rather than ignored after it with nothing to
# say that nothing was installed.
if [[ $system -eq 1 && $do_install -eq 0 && $do_udev -eq 0 ]]; then
    echo "--system is for an install:  scripts/build.sh --install --system [DIR]" >&2
    exit 2
fi

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
# the build works.
need_module() {
    # The module to probe and, optionally, what to call it in the output.
    local probe="$1" label="${2:-$1}"
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
    need_module libusb-1.0
    if [[ $lean -eq 0 ]]; then
        need_module gtk4
        need_module gtk4-layer-shell-0 gtk4-layer-shell
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

# ------------------------------------------------------- install (and udev)
#
# Delegated, so a source install and a release-tarball install are literally the
# same steps — install-payload.sh also ships inside the archive.

if [[ $do_install -eq 1 || $do_udev -eq 1 ]]; then
    echo
    if [[ -z $install_dir ]]; then
        if [[ $system -eq 1 ]]; then install_dir=/usr/local/bin; else install_dir="$HOME/.local/bin"; fi
    fi
    args=("$install_dir" "target/release" "assets")
    if [[ $do_udev -eq 1 ]]; then args+=(--udev); else args+=(--no-udev); fi
    if [[ $lean -eq 1 ]]; then args+=(--lean); fi
    # --udev without --install means the rule and nothing else.
    if [[ $do_install -eq 0 ]]; then args+=(--no-bins); fi
    if [[ $system -eq 1 ]]; then
        echo "Installing for every user into $install_dir — this uses sudo."
        sudo bash scripts/install-payload.sh "${args[@]}" --system
    else
        bash scripts/install-payload.sh "${args[@]}"
    fi
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
# Not when --install ran: install-payload.sh already said this, and saying it
# twice in one run reads as two different problems.
if [[ $do_install -eq 0 && $do_udev -eq 0 && ! -e /etc/udev/rules.d/60-tobii.rules ]]; then
    echo
    echo "  ${bold}The udev rule is not installed${reset}, so the tracker needs root."
    echo "  ${dim}scripts/build.sh --udev${reset}   or   ${dim}sudo cp assets/60-tobii.rules /etc/udev/rules.d/${reset}"
fi
