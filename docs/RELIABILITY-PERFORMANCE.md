# Reliability and performance audit — 2026-09-05

This audit follows a file from discovery through reconciliation, query
preparation and browser rendering. It includes implemented fixes, regression
tests and reproducible measurements. The existing uncommitted frontend and
watcher work was preserved. Initial integration probes used private directories,
sockets and daemon processes. The user subsequently requested installation and
launch of the release builds; the native follow-up also measures the live index.

## Findings and changes

| Area | Failure found | Change |
|---|---|---|
| Engine lifetime | The preparing thread held `Shared`, which held the sender on which that same thread waited. Dropping an idle engine retained the index and source indefinitely. | Explicit stop channel and owned join handle; shutdown joins both workers. A public test checks that weak references to the index and source expire. |
| Silent changes | A source with no watch and no pulse could remain stale forever. A flat pulse or activity in another subtree could also conceal missed events. | Full fallback and safety reconciliation, independent of ongoing subtree activity. Incomplete watch coverage is not treated as a complete watch. |
| Recovery cost | Simply adding frequent full scans would create sustained I/O and CPU pressure on large trees. | Periodic recovery and failed full-pass retries rest for at least twenty times the previous full scan's wall duration, with the existing/configured floors retained. |
| Failed writes and scans | A failed sweep or partially consumed change iterator could leave stale entries with no timely retry. | Failed sweeps abandon their generation; incomplete scans and failed change application schedule full-source retries. Failed maintenance preserves dirty state. |
| Cache validity | Prepared `dm:1h` results survived the passage of time as long as the index revision stayed unchanged. | Store and compare the parsed query, including its current time cutoff, before using a prepared page. |
| Refresh signaling | Explicit flush could publish rows without invalidating prepared results or waking clients. Shutdown could leave an unchanged `await` blocked. | Publish a revision after a successful flush and wake waiters when stopping. |
| Browser data cache | A stationary list fetched all reachable results; scrolling retained every fetched window and path. | Two-window read-ahead distance and a 32-window LRU, with rows and path reference counts evicted together. |
| Expensive refreshes | A time throttle started more work before the preceding request finished and its cost became known. | One running operation and one coalesced follow-up per periodic refresh lane. |
| Hidden pages | Previously scheduled refreshes could still start window requests after the page became hidden. | Recheck visibility when executing refreshes and window fills; remember missed revisions for return. |
| Timing | Engine search timing excluded folder-size enrichment. A frontend could classify an expensive page as cheap. | `took_us` now measures the whole engine search operation; transport and rendering are still separate. |

The native search algorithms were not replaced. Existing comparison tests
against `scour-mock::brute_force` remain the correctness guard. Turkish folding,
query aliases and locale-sensitive ordering remain in place.

## What file tracking can promise

The implemented contract is eventual convergence to the observable filesystem
state, provided the source becomes readable, the index becomes writable and
the worker gets time to complete a scan and commit. It is not an event ledger.

1. A complete event feed supplies changes promptly. The worker batches them,
   and waiting clients receive a new revision when the index changes.
2. Filesystem pulses can trigger recovery on unwatched sources. A pulse is a
   hint about filesystem activity, not evidence about an individual directory.
3. Sources with neither complete coverage nor a usable pulse receive fallback
   scans. All sources receive periodic full safety scans, including those whose
   pulses remain flat or whose watchers continue reporting other subtrees.
4. A scan only sweeps roots it could inspect, preserving explicitly blind
   subtrees. Failed/incomplete reconciliation retries rather than treating
   missing evidence as deletion.
5. Startup scanning remains enabled by default because no source here offers
   journal replay across process downtime.

This design follows the documented limitations of the underlying APIs:

- Linux inotify can overflow, does not recursively watch a tree by itself,
  and has races around paired rename events. Its manual explicitly recommends
  consistency checking and rebuilding caches after inconsistencies.
  [inotify(7)](https://man7.org/linux/man-pages/man7/inotify.7.html)
- fanotify also has queue-overflow events; using it does not make the event
  stream durable or infallible.
  [fanotify(7)](https://man7.org/linux/man-pages/man7/fanotify.7.html)
- The watcher library documents missing events on network filesystems and
  limitations on large watched trees.
  [notify documentation](https://docs.rs/notify/8.2.0/notify/)
- Block I/O counters describe device activity. Using them to infer changes in
  one indexed subtree is an application heuristic, which is why the full
  safety pass must not depend on a changing counter.
  [Linux I/O statistics](https://docs.kernel.org/admin-guide/iostats.html)

Consequences that must remain explicit:

- A file created and deleted between all observations cannot be reconstructed.
- An unreadable directory or disconnected root is retained conservatively;
  its absence is not proof that every indexed file beneath it was deleted.
- Files changing while a scan runs are not a filesystem snapshot. Queued events
  and a later reconciliation are still needed.
- An unchanged size/time identity can conceal content-only edits. This source
  indexes names and metadata; it does not advertise full file-content search.
  Memory-mapped writes also have event limitations documented by inotify.
- Relative-time cache validity is now checked on each search. Merely leaving a
  time-filtered UI open does not yet schedule refresh exactly when a row ages
  across the cutoff, without any index event or subsequent request.

## Configuration and resource tradeoffs

The defaults below are wired into the daemon and are supplied when an older
configuration omits the fields:

```toml
[service]
poll_interval_secs = 60
reconcile_interval_secs = 1800
```

`poll_interval_secs` applies when a source has neither complete watch coverage
nor a pulse. An unwatched source with a moving pulse keeps the pulse recovery
path, whose minimum rest is 15 seconds. A quiet source with a pulse still has
the full safety pass governed by `reconcile_interval_secs`.

For each completed full pass, normal periodic scheduling uses
`max(configured floor, scan wall duration * 20)` as its rest period. The blind
watch recovery path also observes the scan-cost floor. This bounds the duty of
repeated periodic scans of that source; it is **not** a total-process CPU cap.
Multiple sources, parallel scan workers, searches and explicit scans consume
additional resources. Failed full passes now obey the same cost floor.

Both configuration values are clamped to at least one second when wired.
Deadlines are checked every two seconds. Queue delay, the scan itself, commit
batching and failures add latency. A 30-minute setting is therefore not a
guarantee that every missing file appears within exactly 30 minutes.

Failure retries retain the existing widening delays: 10 seconds, 30 seconds,
2 minutes, 10 minutes and then one hour, increased to at least twenty times
the full pass duration. The wait starts after the failed scan finishes. A due
periodic check preserves the previous scan cost and does not override a pending retry. A long-disconnected source may take that next retry
to become current after reconnection.

## Native Slint follow-up

The user reports initial scroll stalls and growing RAM. The native window uses
Slint's `ListView` for both the table and the tile rows, with a custom paged
model. Slint instantiates visible row components; the application's row data,
images, prepared search results and kernel caches have separate lifetimes.
[Slint ListView reference](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/views/listview/)

The follow-up changes five concrete behaviors:

- An empty model uses `reset` when first populated, including tile lines.
  Slint 1.16.1's repeater inserts one placeholder per added row for
  `row_added(0, count)` on an empty repeater. Reset lets visible layout supply
  the instances. Later growth/shrink still uses incremental notifications to
  preserve the viewport. A regression attaches an actual Slint model peer and
  checks a five-million-row initial count followed by growth and shrinkage.
  This identifies a problematic notification path, not proof that every
  observed startup peak followed it; an initial capped page may arrive first.
- Preparation is requested only for nonempty pages wholly inside the first
  20,000 results, after a measured page cost of at least 20 ms. Cheap paging
  and pages beyond that window do not warm a larger result nobody can use.
  Concurrent expensive requests cannot each start a preparation. Expensive
  ordering tests and relative-time cache expiry continue exercising the cache.
- Native live refresh rests after completion for `max(500 ms, last page round
  trip * 10)`. The service-reported time is used if larger. A slow response
  cannot consume its rest while in flight and immediately trigger another.
  Missing scroll pages and explicit new queries bypass this optional refresh
  clock; expensive speculation remains disabled.
- A missing row reported by the previous layout cannot redirect a request
  away from the current viewport during a scrollbar jump.
- Incomplete full scans use their measured cost when scheduling retries,
  including the first retry. A scan taking longer than its fixed retry delay
  no longer schedules a retry deadline that has already passed.

The native row cache already had a 32-page / 6,400-row LRU before this follow-up;
it was not replaced with another renderer. This is not a decoded-image byte
budget. Grid thumbnail lookup/decode still runs in bounded batches on the UI
thread, and retained images need a separate byte budget and asynchronous loading
before the grid can be described as fully isolated from disk/decode stalls.

On this desktop, the service cgroup at one post-start sample accounted for about
4.7 GiB, including about 3.3 GiB of reclaimable kernel slab and 1.0 GiB of file
cache. Process anonymous RSS at that point was about 97 MiB, with total RSS
about 524 MiB. These figures overlap: do not add RSS to cgroup total or add
file-mapped bytes to file bytes. A 10-second idle interval used 0.30% of one core.
Neither an idle sample nor a smaller GUI heap establishes the startup peak.

Kernel slab here can include filesystem dentries and inodes left by walking
millions of files. It is reclaimable, but it still creates real memory pressure.
A `memory.high` budget would trade cache retention for reclaim work and possibly
higher latency; no unmeasured system-wide cache purge or arbitrary hard limit
was applied. Startup reconciliation remains necessary to detect changes while
the service was stopped. The multi-million-file startup memory peak remains a
separate open optimization, not a solved problem.
[Linux cgroup memory accounting and control](https://docs.kernel.org/admin-guide/cgroup-v2.html)

## Build resource follow-up

Directory paths in `DirWriter` now have one shared string allocation, and its
lookup map is dropped before encoding. Insertion order and provisional IDs are
preserved. Segment trigrams reuse the folded filename already written to the
name arena; public raw-input trigram callers still get defensive folding.
The directory remap is released immediately after use.

Five alternating release runs per version measured a 300,000-directory build
at 160,376 → 107,728 KiB peak RSS and 273.913 → 179.916 ms CPU time. A streamed
million-row segment measured 175,244 → 164,696 KiB peak RSS and 1320.615 →
1157.436 ms CPU time. Output sizes and fingerprints matched. The smaller
10,000-directory case showed overlapping time ranges and no established
speedup. Commands, full medians and limits are in `docs/MEASUREMENTS.md` under
“Resource costs without reducing coverage”.

This removes duplicate storage and duplicate work. It does not reduce indexed
coverage, cache capacity, watcher coverage or commit frequency. It also does
not establish a reduction in instantaneous CPU-percent peaks or the service's
whole-startup cgroup memory peak: kernel filesystem caches dominate the latter
in the observed desktop samples. Those claims require separate measurements.

## Browser behavior and limits

DOM virtualization and data caching are separate. Drawing only visible rows
did not prevent the JavaScript maps from retaining all 20,000 reachable rows.
The LRU now targets 32 windows of 200 rows, keeping visible and pending windows
safe. If a pathological viewport itself spans more than 32 windows, protected
windows take precedence; this is a row-count target, not a strict heap-byte cap.
Images, selections, previews, path lengths and the browser's own caches have
separate costs.

The page fetches the viewport first, and at most two windows beyond either
edge after the query settles and the measured page cost permits speculation.
Rows evicted from the data cache can be fetched again on return. Reference
counts for paths are removed with the rows, and shrinking a sparse result
clears high-index entries even when the map contains few rows.

Old query responses cannot replace a newer query's rows. Outdated scroll
requests are aborted, and old completions cannot clear a newer request at the
same offset. These behaviors are exercised using delayed transport responses.
Aborting a browser request does not prove that server-side search work already
started has been cancelled.

Periodic row and count refreshes coalesce while in flight. Visibility is checked
when callbacks run, not only when an event was received. The existing visibility
listener resumes updates on return. This follows the browser visibility API;
it does not assume that background timer throttling cancels application work.
[Page Visibility API](https://developer.mozilla.org/en-US/docs/Web/API/Page_Visibility_API)

The 20,000-row browse reach remains. CLI/API searches and streaming export are
separate from that browser limit. There is still only one prepared ordering in
the engine, shared across clients, so different concurrent queries can displace
each other's prepared results.

## Validation

Run from the repository root:

```bash
bash scripts/check --quick
node --test apps/scour-web/tests/page.test.cjs
cargo build --release -p scourd -p scour-web
python3 scripts/reliability-probe.py 2000
```

The complete quick check passed: formatting, strict workspace Clippy, parsing
the embedded JavaScript, 751 Rust tests/doc tests and the browser scheduling
test runner. The runner contains six Node tests. Eight Rust tests were ignored:
four manual performance probes and four tests requiring a privileged fanotify
descriptor. They are not counted as verified event-feed coverage. The cargo
wrapper explicitly reports a skip if Node is unavailable; CI should install it.

New regression coverage includes engine object release, wake-on-shutdown,
flush revision signaling, silent source changes, failed sweep retry, relative
date cache expiration, safety scheduling despite activity elsewhere, scan-cost
backoff, configuration compatibility, asynchronous refresh coalescing, bounded
prefetch/LRU, sparse shrinking, hidden pages and delayed query/scroll replies.
The engine release and relative-date regressions were reproduced as failures
against the earlier implementation before being verified with the fixes.

The private filesystem probe starts with 2,000 files in 20 directories and
compares every returned path, type and file size against the filesystem. It
creates files, modifies sizes, renames a populated directory, deletes a subtree,
performs an atomic replacement and adds a hard link. Watching is disabled;
there is no explicit rescan after the mutations. This verifies reconciliation,
not privileged watcher delivery. Probe recovery intervals are deliberately
shorter than production defaults. Exact timings and methods are in
[MEASUREMENTS.md](MEASUREMENTS.md#september-2026-reliability-and-browser-cache-audit).

A real in-app browser loaded a separate 2,001-entry fixture. Scrolling to the
last file (`report-1999.txt`), replacing a query rapidly and switching to the
icon grid produced the expected rows. Browser console inspection returned no
warnings or errors. This is an interaction smoke test, not a Web Vitals trace.
Chrome DevTools MCP was unavailable, so LCP, INP, CLS, long tasks and JS heap
allocation were not measured. Deterministic cache row counts are not heap MB.

Changed engine/config code was also compile-checked for Windows, macOS and
Android. A broader Windows check failed in existing `scour-trash` Unix imports
and APIs. This audit therefore does not certify the entire workspace for
Windows or runtime behavior on any non-Linux platform.

## Remaining work, in priority order

1. **Filtered name ordering.** The million-file fixture's `ext:rs` query sorted
   by name visited all 1,076,156 entries and took 203.21 ms for 40 hits. Empty
   name ordering was cheap. Investigate a filtered walk of the stored name
   order, with brute-force checks for collation, ties, deletion and deep pages.
2. **Sustained churn and failure soak.** Exercise actual privileged fanotify,
   queue overflow, watch exhaustion, repeated mount loss and full disks while
   searching. The deterministic and short filesystem tests do not establish
   behavior over days or all filesystems.
3. **Backpressure across scans and exports.** The event queue is bounded, but
   engine jobs are unbounded and only full-source scan requests are coalesced.
   Slow consumers of an index scan/export can hold read-side work open and
   delay writes. Measure queue age and writer latency before changing locking
   or the contract for a consistent export.
4. **Freshness observability.** Expose per-source last successful reconciliation,
   coverage gaps, retry deadline and queue age as typed status fields. Existing
   aggregate counters and logs do not communicate every degraded state to a
   person looking at an apparently quiet list. Update protocol/AGENTS and
   frontend translations together when adding those fields.
5. **Time-only invalidation.** Schedule the next relevant relative-date boundary
   without polling every visible query continuously. The server needs to
   identify the deadline; frontends must not parse the query themselves.
6. **Real memory/load profiles.** Measure warm and cold starts, p50/p95/p99
   query latency, foreground contention, browser heap after repeated scrolls,
   and process anonymous/file-backed RSS separately from cgroup page cache.
   Existing historical desktop measurements are useful context, not a before
   measurement of this exact patch.

These remaining items require further targeted implementation and measurement.
The fixes above strengthen recovery and bound avoidable work; they do not
justify calling the complete system perfect or lossless.

## Opening memory and administrative prompts

The folder totals cache now uses 512 reusable parent decode slots, exact
reservation for directory rows, in-place updates after deaths and early removal
of caches for retired segments. A five-pair test reduced the operation's
anonymous peak from 57.1 to 19.5 MiB and retained memory from 48.9 to 19.6 MiB;
CPU also decreased. The actual Slint folder-query opening reduced combined
GUI/service RSS peak from 312788 to 282768 KiB. See the final section of
`MEASUREMENTS.md` for commands, raw artifacts and the empty-query control that
did not trigger this cache. File-backed index pages are not forcibly evicted.

The optional host-specific `packaging/install-service.sh` installs a root-owned
watch helper and unit before granting `hasan` start/stop/restart on that unit via
polkit. Installing the rule still requires one administrator authentication;
subsequent lifecycle operations do not. No general passwordless sudo or binary
capability is installed. The privileged mount uses a fresh private directory in
`/run` instead of a predictable path in `/tmp`. Unit/rule ownership and an actual
noninteractive restart must be verified on the host after installation.
