//! "Has anything happened here?", for microseconds.
//!
//! A watcher on Linux costs one inotify watch per directory out of a budget
//! shared with every other program the user runs, and a volume that changes
//! four times a day does not earn 152,529 of them. What replaces watching is
//! reconciliation — but reconciliation on a timer is work done mostly for
//! nothing, so it wants a trigger.
//!
//! This is the trigger. Two of them, chosen by what is under the root:
//!
//! * **btrfs** answers exactly. `BTRFS_IOC_GET_SUBVOL_INFO` reports the
//!   subvolume's `ctransid` — the transaction it last changed in — and it has
//!   no capability check, unlike the `TREE_SEARCH` that `btrfs subvolume show`
//!   reaches for and fails on. Measured: still across three idle seconds,
//!   moved on a write, a rename and a delete, **8 µs a read**.
//!
//! * **Anything else** falls back to the block layer: the write-sector counter
//!   for the partition behind the root, out of `/proc/diskstats`. Measured on
//!   the NTFS volume: zero drift over three idle seconds, and it moves for a
//!   rename, which `statvfs`'s free-block count sleeps through.
//!
//! Neither says *what* changed, and that is the point — they cost nothing and
//! the expensive question is only asked when the answer can differ.
//!
//! **The block counter is only as good as the partition is private.** `/home`
//! shares `nvme0n1p5` with `/var/log`, `/var/cache` and `/srv`, so its write
//! sectors moved fourteen times in fifteen idle seconds — noise, not signal.
//! That is why btrfs is asked first and why a caller must treat a moving pulse
//! as "look", never as "something of mine changed".

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::os::fd::AsRawFd;
    use std::path::Path;

    /// Where a pulse comes from for one root, decided once.
    #[derive(Debug, Clone)]
    pub enum Probe {
        /// The subvolume's last-changed transaction id.
        Btrfs,
        /// Write sectors for a block device, as named in `/proc/diskstats`.
        Blocks(String),
        /// Nothing cheap to ask.
        None,
    }

    // BTRFS_IOC_GET_SUBVOL_INFO — _IOR(0x94, 60, struct
    // btrfs_ioctl_get_subvol_info_args). The struct is 504 bytes and
    // `ctransid` sits at offset 344; nothing else in it is wanted here.
    const BTRFS_GET_SUBVOL_INFO: libc::c_ulong = (2 << 30) | (504 << 16) | (0x94 << 8) | 60;
    const CTRANSID_AT: usize = 344;
    const BTRFS_MAGIC: i64 = 0x9123_683E;

    pub fn probe_for(root: &Path) -> Probe {
        if fs_magic(root) == Some(BTRFS_MAGIC) {
            return Probe::Btrfs;
        }
        match device_of(root) {
            Some(dev) => Probe::Blocks(dev),
            None => Probe::None,
        }
    }

    pub fn read(root: &Path, probe: &Probe) -> Option<u64> {
        match probe {
            Probe::Btrfs => ctransid(root),
            Probe::Blocks(dev) => write_sectors(dev),
            Probe::None => None,
        }
    }

    fn fs_magic(root: &Path) -> Option<i64> {
        let path = std::ffi::CString::new(root.as_os_str().as_encoded_bytes()).ok()?;
        let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(path.as_ptr(), &mut buf) } != 0 {
            return None;
        }
        Some(buf.f_type as i64)
    }

    fn ctransid(root: &Path) -> Option<u64> {
        let file = fs::File::open(root).ok()?;
        let mut buf = [0u8; 504];
        // SAFETY: the buffer is exactly the size the ioctl's encoding names,
        // and the kernel only writes into it.
        let rc = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                BTRFS_GET_SUBVOL_INFO,
                buf.as_mut_ptr() as *mut libc::c_void,
            )
        };
        // **Not `!= 0`.** This ioctl answers with a *non-negative* number, and
        // on this machine it answers 1 — the search that filled the struct
        // found its key past an exact match and the kernel passes that back.
        // Reading it as failure cost an afternoon: the struct was filled every
        // time, and the error printed alongside was a stale `errno` from
        // something else entirely.
        if rc < 0 {
            if std::env::var_os("SCOUR_PULSE_TRACE").is_some() {
                eprintln!(
                    "pulse: GET_SUBVOL_INFO on {} failed: {}",
                    root.display(),
                    std::io::Error::last_os_error()
                );
            }
            return None;
        }
        Some(u64::from_le_bytes(
            buf[CTRANSID_AT..CTRANSID_AT + 8].try_into().ok()?,
        ))
    }

    /// The device name `/proc/diskstats` uses for whatever this path is on.
    ///
    /// From `/proc/self/mountinfo`, which gives the major:minor of the mount
    /// the path belongs to; `/sys/dev/block/<major>:<minor>` names it. The
    /// longest matching mount point wins, so a bind mount inside another does
    /// not answer for its parent.
    fn device_of(root: &Path) -> Option<String> {
        let root = root.canonicalize().ok()?;
        let mounts = fs::read_to_string("/proc/self/mountinfo").ok()?;
        let mut best: Option<(usize, String)> = None;
        for line in mounts.lines() {
            let mut it = line.split_whitespace();
            let dev = it.nth(2)?; // major:minor
            let point = it.nth(1)?; // mount point
            if !root.starts_with(point) {
                continue;
            }
            if best.as_ref().is_none_or(|(n, _)| point.len() > *n) {
                best = Some((point.len(), dev.to_string()));
            }
        }
        let (_, dev) = best?;
        let name = fs::read_link(format!("/sys/dev/block/{dev}")).ok()?;
        Some(name.file_name()?.to_string_lossy().into_owned())
    }

    fn write_sectors(dev: &str) -> Option<u64> {
        let stats = fs::read_to_string("/proc/diskstats").ok()?;
        for line in stats.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() > 9 && f[2] == dev {
                return f[9].parse().ok();
            }
        }
        None
    }
}

#[cfg(not(target_os = "linux"))]
mod linux {
    use std::path::Path;

    #[derive(Debug, Clone)]
    pub enum Probe {
        None,
    }
    pub fn probe_for(_root: &Path) -> Probe {
        Probe::None
    }
    pub fn read(_root: &Path, _probe: &Probe) -> Option<u64> {
        None
    }
}

pub use linux::{Probe, probe_for, read};
