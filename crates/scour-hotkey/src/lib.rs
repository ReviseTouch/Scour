//! The desktop's key that opens Scour: what it is, setting it, clearing it.
//!
//! GNOME keeps custom keybindings in dconf and is spoken to through `gsettings`;
//! KDE keeps them in `kglobalshortcutsrc`, through `kwriteconfig`. On Windows
//! the key is the Start menu shortcut's own, which Explorer registers. A Mac
//! has nowhere to write one, so Scour's service holds it ([`listen`]) and a
//! file says which. Anywhere else, and inside a Flatpak, a program cannot
//! bind: it can only say which command to bind, and the face says it.

mod desktop;
mod gnome;
mod hotkey;
mod kde;
mod kept;
mod key;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod windows;

pub use desktop::{Desktop, FLATPAK_APP, Os, Tools};
pub use hotkey::{Applies, Error, Hotkey, Status, WINDOWS_DEFAULT, WINDOWS_REFUSED};
pub use kept::{MAC_DEFAULT, MAC_REFUSED};
pub use key::{Key, KeyError, Modifier};
#[cfg(target_os = "macos")]
pub use mac::listen;
