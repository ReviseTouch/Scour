//! Narrowing the walk to the blocks that could match.
//!
//! The scan answers every query correctly and most of them quickly, and the
//! measurement said exactly where it does not: a *selective* substring. `rapor`
//! matches fifteen files out of 1,197,514, so there is no page to fill and no
//! count to reach, and the walk runs to the end — 28.9 ms where an inverted
//! index answers in 1.7.
//!
//! This is that index, and it is deliberately the weakest one that closes the
//! gap.
//!
//! ## It narrows; it never answers
//!
//! A posting list here holds **block numbers**, not rows: which groups of
//! [`BLOCK`] rows contain a trigram. A query intersects the lists of its
//! trigrams and the walk then visits only those blocks — where it applies the
//! same exact byte comparison it always did.
//!
//! So the index cannot be wrong in the direction that matters. A name that
//! contains the needle contains every trigram of the needle, so its block is in
//! every one of those lists and survives the intersection: **no match can be
//! missed**. A block that survives without containing a match costs a few
//! microseconds of scanning and nothing else. The guarantee the whole design
//! rests on — that this thing can be slow but not wrong — still holds.
//!
//! That is also why the blocks are 128 rows rather than one. It makes the lists
//! thirty times shorter, and the false positives it admits are paid for at
//! memory speed by code that was going to run anyway.
//!
//! ## What it declines to index
//!
//! A trigram present in most blocks narrows nothing, and those are the trigrams
//! that would cost the most to store. They are recorded in the dictionary as
//! *present but unindexed*, which is not the same as absent: absent means no
//! row in this segment contains it, and that is an answer.
//!
//! Terms shorter than three bytes have no trigram at all and fall back to the
//! full walk. That is worth stating plainly: it is slower, and it is an answer.
//! A trigram index alone has to refuse them.

use std::collections::HashMap;

use crate::columns::BLOCK;
use crate::names::Folded;
use crate::varint;

/// A trigram is three consecutive bytes of the folded name.
///
/// Bytes rather than characters, which sounds wrong for text and is right here:
/// the query is folded by the same code that folded the name, so both sides cut
/// the same windows out of the same bytes. A window landing inside a multi-byte
/// character is not a character, but it is the *same* not-a-character on both
/// sides, which is all a filter needs.
pub fn for_each(folded: &[u8], mut f: impl FnMut(u32)) {
    for w in folded.windows(3) {
        f((u32::from(w[0]) << 16) | (u32::from(w[1]) << 8) | u32::from(w[2]));
    }
}

/// Above this share of blocks, a trigram is not stored.
///
/// It would narrow the candidate set to most of the index while costing the
/// largest posting list in the file — the worst trade available.
const TOO_COMMON: f64 = 0.4;

/// What the dictionary records for a trigram that is present everywhere.
const UNINDEXED: u32 = u32::MAX;

#[derive(Debug, Default)]
struct List {
    /// Last block appended, so the next one can be stored as a difference.
    last: u32,
    count: u32,
    bytes: Vec<u8>,
}

/// Collects, one block at a time.
///
/// The lists are delta-encoded as they are appended rather than buffered as
/// numbers: blocks arrive in increasing order, so nothing needs sorting, and
/// what is held is what will be written. Buffering the pairs instead would be
/// eleven million of them at a million entries.
#[derive(Debug)]
pub struct TrigramWriter {
    lists: HashMap<u32, List>,
    /// Trigrams seen in the block being filled, as a bitmap over the whole
    /// 24-bit key space plus the list of keys that were set.
    ///
    /// This was a `HashSet<u32>` and it was the single most expensive thing in
    /// a scan: three bytes make a key, so there are only 2^24 of them, and
    /// hashing a number that small to store it in a table costs more than
    /// addressing it directly. The bitmap is 2 MB and lives for one segment;
    /// the list is what makes clearing it proportional to what was set rather
    /// than to the key space.
    seen_bits: Vec<u64>,
    seen_list: Vec<u32>,
    /// The writer folds, rather than trusting the caller to have folded.
    ///
    /// Not tidiness: a name indexed under its own spelling and searched for
    /// under a folded one produces a *false negative*, which is the one failure
    /// this design is not allowed to have. It was written the other way first
    /// and the test caught `Colpan` being unfindable as `colpan`.
    fold: Folded,
    block: u32,
    rows: usize,
}

impl Default for TrigramWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl TrigramWriter {
    pub fn new() -> TrigramWriter {
        TrigramWriter {
            lists: HashMap::new(),
            // 2^24 bits, one per possible key.
            seen_bits: vec![0u64; (1 << 24) / 64],
            seen_list: Vec::new(),
            fold: Folded::default(),
            block: 0,
            rows: 0,
        }
    }

    /// Add one row's name, as the filesystem spells it.
    pub fn push(&mut self, name: &[u8]) {
        let bits = &mut self.seen_bits;
        let list = &mut self.seen_list;
        for_each(self.fold.fold_bytes(name), |key| {
            let (word, bit) = ((key >> 6) as usize, 1u64 << (key & 63));
            if bits[word] & bit == 0 {
                bits[word] |= bit;
                list.push(key);
            }
        });
        self.rows += 1;
        if self.rows.is_multiple_of(BLOCK) {
            self.seal();
        }
    }

    fn seal(&mut self) {
        for key in self.seen_list.drain(..) {
            self.seen_bits[(key >> 6) as usize] &= !(1u64 << (key & 63));
            let list = self.lists.entry(key).or_default();
            // The difference from the previous block, which for the first is
            // the block number itself.
            varint::put(&mut list.bytes, u64::from(self.block - list.last));
            list.last = self.block;
            list.count += 1;
        }
        self.block += 1;
    }

    /// Dictionary and postings.
    pub fn finish(mut self) -> (Vec<u8>, Vec<u8>) {
        if !self.seen_list.is_empty() {
            self.seal();
        }
        let blocks = self.block;
        let cutoff = (f64::from(blocks) * TOO_COMMON) as u32;

        let mut keys: Vec<u32> = self.lists.keys().copied().collect();
        keys.sort_unstable();

        let mut post = Vec::new();
        let mut dict = Vec::with_capacity(8 + keys.len() * 8);
        dict.extend_from_slice(&(keys.len() as u32).to_le_bytes());
        dict.extend_from_slice(&blocks.to_le_bytes());
        let mut offsets = Vec::with_capacity(keys.len());
        for &k in &keys {
            let list = &self.lists[&k];
            if list.count > cutoff {
                offsets.push(UNINDEXED);
                continue;
            }
            offsets.push(post.len() as u32);
            varint::put(&mut post, u64::from(list.count));
            post.extend_from_slice(&list.bytes);
        }
        for (k, off) in keys.iter().zip(&offsets) {
            dict.extend_from_slice(&k.to_le_bytes());
            dict.extend_from_slice(&off.to_le_bytes());
        }
        (dict, post)
    }
}

/// What the dictionary says about one trigram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lookup<'a> {
    /// No row in this segment contains it. An answer, not a failure.
    Absent,
    /// Present in too many blocks to be worth storing. Narrows nothing.
    Everywhere,
    /// A delta-encoded list of block numbers.
    Blocks { count: u32, bytes: &'a [u8] },
}

/// The index, read in place.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrigramIndex<'a> {
    entries: &'a [u8],
    post: &'a [u8],
    len: usize,
    blocks: u32,
}

impl<'a> TrigramIndex<'a> {
    pub fn open(dict: &'a [u8], post: &'a [u8]) -> Option<TrigramIndex<'a>> {
        if dict.len() < 8 {
            return None;
        }
        let len = u32::from_le_bytes(dict[0..4].try_into().ok()?) as usize;
        let blocks = u32::from_le_bytes(dict[4..8].try_into().ok()?);
        let entries = dict.get(8..8 + len * 8)?;
        Some(TrigramIndex {
            entries,
            post,
            len,
            blocks,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn blocks(&self) -> u32 {
        self.blocks
    }

    fn key_at(&self, i: usize) -> u32 {
        u32::from_le_bytes(self.entries[i * 8..i * 8 + 4].try_into().expect("4 bytes"))
    }

    fn off_at(&self, i: usize) -> u32 {
        u32::from_le_bytes(
            self.entries[i * 8 + 4..i * 8 + 8]
                .try_into()
                .expect("4 bytes"),
        )
    }

    fn lookup(&self, key: u32) -> Lookup<'a> {
        let mut lo = 0usize;
        let mut hi = self.len;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.key_at(mid).cmp(&key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let off = self.off_at(mid);
                    if off == UNINDEXED {
                        return Lookup::Everywhere;
                    }
                    let Some(rest) = self.post.get(off as usize..) else {
                        return Lookup::Everywhere;
                    };
                    let Some((count, used)) = varint::get(rest) else {
                        return Lookup::Everywhere;
                    };
                    return Lookup::Blocks {
                        count: count as u32,
                        bytes: &rest[used..],
                    };
                }
            }
        }
        Lookup::Absent
    }

    fn decode(count: u32, bytes: &[u8]) -> Vec<u32> {
        let mut out = Vec::with_capacity(count as usize);
        let mut at = 0usize;
        let mut block = 0u32;
        for _ in 0..count {
            let Some((delta, used)) = varint::get(&bytes[at..]) else {
                break;
            };
            block += delta as u32;
            out.push(block);
            at += used;
        }
        out
    }

    /// Blocks that could contain `folded`.
    ///
    /// `None` means "this cannot be narrowed usefully — walk everything", which
    /// is a slower answer and never a wrong one. `Some(empty)` means the
    /// segment genuinely holds nothing that matches.
    pub fn candidates(&self, folded: &[u8]) -> Option<Vec<u32>> {
        if self.len == 0 || folded.len() < 3 {
            return None;
        }
        let mut keys: Vec<u32> = Vec::new();
        for_each(folded, |k| keys.push(k));
        keys.sort_unstable();
        keys.dedup();

        let mut lists: Vec<(u32, &[u8])> = Vec::with_capacity(keys.len());
        for k in keys {
            match self.lookup(k) {
                // One trigram nobody has is the whole answer.
                Lookup::Absent => return Some(Vec::new()),
                Lookup::Everywhere => {}
                Lookup::Blocks { count, bytes } => lists.push((count, bytes)),
            }
        }
        if lists.is_empty() {
            return None;
        }
        // Cheapest first: the intersection can only shrink, so starting from
        // the shortest list means every later pass is over the smallest set so
        // far.
        lists.sort_unstable_by_key(|(count, _)| *count);
        let mut out = Self::decode(lists[0].0, lists[0].1);
        for (count, bytes) in &lists[1..] {
            if out.is_empty() {
                break;
            }
            let other = Self::decode(*count, bytes);
            out = intersect(&out, &other);
        }
        // Narrowing to most of the index is not narrowing. The walk would visit
        // nearly everything anyway and would have paid for this as well.
        if out.len() as u32 * 2 > self.blocks {
            return None;
        }
        Some(out)
    }
}

/// Both sorted; one pass.
fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len().min(b.len()));
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an index over names, one block per `BLOCK` of them.
    fn build(names: &[&str]) -> (Vec<u8>, Vec<u8>) {
        let mut w = TrigramWriter::new();
        for n in names {
            w.push(n.as_bytes());
        }
        w.finish()
    }

    #[test]
    fn every_matching_block_survives_the_intersection() {
        // The only property that matters: a name containing the needle is in a
        // block the candidate list keeps. A false positive is a few
        // microseconds; a false negative is a file the user cannot find.
        let names: Vec<String> = (0..2_000)
            .map(|i| match i % 7 {
                0 => format!("rapor-{i}.pdf"),
                1 => format!("main_{i}.rs"),
                2 => format!("Colpan{i}.toml"),
                _ => format!("file{i}.txt"),
            })
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (dict, post) = build(&refs);
        let idx = TrigramIndex::open(&dict, &post).expect("open");

        for needle in ["rapor", "main", "colpan", "file", "pdf", "por-", "n_1"] {
            let want: Vec<u32> = refs
                .iter()
                .enumerate()
                .filter(|(_, n)| n.to_lowercase().contains(needle))
                .map(|(i, _)| (i / BLOCK) as u32)
                .collect();
            match idx.candidates(needle.as_bytes()) {
                // Walking everything is always allowed.
                None => {}
                Some(got) => {
                    for b in want {
                        assert!(got.contains(&b), "{needle:?} lost block {b} of {got:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_trigram_nobody_has_is_an_answer_on_its_own() {
        let (dict, post) = build(&["main.rs", "lib.rs", "rapor.pdf"]);
        let idx = TrigramIndex::open(&dict, &post).expect("open");
        assert_eq!(idx.candidates(b"zzzz"), Some(Vec::new()));
        assert_eq!(idx.candidates(b"qqq"), Some(Vec::new()));
    }

    #[test]
    fn a_term_too_short_to_have_a_trigram_asks_for_the_whole_walk() {
        let (dict, post) = build(&["main.rs", "lib.rs"]);
        let idx = TrigramIndex::open(&dict, &post).expect("open");
        assert_eq!(idx.candidates(b"ab"), None);
        assert_eq!(idx.candidates(b"a"), None);
        assert_eq!(idx.candidates(b""), None);
    }

    #[test]
    fn a_selective_term_narrows_to_almost_nothing() {
        // The reason this file exists.
        let mut names: Vec<String> = (0..12_800).map(|i| format!("file{i}.txt")).collect();
        names[5_000] = "the-only-rapor.pdf".into();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (dict, post) = build(&refs);
        let idx = TrigramIndex::open(&dict, &post).expect("open");
        let got = idx.candidates(b"rapor").expect("narrowed");
        assert_eq!(got, vec![(5_000 / BLOCK) as u32]);
    }

    #[test]
    fn a_trigram_in_every_block_is_recorded_as_present_rather_than_stored() {
        // `.tx` is in every one of these, so storing its list would cost the
        // largest posting in the file to narrow nothing.
        let names: Vec<String> = (0..12_800).map(|i| format!("file{i}.txt")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (dict, post) = build(&refs);
        let idx = TrigramIndex::open(&dict, &post).expect("open");
        assert_eq!(idx.lookup(0x2e_74_78), Lookup::Everywhere, ".tx");
        // And a query made only of such trigrams asks for the full walk rather
        // than claiming there is nothing.
        assert_eq!(idx.candidates(b".txt"), None);
    }

    #[test]
    fn an_empty_index_is_readable_and_narrows_nothing() {
        let (dict, post) = build(&[]);
        let idx = TrigramIndex::open(&dict, &post).expect("open");
        assert!(idx.is_empty());
        assert_eq!(idx.candidates(b"rapor"), None);
        assert!(TrigramIndex::open(&[], &[]).is_none());
    }

    #[test]
    fn two_builds_of_the_same_input_agree_byte_for_byte() {
        let names: Vec<String> = (0..300).map(|i| format!("f{i}.rs")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(build(&refs), build(&refs));
    }
}
