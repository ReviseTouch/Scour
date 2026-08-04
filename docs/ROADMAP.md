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
crate already cross-checks against the Windows target locally, so
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

## Phase 2.5 — The sink — **partly done** (`f758fa2`)

Measured 2026-08-04, and it reorders everything that follows. A bare parallel
`getdents64` walk reads this machine's 1.5 M-entry NTFS volume in **292 ms**
warm; with full `statx` metadata, 348 ms; through the `ignore` crate and a
channel — Scour's own walker shape — 593 ms. Scour end to end takes
**2,272–3,067 ms**.

So roughly **1.7 seconds is downstream of the walk**: building the index, not
reading the filesystem. That is the largest single number on the table and it
needs no platform-specific code, no new dependency and no privilege.

**Done so far.** Trigram extraction was a third of `build`; replacing its
`HashSet<u32>` with a bitmap over the 24-bit key space cut its share of a scan
from 1,010 ms to 616 ms. Building segments on background threads was tried,
measured at 11% and reverted — it broke the manifest and the hard-link rule,
and 11% against ±10% noise is not worth reworking `flush`, `sweep`,
`begin_generation` and `fold` for. Both are written up in `MEASUREMENTS.md`,
including the reasons.

**Still open.** Of the remaining `build` time: `dirs.intern` ~19 ms per 100k
rows, the sort ~18 ms, filling sixteen columns ~13 ms. And trigrams could be
built *after* the scan rather than during it, which is worth 616 ms — but that
number was 1,010 ms before the bitmap, so the case is weaker than it was.

`docs/ENUMERATION.md` is the whole survey — what every filesystem offers, what
each costs in privilege, and what was measured and rejected (io_uring is
*slower*; narrowing the `statx` mask buys nothing). Two things it found that
belong to other phases: btrfs `st_dev` is anonymous and changes across reboots,
so a persisted `EntryId` does not survive one — `stx_subvol` is the durable key
and this kernel has it; and the recorded reason for `watching 0` was wrong, so
`Caps::RECURSIVE_WATCH = false` on Linux rests on a diagnosis that does not
hold.

## Phase 3 — Ranking — **done** (`99b4bf9`, and the distance byte)

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

**The name half:** `SortKey::Relevance` scores a name against the query's
terms, and the rung order was corrected twice by measurement — ranking an exact
name highest filled the page with `.git/refs/heads/main`, and excluding `.git`
replaced it with a hundred `android/src/main` directories.

**The path half:** one byte a directory, written when the segment is written
and read by number at query time — 162 KB for 165,895 directories, and no path
is rebuilt to rank a row. It holds **how far the directory is from being
something the user wrote**: every component is a step and a hidden or
build-output component is three. Bounded so that it can only order rows the
name has already tied, which is what makes it safe to apply to every query.

Measured against the right metric, which took a correction: of the 902 `main`
matches under `~/Projeler`, 794 are under `target/`, `build/` or `.git/`, so
the "48 of the first 200 are the user's own work" figure this document used to
carry was counting generated files as authored ones. Of files actually written
by the user, the first two hundred results held **none**. They now hold the
whole first page. Numbers, refuted alternatives and the cost in
`MEASUREMENTS.md`.

**What ranking still has open**, none of it blocking: relevance walks every
matching row, so it costs 4–9 ms where the stored order costs a fraction of
that — fine for a keystroke today at 1.4 M entries, worth revisiting at ten
million. And the index format is now **4**; an older index is refused and
rescanned rather than ranked as if every file were equally close to home.

## Phase 4 — The taxonomy — **done**

Fourteen kinds, from eight. On this machine **more than half of every file used
to have no kind at all** — 58.50% unknown, against 11.51% now — and one line of
the table is most of that: `build`, which is 47.87% of what is indexed here.
Numbers, the rules that were rejected, and what is deliberately left unknown
are in `MEASUREMENTS.md`; the argument for each line is in `TAXONOMY.md`.

Four things came with it, each of which was a defect that would have surfaced
through the GUI first:

* **`from_name` returns a set.** `kind:media` has to keep matching rows in an
  index written before the split, and `kind:text` names four kinds at once.
  `Match::Kind` carries a list and the index compiles it to a bitset.
* **`Kind::token()` is not `Kind::msgid()`.** The facet rail builds a query out
  of a facet key, and `Executable` folds to a word the parser does not take —
  so clicking that one row searched for literal text. A label can be two words
  and can be translated; a token can be neither. The facet key is now the
  token, `FacetResponse::by` says so, and the frontends translate for display.
* **`FacetResponse::capped`.** `FACET_SCAN_CAP` truncated the scan silently.
  It shows immediately at this size: a kind facet over 1.4 M entries reads the
  first 200,000 and now says so.
* **Format 5**, which is the first bump where *nothing changed shape*. Every
  row of a version-4 index would still decode, into the wrong answer.

## Phase 5 — The window

**Slint.** One language, no webview, a small bundle, and the desktop app is
what this is for. A browser client stays possible and secondary — the service
already speaks a protocol over a socket, so a small HTTP bridge in front of it
would serve remote or in-browser use without the desktop app knowing.

`apps/scour-gui` is a **frontend**: it may name `scour-core`, `scour-proto`,
`scour-ipc`, `scour-config`, `scour-i18n`, exactly like `apps/scour`, and never
the engine or an index.

Pin exactly — `slint = "=1.16.1"`, `slint-build = "=1.16.1"`. A caret means
Cargo picks 1.17, whose winit backend divides by a refresh rate it reads as
zero (`frame_throttle.rs:58`) and crashes under Wine on every start.

| Stage | What | Rough size |
|---|---|---|
| 5.1 | Skeleton, two IPC connections, `search`, a plain 200-row table, the mockup's tokens, the meter line | ~600 lines |
| 5.2 | Facet rail from `facets`, the query line, sort headers, open/reveal, match highlighting, the catalogue via `@tr()` | ~800 lines |
| 5.3 | The list: `ListView`, LRU pages, generation tokens, skeleton rows, the addressable-window bound | ~250 lines |
| 5.4 | Hidden window, `--show`, single instance, tray, spawn-on-demand daemon | ~300 lines |

### The query line, which is the hard part

**Slint's `TextInput` has no range colouring.** One `color` for the whole
field; "the first three characters red" cannot be said. Upstream #9560 puts
editable text out of scope and 1.17 did not add it. Everything else about this
line follows from that one sentence.

The mockup is therefore not a template to copy — it uses real range colouring,
which is exactly what is unavailable. What carries over is the *behaviour*, and
the mockup is where it was worked out:

* **Chips plus a tail.** Completed terms are ordinary `Text` elements, so each
  can be many colours; the term being typed stays one `TextInput` painted by
  its *kind*. Rust computes the kind on every keystroke and hands the UI an
  `int` — colour logic in `.slint` would put the language's rules in two
  places.
* **The tail's colour must equal the chip's colour.** If they differ,
  committing a term changes how it looks and reads as "the colour arrives
  late". This was got wrong once in the source project and noticed instantly.
* **Term actions come free.** Hover, the `!` toggle and the `✕` are arithmetic
  in the mockup — the pointer's x divided by a character width — because there
  is no element per term. In Slint the terms *are* elements, so this is a
  `TouchArea` each.
* **Spans still come from the engine.** `explain` already returns them, and
  they are what decides a chip's kind. The frontend must not tokenise; that is
  a second parser, and the mockup measured what happens when two of them drift
  — 27 of 28 queries agreed, and the one that did not still looked coloured.

Four Slint traps, each of which compiles and then misbehaves:

* A conditional element (`if c.kind == 1: Text`) is **not** included in the
  parent's `spacing`; give the element its own `width: self.preferred-width +
  Npx` instead.
* `visible: false` still occupies layout. Empty the text instead.
* Referring to an outside element's `has-focus` **from inside a `for`** breaks
  keyboard input entirely — no error, the `TextInput` simply stops receiving
  keys. Feed an `in property <bool>` from Rust.
* `PointerEvent` has no `position`; use `self.absolute-position + self.mouse-x`.

If chips prove not to be enough — inline parse errors, a background badge per
operator, a query long enough to scroll — the other route is drawing the text
with `cosmic-text` and handing Slint an `image`. Measured cost in the source
project: ~1,150 lines plus a ~400-line bridge for keys, clipboard, focus and
DPI. Not first.

### The list

The addressable window is **ten thousand rows**, measured in phase 2.2. Beyond
it the UI says so and invites a narrower query, which is what a search tool
should encourage anyway. `ListView` is virtualised already, so the work is
paging: LRU pages, a generation token per query so a stale reply is dropped
rather than shown, and skeleton rows rather than the previous query's rows.

### Two connections, not one

`scour-ipc` is one call at a time with no cancellation, but `scourd` is
thread-per-connection and `Engine` is `Sync`. An interactive connection
(`search`, `facets`, `explain`) and a background one (`count`, `tree`, `stat`,
`status`) is what stops a 20 ms report query sitting in front of a keystroke.

### Highlighting

Match offsets are computed in Rust with `fold_indexed`, never in the UI: Turkish
folding changes byte lengths — `İ` is two bytes and folds to one — so offsets
found in folded text cannot be applied to the original. `scour-core` has the
function and its doc comment says this is what it is for. The same folding must
serve the filter, the highlight and the sort, or a user sees a row that matched
but was not highlighted and concludes the search is broken.

### The hotkey

A compositor binding on Linux (`bind = SUPER, SPACE, exec, scour-gui --show`)
with a single-instance guard, because a portal-less global hotkey is not
something an application can claim under Wayland. Windows and macOS register
one directly.

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

Two binaries in one package: `scourd` and `scour-gui`, plus the `scour` CLI.
Slint links its own renderer, so there is no webview runtime to depend on and
no `libwebkit2gtk` in `Depends:` — the Linux package needs the graphics stack
its backend uses and nothing more.

Two details that are cheap to get right and expensive to discover:

* **Nothing stops the daemon during an update.** On Windows a running
  `scourd.exe` holds a lock on its own image, so an installer that replaces it
  fails halfway. Stop it over the socket first, then replace, then start.
* **The GUI is not the service.** Packaging them as one unit is right;
  starting them as one is not, because the service outlives every window.

Daemon lifecycle differs per platform and each is a known pattern: systemd
socket activation on Linux (the gpg-agent shape, no postinst), `SMAppService`
on macOS with the plist inside the signed bundle, `HKCU\…\Run` on Windows —
with spawn-on-demand as the backstop everywhere, since each of those can be
disabled.

Signing costs real money and calendar time, whichever toolkit is underneath:
~$99/yr Apple plus ~$250/yr for a Windows OV certificate and a ~$130 token.
Azure's $9.99/mo alternative is not available
to individuals in Türkiye. **Skip EV** — Microsoft now states in
writing that paying the premium for SmartScreen reputation is no longer
justified.

---

## The shape of the running system

Settled by measurement over 2026-08-03/04, and worth stating in one place
because two different things were being called "the service".

### One process is certain: `scourd`, unprivileged

It holds the index, runs the watcher, answers queries. It does **not** need
root, and that is measured rather than assumed: running the whole-system walk
as root adds **1,064 files out of 2,129,212** — the rest of what root can see
is world-readable anyway, including all 58,285 files under `/usr/lib/modules`.

### One process is optional: a privileged scanner

Its justification is not scanning. Root buys a thousandth of the files and no
speed at all. It exists for two specific things:

* **btrfs `min_transid`** — "what changed since generation N" in **0.3 ms**
  against 10,883 ms for the whole tree. Needs `CAP_SYS_ADMIN`. This is the
  first real justification `Caps::JOURNAL` has had on Linux.
* **A spinning disk**, where a parallel walk is the wrong shape and reading
  the metadata tree sequentially is the right one. Not measured — there is no
  HDD here.

It takes **no commands**: no socket, no pipe, no verb. Its only input is a
root-owned list of roots; its only output is the index. See §7 of
`ENUMERATION.md`.

### How they talk: two answers, and one of them is "they do not"

**Clients ↔ `scourd`** — a local socket carrying NDJSON, which is what exists
today. Measured:

| reply | engine | transport |
|---|---|---|
| 200 rows | 2.47 ms | **0.27 ms** (73.6 KB) |
| 1000 rows | 5.65 ms | 0.80 ms (331.8 KB) |

Transport is a tenth of engine time, so gRPC would buy at most 0.1–0.2 ms and
cost `tonic`, `prost`, `tokio` and `hyper`. Not worth it. The CLI, the GUI, a
TUI and the MCP server are all clients of this one socket.

**Privileged scanner ↔ `scourd`** — **they do not talk.** The scanner writes
index files; `scourd` maps them. `mmap` *is* shared memory: the same physical
pages in both processes, no copy, no protocol, no attack surface. The single
constraint is the writer lock from `db9c95c` — one writer per directory —
so the scanner writes a **base layer** and `scourd` an **incremental layer**,
and a query merges the two. Segments already work exactly that way.

## Sources beyond the local disk

`trait Source` exists for this, and `Key::Opaque` exists because not every
source has an inode or a stable path. Each of these is one implementation
named only in `apps/scourd/src/wire.rs`:

| source | identity | notes |
|---|---|---|
| Docker container | container id + inode | no watch; the daemon has no inotify inside it |
| SSH / SFTP | `Key::Opaque` over the path | `Medium::Network` — 4 threads, 5 s debounce |
| FTP | `Key::PathHash` | no stable ids, no watch |
| S3 and object stores | `Key::Opaque(etag)` | a change token is a real `Caps::JOURNAL` |
| A virtual machine's disk | depends on how it is reached | mounted → ordinary `FsSource` |

The classification added in `f02279b` already serves these: a source declares
its `Medium` and gets the right thread count and debounce with no special
casing. What each still needs is its own `scan`, `watch` and `open`.

## A terminal client

The CLI already speaks every operation and has `--json` on all of them, so a
TUI is a third client of the same socket rather than a new subsystem. It shares
the query line's behaviour with the GUI — the roles, the two warning colours,
the completions — because those come from `explain` over the wire.

## The MCP surface is the point, not a side door

Worth stating because it changes what "good enough" means. A model asking
"where is the config that sets the socket path" is doing what `grep -r` does,
and paying for it: a recursive walk, every file opened, every byte read, and
a context window filled with false positives.

Scour already answers the path half of that instantly and over a protocol
designed for it — bounded replies that say when they were cut, `explain` so a
caller can check how its query was read, `scour://syntax` so it can learn the
language first. What is missing is the content half: `content:` parses today
and every index refuses it.

That is the case for document indexing, and it is a stronger one than "users
might want to search inside files". `trait Extractor` and the reserved schema
field were put there for it. The measurement that would decide it: how much of
a repository's text can be indexed, at what bytes per entry, against how long
`rg` takes on the same tree cold.

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
* The design mockup is a **behaviour** specification, not markup to port. It
  uses real range colouring, which is the one thing Slint's `TextInput` cannot
  do; what carries over is what the query line *does* — the roles, the two
  warning colours, the term actions, the completions. Its comments are English
  now (all 62 were Turkish); its visible text stays Turkish, because that half
  comes from `lang/tr` in the product.
