//! On-demand inline Grep capsule and stream view for the Slint window.

use std::cell::RefCell;
use std::rc::Rc;

use scour_core::{Catalog, EntryId, Hit, SortKey, SourceId};
use scour_i18n::Catalogue;
use scour_proto::{GrepFile, GrepMode, GrepResponse, Request, Response};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::link::{Ask, Link};
use crate::ui::GrepItem;
use crate::{Found, MainWindow, State};

#[derive(Default)]
pub struct GrepUi {
    pub open: bool,
    pub pattern: String,
    pub mode: GrepMode,
    pub exact_case: bool,
    pub binary: bool,
    pub generation: u64,
    pub files: Vec<GrepFile>,
    pub next: Option<u32>,
    /// How many files the query lists, and whether that is only as far as
    /// the count went: what a pattern would be read in.
    pub listed: u64,
    pub listed_more: bool,
    /// The file of the stream the preview is on.
    pub selected: i32,
    /// Asked, and not answered yet.
    pub reading: bool,
}

/// The query listed this many files: the line says how many it would read.
pub fn listed(w: &MainWindow, s: &mut State, cat: &Catalogue, total: u64, more: bool) {
    s.grep.listed = total;
    s.grep.listed_more = more;
    meter(w, s, cat);
}

/// What the line under the field says at its end: how many files a pattern
/// would be read in, that they are being read, or how many hold it.
fn meter(w: &MainWindow, s: &State, cat: &Catalogue) {
    let g = &s.grep;
    let said = if !g.open {
        String::new()
    } else if g.pattern.is_empty() {
        let more = if g.listed_more { "+" } else { "" };
        cat.get("{n} files to read")
            .replace("{n}", &format!("{}{more}", crate::grouped(g.listed)))
    } else if g.reading {
        cat.get("reading…").into_owned()
    } else {
        let more = if g.next.is_some() { "+" } else { "" };
        cat.get("in {n} files").replace(
            "{n}",
            &format!("{}{more}", crate::grouped(g.files.len() as u64)),
        )
    };
    w.set_grep_meter(said.into());
}

fn home_form(p: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    crate::qlens::home_path(&home, p)
}

fn cut_hit(text: &str, at: (u32, u32)) -> (String, String, String) {
    let (s, len) = (at.0 as usize, at.1 as usize);
    match (text.get(..s), text.get(s..s + len), text.get(s + len..)) {
        (Some(pre), Some(hit), Some(post)) => {
            let clean = |v: &str| v.replace(['\t', '\r', '\n'], " ");
            let mut p = clean(pre).trim_start().to_owned();
            if p.chars().count() > 44 {
                let tail: String = p
                    .chars()
                    .rev()
                    .take(44)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                p = format!("…{}", tail.trim_start());
            }
            (p, clean(hit), clean(post))
        }
        _ => (
            text.replace(['\t', '\r', '\n'], " "),
            String::new(),
            String::new(),
        ),
    }
}

pub fn trigger(w: &MainWindow, s: &mut State, link: &Link, cat: &Catalogue, append: bool) {
    if !s.grep.open || s.grep.pattern.is_empty() {
        s.grep.generation += 1;
        s.grep.files.clear();
        s.grep.next = None;
        s.grep.reading = false;
        w.set_grep_active(false);
        meter(w, s, cat);
        return;
    }
    s.grep.reading = true;
    meter(w, s, cat);
    s.grep.generation += 1;
    let offset = if append { s.grep.next.unwrap_or(0) } else { 0 };
    if !append {
        s.grep.files.clear();
        s.grep.next = None;
        w.set_grep_summary(cat.get("Scanning matching files…").as_ref().into());
        w.set_grep_more_on(false);
        w.set_grep_items(ModelRc::new(VecModel::from(Vec::<GrepItem>::new())));
        w.set_grep_active(true);
    }
    link.send(Ask::Grep {
        generation: s.grep.generation,
        append,
        request: Request::Grep {
            query: s.query.clone(),
            pattern: s.grep.pattern.clone(),
            mode: s.grep.mode,
            exact_case: s.grep.exact_case,
            binary: s.grep.binary,
            sort: SortKey::named(&s.sort).unwrap_or_default(),
            descending: s.descending,
            offset,
            top: 50,
            hits: 5,
            read_budget: 256 << 20,
            max_size: 64 << 20,
            timeout_ms: 6_000,
            rate_mib: 0,
        },
    });
}

pub fn answered(
    w: &MainWindow,
    s: &mut State,
    link: &Link,
    cat: &Catalogue,
    generation: u64,
    append: bool,
    reply: Response,
) {
    if generation != s.grep.generation || !s.grep.open || s.grep.pattern.is_empty() {
        return;
    }
    let Response::Grep(GrepResponse {
        files,
        scanned,
        read,
        binary,
        too_big,
        unreadable,
        header_only,
        no_reader,
        locked,
        offline,
        no_text,
        broken,
        next,
        took_us,
        ..
    }) = reply
    else {
        return;
    };
    if append {
        s.grep.files.extend(files);
    } else {
        s.grep.files = files;
    }
    s.grep.next = next;
    s.grep.reading = false;
    meter(w, s, cat);

    let mut summary = cat
        .get("{f} files · {s} scanned · {b} read")
        .replace("{f}", &crate::grouped(s.grep.files.len() as u64))
        .replace("{s}", &crate::grouped(scanned))
        .replace(
            "{b}",
            &scour_ui::format::compact_bytes(read, crate::marks().1),
        );
    summary.push_str(&format!(" · {:.1} ms", took_us as f64 / 1000.0));
    for (n, msgid) in [
        (binary, "{n} binary skipped"),
        (too_big, "{n} too large"),
        (header_only, "{n} media skipped"),
        (no_reader, "{n} documents not read"),
        (locked, "{n} locked"),
        (offline, "{n} in the cloud"),
        (no_text, "{n} without text"),
        (broken, "{n} would not open"),
    ] {
        if n > 0 {
            summary.push_str("  ·  ");
            summary.push_str(&cat.get(msgid).replace("{n}", &crate::grouped(n)));
        }
    }
    if unreadable > 0 {
        summary.push_str(&format!(
            "  ·  {} {}",
            crate::grouped(unreadable),
            cat.get("unreadable")
        ));
    }
    w.set_grep_summary(summary.into());
    w.set_grep_more_on(next.is_some());

    let heads = w.get_heads();
    let cols: Vec<&'static scour_ui::Column> = (0..heads.row_count())
        .filter_map(|i| scour_ui::column(heads.row_data(i)?.id.as_str()))
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let frozen_note = cat
        .get("this volume does not record reads (mounted noatime)")
        .into_owned();

    let mut items = Vec::new();
    for (fi, gf) in s.grep.files.iter().enumerate() {
        let name = scour_ui::path::leaf(&gf.path).to_owned();
        let dir = home_form(scour_ui::path::folder(&gf.path));
        let count = format!("{}{}", gf.hits.len(), if gf.more { "+" } else { "" });
        let hit = Hit {
            id: EntryId::path_hash(SourceId(0), &gf.path),
            path: gf.path.clone(),
            is_dir: false,
            kind: gf.kind,
            meta: gf.meta,
            under: None,
            found: Vec::new(),
        };
        let kind_word = cat.get(gf.kind.msgid());
        let row = crate::rows::row_of(
            &hit,
            &[],
            &crate::rows::Shape {
                columns: &cols,
                mounts: &s.mounts,
                kind: &kind_word,
                frozen_note: &frozen_note,
                now,
                system_icons: false,
            },
            false,
        );
        items.push(GrepItem {
            header: true,
            file_idx: fi as i32,
            cells: row.cells,
            ktoken: row.ktoken,
            tint: row.tint,
            ext: row.ext,
            name: name.into(),
            dir: dir.into(),
            count: count.into(),
            line: slint::SharedString::new(),
            pre: slint::SharedString::new(),
            hit: slint::SharedString::new(),
            post: slint::SharedString::new(),
        });
        for h in &gf.hits {
            let (pre, hit, post) = cut_hit(&h.text, h.at);
            let line = h
                .line
                .map_or_else(|| format!("@{}", h.offset), |l| l.to_string());
            items.push(GrepItem {
                header: false,
                file_idx: fi as i32,
                cells: ModelRc::default(),
                ktoken: slint::SharedString::new(),
                tint: slint::Brush::default(),
                ext: slint::SharedString::new(),
                name: slint::SharedString::new(),
                dir: slint::SharedString::new(),
                count: slint::SharedString::new(),
                line: line.into(),
                pre: pre.into(),
                hit: hit.into(),
                post: post.into(),
            });
        }
    }
    w.set_grep_empty(if s.grep.files.is_empty() {
        cat.get("No files held that pattern in the scanned batch.")
            .as_ref()
            .into()
    } else {
        slint::SharedString::new()
    });
    w.set_grep_items(ModelRc::new(VecModel::from(items)));
    w.set_grep_active(true);
    // A new stream opens on its first file, so the preview shows what was
    // found at once; more of the same stream leaves where the reader is.
    if !append {
        s.grep.selected = -1;
        w.set_grep_sel(-1);
        if w.get_peeking() && !s.grep.files.is_empty() {
            select(w, s, link, cat, 0);
        }
    }
}

/// A file of the stream is the one looked at: the preview opens on it, with
/// the lines that held the pattern, marked.
pub fn select(w: &MainWindow, s: &mut State, link: &Link, cat: &Catalogue, fi: i32) {
    let Some(gf) = s.grep.files.get(fi.max(0) as usize).cloned() else {
        return;
    };
    s.grep.selected = fi;
    w.set_grep_sel(fi);
    s.peek_path = gf.path.clone();
    w.set_peeking(true);
    w.set_peek_name(scour_ui::path::leaf(&gf.path).into());
    w.set_peek_path(gf.path.as_str().into());
    let found: Vec<Found> = gf
        .hits
        .iter()
        .map(|h| {
            let (pre, hit, post) = cut_hit(&h.text, h.at);
            Found {
                line: h
                    .line
                    .map_or_else(|| format!("@{}", h.offset), |l| l.to_string())
                    .into(),
                pre: pre.into(),
                hit: hit.into(),
                post: post.into(),
            }
        })
        .collect();
    w.set_peek_found(ModelRc::new(VecModel::from(found)));
    w.set_peek_note(cat.get("reading…").as_ref().into());
    link.send(Ask::Peek {
        path: gf.path.clone(),
    });
    link.send(Ask::PeekFacts { path: gf.path });
}

pub fn wire(
    window: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    cat: &Rc<RefCell<Rc<Catalogue>>>,
) {
    {
        let (weak, state, link, cat) = (
            window.as_weak(),
            Rc::clone(state),
            Rc::clone(link),
            Rc::clone(cat),
        );
        window.on_grep_toggled(move || {
            let Some(w) = weak.upgrade() else { return };
            let open = {
                let mut s = state.borrow_mut();
                s.grep.open = !s.grep.open;
                s.grep.open
            };
            w.set_grep_open(open);
            if open {
                w.set_grep_focus_ask(w.get_grep_focus_ask() + 1);
            } else {
                w.invoke_focus_query();
            }
            trigger(&w, &mut state.borrow_mut(), &link, &cat.borrow(), false);
        });
    }
    {
        let (weak, state, link, cat) = (
            window.as_weak(),
            Rc::clone(state),
            Rc::clone(link),
            Rc::clone(cat),
        );
        window.on_grep_edited(move |txt| {
            let Some(w) = weak.upgrade() else { return };
            state.borrow_mut().grep.pattern = txt.to_string();
            trigger(&w, &mut state.borrow_mut(), &link, &cat.borrow(), false);
        });
    }
    {
        let (weak, state, link, cat) = (
            window.as_weak(),
            Rc::clone(state),
            Rc::clone(link),
            Rc::clone(cat),
        );
        window.on_grep_mode_picked(move |m| {
            let Some(w) = weak.upgrade() else { return };
            {
                let mut s = state.borrow_mut();
                s.grep.mode = match m.as_str() {
                    "regex" => GrepMode::Regex,
                    "hex" => GrepMode::Hex,
                    _ => GrepMode::Text,
                };
            }
            w.set_grep_mode(m);
            trigger(&w, &mut state.borrow_mut(), &link, &cat.borrow(), false);
        });
    }
    {
        let (weak, state, link, cat) = (
            window.as_weak(),
            Rc::clone(state),
            Rc::clone(link),
            Rc::clone(cat),
        );
        window.on_grep_case_toggled(move || {
            let Some(w) = weak.upgrade() else { return };
            let exact = {
                let mut s = state.borrow_mut();
                s.grep.exact_case = !s.grep.exact_case;
                s.grep.exact_case
            };
            w.set_grep_case(exact);
            trigger(&w, &mut state.borrow_mut(), &link, &cat.borrow(), false);
        });
    }
    {
        let (weak, state, link, cat) = (
            window.as_weak(),
            Rc::clone(state),
            Rc::clone(link),
            Rc::clone(cat),
        );
        window.on_grep_bin_toggled(move || {
            let Some(w) = weak.upgrade() else { return };
            let bin = {
                let mut s = state.borrow_mut();
                s.grep.binary = !s.grep.binary;
                s.grep.binary
            };
            w.set_grep_binary(bin);
            trigger(&w, &mut state.borrow_mut(), &link, &cat.borrow(), false);
        });
    }
    {
        let (weak, state, link, cat) = (
            window.as_weak(),
            Rc::clone(state),
            Rc::clone(link),
            Rc::clone(cat),
        );
        window.on_grep_more(move || {
            let Some(w) = weak.upgrade() else { return };
            trigger(&w, &mut state.borrow_mut(), &link, &cat.borrow(), true);
        });
    }
    {
        let (weak, state, link, cat) = (
            window.as_weak(),
            Rc::clone(state),
            Rc::clone(link),
            Rc::clone(cat),
        );
        window.on_grep_clicked(move |fi| {
            let Some(w) = weak.upgrade() else { return };
            select(&w, &mut state.borrow_mut(), &link, &cat.borrow(), fi);
        });
    }
    {
        let (weak, state, link, cat) = (
            window.as_weak(),
            Rc::clone(state),
            Rc::clone(link),
            Rc::clone(cat),
        );
        // ↑ and ↓ walk the stream's files while it is the list: the preview
        // follows, as it does down the ordinary list.
        window.on_grep_step(move |by| {
            let Some(w) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let last = st.grep.files.len() as i32 - 1;
            if last < 0 {
                return;
            }
            let next = (st.grep.selected + by).clamp(0, last);
            select(&w, &mut st, &link, &cat.borrow(), next);
        });
    }
    {
        let state = Rc::clone(state);
        window.on_grep_opened(move |fi| {
            let path = state
                .borrow()
                .grep
                .files
                .get(fi.max(0) as usize)
                .map(|f| f.path.clone());
            if let Some(p) = path {
                let _ = scour_openers::open(std::ffi::OsStr::new(&p));
            }
        });
    }
}
