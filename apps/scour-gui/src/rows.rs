//! Turning a reply into what the window draws.
//!
//! All formatting stays on this side of the language boundary: a `.slint` file
//! that sized bytes or folded Turkish text would be a second copy of what
//! `scour-core` already has.

// `Cell` here is the interior-mutability one; the table's is `crate::Cell`.
use std::cell::{Cell as Flag, RefCell};

use scour_core::{Hit, Kind, text::Folder};

use crate::{Cell, Found, Row};

/// Whether two rows draw the same. Not `==`: the cells are a model, and two
/// models built from the same text are two models.
fn same_row(a: &Row, b: &Row) -> bool {
    use slint::Model;
    a.name == b.name
        && a.path == b.path
        && a.fresh == b.fresh
        && a.ktoken == b.ktoken
        && a.tint == b.tint
        && a.ext == b.ext
        && a.is_dir == b.is_dir
        && a.age == b.age
        && a.picked == b.picked
        && a.thumb == b.thumb
        && a.shot == b.shot
        && a.has_sys_icon == b.has_sys_icon
        && a.sys_icon == b.sys_icon
        && a.runs == b.runs
        && a.cells.row_count() == b.cells.row_count()
        && a.cells.iter().zip(b.cells.iter()).all(|(x, y)| x == y)
}

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

fn one_edit(a: &[char], b: &[char]) -> bool {
    if a == b {
        return true;
    }
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let mut i = 0;
    while i < a.len() && i < b.len() && a[i] == b[i] {
        i += 1;
    }
    if a.len() == b.len() {
        a[i + 1..] == b[i + 1..]
            || (i + 1 < a.len() && a[i] == b[i + 1] && a[i + 1] == b[i] && a[i + 2..] == b[i + 2..])
    } else if a.len() > b.len() {
        a[i + 1..] == b[i..]
    } else {
        a[i..] == b[i + 1..]
    }
}

/// A name in seven parts — before, mark, between, mark, between, mark,
/// after — with every place each plain term is marked, up to three. With
/// none, the one place `~` or `letters:` finds, as [`split_at_match`] does.
pub fn name_parts<'a>(name: &'a str, terms: &[String]) -> [&'a str; 7] {
    // `a;b` and `a|b` are either word: each is marked where it is.
    let plain: Vec<&str> = terms
        .iter()
        .map(String::as_str)
        .filter(|t| !t.starts_with('^') && !t.starts_with('~'))
        .flat_map(|t| t.split([';', '|']))
        .collect();
    let marks = scour_core::Unaccented::marks(name, &plain);
    if marks.is_empty() {
        let (pre, hit, post) = split_at_match(name, terms);
        return [pre, hit, "", "", "", "", post];
    }
    let mut parts = [""; 7];
    let mut at = 0;
    for (k, &(a, b)) in marks.iter().take(3).enumerate() {
        parts[k * 2] = &name[at..a];
        parts[k * 2 + 1] = &name[a..b];
        at = b;
    }
    // Past the third mark the rest is plain: three are what a row can show.
    let shown = marks.len().min(3);
    parts[shown * 2] = &name[at..];
    if shown < 3 {
        // What follows the last mark is the part drawn last.
        parts[6] = parts[shown * 2];
        parts[shown * 2] = "";
    }
    parts
}

/// A name cut into what precedes the match, the match, and what follows.
/// The search runs on folded text and Turkish folding changes byte lengths, so
/// an offset goes back through `fold_indexed` before it touches the original.
pub fn split_at_match<'a>(name: &'a str, terms: &[String]) -> (&'a str, &'a str, &'a str) {
    // Nothing to mark: no fold of the name, which is two allocations a row.
    if terms.is_empty() {
        return (name, "", "");
    }
    // As a name is searched: `sirket` marks its run in `Şirket`.
    let folder = scour_core::text::Unaccented;
    let (folded, back) = folder.fold_indexed(name);
    let mut best: Option<(usize, usize)> = None;
    for term in terms {
        if term.starts_with('^') || term.starts_with('~') {
            continue;
        }
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
    // Approximate 1-edit fallback for `fuzzy:` (`~word`) terms.
    if best.is_none() {
        let chars: Vec<(usize, char)> = folded.char_indices().collect();
        for term in terms.iter().filter_map(|t| t.strip_prefix('~')) {
            let want: Vec<char> = folder.fold(term).chars().collect();
            if want.len() < 3 {
                continue;
            }
            'lens: for len in [want.len() - 1, want.len(), want.len() + 1] {
                if len < 2 || len > chars.len() {
                    continue;
                }
                for w in 0..=chars.len() - len {
                    let sub: Vec<char> = chars[w..w + len].iter().map(|(_, c)| *c).collect();
                    if one_edit(&sub, &want)
                        && !sub
                            .last()
                            .is_some_and(|c| matches!(c, '.' | '_' | '-' | ' '))
                    {
                        let from = chars[w].0;
                        let to = chars.get(w + len).map_or(folded.len(), |(p, _)| *p);
                        best = Some((from, to));
                        break 'lens;
                    }
                }
            }
            if best.is_some() {
                break;
            }
        }
    }
    // Subsequence `letters:` (`^word`) span fallback: highlights the first coalesced run.
    if best.is_none() {
        for term in terms.iter().filter_map(|t| t.strip_prefix('^')) {
            if let Some(spans) = scour_ui::query::subseq_spans(name, term)
                && let Some(&(a, b)) = spans.first()
            {
                return (&name[..a], &name[a..b], &name[b..]);
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
pub(crate) fn tint_of(token: &str) -> slint::Brush {
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
    pub system_icons: bool,
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
            let [pre, hit, mid, hit2, mid2, hit3, post] = name_parts(shown_name(h), terms);
            Cell {
                pre: pre.into(),
                hit: hit.into(),
                mid: mid.into(),
                hit2: hit2.into(),
                mid2: mid2.into(),
                hit3: hit3.into(),
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

/// What the name column says. A drive's root has no name of its own — `C:/`
/// ends in its separator — so it is called by its path, as Explorer calls it.
pub fn shown_name(h: &Hit) -> &str {
    match h.name() {
        "" => h.path.trim_end_matches('/'),
        name => name,
    }
}

pub fn row_of(h: &Hit, terms: &[String], shape: &Shape, fresh: bool) -> Row {
    let cells: Vec<Cell> = shape
        .columns
        .iter()
        .map(|c| cell_of(h, c.id, shape, terms))
        .collect();
    let (sys_icon, has_sys_icon) = if shape.system_icons {
        cached_system_icon(h)
    } else {
        (blank(), false)
    };
    Row {
        cells: slint::ModelRc::new(slint::VecModel::from(cells)),
        found: found_of(h),
        name: shown_name(h).into(),
        path: h.path.as_str().into(),
        fresh,
        ktoken: h.kind.token().into(),
        tint: tint_of(h.kind.token()),
        ext: if h.is_dir {
            slint::SharedString::new()
        } else {
            badge_of(h.name()).into()
        },
        is_dir: h.is_dir,
        age: band(shape.now, h.meta.mtime),
        picked: false,
        // Empty; filled in after the row is on screen by `look_for_pictures`.
        thumb: blank(),
        shot: false,
        sys_icon,
        has_sys_icon,
        runs: !h.is_dir && scour_core::runs_when_opened(h.name(), h.meta.mode),
    }
}

/// The extension as the file icon spells it: up to five letters or digits, in
/// capitals. Empty for a name with none — `.bashrc` is a name, not a type — and
/// for one too long to read at that size.
pub fn badge_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && (1..=5).contains(&ext.chars().count())
                && ext.chars().all(char::is_alphanumeric) =>
        {
            ext.to_uppercase()
        }
        _ => String::new(),
    }
}

/// A line out of a file in seven parts, every find marked — up to three —
/// as [`Marked`] draws it: tabs and line ends as spaces, the indent gone, and
/// only the last `before` characters ahead of the first find, so it is in
/// sight however far into the line it is.
pub(crate) fn line_parts(
    text: &str,
    at: (u32, u32),
    marks: &[(u32, u32)],
    before: usize,
) -> [String; 7] {
    let tidy = |s: &str| s.replace(['\t', '\r', '\n'], " ");
    let one = [at];
    let marks: Vec<(usize, usize)> = (if marks.is_empty() { &one[..] } else { marks })
        .iter()
        .map(|&(a, l)| (a as usize, (a + l) as usize))
        .filter(|&(a, b)| a < b && text.get(a..b).is_some())
        .collect();
    let mut parts: [String; 7] = Default::default();
    if marks.is_empty() {
        parts[0] = tidy(text);
        return parts;
    }
    let mut at = 0;
    for (k, &(a, b)) in marks.iter().take(3).enumerate() {
        parts[k * 2] = tidy(&text[at..a]);
        parts[k * 2 + 1] = tidy(&text[a..b]);
        at = b;
    }
    // What follows the last mark shown is drawn last, a fourth find plain.
    parts[6] = tidy(&text[at..]);
    let pre = parts[0].trim_start().to_owned();
    let kept = pre.chars().count();
    parts[0] = if kept > before {
        let cut: String = pre.chars().skip(kept - before).collect();
        format!("…{}", cut.trim_start())
    } else {
        pre
    };
    parts
}

/// Where a `content:` query found its text in the file, as a row and the
/// preview draw it. A line is one line: tabs become spaces, the indent goes,
/// and only the last few dozen characters before the find are kept, so the
/// find is in sight however far into the line it is.
fn found_of(h: &Hit) -> slint::ModelRc<Found> {
    if h.found.is_empty() {
        return slint::ModelRc::default();
    }
    const BEFORE: usize = 40;
    let found: Vec<Found> = h
        .found
        .iter()
        .map(|f| {
            let [pre, hit, mid, hit2, mid2, hit3, post] =
                line_parts(&f.text, f.at, &f.marks, BEFORE);
            Found {
                line: f.line.map(|n| n.to_string()).unwrap_or_default().into(),
                pre: pre.into(),
                hit: hit.into(),
                mid: mid.into(),
                hit2: hit2.into(),
                mid2: mid2.into(),
                hit3: hit3.into(),
                post: post.into(),
            }
        })
        .collect();
    slint::ModelRc::new(slint::VecModel::from(found))
}

fn cached_system_icon(h: &Hit) -> (slint::Image, bool) {
    use scour_thumbs::sysicon::{SystemIcon, key_for_entry, lookup};
    use std::collections::HashMap;

    thread_local! {
        static CACHE: RefCell<HashMap<String, Option<slint::Image>>> =
            RefCell::new(HashMap::new());
    }
    let key = key_for_entry(&h.path, h.name(), h.is_dir, h.kind.token());
    CACHE.with(|cell| {
        if let Some(found) = cell.borrow().get(&key) {
            return match found {
                Some(img) => (img.clone(), true),
                None => (blank(), false),
            };
        }
        // Request 64px so vector SVGs stay vector and raster OS icons stay sharp on HiDPI and grid views.
        let built = lookup(&key, 64).and_then(|ic| match ic {
            SystemIcon::Svg(bytes) => slint::Image::load_from_svg_data(&bytes).ok(),
            SystemIcon::Png(bytes) => {
                let rgba = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                    .ok()?
                    .into_rgba8();
                let (w, h) = rgba.dimensions();
                (w > 0 && h > 0).then(|| {
                    slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
                        rgba.as_raw(),
                        w,
                        h,
                    ))
                })
            }
            SystemIcon::Rgba {
                width,
                height,
                rgba,
            } => (width > 0 && height > 0 && rgba.len() == (width as usize * height as usize * 4))
                .then(|| {
                    slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
                        &rgba, width, height,
                    ))
                }),
        });
        let mut map = cell.borrow_mut();
        if map.len() >= 1024 {
            map.clear();
        }
        map.insert(key, built.clone());
        match built {
            Some(img) => (img, true),
            None => (blank(), false),
        }
    })
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
    fn every_term_is_marked_up_to_three_places() {
        let terms = ["hasan".to_string(), "salih".to_string()];
        assert_eq!(
            name_parts("030713_Hasan_Salihamidi.rtf", &terms),
            ["030713_", "Hasan", "_", "Salih", "", "", "amidi.rtf"]
        );
        // A name that begins with its match has nothing before it.
        assert_eq!(
            name_parts("HASAN SALİHOĞLU", &terms),
            ["", "HASAN", " ", "SALİH", "", "", "OĞLU"]
        );
        let many = name_parts("a-a-a-a", &["a".to_string()]);
        assert_eq!(many, ["", "a", "-", "a", "-", "a", "-a"]);
        assert_eq!(
            name_parts("main.rs", &[]),
            ["main.rs", "", "", "", "", "", ""]
        );
        // A line out of a file: every find the service marked.
        let p = line_parts(
            "icra dairesi ve icra",
            (0, 4),
            &[(0, 4), (5, 7), (16, 4)],
            40,
        );
        assert_eq!(
            p,
            ["", "icra", " ", "dairesi", " ve ", "icra", ""].map(String::from)
        );
        // An older service sends `at` alone.
        let p = line_parts("bir icra\tdosyası", (4, 4), &[], 40);
        assert_eq!(
            p,
            ["bir ", "icra", "", "", "", "", " dosyası"].map(String::from)
        );
        // Either word of an OR.
        assert_eq!(
            name_parts("rapor-belge.pdf", &["belge;rapor".to_string()]),
            ["", "rapor", "-", "belge", "", "", ".pdf"]
        );
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
    /// The kind's token, for the preview's count of several rows by kind.
    pub ktoken: String,
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
    /// The list's slots, told of every change the view is told of.
    slots: RefCell<std::rc::Weak<Slots>>,
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
            slots: RefCell::new(std::rc::Weak::new()),
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
                let mut had = 0;
                for kept in (0..SPAN).filter_map(|i| pages.at(page * SPAN + i)) {
                    let path = kept.row.path.to_string();
                    if kept.pic == Pic::Shown {
                        drawn.insert(path.clone(), kept.row.thumb.clone());
                    }
                    known.insert(path, kept.pic);
                    had += 1;
                }
                // Only as far as the page reached before: a first page cut to a
                // screenful, read whole, brings the rest of the page, not arrivals.
                for row in rows.iter_mut().take(had) {
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
        // What the rows were, so a refetch that brings the same rows redraws
        // none of them. The index moves every second on a busy machine and the
        // page in sight is asked again each time: 200 rows told they changed
        // was the window's 3% of a core while nothing on screen did.
        let before: Vec<Option<Row>> = {
            let pages = self.pages.borrow();
            (0..SPAN)
                .map(|i| pages.at(page * SPAN + i).map(|k| k.row.clone()))
                .collect()
        };
        let change = self.pages.borrow_mut().put(page, kept, total);
        self.want.set(None);
        match change {
            scour_page::Change::Rows { from, to } => {
                let differ: Vec<usize> = {
                    let pages = self.pages.borrow();
                    (from..to)
                        .filter(|&row| {
                            let was = before.get(row - from).and_then(Option::as_ref);
                            match (was, pages.at(row).map(|k| &k.row)) {
                                (Some(a), Some(b)) => !same_row(a, b),
                                (None, None) => false,
                                _ => true,
                            }
                        })
                        .collect()
                };
                crate::trace(&format!(
                    "page {page}: {} of {} rows changed",
                    differ.len(),
                    to - from
                ));
                for row in differ {
                    self.changed(row);
                }
            }
            other => self.tell(other),
        }
        arrived
    }

    /// One row is not what it was: the view and the slot showing it are told.
    fn changed(&self, row: usize) {
        self.notify.row_changed(row);
        if let Some(slots) = self.slots.borrow().upgrade() {
            slots.row_changed(row);
        }
    }

    /// Pass on what a page call changed, in the terms the view understands.
    fn tell(&self, change: scour_page::Change) {
        match change {
            scour_page::Change::Nothing => {}
            scour_page::Change::Rows { from, to } => {
                for row in from..to {
                    self.changed(row);
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
                if let Some(slots) = self.slots.borrow().upgrade() {
                    slots.refresh();
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
            self.changed(row);
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
            ktoken: found.row.ktoken.to_string(),
        })
    }

    /// The kind's token of a row, for the preview's picture while it waits.
    pub fn ktoken_at(&self, row: usize) -> Option<String> {
        let pages = self.pages.borrow();
        pages.at(row).map(|k| k.row.ktoken.to_string())
    }

    /// Find which row index holds `path` among the pages currently in hand.
    pub fn find_path(&self, path: &str) -> Option<usize> {
        if path.is_empty() {
            return None;
        }
        let pages = self.pages.borrow();
        let mut held: Vec<usize> = pages.pages().collect();
        held.sort_unstable();
        for page in held {
            for i in 0..SPAN {
                let Some(kept) = pages.at(page * SPAN + i) else {
                    break;
                };
                if kept.row.path.as_str() == path {
                    return Some(page * SPAN + i);
                }
            }
        }
        None
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
            self.changed(row);
        }
    }

    /// Inspect the rows in sight and split them into `(ready for off-thread
    /// pixel decode, worth asking the service to generate)`. Bounded to the
    /// range, to unlooked rows and to `batch`.
    pub fn look_for_pictures(
        &self,
        from: usize,
        to: usize,
        batch: usize,
    ) -> (Vec<(String, crate::tiles::TileSource)>, Vec<String>) {
        if self.unlooked.get() == 0 {
            return (Vec::new(), Vec::new());
        }
        let mut decode = Vec::new();
        let mut ask = Vec::new();
        let mut looked = 0usize;
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
            if scour_thumbs::never_for(&token) {
                kept.pic = Pic::Nothing;
            } else if let Some(picture) = scour_thumbs::cache::existing(&path) {
                kept.pic = Pic::Missing;
                decode.push((path, crate::tiles::TileSource::CachedPng(picture)));
            } else if scour_thumbs::may_native(&path, &token) {
                kept.pic = Pic::Missing;
                decode.push((path, crate::tiles::TileSource::Native));
            } else if scour_thumbs::may(&path, &token) {
                kept.pic = Pic::Missing;
                ask.push(path);
            } else if crate::has_direct_image_ext(&path) {
                kept.pic = Pic::Missing;
                decode.push((path, crate::tiles::TileSource::DirectImage));
            } else {
                kept.pic = Pic::Nothing;
            }
            row += 1;
        }
        (decode, ask)
    }

    /// Attach a decoded RGBA pixel buffer (or record its absence) to any held
    /// row waiting on `path`, returning the row indices whose drawing changed.
    pub fn give_picture(&self, path: &str, outcome: &crate::tiles::TileOutcome) -> Vec<usize> {
        let mut drawn = Vec::new();
        {
            let mut pages = self.pages.borrow_mut();
            let held: Vec<usize> = pages.pages().collect();
            for page in held {
                let Some(rows) = pages.rows_mut(page) else {
                    continue;
                };
                for (i, kept) in rows.iter_mut().enumerate() {
                    if kept.pic != Pic::Missing || kept.row.path.as_str() != path {
                        continue;
                    }
                    match outcome {
                        crate::tiles::TileOutcome::Pixels(buf) => {
                            kept.row.thumb = slint::Image::from_rgba8(buf.clone());
                            kept.row.shot = true;
                            kept.pic = Pic::Shown;
                            drawn.push(page * SPAN + i);
                        }
                        crate::tiles::TileOutcome::Nothing => {
                            kept.pic = Pic::Nothing;
                        }
                    }
                }
            }
        }
        for &row in &drawn {
            self.changed(row);
        }
        drawn
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

    /// Where a `content:` query found its text in a row's file, if its page
    /// is in hand.
    pub fn found_at(&self, row: usize) -> slint::ModelRc<Found> {
        let pages = self.pages.borrow();
        pages
            .at(row)
            .map(|k| k.row.found.clone())
            .unwrap_or_default()
    }

    /// The path of a row, if its page is in hand.
    pub fn path_at(&self, row: usize) -> Option<String> {
        let pages = self.pages.borrow();
        pages
            .at(row)
            .map(|k| k.row.path.to_string())
            .filter(|p| !p.is_empty())
    }

    /// The path of a row and whether opening it would start a program.
    pub fn opening_at(&self, row: usize) -> Option<(String, bool)> {
        let pages = self.pages.borrow();
        pages
            .at(row)
            .map(|k| (k.row.path.to_string(), k.row.runs))
            .filter(|(p, _)| !p.is_empty())
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

/// The rows the detail list draws, one slot each, reused as the list moves.
/// A `ListView` builds the row scrolling in and drops the one scrolling out,
/// and Slint repaints the whole window for every element it frees: moving a
/// row a frame cost twice what moving less than a row did. A slot is told its
/// new row and keeps its elements, so a step down changes one slot.
pub struct Slots {
    rows: std::rc::Rc<Rows>,
    /// Which row each slot shows; `None` past the end. Row `r` lives in slot
    /// `r % len`, so the slots of the rows still on screen never move.
    map: RefCell<Vec<Option<usize>>>,
    /// The first row covered, kept for a refresh.
    from: Flag<usize>,
    notify: slint::ModelNotify,
}

impl Slots {
    pub fn new(rows: &std::rc::Rc<Rows>) -> std::rc::Rc<Slots> {
        let slots = std::rc::Rc::new(Slots {
            rows: std::rc::Rc::clone(rows),
            map: RefCell::new(Vec::new()),
            from: Flag::new(0),
            notify: slint::ModelNotify::default(),
        });
        *rows.slots.borrow_mut() = std::rc::Rc::downgrade(&slots);
        slots
    }

    /// Cover `count` rows from `first`. A slot whose row is unchanged is not
    /// told, which is the whole point.
    pub fn place(&self, first: usize, count: usize) {
        let count = count.max(1);
        self.from.set(first);
        let total = slint::Model::row_count(&*self.rows);
        let was = self.map.borrow().len();
        let mut changed = Vec::new();
        {
            let mut map = self.map.borrow_mut();
            if was != count {
                // Another length moves every row to another slot.
                map.clear();
                map.resize(count, None);
            }
            for row in first..first + count {
                let slot = row % count;
                let want = (row < total).then_some(row);
                if map[slot] != want {
                    map[slot] = want;
                    changed.push(slot);
                }
            }
        }
        if count > was {
            self.notify.row_added(was, count - was);
        } else if count < was {
            self.notify.row_removed(count, was - count);
        }
        for slot in changed {
            self.notify.row_changed(slot);
        }
    }

    /// Row `row` changed; the slot showing it, if any, is told.
    fn row_changed(&self, row: usize) {
        let len = self.map.borrow().len();
        if len == 0 {
            return;
        }
        let slot = row % len;
        if self.map.borrow()[slot] == Some(row) {
            self.notify.row_changed(slot);
        }
    }

    /// The rows changed length: every slot again, past the end included.
    fn refresh(&self) {
        let len = self.map.borrow().len();
        if len == 0 {
            return;
        }
        self.place(self.from.get(), len);
        for slot in 0..len {
            self.notify.row_changed(slot);
        }
    }
}

impl slint::Model for Slots {
    type Data = crate::Slot;

    fn row_count(&self) -> usize {
        self.map.borrow().len()
    }

    fn row_data(&self, slot: usize) -> Option<crate::Slot> {
        let at = *self.map.borrow().get(slot)?;
        Some(
            match at.and_then(|row| Some((row, self.rows.row_data(row)?))) {
                Some((row, data)) => crate::Slot {
                    i: row as i32,
                    row: data,
                },
                None => crate::Slot {
                    i: -1,
                    row: Row {
                        thumb: blank(),
                        sys_icon: blank(),
                        ..Row::default()
                    },
                },
            },
        )
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
mod model_tests;
