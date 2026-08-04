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
    /// When the process started, until the first rows are drawn.
    static FIRST: std::cell::Cell<Option<std::time::Instant>> = const {
        std::cell::Cell::new(None)
    };
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

/// Diagnostics, off unless asked for.
///
/// `SCOUR_TRACE=1 scour-gui` — because the interesting failures here are the
/// ones where a keystroke goes in and nothing comes out, and the only way to
/// tell which of the four steps dropped it is to watch all four.
fn trace(what: &str) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var("SCOUR_TRACE").is_ok()) {
        eprintln!("gui: {what}");
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
/// **Longer than a person's gap between keys, or it collapses nothing.** Sixty
/// milliseconds was the first number here and it was worse than useless: a
/// keystroke every 150 ms means every timer fires before the next key arrives,
/// so `toki` issued four searches and four facet counts instead of one. The
/// trace showed all eight going out and all eight coming back, seven of them
/// to be thrown away *after* being paid for.
///
/// A hundred and eighty was right when a search cost ninety milliseconds and
/// eight of them were in flight at once. It is wrong now that one costs two:
/// **the wait became the whole of the latency.** A keystroke that could be
/// answered in 2 ms was being answered in 182, and the 180 was mine.
///
/// Then twenty-five, and then measured again: key to pixels was 31 ms and
/// **25 of them were this**. So it is zero, and the debounce is gone.
///
/// What a debounce buys is fewer wasted queries, and what it costs is every
/// keystroke's latency. That was the right trade at ninety milliseconds a
/// query and is a bad one at two: a search now costs less than the wait did,
/// so waiting to avoid it is spending more than it saves. The generation guard
/// is what makes it safe — a stale reply is dropped whether or not a timer
/// existed.
const DEBOUNCE_MS: u64 = 0;

/// How many rows are fetched at a time.
///
/// The screen holds about twenty. Two hundred was the first number here and
/// every one of them costs a path rebuilt in the engine and six strings
/// allocated in the window, on every keystroke, for rows nobody scrolls to
/// before typing the next letter.
///
/// Sixty is three screens of scrolling with the mouse already moving, which is
/// as far as anyone gets before the list has been replaced anyway. Paging past
/// it is Phase 5.3.
const PAGE: u32 = 60;

struct State {
    generation: u64,
    /// When the keystroke behind the request in flight was typed.
    ///
    /// The only latency that matters is this one — engine time is a fraction
    /// of it and was, for a while, the only part being measured.
    typed_at: Option<std::time::Instant>,
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
    // From the process starting to the first row on screen. The one number a
    // person sees before they have typed anything, and the only one the
    // window's own start-up appears in.
    let launched = std::time::Instant::now();
    let cat = Rc::new(Catalogue::for_language(&language()));
    let window = MainWindow::new().context("the window could not be created")?;
    trace(&format!("window built {:.1?} in", launched.elapsed()));

    let state = Rc::new(RefCell::new(State {
        generation: 0,
        typed_at: None,
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

    let sink = move |got: Got| {
        let _ = slint::invoke_from_event_loop(move || deliver(got));
    };

    let link = Rc::new(Link::start(addr, sink));

    {
        // Registered after the link exists, because answering a search now
        // asks one more question — the facet count that goes with it.
        let link = Rc::clone(&link);
        INBOX.with(|slot| {
            *slot.borrow_mut() = Some(Rc::new(move |got: Got| {
                let Some(w) = weak.upgrade() else { return };
                apply(&w, &ui_state, &ui_rows, &ui_facets, &ui_cat, &link, got);
            }));
        });
    }

    // --- the query line ---------------------------------------------------
    {
        let state = state.clone();
        let link = link.clone();
        let weak = window.as_weak();
        window.on_query_changed(move |text| {
            trace(&format!("query-changed {text:?}"));
            let generation = {
                let mut s = state.borrow_mut();
                s.query = text.to_string();
                s.generation += 1;
                s.typed_at = Some(std::time::Instant::now());
                s.generation
            };
            if let Some(w) = weak.upgrade() {
                w.set_busy(true);
            }
            // Straight out, no timer. At `DEBOUNCE_MS` of zero the wait is
            // the only thing a timer would add, and a stale reply is dropped
            // by generation whether or not one ran.
            if DEBOUNCE_MS == 0 {
                trace(&format!("dispatch {generation}"));
                dispatch(&state, &link);
                return;
            }
            let state = state.clone();
            let link = link.clone();
            let weak = weak.clone();
            slint::Timer::single_shot(std::time::Duration::from_millis(DEBOUNCE_MS), move || {
                if state.borrow().generation != generation {
                    trace(&format!("timer {generation} superseded"));
                    return;
                }
                let _ = weak;
                trace(&format!("dispatch {generation}"));
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

    // A way to exercise the whole pipeline without a keyboard.
    //
    // `SCOUR_SELFTEST=rapor scour-gui` raises `query-changed` exactly as the
    // text field does, which splits the one failure that matters — a keystroke
    // going in and nothing coming out — into the half above the callback and
    // the half below it.
    if let Ok(q) = std::env::var("SCOUR_SELFTEST") {
        // Types the word one character at a time, on the clock, the way a
        // person does — because the number that matters is key to pixels and
        // nothing measurable from outside the window can see it.
        let weak = window.as_weak();
        let chars: Vec<String> = q
            .char_indices()
            .map(|(i, c)| q[..i + c.len_utf8()].to_owned())
            .collect();
        let step = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let timer = std::rc::Rc::new(slint::Timer::default());
        let held = timer.clone();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(150),
            move || {
                let i = step.get();
                let Some(w) = weak.upgrade() else { return };
                match chars.get(i) {
                    Some(prefix) => {
                        w.set_query(prefix.clone().into());
                        w.invoke_query_changed(prefix.clone().into());
                        step.set(i + 1);
                    }
                    None => {
                        let _ = &held;
                        step.set(0);
                    }
                }
            },
        );
        // Kept alive for the life of the window; a dropped `Timer` stops.
        std::mem::forget(timer);
    }

    // The first search is the empty one: everything, newest first, which is
    // what the window should already be showing when it appears.
    trace(&format!("first search sent {:.1?} in", launched.elapsed()));
    dispatch(&state, &link);
    FIRST.with(|f| f.set(Some(launched)));
    window.run().context("the event loop failed")?;
    Ok(())
}

/// Send the search for the current state.
///
/// The facet count is **not** sent here, and that is the fix for the second
/// half of the same problem: it is a sidebar, it costs as much as the search,
/// and sending it beside every search doubled the traffic to answer a question
/// nobody had finished asking. It goes out once the search it belongs to has
/// actually been shown — see [`apply`].
fn dispatch(state: &Rc<RefCell<State>>, link: &Rc<Link>) {
    let (generation, query, sort, descending) = {
        let s = state.borrow();
        (s.generation, full_query(&s), s.sort.clone(), s.descending)
    };
    link.send(Ask::Search {
        generation,
        sort: order_for(&query, &sort),
        query,
        descending,
        limit: PAGE,
    });
}

/// Below how many characters a term stops narrowing anything.
///
/// The trigram filter is built on three-letter keys, so a shorter term hands
/// the walk every row in the index.
const TRIGRAM_MIN: usize = 3;

/// Which order to actually ask for.
///
/// Relevance has to see **every** match before it knows which forty win. That
/// is the right trade at `toki` and a terrible one at `t`, because the two
/// differ by three orders of magnitude in how many rows they match — measured
/// on 2,981,748 entries:
///
/// | | relevance | stored order |
/// |---|---|---|
/// | `t` | 899 ms, full scan | **41 ms** |
/// | `to` | 199 ms, full scan | 80 ms |
/// | `tok` | 65 ms | 62 ms |
///
/// So below the trigram minimum the window asks for the stored order, which
/// stops as soon as it has a page. This is not a compromise on the answer:
/// ranking a million matches of `t` by how well the name answers `t` is noise,
/// and "the most recently changed things with a t in them" is both instant and
/// more use. From three characters on, relevance is asked for and paid for.
///
/// A sort the user chose is never overridden — only the default is.
fn order_for(query: &str, sort: &str) -> String {
    if sort != "relevance" {
        return sort.to_owned();
    }
    let shortest = terms_of(query).iter().map(String::len).min();
    match shortest {
        Some(n) if n < TRIGRAM_MIN => "modified".into(),
        _ => sort.to_owned(),
    }
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
    link: &Rc<Link>,
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
            trace(&format!(
                "reply {generation} (shown {}, current {})",
                state.borrow().shown,
                state.borrow().generation
            ));
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
            let n = fresh.len();
            rows.set_vec(fresh);
            if let Some(t) = FIRST.with(std::cell::Cell::take) {
                trace(&format!(
                    "first rows on screen {:.1?} after launch",
                    t.elapsed()
                ));
            }
            trace(&format!(
                "drew {n} rows {:.1} ms after the key",
                state
                    .borrow()
                    .typed_at
                    .map(|t| t.elapsed().as_secs_f64() * 1000.0)
                    .unwrap_or(0.0)
            ));
            {
                let mut s = state.borrow_mut();
                s.shown = generation;
                s.hits = r.hits;
            }
            w.set_selected(0);
            w.set_busy(false);
            // Now, and only now, the sidebar. A facet count costs about what
            // the search did, and asking for it beside every keystroke doubled
            // the work to answer a question the user had not finished typing.
            // This one belongs to a result already on screen.
            {
                let s = state.borrow();
                let query = full_query(&s);
                link.send(Ask::Facets {
                    generation,
                    query: query.clone(),
                });
                // And the exact total, only if the fast answer was cut short.
                if r.capped {
                    link.send(Ask::Count { generation, query });
                }
            }
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
        // The exact total, which the interactive search deliberately did not
        // stop to compute. It arrives after the list is already on screen, so
        // the meter tightens from `1000+` to a number rather than waiting for
        // one.
        Got::Count { generation, reply } => {
            if generation != state.borrow().generation {
                return;
            }
            if let Response::Count { total, capped } = *reply {
                w.set_meter(
                    format!(
                        "{total}{} {}",
                        if capped { "+" } else { "" },
                        cat.get("matches")
                    )
                    .into(),
                );
            }
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
            typed_at: None,
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
    fn a_term_too_short_to_narrow_is_not_ranked() {
        // Relevance walks every match. At `t` that is a million rows for an
        // ordering nobody can read; at `tok` it is the point of the feature.
        assert_eq!(order_for("t", "relevance"), "modified");
        assert_eq!(order_for("to", "relevance"), "modified");
        assert_eq!(order_for("tok", "relevance"), "relevance");
        // The shortest term decides: one narrow term does not rescue the walk
        // if another is wide open.
        assert_eq!(order_for("rapor t", "relevance"), "modified");
        // A field term is not a name term and does not count either way.
        assert_eq!(order_for("kind:image", "relevance"), "relevance");
        // And a sort somebody asked for out loud is left alone.
        assert_eq!(order_for("t", "size"), "size");
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
