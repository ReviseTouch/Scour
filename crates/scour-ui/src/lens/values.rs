//! The type panel: `kind:` and `ext:` values put in, taken out and left out.
//! Negation covers a whole term, so "documents but not build output" is two
//! terms, `kind:doc !kind:build`; a value is in one or the other, never both.

use super::group::{remove_term, runs};
use super::rest::{appended, field_terms, without_field};
use super::{Choice, KINDS, Term, neg, replace};

/// The kinds as tiles, in [`KINDS`] order, with the msgids that name them
/// (`scour_core::Kind::msgid`).
pub const KIND_LABELS: [(&str, &str); 13] = [
    ("folder", "Folder"),
    ("code", "Code"),
    ("doc", "Document"),
    ("image", "Image"),
    ("data", "Data"),
    ("config", "Configuration"),
    ("archive", "Archive"),
    ("exec", "Executable"),
    ("audio", "Audio"),
    ("video", "Video"),
    ("font", "Font"),
    ("build", "Build output"),
    ("file", "File"),
];

/// The other spellings of one kind the service reads (`Kind::from_name`).
const KIND_SPELLINGS: &[(&str, &[&str])] = &[
    ("folder", &["dir", "klasor", "klasör"]),
    ("code", &["source", "kod", "kaynak"]),
    ("image", &["img", "pic", "gorsel", "görsel", "resim"]),
    ("archive", &["arsiv", "arşiv", "zip"]),
    ("doc", &["document", "belge"]),
    (
        "exec",
        &[
            "exe",
            "bin",
            "executable",
            "calistirilabilir",
            "çalıştırılabilir",
        ],
    ),
    ("audio", &["ses", "muzik", "müzik"]),
    ("video", &["film"]),
    ("build", &["derleme", "cikti", "çıktı"]),
    ("data", &["veri"]),
    (
        "config",
        &["conf", "settings", "ayar", "yapilandirma", "yapılandırma"],
    ),
    ("font", &["yazitipi", "yazıtipi"]),
    ("file", &["dosya"]),
];

/// How many extensions the row under the kinds shows.
pub const EXT_TOP: u32 = 8;

/// The msgid of a kind's tile, for a [`KINDS`] token.
pub fn kind_label(token: &str) -> Option<&'static str> {
    KIND_LABELS
        .iter()
        .find(|(k, _)| *k == token)
        .map(|(_, l)| *l)
}

/// A value as the facet keys it: lower case, an extension without its dot,
/// a kind by its token (`belge` is `doc`). One the service names several
/// kinds by (`media`, `text`) stays as written.
pub fn value_key(field: &str, value: &str) -> String {
    let v: String = value
        .chars()
        .map(|c| if matches!(c, 'İ' | 'I') { 'i' } else { c })
        .collect::<String>()
        .to_lowercase();
    match field {
        "ext" => v.trim_start_matches('.').to_string(),
        "kind" if !KINDS.contains(&v.as_str()) => KIND_SPELLINGS
            .iter()
            .find(|(_, all)| all.contains(&v.as_str()))
            .map_or(v, |(k, _)| (*k).to_string()),
        _ => v,
    }
}

/// The values of a list term, as written, empty ones left out.
fn values(t: &Term) -> Vec<&str> {
    t.value.split(';').filter(|v| !v.is_empty()).collect()
}

/// What the query's terms of a list field hold, by [`value_key`], each once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Picked {
    /// In a term without `!`: asked for.
    pub on: Vec<String>,
    /// In a `!` term: left out.
    pub off: Vec<String>,
}

/// The values of `field` (`kind` or `ext`) the query asks for and leaves out.
pub fn picked(text: &str, field: &str) -> Picked {
    let mut out = Picked::default();
    for t in field_terms(text, field) {
        let side = if t.not { &mut out.off } else { &mut out.on };
        for v in values(&t) {
            let k = value_key(field, v);
            if !side.contains(&k) {
                side.push(k);
            }
        }
    }
    out
}

/// The query with one value of `field` toggled, and the caret after what
/// changed. `exclude` false: in a term without `!` (the first, or a new one
/// at the end); true: in a `!` term standing alone (the first, or a new
/// one). A value already on that side comes out, its term with it when it
/// empties; one on the other side moves. Values the panel did not offer
/// stay, and a term keeps its field's spelling.
pub fn value_toggle(text: &str, field: &str, value: &str, exclude: bool) -> (String, usize) {
    let key = value_key(field, value);
    let has = |t: &Term| values(t).iter().any(|v| value_key(field, v) == key);
    let holder = |text: &str, not: bool| {
        field_terms(text, field)
            .into_iter()
            .find(|t| t.not == not && has(t))
    };
    if let Some(t) = holder(text, exclude) {
        return take_out(text, &t, field, &key);
    }
    let text = match holder(text, !exclude) {
        Some(t) => take_out(text, &t, field, &key).0,
        None => text.to_string(),
    };
    let alone = |t: &Term| runs(&text).iter().any(|r| r.len() == 1 && r[0] == *t);
    let into = field_terms(&text, field)
        .into_iter()
        .find(|t| t.not == exclude && (!exclude || alone(t)));
    match into {
        Some(t) => {
            let mut vs = values(&t);
            vs.push(value);
            replace(&text, &t, &written(&text, &t, &vs))
        }
        None => {
            let bang = if exclude { "!" } else { "" };
            appended(&text, &format!("{bang}{field}:{value}"))
        }
    }
}

/// A term less the values keyed `key`; gone when none is left.
fn take_out(text: &str, t: &Term, field: &str, key: &str) -> (String, usize) {
    let left: Vec<&str> = values(t)
        .into_iter()
        .filter(|v| value_key(field, v) != key)
        .collect();
    if left.is_empty() {
        remove_term(text, t)
    } else {
        replace(text, t, &written(text, t, &left))
    }
}

/// A list term rewritten with `values`, its `!` and field's spelling kept.
fn written(text: &str, t: &Term, values: &[&str]) -> String {
    let raw = text[t.start..t.end].trim_start_matches('!');
    let name = raw.split_once(':').map_or("kind", |(n, _)| n);
    format!("{}{name}:{}", neg(t), values.join(";"))
}

/// The choices of [`list`](super::list) that keep what they were not handed:
/// toggling one value leaves every other value of the term as it is.
pub fn list_kept(t: &Term, values: &[(String, String)]) -> Vec<Choice> {
    let field = t.field.unwrap_or("kind");
    let have = self::values(t);
    let key = |v: &str| value_key(field, v);
    values
        .iter()
        .map(|(value, label)| {
            let on = have.iter().any(|h| key(h) == key(value));
            let mut next: Vec<&str> = have
                .iter()
                .copied()
                .filter(|h| key(h) != key(value))
                .collect();
            if !on {
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

/// The two queries the type panel counts over.
///
/// Ask `facets{query: kinds, by: [Kind, Age{edges: [0]}]}` for the tiles and
/// `facets{query: exts, by: [Ext{top: EXT_TOP}, Age{edges: [0]}]}` for the
/// extensions under them; the age group is there only to make the counts
/// exact (the bridge: `by=kind,age&edges=0`, or `exact=1`). When
/// [`TypeAsk::one_walk`], one call `by: [Kind, Ext{top}, Age]` answers both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAsk {
    /// The query less its `kind:` terms, `!` or not.
    pub kinds: String,
    /// The query less its `ext:` terms: the kinds chosen narrow it.
    pub exts: String,
}

impl TypeAsk {
    pub fn one_walk(&self) -> bool {
        self.kinds == self.exts
    }
}

pub fn type_ask(text: &str) -> TypeAsk {
    TypeAsk {
        kinds: without_field(text, "kind"),
        exts: without_field(text, "ext"),
    }
}

#[cfg(test)]
#[path = "values_tests.rs"]
mod tests;
