//! What the service answered, put on screen: the long answers `apply` hands
//! on, each with the window and its models as `apply` holds them.

use super::*;

/// Everything `apply` is handed besides the answer, so an answer's own
/// function sees the same names its arm did.
#[derive(Clone, Copy)]
pub(super) struct Cx<'a> {
    pub w: &'a MainWindow,
    pub state: &'a Rc<RefCell<State>>,
    pub table: &'a Rc<RefCell<Table>>,
    pub rows: &'a Rc<rows::Rows>,
    pub lines: &'a Rc<rows::Lines>,
    pub picks: &'a Rc<RefCell<Selection>>,
    pub facets: &'a Rc<VecModel<Facet>>,
    pub rules: &'a [Rc<VecModel<Rule>>; 3],
    pub cat: &'a Rc<Catalogue>,
    pub link: &'a Rc<Link>,
}

pub(super) fn search(cx: &Cx, got: Got) {
    let Cx {
        w,
        state,
        table,
        rows,
        lines,
        picks,
        cat,
        link,
        ..
    } = *cx;
    let Got::Search {
        generation,
        offset,
        limit,
        reply,
    } = got
    else {
        unreachable!("apply sends only Search here")
    };
    trace(&format!(
        "reply {generation} (shown {}, current {})",
        state.borrow().shown,
        state.borrow().generation
    ));
    {
        let s = state.borrow();
        // Stale: a newer keystroke has already gone out, and showing
        // this would make the list go backwards.
        if generation < s.shown || generation != s.generation {
            return;
        }
    }
    let Response::Search(r) = *reply else {
        w.set_busy(false);
        return;
    };
    // A `content:` query's rows are two lines: the file, and where it
    // holds the text. The height is the theme's, which every scroll
    // and slot reads, so changing it is the whole of the switch.
    let tall = scour_ui::lens::reads(&state.borrow().query);
    if w.get_tall() != tall {
        w.set_tall(tall);
        w.global::<Theme>().set_row_height(if tall {
            scour_ui::METRICS.row + 22.0
        } else {
            scour_ui::METRICS.row
        });
    }
    // What a first page that found nothing nearly found, or nothing.
    if offset == 0 {
        let offered = r.similar.as_ref().filter(|_| r.total == 0);
        w.set_similar_said(match offered {
            Some(similar) => t(cat, "Similar: {n} files —")
                .replace(
                    "{n}",
                    &format!(
                        "{}{}",
                        grouped(similar.total),
                        if similar.capped { "+" } else { "" }
                    ),
                )
                .into(),
            None => slint::SharedString::new(),
        });
        w.set_similar_query(
            offered
                .map(|s| slint::SharedString::from(s.query.as_str()))
                .unwrap_or_default(),
        );
        state.borrow_mut().similar = offered.map(|s| s.query.clone()).unwrap_or_default();
        qwire::searched(w, &mut state.borrow_mut(), link, cat, r.total, r.capped);
        grep_ui::listed(w, &mut state.borrow_mut(), cat, r.total, r.capped);
        qwire::narrow(w, &mut state.borrow_mut(), link, cat, r.total);
    }
    let now = unix_now();
    let terms = terms_of(&state.borrow().query);
    // Read once: a page is two hundred rows and the catalogue is a lookup.
    let frozen_note = t(
        cat,
        "this volume does not record reads (noatime) — the number shown would be left over from when the file was created",
    );
    let page: Vec<Row> = r
        .hits
        .iter()
        // The kind's word comes from the catalogue by the engine's own
        // msgid. Which rows are arrivals is the model's — see `Rows::put`.
        .map(|h| {
            rows::row_of(
                h,
                &terms,
                &rows::Shape {
                    columns: &table.borrow().shown,
                    mounts: &state.borrow().mounts,
                    kind: &t(cat, h.kind.msgid()),
                    frozen_note: &frozen_note,
                    now,
                    system_icons: w.get_system_icons(),
                },
                false,
            )
        })
        .collect();
    // Beside the page rather than on it: Slint counts in 32 bits.
    let weights: Vec<i64> = r.hits.iter().map(|h| h.meta.size.max(0)).collect();
    let n = page.len();
    // A new question's first page decides how the row is divided up;
    // later pages and refreshes leave it, or it moves under the reader.
    if offset == 0 && state.borrow().rewind {
        table
            .borrow_mut()
            .measure(&page, w.get_name_advance(), w.get_cell_advance());
        relayout(w, &table.borrow());
    }
    // **The row being read stays put.** A re-read of the page in sight
    // after the index moved shifts every row below a file that arrived
    // or left above it; the list is moved by the same number of rows,
    // found by the first visible row's path. Not at the top, where what
    // arrives is what somebody is waiting to see, nor in the tiles.
    let is_rewind = state.borrow().rewind && offset == 0;
    let first = w.get_first_row().max(0) as usize;
    let sel_before = w.get_selected();
    let (sel_path, sel_pinned, sel_was_visible) = if is_rewind {
        (None, false, false)
    } else {
        let mut held = picks.borrow_mut();
        if held.focused.is_none() && sel_before >= 0 {
            held.focused = path_of(rows, sel_before);
        }
        let bar = !held.rows.is_empty();
        let pinned = held.pinned || bar;
        let vis = held.chasing > 0 || (sel_before >= 0 && row_in_view(w, sel_before as usize, bar));
        (held.focused.clone(), pinned, vis)
    };
    let anchor = (!is_rewind
        && !sel_pinned
        && !w.get_grid()
        && first > 0
        && (offset as usize..offset as usize + n).contains(&first))
    .then(|| rows.path_at(first))
    .flatten();
    let (any_fresh, total_len) = {
        let mut s = state.borrow_mut();
        // A reading's answer is its own count, and the newest there is:
        // neither the rail nor an earlier count knows how far it has got.
        if r.reading.is_some() {
            s.exact_count = Some(ExactCount {
                query_revision: s.query_revision,
                total: r.total,
                capped: r.capped,
            });
        }
        let counted = s
            .exact_count
            .map(|c| c.total)
            .unwrap_or(r.total)
            .min(i32::MAX as u64) as usize;
        let total = list_length(counted, offset as usize, n, limit as usize, rows.served());
        // Carried by the answer: state may already name a later page.
        s.page_offset = offset;
        let arrived = rows.put(offset as usize / rows::SPAN, page, weights, total);
        // The tiles hold the same rows, and the result may be longer.
        lines.touched(offset as usize, offset as usize + n);
        lines.sync();
        (arrived, total)
    };
    picks.borrow_mut().refresh(rows);

    if let Some(path) = anchor
        && rows.path_at(first).as_deref() != Some(path.as_str())
    {
        let from = offset as usize;
        let moved_to =
            (from..from + n).find(|&row| rows.path_at(row).as_deref() == Some(path.as_str()));
        if let Some(to) = moved_to {
            let by = to as f32 - first as f32;
            let step = w.global::<Theme>().get_row_height();
            w.invoke_scroll_to((w.get_list_down() + by * step).max(0.0));
            let selected = w.get_selected();
            if selected >= 0 {
                w.set_selected((selected as f32 + by).max(0.0) as i32);
            }
            if by > 0.0 {
                w.set_new_above(w.get_new_above() + by as i32);
            }
            trace(&format!("kept {path:?} in place: {by:+} rows"));
        }
    }
    let mut chase_page = None;
    if let Some(ref path) = sel_path {
        if let Some(to) = rows.find_path(path) {
            picks.borrow_mut().chasing = 0;
            if w.get_selected() != to as i32 {
                w.set_selected(to as i32);
            }
            if sel_pinned && sel_was_visible {
                let bar = !picks.borrow().rows.is_empty();
                if let Some(want_down) = scroll_to_keep_row(w, to, bar) {
                    w.invoke_scroll_to(want_down);
                }
            }
        } else {
            if w.get_selected() >= 0
                && path_of(rows, w.get_selected()).as_deref() != Some(path.as_str())
            {
                w.set_selected(-1);
            }
            let next_page = offset as usize / rows::SPAN + 1;
            if sel_pinned
                && sel_was_visible
                && n == limit as usize
                && next_page * rows::SPAN < total_len
                && picks.borrow().chasing < 2
            {
                picks.borrow_mut().chasing += 1;
                chase_page = Some(next_page);
            } else {
                picks.borrow_mut().chasing = 0;
            }
        }
    }
    // Slint's `animate` interpolates when a property *changes*, and the
    // arrival wash is 1.6 s.
    if any_fresh {
        let model: Rc<rows::Rows> = Rc::clone(rows);
        slint::Timer::single_shot(std::time::Duration::from_millis(1600), move || {
            model.clear_fresh()
        });
    }
    if let Some(t) = FIRST.with(std::cell::Cell::take) {
        trace(&format!(
            "first rows on screen {:.1?} after launch",
            t.elapsed()
        ));
    }
    {
        let mut s = state.borrow_mut();
        s.page_cost_us = r.took_us.max(
            s.page_sent
                .map(|t| t.elapsed().as_micros().min(u64::MAX as u128) as u64)
                .unwrap_or(0),
        );
        s.page_landed_at = Some(std::time::Instant::now());
    }
    trace(&format!(
        "page {offset} landed in {:.1} ms round trip, {:.2} ms in the engine, {} rows visited",
        state
            .borrow()
            .page_sent
            .map(|t| t.elapsed().as_secs_f64() * 1000.0)
            .unwrap_or(0.0),
        r.took_us as f64 / 1000.0,
        r.rows_visited,
    ));
    // What was drawn, not how much: a count agrees with the meter without
    // saying the rows are the right rows.
    trace(&format!(
        "first row {:?}",
        r.hits.first().map(|h| h.path.as_str()).unwrap_or("")
    ));
    trace(&format!(
        "drew {n} rows {:.1} ms after the key",
        state
            .borrow()
            .typed_at
            .map(|t| t.elapsed().as_secs_f64() * 1000.0)
            .unwrap_or(0.0)
    ));
    let (query_revision, query, ask_background, exact_count) = {
        let mut s = state.borrow_mut();
        s.shown = generation;
        if !r.capped {
            s.exact_count = Some(ExactCount {
                query_revision: s.query_revision,
                total: r.total,
                capped: false,
            });
        }
        s.hits = r.hits;
        let query_revision = s.query_revision;
        let ask_background = s.start_background();
        (
            query_revision,
            // Without the rail's own filter: the search wants `full_query`.
            facet_query(&s),
            ask_background,
            s.exact_count.filter(|c| c.query_revision == query_revision),
        )
    };
    // **A page landing never moves the viewport. A new question does.**
    // Rows are drawn at their place in the whole result, and the flag is
    // set where the question is asked rather than worked out here.
    let rewind = {
        let mut s = state.borrow_mut();
        // Only the answer asked for from the top rewinds.
        let rewind = s.rewind && offset == 0;
        s.rewind &= !rewind;
        s.search_out &= !rewind;
        rewind
    };
    if rewind {
        w.set_selected(0);
        w.invoke_scroll_to(0.0);
        // A different question, so the old selection answers nothing.
        let mut held = picks.borrow_mut();
        held.clear();
        held.focused = rows.path_at(0);
    }
    // The marks live on the rows, and this page brought new ones.
    if !picks.borrow().rows.is_empty() || w.get_picked() > 0 {
        let asking = w.get_pick_asking() && !picks.borrow().rows.is_empty();
        show_picks(w, cat, rows, &picks.borrow(), asking);
    }
    if w.get_peeking() && picks.borrow().chasing == 0 {
        peek(w, state, link, rows, cat);
    }
    w.set_busy(false);
    // **And straight on to the next**: nothing is asked for while an
    // answer is in flight, so a drag continues here.
    if let Some(page) = chase_page {
        let rev = state.borrow().revision;
        rows.asking(page, rev);
        send_page(state, link, (page * rows::SPAN) as u32, PAGE_MAX);
    } else {
        follow(w, state, link, rows);
    }
    // Now, and only now, the sidebar: it costs about what the search did.
    if ask_background {
        schedule_background(state, link, query_revision, query);
    }
    let (total, capped) = exact_count
        .map(|c| (c.total, c.capped))
        .unwrap_or((r.total, r.capped));
    // The page's sentence, in the page's order: found out of how many
    // there are, what it cost, how much of the index was walked.
    let indexed = state.borrow().indexed;
    meter(
        w,
        format!(
            "{}{} / {}",
            grouped(total),
            if capped { "+" } else { "" },
            grouped(indexed),
        ),
        format!("{:.2} ms", r.took_us as f64 / 1000.0),
        match r.reading {
            Some(reading) => cat
                .get(scour_ui::reading_said(
                    reading.stage == scour_core::ReadingStage::Running,
                    reading.stage == scour_core::ReadingStage::Done,
                ))
                .replace("{n}", &grouped(reading.files)),
            None => format!("{} {}", grouped(r.rows_visited), cat.get("rows read")),
        },
    );
    w.set_reading_stage(match r.reading.map(|reading| reading.stage) {
        None => 0,
        Some(scour_core::ReadingStage::Running) => 1,
        Some(scour_core::ReadingStage::Paused) => 2,
        Some(scour_core::ReadingStage::Done) => 3,
    });
}

pub(super) fn count(cx: &Cx, got: Got) {
    let Cx {
        w,
        state,
        rows,
        cat,
        ..
    } = *cx;
    let Got::Count {
        query_revision,
        reply,
    } = got
    else {
        unreachable!("apply sends only Count here")
    };
    if query_revision != state.borrow().query_revision {
        return;
    }
    if let Response::Count {
        total,
        capped,
        took_us: _,
        misread: _,
    } = *reply
    {
        let indexed = state.borrow().indexed;
        state.borrow_mut().exact_count = Some(ExactCount {
            query_revision,
            total,
            capped,
        });
        // And the list gets as long as the answer: the interactive search
        // counts only to its cap, so the list ran out above the result.
        rows.set_total(total.min(i32::MAX as u64) as usize);
        grep_ui::listed(w, &mut state.borrow_mut(), cat, total, capped);
        // Only the second number moves: a meter that reflows reads as a
        // window changing its mind.
        trace(&format!(
            "meter/search {}{} / {}",
            grouped(total),
            if capped { "+" } else { "" },
            grouped(indexed)
        ));
        w.set_meter_count(
            format!(
                "{}{} / {}",
                grouped(total),
                if capped { "+" } else { "" },
                grouped(indexed),
            )
            .into(),
        );
    }
}

pub(super) fn explain(cx: &Cx, got: Got) {
    let Cx {
        w,
        state,
        cat,
        link,
        ..
    } = *cx;
    let Got::Explain {
        query_revision,
        reply,
    } = got
    else {
        unreachable!("apply sends only Explain here")
    };
    trace(&format!(
        "explain {query_revision} (current {})",
        state.borrow().query_revision
    ));
    if query_revision != state.borrow().query_revision {
        return;
    }
    let Response::Explain {
        spans, description, ..
    } = *reply
    else {
        return;
    };
    let query = state.borrow().query.clone();
    state.borrow_mut().spans = spans;
    qwire::draw(w, &mut state.borrow_mut(), link, cat, qwire::Lensing::Keep);
    qwire::explained(w, &mut state.borrow_mut(), cat);
    // The colours say what a piece is; this says what the whole asks.
    let spans = &state.borrow().spans;
    let warn = spans.iter().any(|sp| sp.role.is_warning());
    let telling = warn
        || spans.iter().any(|sp| sp.role.is_telling())
        || spans.iter().any(|sp| {
            sp.role == scour_core::Role::Text
                && scour_ui::MISTAKEN
                    .iter()
                    .any(|w| sp.of(&query).eq_ignore_ascii_case(w))
        });
    w.set_reading(if telling {
        description.as_str().into()
    } else {
        slint::SharedString::new()
    });
    w.set_reading_warn(warn);
}

pub(super) fn dupes(cx: &Cx, got: Got) {
    let Cx { w, cat, .. } = *cx;
    let Got::Dupes(reply) = got else {
        unreachable!("apply sends only Dupes here")
    };
    let Response::Duplicates {
        groups,
        candidates,
        waste,
        proven,
        unconfirmed,
        ..
    } = *reply
    else {
        return;
    };
    w.set_dupes_sum(
        t(cat, "{proven} confirmed · {waste} candidate · {n} files")
            .replace("{proven}", &compact_bytes(proven))
            .replace("{waste}", &compact_bytes(waste))
            .replace("{n}", &grouped(candidates))
            .into(),
    );
    w.set_dupes_note(
        if unconfirmed == 0 {
            t(cat, "All of them were compared by reading.").to_string()
        } else {
            t(
                cat,
                "Being the same size is not being the same file. {n} groups were not confirmed by reading — those are candidates, not duplicates.",
            )
            .replace("{n}", &grouped(unconfirmed))
        }
        .into(),
    );
    let list: Vec<Dupe> = groups
        .iter()
        .map(|g| Dupe {
            size: compact_bytes(g.size).into(),
            times: format!("×{}  ·  {}", g.paths.len(), compact_bytes(g.waste)).into(),
            certainty: t(
                cat,
                match g.certainty.as_str() {
                    "content" => "identical",
                    "edges" => "same ends",
                    _ => "same size only",
                },
            ),
            proven: g.certainty == "content",
            // Six and a count: twenty-eight copies is a fact about the group.
            paths: {
                let mut lines: Vec<String> = g.paths.iter().take(SHOWN_PATHS).cloned().collect();
                if g.paths.len() > SHOWN_PATHS {
                    lines.push(format!(
                        "… {}",
                        t(cat, "{n} more")
                            .replace("{n}", &grouped((g.paths.len() - SHOWN_PATHS) as u64))
                    ));
                }
                lines.join("\n").into()
            },
        })
        .collect();
    w.set_dupes(ModelRc::new(VecModel::from(list)));
}

pub(super) fn rules(cx: &Cx, got: Got) {
    let Cx { state, rules, .. } = *cx;
    let Got::Rules(reply) = got else {
        unreachable!("apply sends only Rules here")
    };
    let Response::Rules {
        builtin_paths,
        builtin_dirs,
        builtin_files,
        config_paths,
        config_dirs,
        config_files,
        config_allow,
        added_paths,
        added_dirs,
        added_files,
        added_allow,
        off,
    } = *reply
    else {
        return;
    };
    // An answer repeating what was sent means the service has caught up;
    // one that does not is older than this window.
    let off = {
        let mut s = state.borrow_mut();
        match &s.sent_off {
            Some(sent) if sent.len() == off.len() && sent.iter().all(|o| off.contains(o)) => {
                s.sent_off = None;
                off
            }
            Some(sent) => sent.clone(),
            None => off,
        }
    };
    state.borrow_mut().exclude_off = off.clone();
    let is_off = |id: &str| off.iter().any(|o| o.eq_ignore_ascii_case(id));
    // Kept as the answer gave it: deleting one sends the list back short.
    {
        let mut s = state.borrow_mut();
        s.added_paths = added_paths.clone();
        s.added_dirs = added_dirs.clone();
        s.added_files = added_files.clone();
    }
    let group = |rows: Vec<(&str, Vec<String>)>, removable: bool| {
        let mut out: Vec<Rule> = Vec::new();
        for (kind, list) in rows {
            for value in list {
                let id = scour_settings::rule_id(kind, &value);
                out.push(Rule {
                    off: is_off(&id),
                    id: id.as_str().into(),
                    value: value.as_str().into(),
                    kind: kind.into(),
                    removable,
                });
            }
        }
        out
    };
    // One string per list, so a repeated answer is recognised early.
    let mark = |rows: &[(&str, Vec<String>)]| -> String {
        let mut out = String::new();
        for (kind, list) in rows {
            for value in list {
                let id = scour_settings::rule_id(kind, value);
                out.push_str(&id);
                out.push(if is_off(&id) { '-' } else { '+' });
                out.push('\n');
            }
        }
        out
    };
    let added = vec![
        ("path", added_paths),
        ("dir", added_dirs),
        ("file", added_files),
        ("allow", added_allow),
    ];
    let config = vec![
        ("path", config_paths),
        ("dir", config_dirs),
        ("file", config_files),
        ("allow", config_allow),
    ];
    let builtin = vec![
        ("path", builtin_paths),
        ("dir", builtin_dirs),
        ("file", builtin_files),
    ];
    let fresh = [mark(&added), mark(&config), mark(&builtin)];
    let was = state.borrow().rules_shown.clone();
    let next = [
        group(added, true),
        group(config, false),
        group(builtin, false),
    ];
    for ((model, rows), (before, after)) in rules.iter().zip(next).zip(was.iter().zip(fresh.iter()))
    {
        if before == after {
            continue;
        }
        // Same rules, one switched: write only the rows that differ.
        if model.row_count() == rows.len() {
            for (at, row) in rows.into_iter().enumerate() {
                if model.row_data(at).as_ref() != Some(&row) {
                    model.set_row_data(at, row);
                }
            }
        } else {
            model.set_vec(rows);
        }
    }
    state.borrow_mut().rules_shown = fresh;
}

pub(super) fn awake(cx: &Cx, got: Got) {
    let Cx {
        w,
        state,
        rows,
        cat,
        link,
        ..
    } = *cx;
    let Got::Awake(reply) = got else {
        unreachable!("apply sends only Awake here")
    };
    let Response::Status(st) = *reply else { return };
    disks_moved(state, link, st.asking);
    if w.get_panel() == "settings" && w.get_settings_section() == "index" {
        link.send(Ask::IndexFacts);
        link.send(Ask::Texts);
    }
    // Free: a wait is answered with the whole status. The total too,
    // or the meter kept dividing by what the index held at start-up.
    w.set_scanning(scanning_note(cat, &st));
    state.borrow_mut().indexed = st.entries;
    // **A window nobody is using stops following.** Not "open" — *used*;
    // see [`AWAKE_FOR`], and `stir` undoes it within the frame.
    if state.borrow().stirred.elapsed() >= AWAKE_FOR {
        {
            let mut s = state.borrow_mut();
            s.dozing = true;
            s.revision = st.revision;
        }
        rows.mark(st.revision);
        let link = Rc::clone(link);
        let state = Rc::clone(state);
        slint::Timer::single_shot(DOZE_AGAIN, move || {
            let (dozing, since) = {
                let s = state.borrow();
                (s.dozing, s.revision)
            };
            if dozing {
                link.send(Ask::Await { since });
            }
        });
        return;
    }
    {
        let mut s = state.borrow_mut();
        if st.revision == s.revision {
            // The timeout ran out rather than the index moving.
            link.send(Ask::Await { since: s.revision });
            return;
        }
        s.revision = st.revision;
    }
    // **Marked, not thrown away**: the page being looked at is re-read at
    // once, the others when somebody looks at them.
    rows.mark(state.borrow().revision);
    follow(w, state, link, rows);
    // A beat before waiting again: the service answers the instant its
    // index moves, and re-arming at once cost a quarter of a core. The
    // beat is the person's to choose — see `Settings::refresh_every`.
    let rest = AWAIT_AGAIN.max(state.borrow().refresh);
    let link = Rc::clone(link);
    let state = Rc::clone(state);
    slint::Timer::single_shot(rest, move || {
        link.send(Ask::Await {
            since: state.borrow().revision,
        });
    });
}

pub(super) fn status(cx: &Cx, got: Got) {
    let Cx {
        w,
        state,
        cat,
        link,
        ..
    } = *cx;
    let Got::Status(reply) = got else {
        unreachable!("apply sends only Status here")
    };
    let Response::Status(st) = *reply else { return };
    disks_moved(state, link, st.asking);
    // The first revision, and the start of the long poll.
    {
        let mut s = state.borrow_mut();
        s.indexed = st.entries;
        if s.revision == 0 {
            s.revision = st.revision;
            link.send(Ask::Await { since: st.revision });
        }
    }
    let holding = format!(
        "{} {}  ·  {} {}  ·  {} {}",
        t(cat, "index"),
        compact_bytes(st.index_bytes),
        st.sources,
        t(cat, "sources"),
        st.watching,
        t(cat, "watching"),
    );
    w.set_about_body(
        format!(
            "Scour {}\n\n{} {}\n{}",
            env!("CARGO_PKG_VERSION"),
            grouped(st.entries),
            t(cat, "entries"),
            holding
        )
        .into(),
    );
    w.set_holding(holding.into());
}

pub(super) fn places(cx: &Cx, got: Got) {
    let Cx { w, state, cat, .. } = *cx;
    let Got::Places(reply) = got else {
        unreachable!("apply sends only Places here")
    };
    if let Response::Stats(s) = &*reply {
        w.set_index_dir(s.dir.as_str().into());
        let mut facts = format!(
            "{} {} · {}",
            grouped(s.entries),
            t(cat, "entries"),
            compact_bytes(s.bytes_on_disk)
        );
        if s.written_at > 0 {
            facts.push_str(&format!(
                " · {} {}",
                t(cat, "written"),
                scour_ui::format::stamp(s.written_at)
            ));
        }
        w.set_index_facts(facts.into());
        return;
    }
    if let Response::Texts {
        on,
        dir,
        documents,
        no_text,
        failed,
        disk,
        max_mb,
        reader,
        ..
    } = &*reply
    {
        w.set_texts_on(*on);
        w.set_texts_dir(dir.as_str().into());
        let facts = if !reader {
            t(cat, "No document reader is installed.").to_string()
        } else if !on {
            t(cat, "Not kept: every search reads them again.").to_string()
        } else {
            format!(
                "{dir}\n{} {} · {} {} · {} {} · {} / {}",
                grouped(*documents),
                t(cat, "with text"),
                grouped(*no_text),
                t(cat, "without text"),
                grouped(*failed),
                t(cat, "would not open"),
                compact_bytes(*disk),
                compact_bytes(u64::from(*max_mb) << 20),
            )
        };
        w.set_texts_facts(facts.into());
        return;
    }
    if matches!(&*reply, Response::Accepted) {
        return;
    }
    let Response::Places(p) = *reply else {
        trace(&format!("places: unexpected reply {reply:?}"));
        return;
    };
    state.borrow_mut().mounts = p.mounts.clone();
    state.borrow_mut().qb.places = p
        .places
        .iter()
        .map(|pl| (pl.label.clone(), pl.path.clone()))
        .collect();
    let scopes: Vec<Facet> = p
        .places
        .iter()
        .map(|place| Facet {
            label: place.label.as_str().into(),
            token: format!("under:{}", place.path).into(),
            count: slint::SharedString::new(),
            share: 0.0,
            icon: Default::default(),
        })
        .collect();
    w.set_scopes(ModelRc::new(VecModel::from(scopes)));
}

pub(super) fn facets(cx: &Cx, got: Got) {
    let Cx {
        w,
        state,
        rows,
        facets,
        cat,
        link,
        ..
    } = *cx;
    let Got::Facets {
        query_revision,
        half,
        reply,
    } = got
    else {
        unreachable!("apply sends only Facets here")
    };
    if query_revision != state.borrow().query_revision {
        return;
    }
    let Response::Facets(f) = *reply else { return };
    // Two questions, one walk: the reply carries a group per question,
    // read apart by their `by`.
    let kinds: &[scour_core::Facet] = f
        .groups
        .iter()
        .find(|g| matches!(g.by, scour_core::FacetBy::Kind))
        .map(|g| g.facets.as_slice())
        .unwrap_or(&f.facets);
    let ages: &[scour_core::Facet] = f
        .groups
        .iter()
        .find(|g| matches!(g.by, scour_core::FacetBy::Age { .. }))
        .map(|g| g.facets.as_slice())
        .unwrap_or(&[]);

    if half.kinds() {
        // The bar is that kind's share of the largest, ordered by the
        // taxonomy so the rail does not reshuffle under the pointer.
        let top = kinds.iter().map(|x| x.count).max().unwrap_or(1).max(1);
        let mut fresh: Vec<Facet> = Vec::new();
        for k in rows::offered_kinds() {
            let token = k.token();
            let Some(hit) = kinds.iter().find(|x| x.key == token) else {
                continue;
            };
            fresh.push(Facet {
                label: t(cat, k.msgid()),
                // The whole term: the slot holds `dm:38d` too.
                token: format!("kind:{token}").into(),
                count: compact(hit.count).into(),
                share: hit.count as f32 / top as f32,
                icon: token.into(),
            });
        }
        facets.set_vec(fresh);
    }

    if half.ages() {
        // Oldest on the left: `bar_edges()` is newest first — it is the
        // list of upper bounds — and the ribbon reads left to right.
        let edges = scour_ui::bar_edges();
        let count_of = |key: &str| -> i32 {
            ages.iter()
                .find(|x| x.key == key)
                .map(|x| x.count)
                .unwrap_or(0) as i32
        };
        let mut peak = 1i32;
        let mut bars: Vec<Bar> = edges
            .iter()
            .rev()
            .map(|days| {
                let count = count_of(&days.to_string());
                peak = peak.max(count);
                Bar {
                    count,
                    days: *days as i32,
                    band: scour_ui::band_of(*days as f64) as i32,
                    about: String::new().into(),
                }
            })
            .collect();
        // Older than the last edge belongs to the oldest bar: two years
        // is where the scale ends, not the files.
        if let Some(first) = bars.first_mut() {
            // The oldest bar has no upper bound, so it is not a filter.
            first.days = 0;
            first.count += count_of("older");
            peak = peak.max(first.count);
        }
        w.set_bar_peak(peak);
        w.set_bars(ModelRc::new(VecModel::from(bars)));
    }
    // Only when one walk answered both: a stripped query counts wider.
    if half == Half::Both {
        let indexed = state.borrow().indexed;
        let count_query = {
            let mut s = state.borrow_mut();
            if f.capped {
                s.start_count().then(|| full_query(&s))
            } else {
                s.exact_count = Some(ExactCount {
                    query_revision,
                    total: f.total,
                    capped: false,
                });
                None
            }
        };
        // The facet walk counted the whole matching set on its way.
        if !f.capped {
            rows.set_total(f.total.min(i32::MAX as u64) as usize);
        }
        // The same sentence the search reply draws — see there.
        trace(&format!(
            "meter/facets {}{} / {}",
            grouped(f.total),
            if f.capped { "+" } else { "" },
            grouped(indexed)
        ));
        w.set_meter_count(
            format!(
                "{}{} / {}",
                grouped(f.total),
                if f.capped { "+" } else { "" },
                grouped(indexed),
            )
            .into(),
        );
        // Only the facet walk's own safety cap makes a second pass needed.
        if let Some(query) = count_query {
            link.send(Ask::Count {
                query_revision,
                query,
            });
        }
    }
}
