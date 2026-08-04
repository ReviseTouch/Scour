//! Asking the filesystem what it can actually promise.
//!
//! [`Caps`] used to be a constant: every Unix build declared `STABLE_IDS |
//! CASE_SENSITIVE` at compile time, whatever it was pointed at. Both halves of
//! that are wrong on filesystems people really use.
//!
//! * **Identity.** `st_ino` on FAT and exFAT is invented by the driver, not
//!   stored on disk, and it can differ after a remount. Building an [`EntryId`]
//!   from it means a rescan can decide every file is new — the index doubles,
//!   the sweep removes the originals, and nothing says why. A path hash is
//!   worse at renames and honest about it.
//! * **Case.** This machine's NTFS volume answers `PROJELER` and refuses
//!   `projeler`, because Linux's ntfs3 is case-sensitive by default. The same
//!   disk under Windows is not. So the answer belongs to the mounted
//!   filesystem, not to the format and not to the operating system.
//!
//! Measured here rather than assumed: the magic numbers below were read off
//! this machine with `stat -f -c %t`, which is also how the ntfs3 value was
//! found — `coreutils` does not know it and prints `UNKNOWN`.
//!
//! [`EntryId`]: scour_core::EntryId

use std::path::Path;

/// How a mount behaves under a scan.
///
/// Separate from [`FsTraits`], which is about correctness. This is about
/// speed, and the two do not correlate: exFAT has no stable ids and is fast,
/// NFS has stable ids and is slow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Medium {
    /// A local device with deep hardware queues. Parallelism is free.
    Solid,
    /// A spinning disk. Every concurrent reader is a seek, so parallelism
    /// actively hurts.
    Spinning,
    /// Reached over a network — NFS, SMB, sshfs, a cloud mount. Bounded by
    /// round trips rather than by the device.
    Network,
    /// In memory. tmpfs, ramfs.
    Memory,
    /// Could not be determined.
    Unknown,
}

impl Medium {
    /// How many walker threads this mount is worth.
    ///
    /// Measured on this machine, `/home/hasan`, 1.85 M entries, two rounds:
    ///
    /// | threads | round 1 | round 2 |
    /// |---|---|---|
    /// | 8 | 1073 ms | 337 ms |
    /// | 16 | 284 ms | 306 ms |
    /// | **20** | **241 ms** | **277 ms** |
    /// | 32 | 250 ms | 282 ms |
    /// | 48 | 365 ms | 705 ms |
    ///
    /// Twenty is this machine's core count *and* its NVMe hardware queue
    /// count — the driver opens one queue per core, which is why the two
    /// agree. Going past it buys nothing and 48 costs dearly.
    ///
    /// The previous project's rule was `cores * 2`, on a note claiming 32 beat
    /// 20 by 20%. That did not reproduce here; the numbers above are why this
    /// says `cores`.
    ///
    /// The network and spinning figures are **not measured** — there is no HDD
    /// and no network mount on this machine. They are conservative guesses,
    /// and marked as such rather than presented as findings.
    pub fn threads(self, cores: usize) -> usize {
        match self {
            Medium::Solid | Medium::Memory => cores.clamp(2, 32),
            // One seek at a time. Concurrency on a spinning disk turns a
            // sequential read into a head-thrashing one.
            Medium::Spinning => 1,
            // Bounded by latency, so some concurrency hides round trips — but
            // too much floods a link that the local kernel cannot see.
            Medium::Network => 4,
            Medium::Unknown => (cores / 2).clamp(2, 8),
        }
    }

    /// How long to let filesystem events settle before acting on them.
    ///
    /// A network mount reports changes late and in bursts, and each reaction
    /// costs a round trip; batching harder is worth more there than promptness.
    pub fn debounce_ms(self) -> u64 {
        match self {
            Medium::Solid | Medium::Memory => 200,
            Medium::Spinning => 500,
            Medium::Network => 5_000,
            Medium::Unknown => 500,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Medium::Solid => "solid-state",
            Medium::Spinning => "spinning",
            Medium::Network => "network",
            Medium::Memory => "memory",
            Medium::Unknown => "unknown",
        }
    }
}

/// What one mounted filesystem promises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsTraits {
    /// `st_ino` is stored on disk and survives a remount, so it can be an
    /// identity. False on FAT-family filesystems and whenever unknown.
    pub stable_ids: bool,
    /// Two names differing only in case are two files.
    pub case_sensitive: bool,
}

impl FsTraits {
    /// What to assume when the filesystem cannot be identified.
    ///
    /// Not "no idea, allow everything". A wrongly claimed stable id costs a
    /// full re-index and a silent one; a wrongly withheld one costs renames
    /// being seen as delete-plus-add, which is visible and survivable. So the
    /// unknown case takes the cheaper mistake.
    pub const UNKNOWN: FsTraits = FsTraits {
        stable_ids: false,
        case_sensitive: cfg!(unix),
    };

    /// The narrower of two, for a source spanning more than one filesystem.
    pub fn and(self, other: FsTraits) -> FsTraits {
        FsTraits {
            stable_ids: self.stable_ids && other.stable_ids,
            case_sensitive: self.case_sensitive && other.case_sensitive,
        }
    }
}

/// The unknown case must never claim a stable identity.
///
/// A compile-time assertion rather than a test, because it is a statement
/// about a constant: claiming one that was never verified costs a silent full
/// re-index, and the default is the one thing here that could be changed
/// without anybody noticing.
const _: () = assert!(!FsTraits::UNKNOWN.stable_ids);

/// What the mount under `path` is like to read.
///
/// Three questions in order, because each is cheaper and more certain than the
/// next: is the filesystem itself a network or memory one (`statfs` says so
/// outright), does the kernel call its device rotational, and how many
/// hardware queues does that device have.
pub fn medium_of(path: &Path) -> Medium {
    #[cfg(target_os = "linux")]
    {
        linux::medium_of(path)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        Medium::Unknown
    }
}

/// What the filesystem under `path` promises.
///
/// One `statfs` per root at start-up, not per entry. A root that spans a
/// nested mount — `/` with an NTFS volume under it — is judged by the root
/// itself; per-device judgement is possible from `st_dev` and is not done
/// here, because a source whose roots straddle filesystems is the unusual
/// case and taking the narrower promise for all of it is safe.
pub fn traits_of(path: &Path) -> FsTraits {
    #[cfg(target_os = "linux")]
    {
        linux::traits_of(path)
    }
    #[cfg(not(target_os = "linux"))]
    {
        other::traits_of(path)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::FsTraits;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    // Read off this machine with `stat -f -c %t`, and cross-checked against
    // the kernel's `include/uapi/linux/magic.h` where it lists them.
    const EXT: i64 = 0xEF53; // ext2, ext3, ext4 — all three
    const BTRFS: i64 = 0x9123_683E;
    const XFS: i64 = 0x5846_5342;
    const F2FS: i64 = 0xF2F5_2010;
    const TMPFS: i64 = 0x0102_1994;
    const ZFS: i64 = 0x2FC1_2FC1;
    const OVERLAY: i64 = 0x794C_7630;
    // The FAT family, where `st_ino` is the driver's invention.
    const MSDOS: i64 = 0x4D44;
    const EXFAT: i64 = 0x2011_BAB0;
    // Two NTFS drivers with two different magics. The second is what ntfs3
    // reports and is "ntfs" as little-endian bytes; `coreutils` prints it as
    // UNKNOWN, which is how it came to be measured rather than looked up.
    const NTFS_3G: i64 = 0x5346_544E;
    const NTFS3: i64 = 0x7366_746E;
    // Reached over a network, whatever the device underneath turns out to be.
    // `fuse` covers sshfs, rclone and most cloud mounts; it also covers local
    // FUSE filesystems, and treating one of those as remote costs some
    // parallelism rather than correctness.
    const NFS: i64 = 0x6969;
    const SMB: i64 = 0x517B;
    const CIFS: i64 = 0xFF53_4D42;
    const SMB2: i64 = 0xFE53_4D42;
    const FUSE: i64 = 0x6573_5546;
    const NINEP: i64 = 0x0102_1997;
    const AFS: i64 = 0x5346_414F;
    const CEPH: i64 = 0x00C3_6400;
    const RAMFS: i64 = 0x8584_5846;

    pub fn traits_of(path: &Path) -> FsTraits {
        let Some(magic) = magic(path) else {
            return FsTraits::UNKNOWN;
        };
        match magic {
            EXT | BTRFS | XFS | F2FS | TMPFS | ZFS | OVERLAY => FsTraits {
                stable_ids: true,
                case_sensitive: true,
            },
            // Linux's NTFS drivers keep the on-disk MFT record number, so the
            // identity is real. Case is the driver's: ntfs3 is sensitive
            // unless mounted `nocase`, which cannot be seen from here — and
            // claiming sensitivity when the mount is insensitive only means a
            // duplicate is ruled out that would have been ruled out anyway.
            NTFS_3G | NTFS3 => FsTraits {
                stable_ids: true,
                case_sensitive: true,
            },
            MSDOS | EXFAT => FsTraits {
                stable_ids: false,
                case_sensitive: false,
            },
            _ => FsTraits::UNKNOWN,
        }
    }

    pub fn medium_of(path: &Path) -> super::Medium {
        use super::Medium;
        match magic(path) {
            Some(NFS | SMB | CIFS | SMB2 | FUSE | NINEP | AFS | CEPH) => return Medium::Network,
            Some(TMPFS | RAMFS) => return Medium::Memory,
            _ => {}
        }
        match rotational(path) {
            Some(true) => Medium::Spinning,
            Some(false) => Medium::Solid,
            None => Medium::Unknown,
        }
    }

    /// Does the kernel call the device under this path rotational?
    ///
    /// The chain is mount point → source device → `/sys/dev/block/MAJ:MIN`.
    /// Two things make it less obvious than it looks. A partition's sysfs
    /// entry has no `queue/`, so the parent disk's has to be read — hence the
    /// `..` fallback. And btrfs reports its source as `/dev/nvme0n1p5[/@home]`,
    /// with the subvolume in brackets, which is not a path that exists.
    fn rotational(path: &Path) -> Option<bool> {
        let out = std::process::Command::new("findmnt")
            .args(["-no", "SOURCE", "--target"])
            .arg(path)
            .output()
            .ok()?;
        let src = String::from_utf8_lossy(&out.stdout);
        let src = src.trim().split('[').next()?.trim();
        if !src.starts_with("/dev/") {
            return None;
        }
        let md = std::fs::metadata(src).ok()?;
        use std::os::unix::fs::MetadataExt;
        let (maj, min) = (libc::major(md.rdev()), libc::minor(md.rdev()));
        let base = format!("/sys/dev/block/{maj}:{min}");
        for p in [
            format!("{base}/queue/rotational"),
            format!("{base}/../queue/rotational"),
        ] {
            if let Ok(s) = std::fs::read_to_string(&p) {
                return Some(s.trim() == "1");
            }
        }
        None
    }

    /// `statfs(2)`'s `f_type`, or `None` if the path cannot be reached.
    fn magic(path: &Path) -> Option<i64> {
        let c = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: a zeroed `statfs` is a valid one to write into, and the
        // path is a NUL-terminated C string that outlives the call.
        let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statfs(c.as_ptr(), &mut buf) };
        if rc != 0 {
            return None;
        }
        Some(buf.f_type as i64)
    }
}

#[cfg(not(target_os = "linux"))]
mod other {
    use super::FsTraits;
    use std::path::Path;

    /// Everywhere else, for now, the conservative answer.
    ///
    /// Windows can do better — `GetVolumeInformationW` reports the filesystem
    /// name and `FILE_CASE_SENSITIVE_SEARCH` — and macOS's `statfs` carries
    /// `f_fstypename`. Neither is written yet, and claiming a promise that has
    /// not been checked is what this module exists to stop.
    pub fn traits_of(_path: &Path) -> FsTraits {
        FsTraits::UNKNOWN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spanning_two_filesystems_takes_the_narrower_promise() {
        let good = FsTraits {
            stable_ids: true,
            case_sensitive: true,
        };
        let fat = FsTraits {
            stable_ids: false,
            case_sensitive: false,
        };
        assert_eq!(good.and(fat), fat);
        assert_eq!(good.and(good), good);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_real_directory_is_identified() {
        // Whatever this machine's filesystem is, a temp directory is on one of
        // the recognised ones and must not fall through to UNKNOWN.
        let t = traits_of(&std::env::temp_dir());
        assert!(t.stable_ids, "{t:?}");
    }

    #[test]
    fn an_unreachable_path_does_not_claim_anything() {
        assert_eq!(
            traits_of(Path::new("/nonexistent-scour-test-path")),
            FsTraits::UNKNOWN
        );
    }
}
