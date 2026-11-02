//! Dragging files out of a window into another program: a file manager, a
//! mail, a chat. The window's toolkit says when a drag begins; this hands it
//! to the display server, which owns everything after.

mod uri;
#[cfg(target_os = "linux")]
mod wayland;

pub use uri::{file_uri, uri_list};

#[cfg(target_os = "linux")]
pub use wayland::{Refused, Wayland};
