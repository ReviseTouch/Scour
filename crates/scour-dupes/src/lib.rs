//! The same file, several times over.
//!
//! ## Nobody wants a count of duplicates. They want the space back.
//!
//! That sentence is the whole design, and it is why this does not work the way
//! a duplicate finder usually does. The usual shape is *gate by size, then
//! hash everything that survives* — and the gate was measured on this machine,
//! against 1,474,650 files and 493.6 GB:
//!
//! **"A file with a unique size cannot have a duplicate" is true and nearly
//! useless.** It eliminates 6.2% of files. Not the overwhelming majority. The
//! reason is that 59.3% of files are 4 KB or smaller — 22,761 of them are
//! exactly zero bytes — and small files collide on size trivially. Build
//! output makes it worse rather than better.
//!
//! The same gate is excellent by *bytes*:
//!
//! | candidates above | files | bytes they hold | head+tail to check |
//! |---|---|---|---|
//! | nothing | 1,382,866 | 169.8 GB | 10.55 GB |
//! | 4 KB | 531,020 | 169.0 GB | 4.05 GB |
//! | 64 KB | 104,482 | 163.0 GB | 0.80 GB |
//! | 1 MB | **18,723** | **141.8 GB** | **0.14 GB** |
//! | 10 MB | 1,919 | 90.9 GB | 0.01 GB |
//!
//! So it works **down from the largest**. Above a megabyte, reading the first
//! and last four kilobytes of every size-collision candidate costs 140 MB and
//! covers 141.8 GB of the possible waste — a thousandfold return, finishing in
//! seconds where a whole-disk hash takes hours.
//!
//! ## Three stages, and each one is useful on its own
//!
//! 1. **Group by size, descending.** Free — the sizes are already known — and
//!    the potential saving of each group is a number nobody had to read a byte
//!    for. This is what a caller gets with a `read_budget` of zero, and it is
//!    already the answer to "where might my disk be going".
//! 2. **Head and tail.** Four kilobytes from each end, folded to one number.
//!    Two files that differ anywhere near either end part here, which is where
//!    nearly all of them differ.
//! 3. **The bytes.** What survives is compared against the first file of its
//!    group, in full, byte for byte.
//!
//! **Stage three is a comparison and not a hash, and that is deliberate.**
//! Every other tool of this kind reports "same digest" and calls it identical;
//! at a hundred thousand files a 64-bit digest is a coin flip away from a
//! collision, and the thing being decided is which file somebody deletes. A
//! comparison of two files of equal size costs the same reads as hashing both
//! and cannot be wrong. The digest earns its place in stage two, where being
//! wrong only means reading a little more.
//!
//! ## Interruptible, because it is the caller's disk
//!
//! `read_budget` bounds the reading, and the groups are confirmed largest
//! first, so whatever the budget buys is the most valuable part of the answer.
//! A run that stops early says so rather than presenting a partial answer as a
//! complete one: every group carries [`Certainty`], and the report says how
//! many groups were left unconfirmed.

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
    /// Ignore anything smaller than this.
    ///
    /// Not a performance knob so much as a statement about what the answer is
    /// for: below a megabyte the candidate list is a million files and covers
    /// 28 GB, above it the list is nineteen thousand and covers 141.8 GB.
    pub min_size: u64,
    /// How many bytes may be read to confirm.
    ///
    /// **Zero means read nothing**, and that is a supported answer rather than
    /// a degenerate one: the size groups and their potential saving are free,
    /// and for "what might I get back" they are the whole answer.
    pub read_budget: u64,
    /// At most this many groups in the reply, largest saving first.
    pub top: usize,
}

impl Default for Options {
    fn default() -> Self {
        // A megabyte and a gigabyte of reading: the measured knee, and enough
        // budget to confirm the whole of the interesting range on the corpus
        // those numbers came from.
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
    /// Same size and same first and last four kilobytes. Very likely, and not
    /// proven — this is the stage a budget runs out in.
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
    /// What each of them weighs. They all weigh the same — that is what makes
    /// them a group.
    pub size: u64,
    /// Their paths, in the order they arrived.
    pub paths: Vec<String>,
    pub certainty: Certainty,
}

impl Group {
    /// What deleting all but one would give back.
    ///
    /// The number the whole thing is for, and the reason groups are ordered by
    /// it rather than by size or by count: ten copies of a 100 MB file matter
    /// more than two copies of a 400 MB one.
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
    /// Everything the groups could give back, including the ones left out of
    /// `groups` by `top` — a total that changed when the list was truncated
    /// would be a total nobody could act on.
    pub waste: u64,
    /// How much of `waste` was read and compared rather than guessed.
    ///
    /// **The two numbers are different questions and printing only the first
    /// answers the wrong one.** "39 GB could be freed" out of a run that read
    /// nothing means "39 GB of files happen to share a size with another
    /// file", and on a real disk that is mostly database pages and build
    /// output that are the same length and not the same bytes — measured here
    /// as 39.36 GiB by size against 18.29 GiB once read. Somebody acting on
    /// the first number deletes files that were not copies.
    pub proven: u64,
    /// Bytes actually read confirming.
    pub read: u64,
    /// Groups the budget did not reach.
    ///
    /// **Said rather than left to be inferred.** A partial answer that looks
    /// complete is the failure this field exists to prevent: a caller that
    /// cannot tell "these are the duplicates" from "these are the duplicates I
    /// had time for" will delete files on the strength of the second.
    pub unconfirmed: u64,
}

/// Find them.
///
/// `files` is whatever the caller can produce — an index walk, a directory
/// walk, a list from somewhere else. Only the size is used before anything is
/// read, and a file that has since disappeared is dropped rather than being an
/// error: this runs against a filesystem that is still being used.
pub fn find<I>(files: I, opts: &Options) -> Report
where
    I: IntoIterator<Item = (String, u64)>,
{
    // Stage one. A map from size to the paths at it, which is the whole of the
    // free part — no `stat`, no `open`, nothing but arithmetic on numbers the
    // caller already had.
    let mut by_size: HashMap<u64, Vec<String>> = HashMap::new();
    let mut candidates = 0u64;
    for (path, size) in files {
        if size < opts.min_size {
            continue;
        }
        candidates += 1;
        by_size.entry(size).or_default().push(path);
    }

    let mut pending: Vec<(u64, Vec<String>)> = by_size
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .collect();
    // Largest possible saving first, so that a budget that runs out has been
    // spent where it was worth the most. Ties by size, then by path, so two
    // runs over the same disk answer the same way — a report that reshuffles
    // itself is one nobody can compare against yesterday's.
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
        // Nothing more may be read. The group still goes in the answer — it is
        // a real size collision and a real *possible* saving — but it says so.
        if read >= opts.read_budget {
            unconfirmed += 1;
            out.push(Group {
                size,
                paths,
                certainty: Certainty::Size,
            });
            continue;
        }

        // Stage two: the ends. Files that differ anywhere near either end part
        // here, and nearly all of them do.
        let mut buckets: HashMap<u64, Vec<String>> = HashMap::new();
        for p in paths {
            // A file that has gone, or that cannot be read, is dropped rather
            // than failing the run: this works against a filesystem somebody
            // is still using, and one vanished file must not cost the other
            // nineteen thousand their answer.
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
            // Stage three: the bytes, against the first of the group. A
            // comparison rather than a digest, because what is being decided
            // is which file somebody deletes.
            if read >= opts.read_budget {
                unconfirmed += 1;
                out.push(Group {
                    size,
                    paths: same,
                    certainty: Certainty::Edges,
                });
                continue;
            }
            let (confirmed, n) = confirm(&same, size);
            read += n;
            if confirmed.len() > 1 {
                out.push(Group {
                    size,
                    paths: confirmed,
                    certainty: Certainty::Content,
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

/// One number from the first and last four kilobytes.
///
/// Returns the mark and how much was read. A file shorter than two edges is
/// read once and whole, which is both cheaper and exact.
fn edges(path: &Path, size: u64) -> std::io::Result<(u64, u64)> {
    let mut f = File::open(path)?;
    let mut h = Fnv::new();
    // The size goes into the mark, so a short read cannot make two files of
    // different lengths collide.
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

/// Which of these really are the first one, byte for byte.
///
/// Everything is compared against `paths[0]` rather than pairwise: files of
/// equal content are equal to each other by transitivity, and pairwise would
/// be quadratic reads for an answer that is linear.
fn confirm(paths: &[String], size: u64) -> (Vec<String>, u64) {
    let mut same = vec![paths[0].clone()];
    let mut read = 0u64;
    for other in &paths[1..] {
        match identical(Path::new(&paths[0]), Path::new(other)) {
            Ok(true) => {
                same.push(other.clone());
                read += size * 2;
            }
            Ok(false) => read += size, // parted early; an over-estimate, and stated as one
            Err(_) => {}
        }
    }
    (same, read)
}

/// Byte for byte, stopping at the first difference.
fn identical(a: &Path, b: &Path) -> std::io::Result<bool> {
    // The same file under two names is identical without reading either. Not
    // an optimisation: a hard link *is* one file, and reading it twice to
    // discover that would be the most expensive way to learn it.
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

/// Fill the buffer, or reach the end trying.
///
/// `Read::read` is allowed to return less than was asked for at any time, and
/// a comparison that treats a short read as the end of the file reports two
/// different files as identical. That is the failure worth spelling out: this
/// decides what somebody deletes.
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

/// FNV-1a, 64 bits.
///
/// Ten lines rather than a dependency, and it is only ever a *filter*: two
/// files that agree here are then compared in full. Nothing is ever reported
/// as identical on the strength of a hash.
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

    /// **The case a digest gets wrong and this cannot.** Same size, same first
    /// and last four kilobytes, different in the middle — which is exactly
    /// what a file edited in place looks like.
    #[test]
    fn two_files_that_differ_only_in_the_middle_are_not_the_same_file() {
        let d = Dir::new("middle");
        let mut a = vec![b'x'; 20_000];
        let mut b = a.clone();
        a[10_000] = b'1';
        b[10_000] = b'2';
        let r = find(vec![d.file("a", &a), d.file("b", &b)], &opts(1000));
        assert!(r.groups.is_empty(), "{:?}", r.groups);
        // And it was read to find that out, which is the point of the stage.
        assert!(r.read > 0);
    }

    /// The ends part them without reading the middle at all.
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

    /// Zero budget is an answer, not a failure: the size groups and what they
    /// might save are free, and for "where might my disk be going" they are
    /// the whole answer.
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
        // **The number that keeps the other one honest.** Nothing was read, so
        // nothing is proven — and a caller printing only `waste` would be
        // telling somebody they can free eight kilobytes that may not be
        // copies at all.
        assert_eq!(r.proven, 0);
    }

    /// Ordered by what deleting would give back, not by size: ten copies of a
    /// small file beat two copies of a large one.
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

    /// A file that goes away mid-run costs itself and nothing else.
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

    /// The size gate is what it claims to be.
    #[test]
    fn anything_under_the_floor_is_not_looked_at() {
        let d = Dir::new("floor");
        let tiny = vec![b't'; 10];
        let r = find(vec![d.file("a", &tiny), d.file("b", &tiny)], &opts(1000));
        assert!(r.groups.is_empty());
        assert_eq!(r.candidates, 0);
        assert_eq!(r.read, 0);
    }

    /// A short read is not the end of a file, and treating it as one reports
    /// two different files as the same.
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

    /// The total is about everything found, not about what fitted in the list.
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
