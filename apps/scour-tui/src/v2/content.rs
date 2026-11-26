//! Editing the content field: a caret in the pattern, the pill's deeds on a
//! capsule, and the `:` menu. Every change rewrites the pattern text and
//! asks again; nothing here keeps a second model of it.

use scour_proto::GrepMode;
use scour_ui::pattern::{self, Item, ItemAction, edit};

use crate::app::{App, Want};

/// What a pill's button does to its piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deed {
    /// A letter off for the alternative, or exact again.
    Similar,
    /// Excluded, or no longer.
    Not,
    Remove,
}

/// The capsule a pill is over: its piece, the alternative a letter off is
/// for, and whether it was chosen from the keyboard — then its keys act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pill {
    pub piece: usize,
    pub alt: usize,
    pub by_key: bool,
}

fn byte_at(s: &str, ch: usize) -> usize {
    s.char_indices().nth(ch).map_or(s.len(), |(i, _)| i)
}

impl App {
    /// The caret in the pattern, in characters.
    pub fn grep_caret(&self) -> usize {
        self.v2.grep_caret.min(self.v2.grep_pattern.chars().count())
    }

    /// The pattern is now `text`, the caret at `caret`: drawn again from it
    /// and asked again.
    fn grep_write(&mut self, text: String, caret: usize) -> Want {
        self.v2.grep_pattern = text;
        self.v2.grep_caret = caret;
        self.v2.grep_menu_at = 0;
        let asked = pattern::asking(&self.v2.grep_pattern, caret).map(|(c, _)| c);
        if asked != self.v2.grep_menu_shut {
            self.v2.grep_menu_shut = None;
        }
        self.dirty = true;
        self.dispatch_grep(false)
    }

    pub fn grep_type(&mut self, c: char) -> Want {
        self.v2.grep_pill = None;
        let at = self.grep_caret();
        let mut text = self.v2.grep_pattern.clone();
        text.insert(byte_at(&text, at), c);
        self.grep_write(text, at + 1)
    }

    /// A character rubbed out, before the caret or after it: letters, as any
    /// field rubs them out. A capsule the caret reaches is letters by then.
    pub fn grep_rub(&mut self, after: bool) -> Want {
        self.v2.grep_pill = None;
        let at = self.grep_caret();
        let gone = if after { at } else { at.wrapping_sub(1) };
        if gone >= self.v2.grep_pattern.chars().count() {
            return Want::Nothing;
        }
        let mut text = self.v2.grep_pattern.clone();
        text.remove(byte_at(&text, gone));
        self.grep_write(text, gone)
    }

    pub fn grep_step(&mut self, by: isize) {
        self.v2.grep_pill = None;
        let n = self.v2.grep_pattern.chars().count();
        self.v2.grep_caret = self.grep_caret().saturating_add_signed(by).min(n);
        self.dirty = true;
    }

    /// The `:` the caret is asking with and what it offers, while the menu
    /// is open: not after `Esc` on that `:`, and not when nothing is left.
    pub fn grep_menu(&self) -> Option<(usize, String, Vec<Item>)> {
        if !self.v2.grep_open || self.v2.grep_mode != GrepMode::Text {
            return None;
        }
        let (colon, typed) = pattern::asking(&self.v2.grep_pattern, self.grep_caret())?;
        if self.v2.grep_menu_shut == Some(colon) {
            return None;
        }
        let items = pattern::items(&typed, |m| self.say(m).into_owned());
        (!items.is_empty()).then_some((colon, typed, items))
    }

    pub fn grep_menu_walk(&mut self, by: isize) {
        if let Some((_, _, items)) = self.grep_menu() {
            let at = self.v2.grep_menu_at.min(items.len() - 1);
            self.v2.grep_menu_at = at.saturating_add_signed(by).min(items.len() - 1);
            self.dirty = true;
        }
    }

    /// The menu's item `at`, or the one its cursor is on: written in place of
    /// the `:` and its letters, or the case flipped.
    pub fn grep_menu_choose(&mut self, at: Option<usize>) -> Want {
        let Some((colon, _, items)) = self.grep_menu() else {
            return Want::Nothing;
        };
        let item = items[at.unwrap_or(self.v2.grep_menu_at).min(items.len() - 1)];
        let (text, caret) = pattern::choose(&self.v2.grep_pattern, colon, self.grep_caret(), &item);
        if item.action == ItemAction::Case {
            self.v2.grep_exact = !self.v2.grep_exact;
        }
        self.grep_write(text, caret)
    }

    /// Closed on this `:`, which stays as typed.
    pub fn grep_menu_close(&mut self) {
        if let Some((colon, _, _)) = self.grep_menu() {
            self.v2.grep_menu_shut = Some(colon);
            self.dirty = true;
        }
    }

    /// The capsule `by` steps from the chosen one — from the caret when none
    /// is — chosen, its pill showing its keys.
    pub fn grep_choose(&mut self, by: isize) {
        let pieces = pattern::pieces(&self.v2.grep_pattern);
        let caret = self.grep_caret();
        let edited = pattern::piece_at(&pieces, caret);
        let caps: Vec<usize> = (0..pieces.len()).filter(|&k| Some(k) != edited).collect();
        let chosen = self.v2.grep_pill.filter(|p| p.by_key).map(|p| p.piece);
        let next = match chosen.and_then(|c| caps.iter().position(|&k| k == c)) {
            Some(i) => caps
                .get(i.saturating_add_signed(by))
                .copied()
                .or(Some(caps[i])),
            None if by < 0 => caps.iter().rev().copied().find(|&k| pieces[k].end <= caret),
            None => caps.iter().copied().find(|&k| pieces[k].start >= caret),
        };
        if let Some(piece) = next {
            self.v2.grep_pill = Some(Pill {
                piece,
                alt: 0,
                by_key: true,
            });
            self.dirty = true;
        }
    }

    /// A press on a capsule: the caret at its end, where it turns to letters.
    pub fn grep_into(&mut self, piece: usize) {
        if let Some(p) = pattern::pieces(&self.v2.grep_pattern).get(piece) {
            self.v2.grep_caret = p.end;
            self.v2.grep_pill = None;
            self.dirty = true;
        }
    }

    /// What the pill's button does to the piece it is over.
    pub fn grep_deed(&mut self, deed: Deed) -> Want {
        let Some(pill) = self.v2.grep_pill else {
            return Want::Nothing;
        };
        let old = &self.v2.grep_pattern;
        let Some(end) = pattern::pieces(old).get(pill.piece).map(|p| p.end) else {
            return Want::Nothing;
        };
        let text = match deed {
            Deed::Remove => edit::remove(old, pill.piece),
            Deed::Not => edit::toggle_not(old, pill.piece),
            Deed::Similar => edit::toggle_similar(old, pill.piece, pill.alt),
        };
        // The caret is never in the piece: after it, it moves by what changed.
        let (was, now) = (old.chars().count(), text.chars().count());
        let caret = self.grep_caret();
        let caret = if caret >= end {
            (caret + now).saturating_sub(was)
        } else {
            caret.min(now)
        };
        if deed == Deed::Remove {
            self.v2.grep_pill = None;
        }
        self.grep_write(text, caret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Want;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn open(pattern: &str) -> App {
        let mut app = App::default();
        app.v2.grep_open = true;
        app.v2.grep_pattern = pattern.into();
        app.v2.grep_caret = pattern.chars().count();
        app
    }

    fn key(app: &mut App, code: KeyCode, m: KeyModifiers) -> Want {
        crate::keys::press(app, KeyEvent::new(code, m))
    }

    #[test]
    fn letters_go_in_at_the_caret_and_rub_out_one_at_a_time() {
        let mut app = open("ab cd");
        key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.v2.grep_pattern, "ab xcd");
        // At the start of `cd` the space before goes, not the capsule `ab`.
        key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(app.v2.grep_pattern, "abxcd");
        assert_eq!(app.grep_caret(), 2);
        key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(app.v2.grep_pattern, "abcd");
    }

    #[test]
    fn a_colon_opens_the_menu_and_esc_leaves_it_typed() {
        let mut app = open("icra ");
        key(&mut app, KeyCode::Char(':'), KeyModifiers::NONE);
        assert_eq!(app.grep_menu().map(|m| m.2.len()), Some(5));
        for c in "qq".chars() {
            key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(app.grep_menu(), None, "nothing left: the `:` is text");
        let mut app = open("icra ");
        for c in ":sim".chars() {
            key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        let tokens: Vec<&str> = app.grep_menu().unwrap().2.iter().map(|i| i.token).collect();
        assert_eq!(tokens, ["similar"]);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.v2.grep_pattern, "icra ~");
        assert_eq!(app.grep_caret(), 6);
        assert!(app.v2.grep_open);

        let mut app = open("a ");
        key(&mut app, KeyCode::Char(':'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.v2.grep_open, "Esc closes the menu, not the field");
        assert_eq!(app.v2.grep_pattern, "a :");
        assert_eq!(app.grep_menu(), None);
    }

    #[test]
    fn match_case_from_the_menu_flips_the_toggle_and_takes_the_colon() {
        let mut app = open("a ");
        for c in ":match".chars() {
            key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(!app.v2.grep_exact);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.v2.grep_exact);
        assert_eq!(app.v2.grep_pattern, "a ");
    }

    #[test]
    fn a_chosen_capsule_takes_the_pills_keys() {
        let mut app = open("ab cd ef");
        key(&mut app, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(app.v2.grep_pill.map(|p| p.piece), Some(1));
        key(&mut app, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(app.v2.grep_pill.map(|p| p.piece), Some(0));
        key(&mut app, KeyCode::Char('!'), KeyModifiers::NONE);
        assert_eq!(app.v2.grep_pattern, "!ab cd ef");
        key(&mut app, KeyCode::Char('~'), KeyModifiers::NONE);
        assert_eq!(app.v2.grep_pattern, "!~ab cd ef");
        assert_eq!(app.grep_caret(), 10, "the caret after them moved with them");
        key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(app.v2.grep_pattern, "cd ef");
        assert_eq!(app.grep_caret(), 5);
        assert_eq!(app.v2.grep_pill, None);
        // Any other key lets the choice go and types.
        key(&mut app, KeyCode::Left, KeyModifiers::ALT);
        key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.v2.grep_pattern, "cd efx");
        assert_eq!(app.v2.grep_pill, None);
    }

    #[test]
    fn a_press_on_a_capsule_puts_the_caret_at_its_end() {
        let mut app = open("ab cd");
        app.grep_into(0);
        assert_eq!(app.grep_caret(), 2);
    }
}
