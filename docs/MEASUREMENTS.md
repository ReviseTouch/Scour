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

## Where the design numbers come from

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
