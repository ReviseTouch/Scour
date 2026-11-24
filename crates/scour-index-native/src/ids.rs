//! Finding a row by its path, the only thing that identifies it: the top half
//! of a path digest paired with a row number, sorted. Not an inode — a careful
//! save renames a temporary over the target. A probe returns *candidates*,
//! which the caller confirms against the directory and name the row carries.
//!
//! In buckets by the digest's leading bits, about sixteen entries each: an
//! entry is the bits left and a packed row, 37 bits for five million rows.

use scour_core::SourceId;

use crate::packed;

/// A stable 64-bit digest of a source and a path — the same number an
/// [`EntryId`] carries. Spelled out rather than delegated to a standard hasher,
/// which may change its output between releases and go on disk unnoticed.
///
/// [`EntryId`]: scour_core::EntryId
pub use scour_core::path_digest as digest;

/// The half of the digest that goes in the file.
fn narrow(d: u64) -> u32 {
    (d >> 32) as u32
}

/// How many leading bits of the half-digest name a bucket, for a table of
/// `len` entries: sixteen to thirty-two entries a bucket, 2^24 buckets at most.
fn bucket_bits(len: usize) -> u32 {
    if len < 32 {
        0
    } else {
        (len / 16).ilog2().min(24)
    }
}

#[derive(Debug, Default)]
pub struct IdWriter {
    pairs: Vec<(u32, u32)>,
}

impl IdWriter {
    pub fn new() -> IdWriter {
        IdWriter::default()
    }

    pub fn push(&mut self, source: SourceId, path: &str, row: u32) {
        self.pairs.push((narrow(digest(source, path)), row));
    }

    /// The count, where each bucket begins and one more for the end, then the
    /// entries packed: the digest bits a bucket does not say, then the row.
    pub fn finish(mut self) -> Vec<u8> {
        // By hash then row: two builds of one input must give the same bytes.
        self.pairs.sort_unstable();
        let len = self.pairs.len();
        let bits = bucket_bits(len);
        let (low_bits, row_bits) = (32 - bits, packed::width_for(len));
        let buckets = 1usize << bits;
        let mut out = Vec::with_capacity(
            4 + (buckets + 1) * 4 + packed::len(len, low_bits + row_bits).unwrap_or(0),
        );
        out.extend_from_slice(&(len as u32).to_le_bytes());
        let mut at = 0usize;
        for bucket in 0..=buckets {
            // The first entry at or past this bucket; the last is the count.
            while at < len && (bucket_of(self.pairs[at].0, bits) as usize) < bucket {
                at += 1;
            }
            out.extend_from_slice(&(at as u32).to_le_bytes());
        }
        let low = low_mask(low_bits);
        packed::pack(
            self.pairs
                .iter()
                .map(|&(h, row)| u64::from(h & low) << row_bits | u64::from(row)),
            low_bits + row_bits,
            &mut out,
        );
        out
    }
}

fn bucket_of(hash: u32, bits: u32) -> u32 {
    // A shift by all thirty-two bits is not zero, it is undefined.
    if bits == 0 { 0 } else { hash >> (32 - bits) }
}

fn low_mask(low_bits: u32) -> u32 {
    if low_bits >= 32 {
        u32::MAX
    } else {
        (1u32 << low_bits) - 1
    }
}

/// The table, read in place.
#[derive(Debug, Clone, Copy, Default)]
pub struct IdMap<'a> {
    len: usize,
    /// The leading digest bits that name a bucket, and the bits of a row.
    bits: u32,
    row_bits: u32,
    starts: &'a [u8],
    entries: &'a [u8],
}

impl<'a> IdMap<'a> {
    /// Open the table, or refuse one that is not exactly as long as its count
    /// says: it is written whole.
    pub fn open(bytes: &'a [u8]) -> Option<IdMap<'a>> {
        let len = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
        let bits = bucket_bits(len);
        let row_bits = packed::width_for(len);
        let table = 4 + ((1usize << bits) + 1) * 4;
        let end = table.checked_add(packed::len(len, 32 - bits + row_bits)?)?;
        if bytes.len() != end {
            return None;
        }
        Some(IdMap {
            len,
            bits,
            row_bits,
            starts: &bytes[4..table],
            entries: &bytes[table..],
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The half-digest a lookup is keyed on. Public so a caller holding many
    /// paths can sort them the same way and merge rather than search.
    pub fn key_of(source: SourceId, path: &str) -> u32 {
        narrow(digest(source, path))
    }

    /// Where a bucket's entries begin; the count for one past the last, and
    /// for a number the table cannot hold.
    fn start(&self, bucket: usize) -> usize {
        let at = bucket * 4;
        self.starts.get(at..at + 4).map_or(self.len, |b| {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize
        })
    }

    /// The `i`-th entry: the digest bits its bucket does not say, and the row.
    fn entry(&self, i: usize) -> Option<(u32, u32)> {
        if i >= self.len {
            return None;
        }
        let v = packed::get(self.entries, 32 - self.bits + self.row_bits, i)?;
        Some((
            (v >> self.row_bits) as u32,
            (v & ((1u64 << self.row_bits) - 1)) as u32,
        ))
    }

    /// Rows that *might* hold this path — the caller confirms against the
    /// directory and name the row carries. Almost always empty or one.
    pub fn candidates(&self, source: SourceId, path: &str) -> impl Iterator<Item = u32> + '_ {
        self.rows_for(narrow(digest(source, path)))
    }

    /// The same, for a caller that has already computed the key.
    pub fn rows_for(&self, want: u32) -> impl Iterator<Item = u32> + '_ {
        let bucket = bucket_of(want, self.bits) as usize;
        let low = want & low_mask(32 - self.bits);
        let (mut lo, end) = (self.start(bucket), self.start(bucket + 1));
        let mut hi = end;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.entry(mid).is_some_and(|(l, _)| l < low) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        (lo..end)
            .map_while(move |i| self.entry(i))
            .take_while(move |&(l, _)| l == low)
            .map(|(_, row)| row)
    }

    /// Every entry in stored order: half-digest first, then row.
    pub fn iter(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let buckets = 1usize << self.bits;
        let mut bucket = 0usize;
        (0..self.len).map_while(move |i| {
            while bucket + 1 < buckets && self.start(bucket + 1) <= i {
                bucket += 1;
            }
            let (low, row) = self.entry(i)?;
            let high = if self.bits == 0 {
                0
            } else {
                (bucket as u32) << (32 - self.bits)
            };
            Some((high | low, row))
        })
    }

    /// Every `(row, j)` where the table's entry and the `j`-th of `n` keys are
    /// the same half-digest; the keys ascending, as [`IdMap::key_of`] sorts.
    /// A merge inside each bucket the keys name, and no read of any other: a
    /// walk of two million paths costs a small segment what its keys cost.
    pub fn join(&self, n: usize, key: impl Fn(usize) -> u32, mut found: impl FnMut(u32, usize)) {
        let low = low_mask(32 - self.bits);
        let mut j = 0usize;
        while j < n {
            let bucket = bucket_of(key(j), self.bits);
            let end = self.start(bucket as usize + 1);
            let mut i = self.start(bucket as usize);
            let entry = |i: usize| self.entry(i).filter(|_| i < end);
            let mut at = entry(i);
            while j < n && bucket_of(key(j), self.bits) == bucket {
                let h = key(j);
                // A run of equal hashes each side: both are tiny, so the cross
                // product is cheap.
                let run = (j..n).take_while(|&w| key(w) == h).count();
                while at.is_some_and(|(l, _)| l < h & low) {
                    i += 1;
                    at = entry(i);
                }
                while let Some((_, row)) = at.filter(|&(l, _)| l == h & low) {
                    for w in j..j + run {
                        found(row, w);
                    }
                    i += 1;
                    at = entry(i);
                }
                j += run;
            }
        }
    }
}

#[cfg(test)]
mod tests;
