//! A word with letters wrong, missing, extra or swapped, as one expression.
//! Spelled right it is found anywhere; misspelt, only where a word begins, as
//! the name query's `fuzzy:` has it.

use crate::pattern::{Encoding, atom};

/// How many mistakes a word is forgiven: the caller's rule, so that a file's
/// contents and its name forgive alike.
pub type Forgive = fn(&str) -> u8;

/// `edits + 1` parts of `word`, a character apart so no mistake spoils two:
/// a find holds one of them as it is. None when a part would be empty.
pub(crate) fn pieces(word: &str, edits: u8) -> Option<Vec<String>> {
    let chars: Vec<char> = word.chars().collect();
    let parts = usize::from(edits) + 1;
    let held = chars.len().checked_sub(parts - 1)?;
    if held < parts {
        return None;
    }
    let mut out = Vec::with_capacity(parts);
    let mut at = 0;
    for i in 0..parts {
        let len = held / parts + usize::from(i < held % parts);
        out.push(chars[at..at + len].iter().collect());
        at += len + 1;
    }
    Some(out)
}

/// One word in one encoding, and the mistakes it is forgiven.
pub(crate) struct Near {
    /// Each character of the word, every way its case may be written.
    atoms: Vec<String>,
    edits: u8,
    /// One character a word begins after.
    lead: String,
    /// One character a mistake may put in the word: a letter or a digit.
    any: &'static str,
}

impl Near {
    /// None when the encoding cannot write the word.
    pub(crate) fn new(word: &str, edits: u8, encoding: Encoding, exact_case: bool) -> Option<Near> {
        let atoms = word
            .chars()
            .map(|c| atom(c, encoding, exact_case))
            .collect::<Option<Vec<String>>>()?;
        let first = word.chars().next()?;
        let mut ascii = String::from(r"\x00-\x2F\x3A-\x40\x5B-\x60\x7B-\x7F");
        if first.is_alphabetic() {
            ascii.push_str("0-9");
        } else if first.is_numeric() {
            ascii.push_str("A-Za-z");
        }
        let (lead, any) = match encoding {
            Encoding::Utf8 => (
                format!(
                    r"(?:[{ascii}]|\xC2[\xA0\xAB\xBB]|\xE2\x80[\x80-\xBF]|\xE2\x81[\x80-\xAF])"
                ),
                // `C2` and `E2` lead the spaces and punctuation, and no letter.
                r"(?:[0-9A-Za-z]|[\xC3-\xE1\xE3-\xF4][\x80-\xBF]{1,3})",
            ),
            // Only ASCII: a byte past it may be the tail of a UTF-8 letter.
            Encoding::Cp1254 => (format!("[{ascii}]"), r"[0-9A-Za-z\x80-\xFF]"),
            Encoding::Utf16Le => (
                format!(r"(?:[{ascii}]\x00|[\xA0\xAB\xBB]\x00|[\x00-\x6F]\x20)"),
                r"(?:[0-9A-Za-z\xC0-\xFF]\x00|[\x00-\xFF][\x01-\x1F\x21-\xFF])",
            ),
            Encoding::Utf16Be => (
                format!(r"(?:\x00[{ascii}]|\x00[\xA0\xAB\xBB]|\x20[\x00-\x6F])"),
                r"(?:\x00[0-9A-Za-z\xC0-\xFF]|[\x01-\x1F\x21-\xFF][\x00-\xFF])",
            ),
        };
        Some(Near {
            atoms,
            edits,
            lead,
            any,
        })
    }

    /// The word anywhere, or a misspelling with the character before it.
    pub(crate) fn find(&self) -> String {
        format!(
            r"{}|(?:\A|{})(?:{})",
            self.atoms.concat(),
            self.lead,
            self.body()
        )
    }

    /// The word or a misspelling of it, and nothing before it.
    pub(crate) fn body(&self) -> String {
        let mut out = String::new();
        self.write(0, self.atoms.len(), self.edits, true, &mut out);
        out
    }

    /// Characters `from..to` within `edits` mistakes, nothing extra after the
    /// last (the next part's) nor, when `first`, before the first.
    fn write(&self, from: usize, to: usize, edits: u8, first: bool, out: &mut String) {
        let len = to - from;
        if edits == 0 || len == 0 {
            out.extend(self.atoms[from..to].iter().map(String::as_str));
        } else if len <= 3 || (edits == 1 && len <= 5) {
            self.write_each(from, to, edits, first, out);
        } else {
            self.write_halves(from, to, edits, first, out);
        }
    }

    /// One mistake at each place in turn, the right spelling tried first and
    /// a missing letter last, so a find is as long as it can be.
    fn write_each(&self, at: usize, to: usize, edits: u8, first: bool, out: &mut String) {
        let atoms = &self.atoms;
        if at == to || edits == 0 {
            return self.write(at, to, 0, first, out);
        }
        out.push_str("(?:");
        out.push_str(&atoms[at]);
        self.write_each(at + 1, to, edits, false, out);
        out.push('|');
        out.push_str(self.any);
        self.write_each(at + 1, to, edits - 1, false, out);
        if at + 1 < to && atoms[at] != atoms[at + 1] {
            out.push('|');
            out.push_str(&atoms[at + 1]);
            out.push_str(&atoms[at]);
            self.write_each(at + 2, to, edits - 1, false, out);
        }
        if !first {
            out.push('|');
            out.push_str(self.any);
            self.write_each(at, to, edits - 1, false, out);
        }
        out.push('|');
        self.write_each(at + 1, to, edits - 1, first, out);
        out.push(')');
    }

    /// The mistakes shared between the halves every way, or one the swap across
    /// them: written whole, sixteen letters forgiven two overflow the engine.
    fn write_halves(&self, from: usize, to: usize, edits: u8, first: bool, out: &mut String) {
        let mid = from + (to - from).div_ceil(2);
        out.push_str("(?:");
        for left in 0..=edits {
            if left > 0 {
                out.push('|');
            }
            self.write(from, mid, left, first, out);
            self.write(mid, to, edits - left, false, out);
        }
        if self.atoms[mid - 1] != self.atoms[mid] {
            for left in 0..edits {
                out.push('|');
                self.write(from, mid - 1, left, first, out);
                out.push_str(&self.atoms[mid]);
                out.push_str(&self.atoms[mid - 1]);
                self.write(mid + 1, to, edits - 1 - left, false, out);
            }
        }
        out.push(')');
    }
}
