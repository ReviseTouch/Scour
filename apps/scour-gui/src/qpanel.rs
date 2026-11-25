//! What the time, type and size panels ask the service and keep of its
//! answers. Each answer is kept under the key its question was asked with,
//! so one that comes back after the query moved on still lands where it
//! belongs, and nothing is asked twice while the lens stays open.

use std::collections::HashMap;

use scour_core::{FacetBy, FacetResponse};
use scour_proto::{Request, Response};

/// Counted up to this and no further: a choice that keeps more is "100,000+".
pub const COUNT_CAP: u32 = 100_000;

/// One question: its slot on the lens lane, the key its answer is kept
/// under, and the request.
pub type Want = (u32, String, Request);

/// A count as answered: how many, and whether that is a floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counted {
    pub n: u64,
    pub capped: bool,
}

/// What the panels were told, on the open lens.
#[derive(Debug, Clone, Default)]
pub struct Got {
    /// Time: the presets' and the bars' age facet.
    pub ages: Option<FacetResponse>,
    /// Type: the kinds' facet, and the extensions' (the same answer when
    /// one walk asks both).
    pub kinds: Option<FacetResponse>,
    pub exts: Option<FacetResponse>,
    /// Size: each preset's count, in `SIZE_PRESETS` order.
    pub sizes: Vec<Option<Counted>>,
    /// What the whole query finds, when no bars say it.
    pub whole: Option<Counted>,
}

pub fn facets<'a>(seen: &'a HashMap<String, Response>, key: &str) -> Option<&'a FacetResponse> {
    match seen.get(key) {
        Some(Response::Facets(f)) => Some(f),
        _ => None,
    }
}

/// A count already answered, or asked for in `out` under `count {query}`.
pub fn count(
    seen: &HashMap<String, Response>,
    slot: u32,
    query: &str,
    out: &mut Vec<Want>,
) -> Option<Counted> {
    let key = format!("count {query}");
    match seen.get(&key) {
        Some(Response::Count { total, capped, .. }) => Some(Counted {
            n: *total,
            capped: *capped,
        }),
        _ => {
            out.push((
                slot,
                key,
                Request::Count {
                    query: query.to_string(),
                    cap: COUNT_CAP,
                },
            ));
            None
        }
    }
}

/// A facet already answered, or asked for in `out` under `key`.
pub fn ask_facets(
    seen: &HashMap<String, Response>,
    slot: u32,
    key: String,
    query: &str,
    by: Vec<FacetBy>,
    out: &mut Vec<Want>,
) -> Option<FacetResponse> {
    if let Some(f) = facets(seen, &key) {
        return Some(f.clone());
    }
    out.push((
        slot,
        key,
        Request::Facets {
            query: query.to_string(),
            by,
        },
    ));
    None
}

/// The `(key, count)` pairs of the first group asked as `by` says.
pub fn group(f: &FacetResponse, which: impl Fn(&FacetBy) -> bool) -> Vec<(&str, u64)> {
    f.groups
        .iter()
        .find(|g| which(&g.by))
        .map(|g| g.facets.iter().map(|x| (x.key.as_str(), x.count)).collect())
        .unwrap_or_default()
}

/// The `n`th group's pairs, whatever it was asked as.
pub fn nth(f: &FacetResponse, n: usize) -> Vec<(&str, u64)> {
    f.groups
        .get(n)
        .map(|g| g.facets.iter().map(|x| (x.key.as_str(), x.count)).collect())
        .unwrap_or_default()
}

/// A count as the panels say it: grouped, `+` after a floor.
pub fn said(c: Option<Counted>) -> String {
    match c {
        Some(Counted { n, capped: true }) => format!("{}+", crate::grouped(n)),
        Some(Counted { n, .. }) => crate::grouped(n),
        None => "…".into(),
    }
}

#[cfg(test)]
#[path = "qpanel_tests.rs"]
mod tests;
