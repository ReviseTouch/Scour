//! What a press in the place lens writes: a tile into or out of the place
//! group, a folder added or left out, an entry of "Searching in" taken out,
//! a disk's question answered. Each leaves the lens open on the group.

use std::cell::RefCell;
use std::rc::Rc;

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_ui::lens::{self, Pick};
use scour_ui::path::home_form;
use slint::ComponentHandle;

use crate::link::{Ask, Got, Link};
use crate::qwire::{self, Lensing, home, write};
use crate::{MainWindow, State};

/// Where the place group starts in `text`, else the lens's own term: where
/// [`lens::place_settle`] writes an empty `under:` once the group is gone.
fn group_at(text: &str, home: &str, lens_at: usize) -> usize {
    lens::scope(text, home)
        .within
        .first()
        .map_or(lens_at, |h| h.term.start.min(lens_at))
}

/// A press on a tile, or Enter on one: [`lens::tile_toggle`], and the list
/// goes to the disk's own root when a source holds all of it.
pub fn tile_pick(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    k: usize,
    pick: Pick,
    redraw: &impl Fn(Lensing),
) {
    let text = w.get_query().to_string();
    let hm = home();
    let tile = state
        .borrow()
        .qb
        .lenses
        .open
        .as_ref()
        .and_then(|o| o.tiles.get(k))
        .filter(|t| t.choosable())
        .map(|t| (t.roots.clone(), t.whole.then(|| t.root.clone())));
    let Some((roots, root)) = tile else { return };
    let at = group_at(&text, &hm, lens_at(state));
    let (q, _) = lens::tile_toggle(&text, &hm, &roots, pick);
    let (q, caret) = lens::place_settle(&q, at, lens_not(state));
    rewrite(w, state, q, caret, root, redraw);
}

/// Write `q` and keep the lens open, its list in `go`, else where it is.
fn rewrite(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    q: String,
    caret: usize,
    go: Option<String>,
    redraw: &impl Fn(Lensing),
) {
    let active = go.or_else(|| {
        let st = state.borrow();
        st.qb.lenses.open.as_ref().map(|o| o.place.active.clone())
    });
    {
        let lenses = &mut state.borrow_mut().qb.lenses;
        lenses.browse = active.map(|a| (q.clone(), a));
        lenses.hold = true;
    }
    write(w, state, q, caret);
    redraw(Lensing::Asked);
}

/// The lens was opened on an exclusion: it stays on one.
fn lens_not(state: &Rc<RefCell<State>>) -> bool {
    let st = state.borrow();
    st.qb.lenses.open.as_ref().is_some_and(|o| o.term.not)
}

/// Where the open lens's term starts, or the end of nothing.
fn lens_at(state: &Rc<RefCell<State>>) -> usize {
    let st = state.borrow();
    st.qb.lenses.open.as_ref().map_or(usize::MAX, |o| o.a)
}

/// An entry of the "Searching in" line taken out: a disk held whole as one.
fn drop_held(w: &MainWindow, state: &Rc<RefCell<State>>, i: usize, redraw: &impl Fn(Lensing)) {
    let text = w.get_query().to_string();
    let hm = home();
    let spot = {
        let st = state.borrow();
        let tiles = st.qb.lenses.open.as_ref().map(|o| o.tiles.as_slice());
        lens::spots(&text, &hm, tiles.unwrap_or_default())
            .into_iter()
            .nth(i)
    };
    let Some(spot) = spot else { return };
    let at = group_at(&text, &hm, lens_at(state));
    let (q, _) = spot.remove(&text, &hm);
    let (q, caret) = lens::place_settle(&q, at, lens_not(state));
    rewrite(w, state, q, caret, None, redraw);
}

pub fn wire(
    window: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    cat: &Rc<RefCell<Rc<Catalogue>>>,
    redraw: impl Fn(Lensing) + Clone + 'static,
) {
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        window.on_lens_tile(move |i, also| {
            if let Some(w) = weak.upgrade() {
                let pick = if also { Pick::Also } else { Pick::Only };
                tile_pick(&w, &state, i.max(0) as usize, pick, &redraw);
            }
        });
    }
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        window.on_lens_held_drop(move |i| {
            if let Some(w) = weak.upgrade() {
                drop_held(&w, &state, i.max(0) as usize, &redraw);
            }
        });
    }
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        window.on_where_pressed(move || {
            if let Some(w) = weak.upgrade() {
                qwire::open_or_focus_field(&w, &state, &redraw, "under");
            }
        });
    }
    let (weak, state, link, cat) = (
        window.as_weak(),
        Rc::clone(state),
        Rc::clone(link),
        Rc::clone(cat),
    );
    window.on_lens_place_act(move |what, target| {
        if let Some(w) = weak.upgrade() {
            act(&w, &state, &link, &cat.borrow(), &redraw, &what, &target);
        }
    });
}

/// The answer to a disk's question, and the lens asks for the disks again.
fn answer_disk(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Link,
    cat: &Catalogue,
    volume: &str,
    what: &str,
) {
    link.send(Ask::Disk {
        volume: volume.to_string(),
        what: what.to_string(),
    });
    if what == "index" {
        crate::told(w, cat.get("The disk is being indexed.").as_ref().into());
    }
    let lenses = &mut state.borrow_mut().qb.lenses;
    lenses.forget("volumes");
    lenses.forget("sources");
}

/// The term the lens is on, and the caret — editing while no click pinned
/// the list to a folder — for [`lens::place_add_at`] to let it give way.
struct On {
    a: usize,
    b: usize,
    field: &'static str,
    neg: &'static str,
    not: bool,
    caret: lens::Caret,
}

fn on(state: &Rc<RefCell<State>>, text: &str) -> Option<On> {
    let st = state.borrow();
    let lenses = &st.qb.lenses;
    let o = lenses.open.as_ref()?;
    let t = &o.term;
    let pinned = crate::qplace::pinned(&lenses.browse, text).is_some();
    Some(On {
        a: o.a,
        b: o.b,
        field: t.field.unwrap_or("under"),
        neg: if t.not { "!" } else { "" },
        not: t.not,
        caret: lens::Caret {
            at: st.qb.cursor.min(text.len()),
            editing: lenses.editing && !pinned,
        },
    })
}

/// A path as a term's value: quoted when a space, `|` or `;` would cut it.
pub(crate) fn quote(s: String) -> String {
    if s.chars()
        .any(|c| c.is_whitespace() || matches!(c, '|' | ';'))
    {
        format!("\"{s}\"")
    } else {
        s
    }
}

/// Into a folder, or its other reading (`flip`): the term becomes the folder
/// and a `/`, and the lens stays open in it.
fn go(w: &MainWindow, state: &Rc<RefCell<State>>, on: &On, flip: bool, target: &str) {
    let text = w.get_query().to_string();
    let field = match (flip, on.field) {
        (true, "under") => "parent",
        (true, _) => "under",
        _ => on.field,
    };
    let shown = home_form(&home(), target);
    let slash = if shown.ends_with('/') {
        shown
    } else {
        format!("{shown}/")
    };
    let raw = format!("{}{field}:{}", on.neg, quote(slash));
    let caret = on.a + raw.len() - usize::from(raw.ends_with('"'));
    write(
        w,
        state,
        format!("{}{raw}{}", &text[..on.a], &text[on.b..]),
        caret,
    );
}

/// This folder chosen: it takes the term's place and the lens closes.
fn choose(w: &MainWindow, state: &Rc<RefCell<State>>, on: &On, target: &str) {
    let text = w.get_query().to_string();
    let tail = if text[on.b..].starts_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    let ins = format!(
        "{}{}:{}{tail}",
        on.neg,
        on.field,
        quote(home_form(&home(), target))
    );
    let q = format!("{}{ins}{}", &text[..on.a], &text[on.b..]);
    state.borrow_mut().qb.lenses.close();
    w.set_lens_open(false);
    write(w, state, q, on.a + ins.len());
}

/// The system's own folder picker, its answer delivered as `PickedFolder`.
fn pick(on: &On, start: &str) {
    let (a, b, start) = (on.a, on.b, start.to_string());
    let prefix = format!("{}{}:", on.neg, on.field);
    std::thread::spawn(move || {
        let start_p = (!start.is_empty()).then(|| std::path::Path::new(&start));
        if let Some(chosen) = scour_openers::pick_folder(start_p) {
            let _ = slint::invoke_from_event_loop(move || {
                crate::deliver(Got::PickedFolder {
                    a,
                    b,
                    prefix,
                    path: chosen,
                });
            });
        }
    });
}

fn act(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Link,
    cat: &Catalogue,
    redraw: &impl Fn(Lensing),
    what: &str,
    target: &str,
) {
    let text = w.get_query().to_string();
    let Some(on) = on(state, &text) else { return };
    let hm = home();
    match what {
        "disk" | "later" | "never" => {
            let how = match what {
                "disk" => "index",
                "later" => "later",
                _ => "leave",
            };
            answer_disk(w, state, link, cat, target, how);
            redraw(Lensing::Asked);
        }
        "pick" => pick(&on, target),
        "dir" | "flip" => {
            go(w, state, &on, what == "flip", target);
            redraw(Lensing::Asked);
        }
        "choose" => {
            choose(w, state, &on, target);
            redraw(Lensing::Keep);
        }
        "inc" | "exc" => {
            let at = group_at(&text, &hm, on.a);
            let (q, _) = if what == "inc" {
                lens::place_add_at(&text, &hm, target, on.caret)
            } else {
                lens::place_exclude(&text, &hm, target)
            };
            let (q, caret) = lens::place_settle(&q, at, on.not);
            rewrite(w, state, q, caret, None, redraw);
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "placeact_tests.rs"]
mod tests;
