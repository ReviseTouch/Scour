//! Everything the terminal knows, and the pure transitions on it.
//!
//! One struct, no drawing, no sockets: what happened goes in, what to ask the
//! service for comes out. That split is what makes this testable without a
//! terminal and without a service — the window has to photograph itself to
//! check anything, and this does not.

use scour_core::{Hit, SortKey};
use scour_page::{Change, Pages};

use crate::link::TYPING_CAP;

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
    /// True while the key list is over everything.
    pub helping: bool,
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
            helping: false,
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

    fn ask(&mut self, offset: u32, limit: u32, cap: u32) -> Want {
        self.pages.asking(Pages::<Hit>::page_of(offset as usize));
        Want::Page {
            generation: self.generation,
            query: self.query.clone(),
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
