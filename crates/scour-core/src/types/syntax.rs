//! Query text, cut into coloured pieces.
//!
//! A search box that colours what is being typed has to agree with the parser
//! about what the words mean, and the only way to guarantee that is for the
//! parser to say so. So this is not a lexer for a frontend to reimplement: it
//! is the shape the engine hands back after reading a query, and a frontend's
//! whole job is to map a [`Role`] to a colour.
//!
//! The roles that matter most are the two that say *this is not what you think
//! it is*. Scour's parser never fails — an unreadable term becomes a search for
//! its own text — which is what keeps a half-typed query usable and is also the
//! one way it can silently answer the wrong question. `kind:zurna` finds files
//! called "kind:zurna". [`Role::UnknownField`] and [`Role::BadValue`] exist so
//! that a user can see that happening instead of discovering it in the results.

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
    /// What separates two terms: whitespace, or a `;` written instead of it.
    ///
    /// **`;` is a space somebody typed without pressing space.** It has been
    /// a literal character and it has been `|`, and each rule was replaced
    /// because a query came back wrong: as a character `OPUS ; SONNET` found
    /// neither word, and as `|` `hasan;genel` found every file called
    /// `hasan`. Between terms it now means what a space means, which is
    /// *both*. Inside a field's value it goes on meaning "any of these" —
    /// [`Role::Sep`] — and that is not one mark used two ways: a list of
    /// extensions can only ever be an "any", and two terms are a different
    /// thing from two values.
    Space,
}

impl Role {
    /// Every role, so that a frontend's tests can walk them.
    ///
    /// A colour scheme is a table with one row per role, and the way it goes
    /// wrong is a role that has no row: nothing fails, the run is drawn in
    /// whatever the layer's own colour is, and the day it matters is the day
    /// somebody types `kind:zurna`. See the test below for what keeps this
    /// list complete.
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

    /// Is this run something the engine could not use as written?
    ///
    /// The two warning roles are worth naming together, because a frontend
    /// almost always wants to treat them alike — both mean "this term is not
    /// doing what its spelling suggests".
    pub fn is_warning(self) -> bool {
        matches!(self, Role::UnknownField | Role::BadValue)
    }

    /// Does this run mean the query is worth reading back?
    ///
    /// A search box can say what it understood, in words — "extension is .rs
    /// and not name contains cache" — and for `ext:rs !cache` that is worth a
    /// line. For `hasan` it reads the word back and is furniture.
    ///
    /// So the rule is a property of the query rather than a preference: if
    /// anything in it is more than a plain word, the reading appears, and its
    /// being there is then a signal in itself. The case it exists for is
    /// `HASAN;DENEME !ama ;deneme` — four things that look like four AND-ed
    /// words and are three, one of them an `or` with the exclusion inside it,
    /// which is why the answer was the whole disk.
    ///
    /// A [`Role::Value`] is not in the list and does not need to be: it never
    /// occurs without the field and colon in front of it, both of which are.
    /// Nor is [`Role::Sep`], for the same reason.
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

    /// Is this run punctuation rather than something the user typed to search
    /// for?
    ///
    /// What a caret sitting next to one of these will delete is a piece of
    /// *syntax*, and the meaning of the query changes rather than its wording:
    /// backspacing over the `:` in `ext:pdf` turns a filter into a search for
    /// the text "extpdf". A frontend can mark the character the caret is
    /// touching so that is visible before the key is pressed rather than after.
    pub fn is_syntax(self) -> bool {
        matches!(
            self,
            Role::Colon | Role::Sep | Role::Quote | Role::Cmp | Role::Not | Role::Or
        )
    }
}

/// A run of query text with one role.
///
/// Offsets are **byte** offsets into the query as it was sent, so a frontend
/// slicing UTF-8 gets valid boundaries. Spans are in order, never overlap, and
/// together cover the whole string — a frontend can concatenate them and expect
/// the original back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub len: u32,
    pub role: Role,
    /// Is this run part of a term that must **not** match?
    ///
    /// A query line answers two questions — what is being looked for, and what
    /// is being kept out — and [`Role`] answers neither: `pdf` in `!ext:pdf`
    /// is a `Value` exactly as it is in `ext:pdf`. [`Role::Not`] covers the
    /// `!` alone, one character wide, so a frontend colouring roles put the
    /// whole excluded term in the colour of the thing being *sought*.
    ///
    /// Working the extent out from the spans is a frontend's second parser —
    /// the thing the rest of this file exists to prevent — and it is not a
    /// one-liner either: `!` may prefix a bare word, a field term, or one
    /// alternative of a `|` group, and a list carries it across the spaces
    /// inside `!ext:rs; toml`. So the parser says it, once.
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

    /// The whole term this span sits in, given the query it came from.
    ///
    /// For colouring, a span is exactly the right extent. For *telling someone
    /// what went wrong*, it is not: [`Role::BadValue`] covers the value alone,
    /// so `size:>abc` reports `>abc` — and a value with no field in front of it
    /// does not say who refused it, which is the only thing the reader needs.
    ///
    /// A term is a run of non-space, which is what the tokeniser means by one.
    /// The two warning roles cannot occur inside a quoted phrase — a phrase is
    /// literal all the way through — so there is no quoted case to widen past.
    pub fn term_of<'a>(&self, query: &'a str) -> &'a str {
        let start = (self.start as usize).min(query.len());
        let end = (start + self.len as usize).min(query.len());
        // Only ASCII space is cut on, so both edges stay on a char boundary
        // wherever the term itself is.
        let from = query[..start].rfind(' ').map_or(0, |i| i + 1);
        let to = query[end..].find(' ').map_or(query.len(), |i| end + i);
        query.get(from..to).unwrap_or("")
    }
}

/// Something the user could type next.
///
/// Completions come from the same table the parser reads, so a field that
/// exists is offered and a field that does not cannot be.
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

    /// [`Role::ALL`] is all of them.
    ///
    /// The match is exhaustive, so a role added to the enum stops this
    /// compiling — and the arm that has to be written is right next to the
    /// list that also has to be added to.
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

    /// Cutting on spaces must not cut inside a character.
    ///
    /// Byte offsets and multi-byte text is the pairing that produces a panic
    /// rather than a wrong answer, and every path in this file is byte offsets.
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
