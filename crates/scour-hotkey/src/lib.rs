//! The desktop's key that opens Scour: what it is, setting it, clearing it.
//!
//! GNOME keeps custom keybindings in dconf and is spoken to through `gsettings`;
//! KDE keeps them in `kglobalshortcutsrc`, through `kwriteconfig`. Anywhere
//! else, and inside a Flatpak, a program cannot bind: it can only say which
//! command to bind, and the face says it.

mod desktop;
mod gnome;
mod hotkey;
mod kde;
mod key;

pub use desktop::{Desktop, FLATPAK_APP, Tools};
pub use hotkey::{Applies, Error, Hotkey, Status};
pub use key::{Key, KeyError, Modifier};
