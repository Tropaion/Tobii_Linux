#!/usr/bin/env bash
# Install an unpacked tobii-linux into a home directory.
#
# The one definition of what "installed" means for a non-package install:
# binaries, the application-menu entry, the icon, and the udev rule. Both
# `scripts/build.sh --install` (source tree) and the `install.sh` inside a
# release tarball run this, so the two cannot drift — the release archive used
# to have no installer at all, and the notes told people to `tar -xzf` into
# ~/.local/bin, which unpacks a versioned directory rather than the binaries.
#
#   install-payload.sh <bindir> <bin-src-dir> <asset-dir> \
#       [--udev|--no-udev] [--lean] [--no-bins]
#
# It is deliberately quiet about what it cannot do: it never uses sudo without
# saying so first, and skips the udev rule rather than prompting when told to.
set -euo pipefail

bindir="$1"; binsrc="$2"; assets="$3"; shift 3
udev="ask"
lean=0
bins_wanted=1
for a in "$@"; do
    case "$a" in
        --udev)    udev="yes" ;;
        --no-udev) udev="no" ;;
        --lean)    lean=1 ;;
        --no-bins) bins_wanted=0 ;;   # `build.sh --udev` alone: the rule only
    esac
done

if [[ -t 1 ]]; then bold=$'\e[1m'; dim=$'\e[2m'; reset=$'\e[0m'
else bold=""; dim=""; reset=""; fi

if [[ $bins_wanted -eq 1 ]]; then
    echo "${bold}Installing into $bindir${reset}"
    mkdir -p "$bindir"

    bins=(tobii)
    # `if`, not `[[ ]] &&`: under `set -e` a false test as the whole command
    # aborts the script, so a --lean install would exit here silently.
    if [[ $lean -eq 0 ]]; then bins+=(tobii-gtk); fi

    for b in "${bins[@]}"; do
        [[ -f "$binsrc/$b" ]] || continue
        # Not `cp` onto a running binary: that writes through the inode and can
        # kill a process executing from it. Write beside it and rename, which
        # replaces the directory entry and leaves the running process on the old
        # inode — the same thing the in-app updater does.
        tmp="$bindir/.$b.new-$$"
        cp "$binsrc/$b" "$tmp"
        chmod 755 "$tmp"
        mv -f "$tmp" "$bindir/$b"
        echo "  $bindir/$b"
    done

    case ":${PATH}:" in
        *":$bindir:"*) ;;
        *)
            echo
            echo "  ${bold}Note:${reset} $bindir is not in your PATH."
            echo "  ${dim}fish:  fish_add_path $bindir${reset}"
            echo "  ${dim}bash:  echo 'export PATH=\"$bindir:\$PATH\"' >> ~/.bashrc${reset}"
            ;;
    esac
fi

# The application menu entry, per-user under XDG_DATA_HOME so this needs no
# root. Only the GUI has anything to show, so a --lean install skips it.
if [[ $bins_wanted -eq 1 && $lean -eq 0 && -f "$assets/com.tobiilinux.Configuration.desktop" ]]; then
    data="${XDG_DATA_HOME:-$HOME/.local/share}"
    apps="$data/applications"
    icons="$data/icons/hicolor/scalable/apps"
    mkdir -p "$apps" "$icons"

    # Exec is rewritten to the absolute path. The shipped entry says a bare
    # `tobii-gtk` because the .deb/.rpm put it in /usr/bin, which is always on
    # PATH — but a desktop launcher does not read your shell profile, so for a
    # ~/.local/bin install the bare name is a menu entry that silently does
    # nothing. The updater replaces the binary at the same path, so an absolute
    # Exec stays correct across updates.
    sed "s|^Exec=tobii-gtk\$|Exec=$bindir/tobii-gtk|" \
        "$assets/com.tobiilinux.Configuration.desktop" > "$apps/com.tobiilinux.Configuration.desktop"
    cp "$assets/com.tobiilinux.Configuration.svg" "$icons/"

    # Without this the entry can take minutes to appear, or not appear until the
    # next login — which reads as "the install did not work".
    command -v update-desktop-database >/dev/null 2>&1 && \
        update-desktop-database "$apps" 2>/dev/null || true
    if command -v gtk4-update-icon-cache >/dev/null 2>&1; then
        gtk4-update-icon-cache -qtf "$data/icons/hicolor" 2>/dev/null || true
    elif command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -qtf "$data/icons/hicolor" 2>/dev/null || true
    fi
    echo "  $apps/com.tobiilinux.Configuration.desktop"
fi

# ---------------------------------------------------------------------- udev
#
# Without the rule the tracker is only usable as root, which presents as
# "nothing works" rather than as a permissions problem. It is the one step that
# needs sudo, so it is the one step that announces itself.
# The old name is still checked for. Until v0.1.0 this rule was called
# 99-tobii.rules, which is too late in udev's lexical order for
# 73-seat-late.rules to act on its `uaccess` tag — so it granted access only
# through MODE="0666". A leftover copy still wins on mode, undoing the point of
# the new one, so it is removed alongside the install rather than left to sit
# there quietly making the tighter rule pointless.
rule="$assets/60-tobii.rules"
legacy=/etc/udev/rules.d/99-tobii.rules
if [[ -e /etc/udev/rules.d/60-tobii.rules && ! -e "$legacy" ]]; then
    :
elif [[ ! -f "$rule" ]]; then
    :
elif [[ "$udev" == "no" ]]; then
    echo
    echo "  ${bold}The udev rule is not installed${reset}, so the tracker needs root."
    echo "  ${dim}sudo cp $rule /etc/udev/rules.d/ && sudo udevadm control --reload${reset}"
else
    if [[ "$udev" == "ask" ]]; then
        echo
        echo "${bold}The udev rule${reset}"
        echo "${dim}Needed once, so the tracker works without root. This step uses sudo.${reset}"
        # No tty (a pipe, a CI job) means no question: print the command instead
        # of hanging on a prompt nobody can answer.
        if [[ -t 0 ]]; then
            # `|| reply=n`: under `set -e`, Ctrl-D makes read return non-zero
            # and would abort the install after the binaries are already in
            # place. Treat it as "no", which is what it means.
            read -r -p "  Install it now? [Y/n] " reply || reply=n
            if [[ "$reply" =~ ^[Nn] ]]; then udev="no"; fi
        else
            udev="no"
        fi
        if [[ "$udev" == "no" ]]; then
            echo "  ${dim}skipped — sudo cp $rule /etc/udev/rules.d/${reset}"
        fi
    fi
    if [[ "$udev" != "no" ]]; then
        sudo cp "$rule" /etc/udev/rules.d/
        if [[ -e "$legacy" ]]; then
            sudo rm -f "$legacy"
            echo "  removed $legacy (the old rule, which overrode the new one's mode)"
        fi
        sudo udevadm control --reload
        sudo udevadm trigger
        echo "  installed — ${bold}re-plug the Eye Tracker 5${reset} for it to take effect"
    fi
fi
