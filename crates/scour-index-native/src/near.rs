//! A term forgiven its typing mistakes, and letters with anything between.
//!
//! [`Near`] asks whether a name holds a term, or something within a few edits
//! of it — a letter wrong, missing, extra, or two swapped. The term spelled
//! right is found anywhere in the name; a misspelling only where a word of the
//! name begins, because anywhere is everywhere: one mistake let `salih` find
//! `universal_hash` and `neslihan`, and a longer word found more than a shorter.
//!
//! A first pass over the name's bytes keeps the column of the edit-distance
//! table as bits (Myers; Hyyrö for the swap), so a row costs its length and not
//! its length times the term's; it says whether any part is near. Only a name
//! it passes without the right spelling is read again, a cell at a time, from
//! where its words begin.

use crate::trigram::TrigramIndex;

/// The longest term the bit columns hold.
pub(crate) const WIDEST: usize = 64;

/// Where a mistake may be forgiven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rule {
    /// Anywhere in a match that begins where a word of the name begins.
    #[default]
    WordStart,
    /// The same, and the start of the term is held to what a term that short
    /// is forgiven: the first four letters exactly, the first ten with one
    /// mistake. A longer term then never finds what a shorter one does not.
    Strict,
}

/// A term of at most [`WIDEST`] characters and how many mistakes it is forgiven.
#[derive(Debug)]
pub struct Near {
    rule: Rule,
    /// The term, a character a place, for the second reading.
    chars: Vec<char>,
    /// Where each ASCII byte stands in the term, a bit a place.
    ascii: [u64; 128],
    /// The same for the term's other characters, by their UTF-8 bytes.
    wide: Vec<([u8; 4], u8, u64)>,
    /// The term's length in characters.
    len: u32,
    edits: u32,
    folded: Vec<u8>,
    /// Which pairs of bytes the term holds, hashed to a bit each, and how many
    /// places in a name must hold one of them: every pair of the term's but
    /// the few each mistake can spoil. Zero when that says nothing.
    pairs: [u64; 64],
    pairs_needed: u32,
}

/// A pair of bytes as a bit of [`Near::pairs`]. Two pairs may share one, which
/// only lets a name through to the columns.
fn pair_bit(a: u8, b: u8) -> (usize, u64) {
    let hash = (usize::from(a) << 4) ^ usize::from(b);
    (hash >> 6 & 63, 1u64 << (hash & 63))
}

impl Near {
    /// `None` for a term the columns cannot hold, or an empty one.
    pub fn new(folded: &str, edits: u8, rule: Rule) -> Option<Near> {
        let len = folded.chars().count();
        if len == 0 || len > WIDEST {
            return None;
        }
        let mut near = Near {
            rule,
            chars: folded.chars().collect(),
            ascii: [0; 128],
            wide: Vec::new(),
            len: len as u32,
            edits: u32::from(edits),
            folded: folded.as_bytes().to_vec(),
            pairs: [0; 64],
            pairs_needed: 0,
        };
        for w in folded.as_bytes().windows(2) {
            let (word, bit) = pair_bit(w[0], w[1]);
            near.pairs[word] |= bit;
        }
        // A swap of two neighbours spoils the pairs over both, and no mistake more.
        let lens: Vec<usize> = folded.chars().map(char::len_utf8).collect();
        let spoiled = lens
            .windows(2)
            .map(|w| w[0] + w[1])
            .max()
            .unwrap_or(folded.len())
            + 1;
        near.pairs_needed = (folded.len() - 1).saturating_sub(usize::from(edits) * spoiled) as u32;
        for (place, c) in folded.chars().enumerate() {
            let bit = 1u64 << place;
            if c.is_ascii() {
                near.ascii[c as usize] |= bit;
                continue;
            }
            let mut bytes = [0u8; 4];
            let n = c.encode_utf8(&mut bytes).len() as u8;
            match near.wide.iter_mut().find(|w| w.0 == bytes) {
                Some(w) => w.2 |= bit,
                None => near.wide.push((bytes, n, bit)),
            }
        }
        Some(near)
    }

    /// Is some part of this **already folded** name near enough to the term?
    pub fn found_in(&self, name: &[u8]) -> bool {
        // The pairs first: a load a byte with nothing carried between them,
        // where a column is a dozen operations each waiting on the last.
        if self.pairs_needed > 0 {
            let mut held = 0u32;
            for w in name.windows(2) {
                let (word, bit) = pair_bit(w[0], w[1]);
                held += u32::from(self.pairs[word] & bit != 0);
            }
            if held < self.pairs_needed {
                return false;
            }
        }
        // A column of the table, as the differences between its cells: `up`
        // where a cell is one more than the cell above, `down` where one less.
        let (mut up, mut down) = (!0u64, 0u64);
        // The diagonal of the last column and its matches, for the swap.
        let (mut diagonal, mut before) = (!0u64, 0u64);
        // The bottom cell: the fewest edits to end a match at this character.
        let mut score = self.len;
        let last = self.len - 1;
        let mut at = 0usize;
        // Some part near enough, though perhaps not where a word begins.
        let mut nearly = false;
        while at < name.len() {
            let b = name[at];
            let matches = if b < 0x80 {
                at += 1;
                self.ascii[b as usize]
            } else {
                self.wide_at(name, &mut at)
            };
            let swapped = ((!diagonal & matches) << 1) & before;
            diagonal = (((matches & up).wrapping_add(up)) ^ up) | matches | down | swapped;
            let right = down | !(diagonal | up);
            let left = up & diagonal;
            // Never both, and no branch: which of them it is is a coin toss.
            score = score + (right >> last & 1) as u32 - (left >> last & 1) as u32;
            // Spelled right: anywhere will do.
            if score == 0 {
                return true;
            }
            nearly |= score <= self.edits;
            // Nothing shifted in: a match may begin at any character.
            let (right, left) = (right << 1, left << 1);
            up = left | !(diagonal | right);
            down = right & diagonal;
            before = matches;
        }
        nearly && self.begun_at_a_word(name)
    }

    /// The table a cell at a time, its matches begun only where a word of the
    /// name begins, and each row held to what the rule allows it.
    #[cold]
    fn begun_at_a_word(&self, name: &[u8]) -> bool {
        const NONE: u32 = u32::MAX / 2;
        let text = String::from_utf8_lossy(name);
        let n: Vec<char> = text.chars().collect();
        let t = &self.chars;
        let allowed = |i: usize| match self.rule {
            Rule::WordStart => self.edits,
            Rule::Strict => self.edits.min(u32::from(scour_core::mistakes_allowed(
                &t[..i].iter().collect::<String>(),
            ))),
        };
        // Rows of the table: the term's first `i` characters against the name.
        let width = n.len() + 1;
        let mut two_up = vec![NONE; width];
        let mut up: Vec<u32> = (0..width)
            .map(|j| {
                if j < n.len() && word_starts(&n, j) {
                    0
                } else {
                    NONE
                }
            })
            .collect();
        let mut row = vec![NONE; width];
        for i in 1..=t.len() {
            let cap = allowed(i);
            // A swap is a mistake the row above already carries: it may not
            // step over a row that allows none.
            let cap_above = allowed(i - 1);
            row[0] = up[0].saturating_add(1);
            for j in 1..width {
                let cost = u32::from(t[i - 1] != n[j - 1]);
                let mut best = up[j]
                    .saturating_add(1)
                    .min(row[j - 1].saturating_add(1))
                    .min(up[j - 1].saturating_add(cost));
                if i > 1
                    && j > 1
                    && t[i - 1] == n[j - 2]
                    && t[i - 2] == n[j - 1]
                    && two_up[j - 2].saturating_add(1) <= cap_above
                {
                    best = best.min(two_up[j - 2].saturating_add(1));
                }
                row[j] = best;
            }
            for cell in &mut row {
                if *cell > cap {
                    *cell = NONE;
                }
            }
            std::mem::swap(&mut two_up, &mut up);
            std::mem::swap(&mut up, &mut row);
        }
        up.iter().any(|&d| d < NONE)
    }

    /// Where the character at `at` stands in the term, stepping over it: one
    /// character by its lead byte, a stray byte its own.
    #[cold]
    fn wide_at(&self, name: &[u8], at: &mut usize) -> u64 {
        let n = match name[*at] {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        }
        .min(name.len() - *at);
        let bytes = &name[*at..*at + n];
        *at += n;
        self.wide
            .iter()
            .find(|w| &w.0[..w.1 as usize] == bytes)
            .map_or(0, |w| w.2)
    }

    /// The blocks whose names can be near the term, when the trigram index
    /// can say. One mistake is two neighbours swapped — and the name then holds
    /// that spelling whole — or it spoils the windows over one character.
    /// Two are bounded together, by the most windows each can spoil.
    pub(crate) fn blocks(&self, tri: &TrigramIndex<'_>) -> Option<Vec<u32>> {
        let term: Vec<char> = std::str::from_utf8(&self.folded).ok()?.chars().collect();
        let widest = term.iter().map(|c| c.len_utf8()).max()?;
        let pair = term
            .windows(2)
            .map(|w| w[0].len_utf8() + w[1].len_utf8())
            .max()
            .unwrap_or(widest);
        if self.edits != 1 {
            return tri.candidates_near(&self.folded, self.edits as usize * (pair + 2));
        }
        let mut blocks = tri.candidates_near(&self.folded, widest + 2)?;
        for i in 1..term.len() {
            if term[i - 1] == term[i] {
                continue;
            }
            let mut swapped = term.clone();
            swapped.swap(i - 1, i);
            let spelling: String = swapped.into_iter().collect();
            blocks = either(&blocks, &tri.candidates(spelling.as_bytes())?);
        }
        Some(blocks)
    }
}

/// Does a word of the name begin at its `j`th character: the first, one after
/// anything not a letter or a digit, or where letters and digits meet
/// (`2026rapor`).
fn word_starts(name: &[char], j: usize) -> bool {
    let c = name[j];
    if !c.is_alphanumeric() {
        return false;
    }
    match j.checked_sub(1).map(|p| name[p]) {
        None => true,
        Some(p) => !p.is_alphanumeric() || p.is_numeric() != c.is_numeric(),
    }
}

/// Both sorted; every block of either, once.
fn either(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        let next = a[i].min(b[j]);
        i += usize::from(a[i] == next);
        j += usize::from(b[j] == next);
        out.push(next);
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

/// Letters in order with anything between them, one character at a time: a
/// byte of one character never completes another's.
#[derive(Debug)]
pub struct Letters {
    folded: Vec<u8>,
    /// Where each character of the term ends.
    ends: Vec<u32>,
}

impl Letters {
    pub fn new(folded: &str) -> Letters {
        Letters {
            folded: folded.as_bytes().to_vec(),
            ends: folded
                .char_indices()
                .map(|(at, c)| (at + c.len_utf8()) as u32)
                .collect(),
        }
    }

    /// Does this **already folded** name hold every letter, in order?
    pub fn found_in(&self, name: &[u8]) -> bool {
        let mut rest = name;
        let mut from = 0usize;
        for &end in &self.ends {
            let letter = &self.folded[from..end as usize];
            from = end as usize;
            let at = match letter {
                [one] => memchr::memchr(*one, rest),
                more => memchr::memmem::find(rest, more),
            };
            match at {
                Some(at) => rest = &rest[at + letter.len()..],
                None => return false,
            }
        }
        true
    }
}

#[cfg(test)]
mod tests;
