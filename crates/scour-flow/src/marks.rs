//! Where the words a search found stand in a document's text: as a search reads
//! by default, not minding a letter's case, Turkish's dotted and dotless i, or
//! a mark on a letter (`şirket` is `sirket`).

/// The words to mark, each folded once.
#[derive(Debug, Clone, Default)]
pub struct Marks {
    terms: Vec<Vec<char>>,
}

/// Words marked at most, and a word's length: what a face sends is a search's
/// finds, not a document.
pub const MOST_TERMS: usize = 20;
pub const MOST_TERM: usize = 100;

impl Marks {
    /// From the finds' own words; blanks and repeats dropped.
    pub fn new<S: AsRef<str>>(words: &[S]) -> Marks {
        let mut terms: Vec<Vec<char>> = Vec::new();
        for w in words.iter().take(MOST_TERMS) {
            let folded: Vec<char> = w
                .as_ref()
                .trim()
                .chars()
                .take(MOST_TERM)
                .filter_map(fold)
                .collect();
            if !folded.is_empty() && !terms.contains(&folded) {
                terms.push(folded);
            }
        }
        // The longest first, so a word inside a longer one is not marked twice.
        terms.sort_by_key(|t| std::cmp::Reverse(t.len()));
        Marks { terms }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Byte ranges of `text` that are one of the words, in order, apart.
    pub fn find(&self, text: &str) -> Vec<(usize, usize)> {
        if self.terms.is_empty() || text.is_empty() {
            return Vec::new();
        }
        // Each folded letter and the bytes it came from.
        let mut folded: Vec<(char, usize, usize)> = Vec::with_capacity(text.len());
        for (at, c) in text.char_indices() {
            if let Some(f) = fold(c) {
                folded.push((f, at, at + c.len_utf8()));
            } else if let Some(last) = folded.last_mut() {
                // A combining mark belongs to the letter before it.
                last.2 = at + c.len_utf8();
            }
        }
        let mut out: Vec<(usize, usize)> = Vec::new();
        for term in &self.terms {
            if term.len() > folded.len() {
                continue;
            }
            let mut i = 0;
            while i + term.len() <= folded.len() {
                if folded[i..i + term.len()]
                    .iter()
                    .map(|f| f.0)
                    .eq(term.iter().copied())
                {
                    let range = (folded[i].1, folded[i + term.len() - 1].2);
                    if !out.iter().any(|r| r.0 < range.1 && range.0 < r.1) {
                        out.push(range);
                    }
                    i += term.len();
                } else {
                    i += 1;
                }
            }
        }
        out.sort_unstable();
        out
    }
}

/// A letter as a search compares it; None for a mark that only sits on one.
fn fold(c: char) -> Option<char> {
    if ('\u{300}'..='\u{36f}').contains(&c) {
        return None;
    }
    Some(match c {
        'İ' | 'I' | 'ı' | 'i' | 'î' | 'Î' | 'ï' | 'í' | 'ì' => 'i',
        'ş' | 'Ş' => 's',
        'ç' | 'Ç' => 'c',
        'ğ' | 'Ğ' => 'g',
        'ö' | 'Ö' | 'ô' | 'ó' | 'ò' => 'o',
        'ü' | 'Ü' | 'û' | 'ú' | 'ù' => 'u',
        'â' | 'Â' | 'á' | 'à' | 'ä' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        c => c.to_lowercase().next().unwrap_or(c),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(words: &[&str], text: &str) -> Vec<String> {
        Marks::new(words)
            .find(text)
            .into_iter()
            .map(|(a, b)| text[a..b].to_owned())
            .collect()
    }

    #[test]
    fn a_word_is_found_whatever_its_case_its_i_or_its_marks() {
        assert_eq!(
            found(&["hizmet"], "HİZMET ALAN ve hizmet veren"),
            ["HİZMET", "hizmet"]
        );
        assert_eq!(found(&["SIRKET"], "Deniz Yapı Anonim Şirketi"), ["Şirket"]);
        assert_eq!(found(&["ıslak"], "Islak imza"), ["Islak"]);
        assert_eq!(found(&["kira"], "Kira sözleşmesi kiracı"), ["Kira", "kira"]);
    }

    #[test]
    fn the_longer_word_wins_and_nothing_is_marked_twice() {
        assert_eq!(
            found(&["söz", "sözleşme"], "sözleşmesi söz"),
            ["sözleşme", "söz"]
        );
        assert!(found(&["", "  "], "anything").is_empty());
    }

    #[test]
    fn a_combining_mark_stays_with_its_letter() {
        // "s" and a combining cedilla: one letter to a reader.
        let text = "s\u{327}irket";
        assert_eq!(found(&["sirket"], text), [text]);
    }
}
