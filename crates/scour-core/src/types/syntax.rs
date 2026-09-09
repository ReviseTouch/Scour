//! Query text, cut into coloured pieces by the parser itself, so that a search
//! box never reimplements the lexer: a frontend maps a [`Role`] to a colour.
//!
//! The parser never fails — `kind:zurna` becomes a search for the text
//! "kind:zurna" — which [`Role::UnknownField`] and [`Role::BadValue`] mark.

use serde::{Deserialize, Serialize};

/// What a run of query text is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Ordinary text, matched against the name.
    Text,
    /// Text containing `*` or `?`, matched end to end.
    Glob,
    /// A quoted phrase, wildcards and all taken literally.
    Phrase,
    /// The quote characters themselves.
    Quote,
    /// A field name the parser recognises: `ext`, `under`, `dm`.
    Field,
    /// Two or more letters and a colon, but not a field. **This term is being
    /// searched for as literal text** — `sizE:>1mb` looks for that string.
    UnknownField,
    /// The `:` between a field and its value.
    Colon,
    /// A value the field can use.
    Value,
    /// A real field with a value it cannot read, so the whole term falls back
    /// to text: `kind:zurna`, `size:abc`, `dm:soon`.
    BadValue,
    /// A comparison in front of a value: `>`, `<=`, `=`.
    Cmp,
    /// `!` — the rest of this term must not match.
    Not,
    /// `|` — either side may match.
    Or,
    /// The separator inside a multi-valued field: `ext:rs;toml`.
    Sep,
    /// What separates two terms: whitespace, or a `;` written instead of it. Between
    /// terms `;` means what a space means, which is *both*; inside a field's value it
    /// goes on meaning "any of these" — [`Role::Sep`].
    Space,
}

impl Role {
    /// Every role, so that a frontend's tests can walk them. A colour table missing a
    /// row fails silently: the run is drawn in whatever the layer's own colour is.
    pub const ALL: [Role; 14] = [
        Role::Text,
        Role::Glob,
        Role::Phrase,
        Role::Quote,
        Role::Field,
        Role::UnknownField,
        Role::Colon,
        Role::Value,
        Role::BadValue,
        Role::Cmp,
        Role::Not,
        Role::Or,
        Role::Sep,
        Role::Space,
    ];

    /// Is this run something the engine could not use as written? Both warning roles
    /// mean "this term is not doing what its spelling suggests".
    pub fn is_warning(self) -> bool {
        matches!(self, Role::UnknownField | Role::BadValue)
    }

    /// Does this run mean the query is worth reading back in words? True of anything
    /// more than a plain word: `HASAN;DENEME !ama ;deneme` looks like four AND-ed words
    /// and is three. [`Role::Value`] and [`Role::Sep`] never occur without a field.
    pub fn is_telling(self) -> bool {
        matches!(
            self,
            Role::Field
                | Role::Colon
                | Role::Cmp
                | Role::Quote
                | Role::Phrase
                | Role::Glob
                | Role::Not
                | Role::Or
                | Role::UnknownField
                | Role::BadValue
        )
    }

    /// Is this run punctuation rather than something the user typed to search for?
    /// Deleting one changes the query's meaning rather than its wording: without its
    /// `:`, `ext:pdf` is a search for the text "extpdf".
    pub fn is_syntax(self) -> bool {
        matches!(
            self,
            Role::Colon | Role::Sep | Role::Quote | Role::Cmp | Role::Not | Role::Or
        )
    }
}

/// A run of query text with one role. Offsets are **byte** offsets into the query
/// as sent; spans are in order, never overlap, and concatenate back to the original.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub len: u32,
    pub role: Role,
    /// Is this run part of a term that must **not** match? [`Role`] does not say it:
    /// `pdf` is a `Value` in `!ext:pdf` as in `ext:pdf`, and [`Role::Not`] covers the
    /// `!` alone, which may carry across a `|` group and across `!ext:rs; toml`.
    #[serde(default)]
    pub not: bool,
}

impl Span {
    pub fn new(start: usize, len: usize, role: Role) -> Self {
        Span {
            start: start as u32,
            len: len as u32,
            role,
            not: false,
        }
    }

    /// The same run, marked as part of an excluded term.
    pub fn excluded(mut self) -> Self {
        self.not = true;
        self
    }

    /// The text this span covers, given the query it came from.
    pub fn of<'a>(&self, query: &'a str) -> &'a str {
        let start = self.start as usize;
        let end = start + self.len as usize;
        query.get(start..end).unwrap_or("")
    }

    /// The whole term this span sits in, given the query it came from. For reporting
    /// a fault: [`Role::BadValue`] covers `>abc` alone, which does not name the field
    /// that refused it. A term is a run of non-space, as the tokeniser means it.
    pub fn term_of<'a>(&self, query: &'a str) -> &'a str {
        let start = (self.start as usize).min(query.len());
        let end = (start + self.len as usize).min(query.len());
        // Only ASCII space is cut on, so both edges stay on a char boundary.
        let from = query[..start].rfind(' ').map_or(0, |i| i + 1);
        let to = query[end..].find(' ').map_or(query.len(), |i| end + i);
        query.get(from..to).unwrap_or("")
    }
}

/// Something the user could type next, from the same table the parser reads, so a
/// field that does not exist cannot be offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    /// What to insert, complete: `ext:`, `kind:image`, `dm:7d`.
    pub insert: String,
    /// What to show. Usually the same as `insert`.
    pub label: String,
    /// One line about it, in English, as a message id like every other string
    /// the engine produces.
    pub about: String,
    /// What kind of thing is being offered.
    pub kind: CompletionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionKind {
    /// A field name: `ext:`, `size:`.
    Field,
    /// A value for the field already typed: `image` after `kind:`.
    Value,
    /// An operator or piece of punctuation: `!`, `|`, `"`.
    Operator,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`Role::ALL`] is all of them: the match is exhaustive, so a new role stops this
    /// compiling next to the list it also has to be added to.
    #[test]
    fn every_role_is_in_the_list() {
        fn place(role: Role) -> usize {
            match role {
                Role::Text => 0,
                Role::Glob => 1,
                Role::Phrase => 2,
                Role::Quote => 3,
                Role::Field => 4,
                Role::UnknownField => 5,
                Role::Colon => 6,
                Role::Value => 7,
                Role::BadValue => 8,
                Role::Cmp => 9,
                Role::Not => 10,
                Role::Or => 11,
                Role::Sep => 12,
                Role::Space => 13,
            }
        }
        for (i, role) in Role::ALL.into_iter().enumerate() {
            assert_eq!(place(role), i, "{role:?} is in the wrong place in ALL");
        }
    }

    fn span(query: &str, needle: &str) -> Span {
        let at = query.find(needle).expect("needle not in query");
        Span::new(at, needle.len(), Role::BadValue)
    }

    #[test]
    fn a_value_is_reported_with_the_field_that_refused_it() {
        let q = "ext:rs size:>abc dm:7d";
        assert_eq!(span(q, ">abc").term_of(q), "size:>abc");
    }

    #[test]
    fn a_term_at_either_end_keeps_its_edge() {
        assert_eq!(span("size:>abc", ">abc").term_of("size:>abc"), "size:>abc");
        assert_eq!(span(">abc ext:rs", ">abc").term_of(">abc ext:rs"), ">abc");
    }

    /// Cutting on spaces must not cut inside a character: every offset here is a byte
    /// offset, and multi-byte text panics rather than answering wrong.
    #[test]
    fn a_term_beside_letters_that_are_not_one_byte_is_still_a_term() {
        let q = "değiştirme kind:zurna öğe";
        assert_eq!(span(q, "zurna").term_of(q), "kind:zurna");
        let q = "kind:çğüşöı";
        assert_eq!(span(q, "çğüşöı").term_of(q), "kind:çğüşöı");
    }

    #[test]
    fn a_span_the_query_does_not_reach_answers_rather_than_panics() {
        let s = Span::new(40, 9, Role::BadValue);
        assert_eq!(s.of("short"), "");
        assert_eq!(s.term_of("short"), "short");
    }
}
