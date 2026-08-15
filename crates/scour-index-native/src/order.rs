//! The rows of a segment, in path order.
//!
//! Rows are stored newest-first, which is what makes "the newest two hundred"
//! a read of two hundred rows. Every other order has to earn its page some
//! other way, and each of them does: a number is bounded by the zone map, a
//! name is walked out of the folded arena at gigabytes a second. **Ordering by
//! path had neither.**
//!
//! It could not use the zone map, because no stored number bounds a path. It
//! could not use the arena either, because a path is a *directory joined to a
//! name* and the directory is front-coded — so the key had to be built, per
//! matching row, out of two reads. Measured on 2,234,583 rows, whole table,
//! page of two hundred: the walk alone was 31 ms, reading each row's spelled
//! name took it to 102, resolving that row's directory took it to 202, and
//! joining and comparing was free. Two reads a match, and no cleverness in the
//! key can remove them — an abbreviated key was tried twice and is refused at
//! [`crate::search::key_is_exact`].
//!
//! So the order is stored. Four bytes a row, written when the segment is built
//! — where the directory table has just been sorted and every name is in hand,
//! so the ordering costs one sort of what is already in memory rather than a
//! second pass over the corpus. Ordering by path is then the same shape as
//! ordering by date: read positions until the page is full and stop.
//!
//! ## Why not a rank a row
//!
//! The obvious column is the inverse of this one — a row's *position* in path
//! order — and it fits the existing machinery exactly: another [`crate::Field`],
//! another zone map, `sort_field` returns it and everything else is untouched.
//! It was rejected because the zone map cannot read it. A block holds
//! thirty-two rows adjacent in *date* order, and their positions in path order
//! are thirty-two numbers scattered across the whole corpus — so every block's
//! recorded range is very nearly the whole range, and a range that admits
//! everything skips nothing. It would have turned a built key into a column
//! read, which is worth having, and left the walk visiting every row.
//!
//! This direction is read the other way round and needs no bound at all: the
//! positions *are* the order, so the walk stops when the page is full.
//!
//! ## What it does not do
//!
//! Positions are a segment's own. Two segments both number their rows from
//! zero, so a position says nothing across them, and the merge in
//! `index.rs` still compares real paths. That costs what it always did and no
//! more, because it only ever sees the candidates — a page a segment, not a
//! corpus. See the note at the end of `run_with`.

use std::cmp::Ordering;

use crate::dirs::DirTable;

/// The rows of one segment in ascending path order.
///
/// Read in place out of a mapped file: a count, then one 32-bit row number per
/// row, in the order the paths sort.
#[derive(Debug, Clone, Copy)]
pub struct PathOrder<'a> {
    rows: usize,
    rows_bytes: &'a [u8],
}

impl<'a> PathOrder<'a> {
    /// Open the file, or refuse it.
    ///
    /// The length is checked exactly rather than loosely, which is the same
    /// standard the live bitmap is held to and for the same reason: this file
    /// is written whole, so a length that does not say so is damage rather
    /// than a shorter answer. A file that is *absent* means something quite
    /// different — a segment written before this existed — and that is
    /// [`crate::Live`]'s distinction to make, not this one's.
    pub fn open(bytes: &'a [u8]) -> Option<PathOrder<'a>> {
        let rows = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
        if bytes.len() != 4usize.checked_add(rows.checked_mul(4)?)? {
            return None;
        }
        Some(PathOrder {
            rows,
            rows_bytes: &bytes[4..],
        })
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// The row whose path is `i`-th in ascending order.
    ///
    /// A row number this segment does not hold answers `None` rather than
    /// being handed on. The check is one comparison against a value already in
    /// a register, and it is what keeps a damaged file from reaching
    /// [`crate::search::Segment`] as an index into a column.
    pub fn at(&self, i: usize) -> Option<u32> {
        let at = i.checked_mul(4)?;
        let row = u32::from_le_bytes(self.rows_bytes.get(at..at + 4)?.try_into().ok()?);
        ((row as usize) < self.rows).then_some(row)
    }
}

/// Order a segment's rows by the path each one spells.
///
/// `dir_of` is the directory number of every row, already remapped to the
/// written table, and `names` is the spelled arena as [`crate::NameWriter`]
/// holds it before packing: every name NUL-terminated, in row order.
///
/// **The comparison is on the joined path and not on the pair.** `("/a", "c")`
/// is less than `("/a-x", "b")` as a pair and `/a-x/b` is less than `/a/c` as a
/// path, because `-` sorts before `/` — so a sort by directory and then by name
/// is a different order, and the merge, `sort_hits` and the brute-force
/// reference all use the path. The same trap is written down at
/// `index.rs::joined_path`.
pub fn build(dirs: &DirTable<'_>, dir_of: &[u32], names: &[u8]) -> Vec<u8> {
    let rows = dir_of.len();
    // What each row's name is appended to, decoded once for the whole table
    // rather than once per comparison. See `DirTable::join_prefixes`.
    let (prefix, prefix_at) = dirs.join_prefixes();
    let name_at = name_offsets(rows, names);

    // A name lives between its start and the NUL before the next one.
    let name = |row: u32| -> &[u8] {
        let (a, b) = (
            name_at[row as usize] as usize,
            name_at[row as usize + 1] as usize,
        );
        names.get(a..b.saturating_sub(1)).unwrap_or_default()
    };
    let head = |row: u32| -> &[u8] {
        let d = dir_of[row as usize] as usize;
        match (prefix_at.get(d), prefix_at.get(d + 1)) {
            (Some(&a), Some(&b)) => prefix.get(a as usize..b as usize).unwrap_or_default(),
            _ => b"",
        }
    };

    let mut order: Vec<u32> = (0..rows as u32).collect();
    order.sort_unstable_by(|&a, &b| {
        joined(head(a), name(a), head(b), name(b))
            // Two rows can spell the same path — one path under two sources is
            // two rows — and a build has to be reproducible, so the tie is
            // broken by something stored. The row number is also the right
            // answer: rows are written newest-first, and `sort_hits` breaks a
            // tie on the path by taking the newest first.
            .then(a.cmp(&b))
    });

    let mut out = Vec::with_capacity(4 + rows * 4);
    out.extend_from_slice(&(rows as u32).to_le_bytes());
    for row in order {
        out.extend_from_slice(&row.to_le_bytes());
    }
    out
}

/// Where each name begins in the arena, with a final entry at the end.
fn name_offsets(rows: usize, names: &[u8]) -> Vec<u32> {
    let mut at = Vec::with_capacity(rows + 1);
    let mut cursor = 0usize;
    for _ in 0..rows {
        at.push(cursor as u32);
        match memchr::memchr(0, names.get(cursor..).unwrap_or_default()) {
            Some(n) => cursor += n + 1,
            // A short arena is not something to guess about: the rows left
            // over get empty names and sort first, and the caller's own
            // validation is what refuses the segment.
            None => break,
        }
    }
    at.resize(rows, cursor as u32);
    at.push(cursor as u32);
    at
}

/// Compare `ha + na` with `hb + nb` without joining either.
///
/// `h` is what the name is appended to — the directory and its separator, or
/// nothing at the root. Three cases, and the third is the one that makes this
/// worth writing out: when one head is a prefix of the other, the shorter row's
/// *name* is what the longer row still has to spell. `/a` holding `b` against
/// `/a/b` holding `c` is `/a/b` against `/a/b/c`, and the answer comes from
/// comparing `b` with `b/`.
fn joined(ha: &[u8], na: &[u8], hb: &[u8], nb: &[u8]) -> Ordering {
    let m = ha.len().min(hb.len());
    match ha[..m].cmp(&hb[..m]) {
        Ordering::Equal => {}
        o => return o,
    }
    match ha.len().cmp(&hb.len()) {
        Ordering::Equal => na.cmp(nb),
        // `ha` ran out first, so what is left of `hb` stands where `na` does.
        // A name holds no separator and a head ends with one, so these can
        // never be equal.
        Ordering::Less => na.cmp(&hb[m..]),
        Ordering::Greater => ha[m..].cmp(nb),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dirs::DirWriter;
    use crate::names::NameWriter;

    /// Build the order for a list of paths and read it back as paths.
    fn ordered(paths: &[&str]) -> Vec<String> {
        let mut dirs = DirWriter::new();
        let mut names = NameWriter::new();
        let mut provisional = Vec::new();
        for p in paths {
            let (parent, name) = crate::search::Segment::split_path(p);
            provisional.push(dirs.intern(parent));
            names.push(name);
        }
        let (bytes, remap) = dirs.finish();
        let dir_of: Vec<u32> = provisional.iter().map(|&p| remap[p as usize]).collect();
        let table = DirTable::open(&bytes).expect("table");
        let blob = build(&table, &dir_of, names.spelled());
        let order = PathOrder::open(&blob).expect("order");
        assert_eq!(order.rows(), paths.len());
        (0..order.rows())
            .map(|i| paths[order.at(i).expect("row") as usize].to_owned())
            .collect()
    }

    /// The order has to be the one `str::cmp` gives, on every corpus.
    fn agrees(paths: &[&str]) {
        let mut want: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
        want.sort();
        assert_eq!(ordered(paths), want, "for {paths:?}");
    }

    #[test]
    fn the_order_is_the_order_strings_have() {
        agrees(&["/a/c.rs", "/b/a.rs", "/a/b.rs"]);
        agrees(&["/home/u/rapor.pdf", "/home/u/belge.txt", "/home/u/.bashrc"]);
        agrees(&["/lonely.txt", "/a/x", "/z/y"]);
    }

    /// The case a sort by directory and then by name gets wrong.
    ///
    /// `-` is 0x2D and `/` is 0x2F, so `/a-x/b` comes before `/a/c` even though
    /// the directory `/a` comes before `/a-x`. This is the whole reason
    /// [`joined`] exists.
    #[test]
    fn a_dash_in_a_folder_name_sorts_before_the_separator() {
        agrees(&["/a/c", "/a-x/b"]);
        agrees(&["/p/a-b/x", "/p/a/z", "/p/a.b/y", "/p/a0/w"]);
    }

    /// A directory's own files interleave with what is under its children.
    #[test]
    fn a_folder_and_its_children_interleave_by_name() {
        agrees(&["/a/b", "/a/b/c", "/a/a", "/a/c"]);
        agrees(&["/a/m", "/a/m/n", "/a/m0/n"]);
    }

    #[test]
    fn two_rows_at_one_path_keep_the_newer_row_first() {
        // Same path twice — one path under two sources. The order has to be
        // total and reproducible, and the row number is what makes it so.
        let out = ordered(&["/a/x", "/a/x", "/a/w"]);
        assert_eq!(out, ["/a/w", "/a/x", "/a/x"]);
    }

    #[test]
    fn an_empty_segment_has_an_empty_order() {
        let blob = build(
            &DirTable::open(&DirWriter::new().finish().0).expect("table"),
            &[],
            b"",
        );
        let order = PathOrder::open(&blob).expect("order");
        assert!(order.is_empty());
        assert_eq!(order.at(0), None);
    }

    #[test]
    fn a_file_of_the_wrong_length_is_refused_rather_than_read() {
        let mut blob = build(
            &DirTable::open(&DirWriter::new().finish().0).expect("table"),
            &[],
            b"",
        );
        blob.push(0);
        assert!(PathOrder::open(&blob).is_none());
        assert!(PathOrder::open(&[]).is_none());
        // A count that claims more rows than the file can hold.
        assert!(PathOrder::open(&[9, 0, 0, 0]).is_none());
    }

    #[test]
    fn a_position_naming_a_row_the_segment_lacks_answers_nothing() {
        // Right length, wrong contents. Nothing here can tell that apart from
        // a good file — the parts of a segment are not checksummed — but a row
        // number out of range must not reach a column read.
        let blob = [1u8, 0, 0, 0, 7, 0, 0, 0];
        let order = PathOrder::open(&blob).expect("order");
        assert_eq!(order.rows(), 1);
        assert_eq!(order.at(0), None);
    }
}
