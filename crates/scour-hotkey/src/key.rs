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

/// Windows' modifier bits in a shortcut's key, and the one that marks a key
/// of the cluster between the letters and the number pad.
const HOTKEYF_SHIFT: u16 = 0x01;
const HOTKEYF_CONTROL: u16 = 0x02;
const HOTKEYF_ALT: u16 = 0x04;
const HOTKEYF_EXT: u16 = 0x08;

/// Named keys as Windows' virtual-key code, whether it is of the extended
/// cluster, and Carbon's key code. Positions, not characters, on a Mac.
const CODES: &[(&str, u8, bool, u32)] = &[
    ("space", 0x20, false, 0x31),
    ("tab", 0x09, false, 0x30),
    ("return", 0x0d, false, 0x24),
    ("escape", 0x1b, false, 0x35),
    ("backspace", 0x08, false, 0x33),
    ("delete", 0x2e, true, 0x75),
    ("home", 0x24, true, 0x73),
    ("end", 0x23, true, 0x77),
    ("pageup", 0x21, true, 0x74),
    ("pagedown", 0x22, true, 0x79),
    ("left", 0x25, true, 0x7b),
    ("up", 0x26, true, 0x7e),
    ("right", 0x27, true, 0x7c),
    ("down", 0x28, true, 0x7d),
];

/// Carbon's key codes for the letters and the digits, `a`–`z` then `0`–`9`,
/// and for F1–F12.
const MAC_LETTERS: [u32; 26] = [
    0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f, 0x23,
    0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d, 0x07, 0x10, 0x06,
];
const MAC_DIGITS: [u32; 10] = [0x1d, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1a, 0x1c, 0x19];
const MAC_F: [u32; 12] = [
    0x7a, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6d, 0x67, 0x6f,
];

/// Carbon's modifier bits.
const MAC_CMD: u32 = 0x0100;
const MAC_SHIFT: u32 = 0x0200;
const MAC_OPTION: u32 = 0x0800;
const MAC_CONTROL: u32 = 0x1000;

impl Key {
    /// A letter, a digit, F1–F12 or a named key: what both Windows and a Mac
    /// can be told by code. `(Windows' code, extended, Carbon's code)`.
    fn codes(&self) -> Option<(u8, bool, u32)> {
        let mut chars = self.key.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return match c {
                'a'..='z' => Some((
                    c.to_ascii_uppercase() as u8,
                    false,
                    MAC_LETTERS[(c as u8 - b'a') as usize],
                )),
                '0'..='9' => Some((c as u8, false, MAC_DIGITS[(c as u8 - b'0') as usize])),
                _ => None,
            };
        }
        if let Some(n) = self
            .key
            .strip_prefix('F')
            .and_then(|n| n.parse::<u8>().ok())
        {
            return (1..=12)
                .contains(&n)
                .then(|| (0x70 + n - 1, false, MAC_F[usize::from(n - 1)]));
        }
        CODES
            .iter()
            .find(|(name, ..)| *name == self.key)
            .map(|&(_, vk, ext, mac)| (vk, ext, mac))
    }

    /// The word a Windows shortcut keeps its key as: the virtual-key code, and
    /// the modifiers above it. `None` for what a shortcut cannot carry — the
    /// Windows key, fewer than two of Ctrl, Alt and Shift, or a key with no
    /// code of its own.
    pub fn windows(&self) -> Option<u16> {
        if self.mods.contains(&Modifier::Super) || self.mods.len() < 2 {
            return None;
        }
        let (vk, ext, _) = self.codes()?;
        let mut flags = if ext { HOTKEYF_EXT } else { 0 };
        for m in &self.mods {
            flags |= match m {
                Modifier::Ctrl => HOTKEYF_CONTROL,
                Modifier::Alt => HOTKEYF_ALT,
                Modifier::Shift => HOTKEYF_SHIFT,
                Modifier::Super => 0,
            };
        }
        Some(flags << 8 | u16::from(vk))
    }

    /// Read a shortcut's word back. `None` for no key, or one not written here.
    pub fn from_windows(word: u16) -> Option<Key> {
        let (vk, flags) = ((word & 0xff) as u8, word >> 8);
        let key = match vk {
            b'A'..=b'Z' => (vk as char).to_ascii_lowercase().to_string(),
            b'0'..=b'9' => (vk as char).to_string(),
            0x70..=0x7b => format!("F{}", vk - 0x70 + 1),
            _ => (*CODES.iter().find(|(_, code, ..)| *code == vk)?)
                .0
                .to_owned(),
        };
        let mut mods = Vec::new();
        for (bit, m) in [
            (HOTKEYF_CONTROL, Modifier::Ctrl),
            (HOTKEYF_ALT, Modifier::Alt),
            (HOTKEYF_SHIFT, Modifier::Shift),
        ] {
            if flags & bit != 0 {
                mods.push(m);
            }
        }
        Some(Key { mods, key })
    }

    /// Carbon's key code and modifiers, for a hot key registered on a Mac —
    /// where `super` is Command. `None` without Command, Control or Option, or
    /// for a key with no code of its own.
    pub fn mac(&self) -> Option<(u32, u32)> {
        let (_, _, code) = self.codes()?;
        let mut mods = 0;
        for m in &self.mods {
            mods |= match m {
                Modifier::Super => MAC_CMD,
                Modifier::Ctrl => MAC_CONTROL,
                Modifier::Alt => MAC_OPTION,
                Modifier::Shift => MAC_SHIFT,
            };
        }
        (mods & !MAC_SHIFT != 0).then_some((code, mods))
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
