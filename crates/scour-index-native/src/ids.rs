//! Finding a row by the only thing that identifies it: its path.
//!
//! Everything else in a segment is arranged for *searching*, which walks rows
//! in order and never asks "where is this particular file?". Writing does ask:
//! an upsert of a path that is already indexed has to kill the old row, or the
//! same path appears twice.
//!
//! So one more file, and it is deliberately the smallest thing that answers
//! that question: the top half of a digest of the path, paired with a row
//! number, sorted. Eight bytes an entry against the thirty-six the rest of the
//! segment costs.
//!
//! ## Why the path, and not an id the source handed over
//!
//! It used to be an id, and on Linux that id was the inode. An inode is a
//! promise about the *object*, and a row is not an object — it is a name.
//! Everything that saves carefully writes a temporary file and renames it over
//! the target, so the path survives and the inode does not: the upsert arrives
//! under a new identity, nothing ever says the old one is gone, and both rows
//! stay. Measured on the live index before this changed: **267 rows for one
//! cache file**, one per save, growing for as long as the service ran. A file
//! modified *in place* stayed a single row, which is exactly why it took so
//! long to notice.
//!
//! The path is also the only thing a removal can name — a deleted file cannot
//! be stat-ed — so keying on it is what makes an upsert and a removal talk
//! about the same thing.
//!
//! ## Why half a digest
//!
//! Thirty-two bits over a million rows collide about once in five thousand
//! probes, which would be unacceptable if a collision were an error. It is not:
//! a probe returns *candidates*, and the caller confirms each against the
//! directory number and name the row actually carries. So the narrow key costs
//! a rare extra comparison and saves four bytes on every entry, and the
//! confirmation is exact — a hash is never the last word on whether two rows
//! are the same file.

use scour_core::SourceId;

/// A stable 64-bit digest of a source and a path.
///
/// [`scour_core::path_digest`], and deliberately not a second one: the same
/// number is what an [`EntryId`] carries for a path, so a row's identity and
/// the key it is filed under cannot drift apart. It is written into a file, so
/// it is spelled out there rather than delegated to a standard hasher —
/// `DefaultHasher` and the fast hashers are all explicitly allowed to change
/// their output between releases, which would turn every existing index into
/// one that silently cannot find anything.
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
        // By hash, then by row, so that two builds of the same input produce
        // the same bytes — the property the rest of the segment also holds and
        // that makes a merge checkable against a rebuild.
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

    /// Rows that *might* hold this path.
    ///
    /// Might, not do: the caller confirms against the directory and name the
    /// row carries. Almost always empty or exactly one.
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
        // Almost: a collision is allowed, a wrong answer is not. What this
        // asserts is that the caller is never handed *every* row.
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
        // If this changes, every index on disk stops finding anything, and
        // nothing reports an error. The number is the format.
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
