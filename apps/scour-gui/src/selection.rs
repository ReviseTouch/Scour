//! The rows picked, where they are on screen, and the line that says so.

use super::*;
use scour_i18n::Catalogue;
use slint::ComponentHandle;
use std::rc::Rc;

/// How many of a group's paths are listed before the rest are counted.
pub(super) const SHOWN_PATHS: usize = 6;

#[derive(Default)]
pub(super) struct Selection {
    pub(super) rows: std::collections::BTreeMap<String, rows::Pick>,
    pub(super) at: std::collections::HashMap<String, usize>,
    pub(super) focused: Option<String>,
    pub(super) anchor_path: Option<String>,
    pub(super) anchor_row: i32,
    pub(super) pinned: bool,
    pub(super) chasing: u8,
}

impl Selection {
    pub(super) fn clear(&mut self) {
        self.rows.clear();
        self.at.clear();
        self.focused = None;
        self.anchor_path = None;
        self.anchor_row = 0;
        self.pinned = false;
        self.chasing = 0;
    }

    pub(super) fn anchor_in(&self, rows: &rows::Rows) -> i32 {
        self.anchor_path
            .as_deref()
            .and_then(|p| rows.find_path(p))
            .map(|r| r as i32)
            .unwrap_or(self.anchor_row)
    }

    pub(super) fn refresh(&mut self, rows: &rows::Rows) {
        for (path, pick) in &mut self.rows {
            if let Some(at) = rows.find_path(path) {
                self.at.insert(path.clone(), at);
                if let Some(fresh) = rows.pick_at(at) {
                    *pick = fresh;
                }
            }
        }
    }
}

pub(super) fn row_geometry(w: &MainWindow, row: usize, bar: bool) -> (f32, f32, f32, f32) {
    let (line, line_h) = if w.get_grid() {
        let per = w.get_per_line().max(1) as usize;
        (row / per, w.get_tile_h())
    } else {
        (row, w.global::<Theme>().get_row_height())
    };
    let down = w.get_view_down().max(0.0);
    let clearance = if bar { 32.0 } else { 0.0 };
    let vis = (w.get_list_shown() - clearance).max(line_h);
    (line as f32 * line_h, line_h, down, vis)
}

pub(super) fn row_in_view(w: &MainWindow, row: usize, bar: bool) -> bool {
    let (top, h, down, vis) = row_geometry(w, row, bar);
    top + h > down && top < down + vis
}

pub(super) fn scroll_to_keep_row(w: &MainWindow, row: usize, bar: bool) -> Option<f32> {
    let (top, h, down, vis) = row_geometry(w, row, bar);
    if top + h > down + vis {
        Some(top + h - vis)
    } else if top < down {
        Some(top)
    } else {
        None
    }
}

/// What the selection bar says. Folders are counted, never weighed: a total that
/// mixed the size column's `~` with exact bytes is one number from two claims.
fn picked_line(cat: &Catalogue, picks: &Selection) -> (String, String, String) {
    let dirs = picks.rows.values().filter(|p| p.is_dir).count();
    let files = picks.rows.len() - dirs;
    let bytes: u64 = picks
        .rows
        .values()
        .filter(|p| !p.is_dir)
        .map(|p| p.bytes.max(0) as u64)
        .sum();
    let mut after = Vec::new();
    if files > 0 {
        after.push(compact_bytes(bytes));
    }
    if dirs > 0 {
        after.push(t(cat, "{n} folders").replace("{n}", &grouped(dirs as u64)));
    }
    // One row: its path, which says more than "1 selected".
    if let [one] = picks.rows.values().collect::<Vec<_>>().as_slice() {
        let tail = after.join("  ·  ");
        let tail = if tail.is_empty() {
            String::new()
        } else {
            format!("  ·  {tail}")
        };
        return (one.path.clone(), String::new(), tail);
    }
    // Split where the number goes: a language need not put it first.
    let sentence = t(cat, "{n} selected");
    let (pre, post) = sentence
        .split_once("{n}")
        .unwrap_or(("", sentence.as_str()));
    let mut post = post.to_string();
    for part in after {
        post.push_str("  ·  ");
        post.push_str(&part);
    }
    (pre.to_string(), grouped(picks.rows.len() as u64), post)
}

/// The folders a selection sits in, each once: eleven files, one window.
pub(super) fn folders_of(picks: &Selection) -> Vec<String> {
    let mut seen: Vec<String> = picks
        .rows
        .values()
        .map(|p| p.folder().to_string())
        .collect();
    seen.sort();
    seen.dedup();
    seen
}

/// Put the selection on screen: the sentence, the buttons, and the rows.
pub(super) fn show_picks(
    w: &MainWindow,
    cat: &Catalogue,
    rows: &Rc<rows::Rows>,
    picks: &Selection,
    asking: bool,
) {
    w.set_picked(picks.rows.len() as i32);
    let (pre, n, post) = picked_line(cat, picks);
    w.set_picked_pre(pre.into());
    w.set_picked_n(n.into());
    w.set_picked_post(post.into());
    w.set_pick_copy(t(cat, "Copy the paths"));
    w.set_read_on(t(cat, scour_ui::READ_ON));
    w.set_read_again(t(cat, scour_ui::READ_AGAIN));
    w.set_pick_drop(t(cat, "Clear the selection"));
    w.set_pick_asking(asking);
    let folders = folders_of(picks).len();
    w.set_pick_folders_ok(folders <= scour_ui::OPEN_FOLDERS_MOST);
    w.set_pick_folders(
        if folders > scour_ui::OPEN_FOLDERS_MOST {
            t(cat, "{n} folders: too many to open").replace("{n}", &grouped(folders as u64))
        } else if asking {
            t(cat, "Open {n} windows?").replace("{n}", &grouped(folders as u64))
        } else {
            t(cat, "Open their folders ({n})").replace("{n}", &grouped(folders as u64))
        }
        .into(),
    );
    let marked: std::collections::HashSet<String> = picks.rows.keys().cloned().collect();
    rows.mark_picked(&marked);
}

/// The path of the row the list calls `i`, if that row is in hand.
pub(super) fn path_of(rows: &Rc<rows::Rows>, i: i32) -> Option<String> {
    rows.path_at(usize::try_from(i).ok()?)
}
