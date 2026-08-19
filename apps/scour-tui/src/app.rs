//! Everything the terminal knows, and the pure transitions on it.
//!
//! One struct, no drawing, no sockets: what happened goes in, what to ask the
//! service for comes out. That split is what makes this testable without a
//! terminal and without a service — the window has to photograph itself to
//! check anything, and this does not.

use scour_core::{Hit, SortKey};
use scour_page::{Change, Pages};

use crate::link::TYPING_CAP;

/// What is over the list, if anything.
///
/// One at a time, and the same rule the window follows: a second panel behind
/// the first is a panel nobody can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    None,
    /// What the walk skips, and which of those are switched off.
    Rules,
    /// Which of the two languages to speak.
    Language,
    /// Window, terminal, browser.
    Faces,
}

/// Which of the two the bare letters go to.
///
/// **Search is where it starts, and that is the whole argument for having
/// modes at all being a small one.** Everything opens a terminal expecting to
/// type; a normal mode nobody asked for would meet them with a beep. So the
/// keys that move are always available with the arrows, and the letters that
/// move are a mode somebody chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Letters go into the query.
    Search,
    /// Letters move: `j k g G`, and the rest of the map in `keys`.
    Move,
}

/// What a step wants done about the service.
///
/// Returned rather than done, for the reason [`scour_page::Change`] is: the
/// event loop owns the socket, and a state machine that reached for it could
/// not be stepped in a test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// Nothing.
    Nothing,
    /// A page of the current query.
    Page {
        generation: u64,
        query: String,
        sort: SortKey,
        descending: bool,
        offset: u32,
        limit: u32,
        cap: u32,
    },
    /// Ask what the walk skips.
    Rules,
    /// Replace the list of switched-off rules.
    OffRules(Vec<String>),
    /// Remember a preference.
    Remember(scour_settings::Change),
    /// Write the whole result to this file.
    Export { query: String, to: String },
    /// Close the terminal.
    Leave,
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
    /// The rows somebody has picked, by path, and what they weigh.
    ///
    /// **By path rather than by row number.** A row number means nothing
    /// across a re-sort or a new query, and a selection that survives neither
    /// is not a selection anybody can act on.
    pub picked: std::collections::BTreeMap<String, i64>,
    /// Where a run of `Shift` presses started.
    pub anchor: usize,
    /// Which panel is over everything, if any.
    pub panel: Panel,
    /// Where the cursor is inside the open panel.
    pub panel_at: usize,
    /// The skip rules, as the service last reported them: three groups and
    /// what is switched off. **Kept from the answer**, because deleting one
    /// means sending the list without it, and a window that has not been told
    /// what is in the list cannot take anything out of it.
    pub rules: Vec<(String, String, bool, bool)>,
    /// What was said about the last thing done — a file written, a language
    /// changed. Cleared by the next keystroke.
    pub note: String,
    /// True while the key list is over everything.
    pub helping: bool,
    /// The rail: what the matching rows are made of, and where they live.
    pub kinds: Vec<(String, u64)>,
    pub places: Vec<(String, String)>,
    /// The twenty-four bars of the time strip, oldest first, and the day
    /// each of them stands for.
    pub strip: Vec<(u32, u64)>,
    /// Which filter is in force, if any — `kind:code`, `under:"…"`, `dm:7d`.
    pub filter: Option<String>,
    /// Whether the rail is on screen. Off under eighty columns, where it
    /// would take a third of the list.
    pub rail: bool,
    /// True while the arrows move in the rail rather than the list.
    pub in_rail: bool,
    /// Which line of the rail the cursor is on.
    pub rail_at: usize,
    /// Set when a redraw is owed. **Nothing is drawn without one** — a
    /// terminal that redraws on a timer burns a core doing nothing.
    pub dirty: bool,
    pub leaving: bool,
}

impl Default for App {
    fn default() -> Self {
        App {
            query: String::new(),
            caret: 0,
            mode: Mode::Search,
            generation: 0,
            pages: Pages::default(),
            cursor: 0,
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
            panel: Panel::None,
            panel_at: 0,
            rules: Vec::new(),
            note: String::new(),
            helping: false,
            kinds: Vec::new(),
            places: Vec::new(),
            strip: Vec::new(),
            filter: None,
            rail: true,
            in_rail: false,
            rail_at: 0,
            dirty: true,
            leaving: false,
        }
    }
}

/// The keys a column can be sorted by, in the order the columns are drawn.
///
/// The same five the window's headings offer, so that a list sorted in one
/// face and then opened in another is in the same order.
pub const SORTS: [(SortKey, &str); 5] = [
    (SortKey::Name, "name"),
    (SortKey::Kind, "kind"),
    (SortKey::Path, "path"),
    (SortKey::Modified, "changed"),
    (SortKey::Size, "size"),
];

impl App {
    /// What the sort is called, for the meter.
    pub fn sort_name(&self) -> &'static str {
        SORTS
            .iter()
            .find(|(key, _)| *key == self.sort)
            .map(|(_, name)| *name)
            .unwrap_or("relevance")
    }

    /// Sort by the next column along, or the previous one.
    ///
    /// **The query stays and the selection stays**; only the order changes.
    /// Re-asking is unavoidable — the order is the service's — but a re-sort
    /// that emptied the selection would make sorting something people avoid.
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
        self.trouble.clear();
        // **A new question, a new selection.** What was picked belongs to the
        // rows that were on screen; carrying it into a different result means
        // acting later on files somebody cannot see.
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
        // **An answer to an older keystroke is dropped, not drawn.** It is the
        // list going backwards under somebody's hands otherwise.
        if generation != self.generation {
            return Want::Nothing;
        }
        self.took_us = reply.took_us;
        self.rows_visited = reply.rows_visited;
        self.capped = reply.capped;
        self.trouble.clear();
        let page = Pages::<Hit>::page_of(offset as usize);
        let arrived = reply.hits.len();
        // A page short of both what was asked for and the most this service
        // has ever given is the end of the result. Both conditions, because
        // the first page of a new query is deliberately short.
        let total = self.pages.length(page, arrived, limit as usize);
        let total = total.max(reply.total as usize);
        match self.pages.put(page, reply.hits, total) {
            Change::Nothing => {}
            _ => self.dirty = true,
        }
        self.follow()
    }

    /// The rail's counts arrived.
    pub fn counted(&mut self, generation: u64, reply: scour_core::FacetResponse) {
        if generation != self.generation {
            return;
        }
        for group in reply.groups {
            match group.by {
                scour_core::FacetBy::Kind => {
                    self.kinds = group
                        .facets
                        .into_iter()
                        .filter(|f| f.count > 0)
                        .map(|f| (f.key, f.count))
                        .collect();
                }
                scour_core::FacetBy::Age { .. } => {
                    // **Every band, including the empty ones.** The answer
                    // leaves out bands nothing fell into, and a strip built
                    // from what came back has a different number of bars for
                    // every query — so the axis stops meaning anything and two
                    // strips cannot be compared. The bands are ours to begin
                    // with; the answer only fills them.
                    //
                    // `older` is the overflow and is not a bar: it is
                    // everything before the axis starts.
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

    /// Everything the rail offers, in the order it is drawn: what it says and
    /// what pressing it asks for.
    ///
    /// **One list rather than three sections walked separately**, because the
    /// cursor moves down all of it and a section boundary is a blank line, not
    /// a place to get stuck.
    pub fn rail_lines(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .kinds
            .iter()
            .take(9)
            .map(|(token, _)| (token.clone(), scour_ui::query::of_kind(token)))
            .collect();
        out.extend(
            self.places
                .iter()
                .take(6)
                .map(|(label, path)| (label.clone(), scour_ui::query::of_place(path))),
        );
        out.extend(
            scour_ui::query::SIZES
                .iter()
                .map(|(label, term)| ((*label).to_string(), (*term).to_string())),
        );
        out
    }

    /// Which of the rail's offers is drawn on this line of it, if any.
    ///
    /// The rail has headings and blank lines between its sections, and they
    /// are not stops — a press on `KIND` should do nothing rather than press
    /// whatever is nearest. The shape has to agree with `draw::side`, and this
    /// is the one place that knows it.
    pub fn rail_hit(&self, line: usize) -> Option<usize> {
        let kinds = self.kinds.len().min(9);
        let places = self.places.len().min(6);
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

    /// Press a filter in the rail, or press the one in force to clear it.
    pub fn press_filter(&mut self, term: &str) -> Want {
        self.filter = scour_ui::query::pressed(self.filter.as_deref(), term);
        // **The rail keeps what it is showing until new counts arrive.**
        // Emptying it moves every line under the cursor, so the next press
        // lands on something nobody aimed at — and a rail that blinks empty
        // after every press is one people stop trusting.
        self.typed()
    }

    /// The service said no.
    pub fn upset(&mut self, generation: u64, why: String) -> Want {
        if generation != self.generation {
            return Want::Nothing;
        }
        self.pages.forget_asking();
        self.trouble = why;
        self.dirty = true;
        Want::Nothing
    }

    /// Ask for whatever the eye is missing, if anything.
    pub fn follow(&mut self) -> Want {
        let first = self.top;
        let last = (self.top + self.room).saturating_sub(1);
        // Speculating is free here in a way it is not in the window: a page is
        // two hundred rows and the terminal holds thirty-two of them.
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

    fn ask(&mut self, offset: u32, limit: u32, cap: u32) -> Want {
        self.pages.asking(Pages::<Hit>::page_of(offset as usize));
        Want::Page {
            generation: self.generation,
            query: self.asking(),
            sort: self.sort,
            descending: self.descending,
            offset,
            limit,
            cap,
        }
    }

    /// Move the cursor by `by` rows, and the view with it.
    pub fn walk(&mut self, by: isize) -> Want {
        let total = self.pages.total();
        if total == 0 {
            return Want::Nothing;
        }
        let last = total - 1;
        self.cursor = self.cursor.saturating_add_signed(by).min(last);
        self.settle();
        self.follow()
    }

    /// Put the cursor at a row outright: `Home`, `End`, a mouse press.
    pub fn go(&mut self, row: usize) -> Want {
        let total = self.pages.total();
        if total == 0 {
            return Want::Nothing;
        }
        self.cursor = row.min(total - 1);
        self.settle();
        self.follow()
    }

    /// Keep the cursor on screen, moving the view the least it can.
    ///
    /// **Not centred.** A list that recentres on every step makes the text
    /// move while the cursor stands still, which is much harder to read than
    /// the other way round.
    fn settle(&mut self) {
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
        // Character by character, not byte by byte: `Değişiklik` is ten
        // characters and thirteen bytes, and cutting a byte off the end of one
        // of them is a panic.
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

    /// Switch the rule under the panel's cursor off, or back on.
    ///
    /// Returns the whole switched-off list to send: the service replaces it
    /// outright, and a list built from what this window has pressed rather
    /// than from what the service said would switch every other rule on.
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

    /// How many lines the open panel offers.
    pub fn panel_lines(&self) -> usize {
        match self.panel {
            Panel::Rules => self.rules.len(),
            Panel::Language => 2,
            Panel::Faces => 3,
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

    /// Speak this language from now on: 0 is Turkish, 1 is English.
    ///
    /// **Remembered rather than applied here.** The catalogue is read once at
    /// startup and the strings in this interface are few and English; what
    /// this changes is what every face opens in next.
    pub fn speak(&mut self, which: usize) -> Want {
        let tag = if which == 0 { "tr" } else { "en" };
        self.note = format!("language: {tag} — takes effect on the next start");
        self.dirty = true;
        Want::Remember(scour_settings::Change {
            language: Some(tag.to_string()),
            ..Default::default()
        })
    }

    /// Start another face, and remember that it is the one to open.
    ///
    /// 0 is the window, 1 is this, 2 is the browser. **Through the launcher**,
    /// which owns the list of terminals and the rule about which face opens by
    /// default.
    pub fn run_face(&mut self, which: usize) -> Want {
        let face = match which {
            0 => "window",
            1 => "tui",
            _ => "browser",
        };
        if face == "tui" {
            self.note = "already running here".into();
            self.dirty = true;
            return Want::Nothing;
        }
        let started = std::process::Command::new("scour-open")
            .arg(face)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        self.note = match started {
            Ok(_) => format!("starting the {face}"),
            Err(e) => format!("scour-open: {e}"),
        };
        self.panel = Panel::None;
        self.dirty = true;
        Want::Remember(scour_settings::Change {
            face: Some(face.to_string()),
            ..Default::default()
        })
    }

    /// Write the whole result to a spreadsheet in the download folder.
    ///
    /// **Where downloads go, and said outright.** A file appearing silently in
    /// somebody's home is a file they find a week later; the window learned
    /// that one the same way.
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
        self.note = format!("writing {to}…");
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
        let mut app = App::default();
        app.room = 10;
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
        let mut app = App::default();
        app.room = 10;
        app.insert('a');
        app.landed(1, 0, 200, reply(hits(0, 40), 40));
        app.go(usize::MAX);
        assert_eq!(app.cursor, 39);
        assert_eq!(app.top, 30, "the last screenful, not past it");
    }

    #[test]
    fn a_short_page_is_the_end_of_the_result() {
        let mut app = App::default();
        app.room = 10;
        app.insert('a');
        // Asked for 200, given 40: there is no more.
        app.landed(1, 0, 200, reply(hits(0, 40), 1_000));
        assert_eq!(app.pages.total(), 1_000, "the count is still the count");
        // And a page that fills is not the end.
        app.landed(1, 0, 200, reply(hits(0, 200), 5_000));
        assert_eq!(app.pages.total(), 5_000);
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
