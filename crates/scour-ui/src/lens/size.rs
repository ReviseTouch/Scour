//! The size panel: five presets, each counted with its own `count` (there is
//! no size facet), and which preset a `size:` or `empty:` term is.

use super::rest::{is_empty_term, with_term, without_field};
use super::{Term, neg};

/// One preset: its msgid and what it writes, without `!`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizePreset {
    pub label: &'static str,
    pub term: &'static str,
}

/// The presets, smallest first. The four bands are [`SIZES`](super::SIZES).
pub const SIZE_PRESETS: [SizePreset; 5] = [
    SizePreset {
        label: "Empty",
        term: "empty:",
    },
    SizePreset {
        label: "under 100 KB",
        term: "size:<100kb",
    },
    SizePreset {
        label: "100 KB to 10 MB",
        term: "size:100kb..10mb",
    },
    SizePreset {
        label: "10 MB to 1 GB",
        term: "size:10mb..1gb",
    },
    SizePreset {
        label: "over 1 GB",
        term: "size:>1gb",
    },
];

/// Empty left out: `!empty:`, every folder and every file with bytes in it.
/// Not `size:>0`, which also drops every folder: a folder's size is stored
/// as 0.
pub const NOT_EMPTY: &str = "!empty:";

/// The term preset `i` writes in place of `t`, its `!` kept.
pub fn size_preset_term(t: &Term, i: usize) -> String {
    format!("{}{}", neg(t), SIZE_PRESETS[i].term)
}

/// Which preset a term is, `!` set aside (read it from `t.not`): `empty:`
/// and `bos:` are Empty, a `size:` value one of the four as written, any
/// case. `None` for anything else, `size:>0` included.
pub fn size_preset_of(text: &str, t: &Term) -> Option<usize> {
    if is_empty_term(text, t) {
        return Some(0);
    }
    if t.field != Some("size") {
        return None;
    }
    let v = t.value.to_lowercase();
    SIZE_PRESETS
        .iter()
        .position(|p| p.term.strip_prefix("size:") == Some(v.as_str()))
}

/// The query each preset is counted with, in [`SIZE_PRESETS`] order: the
/// query less its size terms (`empty:` among them), and the preset.
pub fn size_counts(text: &str) -> Vec<String> {
    let rest = without_field(text, "size");
    SIZE_PRESETS
        .iter()
        .map(|p| with_term(&rest, p.term))
        .collect()
}

#[cfg(test)]
#[path = "size_tests.rs"]
mod tests;
