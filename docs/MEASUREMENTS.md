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
as bit columns (Myers; Hyyrö for the swap) — 58 ns a name alone, 12 with the
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

## The walker is surefoot (2026-11-19)

The walk left `ignore` for `surefoot`, this project's own walker in a
repository of its own. Same threads, same rules, same rows; what changed is
that a file's `stat` goes through the open directory instead of its full path,
and that idle walker threads wait instead of polling.

The walk alone, `/home/hasan`, 5.61 M entries, cache warm, turn about, medians
of three:

| | 2 threads | 8 threads |
|---|---|---|
| `ignore`, `stat` by path | 3.73 s | 1.18 s |
| surefoot, `stat` through the directory | 2.92 s | 0.98 s |
| no `stat` | 1.4–1.6 s | 0.57 s |

The service, `scourd --scan-only` into an empty index: three sources, 4.89 M
entries, six rounds turn about, other builds running on the machine.

| | before | surefoot |
|---|---|---|
| wall, median (mean) | 15.2 s (15.0) | 12.9 s (13.1) |
| CPU, user + system, mean | 39.8 s | 36.6 s |
| system time, mean | 16.5 s | 14.7 s |
| rounds won | 1 | 5 |

The binary is 465 kB smaller: `ignore`, `globset` and `bstr` are gone.

surefoot can also choose the thread count while it walks (`Threads::Auto`).
Here it lost: five rounds turn about against the fixed two threads, 14.0 s
against 11.9 s at the median, four rounds of five. The indexing consumer is the
limit, so every thread it tried was given back — 9 to 33 changes a source,
down to one thread at times. The scan stays on its measured fixed numbers.

Not measured: a cold cache, a spinning disk, Windows.

## Reading the files a query names (2026-11-20)

`scour grep` against a copy of the index on a socket of its own, a needle that
is nowhere, so every file is read to its end. Eight reading threads on NVMe
btrfs, the page cache warm, other builds running on the machine — a floor for
what the code costs, not a claim about a cold disk.

```sh
scour --socket … --json grep -n 1 --budget-mb 0 --seconds 300 zzqqxx_absent_needle 'kind:code'
```

| query | files | read | first run | second run |
|---|---|---|---|---|
| `ext:rs` | 166,589 | 1.46 GiB | 2.03 s | 0.29 s |
| `kind:code` | 882,362 | 9.73 GiB | 6.05 s | 3.71 s |

**Asking the index for candidates was the cost, not reading them.** A page of
candidates takes the index about 60 ms whatever its size and wherever it
starts (67 ms at offset 800,000, the same with `count_cap` of one). In pages
of 2,048 the 905,000 files of `kind:code` took 47.1 s and 47.8 s; in pages
that grow to 65,536 the same binary otherwise takes the times above. Not
alternated: the two were measured one after the other.

What came first: `scour grep -n 20 unsafe kind:code` answered in 0.07 s after
344 files and 37 MiB.

Partly cold, one run each: 410,174 files under 1 MiB on `/mnt/depo` (ntfs3,
NVMe), binary ones included, 23.7 GiB in the 60 s allowed — 6,800 files and
404 MiB a second; every file under `/usr`, 949,084 of them and 57.1 GiB, in
68.3 s.

Not measured: a cold cache alone, a spinning disk, a network share, Windows;
whether more than eight threads, or `io_uring`, reads a solid disk faster —
the thread counts are the walk's until that is done.

## Deep pages by extension, oldest first, and inside a large tie (2026-11-22)

A copy of this machine's index (6.94 M live rows, seven segments, 1.1 M dead
rows in the largest), `deepcheck` run reached and then with `SCOUR_NO_REACH=1`:
343 pages at fixed offsets, seven orders, every one identical to the walk's —
8.2 s against 313.3 s. Not alternated: the walk is what the extension order
and oldest-first always were, so its column is the before.

| order, empty query | walk, median | reached, median | reached, worst |
|---|---:|---:|---:|
| extension, descending | 1,125 ms | 45 ms | 73 ms |
| extension, ascending | 1,167 ms | 48 ms | 83 ms |
| modified, oldest first | 182 ms | 0.4 ms | 65 ms |
| name, either way | 787–850 ms | 19 ms | 21 ms |
| path, either way | 970–1,315 ms | 14 ms | 15 ms |

**The extension order was never reached**, though every segment stores it.
Hooking it to the name order's bisection was not enough on its own: that
gathered the whole tie group around the page and gave up past 100,000 rows,
and an extension's group is that large as a rule. Name order gave up the same
way where a name is shared widely — 836 ms at offset 3.6 M, measured on the
live service earlier the same day. A group's positions run in row order, and rows are stored
newest first, so the dates fall along the group: a second bisection, on the
date, finds the second the page begins in, and only that second's rows are
gathered. Descending, the group before is read from its start rather than
finished.

**Oldest first is the newest-first reach with the date bisected the other
way.** Inside a date the path still runs forwards, so a segment's part is read
one date at a time, each found by a bisection below the last.

What is left:

- **The rank is rebuilt for every page.** With one dead row in a segment,
  every position of its stored order is read to count the live ones before it:
  most of name's 19 ms, and with the extension order's chunks to decode, most
  of its 45.
- **A second shared by many rows is stepped through.** The 65 ms worst is
  offset 77,777 oldest first: the oldest 77,778 rows share one date, the epoch —
  flatpak's objects and container layers. Newest
  first has the same limit at the end of the list.
- Still walked to: a filtered query, a hidden prefix, the numeric orders other
  than date, relevance oldest first.

```sh
cargo run --release -p scour-index-native --example deepcheck -- /var/tmp/idx/native > reach.txt
SCOUR_NO_REACH=1 cargo run --release -p scour-index-native --example deepcheck -- /var/tmp/idx/native > walk.txt
```

## What a window costs the service as it opens (2026-11-22)

A window opened on the empty query sent the service from 30 MB to 490 MB and
held it there. Two builds alternated twice on one copy of this machine's index
(6.94 M rows), each request from released pages; every answer identical
between them but the age bands, which moved with the clock.

| request | before | after | anonymous peak | file pages read |
|---|---:|---:|---:|---:|
| narrowing line: facets by kind, extension, age | 683–734 ms | 172–198 ms | 2 → 2 MB | 203 → 92 MB |
| narrowing line: usage under home | 654–670 ms | 295–296 ms | 283 → 4 MB | 82 → 82 MB |
| usage under home, `rapor` | 426–427 ms | 16–17 ms | 306 → 5 MB | 89 → 89 MB |
| rail: facets by kind and age | 332–339 ms | 115–118 ms | 3 → 3 MB | 18 → 17 MB |
| first page by size, after a page's folder sizes | 966–985 ms | 309–331 ms | 42 → 49 MB | 246 → 176 MB |
| usage with no scope (`scour du`, the report on everything) | 876–907 ms | 324–327 ms | 299 → 8 MB | 88 → 86 MB |

**The window asked twice a second's work behind every refresh.** The
narrowing line asked again whenever the total moved, and the empty query's
total is every live row, which moves with every file written anywhere. It now
asks once a query, as the page did.

**Facets counted a kind and an age band by string**: a `String` made for each,
for every row, 14 million allocations for the rail. They are counted by number
now. Asked with an age, the count is exact and every name was read for its
extension, all 200 MB of the name arena; on the empty query the extension
order says it instead, a group an extension and one name a group — 6,789 in
the largest segment.

**Usage under a folder rolled the whole tree up** to read two levels of it: a
path and a total for every directory, 700,000 under home. A directory is now
sent to the child of the scope it lies under as the table is read, and a row is
charged to that child. The line also asked for the largest eight by bytes and
picked the two with the most files among them; it asks for every child.

**The size order's folder table read every directory row's name** to find its
path in the table. The table is read once in order instead, which gives every
directory its descendants, and the segment's path digests give each its own
row: 742 rows of 734,121 still read their name, under a folder with a gap below
it. Its anonymous peak is higher by 43 MB, the tables of the pass.

**Usage with no scope rolled the whole tree up too**, to find one maximal
directory, `/`. A directory now goes to the topmost of its ancestors its
segment's table holds, and the few tops merge by path. The roll-up kept as the
fallback had a wrong total besides: its stack closed `/a` at `/a-b`, which
sorts between `/a` and `/a/x`, and handed what is under `/a/x` to the parent.
The two new ways are tested against totals counted from the files.

**And it is given back.** What a query brought in — the index's pages and now
the folder table, which nothing released before — goes back once nobody asks:
after a minute, or a few seconds on a machine under 8 GB, or as Settings →
Performance says. Idle on this machine after a window by size, before this:
146 MB, 130 of it the service's own; idle before any window, 70 MB.

What is left: usage still walks every row's columns, 295 ms; the size-order
table is built again after every release, 300 ms; the opening walk peaks at
126 MB of the service's own and 317 MB of pages.

```sh
cargo run --release -p scour-index-native --example windowcost -- /var/tmp/idx/native /home/hasan
```

## Streaming SIMD grep inside ZIP, Office and UYAP `.udf` documents (2026-11-23)

`scour-grep` inspects `PK\x03\x04` containers in memory when they fit the
1 MiB read buffer (zero extra syscalls, reusable per-thread `zlib-rs` SIMD
inflater), extracts UYAP `.udf` `<![CDATA[...]]>` slices zero-copy, strips
Office/ODF XML runs with `memchr` while skipping embedded media and style
sheets, and recurses into nested `.docx`/`.udf` entries inside `.zip` archives.
Alternated on 5,000 `.udf` and 5,000 `.docx` files from this machine's index
(`mahkemesi`, Turkish `İ`/`ı` case-folded):

| corpus (5,000 files) | `SCOUR_NO_ZIP=1` (before) | SIMD ZIP grep (after) | throughput |
|---|---:|---:|---:|
| `.udf` (27.9 MiB compressed) | 4.14 ms · 4,991 skipped, 4 found | **19.66 ms** · 0 skipped, **2,354 found** | **254,370 files/s · 1,419 MiB/s** |
| `.docx` (84.6 MiB compressed) | 4.97 ms · 4,990 skipped, 0 found | **67.83 ms** · 3 skipped, **195 found** | **73,718 files/s · 1,247 MiB/s** |

Cold-cache first pass on the same 5,000 files across `/home` and `/mnt/depo`:
19.71 ms (`.udf`) and 69.82 ms (`.docx`).

```sh
scour export -o /tmp/udf.csv "ext:udf"
awk -F, 'NR>1 && NR<=5001 {print $2"/"$1}' /tmp/udf.csv > /tmp/udf5k.txt
SCOUR_NO_ZIP=1 ./target/release/examples/zipcost "mahkemesi" < /tmp/udf5k.txt
./target/release/examples/zipcost "mahkemesi" < /tmp/udf5k.txt
```

## The ZIP reader shared with the preview (2026-11-24)

`scour-grep` and `scour-preview` each read a ZIP's directory and inflated its
entries; both now use `scour-extract`'s. The question was whether grep, whose
reader it was, pays for the move across a crate boundary. Two release
binaries of `zipcost`, before (A) and after (B), run alternately seven times
each, warm cache, 1,400 documents (200 each of `.udf .docx .xlsx .pptx .odt
.epub .zip`, 886 MiB, 429 MiB read):

| | median | range | found / clean / binary |
|---|---:|---:|---|
| A, grep's own reader | 136.3 ms | 133.1–152.9 | 366 / 995 / 39 |
| B, `scour-extract` | 138.9 ms | 132.3–143.5 | 366 / 995 / 39 |

1.9 %, inside A's own spread of 15 %: no cost found. Every run found the same
files. Thin LTO inlines across the crate line in a release build.

```sh
for e in udf docx xlsx pptx odt epub zip; do
  scour --json -n 200 "ext:$e size:<64mb" | jq -r '.hits[].path'
done > /tmp/zips.txt
cargo build --release -p scour-grep --example zipcost   # once before, once after
for i in 1 2 3 4 5 6 7; do A mahkeme < /tmp/zips.txt; B mahkeme < /tmp/zips.txt; done
```

## The index split by subject, before and after (2026-11-24)

`index.rs` and `search.rs` were cut into modules and five trait methods became
a call each into an inherent method holding the body. One codegen unit a
crate, so no move should show; measured anyway. `searchcost` from
`2532805` (A) and from the split (B), release, alternately five times each,
on a reflink copy of this machine's index (7,170,522 rows, 7 segments); the
median of each cell, thirteen orders at offsets 0, 2,000 and 19,800:

| query | A, all cells | B, all cells | |
|---|---:|---:|---:|
| `""` | 584.4 ms | 577.9 ms | −1.1 % |
| `rapor` | 432.5 ms | 433.7 ms | +0.3 % |

No cell moved outside its own run-to-run spread.

```sh
cp -a --reflink=auto ~/.local/share/scour/index /var/tmp/idx
git checkout 2532805 -- crates/scour-index-native   # A; then HEAD for B
cargo build --release -p scour-index-native --example searchcost
for i in 1 2 3 4 5; do A /var/tmp/idx/native "$q"; B /var/tmp/idx/native "$q"; done
```

## Content search, Phase 0 (2026-11-24)

What reading costs per format today, cold and warm, and what `pdf_oxide`,
`office_oxide` and Xberg make of 21,298 real documents: `docs/bench/phase-0.md`,
raw numbers in `docs/bench/content-baseline.txt`. In short: text and most
ZIP documents cost nothing; XLSX (24 CPU s for 3,000) and EPUB are the
expensive streamed formats; PDF, DOC, XLS and PPT are not read at all today,
and 2,201 of 3,000 DOC hold a common word.

```sh
derle cargo build --release -p scour-grep --example scan
scripts/content-bench baseline /var/tmp/scour-corpus
```

## An inflater handed only the room it needs (2026-11-24)

**XLSX ten times faster, EPUB three, DOCX half again.** Each reading thread
keeps one output buffer between ZIP entries, and it grows to the largest
entry seen. `flate2`'s `decompress_vec` hands `zlib-rs` the buffer's whole
spare capacity, and the cost was in proportion to it: a 1 KiB part read
after a 30 MiB sheet paid for 30 MiB (mean capacity 12 MiB over 2,000
workbooks, measured inside the branch). `inflate_whole` now gives it a
window of the entry's size. Found while measuring Phase 1's added parts
(sheet names cost 35 % more CPU, all of it in this).

Two release binaries of the `scan` example, before (A) and after (N),
alternated three times, warm cache, needle that is nowhere:

| corpus | A seconds | N seconds | A CPU s | N CPU s | peak MiB A → N |
|---|---:|---:|---:|---:|---:|
| XLSX, 3,000 | 2.48–3.17 | 0.29–0.31 | 19.7–23.9 | 1.94–2.04 | 263–309 → 213–216 |
| EPUB, 1,690 | 1.25–1.53 | 0.43 | 9.5–11.4 | 3.37–3.39 | 57–60 → 53–55 |
| DOCX, 3,000 | 0.056–0.057 | 0.037–0.039 | 0.39 | 0.25 | 35 → 29–33 |
| PPTX, 889 | 0.024 | 0.020–0.021 | 0.17 | 0.15 | 18 → 17 |

The files with a find are the same for `mahkeme` in DOCX, ODT, UDF and EPUB;
N finds four more XLSX, one more PPTX (sheet names, comments, charts) and
two more RTF (now read as text).

```sh
derle cargo build --release -p scour-grep --example scan   # once at each commit
for i in 1 2 3; do A zzqq < doc-xlsx.txt; N zzqq < doc-xlsx.txt; done
```

## Content search, Phase 1 (2026-11-24)

Against ripgrep on 20,000 source files (`rg -i -F --no-ignore --hidden`,
one path list, 9 binary files skipped by both): for `unsafe`, `TODO`,
`pub fn` and `mahkeme` the same files, and the same first line in each.
Whole process, warm, five alternations: scour 30–34 ms, rg 105–109 ms
(rg's time includes `xargs`).

A 300 MB file with no newline: 4.9 MiB peak, one reader's buffer. A
UTF-16LE word in a Windows executable (`CompanyName` in the version
resource) is found with `--binary` in 1,638 of 3,000 `.exe`/`.dll`.
Every corpus, cold and warm: `docs/bench/content-phase-1.txt`.

## A spinning disk, in what order (2026-11-24)

PC2 (Windows, NTFS, Seagate ST500DM002 7,200 rpm), the `scan` example over
five disjoint random sets of 12,000 files under 1 MiB taken from the same
442,607 on one volume — each set read once, so each run is cold without
needing to flush the cache (no administrator there). One reader, as the
spinning-disk rule says. The listing that made the sets had read the
directories, so the MFT was partly warm for all five alike.

| set | order | seconds | files/s |
|---|---|---:|---:|
| 0 | path | 217.8 | 55 |
| 1 | physical (first extent's LCN, `FSCTL_GET_RETRIEVAL_POINTERS`) | 401.8 | 30 |
| 2 | physical | 363.8 | 33 |
| 3 | path | 294.0 | 41 |
| 4 | given (random) | 318.8 | 38 |

**The physical order lost** — slower than reading at random, the extra open
and query of every file before reading it costing more than any seek it
saved — and the plan's rule for a gain under 20 % applied: the code that
gathered it (`FIEMAP` on Linux, retrieval pointers on Windows) was removed,
and no column for it goes into the index. The path order stays for a
spinning disk's large sets (≥ 2,000 files or 64 MiB); a smaller set is
read as given.

```sh
SCOUR_GREP_ORDER=path|given scan.exe zzqqxx_absent < setN.txt
```

## Readers on a solid disk (2026-11-24)

This machine, NVMe btrfs, 30,690 files (source, executables, DOCX, XLSX,
EPUB; 2.4 GiB), each dropped from the page cache first (`--cold`; 722 would
not leave, being mapped by running programs), two passes:

| readers | 1 | 2 | 4 | 8 | 12 | 16 | 20 |
|---|---:|---:|---:|---:|---:|---:|---:|
| seconds, pass 1 | 11.70 | 5.94 | 3.10 | 1.72 | 1.24 | 1.01 | 0.94 |
| seconds, pass 2 | 10.35 | 5.60 | 3.10 | 1.71 | 1.26 | 1.10 | 0.92 |

It keeps gaining past eight, the old ceiling. A solid disk now gets half
the cores up to sixteen (ten here); the other half is left to the desk, and
past its first batch a reading runs at idle disk priority and nice 10.

```sh
SCOUR_GREP_READERS=N scan --cold zzqq < mixed.txt
```

## Documents' text, read in another process and kept (2026-11-24)

An isolated service (own socket, own index) over 886 documents copied from
this machine — 400 PDF, 300 DOC, 150 XLS, 36 EML; 553 MiB — release build,
`scour grep -n 2000 --lines 0 mahkeme`, twice per round, the cache cleared
before each round:

| | round 1 | round 2 |
|---|---:|---:|
| nothing kept: 842 read, 44 without text, 1 would not open | 19.06 s | 17.42 s |
| kept: the same 768 files found | 0.116 s, 0 B read | 0.158 s, 0 B read |

The second search opens no document: an inotify watch on the tree saw only
its five directories opened. The kept text was 10.4 MiB on disk for 40.7 MiB
of text with `ruzstd`; level 19 made 5.46 MiB of the same text. A changed e-mail was read again (95 B) and its old word no longer
found; four extractors killed with `kill -9` mid-search left the service up,
the search finished, and those four documents were counted as would not open.
Four idle extractors held 430 MB between two searches; they now end after
20 s idle. The service idle for 60 s afterwards: 3 ticks and 7 threads, the
installed build on the same index 2 ticks and 6 threads.

**Compression,** measured again on 124.5 MiB of text from 5,973 documents
(2,000 PDF, 2,000 DOC, 1,000 XLS, 600 PPT, 373 EML from this machine), one
thread, every block compressed and read back:

| | block | size | ratio | compress | read back |
|---|---|---:|---:|---:|---:|
| libzstd 3 | 4 MiB | 24.60 MiB | 5.06× | 492 MB/s | 1,673 MB/s |
| libzstd 9 | 4 MiB | 21.15 MiB | 5.89× | 123 MB/s | 1,936 MB/s |
| libzstd 19 | 4 MiB | 18.85 MiB | 6.60× | 5.3 MB/s | 1,949 MB/s |
| **structured-zstd 19** | 4 MiB | **18.85 MiB** | **6.60×** | 6.4 MB/s | **2,226 MB/s** |
| structured-zstd 12 | 4 MiB | 20.89 MiB | 5.96× | 81 MB/s | 2,409 MB/s |
| structured-zstd 19 | 1 MiB | 20.55 MiB | 6.06× | 7.1 MB/s | 2,108 MB/s |
| ruzstd (fastest only) | 4 MiB | 37.25 MiB | 3.34× | 134 MB/s | 485 MB/s |
| brotli q9 | 4 MiB | 20.58 MiB | 6.05× | 21 MB/s | 744 MB/s |
| brotli q11 | 4 MiB | 17.73 MiB | 7.02× | 1.1 MB/s | 704 MB/s |
| lz4 | 4 MiB | 42.53 MiB | 2.93× | 735 MB/s | 3,434 MB/s |

`structured-zstd` is zstd written in Rust: at every level the same size as
libzstd, read back faster, and the frames are one format — all 32 blocks of
the 124.5 MiB came back byte for byte through both decoders. Kept: level 19,
4 MiB blocks. Compressing at level 19 is slow, so a full block is sealed,
read from memory meanwhile, and compressed by a thread of its own; a search
never waits on it. brotli 11 is 6 % smaller and three times slower to read;
reading is what every search does. An earlier commit of this phase used
`ruzstd`, which writes only its fastest level: half the ratio, a fifth of
the read speed.

**Phrases in PDFs.** Five court phrases over the 400 PDFs, a quoted phrase
against `(?i)` with `\s+` between its words (Turkish `i` spelled out):
365 of 429 files before, **429 of 429** once a space in a phrase matches any
run of blanks, line breaks, no-break and typographic spaces. Line breaks were
the smaller part; `U+00A0` between words the larger.

```sh
scour --socket S texts --clear
scour --socket S grep -n 2000 --lines 0 mahkeme          # twice
scour --socket S grep -n 2000 --lines 0 '"asliye hukuk"' /pdf/
scour --socket S grep -e -n 2000 --lines 0 '(?i)asl(?:i|İ|ı|I)ye\s+hukuk' /pdf/
```

## The service idle, phase by phase (2026-11-24)

An isolated service over 3,170 files (source and 200 PDFs), one walk, one
`grep`, forty seconds to settle, then 120 s with nothing asked. Debug-profile
builds (optimized, with debug info) of each phase's last commit:

| build | CPU ticks in 120 s | threads | RSS |
|---|---:|---:|---:|
| before content search (`55bc3bd`) | 4 | 6 | 8.3 MiB |
| phase 1 (`a09d828`) | 3 | 6 | 12.2 MiB |
| phase 2 (`be1113e`) | 2 | 6 | 14.2 MiB |
| phase 3 (`455c913`) | 3 | 7 | 14.7 MiB |
| phase 3, as now | 4 | 8 | 7.1 MiB |

The ticks are noise. The two threads added sleep: the cache's tidying every
five minutes and reading ahead, which looks every five minutes whether it
may go on and does nothing unless chosen.

## The desk during a cold reading (2026-11-24)

This machine (NVMe, 20 threads): a probe reads 4 KiB of a file dropped from
the cache and does 5 ms of CPU work every 100 ms, while 13,600 files are read
cold again and again; normal and lowest priority alternated:

| | small read median / p95 | 5 ms of work |
|---|---:|---:|
| nothing running | 0.12 / 1.35 ms | 5.00 ms |
| reading, normal | 0.10–0.11 / 0.33–0.37 ms | 5.00 ms |
| reading, lowest priority | 0.10–0.11 / 0.30–0.35 ms | 5.00 ms |

Nothing to feel here, either way. A spinning disk is where it would show.

## A 5,400 rpm disk, every cache dropped (2026-11-24)

A 1 TB 2.5" disk (ST1000LM024) on USB, NTFS through `ntfs3`, BFQ. 10,000
files under 1 MiB taken at random from one partition, 1.1 GiB; the partition
unmounted and mounted again before each run, so nothing of it — data or MFT
— is in memory. The `scan` example, `SCAN_ROOTS` naming the mount:

| order | readers | round 1 | round 2 |
|---|---:|---:|---:|
| **where each file lies (`FIEMAP`)** | 1 | **31.7 s** (7.2 s of it asking) | **31.7 s** (7.2 s) |
| by path | 1 | 60.5 s | 60.7 s |
| as given (random) | 1 | 65.3 s | 65.2 s |
| by path | 2 | 61.5 s | 61.4 s |
| by path | 4 | 64.6 s | 80.5 s |

The physical order is **1.9× faster**, its cost included, so it is back for
a spinning disk on Linux — the plan's 20 % rule, applied the other way from
PC2's. That measurement read each order on a different set with the MFT
already warm from listing them, and Windows' retrieval pointers are another
code path; Windows reads by path until it is repeated with the cache truly
cold. One reader stays right: two gain nothing, four lose.

**The desk beside it.** A probe reading 4 KiB of other cold files on the
same disk every 100 ms, while the set is read cold over and over:

| reading | small read, median / p95 |
|---|---:|
| nothing | 11.0 / 16.1 ms |
| normal priority | 20.2 / 32.2 ms, 20.0 / 31.1 ms |
| idle I/O class, nice 10 | 19.4 / 33.3 ms, 18.5 / 32.1 ms |
| idle class, 16 MiB/s | 13.3 / 26.8 ms |
| idle class, 8 MiB/s | 12.3 / 23.1 ms |
| idle class, 4 MiB/s | 11.4 / 17.2 ms |

The idle I/O class changed nothing even under BFQ; a pace does. Reading
ahead, which nobody waits on, now goes at 4 MiB/s. Every run had a stall of
about 1.7 s, with nothing of ours reading too: the disk's or its bridge's.

```sh
udisksctl unmount -b /dev/sdX5; udisksctl mount -b /dev/sdX5
SCAN_ROOTS=/run/media/$USER/LABEL SCOUR_GREP_ORDER=physical|path|given \
  SCOUR_GREP_READERS=1 scan zzqq < set.txt
```

## Flash behind USB, which says it spins (2026-11-25)

A USB stick ("UDisk", 7.5 GB) and a class 4 card in a USB reader (7.4 GB),
both FAT, both reporting `rotational` 1 — so Scour took both for spinning
disks: one reader, files in place order. 3,000 small files (30 MiB) on
each, the volume remounted before every run.

The first 4 KiB of a file just dropped from the cache, median of 200: the
stick 0.42 ms, the card 0.97 ms; the 5,400 rpm disk above, 11 ms.

| readers, order as given | 1 | 2 | 4 | 8 |
|---|---:|---:|---:|---:|
| stick, round 1 / 2 | 2.57 / 2.53 s | **2.51 / 1.93 s** | 2.07 / 2.08 s | 2.13 / 2.60 s |
| card, round 1 / 2 | 4.61 / 4.64 s | **4.05 / 4.05 s** | 5.95 / 5.69 s | 4.21 / 4.22 s |

Path and place order made no difference on either (within 2 %). So a disk
that says it spins is asked again before a read of 64 files or more: the
first 4 KiB of up to five of them, each dropped from the cache first, and a
median under 3 ms is flash — two readers, the order given. Asked once a
disk for the life of the process. The two binaries alternated, nothing set
by hand:

| | before | after |
|---|---:|---:|
| stick | 2.42 / 2.50 s | 2.20 / 2.06 s |
| card | 4.63 / 4.71 s | 4.07 / 4.26 s |

The walk asks `surefoot` the same question and still reads these with one
thread; the same check belongs there, in a release of `surefoot`.

## Windows: a 7,200 rpm disk in place order, and the volume read raw (2026-11-25)

PC2 (Windows, NTFS, ST500DM002). Every set is 12,000 files under 1 MiB that
nothing of ours had read, picked from the service's own index (an export,
so the disk is not walked to list them); a set read a second time took
2.7 s against its first 266 s, so the first readings were cold.

**Place order, through the system** (`scan`, one reader, ABBA):

| set | order | total | asking where each file is | reading |
|---|---|---:|---:|---:|
| 0 | place | 314.7 s | 215.9 s | ~99 s |
| 1 | path | 278.4 s | — | 278 s |
| 2 | path | 266.0 s | — | 266 s |
| 3 | place | 300.8 s | 204.3 s | ~96 s |

Read in place order the files come 2.8× faster, but asking for each file's
place by opening it to read costs 17 ms a file on Windows and eats all of
it. Windows reads by path.

**The volume read raw** (`tools/ntfsraw`, elevated): each file opened for its
attributes only and its extents asked for, the volume opened as a device
and read uncached in place order in spans of up to 4 MiB (gaps under
256 KiB read through); files inside their MFT record taken from the record;
compressed, encrypted and sparse files the normal way:

| | total | asking | reading |
|---|---:|---:|---:|
| through the system, never-read set | 264.6 s | — | 264.6 s |
| **raw, never-read set** | **173.9 s** | 66.8 s | 107.1 s |
| **raw again, the MFT warm** | **75.4 s** | 0.9 s | 74.5 s |

Every one of the 10,484 files read both ways was the same, 4 KiB block by
block. The 1,516 the asking account may not open were not read raw either:
the open for attributes is refused first. The asking was the cost —
5.6 ms a file while the MFT is cold — so the MFT itself was read next,
once from start to end, for every file's extents (`ntfsraw mft`): no file
is opened at all. 12,000 more never-read entries (10,511 files; the rest
are folders the export also named, which neither way reads):

| | total | MFT read (507 MB) | files read |
|---|---:|---:|---:|
| through the system | 263.4 s | — | 263.4 s |
| **from the MFT, gaps under 256 KiB read through** | **86.8 s** | 14.7 s | 71.3 s |
| **from the MFT, gaps under 1 MiB read through** | **75.6 s** | 4.5 s | 70.3 s |

**3.0–3.5× with everything cold**, and the 10,511 files the same both ways,
block by block (`verify-mft`). Reading through wider gaps made no
difference (71.3 s against 70.3, more bytes in fewer spans); the MFT read
itself varied from 4.5 to 28 s between runs, a fixed cost whatever the
number of files asked for. Without opening files there is no access check
either: a reading of the service's must check each file's security
descriptor (`$Secure`) against the asker's token before it reads.

**Queue depth.** The same MFT reading with its spans kept `NTFSRAW_QD` deep
in flight (overlapped reads, the volume opened `FILE_FLAG_OVERLAPPED`), so
the disk orders neighbours itself; six more never-read sets, the depths
run 1, 8, 32, 32, 8, 1:

| depth | total | reading files |
|---:|---:|---:|
| 1 | 100.0 s, 75.9 s | 94.0 s, 70.7 s |
| 8 | 55.0 s, 53.9 s | 49.4 s, 48.6 s |
| **32** | **49.7 s, 49.4 s** | **44.3 s, 44.1 s** |

Depth 32 reads 1.6–2.1× faster than depth 1: the software sorts the whole
set, the disk's own queue the next thirty-two by where the platter is.
Against the system's reading of a never-read set, 263.4 s, that is **5.3×**.
10,500 files the same both ways at depth 32.

**A USB stick on Windows, FAT32** (3,000 files, 30 MiB; `fsutil volume
dismount` before every run, so nothing of it is cached): Windows calls it
solid, rightly. Scour's reader took 2.62–2.67 s with 1, 2, 4 or 8 readers;
read raw — extents through opens for attributes, FAT32's data region from
its boot sector — 2.73 s (1.17 s asking, 1.56 s reading). No gain: the
stick reads about 24 MB/s whatever asks it. All 3,000 files the same both
ways; LCN 0 is cluster 2 (taken as cluster 0, 2,999 differed).

The same stick and the same 3,000 files, formatted exFAT and then NTFS
(`format /Q`, the files copied back, dismounted before every run):

| | Scour's reader, 1 / 2 / 4 / 8 | raw, extents by opens | raw, from the MFT |
|---|---|---:|---:|
| FAT32 | 2.62–2.67 s | 2.73 s | — |
| exFAT | 3.10 / 3.13 / 3.08 / 3.09 s | 5.92 s | — |
| NTFS | 4.47 / 3.52 / 3.44 / 3.46 s | 2.38 s | **1.67 s** |

On exFAT the raw reading loses: its clusters are large, so spans merged
over gaps read 103 MiB for 30 MiB of files. On NTFS the MFT reading is the
fastest of all (3 MiB of MFT in 0.14 s, then 1.53 s of reading) — 2× Scour's
reader there, because the system's opening of NTFS files is the slow part,
not the stick. One reader is slow on NTFS only. Every file the same in all
three verifications. The stick is FAT32 again.

```sh
cd tools/ntfsraw && cargo xwin build --release --target x86_64-pc-windows-msvc
ntfsraw.exe raw set.txt      # elevated
ntfsraw.exe normal set.txt
ntfsraw.exe verify set.txt   # elevated: both ways, every block compared
ntfsraw.exe mft set.txt      # elevated: extents from the MFT, no file opened
NTFSRAW_GAP_KIB=1024 ntfsraw.exe mft set.txt
NTFSRAW_QD=32 ntfsraw.exe mft set.txt   # 32 reads in flight
```

## The service's own raw lane, PC2 (2026-11-25)

`scour-grep`'s raw lane (`src/raw`), through `examples/scan.rs`, elevated,
`SCOUR_GREP_RAW_LOG=1`. Sets of 12,000 paths from G:'s index never read
before, 7,200 rpm, raw and normal in turn:

| set | way | total | of it |
|---|---|---:|---|
| svd0 | raw | 192.4 s | 10,403 files raw: MFT 507.5 MiB in 7.0 s, their data in 44.5 s; 1,596 the normal way |
| svd1 | normal | 219.4 s | |

The 1,596 left are mostly directories — the sets were exported with them,
and a search never hands one to the reader (`scour-engine/src/grep.rs`
leaves them out) — opened the normal way on a cold disk at about 90 ms
each. The raw part itself read 10,403 files in 51.5 s; the comparison
without directories is still to be run. The MFT's own reading varied from
7 s to 39 s between runs on the same disk. Every answer the same both ways
on one set with a needle that is found: 1,048 lines, 0 different.

Two faults found on the way, both fixed: `$SDS` keeps freed descriptors as
zeros with entries after them (the parser stopped at the first), and
`%TEMP%` given in its short 8.3 form names nothing the MFT spells (a short
name is read the normal way; the test now uses the long one).

C: (solid), the component and driver stores, never read: the normal way took
7.2–11.2 s cold and 0.58 s warm for 12,000 files — opening, not reading, on
Windows even on flash. The raw lane did not run there: C:'s MFT is in so
many pieces that its own record holds an attribute list, which the lane now
follows (`mft::mft_runs`). **Not yet measured on C:**, so raw reading stays
off by default on a solid disk.

## Content: `~word`, a letter off (2026-11-26)

`target/release/examples/scan --expr PATTERN < LIST` (scour-grep), warm cache,
plain and `~` alternated A B A B A B; seconds include compiling the pattern.

| list | plain s / cpu | `~` s / cpu | wall / cpu |
|---|---|---|---|
| source, 20k files, 226 MB, `rapor` | 0.016 / 0.13 | 0.045 / 0.39 | 2.8× / 2.9× |
| source, `muhasebeciler` | 0.016 / 0.13 | 0.059 / 0.41 | 3.6× / 3.1× |
| docx, `muhasebeciler` | 0.034 / 0.26 | 0.057 / 0.33 | 1.7× / 1.3× |
| odt, `muhasebeciler` | 0.013 / 0.11 | 0.036 / 0.17 | 2.7× / 1.55× |
| mixed, 30,690 files, 2.5 GB, `muhasebeciler` | 0.60 / 5.9 | 0.79 / 7.6 | 1.3× / 1.3× |

The literal prefilter (k+1 pieces of the word, one of which every misspelling
keeps) took `~dilekçeler` on source from 0.050 s / 0.43 cpu to 0.031 / 0.23.
Compiling the pattern: 3 ms at 5 letters, 6 at 10, 21 at 11, 43 at 20, 209 at
64; most of the gap on short runs. Source code stays near 3× for long words:
their pieces are in nearly every file. Not tried: a per-window prefilter.

## Content: text read by its UTF-8 spelling alone (2026-11-28)

A word's spelling in Windows' Turkish code page sat beside its UTF-8 one in a
single expression, `(utf8)|(1254)`. That alternation stops the regex engine's
literal search, and a word of vowels was read byte by byte. Now a buffer that
is UTF-8 (checked with `simdutf8`) is read by the UTF-8 spelling alone; one
that is not, by both, as before. Words taken out of a document are UTF-8 and
are not checked.

`target/release/examples/scan --quiet WORD < LIST`: 96 files of 16 MB, 1.5 GB of
Turkish text (ŞELÇUKLU TIP.doc's text repeated), warm cache, ten readers,
before and after alternated A B A B. Each word is one the text does not hold,
so every byte is read:

| word | before MiB/s | after MiB/s |
|---|---|---|
| `ailevi` | 6,233 – 6,426 | 28,321 – 30,101 |
| `aileye` | 6,257 – 6,504 | 24,892 – 30,491 |
| `ulusal` | 6,246 – 6,327 | 21,541 – 22,304 |
| `iyilik` | 6,441 – 6,449 | 19,680 – 20,125 |
| `eline` | 34,337 – 36,259 | 22,191 – 22,600 |
| `yolcu` | 28,113 – 28,381 | 23,209 – 24,511 |
| `oyuncu` | 34,129 – 35,034 | 22,771 – 23,570 |
| `uygulama` | 28,598 – 32,987 | 25,889 – 26,191 |

Before, half these words were read at 6.3 GB/s, below a PCIe 4 disk (about 7)
and half a PCIe 5 one (about 14). After, none is below 19.7. The check costs
the words that were fast a fifth to a third: `simdutf8` reads Turkish text at
16 GB/s on one thread, and `std`'s check at 0.97, slower than the search.

One thread, 64 MB in memory, read a megabyte at a time (MB/s):

| word | before | after, a file | after, a document's words |
|---|---|---|---|
| `aile` | 863 | 5,857 | 8,880 |
| `Necmeddin` | 7,039 | 6,820 | 8,456 |
| `hastalik` | 6,708 | 3,786 | 5,274 |
| `the` | 12,728 | 7,261 | 8,322 |

The UTF-8 expression alone is not always the faster of the two: the engine
picks its literal differently, and `hastalik` lost a fifth with no check at
all.

Tried and set aside, on the same text:
- **Each letter's code-page byte in its own place**, one expression with no
  top-level alternation: `aile` 1,562 MB/s.
- **A run of letters no mark sits on** (`dd` in `Necmeddin`) looked for first,
  the expression read around it: 15–20 GB/s with two such letters, 377 MB/s
  with one (`aile`).
- **The text folded, then a plain search**: 800 MB/s. 12 % of Turkish
  characters are marked, so a 16-byte block is seldom all ASCII. It would
  need a vectorised fold written for it; this CPU has no AVX-512 compress.

## Kept texts, read in the order a search reads them (2026-11-28)

A kept text is found by its row in the table, and its block is decompressed
whole: 4 MiB in about 1.5 ms (2.4 GB/s). Only the last 8 blocks stay
uncompressed. What a search pays is decided by whether it reads the texts in
the order they were written.

A search reads files in the order of the answer's sort, by default newest
first, and the cache was filled by such a search. Read that way, it
decompresses each block once.

The setup:
- The installed cache, copied with reflinks: 23,280 texts, 71.8 MB of text,
  24 blocks; mean 3.1 KB, median 1.3 KB.
- A cache forty times that size, built from the same texts under other paths:
  931,200 texts, 2.87 GB, 680 blocks, 337 MB on disk.
- Every text got through `Store::get`, by one reader or by ten taking the next
  in turn as a lane's readers do. Release build.
- Before and after alternated A B B A. The table gives both rounds, and the
  load average was 4 to 9 during them.

| | before | after |
|---|---:|---:|
| installed copy, newest first, 1 reader | 0.107 – 0.112 s | 0.049 – 0.056 s |
| installed copy, newest first, 10 readers | 0.096 – 0.118 s (0.9 – 1.1 s CPU) | 0.052 – 0.059 s (0.11 s CPU) |
| installed copy, by path, 10 readers | 2.82 – 3.67 s (28 – 36 s CPU) | 1.23 – 1.25 s (5.7 s CPU) |
| installed copy compacted, then newest first: blocks decompressed | 12,140 – 12,165 | 22 |
| the same, 1 reader | 25.9 – 26.3 s | 0.054 – 0.055 s |
| the same, 10 readers | 6.5 – 7.0 s (64 – 69 s CPU) | 0.059 – 0.062 s |
| the compaction itself | 28.2 – 29.2 s | 10.0 – 10.2 s |
| forty times, block order, 1 reader | 4.16 – 4.87 s | 1.83 – 2.17 s |
| forty times, block order, 10 readers: blocks decompressed | 6,584 – 6,594 of 680 | 680 |
| the same, time | 2.79 – 3.47 s (26 – 31 s CPU) | 2.40 – 2.62 s (4.4 s CPU) |
| forty times, newest first, 10 readers: blocks decompressed | 13,683 – 14,126 of 808 | 881 – 882 |
| the same, time | 4.78 – 6.09 s (46 – 58 s CPU) | 2.09 – 2.25 s (4.7 s CPU) |

Three changes:
- **A compaction writes the texts again in the order they lay in.** It used
  to write them in the table's order, which is a hash map's order. A search
  after it decompressed a block for most documents it read.
- **A block is decompressed once.** A reader that wants a block another is
  reading waits for it. Before, each decompressed it for itself, nearly ten
  times over with ten readers.
- **A text is checked as UTF-8 by `simdutf8`.** One reader's 4.2 – 4.9 s in
  block order before, part by part:
  - copying each text and `std`'s check of it, 2.6 – 2.9 s, the copy 0.1 –
    0.2 s of that (the check reads this text at 1.1 GB/s);
  - decompressing, 1.0 – 1.5 s;
  - the table, 0.3 – 0.4 s.

Ten readers are still no faster than one in block order. They all reach the
next block together and wait for the one decompressing it. Reading ahead
would be the next step, and was not tried.

None of these changes helps an order the texts were not written in:
- **By path, one reader,** a text in ten costs a block: 3.9 s for the
  installed copy, before and after.
- **In random order, 30,000 texts of the large copy** decompress 29,520
  blocks: 48 – 52 s on one reader, 8.6 – 11 s on ten, before and after.

**So a lane now reads its kept documents by block, then everything else.**
- Before reading, the scanner asks where each document's text is kept
  (`Texts::place`, by path alone), and sorts those documents by block and
  offset.
- A reader takes all the documents of one block at once (`Texts::kept_each`),
  and its block is held until the last of them. Readers therefore open
  different blocks side by side, however few the hot ones.
- A document whose text no longer answers is read from its file, as before.
- The finds are the same: outcomes are kept by file, so the order of reading
  within a batch does not change them.

The real scanner over the 50,649 files the installed cache then held, each
file stat'ed, texts from a copy of the cache, nothing extracted. Batches of
2,048, as the engine hands them over; ten readers a disk. Before is the
commit above, alternated B A B A after a warm-up:

| order given | before | after |
|---|---:|---:|
| newest first | 0.210 – 0.211 s | 0.198 – 0.213 s |
| by path | 9.04 – 9.09 s (74 s CPU) | 0.367 – 0.378 s (3.0 s CPU) |
| random | 14.43 – 14.46 s (134 s CPU) | 0.667 – 0.688 s (6.5 s CPU) |

The grouping stays inside a batch, and it pays when a batch holds many texts
of each block it touches. In random order, a batch of 2,048 spread over the
cache's 2,400 or so blocks at the 1 GB cap would touch about 1,400 of them.
That would save a third, not twentyfold. This is computed, not measured.
Smaller units of compression are what would help there.

The harness was a test module pulled into `store.rs` for the runs and taken
out after. The installed cache was copied with `cp --reflink`, never opened
in place.

## Kept texts at scale: the table, a full cache, compaction (2026-11-28)

What grows with the cache is the bookkeeping, not the reading: a kept text is
never slower to read than to extract. The table was measured on synthetic
rows (paths of about 90 bytes, as an archive's are), release build:

| rows | before: in memory | after | before: written for each 4 MiB block | after |
|---|---:|---:|---:|---:|
| 100,000 | 19 MB | 16 MB | the whole table, 13.4 MB | 139 KB |
| 1,000,000 | 237 MB | 149 MB | 134 MB | 139 KB |
| 3,000,000 | 580 MB | 435 MB | 402 MB | 139 KB |

**Memory.**
- A map of strings held a path, its allocation and its row: 200 to 250 bytes
  a document.
- Now paths are kept end to end in one buffer and a row names its path by
  offset, with a `hashbrown` index of slot numbers: about 150.
- Loading at start is slower, 0.62 s for a million against 0.45. The base is
  encoded faster, 39 ms against 60.

**Writes.**
- The table was encoded under the lock searches wait on, 60 to 224 ms, and
  written whole after every block. Filling a cache of a million documents
  wrote hundreds of gigabytes.
- Now each change is a record appended to a log after the base: checksummed,
  a torn tail ends it.
- The base is written whole again only when the log has grown to half of it,
  or after a compaction or a clearing. A generation number ties the log to
  its base.
- A block's number is never used twice: a row whose block was lost in a crash
  keeps that number taken.

**Full.**
- At its size the cache gave up its oldest block for every new one. A search
  wider than the cache therefore extracted, compressed and threw away what it
  had kept a minute before, and kept nothing.
- Now the block read longest ago is given up only if nobody read it within a
  day. Otherwise the cache says it is full and keeps no new text.
- Documents refused are counted once a path, with their text. The settings
  show the count and about how much would hold them all, beside a size limit
  from 512 MB to 20 GB; the default stays 1 GB.

**Compaction and packing.** Level by level, on 33.6 MB of this machine's kept
text (eight full blocks), structured-zstd:

| level | ratio | compress |
|---|---:|---:|
| 3 | 6.68 | 537 MB/s |
| 9 | 8.72 | 146 MB/s |
| 12 | 9.10 | 101 MB/s |
| 15 | 9.50 | 9.7 MB/s |
| 19 | 10.10 | 6.8 MB/s |

- Compaction wrote every live text again at level 19: about a quarter of an
  hour of a core a gigabyte.
- Now a block nine tenths of whose text is still pointed at is left as it
  is, and the rest are written at level 12.
- New blocks stay at 19, unless more than two wait for the packer. Then the
  next is written at 12, so full blocks do not pile up in memory while the
  readers outrun level 19.

**The installed cache, copied:** a table of version 3, 50,649 texts, 31 MiB.
- It opened in 36 ms, and every text read back.
- It was written as version 4, and reopened with the same counts and the
  same 50,649 texts.

## The window's preview scrolling, Slint 1.16.1 → 1.19.1 (2026-11-28)

The window in a headless GNOME of its own (1600×900), against a test service,
release builds. The wheel is turned over the preview by the window itself
(`SCOUR_GUI_WHEEL=60`, `SCOUR_GUI_WHEEL_AT=-190,450`, a notch every 50 ms, 100
notches), and `SCOUR_GUI_STALLS` reports, a second at a time, how long a 2 ms
timer was held up. "Busy" is the sum of the overruns, per second.

| preview | Slint | busy while scrolling | longest stall |
|---|---|---|---|
| Word document, 1,100 rows (`ListView`) | 1.16.1 | 37–56 ms/s | 5.5 ms |
| same | 1.19.1 | 24–34 ms/s | 4.9 ms |
| same, every row laid out (`Flickable`) | 1.19.1 | 84–141 ms/s | 10.4 ms |
| Word document, 4,000 blocks, every row laid out | 1.19.1 | one stall of 2,189 ms, then no frame for 20 s | — |
| Word document, 4,000 blocks (`ListView`) | 1.19.1 | 28–41 ms/s | 4.3 ms |
| PDF, 352 pages | 1.19.1 | 12–21 ms/s | 3.3 ms |

What a person saw as stutter in a document was not time: no frame over 6 ms
either way. A `ListView` reckons its height as the average of the rows in
sight times their count, and its place from that, every frame, so with rows of
uneven height the scrollbar and the place under the wheel moved by
themselves. Laying every row out fixes the numbers and costs the window: Slint
visits every element each frame, on screen or not. The document stays a
`ListView`, and its rail is drawn in the one measure that holds still — the
share of rows above the view. The content search's list, two heights known in
advance, is placed exactly by the window instead (`grep_list.rs`).

Every screen photographed with both releases and compared pixel by pixel
(1–5 % of pixels differ): text lines are 1–2 px tighter and glyph advances a
hair wider in 1.19 (a segmented control 245 → 247 px); nothing moved.

## Drawing more formats, a file at a time (2026-11-28)

`cargo run --release -p scour-doctext --example flow_cost -- FILE...` reads
each file as the extractor does and draws it; one run each, this machine.
A drawing stops at its bounds (`scour_flow::MOST_*`), so a large file costs
what reaching the bounds costs.

| what | file | bytes | ms | came out as |
|---|---|---|---|---|
| Word, macro-enabled | poi `45690.docm` | 49,596 | 3.12 | 3 headings, 11 paragraphs, 2 pictures, 2 tables |
| Word template | poi `60316.dotx` | 59,583 | 1.00 | 4 headings, 21 paragraphs |
| Excel, macro-enabled | poi `57181.xlsm` | 334,727 | 39.69 | 6 sheets |
| PowerPoint template | poi `bug59273.potx` | 617,839 | 1.46 | 1 slide |
| OpenDocument template | LibreOffice `Candy.otp` | 1,652,392 | 2.13 | 13 slides |
| flat OpenDocument | LibreOffice `CREDITS.fodt` | 2,054,738 | 17.24 | 5 headings, 9 paragraphs, a picture, 5 tables |
| Markdown | this repository's `MEASUREMENTS.md` | 76,060 | 0.52 | 43 headings, 237 paragraphs, 54 tables |
| Markdown | a 2.3 MB notices file | 2,329,787 | 2.54 | cut at the bounds |
| CSV | csv crate `game.csv` | 2,600,000 | 2.25 | a sheet, cut at 2,000 rows |
| CSV, 1,602 columns | nvidia `dlsnetparams.csv` | 2,673,928 | 8.39 | a sheet (74.47 before fields past 80 columns were passed over unread) |
| HTML | rustdoc `struct.Vec.html` | 952,692 | 6.50 | 901 headings, 1,726 paragraphs |
| HTML | Chromium's licences | 20,115,518 | 9.01 | cut (46.75 before end tags were found in place) |
| HTML | Rust's `COPYRIGHT.html` | 15,380,909 | 4.80 | cut (109.85 before) |
| e-book | a 2 MB EPUB | 2,067,824 | 4.53 | 31 chapters, cut at the bounds |
| e-book | a 31 MB EPUB | 31,662,481 | 1.42 | 6 chapters, 2 pictures |
| message | mail-parser's `legacy/015.eml` | 11,502 | 0.10 | its two inline pictures drawn |

An archive's listing is read in the service, not drawn from its bytes: a
zip's central directory from its end whatever its size, a tar's headers from
its start.

## Reproducing

```sh
scripts/bench [label]                         # queries, service CPU/RSS, first frame → /tmp/scour-bench-<label>.txt
cargo run --release -p scour-index-native --example searchcost
cargo run --release -p scour-index-native --example build_cost -- segment 1000000
python3 scripts/reliability-probe.py 2000     # 2,000 files: create, mutate, replace; checks convergence
```
