#!/usr/bin/env bash
# Build real filesystems in RAM and check `crates/scour-source-fs/src/fs.rs`'s
# claims against them. Root is for `mount` alone; all of it is deleted on exit.
#
#   sudo scripts/fstest.sh              # every filesystem the machine can make
#   sudo scripts/fstest.sh exfat vfat   # only these
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${TMPDIR:-/dev/shm}/scour-fstest.$$"
EXAMPLE="$ROOT/target/release/examples/fstraits"

# Minimum sizes, in MiB: XFS refuses under 300 and btrfs under ~110, while the
# FAT family and ext4 are happy in a tenth of that.
declare -A SIZE=( [fat16]=64 [fat32]=64 [exfat]=64 [ext4]=64 [ext2]=64 [xfs]=320 [btrfs]=128 [f2fs]=128 )
# The command, where it is not simply `mkfs.<name>`: `mkfs.vfat` on a 64 MB
# image produces FAT16, and FAT32 needs `-F 32` and at least 33 MB.
declare -A MKFS=( [fat16]="mkfs.vfat -F 16" [fat32]="mkfs.vfat -F 32" )
# What makes a filesystem usable by the invoking user, where it has the idea.
declare -A OPTS=( [fat16]="uid=SUDO_UID,gid=SUDO_GID" [fat32]="uid=SUDO_UID,gid=SUDO_GID" [exfat]="uid=SUDO_UID,gid=SUDO_GID" )

cleanup() {
    for m in "$WORK"/mnt-*; do
        [ -d "$m" ] && mountpoint -q "$m" && umount "$m" 2>/dev/null
    done
    rm -rf "$WORK"
}
trap cleanup EXIT

if [ "$(id -u)" -ne 0 ]; then
    echo "needs root, for mount and nothing else: sudo $0 $*" >&2
    exit 1
fi
if [ ! -x "$EXAMPLE" ]; then
    echo "build the probe first:  cargo build --release -p scour-source-fs --example fstraits" >&2
    exit 1
fi

WANT=("$@")
[ ${#WANT[@]} -eq 0 ] && WANT=(fat16 fat32 exfat ext4 xfs btrfs f2fs)
mkdir -p "$WORK"

# The awkward cases: a name differing only by case, a non-ASCII name, a name
# with a semicolon, and a hard link.
populate() {
    local d="$1"
    mkdir -p "$d/sub/deeper"
    echo hello > "$d/README.md"
    echo hello > "$d/readme.md" 2>/dev/null   # fails on a case-insensitive fs
    echo v > "$d/sub/Değişiklik Raporu.txt"
    echo v > "$d/sub/rapor; ek.txt"
    echo v > "$d/sub/deeper/target.bin"
    ln "$d/sub/deeper/target.bin" "$d/sub/link.bin" 2>/dev/null
    sync
}

echo "MEDIUM is always solid-state here: everything is a loop device over"
echo "tmpfs, so the rotational flag says nothing about the format being tested."
echo
printf '%-8s %-10s %-9s %-8s %-14s %-8s %s\n' \
    FS SIZE STABLE_IDS CASE MEDIUM THREADS "reality"
for fs in "${WANT[@]}"; do
    mk="${MKFS[$fs]:-mkfs.$fs}"
    command -v "${mk%% *}" >/dev/null || { printf '%-8s %s\n' "$fs" "no ${mk%% *} on this machine"; continue; }

    mb="${SIZE[$fs]:-128}"
    img="$WORK/$fs.img"
    mnt="$WORK/mnt-$fs"
    mkdir -p "$mnt"
    truncate -s "${mb}M" "$img"
    if ! $mk "$img" >/dev/null 2>&1; then
        printf '%-8s %s\n' "$fs" "mkfs failed at ${mb}M"
        continue
    fi

    opt="${OPTS[$fs]:-}"
    opt="${opt//SUDO_UID/${SUDO_UID:-0}}"
    opt="${opt//SUDO_GID/${SUDO_GID:-0}}"
    if [ -n "$opt" ]; then
        mount -o "loop,$opt" "$img" "$mnt" 2>/dev/null || { printf '%-8s %s\n' "$fs" "mount failed"; continue; }
    else
        mount -o loop "$img" "$mnt" 2>/dev/null || { printf '%-8s %s\n' "$fs" "mount failed"; continue; }
        chown -R "${SUDO_UID:-0}:${SUDO_GID:-0}" "$mnt"
    fi

    real=$(file -b "$img" | grep -oE 'FAT \([0-9]+ bit\)|exFAT|ext[234]|XFS|BTRFS|F2FS' | head -1)
    populate "$mnt"
    # How many entries the directory holds, not how many names resolve: on a
    # case-insensitive filesystem `-f` says yes to both spellings.
    n=$(find "$mnt" -maxdepth 1 -iname 'readme.md' | wc -l)
    both=$([ "$n" -ge 2 ] && echo 2-entries || echo 1-entry)
    # One inode with two links → hard links are supported.
    links=$(stat -c %h "$mnt/sub/deeper/target.bin" 2>/dev/null || echo 0)

    read -r _ ids case medium threads _ < <(
        "$EXAMPLE" "$mnt" | awk '{print $1, $2, $3, $4, $5, $6}'
    )
    printf '%-8s %-10s %-9s %-8s %-14s %-8s %s\n' \
        "$fs" "${mb}M" \
        "${ids#*=}" "${case#*=}" "${medium#*=}" "${threads#*=}" \
        "links=$links readme=$both ${real:+is=$real}"
done
