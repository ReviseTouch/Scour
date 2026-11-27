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
    blank(w);
    if path.is_empty() {
        w.set_peek_name(slint::SharedString::new());
        w.set_peek_path(slint::SharedString::new());
        w.set_peek_crumbs(ModelRc::default());
        w.set_peek_acts(ModelRc::default());
        return;
    }
    w.set_peek_name(scour_ui::path::leaf(&path).into());
    w.set_peek_path(path.as_str().into());
    w.set_peek_crumbs(crumbs(w, scour_ui::path::folder(&path)));
    w.set_peek_acts(acts(cat, is_dir, false, 1));
    w.set_peek_ktoken(
        rows.ktoken_at(selected.max(0) as usize)
            .unwrap_or_default()
            .into(),
    );
    w.set_peek_dir(is_dir);
    w.set_peek_found(rows.found_at(selected.max(0) as usize));
    if is_dir {
        ask_dir(link, &path);
    } else {
        link.send(Ask::Peek { path: path.clone() });
    }
    link.send(Ask::PeekFacts { path });
}

/// Everything a row's preview said, taken away: what stands until the new
/// row's answers come is its name, its kind's picture, and nothing of the old.
fn blank(w: &MainWindow) {
    peek::clear(w);
    w.set_peek_shot(rows::blank());
    w.set_peek_has_shot(false);
    w.set_peek_shot_said(slint::SharedString::new());
    w.set_peek_text(slint::SharedString::new());
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

/// What a folder's preview asks: its folders by weight, the files directly in
/// it by size, its newest files anywhere under it. From the index; no file is read.
fn ask_dir(link: &Link, path: &str) {
    let scoped = |field: &str| format!("{field}:\"{path}\" file:");
    let page = |limit: usize| scour_core::Page {
        offset: 0,
        limit: limit as _,
        // Nothing here reads the total.
        count_cap: 1,
    };
    link.send(Ask::PeekDir {
        path: path.to_owned(),
        slot: 0,
        request: scour_proto::Request::Usage {
            path: path.to_owned(),
            top: says::HEAVIEST as u32,
            query: String::new(),
        },
    });
    link.send(Ask::PeekDir {
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
    w.set_peek_text(peek_head(&look.head, w.window().scale_factor()).into());
    w.set_peek_shot(rows::blank());
    w.set_peek_has_shot(false);

    if look.shape == "text" && !look.head.is_empty() {
        w.set_peek_note(slint::SharedString::new());
        return false;
    }

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
