//! Which filesystems the handed-over `fanotify` group can hear.
//!
//! The helper marks the disks there are when it runs, then gives up the right
//! to mark more. A disk plugged in or chosen later, and anything on a network,
//! has no mark: its events never come, so a source there is not watched, and
//! is walked as one that nothing watches is.

use std::collections::HashSet;
use std::path::Path;

/// `major:minor` of every filesystem the group has a mark on, from the
/// lines `fdinfo` gives a fanotify descriptor: `fanotify sdev:10300008 …`,
/// the kernel's own encoding, twelve bits of major above twenty of minor.
pub fn marked(fdinfo: &str) -> HashSet<(u32, u32)> {
    fdinfo
        .lines()
        .filter_map(|l| l.strip_prefix("fanotify sdev:")?.split_whitespace().next())
        .filter_map(|hex| u32::from_str_radix(hex, 16).ok())
        .map(|dev| (dev >> 20, dev & 0xf_ffff))
        .collect()
}

/// The filesystem a path is on, by the longest mount point holding it.
pub fn device_of(mountinfo: &str, path: &Path) -> Option<(u32, u32)> {
    let mut best: Option<(usize, (u32, u32))> = None;
    for line in mountinfo.lines() {
        let mut it = line.split_whitespace();
        let (Some(dev), Some(point)) = (it.nth(2), it.nth(1)) else {
            continue;
        };
        let point = point.replace("\\040", " ");
        if !path.starts_with(&point) || best.is_some_and(|(n, _)| point.len() <= n) {
            continue;
        }
        let Some((major, minor)) = dev.split_once(':') else {
            continue;
        };
        if let (Ok(major), Ok(minor)) = (major.parse(), minor.parse()) {
            best = Some((point.len(), (major, minor)));
        }
    }
    best.map(|(_, dev)| dev)
}

/// Whether every one of these roots is on a filesystem the group behind
/// `raw` can hear. Read now, not once: what is mounted changes.
pub fn covered(raw: std::os::fd::RawFd, roots: &[std::path::PathBuf]) -> bool {
    let fdinfo = std::fs::read_to_string(format!("/proc/self/fdinfo/{raw}")).unwrap_or_default();
    let table = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let marks = marked(&fdinfo);
    roots.iter().all(|r| {
        let real = r.canonicalize().unwrap_or_else(|_| r.clone());
        device_of(&table, &real).is_some_and(|d| marks.contains(&d))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This machine's, on the day: `/` on btrfs, through its superblock's
    /// device `0:35`, and an NTFS disk on `259:8`.
    #[test]
    fn a_root_is_covered_when_its_filesystem_has_a_mark() {
        let fdinfo = "pos:\t0\nfanotify flags:1e12 event-flags:8000\n\
                      fanotify sdev:10300008 mflags:0 mask:48000fce ignored_mask:0\n\
                      fanotify sdev:23 mflags:0 mask:48000fce ignored_mask:0\n";
        let marks = marked(fdinfo);
        assert_eq!(marks, HashSet::from([(259, 8), (0, 35)]));

        let table = "60 1 0:35 /@ / rw - btrfs /dev/nvme0n1p5 rw\n\
                     61 60 0:35 /@home /home rw - btrfs /dev/nvme0n1p5 rw\n\
                     70 60 259:8 / /mnt/depo rw - ntfs3 /dev/nvme1n1p2 rw\n\
                     80 60 8:17 / /run/media/u/My\\040Stick rw - vfat /dev/sdb1 rw\n";
        let on = |p: &str| device_of(table, Path::new(p));
        assert_eq!(on("/home/u/x"), Some((0, 35)));
        assert_eq!(on("/mnt/depo/a"), Some((259, 8)));
        assert_eq!(on("/run/media/u/My Stick"), Some((8, 17)));
        assert!(
            !marks.contains(&on("/run/media/u/My Stick").unwrap()),
            "plugged in later"
        );
    }
}
