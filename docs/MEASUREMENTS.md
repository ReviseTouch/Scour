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

## Reproducing

```sh
scripts/bench [label]                         # queries, service CPU/RSS, first frame → /tmp/scour-bench-<label>.txt
cargo run --release -p scour-index-native --example searchcost
cargo run --release -p scour-index-native --example build_cost -- segment 1000000
python3 scripts/reliability-probe.py 2000     # 2,000 files: create, mutate, replace; checks convergence
```
