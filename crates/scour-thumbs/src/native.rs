//! Direct in-memory system pictures on platforms whose OS renders thumbnails
//! itself: Windows (`IShellItemImageFactory`) and macOS (`QuickLook.framework`).
//! On Linux the desktop uses freedesktop PNG caches instead (`cache` / `make`).

use std::path::Path;

pub use crate::shell::Picture;

/// True on platforms that provide direct in-memory system pictures.
pub const fn available() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

/// The OS's own rendered picture of `path`, at most `px` on its longer side.
/// May decode on a miss, so callers must invoke this off the UI thread.
pub fn picture(path: &Path, px: u32) -> Option<Picture> {
    #[cfg(windows)]
    {
        crate::shell::picture(path, px)
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::picture(path, px)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (path, px);
        None
    }
}

/// Cache-only lookup where the OS supports it without running a slow handler.
pub fn picture_cached(path: &Path, px: u32) -> Option<Picture> {
    #[cfg(windows)]
    {
        crate::shell::picture_cached(path, px)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, px);
        None
    }
}

/// The OS's own rendered picture encoded as PNG bytes, for HTTP callers.
pub fn png(path: &Path, px: u32) -> Option<Vec<u8>> {
    let pic = picture(path, px)?;
    crate::png::encode_rgba(pic.width, pic.height, &pic.rgba)
}
