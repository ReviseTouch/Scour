//! A term forgiven its typing mistakes, and letters with anything between.
//!
//! [`Near`] asks whether any part of a name is within a few edits of a term —
//! a letter wrong, missing, extra, or two swapped — in one pass over the name's
//! bytes: the column of the edit-distance table is kept as bits (Myers; Hyyrö
//! for the swap), so a row costs its length and not its length times the term's.

/// The longest term the bit columns hold.
pub(crate) const WIDEST: usize = 64;

/// A term of at most [`WIDEST`] characters and how many mistakes it is forgiven.
#[derive(Debug)]
pub struct Near {
    /// Where each ASCII byte stands in the term, a bit a place.
    ascii: [u64; 128],
    /// The same for the term's other characters, by their UTF-8 bytes.
    wide: Vec<([u8; 4], u8, u64)>,
    /// The term's length in characters, and the bit of its last.
    len: u32,
    top: u64,
    edits: u32,
    folded: Vec<u8>,
}

impl Near {
    /// `None` for a term the columns cannot hold, or an empty one.
    pub fn new(folded: &str, edits: u8) -> Option<Near> {
        let len = folded.chars().count();
        if len == 0 || len > WIDEST {
            return None;
        }
        let mut near = Near {
            ascii: [0; 128],
            wide: Vec::new(),
            len: len as u32,
            top: 1u64 << (len - 1),
            edits: u32::from(edits),
            folded: folded.as_bytes().to_vec(),
        };
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

    /// The term, as the trigram index is keyed.
    pub(crate) fn folded(&self) -> &[u8] {
        &self.folded
    }

    pub(crate) fn edits(&self) -> u32 {
        self.edits
    }

    /// Is some part of this **already folded** name near enough to the term?
    pub fn found_in(&self, name: &[u8]) -> bool {
        // A column of the table, as the differences between its cells: `up`
        // where a cell is one more than the cell above, `down` where one less.
        let (mut up, mut down) = (!0u64, 0u64);
        // The diagonal of the last column and its matches, for the swap.
        let (mut diagonal, mut before) = (!0u64, 0u64);
        // The bottom cell: the fewest edits to end a match at this character.
        let mut score = self.len;
        let mut at = 0usize;
        while at < name.len() {
            let b = name[at];
            let matches = if b < 0x80 {
                at += 1;
                self.ascii[b as usize]
            } else {
                // One character, by its lead byte; a stray byte is its own.
                let n = match b {
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf7 => 4,
                    _ => 1,
                }
                .min(name.len() - at);
                let bytes = &name[at..at + n];
                at += n;
                self.wide
                    .iter()
                    .find(|w| &w.0[..w.1 as usize] == bytes)
                    .map_or(0, |w| w.2)
            };
            let swapped = ((!diagonal & matches) << 1) & before;
            diagonal = (((matches & up).wrapping_add(up)) ^ up) | matches | down | swapped;
            let right = down | !(diagonal | up);
            let left = up & diagonal;
            if right & self.top != 0 {
                score += 1;
            } else if left & self.top != 0 {
                score -= 1;
            }
            if score <= self.edits {
                return true;
            }
            // Nothing shifted in: a match may begin at any character.
            let (right, left) = (right << 1, left << 1);
            up = left | !(diagonal | right);
            down = right & diagonal;
            before = matches;
        }
        // An empty name, or a term every letter of which may be missing.
        self.len <= self.edits
    }

    /// The most trigrams of the term one mistake can spoil: a swap of two
    /// neighbours takes every window that touches either.
    pub(crate) fn spoiled_by_one(&self) -> usize {
        let lens: Vec<usize> = std::str::from_utf8(&self.folded)
            .unwrap_or_default()
            .chars()
            .map(char::len_utf8)
            .collect();
        let pair = lens.windows(2).map(|w| w[0] + w[1]).max();
        pair.or(lens.first().copied()).unwrap_or(1) + 2
    }
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
mod tests {
    use super::*;

    /// The table itself, a cell at a time: the fewest edits between the term
    /// and any part of the name, a swap of neighbours counting as one.
    fn slowly(term: &str, name: &str) -> usize {
        let t: Vec<char> = term.chars().collect();
        let n: Vec<char> = name.chars().collect();
        // rows are the term, columns the name; the top row is free.
        let mut d = vec![vec![0usize; n.len() + 1]; t.len() + 1];
        for (i, row) in d.iter_mut().enumerate() {
            row[0] = i;
        }
        for i in 1..=t.len() {
            for j in 1..=n.len() {
                let cost = usize::from(t[i - 1] != n[j - 1]);
                let mut best = (d[i - 1][j] + 1)
                    .min(d[i][j - 1] + 1)
                    .min(d[i - 1][j - 1] + cost);
                if i > 1 && j > 1 && t[i - 1] == n[j - 2] && t[i - 2] == n[j - 1] {
                    best = best.min(d[i - 2][j - 2] + 1);
                }
                d[i][j] = best;
            }
        }
        d[t.len()].iter().copied().min().unwrap_or(t.len())
    }

    #[test]
    fn a_wrong_a_missing_an_extra_and_a_swapped_letter_are_one_mistake_each() {
        let near = |term: &str, edits| Near::new(term, edits).expect("fits");
        for name in [
            "rapor.pdf",
            "2026-rapot.pdf",
            "rpor",
            "rappor",
            "raopr",
            "eski_raporu",
        ] {
            assert!(near("rapor", 1).found_in(name.as_bytes()), "{name}");
        }
        for name in ["rpr.pdf", "roapr2", "tabor", ""] {
            assert!(!near("rapor", 1).found_in(name.as_bytes()), "{name}");
        }
        assert!(near("muhasebe", 2).found_in(b"muhsaebe-2026.xlsx"));
        assert!(!near("muhasebe", 1).found_in(b"muhsaebi-2026.xlsx"));
        assert!(near("rapor", 0).found_in(b"rapor"));
        assert!(!near("rapor", 0).found_in(b"rapo"));
        assert!(Near::new("", 1).is_none());
        assert!(Near::new(&"a".repeat(65), 2).is_none());
        assert!(near(&"a".repeat(64), 2).found_in("a".repeat(62).as_bytes()));
    }

    #[test]
    fn the_bit_columns_agree_with_the_table_on_every_mixture() {
        let alphabet = ["a", "b", "c", "r", "ş", "д", "中", "🙂", ".", "o"];
        let mut state: u64 = 0x243f_6a88_85a3_08d3;
        let mut next = |n: usize| -> usize {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            ((state >> 33) as usize) % n
        };
        let word = |next: &mut dyn FnMut(usize) -> usize, most: usize| -> String {
            (0..next(most) + 1)
                .map(|_| alphabet[next(alphabet.len())])
                .collect()
        };
        let mut found = [0usize; 2];
        for _ in 0..40_000 {
            let term = word(&mut next, 9);
            let name = word(&mut next, 24);
            let want = slowly(&term, &name);
            for edits in 0..=3u8 {
                let got = Near::new(&term, edits)
                    .expect("fits")
                    .found_in(name.as_bytes());
                assert_eq!(
                    got,
                    want <= usize::from(edits),
                    "{term:?} in {name:?}, {edits}"
                );
                found[usize::from(got)] += 1;
            }
        }
        assert!(found[0] > 10_000 && found[1] > 10_000, "{found:?}");
    }

    #[test]
    fn bytes_that_are_not_text_are_read_without_a_panic() {
        let near = Near::new("rapor", 1).expect("fits");
        assert!(near.found_in(b"\xff\xfe rapo\xc3"));
        assert!(!near.found_in(b"\xe4\xb8"));
        assert!(!Letters::new("ab").found_in(b"\xff\xfea"));
    }

    #[test]
    fn letters_are_found_in_order_with_anything_between() {
        for (term, name, want) in [
            ("scrgui", "scour-gui", true),
            ("scrgui", "scour-tui", false),
            ("sg", "gs", false),
            ("aa", "a", false),
            ("aa", "aba", true),
            ("", "anything", true),
            ("şk", "şirket", true),
            // The second byte of `ş` is not a letter of its own.
            ("\u{15f}", "\u{11f}", false),
            ("中文", "中x文", true),
        ] {
            assert_eq!(
                Letters::new(term).found_in(name.as_bytes()),
                want,
                "{term} {name}"
            );
        }
    }
}
