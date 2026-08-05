# Audit — 2026-08-04

An end-to-end read of what enters the index, what leaves it, and what never
arrives. Everything below was reproduced against the live service on this
machine (2,138,153 entries, 183 MB, two sources).

The order is by what it costs a user, not by where it sits in the code.

| | | |
|---|---|---|
| §1 | a directory in a split parent is never watched | fixed |
| §2 | a file written into a brand-new directory is lost | fixed |
| §3 | the watcher follows symlinks the walk does not | fixed — with a correction, below |
| §4 | `Rebuild` does not terminate under churn | fixed, and found a second race |
| §5 | `.alive` files outlive their segments | fixed |
| §6 | fragmentation costs nothing measurable | acted on: no more automatic rebuild |
| §7 | idle housekeeping never runs | fixed |
| §8 | a pending `rm -rf` made commits quadratic | measured, then fixed — 29× |
| §9 | smaller things | fixed, except two left alone on purpose |
| §11 | an atomic save leaves a ghost row at the same path | fixed — a row's identity is now its path |

Verified afterwards on the live index rather than only in tests. Five thousand
files written into two hundred freshly created directories, as fast as a shell
loop can do it: **5,000 of 5,000** found, at their real paths. Deleting the
same tree: gone from the index within forty seconds, and five commits over
20 ms, the worst 166. A restart took the index directory from 1,454 orphan
segments to none.

The first attempt at §1 got 3,740 of those 5,000, and the 1,260 it lost were
not scattered — packages 32 to 82, one unbroken run. The walk was being run
before the watch was extended, so the window between them was covered by
neither: the same race, moved rather than removed. **Watch first, then walk.**

---

## 1. A directory created in a split parent is never watched — permanently

`/home/hasan` cannot be watched recursively:

```bash
cargo run --release -q --example canwatch -p scour-source-fs -- /home/hasan
# /home/hasan: REFUSED after 400.3ms — Permission denied (os error 13)
#   about ["/home/hasan/.local/share/waydroid/data/vendor"]
```

So `watch::cover` does what it was written to do: a **non-recursive** watch on
`/home/hasan` and a recursive one per child. That covers everything that
existed at start-up. It does not cover anything created afterwards, because a
non-recursive inotify watch reports the creation of a child directory and does
not descend into it — and `translate` treats that event as an ordinary upsert
of one row.

Reproduced:

```bash
mkdir -p ~/kacaktest-a/alt/derin && echo x > ~/kacaktest-a/alt/derin/zzqq1.txt
sleep 25 && scour zzqq1
# nothing at /home/hasan/kacaktest-a/... — see §3 for where it did turn up
```

Ten minutes later a second file written into the same directory was still not
seen under its real path.

**What it means for a user:** clone a repository into your home directory and
it does not exist as far as search is concerned, until something triggers a
full walk.

**The shape of a fix:** a `Create` event for a directory is not an upsert, it
is a `Change::Rescan` of that path — the engine already knows how to walk a
subtree, and a walk covers both the directory and everything the watch could
not have seen inside it. `cover` should also be re-entered for the new
directory so that live updates continue below it.

---

## 2. Files written immediately inside a brand-new directory are lost

The same gap without the split, and it applies on every platform. Between
`mkdir a/b` and the moment `notify` has processed that event and added a watch,
anything created inside `a/b` produces no event at all. inotify has nothing to
report an event *against*.

Reproduced, twice, with the two halves separated:

| | on disk | in the index |
|---|---|---|
| `mkdir d && echo > d/f` (same instant) | yes | **no** |
| `mkdir d`, wait 8 s, `echo > d/f` | yes | yes |

`/home/hasan/Projeler/zzqqdirB/zzqqB.txt` stayed missing for the rest of the
session. Nothing recovers it but a rescan.

`mkdir -p x/y && touch x/y/z` is what `git clone`, `cargo new`, `unzip`, `tar
-x` and every installer do. This is not an edge case.

Same fix as §1: a created directory means "walk me", not "add a row".

---

## 3. The watcher follows symlinks; the walk does not

`scan` runs with `follow_symlinks = false`. `notify`'s recursive watch does
not, and this machine has:

```
~/.wine-hukuk/dosdevices/z: -> /
```

**Correction, and it is worth keeping rather than editing away.** The first
reading of this was that the watcher had walked out onto the whole root
filesystem, on the strength of the watch count:

```bash
grep -c '^inotify' /proc/$(pgrep -f 'bin/scourd$')/fdinfo/*
# 242643
```

That number is not evidence of anything. `find /home/hasan -xdev -type d`
returns **242,242** — the home directory costs essentially all of it on its
own, and there was never room for `/mnt/depo` in there. The inference was
wrong and the arithmetic that would have caught it took one command.

What *is* real is the rows, which were reproduced directly. Every file created
in the home directory is indexed a second time under a path that does not
exist:

```
/home/hasan/.wine-hukuk/dosdevices/z:/home/hasan/kacaktest-a/alt/derin/zzqq1.txt
```

And because that string *is* textually under `/home/hasan`, every sweep of the
`home` source kills those rows, and the watcher puts them back. A permanent
churn loop between the walk and the watch, each undoing the other.

**The shape of a fix:** `cover` must not descend through a symlink when the
scan is not following them — the two have to agree, or the index holds rows the
sweep is guaranteed to delete.

---

## 4. `Maintenance::Rebuild` does not terminate while anything is changing

```rust
loop {
    let group = groups().find(|g| g.len() > 1 || (one segment with dead rows));
    match group { Some(g) => self.fold(&g)?, None => break }
}
```

`fold` releases the lock while it builds, which is what made it safe against
searches. It also means a commit can land during the fold, and a commit appends
a segment **to the current generation** — so the group the loop just folded has
two members again, and it folds the entire index a second time.

Measured:

```bash
scour maintain rebuild     # polled until unsorted == 0
# gave up after 10 minutes; index still at 38 segments,
# back to 52 a few minutes later
```

`Maintenance::Compact` does not have this problem: `next_head` requires a group
of three and excludes the largest member unless a quarter of it is dead, so
each round strictly shrinks. `Rebuild` has no such guard.

**The shape of a fix:** decide the work once, before the first fold, and do
only that. A rebuild is "make the segments that exist right now into one", not
"loop until the index stops changing", which on a live machine is never.

---

## 5. `.alive` files are recreated after their segment is erased

182 orphan segments on disk, against 55 in the manifest:

```bash
# 182 segments' files exist that the manifest does not name; 191,497 bytes.
# Mostly a lone .alive — one of them 160,843 bytes.
```

The order in `flush_prepare`:

1. snapshot `alive` for every **touched** segment,
2. `forget_empty` drops the segments that went fully dead and returns them,
3. `save_meta`,
4. `Live::erase` unlinks their files.

Then the caller — `commit`, deliberately outside the lock — runs
`pending.write_alive(&self.dir)`, which writes an `.alive` for every segment in
the snapshot **including the ones just erased**. A segment that was swept empty
is both touched and gone, so its bitmap comes back as a file nothing will ever
open or remove.

They also inflate `bytes_on_disk`, which is `dir_size` over the whole
directory, so the status line over-reports.

A second, smaller source of orphans: `Live::write` writes eight files one at a
time, and a kill in the middle leaves a partial set the manifest never names.
`seg-00000051` has `{cols, names}`, `seg-00000133` has `{names, cols, dirs,
ids}` — the first two and the first four in write order.

**The shape of a fix:** drop the erased numbers out of `pending.alive` before
returning it, and sweep unreferenced `seg-*` files at open time.

---

## 6. Fragmentation is real, and it is not what a search pays for

This one corrected me. The index had drifted to 56 segments and **851,471
unsorted entries** — four times `rebuild_threshold` — and the obvious reading
was that this is why broad queries are slow. It is not.

| query | before: 56 segments, 851k unsorted | after: ~1 real segment, 97 unsorted |
|---|---|---|
| `rapor` | 26.20 / 28.01 ms | 34.50 / 35.28 ms |
| `cargo` | 10.74 / 12.35 ms | 12.64 / 14.06 ms |
| `config` | 10.01 / 11.96 ms | 11.03 / 15.72 ms |
| `ext:pdf` | 50.29 / 51.90 ms | 55.14 / 60.48 ms |
| `test` | 30.44 / 34.23 ms | 34.31 / 34.34 ms |

Warm, three runs each, first discarded. `rows_visited` barely moved: 288,896 →
288,000 for `rapor`.

So what a broad query costs is the **number of candidate rows**, not the number
of segments holding them. Rebuilding cost more than ten minutes of a core and
bought nothing.

Two things follow. `rebuild_threshold` is measuring something that does not
predict latency, and the interesting question for `rapor` at 34 ms is not "how
many segments" but "why are 288,000 rows candidates for a five-letter term" —
which is the trigram filter's block granularity, not the segment list.

---

## 7. Idle housekeeping never runs on a machine in use

```rust
if !dirty && !idle_done && last_commit.elapsed() >= idle_after { ... }
```

`idle_after` is 20 s and it is measured from the last **commit**. Any change
arriving within 20 s of a commit sets `dirty` and resets the wait. A desktop
produces one filesystem change every few seconds — a browser cache, a journal,
an editor — so the window does not open.

The escape hatch that exists for exactly this, `compact_urgent`, is set to 64.
The index sat at 52–56 for the whole session. Neither path ran.

Together with §4: automatic maintenance either never starts, or never stops.

---

## 8. A pending `rm -rf` made a commit quadratic — measured, then fixed

Every removed path becomes one entry in `hidden_prefixes`. `flush_prepare`
then, per segment, built one `DirScope` per prefix and checked **every live
row** against **every scope**. Commits run once a second, so the list is
bounded by a second of removals — and a second of `rm -rf node_modules` is
thousands of paths.

It was flagged unmeasured, so the first thing was to measure it:

```bash
cargo run --release -p scour-index-native --example removal
```

One million rows, one segment, timing the commit alone:

| removed paths in the batch | before | after |
|---|---|---|
| 1 | 14.89 ms | 14.18 ms |
| 16 | 20.77 | 16.22 |
| 256 | 174.24 | 27.68 |
| 1,024 | 567.42 | 41.59 |
| **4,096** | **2.24 s** | **77.52 ms** |
| one prefix over the whole subtree | 15.08 ms | 16.35 ms |

At the batch size the engine actually uses, **29×**, and 547 µs a path became
18.9. The ~14 ms floor is the pass over a million rows that a commit does
anyway.

Two changes, and the first is the one that matters:

* The question is asked **from the path, not from the list**. A path has a
  dozen ancestors however many thousand members the set has, so "is this under
  anything removed" is a dozen lookups and stops growing with the batch. That
  is `scour_core::PrefixSet`, shared with the engine's walk coalescing, which
  had grown its own copy of the same idea.
* Within a segment, the prefixes become **merged ranges of directory numbers**
  plus a list sorted by parent for the rows a range cannot reach — a removed
  file has no directory number of its own, and a removed directory's own row
  carries its parent's.

The obvious version of the first — sort the members, binary-search for the
greatest one not after the path — is **wrong**, and a test rather than a
review said so. Sort order does not put an ancestor next to its descendant:
with `/pkg/lib` and `/pkg/lib-old` both removed, `/pkg/lib/deep/f.rs` sorts
*after* `/pkg/lib-old`, because `-` is below `/`. One comparison lands on the
member that does not match and misses the one that does.

---

## 9. Smaller things, in one place

Fixed:

- **`sweep` built a path per out-of-scope row** — a `String` for every live row
  the directory scope did not already accept, under the write lock. Harmless
  when a sweep followed a full rescan; not harmless now that creating a folder
  queues a walk. The directory number already answers it.
- **An empty-path `Rescan` was not coalesced**, so ten overflow notifications
  were ten full walks of every source, one after another on the worker thread.
- **The owner lookup in the `Rescan` arm was a bare `starts_with`**, unlike
  `Engine::owner_of` two hundred lines above it. A root of `/home/hasan`
  claimed `/home/hasanX`. Both now go through the same function.

Left alone, deliberately:

- **The durability window.** `commit` saves the manifest under the lock and
  writes the `.alive` bits after releasing it, so a hard kill in between brings
  some deleted rows back until the next sweep takes them.

  The fix is to write the bits before the manifest, and the fix is worse than
  the fault: that write is an `fsync` per touched segment, measured at 13 ms a
  segment and three or four segments a commit, once a second, with every search
  waiting. Trading a bounded, self-healing staleness after an unclean shutdown
  for 40 ms of held lock every second is the wrong way round. A graceful stop
  commits, so this needs a `SIGKILL` or a power cut to happen at all.

  What would close it honestly is a clean-shutdown marker in the manifest, so
  the engine could rescan after an unclean start. That is a format bump, and a
  format bump costs the user a full reindex — too much for this.

- **An unknown `word:` prefix is searched for literally.** `is:dir` finds
  nothing and looks broken; `explain` says `name contains "is:dir"`, correctly,
  and the token is `folder:` or `kind:folder`. Nothing is wrong in the engine;
  the window simply never shows what `explain` already knows. That belongs with
  the coloured query chips (Phase 5.2), not here.

---

## 10. Android

Not a review finding — the question was whether anything blocks a port.

**Nothing in the dependency graph does.** Every engine crate cross-compiles
today, unchanged:

```bash
ANDROID_NDK_HOME=~/Android/Sdk/ndk/28.2.13676358 \
  cargo ndk -t arm64-v8a check -p scour-core -p scour-query -p scour-config \
    -p scour-index-native -p scour-source-fs -p scour-engine -p scour-ipc \
    -p scour-proto -p scour-i18n
# Finished `dev` profile in 2.92s
```

That covers mmap, `flock`, inotify, unix sockets and the walker. Slint 1.16.1 —
the version pinned for the Wine crash — already carries
`backend-android-activity-06`.

What does not port is not a crate, it is the shape of the program:

- **There is no daemon on Android.** `scour-gui` is a `[[bin]]` that owns no
  index and talks over a socket; Android gives an app one process and kills it
  when it is not in front. The engine has to be built in-process. The
  architecture is already arranged for that — `apps/scourd/src/wire.rs` is the
  only file in the workspace that names a concrete implementation, so an
  Android entry point is a second file of that shape plus a `cdylib` and an
  `android_main`.
- **`platform_defaults()` has no Android arm.** `target_os = "android"` is not
  `target_os = "linux"`, so the list comes back empty and `/proc` and `/sys`
  are fair game. The compiler already says so — `rules.rs:135`, "variable does
  not need to be mutable", only under that target.
- **`directories` has no Android knowledge.** The config and index directories
  must come from the JNI `Context`, not from XDG.
- **Scoped storage is the real constraint.** Since Android 11 an app cannot
  walk anything outside its own directory without `MANAGE_EXTERNAL_STORAGE`,
  which Google Play grants to file managers and little else. Sideloaded or on
  F-Droid this is unrestricted.
- **inotify on `/sdcard` is unreliable** — it is a FUSE mount and events are
  frequently not delivered. Live updates there probably have to be a periodic
  walk, which the engine can already express as a source with no `WATCH`
  capability.
- Global hotkey, tray and "open the containing folder" have no counterpart.

Scale is the one thing that gets easier: a phone holds a few hundred thousand
files, not two million.

---

## 11. A path rewritten atomically leaves a row behind — every time

Found on 2026-08-05 while verifying live results, and fixed the same day by
answering the question underneath it: **what identifies a row?**

Identity is the inode where the filesystem promises stable ids. An editor, a
browser and every other careful writer save by writing a temporary file and
renaming it over the target — so the path survives and the inode does not. The
upsert that follows carries a *new* id, nothing ever says the old one is gone,
and both rows stay in the index at the same path.

Reproduced against the live service, on a file Ladybird rewrites every few
seconds:

```
scour search alt-svc-cache
# 10 of 267 …   ten identical paths, sizes alternating 1.24 KiB / 0 B

# and the ids differ, which is the whole story:
#   ino 3680329  1269 B  20:26:25
#   ino 3680323     0 B  20:26:25
#   ino 3680303     0 B  20:26:25
#   ino 3680280     0 B  20:26:22
```

It is unbounded: one row per save, for as long as the service runs. A file
modified **in place** is fine — eight writes to the same inode stay one row —
so this is specifically the write-and-rename pattern, which is most of them.

### What it turned out to be

Not a missing removal — a **second notion of identity**. The index keyed rows
on whatever the source called an entry; the source called it an inode; and the
only thing a watcher can name when something disappears is a path. Two
vocabularies, and the one that could not express a removal was the one rows
were filed under.

So the path is the identity now, everywhere:

* the row table is keyed on a digest of source and path, and a candidate is
  confirmed against the directory number and name the row actually carries —
  exact, so a digest collision cannot kill the wrong row;
* `KeyKind`, `KeyA` and `KeyB` are gone from the columns, which is where the
  second identity was stored;
* `Change::Remove(EntryId)` is gone from the vocabulary. Nothing ever emitted
  it, and `Change::path()` had to answer `None` for it — the type saying out
  loud that the variant could not describe itself;
* `Caps::STABLE_IDS` and the filesystem trait behind it are gone with it. The
  measurement that produced them still stands (vfat and exfat invent `st_ino`;
  0 of 50 files kept theirs across a remount) and is kept in
  `examples/filesystems.rs` — it is simply no longer load bearing, because
  nothing asks a filesystem for an identity.

Measured after: ten atomic saves over one path leave **one row**, the same file
that had 267. Costs and gains in `docs/MEASUREMENTS.md` — the index is 11.6%
smaller and a rescan writes 26% slower, which is the price of confirming every
kill against the real name.

The mock filesystem was generating duplicate paths — five in 8,667 — and the
old identity hid it, because two rows at one path looked like two files. A
generator that produces what a filesystem cannot is worse than a missing case:
every count taken from it disagrees with a correct index.

---

## 12. An outside review, and the four things underneath it

On 2026-08-05 the tree was handed to a second model with one instruction: be
adversarial, measure, and criticise. Its report is kept verbatim in
[`REVIEW-2026-08-05.md`](REVIEW-2026-08-05.md) — twenty findings with
reproductions, a scratch program that produces them, and its own interleaved
measurements of the walk, the builder and the removal path.

The findings are not twenty separate mistakes. They are four, each of which
this codebase had made in several places at once:

| | the mistake | what it produced |
|---|---|---|
| **A** | a failure is a value that can be ignored | a full disk silently discarded everything written since the last commit, and announced a revision |
| **B** | the watcher's "I do not know" was rendered as "nothing happened" or "it is gone" | an overflowed queue drifted for ever; an unreadable directory was deleted |
| **C** | a path was a string to compare, not a location to resolve | `stat` walked out of its source with `..`; `.git/objects` was never excluded; one source swept another's rows |
| **D** | a restart was assumed to be a continuation | changes made while the service was stopped never arrived, and the first walk ran before anything was watching |

### Fixed here

* **A.** `Pending` carries the only copy of the staged rows and every failure
  path hands it back (`NativeIndex::restore`). The engine keeps `dirty`, does
  not clear `pending`, does not announce a revision, and counts the failures in
  `Status::unwritten`. A scan whose batches did not all land does not sweep —
  the sweep's evidence is the walk, and a batch that failed is a file that
  exists and is unstamped. An unreadable manifest is `IndexCorrupt` rather than
  "this index is empty", and a `.alive` file of the wrong length is damage
  rather than "every row is dead".
* **B.** `event.need_rescan()` is honoured before anything else, which is how
  every backend says it lost track — inotify sends it with **no paths**, so the
  arm that looped over paths did nothing with the one message that matters. A
  `stat` that fails with anything other than `NotFound` queues a walk instead
  of a removal. Cover failures after the fact join the reported set instead of
  being dropped on the next line.
* **C.** `FsSource::stat` resolves the parent and compares it against resolved
  roots, so `..` and intermediate symlinks cannot leave the source; the final
  component is joined back unresolved, because a symlink is a row of its own.
  Directory rules compile to component sequences matched at a boundary, so
  `.git/objects` excludes its tree — on this machine `.cargo/registry` alone
  was **133,152 indexed files** that the defaults had always claimed to skip.
  `Index::sweep` takes the `SourceId` whose walk it is.
* **D.** `scan.on_start` defaults to **true**: with no journal, a change made
  while the service is stopped has no other way in. The baseline walk is queued
  *after* the watches are installed, which is the rule this codebase already
  worked out for subtrees and had not applied to the biggest walk of all.

### Not fixed, and deliberately

* **Non-UTF-8 names collapse.** `to_string_lossy` gives two distinct byte names
  the same path, and path identity then makes them one row. This is the
  expensive one: a native key means a format revision, a wire-vocabulary change
  and every frontend. It should be done together with source qualification of
  `Change`, stable source ids and object identity, in one revision rather than
  four lossy ones.
* **Hard links are counted twice by `usage`.** `usage.rs` still says inode
  identity makes one row per object; that premise went when identity became the
  path. Search returning both names is right; `du` semantics need an object
  identity captured at scan time.
* **Mount identity is never checked**, so an unmount mid-scan can still
  authorise a sweep, and an emptied source cannot be reconciled at all.
* **Windows does not compile** (three errors), and nothing tests any target but
  Linux.
