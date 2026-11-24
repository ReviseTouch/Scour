//! The selection.

use super::*;

pub(crate) fn wire(wiring: &Wiring) {
    let Wiring {
        window,
        cat,
        state,
        rows,
        picks,
        slots,
        link,
        ..
    } = *wiring;
    // Plain replaces it, `Ctrl` adds one, `Shift` takes the run.
    {
        let picks = Rc::clone(picks);
        let rows = Rc::clone(rows);
        window.on_navigated(move |row| {
            let mut held = picks.borrow_mut();
            held.focused = path_of(&rows, row);
            held.pinned = true;
            held.chasing = 0;
        });
    }
    {
        let picks = Rc::clone(picks);
        let rows = Rc::clone(rows);
        let cat = Rc::clone(cat);
        let weak = window.as_weak();
        window.on_pick(move |row, adding, run| {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let Some(here) = rows.pick_at(row.max(0) as usize) else {
                return;
            };
            let mut held = picks.borrow_mut();
            held.focused = Some(here.path.clone());
            held.pinned = true;
            held.chasing = 0;
            if run {
                // The run is what is in hand: a range over millions can cross
                // pages nobody has fetched.
                held.rows.clear();
                held.at.clear();
                let anc = held.anchor_in(&rows);
                let (from, to) = if anc <= row { (anc, row) } else { (row, anc) };
                for at in from.max(0)..=to.max(0) {
                    if let Some(pick) = rows.pick_at(at as usize) {
                        held.at.insert(pick.path.clone(), at as usize);
                        held.rows.insert(pick.path.clone(), pick);
                    }
                }
            } else if adding {
                held.anchor_row = row;
                held.anchor_path = Some(here.path.clone());
                if held.rows.remove(&here.path).is_some() {
                    held.at.remove(&here.path);
                } else {
                    held.at.insert(here.path.clone(), row.max(0) as usize);
                    held.rows.insert(here.path.clone(), here);
                }
            } else {
                held.anchor_row = row;
                held.anchor_path = Some(here.path.clone());
                held.rows.clear();
                held.at.clear();
                held.at.insert(here.path.clone(), row.max(0) as usize);
                held.rows.insert(here.path.clone(), here);
            }
            show_picks(&w, &cat, &rows, &held, false);
        });
    }
    {
        let picks = Rc::clone(picks);
        let rows = Rc::clone(rows);
        let slots = Rc::clone(slots);
        let state = Rc::clone(state);
        let link = Rc::clone(link);
        let cat = Rc::clone(cat);
        let weak = window.as_weak();
        window.on_pick_focused(move || {
            let Some(w) = weak.upgrade() else { return };
            let target = {
                let mut held = picks.borrow_mut();
                let keys: Vec<String> = held.rows.keys().cloned().collect();
                if keys.is_empty() {
                    return;
                }
                let chosen = if keys.len() > 1
                    && let Some(ref cur) = held.focused
                    && let Some(pos) = keys.iter().position(|k| k == cur)
                {
                    let visible = rows
                        .find_path(cur)
                        .is_some_and(|r| scroll_to_keep_row(&w, r, true).is_none());
                    if visible {
                        keys[(pos + 1) % keys.len()].clone()
                    } else {
                        cur.clone()
                    }
                } else {
                    keys[0].clone()
                };
                held.focused = Some(chosen.clone());
                held.pinned = true;
                let row = rows
                    .find_path(&chosen)
                    .or_else(|| held.at.get(&chosen).copied());
                (chosen, row)
            };
            if let Some(row) = target.1 {
                w.set_selected(row as i32);
                if scroll_to_keep_row(&w, row, true).is_some() {
                    let (top, h, _, vis) = row_geometry(&w, row, true);
                    w.invoke_scroll_to((top - (vis - h) * 0.5).max(0.0));
                }
                place(&w, &slots);
                follow(&w, &state, &link, &rows);
                if w.get_peeking() {
                    let cat_now = cat.borrow().clone();
                    peek(&w, &state, &link, &rows, &cat_now);
                }
            }
            w.invoke_focus_query();
        });
    }
    {
        let picks = Rc::clone(picks);
        let rows = Rc::clone(rows);
        let cat = Rc::clone(cat);
        let weak = window.as_weak();
        window.on_pick_dropped(move || {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            picks.borrow_mut().clear();
            w.set_selected(-1);
            show_picks(&w, &cat, &rows, &picks.borrow(), false);
        });
    }
    {
        let picks = Rc::clone(picks);
        let weak = window.as_weak();
        let cat = Rc::clone(cat);
        window.on_pick_copied(move || {
            let cat = cat.borrow().clone();
            for pick in picks.borrow().rows.values() {
                println!("{}", pick.path);
            }
            if let Some(w) = weak.upgrade() {
                told(&w, t(&cat, "path printed to the terminal"));
            }
        });
    }
    {
        let picks = Rc::clone(picks);
        let rows = Rc::clone(rows);
        let cat = Rc::clone(cat);
        let asking = Rc::new(std::cell::Cell::new(false));
        let weak = window.as_weak();
        window.on_pick_opened(move || {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let folders = folders_of(&picks.borrow());
            if folders.len() > scour_ui::OPEN_FOLDERS_MOST {
                return;
            }
            // More than a few windows is asked about first.
            if folders.len() > scour_ui::OPEN_FOLDERS_ASK && !asking.get() {
                asking.set(true);
                show_picks(&w, &cat, &rows, &picks.borrow(), true);
                return;
            }
            asking.set(false);
            for folder in folders {
                open(&folder);
            }
            show_picks(&w, &cat, &rows, &picks.borrow(), false);
        });
    }
}
