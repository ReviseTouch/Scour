#!/usr/bin/env bash
# Build a filesystem of each format in RAM and check what Scour believes about it.
#
#   sudo scripts/fsmatrix.sh [format...]
#
# Needs root, and there is no way around that: mounting a loopback image needs
# real CAP_SYS_ADMIN. A user namespace does not help — ext4, btrfs, xfs, vfat
# and exfat are not marked FS_USERNS_MOUNT, so `unshare -r -m mount -o loop`
# fails with EPERM. (Tested; it does.)
#
# The images live in /dev/shm, so this touches no disk and leaves nothing
# behind. Each is mounted with the invoking user as owner, the `filesystems`
# example is run against it, and it is unmounted again.
#
# What it is for: `FsTraits` comes from a table of statfs magic numbers, and a
# wrong entry there is not a crash. It is a silently wrong index — a claimed
# `stable_ids` where st_ino is invented makes every file its own duplicate
# after a remount, and a claimed `case_sensitive` where the filesystem folds
# makes Rapor.pdf and rapor.pdf two rows for one file. The FAT family is the
# case that matters, and it is the one no developer machine has mounted.

set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "needs root: sudo $0 $*" >&2
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

echo "building in RAM, $(nproc) cores, $(free -h | awk '/Mem:/{print $7}') available"
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
    if ! mkfs."$fs" -q "$img" >/dev/null 2>&1 && ! mkfs."$fs" "$img" >/dev/null 2>&1; then
        echo "skip $fs: mkfs failed"
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
# Run as the invoking user: the probe writes files, and running it as root
# would measure permissions the service will never have.
sudo -u "#$UID_" env HOME="$HOME_" \
    "$REPO/target/release/examples/filesystems" "${BUILT[@]}" \
    || cargo run --release -q --manifest-path "$REPO/Cargo.toml" \
         -p scour-source-fs --example filesystems -- "${BUILT[@]}"
