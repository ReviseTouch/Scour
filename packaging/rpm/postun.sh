# The same two caches. $1 is the number of copies left: 0 on the last erase,
# 1 during an upgrade, when the new %post has already refreshed them.
if [ "$1" = 0 ]; then
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q /usr/share/applications || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -qtf /usr/share/icons/hicolor || true
    fi
fi
exit 0
