use scour_core::FacetBy;
use scour_proto::{Request, Response};
use scour_ui::lens::{PRESET_EDGES, Scale};

use crate::qlens::{Chosen, Env, Lenses};
use crate::{qsize, qtime, qtype};

fn reply(json: &str) -> Response {
    serde_json::from_str(json).expect("a reply the service sends")
}

fn count(n: u64) -> Response {
    reply(&format!(
        r#"{{"result":"count","total":{n},"capped":false}}"#
    ))
}

fn with_env<R>(f: impl FnOnce(&Env) -> R) -> R {
    let tr = |m: &str| m.to_string();
    f(&Env {
        tr: &tr,
        home: "/home/u",
        places: &[],
    })
}

fn open(lenses: &mut Lenses, q: &str) -> Vec<(u32, Request)> {
    with_env(|env| lenses.sync(q, q.len(), true, env))
}

fn answer(lenses: &mut Lenses, g: u64, slot: u32, r: Response, q: &str) -> Vec<(u32, Request)> {
    with_env(|env| lenses.answer(g, slot, r, q, env))
}

#[test]
fn the_time_panel_asks_one_walk_for_the_presets_and_the_bars_and_once_per_scale() {
    let mut lenses = Lenses::default();
    let q = "rapor dm:7d";
    let wants = open(&mut lenses, q);
    assert_eq!(wants.len(), 1);
    let Request::Facets { query, by } = &wants[0].1 else {
        panic!("a facet");
    };
    assert_eq!(query, "rapor");
    assert_eq!(
        by,
        &[
            FacetBy::Age {
                edges: PRESET_EDGES.to_vec()
            },
            FacetBy::Age {
                edges: Scale::Day.edges()
            },
        ]
    );
    let ages = reply(
        r#"{"result":"facets","groups":[
          {"by":{"age":{"edges":[0,1,6,29,364]}},"facets":[{"key":"0","count":5},{"key":"6","count":2},{"key":"older","count":9}]},
          {"by":{"age":{"edges":[0]}},"facets":[{"key":"0","count":5},{"key":"5","count":2}]}],
          "total":16,"facets":[],"took_us":0}"#,
    );
    let g = lenses.generation;
    assert!(answer(&mut lenses, g, qtime::AGES, ages, q).is_empty());
    let o = lenses.open.as_ref().unwrap();
    let ns: Vec<Option<i64>> = o.items.iter().map(|i| i.n).collect();
    assert_eq!(ns, [Some(5), Some(5), Some(7), Some(7), Some(7)]);
    assert_eq!(qtime::kept(o, &lenses.look), Some(7));
    assert_eq!(o.sel, 2, "the panel rests on the preset the term is");
    // A preset written, the panel open on it again: nothing asked.
    lenses.hold = true;
    assert!(open(&mut lenses, "rapor dm:30d").is_empty());
    // Another scale is another question.
    lenses.look.scale = Scale::Month;
    let wants = with_env(|env| lenses.refill("rapor dm:30d", env));
    assert_eq!(wants.len(), 1);
}

#[test]
fn an_answer_that_outlives_its_generation_is_kept_under_what_it_was_asked_for() {
    let mut lenses = Lenses::default();
    let q = "dm:7d";
    open(&mut lenses, q);
    let old = lenses.generation;
    lenses.hold = true;
    let again = open(&mut lenses, "dm:30d");
    assert_eq!(again.len(), 1, "asked again under the new generation");
    let ages = reply(r#"{"result":"facets","groups":[],"total":3,"facets":[],"took_us":0}"#);
    answer(&mut lenses, old, qtime::AGES, ages, "dm:30d");
    assert!(lenses.open.as_ref().unwrap().got.ages.is_some());
}

#[test]
fn the_size_panel_counts_its_five_presets_over_the_query_less_its_size_terms() {
    let mut lenses = Lenses::default();
    let q = "rapor empty:";
    let wants = open(&mut lenses, q);
    let queries: Vec<String> = wants
        .iter()
        .map(|(_, r)| match r {
            Request::Count { query, .. } => query.clone(),
            _ => panic!("a count"),
        })
        .collect();
    assert_eq!(
        queries,
        [
            "rapor empty:",
            "rapor size:<100kb",
            "rapor size:100kb..10mb",
            "rapor size:10mb..1gb",
            "rapor size:>1gb"
        ]
    );
    let g = lenses.generation;
    answer(&mut lenses, g, qsize::SIZES, count(310), q);
    let o = lenses.open.as_ref().unwrap();
    assert_eq!(o.items[0].n, Some(310));
    assert_eq!(
        qsize::chosen(o, q),
        Some(0),
        "empty: opens the size panel on Empty"
    );
    assert_eq!(o.sel, 0);
}

#[test]
fn the_type_panel_asks_one_walk_until_a_kind_is_chosen_then_two() {
    let mut lenses = Lenses::default();
    let wants = open(&mut lenses, "rapor kind:");
    assert_eq!(wants.len(), 1);
    let Request::Facets { by, .. } = &wants[0].1 else {
        panic!("a facet");
    };
    assert_eq!(by.len(), 3);
    let wants = open(&mut lenses, "rapor kind:doc");
    let asked: Vec<(u32, String)> = wants
        .iter()
        .map(|(s, r)| match r {
            Request::Facets { query, .. } => (*s, query.clone()),
            _ => panic!("a facet"),
        })
        .collect();
    assert_eq!(
        asked,
        [
            (qtype::KINDS, "rapor".into()),
            (qtype::EXTS, "rapor kind:doc".into())
        ]
    );
}

#[test]
fn ext_opens_the_type_panel_on_its_extensions() {
    let mut lenses = Lenses::default();
    let q = "ext:pdf";
    open(&mut lenses, q);
    let o = lenses.open.as_ref().unwrap();
    assert_eq!(o.items.len(), 14, "thirteen kinds and the pdf held");
    assert_eq!(o.sel, 13);
    assert_eq!(o.items[13].value, Some(("ext", "pdf".to_string())));
}

#[test]
fn enter_on_a_tile_toggles_its_kind_and_on_a_preset_panel_resting_on_none_keeps_the_term() {
    let mut lenses = Lenses::default();
    open(&mut lenses, "kind:doc");
    let Some(Chosen::Value { field, value, keep }) = lenses.choose(2, true, "kind:doc") else {
        panic!("a value");
    };
    assert_eq!((field, value.as_str(), keep), ("kind", "doc", true));
    let q = "dm:30d..7d";
    open(&mut lenses, q);
    let o = lenses.open.as_ref().unwrap();
    assert_eq!(o.sel, o.items.len(), "no preset is this band");
    let Some(Chosen::Edit { query, keep, .. }) = lenses.choose(o.sel, false, q) else {
        panic!("an edit");
    };
    assert_eq!((query.as_str(), keep), (q, false));
}

#[test]
fn a_negated_size_preset_counts_the_presets_as_ever_and_the_query_for_what_it_keeps() {
    let mut lenses = Lenses::default();
    let q = "rapor !empty:";
    let wants = open(&mut lenses, q);
    let queries: Vec<(u32, String)> = wants
        .iter()
        .map(|(s, r)| match r {
            Request::Count { query, .. } => (*s, query.clone()),
            _ => panic!("a count"),
        })
        .collect();
    assert_eq!(queries[0], (qsize::SIZES, "rapor empty:".into()));
    assert_eq!(queries.last().unwrap(), &(qsize::WHOLE, q.to_string()));
    assert_eq!(queries.len(), 6);
}
