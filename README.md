# Scour

Instant file search, and a filesystem a language model can actually explore.

Type three characters and get an answer in about a millisecond, over an index
of everything. The same index answers `scour tree /some/huge/directory` in the
same millisecond, whether that directory holds ten files or a million — which
is what makes it usable from an assistant's context window.

Written in Rust, and nothing here is a wrapper around anything else.

**[revisetouch.com/scour](https://revisetouch.com/scour)** — what it looks like,
in all four faces, with the numbers beside them.
**[Documentation](https://revisetouch.com/en/docs/scour/introduction)** ·
**[Releases](https://github.com/hasantr/Scour/releases)**

Developed and used daily on Linux, against 4.6 million entries across an ext4
home and an NTFS volume.

**Windows: it runs.** The whole workspace builds for
`x86_64-pc-windows-msvc` — the Unix-only pieces are behind `cfg` now, and the
wastebasket says so rather than pretending — and the binaries were used on a
Windows desktop on 2026-09-08: the service indexed, the command line searched,
the query language answered. Not exhaustively tested, and three things are
known to be untried there: the window, live watching, and network or FAT32
volumes. There is no USN journal reader, so the first scan is a walk rather
than a journal read; searching is the same speed, the first scan is not.

**macOS: compiles, never run.** `x86_64-apple-darwin` and
`aarch64-apple-darwin` both pass `cargo check --workspace` under
`-D warnings`, and nothing else is claimed. A `cross` job in CI keeps all
three honest — compiling is not running, but it is the half that can be
checked from here.

```
$ scour "ext:rs size:>10kb dm:7d"
 40.18 KiB  2026-08-02  /home/u/Projeler/Scour/crates/scour-index-native/src/search.rs
 12.01 KiB  2026-08-02  /home/u/Projeler/Scour/crates/scour-config/src/schema.rs
…
40 / 43 · 1.74 ms
```

## Why this rather than `find`

`find` walks the filesystem every time you ask. Scour walks it once, keeps an
index, and watches for changes. The difference is not a constant factor:

| | 44,755 entries |
|---|---|
| `*.toml` (23 matches) | 0.74 ms |
| `ext:rs` (136 matches) | 1.09 ms |
| `kind:code dm:7d` (3,559 matches) | 2.18 ms |
| index on disk | 19.2 MiB |

Whole round trips — socket, parse, search, sort, count, and forty complete rows.
More, and the method, in [docs/MEASUREMENTS.md](docs/MEASUREMENTS.md).

The [September reliability and performance audit](docs/RELIABILITY-PERFORMANCE.md)
records the change-tracking recovery rules, bounded browser cache, regression
tests, measured costs and remaining limits. Event feeds are backed by full
reconciliation: by default every 30 minutes, or every minute when a source has
neither a complete watch nor a filesystem pulse. Expensive passes rest longer.
These are configurable under `[service]` as `reconcile_interval_secs` and
`poll_interval_secs`; the audit explains why they are not strict latency bounds.

Memory is the other half. The index is memory-mapped, so it lives in the page
cache and the kernel can reclaim it under pressure. Searching costs tens of
megabytes of resident memory rather than holding the whole index in RAM.

## Install

### From a release

Download the tarball from [Releases](https://github.com/hasantr/Scour/releases),
then:

```bash
tar xzf scour-0.1.0-linux-x86_64.tar.gz
cd scour-0.1.0-linux-x86_64
./install.sh
```

Nothing in that script asks for a password. It puts seven binaries in
`~/.local/bin`, a menu entry and an icon in `~/.local/share`, and prints what
to do next. To undo it, delete those files.

**Where the binaries run.** They are built against glibc 2.39 — Ubuntu 24.04 or
newer, Debian 13+, Fedora 40+, and any rolling distribution. Ubuntu 22.04 and
Debian 12 carry an older glibc and want the source route below.

**Tested on a clean Ubuntu 24.04.4 guest**, not merely compiled for it: the
installer ran, all seven binaries reported their version, the service indexed
the home directory, `scour bash` answered in 0.13 ms, and the browser face
served its page.

### From source

```bash
cargo build --release
./target/release/scourd &        # indexes your home directory on first run
./target/release/scour rapor     # search
./target/release/scour where     # where the settings and index live
```

A C compiler is used if one is present, for a single thing: pinning two libm
symbols to an older version so the window can be copied to a machine with an
older glibc than the one it was built on. Without a compiler the build still
succeeds and the result needs the glibc it was made on.

### Watching, and the one privilege

On Linux there is one watching mechanism and it is a `fanotify` mark: one per
volume, immediate, and costing nothing per directory. It needs `CAP_SYS_ADMIN`
to place, which `scourd` deliberately does not have — so a small helper places
the marks, hands the descriptor over, drops the privilege and execs `scourd`.

**There is no inotify fallback, on purpose.** inotify costs one watch per
directory out of a budget that belongs to your *session*, not to Scour. A large
home does not fit in it, and what runs out is not Scour: it is the budget the
next editor, file manager or language server needs, and they fail with an error
that never contains the word "watch". This machine lost the ability to open a
development tool that way, twice. Raising the limit does not fix it either —
the unprivileged alternative is narrower still: `fs.fanotify.max_user_marks` is
295,420 here against an inotify budget of 524,288, for ~609,000 directories.

Without the descriptor, `scourd` says so and reconciles by walking when a
source's pulse moves — a cheap counter read from the root's block device.
Nothing is missed; changes take longer to appear, and on a volume that is
written to constantly the repeated walks cost more IO than watching would have.

A root with no block device behind it — NFS, CIFS, sshfs, any FUSE mount,
tmpfs — has no pulse to read, so it is walked once and then not reconciled at
all. Those need the mark.

To have the mark placed at boot, install the system unit — the only part that
needs root:

```bash
cargo build --release -p scour-watch
sudo bash packaging/install-service.sh
systemctl start scour.service
```

The shipped unit targets `hasan` (UID 1000); adapt the unit, rule and installer
checks together for another account. Read them before installing. The installer
keeps `scour-watch` root-owned and grants only this service's start/stop/restart
to that account. Other administration still requires authentication. Its scope
uses [systemd's unit and verb details](https://github.com/systemd/systemd/blob/main/src/core/dbus-util.c)
and [polkit rules](https://polkit.pages.freedesktop.org/polkit/polkit.8.html).

### Settings

TOML, at `~/.config/scour/config.toml` (or the platform's equivalent). The
exclusion lists are the part worth editing — they are the first place to look
when something you expected is missing.

## Query language

Whitespace is AND, `|` is OR, `!` is NOT, quotes make a phrase, `*` and `?` are
wildcards, and `field:value` narrows.

```
rapor ext:pdf dm:30d          PDFs with "rapor" in the name, last 30 days
*.log size:>100mb             log files over 100 MB
under:/home/u/Projeler *.rs   Rust files anywhere in one project tree
path:src ext:rs !test         Rust files under src, excluding tests
kind:image dm:today           images touched today
```

`scour syntax` prints the full reference. Search is case-insensitive, and
Turkish `i`, `ı`, `I` and `İ` are treated as the same letter — `ISTANBUL`,
`İstanbul` and `ıstanbul` all find each other.

The parser never fails. A field it does not recognise is searched for as
literal text rather than rejected, which keeps a half-typed query usable — and
`scour explain "<query>"` reads back how a query was actually understood.

## For language models

`scour-mcp` is an MCP server. Print the block to paste into a client with:

```bash
scour mcp-config
```

Eight read-only tools. What makes them worth having is not `scour_search` — a
model can already run `find` — it is that **every answer is bounded**:

* `scour_tree` lists a directory of a million files as fast as one of ten, and
  says how many entries it left out rather than returning them.
* `scour_count` answers "how many Rust files are there" without listing any.
* `scour_facets` answers "what is in here" without reading anything.
* `scour_sources` says what is indexed, so "not found" can be told apart from
  "not looked at".

Rescanning, maintenance and shutdown exist in the protocol and are deliberately
not exposed: a model exploring a filesystem has no business rebuilding an index.

## How it is put together

One rule: **nothing but `scour-core` is depended on by more than one layer.**

```
        scour (CLI)      scour-mcp       [the GUI, later]
              └──────── scour-proto ────────┘
                             │
                        scour-ipc          local socket, NDJSON
                             │
                          scourd           ← the only place concrete types are named
                             │
                       scour-engine        Box<dyn Source>, Arc<dyn Index>
                             │
        ┌────────────── scour-core ──────────────┐
        │              types + traits            │
 scour-index-native   scour-source-fs  scour-config  scour-query  scour-i18n
```

`scour-core` holds the shared vocabulary and the interfaces and takes no
dependency beyond `serde` and `bitflags`. `scour-engine` is handed a
`Box<dyn Source>` and an `Arc<dyn Index>` and has no way of discovering that one
is a filesystem and the other an index — its manifest names neither. Replacing
the search engine is one line in `scourd`.

That is not architecture for its own sake. It is what made adding the MCP server
a matter of writing argument structs and a renderer, and it is what lets the
things below arrive without anything above noticing.

### The index

Every decision in it came out of a measurement, and the ones that look odd are
the ones that were measured hardest:

* **Documents are stored newest-first**, so the default view walks the postings
  and stops at the first page instead of visiting every match — 14.8 ms became
  0.022 ms in the prototype.
* **That order can only be rebuilt, never maintained.** The index is a *sorted
  body* plus an *unsorted tail*; the body terminates early, the tail is read in
  full, and `scour maintain rebuild` folds one into the other.
* **Displayed text lives in the document store, not in columns.** 0.32 µs a row
  against 14.33 µs. A column is right for what you sort on and wrong for what
  you only show.
* **Every ancestor directory is a token**, so deleting a subtree is one term:
  378,100 documents marked in 1.3 µs. It is also why `under:` and `parent:` are
  single posting lists rather than substring tests.
* **Counts stop.** With early termination, an exact total is the only work left
  that grows with the number of hits.

Results are checked against a brute-force reference that looks at every entry.
That comparison found three bugs that a timing benchmark would have called a
success, because all three returned a *fast wrong answer*.

## What is deliberately left open

Doors, with hinges already fitted:

| | today | behind it |
|---|---|---|
| Document contents | `Extractor` trait, `content:` in the language, a reserved schema field | PDF, DOCX and the rest, as cargo features |
| Cloud storage | `Source` trait, `SourceKind::Cloud` | S3, WebDAV, Google Drive |
| Platform fast paths | `Caps::JOURNAL`, declared and unclaimed | NTFS `$MFT` + USN journal, `fanotify`, `getattrlistbulk` |
| Another index | `Index` trait | in-memory, remote, hybrid |
| Mobile | the core is plain Rust; frontends are separate | a phone as a client of a desktop service |

None of these needs a change to anything above it. That is the whole point of
the shape.

## Languages

English is the source language: every comment, every identifier, every message
id. Translations are gettext catalogues in `lang/`, compiled into the binary,
keyed by the English text — so a missing entry falls back to correct English
rather than to a bare key.

```bash
SCOUR_LANG=tr scour status
```

Shipped: English, Turkish.

## Licence

MIT or Apache-2.0, at your option.
