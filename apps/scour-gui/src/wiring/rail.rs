//! The rail.

use super::*;

pub(crate) fn wire(wiring: &Wiring) {
    let Wiring {
        window,
        config,
        cat,
        tabs,
        state,
        rows,
        rules,
        table,
        folders,
        folders_off,
        folders_unwatched,
        addr,
        link,
        ..
    } = *wiring;
    // A press writes its term into the text, as the page does: a filter kept
    // apart from the field can be neither seen nor deleted there.
    {
        let weak = window.as_weak();
        window.on_facet_clicked(move |token| {
            let Some(w) = weak.upgrade() else { return };
            let text = scour_ui::query::toggle(&w.get_query(), &token);
            w.set_query(text.as_str().into());
            w.invoke_query_changed(text.into());
            w.invoke_caret_to_end();
        });
    }
    window.on_term_on(|text, token| scour_ui::query::has_term(&text, &token));
    // Through `scour-clip`, as copying a path is: the helper holds the text on
    // Wayland after this window has lost the keyboard.
    {
        let weak = window.as_weak();
        let cat = Rc::clone(cat);
        window.on_copy_span(move |text, from, to| {
            let Some(w) = weak.upgrade() else { return };
            let Some(part) = span_of(&text, from, to) else {
                return;
            };
            match scour_clip::text(part) {
                Ok(()) => told(&w, t(&cat.borrow(), "text copied")),
                Err(e) => told(&w, format!("{e}").into()),
            }
        });
    }
    qwire::wire(window, state, link, cat);
    grep_ui::wire(window, state, link, cat);

    // Two buttons: one writes a file, the other opens the settings. A second
    // press closes them.
    {
        let weak = window.as_weak();
        let addr = addr.clone();
        let cat_for_tools = Rc::clone(cat);
        window.on_tool_clicked(move |what| {
            let Some(w) = weak.upgrade() else { return };
            match what.as_str() {
                "export" => {
                    export(&w, &addr, &cat_for_tools.borrow());
                }
                // What used to be panels of their own are sections of one page:
                // the old names still open it, on their section.
                other => {
                    let section = match other {
                        "faces" | "language" | "general" => "general",
                        "search" => "search",
                        "rules" => "rules",
                        // The folders are on the disks' page.
                        "disks" | "folders" => "disks",
                        "help" | "keys" => "keys",
                        "view" => "view",
                        "index" => "index",
                        "about" => "about",
                        _ => "",
                    };
                    let shown = w.get_panel() == "settings";
                    if shown && (section.is_empty() || w.get_settings_section() == section) {
                        w.set_panel("".into());
                        return;
                    }
                    let section = if section.is_empty() {
                        w.get_settings_section().to_string()
                    } else {
                        section.to_string()
                    };
                    w.set_panel("settings".into());
                    w.invoke_settings_picked(section.into());
                }
            }
        });
    }

    // The query field's right-click menu: cut, copy, paste, all of it, none.
    {
        let weak = window.as_weak();
        let cat = Rc::clone(cat);
        window.on_query_menu_at(move |x, y| {
            trace(&format!("query menu at {x},{y}"));
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let item = |id: &str, msgid: &str, key: &str, rule: bool| MenuItem {
                id: format!("text:{id}").into(),
                label: t(&cat, msgid),
                key: key.into(),
                rule,
                ..Default::default()
            };
            let model = vec![
                item("cut", "Cut", "Ctrl+X", false),
                item("copy", "Copy", "Ctrl+C", false),
                item("paste", "Paste", "Ctrl+V", false),
                item("select-all", "Select all", "Ctrl+A", true),
                item("clear", "Clear", "Esc", false),
            ];
            w.set_menu_strip(ModelRc::default());
            w.set_menu_title("".into());
            w.set_menu_columns(false);
            w.set_menu(ModelRc::new(VecModel::from(model)));
            w.set_menu_x(x);
            w.set_menu_y(y);
            w.set_menu_open(true);
        });
    }

    // The rail and the ribbon, put away or back: kept for every face.
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        window.on_panes_changed(move || {
            let Some(w) = weak.upgrade() else { return };
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    rail_shown: Some(w.get_rail_shown()),
                    ribbon_shown: Some(w.get_ribbon_shown()),
                    ..Default::default()
                },
            });
        });
    }

    // The Index section's three buttons. What they started is said under them;
    // the facts above are asked again as the index moves.
    {
        let weak = window.as_weak();
        window.on_index_open(move || {
            let Some(w) = weak.upgrade() else { return };
            let dir = w.get_index_dir().to_string();
            if !dir.is_empty()
                && let Err(e) = scour_openers::open(std::ffi::OsStr::new(&dir))
            {
                trace(&format!("index: the folder did not open: {e}"));
            }
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let cat = Rc::clone(cat);
        window.on_texts_move({
            let weak = window.as_weak();
            move || {
                let Some(w) = weak.upgrade() else { return };
                let start = w.get_texts_dir().to_string();
                std::thread::spawn(move || {
                    let at = std::path::Path::new(&start);
                    let start = at.parent().filter(|p| p.is_dir());
                    if let Some(chosen) = scour_openers::pick_folder(start) {
                        let _ = slint::invoke_from_event_loop(move || {
                            crate::deliver(Got::TextsDir(chosen));
                        });
                    }
                });
            }
        });
        window.on_texts_forget({
            let link = Rc::clone(&link);
            move || {
                link.send(Ask::ForgetTexts);
                link.send(Ask::Texts);
            }
        });
        window.on_index_tidy(move || {
            let Some(w) = weak.upgrade() else { return };
            link.send(Ask::Reorganize);
            w.set_index_said(t(
                &cat.borrow(),
                "Reorganizing. Searching goes on meanwhile.",
            ));
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let cat = Rc::clone(cat);
        window.on_index_reset(move || {
            let Some(w) = weak.upgrade() else { return };
            link.send(Ask::Reset);
            w.set_index_said(t(
                &cat.borrow(),
                "The index was emptied and every disk is being scanned again.",
            ));
        });
    }

    // A section of the settings page, and what it needs asked for: the
    // lists come from the service, the key from the desktop, each fresh.
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        window.on_settings_picked(move |section| {
            let Some(w) = weak.upgrade() else { return };
            // A section that moved is still asked for by its old name.
            let section: slint::SharedString = match section.as_str() {
                "folders" => "disks".into(),
                _ => section,
            };
            w.set_settings_section(section.clone());
            match section.as_str() {
                "rules" => link.send(Ask::Rules),
                "disks" => link.send(Ask::Volumes),
                "about" => link.send(Ask::Status),
                "index" => link.send(Ask::IndexFacts),
                // The desktop may have been changed in its own settings since
                // this window started; the row must not be stale.
                "keys" => hotkey::work(hotkey::Deed::Look),
                _ => {}
            }
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        window.on_theme_picked(move |theme| {
            if let Some(w) = weak.upgrade() {
                w.global::<Theme>().set_chosen(theme.clone());
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    theme: Some(theme.to_string()),
                    ..Default::default()
                },
            });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        window.on_face_default_picked(move |face| {
            if let Some(w) = weak.upgrade() {
                w.set_face_default(face.clone());
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    face: Some(face.to_string()),
                    ..Default::default()
                },
            });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let state = Rc::clone(state);
        window.on_refresh_picked(move |value| {
            let Some(w) = weak.upgrade() else { return };
            let Ok(seconds) = value.parse::<u32>() else {
                return;
            };
            w.set_refresh_choice(value.clone());
            state.borrow_mut().refresh = std::time::Duration::from_secs(u64::from(seconds));
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    refresh_every: Some(seconds),
                    ..Default::default()
                },
            });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let state = Rc::clone(state);
        window.on_perf_picked(move |kind, value| {
            let Some(w) = weak.upgrade() else { return };
            let mut change = scour_settings::Change::default();
            match kind.as_str() {
                "memory" => {
                    w.set_memory_choice(value.clone());
                    change.memory = Some(value.to_string());
                }
                "narrow" => {
                    let on = value == "on";
                    w.set_narrow_on(on);
                    change.narrow_off = Some(!on);
                    let mut s = state.borrow_mut();
                    s.qb.narrow_off = !on;
                    // Asked again on the next page, for the query in sight.
                    s.qb.narrow_for = "\u{0}".into();
                }
                "threads" => {
                    w.set_threads_choice(value.clone());
                    change.search_threads = value.parse().ok();
                }
                "texts" => {
                    let on = value == "on";
                    w.set_texts_on(on);
                    change.texts_off = Some(!on);
                }
                "texts-ahead" => {
                    let on = value == "on";
                    w.set_texts_ahead_on(on);
                    change.texts_ahead = Some(on);
                }
                _ => {
                    w.set_fresh_choice(value.clone());
                    change.fresh_within = value.parse().ok();
                }
            }
            link.send(Ask::Remember { change });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        window.on_policy_picked(move |kind, value| {
            let Some(w) = weak.upgrade() else { return };
            let mut change = scour_settings::Change::default();
            if kind == "every" {
                w.set_every_choice(value.clone());
                change.folders_every = value.parse().ok();
            } else if kind == "network" {
                w.set_network_choice(value.clone());
                change.network = Some(value.to_string());
            } else if kind == "optical" {
                w.set_optical_choice(value.clone());
                change.optical = Some(value.to_string());
            } else {
                w.set_removable_choice(value.clone());
                change.removable = Some(value.to_string());
            }
            link.send(Ask::Remember { change });
            link.send(Ask::Volumes);
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let folders = Rc::clone(folders);
        let cat = Rc::clone(cat);
        window.on_folder_added(move |typed| {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            // Judged first, on its own line: a borrow in a `match`'s head lives
            // to the end of the match, and the push below then panicked with
            // "already borrowed" on every folder added (17 November).
            let judged = folder_to_add(&typed, &folders.borrow());
            match judged {
                Ok(path) => {
                    folders.borrow_mut().push(path);
                    w.set_folders(strings(&folders.borrow()));
                    w.set_folder_typed("".into());
                    w.set_folder_error("".into());
                    link.send(Ask::Remember {
                        change: scour_settings::Change {
                            folders: Some(folders.borrow().clone()),
                            ..Default::default()
                        },
                    });
                    link.send(Ask::Volumes);
                }
                Err(msgid) => w.set_folder_error(t(&cat, msgid)),
            }
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let folders = Rc::clone(folders);
        {
            let link = Rc::clone(&link);
            window.on_reread(move |from_start| link.send(Ask::Reread { from_start }));
        }
        {
            let link = Rc::clone(&link);
            window.on_rescan_root(move |path| {
                if path.is_empty() {
                    return;
                }
                link.send(Ask::Rescan {
                    path: path.to_string(),
                });
                // The answer to that is the line beside it turning to "walking".
                link.send(Ask::Volumes);
            });
        }
        window.on_folder_dropped(move |path| {
            let Some(w) = weak.upgrade() else { return };
            folders.borrow_mut().retain(|f| f.as_str() != path.as_str());
            w.set_folders(strings(&folders.borrow()));
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    folders: Some(folders.borrow().clone()),
                    ..Default::default()
                },
            });
        });
    }

    // A folder switched on or off, or watched or only walked. The list goes to
    // the service whole, and the folders are asked for again: the answer says
    // what the switch did.
    for watch in [false, true] {
        let link = Rc::clone(link);
        let list = Rc::clone(if watch {
            folders_unwatched
        } else {
            folders_off
        });
        let handler = move |path: slint::SharedString, on: bool| {
            let path = path.to_string();
            {
                let mut l = list.borrow_mut();
                l.retain(|p| p != &path);
                if !on {
                    l.push(path);
                }
            }
            let now = Some(list.borrow().clone());
            let change = if watch {
                scour_settings::Change {
                    folders_unwatched: now,
                    ..Default::default()
                }
            } else {
                scour_settings::Change {
                    folders_off: now,
                    ..Default::default()
                }
            };
            link.send(Ask::Remember { change });
            link.send(Ask::Volumes);
        };
        if watch {
            window.on_folder_watched(handler);
        } else {
            window.on_folder_switched(handler);
        }
    }

    // A disk switched in the panel, or the question answered. The bar goes at
    // once; the list that comes back says what the answer did.
    {
        let link = Rc::clone(link);
        window.on_disk_toggled(move |id, on| {
            link.send(Ask::Disk {
                volume: id.to_string(),
                what: if on { "index" } else { "leave" }.into(),
            });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let state = Rc::clone(state);
        window.on_disk_answered(move |what| {
            let volume = std::mem::take(&mut state.borrow_mut().asked_disk);
            if let Some(w) = weak.upgrade() {
                w.set_disk_question("".into());
            }
            if !volume.is_empty() {
                link.send(Ask::Disk {
                    volume,
                    what: what.to_string(),
                });
            }
        });
    }

    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        window.on_view_clicked(move |mode| {
            let Some(w) = weak.upgrade() else { return };
            w.set_view_mode(mode.clone());
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    layout: Some(mode.to_string()),
                    ..Default::default()
                },
            });
        });
    }

    {
        let link = Rc::clone(link);
        let state = Rc::clone(state);
        let weak = window.as_weak();
        window.on_peek_toggled(move || {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            let on = !w.get_peeking();
            w.set_peeking(on);
            if !on {
                peek::clear(&w);
            }
            // So the next tick asks about whatever is selected.
            state.borrow_mut().peek_path.clear();
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    preview: Some(on),
                    ..Default::default()
                },
            });
        });
    }

    {
        let weak = window.as_weak();
        window.on_peek_page_scrolled(move |idx| {
            if let Some(w) = weak.upgrade() {
                peek::scrolled(&w, idx.max(0) as usize);
            }
        });
    }

    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let table = Rc::clone(table);
        window.on_column_dragged(move |which, delta| {
            let Some(w) = weak.upgrade() else { return };
            // The floor is the column's own; `lay_out` will not go under it either.
            let floor = scour_ui::column(&which).map_or(48.0, |c| c.min as f32);
            let Some(at) = table
                .borrow()
                .shown
                .iter()
                .position(|c| c.id == which.as_str())
            else {
                return;
            };
            let Some(was) = w.get_cw().row_data(at) else {
                return;
            };
            let now = (was + delta).max(floor);
            table
                .borrow_mut()
                .widths
                .insert(which.to_string(), now as u32);
            relayout(&w, &table.borrow());
            // A drag past where the column stops — the others on their floors —
            // keeps what is on screen, not where the pointer went: kept, the
            // overshoot came back as a width nobody saw once the window grew.
            if let Some(shown) = w.get_cw().row_data(at)
                && shown < now
            {
                table
                    .borrow_mut()
                    .widths
                    .insert(which.to_string(), shown as u32);
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    widths: Some(table.borrow().widths.clone()),
                    ..Default::default()
                },
            });
        });
    }

    // The preview's width, kept with the columns' so the page opens at it too.
    {
        let link = Rc::clone(link);
        let table = Rc::clone(table);
        window.on_peek_resized(move |px| {
            if px <= 0 {
                table.borrow_mut().widths.remove("peek");
            } else {
                table.borrow_mut().widths.insert("peek".into(), px as u32);
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    widths: Some(table.borrow().widths.clone()),
                    ..Default::default()
                },
            });
        });
    }

    // **Double-click an edge and that column decides for itself again**: a dragged
    // width otherwise stops stretching with the window for good.
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let table = Rc::clone(table);
        window.on_column_reset(move |which| {
            let Some(w) = weak.upgrade() else { return };
            // Removing it is what "nobody has touched it" means in the settings.
            table.borrow_mut().widths.remove(which.as_str());
            relayout(&w, &table.borrow());
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    widths: Some(table.borrow().widths.clone()),
                    ..Default::default()
                },
            });
        });
    }

    // The ⋮ at the end of the header row uses the right-click menu's own overlay;
    // `menu-columns` says which of the two it is holding.
    {
        let weak = window.as_weak();
        let table = Rc::clone(table);
        let cat = Rc::clone(cat);
        window.on_columns_clicked(move |x, y| {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let table = table.borrow();
            let mut model: Vec<MenuItem> = scour_ui::COLUMNS
                .iter()
                .map(|c| {
                    let on = table.showing(c.id);
                    MenuItem {
                        id: c.id.into(),
                        label: t(&cat, c.msgid),
                        key: "".into(),
                        rule: false,
                        careful: false,
                        heavy: false,
                        on,
                        // The last one standing cannot be turned off.
                        off: on && table.shown.len() == 1,
                        ..Default::default()
                    }
                })
                .collect();
            model.push(MenuItem {
                id: "".into(),
                label: t(&cat, "Back to the default"),
                key: "".into(),
                rule: true,
                careful: false,
                heavy: false,
                on: false,
                off: false,
                ..Default::default()
            });
            w.set_menu_strip(ModelRc::default());
            w.set_menu_title("".into());
            w.set_menu(ModelRc::new(VecModel::from(model)));
            w.set_menu_x(x);
            w.set_menu_y(y);
            w.set_menu_columns(true);
            w.set_menu_open(true);
        });
    }

    {
        let weak = window.as_weak();
        let table = Rc::clone(table);
        let link = Rc::clone(link);
        let state = Rc::clone(state);
        let cat = Rc::clone(cat);
        window.on_column_toggled(move |id| {
            let Some(w) = weak.upgrade() else { return };
            stir(&state, &link);
            {
                let mut t = table.borrow_mut();
                if id.is_empty() {
                    t.shown = COLUMN_DEFAULT
                        .iter()
                        .filter_map(|id| scour_ui::column(id))
                        .collect();
                    // The widths too: a drag is the other half of how a table
                    // was left, and the only one with no other way back.
                    t.widths.retain(|k, _| k == "peek");
                } else {
                    t.toggle(&id);
                }
            }
            w.set_menu_open(false);
            w.set_menu_columns(false);
            set_heads(&w, &table.borrow(), &cat.borrow());
            relayout(&w, &table.borrow());
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    columns: Some(table.borrow().ids()),
                    widths: id.is_empty().then(|| table.borrow().widths.clone()),
                    ..Default::default()
                },
            });
            // And the rows again, because a row is its cells: nothing holds the
            // value of a column that was not shown. Re-running costs 1 ms.
            let query = w.get_query();
            w.invoke_query_changed(query);
        });
    }

    {
        let weak = window.as_weak();
        let table = Rc::clone(table);
        window.on_relayout(move |_| {
            if let Some(w) = weak.upgrade() {
                relayout(&w, &table.borrow());
            }
        });
    }

    // A bar is a filter like a rail row; `dm:38d` is the bar's upper bound.
    {
        let weak = window.as_weak();
        window.on_bar_clicked(move |days| {
            if let Some(w) = weak.upgrade() {
                w.invoke_facet_clicked(format!("dm:{days}d").into());
            }
        });
    }

    // The list of what is off is kept whole, because that is what `Change` carries.
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let state = Rc::clone(state);
        let tick = rules.clone();
        window.on_rule_toggled(move |id| {
            let Some(w) = weak.upgrade() else { return };
            let id = id.to_string();
            let held = {
                let mut s = state.borrow_mut();
                if let Some(at) = s
                    .exclude_off
                    .iter()
                    .position(|o| o.eq_ignore_ascii_case(&id))
                {
                    s.exclude_off.remove(at);
                } else {
                    s.exclude_off.push(id.clone());
                }
                s.sent_off = Some(s.exclude_off.clone());
                s.exclude_off.clone()
            };
            // **The tick moves on the press**: a round trip may be coalesced away,
            // and a tick that does not move is pressed again.
            for model in tick.iter() {
                for at in 0..model.row_count() {
                    let Some(row) = model.row_data(at) else {
                        continue;
                    };
                    if row.id.eq_ignore_ascii_case(&id) {
                        model.set_row_data(
                            at,
                            Rule {
                                off: !row.off,
                                ..row
                            },
                        );
                    }
                }
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    exclude_off: Some(held),
                    ..Default::default()
                },
            });
            // Ask again rather than guessing: the reply is what is in force.
            link.send(Ask::Rules);
            let _ = w;
        });
    }

    // **A rule is added as a name**; one starting with a slash is a path instead.
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let state = Rc::clone(state);
        window.on_rule_added(move |text| {
            let value = text.trim().to_string();
            if value.is_empty() {
                return;
            }
            let change = {
                let mut s = state.borrow_mut();
                if value.starts_with('/') {
                    s.added_paths.push(value);
                    scour_settings::Change {
                        exclude_paths: Some(s.added_paths.clone()),
                        ..Default::default()
                    }
                } else {
                    s.added_dirs.push(value);
                    scour_settings::Change {
                        exclude_dirs: Some(s.added_dirs.clone()),
                        ..Default::default()
                    }
                }
            };
            link.send(Ask::Remember { change });
            link.send(Ask::Rules);
            let _ = weak.upgrade();
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let state = Rc::clone(state);
        window.on_rule_dropped(move |kind, value| {
            let value = value.to_string();
            let change = {
                let mut s = state.borrow_mut();
                let list = match kind.as_str() {
                    "path" => &mut s.added_paths,
                    "file" => &mut s.added_files,
                    _ => &mut s.added_dirs,
                };
                list.retain(|v| v != &value);
                let list = list.clone();
                match kind.as_str() {
                    "path" => scour_settings::Change {
                        exclude_paths: Some(list),
                        ..Default::default()
                    },
                    "file" => scour_settings::Change {
                        exclude_files: Some(list),
                        ..Default::default()
                    },
                    _ => scour_settings::Change {
                        exclude_dirs: Some(list),
                        ..Default::default()
                    },
                }
            };
            link.send(Ask::Remember { change });
            link.send(Ask::Rules);
            let _ = weak.upgrade();
        });
    }

    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let held = Rc::clone(cat);
        let state = Rc::clone(state);
        let model = Rc::clone(rows);
        let configured = config.ui.language.clone();
        let table = Rc::clone(table);
        window.on_language_picked(move |tag| {
            let Some(w) = weak.upgrade() else { return };
            // Empty is the desktop's own: chosen as the config file and the
            // environment say, as at start.
            let speak = scour_i18n::choose(&tag, &configured);
            w.set_language(speak.as_str().into());
            w.set_language_choice(tag.clone());
            // The words change now. What comes from an answer is re-asked for,
            // because it is worded where it arrives.
            *held.borrow_mut() = Rc::new(Catalogue::for_language(&speak));
            words(&w, &held.borrow());
            set_heads(&w, &table.borrow(), &held.borrow());
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    language: Some(tag.to_string()),
                    ..Default::default()
                },
            });
            // A new question, so the sidebar is asked again: the facet walk runs
            // once per matching set.
            state.borrow_mut().advance_query();
            dispatch(&state, &link, &model, w.get_visible_rows().max(0) as u32);
            if w.get_tab() == "report" {
                let scope = state.borrow().scope.clone();
                w.invoke_report_open(scope.as_str().into());
            }
        });
    }

    {
        let state = state.clone();
        let link = link.clone();
        let model = Rc::clone(rows);
        let weak = window.as_weak();
        let tabs = Rc::clone(tabs);
        let cat = Rc::clone(cat);
        window.on_sort_by(move |key| {
            stir(&state, &link);
            let key = key.to_string();
            {
                let mut s = state.borrow_mut();
                if s.sort == key {
                    s.descending = !s.descending;
                } else {
                    s.descending = scour_core::SortKey::named(&key)
                        .unwrap_or_default()
                        .starts_descending();
                    s.sort = key;
                }
                s.advance_order();
            }
            // Read back: `sort` may be unchanged with only the direction flipped.
            if let Some(w) = weak.upgrade() {
                w.set_sorted_by(state.borrow().sort.as_str().into());
                w.set_sorted_down(state.borrow().descending);
            }
            // The tab in front says its order on its name.
            tabs.borrow_mut().hold(now_tab(&state.borrow()));
            if let Some(w) = weak.upgrade() {
                draw_tabs(&w, &tabs.borrow(), &cat.borrow());
            }
            // Kept for every face, as the page keeps it.
            let (sort, descending) = {
                let s = state.borrow();
                (s.sort.clone(), s.descending)
            };
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    sort: Some(sort),
                    descending: Some(descending),
                    ..Default::default()
                },
            });
            let rows = match weak.upgrade() {
                Some(w) => {
                    w.set_busy(true);
                    w.get_visible_rows().max(0) as u32
                }
                None => 60,
            };
            dispatch(&state, &link, &model, rows);
            if let Some(w) = weak.upgrade()
                && state.borrow().grep.open
            {
                grep_ui::trigger(&w, &mut state.borrow_mut(), &link, &cat.borrow(), false);
            }
        });
    }
}
