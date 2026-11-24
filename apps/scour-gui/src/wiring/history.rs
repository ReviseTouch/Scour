//! What was searched before.

use super::*;

pub(crate) fn wire(wiring: &Wiring) {
    let Wiring {
        window,
        cat,
        state,
        menu_path,
        rows,
        link,
        ..
    } = *wiring;
    {
        let state = Rc::clone(state);
        let link = Rc::clone(link);
        let weak = window.as_weak();
        window.on_query_committed(move || {
            stir(&state, &link);
            let query = full_query(&state.borrow());
            remember(&state, &link, &query);
            if let Some(w) = weak.upgrade() {
                w.set_has_past(!state.borrow().past.is_empty());
            }
        });
    }
    {
        let state = Rc::clone(state);
        let weak = window.as_weak();
        window.on_past_toggled(move || {
            let Some(w) = weak.upgrade() else { return };
            if w.get_past_open() {
                w.set_past_open(false);
                return;
            }
            w.set_lens_open(false);
            let lines = past_matching(&state.borrow().past, &state.borrow().query);
            w.set_past_picked(0);
            w.set_past(ModelRc::new(VecModel::from(lines.clone())));
            w.set_past_open(!lines.is_empty());
        });
    }
    {
        let weak = window.as_weak();
        window.on_past_taken(move |line| {
            let Some(w) = weak.upgrade() else { return };
            w.set_past_open(false);
            // **Set, then tell**: writing the two-way bound text does not fire
            // `edited`, so the second line is what searches.
            w.set_query(line.clone());
            w.invoke_query_changed(line);
            w.invoke_caret_to_end();
        });
    }
    {
        let rows = Rc::clone(rows);
        // The row selected in its folder, as the menu's "open its folder" does:
        // opening the folder alone selected nothing.
        window.on_reveal(move |i| {
            if let Some(path) = path_of(&rows, i) {
                scour_openers::reveal(&[scour_core::path::to_path(&path).as_path()]);
            }
        });
    }
    {
        let rows = Rc::clone(rows);
        let weak = window.as_weak();
        let cat = Rc::clone(cat);
        window.on_copy_path(move |i| {
            let cat = cat.borrow().clone();
            let Some(path) = path_of(&rows, i) else {
                return;
            };
            println!("{path}");
            if let Some(w) = weak.upgrade() {
                told(&w, t(&cat, "path printed to the terminal"));
            }
        });
    }

    // A menu item by its key: about the selected row, so whatever path a
    // menu set aside is dropped first.
    {
        let menu_path = Rc::clone(menu_path);
        let weak = window.as_weak();
        window.on_key_action(move |id| {
            menu_path.borrow_mut().take();
            if let Some(w) = weak.upgrade() {
                w.invoke_menu_pick(id);
            }
        });
    }
}
