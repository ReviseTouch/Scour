#!/bin/bash
# One-time privileged setup for the hasan unit shipped with this repository.
# Read scour.service and 50-scour-service.rules before running with pkexec/sudo.
set -euo pipefail
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
if [[ $EUID != 0 ]]; then
    echo "Run this installer once as administrator (pkexec or sudo)." >&2
    exit 1
fi
scour_repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
[[ $(id -u hasan) == 1000 ]]
[[ -x "$scour_repo/target/release/scour-watch" ]]
[[ -x /home/hasan/.local/bin/scourd ]]

# The allow rule must never point at a privileged executable in a writable home.
install -d -o root -g root -m755 /usr/local/libexec/scour
scour_stage=$(mktemp -d /usr/local/libexec/scour/install.XXXXXX)
trap 'rm -rf -- "$scour_stage"' EXIT
install -o root -g root -m755 "$scour_repo/target/release/scour-watch" "$scour_stage/scour-watch"
install -o root -g root -m644 "$scour_repo/packaging/scour.service" "$scour_stage/scour.service"
install -o root -g root -m644 "$scour_repo/packaging/50-scour-service.rules" "$scour_stage/50-scour-service.rules"

scour_backup=/var/backups/scour-service/$(date -u +%Y%m%d-%H%M%S)
install -d -o root -g root -m700 "$scour_backup"
for scour_file in /usr/local/libexec/scour/scour-watch /etc/systemd/system/scour.service /etc/polkit-1/rules.d/50-scour-service.rules; do
    if [[ -e $scour_file ]]; then cp -a -- "$scour_file" "$scour_backup/"; fi
done
mv -fT -- "$scour_stage/scour-watch" /usr/local/libexec/scour/scour-watch
systemd-analyze verify "$scour_stage/scour.service"
install -o root -g root -m644 "$scour_stage/scour.service" /etc/systemd/system/scour.service
systemctl daemon-reload
# Load the trusted unit before opening its lifecycle permission.
install -o root -g root -m644 "$scour_stage/50-scour-service.rules" /etc/polkit-1/rules.d/50-scour-service.rules
systemctl enable scour.service
echo "Installed. Backup: $scour_backup"
echo "hasan can now start, stop and restart scour.service without authentication."
