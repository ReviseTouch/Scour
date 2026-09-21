#!/bin/sh
# Put Scour where the desktop can find it, without asking for a password.
#
# **Everything here is unprivileged and reversible.** Binaries go to
# `~/.local/bin`, the menu entry and its icons to `~/.local/share`, and the
# user unit — if you say yes — to `~/.config/systemd/user`. The one thing that
# needs root — the fanotify mark that makes watching a whole filesystem cheap
# — is printed at the end rather than done, because a script that asks for a
# password is a script nobody should run without reading, and Scour works
# without it.
#
#   ./install.sh              install, and offer to start it with your session
#   ./install.sh --yes        ...and take yes for an answer
#   ./install.sh --no-service install the files only; print how to start it
#
# With no terminal on standard input nothing is asked: the files are
# installed and the hint is printed, which is what a pipe or a package
# postinst wants.
set -eu

service=ask
for a in "$@"; do
    case "$a" in
        --no-service) service=no ;;
        -y|--yes) service=yes ;;
        -h|--help) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "install.sh: unknown option $a" >&2; exit 2 ;;
    esac
done
if [ "$service" = ask ] && [ ! -t 0 ]; then service=no; fi

here=$(cd "$(dirname "$0")" && pwd)
bin=${PREFIX:-$HOME/.local}/bin
share=${PREFIX:-$HOME/.local}/share
icons=$share/icons/hicolor

mkdir -p "$bin" "$share/applications" "$icons/scalable/apps"

for b in scour scourd scour-gui scour-tui scour-web scour-watch scour-mcp; do
    [ -f "$here/bin/$b" ] || continue
    install -m755 "$here/bin/$b" "$bin/$b"
    echo "  $bin/$b"
done
install -m755 "$here/scripts/scour-open" "$bin/scour-open"
install -m755 "$here/scripts/scour-app" "$bin/scour-app"
install -m644 "$here/packaging/scour.desktop" "$share/applications/scour.desktop"
install -m644 "$here/assets/scour.svg" "$icons/scalable/apps/scour.svg"
# The raster sizes as well: GNOME's shell reads the SVG, but KDE's task
# manager, XFCE, LXQt and the older docks look for a PNG at the size they
# draw and show nothing at all when there is none.
for png in "$here"/assets/icons/hicolor/*/apps/scour.png; do
    [ -f "$png" ] || continue
    size=$(basename "$(dirname "$(dirname "$png")")")
    mkdir -p "$icons/$size/apps"
    install -m644 "$png" "$icons/$size/apps/scour.png"
done
update-desktop-database "$share/applications" 2>/dev/null || true
gtk-update-icon-cache -qtf "$icons" 2>/dev/null || true

# A key to open it, on the desktops that let a program set one. Super+F unless
# `SCOUR_KEY` says otherwise; `SCOUR_KEY=none` skips this. Pressing it again
# opens the face last switched to, and for the window brings it forward: scour-open.
#
# The command line does it: `scour hotkey` knows GNOME's dconf entry and KDE's
# kglobalshortcutsrc, and this script no longer has a second opinion about
# either. `--json` because the words are translated and the keys are not.
key=${SCOUR_KEY:-super+f}
bound=""
if [ "$key" != none ]; then
    # Never fatal. A container has no desktop to bind on, and under `set -e` a
    # failure anywhere but an `if` condition would end the install here.
    if said=$("$bin/scour" --json hotkey set "$key" 2>/dev/null); then
        where=$(printf '%s' "$said" | sed -n 's/.*"desktop": *"\([^"]*\)".*/\1/p')
        # The JSON spells them in lower case; these two are names.
        case "$where" in
            gnome) where=GNOME ;;
            kde) where=KDE ;;
        esac
        bound="$key (${where:-this desktop})"
        case "$said" in
            *'"applies": "after-login"'*)
                bound="$bound — after the next login, or: kquitapp6 kglobalaccel" ;;
        esac
    fi
fi

# --- start it with the session -------------------------------------------
#
# Never when a *system* unit is already running: two writers race for the
# index lock and the loser exits in a restart loop.
units=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
system_unit=""
user_systemd=no
if command -v systemctl >/dev/null 2>&1; then
    if systemctl --user show-environment >/dev/null 2>&1; then user_systemd=yes; fi
    if systemctl is-active --quiet scour.service 2>/dev/null; then
        system_unit=scour.service
    else
        system_unit=$(systemctl list-units --state=active --no-legend 'scour@*.service' 2>/dev/null | awk '{print $1; exit}')
    fi
fi

started=skipped
if [ "$user_systemd" = yes ] && [ -z "$system_unit" ] && [ "$service" != no ]; then
    answer=y
    if [ "$service" = ask ]; then
        echo
        printf '  Start Scour with your session now (a systemd user service)? [Y/n] '
        if ! read -r answer; then answer=y; fi
        [ -n "$answer" ] || answer=y
    fi
    case "$answer" in
        [Nn]*) ;;
        *)
            install -Dm644 "$here/packaging/scourd.service" "$units/scourd.service"
            systemctl --user daemon-reload
            if systemctl --user enable --now scourd.service; then started=waiting; fi
            ;;
    esac
fi
# Enabled is not running, and running is not answering. Ask the socket.
if [ "$started" = waiting ]; then
    started=late
    n=0
    while [ "$n" -lt 10 ]; do
        if "$bin/scour" status >/dev/null 2>&1; then started=yes; break; fi
        sleep 1
        n=$((n + 1))
    done
fi

echo
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "  NOTE: $bin is not on your PATH. Add it to your shell's profile." ;;
esac

cat <<'TXT'

  Use it:

      scour rapor         # search from the terminal
      scour-gui           # the window   (also in the application menu)
      scour-tui           # the terminal face
      scour-web           # opens in a browser
TXT

case "$started" in
    yes) echo
         echo "  scourd is running and indexing your home directory now." ;;
    late) echo
          echo "  scourd was enabled but did not answer within 10 s — it may still be"
          echo "  starting. Look: journalctl --user -u scourd.service -e" ;;
    *) if [ -n "$system_unit" ]; then
           echo
           echo "  $system_unit is already running; nothing to start."
       else
           cat <<'TXT'

      scourd &            # indexes your home directory on first run

  Start it with your session (optional):

      install -Dm644 packaging/scourd.service ~/.config/systemd/user/scourd.service
      systemctl --user enable --now scourd.service
TXT
       fi ;;
esac

cat <<'TXT'

  Watching, and why it is worth a password once:

      Without a filesystem mark there is no live watching: scourd reconciles by
      walking when a volume's write counter moves. Nothing is missed; changes
      take longer to appear. The mark needs root once — the system unit:

          sudo bash packaging/install-service.sh     # [--user NAME] [ROOT...]

      It prints the unit it installed and how to start it. Read that file
      first — it explains what the privilege is for and where it is dropped.
      Install one or the other, never both.

TXT
if [ -n "$bound" ]; then
    echo "  A key to open it: $bound. Change it in your desktop's keyboard settings;"
    echo "  the command is scour-open: it opens the face you last switched to."
else
    echo "  A key to open it: bind any shortcut to scour-open in your desktop's keyboard"
    echo "  settings — pressing it again brings the same window forward."
fi
cat <<'TXT'

  Uninstall: delete the files listed above. The index and settings live under
  ~/.local/share/scour and ~/.config/scour; `scour where` prints both.

TXT
