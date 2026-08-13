# Reports: disk usage, duplicates, and a page per directory

Three features that look like three subsystems and are not. Each falls out of
something the index already stores, and the plan below is ordered by what is
free first.

Every number here was measured on the real corpus — 1,217,362 entries of a home
directory — and the commands are in `docs/MEASUREMENTS.md`.

---

## A. What a directory weighs

TreeSize answers this by walking the filesystem, which takes minutes. Everything
it needs is already indexed, and the layout happens to make the answer nearly
free:

* every row carries the number of the directory it sits in;
* directory numbers are handed out in **sorted path order**, so a subtree is a
  contiguous range of them plus the directory's own number — which is why
  `DirScope` has two fields rather than one.

So two passes:

1. **Rows.** One sequential pass accumulating `bytes[dir_id] += size` and
   `files[dir_id] += 1`. Only live rows; the zone map on `DirId` skips whole
   blocks when the report is scoped to a subtree.
2. **Directories.** One pass over the table, which is sorted, so a stack of open
   ancestors reconstructs the hierarchy: a path that is not under the top of the
   stack closes it, and closing adds its total to whatever is below.

```bash
cargo run --release -p scour-index-native --example rollup <index-dir>
```

**Measured: 2.0 ms for the rows and 1.7 ms for the rollup per 100,000 entries**
— about **45 ms for the whole of a 1.2 million entry disk**, and proportional to
the subtree when scoped.

It stores nothing. Caching the two arrays would cost 20 bytes a directory —
2.8 MB at this size — and buy the second query. That is not worth doing until a
measurement says the 45 ms is in someone's way, and the same arrays would then
have to be kept correct across every removal.

### Built, and what the plan got wrong about the cost

`UsageRequest` takes a query, so the report answers "where do my photos sit" as
well as "what is in this folder". A frontend that passes one owes its reader a
word: the figure is then what the *matching* files come to and is no longer
comparable with `du`.

```bash
cargo run --release -p scour-index-native --example report <index-dir> [scope]
```

The plan expected a filter to be nearly free, on the reasoning that the rollup
would then walk only the rows a search walks. It does — and that is not where
the time goes. Measured over 2,228,623 rows in 255,869 directories:

| | whole index | scoped to a folder |
|---|---|---|
| no filter | 376 ms | 57 ms |
| `ext:rs` (12,944 rows) | 224 ms | 13.6 ms |
| `kind:image` (210,508 rows) | 288 ms | 39.5 ms |
| `dm:>1y` (1,629,313 rows) | 332 ms | 62.0 ms |
| matching nothing | 243 ms | 13.9 ms |

**A query that matches nothing still costs 243 ms.** Pass 1 is not the expensive
half: pass 2 is, and it is the same work whatever was asked, because the tree is
the same tree. Every directory is decoded to a `String`, hashed into the map
that merges segments, and sorted — 255,869 of them, filter or no filter. A wide
filter can even lose (`dm:>1y` matches 73% of rows and pays the plan on top).

So a filter buys about a third, and scoping buys an order of magnitude. If the
report is ever to be fast in the unscoped case, pass 2 is the thing to attack:
the paths could be compared as segment-local ids and only materialised for the
few rows that are answered with.

Both `size` and `disk` are already columns, so "size" and "size on disk" are the
same query with a different field.

### What has to be said out loud

* **Hard links are counted once per link**, like `du` without `-l` and like
  TreeSize. Counting them once per inode means holding a seen-set of
  `(dev, ino)` for the subtree — cheap for a folder, 10 MB for a whole disk.
  Report both, name them, and let the caller choose.
* **Sparse files** differ between `size` and `disk`; showing the logical size
  and calling it disk usage is the standard lie and this should not tell it.
* **Cross-device** subtrees mix `dev`; a rollup is per source, and a mount point
  inside a scanned tree belongs to whichever source scanned it.

---

## B. The same file, four times over

"Duplicate" is four different questions, and three of them are already answered.

| question | what decides it | cost |
|---|---|---|
| the *same* file, two names | `dev` and `ino` are equal | free — 227,552 extra links here |
| the same **name** elsewhere | the name arena | free — 658,650 distinct names of 1,564,335 entries, so 2.37 names in three |
| the same **size** | the `Size` column | free — one sort, but see below |
| the same **content** | a digest of the bytes | needs reading the files |

The third is the gate — but not in the way this document first claimed, and the
measurement is worth keeping because it changed the design.

**"A file with a unique size cannot have a duplicate" is true and nearly
useless.** Measured on 1,474,650 files and 493.6 GB: it eliminates **6.2%**.
Not the overwhelming majority. The reason is that **59.3% of files are 4 KB or
smaller** — 22,761 of them are exactly zero bytes — and small files collide on
size trivially. Build output makes this worse, not better.

The gate is worthless by file count and excellent by bytes:

| candidates above | files | bytes they hold | head+tail reads to check |
|---|---|---|---|
| nothing | 1,382,866 | 169.8 GB | 10.55 GB |
| 4 KB | 531,020 | 169.0 GB | 4.05 GB |
| 64 KB | 104,482 | 163.0 GB | 0.80 GB |
| 1 MB | **18,723** | **141.8 GB** | **0.14 GB** |
| 10 MB | 1,919 | 90.9 GB | 0.01 GB |

So the design is not "gate by size then hash" but **work down from the
largest**. Nobody wants a count of duplicates; they want the space back.
Hashing the head and tail of every size-collision candidate over a megabyte
costs **140 MB of reads** and covers **141.8 GB** of the possible waste — a
thousandfold return, and it finishes in seconds rather than the hours a
whole-disk hash would take.

The plan for content, corrected:

1. Group by size, **descending**. Report the potential saving of each group
   before reading anything — that number is free and it is what the user is
   actually asking for.
2. Hash the **first and last 4 KB** of the largest groups first, so the biggest
   savings are confirmed first and the job is useful the moment it is
   interrupted.
3. Only what survives gets hashed in full.
4. Stop when the caller stops caring. There is no reason to descend below a
   megabyte unless someone asks.

Where it lives: a `Digest` column, reserved in the format now and filled by a
**maintenance job**, not by a query — it is I/O bound and belongs beside
`Maintenance::Compact`. That is the same door the design already left open for
document content, and it should use the same one.

A digest column costs 8 bytes an entry if every file gets one, and far less if
only size-collision groups do — which is the only place it can ever matter.
Measure before choosing.

---

## C. The report is not a subsystem

It is the aggregations that already exist, scoped by one term. Point it at a
directory and every panel recomputes, because every panel is a query with
`under:<path>` in it:

| panel | mechanism | measured |
|---|---|---|
| subtree total, biggest children | A | ~45 ms whole disk, less when scoped |
| by kind, by extension | `facets`, already implemented | microseconds |
| age distribution | the `mtime` column, already the row order | ~0 |
| biggest files | sort by `Size` within the scope | 1.5 ms |
| recently changed | the stored order — this is what the index *is* | 0.1 ms |
| never read | `atime` against `mtime` | one column pass |
| duplicates here | B | free through size, a job for content |

Changing directory changes one term. That is the whole mechanism, and it is why
the report can follow a click without a new code path.

**What it cannot do yet** is *history* — "what grew since last week" needs two
snapshots, and the index holds one. The honest version is a job that writes a
small rollup of every directory once a day; a few hundred kilobytes a day at
this size. Worth doing, but only once someone wants it.

---

## D. What it costs the architecture

The rule stays: nothing outside `scour-core` is depended on by more than one
layer.

* **`scour-core`** gains the vocabulary — `Usage`, `DirUsage`, `DupGroup`,
  `DupKind`, `ReportRequest`, `ReportResponse` — and `trait Index` gains
  `usage()` and `duplicates()`, **defaulted to `Err(Unsupported)`** so that
  adding them does not force the tantivy implementation to grow a rollup it has
  no layout for. `Caps` already exists for saying which implementation can do
  what.
* **`scour-index-native`** gains the two passes and, later, the digest column.
* **`apps/scour`** gains `scour du <path>`, `scour dupes [under]`,
  `scour report <path>`.
* **`apps/scour-mcp`** gains `scour_disk_usage`, `scour_duplicates`,
  `scour_report`.

That last line is the strongest argument for building this at all. "What is
eating my disk", "find the duplicates under this folder", "summarise this
directory" are exactly the questions an assistant is asked and cannot currently
answer without walking a filesystem it should not be walking.

---

## E. Order

1. **A — disk usage.** No new bytes, measured, and the biggest visible thing the
   index can already do.
2. **C — the report**, which is mostly assembling A with facets that exist.
3. **B, tiers one to three.** Free. Hard links, repeated names, repeated sizes.
4. **B, tier four.** A digest column and a job. Only after the first three prove
   the feature is wanted.

**Done, and not the way this planned it.** Three corrections worth keeping,
because each was a reasonable expectation that the work disproved:

* **Tier one is gone.** The `dev`/`ino` columns were removed from the index to
  save 19 MB of 200 — see `columns.rs` — so hard links can be counted through
  `Links` and cannot be grouped. Not a regression to fix; a cost that was paid
  deliberately and has to be stated rather than worked around.
* **No `duplicates()` on `trait Index`, and no digest column.** The candidates
  are an ordinary `size:>=N` search ordered by size, and everything after it is
  `crates/scour-dupes`, which takes `(path, size)` pairs and depends on
  nothing. The whole of D's "what it costs the architecture" turned out to cost
  one method on `Engine`.
* **Tier four is a comparison, not a digest.** Same reads, no collision, and
  the thing being decided is which file somebody deletes.

Measured on this disk: 39.16 GiB of size candidates in 84 ms with nothing read,
of which 4.16 GiB survived being read and compared. Those two numbers travel
together everywhere, because the first one alone is an instruction to delete
files that were never copies.

Nothing here needs a file format change except the digest, and that one is
additive.
