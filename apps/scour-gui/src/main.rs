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

pub use ui::{Bar, Facet, Fonts, MainWindow, Row, Scheme, Span, Theme};

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

/// How long to leave the index alone between one change and asking about the
/// next. See the note where it is used.
const AWAIT_AGAIN: std::time::Duration = std::time::Duration::from_millis(250);

/// How long the list has to have been still before a page is re-read.
///
/// Re-reading is for a page the index has moved under, and the index moves
/// constantly while anything is being scanned. Missing pages are never held
/// back by this — only the ones that are already on screen and merely a moment
/// out of date.
const SETTLED: std::time::Duration = std::time::Duration::from_millis(500);

/// What a page has to cost before the next one is guessed at, in microseconds.
///
/// A page is a walk of the whole matching set above it: 1 ms near the top of
/// this index, 17 ms at a hundred thousand rows, 125 ms at two and a half
/// million. Reading one ahead of the eye is what keeps scrolling from waiting,
/// and it is only worth it while the answer is cheap enough that a wrong guess
/// costs nothing anybody notices.
const CHEAP_PAGE_US: u64 = 20_000;

/// Rows in a page, which is the unit every request after the first asks for.
///
/// The list itself is as long as the result — the view is told the real total
/// and asks for the rows it is about to draw — so this is only how the rows
/// arrive. It is [`rows::SPAN`], because pages are kept and found again by
/// dividing, and that only works if they all line up.
const PAGE_MAX: u32 = rows::SPAN as u32;

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
    /// How many rows a page asks for.
    row_limit: u32,
    /// Where the page on screen begins in the whole result.
    page_offset: u32,
    /// When the page now in flight was asked for, for the trace.
    page_sent: Option<std::time::Instant>,
    /// When a page was last asked for.
    ///
    /// Read to decide whether there is time to re-read a page the index has
    /// moved under. During a scan it moves several times a second, and a list
    /// that re-read the page under the pointer every time would spend a drag
    /// fetching the same rows.
    asked_at: Option<std::time::Instant>,
    /// What the last page cost the service, in microseconds.
    ///
    /// Read to decide whether guessing at the next one is worth it: a page is
    /// a walk of everything above it, so at the bottom of a long result a
    /// guess is expensive and a wrong guess is wasted.
    page_cost_us: u64,
    /// A question was asked whose answer belongs at the top of the list.
    ///
    /// A new query, a new sort, a rail press. Not a page fetch and not the
    /// live refresh, which are the same question asked again and must leave
    /// the eye where it is.
    rewind: bool,
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
    /// The index revision this window has already seen. The long poll waits
    /// for anything past it.
    revision: u64,
}

#[derive(Clone, Copy)]
struct ExactCount {
    query_revision: u64,
    total: u64,
    capped: bool,
}

impl State {
    fn advance_query(&mut self) {
        self.generation += 1;
        self.query_revision += 1;
        self.background_query = None;
        self.count_query = None;
        self.exact_count = None;
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
}

/// What the selection bar says, in the catalogue's words and the reader's
/// order.
///
/// Folders are counted, never weighed. What a folder holds is the `~` number
/// in the size column — the part of it this index has — and adding that into a
/// total beside exact file sizes would make one number out of two different
/// kinds of claim.
fn picked_line(cat: &Catalogue, picks: &std::collections::BTreeMap<usize, rows::Pick>) -> String {
    let dirs = picks.values().filter(|p| p.is_dir).count();
    let files = picks.len() - dirs;
    let bytes: u64 = picks
        .values()
        .filter(|p| !p.is_dir)
        .map(|p| p.bytes.max(0) as u64)
        .sum();
    let mut parts = vec![t(cat, "{n} selected").replace("{n}", &grouped(picks.len() as u64))];
    if files > 0 {
        parts.push(compact_bytes(bytes));
    }
    if dirs > 0 {
        parts.push(t(cat, "{n} folders").replace("{n}", &grouped(dirs as u64)));
    }
    parts.join("  ·  ")
}

/// The folders a selection sits in, each one once.
///
/// Eleven files from the same directory is one window, not eleven — and that
/// is the ordinary shape of a selection, because a search that found them
/// together usually found them together somewhere.
fn folders_of(picks: &std::collections::BTreeMap<usize, rows::Pick>) -> Vec<String> {
    let mut seen: Vec<String> = picks.values().map(|p| p.folder().to_string()).collect();
    seen.sort();
    seen.dedup();
    seen
}

/// Put the selection on screen: the sentence, the buttons, and the rows.
fn show_picks(
    w: &MainWindow,
    cat: &Catalogue,
    rows: &Rc<rows::Rows>,
    picks: &std::collections::BTreeMap<usize, rows::Pick>,
    asking: bool,
) {
    w.set_picked(picks.len() as i32);
    w.set_picked_line(picked_line(cat, picks).into());
    w.set_pick_copy(t(cat, "Copy the paths"));
    w.set_pick_drop(t(cat, "Drop it"));
    w.set_pick_asking(asking);
    let folders = folders_of(picks).len();
    w.set_pick_folders(
        if asking {
            t(cat, "Open {n} windows?").replace("{n}", &grouped(folders as u64))
        } else {
            t(cat, "Open their folders ({n})").replace("{n}", &grouped(folders as u64))
        }
        .into(),
    );
    let marked: std::collections::HashSet<String> =
        picks.values().map(|p| p.path.clone()).collect();
    rows.mark_picked(&marked);
}

/// The path of the row the list calls `i`, if that row is in hand.
fn path_of(rows: &Rc<rows::Rows>, i: i32) -> Option<String> {
    rows.path_at(usize::try_from(i).ok()?)
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
        page_sent: None,
        asked_at: None,
        page_cost_us: 0,
        rewind: true,
        typed_at: None,
        shown: 0,
        query: String::new(),
        sort: "relevance".into(),
        descending: true,
        facet: None,
        hits: Vec::new(),
        revision: 0,
        down: false,
    }));

    let rows: Rc<rows::Rows> = Rc::new(rows::Rows::default());
    // **What is selected, by path.** By path and not by row number, because a
    // row number is a place in a result that moves under it: the index changes,
    // the sort changes, and the fourth row is a different file. A selection is
    // of files.
    let picks: Rc<RefCell<std::collections::BTreeMap<usize, rows::Pick>>> =
        Rc::new(RefCell::new(std::collections::BTreeMap::new()));
    // Where the last press was, so `Shift` has a run to take.
    let anchor: Rc<std::cell::Cell<i32>> = Rc::new(std::cell::Cell::new(0));
    // The same rows, a line at a time, for the tile views. It reads the model
    // above rather than holding anything of its own.
    let lines: Rc<rows::Lines> = Rc::new(rows::Lines::new(Rc::clone(&rows)));
    let facets: Rc<VecModel<Facet>> = Rc::new(VecModel::default());
    window.set_rows(ModelRc::from(rows.clone()));
    window.set_lines(ModelRc::from(lines.clone()));
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
    window.set_scope_heading(t(&cat, "Scope"));
    window.set_size_heading(t(&cat, "Size"));
    // Three bands, the page's own: what is taking the room, and what is empty.
    // Fixed rather than counted — a count here would cost a walk per band for
    // a filter people apply, look at, and drop.
    let sizes: Vec<Facet> = [
        (">10 MB", "size:>10mb"),
        (">1 MB", "size:>1mb"),
        ("= 0", "size:=0"),
    ]
    .iter()
    .map(|(label, token)| Facet {
        label: (*label).into(),
        token: (*token).into(),
        count: slint::SharedString::new(),
        share: 0.0,
    })
    .collect();
    window.set_sizes(ModelRc::new(VecModel::from(sizes)));
    window.set_ribbon_label(t(&cat, "Time distribution"));
    window.set_help_title(t(&cat, "Help"));
    window.set_lang_title(t(&cat, "language"));
    window.set_rules_title(t(&cat, "What is skipped"));
    // The help is the page's own opening paragraph — what a person can type —
    // rather than a second explanation written for this window.
    window.set_help_body(t(
        &cat,
        "A word on its own matches the name. Put <code>!</code> in front of any term to exclude it, and write several to mean all of them at once.",
    ));
    // The two languages the catalogue has. `""` is "whatever the desktop
    // says", which is what the config file means by an empty string.
    let langs: Vec<Facet> = [("English", "en"), ("Türkçe", "tr")]
        .iter()
        .map(|(label, tag)| Facet {
            label: (*label).into(),
            token: (*tag).into(),
            count: slint::SharedString::new(),
            share: 0.0,
        })
        .collect();
    window.set_languages(ModelRc::new(VecModel::from(langs)));
    window.set_language(language(&config).as_str().into());
    // The shape the window was left in. `detail` when nothing was chosen —
    // and when something unknown was, which is the same answer a frontend
    // should give to a word it does not have a drawing for.
    let kept = scour_settings::Settings::load(&state_dir(&config));
    // The widths somebody dragged, in either window: they are keyed by column
    // id and kept beside the index, so a column widened in the browser opens
    // that wide here.
    for (id, px) in &kept.widths {
        let v = *px as f32;
        match id.as_str() {
            "name" => window.set_uw_name(v),
            "kind" => window.set_uw_kind(v),
            "path" => window.set_uw_path(v),
            "mtime" => window.set_uw_mtime(v),
            "size" => window.set_uw_size(v),
            _ => {}
        }
    }
    let kept_layout = kept.layout;
    if matches!(kept_layout.as_str(), "icons" | "large") {
        window.set_view_mode(kept_layout.as_str().into());
    }
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
    let ui_lines = lines.clone();
    let ui_picks = Rc::clone(&picks);
    let ui_facets = facets.clone();
    let ui_cat = cat.clone();

    let sink = move |got: Got| {
        let _ = slint::invoke_from_event_loop(move || deliver(got));
    };

    let link = Rc::new(Link::start(addr.clone(), sink));

    {
        // Registered after the link exists, because answering a search now
        // asks one more question — the facet count that goes with it.
        let link = Rc::clone(&link);
        INBOX.with(|slot| {
            *slot.borrow_mut() = Some(Rc::new(move |got: Got| {
                let Some(w) = weak.upgrade() else { return };
                apply(
                    &w, &ui_state, &ui_rows, &ui_lines, &ui_picks, &ui_facets, &ui_cat, &link, got,
                );
            }));
        });
    }

    // --- the query line ---------------------------------------------------
    {
        let state = state.clone();
        let link = link.clone();
        let facets = facets.clone();
        let model = Rc::clone(&rows);
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
                dispatch(&state, &link, &model, rows);
                return;
            }
            let state = state.clone();
            let link = link.clone();
            let model = Rc::clone(&model);
            let weak = weak.clone();
            slint::Timer::single_shot(std::time::Duration::from_millis(DEBOUNCE_MS), move || {
                if state.borrow().generation != generation {
                    trace(&format!("timer {generation} superseded"));
                    return;
                }
                let _ = weak;
                trace(&format!("dispatch {generation}"));
                dispatch(&state, &link, &model, rows);
            });
        });
    }

    // --- the rail ---------------------------------------------------------
    {
        let state = state.clone();
        let link = link.clone();
        let facets = facets.clone();
        let model = Rc::clone(&rows);
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
            dispatch(&state, &link, &model, rows);
        });
    }

    // --- the column headers ----------------------------------------------
    // The four window buttons. Three open a panel; the fourth writes a file.
    //
    // **A second press closes it**, which is what a person expects of a
    // button that opened something, and what the browser's own `?` does.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let addr = addr.clone();
        let cat_for_tools = Rc::clone(&cat);
        window.on_tool_clicked(move |what| {
            let Some(w) = weak.upgrade() else { return };
            match what.as_str() {
                "export" => {
                    export(&w, &addr, &cat_for_tools);
                }
                other => {
                    let open = w.get_panel() == other;
                    w.set_panel(if open { "".into() } else { other.into() });
                    // Asked when it opens rather than kept fresh: the rules
                    // change when somebody changes them, and this window is
                    // the one changing them.
                    if !open && other == "rules" {
                        link.send(Ask::Rules);
                    }
                }
            }
        });
    }

    // The view switch. Remembered like every other choice a person makes, so
    // the window opens in the shape they left it.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        window.on_view_clicked(move |mode| {
            let Some(w) = weak.upgrade() else { return };
            w.set_view_mode(mode.clone());
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    layout: Some(mode.to_string()),
                    ..Default::default()
                },
            });
        });
    }

    // Dragging a column edge. The width follows the pointer and what it lands
    // on is kept by the service, keyed by column id — the same key the browser
    // page uses, so a column dragged in one window is that wide in the other.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        window.on_column_dragged(move |which, delta| {
            let Some(w) = weak.upgrade() else { return };
            // A floor, because a column dragged to nothing cannot be dragged
            // back: there is no edge left to take hold of.
            let clamp = |v: f32| v.max(48.0);
            let now = match which.as_str() {
                "name" => {
                    let v = clamp(w.get_cw_name() + delta);
                    w.set_uw_name(v);
                    v
                }
                "kind" => {
                    let v = clamp(w.get_cw_kind() + delta);
                    w.set_uw_kind(v);
                    v
                }
                "path" => {
                    let v = clamp(w.get_cw_path() + delta);
                    w.set_uw_path(v);
                    v
                }
                "mtime" => {
                    let v = clamp(w.get_cw_mtime() + delta);
                    w.set_uw_mtime(v);
                    v
                }
                "size" => {
                    let v = clamp(w.get_cw_size() + delta);
                    w.set_uw_size(v);
                    v
                }
                _ => return,
            };
            let mut widths = std::collections::BTreeMap::new();
            widths.insert(which.to_string(), now as u32);
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    widths: Some(widths),
                    ..Default::default()
                },
            });
        });
    }

    // A bar is a filter, and the same one the rail rows are: it adds a term
    // to the text rather than to a hidden state, and pressing it again takes
    // it off. `dm:38d` is "changed within the last 38 days", which is what the
    // bar's upper bound means.
    {
        let weak = window.as_weak();
        window.on_bar_clicked(move |days| {
            if let Some(w) = weak.upgrade() {
                w.invoke_facet_clicked(format!("dm:{days}d").into());
            }
        });
    }

    // Switching a rule off, or back on. The list of what is off is kept whole
    // rather than patched, because that is what `Change` carries — and the
    // service takes it from there: the engine re-tunes, the watchers re-tune,
    // and a scan brings the index in line.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let off: Rc<RefCell<Vec<String>>> = Rc::default();
        window.on_rule_toggled(move |id| {
            let Some(w) = weak.upgrade() else { return };
            let mut held = off.borrow_mut();
            let id = id.to_string();
            if let Some(at) = held.iter().position(|o| o.eq_ignore_ascii_case(&id)) {
                held.remove(at);
            } else {
                held.push(id);
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    exclude_off: Some(held.clone()),
                    ..Default::default()
                },
            });
            // Ask again rather than guessing what the service made of it: the
            // reply is the truth about what is in force.
            link.send(Ask::Rules);
            let _ = w;
        });
    }

    // A language is a restart of the words, not of the window: the catalogue
    // is rebuilt, every visible string is written again, and the choice is
    // kept by the service so the browser page opens in the same language.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        window.on_language_picked(move |tag| {
            let Some(w) = weak.upgrade() else { return };
            w.set_language(tag.clone());
            w.set_panel("".into());
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    language: Some(tag.to_string()),
                    ..Default::default()
                },
            });
        });
    }

    {
        let state = state.clone();
        let link = link.clone();
        let model = Rc::clone(&rows);
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
            dispatch(&state, &link, &model, rows);
        });
    }

    // --- the selection ----------------------------------------------------
    //
    // Plain replaces it, `Ctrl` adds one, `Shift` takes the run — the browser
    // page's gestures, because the two windows are the same program.
    {
        let picks = Rc::clone(&picks);
        let anchor = Rc::clone(&anchor);
        let rows = Rc::clone(&rows);
        let cat = cat.clone();
        let weak = window.as_weak();
        window.on_pick(move |row, adding, run| {
            let Some(w) = weak.upgrade() else { return };
            let Some(here) = rows.pick_at(row.max(0) as usize) else {
                return;
            };
            let mut held = picks.borrow_mut();
            if run {
                // **The run, out of what is in hand.** A range over a result
                // of millions can cross pages nobody has fetched, and a
                // selection of rows this window has never seen is a promise it
                // cannot keep — so the run is what it holds between the two
                // ends, which is what is on screen and near it.
                held.clear();
                let (from, to) = if anchor.get() <= row {
                    (anchor.get(), row)
                } else {
                    (row, anchor.get())
                };
                for at in from.max(0)..=to.max(0) {
                    if let Some(pick) = rows.pick_at(at as usize) {
                        held.insert(at as usize, pick);
                    }
                }
            } else if adding {
                anchor.set(row);
                if held.values().any(|p| p.path == here.path) {
                    held.retain(|_, p| p.path != here.path);
                } else {
                    held.insert(row.max(0) as usize, here);
                }
            } else {
                anchor.set(row);
                held.clear();
                held.insert(row.max(0) as usize, here);
            }
            show_picks(&w, &cat, &rows, &held, false);
        });
    }
    {
        let picks = Rc::clone(&picks);
        let rows = Rc::clone(&rows);
        let cat = cat.clone();
        let weak = window.as_weak();
        window.on_pick_dropped(move || {
            let Some(w) = weak.upgrade() else { return };
            picks.borrow_mut().clear();
            show_picks(&w, &cat, &rows, &picks.borrow(), false);
        });
    }
    {
        let picks = Rc::clone(&picks);
        let weak = window.as_weak();
        let cat = cat.clone();
        window.on_pick_copied(move || {
            // Where a single path goes, and for the same reason: this window
            // is a client of a service and a clipboard crate to copy a string
            // is a dependency for a line of text.
            for pick in picks.borrow().values() {
                println!("{}", pick.path);
            }
            if let Some(w) = weak.upgrade() {
                w.set_hint(t(&cat, "path printed to the terminal"));
            }
        });
    }
    {
        let picks = Rc::clone(&picks);
        let rows = Rc::clone(&rows);
        let cat = cat.clone();
        let asking = Rc::new(std::cell::Cell::new(false));
        let weak = window.as_weak();
        window.on_pick_opened(move || {
            let Some(w) = weak.upgrade() else { return };
            let folders = folders_of(&picks.borrow());
            // **More than a couple of windows is asked about first.** Opening
            // eleven file managers because somebody selected eleven files is
            // not a thing to do without being sure, and the page asks the same
            // question in the same place.
            if folders.len() > 2 && !asking.get() {
                asking.set(true);
                show_picks(&w, &cat, &rows, &picks.borrow(), true);
                return;
            }
            asking.set(false);
            for folder in folders {
                open(&folder);
            }
            show_picks(&w, &cat, &rows, &picks.borrow(), false);
        });
    }

    // --- opening things ---------------------------------------------------
    //
    // **The path comes off the row.** It used to be read out of the last page
    // of hits by the list's own row number — which is a number in the whole
    // result, so row 4,000 of a page of two hundred opened whatever happened
    // to be fourth in it. There is no second list to keep in step now.
    {
        let rows = Rc::clone(&rows);
        window.on_activated(move |i| {
            if let Some(path) = path_of(&rows, i) {
                open(&path);
            }
        });
    }
    {
        let rows = Rc::clone(&rows);
        window.on_reveal(move |i| {
            if let Some(path) = path_of(&rows, i) {
                let dir = match path.rfind('/') {
                    Some(0) => "/",
                    Some(at) => &path[..at],
                    None => ".",
                };
                open(dir);
            }
        });
    }
    {
        let rows = Rc::clone(&rows);
        let weak = window.as_weak();
        let cat = cat.clone();
        window.on_copy_path(move |i| {
            let Some(path) = path_of(&rows, i) else {
                return;
            };
            // No clipboard dependency: the window is a client of a service, and
            // adding an X11/Wayland clipboard crate to copy one string is a
            // dependency for a line of text. The path goes to stdout, where a
            // shell pipeline can take it, and the hint says so.
            println!("{path}");
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
    // **What scrolled into sight.** The list says when it has moved and this
    // asks for what moved into view — see [`follow`], which is where the
    // decision lives, and `first-row` in `main.slint`, which is what raises
    // it.
    {
        let rows = Rc::clone(&rows);
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let weak = window.as_weak();
        window.on_moved(move || {
            if let Some(w) = weak.upgrade() {
                follow(&w, &state, &link, &rows);
            }
        });
    }
    // And the backstop, for what changes the view without moving the list: a
    // window somebody made taller, a page that came back short, an answer that
    // never came. Ten times a second is cheap enough to leave running and slow
    // enough that it is never what scrolling waits for.
    {
        let rows = Rc::clone(&rows);
        let lines = Rc::clone(&lines);
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let weak = window.as_weak();
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(100),
            move || {
                let Some(w) = weak.upgrade() else { return };
                // **The window is what knows how wide a line is.** The mode
                // and the width both decide it, and both change without
                // asking anybody — a resize, a press on a view button — so it
                // is read here rather than announced from four places.
                lines.per_line(if w.get_grid() {
                    w.get_per_line().max(1) as usize
                } else {
                    0
                });
                lines.sync();
                follow(&w, &state, &link, &rows);
            },
        );
    }

    // The scopes, once: they do not change while the window is open. On the
    // slow lane, because the fast one coalesces and would drop this the
    // instant a keystroke followed it.
    link.send(Ask::Places);
    link.send(Ask::Status);
    trace(&format!("first search sent {:.1?} in", launched.elapsed()));
    dispatch(
        &state,
        &link,
        &rows,
        window.get_visible_rows().max(0) as u32,
    );
    FIRST.with(|f| f.set(Some(launched)));
    // Type a query before the window opens, for the same reason `SCOUR_GUI_SNAP`
    // exists: a picture of an empty box says nothing about how a query looks.
    // Open a panel before the window does, for the same reason the query flag
    // exists: a picture of a closed panel says nothing about the panel.
    // Force a scheme, for looking at the other one on a desktop that has
    // made its choice.
    if let Ok(scheme) = std::env::var("SCOUR_GUI_SCHEME") {
        window.global::<Theme>().set_dark(scheme != "light");
    }

    // Scroll somewhere before the snapshot, so fetching can be tested without
    // a hand on a wheel.
    //
    // A comma-separated list is walked a step at a time, which is the case
    // that matters: one jump lands somewhere and settles, while a run of them
    // is what a wheel does — and a window that fetches a page per frame, or
    // sends the list back to where it was, only says so while it is moving.
    if let Ok(spec) = std::env::var("SCOUR_GUI_SCROLL") {
        let stops: Vec<f32> = spec
            .split(',')
            .filter_map(|px| px.trim().parse().ok())
            .collect();
        // How long a stop lasts. A wheel crosses a row boundary every frame,
        // so a drag is measured at `SCOUR_GUI_SCROLL_MS=16` and a settled list
        // at the default.
        let every = std::env::var("SCOUR_GUI_SCROLL_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(600);
        let weak = window.as_weak();
        let at = std::cell::Cell::new(0usize);
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(every),
            move || {
                let i = at.get();
                let Some(w) = weak.upgrade() else { return };
                let Some(&px) = stops.get(i) else {
                    trace(&format!("resting at row {}", w.get_first_row()));
                    return;
                };
                at.set(i + 1);
                w.invoke_scroll_to(px);
                trace(&format!("scrolled to {px}px, row {}", w.get_first_row()));
            },
        );
    }

    // Pick a few rows before the window opens, for the same reason the query
    // flag exists: a picture of a list with nothing selected says nothing
    // about the bar that appears when something is.
    if let Ok(n) = std::env::var("SCOUR_GUI_PICK")
        && let Ok(n) = n.parse::<usize>()
    {
        let picks = Rc::clone(&picks);
        let rows = Rc::clone(&rows);
        let cat = cat.clone();
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1200), move || {
            let Some(w) = weak.upgrade() else { return };
            let mut held = picks.borrow_mut();
            for row in 0..n {
                if let Some(pick) = rows.pick_at(row) {
                    held.insert(row, pick);
                }
            }
            show_picks(&w, &cat, &rows, &held, false);
        });
    }

    if let Ok(mode) = std::env::var("SCOUR_GUI_VIEW") {
        window.set_view_mode(mode.as_str().into());
    }

    // Press a rail row before the window opens — the filter slot, not the
    // text, which are two different things and only one of them leaves the
    // rail whole.
    if let Ok(term) = std::env::var("SCOUR_GUI_FACET") {
        let weak = window.as_weak();
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(120),
            move || {
                if let Some(w) = weak.upgrade() {
                    w.invoke_facet_clicked(term.as_str().into());
                }
            },
        );
    }

    if let Ok(which) = std::env::var("SCOUR_GUI_PANEL") {
        // Press it, do not set it: the handler is what asks the service for
        // what the panel shows, and setting the property first made the press
        // read as a second one — which closes it and sends nothing.
        window.invoke_tool_clicked(which.as_str().into());
    }

    if let Ok(q) = std::env::var("SCOUR_GUI_QUERY") {
        // Set, then tell the window once. Calling `query-changed` *and*
        // letting the two-way binding fire it produced "rapor", "erapor",
        // "emrapor" — the callback writing back into the property it was
        // called from.
        window.set_query(q.as_str().into());
        let weak = window.as_weak();
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(80),
            move || {
                if let Some(w) = weak.upgrade() {
                    w.invoke_query_changed(w.get_query());
                }
            },
        );
    }

    // Photograph the window and leave, when asked. See [`snapshot`].
    if let Ok(path) = std::env::var("SCOUR_GUI_SNAP") {
        let weak = window.as_weak();
        // Held rather than dropped: a `Timer` that goes out of scope never
        // fires. Late enough that the first page of rows has arrived and been
        // laid out — anything earlier photographs an empty list.
        let t = Box::leak(Box::new(slint::Timer::default()));
        // Later, when there is a scroll to walk first: `SCOUR_GUI_SNAP_MS`.
        let after = std::env::var("SCOUR_GUI_SNAP_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(2500);
        t.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(after),
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

/// Fetch the page the list is about to need, if it is not already coming.
///
/// **Fetching cannot happen where the need is noticed.** `row_data` is called
/// while the view is laying out, and a request started there would re-enter
/// the model being laid out. So the need is recorded and acted on here.
///
/// And the position is what is read, not only a reported miss. A miss is the
/// view saying "I drew a row you do not hold" — true while the eye is inside
/// the loaded page, useless the moment a page lands somewhere the eye is not,
/// because then there is no missing row to draw and the view says nothing.
/// That is the shape of "only the first page ever loads".
fn follow(w: &MainWindow, state: &Rc<RefCell<State>>, link: &Rc<Link>, rows: &Rc<rows::Rows>) {
    let visible = w.get_visible_rows().max(0) as usize;
    let first = w.get_first_row().max(0) as usize;
    // A row the view drew and could not fill wins over the viewport: it is the
    // same place, one frame earlier.
    let first = rows.wanted().unwrap_or(first);
    let (revision, cost, quiet) = {
        let s = state.borrow();
        (
            s.revision,
            s.page_cost_us,
            s.asked_at.is_none_or(|at| at.elapsed() >= SETTLED),
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
    state.borrow_mut().asked_at = Some(std::time::Instant::now());
    send_page(state, link, (page * rows::SPAN) as u32, PAGE_MAX);
}

/// Send the search for the current state.
///
/// The facet count is **not** sent here, and that is the fix for the second
/// half of the same problem: it is a sidebar, it costs as much as the search,
/// and sending it beside every search doubled the traffic to answer a question
/// nobody had finished asking. It goes out once the search it belongs to has
/// actually been shown — see [`apply`].
fn dispatch(state: &Rc<RefCell<State>>, link: &Rc<Link>, model: &Rc<rows::Rows>, rows: u32) {
    let limit = rows.clamp(20, PAGE_MAX);
    {
        let mut s = state.borrow_mut();
        s.row_limit = limit;
        s.page_offset = 0;
        s.rewind = true;
    }
    // **The pages in hand answer a question nobody is asking any more.** They
    // are kept so that scrolling back is free; keeping them across a new query
    // would mean scrolling down into the last query's results, because a page
    // that is held is a page that is not fetched again.
    model.empty();
    send_search(state, link, 0, limit);
}

/// How long the result is, given a page of it and a count of it.
///
/// **A page shorter than the service is willing to give is the end of the
/// result**, whatever a count taken a moment ago says. The index moves while
/// somebody is scrolling, so a total measured before the last page was
/// fetched can claim rows that are no longer there — and the list then asks
/// for them, draws them blank, and asks again, for as long as anybody looks
/// at the bottom of it.
///
/// Two conditions, and both are needed. Shorter than what was **asked for**,
/// because the first page of a new query is deliberately only what fits on
/// screen and that is not an ending. And shorter than any page this service
/// has ever **served**, because a service whose own page ceiling is lower
/// than the request answers every page short and none of them is an ending
/// either.
fn list_length(counted: usize, offset: usize, got: usize, asked: usize, served: usize) -> usize {
    if got < asked && got < served {
        return offset + got;
    }
    // Otherwise the count stands — but never below what is already in hand.
    counted.max(offset + got)
}

fn send_search(state: &Rc<RefCell<State>>, link: &Rc<Link>, offset: u32, limit: u32) {
    // Beside the search, on the same lane and with the same coalescing: the
    // colours belong to the query that is on screen, and the newest is the
    // only one anybody will see.
    let (query_revision, query) = {
        let s = state.borrow();
        (s.query_revision, full_query(&s))
    };
    link.send(Ask::Explain {
        query_revision,
        query,
    });
    send_page(state, link, offset, limit);
}

/// Another page of the query that is already on screen.
///
/// Scrolling and the live refresh both come through here rather than through
/// [`send_search`]: the query has not changed, so reading it back for the
/// colouring is a parse per page for an answer that is already drawn.
fn send_page(state: &Rc<RefCell<State>>, link: &Rc<Link>, offset: u32, limit: u32) {
    let (generation, query_revision, query, sort, descending) = {
        let mut s = state.borrow_mut();
        s.page_sent = Some(std::time::Instant::now());
        let s = &*s;
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
/// The query as the engine gets it: what was typed, plus whatever filter is
/// pressed.
///
/// **The filter is a whole term now**, not a kind's token with `kind:` glued
/// on in front. It started as one — the rail only offered kinds — and the day
/// the ribbon and the scopes started using the same slot it began producing
/// `kind:dm:38d`, which parses as a search for that text and answers nothing.
fn full_query(s: &State) -> String {
    match &s.facet {
        Some(term) if s.query.trim().is_empty() => term.clone(),
        Some(term) => format!("{} {term}", s.query.trim()),
        None => s.query.trim().to_owned(),
    }
}

/// What the rail and the ribbon are counted over: the typed query, without the
/// filter they themselves offered.
///
/// **A rail counted through its own filter is a rail with one row in it.**
/// Press `kind:code` and every count but that one becomes zero, so the rail
/// answers "there is nothing else" about a question nobody asked — and there
/// is then no way back to the other kinds except by editing the text. The
/// browser page has a test with this name; this is the same rule.
fn facet_query(s: &State) -> String {
    s.query.trim().to_owned()
}

#[allow(clippy::too_many_arguments)]
fn apply(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    rows: &Rc<rows::Rows>,
    lines: &Rc<rows::Lines>,
    picks: &Rc<RefCell<std::collections::BTreeMap<usize, rows::Pick>>>,
    facets: &Rc<VecModel<Facet>>,
    cat: &Rc<Catalogue>,
    link: &Rc<Link>,
    got: Got,
) {
    match got {
        Got::Down(why) => {
            let mut s = state.borrow_mut();
            s.down = true;
            rows.forget_asking();
            w.set_busy(false);
            w.set_meter(format!("{} — {why}", cat.get("the service is not running")).into());
        }
        Got::Refused { revision, why } => {
            match revision {
                ReplyRevision::Search(generation) if generation == state.borrow().generation => {
                    rows.forget_asking();
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
        Got::Search {
            generation,
            offset,
            limit,
            reply,
        } => {
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
            let page: Vec<Row> = r
                .hits
                .iter()
                // The kind's word comes from the catalogue, by the engine's
                // own msgid — the same string the rail's labels and the
                // browser page use. A window that spelled these itself would
                // be a second vocabulary, and the day the engine learned a
                // fourteenth kind this one would show a blank.
                //
                // **Which of them are arrivals is not decided here**, because
                // here does not know: it is whether *this page* held them a
                // moment ago, and the page that held them is the model's. See
                // `Rows::put`.
                .map(|h| rows::row_of(h, &terms, now, &t(cat, h.kind.msgid()), false))
                .collect();
            // What each row weighs, beside the page rather than on it: Slint
            // counts in 32 bits and a file does not.
            let weights: Vec<i64> = r.hits.iter().map(|h| h.meta.size.max(0)).collect();
            let n = page.len();
            let any_fresh = {
                let mut s = state.borrow_mut();
                let counted = s
                    .exact_count
                    .map(|c| c.total)
                    .unwrap_or(r.total)
                    .min(i32::MAX as u64) as usize;
                let total = list_length(counted, offset as usize, n, limit as usize, rows.served());
                // **Where this page was asked for, carried by the answer.**
                // Read off the window's own state it was whichever page had
                // been requested most recently, which after a flick of the
                // wheel is not this one — so a page arrived and was filed two
                // hundred rows from where its rows belong.
                s.page_offset = offset;
                // The whole result's length, so the view sizes itself from it;
                // the page and which one it is, so the model can find it again.
                let arrived = rows.put(offset as usize / rows::SPAN, page, weights, total);
                // The tiles hold the same rows, so the lines carrying them
                // have changed too — and the result may have got longer.
                lines.touched(offset as usize, offset as usize + n);
                lines.sync();
                arrived
            };

            // **Put the flags out again.** Slint's `animate` interpolates when
            // a property *changes*; nothing here was changing it back, so a
            // row marked as new stayed washed orange until the next answer
            // replaced it — an arrival highlight that never finished arriving.
            // The wash is 1.6s in the page, so the flags come off then and the
            // animation carries the fade. Nothing arms this unless a page that
            // was already in hand came back holding something it did not.
            if any_fresh {
                let model: Rc<rows::Rows> = Rc::clone(rows);
                slint::Timer::single_shot(std::time::Duration::from_millis(1600), move || {
                    model.clear_fresh()
                });
            }
            if let Some(t) = FIRST.with(std::cell::Cell::take) {
                trace(&format!(
                    "first rows on screen {:.1?} after launch",
                    t.elapsed()
                ));
            }
            state.borrow_mut().page_cost_us = r.took_us;
            trace(&format!(
                "page {offset} landed in {:.1} ms round trip, {:.2} ms in the engine, {} rows visited",
                state
                    .borrow()
                    .page_sent
                    .map(|t| t.elapsed().as_secs_f64() * 1000.0)
                    .unwrap_or(0.0),
                r.took_us as f64 / 1000.0,
                r.rows_visited,
            ));
            trace(&format!(
                "drew {n} rows {:.1} ms after the key",
                state
                    .borrow()
                    .typed_at
                    .map(|t| t.elapsed().as_secs_f64() * 1000.0)
                    .unwrap_or(0.0)
            ));
            let (query_revision, query, ask_background, exact_count) = {
                let mut s = state.borrow_mut();
                s.shown = generation;
                if !r.capped {
                    s.exact_count = Some(ExactCount {
                        query_revision: s.query_revision,
                        total: r.total,
                        capped: false,
                    });
                }
                s.hits = r.hits;
                let query_revision = s.query_revision;
                let ask_background = s.start_background();
                (
                    query_revision,
                    // **Without the filter the rail itself offered.** The
                    // search below wants `full_query`; the rail and the ribbon
                    // want what was typed, or pressing `kind:code` leaves the
                    // rail with one row and no way back to the others.
                    facet_query(&s),
                    ask_background,
                    s.exact_count.filter(|c| c.query_revision == query_revision),
                )
            };
            // **A page landing never moves the viewport. A new question
            // does.** Rows are drawn at their place in the whole result, so a
            // page that arrives while somebody is reading lands under the rows
            // it belongs to and the eye stays where it was — that is the whole
            // reason the list is a model and not a sliding window.
            //
            // The flag is set where the question is asked, not worked out
            // here. It was worked out here, from whether the answer belonged
            // to the query already on screen — and the line above had just
            // filed this answer as that query, so the test said "the same one"
            // every time and a fresh search left the window looking at row
            // nine thousand of a result it had never seen.
            let rewind = {
                let mut s = state.borrow_mut();
                // Only by the answer that was asked for from the top: a page
                // fetched under a scrolled list is not the one that rewinds it.
                let rewind = s.rewind && offset == 0;
                s.rewind &= !rewind;
                rewind
            };
            if rewind {
                w.set_selected(0);
                w.invoke_scroll_to(0.0);
                // A different question, so what was picked out of the answer
                // to the last one is not an answer to anything.
                picks.borrow_mut().clear();
            }
            // The marks live on the rows and a page that has just landed is
            // carrying new ones, so what is picked has to be painted again.
            if !picks.borrow().is_empty() || w.get_picked() > 0 {
                let asking = w.get_pick_asking() && !picks.borrow().is_empty();
                show_picks(w, cat, rows, &picks.borrow(), asking);
            }
            w.set_busy(false);
            // **And straight on to the next one.** Nothing is asked for while
            // an answer is on its way, so this is where a drag continues: the
            // page that just landed may already be behind the hand, and the
            // question is asked again about where the list is now.
            follow(w, state, link, rows);
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
                // **And the list gets as long as the answer.** The interactive
                // search counts only to its cap, so until this the list was a
                // thousand rows tall over an index of millions: a scrollbar
                // that ran out, rows that could not be reached, and a jump the
                // first time a page fetch happened to carry the real length.
                rows.set_total(total.min(i32::MAX as u64) as usize);
                // The exact total arrives after the list is on screen, so
                // only the second number moves. Keeping the sentence's shape
                // is the point: a meter that reflows when a background answer
                // lands reads as the window having changed its mind.
                w.set_meter(
                    format!(
                        "{} / {}{}",
                        grouped(rows.held() as u64),
                        grouped(total),
                        if capped { "+" } else { "" },
                    )
                    .into(),
                );
            }
        }
        // The scopes: this desktop's own folders, each a `under:` term. The
        // labels are the desktop's own words — `user-dirs.dirs` is written in
        // the language the desktop was set up in — so nothing here translates
        // them and nothing here guesses at `~/Documents`.
        // The query, read back and cut into runs. Painted under the box.
        //
        // **The roles are the engine's**, mapped to five colours and nothing
        // else. Two of them say *this is not what you think it is* — a field
        // the parser did not recognise, a value it could not use — and those
        // are the only reason the colouring earns its place: without them a
        // mistyped `sizE:>1mb` is searched for as text and answers `0 of 0`,
        // which is what a correct query matching nothing also answers.
        Got::Explain {
            query_revision,
            reply,
        } => {
            if query_revision != state.borrow().query_revision {
                return;
            }
            let Response::Explain { spans, .. } = *reply else {
                return;
            };
            let query = full_query(&state.borrow());
            let runs: Vec<Span> = spans
                .iter()
                .map(|sp| Span {
                    text: query
                        .get(sp.start as usize..(sp.start + sp.len) as usize)
                        .unwrap_or_default()
                        .into(),
                    role: match sp.role {
                        scour_core::Role::Field => 1,
                        scour_core::Role::Value => 2,
                        scour_core::Role::Glob => 3,
                        scour_core::Role::Not => 4,
                        scour_core::Role::UnknownField | scour_core::Role::BadValue => 5,
                        _ => 0,
                    },
                })
                .collect();
            w.set_spans(ModelRc::new(VecModel::from(runs)));
        }
        // The exclusion rules, in the three groups the service keeps them in:
        // what a window added, what `config.toml` says, what is built in. Only
        // the first can be deleted; any of them can be switched off, and the
        // ones that are come back marked.
        Got::Rules(reply) => {
            let Response::Rules {
                builtin_paths,
                builtin_dirs,
                builtin_files,
                config_paths,
                config_dirs,
                config_files,
                config_allow,
                added_paths,
                added_dirs,
                added_files,
                added_allow,
                off,
            } = *reply
            else {
                return;
            };
            let is_off = |id: &str| off.iter().any(|o| o.eq_ignore_ascii_case(id));
            let mut rows: Vec<Facet> = Vec::new();
            for (group, kind, list) in [
                ("added", "path", added_paths),
                ("added", "dir", added_dirs),
                ("added", "file", added_files),
                ("added", "allow", added_allow),
                ("config", "path", config_paths),
                ("config", "dir", config_dirs),
                ("config", "file", config_files),
                ("config", "allow", config_allow),
                ("builtin", "path", builtin_paths),
                ("builtin", "dir", builtin_dirs),
                ("builtin", "file", builtin_files),
            ] {
                for value in list {
                    let id = scour_settings::rule_id(kind, &value);
                    rows.push(Facet {
                        label: value.as_str().into(),
                        token: id.as_str().into(),
                        // The group and the state, in the place a count goes:
                        // a rule that is listed but not in force reads as in
                        // force otherwise, which is the one misreading that
                        // matters here.
                        count: if is_off(&id) {
                            format!("{group} · {}", t(cat, "off")).into()
                        } else {
                            group.into()
                        },
                        share: 0.0,
                    });
                }
            }
            w.set_rules(ModelRc::new(VecModel::from(rows)));
        }
        // What the service is holding, said once. The page has this beside the
        // counts and it is the answer to "is this everything?" — an index of
        // 636 MB over three sources is a different claim from one over one.
        // **The index moved.** This is what makes the list live: the service
        // holds the request open until something it holds changes, and then
        // the window searches again and waits again. A list that only shows
        // what was true when the window opened is a list that quietly goes
        // wrong while somebody watches it.
        //
        // The search goes out *before* the next wait, so a burst of changes
        // does not queue a search per change: the next `Await` carries the
        // revision the answer came back with, which has already moved past
        // everything in the burst.
        Got::Awake(reply) => {
            let Response::Status(st) = *reply else { return };
            {
                let mut s = state.borrow_mut();
                if st.revision == s.revision {
                    // The timeout ran out rather than the index moving. Ask
                    // again; nothing else to do.
                    link.send(Ask::Await { since: s.revision });
                    return;
                }
                s.revision = st.revision;
            }
            // **Marked, not thrown away.** Every page in hand is now a
            // little out of date, and the one being looked at is re-read at
            // once — but the others are left alone until somebody looks at
            // them, or an index that changes every second would have this
            // window fetching every page it has ever seen.
            rows.mark(state.borrow().revision);
            follow(w, state, link, rows);
            // **And a beat before waiting again.** The service answers this
            // the instant its index moves, and while anything is being scanned
            // that is hundreds of times a second — so re-arming immediately is
            // a request loop between two processes, measured at a quarter of a
            // core with an empty list on screen and nothing to draw. A list
            // that catches up four times a second is a live list; one that
            // catches up four hundred times a second is a spin.
            let link = Rc::clone(link);
            let state = Rc::clone(state);
            slint::Timer::single_shot(AWAIT_AGAIN, move || {
                link.send(Ask::Await {
                    since: state.borrow().revision,
                });
            });
        }
        Got::Status(reply) => {
            let Response::Status(st) = *reply else { return };
            // The first revision, and the start of the long poll: everything
            // after this is the service telling the window when to look again.
            {
                let mut s = state.borrow_mut();
                if s.revision == 0 {
                    s.revision = st.revision;
                    link.send(Ask::Await { since: st.revision });
                }
            }
            w.set_holding(
                format!(
                    "{} {}  ·  {} {}  ·  {} {}",
                    t(cat, "index"),
                    compact_bytes(st.index_bytes),
                    st.sources,
                    t(cat, "sources"),
                    st.watching,
                    t(cat, "watching"),
                )
                .into(),
            );
        }
        Got::Places(reply) => {
            let Response::Places(p) = *reply else {
                trace(&format!("places: unexpected reply {reply:?}"));
                return;
            };
            let scopes: Vec<Facet> = p
                .places
                .iter()
                .map(|place| Facet {
                    label: place.label.as_str().into(),
                    token: format!("under:{}", place.path).into(),
                    count: slint::SharedString::new(),
                    share: 0.0,
                })
                .collect();
            w.set_scopes(ModelRc::new(VecModel::from(scopes)));
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

            // The bar behind each row is that kind's share of the largest,
            // which is the page's rule: it answers "is this most of what
            // matched" without a second number to read.
            let top = kinds.iter().map(|x| x.count).max().unwrap_or(1).max(1);
            let mut fresh: Vec<Facet> = Vec::new();
            for k in rows::offered_kinds() {
                let token = k.token();
                let Some(hit) = kinds.iter().find(|x| x.key == token) else {
                    continue;
                };
                fresh.push(Facet {
                    label: t(cat, k.msgid()),
                    // The whole term, not the bare token: the filter slot
                    // holds `kind:code`, `under:/home/x`, `dm:38d` — one kind
                    // of thing, so the query is built by joining rather than
                    // by remembering which prefix goes with which.
                    token: format!("kind:{token}").into(),
                    count: compact(hit.count).into(),
                    share: hit.count as f32 / top as f32,
                });
            }
            facets.set_vec(fresh);

            // The ribbon. Keys are the edges as text, newest first, and
            // `older` is everything past the last one — the service's own
            // wording, so nothing here has to know how the bands were made.
            // **Oldest on the left, which means reversing what the service
            // was asked for.** `bar_edges()` is newest first — it is the list
            // of upper bounds, and the smallest bound is the newest bar — but
            // the ribbon reads left to right as time does, and its axis says
            // "2 years ago" at the left end. Drawn in the asked-for order the
            // bars ran backwards under an axis that did not, which is worse
            // than no ribbon: it is a ribbon that is confidently wrong.
            let edges = scour_ui::bar_edges();
            let count_of = |key: &str| -> i32 {
                ages.iter()
                    .find(|x| x.key == key)
                    .map(|x| x.count)
                    .unwrap_or(0) as i32
            };
            let mut peak = 1i32;
            let mut bars: Vec<Bar> = edges
                .iter()
                .rev()
                .map(|days| {
                    let count = count_of(&days.to_string());
                    peak = peak.max(count);
                    Bar {
                        count,
                        days: *days as i32,
                        band: scour_ui::band_of(*days as f64) as i32,
                        about: String::new().into(),
                    }
                })
                .collect();
            // Anything older than the last edge belongs to the oldest bar
            // rather than to nothing: two years is where the scale ends, not
            // where the files do.
            if let Some(first) = bars.first_mut() {
                // The oldest bar is not a filter: it has no upper bound, so
                // `dm:730d` would select everything rather than narrow.
                first.days = 0;
                first.count += count_of("older");
                peak = peak.max(first.count);
            }
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
            // The facet walk counted the whole matching set on its way, so it
            // is the first thing that can tell the list how long it really is.
            if !f.capped {
                rows.set_total(f.total.min(i32::MAX as u64) as usize);
            }
            w.set_meter(
                format!(
                    "{} / {}{}",
                    grouped(rows.held() as u64),
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

/// Write the whole matching set to a file, without stopping the window.
///
/// **Its own connection and its own thread.** The export is a stream — 2.25 M
/// rows and 3.6 seconds on this machine — and the lanes are request/response;
/// putting it on one would hold every keystroke behind it. Nothing is held in
/// memory here either: the service writes pieces and each goes straight to the
/// file, which is why the size of the answer is bounded by the disk and by
/// nothing else.
///
/// The file lands beside the person rather than behind a dialog this window
/// does not have yet. Where it went is said in the meter, because a file
/// written somewhere nobody was told about is a file that was not written.
fn export(window: &MainWindow, addr: &str, cat: &Catalogue) {
    let query = window.get_query().to_string();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let path = std::path::PathBuf::from(home).join(format!("scour-{stamp}.csv"));
    let weak = window.as_weak();
    let addr = addr.to_string();
    let waiting = t(cat, "writing…");
    let wrote = t(cat, "written to");
    let failed = t(cat, "could not be written");
    window.set_meter(waiting);

    std::thread::spawn(move || {
        let outcome = (|| -> std::io::Result<u64> {
            let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
            let mut client = scour_ipc::Client::connect(&addr)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            let mut bytes = 0u64;
            let mut hit: Option<std::io::Error> = None;
            let _ = client.stream(
                scour_proto::Request::Export {
                    query,
                    columns: Vec::new(),
                },
                |piece| match piece {
                    scour_proto::Response::ExportChunk { csv } => {
                        use std::io::Write;
                        match file.write_all(csv.as_bytes()) {
                            Ok(()) => {
                                bytes += csv.len() as u64;
                                true
                            }
                            Err(e) => {
                                hit = Some(e);
                                false
                            }
                        }
                    }
                    _ => true,
                },
            );
            use std::io::Write;
            file.flush()?;
            match hit {
                Some(e) => Err(e),
                None => Ok(bytes),
            }
        })();

        let said = match outcome {
            Ok(bytes) => format!("{wrote} {} · {}", path.display(), compact(bytes)),
            Err(e) => format!("{failed}: {e}"),
        };
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                w.set_meter(said.into());
            }
        });
    });
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

/// Bytes, the way the meter says them: `636,3 MB`.
fn compact_bytes(n: u64) -> String {
    let mb = n as f64 / 1_048_576.0;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0).replace('.', ",")
    } else {
        format!("{mb:.1} MB").replace('.', ",")
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

/// Where the settings this window shares with the others live.
fn state_dir(cfg: &scour_config::Config) -> std::path::PathBuf {
    cfg.index
        .dir
        .parent()
        .unwrap_or(cfg.index.dir.as_path())
        .join("state")
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
            page_sent: None,
            asked_at: None,
            page_cost_us: 0,
            rewind: false,
            typed_at: None,
            shown: 0,
            query: "rapor".into(),
            sort: "relevance".into(),
            descending: true,
            facet: None,
            hits: Vec::new(),
            revision: 0,
            down: false,
        };
        assert_eq!(full_query(&s), "rapor");

        // **The slot holds a whole term.** It used to hold a kind's bare token
        // and `full_query` glued `kind:` on, which was fine while the rail
        // only offered kinds — and started producing `kind:dm:38d` the day the
        // ribbon and the scopes began using the same slot.
        s.facet = Some("kind:image".into());
        assert_eq!(full_query(&s), "rapor kind:image");
        s.facet = Some("under:/home/u/Belgeler".into());
        assert_eq!(full_query(&s), "rapor under:/home/u/Belgeler");
        s.facet = Some("dm:38d".into());
        assert_eq!(full_query(&s), "rapor dm:38d");

        s.query = "  ".into();
        assert_eq!(full_query(&s), "dm:38d");

        // And the rail is counted over what was typed, never over the term it
        // offered — otherwise pressing one leaves the rail with a single row.
        s.query = "rapor".into();
        assert_eq!(facet_query(&s), "rapor");
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
            page_sent: None,
            asked_at: None,
            page_cost_us: 0,
            rewind: false,
            typed_at: None,
            shown: 4,
            query: "rapor".into(),
            sort: "modified".into(),
            descending: true,
            facet: None,
            hits: Vec::new(),
            revision: 0,
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
    fn a_page_that_comes_back_short_is_the_end_of_the_result() {
        // The ordinary case: a full page in the middle of a long result, and
        // the count is what says how long it is.
        assert_eq!(list_length(2_500_000, 400, 200, 200, 200), 2_500_000);
        // The last page of a result that shrank while somebody scrolled to it.
        // Believing the count here leaves six rows that can never arrive, and
        // they are asked for for ever.
        assert_eq!(list_length(979, 779, 194, 200, 200), 973);
        // The first page of a new query is only what fits on screen. It is
        // short, and it is not an ending.
        assert_eq!(list_length(2_500_000, 0, 24, 24, 200), 2_500_000);
        // Nor is a service whose own page ceiling is below the request.
        assert_eq!(list_length(5_000, 0, 200, 256, 200), 5_000);
        // And a count that is somehow shorter than what is already in hand
        // does not make the loaded rows unreachable.
        assert_eq!(list_length(10, 400, 200, 200, 200), 600);
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
