//! The preview column: what the service says a file is, drawn. Four layers,
//! as `scour_ui::preview` orders them: what it is and where, what can be done,
//! what is in it, the facts.

use super::*;
use crate::ui::{PeekAct, PeekCrumb, PeekHeavy, PeekNew};
use link::{Ask, Link};
use scour_i18n::Catalogue;
use scour_ui::preview as says;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

/// A folder's preview as its three answers arrive: its folders by weight, the
/// files directly in it by size, its newest files anywhere under it.
#[derive(Default)]
pub(super) struct DirParts {
    pub(super) usage: Option<scour_core::UsageResponse>,
    pub(super) files: Option<Vec<scour_core::Hit>>,
    pub(super) newest: Option<Vec<scour_core::Hit>>,
}

/// Keep the preview panel on whatever is selected. **Asked once per row, not per
/// tick**: what it is showing is remembered as a path.
pub(super) fn peek(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    rows: &Rc<rows::Rows>,
    picks: &Rc<RefCell<selection::Selection>>,
    cat: &Catalogue,
) {
    if !w.get_peeking() {
        if !state.borrow().peek_path.is_empty() {
            state.borrow_mut().peek_path.clear();
        }
        return;
    }
    // While the stream of files read for a pattern is the list, the preview is
    // on one of its files — not on the ordinary list behind it.
    if w.get_grep_active() {
        return;
    }
    // Several rows are their own preview, and nothing is read for it.
    {
        let held = picks.borrow();
        if held.rows.len() > 1 {
            let key = many_key(&held);
            if key == state.borrow().peek_path {
                return;
            }
            state.borrow_mut().peek_path = key;
            draw_many(w, cat, &held);
            return;
        }
    }
    let selected = w.get_selected();
    let (path, is_dir) = if selected < 0 {
        (String::new(), false)
    } else {
        rows.what_at(selected as usize)
            .map(|(p, d, _)| (p, d))
            .unwrap_or_default()
    };
    if path == state.borrow().peek_path {
        return;
    }
    {
        let mut s = state.borrow_mut();
        s.peek_path = path.clone();
        s.peek_dir = DirParts::default();
    }
    if path.is_empty() {
        blank(w);
        w.set_peek_name(slint::SharedString::new());
        w.set_peek_path(slint::SharedString::new());
        w.set_peek_crumbs(ModelRc::default());
        w.set_peek_acts(ModelRc::default());
        return;
    }
    let ktoken = rows.ktoken_at(selected.max(0) as usize).unwrap_or_default();
    head_of(w, cat, &path, is_dir, &ktoken);
    w.set_peek_found(rows.found_at(selected.max(0) as usize));
    if is_dir {
        // Once the hand has stopped: a folder's answers cost the service a
        // walk, and arrowing through folders queued them in front of every
        // other answer the window waits for.
        let generation = DIR_GEN.with(|g| {
            g.set(g.get() + 1);
            g.get()
        });
        let (state, link, path) = (Rc::clone(state), Rc::clone(link), path.clone());
        slint::Timer::single_shot(std::time::Duration::from_millis(DIR_WAIT_MS), move || {
            if DIR_GEN.with(std::cell::Cell::get) == generation && state.borrow().peek_path == path
            {
                ask_dir(&link, &path, generation);
            }
        });
    } else {
        // A `content:` query's finds, by line: the text opens on the first.
        let found: Vec<Find> = rows
            .found_at(selected.max(0) as usize)
            .iter()
            .map(|f| match f.line.strip_prefix('@') {
                Some(at) => (None, at.parse().ok(), None),
                None => (f.line.parse().ok(), None, None),
            })
            .collect();
        let first = found.first().copied().unwrap_or_default();
        finds(found);
        link.send(Ask::Peek {
            path: path.clone(),
            at: first.1,
            line: first.0,
        });
    }
    link.send(Ask::PeekFacts { path });
}

/// A find of the file in the preview: its line and its byte, either unknown,
/// and for a document searched in its text, that text's length.
pub(super) type Find = (Option<u64>, Option<u64>, Option<u64>);

thread_local! {
    /// The finds of the file in the preview, and its length from the last
    /// answer: what the rail is drawn from.
    static FINDS: RefCell<Vec<Find>> = const { RefCell::new(Vec::new()) };
    static LEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The finds of the file the preview is about to show.
pub(super) fn finds(found: Vec<Find>) {
    FINDS.with(|f| *f.borrow_mut() = found);
}

/// A document's finds as shares of its text, for the rail beside its pages,
/// and the first of them to open it at.
fn doc_marks(w: &MainWindow) {
    let marks: Vec<f32> = FINDS.with(|f| {
        f.borrow()
            .iter()
            .filter_map(|&(_, offset, len)| Some(offset? as f32 / len?.max(1) as f32))
            .map(|r| r.clamp(0.0, 1.0))
            .collect()
    });
    w.set_peek_doc_at(marks.first().copied().unwrap_or(-1.0));
    w.set_peek_doc_marks(ModelRc::new(VecModel::from(marks)));
}

/// The rail beside the text, after Colpan's minimap: the stretch shown as a
/// band on the whole file when it is not all of it, each find inside it at
/// its byte's share of the text — wrapped lines of one face are near enough
/// even rows — and each one outside it, which only a byte can place.
fn rail(w: &MainWindow, look: &scour_preview::Look, shown: &str) {
    let len = look.len.max(1);
    let whole = !look.cut && look.from == 0;
    let bytes = shown.len().max(1) as u64;
    let (b0, b1) = if whole {
        (0.0, 1.0)
    } else {
        (
            look.from as f32 / len as f32,
            ((look.from + bytes) as f32 / len as f32).min(1.0),
        )
    };
    let first = look.first_line.max(1);
    // Where each line of the text begins, for a find known by its line.
    let starts: Vec<usize> = std::iter::once(0)
        .chain(shown.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let mut inside = Vec::new();
    let mut far = Vec::new();
    FINDS.with(|f| {
        for &(line, offset, doc) in f.borrow().iter() {
            // A document's byte is in its text, not in what is shown.
            if doc.is_some() {
                continue;
            }
            let at = match (offset, line) {
                (Some(o), _) if o >= look.from && o < look.from + bytes => Some(o - look.from),
                (Some(o), _) => {
                    if !whole {
                        far.push(o as f32 / len as f32);
                    }
                    None
                }
                (None, Some(l)) if l >= first => {
                    starts.get((l - first) as usize).map(|&b| b as u64)
                }
                _ => None,
            };
            if let Some(at) = at {
                inside.push(b0 + at as f32 / bytes as f32 * (b1 - b0));
            }
        }
    });
    LEN.with(|l| l.set(len));
    w.set_peek_band0(b0);
    w.set_peek_band1(b1);
    w.set_peek_rail(ModelRc::new(VecModel::from(inside)));
    w.set_peek_far(ModelRc::new(VecModel::from(far)));
    w.set_peek_line_base((first - 1) as i32);
    w.set_peek_line_whole(whole);
}

/// A press on the rail outside the stretch shown: the find nearest it within a
/// few pixels, or the place itself, is read instead.
pub(super) fn reread(w: &MainWindow, link: &Link, ratio: f32, rail_px: f32) {
    let len = LEN.with(std::cell::Cell::get).max(1);
    let near = FINDS.with(|f| {
        f.borrow()
            .iter()
            .filter(|&&(_, _, doc)| doc.is_none())
            .filter_map(|&(_, o, _)| o)
            .map(|o| ((o as f32 / len as f32 - ratio).abs() * rail_px, o))
            .filter(|(d, _)| *d < 8.0)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, o)| o)
    });
    let at = near.unwrap_or((ratio.clamp(0.0, 1.0) * len as f32) as u64);
    link.send(Ask::Peek {
        path: w.get_peek_path().to_string(),
        at: Some(at),
        line: None,
    });
}

/// The heading of one file's preview — its name, its folder as parts, the
/// buttons for one row — with the rest emptied: for the list's row and for the
/// file a content search shows, which the list does not hold.
pub(super) fn head_of(w: &MainWindow, cat: &Catalogue, path: &str, is_dir: bool, ktoken: &str) {
    blank(w);
    w.set_peek_name(scour_ui::path::leaf(path).into());
    w.set_peek_path(path.into());
    w.set_peek_crumbs(crumbs(w, scour_ui::path::folder(path)));
    w.set_peek_acts(acts(cat, is_dir, false, 1));
    w.set_peek_ktoken(ktoken.into());
    w.set_peek_dir(is_dir);
}

/// Everything a row's preview said, taken away: what stands until the new
/// row's answers come is its name, its kind's picture, and nothing of the old.
fn blank(w: &MainWindow) {
    peek::clear(w);
    w.set_peek_can_flow(false);
    w.set_peek_doc_marks(ModelRc::default());
    w.set_peek_doc_at(-1.0);
    w.set_peek_shot(rows::blank());
    w.set_peek_has_shot(false);
    w.set_peek_shot_said(slint::SharedString::new());
    w.set_peek_text(slint::SharedString::new());
    w.set_peek_line_said(slint::SharedString::new());
    w.set_peek_note(slint::SharedString::new());
    w.set_peek_cannot(false);
    w.set_peek_many(false);
    w.set_peek_dir(false);
    w.set_peek_dir_said(slint::SharedString::new());
    w.set_peek_heavy(ModelRc::default());
    w.set_peek_newest(ModelRc::default());
    w.set_peek_sum_kind(slint::SharedString::new());
    w.set_peek_sum_rest(slint::SharedString::new());
    w.set_peek_facts(ModelRc::new(VecModel::from(Vec::<Fact>::new())));
    w.set_peek_found(ModelRc::default());
}

/// What several rows are, for telling one selection from the next.
fn many_key(held: &selection::Selection) -> String {
    let first = held.rows.keys().next().map_or("", String::as_str);
    let last = held.rows.keys().next_back().map_or("", String::as_str);
    format!("\u{0}many {} {first} {last}", held.rows.len())
}

/// The kind's word for a kind's token, as the rail says it.
fn kind_word(cat: &Catalogue, token: &str) -> String {
    scour_core::Kind::from_name(token)
        .and_then(|k| k.first())
        .map_or_else(|| token.to_owned(), |k| t(cat, k.msgid()).to_string())
}

/// Several rows: how many, across how many folders, each kind with how many of
/// it, and what the files among them weigh. The buttons are the menu's lines
/// for several.
fn draw_many(w: &MainWindow, cat: &Catalogue, held: &selection::Selection) {
    blank(w);
    let n = held.rows.len();
    let said = t(cat, says::MANY).replace("{n}", &grouped(n as u64));
    let folders: std::collections::HashSet<&str> = held
        .rows
        .values()
        .map(|p| scour_ui::path::folder(&p.path))
        .collect();
    let where_ = if folders.len() == 1 {
        t(cat, says::MANY_FOLDER).to_string()
    } else {
        t(cat, says::MANY_FOLDERS).replace("{n}", &grouped(folders.len() as u64))
    };
    let words: Vec<String> = held
        .rows
        .values()
        .map(|p| kind_word(cat, &p.ktoken))
        .collect();
    let mut parts: Vec<String> = says::tally(words.iter().map(String::as_str))
        .into_iter()
        .map(|(k, c)| format!("{k} {}", grouped(c as u64)))
        .collect();
    let files: Vec<&rows::Pick> = held.rows.values().filter(|p| !p.is_dir).collect();
    if !files.is_empty() {
        let bytes: u64 = files.iter().map(|p| p.bytes.max(0) as u64).sum();
        parts.push(
            t(cat, says::MANY_BYTES).replace("{size}", &scour_ui::format::size(bytes, marks().1)),
        );
    }
    w.set_peek_name(said.as_str().into());
    w.set_peek_path(slint::SharedString::new());
    w.set_peek_crumbs(ModelRc::new(VecModel::from(vec![PeekCrumb {
        label: where_.into(),
        path: slint::SharedString::new(),
        apart: false,
    }])));
    w.set_peek_acts(acts(cat, false, true, n));
    w.set_peek_many(true);
    w.set_peek_many_said(said.into());
    w.set_peek_many_small(parts.join(" · ").into());
}

/// Three of the menu's own lines for what is selected, `{n}` said.
fn acts(cat: &Catalogue, is_dir: bool, many: bool, n: usize) -> ModelRc<PeekAct> {
    let acts: Vec<PeekAct> = says::buttons(is_dir, many)
        .iter()
        .filter_map(|id| {
            let it = says::button(id, is_dir, many)?;
            Some(PeekAct {
                id: (*id).into(),
                label: t(cat, it.msgid).replace("{n}", &grouped(n as u64)).into(),
                enter: it.key == "Enter",
                icon: it.icon.into(),
            })
        })
        .collect();
    ModelRc::new(VecModel::from(acts))
}

/// The folder as parts, `~` for home, each a press that searches it; on one
/// line, the head folded into `…` until it fits the panel.
pub(super) fn crumbs(w: &MainWindow, dir: &str) -> ModelRc<PeekCrumb> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut parts: Vec<(String, String)> = Vec::new();
    let (rest, mut at) =
        if !home.is_empty() && (dir == home || dir.starts_with(&format!("{home}/"))) {
            parts.push(("~".into(), home.clone()));
            (&dir[home.len()..], home.clone())
        } else {
            parts.push(("/".into(), "/".into()));
            (dir, String::new())
        };
    for p in rest.split('/').filter(|p| !p.is_empty()) {
        at.push('/');
        at.push_str(p);
        parts.push((p.to_owned(), at.clone()));
    }
    // A slash before each part, but not straight after the root's own.
    let apart = |parts: &[(String, String)], i: usize| i > 0 && !(i == 1 && parts[0].0 == "/");
    let adv = w.get_peek_adv().max(1.0);
    let room = w.get_peek_width() - 1.0 - 14.0 - 8.0;
    let wide = |parts: &[(String, String)]| -> f32 {
        parts
            .iter()
            .enumerate()
            .map(|(i, (label, _))| {
                label.chars().count() as f32 * adv + 4.0 + if apart(parts, i) { adv } else { 0.0 }
            })
            .sum()
    };
    if parts.len() > 2 && wide(&parts) > room {
        parts.insert(1, ("…".into(), String::new()));
        while parts.len() > 3 && wide(&parts) > room {
            parts.remove(2);
        }
    }
    let model: Vec<PeekCrumb> = (0..parts.len())
        .map(|i| PeekCrumb {
            label: parts[i].0.as_str().into(),
            path: parts[i].1.as_str().into(),
            apart: apart(&parts, i),
        })
        .collect();
    ModelRc::new(VecModel::from(model))
}

/// How long the selection has to rest on a folder before its preview asks.
const DIR_WAIT_MS: u64 = 150;

thread_local! {
    /// Moved by every folder the preview is put on; an older one's asks drop.
    static DIR_GEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// What a folder's preview asks: its folders by weight, the files directly in
/// it by size, its newest files anywhere under it. From the index; no file is read.
fn ask_dir(link: &Link, path: &str, generation: u64) {
    let scoped = |field: &str| format!("{field}:\"{path}\" file:");
    let page = |limit: usize| scour_core::Page {
        offset: 0,
        limit: limit as _,
        // Nothing here reads the total.
        count_cap: 1,
    };
    link.send(Ask::PeekDir {
        generation,
        path: path.to_owned(),
        slot: 0,
        request: scour_proto::Request::Usage {
            path: path.to_owned(),
            top: says::HEAVIEST as u32,
            query: String::new(),
        },
    });
    link.send(Ask::PeekDir {
        generation,
        path: path.to_owned(),
        slot: 1,
        request: scour_proto::Request::Search {
            query: scoped("parent"),
            sort: scour_core::SortKey::Size,
            descending: true,
            page: page(says::HEAVIEST),
        },
    });
    link.send(Ask::PeekDir {
        generation,
        path: path.to_owned(),
        slot: 2,
        request: scour_proto::Request::Search {
            query: scoped("under"),
            sort: scour_core::SortKey::named("modified").unwrap_or_default(),
            descending: true,
            page: page(says::NEWEST),
        },
    });
}

/// One of a folder's three answers, kept; the folder is drawn once all three
/// are in.
pub(super) fn dir_answer(
    w: &MainWindow,
    cat: &Catalogue,
    parts: &mut DirParts,
    slot: u32,
    reply: scour_proto::Response,
) {
    match (slot, reply) {
        (0, scour_proto::Response::Usage(u)) => parts.usage = Some(u),
        (1, scour_proto::Response::Search(r)) => parts.files = Some(r.hits),
        (2, scour_proto::Response::Search(r)) => parts.newest = Some(r.hits),
        _ => return,
    }
    let (Some(u), Some(files), Some(newest)) = (&parts.usage, &parts.files, &parts.newest) else {
        return;
    };
    let decimal = marks().1;
    let mut heavy: Vec<(String, u64, String)> = u
        .children
        .iter()
        .map(|c| {
            (
                scour_ui::path::leaf(&c.path).to_owned(),
                c.bytes,
                c.path.clone(),
            )
        })
        .collect();
    heavy.extend(files.iter().filter(|h| !h.is_dir).map(|h| {
        (
            h.name().to_owned(),
            h.meta.size.max(0) as u64,
            String::new(),
        )
    }));
    heavy.sort_by_key(|h| std::cmp::Reverse(h.1));
    heavy.truncate(says::HEAVIEST);
    let shares = scour_ui::resources::bars(&heavy.iter().map(|h| h.1).collect::<Vec<_>>());
    let heavy: Vec<PeekHeavy> = heavy
        .into_iter()
        .zip(shares)
        .map(|((name, bytes, path), share)| PeekHeavy {
            name: name.into(),
            size: scour_ui::format::size(bytes, decimal).into(),
            share,
            path: path.into(),
        })
        .collect();
    let newest: Vec<PeekNew> = newest
        .iter()
        .filter(|h| !h.is_dir)
        .take(says::NEWEST)
        .map(|h| PeekNew {
            name: h.name().into(),
            when: scour_ui::format::stamp(h.meta.mtime).into(),
        })
        .collect();
    let said = format!(
        "{} · {}",
        t(cat, says::HEAVIEST_SAID),
        t(cat, says::FOLDER_SAID)
            .replace("{n}", &grouped(u.root.files))
            .replace("{size}", &scour_ui::format::size(u.root.bytes, decimal))
    );
    w.set_peek_heavy(ModelRc::new(VecModel::from(heavy)));
    w.set_peek_newest(ModelRc::new(VecModel::from(newest)));
    w.set_peek_dir_said(said.into());
}

/// Pictures for what is on screen: draw the ones the desktop has made, ask for
/// the ones it could make. **After the drawing, on the tick, never on the path a
/// keystroke takes** — see [`rows::Rows::look_for_pictures`] for every bound.
pub(super) fn pictures(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    rows: &Rc<rows::Rows>,
    lines: &Rc<rows::Lines>,
) {
    // Only where a picture is drawn: the detail list shows the kind's glyph.
    if !w.get_grid() {
        return;
    }
    /// How many rows are looked at per tick. Ten ticks a second.
    const LOOKED_AT: usize = 24;
    /// How far past the bottom of the window to look.
    const AHEAD: usize = 12;
    /// How many paths the "already asked" memory holds: a few screenfuls either way.
    const REMEMBERED: usize = 4096;
    let visible = w.get_visible_rows().max(0) as usize;
    let first = w.get_first_row().max(0) as usize;
    let (decode, ask) = rows.look_for_pictures(first, first + visible + AHEAD, LOOKED_AT);
    if !decode.is_empty() {
        let evicted = tiles::submit(decode);
        rows.made_pictures(&evicted);
    }
    let _ = lines;
    if ask.is_empty() {
        return;
    }
    let mut s = state.borrow_mut();
    if s.asking_pictures {
        return;
    }
    let already: std::collections::HashSet<&str> =
        s.asked_pictures.iter().map(String::as_str).collect();
    let ask: Vec<String> = ask
        .iter()
        .filter(|p| !already.contains(p.as_str()))
        .cloned()
        .collect();
    if ask.is_empty() {
        return;
    }
    for path in &ask {
        if s.asked_pictures.len() >= REMEMBERED {
            s.asked_pictures.pop_front();
        }
        s.asked_pictures.push_back(path.clone());
    }
    s.asking_pictures = true;
    drop(s);
    link.send(Ask::Thumbnails {
        files: ask,
        size: None,
    });
}

/// As much of a file as a preview should show — and as much as can be drawn: the
/// software renderer casts every glyph position to `i16`, and a quarter megabyte
/// in a 300 px column is ninety thousand pixels tall. Divided by the scale.
pub(super) fn peek_head(text: &str, scale: f32) -> String {
    let scale = (scale.max(1.0) as usize).max(1);
    let chars = 20_000 / scale;
    let lines = 400 / scale;
    let mut end = text.len();
    let mut seen = 0;
    for (taken, (at, c)) in text.char_indices().enumerate() {
        if taken >= chars || seen >= lines {
            end = at;
            break;
        }
        if c == '\n' {
            seen += 1;
        }
    }
    if end == text.len() {
        return text.to_owned();
    }
    let mut cut = text[..end].to_owned();
    cut.push_str("\n…");
    cut
}

/// Draw what the service says can be shown of a file. Text is set at once;
/// every visual preview is decoded or fetched off the UI thread via [`peek`].
pub(super) fn show_peek(
    w: &MainWindow,
    cat: &Catalogue,
    link: &Rc<Link>,
    path: &str,
    look: &scour_preview::Look,
) -> bool {
    peek::clear(w);
    let shown = peek_head(&look.head, w.window().scale_factor());
    w.set_peek_text(shown.as_str().into());
    w.set_peek_line_said(slint::SharedString::new());
    w.set_peek_shot(rows::blank());
    w.set_peek_has_shot(false);

    // An Office document: its text now, drawn when it comes — unless its text
    // is what was chosen.
    w.set_peek_can_flow(look.flow);
    if look.flow && !w.get_peek_office_text() {
        link.send(Ask::Flow {
            path: path.to_owned(),
        });
    }
    if look.shape == "text" && !look.head.is_empty() {
        w.set_peek_note(slint::SharedString::new());
        rail(w, look, &shown);
        return false;
    }

    doc_marks(w);
    let px = ((480.0 * w.window().scale_factor()) as u32).clamp(256, 1024);
    let direct_image = look.shape == "image" && has_direct_image_ext(path);
    let paged = scour_preview::doc::is_paged(&scour_core::path::to_path(path));
    let cached = scour_thumbs::cache::existing(path).is_some();
    let coming = !scour_thumbs::native::available()
        && !direct_image
        && !paged
        && !cached
        && scour_thumbs::can_make(path);
    if coming {
        link.send(Ask::Thumbnails {
            files: vec![path.to_owned()],
            size: Some(px),
        });
    }

    let can_draw = direct_image || paged || scour_thumbs::native::available() || cached || coming;
    if !can_draw {
        if look.head.is_empty() {
            w.set_peek_note(t(cat, says::CANNOT));
            w.set_peek_cannot(true);
            w.set_peek_native(scour_preview::quicklook(None).is_some());
        }
        return false;
    }

    // While it is read, the kind's picture stands and says enough.
    w.set_peek_note(slint::SharedString::new());
    peek::request(
        w,
        peek::Request {
            path,
            is_image: look.shape == "image",
            px,
            waiting_on_service: coming,
            reading_note: String::new(),
            nothing_note: t(cat, "It could not be previewed.").to_string(),
        },
    );
    coming
}

pub(super) fn has_direct_image_ext(path: &str) -> bool {
    let ext = path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "jfif")
}

/// The facts of one row: the summary line — kind in bold, then size and when it
/// changed — and what "Details" opens on, in [`says::DETAILS`]' order.
pub(super) fn peek_facts(w: &MainWindow, cat: &Catalogue, entry: &scour_core::Entry) {
    let m = &entry.meta;
    let items = if entry.is_dir && m.items >= 0 {
        t(cat, "{n} items").replace("{n}", &grouped(m.items as u64))
    } else {
        String::new()
    };
    let facts = scour_ui::preview::Facts {
        folder: scour_ui::path::folder(&entry.path),
        kind: &t(cat, entry.kind().msgid()),
        is_dir: entry.is_dir,
        size: m.size.max(0) as u64,
        items: &items,
        mtime: m.mtime,
        ctime: m.ctime,
        atime: m.atime,
        mode: &scour_core::mode_string(m.mode),
        owner: &[
            scour_core::owner_name(scour_core::Owner::User, m.uid),
            scour_core::owner_name(scour_core::Owner::Group, m.gid),
        ]
        .join(" · "),
    };
    let decimal = marks().1;
    let when = facts.value("modified", decimal);
    let rest: Vec<String> = [
        facts.value("size", decimal),
        if when.is_empty() {
            String::new()
        } else {
            t(cat, says::CHANGED).replace("{when}", &when)
        },
    ]
    .into_iter()
    .filter(|v| !v.is_empty())
    .collect();
    let rows: Vec<Fact> = facts
        .details(decimal)
        .into_iter()
        .map(|(msgid, value)| Fact {
            label: t(cat, msgid),
            value: value.into(),
        })
        .collect();
    w.set_peek_sum_kind(facts.value("kind", decimal).into());
    w.set_peek_sum_rest(rest.join(" · ").into());
    w.set_peek_facts(ModelRc::new(VecModel::from(rows)));
}
