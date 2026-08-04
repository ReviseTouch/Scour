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
is not readable without being in group `disk`.

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
| **btrfs** | `TREE_SEARCH_V2` | **CAP_SYS_ADMIN** | everything, plus `min_transid` deltas |
| **XFS** | `XFS_IOC_BULKSTAT` | **CAP_SYS_ADMIN** | metadata but **no names** |
| ext4, F2FS | none exists | — | `getdents64` is the floor |
| exFAT, FAT32 | none possible | — | directory entries *are* the metadata |
| **ZFS** | `ZFS_IOC_NEXT_OBJ` | none | but paths need `OBJ_TO_STATS` → root |
| **APFS, HFS+** | `getattrlistbulk` | **none** | full `struct stat` per entry |
| ReFS | none | — | `GetFileInformationByHandleEx` only |
| **Windows, any fs** | `GetFileInformationByHandleEx` | **none** | name, size, allocation, 4 times |

### btrfs deserves a paragraph because it is the best design here and unusable

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

**Not worth it:** anything requiring root. btrfs `TREE_SEARCH_V2` is the best
interface in this survey and is `CAP_SYS_ADMIN`; XFS bulkstat is `CAP_SYS_ADMIN`
*and* returns no names; Windows `$MFT` and `FSCTL_ENUM_USN_DATA` need
Administrator — which is exactly why Everything ships a service, and why this
does not.
