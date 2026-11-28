//! The content search's list, placed here: a file's heading and each line
//! under it are of heights known here, so every item's place is exact, and
//! only what is in sight is drawn, in slots that keep their item while it
//! stays in sight. Slint's `ListView` guessed the list's height from the rows
//! on screen; with a heading taller than a line the guess, the scrollbar and
//! the place under the hand moved together.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{Model, ModelRc, VecModel};

use crate::MainWindow;
use crate::ui::{GrepItem, GrepSlot};

/// A file's heading, in logical pixels.
pub const HEAD: f32 = 32.0;
/// A line under it.
pub const LINE: f32 = 25.0;

#[derive(Default)]
struct List {
    items: Vec<GrepItem>,
    /// Where each item's top is, and one more: the whole list's height.
    tops: Vec<f32>,
    pool: Option<Rc<VecModel<GrepSlot>>>,
    /// Which item each slot shows. Item `r` lives in slot `r % len`, so the
    /// slots of the items still in sight never move.
    map: Vec<Option<usize>>,
}

thread_local! {
    static LIST: RefCell<List> = RefCell::new(List::default());
}

fn height(item: &GrepItem) -> f32 {
    if item.header { HEAD } else { LINE }
}

/// Every item's top, and the whole height last.
fn tops(items: &[GrepItem]) -> Vec<f32> {
    let mut at = 0.0;
    let mut tops = Vec::with_capacity(items.len() + 1);
    for item in items {
        tops.push(at);
        at += height(item);
    }
    tops.push(at);
    tops
}

/// The item under `down` pixels from the top; the last one past the end.
fn first_at(tops: &[f32], down: f32) -> usize {
    let items = tops.len().saturating_sub(1);
    tops[..items]
        .partition_point(|&top| top <= down)
        .saturating_sub(1)
}

/// Show `items`, the stream so far. More of the same stream leaves the reader
/// where they are; a new one opens at its top.
pub fn set(w: &MainWindow, items: Vec<GrepItem>, keep: bool) {
    let tall = LIST.with(|l| {
        let mut l = l.borrow_mut();
        l.tops = tops(&items);
        l.items = items;
        // An item at the same place may be another one now.
        l.map.iter_mut().for_each(|m| *m = None);
        l.tops.last().copied().unwrap_or(0.0)
    });
    w.set_grep_tall(tall);
    if !keep {
        w.invoke_grep_scroll_to(0.0);
    }
    place(w);
}

/// Cover what is in sight: called when the list moves or changes size.
pub fn place(w: &MainWindow) {
    let down = w.get_grep_down().max(0.0);
    let shown = w.get_grep_shown().max(0.0);
    // As many as fit if every one is a line, one cut at each edge, one to spare.
    let count = (shown / LINE).ceil() as usize + 3;
    LIST.with(|l| {
        let mut l = l.borrow_mut();
        let pool = match &l.pool {
            Some(pool) => Rc::clone(pool),
            None => {
                let pool = Rc::new(VecModel::default());
                w.set_grep_slots(ModelRc::from(Rc::clone(&pool)));
                l.pool = Some(Rc::clone(&pool));
                pool
            }
        };
        if l.map.len() != count || pool.row_count() != count {
            l.map = vec![None; count];
            pool.set_vec(vec![GrepSlot::default(); count]);
        }
        let first = first_at(&l.tops, down);
        for r in first..first + count {
            let slot = r % count;
            let want = (r < l.items.len()).then_some(r);
            if l.map[slot] == want {
                continue;
            }
            l.map[slot] = want;
            let data = match want {
                Some(r) => GrepSlot {
                    y: l.tops[r],
                    h: height(&l.items[r]),
                    shown: true,
                    item: l.items[r].clone(),
                },
                None => GrepSlot::default(),
            };
            pool.set_row_data(slot, data);
        }
    });
}

/// Bring file `fi`'s heading, and a line of it, into sight, moving no further
/// than that.
pub fn reveal(w: &MainWindow, fi: i32) {
    let Some((top, bottom)) = LIST.with(|l| {
        let l = l.borrow();
        let i = l
            .items
            .iter()
            .position(|it| it.header && it.file_idx == fi)?;
        let lines = l.items.get(i + 1).is_some_and(|it| !it.header);
        Some((l.tops[i], l.tops[i] + HEAD + if lines { LINE } else { 0.0 }))
    }) else {
        return;
    };
    let down = w.get_grep_down();
    let shown = w.get_grep_shown();
    if top < down {
        w.invoke_grep_scroll_to(top);
    } else if bottom > down + shown {
        w.invoke_grep_scroll_to(bottom - shown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(header: bool) -> GrepItem {
        GrepItem {
            header,
            ..GrepItem::default()
        }
    }

    #[test]
    fn every_item_stands_where_the_ones_above_it_end() {
        let items = [
            item(true),
            item(false),
            item(false),
            item(true),
            item(false),
        ];
        let t = tops(&items);
        assert_eq!(t, vec![0.0, 32.0, 57.0, 82.0, 114.0, 139.0]);
    }

    #[test]
    fn the_item_in_sight_is_found_by_its_place() {
        let items = [
            item(true),
            item(false),
            item(false),
            item(true),
            item(false),
        ];
        let t = tops(&items);
        assert_eq!(first_at(&t, 0.0), 0);
        assert_eq!(first_at(&t, 31.9), 0);
        assert_eq!(first_at(&t, 32.0), 1);
        assert_eq!(first_at(&t, 100.0), 3);
        assert_eq!(first_at(&t, 1e6), 4);
        assert_eq!(first_at(&tops(&[]), 50.0), 0);
    }
}
