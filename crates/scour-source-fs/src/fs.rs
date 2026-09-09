//! Asking the filesystem what it can actually promise.
//!
//! Case and mode belong to the mounted filesystem, not to the format and not to
//! the operating system: this machine's NTFS volume is case-sensitive under
//! ntfs3 and insensitive under Windows. The magics below came from `stat -f -c %t`.

use std::path::Path;

/// How a mount behaves under a scan.
/// Speed, not correctness: exFAT has no stable ids and is fast, NFS the reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Medium {
    /// A local device with deep hardware queues. Parallelism is free.
    Solid,
    /// A spinning disk. Every concurrent reader is a seek, so parallelism hurts.
    Spinning,
    /// Reached over a network. Bounded by round trips rather than by the device.
    Network,
    /// In memory. tmpfs, ramfs.
    Memory,
    /// Could not be determined.
    Unknown,
}

impl Medium {
    /// How many walker threads this mount is worth — capped by the one indexing
    /// consumer, not the device: `/mnt/depo`'s 1,565,781 entries cost 13.3 core-s at
    /// two and 176.3 at twenty. Network and spinning are unmeasured guesses.
    pub fn threads(self, cores: usize) -> usize {
        match self {
            Medium::Solid | Medium::Memory => 2,
            // One seek at a time; concurrency here is head-thrashing.
            Medium::Spinning => 1,
            // Some concurrency hides round trips; too much floods an unseen link.
            Medium::Network => 4,
            Medium::Unknown => (cores / 2).clamp(2, 8),
        }
    }

    /// How many threads are worth walking on when the consumer is **not** the limit.
    /// `/mnt/depo`'s 152,530 directories: 1.10 s on the scan's two threads, 0.33 s
    /// on eight, 0.22 s on sixteen — eight, so start-up does not take the desktop.
    pub fn walk_threads(self, cores: usize) -> usize {
        match self {
            Medium::Spinning => 1,
            // Bounded by round trips rather than by the device.
            Medium::Network => 4,
            Medium::Solid | Medium::Memory | Medium::Unknown => (cores / 2).clamp(2, 8),
        }
    }

    /// How long to let filesystem events settle before acting on them.
    /// A network mount reports late and in bursts, and each reaction is a round trip.
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
    /// Withheld wherever a mount supplies the mode: `/mnt/depo` is mounted
    /// `fmask=0022`, and 293,811 files there classified `exec` against 21,342 beside it.
    pub real_modes: bool,
}

impl FsTraits {
    /// What to assume when the filesystem cannot be identified.
    /// Not "no idea, allow everything": every field takes the cheaper mistake.
    pub const UNKNOWN: FsTraits = FsTraits {
        case_sensitive: cfg!(unix),
        // Claiming it wrongly fills `kind:exec` with every file on the volume.
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

/// What the mount under `path` is like to read: whether `statfs` calls the
/// filesystem a network or memory one, then whether the kernel calls its device
/// rotational.
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

/// What the filesystem under `path` promises: one `statfs` per root at start-up,
/// not per entry. A root spanning a nested mount is judged by the root itself,
/// because the narrower promise is the safe one.
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

    // Read with `stat -f -c %t`, cross-checked against `linux/magic.h`.
    const EXT: i64 = 0xEF53; // ext2, ext3, ext4 — all three
    const BTRFS: i64 = 0x9123_683E;
    const XFS: i64 = 0x5846_5342;
    const F2FS: i64 = 0xF2F5_2010;
    const TMPFS: i64 = 0x0102_1994;
    const ZFS: i64 = 0x2FC1_2FC1;
    const OVERLAY: i64 = 0x794C_7630;
    const MSDOS: i64 = 0x4D44;
    const EXFAT: i64 = 0x2011_BAB0;
    // Two NTFS drivers, two magics; the second is `ntfs` as little-endian bytes.
    const NTFS_3G: i64 = 0x5346_544E;
    const NTFS3: i64 = 0x7366_746E;
    // Reached over a network. A local FUSE mount read as remote costs parallelism.
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
            // ntfs3 is case-sensitive unless mounted `nocase`, invisible from here.
            NTFS_3G | NTFS3 => FsTraits {
                case_sensitive: true,
                // `stat` returns `fmask`/`dmask`, the same value for every file.
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
    /// A partition's sysfs entry has no `queue/`, hence the `..` fallback; btrfs names
    /// its source `/dev/nvme0n1p5[/@home]`, which is not a path that exists.
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
        // SAFETY: a zeroed `statfs` is valid to write into, and `c` outlives the call.
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

    // Written out rather than imported: `windows-sys` moves them across versions.
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const FILE_CASE_SENSITIVE_SEARCH: u32 = 0x0000_0001;

    /// The volume root a path lives on: `C:\` for `C:\Users\x`.
    /// Windows keys everything it can say about a filesystem to the volume.
    fn volume_root(path: &Path) -> Option<Vec<u16>> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut root = vec![0u16; 260];
        // SAFETY: both buffers are valid for the lengths passed; the input is NUL-terminated.
        let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
        (ok != 0).then_some(root)
    }

    pub fn traits_of(path: &Path) -> FsTraits {
        let Some(root) = volume_root(path) else {
            return FsTraits::UNKNOWN;
        };
        let mut name = [0u16; 64];
        let mut flags: u32 = 0;
        // SAFETY: null is accepted for unwanted outputs; both buffers are valid.
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
        // Read and not used: the same call fills `flags`, and debugging wants it.
        let _fs = String::from_utf16_lossy(&name[..name.iter().position(|&c| c == 0).unwrap_or(0)]);

        // Off by default even on NTFS — the opposite of the same disk under ntfs3.
        let case_sensitive = flags & FILE_CASE_SENSITIVE_SEARCH != 0;

        FsTraits {
            case_sensitive,
            // Windows has ACLs, not a mode bit; `kind_of` classifies by extension.
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
            // Usually flash, and assuming spinning would slow every USB stick.
            DRIVE_REMOVABLE | DRIVE_FIXED => Medium::Solid,
            _ => Medium::Unknown,
        }
        // Telling NVMe from SATA needs the raw volume handle: privileged, untestable here.
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{FsTraits, Medium};
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// `statfs`'s own name for the filesystem: "apfs", "hfs", "exfat", "msdos",
    /// "nfs", "smbfs", "webdav".
    fn fstype(path: &Path) -> Option<(String, u32)> {
        let c = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: a zeroed `statfs` is valid to write into, and `c` outlives the call.
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
            // Either can be formatted case-sensitive and `statfs` does not say.
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
        // `MNT_LOCAL` is off for anything reached over a network, whatever it calls itself.
        if flags & libc::MNT_LOCAL as u32 == 0 {
            return Medium::Network;
        }
        match fs.as_str() {
            "nfs" | "smbfs" | "afpfs" | "webdav" | "ftp" => Medium::Network,
            // APFS is not offered on rotational media; an external HFS+ disk is missed.
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
        // A temp directory is on a recognised filesystem, whatever this machine runs.
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
