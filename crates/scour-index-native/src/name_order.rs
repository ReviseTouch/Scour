//! The rows of a segment, in folded-name order.
//!
//! Names are not a numeric column, so a page sorted by name used to walk every
//! matching row before it could decide which rows won. The folded name arena
//! already holds the exact comparison bytes; this file stores the result of
//! sorting those bytes once, when the segment is built.
//!
//! The row list costs four bytes per row. One additional bit marks the start
//! of each equal-name group. That bit is what makes descending order correct:
//! reversing the whole row list would also reverse the newest-first/path-first
//! tie order, while Scour deliberately keeps that tie order in both directions.

/// The rows of one segment in ascending folded-name order.
///
/// Read in place out of a mapped file: a count, one 32-bit row number per row,
/// then one bit per position marking the start of an equal-name group.
#[derive(Debug, Clone, Copy)]
pub struct NameOrder<'a> {
    rows: usize,
    rows_bytes: &'a [u8],
    groups: &'a [u8],
}

impl<'a> NameOrder<'a> {
    /// Open the file, or refuse a malformed one.
    ///
    /// Absence is handled by the segment reader as a legacy segment. A file
    /// that is present has to describe all of its rows exactly.
    pub fn open(bytes: &'a [u8]) -> Option<NameOrder<'a>> {
        let rows = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
        let order_len = rows.checked_mul(4)?;
        let groups_len = rows.div_ceil(8);
        let end = 4usize.checked_add(order_len)?.checked_add(groups_len)?;
        if bytes.len() != end {
            return None;
        }
        let rows_bytes = &bytes[4..4 + order_len];
        let groups = &bytes[4 + order_len..];
        if rows > 0 && groups.first().is_none_or(|byte| byte & 1 == 0) {
            return None;
        }
        Some(NameOrder {
            rows,
            rows_bytes,
            groups,
        })
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// The row whose folded name is `i`-th in ascending order.
    pub fn at(&self, i: usize) -> Option<u32> {
        let at = i.checked_mul(4)?;
        let row = u32::from_le_bytes(self.rows_bytes.get(at..at + 4)?.try_into().ok()?);
        ((row as usize) < self.rows).then_some(row)
    }

    /// The start of the group containing position `i`.
    ///
    /// Descending text order reads primary-key groups from the end while
    /// retaining forward row order inside each group. Looking one bit at a
    /// time makes a million-row tie a million branches before the first page;
    /// scanning the compact boundary bytes keeps that case cheap.
    pub(crate) fn group_at_or_before(&self, i: usize) -> Option<usize> {
        if i >= self.rows {
            return None;
        }
        let byte = i / 8;
        let bit = i % 8;
        let mask = if bit == 7 {
            u8::MAX
        } else {
            (1u8 << (bit + 1)) - 1
        };
        let here = self.groups.get(byte).copied().unwrap_or(0) & mask;
        if here != 0 {
            return Some(byte * 8 + (7 - here.leading_zeros() as usize));
        }
        self.groups[..byte]
            .iter()
            .rposition(|&bits| bits != 0)
            .map(|at| at * 8 + (7 - self.groups[at].leading_zeros() as usize))
    }
}

/// Order rows by their already-folded names.
///
/// `names` is the unpacked, NUL-terminated folded arena held by
/// [`crate::NameWriter`] while a segment is being built. Rows already have the
/// stored newest-first/path-first order, so the row number is the exact tie
/// breaker used by a name sort.
#[cfg(test)]
fn build(rows: usize, names: &[u8], order: Vec<u32>) -> Vec<u8> {
    build_reusing(rows, names, order).0
}

/// Build the name order and return its row-list allocation for another order.
///
/// A full rebuild has one `u32` scratch slot per row already. Returning it
/// after serialisation lets the extension order reuse those same bytes instead
/// of adding another four bytes per row to peak memory.
pub(crate) fn build_reusing(rows: usize, names: &[u8], order: Vec<u32>) -> (Vec<u8>, Vec<u32>) {
    build_keyed(rows, names, order, |name| name, None)
}

/// Build this grouped-row format using one derived key per name.
///
/// When `eligible` is present, a clear row bit gives that row an empty key.
/// The extension order uses this to remember eligibility derived from the raw
/// spelling while comparing bytes derived from the folded spelling, without
/// keeping a second four-byte offset table during a rebuild.
pub(crate) fn build_keyed(
    rows: usize,
    names: &[u8],
    mut order: Vec<u32>,
    key: for<'b> fn(&'b [u8]) -> &'b [u8],
    eligible: Option<&[u8]>,
) -> (Vec<u8>, Vec<u32>) {
    let name_at = name_offsets(rows, names);
    let name = |row: u32| -> &[u8] {
        let (a, b) = (
            name_at[row as usize] as usize,
            name_at[row as usize + 1] as usize,
        );
        names.get(a..b.saturating_sub(1)).unwrap_or_default()
    };
    let value = |row: u32| {
        let allowed = eligible.is_none_or(|bits| {
            bits.get(row as usize / 8)
                .is_some_and(|byte| byte & (1u8 << (row as usize % 8)) != 0)
        });
        if allowed { key(name(row)) } else { b"" }
    };

    // The segment builder hands over its directory-number scratch here. It is
    // dead after the columns have been written and already has one `u32` of
    // capacity per row, so reusing it avoids another four-byte-per-row peak
    // during a full rebuild.
    order.clear();
    order.extend(0..rows as u32);
    order.sort_unstable_by(|&a, &b| value(a).cmp(value(b)).then(a.cmp(&b)));

    let mut groups = vec![0u8; rows.div_ceil(8)];
    for i in 0..rows {
        if (i == 0 || value(order[i - 1]) != value(order[i]))
            && let Some(byte) = groups.get_mut(i / 8)
        {
            *byte |= 1 << (i % 8);
        }
    }

    let mut out = Vec::with_capacity(4 + rows * 4 + groups.len());
    out.extend_from_slice(&(rows as u32).to_le_bytes());
    for &row in &order {
        out.extend_from_slice(&row.to_le_bytes());
    }
    out.extend_from_slice(&groups);
    (out, order)
}

/// Where each NUL-terminated name begins, with one final end position.
fn name_offsets(rows: usize, names: &[u8]) -> Vec<u32> {
    let mut at = Vec::with_capacity(rows + 1);
    let mut cursor = 0usize;
    for _ in 0..rows {
        at.push(cursor as u32);
        match memchr::memchr(0, names.get(cursor..).unwrap_or_default()) {
            Some(n) => cursor += n + 1,
            None => break,
        }
    }
    at.resize(rows, cursor as u32);
    at.push(cursor as u32);
    at
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::NameWriter;

    fn order(names: &[&str]) -> (Vec<usize>, Vec<bool>) {
        let mut writer = NameWriter::new();
        for name in names {
            writer.push(name);
        }
        let blob = build(writer.len(), writer.folded(), Vec::new());
        let stored = NameOrder::open(&blob).expect("name order");
        let rows = (0..stored.rows())
            .map(|i| stored.at(i).expect("row") as usize)
            .collect();
        let groups = (0..stored.rows())
            .map(|i| stored.group_at_or_before(i) == Some(i))
            .collect();
        (rows, groups)
    }

    #[test]
    fn names_are_ordered_by_the_same_folded_bytes_searches_use() {
        let names = ["z.txt", "B.txt", "a.txt", "b.TXT", "İstanbul.txt"];
        let (rows, _) = order(&names);
        let got: Vec<&str> = rows.into_iter().map(|row| names[row]).collect();
        assert_eq!(got, ["a.txt", "B.txt", "b.TXT", "İstanbul.txt", "z.txt"]);
    }

    #[test]
    fn equal_folded_names_keep_row_order_and_mark_one_group() {
        let (rows, groups) = order(&["B.txt", "b.TXT", "a.txt", "C.txt"]);
        assert_eq!(rows, [2, 0, 1, 3]);
        assert_eq!(groups, [true, true, false, true]);
    }

    #[test]
    fn malformed_files_are_refused() {
        let (rows, _) = order(&["one"]);
        assert_eq!(rows, [0]);
        assert!(NameOrder::open(&[]).is_none());
        assert!(NameOrder::open(&[9, 0, 0, 0]).is_none());
        // One row and a valid row number, but no first-group marker.
        assert!(NameOrder::open(&[1, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
    }

    #[test]
    fn an_empty_order_is_readable() {
        let blob = build(0, b"", Vec::new());
        let stored = NameOrder::open(&blob).expect("empty order");
        assert!(stored.is_empty());
        assert_eq!(stored.at(0), None);
    }
}
