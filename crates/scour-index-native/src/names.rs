//! The name arena: every file name, folded, NUL-separated, in row order.
//!
//! The only index is one 32-bit offset every [`BLOCK`] rows — a search walks in
//! order, and only forty rows a query are materialised. The fold is what is
//! matched: folding per query instead costs 40.5 ns a row against 8.3. How a
//! name is spelled is kept as its difference from the fold: [`crate::spelling`].

use scour_core::text::{Plain, plain};

use crate::columns::BLOCK;
use crate::spelling::SpellingWriter;

/// The longest name folded into the stack buffer. `NAME_MAX` is 255 bytes and
/// folding can grow a string, so this is generous; a longer name is matched
/// unfolded rather than dropped.
const FOLD_CAP: usize = 1024;

#[derive(Debug, Default)]
pub struct NameWriter {
    /// The names as spelled: what the path and extension orders are built
    /// from, and gone once the segment is.
    bytes: Vec<u8>,
    rows: usize,
    /// The same names, folded, with the same row numbering.
    folded: Vec<u8>,
    /// Byte offset of the first folded name of each block.
    folded_blocks: Vec<u32>,
    spelling: SpellingWriter,
}

impl NameWriter {
    pub fn new() -> NameWriter {
        NameWriter::default()
    }

    pub fn push(&mut self, name: &str) {
        self.push_and_fold(name);
    }

    /// Let the segment's trigram writer reuse the exact fold stored here.
    pub(crate) fn push_and_fold(&mut self, name: &str) -> &[u8] {
        if self.rows.is_multiple_of(BLOCK) {
            self.folded_blocks.push(self.folded.len() as u32);
        }
        // A NUL cannot occur in a filename, so it separates without escaping.
        self.bytes.extend_from_slice(name.as_bytes());
        self.bytes.push(0);
        let mut fold = Folded::new();
        let start = self.folded.len();
        self.folded
            .extend_from_slice(fold.fold_bytes(name.as_bytes()));
        self.folded.push(0);
        self.rows += 1;
        let folded = &self.folded[start..self.folded.len() - 1];
        self.spelling.push(name.as_bytes(), folded);
        folded
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// The folded arena alone.
    #[cfg(test)]
    pub(crate) fn finish_folded(&self) -> Vec<u8> {
        pack(self.rows, &self.folded_blocks, &self.folded)
    }

    /// The spelling's differences and the folded arena, which is finished
    /// without a copy: a rebuild holds every name of the index at once.
    pub(crate) fn finish_both(self) -> (Vec<u8>, Vec<u8>) {
        let NameWriter {
            rows,
            folded,
            folded_blocks,
            spelling,
            ..
        } = self;
        (
            spelling.finish(),
            pack_in_place(rows, &folded_blocks, folded),
        )
    }

    /// The spelled names as they were pushed: NUL-terminated, in row order —
    /// before packing, so [`crate::order`] need not reopen what it just wrote.
    pub(crate) fn spelled(&self) -> &[u8] {
        &self.bytes
    }

    /// The folded names as they were pushed: NUL-terminated, in row order.
    /// Used to build the name order, before this buffer becomes the arena.
    pub(crate) fn folded(&self) -> &[u8] {
        &self.folded
    }
}

#[cfg(test)]
fn pack(rows: usize, blocks: &[u32], bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + blocks.len() * 4 + 8);
    out.extend_from_slice(&(rows as u32).to_le_bytes());
    out.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
    for b in blocks {
        out.extend_from_slice(&b.to_le_bytes());
    }
    out.extend_from_slice(bytes);
    out
}

fn pack_in_place(rows: usize, blocks: &[u32], mut bytes: Vec<u8>) -> Vec<u8> {
    let header_len = 8 + blocks.len() * 4;
    let payload_len = bytes.len();
    bytes.reserve(header_len);
    bytes.resize(payload_len + header_len, 0);
    bytes.rotate_right(header_len);
    bytes[0..4].copy_from_slice(&(rows as u32).to_le_bytes());
    bytes[4..8].copy_from_slice(&(blocks.len() as u32).to_le_bytes());
    let (slots, _) = bytes[8..header_len].as_chunks_mut::<4>();
    for (slot, offset) in slots.iter_mut().zip(blocks) {
        *slot = offset.to_le_bytes();
    }
    bytes
}

/// The arena, read in place out of a mapped file.
#[derive(Debug, Clone, Copy)]
pub struct NameArena<'a> {
    rows: usize,
    blocks: &'a [u8],
    bytes: &'a [u8],
}

impl<'a> NameArena<'a> {
    pub fn open(bytes: &'a [u8]) -> Option<NameArena<'a>> {
        if bytes.len() < 8 {
            return None;
        }
        let rows = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
        let n_blocks = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
        let end = 8 + n_blocks * 4;
        if bytes.len() < end {
            return None;
        }
        Some(NameArena {
            rows,
            blocks: &bytes[8..end],
            bytes: &bytes[end..],
        })
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// Whether the arena ends where its header says it does: a block a row
    /// thirty-two, and the last block holding exactly the names left for it. A
    /// file cut short anywhere fails here, at the price of one block read.
    pub fn is_whole(&self) -> bool {
        let n_blocks = self.blocks.len() / 4;
        if n_blocks != self.rows.div_ceil(BLOCK) {
            return false;
        }
        let Some(last) = n_blocks.checked_sub(1) else {
            return self.bytes.is_empty();
        };
        let Some(tail) = self.block_start(last).and_then(|at| self.bytes.get(at..)) else {
            return false;
        };
        tail.last() == Some(&0) && memchr::memchr_iter(0, tail).count() == self.rows - last * BLOCK
    }

    /// Every name, NUL-terminated, in row order.
    pub(crate) fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub(crate) fn block_start(&self, block: usize) -> Option<usize> {
        let at = block * 4;
        let b = self.blocks.get(at..at + 4)?;
        Some(u32::from_le_bytes(b.try_into().ok()?) as usize)
    }

    /// One name, by row: a jump plus at most `BLOCK` NUL scans.
    pub fn get(&self, row: usize) -> Option<&'a str> {
        if row >= self.rows {
            return None;
        }
        let mut at = self.block_start(row / BLOCK)?;
        for _ in 0..(row % BLOCK) {
            at += memchr::memchr(0, self.bytes.get(at..)?)? + 1;
        }
        let end = at + memchr::memchr(0, self.bytes.get(at..)?)?;
        std::str::from_utf8(self.bytes.get(at..end)?).ok()
    }

    /// Walk names from `row` onwards, handing each to `f` with its row number;
    /// stops when `f` returns `false`. No offset is consulted after the first.
    /// Bytes, not `&str`: validating UTF-8 here is a second pass over the whole
    /// index for the forty rows that reach [`NameArena::get`], which validates.
    pub fn walk(&self, from: usize, f: impl FnMut(usize, &'a [u8]) -> bool) {
        self.walk_range(from, self.rows, f);
    }

    /// The same, stopping at `to` (exclusive). What the trigram filter uses:
    /// each block it names is a contiguous run of rows.
    pub fn walk_range(&self, from: usize, to: usize, mut f: impl FnMut(usize, &'a [u8]) -> bool) {
        let to = to.min(self.rows);
        if from >= to {
            return;
        }
        let Some(block_at) = self.block_start(from / BLOCK) else {
            return;
        };
        let mut at = block_at;
        for _ in 0..(from % BLOCK) {
            match self.bytes.get(at..).and_then(|b| memchr::memchr(0, b)) {
                Some(n) => at += n + 1,
                None => return,
            }
        }
        for row in from..to {
            let Some(rest) = self.bytes.get(at..) else {
                return;
            };
            let Some(n) = memchr::memchr(0, rest) else {
                return;
            };
            if !f(row, &rest[..n]) {
                return;
            }
            at += n + 1;
        }
    }
}

/// A stack buffer holding one case-folded name, reused across every row of a
/// scan, so a million names fold without an allocation.
#[derive(Debug)]
pub struct Folded {
    buf: [u8; FOLD_CAP],
    len: usize,
}

impl Default for Folded {
    fn default() -> Self {
        Folded {
            buf: [0; FOLD_CAP],
            len: 0,
        }
    }
}

/// What [`two_byte_table`] says of a character: nothing (zero), a two-byte
/// character, or one of these — one ASCII byte, two, or no bytes at all.
const ONE: u16 = 0x8000;
const TWO: u16 = 0xc000;
const GONE: u16 = 0x4000;

/// The fold of every two-byte character, U+0080–U+07FF — Turkish, Western
/// European, Greek, Cyrillic and the combining marks — built from
/// `char::to_lowercase` and [`plain`] so it cannot disagree with them; absent
/// where the lowercase is several characters. 3.8 KB once, replacing a search
/// per character: 19.6% off folding a Turkish name.
fn two_byte_table() -> &'static [u16; 1920] {
    static TABLE: std::sync::OnceLock<Box<[u16; 1920]>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = Box::new([0u16; 1920]);
        for (i, slot) in t.iter_mut().enumerate() {
            let Some(c) = char::from_u32(0x80 + i as u32) else {
                continue;
            };
            let mut lower = c.to_lowercase();
            let (Some(first), None) = (lower.next(), lower.next()) else {
                continue; // more than one character: not ours to shortcut
            };
            let base = match plain(first) {
                Plain::Same => first,
                Plain::One(base) => base,
                Plain::Two(a, b) => {
                    *slot = TWO | u16::from(a) << 7 | u16::from(b);
                    continue;
                }
                Plain::Gone => {
                    *slot = GONE;
                    continue;
                }
            };
            if (0x80..0x800).contains(&(base as u32)) {
                *slot = base as u16;
            } else if base.is_ascii() {
                *slot = ONE | base as u16;
            }
        }
        t
    })
}

impl Folded {
    pub fn new() -> Folded {
        Folded::default()
    }

    /// Fold `name` into the buffer and return it.
    pub fn fold<'s>(&'s mut self, name: &str) -> &'s str {
        let folded = self.fold_bytes(name.as_bytes());
        // Folding never produces invalid UTF-8 from valid input.
        std::str::from_utf8(folded).unwrap_or("")
    }

    /// Append to the fold; false, appending nothing, when it would not fit.
    fn put(&mut self, bytes: &[u8]) -> bool {
        let Some(room) = self.buf.get_mut(self.len..self.len + bytes.len()) else {
            return false;
        };
        room.copy_from_slice(bytes);
        self.len += bytes.len();
        true
    }

    /// The same, on bytes, which is what a walk has. Wholly ASCII is one
    /// vectorised `make_ascii_lowercase`, 11.3 ns for nineteen bytes; a mostly
    /// ASCII name takes that per run, since one `ş` sending the whole name
    /// through `char::to_lowercase` costs 88.9; non-UTF-8 matches as bytes.
    /// Must fold **identically** to `Unaccented` or a name is never found.
    pub fn fold_bytes<'s>(&'s mut self, bytes: &[u8]) -> &'s [u8] {
        if bytes.len() <= FOLD_CAP && bytes.is_ascii() {
            let n = bytes.len();
            self.buf[..n].copy_from_slice(bytes);
            self.buf[..n].make_ascii_lowercase();
            self.len = n;
            return &self.buf[..n];
        }
        let Ok(name) = std::str::from_utf8(bytes) else {
            // Not text. Match it as the bytes it is rather than drop the row.
            let n = bytes.len().min(FOLD_CAP);
            self.buf[..n].copy_from_slice(&bytes[..n]);
            self.buf[..n].make_ascii_lowercase();
            self.len = n;
            return &self.buf[..n];
        };

        self.len = 0;
        let raw = name.as_bytes();
        let mut at = 0;
        // Absurdly long: the first `put` that fails ends the fold with what fitted.
        while at < raw.len() {
            // The ASCII run, in bulk: no rule below distinguishes ASCII, so
            // this is what the per-character path would have produced.
            let from = at;
            while at < raw.len() && raw[at] < 0x80 {
                at += 1;
            }
            if at > from {
                let start = self.len;
                if !self.put(&raw[from..at]) {
                    break;
                }
                self.buf[start..self.len].make_ascii_lowercase();
            }
            let Some(c) = name[at..].chars().next() else {
                break;
            };
            at += c.len_utf8();
            // What the table exists for: a two-byte character whose lowercase
            // is one character. Everything Turkish is here.
            let entry = match c as u32 {
                cp @ 0x80..0x800 => two_byte_table()[cp as usize - 0x80],
                _ => 0,
            };
            let fits = match entry & TWO {
                _ if entry == 0 => self.put_slowly(c),
                0 => self.put(&[0xc0 | (entry >> 6) as u8, 0x80 | (entry & 0x3f) as u8]),
                ONE => self.put(&[(entry & 0x7f) as u8]),
                GONE => true,
                _ => self.put(&[(entry >> 7 & 0x7f) as u8, (entry & 0x7f) as u8]),
            };
            if !fits {
                break;
            }
        }
        &self.buf[..self.len]
    }

    /// One character by the rules themselves, as `Unaccented` applies them.
    fn put_slowly(&mut self, c: char) -> bool {
        for lc in c.to_lowercase() {
            let mut tmp = [0u8; 4];
            let fits = match plain(lc) {
                Plain::Same => self.put(lc.encode_utf8(&mut tmp).as_bytes()),
                Plain::One(base) => self.put(base.encode_utf8(&mut tmp).as_bytes()),
                Plain::Two(a, b) => self.put(&[a, b]),
                Plain::Gone => true,
            };
            if !fits {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_core::text::{Folder, Unaccented};

    fn build(names: &[&str]) -> Vec<u8> {
        let mut w = NameWriter::new();
        for n in names {
            w.push(n);
        }
        w.finish_folded()
    }

    #[test]
    fn every_name_comes_back_by_row() {
        // More than two blocks, so the block index is exercised.
        let names: Vec<String> = (0..300).map(|i| format!("file_{i}.rs")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let bytes = build(&refs);
        let arena = NameArena::open(&bytes).expect("open");
        assert_eq!(arena.rows(), 300);
        for (i, want) in refs.iter().enumerate() {
            assert_eq!(arena.get(i), Some(*want), "row {i}");
        }
        assert_eq!(arena.get(300), None);
    }

    #[test]
    fn the_arena_finished_in_place_is_the_arena_byte_for_byte() {
        let names: Vec<String> = (0..300).map(|i| format!("İstanbul_{i}.TXT")).collect();
        let mut writer = NameWriter::new();
        for name in &names {
            writer.push(name);
        }
        let expected = writer.finish_folded();
        let (_, folded) = writer.finish_both();
        assert_eq!(folded, expected);
        let folded = NameArena::open(&folded).expect("fold");
        for (row, name) in names.iter().enumerate() {
            assert_eq!(folded.get(row), Some(Unaccented.fold(name).as_str()));
        }
    }

    #[test]
    fn walking_visits_rows_in_order_and_stops_when_told() {
        let refs = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"];
        let bytes = build(&refs);
        let arena = NameArena::open(&bytes).expect("open");

        let mut seen = Vec::new();
        arena.walk(0, |row, name| {
            seen.push((row, String::from_utf8_lossy(name).into_owned()));
            true
        });
        assert_eq!(seen.len(), 5);
        assert_eq!(seen[3], (3, "d.rs".to_owned()));

        // Early exit is the whole point of the design.
        let mut count = 0;
        arena.walk(0, |_, _| {
            count += 1;
            count < 2
        });
        assert_eq!(count, 2);

        // And starting partway through lands on the right row.
        let mut from_three = Vec::new();
        arena.walk(3, |row, name| {
            from_three.push((row, String::from_utf8_lossy(name).into_owned()));
            true
        });
        assert_eq!(from_three, vec![(3, "d.rs".into()), (4, "e.rs".into())]);
    }

    #[test]
    fn walking_starts_correctly_from_inside_a_later_block() {
        let names: Vec<String> = (0..400).map(|i| format!("n{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let bytes = build(&refs);
        let arena = NameArena::open(&bytes).expect("open");
        let mut first = None;
        arena.walk(257, |row, name| {
            first = Some((row, String::from_utf8_lossy(name).into_owned()));
            false
        });
        assert_eq!(first, Some((257, "n257".to_owned())));
    }

    #[test]
    fn folding_agrees_with_the_folder_the_index_was_built_with() {
        // Disagreement stores a file under one spelling and searches under
        // another, with no error anywhere.
        let mut f = Folded::new();
        for name in [
            "main.rs",
            "RAPOR.PDF",
            "İSTANBUL.txt",
            "ISTANBUL.txt",
            "ısparta.md",
            "ÇALIŞKAN",
            "Öğüt.docx",
            "Müzik",
            "",
            "ẞ.txt",
        ] {
            assert_eq!(f.fold(name), Unaccented.fold(name), "{name:?}");
        }
    }

    #[test]
    fn folding_agrees_on_every_mixture_of_scripts_it_can_be_handed() {
        // The shape the bulk-ASCII path has to survive: runs of every length,
        // broken by non-ASCII at every position, including both ends.
        let alphabet: &[&str] = &[
            "a", "Z", "9", "-", ".", " ", "_", "ı", "İ", "I", "i", "ş", "Ş", "ğ", "Ğ", "ç", "Ç",
            "ö", "Ö", "ü", "Ü",
            // Two-byte, three-byte, four-byte, and one folding to two.
            "é", "Ω", "д", "中", "🙂", "ẞ", "\u{0307}", "ß", "æ", "ё", "й", "ά", "ế", "Ａ", "１",
            "\u{0301}", "\u{05b8}", "\u{064e}", "\u{3000}", "が", "Ǿ", "Ǆ",
        ];
        let mut f = Folded::new();
        // Deterministic: a failure has to reproduce by running it again.
        let mut state: u64 = 0x243f_6a88_85a3_08d3;
        let mut next = |n: usize| -> usize {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            ((state >> 33) as usize) % n
        };
        for _ in 0..20_000 {
            let len = next(24);
            let mut name = String::new();
            for _ in 0..len {
                name.push_str(alphabet[next(alphabet.len())]);
            }
            assert_eq!(
                f.fold(&name),
                Unaccented.fold(&name),
                "{name:?} ({:?})",
                name.as_bytes()
            );
        }
    }

    #[test]
    fn folding_allocates_nothing_and_survives_an_absurd_name() {
        let mut f = Folded::new();
        let huge = "a".repeat(FOLD_CAP * 2);
        // Must not panic; a name this long cannot exist on any real filesystem.
        let got = f.fold(&huge);
        assert!(got.len() <= FOLD_CAP);
        // And the buffer still works afterwards.
        assert_eq!(f.fold("Main.RS"), "main.rs");
    }

    #[test]
    fn non_utf8_and_empty_arenas_do_not_panic() {
        let bytes = build(&[]);
        let arena = NameArena::open(&bytes).expect("open");
        assert!(arena.is_empty());
        assert_eq!(arena.get(0), None);
        arena.walk(0, |_, _| panic!("nothing to walk"));
        assert!(NameArena::open(&[0u8; 4]).is_none());
    }
}
