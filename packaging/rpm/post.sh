# Refresh the two caches the desktop reads, and nothing else. Installing Scour
# does not start it: which face opens and whether the service runs are the
# owner's choices, made from inside a face or with `systemctl --user`.
# A scriptlet that exits non-zero fails the transaction, hence the || true.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -qtf /usr/share/icons/hicolor || true
fi
exit 0
