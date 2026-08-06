# Enumeration: what is fast, what needs root, what is already ours

Everything (voidtools) is fast on Windows because it does not walk the tree: it
reads the NTFS **$MFT** — one contiguous metadata table — and then follows the
**USN change journal**. This is a survey of whether any other filesystem offers
the same thing, what privilege each path costs, and how much of the theoretical
win is left once a plain parallel walk has been measured honestly.

Everything below was measured on this machine unless marked otherwise. Kernel
6.18, btrfs on `/` and `/home` (separate subvolumes), an `ntfs3` volume at
`/mnt/depo` with 1.5 M entries. No root. Scratch code under `/tmp/fsbulk/`.

---

## 1. The finding that matters most

**The kernel is not what is slow.**

| path, 1.5 M entries on NTFS, warm cache | time |
|---|---|
| bare parallel `getdents64`, 20 threads | **292 ms** |
| + `statx`, full metadata | 348 ms |
| `ignore` crate + channel — Scour's walker shape | 593 ms |
| **Scour, end to end** | **2,272–3,067 ms** |

```bash
/tmp/fsbulk/walk /mnt/depo 20      # 1,565,767 entries in 292.4 ms
```

The walk is under a quarter of the total. **About 1.7 s is downstream of it**
— building the index, not reading the filesystem. No ioctl on any platform
competes with that, which makes the sink the first thing to work on and every
platform-specific enumeration path the second.

Single-threaded, the same walk takes **14,507 ms**. Threads are worth 50×
here; that is the one number about walking that is already settled.

## 2. `$MFT` is readable on Linux without root

`ntfs3` exposes it as an ordinary file. It does not appear in `readdir`, but
opening it by name works, and the `uid=` mount option makes the caller its
owner:

```
-rwxr-xr-x 1 hasan hasan 2213543936 /mnt/depo/$MFT
$ head -c 4 '/mnt/depo/$MFT'  →  FILE
```

`$LogFile`, `$Secure` and `$Bitmap` open too. `$Extend/$UsnJrnl` is mode 000 —
so the *journal* is closed even though the table is open — and the raw device
is not readable without being in group `disk`. The obvious next idea does not
work either: the journal's data lives in the `$J` alternate data stream, and
`ntfs3` exposes no alternate data streams at all — `$Extend/$UsnJrnl:$J` does
not resolve, and the driver surfaces no extended attributes to reach it
through.

**The journal itself is alive, which the table says outright.** `$UsnJrnl` is
record 38; it carries an `$ATTRIBUTE_LIST`, and following that lists
`$DATA:$J` in records 251269 and 537269, the second starting at VCN 148144. So
Windows is keeping it, and this is not a volume with journalling switched off.
What stands between us and its bytes is that `$J` is non-resident: its runs
point at clusters, and reading clusters means the raw device —
`/dev/nvme1n1p2`, `root:disk`, and this account is not in `disk`.

So the distance to a USN feed on Linux is one `usermod -aG disk`. That is not a
small grant: group `disk` is read access to every block device, which is read
access to every file on the machine regardless of permissions.

**And it would buy something broken, which was measured rather than assumed.**
`$J`'s own record gives its size without reading a single cluster: 7,352,352,768
bytes allocated, 7,352,039,480 written. A file was then created through `ntfs3`,
`sync()` called, three seconds waited, and the record read again bypassing the
page cache:

```
$J before : alloc 7,352,352,768 · real 7,352,039,480
$J after  : alloc 7,352,352,768 · real 7,352,039,480   (unchanged)
```

Not one byte. Windows keeps the journal; `ntfs3` does not append to it. So a
journal follower on Linux would never see a file Linux itself wrote — the feed
is not merely expensive to reach, it is **wrong**. On Linux this volume has a
free bulk path and no change feed worth having.

The inversion is worth stating plainly, because it decides both ports: **on
Windows the journal is complete and the table is a snapshot; on Linux the table
is complete and the journal is blind.** Each system keeps its own record fully
and the other's not at all. That is why a Windows port must read `$MFT` at
startup rather than trusting a stored USN — anything the other system wrote
while it was off is in the table and nowhere else.

**A Linux write reaches the table.** Worth checking before building anything on
it, because the volume is mounted `rw` and both systems write to it: a file
created through `ntfs3` was found in `$MFT` afterwards — once, by its UTF-16
name, through the page cache and again with `O_DIRECT`, against a control name
already on the volume found 197 times. So the table stays authoritative no
matter which system wrote, which is the whole premise.

Two limits found with it. The record arrives **late**: the same probe found
nothing after `sync()` and a second, and found it after a second and a half.
And `$MFT`'s own mtime and size do **not** move when the volume changes, so
there is no cheap "has anything changed" probe — the table offers a snapshot,
never a delta.

That also names a trap for the eventual Windows port. `ntfs3` maintains the
table but there is no reason to think it maintains the *USN journal* — so on a
machine that boots both, an indexer following only the journal misses
everything the other system wrote. Everything survives this by re-reading
`$MFT` at startup; a design that only ever follows the journal would not.

**And the volume does not deserve watching at all, which the timestamps say
outright.** Of its 177,915 directories:

| changed within | directories |
|---|---:|
| a day | **4** |
| a week | 17,480 (9%) |
| a month | 34,523 cumulative (19%) |
| **over a year ago** | **93%** |

It is an archive. Watching it costs 152,529 inotify watches — 64% of this
machine's entire budget — to hear about four directories a day.

What replaces it is a sweep of the directory timestamps, which needs no
enumeration because the index already knows every directory: **278 ms for
177,915 `stat` calls**, 1.8 µs each. Once a minute is half a percent of a core
and a worst case of sixty seconds on a disk that changes four times a day.

Two things were tried against that and are recorded so they are not tried
again. **Parallelising the sweep makes it slower** — 279 ms on one thread, 690
on four, 1,319 on eight: `ntfs3` serialises metadata reads and the threads only
add contention. And **there is no cheap "did anything change" flag**: `$MFT`'s
mtime does not move, `$LogFile`'s first pages do not change on a Linux write
(`ntfs3` does not write NTFS's own log), and `statvfs` free blocks move only
for changes that alter allocation — a rename would slip past it.

**Which is a plan rather than a gap.** A volume whose whole table reads in
1,425 ms cold and 454 ms warm does not need watching: reconciling it is
cheaper than subscribing to it. On this machine that is **152,529 of the
237,488 indexed directories — 64% of the watch budget** — and it costs no
privilege, because none of what it needs is behind one. What is left to watch
is `/home`, 85,081 directories on btrfs, and that is the only place where
`CAP_SYS_ADMIN` has anything to sell.

None of the reading above is implemented in Scour: it is a measurement of what
a parser could do, made with a standalone one (`mft.c`). There is no `Caps`
flag for it yet — `WATCH`, `RECURSIVE_WATCH`, `JOURNAL`, `CONTENT` and
`CASE_SENSITIVE` are what exist — and adding one is the smaller half of the
work.

A full parser (fixups, `$STANDARD_INFORMATION`, `$FILE_NAME` with 8.3-namespace
dedup, `$DATA` real size, parent-reference path reconstruction) produced
**1,560,786 entries** with paths, sizes and all four timestamps, byte-exact
against `stat()`. The walk finds 1,565,767 — 0.3% apart, being hard links and
metadata files.

| | cold | warm |
|---|---|---|
| `getdents64` + `statx` | 7,657 ms | 348 ms |
| **`$MFT` parse, single thread** | **1,425 ms** | 454 ms |

**5.4× on the cold path**, and 3.4× against Scour's measured 4.8 s. 1,270 ms of
that 1,425 is sequential I/O at 1.7 GB/s, so it is I/O-bound and close to its
floor; only overlapping read with parse would help.

**Nothing grows when a file is created, and that is not a failure to record
it.** The table is 2,213,543,936 bytes — 2,161,664 fixed 1 KB records, laid out
at format time. A create claims a free slot and writes it in place; a delete
clears the in-use bit. `ntfs3` reports the record number as the inode, so this
is watchable without scanning anything:

```
file created  → inode 200504 · record 200504 in use, name present
file deleted  →               record 200504 free,   name gone
$MFT size     : 2,213,543,936 bytes, both times
```

It is an array, not a log. Which is why `$MFT`'s size and mtime say nothing
about whether the volume changed, and why the only cheap delta on NTFS is the
journal — the one thing Linux cannot keep.

The last line above is also a trap for a parser: `ntfs3` **zeroes** the record
on delete, so the name is gone. Windows usually leaves the bytes, which is what
undelete tools live on. A reader must believe the in-use flag, not the presence
of a name, or it indexes deleted files on one platform and not the other.

Traps, all of them found by hitting them: NTFS fixups must be applied before
parsing; a record can hold several `$FILE_NAME` attributes (76,476 do on this
volume) and a naive reader emits `PROGRA~1` beside `Program Files`; times come
from `$STANDARD_INFORMATION` but size from `$DATA`, because `$FILE_NAME`'s
copies are stale by design; extension segments must be skipped.

## 3. Every other path

| filesystem | bulk path | privilege | returns |
|---|---|---|---|
| **NTFS via ntfs3** | `$MFT` as a file | **none** | name, parent, size, 4 times, attrs |
| NTFS on Windows | `$MFT` raw volume | Administrator | same |
| NTFS on Windows | `FSCTL_ENUM_USN_DATA` | Administrator | names + parents only — no size, no mtime |
| **btrfs** | `TREE_SEARCH_V2` | **CAP_SYS_ADMIN** | everything — but 31× slower than walking; see §7 |
| **XFS** | `XFS_IOC_BULKSTAT` | **CAP_SYS_ADMIN** | metadata but **no names** |
| ext4, F2FS | none exists | — | `getdents64` is the floor |
| exFAT, FAT32 | none possible | — | directory entries *are* the metadata |
| **ZFS** | `ZFS_IOC_NEXT_OBJ` | none | but paths need `OBJ_TO_STATS` → root |
| **APFS, HFS+** | `getattrlistbulk` | **none** | full `struct stat` per entry |
| ReFS | none | — | `GetFileInformationByHandleEx` only |
| **Windows, any fs** | `GetFileInformationByHandleEx` | **none** | name, size, allocation, 4 times |

### btrfs deserves a paragraph: the best design here, and not a scanner

`TREE_SEARCH_V2` returns names, full metadata including birth time, parent
links, *and* a `min_transid` cursor that prunes at node-pointer granularity
without reading skipped subtrees — a real journal, the thing `Caps::JOURNAL`
was reserved for. It has been `CAP_SYS_ADMIN` since 2.6.34.

The "`tree_id == 0` works unprivileged" folklore is false: the capability check
is the first statement in `btrfs_ioctl_tree_search`, before any argument is
read. A user namespace does not rescue it either, because btrfs calls
`capable()` rather than `ns_capable()`:

```
unshare -Ur   (uid 0 inside the namespace)
BTRFS_IOC_TREE_SEARCH_V2 → -1 EPERM
```

`INO_LOOKUP` does succeed unprivileged, but only for `objectid == 256`, where
it returns the subvolume id and a deliberately emptied name.

### Windows needs no privilege for its biggest win

`GetFileInformationByHandleEx` with `FileFullDirectoryInfo` returns name,
`EndOfFile`, `AllocationSize` and all four timestamps per entry in one buffered
call, and Microsoft documents that *no specific access rights are required*.
There is no per-file `stat` on Windows at all. Use `FileFullDirectoryInfo`, not
`FileIdBothDirectoryInfo` — the docs say the `FileId` costs an MFT lookup.

This should be built before anything that needs Administrator. It is also the
only bulk path that works on ReFS.

### macOS

`getattrlistbulk` needs no privilege and gives a full `struct stat` equivalent
per entry. Two traps from the XNU source: always request `ATTR_CMN_OBJTYPE`,
and never request `ATTR_CMN_UUID` / `GRPUUID` / `EXTENDED_SECURITY` — any of
those makes the kernel fall back silently to a per-entry lookup loop. Apple's
own `fts(3)` uses it correctly and DTS recommends `fts` over the raw call.

## 4. Measured and rejected

**io_uring is slower.** 200k paths, same machine: synchronous loop **666
ns/call**, io_uring **723 ns/call**. `IORING_OP_STATX` sets `REQ_F_FORCE_ASYNC`
at prep time, so every call is punted to a kernel worker. `IORING_OP_GETDENTS`
does not exist — the 2021–2023 series died on inode locking and was never
merged. Nobody had published this comparison.

**Narrowing the `statx` mask buys nothing.** 1.04 M paths, every mask from
`STATX_TYPE` to `0xffff`: all within **752–1026 ns/call**. An early measurement
suggesting a 4.8× spread was a cold-inode-cache artifact of run ordering.
`AT_STATX_DONT_SYNC` is a documented no-op on local filesystems, and
`AT_STATX_FORCE_SYNC` measured *faster* — which is to say, noise.

**Buffer size barely matters.** 4 KB is 15–20% worse; 16 KB through 1 MB are
all within noise. 32 KB matches glibc and is what the measurements above use.

**What an inotify watch actually costs, since the folklore is a round number.**
Adding them: 4,993 watches in 7 ms — **1.5 µs each**, so the 455,367 this
machine holds are 0.7 s of setup and nothing else. On the write path, three
interleaved rounds of 3,000 creates and deletes in a watched directory against
an unwatched one on the same filesystem:

| | create | delete |
|---|---:|---:|
| unwatched | 19.8 / 20.2 / 19.8 ms | 5.7 / 5.8 / 5.8 ms |
| watched | 21.7 / 22.0 / 21.9 ms | 6.1 / 5.9 / 6.0 ms |

**About 0.7 µs a file, ~10% on creates and ~3% on deletes**, with nobody even
reading the events. Memory could not be measured from an unprivileged account:
the delta is under `/proc/meminfo`'s noise floor and `/proc/slabinfo` is
root-only. The mark itself is small; the usual "1 KB a watch" is mostly the
inode and dentry a watch **pins**, which is memory the kernel can no longer
reclaim — stated from the structures rather than measured here.

So watching is not expensive. The problem is not the price of a watch, it is
that the budget is **small, shared with every other program the user runs, and
fails silently**: `inotify_add_watch` returns `ENOSPC`, the directory is simply
never watched, and nothing anywhere says so. That is the difference between
slow and wrong, and it is why the answer is `Change::Rescan` rather than a
bigger number.

**fanotify is inotify with a worse budget.** Unprivileged
`FAN_CLASS_NOTIF|FAN_REPORT_DFID_NAME` initialises, and the FID design works —
an event's `dfid` byte-matches a handle from `name_to_handle_at`, which needs
no capability, so paths can be resolved from your own map without the
`CAP_DAC_READ_SEARCH`-gated `open_by_handle_at`. But:

```
FAN_MARK_ADD (single directory)   →  0   ok
FAN_MARK_ADD|FAN_MARK_MOUNT       → -1   EPERM
FAN_MARK_ADD|FAN_MARK_FILESYSTEM  → -1   EPERM
```

The whole-filesystem mark is the only reason to prefer fanotify, and it needs
`CAP_SYS_ADMIN`. Unprivileged, the mark budget is 268,590 against inotify's
524,288.

**Sorting by inode before stat: inconclusive.** Controlled cold trials on
btrfs: 4,741 ms unsorted / 6,912 sorted, then 5,602 unsorted / 3,926 sorted.
Warm, no difference. The variance exceeds the effect, so no number is claimed.
It would plausibly matter more on ext4, whose inode table is a flat array;
that could not be tested without root.

**exFAT/FAT raw parsing is pointless.** The directory entries *are* the
metadata — a raw parser reads the same bytes as `getdents64`, for the price of
root and a coherency hazard. Worth knowing for identity, though: both Linux
drivers assign `i_ino = iunique()`, so `st_ino` is not stable across a remount
or even across inode eviction. `Caps::STABLE_IDS` must be false there, which
[`fs.rs`](../crates/scour-source-fs/src/fs.rs) now ensures.

**ZFS.** `ZFS_IOC_NEXT_OBJ` is genuinely unprivileged, but turning objects into
paths needs `OBJ_TO_STATS`, which needs `CAP_SYS_ADMIN` or a one-time root
`zfs allow <user> diff`. Open issue #13951 breaks `zfs diff` for this case
anyway, and no Rust crate wraps any of it.

## 5. Two defects this survey found in Scour

### `EntryId` is not durable across a reboot on btrfs

`scan.rs` builds identities from `m.dev()`, and btrfs `st_dev` is an
**anonymous** device allocated at mount time — major 0, not stored on disk:

```
/home     dev=0:52  ino=257  subvol=257
/var/log  dev=0:58  ino=256  subvol=262
```

Inode numbers restart at 256 in every subvolume, so `dev` is the only thing
keeping them distinct — and if it changes between boots, every identity in a
persisted index changes with it and the next scan sees a filesystem full of new
files.

The durable key is **`stx_subvol`** (`statx`, kernel ≥ 6.10; btrfs fills it
unconditionally, at no cost). Branch on `result_mask & STATX_SUBVOL` and fall
back to `dev` elsewhere. This machine runs 6.18, so it is available today.

### The recorded reason for `watching 0` is wrong

`MEASUREMENTS.md` blamed 112,148 directories against the inotify per-user
limit. **That limit is 524,288 on this machine**, and a raw loop installs
**227,806 watches in 311 ms using 28 MB** with no `ENOSPC` at all. Running
`notify` 8.2 the way `watch.rs` does:

```
watch(/home/hasan) → Err in    319 ms: PermissionDenied ".../waydroid/data/vendor"
watch(/mnt/depo)   → Ok  in 39,879 ms, VmRSS 78 MB
```

It is **one `EACCES` on one unreadable directory, and `notify` abandons the
whole recursive watch**. Not the limit. So `Caps::RECURSIVE_WATCH = false` on
Linux currently rests on a diagnosis that does not hold — though 40 s to
install watches is a separate problem worth its own look.

## 6. What to build, in order

1. **The sink.** 593 ms of walker against 2,272 ms total means ~1.7 s is
   downstream. Nothing platform-specific competes with that, and it costs no
   new dependency and no new crate.
2. **`scour-source-mft`** — `$MFT` on Linux/ntfs3. The only bulk path open to
   an unprivileged daemon on a filesystem we actually have, and 3.4× on the
   cold number a user feels. A new crate implementing `Source`, named only in
   `apps/scourd/src/wire.rs`; `scan()` reads `$MFT` and everything else
   delegates to `FsSource`. Must probe at run time — `open($MFT)` succeeding
   *and* record 0 starting with `FILE` — and fall back silently. ~600 lines.
   `Caps::STABLE_IDS` is genuinely true here: ntfs3 exposes MFT record numbers
   as inode numbers.
3. **Windows `GetFileInformationByHandleEx`.** No privilege, removes every
   per-file stat, works on ReFS. `windows-sys` 0.61 has complete bindings.
4. **macOS `getattrlistbulk`**, with the two traps above.
5. **`Caps::JOURNAL`, and only on macOS.** FSEvents is a real durable journal:
   persist `(FSEventStreamGetLatestEventId, FSEventsCopyUUIDForDevice)`, replay
   with `FSEventStreamCreateRelativeToDevice`, and treat a changed UUID as
   "rescan". **No Rust crate exposes it** — `notify` and `fsevent` both
   hardcode `kFSEventStreamEventIdSinceNow` in a private field — so it is a
   small amount of `objc2-core-services` FFI. It is the difference between a
   rescan on every daemon start and none. Google Drive's `changes.list` token
   is the other durable cursor. On Linux there is nothing without root.

**Still not worth it:** XFS bulkstat, which is `CAP_SYS_ADMIN` *and* returns no
names, so it could not feed a name index even with the capability. ext4, F2FS,
exFAT and FAT have nothing to reach for. io_uring is slower. Unprivileged
fanotify is inotify with a smaller budget.

---

## 7. The privileged scanner

Everything ships a Windows service running as Administrator, because `$MFT`
there needs raw volume access. The same shape answers btrfs, whose
`TREE_SEARCH_V2` is the best interface in this survey and needs
`CAP_SYS_ADMIN`. So the rule is not "which platform" but **"is there a bulk
path behind a privilege"**:

| | bulk path | service needed |
|---|---|---|
| Windows, NTFS/ReFS | `$MFT` + USN journal | **yes** — Administrator |
| Linux, btrfs | `TREE_SEARCH_V2` + `min_transid` | **yes** — `CAP_SYS_ADMIN` |
| Linux, NTFS via ntfs3 | `$MFT` as an ordinary file | **no** — already ours |
| everything else | `getdents64` | no — nothing to gain |

### It takes no commands, and that is the design

The scanner is not a smaller daemon. It has **no socket, no pipe, no command
of any kind**. Its entire input is a list of roots; its entire output is the
index. There is no "delete", no "rescan this", no "stop" — not disabled, *not
implemented*. A request that does not exist cannot be abused, and a feature
added to Scour later cannot reach through it, because the scanner would have
to be taught a verb it does not have.

That is a stronger position than OpenSSH's privilege separation, where the
privileged half still answers messages from the unprivileged one. Here nothing
crosses upward at all.

### The root list is root's

The one remaining channel is the list of roots, and it is a real one: it says
*"read this directory as root and write what is in it somewhere I can read"*.
A user who could edit it could ask for `/root` and read back every filename in
it. Filenames are not contents, but `/root/.ssh/id_ed25519_prod` is a map of a
machine.

So the file is **owned by root and not writable without `sudo`**. Adding a new
root costs a `sudo`; that is the price, and it is paid once per disk.

The threat model this settles: an attacker who can already obtain `sudo` is
root, and does not need us to read `/root` for them. We are not a tool for
that attacker. We are only a tool for the one who *cannot* get `sudo` — and
for them the channel is closed.

### What remains, and it is not the input side

Two things the closed input does not cover, both about the output:

* **The index is readable by whoever can read it.** Anything the scanner is
  pointed at becomes visible to every reader of that index. On a single-user
  desktop that is exactly what is wanted. On a shared machine, pointing it at
  `/home` opens every user's filenames to every other user. This is a
  deployment decision, and the file mode is where it is made.
* **Two writers cannot share one index directory.** The lock added in
  `db9c95c` allows exactly one, because segments are mmapped and a second
  writer truncating a mapped file is undefined behaviour. The scanner writes a
  **base layer**; the unprivileged daemon writes an **incremental layer** from
  its watcher; a query merges the two. The engine is already built for this —
  segments are exactly that shape — but it means two directories, two
  manifests, two locks.

### Measured: it is not a faster scanner, it is a journal

```bash
sudo /tmp/fsbulk/btrfs_bulk /home/hasan
```

| | time | items |
|---|---|---|
| **full tree** | **10,883 ms** | 11,113,801 items · 1,564,692 inodes · 1.35 GB read |
| `getdents64`, 20 threads, same subvolume | **345 ms** | — |
| **delta, `transid >= 7552`** (latest) | **0.3 ms** | 667 |
| delta, `transid >= 7502` (50 generations back) | 32.6 ms | 444,303 |
| delta, `transid >= 7452` (100 back) | 33.9 ms | 451,756 |

**As a scanner it is 31× slower than walking the tree.** That was not the
expectation and it is worth understanding: the metadata tree holds about
**seven items per inode** — extents, xattrs, directory indices — so reading it
whole means reading 1.35 GB to find 1.5 M files. `getdents64` reads only the
directory entries. Filtering by key type would cut the *transfer* but not the
traversal; the leaves still have to be read. There is no first-scan argument
for `TREE_SEARCH_V2` on btrfs, and this survey expected there to be one.

**As a journal it is extraordinary.** "What changed since the last commit"
answers in **0.3 ms**, against 10,883 ms for the full tree — four orders of
magnitude — and it stays cheap far back: a hundred generations of history costs
34 ms. This is exactly what `min_transid` promises, pruning at node-pointer
granularity without reading skipped subtrees, and it is what Everything's USN
journal actually buys. Not a fast first scan: **never needing another one.**

So the btrfs half of the privileged scanner inverts. It should not do the
scanning:

* **First scan** — the unprivileged daemon, `getdents64`, 345 ms, no service
  involved.
* **Every start after that, and every check while running** — the privileged
  service asks `min_transid` and writes only what moved. 0.3 ms where there is
  now a full rescan.

That also makes it a much smaller thing to build than a scanner, and it is the
first justification `Caps::JOURNAL` has ever had on Linux.

One number worth keeping for its own sake: the `INODE_REF` items carry the name
and the parent together — 1,743,173 of them against 1,564,692 inodes, the
difference being hard links. That pairing is what ext4 and XFS cannot give from
their inode tables, and it is why btrfs is the only real MFT equivalent here.
