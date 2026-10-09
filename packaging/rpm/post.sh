# Refresh the two caches the desktop reads, enable the service for every
# account's session, and start it — or restart it on the new binary — in the
# sessions open now, unless an administrator has disabled it.
# Every account with a session open now: run `systemctl --user ACTION` on
# scourd.service in its own user manager, as that account. Accounts below
# UID_MIN are left alone: a display manager's greeter has a session too.
in_sessions() {
    command -v loginctl >/dev/null 2>&1 && command -v runuser >/dev/null 2>&1 || return 0
    min=$(awk '$1 == "UID_MIN" { print $2 }' /etc/login.defs 2>/dev/null)
    for uid in $(loginctl list-users --no-legend 2>/dev/null | awk '{ print $1 }'); do
        [ "$uid" -ge "${min:-1000}" ] 2>/dev/null || continue
        [ -S "/run/user/$uid/bus" ] || continue
        who=$(id -nu "$uid" 2>/dev/null) || continue
        runuser -u "$who" -- env XDG_RUNTIME_DIR="/run/user/$uid" \
            DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" \
            systemctl --user "$1" scourd.service >/dev/null 2>&1 || true
    done
}
# A scriptlet that exits non-zero fails the transaction, hence the || true.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -qtf /usr/share/icons/hicolor || true
fi
# Once, noted, so an administrator's later `systemctl --global disable` stays
# through upgrades. Not %systemd_user_post: that is a preset, and a preset
# leaves a unit no distribution lists disabled.
if [ ! -e /var/lib/scour/user-unit-enabled ] && command -v systemctl >/dev/null 2>&1; then
    systemctl --global enable scourd.service >/dev/null 2>&1 || true
    mkdir -p /var/lib/scour && : >/var/lib/scour/user-unit-enabled
fi
if command -v systemctl >/dev/null 2>&1 && systemctl --global is-enabled --quiet scourd.service 2>/dev/null; then
    in_sessions restart
fi
exit 0
