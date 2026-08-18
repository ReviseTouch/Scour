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
        path: h.path.as_str().into(),
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

/// Rows in one page, and the size of every request the list makes.
///
/// Aligned, so that a page is a *thing*: asked for once, kept, and found again
/// by dividing. Sliding windows at arbitrary offsets cannot be kept, because
/// no two of them line up.
pub const SPAN: usize = scour_core::PAGE_ROWS as usize;

/// How many pages are held at once.
///
/// **This is what makes the list feel like it is in memory.** Everything
/// already looked at is still here, so scrolling back is a lookup rather than
/// a request — and the request is what somebody sees as a stutter. Thirty-two
/// pages is 6,400 rows of strings, a few megabytes, and about fifty screens
/// in either direction.
const KEPT: usize = 32;

struct Held {
    rows: Vec<Row>,
    /// The index revision these rows were read at. See [`Rows::mark`].
    revision: u64,
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
/// draw. `row_data` answers from the pages in hand, and a row that has not
/// arrived is drawn empty for one frame rather than left as a hole.
///
/// Taken from `Hukuk-Dosyalar`'s `RowsModel`, which does the same thing over a
/// store that is already in memory — and this is as close to that as a list
/// whose rows live in another process can get. What that one never does is
/// wait, so this one keeps [`KEPT`] pages and asks for the next one before
/// anybody reaches it: waiting is then only for somewhere nobody has been.
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
    pages: RefCell<std::collections::HashMap<usize, Held>>,
    /// Which page was used least recently, first. What eviction reads.
    order: RefCell<Vec<usize>>,
    /// The page `row_data` last answered from, so the order is only rewritten
    /// when the eye crosses a page boundary rather than on every row drawn.
    touched: Cell<usize>,
    /// How long the result is, which is what the view is sized from.
    total: Cell<usize>,
    /// The page a fetch is out for, and the index revision it was asked at.
    asked: Cell<Option<(usize, u64)>>,
    /// A row the view asked for and this could not answer.
    want: Cell<Option<usize>>,
    /// What "up to date" means now. See [`Rows::mark`].
    revision: Cell<u64>,
    /// The most rows a page has ever actually held.
    ///
    /// What the service gives, which is not always what was asked for: it has
    /// a page ceiling of its own. See [`Rows::served`].
    served: Cell<usize>,
    /// How many times the view has been told the list changed length.
    ///
    /// Kept because it is the number the scrolling bug was made of: it should
    /// move when the result's length changes and at no other time.
    resets: Cell<u64>,
    notify: slint::ModelNotify,
}

impl Default for Rows {
    fn default() -> Self {
        Rows {
            pages: RefCell::new(std::collections::HashMap::new()),
            order: RefCell::new(Vec::new()),
            touched: Cell::new(usize::MAX),
            total: Cell::new(0),
            asked: Cell::new(None),
            want: Cell::new(None),
            revision: Cell::new(0),
            served: Cell::new(0),
            resets: Cell::new(0),
            notify: slint::ModelNotify::default(),
        }
    }
}

impl Rows {
    /// Which page a row belongs to.
    pub fn page_of(row: usize) -> usize {
        row / SPAN
    }

    /// Hand over a page: which one, its rows, and how long the whole result is.
    ///
    /// Returns whether anything in it is new, which is what arms the arrival
    /// wash — see below for what "new" has to mean.
    pub fn put(&self, page: usize, mut rows: Vec<Row>, total: usize) -> bool {
        // **New means new *here*.** A row is an arrival when this page has
        // been read before and did not have it; a page nobody had read yet has
        // no arrivals in it at all.
        //
        // What it was: every row whose path was not in the *previous answer*,
        // whichever page that was for. So dragging the scrollbar washed the
        // whole list orange at every stop — two hundred files that had not
        // changed since 2019, announcing themselves as changes — and the
        // animation that says so kept the window repainting while it did.
        let mut arrived = false;
        if let Some(before) = self.pages.borrow().get(&page) {
            let had: std::collections::HashSet<&str> =
                before.rows.iter().map(|r| r.path.as_str()).collect();
            for row in rows.iter_mut() {
                row.fresh = !row.path.is_empty() && !had.contains(row.path.as_str());
                arrived |= row.fresh;
            }
        }
        // Stamped with the revision the *request* went out at, not the one
        // that is current now: an index that moved while the page was in
        // flight has not been read yet, and stamping it as read would leave
        // the change unfetched until the next one.
        let revision = match self.asked.get() {
            Some((asked, revision)) if asked == page => revision,
            _ => self.revision.get(),
        };
        self.served.set(self.served.get().max(rows.len()));
        let held = Held { rows, revision };
        let touched: Vec<usize> = {
            let mut pages = self.pages.borrow_mut();
            let n = held.rows.len();
            pages.insert(page, held);
            self.use_page(page);
            (page * SPAN..page * SPAN + n).collect()
        };
        self.evict();
        self.asked.set(None);
        self.want.set(None);
        let was = self.total.get();
        if total != was {
            self.total.set(total);
            self.resized(was, total);
            return arrived;
        }
        for row in touched {
            self.notify.row_changed(row);
        }
        arrived
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
        let total = total.max(self.held_to());
        let was = self.total.get();
        if total == was {
            return;
        }
        self.total.set(total);
        self.resized(was, total);
    }

    /// Everything here belongs to a different question. Start again.
    pub fn empty(&self) {
        self.pages.borrow_mut().clear();
        self.order.borrow_mut().clear();
        self.touched.set(usize::MAX);
        self.asked.set(None);
        self.want.set(None);
    }

    /// The index has moved past what these pages were read at.
    ///
    /// **Marked, not thrown away.** A page that is a second out of date is far
    /// better than a blank one: it is drawn at once and corrected when its
    /// answer arrives. Only what is on screen is re-read — a page nobody is
    /// looking at is re-read when somebody looks at it, and an index that
    /// changes every second would otherwise have this window fetching for ever.
    pub fn mark(&self, revision: u64) {
        self.revision.set(revision);
    }

    /// Take the arrival flags off the rows that are held.
    ///
    /// **Only the ones that are held**, which is the whole point. This used to
    /// walk `0..row_count()` — the whole result — asking the model for every
    /// row: seconds of frozen window on a large index, and every one of those
    /// millions of misses looked to the model like the view asking for a row
    /// it could not see, which sent the list somewhere else entirely.
    pub fn clear_fresh(&self) {
        let mut cleared = Vec::new();
        {
            let mut pages = self.pages.borrow_mut();
            for (page, held) in pages.iter_mut() {
                for (i, row) in held.rows.iter_mut().enumerate() {
                    if row.fresh {
                        row.fresh = false;
                        cleared.push(page * SPAN + i);
                    }
                }
            }
        }
        for row in cleared {
            self.notify.row_changed(row);
        }
    }

    /// Note that a page has been asked for, at this index revision, so the
    /// same one is not asked for again on every frame until it lands.
    pub fn asking(&self, page: usize, revision: u64) {
        self.asked.set(Some((page, revision)));
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

    /// The row the view asked for and did not get, if any. Taken, not read:
    /// one fetch per miss.
    pub fn wanted(&self) -> Option<usize> {
        self.want.take()
    }

    /// The page to ask for next, if any: what the eye is on, then where it is
    /// going, then where it has been.
    ///
    /// **The last two are why scrolling does not wait.** A page is about fifty
    /// screens; asking for the next one the moment this one is complete means
    /// the answer — 2 to 25 ms of it — is already here when somebody arrives.
    /// They are asked for only when *missing*, never merely because the index
    /// moved: a page nobody is looking at is not worth a request, and an index
    /// that changes every second would otherwise keep this fetching for ever.
    pub fn next_page(
        &self,
        first: usize,
        last: usize,
        speculate: bool,
        refresh: bool,
    ) -> Option<usize> {
        let total = self.total.get();
        if total == 0 {
            return None;
        }
        // **One request at a time, always for where the eye is now.** A hand
        // that throws the scrollbar across a million rows crosses a page every
        // frame, and a request per crossing is sixty requests for the one page
        // anybody will look at — each of them queued ahead of it. So nothing
        // is asked for while an answer is on its way, and when it lands the
        // next question is asked about wherever the list has got to by then.
        if self.asked.get().is_some() {
            return None;
        }
        let end = (total - 1) / SPAN;
        let from = Self::page_of(first.min(total - 1));
        let to = Self::page_of(last.min(total - 1));
        // On screen: fetched when missing, and re-read when the index has moved
        // under them — but the second only when the caller says there is time
        // for it. During a scan the index moves several times a second, and a
        // list that re-read the page under the pointer every time it did would
        // spend a drag fetching the same rows over and over.
        for page in from..=to {
            let held = self.pages.borrow();
            match held.get(&page) {
                None => return Some(page),
                Some(held) if held.rows.is_empty() => return Some(page),
                Some(held) if refresh && held.revision != self.revision.get() => {
                    return Some(page);
                }
                Some(_) => {}
            }
        }
        // Ahead, then behind: only what is missing altogether, and only while
        // a page is cheap. **Speculation is worth what it costs**, and deep in
        // a long result a page costs the service a walk of everything above it
        // — 125 ms at two and a half million rows, measured. Guessing wrong
        // there spends that on rows nobody asked for.
        if !speculate {
            return None;
        }
        let near = [(to < end).then_some(to + 1), from.checked_sub(1)];
        near.into_iter().flatten().find(|page| !self.holds(*page))
    }

    fn holds(&self, page: usize) -> bool {
        self.pages.borrow().contains_key(&page)
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

    /// The path of a row, if it is in hand.
    ///
    /// What opening, revealing and copying read. It comes off the row itself
    /// rather than out of a separate list of hits, because the two would then
    /// have to be kept in step — and while they were a window and a page they
    /// were not: the list's row 4,000 was read as the four-thousandth of a
    /// page of two hundred, which opened a file two hundred rows away.
    pub fn path_at(&self, row: usize) -> Option<String> {
        let pages = self.pages.borrow();
        let held = pages.get(&Self::page_of(row))?;
        held.rows
            .get(row % SPAN)
            .map(|r| r.path.to_string())
            .filter(|p| !p.is_empty())
    }

    /// How long the result is.
    pub fn length(&self) -> usize {
        self.total.get()
    }

    /// How many rows are held, over all the pages kept.
    pub fn held(&self) -> usize {
        self.pages.borrow().values().map(|h| h.rows.len()).sum()
    }

    /// How many times the view has been told to re-measure. See [`Rows`].
    #[cfg(test)]
    pub fn resets(&self) -> u64 {
        self.resets.get()
    }

    /// One past the last row held, over all the pages kept.
    fn held_to(&self) -> usize {
        self.pages
            .borrow()
            .iter()
            .map(|(page, held)| page * SPAN + held.rows.len())
            .max()
            .unwrap_or(0)
    }

    /// Say a page has just been used, for eviction's sake.
    fn use_page(&self, page: usize) {
        let mut order = self.order.borrow_mut();
        order.retain(|p| *p != page);
        order.push(page);
    }

    fn evict(&self) {
        let mut order = self.order.borrow_mut();
        while order.len() > KEPT {
            let oldest = order.remove(0);
            self.pages.borrow_mut().remove(&oldest);
        }
    }

    /// The list is a different length than the view thinks.
    ///
    /// **Added and removed, not reset.** A reset makes the view throw its
    /// layout state away with its elements, and what it rebuilds it from is
    /// the top — so a list that grew while somebody was reading row nine
    /// thousand put them back at row one. Saying which rows appeared leaves
    /// the viewport where it is.
    fn resized(&self, was: usize, now: usize) {
        if was == now {
            return;
        }
        self.resets.set(self.resets.get() + 1);
        if now > was {
            self.notify.row_added(was, now - was);
        } else {
            self.notify.row_removed(now, was - now);
        }
    }
}

impl slint::Model for Rows {
    type Data = Row;

    fn row_count(&self) -> usize {
        self.total.get()
    }

    fn row_data(&self, row: usize) -> Option<Row> {
        let page = Self::page_of(row);
        if let Some(held) = self.pages.borrow().get(&page)
            && let Some(found) = held.rows.get(row % SPAN)
        {
            // Only when the eye crosses into another page, so drawing a screen
            // is not thirty rewrites of the same list.
            if self.touched.get() != page {
                self.touched.set(page);
                self.use_page(page);
            }
            return Some(found.clone());
        }
        // Not in hand. Remember the first such row — the view asks for a run
        // of them and they all want the same page — and give back a blank so
        // the list keeps its shape while it arrives.
        if self.want.get().is_none() {
            self.want.set(Some(row));
        }
        Some(Row::default())
    }

    fn model_tracker(&self) -> &dyn slint::ModelTracker {
        &self.notify
    }
}

/// The same results, a line at a time, for the tile views.
///
/// **A model over a model, which is the whole reason the tile view came back.**
/// The first one laid itself out by looping over a count and indexing into the
/// rows — and a `for` over an integer builds every element at once, so the list
/// stopped being lazy and stopped fetching. Here a line *is* a model: the view
/// asks for the lines it is about to draw, each of those asks [`Rows`] for its
/// tiles, and a tile that has not arrived records the same miss a row does.
pub struct Lines {
    rows: std::rc::Rc<Rows>,
    /// Tiles on a line, and zero while the table is showing — a model nobody
    /// is looking at should not be building anything.
    per: Cell<usize>,
    /// The length this last told the view about. See [`Lines::sync`].
    shown: Cell<usize>,
    notify: slint::ModelNotify,
}

impl Lines {
    pub fn new(rows: std::rc::Rc<Rows>) -> Lines {
        Lines {
            rows,
            per: Cell::new(0),
            shown: Cell::new(0),
            notify: slint::ModelNotify::default(),
        }
    }

    /// How many tiles fit on a line, or zero while the table is showing.
    pub fn per_line(&self, per: usize) {
        if per != self.per.get() {
            self.per.set(per);
            // Every line holds different results now, not merely a different
            // number of them, so this one really is a reset.
            self.shown.set(self.lines());
            self.notify.reset();
        }
    }

    /// Tell the view if the result has changed length under it.
    ///
    /// Checked rather than announced, because the length is the row count
    /// divided by the tiles on a line and both of those move — the window is
    /// resized, the count arrives, a page lands past the end. One comparison
    /// on a timer is cheaper than four callers remembering to say so.
    pub fn sync(&self) {
        self.stretch();
    }

    /// The rows in `from..to` have changed, so the lines holding them have.
    pub fn touched(&self, from: usize, to: usize) {
        let per = self.per.get();
        if per == 0 || to <= from {
            return;
        }
        for line in (from / per)..=((to - 1) / per) {
            self.notify.row_changed(line);
        }
    }

    fn lines(&self) -> usize {
        match self.per.get() {
            0 => 0,
            per => self.rows.length().div_ceil(per),
        }
    }

    /// A different number of lines, said as an addition or a removal — see
    /// [`Rows::resized`] for why not a reset.
    fn stretch(&self) {
        let was = self.shown.get();
        let now = self.lines();
        self.shown.set(now);
        match now.cmp(&was) {
            std::cmp::Ordering::Greater => self.notify.row_added(was, now - was),
            std::cmp::Ordering::Less => self.notify.row_removed(now, was - now),
            std::cmp::Ordering::Equal => {}
        }
    }
}

impl slint::Model for Lines {
    type Data = slint::ModelRc<Row>;

    fn row_count(&self) -> usize {
        self.shown.get()
    }

    fn row_data(&self, line: usize) -> Option<slint::ModelRc<Row>> {
        let per = self.per.get();
        if per == 0 {
            return None;
        }
        let total = self.rows.length();
        let from = line * per;
        let tiles: Vec<Row> = (from..(from + per).min(total))
            .map(|row| slint::Model::row_data(&*self.rows, row).unwrap_or_default())
            .collect();
        Some(slint::ModelRc::new(slint::VecModel::from(tiles)))
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

    fn named(paths: &[&str]) -> Vec<Row> {
        paths
            .iter()
            .map(|p| Row {
                path: (*p).into(),
                ..Row::default()
            })
            .collect()
    }

    #[test]
    fn the_list_is_as_long_as_the_result_not_as_the_page() {
        let rows = Rows::default();
        rows.put(0, page(SPAN), 2_500_000);
        assert_eq!(rows.row_count(), 2_500_000);
        assert_eq!(rows.held(), SPAN);
    }

    #[test]
    fn a_page_landing_in_place_does_not_make_the_view_re_measure() {
        // The live refresh: the same query, the same length, new rows. A reset
        // here is a rebuilt list and a re-clamped viewport, which is what the
        // scrolling jump was.
        let rows = Rows::default();
        rows.put(0, page(SPAN), 10_000);
        let after_first = rows.resets();
        rows.put(0, page(SPAN), 10_000);
        rows.put(1, page(SPAN), 10_000);
        assert_eq!(
            rows.resets(),
            after_first,
            "no reset while the length holds"
        );

        // And when the length really does change, the view has to be told.
        rows.put(2, page(SPAN), 20_000);
        assert_eq!(rows.resets(), after_first + 1);
    }

    #[test]
    fn the_exact_count_lengthens_the_list_it_does_not_reload_it() {
        // The search counts to its cap; the exact total follows a moment
        // later. Until this existed the list stayed as long as the cap.
        let rows = Rows::default();
        rows.put(0, page(SPAN), 1_000);
        rows.set_total(2_481_902);
        assert_eq!(rows.row_count(), 2_481_902);
        assert_eq!(rows.held(), SPAN, "the page it was showing is intact");
        rows.set_total(2_481_902);
        assert_eq!(rows.resets(), 2, "and saying it twice costs nothing");
    }

    #[test]
    fn a_row_outside_the_pages_in_hand_is_asked_for_once() {
        let rows = Rows::default();
        rows.put(0, page(SPAN), 10_000);
        assert!(
            rows.row_data(SPAN + 44).is_some(),
            "drawn blank, not left a hole"
        );
        rows.row_data(SPAN + 45);
        assert_eq!(
            rows.wanted(),
            Some(SPAN + 44),
            "the first miss, not the last"
        );
        assert_eq!(rows.wanted(), None, "taken, so one fetch per miss");
    }

    #[test]
    fn what_is_asked_for_is_where_the_eye_is_then_where_it_is_going() {
        let rows = Rows::default();
        rows.put(0, page(SPAN), 10_000);
        // The screen is covered, so the next page is the one ahead of it.
        assert_eq!(rows.next_page(0, 24, true, true), Some(1));
        rows.asking(1, 0);
        assert_eq!(
            rows.next_page(0, 24, true, true),
            None,
            "and it is not asked for twice"
        );
        rows.put(1, page(SPAN), 10_000);
        // Now ahead is held too, so nothing is wanted until the eye moves.
        assert_eq!(rows.next_page(0, 24, true, true), None);
        // Two pages down, what is on screen wins over what is beside it.
        assert_eq!(rows.next_page(SPAN * 3, SPAN * 3 + 24, true, true), Some(3));
        // At the top of the list there is nothing behind to fetch.
        rows.put(2, page(SPAN), 10_000);
        rows.put(3, page(SPAN), 10_000);
        assert_eq!(rows.next_page(0, 24, true, true), None);
    }

    #[test]
    fn nothing_is_guessed_at_while_a_page_is_expensive() {
        // Deep in a long result a page costs the service a walk of everything
        // above it. What is on screen is still fetched; what somebody might
        // scroll to is not.
        let rows = Rows::default();
        rows.put(9, page(SPAN), 4_000_000);
        assert_eq!(rows.next_page(9 * SPAN, 9 * SPAN + 24, false, true), None);
        assert_eq!(
            rows.next_page(9 * SPAN, 9 * SPAN + 24, true, true),
            Some(10)
        );
        assert_eq!(
            rows.next_page(20 * SPAN, 20 * SPAN + 24, false, true),
            Some(20),
            "but the page being looked at is not a guess"
        );
    }

    #[test]
    fn a_page_the_index_has_moved_past_is_re_read_only_where_it_is_seen() {
        let rows = Rows::default();
        rows.put(0, page(SPAN), 10_000);
        rows.put(1, page(SPAN), 10_000);
        rows.mark(7);
        // On screen: re-read, because what it shows may be out of date.
        assert_eq!(rows.next_page(0, 24, true, true), Some(0));
        rows.asking(0, 7);
        rows.put(0, page(SPAN), 10_000);
        // Off screen: left alone. An index that moves every second would
        // otherwise have this window fetching every page it has ever seen.
        assert_eq!(rows.next_page(0, 24, true, true), None);
    }

    #[test]
    fn scrolling_back_over_something_already_seen_asks_for_nothing() {
        // The whole point of keeping pages: a request is what somebody sees as
        // a stutter, and going back over what you have just read makes none.
        let rows = Rows::default();
        for p in 0..8 {
            rows.put(p, page(SPAN), 10_000);
        }
        // Page 7 still wants the one after it — that is the fetch that runs
        // ahead of the eye, not a re-read of anything.
        assert_eq!(rows.next_page(7 * SPAN, 7 * SPAN + 24, true, true), Some(8));
        for p in (0..7).rev() {
            let first = p * SPAN;
            assert_eq!(
                rows.next_page(first, first + 24, true, true),
                None,
                "page {p} was already read, and so were both beside it"
            );
        }
    }

    #[test]
    fn only_so_many_pages_are_kept() {
        let rows = Rows::default();
        for p in 0..KEPT + 4 {
            rows.put(p, page(SPAN), 100_000);
        }
        assert_eq!(rows.held(), KEPT * SPAN);
        assert!(
            rows.next_page(0, 24, true, true).is_some(),
            "the oldest went first"
        );
        assert!(
            rows.next_page(KEPT * SPAN, KEPT * SPAN + 24, true, true)
                .is_none(),
            "and the newest stayed"
        );
    }

    #[test]
    fn a_page_nobody_had_read_holds_no_arrivals() {
        // Dragging the scrollbar washed the whole list orange at every stop:
        // two hundred files that had not changed in years, each announcing
        // itself as a change, because "new" was measured against whichever
        // page had been fetched last rather than against this one.
        let rows = Rows::default();
        assert!(
            !rows.put(0, named(&["/a", "/b"]), 10_000),
            "the first read of a page is not an arrival"
        );
        assert!(
            !rows.put(7, named(&["/c", "/d"]), 10_000),
            "nor is the first read of another page"
        );
        assert!(!rows.row_data(7 * SPAN).unwrap().fresh);

        // But a page that comes back holding something it did not before is
        // exactly what the wash is for.
        assert!(rows.put(7, named(&["/c", "/new", "/d"]), 10_000));
        assert!(!rows.row_data(7 * SPAN).unwrap().fresh, "/c was here");
        assert!(rows.row_data(7 * SPAN + 1).unwrap().fresh, "/new was not");
        assert!(!rows.row_data(7 * SPAN + 2).unwrap().fresh, "/d was here");

        // And the same page unchanged says nothing at all.
        assert!(!rows.put(7, named(&["/c", "/new", "/d"]), 10_000));
    }

    #[test]
    fn the_arrival_flags_come_off_what_is_held_and_nothing_else() {
        let rows = Rows::default();
        // Read twice, the second time with a row the first did not have.
        rows.put(5, named(&["/a", "/b", "/c", "/d"]), 2_000_000);
        assert!(rows.put(5, named(&["/a", "/new", "/c", "/d"]), 2_000_000));
        let before = rows.resets();
        rows.clear_fresh();
        assert!(!rows.row_data(5 * SPAN + 1).unwrap().fresh);
        assert_eq!(rows.resets(), before, "clearing a flag is not a re-measure");
        assert_eq!(rows.wanted(), None, "and it asks for nothing");
    }

    #[test]
    fn the_row_a_list_of_millions_calls_four_thousand_is_the_right_file() {
        // Reading it out of a page as though the page began at row zero is
        // what opened a file two hundred rows away.
        let rows = Rows::default();
        rows.put(20, named(&["/a/one.txt", "/a/two.txt"]), 2_000_000);
        assert_eq!(rows.path_at(20 * SPAN + 1).as_deref(), Some("/a/two.txt"));
        assert_eq!(rows.path_at(20 * SPAN + 2), None, "past the page's end");
        assert_eq!(rows.path_at(0), None, "a page that is not in hand");
    }

    #[test]
    fn a_new_question_empties_the_pages() {
        let rows = Rows::default();
        rows.put(0, named(&["/old"]), 10_000);
        rows.empty();
        assert_eq!(rows.path_at(0), None);
        assert_eq!(rows.next_page(0, 24, true, true), Some(0));
    }
}
