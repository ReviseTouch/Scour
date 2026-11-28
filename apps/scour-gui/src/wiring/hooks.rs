//! `SCOUR_GUI_*`: put the window in a state and send it a synthetic event —
//! the only way to check a click or a picture without a person.

use super::*;

pub(crate) fn wire(wiring: &Wiring) {
    let Wiring {
        window,
        cat,
        rows,
        picks,
        link,
        ..
    } = *wiring;
    if let Ok(scheme) = std::env::var("SCOUR_GUI_SCHEME") {
        window.global::<Theme>().set_chosen(scheme.into());
    }

    // How long the event loop was held, a second at a time: a tick every 2 ms
    // that arrives late was kept waiting by a frame or by a callback. Average
    // frame rates hide the one long frame a person sees as a stutter.
    if std::env::var_os("SCOUR_GUI_STALLS").is_some() {
        let last = std::cell::Cell::new(std::time::Instant::now());
        let gaps = std::cell::RefCell::new(Vec::<f64>::new());
        let since = std::cell::Cell::new(std::time::Instant::now());
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(2),
            move || {
                let now = std::time::Instant::now();
                gaps.borrow_mut()
                    .push(now.duration_since(last.replace(now)).as_secs_f64() * 1e3);
                if since.get().elapsed() >= std::time::Duration::from_secs(1) {
                    since.set(now);
                    let mut g = std::mem::take(&mut *gaps.borrow_mut());
                    g.sort_by(f64::total_cmp);
                    let over = |ms: f64| g.iter().filter(|&&x| x > ms).count();
                    // Busy is what each tick arrived past its 2 ms.
                    let busy: f64 = g.iter().map(|x| (x - 2.0).max(0.0)).sum();
                    eprintln!(
                        "stalls: busy {busy:.0} ms/s · max {:.1} ms · p99 {:.1} · >20 ms {} · >33 ms {} · >50 ms {}",
                        g.last().copied().unwrap_or(0.0),
                        g.get(g.len() * 99 / 100).copied().unwrap_or(0.0),
                        over(20.0),
                        over(33.0),
                        over(50.0),
                    );
                }
            },
        );
    }

    // A wheel turned over the middle of the window, `SCOUR_GUI_WHEEL=px` a
    // notch, one every `SCOUR_GUI_SCROLL_MS`: the events a person sends, so
    // only what a person moves is redrawn — `scroll_to` moves hidden views too.
    // `SCOUR_GUI_WHEEL_AT=x,y` turns it elsewhere, a negative x counted from
    // the right edge (the preview's side), and `SCOUR_GUI_WHEEL_AFTER=ms`
    // waits for what is under it to arrive.
    if let Ok(px) = std::env::var("SCOUR_GUI_WHEEL")
        && let Ok(px) = px.parse::<f32>()
    {
        let every = std::env::var("SCOUR_GUI_SCROLL_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(16);
        let steps = std::env::var("SCOUR_GUI_WHEEL_STEPS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(400usize);
        let at: Option<(f32, f32)> = std::env::var("SCOUR_GUI_WHEEL_AT").ok().and_then(|s| {
            let (x, y) = s.split_once(',')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        });
        let after = std::env::var("SCOUR_GUI_WHEEL_AFTER")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(0u64);
        let weak = window.as_weak();
        let done = std::cell::Cell::new(0usize);
        let start = std::time::Instant::now();
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(every),
            move || {
                let Some(w) = weak.upgrade() else { return };
                if done.get() >= steps || start.elapsed().as_millis() < u128::from(after) {
                    return;
                }
                done.set(done.get() + 1);
                let size = w.window().size().to_logical(w.window().scale_factor());
                let (x, y) = match at {
                    Some((x, y)) if x < 0.0 => (size.width + x, y),
                    Some(xy) => xy,
                    None => (size.width / 2.0, size.height / 2.0),
                };
                w.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: slint::LogicalPosition::new(x, y),
                        delta_x: 0.0,
                        delta_y: -px,
                    });
            },
        );
    }

    // Scroll before the snapshot. A comma-separated list is walked a step at a
    // time: a window that fetches a page per frame only says so while moving.
    if let Ok(spec) = std::env::var("SCOUR_GUI_SCROLL") {
        let stops: Vec<f32> = spec
            .split(',')
            .filter_map(|px| px.trim().parse().ok())
            .collect();
        // How long a stop lasts. A drag is `SCOUR_GUI_SCROLL_MS=16`.
        let every = std::env::var("SCOUR_GUI_SCROLL_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(600);
        let weak = window.as_weak();
        let at = std::cell::Cell::new(0usize);
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(every),
            move || {
                let i = at.get();
                let Some(w) = weak.upgrade() else { return };
                let Some(&px) = stops.get(i) else {
                    trace(&format!("resting at row {}", w.get_first_row()));
                    return;
                };
                at.set(i + 1);
                w.invoke_scroll_to(px);
                trace(&format!("scrolled to {px}px, row {}", w.get_first_row()));
            },
        );
    }

    if let Ok(n) = std::env::var("SCOUR_GUI_PICK")
        && let Ok(n) = n.parse::<usize>()
    {
        let picks = Rc::clone(picks);
        let rows = Rc::clone(rows);
        let cat = Rc::clone(cat);
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1200), move || {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let mut held = picks.borrow_mut();
            for row in 0..n {
                if let Some(pick) = rows.pick_at(row) {
                    held.focused = Some(pick.path.clone());
                    held.pinned = true;
                    held.rows.insert(pick.path.clone(), pick);
                }
            }
            show_picks(&w, &cat, &rows, &held, false);
        });
    }

    if let Ok(text) = std::env::var("SCOUR_GUI_ADD_RULE") {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_rule_added(text.as_str().into());
            }
        });
    }
    if let Ok(spec) = std::env::var("SCOUR_GUI_DROP_RULE")
        && let Some((kind, value)) = spec.split_once(':')
    {
        let weak = window.as_weak();
        let (kind, value) = (kind.to_string(), value.to_string());
        slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_rule_dropped(kind.as_str().into(), value.as_str().into());
            }
        });
    }

    if let Ok(id) = std::env::var("SCOUR_GUI_RULE") {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_rule_toggled(id.as_str().into());
            }
        });
    }

    if let Ok(what) = std::env::var("SCOUR_GUI_TOOL") {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1200), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_tool_clicked(what.as_str().into());
                // `SCOUR_GUI_SECTION=folders`: which page of the settings, for a
                // photograph of one.
                if let Ok(section) = std::env::var("SCOUR_GUI_SECTION") {
                    w.invoke_settings_picked(section.as_str().into());
                }
            }
        });
    }

    if let Ok(tag) = std::env::var("SCOUR_GUI_LANG") {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_language_picked(tag.as_str().into());
            }
        });
    }

    if std::env::var("SCOUR_GUI_TAB").as_deref() == Ok("report") {
        window.set_tab("report".into());
        // The duplicate hunt is the one part of the report nobody runs unasked.
        window.set_dupes_open(std::env::var_os("SCOUR_GUI_DUPES").is_some());
        // Nothing named is everything indexed, which is the scope a press on the
        // tab opens; `SCOUR_GUI_SCOPE` starts further down.
        let scope = std::env::var("SCOUR_GUI_SCOPE").unwrap_or_default();
        window.invoke_report_open(scope.as_str().into());
    }

    if let Ok(px) = std::env::var("SCOUR_GUI_RAIL")
        && let Ok(px) = px.parse::<f32>()
    {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_scroll_rail(px);
            }
        });
    }

    if std::env::var_os("SCOUR_GUI_PEEK").is_some() {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(400), move || {
            if let Some(w) = weak.upgrade()
                && !w.get_peeking()
            {
                w.invoke_peek_toggled();
            }
        });
    }

    // `SCOUR_GUI_RESOURCES=1` opens the status bar's panel; `=index` opens a
    // row's ask as well, and `=sort:go` presses its second step.
    if let Ok(spec) = std::env::var("SCOUR_GUI_RESOURCES") {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
            let Some(w) = weak.upgrade() else { return };
            w.global::<HeldPanel>().invoke_toggle();
            let (token, go) = match spec.split_once(':') {
                Some((token, step)) => (token.to_owned(), step == "go"),
                None => (spec.clone(), false),
            };
            if token == "1" {
                return;
            }
            let weak = w.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(1200), move || {
                let Some(w) = weak.upgrade() else { return };
                w.global::<HeldPanel>()
                    .invoke_act(token.as_str().into(), "ask".into());
                if go {
                    let weak = w.as_weak();
                    slint::Timer::single_shot(std::time::Duration::from_millis(400), move || {
                        if let Some(w) = weak.upgrade() {
                            w.global::<HeldPanel>()
                                .invoke_act(token.as_str().into(), "go".into());
                        }
                    });
                }
            });
        });
    }

    // Text set from Rust leaves the caret at nought.
    if std::env::var_os("SCOUR_GUI_END").is_some() {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(900), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_caret_to_end();
            }
        });
    }

    window.set_trace(std::env::var("SCOUR_TRACE").is_ok());

    // `SCOUR_GUI_KEY=ctrl+a`: chords by commas, modifiers by `+`, a bare word one
    // of Slint's named keys.
    if let Ok(spec) = std::env::var("SCOUR_GUI_KEY") {
        let chords: Vec<String> = spec.split(',').map(|c| c.trim().to_string()).collect();
        let after = std::env::var("SCOUR_GUI_KEY_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(1000);
        let weak = window.as_weak();
        // **One chord a tick**: seven backspaces in one callback are one repaint,
        // and a stale frame between keystrokes is what this has to catch.
        let every = std::env::var("SCOUR_GUI_KEY_EVERY")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(160);
        slint::Timer::single_shot(std::time::Duration::from_millis(after), move || {
            let Some(w) = weak.upgrade() else { return };
            let mut left = chords.clone().into_iter();
            let weak = w.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            t.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(every),
                move || {
                    let Some(w) = weak.upgrade() else { return };
                    let Some(chord) = left.next() else { return };
                    let chords = [chord];
                    for chord in &chords {
                        let mut held: Vec<slint::SharedString> = Vec::new();
                        let mut key = slint::SharedString::new();
                        for part in chord.split('+') {
                            match part.to_ascii_lowercase().as_str() {
                                "ctrl" | "control" => {
                                    held.push(slint::platform::Key::Control.into())
                                }
                                "shift" => held.push(slint::platform::Key::Shift.into()),
                                "alt" => held.push(slint::platform::Key::Alt.into()),
                                "meta" | "super" => held.push(slint::platform::Key::Meta.into()),
                                _ => key = named_key(part),
                            }
                        }
                        trace(&format!("synthetic key {chord}"));
                        for m in &held {
                            w.window()
                                .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                                    text: m.clone(),
                                });
                        }
                        w.window()
                            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                                text: key.clone(),
                            });
                        w.window()
                            .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                                text: key,
                            });
                        for m in held.iter().rev() {
                            w.window()
                                .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                                    text: m.clone(),
                                });
                        }
                    }
                },
            );
        });
    }

    // `SCOUR_GUI_MENU=3` opens the menu on the fourth row: a pointer event cannot
    // be synthesised into a Slint window from outside it.
    if let Some(row) = std::env::var("SCOUR_GUI_MENU")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
    {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1400), move || {
            if let Some(w) = weak.upgrade() {
                w.set_selected(row);
                w.invoke_menu_at(row, 360.0 * 1.0, 300.0 * 1.0);
            }
        });
    }

    if let Ok(mode) = std::env::var("SCOUR_GUI_VIEW") {
        window.set_view_mode(mode.as_str().into());
    }

    // Press a rail row before the window opens — the filter slot, not the text.
    if let Ok(term) = std::env::var("SCOUR_GUI_FACET") {
        let weak = window.as_weak();
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(120),
            move || {
                if let Some(w) = weak.upgrade() {
                    w.invoke_facet_clicked(term.as_str().into());
                }
            },
        );
    }

    if let Ok(which) = std::env::var("SCOUR_GUI_PANEL") {
        if which == "columns" {
            // Where the ⋮ is, roughly: nothing has been laid out yet.
            window.invoke_columns_clicked(1500.0, 150.0);
        }
        // Press it, do not set it: setting first makes the press read as a second.
        window.invoke_tool_clicked(which.as_str().into());
    }

    if let Ok(q) = std::env::var("SCOUR_GUI_QUERY") {
        // Set, then tell once: both produced "rapor", "erapor", "emrapor".
        window.set_query(q.as_str().into());
        let weak = window.as_weak();
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(80),
            move || {
                if let Some(w) = weak.upgrade() {
                    w.invoke_query_changed(w.get_query());
                }
            },
        );
    }

    // `SCOUR_GUI_MOVE=280,69`: the pointer rests there, nothing pressed: for the
    // look of what appears under it. Logical pixels.
    if let Ok(spec) = std::env::var("SCOUR_GUI_MOVE") {
        let p: Vec<f32> = spec
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        if let [x, y] = p[..] {
            let weak = window.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            t.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(2500),
                move || {
                    if let Some(w) = weak.upgrade() {
                        w.window()
                            .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                                position: slint::LogicalPosition::new(x, y),
                            });
                    }
                },
            );
        }
    }

    // `SCOUR_GUI_CLICK=900,146`: the only way to ask whether the window's hit test
    // agrees with what it drew. Logical pixels.
    if let Ok(spec) = std::env::var("SCOUR_GUI_CLICK") {
        let point: Vec<f32> = spec
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        if let [x, first, last, step] = point[..] {
            // A walk of presses down a column: `x,first,last,step`.
            let weak = window.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            let mut y = first;
            t.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(500),
                move || {
                    let Some(w) = weak.upgrade() else { return };
                    if y > last {
                        return;
                    }
                    let at = slint::LogicalPosition::new(x, y);
                    trace(&format!("press at {x},{y}"));
                    for e in [
                        slint::platform::WindowEvent::PointerMoved { position: at },
                        slint::platform::WindowEvent::PointerPressed {
                            position: at,
                            button: slint::platform::PointerEventButton::Left,
                        },
                        slint::platform::WindowEvent::PointerReleased {
                            position: at,
                            button: slint::platform::PointerEventButton::Left,
                        },
                    ] {
                        w.window().dispatch_event(e);
                    }
                    y += step;
                },
            );
        } else if let [x, y, n] = point[..] {
            // `x,y,n`: n clicks in one place, at once — `2` is a double-click.
            let weak = window.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            let after = std::env::var("SCOUR_GUI_CLICK_MS")
                .ok()
                .and_then(|ms| ms.parse().ok())
                .unwrap_or(2500);
            t.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(after),
                move || {
                    let Some(w) = weak.upgrade() else { return };
                    let at = slint::LogicalPosition::new(x, y);
                    trace(&format!("synthetic {n} clicks at {x},{y}"));
                    w.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                            position: at,
                        });
                    let button = slint::platform::PointerEventButton::Left;
                    for _ in 0..n as u32 {
                        w.window()
                            .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                                position: at,
                                button,
                            });
                        w.window()
                            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                                position: at,
                                button,
                            });
                    }
                },
            );
        } else if let [x, y] = point[..] {
            let weak = window.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            let after = std::env::var("SCOUR_GUI_CLICK_MS")
                .ok()
                .and_then(|ms| ms.parse().ok())
                .unwrap_or(2500);
            t.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(after),
                move || {
                    let Some(w) = weak.upgrade() else { return };
                    let at = slint::LogicalPosition::new(x, y);
                    trace(&format!("synthetic click at {x},{y}"));
                    w.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                            position: at,
                        });
                    w.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                            position: at,
                            button: slint::platform::PointerEventButton::Left,
                        });
                    // Released a moment later: one tick is not what a hand does.
                    let weak = w.as_weak();
                    let r = Box::leak(Box::new(slint::Timer::default()));
                    r.start(
                        slint::TimerMode::SingleShot,
                        std::time::Duration::from_millis(90),
                        move || {
                            let Some(w) = weak.upgrade() else { return };
                            w.window().dispatch_event(
                                slint::platform::WindowEvent::PointerReleased {
                                    position: at,
                                    button: slint::platform::PointerEventButton::Left,
                                },
                            );
                        },
                    );
                },
            );
        }
    }

    // `SCOUR_GUI_SIDEWAYS=x,y,px`: after `SCOUR_GUI_CLICK_MS`, one sideways
    // wheel step of px at x,y — a touchpad pushing a panel's content across.
    if let Ok(spec) = std::env::var("SCOUR_GUI_SIDEWAYS") {
        let n: Vec<f32> = spec
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .collect();
        if let [x, y, px] = n[..] {
            let after = std::env::var("SCOUR_GUI_CLICK_MS")
                .ok()
                .and_then(|ms| ms.parse().ok())
                .unwrap_or(2500);
            let weak = window.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            t.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(after),
                move || {
                    let Some(w) = weak.upgrade() else { return };
                    let position = slint::LogicalPosition::new(x, y);
                    w.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
                    w.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                            position,
                            delta_x: -px,
                            delta_y: 0.0,
                        });
                    trace(&format!("sideways {px} at {x},{y}"));
                },
            );
        }
    }

    // `SCOUR_GUI_DRAG=x1,y,x2`: after `SCOUR_GUI_CLICK_MS`, press at x1, move
    // to x2 in 10px steps a frame apart, let go — a hand on a column's edge.
    if let Ok(spec) = std::env::var("SCOUR_GUI_DRAG") {
        let n: Vec<f32> = spec
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .collect();
        if let [from, y, to] = n[..] {
            let after: u64 = std::env::var("SCOUR_GUI_CLICK_MS")
                .ok()
                .and_then(|ms| ms.parse().ok())
                .unwrap_or(2500);
            let weak = window.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            let mut tick = 0u64;
            let mut x = from;
            let mut done = false;
            let button = slint::platform::PointerEventButton::Left;
            t.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(16),
                move || {
                    tick += 1;
                    let Some(w) = weak.upgrade() else { return };
                    if done || tick * 16 < after {
                        return;
                    }
                    let win = w.window();
                    let at = |x: f32| slint::LogicalPosition::new(x, y);
                    if tick * 16 < after + 16 {
                        win.dispatch_event(slint::platform::WindowEvent::PointerMoved {
                            position: at(x),
                        });
                        win.dispatch_event(slint::platform::WindowEvent::PointerPressed {
                            position: at(x),
                            button,
                        });
                        trace(&format!("drag from {x},{y}"));
                        return;
                    }
                    x = if (to - x).abs() <= 10.0 {
                        to
                    } else {
                        x + 10.0f32.copysign(to - x)
                    };
                    win.dispatch_event(slint::platform::WindowEvent::PointerMoved {
                        position: at(x),
                    });
                    if x == to {
                        win.dispatch_event(slint::platform::WindowEvent::PointerReleased {
                            position: at(x),
                            button,
                        });
                        trace(&format!("drag let go at {x},{y}"));
                        done = true;
                    }
                },
            );
        }
    }

    // `SCOUR_GUI_HOVER=900:120,160,200` prints which row each stop lands on.
    if let Ok(spec) = std::env::var("SCOUR_GUI_HOVER")
        && let Some((x, ys)) = spec.split_once(':')
    {
        let x: f32 = x.trim().parse().unwrap_or(0.0);
        let stops: Vec<f32> = ys
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        let weak = window.as_weak();
        let t = Box::leak(Box::new(slint::Timer::default()));
        let mut left = stops.into_iter();
        let mut first = true;
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(400),
            move || {
                if first {
                    first = false;
                    return;
                }
                let Some(w) = weak.upgrade() else { return };
                let Some(y) = left.next() else { return };
                trace(&format!("pointer to {x},{y}"));
                w.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                        position: slint::LogicalPosition::new(x, y),
                    });
            },
        );
    }

    // `SCOUR_GUI_RESIZE=1240,820` — resize from the inside, because there is
    // no hand to drag the corner in a test. `1240,820,1500` says when.
    if let Ok(spec) = std::env::var("SCOUR_GUI_RESIZE") {
        let at: Vec<f32> = spec
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        if let [w, h, ..] = at[..] {
            let after = at.get(2).copied().unwrap_or(1200.0) as u64;
            let weak = window.as_weak();
            let t = Box::leak(Box::new(slint::Timer::default()));
            t.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(after),
                move || {
                    let Some(win) = weak.upgrade() else { return };
                    trace(&format!("resizing to {w}x{h}"));
                    win.window().set_size(slint::LogicalSize::new(w, h));
                },
            );
        }
    }

    // For a photograph of the time lens's calendar: the lens opened on the
    // query's term, the calendar shown, `SCOUR_GUI_CAL` days ago picked first
    // and three days ago under the pointer.
    if let Some(back) = std::env::var("SCOUR_GUI_CAL")
        .ok()
        .and_then(|d| d.parse::<i32>().ok())
    {
        let weak = window.as_weak();
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(1200),
            move || {
                let Some(w) = weak.upgrade() else { return };
                w.invoke_lens_field("dm".into());
                w.set_cal_open(true);
                w.invoke_cal_opened();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                let today = scour_ui::calendar::today(now).0 as i32;
                w.set_cal_from(today - back);
                w.set_cal_hover(today - 3);
            },
        );
    }

    // Photograph the window and leave, when asked. See [`snapshot`].
    if let Ok(path) = std::env::var("SCOUR_GUI_SNAP") {
        let weak = window.as_weak();
        // Held rather than dropped: a `Timer` that goes out of scope never fires.
        let t = Box::leak(Box::new(slint::Timer::default()));
        let after = std::env::var("SCOUR_GUI_SNAP_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(2500);
        t.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(after),
            move || {
                if let Some(w) = weak.upgrade() {
                    snapshot(&w, &path);
                }
                slint::quit_event_loop().ok();
            },
        );
    }
    // Ctrl and + or - make everything bigger or smaller, Ctrl+0 puts it back.
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        window.on_zoom_step(move |step| {
            let Some(w) = weak.upgrade() else { return };
            let now = ZOOM.with(std::cell::Cell::get);
            let next = match step {
                0 => default_zoom(),
                s => ((now + 0.1 * s as f32) * 20.0).round() / 20.0,
            };
            set_zoom(&w, &link, next);
        });
    }
    // The same, from the settings' row.
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        window.on_zoom_picked(move |v| {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(next) = v.parse::<f32>() {
                set_zoom(&w, &link, next);
            }
        });
    }
    // The first frame is drawn at the desktop's scale; the zoom follows at once.
    {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            if let Some(w) = weak.upgrade() {
                apply_zoom(&w);
                // A scale change takes the focus with it.
                w.invoke_focus_query();
                drag::prepare(&w);
                frame::let_go_on_the_frame(w.window());
            }
        });
    }
}
