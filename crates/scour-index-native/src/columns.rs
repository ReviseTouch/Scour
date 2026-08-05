//! The numbers, in columns.
//!
//! Sixteen values an entry — eleven from `stat`, the directory number, and
//! four that carry the entry's identity —
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
/// Two tensions, and the second one decides it.
///
/// **Packing.** A larger block amortises the per-block minimum and width
/// further; a smaller one packs tighter, because a narrow range needs fewer
/// bits. On its own that argued for 128, which is also the width SIMD
/// bit-packers use.
///
/// **Selectivity**, which was not measured until much later and matters more.
/// A block is the unit the trigram filter and the zone map can skip, and it
/// survives if *anything* in it might match — so 128 names to a block hands a
/// term 128 rows for every one that could be right. Measured over 750,717 real
/// entries and six real terms:
///
/// | rows a block | index | candidates | query |
/// |---|---|---|---|
/// | 128 | 51 MB | 267,296 | 5.76 ms |
/// | 64 | 53 MB | 169,248 | 3.86 ms |
/// | **32** | **57 MB** | **104,192** | **2.99 ms** |
/// | 16 | 65 MB | 60,352 | 2.16 ms |
///
/// Thirty-two is the knee: 48% off the query for 11% more on disk, where the
/// next halving buys a fifth as much per megabyte. It is also the number that
/// decides how a *larger* index behaves, because what grows with the corpus is
/// how many candidates a term must walk, not what each one costs — that has
/// been measured flat at about 25 ns whatever the sort.
pub const BLOCK: usize = 32;

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
    /// Which source produced the entry.
    ///
    /// The last of the identity, and the only part of it that is not already
    /// in the directory table and the name arena. Three columns used to sit
    /// beside it holding whatever the source called the entry — an inode, a
    /// hash — and they are gone: a row is identified by its source and its
    /// path, so storing a second identity beside the first meant carrying
    /// eight incompressible bytes a row to answer a question the path already
    /// answers. **19 MB of a 200 MB index**, spent on the thing that was
    /// producing duplicate rows.
    Source = 12,
}

impl Field {
    pub const ALL: [Field; 13] = [
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
        Field::Source,
    ];

    pub fn index(self) -> usize {
        self as usize
    }
}

/// Encodes rows as they arrive, one block at a time.
///
/// Nothing is buffered but the block being filled. The first version held every
/// row until `finish` — sixteen numbers at eight bytes each, so **152 MB** at a
/// million entries, all of it anonymous and all of it live for the whole of a
/// rebuild. Encoding as the block fills costs the same arithmetic and holds
/// only what will be written.
#[derive(Debug, Default)]
pub struct ColumnWriter {
    /// The block being filled, in row order.
    pending: Vec<[i64; Field::ALL.len()]>,
    /// Encoded blocks and their offsets, one per column.
    blocks: Vec<Vec<u8>>,
    offsets: Vec<Vec<u32>>,
    rows: usize,
}

impl ColumnWriter {
    pub fn new() -> ColumnWriter {
        ColumnWriter {
            pending: Vec::with_capacity(BLOCK),
            blocks: vec![Vec::new(); Field::ALL.len()],
            offsets: vec![Vec::new(); Field::ALL.len()],
            rows: 0,
        }
    }

    pub fn push(&mut self, row: [i64; Field::ALL.len()]) {
        self.pending.push(row);
        self.rows += 1;
        if self.pending.len() == BLOCK {
            self.seal();
        }
    }

    /// Encode the pending rows into every column and forget them.
    fn seal(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let mut vals = Vec::with_capacity(self.pending.len());
        for field in Field::ALL {
            let c = field.index();
            vals.clear();
            vals.extend(self.pending.iter().map(|r| r[c]));
            let (min, bits) = varint::width_for(&vals);
            let max = vals.iter().copied().max().unwrap_or(min);
            let blocks = &mut self.blocks[c];
            self.offsets[c].push(blocks.len() as u32);
            blocks.extend_from_slice(&min.to_le_bytes());
            // The *true* maximum, not `min + 2^bits - 1`.
            //
            // Eight bytes a block a column — one byte an entry — and it is what
            // lets a numeric filter reject a hundred and twenty-eight rows with
            // one comparison. The width-derived bound is far too loose to do
            // that: a block of file sizes spanning a kilobyte to a megabyte has
            // twenty bits of width, so the derived maximum is a megabyte
            // whatever the block actually holds.
            blocks.extend_from_slice(&max.to_le_bytes());
            blocks.push(bits as u8);
            varint::pack(blocks, &vals, min, bits);
        }
        self.pending.clear();
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// Assemble the file.
    ///
    /// Layout: a header of (row count, column count), then per column a
    /// 64-bit offset to its section. A section is a count of blocks, a 32-bit
    /// offset for each, and then the blocks — each an 8-byte minimum, an 8-byte
    /// maximum, a byte of width, and the packed values.
    ///
    /// The offset array is what makes a read O(1), and it was added after a
    /// measurement rather than by foresight. Blocks are variable width, so
    /// without it reaching block *b* means walking the headers of the *b*
    /// before it — which turned a single-column filter over a million rows
    /// into five seconds. Four bytes a block is 0.03 bytes an entry.
    pub fn finish(mut self) -> Vec<u8> {
        self.seal();
        let cols = Field::ALL.len();
        let header = 8 + cols * 8;
        let body_len = |c: usize| 4 + self.offsets[c].len() * 4 + self.blocks[c].len();

        let mut out = Vec::with_capacity(header + (0..cols).map(body_len).sum::<usize>());
        out.extend_from_slice(&(self.rows as u32).to_le_bytes());
        out.extend_from_slice(&(cols as u32).to_le_bytes());
        let mut at = header as u64;
        for c in 0..cols {
            out.extend_from_slice(&at.to_le_bytes());
            at += body_len(c) as u64;
        }
        for c in 0..cols {
            out.extend_from_slice(&(self.offsets[c].len() as u32).to_le_bytes());
            for o in &self.offsets[c] {
                out.extend_from_slice(&o.to_le_bytes());
            }
            out.extend_from_slice(&self.blocks[c]);
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

    /// One value, in constant time.
    ///
    /// Random access rather than bulk decode: a filter usually rejects a row on
    /// the first column it looks at, and decoding the other fifteen would be
    /// work thrown away.
    pub fn get(&self, field: Field, row: usize) -> Option<i64> {
        if row >= self.rows {
            return None;
        }
        let at = field.index() * 8;
        let section = u64::from_le_bytes(self.offsets.get(at..at + 8)?.try_into().ok()?) as usize;
        let n_blocks =
            u32::from_le_bytes(self.bytes.get(section..section + 4)?.try_into().ok()?) as usize;
        let block = row / BLOCK;
        if block >= n_blocks {
            return None;
        }
        let idx = section + 4 + block * 4;
        let start = u32::from_le_bytes(self.bytes.get(idx..idx + 4)?.try_into().ok()?) as usize;
        let at = section + 4 + n_blocks * 4 + start;
        let min = i64::from_le_bytes(self.bytes.get(at..at + 8)?.try_into().ok()?);
        let bits = *self.bytes.get(at + 16)? as u32;
        let packed = self.bytes.get(at + 17..)?;
        Some(varint::unpack_one(packed, min, bits, row % BLOCK))
    }

    /// The range of values a block holds, without decoding any of them.
    ///
    /// The zone map. A filter that cannot be satisfied anywhere in `[min, max]`
    /// rejects a hundred and twenty-eight rows with two comparisons, and the
    /// walk never touches their names, their paths or any other column.
    ///
    /// It can only reject. A block whose range admits the filter still has
    /// every row tested exactly as before, so this cannot turn a match into a
    /// miss — only a miss into a skip.
    pub fn block_range(&self, field: Field, block: usize) -> Option<(i64, i64)> {
        let at = field.index() * 8;
        let section = u64::from_le_bytes(self.offsets.get(at..at + 8)?.try_into().ok()?) as usize;
        let n_blocks =
            u32::from_le_bytes(self.bytes.get(section..section + 4)?.try_into().ok()?) as usize;
        if block >= n_blocks {
            return None;
        }
        let idx = section + 4 + block * 4;
        let start = u32::from_le_bytes(self.bytes.get(idx..idx + 4)?.try_into().ok()?) as usize;
        let at = section + 4 + n_blocks * 4 + start;
        let min = i64::from_le_bytes(self.bytes.get(at..at + 8)?.try_into().ok()?);
        let max = i64::from_le_bytes(self.bytes.get(at + 8..at + 16)?.try_into().ok()?);
        Some((min, max))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(dir: i64, size: i64, mtime: i64) -> [i64; Field::ALL.len()] {
        let mut r = [0i64; Field::ALL.len()];
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
        r[Field::Source.index()] = 0;
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

        // Sixteen i64 stored plainly would be 128 bytes an entry. What the
        // test asserts is the order of magnitude, not a figure it invented.
        //
        // The bound was 9.6 when a block held 128 rows and this measured 23.30
        // the day it became 32 — because the per-block minimum and width are
        // paid four times as often, and this synthetic corpus is the worst
        // case for that: fifteen of its sixteen columns are constant, so the
        // overhead *is* the file. On the real corpus the same change took the
        // whole index from 51 MB to 57 for 750,717 entries, and bought 48% off
        // every query. That is the trade, and it is recorded in `BLOCK`.
        let per_entry = bytes.len() as f64 / 4_096.0;
        assert!(
            per_entry < 26.0,
            "sixteen columns should still beat storing them, got {per_entry:.2}"
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
        assert_eq!(Field::Source.index(), 12);
        assert_eq!(Field::ALL.len(), 13);
    }
}
