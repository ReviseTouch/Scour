//! Finding a row by its path, the only thing that identifies it: the top half
//! of a path digest paired with a row number, sorted, eight bytes an entry.
//! Not an inode — a careful save renames a temporary over the target, so the
//! path survives and the inode does not. A probe returns *candidates*, which
//! the caller confirms against the directory number and name the row carries.

use scour_core::SourceId;

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

    pub fn finish(mut self) -> Vec<u8> {
        // By hash then row: two builds of one input must give the same bytes.
        self.pairs.sort_unstable();
        let mut out = Vec::with_capacity(4 + self.pairs.len() * 8);
        out.extend_from_slice(&(self.pairs.len() as u32).to_le_bytes());
        for (h, row) in &self.pairs {
            out.extend_from_slice(&h.to_le_bytes());
            out.extend_from_slice(&row.to_le_bytes());
        }
        out
    }
}

/// The table, read in place.
#[derive(Debug, Clone, Copy, Default)]
pub struct IdMap<'a> {
    pairs: &'a [u8],
    len: usize,
}

impl<'a> IdMap<'a> {
    pub fn open(bytes: &'a [u8]) -> Option<IdMap<'a>> {
        if bytes.len() < 4 {
            return None;
        }
        let len = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
        let pairs = bytes.get(4..4 + len * 8)?;
        Some(IdMap { pairs, len })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The `i`-th pair, in stored order: hash first, then row.
    pub fn at(&self, i: usize) -> (u32, u32) {
        (self.hash_at(i), self.row_at(i))
    }

    /// The half-digest a lookup is keyed on. Public so a caller holding many
    /// paths can sort them the same way and merge rather than search.
    pub fn key_of(source: SourceId, path: &str) -> u32 {
        narrow(digest(source, path))
    }

    fn hash_at(&self, i: usize) -> u32 {
        let at = i * 8;
        u32::from_le_bytes(self.pairs[at..at + 4].try_into().expect("4 bytes"))
    }

    fn row_at(&self, i: usize) -> u32 {
        let at = i * 8 + 4;
        u32::from_le_bytes(self.pairs[at..at + 4].try_into().expect("4 bytes"))
    }

    /// Rows that *might* hold this path — the caller confirms against the
    /// directory and name the row carries. Almost always empty or one.
    pub fn candidates(&self, source: SourceId, path: &str) -> impl Iterator<Item = u32> + '_ {
        self.rows_for(narrow(digest(source, path)))
    }

    /// The same, for a caller that has already computed the key.
    pub fn rows_for(&self, want: u32) -> impl Iterator<Item = u32> + '_ {
        let mut lo = 0usize;
        let mut hi = self.len;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.hash_at(mid) < want {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        (lo..self.len)
            .take_while(move |&i| self.hash_at(i) == want)
            .map(move |i| self.row_at(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: SourceId = SourceId(0);

    fn path(n: u64) -> String {
        format!("/home/u/dosya-{n}.txt")
    }

    #[test]
    fn every_path_finds_its_own_row() {
        let mut w = IdWriter::new();
        for n in 0..1000u64 {
            w.push(S, &path(n), n as u32);
        }
        let bytes = w.finish();
        let map = IdMap::open(&bytes).expect("open");
        assert_eq!(map.len(), 1000);
        for n in 0..1000u64 {
            let rows: Vec<u32> = map.candidates(S, &path(n)).collect();
            assert!(rows.contains(&(n as u32)), "row for {n} not among {rows:?}");
        }
    }

    #[test]
    fn an_absent_path_returns_almost_nothing() {
        // A collision is allowed; being handed every row is not.
        let mut w = IdWriter::new();
        for n in 0..1000u64 {
            w.push(S, &path(n), n as u32);
        }
        let bytes = w.finish();
        let map = IdMap::open(&bytes).expect("open");
        let mut worst = 0;
        for n in 5000..6000u64 {
            worst = worst.max(map.candidates(S, &path(n)).count());
        }
        assert!(worst <= 2, "a probe returned {worst} candidates");
    }

    #[test]
    fn the_digest_is_pinned_to_these_numbers() {
        // The number is the format: change it and no index on disk finds
        // anything, with no error anywhere.
        assert_eq!(digest(SourceId(0), "/a/b"), 0xe8e9_2dc1_109d_665b);
        assert_ne!(
            digest(SourceId(0), "/a/b"),
            digest(SourceId(1), "/a/b"),
            "the source is part of the identity"
        );
        assert_ne!(
            digest(SourceId(0), "/a/b"),
            digest(SourceId(0), "/a/B"),
            "case belongs to the path, whatever the filesystem thinks of it"
        );
    }

    #[test]
    fn two_writers_over_the_same_input_agree_byte_for_byte() {
        let mut a = IdWriter::new();
        let mut b = IdWriter::new();
        for n in 0..100u64 {
            a.push(S, &path(n), n as u32);
        }
        for n in (0..100u64).rev() {
            b.push(S, &path(n), n as u32);
        }
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn an_empty_table_is_readable() {
        let bytes = IdWriter::new().finish();
        let map = IdMap::open(&bytes).expect("open");
        assert!(map.is_empty());
        assert_eq!(map.candidates(S, "/a").count(), 0);
        assert!(IdMap::open(&[]).is_none());
    }
}
