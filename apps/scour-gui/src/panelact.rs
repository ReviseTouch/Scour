//! What a press in the time, type and size panels writes: a preset, a run of
//! bars or an arrow on them, a scale or a mode, a range of calendar days, a
//! kind or an extension toggled. Each leaves the panel open on its term.

use std::cell::RefCell;
use std::rc::Rc;

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_ui::calendar;
use scour_ui::lens::{self, Lens, MODES, SCALES, Selected};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::link::Link;
use crate::qwire::{self, Lensing, write};
use crate::{MainWindow, State, qtime, qtype};

/// The calendar's two months: the one `back` months before today's, and the
/// one before that, in the language of the window.
pub fn show_calendar(w: &MainWindow, back: i64, cat: &Catalogue) {
    let (today, y, m) = calendar::today(qtime::now());
    let months: Vec<crate::CalMonth> = [-1 - back, -back]
        .into_iter()
        .map(|delta| {
            let (y, m) = calendar::shift(y, m, delta);
            let days: Vec<crate::CalDay> = calendar::month(y, m)
                .into_iter()
                .map(|d| crate::CalDay {
                    n: d.n as i32,
                    label: d.day.to_string().into(),
                    here: d.here,
                    today: d.n == today,
                    future: d.n > today,
                })
                .collect();
            crate::CalMonth {
                title: format!("{} {y}", cat.get(calendar::MONTHS[m as usize - 1])).into(),
                days: ModelRc::new(VecModel::from(days)),
            }
        })
        .collect();
    w.set_cal_months(ModelRc::new(VecModel::from(months)));
}

/// Write `term` in place of the lens's own and keep the panel open on it.
fn put(w: &MainWindow, state: &Rc<RefCell<State>>, term: &str, redraw: &impl Fn(Lensing)) {
    let text = w.get_query().to_string();
    let at = state.borrow().qb.lenses.open.as_ref().map(|o| (o.a, o.b));
    let Some((a, b)) = at.filter(|&(_, b)| b <= text.len()) else {
        return;
    };
    let q = format!("{}{term}{}", &text[..a], &text[b..]);
    state.borrow_mut().qb.lenses.hold = true;
    write(w, state, q, a + term.len());
    redraw(Lensing::Asked);
}

/// Write a whole new query, the caret on the panel's term, the panel open.
fn rewrite(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    (q, caret): (String, usize),
    redraw: &impl Fn(Lensing),
) {
    state.borrow_mut().qb.lenses.hold = true;
    write(w, state, q, caret);
    redraw(Lensing::Asked);
}

/// Enter or Tab on a kind or an extension: toggled in, the panel open on Tab.
pub fn value(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    field: &str,
    v: &str,
    keep: bool,
    redraw: &impl Fn(Lensing),
) {
    let text = w.get_query().to_string();
    let at = state
        .borrow()
        .qb
        .lenses
        .open
        .as_ref()
        .map_or((0, 0), |o| (o.a, o.b));
    let (q, caret) = qtype::toggle(&text, field, v, false, at);
    if keep {
        rewrite(w, state, (q, caret), redraw);
    } else {
        w.set_lens_open(false);
        write(w, state, q, caret);
        redraw(Lensing::Keep);
    }
}

/// A chip or tile pressed, by its line in the lens; Alt leaves it out.
fn press(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    i: usize,
    alt: bool,
    redraw: &impl Fn(Lensing),
) {
    let text = w.get_query().to_string();
    let it = {
        let st = state.borrow();
        let Some(o) = st.qb.lenses.open.as_ref() else {
            return;
        };
        let Some(it) = o.items.get(i) else { return };
        (o.kind, it.term.clone(), it.value.clone(), (o.a, o.b))
    };
    match it {
        (Lens::Kind | Lens::Ext, _, Some((field, value)), at) => {
            rewrite(
                w,
                state,
                qtype::toggle(&text, field, &value, alt, at),
                redraw,
            );
        }
        (Lens::Time | Lens::Size, term, ..) => {
            let bare = term.trim_start_matches('!');
            let term = if alt { format!("!{bare}") } else { term };
            put(w, state, &term, redraw);
        }
        _ => {}
    }
}

/// The time term for bars `a..=b` as the panel now looks; the bars take the
/// arrows from here on.
fn bars(w: &MainWindow, state: &Rc<RefCell<State>>, a: usize, b: usize, redraw: &impl Fn(Lensing)) {
    let term = {
        let mut st = state.borrow_mut();
        st.qb.lenses.look.armed = true;
        let look = st.qb.lenses.look;
        let Some(o) = st.qb.lenses.open.as_ref().filter(|o| o.kind == Lens::Time) else {
            return;
        };
        qtime::bars_term(&o.term, &look, a, b)
    };
    put(w, state, &term, redraw);
}

/// An arrow on the bars: true when it moved them.
fn bar_key(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    dir: i32,
    shift: bool,
    redraw: &impl Fn(Lensing),
) -> bool {
    let run = {
        let st = state.borrow();
        let look = st.qb.lenses.look;
        st.qb
            .lenses
            .open
            .as_ref()
            .filter(|o| o.kind == Lens::Time && look.armed)
            .and_then(|o| qtime::step(&o.term.value, &look, dir, shift))
    };
    let Some((a, b)) = run else { return false };
    bars(w, state, a, b, redraw);
    true
}

/// Between, Before or After: the run the term is, written again in it.
fn mode(w: &MainWindow, state: &Rc<RefCell<State>>, i: usize, redraw: &impl Fn(Lensing)) -> bool {
    let term = {
        let mut st = state.borrow_mut();
        let lenses = &mut st.qb.lenses;
        lenses.look.mode = MODES[i.min(MODES.len() - 1)];
        let look = lenses.look;
        lenses.open.as_ref().and_then(|o| {
            let s = qtime::shown(&o.term.value, &look)?;
            let t = lens::time_term(
                &o.term,
                Selected {
                    mode: look.mode,
                    ..s
                },
            );
            Some(if t.is_empty() { "dm:".to_string() } else { t })
        })
    };
    match term {
        Some(t) => {
            put(w, state, &t, redraw);
            true
        }
        None => false,
    }
}

/// A range of days picked in the calendar: dates, both ends included.
fn days(w: &MainWindow, state: &Rc<RefCell<State>>, a: i64, b: i64, redraw: &impl Fn(Lensing)) {
    let term = {
        let st = state.borrow();
        let Some(o) = st.qb.lenses.open.as_ref() else {
            return;
        };
        calendar::range_term(o.term.field.unwrap_or("dm"), o.term.not, a, b)
    };
    put(w, state, &term, redraw);
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
        window.on_lens_press(move |i, alt| {
            if let Some(w) = weak.upgrade() {
                press(&w, &state, i.max(0) as usize, alt, &redraw);
            }
        });
    }
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        window.on_lens_bars(move |a, b| {
            if let Some(w) = weak.upgrade() {
                bars(&w, &state, a.max(0) as usize, b.max(0) as usize, &redraw);
            }
        });
    }
    {
        let (weak, state, redraw) = (window.as_weak(), Rc::clone(state), redraw.clone());
        window.on_lens_bar_key(move |dir, shift| {
            weak.upgrade()
                .is_some_and(|w| bar_key(&w, &state, dir, shift, &redraw))
        });
    }
    {
        let (weak, state, link, cat) = (
            window.as_weak(),
            Rc::clone(state),
            Rc::clone(link),
            Rc::clone(cat),
        );
        window.on_lens_scale(move |i| {
            let Some(w) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.qb.lenses.look.scale = SCALES[(i.max(0) as usize).min(SCALES.len() - 1)];
            qwire::refilled(&w, &mut st, &link, &cat.borrow());
        });
    }
    {
        let (weak, state, link, cat, redraw) = (
            window.as_weak(),
            Rc::clone(state),
            Rc::clone(link),
            Rc::clone(cat),
            redraw.clone(),
        );
        window.on_lens_mode(move |i| {
            let Some(w) = weak.upgrade() else { return };
            if !mode(&w, &state, i.max(0) as usize, &redraw) {
                qwire::refilled(&w, &mut state.borrow_mut(), &link, &cat.borrow());
            }
        });
    }
    {
        let (weak, state, cat) = (window.as_weak(), Rc::clone(state), Rc::clone(cat));
        window.on_cal_opened(move || {
            let Some(w) = weak.upgrade() else { return };
            state.borrow_mut().qb.cal_back = 0;
            show_calendar(&w, 0, &cat.borrow());
        });
    }
    {
        let (weak, state, cat) = (window.as_weak(), Rc::clone(state), Rc::clone(cat));
        window.on_cal_shift(move |by| {
            let Some(w) = weak.upgrade() else { return };
            // Back into the past only: the newest month shown is today's.
            let back = (state.borrow().qb.cal_back - i64::from(by)).max(0);
            state.borrow_mut().qb.cal_back = back;
            show_calendar(&w, back, &cat.borrow());
        });
    }
    {
        let (weak, state) = (window.as_weak(), Rc::clone(state));
        window.on_lens_cal_range(move |a, b| {
            if let Some(w) = weak.upgrade() {
                days(&w, &state, i64::from(a), i64::from(b), &redraw);
            }
        });
    }
}
