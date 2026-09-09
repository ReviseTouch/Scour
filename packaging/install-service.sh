#!/bin/bash
# One-time privileged setup: the fanotify helper, its system unit and the polkit
# rule that lets one account manage the unit. Read scour.service and
# 50-scour-service.rules first.
#
#   sudo bash packaging/install-service.sh [--user NAME] [ROOT...]
#
# The user defaults to whoever ran sudo/pkexec; the roots (filesystems to mark)
# default to /home. `scourd` must already be installed in that user's
# ~/.local/bin, which `install.sh` does.
set -euo pipefail
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
if [[ $EUID != 0 ]]; then
    echo "Run this installer once as administrator (pkexec or sudo)." >&2
    exit 1
fi
user=${SUDO_USER:-}
if [[ -z $user && -n ${PKEXEC_UID:-} ]]; then user=$(id -un "$PKEXEC_UID"); fi
if [[ ${1:-} == --user ]]; then user=$2; shift 2; fi
[[ -n $user && $user != root ]] || { echo "which account runs Scour? pass --user NAME" >&2; exit 1; }
uid=$(id -u "$user")
home=$(getent passwd "$user" | cut -d: -f6)
roots=("$@"); [[ ${#roots[@]} -gt 0 ]] || roots=(/home)
for r in "${roots[@]}"; do [[ -d $r ]] || { echo "not a directory: $r" >&2; exit 1; }; done

scour_repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
helper=$scour_repo/target/release/scour-watch
[[ -x $helper ]] || helper=$scour_repo/bin/scour-watch
[[ -x $helper ]] || { echo "scour-watch not found: cargo build --release -p scour-watch" >&2; exit 1; }
[[ -x $home/.local/bin/scourd ]] || { echo "$home/.local/bin/scourd is missing: run install.sh as $user first" >&2; exit 1; }

# The allow rule must never point at a privileged executable in a writable home.
install -d -o root -g root -m755 /usr/local/libexec/scour
stage=$(mktemp -d /usr/local/libexec/scour/install.XXXXXX)
trap 'rm -rf -- "$stage"' EXIT
install -o root -g root -m755 "$helper" "$stage/scour-watch"
sed -e "s|@USER@|$user|g" -e "s|@UID@|$uid|g" -e "s|@HOME@|$home|g" -e "s|@ROOTS@|${roots[*]}|g" \
    "$scour_repo/packaging/scour.service" > "$stage/scour.service"
sed -e "s|@USER@|$user|g" "$scour_repo/packaging/50-scour-service.rules" > "$stage/50-scour-service.rules"
chmod 644 "$stage/scour.service" "$stage/50-scour-service.rules"

backup=/var/backups/scour-service/$(date -u +%Y%m%d-%H%M%S)
install -d -o root -g root -m700 "$backup"
for f in /usr/local/libexec/scour/scour-watch /etc/systemd/system/scour.service /etc/polkit-1/rules.d/50-scour-service.rules; do
    if [[ -e $f ]]; then cp -a -- "$f" "$backup/"; fi
done
mv -fT -- "$stage/scour-watch" /usr/local/libexec/scour/scour-watch
systemd-analyze verify "$stage/scour.service"
install -o root -g root -m644 "$stage/scour.service" /etc/systemd/system/scour.service
systemctl daemon-reload
# Load the trusted unit before opening its lifecycle permission.
install -o root -g root -m644 "$stage/50-scour-service.rules" /etc/polkit-1/rules.d/50-scour-service.rules
systemctl enable scour.service
echo "Installed for $user (roots: ${roots[*]}). Backup: $backup"
echo "$user can now start, stop and restart scour.service without a prompt:  systemctl start scour.service"
