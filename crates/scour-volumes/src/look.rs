//! Asking the machine it runs on.

use crate::volume::Volume;

/// Every volume mounted now, in the system's own order.
pub fn look() -> Vec<Volume> {
    #[cfg(target_os = "linux")]
    return crate::linux::look();
    #[cfg(windows)]
    return crate::windows::look();
    // macOS and the rest: nothing yet — `/Volumes` and `getmntinfo` are the
    // way in when the time comes. No volumes is the same as today.
    #[cfg(not(any(target_os = "linux", windows)))]
    return Vec::new();
}
