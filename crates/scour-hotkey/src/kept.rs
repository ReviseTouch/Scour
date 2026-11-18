//! A key this program holds itself, kept in a file: a Mac has no settings a
//! program may write a global key into, so Scour's own service registers one
//! ([`crate::listen`]) and this file says which. No file is the key Scour comes
//! with; an empty one is "none".

use std::path::Path;

use crate::hotkey::Error;
use crate::key::Key;

/// The key a Mac opens Scour with until somebody chooses another.
pub const MAC_DEFAULT: &str = "super+shift+f";

/// Why a combination cannot be registered on a Mac. A msgid.
pub const MAC_REFUSED: &str = "A key here needs Command, Control or Option with it, and has to be a letter, a digit, a function key or a named key such as space.";

pub fn current(file: &Path) -> Result<Option<Key>, Error> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => MAC_DEFAULT.to_owned(),
        Err(e) => return Err(Error::Tool(format!("{}: {e}", file.display()))),
    };
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    // Anything parses as a key's name; only what can be registered is one.
    match Key::parse(text) {
        Ok(key) if key.mac().is_some() => Ok(Some(key)),
        _ => Err(Error::Unreadable(text.to_owned())),
    }
}

pub fn bind(file: &Path, key: &Key) -> Result<(), Error> {
    if key.mac().is_none() {
        return Err(Error::Refused(MAC_REFUSED));
    }
    write(file, &key.to_string())
}

pub fn clear(file: &Path) -> Result<(), Error> {
    write(file, "")
}

fn write(file: &Path, text: &str) -> Result<(), Error> {
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(file, text).map_err(|e| Error::Tool(format!("{}: {e}", file.display())))
}
