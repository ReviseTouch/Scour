//! One table: what every key does, in both modes.
//!
//! **The help screen will be printed from this**, so that the documentation
//! cannot drift from the behaviour — the two have to be the same list or one
//! of them is a lie.
//!
//! The rule the modes follow: **a mode never quietly changes what a key
//! means.** `Enter`, `Tab`, the arrows, anything with `Ctrl`, and the mouse do
//! the same thing in both. What a mode decides is only where the bare letters
//! go — into the query, or into moving.

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::{App, Mode, Panel, Spot, Want};

/// Every key, and what it does. **The help screen is printed from this**, so
/// that what is documented and what happens cannot drift apart.
pub const MAP: &[(&str, &str)] = &[
    ("type", "search"),
    ("↑ ↓ · PgUp PgDn · Home End", "move"),
    ("Enter", "open"),
    ("Shift+Enter", "open the folder"),
    ("Space", "pick · Shift+↑↓ for a run"),
    ("Ctrl+A", "pick nothing"),
    ("Ctrl+← →", "sort by the next column · or click a heading"),
    ("Ctrl+↑ ↓", "reverse the order"),
    ("Tab", "the rail, and back"),
    ("Ctrl+K", "what is skipped"),
    ("Ctrl+L", "language"),
    ("Ctrl+U", "which face to run"),
    ("Ctrl+E", "write the result as a spreadsheet"),
    ("F1", "this"),
    ("Esc", "clear the query, then move mode"),
    ("j k · g G · d u", "move, in move mode"),
    ("i · /", "back to typing"),
    ("Ctrl+C · Ctrl+Q", "leave"),
];

/// What a key does. Returns what to ask the service for, if anything.
pub fn press(app: &mut App, key: KeyEvent) -> Want {
    // Windows sends a key twice — down and up — and a terminal that acted on
    // both would type every letter twice.
    if key.kind == KeyEventKind::Release {
        return Want::Nothing;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let page = app.room.max(1) as isize;

    // The help is a panel over everything, and any key at all closes it: a
    // panel somebody has read is a panel in the way.
    if app.helping {
        app.helping = false;
        app.dirty = true;
        return Want::Nothing;
    }

    // A panel takes the arrows and `Enter` while it is open, and nothing else
    // about the keyboard changes: the query still types, `Ctrl+C` still leaves.
    if app.panel != Panel::None {
        match key.code {
            KeyCode::Esc => {
                app.show(app.panel);
                return Want::Nothing;
            }
            KeyCode::Up => {
                app.panel_walk(-1);
                return Want::Nothing;
            }
            KeyCode::Down => {
                app.panel_walk(1);
                return Want::Nothing;
            }
            KeyCode::Enter | KeyCode::Char(' ') => return panel_press(app),
            _ => {}
        }
    }

    // The keys that mean the same thing in both modes, first — so that nothing
    // below can shadow them.
    match key.code {
        KeyCode::Char('c' | 'q') if ctrl => {
            app.leaving = true;
            return Want::Leave;
        }
        KeyCode::Char('a') if ctrl => {
            app.unpick();
            return Want::Nothing;
        }
        KeyCode::F(1) => {
            app.helping = true;
            app.dirty = true;
            return Want::Nothing;
        }
        KeyCode::Char('k') if ctrl => {
            app.show(Panel::Rules);
            // Asked when it opens rather than kept fresh: the rules change
            // when somebody changes them, and this is the thing changing them.
            return Want::Rules;
        }
        KeyCode::Char('l') if ctrl => {
            app.show(Panel::Language);
            return Want::Nothing;
        }
        KeyCode::Char('u') if ctrl => {
            app.show(Panel::Faces);
            return Want::Nothing;
        }
        KeyCode::Char('e') if ctrl => return app.write_sheet(),
        // Sorting: left and right along the columns, up and down for the
        // direction. `Ctrl` because the bare arrows move and always will.
        KeyCode::Left if ctrl => return app.resort(-1),
        KeyCode::Right if ctrl => return app.resort(1),
        KeyCode::Up | KeyCode::Down if ctrl => return app.flip(),
        KeyCode::Up if shift => {
            let to = app.cursor.saturating_sub(1);
            return app.pick_to(to);
        }
        KeyCode::Down if shift => {
            let to = app.cursor + 1;
            return app.pick_to(to);
        }
        // **`Tab` decides where the arrows go**, and the footer says which.
        KeyCode::Tab | KeyCode::BackTab => {
            app.in_rail = !app.in_rail && app.rail;
            app.dirty = true;
            return Want::Nothing;
        }
        KeyCode::Up if app.in_rail => {
            app.rail_walk(-1);
            return Want::Nothing;
        }
        KeyCode::Down if app.in_rail => {
            app.rail_walk(1);
            return Want::Nothing;
        }
        KeyCode::Up => return app.walk(-1),
        KeyCode::Down => return app.walk(1),
        KeyCode::PageUp => return app.walk(-page),
        KeyCode::PageDown => return app.walk(page),
        KeyCode::Home => return app.go(0),
        KeyCode::End => return app.go(usize::MAX),
        // In the rail, `Enter` presses the filter under the cursor; in the
        // list it opens what is under it. One key, two places, and the cursor
        // says which — the same rule the arrows follow.
        KeyCode::Enter if app.in_rail => return app.rail_press(),
        KeyCode::Enter => return open(app, shift),
        _ => {}
    }

    match app.mode {
        Mode::Search => match key.code {
            // Space picks only when there is nothing to type into; in search
            // mode a space is a space, which is how two terms are separated.
            KeyCode::Char(c) if !ctrl => app.insert(c),
            KeyCode::Backspace => app.backspace(),
            KeyCode::Left => {
                app.caret = app.query[..app.caret]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                app.dirty = true;
                Want::Nothing
            }
            KeyCode::Right => {
                if let Some(c) = app.query[app.caret..].chars().next() {
                    app.caret += c.len_utf8();
                    app.dirty = true;
                }
                Want::Nothing
            }
            // **`Esc` empties the query before it changes the mode.** Somebody
            // pressing it is nearly always saying "not that" about what they
            // typed; moving them into another mode instead would answer a
            // question they did not ask.
            KeyCode::Esc => {
                if app.query.is_empty() {
                    app.mode = Mode::Move;
                    app.dirty = true;
                    Want::Nothing
                } else {
                    app.query.clear();
                    app.caret = 0;
                    app.typed()
                }
            }
            _ => Want::Nothing,
        },
        Mode::Move => match key.code {
            KeyCode::Char(' ') => app.pick(),
            KeyCode::Char('j') => app.walk(1),
            KeyCode::Char('k') => app.walk(-1),
            KeyCode::Char('d') => app.walk(page / 2),
            KeyCode::Char('u') => app.walk(-page / 2),
            KeyCode::Char('g') => app.go(0),
            KeyCode::Char('G') => app.go(usize::MAX),
            KeyCode::Char('q') => {
                app.leaving = true;
                Want::Leave
            }
            // Back to typing, the two ways every editor offers.
            KeyCode::Char('i') | KeyCode::Char('/') => {
                app.mode = Mode::Search;
                app.dirty = true;
                Want::Nothing
            }
            KeyCode::Esc => {
                app.mode = Mode::Search;
                app.dirty = true;
                Want::Nothing
            }
            _ => Want::Nothing,
        },
    }
}

/// Press whatever the open panel's cursor is on.
fn panel_press(app: &mut App) -> Want {
    match app.panel {
        Panel::Rules => match app.toggle_rule() {
            Some(off) => Want::OffRules(off),
            None => Want::Nothing,
        },
        Panel::Language => app.speak(app.panel_at),
        Panel::Faces => app.run_face(app.panel_at),
        Panel::None => Want::Nothing,
    }
}

/// What the mouse does: the wheel moves whatever is under it, a press acts on
/// wherever it landed.
///
/// **Where it landed decides what it means**, which is the rule `Tab` follows
/// on the keyboard: in the rail a press presses a filter, in the list it puts
/// the cursor on a row, on the strip it narrows to that band of time.
///
/// The geometry has to agree with `draw`, and there is no way round that — a
/// terminal reports a row and a column and says nothing about what is drawn
/// there. It is kept to the three numbers here rather than spread about.
pub fn mouse(app: &mut App, m: MouseEvent, size: (u16, u16)) -> Want {
    let spot = spot_at(app, m.column, m.row, size);
    // **Everything the pointer passes over answers to it.** A terminal draws
    // no hover of its own, so this is the whole of it: what is under the
    // pointer is remembered, and the drawing lights it.
    if app.hover != spot {
        app.hover = spot;
        app.dirty = true;
    }
    match m.kind {
        MouseEventKind::Moved => Want::Nothing,
        // **Dragging the thumb is the one thing a press cannot be**: it goes
        // on after the button is down and has to be followed all the way, not
        // acted on when it is let go.
        MouseEventKind::Drag(MouseButton::Left) => match app.pressed {
            Spot::Bar(_) => {
                let (_, height) = size;
                let strip_high: u16 = if height >= 20 && !app.strip.is_empty() {
                    3
                } else {
                    0
                };
                let list_to = height.saturating_sub(1 + strip_high);
                let high = list_to.saturating_sub(crate::draw::LIST_TOP + 1);
                let at = m.row.saturating_sub(crate::draw::LIST_TOP).min(high);
                app.drag_bar(at, high)
            }
            _ => Want::Nothing,
        },
        MouseEventKind::ScrollDown => match spot {
            Spot::Rail(_) => {
                app.rail_walk(1);
                Want::Nothing
            }
            Spot::Panel(_) => {
                app.panel_walk(1);
                Want::Nothing
            }
            _ => app.walk(3),
        },
        MouseEventKind::ScrollUp => match spot {
            Spot::Rail(_) => {
                app.rail_walk(-1);
                Want::Nothing
            }
            Spot::Panel(_) => {
                app.panel_walk(-1);
                Want::Nothing
            }
            _ => app.walk(-3),
        },
        // **Pressed, then done on the release** — which is what every button
        // anywhere does, and it is what lets somebody press, think better of
        // it, and slide off before letting go.
        MouseEventKind::Down(MouseButton::Left) => {
            app.pressed = spot;
            app.dirty = true;
            Want::Nothing
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let pressed = std::mem::take(&mut app.pressed);
            app.dirty = true;
            if pressed != spot || pressed == Spot::Nothing {
                // Let go somewhere else: nothing happens, as everywhere else.
                if app.panel != Panel::None && !matches!(spot, Spot::Panel(_)) {
                    app.show(app.panel);
                }
                return Want::Nothing;
            }
            match spot {
                Spot::Row(row) => {
                    app.in_rail = false;
                    app.go(row)
                }
                Spot::Rail(at) => {
                    app.in_rail = true;
                    app.rail_at = at;
                    app.rail_press()
                }
                Spot::Strip(at) => {
                    let days = app.strip[at].0;
                    app.press_filter(&scour_ui::query::of_age(days))
                }
                Spot::Panel(at) => {
                    app.panel_at = at;
                    panel_press(app)
                }
                Spot::Head(column) => app.sort_by(column),
                // A press on the track jumps there, the same as a drag to it.
                Spot::Bar(at) => {
                    let (_, height) = size;
                    let strip_high: u16 = if height >= 20 && !app.strip.is_empty() {
                        3
                    } else {
                        0
                    };
                    let list_to = height.saturating_sub(1 + strip_high);
                    app.drag_bar(at, list_to.saturating_sub(crate::draw::LIST_TOP + 1))
                }
                Spot::Query => {
                    app.mode = Mode::Search;
                    Want::Nothing
                }
                Spot::Nothing => Want::Nothing,
            }
        }
        _ => Want::Nothing,
    }
}

/// What is drawn at this column and row.
///
/// The one piece of arithmetic that turns a place on the screen into a thing,
/// and the reason there is exactly one: `draw` and this have to agree, and
/// nothing tells you when they stop.
pub fn spot_at(app: &App, col: u16, row: u16, size: (u16, u16)) -> Spot {
    use crate::draw::{LIST_TOP, QUERY_HIGH, RAIL_TOP, RAIL_WIDE};
    let (width, height) = size;

    if app.panel != Panel::None {
        let area = ratatui::layout::Rect::new(0, 0, width, height);
        let lines = app.panel_lines();
        let box_area = crate::draw::panel_rect(area, lines);
        let inside = col >= box_area.x
            && col < box_area.x + box_area.width
            && row >= box_area.y
            && row < box_area.y + box_area.height;
        if !inside || row < box_area.y + 2 {
            return Spot::Nothing;
        }
        let room = box_area.height.saturating_sub(3) as usize;
        let from = app.panel_at.saturating_sub(room.saturating_sub(1));
        let at = from + (row - (box_area.y + 2)) as usize;
        return if at < lines {
            Spot::Panel(at)
        } else {
            Spot::Nothing
        };
    }

    if row < QUERY_HIGH {
        return Spot::Query;
    }

    let strip_high: u16 = if height >= 20 && !app.strip.is_empty() {
        3
    } else {
        0
    };
    let list_to = height.saturating_sub(1 + strip_high);
    if strip_high > 0 && row >= list_to && row < list_to + 2 {
        let bands = app.strip.len().max(1);
        let room = width.saturating_sub(2).max(1) as usize;
        let at = (col.saturating_sub(1) as usize * bands / room).min(bands - 1);
        return Spot::Strip(at);
    }

    let railed = width >= 100 && app.rail;
    if railed && col < RAIL_WIDE {
        if row < RAIL_TOP || row >= list_to {
            return Spot::Nothing;
        }
        return match app.rail_hit((row - RAIL_TOP) as usize) {
            Some(at) => Spot::Rail(at),
            None => Spot::Nothing,
        };
    }
    // The heading row, one above the list: which column was hit is the same
    // arithmetic that drew them.
    if row == LIST_TOP - 1 {
        let from = if railed { RAIL_WIDE } else { 0 };
        return match crate::draw::column_at(col.saturating_sub(from), width - from) {
            Some(column) => Spot::Head(column),
            None => Spot::Nothing,
        };
    }
    if row < LIST_TOP || row >= list_to {
        return Spot::Nothing;
    }
    // The scrollbar has the last column of the list to itself.
    if col + 1 >= width && app.pages.total() > app.room {
        return Spot::Bar(row - LIST_TOP);
    }
    let at = app.top + (row - LIST_TOP) as usize;
    if at < app.pages.total() {
        Spot::Row(at)
    } else {
        Spot::Nothing
    }
}

/// Hand the row under the cursor to the desktop — or its folder.
///
/// **Detached, and nothing is waited for.** A file manager that takes two
/// seconds to start would otherwise be two seconds of a terminal that does not
/// answer the keyboard.
fn open(app: &mut App, folder: bool) -> Want {
    let Some(hit) = app.here() else {
        return Want::Nothing;
    };
    let path = if folder {
        scour_ui::path::folder(&hit.path).to_string()
    } else {
        hit.path.clone()
    };
    let _ = std::process::Command::new("xdg-open")
        .arg(&path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    Want::Nothing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn letters_type_in_search_and_move_in_move() {
        let mut app = App::default();
        press(&mut app, key(KeyCode::Char('j')));
        assert_eq!(app.query, "j", "a letter is a letter while searching");

        app.query.clear();
        app.caret = 0;
        press(&mut app, key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Move, "an empty query, then the mode");
        press(&mut app, key(KeyCode::Char('j')));
        assert_eq!(app.query, "", "and now it moves instead");
    }

    #[test]
    fn escape_clears_what_was_typed_before_it_changes_anything_else() {
        let mut app = App::default();
        press(&mut app, key(KeyCode::Char('a')));
        press(&mut app, key(KeyCode::Esc));
        assert_eq!(app.query, "");
        assert_eq!(app.mode, Mode::Search, "still typing");
    }

    #[test]
    fn the_arrows_move_in_both_modes() {
        let mut app = App::default();
        app.room = 10;
        app.pages.set_total(100);
        press(&mut app, key(KeyCode::Down));
        assert_eq!(app.cursor, 1);
        app.mode = Mode::Move;
        press(&mut app, key(KeyCode::Down));
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn space_picks_in_move_mode_and_types_in_search() {
        let mut app = App::default();
        app.room = 4;
        app.pages.set_total(10);
        press(&mut app, key(KeyCode::Char(' ')));
        assert_eq!(app.query, " ", "a space is a space while typing");

        let mut app = App {
            mode: Mode::Move,
            ..App::default()
        };
        app.room = 4;
        press(&mut app, key(KeyCode::Char(' ')));
        assert!(app.picked.is_empty(), "nothing under the cursor to pick");
    }

    #[test]
    fn sorting_is_ctrl_and_never_the_bare_arrows() {
        let mut app = App::default();
        app.room = 4;
        app.pages.set_total(10);
        let was = app.sort;
        press(&mut app, key(KeyCode::Right));
        assert_eq!(app.sort, was, "a bare arrow does not re-sort");
        press(
            &mut app,
            KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL),
        );
        assert_ne!(app.sort, was, "with ctrl it does");
        let down = app.descending;
        press(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
        assert_ne!(app.descending, down);
    }

    #[test]
    fn the_help_is_closed_by_whatever_is_pressed_next() {
        let mut app = App::default();
        press(&mut app, key(KeyCode::F(1)));
        assert!(app.helping);
        press(&mut app, key(KeyCode::Char('x')));
        assert!(!app.helping);
        assert_eq!(app.query, "", "and that key did nothing else");
    }

    #[test]
    fn tab_moves_the_arrows_into_the_rail_and_back() {
        let mut app = App::default();
        app.room = 4;
        app.pages.set_total(100);
        app.kinds = vec![("doc".into(), 9), ("code".into(), 4)];
        press(&mut app, key(KeyCode::Tab));
        assert!(app.in_rail);
        press(&mut app, key(KeyCode::Down));
        assert_eq!(app.rail_at, 1, "in the rail, down moves the rail");
        assert_eq!(app.cursor, 0, "and leaves the list alone");
        press(&mut app, key(KeyCode::Tab));
        assert!(!app.in_rail);
        press(&mut app, key(KeyCode::Down));
        assert_eq!(app.cursor, 1, "and now it moves the list again");
    }

    #[test]
    fn pressing_a_filter_narrows_the_query_and_pressing_it_again_does_not() {
        let mut app = App::default();
        app.room = 4;
        app.kinds = vec![("doc".into(), 9)];
        app.in_rail = true;
        press(&mut app, key(KeyCode::Enter));
        assert_eq!(app.filter.as_deref(), Some("kind:doc"));
        assert_eq!(
            app.asking(),
            "kind:doc",
            "and it is what the service is asked"
        );
        press(&mut app, key(KeyCode::Enter));
        assert_eq!(app.filter, None, "the same press clears it");
    }

    #[test]
    fn a_panel_takes_the_arrows_and_gives_them_back() {
        let mut app = App::default();
        app.room = 4;
        app.pages.set_total(100);
        app.rules = vec![
            ("dir:target".into(), "target".into(), false, true),
            ("path:/proc".into(), "/proc".into(), true, false),
        ];
        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.panel, Panel::Rules);
        press(&mut app, key(KeyCode::Down));
        assert_eq!(app.panel_at, 1);
        assert_eq!(app.cursor, 0, "the list did not move");
        press(&mut app, key(KeyCode::Esc));
        assert_eq!(app.panel, Panel::None);
        press(&mut app, key(KeyCode::Down));
        assert_eq!(app.cursor, 1, "and the arrows are the list's again");
    }

    #[test]
    fn switching_a_rule_off_sends_the_whole_list_the_service_gave() {
        // The window's data loss, which this must not repeat: a list built
        // from what has been pressed rather than from the answer switches
        // every other rule back on.
        let mut app = App::default();
        app.rules = vec![
            ("dir:target".into(), "target".into(), false, true),
            ("path:/proc".into(), "/proc".into(), true, false),
            ("path:/sys".into(), "/sys".into(), true, false),
        ];
        app.panel = Panel::Rules;
        app.panel_at = 0;
        let want = panel_press(&mut app);
        match want {
            Want::OffRules(off) => assert_eq!(
                off,
                vec!["dir:target", "path:/proc", "path:/sys"],
                "the one pressed, and everything already off"
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ctrl_c_leaves_from_either_mode() {
        for mode in [Mode::Search, Mode::Move] {
            let mut app = App {
                mode,
                ..App::default()
            };
            press(
                &mut app,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            );
            assert!(app.leaving, "{mode:?}");
        }
    }
}
