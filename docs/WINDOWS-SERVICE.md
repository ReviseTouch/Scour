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

Until a step is tried on a real Windows machine it is written down as untried.
