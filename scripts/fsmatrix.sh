#!/usr/bin/env bash
# Build a filesystem of each format in RAM and check what `FsTraits` believes
# about it: a wrong statfs magic entry is a silently wrong index, not a crash.
# Root is for `mount(2)` alone and unavoidable — these formats are not
# FS_USERNS_MOUNT, and a FUSE mount reports FUSE's magic, not the image's.
#
#   sudo bash scripts/fsmatrix.sh [format...]     # `bash`, for sudo's secure_path

set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "needs root: sudo bash $0 $*" >&2
    exit 1
fi

# Whoever called sudo owns the mounts, so the probe can write to them.
UID_="${SUDO_UID:-0}"
GID_="${SUDO_GID:-0}"
HOME_="$(getent passwd "$UID_" | cut -d: -f6)"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

ALL=(ext4 ext2 btrfs xfs vfat exfat)
FORMATS=("${@:-${ALL[@]}}")

BASE=/dev/shm/scour-fsmatrix
mkdir -p "$BASE"
cleanup() {
    for m in "$BASE"/mnt-*; do
        [[ -d $m ]] && mountpoint -q "$m" && umount "$m" || true
    done
    rm -rf "$BASE"
}
trap cleanup EXIT

# xfs will not go below 300 MB, and btrfs wants room for its metadata.
size_for() { case "$1" in xfs) echo 320 ;; btrfs) echo 200 ;; *) echo 96 ;; esac; }

# vfat and exfat have no ownership of their own; it is set at mount time.
mount_opts_for() {
    case "$1" in
        vfat|exfat) echo "uid=$UID_,gid=$GID_" ;;
        *) echo "" ;;
    esac
}

# LC_ALL, because `free` translates its headings and this parses one.
echo "building in RAM, $(LC_ALL=C free -h | awk '/^Mem:/{print $7}') available in /dev/shm"
echo

BUILT=()
for fs in "${FORMATS[@]}"; do
    if ! command -v "mkfs.$fs" >/dev/null; then
        echo "skip $fs: mkfs.$fs is not installed"
        continue
    fi
    img="$BASE/$fs.img"
    mnt="$BASE/mnt-$fs"
    mkdir -p "$mnt"
    truncate -s "$(size_for "$fs")M" "$img"
    # `-q` is not universal: vfat and exfat have no such flag.
    if ! mkfs."$fs" -q "$img" >/dev/null 2>&1 && ! mkfs."$fs" "$img" >/dev/null 2>&1; then
        echo "skip $fs: mkfs.$fs would not make a filesystem in $(size_for "$fs") MB"
        continue
    fi
    opts="$(mount_opts_for "$fs")"
    if ! mount -o "loop${opts:+,$opts}" "$img" "$mnt" 2>/dev/null; then
        echo "skip $fs: mount failed"
        continue
    fi
    # Owned by the caller for the formats that carry ownership themselves.
    chown "$UID_:$GID_" "$mnt" 2>/dev/null || true
    BUILT+=("$mnt")
done

echo
# As the invoking user: as root it would measure permissions the service will
# never have.
sudo -u "#$UID_" env HOME="$HOME_" \
    "$REPO/target/release/examples/filesystems" "${BUILT[@]}" \
    || cargo run --release -q --manifest-path "$REPO/Cargo.toml" \
         -p scour-source-fs --example filesystems -- "${BUILT[@]}"

# What the example cannot ask: it sees one mount session, so it can only test
# that ids are distinct and survive a rename. FAT synthesises `st_ino` from the
# directory entry's position, and whether that survives a remount is the claim.
echo
printf '%-8s %-22s %s\n' "format" "ids survive a remount" "ids survive a move"

for mnt in "${BUILT[@]}"; do
    fs="${mnt##*/mnt-}"
    img="$BASE/$fs.img"
    opts="$(mount_opts_for "$fs")"
    d="$mnt/.scour-remount"
    sub="$d/moved"
    mkdir -p "$sub"
    for i in $(seq 0 49); do : > "$d/f$i"; done
    # A file of its own for the move, or the remount count is short by one.
    : > "$d/mover"
    sync

    before="$(cd "$d" && stat -c '%n %i' f* | sort)"

    # A move within one filesystem, which on FAT rewrites the directory entry
    # the number is derived from.
    mv "$d/mover" "$sub/mover"
    moved_before="$(stat -c '%i' "$sub/mover")"

    umount "$mnt"
    mount -o "loop${opts:+,$opts}" "$img" "$mnt"

    after="$(cd "$d" && stat -c '%n %i' f* 2>/dev/null | sort)"
    same="$(comm -12 <(echo "$before") <(echo "$after") | wc -l)"
    total="$(echo "$before" | wc -l)"
    moved_after="$(stat -c '%i' "$sub/mover" 2>/dev/null || echo "-")"
    if [[ "$moved_before" == "$moved_after" ]]; then move="yes"; else move="NO"; fi

    printf '%-8s %-22s %s\n' "$fs" "$same/$total" "$move"
    rm -rf "$d"
done
