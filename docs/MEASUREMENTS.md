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
  directories against the inotify per-user limit. `Caps::RECURSIVE_WATCH` is
  false on Linux for exactly this reason, and the honest fix is `fanotify`,
  which needs privileges — or falling back to periodic rescans, which nothing
  currently does.
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
