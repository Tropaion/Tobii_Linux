#!/usr/bin/env bash
# Install an unpacked tobii-linux, for one user or — with --system — for everyone.
#
# The one definition of what "installed" means for a non-package install:
# binaries, the application-menu entry, the icon, the udev rule, and a manifest
# `tobii uninstall` reads to find this install again. Both `scripts/build.sh
# --install` (source tree) and the `install.sh` inside a release tarball run
# this, so the two cannot drift — the release archive used to have no installer
# at all, and the notes told people to `tar -xzf` into ~/.local/bin, which
# unpacks a versioned directory rather than the binaries.
#
#   install-payload.sh <bindir> <bin-src-dir> <asset-dir> \
#       [--udev|--no-udev] [--lean] [--no-bins] [--system]
#
# It is deliberately quiet about what it cannot do: it never uses sudo without
# saying so first, and skips the udev rule rather than prompting when told to.
#
# ROOT is refused unless --system. Under plain `sudo`, HOME is /root, so a
# per-user install went — silently and completely — into root's home: binaries
# in /root/.local/bin and a menu entry nobody would ever see. Reproduced on
# 2026-09-11 by a user who reached for sudo because the updater had told them
# to. --system is the deliberate form: binaries in /usr/local/bin by default,
# the menu entry and icon in /usr/local/share, and no nested sudo.
#
# Test seams, not for users: TOBII_TEST_EUID stands in for `id -u` and
# TOBII_SYSTEM_DATA_DIR for /usr/local/share, so scripts/test-install-payload.sh
# can exercise the root paths without root.
set -euo pipefail

bindir="$1"; binsrc="$2"; assets="$3"; shift 3
udev="ask"
lean=0
bins_wanted=1
system=0
for a in "$@"; do
    case "$a" in
        --udev)    udev="yes" ;;
        --no-udev) udev="no" ;;
        --lean)    lean=1 ;;
        --no-bins) bins_wanted=0 ;;   # `build.sh --udev` alone: the rule only
        --system)  system=1 ;;
        # Refused rather than ignored. An unknown flag used to fall through
        # silently, so a mistyped --no-udev ran the one step it was meant to skip.
        *) echo "install-payload.sh: unknown option: $a" >&2; exit 2 ;;
    esac
done

if [[ -t 1 ]]; then bold=$'\e[1m'; dim=$'\e[2m'; reset=$'\e[0m'
else bold=""; dim=""; reset=""; fi

euid="${TOBII_TEST_EUID:-$(id -u)}"
if [[ $system -eq 0 && $euid -eq 0 ]]; then
    {
        echo "${bold}Not installing as root.${reset}"
        echo "Under sudo your home directory is /root, so this would install for the root"
        echo "account: binaries in /root/.local/bin and a menu entry you would never see."
        echo "  As yourself:      ./install.sh                  (it asks before using sudo, for the udev rule)"
        echo "  For every user:   sudo ./install.sh --system [DIR]    (default /usr/local/bin)"
    } >&2
    exit 1
fi
if [[ $system -eq 1 && $euid -ne 0 ]]; then
    echo "--system installs for every user and needs root:  sudo ./install.sh --system" >&2
    exit 1
fi

# Root runs the udev commands directly; a user goes through sudo, announced.
as_root() { if [[ $euid -eq 0 ]]; then "$@"; else sudo "$@"; fi; }

# A relative XDG_* is ignored, as the XDG spec says and as every reader in the
# program does (tobii_config::paths): otherwise this writes where nothing else
# ever looks, the uninstaller included.
cfg="$HOME/.config"
if [[ ${XDG_CONFIG_HOME:-} == /* ]]; then cfg="$XDG_CONFIG_HOME"; fi
if [[ $system -eq 1 ]]; then
    data="${TOBII_SYSTEM_DATA_DIR:-/usr/local/share}"
else
    data="$HOME/.local/share"
    if [[ ${XDG_DATA_HOME:-} == /* ]]; then data="$XDG_DATA_HOME"; fi
fi
# Absolute: it goes into a desktop entry's Exec and into the manifest, and a
# relative path in either means somewhere else the moment the cwd changes.
bindir="$(realpath -m -- "$bindir")"

# One Exec argument as the Desktop Entry spec wants it: in double quotes, with
# `%` doubled (a lone `%` starts a field code). A path with `"`, `` ` ``, `$` or
# `\` would need the spec's backslash escaping, which applies at two levels that
# launchers do not agree on — so it gets no entry rather than a wrong one.
desktop_arg() {
    case "$1" in
        *[\"\`\$\\]*) return 1 ;;
    esac
    local pct='%'
    printf '"%s"' "${1//"$pct"/%%}"
}

# Replace the first Exec line of a desktop file, line by line: the value is data,
# and sed would read `|` and `&` in it as syntax.
set_exec() {
    local file="$1" value="$2" line done=0 tmp="$1.new-$$"
    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ $done -eq 0 && "$line" == Exec=* ]]; then
            printf 'Exec=%s\n' "$value"; done=1
        else
            printf '%s\n' "$line"
        fi
    done < "$file" > "$tmp"
    mv -f "$tmp" "$file"
}

# The program an Exec line runs: its first argument with the quoting and the
# hub's own escaping undone (`%%` is `%`, a backslash escapes the next character),
# so a path containing one of those is read as the path it is.
exec_program() {
    local s="$1" out="" i c pct='%%'
    s="${s//"$pct"/%}"
    if [[ "$s" == \"* ]]; then
        s="${s:1}"
        for (( i = 0; i < ${#s}; i++ )); do
            c="${s:i:1}"
            if [[ "$c" == "\\" ]]; then i=$((i + 1)); out+="${s:i:1}"
            elif [[ "$c" == '"' ]]; then break
            else out+="$c"; fi
        done
    else
        out="${s%% *}"
    fi
    printf '%s' "$out"
}

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

    # -ef, not string equality: a PATH entry that is a symlink to this directory,
    # or this directory named through one, is the same place.
    on_path=0
    IFS=: read -r -a path_dirs <<< "${PATH:-}"
    for e in "${path_dirs[@]}"; do
        if [[ -n "$e" && -d "$e" && "$e" -ef "$bindir" ]]; then on_path=1; break; fi
    done
    if [[ $on_path -eq 0 ]]; then
        echo
        echo "  ${bold}Note:${reset} $bindir is not in your PATH."
        echo "  ${dim}fish:  fish_add_path $bindir${reset}"
        echo "  ${dim}bash:  echo 'export PATH=\"$bindir:\$PATH\"' >> ~/.bashrc${reset}"
    fi
fi

# The application menu entry and the icon. Only the GUI has anything to show,
# so a --lean install skips them.
if [[ $bins_wanted -eq 1 && $lean -eq 0 && -f "$assets/com.tobiilinux.Configuration.desktop" ]]; then
    apps="$data/applications"
    hicolor="$data/icons/hicolor"
    icons="$hicolor/scalable/apps"
    # Plasma looks only in icon folders that existed when it started (measured:
    # a first install showed a placeholder until plasmashell was restarted), so
    # whether this run creates the folder decides whether to say so below.
    new_icon_dir=0
    if [[ ! -d "$icons" ]]; then new_icon_dir=1; fi
    mkdir -p "$apps" "$icons"

    # Exec is rewritten to the absolute path, quoted. The shipped entry says a
    # bare `tobii-gtk` because the .deb/.rpm put it in /usr/bin, which is always
    # on PATH — but a desktop launcher does not read your shell profile, so for a
    # ~/.local/bin install the bare name is a menu entry that silently does
    # nothing. The updater replaces the binary at the same path, so an absolute
    # Exec stays correct across updates. Written line by line, not with sed: sed
    # read `|` in the path as syntax and aborted the install, and `&` as "the
    # match" and wrote a wrong path.
    if exec_arg="$(desktop_arg "$bindir/tobii-gtk")"; then
        cp "$assets/com.tobiilinux.Configuration.desktop" "$apps/com.tobiilinux.Configuration.desktop"
        set_exec "$apps/com.tobiilinux.Configuration.desktop" "$exec_arg"
    else
        rm -f "$apps/com.tobiilinux.Configuration.desktop"
        echo "  ${bold}No menu entry:${reset} $bindir holds a character a menu entry cannot name"
        echo "  reliably (a quote, a backtick, \$ or a backslash). Start it with $bindir/tobii-gtk."
    fi
    cp "$assets/com.tobiilinux.Configuration.svg" "$icons/"

    # Without this the entry can take minutes to appear, or not appear until the
    # next login — which reads as "the install did not work".
    command -v update-desktop-database >/dev/null 2>&1 && \
        update-desktop-database "$apps" 2>/dev/null || true
    # Refreshed only if one is already there, and never created. GTK trusts an
    # icon-theme.cache over the folder it describes, so a cache this created
    # would hide every icon another program adds there later without refreshing
    # it — and this used to create one in ~/.local/share/icons/hicolor.
    if [[ -f "$hicolor/icon-theme.cache" ]]; then
        if command -v gtk4-update-icon-cache >/dev/null 2>&1; then
            gtk4-update-icon-cache -qtf "$hicolor" 2>/dev/null || true
        elif command -v gtk-update-icon-cache >/dev/null 2>&1; then
            gtk-update-icon-cache -qtf "$hicolor" 2>/dev/null || true
        fi
    fi
    if [[ -f "$apps/com.tobiilinux.Configuration.desktop" ]]; then
        echo "  $apps/com.tobiilinux.Configuration.desktop"
    fi
    if [[ $new_icon_dir -eq 1 ]] && pgrep -x plasmashell >/dev/null 2>&1; then
        echo "  ${dim}Plasma looks only in icon folders that existed when it started, so the${reset}"
        echo "  ${dim}menu entry may show a generic icon until you next log in.${reset}"
    fi
fi

# ------------------------------------------------------------------ manifest
#
# Where this install is, for `tobii uninstall`. A discovery hint only: the
# uninstaller deletes fixed, known names in the directories listed here, never
# whatever this file says — anyone who can write here could edit it.
if [[ $bins_wanted -eq 1 ]]; then
    manifest="$data/tobii-linux/installs"
    mkdir -p "$data/tobii-linux"
    line="bindir=$bindir"
    if ! grep -qxF -- "$line" "$manifest" 2>/dev/null; then
        tmp="$manifest.new-$$"
        { if [[ -f "$manifest" ]]; then cat "$manifest"; fi; echo "$line"; } > "$tmp"
        mv -f "$tmp" "$manifest"
    fi
fi

# ----------------------------------------------------------------- autostart
#
# The login entry names one absolute binary — whichever copy switched it on —
# and nothing else keeps it current. Moving an install (a package removed, a
# tarball put in its place) left it pointing at a binary that no longer existed,
# and the next login started nothing. Repaired only when it points at nothing:
# pointed at another copy that still exists, it is left alone and named, because
# which copy starts at login is the user's choice.
#
# Only an ABSOLUTE path that no longer exists is repaired. A bare name is looked
# up on PATH by the launcher and may well work; an `env …` wrapper is something
# the user wrote; a symlinked entry belongs to a dotfile manager, and rewriting
# it would replace the link with a plain file. Those are named, never touched.
if [[ $system -eq 0 && $bins_wanted -eq 1 && $lean -eq 0 ]]; then
    auto="$cfg/autostart/com.tobiilinux.Configuration.desktop"
    want="$bindir/tobii-gtk"
    if [[ -L "$auto" ]]; then
        echo "  ${dim}Start at login is a symlink ($auto); left as it is.${reset}"
    elif [[ -f "$auto" ]]; then
        target="$(exec_program "$(sed -n 's/^Exec=//p' "$auto" | head -n1)")"
        case "$target" in
            "") ;;
            env|*/env)
                echo "  ${dim}Start at login runs the program through env; left as it is.${reset}" ;;
            /*)
                if [[ "$target" != "$want" && ! -e "$target" ]]; then
                    if exec_arg="$(desktop_arg "$want")"; then
                        set_exec "$auto" "$exec_arg --background"
                        echo "  repaired start at login: it ran $target, which no longer exists"
                    else
                        echo "  ${bold}Start at login${reset} runs $target, which is gone — switch it off and on in the hub."
                    fi
                elif [[ "$target" != "$want" ]]; then
                    echo "  ${dim}Start at login runs $target, not this copy. To change that, switch it${reset}"
                    echo "  ${dim}off and on in the hub you want to start.${reset}"
                fi ;;
            *)
                if ! command -v -- "$target" >/dev/null 2>&1; then
                    echo "  ${dim}Start at login runs \`$target\`, which is not on this PATH; left as it is.${reset}"
                fi ;;
        esac
    fi
fi

# ------------------------------------------------------------ running copies
#
# The hub runs one instance per session and hands every later launch to it. So a
# hub still running from an old or removed copy answers every launch — the menu,
# a terminal, this install — with the old program for as long as it lives. On
# 2026-09-11 a v0.1.0 hub whose package had been removed absorbed five launches
# and four installs this way. Named here, because nothing on screen says so.
#
# Only the invoking user's processes: as a user the kernel hides everyone else's
# anyway, but a --system run is root, and "kill <pid>" advice about another
# person's session hub is not advice to give.
if [[ $bins_wanted -eq 1 ]]; then
    me="${SUDO_UID:-$(id -u)}"
    for p in /proc/[0-9]*; do
        [[ "$(stat -c %u "$p" 2>/dev/null)" == "$me" ]] || continue
        exe="$(readlink "$p/exe" 2>/dev/null)" || continue
        [[ "$(basename -- "${exe% (deleted)}")" == tobii-gtk ]] || continue
        pid="${p#/proc/}"
        if [[ "$exe" == "$bindir/tobii-gtk (deleted)" ]]; then
            echo "  ${bold}The hub is still running the previous version${reset} (pid $pid)."
            echo "  Quit it from its cogwheel menu and start it again to run this one."
        elif [[ "$exe" != "$bindir/tobii-gtk" ]]; then
            echo "  ${bold}Another copy of the hub is running${reset}: $exe (pid $pid)."
            echo "  Every launch of the app goes to it until it exits. Quit it from its"
            echo "  cogwheel menu, or:  kill $pid"
        fi
    done
fi

# ---------------------------------------------------------------------- udev
#
# Without the rule the tracker is only usable as root, which presents as
# "nothing works" rather than as a permissions problem. It is the one step that
# needs root, so it is the one step that announces itself.
# The old name is still checked for. Until v0.1.0 this rule was called
# 99-tobii.rules, which is too late in udev's lexical order for
# 73-seat-late.rules to act on its `uaccess` tag — so it granted access only
# through MODE="0666". A leftover copy still wins on mode, undoing the point of
# the new one, so it is removed alongside the install rather than left to sit
# there quietly making the tighter rule pointless.
rule="$assets/60-tobii.rules"
legacy=/etc/udev/rules.d/99-tobii.rules

# Every hint that tells the user to install the rule by hand has to tell them to
# delete the old one too. The accept-path below does it; a hint that leaves it
# out hands somebody a command that appears to harden the rule and does not,
# because 99- sorts after 60- and its MODE="0666" wins.
legacy_hint() {
    [[ -e "$legacy" ]] || return 0
    echo "  ${dim}sudo rm -f $legacy${reset}   # the old rule still overrides the new one's mode"
}
# Nothing to do only when the installed rule is BYTE-IDENTICAL to the one being
# shipped and there is no stale copy to remove. Testing for the file's presence
# alone meant an updated rule was never installed on top of an older one — and
# the whole reason this release ships a new rule is that the old one was wrong.
if { [[ -f /etc/udev/rules.d/60-tobii.rules ]] \
     && cmp -s "$rule" /etc/udev/rules.d/60-tobii.rules \
     && [[ ! -e "$legacy" ]]; } || [[ ! -f "$rule" ]]; then
    :
elif [[ "$udev" == "no" ]]; then
    echo
    echo "  ${bold}The udev rule is not installed${reset}, so the tracker needs root."
    echo "  ${dim}sudo cp $rule /etc/udev/rules.d/ && sudo udevadm control --reload${reset}"
    legacy_hint
else
    if [[ "$udev" == "ask" ]]; then
        echo
        echo "${bold}The udev rule${reset}"
        if [[ $euid -eq 0 ]]; then
            echo "${dim}Needed once, so the tracker works without root.${reset}"
        else
            echo "${dim}Needed once, so the tracker works without root. This step uses sudo.${reset}"
        fi
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
            legacy_hint
        fi
    fi
    if [[ "$udev" != "no" ]]; then
        as_root cp "$rule" /etc/udev/rules.d/
        if [[ -e "$legacy" ]]; then
            as_root rm -f "$legacy"
            echo "  removed $legacy (the old rule, which overrode the new one's mode)"
        fi
        as_root udevadm control --reload
        # Only the USB subsystem. A bare `udevadm trigger` re-events every
        # device on the machine, which on a desktop means re-probing disks,
        # input devices and graphics for the sake of one tracker.
        as_root udevadm trigger --subsystem-match=usb
        # And misc, for the /dev/uinput grant the virtual joystick needs. The
        # comment above explains why this is not a bare `udevadm trigger`; it
        # was written when the rule only covered the tracker.
        as_root udevadm trigger --subsystem-match=misc
        echo "  installed — ${bold}re-plug the Eye Tracker 5${reset} for it to take effect"
    fi
fi

if [[ $bins_wanted -eq 1 ]]; then
    echo
    if [[ $system -eq 1 ]]; then
        echo "${dim}To remove it later:  sudo tobii uninstall --system${reset}"
    else
        echo "${dim}To remove it later:  tobii uninstall${reset}"
    fi
fi
