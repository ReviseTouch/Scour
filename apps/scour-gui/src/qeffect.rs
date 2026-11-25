//! What a capsule does to the list, said over it and never in its line: the
//! pill's "18,420 → 212" (the query without the capsule against the list's
//! total), the red mark on a capsule whose removal would bring an empty list
//! back, and the runs joined by `|` that get a dashed frame. The page's
//! rules (`038-capsule-marks.js`), kept the same.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_proto::{Request, Response};
use scour_ui::lens::{self, Term};
use slint::ComponentHandle;

use crate::link::{Ask, Link};
use crate::{MainWindow, State};

/// The lens lane's slots from here up are the pill's counts.
pub const EFFECT: u32 = 2000;
/// How long the pointer rests on a capsule, and how long since the last key,
/// before its effect is asked.
const REST: Duration = Duration::from_millis(250);
const QUIET: Duration = Duration::from_millis(600);

/// The pill's counts: answered by query text and capsule span, and what is
/// on its way by generation and slot.
#[derive(Default)]
pub struct Effect {
    seen: HashMap<String, (u64, bool)>,
    pending: HashMap<(u64, u32), String>,
    next: u32,
    timer: slint::Timer,
}

/// The query a capsule at `[a, b]` is counted without: every term in it out,
/// each with its `|` (`lens::remove_term`), the later first.
pub fn without(text: &str, a: usize, b: usize) -> String {
    let mut inside: Vec<Term> = lens::terms(text)
        .into_iter()
        .filter(|t| t.start >= a && t.end <= b)
        .collect();
    inside.sort_by_key(|t| std::cmp::Reverse(t.start));
    let mut out = text.to_string();
    for t in inside {
        out = lens::remove_term(&out, &t).0;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the capsule at `t` gets an effect: a filter with something in it,
/// or a group; never a plain word, an empty filter, or a query reading files.
pub fn counted(text: &str, t: &Term) -> bool {
    let members = lens::terms(text)
        .into_iter()
        .filter(|m| m.start >= t.start && m.end <= t.end)
        .count();
    let filter = members > 1 || ((t.field.is_some() || t.not) && !t.value.trim().is_empty());
    filter && !lens::reads(text)
}

/// What leaving the capsule at `[a, b]` out would find, when the empty
/// list's answer (by term text) names a term drawn in it and finds some.
pub fn rescuer(
    text: &str,
    a: usize,
    b: usize,
    rescue: &[(String, String, Option<(u64, bool)>)],
) -> Option<(u64, bool)> {
    lens::terms(text)
        .into_iter()
        .filter(|t| t.start >= a && t.end <= b)
        .find_map(|t| {
            let raw = &text[t.start..t.end];
            rescue
                .iter()
                .find(|r| r.0 == raw)
                .and_then(|r| r.2)
                .filter(|n| n.0 > 0)
        })
}

/// The runs joined by `|` that are framed: two members or more, not drawn as
/// one capsule already (`drawn`, the group capsules' spans).
pub fn or_runs(text: &str, drawn: &[(usize, usize)]) -> Vec<(usize, usize)> {
    lens::runs(text)
        .into_iter()
        .filter(|r| r.len() > 1)
        .map(|r| (r[0].start, r[r.len() - 1].end))
        .filter(|&(a, b)| !drawn.iter().any(|&(s, e)| a >= s && b <= e))
        .collect()
}

/// Whether an answer of `generation` to the empty list's rescue, `asked`
/// with a generation for a query, is for the list shown now.
pub fn current(asked: (u64, &str), generation: u64, query: &str) -> bool {
    asked.0 == generation && asked.1 == query
}

fn said(n: u64, capped: bool) -> String {
    format!("{}{}", crate::grouped(n), if capped { "+" } else { "" })
}

/// The pill's words for the capsule at `t`, from what is known now.
fn words(s: &State, cat: &Catalogue, text: &str, t: &Term) -> Option<String> {
    let qb = &s.qb;
    if qb.rescue_for == text
        && let Some((n, capped)) = rescuer(text, t.start, t.end, &qb.rescue)
    {
        return Some(
            cat.get("without it: {n} results")
                .replace("{n}", &said(n, capped)),
        );
    }
    let (n, capped) = *qb.effect.seen.get(&key(text, t))?;
    let (listed, total, all) = qb.listed.as_ref()?;
    (listed == text).then(|| format!("{} → {}", said(n, capped), said(*total, !all)))
}

fn key(text: &str, t: &Term) -> String {
    format!("{text}\n{}\n{}", t.start, t.end)
}

/// The pointer came to rest on a capsule: say its effect if it is known,
/// else ask once it has rested and nobody types.
pub fn hovered(w: &MainWindow, state: &Rc<RefCell<State>>, link: &Rc<Link>, cat: &Catalogue) {
    let text = w.get_query().to_string();
    let Some(t) = state.borrow().qb.hover.clone() else {
        w.set_hov_fx(Default::default());
        return;
    };
    let known = words(&state.borrow(), cat, &text, &t);
    w.set_hov_fx(known.clone().unwrap_or_default().into());
    if known.is_some() || !counted(&text, &t) {
        return;
    }
    let (weak, st, link) = (w.as_weak(), Rc::clone(state), Rc::clone(link));
    let ask = move || {
        let Some(w) = weak.upgrade() else { return };
        let mut s = st.borrow_mut();
        let still = s.qb.hover.as_ref() == Some(&t) && w.get_query() == text.as_str();
        let quiet = s.typed_at.is_none_or(|at| at.elapsed() >= QUIET);
        if !still || !quiet {
            return;
        }
        let generation = s.qb.lenses.generation;
        let slot = EFFECT + s.qb.effect.next % 1000;
        s.qb.effect.next += 1;
        s.qb.effect
            .pending
            .insert((generation, slot), key(&text, &t));
        link.send(Ask::Lens {
            generation,
            slot,
            request: Request::Count {
                query: without(&text, t.start, t.end),
                cap: 100_000,
            },
        });
    };
    // Past the rest, and past the quiet after the last key.
    let typed = state.borrow().typed_at.map(|at| at.elapsed());
    let wait = typed.map_or(REST, |e| REST.max(QUIET.saturating_sub(e)));
    state
        .borrow()
        .qb
        .effect
        .timer
        .start(slint::TimerMode::SingleShot, wait, ask);
}

/// A count for the pill came back: kept under what it was asked for, and
/// said if the pointer is still on that capsule.
pub fn answered(
    w: &MainWindow,
    s: &mut State,
    cat: &Catalogue,
    generation: u64,
    slot: u32,
    reply: &Response,
) {
    let Response::Count { total, capped, .. } = reply else {
        return;
    };
    let Some(k) = s.qb.effect.pending.remove(&(generation, slot)) else {
        return;
    };
    s.qb.effect.seen.insert(k, (*total, *capped));
    let text = w.get_query().to_string();
    if let Some(t) = s.qb.hover.clone()
        && let Some(said) = words(s, cat, &text, &t)
    {
        w.set_hov_fx(said.into());
    }
}

#[cfg(test)]
#[path = "qeffect_tests.rs"]
mod tests;
