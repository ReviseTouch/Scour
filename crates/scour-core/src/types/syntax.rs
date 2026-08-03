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
    /// Whitespace between terms.
    Space,
}

impl Role {
    /// Is this run something the engine could not use as written?
    ///
    /// The two warning roles are worth naming together, because a frontend
    /// almost always wants to treat them alike — both mean "this term is not
    /// doing what its spelling suggests".
    pub fn is_warning(self) -> bool {
        matches!(self, Role::UnknownField | Role::BadValue)
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
}

impl Span {
    pub fn new(start: usize, len: usize, role: Role) -> Self {
        Span {
            start: start as u32,
            len: len as u32,
            role,
        }
    }

    /// The text this span covers, given the query it came from.
    pub fn of<'a>(&self, query: &'a str) -> &'a str {
        let start = self.start as usize;
        let end = start + self.len as usize;
        query.get(start..end).unwrap_or("")
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
