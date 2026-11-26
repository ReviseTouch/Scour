//! Asking for the list: debounced, paged, refreshed in the background, the
//! query read back and remembered.

use super::*;
use link::{Ask, Half, Link};
use scour_i18n::Catalogue;
use std::cell::RefCell;
use std::rc::Rc;

pub(super) fn fetch_from(first: usize, visible: usize, wanted: Option<usize>) -> usize {
    wanted
        .filter(|row| (first..=first.saturating_add(visible)).contains(row))
        .unwrap_or(first)
}

/// Refresh is optional; its rest starts after the reply, so a slow response
/// cannot spend the delay in flight and repeat at once.
pub(super) fn refresh_ready(elapsed: Option<std::time::Duration>, cost_us: u64) -> bool {
    let rest = SETTLED.max(std::time::Duration::from_micros(cost_us).saturating_mul(10));
    elapsed.is_none_or(|spent| spent >= rest)
}

/// Somebody did something here: restarts [`AWAKE_FOR`] and, if the window had
/// dozed, asks at once rather than waiting out [`DOZE_AGAIN`].
pub(super) fn stir(state: &Rc<RefCell<State>>, link: &Rc<Link>) {
    let woke = {
        let mut s = state.borrow_mut();
        s.stirred = std::time::Instant::now();
        std::mem::replace(&mut s.dozing, false)
    };
    if woke {
        let since = state.borrow().revision;
        link.send(Ask::Await { since });
    }
}

/// Put the detail list's slots on the rows in sight: one row above the first
/// shown and the rest below, so a step either way finds a slot already there.
pub(super) fn place(w: &MainWindow, slots: &rows::Slots) {
    let from = (w.get_first_row().max(0) as usize).saturating_sub(1);
    slots.place(from, w.get_pool().max(1) as usize);
    w.set_slot_from(from as i32);
}

/// Fetch the page the list is about to need, if it is not already coming.
/// **Fetching cannot happen where the need is noticed**: `row_data` runs while
/// the view is laying out. The position is read, not only a reported miss.
pub(super) fn follow(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    rows: &Rc<rows::Rows>,
) {
    /* **A new question is on its way and the viewport is still the old one's.**
     * A page asked for here would be of the new query at the old position, and
     * the lane keeps only the newest of what is queued: behind a slow page it
     * replaced the search from the top, the rewind never came, and the list
     * stayed where the last query had been scrolled to. The answer from the top
     * calls this again. */
    if state.borrow().rewind {
        return;
    }
    let visible = w.get_visible_rows().max(0) as usize;
    let first = w.get_first_row().max(0) as usize;
    // A layout miss may belong to the previous viewport during a fast jump.
    let first = fetch_from(first, visible, rows.wanted());
    let (revision, cost, quiet) = {
        let s = state.borrow();
        (
            s.revision,
            s.page_cost_us,
            refresh_ready(s.page_landed_at.map(|at| at.elapsed()), s.page_cost_us),
        )
    };
    let Some(page) = rows.next_page(first, first + visible, cost < CHEAP_PAGE_US, quiet) else {
        return;
    };
    trace(&format!(
        "row {first} wants page {page}, {} rows in hand",
        rows.held()
    ));
    rows.asking(page, revision);
    send_page(state, link, (page * rows::SPAN) as u32, PAGE_MAX);
}

/// Send the search for the current state. The facet count is **not** sent here:
/// it costs about what the search does, so it waits — see [`apply`].
pub(super) fn dispatch(
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    model: &Rc<rows::Rows>,
    rows: u32,
) {
    let limit = rows.clamp(20, PAGE_MAX);
    {
        let mut s = state.borrow_mut();
        s.row_limit = limit;
        s.page_offset = 0;
        s.rewind = true;
        s.search_out = true;
    }
    // Keeping the pages in hand across a new query would mean scrolling down
    // into the last query's results.
    model.empty();
    send_search(state, link, 0, limit);
}

/// How long the result is, given a page of it and a count of it. **A page shorter
/// than the service is willing to give is the end of the result**, whatever a
/// count taken a moment ago says: shorter than asked for, and than ever served.
pub(super) fn list_length(
    counted: usize,
    offset: usize,
    got: usize,
    asked: usize,
    served: usize,
) -> usize {
    if got < asked && got < served {
        return offset + got;
    }
    // Otherwise the count stands — but never below what is already in hand.
    counted.max(offset + got)
}

pub(super) fn send_search(state: &Rc<RefCell<State>>, link: &Rc<Link>, offset: u32, limit: u32) {
    // Beside the search, on the same lane and with the same coalescing. What
    // was typed, not the rail's filter: the colours are drawn over the field,
    // and a term painted there that is not in it cannot be deleted.
    let (query_revision, query) = {
        let s = state.borrow();
        (s.query_revision, s.query.clone())
    };
    link.send(Ask::Explain {
        query_revision,
        query,
    });
    send_page(state, link, offset, limit);
}

/// Another page of the query already on screen. Scrolling and the live refresh
/// come here rather than [`send_search`]: the query has not changed.
pub(super) fn send_page(state: &Rc<RefCell<State>>, link: &Rc<Link>, offset: u32, limit: u32) {
    let (generation, query_revision, query, sort, descending) = {
        let mut s = state.borrow_mut();
        s.page_sent = Some(std::time::Instant::now());
        let s = &*s;
        (
            s.generation,
            s.query_revision,
            full_query(s),
            s.sort.clone(),
            s.descending,
        )
    };
    link.send(Ask::Search {
        generation,
        query_revision,
        sort: order_for(&query, &sort),
        query,
        descending,
        offset,
        limit,
    });
}

pub(super) fn schedule_background(
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    query_revision: u64,
    query: String,
) {
    let state = Rc::clone(state);
    let link = Rc::clone(link);
    slint::Timer::single_shot(
        std::time::Duration::from_millis(BACKGROUND_IDLE_MS),
        move || {
            if state.borrow().query_revision != query_revision {
                return;
            }
            // **Each half without the term it sets**: counting the rail through
            // `kind:code` leaves it one row. One walk unless the query names both.
            let for_kinds = scour_query::without(&query, &["kind"]);
            let for_ages = scour_query::without(&query, &["dm"]);
            match (for_kinds, for_ages) {
                (None, None) => link.send(Ask::Facets {
                    query_revision,
                    query,
                    half: Half::Both,
                }),
                (kinds, ages) => {
                    link.send(Ask::Facets {
                        query_revision,
                        query: kinds.unwrap_or_else(|| query.clone()),
                        half: Half::Kinds,
                    });
                    link.send(Ask::Facets {
                        query_revision,
                        query: ages.unwrap_or_else(|| query.clone()),
                        half: Half::Ages,
                    });
                    // **And the count, which neither of those answers any more**:
                    // two walks over two wider queries count something else.
                    let count = {
                        let mut s = state.borrow_mut();
                        s.start_count().then(|| full_query(&s))
                    };
                    if let Some(query) = count {
                        link.send(Ask::Count {
                            query_revision,
                            query,
                        });
                    }
                }
            }
        },
    );
}

/// Below how many characters a term stops narrowing: the trigram filter is built
/// on three-letter keys.
const TRIGRAM_MIN: usize = 3;

/// Which order to actually ask for. Relevance sees **every** match before it
/// knows which forty win: on 2,981,748 entries `t` costs 899 ms against 41 ms in
/// stored order, so below the trigram minimum the stored order is asked for.
pub(super) fn order_for(query: &str, sort: &str) -> String {
    if sort != "relevance" {
        return sort.to_owned();
    }
    let shortest = terms_of(query).iter().map(String::len).min();
    match shortest {
        Some(n) if n < TRIGRAM_MIN => "modified".into(),
        _ => sort.to_owned(),
    }
}

/// What was searched before, narrowed by what is typed now.
pub(super) fn past_matching(past: &[String], typed: &str) -> Vec<slint::SharedString> {
    let typed = typed.trim().to_lowercase();
    // What is typed narrows it; when nothing past holds it, all of it, so the
    // gesture never opens nothing.
    let narrows = past.iter().any(|line| line.to_lowercase().contains(&typed));
    past.iter()
        .filter(|line| typed.is_empty() || !narrows || line.to_lowercase().contains(&typed))
        // The cap is here rather than on the box: a layout gives its parent a
        // minimum height, so a shorter box is overflowed by its rows.
        .take(PAST_SHOWN)
        .map(|line| line.as_str().into())
        .collect()
}

/// How many past searches the box offers at once; the service keeps a hundred
/// (`scour_settings::HISTORY`).
const PAST_SHOWN: usize = 12;

/// Put one query at the front of the history: what was committed to, not every
/// keystroke searched. The cap is `scour_settings::HISTORY`.
pub(super) fn remember(state: &Rc<RefCell<State>>, link: &Rc<Link>, query: &str) {
    let query = query.trim().to_owned();
    if query.is_empty() || state.borrow().past.first() == Some(&query) {
        return;
    }
    {
        let mut s = state.borrow_mut();
        s.past.retain(|line| line != &query);
        s.past.insert(0, query.clone());
        s.past.truncate(scour_settings::HISTORY);
    }
    link.send(Ask::Remember {
        change: scour_settings::Change {
            remember: Some(query),
            ..Default::default()
        },
    });
}

/// The coloured runs to draw, for the query that is **on screen now**: sliced
/// from the text in the box, not from the text the spans arrived with. A reply
/// lands a round trip late, so anything past the query's end is dropped.
pub(super) fn painted(query: &str, spans: &[scour_core::Span]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len() + 2);
    // Characters, not bytes: the wire counts bytes, the window counts characters.
    let mut chars = 0i32;
    let add = |slice: &str, role: i32, not: bool, out: &mut Vec<Span>, chars: &mut i32| {
        if slice.is_empty() {
            return;
        }
        out.push(Span {
            at: *chars,
            text: slice.into(),
            role,
            not,
            line: 0,
            ..Span::default()
        });
        *chars += slice.chars().count() as i32;
    };
    let mut at = 0usize;
    for sp in spans {
        // An offset the query no longer reaches is a span from a longer query.
        let start = boundary(query, sp.start as usize).max(at);
        let end = boundary(query, sp.start as usize + sp.len as usize).max(start);
        if start > at {
            add(&query[at..start], 6, false, &mut out, &mut chars);
        }
        let run = &query[start..end];
        let role = match sp.role {
            scour_core::Role::Field | scour_core::Role::Cmp | scour_core::Role::Or => 1,
            scour_core::Role::Value | scour_core::Role::Phrase => 2,
            scour_core::Role::Glob => 3,
            scour_core::Role::Not => 4,
            scour_core::Role::UnknownField | scour_core::Role::BadValue => 5,
            // What is being looked for, which is not "everything else".
            scour_core::Role::Text => 6,
            // The syntax between terms, said quietly.
            // A capsule's frame, the label's colour: it is not typed into.
            scour_core::Role::Colon | scour_core::Role::Quote => 8,
            scour_core::Role::Sep | scour_core::Role::Space => 7,
            // **No catch-all**: a role added to `scour_core` stops compiling
            // here instead of silently taking the ordinary ink.
        };
        // A quoted value's quotes are the capsule's frame, drawn as its label.
        match run.strip_prefix('"') {
            Some(inner) if role == 2 && run.len() > 1 => {
                let (mid, close) = match inner.strip_suffix('"') {
                    Some(mid) => (mid, "\""),
                    None => (inner, ""),
                };
                add("\"", 8, sp.not, &mut out, &mut chars);
                add(mid, 2, sp.not, &mut out, &mut chars);
                add(close, 8, sp.not, &mut out, &mut chars);
            }
            _ => add(run, role, sp.not, &mut out, &mut chars),
        }
        at = end;
    }
    // What was typed since the spans were asked for.
    if at < query.len() {
        add(&query[at..], 6, false, &mut out, &mut chars);
    }
    out
}

/// A byte offset inside `text` that is a character boundary, at or before
/// `want`, and never past the end.
fn boundary(text: &str, want: usize) -> usize {
    let mut at = want.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The query as the engine gets it: what is in the field, the rail's terms
/// included — a press writes them there.
pub(super) fn full_query(s: &State) -> String {
    s.query.trim().to_owned()
}

/// What the rail and the ribbon are counted over: the typed query, without the
/// filter they themselves offered. Counted through it, a rail has one row.
pub(super) fn facet_query(s: &State) -> String {
    s.query.trim().to_owned()
}

/// The plain text terms of a query, for highlighting and for nothing else: a
/// `kind:` term matched a *column*, a negated term matched nothing, and quotes
/// hold a phrase together. What a term **means** stays `explain`'s answer.
/// [`scour_settings::Settings::typos`] as the settings row's value: an
/// unknown word is the offer, as the service reads it.
/// A term put in the query as the menu's searches do: in place of the one of
/// the same field, the rest kept, shown in the field and searched.
pub(super) fn put_term(w: &MainWindow, term: &str) {
    let text = scour_ui::query::toggle(&w.get_query(), term);
    w.set_tab("search".into());
    w.set_query(text.as_str().into());
    w.invoke_query_changed(text.into());
    w.invoke_caret_to_end();
}

/// The status bar's word for how every word is read.
pub(super) fn typos_said(cat: &Catalogue, choice: &str) -> String {
    let how = match choice {
        "always" => "always",
        "off" => "off",
        _ => "offer it",
    };
    t(cat, "Similar spelling: {how}").replace("{how}", &t(cat, how))
}

pub(super) fn typos_choice(kept: &str) -> &'static str {
    match kept {
        "off" => "off",
        "always" => "always",
        _ => "",
    }
}

pub(super) fn terms_of(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let push = |w: &mut String, quoted: bool, out: &mut Vec<String>| {
        let t = std::mem::take(w);
        if t.is_empty() || (!quoted && t.starts_with('!')) {
            return;
        }
        if !quoted && let Some((f, v)) = t.split_once(':') {
            let fl = f.to_lowercase();
            let v = v.trim_matches('"').trim_matches('*');
            if !v.is_empty() {
                if matches!(fl.as_str(), "fuzzy" | "typo" | "benzer") {
                    out.push(format!("~{v}"));
                } else if matches!(fl.as_str(), "letters" | "harfler") {
                    out.push(format!("^{v}"));
                }
            }
            return;
        }
        let t = t.trim_matches('*');
        if !t.is_empty() {
            out.push(t.to_owned());
        }
    };
    for ch in query.chars() {
        match ch {
            '"' => {
                push(&mut word, quoted, &mut out);
                quoted = !quoted;
            }
            c if c.is_whitespace() && !quoted => push(&mut word, quoted, &mut out),
            c => word.push(c),
        }
    }
    push(&mut word, quoted, &mut out);
    out
}
