# Windows: two ways to run, and the move between them

Decided with Hasan on 17 November 2026. Settings always stay with the person.

| | Without administrator (default) | With administrator |
|---|---|---|
| Programs | `%LOCALAPPDATA%\Programs\Scour` | `C:\Program Files\Scour` |
| Service | `scourd.exe`, a process in the user's session | the `Scour` Windows service, started with Windows |
| First walk | directory by directory | the NTFS master file table, read whole |
| Live changes | `ReadDirectoryChangesW` | the NTFS change journal (USN) |
| Index | `%APPDATA%\scour\data\index` | `C:\ProgramData\Scour\index` |
| Settings | `%APPDATA%\scour\data\state` | the same — the person's, not the service's |

Installing never asks for anything. **Settings → Disks → Run as a Windows service**
asks once (UAC) and moves the installation; **Run in my session** moves it back.

## Why Program Files

A service runs as `LocalSystem`. Its executable must not be writable by the
person: anything that can replace `scourd.exe` in `%LOCALAPPDATA%` would then run
as SYSTEM at the next start. `C:\Program Files` is writable by administrators
only, and that is the whole reason for the move.

## Who may talk to it

The service listens on the same pipe name the session process uses,
`\\.\pipe\scour-<user>`, so no face needs to know which mode it is in. The pipe
carries an explicit DACL: SYSTEM, Administrators, and the one user who switched
the mode on, read and write. Nobody else — a second account on the same machine
would otherwise read every file name of every disk.

## Steps, each committed and tried on PC2

1. **The service itself.** `scourd --service --owner <SID> --state <dir>` under
   the Service Control Manager: `StartServiceCtrlDispatcherW`, stop on
   `SERVICE_CONTROL_STOP`/`SHUTDOWN`. The pipe gets the DACL above. The index in
   `ProgramData`, the settings read from and written to the owner's `--state`.
   Through `windows-sys` (already a dependency): no new crate, so PC2's vendored
   sources still build.
2. **The move.** `scour-gui --elevate` relaunched with `runas`: copy to Program
   Files, `CreateServiceW` (automatic start, `LocalSystem`), stop and remove the
   session install and its Run value, start the service. `--unelevate` the
   reverse. The faces, finding the pipe, do not start a session `scourd` when the
   service is registered.
3. **The change journal.** For NTFS volumes, when running as the service:
   `FSCTL_QUERY_USN_JOURNAL` + `FSCTL_READ_USN_JOURNAL` from the last USN kept
   per volume, mapped to the same changes the watcher sends. Changes made while
   the service was stopped are read at start instead of walked.
4. **The master file table.** `FSCTL_ENUM_USN_DATA` for the first walk of an NTFS
   volume: names and parent references for every record, joined into paths;
   sizes and times from the records where the enumeration lacks them. Everything
   else stays a directory walk.
5. **An installer and updates.** An MSI that stops the service, replaces the files
   and starts it again; the window offers a newer release from GitHub and runs it
   after checking its SHA-256.

## Where each step stands (17 November)

| Step | State |
|---|---|
| 1. The service | tried on PC2: running, automatic start, pipe DACL holds |
| 2. The move | tried on PC2; the tidy-up of the old session install not yet |
| 3. The change journal | written: `scour-source-fs/src/usn.rs`, its position kept as `journal-<source>` beside the index, 45 s behind what was read; a restart that finds it still in the journal skips the start walk (`WatchHandle::caught_up`). Tried on PC2: all seven volumes read on from the last run |
| 4. The master file table | written: `scour-source-fs/src/mft.rs`, a raw read of `$MFT` (not `FSCTL_ENUM_USN_DATA`, which has no sizes or times), for every whole walk of a drive root the process may open. Tried on PC2: 2,395,699 entries on two SSDs and a spinning disk in 23 s, the spinning disk's 1,380,256 in 21.4 s |
| 5. Installer and updates | not started |

Also for the service: a network folder is walked as its owner
(`scourd/src/as_owner.rs`: the walker threads borrow the token of the owner's
logged-on session), and not watched, every ten minutes at most. Tried on
PC2: 2,808 entries in 2.4 s. Walked in order on that one thread — the
parallel walker's threads read a directory before calling the visitor, so
each one's first read was as SYSTEM.

Whole walks of every source (the start, "walk everything") now run one
lane per physical disk, side by side, in one index generation swept once
(`Index::sweep_many`): the disk of a volume from
`IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`, a spinning one from its seek penalty.
Tried on PC2: four lanes at once (two SSDs, the spinning disk, the share).
On Linux the disk comes from `findmnt` and `/sys/class/block`, checked here.

Until a step is tried on a real Windows machine it is written down as untried.
