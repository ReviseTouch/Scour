# Scour

Instant file search, and a filesystem a language model can actually explore.

Type three characters and get an answer in about a millisecond, over an index
of everything. The same index answers `scour tree /some/huge/directory` in the
same millisecond, whether that directory holds ten files or a million — which
is what makes it usable from an assistant's context window.

Written in Rust, and nothing here is a wrapper around anything else.

Developed and used daily on Linux, against 4.6 million entries across an ext4
home and an NTFS volume. It is written for Windows and macOS too and compiles
for both, but it has not been run on either — and compiling is not running:
every fault found in a day of measuring this on Linux was one a compiler
cannot see.

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
the home directory and watched it with inotify, `scour bash` answered in
0.13 ms, and the browser face served its page.

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

`scourd` watches with inotify by default — one watch per directory, from a
budget shared with everything else in your session. A large home does not fit
in it, and rather than take the whole budget and break the next program that
wants a watch, `scourd` says so and reconciles by walking instead. Nothing is
missed; changes take longer to appear.

For a filesystem-wide mark instead — one `fanotify` mark per volume, immediate
and nearly free — install the system unit, which is the only part that needs
root:

```bash
sudo install -m644 packaging/scour.service /etc/systemd/system/scour.service
sudo systemctl daemon-reload
sudo systemctl enable --now scour.service
```

Read that file before running it: it explains what the privilege is for and
where `scour-watch` drops it.

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
