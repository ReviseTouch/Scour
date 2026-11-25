//! The launcher: the filters `:` starts, in two groups, each with the picture
//! its capsule's header carries and whether the query already holds one.

use super::{Choice, Term, replace, terms};

/// Which heading a filter of the launcher is listed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Leaves fewer files: place, time, type, extension, size.
    Narrow,
    /// Changes how the words are looked for.
    How,
}

impl Group {
    /// The msgid of its heading.
    pub fn heading(self) -> &'static str {
        match self {
            Group::Narrow => "Narrow",
            Group::How => "How it searches",
        }
    }
}

/// One filter `:` can start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Filter {
    /// The field's canonical name; the term it starts is `field:`.
    pub field: &'static str,
    /// The msgid that names it.
    pub label: &'static str,
    /// The msgid of the line that says what it does.
    pub about: &'static str,
    pub group: Group,
    /// The picture its capsule's header is drawn as: `folder`, `clock`,
    /// `shapes`, `ext`, `size`, `approx`, `read`.
    pub icon: &'static str,
}

const fn filter(
    field: &'static str,
    label: &'static str,
    about: &'static str,
    group: Group,
    icon: &'static str,
) -> Filter {
    Filter {
        field,
        label,
        about,
        group,
        icon,
    }
}

/// The filters `:` starts, in the order they are listed.
pub const LAUNCH: &[Filter] = &[
    filter(
        "under",
        "Location",
        "which disk, which folder",
        Group::Narrow,
        "folder",
    ),
    filter(
        "dm",
        "Modified",
        "when it last changed",
        Group::Narrow,
        "clock",
    ),
    filter(
        "kind",
        "Type",
        "document, code, image…",
        Group::Narrow,
        "shapes",
    ),
    filter("ext", "Extension", "pdf, rs, docx…", Group::Narrow, "ext"),
    filter("size", "Size", "how big", Group::Narrow, "size"),
    filter(
        "fuzzy",
        "Similar spelling",
        "one letter off still matches",
        Group::How,
        "approx",
    ),
    filter(
        "content",
        "Inside",
        "looks inside the file, not at its name",
        Group::How,
        "read",
    ),
];

/// The filter a field belongs to: `parent:` is listed as the place it is.
pub fn filter_of(field: &str) -> Option<&'static Filter> {
    let key = if field == "parent" { "under" } else { field };
    LAUNCH.iter().find(|f| f.field == key)
}

/// One line of the launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    /// The label and about as msgids, and the term it starts (`under:`).
    pub choice: Choice,
    pub group: Group,
    pub icon: &'static str,
    /// The fewest letters after `:` that leave this line alone, `:` included;
    /// empty when none do.
    pub key: String,
    /// The query already has a term of this field: choosing it goes there.
    pub held: bool,
}

/// Folded as a typed name is matched: case aside, dotted and dotless i one.
fn fold(s: &str) -> String {
    s.chars()
        .map(|c| {
            if matches!(c, 'İ' | 'I' | 'ı') {
                'i'
            } else {
                c
            }
        })
        .collect::<String>()
        .to_lowercase()
}

/// Whether `want` (folded, without `:`) keeps a filter whose name reads `name`.
fn keeps(f: &Filter, name: &str, want: &str) -> bool {
    want.is_empty() || f.field.starts_with(want) || name.starts_with(want)
}

/// The shortest letters that keep only the filter at `at`; its name's on a tie.
fn key_of(at: usize, names: &[String]) -> String {
    let unique = |p: &str| {
        let hits: Vec<usize> = (0..LAUNCH.len())
            .filter(|&i| !p.is_empty() && keeps(&LAUNCH[i], &names[i], p))
            .collect();
        hits == [at]
    };
    let prefixes = |s: &str| -> Vec<String> {
        s.char_indices()
            .map(|(i, c)| s[..i + c.len_utf8()].to_string())
            .collect()
    };
    let (name, field) = (prefixes(&names[at]), prefixes(LAUNCH[at].field));
    let longest = name.len().max(field.len());
    (0..longest)
        .flat_map(|n| [name.get(n), field.get(n)])
        .flatten()
        .find(|p| unique(p))
        .map_or_else(String::new, |p| format!(":{p}"))
}

/// Whether a term is of this filter's field, written with `!` or not.
fn holds(field: &str, t: &Term) -> bool {
    t.field
        .is_some_and(|f| filter_of(f).is_some_and(|x| x.field == field))
}

/// The first term of `query` that is of `field`'s filter.
pub fn held(query: &str, field: &str) -> Option<Term> {
    terms(query).into_iter().find(|t| holds(field, t))
}

/// What `:` and the letters after it offer in `query`, matched against the
/// field and the name a person reads (`name` translates a msgid).
pub fn launch(query: &str, typed: &str, name: impl Fn(&str) -> String) -> Vec<Start> {
    let want = fold(typed.trim_start_matches(':'));
    let names: Vec<String> = LAUNCH.iter().map(|f| fold(&name(f.label))).collect();
    LAUNCH
        .iter()
        .enumerate()
        .filter(|(i, f)| keeps(f, &names[*i], &want))
        .map(|(i, f)| Start {
            choice: Choice::new(f.label, f.about, format!("{}:", f.field)),
            group: f.group,
            icon: f.icon,
            key: key_of(i, &names),
            held: held(query, f.field).is_some(),
        })
        .collect()
}

/// Choosing a held line: the launcher term goes, and the caret stands at the
/// end of the term already there, which opens its own lens.
pub fn into_held(query: &str, launcher: &Term, field: &str) -> Option<(String, usize)> {
    let (rest, _) = replace(query, launcher, "");
    let t = held(&rest, field)?;
    Some((rest, t.end))
}
