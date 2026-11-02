//! The one door: a window's handles in, a drag out, whatever the platform.

use std::path::Path;

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

/// Why a drag did not start.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// This platform, or this display server, offers no way to.
    Unsupported,
    /// No press is held: a drag has to begin on one.
    NoPress,
    /// The connection to the display server failed.
    Broken,
}

/// A window's drag, for as long as the window lives.
#[derive(Debug)]
pub struct Dragger {
    inner: Inner,
}

#[derive(Debug)]
enum Inner {
    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    ))]
    Wayland(Box<crate::wayland::Wayland>),
    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    ))]
    X11(crate::x11::X11),
    #[cfg(windows)]
    Windows(crate::windows::Windows),
    #[cfg(target_os = "macos")]
    Mac(crate::macos::Mac),
    /// Never made: it keeps the enum inhabited where no platform is built in.
    #[allow(dead_code)]
    Nothing,
}

impl Dragger {
    /// The drag for this window, or nothing where the platform has none here.
    ///
    /// # Safety
    ///
    /// The handles must be this window's, valid for as long as the value lives.
    #[allow(unused_variables, unreachable_code)]
    pub unsafe fn new(display: RawDisplayHandle, window: RawWindowHandle) -> Option<Dragger> {
        let inner = match (display, window) {
            #[cfg(all(
                unix,
                not(any(target_os = "macos", target_os = "ios", target_os = "android"))
            ))]
            (RawDisplayHandle::Wayland(d), RawWindowHandle::Wayland(w)) => {
                // SAFETY: the caller vouches for both.
                let it =
                    unsafe { crate::wayland::Wayland::new(d.display.as_ptr(), w.surface.as_ptr()) };
                Inner::Wayland(Box::new(it?))
            }
            #[cfg(all(
                unix,
                not(any(target_os = "macos", target_os = "ios", target_os = "android"))
            ))]
            (RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_), _) => {
                Inner::X11(crate::x11::X11::new()?)
            }
            #[cfg(windows)]
            (_, RawWindowHandle::Win32(_)) => Inner::Windows(crate::windows::Windows),
            #[cfg(target_os = "macos")]
            (_, RawWindowHandle::AppKit(w)) => {
                // SAFETY: the caller vouches for the view.
                Inner::Mac(unsafe { crate::macos::Mac::new(w.ns_view.as_ptr()) }?)
            }
            _ => return None,
        };
        Some(Dragger { inner })
    }

    /// Start dragging these files, on the press held now. On Windows this
    /// returns when the drop is over; elsewhere at once.
    #[allow(unused_variables)]
    pub fn files(&mut self, paths: &[&Path]) -> Result<(), Refused> {
        match &mut self.inner {
            #[cfg(all(
                unix,
                not(any(target_os = "macos", target_os = "ios", target_os = "android"))
            ))]
            Inner::Wayland(it) => it.files(paths),
            #[cfg(all(
                unix,
                not(any(target_os = "macos", target_os = "ios", target_os = "android"))
            ))]
            Inner::X11(it) => it.files(paths),
            #[cfg(windows)]
            Inner::Windows(it) => it.files(paths),
            #[cfg(target_os = "macos")]
            Inner::Mac(it) => it.files(paths),
            Inner::Nothing => Err(Refused::Unsupported),
        }
    }

    /// Whether [`Dragger::pump`] has to be called now and then even while no
    /// drag is under way: on Wayland, where this keeps a queue of its own.
    pub fn needs_pumping(&self) -> bool {
        match &self.inner {
            #[cfg(all(
                unix,
                not(any(target_os = "macos", target_os = "ios", target_os = "android"))
            ))]
            Inner::Wayland(_) => true,
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }

    /// Read what has arrived for the drag and answer it.
    pub fn pump(&mut self) {
        match &mut self.inner {
            #[cfg(all(
                unix,
                not(any(target_os = "macos", target_os = "ios", target_os = "android"))
            ))]
            Inner::Wayland(it) => it.pump(),
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }

    /// A drag is under way: neither dropped nor cancelled yet.
    pub fn dragging(&self) -> bool {
        match &self.inner {
            #[cfg(all(
                unix,
                not(any(target_os = "macos", target_os = "ios", target_os = "android"))
            ))]
            Inner::Wayland(it) => it.dragging(),
            #[cfg(all(
                unix,
                not(any(target_os = "macos", target_os = "ios", target_os = "android"))
            ))]
            Inner::X11(it) => it.dragging(),
            #[cfg(target_os = "macos")]
            Inner::Mac(it) => it.dragging(),
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }
}
