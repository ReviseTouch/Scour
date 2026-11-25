//! Which disk a path is on, and how many readers that disk is worth.

use std::path::{Path, PathBuf};

use surefoot::Medium;

/// The roots files are under and the disk each stands on: asked of
/// `surefoot` once a root, never once a file.
#[derive(Debug, Clone, Default)]
pub struct Ground {
    /// Longest root first, so a mount inside another is found before it.
    roots: Vec<(PathBuf, usize)>,
    lanes: Vec<Medium>,
    /// The most readers a lane's filesystem bears, whatever its disk does.
    caps: Vec<Option<usize>>,
    /// A lane whose files a FUSE driver fetches from somewhere: not read.
    remote: Vec<bool>,
}

impl Ground {
    /// Ask the machine about each root.
    pub fn probe<P: AsRef<Path>>(roots: impl IntoIterator<Item = P>) -> Ground {
        let roots: Vec<PathBuf> = roots
            .into_iter()
            .map(|r| r.as_ref().to_path_buf())
            .collect();
        let mut ground = Ground::of(roots.iter().map(|root| {
            (
                root.clone(),
                surefoot::probe::disk_of(root),
                surefoot::probe::medium_of(root),
            )
        }));
        for root in &roots {
            let lane = ground.lane_of(root);
            let fs = surefoot::probe::filesystem_of(root);
            if let Some(cap) = fs.as_deref().and_then(cap_of) {
                let slot = &mut ground.caps[lane];
                *slot = Some(slot.map_or(cap, |c| c.min(cap)));
            }
            if fs.as_deref().is_some_and(fetched) {
                ground.remote[lane] = true;
            }
        }
        ground
    }

    /// From answers already had. Roots naming one disk share a lane; a root
    /// with no disk named has a lane of its own.
    pub fn of(roots: impl IntoIterator<Item = (PathBuf, Option<String>, Medium)>) -> Ground {
        let mut ground = Ground::default();
        let mut disks: Vec<Option<String>> = Vec::new();
        for (root, disk, medium) in roots {
            let lane = match disks.iter().position(|d| disk.is_some() && *d == disk) {
                Some(lane) => lane,
                None => {
                    disks.push(disk);
                    ground.lanes.push(medium);
                    ground.caps.push(None);
                    ground.remote.push(false);
                    disks.len() - 1
                }
            };
            ground.roots.push((root, lane));
        }
        ground
            .roots
            .sort_by_key(|(root, _)| std::cmp::Reverse(root.as_os_str().len()));
        ground
    }

    /// One more than the disks known: the last lane is everything else.
    pub(crate) fn lanes(&self) -> usize {
        self.lanes.len() + 1
    }

    pub(crate) fn lane_of(&self, path: &Path) -> usize {
        self.roots
            .iter()
            .find(|(root, _)| path.starts_with(root))
            .map_or(self.lanes.len(), |(_, lane)| *lane)
    }

    /// A root on this lane: what a disk is known by.
    pub(crate) fn root_of(&self, lane: usize) -> Option<&Path> {
        self.roots
            .iter()
            .find(|(_, l)| *l == lane)
            .map(|(root, _)| root.as_path())
    }

    #[cfg(test)]
    pub(crate) fn fetching(&mut self, lane: usize) {
        self.remote[lane] = true;
    }

    /// Whether this lane's files would be fetched to be read: see [`fetched`].
    pub(crate) fn remote(&self, lane: usize) -> bool {
        self.remote.get(lane).copied().unwrap_or(false)
    }

    pub(crate) fn medium(&self, lane: usize) -> Medium {
        self.lanes.get(lane).copied().unwrap_or(Medium::Unknown)
    }

    /// Readers for this lane: what its medium is worth, within what its
    /// filesystem bears.
    pub(crate) fn readers(&self, lane: usize, cores: usize) -> usize {
        let wide = readers(self.medium(lane), cores);
        match self.caps.get(lane).copied().flatten() {
            Some(cap) => wide.min(cap),
            None => wide,
        }
    }
}

/// Every lane this wide, for measuring: `SCOUR_GREP_READERS`.
pub(crate) fn forced() -> Option<usize> {
    std::env::var("SCOUR_GREP_READERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
}

/// Filesystems that bear fewer readers than their disk: a FUSE driver
/// (`ntfs-3g`, sshfs, rclone) serves every reader through one process, and
/// ZFS reads ahead on its own. The design's numbers, not yet measured.
fn cap_of(filesystem: &str) -> Option<usize> {
    let fs = filesystem.to_ascii_lowercase();
    (fs == "fuseblk" || fs == "fuse" || fs.starts_with("fuse.") || fs == "zfs").then_some(4)
}

/// A filesystem a driver in user space serves from somewhere else: rclone,
/// a Google Drive or GNOME online-accounts mount, sshfs. Reading a file there
/// downloads it, and nothing in the file says so: none is read. `fuseblk` is
/// a FUSE driver over a local disk — ntfs-3g, exfat-fuse — and is read.
fn fetched(filesystem: &str) -> bool {
    let fs = filesystem.to_ascii_lowercase();
    fs != "fuseblk" && (fs.contains("fuse") || fs.contains("winfsp") || fs.contains("dokan"))
}

/// How many threads reading file contents a medium is worth. Measured
/// (`docs/MEASUREMENTS.md`, 2026-11-24): a spinning disk reads fastest alone;
/// a solid one keeps gaining past eight — cold NVMe, 30,690 files: 1.72 s at
/// eight, 1.25 at twelve, 1.05 at sixteen — so half the cores, up to sixteen,
/// the other half left to whoever is at the desk. A network share is round
/// trips, so wide: the design's eight, until a share is measured; a disk
/// nobody can name, four and in the order given.
pub fn readers(medium: Medium, cores: usize) -> usize {
    if let Some(n) = forced() {
        return n;
    }
    match medium {
        Medium::Spinning => 1,
        Medium::Network => 8,
        Medium::Unknown => 4,
        Medium::Solid | Medium::Memory => (cores / 2).clamp(2, 16),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_on_the_longest_root_that_holds_it() {
        let ground = Ground::of([
            ("/".into(), Some("nvme1".into()), Medium::Solid),
            ("/home".into(), Some("nvme1".into()), Medium::Solid),
            ("/mnt/depo".into(), Some("sda".into()), Medium::Spinning),
        ]);
        assert_eq!(ground.lanes(), 3);
        assert_eq!(
            ground.lane_of(Path::new("/home/a")),
            ground.lane_of(Path::new("/etc/b"))
        );
        let depo = ground.lane_of(Path::new("/mnt/depo/x"));
        assert_eq!(ground.medium(depo), Medium::Spinning);
        // `/mnt/depository` is not under `/mnt/depo`.
        assert_ne!(ground.lane_of(Path::new("/mnt/depository/x")), depo);
    }

    #[test]
    fn a_fuse_or_zfs_volume_is_read_by_four_at_most() {
        assert_eq!(cap_of("fuseblk"), Some(4));
        assert_eq!(cap_of("fuse.sshfs"), Some(4));
        assert_eq!(cap_of("zfs"), Some(4));
        assert_eq!(cap_of("btrfs"), None);
        assert_eq!(cap_of("ntfs3"), None);
        let mut ground = Ground::of([("/z".into(), Some("pool".into()), Medium::Solid)]);
        ground.caps[0] = cap_of("zfs");
        assert_eq!(ground.readers(0, 20), 4);
        assert_eq!(
            ground.readers(ground.lane_of(Path::new("/elsewhere")), 20),
            4
        );
    }

    #[test]
    fn what_a_driver_fetches_from_elsewhere_is_told_from_a_local_disk() {
        for fs in [
            "fuse.rclone",
            "fuse.sshfs",
            "fuse.gvfsd-fuse",
            "fuse",
            "macfuse",
            "FUSE-rclone",
        ] {
            assert!(fetched(fs), "{fs}");
        }
        for fs in ["fuseblk", "ntfs3", "btrfs", "nfs", "cifs", "NTFS"] {
            assert!(!fetched(fs), "{fs}");
        }
    }

    #[test]
    fn what_is_under_no_root_is_read_carefully() {
        let ground = Ground::of([("/data".into(), None, Medium::Solid)]);
        let elsewhere = ground.lane_of(Path::new("/other/file"));
        assert_eq!(ground.medium(elsewhere), Medium::Unknown);
        assert_eq!(readers(Medium::Unknown, 20), 4);
        assert_eq!(readers(Medium::Spinning, 20), 1);
        assert_eq!(readers(Medium::Solid, 20), 10);
        assert_eq!(readers(Medium::Solid, 64), 16);
    }
}
