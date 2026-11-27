//! The right-click menu.

use super::*;

pub(crate) fn wire(wiring: &Wiring) {
    let Wiring {
        window,
        cat,
        state,
        menu_path,
        rows,
        picks,
        addr,
        link,
        ..
    } = *wiring;
    // What the menu says and which items a row gets is `scour_ui::menu`'s.
    {
        let rows = Rc::clone(rows);
        let picks = Rc::clone(picks);
        let menu_path = Rc::clone(menu_path);
        let weak = window.as_weak();
        let cat = Rc::clone(cat);
        window.on_menu_at(move |i, x, y| {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            // A row of the list, so the selection is what the menu is about.
            menu_path.borrow_mut().take();
            let row_idx = usize::try_from(i).unwrap_or(0);
            let here = rows.what_at(row_idx);
            if let Some((ref path, _, _)) = here {
                let mut held = picks.borrow_mut();
                held.focused = Some(path.clone());
                held.pinned = true;
                held.chasing = 0;
                if !held.rows.contains_key(path)
                    && let Some(pick) = rows.pick_at(row_idx)
                {
                    held.anchor_row = i;
                    held.anchor_path = Some(path.clone());
                    held.rows.clear();
                    held.at.clear();
                    held.at.insert(path.clone(), row_idx);
                    held.rows.insert(path.clone(), pick);
                    show_picks(&w, &cat, &rows, &held, false);
                }
            }
            let picked = picks.borrow().rows.len();
            let is_dir = here.as_ref().is_some_and(|(_, d, _)| *d);
            let (strip, list) = menu_parts(&cat, picked, is_dir);
            if picked > 1 {
                menu_heading_many(&w, &cat, &picks.borrow());
            } else if let Some((path, is_dir, bytes)) = &here {
                menu_heading(&w, path, *is_dir, *bytes);
            }
            w.set_menu_strip(slint::ModelRc::new(slint::VecModel::from(strip)));
            w.set_menu(slint::ModelRc::new(slint::VecModel::from(list)));
            w.set_menu_x(x);
            w.set_menu_y(y);
            w.set_menu_open(true);
        });
    }

    // A button of the preview: the menu's own act, on the list's row — or on
    // the file a content search shows, which is no row of the list.
    {
        let menu_path = Rc::clone(menu_path);
        let weak = window.as_weak();
        window.on_peek_pick(move |id| {
            let Some(w) = weak.upgrade() else { return };
            let file =
                (w.get_grep_active() && !w.get_peek_many()).then(|| w.get_peek_path().to_string());
            *menu_path.borrow_mut() = file;
            w.invoke_menu_pick(id);
        });
    }

    // The same menu over one of the report's heaviest files. It is about that
    // one path, not about the list's selection, which is a different set.
    {
        let menu_path = Rc::clone(menu_path);
        let weak = window.as_weak();
        let cat = Rc::clone(cat);
        window.on_big_menu_at(move |path, x, y| {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            *menu_path.borrow_mut() = Some(path.to_string());
            let (strip, list) = menu_parts(&cat, 1, false);
            let bytes = std::fs::metadata(path.as_str())
                .map(|m| m.len() as i64)
                .unwrap_or(-1);
            menu_heading(&w, &path, false, bytes);
            w.set_menu_strip(slint::ModelRc::new(slint::VecModel::from(strip)));
            w.set_menu(slint::ModelRc::new(slint::VecModel::from(list)));
            w.set_menu_x(x);
            w.set_menu_y(y);
            w.set_menu_open(true);
        });
    }

    // A row pulled out of the window: the selection when the row is in it,
    // otherwise the row alone — what a file manager does.
    {
        let rows = Rc::clone(rows);
        let picks = Rc::clone(picks);
        let weak = window.as_weak();
        window.on_drag_out(move |row, x, y| {
            let Some(w) = weak.upgrade() else { return };
            let row = usize::try_from(row).unwrap_or(0);
            let paths: Vec<String> = {
                let picks = picks.borrow();
                let row_path = rows.path_at(row);
                if picks.rows.len() > 1
                    && row_path
                        .as_deref()
                        .is_some_and(|p| picks.rows.contains_key(p))
                {
                    picks.rows.keys().cloned().collect()
                } else {
                    row_path.into_iter().collect()
                }
            };
            trace(&format!("drag-out row {row}: {} path(s)", paths.len()));
            if !paths.is_empty() {
                drag::files(&w, paths, x, y, |_| {});
            }
        });
    }

    // What a pending question is about. **The paths are taken when the menu is
    // pressed**: a watched index can move a row out from under a selection.
    let pending: Rc<RefCell<Option<(String, Vec<String>)>>> = Rc::new(RefCell::new(None));

    {
        let rows = Rc::clone(rows);
        let picks = Rc::clone(picks);
        let state = Rc::clone(state);
        let link = Rc::clone(link);
        let model = Rc::clone(&rows);
        let weak = window.as_weak();
        let cat = Rc::clone(cat);
        let pending = Rc::clone(&pending);
        let menu_path = Rc::clone(menu_path);
        let addr = addr.clone();
        window.on_menu_pick(move |id| {
            stir(&state, &link);
            let Some(w) = weak.upgrade() else { return };
            // The query field's own menu: edits, not a row.
            if let Some(what) = id.strip_prefix("text:") {
                w.invoke_query_edit(what.into());
                return;
            }
            let cat_now = cat.borrow().clone();
            let here = w.get_selected();
            // A path set aside by the report names one file and nothing else;
            // otherwise the menu is about the row the list has selected.
            let over = menu_path.borrow().clone();
            // The size too, for "find its duplicates": a copy is always among
            // the files of exactly its size. Off the disk for the report's path.
            let (path, is_dir, bytes) = match over {
                Some(one) => {
                    let b = std::fs::metadata(&one).map_or(-1, |m| m.len() as i64);
                    (one, false, b)
                }
                None => match rows.what_at(usize::try_from(here).unwrap_or(0)) {
                    Some((p, dir, b)) => (p, dir, b),
                    None => return,
                },
            };
            let chosen: Vec<String> =
                if menu_path.borrow().is_none() && picks.borrow().rows.len() > 1 {
                    picks.borrow().rows.keys().cloned().collect()
                } else {
                    vec![path.clone()]
                };
            // The status line: `hint` is the empty field's placeholder, unseen while
            // the field holds the query these rows came from.
            let say = |w: &MainWindow, msg: slint::SharedString| told(w, msg);
            let folder = |p: &str| match p.rfind('/') {
                Some(0) => "/".to_string(),
                Some(at) => p[..at].to_string(),
                None => ".".to_string(),
            };
            let leaf = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();

            match id.as_str() {
                "open" => open(&path),
                // The row itself, selected in its folder: opening the folder
                // alone did nothing to one already open, and showed nothing.
                "folder" => scour_openers::reveal(&[scour_core::path::to_path(&path).as_path()]),
                "folders" => {
                    let native: Vec<std::path::PathBuf> = chosen
                        .iter()
                        .map(|p| scour_core::path::to_path(p))
                        .collect();
                    let paths: Vec<&std::path::Path> =
                        native.iter().map(std::path::PathBuf::as_path).collect();
                    scour_openers::reveal(&paths);
                }
                "clear" => {
                    picks.borrow_mut().clear();
                    w.set_selected(-1);
                    show_picks(&w, &cat_now, &model, &picks.borrow(), false);
                }

                // The three that are only possible because there is an index.
                // Into the query, where it can be seen and taken out: in place
                // of a term of the same field, the rest of the query kept.
                "search-here" => {
                    let scope = if is_dir { path.clone() } else { folder(&path) };
                    put_term(&w, &qwire::place_term(&scope));
                }
                "search-kind" => {
                    let name = leaf(&path);
                    match name.rfind('.').filter(|at| *at > 0) {
                        Some(at) => put_term(&w, &format!("ext:{}", name[at + 1..].to_lowercase())),
                        None => say(&w, t(&cat_now, "no extension to search for")),
                    }
                }
                // Every copy of a file is among the files of exactly its size: the
                // list shows them, here, and says so. It used to open the report
                // and ask it nothing about this file (17 November).
                "duplicates" => {
                    if bytes < 0 {
                        say(&w, t(&cat_now, "its size is not known"));
                    } else {
                        w.set_tab("search".into());
                        let q = format!("size:={bytes}");
                        w.set_query(q.as_str().into());
                        w.invoke_query_changed(q.into());
                        say(
                            &w,
                            t(&cat_now, "files the size of {name}: any copy of it is here")
                                .replace("{name}", &leaf(&path))
                                .into(),
                        );
                    }
                }
                "usage" => {
                    w.invoke_query_changed(format!("under:{path}").into());
                    w.set_tab("report".into());
                }
                "skip" => {
                    w.set_panel("settings".into());
                    w.invoke_settings_picked("rules".into());
                    say(&w, leaf(&path).into());
                }

                "copy-path" | "copy-name" => {
                    let text = if id == "copy-name" {
                        chosen
                            .iter()
                            .map(|p| leaf(p))
                            .collect::<Vec<_>>()
                            .join("\n")
                    } else {
                        chosen.join("\n")
                    };
                    match scour_clip::text(&text) {
                        Ok(()) => say(&w, t(&cat_now, "path copied")),
                        // No helper on this machine: the path still goes to stdout.
                        Err(e) => {
                            println!("{text}");
                            say(&w, format!("{e}").into());
                        }
                    }
                }
                "copy-file" => {
                    let native: Vec<std::path::PathBuf> = chosen
                        .iter()
                        .map(|p| scour_core::path::to_path(p))
                        .collect();
                    let paths: Vec<&std::path::Path> =
                        native.iter().map(std::path::PathBuf::as_path).collect();
                    match scour_clip::files(&paths) {
                        Ok(()) => say(&w, t(&cat_now, "path copied")),
                        Err(e) => say(&w, format!("{e}").into()),
                    }
                }
                "details" => w.set_peeking(!w.get_peeking()),
                // The list as the query has it, as the toolbar's export did.
                "csv" => w.invoke_tool_clicked("export".into()),

                "open-with" => {
                    // The menu becomes the list rather than growing a submenu;
                    // the ids carry a prefix so the press comes back knowing.
                    let name = leaf(&path);
                    let mime = scour_thumbs::known::known().mime_of(&name).unwrap_or("");
                    let found = scour_openers::openers(mime);
                    if found.is_empty() {
                        say(&w, t(&cat_now, "nothing on this machine claims it"));
                        return;
                    }
                    let model: Vec<MenuItem> = found
                        .iter()
                        .map(|o| MenuItem {
                            id: format!("open-with:{}", o.id).into(),
                            label: if o.preferred {
                                format!("★ {}", o.name).into()
                            } else {
                                o.name.as_str().into()
                            },
                            key: "".into(),
                            rule: false,
                            careful: false,
                            heavy: false,
                            on: false,
                            off: false,
                            ..Default::default()
                        })
                        .collect();
                    w.set_menu_strip(slint::ModelRc::default());
                    w.set_menu(slint::ModelRc::new(slint::VecModel::from(model)));
                    w.set_menu_open(true);
                }

                "send" => {
                    // The menu becomes the places, as it becomes the programs
                    // for "Open with…"; a drive's name is its own.
                    let found = scour_sendto::targets();
                    if found.is_empty() {
                        say(&w, t(&cat_now, "nowhere to send to on this machine"));
                        return;
                    }
                    let model: Vec<MenuItem> = found
                        .iter()
                        .map(|o| MenuItem {
                            id: format!("send:{}", o.id).into(),
                            label: if o.named {
                                o.label.as_str().into()
                            } else {
                                t(&cat_now, &o.label)
                            },
                            key: "".into(),
                            rule: false,
                            careful: false,
                            heavy: false,
                            on: false,
                            off: false,
                            ..Default::default()
                        })
                        .collect();
                    w.set_menu_strip(slint::ModelRc::default());
                    w.set_menu(slint::ModelRc::new(slint::VecModel::from(model)));
                    w.set_menu_open(true);
                }

                "rename" => {
                    let was = leaf(&path);
                    w.set_ask_title(t(&cat_now, "Rename…"));
                    w.set_ask_body(path.as_str().into());
                    w.set_ask_text(was.as_str().into());
                    w.set_ask_typing(true);
                    w.set_ask_yes(t(&cat_now, "Rename"));
                    w.set_ask_no(t(&cat_now, "Cancel"));
                    *pending.borrow_mut() = Some(("rename".into(), vec![path.clone()]));
                    w.set_ask_open(true);
                }

                // The two that ask first; everything above happens on the press.
                "trash" | "open-all" | "delete" => {
                    let many = chosen.len() > 1;
                    let title = t(
                        &cat_now,
                        match (id.as_str(), many) {
                            ("trash", _) => "Move to trash",
                            ("delete", false) => "Delete permanently…",
                            ("delete", true) => "Delete {n} permanently…",
                            _ => "Open all {n}…",
                        },
                    )
                    .replace("{n}", &grouped(chosen.len() as u64));
                    // A folder's name reads like a file's; it takes its tree with it.
                    let dir = |p: &String| {
                        (*p == path && is_dir)
                            || picks.borrow().rows.get(p).is_some_and(|k| k.is_dir)
                    };
                    // Eight names and a count: a longer list is one nobody reads.
                    let mut body: Vec<String> = chosen
                        .iter()
                        .take(8)
                        .map(|p| if dir(p) { leaf(p) + "/" } else { leaf(p) })
                        .collect();
                    if chosen.len() > 8 {
                        body.push(format!("… +{}", grouped((chosen.len() - 8) as u64)));
                    }
                    // Said before the names: nothing brings these back.
                    if id == "delete" {
                        if chosen.iter().any(dir) {
                            body.insert(
                                0,
                                t(&cat_now, "Folders go with everything in them.").to_string(),
                            );
                        }
                        body.insert(0, t(&cat_now, "This cannot be undone.").to_string());
                    }
                    w.set_ask_title(title.into());
                    w.set_ask_body(body.join("\n").into());
                    w.set_ask_yes(t(
                        &cat_now,
                        match id.as_str() {
                            "trash" => "Move",
                            "delete" => "Delete",
                            _ => "Open",
                        },
                    ));
                    w.set_ask_no(t(&cat_now, "Cancel"));
                    w.set_ask_typing(false);
                    *pending.borrow_mut() = Some((id.to_string(), chosen));
                    w.set_ask_open(true);
                }

                other if other.starts_with("send:") => {
                    let native: Vec<std::path::PathBuf> = chosen
                        .iter()
                        .map(|p| scour_core::path::to_path(p))
                        .collect();
                    let paths: Vec<&std::path::Path> =
                        native.iter().map(std::path::PathBuf::as_path).collect();
                    let said = match scour_sendto::send(&other["send:".len()..], &paths) {
                        Ok(scour_sendto::Sent::Linked(_)) => {
                            t(&cat_now, "a link is on the desktop")
                        }
                        Ok(scour_sendto::Sent::Handed) => t(&cat_now, "handed over"),
                        Ok(scour_sendto::Sent::Packing(at)) => t(&cat_now, "packing into {path}")
                            .replace("{path}", &at.to_string_lossy())
                            .into(),
                        Ok(scour_sendto::Sent::Copying(at)) => t(&cat_now, "copying to {path}")
                            .replace("{path}", &at.to_string_lossy())
                            .into(),
                        Err(e) => t(&cat_now, e.msgid()),
                    };
                    say(&w, said);
                }
                other if other.starts_with("open-with:") => {
                    let wanted = &other["open-with:".len()..];
                    let name = leaf(&path);
                    let mime = scour_thumbs::known::known().mime_of(&name).unwrap_or("");
                    match scour_openers::openers(mime)
                        .into_iter()
                        .find(|o| o.id == wanted)
                    {
                        Some(chosen) => {
                            if let Err(e) =
                                scour_openers::launch(&chosen, &scour_core::path::to_path(&path))
                            {
                                say(&w, format!("{e}").into());
                            }
                        }
                        None => say(&w, t(&cat_now, "nothing on this machine claims it")),
                    }
                }
                _ => say(&w, t(&cat_now, "not in this face yet")),
            }
            let _ = (&state, &link, &addr);
        });
    }

    {
        let link = Rc::clone(link);
        let state = Rc::clone(state);
        let model = Rc::clone(rows);
        let weak = window.as_weak();
        let cat = Rc::clone(cat);
        let pending = Rc::clone(&pending);
        let addr = addr.clone();
        window.on_ask_answer(move |yes| {
            stir(&state, &link);
            let Some((what, paths)) = pending.borrow_mut().take() else {
                return;
            };
            if !yes {
                return;
            }
            let Some(w) = weak.upgrade() else { return };
            let cat_now = cat.borrow().clone();
            if what == "rename" {
                let Some(from) = paths.first() else { return };
                let asked = w.get_ask_text().to_string();
                match scour_name::rename(&scour_core::path::to_path(from), &asked) {
                    Ok(now) => {
                        // Both ends: one path is gone, the other has appeared.
                        recheck(&addr, vec![from.clone(), scour_core::path::from_path(&now)]);
                        let query = state.borrow().query.clone();
                        w.invoke_query_changed(query.into());
                    }
                    Err(why) => told(&w, t(&cat_now, why.msgid())),
                }
                return;
            }
            if what == "open-all" {
                for p in &paths {
                    open(p);
                }
                return;
            }
            if what == "delete" {
                let mut gone = 0usize;
                let mut refused: Option<String> = None;
                for p in &paths {
                    match scour_trash::erase(&scour_core::path::to_path(p)) {
                        Ok(()) => gone += 1,
                        Err(e) => {
                            refused.get_or_insert_with(|| e.to_string());
                        }
                    }
                }
                recheck(&addr, paths);
                told(
                    &w,
                    match refused {
                        Some(why) => why.into(),
                        None => t(&cat_now, "{n} deleted")
                            .replace("{n}", &grouped(gone as u64))
                            .into(),
                    },
                );
                let query = state.borrow().query.clone();
                w.invoke_query_changed(query.into());
                return;
            }
            // The move is this process's; the service is only told to look again.
            let mut gone = 0usize;
            let mut refused: Option<String> = None;
            for p in &paths {
                match scour_trash::trash(&scour_core::path::to_path(p)) {
                    Ok(_) => gone += 1,
                    Err(e) => {
                        refused.get_or_insert_with(|| e.to_string());
                    }
                }
            }
            recheck(&addr, paths);
            told(
                &w,
                match refused {
                    Some(why) => why.into(),
                    None => t(&cat_now, "{n} moved to trash")
                        .replace("{n}", &grouped(gone as u64))
                        .into(),
                },
            );
            let query = state.borrow().query.clone();
            w.invoke_query_changed(query.into());
            let _ = (&link, &model);
        });
    }
}
