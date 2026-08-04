#!/usr/bin/env bash
# Build real filesystems in RAM and check what Scour makes of them.
#
# Every claim in `crates/scour-source-fs/src/fs.rs` — this one has stable
# inode numbers, that one is case-insensitive, this other one is a spinning
# disk — was written against two NVMe drives and one NTFS volume, because that
# is what this machine has. Everything else was reasoned about rather than
# tried.
#
# This makes the rest available: a file in tmpfs, formatted, mounted on a loop
# device, filled with a small tree, and handed to the same code paths the real
# scanner uses. tmpfs so the disk is never touched; loop so no partition is
# risked; sizes chosen per filesystem because their minimums differ by two
# orders of magnitude.
#
#   sudo scripts/fstest.sh              # every filesystem the machine can make
#   sudo scripts/fstest.sh exfat vfat   # only these
#
# Needs root — for `mount`, and only for that. Everything it mounts is a file
# it created under /dev/shm, and it unmounts and deletes all of them on exit,
# including on failure.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${TMPDIR:-/dev/shm}/scour-fstest.$$"
EXAMPLE="$ROOT/target/release/examples/fstraits"

# Minimum sizes, in MiB. XFS refuses under 300 MB and btrfs under ~110; the
# FAT family and ext4 are happy in a tenth of that. Using one size for all of
# them would mean either wasting a gigabyte of RAM or skipping half the list.
declare -A SIZE=( [vfat]=64 [exfat]=64 [ext4]=64 [ext2]=64 [xfs]=320 [btrfs]=128 [f2fs]=128 )
# Mount options that make a filesystem usable by the invoking user rather than
# only by root, where the filesystem supports the idea at all.
declare -A OPTS=( [vfat]="uid=SUDO_UID,gid=SUDO_GID" [exfat]="uid=SUDO_UID,gid=SUDO_GID" )

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
[ ${#WANT[@]} -eq 0 ] && WANT=(vfat exfat ext4 xfs btrfs f2fs)
mkdir -p "$WORK"

# A small tree with the awkward cases in it: a name that only differs by case,
# a non-ASCII name, a name with a semicolon, and a hard link.
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
    mk="mkfs.$fs"
    command -v "$mk" >/dev/null || { printf '%-8s %s\n' "$fs" "no $mk on this machine"; continue; }

    mb="${SIZE[$fs]:-128}"
    img="$WORK/$fs.img"
    mnt="$WORK/mnt-$fs"
    mkdir -p "$mnt"
    truncate -s "${mb}M" "$img"
    if ! "$mk" "$img" >/dev/null 2>&1; then
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

    populate "$mnt"
    # How many *entries* the directory actually holds, not how many names
    # resolve. On a case-insensitive filesystem `readme.md` finds `README.md`,
    # so testing `-f` on both says yes everywhere — which is exactly what the
    # first version of this script reported for vfat and exfat.
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
        "links=$links readme=$both"
done
