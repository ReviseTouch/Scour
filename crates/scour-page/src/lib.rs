//! Which page of a result to hold, and which one to ask for next: a window onto
//! a list of millions that no face actually has.
//!
//! Policy only, no drawing. [`Pages`] is generic over whatever a row is to its
//! owner, and says what changed rather than telling anybody to redraw.

use std::collections::HashMap;

/// Rows per page, everywhere. The service caps a page at this, so asking for more
/// returns short — which a caller reads as the end of the list.
pub const SPAN: usize = scour_core::PAGE_ROWS as usize;

/// How many pages to keep before letting the least recently used go: 32 pages is
/// 6,400 rows, several screens either way of wherever somebody is.
pub const KEPT: usize = 32;

/// A page that has arrived, and what the index looked like when it did.
#[derive(Debug)]
struct Held<T> {
    rows: Vec<T>,
    revision: u64,
    /// Read again and no longer: whatever it is short of, the service does
    /// not have. Keeps a page that is short for good from being asked forever.
    settled: bool,
}

/// What a call changed, for a caller that has to tell a view about it. Said and
/// not done: every face notifies its view differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Nothing arrived that was not already here.
    Nothing,
    /// These rows are new or different: a half-open range.
    Rows { from: usize, to: usize },
    /// The result is a different length than it was — and, when a page arrived in
    /// the same call, which rows that page replaced. `rows` is `None` only when
    /// the length moved on its own, with no page behind it.
    Length {
        was: usize,
        now: usize,
        rows: Option<(usize, usize)>,
    },
}

/// The pages of one result, and the rules about which ones to have.
#[derive(Debug)]
pub struct Pages<T> {
    held: HashMap<usize, Held<T>>,
    /// Least recently used first. What eviction reads.
    order: Vec<usize>,
    /// How long the result is, which is what a view is sized from.
    total: usize,
    /// The page a fetch is out for, and the revision it was asked at.
    asked: Option<(usize, u64)>,
    /// What "up to date" means now.
    revision: u64,
    /// The most rows a page has ever actually held.
    served: usize,
}

impl<T> Default for Pages<T> {
    fn default() -> Self {
        Pages {
            held: HashMap::new(),
            order: Vec::new(),
            total: 0,
            asked: None,
            revision: 0,
            served: 0,
        }
    }
}

impl<T> Pages<T> {
    /// Which page a row belongs to.
    pub fn page_of(row: usize) -> usize {
        row / SPAN
    }

    /// The first row of a page.
    pub fn start_of(page: usize) -> usize {
        page * SPAN
    }

    /// How long the result is, as far as this knows.
    pub fn total(&self) -> usize {
        self.total
    }

    /// The most rows any page has actually carried — not [`SPAN`]: a caller that
    /// asked for 256 and got the service's 200 would read every page as the last.
    pub fn served(&self) -> usize {
        self.served.max(1)
    }

    /// Is this page here, with rows in it?
    pub fn holds(&self, page: usize) -> bool {
        self.held.get(&page).is_some_and(|h| !h.rows.is_empty())
    }

    /// The row at this position, if the page holding it is here.
    pub fn at(&self, row: usize) -> Option<&T> {
        let page = Self::page_of(row);
        self.held.get(&page)?.rows.get(row - Self::start_of(page))
    }

    /// The pages belong to a different question. Drop them — but not the length
    /// or the observed page size: a view sized from zero collapses and springs
    /// back. [`Pages::set_total`] is what changes the length.
    pub fn empty(&mut self) {
        self.held.clear();
        self.order.clear();
        self.asked = None;
    }

    /// What the index looks like now. A page fetched before this is stale, and
    /// [`Pages::next_page`] offers to refetch it when the caller says there is time.
    pub fn mark(&mut self, revision: u64) {
        self.revision = revision;
    }

    /// Note that a fetch has gone out, so nothing else is asked for meanwhile.
    pub fn asking(&mut self, page: usize) {
        self.asked = Some((page, self.revision));
    }

    /// Forget an outstanding fetch: the service died, or nobody wants that query.
    pub fn forget_asking(&mut self) {
        self.asked = None;
    }

    /// The page a fetch is out for.
    pub fn asked(&self) -> Option<usize> {
        self.asked.map(|(page, _)| page)
    }

    /// File a page that has arrived; `total` is the whole result's length. Returns
    /// what changed. The page number comes from the answer and not from what was
    /// last requested, or a scroll mid-fetch files the rows one page out.
    pub fn put(&mut self, page: usize, rows: Vec<T>, total: usize) -> Change {
        self.served = self.served.max(rows.len());
        let count = rows.len();
        // Stamped with the revision the request went out at, not the current
        // one: an index that moved in flight has not been read yet.
        let revision = match self.asked {
            Some((out, revision)) if out == page => revision,
            _ => self.revision,
        };
        // The same length twice is all there is, whatever the count said.
        let settled = self.held.get(&page).is_some_and(|h| h.rows.len() >= count);
        self.held.insert(
            page,
            Held {
                rows,
                revision,
                settled,
            },
        );
        if self.asked.map(|(out, _)| out) == Some(page) {
            self.asked = None;
        }
        self.touch(page);
        self.evict();
        let was = self.total;
        let from = Self::start_of(page);
        let to = from + count;
        if total != was {
            self.total = total;
            return Change::Length {
                was,
                now: total,
                rows: Some((from, to)),
            };
        }
        Change::Rows { from, to }
    }

    /// Say how long the result is without having a page to go with it. A counting
    /// cap is not a length: an interactive search stops counting at a thousand.
    pub fn set_total(&mut self, total: usize) -> Change {
        let was = self.total;
        if total == was {
            return Change::Nothing;
        }
        self.total = total;
        Change::Length {
            was,
            now: total,
            rows: None,
        }
    }

    /// Which page to ask for, if any. `first..=last` is what the eye can see;
    /// `speculate` allows one page either side, `refresh` a page gone stale.
    pub fn next_page(
        &self,
        first: usize,
        last: usize,
        speculate: bool,
        refresh: bool,
    ) -> Option<usize> {
        if self.total == 0 {
            return None;
        }
        // One request at a time, always for where the eye is now: a thrown
        // scrollbar crosses a page per frame and would queue sixty requests.
        if self.asked.is_some() {
            return None;
        }
        let end = (self.total - 1) / SPAN;
        let from = Self::page_of(first.min(self.total - 1));
        let to = Self::page_of(last.min(self.total - 1));
        for page in from..=to {
            // The last row of this page the eye can see, within the result.
            let seen = last
                .min(Self::start_of(page) + SPAN - 1)
                .min(self.total - 1);
            match self.held.get(&page) {
                None => return Some(page),
                Some(held) if held.rows.is_empty() => return Some(page),
                // A new question's first page is cut to a screenful; the rows
                // past it are in the result and not in the page.
                Some(held) if !held.settled && seen >= Self::start_of(page) + held.rows.len() => {
                    return Some(page);
                }
                Some(held) if refresh && held.revision != self.revision => return Some(page),
                Some(_) => {}
            }
        }
        // Ahead, then behind, and only while a page is cheap: deep in a long
        // result a page costs the service a walk of everything above it.
        if !speculate {
            return None;
        }
        let near = [(to < end).then_some(to + 1), from.checked_sub(1)];
        near.into_iter().flatten().find(|page| !self.holds(*page))
    }

    /// How long the list is to draw, given a page that has just arrived. A short
    /// page ends the result only if it is short by both measures — shorter than
    /// what was asked for and than the most this service has ever given.
    pub fn length(&self, page: usize, arrived: usize, asked_for: usize) -> usize {
        if arrived < asked_for && arrived < self.served() {
            Self::start_of(page) + arrived
        } else {
            self.total
        }
    }

    /// Note that a row was read, so eviction knows what is being looked at.
    pub fn touched(&mut self, row: usize) {
        let page = Self::page_of(row);
        if self.held.contains_key(&page) {
            self.touch(page);
        }
    }

    fn touch(&mut self, page: usize) {
        if let Some(at) = self.order.iter().position(|p| *p == page) {
            self.order.remove(at);
        }
        self.order.push(page);
    }

    fn evict(&mut self) {
        while self.order.len() > KEPT {
            let oldest = self.order.remove(0);
            // Never the page a fetch is out for: it is about to be written.
            if self.asked.map(|(p, _)| p) == Some(oldest) {
                self.order.push(oldest);
                break;
            }
            self.held.remove(&oldest);
        }
    }

    /// The pages here, oldest use first, for a caller walking only what it holds.
    pub fn pages(&self) -> impl Iterator<Item = usize> + '_ {
        self.order.iter().copied()
    }

    /// The rows of a page, for a caller that has to rewrite them in place.
    pub fn rows_mut(&mut self, page: usize) -> Option<&mut [T]> {
        self.held.get_mut(&page).map(|h| h.rows.as_mut_slice())
    }
}

#[cfg(test)]
mod tests;
