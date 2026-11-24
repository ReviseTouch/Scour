//! Opening things.

use super::*;

pub(crate) fn wire(wiring: &Wiring) {
    let Wiring {
        window,
        cat,
        state,
        rows,
        link,
        ..
    } = *wiring;
    // **The path comes off the row**, not out of a page of hits by row number.
    {
        let rows = Rc::clone(rows);
        let state = Rc::clone(state);
        let link = Rc::clone(link);
        let weak = window.as_weak();
        let cat = Rc::clone(cat);
        window.on_activated(move |i| {
            stir(&state, &link);
            let query = full_query(&state.borrow());
            remember(&state, &link, &query);
            let Some(w) = weak.upgrade() else { return };
            w.set_has_past(!state.borrow().past.is_empty());
            let Some((path, runs)) = usize::try_from(i).ok().and_then(|r| rows.opening_at(r))
            else {
                return;
            };
            // **A program is shown, never started**: a search lists whatever
            // is in Downloads and in unpacked archives, and a double-click is
            // too easy to make for that to run it. Its folder, with it selected.
            if runs {
                scour_openers::reveal(&[scour_core::path::to_path(&path).as_path()]);
                let name = path.rsplit('/').next().unwrap_or(&path);
                // The status line, not `hint`: that is the empty field's
                // placeholder, and the field holds a query here.
                told(
                    &w,
                    t(
                        &cat.borrow(),
                        "{name} is a program: shown in its folder, not run",
                    )
                    .replace("{name}", name)
                    .into(),
                );
            } else {
                open(&path);
            }
        });
    }
    // A double-click: on the location, the row in its folder; anywhere else
    // what Enter does. Which column is read off the column table, the one the
    // rows and the header are drawn from.
    {
        let rows = Rc::clone(rows);
        let weak = window.as_weak();
        window.on_opened_at(move |i, x| {
            let Some(w) = weak.upgrade() else { return };
            let (heads, cx, cw) = (w.get_heads(), w.get_cx(), w.get_cw());
            let column = (0..heads.row_count()).find(|&c| {
                let (Some(at), Some(wide)) = (cx.row_data(c), cw.row_data(c)) else {
                    return false;
                };
                x >= at && x < at + wide
            });
            let on_location = column
                .and_then(|c| heads.row_data(c))
                .is_some_and(|h| h.id == "path");
            if on_location {
                if let Some(path) = path_of(&rows, i) {
                    scour_openers::reveal(&[scour_core::path::to_path(&path).as_path()]);
                }
            } else {
                w.invoke_activated(i);
            }
        });
    }
}
