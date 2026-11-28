//! The query bar wired to the window: what Rust draws for it, what each press
//! and key does, and what comes back from the service for the lens. The
//! geometry is `qbar`, the lens `qlens`; this is where they meet `State`.

use std::cell::RefCell;
use std::rc::Rc;

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_proto::Response;
use scour_ui::{frame, lens};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::link::{Ask, Link};
use crate::qbar::{self, Field};
use crate::qlens::{self, Chosen, Env, Lenses};
use crate::{LensRow, MainWindow, Reach, State};

/// Everything the bar keeps between presses.
#[derive(Default)]
pub struct QueryBar {
    pub field: Field,
    pub lenses: Lenses,
    /// This desktop's folders, label and path, from `Places`.
    pub places: Vec<(String, String)>,
    pub anchor: usize,
    pub cursor: usize,
    /// The term the pointer was last on: its buttons act on it.
    pub(crate) hover: Option<lens::Term>,
    /// Each filter of a query that found nothing: the term, the query
    /// without it, and what that finds once counted.
    pub(crate) rescue: Vec<(String, String, Option<(u64, bool)>)>,
    pub(crate) rescue_for: String,
    /// The lens generation the rescue's counts were asked with.
    rescue_gen: u64,
    /// The list's query, its total, and whether that total is exact.
    pub(crate) listed: Option<(String, u64, bool)>,
    /// The pill's effect counts.
    pub(crate) effect: crate::qeffect::Effect,
    /// The query with a misspelt field name put right, when there is one.
    fix: Option<String>,
    /// The narrowing line: the query it is for, its answers, and its chips
    /// as label, count and the term a press writes.
    pub narrow_for: String,
    /// The query's revision its asks carry: an answer for an older one is
    /// dropped, so the same text under a newer revision is asked again.
    narrow_revision: u64,
    narrow_total: u64,
    /// Moved by each new ask and each hiding: a wait still running for an
    /// older one sends nothing.
    narrow_gen: u64,
    /// The line switched off in the settings: nothing is asked for it.
    pub narrow_off: bool,
    pub narrow_pinned: bool,
    pub narrow_hidden: bool,
    narrow_facets: Option<scour_core::FacetResponse>,
    narrow_usage: Option<scour_core::UsageResponse>,
    /// Group, label, count and the term a press writes.
    chips: Vec<(&'static str, String, u64, String)>,
    /// How many files each size band would leave, as asked.
    narrow_sizes: [Option<u64>; 4],
    /// How many months the time lens's calendar is moved back from today's.
    pub(crate) cal_back: i64,
    /// How wide the window draws each word a capsule shows in its own face —
    /// a header's name, a value said in words — and the words it was asked to
    /// measure, in the order it was asked.
    pub(crate) widths: std::collections::HashMap<(String, i32), f32>,
    pub(crate) asked: Vec<(String, i32)>,
    /// The place groups drawn as one capsule: their bytes, and where; and
    /// each as folded, for the arrow keys.
    pub groups: Vec<(usize, usize, qbar::Laid)>,
    pub caps: Vec<lens::PlaceCap>,
    /// The text `write` set, so its own `query-changed` is not read as typing.
    pub(crate) wrote: Option<String>,
    /// The last `volumes` and `sources` answers: the scope chip's disks.
    pub disks: Vec<scour_proto::Disk>,
    pub sources: Vec<scour_core::SourceInfo>,
    /// What the chip shows now, so an unchanged one is not set again.
    pub chip: crate::qscope::Chip,
}

impl QueryBar {
    /// A bar, its narrowing line switched off or not.
    pub fn off(narrow_off: bool) -> QueryBar {
        QueryBar {
            narrow_off,
            ..QueryBar::default()
        }
    }
}

/// Whether drawing also opens, follows or leaves the lens.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lensing {
    Keep,
    Follow,
    Asked,
}

/// Slots above this are the rescue's counts, not the lens's.
const RESCUE: u32 = 1000;

pub(crate) fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// `under:` for a folder, as a person writes it: under home with `~`, quoted
/// when it has a space.
pub fn place_term(path: &str) -> String {
    let home = home();
    let shown = match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_string(),
    };
    lens::place(&lens::terms("under:x")[0], &shown)
}

fn with_env<R>(cat: &Catalogue, places: &[(String, String)], f: impl FnOnce(&Env) -> R) -> R {
    let tr = |m: &str| cat.get(m).into_owned();
    let home = home();
    f(&Env {
        tr: &tr,
        home: &home,
        places,
    })
}

fn send(link: &Link, generation: u64, wants: Vec<(u32, scour_proto::Request)>) {
    for (slot, request) in wants {
        link.send(Ask::Lens {
            generation,
            slot,
            request,
        });
    }
}

/// The window measured one of the words it was asked to. True when that
/// moves something.
pub fn measured(s: &mut State, i: usize, w: f32) -> bool {
    let Some(key) = s.qb.asked.get(i).cloned() else {
        return false;
    };
    if w <= 0.0
        || s.qb
            .widths
            .get(&key)
            .is_some_and(|old| (old - w).abs() < 0.05)
    {
        return false;
    }
    s.qb.widths.insert(key, w);
    true
}

/// Lay the query out and draw it: the runs, the capsules, the caret, the
/// selection, the field's height; and the lens, as `lensing` says.
pub fn draw(w: &MainWindow, s: &mut State, link: &Link, cat: &Catalogue, lensing: Lensing) {
    let text = w.get_query().to_string();
    let qb = &mut s.qb;
    let asked_before = qb.asked.len();
    let crate::qlay::Laidout {
        mut caps,
        runs,
        bits,
        frames,
    } = crate::qlay::lay_out(w, qb, cat, &text, &s.spans);
    if let Some(h) = &qb.hover {
        let (a, b) = (
            qbar::chars_before(&text, h.start),
            qbar::chars_before(&text, h.end),
        );
        for c in &mut caps {
            let l = qb.field.lines[c.line as usize];
            let (from, to) = (l.start + c.at as usize, l.start + (c.at + c.len) as usize);
            c.hov = from < b && to > a && w.get_hov_on();
        }
    }
    let (line, col, sels) = qb.field.caret(&text, qb.anchor, qb.cursor);
    if qb.asked.len() != asked_before {
        let rows: Vec<crate::QMeasure> = qb
            .asked
            .iter()
            .map(|(text, face)| crate::QMeasure {
                text: text.as_str().into(),
                face: *face,
            })
            .collect();
        w.set_q_measure(ModelRc::new(VecModel::from(rows)));
    }
    let field = &qb.field;
    // The caret at the end of the filter being written stands after its last
    // letter, not after the room the well keeps open.
    let caret_x = field.x(line.max(0) as usize, col)
        - caps
            .iter()
            .find(|c| c.edit && c.line == line && c.at + c.len == col)
            .map_or(0.0, |c| c.tail - 2.0);
    // Just before a filter, the caret stands in the gap before it, not on its
    // frame's left edge.
    let caret_x = if caps.iter().any(|c| c.line == line && c.at == col) {
        // At the start of the line too, into the field's own margin: the
        // filter's frame begins 3 px before its first letter.
        caret_x - if col == 0 { 6.0 } else { 4.0 }
    } else {
        caret_x
    };
    let mut sels = sels;
    for sl in &mut sels {
        let l = sl.line as usize;
        sl.x = field.x(l, sl.at);
        sl.w = (field.x(l, sl.at + sl.len) - sl.x).max(0.0);
    }
    let caret_x = crate::qlay::caret_x(qb, &text, qb.cursor).unwrap_or(caret_x);
    w.set_caret_x(caret_x);
    // The caret in the term the pointer is on: it is being written.
    let writing = qb.hover.as_ref().is_some_and(|h| {
        qb.anchor == qb.cursor && qb.cursor > h.start && qb.cursor <= h.end && w.get_q_focused()
    });
    w.set_hov_edit(writing);
    w.set_spans(ModelRc::new(VecModel::from(runs)));
    w.set_caps(ModelRc::new(VecModel::from(caps)));
    w.set_q_bits(ModelRc::new(VecModel::from(bits)));
    w.set_q_frames(ModelRc::new(VecModel::from(frames)));
    w.set_sels(ModelRc::new(VecModel::from(sels)));
    w.set_q_shown(qb.field.shown() as i32);
    w.set_q_first(qb.field.first as i32);
    w.set_caret_line(line);
    w.set_caret_col(col);
    let ts = lens::terms(&text);
    w.set_loc_on(
        ts.iter()
            .any(|t| matches!(t.field, Some("under" | "parent"))),
    );
    w.set_time_on(ts.iter().any(|t| t.field == Some("dm")));
    w.set_fuzzy_on(
        ts.iter()
            .any(|t| matches!(t.field, Some("fuzzy" | "letters")))
            || w.get_typos_choice() == "always",
    );
    if lensing != Lensing::Keep {
        let focused = qb.anchor == qb.cursor;
        let wants = if focused {
            with_env(cat, &qb.places, |env| {
                qb.lenses
                    .sync(&text, qb.cursor, lensing == Lensing::Asked, env)
            })
        } else {
            qb.lenses.close();
            Vec::new()
        };
        send(link, qb.lenses.generation, wants);
    }
    show_lens(w, &mut s.qb, cat);
    crate::qscope::draw(w, &mut s.qb, cat, &text);
}

/// The open lens, as rows and words.
fn show_lens(w: &MainWindow, qb: &mut QueryBar, cat: &Catalogue) {
    let text = w.get_query().to_string();
    let Some(o) = qb
        .lenses
        .open
        .as_ref()
        .filter(|o| o.kind == lens::Lens::Place || !o.items.is_empty())
    else {
        w.set_lens_open(false);
        return;
    };
    let kind_tag = match o.kind {
        lens::Lens::Place => "place",
        lens::Lens::Time => "time",
        lens::Lens::Size => "size",
        lens::Lens::Word => "word",
        lens::Lens::Kind | lens::Lens::Ext => "type",
        _ => "",
    };
    w.set_lens_kind(kind_tag.into());
    if o.kind == lens::Lens::Place {
        crate::placewire::show(w, o, cat, &text);
    }
    if !kind_tag.is_empty() && kind_tag != "word" && kind_tag != "place" {
        crate::panelwire::show(w, qb, cat, &text);
    }
    let Some(o) = qb.lenses.open.as_ref() else {
        return;
    };
    let peak = o
        .items
        .iter()
        .filter_map(|i| i.n)
        .map(i64::unsigned_abs)
        .max()
        .unwrap_or(0)
        .max(1);
    let rows: Vec<LensRow> = o
        .items
        .iter()
        .map(|i| LensRow {
            label: i.label.as_str().into(),
            hint: i.hint.as_str().into(),
            group: i.group.as_str().into(),
            n: match i.n {
                Some(n) if n < 0 => format!("{}+", crate::grouped(n.unsigned_abs())).into(),
                Some(n) => crate::grouped(n as u64).into(),
                None if i.count => "…".into(),
                None => slint::SharedString::new(),
            },
            bar: i.n.map_or(0.0, |n| n.unsigned_abs() as f32 / peak as f32),
            zero: i.n == Some(0),
            wait: i.wait,
            reads: i.reads,
            stays: i.stays || i.dir.is_some(),
            icon: i.icon.into(),
            key: i.key.as_str().into(),
            held: i.held,
        })
        .collect();
    let counted = o.items.iter().any(|i| i.count || i.n.is_some());
    // A member of a place group: the lens hangs from the whole capsule.
    let from = qb
        .groups
        .iter()
        .find(|g| g.0 <= o.a && o.a <= g.1)
        .map_or(o.a, |g| g.0);
    let (line, col) = qb.field.start_of(&text, from);
    w.set_lens_rows(ModelRc::new(VecModel::from(rows)));
    w.set_lens_sel(o.sel as i32);
    w.set_lens_title(o.title.as_str().into());
    w.set_lens_foot(o.foot.as_str().into());
    w.set_lens_said(if counted {
        cat.get("results it would leave").as_ref().into()
    } else {
        slint::SharedString::new()
    });
    w.set_lens_reads(cat.get("reads files").as_ref().into());
    w.set_lens_held(cat.get("in the query").as_ref().into());
    let end_line = qb.field.start_of(&text, o.b.max(1) - 1).0;
    w.set_lens_line(end_line.max(line));
    w.set_lens_col(col);
    w.set_lens_x(qb.field.x(qb.field.first + line.max(0) as usize, col));
    w.set_lens_launching(o.kind == lens::Lens::Launch);
    w.set_lens_open(true);
}

/// Put a new query in the field, search it, and put the caret at `caret`.
pub(crate) fn write(w: &MainWindow, s: &Rc<RefCell<State>>, query: String, caret: usize) {
    {
        let mut st = s.borrow_mut();
        st.qb.anchor = caret;
        st.qb.cursor = caret;
        st.qb.hover = None;
        st.qb.wrote = Some(query.clone());
        st.qb.lenses.editing = false;
    }
    w.set_hov_on(false);
    w.set_query(query.as_str().into());
    w.invoke_query_changed(query.into());
    w.set_anchor_to(-1);
    w.set_caret_to(caret as i32);
    w.set_caret_ask(w.get_caret_ask() + 1);
}

/// An answer for the lens or for the rescue.
pub fn answered(
    w: &MainWindow,
    s: &mut State,
    link: &Link,
    cat: &Catalogue,
    generation: u64,
    slot: u32,
    reply: Response,
) {
    if slot >= crate::qeffect::EFFECT {
        crate::qeffect::answered(w, s, cat, generation, slot, &reply);
        return;
    }
    if slot >= RESCUE {
        // Only for the query it was asked for: a late one for an older
        // query must not land in this list.
        let asked = (s.qb.rescue_gen, s.qb.rescue_for.as_str());
        if !s.qb.rescue.is_empty() && crate::qeffect::current(asked, generation, &s.query) {
            rescue_answer(w, s, cat, slot - RESCUE, &reply);
            // The capsules it marks.
            draw(w, s, link, cat, Lensing::Keep);
        }
        return;
    }
    let text = w.get_query().to_string();
    let qb = &mut s.qb;
    match &reply {
        Response::Volumes { disks, .. } => qb.disks = disks.clone(),
        Response::Sources { sources } => qb.sources = sources.clone(),
        _ => {}
    }
    let wants = with_env(cat, &qb.places, |env| {
        qb.lenses.answer(generation, slot, reply, &text, env)
    });
    send(link, qb.lenses.generation, wants);
    show_lens(w, &mut s.qb, cat);
}

/// The open lens asks again for what it shows: a panel changed its scale.
pub fn refilled(w: &MainWindow, s: &mut State, link: &Link, cat: &Catalogue) {
    let text = w.get_query().to_string();
    let qb = &mut s.qb;
    let wants = with_env(cat, &qb.places, |env| qb.lenses.refill(&text, env));
    send(link, qb.lenses.generation, wants);
    show_lens(w, &mut s.qb, cat);
}

/// A search came back. With nothing found, ask what each filter left out
/// would find; with something, take the offers away.
pub fn searched(
    w: &MainWindow,
    s: &mut State,
    link: &Link,
    cat: &Catalogue,
    total: u64,
    capped: bool,
) {
    let text = s.query.clone();
    s.qb.listed = Some((text.clone(), total, !capped));
    if total > 0 || text.trim().is_empty() || lens::reads(&text) {
        s.qb.rescue.clear();
        s.qb.rescue_for.clear();
        show_rescue(w, &s.qb, cat);
        return;
    }
    if s.qb.rescue_for == text {
        return;
    }
    s.qb.rescue_for = text.clone();
    s.qb.rescue = qbar_filters(&text, &s.spans)
        .into_iter()
        .take(4)
        .map(|t| {
            (
                text[t.start..t.end].to_string(),
                lens::without(&text, &t),
                None,
            )
        })
        .collect();
    let generation = s.qb.lenses.generation;
    s.qb.rescue_gen = generation;
    for (i, (_, without, _)) in s.qb.rescue.iter().enumerate() {
        link.send(Ask::Lens {
            generation,
            slot: RESCUE + i as u32,
            request: scour_proto::Request::Count {
                query: without.clone(),
                cap: 100_000,
            },
        });
    }
    show_rescue(w, &s.qb, cat);
}

/// The terms of a query drawn as capsules: what the rescue may leave out.
fn qbar_filters(text: &str, read: &[scour_core::Span]) -> Vec<lens::Term> {
    lens::terms(text)
        .into_iter()
        .filter(|t| {
            let inside = |sp: &&scour_core::Span| {
                (sp.start as usize) < t.end && (sp.start + sp.len) as usize > t.start
            };
            let named = read
                .iter()
                .filter(inside)
                .any(|sp| sp.role == scour_core::Role::Field);
            let wrong = read.iter().filter(inside).any(|sp| sp.role.is_warning());
            // A field still being typed (`dm:`) is not a filter yet.
            !wrong && !t.value.is_empty() && (named || t.field.is_some())
        })
        .collect()
}

fn rescue_answer(w: &MainWindow, s: &mut State, cat: &Catalogue, i: u32, reply: &Response) {
    let Response::Count { total, capped, .. } = reply else {
        return;
    };
    if let Some(r) = s.qb.rescue.get_mut(i as usize) {
        r.2 = Some((*total, *capped));
    }
    show_rescue(w, &s.qb, cat);
}

fn show_rescue(w: &MainWindow, qb: &QueryBar, cat: &Catalogue) {
    let lines: Vec<slint::SharedString> = qb
        .rescue
        .iter()
        .filter_map(|(term, _, n)| {
            let (n, capped) = (*n)?;
            (n > 0).then(|| {
                let count = format!("{}{}", crate::grouped(n), if capped { "+" } else { "" });
                format!(
                    "{term}  ·  {}",
                    cat.get("without it: {n} results").replace("{n}", &count)
                )
                .into()
            })
        })
        .collect();
    w.set_offers_said(if lines.is_empty() {
        slint::SharedString::new()
    } else {
        cat.get("Left out, one of these would find something:")
            .as_ref()
            .into()
    });
    w.set_offers(ModelRc::new(VecModel::from(lines)));
}

/// A first page landed: ask what the narrowing line shows for this query —
/// one `facets` walk and one `usage` under home. Nothing for a query that
/// reads files, or that found fewer than two.
///
/// **Once a query**, as the page does it. Asked again whenever the total moved,
/// the empty query — whose total is every live row, and moves with every file
/// written anywhere — had the two walks re-run behind each refresh: 1.3 s of
/// the service's CPU and 280 MB of its memory, for chips nobody had touched.
/// Only crossing the two the line needs asks again.
///
/// **After [`NARROW_IDLE_MS`] of quiet**, as the page's `narrowSoon`: the walks
/// cost the service 60–520 ms a query, and asked on every key they were 74 % of
/// a core while typing, against 3.5 % without them (measured 2026-11-27).
pub fn narrow(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    cat: &Catalogue,
    total: u64,
) {
    let Some(asks) = narrow_asks(w, &mut state.borrow_mut(), cat, total) else {
        return;
    };
    if asks.is_empty() {
        return;
    }
    let (mine, revision) = {
        let s = state.borrow();
        (s.qb.narrow_gen, s.query_revision)
    };
    let (state, link) = (Rc::clone(state), Rc::clone(link));
    slint::Timer::single_shot(
        std::time::Duration::from_millis(NARROW_IDLE_MS),
        move || {
            let s = state.borrow();
            if s.qb.narrow_gen != mine || s.query_revision != revision {
                return;
            }
            drop(s);
            for ask in asks {
                link.send(ask);
            }
        },
    );
}

/// How long the query has to stay as it is before the narrowing line asks.
const NARROW_IDLE_MS: u64 = 250;

/// The line put back for the query in sight, and what it is to ask, or
/// `None` when it is already asked for it.
fn narrow_asks(w: &MainWindow, s: &mut State, cat: &Catalogue, total: u64) -> Option<Vec<Ask>> {
    // The empty query too: the line is always there under the bar, and its
    // two walks are 180 ms and 300 ms once a window opens (MEASUREMENTS,
    // 2026-11-22), not the second they were.
    if s.qb.narrow_off || s.qb.narrow_hidden {
        s.qb.narrow_gen += 1;
        if !s.qb.chips.is_empty() {
            s.qb.chips.clear();
            show_chips(w, &s.qb, cat);
        }
        w.set_narrow_shown(false);
        return Some(Vec::new());
    }
    w.set_narrow_shown(true);
    let text = s.query.clone();
    // The same text again is not enough: the query asked anew moves its
    // revision, and the answers to the last asks are dropped for it. Without
    // the revision the line asked once, lost its answers, and stayed empty —
    // a window that opened quickly enough re-sent its first query inside the
    // 250 ms wait (2026-11-28, Slint 1.19).
    if s.qb.narrow_for == text
        && s.qb.narrow_revision == s.query_revision
        && (s.qb.narrow_total < 2) == (total < 2)
    {
        return None;
    }
    s.qb.narrow_gen += 1;
    s.qb.narrow_for = text.clone();
    s.qb.narrow_revision = s.query_revision;
    s.qb.narrow_total = total;
    s.qb.narrow_facets = None;
    s.qb.narrow_usage = None;
    s.qb.narrow_sizes = [None; 4];
    s.qb.chips.clear();
    show_chips(w, &s.qb, cat);
    let ts = lens::terms(&text);
    let mut asks = Vec::new();
    if total < 2 || lens::reads(&text) {
        return Some(asks);
    }
    let revision = s.query_revision;
    asks.push(Ask::Narrow {
        query_revision: revision,
        slot: 0,
        request: scour_proto::Request::Facets {
            query: text.clone(),
            by: vec![
                scour_core::FacetBy::Kind,
                scour_core::FacetBy::Ext { top: 6 },
                scour_core::FacetBy::Age {
                    edges: vec![1, 7, 30, 365],
                },
            ],
        },
    });
    // What each size band would leave: one count apiece, asked beside the rest.
    if !ts.iter().any(|t| t.field == Some("size") && !t.not) {
        for (i, (term, _)) in SIZE_CHIPS.iter().enumerate() {
            asks.push(Ask::Narrow {
                query_revision: revision,
                slot: 2 + i as u32,
                request: scour_proto::Request::Count {
                    query: format!("{text} size:{term}"),
                    cap: 1_000_000,
                },
            });
        }
    }
    let home = home();
    if !home.is_empty()
        && !ts
            .iter()
            .any(|t| matches!(t.field, Some("under" | "parent")))
    {
        asks.push(Ask::Narrow {
            query_revision: revision,
            slot: 1,
            // Every child, not the top eight: the answer ranks them by bytes
            // and the line picks the two with the most files, which the
            // largest eight need not hold.
            request: scour_proto::Request::Usage {
                path: home,
                top: u32::MAX,
                query: text,
            },
        });
    }
    Some(asks)
}

/// The size bands a chip offers, as the term and the msgid that names them.
const SIZE_CHIPS: [(&str, &str); 4] = [
    ("<100kb", "under 100 KB"),
    (">1mb", "over 1 MB"),
    (">100mb", "over 100 MB"),
    (">1gb", "over 1 GB"),
];

/// An answer for the narrowing line.
pub fn narrowed(
    w: &MainWindow,
    s: &mut State,
    cat: &Catalogue,
    revision: u64,
    slot: u32,
    reply: Response,
) {
    if revision != s.query_revision {
        return;
    }
    match reply {
        Response::Facets(f) => s.qb.narrow_facets = Some(f),
        Response::Usage(u) => s.qb.narrow_usage = Some(u),
        // A count that stopped at its cap is not a number to print: the band
        // is left out rather than offered as "1.000.000".
        Response::Count { total, capped, .. } if (2..6).contains(&slot) => {
            s.qb.narrow_sizes[(slot - 2) as usize] = Some(if capped { 0 } else { total });
        }
        _ => return,
    }
    let text = s.qb.narrow_for.clone();
    let ts = lens::terms(&text);
    let has = |f: &str| ts.iter().any(|t| t.field == Some(f) && !t.not);
    let all =
        s.qb.narrow_facets
            .as_ref()
            .map_or(s.qb.narrow_total, |f| f.total);
    let mut chips: Vec<(&'static str, String, u64, String)> = Vec::new();
    let group = |by: fn(&scour_core::FacetBy) -> bool| {
        s.qb.narrow_facets
            .as_ref()
            .and_then(|f| f.groups.iter().find(|g| by(&g.by)))
            .map(|g| g.facets.clone())
            .unwrap_or_default()
    };
    if !has("kind") {
        for x in group(|b| matches!(b, scour_core::FacetBy::Kind))
            .into_iter()
            .take(5)
        {
            if x.count > 0 && x.count < all {
                let name = scour_core::Kind::from_name(&x.key)
                    .and_then(|k| k.first())
                    .map_or_else(|| x.key.clone(), |k| cat.get(k.msgid()).into_owned());
                chips.push(("kind", name, x.count, format!("kind:{}", x.key)));
            }
        }
    }
    if !has("ext") {
        for x in group(|b| matches!(b, scour_core::FacetBy::Ext { .. }))
            .into_iter()
            .take(6)
        {
            if !x.key.is_empty() && x.count > 0 && x.count < all {
                chips.push((
                    "ext",
                    format!(".{}", x.key),
                    x.count,
                    format!("ext:{}", x.key),
                ));
            }
        }
    }
    if !has("dm") {
        // The bands are apart (a file is in the first it is not older than),
        // and a chip says "within": each one counts the ones before it too.
        let bands = group(|b| matches!(b, scour_core::FacetBy::Age { .. }));
        let mut within = 0;
        for (key, label, term) in [
            ("1", "last 24 hours", "dm:24h"),
            ("7", "last 7 days", "dm:7d"),
            ("30", "last 30 days", "dm:30d"),
            ("365", "last year", "dm:1y"),
        ] {
            within += bands.iter().find(|x| x.key == key).map_or(0, |x| x.count);
            if within > 0 && within < all {
                chips.push(("dm", cat.get(label).into_owned(), within, term.into()));
            }
        }
    }
    if !has("size") {
        for (i, (term, label)) in SIZE_CHIPS.iter().enumerate() {
            if let Some(n) = s.qb.narrow_sizes[i]
                && n > 0
                && n < all
            {
                chips.push((
                    "size",
                    cat.get(label).into_owned(),
                    n,
                    format!("size:{term}"),
                ));
            }
        }
    }
    if let Some(u) = &s.qb.narrow_usage {
        let home = home();
        let mut kids: Vec<_> = u
            .children
            .iter()
            .filter(|c| c.files > 0 && c.files < all)
            .collect();
        kids.sort_by_key(|c| std::cmp::Reverse(c.files));
        for c in kids.into_iter().take(4) {
            let shown = match c.path.strip_prefix(&home) {
                Some(rest) if !home.is_empty() => format!("~{rest}"),
                _ => c.path.clone(),
            };
            let term = lens::place(&lens::terms("under:x")[0], &shown);
            chips.push(("under", shown, c.files, term));
        }
    }
    s.qb.chips = chips;
    show_chips(w, &s.qb, cat);
}

fn show_chips(w: &MainWindow, qb: &QueryBar, _cat: &Catalogue) {
    let chips: Vec<crate::Facet> = qb
        .chips
        .iter()
        .map(|(group, label, n, term)| crate::Facet {
            label: label.as_str().into(),
            token: term.as_str().into(),
            count: crate::grouped(*n).into(),
            share: 0.0,
            // The group it belongs to: the line draws a picture where one begins.
            icon: (*group).into(),
        })
        .collect();
    w.set_chips(ModelRc::new(VecModel::from(chips)));
}

/// A press on a chip: the term goes in, in place of one of the same field;
/// with Alt held it goes in excluded.
fn take_chip(w: &MainWindow, s: &Rc<RefCell<State>>, term: &str, alt: bool) {
    let text = w.get_query().to_string();
    let field = &term[..term.find(':').map_or(0, |i| i + 1)];
    let same = if alt {
        None
    } else {
        lens::terms(&text)
            .into_iter()
            .find(|t| !t.not && text[t.start..t.end].to_lowercase().starts_with(field))
    };
    let term = if alt {
        format!("!{term}")
    } else {
        term.to_string()
    };
    let query = match same {
        Some(t) => lens::replace(&text, &t, &term).0,
        None if text.trim().is_empty() => term,
        None => format!("{} {term}", text.trim_end()),
    };
    let n = query.len();
    write(w, s, query, n);
}

/// The query was read back: offer the spelling a misspelt field meant.
pub fn explained(w: &MainWindow, s: &mut State, cat: &Catalogue) {
    let text = s.query.clone();
    s.qb.fix = None;
    w.set_fix_said(slint::SharedString::new());
    for t in lens::terms(&text) {
        let raw = &text[t.start..t.end];
        let bare = raw.strip_prefix('!').unwrap_or(raw);
        let Some((name, value)) = bare.split_once(':') else {
            continue;
        };
        let unknown = s.spans.iter().any(|sp| {
            sp.role == scour_core::Role::UnknownField
                && (sp.start as usize) >= t.start
                && (sp.start as usize) < t.end
        });
        if !unknown {
            continue;
        }
        let Some(near) = lens::near_field(name) else {
            continue;
        };
        let neg = if raw.starts_with('!') { "!" } else { "" };
        let fixed = format!("{}{neg}{near}:{value}{}", &text[..t.start], &text[t.end..]);
        w.set_fix_said(
            cat.get("mean {f}?")
                .replace("{f}", &format!("{near}:"))
                .into(),
        );
        s.qb.fix = Some(fixed);
        return;
    }
}

/// A filter with nothing after its colon, which the caret `at` is not in.
fn empty_filter(text: &str, at: Option<usize>) -> Option<lens::Term> {
    lens::terms(text).into_iter().find(|t| {
        t.field.is_some()
            && t.value.trim().is_empty()
            && at.is_none_or(|c| !(c > t.start && c <= t.end))
    })
}

/// A filter with nothing in it does not stay behind: leaving it takes it
/// out, the caret kept where it was in what remains. True when one went.
fn drop_empty(w: &MainWindow, s: &Rc<RefCell<State>>, staying_in: Option<usize>) -> bool {
    let text = w.get_query().to_string();
    let Some(t) = empty_filter(&text, staying_in) else {
        return false;
    };
    let cur = s.borrow().qb.cursor.min(text.len());
    let (query, _) = lens::remove_term(&text, &t);
    let gone = text.len() - query.len();
    let caret = if cur > t.end {
        cur - gone
    } else {
        cur.min(t.start)
    };
    s.borrow_mut().qb.lenses.close();
    w.set_lens_open(false);
    write(w, s, query, caret.min(text.len() - gone));
    true
}

/// Leave the term the caret is at the end of: the lens shuts and a space
/// follows the term, so the caret stands outside it. True when there was a
/// term or a lens to leave.
fn exit(w: &MainWindow, s: &Rc<RefCell<State>>) -> bool {
    let had = w.get_lens_open();
    s.borrow_mut().qb.lenses.close();
    w.set_lens_open(false);
    let text = w.get_query().to_string();
    let (anchor, c) = {
        let st = s.borrow();
        (st.qb.anchor, st.qb.cursor.min(text.len()))
    };
    if anchor != c {
        return had;
    }
    let Some(t) = lens::term_at(&text, c) else {
        return had;
    };
    let filter = t.not || t.field.is_some() || text[t.start..t.end].contains(':');
    if c != t.end || !filter {
        return had;
    }
    // Left with nothing in it, a filter goes rather than stays empty.
    if t.field.is_some() && t.value.trim().is_empty() {
        return drop_empty(w, s, None);
    }
    let query = if text[c..].starts_with(char::is_whitespace) {
        text.clone()
    } else {
        format!("{} {}", &text[..c], &text[c..])
    };
    write(w, s, query, c + 1);
    s.borrow_mut().qb.lenses.close();
    w.set_lens_open(false);
    true
}

fn known_field(spans: &[scour_core::Span], t: &lens::Term) -> bool {
    let inside = |sp: &&scour_core::Span| {
        (sp.start as usize) < t.end && (sp.start + sp.len) as usize > t.start
    };
    let unknown = spans
        .iter()
        .filter(inside)
        .any(|sp| sp.role == scour_core::Role::UnknownField);
    !unknown
        && (t.field.is_some()
            || spans
                .iter()
                .filter(inside)
                .any(|sp| sp.role == scour_core::Role::Field))
}

fn apply_frame_edit(w: &MainWindow, s: &Rc<RefCell<State>>, next: (String, usize)) -> bool {
    let current = w.get_query().to_string();
    let (query, caret) = next;
    if query == current {
        s.borrow_mut().qb.anchor = caret;
        s.borrow_mut().qb.cursor = caret;
        w.set_anchor_to(-1);
        w.set_caret_to(caret as i32);
        w.set_caret_ask(w.get_caret_ask() + 1);
    } else {
        if query.len() < current.len() {
            s.borrow_mut().qb.lenses.close();
            w.set_lens_open(false);
        }
        write(w, s, query, caret);
    }
    true
}

/// Backspace or Delete at a part of a place group drawn whole: the part goes.
fn group_key(w: &MainWindow, s: &Rc<RefCell<State>>, back: bool) -> bool {
    let text = w.get_query().to_string();
    let next = {
        let st = s.borrow();
        let c = st.qb.cursor.min(text.len());
        if st.qb.anchor != c || !st.qb.groups.iter().any(|g| g.0 <= c && c <= g.1) {
            return false;
        }
        let tiles = crate::qplace::tiles(&st.qb.disks, &st.qb.sources);
        let caret = Some(lens::Caret {
            at: c,
            editing: st.qb.lenses.editing,
        });
        let mut caps = lens::place_caps_at(&text, &home(), &tiles, caret);
        caps.extend(lens::exclusion_caps(&text, &home(), &tiles, caret));
        if back {
            crate::qgroup::backspace(&text, &home(), &caps, c)
        } else {
            crate::qgroup::delete(&text, &home(), &caps, c)
        }
    };
    next.is_some_and(|next| apply_frame_edit(w, s, next))
}

fn frame_backspace(w: &MainWindow, s: &Rc<RefCell<State>>) -> bool {
    if group_key(w, s, true) {
        return true;
    }
    let text = w.get_query().to_string();
    let (anchor, c, spans) = {
        let st = s.borrow();
        (st.qb.anchor, st.qb.cursor.min(text.len()), st.spans.clone())
    };
    if anchor != c {
        return false;
    }
    frame::backspace(&text, c, |t| known_field(&spans, t))
        .is_some_and(|next| apply_frame_edit(w, s, next))
}

fn frame_delete(w: &MainWindow, s: &Rc<RefCell<State>>) -> bool {
    if group_key(w, s, false) {
        return true;
    }
    let text = w.get_query().to_string();
    let (anchor, c, spans) = {
        let st = s.borrow();
        (st.qb.anchor, st.qb.cursor.min(text.len()), st.spans.clone())
    };
    if anchor != c {
        return false;
    }
    frame::delete_forward(&text, c, |t| known_field(&spans, t))
        .is_some_and(|next| apply_frame_edit(w, s, next))
}

fn frame_before_char(w: &MainWindow, s: &Rc<RefCell<State>>, ch: &str) -> bool {
    let text = w.get_query().to_string();
    let (anchor, c, spans) = {
        let st = s.borrow();
        (st.qb.anchor, st.qb.cursor.min(text.len()), st.spans.clone())
    };
    if anchor != c {
        return false;
    }
    frame::insert_before(&text, c, ch, |t| known_field(&spans, t))
        .is_some_and(|next| apply_frame_edit(w, s, next))
}

/// The launcher shut without a choice: a lone `:` it left goes with it.
fn drop_launcher(w: &MainWindow, s: &Rc<RefCell<State>>) {
    s.borrow_mut().qb.lenses.close();
    w.set_lens_open(false);
    let text = w.get_query().to_string();
    if let Some(t) = lens::terms(&text)
        .into_iter()
        .find(|t| &text[t.start..t.end] == ":")
    {
        let (query, caret) = lens::replace(&text, &t, "");
        write(w, s, query, caret);
    }
}

/// Take a term out, exclude it or take the exclusion off, or read a word a
/// letter off: what a term's buttons do.
fn act(w: &MainWindow, state: &Rc<RefCell<State>>, t: &lens::Term, what: &str) {
    let text = w.get_query().to_string();
    if t.end > text.len() {
        return;
    }
    if crate::qcaret::group_pill(w, state, &text, t, what) {
        return;
    }
    let raw = &text[t.start..t.end];
    let body = raw.strip_prefix('!').unwrap_or(raw);
    let neg = if t.not { "!" } else { "" };
    let with = match what {
        "drop" => String::new(),
        "fz" if t.field == Some("fuzzy") => {
            format!("{neg}{}", &body[body.find(':').map_or(0, |i| i + 1)..])
        }
        "fz" => format!("{neg}fuzzy:{body}"),
        _ if t.not => body.to_string(),
        _ => format!("!{raw}"),
    };
    let (query, caret) = lens::replace(&text, t, &with);
    write(w, state, query, caret);
    // Kept under the pointer, so a second press undoes the first in place.
    if !with.is_empty()
        && let Some(t) = lens::term_at(&w.get_query(), caret)
    {
        state.borrow_mut().qb.hover = Some(t);
        w.set_hov_not(with.starts_with('!'));
        w.set_hov_fz_on(with.trim_start_matches('!').starts_with("fuzzy:"));
        w.set_hov_on(true);
    }
}

/// Every callback the bar answers.
pub fn wire(
    window: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    cat: &Rc<RefCell<Rc<Catalogue>>>,
) {
    let redraw = {
        let (weak, state, link, cat) = (
            window.as_weak(),
            Rc::clone(state),
            Rc::clone(link),
            Rc::clone(cat),
        );
        move |lensing: Lensing| {
            if let Some(w) = weak.upgrade() {
                draw(&w, &mut state.borrow_mut(), &link, &cat.borrow(), lensing);
            }
        }
    };
    {
        let (state, redraw) = (Rc::clone(state), redraw.clone());
        let state2 = Rc::clone(&state);
        window.on_q_col(move |line, x, round| state2.borrow().qb.field.col_at(line, x, round));
        window.on_q_room(move |room| {
            let mut st = state.borrow_mut();
            if (st.qb.field.room - room).abs() < 0.5 {
                return;
            }
            st.qb.field.room = room;
            drop(st);
            redraw(Lensing::Keep);
        });
    }
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        window.on_q_caret(move |a, c| {
            let Some(w) = weak.upgrade() else { return };
            let text = w.get_query().to_string();
            let raw_a = a.max(0) as usize;
            let raw_c = c.max(0) as usize;
            let (sa, sc) = {
                let mut s = state.borrow_mut();
                let prev = s.qb.cursor;
                let (sa, sc) =
                    frame::clamp_selection(&text, prev, raw_a, raw_c, |t| known_field(&s.spans, t));
                s.qb.anchor = sa;
                s.qb.cursor = sc;
                let at = |c: usize| lens::term_at(&text, c.min(text.len())).map(|t| t.start);
                if at(prev) != at(sc) {
                    s.qb.lenses.editing = false;
                }
                (sa, sc)
            };
            if (sa, sc) != (raw_a, raw_c) {
                w.set_anchor_to(if sa == sc { -1 } else { sa as i32 });
                w.set_caret_to(sc as i32);
                w.set_caret_ask(w.get_caret_ask() + 1);
                return;
            }
            redraw(Lensing::Follow);
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_q_hit(move |line, col| {
            let Some(w) = weak.upgrade() else { return 0 };
            let text = w.get_query().to_string();
            let s = state.borrow();
            let off = s.qb.field.hit(&text, line, col);
            frame::snap_press(&text, off, |t| known_field(&s.spans, t)) as i32
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_q_word(move |line, col| {
            let Some(w) = weak.upgrade() else {
                return Reach::default();
            };
            let text = w.get_query().to_string();
            let s = state.borrow();
            let at = s.qb.field.hit(&text, line, col);
            let (from, to) = frame::word_range(&text, at, |t| known_field(&s.spans, t));
            Reach {
                from: from as i32,
                to: to as i32,
            }
        });
    }
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        let (link, cat) = (Rc::clone(link), Rc::clone(cat));
        window.on_q_hover(move |line, col| {
            let Some(w) = weak.upgrade() else { return };
            let text = w.get_query().to_string();
            let found = (line >= 0)
                .then(|| state.borrow().qb.field.term_at(&text, line, col))
                .flatten();
            // A member of a place group: the pill acts on the whole group.
            let found = found.map(|(t, eline, ecol)| {
                let st = state.borrow();
                match st.qb.groups.iter().find(|g| g.0 <= t.start && t.end <= g.1) {
                    Some((a, b, l)) => (
                        lens::Term {
                            start: *a,
                            end: *b,
                            not: text[*a..].starts_with('!'),
                            field: Some("under"),
                            value: String::new(),
                        },
                        l.line as i32 - st.qb.field.first as i32,
                        (l.at + l.len) as i32,
                    ),
                    None => (t, eline, ecol),
                }
            });
            let Some((t, eline, ecol)) = found else {
                if w.get_hov_on() {
                    w.set_hov_on(false);
                    w.set_hov_fx(Default::default());
                    redraw(Lensing::Keep);
                }
                return;
            };
            let same = state
                .borrow()
                .qb
                .hover
                .as_ref()
                .is_some_and(|h| h.start == t.start && h.end == t.end);
            if same && w.get_hov_on() {
                return;
            }
            let raw = &text[t.start..t.end];
            let body = raw.strip_prefix('!').unwrap_or(raw);
            let fz_on = t.field == Some("fuzzy");
            let fz = fz_on
                || (t.field.is_none()
                    && !body.contains([':', '"', '*', '?', '/', '|', '(', ')'])
                    && body.chars().count() >= 3);
            w.set_hov_line(eline);
            w.set_hov_col(ecol);
            {
                let s = state.borrow();
                let f = &s.qb.field;
                w.set_hov_x(f.x(f.first + eline.max(0) as usize, ecol));
            }
            w.set_hov_not(t.not);
            w.set_hov_cap(t.not || t.field.is_some());
            w.set_hov_fz(fz);
            w.set_hov_fz_on(fz_on);
            w.set_hov_on(true);
            state.borrow_mut().qb.hover = Some(t);
            redraw(Lensing::Keep);
            crate::qeffect::hovered(&w, &state, &link, &cat.borrow());
        });
    }
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        window.on_q_pressed(move |line, col| {
            let Some(w) = weak.upgrade() else { return };
            let text = w.get_query().to_string();
            // A press anywhere else leaves an empty filter, which goes.
            let cur = state.borrow().qb.cursor;
            if drop_empty(&w, &state, Some(cur)) {
                return;
            }
            crate::qcaret::press_part(&state, &text);
            let t = state.borrow().qb.field.term_at(&text, line, col);
            // A press past the end of the text leaves the term there.
            if t.is_none() && state.borrow().qb.cursor >= text.len() && exit(&w, &state) {
                return;
            }
            let named =
                t.is_some_and(|(t, ..)| t.field.is_some() && lens::lens_of(&text, &t).is_some());
            redraw(if named {
                Lensing::Asked
            } else {
                Lensing::Follow
            });
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_term_act(move |what| {
            let Some(w) = weak.upgrade() else { return };
            let Some(t) = state.borrow().qb.hover.clone() else {
                return;
            };
            act(&w, &state, &t, &what);
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        // A capsule's own `×`, and the `!` over its picture: they name the
        // term they belong to, which need not be the one last hovered.
        window.on_cap_act(move |what, from, to| {
            let Some(w) = weak.upgrade() else { return };
            let text = w.get_query().to_string();
            let Some(t) = lens::terms(&text)
                .into_iter()
                .find(|t| t.start == from.max(0) as usize && t.end == to.max(0) as usize)
            else {
                return;
            };
            act(&w, &state, &t, &what);
        });
    }
    {
        let (weak, state, cat) = (window.as_weak(), Rc::clone(state), Rc::clone(cat));
        window.on_lens_step(move |by| {
            let Some(w) = weak.upgrade() else { return };
            let mut s = state.borrow_mut();
            s.qb.lenses.step(by);
            show_lens(&w, &mut s.qb, &cat.borrow());
        });
    }
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        window.on_lens_pick(move |i, stay| {
            let Some(w) = weak.upgrade() else { return };
            let text = w.get_query().to_string();
            let chosen = state
                .borrow_mut()
                .qb
                .lenses
                .choose(i.max(0) as usize, stay, &text);
            match chosen {
                Some(Chosen::Edit { query, caret, keep }) => {
                    write(&w, &state, query, caret);
                    redraw(if keep { Lensing::Asked } else { Lensing::Keep });
                    if !keep {
                        w.set_lens_open(false);
                    }
                }
                Some(Chosen::Inside { query, caret }) => {
                    write(&w, &state, query, caret);
                    w.set_lens_open(false);
                    // The line under the field: opened, or the keyboard put in it.
                    if w.get_grep_open() {
                        w.set_grep_focus_ask(w.get_grep_focus_ask() + 1);
                    } else {
                        w.invoke_grep_toggled();
                    }
                }
                Some(Chosen::Tile(k)) => {
                    crate::placeact::tile_pick(&w, &state, k, lens::Pick::Only, &redraw);
                }
                Some(Chosen::Value { field, value, keep }) => {
                    crate::panelact::value(&w, &state, field, &value, keep, &redraw);
                }
                None => {}
            }
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_lens_exit(move || weak.upgrade().is_some_and(|w| exit(&w, &state)));
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_q_backspace(move || weak.upgrade().is_some_and(|w| frame_backspace(&w, &state)));
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_q_arrow(move |forward| {
            weak.upgrade()
                .is_some_and(|w| crate::qcaret::arrow(&w, &state, forward))
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_q_delete(move || weak.upgrade().is_some_and(|w| frame_delete(&w, &state)));
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_q_before_char(move |ch| {
            weak.upgrade().is_some_and(|w| {
                // A space is how a term is left: an empty filter goes with it.
                (ch.as_str() == " " && {
                    let text = w.get_query().to_string();
                    let c = state.borrow().qb.cursor.min(text.len());
                    lens::term_at(&text, c).is_some_and(|t| {
                        c == t.end && t.field.is_some() && t.value.trim().is_empty()
                    }) && drop_empty(&w, &state, None)
                }) || frame_before_char(&w, &state, ch.as_str())
            })
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_lens_right(move || {
            let Some(w) = weak.upgrade() else {
                return false;
            };
            if !state.borrow().qb.lenses.deepens() {
                return false;
            }
            let sel = w.get_lens_sel();
            w.invoke_lens_pick(sel, true);
            true
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_lens_escape(move || {
            if let Some(w) = weak.upgrade() {
                if w.get_lens_launching() {
                    drop_launcher(&w, &state);
                } else if !drop_empty(&w, &state, None) {
                    state.borrow_mut().qb.lenses.close();
                    w.set_lens_open(false);
                }
            }
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_lens_close(move || {
            state.borrow_mut().qb.lenses.close();
            if let Some(w) = weak.upgrade() {
                w.set_lens_open(false);
            }
        });
    }
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        let launch = {
            let (weak, state, redraw) = (weak.clone(), Rc::clone(&state), redraw.clone());
            move || {
                let Some(w) = weak.upgrade() else { return };
                let text = w.get_query().to_string();
                // Pressed again while it is open: it closes, and takes the
                // lone `:` it left.
                if w.get_lens_open() && w.get_lens_launching() {
                    drop_launcher(&w, &state);
                    return;
                }
                // A `:` already waiting is opened, not joined by another.
                let waiting = lens::terms(&text).into_iter().find(|t| {
                    let r = &text[t.start..t.end];
                    r.starts_with(':') && !r[1..].contains(':')
                });
                if let Some(t) = waiting {
                    {
                        let mut st = state.borrow_mut();
                        st.qb.anchor = t.end;
                        st.qb.cursor = t.end;
                    }
                    w.set_caret_to(t.end as i32);
                    w.set_caret_ask(w.get_caret_ask() + 1);
                    redraw(Lensing::Asked);
                    return;
                }
                let mut c = state.borrow().qb.cursor.min(text.len());
                // Inside a term, the new one goes after it rather than
                // splitting it in two.
                if let Some(t) = lens::term_at(&text, c)
                    && t.start < c
                    && c < t.end
                {
                    c = t.end;
                }
                let lead = if c > 0 && !text[..c].ends_with(char::is_whitespace) {
                    " "
                } else {
                    ""
                };
                let tail = if c < text.len() && !text[c..].starts_with(char::is_whitespace) {
                    " "
                } else {
                    ""
                };
                let query = format!("{}{lead}:{tail}{}", &text[..c], &text[c..]);
                write(&w, &state, query, c + lead.len() + 1);
                redraw(Lensing::Asked);
            }
        };
        window.on_lens_launch(launch.clone());
        window.on_lens_toggle(move || {
            let Some(w) = weak.upgrade() else { return };
            if w.get_lens_open() {
                state.borrow_mut().qb.lenses.close();
                w.set_lens_open(false);
                return;
            }
            let text = w.get_query().to_string();
            let c = state.borrow().qb.cursor;
            let has = lens::term_at(&text, c).is_some_and(|t| lens::lens_of(&text, &t).is_some());
            if has {
                redraw(Lensing::Asked);
            } else {
                launch();
            }
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_offer_taken(move |i| {
            let Some(w) = weak.upgrade() else { return };
            let q = state
                .borrow()
                .qb
                .rescue
                .get(i.max(0) as usize)
                .map(|r| r.1.clone());
            let Some(q) = q else { return };
            let n = q.len();
            write(&w, &state, q, n);
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_narrow_taken(move |term, alt| {
            if let Some(w) = weak.upgrade() {
                take_chip(&w, &state, &term, alt);
            }
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_fix_taken(move || {
            let Some(w) = weak.upgrade() else { return };
            let fixed = state.borrow_mut().qb.fix.take();
            if let Some(q) = fixed {
                let n = q.len();
                write(&w, &state, q, n);
            }
        });
    }
    {
        let redraw = redraw.clone();
        window.on_q_focus_moved(move || {
            let redraw = redraw.clone();
            slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                redraw(Lensing::Keep);
            });
        });
    }
    {
        let (state, redraw) = (Rc::clone(state), redraw.clone());
        window.on_q_measured(move |i, wide| {
            // Told while the window lays itself out: the drawing waits its turn.
            let moved = state
                .try_borrow_mut()
                .is_ok_and(|mut s| measured(&mut s, i.max(0) as usize, wide));
            if moved {
                let redraw = redraw.clone();
                slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                    redraw(Lensing::Keep);
                });
            }
        });
    }
    wire_v2(window, state, link, cat, redraw);
}

pub(crate) fn open_or_focus_field(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    redraw: &impl Fn(Lensing),
    field_name: &str,
) {
    let text = w.get_query().to_string();
    let existing = lens::terms(&text).into_iter().find(|t| {
        t.field == Some(field_name) || (field_name == "under" && t.field == Some("parent"))
    });
    if let Some(t) = existing {
        let end = if text[..t.end].ends_with('"') {
            t.end - 1
        } else {
            t.end
        };
        {
            let mut st = state.borrow_mut();
            st.qb.anchor = end;
            st.qb.cursor = end;
        }
        w.set_caret_to(end as i32);
        w.set_caret_ask(w.get_caret_ask() + 1);
        redraw(Lensing::Asked);
        return;
    }
    // Where it looks is the first thing the query says; anything else is
    // written at the end.
    let (q, caret) = if field_name == "under" {
        lens::at_head(&text, "under:", 0)
    } else {
        let lead = if !text.is_empty() && !text.ends_with(char::is_whitespace) {
            " "
        } else {
            ""
        };
        let q = format!("{text}{lead}{field_name}:");
        let caret = q.len();
        (q, caret)
    };
    write(w, state, q, caret);
    redraw(Lensing::Asked);
}

pub fn picked_folder(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    a: usize,
    b: usize,
    prefix: String,
    path: String,
) {
    let text = w.get_query().to_string();
    let a = a.min(text.len());
    let b = b.min(text.len()).max(a);
    let shown = qlens::home_path(&home(), &path);
    let quoted = crate::placeact::quote(shown);
    let tail = if !text[b..].starts_with(char::is_whitespace) {
        " "
    } else {
        ""
    };
    let ins = format!("{prefix}{quoted}{tail}");
    let q = format!("{}{ins}{}", &text[..a], &text[b..]);
    let caret = a + ins.len();
    state.borrow_mut().qb.lenses.close();
    w.set_lens_open(false);
    write(w, state, q, caret);
}

fn wire_v2(
    window: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    cat: &Rc<RefCell<Rc<Catalogue>>>,
    redraw: impl Fn(Lensing) + Clone + 'static,
) {
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        window.on_lens_field(move |name| {
            if let Some(w) = weak.upgrade() {
                open_or_focus_field(&w, &state, &redraw, name.as_str());
            }
        });
    }
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        window.on_lens_fuzzy(move || {
            let Some(w) = weak.upgrade() else { return };
            let text = w.get_query().to_string();
            let c = state.borrow().qb.cursor.min(text.len());
            let word = lens::term_at(&text, c)
                .filter(|t| lens::lens_of(&text, t) == Some(lens::Lens::Word))
                .or_else(|| {
                    lens::terms(&text)
                        .into_iter()
                        .find(|t| lens::lens_of(&text, t) == Some(lens::Lens::Word))
                });
            if let Some(t) = word {
                {
                    let mut st = state.borrow_mut();
                    st.qb.anchor = t.end;
                    st.qb.cursor = t.end;
                }
                w.set_caret_to(t.end as i32);
                w.set_caret_ask(w.get_caret_ask() + 1);
                redraw(Lensing::Asked);
            } else {
                open_or_focus_field(&w, &state, &redraw, "fuzzy");
            }
        });
    }
    crate::panelact::wire(window, state, link, cat, redraw.clone());
    crate::placeact::wire(window, state, link, cat, redraw.clone());
    {
        let (weak, state, link, cat) = (
            window.as_weak(),
            Rc::clone(state),
            Rc::clone(link),
            Rc::clone(cat),
        );
        window.on_narrow_toggled(move || {
            let Some(w) = weak.upgrade() else { return };
            let total = {
                let mut s = state.borrow_mut();
                if w.get_narrow_shown() {
                    s.qb.narrow_pinned = false;
                    s.qb.narrow_hidden = true;
                } else {
                    s.qb.narrow_hidden = false;
                    s.qb.narrow_pinned = true;
                    s.qb.narrow_for.clear();
                }
                s.qb.narrow_total.max(100)
            };
            narrow(&w, &state, &link, &cat.borrow(), total);
        });
    }
}
