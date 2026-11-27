// A window, not a console program: without this Windows opens a console
// window behind it for its standard output.
#![cfg_attr(windows, windows_subsystem = "windows")]
//! The Scour window.
//!
//! A frontend only: no index, no walk, everything over a socket. Service
//! calls run on worker threads and return as events ([`link`]); query
//! meaning is the engine's; a reply for a superseded keystroke is dropped.

mod actions;
mod answers;
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
use answers::Cx;
use peeking::*;
use search::*;
use selection::*;
pub(crate) use size::*;
use start::*;
use status::*;
use tab_strip::*;
use table::*;
use wiring::Wiring;
pub(crate) use wording::*;

mod autostart;
mod disks;
mod drag;
mod flowing;
mod frame;
mod grep_field;
mod grep_lay;
mod grep_ui;
mod hotkey;
mod install;
mod link;
mod panelact;
mod panelwire;
mod paste;
mod peek;
mod pixels;
mod placeact;
mod placewire;
mod qbar;
mod qcaret;
mod qdress;
mod qeffect;
mod qflow;
mod qgroup;
mod qlay;
mod qlens;
mod qlist;
mod qpanel;
mod qplace;
mod qscope;
mod qsize;
mod qtime;
mod qtype;
mod qwire;
mod report;
mod resource_gauges;
mod resource_lines;
mod resources;
mod rows;
mod service_mode;
mod tabs;
mod tiles;
mod tray;
mod wiring;

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
    Found, HeadInfo, HeldLine, HeldPanel, Kid, LensRow, MainWindow, MemLine, MenuItem, QBit,
    QMeasure, Reach, Row, Rule, RunLine, Scheme, Seg, Sel, Slice, Slot, SortLine, Span, Theme,
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
pub(crate) fn deliver(got: Got) {
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
    /// A folder's preview, its three answers as they come.
    peek_dir: peeking::DirParts,
    /// Files already asked about, oldest first. Bounded, and outliving the rows:
    /// one screenful otherwise started eleven thumbnailers twice.
    asked_pictures: std::collections::VecDeque<String>,
    /// The status bar's panel of what the service keeps. See `resources`.
    res: resources::Panel,
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
    still_caret();
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
            content: String::new(),
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
    window.set_peek_office_text(kept.office_view == "text");
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
        peek_dir: peeking::DirParts::default(),
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
        res: resources::Panel::default(),
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
    window.set_peek_details(kept.preview_details);
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
    window.set_texts_on(!kept.texts_off);
    window.set_texts_ahead_on(kept.texts_ahead);
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
    window.set_optical_choice(kept.optical.as_str().into());
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
    let kept_layout = kept.layout.clone();
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
    let sink = move |got: Got| {
        let _ = slint::invoke_from_event_loop(move || deliver(got));
    };

    let link = Rc::new(Link::start(
        addr.clone(),
        config.state_dir().join("scourd.log"),
        sink,
    ));
    let tabs_later = Rc::new(slint::Timer::default());
    let wiring = Wiring {
        window: &window,
        config: &config,
        kept: &kept,
        cat: &cat,
        tabs: &tabs,
        tabs_later: &tabs_later,
        state: &state,
        weighed: &weighed,
        menu_path: &menu_path,
        rows: &rows,
        picks: &picks,
        lines: &lines,
        facets: &facets,
        rules: &rules,
        slots: &slots,
        table: &table,
        folders: &folders,
        folders_off: &folders_off,
        folders_unwatched: &folders_unwatched,
        addr: &addr,
        link: &link,
        opened_at: &opened_at,
    };
    wiring::service::wire(&wiring);
    wiring::window_size::wire(&wiring);
    wiring::tab_bar::wire(&wiring);
    wiring::query::wire(&wiring);
    wiring::words::wire(&wiring);
    wiring::rail::wire(&wiring);
    wiring::report_tab::wire(&wiring);
    wiring::picking::wire(&wiring);
    wiring::opening::wire(&wiring);
    wiring::history::wire(&wiring);
    wiring::menu::wire(&wiring);
    wiring::resources::wire(&wiring);
    // `SCOUR_GUI_PANIC=1` stands on the hook, and leaves a real crash's file.
    if std::env::var("SCOUR_GUI_PANIC").is_ok() {
        panic!("deliberate — proving the crash log writes");
    }

    // A query asked for on the command line is a search of its own: the front
    // tab's content line comes back only with the front tab's query.
    let reading = args.query.is_empty()
        && tab_strip::put_content(&window, &mut state.borrow_mut(), &front.content);
    if !args.query.is_empty() {
        window.set_query(args.query.clone().into());
        window.invoke_query_changed(args.query.clone().into());
    } else if !front.query.is_empty() {
        window.set_query(front.query.as_str().into());
        window.invoke_query_changed(front.query.as_str().into());
    } else if reading {
        grep_ui::trigger(
            &window,
            &mut state.borrow_mut(),
            &link,
            &cat.borrow(),
            false,
        );
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
        window.on_moved(move || {
            stir(&state, &link);
            if let Some(w) = weak.upgrade() {
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
        let picks = Rc::clone(&picks);
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
                peek(&w, &state, &link, &rows, &picks, &words.borrow().clone());
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
    wiring::hooks::wire(&wiring);
    wiring::clicks::wire(&window);
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
    let cx = Cx {
        w,
        state,
        table,
        rows,
        lines,
        picks,
        facets,
        rules,
        cat,
        link,
    };
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
        Got::TextsDir(dir) => {
            let change = scour_settings::Change {
                texts_dir: Some(dir),
                ..Default::default()
            };
            link.send(Ask::Remember { change });
            link.send(Ask::Texts);
        }
        Got::Down(why) => {
            let mut s = state.borrow_mut();
            s.down = true;
            s.search_out = false;
            s.res.lost();
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
        got @ Got::Search { .. } => answers::search(&cx, got),
        // The exact total the interactive search did not stop to compute: it lands
        // after the list, so the meter tightens from `1000+` to a number.
        got @ Got::Count { .. } => answers::count(&cx, got),
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
        got @ Got::Explain { .. } => answers::explain(&cx, got),
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
                link.send(Ask::Peek {
                    path: waiting,
                    at: None,
                    line: None,
                });
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
                Response::Flow(flow) => flowing::show(w, &path, flow, cat.language()),
                Response::Stat(entry) => peek_facts(w, cat, &entry),
                _ => {}
            }
        }
        Got::PeekDir { path, slot, reply } => {
            let mut s = state.borrow_mut();
            if path != s.peek_path {
                return;
            }
            peeking::dir_answer(w, cat, &mut s.peek_dir, slot, *reply);
        }
        // **Being the same size is not being the same file**: only `content` —
        // read end to end and compared — licenses the word "duplicate".
        got @ Got::Dupes { .. } => answers::dupes(&cx, got),
        // The list, or an answer taken: then the list again, which says what
        // the answer did.
        Got::Volumes(reply) => match *reply {
            Response::Volumes { disks, folders } => {
                state.borrow_mut().asked_disk = disks::draw(w, cat, &disks);
                state.borrow_mut().qb.disks = disks.clone();
                // The chip, and a place group's disks in the bar.
                qwire::draw(w, &mut state.borrow_mut(), link, cat, qwire::Lensing::Keep);
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
            Response::Sources { sources } => {
                state.borrow_mut().qb.sources = sources;
                qwire::draw(w, &mut state.borrow_mut(), link, cat, qwire::Lensing::Keep);
            }
            _ => link.send(Ask::Volumes),
        },
        // The exclusion rules, in the service's three groups. Only what a window
        // added can be deleted; any of them can be switched off.
        got @ Got::Rules { .. } => answers::rules(&cx, got),
        // The index moved. The search goes out *before* the next wait, so a burst
        // of changes does not queue a search per change.
        got @ Got::Awake { .. } => answers::awake(&cx, got),
        // What the service is holding: 636 MB over three sources is a different
        // claim from the same over one.
        got @ Got::Status { .. } => answers::status(&cx, got),
        // This desktop's own folders, each an `under:` term. The labels are the
        // desktop's own words, so nothing here translates them.
        got @ Got::Places { .. } => answers::places(&cx, got),
        got @ Got::Facets { .. } => answers::facets(&cx, got),
        Got::Resources(reply) => wiring::resources::landed(&cx, reply),
        Got::Pressed { press, outcome } => wiring::resources::pressed(&cx, press, outcome),
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
