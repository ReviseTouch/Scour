//! The query line.

use super::*;

pub(crate) fn wire(wiring: &Wiring) {
    let Wiring {
        window,
        cat,
        tabs,
        tabs_later,
        state,
        rows,
        facets,
        link,
        ..
    } = *wiring;
    {
        let state = state.clone();
        let link = link.clone();
        let facets = facets.clone();
        let model = Rc::clone(rows);
        let weak = window.as_weak();
        let tabs = Rc::clone(tabs);
        let tabs_later = Rc::clone(tabs_later);
        let cat = Rc::clone(cat);
        window.on_query_changed(move |text| {
            stir(&state, &link);
            // **A query has a length, and the reason is the renderer**: the
            // software renderer places every glyph at an `i16` coordinate, so a
            // line past 32 767 physical pixels is a panic with no error path.
            let text = if text.chars().count() > 600 {
                let cut: slint::SharedString = text.chars().take(600).collect::<String>().into();
                if let Some(w) = weak.upgrade() {
                    w.set_query(cut.clone());
                }
                cut
            } else {
                text
            };
            trace(&format!("query-changed {text:?}"));
            if let Some(w) = weak.upgrade() {
                w.set_note(slint::SharedString::new());
            }
            let generation = {
                let mut s = state.borrow_mut();
                // Typed, unless it is the text a lens or a button just wrote.
                let ours = s.qb.wrote.take().is_some_and(|w| w == text.as_str());
                s.qb.lenses.editing = !ours;
                s.query = text.to_string();
                s.advance_query();
                s.typed_at = Some(std::time::Instant::now());
                s.generation
            };
            // The tab in front is named by what is typed in it.
            tabs.borrow_mut().hold(now_tab(&state.borrow()));
            if let Some(w) = weak.upgrade() {
                draw_tabs(&w, &tabs.borrow(), &cat.borrow());
            }
            {
                let link = Rc::clone(&link);
                let tabs = Rc::clone(&tabs);
                tabs_later.start(
                    slint::TimerMode::SingleShot,
                    std::time::Duration::from_secs(2),
                    move || keep_tabs(&link, &tabs.borrow()),
                );
            }
            // Laid over the text as it is now, so a deletion shows this frame.
            if let Some(w) = weak.upgrade() {
                qwire::draw(
                    &w,
                    &mut state.borrow_mut(),
                    &link,
                    &cat.borrow(),
                    qwire::Lensing::Follow,
                );
                if state.borrow().grep.open && !state.borrow().grep.pattern.is_empty() {
                    grep_ui::trigger(&w, &mut state.borrow_mut(), &link, &cat.borrow(), false);
                }
                let s = state.borrow();
                if w.get_past_open() {
                    let lines = past_matching(&s.past, &s.query);
                    w.set_past_open(!lines.is_empty());
                    w.set_past_picked(0);
                    w.set_past(ModelRc::new(VecModel::from(lines)));
                }
            }
            // Counts from the previous matching set are worse than an empty rail.
            facets.set_vec(Vec::new());
            if let Some(w) = weak.upgrade() {
                w.set_busy(true);
            }
            // Straight out, no timer: generation drops a stale reply regardless.
            let rows = weak
                .upgrade()
                .map_or(60, |w| w.get_visible_rows().max(0) as u32);
            if DEBOUNCE_MS == 0 {
                trace(&format!("dispatch {generation}, {rows} rows"));
                dispatch(&state, &link, &model, rows);
                return;
            }
            let state = state.clone();
            let link = link.clone();
            let model = Rc::clone(&model);
            let weak = weak.clone();
            slint::Timer::single_shot(std::time::Duration::from_millis(DEBOUNCE_MS), move || {
                if state.borrow().generation != generation {
                    trace(&format!("timer {generation} superseded"));
                    return;
                }
                let _ = weak;
                trace(&format!("dispatch {generation}"));
                dispatch(&state, &link, &model, rows);
            });
        });
    }
}
