// A window, not a console program: without this Windows opens a console
// window behind it for its standard output.
#![cfg_attr(windows, windows_subsystem = "windows")]
//! The Scour window.
//!
//! A frontend only: no index, no walk, everything over a socket. Service
//! calls run on worker threads and return as events ([`link`]); query
//! meaning is the engine's; a reply for a superseded keystroke is dropped.

mod actions;
mod peeking;
mod search;
mod selection;
mod size;
mod start;
mod status;
mod tab_strip;
mod table;
mod wording;
use actions::*;
use peeking::*;
use search::*;
use selection::*;
pub(crate) use size::*;
use start::*;
use status::*;
use tab_strip::*;
use table::*;
pub(crate) use wording::*;

mod autostart;
mod disks;
mod drag;
mod frame;
mod grep_ui;
mod hotkey;
mod install;
mod link;
mod paste;
mod peek;
mod pixels;
mod qbar;
mod qlens;
mod qwire;
mod report;
mod rows;
mod service_mode;
mod tabs;
mod tiles;
mod tray;

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
    Bar, Big, CalDay, CalMonth, Cap, Cell, Disk, DragTray, Dupe, Facet, Fact, FolderRow, Fonts,
    Found, HeadInfo, Kid, LensRow, MainWindow, MenuItem, QMeasure, Reach, Row, Rule, Scheme, Seg,
    Sel, Slice, Slot, Span, Theme,
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
        "f2" => Key::F2.into(),
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
    /// The query field: its layout, the caret, the lens. See `qwire`.
    qb: qwire::QueryBar,
    /// On-demand inline Grep capsule state.
    grep: grep_ui::GrepUi,
    /// The query that shows what the search in sight nearly found, when it
    /// found nothing and the service offered. See `SearchResponse::similar`.
    similar: String,
    /// Where the volumes are: `Accessed` on `noatime` is the creation time.
    mounts: Vec<scour_places::Mount>,
    /// When somebody last did something here. See [`AWAKE_FOR`].
    stirred: std::time::Instant,
    /// How long to leave between re-reading the rows in sight, once the index
    /// moved. `Settings::refresh_after`.
    refresh: std::time::Duration,
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
    /// Whether that question is still out: asked, and the service has not gone
    /// away under it. A rewind with nothing out is asked again when it is back.
    search_out: bool,
    typed_at: Option<std::time::Instant>,
    /// The generation whose search reply is currently on screen.
    shown: u64,
    query: String,
    sort: String,
    descending: bool,
    /// The rules this window added, as reported: deleting one sends the list back.
    added_paths: Vec<String>,
    added_dirs: Vec<String>,
    added_files: Vec<String>,
    /// The switched-off list last sent, unconfirmed: `Rules` answers coalesce.
    sent_off: Option<Vec<String>>,
    /// How many disks were waiting for an answer at the last status; the list
    /// is asked for again when that moves. `None` until the first.
    disks_asking: Option<u32>,
    /// The disk the question is about, which an answer names.
    asked_disk: String,
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

/// The floors the duplicate hunt offers. A unique size eliminates only 6.2% of
/// files, so a floor is what makes the question answerable.
const FLOORS: [(u64, &str); 4] = [
    (1 << 20, "1 MB+"),
    (10 << 20, "10 MB+"),
    (100 << 20, "100 MB+"),
    (1 << 30, "1 GB+"),
];

// What the window takes on the command line. **Ordinary comments, not doc
// comments**: clap prints a doc comment as the text `--help` shows.
#[derive(clap::Parser)]
#[command(name = "scour-gui", about = "Scour in a window", version)]
struct Args {
    /// Talk to a service listening here
    #[arg(long)]
    socket: Option<String>,
    // Start the service if none is running, and leave without a window: what
    // the Run key says at login on Windows. See `autostart`.
    #[arg(long, hide = true)]
    start_service: bool,
    // Install for this account, or take that away again: Windows' `install.cmd`
    // and `uninstall.cmd`. See `install`.
    #[arg(long, hide = true)]
    install: bool,
    #[arg(long, hide = true)]
    uninstall: bool,
    // The two halves of a move between the session and the Windows service that
    // need an administrator: this window started again with `runas`. See
    // `service_mode`.
    #[arg(long, hide = true, num_args = 6, allow_hyphen_values = true)]
    service_on: Option<Vec<String>>,
    #[arg(long, hide = true, num_args = 1, allow_hyphen_values = true)]
    service_off: Option<Vec<String>>,
    /// Start with this query
    #[arg(short, long, default_value = "")]
    query: String,
    /// Open a small window to drag these files from, into another program
    #[arg(long, num_args = 1.., value_name = "PATH")]
    drag: Vec<String>,
}

fn main() -> Result<()> {
    // `--version` and `--help` answer and leave, before any socket or window.
    let args = <Args as clap::Parser>::parse();
    if args.install || args.uninstall {
        install::run(args.install);
        return Ok(());
    }
    #[cfg(windows)]
    if let Some(v) = args.service_on.as_ref().or(args.service_off.as_ref()) {
        let done = if args.service_on.is_some() {
            service_mode::elevated_on(v)
        } else {
            service_mode::elevated_off(v)
        };
        if let Err(e) = done {
            let _ = rfd::MessageDialog::new()
                .set_title("Scour")
                .set_description(e)
                .set_level(rfd::MessageLevel::Error)
                .show();
            std::process::exit(1);
        }
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    wayland_found_again();
    let launched = std::time::Instant::now();
    let config = scour_config::Config::load_or_default().0;
    // A window from a desktop entry has nowhere for standard error to go.
    crash_log(&config.state_dir());
    // Not the one window: the page and the terminal open one per drag, and it
    // needs no service.
    if !args.drag.is_empty() {
        return tray::run(&args.drag, &config);
    }
    if args.start_service {
        let addr = args.socket.clone().unwrap_or_else(|| config.socket());
        let _ = link::boot(&addr, &config.state_dir().join("scourd.log"));
        return Ok(());
    }

    // One window. A second start hands over to the first and leaves.
    #[cfg(unix)]
    let claim = match only_one(&args.socket.clone().unwrap_or_else(|| config.socket())) {
        Some(claim) => claim,
        None => return Ok(()),
    };

    let kept = scour_settings::Settings::load(&config.state_dir()).opened();
    // Behind a cell: picking a language rebuilds it and rewrites every string.
    let cat: Rc<RefCell<Rc<Catalogue>>> = Rc::new(RefCell::new(Rc::new(Catalogue::for_language(
        &language(&kept, &config),
    ))));
    let window = MainWindow::new().context("the window could not be created")?;
    // `scour`, the desktop entry's own name: that is how a Wayland desktop
    // finds the icon and the menu entry for a window. **After `new`, before
    // the window is shown**: before any component exists Slint has no
    // platform and refuses with `NoPlatform`, and every desktop then drew a
    // generic icon; the winit window that carries the id is made on showing.
    if let Err(e) = slint::set_xdg_app_id("scour") {
        eprintln!("scour-gui: the window's app id could not be set: {e}");
    }
    #[cfg(unix)]
    answer_the_next_start(claim.listener.try_clone()?, window.as_weak());
    dress(&window);
    words(&window, &cat.borrow());
    window
        .global::<ui::Clip>()
        .on_paste_into(|text, anchor, cursor, one_line| {
            let spliced = paste::text().and_then(|pasted| {
                paste::splice(
                    &text,
                    usize::try_from(anchor).ok()?,
                    usize::try_from(cursor).ok()?,
                    &pasted,
                    one_line,
                )
            });
            trace(&format!(
                "paste: {}",
                if spliced.is_some() {
                    "X11"
                } else {
                    "Slint's own"
                }
            ));
            match spliced {
                Some((text, caret)) => ui::Spliced {
                    ok: true,
                    text: text.into(),
                    caret: i32::try_from(caret).unwrap_or(i32::MAX),
                },
                None => ui::Spliced::default(),
            }
        });
    // The order chosen last, in whichever face: a word no key has is a file
    // from somewhere else, and the newest-first default stands.
    // The search tabs as they were left; one tab of the kept order when there
    // were none. The tab in front decides the order the window opens in.
    let tabs = Rc::new(RefCell::new(tabs::Tabs::kept(
        &kept.tabs,
        kept.tab,
        scour_settings::SearchTab {
            query: args.query.clone(),
            sort: kept.sort.clone(),
            descending: kept.descending,
        },
    )));
    let front = tabs.borrow().front().clone();
    let kept_sort = scour_core::SortKey::named(&front.sort)
        .unwrap_or_default()
        .name();
    let kept_descending = front.descending.unwrap_or(true);
    window.set_sorted_by(kept_sort.into());
    window.set_sorted_down(kept_descending);
    draw_tabs(&window, &tabs.borrow(), &cat.borrow());
    window.set_has_past(!kept.history.is_empty());
    window.set_grid_lines(kept.grid_lines);
    window.set_grid_columns(kept.grid_columns);
    window.set_system_icons(kept.system_icons);
    window.set_tabs_on(!kept.tabs_off);
    window.set_accents_ignored(!kept.accents_exact);
    window.set_typos_choice(typos_choice(&kept.typos).into());
    window.set_typos_strict(kept.typos_strict);
    window.set_field_words(kept.field_words);
    // Said again: the words were put in above, before the choice was known,
    // and said "offer it" whatever had been chosen.
    window.set_typos_said(typos_said(&cat.borrow(), &window.get_typos_choice()).into());
    window.set_rail_shown(kept.rail_shown);
    window.set_ribbon_shown(kept.ribbon_shown);
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
        qb: qwire::QueryBar::off(kept.narrow_off),
        grep: grep_ui::GrepUi::default(),
        similar: String::new(),
        mounts: Vec::new(),
        // Awake at birth: opening the window is somebody doing something in it.
        stirred: std::time::Instant::now(),
        refresh: kept.refresh_after(),
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
        search_out: false,
        typed_at: None,
        shown: 0,
        query: String::new(),
        sort: kept_sort.into(),
        descending: kept_descending,
        added_paths: Vec::new(),
        added_dirs: Vec::new(),
        added_files: Vec::new(),
        sent_off: None,
        disks_asking: None,
        asked_disk: String::new(),
        rules_shown: Default::default(),
        exclude_off: Vec::new(),
        scope: String::new(),
        hits: Vec::new(),
        revision: 0,
        down: false,
    }));

    // The report's last answer, held so a resize can relabel its strips without
    // weighing the folder again.
    let weighed: Rc<RefCell<report::Kept>> = Rc::new(RefCell::new(report::Kept::default()));
    // The path the right-click menu is about, when it was not a row of the list:
    // the report's heaviest files are not in the table's model.
    let menu_path: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    let rows: Rc<rows::Rows> = Rc::new(rows::Rows::default());
    // **Selected by path**: the index and the sort both move under a row number.
    let picks: Rc<RefCell<Selection>> = Rc::new(RefCell::new(Selection::default()));
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
    // What the detail list draws: slots moved from row to row, never rebuilt.
    let slots = rows::Slots::new(&rows);
    window.set_slots(ModelRc::from(slots.clone()));
    place(&window, &slots);
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
    // The panel's width lives beside the columns', under `peek`, for every face.
    window.set_peek_min(scour_ui::preview::PANEL_MIN as f32);
    window.set_peek_max(scour_ui::preview::PANEL_MAX as f32);
    window.set_peek_width(
        kept.widths
            .get("peek")
            .copied()
            .unwrap_or(scour_ui::preview::PANEL_WIDE)
            .clamp(scour_ui::preview::PANEL_MIN, scour_ui::preview::PANEL_MAX) as f32,
    );
    // The settings page's choices, as they were left.
    window
        .global::<Theme>()
        .set_chosen(kept.theme.as_str().into());
    window.set_language_choice(kept.language.as_str().into());
    window.set_face_default(if kept.face.is_empty() {
        "window".into()
    } else {
        kept.face.as_str().into()
    });
    window.set_removable_choice(kept.removable.as_str().into());
    window.set_refresh_choice(kept.refresh_after().as_secs().to_string().into());
    window.set_memory_choice(kept.memory.as_str().into());
    window.set_narrow_on(!kept.narrow_off);
    window.set_threads_choice(kept.search_threads.to_string().into());
    window.set_fresh_choice(kept.fresh_within.to_string().into());
    window.set_every_choice(
        match kept.folders_every {
            0 => 10,
            n => n,
        }
        .to_string()
        .into(),
    );
    window.set_network_choice(kept.network.as_str().into());
    let folders = Rc::new(RefCell::new(kept.folders.clone()));
    // Which of them are switched off, and which only walked: replaced whole
    // when either changes, as `Change` carries them.
    let folders_off = Rc::new(RefCell::new(kept.folders_off.clone()));
    let folders_unwatched = Rc::new(RefCell::new(kept.folders_unwatched.clone()));
    // Before the service has said how their walks went: the folders, bare.
    disks::draw_folders(
        &window,
        &cat.borrow(),
        &kept
            .folders
            .iter()
            .map(|f| scour_proto::FolderState {
                path: f.clone(),
                on: !kept.folders_off.contains(f),
                watched: !kept.folders_unwatched.contains(f),
                held: false,
                walked: None,
                live: false,
                changed: None,
                walking: false,
            })
            .collect::<Vec<_>>(),
    );
    window.set_folders(strings(&folders.borrow()));
    let kept_layout = kept.layout;
    if matches!(kept_layout.as_str(), "icons" | "large") {
        window.set_view_mode(kept_layout.as_str().into());
    }
    // Where it was left. Set before the window is shown: one that appears and
    // then jumps is worse than one that appears a frame later. The screen is
    // not knowable yet — [`opening_size`] holds it inside a ceiling for now,
    // and the first resize below does it properly.
    let opened_at = opening_size(kept_size(kept.view.get("slint")), None);
    window
        .window()
        .set_size(slint::LogicalSize::new(opened_at.0, opened_at.1));
    ZOOM.with(|z| z.set(kept_zoom(kept.view.get("slint"))));
    window.set_zoom_choice(zoom_choice(ZOOM.with(std::cell::Cell::get)).into());
    trace(&format!("opening at {:.0}x{:.0}", opened_at.0, opened_at.1));
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
    let ui_weighed = Rc::clone(&weighed);

    let sink = move |got: Got| {
        let _ = slint::invoke_from_event_loop(move || deliver(got));
    };

    let link = Rc::new(Link::start(
        addr.clone(),
        config.state_dir().join("scourd.log"),
        sink,
    ));

    // The move between the session and the Windows service. Off this thread:
    // the consent dialog and the copying take seconds. On success the next
    // window is already open, and this one leaves.
    {
        let weak = window.as_weak();
        let config = config.clone();
        window.on_service_switch(move || {
            let Some(w) = weak.upgrade() else { return };
            w.set_service_busy(true);
            let to_service = !service_mode::active();
            let config = config.clone();
            let weak = w.as_weak();
            std::thread::spawn(move || {
                let done = service_mode::switch(to_service, &config);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_service_busy(false);
                    }
                    match done {
                        Ok(()) => std::process::exit(0),
                        Err(e) => {
                            let _ = rfd::MessageDialog::new()
                                .set_title("Scour")
                                .set_description(e)
                                .set_level(rfd::MessageLevel::Error)
                                .show();
                        }
                    }
                });
            });
        });
    }

    // Starting with Windows without the service: the account's Run key.
    {
        let weak = window.as_weak();
        window.on_autostart_picked(move |on| {
            let _ = autostart::set(on);
            if let Some(w) = weak.upgrade() {
                w.set_autostart_on(autostart::is_on());
            }
        });
    }
    // Run as the service, the session's leftovers go: the per-user copy of the
    // programs and the Run value that started it. Off the window's thread.
    #[cfg(windows)]
    if service_mode::active() {
        std::thread::spawn(service_mode::tidy);
    }

    // The tabs switched on or off; the tabs themselves are kept either way.
    {
        let link = Rc::clone(&link);
        let weak = window.as_weak();
        window.on_tabs_picked(move |on| {
            if let Some(w) = weak.upgrade() {
                w.set_tabs_on(on);
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    tabs_off: Some(!on),
                    ..Default::default()
                },
            });
        });
    }

    // Lines between the columns, the same way.
    {
        let link = Rc::clone(&link);
        let weak = window.as_weak();
        window.on_grid_columns_picked(move |on| {
            if let Some(w) = weak.upgrade() {
                w.set_grid_columns(on);
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    grid_columns: Some(on),
                    ..Default::default()
                },
            });
        });
    }

    // Lines in the list, from the settings page; kept for every face.
    {
        let link = Rc::clone(&link);
        let weak = window.as_weak();
        window.on_grid_picked(move |on| {
            if let Some(w) = weak.upgrade() {
                w.set_grid_lines(on);
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    grid_lines: Some(on),
                    ..Default::default()
                },
            });
        });
    }

    {
        let link = Rc::clone(&link);
        let state = Rc::clone(&state);
        let model = Rc::clone(&rows);
        let weak = window.as_weak();
        window.on_system_icons_picked(move |on| {
            if let Some(w) = weak.upgrade() {
                w.set_system_icons(on);
                if on {
                    state.borrow_mut().advance_order();
                    dispatch(&state, &link, &model, w.get_visible_rows().max(0) as u32);
                }
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    system_icons: Some(on),
                    ..Default::default()
                },
            });
        });
    }

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
                    &ui_weighed,
                    got,
                );
            }));
        });
    }

    // The desktop's key: asked for at once, on a thread of its own, because
    // every answer here is a process this window must not wait for.
    hotkey::start(kept.key_hint_seen);
    hotkey::wire(&window, &link, &cat);

    // --- the window's own size --------------------------------------------
    // Kept under `view.slint.size`, as the page keeps its own under
    // `view.web`. Written once the drag stops, never per frame.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        // What the settings already say, so an untouched window writes nothing.
        let saved = Rc::new(std::cell::Cell::new(opened_at));
        // Asked once, as soon as there is a real window behind the Slint one —
        // which is the first moment a monitor can be named at all, and why this
        // is not done at birth.
        let asked = Rc::new(std::cell::Cell::new(false));
        let later = Rc::new(slint::Timer::default());
        window.on_resized(move || {
            use i_slint_backend_winit::WinitWindowAccessor;
            let Some(w) = weak.upgrade() else { return };
            if !asked.get() && w.window().has_winit_window() {
                asked.set(true);
                let screen = screen_size(w.window());
                trace(&format!("screen {screen:?}"));
                let fits = opening_size(Some(window_size(w.window())), screen);
                if fits != window_size(w.window()) {
                    // Lands back here, and the second time it fits. The
                    // settings keep the larger size: this screen is where the
                    // window is now, not what somebody chose.
                    w.window().set_size(slint::LogicalSize::new(fits.0, fits.1));
                    saved.set(fits);
                    return;
                }
            }
            let link = Rc::clone(&link);
            let saved = Rc::clone(&saved);
            let weak = w.as_weak();
            later.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(SIZE_SETTLES_MS),
                move || {
                    let Some(w) = weak.upgrade() else { return };
                    keep_size(w.window(), &link, &saved);
                },
            );
        });
    }

    // --- the search tabs ---------------------------------------------------
    // Kept two seconds after the last keystroke, not on every one.
    let tabs_later = Rc::new(slint::Timer::default());
    {
        let keep = {
            let link = Rc::clone(&link);
            let tabs = Rc::clone(&tabs);
            move || keep_tabs(&link, &tabs.borrow())
        };
        let keep = Rc::new(keep);
        let go = {
            let state = Rc::clone(&state);
            let tabs = Rc::clone(&tabs);
            let cat = Rc::clone(&cat);
            let weak = window.as_weak();
            let keep = Rc::clone(&keep);
            move |moved: Option<scour_settings::SearchTab>| {
                let Some(w) = weak.upgrade() else { return };
                draw_tabs(&w, &tabs.borrow(), &cat.borrow());
                keep();
                if let Some(tab) = moved {
                    show_tab(&w, &state, &tab);
                }
            }
        };
        let go = Rc::new(go);
        {
            let (state, tabs, go) = (Rc::clone(&state), Rc::clone(&tabs), Rc::clone(&go));
            window.on_tab_new(move || {
                let new = tabs.borrow_mut().open(now_tab(&state.borrow()));
                go(Some(new));
            });
        }
        {
            let (state, tabs, go) = (Rc::clone(&state), Rc::clone(&tabs), Rc::clone(&go));
            window.on_tab_picked(move |i| {
                let Ok(i) = usize::try_from(i) else { return };
                let moved = tabs.borrow_mut().switch(i, now_tab(&state.borrow()));
                if moved.is_some() {
                    go(moved);
                }
            });
        }
        {
            let (state, tabs, go) = (Rc::clone(&state), Rc::clone(&tabs), Rc::clone(&go));
            window.on_tab_closed(move |i| {
                let Ok(i) = usize::try_from(i) else { return };
                let before = tabs.borrow().len();
                let moved = tabs.borrow_mut().close(i, now_tab(&state.borrow()));
                if tabs.borrow().len() != before {
                    go(moved);
                }
            });
        }
        {
            let (state, tabs, go) = (Rc::clone(&state), Rc::clone(&tabs), Rc::clone(&go));
            window.on_tab_step(move |by| {
                let moved = tabs
                    .borrow_mut()
                    .step(by as isize, now_tab(&state.borrow()));
                if moved.is_some() {
                    go(moved);
                }
            });
        }
    }

    // --- the query line ---------------------------------------------------
    {
        let state = state.clone();
        let link = link.clone();
        let facets = facets.clone();
        let model = Rc::clone(&rows);
        let weak = window.as_weak();
        let tabs = Rc::clone(&tabs);
        let tabs_later = Rc::clone(&tabs_later);
        let cat = Rc::clone(&cat);
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
            // The tab in front is named by what is typed in it.
            tabs.borrow_mut().hold(now_tab(&state.borrow()));
            if let Some(w) = weak.upgrade() {
                draw_tabs(&w, &tabs.borrow(), &cat.borrow());
            }
            {
                let link = Rc::clone(&link);
                let tabs = Rc::clone(&tabs);
                tabs_later.start(
                    slint::TimerMode::SingleShot,
                    std::time::Duration::from_secs(2),
                    move || keep_tabs(&link, &tabs.borrow()),
                );
            }
            // Laid over the text as it is now, so a deletion shows this frame.
            if let Some(w) = weak.upgrade() {
                qwire::draw(
                    &w,
                    &mut state.borrow_mut(),
                    &link,
                    &cat.borrow(),
                    qwire::Lensing::Follow,
                );
                if state.borrow().grep.open && !state.borrow().grep.pattern.is_empty() {
                    grep_ui::trigger(&w, &mut state.borrow_mut(), &link, &cat.borrow(), false);
                }
                let s = state.borrow();
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

    // --- how words are read ------------------------------------------------
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        window.on_field_words_picked(move |words| {
            if let Some(w) = weak.upgrade() {
                w.set_field_words(words);
                // Drawn again, the headers as now chosen.
                w.invoke_query_changed(w.get_query());
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    field_words: Some(words),
                    ..Default::default()
                },
            });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        window.on_typos_strict_picked(move |strict| {
            if let Some(w) = weak.upgrade() {
                w.set_typos_strict(strict);
                // Asked again once the service has the new rule.
                let weak = w.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
                    if let Some(w) = weak.upgrade() {
                        w.invoke_query_changed(w.get_query());
                    }
                });
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    typos_strict: Some(strict),
                    ..Default::default()
                },
            });
        });
    }
    // Kept by the service, which reads every query: the answer in sight changes
    // when the service says its index would answer differently.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        window.on_accents_picked(move |ignored| {
            if let Some(w) = weak.upgrade() {
                w.set_accents_ignored(ignored);
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    accents_exact: Some(!ignored),
                    ..Default::default()
                },
            });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let cat = Rc::clone(&cat);
        window.on_typos_picked(move |choice| {
            if let Some(w) = weak.upgrade() {
                w.set_typos_choice(choice.clone());
                w.set_typos_said(typos_said(&cat.borrow(), &choice).into());
                // Asked again once the service has the new rule.
                let weak = w.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
                    if let Some(w) = weak.upgrade() {
                        w.invoke_query_changed(w.get_query());
                    }
                });
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    typos: Some(choice.to_string()),
                    ..Default::default()
                },
            });
        });
    }
    {
        let weak = window.as_weak();
        let state = Rc::clone(&state);
        window.on_similar_clicked(move || {
            let Some(w) = weak.upgrade() else { return };
            let text = state.borrow().similar.clone();
            if text.is_empty() {
                return;
            }
            w.set_query(text.as_str().into());
            w.invoke_query_changed(text.into());
            w.invoke_caret_to_end();
        });
    }

    // --- the rail ---------------------------------------------------------
    // A press writes its term into the text, as the page does: a filter kept
    // apart from the field can be neither seen nor deleted there.
    {
        let weak = window.as_weak();
        window.on_facet_clicked(move |token| {
            let Some(w) = weak.upgrade() else { return };
            let text = scour_ui::query::toggle(&w.get_query(), &token);
            w.set_query(text.as_str().into());
            w.invoke_query_changed(text.into());
            w.invoke_caret_to_end();
        });
    }
    window.on_term_on(|text, token| scour_ui::query::has_term(&text, &token));
    // Through `scour-clip`, as copying a path is: the helper holds the text on
    // Wayland after this window has lost the keyboard.
    {
        let weak = window.as_weak();
        let cat = Rc::clone(&cat);
        window.on_copy_span(move |text, from, to| {
            let Some(w) = weak.upgrade() else { return };
            let Some(part) = span_of(&text, from, to) else {
                return;
            };
            match scour_clip::text(part) {
                Ok(()) => told(&w, t(&cat.borrow(), "text copied")),
                Err(e) => told(&w, format!("{e}").into()),
            }
        });
    }
    qwire::wire(&window, &state, &link, &cat);
    grep_ui::wire(&window, &state, &link, &cat);

    // Two buttons: one writes a file, the other opens the settings. A second
    // press closes them.
    {
        let weak = window.as_weak();
        let addr = addr.clone();
        let cat_for_tools = Rc::clone(&cat);
        window.on_tool_clicked(move |what| {
            let Some(w) = weak.upgrade() else { return };
            match what.as_str() {
                "export" => {
                    export(&w, &addr, &cat_for_tools.borrow());
                }
                // What used to be panels of their own are sections of one page:
                // the old names still open it, on their section.
                other => {
                    let section = match other {
                        "faces" | "language" | "general" => "general",
                        "search" => "search",
                        "rules" => "rules",
                        // The folders are on the disks' page.
                        "disks" | "folders" => "disks",
                        "help" | "keys" => "keys",
                        "view" => "view",
                        "index" => "index",
                        "about" => "about",
                        _ => "",
                    };
                    let shown = w.get_panel() == "settings";
                    if shown && (section.is_empty() || w.get_settings_section() == section) {
                        w.set_panel("".into());
                        return;
                    }
                    let section = if section.is_empty() {
                        w.get_settings_section().to_string()
                    } else {
                        section.to_string()
                    };
                    w.set_panel("settings".into());
                    w.invoke_settings_picked(section.into());
                }
            }
        });
    }

    // The query field's right-click menu: cut, copy, paste, all of it, none.
    {
        let weak = window.as_weak();
        let cat = Rc::clone(&cat);
        window.on_query_menu_at(move |x, y| {
            trace(&format!("query menu at {x},{y}"));
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let item = |id: &str, msgid: &str, key: &str, rule: bool| MenuItem {
                id: format!("text:{id}").into(),
                label: t(&cat, msgid),
                key: key.into(),
                rule,
                ..Default::default()
            };
            let model = vec![
                item("cut", "Cut", "Ctrl+X", false),
                item("copy", "Copy", "Ctrl+C", false),
                item("paste", "Paste", "Ctrl+V", false),
                item("select-all", "Select all", "Ctrl+A", true),
                item("clear", "Clear", "Esc", false),
            ];
            w.set_menu_strip(ModelRc::default());
            w.set_menu_title("".into());
            w.set_menu_columns(false);
            w.set_menu(ModelRc::new(VecModel::from(model)));
            w.set_menu_x(x);
            w.set_menu_y(y);
            w.set_menu_open(true);
        });
    }

    // The rail and the ribbon, put away or back: kept for every face.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        window.on_panes_changed(move || {
            let Some(w) = weak.upgrade() else { return };
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    rail_shown: Some(w.get_rail_shown()),
                    ribbon_shown: Some(w.get_ribbon_shown()),
                    ..Default::default()
                },
            });
        });
    }

    // The Index section's three buttons. What they started is said under them;
    // the facts above are asked again as the index moves.
    {
        let weak = window.as_weak();
        window.on_index_open(move || {
            let Some(w) = weak.upgrade() else { return };
            let dir = w.get_index_dir().to_string();
            if !dir.is_empty()
                && let Err(e) = scour_openers::open(std::ffi::OsStr::new(&dir))
            {
                trace(&format!("index: the folder did not open: {e}"));
            }
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let cat = Rc::clone(&cat);
        window.on_index_tidy(move || {
            let Some(w) = weak.upgrade() else { return };
            link.send(Ask::Reorganize);
            w.set_index_said(t(
                &cat.borrow(),
                "Reorganizing. Searching goes on meanwhile.",
            ));
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let cat = Rc::clone(&cat);
        window.on_index_reset(move || {
            let Some(w) = weak.upgrade() else { return };
            link.send(Ask::Reset);
            w.set_index_said(t(
                &cat.borrow(),
                "The index was emptied and every disk is being scanned again.",
            ));
        });
    }

    // A section of the settings page, and what it needs asked for: the
    // lists come from the service, the key from the desktop, each fresh.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        window.on_settings_picked(move |section| {
            let Some(w) = weak.upgrade() else { return };
            // A section that moved is still asked for by its old name.
            let section: slint::SharedString = match section.as_str() {
                "folders" => "disks".into(),
                _ => section,
            };
            w.set_settings_section(section.clone());
            match section.as_str() {
                "rules" => link.send(Ask::Rules),
                "disks" => link.send(Ask::Volumes),
                "about" => link.send(Ask::Status),
                "index" => link.send(Ask::IndexFacts),
                // The desktop may have been changed in its own settings since
                // this window started; the row must not be stale.
                "keys" => hotkey::work(hotkey::Deed::Look),
                _ => {}
            }
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        window.on_theme_picked(move |theme| {
            if let Some(w) = weak.upgrade() {
                w.global::<Theme>().set_chosen(theme.clone());
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    theme: Some(theme.to_string()),
                    ..Default::default()
                },
            });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        window.on_face_default_picked(move |face| {
            if let Some(w) = weak.upgrade() {
                w.set_face_default(face.clone());
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    face: Some(face.to_string()),
                    ..Default::default()
                },
            });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let state = Rc::clone(&state);
        window.on_refresh_picked(move |value| {
            let Some(w) = weak.upgrade() else { return };
            let Ok(seconds) = value.parse::<u32>() else {
                return;
            };
            w.set_refresh_choice(value.clone());
            state.borrow_mut().refresh = std::time::Duration::from_secs(u64::from(seconds));
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    refresh_every: Some(seconds),
                    ..Default::default()
                },
            });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let state = Rc::clone(&state);
        window.on_perf_picked(move |kind, value| {
            let Some(w) = weak.upgrade() else { return };
            let mut change = scour_settings::Change::default();
            match kind.as_str() {
                "memory" => {
                    w.set_memory_choice(value.clone());
                    change.memory = Some(value.to_string());
                }
                "narrow" => {
                    let on = value == "on";
                    w.set_narrow_on(on);
                    change.narrow_off = Some(!on);
                    let mut s = state.borrow_mut();
                    s.qb.narrow_off = !on;
                    // Asked again on the next page, for the query in sight.
                    s.qb.narrow_for = "\u{0}".into();
                }
                "threads" => {
                    w.set_threads_choice(value.clone());
                    change.search_threads = value.parse().ok();
                }
                _ => {
                    w.set_fresh_choice(value.clone());
                    change.fresh_within = value.parse().ok();
                }
            }
            link.send(Ask::Remember { change });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        window.on_policy_picked(move |kind, value| {
            let Some(w) = weak.upgrade() else { return };
            let mut change = scour_settings::Change::default();
            if kind == "every" {
                w.set_every_choice(value.clone());
                change.folders_every = value.parse().ok();
            } else if kind == "network" {
                w.set_network_choice(value.clone());
                change.network = Some(value.to_string());
            } else {
                w.set_removable_choice(value.clone());
                change.removable = Some(value.to_string());
            }
            link.send(Ask::Remember { change });
            link.send(Ask::Volumes);
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let folders = Rc::clone(&folders);
        let cat = Rc::clone(&cat);
        window.on_folder_added(move |typed| {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            // Judged first, on its own line: a borrow in a `match`'s head lives
            // to the end of the match, and the push below then panicked with
            // "already borrowed" on every folder added (17 November).
            let judged = folder_to_add(&typed, &folders.borrow());
            match judged {
                Ok(path) => {
                    folders.borrow_mut().push(path);
                    w.set_folders(strings(&folders.borrow()));
                    w.set_folder_typed("".into());
                    w.set_folder_error("".into());
                    link.send(Ask::Remember {
                        change: scour_settings::Change {
                            folders: Some(folders.borrow().clone()),
                            ..Default::default()
                        },
                    });
                    link.send(Ask::Volumes);
                }
                Err(msgid) => w.set_folder_error(t(&cat, msgid)),
            }
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let folders = Rc::clone(&folders);
        {
            let link = Rc::clone(&link);
            window.on_reread(move |from_start| link.send(Ask::Reread { from_start }));
        }
        {
            let link = Rc::clone(&link);
            window.on_rescan_root(move |path| {
                if path.is_empty() {
                    return;
                }
                link.send(Ask::Rescan {
                    path: path.to_string(),
                });
                // The answer to that is the line beside it turning to "walking".
                link.send(Ask::Volumes);
            });
        }
        window.on_folder_dropped(move |path| {
            let Some(w) = weak.upgrade() else { return };
            folders.borrow_mut().retain(|f| f.as_str() != path.as_str());
            w.set_folders(strings(&folders.borrow()));
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    folders: Some(folders.borrow().clone()),
                    ..Default::default()
                },
            });
        });
    }

    // A folder switched on or off, or watched or only walked. The list goes to
    // the service whole, and the folders are asked for again: the answer says
    // what the switch did.
    for watch in [false, true] {
        let link = Rc::clone(&link);
        let list = Rc::clone(if watch {
            &folders_unwatched
        } else {
            &folders_off
        });
        let handler = move |path: slint::SharedString, on: bool| {
            let path = path.to_string();
            {
                let mut l = list.borrow_mut();
                l.retain(|p| p != &path);
                if !on {
                    l.push(path);
                }
            }
            let now = Some(list.borrow().clone());
            let change = if watch {
                scour_settings::Change {
                    folders_unwatched: now,
                    ..Default::default()
                }
            } else {
                scour_settings::Change {
                    folders_off: now,
                    ..Default::default()
                }
            };
            link.send(Ask::Remember { change });
            link.send(Ask::Volumes);
        };
        if watch {
            window.on_folder_watched(handler);
        } else {
            window.on_folder_switched(handler);
        }
    }

    // A disk switched in the panel, or the question answered. The bar goes at
    // once; the list that comes back says what the answer did.
    {
        let link = Rc::clone(&link);
        window.on_disk_toggled(move |id, on| {
            link.send(Ask::Disk {
                volume: id.to_string(),
                what: if on { "index" } else { "leave" }.into(),
            });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let state = Rc::clone(&state);
        window.on_disk_answered(move |what| {
            let volume = std::mem::take(&mut state.borrow_mut().asked_disk);
            if let Some(w) = weak.upgrade() {
                w.set_disk_question("".into());
            }
            if !volume.is_empty() {
                link.send(Ask::Disk {
                    volume,
                    what: what.to_string(),
                });
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
            if !on {
                peek::clear(&w);
            }
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
        window.on_peek_page_scrolled(move |idx| {
            if let Some(w) = weak.upgrade() {
                peek::scrolled(&w, idx.max(0) as usize);
            }
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
            table
                .borrow_mut()
                .widths
                .insert(which.to_string(), now as u32);
            relayout(&w, &table.borrow());
            // A drag past where the column stops — the others on their floors —
            // keeps what is on screen, not where the pointer went: kept, the
            // overshoot came back as a width nobody saw once the window grew.
            if let Some(shown) = w.get_cw().row_data(at)
                && shown < now
            {
                table
                    .borrow_mut()
                    .widths
                    .insert(which.to_string(), shown as u32);
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    widths: Some(table.borrow().widths.clone()),
                    ..Default::default()
                },
            });
        });
    }

    // The preview's width, kept with the columns' so the page opens at it too.
    {
        let link = Rc::clone(&link);
        let table = Rc::clone(&table);
        window.on_peek_resized(move |px| {
            if px <= 0 {
                table.borrow_mut().widths.remove("peek");
            } else {
                table.borrow_mut().widths.insert("peek".into(), px as u32);
            }
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
                        ..Default::default()
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
                ..Default::default()
            });
            w.set_menu_strip(ModelRc::default());
            w.set_menu_title("".into());
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
                    // The widths too: a drag is the other half of how a table
                    // was left, and the only one with no other way back.
                    t.widths.retain(|k, _| k == "peek");
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
                    widths: id.is_empty().then(|| table.borrow().widths.clone()),
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
        let configured = config.ui.language.clone();
        window.on_language_picked(move |tag| {
            let Some(w) = weak.upgrade() else { return };
            // Empty is the desktop's own: chosen as the config file and the
            // environment say, as at start.
            let speak = scour_i18n::choose(&tag, &configured);
            w.set_language(speak.as_str().into());
            w.set_language_choice(tag.clone());
            // The words change now. What comes from an answer is re-asked for,
            // because it is worded where it arrives.
            *held.borrow_mut() = Rc::new(Catalogue::for_language(&speak));
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
        let tabs = Rc::clone(&tabs);
        let cat = Rc::clone(&cat);
        window.on_sort_by(move |key| {
            stir(&state, &link);
            let key = key.to_string();
            {
                let mut s = state.borrow_mut();
                if s.sort == key {
                    s.descending = !s.descending;
                } else {
                    s.descending = scour_core::SortKey::named(&key)
                        .unwrap_or_default()
                        .starts_descending();
                    s.sort = key;
                }
                s.advance_order();
            }
            // Read back: `sort` may be unchanged with only the direction flipped.
            if let Some(w) = weak.upgrade() {
                w.set_sorted_by(state.borrow().sort.as_str().into());
                w.set_sorted_down(state.borrow().descending);
            }
            // The tab in front says its order on its name.
            tabs.borrow_mut().hold(now_tab(&state.borrow()));
            if let Some(w) = weak.upgrade() {
                draw_tabs(&w, &tabs.borrow(), &cat.borrow());
            }
            // Kept for every face, as the page keeps it.
            let (sort, descending) = {
                let s = state.borrow();
                (s.sort.clone(), s.descending)
            };
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    sort: Some(sort),
                    descending: Some(descending),
                    ..Default::default()
                },
            });
            let rows = match weak.upgrade() {
                Some(w) => {
                    w.set_busy(true);
                    w.get_visible_rows().max(0) as u32
                }
                None => 60,
            };
            dispatch(&state, &link, &model, rows);
            if let Some(w) = weak.upgrade()
                && state.borrow().grep.open
            {
                grep_ui::trigger(&w, &mut state.borrow_mut(), &link, &cat.borrow(), false);
            }
        });
    }

    // --- the report -------------------------------------------------------
    // Weighed when looked at and whenever a folder in it is pressed, never while
    // the search tab is showing, because it is a walk.
    {
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let cat = Rc::clone(&cat);
        let weighed = Rc::clone(&weighed);
        let weak = window.as_weak();
        window.on_report_open(move |path| {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            state.borrow_mut().scope = path.to_string();
            // A new scope: what is held answers for the old one.
            *weighed.borrow_mut() = report::Kept::default();
            report::words(&w, &cat, &path);
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

    // The strips run the width of the report, and which of their segments can
    // carry a word is decided in Rust. A resize changes that answer, and the
    // reply is kept so the folder is not weighed again to find out.
    {
        let cat = Rc::clone(&cat);
        let weighed = Rc::clone(&weighed);
        let weak = window.as_weak();
        window.on_report_resized(move || {
            let Some(w) = weak.upgrade() else { return };
            let held = weighed.borrow();
            let Some(u) = held.usage.as_ref() else { return };
            report::draw_usage(&w, &cat.borrow().clone(), &held.path, u);
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
        let weak = window.as_weak();
        // The fuller report, in the browser, on the same question. Not a switch
        // of face: this window stays, and so does the face somebody prefers.
        {
            let weak = window.as_weak();
            let state = Rc::clone(&state);
            let cat = Rc::clone(&cat);
            window.on_report_in_browser(move || {
                let Some(w) = weak.upgrade() else { return };
                let Some(mut command) = scour_launch::face("browser") else {
                    return;
                };
                let query = state.borrow().query.clone();
                command.env(
                    "SCOUR_APP_GOTO",
                    format!("q={}&tab=report", query_encoded(&query)),
                );
                match command.spawn() {
                    Ok(_) => said(&w, t(&cat.borrow(), "starting…")),
                    Err(e) => said(&w, format!("browser: {e}").into()),
                }
            });
        }
        window.on_report_search(move || {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            let scope = state.borrow().scope.clone();
            let query = if scope.is_empty() {
                String::new()
            } else {
                qwire::place_term(&scope)
            };
            w.set_tab("search".into());
            // Set, then tell: the field shows what is searched.
            w.set_query(query.as_str().into());
            w.invoke_query_changed(query.into());
            w.invoke_caret_to_end();
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
        let rows = Rc::clone(&rows);
        window.on_navigated(move |row| {
            let mut held = picks.borrow_mut();
            held.focused = path_of(&rows, row);
            held.pinned = true;
            held.chasing = 0;
        });
    }
    {
        let picks = Rc::clone(&picks);
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
            held.focused = Some(here.path.clone());
            held.pinned = true;
            held.chasing = 0;
            if run {
                // The run is what is in hand: a range over millions can cross
                // pages nobody has fetched.
                held.rows.clear();
                held.at.clear();
                let anc = held.anchor_in(&rows);
                let (from, to) = if anc <= row { (anc, row) } else { (row, anc) };
                for at in from.max(0)..=to.max(0) {
                    if let Some(pick) = rows.pick_at(at as usize) {
                        held.at.insert(pick.path.clone(), at as usize);
                        held.rows.insert(pick.path.clone(), pick);
                    }
                }
            } else if adding {
                held.anchor_row = row;
                held.anchor_path = Some(here.path.clone());
                if held.rows.remove(&here.path).is_some() {
                    held.at.remove(&here.path);
                } else {
                    held.at.insert(here.path.clone(), row.max(0) as usize);
                    held.rows.insert(here.path.clone(), here);
                }
            } else {
                held.anchor_row = row;
                held.anchor_path = Some(here.path.clone());
                held.rows.clear();
                held.at.clear();
                held.at.insert(here.path.clone(), row.max(0) as usize);
                held.rows.insert(here.path.clone(), here);
            }
            show_picks(&w, &cat, &rows, &held, false);
        });
    }
    {
        let picks = Rc::clone(&picks);
        let rows = Rc::clone(&rows);
        let slots = Rc::clone(&slots);
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let cat = Rc::clone(&cat);
        let weak = window.as_weak();
        window.on_pick_focused(move || {
            let Some(w) = weak.upgrade() else { return };
            let target = {
                let mut held = picks.borrow_mut();
                let keys: Vec<String> = held.rows.keys().cloned().collect();
                if keys.is_empty() {
                    return;
                }
                let chosen = if keys.len() > 1
                    && let Some(ref cur) = held.focused
                    && let Some(pos) = keys.iter().position(|k| k == cur)
                {
                    let visible = rows
                        .find_path(cur)
                        .is_some_and(|r| scroll_to_keep_row(&w, r, true).is_none());
                    if visible {
                        keys[(pos + 1) % keys.len()].clone()
                    } else {
                        cur.clone()
                    }
                } else {
                    keys[0].clone()
                };
                held.focused = Some(chosen.clone());
                held.pinned = true;
                let row = rows
                    .find_path(&chosen)
                    .or_else(|| held.at.get(&chosen).copied());
                (chosen, row)
            };
            if let Some(row) = target.1 {
                w.set_selected(row as i32);
                if scroll_to_keep_row(&w, row, true).is_some() {
                    let (top, h, _, vis) = row_geometry(&w, row, true);
                    w.invoke_scroll_to((top - (vis - h) * 0.5).max(0.0));
                }
                place(&w, &slots);
                follow(&w, &state, &link, &rows);
                if w.get_peeking() {
                    let cat_now = cat.borrow().clone();
                    peek(&w, &state, &link, &rows, &cat_now);
                }
            }
            w.invoke_focus_query();
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
            w.set_selected(-1);
            show_picks(&w, &cat, &rows, &picks.borrow(), false);
        });
    }
    {
        let picks = Rc::clone(&picks);
        let weak = window.as_weak();
        let cat = Rc::clone(&cat);
        window.on_pick_copied(move || {
            let cat = cat.borrow().clone();
            for pick in picks.borrow().rows.values() {
                println!("{}", pick.path);
            }
            if let Some(w) = weak.upgrade() {
                told(&w, t(&cat, "path printed to the terminal"));
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
            if folders.len() > scour_ui::OPEN_FOLDERS_MOST {
                return;
            }
            // More than a few windows is asked about first.
            if folders.len() > scour_ui::OPEN_FOLDERS_ASK && !asking.get() {
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
        let cat = Rc::clone(&cat);
        window.on_activated(move |i| {
            stir(&state, &link);
            let query = full_query(&state.borrow());
            remember(&state, &link, &query);
            let Some(w) = weak.upgrade() else { return };
            w.set_has_past(!state.borrow().past.is_empty());
            let Some((path, runs)) = usize::try_from(i).ok().and_then(|r| rows.opening_at(r))
            else {
                return;
            };
            // **A program is shown, never started**: a search lists whatever
            // is in Downloads and in unpacked archives, and a double-click is
            // too easy to make for that to run it. Its folder, with it selected.
            if runs {
                scour_openers::reveal(&[scour_core::path::to_path(&path).as_path()]);
                let name = path.rsplit('/').next().unwrap_or(&path);
                // The status line, not `hint`: that is the empty field's
                // placeholder, and the field holds a query here.
                told(
                    &w,
                    t(
                        &cat.borrow(),
                        "{name} is a program: shown in its folder, not run",
                    )
                    .replace("{name}", name)
                    .into(),
                );
            } else {
                open(&path);
            }
        });
    }
    // A double-click: on the location, the row in its folder; anywhere else
    // what Enter does. Which column is read off the column table, the one the
    // rows and the header are drawn from.
    {
        let rows = Rc::clone(&rows);
        let weak = window.as_weak();
        window.on_opened_at(move |i, x| {
            let Some(w) = weak.upgrade() else { return };
            let (heads, cx, cw) = (w.get_heads(), w.get_cx(), w.get_cw());
            let column = (0..heads.row_count()).find(|&c| {
                let (Some(at), Some(wide)) = (cx.row_data(c), cw.row_data(c)) else {
                    return false;
                };
                x >= at && x < at + wide
            });
            let on_location = column
                .and_then(|c| heads.row_data(c))
                .is_some_and(|h| h.id == "path");
            if on_location {
                if let Some(path) = path_of(&rows, i) {
                    scour_openers::reveal(&[scour_core::path::to_path(&path).as_path()]);
                }
            } else {
                w.invoke_activated(i);
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
            w.set_lens_open(false);
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
        // The row selected in its folder, as the menu's "open its folder" does:
        // opening the folder alone selected nothing.
        window.on_reveal(move |i| {
            if let Some(path) = path_of(&rows, i) {
                scour_openers::reveal(&[scour_core::path::to_path(&path).as_path()]);
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
                told(&w, t(&cat, "path printed to the terminal"));
            }
        });
    }

    // A menu item by its key: about the selected row, so whatever path a
    // menu set aside is dropped first.
    {
        let menu_path = Rc::clone(&menu_path);
        let weak = window.as_weak();
        window.on_key_action(move |id| {
            menu_path.borrow_mut().take();
            if let Some(w) = weak.upgrade() {
                w.invoke_menu_pick(id);
            }
        });
    }

    // --- the right-click menu --------------------------------------------
    // What the menu says and which items a row gets is `scour_ui::menu`'s.
    {
        let rows = Rc::clone(&rows);
        let picks = Rc::clone(&picks);
        let menu_path = Rc::clone(&menu_path);
        let weak = window.as_weak();
        let cat = Rc::clone(&cat);
        window.on_menu_at(move |i, x, y| {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            // A row of the list, so the selection is what the menu is about.
            menu_path.borrow_mut().take();
            let row_idx = usize::try_from(i).unwrap_or(0);
            let here = rows.what_at(row_idx);
            if let Some((ref path, _, _)) = here {
                let mut held = picks.borrow_mut();
                held.focused = Some(path.clone());
                held.pinned = true;
                held.chasing = 0;
                if !held.rows.contains_key(path)
                    && let Some(pick) = rows.pick_at(row_idx)
                {
                    held.anchor_row = i;
                    held.anchor_path = Some(path.clone());
                    held.rows.clear();
                    held.at.clear();
                    held.at.insert(path.clone(), row_idx);
                    held.rows.insert(path.clone(), pick);
                    show_picks(&w, &cat, &rows, &held, false);
                }
            }
            let picked = picks.borrow().rows.len();
            let is_dir = here.as_ref().is_some_and(|(_, d, _)| *d);
            let (strip, list) = menu_parts(&cat, picked, is_dir);
            if picked > 1 {
                menu_heading_many(&w, &cat, &picks.borrow());
            } else if let Some((path, is_dir, bytes)) = &here {
                menu_heading(&w, path, *is_dir, *bytes);
            }
            w.set_menu_strip(slint::ModelRc::new(slint::VecModel::from(strip)));
            w.set_menu(slint::ModelRc::new(slint::VecModel::from(list)));
            w.set_menu_x(x);
            w.set_menu_y(y);
            w.set_menu_open(true);
        });
    }

    // The same menu over one of the report's heaviest files. It is about that
    // one path, not about the list's selection, which is a different set.
    {
        let menu_path = Rc::clone(&menu_path);
        let weak = window.as_weak();
        let cat = Rc::clone(&cat);
        window.on_big_menu_at(move |path, x, y| {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            *menu_path.borrow_mut() = Some(path.to_string());
            let (strip, list) = menu_parts(&cat, 1, false);
            let bytes = std::fs::metadata(path.as_str())
                .map(|m| m.len() as i64)
                .unwrap_or(-1);
            menu_heading(&w, &path, false, bytes);
            w.set_menu_strip(slint::ModelRc::new(slint::VecModel::from(strip)));
            w.set_menu(slint::ModelRc::new(slint::VecModel::from(list)));
            w.set_menu_x(x);
            w.set_menu_y(y);
            w.set_menu_open(true);
        });
    }

    // A row pulled out of the window: the selection when the row is in it,
    // otherwise the row alone — what a file manager does.
    {
        let rows = Rc::clone(&rows);
        let picks = Rc::clone(&picks);
        let weak = window.as_weak();
        window.on_drag_out(move |row, x, y| {
            let Some(w) = weak.upgrade() else { return };
            let row = usize::try_from(row).unwrap_or(0);
            let paths: Vec<String> = {
                let picks = picks.borrow();
                let row_path = rows.path_at(row);
                if picks.rows.len() > 1
                    && row_path
                        .as_deref()
                        .is_some_and(|p| picks.rows.contains_key(p))
                {
                    picks.rows.keys().cloned().collect()
                } else {
                    row_path.into_iter().collect()
                }
            };
            trace(&format!("drag-out row {row}: {} path(s)", paths.len()));
            if !paths.is_empty() {
                drag::files(&w, paths, x, y, |_| {});
            }
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
        let menu_path = Rc::clone(&menu_path);
        let addr = addr.clone();
        window.on_menu_pick(move |id| {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            // The query field's own menu: edits, not a row.
            if let Some(what) = id.strip_prefix("text:") {
                w.invoke_query_edit(what.into());
                return;
            }
            let cat_now = cat.borrow().clone();
            let here = w.get_selected();
            // A path set aside by the report names one file and nothing else;
            // otherwise the menu is about the row the list has selected.
            let over = menu_path.borrow().clone();
            // The size too, for "find its duplicates": a copy is always among
            // the files of exactly its size. Off the disk for the report's path.
            let (path, is_dir, bytes) = match over {
                Some(one) => {
                    let b = std::fs::metadata(&one).map_or(-1, |m| m.len() as i64);
                    (one, false, b)
                }
                None => match rows.what_at(usize::try_from(here).unwrap_or(0)) {
                    Some((p, dir, b)) => (p, dir, b),
                    None => return,
                },
            };
            let chosen: Vec<String> =
                if menu_path.borrow().is_none() && picks.borrow().rows.len() > 1 {
                    picks.borrow().rows.keys().cloned().collect()
                } else {
                    vec![path.clone()]
                };
            // The status line: `hint` is the empty field's placeholder, unseen while
            // the field holds the query these rows came from.
            let say = |w: &MainWindow, msg: slint::SharedString| told(w, msg);
            let folder = |p: &str| match p.rfind('/') {
                Some(0) => "/".to_string(),
                Some(at) => p[..at].to_string(),
                None => ".".to_string(),
            };
            let leaf = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();

            match id.as_str() {
                "open" => open(&path),
                // The row itself, selected in its folder: opening the folder
                // alone did nothing to one already open, and showed nothing.
                "folder" => scour_openers::reveal(&[scour_core::path::to_path(&path).as_path()]),
                "folders" => {
                    let native: Vec<std::path::PathBuf> = chosen
                        .iter()
                        .map(|p| scour_core::path::to_path(p))
                        .collect();
                    let paths: Vec<&std::path::Path> =
                        native.iter().map(std::path::PathBuf::as_path).collect();
                    scour_openers::reveal(&paths);
                }
                "clear" => {
                    picks.borrow_mut().clear();
                    w.set_selected(-1);
                    show_picks(&w, &cat_now, &model, &picks.borrow(), false);
                }

                // The three that are only possible because there is an index.
                // Into the query, where it can be seen and taken out: in place
                // of a term of the same field, the rest of the query kept.
                "search-here" => {
                    let scope = if is_dir { path.clone() } else { folder(&path) };
                    put_term(&w, &qwire::place_term(&scope));
                }
                "search-kind" => {
                    let name = leaf(&path);
                    match name.rfind('.').filter(|at| *at > 0) {
                        Some(at) => put_term(&w, &format!("ext:{}", name[at + 1..].to_lowercase())),
                        None => say(&w, t(&cat_now, "no extension to search for")),
                    }
                }
                // Every copy of a file is among the files of exactly its size: the
                // list shows them, here, and says so. It used to open the report
                // and ask it nothing about this file (17 November).
                "duplicates" => {
                    if bytes < 0 {
                        say(&w, t(&cat_now, "its size is not known"));
                    } else {
                        w.set_tab("search".into());
                        let q = format!("size:={bytes}");
                        w.set_query(q.as_str().into());
                        w.invoke_query_changed(q.into());
                        say(
                            &w,
                            t(&cat_now, "files the size of {name}: any copy of it is here")
                                .replace("{name}", &leaf(&path))
                                .into(),
                        );
                    }
                }
                "usage" => {
                    w.invoke_query_changed(format!("under:{path}").into());
                    w.set_tab("report".into());
                }
                "skip" => {
                    w.set_panel("settings".into());
                    w.invoke_settings_picked("rules".into());
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
                    let native: Vec<std::path::PathBuf> = chosen
                        .iter()
                        .map(|p| scour_core::path::to_path(p))
                        .collect();
                    let paths: Vec<&std::path::Path> =
                        native.iter().map(std::path::PathBuf::as_path).collect();
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
                            ..Default::default()
                        })
                        .collect();
                    w.set_menu_strip(slint::ModelRc::default());
                    w.set_menu(slint::ModelRc::new(slint::VecModel::from(model)));
                    w.set_menu_open(true);
                }

                "send" => {
                    // The menu becomes the places, as it becomes the programs
                    // for "Open with…"; a drive's name is its own.
                    let found = scour_sendto::targets();
                    if found.is_empty() {
                        say(&w, t(&cat_now, "nowhere to send to on this machine"));
                        return;
                    }
                    let model: Vec<MenuItem> = found
                        .iter()
                        .map(|o| MenuItem {
                            id: format!("send:{}", o.id).into(),
                            label: if o.named {
                                o.label.as_str().into()
                            } else {
                                t(&cat_now, &o.label)
                            },
                            key: "".into(),
                            rule: false,
                            careful: false,
                            heavy: false,
                            on: false,
                            off: false,
                            ..Default::default()
                        })
                        .collect();
                    w.set_menu_strip(slint::ModelRc::default());
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
                "trash" | "open-all" | "delete" => {
                    let many = chosen.len() > 1;
                    let title = t(
                        &cat_now,
                        match (id.as_str(), many) {
                            ("trash", _) => "Move to trash",
                            ("delete", false) => "Delete permanently…",
                            ("delete", true) => "Delete {n} permanently…",
                            _ => "Open all {n}…",
                        },
                    )
                    .replace("{n}", &grouped(chosen.len() as u64));
                    // A folder's name reads like a file's; it takes its tree with it.
                    let dir = |p: &String| {
                        (*p == path && is_dir)
                            || picks.borrow().rows.get(p).is_some_and(|k| k.is_dir)
                    };
                    // Eight names and a count: a longer list is one nobody reads.
                    let mut body: Vec<String> = chosen
                        .iter()
                        .take(8)
                        .map(|p| if dir(p) { leaf(p) + "/" } else { leaf(p) })
                        .collect();
                    if chosen.len() > 8 {
                        body.push(format!("… +{}", grouped((chosen.len() - 8) as u64)));
                    }
                    // Said before the names: nothing brings these back.
                    if id == "delete" {
                        if chosen.iter().any(dir) {
                            body.insert(
                                0,
                                t(&cat_now, "Folders go with everything in them.").to_string(),
                            );
                        }
                        body.insert(0, t(&cat_now, "This cannot be undone.").to_string());
                    }
                    w.set_ask_title(title.into());
                    w.set_ask_body(body.join("\n").into());
                    w.set_ask_yes(t(
                        &cat_now,
                        match id.as_str() {
                            "trash" => "Move",
                            "delete" => "Delete",
                            _ => "Open",
                        },
                    ));
                    w.set_ask_no(t(&cat_now, "Cancel"));
                    w.set_ask_typing(false);
                    *pending.borrow_mut() = Some((id.to_string(), chosen));
                    w.set_ask_open(true);
                }

                other if other.starts_with("send:") => {
                    let native: Vec<std::path::PathBuf> = chosen
                        .iter()
                        .map(|p| scour_core::path::to_path(p))
                        .collect();
                    let paths: Vec<&std::path::Path> =
                        native.iter().map(std::path::PathBuf::as_path).collect();
                    let said = match scour_sendto::send(&other["send:".len()..], &paths) {
                        Ok(scour_sendto::Sent::Linked(_)) => {
                            t(&cat_now, "a link is on the desktop")
                        }
                        Ok(scour_sendto::Sent::Handed) => t(&cat_now, "handed over"),
                        Ok(scour_sendto::Sent::Packing(at)) => t(&cat_now, "packing into {path}")
                            .replace("{path}", &at.to_string_lossy())
                            .into(),
                        Ok(scour_sendto::Sent::Copying(at)) => t(&cat_now, "copying to {path}")
                            .replace("{path}", &at.to_string_lossy())
                            .into(),
                        Err(e) => t(&cat_now, e.msgid()),
                    };
                    say(&w, said);
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
                                scour_openers::launch(&chosen, &scour_core::path::to_path(&path))
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
                match scour_name::rename(&scour_core::path::to_path(from), &asked) {
                    Ok(now) => {
                        // Both ends: one path is gone, the other has appeared.
                        recheck(&addr, vec![from.clone(), scour_core::path::from_path(&now)]);
                        let query = state.borrow().query.clone();
                        w.invoke_query_changed(query.into());
                    }
                    Err(why) => told(&w, t(&cat_now, why.msgid())),
                }
                return;
            }
            if what == "open-all" {
                for p in &paths {
                    open(p);
                }
                return;
            }
            if what == "delete" {
                let mut gone = 0usize;
                let mut refused: Option<String> = None;
                for p in &paths {
                    match scour_trash::erase(&scour_core::path::to_path(p)) {
                        Ok(()) => gone += 1,
                        Err(e) => {
                            refused.get_or_insert_with(|| e.to_string());
                        }
                    }
                }
                recheck(&addr, paths);
                told(
                    &w,
                    match refused {
                        Some(why) => why.into(),
                        None => t(&cat_now, "{n} deleted")
                            .replace("{n}", &grouped(gone as u64))
                            .into(),
                    },
                );
                let query = state.borrow().query.clone();
                w.invoke_query_changed(query.into());
                return;
            }
            // The move is this process's; the service is only told to look again.
            let mut gone = 0usize;
            let mut refused: Option<String> = None;
            for p in &paths {
                match scour_trash::trash(&scour_core::path::to_path(p)) {
                    Ok(_) => gone += 1,
                    Err(e) => {
                        refused.get_or_insert_with(|| e.to_string());
                    }
                }
            }
            recheck(&addr, paths);
            told(
                &w,
                match refused {
                    Some(why) => why.into(),
                    None => t(&cat_now, "{n} moved to trash")
                        .replace("{n}", &grouped(gone as u64))
                        .into(),
                },
            );
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
    } else if !front.query.is_empty() {
        window.set_query(front.query.as_str().into());
        window.invoke_query_changed(front.query.as_str().into());
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

    // **What scrolled into sight.** See [`follow`] for the decision. The slots
    // first: this runs before the frame that shows the new position is drawn.
    {
        let rows = Rc::clone(&rows);
        let slots = Rc::clone(&slots);
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let weak = window.as_weak();
        // Restarted on every move, so it fires a moment after the last one.
        let still = Rc::new(slint::Timer::default());
        window.on_moved(move || {
            stir(&state, &link);
            if let Some(w) = weak.upgrade() {
                w.set_moving(true);
                let weak = w.as_weak();
                still.start(
                    slint::TimerMode::SingleShot,
                    std::time::Duration::from_millis(300),
                    move || {
                        if let Some(w) = weak.upgrade() {
                            w.set_moving(false);
                        }
                    },
                );
                place(&w, &slots);
                follow(&w, &state, &link, &rows);
            }
        });
    }
    // The backstop, for what changes the view without moving the list: ten times
    // a second is cheap to leave running and never what scrolling waits for.
    {
        let rows = Rc::clone(&rows);
        let slots = Rc::clone(&slots);
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
                // The zoom again if the desktop put its own scale back — a
                // move to another monitor does.
                apply_zoom(&w);
                // The window is what knows how wide a line is: mode and width
                // both decide it, and both change without asking.
                lines.per_line(if w.get_grid() {
                    w.get_per_line().max(1) as usize
                } else {
                    0
                });
                lines.sync();
                place(&w, &slots);
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
        window.global::<Theme>().set_chosen(scheme.into());
    }

    // How long the event loop was held, a second at a time: a tick every 2 ms
    // that arrives late was kept waiting by a frame or by a callback. Average
    // frame rates hide the one long frame a person sees as a stutter.
    if std::env::var_os("SCOUR_GUI_STALLS").is_some() {
        let last = std::cell::Cell::new(std::time::Instant::now());
        let gaps = std::cell::RefCell::new(Vec::<f64>::new());
        let since = std::cell::Cell::new(std::time::Instant::now());
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(2),
            move || {
                let now = std::time::Instant::now();
                gaps.borrow_mut()
                    .push(now.duration_since(last.replace(now)).as_secs_f64() * 1e3);
                if since.get().elapsed() >= std::time::Duration::from_secs(1) {
                    since.set(now);
                    let mut g = std::mem::take(&mut *gaps.borrow_mut());
                    g.sort_by(f64::total_cmp);
                    let over = |ms: f64| g.iter().filter(|&&x| x > ms).count();
                    // Busy is what each tick arrived past its 2 ms.
                    let busy: f64 = g.iter().map(|x| (x - 2.0).max(0.0)).sum();
                    eprintln!(
                        "stalls: busy {busy:.0} ms/s · max {:.1} ms · p99 {:.1} · >20 ms {} · >33 ms {} · >50 ms {}",
                        g.last().copied().unwrap_or(0.0),
                        g.get(g.len() * 99 / 100).copied().unwrap_or(0.0),
                        over(20.0),
                        over(33.0),
                        over(50.0),
                    );
                }
            },
        );
    }

    // A wheel turned over the middle of the window, `SCOUR_GUI_WHEEL=px` a
    // notch, one every `SCOUR_GUI_SCROLL_MS`: the events a person sends, so
    // only what a person moves is redrawn — `scroll_to` moves hidden views too.
    if let Ok(px) = std::env::var("SCOUR_GUI_WHEEL")
        && let Ok(px) = px.parse::<f32>()
    {
        let every = std::env::var("SCOUR_GUI_SCROLL_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(16);
        let steps = std::env::var("SCOUR_GUI_WHEEL_STEPS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(400usize);
        let weak = window.as_weak();
        let done = std::cell::Cell::new(0usize);
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(every),
            move || {
                let Some(w) = weak.upgrade() else { return };
                if done.get() >= steps {
                    return;
                }
                done.set(done.get() + 1);
                let size = w.window().size().to_logical(w.window().scale_factor());
                w.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: slint::LogicalPosition::new(size.width / 2.0, size.height / 2.0),
                        delta_x: 0.0,
                        delta_y: -px,
                    });
            },
        );
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
                    held.focused = Some(pick.path.clone());
                    held.pinned = true;
                    held.rows.insert(pick.path.clone(), pick);
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
                // `SCOUR_GUI_SECTION=folders`: which page of the settings, for a
                // photograph of one.
                if let Ok(section) = std::env::var("SCOUR_GUI_SECTION") {
                    w.invoke_settings_picked(section.as_str().into());
                }
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
        // Nothing named is everything indexed, which is the scope a press on the
        // tab opens; `SCOUR_GUI_SCOPE` starts further down.
        let scope = std::env::var("SCOUR_GUI_SCOPE").unwrap_or_default();
        window.invoke_report_open(scope.as_str().into());
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

    // `SCOUR_GUI_MOVE=280,69`: the pointer rests there, nothing pressed: for the
    // look of what appears under it. Logical pixels.
    if let Ok(spec) = std::env::var("SCOUR_GUI_MOVE") {
        let p: Vec<f32> = spec
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        if let [x, y] = p[..] {
            let weak = window.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            t.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(2500),
                move || {
                    if let Some(w) = weak.upgrade() {
                        w.window()
                            .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                                position: slint::LogicalPosition::new(x, y),
                            });
                    }
                },
            );
        }
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
        } else if let [x, y, n] = point[..] {
            // `x,y,n`: n clicks in one place, at once — `2` is a double-click.
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
                    trace(&format!("synthetic {n} clicks at {x},{y}"));
                    w.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                            position: at,
                        });
                    let button = slint::platform::PointerEventButton::Left;
                    for _ in 0..n as u32 {
                        w.window()
                            .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                                position: at,
                                button,
                            });
                        w.window()
                            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                                position: at,
                                button,
                            });
                    }
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

    // `SCOUR_GUI_SIDEWAYS=x,y,px`: after `SCOUR_GUI_CLICK_MS`, one sideways
    // wheel step of px at x,y — a touchpad pushing a panel's content across.
    if let Ok(spec) = std::env::var("SCOUR_GUI_SIDEWAYS") {
        let n: Vec<f32> = spec
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .collect();
        if let [x, y, px] = n[..] {
            let after = std::env::var("SCOUR_GUI_CLICK_MS")
                .ok()
                .and_then(|ms| ms.parse().ok())
                .unwrap_or(2500);
            let weak = window.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            t.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(after),
                move || {
                    let Some(w) = weak.upgrade() else { return };
                    let position = slint::LogicalPosition::new(x, y);
                    w.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
                    w.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                            position,
                            delta_x: -px,
                            delta_y: 0.0,
                        });
                    trace(&format!("sideways {px} at {x},{y}"));
                },
            );
        }
    }

    // `SCOUR_GUI_DRAG=x1,y,x2`: after `SCOUR_GUI_CLICK_MS`, press at x1, move
    // to x2 in 10px steps a frame apart, let go — a hand on a column's edge.
    if let Ok(spec) = std::env::var("SCOUR_GUI_DRAG") {
        let n: Vec<f32> = spec
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .collect();
        if let [from, y, to] = n[..] {
            let after: u64 = std::env::var("SCOUR_GUI_CLICK_MS")
                .ok()
                .and_then(|ms| ms.parse().ok())
                .unwrap_or(2500);
            let weak = window.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            let mut tick = 0u64;
            let mut x = from;
            let mut done = false;
            let button = slint::platform::PointerEventButton::Left;
            t.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(16),
                move || {
                    tick += 1;
                    let Some(w) = weak.upgrade() else { return };
                    if done || tick * 16 < after {
                        return;
                    }
                    let win = w.window();
                    let at = |x: f32| slint::LogicalPosition::new(x, y);
                    if tick * 16 < after + 16 {
                        win.dispatch_event(slint::platform::WindowEvent::PointerMoved {
                            position: at(x),
                        });
                        win.dispatch_event(slint::platform::WindowEvent::PointerPressed {
                            position: at(x),
                            button,
                        });
                        trace(&format!("drag from {x},{y}"));
                        return;
                    }
                    x = if (to - x).abs() <= 10.0 {
                        to
                    } else {
                        x + 10.0f32.copysign(to - x)
                    };
                    win.dispatch_event(slint::platform::WindowEvent::PointerMoved {
                        position: at(x),
                    });
                    if x == to {
                        win.dispatch_event(slint::platform::WindowEvent::PointerReleased {
                            position: at(x),
                            button,
                        });
                        trace(&format!("drag let go at {x},{y}"));
                        done = true;
                    }
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

    // `SCOUR_GUI_RESIZE=1240,820` — resize from the inside, because there is
    // no hand to drag the corner in a test. `1240,820,1500` says when.
    if let Ok(spec) = std::env::var("SCOUR_GUI_RESIZE") {
        let at: Vec<f32> = spec
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        if let [w, h, ..] = at[..] {
            let after = at.get(2).copied().unwrap_or(1200.0) as u64;
            let weak = window.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            t.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(after),
                move || {
                    let Some(win) = weak.upgrade() else { return };
                    trace(&format!("resizing to {w}x{h}"));
                    win.window().set_size(slint::LogicalSize::new(w, h));
                },
            );
        }
    }

    // For a photograph of the time lens's calendar: the lens opened on the
    // query's term, the calendar shown, `SCOUR_GUI_CAL` days ago picked first
    // and three days ago under the pointer.
    if let Some(back) = std::env::var("SCOUR_GUI_CAL")
        .ok()
        .and_then(|d| d.parse::<i32>().ok())
    {
        let weak = window.as_weak();
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(1200),
            move || {
                let Some(w) = weak.upgrade() else { return };
                w.invoke_lens_field("dm".into());
                w.set_cal_open(true);
                w.invoke_cal_opened();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                let today = scour_ui::calendar::today(now).0 as i32;
                w.set_cal_from(today - back);
                w.set_cal_hover(today - 3);
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
    // Ctrl and + or - make everything bigger or smaller, Ctrl+0 puts it back.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        window.on_zoom_step(move |step| {
            let Some(w) = weak.upgrade() else { return };
            let now = ZOOM.with(std::cell::Cell::get);
            let next = match step {
                0 => default_zoom(),
                s => ((now + 0.1 * s as f32) * 20.0).round() / 20.0,
            };
            set_zoom(&w, &link, next);
        });
    }
    // The same, from the settings' row.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        window.on_zoom_picked(move |v| {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(next) = v.parse::<f32>() {
                set_zoom(&w, &link, next);
            }
        });
    }
    // The first frame is drawn at the desktop's scale; the zoom follows at once.
    {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            if let Some(w) = weak.upgrade() {
                apply_zoom(&w);
                // A scale change takes the focus with it.
                w.invoke_focus_query();
                drag::prepare(&w);
                frame::let_go_on_the_frame(w.window());
            }
        });
    }
    window.run().context("the event loop failed")?;
    #[cfg(unix)]
    drop(claim);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    table: &Rc<RefCell<Table>>,
    rows: &Rc<rows::Rows>,
    lines: &Rc<rows::Lines>,
    picks: &Rc<RefCell<Selection>>,
    facets: &Rc<VecModel<Facet>>,
    rules: &[Rc<VecModel<Rule>>; 3],
    cat: &Rc<Catalogue>,
    link: &Rc<Link>,
    weighed: &Rc<RefCell<report::Kept>>,
    got: Got,
) {
    match got {
        Got::Key(done) => hotkey::landed(w, cat, *done),
        Got::Grep {
            generation,
            append,
            reply,
        } => {
            grep_ui::answered(
                w,
                &mut state.borrow_mut(),
                link,
                cat,
                generation,
                append,
                *reply,
            );
        }
        Got::PickedFolder { a, b, prefix, path } => {
            qwire::picked_folder(w, state, a, b, prefix, path);
        }
        Got::Down(why) => {
            let mut s = state.borrow_mut();
            s.down = true;
            s.search_out = false;
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
            let again = {
                let mut s = state.borrow_mut();
                s.down = false;
                // The question the service was not there for: nothing else
                // fetches while its rewind is pending.
                let again = s.rewind && !s.search_out;
                s.search_out |= again;
                again.then_some(s.row_limit)
            };
            if let Some(limit) = again {
                send_search(state, link, 0, limit);
            }
            w.set_hint(t(
                cat,
                "file name  ·  ext:pdf  ·  kind:image dm:7d  ·  size:>10mb",
            ));
        }
        // The empty list is right: there is nothing to show until a service
        // answers. Only the line under it changes.
        Got::Starting => said(w, t(cat, "Starting the Scour service…")),
        Got::Started(why) => match why {
            // The reason first, then the hint the window used to show alone.
            Some(why) => {
                w.set_busy(false);
                said(
                    w,
                    format!("{why} — {}", cat.get("Start one with `scourd`.")).into(),
                );
            }
            None => said(w, t(cat, "connecting…")),
        },
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
            // A `content:` query's rows are two lines: the file, and where it
            // holds the text. The height is the theme's, which every scroll
            // and slot reads, so changing it is the whole of the switch.
            let tall = scour_ui::lens::reads(&state.borrow().query);
            if w.get_tall() != tall {
                w.set_tall(tall);
                w.global::<Theme>().set_row_height(if tall {
                    scour_ui::METRICS.row + 22.0
                } else {
                    scour_ui::METRICS.row
                });
            }
            // What a first page that found nothing nearly found, or nothing.
            if offset == 0 {
                let offered = r.similar.as_ref().filter(|_| r.total == 0);
                w.set_similar_said(match offered {
                    Some(similar) => t(cat, "Similar: {n} files —")
                        .replace(
                            "{n}",
                            &format!(
                                "{}{}",
                                grouped(similar.total),
                                if similar.capped { "+" } else { "" }
                            ),
                        )
                        .into(),
                    None => slint::SharedString::new(),
                });
                w.set_similar_query(
                    offered
                        .map(|s| slint::SharedString::from(s.query.as_str()))
                        .unwrap_or_default(),
                );
                state.borrow_mut().similar = offered.map(|s| s.query.clone()).unwrap_or_default();
                qwire::searched(w, &mut state.borrow_mut(), link, cat, r.total);
                grep_ui::listed(w, &mut state.borrow_mut(), cat, r.total, r.capped);
                qwire::narrow(w, &mut state.borrow_mut(), link, cat, r.total);
            }
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
                            system_icons: w.get_system_icons(),
                        },
                        false,
                    )
                })
                .collect();
            // Beside the page rather than on it: Slint counts in 32 bits.
            let weights: Vec<i64> = r.hits.iter().map(|h| h.meta.size.max(0)).collect();
            let n = page.len();
            // A new question's first page decides how the row is divided up;
            // later pages and refreshes leave it, or it moves under the reader.
            if offset == 0 && state.borrow().rewind {
                table
                    .borrow_mut()
                    .measure(&page, w.get_name_advance(), w.get_cell_advance());
                relayout(w, &table.borrow());
            }
            // **The row being read stays put.** A re-read of the page in sight
            // after the index moved shifts every row below a file that arrived
            // or left above it; the list is moved by the same number of rows,
            // found by the first visible row's path. Not at the top, where what
            // arrives is what somebody is waiting to see, nor in the tiles.
            let is_rewind = state.borrow().rewind && offset == 0;
            let first = w.get_first_row().max(0) as usize;
            let sel_before = w.get_selected();
            let (sel_path, sel_pinned, sel_was_visible) = if is_rewind {
                (None, false, false)
            } else {
                let mut held = picks.borrow_mut();
                if held.focused.is_none() && sel_before >= 0 {
                    held.focused = path_of(rows, sel_before);
                }
                let bar = !held.rows.is_empty();
                let pinned = held.pinned || bar;
                let vis = held.chasing > 0
                    || (sel_before >= 0 && row_in_view(w, sel_before as usize, bar));
                (held.focused.clone(), pinned, vis)
            };
            let anchor = (!is_rewind
                && !sel_pinned
                && !w.get_grid()
                && first > 0
                && (offset as usize..offset as usize + n).contains(&first))
            .then(|| rows.path_at(first))
            .flatten();
            let (any_fresh, total_len) = {
                let mut s = state.borrow_mut();
                // A reading's answer is its own count, and the newest there is:
                // neither the rail nor an earlier count knows how far it has got.
                if r.reading.is_some() {
                    s.exact_count = Some(ExactCount {
                        query_revision: s.query_revision,
                        total: r.total,
                        capped: r.capped,
                    });
                }
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
                (arrived, total)
            };
            picks.borrow_mut().refresh(rows);

            if let Some(path) = anchor
                && rows.path_at(first).as_deref() != Some(path.as_str())
            {
                let from = offset as usize;
                let moved_to = (from..from + n)
                    .find(|&row| rows.path_at(row).as_deref() == Some(path.as_str()));
                if let Some(to) = moved_to {
                    let by = to as f32 - first as f32;
                    let step = w.global::<Theme>().get_row_height();
                    w.invoke_scroll_to((w.get_list_down() + by * step).max(0.0));
                    let selected = w.get_selected();
                    if selected >= 0 {
                        w.set_selected((selected as f32 + by).max(0.0) as i32);
                    }
                    if by > 0.0 {
                        w.set_new_above(w.get_new_above() + by as i32);
                    }
                    trace(&format!("kept {path:?} in place: {by:+} rows"));
                }
            }
            let mut chase_page = None;
            if let Some(ref path) = sel_path {
                if let Some(to) = rows.find_path(path) {
                    picks.borrow_mut().chasing = 0;
                    if w.get_selected() != to as i32 {
                        w.set_selected(to as i32);
                    }
                    if sel_pinned && sel_was_visible {
                        let bar = !picks.borrow().rows.is_empty();
                        if let Some(want_down) = scroll_to_keep_row(w, to, bar) {
                            w.invoke_scroll_to(want_down);
                        }
                    }
                } else {
                    if w.get_selected() >= 0
                        && path_of(rows, w.get_selected()).as_deref() != Some(path.as_str())
                    {
                        w.set_selected(-1);
                    }
                    let next_page = offset as usize / rows::SPAN + 1;
                    if sel_pinned
                        && sel_was_visible
                        && n == limit as usize
                        && next_page * rows::SPAN < total_len
                        && picks.borrow().chasing < 2
                    {
                        picks.borrow_mut().chasing += 1;
                        chase_page = Some(next_page);
                    } else {
                        picks.borrow_mut().chasing = 0;
                    }
                }
            }
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
                s.search_out &= !rewind;
                rewind
            };
            if rewind {
                w.set_selected(0);
                w.invoke_scroll_to(0.0);
                // A different question, so the old selection answers nothing.
                let mut held = picks.borrow_mut();
                held.clear();
                held.focused = rows.path_at(0);
            }
            // The marks live on the rows, and this page brought new ones.
            if !picks.borrow().rows.is_empty() || w.get_picked() > 0 {
                let asking = w.get_pick_asking() && !picks.borrow().rows.is_empty();
                show_picks(w, cat, rows, &picks.borrow(), asking);
            }
            if w.get_peeking() && picks.borrow().chasing == 0 {
                peek(w, state, link, rows, cat);
            }
            w.set_busy(false);
            // **And straight on to the next**: nothing is asked for while an
            // answer is in flight, so a drag continues here.
            if let Some(page) = chase_page {
                let rev = state.borrow().revision;
                rows.asking(page, rev);
                send_page(state, link, (page * rows::SPAN) as u32, PAGE_MAX);
            } else {
                follow(w, state, link, rows);
            }
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
                match r.reading {
                    Some(reading) => cat
                        .get(scour_ui::reading_said(
                            reading.stage == scour_core::ReadingStage::Running,
                            reading.stage == scour_core::ReadingStage::Done,
                        ))
                        .replace("{n}", &grouped(reading.files)),
                    None => format!("{} {}", grouped(r.rows_visited), cat.get("rows read")),
                },
            );
            w.set_reading_stage(match r.reading.map(|reading| reading.stage) {
                None => 0,
                Some(scour_core::ReadingStage::Running) => 1,
                Some(scour_core::ReadingStage::Paused) => 2,
                Some(scour_core::ReadingStage::Done) => 3,
            });
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
                grep_ui::listed(w, &mut state.borrow_mut(), cat, total, capped);
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
        Got::Narrow {
            query_revision,
            slot,
            reply,
        } => qwire::narrowed(
            w,
            &mut state.borrow_mut(),
            cat,
            query_revision,
            slot,
            *reply,
        ),
        Got::Lens {
            generation,
            slot,
            reply,
        } => qwire::answered(
            w,
            &mut state.borrow_mut(),
            link,
            cat,
            generation,
            slot,
            *reply,
        ),
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
            let query = state.borrow().query.clone();
            state.borrow_mut().spans = spans;
            qwire::draw(w, &mut state.borrow_mut(), link, cat, qwire::Lensing::Keep);
            qwire::explained(w, &mut state.borrow_mut(), cat);
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
            report::draw_usage(w, cat, &path, &u);
            *weighed.borrow_mut() = report::Kept {
                usage: Some(u),
                path,
            };
        }
        // What kinds the weight under a folder is in, drawn as a ring.
        Got::Kinds { path, reply } => {
            let Response::Facets(f) = *reply else { return };
            if path != state.borrow().scope {
                return;
            }
            let Some(group) = f
                .groups
                .iter()
                .find(|g| matches!(g.by, scour_core::FacetBy::Kind))
            else {
                return;
            };
            report::draw_kinds(w, cat, &group.facets, f.total, f.capped);
        }
        // The heaviest files under it, with no count: counting is the one piece of
        // work proportional to how many match.
        Got::Biggest { path, reply } => {
            let Response::Search(r) = *reply else { return };
            if path != state.borrow().scope {
                return;
            }
            report::draw_biggest(w, &path, &r.hits);
        }
        Got::Tile { path, outcome } => {
            let drawn = rows.give_picture(&path, &outcome);
            if let (Some(&lo), Some(&hi)) = (drawn.iter().min(), drawn.iter().max()) {
                lines.touched(lo, hi + 1);
            }
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
        // The list, or an answer taken: then the list again, which says what
        // the answer did.
        Got::Volumes(reply) => match *reply {
            Response::Volumes { disks, folders } => {
                state.borrow_mut().asked_disk = disks::draw(w, cat, &disks);
                disks::draw_folders(w, cat, &folders);
                // While something is being walked and the page is open, asked
                // again: the line beside it says when the walk ends.
                let walking =
                    disks.iter().any(|d| d.seen.walking) || folders.iter().any(|f| f.walking);
                let open = w.get_panel() == "settings"
                    && matches!(w.get_settings_section().as_str(), "disks" | "folders");
                if walking && open {
                    let link = Rc::clone(link);
                    slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
                        link.send(Ask::Volumes);
                    });
                }
            }
            _ => link.send(Ask::Volumes),
        },
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
            disks_moved(state, link, st.asking);
            if w.get_panel() == "settings" && w.get_settings_section() == "index" {
                link.send(Ask::IndexFacts);
            }
            // Free: a wait is answered with the whole status. The total too,
            // or the meter kept dividing by what the index held at start-up.
            w.set_scanning(scanning_note(cat, &st));
            state.borrow_mut().indexed = st.entries;
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
            // index moves, and re-arming at once cost a quarter of a core. The
            // beat is the person's to choose — see `Settings::refresh_every`.
            let rest = AWAIT_AGAIN.max(state.borrow().refresh);
            let link = Rc::clone(link);
            let state = Rc::clone(state);
            slint::Timer::single_shot(rest, move || {
                link.send(Ask::Await {
                    since: state.borrow().revision,
                });
            });
        }
        // What the service is holding: 636 MB over three sources is a different
        // claim from the same over one.
        Got::Status(reply) => {
            let Response::Status(st) = *reply else { return };
            disks_moved(state, link, st.asking);
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
            w.set_about_body(
                format!(
                    "Scour {}\n\n{} {}\n{}",
                    env!("CARGO_PKG_VERSION"),
                    grouped(st.entries),
                    t(cat, "entries"),
                    holding
                )
                .into(),
            );
            w.set_holding(holding.into());
        }
        // This desktop's own folders, each an `under:` term. The labels are the
        // desktop's own words, so nothing here translates them.
        Got::Places(reply) => {
            if let Response::Stats(s) = &*reply {
                w.set_index_dir(s.dir.as_str().into());
                let mut facts = format!(
                    "{} {} · {}",
                    grouped(s.entries),
                    t(cat, "entries"),
                    compact_bytes(s.bytes_on_disk)
                );
                if s.written_at > 0 {
                    facts.push_str(&format!(
                        " · {} {}",
                        t(cat, "written"),
                        scour_ui::format::stamp(s.written_at)
                    ));
                }
                w.set_index_facts(facts.into());
                return;
            }
            if matches!(&*reply, Response::Accepted) {
                return;
            }
            let Response::Places(p) = *reply else {
                trace(&format!("places: unexpected reply {reply:?}"));
                return;
            };
            state.borrow_mut().mounts = p.mounts.clone();
            state.borrow_mut().qb.places = p
                .places
                .iter()
                .map(|pl| (pl.label.clone(), pl.path.clone()))
                .collect();
            let scopes: Vec<Facet> = p
                .places
                .iter()
                .map(|place| Facet {
                    label: place.label.as_str().into(),
                    token: format!("under:{}", place.path).into(),
                    count: slint::SharedString::new(),
                    share: 0.0,
                    icon: Default::default(),
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
                        icon: token.into(),
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

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

thread_local! {
    static MARKS: std::cell::Cell<(char, char)> = const { std::cell::Cell::new(('.', ',')) };
}

thread_local! {
    /// How much bigger or smaller than the desktop's scale everything is drawn.
    static ZOOM: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
}

#[cfg(test)]
mod tests;
