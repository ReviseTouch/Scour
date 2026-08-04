# Measurements

Numbers, with the command that produced them. A claim without one of these is
an opinion.

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
