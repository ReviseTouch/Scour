//! How each name is spelled, stored as its difference from the fold.
//!
//! Seven names in ten fold to themselves, and most of the rest differ only in
//! bytes `0x20` apart — ASCII and Latin-1 capitals. So the spelling is the
//! folded arena plus, a block: two bits a row saying how it differs, and for
//! each row that does a bit a byte (flip `0x20`), two bits a byte (that, `ı`,
//! `İ`, or a neighbour like `Ş`) or, failing both, the whole spelling.

use std::borrow::Cow;

use crate::columns::BLOCK;
use crate::names::NameArena;

/// The two row bitmaps a block with any difference begins with.
const HEAD: usize = 8;

/// Collects the differences as names are pushed, a block at a time.
#[derive(Debug, Default)]
pub(crate) struct SpellingWriter {
    /// Where each block's differences begin, and a final entry at the end.
    blocks: Vec<u32>,
    bytes: Vec<u8>,
    rows: usize,
    /// The block being filled: the low and high bit of each row's [`Kind`],
    /// then the body.
    low: u32,
    high: u32,
    body: Vec<u8>,
    /// One code a folded byte, for the name being pushed.
    codes: Vec<u8>,
}

/// How a row's spelling is stored: nothing, one bit a folded byte, two bits a
/// folded byte, or the spelling itself and a NUL.
const SAME: u32 = 0;
const FLIPS: u32 = 1;
const PAIRS: u32 = 2;
const WHOLE: u32 = 3;

/// What a folded byte was spelled as: itself, `0x20` away, or one of two
/// others — for `i` the dotless `ı` and the dotted `İ`, else the byte one away.
const KEPT: u8 = 0;
const FLIPPED: u8 = 1;
const OTHER: u8 = 2;
const DOTTED: u8 = 3;

const DOTLESS_I: [u8; 2] = [0xc4, 0xb1];
const DOTTED_I: [u8; 2] = [0xc4, 0xb0];

/// One code a folded byte, if the spelling is the fold under those codes.
fn align(spelled: &[u8], folded: &[u8], codes: &mut Vec<u8>) -> bool {
    codes.clear();
    let mut at = 0usize;
    for &f in folded {
        let Some(&s) = spelled.get(at) else {
            return false;
        };
        let rest = &spelled[at..];
        let (code, took) = if s == f {
            (KEPT, 1)
        } else if s ^ 0x20 == f {
            (FLIPPED, 1)
        } else if f == b'i' && rest.starts_with(&DOTLESS_I) {
            (OTHER, 2)
        } else if f == b'i' && rest.starts_with(&DOTTED_I) {
            (DOTTED, 2)
        } else if s ^ 1 == f && f != b'i' {
            (OTHER, 1)
        } else {
            return false;
        };
        codes.push(code);
        at += took;
    }
    at == spelled.len()
}

/// The spelling of a folded name under one code a byte.
fn respell(name: &[u8], code: impl Fn(usize) -> u8, out: &mut Vec<u8>) {
    for (i, &b) in name.iter().enumerate() {
        match (code(i), b) {
            (KEPT, _) => out.push(b),
            (FLIPPED, _) => out.push(b ^ 0x20),
            (OTHER, b'i') => out.extend_from_slice(&DOTLESS_I),
            (DOTTED, b'i') => out.extend_from_slice(&DOTTED_I),
            (OTHER, _) => out.push(b ^ 1),
            _ => out.push(b),
        }
    }
}

fn bit(mask: &[u8], i: usize) -> u8 {
    mask[i / 8] >> (i % 8) & 1
}

impl SpellingWriter {
    pub(crate) fn push(&mut self, spelled: &[u8], folded: &[u8]) {
        if self.rows.is_multiple_of(BLOCK) {
            self.close_block();
        }
        let bit = 1u32 << (self.rows % BLOCK);
        self.rows += 1;
        if spelled == folded {
            return;
        }
        let kind = if !align(spelled, folded, &mut self.codes) {
            // A NUL cannot occur in a filename, so it ends the spelling.
            self.body.extend_from_slice(spelled);
            self.body.push(0);
            WHOLE
        } else {
            let wide = self.codes.iter().any(|&c| c > FLIPPED);
            let bytes = folded.len().div_ceil(8);
            let at = self.body.len();
            self.body.resize(at + bytes * if wide { 2 } else { 1 }, 0);
            for (i, &code) in self.codes.iter().enumerate() {
                self.body[at + i / 8] |= (code & 1) << (i % 8);
                if wide {
                    self.body[at + bytes + i / 8] |= (code >> 1) << (i % 8);
                }
            }
            if wide { PAIRS } else { FLIPS }
        };
        if kind & 1 != 0 {
            self.low |= bit;
        }
        if kind & 2 != 0 {
            self.high |= bit;
        }
    }

    fn close_block(&mut self) {
        if self.rows > 0 && self.low | self.high != 0 {
            self.bytes.extend_from_slice(&self.low.to_le_bytes());
            self.bytes.extend_from_slice(&self.high.to_le_bytes());
            self.bytes.append(&mut self.body);
        }
        self.low = 0;
        self.high = 0;
        self.blocks.push(self.bytes.len() as u32);
    }

    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.rows > 0 {
            self.close_block();
        }
        let mut out = Vec::with_capacity(8 + self.blocks.len() * 4 + self.bytes.len());
        out.extend_from_slice(&(self.rows as u32).to_le_bytes());
        out.extend_from_slice(&(self.rows.div_ceil(BLOCK) as u32).to_le_bytes());
        for b in &self.blocks {
            out.extend_from_slice(&b.to_le_bytes());
        }
        out.extend_from_slice(&self.bytes);
        out
    }
}

/// The names as the filesystem spells them, read in place out of two mapped
/// files: the folded arena and the differences.
#[derive(Debug, Clone, Copy)]
pub struct Spelling<'a> {
    folded: NameArena<'a>,
    rows: usize,
    /// One offset a block and one more: a block's differences end where the
    /// next begin.
    blocks: &'a [u8],
    bytes: &'a [u8],
}

impl<'a> Spelling<'a> {
    pub fn open(folded: NameArena<'a>, bytes: &'a [u8]) -> Option<Spelling<'a>> {
        let rows = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
        let n_blocks = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?) as usize;
        let table = if rows == 0 {
            0
        } else {
            n_blocks.checked_add(1)?
        };
        let end = 8usize.checked_add(table.checked_mul(4)?)?;
        if n_blocks != rows.div_ceil(BLOCK) || bytes.len() < end {
            return None;
        }
        Some(Spelling {
            folded,
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

    /// Whether the file is for this fold and ends where its table says: the
    /// last offset is the length, and no block starts after the one behind it.
    pub fn is_whole(&self) -> bool {
        if self.rows != self.folded.rows() {
            return false;
        }
        let n = self.blocks.len() / 4;
        if n == 0 {
            return self.bytes.is_empty();
        }
        let mut last = 0usize;
        for block in 0..n {
            let Some(at) = self.offset(block) else {
                return false;
            };
            // A block with differences holds its two bitmaps at least.
            if at < last || (at > last && at - last < HEAD) {
                return false;
            }
            last = at;
        }
        last == self.bytes.len()
    }

    fn offset(&self, block: usize) -> Option<usize> {
        let at = block * 4;
        let b = self.blocks.get(at..at + 4)?;
        Some(u32::from_le_bytes(b.try_into().ok()?) as usize)
    }

    /// A block's two bitmaps and where its first difference begins. No
    /// differences, or a block the file does not hold whole: all zero.
    fn block(&self, block: usize) -> (u32, u32, usize) {
        let (Some(from), Some(to)) = (self.offset(block), self.offset(block + 1)) else {
            return (0, 0, 0);
        };
        if to < from + HEAD {
            return (0, 0, 0);
        }
        let Some(head) = self.bytes.get(from..from + HEAD) else {
            return (0, 0, 0);
        };
        let word = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        (word(&head[0..4]), word(&head[4..8]), from + HEAD)
    }

    /// One name, by row: a jump and at most `BLOCK` NUL scans. Borrowed unless
    /// the row is spelled with capitals. A caller asking in row order wants
    /// [`Reader`], which is this without the block scan or the allocation.
    pub fn get(&self, row: usize) -> Option<Cow<'a, str>> {
        let (name, patch) = Reader::default().find(self, row)?;
        match patch {
            Patch::Same => std::str::from_utf8(name).ok().map(Cow::Borrowed),
            Patch::Whole(spelled) => std::str::from_utf8(spelled).ok().map(Cow::Borrowed),
            patch => {
                let mut out = Vec::with_capacity(name.len() + 8);
                patch.respell(name, &mut out);
                String::from_utf8(out).ok().map(Cow::Owned)
            }
        }
    }
}

/// What a row's spelling is, beside its folded name.
enum Patch<'a> {
    Same,
    Whole(&'a [u8]),
    /// A bit a byte of the folded name: set where the spelling is `0x20` away.
    Flips(&'a [u8]),
    /// Two bits a byte: the low bits of every code, then the high bits.
    Pairs(&'a [u8], &'a [u8]),
}

impl Patch<'_> {
    fn respell(&self, name: &[u8], out: &mut Vec<u8>) {
        match self {
            Patch::Same | Patch::Whole(_) => out.extend_from_slice(name),
            // Nearly every name that differs: a copy, and a flip a capital.
            Patch::Flips(mask) => {
                let from = out.len();
                out.extend_from_slice(name);
                for (at, &bits) in mask.iter().enumerate() {
                    let mut bits = bits;
                    while bits != 0 {
                        if let Some(b) = out.get_mut(from + at * 8 + bits.trailing_zeros() as usize)
                        {
                            *b ^= 0x20;
                        }
                        bits &= bits - 1;
                    }
                }
            }
            Patch::Pairs(low, high) => respell(name, |i| bit(low, i) | bit(high, i) << 1, out),
        }
    }
}

/// A place in both files, kept between reads, so a forward step is one NUL
/// scan rather than a scan from the block's start — 320 ms of a 665 ms path
/// sort. Never worse than `get`: the shorter of the two distances is taken.
#[derive(Debug, Default)]
pub(crate) struct Reader {
    /// Where `row` begins in the folded arena, and where its difference would.
    /// Meaningless until `placed`.
    at: usize,
    patch: usize,
    row: usize,
    placed: bool,
    /// The last name that had to be rebuilt; reused, so a walk allocates once.
    built: Vec<u8>,
}

impl Reader {
    /// The name of `row`, spelled as the filesystem spells it. Empty for a row
    /// the arena lacks or whose bytes are not UTF-8 — what
    /// `Spelling::get(row).unwrap_or_default()` gives, and so what a sort key
    /// must be, or rows order by something the shown path does not say.
    pub(crate) fn at<'r>(&'r mut self, names: &Spelling<'r>, row: usize) -> &'r str {
        self.read(names, row).unwrap_or_default()
    }

    /// [`Reader::at`], saying nothing where that says "": a row to skip.
    pub(crate) fn read<'r>(&'r mut self, names: &Spelling<'r>, row: usize) -> Option<&'r str> {
        let (name, patch) = self.find(names, row)?;
        let spelled: &'r [u8] = match patch {
            Patch::Same => name,
            Patch::Whole(spelled) => spelled,
            patch => {
                self.built.clear();
                patch.respell(name, &mut self.built);
                &self.built
            }
        };
        std::str::from_utf8(spelled).ok()
    }

    /// The row's folded name and what its spelling adds, stepping on from the
    /// last row read when that is nearer than the block's start.
    fn find<'a>(&mut self, names: &Spelling<'a>, row: usize) -> Option<(&'a [u8], Patch<'a>)> {
        if row >= names.rows {
            return None;
        }
        let folded = names.folded.bytes();
        let within = row % BLOCK;
        let (low, high, body) = names.block(row / BLOCK);
        let kind = |k: usize| (low >> k & 1) | (high >> k & 1) << 1;
        let (mut at, mut patch, mut k) = match row.checked_sub(self.row) {
            Some(ahead) if self.placed && ahead <= within => (self.at, self.patch, within - ahead),
            _ => (names.folded.block_start(row / BLOCK)?, body, 0),
        };
        while k < within {
            let n = memchr::memchr(0, folded.get(at..)?)?;
            patch += match kind(k) {
                SAME => 0,
                FLIPS => n.div_ceil(8),
                PAIRS => 2 * n.div_ceil(8),
                _ => memchr::memchr(0, names.bytes.get(patch..)?)? + 1,
            };
            at += n + 1;
            k += 1;
        }
        let n = memchr::memchr(0, folded.get(at..)?)?;
        let name = &folded[at..at + n];
        self.at = at;
        self.patch = patch;
        self.row = row;
        self.placed = true;
        let bytes = n.div_ceil(8);
        let found = match kind(within) {
            SAME => Patch::Same,
            FLIPS => Patch::Flips(names.bytes.get(patch..patch + bytes)?),
            PAIRS => {
                let both = names.bytes.get(patch..patch + 2 * bytes)?;
                Patch::Pairs(&both[..bytes], &both[bytes..])
            }
            _ => {
                let rest = names.bytes.get(patch..)?;
                Patch::Whole(&rest[..memchr::memchr(0, rest)?])
            }
        };
        Some((name, found))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::NameWriter;

    const NAMES: &[&str] = &[
        "main.rs",
        "README.md",
        "Makefile",
        "IMG_0042.JPG",
        "İSTANBUL.txt",
        "ısparta.md",
        "ÇALIŞKAN",
        "Öğüt.docx",
        "ÜÖÇ.PDF",
        "",
        "ẞ.txt",
        "Привет.TXT",
        "KENAN PİLEVNELİ.docx",
        "para isteme yazısı.docx",
        "ŞIRNAK MERKEZ (İMAR İŞLEMLERİ).docx",
        "Ğ",
        "hi",
        "a",
        "A",
    ];

    /// Enough rows for several blocks, capitals at every place in a block.
    fn corpus() -> Vec<String> {
        (0..333)
            .map(|i| {
                let base = NAMES[(i * 7 + i / 5) % NAMES.len()];
                if i % 3 == 0 {
                    base.to_owned()
                } else {
                    format!("{base}{i}")
                }
            })
            .collect()
    }

    fn build(names: &[String]) -> (Vec<u8>, Vec<u8>) {
        let mut w = NameWriter::new();
        for n in names {
            w.push(n);
        }
        w.finish_both()
    }

    #[test]
    fn every_spelling_comes_back_by_row() {
        let names = corpus();
        let (spell, folded) = build(&names);
        let arena = NameArena::open(&folded).expect("fold");
        let s = Spelling::open(arena, &spell).expect("open");
        assert!(s.is_whole());
        assert_eq!(s.rows(), names.len());
        for (row, want) in names.iter().enumerate() {
            assert_eq!(s.get(row).as_deref(), Some(want.as_str()), "row {row}");
        }
        assert_eq!(s.get(names.len()), None);
    }

    #[test]
    fn a_reader_answers_whatever_get_would_have_answered() {
        let names = corpus();
        let (spell, folded) = build(&names);
        let s = Spelling::open(NameArena::open(&folded).expect("fold"), &spell).expect("open");
        let n = names.len();
        let orders: Vec<Vec<usize>> = vec![
            (0..n).collect(),
            (0..n).rev().collect(),
            (0..n).step_by(32).collect(),
            (0..n).step_by(33).collect(),
            (0..n).step_by(5).collect(),
            (30..40).chain(62..72).chain((30..40).rev()).collect(),
            vec![7, 7, 7, n - 1, 0, n - 1, 0, 32, 31, 32],
            vec![5, n, 6, 4_000, 7],
        ];
        for order in orders {
            let mut r = Reader::default();
            for row in order {
                let want = s.get(row).unwrap_or_default();
                assert_eq!(r.at(&s, row), want, "row {row}");
            }
        }
    }

    #[test]
    fn lowercase_names_cost_one_offset_a_block() {
        let names: Vec<String> = (0..320).map(|i| format!("dosya_{i}.txt")).collect();
        let (spell, _) = build(&names);
        assert_eq!(spell.len(), 8 + 11 * 4);
    }

    #[test]
    fn a_capital_costs_a_bit_a_byte_and_a_turkish_one_two() {
        // The header, two offsets and the block's two bitmaps are 24 bytes.
        let size = |name: &str| build(&[name.to_owned()]).0.len() - 24;
        assert_eq!(size("README.md"), 2);
        assert_eq!(size("KENAN PİLEVNELİ.docx"), 6);
        assert_eq!(size("yazısı"), 2);
        assert_eq!(size("ŞIRNAK"), 2);
        // Folding changed its length in a way no code says: stored whole.
        assert_eq!(size("ẞ.txt"), "ẞ.txt".len() + 1);
    }

    #[test]
    fn a_file_cut_anywhere_is_refused_or_read_and_never_a_panic() {
        let names = corpus();
        let (spell, folded) = build(&names);
        let arena = NameArena::open(&folded).expect("fold");
        for cut in 0..spell.len() {
            let Some(s) = Spelling::open(arena, &spell[..cut]) else {
                continue;
            };
            assert!(!s.is_whole(), "cut at {cut} of {}", spell.len());
            let mut r = Reader::default();
            for row in 0..names.len() {
                let _ = s.get(row);
                let _ = r.at(&s, row);
            }
        }
        // Not for this fold: a row short.
        let (other, _) = build(&names[1..]);
        assert!(!Spelling::open(arena, &other).is_some_and(|s| s.is_whole()));
    }

    #[test]
    fn bytes_that_are_not_a_spelling_are_read_without_a_panic() {
        let names = corpus();
        let (spell, folded) = build(&names);
        let arena = NameArena::open(&folded).expect("fold");
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..200 {
            let mut bad = spell.clone();
            for _ in 0..8 {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                let at = (state >> 33) as usize % bad.len();
                bad[at] = (state >> 13) as u8;
            }
            let Some(s) = Spelling::open(arena, &bad) else {
                continue;
            };
            let mut r = Reader::default();
            for row in 0..names.len() {
                let _ = s.get(row);
                let _ = r.at(&s, row);
            }
        }
    }
}
