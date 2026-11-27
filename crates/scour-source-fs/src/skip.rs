//! The rules as one source reads them.
//!
//! It does not go into other sources' roots under its own: the walk crosses
//! mounts, so `/` walked as one disk would walk `/home` and `/mnt/data` too,
//! which are disks of their own and indexed as such.
//!
//! And a path rule over the whole of it is not its rule: `/run` is skipped
//! everywhere, and a stick a desktop mounts under `/run/media` is a source
//! of its own.
//!
//! Nor does it go into the kernel's own interfaces mounted as filesystems,
//! whatever the rules say: a rule can be switched off, and `/` walked as a
//! disk with every rule off went into `/proc` — 1.7 M entries that are not
//! files, more unreadable places than a walk may be blind in, so the walk
//! never ended and rewrote them every few minutes (2026-11-27).

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use scour_core::{ScanOptions, WatchHandle};

/// The scan options with this source's skips added to what everybody skips,
/// and without the path rules that would hide all of it.
pub fn own<'a>(opts: &'a ScanOptions, roots: &[PathBuf], skip: &[String]) -> Cow<'a, ScanOptions> {
    let covers = |rule: &String| roots.iter().any(|r| r.starts_with(Path::new(rule)));
    let kernel = virtual_under(roots);
    if skip.is_empty() && kernel.is_empty() && !opts.exclude_paths.iter().any(covers) {
        return Cow::Borrowed(opts);
    }
    let mut mine = opts.clone();
    mine.exclude_paths.retain(|rule| !covers(rule));
    mine.deny.extend(skip.iter().cloned());
    mine.deny.extend(kernel);
    Cow::Owned(mine)
}

/// Filesystems that are the kernel's interfaces rather than files, by the type
/// `mountinfo` gives them. `tmpfs` is not among them: `/tmp` holds real files.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const VIRTUAL: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "cgroup",
    "cgroup2",
    "debugfs",
    "tracefs",
    "securityfs",
    "pstore",
    "bpf",
    "configfs",
    "fusectl",
    "mqueue",
    "hugetlbfs",
    "efivarfs",
    "binfmt_misc",
    "autofs",
    "rpc_pipefs",
    "nsfs",
    "selinuxfs",
    "smackfs",
];

/// The mount points of [`VIRTUAL`] filesystems strictly under one of `roots`,
/// in a `mountinfo` table. A root that is one is left alone: it was asked for.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn virtual_in(mountinfo: &str, roots: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    for line in mountinfo.lines() {
        let Some((head, tail)) = line.split_once(" - ") else {
            continue;
        };
        let Some(kind) = tail.split_whitespace().next() else {
            continue;
        };
        if !VIRTUAL.contains(&kind) {
            continue;
        }
        let Some(point) = head.split_whitespace().nth(4) else {
            continue;
        };
        let point = PathBuf::from(crate::pulse::unescape(point));
        let under = roots.iter().any(|r| point != *r && point.starts_with(r));
        let said = crate::path::from_path(&point);
        if under && !out.contains(&said) {
            out.push(said);
        }
    }
    out
}

/// [`virtual_in`] for this machine's mounts as they are now.
fn virtual_under(roots: &[PathBuf]) -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        let table = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
        virtual_in(&table, roots)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = roots;
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "\
22 1 0:21 / /proc rw,nosuid shared:5 - proc proc rw
23 1 0:22 / /sys rw,nosuid shared:6 - sysfs sysfs rw
24 1 0:5 / /dev rw,nosuid shared:2 - devtmpfs devtmpfs rw,size=4096k
25 24 0:23 / /dev/pts rw shared:3 - devpts devpts rw
26 23 0:24 / /sys/fs/cgroup rw shared:7 - cgroup2 cgroup2 rw
27 1 0:25 / /tmp rw shared:8 - tmpfs tmpfs rw
28 1 0:26 / /home rw shared:9 - btrfs /dev/nvme1n1p5 rw
29 1 0:27 / /mnt/My\\040Disk rw shared:10 - proc proc rw
";

    /// Only the kernel's own, only under a root, and a root that is one stays.
    #[test]
    fn the_kernels_interfaces_are_never_walked_whatever_the_rules() {
        let root = |p: &str| vec![PathBuf::from(p)];
        assert_eq!(
            virtual_in(TABLE, &root("/")),
            [
                "/proc",
                "/sys",
                "/dev",
                "/dev/pts",
                "/sys/fs/cgroup",
                "/mnt/My Disk"
            ]
        );
        assert!(virtual_in(TABLE, &root("/home")).is_empty());
        assert_eq!(virtual_in(TABLE, &root("/sys")), ["/sys/fs/cgroup"]);
        assert!(virtual_in(TABLE, &root("/proc")).is_empty());
    }
}

/// A watch that keeps reading the rules this way when they are edited:
/// `retune` is handed the shared options, which know nothing of the source.
#[derive(Debug)]
pub struct Skipping {
    pub inner: Box<dyn WatchHandle>,
    pub roots: Vec<PathBuf>,
    pub skip: Vec<String>,
}

impl WatchHandle for Skipping {
    fn unwatched(&self) -> Vec<String> {
        self.inner.unwatched()
    }

    fn cover(&self, path: &str) {
        self.inner.cover(path);
    }

    fn retune(&self, opts: &ScanOptions) {
        self.inner.retune(&own(opts, &self.roots, &self.skip));
    }

    fn stop(self: Box<Self>) {
        self.inner.stop();
    }
}
