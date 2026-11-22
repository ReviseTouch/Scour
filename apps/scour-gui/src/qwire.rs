//! The query bar wired to the window: what Rust draws for it, what each press
//! and key does, and what comes back from the service for the lens. The
//! geometry is `qbar`, the lens `qlens`; this is where they meet `State`.

use std::cell::RefCell;
use std::rc::Rc;

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_proto::Response;
use scour_ui::lens;
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::link::{Ask, Link};
use crate::qbar::{self, Field};
use crate::qlens::{Chosen, Env, Lenses};
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
    hover: Option<lens::Term>,
    /// Each filter of a query that found nothing: the term, the query
    /// without it, and what that finds once counted.
    rescue: Vec<(String, String, Option<(u64, bool)>)>,
    rescue_for: String,
    /// The query with a misspelt field name put right, when there is one.
    fix: Option<String>,
    /// The narrowing line: the query it is for, its answers, and its chips
    /// as label, count and the term a press writes.
    pub narrow_for: String,
    narrow_total: u64,
    /// The line switched off in the settings: nothing is asked for it.
    pub narrow_off: bool,
    narrow_facets: Option<scour_core::FacetResponse>,
    narrow_usage: Option<scour_core::UsageResponse>,
    chips: Vec<(String, u64, String)>,
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

fn home() -> String {
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
    let kind_name = |token: &str| {
        scour_core::Kind::from_name(token)
            .and_then(|k| k.first())
            .map_or_else(|| token.to_string(), |k| cat.get(k.msgid()).into_owned())
    };
    let grouped = |n: u64| scour_ui::format::grouped(n, crate::marks().0);
    let bytes = |n: u64| scour_ui::format::compact_bytes(n, crate::marks().1);
    let home = home();
    f(&Env {
        tr: &tr,
        kind_name: &kind_name,
        home: &home,
        places,
        grouped: &grouped,
        bytes: &bytes,
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

/// Lay the query out and draw it: the runs, the capsules, the caret, the
/// selection, the field's height; and the lens, as `lensing` says.
pub fn draw(w: &MainWindow, s: &mut State, link: &Link, cat: &Catalogue, lensing: Lensing) {
    let text = w.get_query().to_string();
    let qb = &mut s.qb;
    qb.field.relayout(&text);
    let runs = qb.field.place(crate::painted(&text, &s.spans));
    let mut caps = qb.field.caps(&text, &s.spans);
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
    w.set_spans(ModelRc::new(VecModel::from(runs)));
    w.set_caps(ModelRc::new(VecModel::from(caps)));
    w.set_sels(ModelRc::new(VecModel::from(sels)));
    w.set_q_shown(qb.field.shown() as i32);
    w.set_q_first(qb.field.first as i32);
    w.set_caret_line(line);
    w.set_caret_col(col);
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
}

/// The open lens, as rows and words.
fn show_lens(w: &MainWindow, qb: &mut QueryBar, cat: &Catalogue) {
    let text = w.get_query().to_string();
    let Some(o) = qb.lenses.open.as_ref().filter(|o| !o.items.is_empty()) else {
        w.set_lens_open(false);
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
        })
        .collect();
    let counted = o.items.iter().any(|i| i.count || i.n.is_some());
    let (line, col) = qb.field.start_of(&text, o.a);
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
    let end_line = qb.field.start_of(&text, o.b.max(1) - 1).0;
    w.set_lens_line(end_line.max(line));
    w.set_lens_col(col);
    w.set_lens_launching(o.kind == lens::Lens::Launch);
    w.set_lens_open(true);
}

/// Put a new query in the field, search it, and put the caret at `caret`.
fn write(w: &MainWindow, s: &Rc<RefCell<State>>, query: String, caret: usize) {
    {
        let mut st = s.borrow_mut();
        st.qb.anchor = caret;
        st.qb.cursor = caret;
        st.qb.hover = None;
    }
    w.set_hov_on(false);
    w.set_query(query.as_str().into());
    w.invoke_query_changed(query.into());
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
    if slot >= RESCUE {
        rescue_answer(w, s, cat, slot - RESCUE, &reply);
        return;
    }
    let text = w.get_query().to_string();
    let qb = &mut s.qb;
    let wants = with_env(cat, &qb.places, |env| {
        qb.lenses.answer(generation, slot, reply, &text, env)
    });
    send(link, qb.lenses.generation, wants);
    show_lens(w, &mut s.qb, cat);
}

/// A search came back. With nothing found, ask what each filter left out
/// would find; with something, take the offers away.
pub fn searched(w: &MainWindow, s: &mut State, link: &Link, cat: &Catalogue, total: u64) {
    let text = s.query.clone();
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
pub fn narrow(w: &MainWindow, s: &mut State, link: &Link, cat: &Catalogue, total: u64) {
    if s.qb.narrow_off {
        if !s.qb.chips.is_empty() {
            s.qb.chips.clear();
            show_chips(w, &s.qb, cat);
        }
        return;
    }
    let text = s.query.clone();
    if s.qb.narrow_for == text && (s.qb.narrow_total < 2) == (total < 2) {
        return;
    }
    s.qb.narrow_for = text.clone();
    s.qb.narrow_total = total;
    s.qb.narrow_facets = None;
    s.qb.narrow_usage = None;
    s.qb.chips.clear();
    show_chips(w, &s.qb, cat);
    let ts = lens::terms(&text);
    if total < 2 || lens::reads(&text) {
        return;
    }
    let revision = s.query_revision;
    link.send(Ask::Narrow {
        query_revision: revision,
        slot: 0,
        request: scour_proto::Request::Facets {
            query: text.clone(),
            by: vec![
                scour_core::FacetBy::Kind,
                scour_core::FacetBy::Ext { top: 6 },
                scour_core::FacetBy::Age { edges: vec![7] },
            ],
        },
    });
    let home = home();
    if !home.is_empty()
        && !ts
            .iter()
            .any(|t| matches!(t.field, Some("under" | "parent")))
    {
        link.send(Ask::Narrow {
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
}

/// An answer for the narrowing line.
pub fn narrowed(w: &MainWindow, s: &mut State, cat: &Catalogue, revision: u64, reply: Response) {
    if revision != s.query_revision {
        return;
    }
    match reply {
        Response::Facets(f) => s.qb.narrow_facets = Some(f),
        Response::Usage(u) => s.qb.narrow_usage = Some(u),
        _ => return,
    }
    let text = s.qb.narrow_for.clone();
    let ts = lens::terms(&text);
    let has = |f: &str| ts.iter().any(|t| t.field == Some(f) && !t.not);
    let all =
        s.qb.narrow_facets
            .as_ref()
            .map_or(s.qb.narrow_total, |f| f.total);
    let mut chips = Vec::new();
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
            .take(3)
        {
            if x.count > 0 && x.count < all {
                let name = scour_core::Kind::from_name(&x.key)
                    .and_then(|k| k.first())
                    .map_or_else(|| x.key.clone(), |k| cat.get(k.msgid()).into_owned());
                chips.push((name, x.count, format!("kind:{}", x.key)));
            }
        }
    }
    if !has("ext") {
        for x in group(|b| matches!(b, scour_core::FacetBy::Ext { .. }))
            .into_iter()
            .take(3)
        {
            if !x.key.is_empty() && x.count > 0 && x.count < all {
                chips.push((format!(".{}", x.key), x.count, format!("ext:{}", x.key)));
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
        for c in kids.into_iter().take(2) {
            let shown = match c.path.strip_prefix(&home) {
                Some(rest) if !home.is_empty() => format!("~{rest}"),
                _ => c.path.clone(),
            };
            let term = lens::place(&lens::terms("under:x")[0], &shown);
            chips.push((shown, c.files, term));
        }
    }
    if !has("dm")
        && let Some(week) = group(|b| matches!(b, scour_core::FacetBy::Age { .. }))
            .into_iter()
            .find(|x| x.key == "7")
        && week.count > 0
        && week.count < all
    {
        chips.push((
            cat.get("last 7 days").into_owned(),
            week.count,
            "dm:7d".into(),
        ));
    }
    s.qb.chips = chips;
    show_chips(w, &s.qb, cat);
}

fn show_chips(w: &MainWindow, qb: &QueryBar, _cat: &Catalogue) {
    let chips: Vec<crate::Facet> = qb
        .chips
        .iter()
        .map(|(label, n, term)| crate::Facet {
            label: label.as_str().into(),
            token: term.as_str().into(),
            count: crate::grouped(*n).into(),
            share: 0.0,
            icon: Default::default(),
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

/// Backspace at a capsule's frame: past the closing quote it steps inside;
/// in empty quotes it takes both; after a bare `field:` the whole term.
fn frame_backspace(w: &MainWindow, s: &Rc<RefCell<State>>) -> bool {
    let text = w.get_query().to_string();
    let (anchor, c) = {
        let st = s.borrow();
        (st.qb.anchor, st.qb.cursor.min(text.len()))
    };
    if anchor != c {
        return false;
    }
    let Some(t) = lens::term_at(&text, c) else {
        return false;
    };
    let raw = &text[t.start..t.end];
    let body = raw.strip_prefix('!').unwrap_or(raw);
    let Some(colon) = body.find(':') else {
        return false;
    };
    if t.field.is_none() {
        return false;
    }
    let head = t.start + usize::from(t.not) + colon + 1;
    let b = text.as_bytes();
    let quoted = b.get(head) == Some(&b'"');
    if quoted && c == t.end && c >= 1 && b[c - 1] == b'"' && c - 1 > head {
        s.borrow_mut().qb.cursor = c - 1;
        s.borrow_mut().qb.anchor = c - 1;
        w.set_caret_to((c - 1) as i32);
        w.set_caret_ask(w.get_caret_ask() + 1);
        return true;
    }
    if quoted && c == head + 1 && b.get(c) == Some(&b'"') {
        write(
            w,
            s,
            format!("{}{}", &text[..head], &text[head + 2..]),
            head,
        );
        return true;
    }
    if c == head && c == t.end {
        let (query, caret) = lens::replace(&text, &t, "");
        s.borrow_mut().qb.lenses.close();
        w.set_lens_open(false);
        write(w, s, query, caret);
        return true;
    }
    false
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
        window.on_q_cols(move |n| {
            state.borrow_mut().qb.field.cols = n.max(8) as usize;
            redraw(Lensing::Keep);
        });
    }
    {
        let (state, redraw) = (Rc::clone(state), redraw.clone());
        window.on_q_caret(move |a, c| {
            {
                let mut s = state.borrow_mut();
                s.qb.anchor = a.max(0) as usize;
                s.qb.cursor = c.max(0) as usize;
            }
            redraw(Lensing::Follow);
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_q_hit(move |line, col| {
            let Some(w) = weak.upgrade() else { return 0 };
            let text = w.get_query().to_string();
            state.borrow().qb.field.hit(&text, line, col) as i32
        });
    }
    {
        let weak = window.as_weak();
        window.on_q_word(move |at| {
            let Some(w) = weak.upgrade() else {
                return Reach::default();
            };
            let text = w.get_query().to_string();
            let at = (at.max(0) as usize).min(text.len());
            match lens::terms(&text)
                .into_iter()
                .find(|t| at >= t.start && at <= t.end)
            {
                Some(t) => Reach {
                    from: t.start as i32,
                    to: t.end as i32,
                },
                None => Reach {
                    from: at as i32,
                    to: at as i32,
                },
            }
        });
    }
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        window.on_q_hover(move |line, col| {
            let Some(w) = weak.upgrade() else { return };
            let text = w.get_query().to_string();
            let found = (line >= 0)
                .then(|| state.borrow().qb.field.term_at(&text, line, col))
                .flatten();
            let Some((t, eline, ecol)) = found else {
                if w.get_hov_on() {
                    w.set_hov_on(false);
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
            w.set_hov_not(t.not);
            w.set_hov_fz(fz);
            w.set_hov_fz_on(fz_on);
            w.set_hov_on(true);
            state.borrow_mut().qb.hover = Some(t);
            redraw(Lensing::Keep);
        });
    }
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        window.on_q_pressed(move |line, col| {
            let Some(w) = weak.upgrade() else { return };
            let text = w.get_query().to_string();
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
            let text = w.get_query().to_string();
            let Some(t) = state.borrow().qb.hover.clone() else {
                return;
            };
            if t.end > text.len() {
                return;
            }
            let raw = &text[t.start..t.end];
            let body = raw.strip_prefix('!').unwrap_or(raw);
            let neg = if t.not { "!" } else { "" };
            let with = match what.as_str() {
                "drop" => String::new(),
                "fz" if t.field == Some("fuzzy") => {
                    format!("{neg}{}", &body[body.find(':').map_or(0, |i| i + 1)..])
                }
                "fz" => format!("{neg}fuzzy:{body}"),
                _ if t.not => body.to_string(),
                _ => format!("!{raw}"),
            };
            let (query, caret) = lens::replace(&text, &t, &with);
            write(&w, &state, query, caret);
            // Kept under the pointer, so a second press undoes the first in place.
            if !with.is_empty()
                && let Some(t) = lens::term_at(&w.get_query(), caret)
            {
                state.borrow_mut().qb.hover = Some(t);
                w.set_hov_not(with.starts_with('!'));
                w.set_hov_fz_on(with.trim_start_matches('!').starts_with("fuzzy:"));
                w.set_hov_on(true);
            }
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
        let (weak, state, link, cat, redraw) = (
            window.as_weak(),
            Rc::clone(state),
            Rc::clone(link),
            Rc::clone(cat),
            redraw.clone(),
        );
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
                Some(Chosen::Disk(id)) => {
                    link.send(Ask::Disk {
                        volume: id,
                        what: "index".into(),
                    });
                    crate::told(
                        &w,
                        cat.borrow()
                            .get("The disk is being indexed.")
                            .as_ref()
                            .into(),
                    );
                    state.borrow_mut().qb.lenses.forget("volumes");
                    redraw(Lensing::Asked);
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
                } else {
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
                let c = state.borrow().qb.cursor.min(text.len());
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
}
