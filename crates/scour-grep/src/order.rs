//! In what order one disk's files are read. A spinning disk pays a seek for
//! every jump, so a large set of its files is read by path, neighbours in a
//! directory being near neighbours on the platter; a small set, or a disk
//! that does not seek, is read as it was given, newest first.
//!
//! Where each file lies on the device (`FIEMAP`, retrieval pointers) was
//! measured as an order and lost to the path: `docs/MEASUREMENTS.md`.

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
}

impl Order {
    /// What a lane of `files` on `medium` is read in. `SCOUR_GREP_ORDER`
    /// (`given`, `path`) overrides it, for measuring.
    pub fn of(medium: Medium, files: &[File], list: &[usize]) -> Order {
        if let Some(forced) = std::env::var("SCOUR_GREP_ORDER")
            .ok()
            .and_then(|v| parse(&v))
        {
            return forced;
        }
        let bytes: u64 = list.iter().filter_map(|&i| files[i].size).sum();
        match medium {
            Medium::Spinning if list.len() >= SMALL_FILES || bytes >= SMALL_BYTES => Order::Path,
            _ => Order::Given,
        }
    }
}

fn parse(v: &str) -> Option<Order> {
    match v {
        "given" => Some(Order::Given),
        "path" => Some(Order::Path),
        _ => None,
    }
}

/// Put `list`, indices into `files`, in `order`.
pub(crate) fn arrange(order: Order, files: &[File], list: &mut [usize]) {
    match order {
        Order::Given => {}
        Order::Path => list.sort_by(|a, b| files[*a].path.cmp(&files[*b].path)),
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
        assert_eq!(Order::of(Medium::Spinning, &heavy, &[0]), Order::Path);
        let many: Vec<File> = (0..3000).map(|i| file(&format!("/f{i}"), 1)).collect();
        let list: Vec<usize> = (0..many.len()).collect();
        assert_eq!(Order::of(Medium::Spinning, &many, &list), Order::Path);
        assert_eq!(Order::of(Medium::Solid, &many, &list), Order::Given);
    }
}
