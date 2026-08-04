//! The Scour window.
//!
//! A frontend and nothing else. It holds no index, walks no filesystem and
//! does not link the engine — everything it knows arrived over a socket, which
//! is the same thing `apps/scour` can say and the reason both can exist.
//!
//! Three rules shape the whole file:
//!
//! * **The window never waits.** Every call to the service happens on a worker
//!   thread and comes back as an event ([`link`]).
//! * **The frontend does not parse queries.** What a term means, what colour
//!   it is and where a name matched are all decided by the engine or by
//!   `scour-core`, never here. A second parser is a parser that drifts.
//! * **A stale answer is dropped, not shown.** Every request carries the
//!   keystroke that caused it, and a reply for an older one is discarded. The
//!   alternative — a slow answer to `re` landing after a fast one to `rapor` —
//!   is the most noticeable defect a search-as-you-type box can have.

mod link;
mod rows;

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Context, Result};
use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_proto::Response;
use slint::{ComponentHandle, ModelRc, VecModel};

use link::{Ask, Got, Link};

/// The interface, as `slint-build` generated it.
///
/// Wrapped in a module so the workspace's lints stop at the boundary: this is
/// forty thousand lines nobody wrote, and holding generated code to a rule
/// about `Debug` implementations only trains everyone to ignore warnings.
mod ui {
    #![allow(
        missing_debug_implementations,
        clippy::all,
        clippy::pedantic,
        unused_qualifications
    )]
    slint::include_modules!();
}

pub use ui::{Facet, MainWindow, Row, Theme};

thread_local! {
    /// Where an answer goes once it is back on the UI thread.
    ///
    /// A thread-local rather than a field, because the closure that crosses
    /// from a worker thread has to be `Send` and everything this touches is
    /// `Rc`. Filled once, at start, on the thread that owns the window.
    static INBOX: RefCell<Option<Rc<dyn Fn(Got)>>> = const { RefCell::new(None) };
}

/// Hand one answer to the window. Runs on the UI thread and nowhere else.
fn deliver(got: Got) {
    // Cloned out of the cell before being called: the handler touches the
    // window, the window can raise a callback, and a callback that reached
    // this function again would find the `RefCell` already borrowed.
    let handler = INBOX.with(|slot| slot.borrow().clone());
    if let Some(f) = handler {
        f(got);
    }
}

/// A catalogue lookup, ready for the interface.
///
/// `Catalog::get` answers with a `Cow` — borrowed when the language is English
/// and the msgid *is* the string — and Slint wants its own type. One place to
/// convert rather than twenty.
fn t(cat: &Catalogue, msgid: &str) -> slint::SharedString {
    cat.get(msgid).as_ref().into()
}

/// How long after a keystroke the search actually goes out.
///
/// Not a guess at typing speed — a bound on wasted work. A search is a handful
/// of milliseconds, so this could be zero and still feel instant; what it saves
/// is the six intermediate queries between `r` and `rapor`, each of which
/// matches far more than the finished one and costs far more to answer.
const DEBOUNCE_MS: u64 = 60;

/// How many rows are fetched at a time.
///
/// The addressable window is ten thousand rows — past that a deep page costs
/// more than a frame, measured — but the screen holds twenty and nobody scrolls
/// two hundred by hand. Paging beyond this is Phase 5.3.
const PAGE: u32 = 200;

struct State {
    generation: u64,
    /// The generation whose search reply is currently on screen.
    shown: u64,
    query: String,
    sort: String,
    descending: bool,
    /// The `kind:` term the rail has active, if any.
    facet: Option<String>,
    hits: Vec<scour_core::Hit>,
    down: bool,
}

fn main() -> Result<()> {
    let cat = Rc::new(Catalogue::for_language(&language()));
    let window = MainWindow::new().context("the window could not be created")?;

    let state = Rc::new(RefCell::new(State {
        generation: 0,
        shown: 0,
        query: String::new(),
        sort: "relevance".into(),
        descending: true,
        facet: None,
        hits: Vec::new(),
        down: false,
    }));

    let rows: Rc<VecModel<Row>> = Rc::new(VecModel::default());
    let facets: Rc<VecModel<Facet>> = Rc::new(VecModel::default());
    window.set_rows(ModelRc::from(rows.clone()));
    window.set_facets(ModelRc::from(facets.clone()));
    window.global::<Theme>().set_dark(prefers_dark());
    window.set_hint(t(&cat, "type to search"));
    window.set_meter(t(&cat, "connecting…"));
    window.set_scope_label(t(&cat, "Everything"));

    let addr = scour_config::Config::load_or_default().0.socket();

    // The bridge from the worker threads to the UI thread.
    //
    // `invoke_from_event_loop` takes a `Send` closure, and everything the
    // answer has to touch — the window, the two models, the catalogue, the
    // state — is `Rc` and deliberately not `Send`. So the closure carries only
    // the answer, and finds the rest in a thread-local that was filled on the
    // UI thread. The alternative is an `Arc<Mutex<…>>` around state that only
    // one thread ever touches, which is a lock that can never contend and a
    // claim about threading that is not true.
    let weak = window.as_weak();
    let ui_state = state.clone();
    let ui_rows = rows.clone();
    let ui_facets = facets.clone();
    let ui_cat = cat.clone();
    INBOX.with(|slot| {
        *slot.borrow_mut() = Some(Rc::new(move |got: Got| {
            let Some(w) = weak.upgrade() else { return };
            apply(&w, &ui_state, &ui_rows, &ui_facets, &ui_cat, got);
        }));
    });

    let sink = move |got: Got| {
        let _ = slint::invoke_from_event_loop(move || deliver(got));
    };

    let link = Rc::new(Link::start(addr, sink));

    // --- the query line ---------------------------------------------------
    {
        let state = state.clone();
        let link = link.clone();
        let weak = window.as_weak();
        window.on_query_changed(move |text| {
            let generation = {
                let mut s = state.borrow_mut();
                s.query = text.to_string();
                s.generation += 1;
                s.generation
            };
            if let Some(w) = weak.upgrade() {
                w.set_busy(true);
            }
            // Debounced by generation rather than by cancelling a timer: when
            // the timer fires, it asks the state what the latest keystroke was
            // and gives up if it is no longer the one that scheduled it.
            let state = state.clone();
            let link = link.clone();
            let weak = weak.clone();
            slint::Timer::single_shot(std::time::Duration::from_millis(DEBOUNCE_MS), move || {
                if state.borrow().generation != generation {
                    return;
                }
                let _ = weak;
                dispatch(&state, &link);
            });
        });
    }

    // --- the rail ---------------------------------------------------------
    {
        let state = state.clone();
        let link = link.clone();
        let weak = window.as_weak();
        window.on_facet_clicked(move |token| {
            let token = token.to_string();
            {
                let mut s = state.borrow_mut();
                // Clicking the active one clears it. A filter you cannot see
                // how to remove is worse than no filter.
                s.facet = if s.facet.as_deref() == Some(token.as_str()) {
                    None
                } else {
                    Some(token.clone())
                };
                s.generation += 1;
            }
            if let Some(w) = weak.upgrade() {
                let s = state.borrow();
                w.set_active_facet(s.facet.clone().unwrap_or_default().into());
                w.set_busy(true);
            }
            dispatch(&state, &link);
        });
    }

    // --- the column headers ----------------------------------------------
    {
        let state = state.clone();
        let link = link.clone();
        let weak = window.as_weak();
        window.on_sort_by(move |key| {
            let key = key.to_string();
            {
                let mut s = state.borrow_mut();
                if s.sort == key {
                    s.descending = !s.descending;
                } else {
                    s.sort = key;
                    // Every order but the name reads best newest-or-largest
                    // first, and the name reads best A to Z.
                    s.descending = s.sort != "name" && s.sort != "path";
                }
                s.generation += 1;
            }
            if let Some(w) = weak.upgrade() {
                w.set_busy(true);
            }
            dispatch(&state, &link);
        });
    }

    // --- opening things ---------------------------------------------------
    {
        let state = state.clone();
        window.on_activated(move |i| {
            let s = state.borrow();
            if let Some(h) = s.hits.get(i.max(0) as usize) {
                open(&h.path);
            }
        });
    }
    {
        let state = state.clone();
        window.on_reveal(move |i| {
            let s = state.borrow();
            if let Some(h) = s.hits.get(i.max(0) as usize) {
                let dir = match h.path.rfind('/') {
                    Some(0) => "/",
                    Some(at) => &h.path[..at],
                    None => ".",
                };
                open(dir);
            }
        });
    }
    {
        let state = state.clone();
        let weak = window.as_weak();
        let cat = cat.clone();
        window.on_copy_path(move |i| {
            let s = state.borrow();
            let Some(h) = s.hits.get(i.max(0) as usize) else {
                return;
            };
            // No clipboard dependency: the window is a client of a service, and
            // adding an X11/Wayland clipboard crate to copy one string is a
            // dependency for a line of text. The path goes to stdout, where a
            // shell pipeline can take it, and the hint says so.
            println!("{}", h.path);
            if let Some(w) = weak.upgrade() {
                w.set_hint(t(&cat, "path printed to the terminal"));
            }
        });
    }

    // The first search is the empty one: everything, newest first, which is
    // what the window should already be showing when it appears.
    dispatch(&state, &link);
    window.run().context("the event loop failed")?;
    Ok(())
}

/// Send the search and the facet count for the current state.
fn dispatch(state: &Rc<RefCell<State>>, link: &Rc<Link>) {
    let (generation, query, sort, descending) = {
        let s = state.borrow();
        (s.generation, full_query(&s), s.sort.clone(), s.descending)
    };
    link.send(Ask::Search {
        generation,
        query: query.clone(),
        sort,
        descending,
        limit: PAGE,
    });
    link.send(Ask::Facets { generation, query });
}

/// What the user typed, plus whatever the rail has active.
///
/// Composed here rather than pushed into the text field, and that is a real
/// choice: putting `kind:image` into the box would let the user delete half of
/// it and leave the rail lit with nothing behind it. The rail is a lens over
/// the query, not an edit of it.
fn full_query(s: &State) -> String {
    match &s.facet {
        Some(k) if s.query.trim().is_empty() => format!("kind:{k}"),
        Some(k) => format!("{} kind:{k}", s.query.trim()),
        None => s.query.trim().to_owned(),
    }
}

fn apply(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    rows: &Rc<VecModel<Row>>,
    facets: &Rc<VecModel<Facet>>,
    cat: &Rc<Catalogue>,
    got: Got,
) {
    match got {
        Got::Down(why) => {
            state.borrow_mut().down = true;
            w.set_busy(false);
            w.set_meter(format!("{} — {why}", cat.get("the service is not running")).into());
        }
        Got::Refused { generation, why } => {
            if generation != state.borrow().generation {
                return;
            }
            // Shown rather than swallowed. Most of these are "that term is too
            // short for the index to answer", and an empty list with no reason
            // is how someone concludes the tool is broken while the tool is
            // telling them something.
            w.set_busy(false);
            w.set_meter(why.into());
        }
        Got::Up => {
            state.borrow_mut().down = false;
            w.set_hint(t(cat, "type to search"));
        }
        Got::Search { generation, reply } => {
            {
                let s = state.borrow();
                // Stale: a newer keystroke has already gone out, and showing
                // this would make the list go backwards.
                if generation < s.shown || generation != s.generation {
                    return;
                }
            }
            let Response::Search(r) = *reply else {
                w.set_busy(false);
                return;
            };
            let now = unix_now();
            let terms = terms_of(&state.borrow().query);
            let fresh: Vec<Row> = r
                .hits
                .iter()
                .map(|h| rows::row_of(h, &terms, now, cat))
                .collect();
            rows.set_vec(fresh);
            {
                let mut s = state.borrow_mut();
                s.shown = generation;
                s.hits = r.hits;
            }
            w.set_selected(0);
            w.set_busy(false);
            w.set_meter(
                format!(
                    "{}{} {} · {:.1} ms",
                    r.total,
                    if r.capped { "+" } else { "" },
                    cat.get("matches"),
                    r.took_us as f64 / 1000.0
                )
                .into(),
            );
        }
        Got::Facets { generation, reply } => {
            if generation != state.borrow().generation {
                return;
            }
            let Response::Facets(f) = *reply else { return };
            // Ordered by the taxonomy rather than by count, so the rail does
            // not reshuffle under the pointer between two keystrokes.
            let mut fresh: Vec<Facet> = Vec::new();
            for k in rows::offered_kinds() {
                let token = k.token();
                let Some(hit) = f.facets.iter().find(|x| x.key == token) else {
                    continue;
                };
                fresh.push(Facet {
                    label: t(cat, k.msgid()),
                    token: token.into(),
                    count: compact(hit.count).into(),
                });
            }
            facets.set_vec(fresh);
        }
    }
}

/// The plain text terms of a query, for highlighting and for nothing else.
///
/// Only the words. A `kind:` or `size:` term matched a *column*, not a run of
/// the name, so lighting up `pdf` inside `report.pdf` because the query said
/// `ext:pdf` would claim a match that did not happen there. A negated term
/// matched nothing by definition.
///
/// Quotes hold a phrase together, because that is what they do in the query
/// language: `"iki kelime"` is one thing to find, and highlighting the two
/// words separately would draw two marks where the engine found one.
///
/// This is the one place the window looks at query text at all, and it stops
/// at *which words*. What a term **means** is `explain`'s answer and stays
/// the engine's business — a second parser is a parser that drifts, and the
/// mockup measured what that costs.
fn terms_of(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let push = |w: &mut String, quoted: bool, out: &mut Vec<String>| {
        let t = std::mem::take(w);
        // A field term or an exclusion is not text in the name. Inside quotes
        // a colon is part of the phrase, so the test only applies outside.
        if t.is_empty() || (!quoted && (t.contains(':') || t.starts_with('!'))) {
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

/// A count, short enough for a rail 168 pixels wide.
fn compact(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{}k", n / 1_000),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Hand a path to the desktop.
///
/// `xdg-open` on Linux and its two equivalents elsewhere — spawned and
/// forgotten, because waiting for a file manager to exit would hold the window.
fn open(path: &str) {
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(windows)]
    let cmd = "explorer";
    let _ = std::process::Command::new(cmd).arg(path).spawn();
}

/// The language the catalogue should speak.
fn language() -> String {
    let cfg = scour_config::Config::load_or_default().0;
    if !cfg.ui.language.is_empty() {
        return cfg.ui.language;
    }
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .map(|v| v.split(['_', '.']).next().unwrap_or("en").to_owned())
        .unwrap_or_else(|_| "en".into())
}

/// Is the desktop asking for a dark interface?
///
/// Read once at start and not watched. A theme that changes while the window
/// is open is a real thing and a rare one; a portal subscription to catch it is
/// a dependency and a background task, and this is not the release to spend
/// them on.
fn prefers_dark() -> bool {
    // The GNOME/GTK convention, which every desktop this is likely to run on
    // now honours. Anything unreadable means dark, which is what the palette
    // was designed against first.
    match std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
    {
        Ok(o) => {
            let v = String::from_utf8_lossy(&o.stdout);
            !v.contains("prefer-light")
        }
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_words_are_highlighted() {
        // `ext:pdf` matched a column. Lighting up `pdf` inside `report.pdf`
        // would claim a match that did not happen in the name.
        assert_eq!(terms_of("rapor ext:pdf"), vec!["rapor".to_string()]);
        assert_eq!(terms_of("!eski rapor"), vec!["rapor".to_string()]);
        // Quotes hold a phrase together, the way they do in the query
        // language: one mark, not two.
        assert_eq!(terms_of("\"iki kelime\""), vec!["iki kelime".to_string()]);
        assert_eq!(
            terms_of("\"a:b\""),
            vec!["a:b".to_string()],
            "quoted, so not a field"
        );
        assert!(terms_of("kind:image").is_empty());
    }

    #[test]
    fn the_rail_composes_with_the_text_rather_than_replacing_it() {
        let mut s = State {
            generation: 0,
            shown: 0,
            query: "rapor".into(),
            sort: "relevance".into(),
            descending: true,
            facet: None,
            hits: Vec::new(),
            down: false,
        };
        assert_eq!(full_query(&s), "rapor");
        s.facet = Some("image".into());
        assert_eq!(full_query(&s), "rapor kind:image");
        s.query = "  ".into();
        assert_eq!(full_query(&s), "kind:image");
    }

    #[test]
    fn counts_stay_narrow_enough_for_the_rail() {
        assert_eq!(compact(7), "7");
        assert_eq!(compact(9_999), "9999");
        assert_eq!(compact(12_345), "12k");
        assert_eq!(compact(2_951_074), "3.0M");
    }

    #[test]
    fn every_offered_kind_has_a_word_in_every_shipped_language() {
        for (tag, _) in scour_i18n::LANGUAGES {
            let c = Catalogue::for_language(tag);
            for k in scour_core::Kind::OFFERED {
                assert!(!c.get(k.msgid()).is_empty(), "{tag} has no word for {k:?}");
            }
        }
    }
}
