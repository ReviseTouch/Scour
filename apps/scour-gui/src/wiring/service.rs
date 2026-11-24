//! The switch between the session and the Windows service, and the service's own panels.

use super::*;

pub(crate) fn wire(wiring: &Wiring) {
    let Wiring {
        window,
        config,
        kept,
        cat,
        state,
        weighed,
        rows,
        picks,
        lines,
        facets,
        rules,
        table,
        link,
        ..
    } = *wiring;
    let weak = window.as_weak();
    let ui_state = state.clone();
    let ui_table = Rc::clone(table);
    let ui_rows = rows.clone();
    let ui_lines = lines.clone();
    let ui_picks = Rc::clone(picks);
    let ui_facets = facets.clone();
    let ui_rules = rules.clone();
    let ui_cat = Rc::clone(cat);
    let ui_weighed = Rc::clone(weighed);

    // The move between the session and the Windows service. Off this thread:
    // the consent dialog and the copying take seconds. On success the next
    // window is already open, and this one leaves.
    {
        let weak = window.as_weak();
        let config = config.clone();
        window.on_service_switch(move || {
            let Some(w) = weak.upgrade() else { return };
            w.set_service_busy(true);
            let to_service = !service_mode::active();
            let config = config.clone();
            let weak = w.as_weak();
            std::thread::spawn(move || {
                let done = service_mode::switch(to_service, &config);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_service_busy(false);
                    }
                    match done {
                        Ok(()) => std::process::exit(0),
                        Err(e) => {
                            let _ = rfd::MessageDialog::new()
                                .set_title("Scour")
                                .set_description(e)
                                .set_level(rfd::MessageLevel::Error)
                                .show();
                        }
                    }
                });
            });
        });
    }

    // Starting with Windows without the service: the account's Run key.
    {
        let weak = window.as_weak();
        window.on_autostart_picked(move |on| {
            let _ = autostart::set(on);
            if let Some(w) = weak.upgrade() {
                w.set_autostart_on(autostart::is_on());
            }
        });
    }
    // Run as the service, the session's leftovers go: the per-user copy of the
    // programs and the Run value that started it. Off the window's thread.
    #[cfg(windows)]
    if service_mode::active() {
        std::thread::spawn(service_mode::tidy);
    }

    // The tabs switched on or off; the tabs themselves are kept either way.
    {
        let link = Rc::clone(link);
        let weak = window.as_weak();
        window.on_tabs_picked(move |on| {
            if let Some(w) = weak.upgrade() {
                w.set_tabs_on(on);
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    tabs_off: Some(!on),
                    ..Default::default()
                },
            });
        });
    }

    // Lines between the columns, the same way.
    {
        let link = Rc::clone(link);
        let weak = window.as_weak();
        window.on_grid_columns_picked(move |on| {
            if let Some(w) = weak.upgrade() {
                w.set_grid_columns(on);
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    grid_columns: Some(on),
                    ..Default::default()
                },
            });
        });
    }

    // Lines in the list, from the settings page; kept for every face.
    {
        let link = Rc::clone(link);
        let weak = window.as_weak();
        window.on_grid_picked(move |on| {
            if let Some(w) = weak.upgrade() {
                w.set_grid_lines(on);
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    grid_lines: Some(on),
                    ..Default::default()
                },
            });
        });
    }

    {
        let link = Rc::clone(link);
        let state = Rc::clone(state);
        let model = Rc::clone(rows);
        let weak = window.as_weak();
        window.on_system_icons_picked(move |on| {
            if let Some(w) = weak.upgrade() {
                w.set_system_icons(on);
                if on {
                    state.borrow_mut().advance_order();
                    dispatch(&state, &link, &model, w.get_visible_rows().max(0) as u32);
                }
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    system_icons: Some(on),
                    ..Default::default()
                },
            });
        });
    }

    {
        let link = Rc::clone(link);
        INBOX.with(|slot| {
            *slot.borrow_mut() = Some(Rc::new(move |got: Got| {
                let Some(w) = weak.upgrade() else { return };
                apply(
                    &w,
                    &ui_state,
                    &ui_table,
                    &ui_rows,
                    &ui_lines,
                    &ui_picks,
                    &ui_facets,
                    &ui_rules,
                    &ui_cat.borrow().clone(),
                    &link,
                    &ui_weighed,
                    got,
                );
            }));
        });
    }

    // The desktop's key: asked for at once, on a thread of its own, because
    // every answer here is a process this window must not wait for.
    hotkey::start(kept.key_hint_seen);
    hotkey::wire(window, link, cat);
}
