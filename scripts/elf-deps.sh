# shellcheck shell=bash
# What the packaging scripts read out of the release binaries, defined once.
#
# Sourced, not run: scripts/package.sh uses it for the .deb's glibc floor and
# the .rpm's soname requirements, scripts/aur-bin.sh for the Arch package's
# `depends`, scripts/release.sh for the glibc floor it reports for the archive,
# and the release workflow for its glibc gate. Two copies of "which glibc do
# these binaries need" is how two packages of the same binaries would come to
# declare two different floors.
#
# LC_ALL=C on every objdump: it TRANSLATES its field names — `objdump -f` reads
# "Dateiformat elf64-x86-64" on a German machine, so a probe for "file format"
# found nothing and every rpm requirement lost its (64bit) tag. The same trap
# `install.rs` documents for `pacman -Qo`, walked into once already.

# The highest GLIBC_x.y symbol version any of the given ELF files needs, printed
# without the prefix ("2.39"), or nothing when none of them has one.
#
# `sort -V` puts the newer last, and an empty first operand sorts before every
# version, so the running maximum needs no special case for the first file.
# `[0-9]` after the underscore so GLIBC_PRIVATE is not read as an empty version.
elf_glibc_floor() {
    local bin this floor=""
    for bin in "$@"; do
        this="$(LC_ALL=C objdump -T "$bin" 2>/dev/null \
            | grep -o 'GLIBC_[0-9][0-9.]*' | sort -V | tail -1 || true)"
        [[ -n "$this" ]] || continue
        floor="$(printf '%s\n%s\n' "$floor" "${this#GLIBC_}" | sort -V | tail -1)"
    done
    printf '%s' "$floor"
}

# The DT_NEEDED sonames of the given ELF files, one per line, each once, in the
# order they are first seen. Nothing at all when objdump cannot read them, so a
# caller that needs the list must check it is not empty.
elf_needed() {
    LC_ALL=C objdump -p "$@" 2>/dev/null \
        | awk '$1 == "NEEDED" && !seen[$2]++ { print $2 }' || true
}

# Whether an ELF file is 64-bit, which rpm writes as a "(64bit)" tag on every
# soname requirement and reads from the file itself rather than from a list of
# architecture names.
#
# The output is captured before it is matched: `objdump | grep -q` under
# `pipefail` can report the MATCH as a failure, because grep exits at the first
# hit and objdump then dies of SIGPIPE writing the rest.
elf_is_64() {
    local header
    header="$(LC_ALL=C objdump -f "$1" 2>/dev/null || true)"
    [[ "$header" == *"file format elf64"* ]]
}
