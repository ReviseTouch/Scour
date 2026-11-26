//! The status bar's panel of what the service keeps: open, it is asked for
//! every two seconds; shut, nothing runs. Its presses, and what came of them.

use super::*;
use crate::resource_gauges as gauges;
use crate::resource_lines as lines;
use crate::resources::{self as panel, Press};
use scour_ui::resources::{self as says, Do};

pub(crate) fn wire(wiring: &Wiring) {
    let Wiring {
        window,
        cat,
        state,
        link,
        ..
    } = *wiring;
    let g = window.global::<HeldPanel>();
    let poll = Rc::new(slint::Timer::default());
    let tick: Rc<dyn Fn()> = {
        let weak = window.as_weak();
        let state = Rc::clone(state);
        let link = Rc::clone(link);
        let cat = Rc::clone(cat);
        Rc::new(move || {
            let Some(w) = weak.upgrade() else { return };
            let now = std::time::Instant::now();
            let mut s = state.borrow_mut();
            if !s.res.open {
                return;
            }
            if s.res.expire(now) {
                draw(&w, &s.res, &cat.borrow());
            }
            if s.res.due(now) {
                link.send(Ask::Resources);
            }
        })
    };
    {
        let weak = window.as_weak();
        let state = Rc::clone(state);
        let link = Rc::clone(link);
        let cat = Rc::clone(cat);
        let poll = Rc::clone(&poll);
        g.on_toggle(move || {
            let Some(w) = weak.upgrade() else { return };
            if state.borrow().res.open {
                shut(&w, &state, &poll);
                return;
            }
            state.borrow_mut().res.open = true;
            w.global::<HeldPanel>().set_open(true);
            draw(&w, &state.borrow().res, &cat.borrow());
            // The status bar's own line, asked for again beside it.
            link.send(Ask::Status);
            tick();
            let tick = Rc::clone(&tick);
            poll.start(slint::TimerMode::Repeated, panel::EVERY, move || tick());
        });
    }
    {
        let weak = window.as_weak();
        let state = Rc::clone(state);
        let poll = Rc::clone(&poll);
        g.on_close(move || {
            if let Some(w) = weak.upgrade() {
                shut(&w, &state, &poll);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Rc::clone(state);
        let link = Rc::clone(link);
        let cat = Rc::clone(cat);
        g.on_act(move |token, step| {
            let Some(w) = weak.upgrade() else { return };
            let Some(press) = Press::named(&token) else {
                return;
            };
            // Shown in the file manager; nothing is asked of the service.
            if says::thing(&token).is_some_and(|t| t.act.what == Do::Open) {
                let Press::Clear(kind) = press else { return };
                let path = state.borrow().res.path_of(kind);
                if let Some(path) = path
                    && let Err(e) = scour_openers::open(std::ffi::OsStr::new(&path))
                {
                    trace(&format!("resources: the folder did not open: {e}"));
                }
                return;
            }
            let ask = state.borrow_mut().res.step(press, &step);
            if ask {
                link.send(Ask::Press { press });
            }
            draw(&w, &state.borrow().res, &cat.borrow());
        });
    }
}

/// Closed by its ×, Escape, a press outside it or the status bar again.
fn shut(w: &MainWindow, state: &Rc<RefCell<State>>, poll: &slint::Timer) {
    poll.stop();
    state.borrow_mut().res.shut();
    w.global::<HeldPanel>().set_open(false);
    // A tip over a row that is gone would stay: nothing leaves it to say so.
    if (-44..=-40).contains(&w.get_tip_owner()) {
        w.set_tip_owner(-1);
        w.set_tip_pending("".into());
        w.set_tip_text("".into());
    }
}

/// The panel's answer: drawn, and after a restart the status bar's line too.
pub(crate) fn landed(cx: &Cx, reply: Result<Box<scour_proto::Resources>, String>) {
    let Cx {
        w,
        state,
        cat,
        link,
        ..
    } = *cx;
    let r = match reply {
        Ok(r) => *r,
        Err(why) => {
            trace(&format!("resources: {why}"));
            state.borrow_mut().res.unanswered();
            return;
        }
    };
    if state.borrow_mut().res.took(r) {
        link.send(Ask::Status);
    }
    let s = state.borrow();
    if s.res.open {
        draw(w, &s.res, cat);
    }
}

/// What a press came to, said in its row; what it changed is asked for at once.
pub(crate) fn pressed(cx: &Cx, press: Press, outcome: Result<(), String>) {
    let Cx {
        w,
        state,
        cat,
        link,
        ..
    } = *cx;
    let taken = outcome.is_ok() && press != Press::Restart;
    state
        .borrow_mut()
        .res
        .answered(press, outcome, std::time::Instant::now());
    if taken {
        link.send(Ask::Resources);
        link.send(Ask::Status);
    }
    let s = state.borrow();
    if s.res.open {
        draw(w, &s.res, cat);
    }
}

/// Replace a model's rows in place: a row rebuilt between a press and its
/// release is a press nobody receives. A new model only the first time.
fn put<T: Clone + PartialEq + 'static>(model: ModelRc<T>, rows: Vec<T>) -> Option<ModelRc<T>> {
    let Some(kept) = model.as_any().downcast_ref::<VecModel<T>>() else {
        return Some(ModelRc::new(VecModel::from(rows)));
    };
    if kept.row_count() != rows.len() {
        kept.set_vec(rows);
        return None;
    }
    for (at, row) in rows.into_iter().enumerate() {
        if kept.row_data(at).as_ref() != Some(&row) {
            kept.set_row_data(at, row);
        }
    }
    None
}

/// The panel's words that are not an answer's. Again when the language is.
pub(crate) fn words(w: &MainWindow, cat: &Catalogue) {
    let g = w.global::<HeldPanel>();
    g.set_title(t(cat, says::TITLE));
    g.set_on_disk(t(cat, says::ON_DISK));
    g.set_memory(t(cat, says::MEMORY));
    g.set_processor(t(cat, says::PROCESSOR));
    g.set_programs(t(cat, says::PROGRAMS));
    g.set_cancel(t(cat, "Cancel"));
    g.set_hint(t(cat, says::OPEN_HINT));
    g.set_unreadable(t(cat, says::UNREADABLE));
}

/// Everything the panel shows, from the last answer and the asks open.
fn draw(w: &MainWindow, p: &panel::Panel, cat: &Catalogue) {
    let g = w.global::<HeldPanel>();
    let Some(r) = p.last.as_ref() else { return };
    if let Some(m) = put(g.get_held(), lines::held_lines(p, r, cat)) {
        g.set_held(m);
    }
    if let Some(m) = put(g.get_mem(), gauges::mem_lines(r, cat)) {
        g.set_mem(m);
    }
    if let Some(m) = put(g.get_running(), lines::run_lines(r, cat)) {
        g.set_running(m);
    }
    g.set_sort(lines::sort_line(p, r, cat));
    g.set_disk_total(compact_bytes(r.files.iter().map(|h| h.bytes).sum()).into());

    let scour = gauges::scour_bytes(r);
    g.set_mem_total(if scour > 0 {
        compact_bytes(scour).into()
    } else {
        slint::SharedString::new()
    });
    g.set_mem_unknown(scour == 0 && r.memory.total == 0);
    let machine = gauges::machine(r, cat);
    g.set_machine_shown(machine.is_some());
    if let Some(m) = machine {
        g.set_machine_us(m.us);
        g.set_machine_others(m.others);
        g.set_machine_us_said(m.us_said.into());
        g.set_machine_others_said(m.others_said.into());
        g.set_machine_said(m.said.into());
    }

    let unknown = gauges::cpu_unknown(r);
    g.set_cpu_unknown(unknown);
    let (short, long, machine) = if unknown {
        Default::default()
    } else {
        gauges::cpu_said(p, r, cat)
    };
    g.set_cpu_short(short.into());
    g.set_cpu_long(long.into());
    g.set_cpu_machine(machine.into());
    let line = gauges::spark(&p.spark.iter().copied().collect::<Vec<_>>());
    let (path, area, end) = line.unwrap_or_default();
    g.set_spark_line(path.into());
    g.set_spark_area(area.into());
    g.set_spark_end(end);

    let foot = lines::foot(p, r, cat);
    g.set_foot(foot.said);
    g.set_foot_no(foot.no);
    g.set_restart(foot.restart);
    g.set_restartable(r.restartable);
    g.set_restarting(p.restarting.is_some());
}
