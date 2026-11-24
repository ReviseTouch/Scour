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
}

impl Ground {
    /// Ask the machine about each root.
    pub fn probe<P: AsRef<Path>>(roots: impl IntoIterator<Item = P>) -> Ground {
        Ground::of(roots.into_iter().map(|root| {
            let root = root.as_ref();
            (
                root.to_path_buf(),
                surefoot::probe::disk_of(root),
                surefoot::probe::medium_of(root),
            )
        }))
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

    pub(crate) fn medium(&self, lane: usize) -> Medium {
        self.lanes.get(lane).copied().unwrap_or(Medium::Unknown)
    }
}

/// How many threads reading file contents a medium is worth. Measured
/// (`docs/MEASUREMENTS.md`, 2026-11-24): a spinning disk reads fastest alone;
/// a solid one keeps gaining past eight — cold NVMe, 30,690 files: 1.72 s at
/// eight, 1.25 at twelve, 1.05 at sixteen — so half the cores, up to sixteen,
/// the other half left to whoever is at the desk. Network and unknown stay
/// the walk's numbers until a share is measured.
pub fn readers(medium: Medium, cores: usize) -> usize {
    // For measuring: every lane this wide.
    if let Some(n) = std::env::var("SCOUR_GREP_READERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
    {
        return n;
    }
    match medium {
        Medium::Spinning => 1,
        Medium::Network => 4,
        Medium::Unknown => 2,
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
    fn what_is_under_no_root_is_read_carefully() {
        let ground = Ground::of([("/data".into(), None, Medium::Solid)]);
        let elsewhere = ground.lane_of(Path::new("/other/file"));
        assert_eq!(ground.medium(elsewhere), Medium::Unknown);
        assert_eq!(readers(Medium::Unknown, 20), 2);
        assert_eq!(readers(Medium::Spinning, 20), 1);
        assert_eq!(readers(Medium::Solid, 20), 10);
        assert_eq!(readers(Medium::Solid, 64), 16);
    }
}
