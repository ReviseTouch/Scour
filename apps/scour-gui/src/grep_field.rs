//! The content field's drawing and its pill and `:` menu, wired: the caret
//! and the pointer come in, `grep_lay` says where everything is, and the
//! pill's and the menu's choices, and the keys that open or empty a capsule,
//! rewrite the pattern through the shared model (`scour_ui::pattern`). The
//! pattern text is the only model.

use std::cell::RefCell;
use std::rc::Rc;

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_proto::GrepMode;
use scour_ui::pattern::{self, Item, ItemAction, edit};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::grep_lay::{self, Lay, Metrics};
use crate::link::Link;
use crate::qbar::{byte_of, chars_before};
use crate::ui::{GrepBit, GrepCap, GrepRow, GrepRun, GrepSel};
use crate::{MainWindow, State};

#[derive(Default)]
pub struct Field {
    adv: f32,
    small: f32,
    /// Char columns of the caret and the selection's other end.
    caret: usize,
    anchor: usize,
    lay: Lay,
    /// The capsule the pill was last over: its piece and alternative.
    pill: Option<(usize, usize)>,
    /// The `:` Escape closed: shut until the caret asks with another.
    shut: Option<usize>,
    colon: usize,
    menu: Vec<Item>,
}

fn metrics(f: &Field) -> Metrics {
    Metrics {
        adv: if f.adv > 0.0 { f.adv } else { 8.4 },
        small: if f.small > 0.0 { f.small } else { 6.3 },
    }
}

/// Lay the field out again and hand it to the window, with what `:` offers.
pub fn redraw(w: &MainWindow, s: &mut State, cat: &Catalogue) {
    let text = w.get_grep_pattern().to_string();
    let content = s.grep.mode == GrepMode::Text;
    let f = &mut s.grep.field;
    let n = text.chars().count();
    f.caret = f.caret.min(n);
    f.anchor = f.anchor.min(n);
    let words = |m: &str| cat.get(m).into_owned();
    f.lay = grep_lay::lay(&text, f.caret, content, metrics(f), &words);
    let l = &f.lay;
    w.set_grep_runs(model(l.runs.iter().map(|r| GrepRun {
        text: r.text.as_str().into(),
        x: r.x,
        role: r.role,
    })));
    w.set_grep_caps(model(l.caps.iter().map(|c| GrepCap {
        x: c.x,
        w: c.w,
        hw: c.hw,
        head: c.head.as_str().into(),
        not: c.not,
        live: c.live,
    })));
    w.set_grep_bits(model(l.bits.iter().map(|b| GrepBit {
        x: b.x,
        w: b.w,
        kind: b.kind,
        text: b.text.as_str().into(),
        not: b.not,
    })));
    let (a, b) = (f.anchor.min(f.caret), f.anchor.max(f.caret));
    let sels: Vec<GrepSel> = if a == b {
        Vec::new()
    } else {
        vec![GrepSel {
            x: l.xs[a],
            w: l.xs[b] - l.xs[a],
        }]
    };
    w.set_grep_sels(ModelRc::new(VecModel::from(sels)));
    w.set_grep_caret_x(l.xs[f.caret]);

    // `:` offers what the letters after it leave; nothing left, it is text.
    let asked = if content && a == b {
        pattern::asking(&text, f.caret)
    } else {
        None
    };
    let menu = match asked {
        Some((colon, typed)) if f.shut != Some(colon) => {
            f.colon = colon;
            pattern::items(&typed, |m| cat.get(m).into_owned())
        }
        Some(_) => Vec::new(),
        None => {
            f.shut = None;
            Vec::new()
        }
    };
    if menu != f.menu {
        w.set_grep_menu(model(menu.iter().map(|i| GrepRow {
            icon: i.icon.into(),
            label: cat.get(i.label).as_ref().into(),
            about: cat.get(i.about).as_ref().into(),
            sign: i.sign.into(),
        })));
        w.set_grep_menu_sel(0);
        f.menu = menu;
    }
    w.set_grep_menu_x(l.xs.get(f.colon).copied().unwrap_or(0.0));
}

fn model<T: Clone + 'static>(it: impl Iterator<Item = T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(it.collect::<Vec<T>>()))
}

/// The pattern rewritten, the caret at char `caret`: told to the window, the
/// search and the drawing.
fn rewrite(
    w: &MainWindow,
    s: &mut State,
    link: &Link,
    cat: &Catalogue,
    text: String,
    caret: usize,
) {
    w.set_grep_pattern(text.as_str().into());
    s.grep.pattern = text.clone();
    s.grep.field.caret = caret;
    s.grep.field.anchor = caret;
    w.set_grep_put_at(byte_of(&text, caret) as i32);
    w.set_grep_put(w.get_grep_put() + 1);
    crate::grep_ui::trigger(w, s, link, cat, false);
}

/// Another tab's pattern in the field, the caret at its end. Searching is the
/// caller's.
pub fn load(w: &MainWindow, s: &mut State, text: &str) {
    let n = text.chars().count();
    w.set_grep_pattern(text.into());
    s.grep.pattern = text.to_owned();
    let f = &mut s.grep.field;
    (f.caret, f.anchor, f.pill) = (n, n, None);
    w.set_grep_hov(-1);
    w.set_grep_put_at(i32::try_from(text.len()).unwrap_or(i32::MAX));
    w.set_grep_put(w.get_grep_put() + 1);
}

/// A capsule's pill pressed: `drop`, `neg` or `fz`, on the capsule it was over.
fn act(w: &MainWindow, s: &mut State, link: &Link, cat: &Catalogue, what: &str) {
    let Some((at, alt)) = s.grep.field.pill else {
        return;
    };
    let text = w.get_grep_pattern().to_string();
    let Some(start) = pattern::pieces(&text).get(at).map(|p| p.start) else {
        return;
    };
    let after = match what {
        "drop" => edit::remove(&text, at),
        "neg" => edit::toggle_not(&text, at),
        "fz" => edit::toggle_similar(&text, at, alt),
        _ => return,
    };
    // A caret past the piece moves with what the piece gained or lost.
    let (before, now) = (text.chars().count(), after.chars().count());
    let c = s.grep.field.caret;
    let caret = if c > start {
        (c + now).saturating_sub(before).max(start.min(now))
    } else {
        c
    };
    if what == "drop" {
        s.grep.field.pill = None;
        w.set_grep_hov(-1);
    }
    rewrite(w, s, link, cat, after, caret.min(now));
}

/// What the pill offers over the capsule under the pointer at `x`.
fn hover(w: &MainWindow, s: &mut State, x: f32) {
    let f = &mut s.grep.field;
    let Some((k, alt)) = (x >= 0.0).then(|| grep_lay::over(&f.lay, x)).flatten() else {
        w.set_grep_hov(-1);
        return;
    };
    let cap = &f.lay.caps[k];
    f.pill = Some((cap.piece, alt));
    let text = w.get_grep_pattern().to_string();
    let pieces = pattern::pieces(&text);
    let Some(p) = pieces.get(cap.piece) else {
        return;
    };
    let a = p.alts.get(alt).or(p.alts.first());
    w.set_grep_hov(k as i32);
    w.set_grep_hov_x(cap.x + cap.w);
    w.set_grep_hov_not(cap.not);
    w.set_grep_hov_fz(a.is_some_and(|a| !a.phrase));
    w.set_grep_hov_fz_on(a.is_some_and(|a| a.similar));
}

/// A key the pattern answers itself (`pattern::edit`): Backspace in an empty
/// capsule takes it and its operator, a space stays in its capsule (a word
/// becomes a phrase), a quote comes with its pair, Tab leaves the capsule for
/// a new one. False: the field types it, or Tab moves on.
fn key(w: &MainWindow, s: &mut State, link: &Link, cat: &Catalogue, key: &str) -> bool {
    let f = &s.grep.field;
    if s.grep.mode != GrepMode::Text || f.anchor != f.caret {
        return false;
    }
    let text = w.get_grep_pattern().to_string();
    let done = match key {
        "\u{8}" => edit::backspace(&text, f.caret),
        " " => edit::space(&text, f.caret),
        "\"" => edit::quote(&text, f.caret),
        "\t" => edit::leave(&text, f.caret),
        _ => None,
    };
    let Some((after, caret)) = done else {
        return false;
    };
    if after != text {
        rewrite(w, s, link, cat, after, caret);
    } else if caret != s.grep.field.caret {
        (s.grep.field.caret, s.grep.field.anchor) = (caret, caret);
        w.set_grep_put_at(byte_of(&text, caret) as i32);
        w.set_grep_put(w.get_grep_put() + 1);
        redraw(w, s, cat);
    }
    true
}

/// A line of what `:` offers chosen.
fn pick(w: &MainWindow, s: &mut State, link: &Link, cat: &Catalogue, i: usize) {
    let Some(item) = s.grep.field.menu.get(i).copied() else {
        return;
    };
    let text = w.get_grep_pattern().to_string();
    let (after, caret) = pattern::choose(&text, s.grep.field.colon, s.grep.field.caret, &item);
    if item.action == ItemAction::Case {
        s.grep.exact_case = !s.grep.exact_case;
        w.set_grep_case(s.grep.exact_case);
    }
    rewrite(w, s, link, cat, after, caret);
}

pub fn wire(
    window: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    cat: &Rc<RefCell<Rc<Catalogue>>>,
) {
    let weak = || window.as_weak();
    {
        let (weak, state, cat) = (weak(), Rc::clone(state), Rc::clone(cat));
        window.on_grep_advance(move |adv, small| {
            let Some(w) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            (st.grep.field.adv, st.grep.field.small) = (adv, small);
            redraw(&w, &mut st, &cat.borrow());
        });
    }
    {
        let (weak, state, cat) = (weak(), Rc::clone(state), Rc::clone(cat));
        window.on_grep_caret(move |anchor, cursor| {
            let Some(w) = weak.upgrade() else { return };
            let text = w.get_grep_pattern();
            let mut st = state.borrow_mut();
            st.grep.field.anchor = chars_before(&text, anchor.max(0) as usize);
            st.grep.field.caret = chars_before(&text, cursor.max(0) as usize);
            redraw(&w, &mut st, &cat.borrow());
        });
    }
    {
        let (weak, state) = (weak(), Rc::clone(state));
        window.on_grep_hit(move |x| {
            let Some(w) = weak.upgrade() else { return 0 };
            let col = grep_lay::hit(&state.borrow().grep.field.lay, x);
            byte_of(&w.get_grep_pattern(), col) as i32
        });
    }
    {
        let (weak, state) = (weak(), Rc::clone(state));
        window.on_grep_hover(move |x| {
            let Some(w) = weak.upgrade() else { return };
            hover(&w, &mut state.borrow_mut(), x);
        });
    }
    {
        let (weak, state, link, cat) = (weak(), Rc::clone(state), Rc::clone(link), Rc::clone(cat));
        window.on_grep_act(move |what| {
            let Some(w) = weak.upgrade() else { return };
            act(&w, &mut state.borrow_mut(), &link, &cat.borrow(), &what);
        });
    }
    {
        let (weak, state, link, cat) = (weak(), Rc::clone(state), Rc::clone(link), Rc::clone(cat));
        window.on_grep_key(move |k| {
            let Some(w) = weak.upgrade() else {
                return false;
            };
            key(&w, &mut state.borrow_mut(), &link, &cat.borrow(), &k)
        });
    }
    {
        let (weak, state, link, cat) = (weak(), Rc::clone(state), Rc::clone(link), Rc::clone(cat));
        window.on_grep_menu_pick(move |i| {
            let Some(w) = weak.upgrade() else { return };
            pick(
                &w,
                &mut state.borrow_mut(),
                &link,
                &cat.borrow(),
                i.max(0) as usize,
            );
        });
    }
    {
        let (weak, state, cat) = (weak(), Rc::clone(state), Rc::clone(cat));
        window.on_grep_menu_close(move || {
            let Some(w) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.grep.field.shut = Some(st.grep.field.colon);
            redraw(&w, &mut st, &cat.borrow());
        });
    }
    hook(window, state, link, cat);
}

/// `SCOUR_GUI_GREP=pattern` opens the content field with it, the caret at
/// `SCOUR_GUI_GREP_CARET` characters (the end if unset): for a picture.
fn hook(
    window: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Rc<Link>,
    cat: &Rc<RefCell<Rc<Catalogue>>>,
) {
    let Ok(text) = std::env::var("SCOUR_GUI_GREP") else {
        return;
    };
    let caret = std::env::var("SCOUR_GUI_GREP_CARET")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(text.chars().count());
    let (weak, state, link, cat) = (
        window.as_weak(),
        Rc::clone(state),
        Rc::clone(link),
        Rc::clone(cat),
    );
    let t = Box::leak(Box::new(slint::Timer::default()));
    t.start(
        slint::TimerMode::SingleShot,
        std::time::Duration::from_millis(600),
        move || {
            let Some(w) = weak.upgrade() else { return };
            if !w.get_grep_open() {
                w.invoke_grep_toggled();
            }
            let (weak, state, link, cat, text) = (
                w.as_weak(),
                Rc::clone(&state),
                Rc::clone(&link),
                Rc::clone(&cat),
                text.clone(),
            );
            slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
                let Some(w) = weak.upgrade() else { return };
                rewrite(
                    &w,
                    &mut state.borrow_mut(),
                    &link,
                    &cat.borrow(),
                    text,
                    caret,
                );
            });
        },
    );
}
