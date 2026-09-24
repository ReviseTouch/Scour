# Scour

<img src="assets/scour.svg" width="72" alt="">

*Türkçe: [README.tr.md](README.tr.md)*

Scour is a file indexer and search tool for Linux. It keeps every file and
folder of the configured volumes in an index of its own, updates the index as
the filesystem changes, and answers searches from it in milliseconds. The same
index is exposed to language models through an MCP server. Scour is written in
Rust and depends on no external search engine or database.

**[revisetouch.com/scour](https://revisetouch.com/scour)** ·
**[Documentation](https://revisetouch.com/en/docs/scour/introduction)** ·
**[Releases](https://github.com/ReviseTouch/Scour/releases)**

> **Alpha release.** Scour is in daily use on one Linux machine, against an
> index of 4.8 million entries, and its measurements come from there. The
> Windows build has been started on one machine and is otherwise untested.
> macOS compiles and has not been run. The index format may change between
> releases.

```
$ scour "ext:rs size:>10kb dm:7d"
      Code    5.18 MiB  /home/u/Projeler/Scour/target/release/build/scour-gui/out/main.rs
      Code    20.1 KiB  /home/u/Projeler/Scour/apps/scour/src/main.rs
      Code    30.2 KiB  /home/u/Projeler/Scour/apps/scour/src/render.rs
      Code    24.9 KiB  /home/u/Projeler/ColpanRust/crates/repo_latches/tests/surface_line_budget.rs
      Code    13.8 KiB  /home/u/Projeler/ColpanRust/crates/command_catalog/tests/panels.rs
5 of 1705 in 3.27 ms (28000 rows) · -n 40 for more
```

At a terminal the command prints five rows; when piped, forty. `-n` sets the
number.

## Interfaces

One service (`scourd`) holds the index; four interfaces connect to it over a
local socket and share its settings. The screenshots below were taken at the
same time with the same query, `kind:code dm:7d size:>10kb`, on an index of
4.7 million entries.

**Command line** — `scour`

![Scour on the command line](docs/img/command-line.webp)

**Window** — `scour-gui`, a native Slint application

![The Scour window](docs/img/window.webp)

**Browser** — `scour-web`, a local bridge that serves one page

![Scour in a browser](docs/img/browser.webp)

**Terminal** — `scour-tui`, a full-screen terminal interface

![Scour in a terminal](docs/img/terminal.webp)

## Performance

`find` walks the filesystem on every query. Scour walks it once, keeps an index
and follows changes. On the live index (4.8 million entries, 493 MiB on disk),
measured as complete round trips — socket, parse, search, sort, count and
forty rows:

| query | ms |
|---|---:|
| `rapor` | 7.9 |
| `size:>10mb` | 7.5 |
| `kind:code dm:7d` | 16.7 |
| `kind:image` (1.6 million matches) | 37.3 |

The index is memory-mapped: it lives in the page cache and the kernel can
reclaim it under pressure, so searching costs tens of megabytes of resident
memory rather than the size of the index. Change notifications are not
treated as complete: a watched source also receives a full reconciliation
pass once a day, when the machine is quiet and with one thread; one without a
watch every 30 minutes, and every minute if it has no write counter either. The measurements and the commands that produced them are
in [docs/MEASUREMENTS.md](docs/MEASUREMENTS.md).

## Installation

### From a release

```bash
tar xzf scour-0.2.0-alpha.4-linux-x86_64.tar.gz
cd scour-0.2.0-alpha.4-linux-x86_64
./install.sh
```

The script does not require a password. It installs seven binaries into
`~/.local/bin`, a menu entry and the icon — scalable plus nine raster sizes,
for the panels that do not draw an SVG — into `~/.local/share`, and on GNOME
and KDE binds **Super+F** to Scour (`SCOUR_KEY=ctrl+alt+s` selects another
key, `SCOUR_KEY=none` skips the binding). At a terminal it then offers to start
Scour with your session as a systemd user unit and waits until the index
answers; `--yes` accepts, `--no-service` skips the offer, and a run without a
terminal never asks. Removing those files uninstalls it.

A face that finds no service running starts one. The window, the terminal
interface and the browser bridge look for `scourd` beside their own binary and
on `PATH`, start it detached with its output in `scourd.log` under the state
directory, and wait up to ten seconds for the socket. This needs no systemd.
`SCOUR_NO_AUTOSTART=1` turns it off; the command line never starts anything.

The release binaries are built in a Debian 11 container against glibc 2.31:
Debian 11, Ubuntu 22.04, RHEL 9 and everything later. The window also needs
`libfontconfig1`, which every desktop has. The package was installed and run
in clean Ubuntu 22.04, Ubuntu 24.04 and Debian 11 containers.

### Debian, Ubuntu and Fedora packages

```bash
sudo apt install ./scour_0.2.0.alpha.4-1_amd64.deb      # Debian 11 and later, Ubuntu 22.04 and later
sudo dnf install ./scour-0.2.0.alpha.4-1.x86_64.rpm     # Fedora
```

Both carry the seven binaries, six in `/usr/bin` and the privileged
`scour-watch` in `/usr/libexec/scour/`, the launchers, the menu entry, the
icon at four sizes and the user unit in `/usr/lib/systemd/user/`. Installing
starts nothing: `systemctl --user enable --now scourd.service` starts the
unprivileged service, and `/usr/share/doc/scour/` holds the system unit and
the polkit rule as examples with a note on what a package puts where. The X11
and Wayland libraries the window loads at run time are recommended, not
required, so `--no-install-recommends` gives the command line on a server.
Tested in Ubuntu 22.04, Ubuntu 24.04, Debian 12 and Fedora 40 containers.
`scripts/package` builds both from compiled binaries; it needs `cargo-deb`,
`cargo-generate-rpm` and `rsvg-convert`.

### Flatpak

```bash
flatpak install --user flathub org.flatpak.Builder
flatpak run org.flatpak.Builder --user --install --force-clean \
    build-dir packaging/flatpak/com.revisetouch.Scour.yml
flatpak run com.revisetouch.Scour
```

The manifest is in `packaging/flatpak/`; Scour is not on Flathub yet. A
sandboxed Scour indexes and searches the whole filesystem, and the window,
the browser page and the terminal interface work, but it cannot place a
`fanotify` mark, so nothing is watched live: the service finds a change when
the volume's write counter moves and on its periodic reconciliation pass.
Thumbnails need a program the runtime does not ship. The socket lives inside
the sandbox, so the command line and the MCP server are reached as
`flatpak run --command=scour com.revisetouch.Scour` and
`flatpak run --command=scour-mcp com.revisetouch.Scour`. Settings and index
live under `~/.var/app/com.revisetouch.Scour/`, apart from a tarball install.
`packaging/flatpak/README.md` has the rest.

**Windows.** The workspace builds for `x86_64-pc-windows-msvc`, and the
release includes a zip. The binaries were started once on one Windows machine:
the service indexed and the command line searched. The window, live watching,
network and FAT32 volumes have not been tested there, and there is no USN
journal reader, so the first scan walks the disk. **macOS** compiles and has
not been run. A CI job checks that all three targets compile.

### From source

Requirements: Rust 1.88 or later ([rustup](https://rustup.rs)), `pkg-config`,
and the fontconfig development headers, which only the window needs:

| distribution | packages |
|---|---|
| Ubuntu, Debian | `sudo apt install pkg-config libfontconfig1-dev` |
| Fedora | `sudo dnf install pkgconf fontconfig-devel` |
| Arch | `sudo pacman -S pkgconf fontconfig` |

```bash
git clone https://github.com/ReviseTouch/Scour.git
cd Scour
cargo build --release
scripts/release                      # assembles dist/scour-<version>-linux-x86_64.tar.gz
cd dist && tar xzf scour-*-linux-x86_64.tar.gz && cd scour-*-linux-x86_64 && ./install.sh
```

A clean build takes a few minutes. If a C compiler is present it is used to
pin two libm symbols so the window also runs on older glibc versions; without
one the build still completes. To install by hand instead of `install.sh`:
`install -m755 target/release/scour{,d,-gui,-tui,-web,-watch,-mcp} ~/.local/bin/`
(no menu entry and no shortcut in that case).

A binary's glibc floor is set by the machine that built it, so a tarball built
on a rolling distribution does not start on Ubuntu 22.04. `scripts/release-build`
builds the same seven binaries in a Debian 11 container (podman or docker) into
`target/container/release` and prints the floor each one asks for;
`scripts/release --container` packs those.

### Keyboard shortcut

Any combination can open Scour, and it is set from inside Scour: the window
and the browser page each have a "Keyboard shortcut" row, and at a terminal
`scour hotkey set super+f` does the same (`scour hotkey` shows the state,
`scour hotkey clear` removes it). Combinations are typed in one spelling:
`super+f`, `ctrl+alt+s`, `super+F2`. On GNOME the key works at once, on KDE
after the next login. On other desktops, and inside a Flatpak, Scour cannot
write the binding: it shows the command to bind by hand in the desktop's own
keyboard settings, `scour-open` or `flatpak run com.revisetouch.Scour`. The
installer offers Super+F (`SCOUR_KEY=ctrl+alt+s` picks another,
`SCOUR_KEY=none` skips it), and the window's first run offers it once. The key
runs the launcher, so it opens whichever face you last switched to; pressed
again while Scour is open, it brings the open window forward instead of
starting a second one.

Under GNOME on Wayland only the shell may bring a window forward, so Scour
ships a small Shell extension (`platform/gnome`) that does that one thing. The
installer and the packages put it in place, and setting the key turns it on;
GNOME loads a newly installed extension at the next login. Without it, or on
another desktop, a second press still opens Scour; the window may be flagged
in the taskbar instead of coming to the front.

### Watching the filesystem

On Linux, Scour watches with one `fanotify` mark per volume. The cost does not
depend on the number of directories. Placing a mark requires `CAP_SYS_ADMIN`;
`scourd` does not hold that capability. A separate helper, `scour-watch`,
places the marks, passes the descriptor on, drops the privilege and executes
`scourd`.

inotify is not used and there is no inotify fallback. inotify requires one
watch per directory from a budget shared by every program in the session;
exhausting it makes unrelated programs fail. Without a mark, `scourd`
reconciles a volume by walking it when the volume's write counter changes;
no change is lost, but changes appear later. Network and FUSE mounts have no
write counter and require the mark.

```bash
sudo bash packaging/install-service.sh [--user NAME] [ROOT...]   # the only step that requires root
systemctl start scour@<user>.service
```

The system unit is a template with one instance per account.
`scour@hasan.service` marks the filesystems named in `/etc/scour/hasan.conf`
(the roots given to the installer, default `/home`), drops to that account and
runs `scourd` as it, so an instance can only index what its account can read,
and its socket is in that account's own runtime directory. The account
defaults to the user who ran `sudo`. The installer places the helper under
root-owned `/usr/local/libexec/scour`, installs the template, and installs a
polkit rule that lets each account start, stop and restart its own instance
without authentication and nothing else. Run it again with `--user` to add
another person; an older single-user `scour.service` is migrated. Read both
files before installing. If the per-user unit is enabled for that account the
installer refuses until it is off: `systemctl --user disable --now
scourd.service`.

### Settings

Settings are in `~/.config/scour/config.toml`. The exclusion lists determine
what is left out of the index; consult them when an expected file is missing.

## Query language

Whitespace is AND, `|` is OR, `!` is NOT, quotes enclose a phrase, `*` and `?`
are wildcards, and `field:value` restricts a field.

```
rapor ext:pdf dm:30d          PDFs with "rapor" in the name, modified in the last 30 days
*.log size:>100mb             log files over 100 MB
under:~/Projeler *.rs         Rust files anywhere under one directory; ~ is your home
path:src ext:rs !test         Rust files under src, excluding tests
kind:image dm:today           images modified today
```

`scour syntax` prints the reference. Searching is case-insensitive, and the
Turkish letters `i`, `ı`, `I` and `İ` are treated as one. Parsing does not
fail: an unrecognised field is searched for as text, and `scour explain
"<query>"` shows how a query was read.

## Report

The report answers "where are the bytes" for a folder, and every face draws the
same picture from the same numbers: one bar for the scope's bytes by age with a
legend, one bar for where the bytes are, the heaviest folders and the rest, then
a row a folder with its share behind the name, its own age strip, size, share and
files; a ring for the kinds, six and the rest, whose legend opens the search with
`kind:`; and the largest files as bars. The window and the browser page draw them
in pixels, the terminal face in block cells, and at a terminal `scour du` and
`scour facets` draw the strip and the bars too, while into a pipe they print the
table they always did. The shares come from one crate, `scour-chart`, which
rounds so a legend adds up to a hundred; the page's JavaScript copy is checked
against it on every test run.

## Duplicates

The browser face's report tab lists files that share a size, largest saving
first: each group leads with what deleting all but one copy would free, and
the heading adds the groups up and says whether they were read and proved
identical or merely share a size. Nothing is deleted on a size match. One
copy in each group carries a keep mark, the newest by default; "Move the
other N to trash" is enabled only after "Confirm by reading" has proved that
group identical, asks once, and goes to the desktop's trash, never to an
unlink. Every path has the search list's own menu. Hard links count as
copies; trashing one frees nothing, and the page does not yet say so.

## MCP server

`scour-mcp` is a Model Context Protocol server with eleven read-only tools.
Every answer is bounded and states when it was truncated: `scour_tree` lists a
directory of a million entries as quickly as one of ten and reports how many
entries were omitted; `scour_count` returns a count without a listing;
`scour_facets` summarises a set of files by kind, extension or directory;
`scour_sources` reports which paths are indexed. Rescanning, maintenance and
shutdown exist in the protocol and are not exposed to the model.

`scour mcp-config` prints the configuration for a client and the file it
belongs in:

| client | command |
|---|---|
| Claude Desktop | `scour mcp-config` → `claude_desktop_config.json` |
| Claude Code | `scour mcp-config --for claude-code` → a `claude mcp add` command |
| Codex | `scour mcp-config --for codex` → `~/.codex/config.toml` |
| Cursor | `scour mcp-config --for cursor` → `~/.cursor/mcp.json` |
| Gemini CLI | `scour mcp-config --for gemini` → `~/.gemini/settings.json` |
| VS Code | `scour mcp-config --for vscode` → `.vscode/mcp.json` |

`scourd` must be running; the MCP server is a client of it like every other
interface.

## Architecture

Only `scour-core` — types and traits — is depended on by more than one layer.

```
   scour (CLI)   scour-mcp   scour-web   scour-gui   scour-tui
        └─────────────── scour-proto ───────────────┘
                              │
                          scour-ipc          local socket, NDJSON
                              │
                           scourd            the only place concrete types are named
                              │
                        scour-engine         Box<dyn Source>, Arc<dyn Index>
                              │
         ┌─────────────── scour-core ───────────────┐
         │               types + traits             │
  scour-index-native   scour-source-fs   scour-query   scour-config   scour-i18n
```

The engine receives a source and an index through traits and does not know
their concrete types; replacing the index implementation is a one-line change
in `scourd`. The index stores rows newest first, keeps displayed text outside
the sorted columns, and indexes every ancestor directory as a term, so
removing a subtree is a single operation. The test suite compares every query
shape against a brute-force reference implementation.

Details: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md); the file-kind taxonomy:
[docs/TAXONOMY.md](docs/TAXONOMY.md).

## Languages

English is the source language. Translations are gettext catalogues in
`lang/`, compiled into the binaries and keyed by the English text.
`SCOUR_LANG=tr scour status` selects a language for one run. Shipped: English,
Turkish.

## License

MIT or Apache-2.0, at your option.
