//! The rail and the strip: their lines, what a press on them does, the filters
//! they write.

use super::*;

impl App {
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
    pub fn tools(&self) -> [(&'static str, &'static str); 8] {
        [
            ("settings", "F5"),
            ("lang", "^L"),
            ("skips", "^K"),
            // Where the window has a `⋮` at the end of the header row.
            ("columns", "^T"),
            ("csv", "^E"),
            // Beside the peek key: both are about the row the cursor is on.
            ("menu", "F4"),
            ("keys", "F1"),
            ("disks", "^D"),
        ]
    }

    /// Press one of them — the same thing its key does.
    pub fn tool(&mut self, which: usize) -> Want {
        match which {
            0 | 1 => self.open_settings(Panel::General),
            2 => self.open_settings(Panel::Rules),
            3 => self.open_settings(Panel::Columns),
            4 => self.write_sheet(),
            5 => {
                self.open_menu();
                Want::Nothing
            }
            7 => self.open_settings(Panel::Disks),
            _ => {
                self.helping = !self.helping;
                self.dirty = true;
                Want::Nothing
            }
        }
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

    /// How many of the menu's items sit in the strip across its top.
    pub fn strip_len(&self) -> usize {
        self.menu.iter().take_while(|m| m.strip).count()
    }
}
