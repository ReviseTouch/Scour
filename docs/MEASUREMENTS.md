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
