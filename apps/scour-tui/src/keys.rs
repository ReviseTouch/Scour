//! One table: what every key does, in both modes.
//!
//! The help screen is drawn from [`MAP`], so documentation cannot drift from
//! behaviour. A mode never changes what a key means — `Enter`, `Tab`, the
//! arrows, `Ctrl` and the mouse are the same in both — only where letters go.

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::{App, Mode, Panel, Spot, Want};

/// Every key, and what it does. The help screen is drawn from it.
pub const MAP: &[(&str, &str)] = &[
    ("type", "search"),
    ("↑ ↓ · PgUp PgDn · Home End", "move through the list"),
    ("Enter", "open"),
    ("Shift+Enter", "open the folder"),
    ("Insert · Ctrl+Space", "pick · Shift+↑↓ for a run"),
    ("click the ✓ column", "pick that row"),
    ("Ctrl+A", "pick nothing"),
    (
        "Ctrl+Y · Ctrl+O",
        "copy the picked paths · open their folders",
    ),
    ("Ctrl+← →", "sort by the next column · or click a heading"),
    ("Ctrl+↑ ↓", "reverse the order"),
    ("Tab", "the rail, and back"),
    ("Ctrl+K", "what is skipped"),
    ("Ctrl+L", "language"),
    ("Ctrl+U", "which face to run"),
    ("Ctrl+E", "write the result as a spreadsheet"),
    ("F2 · Ctrl+R", "the report, and back"),
    ("F3", "the head of the file, without opening it"),
    (
        "in the report",
        "↑↓ a folder · Enter into it · Backspace out",
    ),
    ("F1", "this · or press `keys` on the counter line"),
    ("Esc", "clear the query, then move mode"),
    ("j k · g G · d u", "move, in move mode"),
    ("i · /", "back to typing"),
    ("Ctrl+C · Ctrl+Q", "leave"),
];

/// What a key does. Returns what to ask the service for, if anything.
pub fn press(app: &mut App, key: KeyEvent) -> Want {
    // Windows sends a key twice, down and up: acting on both types it twice.
    if key.kind == KeyEventKind::Release {
        return Want::Nothing;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let page = app.room.max(1) as isize;

    // The help is a panel over everything, and any key at all closes it.
    if app.helping {
        app.helping = false;
        app.dirty = true;
        return Want::Nothing;
    }

    // The one panel that listens to letters: elsewhere the query keeps typing
    // while a panel is open. `Space` is a letter here, so this branch comes
    // before the one that reads it as a press.
    if app.panel == Panel::Ask && app.ask_typing {
        match key.code {
            KeyCode::Esc => {
                app.ask_typing = false;
                app.pending = None;
                app.show(Panel::Ask);
                return Want::Nothing;
            }
            KeyCode::Backspace => {
                app.ask_text.pop();
                app.dirty = true;
                return Want::Nothing;
            }
            KeyCode::Char(c) if !ctrl => {
                app.ask_text.push(c);
                app.dirty = true;
                return Want::Nothing;
            }
            _ => {}
        }
    }

    // A panel takes the arrows and `Enter`; the query still types under it.
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

    // In the report the arrows walk the weighed folders, `Enter` goes into one.
    if app.reporting && app.panel == Panel::None {
        match key.code {
            KeyCode::Up => {
                app.weigh_walk(-1);
                return Want::Nothing;
            }
            KeyCode::Down => {
                app.weigh_walk(1);
                return Want::Nothing;
            }
            KeyCode::Enter => return app.weigh_into(),
            KeyCode::Backspace | KeyCode::Left => return app.weigh_up(),
            _ => {}
        }
    }

    // The keys that mean the same in both modes, first: nothing shadows them.
    match key.code {
        KeyCode::Char('c' | 'q') if ctrl => {
            app.leaving = true;
            return Want::Leave;
        }
        KeyCode::Char('a') if ctrl => {
            app.unpick();
            return Want::Nothing;
        }
        // Picking has to work while typing, where `Space` alone is a space.
        KeyCode::Char(' ') if ctrl => return app.pick(),
        KeyCode::Insert => return app.pick(),
        // The report, on the key the window uses for it.
        KeyCode::F(2) => return app.report(),
        // The head of a file, on the key file managers have long used for it.
        KeyCode::F(3) => return app.peek(),
        KeyCode::Char('r') if ctrl => return app.report(),
        // The row's menu, without leaving the query line. Not `Ctrl+M`, which
        // *is* carriage return and reaches this as `Enter`.
        KeyCode::F(4) => {
            app.open_menu();
            return Want::Nothing;
        }
        KeyCode::F(1) => {
            app.helping = true;
            app.dirty = true;
            return Want::Nothing;
        }
        KeyCode::Char('k') if ctrl => {
            app.show(Panel::Rules);
            // Asked when it opens: this panel is the only thing changing them.
            return Want::Rules;
        }
        KeyCode::Char('l') if ctrl => {
            app.show(Panel::Language);
            return Want::Nothing;
        }
        KeyCode::Char('t') if ctrl => {
            app.show(Panel::Columns);
            return Want::Nothing;
        }
        KeyCode::Char('u') if ctrl => {
            app.show(Panel::Faces);
            return Want::Nothing;
        }
        KeyCode::Char('e') if ctrl => return app.write_sheet(),
        // `Ctrl+V` is usually the terminal's own, but terminals that keep it
        // never deliver it, so the clipboard is read directly here too.
        KeyCode::Char('v') if ctrl => {
            return match paste() {
                Some(text) => pasted(app, &text),
                None => {
                    app.note = "nothing to paste".into();
                    app.dirty = true;
                    Want::Nothing
                }
            };
        }
        // Two of the three selection deeds; clearing is `Ctrl+A` above.
        KeyCode::Char('y') if ctrl => return app.deed(0),
        KeyCode::Char('o') if ctrl => return app.deed(1),
        // Sorting takes `Ctrl`: the bare arrows move and always will.
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
        // `Tab` decides where the arrows go, and the footer says which.
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
        KeyCode::Up => {
            let want = app.walk(-1);
            return follow_peek(app, want);
        }
        KeyCode::Down => {
            let want = app.walk(1);
            return follow_peek(app, want);
        }
        KeyCode::PageUp => return app.walk(-page),
        KeyCode::PageDown => return app.walk(page),
        KeyCode::Home => return app.go(0),
        KeyCode::End => return app.go(usize::MAX),
        // One key, two places: the cursor says which, as with the arrows.
        KeyCode::Enter if app.in_rail => return app.rail_press(),
        KeyCode::Enter => return open(app, shift),
        _ => {}
    }

    match app.mode {
        Mode::Search => match key.code {
            // In search mode a space is a space: it separates two terms.
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
            // `Esc` empties the query before it changes the mode.
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
            // `m`, because a terminal has no right button. In move mode only:
            // a bare letter belongs to the query while searching.
            KeyCode::Char('m') => {
                app.open_menu();
                Want::Nothing
            }
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

/// A page still has to be asked for when the cursor moves; only one of the page
/// and the peek can be returned.
fn follow_peek(app: &mut App, want: Want) -> Want {
    let peek = app.repeek();
    match (&want, &peek) {
        // The page matters more: without it there is nothing to peek at.
        (Want::Page { .. }, _) => want,
        (_, Want::Peek(_)) => peek,
        _ => want,
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
        Panel::Menu => app.menu_pick(),
        Panel::Openers => app.open_with(),
        Panel::Columns => app.pick_column(),
        // Enter on the line being typed into means yes.
        Panel::Ask if app.panel_at == 0 => app.ask_answer(2),
        Panel::Ask => app.ask_answer(app.panel_at),
        Panel::None => Want::Nothing,
    }
}

/// Text arriving in one piece — a paste rather than typing. Newlines become
/// spaces, which is what they mean in a query.
pub fn pasted(app: &mut App, text: &str) -> Want {
    let text = text.replace(['\n', '\r', '\t'], " ");
    let mut want = Want::Nothing;
    for c in text.chars() {
        want = app.insert(c);
    }
    app.mode = Mode::Search;
    want
}

/// What is on the clipboard, if anything can say.
fn paste() -> Option<String> {
    for (tool, args) in [
        ("wl-paste", &["--no-newline"][..]),
        ("xclip", &["-selection", "clipboard", "-o"][..]),
        ("xsel", &["--clipboard", "--output"][..]),
    ] {
        if let Ok(out) = std::process::Command::new(tool).args(args).output()
            && out.status.success()
        {
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// What the mouse does: the wheel moves whatever is under it, a press acts on
/// wherever it landed. Where it landed decides what it means — see [`spot_at`].
pub fn mouse(app: &mut App, m: MouseEvent, size: (u16, u16)) -> Want {
    let spot = spot_at(app, m.column, m.row, size);
    // A terminal draws no hover of its own: what is under the pointer is
    // remembered here, and the drawing lights it.
    if app.hover != spot {
        app.hover = spot;
        app.dirty = true;
    }
    match m.kind {
        MouseEventKind::Moved => Want::Nothing,
        // A drag has to be followed while the button is down, not on release.
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
        // Pressed, then done on the release: sliding off cancels it.
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
                // Plain: this row alone. `Ctrl`: this as well. `Shift`: the run.
                Spot::Row(row) => {
                    app.in_rail = false;
                    if m.modifiers.contains(KeyModifiers::SHIFT) {
                        app.pick_to(row)
                    } else if m.modifiers.contains(KeyModifiers::CONTROL) {
                        let want = app.go(row);
                        app.pick();
                        want
                    } else {
                        app.pick_only(row)
                    }
                }
                // The mark is a checkbox: it adds and removes, never replaces.
                Spot::Tick(row) => {
                    app.in_rail = false;
                    let want = app.go(row);
                    app.pick();
                    want
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
                Spot::Chip => app.unfilter(),
                Spot::Tool(which) => app.tool(which),
                Spot::Deed(which) => app.deed(which),
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

/// What is drawn at this column and row. The only place the screen's geometry
/// is read back, because nothing warns when it stops agreeing with `draw`.
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

    // The bar of things to do with a selection, along the bottom row.
    if row + 1 == height && !app.picked.is_empty() {
        return match crate::draw::deed_at(app, col, width) {
            Some(which) => Spot::Deed(which),
            None => Spot::Nothing,
        };
    }
    // The counter line, which carries the tools at its right end.
    if row == QUERY_HIGH && width >= 90 {
        return match crate::draw::tool_at(app, col, width) {
            Some(which) => Spot::Tool(which),
            None => Spot::Nothing,
        };
    }
    if row < QUERY_HIGH {
        if row == crate::draw::QUERY_ROW
            && let Some((from, to)) = crate::draw::chip_at(app)
            && col >= from
            && col < to
        {
            return Spot::Chip;
        }
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
    // The heading row, one above the list, measured as `draw` measured it.
    if row == LIST_TOP - 1 {
        let from = if railed { RAIL_WIDE } else { 0 };
        return match crate::draw::column_at(col.saturating_sub(from), width - from, &app.columns) {
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
    if at >= app.pages.total() {
        return Spot::Nothing;
    }
    // The row's first two columns are the age stripe and the tick: pressing
    // there picks the row, pressing the name puts the cursor on it.
    let from = if railed { RAIL_WIDE } else { 0 };
    if col <= from + 1 {
        return Spot::Tick(at);
    }
    Spot::Row(at)
}

/// Hand the row under the cursor to the desktop — or its folder. Detached: a
/// file manager that takes seconds to start must not hold the keyboard.
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

    /// An app with a screen to put rows on. `App::default()` has no room, and
    /// a view zero rows tall answers every scrolling question the same way.
    fn app(room: usize) -> App {
        App {
            room,
            ..Default::default()
        }
    }

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
        let mut app = app(10);
        app.pages.set_total(100);
        press(&mut app, key(KeyCode::Down));
        assert_eq!(app.cursor, 1);
        app.mode = Mode::Move;
        press(&mut app, key(KeyCode::Down));
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn space_picks_in_move_mode_and_types_in_search() {
        let mut app = app(4);
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
        let mut app = app(4);
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
        let mut app = app(4);
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
        let mut app = app(4);
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
        let mut app = app(4);
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
        // A list built from what was pressed, rather than from the service's
        // answer, switches every other rule back on.
        let mut app = App {
            rules: vec![
                ("dir:target".into(), "target".into(), false, true),
                ("path:/proc".into(), "/proc".into(), true, false),
                ("path:/sys".into(), "/sys".into(), true, false),
            ],
            panel: Panel::Rules,
            panel_at: 0,
            ..Default::default()
        };
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
