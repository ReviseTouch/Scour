#!/bin/sh
# Put Scour where the desktop can find it, without asking for a password.
#
# **Everything here is unprivileged and reversible.** Binaries go to
# `~/.local/bin`, the menu entry and its icon to `~/.local/share`. The one
# thing that needs root — the fanotify mark that makes watching a whole
# filesystem cheap — is printed at the end rather than done, because a script
# that asks for a password is a script nobody should run without reading, and
# Scour works without it.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
bin=${PREFIX:-$HOME/.local}/bin
share=${PREFIX:-$HOME/.local}/share

mkdir -p "$bin" "$share/applications" "$share/icons/hicolor/scalable/apps"

for b in scour scourd scour-gui scour-tui scour-web scour-watch scour-mcp; do
    [ -f "$here/bin/$b" ] || continue
    install -m755 "$here/bin/$b" "$bin/$b"
    echo "  $bin/$b"
done
install -m755 "$here/scripts/scour-open" "$bin/scour-open"
install -m644 "$here/packaging/scour.desktop" "$share/applications/scour.desktop"
install -m644 "$here/assets/scour.svg" "$share/icons/hicolor/scalable/apps/scour.svg"
update-desktop-database "$share/applications" 2>/dev/null || true
gtk-update-icon-cache -qtf "$share/icons/hicolor" 2>/dev/null || true

echo
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "  NOTE: $bin is not on your PATH. Add it to your shell's profile." ;;
esac

cat <<'TXT'

  Start it:

      scourd &            # indexes your home directory on first run
      scour rapor         # search from the terminal
      scour-gui           # the window   (also in the application menu)
      scour-tui           # the terminal face
      scour-web           # opens in a browser

  Watching, and why it is worth a password once:

      Without a filesystem mark, scourd watches with inotify — one watch per
      directory, from a budget shared with everything else in the session. A
      large home does not fit, and scourd will say so and reconcile by walking
      instead. Nothing breaks; changes take longer to appear.

      To have it watched properly, install the system unit:

          sudo install -m644 packaging/scour.service /etc/systemd/system/scour.service
          sudo systemctl daemon-reload
          sudo systemctl enable --now scour.service

      Read that file first — it explains what the privilege is for and where it
      is dropped.

  Uninstall: delete the files listed above. The index and settings live under
  ~/.local/share/scour and ~/.config/scour; `scour where` prints both.

TXT
