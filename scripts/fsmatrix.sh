#!/usr/bin/env bash
# Build a filesystem of each format in RAM and check what Scour believes about it.
#
#   sudo bash scripts/fsmatrix.sh [format...]
#
# `bash` explicitly, and from the repository: sudo resolves a bare relative
# path against secure_path rather than the working directory.
#
# Needs root, and there is no way around that. Two were tried:
#
#   * A user namespace does not help. ext4, btrfs, xfs, vfat and exfat are not
#     marked FS_USERNS_MOUNT, so `unshare -r -m mount -o loop` fails with
#     EPERM whatever capabilities the namespace hands out. (Tested; it does.)
#
#   * FUSE would mount rootless — fuse2fs and ntfs-3g exist and FUSE *is*
#     FS_USERNS_MOUNT — but it would measure the wrong thing. `statfs` on a
#     FUSE mount returns FUSE's magic, not the magic of whatever is underneath,
#     so `traits_of` lands in its UNKNOWN branch and answers about FUSE. Live
#     proof, from mounts this machine already has:
#
#       /run/user/1000/gvfs   fuse.gvfsd-fuse   Network   case=yes ids=NO
#
#     An ext4 image behind fuse2fs would read exactly the same. The sudo is
#     not an oversight; it is what the question requires.
#
# RAM is not the part that needs privilege — the images live in /dev/shm and
# no disk is touched. `mount(2)` is.
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
    # `-q` is not universal — vfat and exfat have no such flag — so the quiet
    # form is tried first and the plain one is the fallback.
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
# Run as the invoking user: the probe writes files, and running it as root
# would measure permissions the service will never have.
sudo -u "#$UID_" env HOME="$HOME_" \
    "$REPO/target/release/examples/filesystems" "${BUILT[@]}" \
    || cargo run --release -q --manifest-path "$REPO/Cargo.toml" \
         -p scour-source-fs --example filesystems -- "${BUILT[@]}"

# --- what the example cannot ask -------------------------------------------
#
# `stable_ids` says `st_ino` "is stored on disk and survives a remount". The
# example runs as an ordinary user against something already mounted, so it can
# only see one mount session: it answers "are the numbers distinct, and do they
# survive a rename", which is a weaker question with the same shape. FAT
# synthesises `st_ino` from the directory entry's position on disk, and whether
# *that* survives being unmounted and mounted again is the whole of the claim.
#
# Only root can unmount, so it is asked here.
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
    # A file of its own for the move, so the remount count is not short by the
    # one that was moved out of the compared set — the control read 49/50 and
    # the missing one was this.
    : > "$d/mover"
    sync

    before="$(cd "$d" && stat -c '%n %i' f* | sort)"

    # A move within the same filesystem, which on FAT rewrites the directory
    # entry the number is derived from.
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
