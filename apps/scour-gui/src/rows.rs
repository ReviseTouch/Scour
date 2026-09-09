//! Turning a reply into what the window draws.
//!
//! All formatting stays on this side of the language boundary: a `.slint` file
//! that sized bytes or folded Turkish text would be a second copy of what
//! `scour-core` already has.

// `Cell` here is the interior-mutability one; the table's is `crate::Cell`.
use std::cell::{Cell as Flag, RefCell};

use scour_core::{Hit, Kind, text::Folder};

use crate::{Cell, Row};

/// The picture a row has when it has none. Not `Image::default()`: two empty
/// Slint images compare unequal, so every row looks changed every frame and an
/// idle window goes from 25% to 49% of a core. One shared pixel equals itself.
pub fn blank() -> slint::Image {
    thread_local! {
        static BLANK: slint::Image = {
            let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(1, 1);
            buffer.make_mut_bytes().fill(0);
            slint::Image::from_rgba8(buffer)
        };
    }
    BLANK.with(slint::Image::clone)
}

/// Which of the six age bands a row falls in — the stripe down its left.
/// The cast the generated Slint struct wants; [`scour_ui::format::band`] decides.
pub fn band(now: i64, mtime: i64) -> i32 {
    scour_ui::format::band(now, mtime) as i32
}

/// A name cut into what precedes the match, the match, and what follows.
/// The search runs on folded text and Turkish folding changes byte lengths, so
/// an offset goes back through `fold_indexed` before it touches the original.
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
            // The earliest match, longest among ties: the eye lands on it first.
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
    // Folded offset back to the original spelling, pushed to a char boundary:
    // a name is arbitrary bytes from a disk and a mid-character slice panics.
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

/// The colour a kind's icon is drawn in, or the window's quiet ink when the
/// kind has none.
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

/// What a row needs that is not on the row: which columns, and what the machine
/// calls things. Built once per page, not once per cell — every answer in it is
/// the same for all two hundred rows of a reply.
pub struct Shape<'a> {
    /// The chosen columns, in the order they are shown.
    pub columns: &'a [&'static scour_ui::Column],
    /// Where volumes are and whether they record reads, for the `Accessed`
    /// column. Empty means nothing is known, and shows the timestamp.
    pub mounts: &'a [scour_places::Mount],
    /// The kind's own word, already translated.
    pub kind: &'a str,
    /// `this volume does not record reads…`, already translated.
    pub frozen_note: &'a str,
    pub now: i64,
}

/// Does this path sit on a volume that has stopped recording reads?
/// The deepest mount wins, which is what gets `/mnt/depo` right when `/` is
/// mounted too. A `noatime` volume's access times mean nothing, so: a dash.
fn frozen_atime(path: &str, mounts: &[scour_places::Mount]) -> bool {
    let mut owner: Option<&scour_places::Mount> = None;
    for m in mounts {
        let under = m.at == "/" || format!("{path}/").starts_with(&format!("{}/", m.at));
        if under && owner.is_none_or(|o| m.at.len() > o.at.len()) {
            owner = Some(m);
        }
    }
    owner.is_some_and(|m| !m.reads)
}

/// One cell, for one column, of one row. The language, the decimal mark and the
/// clock's offset are all applied here; what goes back is text and how to draw it.
fn cell_of(h: &Hit, id: &str, shape: &Shape, terms: &[String]) -> Cell {
    let text = |t: String| Cell {
        id: id.into(),
        text: t.into(),
        age: -1,
        ..Cell::default()
    };
    let mono = |t: String| Cell {
        mono: true,
        ..text(t)
    };
    let num = |t: String| Cell {
        right: true,
        ..mono(t)
    };
    let when = |at: i64| Cell {
        text: if at == 0 {
            slint::SharedString::new()
        } else {
            scour_ui::format::stamp(at).into()
        },
        age: band(shape.now, at),
        mono: true,
        ..text(String::new())
    };

    match id {
        "name" => {
            let (pre, hit, post) = split_at_match(h.name(), terms);
            Cell {
                pre: pre.into(),
                hit: hit.into(),
                post: post.into(),
                ..text(String::new())
            }
        }
        "kind" => text(shape.kind.to_owned()),
        "path" => mono(h.parent().to_owned()),
        "mtime" => when(h.meta.mtime),
        "ctime" => when(h.meta.ctime),
        "atime" => {
            if frozen_atime(&h.path, shape.mounts) {
                Cell {
                    text: "—".into(),
                    dim: true,
                    hint: shape.frozen_note.into(),
                    ..mono(String::new())
                }
            } else {
                when(h.meta.atime)
            }
        }
        // A folder's size is what the index holds under it, and the `~` says so:
        // the scan rules leave things out.
        "size" => num(match (h.is_dir, h.under.as_ref()) {
            (true, Some(u)) => format!("~{}", scour_ui::format::size(u.disk, decimal())),
            (true, None) => String::new(),
            (false, _) => scour_ui::format::size(h.meta.size.max(0) as u64, decimal()),
        }),
        "disk" => num(if h.meta.disk > 0 {
            scour_ui::format::size(h.meta.disk as u64, decimal())
        } else {
            String::new()
        }),
        "ext" => text(scour_core::ext_str(h.name()).to_owned()),
        "perm" => mono(scour_core::mode_string(h.meta.mode)),
        "user" => mono(scour_core::owner_name(scour_core::Owner::User, h.meta.uid)),
        "group" => mono(scour_core::owner_name(scour_core::Owner::Group, h.meta.gid)),
        // An unwritten column is a bug, but not a reason to lose the row.
        _ => text(String::new()),
    }
}

pub fn row_of(h: &Hit, terms: &[String], shape: &Shape, fresh: bool) -> Row {
    let cells: Vec<Cell> = shape
        .columns
        .iter()
        .map(|c| cell_of(h, c.id, shape, terms))
        .collect();
    Row {
        cells: slint::ModelRc::new(slint::VecModel::from(cells)),
        name: h.name().into(),
        path: h.path.as_str().into(),
        fresh,
        ktoken: h.kind.token().into(),
        tint: tint_of(h.kind.token()),
        is_dir: h.is_dir,
        age: band(shape.now, h.meta.mtime),
        picked: false,
        // Empty; filled in after the row is on screen by `look_for_pictures`.
        thumb: blank(),
        shot: false,
    }
}

/// The decimal mark this window is punctuating with. `main` owns the language;
/// this module only draws rows.
fn decimal() -> char {
    crate::marks().1
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
        // `Değişiklik` is ten characters and thirteen bytes: a folded offset
        // used unchanged cuts elsewhere, possibly mid-character.
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

    /// The bands are `scour-ui`'s and tested there; this covers the cast.
    #[test]
    fn the_band_a_row_carries_is_the_shared_one() {
        let now = 1_800_000_000;
        let day = scour_ui::format::DAY;
        assert_eq!(band(now, now), 0);
        assert_eq!(band(now, now - 3 * day), 1);
        assert_eq!(band(now, now - 400 * day), 5);
        assert_eq!(band(now, now + day), 0, "the future is today");
    }
}

/// Rows in one page, and the size of every request the list makes.
/// [`scour_page`] owns the number and the rules that go with it.
pub use scour_page::SPAN;

/// A row and what it weighs. The byte count sits beside the row rather than on
/// it because Slint's numbers are 32-bit and a file's size is not.
pub struct Kept {
    pub row: Row,
    pub bytes: i64,
    /// How far this row has got with its picture.
    pub pic: Pic,
}

/// What is known about a row's thumbnail. Kept on the row, so "already looked"
/// dies with the page and needs no bounded cache beside the list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pic {
    /// Nobody has looked yet.
    Unknown,
    /// Looked, and there is none — but the desktop declares something that
    /// could make one, so it is worth asking the service.
    Missing,
    /// Looked, and there never will be one: no thumbnailer for this type, or
    /// a kind that is never worth a `stat`.
    Nothing,
    /// Drawn.
    Shown,
}

/// One row of a selection: what it is, where, and what it weighs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    pub path: String,
    pub is_dir: bool,
    pub bytes: i64,
}

impl Pick {
    /// The directory it sits in — what "open their folders" opens.
    pub fn folder(&self) -> &str {
        // `.` where the shared one says nothing: this goes to a file manager,
        // and an empty string is not somewhere to open.
        match scour_ui::path::folder(&self.path) {
            "" => ".",
            up => up,
        }
    }
}

/// The list, as a model the view pulls from rather than a vector it is handed:
/// `row_count` is the real total, so the view sizes its scrollbar and asks only
/// for the rows it draws, and [`scour_page::KEPT`] pages are kept. A reset is
/// only ever for a length change — a page landing in a list of the same length
/// is `row_changed` over the rows that differ, which leaves the viewport still.
pub struct Rows {
    /// The pages, and every rule about which ones to have. See [`scour_page`].
    pages: RefCell<scour_page::Pages<Kept>>,
    /// The page `row_data` last answered from, so the order is only rewritten
    /// when the eye crosses a page boundary rather than on every row drawn.
    touched: Flag<usize>,
    /// A row the view asked for and this could not answer.
    want: Flag<Option<usize>>,
    /// How many times the view has been told the list changed length. Moves on a
    /// length change and at no other time.
    resets: Flag<u64>,
    /// How many rows in hand nobody has looked for a picture for; zero lets the
    /// sweep skip a tick. May be too high, never too low: too low loses a picture.
    unlooked: Flag<usize>,
    notify: slint::ModelNotify,
}

impl Default for Rows {
    fn default() -> Self {
        Rows {
            pages: RefCell::new(scour_page::Pages::default()),
            touched: Flag::new(usize::MAX),
            want: Flag::new(None),
            resets: Flag::new(0),
            unlooked: Flag::new(0),
            notify: slint::ModelNotify::default(),
        }
    }
}

impl Rows {
    /// Which page a row belongs to.
    pub fn page_of(row: usize) -> usize {
        scour_page::Pages::<Kept>::page_of(row)
    }

    /// Hand over a page: which one, its rows, and how long the whole result is.
    /// Returns whether anything in it is new, which arms the arrival wash.
    pub fn put(&self, page: usize, mut rows: Vec<Row>, bytes: Vec<i64>, total: usize) -> bool {
        // New means new *here*: a row is an arrival only when this page has been
        // read before and did not hold it.
        let mut arrived = false;
        // What this page already knew, by path. Two things survive a refetch:
        // whether a row is an arrival, and the picture it was drawn with.
        let mut known: std::collections::HashMap<String, Pic> = std::collections::HashMap::new();
        let mut drawn: std::collections::HashMap<String, slint::Image> =
            std::collections::HashMap::new();
        {
            let pages = self.pages.borrow();
            if pages.holds(page) {
                for kept in (0..SPAN).filter_map(|i| pages.at(page * SPAN + i)) {
                    let path = kept.row.path.to_string();
                    if kept.pic == Pic::Shown {
                        drawn.insert(path.clone(), kept.row.thumb.clone());
                    }
                    known.insert(path, kept.pic);
                }
                for row in rows.iter_mut() {
                    row.fresh = !row.path.is_empty() && !known.contains_key(row.path.as_str());
                    arrived |= row.fresh;
                }
            }
        }
        let kept: Vec<Kept> = rows
            .into_iter()
            .enumerate()
            .map(|(i, mut row)| {
                let pic = known
                    .get(row.path.as_str())
                    .copied()
                    .unwrap_or(Pic::Unknown);
                if pic == Pic::Shown
                    && let Some(image) = drawn.get(row.path.as_str())
                {
                    row.thumb = image.clone();
                    row.shot = true;
                }
                Kept {
                    bytes: bytes.get(i).copied().unwrap_or(0),
                    // A verdict survives a refetch; it dies with the page.
                    pic,
                    row,
                }
            })
            .collect();
        self.unlooked
            .set(self.unlooked.get() + kept.iter().filter(|k| k.pic == Pic::Unknown).count());
        let change = self.pages.borrow_mut().put(page, kept, total);
        self.want.set(None);
        self.tell(change);
        arrived
    }

    /// Pass on what a page call changed, in the terms the view understands.
    fn tell(&self, change: scour_page::Change) {
        match change {
            scour_page::Change::Nothing => {}
            scour_page::Change::Rows { from, to } => {
                for row in from..to {
                    self.notify.row_changed(row);
                }
            }
            // Added/removed rather than reset: a reset rebuilds from the top,
            // losing the viewport of anybody reading row nine thousand.
            scour_page::Change::Length { was, now, rows } => {
                self.resets.set(self.resets.get() + 1);
                if was == 0 {
                    // Slint 1.16's repeater allocates `count` slots for
                    // `row_added(0, count)`; an empty model has no viewport to keep.
                    self.notify.reset();
                } else if now > was {
                    self.notify.row_added(was, now - was);
                } else {
                    self.notify.row_removed(now, was - now);
                }
                // A length change does not imply the rows it brought changed, so
                // say both. Clamped: a row past the new end no longer exists.
                if let Some((from, to)) = rows {
                    for row in from..to.min(now) {
                        self.notify.row_changed(row);
                    }
                }
            }
        }
    }

    /// The length changed and nothing else did. The interactive search counts
    /// only to its cap; the exact total arrives a moment later.
    pub fn set_total(&self, total: usize) {
        // Never shorter than what is already loaded: a list that says it holds
        // fewer rows than it is holding cannot draw the ones it has.
        let total = total.max(self.held_to());
        let change = self.pages.borrow_mut().set_total(total);
        self.tell(change);
    }

    /// Everything here belongs to a different question. Start again.
    pub fn empty(&self) {
        self.pages.borrow_mut().empty();
        self.touched.set(usize::MAX);
        self.want.set(None);
    }

    /// The index has moved past what these pages were read at. Marked, not
    /// dropped: a stale page draws at once and is corrected when its answer lands.
    pub fn mark(&self, revision: u64) {
        self.pages.borrow_mut().mark(revision);
    }

    /// Take the arrival flags off the rows that are held — only those, never a
    /// walk of the whole result.
    pub fn clear_fresh(&self) {
        let mut cleared = Vec::new();
        {
            let mut pages = self.pages.borrow_mut();
            let held: Vec<usize> = pages.pages().collect();
            for page in held {
                let Some(rows) = pages.rows_mut(page) else {
                    continue;
                };
                for (i, kept) in rows.iter_mut().enumerate() {
                    if kept.row.fresh {
                        kept.row.fresh = false;
                        cleared.push(page * SPAN + i);
                    }
                }
            }
        }
        for row in cleared {
            self.notify.row_changed(row);
        }
    }

    /// Note that a page has been asked for, so the same one is not asked for
    /// again on every frame until it lands.
    pub fn asking(&self, page: usize, revision: u64) {
        let mut pages = self.pages.borrow_mut();
        pages.mark(revision);
        pages.asking(page);
        drop(pages);
        self.want.set(None);
    }

    /// Forget that a page was asked for, because its answer is not coming: a
    /// refused query or a service that went away leaves a request unanswered.
    pub fn forget_asking(&self) {
        self.pages.borrow_mut().forget_asking();
    }

    /// The row the view asked for and did not get, if any. Taken, not read:
    /// one fetch per miss.
    pub fn wanted(&self) -> Option<usize> {
        self.want.take()
    }

    /// The page to ask for next, if any: what the eye is on, then where it is
    /// going, then where it has been. See [`scour_page::Pages::next_page`].
    pub fn next_page(
        &self,
        first: usize,
        last: usize,
        speculate: bool,
        refresh: bool,
    ) -> Option<usize> {
        self.pages
            .borrow()
            .next_page(first, last, speculate, refresh)
    }

    /// How many rows a page actually holds, as observed rather than assumed.
    pub fn served(&self) -> usize {
        self.pages.borrow().served()
    }

    /// Everything a selection needs about a row, if it is in hand.
    pub fn pick_at(&self, row: usize) -> Option<Pick> {
        let pages = self.pages.borrow();
        let found = pages.at(row)?;
        if found.row.path.is_empty() {
            return None;
        }
        Some(Pick {
            path: found.row.path.to_string(),
            is_dir: found.row.is_dir,
            bytes: found.bytes,
        })
    }

    /// Paint the rows a selection holds, and unpaint the rest. Walks what is in
    /// hand, not what is on screen: a row scrolled away comes back selected.
    pub fn mark_picked(&self, picked: &std::collections::HashSet<String>) {
        let mut changed = Vec::new();
        {
            let mut pages = self.pages.borrow_mut();
            let held: Vec<usize> = pages.pages().collect();
            for page in held {
                let Some(rows) = pages.rows_mut(page) else {
                    continue;
                };
                for (i, kept) in rows.iter_mut().enumerate() {
                    let want = picked.contains(kept.row.path.as_str());
                    if kept.row.picked != want {
                        kept.row.picked = want;
                        changed.push(page * SPAN + i);
                    }
                }
            }
        }
        for row in changed {
            self.notify.row_changed(row);
        }
    }

    /// Look up the pictures for the rows in sight and draw the ones on disk.
    /// Returns `(drawn, worth asking for)`. On the drawing thread, so bounded to
    /// the range, to unlooked rows and to `batch`: four `stat` calls each.
    pub fn look_for_pictures(&self, from: usize, to: usize, batch: usize) -> (usize, Vec<String>) {
        if self.unlooked.get() == 0 {
            return (0, Vec::new());
        }
        let mut drawn = Vec::new();
        let mut ask = Vec::new();
        let mut looked = 0usize;
        {
            let mut pages = self.pages.borrow_mut();
            let mut row = from;
            while row < to && looked < batch {
                let page = Self::page_of(row);
                let Some(rows) = pages.rows_mut(page) else {
                    // A page nobody has fetched: skip it whole, not row by row.
                    row = (page + 1) * SPAN;
                    continue;
                };
                let Some(kept) = rows.get_mut(row % SPAN) else {
                    row += 1;
                    continue;
                };
                if kept.pic != Pic::Unknown || kept.row.path.is_empty() {
                    row += 1;
                    continue;
                }
                looked += 1;
                self.unlooked.set(self.unlooked.get().saturating_sub(1));
                let path = kept.row.path.to_string();
                let token = kept.row.ktoken.to_string();
                // Kind first: `never_for` matches a word, `existing` is four
                // `stat` calls, and for nearly every row the answer is no.
                let found = if scour_thumbs::never_for(&token) {
                    None
                } else {
                    scour_thumbs::cache::existing(&path)
                };
                match found {
                    Some(picture) => match slint::Image::load_from_path(&picture) {
                        Ok(image) => {
                            kept.row.thumb = image;
                            kept.row.shot = true;
                            kept.pic = Pic::Shown;
                            drawn.push(row);
                        }
                        // A cached file that will not decode is not worth remaking.
                        Err(e) => {
                            crate::trace(&format!("{} would not decode: {e}", picture.display()));
                            kept.pic = Pic::Nothing;
                        }
                    },
                    None if scour_thumbs::may(&path, &token) => {
                        kept.pic = Pic::Missing;
                        ask.push(path);
                    }
                    None => kept.pic = Pic::Nothing,
                }
                row += 1;
            }
        }
        for row in &drawn {
            self.notify.row_changed(*row);
        }
        (drawn.len(), ask)
    }

    /// The service has made these; look at them again on the next tick. Only the
    /// mark is undone here, so the decode stays on the bounded path.
    pub fn made_pictures(&self, ready: &[String]) {
        if ready.is_empty() {
            return;
        }
        let made: std::collections::HashSet<&str> = ready.iter().map(String::as_str).collect();
        let mut again = 0usize;
        let mut pages = self.pages.borrow_mut();
        let held: Vec<usize> = pages.pages().collect();
        for page in held {
            let Some(rows) = pages.rows_mut(page) else {
                continue;
            };
            for kept in rows.iter_mut() {
                if kept.pic == Pic::Missing && made.contains(kept.row.path.as_str()) {
                    kept.pic = Pic::Unknown;
                    again += 1;
                }
            }
        }
        self.unlooked.set(self.unlooked.get() + again);
    }

    /// Path, directory flag and weight of a row, in one borrow: the menu needs
    /// all three, and a row can be replaced between two separate looks.
    pub fn what_at(&self, row: usize) -> Option<(String, bool, i64)> {
        let pages = self.pages.borrow();
        pages
            .at(row)
            .map(|k| (k.row.path.to_string(), k.row.is_dir, k.bytes))
            .filter(|(p, _, _)| !p.is_empty())
    }

    /// The path of a row, if its page is in hand.
    pub fn path_at(&self, row: usize) -> Option<String> {
        let pages = self.pages.borrow();
        pages
            .at(row)
            .map(|k| k.row.path.to_string())
            .filter(|p| !p.is_empty())
    }

    /// How long the result is.
    pub fn length(&self) -> usize {
        self.pages.borrow().total()
    }

    /// How many rows are held, over all the pages kept.
    pub fn held(&self) -> usize {
        let pages = self.pages.borrow();
        let held: Vec<usize> = pages.pages().collect();
        held.iter()
            .map(|page| {
                (0..SPAN)
                    .filter(|i| pages.at(page * SPAN + i).is_some())
                    .count()
            })
            .sum()
    }

    /// How many times the view has been told to re-measure. See [`Rows`].
    #[cfg(test)]
    pub fn resets(&self) -> u64 {
        self.resets.get()
    }

    /// One past the last row held, over all the pages kept.
    fn held_to(&self) -> usize {
        let pages = self.pages.borrow();
        pages
            .pages()
            .map(|page| {
                page * SPAN
                    + (0..SPAN)
                        .filter(|i| pages.at(page * SPAN + i).is_some())
                        .count()
            })
            .max()
            .unwrap_or(0)
    }
}

impl slint::Model for Rows {
    type Data = Row;

    fn row_count(&self) -> usize {
        self.pages.borrow().total()
    }

    fn row_data(&self, row: usize) -> Option<Row> {
        // The borrow must end with this statement: what follows takes a mutable
        // one, and a live `Ref` there is a panic rather than a compile error.
        let drawn = self.pages.borrow().at(row).map(|k| k.row.clone());
        if let Some(drawn) = drawn {
            // Only when the eye crosses a page boundary, not once per row drawn.
            let page = Self::page_of(row);
            if self.touched.get() != page {
                self.touched.set(page);
                self.pages.borrow_mut().touched(row);
            }
            return Some(drawn);
        }
        // Not in hand. Remember the first such row — a run of misses wants one
        // page — and give back a blank so the list keeps its shape.
        if self.want.get().is_none() {
            self.want.set(Some(row));
        }
        Some(Row::default())
    }

    fn model_tracker(&self) -> &dyn slint::ModelTracker {
        &self.notify
    }
}

/// The same results, a line at a time, for the tile views. A line must itself be
/// a model: a `for` over an integer builds every element at once, which is a list
/// that has stopped being lazy and stopped fetching.
pub struct Lines {
    rows: std::rc::Rc<Rows>,
    /// Tiles on a line; zero while the table is showing, so nothing is built.
    per: Flag<usize>,
    /// The length this last told the view about. See [`Lines::sync`].
    shown: Flag<usize>,
    notify: slint::ModelNotify,
}

impl Lines {
    pub fn new(rows: std::rc::Rc<Rows>) -> Lines {
        Lines {
            rows,
            per: Flag::new(0),
            shown: Flag::new(0),
            notify: slint::ModelNotify::default(),
        }
    }

    /// How many tiles fit on a line, or zero while the table is showing.
    pub fn per_line(&self, per: usize) {
        if per != self.per.get() {
            self.per.set(per);
            // Every line holds different results now, so this one is a reset.
            self.shown.set(self.lines());
            self.notify.reset();
        }
    }

    /// Tell the view if the result has changed length under it. Checked on a
    /// timer: both the row count and the tiles per line move on their own.
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
    /// [`Rows`] for why not a reset.
    fn stretch(&self) {
        let was = self.shown.get();
        let now = self.lines();
        self.shown.set(now);
        if was == 0 && now > 0 {
            self.notify.reset();
            return;
        }
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
        rows.put(0, page(SPAN), Vec::new(), 2_500_000);
        assert_eq!(rows.row_count(), 2_500_000);
        assert_eq!(rows.held(), SPAN);
    }

    // The notifications actually delivered to Slint's model peer.
    #[derive(Default)]
    struct Notifications(RefCell<Vec<(char, usize, usize)>>);

    impl slint::private_unstable_api::re_exports::ModelChangeListener for Notifications {
        fn row_changed(self: std::pin::Pin<&Self>, _: usize) {}
        fn row_added(self: std::pin::Pin<&Self>, at: usize, n: usize) {
            self.0.borrow_mut().push(('+', at, n));
        }
        fn row_removed(self: std::pin::Pin<&Self>, at: usize, n: usize) {
            self.0.borrow_mut().push(('-', at, n));
        }
        fn reset(self: std::pin::Pin<&Self>) {
            self.0.borrow_mut().push(('r', 0, 0));
        }
    }

    #[test]
    fn first_population_is_lazy_but_later_growth_preserves_the_viewport() {
        use slint::private_unstable_api::re_exports::ModelChangeListenerContainer;
        let rows = std::rc::Rc::new(Rows::default());
        let lines = Lines::new(std::rc::Rc::clone(&rows));
        lines.per_line(4);
        let table = Box::pin(ModelChangeListenerContainer::<Notifications>::default());
        let grid = Box::pin(ModelChangeListenerContainer::<Notifications>::default());
        rows.model_tracker()
            .attach_peer(table.as_ref().model_peer());
        lines
            .model_tracker()
            .attach_peer(grid.as_ref().model_peer());
        rows.put(0, page(SPAN), Vec::new(), 5_000_000);
        lines.sync();
        assert_eq!(*table.as_ref().get().0.borrow(), [('r', 0, 0)]);
        assert_eq!(*grid.as_ref().get().0.borrow(), [('r', 0, 0)]);
        rows.set_total(5_000_004);
        lines.sync();
        assert_eq!(table.as_ref().get().0.borrow()[1], ('+', 5_000_000, 4));
        assert_eq!(grid.as_ref().get().0.borrow()[1], ('+', 1_250_000, 1));
        rows.set_total(5_000_000);
        lines.sync();
        assert_eq!(table.as_ref().get().0.borrow()[2], ('-', 5_000_000, 4));
        assert_eq!(grid.as_ref().get().0.borrow()[2], ('-', 1_250_000, 1));
        assert_eq!(rows.held(), SPAN);
    }

    #[test]
    fn a_page_landing_in_place_does_not_make_the_view_re_measure() {
        // The live refresh: same query, same length, new rows. A reset here
        // rebuilds the list and re-clamps the viewport.
        let rows = Rows::default();
        rows.put(0, page(SPAN), Vec::new(), 10_000);
        let after_first = rows.resets();
        rows.put(0, page(SPAN), Vec::new(), 10_000);
        rows.put(1, page(SPAN), Vec::new(), 10_000);
        assert_eq!(
            rows.resets(),
            after_first,
            "no reset while the length holds"
        );

        // And when the length really does change, the view has to be told.
        rows.put(2, page(SPAN), Vec::new(), 20_000);
        assert_eq!(rows.resets(), after_first + 1);
    }

    #[test]
    fn the_exact_count_lengthens_the_list_it_does_not_reload_it() {
        // The search counts to its cap; the exact total follows a moment later.
        let rows = Rows::default();
        rows.put(0, page(SPAN), Vec::new(), 1_000);
        rows.set_total(2_481_902);
        assert_eq!(rows.row_count(), 2_481_902);
        assert_eq!(rows.held(), SPAN, "the page it was showing is intact");
        rows.set_total(2_481_902);
        assert_eq!(rows.resets(), 2, "and saying it twice costs nothing");
    }

    #[test]
    fn a_row_outside_the_pages_in_hand_is_asked_for_once() {
        let rows = Rows::default();
        rows.put(0, page(SPAN), Vec::new(), 10_000);
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
        rows.put(0, page(SPAN), Vec::new(), 10_000);
        // The screen is covered, so the next page is the one ahead of it.
        assert_eq!(rows.next_page(0, 24, true, true), Some(1));
        rows.asking(1, 0);
        assert_eq!(
            rows.next_page(0, 24, true, true),
            None,
            "and it is not asked for twice"
        );
        rows.put(1, page(SPAN), Vec::new(), 10_000);
        // Now ahead is held too, so nothing is wanted until the eye moves.
        assert_eq!(rows.next_page(0, 24, true, true), None);
        // Two pages down, what is on screen wins over what is beside it.
        assert_eq!(rows.next_page(SPAN * 3, SPAN * 3 + 24, true, true), Some(3));
        // At the top of the list there is nothing behind to fetch.
        rows.put(2, page(SPAN), Vec::new(), 10_000);
        rows.put(3, page(SPAN), Vec::new(), 10_000);
        assert_eq!(rows.next_page(0, 24, true, true), None);
    }

    #[test]
    fn nothing_is_guessed_at_while_a_page_is_expensive() {
        // Deep in a result a page costs a walk of everything above it, so only
        // what is on screen is fetched.
        let rows = Rows::default();
        rows.put(9, page(SPAN), Vec::new(), 4_000_000);
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
        rows.put(0, page(SPAN), Vec::new(), 10_000);
        rows.put(1, page(SPAN), Vec::new(), 10_000);
        rows.mark(7);
        // On screen: re-read, because what it shows may be out of date.
        assert_eq!(rows.next_page(0, 24, true, true), Some(0));
        rows.asking(0, 7);
        rows.put(0, page(SPAN), Vec::new(), 10_000);
        // Off screen: left alone, or a moving index refetches every page seen.
        assert_eq!(rows.next_page(0, 24, true, true), None);
    }

    #[test]
    fn scrolling_back_over_something_already_seen_asks_for_nothing() {
        // The point of keeping pages: going back over what was read asks nothing.
        let rows = Rows::default();
        for p in 0..8 {
            rows.put(p, page(SPAN), Vec::new(), 10_000);
        }
        // Page 7 still wants the one after it: the fetch ahead of the eye.
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
        for p in 0..scour_page::KEPT + 4 {
            rows.put(p, page(SPAN), Vec::new(), 100_000);
        }
        assert_eq!(rows.held(), scour_page::KEPT * SPAN);
        assert!(
            rows.next_page(0, 24, true, true).is_some(),
            "the oldest went first"
        );
        assert!(
            rows.next_page(
                scour_page::KEPT * SPAN,
                scour_page::KEPT * SPAN + 24,
                true,
                true
            )
            .is_none(),
            "and the newest stayed"
        );
    }

    #[test]
    fn a_page_nobody_had_read_holds_no_arrivals() {
        // "New" is measured against this page's own last read, not against
        // whichever page was fetched last.
        let rows = Rows::default();
        assert!(
            !rows.put(0, named(&["/a", "/b"]), Vec::new(), 10_000),
            "the first read of a page is not an arrival"
        );
        assert!(
            !rows.put(7, named(&["/c", "/d"]), Vec::new(), 10_000),
            "nor is the first read of another page"
        );
        assert!(!rows.row_data(7 * SPAN).unwrap().fresh);

        // But a page that comes back holding something it did not before is
        // exactly what the wash is for.
        assert!(rows.put(7, named(&["/c", "/new", "/d"]), Vec::new(), 10_000));
        assert!(!rows.row_data(7 * SPAN).unwrap().fresh, "/c was here");
        assert!(rows.row_data(7 * SPAN + 1).unwrap().fresh, "/new was not");
        assert!(!rows.row_data(7 * SPAN + 2).unwrap().fresh, "/d was here");

        // And the same page unchanged says nothing at all.
        assert!(!rows.put(7, named(&["/c", "/new", "/d"]), Vec::new(), 10_000));
    }

    #[test]
    fn the_arrival_flags_come_off_what_is_held_and_nothing_else() {
        let rows = Rows::default();
        // Read twice, the second time with a row the first did not have.
        rows.put(5, named(&["/a", "/b", "/c", "/d"]), Vec::new(), 2_000_000);
        assert!(rows.put(5, named(&["/a", "/new", "/c", "/d"]), Vec::new(), 2_000_000));
        let before = rows.resets();
        rows.clear_fresh();
        assert!(!rows.row_data(5 * SPAN + 1).unwrap().fresh);
        assert_eq!(rows.resets(), before, "clearing a flag is not a re-measure");
        assert_eq!(rows.wanted(), None, "and it asks for nothing");
    }

    #[test]
    fn a_selection_knows_what_it_holds_and_what_it_weighs() {
        // Weights come with the page and never go through Slint, whose numbers
        // are 32-bit; the size column holds text like `1.30 MiB`.
        let rows = Rows::default();
        rows.put(
            3,
            named(&["/a/one.txt", "/a/two.txt", "/b"]),
            vec![1_000, 3_000_000_000, 0],
            10_000,
        );
        let pick = rows.pick_at(3 * SPAN + 1).expect("in hand");
        assert_eq!(pick.path, "/a/two.txt");
        assert_eq!(pick.bytes, 3_000_000_000, "past what an i32 holds");
        assert_eq!(pick.folder(), "/a");
        assert_eq!(rows.pick_at(3 * SPAN + 9), None, "past the page's end");
        assert_eq!(rows.pick_at(0), None, "a page that is not in hand");
        // A path at the root has the root for a folder, not an empty string.
        assert_eq!(rows.pick_at(3 * SPAN + 2).unwrap().folder(), "/");
    }

    #[test]
    fn what_is_picked_stays_picked_while_it_scrolls_away_and_back() {
        // The selection is by path; the rows it paints come and go with pages.
        let rows = Rows::default();
        rows.put(0, named(&["/a", "/b", "/c"]), Vec::new(), 10_000);
        let picked: std::collections::HashSet<String> = ["/b".to_string()].into_iter().collect();
        rows.mark_picked(&picked);
        assert!(rows.row_data(1).unwrap().picked);
        assert!(!rows.row_data(0).unwrap().picked);
        // The page is read again — a live refresh — and the mark is put back.
        rows.put(0, named(&["/a", "/b", "/c"]), Vec::new(), 10_000);
        rows.mark_picked(&picked);
        assert!(rows.row_data(1).unwrap().picked);
        // And dropping the selection unpaints it.
        rows.mark_picked(&std::collections::HashSet::new());
        assert!(!rows.row_data(1).unwrap().picked);
    }

    #[test]
    fn the_row_a_list_of_millions_calls_four_thousand_is_the_right_file() {
        // A page index read as though the page began at row zero opens the
        // wrong file, two hundred rows away.
        let rows = Rows::default();
        rows.put(
            20,
            named(&["/a/one.txt", "/a/two.txt"]),
            Vec::new(),
            2_000_000,
        );
        assert_eq!(rows.path_at(20 * SPAN + 1).as_deref(), Some("/a/two.txt"));
        assert_eq!(rows.path_at(20 * SPAN + 2), None, "past the page's end");
        assert_eq!(rows.path_at(0), None, "a page that is not in hand");
    }

    #[test]
    fn a_new_question_empties_the_pages() {
        let rows = Rows::default();
        rows.put(0, named(&["/old"]), Vec::new(), 10_000);
        rows.empty();
        assert_eq!(rows.path_at(0), None);
        assert_eq!(rows.next_page(0, 24, true, true), Some(0));
    }
}
