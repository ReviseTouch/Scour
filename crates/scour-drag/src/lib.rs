//! Dragging files out of a window into another program: a file manager, a
//! mail, a chat, a file chooser. The window's toolkit says when a drag
//! begins; this hands it to the display server, which owns everything after.
//!
//! One type, [`Dragger`], made from the window's raw handles; which platform
//! it is decides the rest. Each platform is a file of its own.

mod dragger;
mod uri;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
mod wayland;
#[cfg(windows)]
mod windows;
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
mod x11;

pub use dragger::{Dragger, Refused};
pub use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
pub use uri::{file_uri, uri_list};
