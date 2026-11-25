//! Reading an NTFS volume raw: every file's place from the MFT, read once
//! from start to end, and the files' clusters read in the order they lie on
//! the platter, many requests in flight — no file is opened. On a cold
//! 7,200 rpm disk that read 12,000 files 5.3× faster than the system did, on
//! an NTFS USB stick 2× (`docs/MEASUREMENTS.md`).
//!
//! It needs the volume opened, which needs an administrator: the service has
//! one. And it answers only what the normal way would:
//!
//! - the volume is read decrypted (`\\.\X:`), never the physical disk;
//! - a file is read raw only if the asker's token may read its data, by its
//!   security descriptor from `$Secure` (`AccessCheck`);
//! - places are the MFT's as it is now, never an index's; a file the change
//!   journal says changed in the last minutes, or while reading, is read the
//!   normal way — its newest bytes may be in the cache and not on the disk;
//!   so is a volume with no journal;
//! - compressed, encrypted, sparse, reparse-point and cloud files, files in
//!   more than a few pieces or with an attribute list: the normal way;
//! - **a file found to hold the text is read again the normal way**, and
//!   that is its answer: the raw read only says what is not there. A file
//!   held open by another program, or written a moment ago, is then
//!   answered exactly as it would have been.

#[cfg_attr(not(windows), allow(dead_code))]
mod journal;
#[cfg_attr(not(windows), allow(dead_code))]
mod mft;
#[cfg_attr(not(windows), allow(dead_code))]
mod sds;
#[cfg(windows)]
mod win;

#[cfg(not(windows))]
mod win {
    use std::path::Path;

    pub(crate) struct Plan;

    impl Plan {
        pub(crate) fn prepare(_: char, _: &[(usize, &Path)], _: u64) -> Option<Plan> {
            None
        }
        pub(crate) fn allow(&mut self) -> bool {
            false
        }
        pub(crate) fn read(&mut self, _: &mut dyn FnMut(usize, &[u8]) -> bool) {}
        pub(crate) fn settle(self) -> Settled {
            Settled::default()
        }
    }

    #[derive(Default)]
    pub(crate) struct Settled {
        pub read: Vec<usize>,
        pub left: Vec<usize>,
    }
}

pub(crate) use win::Plan;

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Below this, a lane is read the normal way: reading the MFT first costs
/// more than a few files' opening.
pub(crate) const RAW_FROM: usize = 64;
/// Files larger than this are read the normal way; one is held whole.
#[cfg_attr(not(windows), allow(dead_code))]
const RAW_MAX: u64 = 16 << 20;
/// A file in more pieces than this is read the normal way.
#[cfg_attr(not(windows), allow(dead_code))]
const MOST_RUNS: usize = 16;
/// Changed this recently, a file's newest bytes may be only in the cache.
#[cfg_attr(not(windows), allow(dead_code))]
const RECENT_SECS: u64 = 300;

/// `SCOUR_GREP_RAW`: `0` never, `1` on any lane of an NTFS volume however
/// few its files (a test's), otherwise on a disk that says it spins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Want {
    Never,
    Always,
    Spinning,
}

pub(crate) fn wanted() -> Want {
    match std::env::var("SCOUR_GREP_RAW").as_deref() {
        Ok("0") => Want::Never,
        Ok("1") => Want::Always,
        _ => Want::Spinning,
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn recent_secs() -> u64 {
    std::env::var("SCOUR_GREP_RAW_RECENT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(RECENT_SECS)
}

/// A drive letter, from `X:\…`: what the volume is opened by.
pub(crate) fn letter_of(path: &Path) -> Option<char> {
    let s = path.to_str()?;
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        .then(|| b[0].to_ascii_uppercase() as char)
}

/// A path as the MFT spells it, for comparing: backslashes, lower case.
#[cfg_attr(not(windows), allow(dead_code))]
fn key_of(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

/// What of the MFT is kept while it is read: every directory's name and
/// parent, and the records of files whose name is one asked for.
#[derive(Debug, Default)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct Catalog {
    names: HashSet<String>,
    dirs: HashMap<u64, Dir>,
    files: Vec<(u64, mft::Record)>,
}

#[derive(Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
struct Dir {
    parent: u64,
    parent_seq: u16,
    seq: u16,
    name: String,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Catalog {
    fn new<'a>(paths: impl Iterator<Item = &'a str>) -> Catalog {
        let names = paths
            .filter_map(|p| key_of(p).rsplit('\\').next().map(str::to_owned))
            .collect();
        Catalog {
            names,
            ..Catalog::default()
        }
    }

    fn take(&mut self, number: u64, rec: mft::Record) {
        if rec.directory {
            self.dirs.insert(
                number,
                Dir {
                    parent: rec.parent,
                    parent_seq: rec.parent_seq,
                    seq: rec.seq,
                    name: rec.name,
                },
            );
        } else if self.names.contains(&rec.name.to_lowercase()) {
            self.files.push((number, rec));
        }
    }

    /// Each file asked for that was found: its index, record number, record.
    fn resolve(
        self,
        letter: char,
        wanted: &HashMap<String, usize>,
    ) -> Vec<(usize, u64, mft::Record)> {
        let mut cache: HashMap<u64, Option<String>> = HashMap::new();
        let root = format!("{}:", letter.to_ascii_lowercase());
        let mut out = Vec::new();
        for (number, rec) in self.files {
            let Some(dir) = path_of(rec.parent, rec.parent_seq, &self.dirs, &mut cache, &root, 0)
            else {
                continue;
            };
            let key = format!("{dir}\\{}", rec.name.to_lowercase());
            if let Some(&i) = wanted.get(&key) {
                out.push((i, number, rec));
            }
        }
        out
    }
}

/// A directory's full path, lower case, cached; None when a link in the
/// chain is missing, reused (its sequence number differs) or too deep.
#[cfg_attr(not(windows), allow(dead_code))]
fn path_of(
    n: u64,
    seq: u16,
    dirs: &HashMap<u64, Dir>,
    cache: &mut HashMap<u64, Option<String>>,
    root: &str,
    depth: usize,
) -> Option<String> {
    if n == mft::ROOT {
        return Some(root.to_owned());
    }
    let dir = dirs.get(&n)?;
    if seq != 0 && seq != dir.seq {
        return None;
    }
    if let Some(known) = cache.get(&n) {
        return known.clone();
    }
    // A cycle in a damaged MFT would loop for ever.
    let path = if depth > 512 || dir.parent == n {
        None
    } else {
        path_of(dir.parent, dir.parent_seq, dirs, cache, root, depth + 1)
            .map(|p| format!("{p}\\{}", dir.name.to_lowercase()))
    };
    cache.insert(n, path.clone());
    path
}

/// What may be read raw of a record: its bytes are in the record, or in
/// these runs. None: the normal way.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum Place {
    Here(Vec<u8>),
    Runs(Vec<(u64, u64, u64)>),
}

/// FILETIME ticks a second.
#[cfg_attr(not(windows), allow(dead_code))]
const TICKS: i64 = 10_000_000;

#[cfg_attr(not(windows), allow(dead_code))]
fn place_of(rec: &mft::Record, cluster: u64, max_size: u64, cut: i64) -> Option<Place> {
    const SPARSE: u32 = 0x200;
    const REPARSE: u32 = 0x400;
    const COMPRESSED: u32 = 0x800;
    const OFFLINE: u32 = 0x1000;
    const ENCRYPTED: u32 = 0x4000;
    const RECALL_ON_OPEN: u32 = 0x0004_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    let not_here = SPARSE
        | REPARSE
        | COMPRESSED
        | OFFLINE
        | ENCRYPTED
        | RECALL_ON_OPEN
        | RECALL_ON_DATA_ACCESS;
    if rec.directory || rec.attributes & not_here != 0 || rec.security_id.is_none() {
        return None;
    }
    if rec.modified >= cut || rec.changed >= cut {
        return None;
    }
    if rec.size > RAW_MAX || (max_size != 0 && rec.size > max_size) {
        return None;
    }
    match &rec.data {
        mft::Data::Resident(bytes) if bytes.len() as u64 == rec.size => {
            Some(Place::Here(bytes.clone()))
        }
        mft::Data::Runs(runs)
            if runs.len() <= MOST_RUNS
                && runs.iter().map(|r| r.2).sum::<u64>() * cluster >= rec.size =>
        {
            Some(Place::Runs(runs.clone()))
        }
        _ => None,
    }
}

/// A piece of one file, at most a megabyte: where it lies, where it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
struct Piece {
    file: usize,
    lcn: u64,
    clusters: u64,
    offset: u64,
}

#[cfg_attr(not(windows), allow(dead_code))]
const PIECE: u64 = 1 << 20;
/// One request reads at most this, and over a gap at most `GAP` of what no
/// file asked for: a gap read costs less than a seek.
#[cfg_attr(not(windows), allow(dead_code))]
const SPAN: u64 = 1 << 20;
#[cfg_attr(not(windows), allow(dead_code))]
const GAP: u64 = 256 << 10;

#[cfg_attr(not(windows), allow(dead_code))]
fn pieces_of(file: usize, runs: &[(u64, u64, u64)], cluster: u64, size: u64, out: &mut Vec<Piece>) {
    let most = (PIECE / cluster).max(1);
    let wanted = size.div_ceil(cluster);
    for &(vcn, lcn, n) in runs {
        let n = n.min(wanted.saturating_sub(vcn));
        let mut done = 0;
        while done < n {
            let take = (n - done).min(most);
            out.push(Piece {
                file,
                lcn: lcn + done,
                clusters: take,
                offset: (vcn + done) * cluster,
            });
            done += take;
        }
    }
}

/// Pieces sorted by place, gathered into requests: (first cluster, end
/// cluster, the pieces' range).
#[cfg_attr(not(windows), allow(dead_code))]
fn spans_of(pieces: &[Piece], cluster: u64) -> Vec<(u64, u64, usize, usize)> {
    let mut spans = Vec::new();
    let mut i = 0;
    while i < pieces.len() {
        let start = pieces[i].lcn;
        let mut end = start + pieces[i].clusters;
        let mut j = i + 1;
        while let Some(next) = pieces.get(j) {
            let gap = next.lcn.saturating_sub(end) * cluster;
            let new_end = end.max(next.lcn + next.clusters);
            if gap > GAP || (new_end - start) * cluster > SPAN.max(PIECE) {
                break;
            }
            end = new_end;
            j += 1;
        }
        spans.push((start, end, i, j));
        i = j;
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::mft::tests::{file_name, nonresident, record, resident, standard};
    use super::*;

    fn rec(flags: u16, attrs: &[Vec<u8>]) -> mft::Record {
        mft::parse(&mut record(flags, attrs)).unwrap()
    }

    #[test]
    fn paths_are_built_from_parents_and_reused_records_break_them() {
        let wanted: HashMap<String, usize> = [
            (r"c:\docs\a.txt".to_owned(), 0),
            (r"c:\old\b.txt".to_owned(), 1),
        ]
        .into();
        let mut cat = Catalog::new([r"C:\Docs\A.txt", r"C:\old\b.txt"].into_iter());
        let mut docs = rec(3, &[file_name(5, 5, "Docs")]);
        docs.seq = 2;
        cat.take(40, docs);
        let mut old = rec(3, &[file_name(5, 5, "old")]);
        old.seq = 9;
        cat.take(41, old);
        cat.take(
            100,
            rec(1, &[file_name(40, 2, "A.txt"), resident(0x80, "", b"x")]),
        );
        // Its parent was record 41 of sequence 3; 41 is another directory now.
        cat.take(
            101,
            rec(1, &[file_name(41, 3, "b.txt"), resident(0x80, "", b"y")]),
        );
        // A name nobody asked for is not kept.
        cat.take(102, rec(1, &[file_name(40, 2, "c.txt")]));
        let got = cat.resolve('C', &wanted);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].0, got[0].1), (0, 100));
    }

    #[test]
    fn recent_cloud_and_odd_files_are_left_to_the_normal_way() {
        let runs = [0x21, 0x03, 0x00, 0x10, 0x00];
        let cut = 1000 * TICKS;
        let file = |modified: i64, attrs: u32| {
            rec(
                1,
                &[
                    standard(modified, attrs, 0x100),
                    file_name(5, 5, "f"),
                    nonresident("", 0, 9000, 9000, &runs),
                ],
            )
        };
        assert_eq!(
            place_of(&file(10, 0x20), 4096, 0, cut),
            Some(Place::Runs(vec![(0, 0x1000, 3)]))
        );
        assert_eq!(
            place_of(&file(cut, 0x20), 4096, 0, cut),
            None,
            "changed recently"
        );
        for attrs in [
            0x200,
            0x400,
            0x800,
            0x1000,
            0x4000,
            0x0004_0000,
            0x0040_0000,
        ] {
            assert_eq!(place_of(&file(10, attrs), 4096, 0, cut), None, "{attrs:#x}");
        }
        assert_eq!(
            place_of(&file(10, 0x20), 4096, 4000, cut),
            None,
            "over the limit"
        );
        // Runs that do not cover the size.
        assert_eq!(place_of(&file(10, 0x20), 2048, 0, cut), None);
        let mut no_id = file(10, 0x20);
        no_id.security_id = None;
        assert_eq!(place_of(&no_id, 4096, 0, cut), None);
    }

    #[test]
    fn pieces_stop_at_the_size_and_spans_join_neighbours() {
        let mut pieces = Vec::new();
        // 3 clusters allocated, 2 used; then 300 clusters in one run.
        pieces_of(0, &[(0, 100, 3)], 4096, 5000, &mut pieces);
        pieces_of(1, &[(0, 104, 300)], 4096, 300 * 4096, &mut pieces);
        pieces_of(2, &[(0, 10_000, 1)], 4096, 10, &mut pieces);
        assert_eq!(
            pieces[0],
            Piece {
                file: 0,
                lcn: 100,
                clusters: 2,
                offset: 0
            }
        );
        assert_eq!(pieces[1].clusters, 256);
        assert_eq!(
            pieces[2],
            Piece {
                file: 1,
                lcn: 360,
                clusters: 44,
                offset: 256 * 4096
            }
        );
        pieces.sort_by_key(|p| p.lcn);
        let spans = spans_of(&pieces, 4096);
        // The first file is a request alone — joined to the next it would
        // pass a megabyte — and the far one is too.
        assert_eq!(spans[0], (100, 102, 0, 1));
        assert_eq!(spans.last().unwrap().0, 10_000);
    }

    #[test]
    fn letters_come_from_drive_paths_only() {
        assert_eq!(letter_of(Path::new(r"g:\a\b.txt")), Some('G'));
        assert_eq!(letter_of(Path::new("C:/x")), Some('C'));
        assert_eq!(letter_of(Path::new(r"\\server\share\x")), None);
        assert_eq!(letter_of(Path::new(r"\\?\C:\x")), None);
        assert_eq!(letter_of(Path::new("/home/x")), None);
    }
}
