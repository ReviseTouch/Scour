//! The Scour window.
//!
//! A frontend only: no index, no walk, everything over a socket. Service
//! calls run on worker threads and return as events ([`link`]); query
//! meaning is the engine's; a reply for a superseded keystroke is dropped.

mod link;
mod rows;

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Context, Result};
use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_proto::Response;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use link::{Ask, Got, Half, Link, ReplyRevision};

/// The interface, as `slint-build` generated it, wrapped in a module so the
/// workspace's lints stop at the generated boundary.
mod ui {
    #![allow(
        missing_debug_implementations,
        clippy::all,
        clippy::pedantic,
        unused_qualifications
    )]
    slint::include_modules!();
}

pub use ui::{
    Bar, Cell, Dupe, Facet, Fact, Fonts, HeadInfo, Kid, MainWindow, MenuItem, Row, Rule, Scheme,
    Span, Theme,
};

thread_local! {
    /// When the process started, until the first rows are drawn.
    static FIRST: std::cell::Cell<Option<std::time::Instant>> = const {
        std::cell::Cell::new(None)
    };
    /// Where an answer goes once it is back on the UI thread.
    static INBOX: RefCell<Option<Rc<dyn Fn(Got)>>> = const { RefCell::new(None) };
}

/// Hand one answer to the window. Runs on the UI thread and nowhere else.
fn deliver(got: Got) {
    // Cloned out of the cell first: the handler can re-enter this function.
    let handler = INBOX.with(|slot| slot.borrow().clone());
    if let Some(f) = handler {
        f(got);
    }
}

/// Diagnostics, off unless `SCOUR_TRACE` is set.
pub fn trace(what: &str) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var("SCOUR_TRACE").is_ok()) {
        eprintln!("gui: {what}");
    }
}

/// One key by name. Anything that is not a name is the text itself.
fn named_key(name: &str) -> slint::SharedString {
    use slint::platform::Key;
    match name.to_ascii_lowercase().as_str() {
        "return" | "enter" => Key::Return.into(),
        "escape" | "esc" => Key::Escape.into(),
        "backspace" => Key::Backspace.into(),
        "delete" | "del" => Key::Delete.into(),
        "tab" => Key::Tab.into(),
        "home" => Key::Home.into(),
        "end" => Key::End.into(),
        "left" => Key::LeftArrow.into(),
        "right" => Key::RightArrow.into(),
        "up" => Key::UpArrow.into(),
        "down" => Key::DownArrow.into(),
        "pageup" => Key::PageUp.into(),
        "pagedown" => Key::PageDown.into(),
        "space" => " ".into(),
        other => other.into(),
    }
}

/// A catalogue lookup, converted once to Slint's own string type.
fn t(cat: &Catalogue, msgid: &str) -> slint::SharedString {
    cat.get(msgid).as_ref().into()
}

/// How long after a keystroke the search goes out. Zero: a search costs 2 ms, so
/// any wait is the whole of the latency, and generation still drops stale replies.
const DEBOUNCE_MS: u64 = 0;

/// One ordinary gap between keys for non-interactive work to become stale. Rows
/// leave immediately; only the exact total and the sidebar wait.
const BACKGROUND_IDLE_MS: u64 = 200;

/// How long to leave the index alone between one change and the next ask.
const AWAIT_AGAIN: std::time::Duration = std::time::Duration::from_millis(250);

/// How long after the last touch this window counts itself as watched. A window
/// nobody uses makes the service commit every second instead of every fifteen:
/// 10.7% of a core against 0.4%. After this it dozes ([`DOZE_AGAIN`]).
const AWAKE_FOR: std::time::Duration = std::time::Duration::from_secs(60);

/// How often a dozing window asks anyway, so one brought forward is already right.
const DOZE_AGAIN: std::time::Duration = std::time::Duration::from_secs(10);

/// How long the list must have been still before a page is re-read. Missing pages
/// are never held back by this.
const SETTLED: std::time::Duration = std::time::Duration::from_millis(500);

/// What a page has to cost before the next is guessed at, in microseconds. A page
/// walks the matching set above it: 1 ms near the top, 125 ms at 2.5 M rows.
const CHEAP_PAGE_US: u64 = 20_000;

/// Rows in a page. It is [`rows::SPAN`], because pages are found again by dividing.
const PAGE_MAX: u32 = rows::SPAN as u32;

struct State {
    /// Where the volumes are: `Accessed` on `noatime` is the creation time.
    mounts: Vec<scour_places::Mount>,
    /// When somebody last did something here. See [`AWAKE_FOR`].
    stirred: std::time::Instant,
    dozing: bool,
    generation: u64,
    /// How many entries the index holds: the right-hand number in the meter.
    indexed: u64,
    /// Changes only when the matching set changes, not when its order does.
    query_revision: u64,
    /// What was searched before, newest first; kept here because the list is
    /// narrowed on every keystroke.
    past: Vec<String>,

    /// The coloured runs the engine last sent, as offsets rather than text, so a
    /// keystroke redraws the colours before its answer arrives. See [`painted`].
    spans: Vec<scour_core::Span>,
    /// The query revision whose sidebar and exact count were scheduled.
    background_query: Option<u64>,
    /// The fallback count's query revision: sent once, and only past a facet cap.
    count_query: Option<u64>,
    /// An exact count remains valid across sort changes.
    exact_count: Option<ExactCount>,
    row_limit: u32,
    page_offset: u32,
    page_sent: Option<std::time::Instant>,
    /// When a page last finished arriving: whether there is time to re-read one.
    page_landed_at: Option<std::time::Instant>,
    /// What the last page cost including the round trip, in microseconds.
    page_cost_us: u64,
    /// A question whose answer belongs at the top of the list: a new query, sort
    /// or rail press — never a page fetch or the live refresh.
    rewind: bool,
    typed_at: Option<std::time::Instant>,
    /// The generation whose search reply is currently on screen.
    shown: u64,
    query: String,
    sort: String,
    descending: bool,
    /// The whole term the rail has active, if any.
    facet: Option<String>,
    /// The rules this window added, as reported: deleting one sends the list back.
    added_paths: Vec<String>,
    added_dirs: Vec<String>,
    added_files: Vec<String>,
    /// The switched-off list last sent, unconfirmed: `Rules` answers coalesce.
    sent_off: Option<Vec<String>>,
    /// What the three rule lists were last built from. A model that is replaced
    /// takes its rows with it, so an answer saying nothing new must change nothing.
    rules_shown: [String; 3],
    /// Which rules are switched off, as reported: the list sent is the whole truth.
    exclude_off: Vec<String>,
    /// The folder the report is weighing. Empty is everything indexed.
    scope: String,
    hits: Vec<scour_core::Hit>,
    down: bool,
    /// The index revision this window has seen; the long poll waits past it.
    revision: u64,
    /// A batch of pictures is out. One at a time: a batch is several processes.
    asking_pictures: bool,
    /// What the preview panel is showing, not what is selected: a reply for a row
    /// the arrows have left is dropped against this.
    peek_path: String,
    /// Files already asked about, oldest first. Bounded, and outliving the rows:
    /// one screenful otherwise started eleven thumbnailers twice.
    asked_pictures: std::collections::VecDeque<String>,
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

/// A catalogue string without the page's markup, which this window cannot style.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    for c in text.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out.replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

/// How many of a group's paths are listed before the rest are counted.
const SHOWN_PATHS: usize = 6;

/// The floors the duplicate hunt offers. A unique size eliminates only 6.2% of
/// files, so a floor is what makes the question answerable.
const FLOORS: [(u64, &str); 4] = [
    (1 << 20, "1 MB+"),
    (10 << 20, "10 MB+"),
    (100 << 20, "100 MB+"),
    (1 << 30, "1 GB+"),
];

/// Duplicates under the report's scope, by size alone: reading is a second press.
fn hunt(w: &MainWindow, link: &Rc<Link>, state: &Rc<RefCell<State>>) {
    link.send(Ask::Dupes {
        under: state.borrow().scope.clone(),
        min_size: FLOORS[w.get_dupe_floor().clamp(0, 3) as usize].0,
        read_budget: 0,
    });
}

/// The six age bands, in the order the colours run.
const BANDS: [&str; 6] = [
    "today",
    "this week",
    "this month",
    "six months",
    "this year",
    "older",
];

/// The scope, as a run of buttons: everything, then each ancestor.
fn crumb_of(cat: &Catalogue, path: &str) -> Vec<Facet> {
    scour_ui::path::steps(path, &t(cat, "Everything"))
        .into_iter()
        .map(|(label, walked)| Facet {
            label: label.as_str().into(),
            token: walked.as_str().into(),
            count: slint::SharedString::new(),
            share: 0.0,
        })
        .collect()
}

/// Draw a weighed folder: what it comes to, and where the weight sits.
fn show_usage(w: &MainWindow, cat: &Catalogue, path: &str, u: &scour_core::UsageResponse) {
    w.set_crumb(ModelRc::new(VecModel::from(crumb_of(cat, path))));
    w.set_report_total(compact_bytes(u.root.bytes).into());
    w.set_report_files(
        t(cat, "{files} files · {disk} on disk")
            .replace("{files}", &grouped(u.root.files))
            .replace("{disk}", &compact_bytes(u.root.disk))
            .into(),
    );
    // How much of this weight nothing has touched in a year.
    let stale = if u.root.bytes > 0 {
        ((u.root.age[4] + u.root.age[5]) as f64 / u.root.bytes as f64 * 100.0).round()
    } else {
        0.0
    };
    w.set_report_stale(
        t(cat, "{percent}% of it older than a year")
            .replace("{percent}", &format!("{stale:.0}"))
            .into(),
    );
    let cut = if u.child_count as usize > u.children.len() {
        format!(
            "  ·  {}",
            t(cat, "the heaviest {shown} of {total} folders")
                .replace("{shown}", &grouped(u.children.len() as u64))
                .replace("{total}", &grouped(u.child_count as u64))
        )
    } else {
        String::new()
    };
    w.set_report_took(format!("{:.1} ms{cut}", u.took_us as f64 / 1000.0).into());
    let kids: Vec<Kid> = u
        .children
        .iter()
        .map(|c| {
            let band = |at: usize| {
                if c.bytes == 0 {
                    0.0
                } else {
                    c.age[at] as f32 / c.bytes as f32
                }
            };
            Kid {
                name: scour_ui::path::leaf(&c.path).into(),
                path: c.path.as_str().into(),
                size: compact_bytes(c.bytes).into(),
                share: format!(
                    "{:.1}%",
                    if u.root.bytes == 0 {
                        0.0
                    } else {
                        c.bytes as f64 / u.root.bytes as f64 * 100.0
                    }
                )
                .into(),
                files: grouped(c.files).into(),
                a0: band(0),
                a1: band(1),
                a2: band(2),
                a3: band(3),
                a4: band(4),
                a5: band(5),
            }
        })
        .collect();
    w.set_kids(ModelRc::new(VecModel::from(kids)));
}

/// The engine's own numbers, in three pieces: they are drawn differently.
fn meter(w: &MainWindow, count: String, ms: String, rows: String) {
    trace(&format!("meter {count} · {ms} · {rows}"));
    w.set_meter_count(count.into());
    w.set_meter_ms(ms.into());
    w.set_meter_rows(rows.into());
    w.set_meter(slint::SharedString::new());
}

/// A sentence instead of numbers: connecting, refused, not running.
fn said(w: &MainWindow, sentence: slint::SharedString) {
    w.set_meter_count(slint::SharedString::new());
    w.set_meter_ms(slint::SharedString::new());
    w.set_meter_rows(slint::SharedString::new());
    w.set_meter(sentence);
}

/// What the selection bar says. Folders are counted, never weighed: a total that
/// mixed the size column's `~` with exact bytes is one number from two claims.
fn picked_line(
    cat: &Catalogue,
    picks: &std::collections::BTreeMap<usize, rows::Pick>,
) -> (String, String, String) {
    let dirs = picks.values().filter(|p| p.is_dir).count();
    let files = picks.len() - dirs;
    let bytes: u64 = picks
        .values()
        .filter(|p| !p.is_dir)
        .map(|p| p.bytes.max(0) as u64)
        .sum();
    let mut after = Vec::new();
    if files > 0 {
        after.push(compact_bytes(bytes));
    }
    if dirs > 0 {
        after.push(t(cat, "{n} folders").replace("{n}", &grouped(dirs as u64)));
    }
    // Split where the number goes: a language need not put it first.
    let sentence = t(cat, "{n} selected");
    let (pre, post) = sentence
        .split_once("{n}")
        .unwrap_or(("", sentence.as_str()));
    let mut post = post.to_string();
    for part in after {
        post.push_str("  ·  ");
        post.push_str(&part);
    }
    (pre.to_string(), grouped(picks.len() as u64), post)
}

/// The folders a selection sits in, each once: eleven files, one window.
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
    let (pre, n, post) = picked_line(cat, picks);
    w.set_picked_pre(pre.into());
    w.set_picked_n(n.into());
    w.set_picked_post(post.into());
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

/// Write the next panic to a file. `force_capture` is deliberate: backtraces are
/// off unless `RUST_BACKTRACE` is set, and nobody sets it before a crash.
fn crash_log(state: &std::path::Path) {
    let file = state.join("gui-crash.log");
    let _ = std::fs::create_dir_all(state);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let when = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let where_ = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "?".into());
        let what = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "?".into());
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| "-".into());
        let note = format!(
            "\n=== scour-gui {} · unix {when}\n\
             at        {where_}\n\
             message   {what}\n\
             renderer  SLINT_BACKEND={} \n\
             display   WAYLAND_DISPLAY={} DISPLAY={}\n\
             backtrace\n{}\n",
            env!("CARGO_PKG_VERSION"),
            env("SLINT_BACKEND"),
            env("WAYLAND_DISPLAY"),
            env("DISPLAY"),
            std::backtrace::Backtrace::force_capture(),
        );
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
        {
            let _ = f.write_all(note.as_bytes());
        }
        eprint!("{note}");
        previous(info);
    }));
}

// What the window takes on the command line. **Ordinary comments, not doc
// comments**: clap prints a doc comment as the text `--help` shows.
#[derive(clap::Parser)]
#[command(name = "scour-gui", about = "Scour in a window", version)]
struct Args {
    /// Talk to a service listening here
    #[arg(long)]
    socket: Option<String>,
    /// Start with this query
    #[arg(short, long, default_value = "")]
    query: String,
}

fn main() -> Result<()> {
    // `--version` and `--help` answer and leave, before any socket or window.
    let args = <Args as clap::Parser>::parse();
    let launched = std::time::Instant::now();
    let config = scour_config::Config::load_or_default().0;
    // A window from a desktop entry has nowhere for standard error to go.
    crash_log(&config.state_dir());

    // One window. A second start hands over to the first and leaves.
    #[cfg(unix)]
    let claim = match only_one(&args.socket.clone().unwrap_or_else(|| config.socket())) {
        Some(claim) => claim,
        None => return Ok(()),
    };

    let kept = scour_settings::Settings::load(&config.state_dir());
    // Behind a cell: picking a language rebuilds it and rewrites every string.
    let cat: Rc<RefCell<Rc<Catalogue>>> = Rc::new(RefCell::new(Rc::new(Catalogue::for_language(
        &language(&kept, &config),
    ))));
    let window = MainWindow::new().context("the window could not be created")?;
    #[cfg(unix)]
    answer_the_next_start(claim.listener.try_clone()?, window.as_weak());
    dress(&window);
    words(&window, &cat.borrow());
    window.set_sorted_by("relevance".into());
    window.set_has_past(!kept.history.is_empty());
    trace(&format!("history: {} past searches", kept.history.len()));
    trace(&format!("window built {:.1?} in", launched.elapsed()));
    {
        let weak = window.as_weak();
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(3000),
            move || {
                let Some(w) = weak.upgrade() else { return };
                let w = w.window();
                let s = w.scale_factor();
                let size = w.size();
                trace(&format!(
                    "scale {s} — {}x{} physical, {:.1}x{:.1} logical",
                    size.width,
                    size.height,
                    size.width as f32 / s,
                    size.height as f32 / s
                ));
            },
        );
    }

    let state = Rc::new(RefCell::new(State {
        mounts: Vec::new(),
        // Awake at birth: opening the window is somebody doing something in it.
        stirred: std::time::Instant::now(),
        dozing: false,
        indexed: 0,
        generation: 0,
        query_revision: 0,
        past: kept.history.clone(),
        spans: Vec::new(),
        background_query: None,
        count_query: None,
        exact_count: None,
        row_limit: 20,
        page_offset: 0,
        page_sent: None,
        page_landed_at: None,
        page_cost_us: 0,
        asking_pictures: false,
        peek_path: String::new(),
        asked_pictures: std::collections::VecDeque::new(),
        rewind: true,
        typed_at: None,
        shown: 0,
        query: String::new(),
        sort: "relevance".into(),
        descending: true,
        facet: None,
        added_paths: Vec::new(),
        added_dirs: Vec::new(),
        added_files: Vec::new(),
        sent_off: None,
        rules_shown: Default::default(),
        exclude_off: Vec::new(),
        scope: String::new(),
        hits: Vec::new(),
        revision: 0,
        down: false,
    }));

    let rows: Rc<rows::Rows> = Rc::new(rows::Rows::default());
    // **Selected by path**: the index and the sort both move under a row number.
    let picks: Rc<RefCell<std::collections::BTreeMap<usize, rows::Pick>>> =
        Rc::new(RefCell::new(std::collections::BTreeMap::new()));
    // Where the last press was, so `Shift` has a run to take.
    let anchor: Rc<std::cell::Cell<i32>> = Rc::new(std::cell::Cell::new(0));
    let lines: Rc<rows::Lines> = Rc::new(rows::Lines::new(Rc::clone(&rows)));
    let facets: Rc<VecModel<Facet>> = Rc::new(VecModel::default());
    // **Made once and never replaced**: a row destroyed between a press and the
    // release is a press nobody receives.
    let rules: [Rc<VecModel<Rule>>; 3] = [
        Rc::new(VecModel::default()),
        Rc::new(VecModel::default()),
        Rc::new(VecModel::default()),
    ];
    window.set_rows(ModelRc::from(rows.clone()));
    window.set_lines(ModelRc::from(lines.clone()));
    window.set_facets(ModelRc::from(facets.clone()));
    window.set_rules_added(ModelRc::from(rules[0].clone()));
    window.set_rules_config(ModelRc::from(rules[1].clone()));
    window.set_rules_builtin(ModelRc::from(rules[2].clone()));
    window.set_hint(t(
        &cat.borrow(),
        "file name  ·  ext:pdf  ·  kind:image dm:7d  ·  size:>10mb",
    ));
    said(&window, t(&cat.borrow(), "connecting…"));
    // `""` is "whatever the desktop says", as in the config file.
    window.set_language(language(&kept, &config).as_str().into());
    // The widths somebody dragged, keyed by column id. **Kept whole, not just the
    // five this window draws**: `Change::widths` replaces the map, not merges it.
    let table = Rc::new(RefCell::new(Table::read(&kept)));
    set_heads(&window, &table.borrow(), &cat.borrow());
    relayout(&window, &table.borrow());
    // Read before the first search, so a pinned panel is open in the first frame.
    window.set_peeking(kept.preview);
    let kept_layout = kept.layout;
    if matches!(kept_layout.as_str(), "icons" | "large") {
        window.set_view_mode(kept_layout.as_str().into());
    }
    let addr = match &args.socket {
        Some(given) => given.clone(),
        None => config.socket(),
    };

    // The bridge from the worker threads. `invoke_from_event_loop` wants a `Send`
    // closure and everything the answer touches is `Rc`, so it carries only the
    // answer and finds the rest in a thread-local.
    let weak = window.as_weak();
    let ui_state = state.clone();
    let ui_table = Rc::clone(&table);
    let ui_rows = rows.clone();
    let ui_lines = lines.clone();
    let ui_picks = Rc::clone(&picks);
    let ui_facets = facets.clone();
    let ui_rules = rules.clone();
    let ui_cat = Rc::clone(&cat);

    let sink = move |got: Got| {
        let _ = slint::invoke_from_event_loop(move || deliver(got));
    };

    let link = Rc::new(Link::start(addr.clone(), sink));

    {
        let link = Rc::clone(&link);
        INBOX.with(|slot| {
            *slot.borrow_mut() = Some(Rc::new(move |got: Got| {
                let Some(w) = weak.upgrade() else { return };
                apply(
                    &w,
                    &ui_state,
                    &ui_table,
                    &ui_rows,
                    &ui_lines,
                    &ui_picks,
                    &ui_facets,
                    &ui_rules,
                    &ui_cat.borrow().clone(),
                    &link,
                    got,
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
            stir(&state, &link);
            // **A query has a length, and the reason is the renderer**: the
            // software renderer places every glyph at an `i16` coordinate, so a
            // line past 32 767 physical pixels is a panic with no error path.
            let text = if text.chars().count() > 600 {
                let cut: slint::SharedString = text.chars().take(600).collect::<String>().into();
                if let Some(w) = weak.upgrade() {
                    w.set_query(cut.clone());
                }
                cut
            } else {
                text
            };
            trace(&format!("query-changed {text:?}"));
            if let Some(w) = weak.upgrade() {
                w.set_note(slint::SharedString::new());
            }
            let generation = {
                let mut s = state.borrow_mut();
                s.query = text.to_string();
                s.advance_query();
                s.typed_at = Some(std::time::Instant::now());
                s.generation
            };
            // Laid over the text as it is now, so a deletion shows this frame.
            if let Some(w) = weak.upgrade() {
                let s = state.borrow();
                let runs = painted(&full_query(&s), &s.spans);
                w.set_spans(ModelRc::new(VecModel::from(runs)));
                if w.get_past_open() {
                    let lines = past_matching(&s.past, &s.query);
                    w.set_past_open(!lines.is_empty());
                    w.set_past_picked(0);
                    w.set_past(ModelRc::new(VecModel::from(lines)));
                }
            }
            // Counts from the previous matching set are worse than an empty rail.
            facets.set_vec(Vec::new());
            if let Some(w) = weak.upgrade() {
                w.set_busy(true);
            }
            // Straight out, no timer: generation drops a stale reply regardless.
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
            stir(&state, &link);
            let token = token.to_string();
            {
                let mut s = state.borrow_mut();
                // Clicking the active one clears it — the rule all faces share.
                s.facet = scour_ui::query::pressed(s.facet.as_deref(), &token);
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

    // Five buttons: four open a panel, the fifth writes a file. A second press
    // closes the panel.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let addr = addr.clone();
        let cat_for_tools = Rc::clone(&cat);
        window.on_tool_clicked(move |what| {
            let Some(w) = weak.upgrade() else { return };
            match what.as_str() {
                "export" => {
                    export(&w, &addr, &cat_for_tools.borrow());
                }
                other => {
                    let open = w.get_panel() == other;
                    w.set_panel(if open { "".into() } else { other.into() });
                    if !open && other == "rules" {
                        link.send(Ask::Rules);
                    }
                }
            }
        });
    }

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

    {
        let link = Rc::clone(&link);
        let state = Rc::clone(&state);
        let weak = window.as_weak();
        window.on_peek_toggled(move || {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            let on = !w.get_peeking();
            w.set_peeking(on);
            // So the next tick asks about whatever is selected.
            state.borrow_mut().peek_path.clear();
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    preview: Some(on),
                    ..Default::default()
                },
            });
        });
    }

    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let table = Rc::clone(&table);
        window.on_column_dragged(move |which, delta| {
            let Some(w) = weak.upgrade() else { return };
            // The floor is the column's own; `lay_out` will not go under it either.
            let floor = scour_ui::column(&which).map_or(48.0, |c| c.min as f32);
            let Some(at) = table
                .borrow()
                .shown
                .iter()
                .position(|c| c.id == which.as_str())
            else {
                return;
            };
            let Some(was) = w.get_cw().row_data(at) else {
                return;
            };
            let now = (was + delta).max(floor);
            // What is kept is what was asked for: a drag during a squeeze renders
            // narrower than the pointer went.
            table
                .borrow_mut()
                .widths
                .insert(which.to_string(), now as u32);
            relayout(&w, &table.borrow());
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    widths: Some(table.borrow().widths.clone()),
                    ..Default::default()
                },
            });
        });
    }

    // **Double-click an edge and that column decides for itself again**: a dragged
    // width otherwise stops stretching with the window for good.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let table = Rc::clone(&table);
        window.on_column_reset(move |which| {
            let Some(w) = weak.upgrade() else { return };
            // Removing it is what "nobody has touched it" means in the settings.
            table.borrow_mut().widths.remove(which.as_str());
            relayout(&w, &table.borrow());
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    widths: Some(table.borrow().widths.clone()),
                    ..Default::default()
                },
            });
        });
    }

    // The ⋮ at the end of the header row uses the right-click menu's own overlay;
    // `menu-columns` says which of the two it is holding.
    {
        let weak = window.as_weak();
        let table = Rc::clone(&table);
        let cat = Rc::clone(&cat);
        window.on_columns_clicked(move |x, y| {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let table = table.borrow();
            let mut model: Vec<MenuItem> = scour_ui::COLUMNS
                .iter()
                .map(|c| {
                    let on = table.showing(c.id);
                    MenuItem {
                        id: c.id.into(),
                        label: t(&cat, c.msgid),
                        key: "".into(),
                        rule: false,
                        careful: false,
                        heavy: false,
                        on,
                        // The last one standing cannot be turned off.
                        off: on && table.shown.len() == 1,
                    }
                })
                .collect();
            model.push(MenuItem {
                id: "".into(),
                label: t(&cat, "Back to the default"),
                key: "".into(),
                rule: true,
                careful: false,
                heavy: false,
                on: false,
                off: false,
            });
            w.set_menu(ModelRc::new(VecModel::from(model)));
            w.set_menu_x(x);
            w.set_menu_y(y);
            w.set_menu_columns(true);
            w.set_menu_open(true);
        });
    }

    {
        let weak = window.as_weak();
        let table = Rc::clone(&table);
        let link = Rc::clone(&link);
        let state = Rc::clone(&state);
        let cat = Rc::clone(&cat);
        window.on_column_toggled(move |id| {
            let Some(w) = weak.upgrade() else { return };
            stir(&state, &link);
            {
                let mut t = table.borrow_mut();
                if id.is_empty() {
                    t.shown = COLUMN_DEFAULT
                        .iter()
                        .filter_map(|id| scour_ui::column(id))
                        .collect();
                } else {
                    t.toggle(&id);
                }
            }
            w.set_menu_open(false);
            w.set_menu_columns(false);
            set_heads(&w, &table.borrow(), &cat.borrow());
            relayout(&w, &table.borrow());
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    columns: Some(table.borrow().ids()),
                    ..Default::default()
                },
            });
            // And the rows again, because a row is its cells: nothing holds the
            // value of a column that was not shown. Re-running costs 1 ms.
            let query = w.get_query();
            w.invoke_query_changed(query);
        });
    }

    {
        let weak = window.as_weak();
        let table = Rc::clone(&table);
        window.on_relayout(move |_| {
            if let Some(w) = weak.upgrade() {
                relayout(&w, &table.borrow());
            }
        });
    }

    // A bar is a filter like a rail row; `dm:38d` is the bar's upper bound.
    {
        let weak = window.as_weak();
        window.on_bar_clicked(move |days| {
            if let Some(w) = weak.upgrade() {
                w.invoke_facet_clicked(format!("dm:{days}d").into());
            }
        });
    }

    // The list of what is off is kept whole, because that is what `Change` carries.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let state = Rc::clone(&state);
        let tick = rules.clone();
        window.on_rule_toggled(move |id| {
            let Some(w) = weak.upgrade() else { return };
            let id = id.to_string();
            let held = {
                let mut s = state.borrow_mut();
                if let Some(at) = s
                    .exclude_off
                    .iter()
                    .position(|o| o.eq_ignore_ascii_case(&id))
                {
                    s.exclude_off.remove(at);
                } else {
                    s.exclude_off.push(id.clone());
                }
                s.sent_off = Some(s.exclude_off.clone());
                s.exclude_off.clone()
            };
            // **The tick moves on the press**: a round trip may be coalesced away,
            // and a tick that does not move is pressed again.
            for model in tick.iter() {
                for at in 0..model.row_count() {
                    let Some(row) = model.row_data(at) else {
                        continue;
                    };
                    if row.id.eq_ignore_ascii_case(&id) {
                        model.set_row_data(
                            at,
                            Rule {
                                off: !row.off,
                                ..row
                            },
                        );
                    }
                }
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    exclude_off: Some(held),
                    ..Default::default()
                },
            });
            // Ask again rather than guessing: the reply is what is in force.
            link.send(Ask::Rules);
            let _ = w;
        });
    }

    // **A rule is added as a name**; one starting with a slash is a path instead.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let state = Rc::clone(&state);
        window.on_rule_added(move |text| {
            let value = text.trim().to_string();
            if value.is_empty() {
                return;
            }
            let change = {
                let mut s = state.borrow_mut();
                if value.starts_with('/') {
                    s.added_paths.push(value);
                    scour_settings::Change {
                        exclude_paths: Some(s.added_paths.clone()),
                        ..Default::default()
                    }
                } else {
                    s.added_dirs.push(value);
                    scour_settings::Change {
                        exclude_dirs: Some(s.added_dirs.clone()),
                        ..Default::default()
                    }
                }
            };
            link.send(Ask::Remember { change });
            link.send(Ask::Rules);
            let _ = weak.upgrade();
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let state = Rc::clone(&state);
        window.on_rule_dropped(move |kind, value| {
            let value = value.to_string();
            let change = {
                let mut s = state.borrow_mut();
                let list = match kind.as_str() {
                    "path" => &mut s.added_paths,
                    "file" => &mut s.added_files,
                    _ => &mut s.added_dirs,
                };
                list.retain(|v| v != &value);
                let list = list.clone();
                match kind.as_str() {
                    "path" => scour_settings::Change {
                        exclude_paths: Some(list),
                        ..Default::default()
                    },
                    "file" => scour_settings::Change {
                        exclude_files: Some(list),
                        ..Default::default()
                    },
                    _ => scour_settings::Change {
                        exclude_dirs: Some(list),
                        ..Default::default()
                    },
                }
            };
            link.send(Ask::Remember { change });
            link.send(Ask::Rules);
            let _ = weak.upgrade();
        });
    }

    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let held = Rc::clone(&cat);
        let state = Rc::clone(&state);
        let model = Rc::clone(&rows);
        window.on_language_picked(move |tag| {
            let Some(w) = weak.upgrade() else { return };
            w.set_language(tag.clone());
            w.set_panel("".into());
            // The words change now. What comes from an answer is re-asked for,
            // because it is worded where it arrives.
            *held.borrow_mut() = Rc::new(Catalogue::for_language(&tag));
            words(&w, &held.borrow());
            set_heads(&w, &table.borrow(), &held.borrow());
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    language: Some(tag.to_string()),
                    ..Default::default()
                },
            });
            // A new question, so the sidebar is asked again: the facet walk runs
            // once per matching set.
            state.borrow_mut().advance_query();
            dispatch(&state, &link, &model, w.get_visible_rows().max(0) as u32);
            if w.get_tab() == "report" {
                let scope = state.borrow().scope.clone();
                w.invoke_report_open(scope.as_str().into());
            }
        });
    }

    {
        let state = state.clone();
        let link = link.clone();
        let model = Rc::clone(&rows);
        let weak = window.as_weak();
        window.on_sort_by(move |key| {
            stir(&state, &link);
            let key = key.to_string();
            {
                let mut s = state.borrow_mut();
                if s.sort == key {
                    s.descending = !s.descending;
                } else {
                    s.sort = key;
                    // Every order but the name reads best newest-or-largest first.
                    s.descending = s.sort != "name" && s.sort != "path";
                }
                s.advance_order();
            }
            // Read back: `sort` may be unchanged with only the direction flipped.
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

    // --- the report -------------------------------------------------------
    // Weighed when looked at and whenever a folder in it is pressed, never while
    // the search tab is showing, because it is a walk.
    {
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let cat = Rc::clone(&cat);
        let weak = window.as_weak();
        window.on_report_open(move |path| {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            state.borrow_mut().scope = path.to_string();
            w.set_head_folder(t(&cat, "Folder"));
            w.set_head_age(t(&cat, "By age"));
            w.set_head_share(t(&cat, "Share"));
            w.set_head_files(t(&cat, "Files"));
            w.set_kids_empty(t(
                &cat,
                "There are no further folders to show under this one.",
            ));
            w.set_age_words(ModelRc::new(VecModel::from(
                BANDS.iter().map(|b| t(&cat, b)).collect::<Vec<_>>(),
            )));
            w.set_head_kinds(t(&cat, "By kind"));
            w.set_head_biggest(t(&cat, "Largest files"));
            w.set_head_jump(t(&cat, "Search in this folder"));
            w.set_head_dupes(t(&cat, "Duplicate files"));
            w.set_report_under(if path.is_empty() {
                t(&cat, "everything")
            } else {
                path.clone()
            });
            w.set_jump_label(t(&cat, "Search in this scope"));
            w.set_jump_note(t(
                &cat,
                "From the report into the search: an under: term is added to the query and the search tab opens with the same scope.",
            ));
            w.set_dupes_show(t(&cat, "show"));
            w.set_dupes_hide(t(&cat, "hide"));
            w.set_dupes_confirm(t(&cat, "Confirm by reading"));
            w.set_dupe_floors(ModelRc::new(VecModel::from(
                FLOORS
                    .iter()
                    .map(|(_, label)| slint::SharedString::from(*label))
                    .collect::<Vec<_>>(),
            )));
            link.send(Ask::Usage {
                path: path.to_string(),
            });
            link.send(Ask::Kinds {
                path: path.to_string(),
            });
            link.send(Ask::Biggest {
                path: path.to_string(),
            });
            if w.get_dupes_open() {
                hunt(&w, &link, &state);
            }
        });
    }

    {
        let weak = window.as_weak();
        let cat = Rc::clone(&cat);
        let link = Rc::clone(&link);
        window.on_face_picked(move |which_one| {
            let Some(w) = weak.upgrade() else { return };
            open_face(&w, &which_one, &cat.borrow().clone(), &link);
        });
    }

    {
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let model = Rc::clone(&rows);
        let weak = window.as_weak();
        window.on_report_search(move || {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            let scope = state.borrow().scope.clone();
            let query = if scope.is_empty() {
                String::new()
            } else {
                format!("under:\"{scope}\"")
            };
            {
                let mut s = state.borrow_mut();
                s.query = query.clone();
                s.facet = None;
                s.advance_query();
            }
            w.set_query(query.as_str().into());
            w.set_active_facet(slint::SharedString::new());
            w.set_tab("search".into());
            dispatch(&state, &link, &model, w.get_visible_rows().max(0) as u32);
        });
    }

    {
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let weak = window.as_weak();
        window.on_dupes_toggled(move || {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            let open = !w.get_dupes_open();
            w.set_dupes_open(open);
            if open {
                hunt(&w, &link, &state);
            }
        });
    }
    {
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let weak = window.as_weak();
        window.on_dupes_floored(move |at| {
            let Some(w) = weak.upgrade() else { return };
            w.set_dupe_floor(at);
            if w.get_dupes_open() {
                hunt(&w, &link, &state);
            }
        });
    }
    {
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let weak = window.as_weak();
        window.on_dupes_read(move || {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            // Reading whole files off a disk is what turns a candidate into a
            // duplicate, so it is asked for.
            link.send(Ask::Dupes {
                under: state.borrow().scope.clone(),
                min_size: FLOORS[w.get_dupe_floor().clamp(0, 3) as usize].0,
                read_budget: 8 << 30,
            });
        });
    }

    // --- the selection ---------------------------------------------------
    // Plain replaces it, `Ctrl` adds one, `Shift` takes the run.
    {
        let picks = Rc::clone(&picks);
        let anchor = Rc::clone(&anchor);
        let rows = Rc::clone(&rows);
        let cat = Rc::clone(&cat);
        let weak = window.as_weak();
        window.on_pick(move |row, adding, run| {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let Some(here) = rows.pick_at(row.max(0) as usize) else {
                return;
            };
            let mut held = picks.borrow_mut();
            if run {
                // The run is what is in hand: a range over millions can cross
                // pages nobody has fetched.
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
        let cat = Rc::clone(&cat);
        let weak = window.as_weak();
        window.on_pick_dropped(move || {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            picks.borrow_mut().clear();
            show_picks(&w, &cat, &rows, &picks.borrow(), false);
        });
    }
    {
        let picks = Rc::clone(&picks);
        let weak = window.as_weak();
        let cat = Rc::clone(&cat);
        window.on_pick_copied(move || {
            let cat = cat.borrow().clone();
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
        let cat = Rc::clone(&cat);
        let asking = Rc::new(std::cell::Cell::new(false));
        let weak = window.as_weak();
        window.on_pick_opened(move || {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let folders = folders_of(&picks.borrow());
            // More than a couple of windows is asked about first.
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

    // --- opening things --------------------------------------------------
    // **The path comes off the row**, not out of a page of hits by row number.
    {
        let rows = Rc::clone(&rows);
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let weak = window.as_weak();
        window.on_activated(move |i| {
            stir(&state, &link);
            let query = full_query(&state.borrow());
            remember(&state, &link, &query);
            if let Some(w) = weak.upgrade() {
                w.set_has_past(!state.borrow().past.is_empty());
            }
            if let Some(path) = path_of(&rows, i) {
                open(&path);
            }
        });
    }

    // --- what was searched before -----------------------------------------
    {
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let weak = window.as_weak();
        window.on_query_committed(move || {
            stir(&state, &link);
            let query = full_query(&state.borrow());
            remember(&state, &link, &query);
            if let Some(w) = weak.upgrade() {
                w.set_has_past(!state.borrow().past.is_empty());
            }
        });
    }
    {
        let state = Rc::clone(&state);
        let weak = window.as_weak();
        window.on_past_toggled(move || {
            let Some(w) = weak.upgrade() else { return };
            if w.get_past_open() {
                w.set_past_open(false);
                return;
            }
            let lines = past_matching(&state.borrow().past, &state.borrow().query);
            w.set_past_picked(0);
            w.set_past(ModelRc::new(VecModel::from(lines.clone())));
            w.set_past_open(!lines.is_empty());
        });
    }
    {
        let weak = window.as_weak();
        window.on_past_taken(move |line| {
            let Some(w) = weak.upgrade() else { return };
            w.set_past_open(false);
            // **Set, then tell**: writing the two-way bound text does not fire
            // `edited`, so the second line is what searches.
            w.set_query(line.clone());
            w.invoke_query_changed(line);
            w.invoke_caret_to_end();
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
        let cat = Rc::clone(&cat);
        window.on_copy_path(move |i| {
            let cat = cat.borrow().clone();
            let Some(path) = path_of(&rows, i) else {
                return;
            };
            println!("{path}");
            if let Some(w) = weak.upgrade() {
                w.set_hint(t(&cat, "path printed to the terminal"));
            }
        });
    }

    // --- the right-click menu --------------------------------------------
    // What the menu says and which items a row gets is `scour_ui::menu`'s.
    {
        let rows = Rc::clone(&rows);
        let picks = Rc::clone(&picks);
        let weak = window.as_weak();
        let cat = Rc::clone(&cat);
        window.on_menu_at(move |i, x, y| {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let picked = picks.borrow().len();
            let is_dir = rows
                .what_at(usize::try_from(i).unwrap_or(0))
                .map(|(_, d, _)| d)
                .unwrap_or(false);
            let mut model: Vec<MenuItem> = Vec::new();
            let mut last: Option<u8> = None;
            for item in scour_ui::menu::items_for(picked, is_dir, scour_ui::faces::Face::Window) {
                model.push(MenuItem {
                    id: item.id.into(),
                    // Through the catalogue's own `{n}`, wherever it puts it.
                    label: t(&cat, item.msgid)
                        .replace("{n}", &grouped(picked.max(1) as u64))
                        .into(),
                    key: item.key.into(),
                    rule: last.is_some_and(|l| l != item.group),
                    careful: item.weight == scour_ui::menu::Weight::Careful,
                    heavy: item.weight == scour_ui::menu::Weight::Heavy,
                    on: false,
                    off: false,
                });
                last = Some(item.group);
            }
            w.set_menu(slint::ModelRc::new(slint::VecModel::from(model)));
            w.set_menu_x(x);
            w.set_menu_y(y);
            w.set_menu_open(true);
        });
    }

    // What a pending question is about. **The paths are taken when the menu is
    // pressed**: a watched index can move a row out from under a selection.
    let pending: Rc<RefCell<Option<(String, Vec<String>)>>> = Rc::new(RefCell::new(None));

    {
        let rows = Rc::clone(&rows);
        let picks = Rc::clone(&picks);
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let model = Rc::clone(&rows);
        let weak = window.as_weak();
        let cat = Rc::clone(&cat);
        let pending = Rc::clone(&pending);
        let addr = addr.clone();
        window.on_menu_pick(move |id| {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            let cat_now = cat.borrow().clone();
            let here = w.get_selected();
            let (path, is_dir, _bytes) = match rows.what_at(usize::try_from(here).unwrap_or(0)) {
                Some(x) => x,
                None => return,
            };
            let chosen: Vec<String> = if picks.borrow().len() > 1 {
                picks.borrow().values().map(|p| p.path.clone()).collect()
            } else {
                vec![path.clone()]
            };
            let say = |w: &MainWindow, msg: slint::SharedString| w.set_hint(msg);
            let folder = |p: &str| match p.rfind('/') {
                Some(0) => "/".to_string(),
                Some(at) => p[..at].to_string(),
                None => ".".to_string(),
            };
            let leaf = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();

            match id.as_str() {
                "open" => open(&path),
                "folder" => open(&folder(&path)),
                "folders" => {
                    for dir in folders_of(&picks.borrow()) {
                        open(&dir);
                    }
                }
                "clear" => {
                    picks.borrow_mut().clear();
                    show_picks(&w, &cat_now, &model, &picks.borrow(), false);
                }

                // The three that are only possible because there is an index.
                "search-here" => {
                    let scope = if is_dir { path.clone() } else { folder(&path) };
                    w.invoke_query_changed(format!("under:{scope}").into());
                }
                "search-kind" => {
                    let name = leaf(&path);
                    match name.rfind('.').filter(|at| *at > 0) {
                        Some(at) => w.invoke_query_changed(
                            format!("ext:{}", name[at + 1..].to_lowercase()).into(),
                        ),
                        None => say(&w, t(&cat_now, "no extension to search for")),
                    }
                }
                "duplicates" | "usage" => {
                    if id == "usage" {
                        w.invoke_query_changed(format!("under:{path}").into());
                    }
                    w.set_tab("report".into());
                }
                "skip" => {
                    w.set_panel("rules".into());
                    say(&w, leaf(&path).into());
                }

                "copy-path" | "copy-name" => {
                    let text = if id == "copy-name" {
                        chosen
                            .iter()
                            .map(|p| leaf(p))
                            .collect::<Vec<_>>()
                            .join("\n")
                    } else {
                        chosen.join("\n")
                    };
                    match scour_clip::text(&text) {
                        Ok(()) => say(&w, t(&cat_now, "path copied")),
                        // No helper on this machine: the path still goes to stdout.
                        Err(e) => {
                            println!("{text}");
                            say(&w, format!("{e}").into());
                        }
                    }
                }
                "copy-file" => {
                    let paths: Vec<&std::path::Path> =
                        chosen.iter().map(std::path::Path::new).collect();
                    match scour_clip::files(&paths) {
                        Ok(()) => say(&w, t(&cat_now, "path copied")),
                        Err(e) => say(&w, format!("{e}").into()),
                    }
                }
                "details" => w.set_peeking(!w.get_peeking()),
                "csv" => w.invoke_tool_clicked("csv".into()),

                "open-with" => {
                    // The menu becomes the list rather than growing a submenu;
                    // the ids carry a prefix so the press comes back knowing.
                    let name = leaf(&path);
                    let mime = scour_thumbs::known::known().mime_of(&name).unwrap_or("");
                    let found = scour_openers::openers(mime);
                    if found.is_empty() {
                        say(&w, t(&cat_now, "nothing on this machine claims it"));
                        return;
                    }
                    let model: Vec<MenuItem> = found
                        .iter()
                        .map(|o| MenuItem {
                            id: format!("open-with:{}", o.id).into(),
                            label: if o.preferred {
                                format!("★ {}", o.name).into()
                            } else {
                                o.name.as_str().into()
                            },
                            key: "".into(),
                            rule: false,
                            careful: false,
                            heavy: false,
                            on: false,
                            off: false,
                        })
                        .collect();
                    w.set_menu(slint::ModelRc::new(slint::VecModel::from(model)));
                    w.set_menu_open(true);
                }

                "rename" => {
                    let was = leaf(&path);
                    w.set_ask_title(t(&cat_now, "Rename…"));
                    w.set_ask_body(path.as_str().into());
                    w.set_ask_text(was.as_str().into());
                    w.set_ask_typing(true);
                    w.set_ask_yes(t(&cat_now, "Rename"));
                    w.set_ask_no(t(&cat_now, "Cancel"));
                    *pending.borrow_mut() = Some(("rename".into(), vec![path.clone()]));
                    w.set_ask_open(true);
                }

                // The two that ask first; everything above happens on the press.
                "trash" | "open-all" => {
                    let title = t(
                        &cat_now,
                        if id == "trash" {
                            "Move to the wastebasket"
                        } else {
                            "Open all {n}…"
                        },
                    )
                    .replace("{n}", &grouped(chosen.len() as u64));
                    // Eight names and a count: a longer list is one nobody reads.
                    let mut body: Vec<String> = chosen.iter().take(8).map(|p| leaf(p)).collect();
                    if chosen.len() > 8 {
                        body.push(format!("… +{}", grouped((chosen.len() - 8) as u64)));
                    }
                    w.set_ask_title(title.into());
                    w.set_ask_body(body.join("\n").into());
                    w.set_ask_yes(t(&cat_now, if id == "trash" { "Move" } else { "Open" }));
                    w.set_ask_no(t(&cat_now, "Cancel"));
                    w.set_ask_typing(false);
                    *pending.borrow_mut() = Some((id.to_string(), chosen));
                    w.set_ask_open(true);
                }

                other if other.starts_with("open-with:") => {
                    let wanted = &other["open-with:".len()..];
                    let name = leaf(&path);
                    let mime = scour_thumbs::known::known().mime_of(&name).unwrap_or("");
                    match scour_openers::openers(mime)
                        .into_iter()
                        .find(|o| o.id == wanted)
                    {
                        Some(chosen) => {
                            if let Err(e) =
                                scour_openers::launch(&chosen, std::path::Path::new(&path))
                            {
                                say(&w, format!("{e}").into());
                            }
                        }
                        None => say(&w, t(&cat_now, "nothing on this machine claims it")),
                    }
                }
                _ => say(&w, t(&cat_now, "not in this face yet")),
            }
            let _ = (&state, &link, &addr);
        });
    }

    {
        let link = Rc::clone(&link);
        let state = Rc::clone(&state);
        let model = Rc::clone(&rows);
        let weak = window.as_weak();
        let cat = Rc::clone(&cat);
        let pending = Rc::clone(&pending);
        let addr = addr.clone();
        window.on_ask_answer(move |yes| {
            stir(&state, &link);
            let Some((what, paths)) = pending.borrow_mut().take() else {
                return;
            };
            if !yes {
                return;
            }
            let Some(w) = weak.upgrade() else { return };
            let cat_now = cat.borrow().clone();
            if what == "rename" {
                let Some(from) = paths.first() else { return };
                let asked = w.get_ask_text().to_string();
                match scour_name::rename(std::path::Path::new(from), &asked) {
                    Ok(now) => {
                        // Both ends: one path is gone, the other has appeared.
                        recheck(
                            &addr,
                            vec![from.clone(), now.to_string_lossy().into_owned()],
                        );
                        let query = state.borrow().query.clone();
                        w.invoke_query_changed(query.into());
                    }
                    Err(why) => w.set_hint(t(&cat_now, why.msgid())),
                }
                return;
            }
            if what == "open-all" {
                for p in &paths {
                    open(p);
                }
                return;
            }
            // The move is this process's; the service is only told to look again.
            let mut gone = 0usize;
            let mut refused: Option<String> = None;
            for p in &paths {
                match scour_trash::trash(std::path::Path::new(p)) {
                    Ok(_) => gone += 1,
                    Err(e) => {
                        refused.get_or_insert_with(|| e.to_string());
                    }
                }
            }
            recheck(&addr, paths);
            w.set_hint(match refused {
                Some(why) => why.into(),
                None => t(&cat_now, "{n} moved to the wastebasket")
                    .replace("{n}", &grouped(gone as u64))
                    .into(),
            });
            let query = state.borrow().query.clone();
            w.invoke_query_changed(query.into());
            let _ = (&link, &model);
        });
    }

    // `SCOUR_GUI_PANIC=1` stands on the hook, and leaves a real crash's file.
    if std::env::var("SCOUR_GUI_PANIC").is_ok() {
        panic!("deliberate — proving the crash log writes");
    }

    if !args.query.is_empty() {
        window.set_query(args.query.clone().into());
        window.invoke_query_changed(args.query.clone().into());
    }

    // A long line in one go: how to stand on the renderer's `i16` ceiling.
    if let Some(n) = std::env::var("SCOUR_GUI_LONGQUERY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        let q: String = "m".repeat(n);
        window.set_query(q.clone().into());
        window.invoke_query_changed(q.into());
    }

    // `SCOUR_SELFTEST=rapor` raises `query-changed` exactly as the field does.
    if let Ok(q) = std::env::var("SCOUR_SELFTEST") {
        // One character at a time, on the clock: the number is key to pixels.
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

    // **What scrolled into sight.** See [`follow`] for the decision.
    {
        let rows = Rc::clone(&rows);
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let weak = window.as_weak();
        window.on_moved(move || {
            stir(&state, &link);
            if let Some(w) = weak.upgrade() {
                follow(&w, &state, &link, &rows);
            }
        });
    }
    // The backstop, for what changes the view without moving the list: ten times
    // a second is cheap to leave running and never what scrolling waits for.
    {
        let rows = Rc::clone(&rows);
        let lines = Rc::clone(&lines);
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let words = Rc::clone(&cat);
        let weak = window.as_weak();
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(100),
            move || {
                let Some(w) = weak.upgrade() else { return };
                // The window is what knows how wide a line is: mode and width
                // both decide it, and both change without asking.
                lines.per_line(if w.get_grid() {
                    w.get_per_line().max(1) as usize
                } else {
                    0
                });
                lines.sync();
                follow(&w, &state, &link, &rows);
                pictures(&w, &state, &link, &rows, &lines);
                peek(&w, &state, &link, &rows, &words.borrow().clone());
            },
        );
    }

    // The scopes, once. On the slow lane, which does not coalesce them away.
    link.send(Ask::Places);
    link.send(Ask::Status);
    // The rules at the start, not only when the panel opens: the switched-off
    // list is held whole, and cannot be edited without being replaced.
    link.send(Ask::Rules);
    trace(&format!("first search sent {:.1?} in", launched.elapsed()));
    dispatch(
        &state,
        &link,
        &rows,
        window.get_visible_rows().max(0) as u32,
    );
    FIRST.with(|f| f.set(Some(launched)));
    if let Ok(scheme) = std::env::var("SCOUR_GUI_SCHEME") {
        window.global::<Theme>().set_dark(scheme != "light");
    }

    // Scroll before the snapshot. A comma-separated list is walked a step at a
    // time: a window that fetches a page per frame only says so while moving.
    if let Ok(spec) = std::env::var("SCOUR_GUI_SCROLL") {
        let stops: Vec<f32> = spec
            .split(',')
            .filter_map(|px| px.trim().parse().ok())
            .collect();
        // How long a stop lasts. A drag is `SCOUR_GUI_SCROLL_MS=16`.
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

    if let Ok(n) = std::env::var("SCOUR_GUI_PICK")
        && let Ok(n) = n.parse::<usize>()
    {
        let picks = Rc::clone(&picks);
        let rows = Rc::clone(&rows);
        let cat = Rc::clone(&cat);
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1200), move || {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let mut held = picks.borrow_mut();
            for row in 0..n {
                if let Some(pick) = rows.pick_at(row) {
                    held.insert(row, pick);
                }
            }
            show_picks(&w, &cat, &rows, &held, false);
        });
    }

    if let Ok(text) = std::env::var("SCOUR_GUI_ADD_RULE") {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_rule_added(text.as_str().into());
            }
        });
    }
    if let Ok(spec) = std::env::var("SCOUR_GUI_DROP_RULE")
        && let Some((kind, value)) = spec.split_once(':')
    {
        let weak = window.as_weak();
        let (kind, value) = (kind.to_string(), value.to_string());
        slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_rule_dropped(kind.as_str().into(), value.as_str().into());
            }
        });
    }

    if let Ok(id) = std::env::var("SCOUR_GUI_RULE") {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_rule_toggled(id.as_str().into());
            }
        });
    }

    if let Ok(what) = std::env::var("SCOUR_GUI_TOOL") {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1200), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_tool_clicked(what.as_str().into());
            }
        });
    }

    if let Ok(tag) = std::env::var("SCOUR_GUI_LANG") {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_language_picked(tag.as_str().into());
            }
        });
    }

    if std::env::var("SCOUR_GUI_TAB").as_deref() == Ok("report") {
        window.set_tab("report".into());
        // The duplicate hunt is the one part of the report nobody runs unasked.
        window.set_dupes_open(std::env::var_os("SCOUR_GUI_DUPES").is_some());
        window.invoke_report_open(slint::SharedString::new());
    }

    if let Ok(px) = std::env::var("SCOUR_GUI_RAIL")
        && let Ok(px) = px.parse::<f32>()
    {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_scroll_rail(px);
            }
        });
    }

    if std::env::var_os("SCOUR_GUI_PEEK").is_some() {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(400), move || {
            if let Some(w) = weak.upgrade()
                && !w.get_peeking()
            {
                w.invoke_peek_toggled();
            }
        });
    }

    // Text set from Rust leaves the caret at nought.
    if std::env::var_os("SCOUR_GUI_END").is_some() {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(900), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_caret_to_end();
            }
        });
    }

    window.set_trace(std::env::var("SCOUR_TRACE").is_ok());

    // `SCOUR_GUI_KEY=ctrl+a`: chords by commas, modifiers by `+`, a bare word one
    // of Slint's named keys.
    if let Ok(spec) = std::env::var("SCOUR_GUI_KEY") {
        let chords: Vec<String> = spec.split(',').map(|c| c.trim().to_string()).collect();
        let after = std::env::var("SCOUR_GUI_KEY_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(1000);
        let weak = window.as_weak();
        // **One chord a tick**: seven backspaces in one callback are one repaint,
        // and a stale frame between keystrokes is what this has to catch.
        let every = std::env::var("SCOUR_GUI_KEY_EVERY")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(160);
        slint::Timer::single_shot(std::time::Duration::from_millis(after), move || {
            let Some(w) = weak.upgrade() else { return };
            let mut left = chords.clone().into_iter();
            let weak = w.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            t.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(every),
                move || {
                    let Some(w) = weak.upgrade() else { return };
                    let Some(chord) = left.next() else { return };
                    let chords = [chord];
                    for chord in &chords {
                        let mut held: Vec<slint::SharedString> = Vec::new();
                        let mut key = slint::SharedString::new();
                        for part in chord.split('+') {
                            match part.to_ascii_lowercase().as_str() {
                                "ctrl" | "control" => {
                                    held.push(slint::platform::Key::Control.into())
                                }
                                "shift" => held.push(slint::platform::Key::Shift.into()),
                                "alt" => held.push(slint::platform::Key::Alt.into()),
                                "meta" | "super" => held.push(slint::platform::Key::Meta.into()),
                                _ => key = named_key(part),
                            }
                        }
                        trace(&format!("synthetic key {chord}"));
                        for m in &held {
                            w.window()
                                .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                                    text: m.clone(),
                                });
                        }
                        w.window()
                            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                                text: key.clone(),
                            });
                        w.window()
                            .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                                text: key,
                            });
                        for m in held.iter().rev() {
                            w.window()
                                .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                                    text: m.clone(),
                                });
                        }
                    }
                },
            );
        });
    }

    // `SCOUR_GUI_MENU=3` opens the menu on the fourth row: a pointer event cannot
    // be synthesised into a Slint window from outside it.
    if let Some(row) = std::env::var("SCOUR_GUI_MENU")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
    {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1400), move || {
            if let Some(w) = weak.upgrade() {
                w.set_selected(row);
                w.invoke_menu_at(row, 360.0 * 1.0, 300.0 * 1.0);
            }
        });
    }

    if let Ok(mode) = std::env::var("SCOUR_GUI_VIEW") {
        window.set_view_mode(mode.as_str().into());
    }

    // Press a rail row before the window opens — the filter slot, not the text.
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
        if which == "columns" {
            // Where the ⋮ is, roughly: nothing has been laid out yet.
            window.invoke_columns_clicked(1500.0, 150.0);
        }
        // Press it, do not set it: setting first makes the press read as a second.
        window.invoke_tool_clicked(which.as_str().into());
    }

    if let Ok(q) = std::env::var("SCOUR_GUI_QUERY") {
        // Set, then tell once: both produced "rapor", "erapor", "emrapor".
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

    // `SCOUR_GUI_CLICK=900,146`: the only way to ask whether the window's hit test
    // agrees with what it drew. Logical pixels.
    if let Ok(spec) = std::env::var("SCOUR_GUI_CLICK") {
        let point: Vec<f32> = spec
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        if let [x, first, last, step] = point[..] {
            // A walk of presses down a column: `x,first,last,step`.
            let weak = window.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            let mut y = first;
            t.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(500),
                move || {
                    let Some(w) = weak.upgrade() else { return };
                    if y > last {
                        return;
                    }
                    let at = slint::LogicalPosition::new(x, y);
                    trace(&format!("press at {x},{y}"));
                    for e in [
                        slint::platform::WindowEvent::PointerMoved { position: at },
                        slint::platform::WindowEvent::PointerPressed {
                            position: at,
                            button: slint::platform::PointerEventButton::Left,
                        },
                        slint::platform::WindowEvent::PointerReleased {
                            position: at,
                            button: slint::platform::PointerEventButton::Left,
                        },
                    ] {
                        w.window().dispatch_event(e);
                    }
                    y += step;
                },
            );
        } else if let [x, y] = point[..] {
            let weak = window.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            let after = std::env::var("SCOUR_GUI_CLICK_MS")
                .ok()
                .and_then(|ms| ms.parse().ok())
                .unwrap_or(2500);
            t.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(after),
                move || {
                    let Some(w) = weak.upgrade() else { return };
                    let at = slint::LogicalPosition::new(x, y);
                    trace(&format!("synthetic click at {x},{y}"));
                    w.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                            position: at,
                        });
                    w.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                            position: at,
                            button: slint::platform::PointerEventButton::Left,
                        });
                    // Released a moment later: one tick is not what a hand does.
                    let weak = w.as_weak();
                    let r = Box::leak(Box::new(slint::Timer::default()));
                    r.start(
                        slint::TimerMode::SingleShot,
                        std::time::Duration::from_millis(90),
                        move || {
                            let Some(w) = weak.upgrade() else { return };
                            w.window().dispatch_event(
                                slint::platform::WindowEvent::PointerReleased {
                                    position: at,
                                    button: slint::platform::PointerEventButton::Left,
                                },
                            );
                        },
                    );
                },
            );
        }
    }

    // `SCOUR_GUI_HOVER=900:120,160,200` prints which row each stop lands on.
    if let Ok(spec) = std::env::var("SCOUR_GUI_HOVER")
        && let Some((x, ys)) = spec.split_once(':')
    {
        let x: f32 = x.trim().parse().unwrap_or(0.0);
        let stops: Vec<f32> = ys
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        let weak = window.as_weak();
        let t = Box::leak(Box::new(slint::Timer::default()));
        let mut left = stops.into_iter();
        let mut first = true;
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(400),
            move || {
                if first {
                    first = false;
                    return;
                }
                let Some(w) = weak.upgrade() else { return };
                let Some(y) = left.next() else { return };
                trace(&format!("pointer to {x},{y}"));
                w.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                        position: slint::LogicalPosition::new(x, y),
                    });
            },
        );
    }

    // Photograph the window and leave, when asked. See [`snapshot`].
    if let Ok(path) = std::env::var("SCOUR_GUI_SNAP") {
        let weak = window.as_weak();
        // Held rather than dropped: a `Timer` that goes out of scope never fires.
        let t = Box::leak(Box::new(slint::Timer::default()));
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
    #[cfg(unix)]
    drop(claim);
    Ok(())
}

/// Keep the preview panel on whatever is selected. **Asked once per row, not per
/// tick**: what it is showing is remembered as a path.
fn peek(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    rows: &Rc<rows::Rows>,
    cat: &Catalogue,
) {
    if !w.get_peeking() {
        if !state.borrow().peek_path.is_empty() {
            state.borrow_mut().peek_path.clear();
        }
        return;
    }
    let path = path_of(rows, w.get_selected()).unwrap_or_default();
    if path == state.borrow().peek_path {
        return;
    }
    state.borrow_mut().peek_path = path.clone();
    w.set_peek_shot(rows::blank());
    w.set_peek_has_shot(false);
    w.set_peek_text(slint::SharedString::new());
    if path.is_empty() {
        w.set_peek_name(slint::SharedString::new());
        w.set_peek_path(slint::SharedString::new());
        w.set_peek_note(slint::SharedString::new());
        w.set_peek_facts(ModelRc::new(VecModel::from(Vec::<Fact>::new())));
        return;
    }
    w.set_peek_name(scour_ui::path::leaf(&path).into());
    w.set_peek_path(path.as_str().into());
    w.set_peek_note(t(cat, "reading…"));
    link.send(Ask::Peek { path: path.clone() });
    link.send(Ask::PeekFacts { path });
}

/// Pictures for what is on screen: draw the ones the desktop has made, ask for
/// the ones it could make. **After the drawing, on the tick, never on the path a
/// keystroke takes** — see [`rows::Rows::look_for_pictures`] for every bound.
fn pictures(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    rows: &Rc<rows::Rows>,
    lines: &Rc<rows::Lines>,
) {
    // Only where a picture is drawn: the detail list shows the kind's glyph.
    if !w.get_grid() {
        return;
    }
    /// How many rows are looked at per tick. Ten ticks a second.
    const LOOKED_AT: usize = 24;
    /// How far past the bottom of the window to look.
    const AHEAD: usize = 12;
    /// How many paths the "already asked" memory holds: a few screenfuls either way.
    const REMEMBERED: usize = 4096;
    let visible = w.get_visible_rows().max(0) as usize;
    let first = w.get_first_row().max(0) as usize;
    let (drawn, ask) = rows.look_for_pictures(first, first + visible + AHEAD, LOOKED_AT);
    if drawn > 0 {
        trace(&format!("{drawn} picture(s) drawn"));
        lines.touched(first, first + visible + AHEAD);
    }
    if ask.is_empty() {
        return;
    }
    let mut s = state.borrow_mut();
    if s.asking_pictures {
        return;
    }
    let already: std::collections::HashSet<&str> =
        s.asked_pictures.iter().map(String::as_str).collect();
    let ask: Vec<String> = ask
        .iter()
        .filter(|p| !already.contains(p.as_str()))
        .cloned()
        .collect();
    if ask.is_empty() {
        return;
    }
    for path in &ask {
        if s.asked_pictures.len() >= REMEMBERED {
            s.asked_pictures.pop_front();
        }
        s.asked_pictures.push_back(path.clone());
    }
    s.asking_pictures = true;
    drop(s);
    link.send(Ask::Thumbnails { files: ask });
}

fn fetch_from(first: usize, visible: usize, wanted: Option<usize>) -> usize {
    wanted
        .filter(|row| (first..=first.saturating_add(visible)).contains(row))
        .unwrap_or(first)
}

/// Refresh is optional; its rest starts after the reply, so a slow response
/// cannot spend the delay in flight and repeat at once.
fn refresh_ready(elapsed: Option<std::time::Duration>, cost_us: u64) -> bool {
    let rest = SETTLED.max(std::time::Duration::from_micros(cost_us).saturating_mul(10));
    elapsed.is_none_or(|spent| spent >= rest)
}

/// Somebody did something here: restarts [`AWAKE_FOR`] and, if the window had
/// dozed, asks at once rather than waiting out [`DOZE_AGAIN`].
fn stir(state: &Rc<RefCell<State>>, link: &Rc<Link>) {
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

/// Fetch the page the list is about to need, if it is not already coming.
/// **Fetching cannot happen where the need is noticed**: `row_data` runs while
/// the view is laying out. The position is read, not only a reported miss.
fn follow(w: &MainWindow, state: &Rc<RefCell<State>>, link: &Rc<Link>, rows: &Rc<rows::Rows>) {
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
fn dispatch(state: &Rc<RefCell<State>>, link: &Rc<Link>, model: &Rc<rows::Rows>, rows: u32) {
    let limit = rows.clamp(20, PAGE_MAX);
    {
        let mut s = state.borrow_mut();
        s.row_limit = limit;
        s.page_offset = 0;
        s.rewind = true;
    }
    // Keeping the pages in hand across a new query would mean scrolling down
    // into the last query's results.
    model.empty();
    send_search(state, link, 0, limit);
}

/// How long the result is, given a page of it and a count of it. **A page shorter
/// than the service is willing to give is the end of the result**, whatever a
/// count taken a moment ago says: shorter than asked for, and than ever served.
fn list_length(counted: usize, offset: usize, got: usize, asked: usize, served: usize) -> usize {
    if got < asked && got < served {
        return offset + got;
    }
    // Otherwise the count stands — but never below what is already in hand.
    counted.max(offset + got)
}

fn send_search(state: &Rc<RefCell<State>>, link: &Rc<Link>, offset: u32, limit: u32) {
    // Beside the search, on the same lane and with the same coalescing.
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

/// Another page of the query already on screen. Scrolling and the live refresh
/// come here rather than [`send_search`]: the query has not changed.
fn send_page(state: &Rc<RefCell<State>>, link: &Rc<Link>, offset: u32, limit: u32) {
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

/// What was searched before, narrowed by what is typed now.
fn past_matching(past: &[String], typed: &str) -> Vec<slint::SharedString> {
    let typed = typed.trim().to_lowercase();
    past.iter()
        .filter(|line| typed.is_empty() || line.to_lowercase().contains(&typed))
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
fn remember(state: &Rc<RefCell<State>>, link: &Rc<Link>, query: &str) {
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
fn painted(query: &str, spans: &[scour_core::Span]) -> Vec<Span> {
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
        add(
            &query[start..end],
            // The page's table, role for role.
            match sp.role {
                scour_core::Role::Field | scour_core::Role::Cmp | scour_core::Role::Or => 1,
                scour_core::Role::Value | scour_core::Role::Phrase => 2,
                scour_core::Role::Glob => 3,
                scour_core::Role::Not => 4,
                scour_core::Role::UnknownField | scour_core::Role::BadValue => 5,
                // What is being looked for, which is not "everything else".
                scour_core::Role::Text => 6,
                // The syntax between terms, said quietly.
                scour_core::Role::Colon
                | scour_core::Role::Sep
                | scour_core::Role::Quote
                | scour_core::Role::Space => 7,
                // **No catch-all**: a role added to `scour_core` stops compiling
                // here instead of silently taking the ordinary ink.
            },
            sp.not,
            &mut out,
            &mut chars,
        );
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

/// The query as the engine gets it: what was typed, plus whatever filter is
/// pressed. The filter is a whole term — the rail, ribbon and scopes share a slot.
fn full_query(s: &State) -> String {
    scour_ui::query::compose(&s.query, s.facet.as_deref())
}

/// What the rail and the ribbon are counted over: the typed query, without the
/// filter they themselves offered. Counted through it, a rail has one row.
fn facet_query(s: &State) -> String {
    s.query.trim().to_owned()
}

#[allow(clippy::too_many_arguments)]
fn apply(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    table: &Rc<RefCell<Table>>,
    rows: &Rc<rows::Rows>,
    lines: &Rc<rows::Lines>,
    picks: &Rc<RefCell<std::collections::BTreeMap<usize, rows::Pick>>>,
    facets: &Rc<VecModel<Facet>>,
    rules: &[Rc<VecModel<Rule>>; 3],
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
            said(
                w,
                format!("{} — {why}", cat.get("the service is not running")).into(),
            );
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
                    // Facets and the count refine a list already visible.
                    trace(&format!("background request refused: {why}"));
                    return;
                }
            }
            // Shown rather than swallowed: an empty list with no reason reads as
            // a broken tool.
            w.set_busy(false);
            said(w, why.into());
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
            // Read once: a page is two hundred rows and the catalogue is a lookup.
            let frozen_note = t(
                cat,
                "this volume does not record reads (noatime) — the number shown would be left over from when the file was created",
            );
            let page: Vec<Row> = r
                .hits
                .iter()
                // The kind's word comes from the catalogue by the engine's own
                // msgid. Which rows are arrivals is the model's — see `Rows::put`.
                .map(|h| {
                    rows::row_of(
                        h,
                        &terms,
                        &rows::Shape {
                            columns: &table.borrow().shown,
                            mounts: &state.borrow().mounts,
                            kind: &t(cat, h.kind.msgid()),
                            frozen_note: &frozen_note,
                            now,
                        },
                        false,
                    )
                })
                .collect();
            // Beside the page rather than on it: Slint counts in 32 bits.
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
                // Carried by the answer: state may already name a later page.
                s.page_offset = offset;
                let arrived = rows.put(offset as usize / rows::SPAN, page, weights, total);
                // The tiles hold the same rows, and the result may be longer.
                lines.touched(offset as usize, offset as usize + n);
                lines.sync();
                arrived
            };

            // Slint's `animate` interpolates when a property *changes*, and the
            // arrival wash is 1.6 s.
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
            {
                let mut s = state.borrow_mut();
                s.page_cost_us = r.took_us.max(
                    s.page_sent
                        .map(|t| t.elapsed().as_micros().min(u64::MAX as u128) as u64)
                        .unwrap_or(0),
                );
                s.page_landed_at = Some(std::time::Instant::now());
            }
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
            // What was drawn, not how much: a count agrees with the meter without
            // saying the rows are the right rows.
            trace(&format!(
                "first row {:?}",
                r.hits.first().map(|h| h.path.as_str()).unwrap_or("")
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
                    // Without the rail's own filter: the search wants `full_query`.
                    facet_query(&s),
                    ask_background,
                    s.exact_count.filter(|c| c.query_revision == query_revision),
                )
            };
            // **A page landing never moves the viewport. A new question does.**
            // Rows are drawn at their place in the whole result, and the flag is
            // set where the question is asked rather than worked out here.
            let rewind = {
                let mut s = state.borrow_mut();
                // Only the answer asked for from the top rewinds.
                let rewind = s.rewind && offset == 0;
                s.rewind &= !rewind;
                rewind
            };
            if rewind {
                w.set_selected(0);
                w.invoke_scroll_to(0.0);
                // A different question, so the old selection answers nothing.
                picks.borrow_mut().clear();
            }
            // The marks live on the rows, and this page brought new ones.
            if !picks.borrow().is_empty() || w.get_picked() > 0 {
                let asking = w.get_pick_asking() && !picks.borrow().is_empty();
                show_picks(w, cat, rows, &picks.borrow(), asking);
            }
            w.set_busy(false);
            // **And straight on to the next**: nothing is asked for while an
            // answer is in flight, so a drag continues here.
            follow(w, state, link, rows);
            // Now, and only now, the sidebar: it costs about what the search did.
            if ask_background {
                schedule_background(state, link, query_revision, query);
            }
            let (total, capped) = exact_count
                .map(|c| (c.total, c.capped))
                .unwrap_or((r.total, r.capped));
            // The page's sentence, in the page's order: found out of how many
            // there are, what it cost, how much of the index was walked.
            let indexed = state.borrow().indexed;
            meter(
                w,
                format!(
                    "{}{} / {}",
                    grouped(total),
                    if capped { "+" } else { "" },
                    grouped(indexed),
                ),
                format!("{:.2} ms", r.took_us as f64 / 1000.0),
                format!("{} {}", grouped(r.rows_visited), cat.get("rows read")),
            );
        }
        // The exact total the interactive search did not stop to compute: it lands
        // after the list, so the meter tightens from `1000+` to a number.
        Got::Count {
            query_revision,
            reply,
        } => {
            if query_revision != state.borrow().query_revision {
                return;
            }
            if let Response::Count {
                total,
                capped,
                took_us: _,
                misread: _,
            } = *reply
            {
                let indexed = state.borrow().indexed;
                state.borrow_mut().exact_count = Some(ExactCount {
                    query_revision,
                    total,
                    capped,
                });
                // And the list gets as long as the answer: the interactive search
                // counts only to its cap, so the list ran out above the result.
                rows.set_total(total.min(i32::MAX as u64) as usize);
                // Only the second number moves: a meter that reflows reads as a
                // window changing its mind.
                trace(&format!(
                    "meter/search {}{} / {}",
                    grouped(total),
                    if capped { "+" } else { "" },
                    grouped(indexed)
                ));
                w.set_meter_count(
                    format!(
                        "{}{} / {}",
                        grouped(total),
                        if capped { "+" } else { "" },
                        grouped(indexed),
                    )
                    .into(),
                );
            }
        }
        // The query, read back and cut into runs, painted under the box. Two of
        // the engine's roles say *this is not what you think it is*: without them
        // a mistyped `sizE:>1mb` answers `0 of 0`, like a query matching nothing.
        Got::Explain {
            query_revision,
            reply,
        } => {
            trace(&format!(
                "explain {query_revision} (current {})",
                state.borrow().query_revision
            ));
            if query_revision != state.borrow().query_revision {
                return;
            }
            let Response::Explain {
                spans, description, ..
            } = *reply
            else {
                return;
            };
            let query = full_query(&state.borrow());
            state.borrow_mut().spans = spans;
            let runs = painted(&query, &state.borrow().spans);
            w.set_spans(ModelRc::new(VecModel::from(runs)));
            // The colours say what a piece is; this says what the whole asks.
            let spans = &state.borrow().spans;
            let warn = spans.iter().any(|sp| sp.role.is_warning());
            let telling = warn
                || spans.iter().any(|sp| sp.role.is_telling())
                || spans.iter().any(|sp| {
                    sp.role == scour_core::Role::Text
                        && scour_ui::MISTAKEN
                            .iter()
                            .any(|w| sp.of(&query).eq_ignore_ascii_case(w))
                });
            w.set_reading(if telling {
                description.as_str().into()
            } else {
                slint::SharedString::new()
            });
            w.set_reading_warn(warn);
        }
        // What a folder weighs, asked for whenever the report scope changes.
        Got::Usage { path, reply } => {
            let Response::Usage(u) = *reply else { return };
            // A scope nobody is looking at any more: weighing is a walk.
            if path != state.borrow().scope {
                return;
            }
            show_usage(w, cat, &path, &u);
        }
        // What kinds the weight under a folder is in, drawn as a share.
        Got::Kinds { path, reply } => {
            let Response::Facets(f) = *reply else { return };
            if path != state.borrow().scope {
                return;
            }
            let kinds = f
                .groups
                .iter()
                .find(|g| matches!(g.by, scour_core::FacetBy::Kind));
            let Some(group) = kinds else { return };
            let most = group
                .facets
                .iter()
                .map(|k| k.count)
                .max()
                .unwrap_or(1)
                .max(1);
            // The kind's word comes from the engine's own msgid.
            let mut rows: Vec<Facet> = Vec::new();
            for kind in rows::offered_kinds() {
                let token = kind.token();
                let Some(hit) = group.facets.iter().find(|x| x.key == token) else {
                    continue;
                };
                rows.push(Facet {
                    label: t(cat, kind.msgid()),
                    token: token.into(),
                    count: grouped(hit.count).into(),
                    share: hit.count as f32 / most as f32,
                });
            }
            w.set_report_kinds(ModelRc::new(VecModel::from(rows)));
        }
        // The heaviest files under it, with no count: counting is the one piece of
        // work proportional to how many match.
        Got::Biggest { path, reply } => {
            let Response::Search(r) = *reply else { return };
            if path != state.borrow().scope {
                return;
            }
            let rows: Vec<Facet> = r
                .hits
                .iter()
                .map(|h| Facet {
                    label: h.name().into(),
                    token: h.path.as_str().into(),
                    count: compact_bytes(h.meta.size.max(0) as u64).into(),
                    share: 0.0,
                })
                .collect();
            w.set_report_big(ModelRc::new(VecModel::from(rows)));
        }
        // The pictures the service managed to make. `ran` is how many processes a
        // screenful of unseen files actually starts.
        Got::Thumbnails(reply) => {
            state.borrow_mut().asking_pictures = false;
            let Response::Thumbnails(made) = *reply else {
                return;
            };
            trace(&format!(
                "{} picture(s) ready, {} thumbnailer(s) run",
                made.ready.len(),
                made.ran
            ));
            rows.made_pictures(&made.ready);
            // The preview panel may have been the one waiting for it.
            let waiting = state.borrow().peek_path.clone();
            if !waiting.is_empty() && made.ready.contains(&waiting) {
                link.send(Ask::Peek { path: waiting });
            }
        }
        // The panel's two answers, tagged with the path: either may arrive for a
        // row the arrows have left, and is then dropped.
        Got::Peek { path, reply } => {
            if path != state.borrow().peek_path {
                return;
            }
            match *reply {
                Response::Preview(look) => {
                    show_peek(w, cat, link, &path, &look);
                }
                Response::Stat(entry) => w.set_peek_facts(peek_facts(cat, &entry)),
                _ => {}
            }
        }
        // **Being the same size is not being the same file**: only `content` —
        // read end to end and compared — licenses the word "duplicate".
        Got::Dupes(reply) => {
            let Response::Duplicates {
                groups,
                candidates,
                waste,
                proven,
                unconfirmed,
                ..
            } = *reply
            else {
                return;
            };
            w.set_dupes_sum(
                t(cat, "{proven} confirmed · {waste} candidate · {n} files")
                    .replace("{proven}", &compact_bytes(proven))
                    .replace("{waste}", &compact_bytes(waste))
                    .replace("{n}", &grouped(candidates))
                    .into(),
            );
            w.set_dupes_note(
                if unconfirmed == 0 {
                    t(cat, "All of them were compared by reading.").to_string()
                } else {
                    t(
                        cat,
                        "Being the same size is not being the same file. {n} groups were not confirmed by reading — those are candidates, not duplicates.",
                    )
                    .replace("{n}", &grouped(unconfirmed))
                }
                .into(),
            );
            let list: Vec<Dupe> = groups
                .iter()
                .map(|g| Dupe {
                    size: compact_bytes(g.size).into(),
                    times: format!("×{}  ·  {}", g.paths.len(), compact_bytes(g.waste)).into(),
                    certainty: t(
                        cat,
                        match g.certainty.as_str() {
                            "content" => "identical",
                            "edges" => "same ends",
                            _ => "same size only",
                        },
                    ),
                    proven: g.certainty == "content",
                    // Six and a count: twenty-eight copies is a fact about the group.
                    paths: {
                        let mut lines: Vec<String> =
                            g.paths.iter().take(SHOWN_PATHS).cloned().collect();
                        if g.paths.len() > SHOWN_PATHS {
                            lines.push(format!(
                                "… {}",
                                t(cat, "{n} more")
                                    .replace("{n}", &grouped((g.paths.len() - SHOWN_PATHS) as u64))
                            ));
                        }
                        lines.join("\n").into()
                    },
                })
                .collect();
            w.set_dupes(ModelRc::new(VecModel::from(list)));
        }
        // The exclusion rules, in the service's three groups. Only what a window
        // added can be deleted; any of them can be switched off.
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
            // An answer repeating what was sent means the service has caught up;
            // one that does not is older than this window.
            let off = {
                let mut s = state.borrow_mut();
                match &s.sent_off {
                    Some(sent)
                        if sent.len() == off.len() && sent.iter().all(|o| off.contains(o)) =>
                    {
                        s.sent_off = None;
                        off
                    }
                    Some(sent) => sent.clone(),
                    None => off,
                }
            };
            state.borrow_mut().exclude_off = off.clone();
            let is_off = |id: &str| off.iter().any(|o| o.eq_ignore_ascii_case(id));
            // Kept as the answer gave it: deleting one sends the list back short.
            {
                let mut s = state.borrow_mut();
                s.added_paths = added_paths.clone();
                s.added_dirs = added_dirs.clone();
                s.added_files = added_files.clone();
            }
            let group = |rows: Vec<(&str, Vec<String>)>, removable: bool| {
                let mut out: Vec<Rule> = Vec::new();
                for (kind, list) in rows {
                    for value in list {
                        let id = scour_settings::rule_id(kind, &value);
                        out.push(Rule {
                            off: is_off(&id),
                            id: id.as_str().into(),
                            value: value.as_str().into(),
                            kind: kind.into(),
                            removable,
                        });
                    }
                }
                out
            };
            // One string per list, so a repeated answer is recognised early.
            let mark = |rows: &[(&str, Vec<String>)]| -> String {
                let mut out = String::new();
                for (kind, list) in rows {
                    for value in list {
                        let id = scour_settings::rule_id(kind, value);
                        out.push_str(&id);
                        out.push(if is_off(&id) { '-' } else { '+' });
                        out.push('\n');
                    }
                }
                out
            };
            let added = vec![
                ("path", added_paths),
                ("dir", added_dirs),
                ("file", added_files),
                ("allow", added_allow),
            ];
            let config = vec![
                ("path", config_paths),
                ("dir", config_dirs),
                ("file", config_files),
                ("allow", config_allow),
            ];
            let builtin = vec![
                ("path", builtin_paths),
                ("dir", builtin_dirs),
                ("file", builtin_files),
            ];
            let fresh = [mark(&added), mark(&config), mark(&builtin)];
            let was = state.borrow().rules_shown.clone();
            let next = [
                group(added, true),
                group(config, false),
                group(builtin, false),
            ];
            for ((model, rows), (before, after)) in
                rules.iter().zip(next).zip(was.iter().zip(fresh.iter()))
            {
                if before == after {
                    continue;
                }
                // Same rules, one switched: write only the rows that differ.
                if model.row_count() == rows.len() {
                    for (at, row) in rows.into_iter().enumerate() {
                        if model.row_data(at).as_ref() != Some(&row) {
                            model.set_row_data(at, row);
                        }
                    }
                } else {
                    model.set_vec(rows);
                }
            }
            state.borrow_mut().rules_shown = fresh;
        }
        // The index moved. The search goes out *before* the next wait, so a burst
        // of changes does not queue a search per change.
        Got::Awake(reply) => {
            let Response::Status(st) = *reply else { return };
            // Free: a wait is answered with the whole status.
            w.set_scanning(scanning_note(cat, &st));
            // **A window nobody is using stops following.** Not "open" — *used*;
            // see [`AWAKE_FOR`], and `stir` undoes it within the frame.
            if state.borrow().stirred.elapsed() >= AWAKE_FOR {
                {
                    let mut s = state.borrow_mut();
                    s.dozing = true;
                    s.revision = st.revision;
                }
                rows.mark(st.revision);
                let link = Rc::clone(link);
                let state = Rc::clone(state);
                slint::Timer::single_shot(DOZE_AGAIN, move || {
                    let (dozing, since) = {
                        let s = state.borrow();
                        (s.dozing, s.revision)
                    };
                    if dozing {
                        link.send(Ask::Await { since });
                    }
                });
                return;
            }
            {
                let mut s = state.borrow_mut();
                if st.revision == s.revision {
                    // The timeout ran out rather than the index moving.
                    link.send(Ask::Await { since: s.revision });
                    return;
                }
                s.revision = st.revision;
            }
            // **Marked, not thrown away**: the page being looked at is re-read at
            // once, the others when somebody looks at them.
            rows.mark(state.borrow().revision);
            follow(w, state, link, rows);
            // A beat before waiting again: the service answers the instant its
            // index moves, and re-arming at once cost a quarter of a core.
            let link = Rc::clone(link);
            let state = Rc::clone(state);
            slint::Timer::single_shot(AWAIT_AGAIN, move || {
                link.send(Ask::Await {
                    since: state.borrow().revision,
                });
            });
        }
        // What the service is holding: 636 MB over three sources is a different
        // claim from the same over one.
        Got::Status(reply) => {
            let Response::Status(st) = *reply else { return };
            // The first revision, and the start of the long poll.
            {
                let mut s = state.borrow_mut();
                s.indexed = st.entries;
                if s.revision == 0 {
                    s.revision = st.revision;
                    link.send(Ask::Await { since: st.revision });
                }
            }
            // Every query reads the unsorted tail: a week of use took ordering by
            // path from 1.9 ms to 21.5, and one rebuild put it back.
            let mut holding = format!(
                "{} {}  ·  {} {}  ·  {} {}",
                t(cat, "index"),
                compact_bytes(st.index_bytes),
                st.sources,
                t(cat, "sources"),
                st.watching,
                t(cat, "watching"),
            );
            if st.rebuild_advised {
                holding.push_str("  ·  ");
                holding.push_str(&t(cat, scour_ui::REBUILD_ADVISED));
            }
            w.set_holding(holding.into());
        }
        // This desktop's own folders, each an `under:` term. The labels are the
        // desktop's own words, so nothing here translates them.
        Got::Places(reply) => {
            let Response::Places(p) = *reply else {
                trace(&format!("places: unexpected reply {reply:?}"));
                return;
            };
            state.borrow_mut().mounts = p.mounts.clone();
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
            half,
            reply,
        } => {
            if query_revision != state.borrow().query_revision {
                return;
            }
            let Response::Facets(f) = *reply else { return };
            // Two questions, one walk: the reply carries a group per question,
            // read apart by their `by`.
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

            if half.kinds() {
                // The bar is that kind's share of the largest, ordered by the
                // taxonomy so the rail does not reshuffle under the pointer.
                let top = kinds.iter().map(|x| x.count).max().unwrap_or(1).max(1);
                let mut fresh: Vec<Facet> = Vec::new();
                for k in rows::offered_kinds() {
                    let token = k.token();
                    let Some(hit) = kinds.iter().find(|x| x.key == token) else {
                        continue;
                    };
                    fresh.push(Facet {
                        label: t(cat, k.msgid()),
                        // The whole term: the slot holds `dm:38d` too.
                        token: format!("kind:{token}").into(),
                        count: compact(hit.count).into(),
                        share: hit.count as f32 / top as f32,
                    });
                }
                facets.set_vec(fresh);
            }

            if half.ages() {
                // Oldest on the left: `bar_edges()` is newest first — it is the
                // list of upper bounds — and the ribbon reads left to right.
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
                // Older than the last edge belongs to the oldest bar: two years
                // is where the scale ends, not the files.
                if let Some(first) = bars.first_mut() {
                    // The oldest bar has no upper bound, so it is not a filter.
                    first.days = 0;
                    first.count += count_of("older");
                    peak = peak.max(first.count);
                }
                w.set_bar_peak(peak);
                w.set_bars(ModelRc::new(VecModel::from(bars)));
            }
            // Only when one walk answered both: a stripped query counts wider.
            if half == Half::Both {
                let indexed = state.borrow().indexed;
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
                // The facet walk counted the whole matching set on its way.
                if !f.capped {
                    rows.set_total(f.total.min(i32::MAX as u64) as usize);
                }
                // The same sentence the search reply draws — see there.
                trace(&format!(
                    "meter/facets {}{} / {}",
                    grouped(f.total),
                    if f.capped { "+" } else { "" },
                    grouped(indexed)
                ));
                w.set_meter_count(
                    format!(
                        "{}{} / {}",
                        grouped(f.total),
                        if f.capped { "+" } else { "" },
                        grouped(indexed),
                    )
                    .into(),
                );
                // Only the facet walk's own safety cap makes a second pass needed.
                if let Some(query) = count_query {
                    link.send(Ask::Count {
                        query_revision,
                        query,
                    });
                }
            }
        }
    }
}

/// The lock a running window holds: a socket beside the service's, removed on
/// exit.
#[cfg(unix)]
struct Claim {
    listener: std::os::unix::net::UnixListener,
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl Drop for Claim {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Claim the one window, or tell the one that has it to show itself.
///
/// `None` means another window answered and this one should leave. A socket
/// file nobody answers on is a crash's leftover and is taken over.
#[cfg(unix)]
fn only_one(service_socket: &str) -> Option<Claim> {
    use std::io::Write;
    use std::os::unix::net::{UnixListener, UnixStream};
    let path = std::path::PathBuf::from(format!("{service_socket}.gui"));
    if let Ok(mut other) = UnixStream::connect(&path) {
        let _ = other.write_all(b"show\n");
        return None;
    }
    let _ = std::fs::remove_file(&path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match UnixListener::bind(&path) {
        Ok(listener) => Some(Claim { listener, path }),
        // Without a lock there is still a window; there may just be two.
        Err(_) => {
            let spare = std::env::temp_dir().join(format!("scour-gui-{}.sock", std::process::id()));
            let _ = std::fs::remove_file(&spare);
            let listener = UnixListener::bind(&spare).ok()?;
            Some(Claim {
                listener,
                path: spare,
            })
        }
    }
}

/// Every connection on the lock is a second start asking for the window.
#[cfg(unix)]
fn answer_the_next_start(
    listener: std::os::unix::net::UnixListener,
    weak: slint::Weak<MainWindow>,
) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            drop(stream);
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(w) = weak.upgrade() else { return };
                use i_slint_backend_winit::WinitWindowAccessor;
                w.window().show().ok();
                w.window().with_winit_window(|ww| {
                    ww.set_minimized(false);
                    ww.focus_window();
                });
            });
        }
    });
}

/// The plain text terms of a query, for highlighting and for nothing else: a
/// `kind:` term matched a *column*, a negated term matched nothing, and quotes
/// hold a phrase together. What a term **means** stays `explain`'s answer.
fn terms_of(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let push = |w: &mut String, quoted: bool, out: &mut Vec<String>| {
        let t = std::mem::take(w);
        // A field term is not text in the name; in quotes a colon is the phrase.
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

/// Tell the service to look at these paths again, now, on a connection of its
/// own: the window's `Link` coalesces for a search box. Failure is silent.
fn recheck(addr: &str, paths: Vec<String>) {
    let addr = addr.to_owned();
    std::thread::spawn(move || {
        if let Ok(mut client) = scour_ipc::Client::connect(&addr) {
            let _ = client.call(scour_proto::Request::Recheck { paths });
        }
    });
}

/// Hand a path to the desktop, spawned and forgotten.
fn open(path: &str) {
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(windows)]
    let cmd = "explorer";
    let _ = std::process::Command::new(cmd).arg(path).spawn();
}

/// Write every string the window shows. It runs again when a language is picked.
fn words(window: &MainWindow, cat: &Catalogue) {
    // Punctuation is part of the language, and this runs whenever it changes.
    MARKS.with(|m| {
        m.set((
            scour_ui::format::group_mark(cat.language()),
            scour_ui::format::decimal_mark(cat.language()),
        ))
    });
    columns(window, cat);
    window.set_tab_search(t(cat, "Search"));
    window.set_tab_report(t(cat, "Report"));
    // The rail's first section is the kinds, and the page calls it `Kind`.
    window.set_scope_label(t(cat, "Kind"));
    window.set_scope_heading(t(cat, "Scope"));
    window.set_size_heading(t(cat, "Size"));
    // Three bands, fixed rather than counted: a count is a walk per band.
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
    window.set_ribbon_label(t(cat, "Time distribution"));
    window.set_help_title(t(cat, "Help"));
    window.set_faces_title(t(cat, "how to run it"));
    window.set_face_window(t(cat, "Window"));
    window.set_face_window_aside(t(cat, "running now"));
    window.set_face_tui(t(cat, "Terminal"));
    window.set_face_tui_note(t(
        cat,
        "Opens a terminal and runs Scour in it. This window stays where it is.",
    ));
    window.set_face_tui_aside(t(cat, "not built yet"));
    window.set_face_web(t(cat, "Browser"));
    // The sentence says what it opens first: a listening port is not what somebody
    // asked for when they asked for a browser.
    window.set_face_web_note(t(
        cat,
        "Starts scour-web, which listens on 127.0.0.1:7621 and opens your browser. Nothing outside this computer can reach it, and the link carries a token that changes every run.",
    ));
    window.set_face_go(t(cat, "open"));
    // The terminal one is offered when there is something to run.
    window.set_face_tui_ready(which("scour-tui").is_some() && which("scour-open").is_some());
    window.set_lang_title(t(cat, "language"));
    window.set_rules_title(t(cat, "What is skipped"));
    window.set_rules_added_title(t(cat, "Added here"));
    window.set_rules_added_note(t(
        cat,
        "The only ones this window may delete. Any rule can be switched off.",
    ));
    window.set_rules_config_title(t(cat, "From config.toml"));
    window.set_rules_config_note(t(
        cat,
        "Written by hand, and left alone — deleting it here would rewrite the file. Switch it off instead.",
    ));
    window.set_rules_builtin_title(t(cat, "Built in"));
    window.set_rules_builtin_note(t(
        cat,
        "Part of the program. Mostly build output and package caches, which churn constantly and bury real results.",
    ));
    window.set_rules_hint(t(cat, "a directory name, or a path"));
    window.set_rules_add(t(cat, "add"));
    window.set_rules_off_word(t(cat, "off"));
    // The page's own legend, stripped of the markup it carries. The terms, then
    // the keys: both are what clicking cannot teach.
    window.set_help_body(
        [
            "A word matches the name. Several words mean all of them.",
            "",
            "  !word           leave it out",
            "  \"two words\"     as written",
            "  *.pdf   rep?rt  wildcards",
            "  a | b           either one",
            "",
            "  ext:pdf         extension",
            "  kind:image      image, video, code, doc, archive, folder…",
            "  size:>10mb      also <, and kb mb gb",
            "  dm:7d           changed in the last 7 days — h, w, m, y too",
            "  dc: da:         created, opened",
            "  under:/home/a   inside that folder",
            "  is:dir          folders only",
            "",
            "Keys",
            "",
            "  Enter           open        Ctrl+Enter   open its folder",
            "  Space           details     Alt+↓        search this folder",
            "  F2              rename      Delete       to the wastebasket",
            "  Ctrl+C          copy path   Ctrl+Shift+C copy name",
            "  ↑ ↓ PgUp PgDn   move        Home End     first, last",
            "  Escape          clear the selection, or close this",
            "",
            "The colour down the left of each row is how long ago it changed: warm for minutes, cold for years.",
        ]
        .iter()
        .map(|line| if line.is_empty() { String::new() } else { plain(&t(cat, line)) })
        .collect::<Vec<_>>()
        .join("\n")
        .into(),
    );
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
    window.set_ribbon_hint(t(cat, "results by date changed"));
    window.set_axis_oldest(t(cat, "2 years ago"));
    window.set_axis_year(t(cat, "1 year"));
    window.set_axis_month(t(cat, "1 month"));
    window.set_axis_week(t(cat, "1 week"));
    window.set_axis_today(t(cat, "today"));
}

/// Hand the window its palette, from `scour-ui`. Both schemes are pushed: which
/// applies is Slint's to decide.
fn dress(window: &MainWindow) {
    let theme = window.global::<Theme>();
    theme.set_dark_scheme(scheme(&scour_ui::DARK));
    theme.set_light_scheme(scheme(&scour_ui::LIGHT));
    theme.set_unit(scour_ui::METRICS.unit);
    theme.set_row_height(scour_ui::METRICS.row);
    theme.set_radius(scour_ui::METRICS.radius);

    let fonts = window.global::<Fonts>();
    fonts.set_size(scour_ui::METRICS.size);
    fonts.set_mono(mono_family().as_str().into());
}

/// A monospace face this machine actually has. **"monospace" is not a family,
/// and Slint does not treat it as one**: parley falls through to a sans, where
/// `M` is 18.86 px wide and `i` 5.98 under a layer drawn on top of it.
fn mono_family() -> String {
    if let Ok(named) = std::env::var("SCOUR_GUI_MONO") {
        return named;
    }
    #[cfg(target_os = "macos")]
    return "Menlo".to_owned();
    #[cfg(windows)]
    return "Consolas".to_owned();
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let asked = std::process::Command::new("fc-match")
            .args(["-f", "%{family}", "monospace"])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            // fontconfig answers with every name the family goes by.
            .and_then(|names| names.split(',').next().map(str::trim).map(str::to_owned))
            .filter(|name| !name.is_empty());
        trace(&format!("mono family: {asked:?}"));
        // No fontconfig: the old string, which draws in the sans.
        asked.unwrap_or_else(|| "monospace".to_owned())
    }
}

/// The column headings and their widths, from `scour-ui`; painting is not shared.
fn columns(window: &MainWindow, cat: &Catalogue) {
    // The report has a size heading of its own, which is not the table's.
    let head = |id: &str| {
        scour_ui::column(id)
            .map(|c| t(cat, c.msgid))
            .unwrap_or_default()
    };
    window.set_head_size(head("size"));
}

/// The columns this window shows when nobody has said otherwise: `scour-ui`'s.
const COLUMN_DEFAULT: &[&str] = scour_ui::DEFAULT_COLUMNS;

/// Which columns are shown, how wide, and what a person dragged them to: one
/// place, because widths are shared out among the columns showing and a drag has
/// to survive a column being hidden and shown again.
struct Table {
    shown: Vec<&'static scour_ui::Column>,
    /// What each column was dragged to, for **every** column and not only the
    /// showing ones: `Change::widths` replaces the map rather than merging.
    widths: std::collections::BTreeMap<String, u32>,
}

impl Table {
    /// What was saved, or the default: an unknown id is dropped, and an empty list
    /// is the one answer that cannot be right.
    fn read(kept: &scour_settings::Settings) -> Table {
        let mut shown: Vec<&'static scour_ui::Column> = kept
            .columns
            .iter()
            .filter_map(|id| scour_ui::column(id))
            .collect();
        if shown.is_empty() {
            shown = COLUMN_DEFAULT
                .iter()
                .filter_map(|id| scour_ui::column(id))
                .collect();
        }
        Table {
            shown,
            widths: kept.widths.clone(),
        }
    }

    fn ids(&self) -> Vec<String> {
        self.shown.iter().map(|c| c.id.to_owned()).collect()
    }

    fn showing(&self, id: &str) -> bool {
        self.shown.iter().any(|c| c.id == id)
    }

    /// Switch one column on or off, put back in front of the first shown column
    /// that outranks it in `scour_ui::COLUMNS`.
    fn toggle(&mut self, id: &str) {
        if self.shown.len() == 1 && self.showing(id) {
            return;
        }
        if let Some(at) = self.shown.iter().position(|c| c.id == id) {
            self.shown.remove(at);
            return;
        }
        let Some(col) = scour_ui::column(id) else {
            return;
        };
        let rank = |x: &str| scour_ui::COLUMNS.iter().position(|c| c.id == x);
        let at = self
            .shown
            .iter()
            .position(|c| rank(c.id) > rank(col.id))
            .unwrap_or(self.shown.len());
        self.shown.insert(at, col);
    }
}

/// Divide the row up among the columns again: a resize, the preview panel, a
/// dragged edge. `scour_ui::lay_out` promises the widths add up to the room.
fn relayout(window: &MainWindow, table: &Table) {
    let ids = table.ids();
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    let widths = scour_ui::lay_out(
        &ids,
        // Zero is "nobody has touched it", the same as in the settings file.
        |id| table.widths.get(id).copied().filter(|v| *v > 0),
        window.get_lane().max(0.0) as u32,
    );
    if widths.len() != ids.len() {
        return;
    }
    let px: Vec<f32> = widths.iter().map(|w| *w as f32).collect();
    // Where each column starts inside the row: the row's 12px padding, then
    // every earlier column and the 8px gap after it. Handed over so a cell can
    // tell whether the row's pointer is on it without reading its own
    // geometry — which, in a repeater's `changed` binding, is a cycle.
    let mut at = 12.0;
    let starts: Vec<f32> = px
        .iter()
        .map(|w| {
            let here = at;
            at += w + 8.0;
            here
        })
        .collect();
    window.set_cx(ModelRc::new(VecModel::from(starts)));
    window.set_cw(ModelRc::new(VecModel::from(px)));
}

/// The headings, in the order they are shown; redone when either changes.
fn set_heads(window: &MainWindow, table: &Table, cat: &Catalogue) {
    window.set_heads(ModelRc::new(VecModel::from(heads_of(table, cat))));
}

/// The same, without a window — which is what makes the wiring testable.
fn heads_of(table: &Table, cat: &Catalogue) -> Vec<HeadInfo> {
    table
        .shown
        .iter()
        .map(|c| HeadInfo {
            id: c.id.into(),
            label: t(cat, c.msgid),
            sort: c.sort.into(),
            right: c.align == scour_ui::Align::End,
        })
        .collect()
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
        danger: c(&p.danger),
        t0: c(&p.t[0]),
        t1: c(&p.t[1]),
        t2: c(&p.t[2]),
        t3: c(&p.t[3]),
        t4: c(&p.t[4]),
        t5: c(&p.t[5]),
        q_term: c(&p.q_term),
        q_key: c(&p.q_key),
        q_val: c(&p.q_val),
        q_glob: c(&p.q_glob),
        q_not: c(&p.q_not),
        q_bad: c(&p.q_bad),
    }
}

/// The first `name` beside this program, else on the `PATH`: a build being tried
/// out must not quietly start the installed copy.
fn which(name: &str) -> Option<std::path::PathBuf> {
    if let Ok(here) = std::env::current_exe()
        && let Some(dir) = here.parent()
    {
        let beside = dir.join(name);
        if beside.is_file() {
            return Some(beside);
        }
        // A build run out of `target/release` has the scripts two levels up.
        let script = dir.join("../../scripts").join(name);
        if script.is_file() {
            return Some(script);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Start another way of running Scour, detached and never waited for.
fn open_face(window: &MainWindow, which_one: &str, cat: &Catalogue, link: &Link) {
    // Through the launcher, not directly: a terminal interface without a tty exits
    // at once, and which terminal to start is nine programs in `scour-open`.
    let (program, args): (&str, Vec<&str>) = match which_one {
        "web" => ("scour-web", Vec::new()),
        "tui" => ("scour-open", vec!["tui"]),
        _ => return,
    };
    let Some(binary) = which(program) else {
        said(
            window,
            format!("{} — {program}", t(cat, "not found")).into(),
        );
        return;
    };
    let mut command = std::process::Command::new(&binary);
    command.args(&args);
    // In a process group of its own, or closing this window closes what it opened.
    scour_ui::faces::detach(&mut command);
    match command.spawn() {
        Ok(_) => {
            // Switching is also choosing: the desktop entry names no face.
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    face: Some(which_one.to_string()),
                    ..Default::default()
                },
            });
            said(window, t(cat, "starting…"));
            // Switching is moving, not opening a second window onto the same
            // index. After a beat: the preference above is a message on a socket.
            slint::Timer::single_shot(std::time::Duration::from_millis(500), || {
                let _ = slint::quit_event_loop();
            });
        }
        Err(e) => said(window, format!("{program}: {e}").into()),
    }
    window.set_panel("".into());
    window.set_armed_face("".into());
}

/// Write the whole matching set to a file, without stopping the window. **Its own
/// connection and its own thread**: the export is a stream — 2.25 M rows and 3.6 s
/// here — and each piece goes straight to the file rather than into memory.
fn export(window: &MainWindow, addr: &str, cat: &Catalogue) {
    let query = window.get_query().to_string();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Where a desktop puts downloads, offered rather than decided.
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let into = scour_places::downloads().unwrap_or(home);
    let name = format!("scour-{stamp}.csv");
    let weak = window.as_weak();
    let addr = addr.to_string();
    let waiting = t(cat, "writing…");
    let wrote = t(cat, "written to");
    let failed = t(cat, "could not be written");

    std::thread::spawn(move || {
        // Not on the drawing thread: a window that stops repainting looks crashed.
        let Some(path) = rfd::FileDialog::new()
            .set_directory(&into)
            .set_file_name(&name)
            .add_filter("CSV", &["csv"])
            .save_file()
        else {
            // Cancelled: the person who closed the dialog knows what they did.
            return;
        };
        let told_path = path.display().to_string();
        let _ = slint::invoke_from_event_loop({
            let weak = weak.clone();
            let waiting = waiting.clone();
            move || {
                if let Some(w) = weak.upgrade() {
                    w.set_note(waiting);
                }
            }
        });
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

        let told = match outcome {
            Ok(bytes) => format!("{wrote} {told_path}  ·  {}", compact(bytes)),
            Err(e) => format!("{failed}: {e}"),
        };
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                // It has to stay put: the meter is rewritten several times a second.
                w.set_note(told.into());
            }
        });
    });
}

/// Save what the window actually looks like, then leave: the compositor will not
/// hand a screenshot to a terminal. `SCOUR_GUI_SNAP=/path/to.ppm`, plain PPM.
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

/// Bytes, the way the meter says them: `636,4 MB`.
fn compact_bytes(n: u64) -> String {
    scour_ui::format::compact_bytes(n, marks().1)
}

/// How this window is punctuating numbers, set when the language is.
fn marks() -> (char, char) {
    MARKS.with(std::cell::Cell::get)
}

thread_local! {
    static MARKS: std::cell::Cell<(char, char)> = const { std::cell::Cell::new(('.', ',')) };
}

/// A number a person can read: `5356281` becomes `5.356.281`.
fn grouped(n: u64) -> String {
    scour_ui::format::grouped(n, marks().0)
}

/// The language the window should speak: what a face's menu chose
/// (`scour_settings::Settings::language`), then the config, then the desktop.
fn language(kept: &scour_settings::Settings, cfg: &scour_config::Config) -> String {
    scour_i18n::choose(&kept.language, &cfg.ui.language)
}

/// As much of a file as a preview should show — and as much as can be drawn: the
/// software renderer casts every glyph position to `i16`, and a quarter megabyte
/// in a 300 px column is ninety thousand pixels tall. Divided by the scale.
fn peek_head(text: &str, scale: f32) -> String {
    let scale = (scale.max(1.0) as usize).max(1);
    let chars = 20_000 / scale;
    let lines = 400 / scale;
    let mut end = text.len();
    let mut seen = 0;
    for (taken, (at, c)) in text.char_indices().enumerate() {
        if taken >= chars || seen >= lines {
            end = at;
            break;
        }
        if c == '\n' {
            seen += 1;
        }
    }
    if end == text.len() {
        return text.to_owned();
    }
    let mut cut = text[..end].to_owned();
    cut.push_str("\n…");
    cut
}

/// Draw what the service says can be shown of a file — the desktop's thumbnail
/// rather than the file, because a photograph is a decode on the drawing thread.
fn show_peek(
    w: &MainWindow,
    cat: &Catalogue,
    link: &Rc<Link>,
    path: &str,
    look: &scour_preview::Look,
) -> bool {
    w.set_peek_text(peek_head(&look.head, w.window().scale_factor()).into());
    /// The largest picture worth decoding on the drawing thread.
    const SMALL: u64 = 512 * 1024;
    // Whatever the desktop can draw, not only what a browser can: `shape` is the
    // browser's question, and there are thumbnailers for PDFs too.
    let made = scour_thumbs::cache::existing(path).or_else(|| {
        // Only a picture is opened directly: a small PDF is not a small picture.
        (look.shape == "image" && look.len <= SMALL).then(|| std::path::PathBuf::from(path))
    });
    match made.and_then(|p| slint::Image::load_from_path(&p).ok()) {
        Some(image) => {
            w.set_peek_shot(image);
            w.set_peek_has_shot(true);
        }
        None => {
            w.set_peek_shot(rows::blank());
            w.set_peek_has_shot(false);
        }
    }
    // A picture nobody has made is not one that cannot be made. `can_make` is the
    // machine's own table: one extension and one MIME lookup, no syscall.
    let coming = !w.get_peek_has_shot() && scour_thumbs::can_make(path);
    if coming {
        link.send(Ask::Thumbnails {
            files: vec![path.to_owned()],
        });
    }
    // Said only when there is nothing else in the box and nothing on its way.
    w.set_peek_note(if w.get_peek_has_shot() || !look.head.is_empty() {
        slint::SharedString::new()
    } else if coming {
        t(cat, "reading…")
    } else {
        t(cat, "It could not be previewed.")
    });
    coming
}

/// The fact list, in the order [`scour_ui::preview::FACTS`] gives.
fn peek_facts(cat: &Catalogue, entry: &scour_core::Entry) -> ModelRc<Fact> {
    let m = &entry.meta;
    let items = if entry.is_dir && m.items >= 0 {
        t(cat, "{n} items").replace("{n}", &grouped(m.items as u64))
    } else {
        String::new()
    };
    let facts = scour_ui::preview::Facts {
        folder: scour_ui::path::folder(&entry.path),
        kind: &t(cat, entry.kind().msgid()),
        is_dir: entry.is_dir,
        size: m.size.max(0) as u64,
        items: &items,
        mtime: m.mtime,
        ctime: m.ctime,
        atime: m.atime,
        mode: &scour_core::mode_string(m.mode),
        owner: &[
            scour_core::owner_name(scour_core::Owner::User, m.uid),
            scour_core::owner_name(scour_core::Owner::Group, m.gid),
        ]
        .join(" · "),
    };
    let rows: Vec<Fact> = facts
        .lines(marks().1)
        .into_iter()
        .map(|(msgid, value)| Fact {
            label: t(cat, msgid),
            value: value.into(),
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

/// `taranıyor 1.240.000` while the index is walked, nothing otherwise. The
/// wording is `scour_ui::SCANNING`, so every face says the same.
fn scanning_note(cat: &Catalogue, st: &scour_core::Status) -> slint::SharedString {
    if !st.scanning {
        return slint::SharedString::new();
    }
    t(cat, scour_ui::SCANNING)
        .replace("{n}", &grouped(st.scanned))
        .into()
}

#[cfg(test)]
mod tests {
    /// The preview is bounded whatever the service sends, and the cut is checked
    /// for being a real string.
    #[test]
    fn a_preview_is_bounded_however_large_the_file_is() {
        let short = "fn main() {}\n";
        assert_eq!(
            super::peek_head(short, 1.0),
            short,
            "a small file is untouched"
        );

        // A quarter megabyte, which is what the service actually sends.
        let big: String = "lorem ipsum dolor sit amet\n".repeat(10_000);
        let cut = super::peek_head(&big, 1.0);
        assert!(cut.len() < big.len() / 4, "{} of {}", cut.len(), big.len());
        assert!(cut.ends_with('…'), "the reader is told it was cut");
        assert!(cut.lines().count() <= 402);

        // One enormous line: the line cap cannot help and the character cap must.
        let one: String = "x".repeat(300_000);
        assert!(super::peek_head(&one, 1.0).chars().count() <= 20_002);
        // Three times the scale, a third of the text.
        assert!(super::peek_head(&one, 3.0).chars().count() <= 6_669);
    }

    /// A cut that lands inside a multi-byte character must not panic.
    #[test]
    fn the_cut_falls_on_a_character_boundary() {
        let turkish: String = "çğıöşü ".repeat(9_000);
        let cut = super::peek_head(&turkish, 1.0);
        assert!(cut.chars().count() <= 20_002);
        assert!(!cut.is_empty());
    }

    use super::*;

    /// The window's palette is the browser page's. What this checks is the
    /// conversion: Slint takes alpha first and the browser takes it last.
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
        // `ext:pdf` matched a column, not a run of the name.
        assert_eq!(terms_of("rapor ext:pdf"), vec!["rapor".to_string()]);
        assert_eq!(terms_of("!eski rapor"), vec!["rapor".to_string()]);
        // Quotes hold a phrase together: one mark, not two.
        assert_eq!(terms_of("\"iki kelime\""), vec!["iki kelime".to_string()]);
        assert_eq!(
            terms_of("\"a:b\""),
            vec!["a:b".to_string()],
            "quoted, so not a field"
        );
        assert!(terms_of("kind:image").is_empty());
    }

    /// The coloured layer spells what is in the box, never what was.
    #[test]
    fn a_deleted_character_is_not_drawn_by_the_spans_that_still_hold_it() {
        use scour_core::{Role, Span as Run};
        // What `rapor pdf` came back as, now laid over `rapor p`.
        let spans = [
            Run::new(0, 5, Role::Text),
            Run::new(5, 1, Role::Space),
            Run::new(6, 3, Role::Text),
        ];
        let runs = painted("rapor p", &spans);
        let whole: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(whole, "rapor p", "the layer spelled something else");
        assert_eq!(runs.last().unwrap().at, 6, "the last run is misplaced");
    }

    /// And what was typed since is drawn rather than left out.
    #[test]
    fn a_character_typed_since_the_spans_were_asked_for_is_still_drawn() {
        use scour_core::{Role, Span as Run};
        let spans = [Run::new(0, 5, Role::Text)];
        let runs = painted("rapors", &spans);
        let whole: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(whole, "rapors");
    }

    /// Offsets are bytes on the wire and characters on the screen.
    #[test]
    fn a_run_after_a_turkish_word_is_placed_by_characters() {
        use scour_core::{Role, Span as Run};
        // `değiştirme` is ten characters and twelve bytes.
        let q = "değiştirme rapor";
        let spans = [
            Run::new(0, 12, Role::Text),
            Run::new(12, 1, Role::Space),
            Run::new(13, 5, Role::Text),
        ];
        let runs = painted(q, &spans);
        assert_eq!(runs[2].at, 11, "the run after it is placed by bytes");
        let whole: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(whole, q);
    }

    /// A span the query no longer reaches at all.
    #[test]
    fn spans_past_the_end_are_dropped_rather_than_panicking() {
        use scour_core::{Role, Span as Run};
        let spans = [Run::new(0, 5, Role::Text), Run::new(40, 9, Role::Value)];
        let runs = painted("rap", &spans);
        let whole: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(whole, "rap");
    }

    #[test]
    fn expensive_refresh_waits_but_a_new_scroll_page_does_not() {
        use std::time::Duration;
        let rows = rows::Rows::default();
        rows.put(0, vec![Row::default(); rows::SPAN], Vec::new(), 10_000);
        rows.mark(1);
        let early = refresh_ready(Some(Duration::from_secs(5)), 1_000_000);
        assert!(!early);
        assert_eq!(rows.next_page(0, 40, false, early), None);
        assert_eq!(rows.next_page(400, 440, false, early), Some(2));
        let rested = refresh_ready(Some(Duration::from_secs(10)), 1_000_000);
        assert_eq!(rows.next_page(0, 40, false, rested), Some(0));
        assert!(!refresh_ready(Some(Duration::from_millis(499)), 1_000));
        assert!(refresh_ready(Some(Duration::from_millis(500)), 1_000));
    }

    #[test]
    fn a_layout_miss_from_the_previous_viewport_cannot_redirect_a_scroll() {
        assert_eq!(fetch_from(10_000, 40, Some(400)), 10_000);
        assert_eq!(fetch_from(10_000, 40, Some(10_005)), 10_005);
        assert_eq!(fetch_from(0, 40, Some(10_005)), 0);
        assert_eq!(fetch_from(10_000, 40, None), 10_000);
    }

    #[test]
    fn the_rail_composes_with_the_text_rather_than_replacing_it() {
        let mut s = State {
            mounts: Vec::new(),
            indexed: 0,
            stirred: std::time::Instant::now(),
            dozing: false,
            generation: 0,
            query_revision: 0,
            past: Vec::new(),
            spans: Vec::new(),
            background_query: None,
            count_query: None,
            exact_count: None,
            row_limit: 20,
            page_offset: 0,
            page_sent: None,
            page_landed_at: None,
            page_cost_us: 0,
            asking_pictures: false,
            peek_path: String::new(),
            asked_pictures: std::collections::VecDeque::new(),
            rewind: false,
            typed_at: None,
            shown: 0,
            query: "rapor".into(),
            sort: "relevance".into(),
            descending: true,
            facet: None,
            added_paths: Vec::new(),
            added_dirs: Vec::new(),
            added_files: Vec::new(),
            sent_off: None,
            rules_shown: Default::default(),
            exclude_off: Vec::new(),
            scope: String::new(),
            hits: Vec::new(),
            revision: 0,
            down: false,
        };
        assert_eq!(full_query(&s), "rapor");

        // The slot holds a whole term: the ribbon and the scopes share it, so
        // gluing `kind:` on in front produced `kind:dm:38d`.
        s.facet = Some("kind:image".into());
        assert_eq!(full_query(&s), "rapor kind:image");
        s.facet = Some("under:/home/u/Belgeler".into());
        assert_eq!(full_query(&s), "rapor under:/home/u/Belgeler");
        s.facet = Some("dm:38d".into());
        assert_eq!(full_query(&s), "rapor dm:38d");

        s.query = "  ".into();
        assert_eq!(full_query(&s), "dm:38d");

        // The rail is counted over what was typed, never over the term it offered.
        s.query = "rapor".into();
        assert_eq!(facet_query(&s), "rapor");
    }

    /// **The doze is a clock, and the clock is the whole feature.** Both edges.
    #[test]
    fn a_window_dozes_only_after_a_minute_untouched_and_wakes_on_a_touch() {
        let long_ago = std::time::Instant::now() - AWAKE_FOR - std::time::Duration::from_secs(1);
        let just_now = std::time::Instant::now();

        // Untouched for longer than the window stays awake: it dozes.
        assert!(long_ago.elapsed() >= AWAKE_FOR, "the test's own premise");
        // Touched within it: it does not.
        assert!(just_now.elapsed() < AWAKE_FOR);
        // A doze asks far less often than an awake window, and not never.
        assert!(
            DOZE_AGAIN > AWAIT_AGAIN * 8,
            "a doze that asks as often is not a doze"
        );
        assert!(
            DOZE_AGAIN < AWAKE_FOR,
            "a doze must refresh before it could wake"
        );
    }

    #[test]
    fn sorting_reuses_query_scoped_sidebar_and_count_work() {
        let mut s = State {
            mounts: Vec::new(),
            stirred: std::time::Instant::now(),
            dozing: false,
            sent_off: None,
            rules_shown: Default::default(),
            indexed: 0,
            generation: 4,
            query_revision: 2,
            past: Vec::new(),
            spans: Vec::new(),
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
            page_landed_at: None,
            page_cost_us: 0,
            asking_pictures: false,
            peek_path: String::new(),
            asked_pictures: std::collections::VecDeque::new(),
            rewind: false,
            typed_at: None,
            shown: 4,
            query: "rapor".into(),
            sort: "modified".into(),
            descending: true,
            facet: None,
            added_paths: Vec::new(),
            added_dirs: Vec::new(),
            added_files: Vec::new(),
            exclude_off: Vec::new(),
            scope: String::new(),
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
    fn every_heading_carries_the_sort_key_the_shared_crate_names() {
        let cat = Catalogue::for_language("en");
        // Every column the table can offer, not only the ones showing.
        let table = Table {
            shown: scour_ui::COLUMNS.iter().collect(),
            widths: Default::default(),
        };
        let heads = heads_of(&table, &cat);
        assert_eq!(heads.len(), scour_ui::COLUMNS.len());
        for (h, c) in heads.iter().zip(scour_ui::COLUMNS) {
            assert_eq!(h.id, c.id);
            assert_eq!(h.sort, c.sort, "`{}` asks the wrong sort key", c.id);
            // The word comes from the crate rather than from this file.
            assert!(!h.label.is_empty(), "`{}` has no heading word", c.id);
        }
    }

    #[test]
    fn a_column_switched_on_lands_where_the_table_says_and_moves_nothing() {
        let col = |id: &str| scour_ui::column(id).unwrap();
        let ids = |t: &Table| t.ids().join(",");

        let mut t = Table {
            // Deliberately not the table's own order.
            shown: vec![col("size"), col("name")],
            widths: Default::default(),
        };
        // `path` outranks `size` in `COLUMNS`, so it lands in front of it.
        t.toggle("path");
        assert_eq!(ids(&t), "path,size,name");

        // Off is off, and the width it was dragged to stays behind.
        t.toggle("size");
        assert_eq!(ids(&t), "path,name");

        // **The last one standing cannot be turned off**: a table of no columns is
        // broken, not smaller.
        t.toggle("path");
        assert_eq!(ids(&t), "name");
        t.toggle("name");
        assert_eq!(ids(&t), "name");

        // An id from a newer version, or a typo, changes nothing at all.
        t.toggle("zurna");
        assert_eq!(ids(&t), "name");
    }

    #[test]
    fn a_saved_column_list_is_read_back_and_a_broken_one_is_not_obeyed() {
        let read = |cols: &[&str]| {
            Table::read(&scour_settings::Settings {
                columns: cols.iter().map(|s| (*s).to_string()).collect(),
                ..Default::default()
            })
            .ids()
            .join(",")
        };
        assert_eq!(read(&["kind", "name"]), "kind,name");
        // A column this build lacks is dropped rather than the whole list refused.
        assert_eq!(read(&["kind", "zurna", "name"]), "kind,name");
        // A list with nothing left cannot be right, so the default stands.
        assert_eq!(read(&["zurna"]), COLUMN_DEFAULT.join(","));
        assert_eq!(read(&[]), COLUMN_DEFAULT.join(","));
    }

    #[test]
    fn a_page_that_comes_back_short_is_the_end_of_the_result() {
        // A full page in the middle of a long result: the count says how long.
        assert_eq!(list_length(2_500_000, 400, 200, 200, 200), 2_500_000);
        // A result that shrank while somebody scrolled to it: believing the count
        // leaves six rows asked for for ever.
        assert_eq!(list_length(979, 779, 194, 200, 200), 973);
        // The first page of a new query is short, and is not an ending.
        assert_eq!(list_length(2_500_000, 0, 24, 24, 200), 2_500_000);
        // Nor is a service whose own page ceiling is below the request.
        assert_eq!(list_length(5_000, 0, 200, 256, 200), 5_000);
        // A count shorter than what is in hand cannot hide the loaded rows.
        assert_eq!(list_length(10, 400, 200, 200, 200), 600);
    }

    #[test]
    fn a_term_too_short_to_narrow_is_not_ranked() {
        // At `t` relevance walks a million rows for an ordering nobody can read.
        assert_eq!(order_for("t", "relevance"), "modified");
        assert_eq!(order_for("to", "relevance"), "modified");
        assert_eq!(order_for("tok", "relevance"), "relevance");
        // The shortest term decides.
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
        // **`!c.get(k.msgid()).is_empty()` could not fail**: `get` falls back to
        // the msgid, so `has` asks the catalogue. Only translated languages.
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
