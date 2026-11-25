//! The caret over a place capsule: whether a press edits a folder part or
//! only rests on a disk, and the arrow keys' one stop a part, as drawn.

use std::cell::RefCell;
use std::rc::Rc;

use scour_ui::lens;

use crate::qwire::write;
use crate::{MainWindow, State};

/// A press on a place capsule: into a folder part it edits that folder; on
/// a disk, or anywhere else, it only rests there.
pub fn press_part(state: &Rc<RefCell<State>>, text: &str) {
    let mut st = state.borrow_mut();
    let c = st.qb.cursor.min(text.len());
    let folder = st
        .qb
        .caps
        .iter()
        .find(|p| p.start <= c && c <= p.end)
        .is_some_and(|p| {
            p.part_at(c)
                .map(|k| &p.parts[k])
                .is_some_and(|part| part.shown && part.spot.tile.is_none())
        });
    st.qb.lenses.editing = folder;
}

/// An arrow over a place capsule: one stop a part, as drawn; false when the
/// key leaves the capsule and moves by a letter.
pub fn arrow(w: &MainWindow, s: &Rc<RefCell<State>>, forward: bool) -> bool {
    let text = w.get_query().to_string();
    let next = {
        let st = s.borrow();
        let c = st.qb.cursor.min(text.len());
        if st.qb.anchor != c {
            return false;
        }
        st.qb
            .caps
            .iter()
            .find(|p| p.start <= c && c <= p.end)
            .and_then(|p| p.step(&text, c, forward))
    };
    let Some(at) = next else { return false };
    {
        let mut st = s.borrow_mut();
        st.qb.anchor = at;
        st.qb.cursor = at;
        st.qb.lenses.editing = false;
    }
    w.set_anchor_to(-1);
    w.set_caret_to(at as i32);
    w.set_caret_ask(w.get_caret_ask() + 1);
    true
}

/// The pill over a whole place group: taken out, or turned into "in none of
/// them" and back, the pointer kept on it so a second `!` undoes the first.
pub fn group_pill(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    text: &str,
    t: &lens::Term,
    what: &str,
) -> bool {
    let Some((query, caret)) = crate::qgroup::whole(text, t, what) else {
        return false;
    };
    write(w, state, query, caret);
    if what == "neg" {
        let not = !t.not;
        state.borrow_mut().qb.hover = Some(lens::Term {
            start: t.start,
            end: caret,
            not,
            field: Some("under"),
            value: String::new(),
        });
        w.set_hov_not(not);
        w.set_hov_on(true);
    }
    true
}
