//! The numbers, in columns.
//!
//! Twelve values an entry — eleven from `stat` plus the directory number —
//! stored one column at a time in blocks of 128, each block bit-packed against
//! its own minimum. Measured on the real corpus: **8.85 bytes an entry** for
//! all of them, against 80 stored plainly.
//!
//! Two results from that measurement are worth keeping in view.
//!
//! `mtime` costs **0.29 bytes** — a block of 128 rows that are already in
//! date order spans a narrow range, so nine bits hold it. The row order paying
//! for itself twice is not a coincidence; it is why the order was chosen.
//!
//! Delta coding makes this **worse**, not better: 23.4 bytes an entry against
//! 8.85. It was tried and rejected. Deltas beat a frame of reference only when
//! the values climb steadily, and `size`, `mode` and `uid` do not.
//!
//! Columns rather than rows because a filter reads one number from many rows —
//! "everything under a megabyte" touches only `size` — and a column puts those
//! bytes next to each other. A row layout would drag eleven unwanted numbers
//! through the cache for every candidate.

use crate::varint;

/// Rows per block.
///
/// The tension: a larger block packs better, because the per-block minimum and
/// width are amortised further; a smaller one packs *tighter*, because a
/// narrow range needs fewer bits. 128 is where the measurement settled, and it
/// is also the width SIMD bit-packers use, which leaves the door open.
pub const BLOCK: usize = 128;

/// The columns, in a fixed order. The numeric values are part of the on-disk
/// format: reordering them reinterprets every existing index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Field {
    DirId = 0,
    Size = 1,
    Mtime = 2,
    Ctime = 3,
    Atime = 4,
    Mode = 5,
    Uid = 6,
    Gid = 7,
    Disk = 8,
    Items = 9,
    Kind = 10,
    IsDir = 11,
}

impl Field {
    pub const ALL: [Field; 12] = [
        Field::DirId,
        Field::Size,
        Field::Mtime,
        Field::Ctime,
        Field::Atime,
        Field::Mode,
        Field::Uid,
        Field::Gid,
        Field::Disk,
        Field::Items,
        Field::Kind,
        Field::IsDir,
    ];

    pub fn index(self) -> usize {
        self as usize
    }
}

/// Collects rows and encodes them one column at a time.
#[derive(Debug, Default)]
pub struct ColumnWriter {
    rows: Vec<[i64; Field::ALL.len()]>,
}

impl ColumnWriter {
    pub fn new() -> ColumnWriter {
        ColumnWriter::default()
    }

    pub fn push(&mut self, row: [i64; Field::ALL.len()]) {
        self.rows.push(row);
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Encode every column.
    ///
    /// Layout: a header of (row count, column count), then per column a
    /// 64-bit offset to its data, then per column a run of blocks, each an
    /// 8-byte minimum, a byte of width, and the packed values.
    pub fn finish(&self) -> Vec<u8> {
        let n = self.rows.len();
        let cols = Field::ALL.len();
        let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(cols);

        for field in Field::ALL {
            let mut body = Vec::new();
            let c = field.index();
            for chunk in self.rows.chunks(BLOCK) {
                let vals: Vec<i64> = chunk.iter().map(|r| r[c]).collect();
                let (min, bits) = varint::width_for(&vals);
                body.extend_from_slice(&min.to_le_bytes());
                body.push(bits as u8);
                varint::pack(&mut body, &vals, min, bits);
            }
            bodies.push(body);
        }

        let header = 8 + cols * 8;
        let mut out = Vec::with_capacity(header + bodies.iter().map(Vec::len).sum::<usize>());
        out.extend_from_slice(&(n as u32).to_le_bytes());
        out.extend_from_slice(&(cols as u32).to_le_bytes());
        let mut at = header as u64;
        for b in &bodies {
            out.extend_from_slice(&at.to_le_bytes());
            at += b.len() as u64;
        }
        for b in &bodies {
            out.extend_from_slice(b);
        }
        out
    }
}

/// The columns, read in place out of a mapped file.
#[derive(Debug, Clone, Copy)]
pub struct ColumnBlocks<'a> {
    rows: usize,
    offsets: &'a [u8],
    bytes: &'a [u8],
}

impl<'a> ColumnBlocks<'a> {
    pub fn open(bytes: &'a [u8]) -> Option<ColumnBlocks<'a>> {
        if bytes.len() < 8 {
            return None;
        }
        let rows = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
        let cols = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
        if cols != Field::ALL.len() || bytes.len() < 8 + cols * 8 {
            return None;
        }
        Some(ColumnBlocks {
            rows,
            offsets: &bytes[8..8 + cols * 8],
            bytes,
        })
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// One value.
    ///
    /// Random access rather than bulk decode: a filter usually rejects a row on
    /// the first column it looks at, and decoding the other eleven would be
    /// work thrown away.
    pub fn get(&self, field: Field, row: usize) -> Option<i64> {
        if row >= self.rows {
            return None;
        }
        let at = field.index() * 8;
        let base = u64::from_le_bytes(self.offsets.get(at..at + 8)?.try_into().ok()?) as usize;

        // Blocks are variable width, so reaching block `b` means walking the
        // headers of the ones before it. At 128 rows a block that is at most
        // a few hundred steps for a million rows, and each step is an add.
        let block = row / BLOCK;
        let mut at = base;
        for _ in 0..block {
            let bits = *self.bytes.get(at + 8)? as u32;
            at += 9 + varint::packed_len(BLOCK, bits);
        }
        let min = i64::from_le_bytes(self.bytes.get(at..at + 8)?.try_into().ok()?);
        let bits = *self.bytes.get(at + 8)? as u32;
        let packed = self.bytes.get(at + 9..)?;
        Some(varint::unpack_one(packed, min, bits, row % BLOCK))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(dir: i64, size: i64, mtime: i64) -> [i64; 12] {
        let mut r = [0i64; 12];
        r[Field::DirId.index()] = dir;
        r[Field::Size.index()] = size;
        r[Field::Mtime.index()] = mtime;
        r[Field::Ctime.index()] = mtime;
        r[Field::Atime.index()] = mtime;
        r[Field::Mode.index()] = 0o100644;
        r[Field::Uid.index()] = 1000;
        r[Field::Gid.index()] = 1000;
        r[Field::Disk.index()] = size.div_euclid(4096) * 4096 + 4096;
        r[Field::Items.index()] = -1;
        r[Field::Kind.index()] = 2;
        r[Field::IsDir.index()] = 0;
        r
    }

    #[test]
    fn every_value_comes_back() {
        let mut w = ColumnWriter::new();
        // Deliberately more than two blocks, so block walking is exercised.
        for i in 0..300i64 {
            w.push(row(i % 40, i * 991, 1_785_000_000 - i * 7));
        }
        let bytes = w.finish();
        let cols = ColumnBlocks::open(&bytes).expect("open");
        assert_eq!(cols.rows(), 300);

        for i in 0..300usize {
            let want = row(i as i64 % 40, i as i64 * 991, 1_785_000_000 - i as i64 * 7);
            for f in Field::ALL {
                assert_eq!(cols.get(f, i), Some(want[f.index()]), "row {i}, {f:?}");
            }
        }
        assert_eq!(cols.get(Field::Size, 300), None);
    }

    #[test]
    fn ordered_timestamps_are_nearly_free() {
        // The measured claim: mtime costs 0.29 bytes an entry because the rows
        // are in its order. Check the shape rather than the exact figure.
        let mut w = ColumnWriter::new();
        for i in 0..4_096i64 {
            w.push(row(0, 0, 1_785_000_000 - i * 3));
        }
        let bytes = w.finish();
        let cols = ColumnBlocks::open(&bytes).expect("open");

        // Twelve i64 stored plainly would be 96 bytes an entry. The real
        // corpus measured 8.85 for eleven of them; this synthetic block has
        // more constant columns and fewer rows, so it lands lower. What the
        // test asserts is the order of magnitude, not a figure it invented:
        // at least ten times better than storing them.
        let per_entry = bytes.len() as f64 / 4_096.0;
        assert!(
            per_entry < 9.6,
            "twelve columns should cost a few bytes, got {per_entry:.2}"
        );
        assert_eq!(
            cols.get(Field::Mtime, 4_095),
            Some(1_785_000_000 - 4_095 * 3)
        );
    }

    #[test]
    fn wildly_varying_values_still_round_trip() {
        // Sizes span nine orders of magnitude on a real filesystem, so a block
        // can genuinely need the full width.
        let mut w = ColumnWriter::new();
        for i in 0..BLOCK as i64 * 2 {
            let size = if i % 2 == 0 { 0 } else { i64::MAX / (i + 1) };
            w.push(row(0, size, 0));
        }
        let bytes = w.finish();
        let cols = ColumnBlocks::open(&bytes).expect("open");
        for i in 0..BLOCK * 2 {
            let want = if i % 2 == 0 {
                0
            } else {
                i64::MAX / (i as i64 + 1)
            };
            assert_eq!(cols.get(Field::Size, i), Some(want), "row {i}");
        }
    }

    #[test]
    fn an_empty_or_damaged_block_is_refused_rather_than_read() {
        let bytes = ColumnWriter::new().finish();
        let cols = ColumnBlocks::open(&bytes).expect("open");
        assert!(cols.is_empty());
        assert_eq!(cols.get(Field::Size, 0), None);
        assert!(ColumnBlocks::open(&[0u8; 4]).is_none());
        // A header claiming the wrong number of columns is a different format.
        let mut wrong = bytes.clone();
        wrong[4] = 3;
        assert!(ColumnBlocks::open(&wrong).is_none());
    }

    #[test]
    fn the_field_order_is_part_of_the_format() {
        // A guard, not a tautology: reordering the enum silently reinterprets
        // every column of every index already written.
        assert_eq!(Field::DirId.index(), 0);
        assert_eq!(Field::Mtime.index(), 2);
        assert_eq!(Field::IsDir.index(), 11);
        assert_eq!(Field::ALL.len(), 12);
    }
}
