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
