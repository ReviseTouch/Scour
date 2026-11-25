//! In what order one disk's files are read. A spinning disk pays a seek for
//! every jump, so a large set of its files is read where each lies on the
//! platter — asked of the filesystem (`FIEMAP`) on Linux, where that read
//! 10,000 files in 31.7 s against 60.6 s by path on a 5,400 rpm disk — and by
//! path elsewhere, neighbours in a directory being near neighbours on the
//! platter. Windows' retrieval pointers lost to the path on PC2, in a
//! measurement whose cache was not truly cold; until it is repeated, Windows
//! reads by path. A small set, or a disk that does not seek, is read as it
//! was given, newest first. `docs/MEASUREMENTS.md`.

use surefoot::Medium;

use crate::scan::File;

/// Below both, sorting costs more than it saves: the set is read in moments.
const SMALL_FILES: usize = 2_000;
const SMALL_BYTES: u64 = 64 << 20;

/// How the files of a lane are put in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// As given: newest first, from the index.
    Given,
    /// By path: neighbours in a directory are often neighbours on the disk.
    Path,
    /// By where each file's first byte is on the device; by path where the
    /// filesystem will not say.
    Physical,
}

impl Order {
    /// What a lane of `files` on `medium` is read in. `SCOUR_GREP_ORDER`
    /// (`given`, `path`, `physical`) overrides it, for measuring.
    pub fn of(medium: Medium, files: &[File], list: &[usize]) -> Order {
        if let Some(forced) = std::env::var("SCOUR_GREP_ORDER")
            .ok()
            .and_then(|v| parse(&v))
        {
            return forced;
        }
        let bytes: u64 = list.iter().filter_map(|&i| files[i].size).sum();
        match medium {
            Medium::Spinning if list.len() >= SMALL_FILES || bytes >= SMALL_BYTES => {
                if cfg!(target_os = "linux") {
                    Order::Physical
                } else {
                    Order::Path
                }
            }
            _ => Order::Given,
        }
    }
}

fn parse(v: &str) -> Option<Order> {
    match v {
        "given" => Some(Order::Given),
        "path" => Some(Order::Path),
        "physical" => Some(Order::Physical),
        _ => None,
    }
}

/// Put `list`, indices into `files`, in `order`.
pub(crate) fn arrange(order: Order, files: &[File], list: &mut [usize]) {
    match order {
        Order::Given => {}
        Order::Path => list.sort_by(|a, b| files[*a].path.cmp(&files[*b].path)),
        Order::Physical => {
            let mut keyed: Vec<(Place, usize)> =
                list.iter().map(|&i| (place(&files[i].path), i)).collect();
            // The unplaced last, by path; ties by path too.
            keyed.sort_by(|(ka, a), (kb, b)| {
                ka.cmp(kb).then_with(|| files[*a].path.cmp(&files[*b].path))
            });
            for (slot, (_, i)) in list.iter_mut().zip(keyed) {
                *slot = i;
            }
        }
    }
}

/// Where a file lies: its device, how sure that is (0 its first extent,
/// 1 only its inode, 2 nothing), and the number.
type Place = (u64, u8, u64);

fn place(path: &std::path::Path) -> Place {
    sys::place(path).unwrap_or((u64::MAX, 2, 0))
}

#[cfg(target_os = "linux")]
mod sys {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::path::Path;

    /// `_IOWR('f', 11, struct fiemap)`.
    const FS_IOC_FIEMAP: libc::c_ulong = 0xC020_660B;

    /// `struct fiemap` with room for one `struct fiemap_extent`.
    #[repr(C)]
    struct Fiemap {
        start: u64,
        length: u64,
        flags: u32,
        mapped: u32,
        count: u32,
        reserved: u32,
        /// logical, physical, length, two reserved, then flags and three reserved.
        extent: [u64; 7],
    }

    pub(super) fn place(path: &Path) -> Option<super::Place> {
        let file = std::fs::File::options()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)
            .ok()?;
        let meta = file.metadata().ok()?;
        let mut map = Fiemap {
            start: 0,
            length: u64::MAX,
            flags: 0,
            mapped: 0,
            count: 1,
            reserved: 0,
            extent: [0; 7],
        };
        // SAFETY: the buffer holds the header and the one extent asked for.
        let ok = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_FIEMAP, &mut map) } == 0;
        if ok && map.mapped > 0 && map.extent[1] > 0 {
            return Some((meta.dev(), 0, map.extent[1]));
        }
        Some((meta.dev(), 1, meta.ino()))
    }
}

#[cfg(not(target_os = "linux"))]
mod sys {
    pub(super) fn place(_path: &std::path::Path) -> Option<super::Place> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> File {
        File {
            path: path.into(),
            size: Some(size),
            class: None,
        }
    }

    #[test]
    fn only_a_large_set_on_a_spinning_disk_is_put_in_place() {
        let few: Vec<File> = (0..10).map(|i| file(&format!("/f{i}"), 10)).collect();
        let list: Vec<usize> = (0..few.len()).collect();
        assert_eq!(Order::of(Medium::Spinning, &few, &list), Order::Given);
        let heavy = vec![file("/big", 100 << 20)];
        let large = if cfg!(target_os = "linux") {
            Order::Physical
        } else {
            Order::Path
        };
        assert_eq!(Order::of(Medium::Spinning, &heavy, &[0]), large);
        let many: Vec<File> = (0..3000).map(|i| file(&format!("/f{i}"), 1)).collect();
        let list: Vec<usize> = (0..many.len()).collect();
        assert_eq!(Order::of(Medium::Spinning, &many, &list), large);
        assert_eq!(Order::of(Medium::Solid, &many, &list), Order::Given);
    }

    #[test]
    fn files_are_placed_and_the_unplaceable_go_last_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for name in ["b", "a", "c"] {
            let path = dir.path().join(name);
            std::fs::write(&path, vec![b'x'; 8192]).unwrap();
            files.push(File {
                path,
                size: Some(8192),
                class: None,
            });
        }
        files.push(file("/nowhere/z", 1));
        files.push(file("/nowhere/y", 1));
        let mut list: Vec<usize> = (0..files.len()).collect();
        arrange(Order::Physical, &files, &mut list);
        // Every real file before the two that cannot be placed, and those by path.
        assert_eq!(&list[3..], &[4, 3]);
        let mut real = list[..3].to_vec();
        real.sort();
        assert_eq!(real, [0, 1, 2]);
    }
}
