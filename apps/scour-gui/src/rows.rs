//! Turning a reply into what the window draws.
//!
//! Everything here is formatting, and it is all on this side of the language
//! boundary on purpose. A `.slint` file that formats a size has to know about
//! binary units; one that computes a highlight has to fold Turkish text. Both
//! would be a second implementation of something that
//! already exists in `scour-core`, and the two would drift.

use std::cell::{Cell, RefCell};

use humansize::{BINARY, format_size};
use scour_core::{Hit, Kind, text::Folder};

use crate::Row;

const DAY: i64 = 86_400;

/// Which of the six age bands a timestamp falls in.
///
/// The same bands the disk-usage report uses, and for the same reason: how old
/// a thing is answers "is this what I was just working on" faster than a date
/// does, and it does it in six pixels.
pub fn band(now: i64, mtime: i64) -> i32 {
    match now - mtime {
        a if a < DAY => 0,
        a if a < 7 * DAY => 1,
        a if a < 30 * DAY => 2,
        a if a < 180 * DAY => 3,
        a if a < 365 * DAY => 4,
        _ => 5,
    }
}

/// A name cut into what precedes the match, the match, and what follows.
///
/// Split here rather than in the interface, and not only because Slint has no
/// substring: the search runs on **folded** text, and Turkish folding changes
/// byte lengths — `İ` is two bytes and folds to one — so an offset found in
/// folded text cannot be applied to the original spelling. `fold_indexed` is
/// the function that maps it back, it lives in `scour-core`, and its doc
/// comment says this is what it is for.
pub fn split_at_match<'a>(name: &'a str, terms: &[String]) -> (&'a str, &'a str, &'a str) {
    let folder = scour_core::text::DefaultFolder;
    let (folded, back) = folder.fold_indexed(name);
    let mut best: Option<(usize, usize)> = None;
    for term in terms {
        let needle = folder.fold(term);
        if needle.is_empty() {
            continue;
        }
        if let Some(at) = folded.find(&needle) {
            // The earliest match, and the longest among those: a query of two
            // terms should light up the one the eye lands on first.
            let end = at + needle.len();
            let cand = (at, end);
            if best.is_none_or(|(b_at, b_end)| (at, end - at) < (b_at, b_end - b_at)) {
                best = Some(cand);
            }
        }
    }
    let Some((from, to)) = best else {
        return (name, "", "");
    };
    // Folded byte offset → the same place in the original spelling. Clamped
    // and pushed to a character boundary, because a slice that lands mid
    // character is a panic and a name is arbitrary bytes from a disk.
    let mut a = (back.get(from).copied().unwrap_or(0) as usize).min(name.len());
    let mut b = (back.get(to).copied().unwrap_or(name.len() as u32) as usize).min(name.len());
    while a > 0 && !name.is_char_boundary(a) {
        a -= 1;
    }
    while b < name.len() && !name.is_char_boundary(b) {
        b += 1;
    }
    if a > b {
        return (name, "", "");
    }
    (&name[..a], &name[a..b], &name[b..])
}

/// One hit, formatted.
/// The colour a kind's icon is drawn in, or the window's quiet ink when the
/// kind has none — a plain file is not a category worth a hue.
fn tint_of(token: &str) -> slint::Brush {
    match scour_ui::kind_colour(token) {
        Some(c) => {
            let (a, r, g, b) = c.argb();
            slint::Brush::SolidColor(slint::Color::from_argb_u8(a, r, g, b))
        }
        None => {
            let (a, r, g, b) = scour_ui::DARK.ink_3.argb();
            slint::Brush::SolidColor(slint::Color::from_argb_u8(a, r, g, b))
        }
    }
}

pub fn row_of(h: &Hit, terms: &[String], now: i64, kind: &str, fresh: bool) -> Row {
    let (pre, hit, post) = split_at_match(h.name(), terms);
    Row {
        pre: pre.into(),
        hit: hit.into(),
        post: post.into(),
        folder: h.parent().into(),
        kind: kind.into(),
        fresh,
        ktoken: h.kind.token().into(),
        tint: tint_of(h.kind.token()),
        size: if h.is_dir {
            slint::SharedString::new()
        } else {
            format_size(h.meta.size.max(0) as u64, BINARY).into()
        },
        stamp: stamp(h.meta.mtime).into(),
        is_dir: h.is_dir,
        age: band(now, h.meta.mtime),
    }
}

/// `YYYY-MM-DD HH:MM`, in UTC.
///
/// The same choice the CLI makes and for the same reason: local time needs the
/// zone database, and a listing is read for ordering far more often than for
/// the exact minute. A window will want a real clock eventually; this is not
/// the thing to stop and get right before it opens.
pub fn stamp(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let days = secs.div_euclid(DAY);
    let rest = secs.rem_euclid(DAY);
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60
    )
}

/// Days since the epoch to a calendar date. Howard Hinnant's `civil_from_days`.
fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The kinds the rail offers, in the order it shows them.
pub fn offered_kinds() -> &'static [Kind] {
    &Kind::OFFERED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_split_lands_on_the_original_spelling_not_the_folded_one() {
        // `Değişiklik` is ten characters and thirteen bytes. An offset taken
        // from the folded text and used unchanged would cut somewhere else —
        // and could cut a character in half, which is a panic.
        let (pre, hit, post) = split_at_match("Değişiklik Raporu.txt", &["raporu".into()]);
        assert_eq!(pre, "Değişiklik ");
        assert_eq!(hit, "Raporu");
        assert_eq!(post, ".txt");
    }

    #[test]
    fn folding_finds_what_a_turkish_keyboard_typed() {
        // The case the fold exists for: `İ` is two bytes and folds to one.
        let (pre, hit, post) = split_at_match("İSTANBUL.pdf", &["istanbul".into()]);
        assert_eq!((pre, hit, post), ("", "İSTANBUL", ".pdf"));
    }

    #[test]
    fn a_term_that_is_not_there_lights_nothing_up() {
        assert_eq!(
            split_at_match("main.rs", &["zzz".into()]),
            ("main.rs", "", "")
        );
        assert_eq!(split_at_match("main.rs", &[]), ("main.rs", "", ""));
    }

    #[test]
    fn the_earliest_match_wins_when_several_terms_hit() {
        let (pre, hit, _) = split_at_match("rapor-belge.pdf", &["belge".into(), "rapor".into()]);
        assert_eq!((pre, hit), ("", "rapor"));
    }

    #[test]
    fn a_name_that_is_not_text_does_not_panic() {
        // Names come off a disk and are arbitrary bytes; this one is what a
        // lossy conversion leaves behind.
        let odd = "caf\u{fffd}\u{301}.txt";
        let (pre, hit, post) = split_at_match(odd, &["caf".into()]);
        assert_eq!(format!("{pre}{hit}{post}"), odd);
    }

    #[test]
    fn stamps_are_the_dates_they_claim_to_be() {
        assert_eq!(stamp(1_769_817_600), "2026-01-31 00:00");
        assert_eq!(stamp(0), "");
    }

    #[test]
    fn the_bands_run_from_today_to_older() {
        let now = 1_800_000_000;
        assert_eq!(band(now, now), 0);
        assert_eq!(band(now, now - 3 * DAY), 1);
        assert_eq!(band(now, now - 400 * DAY), 5);
        // A timestamp in the future is today, not older than everything.
        assert_eq!(band(now, now + DAY), 0);
    }
}

/// The list, as a model the view pulls from rather than a vector it is handed.
///
/// **Why this shape.** A `VecModel` holds every row the view can show, so a
/// window over five million results has to be a sliding window — and then the
/// scrollbar measures the window, scrolling past its edge shows blank, and
/// every fetch has to move the viewport back to where the eye was.
///
/// `Model` inverts it: `row_count` is the real total, so the view sizes itself
/// and its scrollbar correctly and asks for exactly the rows it is about to
/// draw. `row_data` answers from the loaded window, and a row that has not
/// arrived is drawn empty for one frame rather than left as a hole.
///
/// Taken from `Hukuk-Dosyalar`'s `RowsModel`, which does the same thing over a
/// store that is already in memory.
///
/// ## What the notifications have to be
///
/// This model is refreshed while somebody is looking at it — every time the
/// index moves, and every time a page arrives — and **how** it says so decides
/// whether the list stays still. [`slint::ModelNotify::reset`] means *the
/// whole thing changed*: the view throws its elements away, rebuilds them, and
/// re-clamps a viewport it has just re-measured. Doing that on every reply is
/// what made scrolling jump, flash, and land back at the top.
///
/// So a reset happens for one reason only — the list got longer or shorter, so
/// the view really does have to re-measure. A page landing in a list of
/// unchanged length is [`slint::ModelNotify::row_changed`] over the rows that
/// actually differ, which leaves the viewport, the scrollbar and every element
/// outside those rows exactly where they were.
pub struct Rows {
    loaded: RefCell<Vec<Row>>,
    /// Where `loaded` begins in the whole result.
    offset: Cell<usize>,
    /// How long the result is, which is what the view is sized from.
    total: Cell<usize>,
    /// The page a fetch is out for.
    ///
    /// Without it, every frame drawn between asking and answering reports the
    /// same miss again, and each one moves the window somewhere slightly
    /// different — a list that fetches forever and never settles.
    asked: Cell<Option<(usize, usize)>>,
    /// A row the view asked for and this could not answer.
    want: Cell<Option<usize>>,
    /// The most rows a page has ever actually held.
    ///
    /// What the service gives, which is not always what was asked for: it has
    /// a page ceiling of its own. See [`Rows::served`].
    served: Cell<usize>,
    /// How many times the view has been told to re-measure.
    ///
    /// Kept because it is the number the scrolling bug was made of: it should
    /// move when the result's length changes and at no other time.
    resets: Cell<u64>,
    notify: slint::ModelNotify,
}

impl Default for Rows {
    fn default() -> Self {
        Rows {
            loaded: RefCell::new(Vec::new()),
            offset: Cell::new(0),
            total: Cell::new(0),
            asked: Cell::new(None),
            want: Cell::new(None),
            served: Cell::new(0),
            resets: Cell::new(0),
            notify: slint::ModelNotify::default(),
        }
    }
}

impl Rows {
    /// Hand over a page: the rows, where they start, and how long the whole
    /// result is.
    pub fn put(&self, rows: Vec<Row>, offset: usize, total: usize) {
        let before = (self.offset.get(), self.loaded.borrow().len());
        let grew = total != self.total.get();
        self.served.set(self.served.get().max(rows.len()));
        *self.loaded.borrow_mut() = rows;
        self.offset.set(offset);
        self.total.set(total);
        self.asked.set(None);
        self.want.set(None);
        if grew {
            self.reset();
            return;
        }
        let after = (offset, self.loaded.borrow().len());
        for row in touched(before, after, total) {
            self.notify.row_changed(row);
        }
    }

    /// The length changed and nothing else did.
    ///
    /// The interactive search counts only to its cap, so the first answer to
    /// `a` says a thousand and the exact count arrives a moment later. Without
    /// this the list stays a thousand rows tall over an index of millions —
    /// and then jumps the first time a page happens to be fetched.
    pub fn set_total(&self, total: usize) {
        // Never shorter than what is already loaded: a list that says it holds
        // fewer rows than it is holding cannot draw the ones it has.
        let total = total.max(self.offset.get() + self.loaded.borrow().len());
        if total == self.total.get() {
            return;
        }
        self.total.set(total);
        self.reset();
    }

    /// Take the arrival flags off the rows that are loaded.
    ///
    /// **Only the ones that are loaded**, which is the whole point. This used
    /// to walk `0..row_count()` — the whole result — asking the model for
    /// every row: seconds of frozen window on a large index, and every one of
    /// those millions of misses looked to the model like the view asking for a
    /// row it could not see, which sent the list somewhere else entirely.
    pub fn clear_fresh(&self) {
        let offset = self.offset.get();
        let mut cleared = Vec::new();
        {
            let mut loaded = self.loaded.borrow_mut();
            for (i, row) in loaded.iter_mut().enumerate() {
                if row.fresh {
                    row.fresh = false;
                    cleared.push(offset + i);
                }
            }
        }
        for row in cleared {
            self.notify.row_changed(row);
        }
    }

    /// Note that a page has been asked for, so the same miss is not asked for
    /// again on every frame until it lands.
    pub fn asking(&self, offset: usize, limit: usize) {
        self.asked.set(Some((offset, limit)));
        self.want.set(None);
    }

    /// Forget that a page was asked for, because its answer is not coming.
    ///
    /// A refused query and a service that went away both leave a request
    /// unanswered, and without this the window would sit behind a page that
    /// will never land and never ask for another.
    pub fn forget_asking(&self) {
        self.asked.set(None);
    }

    /// Where the page that is current *or on its way* begins.
    ///
    /// The two differ for as long as a fetch is in flight, and the live
    /// refresh has to ask about the second one: re-fetching the page that is
    /// on screen while a scroll is being answered put the answer to the scroll
    /// on screen and then replaced it with where the list used to be.
    pub fn page_now(&self) -> usize {
        match self.asked.get() {
            Some((offset, _)) => offset,
            None => self.offset.get(),
        }
    }

    /// Whether `count` rows from `first` still have to be fetched: not loaded,
    /// and not already on their way.
    pub fn needs(&self, first: usize, count: usize) -> bool {
        if self.covers(first, count) {
            return false;
        }
        match self.asked.get() {
            Some((offset, limit)) => first < offset || first + count > offset + limit,
            None => true,
        }
    }

    /// The row the view asked for and did not get, if any. Taken, not read:
    /// one fetch per miss.
    pub fn wanted(&self) -> Option<usize> {
        self.want.take()
    }

    /// Whether `count` rows from `first` can all be drawn from what is loaded.
    ///
    /// What the window polls, rather than waiting to be told. A miss reported
    /// by `row_data` only arrives if the view draws the missing row, and after
    /// a page lands somewhere the eye is not, it never does — which is a list
    /// that loads its first page and then stops.
    pub fn covers(&self, first: usize, count: usize) -> bool {
        let total = self.total.get();
        if total == 0 {
            return true;
        }
        let first = first.min(total - 1);
        let last = first.saturating_add(count).min(total);
        let offset = self.offset.get();
        let len = self.loaded.borrow().len();
        first >= offset && last <= offset + len
    }

    /// Where the loaded page begins.
    pub fn at(&self) -> usize {
        self.offset.get()
    }

    pub fn loaded_len(&self) -> usize {
        self.loaded.borrow().len()
    }

    /// How many rows a page actually holds.
    ///
    /// **Observed, not assumed.** The service has a page ceiling of its own,
    /// and if it is lower than what this asks for, a page pinned to the end of
    /// the result stops short of it — so the last rows of a long list are
    /// asked for, drawn blank, and asked for again for as long as anybody
    /// looks at them. This is the evidence for how big a page really is.
    pub fn served(&self) -> usize {
        self.served.get().max(1)
    }

    pub fn total(&self) -> usize {
        self.total.get()
    }

    /// How many times the view has been told to re-measure. See [`Rows`].
    #[cfg(test)]
    pub fn resets(&self) -> u64 {
        self.resets.get()
    }

    fn reset(&self) {
        self.resets.set(self.resets.get() + 1);
        self.notify.reset();
    }
}

/// The rows two windows disagree about: everything either of them held.
///
/// A page arriving in place changes the rows it lands on and the rows it
/// leaves behind, and nothing else in a result of millions.
fn touched(
    before: (usize, usize),
    after: (usize, usize),
    total: usize,
) -> impl Iterator<Item = usize> {
    let ends = |(offset, len): (usize, usize)| (offset, offset + len);
    let (a0, a1) = ends(before);
    let (b0, b1) = ends(after);
    let from = a0.min(b0);
    let to = a1.max(b1).min(total);
    from..to.max(from)
}

impl slint::Model for Rows {
    type Data = Row;

    fn row_count(&self) -> usize {
        self.total.get()
    }

    fn row_data(&self, row: usize) -> Option<Row> {
        let offset = self.offset.get();
        let loaded = self.loaded.borrow();
        if row >= offset && row < offset + loaded.len() {
            return Some(loaded[row - offset].clone());
        }
        // Outside the loaded window. Remember the first such row — the view
        // asks for a run of them and they all want the same page — and give
        // back a blank so the list keeps its shape while it arrives.
        if let Some((offset, limit)) = self.asked.get()
            && row >= offset
            && row < offset + limit
        {
            return Some(Row::default());
        }
        if self.want.get().is_none() {
            self.want.set(Some(row));
        }
        Some(Row::default())
    }

    fn model_tracker(&self) -> &dyn slint::ModelTracker {
        &self.notify
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;
    use slint::Model;

    fn page(n: usize) -> Vec<Row> {
        (0..n).map(|_| Row::default()).collect()
    }

    #[test]
    fn the_list_is_as_long_as_the_result_not_as_the_page() {
        let rows = Rows::default();
        rows.put(page(256), 0, 2_500_000);
        assert_eq!(rows.row_count(), 2_500_000);
        assert_eq!(rows.loaded_len(), 256);
    }

    #[test]
    fn a_page_landing_in_place_does_not_make_the_view_re_measure() {
        // The live refresh: the same query, the same length, new rows. A reset
        // here is a rebuilt list and a re-clamped viewport, which is what the
        // scrolling jump was.
        let rows = Rows::default();
        rows.put(page(256), 0, 10_000);
        let after_first = rows.resets();
        rows.put(page(256), 0, 10_000);
        rows.put(page(256), 128, 10_000);
        assert_eq!(
            rows.resets(),
            after_first,
            "no reset while the length holds"
        );

        // And when the length really does change, the view has to be told.
        rows.put(page(256), 128, 20_000);
        assert_eq!(rows.resets(), after_first + 1);
    }

    #[test]
    fn the_exact_count_lengthens_the_list_it_does_not_reload_it() {
        // The search counts to its cap; the exact total follows a moment
        // later. Until this existed the list stayed as long as the cap.
        let rows = Rows::default();
        rows.put(page(256), 0, 1_000);
        rows.set_total(2_481_902);
        assert_eq!(rows.row_count(), 2_481_902);
        assert_eq!(rows.loaded_len(), 256, "the page it was showing is intact");
        rows.set_total(2_481_902);
        assert_eq!(rows.resets(), 2, "and saying it twice costs nothing");
    }

    #[test]
    fn a_row_outside_the_window_is_asked_for_once() {
        let rows = Rows::default();
        rows.put(page(256), 0, 10_000);
        assert!(rows.row_data(300).is_some(), "drawn blank, not left a hole");
        rows.row_data(301);
        assert_eq!(rows.wanted(), Some(300), "the first miss, not the last");
        assert_eq!(rows.wanted(), None, "taken, so one fetch per miss");
    }

    #[test]
    fn a_page_already_on_its_way_is_not_asked_for_again() {
        let rows = Rows::default();
        rows.put(page(256), 0, 10_000);
        rows.asking(256, 256);
        rows.row_data(300);
        assert_eq!(rows.wanted(), None, "the answer to this is already coming");
        assert!(!rows.needs(300, 30), "and the window does not ask twice");
        // Somewhere else entirely, though, is a different question.
        rows.row_data(9_000);
        assert_eq!(rows.wanted(), Some(9_000));
        assert!(rows.needs(9_000, 30));
        assert_eq!(rows.page_now(), 256, "the refresh follows the fetch");
        // A refusal or a service that went away leaves the page unanswered,
        // and the window has to be able to ask again.
        rows.forget_asking();
        assert!(rows.needs(300, 30));
    }

    #[test]
    fn a_page_is_as_big_as_the_service_makes_it() {
        let rows = Rows::default();
        // The first page is only what fits on screen; the ones after it are
        // full. What a page holds is the biggest of them, not the latest.
        rows.put(page(24), 0, 10_000);
        assert_eq!(rows.served(), 24);
        rows.put(page(200), 0, 10_000);
        rows.put(page(13), 9_987, 10_000);
        assert_eq!(rows.served(), 200, "the short tail is not a smaller page");
    }

    #[test]
    fn what_is_loaded_is_what_can_be_drawn() {
        let rows = Rows::default();
        rows.put(page(256), 128, 10_000);
        assert!(rows.covers(128, 30));
        assert!(rows.covers(354, 30));
        assert!(!rows.covers(127, 30), "one row above the window");
        assert!(!rows.covers(355, 30), "runs off the end of it");
        // The end of the result is covered by whatever is left of it.
        rows.put(page(40), 9_960, 10_000);
        assert!(rows.covers(9_990, 30));
    }

    #[test]
    fn the_arrival_flags_come_off_what_is_loaded_and_nothing_else() {
        let rows = Rows::default();
        let mut marked = page(4);
        marked[1].fresh = true;
        rows.put(marked, 1_000, 2_000_000);
        let before = rows.resets();
        rows.clear_fresh();
        assert!(!rows.row_data(1_001).unwrap().fresh);
        assert_eq!(rows.resets(), before, "clearing a flag is not a re-measure");
        assert_eq!(rows.wanted(), None, "and it asks for nothing");
    }
}
