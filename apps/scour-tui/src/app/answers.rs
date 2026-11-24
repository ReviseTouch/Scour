//! What the service answered, landing; the typing that asks; the cursor that
//! follows the rows.

use super::*;
use crate::link::TYPING_CAP;
use scour_core::Hit;
use scour_page::{Change, Pages};

impl App {
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
        if self.v2.grep_open && !self.v2.grep_pattern.is_empty() {
            return self.dispatch_grep(false);
        }
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
        if offset == 0 {
            self.similar = reply.similar.clone().filter(|_| reply.total == 0);
        }
        self.took_us = reply.took_us;
        self.rows_visited = reply.rows_visited;
        self.reading = reply.reading;
        self.turn = self.turn.wrapping_add(1);
        self.capped = reply.capped;
        self.trouble.clear();
        let page = Pages::<Hit>::page_of(offset as usize);
        let arrived = reply.hits.len();
        // A page short of both what was asked for and the largest page seen is
        // the end: both, because a new query's first page is short on purpose.
        let total = self.pages.length(page, arrived, limit as usize);
        // A reading's own count is the newest there is, and may be shorter
        // than the last: read again, it begins from nothing.
        let total = match reply.reading {
            Some(_) => reply.total as usize,
            None => total.max(reply.total as usize),
        };
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

    /// Nothing was listening, so one is being started.
    pub fn booting(&mut self) {
        self.note = self.say("Starting the Scour service…").into_owned();
        self.dirty = true;
    }

    /// That attempt is over; `why` is there when nothing is listening even so.
    pub fn booted(&mut self, why: Option<String>) {
        self.note.clear();
        if let Some(why) = why {
            // The reason first, then the hint the face used to print on its
            // own: the reason is the part that is new.
            self.trouble = format!("{why} — {}", self.say("Start one with `scourd`."));
        }
        self.dirty = true;
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

    pub(super) fn ask(&mut self, offset: u32, limit: u32, cap: u32) -> Want {
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
}
