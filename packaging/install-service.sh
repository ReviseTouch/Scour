#!/bin/bash
# Privileged setup: once per machine for the fanotify helper, the scour@.service
# template and the polkit rule, then once per account for that person's roots and
# instance. Read scour@.service and 50-scour-service.rules first.
#
#   sudo bash packaging/install-service.sh [--user NAME] [ROOT...]
#
# Run it again with another --user to add a second person; nothing belonging to
# the first is touched. The account defaults to whoever ran sudo/pkexec and the
# roots — the filesystems to mark — to /home. `scourd` must already be in that
# account's ~/.local/bin, which install.sh puts there.
#
# A machine still carrying the old single-user scour.service is migrated: that
# unit is stopped, disabled and backed up, and its account and roots become the
# first instance unless --user says otherwise.
set -euo pipefail
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
if [[ $EUID != 0 ]]; then
    echo "Run this installer as administrator (pkexec or sudo)." >&2
    exit 1
fi

old_unit=/etc/systemd/system/scour.service
# What the superseded unit was configured with: field "user" or "roots".
old_of() {
    [[ -e $old_unit ]] || return 0
    awk -v want="$1" '/^ExecStart=/ && !seen {
        seen = 1
        for (i = 2; i <= NF; i++) {
            if ($i == "--as") { u = $(++i); continue }
            if ($i == "--") break
            if ($i ~ /^\//) r = r " " $i
        }
        print (want == "user" ? u : substr(r, 2))
    }' "$old_unit"
}

old_user=$(old_of user)
old_roots=$(old_of roots)

user=${SUDO_USER:-}
if [[ -z $user && -n ${PKEXEC_UID:-} ]]; then user=$(id -un "$PKEXEC_UID"); fi
named=0
if [[ ${1:-} == --user ]]; then user=$2; named=1; shift 2; fi
roots=("$@")
if [[ $named == 0 && -n $old_user ]]; then user=$old_user; fi
if [[ ${#roots[@]} -eq 0 && -n $old_roots ]]; then read -ra roots <<<"$old_roots"; fi

[[ -n $user && $user != root ]] || { echo "which account runs Scour? pass --user NAME" >&2; exit 1; }
# The name becomes a unit name and a file name, so only what both accept.
[[ $user =~ ^[a-zA-Z_][a-zA-Z0-9_.-]*$ ]] || { echo "not a usable account name: $user" >&2; exit 1; }
uid=$(id -u "$user")
home=$(getent passwd "$user" | cut -d: -f6)
[[ -n $home ]] || { echo "no home directory for $user in the password database" >&2; exit 1; }
[[ ${#roots[@]} -gt 0 ]] || roots=(/home)
for r in "${roots[@]}"; do [[ -d $r ]] || { echo "not a directory: $r" >&2; exit 1; }; done

scour_repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
helper=$scour_repo/target/release/scour-watch
[[ -x $helper ]] || helper=$scour_repo/bin/scour-watch
[[ -x $helper ]] || { echo "scour-watch not found: cargo build --release -p scour-watch" >&2; exit 1; }
[[ -x $home/.local/bin/scourd ]] || { echo "$home/.local/bin/scourd is missing: run install.sh as $user first" >&2; exit 1; }

# Two writers race for the index lock and the loser restarts forever, so the
# per-user unit has to be off before a system instance is enabled for the account.
enabled=$(ls "$home"/.config/systemd/user/*.wants/scourd.service 2>/dev/null | head -1 || true)
if [[ -n $enabled ]]; then
    echo "$user has the per-user scourd.service enabled ($enabled)." >&2
    echo "Only one writer may hold the index. Turn it off as $user, then run this again:" >&2
    echo "    systemctl --user disable --now scourd.service" >&2
    exit 1
fi

# The allow rule must never point at a privileged executable in a writable home.
install -d -o root -g root -m755 /usr/local/libexec/scour /etc/scour /etc/polkit-1/rules.d
stage=$(mktemp -d /usr/local/libexec/scour/install.XXXXXX)
trap 'rm -rf -- "$stage"' EXIT
install -o root -g root -m755 "$helper" "$stage/scour-watch"
# Staged under the instance's own name so `systemd-analyze verify` resolves %i
# exactly as systemd will; the file installed from it is the template.
install -m644 "$scour_repo/packaging/scour@.service" "$stage/scour@$user.service"
install -m644 "$scour_repo/packaging/50-scour-service.rules" "$stage/50-scour-service.rules"
printf '# Which filesystems scour@%s.service marks, space separated. Restart to apply.\nSCOUR_ROOTS=%s\n' \
    "$user" "${roots[*]}" > "$stage/$user.conf"
# No specifier turns %i into a uid, and /run/user/<uid> is where the socket goes.
printf '[Unit]\nAfter=user@%s.service\nWants=user@%s.service\n' "$uid" "$uid" > "$stage/10-session.conf"
chmod 644 "$stage/scour@$user.service" "$stage/50-scour-service.rules" "$stage/$user.conf" "$stage/10-session.conf"

dropin=/etc/systemd/system/scour@$user.service.d
backup=/var/backups/scour-service/$(date -u +%Y%m%d-%H%M%S)
install -d -o root -g root -m700 "$backup"
for f in /usr/local/libexec/scour/scour-watch "$old_unit" /etc/systemd/system/scour@.service \
         "$dropin/10-session.conf" "/etc/scour/$user.conf" \
         /etc/polkit-1/rules.d/50-scour-service.rules; do
    if [[ -e $f ]]; then cp -a -- "$f" "$backup/"; fi
done

mv -fT -- "$stage/scour-watch" /usr/local/libexec/scour/scour-watch
systemd-analyze verify "$stage/scour@$user.service"
install -o root -g root -m644 "$stage/scour@$user.service" /etc/systemd/system/scour@.service
install -d -o root -g root -m755 "$dropin"
install -o root -g root -m644 "$stage/10-session.conf" "$dropin/10-session.conf"
install -o root -g root -m644 "$stage/$user.conf" "/etc/scour/$user.conf"

# Off with the superseded unit before the instance that replaces it goes on.
if [[ -e $old_unit ]]; then
    if systemctl is-enabled --quiet scour.service 2>/dev/null; then
        systemctl disable --now scour.service
        echo "Migrated: scour.service was enabled; it is now stopped and disabled."
    else
        systemctl stop scour.service 2>/dev/null || true
    fi
    rm -f -- "$old_unit"
    echo "The single-user unit was removed (a copy is in $backup); scour@$user.service replaces it."
    if [[ -n $old_user && $old_user != "$user" ]]; then
        echo "It belonged to $old_user, who now has no instance. Give them one with --user $old_user."
    fi
fi

systemctl daemon-reload
# Load the trusted unit before opening its lifecycle permission.
install -o root -g root -m644 "$stage/50-scour-service.rules" /etc/polkit-1/rules.d/50-scour-service.rules
systemctl enable "scour@$user.service"
echo "Installed scour@$user.service (roots: ${roots[*]}). Backup: $backup"
echo "$user can now start, stop and restart it without a prompt:  systemctl start scour@$user.service"
echo "Another person on this machine:  sudo bash packaging/install-service.sh --user NAME [ROOT...]"
