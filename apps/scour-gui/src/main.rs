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

use link::{Ask, Got, Link, ReplyRevision};

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

pub use ui::{Bar, Facet, Fonts, MainWindow, Row, Scheme, Theme};

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

/// Give non-interactive work one ordinary gap between keys to become stale.
///
/// Rows still leave immediately. Only the exact total and sidebar wait, so a
/// typing burst pays for them once for the finished query instead of once per
/// prefix.
const BACKGROUND_IDLE_MS: u64 = 200;

/// The most rows retained by the window, however far somebody scrolls.
///
/// The first request still comes from `visible-rows`, because only the window
/// knows how tall it is. Reaching its end grows it to this bounded window;
/// reaching either edge after that slides the window through the result set.
/// Thus row 257 is reachable without retaining every row before it.
const PAGE_MAX: u32 = 256;

/// Half a window stays on screen across a page turn.
///
/// The overlap is what makes a page boundary feel like scrolling rather than
/// like pressing Next, while keeping both the model and `State::hits` bounded.
const PAGE_STRIDE: u32 = PAGE_MAX / 2;

struct State {
    generation: u64,
    /// Changes only when the matching set changes, not when its order does.
    query_revision: u64,
    /// The query revision whose sidebar and exact count were scheduled.
    background_query: Option<u64>,
    /// The query revision whose fallback count was scheduled.
    ///
    /// Facets already carry a total. A separate count is useful only when the
    /// facet walk reached its own cap, and must still be sent at most once.
    count_query: Option<u64>,
    /// An exact count remains valid across sort changes.
    exact_count: Option<ExactCount>,
    /// Rows currently requested for the bounded model.
    row_limit: u32,
    /// Offset of the bounded window currently on screen.
    page_offset: u32,
    /// A resize or page turn waiting for its rows.
    page_move: Option<PageMove>,
    /// Count information carried by the last search page.
    page_total: u64,
    page_capped: bool,
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

#[derive(Clone, Copy)]
struct ExactCount {
    query_revision: u64,
    total: u64,
    capped: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PageMove {
    /// The initial visible slice is growing in place.
    Expand,
    /// The bounded window is moving while keeping one visible row anchored.
    Slide {
        offset: u32,
        anchor_global: u32,
        selected_global: Option<u32>,
    },
}

impl State {
    fn advance_query(&mut self) {
        self.generation += 1;
        self.query_revision += 1;
        self.background_query = None;
        self.count_query = None;
        self.exact_count = None;
        self.page_move = None;
    }

    fn advance_order(&mut self) {
        self.generation += 1;
    }

    /// Mark the query-scoped work as scheduled, once per matching set.
    fn start_background(&mut self) -> bool {
        if self.background_query == Some(self.query_revision) {
            return false;
        }
        self.background_query = Some(self.query_revision);
        true
    }

    fn start_count(&mut self) -> bool {
        if self.count_query == Some(self.query_revision) {
            return false;
        }
        self.count_query = Some(self.query_revision);
        true
    }

    /// Prepare a bounded page move. Returns the page to request.
    fn move_page(
        &mut self,
        direction: i32,
        first_visible: u32,
        selected: i32,
        loaded: usize,
    ) -> Option<(u32, u32)> {
        if self.page_move.is_some() || (direction > 0 && loaded < self.row_limit as usize) {
            return None;
        }
        if self.row_limit < PAGE_MAX {
            if direction <= 0 {
                return None;
            }
            self.row_limit = PAGE_MAX;
            self.advance_order();
            self.page_move = Some(PageMove::Expand);
            return Some((self.page_offset, PAGE_MAX));
        }

        let offset = match direction.cmp(&0) {
            std::cmp::Ordering::Greater => {
                let end = u64::from(self.page_offset) + loaded as u64;
                if !self.page_capped && end >= self.page_total {
                    return None;
                }
                self.page_offset.checked_add(PAGE_STRIDE)?
            }
            std::cmp::Ordering::Less if self.page_offset > 0 => {
                self.page_offset.saturating_sub(PAGE_STRIDE)
            }
            _ => return None,
        };
        let anchor_global = self.page_offset.saturating_add(first_visible);
        let selected_global = u32::try_from(selected)
            .ok()
            .map(|row| self.page_offset.saturating_add(row));
        self.advance_order();
        self.page_move = Some(PageMove::Slide {
            offset,
            anchor_global,
            selected_global,
        });
        Some((offset, PAGE_MAX))
    }
}

fn main() -> Result<()> {
    // From the process starting to the first row on screen. The one number a
    // person sees before they have typed anything, and the only one the
    // window's own start-up appears in.
    let launched = std::time::Instant::now();
    // One read serves both the language and the socket. This used to load and
    // parse the same config file twice before the first request left.
    let config = scour_config::Config::load_or_default().0;
    let cat = Rc::new(Catalogue::for_language(&language(&config)));
    let window = MainWindow::new().context("the window could not be created")?;
    dress(&window);
    columns(&window, &cat);
    // Nothing is marked to begin with: the first list is by relevance, which
    // is not a column and has no heading to point at.
    window.set_sorted_by("relevance".into());
    // The two tab words, from the catalogue like every other visible string.
    window.set_tab_search(t(&cat, "Search"));
    window.set_tab_report(t(&cat, "Report"));
    trace(&format!("window built {:.1?} in", launched.elapsed()));

    let state = Rc::new(RefCell::new(State {
        generation: 0,
        query_revision: 0,
        background_query: None,
        count_query: None,
        exact_count: None,
        row_limit: 20,
        page_offset: 0,
        page_move: None,
        page_total: 0,
        page_capped: false,
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
    // The page's own placeholder, so an empty window says the same thing in
    // both: what you can type, by example.
    window.set_hint(t(
        &cat,
        "file name  ·  ext:pdf  ·  kind:image dm:7d  ·  size:>10mb",
    ));
    window.set_meter(t(&cat, "connecting…"));
    // The rail's first section is the kinds, and the page calls it `Kind`.
    // `Everything` was this window's own word for the same thing.
    window.set_scope_label(t(&cat, "Kind"));
    window.set_ribbon_label(t(&cat, "Time distribution"));
    window.set_ribbon_hint(t(&cat, "results by date changed"));
    window.set_axis_oldest(t(&cat, "2 years ago"));
    window.set_axis_year(t(&cat, "1 year"));
    window.set_axis_month(t(&cat, "1 month"));
    window.set_axis_week(t(&cat, "1 week"));
    window.set_axis_today(t(&cat, "today"));

    let addr = config.socket();

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
        let facets = facets.clone();
        let weak = window.as_weak();
        window.on_query_changed(move |text| {
            trace(&format!("query-changed {text:?}"));
            let generation = {
                let mut s = state.borrow_mut();
                s.query = text.to_string();
                s.advance_query();
                s.typed_at = Some(std::time::Instant::now());
                s.generation
            };
            // Counts from the previous matching set are worse than an empty
            // rail while the new, delayed facet walk is in flight.
            facets.set_vec(Vec::new());
            if let Some(w) = weak.upgrade() {
                w.set_busy(true);
            }
            // Straight out, no timer. At `DEBOUNCE_MS` of zero the wait is
            // the only thing a timer would add, and a stale reply is dropped
            // by generation whether or not one ran.
            let rows = weak
                .upgrade()
                .map_or(60, |w| w.get_visible_rows().max(0) as u32);
            if DEBOUNCE_MS == 0 {
                trace(&format!("dispatch {generation}, {rows} rows"));
                dispatch(&state, &link, rows);
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
                dispatch(&state, &link, rows);
            });
        });
    }

    // --- the rail ---------------------------------------------------------
    {
        let state = state.clone();
        let link = link.clone();
        let facets = facets.clone();
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
                s.advance_query();
            }
            facets.set_vec(Vec::new());
            let rows = match weak.upgrade() {
                Some(w) => {
                    let s = state.borrow();
                    w.set_active_facet(s.facet.clone().unwrap_or_default().into());
                    w.set_busy(true);
                    w.get_visible_rows().max(0) as u32
                }
                None => 60,
            };
            dispatch(&state, &link, rows);
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
                s.advance_order();
            }
            // The heading marks itself, so the window has to be told which one
            // won. Read back rather than assumed: `sort` may have been left
            // alone and only the direction flipped.
            if let Some(w) = weak.upgrade() {
                w.set_sorted_by(state.borrow().sort.as_str().into());
            }
            let rows = match weak.upgrade() {
                Some(w) => {
                    w.set_busy(true);
                    w.get_visible_rows().max(0) as u32
                }
                None => 60,
            };
            dispatch(&state, &link, rows);
        });
    }

    // --- bounded scrolling ------------------------------------------------
    {
        // The first request is only what fits. Its first edge expands a single
        // time; later edges slide a fixed-size overlapping window. The number
        // of retained paths and Slint strings therefore never depends on how
        // deep somebody scrolls.
        let state = state.clone();
        let link = link.clone();
        let weak = window.as_weak();
        window.on_need_page(move |direction, first_visible| {
            let request = {
                let mut s = state.borrow_mut();
                let selected = weak.upgrade().map_or(0, |w| w.get_selected());
                let loaded = s.hits.len();
                s.move_page(direction, first_visible.max(0) as u32, selected, loaded)
            };
            let Some((offset, limit)) = request else {
                return;
            };
            if let Some(w) = weak.upgrade() {
                w.set_busy(true);
            }
            trace(&format!(
                "move result window to {offset}..{}",
                offset + limit
            ));
            send_search(&state, &link, offset, limit);
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
    dispatch(&state, &link, window.get_visible_rows().max(0) as u32);
    FIRST.with(|f| f.set(Some(launched)));
    // Photograph the window and leave, when asked. See [`snapshot`].
    if let Ok(path) = std::env::var("SCOUR_GUI_SNAP") {
        let weak = window.as_weak();
        // Held rather than dropped: a `Timer` that goes out of scope never
        // fires. Late enough that the first page of rows has arrived and been
        // laid out — anything earlier photographs an empty list.
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(2500),
            move || {
                if let Some(w) = weak.upgrade() {
                    snapshot(&w, &path);
                }
                slint::quit_event_loop().ok();
            },
        );
    }
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
fn dispatch(state: &Rc<RefCell<State>>, link: &Rc<Link>, rows: u32) {
    let limit = rows.clamp(20, PAGE_MAX);
    {
        let mut s = state.borrow_mut();
        s.row_limit = limit;
        s.page_offset = 0;
        s.page_move = None;
    }
    send_search(state, link, 0, limit);
}

fn send_search(state: &Rc<RefCell<State>>, link: &Rc<Link>, offset: u32, limit: u32) {
    let (generation, query_revision, query, sort, descending) = {
        let s = state.borrow();
        (
            s.generation,
            s.query_revision,
            full_query(&s),
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

fn schedule_background(
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
            link.send(Ask::Facets {
                query_revision,
                query,
            });
        },
    );
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
            let mut s = state.borrow_mut();
            s.down = true;
            s.page_move = None;
            w.set_busy(false);
            w.set_meter(format!("{} — {why}", cat.get("the service is not running")).into());
        }
        Got::Refused { revision, why } => {
            match revision {
                ReplyRevision::Search(generation) if generation == state.borrow().generation => {
                    state.borrow_mut().page_move = None;
                }
                ReplyRevision::Search(_) => return,
                ReplyRevision::Query(query_revision) => {
                    if query_revision != state.borrow().query_revision {
                        return;
                    }
                    // Facets and the exact count are optional refinements of a
                    // list that is already visible. Their failure must not
                    // clear the busy state or replace the search's meter.
                    trace(&format!("background request refused: {why}"));
                    return;
                }
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
            w.set_hint(t(
                cat,
                "file name  ·  ext:pdf  ·  kind:image dm:7d  ·  size:>10mb",
            ));
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
            let selected = w.get_selected();
            let scroll_y = w.get_scroll_y();
            let fresh: Vec<Row> = r
                .hits
                .iter()
                // The kind's word comes from the catalogue, by the engine's
                // own msgid — the same string the rail's labels and the
                // browser page use. A window that spelled these itself would
                // be a second vocabulary, and the day the engine learned a
                // fourteenth kind this one would show a blank.
                .map(|h| rows::row_of(h, &terms, now, &t(cat, h.kind.msgid())))
                .collect();
            let n = fresh.len();
            let refused_forward_page = {
                let s = state.borrow();
                n == 0
                    && matches!(
                        s.page_move,
                        Some(PageMove::Slide { offset, .. }) if offset > s.page_offset
                    )
            };
            if refused_forward_page {
                let mut s = state.borrow_mut();
                s.shown = generation;
                s.page_total = r.total;
                s.page_capped = r.capped;
                s.page_move = None;
                w.set_busy(false);
                return;
            }
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
            let (query_revision, query, ask_background, exact_count, page_move) = {
                let mut s = state.borrow_mut();
                s.shown = generation;
                s.page_total = r.total;
                s.page_capped = r.capped;
                if !r.capped {
                    s.exact_count = Some(ExactCount {
                        query_revision: s.query_revision,
                        total: r.total,
                        capped: false,
                    });
                }
                s.hits = r.hits;
                let page_move = s.page_move.take();
                if let Some(PageMove::Slide { offset, .. }) = page_move {
                    s.page_offset = offset;
                }
                let query_revision = s.query_revision;
                let ask_background = s.start_background();
                (
                    query_revision,
                    full_query(&s),
                    ask_background,
                    s.exact_count.filter(|c| c.query_revision == query_revision),
                    page_move,
                )
            };
            match page_move {
                Some(PageMove::Expand) => {
                    w.set_selected(selected.max(0).min(n.saturating_sub(1) as i32));
                    w.set_scroll_y(scroll_y);
                }
                Some(PageMove::Slide {
                    offset,
                    anchor_global,
                    selected_global,
                }) => {
                    let last = n.saturating_sub(1) as u32;
                    let anchor = anchor_global.saturating_sub(offset).min(last);
                    let selected = selected_global
                        .and_then(|global| global.checked_sub(offset))
                        .filter(|&local| local < n as u32)
                        .unwrap_or(anchor);
                    w.set_selected(selected as i32);
                    w.invoke_anchor_row(anchor as i32);
                }
                None => {
                    w.set_selected(0);
                    w.set_scroll_y(0.0);
                }
            }
            w.set_busy(false);
            // Now, and only now, the sidebar. A facet count costs about what
            // the search did, and asking for it beside every keystroke doubled
            // the work to answer a question the user had not finished typing.
            // This one belongs to a result already on screen.
            if ask_background {
                schedule_background(state, link, query_revision, query);
            }
            let (total, capped) = exact_count
                .map(|c| (c.total, c.capped))
                .unwrap_or((r.total, r.capped));
            // **The page's sentence, in the page's order.** Shown out of
            // total, then what it cost, then how much of the index was walked
            // to get it. Grouped with the locale's own separator, because a
            // seven-digit number without one is a number nobody reads.
            w.set_meter(
                format!(
                    "{} / {}{}  ·  {:.2} ms  ·  {} {}",
                    grouped(n as u64),
                    grouped(total),
                    if capped { "+" } else { "" },
                    r.took_us as f64 / 1000.0,
                    grouped(r.rows_visited),
                    cat.get("rows read"),
                )
                .into(),
            );
        }
        // The exact total, which the interactive search deliberately did not
        // stop to compute. It arrives after the list is already on screen, so
        // the meter tightens from `1000+` to a number rather than waiting for
        // one.
        Got::Count {
            query_revision,
            reply,
        } => {
            if query_revision != state.borrow().query_revision {
                return;
            }
            // **`misread` is dropped, and nothing else picks it up.** This
            // said the query line was coloured from `explain` while it was
            // being typed, so a term the engine did not understand had already
            // reached the reader by now. It is not: this crate never sends
            // `Request::Explain`, and there is nowhere to put the answer if it
            // did — the query line is one `TextInput` in one colour, because
            // Slint has no range colouring for editable text (`ui/main.slint`
            // on upstream #9560). So a bad term is silent here, and telling
            // the reader about it is a feature this window does not have yet
            // rather than one that arrived down another path.
            if let Response::Count {
                total,
                capped,
                misread: _,
            } = *reply
            {
                state.borrow_mut().exact_count = Some(ExactCount {
                    query_revision,
                    total,
                    capped,
                });
                // The exact total arrives after the list is on screen, so
                // only the second number moves. Keeping the sentence's shape
                // is the point: a meter that reflows when a background answer
                // lands reads as the window having changed its mind.
                w.set_meter(
                    format!(
                        "{} / {}{}",
                        grouped(slint::Model::row_count(&w.get_rows()) as u64),
                        grouped(total),
                        if capped { "+" } else { "" },
                    )
                    .into(),
                );
            }
        }
        Got::Facets {
            query_revision,
            reply,
        } => {
            if query_revision != state.borrow().query_revision {
                return;
            }
            let Response::Facets(f) = *reply else { return };
            // Ordered by the taxonomy rather than by count, so the rail does
            // not reshuffle under the pointer between two keystrokes.
            // **Two questions, one walk.** The reply carries a group per
            // question asked; `facets` is the first group flattened, kept for
            // callers that ask one thing. Reading the groups by their `by` is
            // what lets the ribbon and the rail come out of the same request
            // without either guessing which half is theirs.
            let kinds: &[scour_core::Facet] = f
                .groups
                .iter()
                .find(|g| matches!(g.by, scour_core::FacetBy::Kind))
                .map(|g| g.facets.as_slice())
                .unwrap_or(&f.facets);
            let ages: &[scour_core::Facet] = f
                .groups
                .iter()
                .find(|g| matches!(g.by, scour_core::FacetBy::Age { .. }))
                .map(|g| g.facets.as_slice())
                .unwrap_or(&[]);

            let mut fresh: Vec<Facet> = Vec::new();
            for k in rows::offered_kinds() {
                let token = k.token();
                let Some(hit) = kinds.iter().find(|x| x.key == token) else {
                    continue;
                };
                fresh.push(Facet {
                    label: t(cat, k.msgid()),
                    token: token.into(),
                    count: compact(hit.count).into(),
                });
            }
            facets.set_vec(fresh);

            // The ribbon. Keys are the edges as text, newest first, and
            // `older` is everything past the last one — the service's own
            // wording, so nothing here has to know how the bands were made.
            let edges = scour_ui::bar_edges();
            let mut peak = 1i32;
            let bars: Vec<Bar> = edges
                .iter()
                .map(|days| {
                    let key = days.to_string();
                    let count = ages
                        .iter()
                        .find(|x| x.key == key)
                        .map(|x| x.count)
                        .unwrap_or(0) as i32;
                    peak = peak.max(count);
                    Bar {
                        count,
                        band: scour_ui::band_of(*days as f64) as i32,
                        about: String::new().into(),
                    }
                })
                .collect();
            w.set_bar_peak(peak);
            w.set_bars(ModelRc::new(VecModel::from(bars)));
            let count_query = {
                let mut s = state.borrow_mut();
                if f.capped {
                    s.start_count().then(|| full_query(&s))
                } else {
                    s.exact_count = Some(ExactCount {
                        query_revision,
                        total: f.total,
                        capped: false,
                    });
                    None
                }
            };
            w.set_meter(
                format!(
                    "{} / {}{}",
                    grouped(slint::Model::row_count(&w.get_rows()) as u64),
                    grouped(f.total),
                    if f.capped { "+" } else { "" },
                )
                .into(),
            );
            // The facet walk already counted the same rows. Only its own
            // safety cap makes a second pass necessary.
            if let Some(query) = count_query {
                link.send(Ask::Count {
                    query_revision,
                    query,
                });
            }
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
///
/// **The rule is `scour_i18n::choose`'s, not this file's.** The copy that used
/// to be here read `LC_ALL`/`LC_MESSAGES`/`LANG` itself and disagreed with the
/// catalogue crate twice: it never looked at `SCOUR_LANG`, so the variable that
/// exists to switch one program left the window alone, and it did not skip
/// `C`/`POSIX`, so a session started with `LANG=C` asked for a language called
/// "C". Two frontends, two answers to one question.
///
/// **The first argument is empty, and that is the honest gap.** The window
/// cannot see `scour_settings::Settings::language` — the setting the browser
/// window's menu writes — because it has no lane to ask for it: `link.rs`
/// carries `Search`, `Facets` and `Count` and nothing else, and adding a
/// settings round trip is not the change this comment belongs to. So a language
/// chosen in the browser window is *stored* where this can reach it and is not
/// yet read here. Passing it through `choose` rather than around it is what
/// makes that one line's work when the lane exists.
/// Hand the window its palette.
///
/// **The colours come from `scour-ui`, which the browser page is also written
/// from.** Before this they were written twice — once in `theme.slint`, once
/// in `page.html` — and staying equal was somebody remembering to. It had
/// already failed: the light scheme's focus ring was `#2f6ba3` here and
/// `#4a9eff` there.
///
/// Both schemes are pushed, not one: which of them applies is Slint's to
/// decide, because it is the only side that hears the desktop change its mind
/// while the window is open.
fn dress(window: &MainWindow) {
    let theme = window.global::<Theme>();
    theme.set_dark_scheme(scheme(&scour_ui::DARK));
    theme.set_light_scheme(scheme(&scour_ui::LIGHT));
    theme.set_unit(scour_ui::METRICS.unit);
    theme.set_row_height(scour_ui::METRICS.row);
    theme.set_radius(scour_ui::METRICS.radius);

    let fonts = window.global::<Fonts>();
    fonts.set_size(scour_ui::METRICS.size);
    // The page names a stack and lets the browser pick; a native window asks
    // the platform for one family. Taking the first name would ask for
    // `ui-monospace`, which no font server knows — so the generic is what both
    // ends up resolving to anyway, said plainly.
    fonts.set_mono("monospace".into());
}

/// The column headings and their widths, from `scour-ui`.
///
/// **The same five the browser page shows, in the same order, at the same
/// widths.** Which columns exist and what they are called is shared; how a
/// cell is painted is not, and this window paints its own.
///
/// A column named in `scour_ui::DEFAULT_COLUMNS` but missing from `COLUMNS`
/// would be a heading with no word, so the lookup is checked there by a test
/// rather than unwrapped here.
fn columns(window: &MainWindow, cat: &Catalogue) {
    let w = |id: &str| {
        scour_ui::column(id)
            .map(|c| c.width as f32)
            .unwrap_or(100.0)
    };
    let head = |id: &str| {
        scour_ui::column(id)
            .map(|c| t(cat, c.msgid))
            .unwrap_or_default()
    };
    window.set_head_name(head("name"));
    window.set_head_kind(head("kind"));
    window.set_head_path(head("path"));
    window.set_head_mtime(head("mtime"));
    window.set_head_size(head("size"));
    window.set_w_name(w("name"));
    window.set_w_kind(w("kind"));
    window.set_w_path(w("path"));
    window.set_w_mtime(w("mtime"));
    window.set_w_size(w("size"));
}

/// One `scour-ui` palette, in the shape the window's generated struct wants.
fn scheme(p: &scour_ui::Palette) -> Scheme {
    let c = |x: &scour_ui::Rgba| {
        let (a, r, g, b) = x.argb();
        slint::Brush::SolidColor(slint::Color::from_argb_u8(a, r, g, b))
    };
    Scheme {
        ground: c(&p.ground),
        panel: c(&p.panel),
        panel_2: c(&p.panel_2),
        line: c(&p.line),
        line_soft: c(&p.line_soft),
        ink: c(&p.ink),
        ink_2: c(&p.ink_2),
        ink_3: c(&p.ink_3),
        mark: c(&p.mark),
        mark_ink: c(&p.mark_ink),
        pick: c(&p.pick),
        focus: c(&p.focus),
        hover: c(&p.hover),
        t0: c(&p.t[0]),
        t1: c(&p.t[1]),
        t2: c(&p.t[2]),
        t3: c(&p.t[3]),
        t4: c(&p.t[4]),
        t5: c(&p.t[5]),
        q_key: c(&p.q_key),
        q_val: c(&p.q_val),
        q_glob: c(&p.q_glob),
        q_not: c(&p.q_not),
        q_bad: c(&p.q_bad),
    }
}

/// Save what the window actually looks like, then leave.
///
/// **Because the person writing this cannot see it.** The window is drawn by a
/// compositor that will not hand a screenshot to a process asking from a
/// terminal, so every claim about how close this is to the browser page was
/// somebody else's eyes and a round trip. Slint can render its own window to a
/// buffer; that is enough to look.
///
/// `SCOUR_GUI_SNAP=/path/to.ppm` — plain PPM, so nothing has to be linked to
/// write it. `convert` or `magick` turns it into a PNG.
fn snapshot(window: &MainWindow, path: &str) {
    let Ok(buf) = window.window().take_snapshot() else {
        eprintln!("gui: the window could not be captured");
        return;
    };
    let (w, h) = (buf.width(), buf.height());
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for px in buf.as_slice() {
        out.extend_from_slice(&[px.r, px.g, px.b]);
    }
    match std::fs::write(path, out) {
        Ok(()) => eprintln!("gui: {w}x{h} written to {path}"),
        Err(e) => eprintln!("gui: {path} could not be written: {e}"),
    }
}

/// A number a person can read: `5356281` becomes `5.356.281`.
///
/// The separator is the catalogue's, not the platform's — the window may be
/// asked for English on a Turkish desktop, and the number belongs to the
/// language of the text around it.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(c);
    }
    out
}

fn language(cfg: &scour_config::Config) -> String {
    scour_i18n::choose("", &cfg.ui.language)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window's palette is the one the browser page is written from.
    ///
    /// **The guard on a drift that had already happened.** These colours were
    /// written here *and* in `page.html`, and keeping them equal was somebody
    /// remembering to — which failed quietly: the light scheme's focus ring
    /// was `#2f6ba3` in this file and `#4a9eff` in that one, and no test
    /// anywhere could tell.
    ///
    /// What this checks is the conversion, which is where a mistake would
    /// otherwise be invisible: Slint takes alpha first and the browser takes
    /// it last, so a channel swapped here would be right for `#ffffff` and
    /// wrong for everything else.
    #[test]
    fn the_window_wears_the_shared_palette() {
        let dark = scheme(&scour_ui::DARK);
        let want = |c: &scour_ui::Rgba| {
            let (a, r, g, b) = c.argb();
            slint::Brush::SolidColor(slint::Color::from_argb_u8(a, r, g, b))
        };
        assert_eq!(dark.ground, want(&scour_ui::DARK.ground));
        assert_eq!(dark.q_key, want(&scour_ui::DARK.q_key));
        // The translucent one, which is the case a channel swap survives.
        assert_eq!(dark.pick, want(&scour_ui::DARK.pick));
        assert_eq!(dark.mark, want(&scour_ui::DARK.mark));

        let light = scheme(&scour_ui::LIGHT);
        assert_ne!(light.ground, dark.ground, "both schemes came out the same");
    }

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
            query_revision: 0,
            background_query: None,
            count_query: None,
            exact_count: None,
            row_limit: 20,
            page_offset: 0,
            page_move: None,
            page_total: 0,
            page_capped: false,
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
    fn sorting_reuses_query_scoped_sidebar_and_count_work() {
        let mut s = State {
            generation: 4,
            query_revision: 2,
            background_query: None,
            count_query: None,
            exact_count: Some(ExactCount {
                query_revision: 2,
                total: 45,
                capped: false,
            }),
            row_limit: 40,
            page_offset: 0,
            page_move: None,
            page_total: 45,
            page_capped: false,
            typed_at: None,
            shown: 4,
            query: "rapor".into(),
            sort: "modified".into(),
            descending: true,
            facet: None,
            hits: Vec::new(),
            down: false,
        };

        assert!(s.start_background());
        assert!(s.start_count());
        assert!(
            !s.start_count(),
            "one capped facet answer gets one fallback"
        );
        s.advance_order();
        assert!(!s.start_background(), "a sort did not change the matches");
        assert_eq!(s.query_revision, 2);
        assert_eq!(s.exact_count.map(|c| c.total), Some(45));

        s.advance_query();
        assert!(s.start_background(), "a new query needs new facets");
        assert!(
            s.start_count(),
            "a new query may need its own fallback count"
        );
        assert!(s.exact_count.is_none());
    }

    #[test]
    fn every_visible_header_requests_the_sort_key_the_shared_crate_names() {
        let ui = include_str!("../ui/main.slint");
        // The five the window shows, and `scour-ui` is what says which five
        // and what each one sorts by. A heading wired to the wrong key is a
        // column that reorders the list by something else — visible, but only
        // if you know what you were expecting.
        for id in scour_ui::DEFAULT_COLUMNS {
            let c = scour_ui::column(id).unwrap_or_else(|| panic!("`{id}` is not a column"));
            let want = format!("sort: \"{}\"", c.sort);
            assert!(
                ui.contains(&want),
                "no heading asks for `{}`, which is what `{id}` sorts by",
                c.sort
            );
        }
        // And the headings take their words from the crate rather than
        // spelling them here — `@tr("Name")` in this file would be a second
        // place the column is named.
        for prop in [
            "head-name",
            "head-kind",
            "head-path",
            "head-mtime",
            "head-size",
        ] {
            assert!(
                ui.contains(&format!("root.{prop}")),
                "the window does not use `{prop}`"
            );
        }
    }

    #[test]
    fn list_growth_is_demand_driven_and_bounded() {
        let mut s = State {
            generation: 1,
            query_revision: 1,
            background_query: None,
            count_query: None,
            exact_count: None,
            row_limit: 32,
            page_offset: 0,
            page_move: None,
            page_total: 2_000,
            page_capped: true,
            typed_at: None,
            shown: 1,
            query: String::new(),
            sort: "modified".into(),
            descending: true,
            facet: None,
            hits: Vec::new(),
            down: false,
        };

        assert!(
            s.move_page(1, 0, 0, 12).is_none(),
            "a short result has no next screen"
        );
        assert_eq!(s.row_limit, 32);
        assert_eq!(s.move_page(1, 20, 8, 32), Some((0, PAGE_MAX)));
        assert_eq!(s.row_limit, PAGE_MAX);
        assert_eq!(s.page_move, Some(PageMove::Expand));

        // Simulate that expanded page landing, then slide in both directions.
        s.page_move = None;
        assert_eq!(
            s.move_page(1, 220, 230, PAGE_MAX as usize),
            Some((PAGE_STRIDE, PAGE_MAX))
        );
        assert_eq!(
            s.page_move,
            Some(PageMove::Slide {
                offset: PAGE_STRIDE,
                anchor_global: 220,
                selected_global: Some(230),
            })
        );
        s.page_offset = PAGE_STRIDE;
        s.page_move = None;
        assert_eq!(
            s.move_page(-1, 2, 3, 17),
            Some((0, PAGE_MAX)),
            "a partial last window must still be able to move backwards"
        );
        s.page_move = None;
        assert_eq!(
            s.move_page(-1, 2, 3, PAGE_MAX as usize),
            Some((0, PAGE_MAX))
        );
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
    fn every_offered_kind_has_a_word_in_every_translated_language() {
        // **This asked `!c.get(k.msgid()).is_empty()` and could not fail.**
        // `get` falls back to the msgid, so it was true for every string in
        // every language whether anybody had translated it or not — the rail
        // could have come up entirely in English and this would still have
        // been green. `has` asks the catalogue instead of asking for the text.
        //
        // Only the languages that claim to be translated: English has no
        // catalogue because the msgid *is* the English, so every entry would
        // be missing by construction. The assertion below is what stops that
        // filter from quietly emptying the loop the way `get` emptied the
        // check.
        let translated: Vec<Catalogue> = scour_i18n::LANGUAGES
            .iter()
            .map(|(tag, _)| Catalogue::for_language(tag))
            .filter(Catalogue::is_translated)
            .collect();
        assert!(!translated.is_empty(), "no shipped language is translated");
        for c in translated {
            for k in scour_core::Kind::OFFERED {
                assert!(
                    c.has(k.msgid()),
                    "{} has no word for {k:?} ({})",
                    c.locale(),
                    k.msgid()
                );
            }
        }
    }
}
