# Stop the service in the sessions open now before its binary goes away. $1 is
# the number of copies left after this: 0 on the last erase, 1 during an
# upgrade, when %post restarts it on the new binary instead.
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

if [ "$1" = 0 ]; then
    in_sessions stop
fi
exit 0
