//! The search tabs.

use super::*;

pub(crate) fn wire(wiring: &Wiring) {
    let Wiring {
        window,
        cat,
        tabs,
        state,
        link,
        ..
    } = *wiring;
    // Kept two seconds after the last keystroke, not on every one.
    {
        let keep = {
            let link = Rc::clone(link);
            let tabs = Rc::clone(tabs);
            move || keep_tabs(&link, &tabs.borrow())
        };
        let keep = Rc::new(keep);
        let go = {
            let state = Rc::clone(state);
            let tabs = Rc::clone(tabs);
            let cat = Rc::clone(cat);
            let link = Rc::clone(link);
            let weak = window.as_weak();
            let keep = Rc::clone(&keep);
            move |moved: Option<scour_settings::SearchTab>| {
                let Some(w) = weak.upgrade() else { return };
                draw_tabs(&w, &tabs.borrow(), &cat.borrow());
                keep();
                if let Some(tab) = moved {
                    show_tab(&w, &state, &link, &cat.borrow(), &tab);
                }
            }
        };
        let go = Rc::new(go);
        {
            let (state, tabs, go) = (Rc::clone(state), Rc::clone(tabs), Rc::clone(&go));
            window.on_tab_new(move || {
                let new = tabs.borrow_mut().open(now_tab(&state.borrow()));
                go(Some(new));
            });
        }
        {
            let (state, tabs, go) = (Rc::clone(state), Rc::clone(tabs), Rc::clone(&go));
            window.on_tab_picked(move |i| {
                let Ok(i) = usize::try_from(i) else { return };
                let moved = tabs.borrow_mut().switch(i, now_tab(&state.borrow()));
                if moved.is_some() {
                    go(moved);
                }
            });
        }
        {
            let (state, tabs, go) = (Rc::clone(state), Rc::clone(tabs), Rc::clone(&go));
            window.on_tab_closed(move |i| {
                let Ok(i) = usize::try_from(i) else { return };
                let before = tabs.borrow().len();
                let moved = tabs.borrow_mut().close(i, now_tab(&state.borrow()));
                if tabs.borrow().len() != before {
                    go(moved);
                }
            });
        }
        {
            let (state, tabs, go) = (Rc::clone(state), Rc::clone(tabs), Rc::clone(&go));
            window.on_tab_step(move |by| {
                let moved = tabs
                    .borrow_mut()
                    .step(by as isize, now_tab(&state.borrow()));
                if moved.is_some() {
                    go(moved);
                }
            });
        }
    }
}
