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

# A key to open it, on the desktops that let a script set one. Super+F unless
# `SCOUR_KEY` says otherwise; `SCOUR_KEY=none` skips this. Pressing it again
# brings the same window forward — scour-gui never opens a second copy.
key=${SCOUR_KEY:-super+f}
bound=""
if [ "$key" != none ]; then
    gnome_key=$(printf '%s' "$key" | sed -e 's/super+/<Super>/' -e 's/ctrl+/<Control>/' -e 's/alt+/<Alt>/' -e 's/shift+/<Shift>/')
    kde_key=$(printf '%s' "$key" | sed -e 's/super+/Meta+/' -e 's/ctrl+/Ctrl+/' -e 's/alt+/Alt+/' -e 's/shift+/Shift+/' -e 's/+\(.\)$/+\U\1/')
    if command -v gsettings >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1 \
        && gsettings list-schemas 2>/dev/null | grep -qx org.gnome.settings-daemon.plugins.media-keys; then
        media=org.gnome.settings-daemon.plugins.media-keys
        path=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/scour/
        entry="$media.custom-keybinding:$path"
        gsettings set "$entry" name 'Scour' \
            && gsettings set "$entry" command "$bin/scour-gui" \
            && gsettings set "$entry" binding "$gnome_key" \
            && current=$(gsettings get "$media" custom-keybindings) \
            && python3 - "$path" "$current" <<'PY' | xargs -0 gsettings set "$media" custom-keybindings \
            && bound="$key (GNOME)"
import ast, sys
want, current = sys.argv[1], sys.argv[2]
paths = [] if current.strip() in ("@as []", "[]") else ast.literal_eval(current)
if want not in paths:
    paths.append(want)
sys.stdout.write("[" + ", ".join(f"'{p}'" for p in paths) + "]\0")
PY
    elif command -v kwriteconfig6 >/dev/null 2>&1 || command -v kwriteconfig5 >/dev/null 2>&1; then
        kw=$(command -v kwriteconfig6 || command -v kwriteconfig5)
        "$kw" --file kglobalshortcutsrc --group services --group scour.desktop --key _launch "$kde_key" \
            && bound="$key (KDE — after the next login, or: kquitapp6 kglobalaccel)"
    fi
fi

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

  Start it with your session (optional):

      install -Dm644 packaging/scourd.service ~/.config/systemd/user/scourd.service
      systemctl --user enable --now scourd.service

  Watching, and why it is worth a password once:

      Without a filesystem mark there is no live watching: scourd reconciles by
      walking when a volume's write counter moves. Nothing is missed; changes
      take longer to appear. The mark needs root once — the system unit:

          sudo bash packaging/install-service.sh     # [--user NAME] [ROOT...]
          systemctl start scour.service

      Read that file first — it explains what the privilege is for and where it
      is dropped.

TXT
if [ -n "$bound" ]; then
    echo "  A key to open it: $bound. Change it in your desktop's keyboard settings;"
    echo "  the command is scour-gui, and pressing it again brings the window forward."
else
    echo "  A key to open it: bind any shortcut to scour-gui in your desktop's keyboard"
    echo "  settings — pressing it again brings the same window forward."
fi
cat <<'TXT'

  Uninstall: delete the files listed above. The index and settings live under
  ~/.local/share/scour and ~/.config/scour; `scour where` prints both.

TXT
