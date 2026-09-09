//! Everything the terminal knows, and the pure transitions on it.
//!
//! One struct, no drawing, no sockets: what happened goes in, what to ask the
//! service for comes out. That split is what makes this testable without a
//! terminal and without a service.

use scour_core::{Hit, SortKey};
use scour_page::{Change, Pages};

use crate::link::TYPING_CAP;

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
    /// Which of the two languages to speak.
    Language,
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
}

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
    /// Ask what the walk skips.
    Rules,
    /// Ask for everything the report shows.
    Report,
    /// Weigh this folder.
    Weigh(String),
    /// Show what can be shown of this file.
    Peek(String),
    /// Replace the list of switched-off rules.
    OffRules(Vec<String>),
    /// Remember a preference.
    Remember(scour_settings::Change),
    /// Write the whole result to this file.
    Export { query: String, to: String },
    /// Close the terminal.
    Leave,
}

/// Hand a path to the desktop, and forget about it: detached, with every
/// standard stream closed.
fn launch(path: &str) {
    let _ = std::process::Command::new("xdg-open")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
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
}

/// The whole state of the terminal.
pub struct App {
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
    /// What the last answer cost, for the meter.
    pub took_us: u64,
    pub rows_visited: u64,
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
    /// What was said about the last thing done — a file written, a language
    /// changed. Cleared by the next keystroke.
    pub note: String,
    /// What the index looked like when the pages in hand were read.
    pub revision: u64,
    /// What the pointer is over, and what it is holding down. A terminal draws
    /// no hover of its own, so these two are the whole of it.
    pub hover: Spot,
    pub pressed: Spot,
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
}

impl Default for App {
    fn default() -> Self {
        App {
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
            took_us: 0,
            rows_visited: 0,
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
            dupes: Vec::new(),
            waste: 0,
            panel: Panel::None,
            panel_at: 0,
            columns: scour_ui::DEFAULT_COLUMNS
                .iter()
                .filter_map(|id| scour_ui::column(id))
                .collect(),
            menu: Vec::new(),
            pending: None,
            ask_title: String::new(),
            ask_typing: false,
            ask_text: String::new(),
            ask_yes: String::new(),
            openers: Vec::new(),
            rules: Vec::new(),
            note: String::new(),
            revision: 0,
            hover: Spot::default(),
            pressed: Spot::default(),
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
            dirty: true,
            leaving: false,
            words: scour_i18n::Catalogue::english(),
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
    Some(match name {
        "name" => SortKey::Name,
        "path" => SortKey::Path,
        "modified" => SortKey::Modified,
        "created" => SortKey::Created,
        "accessed" => SortKey::Accessed,
        "size" => SortKey::Size,
        "disk" => SortKey::Disk,
        "kind" => SortKey::Kind,
        "ext" => SortKey::Ext,
        "mode" => SortKey::Mode,
        "uid" => SortKey::Uid,
        "gid" => SortKey::Gid,
        _ => return None,
    })
}

impl App {
    /// What the sort is called, for the meter — as a msgid, not as words: the
    /// word is looked up where it is drawn.
    pub fn sort_name(&self) -> &'static str {
        SORTS
            .iter()
            .find(|(key, _)| *key == self.sort)
            .map(|(_, name)| *name)
            .unwrap_or("relevance")
    }

    /// Sort by a column, or turn it round when it is the one already sorted by.
    /// The first press sorts, the second reverses, and it starts descending.
    pub fn sort_by(&mut self, column: usize) -> Want {
        // The key comes off the column drawn at that position, not off a fixed
        // list: which columns are on screen is somebody's own arrangement.
        let Some(key) = self.columns.get(column).and_then(|c| sort_key(c.sort)) else {
            return Want::Nothing;
        };
        if self.sort == key {
            return self.flip();
        }
        self.sort = key;
        self.descending = true;
        self.reask()
    }

    /// Which heading has the arrow on it, if the column it sorts by is shown.
    pub fn sorted_column(&self) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| sort_key(c.sort) == Some(self.sort))
    }

    /// Sort by the next column along, or the previous one. The query and the
    /// selection stay; only the order changes.
    pub fn resort(&mut self, by: isize) -> Want {
        let at = SORTS.iter().position(|(key, _)| *key == self.sort);
        let next = match at {
            Some(at) => at.saturating_add_signed(by).min(SORTS.len() - 1),
            None => 0,
        };
        self.sort = SORTS[next].0;
        self.reask()
    }

    /// Ascending or descending.
    pub fn flip(&mut self) -> Want {
        self.descending = !self.descending;
        self.reask()
    }

    /// The same query, asked again: a new order, or a new direction.
    fn reask(&mut self) -> Want {
        self.generation += 1;
        self.pages.empty();
        self.cursor = 0;
        self.top = 0;
        self.dirty = true;
        self.ask(0, scour_page::SPAN as u32, TYPING_CAP)
    }

    /// Pick this row and nothing else — what a plain click does everywhere. A
    /// click is a selection, not a cursor move: the deed bar depends on it.
    pub fn pick_only(&mut self, row: usize) -> Want {
        let want = self.go(row);
        self.picked.clear();
        self.anchor = row;
        if let Some(hit) = self.pages.at(row) {
            let bytes = if hit.is_dir { 0 } else { hit.meta.size.max(0) };
            self.picked.insert(hit.path.clone(), bytes);
        }
        self.dirty = true;
        want
    }

    /// Pick or unpick the row under the cursor.
    pub fn pick(&mut self) -> Want {
        let Some(hit) = self.pages.at(self.cursor) else {
            return Want::Nothing;
        };
        let path = hit.path.clone();
        let bytes = if hit.is_dir { 0 } else { hit.meta.size.max(0) };
        if self.picked.remove(&path).is_none() {
            self.picked.insert(path, bytes);
        }
        self.anchor = self.cursor;
        self.dirty = true;
        Want::Nothing
    }

    /// Extend the selection from the anchor to wherever the cursor now is.
    pub fn pick_to(&mut self, row: usize) -> Want {
        let (from, to) = if row < self.anchor {
            (row, self.anchor)
        } else {
            (self.anchor, row)
        };
        for at in from..=to {
            if let Some(hit) = self.pages.at(at) {
                let bytes = if hit.is_dir { 0 } else { hit.meta.size.max(0) };
                self.picked.insert(hit.path.clone(), bytes);
            }
        }
        self.dirty = true;
        self.go(row)
    }

    /// Nothing is picked any more.
    pub fn unpick(&mut self) {
        if !self.picked.is_empty() {
            self.picked.clear();
            self.dirty = true;
        }
    }

    /// What a selection comes to: how many, how many of them folders, and the
    /// bytes of the files among them.
    pub fn weighed(&self) -> (usize, usize, u64) {
        let folders = self.picked.values().filter(|b| **b == 0).count();
        let bytes = self.picked.values().map(|b| *b as u64).sum();
        (self.picked.len(), folders, bytes)
    }

    /// The query changed: everything in hand belongs to the old one.
    pub fn typed(&mut self) -> Want {
        self.generation += 1;
        self.pages.empty();
        self.pages.set_total(0);
        self.cursor = 0;
        self.top = 0;
        self.anchor = 0;
        self.anchored = false;
        self.cursor_at = None;
        self.trouble.clear();
        // The old colouring belongs to the old text; drawing it over the new
        // one is worse than drawing none.
        self.spans.clear();
        // A new question, a new selection: carrying one over means acting on
        // files nobody can see.
        self.picked.clear();
        self.dirty = true;
        self.ask(0, scour_page::SPAN as u32, TYPING_CAP)
    }

    /// A page arrived. Returns what to ask for next, if anything.
    pub fn landed(
        &mut self,
        generation: u64,
        offset: u32,
        limit: u32,
        reply: scour_core::SearchResponse,
    ) -> Want {
        // An answer to an older keystroke is dropped: the list would go
        // backwards under somebody's hands.
        if generation != self.generation {
            return Want::Nothing;
        }
        self.took_us = reply.took_us;
        self.rows_visited = reply.rows_visited;
        self.capped = reply.capped;
        self.trouble.clear();
        let page = Pages::<Hit>::page_of(offset as usize);
        let arrived = reply.hits.len();
        // A page short of both what was asked for and the largest page seen is
        // the end: both, because a new query's first page is short on purpose.
        let total = self.pages.length(page, arrived, limit as usize);
        let total = total.max(reply.total as usize);
        let first = reply
            .hits
            .first()
            .map(|h| h.path.clone())
            .unwrap_or_default();
        match self.pages.put(page, reply.hits, total) {
            Change::Nothing => {}
            _ => self.dirty = true,
        }
        trace(&format!(
            "landed page {page} ({arrived} rows, total {total}); its first is {first}"
        ));
        self.refollow();
        self.follow()
    }

    /// The query, read back by the parser.
    pub fn explained(&mut self, generation: u64, spans: Vec<scour_core::Span>) {
        if generation != self.generation {
            return;
        }
        trace(&format!("query read back in {} runs", spans.len()));
        self.spans = spans;
        self.dirty = true;
    }

    /// Show the report, or go back to the list. Asked for when it opens rather
    /// than kept fresh: finding duplicates reads files.
    pub fn report(&mut self) -> Want {
        self.reporting = !self.reporting;
        self.dirty = true;
        if !self.reporting {
            return Want::Nothing;
        }
        // It opens on the home directory: weighing everything indexed answers
        // with one child, the filesystem root. `Backspace` still walks out.
        if self.usage.is_none() {
            self.weighing = std::env::var("HOME").unwrap_or_default();
        }
        Want::Report
    }

    /// Open or close the peek, and ask for what it shows.
    pub fn peek(&mut self) -> Want {
        self.peeking = !self.peeking;
        self.peek = None;
        self.dirty = true;
        if !self.peeking {
            return Want::Nothing;
        }
        match self.here() {
            Some(hit) if !hit.is_dir => Want::Peek(hit.path.clone()),
            _ => {
                self.peeking = false;
                Want::Nothing
            }
        }
    }

    /// The cursor moved while the peek is open: it follows.
    pub fn repeek(&mut self) -> Want {
        if !self.peeking {
            return Want::Nothing;
        }
        self.peek = None;
        match self.here() {
            Some(hit) if !hit.is_dir => Want::Peek(hit.path.clone()),
            _ => Want::Nothing,
        }
    }

    /// Weigh a folder — a child of the one being weighed, or its parent.
    pub fn weigh(&mut self, path: String) -> Want {
        self.weighing = path.clone();
        self.usage = None;
        self.weigh_at = 0;
        self.dirty = true;
        Want::Weigh(path)
    }

    /// Move the cursor over the weighed children.
    pub fn weigh_walk(&mut self, by: isize) {
        let count = self
            .usage
            .as_ref()
            .map(|u| u.children.len().min(WEIGHED))
            .unwrap_or(0);
        if count == 0 {
            return;
        }
        self.weigh_at = self
            .weigh_at
            .saturating_add_signed(by)
            .min(count.saturating_sub(1));
        self.dirty = true;
    }

    /// Go into the child under the cursor.
    pub fn weigh_into(&mut self) -> Want {
        let Some(usage) = &self.usage else {
            return Want::Nothing;
        };
        let Some(child) = usage.children.get(self.weigh_at) else {
            return Want::Nothing;
        };
        let path = child.path.clone();
        self.weigh(path)
    }

    /// Back out to the folder above.
    pub fn weigh_up(&mut self) -> Want {
        if self.weighing.is_empty() {
            return Want::Nothing;
        }
        let up = scour_ui::path::folder(&self.weighing).to_string();
        // `/` has no parent that means anything here; the whole index does.
        let up = if up == "/" { String::new() } else { up };
        self.weigh(up)
    }

    /// The rail's counts arrived. `age` says which of the two questions this
    /// answers: both groups come back in either reply, over different queries.
    pub fn counted(&mut self, generation: u64, age: bool, reply: scour_core::FacetResponse) {
        if generation != self.generation {
            return;
        }
        for group in reply.groups {
            match group.by {
                scour_core::FacetBy::Kind if !age => {
                    // Every kind, including the ones with none: `video 0` is an
                    // answer, and a vanishing row moves every line below it.
                    let counted: std::collections::HashMap<String, u64> =
                        group.facets.into_iter().map(|f| (f.key, f.count)).collect();
                    let mut kinds: Vec<(String, u64)> = scour_core::Kind::OFFERED
                        .iter()
                        .map(|kind| {
                            let token = kind.token().to_string();
                            let count = counted.get(&token).copied().unwrap_or(0);
                            (token, count)
                        })
                        .collect();
                    // Largest first, so a short rail drops the empty ones.
                    kinds.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
                    self.kinds = kinds;
                }
                scour_core::FacetBy::Age { .. } if age => {
                    // Every band, including the empty ones the answer leaves
                    // out: a strip with a different bar count per query has no
                    // axis. `older` is the overflow, not a bar.
                    let counts: std::collections::HashMap<u32, u64> = group
                        .facets
                        .into_iter()
                        .filter_map(|f| f.key.parse::<u32>().ok().map(|days| (days, f.count)))
                        .collect();
                    let mut edges = scour_ui::bar_edges();
                    edges.sort_unstable_by(|a, b| b.cmp(a));
                    self.strip = edges
                        .into_iter()
                        .map(|days| (days, counts.get(&days).copied().unwrap_or(0)))
                        .collect();
                }
                _ => {}
            }
        }
        self.dirty = true;
    }

    /// How many kinds and how many places the rail has room for, counted from
    /// the terminal's height. Both the drawing and the hit test ask this.
    pub fn rail_room(&self) -> (usize, usize) {
        // The rail spans the heading line as well as the list.
        let lines = (self.room + 1).saturating_sub(RAIL_FIXED);
        let places = self.places.len().min(PLACES_MOST);
        let kinds = self
            .kinds
            .len()
            .min(lines.saturating_sub(places).max(KINDS_LEAST));
        // A short terminal takes the kinds' floor out of the places.
        let places = places.min(lines.saturating_sub(kinds));
        (kinds, places)
    }

    /// Everything the rail offers, in the order it is drawn: what it says and
    /// what pressing it asks for. One list, because the cursor walks all of it.
    pub fn rail_lines(&self) -> Vec<(String, String)> {
        let (kinds, places) = self.rail_room();
        let mut out: Vec<(String, String)> = self
            .kinds
            .iter()
            .take(kinds)
            .map(|(token, _)| (token.clone(), scour_ui::query::of_kind(token)))
            .collect();
        out.extend(
            self.places
                .iter()
                .take(places)
                .map(|(label, path)| (label.clone(), scour_ui::query::of_place(path))),
        );
        out.extend(
            scour_ui::query::SIZES
                .iter()
                .map(|(label, term)| ((*label).to_string(), (*term).to_string())),
        );
        out
    }

    /// Which of the rail's offers is drawn on this line of it, if any. Headings
    /// and blanks are not stops; the shape here has to agree with `draw::side`.
    pub fn rail_hit(&self, line: usize) -> Option<usize> {
        let (kinds, places) = self.rail_room();
        let sizes = scour_ui::query::SIZES.len();
        // heading, kinds…, blank, heading, places…, blank, heading, sizes…
        let mut at = 0usize;
        let mut row = 0usize;
        for section in [kinds, places, sizes] {
            row += 1; // the heading
            if line >= row && line < row + section {
                return Some(at + (line - row));
            }
            at += section;
            row += section + 1; // the rows, then the blank line after them
        }
        None
    }

    /// Move the cursor in the rail.
    pub fn rail_walk(&mut self, by: isize) {
        let lines = self.rail_lines().len();
        if lines == 0 {
            return;
        }
        self.rail_at = self
            .rail_at
            .saturating_add_signed(by)
            .min(lines.saturating_sub(1));
        self.dirty = true;
    }

    /// Press whatever the rail's cursor is on.
    pub fn rail_press(&mut self) -> Want {
        let lines = self.rail_lines();
        match lines.get(self.rail_at) {
            Some((_, term)) => {
                let term = term.clone();
                self.press_filter(&term)
            }
            None => Want::Nothing,
        }
    }

    /// The tools, in the order they are drawn, and the key that also does it.
    /// The labels are msgids: [`crate::draw::tool_spans`] measures and looks up.
    pub fn tools(&self) -> [(&'static str, &'static str); 7] {
        [
            ("faces", "^U"),
            ("lang", "^L"),
            ("skips", "^K"),
            // Where the window has a `⋮` at the end of the header row.
            ("columns", "^T"),
            ("csv", "^E"),
            // Beside the peek key: both are about the row the cursor is on.
            ("menu", "F4"),
            ("keys", "F1"),
        ]
    }

    /// Press one of them — the same thing its key does.
    pub fn tool(&mut self, which: usize) -> Want {
        match which {
            0 => {
                self.show(Panel::Faces);
                Want::Nothing
            }
            1 => {
                self.show(Panel::Language);
                Want::Nothing
            }
            2 => {
                self.show(Panel::Rules);
                Want::Rules
            }
            3 => {
                self.show(Panel::Columns);
                Want::Nothing
            }
            4 => self.write_sheet(),
            5 => {
                self.open_menu();
                Want::Nothing
            }
            _ => {
                self.helping = !self.helping;
                self.dirty = true;
                Want::Nothing
            }
        }
    }

    /// What can be done with a selection, in the order the bar shows them — the
    /// same three the window offers, in the same words.
    pub fn deeds(&self) -> [(&'static str, char); 3] {
        [("copy paths", 'y'), ("open folders", 'o'), ("clear", 'x')]
    }

    /// Do one of them.
    pub fn deed(&mut self, which: usize) -> Want {
        match which {
            0 => {
                let paths: Vec<String> = self.picked.keys().cloned().collect();
                let n = paths.len();
                // Nothing picked, nothing done: copying an empty selection
                // empties somebody's clipboard rather than leaving it alone.
                if n == 0 {
                    self.note = self.say("nothing picked").into_owned();
                    self.dirty = true;
                    return Want::Nothing;
                }
                self.note = match copy(&paths.join("\n")) {
                    Ok(how) if n == 1 => format!("{} ({how})", self.say("path copied")),
                    Ok(how) => format!(
                        "{} ({how})",
                        self.say("{n} paths copied").replace("{n}", &n.to_string())
                    ),
                    Err(why) => format!("{}: {why}", self.say("nothing copied")),
                };
            }
            1 => {
                // Each folder once, however many of its files are picked.
                let mut folders: Vec<&str> = self
                    .picked
                    .keys()
                    .map(|p| scour_ui::path::folder(p))
                    .collect();
                folders.sort_unstable();
                folders.dedup();
                for folder in &folders {
                    let _ = std::process::Command::new("xdg-open")
                        .arg(folder)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                }
                self.note = self
                    .say("opening {n} folders")
                    .replace("{n}", &folders.len().to_string());
            }
            _ => {
                self.unpick();
                self.note.clear();
            }
        }
        self.dirty = true;
        Want::Nothing
    }

    /// Take the filter off, whatever it is.
    pub fn unfilter(&mut self) -> Want {
        if self.filter.is_none() {
            return Want::Nothing;
        }
        self.filter = None;
        self.typed()
    }

    /// Press a filter in the rail, or press the one in force to clear it.
    pub fn press_filter(&mut self, term: &str) -> Want {
        self.filter = scour_ui::query::pressed(self.filter.as_deref(), term);
        // The rail keeps what it shows until new counts arrive: emptying it
        // moves every line, so the next press lands where nobody aimed.
        self.typed()
    }

    /// The exact count arrived: no more "at least".
    pub fn counted_exactly(&mut self, generation: u64, total: u64) {
        if generation != self.generation {
            return;
        }
        self.capped = false;
        self.pages.set_total(total as usize);
        self.dirty = true;
    }

    /// The index moved: the pages in hand are marked stale, and only the one on
    /// screen is re-read. The caller waits again either way.
    pub fn awake(&mut self, revision: u64) -> Want {
        if revision == self.revision {
            return Want::Nothing;
        }
        trace(&format!(
            "awake {revision}: cursor {} top {} on {:?}",
            self.cursor,
            self.top,
            self.cursor_at.as_deref().unwrap_or("—")
        ));
        self.revision = revision;
        self.pages.mark(revision);
        self.dirty = true;
        // Refreshing, not re-querying: the row under the cursor stays put.
        let first = self.top;
        let last = (self.top + self.room).saturating_sub(1);
        match self.pages.next_page(first, last, false, true) {
            Some(page) => {
                let offset = (page * scour_page::SPAN) as u32;
                self.ask(offset, scour_page::SPAN as u32, TYPING_CAP)
            }
            None => Want::Nothing,
        }
    }

    /// The service said no.
    pub fn upset(&mut self, generation: u64, why: String) -> Want {
        if generation != self.generation {
            return Want::Nothing;
        }
        self.pages.forget_asking();
        // Through the catalogue on the way in: this program sends msgids and
        // the service sends English, and the lookup handles both.
        self.trouble = self.say(&why).into_owned();
        self.dirty = true;
        Want::Nothing
    }

    /// Ask for whatever the eye is missing, if anything.
    pub fn follow(&mut self) -> Want {
        let first = self.top;
        let last = (self.top + self.room).saturating_sub(1);
        // Speculating is cheap: a page is two hundred rows and the terminal
        // holds thirty-two pages.
        match self.pages.next_page(first, last, true, false) {
            Some(page) => {
                let offset = (page * scour_page::SPAN) as u32;
                self.ask(offset, scour_page::SPAN as u32, TYPING_CAP)
            }
            None => Want::Nothing,
        }
    }

    /// What the service is asked: what was typed, and whatever is pressed.
    pub fn asking(&self) -> String {
        scour_ui::query::compose(&self.query, self.filter.as_deref())
    }

    /// What the **kinds** are counted over: the query without its own kind
    /// term, or pressing one kind would leave the rail with only that kind on it.
    pub fn kinds_over(&self) -> String {
        let filter = self.filter.as_deref().filter(|f| !f.starts_with("kind:"));
        let typed = scour_query::without(&self.query, &["kind"]);
        scour_ui::query::compose(typed.as_deref().unwrap_or(&self.query), filter)
    }

    /// What the **strip** is asked about, which is not the same rows: the query
    /// without its age term, typed or pressed, or it filters itself to one bar.
    pub fn strip_over(&self) -> String {
        let filter = self.filter.as_deref().filter(|f| !f.starts_with("dm:"));
        let typed = scour_query::without(&self.query, &["dm"]);
        scour_ui::query::compose(typed.as_deref().unwrap_or(&self.query), filter)
    }

    fn ask(&mut self, offset: u32, limit: u32, cap: u32) -> Want {
        self.pages.asking(Pages::<Hit>::page_of(offset as usize));
        Want::Page {
            generation: self.generation,
            query: self.asking(),
            counting: self.kinds_over(),
            over: self.strip_over(),
            sort: self.sort,
            descending: self.descending,
            offset,
            limit,
            cap,
        }
    }

    /// Move the cursor by `by` rows, and the view with it.
    pub fn walk(&mut self, by: isize) -> Want {
        // Moved by hand: from here the cursor is about a file, not a place.
        self.anchored = true;
        let total = self.pages.total();
        if total == 0 {
            return Want::Nothing;
        }
        let last = total - 1;
        self.cursor = self.cursor.saturating_add_signed(by).min(last);
        self.settle();
        self.follow()
    }

    /// Take the list to where the scrollbar was dragged: `at` is which row of
    /// the track the pointer is on, out of `high`. The thumb follows the pointer.
    pub fn drag_bar(&mut self, at: u16, high: u16) -> Want {
        let total = self.pages.total();
        let last = total.saturating_sub(self.room);
        if last == 0 || high == 0 {
            return Want::Nothing;
        }
        self.top = (at as usize * last) / high.max(1) as usize;
        self.top = self.top.min(last);
        // The cursor comes along, or the next arrow key scrolls back to it.
        self.cursor = self.cursor.clamp(self.top, self.top + self.room - 1);
        self.dirty = true;
        self.follow()
    }

    /// Put the cursor at a row outright: `Home`, `End`, a mouse press.
    pub fn go(&mut self, row: usize) -> Want {
        // Moved by hand: from here the cursor is about a file, not a place.
        self.anchored = true;
        let total = self.pages.total();
        if total == 0 {
            return Want::Nothing;
        }
        self.cursor = row.min(total - 1);
        self.settle();
        self.follow()
    }

    /// Follow the row the cursor was on, wherever it went. Only the pages held
    /// are searched: a row outside them is one nobody is looking at.
    fn refollow(&mut self) {
        if !self.anchored {
            // Nobody chose a row: the cursor keeps its place in the list.
            return;
        }
        let at_cursor = self.pages.at(self.cursor).map(|h| h.path.clone());
        trace(&format!(
            "refollow: cursor {} holds {:?}, wants {:?}",
            self.cursor,
            at_cursor.as_deref().unwrap_or("—"),
            self.cursor_at.as_deref().unwrap_or("—")
        ));
        let Some(want) = self.cursor_at.clone() else {
            // Chosen before its page arrived, which a click can do. Take it now.
            self.cursor_at = at_cursor;
            return;
        };
        if self.pages.at(self.cursor).is_some_and(|h| h.path == want) {
            return;
        }
        let pages: Vec<usize> = self.pages.pages().collect();
        for page in pages {
            for i in 0..scour_page::SPAN {
                let row = page * scour_page::SPAN + i;
                if self.pages.at(row).is_some_and(|h| h.path == want) {
                    trace(&format!(
                        "found it at {row}, was {}; the view stays at {}",
                        self.cursor, self.top
                    ));
                    // The row moves, the view does not: moving both pins the
                    // row to its line and hides whatever arrived above it.
                    self.cursor = row;
                    self.settle();
                    return;
                }
            }
        }
        trace("not in any page held — the cursor stays where it is");
    }

    /// Keep the cursor on screen, moving the view the least it can. Not
    /// centred: text that moves under a still cursor is harder to read.
    fn settle(&mut self) {
        // What the cursor is on, noted only once somebody has moved it.
        if self.anchored {
            self.cursor_at = self.pages.at(self.cursor).map(|h| h.path.clone());
        }
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + self.room {
            self.top = self.cursor + 1 - self.room;
        }
        let total = self.pages.total();
        let most = total.saturating_sub(self.room);
        self.top = self.top.min(most);
        self.dirty = true;
    }

    /// The terminal changed size.
    pub fn resized(&mut self, room: usize) -> Want {
        if room == self.room {
            return Want::Nothing;
        }
        self.room = room.max(1);
        self.settle();
        self.follow()
    }

    /// A character typed into the query.
    pub fn insert(&mut self, c: char) -> Want {
        self.query.insert(self.caret, c);
        self.caret += c.len_utf8();
        self.typed()
    }

    /// Rub out the character before the caret.
    pub fn backspace(&mut self) -> Want {
        if self.caret == 0 {
            return Want::Nothing;
        }
        // Character by character, not byte by byte: cutting a byte off a
        // multi-byte character is a panic.
        let at = self.query[..self.caret]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.query.remove(at);
        self.caret = at;
        self.typed()
    }

    /// The rules arrived. Kept as the answer gave them.
    pub fn ruled(
        &mut self,
        added: Vec<(String, String)>,
        config: Vec<(String, String)>,
        builtin: Vec<(String, String)>,
        off: Vec<String>,
    ) {
        let is_off = |id: &str| off.iter().any(|o| o.eq_ignore_ascii_case(id));
        let mut out = Vec::new();
        for (group, removable) in [(added, true), (config, false), (builtin, false)] {
            for (kind, value) in group {
                let id = scour_settings::rule_id(&kind, &value);
                let off = is_off(&id);
                out.push((id, value, off, removable));
            }
        }
        self.rules = out;
        self.dirty = true;
    }

    /// Switch the rule under the panel's cursor off, or back on. Returns the
    /// whole switched-off list: the service replaces it outright.
    pub fn toggle_rule(&mut self) -> Option<Vec<String>> {
        let (_, _, off, _) = self.rules.get_mut(self.panel_at)?;
        *off = !*off;
        self.dirty = true;
        Some(
            self.rules
                .iter()
                .filter(|(_, _, off, _)| *off)
                .map(|(id, _, _, _)| id.clone())
                .collect(),
        )
    }

    /// Open a panel, or close the one that is open.
    pub fn show(&mut self, panel: Panel) {
        self.panel = if self.panel == panel {
            Panel::None
        } else {
            panel
        };
        self.panel_at = 0;
        self.note.clear();
        self.dirty = true;
    }

    /// Does this path sit on a volume that has stopped recording reads? The
    /// deepest mount wins, and nothing known answers no.
    pub fn frozen_atime(&self, path: &str) -> bool {
        let mut owner: Option<&scour_places::Mount> = None;
        for m in &self.mounts {
            let under = m.at == "/" || format!("{path}/").starts_with(&format!("{}/", m.at));
            if under && owner.is_none_or(|o| m.at.len() > o.at.len()) {
                owner = Some(m);
            }
        }
        owner.is_some_and(|m| !m.reads)
    }

    /// The line the cursor is on in the column panel: switch it, and keep the
    /// panel open — turning three columns on is three presses.
    pub fn pick_column(&mut self) -> Want {
        match scour_ui::COLUMNS.get(self.panel_at) {
            Some(c) => {
                let id = c.id;
                self.toggle_column(id);
            }
            // Past the end is the line that puts them all back.
            None => {
                self.columns = scour_ui::DEFAULT_COLUMNS
                    .iter()
                    .filter_map(|id| scour_ui::column(id))
                    .collect();
            }
        }
        Want::Remember(scour_settings::Change {
            columns: Some(self.columns.iter().map(|c| c.id.to_owned()).collect()),
            ..Default::default()
        })
    }

    /// Take the saved column list, dropping anything this build does not have.
    /// An empty result leaves the default: a table of no columns is not a table.
    pub fn columns_from(&mut self, saved: &[String]) {
        let shown: Vec<&'static scour_ui::Column> =
            saved.iter().filter_map(|id| scour_ui::column(id)).collect();
        if !shown.is_empty() {
            self.columns = shown;
        }
    }

    /// Switch one column on or off, putting it back where its neighbours expect
    /// it without moving them: rebuilding in table order loses an arrangement.
    pub fn toggle_column(&mut self, id: &str) {
        let showing = self.columns.iter().any(|c| c.id == id);
        if showing && self.columns.len() == 1 {
            return;
        }
        if let Some(at) = self.columns.iter().position(|c| c.id == id) {
            self.columns.remove(at);
            return;
        }
        let Some(col) = scour_ui::column(id) else {
            return;
        };
        let rank = |x: &str| scour_ui::COLUMNS.iter().position(|c| c.id == x);
        let at = self
            .columns
            .iter()
            .position(|c| rank(c.id) > rank(col.id))
            .unwrap_or(self.columns.len());
        self.columns.insert(at, col);
    }

    /// Build the menu for the row the cursor is on, and open it. What is in it
    /// is `scour-ui::menu`'s decision, asked as `Face::Terminal`.
    pub fn open_menu(&mut self) {
        let Some(hit) = self.here().cloned() else {
            return;
        };
        let picked = self.picked.len();
        let count = scour_ui::format::grouped(picked.max(1) as u64, self.mark().0);
        self.menu.clear();
        let mut last: Option<u8> = None;
        for item in scour_ui::menu::items_for(picked, hit.is_dir, scour_ui::faces::Face::Terminal) {
            self.menu.push(MenuLine {
                id: item.id.to_string(),
                label: self.say(item.msgid).replace("{n}", &count),
                key: item.key.to_string(),
                rule: last.is_some_and(|l| l != item.group),
                careful: item.weight == scour_ui::menu::Weight::Careful,
                heavy: item.weight == scour_ui::menu::Weight::Heavy,
            });
            last = Some(item.group);
        }
        self.panel = Panel::Menu;
        self.panel_at = 0;
        self.note.clear();
        self.dirty = true;
    }

    /// Which rows the menu is about: the selection, or the row under it.
    fn menu_rows(&self) -> Vec<String> {
        if self.picked.len() > 1 {
            self.picked.keys().cloned().collect()
        } else {
            self.here()
                .map(|h| vec![h.path.clone()])
                .unwrap_or_default()
        }
    }

    /// Do whatever the menu's cursor is on.
    pub fn menu_pick(&mut self) -> Want {
        let Some(line) = self.menu.get(self.panel_at).cloned() else {
            return Want::Nothing;
        };
        let rows = self.menu_rows();
        let Some(first) = rows.first().cloned() else {
            return Want::Nothing;
        };
        let is_dir = self.here().map(|h| h.is_dir).unwrap_or(false);
        let leaf = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
        let folder = |p: &str| scour_ui::path::folder(p).to_string();

        // Everything but the two that ask closes the menu on the press.
        if line.id != "trash" && line.id != "open-all" {
            self.panel = Panel::None;
            self.dirty = true;
        }

        match line.id.as_str() {
            "open" => {
                launch(&first);
                Want::Nothing
            }
            "folder" => {
                launch(&folder(&first));
                Want::Nothing
            }
            "folders" => {
                let mut seen: Vec<String> = rows.iter().map(|p| folder(p)).collect();
                seen.sort();
                seen.dedup();
                for dir in seen {
                    launch(&dir);
                }
                Want::Nothing
            }
            "clear" => {
                self.picked.clear();
                Want::Nothing
            }

            // The three that exist because there is an index.
            "search-here" => {
                let scope = if is_dir {
                    first.clone()
                } else {
                    folder(&first)
                };
                self.query = format!("under:{scope}");
                self.caret = self.query.len();
                self.typed()
            }
            "search-kind" => {
                let name = leaf(&first);
                match name.rfind('.').filter(|at| *at > 0) {
                    Some(at) => {
                        let ext = name[at + 1..].to_lowercase();
                        self.query = format!("ext:{ext}");
                        self.caret = self.query.len();
                        self.typed()
                    }
                    None => {
                        self.note = self.say("no extension to search for").into_owned();
                        Want::Nothing
                    }
                }
            }
            "duplicates" => Want::Report,
            "usage" => Want::Weigh(first.clone()),
            "skip" => {
                self.note = leaf(&first);
                self.show(Panel::Rules);
                Want::Nothing
            }

            "copy-path" | "copy-name" => {
                let text = if line.id == "copy-name" {
                    rows.iter().map(|p| leaf(p)).collect::<Vec<_>>().join("\n")
                } else {
                    rows.join("\n")
                };
                self.note = match scour_clip::text(&text) {
                    Ok(()) => self.say("path copied").into_owned(),
                    Err(e) => e.to_string(),
                };
                Want::Nothing
            }
            "copy-file" => {
                let paths: Vec<&std::path::Path> = rows.iter().map(std::path::Path::new).collect();
                self.note = match scour_clip::files(&paths) {
                    Ok(()) => self.say("path copied").into_owned(),
                    Err(e) => e.to_string(),
                };
                Want::Nothing
            }
            "details" => Want::Peek(first.clone()),
            "csv" => self.write_sheet(),

            "open-with" => {
                let name = leaf(&first);
                let mime = scour_thumbs::known::known().mime_of(&name).unwrap_or("");
                self.openers = scour_openers::openers(mime)
                    .into_iter()
                    .map(|o| {
                        (
                            o.id,
                            if o.preferred {
                                format!("★ {}", o.name)
                            } else {
                                o.name
                            },
                        )
                    })
                    .collect();
                if self.openers.is_empty() {
                    self.note = self.say("nothing on this machine claims it").into_owned();
                    return Want::Nothing;
                }
                self.panel = Panel::Openers;
                self.panel_at = 0;
                self.dirty = true;
                Want::Nothing
            }

            "rename" => {
                self.ask_title = self.say("Rename…").into_owned();
                self.ask_text = leaf(&first);
                self.ask_yes = self.say("Rename").into_owned();
                self.ask_typing = true;
                self.pending = Some(("rename".into(), vec![first.clone()]));
                self.panel = Panel::Ask;
                // The cursor sits on *Cancel* while the letters go into the
                // line above it: one keyboard, two places it could be typing.
                self.panel_at = 1;
                self.dirty = true;
                Want::Nothing
            }

            "trash" | "open-all" => {
                // Eight names and then how many are left: a list that runs off
                // the panel is one nobody read before pressing yes.
                let mut names: Vec<String> = rows.iter().take(8).map(|p| leaf(p)).collect();
                if rows.len() > 8 {
                    names.push(format!("… +{}", rows.len() - 8));
                }
                self.ask_title = format!("{}  —  {}", line.label, names.join(", "));
                self.ask_yes = self
                    .say(if line.id == "trash" { "Move" } else { "Open" })
                    .into_owned();
                self.ask_typing = false;
                self.pending = Some((line.id.clone(), rows));
                self.panel = Panel::Ask;
                // The cursor starts on "no": a terminal cannot dim what is
                // behind a question, so the cursor is what marks the safe answer.
                self.panel_at = 1;
                self.dirty = true;
                Want::Nothing
            }

            _ => {
                self.note = self.say("not in this face yet").into_owned();
                Want::Nothing
            }
        }
    }

    /// Start the program the openers list is on.
    pub fn open_with(&mut self) -> Want {
        let Some((id, _)) = self.openers.get(self.panel_at).cloned() else {
            return Want::Nothing;
        };
        self.panel = Panel::None;
        self.dirty = true;
        let Some(path) = self.here().map(|h| h.path.clone()) else {
            return Want::Nothing;
        };
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        let mime = scour_thumbs::known::known().mime_of(&name).unwrap_or("");
        if let Some(chosen) = scour_openers::openers(mime)
            .into_iter()
            .find(|o| o.id == id)
            && let Err(e) = scour_openers::launch(&chosen, std::path::Path::new(&path))
        {
            self.note = e.to_string();
        }
        Want::Nothing
    }

    /// Answer the question that is up: `0` is the line being typed into, `1` is
    /// *Cancel*, and only `2` is yes.
    pub fn ask_answer(&mut self, which: usize) -> Want {
        self.panel = Panel::None;
        self.dirty = true;
        let Some((what, paths)) = self.pending.take() else {
            return Want::Nothing;
        };
        // 0 is the line being typed into and 1 is *Cancel*; only 2 is yes.
        if which < 2 {
            return Want::Nothing;
        }
        let typing = std::mem::take(&mut self.ask_text);
        self.ask_typing = false;
        if what == "rename" {
            let Some(from) = paths.first() else {
                return Want::Nothing;
            };
            return match scour_name::rename(std::path::Path::new(from), &typing) {
                Ok(now) => Want::Recheck(vec![from.clone(), now.to_string_lossy().into_owned()]),
                Err(why) => {
                    self.note = self.say(why.msgid()).into_owned();
                    Want::Nothing
                }
            };
        }
        if what == "open-all" {
            for p in &paths {
                launch(p);
            }
            return Want::Nothing;
        }
        // The move is this process's. The service is only told to look again.
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
        self.note = match refused {
            Some(why) => why,
            None => self
                .say("{n} moved to the wastebasket")
                .replace("{n}", &gone.to_string()),
        };
        Want::Recheck(paths)
    }

    /// How many lines the open panel offers.
    pub fn panel_lines(&self) -> usize {
        match self.panel {
            Panel::Rules => self.rules.len(),
            Panel::Language => 2,
            Panel::Faces => 3,
            Panel::Menu => self.menu.len(),
            Panel::Openers => self.openers.len(),
            // Every column, and the line that goes back to the default.
            Panel::Columns => scour_ui::COLUMNS.len() + 1,
            Panel::Ask => 3,
            Panel::None => 0,
        }
    }

    /// Move the cursor inside whatever panel is open.
    pub fn panel_walk(&mut self, by: isize) {
        let lines = self.panel_lines();
        if lines == 0 {
            return;
        }
        self.panel_at = self
            .panel_at
            .saturating_add_signed(by)
            .min(lines.saturating_sub(1));
        self.dirty = true;
    }

    /// Speak this language from now on, by its place in `scour_i18n::LANGUAGES`:
    /// 0 is English, 1 is Turkish. Applied now and remembered for every face.
    pub fn speak(&mut self, which: usize) -> Want {
        let Some((tag, endonym)) = scour_i18n::LANGUAGES.get(which) else {
            return Want::Nothing;
        };
        // The words change now, not on the next start: immediate mode redraws
        // every cell from this struct, digits and their punctuation included.
        self.words = scour_i18n::Catalogue::for_language(tag);
        self.panel = Panel::None;
        self.note = (*endonym).to_string();
        self.dirty = true;
        Want::Remember(scour_settings::Change {
            language: Some((*tag).to_string()),
            ..Default::default()
        })
    }

    /// Start another face, and remember that it is the one to open: 0 is the
    /// window, 1 is this, 2 is the browser. Through `scour-open`.
    pub fn run_face(&mut self, which: usize) -> Want {
        let face = match which {
            0 => "window",
            1 => "tui",
            _ => "browser",
        };
        if face == "tui" {
            self.note = self.say("already running here").into_owned();
            self.dirty = true;
            return Want::Nothing;
        }
        let mut command = std::process::Command::new("scour-open");
        command.arg(face);
        // In a process group of its own, or closing this terminal closes what
        // it just opened. See `scour_ui::faces::detach`.
        scour_ui::faces::detach(&mut command);
        match command.spawn() {
            Ok(_) => {
                self.note = self.say("starting…").into_owned();
                // And this one goes: switching is moving, not opening a second.
                self.leaving = true;
            }
            Err(e) => self.note = format!("scour-open: {e}"),
        }
        self.panel = Panel::None;
        self.dirty = true;
        Want::Remember(scour_settings::Change {
            face: Some(face.to_string()),
            ..Default::default()
        })
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// An app with a screen to put rows on. `App::default()` has no room, and
    /// a view zero rows tall answers every scrolling question the same way.
    fn app(room: usize) -> App {
        App {
            room,
            ..Default::default()
        }
    }

    fn hits(from: usize, n: usize) -> Vec<Hit> {
        (from..from + n)
            .map(|i| {
                let path = format!("/x/{i}");
                Hit {
                    id: scour_core::EntryId::path_hash(scour_core::SourceId(0), &path),
                    path,
                    is_dir: false,
                    kind: scour_core::Kind::File,
                    meta: scour_core::Meta::UNKNOWN,
                    under: None,
                }
            })
            .collect()
    }

    fn reply(hits: Vec<Hit>, total: u64) -> scour_core::SearchResponse {
        scour_core::SearchResponse {
            hits,
            total,
            ..scour_core::SearchResponse::default()
        }
    }

    #[test]
    fn typing_asks_for_the_first_page_of_the_new_question() {
        let mut app = App::default();
        let want = app.insert('a');
        assert!(matches!(
            want,
            Want::Page {
                offset: 0,
                generation: 1,
                ..
            }
        ));
        assert_eq!(app.query, "a");
    }

    #[test]
    fn an_answer_to_an_older_keystroke_is_dropped() {
        let mut app = App::default();
        app.insert('a');
        app.insert('b');
        // The answer to `a`, arriving after `ab` went out.
        let want = app.landed(1, 0, 200, reply(hits(0, 200), 900));
        assert_eq!(want, Want::Nothing);
        assert_eq!(app.pages.total(), 0, "and nothing of it is kept");
    }

    #[test]
    fn the_cursor_pushes_the_view_the_least_it_can() {
        let mut app = app(10);
        app.insert('a');
        app.landed(1, 0, 200, reply(hits(0, 200), 1_000));
        app.go(0);
        assert_eq!(app.top, 0);
        app.walk(9);
        assert_eq!(app.top, 0, "still on screen");
        app.walk(1);
        assert_eq!(app.top, 1, "one row, not a page");
        app.go(0);
        assert_eq!(app.top, 0, "and back the same way");
    }

    #[test]
    fn the_view_never_shows_past_the_end() {
        let mut app = app(10);
        app.insert('a');
        app.landed(1, 0, 200, reply(hits(0, 40), 40));
        app.go(usize::MAX);
        assert_eq!(app.cursor, 39);
        assert_eq!(app.top, 30, "the last screenful, not past it");
    }

    #[test]
    fn a_short_page_is_the_end_of_the_result() {
        let mut app = app(10);
        app.insert('a');
        // Asked for 200, given 40: there is no more.
        app.landed(1, 0, 200, reply(hits(0, 40), 1_000));
        assert_eq!(app.pages.total(), 1_000, "the count is still the count");
        // And a page that fills is not the end.
        app.landed(1, 0, 200, reply(hits(0, 200), 5_000));
        assert_eq!(app.pages.total(), 5_000);
    }

    /// A row number is not an identity: the list shifts down by one and the
    /// cursor has to shift with it.
    #[test]
    fn the_cursor_stays_on_the_file_when_the_list_moves_under_it() {
        let mut app = app(10);
        app.insert('a');
        app.landed(1, 0, 200, reply(hits(0, 200), 1_000));
        app.go(3);
        let was = app.cursor_at.clone();
        assert_eq!(was.as_deref(), Some("/x/3"));

        // A file appears at the top: every row moves down one.
        let mut shifted = hits(0, 199);
        shifted.insert(0, hits(999, 1)[0].clone());
        app.landed(1, 0, 200, reply(shifted, 1_000));

        assert_eq!(app.cursor, 4, "the cursor moved with the row");
        assert_eq!(
            app.cursor_at.as_deref(),
            Some("/x/3"),
            "and it is the same file"
        );
        // The view stays where it is, so the row slides down a line and the
        // file that arrived is drawn above it.
        assert_eq!(app.top, 0);
        assert_eq!(app.cursor - app.top, 4, "one line further down the screen");
    }

    /// An untouched cursor stays at the top of the list: the other half of the
    /// rule, without which every file saved walks it down the screen.
    #[test]
    fn a_cursor_nobody_moved_belongs_to_the_list_rather_than_to_a_file() {
        let mut app = app(10);
        app.insert('a');
        app.landed(1, 0, 200, reply(hits(0, 200), 1_000));
        assert_eq!(app.cursor, 0);

        let mut shifted = hits(0, 199);
        shifted.insert(0, hits(999, 1)[0].clone());
        app.landed(1, 0, 200, reply(shifted, 1_000));

        assert_eq!(app.cursor, 0, "still the top");
        assert_eq!(app.top, 0, "and the newest row is on it");
        assert_eq!(
            app.pages.at(0).map(|h| h.path.as_str()),
            Some("/x/999"),
            "which is the file that just arrived"
        );
    }

    #[test]
    fn backspace_cuts_characters_rather_than_bytes() {
        let mut app = App::default();
        for c in "Değ".chars() {
            app.insert(c);
        }
        app.backspace();
        assert_eq!(app.query, "De", "and did not panic on the ğ");
    }
}

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
    /// looked up by value. It scans its own file: a msgid in a comment counts.
    fn asked_for() -> Vec<String> {
        const SOURCE: [&str; 6] = [
            include_str!("app.rs"),
            include_str!("draw.rs"),
            include_str!("keys.rs"),
            include_str!("link.rs"),
            include_str!("main.rs"),
            include_str!("icons.rs"),
        ];
        let mut out: Vec<String> = Vec::new();
        for text in SOURCE {
            let mut rest = text;
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
