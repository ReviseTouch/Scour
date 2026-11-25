//! `GET /api/facets`: the sidebar's and the lenses' counts, every question
//! answered by one walk.
//!
//! - `q` the query; `by` a comma list of `kind`, `ext`, `age` (default `kind`),
//!   one group each, answered in the order asked.
//! - `top` how many extensions `ext` returns: default 24, at most [`EXT_TOP_MAX`].
//! - `edges` age edges in days, ascending, comma-separated; several lists are
//!   separated by `;`. The n-th `age` in `by` takes the n-th list, and an `age`
//!   past the last list takes the last, so one list serves them all:
//!   `by=age,age&edges=0,1,6,29,364;1,2,3,4,5,6,7`.
//! - `exact=1` makes kind and ext counts exact rather than the first 200,000
//!   rows, as the window does: an age group lifts the service's sampling cap,
//!   so one is added when none was asked, and left out of the reply.
//!
//! Reply: `{groups: [{by, facets: [{key, count}], top?, edges?}], total,
//! capped, took_us}`. An `ext` group carries its `top`, an `age` group the
//! `edges` it was counted by, so two age groups are told apart by more than
//! their order. Age keys are an edge as text or `older`; only non-zero bands.

use std::net::TcpStream;

use scour_client::Pool;
use scour_core::{FacetBy, FacetResponse};
use scour_proto::{Request, Response};

use crate::{call, http};

/// `ext`'s top when none is asked.
const EXT_TOP: u32 = 24;
/// The most `top` may ask: a lens lists, it does not export.
pub(crate) const EXT_TOP_MAX: u32 = 1000;

pub(super) fn api_facets(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let (request, added) = facet_request(req);
    match call(client, request) {
        Ok(Response::Facets(r)) => http::json(stream, &facet_json(&r, added)),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// The request the query string asks, and whether an age group was added
/// only to make it exact.
pub(crate) fn facet_request(req: &http::Req) -> (Request, bool) {
    // Sorted because the service files a row by the first edge it is under.
    let lists: Vec<Vec<u32>> = req
        .param("edges")
        .unwrap_or_default()
        .split(';')
        .map(|list| {
            let mut edges: Vec<u32> = list.split(',').filter_map(|s| s.parse().ok()).collect();
            edges.sort_unstable();
            edges.dedup();
            edges
        })
        .collect();
    let top = req
        .param("top")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(EXT_TOP)
        .clamp(1, EXT_TOP_MAX);
    let mut by = Vec::new();
    let mut ages = 0;
    for name in req.param("by").unwrap_or("kind").split(',') {
        by.push(match name {
            "" => continue,
            "age" => {
                let edges = lists.get(ages).or(lists.last()).cloned();
                ages += 1;
                FacetBy::Age {
                    edges: edges.unwrap_or_default(),
                }
            }
            "ext" => FacetBy::Ext { top },
            _ => FacetBy::Kind,
        });
    }
    let added = ages == 0 && req.param("exact") == Some("1");
    if added {
        by.push(FacetBy::Age {
            edges: scour_ui::bar_edges(),
        });
    }
    let query = req.param("q").unwrap_or_default().to_owned();
    (Request::Facets { query, by }, added)
}

/// The reply as the page reads it, without the group `added` asked for.
pub(crate) fn facet_json(r: &FacetResponse, added: bool) -> serde_json::Value {
    let asked = r.groups.len() - usize::from(added && !r.groups.is_empty());
    let groups: Vec<serde_json::Value> = r.groups[..asked]
        .iter()
        .map(|g| {
            let mut out = serde_json::json!({
                "by": match &g.by {
                    FacetBy::Kind => "kind",
                    FacetBy::Ext { .. } => "ext",
                    FacetBy::Dir { .. } => "dir",
                    FacetBy::Age { .. } => "age",
                },
                "facets": g.facets.iter()
                    .map(|f| serde_json::json!({ "key": f.key, "count": f.count }))
                    .collect::<Vec<_>>(),
            });
            match &g.by {
                FacetBy::Ext { top } => out["top"] = serde_json::json!(top),
                FacetBy::Age { edges } => out["edges"] = serde_json::json!(edges),
                _ => {}
            }
            out
        })
        .collect();
    serde_json::json!({
        "groups": groups,
        "total": r.total,
        "capped": r.capped,
        "took_us": r.took_us,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(query: &[(&str, &str)]) -> (Vec<FacetBy>, bool) {
        let req = http::Req {
            method: "GET".into(),
            path: "/api/facets".into(),
            query: query
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            headers: Default::default(),
            body: String::new(),
        };
        match facet_request(&req) {
            (Request::Facets { by, .. }, added) => (by, added),
            (other, _) => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_pages_calls_ask_what_they_always_asked() {
        assert_eq!(asked(&[]), (vec![FacetBy::Kind], false));
        assert_eq!(
            asked(&[("by", "kind,ext,age"), ("edges", "1,7,30,365")]),
            (
                vec![
                    FacetBy::Kind,
                    FacetBy::Ext { top: 24 },
                    FacetBy::Age {
                        edges: vec![1, 7, 30, 365]
                    },
                ],
                false
            )
        );
        assert_eq!(
            asked(&[("by", "age")]),
            (vec![FacetBy::Age { edges: vec![] }], false)
        );
    }

    #[test]
    fn ext_takes_the_top_it_is_given_within_bounds() {
        let top = |t: &str| asked(&[("by", "ext"), ("top", t)]).0;
        assert_eq!(top("8"), vec![FacetBy::Ext { top: 8 }]);
        assert_eq!(top("0"), vec![FacetBy::Ext { top: 1 }]);
        assert_eq!(top("4000000000"), vec![FacetBy::Ext { top: EXT_TOP_MAX }]);
        assert_eq!(top("many"), vec![FacetBy::Ext { top: 24 }]);
    }

    #[test]
    fn each_age_takes_its_own_list_and_the_last_list_repeats() {
        let (by, _) = asked(&[
            ("by", "age,kind,age,age"),
            ("edges", "0,1,6,29,364;7,1,2,2"),
        ]);
        let age = |edges: Vec<u32>| FacetBy::Age { edges };
        assert_eq!(
            by,
            vec![
                age(vec![0, 1, 6, 29, 364]),
                FacetBy::Kind,
                // Sorted and without the repeat: unsorted edges misfile rows.
                age(vec![1, 2, 7]),
                age(vec![1, 2, 7]),
            ]
        );
    }

    #[test]
    fn exact_adds_one_age_group_only_when_none_was_asked() {
        let (by, added) = asked(&[("by", "kind"), ("exact", "1")]);
        assert!(added);
        assert_eq!(
            by,
            vec![
                FacetBy::Kind,
                FacetBy::Age {
                    edges: scour_ui::bar_edges()
                }
            ]
        );
        let (by, added) = asked(&[("by", "ext,age"), ("edges", "1"), ("exact", "1")]);
        assert!(!added);
        assert_eq!(by.len(), 2);
        assert!(!asked(&[("by", "kind"), ("exact", "0")]).1);
    }
}
