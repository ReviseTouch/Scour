# Scour

Instant file search, and a filesystem a language model can actually explore.

Type three characters and get an answer in a few milliseconds, over an index
of everything. The same index answers `scour tree /some/huge/directory` in the
same time whether that directory holds ten files or a million — which is what
makes it usable from an assistant's context window.

Written in Rust, and nothing here is a wrapper around anything else.

**[revisetouch.com/scour](https://revisetouch.com/scour)** ·
**[Documentation](https://revisetouch.com/en/docs/scour/introduction)** ·
**[Releases](https://github.com/ReviseTouch/Scour/releases)**

> **Alpha.** Used daily on Linux against 4.8 million entries and measured
> there; the Windows build has had one afternoon on one machine and macOS has
> never been run. The index format may still change.

```
$ scour "ext:rs size:>10kb dm:7d"
      Code    5.18 MiB  /home/u/Projeler/Scour/target/release/build/scour-gui/out/main.rs
      Code    20.1 KiB  /home/u/Projeler/Scour/apps/scour/src/main.rs
      Code    30.2 KiB  /home/u/Projeler/Scour/apps/scour/src/render.rs
      Code    24.9 KiB  /home/u/Projeler/ColpanRust/crates/repo_latches/tests/surface_line_budget.rs
      Code    13.8 KiB  /home/u/Projeler/ColpanRust/crates/command_catalog/tests/panels.rs
5 of 1705 in 3.27 ms (28000 rows) · -n 40 for more
```

Five rows at a terminal, forty into a pipe — a search that answers in
milliseconds is asked again, not scrolled. `-n` says how many.

## The four faces

One index, one service, one query. These were photographed together, running
`kind:code dm:7d size:>10kb` against the same 4.7 million entries.

**The command line** — `scour`.

![Scour on the command line](docs/img/command-line.webp)

**The window** — `scour-gui`, Slint, no browser inside it.

![The Scour window](docs/img/window.webp)

**The browser** — `scour-web` serves one page to a browser already open.

![Scour in a browser](docs/img/browser.webp)

**The terminal** — `scour-tui`, the same rails and the same colours.

![Scour in a terminal](docs/img/terminal.webp)

## Why this rather than `find`

`find` walks the filesystem every time you ask. Scour walks it once, keeps an
index, and watches for changes. On the live index — 4.8 million entries,
493 MiB on disk — whole round trips, socket to forty rows:

| | ms |
|---|---:|
| `rapor` | 7.9 |
| `size:>10mb` | 7.5 |
| `kind:code dm:7d` | 16.7 |
| `kind:image` (1.6 million matches) | 37.3 |

The index is memory-mapped, so it lives in the page cache and the kernel can
reclaim it; searching costs tens of megabytes resident, not the whole index.
Change feeds are hints, not proof: every source also gets a full reconciliation
pass, every 30 minutes by default and every minute where nothing watches or
pulses. Numbers, and how they were taken, in [docs/MEASUREMENTS.md](docs/MEASUREMENTS.md).

## Install

### From a release

```bash
tar xzf scour-0.2.0-alpha.1-linux-x86_64.tar.gz
cd scour-0.2.0-alpha.1-linux-x86_64
./install.sh
```

Nothing in it asks for a password: seven binaries into `~/.local/bin`, a menu
entry and an icon into `~/.local/share`. To undo it, delete those files. Built
against glibc 2.39 — Ubuntu 24.04+, Debian 13+, Fedora 40+, any rolling
distribution; older ones want the source route. Tested on a clean Ubuntu
24.04 guest, not merely compiled for it.

**Windows: it runs.** The workspace builds for `x86_64-pc-windows-msvc` and the
binaries were used on a Windows desktop: the service indexed, the command line
searched. Untried there: the window, live watching, network and FAT32 volumes.
No USN journal reader yet, so the first scan walks. **macOS: compiles, never
run.** A `cross` CI job keeps all three targets compiling.

### From source

```bash
cargo build --release
./target/release/scourd &        # indexes your home directory on first run
./target/release/scour rapor     # search
./target/release/scour where     # where the settings and index live
```

### A key to open it

Bind any key to `scour-gui`: the first press opens the window, the next one
brings the same window forward — a second copy is never started.

| desktop | where |
|---|---|
| GNOME | Settings → Keyboard → Keyboard Shortcuts → Custom Shortcuts → `+`, command `scour-gui` |
| KDE Plasma | System Settings → Shortcuts → Add Command… `scour-gui` (or right-click Scour in the menu → Edit Application → Application → Trigger) |
| anything else | your compositor's `bindsym`/`exec` line — `scour-gui`, nothing more |

Under Wayland, GNOME does not let a program raise its own window; the press
still works, but the window may blink in the taskbar instead of coming to the
front. `platform/gnome` holds a tiny Shell extension that fixes that, and
`scripts/install-desktop` installs it with a binding.

### Watching, and the one privilege

On Linux the watcher is one `fanotify` mark per volume: immediate, and free per
directory. Placing it needs `CAP_SYS_ADMIN`, which `scourd` does not have — a
small helper places the marks, hands over the descriptor, drops the privilege
and execs `scourd`. **There is no inotify fallback, on purpose**: inotify costs
one watch per directory out of a budget that belongs to your session, and what
runs out is the next editor's. Without the mark, `scourd` reconciles by walking
when a volume's write counter moves; nothing is missed, changes take longer to
appear. Network and FUSE mounts have no counter and need the mark.

```bash
sudo bash packaging/install-service.sh [--user NAME] [ROOT...]   # the only step that needs root
systemctl start scour.service
```

The user defaults to whoever ran `sudo`; the roots — the filesystems to mark —
default to `/home`. Read the unit and the polkit rule before installing: the
rule lets that one account start and stop this one service without a prompt,
and nothing else. Turn the user unit off first if you had one
(`systemctl --user disable --now scourd.service`).

### Settings

TOML at `~/.config/scour/config.toml`. The exclusion lists are the part worth
editing — the first place to look when something you expected is missing.

## Query language

Whitespace is AND, `|` is OR, `!` is NOT, quotes make a phrase, `*` and `?` are
wildcards, `field:value` narrows.

```
rapor ext:pdf dm:30d          PDFs with "rapor" in the name, last 30 days
*.log size:>100mb             log files over 100 MB
under:/home/u/Projeler *.rs   Rust files anywhere in one project tree
path:src ext:rs !test         Rust files under src, excluding tests
kind:image dm:today           images touched today
```

`scour syntax` prints the reference. Search is case-insensitive, and Turkish
`i`, `ı`, `I`, `İ` are one letter. The parser never fails: an unknown field is
searched for as text, and `scour explain "<query>"` reads back how a query was
understood.

## For language models

`scour-mcp` is an MCP server with eleven read-only tools, and **every answer
is bounded** and says when it was cut: `scour_tree` lists a directory of a
million files as fast as one of ten and says how many it left out;
`scour_count` answers "how many" without listing; `scour_facets` answers
"what is in here" without reading anything; `scour_sources` tells "not
found" from "not looked at". Rescan, maintenance and shutdown exist in the
protocol and are deliberately not exposed.

`scour mcp-config` prints the snippet for a client and says where it goes:

| client | |
|---|---|
| Claude Desktop | `scour mcp-config` → `claude_desktop_config.json` |
| Claude Code | `scour mcp-config --for claude-code` → one `claude mcp add` command |
| Codex | `scour mcp-config --for codex` → `~/.codex/config.toml` |
| Cursor | `scour mcp-config --for cursor` → `~/.cursor/mcp.json` |
| Gemini CLI | `scour mcp-config --for gemini` → `~/.gemini/settings.json` |
| VS Code | `scour mcp-config --for vscode` → `.vscode/mcp.json` |

`scourd` has to be running; the server is a client of it like every other face.

## How it is put together

One rule: **nothing but `scour-core` is depended on by more than one layer.**

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

The engine is handed a source and an index and cannot tell what either is;
replacing the search engine is one line in `scourd`. The index stores rows
newest first, keeps displayed text out of the sorted columns, and tokenises
every ancestor directory so deleting a subtree is one term. Every result is
checked against a brute-force reference in the tests.

Details: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), and the kind taxonomy in
[docs/TAXONOMY.md](docs/TAXONOMY.md).

## Languages

English is the source language; translations are gettext catalogues in
`lang/`, compiled in and keyed by the English text. `SCOUR_LANG=tr scour status`.
Shipped: English, Turkish.

## Licence

MIT or Apache-2.0, at your option.
