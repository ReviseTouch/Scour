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

use crate::app::{App, Mode, Want};

/// What a key does. Returns what to ask the service for, if anything.
pub fn press(app: &mut App, key: KeyEvent) -> Want {
    // Windows sends a key twice — down and up — and a terminal that acted on
    // both would type every letter twice.
    if key.kind == KeyEventKind::Release {
        return Want::Nothing;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let page = app.room.max(1) as isize;

    // The keys that mean the same thing in both modes, first — so that nothing
    // below can shadow them.
    match key.code {
        KeyCode::Char('c' | 'q') if ctrl => {
            app.leaving = true;
            return Want::Leave;
        }
        KeyCode::Up => return app.walk(-1),
        KeyCode::Down => return app.walk(1),
        KeyCode::PageUp => return app.walk(-page),
        KeyCode::PageDown => return app.walk(page),
        KeyCode::Home => return app.go(0),
        KeyCode::End => return app.go(usize::MAX),
        KeyCode::Enter => return open(app),
        _ => {}
    }

    match app.mode {
        Mode::Search => match key.code {
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

/// What the mouse does: the wheel scrolls, a press puts the cursor on a row.
pub fn mouse(app: &mut App, m: MouseEvent) -> Want {
    match m.kind {
        MouseEventKind::ScrollDown => app.walk(3),
        MouseEventKind::ScrollUp => app.walk(-3),
        MouseEventKind::Down(MouseButton::Left) => {
            // Three lines of chrome sit above the list: query, meter, heading.
            let row = m.row as usize;
            if row < 3 {
                return Want::Nothing;
            }
            app.go(app.top + (row - 3))
        }
        _ => Want::Nothing,
    }
}

/// Hand the row under the cursor to the desktop.
///
/// **Detached, and nothing is waited for.** A file manager that takes two
/// seconds to start would otherwise be two seconds of a terminal that does not
/// answer the keyboard.
fn open(app: &mut App) -> Want {
    let Some(hit) = app.here() else {
        return Want::Nothing;
    };
    let path = hit.path.clone();
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
