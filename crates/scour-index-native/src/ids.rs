//! Finding a row by identity.
//!
//! Everything else in a segment is arranged for *searching*, which walks rows
//! in order and never asks "where is this particular file?". Writing does ask,
//! twice: a removal names an entry and nothing else, and an upsert of a file
//! that already exists has to hide the old row or the same path appears twice.
//!
//! So one more file, and it is deliberately the smallest thing that answers
//! that question: the top half of a digest of the identity, paired with a row
//! number, sorted. Eight bytes an entry against the thirty-six the rest of the
//! segment costs.
//!
//! ## Why half a digest
//!
//! Thirty-two bits over a million rows collide about once in five thousand
//! probes, which would be unacceptable if a collision were an error. It is not:
//! a probe returns *candidates*, and the caller confirms each against the
//! identity actually stored in the columns. So the narrow key costs a rare
//! extra column read and saves four bytes on every entry, and there is no
//! accuracy argument on the other side — the confirmation is exact.

use scour_core::{EntryId, Key};

/// A stable 64-bit digest of an entry's identity.
///
/// Hand-written, and spelled out rather than delegated, because it is written
/// into a file: `DefaultHasher` and the fast hashers are all explicitly allowed
/// to change their output between releases, which would turn every existing
/// index into one that silently cannot find anything. This is FNV-1a, which is
/// fixed by its definition.
pub fn digest(id: &EntryId) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(PRIME);
        }
    };
    eat(&id.source.0.to_le_bytes());
    match &id.key {
        Key::Inode { dev, ino } => {
            eat(&[1]);
            eat(&dev.to_le_bytes());
            eat(&ino.to_le_bytes());
        }
        Key::PathHash(h) => {
            eat(&[2]);
            eat(&h.to_le_bytes());
        }
        Key::Opaque(bytes) => {
            eat(&[3]);
            eat(bytes);
        }
    }
    h
}

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

    pub fn push(&mut self, id: &EntryId, row: u32) {
        self.pairs.push((narrow(digest(id)), row));
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
    /// identities can sort them the same way and merge rather than search.
    pub fn key_of(id: &EntryId) -> u32 {
        narrow(digest(id))
    }

    fn hash_at(&self, i: usize) -> u32 {
        let at = i * 8;
        u32::from_le_bytes(self.pairs[at..at + 4].try_into().expect("4 bytes"))
    }

    fn row_at(&self, i: usize) -> u32 {
        let at = i * 8 + 4;
        u32::from_le_bytes(self.pairs[at..at + 4].try_into().expect("4 bytes"))
    }

    /// Rows whose identity *might* be `id`.
    ///
    /// Might, not does: the caller confirms against the stored identity. Almost
    /// always empty or exactly one.
    pub fn candidates(&self, id: &EntryId) -> impl Iterator<Item = u32> + '_ {
        let want = narrow(digest(id));
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
        let start = lo;
        (start..self.len)
            .take_while(move |&i| self.hash_at(i) == want)
            .map(move |i| self.row_at(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_core::SourceId;

    fn id(n: u64) -> EntryId {
        EntryId::inode(SourceId(0), 66_310, n)
    }

    #[test]
    fn every_id_finds_its_own_row() {
        let mut w = IdWriter::new();
        for n in 0..1000u64 {
            w.push(&id(n), n as u32);
        }
        let bytes = w.finish();
        let map = IdMap::open(&bytes).expect("open");
        assert_eq!(map.len(), 1000);
        for n in 0..1000u64 {
            let rows: Vec<u32> = map.candidates(&id(n)).collect();
            assert!(rows.contains(&(n as u32)), "row for {n} not among {rows:?}");
        }
    }

    #[test]
    fn an_absent_id_returns_almost_nothing() {
        // Almost: a collision is allowed, a wrong answer is not. What this
        // asserts is that the caller is never handed *every* row.
        let mut w = IdWriter::new();
        for n in 0..1000u64 {
            w.push(&id(n), n as u32);
        }
        let bytes = w.finish();
        let map = IdMap::open(&bytes).expect("open");
        let mut worst = 0;
        for n in 5000..6000u64 {
            worst = worst.max(map.candidates(&id(n)).count());
        }
        assert!(worst <= 2, "a probe returned {worst} candidates");
    }

    #[test]
    fn the_digest_is_pinned_to_these_numbers() {
        // If this changes, every index on disk stops finding anything, and
        // nothing reports an error. The numbers are the format.
        assert_eq!(
            digest(&EntryId::inode(SourceId(0), 1, 2)),
            0xd6cf_1123_4a9c_1b1f
        );
        assert_eq!(
            digest(&EntryId::path_hash(SourceId(0), "/a/b")),
            digest(&EntryId::path_hash(SourceId(0), "/a/b")),
        );
        assert_ne!(
            digest(&EntryId::inode(SourceId(0), 1, 2)),
            digest(&EntryId::inode(SourceId(1), 1, 2)),
            "the source is part of the identity"
        );
    }

    #[test]
    fn two_writers_over_the_same_input_agree_byte_for_byte() {
        let mut a = IdWriter::new();
        let mut b = IdWriter::new();
        for n in 0..100u64 {
            a.push(&id(n), n as u32);
        }
        for n in (0..100u64).rev() {
            b.push(&id(n), n as u32);
        }
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn an_empty_table_is_readable() {
        let bytes = IdWriter::new().finish();
        let map = IdMap::open(&bytes).expect("open");
        assert!(map.is_empty());
        assert_eq!(map.candidates(&id(1)).count(), 0);
        assert!(IdMap::open(&[]).is_none());
    }
}
