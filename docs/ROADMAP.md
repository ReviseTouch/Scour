# Roadmap

What is left, in the order it should be done, and why that order.

This is the synthesis of three investigations run on 2026-08-03 — a codebase
audit, a product-gap review, and a desktop-UI implementation study — plus the
two feature plans already written (`REPORTS.md`, `TAXONOMY.md`). Every number
below is either measured (`MEASUREMENTS.md`) or a file:line that was checked.

The engine is done. Nothing in this document is about making search faster;
1.2 M entries answer in 0.16–2.4 ms and the audit's verdict on the search code
was that it had nothing to add. What is left is everything around it:
durability, honesty of the ranking, the taxonomy, a window, and a way to ship.

---

## Phase 1 — Survive being interrupted — **done** (`db9c95c`)

**Why first:** every phase after this one multiplies the number of processes
touching the index directory. A GUI that spawns the daemon means two writers
where there was one, and the failure mode is not a bad answer — it is a
corrupted index and a re-scan of the disk.

| # | What | Where |
|---|---|---|
| 1.1 | **Directory lock.** Nothing stops two processes writing the same index. Both call `std::fs::write` on `seg-N.names` while the other has it **mmapped** — that is undefined behaviour, not a race that resolves badly. `Error::IndexBusy` is already defined and never constructed. A lock file with an exclusive `flock`/`LockFileEx` held for the writer's life, released by the OS on kill. | `scour-index-native/src/index.rs` |
| 1.2 | **Commit durability.** There is no `fsync` anywhere in the workspace, the manifest is written in place rather than written-and-renamed, and `drop_empty` unlinks segment files *before* `save_meta` records that they are gone. A kill in that window leaves a manifest naming files that do not exist, and the index does not open again. Order: write segments → fsync each → write `meta.json.tmp` → fsync → rename → fsync the directory. | `scour-index-native/src/index.rs` |
| 1.3 | **IPC frame limit.** `read_line` has no ceiling. 256 MB with no newline in it took the daemon from 19 MB to 282 MB of resident memory. A `take(MAX_LINE)` and a typed error. | `scour-ipc/src/lib.rs` |
| 1.4 | **`Rescan { path: "" }` is dropped.** The watcher emits it when the inotify watch limit is exceeded — precisely when the index is about to start drifting silently and permanently. An empty path must mean *all roots of this source*, not *no path matched*. | `scour-engine/src/engine.rs` |
| 1.5 | **`shutdown` does not shut down.** It replies and leaves the threads running. | `apps/scourd` |
| 1.6 | **Stale-index panic.** `staged_at` is trusted against a segment count that a concurrent maintain can change. | `scour-index-native/src/index.rs` |
| 1.7 | **Windows pipe name is machine-global.** Two users on one machine collide, and the second one to start talks to the first one's index. Include the user in the name. | `scour-ipc/src/lib.rs` |

All seven are done. Measured cost: **34% on a scan** (2,272 ms → 3,033 ms on
1.5 M entries), paid per commit rather than per entry, and one `flock` at open.
A second service on the same index refuses to start with the reason; `shutdown`
now ends the process. Numbers in `MEASUREMENTS.md`.

Two things fell out of doing it. `maintain rebuild` was reporting `0 B → 0 B in
0 ms` for work it had demonstrably done — the heavy levels are queued for the
worker, and their empty placeholder was being printed as if it were a
measurement; they answer `accepted` now. And `Error::IndexBusy` gained a
`detail`, because "the index is busy" without saying which index is not an
answer anyone can act on.

## Phase 2 — Make CI real, and measure the paging curve — **2.2 and 2.3 done** (`588810f`)

**2.1 — CI has never run.** The workflow exists, the repository has no remote,
and nothing has ever executed it. It would fail today. Every crate that does
not pull tantivy already cross-checks against the Windows target locally, so
the fix is small — but it has to actually run somewhere.

*This needs a decision:* a GitHub repository puts the code outside this
machine, so it is not something to do without asking. The alternative that
needs no approval is a local three-target `cargo check` in a script, which
catches the compile errors and none of the platform behaviour.

**2.2 — Measure deep-offset cost before designing a scrollbar around it.**
`search.rs:635` sets `need = offset + limit`, and then materialises all of
`kept` — building a front-coded path per match, which the code's own comment
calls the expensive part of the whole operation — before `.skip(offset)`.
Across segments, `index.rs:655` asks *each* segment for `offset: 0, limit:
need`. So the cost of page 250 is roughly `segments × (offset + limit)` paths
built and thrown away, and it is worst right after a large scan and best right
after `maintain rebuild`.

Measured. Linear in the offset and multiplied by the segment count: offset 0 is
0.54 ms and offset 200,000 is 225.07 ms on one segment, and at offset 10,000
sixteen segments cost 298.74 ms against 12.98 ms for one — 23×, more than the
segment count, because each segment builds its own full prefix. Table in
`MEASUREMENTS.md`.

**The addressable window is ten thousand rows, not the fifty thousand guessed
below.** A 60 fps frame is about 16 ms; after a rebuild that is around row
12,000 and after a large scan around row 1,000.

**2.3 — `rows_built` is on the wire.** It was computed in `Found` and dropped
before `SearchResponse`; it is the one number that makes 2.2 diagnosable from a
client rather than only from a benchmark, and the CLI prints it when it
dominates.

## Phase 3 — Ranking

The largest quality gap in the product, and the one thing a user notices
immediately without being able to name it. Today the orders are *modified*,
*name*, *size*, *path* — all of them total and none of them about the query.
Typing `main` puts a `main.rs` buried in a dependency's build directory above
the `main.rs` of the project open on screen.

What a relevance order has to weigh, in the order it matters: whole-name match
over substring, match at a word boundary over mid-word, shallower path over
deeper, shorter name over longer, and recency as the tie-break it already is.
None of this needs new columns — depth is derivable from the directory table,
and everything else is in the name arena.

Do it before the GUI. The GUI's first screenful *is* this ordering, and
building the list against the wrong default means rebuilding the impression it
makes.

## Phase 4 — The taxonomy

`Kind` has 8 variants; `TAXONOMY.md` designs 14 and the rail in the mockup
assumes them. This is a `scour-core` change plus extension tables, and it is
worth doing before the GUI's facet rail exists so that the rail is written
against `facets(query, by: kind)` from the first line and never against a
hardcoded list.

Two defects to fix in the same commit:

* **`Kind::Exec` does not round-trip.** `msgid()` returns `"Executable"`,
  which folds to `"executable"`, which `from_name` does not accept — it takes
  `exec`/`exe`/`bin`. A facet click on that one kind would produce a query term
  that parses as literal text. The other seven are fine; this is one arm of one
  match.
* **`FacetResponse` cannot say it was capped.** `FACET_SCAN_CAP = 200_000`
  truncates the scan silently. `SearchResponse` gets this right with `capped`;
  facets should too, or the rail lies at scale without a way to tell.

## Phase 5 — The window

Tauri 2.11, vanilla TypeScript, Vite. `apps/scour-gui` is a **frontend**: it
may name `scour-core`, `scour-proto`, `scour-ipc`, `scour-config`,
`scour-i18n`, exactly like `apps/scour`, and never the engine or an index.

Stage 0 is not code: `webkit2gtk-4.1` is not installed on this machine
(`webkitgtk-6.0` is the GTK4 port, which `wry` cannot use), so nothing builds
until it is.

| Stage | What | Rough size |
|---|---|---|
| 5.1 | Skeleton, two IPC lanes, one `search` command, a plain 200-row table, the mockup's tokens, the measurement line | ~600 lines |
| 5.2 | Facet rail from `facets`, query-line colouring, sort headers, open/reveal, Turkish-correct highlight offsets computed in Rust, the catalogue | ~700 lines |
| 5.3 | Virtual list: spacer windowing, LRU pages, generation tokens, skeleton rows, paged scroll remapping | ~250 lines |
| 5.4 | Hidden window, `--show`, single instance, tray, spawn-on-demand daemon | ~300 lines |

Three decisions worth recording because they are not obvious:

* **Two connections, not one.** `scour-ipc` is one-call-at-a-time with no
  cancellation, but `scourd` is thread-per-connection and `Engine` is `Sync`.
  An interactive lane (search, facets) and a background lane (count, tree,
  stat, status) means a 20 ms report query never sits in front of a keystroke.
* **Highlight offsets are computed in Rust.** `Hit` carries no match ranges,
  and a JavaScript `toLowerCase()` gets `İ`/`ı`/`I`/`i` wrong. `fold_indexed`
  exists for exactly this and its doc comment says so.
* **Filenames never touch `innerHTML`.** A file named `<img src=x onerror=…>`
  is trivial to create and the indexer will find it. Rows are cloned from a
  `<template>` and filled with `textContent`.

The scroll-height ceiling is 33.5 M pixels — about 1.1 M rows at 30 px — so a
1.2 M-row result already exceeds it and needs paged remapping rather than
linear scaling. But the real bound is 2.2's curve, and for v1 the honest answer
is a bounded addressable window with a line of text explaining it, not a
scrollbar that pretends row 800,000 is one drag away. Phase 2.2 measured where
that bound is: **ten thousand rows**.

On Linux the global hotkey is a compositor binding calling `scour-gui --show`,
intercepted by the single-instance plugin — not a workaround but the design.
`global-hotkey` 0.8 is X11-only and under Wayland it registers successfully and
then never fires. Windows and macOS keep the plugin, where it works.

## Phase 6 — Disk usage and the report tab

`REPORTS.md` §A and §C, in that order, because §C is mostly §A plus facets that
already exist. `trait Index` gains `usage()` defaulted to `Err(Unsupported)`,
`scour-index-native` implements the two passes the `rollup` example already
demonstrates (measured: ~45 ms for a 1.2 M-entry disk), the CLI gains `scour
du`, and the MCP server gains `scour_disk_usage`.

That last one is the strongest argument for the whole phase. *"What is eating
my disk"* is a question an assistant is asked constantly and currently cannot
answer without walking a filesystem it should not be walking.

Wire change → `AGENTS.md` in the same commit.

A reduced report — children by file count from `FacetBy::Dir`, biggest files by
size — is possible before this and needs no backend work at all. Bytes and the
age distribution are not.

## Phase 7 — Shipping

Bundles for `.deb`/`.rpm`/AppImage, NSIS on Windows, `.app`/`.dmg` on macOS.
Three details that are cheap to get right and expensive to discover:

* `bundle.linux.deb.depends` **has no default** — unset, the package installs
  and then fails to start.
* `scourd` ships as an ordinary packaged binary, not `externalBin`, which
  breaks macOS notarisation ([tauri#11992](https://github.com/tauri-apps/tauri/issues/11992), open).
* Nothing in the updater stops the daemon, and on Windows a running `scourd.exe`
  holds a lock on its own image. Stop it over the socket first.

Daemon lifecycle differs per platform and each is a known pattern: systemd
socket activation on Linux (the gpg-agent shape, no postinst), `SMAppService`
on macOS with the plist inside the signed bundle, `HKCU\…\Run` on Windows —
with spawn-on-demand as the backstop everywhere, since each of those can be
disabled.

Signing costs real money and calendar time: ~$99/yr Apple plus ~$250/yr for a
Windows OV certificate and a ~$130 token. Azure's $9.99/mo alternative is not
available to individuals in Türkiye. **Skip EV** — Microsoft now states in
writing that paying the premium for SmartScreen reputation is no longer
justified.

---

## Deliberately later

* **Duplicate detection by content** (`REPORTS.md` §B tier four). The first
  three tiers — same inode, same name, same size — are free and can come with
  Phase 6. A digest column is a maintenance job and should wait until the free
  tiers prove anyone wants the feature.
* **Regex, `case:`, `ww:`, query grouping with parentheses.** Real gaps against
  Everything, none of them blocking.
* **Windows file attributes** (hidden, system, archive) as query terms.
* **A time facet** for the mockup's age ribbon. The interim is six `count`
  calls with `dm:` ranges on the background lane, ~2–15 ms at measured rates.
* **History** — "what grew since last week" needs two snapshots and the index
  holds one. A daily rollup job, a few hundred kilobytes a day.
* **Document content.** The door is open — `trait Extractor`, a reserved
  schema field — and nothing walks through it yet.

## Noted, not scheduled

Small truths found while reading, none of them worth a commit of their own;
each belongs to whichever phase next touches its file.

* Six configuration keys are parsed and read by nothing: `scan.fast` (the
  documentation claims it is the difference between one minute and ten),
  `ui.language`, `ui.columns`, `service.maintain_every_hours`,
  `content.max_file_mb`.
* `/tmp` is in the Linux platform exclusions and cannot be removed, so a source
  rooted there indexes nothing and says nothing.
* `scour_core::text::upper_tr` is defined, tested, and called from nowhere —
  and the GUI needs it, because CSS `text-transform: uppercase` spells
  "DEĞIŞTIRME".
* `scour-i18n/src/lib.rs:10` says the UI uses Slint's `@tr()`. It will not.
* `scour_query::describe` builds English with `format!`, so its output is not a
  lookupable msgid despite the doc comment saying it is. Explain will be
  English-only until that changes.
* The design mockup's comments were in Turkish, unlike everything in this
  repository. Translated — all 62 of them — so that the parts of it which
  become `apps/scour-gui` arrive in the source language rather than needing a
  pass afterwards. Its *visible* text stays Turkish: that is the user-facing
  half, and in the product it comes from `lang/tr`.
