//! Which column the list is sorted by, and asking again when it changes.

use super::*;
use crate::link::TYPING_CAP;
use scour_core::SortKey;

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
    /// The first press sorts, the second reverses; a date or a size starts
    /// newest or largest first, a word from A, as in every face.
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
        self.descending = key.starts_descending();
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

    /// Start in the order the settings hold, whichever face chose it.
    pub fn order_from(&mut self, kept: &scour_settings::Settings) {
        self.sort = SortKey::named(&kept.sort).unwrap_or_default();
        self.descending = kept.descending.unwrap_or(true);
        self.kept_order = (self.sort, self.descending);
    }

    /// The order, for the settings, when it is not the one they hold: a column
    /// picked by key or by mouse is kept for every face, as the page keeps it.
    pub fn order_to_keep(&mut self) -> Option<scour_settings::Change> {
        let now = (self.sort, self.descending);
        if now == self.kept_order {
            return None;
        }
        self.kept_order = now;
        Some(scour_settings::Change {
            sort: Some(self.sort.name().into()),
            descending: Some(self.descending),
            ..Default::default()
        })
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
        if self.v2.grep_open && !self.v2.grep_pattern.is_empty() {
            return self.dispatch_grep(false);
        }
        self.ask(0, scour_page::SPAN as u32, TYPING_CAP)
    }
}
