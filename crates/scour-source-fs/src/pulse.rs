//! "Has anything happened here?", for microseconds — a trigger for reconciliation.
//!
//! btrfs answers with `BTRFS_IOC_GET_SUBVOL_INFO`'s `ctransid`: 8 µs a read, no
//! capability check. Anything else falls back to a write-sector counter, which its
//! neighbours move too — a pulse means "look", never "something of mine changed".

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

    // BTRFS_IOC_GET_SUBVOL_INFO — _IOR(0x94, 60, btrfs_ioctl_get_subvol_info_args),
    // a 504-byte struct with `ctransid` at offset 344.
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

    /// One subvolume's `ctransid`, for [`nested`]'s paths.
    pub fn subvolume(path: &Path) -> Option<u64> {
        ctransid(path)
    }

    /// The btrfs subvolumes mounted under a btrfs root, each with a `ctransid`
    /// of its own that the root's never moves for: `/var/log`, `/srv`, `/opt`
    /// on the layouts distributions install. Read once; see [`super::nested_in`].
    pub fn nested(root: &Path) -> Vec<std::path::PathBuf> {
        if fs_magic(root) != Some(BTRFS_MAGIC) {
            return Vec::new();
        }
        let table = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
        super::nested_in(&table, root)
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
        // **Not `!= 0`.** The ioctl answers with a non-negative number and answers
        // 1 here; the struct is filled either way.
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
    /// From `/proc/self/mountinfo`'s major:minor, named by `/sys/dev/block`. The
    /// longest matching mount point wins, so a bind mount does not answer for its parent.
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
    pub fn nested(_root: &Path) -> Vec<std::path::PathBuf> {
        Vec::new()
    }
    pub fn subvolume(_path: &Path) -> Option<u64> {
        None
    }
}

pub use linux::{Probe, nested, probe_for, read, subvolume};

/// The btrfs mount points strictly under `root` in a `mountinfo` table, in
/// the table's order. By path component: `/srv2` is not under `/srv`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn nested_in(mountinfo: &str, root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for line in mountinfo.lines() {
        let Some((head, tail)) = line.split_once(" - ") else {
            continue;
        };
        if tail.split_whitespace().next() != Some("btrfs") {
            continue;
        }
        let Some(point) = head.split_whitespace().nth(4) else {
            continue;
        };
        let point = std::path::PathBuf::from(unescape(point));
        if point != root && point.starts_with(root) && !out.contains(&point) {
            out.push(point);
        }
    }
    out
}

/// `mountinfo` writes a space, a tab, a newline and a backslash as octal.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && let Ok(n) = u8::from_str_radix(&field[i + 1..i + 4], 8)
        {
            out.push(n);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This machine's table, cut down: `/` is `@`, and four subvolumes hang
    /// under it; `/home` is another source's root and is taken out by the
    /// source, not here; NTFS is not btrfs.
    #[test]
    fn the_subvolumes_under_a_root_are_found_by_component() {
        let table = "\
60 1 0:35 /@ / rw,noatime - btrfs /dev/nvme1n1p5 rw\n\
61 60 0:36 /@home /home rw,noatime - btrfs /dev/nvme1n1p5 rw\n\
62 60 0:37 /@log /var/log rw,noatime - btrfs /dev/nvme1n1p5 rw\n\
63 60 0:38 /@srv /srv rw,noatime - btrfs /dev/nvme1n1p5 rw\n\
64 60 0:39 /@x /srv2 rw - btrfs /dev/nvme1n1p5 rw\n\
65 60 0:40 /@sp /var/with\\040space rw - btrfs /dev/nvme1n1p5 rw\n\
70 60 259:8 / /mnt/depo rw - ntfs3 /dev/nvme0n1p2 rw\n";
        let found = nested_in(table, std::path::Path::new("/"));
        let found: Vec<_> = found
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            found,
            ["/home", "/var/log", "/srv", "/srv2", "/var/with space"]
        );
        let under_srv = nested_in(table, std::path::Path::new("/srv"));
        assert!(under_srv.is_empty(), "{under_srv:?}");
    }
}
