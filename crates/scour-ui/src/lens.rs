//! The lens: what the term under the caret can be changed into, offered as a
//! short list under it. Text in, text out — the query stays the one thing a
//! window holds, and a choice is a replacement of one term. Counts beside the
//! choices are the window's to ask: each is the query with that term swapped.
//!
//! Which lens a term opens is a decision about the window, not a reading of
//! the query: the service still says what every term means.

mod capsule;
mod folders;
mod group;
mod launch;
#[cfg(test)]
mod machine;
mod rest;
mod size;
mod split;
mod spots;
mod time;
mod time_pick;
mod time_said;
mod values;
pub use capsule::*;
pub use folders::*;
pub use group::*;
pub use launch::*;
pub use rest::*;
pub use size::*;
pub use split::*;
pub use spots::*;
pub use time::*;
pub use time_pick::*;
pub use time_said::*;
pub use values::*;

/// A term of the query, by byte offsets into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub start: usize,
    pub end: usize,
    /// Written with `!`.
    pub not: bool,
    /// The field's canonical name, when the term names one this module knows.
    pub field: Option<&'static str>,
    /// What follows the colon, or the whole term less `!`, without quotes.
    pub value: String,
}

/// Whether a query reads files: a `content:` term with something in it. An
/// empty one is a filter not written yet, which the search leaves out.
pub fn reads(query: &str) -> bool {
    terms(query)
        .iter()
        .any(|t| t.field == Some("content") && !t.value.trim().is_empty())
}

/// What a lens lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lens {
    /// `:` typed on its own: the filters that can be started.
    Launch,
    /// `under:` and `parent:` — folders, and the disks at the top.
    Place,
    Time,
    Size,
    /// `kind:` and `ext:`, several at once.
    Kind,
    Ext,
    /// A plain word: how it is read.
    Word,
    /// `content:`.
    Content,
}

/// The spellings the lens answers to. The service's table is longer; a field
/// missing here opens no lens and is otherwise untouched.
const SPELLINGS: &[(&str, &[&str])] = &[
    ("under", &["under", "in", "altinda"]),
    ("parent", &["parent", "child", "children"]),
    ("dm", &["dm", "modified"]),
    ("size", &["size"]),
    ("kind", &["kind", "type", "tur", "tür"]),
    ("ext", &["ext"]),
    ("content", &["content", "text", "icerik", "içerik"]),
    ("fuzzy", &["fuzzy", "typo", "benzer"]),
    ("letters", &["letters", "harfler"]),
];

/// Folded as the service folds a field's name: case aside, and the dotted
/// and dotless i one letter, so `sizE:` and `ALTINDA:` are fields.
fn canonical(name: &str) -> Option<&'static str> {
    let folded: String = name
        .chars()
        .map(|c| {
            if matches!(c, 'İ' | 'I' | 'ı') {
                'i'
            } else {
                c
            }
        })
        .collect::<String>()
        .to_lowercase();
    SPELLINGS
        .iter()
        .find(|(_, all)| all.contains(&folded.as_str()))
        .map(|(c, _)| *c)
}

fn term(text: &str, start: usize, end: usize) -> Term {
    let raw = &text[start..end];
    let not = raw.starts_with('!');
    let bare = raw.strip_prefix('!').unwrap_or(raw);
    let split = (!bare.starts_with('"'))
        .then(|| bare.split_once(':'))
        .flatten()
        .filter(|(f, _)| f.chars().count() >= 2 && f.chars().all(char::is_alphabetic));
    let (field, value) = match split {
        Some((f, v)) => (canonical(f), v),
        None => (None, bare),
    };
    let value = value.trim_matches('"').to_string();
    Term {
        start,
        end,
        not,
        field,
        value,
    }
}

/// The term the caret is in or just after. A caret before a term's first
/// character belongs to the space in front of it.
pub fn term_at(text: &str, caret: usize) -> Option<Term> {
    terms(text)
        .into_iter()
        .find(|t| caret > t.start && caret <= t.end)
}

/// The field a name one letter off is likely to mean: `siz` is `size`,
/// `udner` is `under`. Only the fields a lens opens for.
pub fn near_field(name: &str) -> Option<&'static str> {
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let one = |w: &str| {
        let w: Vec<char> = w.chars().collect();
        if n == w || n.len().abs_diff(w.len()) > 1 {
            return false;
        }
        let mut i = 0;
        while i < n.len() && i < w.len() && n[i] == w[i] {
            i += 1;
        }
        if n.len() == w.len() {
            n[i + 1..] == w[i + 1..]
                || (i + 1 < n.len()
                    && n[i] == w[i + 1]
                    && n[i + 1] == w[i]
                    && n[i + 2..] == w[i + 2..])
        } else if n.len() > w.len() {
            n[i + 1..] == w[i..]
        } else {
            n[i..] == w[i + 1..]
        }
    };
    SPELLINGS
        .iter()
        .find(|(_, all)| all.iter().any(|w| one(w)))
        .map(|(c, _)| *c)
}

/// Which lens a term opens, if any. A field this module does not know, or a
/// term the launcher has not finished, opens none.
pub fn lens_of(text: &str, t: &Term) -> Option<Lens> {
    let raw = &text[t.start..t.end];
    if raw.starts_with(':') && !raw[1..].contains(':') {
        return Some(Lens::Launch);
    }
    match t.field {
        Some("under" | "parent") => Some(Lens::Place),
        Some("dm") => Some(Lens::Time),
        Some("size") => Some(Lens::Size),
        Some("kind") => Some(Lens::Kind),
        Some("ext") => Some(Lens::Ext),
        Some("content") => Some(Lens::Content),
        Some("fuzzy" | "letters") => Some(Lens::Word),
        Some(_) => None,
        None if raw.contains(':') => None,
        None if t.value.chars().count() >= 2 => Some(Lens::Word),
        None => None,
    }
}

/// Whether a lens opens by itself when the caret reaches the end of such a
/// term: the fields whose values are hard to remember, never a plain word.
pub fn opens_itself(lens: Lens) -> bool {
    !matches!(lens, Lens::Word)
}

/// The query with one term replaced, and where the caret goes: after the new
/// term. An empty replacement takes the term and one space beside it.
pub fn replace(text: &str, t: &Term, with: &str) -> (String, usize) {
    if !with.is_empty() {
        let out = format!("{}{with}{}", &text[..t.start], &text[t.end..]);
        return (out, t.start + with.len());
    }
    let (mut a, mut b) = (t.start, t.end);
    if text[b..].starts_with(' ') {
        b += 1;
    } else if text[..a].ends_with(' ') {
        a -= 1;
    }
    (format!("{}{}", &text[..a], &text[b..]), a)
}

/// The query less one term: what a lens counts its choices against.
pub fn without(text: &str, t: &Term) -> String {
    replace(text, t, "").0.trim().to_string()
}

/// One line of a lens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The msgid that names it, or a value shown as it is.
    pub label: String,
    /// A second, quieter part: the term it writes, or a size.
    pub hint: String,
    /// The term it writes in place of the one under the caret.
    pub term: String,
    /// Writing it keeps the lens open: a list takes several values.
    pub stays: bool,
}

impl Choice {
    fn new(label: &str, hint: &str, term: String) -> Self {
        Choice {
            label: label.to_string(),
            hint: hint.to_string(),
            term,
            stays: false,
        }
    }
}

fn neg(t: &Term) -> &'static str {
    if t.not { "!" } else { "" }
}

/// The values `kind:` is offered with when nothing has been counted yet: the
/// service's `KIND_VALUES`, in its order.
pub const KINDS: &[&str] = &[
    "folder", "code", "doc", "image", "data", "config", "archive", "exec", "audio", "video",
    "font", "build", "file",
];

/// The bands a `dm:` lens offers, newest first.
pub const TIMES: &[(&str, &str)] = &[
    ("today", "today"),
    ("yesterday", "yesterday"),
    ("7d", "last 7 days"),
    ("30d", "last 30 days"),
    ("1y", "last year"),
];

/// The bands a `size:` lens offers, smallest first.
pub const SIZES: &[(&str, &str)] = &[
    ("<100kb", "under 100 KB"),
    ("100kb..10mb", "100 KB to 10 MB"),
    ("10mb..1gb", "10 MB to 1 GB"),
    (">1gb", "over 1 GB"),
];

/// The choices of a band lens: one term per band, the field as it was typed.
pub fn bands(t: &Term, bands: &[(&str, &str)]) -> Vec<Choice> {
    let field = t.field.unwrap_or("dm");
    bands
        .iter()
        .map(|(v, l)| Choice::new(l, v, format!("{}{field}:{v}", neg(t))))
        .collect()
}

/// The values of a list field, one already there taken out and one not there
/// put in. Each writes the whole term and keeps the lens open.
pub fn list(t: &Term, values: &[(String, String)]) -> Vec<Choice> {
    let field = t.field.unwrap_or("kind");
    let have: Vec<&str> = t.value.split(';').filter(|v| !v.is_empty()).collect();
    let known = |v: &&str| values.iter().any(|(k, _)| k == v);
    values
        .iter()
        .map(|(value, label)| {
            let on = have.contains(&value.as_str());
            let mut next: Vec<&str> = have.iter().copied().filter(known).collect();
            if on {
                next.retain(|v| v != value);
            } else {
                next.push(value);
            }
            Choice {
                label: label.clone(),
                hint: if on { "✓".into() } else { String::new() },
                term: format!("{}{field}:{}", neg(t), next.join(";")),
                stays: true,
            }
        })
        .collect()
}

fn quoted(v: &str) -> String {
    if v.chars().any(char::is_whitespace) {
        format!("\"{v}\"")
    } else {
        v.to_string()
    }
}

/// How a word may be read: as typed, a letter off, its letters in order, in
/// the files' contents, or excluded.
pub fn word(t: &Term) -> Vec<Choice> {
    let v = &t.value;
    let n = neg(t);
    let mut out = vec![Choice::new("as typed", "", format!("{n}{}", quoted(v)))];
    if !v.chars().any(char::is_whitespace) {
        out.push(Choice::new(
            "a letter off still matches",
            "fuzzy:",
            format!("{n}fuzzy:{v}"),
        ));
        out.push(Choice::new(
            "these letters in this order",
            "letters:",
            format!("{n}letters:{v}"),
        ));
    }
    out.push(Choice::new(
        "inside the files",
        "content:",
        format!("{n}content:\"{v}\""),
    ));
    let flipped = if t.not { "" } else { "!" };
    out.push(Choice::new(
        if t.not {
            "stop excluding it"
        } else {
            "exclude it"
        },
        "!",
        format!("{flipped}{}", quoted(v)),
    ));
    out
}

/// A `content:` term: read for the text, or look for it in names instead.
pub fn content(t: &Term) -> Vec<Choice> {
    let v = &t.value;
    vec![
        Choice::new(
            "inside the files",
            "content:",
            format!("{}content:\"{v}\"", neg(t)),
        ),
        Choice::new("in names instead", "", format!("{}{}", neg(t), quoted(v))),
    ]
}

/// A place term for a folder, quoted when its path has a space.
pub fn place(t: &Term, path: &str) -> String {
    let field = t.field.unwrap_or("under");
    format!("{}{field}:{}", neg(t), quoted(path))
}

/// The other reading of a place: `under:` everything below, `parent:` only
/// what is directly inside.
pub fn place_flipped(t: &Term) -> Choice {
    let (other, label) = match t.field {
        Some("parent") => ("under", "everything below"),
        _ => ("parent", "only what is directly inside"),
    };
    Choice::new(
        label,
        other,
        format!("{}{other}:{}", neg(t), quoted(&t.value)),
    )
}

/// The folder a place term's value names, and the part after its last `/`
/// still being typed: `~/Bel` is `~` and `Bel`.
pub fn place_parts(value: &str) -> (String, String) {
    match value.rfind('/') {
        Some(0) => ("/".into(), value[1..].to_string()),
        Some(i) => (value[..i].to_string(), value[i + 1..].to_string()),
        None => (String::new(), value.to_string()),
    }
}

#[cfg(test)]
#[path = "lens_tests.rs"]
mod tests;
