//! A key combination in one spelling, and the two the desktops use.

use std::fmt;

/// A modifier, in the order the neutral spelling writes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Modifier {
    Super,
    Ctrl,
    Alt,
    Shift,
}

/// `super+f`, `ctrl+alt+s`, `super+F2`, `ctrl+"`: modifiers, then one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    mods: Vec<Modifier>,
    /// A lowercase letter or digit, a punctuation character, `F<n>`, or a name
    /// from [`SPECIAL`] such as `space`.
    key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    Empty,
    /// Modifiers and nothing to press with them: `ctrl+`.
    NoKey,
    UnknownModifier(String),
    /// Modifiers alone are not a key.
    OnlyModifiers,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::Empty => write!(f, "no key given"),
            KeyError::NoKey => write!(f, "a key has to follow the modifiers"),
            KeyError::UnknownModifier(m) => write!(f, "not a modifier: {m}"),
            KeyError::OnlyModifiers => write!(f, "modifiers alone are not a key"),
        }
    }
}

impl std::error::Error for KeyError {}

/// Punctuation as a character and as the X keysym GNOME writes.
const NAMED: &[(char, &str)] = &[
    (' ', "space"),
    ('.', "period"),
    (',', "comma"),
    (';', "semicolon"),
    (':', "colon"),
    ('\'', "apostrophe"),
    ('"', "quotedbl"),
    ('/', "slash"),
    ('\\', "backslash"),
    ('-', "minus"),
    ('=', "equal"),
    ('+', "plus"),
    ('[', "bracketleft"),
    (']', "bracketright"),
    ('`', "grave"),
    ('<', "less"),
    ('>', "greater"),
    ('*', "asterisk"),
    ('#', "numbersign"),
    ('!', "exclam"),
    ('?', "question"),
    ('@', "at"),
    ('&', "ampersand"),
    ('%', "percent"),
    ('$', "dollar"),
    ('^', "asciicircum"),
    ('~', "asciitilde"),
    ('|', "bar"),
    ('_', "underscore"),
    ('(', "parenleft"),
    (')', "parenright"),
    ('{', "braceleft"),
    ('}', "braceright"),
];

/// Keys with names: ours, GNOME's keysym, KDE's Qt name.
const SPECIAL: &[(&str, &str, &str)] = &[
    ("space", "space", "Space"),
    ("tab", "Tab", "Tab"),
    ("return", "Return", "Return"),
    ("escape", "Escape", "Esc"),
    ("backspace", "BackSpace", "Backspace"),
    ("delete", "Delete", "Del"),
    ("insert", "Insert", "Ins"),
    ("home", "Home", "Home"),
    ("end", "End", "End"),
    ("pageup", "Page_Up", "PgUp"),
    ("pagedown", "Page_Down", "PgDown"),
    ("up", "Up", "Up"),
    ("down", "Down", "Down"),
    ("left", "Left", "Left"),
    ("right", "Right", "Right"),
    ("print", "Print", "Print"),
    ("pause", "Pause", "Pause"),
    ("menu", "Menu", "Menu"),
];

impl Key {
    /// The neutral spelling, case aside: `super+f`, `Ctrl+Alt+S`, `super+F2`,
    /// `ctrl+"`, `super+space`. `win`, `meta` and `cmd` mean `super`; `control`
    /// means `ctrl`; `option` means `alt`.
    pub fn parse(s: &str) -> Result<Key, KeyError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(KeyError::Empty);
        }
        // The last `+` separates the key; `ctrl++` is Ctrl and the plus key, and
        // a lone trailing `+` is a modifier with nothing after it.
        let (mods_part, key_part) = if let Some(head) = s.strip_suffix("++") {
            (head, "+")
        } else if s.ends_with('+') {
            return Err(KeyError::NoKey);
        } else {
            match s.rfind('+') {
                None => ("", s),
                Some(at) => (&s[..at], &s[at + 1..]),
            }
        };
        let mut mods = Vec::new();
        for m in mods_part.split('+').filter(|m| !m.is_empty()) {
            let m = match m.to_ascii_lowercase().as_str() {
                "super" | "win" | "meta" | "cmd" | "mod4" => Modifier::Super,
                "ctrl" | "control" | "primary" => Modifier::Ctrl,
                "alt" | "option" | "mod1" => Modifier::Alt,
                "shift" => Modifier::Shift,
                other => return Err(KeyError::UnknownModifier(other.to_owned())),
            };
            if !mods.contains(&m) {
                mods.push(m);
            }
        }
        mods.sort();
        let key = normalise(key_part).ok_or(KeyError::OnlyModifiers)?;
        Ok(Key { mods, key })
    }

    pub fn modifiers(&self) -> &[Modifier] {
        &self.mods
    }

    /// The key alone, in the neutral spelling.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// GNOME's spelling: `<Super>f`, `<Control>quotedbl`, `<Super><Shift>F2`.
    pub fn gnome(&self) -> String {
        let mut out = String::new();
        for m in &self.mods {
            out.push_str(match m {
                Modifier::Super => "<Super>",
                Modifier::Ctrl => "<Control>",
                Modifier::Alt => "<Alt>",
                Modifier::Shift => "<Shift>",
            });
        }
        out.push_str(&gnome_key(&self.key));
        out
    }

    /// KDE's spelling: `Meta+F`, `Ctrl+"`, `Meta+Shift+F2`.
    pub fn kde(&self) -> String {
        let mut parts: Vec<String> = self
            .mods
            .iter()
            .map(|m| {
                match m {
                    Modifier::Super => "Meta",
                    Modifier::Ctrl => "Ctrl",
                    Modifier::Alt => "Alt",
                    Modifier::Shift => "Shift",
                }
                .to_owned()
            })
            .collect();
        parts.push(kde_key(&self.key));
        parts.join("+")
    }

    /// Read GNOME's spelling back. `None` for a spelling this crate does not know.
    pub fn from_gnome(s: &str) -> Option<Key> {
        let s = s.trim();
        let mut mods = Vec::new();
        let mut rest = s;
        while let Some(close) = rest.strip_prefix('<').and_then(|r| r.find('>')) {
            let name = &rest[1..close + 1];
            let m = match name.to_ascii_lowercase().as_str() {
                "super" | "mod4" | "meta" | "hyper" => Modifier::Super,
                "control" | "ctrl" | "primary" => Modifier::Ctrl,
                "alt" | "mod1" => Modifier::Alt,
                "shift" => Modifier::Shift,
                _ => return None,
            };
            if !mods.contains(&m) {
                mods.push(m);
            }
            rest = &rest[close + 2..];
        }
        if rest.is_empty() {
            return None;
        }
        mods.sort();
        // A keysym name for punctuation, or a name from the table, or a key as is.
        let key = NAMED
            .iter()
            .find(|(_, sym)| *sym == rest)
            .map(|(c, _)| c.to_string())
            .or_else(|| {
                SPECIAL
                    .iter()
                    .find(|(_, sym, _)| *sym == rest)
                    .map(|(ours, _, _)| (*ours).to_owned())
            })
            .or_else(|| normalise(rest))?;
        Some(Key { mods, key })
    }

    /// Read KDE's spelling back. `None` for a spelling this crate does not know.
    pub fn from_kde(s: &str) -> Option<Key> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let (mods_part, key_part) = if let Some(head) = s.strip_suffix("++") {
            (head, "+")
        } else if s.ends_with('+') {
            return None;
        } else {
            match s.rfind('+') {
                None => ("", s),
                Some(at) => (&s[..at], &s[at + 1..]),
            }
        };
        let mut mods = Vec::new();
        for m in mods_part.split('+').filter(|m| !m.is_empty()) {
            let m = match m {
                "Meta" | "Super" => Modifier::Super,
                "Ctrl" => Modifier::Ctrl,
                "Alt" => Modifier::Alt,
                "Shift" => Modifier::Shift,
                _ => return None,
            };
            if !mods.contains(&m) {
                mods.push(m);
            }
        }
        mods.sort();
        let key = SPECIAL
            .iter()
            .find(|(_, _, qt)| *qt == key_part)
            .map(|(ours, _, _)| (*ours).to_owned())
            .or_else(|| normalise(key_part))?;
        Some(Key { mods, key })
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for m in &self.mods {
            f.write_str(match m {
                Modifier::Super => "super+",
                Modifier::Ctrl => "ctrl+",
                Modifier::Alt => "alt+",
                Modifier::Shift => "shift+",
            })?;
        }
        f.write_str(&self.key)
    }
}

impl std::str::FromStr for Key {
    type Err = KeyError;
    fn from_str(s: &str) -> Result<Key, KeyError> {
        Key::parse(s)
    }
}

/// The key part in the neutral spelling, or `None` when there is nothing there.
fn normalise(k: &str) -> Option<String> {
    let k = k.trim();
    let mut chars = k.chars();
    match (chars.next(), chars.next()) {
        (None, _) => None,
        // One character: a letter folds to lowercase, anything else is itself.
        (Some(c), None) => Some(if c == ' ' {
            "space".to_owned()
        } else {
            c.to_lowercase().collect()
        }),
        _ => {
            let lower = k.to_ascii_lowercase();
            if let Some(n) = lower.strip_prefix('f')
                && !n.is_empty()
                && n.chars().all(|c| c.is_ascii_digit())
            {
                return Some(format!("F{n}"));
            }
            if SPECIAL.iter().any(|(ours, _, _)| *ours == lower) {
                return Some(lower);
            }
            // `enter` and `esc` are what people type; `pgup` too.
            Some(match lower.as_str() {
                "enter" => "return".to_owned(),
                "esc" => "escape".to_owned(),
                "del" => "delete".to_owned(),
                "ins" => "insert".to_owned(),
                "pgup" => "pageup".to_owned(),
                "pgdown" | "pgdn" => "pagedown".to_owned(),
                _ => lower,
            })
        }
    }
}

fn gnome_key(k: &str) -> String {
    let mut chars = k.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return NAMED
            .iter()
            .find(|(ch, _)| *ch == c)
            .map(|(_, sym)| (*sym).to_owned())
            .unwrap_or_else(|| c.to_string());
    }
    SPECIAL
        .iter()
        .find(|(ours, _, _)| *ours == k)
        .map(|(_, sym, _)| (*sym).to_owned())
        .unwrap_or_else(|| k.to_owned())
}

fn kde_key(k: &str) -> String {
    let mut chars = k.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return c.to_uppercase().collect();
    }
    SPECIAL
        .iter()
        .find(|(ours, _, _)| *ours == k)
        .map(|(_, _, qt)| (*qt).to_owned())
        .unwrap_or_else(|| k.to_owned())
}
