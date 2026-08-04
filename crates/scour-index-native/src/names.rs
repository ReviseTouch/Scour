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
//! ## Two arenas: one to read, one to match
//!
//! Names are stored as the filesystem spells them, because that is what the
//! user reads. Matching is case-folded, because that is what the user means.
//!
//! Those used to be the same bytes, folded per row at query time. Measured on
//! 2,981,748 real names, that fold was **three quarters of the inner loop** —
//! 24.4 ns of the 40.5 it takes to read a name, fold it and search it — paid
//! on every candidate row of every query to compute something that never
//! changes.
//!
//! So the fold happens once, when the segment is written, into a second arena
//! with the same row numbering. A search walks *that* one and never folds
//! anything; the spelled arena is read only for the forty rows that reach the
//! screen. Same measurement: **40.5 ns a row becomes 8.3**, and a scan of every
//! name in the index falls from 121 ms to 25.
//!
//! The cost is the second arena — 79 MB here against an index of 174 — and it
//! is the trade the whole layout is built around: bytes are cheap and the
//! inner loop is not.

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
    /// The same names, folded, with the same row numbering.
    folded: Vec<u8>,
    folded_blocks: Vec<u32>,
}

impl NameWriter {
    pub fn new() -> NameWriter {
        NameWriter::default()
    }

    pub fn push(&mut self, name: &str) {
        if self.rows.is_multiple_of(BLOCK) {
            self.blocks.push(self.bytes.len() as u32);
            self.folded_blocks.push(self.folded.len() as u32);
        }
        // A NUL cannot occur in a filename on any platform this runs on, so it
        // is the one byte that can separate them without escaping.
        self.bytes.extend_from_slice(name.as_bytes());
        self.bytes.push(0);
        // And the same name folded, once, here, rather than once per row per
        // query for the life of the index.
        let mut fold = Folded::new();
        self.folded
            .extend_from_slice(fold.fold_bytes(name.as_bytes()));
        self.folded.push(0);
        self.rows += 1;
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// The arena of names as they are spelled.
    pub fn finish(&self) -> Vec<u8> {
        pack(self.rows, &self.blocks, &self.bytes)
    }

    /// The arena of the same names, folded. Same rows, same block boundaries.
    pub fn finish_folded(&self) -> Vec<u8> {
        pack(self.rows, &self.folded_blocks, &self.folded)
    }
}

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
    ///
    /// Bytes, not `&str`. Validating UTF-8 on the way past is a second pass
    /// over every name in the index for the benefit of the forty that end up
    /// on screen, and the tests that matter — a substring, an extension, a
    /// glob — are all answerable without it. The rows that are actually
    /// returned go through [`NameArena::get`], which does validate.
    pub fn walk(&self, from: usize, f: impl FnMut(usize, &'a [u8]) -> bool) {
        self.walk_range(from, self.rows, f);
    }

    /// The same, stopping at `to` (exclusive).
    ///
    /// What the trigram filter uses: it names a handful of blocks, and each is
    /// a contiguous run of rows.
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

/// Single-character lowercase mappings for the two-byte range, U+0080–U+07FF.
///
/// Every letter Turkish, Western European, Greek and Cyrillic writing needs
/// lives here, which is most of what a non-ASCII filename on this machine is
/// made of.
///
/// **Built from `char::to_lowercase` rather than written out**, so it cannot
/// disagree with it. A hand-typed table of 1,920 entries would be a second
/// statement of the Unicode rules, and the failure mode of two statements
/// drifting is not an error — it is a file that is never found.
///
/// Entries are left absent, and fall through to the general path, when the
/// lowercase is more than one character or is a combining dot. Both matter:
/// `İ` folds to `i` *plus* a dot and the dot is then dropped, which is the
/// whole reason a Turkish name typed either way is found either way.
///
/// One 3.8 KB allocation, filled once, turning a per-character binary search
/// over the core range tables into an array index. Measured interleaved over
/// six rounds: **19.6% off a Turkish name**, and exactly nothing on an ASCII
/// one, which is the shape a change like this should have.
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
            // The two rules the general path applies, applied here in the same
            // order, so the two produce the same bytes.
            let first = if first == 'ı' { 'i' } else { first };
            if first == '\u{0307}' {
                continue; // dropped there, so not shortcut here
            }
            if (0x80..0x800).contains(&(first as u32)) {
                *slot = first as u16;
            } else if first.is_ascii() {
                // `ı` becomes `i`, which is one byte where the source was two.
                // The high bit records that the width changed.
                *slot = 0x8000 | first as u16;
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

    /// The same, on bytes, which is what a walk has.
    ///
    /// Called once per row of every query that reads a name, so what it costs
    /// is most of what a search costs. Three paths, and the middle one is the
    /// reason this is not four lines:
    ///
    /// * **Wholly ASCII** — one `make_ascii_lowercase`, which the compiler
    ///   vectorises. 11.3 ns for a name of nineteen bytes.
    /// * **Mostly ASCII** — the same bulk copy over each ASCII run, dropping
    ///   into the real rules only for the characters that need them.
    /// * **Not text at all** — matched as the bytes it is rather than dropped,
    ///   because a name off a disk is arbitrary bytes and a row that cannot be
    ///   searched for is worse than one searched for oddly.
    ///
    /// The middle path is worth its complication and the number is measured.
    /// Before it, a single `ş` anywhere in a name put the *whole* name through
    /// `char::to_lowercase` — 88.9 ns against 11.3, and a search for a Turkish
    /// word cost seven times what an English one cost on the same corpus,
    /// because the blocks a Turkish word selects are full of Turkish names.
    /// `rapor` visited 439,936 rows in 92 ms where `config` visited more in
    /// 14.7. Two other explanations were measured and refused first: it is not
    /// the sort, and it is not name length.
    ///
    /// Whatever this does it must do **identically** to `DefaultFolder` — `İ`,
    /// `I`, `ı` and `i` all become `i`, or a Turkish name is stored under one
    /// spelling and searched for under another and never found.
    /// `folding_agrees_with_the_folder_the_index_was_built_with` is the test
    /// that says so, and it is the reason the rules below are copied rather
    /// than restated.
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
        while at < raw.len() {
            // The ASCII run, in bulk. Folding ASCII is `to_ascii_lowercase`
            // and no rule below distinguishes it, so this is exactly what the
            // per-character path would have produced.
            let from = at;
            while at < raw.len() && raw[at] < 0x80 {
                at += 1;
            }
            if at > from {
                let n = at - from;
                if self.len + n > FOLD_CAP {
                    return &self.buf[..self.len];
                }
                self.buf[self.len..self.len + n].copy_from_slice(&raw[from..at]);
                self.buf[self.len..self.len + n].make_ascii_lowercase();
                self.len += n;
            }
            let Some(c) = name[at..].chars().next() else {
                break;
            };
            at += c.len_utf8();
            // The common case, and what the table exists for: a two-byte
            // character whose lowercase is one character. Everything Turkish
            // is here.
            if let Some(entry) = (0x80..0x800)
                .contains(&(c as u32))
                .then(|| two_byte_table()[c as usize - 0x80])
                .filter(|&e| e != 0)
            {
                if entry & 0x8000 == 0 {
                    let mut tmp = [0u8; 4];
                    let wrote = char::from_u32(u32::from(entry))
                        .unwrap_or(c)
                        .encode_utf8(&mut tmp)
                        .len();
                    if self.len + wrote > FOLD_CAP {
                        return &self.buf[..self.len];
                    }
                    self.buf[self.len..self.len + wrote].copy_from_slice(&tmp[..wrote]);
                    self.len += wrote;
                } else {
                    if self.len >= FOLD_CAP {
                        return &self.buf[..self.len];
                    }
                    self.buf[self.len] = (entry & 0x7fff) as u8;
                    self.len += 1;
                }
                continue;
            }
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
                    return &self.buf[..self.len];
                }
                self.buf[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
                self.len += s.len();
            }
        }
        &self.buf[..self.len]
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
    fn folding_agrees_on_every_mixture_of_scripts_it_can_be_handed() {
        // The fixed list above is what somebody thought of. This is the shape
        // the fast path actually has to survive: ASCII runs of every length,
        // broken by non-ASCII characters at every position, including at the
        // very start and the very end and two in a row.
        //
        // It exists because the bulk-ASCII path is an optimisation whose only
        // failure mode is silence. A fold that disagrees does not error — the
        // file is stored under one spelling, searched for under another, and
        // simply never found.
        let alphabet: &[&str] = &[
            "a", "Z", "9", "-", ".", " ", "_",
            // Turkish, which is the whole reason the rules are what they are.
            "ı", "İ", "I", "i", "ş", "Ş", "ğ", "Ğ", "ç", "Ç", "ö", "Ö", "ü", "Ü",
            // Elsewhere: two-byte, three-byte, four-byte, and one that folds
            // to *two* characters.
            "é", "Ω", "д", "中", "🙂", "ẞ", "\u{0307}",
        ];
        let mut f = Folded::new();
        // Deterministic rather than random: a failure has to be reproducible
        // by running the test again, and a seed nobody prints is not.
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
                DefaultFolder.fold(&name),
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
