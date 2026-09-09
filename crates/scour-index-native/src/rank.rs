//! Reaching a row instead of walking to it.
//!
//! `Mtime` never rises with the row number, so "the first row not newer than
//! *t*" is a binary search over a column, not a walk. The live rank is one
//! popcount per eight rows, built per request: a stale one pages from nowhere.

/// Rows between two entries of the prefix: 4 bytes per 512 rows, about 20 KB
/// for this index, and a handful of popcounts after the last entry.
const STRIDE: usize = 512;

/// How many rows are live before each `STRIDE`-th row.
pub struct LiveRank {
    prefix: Vec<u32>,
    rows: usize,
    /// An absent bitmap means nothing was ever deleted; every row is live.
    all_live: bool,
}

impl LiveRank {
    /// Count the live rows of a segment, in strides.
    pub fn build(alive: &[u8], rows: usize) -> LiveRank {
        if alive.is_empty() {
            return LiveRank {
                prefix: Vec::new(),
                rows,
                all_live: true,
            };
        }
        let mut prefix = Vec::with_capacity(rows.div_ceil(STRIDE));
        let mut running = 0u32;
        for chunk in alive.chunks(STRIDE / 8) {
            prefix.push(running);
            running += chunk.iter().map(|b| b.count_ones()).sum::<u32>();
        }
        LiveRank {
            prefix,
            rows,
            all_live: false,
        }
    }

    /// How many of the rows before `row` are live.
    pub fn upto(&self, alive: &[u8], row: usize) -> usize {
        let row = row.min(self.rows);
        if self.all_live {
            return row;
        }
        let entry = row / STRIDE;
        let mut count = match self.prefix.get(entry) {
            Some(n) => *n as usize,
            // Past the last stride the table already holds everything.
            None => return self.prefix.last().map_or(0, |n| *n as usize) + self.tail(alive, row),
        };
        let from = entry * STRIDE;
        count += ones(alive, from, row);
        count
    }

    /// Live rows in the whole segment.
    pub fn total(&self, alive: &[u8]) -> usize {
        self.upto(alive, self.rows)
    }

    fn tail(&self, alive: &[u8], row: usize) -> usize {
        let from = self.prefix.len().saturating_sub(1) * STRIDE;
        ones(alive, from, row)
    }
}

/// Live rows in `from..to`, counted a byte at a time where the range is whole
/// bytes and a bit at a time at the two ends.
fn ones(alive: &[u8], from: usize, to: usize) -> usize {
    if from >= to {
        return 0;
    }
    let mut count = 0usize;
    let first_whole = from.div_ceil(8);
    let last_whole = to / 8;
    for row in from..(first_whole * 8).min(to) {
        count += usize::from(bit(alive, row));
    }
    if first_whole < last_whole {
        for byte in &alive[first_whole.min(alive.len())..last_whole.min(alive.len())] {
            count += byte.count_ones() as usize;
        }
    }
    for row in (last_whole * 8).max(from)..to {
        count += usize::from(bit(alive, row));
    }
    count
}

fn bit(alive: &[u8], row: usize) -> bool {
    match alive.get(row / 8) {
        Some(byte) => byte & (1 << (row % 8)) != 0,
        None => false,
    }
}

/// The first row whose value is not above `bound`, in a column that never rises
/// with the row number; `rows` when none is. `at` is a closure so this owes
/// nothing to how a segment is opened.
pub fn first_at_or_below(rows: usize, bound: i64, at: impl Fn(usize) -> i64) -> usize {
    let mut lo = 0usize;
    let mut hi = rows;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if at(mid) > bound {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(bits: &[bool]) -> Vec<u8> {
        let mut out = vec![0u8; bits.len().div_ceil(8)];
        for (i, on) in bits.iter().enumerate() {
            if *on {
                out[i / 8] |= 1 << (i % 8);
            }
        }
        out
    }

    #[test]
    fn a_rank_counts_what_a_walk_would() {
        // Longer than one stride, where the prefix stops being the whole answer.
        let rows = STRIDE * 3 + 137;
        let bits: Vec<bool> = (0..rows).map(|i| i % 3 != 0).collect();
        let alive = bitmap(&bits);
        let rank = LiveRank::build(&alive, rows);
        let mut walked = 0usize;
        for (row, live) in bits.iter().enumerate() {
            assert_eq!(rank.upto(&alive, row), walked, "at row {row}");
            walked += usize::from(*live);
        }
        assert_eq!(rank.upto(&alive, rows), walked, "and one past the last row");
        assert_eq!(rank.total(&alive), walked);
    }

    #[test]
    fn an_absent_bitmap_means_nothing_has_died() {
        // The rank must agree with `Segment::is_alive` on an empty slice.
        let rank = LiveRank::build(&[], 1_000);
        assert_eq!(rank.upto(&[], 0), 0);
        assert_eq!(rank.upto(&[], 640), 640);
        assert_eq!(rank.total(&[]), 1_000);
    }

    #[test]
    fn a_row_past_the_end_counts_the_whole_segment() {
        let rows = 20;
        let alive = bitmap(&vec![true; rows]);
        let rank = LiveRank::build(&alive, rows);
        assert_eq!(rank.upto(&alive, 9_999), rows);
    }

    #[test]
    fn the_boundary_is_the_first_row_not_above_it() {
        // Equal values in a run: an off-by-one here starts the page one row late.
        let column = [90i64, 80, 70, 70, 70, 60, 50];
        let at = |i: usize| column[i];
        assert_eq!(first_at_or_below(column.len(), 100, at), 0);
        assert_eq!(first_at_or_below(column.len(), 90, at), 0);
        assert_eq!(first_at_or_below(column.len(), 85, at), 1);
        assert_eq!(
            first_at_or_below(column.len(), 70, at),
            2,
            "the first of the group"
        );
        assert_eq!(first_at_or_below(column.len(), 69, at), 5, "past all of it");
        assert_eq!(first_at_or_below(column.len(), 50, at), 6);
        assert_eq!(first_at_or_below(column.len(), 49, at), 7, "none of them");
        assert_eq!(first_at_or_below(0, 50, at), 0, "and an empty segment");
    }
}
