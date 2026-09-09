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
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use link::{Ask, Got, Half, Link, ReplyRevision};

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
pub fn trace(what: &str) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var("SCOUR_TRACE").is_ok()) {
        eprintln!("gui: {what}");
    }
}

/// One key by name, for the synthetic keyboard.
///
/// Anything that is not a name is the text itself, so `a` is the letter and
/// `Return` is the key — which is exactly how Slint's own key events are
/// spelled: a named key is a character in the private use area.
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

/// How long after the last touch this window still counts itself as watched.
///
/// **A window nobody is using should not make the service work.** Measured
/// here: with a face attached the service commits every second instead of
/// every fifteen — 10.7% of a core and 103 MB a minute, against 0.4% and
/// 20 MB with nothing attached. Twenty-six times the CPU to keep a list fresh
/// that nobody is reading, which is exactly a window left behind a browser
/// while a build runs.
///
/// So being *open* is not the signal; being *used* is. A key, a click, a
/// scroll, a menu, a drag restarts this clock and the list is live again in
/// the same frame. After it runs out the window dozes — see [`DOZE_AGAIN`].
///
/// A minute: longer than any pause in reading a list, shorter than any time a
/// window sits genuinely forgotten.
const AWAKE_FOR: std::time::Duration = std::time::Duration::from_secs(60);

/// How often a dozing window asks anyway.
///
/// Not never: a window brought back to the front has to be right, and the
/// cheapest way to be right is to have been roughly right all along.
const DOZE_AGAIN: std::time::Duration = std::time::Duration::from_secs(10);

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
    /// Where the volumes are and whether they record reads.
    ///
    /// **For one column.** `Accessed` on a `noatime` volume is the file's
    /// creation time wearing a different heading, so the column says nothing
    /// there rather than saying something false. The list arrives with the
    /// desktop's folders and is kept for the same reason they are: it does not
    /// change while the window is open.
    mounts: Vec<scour_places::Mount>,
    /// When somebody last did something here. See [`AWAKE_FOR`].
    stirred: std::time::Instant,
    /// True while the window has stopped following the index closely.
    dozing: bool,
    generation: u64,
    /// How many entries the index holds, as of the last status.
    ///
    /// **The right-hand number in the meter.** It is the same sentence the
    /// page draws — how many things this query found, out of how many there
    /// are — and it was `held / matches` here, which is a different question
    /// wearing the same clothes: two hundred is the page size, not an answer.
    indexed: u64,
    /// Changes only when the matching set changes, not when its order does.
    query_revision: u64,
    /// What was searched before, newest first.
    ///
    /// Kept here as well as in the settings file because the list is narrowed
    /// by what is typed on every keystroke, and reading a file to answer a
    /// keystroke is not a thing to do.
    past: Vec<String>,

    /// The coloured runs the engine last sent, as it sent them.
    ///
    /// **Kept, because a keystroke has to redraw the colours before the
    /// answer to it arrives.** They are offsets rather than text, so they can
    /// be laid over whatever is in the box now — which is the only way the
    /// layer cannot show a character that has been deleted. See [`painted`].
    spans: Vec<scour_core::Span>,
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
    /// When a page last finished arriving.
    ///
    /// Read to decide whether there is time to re-read a page the index has
    /// moved under. During a scan it moves several times a second, and a list
    /// that re-read the page under the pointer every time would spend a drag
    /// fetching the same rows.
    page_landed_at: Option<std::time::Instant>,
    /// What the last page cost including the round trip, in microseconds.
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
    /// The skip rules this window added, as the service last reported them.
    ///
    /// Deleting one means sending the list without it, so the list has to be
    /// the service's rather than this window's guess at it.
    added_paths: Vec<String>,
    added_dirs: Vec<String>,
    added_files: Vec<String>,
    /// The switched-off list this window last sent, while the service has yet
    /// to say it back.
    ///
    /// **A late answer must not undo a fresh press.** `Rules` answers arrive
    /// coalesced and out of order — seven presses were answered twice, and
    /// each answer carried a list two changes old — so an answer that
    /// disagrees with what was just sent is an answer about the past, and the
    /// tick it would draw is the tick somebody just cleared.
    sent_off: Option<Vec<String>>,
    /// What the three rule lists were last built from.
    ///
    /// **A model that is replaced takes its rows with it**, and a row that is
    /// destroyed between a press and the release is a row whose `clicked`
    /// never fires: the release finds a different element, which was never
    /// pressed. The service answers `Rules` whenever it is asked — opening
    /// the panel asks — so an answer that says nothing new must change
    /// nothing, or every such answer is a press thrown away.
    rules_shown: [String; 3],
    /// The skip rules that are switched off, as the service last reported
    /// them.
    ///
    /// **Kept from the answer, not from what this window has pressed.** It was
    /// a list that started empty every time the window opened, so the first
    /// rule anybody switched off sent a list of exactly one — and the service
    /// takes that list as the whole truth. Everything switched off in the
    /// browser page, or in this window yesterday, came back on.
    exclude_off: Vec<String>,
    /// The folder the report is weighing. Empty is everything indexed.
    scope: String,
    hits: Vec<scour_core::Hit>,
    down: bool,
    /// The index revision this window has already seen. The long poll waits
    /// for anything past it.
    revision: u64,
    /// A batch of pictures is out and has not been answered.
    ///
    /// **One at a time, on the whole window.** A batch is several processes
    /// decoding video; sending the next one before the last is answered is
    /// how a scroll turns into a queue of work for rows nobody is looking at
    /// any more. The page holds the same flag for the same reason.
    asking_pictures: bool,
    /// The row the preview panel is about, as a full path.
    ///
    /// **What the panel is showing, not what is selected.** A reply that
    /// arrives for a row the arrows have already left is dropped by comparing
    /// against this; without it, running down a list leaves the panel showing
    /// whichever answer happened to come back last.
    peek_path: String,
    /// Files the service has already been asked about, oldest first.
    ///
    /// **This has to outlive the rows, which is why it is not on them.** The
    /// list is live: while the index is being scanned a page is refetched
    /// several times a second, and every refetch is a fresh row that has
    /// never been looked at. Without this, a file nothing can draw is asked
    /// about again on every refresh — measured at eleven thumbnailers started
    /// a second time for the same three pictures.
    ///
    /// Bounded and oldest-out, because it is a window over a result that can
    /// be millions of rows and an unbounded set of paths is a leak with a
    /// respectable name.
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

/// A catalogue string without the page's markup.
///
/// Every visible string is shared with the browser page, and some of them
/// carry `<code>` or `<b>` because the page has a stylesheet to hang on them.
/// This window has none, and was showing the tags.
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

/// The size floors the duplicate hunt offers, and their words.
///
/// A unique size eliminates only 6.2% of files, so a floor is what makes the
/// question answerable: candidates over a megabyte are eighteen thousand of
/// them holding 141.8 GB, measured on the live index.
const FLOORS: [(u64, &str); 4] = [
    (1 << 20, "1 MB+"),
    (10 << 20, "10 MB+"),
    (100 << 20, "100 MB+"),
    (1 << 30, "1 GB+"),
];

/// Ask for the duplicates under the report's scope, at the chosen floor.
///
/// By size alone: reading is what turns a candidate into a duplicate and it
/// reads whole files off a disk, so it is a second press rather than something
/// that happens because somebody opened a panel.
fn hunt(w: &MainWindow, link: &Rc<Link>, state: &Rc<RefCell<State>>) {
    link.send(Ask::Dupes {
        under: state.borrow().scope.clone(),
        min_size: FLOORS[w.get_dupe_floor().clamp(0, 3) as usize].0,
        read_budget: 0,
    });
}

/// The six age bands, in the order the colours run./// The six age bands, in the order the colours run.
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
    // **The one number no disk-usage tool shows**, and the one that decides
    // what to delete: how much of this weight nothing has touched in a year.
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
    // Said when the list is cut, because a list that silently stops at
    // twenty-four reads as a folder with twenty-four children.
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

/// The engine's own numbers, in three pieces.
///
/// Three claims of different weight and they are drawn differently — see the
/// meter row in `main.slint`. Passing them as one string is what made the
/// window's line grey where the page's is coloured.
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

/// What the selection bar says, in the catalogue's words and the reader's
/// order.
///
/// Folders are counted, never weighed. What a folder holds is the `~` number
/// in the size column — the part of it this index has — and adding that into a
/// total beside exact file sizes would make one number out of two different
/// kinds of claim.
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
    // Split where the number goes rather than glued to the front of the
    // sentence: a language does not have to put it first, and this one does
    // not always want the word order English does.
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

/// Write the next panic to a file, as well as to standard error.
///
/// Costs nothing until something panics. `force_capture` is deliberate:
/// backtraces are off unless `RUST_BACKTRACE` is set, and the person who hits
/// this crash has no reason to have set it.
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

// What the window takes on the command line.
//
// **Ordinary comments, not doc comments.** Clap reads a doc comment as the
// text it shows the reader, so the reasoning below would have been printed by
// `--help` — which is exactly the kind of thing that must not leak out of the
// source.
//
// It took no arguments at all, which is worse than it sounds: `scour-gui
// --version` opened a window. That is the first thing anybody types after
// unpacking a program, and answering it with a search window is the wrong
// first impression twice over — on a machine with no display it printed a
// failure to create a window, which reads as a broken program rather than a
// misread flag.
//
// The two options are the terminal face's, spelled the same way, because a
// person who has learned one face's flags has learned the others'.
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
    // **Before anything else opens.** `--version` and `--help` have to answer
    // and leave; clap does that itself, and it has to happen before a socket
    // is dialled or a window is asked for.
    let args = <Args as clap::Parser>::parse();
    // From the process starting to the first row on screen. The one number a
    // person sees before they have typed anything, and the only one the
    // window's own start-up appears in.
    let launched = std::time::Instant::now();
    // One read serves both the language and the socket. This used to load and
    // parse the same config file twice before the first request left.
    let config = scour_config::Config::load_or_default().0;
    // The shape the window was left in, and everything else chosen from
    // inside a face. **Read before the catalogue** because the language is one
    // of them: it used to be loaded three hundred lines further down, after
    // the words had already been chosen without it.
    // **What a crash leaves behind.**
    //
    // A panic prints to standard error, and a window opened from a desktop
    // entry has nowhere for standard error to go: the process disappears and
    // the person watching gets a window that closes itself. That is the whole
    // of what was known about the one crash this window has — a report with
    // no evidence attached, and no way to ask for any without asking somebody
    // to launch it from a terminal and wait for it to happen again.
    //
    // So it writes the panic down. Message, location, backtrace, and the two
    // pieces of outside state that change which drawing code runs at all: the
    // renderer and the display server. Appended rather than replaced, because
    // the interesting case is a crash that happens now and then and the
    // question is what the times have in common.
    crash_log(&config.state_dir());

    let kept = scour_settings::Settings::load(&config.state_dir());
    // **The words, and they can be changed while the window is open.** Held
    // behind a cell so that picking a language rebuilds the catalogue and
    // rewrites every visible string — it used to only remember the choice,
    // and the window went on speaking the old language until it was restarted.
    let cat: Rc<RefCell<Rc<Catalogue>>> = Rc::new(RefCell::new(Rc::new(Catalogue::for_language(
        &language(&kept, &config),
    ))));
    let window = MainWindow::new().context("the window could not be created")?;
    dress(&window);
    words(&window, &cat.borrow());
    // Nothing is marked to begin with: the first list is by relevance, which
    // is not a column and has no heading to point at.
    window.set_sorted_by("relevance".into());
    // The chevron appears only when there is something behind it.
    window.set_has_past(!kept.history.is_empty());
    trace(&format!("history: {} past searches", kept.history.len()));
    trace(&format!("window built {:.1?} in", launched.elapsed()));
    {
        // What the window believes the display does to it, asked once it is on
        // screen. A window whose scale is not the compositor's puts every
        // pointer a percentage away from where it was aimed — nothing at the
        // top of the panel, a whole row down at the bottom.
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
    // **The three rule lists are made once and never replaced.** Handing the
    // window a new model destroys every row in it, and a row destroyed between
    // a press and the release is a press nobody receives: the release lands on
    // a row that was never pressed. The service answers `Rules` after every
    // change — so replacing the model on each answer took away the very press
    // that caused it, and then delivered it again to whatever moved into that
    // place, which switched a second rule nobody touched.
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
    // The page's own placeholder, so an empty window says the same thing in
    // both: what you can type, by example.
    window.set_hint(t(
        &cat.borrow(),
        "file name  ·  ext:pdf  ·  kind:image dm:7d  ·  size:>10mb",
    ));
    said(&window, t(&cat.borrow(), "connecting…"));
    // The two languages the catalogue has. `""` is "whatever the desktop
    // says", which is what the config file means by an empty string.
    window.set_language(language(&kept, &config).as_str().into());
    // The widths somebody dragged, in either window: they are keyed by column
    // id and kept beside the index, so a column widened in the browser opens
    // that wide here.
    //
    // **Kept whole, not just the five this window draws.** `Change::widths`
    // replaces the map rather than merging into it — a change is the new
    // answer, which is the only way a width can ever be *removed*. So a save
    // has to send every width there is, and this window can see five of the
    // twelve columns: sending only those would delete a width the browser page
    // had set on `ext` or `perm`, silently, the first time anybody dragged
    // anything here.
    let table = Rc::new(RefCell::new(Table::read(&kept)));
    // And share the row out once with them, so the first frame is already the
    // right shape rather than the defaults for the instant before a resize.
    set_heads(&window, &table.borrow(), &cat.borrow());
    relayout(&window, &table.borrow());
    // The panel somebody left open. Read here with the rest of the shape,
    // before the first search, so it is open in the first frame rather than
    // appearing a moment later.
    window.set_peeking(kept.preview);
    let kept_layout = kept.layout;
    if matches!(kept_layout.as_str(), "icons" | "large") {
        window.set_view_mode(kept_layout.as_str().into());
    }
    let addr = match &args.socket {
        Some(given) => given.clone(),
        None => config.socket(),
    };

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
        // Registered after the link exists, because answering a search now
        // asks one more question — the facet count that goes with it.
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
            // **A query has a length, and the reason is the renderer.**
            //
            // Slint's software renderer places every glyph at an `i16`
            // coordinate, so a line of text wider than 32 767 physical pixels
            // is not a line that scrolls — it is a panic with no error path.
            // The field is 20 px mono, about twelve pixels a character, so
            // six hundred characters is some seven thousand pixels: still
            // inside the ceiling at four times scale, which is past any
            // display setting a person is likely to have.
            //
            // Nobody types six hundred characters into a search box. This is
            // here for the paste — a page of paths dropped into the field is
            // the one way it happens, and before this it closed the window.
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
            // **The colours move with the letters, not with the answer.**
            // Laid over the text as it is now, from the spans in hand: what
            // was deleted is gone this frame rather than a round trip later,
            // and what was typed is drawn plainly until the service says what
            // it is. See [`painted`].
            if let Some(w) = weak.upgrade() {
                let s = state.borrow();
                let runs = painted(&full_query(&s), &s.spans);
                w.set_spans(ModelRc::new(VecModel::from(runs)));
                // The list narrows as the query does, while it is open — the
                // chevron is worth pressing in the middle of typing, not only
                // on an empty box.
                if w.get_past_open() {
                    let lines = past_matching(&s.past, &s.query);
                    w.set_past_open(!lines.is_empty());
                    w.set_past_picked(0);
                    w.set_past(ModelRc::new(VecModel::from(lines)));
                }
            }
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
            stir(&state, &link);
            let token = token.to_string();
            {
                let mut s = state.borrow_mut();
                // Clicking the active one clears it — the shared rule, so
                // that pressing a filter means the same thing in all three.
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
                    export(&w, &addr, &cat_for_tools.borrow());
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

    // The preview panel, and it is remembered: a panel somebody pinned open is
    // a decision about how they work, not a gesture they repeat every morning.
    // The browser page reads the same setting.
    {
        let link = Rc::clone(&link);
        let state = Rc::clone(&state);
        let weak = window.as_weak();
        window.on_peek_toggled(move || {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            let on = !w.get_peeking();
            w.set_peeking(on);
            // So the next tick asks about whatever is selected rather than
            // finding the path it already had and doing nothing.
            state.borrow_mut().peek_path.clear();
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    preview: Some(on),
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
        let table = Rc::clone(&table);
        window.on_column_dragged(move |which, delta| {
            let Some(w) = weak.upgrade() else { return };
            // **The floor is the column's own, not one number for all five.**
            // It was 48 pixels flat, which is under every heading's own word
            // and well under what a date needs: a column dragged to it was a
            // column whose contents were gone. `scour-ui` says how narrow each
            // one may be, and `lay_out` will not go under it either.
            let floor = scour_ui::column(&which).map_or(48.0, |c| c.min as f32);
            // Where it is now, read off the rendered widths by position —
            // the one place that has to know which column is which, and the
            // list it asks is the same one the header was drawn from.
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
            // **And then the whole row again, not just this column.** Widening
            // one column narrows the others, and before this the others were
            // simply not told: the total ran past the edge and whatever was
            // out there stopped being on screen.
            // **What is kept is what was asked for, not what it came out as.**
            // A drag that lands during a squeeze renders narrower than the
            // pointer went; saving the rendered width would walk the column in
            // a little every time the preview panel was opened and it was
            // dragged again.
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

    // **Double-click an edge and that column decides for itself again.**
    //
    // Without this there was no way back: a width dragged here was kept for
    // good, and a column pinned at what a narrower window wanted stayed
    // pinned — it stopped stretching with the window, and nothing on screen
    // said why or offered to undo it. The browser page has had this gesture
    // all along; the window had the same grip and only half of what it does.
    {
        let weak = window.as_weak();
        let link = Rc::clone(&link);
        let table = Rc::clone(&table);
        window.on_column_reset(move |which| {
            let Some(w) = weak.upgrade() else { return };
            // Removing it is what "nobody has touched it" means, here and in
            // the settings file.
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

    // **Which columns, from the ⋮ at the end of the header row.**
    //
    // The same overlay the right-click menu uses, holding switches instead of
    // actions: one menu to place, dismiss and keep on screen rather than two
    // that would drift apart. `menu-columns` is what says which it is holding.
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
                        // **The last one standing cannot be turned off.** A
                        // table of no columns is not a smaller table, it is a
                        // broken one — and the page refuses the same press for
                        // the same reason.
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
            // Under the ⋮ itself. The overlay keeps itself on screen from
            // there — see `menu-x` — and the menu is wider than the button,
            // so it hangs to the left of it rather than off the edge.
            w.set_menu_x(x);
            w.set_menu_y(y);
            w.set_menu_columns(true);
            w.set_menu_open(true);
        });
    }

    // One switch pressed. The table changes, the rows are rebuilt because
    // their cells are the columns, and the choice is saved — in that order,
    // because the rows are built from the table.
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
            // **And the rows again, because a row is its cells.** Nothing on
            // screen holds the value of a column that was not being shown, so
            // the page is asked for afresh rather than repainted. Re-running
            // the query is what does that, and it is a millisecond.
            let query = w.get_query();
            w.invoke_query_changed(query);
        });
    }

    // The room changed — the window was resized, or the preview panel came
    // and went. Slint reports it; the widths are worked out in Rust.
    {
        let weak = window.as_weak();
        let table = Rc::clone(&table);
        window.on_relayout(move |_| {
            if let Some(w) = weak.upgrade() {
                relayout(&w, &table.borrow());
            }
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
            // **The tick moves on the press.** Waiting for the answer means
            // waiting for a round trip that may be coalesced away, and a tick
            // that does not move is a tick somebody presses again.
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
            // Ask again rather than guessing what the service made of it: the
            // reply is the truth about what is in force.
            link.send(Ask::Rules);
            let _ = w;
        });
    }

    // **A rule is added as a name whatever it looks like**, because that is the
    // rule people mean: matched wherever it appears. One that starts with a
    // slash is a path instead, and telling them apart by anything subtler
    // would be a rule nobody was told about.
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

    // A language is a restart of the words, not of the window: the catalogue
    // is rebuilt, every visible string is written again, and the choice is
    // kept by the service so the browser page opens in the same language.
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
            // **The words change now, not on the next start.** The catalogue
            // is rebuilt and every visible string written again; what comes
            // from an answer — the rail's kinds, the meter, the report — is
            // re-asked for, because those are worded where they arrive.
            *held.borrow_mut() = Rc::new(Catalogue::for_language(&tag));
            words(&w, &held.borrow());
            // The headings are words as well, and which of them there are is
            // the table's answer rather than the catalogue's.
            set_heads(&w, &table.borrow(), &held.borrow());
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    language: Some(tag.to_string()),
                    ..Default::default()
                },
            });
            // **A new question, so that the sidebar is asked again too.**
            // The facet walk is done once per matching set; without this the
            // rail kept the words it was filled with and the kinds beside the
            // counts stayed in the language nobody chose.
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

    // --- the report -------------------------------------------------------
    //
    // Weighed when it is looked at and again whenever a folder in it is
    // pressed. Nothing is weighed while the search tab is showing: it is a
    // walk of everything under the scope, and a tab nobody has opened should
    // not be spending it.
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
            // The words the table is headed with, said once here rather than
            // in the interface — the catalogue is the service's vocabulary and
            // the window does not keep a second one.
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

    // From the report into the search: the scope becomes an `under:` term and
    // the search tab opens with it. One query rather than a second kind of
    // scope the search would have to know about.
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

    // The duplicate hunt: not run unasked, and remembered once it is.
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
            // **Reading is the only thing that turns a candidate into a
            // duplicate**, and it is asked for rather than assumed: it reads
            // whole files off a disk.
            link.send(Ask::Dupes {
                under: state.borrow().scope.clone(),
                min_size: FLOORS[w.get_dupe_floor().clamp(0, 3) as usize].0,
                read_budget: 8 << 30,
            });
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
        let cat = Rc::clone(&cat);
        let asking = Rc::new(std::cell::Cell::new(false));
        let weak = window.as_weak();
        window.on_pick_opened(move || {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
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
        let state = Rc::clone(&state);
        let link = Rc::clone(&link);
        let weak = window.as_weak();
        window.on_activated(move |i| {
            stir(&state, &link);
            // **Opening a result is committing to the query too.** A person
            // who typed, looked and pressed the row meant that search as
            // much as one who pressed Enter — and the browser page has
            // counted both from the start.
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
            // Put it in the box and search it, the way typing it would.
            // **Set, then tell.** The text is two-way bound, and writing it
            // from here does *not* fire `edited` — that is for a person's
            // keystrokes — so without the second line the box changed and
            // nothing was searched.
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

    // --- the right-click menu ---------------------------------------------
    //
    // Two callbacks and no vocabulary: what the menu says and which items a row
    // gets is `scour_ui::menu`'s, and the window only draws and reports back.
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
                    // The count goes in through the same `{n}` the catalogue
                    // uses, so a language that puts the number somewhere else
                    // still gets it there.
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

    // What a pending question is about, while it is being asked.
    //
    // **The paths are taken when the menu is pressed, not when the answer
    // comes back.** In between, an index that is being watched can move a row
    // out from under the selection — and a question about twelve rows that
    // deletes whichever twelve are there when it is answered is a different
    // program from the one the reader agreed to.
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
            // One row or the whole selection, in the order they were picked.
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
                        // No helper on this machine: the path still goes
                        // somewhere a shell can take it, which is what this
                        // window did before it had a clipboard at all.
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
                    // **The menu becomes the list rather than growing a
                    // submenu.** A submenu is a second thing to learn how to
                    // leave, and this one closes exactly the way the menu it
                    // replaced does. The ids carry a prefix so the press comes
                    // back somewhere that knows what it is looking at.
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

                // The two that ask first. Everything above happens on the
                // press; these two put the question up and wait.
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
                    // Eight names and then a line saying how many are left.
                    // A list that runs off the bottom of the sheet is a list
                    // nobody read before pressing yes.
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
                        // Both ends: the old path is gone and the new one has
                        // appeared, and the index has heard of neither.
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
            // The move is this process's, with this user's permissions. The
            // service is only told to look again — see `Request::Recheck`.
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
            // Ask the current query again, so the rows go now.
            let query = state.borrow().query.clone();
            w.invoke_query_changed(query.into());
            let _ = (&link, &model);
        });
    }

    // Proof that the paragraph above works.
    //
    // A crash logger nobody has ever seen fire is a crash logger that does
    // not fire: the hook could be installed after the panic it was meant to
    // catch, the directory could be unwritable, the format string could be
    // wrong. `SCOUR_GUI_PANIC=1 scour-gui` stands on it deliberately, and the
    // file it leaves is the same file a real crash leaves.
    if std::env::var("SCOUR_GUI_PANIC").is_ok() {
        panic!("deliberate — proving the crash log writes");
    }

    // The field, filled in one go rather than typed.
    //
    // `SCOUR_SELFTEST` types on a 150 ms clock, which is the right shape for
    // measuring a keystroke and the wrong one for asking what a *long* line
    // does: four thousand characters would take ten minutes to arrive. This
    // sets the text once, and it exists because the software renderer casts
    // every glyph position to `i16` — a line wide enough to leave that range
    // is a crash, and this is how to stand on it deliberately.
    // The query a person asked for on the command line, before anything is
    // typed. The same flag the terminal face takes.
    if !args.query.is_empty() {
        window.set_query(args.query.clone().into());
        window.invoke_query_changed(args.query.clone().into());
    }

    if let Some(n) = std::env::var("SCOUR_GUI_LONGQUERY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        let q: String = "m".repeat(n);
        window.set_query(q.clone().into());
        window.invoke_query_changed(q.into());
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
            stir(&state, &link);
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
        let words = Rc::clone(&cat);
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
                pictures(&w, &state, &link, &rows, &lines);
                peek(&w, &state, &link, &rows, &words.borrow().clone());
            },
        );
    }

    // The scopes, once: they do not change while the window is open. On the
    // slow lane, because the fast one coalesces and would drop this the
    // instant a keystroke followed it.
    link.send(Ask::Places);
    link.send(Ask::Status);
    // **The rules, at the start and not only when the panel opens.** Which of
    // them are switched off is a list the service holds whole: a window that
    // has not been told cannot edit it without replacing it, and the first
    // rule anybody pressed used to turn every other one back on.
    link.send(Ask::Rules);
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

    // Open the report before the window does, for the same reason the panel
    // flag exists: a picture of the search tab says nothing about the other one.
    // Pick a language before the window opens, to see that the words really
    // change rather than only being remembered.
    // Press a window button before anybody does — `export` writes a file, so
    // this is how "does the button work" is answered without a hand on it.
    // Switch a skip rule off and on again — the round trip, so that "does the
    // button work" is answered without leaving anybody's rules changed.
    // Add a skip rule and take it away again, so that "does the field work"
    // is answered without anybody typing into it.
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
        // With the duplicate hunt open, when asked: it is the one part of the
        // report that is not run unless somebody presses for it.
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

    // The preview panel, for a picture of it. `SCOUR_GUI_PEEK=1` opens it
    // whatever the settings say; the panel is a mode, so there is no other way
    // to photograph it from outside.
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

    // The caret at the end of a long query, which is the only way a picture
    // can be taken of a scrolled field: nothing outside this window presses a
    // key, and text set from Rust leaves the caret at nought.
    if std::env::var_os("SCOUR_GUI_END").is_some() {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(900), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_caret_to_end();
            }
        });
    }

    // Press keys, without a keyboard. `SCOUR_GUI_KEY=ctrl+a` — chords
    // separated by commas, modifiers by `+`, and a bare word is one of
    // Slint's named keys. The compositor here will not send a key to a window
    // that a test started, and a shortcut is the one thing no other hook can
    // reach: `Ctrl+A` in the query box goes through the text input's own
    // handling, not through anything this program calls.
    window.set_trace(std::env::var("SCOUR_TRACE").is_ok());

    if let Ok(spec) = std::env::var("SCOUR_GUI_KEY") {
        let chords: Vec<String> = spec.split(',').map(|c| c.trim().to_string()).collect();
        let after = std::env::var("SCOUR_GUI_KEY_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(1000);
        let weak = window.as_weak();
        // **One chord a tick, not all of them in one callback.** Seven
        // backspaces pressed inside a single callback are seven changes and
        // one repaint — and a stale frame between two keystrokes is precisely
        // what this hook has to be able to catch.
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

    // The right-click menu, opened without a right hand.
    //
    // `SCOUR_GUI_MENU=3 scour-gui` puts it on the fourth row. A menu is drawn
    // by a pointer event and there is no way to synthesise one into a Slint
    // window from outside it, so without this the only way to look at the menu
    // is to open a window on somebody's screen and use their mouse.
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
        // The column picker is not a panel but it opens like one, and a
        // picture of it is the only way to check it from here.
        if which == "columns" {
            // Where the ⋮ is, roughly: nothing has been laid out yet at this
            // point, so a picture of the menu is a picture of the menu rather
            // than of where it opens.
            window.invoke_columns_clicked(1500.0, 150.0);
        }
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
    // Press and release at a point of the window, without a pointer. The
    // compositor here refuses to warp one, and a click that never leaves the
    // program is the only way to ask whether the window's own hit test agrees
    // with what it drew. Logical pixels, comma separated: `SCOUR_GUI_CLICK=900,146`.
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
                    // Released a moment later, because a press and a release in
                    // the same tick is not what a hand does and a `Flickable`
                    // is entitled to treat it differently.
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

    // Walk a pointer down the window without a hand, printing which row each
    // stop lands on: `SCOUR_GUI_HOVER=900:120,160,200`.
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
/// Keep the preview panel on whatever is selected.
///
/// **Asked once per row, not once per tick.** What the panel is showing is
/// remembered as a path, so arrowing down a list asks about each row it lands
/// on and running back up over one already drawn asks nothing at all.
///
/// The name and the path go up immediately — they are on the row, which is
/// already in hand — and the two answers fill the rest in when they arrive.
/// The alternative is a panel that goes blank between rows, which is what a
/// list feels like when it stutters.
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

/// Pictures for what is on screen: draw the ones the desktop has already made,
/// and ask for the ones it could make.
///
/// **After the drawing, on the tick, and never on the path a keystroke takes.**
/// Making a thumbnail is a separate process doing image decoding; nothing about
/// it may be in front of a paint, a scroll or a keystroke. What runs here is a
/// bounded number of `stat` calls and PNG decodes — see
/// [`rows::Rows::look_for_pictures`], which is where every bound is.
///
/// A little past the bottom of the window as well as what is in it: scrolling
/// a row at a time should not be a picture arriving a row at a time.
fn pictures(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    rows: &Rc<rows::Rows>,
    lines: &Rc<rows::Lines>,
) {
    // **Only where a picture is drawn.** The detail list shows the kind's
    // glyph and nothing else — the owner's call, and it makes this whole pass
    // work done for a thing nobody would see. A window in the detail view
    // costs exactly what it cost before pictures existed.
    if !w.get_grid() {
        return;
    }
    /// How many rows are looked at per tick. Ten ticks a second, so a
    /// screenful is filled inside a second even in the tile view.
    const LOOKED_AT: usize = 24;
    /// How far past the bottom of the window to look.
    const AHEAD: usize = 12;
    /// How many paths the "already asked" memory holds. The page's number,
    /// and for the page's reason: a few screenfuls of scrolling either way.
    const REMEMBERED: usize = 4096;
    let visible = w.get_visible_rows().max(0) as usize;
    let first = w.get_first_row().max(0) as usize;
    let (drawn, ask) = rows.look_for_pictures(first, first + visible + AHEAD, LOOKED_AT);
    if drawn > 0 {
        trace(&format!("{drawn} picture(s) drawn"));
        // The tiles hold the same rows, so the lines carrying them changed.
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

/// Refresh is optional work. Its rest starts after the reply, so a slow
/// response cannot spend the entire delay in flight and immediately repeat.
/// Missing pages and explicit new queries do not wait for this clock.
fn refresh_ready(elapsed: Option<std::time::Duration>, cost_us: u64) -> bool {
    let rest = SETTLED.max(std::time::Duration::from_micros(cost_us).saturating_mul(10));
    elapsed.is_none_or(|spent| spent >= rest)
}

/// Somebody just did something in this window.
///
/// Restarts the clock in [`AWAKE_FOR`] and, if the window had dozed off, asks
/// the service at once rather than waiting out [`DOZE_AGAIN`] — so the first
/// thing a person sees after touching it is current.
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
            // **Each half without the term it sets.** The rail offers kinds
            // and the ribbon offers ages, so counting them over the query as
            // typed makes `kind:code` a rail of one number and `dm:7d` a
            // ribbon of one bar — controls that can only confirm what is
            // already on the screen. The browser had this right from the
            // start; the window did not.
            //
            // Both come out of one walk unless the query names a kind or an
            // age, which is the uncommon case and the only one that pays for
            // a second.
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
                    // **And the count, which neither of those answers any
                    // more.** One walk over the query in force used to count
                    // it exactly on its way past; two walks over two wider
                    // queries count something else, so the meter fell back to
                    // the search's own ceiling and read `1.000+` over a result
                    // of thirteen thousand. This is the pass that exists for
                    // exactly that, asked here rather than after a reply that
                    // is no longer about the right rows.
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
/// What was searched before, narrowed by what is typed now.
///
/// **A hundred past queries is not a list anybody reads.** An empty box
/// offers everything, newest first; anything typed keeps the lines that
/// contain it, which is what makes the chevron useful mid-query rather than
/// only on an empty box. The browser page has done this from the start and
/// the rule is its own.
fn past_matching(past: &[String], typed: &str) -> Vec<slint::SharedString> {
    let typed = typed.trim().to_lowercase();
    past.iter()
        .filter(|line| typed.is_empty() || line.to_lowercase().contains(&typed))
        // **A dozen, and the cap is here rather than on the box.** The list
        // is drawn by a layout, and a layout gives its parent a minimum
        // height — a box told to be shorter than its rows is simply
        // overflowed by them, out of the card and over the results.
        .take(PAST_SHOWN)
        .map(|line| line.as_str().into())
        .collect()
}

/// How many past searches the box offers at once.
///
/// The service keeps a hundred (`scour_settings::HISTORY`); this is what fits
/// under the box without covering the answer it is meant to help you find.
/// Typing narrows the list, which is what reaches the older ones.
const PAST_SHOWN: usize = 12;

/// Put one query at the front of the history.
///
/// **What was committed to, not what was searched.** The box searches per
/// keystroke, so what went out is `r`, `ra`, `rap`; what belongs here is the
/// query somebody pressed Enter on. The cap is the service's — see
/// `scour_settings::HISTORY` — and this copy is the window keeping in step.
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

/// The coloured runs to draw, for the query that is **on screen now**.
///
/// **Sliced from the text in the box, not from the text the spans arrived
/// with.** The spans come back from the service a round trip after the
/// keystroke that caused them, and a reply for a query that has already been
/// superseded is dropped — so between one keystroke and the next answer, the
/// newest spans in hand describe a *different string* from the one being
/// typed. Drawing their own text meant that deleting a character left it on
/// screen until the answer came back, and typing fast left several.
///
/// So the spans are treated as what they are: offsets. Anything they do not
/// reach is drawn as ordinary text, anything past the end of the query is
/// dropped, and the layer therefore always spells exactly what the box holds.
/// `paintSpans` in `page.html` is the same function, and is why the browser
/// window has never had this.
fn painted(query: &str, spans: &[scour_core::Span]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len() + 2);
    // Characters, not bytes: the wire counts bytes and the window places each
    // run by counting characters. `değiştirme` is ten characters and twelve
    // bytes, so a run after it would sit two characters too far right.
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
        // An offset the query no longer reaches is a span from a longer
        // query. Clamping rather than skipping keeps the run before it whole.
        let start = boundary(query, sp.start as usize).max(at);
        let end = boundary(query, sp.start as usize + sp.len as usize).max(start);
        if start > at {
            add(&query[at..start], 6, false, &mut out, &mut chars);
        }
        add(
            &query[start..end],
            // **The page's table, role for role.** It was four arms short —
            // a comparison, an `or` and a quoted phrase all fell through to
            // the ordinary ink, and the punctuation between terms was drawn
            // as bright as the words. `;` reading as loudly as a word it is
            // not is how a disjunction went unnoticed.
            match sp.role {
                scour_core::Role::Field | scour_core::Role::Cmp | scour_core::Role::Or => 1,
                scour_core::Role::Value | scour_core::Role::Phrase => 2,
                scour_core::Role::Glob => 3,
                scour_core::Role::Not => 4,
                scour_core::Role::UnknownField | scour_core::Role::BadValue => 5,
                // **What is being looked for**, which is not the same as
                // "everything else": only the words a person typed to find
                // something take this colour.
                scour_core::Role::Text => 6,
                // The syntax between terms, said quietly.
                scour_core::Role::Colon
                | scour_core::Role::Sep
                | scour_core::Role::Quote
                | scour_core::Role::Space => 7,
                // **No catch-all**, deliberately. Every role is named, so a
                // role added to `scour_core` stops compiling here instead of
                // silently taking colour zero — which is the ordinary ink, and
                // therefore the one mistake nobody would notice.
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

fn full_query(s: &State) -> String {
    scour_ui::query::compose(&s.query, s.facet.as_deref())
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
            // The one string a cell needs that is not on the hit. Read once:
            // a page is two hundred rows and the catalogue is a lookup.
            let frozen_note = t(
                cat,
                "this volume does not record reads (noatime) — the number shown would be left over from when the file was created",
            );
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
            // **What was drawn, not how much of it.** A count says the list
            // agreed with the meter; it does not say the rows are the rows
            // the query asked for, and those are different failures with the
            // same symptom — a number that looks right over a list that is
            // not. The first row is enough to tell them apart.
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
            // **The page's sentence, in the page's order.** Found out of how
            // many there are, then what it cost, then how much of the index
            // was walked to get it. Grouped with the locale's own separator,
            // because a seven-digit number without one is a number nobody
            // reads.
            //
            // The left number was `n` — the rows in this page, which is the
            // page size and reads as an answer: two hundred, for a query
            // matching half a million. The comment above it already said
            // "shown out of total" and meant this; the variable did not.
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
                // **How many this query found, out of how many there are.**
                // The left side used to be `rows.held()` — the rows this
                // window happens to be holding, which is the page size and
                // reads as an answer. Two hundred, for a query matching half
                // a million. The page has always drawn the other sentence and
                // there is no reason for two faces to mean different things
                // by the same mark.
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
            // **What it read, in words, when the query has something to say.**
            // The colours answer "what is this piece"; this answers "so what
            // does the whole thing ask", which is the only place an `or`
            // hiding inside an exclusion is visible.
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
        // The exclusion rules, in the three groups the service keeps them in:
        // what a window added, what `config.toml` says, what is built in. Only
        // the first can be deleted; any of them can be switched off, and the
        // ones that are come back marked.
        // What a folder weighs. Asked for when the report tab is looked at
        // and whenever a folder in it is pressed.
        Got::Usage { path, reply } => {
            let Response::Usage(u) = *reply else { return };
            // A folder nobody is looking at any more: weighing is a walk, and
            // a slow answer for a scope that has been left is not an answer.
            if path != state.borrow().scope {
                return;
            }
            show_usage(w, cat, &path, &u);
        }
        // What kinds the weight under a folder is in — the same walk the
        // search rail uses, scoped to the report's folder instead of to a
        // query. Drawn as a share so that a bar answers before a number does.
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
            // The kind's word comes from the engine's own msgid, the same way
            // the rail's does — a window that spelled these itself would be a
            // second vocabulary.
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
        // The heaviest files under it. A page of eight, ordered by size, with
        // no count asked for: nothing here reads a total and counting is the
        // one piece of work proportional to how many match.
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
        // The same file, several times over. **Being the same size is not
        // being the same file**, and the word for a group says which of the
        // two it is — `content` means read end to end and compared, and
        // nothing else licenses the word "duplicate".
        // **The pictures the service managed to make**, marked so the tick
        // that follows loads them. `ran` is the number this design has to be
        // judged on — how many processes a screenful of unseen files actually
        // starts — and it is traced rather than hidden, exactly as the page
        // reports it.
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
        // The preview panel's two answers, both tagged with the path they are
        // about. Either may arrive for a row nobody is looking at any more —
        // the arrows move faster than a disk read — and then it is dropped.
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
                    // Six of them and a count, as the page has it: a group of
                    // twenty-eight copies is a fact about the group, not
                    // twenty-eight lines somebody has to scroll past to reach
                    // the next one.
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
            // The answer is the truth about what is in force, so this is
            // where the window learns it — pressing a rule edits *this* list.
            // An answer that repeats what was sent means the service has
            // caught up; one that does not is older than this window.
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
            // **What this window may delete, kept as the answer gave it.**
            // Deleting one means sending the list without it, so a window that
            // has not been told what is in the list cannot take one out of it
            // — the same trap the switched-off list was in.
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
            // One string per list, so an answer that repeats itself is
            // recognised before it costs anybody their press.
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
                // Same rules in the same order, one of them switched: write
                // the rows that differ and leave the rest of the list — and
                // every one of its elements — where it is.
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
            // **Free, and the reason this indicator is cheap.** A wait is
            // answered with the whole status, so every wake carries how far a
            // walk has got — and during one the index moves several times a
            // second, which is exactly the rate a person needs to believe
            // something is happening.
            w.set_scanning(scanning_note(cat, &st));
            // **A window nobody is using stops following.** Not "open" —
            // *used*: see [`AWAKE_FOR`]. Dozing takes the revision (it is not
            // amnesia, and the pages in hand are marked so the next look
            // re-reads them) but re-reads nothing now and asks again in ten
            // seconds rather than a quarter of one. `stir` undoes it within
            // the frame.
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
                s.indexed = st.entries;
                if s.revision == 0 {
                    s.revision = st.revision;
                    link.send(Ask::Await { since: st.revision });
                }
            }
            // **And whether searching is still as fast as it was built to
            // be.** Every query reads the unsorted tail, so a week of ordinary
            // use took ordering by path from 1.9 ms to 21.5 and one rebuild
            // put it back. The engine has always known when that is due; the
            // answer reached the command line and nowhere else, which is the
            // face this window's owner uses least. Appended rather than given
            // a place of its own: it is true of the index, like everything
            // else on this line, and it is absent almost all of the time.
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

            if half.kinds() {
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
            }

            if half.ages() {
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
            }
            // **Only when one walk answered both.** `f.total` is the size of
            // the set this reply counted, and a stripped query counts a wider
            // one — reading the meter off it would say `2.238.902` over a
            // result of eight thousand. The search reply draws the same
            // sentence and is about the query actually in force, so nothing
            // is lost by leaving it to that one here.
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
                // The facet walk counted the whole matching set on its way, so it
                // is the first thing that can tell the list how long it really is.
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

/// Tell the service to look at these paths again, now.
///
/// **A connection of its own, and that is deliberate.** The window's `Link`
/// has three lanes and a coalescing rule built for a search box; a request
/// that happens once a press, after something on disk has already changed,
/// does not belong in any of them. The export path opens its own socket for
/// the same reason.
///
/// Failure is silent because there is nothing useful to say: the file is
/// already in the wastebasket, and the row will go when a watcher gets to it.
fn recheck(addr: &str, paths: Vec<String>) {
    let addr = addr.to_owned();
    std::thread::spawn(move || {
        if let Ok(mut client) = scour_ipc::Client::connect(&addr) {
            let _ = client.call(scour_proto::Request::Recheck { paths });
        }
    });
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
/// Write every string the window shows, out of the words it is given.
///
/// **One place, and it runs more than once.** Picking a language rebuilds the
/// catalogue and calls this again; before it existed the choice was only
/// remembered and the window went on speaking the language it had opened in.
fn words(window: &MainWindow, cat: &Catalogue) {
    // Punctuation is part of the language, and this runs whenever the language
    // does — so a window switched to English starts saying `5,356,281`.
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
    // `Everything` was this window's own word for the same thing.
    window.set_scope_label(t(cat, "Kind"));
    window.set_scope_heading(t(cat, "Scope"));
    window.set_size_heading(t(cat, "Size"));
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
    // **The sentence says what it opens, before it opens it.** A listening
    // port is not what somebody asked for when they asked for a browser, and
    // a person who would rather not have one has to be told in time to say no.
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
    // The help is the page's own legend, in the page's order — the same six
    // sections, each a heading and a paragraph — rather than a second
    // explanation written for this window. **Stripped of the markup they
    // carry**: the catalogue is shared with a page that hangs a stylesheet on
    // `<code>` and `<b>`, and this window has none, so it was showing tags.
    // **Help is the two things clicking cannot show you.**
    //
    // This was eleven paragraphs on why the freshness ruler exists and what
    // the report proves — true, and not what somebody wants at the moment
    // they press `?`. Cut to the query language it went too far the other
    // way: a panel the size of a tooltip, and still missing the half a person
    // is most likely to come looking for. Every shortcut in this window was
    // undocumented — they live in `scour_ui::menu`, printed beside the items
    // in the right-click menu, and nowhere a person browses.
    //
    // So: the terms, then the keys. Both are things the interface cannot
    // teach by being clicked, and neither is an argument for the design.
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

/// A monospace face this machine actually has.
///
/// **"monospace" is not a family, and Slint does not treat it as one.** The
/// query field asked for `font-family: "monospace"` and got the ordinary
/// interface sans: Slint hands the string to parley as
/// `FontFamilyName::named(...)` — a family *called* "monospace", which no
/// system has — and when that misses it falls through to
/// `FALLBACK_FAMILIES`, which is sans-serif. The generic a browser
/// understands is not a generic here.
///
/// Measured, because a proportional face in this field is not a matter of
/// taste: `M` came out 18.86 px wide and `i` 5.98 at the same size. The
/// coloured layer and the box that takes the typing are two runs of the same
/// string drawn one on top of the other, and the only thing that keeps them
/// there is a face where a character's width does not depend on which
/// character it is.
///
/// **The desktop's own answer, not a favourite of ours.** Every terminal on
/// this machine already resolves `monospace` through fontconfig; picking
/// something else would make this one field disagree with all of them.
/// `SCOUR_GUI_MONO=<family>` names one by hand, which is how the widths above
/// were measured.
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
        // No fontconfig: the old string, which draws in the sans and is at
        // least legible.
        asked.unwrap_or_else(|| "monospace".to_owned())
    }
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
    // The report has a size heading of its own, which is not the table's.
    let head = |id: &str| {
        scour_ui::column(id)
            .map(|c| t(cat, c.msgid))
            .unwrap_or_default()
    };
    window.set_head_size(head("size"));
}

/// The columns this window shows when nobody has said otherwise.
///
/// **`scour-ui`'s, so that this window opens the way it always has.** The
/// browser page's own default is the same five in a different order, which is
/// a disagreement older than this and not one to settle by quietly rearranging
/// somebody's table.
const COLUMN_DEFAULT: &[&str] = scour_ui::DEFAULT_COLUMNS;

/// Which columns are shown, how wide, and what a person dragged them to.
///
/// **One place, because the three answers depend on each other.** Widths are
/// shared out among the columns that are showing; showing one more takes room
/// from the rest; and what somebody dragged has to survive a column being
/// hidden and shown again — which it does here, because the map is kept whole
/// rather than trimmed to what is on screen.
struct Table {
    shown: Vec<&'static scour_ui::Column>,
    /// What each column was dragged to, by id, for **every** column and not
    /// only the ones showing. `Change::widths` replaces the map rather than
    /// merging into it, so a save has to send every width there is: sending
    /// only the visible ones would delete what the browser page had set on a
    /// column this window happens to be hiding.
    widths: std::collections::BTreeMap<String, u32>,
}

impl Table {
    /// What was saved, or the default where it says nothing usable.
    ///
    /// **Every unknown id dropped rather than the list refused.** A setting
    /// written by a newer version, or by hand, names a column this build does
    /// not have; showing the rest is what somebody meant, and an empty list is
    /// the one answer that cannot be right — a table of no columns is not a
    /// smaller table.
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

    /// Switch one column on or off.
    ///
    /// **Put back where its neighbours expect it, without moving the others.**
    /// Rebuilding the list in table order would throw away an arrangement
    /// somebody had made, every time they showed one more column — so a
    /// column arrives in front of the first shown column that comes after it
    /// in `scour_ui::COLUMNS`, and at the end when there is none.
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

/// Divide the row up among the columns again and hand each one its width.
///
/// **Called whenever the room changes, which is more things than it sounds
/// like**: the window resized, the preview panel opened or closed, an edge
/// dragged. The arithmetic is `scour_ui::lay_out` — shared with the browser
/// page, so a table that fits in one fits in the other — and its promise is
/// that the widths add up to the room, which is what keeps the date and the
/// size on screen instead of drawn past the right-hand edge.
fn relayout(window: &MainWindow, table: &Table) {
    let ids = table.ids();
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    let widths = scour_ui::lay_out(
        &ids,
        // What somebody dragged this column to, or nothing. Zero is "nobody
        // has touched it", the same as in the settings file.
        |id| table.widths.get(id).copied().filter(|v| *v > 0),
        window.get_lane().max(0.0) as u32,
    );
    if widths.len() != ids.len() {
        return;
    }
    let px: Vec<f32> = widths.iter().map(|w| *w as f32).collect();
    window.set_cw(ModelRc::new(VecModel::from(px)));
}

/// The headings, in the order they are shown.
///
/// Redone whenever the columns change or the language does — both are what a
/// heading *is*, and neither happens while anybody is reading one.
fn set_heads(window: &MainWindow, table: &Table, cat: &Catalogue) {
    window.set_heads(ModelRc::new(VecModel::from(heads_of(table, cat))));
}

/// The same, without a window to put them in — which is what makes the wiring
/// testable. A heading carrying the wrong sort key is a column that reorders
/// the list by something else: visible, but only if you knew what to expect.
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
/// The first `name` on the `PATH`, or beside this program.
///
/// **Beside this program first.** A build being tried out is run from its own
/// directory, and a switch that quietly starts the installed copy is a switch
/// that tests the wrong thing.
fn which(name: &str) -> Option<std::path::PathBuf> {
    if let Ok(here) = std::env::current_exe()
        && let Some(dir) = here.parent()
    {
        let beside = dir.join(name);
        if beside.is_file() {
            return Some(beside);
        }
        // A build being run out of `target/release` has the scripts two
        // directories up, which is where the launcher lives.
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

/// Start another way of running Scour, after somebody has said to.
///
/// Started detached, so closing this window does not take the other one with
/// it — and never waited for: a window that blocks on a browser is a window
/// that looks broken while the browser starts.
fn open_face(window: &MainWindow, which_one: &str, cat: &Catalogue, link: &Link) {
    // **The terminal is opened through the launcher, not directly.** A
    // terminal interface without a tty exits before anybody sees it, and which
    // terminal to start is a list of nine programs with nine different flags
    // — kept in `scripts/scour-open` so there is one of it rather than one per
    // face.
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
    // In a process group of its own, or the desktop closing this window closes
    // what it just opened. See `scour_ui::faces::detach`.
    scour_ui::faces::detach(&mut command);
    match command.spawn() {
        Ok(_) => {
            // **Switching is also choosing.** The desktop entry and the hotkey
            // ask for Scour without saying which face; this is what makes
            // that mean the one somebody switched to.
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    face: Some(which_one.to_string()),
                    ..Default::default()
                },
            });
            said(window, t(cat, "starting…"));
            // **And this one goes.** Switching is moving, not opening a
            // second one: two windows onto the same index, both live, both
            // answering the same keystrokes, is not what anybody meant by
            // "switch to the terminal".
            //
            // After a beat, not now: the preference above is a message on a
            // socket and the launcher has an `exec` to get through. Half a
            // second costs nothing and is the difference between a face that
            // starts and one that is killed while starting.
            slint::Timer::single_shot(std::time::Duration::from_millis(500), || {
                let _ = slint::quit_event_loop();
            });
        }
        Err(e) => said(window, format!("{program}: {e}").into()),
    }
    window.set_panel("".into());
    window.set_armed_face("".into());
}

fn export(window: &MainWindow, addr: &str, cat: &Catalogue) {
    let query = window.get_query().to_string();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // **Where a desktop puts what it downloads**, offered rather than
    // decided: the dialog opens there with a name already in it, and whoever
    // presses the button says where it really goes. It used to drop the file
    // in the home directory without asking and without saying, which is a
    // file somebody finds a week later.
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let into = scour_places::downloads().unwrap_or(home);
    let name = format!("scour-{stamp}.csv");
    let weak = window.as_weak();
    let addr = addr.to_string();
    let waiting = t(cat, "writing…");
    let wrote = t(cat, "written to");
    let failed = t(cat, "could not be written");

    std::thread::spawn(move || {
        // **Asked for on this thread**, not on the one drawing the window: the
        // portal takes as long as somebody takes to choose, and a window that
        // stops repainting while a dialog is open looks like a window that has
        // crashed.
        let Some(path) = rfd::FileDialog::new()
            .set_directory(&into)
            .set_file_name(&name)
            .add_filter("CSV", &["csv"])
            .save_file()
        else {
            // Cancelled. Nothing was written and nothing is said: the person
            // who closed the dialog knows what they did.
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
                // **Where it went, and it has to stay put.** The meter is
                // rewritten by every answer — several a second with a live
                // list — so the line saying where a file was written was gone
                // before anybody could read it.
                w.set_note(told.into());
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

/// Bytes, the way the meter says them: `636,4 MB`.
fn compact_bytes(n: u64) -> String {
    scour_ui::format::compact_bytes(n, marks().1)
}

/// How this window is punctuating numbers, set when the language is.
///
/// **A thread-local rather than an argument**, because every caller of these
/// two is drawing one string in the middle of a sentence and threading a mark
/// through all of them would say nothing a reader does not already know. The
/// window is one thread and the language changes in one place.
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

/// The language the window should speak: what was chosen, then what was
/// configured, then the desktop.
///
/// **The chosen one is read now, and used to be ignored.** `Settings::language`
/// is what every face's language menu writes, and the window passed `""` here —
/// so a language picked in this very window took effect at once and then was
/// gone at the next start, with the desktop's answer back in its place.
fn language(kept: &scour_settings::Settings, cfg: &scour_config::Config) -> String {
    scour_i18n::choose(&kept.language, &cfg.ui.language)
}

/// Draw what the service says can be shown of a file.
///
/// **A thumbnail rather than the file.** A preview of a forty-megapixel
/// photograph is a forty-megapixel decode on the drawing thread, for a panel
/// three hundred and eighty pixels wide; the desktop's thumbnail is the same
/// picture at the size actually being looked at, is already made for anything
/// that has ever been seen in a file manager, and is what the grid draws. When
/// there is none the service is asked to make one — through the same door as
/// the grid's, so the same four-at-a-time bound covers both — and this shows
/// what it can in the meantime.
/// As much of a file as a preview should show — and as much as can be drawn.
///
/// **This is a crash fix, and the crash is worth writing down.** The service
/// hands back a quarter of a megabyte of text, the panel put all of it into
/// one wrapping `Text`, and Slint's software renderer casts every glyph's
/// position to `i16`. A quarter megabyte of monospace wrapped into a column
/// three hundred pixels wide is about six thousand lines — near ninety
/// thousand pixels tall — so the cast failed and the window died, silently,
/// on the desktop. It took a long time to find because it depends on *which*
/// file is selected: anything over roughly a hundred kilobytes of text was a
/// crash, everything smaller was fine, and so it looked random.
///
/// The GPU renderer has no such ceiling, and is what runs now. This stays
/// because the software renderer is still the fallback on a machine with no
/// GL, and a fallback that crashes is not one.
///
/// The cap is in characters rather than lines because how many lines the text
/// becomes depends on the panel's width, which is the reader's to change and
/// not knowable here. The line cap is the one that usually bites first, and
/// both are far past what anybody reads to decide whether a file is the right
/// file.
///
/// **Divided by the scale factor, and that is not a detail.** The ceiling is
/// counted in *physical* pixels; the cap is counted in characters, which are
/// logical. A cap that clears the ceiling on a 1× screen clears two thirds of
/// it on a 1.5× one and none of it at 3× — which is a stock display setting,
/// not an exotic one — so a fixed cap is a fix that quietly stops working on
/// somebody else's monitor.
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

fn show_peek(
    w: &MainWindow,
    cat: &Catalogue,
    link: &Rc<Link>,
    path: &str,
    look: &scour_preview::Look,
) -> bool {
    w.set_peek_text(peek_head(&look.head, w.window().scale_factor()).into());
    /// The largest picture worth decoding on the drawing thread.
    ///
    /// Half a megabyte covers an icon, a screenshot of part of a screen, and
    /// every file in the thumbnail cache — which is a directory of pictures
    /// like any other and turns up in a search for one. A photograph is not in
    /// this class and does not need to be: the desktop already has a thumbnail
    /// of it, which is the same picture at the size being looked at.
    const SMALL: u64 = 512 * 1024;
    // **Whatever the desktop can draw, not only what a browser can.** `shape`
    // is the *browser's* question — it says whether an `<img>` or a `<video>`
    // would render the bytes — and the window has no browser in it. What it
    // has is the thumbnail cache, and the machines this runs on declare
    // thumbnailers for PDFs, video, EPUB and office documents as readily as
    // for photographs. Gating on `shape == "image"` meant a PDF said it could
    // not be previewed while `evince-thumbnailer` sat there able to draw its
    // first page.
    let made = scour_thumbs::cache::existing(path).or_else(|| {
        // Only a picture is opened directly: a PDF is not something an image
        // decoder can be pointed at, and a small one is not a small picture.
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
    // A picture nobody has made yet is not a picture that cannot be made.
    // Asked for through the same door the grid uses, so the same four-at-a-time
    // bound covers both, and drawn when it lands. `can_make` is the machine's
    // own table — one extension lookup and one MIME lookup, no syscall — so
    // this asks about a PDF exactly when something is installed that can draw
    // one.
    let coming = !w.get_peek_has_shot() && scour_thumbs::can_make(path);
    if coming {
        link.send(Ask::Thumbnails {
            files: vec![path.to_owned()],
        });
    }
    // Said only when there is nothing else in the box, and nothing on its way.
    // A file whose head is empty because the file is empty is not a failure
    // either — but it has nothing to show, and a blank panel says less than a
    // line does.
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
///
/// The values are formatted by `scour-ui` and the two that are not — a mode as
/// `drwxr-xr-x`, an id as a name — by `scour-core`. The terminal draws the same
/// eight lines from the same call; what differs between the two is only where
/// the row comes from.
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

/// `taranıyor 1.240.000` while the index is being walked, nothing otherwise.
///
/// The wording is `scour_ui::SCANNING`, so the terminal and the page say the
/// same thing — and the number is punctuated the way every other number in
/// this window is.
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
    /// The ceiling the software renderer draws under, and what it costs.
    ///
    /// The number that matters is not 20,000 — it is that whatever comes back
    /// from the service, what reaches the panel is bounded. A cap written as
    /// "take the first N bytes" would also pass a test like this and would
    /// still be wrong: it can split a character in half. So the long case
    /// checks the cut is a real string and that the reader is told it was cut.
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

        // One enormous line, no newline in it at all — the line cap cannot
        // help here and the character cap has to.
        let one: String = "x".repeat(300_000);
        assert!(super::peek_head(&one, 1.0).chars().count() <= 20_002);
        // Three times the scale, a third of the text: the ceiling is
        // counted in physical pixels and the cap has to follow it there.
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

    /// The coloured layer spells what is in the box, never what was.
    ///
    /// The spans arrive a round trip after the keystroke that caused them,
    /// and a reply for a superseded query is dropped — so the newest spans in
    /// hand routinely describe a longer string than the one being typed.
    /// Drawing their own text left the deleted characters on screen.
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

    /// **The doze is a clock, and the clock is the whole feature.** A window
    /// that dozed while somebody was using it is a list that stops updating;
    /// one that never dozes is the twenty-six-times-the-CPU this was written
    /// to remove. Both edges are here, and `stir` in between.
    #[test]
    fn a_window_dozes_only_after_a_minute_untouched_and_wakes_on_a_touch() {
        let long_ago = std::time::Instant::now() - AWAKE_FOR - std::time::Duration::from_secs(1);
        let just_now = std::time::Instant::now();

        // Untouched for longer than the window stays awake: it dozes.
        assert!(long_ago.elapsed() >= AWAKE_FOR, "the test's own premise");
        // Touched within it: it does not.
        assert!(just_now.elapsed() < AWAKE_FOR);
        // A doze asks again far less often than an awake window, which is
        // where the saving is — and not never, so the first frame after a
        // touch is close.
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
        // Every column the table can offer, not only the ones showing: which
        // five are on screen is a person's answer now, and any of the twelve
        // can be one of them.
        let table = Table {
            shown: scour_ui::COLUMNS.iter().collect(),
            widths: Default::default(),
        };
        let heads = heads_of(&table, &cat);
        assert_eq!(heads.len(), scour_ui::COLUMNS.len());
        for (h, c) in heads.iter().zip(scour_ui::COLUMNS) {
            assert_eq!(h.id, c.id);
            assert_eq!(h.sort, c.sort, "`{}` asks the wrong sort key", c.id);
            // The word comes from the crate rather than from this file: a
            // heading spelled here would be a second place the column is
            // named, and the page would go on calling it the first.
            assert!(!h.label.is_empty(), "`{}` has no heading word", c.id);
        }
    }

    #[test]
    fn a_column_switched_on_lands_where_the_table_says_and_moves_nothing() {
        let col = |id: &str| scour_ui::column(id).unwrap();
        let ids = |t: &Table| t.ids().join(",");

        let mut t = Table {
            // Deliberately not the table's own order: this is somebody's
            // arrangement, and switching a column on must not undo it.
            shown: vec![col("size"), col("name")],
            widths: Default::default(),
        };
        // `path` comes after `name` and before `size` in `COLUMNS`, so it
        // lands in front of the first shown column that outranks it — which
        // is `size`, at the front — and the arrangement survives.
        t.toggle("path");
        assert_eq!(ids(&t), "path,size,name");

        // Off is off, and the width it was dragged to stays behind for when
        // it comes back.
        t.toggle("size");
        assert_eq!(ids(&t), "path,name");

        // **The last one standing cannot be turned off.** A table of no
        // columns is not a smaller table, it is a broken one.
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
        // A column this build does not have is dropped rather than the whole
        // list refused: the rest is what somebody meant.
        assert_eq!(read(&["kind", "zurna", "name"]), "kind,name");
        // And a list with nothing left in it is the one answer that cannot be
        // right, so the default stands.
        assert_eq!(read(&["zurna"]), COLUMN_DEFAULT.join(","));
        assert_eq!(read(&[]), COLUMN_DEFAULT.join(","));
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
