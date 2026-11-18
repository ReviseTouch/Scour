//! The one door: ask, bind, clear, on whatever desktop this is.

use std::fmt;

use crate::desktop::{Desktop, Tools};
use crate::key::Key;
use crate::{gnome, kde};

/// What the desktop says about Scour's key right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Bound(Key),
    Unbound,
    /// This program cannot bind here. `command` is what a person binds by hand.
    CannotBind {
        command: String,
    },
}

/// When a binding just written starts working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    Now,
    /// KDE reads the file at login, or when `kglobalaccel` is restarted.
    AfterLogin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A desktop this crate cannot speak to.
    Unsupported,
    /// Inside a Flatpak: `gsettings` there reaches nobody's desktop.
    Sandboxed,
    /// The tool ran and refused; its own words.
    Tool(String),
    /// A binding in a spelling this crate does not read.
    Unreadable(String),
    /// A combination this system cannot take, and why — a msgid, which is the
    /// sentence a face shows: Windows has no shortcut on the Windows key.
    Refused(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported => write!(f, "this desktop cannot be bound from here"),
            Error::Sandboxed => write!(f, "a sandboxed program cannot bind a key"),
            Error::Tool(why) => write!(f, "{why}"),
            Error::Unreadable(s) => write!(f, "a binding in a spelling not read here: {s}"),
            Error::Refused(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug)]
pub struct Hotkey {
    tools: Tools,
}

impl Hotkey {
    pub fn detect() -> Hotkey {
        Hotkey::new(Tools::detect())
    }

    pub fn new(tools: Tools) -> Hotkey {
        Hotkey { tools }
    }

    pub fn tools(&self) -> &Tools {
        &self.tools
    }

    pub fn desktop(&self) -> Desktop {
        self.tools.desktop()
    }

    /// Can this program write a binding here at all?
    pub fn can_bind(&self) -> bool {
        matches!(
            self.desktop(),
            Desktop::Gnome | Desktop::Kde | Desktop::Windows | Desktop::Mac
        )
    }

    /// What the key runs, or would run.
    pub fn command(&self) -> String {
        self.tools.command()
    }

    pub fn status(&self) -> Result<Status, Error> {
        let bound = match self.desktop() {
            Desktop::Gnome => gnome::current(self.gsettings()?)?,
            Desktop::Kde => kde::current(self.kreadconfig()?)?,
            Desktop::Windows => windows_current()?,
            Desktop::Mac => crate::kept::current(self.kept()?)?,
            Desktop::Flatpak | Desktop::Other => {
                return Ok(Status::CannotBind {
                    command: self.command(),
                });
            }
        };
        Ok(bound.map_or(Status::Unbound, Status::Bound))
    }

    /// Write `key` as the one that runs [`Hotkey::command`].
    pub fn bind(&self, key: &Key) -> Result<Applies, Error> {
        match self.desktop() {
            Desktop::Gnome => {
                gnome::bind(self.gsettings()?, key, &self.command())?;
                // Without it the key still opens Scour, just not forward.
                if self.tools.shell_extension {
                    let _ = gnome::enable_extension(self.gsettings()?);
                }
                Ok(Applies::Now)
            }
            Desktop::Kde => {
                kde::bind(self.kwriteconfig()?, key)?;
                Ok(Applies::AfterLogin)
            }
            Desktop::Windows => {
                windows_bind(key, std::path::Path::new(&self.command()))?;
                Ok(Applies::Now)
            }
            Desktop::Mac => {
                crate::kept::bind(self.kept()?, key)?;
                Ok(Applies::Now)
            }
            Desktop::Flatpak => Err(Error::Sandboxed),
            Desktop::Other => Err(Error::Unsupported),
        }
    }

    pub fn clear(&self) -> Result<(), Error> {
        match self.desktop() {
            Desktop::Gnome => gnome::clear(self.gsettings()?),
            Desktop::Kde => kde::clear(self.kwriteconfig()?),
            Desktop::Windows => windows_clear(),
            Desktop::Mac => crate::kept::clear(self.kept()?),
            Desktop::Flatpak => Err(Error::Sandboxed),
            Desktop::Other => Err(Error::Unsupported),
        }
    }

    fn kept(&self) -> Result<&std::path::Path, Error> {
        self.tools.kept.as_deref().ok_or(Error::Unsupported)
    }

    fn gsettings(&self) -> Result<&std::path::Path, Error> {
        self.tools.gsettings.as_deref().ok_or(Error::Unsupported)
    }

    fn kwriteconfig(&self) -> Result<&std::path::Path, Error> {
        self.tools.kwriteconfig.as_deref().ok_or(Error::Unsupported)
    }

    fn kreadconfig(&self) -> Result<&std::path::Path, Error> {
        self.tools.kreadconfig.as_deref().ok_or(Error::Unsupported)
    }
}

#[cfg(windows)]
use crate::windows::{bind as windows_bind, clear as windows_clear, current as windows_current};

/// Asked about Windows from somewhere else: nothing here can answer.
#[cfg(not(windows))]
fn windows_current() -> Result<Option<Key>, Error> {
    Err(Error::Unsupported)
}

#[cfg(not(windows))]
fn windows_bind(key: &Key, _target: &std::path::Path) -> Result<(), Error> {
    // What a shortcut cannot carry is the same question on any system.
    key.windows()
        .map(|_| ())
        .ok_or(Error::Refused(WINDOWS_REFUSED))
        .and(Err(Error::Unsupported))
}

#[cfg(not(windows))]
fn windows_clear() -> Result<(), Error> {
    Err(Error::Unsupported)
}

/// The key the Start menu shortcut is given when Scour is installed.
pub const WINDOWS_DEFAULT: &str = "ctrl+alt+f";

/// Why a combination cannot be a Windows shortcut's. A msgid.
pub const WINDOWS_REFUSED: &str = "Windows opens a shortcut on two of Ctrl, Alt and Shift with a letter, a digit, a function key or a named key. The Windows key cannot be used.";
