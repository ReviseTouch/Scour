//! The rows of a segment, in path order.
//!
//! No stored number bounds a path and the directory is front-coded, so building
//! a path key costs two reads a matching row — 31 ms of walk became 202 for a
//! page of 200 over 2,234,583 rows. Stored instead: four bytes a row.

use std::cmp::Ordering;

use crate::dirs::DirTable;

/// The rows of one segment in ascending path order: a count, then one 32-bit
/// row number per row. Positions are the segment's own, so a merge across
/// segments still compares real paths.
#[derive(Debug, Clone, Copy)]
pub struct PathOrder<'a> {
    rows: usize,
    rows_bytes: &'a [u8],
}

impl<'a> PathOrder<'a> {
    /// Open the file, or refuse it. The length is exact — the file is written
    /// whole — while an *absent* one is [`crate::Live`]'s distinction to make.
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

    /// The row whose path is `i`-th in ascending order. A row number this
    /// segment does not hold answers `None` rather than reaching a column read.
    pub fn at(&self, i: usize) -> Option<u32> {
        let at = i.checked_mul(4)?;
        let row = u32::from_le_bytes(self.rows_bytes.get(at..at + 4)?.try_into().ok()?);
        ((row as usize) < self.rows).then_some(row)
    }
}

/// Order a segment's rows by the path each one spells; `names` is the spelled
/// arena, NUL-terminated in row order. Compared as a joined path, not as a
/// pair: `-` sorts before `/`, so `/a-x/b` < `/a/c` but `("/a","c")` is less.
pub fn build(dirs: &DirTable<'_>, dir_of: &[u32], names: &[u8]) -> Vec<u8> {
    let rows = dir_of.len();
    // What a row's name is appended to, decoded once rather than per comparison.
    let (prefix, prefix_at) = dirs.join_prefixes();
    let name_at = name_offsets(rows, names);

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
            // One path under two sources is two rows; the row number makes the
            // order total, and newest-first is the tie-break `sort_hits` takes.
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
            // A short arena: the rows left over get empty names and sort first;
            // the caller's own validation is what refuses the segment.
            None => break,
        }
    }
    at.resize(rows, cursor as u32);
    at.push(cursor as u32);
    at
}

/// Compare `ha + na` with `hb + nb` without joining. Where one head is a prefix
/// of the other, the shorter row's *name* stands where the longer's head does.
fn joined(ha: &[u8], na: &[u8], hb: &[u8], nb: &[u8]) -> Ordering {
    let m = ha.len().min(hb.len());
    match ha[..m].cmp(&hb[..m]) {
        Ordering::Equal => {}
        o => return o,
    }
    match ha.len().cmp(&hb.len()) {
        Ordering::Equal => na.cmp(nb),
        // What is left of `hb` stands where `na` does; a name holds no
        // separator and a head ends with one, so these are never equal.
        Ordering::Less => na.cmp(&hb[m..]),
        Ordering::Greater => ha[m..].cmp(nb),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dirs::DirWriter;
    use crate::names::NameWriter;

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

    /// `-` is 0x2D and `/` is 0x2F, so `/a-x/b` comes before `/a/c`.
    #[test]
    fn a_dash_in_a_folder_name_sorts_before_the_separator() {
        agrees(&["/a/c", "/a-x/b"]);
        agrees(&["/p/a-b/x", "/p/a/z", "/p/a.b/y", "/p/a0/w"]);
    }

    #[test]
    fn a_folder_and_its_children_interleave_by_name() {
        agrees(&["/a/b", "/a/b/c", "/a/a", "/a/c"]);
        agrees(&["/a/m", "/a/m/n", "/a/m0/n"]);
    }

    #[test]
    fn two_rows_at_one_path_keep_the_newer_row_first() {
        // One path under two sources; the row number makes the order total.
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
        assert!(PathOrder::open(&[9, 0, 0, 0]).is_none());
    }

    #[test]
    fn a_position_naming_a_row_the_segment_lacks_answers_nothing() {
        // Right length, wrong contents: a row number out of range must not
        // reach a column read.
        let blob = [1u8, 0, 0, 0, 7, 0, 0, 0];
        let order = PathOrder::open(&blob).expect("order");
        assert_eq!(order.rows(), 1);
        assert_eq!(order.at(0), None);
    }
}
