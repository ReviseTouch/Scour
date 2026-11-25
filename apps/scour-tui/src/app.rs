//! Everything the terminal knows, and the pure transitions on it.
//!
//! One struct, no drawing, no sockets: what happened goes in, what to ask the
//! service for comes out. That split is what makes this testable without a
//! terminal and without a service.

use scour_core::{Hit, SortKey};
use scour_page::Pages;

use crate::link::Deed;

mod answers;
mod columns;
mod menu;
mod order;
mod rail;
mod selection;
mod settings;
mod weighing;

/// A line of working out, when `SCOUR_TUI_TRACE` is set: to the file it names,
/// or to standard error.
pub fn trace(what: &str) {
    let Some(where_to) = std::env::var_os("SCOUR_TUI_TRACE") else {
        return;
    };
    // A path, when one is given: standard error is the screen being drawn on,
    // so a line written there lands in the middle of the frame.
    let line = format!("tui: {what}\n");
    match where_to.to_str() {
        Some(path) if path.starts_with('/') => {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = f.write_all(line.as_bytes());
            }
        }
        _ => eprint!("{line}"),
    }
}

/// How many weighed children the report draws, and walks over.
pub const WEIGHED: usize = 6;

/// The three size bands are always offered, and the rail's fixed furniture is
/// three headings and two blank lines.
const RAIL_FIXED: usize = 3 + 3 + 2;
/// Never fewer than this many kinds, even on a short terminal: a rail showing
/// two of them says less than the query line already does.
const KINDS_LEAST: usize = 4;
/// More places than this is a list of somebody's whole home directory.
const PLACES_MOST: usize = 6;

/// What the pointer is over, if anything that answers to it. One value, used by
/// both the drawing and the press, so what lights is what a press takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Spot {
    #[default]
    Nothing,
    /// A row of the result, by its number in the whole result.
    Row(usize),
    /// A line of the rail, by its place among what the rail offers.
    Rail(usize),
    /// A band of the time strip.
    Strip(usize),
    /// A line of whatever panel is open.
    Panel(usize),
    /// A section in the settings' list.
    Section(usize),
    /// The query line.
    Query,
    /// The filter written beside the query: pressing it takes the filter off.
    Chip,
    /// One of the three things that can be done with a selection.
    Deed(usize),
    /// The mark at the left of a row: pressing it picks the row.
    Tick(usize),
    /// One of the tools along the counter line.
    Tool(usize),
    /// A column heading, by its place along the row.
    Head(usize),
    /// The scrollbar, by which of its rows the pointer is on.
    Bar(u16),
}

/// What is over the list, if anything. One at a time: a second panel behind the
/// first is a panel nobody can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    None,
    /// What the walk skips, and which of those are switched off.
    Rules,
    /// The disks, and the question about one that came.
    Disks,
    /// The desktop's key that opens Scour: what it is, and typing another.
    Key,
    /// Window, terminal, browser.
    Faces,
    /// What can be done with the row the cursor is on: the list in
    /// `scour-ui::menu`, drawn as a panel because a terminal has no right button.
    Menu,
    /// Which of the programs that claim this kind of file to start. The menu
    /// becomes this list rather than growing a submenu beside itself.
    Openers,
    /// Which columns the table shows: every column with a tick against the ones
    /// that are on, and a last line that puts them all back.
    Columns,
    /// The question that comes before something that changes files. The cursor
    /// starts on the safe answer: a terminal cannot dim what is behind it.
    Ask,
    /// The settings' first section: language, the face that opens, the theme,
    /// and the way to the key and to the other faces.
    General,
    /// What happens to a disk plugged in or on the network, and folders
    /// indexed on their own.
    Later,
    /// How words are read: accents, and typing mistakes.
    Searching,
    /// The keys, as a section to read.
    Keys,
    /// What is running.
    About,
    /// Contextual folder explorer, filter launch, sparkline & fuzzy choices.
    Lens,
}

/// The settings screen's sections, in order: the panel that is each one's
/// content, its title and the line under it, as msgids. One screen for what
/// were separate panels, as in the window and the page.
pub const SECTIONS: [(Panel, &str, &str); 8] = [
    (
        Panel::General,
        "General",
        "The same for every face: the window, the browser and the terminal read these.",
    ),
    (
        Panel::Columns,
        "List and look",
        "Which columns the table shows.",
    ),
    (
        Panel::Searching,
        "Searching",
        "How the words you type are read. The same in every face.",
    ),
    (
        Panel::Disks,
        "Disks scanned",
        "Scour knows every file on these. A disk taken out leaves the index; putting it back takes a scan.",
    ),
    (
        Panel::Later,
        "Disks that come later",
        "What happens to a disk plugged in or on the network, and folders added on their own.",
    ),
    (
        Panel::Rules,
        "Skipped",
        "Places inside the scanned disks that are not looked at. A rule switched off is kept, and can be switched on again.",
    ),
    (
        Panel::Keys,
        "Keyboard",
        "What the keys do, and what the query understands.",
    ),
    (Panel::About, "About", "What is running, and what it holds."),
];

/// Which of the two the bare letters go to. Search is where it starts: the keys
/// that move are always on the arrows, and letters that move are a choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Letters go into the query.
    Search,
    /// Letters move: `j k g G`, and the rest of the map in `keys`.
    Move,
}

/// What a step wants done about the service. Returned rather than done, so that
/// the state machine can be stepped in a test; not boxed, one per key press.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    Nothing,
    /// A page of the current query.
    Page {
        generation: u64,
        query: String,
        /// What the kinds are counted over — the query without its kind term.
        /// See [`App::kinds_over`].
        counting: String,
        /// What the strip is about, which is the query without its age term.
        /// See [`App::strip_over`].
        over: String,
        sort: SortKey,
        descending: bool,
        offset: u32,
        limit: u32,
        cap: u32,
    },
    /// Look at these paths again, now: this program moved them.
    Recheck(Vec<String>),
    /// A `content:` query's reading goes on, or with `true` begins again.
    Reread(bool),
    /// Ask what the walk skips.
    Rules,
    /// Ask for everything the report shows, about this folder.
    Report(String),
    /// Weigh this folder.
    Weigh(String),
    /// Show what can be shown of this file.
    Peek(String),
    /// Replace the list of switched-off rules.
    OffRules(Vec<String>),
    /// Ask for the disks.
    Disks,
    /// Answer about a disk: `index`, `leave` or `later`.
    Disk {
        volume: String,
        what: String,
    },
    /// Remember a preference.
    Remember(scour_settings::Change),
    /// Ask the desktop about its key, or change it.
    Hotkey(Deed),
    /// Write the whole result to this file.
    Export {
        query: String,
        to: String,
    },
    /// Subfolders of a directory for the Miller Place lens.
    Tree(String),
    /// On-demand bounded grep inside matching files.
    Grep {
        generation: u64,
        append: bool,
        query: String,
        pattern: String,
        mode: scour_proto::GrepMode,
        exact_case: bool,
        binary: bool,
        markup: bool,
        sort: SortKey,
        descending: bool,
        offset: u32,
    },
    /// Close the terminal.
    Leave,
}

/// Hand a path to the desktop, and forget about it: detached, with every
/// standard stream closed.
fn launch(path: &str) {
    let _ = scour_openers::open(scour_core::path::to_path(path).as_os_str());
}

/// One drawn line of the menu: a flattened `scour_ui::menu::Item`, its label
/// already translated and already carrying its count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuLine {
    /// Sent back when it is picked. Never translated.
    pub id: String,
    pub label: String,
    pub key: String,
    /// A rule is drawn above this one: it starts a new group.
    pub rule: bool,
    /// Reversible, but it changes something — drawn in the danger colour.
    pub careful: bool,
    /// Asks before it acts — drawn dim.
    pub heavy: bool,
    /// One of the few across the top, a word each, walked with left and right.
    /// They come first in [`App::menu`].
    pub strip: bool,
}

/// The whole state of the terminal.
pub struct App {
    /// How long to leave between re-reading the rows in sight once the index
    /// moved: `Settings::refresh_after`, shared with the other faces.
    pub refresh: std::time::Duration,
    /// What has been typed, and where the caret is in it (in bytes).
    pub query: String,
    pub caret: usize,
    pub mode: Mode,
    /// Which keystroke the answers being drawn belong to.
    pub generation: u64,
    /// The rows in hand, and every rule about which ones to have.
    pub pages: Pages<Hit>,
    /// The row the cursor is on, in the whole result.
    pub cursor: usize,
    /// Whether somebody has put the cursor somewhere. An untouched one belongs
    /// to the list, not to a file: the top row is a place.
    pub anchored: bool,
    /// What that row *is*. A row number is not an identity: one file saved
    /// anywhere pushes every row down, and `Enter` would open the wrong one.
    pub cursor_at: Option<String>,
    /// The first row drawn, which the cursor pushes along.
    pub top: usize,
    /// How many rows the list has room for. The drawing sets it.
    pub room: usize,
    pub sort: SortKey,
    pub descending: bool,
    /// The order the settings hold, so a change is written once.
    pub kept_order: (SortKey, bool),
    /// What the last answer cost, for the meter.
    pub took_us: u64,
    pub rows_visited: u64,
    /// How far a `content:` query's reading has got; none on any other query.
    pub reading: Option<scour_core::Reading>,
    /// Answers landed, which is what turns the mark beside a reading.
    pub turn: usize,
    /// True while the count is a floor rather than a total.
    pub capped: bool,
    /// Something to say instead of the counts.
    pub trouble: String,
    /// The rows somebody has picked, by path, and what they weigh. By path,
    /// because a row number means nothing across a re-sort.
    pub picked: std::collections::BTreeMap<String, i64>,
    /// Where a run of `Shift` presses started.
    pub anchor: usize,
    /// The head of the file under the cursor, when somebody asked for it. Asked
    /// for, not kept up with: every row arrowed past would be a disk read.
    pub peek: Option<scour_preview::Look>,
    /// True while the peek panel is open.
    pub peeking: bool,
    /// True while the report is being read instead of the list. A tab rather
    /// than a panel: it is read for as long as a list is.
    pub reporting: bool,
    /// What the index holds, for the report.
    pub stats: Option<scour_core::IndexStats>,
    /// The folder the report is weighing, and what came back. Walked, not
    /// searched: pressing a child asks about that child.
    pub weighing: String,
    pub usage: Option<scour_core::UsageResponse>,
    /// Which of the weighed children the cursor is on.
    pub weigh_at: usize,
    /// What the weighed folder is made of, by kind, largest first. Counted over
    /// the report's scope, so it is not the rail's list under another name.
    pub scope_kinds: Vec<(String, u64)>,
    /// The duplicate groups: how big one copy is, how many there are, and
    /// where the first of them lives.
    pub dupes: Vec<(u64, u64, String)>,
    pub waste: u64,
    /// Which panel is over everything, if any.
    pub panel: Panel,
    /// Where the cursor is inside the open panel.
    pub panel_at: usize,
    /// Which columns the table shows, in the order it shows them — the setting
    /// every face reads, so a table arranged in one opens arranged in the rest.
    pub columns: Vec<&'static scour_ui::Column>,
    /// The menu as it stands, built when it is opened. Held rather than rebuilt
    /// per frame: the row under it can move while it is up.
    pub menu: Vec<MenuLine>,
    /// The settings screen is up, on the section whose panel is `panel`.
    pub settings: bool,
    /// What the settings hold, as they were read and as they were changed here:
    /// `""` is "nobody chose" for every one of these.
    pub theme: String,
    pub face_default: String,
    pub language_choice: String,
    pub removable: String,
    pub network: String,
    pub optical: String,
    pub folders: Vec<String>,
    /// How the service reads words: `Settings::accents_exact` and `typos`.
    pub accents_exact: bool,
    pub typos: String,
    /// What the search in sight nearly found, when it found nothing.
    pub similar: Option<scour_core::Similar>,
    /// What the menu is about: the row's name, or how many are picked; and
    /// where it is and what it weighs, or what the selection comes to.
    pub menu_title: String,
    pub menu_detail: String,
    /// What a pending question is about: the item id, and the paths it was
    /// asked about. Taken when the question is answered, whichever way.
    pub pending: Option<(String, Vec<String>)>,
    /// The question's own words, while it is up.
    pub ask_title: String,
    /// The question takes a line of text rather than only a yes.
    pub ask_typing: bool,
    /// What has been typed into it.
    pub ask_text: String,
    /// The programs that will take the row under the cursor: what to show,
    /// and which desktop entry to start.
    pub openers: Vec<(String, String)>,
    /// The word on the button that says yes. A rename does not *move*
    /// anything, and a button that says so is a button that lies.
    pub ask_yes: String,
    /// The skip rules as the service last reported them, kept from the answer:
    /// switching one off means sending back the whole list.
    pub rules: Vec<(String, String, bool, bool)>,
    /// The disks as the service last listed them.
    pub disks: Vec<scour_proto::Disk>,
    /// How many waited for an answer at the last status; the list is asked
    /// for again when that moves. `None` until the first.
    pub disks_asking: Option<u32>,
    /// What the desktop last said about the key that opens Scour: which
    /// desktop it is, the combination if there is one, what it runs, and
    /// whether this program may write one here. Empty until it is asked.
    pub key_desktop: String,
    pub key_bound: Option<String>,
    pub key_command: String,
    pub key_can: bool,
    /// What has been typed into the panel, and what came back about it. The
    /// note is the panel's own line: a refusal there belongs beside the field
    /// it is about, not on the counter line with the last file copied.
    pub key_typed: String,
    pub key_note: String,
    /// Whether that note is a refusal rather than a remark.
    pub key_bad: bool,
    /// What was said about the last thing done — a file written, a language
    /// changed. Cleared by the next keystroke.
    pub note: String,
    /// What the index looked like when the pages in hand were read.
    pub revision: u64,
    /// What the pointer is over, and what it is holding down. A terminal draws
    /// no hover of its own, so these two are the whole of it.
    pub hover: Spot,
    pub pressed: Spot,
    /// The row last clicked and when, for telling a double-click from two.
    pub clicked: Option<(usize, std::time::Instant)>,
    /// True while the key list is over everything.
    pub helping: bool,
    /// The query cut into runs, for drawing it in colour. Empty until the
    /// service has read it back.
    pub spans: Vec<scour_core::Span>,
    /// The rail: what the matching rows are made of, and where they live.
    pub kinds: Vec<(String, u64)>,
    pub places: Vec<(String, String)>,
    /// Where the volumes are and whether they record reads. See `frozen_atime`.
    pub mounts: Vec<scour_places::Mount>,
    /// The twenty-four bars of the time strip, oldest first, and the day
    /// each of them stands for.
    pub strip: Vec<(u32, u64)>,
    /// Which filter is in force, if any — `kind:code`, `under:"…"`, `dm:7d`.
    pub filter: Option<String>,
    /// Whether the rail is on screen. Off under a hundred columns, where its
    /// twenty-four would leave the name too narrow to read.
    pub rail: bool,
    /// True while the arrows move in the rail rather than the list.
    pub in_rail: bool,
    /// Which line of the rail the cursor is on.
    pub rail_at: usize,
    /// How far a walk of the index has got, when one is running. Drawn beside
    /// the counts: nothing else on screen moves while a scan runs.
    pub scanning: Option<u64>,
    /// Writes of the index that have failed in a row; see `scour_ui::UNWRITTEN`.
    pub unwritten: u32,
    /// Whether the index has grown an unsorted tail worth rebuilding: every
    /// query reads that tail, so it is what the search drifts to.
    pub rebuild_advised: bool,
    /// Set when a redraw is owed. Nothing is drawn without one: a terminal that
    /// redraws on a timer burns a core doing nothing.
    pub dirty: bool,
    pub leaving: bool,
    /// The words this interface speaks. Here rather than in a constant: every
    /// cell is redrawn from this struct, so swapping it is the whole switch.
    pub words: scour_i18n::Catalogue,
    /// Contextual lenses and the on-demand inline Grep capsule.
    pub v2: crate::v2::V2,
}

impl Default for App {
    fn default() -> Self {
        App {
            refresh: std::time::Duration::from_secs(2),
            query: String::new(),
            caret: 0,
            mode: Mode::Search,
            rebuild_advised: false,
            generation: 0,
            pages: Pages::default(),
            cursor: 0,
            anchored: false,
            cursor_at: None,
            top: 0,
            room: 1,
            sort: SortKey::Modified,
            descending: true,
            kept_order: (SortKey::Modified, true),
            took_us: 0,
            rows_visited: 0,
            reading: None,
            turn: 0,
            capped: false,
            trouble: String::new(),
            picked: std::collections::BTreeMap::new(),
            anchor: 0,
            peek: None,
            peeking: false,
            reporting: false,
            stats: None,
            weighing: String::new(),
            usage: None,
            weigh_at: 0,
            scope_kinds: Vec::new(),
            dupes: Vec::new(),
            waste: 0,
            panel: Panel::None,
            panel_at: 0,
            columns: scour_ui::DEFAULT_COLUMNS
                .iter()
                .filter_map(|id| scour_ui::column(id))
                .collect(),
            menu: Vec::new(),
            settings: false,
            theme: String::new(),
            face_default: String::new(),
            language_choice: String::new(),
            removable: String::new(),
            network: String::new(),
            optical: String::new(),
            folders: Vec::new(),
            accents_exact: false,
            typos: String::new(),
            similar: None,
            menu_title: String::new(),
            menu_detail: String::new(),
            pending: None,
            ask_title: String::new(),
            ask_typing: false,
            ask_text: String::new(),
            ask_yes: String::new(),
            openers: Vec::new(),
            rules: Vec::new(),
            disks: Vec::new(),
            disks_asking: None,
            key_desktop: String::new(),
            key_bound: None,
            key_command: String::new(),
            // Until the desktop has been asked, the panel offers the field: a
            // panel that opens refusing and then changes its mind reads worse.
            key_can: true,
            key_typed: String::new(),
            key_note: String::new(),
            key_bad: false,
            note: String::new(),
            revision: 0,
            hover: Spot::default(),
            pressed: Spot::default(),
            clicked: None,
            helping: false,
            spans: Vec::new(),
            kinds: Vec::new(),
            places: Vec::new(),
            mounts: Vec::new(),
            strip: Vec::new(),
            filter: None,
            rail: true,
            in_rail: false,
            rail_at: 0,
            scanning: None,
            unwritten: 0,
            dirty: true,
            leaving: false,
            words: scour_i18n::Catalogue::english(),
            v2: crate::v2::V2::default(),
        }
    }
}

impl App {
    /// This string, in the reader's language. The msgid is the English, so one
    /// with no entry in the catalogue comes back as itself.
    pub fn say<'a>(&'a self, msgid: &'a str) -> std::borrow::Cow<'a, str> {
        use scour_core::Catalog;
        self.words.get(msgid)
    }

    /// How this language punctuates numbers: the thousands mark, then the
    /// decimal one. Off the catalogue, not the desktop, which may differ.
    pub fn mark(&self) -> (char, char) {
        (
            scour_ui::format::group_mark(self.words.language()),
            scour_ui::format::decimal_mark(self.words.language()),
        )
    }
}

/// The keys the arrow keys cycle through, in the order the columns are drawn —
/// the same the window's headings offer, so a re-sort travels between faces.
pub const SORTS: [(SortKey, &str); 4] = [
    (SortKey::Name, "name"),
    (SortKey::Path, "where"),
    (SortKey::Modified, "changed"),
    (SortKey::Size, "size"),
];

/// The engine's own key behind a column's `sort` word. `None` is a column that
/// cannot be sorted by, which the shared table says by leaving it empty.
pub fn sort_key(name: &str) -> Option<SortKey> {
    SortKey::named(name).filter(|k| *k != SortKey::Relevance)
}

impl App {
    /// Write the whole result to a spreadsheet in the download folder, and say
    /// where it went: a file that appears silently is one nobody finds.
    pub fn write_sheet(&mut self) -> Want {
        let home = std::env::var("HOME").unwrap_or_default();
        let dir = scour_places::downloads()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| home.clone());
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let to = format!("{dir}/scour-{stamp}.csv");
        self.note = format!("{} {to}", self.say("writing…"));
        self.dirty = true;
        Want::Export {
            query: self.asking(),
            to,
        }
    }

    /// The row under the cursor, if its page is in hand.
    pub fn here(&self) -> Option<&Hit> {
        self.pages.at(self.cursor)
    }
}

/// Another of Scour's programs: beside this one when installed together, else
/// on the `PATH`.
fn sibling(name: &str) -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|here| Some(here.parent()?.join(name)))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| name.into())
}

#[cfg(test)]
mod tests;

/// Put text on the clipboard from inside a terminal: the desktop's own tool
/// first, OSC 52 second — VTE accepts the escape and drops the text.
fn copy(text: &str) -> Result<&'static str, String> {
    use std::io::Write;
    let desktop =
        std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    if desktop {
        for (tool, args) in [
            ("wl-copy", &[][..]),
            ("xclip", &["-selection", "clipboard"][..]),
            ("xsel", &["--clipboard", "--input"][..]),
        ] {
            let mut child = match std::process::Command::new(tool)
                .args(args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(_) => continue,
            };
            let wrote = match child.stdin.take() {
                // Dropped here: the tool reads until the pipe closes.
                Some(mut pipe) => pipe.write_all(text.as_bytes()).is_ok(),
                None => false,
            };
            if wrote {
                // `wl-copy` forks: the process that exits is not the holder.
                let _ = child.wait();
                return Ok(tool);
            }
        }
    }
    let coded = base64(text.as_bytes());
    let mut out = std::io::stdout();
    if write!(out, "\x1b]52;c;{coded}\x07").is_ok() && out.flush().is_ok() {
        // "Sent", not "copied": the terminal does not say whether it arrived.
        return Ok("sent to the terminal");
    }
    Err("no clipboard".into())
}

/// Base64, because OSC 52 carries its text that way and this is the only
/// thing in the program that needs it.
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let mut block = [0u8; 3];
        block[..group.len()].copy_from_slice(group);
        let n = u32::from(block[0]) << 16 | u32::from(block[1]) << 8 | u32::from(block[2]);
        for i in 0..4 {
            if i <= group.len() {
                out.push(ABC[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Every word this interface says, checked against the catalogue. The list is
/// read out of the source, so a string added anywhere is already asked about.
#[cfg(test)]
mod words {
    /// Every string handed to [`App::say`] in the terminal, plus the tables
    /// looked up by value. It reads every source file under `src/`, so a file
    /// added later is asked about too; a msgid in a comment counts.
    fn asked_for() -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for text in sources(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")) {
            let mut rest = text.as_str();
            while let Some(at) = rest.find("say(\"") {
                rest = &rest[at + 5..];
                // No escapes are used in any of them, and a msgid that needed
                // one would be a sentence with a quotation mark in it.
                match rest.find('"') {
                    Some(end) => {
                        out.push(rest[..end].to_string());
                        rest = &rest[end..];
                    }
                    None => break,
                }
            }
        }
        let app = super::App::default();
        for (label, _) in app.tools() {
            out.push(label.to_string());
        }
        for (label, _) in app.deeds() {
            out.push(label.to_string());
        }
        for (_, name) in super::SORTS {
            out.push(name.to_string());
        }
        out.push("relevance".into());
        for (key, what) in crate::keys::MAP {
            out.push(what.to_string());
            // Keycaps are the same in every language and are not in the
            // catalogue; an all-lowercase word marks the few that are prose.
            if key
                .split_whitespace()
                .any(|w| w.len() > 1 && w.chars().all(|c| c.is_ascii_lowercase()))
            {
                out.push(key.to_string());
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Every `.rs` file under `dir`, read.
    fn sources(dir: &std::path::Path) -> Vec<String> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).expect("src/").flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(sources(&path));
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(std::fs::read_to_string(&path).expect("a source file"));
            }
        }
        out
    }

    /// The scan reaches the files a hand-kept list once left out.
    #[test]
    fn every_source_file_is_read() {
        let all = sources(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
        assert!(all.len() >= 10, "{} files", all.len());
    }

    #[test]
    fn the_terminal_says_nothing_the_catalogue_has_not_been_told_about() {
        let turkish = scour_i18n::Catalogue::for_language("tr");
        let missing: Vec<String> = asked_for()
            .into_iter()
            .filter(|id| !turkish.has(id))
            .collect();
        assert!(
            missing.is_empty(),
            "{} string(s) would come out in English on a Turkish machine: {missing:#?}",
            missing.len()
        );
    }

    /// A placeholder that survives the translation, or a number lands nowhere:
    /// substitution is a plain `replace`, which reports nothing when it misses.
    #[test]
    fn a_translation_keeps_the_holes_the_english_had() {
        use scour_core::Catalog;
        let turkish = scour_i18n::Catalogue::for_language("tr");
        for id in asked_for() {
            let mut holes: Vec<&str> = id
                .match_indices('{')
                .filter_map(|(at, _)| {
                    let rest = &id[at..];
                    rest.find('}').map(|end| &rest[..=end])
                })
                .collect();
            holes.sort();
            holes.dedup();
            let said = turkish.get(&id);
            for hole in holes {
                assert!(
                    said.contains(hole),
                    "the Turkish for {id:?} has lost {hole}: {said:?}"
                );
            }
        }
    }
}
