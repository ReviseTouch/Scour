# Refresh the two caches the desktop reads, and enable the service for every
# account's session: from the next login on, the index is kept current
# whether a window is open or not. Nothing starts now — a face starts the
# service when it finds none, and that covers this session.
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
exit 0
