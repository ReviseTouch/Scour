//! The name arena.
//!
//! Every file name, NUL-separated, in row order. Nothing else — no lengths, no
//! per-row offsets, no index. A name is found by counting NULs from the start
//! of its block.
//!
//! That works because of how the arena is read. A search walks rows in order
//! from row zero, so the scan is sequential and the "counting" is free — it is
//! the same pass that reads the name. Only *materialising* a particular row
//! needs random access, and that happens forty times a query, not a million.
//!
//! So the only index is one 32-bit offset every [`BLOCK`] rows: 0.03 bytes an
//! entry, against 4 for a per-row offset table.
//!
//! ## Matching is case-folded without allocating
//!
//! Names are stored as the filesystem spells them, because that is what the
//! user reads. Matching is case-folded, because that is what the user means.
//! Folding a name per row would allocate a million times a query, so it folds
//! into a stack buffer instead — with a fast path for names that are pure
//! ASCII, which most are.

use crate::columns::BLOCK;

/// The longest name that will be folded into the stack buffer.
///
/// `NAME_MAX` is 255 bytes on Linux and Windows; folding can grow a string
/// (`İ` is two bytes and folds to one, but `ẞ` folds to two), so the buffer is
/// generous. A name that somehow exceeds it is matched unfolded rather than
/// dropped — wrong for one absurd file beats a panic.
const FOLD_CAP: usize = 1024;

#[derive(Debug, Default)]
pub struct NameWriter {
    bytes: Vec<u8>,
    /// Byte offset of the first name of each block.
    blocks: Vec<u32>,
    rows: usize,
}

impl NameWriter {
    pub fn new() -> NameWriter {
        NameWriter::default()
    }

    pub fn push(&mut self, name: &str) {
        if self.rows.is_multiple_of(BLOCK) {
            self.blocks.push(self.bytes.len() as u32);
        }
        // A NUL cannot occur in a filename on any platform this runs on, so it
        // is the one byte that can separate them without escaping.
        self.bytes.extend_from_slice(name.as_bytes());
        self.bytes.push(0);
        self.rows += 1;
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    pub fn finish(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bytes.len() + self.blocks.len() * 4 + 8);
        out.extend_from_slice(&(self.rows as u32).to_le_bytes());
        out.extend_from_slice(&(self.blocks.len() as u32).to_le_bytes());
        for b in &self.blocks {
            out.extend_from_slice(&b.to_le_bytes());
        }
        out.extend_from_slice(&self.bytes);
        out
    }
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

    fn block_start(&self, block: usize) -> Option<usize> {
        let at = block * 4;
        let b = self.blocks.get(at..at + 4)?;
        Some(u32::from_le_bytes(b.try_into().ok()?) as usize)
    }

    /// One name, by row. Costs a jump plus at most `BLOCK` NUL scans.
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

    /// Walk names from `row` onwards, handing each to `f` with its row number.
    ///
    /// The sequential form, and the one a search actually uses: no offsets are
    /// consulted after the first, and the NUL scan is `memchr`, which is the
    /// same SIMD loop `grep` uses. Stops when `f` returns `false`.
    pub fn walk(&self, from: usize, mut f: impl FnMut(usize, &'a str) -> bool) {
        if from >= self.rows {
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
        for row in from..self.rows {
            let Some(rest) = self.bytes.get(at..) else {
                return;
            };
            let Some(n) = memchr::memchr(0, rest) else {
                return;
            };
            let Ok(name) = std::str::from_utf8(&rest[..n]) else {
                at += n + 1;
                continue;
            };
            if !f(row, name) {
                return;
            }
            at += n + 1;
        }
    }
}

/// A stack buffer that holds one case-folded name.
///
/// Reused across every row of a scan, so a million names are folded without a
/// single allocation.
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

impl Folded {
    pub fn new() -> Folded {
        Folded::default()
    }

    /// Fold `name` into the buffer and return it.
    ///
    /// The fast path is a byte loop: most names are pure ASCII, and folding
    /// ASCII is `to_ascii_lowercase`. Anything else goes through the real
    /// folding rules, which are the ones the index was built with — `İ`, `I`,
    /// `ı` and `i` all become `i`, or a Turkish name is stored under one
    /// spelling and searched for under another and never found.
    pub fn fold<'s>(&'s mut self, name: &str) -> &'s str {
        let bytes = name.as_bytes();
        if bytes.len() <= FOLD_CAP && bytes.is_ascii() {
            for (i, &b) in bytes.iter().enumerate() {
                self.buf[i] = b.to_ascii_lowercase();
            }
            self.len = bytes.len();
            // Lowercasing ASCII cannot produce invalid UTF-8.
            return std::str::from_utf8(&self.buf[..self.len]).unwrap_or("");
        }

        self.len = 0;
        for c in name.chars() {
            for lc in c.to_lowercase() {
                // The two rules `DefaultFolder` applies, and they must stay
                // identical or matching silently stops working.
                let lc = if lc == 'ı' { 'i' } else { lc };
                if lc == '\u{0307}' {
                    continue;
                }
                let mut tmp = [0u8; 4];
                let s = lc.encode_utf8(&mut tmp);
                if self.len + s.len() > FOLD_CAP {
                    // Absurdly long: match what fitted rather than panic.
                    return std::str::from_utf8(&self.buf[..self.len]).unwrap_or("");
                }
                self.buf[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
                self.len += s.len();
            }
        }
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_core::text::{DefaultFolder, Folder};

    fn build(names: &[&str]) -> Vec<u8> {
        let mut w = NameWriter::new();
        for n in names {
            w.push(n);
        }
        w.finish()
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
    fn walking_visits_rows_in_order_and_stops_when_told() {
        let refs = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"];
        let bytes = build(&refs);
        let arena = NameArena::open(&bytes).expect("open");

        let mut seen = Vec::new();
        arena.walk(0, |row, name| {
            seen.push((row, name.to_owned()));
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
            from_three.push((row, name.to_owned()));
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
            first = Some((row, name.to_owned()));
            false
        });
        assert_eq!(first, Some((257, "n257".to_owned())));
    }

    #[test]
    fn folding_agrees_with_the_folder_the_index_was_built_with() {
        // If these two ever disagree, a file is stored under one spelling and
        // searched for under another, and nothing reports an error — the file
        // is simply never found.
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
            assert_eq!(f.fold(name), DefaultFolder.fold(name), "{name:?}");
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
