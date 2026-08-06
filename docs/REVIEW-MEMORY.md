# Where the idle service's memory and CPU go

Measured 2026-08-06 on the same Linux 6.18 desktop and the same native index.
All daemon and diagnostic binaries below are release builds. The working copy
of the index held **2,091,824 entries in 55 segments**; the production index was
not stopped or changed.

## Answered

### There is no live half-gigabyte index structure

`memory.current`, `memory.stat:anon` and `memory.swap.current` say which pages
the cgroup is still charged for. They do not say that malloc still has an
object using those pages. That distinction is the missing part of the original
breakdown.

The measurement build reports glibc's live arena bytes (`mallinfo2.uordblks`),
malloc's live large mappings (`mallinfo2.hblkhd`) and
`/proc/self/smaps_rollup` together. Immediately after one `/mnt/depo`
reconciliation:

| | MiB |
|---|---:|
| anonymous pages | **175.1** |
| live malloc allocations | **10.1** |
| free pages still in glibc arenas | **165.0** |

The last line is subtraction, not attribution by guess. Those pages can move
to zram and remain charged as swap while holding no Rust value. This is why a
failed `malloc_trim(0)` experiment did not prove the memory live: fragmented
and per-thread arenas need not be releasable by a later trim from another
thread.

The named live allocations are these:

| owner | measured storage | necessary? |
|---|---:|---|
| open `NativeIndex`, 2,091,824 entries | **10.4 MiB anonymous** | yes; the segment data and `IdMap` are file-backed mmaps |
| `notify::INotifyWatcher::EventLoop::{watches, paths}` for `/home/hasan` | **74.4 MiB live malloc**, 74.9 MiB anonymous | yes with this backend; it stores all 280,706 directory paths twice |
| each in-flight `Vec<Entry>`, 100,000 rows | **25.6–33.1 MiB** | one is necessary to build; four are a throughput choice |
| each in-flight `SegmentBytes` | **6.7–13.5 MiB** | one is necessary while its segment is written |
| each `Flight::paths` | **114,688 slots**, at least 0.88 MiB for the keys | necessary while builds overlap removals |

The four concurrent builders therefore explain roughly 130 MiB of entries,
another 30–54 MiB of segment buffers, and at least 3.5 MiB of flight keys at
the top of a scan. They are the largest allocation sites. They are all gone
after `settle`; the allocator counter says so directly.

The anonymous floor for opening a two-million-entry index is therefore about
**10–12 MiB**, not proportional to the index size. On this machine live updates
add **74.4 MiB** in userspace for 280,706 watched directories; the supplied
cgroup sample has another 30 MiB of kernel slab. That watch cost follows the
directory count and path lengths, not the two million indexed rows.

The watch number was isolated without opening or scanning an index:

```bash
cargo build --release -p scour-source-fs --example watch_memory
find /home/hasan -xdev -type d 2>/dev/null \
  | awk '{ n += 1; bytes += length($0) } END { print n, bytes, bytes / n }'
printf '\n' | target/release/examples/watch_memory /home/hasan
```

It counted 280,706 directories and 26,361,177 path bytes, 93.9 bytes per path.
The watcher changed live malloc from zero to 51.9 MiB of arena allocations plus
22.5 MiB of malloc mappings. `notify` 8.2's Linux `EventLoop` names the reason
in its fields: a `HashMap<PathBuf, ...>` and a reverse
`HashMap<WatchDescriptor, PathBuf>`.

The scan allocation sites came from the release-only measurement feature,
against a reflinked copy and a config with `scan.on_start = false`:

```bash
cargo build --release -p scourd --features memory-trace
SCOUR_BUILD_TRACE=1 \
SCOUR_MEMORY_NO_PULSES=1 \
SCOUR_MEMORY_RESCAN=/mnt/depo \
target/release/scourd --config <copied-index-config>
```

### The idle CPU was compaction, not the 100 ms tick

The 100 ms worker loop with no sources moving used **2 CPU ticks in 60 s**:
20 ms, or **0.033% of one core**.

```bash
SCOUR_MEMORY_NO_PULSES=1 \
SCOUR_MEMORY_IDLE_SECS=60 \
target/release/scourd --config <copied-index-config>
```

A commit does have a large fixed cost. Replacing existing rows in the copied
2.09 M-row, 55-segment index, five fresh reflinked copies per batch size:

| rows in the commit | median wall | median CPU |
|---:|---:|---:|
| 1 | 177.1 ms | 15.5 ms |
| 4 | 182.4 ms | 14.7 ms |
| 16 | 152.5 ms | 15.7 ms |
| 64 | 161.1 ms | 16.8 ms |

The row count barely matters. With `SCOUR_COMMIT_TRACE=1`, 10–12 ms of the
roughly 16 ms CPU was `flush_prepare`: probing the identity tables across 55
segments and preparing the touched alive bitmap. Durable alive/segment/manifest
writes took 120–145 ms of wall time but only another 4–5 ms of CPU.

That is not enough to explain 0.87% for four rows. The work after the commit is.
The scan generation contained two body segments, **1,250,797 rows and 100,000
rows**. Later watcher commits were stamped with that same generation. Four
one-row segments made the group eligible; compaction spared the largest member
and rewrote the 100,000-row member with the four new rows.

Measured as the exact sequence the worker performs:

| four-row minute | commits, CPU | compact, CPU | total | one core |
|---|---:|---:|---:|---:|
| before | 36.8 ms | **477.0 ms** | **513.8 ms** | **0.857%** |
| after | 23.2 ms | **4.2 ms** | **27.4 ms** | **0.046%** |

The before result is the observed 0.87% to measurement noise. The compaction
also fell from 558.2 ms wall and a 27.0 MiB anonymous peak to 63.9 ms wall and
a 0.5 MiB peak. The fixed run folded only the four trickle segments; it did not
pull a 100,000-row scan segment into a one-minute housekeeping pass.

```bash
cargo build --release -p scour-index-native --examples
target/release/examples/commit_cost <copied-index> 1 <existing-path>...
target/release/examples/compact_cost <copied-index>
```

## Fixed

### Builder arenas are trimmed where their objects die

Successful builders now drop both `Vec<Entry>` and `SegmentBytes`, then call
the existing glibc-only `trim_allocator` from that builder thread before it
exits. The synchronous overflow path does the same. Failure still returns the
entries intact.

Three interleaved release scans, with the same binary and the same copied
index; `SCOUR_NO_BUILDER_TRIM=1` is available only under `memory-trace`:

| | no builder trim | builder trim |
|---|---:|---:|
| settled anonymous, median | **144.1 MiB** | **95.9 MiB** |
| range | 130.3–208.8 MiB | 93.4–104.3 MiB |
| scan time, median | 6.14 s | **5.59 s** |
| peak anonymous, median | 169.0 MiB | 167.8 MiB |

The fix returns a median **48.1 MiB** after the scan and does not claim to
reduce the working peak. It did not make the scan slower in these runs.

### A closed scan now closes its compaction cohort

When the first vouched root closes an open scan generation, the index advances
its current generation once. Rows written by the scan keep their stamp;
subsequent watcher commits receive the next stamp. Reconciliation semantics do
not change, but compaction no longer groups a bulk scan body with the trickle
that follows it.

The regression test also covers a multi-root source: later sweeps for the same
scan do not advance it again. The before/after CPU table above is the direct
measurement of this change.

This costs at most one extra two-segment trickle cohort between scans. The
fixed measurement ended at 40 segments instead of 38; it bought back 472.8 ms
of CPU per four-row minute without changing the file format or a public
surface.

## Found, not fixed

### The remaining allocator slack needs an allocator or worker-pool decision

Even after builder-local trims, median post-scan anonymous memory was 95.9 MiB
against about 10 MiB of live malloc storage. glibc still owns free, fragmented
pages from the parallel walker and builders.

The smallest further changes are global: cap glibc arenas with `mallopt`, ship
a different allocator, or keep a fixed builder pool whose workers can trim
their own arenas between jobs. Each changes allocation behaviour or scan
throughput for the whole process. None was made on this measurement alone.

### The watcher stores every directory path twice

The 74.4 MiB `notify` map is live and necessary to that implementation, not to
inotify itself. A Linux-specific watcher could store one shared path arena and
maps of offsets, or use a different reverse-lookup scheme. That means owning a
platform backend instead of using `notify`; it is a design and maintenance
decision, so it is recorded rather than smuggled into this fix.

### The pulse can request the expensive scan, but it did not move in this run

`/mnt/depo`'s write-sector pulse was sampled every two seconds for twenty
seconds and stayed at **2,983,600** throughout. No pulse-driven scan occurred
during this review. The engine sequence is nevertheless direct and covered by
`a_movement_inside_the_floor_is_remembered_not_dropped`:

1. a changed pulse on an unwatched source sets `pending`;
2. after the 15-second per-source floor, `Nudge::Reconcile` runs a whole-source
   scan;
3. the scan creates the builder allocations measured above.

So the earlier evening's moving pulse can explain why a scan began, but not a
live 500 MiB structure afterwards. The smallest way to avoid a full NTFS walk
is a source that can reconcile from the MFT or a directory-timestamp sweep;
`docs/ENUMERATION.md` measures both. That is a new source capability, not a
local memory fix.

### The watched commit clock counts events poorly

When nobody waits for updates, the engine already commits at 64 events or 15
seconds, whichever comes first. While a window waits, the one-second
`commit_watched` bound dominates and a trickle can still pay one commit per
changed file. `pending` also counts watcher events, not distinct staged paths;
several notifications for four resulting rows can reach the threshold early.

The present measurement does not justify changing that policy: after the
compaction fix, the measured four-row work is 0.046% of a core and the idle
loop is 0.033%. If a second measurement still finds commits material, the
smallest policy change is a distinct-staged-row threshold plus a longer watched
deadline. Its risk is explicit: a newly saved file would take longer than the
current one-second bound to appear in an open window. That decision was not
made here.

## Ruled out

* **The open native index.** 2,091,824 entries used 10.4 MiB anonymous; segment
  files and `IdMap` are mapped and file-backed.
* **The three searches.** They precede the delayed rise and cannot account for
  the allocator's post-scan free pages.
* **`Inner::staged`.** It is empty after `settle`; live malloc for the whole
  process was about 10 MiB at that point.
* **A retained `Flight` or `SegmentBytes`.** The feature measured each one, and
  `settle` cannot finish until every flight has landed and been collected.
* **A Rust heap leak of 500 MiB.** Anonymous plus swap is not live allocation;
  `mallinfo2` measured the live side directly.
* **The 100 ms worker tick.** It cost 0.033% of a core over sixty seconds.
* **Four raw commits as the whole 0.87%.** Four commits cost tens of CPU
  milliseconds; the repeated 100,000-row compaction supplied the missing
  477 ms.
* **The rebuild path.** The supplied rebuild measurement already showed that
  it returns its allocation; this review did not repeat it.
