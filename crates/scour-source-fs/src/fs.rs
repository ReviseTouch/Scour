//! Asking the filesystem what it can actually promise.
//!
//! [`Caps`] used to be a constant: every Unix build declared `STABLE_IDS |
//! CASE_SENSITIVE` at compile time, whatever it was pointed at. Both halves of
//! that were wrong on filesystems people really use.
//!
//! * **Identity** is no longer asked about at all — a row is identified by its
//!   path, so nothing here has to decide whether `st_ino` can be trusted. The
//!   measurement that made the question interesting is kept in
//!   `examples/filesystems.rs`: on vfat and exfat `st_ino` is invented by the
//!   driver and 0 of 50 files kept theirs across a remount.
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
    /// **This number belonged to the device and it does not.** It was twenty
    /// here — the core count, and this machine's NVMe hardware queue count,
    /// which is the same number and looked like a reason. What that measured
    /// was the *walk*: 1.85 M entries in 241 ms at twenty threads against
    /// 1073 ms at eight.
    ///
    /// A scan is not a walk. Entries go down a channel to one thread that
    /// stages and indexes them, and it takes them slower than one walker
    /// produces them. So the channel fills, a walker blocks in `send` while
    /// still owing a directory, and `ignore`'s other workers spin in their
    /// wait-for-work loop until it comes back — at a full core each, starving
    /// the one thread that would have released them. The walk got faster and
    /// the scan got worse.
    ///
    /// First scan of `/mnt/depo`, 1,565,781 entries on ntfs3, whole service,
    /// both directions. The page cache is the variable that matters, so both
    /// states are here — cold is the first scan after a boot, warm is every
    /// restart during a session:
    ///
    /// | threads | warm | cold |
    /// |---|---|---|
    /// | 1 | 12.1 core-s · 8.4 s | — |
    /// | **2** | **13.3 core-s · 5.1 s** | 20.1 core-s · 17-24 s |
    /// | 4 | 31.0 core-s · 8.2 s | 27.2 core-s · **11-14 s** |
    /// | 8 | 70.3 core-s · 10.9 s | — |
    /// | 20 | 176.3 core-s · 13.2 s | — |
    ///
    /// Warm, two wins on both axes and twenty is the worst row on both — it
    /// measured 8,231,481 voluntary context switches against 24,007 at two.
    /// Cold, four is half the wall clock, because the walker is waiting on the
    /// device rather than on anything here.
    ///
    /// **Two.** It is the lower CPU in *both* states — a third less cold, less
    /// than half warm — and the only thing four buys is about six seconds of a
    /// scan that happens once a boot, against a saving on every restart during
    /// the session.
    ///
    /// The number is a symptom and worth naming as one. The walk alone costs
    /// 3.0 core-seconds and scales to four threads perfectly, 0.85 s wall with
    /// no spinning at all; the whole scan costs 13.3. The other ten are the
    /// index, and the wait it imposes: one consumer cannot take rows as fast as
    /// one walker produces them, so the channel fills, a walker blocks holding
    /// a directory it owes, and `ignore`'s other workers spin waiting for it.
    /// A faster consumer would make this number four again — see
    /// `scour-source-fs/examples/walkcost.rs`, which is how it was split.
    ///
    /// The network and spinning figures are **not measured** — there is no HDD
    /// and no network mount on this machine. They are conservative guesses,
    /// and marked as such rather than presented as findings.
    pub fn threads(self, cores: usize) -> usize {
        match self {
            Medium::Solid | Medium::Memory => 2,
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
    /// Two names differing only in case are two files.
    pub case_sensitive: bool,
    /// The executable bit means something here.
    ///
    /// Not quite "the filesystem has modes" — measured, `ntfs3` does store one
    /// for a file created under Linux, and a `chmod 600` on it sticks. What it
    /// cannot do is invent one for the files Windows wrote, and those take
    /// `fmask` off the mount line instead: `/mnt/depo` is mounted `fmask=0022`,
    /// so **everything Windows put there reads 0755**.
    ///
    /// Which makes the bit noise for almost every file on such a volume, and
    /// `kind_of` classifies on it: 293,811 files under `/mnt/depo` came back
    /// `exec` against 21,342 under the real filesystem beside it. Not a
    /// rounding error — a whole volume in the wrong category, and `kind:exec`
    /// useless for finding a program.
    ///
    /// So it is withheld wherever a mount supplies the mode for anything, and
    /// the cost is a genuinely `chmod +x` script on such a volume not being
    /// called executable. That is the cheaper mistake by five orders.
    pub real_modes: bool,
}

impl FsTraits {
    /// What to assume when the filesystem cannot be identified.
    ///
    /// Not "no idea, allow everything": every field here takes the cheaper
    /// mistake.
    pub const UNKNOWN: FsTraits = FsTraits {
        case_sensitive: cfg!(unix),
        // The cheaper mistake again. Withholding it loses `kind:exec` on a
        // filesystem nobody recognised; claiming it wrongly fills the category
        // with every file on the volume.
        real_modes: false,
    };

    /// The narrower of two, for a source spanning more than one filesystem.
    pub fn and(self, other: FsTraits) -> FsTraits {
        FsTraits {
            case_sensitive: self.case_sensitive && other.case_sensitive,
            real_modes: self.real_modes && other.real_modes,
        }
    }
}

/// What the mount under `path` is like to read.
///
/// Three questions in order, because each is cheaper and more certain than the
/// next: is the filesystem itself a network or memory one (`statfs` says so
/// outright), does the kernel call its device rotational, and how many
/// hardware queues does that device have.
pub fn medium_of(path: &Path) -> Medium {
    #[cfg(target_os = "linux")]
    return linux::medium_of(path);
    #[cfg(windows)]
    return windows_impl::medium_of(path);
    #[cfg(target_os = "macos")]
    return macos::medium_of(path);
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    return fallback::medium_of(path);
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
    return linux::traits_of(path);
    #[cfg(windows)]
    return windows_impl::traits_of(path);
    #[cfg(target_os = "macos")]
    return macos::traits_of(path);
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    return fallback::traits_of(path);
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
                case_sensitive: true,
                real_modes: true,
            },
            // Linux's NTFS drivers keep the on-disk MFT record number, so the
            // identity is real. Case is the driver's: ntfs3 is sensitive
            // unless mounted `nocase`, which cannot be seen from here — and
            // claiming sensitivity when the mount is insensitive only means a
            // duplicate is ruled out that would have been ruled out anyway.
            NTFS_3G | NTFS3 => FsTraits {
                case_sensitive: true,
                // NTFS has an access-control model and neither Linux driver
                // maps it onto `st_mode`; what `stat` returns is `fmask` and
                // `dmask` from the mount line, the same value for every file.
                real_modes: false,
            },
            MSDOS | EXFAT => FsTraits {
                case_sensitive: false,
                real_modes: false,
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

#[cfg(windows)]
mod windows_impl {
    use super::{FsTraits, Medium};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW,
    };

    // Win32's own values, written out rather than imported: `windows-sys`
    // moves them between modules across versions, and these have not changed
    // since Windows 95 and will not.
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const FILE_CASE_SENSITIVE_SEARCH: u32 = 0x0000_0001;

    /// The volume root a path lives on: `C:\` for `C:\Users\x`.
    ///
    /// Everything Windows can say about a filesystem is keyed to the volume,
    /// not to the path, and `GetVolumePathNameW` is the supported way to get
    /// from one to the other — including for a path on a mounted volume with
    /// no drive letter of its own.
    fn volume_root(path: &Path) -> Option<Vec<u16>> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut root = vec![0u16; 260];
        // SAFETY: both buffers are valid for the lengths passed, and the input
        // is NUL-terminated.
        let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
        (ok != 0).then_some(root)
    }

    pub fn traits_of(path: &Path) -> FsTraits {
        let Some(root) = volume_root(path) else {
            return FsTraits::UNKNOWN;
        };
        let mut name = [0u16; 64];
        let mut flags: u32 = 0;
        // SAFETY: null is accepted for every output not wanted; the two
        // buffers passed are valid for the lengths given.
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut flags,
                name.as_mut_ptr(),
                name.len() as u32,
            )
        };
        if ok == 0 {
            return FsTraits::UNKNOWN;
        }
        // The filesystem's name — NTFS, ReFS, FAT32 — is read and not used:
        // nothing here is decided by which format it is any more. Kept because
        // the call that fills it is the same one that fills `flags`, and
        // because it is the first thing anybody debugging this will want.
        let _fs = String::from_utf16_lossy(&name[..name.iter().position(|&c| c == 0).unwrap_or(0)]);

        // Windows reports case sensitivity per volume, and it is off by
        // default even on NTFS — the opposite of the same disk under Linux's
        // ntfs3, which is where this ceased to be a property of the format.
        let case_sensitive = flags & FILE_CASE_SENSITIVE_SEARCH != 0;

        FsTraits {
            case_sensitive,
            // Windows has ACLs, not a mode bit. Nothing here can report an
            // executable bit because there is not one to report; `kind_of`
            // classifies by extension there, which is what Explorer does.
            real_modes: false,
        }
    }

    pub fn medium_of(path: &Path) -> Medium {
        let Some(root) = volume_root(path) else {
            return Medium::Unknown;
        };
        // SAFETY: `root` is a NUL-terminated wide string from Windows itself.
        match unsafe { GetDriveTypeW(root.as_ptr()) } {
            DRIVE_REMOTE => Medium::Network,
            // A removable volume is usually flash, and treating it as solid
            // costs nothing if it is not: the alternative reading is "spinning",
            // and a removable spinning disk is rare enough that assuming it
            // would slow down every USB stick.
            DRIVE_REMOVABLE | DRIVE_FIXED => Medium::Solid,
            _ => Medium::Unknown,
        }
        // Telling NVMe from SATA needs `IOCTL_STORAGE_QUERY_PROPERTY` with
        // `StorageAdapterProperty`, and telling a spinning disk from an SSD
        // needs `DEVICE_SEEK_PENALTY_DESCRIPTOR`. Both open the raw volume
        // handle, which is a privileged operation on some systems and a
        // measurable cost on all of them — and neither can be tested from
        // here. Left until there is a Windows machine to measure on.
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{FsTraits, Medium};
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// `statfs`'s own name for the filesystem: "apfs", "hfs", "exfat",
    /// "msdos", "nfs", "smbfs", "webdav".
    ///
    /// A string rather than a magic number, which is the one place macOS is
    /// easier than Linux here.
    fn fstype(path: &Path) -> Option<(String, u32)> {
        let c = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: a zeroed `statfs` is valid to write into and the path is a
        // NUL-terminated C string that outlives the call.
        let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut buf) } != 0 {
            return None;
        }
        let name: Vec<u8> = buf
            .f_fstypename
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        Some((String::from_utf8_lossy(&name).into_owned(), buf.f_flags))
    }

    pub fn traits_of(path: &Path) -> FsTraits {
        let Some((fs, _)) = fstype(path) else {
            return FsTraits::UNKNOWN;
        };
        match fs.as_str() {
            // APFS and HFS+ are case-insensitive as shipped and can be
            // formatted case-sensitive, and nothing in `statfs` says which.
            // The safe reading is insensitive: claiming sensitivity that is
            // not there would let two spellings of one file both be indexed.
            "apfs" | "hfs" => FsTraits {
                case_sensitive: false,
                real_modes: true,
            },
            "msdos" | "exfat" => FsTraits {
                case_sensitive: false,
                real_modes: false,
            },
            _ => FsTraits::UNKNOWN,
        }
    }

    pub fn medium_of(path: &Path) -> Medium {
        let Some((fs, flags)) = fstype(path) else {
            return Medium::Unknown;
        };
        // `MNT_LOCAL` is off for anything reached over a network, whatever it
        // calls itself — which covers the mounts a name check would miss.
        if flags & libc::MNT_LOCAL as u32 == 0 {
            return Medium::Network;
        }
        match fs.as_str() {
            "nfs" | "smbfs" | "afpfs" | "webdav" | "ftp" => Medium::Network,
            // Every Mac since 2016 boots from NVMe, Intel and Apple Silicon
            // alike, and APFS is not offered on rotational media. An external
            // spinning disk formatted HFS+ is the case this gets wrong, and
            // IOKit is where the real answer lives.
            "apfs" => Medium::Solid,
            _ => Medium::Unknown,
        }
    }
}

/// Platforms with no implementation yet.
#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod fallback {
    use super::{FsTraits, Medium};
    use std::path::Path;

    pub fn traits_of(_path: &Path) -> FsTraits {
        FsTraits::UNKNOWN
    }
    pub fn medium_of(_path: &Path) -> Medium {
        Medium::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spanning_two_filesystems_takes_the_narrower_promise() {
        let good = FsTraits {
            case_sensitive: true,
            real_modes: true,
        };
        let fat = FsTraits {
            case_sensitive: false,
            real_modes: false,
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
        assert!(t.case_sensitive, "{t:?}");
    }

    #[test]
    fn an_unreachable_path_does_not_claim_anything() {
        assert_eq!(
            traits_of(Path::new("/nonexistent-scour-test-path")),
            FsTraits::UNKNOWN
        );
    }
}
