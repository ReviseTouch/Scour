//! The preview column: what the service says a file is, drawn.

use super::*;
use link::{Ask, Link};
use scour_i18n::Catalogue;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

/// Keep the preview panel on whatever is selected. **Asked once per row, not per
/// tick**: what it is showing is remembered as a path.
pub(super) fn peek(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    rows: &Rc<rows::Rows>,
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
    state.borrow_mut().peek_path = path.clone();
    peek::clear(w);
    w.set_peek_shot(rows::blank());
    w.set_peek_has_shot(false);
    w.set_peek_text(slint::SharedString::new());
    if path.is_empty() {
        w.set_peek_name(slint::SharedString::new());
        w.set_peek_path(slint::SharedString::new());
        w.set_peek_note(slint::SharedString::new());
        w.set_peek_facts(ModelRc::new(VecModel::from(Vec::<Fact>::new())));
        w.set_peek_found(ModelRc::default());
        return;
    }
    w.set_peek_name(scour_ui::path::leaf(&path).into());
    w.set_peek_path(path.as_str().into());
    w.set_peek_found(rows.found_at(selected.max(0) as usize));
    if is_dir {
        w.set_peek_note(slint::SharedString::new());
    } else {
        w.set_peek_note(t(cat, "reading…"));
        link.send(Ask::Peek { path: path.clone() });
    }
    link.send(Ask::PeekFacts { path });
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
        w.set_peek_note(if look.head.is_empty() {
            t(cat, "It could not be previewed.")
        } else {
            slint::SharedString::new()
        });
        return false;
    }

    w.set_peek_note(if look.head.is_empty() {
        t(cat, "reading…")
    } else {
        slint::SharedString::new()
    });
    peek::request(
        w,
        peek::Request {
            path,
            is_image: look.shape == "image",
            px,
            waiting_on_service: coming,
            reading_note: t(cat, "reading…").to_string(),
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

/// The fact list, in the order [`scour_ui::preview::FACTS`] gives.
pub(super) fn peek_facts(cat: &Catalogue, entry: &scour_core::Entry) -> ModelRc<Fact> {
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
    let rows: Vec<Fact> = facts
        .lines(marks().1)
        .into_iter()
        .map(|(msgid, value)| Fact {
            label: t(cat, msgid),
            value: value.into(),
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}
