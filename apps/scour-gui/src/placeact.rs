//! What a press in the place lens writes: a tile into or out of the place
//! group, a folder added or left out, an entry of "Searching in" taken out,
//! a disk's question answered. Each leaves the lens open on the group.

use std::cell::RefCell;
use std::rc::Rc;

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_ui::lens::{self, Pick};
use scour_ui::path::{full_form, home_form, untrailed};
use slint::ComponentHandle;

use crate::link::{Ask, Got, Link};
use crate::qwire::{self, Lensing, home, write};
use crate::{MainWindow, State};

/// Where the caret goes after a press wrote the place group: at the end of
/// its first member, which starts where the group always did, so the lens
/// hanging from it does not move. With no group left, an empty `under:`
/// waits at the end, for the lens to stay open on.
fn settle(text: String, home: &str) -> (String, usize) {
    if let Some(h) = lens::scope(&text, home).within.first() {
        let end = h.term.end;
        return (text, end);
    }
    if let Some(t) = lens::terms(&text)
        .into_iter()
        .find(|t| t.field == Some("under") && !t.not && t.value.is_empty())
    {
        return (text, t.end);
    }
    let gap = if text.is_empty() || text.ends_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    let out = format!("{text}{gap}under:");
    let n = out.len();
    (out, n)
}

/// `path` lies below `root`, not on it.
fn inside(path: &str, root: &str) -> bool {
    path != root && (root == "/" || path.starts_with(&format!("{root}/")))
}

/// "Add": the folder joins the place group. The term being typed or walked
/// gives way to it; a member it lies inside is narrowed to it, since keeping
/// both would search the wider one.
pub fn add(text: &str, home: &str, path: &str, typing: Option<&lens::Term>) -> String {
    let want = untrailed(&full_form(home, path)).to_string();
    let mut text = match typing {
        Some(t) => lens::remove_term(text, t).0,
        None => text.to_string(),
    };
    let scope = lens::scope(&text, home);
    if scope.holds(&want) {
        return text;
    }
    if let Some(h) = scope.within.iter().find(|h| inside(&want, &h.path)) {
        text = lens::remove_term(&text, &h.term).0;
    }
    lens::place_toggle(&text, home, &want, Pick::Also).0
}

/// "Exclude": `!under:` the folder, a term of its own at the end.
pub fn exclude(text: &str, home: &str, path: &str) -> String {
    let want = untrailed(&full_form(home, path)).to_string();
    if lens::scope(text, home)
        .outside
        .iter()
        .any(|h| h.path == want)
    {
        return text.to_string();
    }
    let shown = home_form(home, &want);
    let value = if shown.contains(|c: char| c.is_whitespace() || c == '|') {
        format!("\"{shown}\"")
    } else {
        shown
    };
    let gap = if text.is_empty() || text.ends_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    format!("{text}{gap}!under:{value}")
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
    let (q, _) = lens::tile_toggle(&text, &hm, &roots, pick);
    let (q, caret) = settle(q, &hm);
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

/// An entry of the "Searching in" line taken out.
fn drop_held(w: &MainWindow, state: &Rc<RefCell<State>>, i: usize, redraw: &impl Fn(Lensing)) {
    let text = w.get_query().to_string();
    let hm = home();
    let scope = lens::scope(&text, &hm);
    let n = scope.within.len();
    let q = if let Some(h) = scope.within.get(i) {
        lens::place_toggle(&text, &hm, &h.path, Pick::Also).0
    } else if let Some(h) = scope.outside.get(i - n) {
        lens::remove_term(&text, &h.term).0
    } else {
        return;
    };
    let (q, caret) = settle(q, &hm);
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

/// The term the lens is on, and whether it is still being typed or walked:
/// empty, ending in `/`, or with letters after its last `/`.
struct On {
    a: usize,
    b: usize,
    field: &'static str,
    neg: &'static str,
    typing: Option<lens::Term>,
}

fn on(state: &Rc<RefCell<State>>) -> Option<On> {
    let st = state.borrow();
    let o = st.qb.lenses.open.as_ref()?;
    let t = &o.term;
    let typing =
        !t.not && (t.value.is_empty() || t.value.ends_with('/') || !o.place.part.is_empty());
    Some(On {
        a: o.a,
        b: o.b,
        field: t.field.unwrap_or("under"),
        neg: if t.not { "!" } else { "" },
        typing: typing.then(|| t.clone()),
    })
}

fn quote(s: String) -> String {
    if s.chars().any(char::is_whitespace) {
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
    let Some(on) = on(state) else { return };
    let text = w.get_query().to_string();
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
        "inc" => {
            let q = add(&text, &hm, target, on.typing.as_ref());
            let (q, caret) = settle(q, &hm);
            rewrite(w, state, q, caret, None, redraw);
        }
        "exc" => rewrite(w, state, exclude(&text, &hm, target), on.b, None, redraw),
        _ => {}
    }
}

#[cfg(test)]
#[path = "placeact_tests.rs"]
mod tests;
