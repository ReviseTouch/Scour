# Measurements

Every number the README claims, with the command that produced it. The full
laboratory notebook — 6,500 lines, in the order things were found, including
the ideas that were measured and rejected — is in git history:
`git show dabcebc:docs/MEASUREMENTS.md`.

## The rule

1. **Alternate.** Two binaries, run turn about; this machine drifts more than
   10% across a day, so a morning is never compared with an afternoon.
2. **One binary per experiment.** Copy it out of `target/` before changing
   anything; an A/B was once invalidated by a rebuild halfway through.
3. **Check what else is running.** A `cargo build` writes tens of thousands of
   files into a watched source and reads as the service's own cost. So does an
   open window.
4. **Measure the broad case.** `facets` is 2.6 ms on a narrow query and 810 ms
   on the empty one; measuring only the first hid a tenth of a core.

## The live index, 2026-09-09

`scripts/bench` against the running service: 4,842,854 entries, 606,640
folders, 493 MiB on disk, six segments. Whole round trips — socket, parse,
search, sort, count and forty rows — median of three.

| query | ms | rows read |
|---|---:|---:|
| `rapor` | 7.85 | 288,800 |
| `a` | 5.82 | 176,920 |
| `size:>10mb` | 7.48 | 205,762 |
| `kind:code dm:7d` | 16.65 | 723,650 |
| `under:"/home/u/Projeler" kind:code` | 23.97 | 727,199 |
| `kind:image` | 37.29 | 1,649,361 |
| `ext:pdf` | 77.19 | 418,272 |
| `path:Projeler` | 110.20 | 206,690 |
| `Projeler/Scour` (a term with a `/` searches the path) | 179.84 | 4,870,812 |

The terminal face draws its first frame in 7–14 ms from launch.

## What each request costs

Service time on 3.3 million entries, window open and untouched (2026-08-25):

| request | service | wall |
|---|---:|---:|
| `count` (empty query) | — | 1.9 ms |
| `facets kind` | 8.5 ms | 9.8 ms |
| `facets ext` | 30.3 ms | 31.8 ms |
| `stat`, `places`, `rules`, `status`, `stats`, `explain` | — | 1.1–1.7 ms |
| `tree /home/u` (depth 1, 50 children) | — | 112 ms |
| `du` (whole index) | 416 ms | 425 ms |
| `du /home/u/Projeler` | — | 76 ms |

`tree` is one `parent:` count per child — its cost is the number of children,
not what each one counts. `du` scales with the scope, not with `top`.

## Ordering, and the unsorted tail

Rows are stored newest first, so the default view stops at the first page. That
order is rebuilt, never maintained: a week of running left 911,973 unsorted
rows, and `scour maintain rebuild` took a path-ordered query from **21.5 ms to
1.9 ms** and the index from 354 to 305 MiB. `scour stats` reports the tail.

Since the path, name and extension orders are stored in every segment, the tail
costs nothing measurable (2026-09-23): 4.54 M rows in 13 segments with 578,000
unsorted, then rebuilt into one in 47 s, answered fourteen query shapes in the
same times within the noise.

## The service over a day

Measured on the live service with `pkexec`, alternating two builds
(2026-09-05):

| | before | after |
|---|---:|---:|
| CPU, a window attached | 10.72% of a core | **5.17%** |
| writes, a window attached | 103 MB/min | **49 MB/min** |
| CPU, nobody looking | 0.41% | 0.78% |
| writes, nobody looking | 20 MB/min | 14 MB/min |
| peak resident (`VmHWM`) | 1,175 MB | **593 MB** |
| query p50 | unchanged | unchanged |

A commit is about ten `fsync`s — 22.5 ms for one row, 23.7 ms for 128 — so the
price is per commit and the rows are free. With a face attached the index
commits every second; alone, every fifteen. That difference is the 26× between
the two CPU rows.

Memory is flat: `RssAnon + VmSwap` 203 MB at 14 minutes, 205 MB at 25. The
mapped segment files are `RssFile` and the kernel drops them under pressure.

## Publishing, then persisting (2026-09-24)

A change is published: it becomes a segment in memory, searchable at once, and
a removal is made in memory. Every thirty seconds, or when the service goes
quiet, the published segments are folded into one and written together with
the live bitmaps and the manifest. A crash loses what was published since, and
the next walk finds it again. Before, the once-a-second commit wrote a segment
and synced about fourteen files, and every walk's start wrote and synced
another with the index's write lock held.

A client waiting on changes as a window does, 50 file writes a second in a
watched directory, the two builds alternated twice, 90 s each:

| | before | after |
|---|---:|---:|
| writes | 69 / 64 MB/min | **6.2 / 7.4 MB/min** |
| segment files created | 58 / 55 a minute | **5.3 / 5.3 a minute** |
| CPU | 8.3% / 9.5% | 8.1% / 9.1% |
| search p50 | 14 / 12 ms | 12 / 11 ms |

The CPU did not move: the commits were disk time, not processor time.

A subtree walk — a directory appearing, dozens a minute during a build — still
synced two or three files with the lock held: the manifest when its rows were
published, because a begun walk leaves it dirty and a publish saved a dirty
manifest, then the live bitmaps and the manifest again at its sweep. On a disk
busy with something else each sync took seconds and every search waited. Now a
sweep removes in memory, as a publish does, and only a durable write saves the
manifest. A build's load — a directory of twenty files every half second, one
removed ten seconds later — with a waiting client and a search every 100 ms,
alternated twice:

| | before | after |
|---|---:|---:|
| writes | 41.3 / 41.3 MB/min | **7.0 / 4.6 MB/min** |
| worker found waiting on a sync | 1.3% / 0.4% | **0.1% / 0.0%** |
| CPU | 10.7% / 27.0% | 19.7% / 10.1% |
| search p99 | 26.8 / 48.5 ms | 46.1 / 13.3 ms |

The CPU and the slowest searches follow the whole-source walks that landed in
each window, not the build — see the next section.

## Walking again (2026-09-24)

Each whole walk now leaves a line in the journal:

```
scourd: walked home in 3.8 s — 2545690 entries, 88 gone, 64 MB read from disk, 493 unreadable, first …
```

That line found the walks repeating themselves. A walk that met a directory it
could not read counted as incomplete, and an incomplete walk is repeated: at
twenty times its own duration, then two minutes, ten, an hour. A container's
root-owned data under the home directory refused 493 directories and `/etc`
refused 82, so every whole walk of both sources repeated itself for as long as
the service ran. Right after a start the home directory was walked three more
times in five minutes, and one 55 s window with a repeated walk in it cost
15.2 CPU-seconds and 353 MB read from disk. A subtree walk that met a refusal
queued a walk of its whole source.

A refusal, or a directory gone before it could be read, is what the next walk
meets too, so neither asks for a retry now; the rows under them are spared as
before. An I/O error or a full file table still does.

Warm, the three sources here walk in 3.8 s (2.55 M entries), 2.7–5.7 s (1.56 M,
NTFS) and 0.5–1.5 s (0.5 M).

Every source used to be walked whole every thirty minutes, watched or not. One
such round here, the machine under memory pressure: the home directory 14.6 s
and 2.5 GB read, the NTFS volume 27.9 s and 2.0 GB, the system directories
2.6 s — 26.8 CPU-seconds and 5 GB of reads, about 10 GB an hour. The NTFS
volume found nothing in any round; the home directory, once two watcher bugs
were fixed (a `.` row for every directory event, and a directory's own times
left behind its children), found only files written through a memory mapping —
SQLite `-wal` and `-shm`, a browser's cache, journald — which no watcher sees.
A watched source is now checked once a day, when the kernel's pressure figures
say the processor and the disk were waited on for under a tenth of the last
minute, with one walker thread; a day late if that never happens. A source
nobody watches keeps the thirty minutes.

The first walk into an empty index shows what it has found after one second,
then after two, four, eight and every sixteen, and wakes a waiting window each
time. Before, a window open on a fresh install heard nothing until the whole
home directory had been walked, and the rows it could have shown arrived a
hundred thousand at a time.

## The watcher

One `fanotify` mark per volume, placed by `scour-watch` with `CAP_SYS_ADMIN`
and handed down. The unprivileged alternative does not fit: this machine has
~609,000 directories, `fs.fanotify.max_user_marks` is 295,420 and the inotify
budget 524,288 — and the inotify budget belongs to the session, so what runs
out is the next editor's.

The directory map keeps names and parents rather than paths (2026-09-06):
**87.8 MB → 39.6 MB** for 604,000 directories, a rename of a subtree with 781
descendants **17.4 ms → 2.3 µs**. A path lookup went from 114 ns to 372 ns — at
the module's busy rate of 2,120 events a second that is 0.15% of a core.

```sh
cargo test -p scour-source-fs --release directory_map_memory_probe -- --ignored --nocapture --test-threads=1
```

## Building an index

`examples/build_cost.rs`, medians of five alternating runs (2026-09-05):

| workload | wall ms | CPU ms | peak RSS |
|---|---:|---:|---:|
| 300,000 distinct directories | 275 → 181 | 274 → 180 | 160 → 108 MB |
| 1,000,000-row segment | 1,326 → 1,164 | 1,321 → 1,157 | 175 → 165 MB |

Same output size and fingerprint before and after.

## A spinning disk

Ubuntu 24.04 guest, 300,000 files in 2,705 directories on a virtio disk that
QEMU throttles to a fixed IOPS ceiling (`block_set_io_throttle`; 120 is a
7200 rpm class, 60 a 5400 rpm laptop), the index on an unthrottled disk, cold
cache, two runs each (2026-09-21):

| | unthrottled | 120 IOPS | 60 IOPS |
|---|---:|---:|---:|
| first walk, wall | 2.9 s | 46.4 s | 94.6 s |
| reconcile with a warm index | — | 45.7 s | — |
| reboot → search answers | — | 54 s | 101 s |
| a 4 KiB read on the same disk, median / p90, idle | 3 / 12 ms | 2 / 12 ms | 4 / 12 ms |
| the same during the walk, one thread | 1 / — ms | 33 / 160 ms | 34 / 290 ms |

The walk reads about 100 MB of metadata whatever the setting; wall time is a
function of the IOPS ceiling alone. A warm index does not help: the reconcile
costs what the first walk cost, so `scan.on_start` on a spinning disk is paid
at every start. Walker threads under a seek limit (`[scan] threads` 1, 2, 4,
8): wall 46.2–46.9 s, flat; the read probe's p90 160 → 290 → 400 → 820 ms.
One thread is the right number for the disk's other users, not for the walk.
`IOSchedulingPriority` did nothing under the `none` scheduler, as under kyber.
Not measured: real seek latency (the throttle is a uniform tax, so the
multi-thread penalty is understated), the index on the slow disk, a watched
steady state.

* Newest-first storage: the default view went from 14.8 ms to 0.022 ms in the
  prototype.
* Displayed text in the document store, not in columns: 0.32 µs a row against
  14.33 µs.
* Every ancestor directory is a token: deleting a subtree of 378,100 rows is
  one term, 1.3 µs.
* The trigram filter, the zone map and the 32-row block were each measured
  against the brute-force oracle in `crates/scour-index-native/tests/whole.rs`,
  which compares every query shape against `scour-mock::brute_force`. That
  comparison found three bugs a timing benchmark had called a success.

## The system disk as a source (2026-10-13)

What indexing every disk adds on this machine: `/` with the home directory and
`/mnt` left out, into an index of its own, the cache warm from daily use.

```
printf '[index]\ndir = "/var/tmp/r/index"\n[[source]]\nname = "system"\nroots = ["/"]\nwatch = false\n[exclude]\npaths = ["/home", "/mnt", "/run/media", "/media"]\n' > /var/tmp/r/config.toml
time target/container/release/scourd --config /var/tmp/r/config.toml --scan-only
```

| | |
|---|---:|
| entries | 622,344 |
| walk | 3.4 s |
| CPU | 1.5 s user, 2.0 s system |
| read from disk | 473 MB |
| index | 49 MB |

Unwatched, the walk is repeated when the disk's write counter moves, resting at
least twenty times its own cost: at most every 68 s while the machine is busy.

## A path term skips blocks (2026-11-02)

A term with a separator can only match a row whose folder holds it, or ends
with the part before its last slash. Those folders are known once a query, so
a block whose folder numbers hold none of them is not read — the zone map
`under:` already used. Two services side by side on two copies of this
machine's index (6.29 M entries), queries alternated, median of seven:

| query | before | after | rows read before → after |
|---|---:|---:|---:|
| `Projeler/Scour` | 146 ms | 61 ms | 5.77 M → 0.84 M |
| `projeler/scour/crates` | 147 ms | 54 ms | 5.77 M → 0.28 M |
| `Belgeler/rapor` | 148 ms | 61 ms | 5.77 M → 0.84 M |
| `src/main` | 149 ms | 69 ms | 5.77 M → 1.26 M |
| `depo/PROJELER/Colpan` | 116 ms | 46 ms | 4.12 M → 0.18 M |
| `a/b` | 149 ms | 112 ms | 5.77 M → 3.27 M |
| `/home/hasan/.config` | 59 ms | 47 ms | 0.91 M → 0.22 M |

The same matches in every row. What is left is mostly reading the folder table
once to find those folders.

## A page deep in a text order, and a count kept apart (2026-11-17)

Two builds alternated on one copy of this machine's index (5.0 M entries, eight
segments), each query five times, the median of the second pass.

A page far down the list in name or path order was walked to: a merge key
built for every row above it. Every segment stores both orders, so the row at
an offset is found by bisecting them on a key until the offset falls in that
key's tie group; the rows around it are ordered by the merge's own comparator.
188 pages at fixed pseudo-random offsets, both orders, both directions, came
back identical to the walk's — in 1.5 s against 101.2 s.

| page of 40 | before | after | keys read after |
|---|---:|---:|---:|
| name, offset 3,000,000 | 529–547 ms | 8–12 ms | 1,100–1,700 |
| name ascending, offset 4,000,000 | 734–789 ms | 7.5–8.2 ms | 1,300–1,800 |
| path, offset 3,000,000 | 895–951 ms | 7.9–8.3 ms | 800–1,000 |
| path, offset 100,000 | 43–73 ms | 7.9 ms | 1,400–1,700 |

The walk also held a key for every row above the page: 480 MB of anonymous
memory for path order at offset three million, on the 6.97 M-entry index.
Still walked to: a filtered query, any hidden prefix, the size and extension
orders.

A name-reading query in a stored text order streamed that order until the
count cap was met, a name read at random per position, and above a cap of a
few thousand gave the stream up for a walk of every row. The stream now stops
at the page and the total is counted in row order. The window types with a
cap of 1,000 and was already quick; the command line and the MCP server ask
for 100,000.

| query, count cap 100,000 | before | after |
|---|---:|---:|
| `a` by name | 100–102 ms | 6.5–7.2 ms |
| `a` by path | 303–324 ms | 6.5–7.4 ms |
| `a` by extension | 411–437 ms | 5.7–6.8 ms |
| `!a` by path | 243–302 ms | 5.8–7.0 ms |
| `e` by path, cap 10,000 | 374–381 ms | 3.1–3.4 ms |
| `kind:folder` by name, by path | 15, 17 ms | 15, 17 ms |
| `rapor` by name | 7.2 ms | 7.6–8.0 ms |

`ext:` read each row's spelling from its block's start to decide whether the
extension counts, before comparing it. Compared first, read after:

| query | before | after |
|---|---:|---:|
| `ext:pdf` by name | 76 ms | 19 ms |
| `ext:pdf` by size | 75–77 ms | 20 ms |
| `ext:rs;toml` by size | 99 ms | 20–21 ms |
| `ext:pdf` by extension | 143 ms | 89 ms |

```sh
cargo run --release -p scour-index-native --example deepcheck -- /tmp/idx/native > reach.txt
SCOUR_NO_REACH=1 cargo run --release -p scour-index-native --example deepcheck -- /tmp/idx/native > walk.txt
```

## A walk in parts, and a path term in the name (2026-11-17)

Same copy, same method. A walk that has to see most of a large segment — a
regular expression, a term too short for the trigram index, relevance, a text
order no stream can answer — is made over equal shares of its candidate
blocks, a thread each, up to eight; each part keeps its own page and count
and the merge orders them as it orders segments. Only above 16,384 candidate
blocks (half a million rows), and a stored-order walk reads a sixteenth of
its blocks first, where a query with common matches is already done.

| query, count cap 1,000 | one thread | eight |
|---|---:|---:|
| `regex:^ab.*\.rs$` by name | 146–150 ms | 46–50 ms |
| `regex:^ab.*\.rs$` by date | 147–148 ms | 54 ms |
| `a` by relevance | 171–179 ms | 45–49 ms |
| `ab` by relevance | 91–94 ms | 23–24 ms |
| `zq` by name | 82–84 ms | 23 ms |
| `zq` by date | 48–50 ms | 25–26 ms |
| `a` by date, `rapor`, `*.rs` by name, the empty query | unchanged | unchanged |

Sixteen threads: `zq` by name 18.7 ms, the regular expression no faster.

`path:` with no separator can match in the name, so no block could be
skipped on its directories alone. The trigram index says which blocks' names
can hold the term; a block is read when either can.

| query by name | before | after | rows read |
|---|---:|---:|---:|
| `path:xyzzyq` (nothing) | 148–151 ms | 26 ms | 5.08 M → 0 |
| `path:scour` | 146–149 ms | 37 ms | 5.08 M → 0.31 M |
| `path:rapor` | 150–152 ms | 50–52 ms | 5.08 M → 0.74 M |
| `path:Projeler` | 143–147 ms | 49–50 ms | 4.98 M → 1.99 M |
| `path:Projeler` by path | 226 ms | 90 ms | |

The 26 ms left under every path term is the directory table folded and
searched once a segment a query: 553,525 directories.

## A deep page in a numeric order is gathered, not kept (2026-11-18)

Scrolling the window two million rows down a size order sent the service from
25 MB to 1.2–1.5 GB of anonymous memory for a few seconds at a time. The order
is not stored, so the walk kept every row above the page to know where the page
began — 48 bytes a row — and the window asks over three connections at once.

Now, from offset 300,000 in any walked numeric order (size, created, accessed,
kind, and the rest), one pass counts the keys into 2^19 logarithmic bins (a
value below 4,096 is a bin of its own, above that a 4,096th of its magnitude),
which places the page and the rows before it; a second pass gathers only the
page's bins, at most 400,000 rows of 24 bytes. A bin fuller than that — one
value shared by most of the index, as kind has — is split by date in one more
pass. Peak anonymous memory, sampled every 2 ms during one query, on the 5.0 M
entry copy:

| query, order, offset | before | after |
|---|---|---|
| everything, size, 2 M | 271 MB · 288 ms | 3 MB · 279 ms |
| everything, kind, 2 M | 274 MB · 262 ms | 6 MB · 183 ms |
| `a`, size, 1 M | 136 MB · 216 ms | 3 MB · 317 ms |
| `a`, created, 1 M | 139 MB · 289 ms | 9 MB · 386 ms |

A filtered page is slower: the walk it replaces is made in parts on several
threads, and the two passes are one. Below 300,000 the walk stays — at 150,000
it was 102 ms and 81 MB peak (VmHWM) against 270 ms and 55 MB gathered. The
extension order is still walked (410 MB VmHWM at 2 M); it has a stored order
and wants the name order's reach instead.

## The index on disk, a third smaller (2026-11-18)

Where the bytes were, on 5,056,158 entries rebuilt into one segment, and what
each file became. Two indexes built from the same rows by two builds
(`examples/transplant`: the old build dumps, each loads and rebuilds).

| file | before | after | |
|---|---:|---:|---|
| names as spelled | 117.8 MB | 8.3 MB | kept as a difference from the fold |
| names folded | 117.3 MB | 117.3 MB | what a search walks, untouched |
| columns | 79.8 MB | 79.8 MB | |
| trigram postings | 51.4 MB | 51.4 MB | |
| path lookup | 40.4 MB | 24.4 MB | bucketed, 37 bits an entry |
| path order | 20.2 MB | 14.5 MB | 23 bits a row |
| name order | 20.9 MB | 15.2 MB | 23 bits a row |
| extension order | 20.9 MB | 4.4 MB | steps between rows |
| directories, trigram keys, live bits | 10.2 MB | 10.2 MB | |
| **the index** | **478.9 MB** | **325.6 MB** | 94.7 → 64.4 bytes an entry |

Seven names in ten fold to themselves. Of the 1,491,282 that do not,
1,260,655 are capitals — one bit a byte — and 230,576 hold a Turkish `ı`, `İ`
or a neighbour like `Ş`, two bits a byte; 51 are stored whole.

Two services alternated on the two indexes, each query once to warm and five
times, four rounds, the order swapped each round; load average 2.4–3.4. The
median of the rounds' medians, and the spread of those; every answer compared
and the same rows.

| query | before | after |
|---|---:|---:|
| everything by name, by path, at offset 2 M | 0.87, 1.00 ms | 0.87, 0.99 ms |
| everything by size at offset 2 M | 309.8 (308.3–313.3) ms | 311.8 (308.2–314.4) ms |
| everything by extension at offset 2 M | 766.5 (762.9–767.5) ms | 607.1 (600.6–611.0) ms |
| `a` by date, by name, by path | 4.98, 5.77, 6.33 ms | 4.79, 5.52, 6.85 ms |
| `a` by relevance | 35.0 (33.9–36.1) ms | 36.7 (34.3–38.9) ms |
| `ab` by relevance | 18.5 (17.7–18.9) ms | 18.4 (18.0–18.9) ms |
| `rapor` by date, by name | 7.40, 8.09 ms | 6.88, 7.85 ms |
| `ext:pdf` by date | 18.9 (18.3–21.1) ms | 11.1 (9.7–12.8) ms |
| `*.rs` by path | 32.4 (31.7–34.0) ms | 20.3 (19.4–20.9) ms |
| `path:Projeler` by name | 46.0 (45.9–46.4) ms | 45.3 (42.9–46.4) ms |
| `path:Projeler` by path | 91.8 (89.1–112.1) ms | 98.4 (96.3–102.3) ms |
| `Projeler/Scour` | 33.1 (32.8–34.6) ms | 33.2 (32.5–35.7) ms |
| `case:README` by date | 139.6 (138.2–140.9) ms | 6.1 (5.8–7.4) ms |
| `regex:^ab.*\.rs$` by name | 38.8 (38.4–45.3) ms | 38.2 (37.9–39.2) ms |
| `zq` by name | 17.3 (16.6–17.7) ms | 17.5 (17.2–17.9) ms |
| `kind:folder` by path | 19.3 (18.9–20.0) ms | 19.9 (19.6–21.4) ms |
| facets by extension | 24.5 (23.7–25.1) ms | 11.3 (10.8–11.7) ms |
| facets by extension, `rapor` | 11.4 (10.5–11.5) ms | 10.7 (10.1–11.3) ms |
| export of `ext:pdf` | 94.0 (93.5–94.9) ms | 87.1 (86.1–88.1) ms |
| the service's CPU over the query set | 3,769 ms | 2,218 ms |

`case:` asks the fold first, which the walk carries and the trigram index is
keyed on; `ext:` and a count by extension read the fold where it can say what
the spelling would. Two rows moved the other way in most sessions, inside
their spread in each: `kind:folder` by path, 1.1 M packed positions read,
18.4–18.8 → 19.1–19.9 ms at its fastest over four sessions; and
`path:Projeler` by path, +5.4%, +4.3%, −3.7% and +7.2% across them (CPU
+3.8%, +6.8%, −2.8%, +1.8%).

| | before | after |
|---|---:|---:|
| peak anonymous memory over the query set | 230.5 MB | 229.0 MB |
| file pages resident after it | 332 MB | 220 MB |
| open and count (`scourd --check`), 25 rounds | 40.5 (36.1–94.6) ms | 41.3 (36.4–80.2) ms |
| load of 5.06 M rows in ten commits | 6.87 (6.74–7.01) s | 6.89 (6.83–7.03) s |
| rebuild into one segment | 12.66 (12.57–12.75) s | 12.42 (12.39–12.48) s |
| CPU of both | 21.61 s | 21.72 s |
| peak resident of both | 1,109, 1,068 MB | 995, 1,009 MB |
| a pass that finds every row unchanged | 2.19 (2.19–2.29) s | 2.10 (2.07–2.12) s |
| every row written again, the old one retired | 9.52 (9.44–9.56) s | 9.57 (9.31–9.60) s |
| a commit of eight rows, wall | 119.3 (118.7–120.9) ms | 113.5 (113.0–114.0) ms |
| the same, CPU | 10.2 ms | 7.4 ms |

Measured and left out: FSST on the folded names (`fsst-rs` 0.5.11, a table
trained on every 64th block). 116.6 MB became 85.1 MB a block at a time, 80.0
a name at a time — and the walk's name read and search went from 11.8 ns a row
to 26.3, 59 ms to 133 over the five million, decoding alone 69 ms at 1.7 GB/s.
A substring cannot be searched for in the coded bytes without decoding them.
That is 73 ms of CPU on every walk of the whole index — `a` by relevance is
201, a regular expression 239, a short term 96 — for 31 MB.

```sh
cargo run --release -p scour-index-native --example transplant -- dump /tmp/old/native /tmp/rows.bin    # the old build
cargo run --release -p scour-index-native --example transplant -- load /tmp/rows.bin /tmp/new/native    # each build
cargo run --release -p scour-index-native --example transplant -- again /tmp/rows.bin /tmp/copy/native  # nothing changed
cargo run --release -p scour-index-native --example transplant -- replace /tmp/rows.bin /tmp/copy/native
cargo run --release -p scour-index-native --example commit_cost -- /tmp/copy/native 0 <eight paths in it>
```

## Accents set aside, and a word forgiven its mistakes (2026-11-18)

A name is now folded without its marks as well as its case, so the arena a
search walks, the trigrams and the name order all changed value: format 13.
The same 7,391,323 rows carried into both formats (`examples/transplant`) and
rebuilt into one segment, each build twice, turn about:

| | format 12 | format 13 |
|---|---:|---:|
| load in ten commits | 12.46, 12.64 s | 12.80, 12.63 s |
| rebuild into one segment | 22.38, 23.04 s | 22.95, 22.39 s |
| the index | 487.59 MB | 486.99 MB |
| names folded | 181.49 MB | 180.93 MB |
| trigram postings | 75.96 MB | 75.56 MB |
| names as spelled (the difference from the fold) | 9.13 MB | 9.52 MB |

`ş ç ğ ö ü â é ñ` and their capitals are a two-bit code against the letter
they fold to, as `ı` and `İ` were; a name with any other marked letter is
stored whole.

`examples/querycost` on the two indexes, each query once to warm and nine
times, four rounds with the order swapped each round; the median of the
rounds' medians and their spread. Load average 5–10: another build was
running. Every page compared by digest and the same.

| query, count cap 1,000 | before | after |
|---|---:|---:|
| everything by date | 1.11 (1.04–1.14) ms | 1.10 (1.05–1.16) ms |
| `rapor` by date | 0.64 (0.60–0.66) | 0.66 (0.66–0.73) |
| `rapor`, counted to 100,000 | 7.16 (6.82–7.40) | 7.54 (7.35–7.64) |
| `rapor` by name, by relevance | 7.46, 8.03 | 7.74, 8.25 |
| `a`, `ab` by relevance | 69.2, 34.0 | 69.2, 34.7 |
| `main`, `scour` | 0.59, 0.26 | 0.64, 0.26 |
| `ext:pdf` by date, by size | 0.56, 1.04 | 0.60, 1.06 |
| `*.rs` by date, by path | 0.31, 15.5 | 0.32, 15.3 |
| `kind:image`, `kind:code dm:7d`, `size:>10mb` | 4.88, 2.24, 2.10 | 5.04, 2.29, 2.10 |
| `path:Projeler` by date, by name | 43.2, 74.8 | 42.3, 75.4 |
| `Projeler/Scour` | 41.6 | 40.7 |
| `case:README` | 0.95 | 0.97 |
| `regex:^ab.*\.rs$`, `zq`, `len:>60` by name | 67.3, 30.2, 50.2 | 69.1, 30.8, 51.2 |
| `sirket` | 0.39, 129 files | 0.56, 241 files |
| `şirket` | 0.73, 112 files | 0.66, the same 112 |
| `dilekçe` | 0.70 (0.67–0.73) | **1.93 (1.75–2.25)** |
| `dilekçe`, counted to 100,000 | 8.10 (7.43–8.22) | **19.4 (19.1–19.8)** |

A run earlier the same hour put `rapor` at 0.64 → 0.61 and its count at 7.21 →
6.98: the plain rows are inside what the machine does between runs. The two
in bold are not. A term typed with a mark is asked of the fold first and then
of each matching name with its marks, which is a read from the block's start
— 0.6 µs a name, for the 18,958 that match and the ones beside them that do
not. Written from the fold's two-bit codes instead of respelled and folded
again it went from 23.2 to 19.4 ms.

With accents not ignored every term is asked that way (`SCOUR_EXACT_ACCENTS=1`):
`rapor` 0.80 ms and 13.0 counted to 100,000; `sirket` 0.63 ms, the 129 files
it found before.

**A word forgiven its mistakes** — a letter wrong, missing, extra, or two
swapped; one mistake from five letters, two from eleven. A name near the term
still holds most of the term's trigrams, so only blocks holding enough are
read: every window but those over one character, or one of the term's
swapped spellings whole. Five letters and one mistake leave no window that
must survive, and every name is read. A name is first asked for the term's
byte pairs (a load a byte, nothing carried), then for the edit distance kept
as bit columns (Myers; Hyyrö for the swap) — 58 ns a name alone, 16 with the
pairs in front.

| query, count cap 1,000 | ms | rows read |
|---|---:|---:|
| `fuzzy:raporr` | 14.2 | 1.10 M |
| `fuzzy:raporr`, counted to 100,000 (15,881) | 21.8 | 1.51 M |
| `fuzzy:rapor` — five letters, common | 19.9 | 0.43 M |
| `fuzzy:kitap` — five letters, rarer | 95.9 | 7.23 M |
| `fuzzy:xqzvw` — five letters, 27 files | 56.8 | 7.39 M |
| `fuzzy:xqzvwk` — six, nothing | 0.66 | 14,208 |
| `fuzzy:sirket` (255) | 16.0 | 0.90 M |
| `fuzzy:dilekce`, `fuzzy:dilkece` | 1.41, 0.98 | 9,661, 8,093 |
| `fuzzy:istanbul`, `fuzzy:itsanbul` | 6.76, 1.74 | 130,188, 20,779 |
| `fuzzy:muhasebe`, `fuzzy:mahkmeesi` | 3.08, 1.96 | 37,376, 28,808 |
| `fuzzy:durusmatutanagi` — two mistakes | 14.4 | 268,928 |
| `letters:scrgui` (8,688) | 49.9 | 5.73 M |
| `letters:xqzvwk` (37) | 41.2 | 7.39 M |
| `letters:scrgui ext:rs` | 6.34 | 546,848 |

Before the swapped spellings and the pairs, `fuzzy:raporr` read 6.1 M rows in
130 ms, and `fuzzy:xqzvw` took 142 ms (553 on one thread, 215 with the pairs).
A whole-index walk is made in parts on eight threads, as a regular expression
is; `letters:` is always one.

What the default costs: a search that finds nothing counts its near matches
once, to 1,000, and the count stands ten seconds — the `fuzzy:` row above for
the same word, so 14 ms after `raporr` and 57 after a five-letter word nothing
resembles. A search that finds anything pays nothing. With typing mistakes
always forgiven every plain word of five letters or more is a `fuzzy:` row.

```sh
cargo run --release -p scour-index-native --example transplant -- dump /tmp/old/native /tmp/rows.bin   # the old build
cargo run --release -p scour-index-native --example transplant -- load /tmp/rows.bin /tmp/new/native   # each build
cargo run --release -p scour-index-native --example querycost -- /tmp/new/native 9 < queries.txt       # each build, turn about
```

## Reproducing

```sh
scripts/bench [label]                         # queries, service CPU/RSS, first frame → /tmp/scour-bench-<label>.txt
cargo run --release -p scour-index-native --example searchcost
cargo run --release -p scour-index-native --example build_cost -- segment 1000000
python3 scripts/reliability-probe.py 2000     # 2,000 files: create, mutate, replace; checks convergence
```
