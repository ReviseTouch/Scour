//! The report.

use super::*;

pub(crate) fn wire(wiring: &Wiring) {
    let Wiring {
        window,
        cat,
        state,
        weighed,
        link,
        ..
    } = *wiring;
    // Weighed when looked at and whenever a folder in it is pressed, never while
    // the search tab is showing, because it is a walk.
    {
        let state = Rc::clone(state);
        let link = Rc::clone(link);
        let cat = Rc::clone(cat);
        let weighed = Rc::clone(weighed);
        let weak = window.as_weak();
        window.on_report_open(move |path| {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            state.borrow_mut().scope = path.to_string();
            // A new scope: what is held answers for the old one.
            *weighed.borrow_mut() = report::Kept::default();
            report::words(&w, &cat, &path);
            w.set_dupes_show(t(&cat, "show"));
            w.set_dupes_hide(t(&cat, "hide"));
            w.set_dupes_confirm(t(&cat, "Confirm by reading"));
            w.set_dupe_floors(ModelRc::new(VecModel::from(
                FLOORS
                    .iter()
                    .map(|(_, label)| slint::SharedString::from(*label))
                    .collect::<Vec<_>>(),
            )));
            link.send(Ask::Usage {
                path: path.to_string(),
            });
            link.send(Ask::Kinds {
                path: path.to_string(),
            });
            link.send(Ask::Biggest {
                path: path.to_string(),
            });
            if w.get_dupes_open() {
                hunt(&w, &link, &state);
            }
        });
    }

    // The strips run the width of the report, and which of their segments can
    // carry a word is decided in Rust. A resize changes that answer, and the
    // reply is kept so the folder is not weighed again to find out.
    {
        let cat = Rc::clone(cat);
        let weighed = Rc::clone(weighed);
        let weak = window.as_weak();
        window.on_report_resized(move || {
            let Some(w) = weak.upgrade() else { return };
            let held = weighed.borrow();
            let Some(u) = held.usage.as_ref() else { return };
            report::draw_usage(&w, &cat.borrow().clone(), &held.path, u);
        });
    }

    {
        let weak = window.as_weak();
        let cat = Rc::clone(cat);
        let link = Rc::clone(link);
        window.on_face_picked(move |which_one| {
            let Some(w) = weak.upgrade() else { return };
            open_face(&w, &which_one, &cat.borrow().clone(), &link);
        });
    }

    {
        let state = Rc::clone(state);
        let link = Rc::clone(link);
        let weak = window.as_weak();
        // The fuller report, in the browser, on the same question. Not a switch
        // of face: this window stays, and so does the face somebody prefers.
        {
            let weak = window.as_weak();
            let state = Rc::clone(&state);
            let cat = Rc::clone(cat);
            window.on_report_in_browser(move || {
                let Some(w) = weak.upgrade() else { return };
                let Some(mut command) = scour_launch::face("browser") else {
                    return;
                };
                let query = state.borrow().query.clone();
                command.env(
                    "SCOUR_APP_GOTO",
                    format!("q={}&tab=report", query_encoded(&query)),
                );
                match command.spawn() {
                    Ok(_) => said(&w, t(&cat.borrow(), "starting…")),
                    Err(e) => said(&w, format!("browser: {e}").into()),
                }
            });
        }
        window.on_report_search(move || {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            let scope = state.borrow().scope.clone();
            let query = if scope.is_empty() {
                String::new()
            } else {
                qwire::place_term(&scope)
            };
            w.set_tab("search".into());
            // Set, then tell: the field shows what is searched.
            w.set_query(query.as_str().into());
            w.invoke_query_changed(query.into());
            w.invoke_caret_to_end();
        });
    }

    {
        let state = Rc::clone(state);
        let link = Rc::clone(link);
        let weak = window.as_weak();
        window.on_dupes_toggled(move || {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            let open = !w.get_dupes_open();
            w.set_dupes_open(open);
            if open {
                hunt(&w, &link, &state);
            }
        });
    }
    {
        let state = Rc::clone(state);
        let link = Rc::clone(link);
        let weak = window.as_weak();
        window.on_dupes_floored(move |at| {
            let Some(w) = weak.upgrade() else { return };
            w.set_dupe_floor(at);
            if w.get_dupes_open() {
                hunt(&w, &link, &state);
            }
        });
    }
    {
        let state = Rc::clone(state);
        let link = Rc::clone(link);
        let weak = window.as_weak();
        window.on_dupes_read(move || {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            // Reading whole files off a disk is what turns a candidate into a
            // duplicate, so it is asked for.
            link.send(Ask::Dupes {
                under: state.borrow().scope.clone(),
                min_size: FLOORS[w.get_dupe_floor().clamp(0, 3) as usize].0,
                read_budget: 8 << 30,
            });
        });
    }
}
