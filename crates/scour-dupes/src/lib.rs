//! The same file, several times over: group by size, then head and tail, then a
//! full byte-for-byte comparison. Confirmed largest first, because a unique size
//! eliminates only 6.2% of files while the 18,723 candidates above 1 MB hold
//! 141.8 GB of the possible waste. Stage three compares rather than digests: it
//! decides what somebody deletes.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// How much of each end is read in stage two.
const EDGE: u64 = 4 * 1024;

/// How much is moved per read while comparing.
const CHUNK: usize = 64 * 1024;

/// What a caller wants out of this.
#[derive(Debug, Clone)]
pub struct Options {
    /// Ignore anything smaller than this. Below a megabyte the candidates are a
    /// million files over 28 GB; above it, nineteen thousand over 141.8 GB.
    pub min_size: u64,
    /// How many bytes may be read to confirm. Zero is a supported answer: the
    /// size groups and their potential saving cost nothing.
    pub read_budget: u64,
    /// At most this many groups in the reply, largest saving first.
    pub top: usize,
}

impl Default for Options {
    fn default() -> Self {
        // A megabyte is the measured knee; a gigabyte of reading confirms the
        // whole interesting range above it.
        Options {
            min_size: 1024 * 1024,
            read_budget: 1024 * 1024 * 1024,
            top: 50,
        }
    }
}

/// How much this group has been proven, rather than guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Certainty {
    /// Same size, nothing read. These files *may* be identical.
    Size,
    /// Same size and same first and last four kilobytes. Likely, not proven.
    Edges,
    /// Read end to end and compared. Identical.
    Content,
}

impl Certainty {
    /// A stable word for a wire format or a message id.
    pub fn token(self) -> &'static str {
        match self {
            Certainty::Size => "size",
            Certainty::Edges => "edges",
            Certainty::Content => "content",
        }
    }
}

/// Files that are, or may be, the same file.
#[derive(Debug, Clone)]
pub struct Group {
    /// What each of them weighs; equal size is what makes them a group.
    pub size: u64,
    /// Their paths, in the order they arrived.
    pub paths: Vec<String>,
    pub certainty: Certainty,
}

impl Group {
    /// What deleting all but one would give back. Groups are ordered by this and
    /// not by size: ten copies of 100 MB beat two copies of 400 MB.
    pub fn waste(&self) -> u64 {
        self.size * (self.paths.len() as u64 - 1)
    }
}

/// What one run found.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Largest saving first.
    pub groups: Vec<Group>,
    /// How many files were considered at all.
    pub candidates: u64,
    /// Everything the groups could give back, including those `top` left out: a
    /// total that moved with the list length would be one nobody could act on.
    pub waste: u64,
    /// How much of `waste` was read and compared rather than guessed: 39.36 GiB
    /// by size was 18.29 GiB once read, so `waste` alone names files that are
    /// merely the same length.
    pub proven: u64,
    /// Bytes actually read confirming.
    pub read: u64,
    /// Groups the budget did not reach. Without it a partial answer looks like a
    /// complete one.
    pub unconfirmed: u64,
}

/// Find them. Only the size is used before anything is read, and a file that has
/// since disappeared is dropped rather than failing the run.
pub fn find<I>(files: I, opts: &Options) -> Report
where
    I: IntoIterator<Item = (String, u64)>,
{
    find_named(files.into_iter().map(|(p, size)| (p, size, 1)), opts)
}

/// The same, told how many names each file has (`st_nlink`). A file with more
/// than one may be a hard link of another candidate, and only those are looked
/// at before the budget allows anything: a size-only answer stays free.
pub fn find_named<I>(files: I, opts: &Options) -> Report
where
    I: IntoIterator<Item = (String, u64, u64)>,
{
    // Stage one: size to paths. No `stat`, no `open` — the caller had the sizes.
    let mut by_size: HashMap<u64, Vec<(String, u64)>> = HashMap::new();
    let mut candidates = 0u64;
    for (path, size, links) in files {
        if size < opts.min_size {
            continue;
        }
        candidates += 1;
        by_size.entry(size).or_default().push((path, links));
    }

    let mut pending: Vec<(u64, Vec<String>)> = by_size
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(size, mut named)| {
            // Sorted first, so the name that stands for a linked file is the
            // same one stage two keeps.
            named.sort();
            let mut files = std::collections::HashSet::new();
            let paths = named
                .into_iter()
                .filter(|(p, links)| {
                    *links < 2 || identity(Path::new(p)).is_none_or(|id| files.insert(id))
                })
                .map(|(p, _)| p)
                .collect::<Vec<_>>();
            (size, paths)
        })
        .filter(|(_, paths)| paths.len() > 1)
        .collect();
    // Largest possible saving first, so a budget runs out where it is worth
    // least. Ties by size then path, so two runs over one disk answer alike.
    pending.sort_by(|a, b| {
        let (wa, wb) = (a.0 * (a.1.len() as u64 - 1), b.0 * (b.1.len() as u64 - 1));
        wb.cmp(&wa)
            .then(b.0.cmp(&a.0))
            .then(a.1.first().cmp(&b.1.first()))
    });

    let mut out: Vec<Group> = Vec::new();
    let mut read = 0u64;
    let mut unconfirmed = 0u64;

    for (size, mut paths) in pending {
        paths.sort();
        // Out of budget: a real size collision and a real possible saving, but
        // it goes into the answer saying it is unconfirmed. Asked before the
        // reading, not after: a budget is what may be read, not where to stop.
        let ends = paths.len() as u64 * size.min(2 * EDGE);
        if read.saturating_add(ends) > opts.read_budget {
            unconfirmed += 1;
            out.push(Group {
                size,
                paths,
                certainty: Certainty::Size,
            });
            continue;
        }

        // Stage two: the ends, which is where nearly all differing files differ.
        let mut buckets: HashMap<u64, Vec<String>> = HashMap::new();
        // Two names of one file — a hard link — are not two copies: deleting
        // one gives back nothing. The first name, sorted, stands for the file.
        let mut files = std::collections::HashSet::new();
        for p in paths {
            if identity(Path::new(&p)).is_some_and(|id| !files.insert(id)) {
                continue;
            }
            // Dropped rather than fatal: one vanished file must not cost the
            // rest of the run its answer.
            if let Ok((mark, n)) = edges(Path::new(&p), size) {
                read += n;
                buckets.entry(mark).or_default().push(p);
            }
        }

        for (_, mut same) in buckets {
            if same.len() < 2 {
                continue;
            }
            same.sort();
            // Stage three: the bytes, a comparison and not a digest, since this
            // decides a deletion. Each class of identical files is its own group.
            let (classes, left, n) = partition(&same, size, opts.read_budget.saturating_sub(read));
            read += n;
            for class in classes.into_iter().filter(|c| c.len() > 1) {
                out.push(Group {
                    size,
                    paths: class,
                    certainty: Certainty::Content,
                });
            }
            // What the budget did not reach still shares a size and its ends.
            if left.len() > 1 {
                unconfirmed += 1;
                out.push(Group {
                    size,
                    paths: left,
                    certainty: Certainty::Edges,
                });
            }
        }
    }

    out.sort_by(|a, b| {
        b.waste()
            .cmp(&a.waste())
            .then(b.size.cmp(&a.size))
            .then(a.paths.first().cmp(&b.paths.first()))
    });
    let waste = out.iter().map(Group::waste).sum();
    let proven = out
        .iter()
        .filter(|g| g.certainty == Certainty::Content)
        .map(Group::waste)
        .sum();
    out.truncate(opts.top);
    Report {
        groups: out,
        candidates,
        waste,
        proven,
        read,
        unconfirmed,
    }
}

/// One number from the first and last four kilobytes, and how much was read. A
/// file shorter than two edges is read once and whole.
fn edges(path: &Path, size: u64) -> std::io::Result<(u64, u64)> {
    let mut f = File::open(path)?;
    let mut h = Fnv::new();
    // The size goes into the mark, so a short read cannot collide two lengths.
    h.write(&size.to_le_bytes());

    let mut buf = vec![0u8; EDGE as usize];
    let head = f.read(&mut buf)?;
    h.write(&buf[..head]);
    let mut moved = head as u64;

    if size > EDGE * 2 {
        f.seek(SeekFrom::End(-(EDGE as i64)))?;
        let tail = f.read(&mut buf)?;
        h.write(&buf[..tail]);
        moved += tail as u64;
    }
    Ok((h.finish(), moved))
}

/// These, sorted into classes that are identical byte for byte, and how much
/// was read. Each file is held against one member of each class found so far:
/// equality is transitive, so that is enough — but only against the first of
/// them all, `a b b` lost the two that matched each other. Stops before a
/// comparison the budget cannot pay for (two whole files); what it had not
/// reached comes back as the second list.
fn partition(paths: &[String], size: u64, budget: u64) -> (Vec<Vec<String>>, Vec<String>, u64) {
    let mut classes: Vec<Vec<String>> = Vec::new();
    let mut read = 0u64;
    for (at, path) in paths.iter().enumerate() {
        let mut placed = false;
        for class in &mut classes {
            if read.saturating_add(size * 2) > budget {
                return (classes, paths[at..].to_vec(), read);
            }
            match identical(Path::new(&class[0]), Path::new(path)) {
                Ok(true) => {
                    read += size * 2;
                    class.push(path.clone());
                    placed = true;
                    break;
                }
                Ok(false) => read += size, // Parted early; a whole file's worth over-estimates.
                // Unreadable: no class, and no new one either.
                Err(_) => {
                    placed = true;
                    break;
                }
            }
        }
        if !placed {
            classes.push(vec![path.clone()]);
        }
    }
    (classes, Vec::new(), read)
}

/// Which file a name is, where the platform can say: the device and the inode.
#[cfg(unix)]
fn identity(p: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(p)
        .ok()
        .map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn identity(_: &Path) -> Option<(u64, u64)> {
    None
}

/// Byte for byte, stopping at the first difference.
fn identical(a: &Path, b: &Path) -> std::io::Result<bool> {
    // The same path twice needs no reading.
    if a == b {
        return Ok(true);
    }
    let (mut fa, mut fb) = (File::open(a)?, File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

/// Fill the buffer, or reach the end trying. `Read::read` may return short at any
/// time, and treating that as end-of-file calls two different files identical.
fn read_full(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut at = 0;
    while at < buf.len() {
        match f.read(&mut buf[at..])? {
            0 => break,
            n => at += n,
        }
    }
    Ok(at)
}

/// FNV-1a, 64 bits. Only ever a filter: two files that agree here are then
/// compared in full, so no hash decides identity.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= *b as u64;
            self.0 = self.0.wrapping_mul(0x1000_0000_01b3);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Dir {
            let p = std::env::temp_dir().join(format!("scour-dupes-{name}"));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).expect("mkdir");
            Dir(p)
        }
        fn file(&self, name: &str, bytes: &[u8]) -> (String, u64) {
            let p = self.0.join(name);
            std::fs::write(&p, bytes).expect("write");
            (p.to_string_lossy().into_owned(), bytes.len() as u64)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn opts(min: u64) -> Options {
        Options {
            min_size: min,
            read_budget: u64::MAX,
            top: 50,
        }
    }

    #[test]
    fn the_same_bytes_under_three_names_are_one_group() {
        let d = Dir::new("three");
        let body = vec![b'a'; 5000];
        let files = vec![
            d.file("one", &body),
            d.file("two", &body),
            d.file("three", &body),
            d.file("other", &vec![b'b'; 5000]),
        ];
        let r = find(files, &opts(1000));
        assert_eq!(r.groups.len(), 1, "{:?}", r.groups);
        assert_eq!(r.groups[0].paths.len(), 3);
        assert_eq!(r.groups[0].certainty, Certainty::Content);
        assert_eq!(r.groups[0].waste(), 10_000);
        assert_eq!(r.waste, 10_000);
        assert_eq!(
            r.proven, 10_000,
            "read and compared, so all of it is proven"
        );
    }

    /// Two names of one inode free nothing when one goes: they are one file,
    /// and only the real copy beside them is waste.
    #[test]
    #[cfg(unix)]
    fn a_hard_link_is_one_file_and_not_a_copy() {
        let d = Dir::new("links");
        let body = vec![b'h'; 6000];
        let first = d.file("a-first", &body);
        let link = d.0.join("b-link");
        std::fs::hard_link(&first.0, &link).expect("link");
        let link = (link.to_string_lossy().into_owned(), first.1);
        let copy = d.file("c-copy", &body);

        let r = find(vec![first.clone(), link.clone(), copy.clone()], &opts(1000));
        assert_eq!(r.groups.len(), 1, "{:?}", r.groups);
        assert_eq!(r.groups[0].paths, vec![first.0.clone(), copy.0]);
        assert_eq!(r.waste, 6000, "one copy's worth, not two");

        // Names of one file and nothing else are no group at all.
        let r = find(vec![first.clone(), link.clone()], &opts(1000));
        assert!(r.groups.is_empty(), "{:?}", r.groups);

        // Told the names, a size-only answer knows it too, reading nothing.
        let named = |(p, size): (String, u64)| (p, size, 2);
        let r = find_named(
            vec![named(first), named(link)],
            &Options {
                read_budget: 0,
                ..opts(1000)
            },
        );
        assert!(r.groups.is_empty(), "{:?}", r.groups);
        assert_eq!(r.read, 0);
    }

    /// Same ends, three files, the first unlike the other two. Held only
    /// against the first, B and C were never compared and the pair was lost.
    fn abc(d: &Dir) -> Vec<(String, u64)> {
        let body = |mid: u8| {
            let mut v = vec![b'X'; 4096];
            v.extend(vec![mid; 16_384]);
            v.extend(vec![b'Z'; 4096]);
            v
        };
        vec![
            d.file("dup_a.bin", &body(b'A')),
            d.file("dup_b.bin", &body(b'B')),
            d.file("dup_c.bin", &body(b'B')),
        ]
    }

    #[test]
    fn two_that_match_each_other_are_found_behind_one_that_matches_neither() {
        let d = Dir::new("abc");
        let files = abc(&d);
        let r = find(files.clone(), &opts(20_000));
        assert_eq!(r.groups.len(), 1, "{:?}", r.groups);
        assert_eq!(
            r.groups[0].paths,
            vec![files[1].0.clone(), files[2].0.clone()]
        );
        assert_eq!(r.groups[0].certainty, Certainty::Content);
        assert_eq!(r.waste, 24_576);
    }

    /// A budget is what may be read, asked before reading: one byte reads
    /// nothing, rather than a whole size group's ends.
    #[test]
    fn a_budget_smaller_than_the_next_read_reads_nothing_more() {
        let d = Dir::new("budget");
        let r = find(
            abc(&d),
            &Options {
                min_size: 20_000,
                read_budget: 1,
                top: 100,
            },
        );
        assert_eq!(r.read, 0);
        assert_eq!(r.unconfirmed, 1);
        assert_eq!(r.groups[0].certainty, Certainty::Size);

        // Enough for the ends and not for one comparison: ends, unconfirmed.
        let r = find(
            abc(&d),
            &Options {
                min_size: 20_000,
                read_budget: 3 * 2 * EDGE,
                top: 100,
            },
        );
        assert!(r.read <= 3 * 2 * EDGE, "read {}", r.read);
        assert_eq!(r.groups[0].certainty, Certainty::Edges);
    }

    /// Same size, same edges, different in the middle — an in-place edit.
    #[test]
    fn two_files_that_differ_only_in_the_middle_are_not_the_same_file() {
        let d = Dir::new("middle");
        let mut a = vec![b'x'; 20_000];
        let mut b = a.clone();
        a[10_000] = b'1';
        b[10_000] = b'2';
        let r = find(vec![d.file("a", &a), d.file("b", &b)], &opts(1000));
        assert!(r.groups.is_empty(), "{:?}", r.groups);
        // And it was read to find that out.
        assert!(r.read > 0);
    }

    #[test]
    fn files_that_differ_at_the_start_are_parted_without_being_read() {
        let d = Dir::new("edges");
        let a = vec![b'a'; 4 * 1024 * 1024];
        let mut b = a.clone();
        b[0] = b'z';
        let r = find(vec![d.file("a", &a), d.file("b", &b)], &opts(1024));
        assert!(r.groups.is_empty());
        // Two edges from each file and nothing more: 16 KB against 8 MB.
        assert!(r.read <= EDGE * 4, "read {} bytes", r.read);
    }

    /// Zero budget is an answer, not a failure: the size groups are free.
    #[test]
    fn nothing_read_still_reports_what_might_be_saved() {
        let d = Dir::new("nobudget");
        let body = vec![b'q'; 8000];
        let files = vec![d.file("one", &body), d.file("two", &body)];
        let r = find(
            files,
            &Options {
                min_size: 1000,
                read_budget: 0,
                top: 50,
            },
        );
        assert_eq!(r.read, 0);
        assert_eq!(r.groups.len(), 1);
        assert_eq!(r.groups[0].certainty, Certainty::Size);
        assert_eq!(r.groups[0].waste(), 8000);
        assert_eq!(r.unconfirmed, 1, "a partial answer must say it is partial");
        // Nothing was read, so nothing is proven.
        assert_eq!(r.proven, 0);
    }

    #[test]
    fn the_biggest_saving_comes_first_even_when_it_is_not_the_biggest_file() {
        let d = Dir::new("order");
        let small = vec![b's'; 2000];
        let big = vec![b'B'; 9000];
        let mut files = vec![d.file("big1", &big), d.file("big2", &big)];
        for i in 0..8 {
            files.push(d.file(&format!("small{i}"), &small));
        }
        let r = find(files, &opts(1000));
        assert_eq!(r.groups.len(), 2);
        // 8 small copies waste 14,000; 2 big ones waste 9,000.
        assert_eq!(r.groups[0].size, 2000);
        assert_eq!(r.groups[0].waste(), 14_000);
        assert_eq!(r.groups[1].size, 9000);
    }

    #[test]
    fn a_file_that_disappeared_does_not_take_the_answer_with_it() {
        let d = Dir::new("gone");
        let body = vec![b'g'; 3000];
        let mut files = vec![d.file("a", &body), d.file("b", &body), d.file("c", &body)];
        files.push(("/nonexistent/never-was".into(), 3000));
        let r = find(files, &opts(1000));
        assert_eq!(r.groups.len(), 1);
        assert_eq!(r.groups[0].paths.len(), 3);
    }

    #[test]
    fn anything_under_the_floor_is_not_looked_at() {
        let d = Dir::new("floor");
        let tiny = vec![b't'; 10];
        let r = find(vec![d.file("a", &tiny), d.file("b", &tiny)], &opts(1000));
        assert!(r.groups.is_empty());
        assert_eq!(r.candidates, 0);
        assert_eq!(r.read, 0);
    }

    #[test]
    fn a_comparison_reads_to_the_end_rather_than_to_the_first_short_read() {
        let d = Dir::new("chunky");
        // Longer than one chunk, differing only past the first boundary.
        let mut a = vec![b'c'; CHUNK * 2 + 500];
        let mut b = a.clone();
        a[CHUNK + 7] = b'1';
        b[CHUNK + 7] = b'2';
        let r = find(vec![d.file("a", &a), d.file("b", &b)], &opts(1000));
        assert!(r.groups.is_empty(), "{:?}", r.groups);
    }

    #[test]
    fn empty_input_is_an_empty_report() {
        let r = find(Vec::new(), &opts(1));
        assert!(r.groups.is_empty());
        assert_eq!(r.candidates, 0);
        assert_eq!(r.waste, 0);
    }

    #[test]
    fn the_total_does_not_change_when_the_list_is_cut() {
        let d = Dir::new("top");
        let mut files = Vec::new();
        for i in 0..5 {
            let body = vec![b'a' + i as u8; 2000 + i * 100];
            files.push(d.file(&format!("{i}-one"), &body));
            files.push(d.file(&format!("{i}-two"), &body));
        }
        let all = find(files.clone(), &opts(1000));
        let cut = find(
            files,
            &Options {
                top: 2,
                ..opts(1000)
            },
        );
        assert_eq!(all.groups.len(), 5);
        assert_eq!(cut.groups.len(), 2);
        assert_eq!(all.waste, cut.waste);
    }
}
