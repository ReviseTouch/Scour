//! Waiting for a disk to come or go, without asking the whole machine again.

use std::time::Duration;

/// Something to wait on until the set of mounted volumes may have changed.
/// "May": an answer of yes is a reason to [`crate::look`] again, not a diff.
pub struct Changes {
    #[cfg(target_os = "linux")]
    table: Option<std::fs::File>,
    #[cfg(windows)]
    letters: u32,
}

impl std::fmt::Debug for Changes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Changes").finish_non_exhaustive()
    }
}

impl Default for Changes {
    fn default() -> Self {
        Self::new()
    }
}

impl Changes {
    /// From now on: what was mounted before this is not a change.
    pub fn new() -> Changes {
        Changes {
            // Held open: the kernel tells this open file about every mount
            // since the last poll of it, so nothing between two waits is lost.
            #[cfg(target_os = "linux")]
            table: std::fs::File::open("/proc/self/mountinfo").ok(),
            #[cfg(windows)]
            letters: crate::windows::letters(),
        }
    }

    /// Block until something is mounted or unmounted, or `timeout` passes;
    /// true for the first.
    pub fn wait(&mut self, timeout: Duration) -> bool {
        #[cfg(target_os = "linux")]
        return self.wait_linux(timeout);
        #[cfg(windows)]
        return self.wait_windows(timeout);
        // macOS and the rest: no volumes are listed there yet, so none change.
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            std::thread::sleep(timeout);
            false
        }
    }

    #[cfg(target_os = "linux")]
    fn wait_linux(&mut self, timeout: Duration) -> bool {
        use std::os::fd::AsRawFd;
        let Some(table) = &self.table else {
            std::thread::sleep(timeout);
            return false;
        };
        let mut fd = libc::pollfd {
            fd: table.as_raw_fd(),
            events: libc::POLLPRI,
            revents: 0,
        };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: one pollfd, as counted.
        let n = unsafe { libc::poll(&mut fd, 1, ms) };
        n > 0 && fd.revents & (libc::POLLPRI | libc::POLLERR) != 0
    }

    /// Windows says nothing when a letter appears, so the letters are compared
    /// every two seconds: one call, which touches no drive.
    #[cfg(windows)]
    fn wait_windows(&mut self, timeout: Duration) -> bool {
        let began = std::time::Instant::now();
        loop {
            let now = crate::windows::letters();
            if now != self.letters {
                self.letters = now;
                return true;
            }
            let left = timeout.saturating_sub(began.elapsed());
            if left.is_zero() {
                return false;
            }
            std::thread::sleep(left.min(Duration::from_secs(2)));
        }
    }
}
