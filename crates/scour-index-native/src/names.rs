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
mod tests;
