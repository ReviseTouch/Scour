# Measurements

Numbers, with the command that produced them. A claim without one of these is
an opinion.

## 2026-08-15 — the path order, stored

Ordering by path was the last expensive order: **291 ms** against 0.6 for the
stored date order, on a page of two hundred out of 2,235,402 rows. Two rounds of
work on the *key* had already landed and the key was no longer the cost. What
was left had been isolated by stubbing the pieces out in turn:

| | ms |
|---|---|
| the walk alone | 31 |
| + reading each row's spelled name | 102 |
| + resolving that row's directory | 202 |
| + joining and comparing | free |

Two reads a **match**, and no cleverness in the key removes them — an
abbreviated key was written twice and refused twice, for reasons kept at
`search::key_is_exact`.

So the order is stored instead of derived: `seg-*.porder`, four bytes a row,
listing the rows in ascending path order. A page is then the first `need`
positions that are live and match, which is the bargain the row numbering
already makes for a date.

### The instrument

A copy of the live index, rebuilt into one segment so that both binaries could
be pointed at exactly the same bytes — the old one ignores a file it does not
know about, so the comparison is one index and two builds rather than two
indexes.

```bash
cp -a ~/.local/share/scour/index /var/tmp/idx && rm -f /var/tmp/idx/native/index.lock
cargo run --release -p scour-index-native --example compact_cost -- /var/tmp/idx/native rebuild
cargo run --release -p scour-index-native --example searchcost -- /var/tmp/idx/native
```

Least of three per cell, binaries alternated, two rounds each.

### What it bought

Whole table, page of two hundred, `path ↓`:

| offset | before | after |
|---|---|---|
| 0 | 284.0 / 280.8 ms | **0.9 / 1.2** |
| 2 000 | 302.5 / 298.0 | **1.7 / 1.4** |
| 19 800 | 319.7 / 309.1 | **7.4 / 7.4** |

Which is the date order's own cost: `modified ↓` measures 0.6–0.9 at offset 0 on
the same runs. Every other order is unchanged — `modified` 0.6, `name` 49,
`size` 2.8, `created` 2.1, `kind` 2.2, `relevance` 0.6, each within the noise of
its own baseline.

**Peak resident size fell with it: 184 MB to 128.** Not a side effect worth
being surprised by — the old path sort built a key per match, and a path key
owns a string, so the benchmark was allocating and discarding 2,235,402 of them.

Filtered queries, same index, `path ↓` at offset 0:

| query | before | after | |
|---|---|---|---|
| `depth:>3` | 365.5 ms | **17.4** | streams |
| `kind:code` | 79.3 | **11.6** | streams |
| `depth:>40` (matches nothing) | 68.9 | **57.1** | streams; the worst case |
| `rapor` | 20.0 | 20.1 | falls back — a text query walks the folded arena |
| `ext:rs` | 3.7 | 4.3 | falls back |

`depth:>40` is the case worth having measured: it narrows no block and matches
nothing, so the whole order is read to find that out, and its positions are
scattered through the segment where the ordinary walk is sequential. It is
still not a regression, because the ordinary walk was reading names it no
longer has to.

### What it cost

**Disk: 8,941,612 bytes on this index — 4.000 bytes a row, 4.60% of the
segment.** The estimate before it was written was ~9 MB and it is exactly that,
because the file is one `u32` a row and nothing else.

**A rebuild: 8.7 s wall, 8.5 s CPU, 433 MiB anonymous peak**, for 2,235,402 rows
folded into one segment. The ordering is a sort of `u32` row numbers against a
comparator that reads the flattened directory prefixes and the name arena, both
of which are already in memory at that moment in the build; the directory table
is decoded once for the whole sort rather than once per comparison, which is
what `DirTable::join_prefixes` exists for.

### The format

The path order is the one part of a segment that may be **absent**, and the
version was deliberately not bumped. An index written before this exists is
read exactly as it was — a search sorted by path builds its keys the way it
always did — and gains the file the next time each segment is folded. The
alternative was `Error::IndexOutdated`, a discard, and a rescan.

What that would have cost is not measured here, and deliberately: pricing it
means walking the owner's disks, one of which is NTFS. The reference point is
the 2026-08-03 run below — **a first scan of 1,197,514 entries on `/home/hasan`,
NVMe, at 6.7 s wall**. This index is 2,235,402 entries across two volumes, the
second of them ntfs3, where a walk costs more per entry than either half of that
comparison. Folding the existing index instead reads no filesystem at all and is
the 8.7 s above — and, unlike a rescan, it is not a period with an empty search
box in front of it.

A file that is *present and does not describe the segment* is refused as
damage, on the same argument the live bitmap is: both are written whole, so a
length that says otherwise is a truncated write. Tested both ways in
`whole.rs`.

### Measured and rejected

**A rank column** — the inverse of this, a row's *position* in path order, as
another `Field` with a zone map. It fits the existing machinery exactly and
needs no new file. It was rejected on the arithmetic rather than by measurement:
a block holds thirty-two rows adjacent in *date* order, so their positions in
path order are thirty-two numbers scattered across the whole corpus, every
block's recorded range is nearly the whole range, and a range that admits
everything skips nothing. It would have turned a built key into a column read —
worth something, roughly the 31 ms of walk plus a read — and left the walk
visiting all 2.2 M rows. The stored order visits two hundred.

## 2026-08-09 — where the idle worker's time goes, and what a wider window would buy

The watcher was measured at 0.029% of a core and closed. What was left was
`scour-worker` at **0.150%**, and the proposal on the table was to widen
fanotify's 200 ms collection window so that repeated writes to one file
deduplicate harder. It was measured before it was written, and the measurement
declined it: the window is not where the worker's time goes.

### The instrument

The live service could not be used — it had exited, and the machine stopped
being idle part-way through the night — so the worker was measured against a
**reflinked copy of the live index** (2,217,125 entries, 11 segments, 183 MiB)
under a second `scourd` with its own socket, its own index directory and
synthetic roots under `/var/tmp`, where nothing but the harness writes. Load is
generated at a chosen rate, so the number of events is known rather than
guessed at. `scan.on_start = false`, or the first walk would sweep a copied
index that no walk had stamped.

```bash
scourd --config <bench.toml>                     # its own socket and index
awk '{print $14+$15}' /proc/<pid>/task/<tid>/stat  # jiffies, both ends
```

### The worker with nothing to do costs nothing

240 s, no events at all, watching two empty roots:

| | jiffies | share of a core |
|---|---|---|
| `scour-worker`, idle | 1 in 240 s | **0.0042%** |

So none of the 0.150% is the loop's own pulse. All of it is work it was asked
to do, and the question is which work.

### The shape of it: a floor, a commit clock, and a cost per wake-up

Same index, 180 s a run, distinct paths so that nothing deduplicates:

| upserts a second | `scour-worker` | worker wake-ups |
|---|---|---|
| 0 | 0.0111% | 0.5/s |
| 3 | 0.0556% | 8.0/s |
| 15 | 0.1278% | 20.2/s |

Three points on a line: **0.006% floor + a fixed commit term + 60 µs a
wake-up.** The 60 µs is per *wake*, not per row — `apply` was measured
separately at **0.45 µs a row** (0.003 ms for one, 0.057 ms for 128), and a
commit costs the same whatever it holds:

```
commit_cost <index copy> 0 <n paths already in the index>
```

| rows in the commit | apply, CPU | commit, CPU |
|---|---|---|
| 1 | 0.003 ms | 22.5 ms |
| 8 | 0.007 ms | 23.8 ms |
| 32 | 0.019 ms | 27.3 ms |
| 128 | 0.057 ms | 23.7 ms |

Flat, because a commit is about ten `fsync`s — seven segment parts, the alive
bitmap and the manifest — and a two-row segment pays all of them. (The 22 ms
here is a cold copy; in the long-lived service the same commit settled at about
3.4 ms, see below.)

### The commit clock is the largest thing the worker does

`commit_idle` is fifteen seconds, so anything at all changing keeps the service
writing a segment four times a minute. Alternating runs, 3 upserts a second,
180 s each, changing only `service.commit_interval_ms` — which raises the floor
above `commit_idle` and so decides the cadence:

| commit every | run 1 | run 2 |
|---|---|---|
| 15 s | 0.0500% | 0.0556% |
| 60 s | **0.0222%** | **0.0222%** |

**0.031% of a core, and the arms do not overlap.** Nine fewer commits in 180 s
is 3.4 ms a commit. At three upserts a second — roughly what this desktop
produces through a 200 ms window — the worker's 0.053% is 0.006% floor, 0.018%
events and **0.031% commit clock**.

### What a wider window would actually buy

One 300 s trace of the real home directory, replayed through every candidate
window rather than run several times, because a desktop's churn differs more
between two minutes than the windows differ between themselves:

```bash
cargo run --release --example window -- 300 /home/you
```

**4,474 events, 177 distinct paths.**

| window | windows | looks | looks/s | deduplicated | largest batch |
|---|---|---|---|---|---|
| **200 ms** | 428 | 869 | 2.90 | **80.6%** | 228 |
| 1 s | 291 | 849 | 2.83 | 81.0% | 236 |
| 5 s | 63 | 563 | 1.88 | 87.4% | 295 |
| 15 s | 22 | 407 | 1.36 | 90.9% | 443 |
| 30 s | 11 | 324 | 1.08 | 92.8% | 646 |

**Two hundred milliseconds already removes four fifths of the duplication.**
The whole of the remaining headroom is 1.54 looks a second, which at 60 µs a
wake-up is **0.009% of a core on the worker** — a third of a jiffy per minute,
under the resolution of the measurement that would have to confirm it. The
loudest single path, `cookies.sqlite-wal`, was written 2,002 times in 300 s and
collapses to one look per window at any width; what a wider window adds is only
the collapsing of *windows*, and there are already only 1.43 of them a second.

The saving that is real is on the reader, not the worker: this module's own
table has 200 ms at 5 wakes a second and 0.030%, and 1000 ms at 1 wake and
0.014%. That is where a wider window pays.

**And it cannot be a fixed number.** The same table reverses at high load —
1000 ms costs *more* than 500 ms at 2,120 events a second, because the batch
reaches 2,600 events and stops fitting in cache. Fifteen seconds at that rate
is 31,800 events, about 1.9 MiB of `Seen`. So the window has to be short when
somebody is waiting or the machine is busy and long only when neither is true,
which is the same rule `commit_watched` and `commit_idle` already implement one
layer up. The watcher cannot read that rule today: `scour-source-fs` may not
depend on the engine. The smallest thing that would let it is a defaulted
method on `WatchHandle` — `fn attention(&self, waiting: bool) {}` — called by
the engine when its watcher count crosses zero. That is a trait in core, not a
dependency between implementations.

### Measured and rejected

* **`SCOUR_WAKE_TRACE=1` costs nothing.** The owner's service was running with
  it set, which made it a suspect. Alternating runs, 3 upserts a second, 180 s:
  off 0.0556% / 0.0444%, on 0.0556% / 0.0500%. The difference is smaller than
  one jiffy in 180 s.
* **Deleting is not more expensive than writing.** A create-and-delete load at
  3 cycles a second produced two events a cycle and cost 0.0722%, against
  0.0736% predicted by the line above for six events a second. `hidden_prefixes`
  and the sweep it triggers cost nothing measurable at this scale.
* **Measuring against the real home directory was abandoned.** Four alternating
  180 s runs gave 3.39% / 1.11% / 1.30% / 1.65% — the spread inside one arm is
  larger than the difference between the arms, because the machine was not idle
  (load 2.3; a browser, two editors). A desktop's churn is not stationary and
  cannot be the control variable.

### Still open

The bench accounts for 0.045–0.055% of a worker at this desktop's event rate,
against the 0.150% measured on the live service. The gap is not explained. It
was measured under fanotify with a second source on ntfs3, and neither could be
reproduced here: the marks need `CAP_SYS_ADMIN`, and this session had no way to
place them.

## 2026-08-07 — what an open window costs, and why it was a share of the machine

An idle service with a search window open sat at **8.59% of a core** against
0.108% with the window shut, same binary, nothing compiling. Two causes, and
the second is the interesting one.

**The empty query's count was walked.** `/api/count` with no query took
**1.233 s** across 2,094,185 rows to arrive at a number every segment already
keeps as its live-row count. A window opens showing the empty query, so this
was part of opening one. Short-circuited in `NativeIndex::search`: no
conditions and nothing hidden means every live row matches.

| | before | after |
|---|---|---|
| `/api/count`, empty query | 1.233 s | **12.8 ms** |

**Correction, same day.** This section first put the window's opening cost —
7.76 s of CPU before, 4.86 s after — in that table, as though the count were
part of it. **The page never calls `/api/count`**: `SERVICE` exposes search,
explain, sidebar, status and wait, and nothing else, which an audit of the web
app found hours later. Whatever moved that number, it was not this. The route
serves the CLI and MCP, where the measurement stands and is worth having; the
attribution to window opening was a causal claim with no measurement under it
and is withdrawn.

**The sidebar refresh was a fixed share of the machine, by construction.**
`atMostEvery` waits `COST` (10) times what the last call took, so a refresh
settles at one eleventh of a core however dear it is — a tenth of a machine,
forever, for numbers nobody is reading. Six facet calls in thirty seconds
accounted for 1.73 s of the 8.59%.

A share is the wrong shape. The live refresh now runs only when the last one
was cheap (`SIDEBAR_LIVE_UNDER_MS`, 40 ms), which keeps it live for the
queries somebody reading the rail has actually typed and stops it for the ones
that are broad enough to be dear. The rows stay live either way at a hundredth
of the price.

| `/api/facets` | | live refresh |
|---|---|---|
| `trabzon` | 2.6 ms | kept |
| `rapor` | 9.5 ms | kept |
| `a` | 134 ms | frozen until the query changes |
| empty | 97.8 ms | frozen until the query changes |

| scourd, 90 s, nothing compiling | before | after |
|---|---|---|
| window open, untouched | 8.59% | **0.72%** |
| window shut | 0.108% | 0.122% |

**Verify the window is actually open, at both ends of the measurement.** Five
readings in this session were invalidated by not doing so — `pgrep -f 'chromium
--app'` matches the measuring shell's own command line, so it must be
`pgrep -f '^/usr/lib/chromium/chromium --app=http'`, and the pid checked again
when the timer stops.

```
pgrep -f '^/usr/lib/chromium/chromium --app=http'   # before and after
awk '{print $14+$15}' /proc/<pid>/stat              # jiffies, both ends
```

## 2026-08-07 — the arena cap, and what it costs

The allocator half of the decision left open in
[`REVIEW-MEMORY.md`](REVIEW-MEMORY.md).

A service up for one hour and three quarters, having scanned both sources at
start-up and served a window, held **589.9 MiB anonymous** against the 95.9
MiB the previous day's fix settled at on an index of the same size. Its shape
said allocator rather than structure: 868 anonymous mappings, the largest at
64, 46, 43, 35 and 34 MiB. glibc gives a thread its own arena, up to eight per
core — **160 on this twenty-core machine** — and each grows to 64 MiB and
hands nothing back.

Alternating runs, one local source of 743,000 entries, fresh index each time.
Memory is settled anonymous from `/proc/<pid>/smaps`, median of three; time is
`--scan-only` wall clock, median of three.

| `MALLOC_ARENA_MAX` | settled anonymous | scan |
|---|---|---|
| unset (160) | 164 MiB | 0.865 s |
| 8 | 98 MiB | 0.866 s |
| 4 | 57 MiB | 1.070 s |
| 2 | 31 MiB | 1.262 s |

Eight is free. Two costs **46% of the scan** and gives back 81% of the memory,
and two is what `scourd` now sets with `mallopt` before it spawns anything.
The reasoning is in `cap_allocator_arenas`: a scan is paid at start-up, the
memory is paid every second the machine is on.

**A measurement is of one binary.** The first pass of this A/B was invalidated
halfway through by rebuilding `scourd` with the cap compiled in — every
"default" row after that point was measuring the new default and read 26 MiB.
The table above uses an explicit `MALLOC_ARENA_MAX=160` control instead of an
unset variable, so the comparison survives the binary changing underneath it.

```
scourd --config <one-source-config> --scan-only                  # time
awk '/^Anonymous:/{a+=$2} END{print a/1024}' /proc/<pid>/smaps    # memory
```

## 2026-08-06 — idle memory and the four-row minute

Release build, a reflinked copy of the live native index: **2,091,824 entries,
55 segments**. The full attribution, commands, failed hypothesis and
before/after tables are in [`REVIEW-MEMORY.md`](REVIEW-MEMORY.md).

The memory was not a live half-gigabyte structure. After one `/mnt/depo`
reconciliation, `/proc/self/smaps_rollup` reported 175.1 MiB anonymous while
glibc `mallinfo2` reported **10.1 MiB live**; 165.0 MiB was free arena pages.
The largest live sites during the scan were four 100,000-row `Vec<Entry>`
batches at 25.6–33.1 MiB each and their 6.7–13.5 MiB `SegmentBytes`. The steady
watch cost was separately measured at **74.4 MiB live malloc** for 280,706
directories; `notify` stores each path in two maps.

Dropping and trimming from each successful builder thread reduced settled
post-scan anonymous memory, median of three interleaved runs, **144.1 → 95.9
MiB**. Median scan time was 6.14 → 5.59 s; peak memory was unchanged.

The 0.87% worker was one 100,000-row compaction per minute. Four one-row
commits joined the preceding scan's generation and made its smaller body
segment eligible again: commits plus compact cost **513.8 ms CPU**, or 0.857%
of a core. Advancing the generation when the scan closes keeps later trickle
rows in their own compaction cohort: the same sequence cost **27.4 ms, 0.046%**.
The worker loop with no work cost another 20 ms in sixty seconds, 0.033%.

## 2026-08-02 — first end-to-end run

Machine: Linux 6.18, NVMe. Release build.

Corpus: two project trees, **44,755 entries** (including `target/`, which is
deliberately indexed — build output is where a lot of "where did that file go"
questions actually land).

```bash
scourd --config <cfg> &
scour maintain rebuild
scour status
```

| | |
|---|---|
| entries | 44,755 |
| index on disk, before rebuild | 26.33 MiB |
| index on disk, after rebuild | 19.23 MiB |
| unsorted tail after rebuild | 0 |

### Keystroke latency

`scour search "<query>" -n 40`, whole round trip: client connect excluded,
socket, parse, search, sort, count, and 40 materialised rows included.

| query | matches | time |
|---|---|---|
| `*.toml` | 23 | 0.74 ms |
| `ext:rs` | 136 | 1.09 ms |
| `under:/…/Scour ext:rs` | 65 | 1.36 ms |
| `engine` | 69 | 1.62 ms |
| `ext:rs size:>10kb` | 43 | 1.74 ms |
| `kind:code dm:7d` | 3,559 | 2.18 ms |
| `sco` | 1,012 | 3.46 ms |
| `scour` | 902 | 4.90 ms |

Before the rebuild the same queries took 5–24 ms and reported `(full scan)`:
with every segment outside the ordered body, there is nothing to terminate
early against. That gap is what `Maintenance::Rebuild` exists to close, and
what `Status::rebuild_advised` exists to warn about.

### Still to measure

* 5M and 10M entries, on this engine rather than on the prototype. The
  prototype measured 28.2 ms and 56.5 ms for the heaviest query at those
  scales; nothing here has confirmed it.
* A subtree rename. Never measured, on either implementation.
* The rebuild threshold — how large the tail may grow before searches are
  noticeably slower. `200_000` is currently a guess.
* Peak memory during a scan of a whole home directory.

## 2026-08-02 — a whole home directory

**855,126 entries**, 112,148 of them folders. Real files, not generated.

```bash
scourd --config <cfg> --scan-only
```

| | |
|---|---|
| scan | 12.8 s (45 s including the commit) |
| index after the scan | 350 MB |
| index after `maintain rebuild` | 271 MB |
| peak RSS while indexing | 1,974 MB, with a 512 MB writer heap |

### The defect this run found

Every search took **215–232 ms** and reported `(full scan)`, on an index that
had just been rebuilt. The cause was in the fast path's precondition: it
required *every* segment to be ordered. Two files changed after the rebuild,
one two-document segment appeared, and early termination switched off
completely.

One saved file turned the whole design off. The loop already handled a mixed
index correctly — it reads an unsorted segment's matches in full and stops
early only in the ordered ones — so the precondition was the only thing wrong.

After the fix, the same query on the same index: **232 ms → 80 ms.**

### Open, and not to be quoted as though measured

* **80 ms is still not the 0.1 µs per match the prototype measured.** The
  daemon had just started and the index was not in the page cache, so this is
  plausibly a cold-start number rather than a steady-state one. It has not been
  separated.
* **The index grew back to 458 MB** across a restart. `Maintenance::Compact`
  exists and had not been run; whether that accounts for all of it is unchecked.
* **Peak RSS of 1,974 MB for 855k entries** is roughly four times the writer
  heap it was given. The prototype measured 1,492 MB for 5M entries with a
  1 GB heap — far better per entry. Not investigated.
* **The daemon holds ~1.2 GB resident after indexing.** The writer's arena is
  never released once a scan finishes. For a service meant to sit in the
  background all day this is the most important number on this page, and it is
  the one thing here that is clearly wrong.
* **Watching a home directory did not start** (`watching 0`): 112,148
  directories against the inotify per-user limit.

  **Corrected 2026-08-04 — this diagnosis was wrong.** The limit on this
  machine is 524,288, and a raw loop installs 227,806 watches in 311 ms using
  28 MB with no `ENOSPC`. What actually happens is that `notify` 8.2 hits one
  `EACCES` on one unreadable directory and abandons the entire recursive
  watch. `fanotify` would not have helped: its whole-filesystem mark needs
  `CAP_SYS_ADMIN`, and unprivileged it has a *smaller* budget than inotify.
  See `docs/ENUMERATION.md`.
* 5M and 10M entries, a subtree rename, and the rebuild threshold: still
  unmeasured on this engine.

## 2026-08-02 — where the 350 MB and the 1.2 GB actually went

Both numbers overshot what the prototype measured (99 bytes an entry, 33 MB
resident while searching). Neither was a surprise that should have been
allowed to happen: both come from decisions taken in this session and never
measured.

### The index

Same corpus, one setting changed:

| | entries | index | `.idx` | `.pos` |
|---|---|---|---|---|
| `paths = true` | 855,126 | **352 MB** | 150.6 MB | 101.9 MB |
| `paths = false` | 917,055 | **195 MB** | 60.7 MB | 18.3 MB |

**Indexing full paths as trigrams costs ~174 MB — about 45% of the index.**
It was added this session to make `path:` a real term instead of a filter,
its cost was described in the code as "the single largest lever on index
size", and then it was never measured. `index.paths = false` in the settings
turns it off; `under:` and `parent:` keep working either way, because those
are ancestor tokens rather than trigrams.

Even without it, 213 bytes an entry against the prototype's 99. Not
investigated, but the obvious suspect is the entry id: the prototype keyed
documents by a `u64`, and this stores a variable-length byte key **indexed
and stored**, which puts 855,126 unique keys in the term dictionary, the
postings *and* the document store.

### The memory

`smaps_rollup`, same process, three moments:

| | RSS | anonymous | file-backed |
|---|---|---|---|
| index open, no query yet | 209 MB | **201 MB** | 8 MB |
| after four searches | 229 MB | 198 MB | 8 MB |
| after a full scan | 2,063 MB | **2,052 MB** | 9 MB |

Two things follow, and they point in opposite directions.

**Searching is as cheap as claimed.** Four searches over 855,126 entries cost
20 MB, and the memory-mapped index barely registers — 8 MB of file-backed
pages, all of it reclaimable. The design's central promise holds.

**The writer is the entire problem.** 201 MB before a single query, and 2 GB
after a scan that never comes back — all of it anonymous, none of it
reclaimable. That is not tantivy's doing: `TantivyIndex` holds one
`IndexWriter` alive for the life of the process with a 512 MB budget, and the
arena grows past that budget and is never released.

The remedy is not a smaller budget. It is not holding a writer at all while
idle: create one when there is work, drop it when the queue drains, and use a
small heap for incremental updates and a large one only for a rebuild. On
glibc, `malloc_trim` afterwards, because a freed arena still sits in the
allocator.

Until that is done, this is a service that costs 200 MB to leave running and
2 GB to leave running after it has indexed anything.

## 2026-08-02 — the memory fix was wrong, and the measurement said so

Two changes were made on the strength of the previous section: hold the writer
only while there is work, and give the steady state a 16 MB buffer instead of
512 MB. Then the same corpus, 962,375 entries:

| | before | after |
|---|---|---|
| peak RSS during the scan | 1,974 MB | **1,795 MB** |
| segments after the scan | 3 | **228** |
| index on disk | 352 MB | 256 MB |
| idle RSS, 25 s after the last query | — | **1,824 MB** |

**The arena was not the cause.** Cutting the budget from 512 MB to 128 MB
moved the peak by 9%. Whatever holds ~1.8 GB during a scan is something else,
and naming it without measuring it would be the same mistake twice. The
leading suspect is the queue between the caller and tantivy's indexing thread
— a walk produces entries faster than they can be indexed — but that is a
hypothesis, not a finding.

**And the small buffer made things worse.** Every time a 16 MB arena fills it
writes a segment; a whole-home scan through it produced **228** where the
large one produced three. Idle housekeeping then had to merge all 228, which
is why the idle sample is *higher* than the working one: it was measured
during that merge, not after it.

The correction is not a different number. It is that the size of the buffer
should follow the shape of the work: `begin_generation` — which the engine
calls before every full walk, and only then — now switches to the bulk budget
and hands it back afterwards. A buffer sized for a trickle is the wrong tool
for a flood.

**Not re-measured.** The numbers above are from before that correction. They
are here because they are what was actually observed, and because a section
that quietly replaced them with better ones would be worth nothing.

### What did improve, and is measured

Queries at 962,375 entries, over an index with no ordered body at all
(228 unsorted segments, worst case for this design):

| query | matches | time |
|---|---|---|
| `kind:image` | 56,733 | 9.90 ms |
| `main` | 2,531 | 10.09 ms |
| `ext:rs` | 65,785 | 13.81 ms |
| `under:/…/Projeler ext:rs` | 2,061 | 19.03 ms |

The comparable figure before the fast-path fix was 215–232 ms. That gain is
real and is not affected by any of the above.

## 2026-08-02 — the root cause, found by watching the curve

Sampling one known process every three seconds during a whole-home scan, and
splitting resident memory into anonymous and file-backed:

| s | RSS | anonymous | file-backed | segments |
|---|---|---|---|---|
| 9 | 96 MB | 89 MB | 8 MB | 0 |
| **12** | 466 MB | 457 MB | 8 MB | 41 |
| **21** | 1,588 MB | **1,580 MB** | 8 MB | 230 |
| 96 | 1,768 MB | 1,632 MB | 8 MB | 457 |

The walk finished in 9.5 seconds. Memory then climbed for another nine, with
file-backed pages flat at 8 MB throughout. Nothing was being mapped; something
was being *queued*.

`add_document` hands a document to tantivy's indexing thread through an
unbounded channel. A directory walk produces entries far faster than they can
be indexed, so the queue became the filesystem: 962,867 documents at roughly
1.6 KB each — a path, a name, eleven numbers and eight ancestor tokens —
is about 1.5 GB. The measurement said 1,580 MB.

tantivy was not being wasteful. It was being handed a million documents at
once and asked to hold them.

The fix is back pressure, and it is ours: commit every 50,000 documents. A
commit blocks until the queue drains, so the queue can no longer grow past
that. The cost is one segment per commit, which compaction folds away.

| | before | after |
|---|---|---|
| peak anonymous during a scan | 1,580 MB | **396 MB** |
| segments after the scan | 457 | 22 |
| index on disk | 352 MB | 220 MB |
| after `maintain rebuild` | — | **166 MB** |
| resident while idle | 2,052 MB | **121 MB** |
| resident while serving searches | — | 123 MB |

963,103 entries. Against the shipped SQLite index at 571,334 entries and
288 MB — 504 bytes an entry against **181**.

### A regression this exposed, unexplained

Some queries became *slower* after a rebuild than they were before it:

| query | 22 unsorted segments | 6 ordered segments |
|---|---|---|
| `main` (2,539) | 10.09 ms | 24.61 ms |
| `ext:rs` (65,786) | 13.81 ms | 174.74 ms |
| `kind:image` (56,739) | 9.90 ms | 489.45 ms |

The suspect is the tie handling at the page boundary. Once documents are
ordered by date, a page boundary can land inside an enormous run of identical
timestamps — a package install stamping fifty thousand files at one instant —
and the walk collects the whole run before truncating it. Collection is
bounded *after* the fact and not during.

That is a hypothesis with an obvious shape, not a finding. It is the next
thing to measure, and until it is, "the rebuild makes searches faster" is not
a claim this project can make on a real filesystem.

### How much back pressure

Same corpus, 963k entries, one constant changed:

| documents in flight | peak anonymous | scan | segments | index |
|---|---|---|---|---|
| 10,000 | **234 MB** | 21.4 s | 107 | 244 MB |
| **50,000** | 396 MB | **13.0 s** | 22 | 220 MB |
| 200,000 | 1,145 MB | 14.7 s | **12** | 210 MB |

Two things this settles.

**Above 50,000 the memory buys nothing.** Four times the queue is three times
the memory and a scan that is *slower*, not faster. Committing is cheap;
holding a million documents is not.

**Below it, the trade is real but poor.** Ten thousand saves 162 MB and costs
8.4 seconds and five times the segments — which the next compaction then has
to merge. And it cannot go much lower: at 10,000 the queue itself is only
about 16 MB, so the remaining ~218 MB is the writer's arena and the walker,
not the thing being bounded.

50,000 is where the curve turns. It should be a setting rather than a
constant, because the right answer depends on the machine — but the default
is measured rather than picked.

## 2026-08-02 — the tie window, and what is still wrong

The 333 ms was in `top_k`, and it was neither the count nor the cache. The
window that grows to contain a tie group **materialised every row of every
attempt**, and it quadruples up to 262,144 — a quarter of a million document
store reads for a page of forty. It now carries only sort keys and addresses
and materialises once, at the end.

| sorted by | before | after |
|---|---|---|
| `main` by name (2,555) | 333.4 ms | **25.4 ms** |
| `ext:rs` by name (65,813) | — | **35.8 ms** |

Repeated three times each, identical. The same mistake existed in the fast
path's own truncation — it looked for the first candidate older than the
window's last, which trims nothing when the whole window shares one timestamp,
and after a rebuild that is the normal case. Now a hard cap.

### Not fixed, and not understood

Sorting by date on this index still costs 340–450 ms for large result sets:

| query | matches | time |
|---|---|---|
| `*.pdf` | 18 | 16.3 ms |
| `rapor` | ~600 | 14.2 ms |
| `main` | 2,555 | 342.3 ms |
| `ext:rs` | 65,813 | 361.6 ms |
| `under:/…/Projeler ext:rs` | 2,061 | 445.5 ms |

Two things are visibly wrong and are the next session's work.

**The sorted body is not surviving.** Every one of those reports `(full scan)`,
meaning no segment is recognised as ordered — on an index that was rebuilt.
Either compaction is replacing segments without the record following them, or
the record is not being reloaded. Until that is understood, none of these
numbers say anything about the design; they measure the design *switched off*.

**And the cost per match is 5.5 µs, not the 0.1 µs the prototype measured** —
fifty times. `main` with 2,555 matches taking 342 ms is not explained by the
match count at all, so there is a fixed cost in here that has not been found.
Three guesses have already been wrong (the count cap, the page cache, the
writer arena); the next step is a profile, not a fourth.

**What this means for the comparison.** SQLite answered in 0.2–12 ms at
571,334 entries. On these numbers it is faster, and saying otherwise would
require the two problems above to be understood first.

## 2026-08-03 — the native index: what a segment count costs

Machine: Linux 6.18, NVMe. Release build. Mock tree, **1,083,334 entries**.

```bash
cargo run --release -p scour-index-native --example fragment 1000000
```

### Layout

An index built through `Index::apply` in one pass, then the same index after a
compaction and after a rebuild. `MAX_STAGED` is 100,000, so a single-pass scan
of a million entries leaves eleven segments no matter how the caller batches.

| layout | segments | index time | MB | B/entry | maintenance |
|---|---|---|---|---|---|
| rebuilt | 1 | 1.7 s | 46.0 | **44.5** | rebuild 3.0 s |
| compacted | 2 | 1.9 s | 46.7 | 45.2 | compact 2.7 s |
| as scanned | 11 | 1.8 s | 50.8 | 49.1 | — |
| 32 commits | 32 | 2.5 s | 55.4 | 53.7 | — |

44.5 bytes an entry, against the two numbers recorded above at a comparable
size: **181 in tantivy** (963,103 entries, after a rebuild) and **504 in
SQLite** (571,334 entries). Eight of the 44.5 are the `ids` table, which exists
only so that a removal and a re-upsert can find the row they are about.

### Query cost against segment count

Best of five, warm, page of 40, count cap 500 — the shape a search box issues.

| query | 1 seg | 2 seg | 11 seg | 16 seg | 32 seg |
|---|---|---|---|---|---|
| `""` | 0.03 | 0.07 | 0.37 | 0.50 | 1.17 |
| `rapor` | 1.02 | 1.15 | 5.15 | 5.76 | 7.52 |
| `ext:rs` | 0.61 | 0.66 | 3.43 | 3.80 | 5.01 |
| `kind:code dm:30d` | 0.09 | 0.14 | 2.88 | 4.40 | 6.23 |
| `under:/…/Projeler ext:rs` | 2.10 | 2.27 | 5.83 | 6.65 | 9.73 |
| `ext:rs` by size | 44.30 | 44.73 | 44.22 | 44.73 | 45.89 |

**Two segments cost what one costs.** That is the whole compaction policy: fold
the head, leave the body. Rewriting the body would cost a pass over the entire
index to move 1.15 ms to 1.02.

### Three things this measurement changed

**The count cap was being spent per segment.** Every segment was handed the
whole cap of 500 and walked until it had found five hundred matches *of its
own* — and thirty-two segments doing that is the whole corpus. `rapor` at 32
segments was **13.48 ms**; with the budget shared it is 4.66. The page limit
still has to be per segment, because the winning rows may be in any of them,
but the count does not.

**`ext:` allocated a `String` a row.** `scour_core::ext_of` folds into a fresh
`String`, which is right for a caller that wants one and wrong a million times
a query. Split into `ext_str` (borrowed, unfolded) plus the fold, so the index
folds into its stack buffer and the rule for what counts as an extension still
lives in exactly one place.

**Every match was materialised in order to be sorted.** Sorting by size built a
`Hit` — including reconstructing a front-coded path — for each of ~200,000
matches, to keep forty. Collecting a sort value and a row number instead took
`ext:rs` by size from **93.9 ms to 44.3**.

Replacing the sort with a selection changed nothing measurable at this size:
the walk dominates. It stays, but it is not why the query is fast.

### Where the remaining time goes

`ext:rs` by size is 44 ms because it visits all 1,083,334 rows and cannot stop
— no sort order but the stored one can. That is the case a trigram layer would
fix, and the file formats do not have to change for it to be added.

`under:/…/Projeler ext:rs` is 2.10 ms at one segment for the same reason in
miniature: the pair matches too few rows to fill a page early, so the walk runs
to the end.

## 2026-08-03 — the two engines, side by side, on a real home directory

Machine: Linux 6.18, NVMe. Release build. `/home/hasan`, **1,197,514 entries**
including `target/` and `node_modules/`. Both engines scanned the same tree
through the same `scourd`, and both answered through the same CLI, so the
socket, the parse, the sort and forty materialised rows are in every number.

```toml
[index]
engine = "native"   # or "tantivy"
```

```bash
scourd --config <cfg> --scan-only     # first scan
scourd --config <cfg> &               # serve
scour maintain rebuild
scour search "<query>" -n 40          # best of five, warm
```

### What the index costs

| | native | tantivy |
|---|---|---|
| on disk, after rebuild | **56.51 MiB** | 213.07 MiB |
| bytes an entry | **49.5** | 186.6 |
| first scan, wall | **6.7 s** | 34.5 s |
| rebuild, wall | **≈4 s** | 39.8 s |
| peak RSS during the scan | **273 MB** | 400 MB |
| peak RSS during the rebuild | **318 MB** | 664 MB |
| resident while serving | **113 MB** | 203 MB |

### What a query costs

Milliseconds, best of five, warm, page of 40, count cap 100,000 — which is what
the CLI asks for, and which means a query matching 73,886 files has to count all
of them. A search box would ask for 500 and stop far earlier.

| query | matches | native | tantivy |
|---|---|---|---|
| `rapor` | 15 | 28.88 | **1.73** |
| `main` | 2,788 | **29.77** | 34.09 |
| `ext:rs` | 73,886 | **18.21** | 80.03 |
| `*.pdf` | 18 | 18.80 | **1.41** |
| `kind:image` | 56,916 | **19.76** | 51.97 |
| `kind:code dm:7d` | 100,000+ | **9.04** | 75.69 |
| `under:/…/Projeler ext:rs` | 2,157 | **23.22** | 68.90 |
| `size:>10mb` | 4,390 | 21.81 | **20.67** |
| `ab` | 52,538 | **29.38** | *refused* |
| `sco` | 3,501 | 31.70 | **23.48** |

`ab` is two characters, which a trigram index cannot answer at all. A scan has
no such limit, and that is one of the things it buys.

### Reading this honestly

The scan wins where a scan should: filters over columns, a scope, an extension,
anything that reads numbers rather than text. It loses where an inverted index
should win — a **selective substring**. `rapor` matches fifteen files out of
1.2 million, so there is no page to fill and no cap to reach, and the walk runs
to the end: 28.88 ms against 1.73.

That is the one case a trigram layer fixes, it is the case the layout was
designed to leave room for, and adding it changes none of the existing files —
it adds two and turns step one of the search into "start from the candidates".

Everything else says ship the native index: a quarter of the disk, a fifth of
the scan time, a tenth of the rebuild, half the memory, and no minimum term
length.

### What the measurement changed on the way

Four things, each found by running this and none of them visible in the mock:

**Empty segments were permanent.** A generation swept down to nothing folded
into an empty segment, and an empty segment has no dead rows either — so it
never qualified to be folded again. A real index reported three segments where
one held everything.

**The columns were buffered whole before being encoded.** Sixteen numbers at
eight bytes a row is **152 MB** at 1.2 million entries, live for the whole of a
rebuild. Encoding each block as it fills is the same arithmetic and holds only
what will be written: rebuild peak 528 → 318 MB.

**Freed memory was not being given back.** A fold builds a whole segment in
memory and drops it; glibc keeps the arena. `malloc_trim` after a fold:
resident 233 → 113 MB.

**Half the walk was UTF-8 validation and a searcher being rebuilt per row.**
The walk handed out `&str`, which validates every name in the index for the
benefit of the forty that reach the screen, and `str::contains` constructs a
Two-Way searcher on every call. Walking bytes and prebuilding the searcher:

| query | before | after |
|---|---|---|
| `rapor` | 71.43 | **28.88** |
| `ext:rs` | 68.05 | **18.21** |
| `sco` | 77.33 | **31.70** |

A third version was written between those two — fold and compare in place, no
copy — on the theory that avoiding the buffer would be faster. It measured at
exactly no improvement, because it gives up SIMD on both halves: the fold
becomes a byte loop instead of `make_ascii_lowercase` and the search becomes a
hand-written scan instead of `memmem`. It was removed, and the comment saying
why is in `search.rs`.

## 2026-08-03 — the trigram filter

Same machine, same tree: `/home/hasan`, **1,199,919 entries**, one segment after
a rebuild. The gap the side-by-side found was a *selective* substring, and this
closes it.

```bash
cargo run --release -p scour-index-native --example fragment   # unit tests aside
scour search "<query>" -n 40 [--count-cap N]
```

### What it costs

| | without | with |
|---|---|---|
| index on disk | 56.5 MiB | **68.2 MiB** |
| bytes an entry | 49.5 | **59.6** |
| first scan | 6.7 s | 7.9 s |
| rebuild | ≈4 s | **2.7 s** |
| resident while serving | 113 MB | **100 MB** |

Ten bytes an entry, against tantivy's 186.6 for the whole index.

### What it buys

Count cap 100,000 — the CLI default, which makes a query matching 73,889 files
count all of them.

| query | matches | before | after | tantivy |
|---|---|---|---|---|
| `rapor` | 15 | 28.88 | **0.24** | 1.73 |
| `main` | 2,795 | 29.77 | **4.04** | 34.09 |
| `ext:rs` | 73,889 | 18.21 | **3.37** | 80.03 |
| `*.pdf` | 18 | 18.80 | **0.74** | 1.41 |
| `kind:image` | 56,916 | 19.76 | **15.45** | 51.97 |
| `kind:code dm:7d` | 100,000+ | 9.04 | **8.93** | 75.69 |
| `under:/…/Projeler ext:rs` | 2,160 | 23.22 | **4.03** | 68.90 |
| `size:>10mb` | 4,445 | 21.81 | 19.50 | **20.67** |
| `ab` | 52,602 | 29.38 | 33.20 | *refused* |
| `sco` | 3,436 | 31.70 | **5.18** | 23.48 |

And the shape a search box actually issues — a page of forty, counting to five
hundred:

| query | ms |
|---|---|
| `rapor` | 0.36 |
| `*.pdf` | 0.74 |
| `ext:rs` | 1.88 |
| `main` | 2.34 |
| `sco` | 3.30 |
| `under:/…/Projeler ext:rs` | 3.95 |
| `kind:code dm:7d` | 4.48 |
| `size:>10mb` | 6.53 |
| `ab` | 9.24 |
| `kind:image` | 9.66 |

**0.24 to 9.7 milliseconds on 1.2 million files**, and the slowest of them is a
two-character term or a pure column filter — neither of which has any text to
narrow on.

### Why it cannot be wrong

The posting lists hold **block numbers**, not rows: which groups of 128 rows
contain a trigram. A query intersects the lists of its trigrams and the walk
visits only those blocks, where it applies the same exact byte comparison it
always did. A name containing the needle contains every trigram of the needle,
so its block survives every intersection — **no match can be missed**. A block
that survives without containing one costs microseconds.

Three tests hold that down: every substring of every fiftieth name compared
against brute force, a term that occurs once asserted to visit under a tenth of
the corpus, and `ext:` and glob queries checked the same way.

The one bug it did have was found this way. The writer took the name as the
filesystem spells it and the query arrived folded, so `Colpan` was unfindable as
`colpan` — a silent false negative, the one failure this design is not allowed
to have. The writer now folds, so a caller cannot forget.

### The two things that are not narrowing

`ext:pdf` and `*.pdf` narrow on `.pdf`, because a name with extension `pdf`
contains `.pdf` — an extension is only an extension when something precedes the
dot. `rap*or` narrows on its longest literal run. Both are containment claims
that follow from what the test means; anything less certain is left out, because
over-narrowing loses files and reports nothing.

A query with no text at all — `kind:image`, `size:>10mb` — has nothing to narrow
on and walks. It got faster anyway: with no test that reads a name and nothing
hidden, the walk no longer touches the name arena at all, which was a `memchr`
and twenty bytes of memory traffic a row for tests that never looked at it.

## 2026-08-03 — against SQLite, which is what this replaces

The RustEverything index is still on this machine and still being served, so it
can be measured rather than remembered. It is SQLite with an FTS5 **trigram**
index over folded names — the same idea as the filter above, which is why the
latencies are close and the sizes are not.

Two different corpora: SQLite holds 674,000 entries and Scour 1,201,115 of the
same home directory, so per-entry figures are the fair comparison and every
latency below flatters SQLite by a factor of 1.8 in row count.

```bash
sqlite3 'file:~/.local/share/rusteverything/index.db?mode=ro'   # .timer on
```

| | SQLite | Scour |
|---|---|---|
| entries | 674,000 | 1,201,115 |
| index on disk | 356.2 MB + 14.7 MB WAL | **68.2 MB** |
| bytes an entry | 577 | **59.6** |
| daemon, anonymous | 46 MB | 50 MB |
| daemon, RSS | 51 MB | 56–81 MB |

The memory is a wash and the disk is not: **9.7× less an entry**. Scour's RSS
is higher and its *anonymous* memory is not — the difference is mapped index
pages, which the kernel reclaims whenever it wants them back.

### Latency

SQLite's timer has millisecond resolution, so anything under one is reported as
zero rather than invented.

| query | SQLite (674k) | Scour (1.2M) |
|---|---|---|
| `rapor` | <1 | 0.24 |
| `main` | <1 | 4.04 |
| `sco` | <1 | 5.18 |
| `size:>10mb`, 40 newest | 1 | 19.50 |
| `ab` | 2 | 33.20 |
| `kind:image`, 40 newest | 6 | 15.45 |
| `ext:rs`, 40 newest | 10 | 3.37 |
| `*.pdf`, 40 newest | **136** | **0.74** |
| count `ext:rs` | **46** | 3.37 |

**FTS5 is not slow.** A name term answers in under a millisecond, and saying
otherwise would be false. What SQLite has instead is *outliers*: a rare
extension sorted by date costs 136 ms, because there is no index on `ext` and
the plan walks the `mtime` index looking for forty of them. An exact count costs
46 ms for the same reason. Scour has no such shape — the columns are the index.

### So what was actually bought

Ten times the disk, no outliers, and a two-character term that neither engine
refuses. Not a tenfold latency win on the common case, and the file that claims
one should be corrected rather than believed.

What is not in this table, and was the actual reason for the rewrite, is the
coupling: an engine reachable only through one GUI, a store that had to be
SQLite, a watcher that had to be Linux. `trait Index` is why both of these
numbers could be taken through the same command line an hour apart.

## 2026-08-03 — the zone map, and the segment that was not there

`ab` at 33 ms and `kind:image` at 15 turned out to be two different problems,
and the second one was not the query.

### A segment a sweep had emptied was still being walked

`scour search "" -n 5` reported **1,304,270 rows visited** on an index of
1,204,537 entries. The `probe` example, written to stop guessing, said why:

```
query ""  cap 100000
  seg 0: 1204270 rows, visited 1204270, counted 0, hits 0, stop false
  seg 1: 1204537 rows, visited  100000, counted 100000, hits 40, stop true
```

Segment 0 held nothing alive. A rescan stamps a new generation, the sweep kills
every row of the old one — and the emptied segment stayed in the list, read end
to end by every query until a rebuild happened to remove it.

Two fixes, one for each level:

* **A segment with no live row is erased** by the flush and by the sweep,
  rather than waiting for a rebuild.
* **A block with no live row is skipped**, on the strength of sixteen bytes of
  the bitmap read once instead of a hundred and twenty-eight times. This is what
  makes a *partly* deleted segment cheap, which the first fix does not cover.

Every number recorded between the trigram commit and this one was taken on an
index carrying that ballast, and is wrong by however much of it was there.

### The zone map

Each column block already stored a minimum. It now stores the true maximum
beside it — eight bytes a block a column, **one byte an entry** — so a numeric
filter rejects a hundred and twenty-eight rows with two comparisons.

The width-derived bound was far too loose to do this: a block of file sizes
spanning a kilobyte to a megabyte has twenty bits of width, so its derived
maximum is a megabyte whatever it actually holds.

`under:` uses it too, on the directory column, because a scope is a range.

### Where it landed

1,209,503 entries, one segment, 69.88 MiB — **60.6 bytes an entry**.

| query | before | after | tantivy |
|---|---|---|---|
| `size:>10mb` | 19.50 | **4.02** | 20.67 |
| `kind:image` | 15.45 | **8.18** | 51.97 |
| `under:/…/Projeler ext:rs` | 4.03 | **1.57** | 68.90 |
| `kind:code dm:7d` | 8.93 | **5.49** | 75.69 |
| `ext:rs` | 3.37 | 3.04 | 80.03 |
| `rapor` | 0.24 | 0.26 | 1.73 |
| `ab` | 33.20 | 30.52 | *refused* |

### What a search box actually feels like

A page of forty counting to five hundred, typing one letter at a time:

| keystroke | rows read | ms |
|---|---|---|
| `r` | 3,965 | 1.01 |
| `ra` | 25,272 | 2.00 |
| `rap` | 39,028 | 2.64 |
| `rapo` | 11,264 | 0.54 |
| `rapor` | 4,480 | 0.26 |
| `ab` | 35,771 | **1.79** |
| `abc` | 56,830 | 2.61 |
| `kind:image` | 130,420 | 3.18 |
| `size:>10mb` | 35,988 | 1.92 |

Nothing above 3.2 ms on 1.2 million files, including the two-character term.

`ab` at 30 ms remains, and it is the honest cost of a different question:
counting all 52,924 matches exactly. That is proportional to the answer, not to
the index, and no filter can make it otherwise — the rows have to be counted
because they all match.

## 2026-08-03 — the curve, from 270,000 to 10,833,334

Everything above is one point. This is the shape.

```bash
cargo run --release -p scour-index-native --example scale 10000000
```

Mock tree, so the per-entry size is 50 bytes rather than the 60.6 a real home
directory measured — real names and paths are longer and more varied. The
*shape* is what this is for.

| entries | MB | B/entry | index | rebuild | anon MB |
|---|---|---|---|---|---|
| 270,834 | 13.1 | 50.8 | 0.30 s | 0.60 s | 1 |
| 541,667 | 26.1 | 50.6 | 0.60 s | 1.3 s | 1 |
| 1,083,334 | 52.1 | 50.4 | 1.4 s | 2.7 s | 2 |
| 2,166,667 | 103.8 | 50.3 | 3.2 s | 6.1 s | 2 |
| 5,416,667 | 258.3 | 50.0 | 10.1 s | 16.2 s | 3 |
| 10,833,334 | 515.4 | 49.9 | 22.9 s | 36.9 s | 4 |

**Size is linear and the per-entry cost falls slightly** — bigger blocks pack a
little better.

**Building is linear.** It was not: see below.

**Memory does not move.** Four megabytes of anonymous memory to serve a
515 MB index over ten million entries. The index is mapped, so what it costs
resident is page cache the kernel reclaims whenever it wants to.

### Latency against forty times the corpus

Best of five, warm, a page of forty counting to five hundred.

| query | 270k | 1,083k | 5,416k | 10,833k | ×40 |
|---|---|---|---|---|---|
| `""` | 0.03 | 0.04 | 0.11 | 0.19 | 6.3 |
| `rapor` | 0.59 | 0.59 | 0.70 | 0.85 | **1.4** |
| `ra` | 0.29 | 0.32 | 0.44 | 0.56 | **1.9** |
| `ext:rs` | 0.32 | 0.34 | 0.43 | 0.56 | **1.8** |
| `*.pdf` | 0.32 | 0.34 | 0.43 | 0.56 | **1.8** |
| `kind:image` | 0.12 | 0.15 | 0.30 | 0.50 | 4.2 |
| `size:>10mb` | 0.01 | 0.04 | 0.21 | 0.93 | 93 |
| `under:/…/Projeler ext:rs` | 0.74 | 1.24 | 1.45 | 2.32 | 3.1 |

**Not linear, and for a reason.** A text query costs what its *answer* costs:
the trigram filter hands the walk a candidate set proportional to the matches,
not to the corpus, so forty times the files costs 1.4 to 1.9 times the
milliseconds. What grows is the fixed part — testing 84,000 block ranges
instead of 2,000 — and that is what `size:>10mb` is showing at 0.93 ms. Linear
with a very small constant, and still under a millisecond at ten million.

Nothing here exceeds **2.4 ms at 10.8 million entries**.

### Two things this measurement fixed

**Indexing was quadratic in the segment count.** 10.8 million entries took
**101.9 seconds**, against 24 for half as many. A commit checks every identity
it writes against every existing segment, and doing that one identity at a time
is a binary search per identity per segment — 577 million probes over the
course of the load, almost all finding nothing.

The identities are now sorted once and merged against the segment's table,
which is already in that order: one sequential pass a segment instead of a
hundred thousand searches. **101.9 → 22.9 s**, and linear.

**`malloc_trim` was running while the thing it should release was still held.**
A fold calls it after writing the new segment — but the new segment's bytes,
half a gigabyte at this size, were still alive until the function returned.
Moving the trim outside took the resident cost of serving ten million entries
from **360 MB to 4**.

## 2026-08-03 — sorting by the other columns

Every sort key worked and was checked against brute force. What had never been
measured is what each one *costs*. On the real home directory, 1,214,678
entries, one segment, a page of forty counting to five hundred:

| query | modified | size | created | accessed | kind | ext | name | path |
|---|---|---|---|---|---|---|---|---|
| `rapor` (18) | 0.35 | 0.31 | 0.27 | 0.25 | 0.23 | 0.32 | 0.35 | 0.35 |
| `ext:rs` (74k) | 1.68 | 5.49 | 6.29 | 4.94 | 9.12 | 5.42 | 7.63 | 21.89 |
| `kind:image` (57k) | 3.49 | 9.77 | 8.52 | 8.31 | 9.26 | 9.89 | 10.00 | 22.02 |
| everything (1.2M) | 0.74 | 25.6 | 20.1 | 18.9 | 19.7 | 29.3 | 34.0 | 269.5 |

**A selective query sorts by anything for nothing** — the filter narrows first,
and forty rows sort in a quarter of a millisecond whichever column they are
ordered by.

Before the three changes below, the same table read: `ext:rs` by extension
**84 ms**, by kind **76**; `kind:image` by name **72**; and the whole corpus by
name **1,687 ms**, by extension **1,516**.

### The tie-break was the whole problem

Ties broke on the path. That is sensible for a key that rarely ties, and
catastrophic for one that always does: sorting by `kind` puts a hundred
thousand rows at one value, and deciding which forty come first *by path* means
building a path for every one of them.

Ties now break the way rows are stored — **newest first, then path**. It is the
better answer as well as the cheaper one: someone sorting by kind wants "code
files, newest first", not "code files in alphabetical path order". And because
rows are already stored in that order, the row number *is* the tie-break: the
comparison becomes total, and there is no group to keep.

The reference in `scour-mock` and the tantivy engine were changed to match,
because this is the contract and not an implementation detail. A test pins the
intent on its own, since changing an implementation and its reference together
proves they agree and not that either is right.

### Text keys are sixteen bytes, not a string

Sorting a million rows by name allocated a million short `Vec`s. The key is now
the first sixteen bytes packed big-endian into a `u128`, which orders
identically to the bytes it came from. An extension is at most twelve bytes by
definition, so for `ext:` the key is *exact* and there is no group at all —
which is what took `ext:rs` by extension from 84 ms to 5.4.

A name can be longer, so its key is abbreviated: the rows sharing sixteen bytes
of name are compared properly, and there are few of them.

### A bug the measurement found

The row-driven walk — the one that skips the name arena when no test reads a
name — was handing `sort_value` an empty name. Sorting by name with a query
that has no name test therefore gave *every row the same key*.

The answer stayed correct, because everything then tied and the final sort
compared the real names. The cost did not: **1,117,687 rows built to return
forty.**

It was invisible to every test, because the tests that sort by name all use
`ext:rs`, which reads names. It became obvious the moment `Found` started
reporting how many rows it had built rather than only how many it had visited —
which is the same lesson as `fast_path` and `rows_visited` before it.

### What is left

`path` over the whole corpus, at 269 ms, is the one thing that cannot be
abbreviated: two paths that share sixteen bytes are the normal case, so the
sort has to build 1.2 million strings. With any filter at all it is 22 ms.

## 2026-08-03 — three optimisations that measured nothing

Before moving on, everything plausible was tried. Three of them were built,
measured against the version without them on the same machine minutes apart,
and **removed**. They are recorded because the next person to have these ideas
should have the numbers rather than the ideas.

### A per-block column cache

`ColumnBlocks::get` re-derives the section offset, the block count, the block
offset and the block header on every row — six bounds-checked reads to deliver
one number, for a filter that asks the same column about a hundred and
twenty-eight consecutive rows.

Cached the block header for the duration of the block, threaded through
`accepts` and `sort_value`.

| query | without | with |
|---|---|---|
| `kind:image` | 3.53 | 3.95 |
| `size:>10mb` | 1.98 | 2.11 |
| `ab` | 2.13 | 1.61 |
| `ext:rs` | 1.02 | 1.29 |
| everything by name | 34.91 | 36.03 |
| everything by path | 273.78 | 290.08 |

Noise in both directions. The compiler was already keeping the offset table in
a register and the branch predictor was already right; what looked like six
reads is one cache line that is always hot. Removed — it was a new public type
and a parameter on two hot functions for nothing.

### Spreading the walk over cores

The orders that cannot stop early visit every candidate block whatever happens,
so there is nothing speculative about doing it on twenty threads. Built with
rayon, chunked by block so the concatenation stays in block order and the answer
stays byte-identical.

| query | one thread | twenty |
|---|---|---|
| everything by name | 55.92 | 55.07 |
| everything by path | 433.54 | 395.61 |
| everything by size | 43.61 | 42.21 |
| everything by extension | 47.98 | 48.04 |

**Five per cent, on twenty cores.** Amdahl, and the sequential part is not the
walk: it is building the candidate list. `everything by size` visits 1.2 million
rows and keeps a `(SortValue, u32)` for each — 48 bytes a row, **58 MB**, grown
by doubling and then selected over. Threads make the cheap half cheaper.

Removed, along with the dependency.

### And the SIMD question

There was none left to add. The three primitives in the inner loop are already
vectorised by the crates chosen for exactly that reason:

* the NUL scan between names is `memchr`, which is the loop `grep` uses;
* folding a name is `make_ascii_lowercase`, which LLVM vectorises;
* the substring test is `memchr::memmem`, prebuilt per query.

What is left is bit-unpacking a column, and the cache experiment above is the
evidence that it is not the bottleneck: if decoding a block header were
expensive, caching it would have shown. The remaining cost is memory bandwidth
in the candidate list, which no instruction set makes narrower.

Hand-written intrinsics would also mean runtime feature detection and a scalar
fallback on a project that has to run on three platforms and possibly a phone.
Not for five per cent of an operation nobody waits on.

### Where it actually stands

1,217,362 entries, one segment, 70.56 MiB. A page of forty counting to five
hundred:

| query | ms |
|---|---|
| `r` | 0.16 |
| `ra` | 0.29 |
| `rapor` | 0.29 |
| `ext:rs` | 0.34 |
| `*.pdf` | 0.55 |
| `ab` | 0.59 |
| `kind:code dm:7d` | 0.60 |
| `main` | 0.71 |
| `size:>10mb` | 0.71 |
| `under:/…/Projeler ext:rs` | 0.92 |
| `kind:image` | 2.38 |

Every keystroke under a millisecond except one, on 1.2 million files. The
remaining costs — an exact count of fifty thousand matches, the whole corpus
sorted by path — are proportional to the answer rather than to the index, and
that is where they should be.

## 2026-08-03 — what a directory weighs

The measurement behind `docs/REPORTS.md`. TreeSize answers this by walking the
filesystem; everything it needs is already indexed, and directory numbers being
handed out in sorted path order makes the rollup two sequential passes.

```bash
cargo run --release -p scour-index-native --example rollup <index-dir>
```

Per 100,000 entries, best of the segments measured:

| pass | what it does | ms |
|---|---|---|
| rows | `bytes[dir_id] += size`, live rows only | **2.0** |
| rollup | one stack walk over the sorted directory table | **1.7** |
| together | | **3.7** |

**About 45 ms for a whole 1.2 million entry disk**, and proportional to the
subtree when scoped — the zone map on `DirId` skips the blocks outside it.

Nothing is stored. Caching the per-directory arrays would cost 20 bytes a
directory, 2.8 MB here, and would have to be kept correct across every removal;
that is not worth doing until 45 ms is measurably in someone's way.

The top of the answer is also a fair check that it works — this machine's real
five, from the whole-disk run:

```
57.1 GB   /home/hasan/Projeler
25.5 GB   /home/hasan/Projeler/ColpanRust/target
20.5 GB   /home/hasan/Projeler/RustWailsChat
19.9 GB   /home/hasan/Projeler/SnipperSlint
 8.8 GB   /home/hasan/.AffinityLinux
```

## 2026-08-03 — an NTFS volume, and where the milliseconds actually go

The first run against a filesystem this was not developed on: `/mnt/depo`,
881 GB of `ntfs3` with 405 GB used, mounted `uid=1000 fmask=0022
windows_names`. A Windows disk, read from Linux — Turkish filenames throughout,
a legal-document archive, and the Windows side of every project.

```bash
scourd --config depo.toml --scan-only     # roots = ["/mnt/depo"], watch = false
```

| | |
|---|---|
| entries | **1,509,184** (172,363 of them directories) |
| scan, cold cache | **4,772 ms** |
| scan, warm | **2,272 ms** |
| peak RSS while scanning | **148.4 MiB** |
| index on disk | **88.4 MB** → 85.6 MB after `maintain rebuild` |
| bytes per entry | **59.4** |

59.4 bytes an entry, against 60.6 on the home directory. The format does not
care what filesystem the names came from.

Watching was off and stayed off — `sources` reports `Caps(STABLE_IDS | CONTENT
| CASE_SENSITIVE)`, with no `WATCH`. That is worth stating because the first
attempt used a binary built minutes before the fix, which ignored `watch =
false` and spent a minute installing inotify watches across 172,363
directories before it would answer at all.

### The count is the whole cost

The same eight queries at the CLI's default cap of 100,000 and at a search
box's cap of 200:

| query | matches | cap 100,000 | cap 200 |
|---|---|---|---|
| `ab` | 86,303 | 143.8 ms (full scan) | **0.33 ms** |
| `rapor` | 15,339 | 94.0 ms | **2.78 ms** |
| `raporu` | 8,146 | 93.1 ms | **2.48 ms** |
| `hukuk` | 11,110 | 83.2 ms | **0.49 ms** |
| `kind:image` | 100,000+ | 17.0 ms | **1.51 ms** |
| `ext:pdf` | 44,578 | 15.5 ms | **0.35 ms** |
| `2026` | 100,000+ | 6.6 ms | **1.89 ms** |
| `ext:rs` | 6,648 | 2.0 ms | — |

Two things fall out of this table.

**The worst case is not the widest query.** `2026` matches more than 100,000
entries and answers in 6.6 ms, because the count hits the cap and stops.
`raporu` matches 8,146 — comfortably *under* the cap — and takes 93.1 ms,
because nothing stops it: an exact total means visiting every row. The
expensive region is matches just below the cap, not matches above it.

**A search box paying keystroke prices gets them.** At a cap of 200 the whole
set is 0.33–2.78 ms on 1.5 million entries, which is the same class as the home
directory. This is what the GUI must do per keystroke, with a higher cap only
when the user stops typing or asks for a report.

Rebuilding first (16 segments → 1, 1,409,184 unsorted → 0) changed none of
these timings by more than noise. The unsorted tail costs what the format
promised it would: nothing, until it is large enough to matter.

### Noted

`maintain rebuild` reported `Rebuild: 0 B → 0 B in 0 ms` while demonstrably
folding sixteen segments into one — `MaintReport` is not being filled in.

## 2026-08-03 — the mockup and the engine, coloured side by side

The design mockup has no engine behind it, so it reimplements the field table
and the span rules in JavaScript. The product does not: `explain` returns the
spans and a frontend maps a role to a colour. This measures the difference
between those two arrangements, because the mockup is the only place the
duplication actually exists and so the only place it can be priced.

Twenty-eight queries, coloured twice — once by `scour explain --json`, once by
the mockup's own `spans()` — and compared run for run:

```bash
python3 <scratchpad>/agree.py
```

**First run: 27 of 28 agreed.** The one that did not was `"iki kelime"`. The
mockup split terms on a whitespace regex, so it cut the phrase in half and
coloured the first word as a quote and the second as plain text; the engine
keeps a quoted run whole. Nothing about that would have shown up in a
screenshot — the query still looked coloured, and the colour was wrong.

After porting the engine's tokeniser: **28 of 28**.

That is the whole argument for the wire carrying spans rather than the frontend
tokenising for itself, made as a number rather than as an opinion. A second
parser does not announce itself when it drifts.

## 2026-08-03 — what durability costs

Nothing in the workspace called `fsync`, the manifest was written in place over
the only copy of itself, and segment files were unlinked *before* the manifest
stopped naming them. Each of those turns a kill into an index that does not
open — not a lost commit, a re-walk of the disk.

Fixing it puts a sync on every segment part, replaces the manifest and the
`alive` bitmaps by rename, and unlinks only after the manifest has been
rewritten. The same NTFS volume, same command as above:

| | before | after |
|---|---|---|
| scan, warm cache | 2,272 ms | **3,033 ms** / 3,067 ms |
| peak RSS | 148.4 MiB | 141.8 MiB |
| index on disk | 88.4 MB | 86.4 MB |

**About 34%**, paid per commit rather than per entry, on 1.5 million entries.
A first run measured 17,056 ms and was discarded: a release build was running
on the same machine. The two clean runs agree to 34 ms of each other.

The directory lock costs one `flock` at open. A second service on the same
index now refuses to start:

```
Error: opening the index at …/depo-index/native
Caused by: The index is busy: another Scour process is writing …/depo-index/native
```

And `shutdown` now ends the process. It used to set a flag that the accept loop
only looked at when the next connection arrived — on an idle machine, never.
Measured: the reply arrives (`{"id":1,"ok":{"result":"accepted"}}`) and two
seconds later `pgrep -xc scourd` reports 0, where it reported 1 before.

## 2026-08-03 — what a deep page costs

Before designing a scrollbar over a million rows, the measurement it depends
on. `search.rs` sets `need = offset + limit` and materialises all of it —
building a front-coded path per match, which the code's own comment calls the
expensive part of the whole operation — and only then skips to the offset.
Across segments it is worse: each one is asked for the whole prefix.

Same NTFS index, 1,509,184 entries, `a` with a count cap of 200, page of 200:

| offset | 16 segments | 1 segment |
|---|---|---|
| 0 | 14.35 ms | **0.54 ms** |
| 1,000 | 38.68 ms | 2.34 ms |
| 10,000 | 298.74 ms | 12.98 ms |
| 50,000 | 1,274.07 ms | 62.03 ms |
| 200,000 | 1,762.89 ms | 225.07 ms |

```bash
scour search "a" --offset N --limit 200 --count-cap 200
scour maintain rebuild          # between the two columns
```

**Linear in the offset, multiplied by the segment count.** At offset 10,000 the
sixteen-segment index is 23× slower than the one-segment index — more than the
segment count itself, because each segment builds its own full prefix and the
merge then throws away all but a page.

That decides the GUI's addressable window, and it is smaller than the 50,000
this roadmap first guessed. A 60 fps scroll has about 16 ms per frame; after a
rebuild that is around **row 12,000**, and after a large scan it is around row
1,000. So: address the first ten thousand rows, say so plainly when the user
reaches the end, and invite a narrower query — which is what a search tool
should encourage anyway. The principled fix later is a keyset term on the wire
(`after: <sort value, row>`), which the total order already makes well-defined.

`rows_built` is now on `SearchResponse`, and the CLI prints it when it
dominates, so this is diagnosable from a client rather than only from a
benchmark:

```
200 of 200+ in 0.82 ms (432 rows)
200 of 200+ in 56.76 ms (59404 rows) · 50200 yol kuruldu
```

## 2026-08-04 — what the filesystem actually promises

`Caps` was a compile-time constant: every Unix build declared `STABLE_IDS |
CASE_SENSITIVE` whatever it was pointed at. Measured on this machine, both
halves are wrong somewhere.

```bash
stat -f -c '%t %T' /  /home  /mnt/depo  /tmp  /proc
```

| path | magic | `coreutils` says |
|---|---|---|
| `/`, `/home` | `0x9123683e` | btrfs |
| `/mnt/depo` | `0x7366746e` | **UNKNOWN** |
| `/tmp` | `0x1021994` | tmpfs |
| `/proc` | `0x9fa0` | proc |

The NTFS volume's magic is not in `coreutils`' table: `0x7366746e` is `ntfs`
as little-endian bytes, the **ntfs3** driver's own value, distinct from the
`0x5346544e` that ntfs-3g reports. It had to be measured to be known.

**The same disk is case-sensitive here and will not be under Windows.**
`/mnt/depo/PROJELER` resolves and `/mnt/depo/projeler` does not — Linux's
ntfs3 is case-sensitive unless mounted `nocase`. So the answer belongs to the
mounted filesystem and its driver, not to the format and not to the operating
system.

`FsTraits` now asks, once per root, with one `statfs`:

```
/              stable_ids=true  case_sensitive=true
/mnt/depo      stable_ids=true  case_sensitive=true
/proc          stable_ids=false case_sensitive=true
/nonexistent   stable_ids=false case_sensitive=true
```

The unknown case withholds `stable_ids`, and a compile-time assertion keeps it
that way. The asymmetry is deliberate: a wrongly *claimed* stable identity
means a rescan decides every file is new — the index doubles and the sweep
then removes the originals, silently — while a wrongly *withheld* one only
means a rename is seen as a delete plus an add.

`entry_of` follows the same answer, so an exFAT stick gets a path hash instead
of an `st_ino` its driver invented.

### Also measured: btrfs subvolumes

`/`, `/home`, `/srv` and `/var/log` are separate subvolumes of one btrfs
filesystem, and **each one's root is inode 256**:

| path | `st_dev` | root inode |
|---|---|---|
| `/` | 36 | 256 |
| `/home` | 52 | 256 |
| `/srv` | 54 | 256 |
| `/var/log` | 58 | 256 |

So carrying `dev` in `Key::Inode` is not ceremony — without it these four
directories would be one identity. But `/proc/self/mountinfo` reports `0:34`
for all of them, and major 0 means an **anonymous** block device: a number the
kernel hands out at mount time rather than one stored on disk. If it can differ
between boots, every identity in a persisted index changes with it and the next
scan sees a filesystem full of new files. Not yet tested — it needs a reboot.

## 2026-08-04 — where the scan time actually goes

The whole-system survey said the kernel was not the slow part. This is where
the rest of it is, measured by instrumenting the scan path and then removing
the instrumentation.

A home directory, 1,396,476 entries, btrfs:

| stage | time | share |
|---|---|---|
| walking the tree | ~271 ms | 12% |
| **`Index::apply`** | **1,862 ms** | **82%** |
| sweep + commit | 132 ms | 6% |
| **total** | **2,133 ms** | |

For comparison, a bare parallel `getdents64` walk of the same directory takes
760 ms and sees *more* — 1.85 M entries against 1.40 M, the difference being
the exclusion rules. So the walk is not merely a minority of the cost, it is a
small one.

Inside `apply`, fifteen segments of 100,000 rows each, ~95 ms per segment:

| stage of `build()` | per 100k rows | share of build |
|---|---|---|
| **trigram extraction** | **~36 ms** | **~33%** |
| `dirs.intern` — parent path → id | ~19 ms | ~17% |
| sort by mtime, path as tie-break | ~18 ms | ~16% |
| pass 2 — filling 16 columns | ~13 ms | ~12% |
| name arena, id table, finishes | ~4 ms | ~4% |
| unaccounted | ~5 ms | — |

Two caveats worth stating. The per-stage figures come from a run that called
`Instant::now()` three times per entry, which inflates the absolute numbers —
the proportions hold, the milliseconds are an upper bound. And `write` is 1 ms
per segment: the segment files are not what costs, which is why the fsync work
in `db9c95c` was affordable.

**The single largest item in the whole scan is trigram extraction**, at about a
third of `build` and roughly a quarter of the entire scan. `dirs.intern` is
next: it hashes a parent path per entry, and after the sort those entries are
in mtime order, so consecutive rows rarely share a directory and a
last-directory cache would not help.

## 2026-08-04 — building segments in parallel: tried, reverted

The scan profile put 82% of the time in `Index::apply`, and inside it fifteen
segments built one after another on a single thread while the walk used twenty.
Segments are independent, so handing each to a background thread looks free.

It is not, and it took two attempts to find out why.

**First attempt: the count moved.** 1,442,476 entries on one run and 1,488,007
on the next, against 1,401,271 for the serial build. The cause is hard links:
two paths, one inode, one `EntryId`, so the second sighting must replace the
first — and `flush` does that by killing the old row in the existing segments.
A segment still on a builder thread is not in that list, so both rows survived.
This home directory holds 288,070 hard links, so it was not a rare race.

**Second attempt: correct, and 11% faster.** Collecting every outstanding build
before the kill fixed the count (1,401,331 on every run). Measured properly —
two binaries, interleaved, eight rounds, because the same code measured 1,834 ms
in the morning and 2,918 ms in the afternoon:

| | serial | parallel |
|---|---|---|
| mean | 2,719 ms | **2,428 ms** |
| median | 2,816 ms | 2,409 ms |
| best | 2,619 ms | 2,294 ms |
| rounds won | 1 of 8 | **7 of 8** |

**And then the tests failed.** Five integration tests in `whole.rs`: `flush`
hands the new segment to a thread and writes the manifest immediately after, so
the manifest does not name it. Some of that 11% was work not done.

Fixing it properly means moving the manifest write out of `flush` and into
`commit`, which changes `sweep`, `begin_generation` and `fold` as well. For 11%,
in a measurement whose own noise is ±10%, that is not a trade worth making.
Reverted.

What the exercise did leave: the hard-link constraint is now written down, and
the measuring method is. A single number taken at one time of day cannot be
compared with one taken at another — from here on these decisions are made with
interleaved A/B runs.

## 2026-08-04 — how many threads a disk is worth

`ScanOptions.threads = 0` meant "let the walker decide", and the walker decides
by core count. That is right on NVMe and wrong on a spinning disk, where every
concurrent reader is another seek.

`/home/hasan`, 1.85 M entries, two rounds:

| threads | round 1 | round 2 |
|---|---|---|
| 8 | 1073 ms | 337 ms |
| 16 | 284 ms | 306 ms |
| **20** | **241 ms** | **277 ms** |
| 32 | 250 ms | 282 ms |
| 48 | 365 ms | 705 ms |

Twenty is this machine's core count **and** its NVMe hardware queue count —
`/sys/block/nvme0n1/mq/` holds twenty entries, because the driver opens one
queue per core. That the two agree is not a coincidence, and it is why the
rule is `cores` rather than a tuned constant.

**The previous project's rule was `cores * 2`**, on a source comment claiming
32 beat 20 by 20% on a 20-core machine. It does not reproduce here: 32 ties
with 20 and 48 costs dearly. Carried over as `cores`.

### Classifying a mount

Three questions, cheapest first:

```
statfs f_type  →  nfs/smb/cifs/9p/afs/ceph/fuse → Network
                  tmpfs/ramfs                   → Memory
/sys/dev/block/MAJ:MIN/queue/rotational = 1     → Spinning
                                        = 0     → Solid
```

Two things make the second step less obvious than it looks. A partition's sysfs
directory has no `queue/`, so the parent disk's has to be read. And btrfs
reports its source as `/dev/nvme0n1p5[/@home]` — the subvolume in brackets is
not part of any path that exists.

Measured on this machine:

```
/              medium=solid-state  threads=20 debounce=200ms
/home          medium=solid-state  threads=20 debounce=200ms
/mnt/depo      medium=solid-state  threads=20 debounce=200ms
/tmp           medium=memory       threads=20 debounce=200ms
/proc          medium=unknown      threads=8  debounce=500ms
```

**The spinning and network figures are guesses, not findings.** This machine
has two NVMe drives and no network mount, so `Spinning => 1` and
`Network => 4, 5 s debounce` are reasoned defaults marked as unmeasured. The
Network debounce is the one with an argument behind it: a remote mount reports
changes late and in bursts, and each reaction costs a round trip, so batching
harder is worth more there than promptness.

## 2026-08-04 — real filesystems, built in RAM

Every claim in `scour-source-fs/src/fs.rs` was written against what this
machine has: two NVMe drives and one NTFS volume. The rest was reasoned about.
`scripts/fstest.sh` makes the rest available — a file in `/dev/shm`, formatted,
loop-mounted, filled with the awkward cases, and handed to the same code the
scanner uses.

```bash
sudo scripts/fstest.sh
```

| fs | size | `STABLE_IDS` | hard links |
|---|---|---|---|
| vfat | 64M | **false** ✓ | **none** (`st_nlink=1`) |
| exfat | 64M | **false** ✓ | **none** |
| ext4 | 64M | true ✓ | yes (`st_nlink=2`) |
| xfs | 320M | true ✓ | yes |
| btrfs | 128M | true ✓ | yes |
| f2fs | 128M | true ✓ | yes |

Two things confirmed. Withholding `STABLE_IDS` from the FAT family is right.
And they have no hard links at all — `ln` fails silently — which is what
`REPORTS.md` predicts for the first tier of duplicate detection there.

**And one test of mine was wrong.** The first version reported every
filesystem as case-sensitive, including vfat, by checking that `README.md` and
`readme.md` both existed. On a case-insensitive filesystem the second lookup
finds the first file, so `-f` says yes everywhere. It now counts directory
*entries* instead.

`MEDIUM` cannot be tested this way at all: everything is a loop device over
tmpfs, so `rotational` says nothing about the format. The script says so in its
own output rather than letting the column look meaningful.

Sizes differ by filesystem because the minimums do — XFS refuses under 300 MB,
btrfs under about 110, and the FAT family is content with 64.

## 2026-08-04 — Windows and macOS, written and cross-checked

`fs.rs` answered `Unknown` on both, so every mount there got the conservative
defaults regardless of what it was. Both are now implemented, and while neither
can be *run* here, all three targets type-check:

```bash
cargo check --target x86_64-pc-windows-msvc --workspace   # clean
cargo check --target x86_64-apple-darwin --workspace      # clean
cargo check --target aarch64-apple-darwin --workspace     # clean
```

**Windows.** `GetVolumePathNameW` maps a path to its volume, then
`GetVolumeInformationW` gives the filesystem name and the flags, and
`GetDriveTypeW` separates a network drive from a local one. Two things this
settles: `FILE_CASE_SENSITIVE_SEARCH` is **off by default even on NTFS**, which
is the opposite of the same disk under Linux's ntfs3 — so case sensitivity
belongs to the mount and not to the format, twice over. And stable ids are
`NTFS | ReFS`, because the FAT family has no file id at all.

Not done: telling NVMe from SATA needs `IOCTL_STORAGE_QUERY_PROPERTY`, and
telling a spinning disk from an SSD needs `DEVICE_SEEK_PENALTY_DESCRIPTOR`.
Both open a raw volume handle, which costs something on every call and is
privileged on some systems. Left until there is a Windows machine to measure on.

**macOS.** `statfs` gives `f_fstypename` as a string — easier than Linux's
magic numbers — and `MNT_LOCAL` catches anything reached over a network
whatever it calls itself. APFS and HFS+ are reported as **case-insensitive**,
because they ship that way and can be formatted either way with nothing in
`statfs` to tell them apart: claiming sensitivity that is not there would let
two spellings of one file both be indexed. APFS is assumed solid-state, which
is true of every Mac since 2016 and wrong for an external HFS+ spinning disk;
IOKit is where the real answer lives.

The Win32 constants (`DRIVE_REMOTE`, `FILE_CASE_SENSITIVE_SEARCH`) are written
out rather than imported: `windows-sys` moves them between modules across
versions, and these have not changed since Windows 95.

## 2026-08-04 — what trigrams cost, and what they are worth

The scan profile put trigram extraction at a third of `build`. Measured
properly this time — interleaved A/B, because a single run at one time of day
cannot be compared with one at another.

### The cost

Same directory, alternating binaries, four rounds each:

| | scan | index |
|---|---|---|
| with trigrams | ~2,980 ms | 97 MB |
| without | ~1,879 ms | 76 MB |

**37% of the scan and 22% of the index.**

### What they buy

Two indexes of the same directory, same queries, count cap 200:

| query | with | without | ratio |
|---|---|---|---|
| `rapor` | 3.66 ms | 68.08 ms | 19× |
| `readme` | 7.59 ms | 54.44 ms | 7× |
| `config` | 4.49 ms | 14.32 ms | 3× |
| `ext:pdf` | 2.66 ms | 41.75 ms | 16× |
| `*.rs` | 4.28 ms | 19.61 ms | 5× |
| **`zzqx`** (no match) | **0.45 ms** | 61.90 ms | **138×** |
| **`kütüphane`** (no match) | **0.08 ms** | 53.54 ms | **669×** |

The two extremes are the ones that decide it: **terms that match nothing**.
The filter says "these three letters appear in no block here" and skips the
segment whole. And a search box spends most of its life on prefixes that match
nothing yet — `k`, `kü`, `küt` — so the case trigrams are best at is the case
that happens most.

Paying 1.1 s once to save 50 ms per keystroke is not a trade worth reversing.

### Making the cost smaller instead

The writer kept the trigrams of the current block in a `HashSet<u32>`. A key is
three bytes — 2^24 possible values — and hashing a number that small to store
it costs more than addressing it directly. Replaced with a 2 MB bitmap over the
whole key space, plus a list of the keys that were set so that clearing a block
is proportional to what it held rather than to the key space.

Three-way interleaved, three rounds:

| | mean scan | trigram's share |
|---|---|---|
| `HashSet` | 2,937 ms | 1,010 ms |
| **bitmap** | **2,543 ms** | **616 ms** |
| no trigrams | 1,927 ms | — |

**The cost fell 39%**, with the same index size, the same search behaviour and
the same brute-force verification passing. What remains — 616 ms — is folding
and the postings-list merge, and building them after the scan rather than
during it is still available if that is worth taking.

## 2026-08-04 — relevance, and what a name alone cannot do

The roadmap calls ranking the largest quality gap, and the demonstration is one
query. `main`, on this machine, in the default order:

```
main.log                                  a log file
.git/logs/refs/heads/main                 git internals
.git/refs/heads/main
target/debug/build/…/main_window.rs       generated
apps/scourd/src/main.rs                   ← eighth
```

`SortKey::Relevance` now scores a name against the query's terms, in rungs wide
enough that no length adjustment can overturn one:

| rung | example for `main` |
|---|---|
| 4000 | the name without its extension is the term — `main.rs` |
| 3000 | the whole name is the term — a folder called `main` |
| 2000 | the name starts with it — `main_window.rs` |
| 1000 | it starts a word inside — `my-main.rs`, not `domain.rs` |
| 100 | matched somewhere |

**Two orderings were tried and measured before this one.** Ranking the exact
name highest is what a scorer "should" do, and it filled the page with
`.git/refs/heads/main`. Excluding `.git` moved the problem rather than solving
it: a hundred `android/src/main` directories took its place. A stem match means
someone named a *file* after the thing being searched for, which turns out to
be a far stronger signal than a directory carrying the word.

### And it is still not enough

With the rungs in the right order, `main` returns `main.c`, `main.m`, `main.f`
— all of them from SDKs and package caches. Of the first 200 results:

| | count |
|---|---|
| under `~/Projeler` (the user's own work) | **48** |
| under `~/.pub-cache`, `~/Android`, `~/.cargo`, `~/.local` | **88** |

Scoped to `~/Projeler`, the first four are still `target/` and `build/`
artefacts; `scourd/src/main.rs` is fifth.

So a name carries no information about whether the file is *yours*, and that is
the information this query needs. The next rung has to come from the path:
a penalty for `target/`, `build/`, `.pub-cache`, `Android/Sdk`, `.git`. The
cheap way to get it is a per-directory table computed once — the directory
numbers are already handed out in sorted path order, so a penalty is one pass
over `DirTable` and one byte per directory, about 130 KB here. Not done yet.

## 2026-08-04 — the distance byte, and the metric that was wrong

The path rung, built as the note above predicted — one pass over `DirTable`,
one byte a directory — but carrying a different number than expected, and
correcting the way the previous section measured success.

### The metric was wrong

That section counted, of the first 200 results for `main`, 48 under
`~/Projeler` against 88 in caches and SDKs, and treated the first number as the
thing to raise. It is not: of the 902 `main` matches under `~/Projeler`, **794
are under `target/`, `build/` or `.git/`**. Counting them as the user's own
work counts generated files as authored ones.

The honest metric is *own work that was written rather than generated* — under
`~/Projeler`, outside those three directories, of which there are 108 in the
whole index. Against that:

| ranked by | in the first 20 | first 40 | first 200 |
|---|---|---|---|
| name alone | **0** | **0** | **0** |
| name + depth | 17 | 29 | 31 |
| name + distance | **20** | 29 | 34 |

Zero, not 48. Not one of the first two hundred results was a file the user
wrote. The first page is now entirely theirs, and the first-200 column stops
rising because there are only 108 such files in existence — the rest of that
page is third-party code with no way to tell it apart by name.

### One number, not three

The first design was a penalty per reason: so much for being hidden, so much
for being under a build directory, so much per level of depth. Measured against
the whole match set for a query — every match, not a page, so that a rule is
judged on how it reorders rather than on what the previous rule left behind —
the three turned out to be the same idea counted in the same unit.

So: **every path component is a step, and a component that is hidden or is a
build directory is three steps.** One byte a directory holds the total.

Three, and each of the alternatives is refuted by a query:

| weight | what it does |
|---|---|
| 1 (plain depth) | `index` returns a generated `build/index.js` bundle first |
| **3** | — |
| 6 | `config` loses `~/.config/fish/config.fish` from the first page entirely, to a Flutter engine `.gni` file |

The middle one is the point of the whole scheme: a dotfile in `~/.config` **is**
the user's own writing. Hidden does not mean generated — only *deep and hidden*
does, and depth already says that. Six overcorrects into treating every dotfile
as a cache.

The build-directory list earns its place separately. Without it, `index` is
`~/Projeler/sezi-server/build/index.js` first and `~/Projeler/Sezi/worker-rs/
build/index.js` sixth: bundles, in ordinary directories, at ordinary depths.

### What it changed

First result, before and after, on the same index:

| query | by name alone | with distance |
|---|---|---|
| `main` | `…/target/debug/build/xberg-tesseract-…/CMakeFiles/_CMakeLTOTest-C/src/main.c` | `~/Projeler/SnipperSlint/src/main.rs` |
| `config` | `~/.cargo/registry/src/…/liblzma-sys-0.4.6/config.h` | `~/Projeler/RustPdfCompressor/src/config.rs` |
| `readme` | `~/.AffinityLinux/Patch/return-affinity-colors/README.md` | `~/Projeler/Scour/README.md` |

### What it costs

**Nothing measurable.** The byte is computed in `DirWriter::finish`, where the
paths already exist as strings, and read by number at query time.

| | |
|---|---|
| index size | 165,895 directories → **162 KB**, of 87.3 MB |
| scan | 2,718 ms, inside the 2,272–3,067 ms band the same scan has always measured |
| query | relevance costs 0.02–0.38 ms more than sorting the same result set by name, and that gap is the *whole* scorer, not the byte |

### The bound, which is the design

A step is 8, the table records at most 60, so the whole distance is at most
480. A long name already costs up to 255. The narrowest gap between two rungs
of the name score is 900, and 255 + 480 is 735.

**So distance can only order rows that the name has already tied.** A file
whose name answers the query better always wins, however deep it is buried.
That is what makes the byte safe to apply to every query rather than something
the user has to know about and switch on.

The cap is a guarantee rather than a policy: walking 230,351 directories on
this machine, the deepest scores **32**.

### Two implementations of one number

A segment reads the distance from its table; the merge across segments
recomputes it from the path, because a `Hit` carries no directory number.
`brute_force` deliberately does not model relevance, so nothing else would
notice those two drifting apart —
`relevance_puts_the_near_copy_first_however_many_segments_there_are` is the
test that does, and it was checked by breaking the merge side on purpose and
watching it fail.

## 2026-08-04 — the taxonomy, applied

`docs/TAXONOMY.md` designed fourteen kinds against a histogram. This is what
happened when the shipped code produced them, on 1,434,900 indexed entries.

### The distribution

| kind | entries | share |
|---|---|---|
| **build** | **686,851** | **47.87%** |
| code | 178,360 | 12.43% |
| folder | 166,452 | 11.60% |
| **file** (unknown) | **165,226** | **11.51%** |
| data | 94,955 | 6.62% |
| image | 57,598 | 4.01% |
| doc | 40,114 | 2.80% |
| exec | 25,911 | 1.81% |
| config | 14,097 | 0.98% |
| archive | 3,673 | 0.26% |
| font | 1,537 | 0.11% |
| audio | 107 | 0.01% |
| video | 19 | 0.00% |

The counts add to the total exactly, which is the only cheap proof that every
row got one kind and no row got two.

### What it replaced

The previous classifier, run over the same filesystem — 1,927,541 entries —
left **1,127,692 of them unknown: 58.50%**. It also produced exactly **124**
`Media` rows, for a category that took a discriminant.

The two denominators are not the same set: the walk sees everything, the index
holds what survives the platform exclusions. So 58.50% against 11.51% is the
right direction and not a subtraction. What is exact is that more than half of
every file on this machine used to have no kind at all, and the reason is one
line of the table: `build`.

### The generated-documentation rule, measured before it was written

Of 189,785 HTML files here, **161,115 — 84.9% — have a dot inside the stem**,
and every one sampled was rustdoc (`struct.Foo.html`, `mod.rs.html`) or
dartdoc. Nothing hand-written appeared among them; a page a person writes is
`index.html`. javadoc's fixed names carry no dot and are listed separately.

The first version of the rule was a list of rustdoc prefixes plus javadoc's
names and caught 83.6% — the plain dot rule is simpler, more general and
catches more, including the `*.rs.html` source pages the prefix list missed.

It is still a heuristic and still an interim. When the scanner grows a
`derived` bit, this becomes a property of location, which is what it was all
along.

### Measured and rejected

**A rule for versioned shared libraries.** `libLLVM.so.22.1-rust-1.99.0-nightly`
is machine code and lands in `File`, because the extension by the
rightmost-dot rule is `0-nightly`. It looks like an obvious gap. It is 204
files on this machine of which **21** are unclassified — 0.0015% — so the rule
is not worth the surface it adds. Noted here so the next person to notice it
does not have to measure it again.

### What is left unknown, largest first

Exactly the six categories `TAXONOMY.md` predicted, and nothing else: a
content-addressed model blob, `.propcol` and `.mzz` (one vendor's private
formats), `libLLVM.so.…-nightly`, and Gradle's hash-named zip cache. Every one
of them is either private, content-addressed, or determined by where it sits.
None is fixable by adding an extension to a table.

### Cost

| | |
|---|---|
| scan | 2,051 ms, against a 2,272–3,067 ms band — the table lookup is a binary search where six `contains` scans used to be |
| index | 99.1 MB, from 87.3 MB — four bits a row instead of three, plus a corpus that grew between runs |

The classification also moved from six linear scans of up to 300 strings to one
binary search over 280 entries, which is why a scan that does strictly more
work did not get slower.

## 2026-08-04 — the disk-usage report, checked against `du`

`REPORTS.md` §A argued that what TreeSize walks a filesystem for is already in
the index, and predicted ~45 ms for a 1.2 M-entry disk from a single-segment
prototype. Built, and measured against the tool it replaces.

### It agrees with `du` exactly

`/home/hasan/Projeler/Scour`, immediately after a rescan so that neither side
is looking at a different tree:

| | Scour | `du` |
|---|---|---|
| logical bytes | 16,691,298,419 | 16,691,298,419 (`du -sb --apparent-size`) |
| bytes on disk | 16,776,974,336 | 16,776,974,336 (`du -sB1`) |
| files | 34,292 | 43,170 (`find -type f`) |

The two byte totals are exact. **The file count is deliberately different**, and
checking why is what makes the byte totals believable: 43,170 names under that
directory resolve to **34,292 distinct inodes**, which is exactly what Scour
reports. Cargo hard-links inside `target/` heavily.

That falls out of the layout rather than being implemented: a source with
stable identities gives every name of one inode the same `EntryId`, so the
index holds one row for it. A `HashSet` of seen identities was written first,
measured, and **folded exactly zero rows** — there was never a second one to
fold. It was removed, and so was the `count_links` request field, because an
option that cannot change the answer is worse than no option at all.

### Cost

| scope | entries | time |
|---|---|---|
| whole index | 1,269,401 files | **163–183 ms** |
| `~/Projeler` | 699,553 files | **91–99 ms** |
| one project | 34,292 files | **32 ms** |

Four times the predicted 45 ms, and the reason is in the prediction: the
prototype rolled up **one** segment. A directory has a different number in every
segment that holds rows in it, so per-segment rollups cannot simply be added —
each segment's stack closes ancestors the others also close. The own-totals are
merged by path first, which means reconstructing 166,452 directory paths and
sorting them.

Still two orders of magnitude under what it replaces, and scoped queries — the
case a report screen actually issues, because clicking a folder is a new scope —
are proportional to the subtree.

### The one thing to know about the answer

The bytes of a hard-linked file are credited to **one** of the directories it
appears in, whichever name was written last. `du` is arbitrary here too — it
credits whichever it reaches first — so the two can agree on a total and
disagree about where the weight sits.

## 2026-08-04 — the window, and two things it made visible

Phase 5.1 built, installed, and pointed at a real index of two sources —
`/home/hasan` and the NTFS volume at `/mnt/depo`, 2,969,355 entries. Using it
found two defects that no benchmark would have.

### A rebuild could not get below one segment per source

`Maintenance::Rebuild` folded within a generation, on the reasoning written
into the code: there is only ever one, because a scan re-upserts every file it
finds under a new stamp and the sweep removes what it did not. **That is true
of one source and false of two.** Each source's scan takes its own generation,
so the index sat at two segments and 1,441,890 unsorted entries however often
a rebuild ran.

What makes folding across generations safe is not that generations stop
mattering — a merged segment carries one stamp, and giving old rows a new one
would hide them from the sweep that exists to remove them. It is that **outside
a scan there is nothing left for a generation to decide**. The index now tracks
whether a generation is open (handed out, not yet swept) and folds everything
only when none is. The old behaviour is what it falls back to, so the test that
pins the invariant passes unchanged.

| | before | after |
|---|---|---|
| segments | 2 | **1** |
| unsorted | 1,441,890 | **0** |

### The first frame cost a full scan

A search window opens with an empty query, and the order it opens in is
relevance. Relevance with no terms scores every row the same — so the walk
visited all 2.97 M rows to hand back a page the row layout was already holding,
and the one frame a person actually watches for cost **121 ms**. Treating "no
terms" as the stored order makes it **0.58 ms**, which is 208×.

### Where it stands, on 2,969,355 entries in one segment

| query | median | matches |
|---|---|---|
| *(empty — what the window opens with)* | **0.58 ms** | 100,000+ |
| `kütüphane` (no match) | **0.05 ms** | 1 |
| `*.slint` | 1.38 ms | 1,399 |
| `sezi` | 3.55 ms | 4,327 |
| `main` | 8.82 ms | 5,175 |
| `config` | 14.68 ms | 17,900 |
| `ext:pdf` | 18.76 ms | 44,611 |
| `rapor` | **92.31 ms** | 15,436 |

### The one that is out of line, and what it is not

`rapor` visits 439,936 rows in 92 ms — 214 ns a row — while `config` visits
*more* rows, 476,672, in 14.7 ms, at 31 ns. Seven times the per-row cost for
the same shape of query.

Two explanations were measured and refused. It is not the sort: `modified`
takes 88 ms on the same term. It is not name length: the matched names average
22.8 bytes under `/home/hasan` and 23.6 under `/mnt/depo`.

What is left, and what the code makes likely, is the **fold**.
`Folded::fold_bytes` has a fast path — `make_ascii_lowercase` over the whole
name, which the compiler vectorises — and a slow one that calls
`char::to_lowercase` per character and walks its iterator. A single non-ASCII
byte anywhere in a name takes the slow path, and the blocks a Turkish word
selects are full of Turkish names. **Unconfirmed**: the correlation is
suggestive and no one has profiled it.

Worth fixing and not yet fixed. Whatever replaces it has to fold *identically*
to `DefaultFolder` — the two are compared by a test for exactly this reason,
because a fold that disagrees does not fail, it silently stops matching.

## 2026-08-04 — the fold, confirmed and then fixed

The previous section left a seven-times per-row cost unexplained and marked it
unconfirmed. It is the fold, it is confirmed, and it is now most of the way
fixed.

### Confirming it

`examples/folding.rs`, on names shaped like the ones on the volume — the same
words in English and in Turkish, and a third set that is English with one `ş`
in every tenth name:

| | ns a name |
|---|---|
| ascii | 11.3 |
| **turkish** | **88.9** |
| ascii, one in ten not | 14.9 |

**7.9×**, against the 7× measured per row on the real index. The model holds:
`Folded::fold_bytes` had a vectorised path for names that are entirely ASCII
and a `char::to_lowercase` loop for everything else, so **one non-ASCII byte
anywhere moved the whole name onto the slow path**.

### Two changes, measured separately

**Bulk the ASCII runs.** A mixed name is mostly ASCII — `değişiklik.rs` is
thirteen characters of which three are not — so each run is copied and
lowercased in bulk and only the rest goes through the rules.

**A table for the two-byte range**, U+0080–U+07FF, which holds every letter
Turkish, Western European, Greek and Cyrillic writing needs. Built *from*
`char::to_lowercase` at first use rather than written out, because a hand-typed
table of 1,920 entries is a second statement of the Unicode rules and two
statements drift.

Interleaved, six rounds each, runs-only against runs-plus-table:

| | runs only | + table | |
|---|---|---|---|
| ascii | 7.6 ns | 7.6 ns | **+0.0%**, won 3/6 |
| turkish | 49.9 ns | 40.1 ns | **−19.6%**, won 6/6 |
| ascii, one in ten not | 9.8 ns | 8.4 ns | −14.3%, won 6/6 |

Exactly nothing on the path that was already fast, which is the shape a change
like this should have.

### End to end, on 2,972,213 entries

| query | before | after |
|---|---|---|
| `rapor` | 92.31 ms | **59.10 ms** |
| `belge` | 65.51 ms | **41.37 ms** |
| `proje` | 64.07 ms | **41.29 ms** |
| `config` | 14.68 ms | 14.63 ms |
| `main` | 8.82 ms | 8.61 ms |

**36% off a Turkish search and nothing off an English one.** Folding a name is
88.9 ns → 40.1, or 2.2× in total.

### What is left

A Turkish query still costs 134 ns a row against 31 for an English one, and
40.1 ns against 7.6 for the fold — the same ratio, so the remaining difference
is still the fold and not something new. Closing it means making the
per-character path itself cheap, and the table is already the cheap half of
that. Not obviously worth more.

### The test that made this safe to do

A fold that disagrees with `DefaultFolder` does not fail — the file is stored
under one spelling, searched for under another, and never found. The existing
agreement test was ten names somebody thought of.

`folding_agrees_on_every_mixture_of_scripts_it_can_be_handed` generates twenty
thousand names from an alphabet of ASCII, Turkish, Greek, Cyrillic, CJK, an
emoji, a character that folds to *two* characters, and a bare combining dot —
deterministically, so a failure reproduces by running it again.

It earned its place twice. It caught the table copying `U+0307` through where
the general path drops it, which is exactly what `İ` → `i` depends on. And when
the Turkish rule was removed on purpose to check the test would notice, it
failed with `"-ğ.-İ9🙂ğ中ı"` — a case no one would have written by hand.

## 2026-08-04 — what a hostile filename does, checked rather than claimed

Eleven files written into a real indexed directory and searched for through the
running service, because "mixed scripts are fine" is worth more as a result
than as an assertion:

| name | found by |
|---|---|
| `Değişiklik Raporu 2026.pdf` | `değişiklik`, `DEĞİŞİKLİK` |
| `İSTANBUL-ıspartaISPARTA.txt` | `istanbul`, `İSTANBUL`, `isparta`, `ısparta`, `ISPARTA` |
| `ÇalışkanÖğrenciÜniversite.docx` | `çalışkan`, `ÇALIŞKAN`, `öğrenci` |
| `mixed_ağırlık_weight_βάρος_вес_重量.md` | `ağırlık`, `βάρος`, `вес`, `ВЕС`, `重量` |
| `🙂 emoji ve ş bir arada.txt` | `emoji` |
| `combining i̇ dot.txt` | `combining` |
| `ẞ-eszett-ss.txt` | `eszett` |
| 245 bytes of `a` then `ş.txt` | `under:` the folder |
| `ş` then 240 bytes of `b` | `under:` the folder |
| 78 `ş` characters | `under:` the folder |
| `bozuk-\xff\xfe-ad.txt` — **not valid UTF-8** | `bozuk` |

All eleven indexed, all eleven found, nothing crashed. The Turkish rule works
in every direction: `ı`, `i`, `I` and `İ` all reach each other.

### Two misses, both correct

`CALISKAN` does not find `Çalışkan`. Scour folds **case, not diacritics** —
which is what Everything does too, and a deliberate line. Worth revisiting as
an option, because a Turkish keyboard is often not in front of a Turkish
speaker.

`ΒΆΡΟΣ` does not find `βάρος`. `Σ` lowercases to `σ` and the file ends in the
final sigma `ς`; `char::to_lowercase` is not context-sensitive and
`DefaultFolder` behaves identically. Fixing it means case *folding* rather than
lowercasing. Not this language's problem to solve first.

### And one real defect, in the CLI rather than the engine

The first run of this test reported zero for everything, and the tool was
right — the *test* was wrong. `scour eszett --limit 1` searches for the three
words `eszett --limit 1`, because the bare form takes everything after the
program name literally. That is the correct behaviour for a search tool, and
`scour explain` rejects the same flag rather than swallowing it, so the two
disagreed.

`-n` and `-s` are now accepted before the query, `--help` states the rule, and
the bare form orders by relevance rather than by date — someone who typed a
word wants the file that answers it, not the file that happens to be newest.

## 2026-08-04 — `watching 0`, and the four things behind it

The status line had said `watching 0` since the service was first installed,
and the recorded reason — inotify running out of watches on a home directory —
was wrong. Nothing had ever asked the system.

```
/home/hasan/Projeler/Scour: watched, 315.8ms to install
/home/hasan: REFUSED after 62.5ms — Permission denied
             about ["/home/hasan/.local/share/waydroid/data/vendor"]
```

The limit is **524,288** and the tree holds **342,000** directories. It was
never close. `notify`'s recursive watch walks the tree itself and abandons the
**whole** watch at the first directory it cannot read, so one root-owned
Waydroid directory left an entire home directory unwatched — and the reason was
discarded twice on the way up, once in `watch::start` and once in
`start_watching`, leaving a count and no cause.

`examples/canwatch.rs` is that probe, kept.

### Ask for the subtree, and split only what is refused

Recursive first; if refused, watch the directory itself and ask each child
separately, six levels deep. The branch that is genuinely unreadable is the
only one that gets split.

| | before | after |
|---|---|---|
| sources watched | 0 of 1 | **1 of 1** |
| subtrees skipped | (all of them) | 191, all under `~/.local/share/waydroid/data` |

### Three more, each found by the previous fix

**Watching blocked start-up.** Installing 342,000 inotify watches one at a time
took **15.1 seconds**, during which the socket did not exist and every client
waited for a service that was already running. Nothing about answering a query
needs the watches, so it moved to a thread of its own: **1.3 s** to first
answer.

**191 queued scans of directories nobody can read.** The first version emitted
`Change::Rescan` for every skipped subtree. A walk needs exactly the permission
the watch just did not have, so each one queued a scan that could read nothing —
and 191 of them behind one worker thread took a query from 14 ms to **51
seconds**. A subtree nobody can read is not pending work.

**The watcher ignored the scan's exclusions.** It reported changes for files the
walk deliberately skips, and every one became an index entry the next walk would
not renew. One `cargo test` under a watched-but-unscanned build directory took a
query from 8 ms to **13 seconds**. `Source::watch` now takes the same
`ScanOptions` as `Source::scan`, which is the honest shape: watching and
scanning have to agree about what is inside a source.

### And the segment explosion underneath all of it

Compaction waited for the machine to go idle, which rests on churn arriving in
bursts. A machine that is compiling never goes idle: **222 segments**, and a
query that answers in 8 ms at one segment taking 13 s.

`compact_urgent` merges past a ceiling without waiting — 222 → **7** on the
next commit. The cost is real and recorded: a compaction holds the index's
write lock, so a query issued during one waits for it. Measured at 25 s once,
mid-churn. Making that not block means building the merged segment outside the
lock, which was tried once for scanning and reverted; it is not done here.

### Where it settles

Quiet machine, 2,981,168 entries, 49 segments:

| query | |
|---|---|
| `main` | 12.2 ms |
| `config` | 24.1 ms |
| `ext:pdf` | 63.1 ms |
| `rapor` | 77.0 ms |

And live tracking works end to end: a file created is findable in about three
seconds, a new folder with a file inside it likewise, a move removes the old
name and adds the new one, and a delete removes both.

## 2026-08-04 — why the window feels slow, and what the floor actually is

The engine answers in single-digit milliseconds on a benchmark and the window
felt like seconds. Three separate causes, and the third is the one that matters.

### The debounce collapsed nothing

Sixty milliseconds is shorter than a person's gap between keystrokes, so every
timer fired before the next key arrived. The trace, typing `toki`:

```
query-changed "t" / "to" / "tok" / "toki"
reply 2 / reply 3 / reply 4 / reply 5 / reply 6 / reply 9
```

Four searches **and** four facet counts, seven of them thrown away *after*
being paid for. 180 ms collapses a burst; the facet count now follows a result
that was actually shown rather than riding along with every keystroke.

### A term shorter than a trigram walks the whole index

| typed | relevance | stored order |
|---|---|---|
| `t` | 899 ms, full scan | **41 ms** |
| `to` | 199 ms, full scan | 80 ms |
| `tok` | 65 ms | 62 ms |

The filter is built on three-letter keys, so below three characters it narrows
nothing — and relevance has to see every match before it can name the top
forty. The window now asks for the stored order below three characters, which
stops as soon as it has a page. Ranking a million matches of `t` by how well
they answer `t` was never information.

### And the real ceiling, measured on the live index

`examples/innerloop.rs`, one segment, 2,981,748 rows:

| | | a row |
|---|---|---|
| `run()`, `rapor`, relevance | 64.3 ms | **145.5 ns** (442,112 visited) |
| sequential scan of **everything** — read | 22.6 ms | 7.6 ns |
| — read and fold | 95.3 ms | 32.0 ns |
| — read, fold, search | 120.8 ms | 40.5 ns |
| — search, names **stored folded** | **24.7 ms** | **8.3 ns** |

Two findings, and neither was where anyone was looking.

**Folding is three quarters of the inner loop.** 24.4 ns of 40.5, paid on every
candidate row of every query, to compute something that never changes. Everything
does not do this — it keeps names in the form it searches.

**The trigram filter is not paying for itself.** It cuts 2.98 M rows to 442 K —
6.7× — and each surviving row then costs 3.6× more, because a block walk jumps
around the name arena where a full scan streams it. Net, it is worth 1.9×:
121 ms against 64 ms.

Put together: **a folded arena makes an unfiltered scan of three million names
cost 25 ms** — less than the filtered walk costs today. Everything scans about
a million names in ten; this is the same speed per name. The ceiling is not
where it looked.

What it costs: a second arena, about 79 MB here, and a format bump. What it
buys is the difference between a search box that is fast on a benchmark and one
that is fast under a person's hands.

## 2026-08-04 — the folded arena

The previous section measured the ceiling and named the change. Done: a second
name arena, folded when the segment is written, with the same row numbering.
A search walks that one and folds nothing; the spelled arena is read only for
the rows that reach the screen.

### Warm, one segment, 2,987,722 entries

| query | before | after | a visited row |
|---|---|---|---|
| `rapor` | 77.0 ms | **28.1 ms** | 192 → **64 ns** |
| `config` | 68.6 ms | **27.5 ms** | 192 → 57 ns |
| `belge` | 49.8 ms | **14.8 ms** | → 47 ns |
| `toki` | 37.5 ms | **12.2 ms** | → 40 ns |
| `main` | 12.2 ms | **9.9 ms** | → 37 ns |

**2.5 to 3.4 times**, and the per-row number is the one that matters because it
is the part that scales.

Typing, as the window actually issues it — the stored order below three
characters, relevance from there:

```
t     (modified)    13.7 ms
to    (modified)   132.1 ms
tok   (relevance)   31.2 ms
toki  (relevance)   17.2 ms
```

And whole words, which is what a search box mostly sees:
`fatura` 1.65 ms, `değişiklik` 3.25 ms, `sunum` 8.1 ms, `belge` 20.9 ms.

### What it cost

The index went from 174 MB to **253 MB** for 2.99 M entries — 79 MB, exactly
the size of the names, as predicted. Format 6.

### Two things it does not fix

**`t` by relevance is still 1.6 s**, and worse than before, because the walk is
no longer the expensive part: a term that matches a million rows pushes a
million sort values into a vector before selecting forty. The window does not
issue that query — below three characters it asks for the stored order — but
the CLI will, and the honest fix is a bounded relevance that stops and says it
stopped, the way counting already does.

**The trigram filter still does not pay for itself.** It was measured at 1.9×
before and the folded arena makes a plain scan cheaper, so the case for it is
weaker now, not stronger. Worth re-measuring against a scan with no filter at
all before it is kept.

## 2026-08-04 — four reasons the service burned a core, none of them the search

The engine answers in tens of milliseconds and the window still felt laggy, so
the question moved from "how fast is a query" to "what is the service doing
when nobody asked it anything". Answer: 88% of a core, idle.

### It was indexing itself

Of the 247 files changed under the home directory in one minute on an idle
machine, **171 were in `~/.local/share/scour`** — the index. A commit writes
segment files, the watcher sees them, the engine turns them into entries, and
committing those writes more segment files.

`apps/scourd/src/wire.rs` excludes the index directory, and it belongs there
rather than in the platform defaults because only that file knows where the
index went. `the_index_never_indexes_itself` is the test.

### `stats()` walked every row, once a second

The compaction check added earlier asks `stats()` after each commit. `stats()`
counted directories by visiting **every row of every segment** — 2.1 M rows a
second, one whole core, and the read lock held against every search while it
happened, to answer a question about the *segment count*.

`Live` now counts its directories once when the segment is opened.

### The watcher `stat`ed every event it was about to discard

Filtering events by the scan's exclusions was right; doing it with `is_dir()`
was not — one syscall for every file a compiler writes, to decide to throw the
event away. `Rules::excludes_path` answers from the path alone.

### A commit per second, for one file

A browser cache touching one file a second produced one segment a second, for
as long as the machine was on: 35 segments in 35 seconds on an idle desktop,
each holding a row or two, each one more thing every future query has to open.

A trickle now waits for a slower clock (`commit_idle`, 15 s) and a burst does
not (`commit_batch`, 64 changes). Whichever comes first, so nothing waits
indefinitely.

### And `target/` was never excluded

The exclusion list's own comment called it "the single loudest source of noise
in a developer's home directory" and did not contain it. **852,437 of 2,986,545
entries** — 28% of the index, none of it written by anyone, and while a compile
runs the watcher turns it into a flood: 3,935 changes queued and the service at
67% of a core. Excluded by name, and `exclude.allow` takes it back.

`build`, `dist` and `out` are deliberately left in: another 249,445 entries, but
they are plausible names for real work in a way `target` beside a `Cargo.toml`
is not.

### Where it landed

| | before | after |
|---|---|---|
| entries | 2,986,545 | **2,133,356** |
| index on disk | 253 MB | **162 MB** |
| service, idle | 88% of a core | **16%** |

| query | |
|---|---|
| `fatura` | 1.2 ms |
| `toki` | 9.1 ms |
| `main` | 11.2 ms |
| `config` | 16.3 ms |
| `belge` | 18.3 ms |
| `rapor` | 34.6 ms |
| `t` (stored order) | 68 ms |
| `to` (stored order) | 129 ms |

**Still not finished.** 16% of a core at rest is not zero, and the two-character
case is the worst thing left on the list: `to` matches so much that the stored
order walks two million rows before it has a page. Both are measured, neither
is guessed at.

## 2026-08-04 — a query waited for the whole rebuild

Steady state was fine and the window still felt wrong, so the thing to measure
was not the average. Forty seconds of one query every 150 ms, with a rebuild
started three seconds in:

| | before | after |
|---|---|---|
| queries that completed | 26 | **231** |
| median | — | **1.8 ms** |
| **worst** | **22,984 ms** | **2,718 ms** |
| over 50 ms | 4 of 26 | 8 of 231 |

A search box that is usually instant and occasionally twenty-three seconds is
not a fast search box, and no amount of shaving the inner loop shows up next to
that.

`maintain` held the **write** lock from the first byte to the last, and every
search takes the read lock. It does not have to: a segment is written once and
never edited, so reading N of them and writing one more touches nothing a
search looks at. Only the *list* changes, and swapping a list is microseconds.

So the merge builds under the **read** lock — which searches also hold, and
therefore do not queue behind — and the write lock is taken once at the end.
Two things had to change with it: `groups` answers in segment *numbers* rather
than positions, because a position means nothing once the lock has been let go,
and the swap re-checks which of those numbers are still there.

**2.7 s is still not right**, and the remaining holder is `flush`: it builds and
writes a segment for the staged rows with the write lock held. The same fix
applies and is not done here.

## 2026-08-04 — the count cap was the whole cost of a short query

The tail was not where the median was. Ninety seconds of a realistic typing mix
on a busy machine: p50 29.9 ms, p90 110, **p99 1,052, worst 6,674** — and the
five slowest were `ra`, `fa`, `fa`, `bel`, `f`. Every one a prefix somebody was
still typing.

Splitting it by query separated two effects that had been read as one:

| query alone, 45 s | p50 | p99 | max |
|---|---|---|---|
| `rapor` | 32.9 ms | 56.7 ms | 2,084 ms |
| `ra` | 99.8 ms | 953 ms | 2,610 ms |
| `f` | 320.9 ms | 11,336 ms | 11,336 ms |

So short prefixes are slower *by construction*, and there is a contention tail
on top of everything — `rapor`'s p99 is 57 ms and its worst is two seconds.

### And the construction was the count, not the search

| | rows visited | |
|---|---|---|
| `f`, cap 100,000 | 157,341 | 65.4 ms |
| `f`, cap 1,000 | 2,674 | **1.3 ms** |
| `ra`, cap 100,000 | 1,208,951 | 23.1 ms |
| `ra`, cap 1,000 | 35,743 | **0.9 ms** |
| `rapor`, either | 434,304 | 11.7 ms |

The window asked the walk to count up to a hundred thousand matches so the
meter could show an accurate total. For `ra` that meant visiting **1.2 million
rows** — to print a number nobody reads while still typing.

A thousand while typing, and the exact figure asked for separately once the
list is already on screen, on the same lane as the facet count. The meter says
`1000+` until then, which is true.

Measured back to back, same machine, same typing sequence `f`→`fatura`:

| | n | p50 | p99 | max |
|---|---|---|---|---|
| cap 100,000 | 1,586 | 3.0 ms | 86.6 ms | 4,995 ms |
| **cap 1,000** | **6,639** | **1.8 ms** | **18.7 ms** | **1,127 ms** |

Four times as many queries answered in the same thirty seconds, and the p99 is
a quarter of what it was.

### What is left, precisely

The contention tail. `rapor`'s worst is still seconds while its p99 is tens of
milliseconds, and the holder is `flush`: it builds and writes a segment for the
staged rows with the write lock held. `fold` was fixed the same way in the
previous commit and `flush` was named there as the next one.

## 2026-08-04 — key to pixels, which is the only latency anyone feels

Every number before this one was measured inside the service. This is the one
the hands see: from the keystroke to the row appearing. Measured by having the
window type into itself a character at a time, because nothing outside it can
see both ends.

| | key → pixels, typical |
|---|---|
| 180 ms debounce, 200 rows | 31 ms |
| 25 ms debounce, 200 rows | 9–14 ms |
| **no debounce, 60 rows** | **3.2 ms** (median of 144, worst 99) |

Two things were mine and both had stopped being true.

**The debounce.** 180 ms was right when a query cost 90 and eight were in
flight at once; at 2 ms a query costs less than the wait does, so waiting to
avoid one spends more than it saves. Key to pixels was 31 ms and **25 of them
were the timer**. Removed — the generation guard drops a stale reply whether or
not a timer ran, which is what makes it safe.

**Two hundred rows a keystroke.** The screen holds twenty. Each row costs a
path rebuilt in the engine and six strings allocated in the window, for rows
nobody scrolls to before typing the next letter. Sixty is three screens.

### Start-up

| | |
|---|---|
| window built | ~110 ms |
| first search sent | ~120 ms |
| first rows on screen | **204–248 ms** |

The 110 ms is Slint creating a window and there is nothing here to shave off
it. One sample of three took 1.8 s, which was the service being busy rather
than the window being slow.

### And the page size comes from the window

Sixty was a guess too. The window reports what fits — `visible-rows`, the
height divided by the row height plus a dozen to scroll into — because only it
knows how tall it is and that changes when somebody drags the edge. At the
default size that is **31 rows**, not 200.

| rows a keystroke | median key → pixels |
|---|---|
| 200 | 9–14 ms |
| 60 | 3.2 ms |
| **31 (what fits)** | **2.9 ms**, p90 15.6, worst 72.6 |

`PAGE_MAX` of 120 is the ceiling, so a maximised window on a tall screen cannot
turn one keystroke into a thousand rebuilt paths.

## 2026-08-04 — the trigram filter, judged instead of assumed

This file twice recorded that the filter was "not paying for itself" and both
records were wrong. `SCOUR_NO_TRIGRAM=1` now exists so the question is settled
by running it. 2,137,518 entries, one segment, quiet machine, twenty rounds:

| term | with | without |
|---|---|---|
| `rapor` | **14.11 ms** | 37.97 ms |
| `belge` | **9.22 ms** | 37.70 ms |
| `config` | **8.91 ms** | 36.48 ms |
| `main` | **5.62 ms** | 36.23 ms |
| `fatura` | **0.85 ms** | 35.02 ms |
| `kütüphane` | **0.06 ms** | 35.50 ms |

Between 2.7× and 590×. What made the earlier arithmetic wrong: the first
comparison was against a sequential scan measured *before* names were stored
folded, and the second against a busy index with five segments and a watcher
running, where the filtered walk was paying for lock contention rather than for
cache misses. A switch costs less than either mistake.

### What the meter says

| | |
|---|---|
| `kütüphane` | 0.000022 s |
| `fatura` | 0.000743 s |
| `değişiklik` | 0.002706 s |
| `main` | 0.004763 s |
| `belge` | 0.007861 s |

Google's famous line said 0.42 seconds.

### And deleting a folder no longer walks the index

`flush` rebuilt a path for every live row of every segment and compared
strings, to find the rows under a removed prefix — 2.1 M path constructions to
delete one folder, with the **write** lock held and every search waiting behind
it. The directory table answers the same question as a range check on a column,
which is what `under:` has always used.

One thing had to be added to keep it correct, and the test found it rather than
the reasoning: a directory's own row lives in its *parent*, so it carries the
parent's number and the range walks past it. `/home/u/Projeler` survived the
removal of `/home/u/Projeler`. The parent's number and the last component are
checked too, and a name is read only for the handful of rows that sit there.

## 2026-08-04 — the commit stopped holding the index while it wrote

`fold` was fixed two commits ago and `flush` was named there as the next one.
Everything a flush does except building and writing the segment is bookkeeping
the lock has to cover; the build and the write are the seconds.

So the staged entries are lifted out under the lock — they were never
searchable, so no answer changes — and the segment is built and written with it
released. Splitting it needed care the reasoning did not supply and two tests
did: the identities to kill are read *from* the staged entries, so taking them
out first left a re-indexed file with its old row still alive.

Measured during a **full rescan**, which is the heaviest write there is —
2.1 M entries re-indexed while queries run:

| | |
|---|---|
| queries completed | 392 |
| p50 | **1.98 ms** |
| p99 | 912 ms |
| worst | **1,061 ms** |

The tail across the three fixes, each measured under the load that provokes it:

| | worst |
|---|---|
| before any of it, during a rebuild | 22,984 ms |
| after `fold` | 2,718 ms |
| after `flush`, during a *full rescan* | **1,061 ms** |

What is left in the lock during a scan is `kill_ids`: every upsert kills the old
row of the same identity, and with a hundred thousand staged that is a merge
against every segment. It edits the bitmap a search reads, so it cannot simply
move out — it would need the bitmap swapped rather than mutated.

## 2026-08-04 — where the remaining nanoseconds are, and where they are not

`rapor` visits 433,280 candidate rows. Split by hand on a copy of the index:

| | a candidate row |
|---|---|
| walk the folded arena and test the needle | **15.2 ns** |
| `run()` by kind | 21.8 ns |
| `run()` by relevance | **25.6 ns** |
| `run()` in stored order | 26.0 ns |
| `run()` by size | 28.8 ns |

The machinery — the alive bit, the clause dispatch, the column read a sort
needs — costs about **ten nanoseconds** over the bare loop, and every sort
costs the same. There is nothing left to win here.

**And relevance is not special**, which took a third wrong guess to establish.
The first run of this measurement showed relevance at 109.9 ns against 22.0 for
the stored order, and the difference was entirely the first `run()` in the
process paying a page fault per mapped page it touched. Warm, they are 25.6 and
26.0. The example now says to read the second figure.

### So the lever is selectivity, not speed

433,280 candidates for 15,349 matches is a **3.5% hit rate**. A block is 128
rows and it is a candidate if the whole block's trigram set contains the term's
three keys — with 128 names in a block, false positives are the normal case,
not the exception.

Fewer rows a block would mean fewer wasted candidates and a larger trigram
index. `BLOCK` is 128 and it is shared with the columns and the name arena, so
it is a format change and a real experiment rather than a tweak. **Not done**,
and it is the next real lever: it is the number that decides how a bigger index
behaves, which is exactly what this is being optimised for.

## 2026-08-04 — the block was sized for packing and decides selectivity

`BLOCK` had been 128 since the layout was designed, chosen because a larger
block amortises the per-block minimum and width further and because 128 is the
width SIMD bit-packers use. Both true, and both about *packing*.

What nobody had measured is that the block is the unit the trigram filter and
the zone map can skip, and a block survives if **anything** in it might match.
128 names to a block hands a term 128 rows for every one that could be right.

Over 750,717 real entries and six real terms, one full scan and one measurement
per size:

| rows a block | index | candidates | query |
|---|---|---|---|
| 128 | 51 MB | 267,296 | 5.76 ms |
| 64 | 53 MB | 169,248 | 3.86 ms |
| **32** | **57 MB** | **104,192** | **2.99 ms** |
| 16 | 65 MB | 60,352 | 2.16 ms |

Per megabyte spent: 128→64 buys 0.95 ms, 64→32 buys 0.22, 32→16 buys 0.10.
**Thirty-two is the knee.** Format 7 — every offset in every file is relative
to the block, so an older index decodes into noise rather than into an answer.

### On the live index, 2,137,224 entries

| | before | after |
|---|---|---|
| `rapor` | 43,726 µs | **24,498 µs** |
| `değişiklik` | 2,706 µs | **821 µs** |
| `fatura` | 743 µs | **416 µs** |
| `belge` | 7,861 µs | **10,058 µs** |
| index on disk | 162 MB | 183 MB |

This is the lever that decides how a *bigger* corpus behaves, which is what it
was chosen for: the per-row cost has been measured flat at about 25 ns whatever
the sort, so what grows with the index is the number of candidates a term walks
and nothing else.

One test had to change with it, and it is worth saying which way. The synthetic
column test asserted under 9.6 bytes an entry and now measures 23.30, because
fifteen of its sixteen columns are constant and the per-block overhead *is* the
file at that shape. The real corpus went from 51 MB to 57 for the same rows.
The bound was raised to 26 and the reason written beside it rather than the
figure quietly adjusted.

## 2026-08-04 — what a commit was actually holding

`SCOUR_LOCK_TRACE=1` prints how long a commit held the write lock, because the
last three things blamed for this tail were each the wrong one. On the live
index it said:

```
commit held the index for 55ms (3 staged)
commit held the index for 67ms (5 staged)
```

**Three staged entries, fifty-five milliseconds, once a second.** Split by
phase, the answer was in neither of the two places it was expected:

```
kills 0 µs · 2 alive bitmaps 33162 µs
kills 1 µs · 4 alive bitmaps 55912 µs
```

### The kills — fixed, and the fix was a second strategy

`kill_ids` merged the wanted identities against the segment's whole table,
which is right for the hundred thousand a bulk pass hands over and absurd for
the three a watcher does: a merge is `O(rows in the segment)` however few are
wanted, because it advances until it passes the last of them. Below the
crossover — `wanted × log rows` against `rows` — it is now a binary search per
identity, which is what the table is sorted for. **0 to 1 µs.**

### The bitmaps — the real holder

`save_alive` replaces the live bits of every touched segment: 262 KB and an
`fsync`, **13 ms a segment**, three or four segments, every second. Copied
under the lock now and written outside it. A crash between the two leaves the
older bitmap, which is exactly what a crash before the write always did.

### Where it lands

| | during a full rescan | normal use |
|---|---|---|
| queries in 60 s | 643 | **735** |
| p50 | 1.35 ms | **1.29 ms** |
| p99 | 329 ms | **3.28 ms** |
| worst | 924 ms | **23.0 ms** |

The whole tail, across four commits, each measured under the load that provokes
it:

| | worst |
|---|---|
| a rebuild, before any of it | 22,984 ms |
| after `fold` left the lock | 2,718 ms |
| after `flush` left it | 1,061 ms |
| after the kills and the bitmaps | **924 ms rescanning, 23 ms in use** |

What is left is the merge itself during a bulk pass, which is the case it was
written for and costs what it costs.

## 2026-08-04 — two size ideas, both measured, both refused

### Storing the name once

`names` and `fnames` hold the same names twice, spelled and folded, and the
obvious saving is to keep one copy wherever the two are identical. Measured
over 1,286,685 real rows:

| | |
|---|---|
| folded is identical to spelled | 586,783 — **45.6%** |
| differs | 699,902 — 54.4% |
| two arenas today | 67.4 MB |
| one arena plus the exceptions | 56.2 MB |
| **saving** | **11.2 MB** |

Over half the names differ, which is what a machine with Turkish documents and
CamelCase source looks like. 11.2 MB is 6% of a 186 MB index, and the price is
a per-row question in the hot loop — "is this one an exception" — on the walk
that everything else has been spent making tight. **Refused.**

It would not be faster either: the walk only ever touches the folded arena, and
the spelled one is read for the thirty rows on screen.

### zstd

The ratios are large and mostly unclaimable:

| | raw | zstd -9 |
|---|---|---|
| cols | 23.7M | 9.6M (−60%) |
| names | 33.9M | 7.6M (−77%) |
| fnames | 33.4M | 7.3M (−78%) |
| tpost | 16.9M | 9.2M (−46%) |
| ids | 9.8M | 8.7M (−11%) |
| **total** | **120.3M** | **43.1M (−64%)** |

Every one of those files is mapped and read in place. Compressing them means
decoding to read: at 32 rows a block, a query touching 104,000 candidates would
decode 3,250 frames, which is more than the whole query costs now. The design
has spent this entire session trading bytes *for* time — 79 MB on a folded
arena to save 24 ns a row — and this is the same trade backwards.

`tpost` is the one honest candidate: trigram lists are read a few times a query
rather than once a row, so decoding them is bounded. Eight megabytes. Not
pursued.

Disk is not the binding constraint here — 186 MB for 2.1 M entries is the same
order as Everything's index for a million — and latency is.

## 2026-08-05 — what a `rm -rf` costs a commit

The watcher reports one removed path per file and one per directory, and a
commit lands once a second, so a few thousand of them arrive in one batch. Every
one of those was checked against every live row of every segment, with the write
lock held.

```bash
cargo run --release -p scour-index-native --example removal
```

One million rows in one segment. The commit alone, so the number is comparable
across index sizes.

| removed paths in the batch | before | after | |
|---|---|---|---|
| 1 | 14.89 ms | 14.18 ms | the floor: a pass over a million rows |
| 16 | 20.77 | 16.22 | |
| 256 | 174.24 | 27.68 | 6.3× |
| 1,024 | 567.42 | 41.59 | 13.6× |
| **4,096** | **2.24 s** | **77.52 ms** | **29×**, and 4,096 is the engine's batch |
| one prefix over the whole subtree | 15.08 | 16.35 | unchanged, as it should be |

547 µs a path became 18.9.

The change is to ask the question from the path rather than from the list: a
path has a dozen ancestors however many thousand paths were removed, so the
check stops growing with the batch. Within a segment the prefixes additionally
become merged ranges of directory numbers, plus a list sorted by parent for the
rows a range cannot reach.

**The obvious version is wrong.** Sorting the removed paths and binary-searching
for the greatest one not after the row's path misses matches, because sort order
does not put an ancestor beside its descendant: with `/pkg/lib` and
`/pkg/lib-old` both removed, `/pkg/lib/deep/f.rs` sorts *after* `/pkg/lib-old`
— `-` is 0x2D and `/` is 0x2F — so the single comparison lands on the member
that does not match. Caught by `the_fast_answer_agrees_with_the_slow_one`,
which asks both ways about 768 probes.

## 2026-08-05 — does live updating work on ntfs3?

The configuration had `watch = false` for the NTFS volume, on two grounds
written into the comment beside it. Both were wrong.

```bash
cargo run --release -p scour-source-fs --example canwatch -- /mnt/depo
# /mnt/depo: watched, 6.3s to install
#    events arrive — live updates will work here
```

"A minute of setup" is 6.3 seconds for 177,915 directories. "The volume only
changes when Windows is running" is not true of a volume mounted `rw` and
written to from both sides.

`canwatch` grew the second line for this. A watch being *accepted* and a watch
*reporting* are different questions — network mounts and FUSE accept one and
report nothing — so it now writes a file under the path, waits to be told, and
removes it.

With `watch = true`, against the live service, each step checked in the index:

| from Linux, on the NTFS volume | result |
|---|---|
| `cp` a file in | indexed, size 100,000 correct |
| rewrite it in place, 100 KB → 300 KB | size **and** mtime updated |
| `mv` to a new name | old name gone, new name present |
| `cp -r` a tree in | all three nested files indexed |
| `mv` a directory **within** the volume | contents follow to the new path |
| `rm -rf` | gone |

The directory move is the one worth naming: renaming a populated directory
produces no event for anything inside it, so the contents keep their old paths
unless the rename is treated as "walk this". It is — `Modify(Name(_))` counts
as fresh, exactly like a create.

What a watch still cannot see is a change made while Linux is not running.
That is what `scan.on_start` is for, and the two together are the whole
picture. Cost of both: **419,574 inotify watches of 524,288**, 80% of the
per-user limit.

## 2026-08-05 — could the NTFS volume be read from `$MFT` instead of walked?

Everything's speed on Windows comes from not walking directories at all: it
reads the Master File Table, which is one sequential structure holding every
name and every piece of metadata on the volume. The question is whether that
is worth doing here.

**It is possible, and it does not need root.** `ntfs3` exposes the table as an
ordinary file:

```bash
ls -la '/mnt/depo/$MFT'
# -rwxr-xr-x 1 hasan hasan 2213543936 ... /mnt/depo/$MFT
```

2.06 GiB, first four bytes `FILE`, 2,161,664 records of 1,024 bytes.

| | |
|---|---|
| `find /mnt/depo -xdev`, no `stat` | 2,722 ms |
| `find /mnt/depo -xdev -printf '%s %T@'` | 12,177 ms |
| **Scour's parallel walk, with metadata** | **6,089 ms** — 1,387,173 entries |
| **reading all of `$MFT`** | **1,210 ms** — 1.83 GB/s |

All warm. The middle rows are the point: the metadata is nine and a half
seconds of single-threaded `stat`, and the walk is fast because it does that on
every core at once. An MFT record already *contains* the size, all three
timestamps, the attributes, the name and the parent reference — so the whole
`stat` storm goes away rather than being parallelised.

A realistic MFT scan is the 1.2 s read plus parsing 2.16 M records and
assembling paths from parent references. Call it **~2 s against 6.1 s**, and
wider than that cold, because one sequential 2 GB read is the access pattern a
disk likes and random metadata across a volume is the one it does not.

**Not built, and the reason is the size of the prize rather than the
difficulty.** The walk runs once, at start-up; inotify covers everything after
it, on ntfs3 as well as ext4. So this buys about four seconds a boot, in
exchange for:

* an NTFS parser, which is one filesystem out of the ones a `Source` may face;
* a *second* implementation for Windows, where `C:\$MFT` cannot be opened this
  way — it needs `FSCTL_GET_NTFS_FILE_RECORD` and Administrator;
* a dependency on the mount exposing system files, which these options do and
  others do not;
* a torn snapshot, since the volume is mounted `rw` while it is read;
* **more** filtering, not less: the walk prunes `target/` and never descends,
  while the table hands over all 2,161,664 records including the 303,658
  entries under excluded directories, to be thrown away afterwards.

### The cold measurement, which was supposed to change the answer

It does not. Caches dropped, then the same walk:

```bash
sync && echo 3 | sudo tee /proc/sys/vm/drop_caches
time find /mnt/depo -xdev -printf '%s\n' > /dev/null
# 11.98 s wall — usr 0.39, sys 3.86
```

**11.98 s cold against 12.18 s warm**, which is the same number. The walk on
this volume is not waiting for the disk; it is paying for syscalls — 3.86 s of
system time for roughly 1.5 M `getdents` and `stat` pairs. Scour's walk is
faster than `find` for exactly that reason and no other: it spreads the same
syscalls across every core.

The MFT read, measured the same way:

| | |
|---|---|
| first pass, off the disk | 1.24 s (1.79 GB/s) |
| second pass, out of the page cache | 0.27 s (8.22 GB/s) |

So the ratio is what it was warm, and the argument that a cold boot would widen
it is simply wrong here. It was the one thing offered as able to reverse the
decision, it was tested, and it did not.

What is left of the case is narrower and worth stating exactly: the walk's cost
is CPU across cores, so the fewer cores a machine has, the better the table
looks — and on a disk where seeks are real rather than an NVMe, the 1.24 s
would hold while the 12 s would not. Neither describes this machine.

`Caps::JOURNAL` exists in the trait for exactly this shape of source; nothing
has to be redesigned to add it later.

**The USN journal is the more interesting half and is out of reach here.** It
is a change log, so it answers "what happened while Linux was not running" —
which is the only thing the boot walk exists for — in a fraction of the data.
But `$Extend/$UsnJrnl` reads back as zero bytes and its `$J` stream is not
exposed under these mount options, and reaching it another way means the raw
partition, which means root.

## 2026-08-05 — is the filesystem table right?

`FsTraits` is read from a table of `statfs` magic numbers, and a wrong row
there is not a crash. It is a silently wrong index: a claimed `stable_ids`
where `st_ino` is invented makes every file its own duplicate after a remount,
and a claimed `case_sensitive` where the filesystem folds makes `Rapor.pdf` and
`rapor.pdf` two rows for one file. Neither raises anything.

The table had never been checked against a filesystem. Nothing on a developer
machine is mounted FAT, which is the row that matters.

```bash
sudo bash scripts/fsmatrix.sh          # builds each format in /dev/shm
```

Each image is made in RAM, mounted, probed by writing files, and unmounted.
`claimed` is the table; `measured` is what the filesystem did.

| | claimed | measured |
|---|---|---|
| ext4 | `case=yes ids=yes` | `case=yes ids=yes rename=yes` |
| ext2 | `case=yes ids=yes` | `case=yes ids=yes rename=yes` |
| btrfs | `case=yes ids=yes` | `case=yes ids=yes rename=yes` |
| xfs | `case=yes ids=yes` | `case=yes ids=yes rename=yes` |
| **vfat** | `case=NO ids=NO` | `case=NO ids=yes rename=yes` |
| **exfat** | `case=NO ids=NO` | `case=NO ids=yes rename=yes` |

Plus btrfs, ntfs3 and tmpfs as they are mounted on this machine, all agreeing.
Nothing claimed more than the filesystem offers, which is the only direction
that is a defect — claiming less is what `FsTraits::UNKNOWN` documents.

### The `ids` column was asking a weaker question

`stable_ids` says `st_ino` "is stored on disk and **survives a remount**". A
probe running as an ordinary user against something already mounted cannot see
past its own session, so `ids=yes` on vfat above means "distinct, and stable
across a rename" — the same shape, a weaker claim. Only root can unmount, so
the script asks properly: fifty files, their numbers, unmount, mount, compare.

| format | ids survive a remount | ids survive a move |
|---|---|---|
| vfat | **0/50** | **NO** |
| exfat | **0/50** | **NO** |
| ext4 | all of them | yes |

Not one. FAT regenerates every number, and moving a file between directories
changes it without a remount at all — the number comes from where the directory
entry sits on disk, and both operations move it.

So `stable_ids: false` for the FAT family is correct, and `entry_of` falling
back to `EntryId::path_hash` is what keeps a USB stick from doubling its own
index on the second scan. The comment there said "can change"; it changes.

(The control row read `49/50` the first time. That was the harness, not ext4 —
the file it moves was inside the compared set. It has its own file now.)

## 2026-08-05 — the sidebar cost more than the list beside it

The design's claim is that a facet count can be recomputed on every keystroke.
It could not, and the reason was not the counting.

`run_with` narrows before it walks: the trigram index says which blocks could
hold the text and the zone map which could satisfy the numbers, and only what
survives is opened. `for_each_match` — behind every facet and every exact
count — did none of that. It walked every row of every segment.

It also handed `accepts` the **spelled** name where a search hands it the
folded one, so `RAPOR.pdf` did not match `rapor` in a facet while it did in
the list. A wrong answer rather than a slow one, and invisible: the sidebar
just counted low.

Both fixed by giving them one walk to share, `search::walk_matches`.

| query | facet before | after |
|---|---|---|
| `rapor`, by kind | 37 ms | 26 ms |
| `rapor`, by age | 39 ms | 24 ms |
| `ext:rs`, by kind | 38 ms | 43 ms |
| empty, by kind | 22 ms | 70 ms |

Warm, fourth run of each, 2.1 M entries. The gain is where there is text to
narrow **by** — `rapor` is a third cheaper. `ext:rs` and the empty query have
no text term, so the trigram filter has nothing to say and the walk is the
walk; the numbers move with the cap, not with the narrowing.

And the totals now agree, which is the part that matters more than the
milliseconds:

| query | search | facets |
|---|---|---|
| `rapor` | 15,349 | 15,349 |
| `RAPOR` | 15,349 | 15,349 |
| `kind:doc rapor` | 9,453 | 9,453 |

### What is still slow, stated rather than left to be found

The empty query's age chart is **782 ms**. It is the one place where nothing
can narrow — no text to filter blocks by — and the scan is deliberately
uncapped because a sampled distribution of a date-ordered index is a picture
of its recent end (see the previous section). It runs once when the page opens
and again if a query is deleted back to empty, not per keystroke.

Two ways out, neither taken yet:

* **One walk for three questions.** The exact count, the kind rail and the age
  chart each walk the same matching set separately. Answering them together is
  three times less work for every query, not just this one.
* **Read the distribution off the zone maps.** Rows are stored in date order,
  so the count newer than a given day is a binary search over the per-block
  minima rather than a walk — for the empty query, exact and effectively free.
  It stops being that simple the moment a query filters, so it would be a fast
  path rather than the answer.

## 2026-08-05 — an executable bit the mount invented

`kind_of` calls a file executable when `st_mode & 0o111` is set, which is right
where the mode is the file's own. On the NTFS volume it is not, for almost
everything on it:

| | `kind:exec` |
|---|---|
| `under:/home/hasan` (btrfs) | 21,342 |
| `under:/mnt/depo` (ntfs3) | **293,811** |
| `under:/mnt/depo` after | **6,897** |

A whole volume in the wrong category, and `kind:exec` useless for finding a
program.

The reason is finer than "NTFS has no modes", and worth writing down because
the obvious version is wrong. Measured:

```bash
echo x > /mnt/depo/probe && stat -c %a /mnt/depo/probe   # 644
chmod 600 /mnt/depo/probe && stat -c %a /mnt/depo/probe  # 600
```

`ntfs3` **does** keep a mode for a file created under Linux. What it cannot do
is invent one for the files Windows wrote, and those fall back to `fmask` off
the mount line — `/mnt/depo` is mounted `fmask=0022`, so everything Windows put
there reads 0755.

So `FsTraits` gains `real_modes`, alongside `stable_ids` and `case_sensitive`
and for the same reason: the filesystem's promise decides, not the platform's.
`entry_of` replaces the invented mode with `0o100644` (`0o040755` for a
directory), which is also what it means — no permissions of its own.

The cost is a genuinely `chmod +x` script on such a volume not being called
executable. Against 286,914 files that are not executable and said they were,
that is the cheaper mistake by five orders of magnitude.

The `filesystems` example probes it, and reports what it can honestly measure
— whether a `chmod` sticks, which on ntfs3 is `yes` even though the flag is
`no`. The two are different questions and the table says both.

## 2026-08-05 — three questions, one walk

The sidebar asks three things about the same rows: how many matched, what
kinds they are, how old they are. Each was a request, and each walked the
matching set again.

`FacetRequest.by` is a list now, `FacetResponse` carries a group per question
and the total, and the index answers them all from one `for_each_match`. No
cleverness: the walk was always the cost and the counting never was.

Warm, fourth run of each, 2.1 M entries, the whole sidebar:

| query | three requests | one |
|---|---|---|
| `rapor` | 26 + 24 + 28 = 78 ms | **25 ms** |
| `ext:rs` | 43 + 42 + 39 = 124 ms | **42 ms** |
| `kind:image` | 86 + 90 + ~40 = 216 ms | **48 ms** |
| empty | 70 + 782 + ~40 = 892 ms | **119 ms** |

The page makes two requests per settled query now — the rows, then everything
else — where it made four.

### The third idea is no longer worth doing

The plan after this was to read the age distribution off the block zone maps,
because rows are stored in date order and a count newer than a given day is a
binary search rather than a walk. It was aimed at the empty query's 782 ms.

That number is 119 ms and it now also carries the count and the kind rail, all
of it behind the rows rather than in front of them. What is left to win is a
fraction of a background request, against a fast path that stops being valid
the moment a query filters. Not built, and this is why.

### What is left, and it is not speed

**A browser will not make an element taller than about 33.5 M pixels.** At
31px a row that is 1.08 M rows, and the empty query is 2.14 M. Past the ceiling
the scroll position stops mapping to a row.

So above it the rows are packed at whatever pitch does fit — the bar still
spans the whole set and dragging it still lands where it points; what is lost
is that a wheel notch covers more rows than it looks like it should. The
alternative is a scrollbar that lies about how much is left.

Verified exactly for `ext:rs` (89,702 rows, uncompressed): jumping to row
20,000 shows what the service returns for that offset. **Not verified for the
compressed path** — the browser harness could not be made to hold a synthetic
scroll position steady long enough to read it back, and a claim without a
measurement is an opinion.

## 2026-08-05 — the columns nothing could ask about

Every index since the first version has stored `mode`, `uid`, `gid` and
`items`, and no query could name any of them. They cost nothing to keep — a
column that barely varies packs to almost zero, which the column measurement
above says in numbers — so what was missing was only a way to say it.

Which matters for the two callers that are not a search box. A sysadmin asks
`find` for the world-writable file and the setuid binary; a model driving MCP
asks the same questions in the same words. Both were being answered by walking
the filesystem when the answer was already in the index.

Nine fields, one new `Match` variant and one new `Test`, because the whole
family is *a column, masked, compared*:

| query | mask | want | any |
|---|---|---|---|
| `perm:644` | `0o7777` | `0o644` | no |
| `perm:-200` | `0o200` | `0o200` | no |
| `perm:/222` | `0o222` | — | yes |
| `node:l` | `0o170000` | `0o120000` | no |
| `suid:` | `0o4000` | `0o4000` | no |

Checked against `find` on the live index:

| | `find` | `scour` |
|---|---|---|
| symlinks under `/home/hasan` | 7,869 | 7,854 |
| owned by uid 0 | 45 | **45** |
| sockets | 320 | **320** |

The fifteen missing symlinks are all under `target/` and `node_modules/`,
which the index excludes and `find` does not — verified by counting them.

Two things the language had to be told rather than guessing:

* **`type:` was already taken**, and rightly — it has meant `kind:` since the
  language was written. `kind:` is what a file *is*; `node:` is what the
  filesystem *made* it, and only the second tells a symlink from its target.
  The test that refuses a duplicate spelling is what caught it.
* **Permission bits are only as true as the filesystem.** The NTFS work above
  withholds an invented mode at index time, so `perm:` and `suid:` find
  nothing on such a volume rather than everything. `node:` is unaffected.

## 2026-08-05 — three more, and what each cost

Aimed at the two callers that are not a search box: a model driving MCP and a
person who knows `find`. Familiar spellings, because the point is that neither
has to learn anything.

**`depth:`** — `/` counted from the root. The directory table is front-coded,
so every depth comes from one sequential pass: a row is its predecessor
truncated and extended, and carrying the slash positions along means a row
costs its own suffix and nothing else. 145,593 directories, checked against
counting the slashes in `DirTable::get` for every one of them:

```bash
cargo run --release -p scour-index-native --example depthcheck -- <index>
# 145593 dizin
# uyuşmayan: 0
```

Built when a query asks and never otherwise — half a megabyte is cheap once
and wasteful always.

**A bare number means *equals* here**, and the first version got it wrong by
inheriting the size convention. `size:1mb` meaning "at least" is right; nobody
looks for a file of exactly a megabyte. `depth:3` reads as "three deep" to
everyone, and taking the size rule made it match 1,000,000 rows where
`depth:<=3` matched 78. Both were working as written; one was written wrong.

| | |
|---|---|
| `depth:2` | 2 — `/home/hasan` and `/mnt/depo`, which is exactly right |
| `depth:3` | 76 |
| `depth:<=3` | 78 |

**`regex:`** — costs no new compilation: `regex-automata` and `regex-syntax`
are already in the tree through `globset`, so the crate is a façade. Priced at
60 against `NameHas`'s 10 in the plan, because nothing narrows it — a pattern
says nothing a trigram index can read.

The first version returned zero for everything, and the reason is worth
keeping: `Plan::needs_name` decides whether the walk reads the name arena at
all, and a test it does not know about is handed an empty name. It said yes
for `NameHas`, `NameGlob`, `Ext` and `PathHas`. Not for a regex. Silent, and
the kind of silent that looks like "nothing matched".

| | |
|---|---|
| `regex:\.rs$` | 89,883, against `ext:rs` at 89,702 |
| `regex:^[0-9]{4}-[0-9]{2}` | 9,622 |

**Parentheses** group only when there is a `|` inside them, and that is the
whole design. `rapor (1).pdf` and `IMG (2)` are files everybody has, so
treating every parenthesis as syntax would break searching for them. Tested
both ways round.

Nesting is not supported and the AST is why: a query is groups AND-ed together
and a group is alternatives OR-ed, which is one level by construction.
`(a|b) (c|d)` works. `(a (b|c))` would need a tree, and the day something needs
one it should get a tree rather than a parser pretending to have one.

## 2026-08-05 — how long a saved file takes to appear, and who pays for it

The list did not update itself: rows were whatever the last keystroke asked
for and stayed that way. Fixing the window was the small half. The measurement
that mattered was underneath it.

**The first reading said there was nothing to fix.** A file created in a
watched directory, timed until a search found it:

```
round 1: created -> findable   0.90s
round 2: created -> findable   0.95s
round 3: created -> findable   0.76s
```

Which looked like the commit clock working and was not. The commit rule is
`waited >= commit_interval && (pending >= commit_batch || waited >=
commit_idle)` — one second, sixty-four changes, fifteen seconds. One created
file is one change, so it should have waited the full fifteen. It did not,
because **this desktop produces 30–50 filesystem changes a second on its own**:

```
pending 36 · 50 · 17 · 18 · 32 · 32        (0.4 s apart, nothing running)
```

The sixty-four was reached by other people's churn every second or two, and
the file rode along. On a quiet machine — a laptop with nothing open — the same
file waits fifteen seconds, and no measurement taken here would ever have
shown it.

So the clock now depends on whether anybody is waiting to be told. Measured
with the two conditions interleaved, because this machine's own churn moves the
answer and two runs an hour apart would not be comparable:

| | median | worst |
|---|---|---|
| a window open and waiting | **0.82 s** | 0.99 s |
| nobody waiting | 7.41 s | 9.24 s |

The floor is `commit_interval`, and that is the honest bound: a commit writes a
segment, and this index cannot make a write visible without one.

**What the open window costs.** A segment per commit is exactly what the
batching exists to avoid, so it is worth knowing what is being spent. Sixty
seconds in each condition, twice, alternating:

| | segments | revisions | `rapor` | `ext:rs` | `kind:image` |
|---|---|---|---|---|---|
| watched | 9 → 10 | +93 | 1.7 ms | 0.6 ms | 1.8 ms |
| idle | 10 → 9 | +55 | 1.5 ms | 1.1 ms | 1.9 ms |
| watched | 9 → 9 | +106 | 1.4 ms | 0.4 ms | 1.4 ms |
| idle | 10 → 8 | +106 | 1.4 ms | 0.9 ms | 1.8 ms |

Nothing. The segment count is held at 8–10 by the merge that already runs, and
the query times are indistinguishable. Which follows from the finding above:
on a machine with churn the commits were already happening, and what changed is
that a single file no longer has to wait for company.

**Nothing polls.** `await` returns when the revision moves and not before, so
an idle desktop costs one round trip every twenty-five seconds instead of the
86,400 searches a day that asking once a second would be.

### Two client-side clocks, both wrong first

The highlight on a row that has just arrived is a 1.6 s fade, and it took two
measurements to last 1.6 s.

* Marking "not in the previous batch" made it live until the next refresh —
  **400 ms** on this machine, because a busy desktop refreshes the list several
  times a second. It flickered rather than faded.
* Remembering arrivals by wall clock and clearing them on a timer that was
  rescheduled by every refresh made it last **7.8 s**, because on a busy
  machine the timer never got to run. The same mistake inverted.

Aged out at the top of each refresh *and* on the timer: **2.05 s**, which is
the 1.8 s intent plus one throttle interval. The fade carries across redraws
through a negative `animation-delay`, so a row redrawn four times fades once.

## 2026-08-05 — a row's identity is its path, and what that cost

The duplicate rows in AUDIT §11 were not a missing removal. They were a second
notion of identity: rows were filed under whatever the source called an entry —
an inode on ext4 — while the only thing a watcher can name when a file
disappears is a path. Every atomic save (write a temporary file, rename it over
the target) produced a new inode at the same path, so the upsert filed a new row
and nothing could say the old one was gone.

Live, before and after, on a cache file the browser rewrites every few seconds:

```
scour search alt-svc-cache
# before:  10 of 267        ten identical paths, four different inodes
# after:    1 of 1
```

Ten atomic saves over one path, watched, on the live index: **one row**, and the
size is the last version's.

### The write path pays, the index gets smaller

Interleaved, alternating, three rounds each, 300,000 mock files, **the same
generator on both sides** — which matters more than it sounds: the first
attempt at this table compared the new code against the old *mock*, whose
allocation pattern differs, and it read as a 25% regression on the bulk path
that reversed once both sides generated the same way.

| | before | after |
|---|---|---|
| first index | 1.37–1.42 µs/entry | 1.45–1.50 |
| re-indexing the same paths | 1.37–1.43 | 1.71–1.83 |
| index on disk | 27.6 MB | **24.4 MB** |

The 11.6% on disk is three columns that no longer exist — `KeyKind`, `KeyA`,
`KeyB` held the source's identity, and `KeyA` was an incompressible 64-bit
value wherever a source hashed paths. On the live index: 89.0 → 84.8 bytes an
entry.

The 26% on a rescan is confirmation. A probe of the row table answers with
candidates — the key is half a digest — and each one is confirmed against the
directory number and name the row really carries, so a collision cannot kill
the wrong row. Where the old code compared four columns, this reads the name
arena. Measured by turning the confirmation off: **1.87 µs against 1.62**, so
exactness is a quarter of a microsecond an entry and the rest is elsewhere.

Two things were fixed while measuring, and both were found by measuring:

* Reconstructing the directory from the front-coded table decodes a whole
  restart block into a fresh `String`. Done per confirmation it cost **2.7
  µs/entry**; cached by directory number for the length of one pass, 1.8.
* `wanted` — the list of paths whose old rows are to be killed — cloned a path
  per staged entry to satisfy the borrow checker. Borrowing the two fields of
  `Inner` apart instead: **0.32 µs an entry**, a third of what writing an entry
  costs in total, spent on strings three lines from the originals.

The digest is now `scour_core::path_digest`, eight bytes at a time, shared with
`EntryId::path_hash` so that "the same path" means one thing. Byte-at-a-time
FNV over a 70-byte path measured 26.6 ns against 6.7 for the 21-byte identity
struct it replaced — three hashes an entry, so about 60 ns, and the three
dropped columns more than pay for it.

Query latency, after, on 2.39 M entries: `rapor` 27.5 ms, `ext:rs` 8.7,
`kind:image` 19.7, the empty query 6.1 — unchanged within noise, and this index
had 22 segments rather than the 8–10 it settles at.

## 2026-08-06 — building segments elsewhere: tried again, kept

[The 2026-08-04 attempt](#2026-08-04--building-segments-in-parallel-tried-reverted)
was reverted at 11%, for two reasons that have both since gone:

* its correctness problem was **hard links** — two paths, one inode, one
  `EntryId`, so a second sighting had to kill the first and a segment on a
  builder thread was not in the list to be killed in. A row is a path now, so
  there is no cross-segment replacement to race with. The same shape survives
  in a narrower form — the *same path* re-indexed while an earlier segment
  holding it is still being written — and is handled by giving each build a
  kill list that is applied the moment it lands.
* its blocker was that `flush` wrote the manifest before the segment existed.
  A build now hands its files back and **whoever next takes the write lock puts
  them in the list and saves the manifest**, so there is no window where the
  two disagree. That also means a builder thread never takes the index lock at
  all, and a slow disk cannot block a search.

Only the staging buffer overflowing builds elsewhere. Everything whose next
line depends on the rows being *in* the index — a sweep about to judge them, a
generation about to stamp them, a fold about to rewrite them, a commit about to
announce a revision — settles first. Two integration tests found exactly that
distinction by failing.

Measured end to end with `scourd --scan-only`, which is a real walk into a real
index, interleaved, alternating, first round of each discarded as cold:

**`/mnt/depo`, 1,565,767 entries, NTFS:**

| round | serial | building elsewhere |
|---|---:|---:|
| 1 | 2,093 ms | 1,364 ms |
| 2 | 2,167 ms | 1,329 ms |
| 3 | 2,161 ms | 1,306 ms |
| 4 | 2,129 ms | 1,419 ms |
| 5 | 2,243 ms | 1,314 ms |
| 6 | 2,232 ms | 1,297 ms |
| **median** | **2,164 ms** | **1,321 ms** |

**1.64×, six rounds of six**, and the same fourteen segments on disk either way.

`~/.rustup`, 247,430 entries: 526 ms against 461 — 13%. The difference is how
many flushes there are to overlap: two on that tree, fourteen on the other.
A pipeline needs something to put in it.

**`MAX_STAGED` was then tried at 40,000** — four times as many chances to have
several builds in the air. 1,290–1,318 ms against 1,297–1,419, which is the
same number, with **34 segments instead of 14**. The build is no longer the
critical path; paying for more segments to speed up something that is not the
bottleneck is how an index gets slower at answering. Left at 100,000.

## Scrolling: what was actually costing the frames

Reported as "laglı gidiş" — the list moved, but not the way the rest of the
desktop moves. Three things were suspected in turn. Two were real and small,
and the third was most of it and was not in this repository at all.

Everything below is measured in the window Hasan uses, driven through
`scripts/probe`, which exists because the embedded browser pane lies: it holds
`document.hidden` true forever, so deferred work never starts and timers are
throttled to about one a second. An earlier "985 ms to fill a screen" was that
pane and nothing else.

### 1. The painter — real, small

`innerHTML` per row against text nodes and a reused `<img>`, three rounds
interleaved, same window, same query, 120 frames a round:

| | median frame | p90 | frames missed |
|---|---:|---:|---:|
| `innerHTML` | 7.1 / 6.2 / 10.6 ms | 19.8 / 19.2 / 20.2 ms | 75 of 360 |
| text nodes | 6.2 / 10.7 / 6.4 ms | 16.5 / 21.3 / 12.3 ms | 43 of 360 |

Better, and noisy enough that it was clearly not the whole story. So the paint
was timed from the inside: **0.29 ms** for the entire `paintWindow` — range,
parse, spacers and sixty-eight rows together. The JavaScript was never the
cost, which is worth knowing before optimising any more of it.

### 2. Painting inline — real, and the cause of the lurches

The browser's own `long-animation-frame` records said: two frames in a hundred
and fifty, 65 ms each, and inside them **24 to 32 ms of forced style and
layout** attributed to a script.

That is a read of `scrollTop` taken while the tree is dirty. Rows landing from
the service called `paintWindow` directly, off a promise, in the middle of a
scroll — so the read had to wait for the browser to lay out a table sitting
under a thirty-million-pixel spacer. Every repaint now goes through `repaint()`
and happens at the top of a frame, where that read is free. Long frames after:
**zero**, forced layout: **zero**.

### 3. XWayland — not ours, and two thirds of it

Everything above still left every frame arriving at 16.6 ms on a 165 Hz screen.
The control was a page containing one tall gradient and no application code at
all, scrolled with a real wheel gesture in the same window:

| | idle | scrolling |
|---|---:|---:|
| default (XWayland) | 6.0 ms | 16.6 ms — 60 Hz |
| `--ozone-platform=wayland` | 6.1 ms | **6.1 ms — 165 Hz** |

Chromium defaults to XWayland here, and an XWayland window scrolls at 60 while
the screen runs at 165. Nothing in the list was involved: an empty page did the
same. `scripts/scour-app` now asks for Wayland when the session is Wayland.

The real list, real launcher, after all three: **6.2 ms median, no long frames,
no forced layout**, and the landing spot after a twelve-thousand-pixel flick
fills to sixty-seven rows with sixty-seven icons drawn.

The order matters more than the numbers. Two days of frame-shaving in the page
would not have found the third one, and the third one was worth more than both
of the others together.

## "Sonsuz scroll": a list that scrolled itself

Reported after the frame-rate work, and it was a different fault entirely. The
window was recording at the time, so this is the trace rather than a theory:

* the page wrote a scroll position **zero times**;
* with no input at all: **806 steps in 5.2 s**, 133,133 → 619,269, and then
  **3,842 steps over 52.8 s** travelling backwards, 619,252 → 497,297;
* and the scrollable height, sampled at every one of those steps, was never the
  same number twice: 620,005, 620,004, 620,030, 620,031, 620,081, 620,064 …

That is a feedback loop, and the page is only half of it. A scrollable area
that changes size makes the browser correct the position it is holding;
correcting the position fires a scroll; a scroll paints; painting changes the
height again. Nothing has to write a position for a list to run away.

### Why the obvious repair does not work

The height was a sum — top spacer, rows, bottom spacer — and the spacers were
sized from `ROW_H = 31`, the number the stylesheet asks for. So: measure the
row instead of assuming it. That was tried, and the measurement says:

| where | drawn row height |
|---|---:|
| near the top | **30.6 px** |
| near the bottom | **30.99 px** |

Same rows, same stylesheet, one screenful apart. This screen is scaled by
1.667, so 31 CSS pixels is 51.67 device pixels and the browser rounds each row
to 51 or 52 according to the fraction it starts at. **There is no row height to
measure.** Any design that computes the scrollable height by multiplying one
has this bug at every scale factor that is not a whole number — it is just
smaller or larger.

### What it is now

`.sizer` is an empty absolutely-positioned element as tall as the list, from
the row count alone. It is the only thing in the scroller that reaches that
far; the table is held above the bottom by the top spacer, with the headings,
the rows at the largest they round to and the end notice all subtracted. The
height stops being a result and goes back to being a decision.

| | distinct heights while scrolling | drift after release | drift at the bottom |
|---|---:|---:|---:|
| two spacers, `ROW_H` | 25 | 317,376 px | −1,408 px |
| two spacers, row measured | 25 | 0 px | 0 px |
| **sizer** | **1** | **0 px** | **0 px** |

Two queries, top and bottom, 80 steps each. Scrolling after: 7.2 ms median, no
long frames, no forced layout, and the bottom of the list fills.

## What is left of "laglı yükleniyor"

Once the list stopped scrolling itself, what remained was loading rather than
scrolling. Measured against the bridge directly, so the page is not in the way:

**Rows are not the slow part.** A 200-row window, median of three:

| offset | `a` (1.35 M matches) | `png` (123 k) |
|---|---:|---:|
| 0 | 16 ms | 7 ms |
| 2,000 | 24 ms | 18 ms |
| 10,000 | 50 ms | 39 ms |
| 19,800 | 84 ms | 46 ms |

Deep windows cost more because the engine walks to the offset, and at the far
end of what the list can reach that is still under a tenth of a second.

**The walk over the whole matching set is.** `api/facets` — the exact count,
the kind rail and the age chart, from one walk — took **1,599 ms** for `a` on
its first call after the index had moved, and **114–200 ms** for the same shape
of query afterwards. `api/count` alone is 68–228 ms warm. So the first broad
query after a commit pays for touching cold mmapped columns, and that is the
second and a half the window feels: the rows are already there, the count still
says "1.000+", and the rail is empty.

Two things follow, neither of them done here:

* the number is a *first-touch* cost, so warming the columns after a commit
  (`madvise(WILLNEED)` on what a broad query would walk) would move it off the
  first keystroke rather than making the walk cheaper;
* the facet walk is uncapped while the count beside it is capped. Capping it
  the same way would bound the wait, at the price of a rail that says "at
  least" instead of a number.

One outlier is unexplained: the page recorded a single `/api/search` at
2,255 ms while everything measured here was under 100 ms. A commit holding the
index while a search waits is the obvious candidate and has not been measured.

## Fetching: the flick asks for nothing on its way past

"Ne kadar aşağı kaydırsam o kadar lag" — and the diagnosis with it: the list
keeps trying to load while the hand is moving, and the process clogs.

That is what it was. A drag of a hundred and twenty thousand pixels crosses a
window boundary every few frames; every crossing started a request, none was
ever called off, and a browser opens six connections to an origin. By the time
the hand stopped, the window being looked at was queued behind twenty-nine
nobody would ever see — and deep windows are dearer, so the queue got slower
exactly as it got longer.

### Row count is nearly free; the walk to the offset is the cost

Three rounds, median, against the bridge directly:

| offset | 60 rows | 200 rows | 600 rows |
|---|---:|---:|---:|
| 0 | 8.0 ms | 9.1 ms | 24.9 ms |
| 2,000 | 15.6 ms | 27.0 ms | 34.1 ms |
| 10,000 | 48.2 ms | 43.8 ms | 59.0 ms |
| 19,800 | 103.0 ms | 88.1 ms | 158.9 ms |

Sixty rows at the far end cost the same as two hundred. So asking for only what
is on the screen — which was suggested — makes each request no cheaper and
means three times as many of them for the same distance travelled. **The number
of requests is the whole cost.** `WINDOW` stays at 200.

### Cancelling was not enough

Abandoning out-of-view requests with `AbortController`, on its own:

| | fill after the hand stops | requests |
|---|---:|---:|
| before | 1,399 / 1,691 ms | 21 / 22 |
| abort only | 1,235 / 3,040 ms | 37 / 37 |

No better, and it asks for *more*: a cancelled request has already been sent,
has already taken a connection, and the engine has already started walking.

### Waiting for the hand to stop is

`SETTLE = 90 ms` of a still range before anything is asked for. Three
interleaved rounds, five flicks each, headless at the real 1.667 scale:

| | fill after the hand stops | requests |
|---|---:|---:|
| before | 3,291 / 1,627 / 1,927 ms | 29 / 22 / 30 |
| **settle** | **232 / 906 / 994 ms** | **2 / 2 / 2** |

Every flick now costs two requests instead of twenty-five, and the screen fills
in about a second at the deep end rather than three. The cancellation stays,
not because it helped on its own but because it is what keeps a *second* flick
from queueing behind the first one's answer.

## Paging stops depending on how deep the page is

The debounce above was a bandage, and Hasan said so: *"debounce yanlış bence…
istekleri iptal edip yenisini yapmak lazım… durduğu yerde yüklenmeye devam
edilmeli, bu hızlı olmalı. bizim daemon iletişimimiz nasıl, o yetişir mi?"*

The transport was never the problem. Wall time against the engine's own
`took_us`, five rounds, median:

| offset | wall | engine | transport |
|---|---:|---:|---:|
| 0 | 23.1 ms | 16.0 ms | 7.2 ms |
| 5,000 | 66.4 ms | 62.4 ms | 4.0 ms |
| 10,000 | 84.6 ms | 80.7 ms | 3.9 ms |
| 19,800 | 118.1 ms | 114.1 ms | 3.9 ms |

HTTP, the Unix socket and the bridge together are **four milliseconds, flat**.
Everything else was the index answering a page by asking every segment for
`offset + limit` hits, merging, sorting the lot and throwing the first `offset`
away — linear in how deep the page is, and it rebuilt every discarded path on
the way.

### One ordering, prepared once

The order does not change while the index does not. `scour-engine` now keeps
the ordered hits of the query being looked at — up to 20,000, which is as far
as the list can reach — built on its own thread after the first page is
answered, and thrown away the moment the index revision moves. A page is a
slice of it.

| offset | before | after |
|---|---:|---:|
| 1,000 | 20.2 ms | 5.7 ms |
| 5,000 | 66.4 ms | 4.9 ms |
| 10,000 | 84.6 ms | 5.9 ms |
| 15,000 | 123.0 ms | 5.7 ms |
| 19,800 | 118.1 ms | **5.5 ms** |

Engine time on every one of those is **0.0 ms**: what is left is the four
milliseconds of transport. The first page of a new query is unchanged, because
it is answered the long way while the ordering is still being built.

### And then the rationing could go

With a window at 5.5 ms there is nothing to ration, so the settle went and the
list asks the moment the range changes, abandoning what scrolls out of view.
Three interleaved rounds, five flicks each:

| | fill after the hand stops | requests |
|---|---:|---:|
| settle 90 ms | 123 / 122 ms | 2 |
| **ask at once** | **17 / 21 ms** | 19–21 |

Several rounds measured **0 ms** — the screen was already full when the hand
stopped, because the rows arrived while it was still moving. Twenty requests at
six milliseconds cost less than two at a hundred and twenty, and they arrive
where the eye is rather than where it stopped.

One thing to watch, not yet measured: the prepared ordering is dropped whenever
the index revision moves, and a busy watcher moves it often. The next window
then pays the old price once and asks for a rebuild. On a quiet machine this
never shows; under a large rescan it might.

## The screen stops going blank

"Ekran yine boş kalıyor — bu html versiyonunda mecburen mi?" No. It was the
pulling, not the HTML.

Two attempts that did **not** work, measured before being believed:

* **Reaching two windows past the edge of the screen.** Blank frames during a
  drag: 12/19, 52/45, 18/12 out of sixty — noise. A fast drag covers sixty to a
  hundred and sixty rows *a frame*, so four hundred rows of headroom is three
  frames of it. No reaching distance survives a hand that means it.
* **Waiting for the hand to stop** — the settle, already removed above. It
  makes the flick free and every stop cost a wait.

What works is not reaching further but already being there. With the engine
keeping the ordering, a window is 5.5 ms at any depth and a thousand rows are
378 KB, so the whole of what the list can reach is worth fetching outright. It
goes one window at a time, only while nothing nearer the screen is outstanding,
outward from the viewport, and is abandoned when the query changes.

Three interleaved rounds, five flicks each, headless at 1.667 scale:

| | blank frames during the drag | fill after the hand stops |
|---|---:|---:|
| on demand | 48 / 41 / 23 of 60 | 18 / 99 / 16 ms |
| **quiet fill** | **0 / 0 / 0 of 60** | **0 ms** |

Fifteen flicks, not one blank frame, and nothing to wait for on stopping
because there was nothing left to fetch.

What it costs: the whole reachable list is cached in **706 ms** for `a` and
2,509 ms for `png`, and the page's heap sits at 16–43 MB with twenty thousand
rows in it. All of it is work done while nothing is being asked for.

## The stall nobody could name was the bridge's single socket

Two loose ends from earlier — a `/api/search` the page once recorded at
2,255 ms, and facet walks that were 130 ms except when they were 1,400 —
turned out to be the same thing, and it was not the engine.

Forty facet calls on a quiet query: **thirty-nine took 2 ms and one took
1,056**, and the slow one landed while the index had twenty-four changes
staged. Not a revision bump, so not the commit swapping segments — something
holding a queue.

The service gives every connection a thread of its own. The bridge held **one**
connection, behind a mutex, for every request the page made. So a screenful of
rows queued behind whatever was in front of it, and one of the things in front
of it is a walk of the whole matching set.

The page's own burst — one facet walk and six windows at once, which is what a
scroll during a fresh query looks like — three interleaved rounds:

| | window, median | window, worst | whole burst |
|---|---:|---:|---:|
| one socket | 230 / 240 / 228 ms | 3,130 / 3,004 / 304 ms | 266 / 273 / 276 ms |
| **pool of 8** | **61 / 65 / 56 ms** | 165 / 1,009 / 1,572 ms | 198 / 792 / 786 ms |

**Four times faster for the thing that becomes rows on the screen**, in every
round. The whole burst sometimes takes longer, and that is the trade being
made on purpose: the facet walk now runs beside the windows instead of ahead
of them, so the count settles later and the rows arrive sooner.

A worst case of one to three seconds survives on both sides and is not this.
It appears only while the index has work staged, so the next thread to pull is
the write lock during a commit — `SCOUR_LOCK_TRACE=1` exists in
`NativeIndex::commit` for exactly that and has not been run against a busy
index yet.

## An open window was holding the service at 100% CPU

Asked in passing — does watching steal CPU? — and the answer turned out to be
about something else entirely.

**What a watch costs is small.** Adding them is 1.5 µs each: the 455,367 this
machine holds are 0.7 s of setup. On the write path, three interleaved rounds
of 3,000 creates and deletes, watched against unwatched on the same
filesystem: 19.8/20.2/19.8 ms against 21.7/22.0/21.9 ms — about **0.7 µs a
file, ~10% on creates and ~3% on deletes**.

**What the page cost was not small.** With the window open and nobody touching
it, `scourd` sat at **115%**. Closed: **1.0%**. Reopened: **97.4%**.

The page's own request log over twelve idle seconds said what it was doing:

| | requests | total |
|---|---:|---:|
| `facets` | 4 | 3,718 ms — 930 ms each |
| `search` | 31 | 3,625 ms — 117 ms each |

Two faults, both introduced the same day as the fix for blank rows:

* **The cache is keyed by position, and positions move.** The list is sorted by
  time, so a file being written walks to the top and everything under it
  shifts. The live refresh handled that by replacing the row map wholesale —
  correct when the map held two windows, ruinous once the page read ahead: it
  discarded twenty thousand rows and asked for them again, **1.26 times a
  second, which is how often this index moves**. Each window now remembers the
  revision it came from; the visible ones are refetched when the index moves
  and the rest are left alone until the eye reaches them.
* **A fixed refresh interval asks an expensive question as often as a cheap
  one.** The three-second count refresh was 930 ms of service time each round,
  on a query with 1.35 M matches, for a number nobody was reading. The
  throttle now charges for what the last call cost and waits ten times that,
  so a background refresh can never take more than about a tenth of a machine.
  Cheap queries are unaffected.

And one in the engine, from the same day: the prepared ordering is thrown away
whenever the index moves, so on a machine with a watcher the preparing thread
was rebuilding twenty thousand hits over and over — **27% of a core, idle**.
It is now built only for a page that is not the first one (which is the only
kind that needs it), never during a scan, and at most once every two seconds.

After: the page makes no background request at all while idle — fifteen seconds
of its log holds one long poll and nothing else. With the scan and the rebuild
it triggered both settled, the service with that same window open measures
**3.3%, 7.2% and 15.8%** across three ten-second windows, against 115% before;
what is left is the live refresh doing its job as the index moves, which is
the thing it is for.

The lesson is not about any of the three faults. Each was a small change that
was correct in isolation, and each stopped being correct because a *different*
change made the thing it assumed cheap expensive. A cache that is thrown away
is free until something fills it; a fixed interval is free until the question
behind it gets big; speculation is free until it is invalidated faster than it
is used. None of it showed up in a search that felt fast — it showed up as a
fan, on a machine nobody was using.

## Verifying the review: the same fixes, on the machine rather than in a copy

`docs/REVIEW-MEMORY.md` measures a four-row minute against a reflinked copy of
the index. This is the same daemon under systemd, one process, window shut,
after a full scan had settled:

| | before | after |
|---|---:|---:|
| idle CPU | 0.87% | **0.30 / 0.37 / 0.25%** |
| rows changing in that minute | 4 | 0 / 4 / 2 |
| own memory (anon + swap) | ~640 MB | 157 + 259 = **416 MB** |

**Three times better here, against nineteen times in the copy, and both numbers
are honest.** The review measured one sequence — the commits and the compaction
that follow four rows. This measures everything the service does: the watcher's
events, the pulses, the commit clock, and that sequence inside it. A fix worth
nineteen times its own cost is worth three times the whole.

The memory is likewise better and not solved: 416 MB of anonymous and swapped
pages for an index whose live allocations the review measured at about 10 MB,
plus 74 MB the `notify` backend genuinely holds. The remainder is the allocator
slack the review declines to chase without a decision, and it is right to.

### And the measurement that was not a measurement

Three of the numbers taken during this session were taken while **a second
`scourd` was running** — one started by hand at 22:21 whose `kill` had matched
the measuring shell's own command line instead of the daemon, so it survived,
held the index lock, kept the service from starting, and watched `/home` beside
it. It had been given a narrower configuration, so the live index shrank to
741,199 rows while it held it.

Two things follow, and the second is the useful one. Any figure in this session
between 22:21 and the cleanup is void. And the index came back on its own: one
restart, one scan, **741,199 rows to 2,091,996 in forty seconds**, with nothing
asked of it. Reconciliation is what that is for, and this is the first time it
has had to prove it against a real accident rather than a test.

## What wakes an untouched window's connection threads

Temporary dispatch instrumentation wrote request names and handler durations to
`/run/user/1000/scour/request-trace`, outside every indexed source. CPU was the
`utime + stime` delta from `/proc/<pid>/stat` and each task's `stat`; one jiffy
is 10 ms on this host. The same release binary was used on both sides, Cargo was
absent, and the window PID was checked before and after.

With the window open and untouched for 30 seconds:

```text
revision=459..479 delta=20 process_jiffies=263 process_cpu=8.76%
scour-conn: 36 + 196 + 0 + 0 + 0 jiffies = 7.73%
await  count=20 elapsed_ms=29490.207
facets count=6  elapsed_ms=1730.572
search count=34 elapsed_ms=99.310
```

The command took byte and jiffy snapshots, slept 30 seconds, then reduced only
the new trace bytes:

```sh
pid=$(systemctl --user show scourd -p MainPID --value)
start_bytes=$(stat -c %s /run/user/1000/scour/request-trace)
start_cpu=$(awk '{print $14+$15}' /proc/$pid/stat)
for f in /proc/$pid/task/*/stat; do
  awk '$2=="(scour-conn)" {print $1, $14+$15}' "$f"
done > /tmp/scour-open-before.$pid
sleep 30
# Repeat the snapshots, subtract by TID, and reduce trace lines after
# start_bytes by request name and elapsed_us.
```

The exact equality between revision delta and completed `await` calls identifies
the wake-up: a commit changes the revision, the long poll returns, and
`indexMoved()` in `apps/scour-web/src/page.html` starts visible-row searches and
the throttled sidebar refresh. The sidebar is the expensive part here: its six
`facets` calls spent 1.731 seconds inside dispatch.

Closing only the window on the same binary removed all `await`, `search`, and
`facets` calls during a 30-second control interval, even though unrelated
activity in the watched home source moved the revision 208 times. Total service
CPU was 31 jiffies (1.03%), including the worker handling that activity. The
window was then reopened and the open measurement above was taken. Suppressing
the refresh would change the documented live-window semantics, so runtime
behaviour was not changed.

## 2026-08-15 — a list ordered by a number was still walking every match

`8530cd7` stopped a size-sorted page building a row per match. It did not stop
it *visiting* one: the stored row order is `(mtime desc, path asc)`, so
`sort:modified` descending terminates at the first page and every other order
went to the end of the corpus to find out which forty won.

What was there to use is the zone map. A block already stores the true minimum
and maximum of every column — `ColumnWriter::seal` writes them, and the numeric
filters have read them for a while — so the blocks can be put in the order of
what each could contribute and opened best first. Once the page's worst row
beats everything the next block could hold, nothing left can enter the page.

### The instrument

A copy, so the running service is neither blocked nor believed. Both binaries
were built from the same example and run alternately, five to eight rounds
each, because another agent's `cargo build` was on the machine and a spike that
hits one side only is a lie. Each cell is the least seen; the example itself
takes the least of three per call.

```bash
cp -a ~/.local/share/scour/index /tmp/scour-topk/idx
rm -f /tmp/scour-topk/idx/index.lock
cargo run --release -p scour-index-native --example searchcost -- /tmp/scour-topk/idx/native
```

2,234,587 rows, 4 segments, empty query, a page of 200. Milliseconds.

| order | offset 0 | 2 000 | 19 800 |
|---|---|---|---|
| modified ↓ (stored order) | 0.5 → 0.5 | 0.7 → 0.7 | 2.9 → 2.9 |
| modified ↑ (backwards) | 0.6 → 0.5 | 0.8 → 0.7 | 2.8 → 2.7 |
| name ↓ | 144.3 → 145.6 | 145.7 → 140.8 | 145.5 → 148.3 |
| **size ↓** | **118.9 → 2.8** | **117.5 → 4.8** | **118.0 → 16.1** |
| **size ↑** | **113.5 → 2.1** | **118.8 → 2.9** | **116.6 → 6.8** |
| **created ↓** | **89.0 → 2.1** | **93.5 → 2.3** | **91.0 → 5.7** |
| **kind ↓** | **102.5 → 2.0** | **97.9 → 2.7** | **104.6 → 10.0** |
| path ↓ | 670.9 → 652.0 | 645.7 → 636.7 | 651.8 → 647.8 |
| relevance ↓ | 0.5 → 0.6 | 0.7 → 0.8 | 2.3 → 2.3 |

`name` and `path` are the control and they are meant to be flat: no stored
number bounds a name, so those still walk everything. They are somebody else's
change.

### With the folder-size table built

Sorted by size a directory is ordered by what is *under* it, which its own
`Size` column knows nothing about — so the block's range has to be widened by
the rollups of the directory rows it holds before it can be used as a bound.
That is a looser bound, and a service that has shown anybody a folder size is
in this state where a fresh process is not. Same command with `warm` as the
third argument:

| order | offset 0 | 2 000 | 19 800 |
|---|---|---|---|
| size ↓ | 130.8 → 3.6 | 133.1 → 6.6 | 130.6 → 16.4 |
| size ↑ | 132.5 → 3.0 | 135.1 → 4.1 | **130.8 → 30.9** |

The bold cell is the weakest result here and the reason is the widening: an
empty folder rolls up to nought, so ascending, a great many blocks look as
though they could reach the smallest value and the order barely separates them.
Four times faster rather than forty, and still right.

### What it costs where it cannot pay

The ordering is a pass over the candidate blocks — a range read each and a sort
— and it saves nothing until there is a page for a block to be out of reach
of. Built eagerly it charged `rapor` sorted by size **9.6–10.9 ms against
8.6–9.0**, thirty thousand blocks ordered to skip none of them, because a term
the trigram filter has already narrowed matches fewer rows than the walk needs
before it can bound anything.

So it is built lazily: the walk starts in block order like every other one and
reorders what is left the moment the page first fills. That needed the walk to
go a block at a time rather than in coalesced runs, since a run here is the
whole index — measured at no cost, 142–148 ms against 148–154 on the same
`name ↓` full scan.

What remains is a query that matches **more than a page and fewer than the
count cap**: the page stops early, the count does not, and the ordering is paid
for nothing. Eight interleaved rounds on `rapor`, page of 200:

| order | before | after |
|---|---|---|
| size ↓ | 8.4 | 8.2 |
| size ↑ | 7.6 | 8.6 |
| created ↓ | 7.4 | 9.1 |
| kind ↓ | 7.5 | 8.3 |
| relevance ↓ (control) | 8.9 | 8.6 |

Up to 1.7 ms on a query already under ten, against a hundred and fifteen saved
on the ones that match everything. It was left there rather than guarded,
because every guard that would catch it needs to guess the number of matches
before walking, and guessing low would throw away the whole result.

## 2026-08-15 — an untouched window asked for the same rows twice

Two things in `page.html` owned the job of keeping the visible window current,
and they both did it. `reviseRows` refetched on every index revision without
registering anything in `LIST.pending`; `fillWindow` ran one frame later from
`paintWindow`, found the window neither in flight nor stamped, and fetched it
again; `reviseRows` then aborted `fillWindow`'s copy after the service had
already built the page.

**How it was measured.** `scripts/scour-app` on port 7699 with
`SCOUR_APP_DEBUG=9333` and `XDG_DATA_HOME` pointed at a scratch browser
profile, against the *owner's* running `scourd` — 2.23 M entries, two watched
sources. `scripts/probe` wrapped `window.fetch` inside the page rather than
attaching CDP's Network domain, because an abandoned request is still a request
the service answered and only the caller can tell it from one that was read.
Bytes came from Resource Timing. Two forty-second stretches back to back per
launch, binaries alternated, `scourd` CPU sampled once a second from
`/proc/<pid>/stat`.

**Put the list on newest-first, and check that it stayed there.** The saved
sort here was `path`, and `applySettings` lands a round trip after the window
draws and ends in `render()` — so a heading click that goes in first is undone
silently a moment later. Three rounds were thrown away to that. It costs twice
over: a path window is 2.5 s of the service against 3 ms, so the reading-ahead
switches itself off and the refresh throttle stretches to twenty-odd seconds,
and the list then looks idle when it is only expensive. It is also what once
had a probe reporting the live refresh broken when it was not.

Five rounds on the shipped binaries, alternating, every leg reported:

| | `/api/search`/s | abandoned | back to back (<150 ms) | MB/s | scourd |
|---|---|---|---|---|---|
| before A, leg 1 | 2.55 | 13 | — | 0.170 | 2.28% |
| before A, leg 2 | 2.75 | 15 | — | 0.182 | 2.27% |
| before B, leg 1 | 2.20 | 8 | 32/88 | 0.153 | 1.67% |
| before B, leg 2 | 2.25 | 6 | 36/90 | 0.165 | 4.64% |
| before C, leg 1 | 4.25 | 34 | 75/170 | 0.275 | 4.48% |
| before C, leg 2 | 2.52 | 12 | 49/101 | 0.175 | 1.81% |
| after A, leg 1 | **1.40** | 0 | 0/56 | 0.108 | 1.39% |
| after A, leg 2 | **1.30** | 0 | 0/52 | 0.100 | 1.53% |
| after B, leg 1 | **1.25** | 0 | 0/50 | 0.097 | 3.53% |
| after B, leg 2 | **1.32** | 0 | 0/53 | 0.103 | 1.43% |

One search per reply to `/api/wait` afterwards, against about two before:
`search 56 / wait 60`, `52/53`, `50/53`, `53/64`. **The CPU column is the
noisiest of the five and says so** — this machine was in use throughout, and
1.67% and 4.64% are the same binary in the same round, as are 1.39% and 3.53%.
Request count and bytes are what carry the result; CPU is reported because it
was asked for and because a single run of it would have lied in either
direction.

**And the reading-ahead was asking for every window twice.** The abandoning
sweep — "anything in flight that the screen has left behind" — did not exempt
the one window the reading-ahead had just started, and that window is off the
screen by definition. So it was abandoned by the next frame and asked for
again. The steady-state rounds above never see this because the reading-ahead
has finished by then, so it was measured on its own by re-running the query
through the page's own `input` handler and counting for forty seconds:

| filling in the reachable list | requests | for how many windows | abandoned |
|---|---|---|---|
| before | 282, 282 | 99 | 86, 79 |
| after | **144, 154, 154** | 99 | **2, 4, 3** |

1.74 and 1.75 read-ahead requests per window before; 1.02, 1.04 and 1.03 after.

**The live refresh still works, checked in a real window.** A file created
under `~/.cache` — a watched source — with the list on newest-first: found at
row 1 after **1,253 ms** on the old binary, and after **1,501 / 1,502 / 501
ms** at rows 1, 5 and 1 on the new one, with the arrival highlight on every
time. A wheel gesture 40,000 px down the same list drew 76 of 76 rows with no
blank frame at any of twenty-four samples, twice.

**One thing that does not work and did not work before either.** With the query
on a string nothing matched, creating a file that matches it moved the counter
to `1 / 2.235.320` and drew no row — identically on both binaries. It is
recorded here because it was found while checking this change and it is not
this change. *(Explained and fixed on 2026-08-15; see "the counter and the
rows were on different clocks" below.)*

```sh
SCOUR_APP_PORT=7699 SCOUR_APP_DEBUG=9333 XDG_DATA_HOME=/var/tmp/scour-idle-home \
  scripts/scour-app &
scripts/probe setup.js     # settings first, then newest-first, confirmed twice
scripts/probe measure.js   # two 40 s stretches, fetch wrapped inside the page
```

## 2026-08-15 — an ordering nobody could keep, rebuilt on a clock

`prepare_loop` walks up to `PREPARE` = 20,000 hits and throws the result away
if the index moved while it was walking. A window open on the list holds
`watchers > 0`, which puts the commit clock on `commit_watched`, so the index
moves about once a second — and `PREPARE_EVERY` was a flat two seconds
justified by a comment assuming the walk costs "about a tenth of a second".
That is true of the stored order and of nothing else: the same file measures
3.2 ms for one window sorted by modification time and 2,463.6 ms sorted by
path.

The leash is now the one the page already uses on itself — `atMostEvery`
charges `floor = max(ms, spent * COST)` — so a walk buys `PREPARE_COST` (10)
times its own length of quiet, never less than the two seconds it had.

Proved by a test rather than by a stopwatch, because the machine this was
written on is in use and its `scourd` may not be restarted:
`an_expensive_ordering_is_not_rebuilt_on_a_clock` gives the engine an index
whose 20,000-hit walk costs 400 ms, holds a waiter so the commit clock is the
watched one, and asks for deep pages for three seconds against an index that is
moving. On the flat interval the walks began at **181 ms and 2,185 ms**; on the
leash there is one.

```sh
cargo test --release -p scour-engine an_expensive_ordering
```

## 2026-08-15 — the counter and the rows were on different clocks

Reported: *"with the query on a string nothing matches, creating a matching
file moves the counter to `1 / 2.235.320` and draws no row."* Recorded above
under the 187eefa merge as older than that change and not understood. It is two
faults at one seam, and neither is in the empty-result path the note pointed
at.

**The counter is not on the clock and the rows are.** `LIST.total` is moved by
four things. Three are events — the search that lands on a keystroke, a window
of rows carrying a total the count cap did not cut, going offline. The fourth
is `reviseCounts`, on `countsSoon = atMostEvery(reviseCounts, 3000)`. The rows
are on `rowsSoon = atMostEvery(refreshRows, 400)`, and `atMostEvery` charges
`floor = max(ms, spent * COST)` — so the rows wait ten times whatever the last
window cost, and one window of a path-sorted list is seconds. Both throttles
are made once at module scope, so **the wait the rows are serving when the
query changes is the previous query's price**; a no-match query's own windows
are cheap and cannot shorten a leash that is already running.

Measured in the running window against the owner's `scourd`, 2,235,893 entries:
list on folder order showing everything, then a query nothing matches, then a
matching file created under `~/.cache`.

| | count says 1 | first window of rows | gap |
|---|---|---|---|
| before, leg 1 | 13,621 ms | 14,940 ms | 1,319 ms |
| before, leg 2 | 2,452 ms | 15,059 ms | **12,607 ms** |
| after, leg 1 | 1,549 ms | 1,567 ms | **18 ms** |

The gap is the leash, so it is as long as the last window was dear. In leg 2
the counter read `1` over an empty list for twelve and a half seconds.

**And the message under the empty list was never taken down.** `empty.hidden`
was written by three of the four things that move `LIST.total`, and the count
refresh was the one that was not. So when the count got there first,
`fillWindow`'s answer arrived with `res.total === LIST.total`, the branch that
hides the message was skipped for having nothing left to change, and "Eşleşme
yok" stayed on the screen **over a row**, until the query changed. Observed at
8 s and again at 33 s on both before-legs.

**What changed.** `setTotal` is now the only thing that moves `LIST.total`,
`LIST.exact` and `empty.hidden`, and it also runs the two sweeps that say what
the cache may still be trusted for. The second of those is new: `dropBeyond`
drops the rows past the end once the count says where the end is, and
`dropShort` is its growing half — a window that came back *short* came back
short because the list ended inside it, so a count saying it does not makes
that window out of date however recently it was fetched. Only the stamp goes,
never the rows, which puts the window in the class `fillWindow` fetches on the
frame rather than the class that waits for the clock. That is the 18 ms above.

**The ordinary live refresh survives, checked rather than assumed** — it is the
regression this could plausibly cause. Newest-first, a list already holding 13
rows, another matching file created: at **row 1 after 555 ms, with the arrival
highlight**, the count following at 605 ms and the sizer 403 → 434 px. The
defect's own case draws its row with the highlight too — 3,976 ms, message
hidden in the same 50 ms sample, count at 4,430 ms — and no ghost row appears
at any sample in either.

**A warning worth more than the fix.** Two rounds were thrown away, and the
first "reproduction" was wrong, because **a window nobody is looking at gets no
frames and the page then draws nothing at all** — with `document.hidden` false
and `visibilityState` "visible" throughout. Measured on this desktop, GNOME
Wayland and XWayland alike, `scripts/scour-app` as it ships:
`requestAnimationFrame` not fired in 2,000 ms. Every paint is coalesced to a
frame, so the list freezes on whatever it last drew while the counter — written
outside the painter — goes on moving. In one such round the list was still
showing the *previous* query's 78 rows and a 620,000 px scrollbar while the
counter said `1`. That is indistinguishable by eye from this defect, and it is
probably how the original note came to say "drew no row". Neither
`--disable-backgrounding-occluded-windows` nor `Page.bringToFront` brings the
frames back; `Page.startScreencast` does, which is why this was measured over
one CDP connection with a screencast running rather than with separate
`scripts/probe` calls. **A probe that checks `document.hidden` is not checking
whether the page can draw. Ask it for a frame.**

Not fixed, deliberately, and each is a decision rather than an oversight:

- **The leash still crosses a query change.** `sidebarCost` is reset by a new
  query because "a new query's cost is its own"; `rowsSoon`'s `floor` is not,
  and that is what makes the gap seconds rather than 400 ms. `dropShort` makes
  the gap stop mattering for a change the count can see, so this is now a
  question about cost rather than about correctness.
- **The page pays full price for a window it cannot draw.** While the frames
  are stopped it keeps `/api/wait` open — which is what makes the service commit
  on the burst clock instead of batching — and goes on refetching rows nobody
  will see. `document.hidden` is what it decides that on, and on this desktop
  `document.hidden` does not mean what the page needs it to mean.

```sh
# One connection, screencast running, so the window has a frame clock at all.
SCOUR_APP_PORT=7688 SCOUR_APP_DEBUG=9344 XDG_DATA_HOME=/var/tmp/scour-emptyfix-home \
  scripts/scour-app &
cargo test --release -p scour-web the_length_of_the_list_and_the_empty_message
```

## 2026-08-15 — status was a directory walk

`NativeIndex::stats()` had stopped walking every indexed row, but still called
`read_dir`, `metadata` and `len` for every file in the index directory on every
request. `status()` calls it too, as does every completed `await`, so this was a
filesystem metadata walk in a path intended to report already-known numbers.

On the running service — 2,236,507 entries and 19–22 segments during the run —
2,000 valid `stats` messages over one persistent socket took **1.041 s wall**
and advanced the service by **1.030 s CPU**, about **520 µs per request**. The
CPU figure includes any other service work during that second; the matching
wall time is the stronger result here.

```sh
pid=$(pgrep -n -x scourd); hz=$(getconf CLK_TCK)
read -r _ _ _ _ _ _ _ _ _ _ _ _ _ u0 s0 _ < /proc/$pid/stat
a=$(date +%s%N)
seq 1 2000 | awk '{printf "{\"id\":%d,\"op\":\"stats\"}\n",$1}' \
  | nc -U /run/user/$(id -u)/scour/scour.sock | head -n 2000 >/dev/null
b=$(date +%s%N)
read -r _ _ _ _ _ _ _ _ _ _ _ _ _ u1 s1 _ < /proc/$pid/stat
printf 'wall_ns=%s service_cpu_ms=%s\n' "$((b-a))" \
  "$((((u1+s1-u0-s0)*1000)/hz))"
```

The replacement caches the summed file-length total between controlled
mutations. It does not redefine the number as “published segment bytes”:
background writes are marked active, a concurrent read still walks and counts
their files, and a failed write leaves the cache dirty so its partial orphan is
included by the next quiet read. Replacements and erases invalidate it too.
`maintain` deliberately keeps direct before/after directory walks.

The internal cost harness uses 220 files, close to the 19–22 segment live
layout. With warm metadata, 2,000 old walks took **267.774 ms** and 2,000 quiet
cached reads took **1.047 ms**: **133.9 µs to 0.52 µs per read, 256× less**.
This isolates the operation removed from `stats`; it is not presented as a
post-deployment end-to-end number. The live socket measurement must be repeated
after the new service is installed.

```sh
cargo test -p scour-index-native --lib cached_directory_byte_cost \
  -- --ignored --nocapture
```

## 2026-08-15 — the copied-index average hid the live Name tail

The 49 ms Name number near the start of this document belongs to an isolated
copy. It did not describe the running service. With 2.237 million live rows,
ordinary empty-query Name pages took **53.561–72.410 ms**, while six of fifty
calls took **1.449–1.576 s** of server time. Five of those slow calls bracketed
a `pending_removals` transition from 2 or 4 to zero; the sixth transition began
and ended between the two status samples. An earlier 60-call probe saw as much
as **2.434 s outside the socket**, which includes queue and IPC time.

The pending-removal overlay used to spell a full path for every accepted row.
The replacement compiles the removal paths once per segment into directory-row
ranges and exact parent/name exclusions. These are deliberately pre-change live
numbers; the post-change tail still has to be measured after the new daemon is
installed. A copied-index result must not be substituted for that measurement.

```sh
for i in $(seq 1 50); do
  s0=$(target/release/scour --json stats)
  j=$(target/release/scour --json search --sort name --limit 200 --count-cap 1000)
  s1=$(target/release/scour --json stats)
  printf '%02d pending=%s-%s took=%sus\n' "$i" \
    "$(printf '%s' "$s0" | jq -r .pending_removals)" \
    "$(printf '%s' "$s1" | jq -r .pending_removals)" \
    "$(printf '%s' "$j" | jq -r .took_us)"
done
```

## 2026-08-15 — Name and extension have persisted orders

`seg-*.norder` and `seg-*.eorder` use the same position-stream shape as the
stored path order. On the isolated 2,236,577-row reflink copy each file is
**9,225,885 bytes (8.80 MiB)**: four bytes and one tie bit per row, plus the
count. Missing files select the legacy scan; a present malformed file is index
corruption rather than a silent fallback.

The extension control on that copy changed at offset zero from **60.1 to 0.6
ms** descending and **56.5 to 0.5 ms** ascending. At offset 19,800 it changed
from about **61 ms to 3.8–4.0 ms**. Name must be remeasured with the final
prefix-compatible arena build before an after-number is recorded.

```sh
cp -a --reflink=always \
  ~/.local/share/scour/index-backup-before-perf-20260815/native \
  ~/.local/share/scour/index-bench-orders-20260815
target/release/examples/compact_cost \
  ~/.local/share/scour/index-bench-orders-20260815 rebuild
target/release/examples/searchcost \
  ~/.local/share/scour/index-bench-orders-20260815
```

## 2026-08-15 — fanotify's directory map uses a packed path arena

The allocator probe models the **255,769** directories watched by the live
service, with 74.0 bytes per path. Retained heap changed from **46,049,776 to
33,031,200 bytes** (−13,018,576, **−28.3%**) and retained path allocations from
255,769 to **19**. Two build pairs were 101.64 → 69.77 ms and 110.58 → 82.40
ms. Lookup did regress: 2,046,152 operations changed from 156.5/156.7 ms to
214.6/196.3 ms, about 19–28 ns extra per lookup. This is a synthetic allocator
probe, not a claim about process RSS.

The same probe measured a leaf rename at **6.056 ms**, a whole-tree rebase at
**16.438 ms**, and relearning an existing directory at 213 ns. Those rename
times are milliseconds, not seconds.

```sh
cargo test -p scour-source-fs directory_map_memory_probe --release -- \
  --ignored --nocapture --test-threads=1
```

## 2026-08-15 — the Slint list is bounded at every depth

This is a deterministic bound, not a frame-time benchmark. The first request
asks only for visible rows, grows to at most **256**, then slides by **128**
while anchoring the visible and selected global rows. Both the Rust hit buffer
and the Slint model therefore retain at most 256 rows regardless of depth. The
test covers the 0 → 128 → 0 transition, including returning from a partial last
window; an empty speculative forward page keeps the previous page visible.

```sh
cargo test -p scour-gui list_growth_is_demand_driven_and_bounded
```

## 2026-08-16 — the double walk at start-up is real, and it is not inotify's

A review listed "a double directory walk at first start — the inotify setup runs
before the scan; 15.1 s measured over 342,000 directories" and proposed merging
the two passes. The premise had to be checked before the fix, because **this
machine does not run inotify**. `watch::start` returns from `fanotify::try_start`
before `notify` is constructed at all, so the recursive watch that walks a tree
to install one watch per directory is never built here. The 15.1 s is a real
number about a real mechanism — it is the cost of *installing* 342,000 watches,
recorded further up this file, and already answered by moving watching to a
thread of its own. It is not this machine's cost and it is not a walk.

**There are still two passes, for a different and unavoidable reason.** A
fanotify event names its parent directory by file handle, never by path, so
`DirMap::build` walks every root to learn which `(dev, ino)` is which directory
— 103,524 under `/home/hasan` and 152,530 under `/mnt/depo`, which is the pair
`scourd`'s start-up line prints. Resolving a handle on demand instead is
`open_by_handle_at`, which wants `CAP_DAC_READ_SEARCH`; this process is
deliberately built to hold no capability. The pass cannot be dropped, and it is
over exactly the directories the scan then walks again.

Measured before anything was changed, alternating map-walk then scan-walk in one
process, warm after a first cold round:

| | directories | entries | map walk | scan walk |
|---|---|---|---|---|
| `/home/hasan` | 103,524 | 896,274 | **0.80 s** | 1.17 s |
| `/mnt/depo` | 152,530 | 1,350,627 | **1.03 s** | 1.73 s |

Cold, on the first touch of each tree: 4.46 s and **15.69 s**. The second pass
was 59–69% of the scan's own walk, and it ran to completion inside
`start_watching` before the scan was so much as queued.

### Merging the two passes was rejected, and not for want of trying

The scan could feed the map — it stats every directory it visits — but three
things stop it, and none is about speed. `scan.on_start = false` is a supported
configuration with a test of its own, and under it there is no scan to feed the
map at all, leaving a watcher that resolves nothing. The map would then have to
be filled from inside the walker's visitor across the global `SUBS` lock, once
per directory, against the reader thread holding the same lock to process
events. And an event for a directory the scan has not yet reached is **dropped,
not escalated** — deliberately, because escalating turns ordinary traffic into a
storm — so every directory would carry a window in which its changes are lost.
That window is the hole `scan.on_start` exists to close, and a merge that opens
it is worth less than the second walk. A true single pass needs `Source::watch`
and `Source::scan` to become one call, which is a change to the trait and to the
engine's ordering contract, and is not attempted here.

### What was done instead: the second pass is no longer single-threaded

It reads the same directories the scan reads, and the scan reads them on several
threads. `DirMap::build` now uses the same `ignore` parallel walker, streaming
batches into the map while the walk runs rather than joining a quarter of a
million paths into a vector first — the packed arena's 13 MB saving must not
come back as a build-time peak on a machine that swaps. Its walkers take the
same `nice` and idle I/O class as the scan's.

Eight threads rather than the scan's two, and the difference is the consumer,
not the device: the scan is capped by an index that cannot take more, and this
walk's consumer is a hash-map insert at 213 ns. `Medium::walk_threads` keeps the
device's opinion and drops the consumer's cap, so a spinning disk still gets one
reader. Eight is not the fastest — sixteen was 0.19 s and 0.22 s — but this runs
while the rest of the session is coming up.

Three rounds, alternating old and new within each round, at the shipped setting
of eight threads on twenty cores:

| | serial | parallel ×8 | |
|---|---|---|---|
| `/home/hasan`, 103,524 dirs | 1.087 / 1.275 / 1.007 s | **0.312 / 0.494 / 0.296 s** | 2.6–3.5× |
| `/mnt/depo`, 152,530 dirs | 1.561 / 1.696 / 1.248 s | **0.364 / 0.397 / 0.349 s** | 3.6–4.3× |

Cold, on the first touch: `/mnt/depo` 6.71 s → **0.378 s**, `/home/hasan` 2.37 s
→ **0.274 s**. Both sources together, warm, the two passes cost about 2.4 s of
start-up before and about 0.70 s after.

**What is not measured: `scourd`'s own start-up wall clock under fanotify.**
Reaching that path needs the descriptor the privileged helper hands over, and
nothing here runs as root. The claim made is narrower and is the whole of the
change's effect: `DirMap::build` is synchronous inside `start_watching`, nothing
else on that path was touched, so the start-up delta is the walk delta above.
The end-to-end number stays unproven.

Both walks agree on the directory count exactly — 103,524 and 152,530, asserted
inside the probe — and two tests hold what the counts imply. Deleting the `Drop`
on `DirBatch`, which is what flushes each thread's last partial buffer, fails
`the_parallel_walk_finds_every_directory_the_stack_walk_found` with 2,801 of
2,801 directories lost, and fails the ordering test beside it.

```sh
SCOUR_WALK_ROOTS=/home/hasan SCOUR_WALK_ROUNDS=3 SCOUR_WALK_THREADS=2:4:8:16 \
  cargo test -p scour-source-fs --release directory_map_walk_cost_probe -- \
  --ignored --nocapture --test-threads=1
cargo test -p scour-source-fs --release the_parallel_walk_finds_every_directory
cargo test -p scour-source-fs --release a_directory_that_appears_during_the_walk
```
## 2026-08-16 — a filter rail counted through its own filter

Reported as "selecting one filter in the left sidebar zeroes the counts of all
the others". It did. The service answers a facet with the keys that matched and
no others, so under a `kind:` term the kind group comes back as a single key:

```sh
scour facets "" --by kind            # 13 keys: doc 459,677 … video 81
scour facets "kind:image" --by kind  # one key: image 200,000 (capped)
```

In the window, on a live index of **2,238,943** entries: selecting `kind:image`
left **12 of the 13** rows in the rail reading `0`, and clicking the 27-day bar
left **21 of the 24** bars flat — which erases the control for widening the
range. Both are true and both are useless: a rail exists to say what switching
would give, and that question is about the query *without* the term the rail
itself put there.

**The trap in fixing it.** `NativeIndex::facets` picks its scan cap from the
questions asked — `AGE_SCAN_CAP` (unbounded) when an age band is wanted,
`FACET_SCAN_CAP` (200,000 rows) otherwise. So asking the stripped query for
`by=kind` alone, which is the obvious way to write the second call, moves the
rail onto the sampled path, and that sample is not proportional. It is the
first 200,000 rows the walk reaches. On the empty query, exact against sampled:

| kind | exact | sampled | share of true |
|---|---|---|---|
| image | 210,551 | 1,430 | 0.007 |
| build | 304,872 | 50,508 | 0.166 |
| video | 81 | 22 | 0.272 |

A proportional sample would put every ratio at 0.089. Thirteen plausible wrong
numbers is worse than thirteen honest zeros, so every facet call keeps
`by=kind,age`. The extra group is close to free — the walk is the cost and the
counting is not, which is why `facets` answers several questions from one walk.

**What it costs.** Medians of three, driven in the real window with
`scripts/scour-app` and `scripts/probe`, counting only the round the query
change asked for. Latency is the slowest of the parallel calls, which is what a
person waits for; service time is all of them added up.

| leg | facet calls | latency ms | service ms |
|---|---|---|---|
| to a blank box | 1 → 1 | 312 → 226 | 312 → 226 |
| to `rapor` | 1 → 1 | 30 → 17 | 30 → 17 |
| click `kind:image` | 1 → 2 | 62 → 247 | 62 → 297 |
| click the 27-day bar | 1 → 2 | 111 → 556 | 111 → 737 |
| both together | 1 → 3 | 27 → 182 | 27 → 416 |

**The unfiltered legs are untouched**, and those are the ones every keystroke
goes through: with no `kind:` or `dm:` term the three questions are about the
same rows and it is the single call it always was. A second is paid for only by
somebody who has clicked a filter, a third only by somebody who has clicked
both. The two unfiltered rows differ by noise, not by work — the same query is
asked in both.

Idling for 30 s on `rapor kind:image`, which is the narrow filtered query that
stays under `SIDEBAR_LIVE_UNDER_MS` and therefore keeps refreshing: scourd at
**4.23% → 4.37%** of a core (medians of three, alternating; **0.40%** with no
window open at all), facet service time **211 → 214 ms** per 30 s, and facet
calls **10 → 8**. The call count did not double because the cost is charged for
the whole set rather than the first answer back, so `atMostEvery` backs the
refresh off to half as many rounds. Sampling the rail every frame through a
query change, the window in which the previous query's numbers are still on
screen was **26.8 → 28.8 ms** — unchanged, and pre-existing: it is the gap
between the keystroke and the search reply that runs `clearSidebarCounts`.

## 2026-08-16 — the export was quadratic because it was in the wrong process

`a7789d8` put `/api/csv` in the browser bridge, where the only way to reach the
whole result set was to page the service. A page costs what it takes to walk to
its offset, so the total is quadratic:

| offset | ms |
|---|---|
| 0 | 2.1 |
| 100,000 | 25.3 |
| 500,000 | 65.5 |
| 1,000,000 | 117.6 |

Half a million rows took **71 seconds**; the whole index wrote 1.4 M lines in
**ten minutes** and had not finished, which is why that version stopped at half
a million and said so in a trailer.

Moved into the service as `Request::Export` — one request, one walk, rows
written as they are produced.

**The corpus.** A copy of the live index, `cp -a --reflink=auto`, served by a
`scourd` of its own on a socket of its own with `watch = false` and
`scan.on_start = false`, so nothing moves under the measurement. 2,248,592
rows, 3 segments, 213 MiB. **Every number below is that copy, not the live
index.**

Three things had to be right before the copy behaved at all, and each produced
a plausible wrong number first:

* **Both sources have to be named in the config.** Rows carry a source id, and
  a source that is no longer configured has its rows forgotten at startup. A
  config naming only `home` opened the copy and answered **889,527** rows where
  the service it came from answers 2,240,389 — the missing 1.35 M were
  `/mnt/depo`'s, erased on open. It reads exactly like a torn copy and is not
  one.
* **The default is a watched home directory**, not no sources. The first run
  had a live watcher applying real changes into the copy: 2,240,275 rows became
  2,248,599 and the segments went 4 → 5 between two counts.
* **A copy is a snapshot of a disk, not of a service.** Even settled, counts
  differed from the live service's by a few thousand until the staged rows
  committed.

**What it costs.** Writing to a file on the same machine:

| | rows | wall | bytes |
|---|---|---|---|
| whole index, `scour export` | 2,248,592 | **3.60 s** | 332,793,596 |
| whole index, `/api/csv` | 2,248,592 | **3.75 s** | 332,790,966 |

Against ten-minutes-and-unfinished, and against 71 s for the half million the
old version could reach. The bridge's extra 0.15 s is the HTTP hop and the JSON
frames.

**What it costs in memory**, which is the number that decides whether the shape
is right. `RssAnon + VmSwap`, sampled every 50 ms, because glibc keeps freed
arenas and plain RSS is not the number:

| process | before | after | peak during |
|---|---|---|---|
| service | 46,016 kB | 51,096 kB | 51,040 kB |
| bridge | 524 kB | 456 kB | 524 kB |

`VmHWM` did not move: 216,504 kB for the service across the whole run — it is
dominated by the mapped index — and 4,676 → 4,628 kB for the bridge. **The
bridge holds nothing.** It ended a 317 MB download using less than it started
with, which is what a relay looks like. The service's five megabytes are the
128 KB frame buffer and what the allocator kept around it.

For comparison, the shape that was rejected without being written: ordering the
whole set needs a key per match held until the last match is seen, and a
`SortValue` is 32 bytes because one of its shapes is a `Vec`. On this corpus
that is ~90 MB for the buffer alone and **559 MB of peak RSS** sorted by path —
measured further up this file, and the regression this week's work removed. See
`NativeIndex::scan` for what an ordered stream would take instead.

**Counts, against the engine's own**, four shapes including one that matches
nothing:

| query | `scour count` | export rows |
|---|---|---|
| `kind:font` | 6,490 | 6,490 |
| `kind:archive` | 12,209 | 12,209 |
| `ext:rs size:>1mb` | 12 | 12 |
| `zzz-nothing-matches-this` | 0 | 0 |

The empty export is 30 bytes: the byte-order mark and the heading row. A
spreadsheet with no rows says "nothing matched"; a zero-byte download says the
export broke.

**The bytes are unchanged.** `kind:font` fetched from `/api/csv` on a bridge
built at `a7789d8` and on one built now, both pointed at the same service:
1,117,358 bytes each, and identical line for line once sorted. Only the order
differs, which is the one thing that changed. Same headers, same `ef bb bf`.

**A cancelled download stops the walk.** Killing `curl` 0.4 s into a
whole-index download: the service spent 40 ticks during it and **zero** in the
two seconds after, against the ~350 a full export costs. The refusal travels
from the failed socket write through `Client::stream` and `Engine::export` into
`Index::scan`, which abandons the walk.

## 2026-08-17 — a grid of files nothing had ever previewed

The new grid modes drew a blank tile for every file with no cached thumbnail,
which is every file in a folder nobody has opened in a file manager. Scour now
asks the desktop's own thumbnailer for the ones it can see. The question is what
that costs when a screenful of unseen files arrives, and the answer has to be
measured because "only what is on screen, and only once scrolling stops" is
otherwise unfalsifiable — so `Request::Thumbnails` carries `ran`, the number of
processes the service actually started, all the way back to the page.

### The instrument

Nothing here touched the owner's service, index, thumbnail cache or bridge port.
Its own everything, under `/var/tmp/scour-thumbs-probe`: `XDG_DATA_HOME`,
`XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, a socket of its own, port 7639, and a
source holding only generated images.

**Headless Chromium, and that is not a shortcut.** An on-screen window on this
desktop is occluded by everything else running, and a compositor stops producing
frames for a surface nobody can see. Measured on the on-screen one:

```
{"visibility":"visible","hidden":false,"focus":true,"framesInOneSecond":0}
```

`document.hidden` is false, `visibilityState` is `visible`, the window has
focus — and `requestAnimationFrame` never fires. The list paints inside one, so
the page sat holding a perfectly good answer it had not drawn, and
`Input.synthesizeScrollGesture` hung forever waiting for a frame that was not
coming. `Page.bringToFront` did not fix it. This is the trap already written
down here in a longer form: a window can be visible and get no frames, and
anything read out of a frame that never arrived is a lie.

### Nothing while it moves, and then only the screen

3,000 images, grid of small tiles, cold cache. Twelve flicks
(`Input.synthesizeScrollGesture`, 6,000 px at 20,000 px/s) back to back, with
the counters sampled every 25 ms from inside the page — polling from outside
leaves a round trip between samples, and a gap longer than the page's 250 ms
quiet time would mean the "still scrolling" state was never tested.

| t (ms) | requests | paths asked | processes | scrollTop |
|---|---|---|---|---|
| 25 | 0 | 0 | 0 | 0 |
| 1,002 | 0 | 0 | 0 | 16,475 |
| 1,975 | 0 | 0 | 0 | 31,890 |
| 2,300 | 0 | 0 | 0 | 34,133 ← stops |
| 2,625 | 1 | 24 | 0 | 34,133 |
| 3,276 | 2 | 48 | 48 | 34,133 |
| 4,250 | 4 | 84 | 72 | 34,133 |
| 4,575 | 4 | 84 | 84 | 34,133 |
| 8,300 | 4 | 84 | 84 | 34,133 |

**34,133 px of a 3,000-tile grid, and not one request.** The first ask lands
325 ms after the scrolling stops — the 250 ms quiet plus a frame. Then four
requests for the 84 loaded tiles on screen, 84 processes, and flat for the next
four seconds. Nothing accumulates, because the batch is rebuilt from what is
visible each time rather than drained from a queue built while it moved.

```bash
python3 /var/tmp/scour-thumbs-probe/scroll.py "under:.../pics/big"
```

### The switch is a real off

Same rig, 60 never-previewed images, the window's picture switch off, six
seconds:

| | requests | paths | processes |
|---|---|---|---|
| pictures off | **0** | 0 | 0 |
| pictures on | 3 | 60 | 60 |

Not "requests that draw nothing" — none at all. `--no-thumbnails` on the bridge
is the operator's equivalent and answers 403; reading pictures that already
exist is not behind it, and still answers 200.

### Attempted once

Eleven files nothing can draw, asked for three times through `/api/thumb`:

| ask | processes |
|---|---|
| first | 5 |
| second | 0 |
| third | 0 |

Five, not eleven: three `.rs` and three `.log` never reached a process at all,
because `make` is answered from the machine's own MIME and thumbnailer tables
with no I/O and no spawn. The five corrupt PNGs *do* have a declared
thumbnailer, so each was tried once and each left a note in
`thumbnails/fail/scour/`. Editing one of them:

| | processes |
|---|---|
| after the file changed | 1 |
| again | 0 |

The note records `Thumb::MTime`, so a file that has been edited since it failed
is worth exactly one more try.

### The cache is the shared one

`GnomeDesktop.DesktopThumbnailFactory.lookup(uri, mtime)` — the call GNOME
Files makes — accepted every thumbnail the window produced, 6 of 6 sampled,
with `Thumb::URI` and `Thumb::MTime` matching the original and `Software:
Scour`. Files are `0600` and directories `0700`, as the standard asks.

**The negative control matters more than the positive one.** The identical
picture with its text chunks stripped out, same bytes otherwise:

```
same picture, metadata removed -> lookup: None
```

So the acceptance above is the metadata being checked, not the file merely
being present. `glycin-thumbnailer` writes none of it — measured, both fields
`None` — which is why the two chunks are written here rather than assumed.

## Applying a rule without walking — 2026-08-17

A rule that is *added* can only take entries out, and the index already holds
every path the answer is about. So `Engine::apply_rules` reads the index and
deletes what the rules now skip, and goes nowhere near a disk. What that is
worth, on a **copy** of the live index (`cp -a --reflink=auto`, 2,249,785
entries, 215 MiB, 8 segments), against a service configured to walk nothing —
`scan.on_start = false`, `watch = false` — so the number is the pass and
nothing else:

| pass | dropped | time |
|---|---|---|
| a rule matching nothing | 0 subtrees | **2,115 / 2,082 / 2,070 ms** |
| `dir:.cache` | 5 subtrees, **78,995 rows** | **2,379 ms** |
| `dir:.cache` again | 0 subtrees | 2,003 ms |

The comparison is not a walk of the same index, which was deliberately not run
on the owner's machine; the reference is the 2026-08-03 first scan above —
1,197,514 entries on `/home/hasan` at **6.7 s** — and this index is 2.24 M
across two volumes, the second of them ntfs3.

Idempotent, and the third row is the point: a pass that reports removals it did
not make would report them again for ever. `tests/smoke.rs` holds that property
over 120 subtrees.

### It was 5.5 s and wrong, and the difference was `is_dir`

The first version asked `Rules::excludes_path`, which is the *watcher's*
filter: no `is_dir`, and generous by design, because there a wrong `false`
costs one check and a wrong `true` costs a row that never updates again. As a
test for what the index should contain it is a different question, and the
answer differs in a real place:

```
/home/hasan/.local/share/containers/storage/overlay/<hash>/diff/usr/share/node_modules
  -> nodejs        (a symlink, 6 B, indexed as a file)
```

There are **36 of these**. A `dir:` rule is not about them, so the walk indexes
them and `excludes_path` says it should not — so every rule change deleted 36
rows, and the next walk put them back. Visible as a sequence that never
settles: 73 subtrees dropped, then 36, then 36, then 36.

Asking `Rules::excludes(path, name, is_dir)` — the same call the walk itself
makes — takes it to 0, and takes the pass from **5.5 s to 2.1 s**, because most
of that time was removal work that should never have happened.
