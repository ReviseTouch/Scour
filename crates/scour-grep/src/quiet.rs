//! A reading thread that gives way: to whoever is at the desk for the CPU,
//! and to every other reader for the disk. For the thread that calls it only,
//! so it ends with the reading.

/// Lower this thread's disk and CPU priority as far as the system allows.
pub(crate) fn give_way() {
    sys::give_way();
}

#[cfg(target_os = "linux")]
mod sys {
    /// `IOPRIO_CLASS_IDLE << IOPRIO_CLASS_SHIFT`; who 0 is this thread.
    const IDLE: libc::c_long = 3 << 13;
    const WHO_PROCESS: libc::c_long = 1;

    pub(super) fn give_way() {
        // SAFETY: plain system calls on this thread; failure leaves it as it was.
        unsafe {
            libc::syscall(libc::SYS_ioprio_set, WHO_PROCESS, 0 as libc::c_long, IDLE);
            libc::setpriority(libc::PRIO_PROCESS, 0, 10);
        }
    }
}

#[cfg(target_os = "macos")]
mod sys {
    const IOPOL_TYPE_DISK: libc::c_int = 0;
    const IOPOL_SCOPE_THREAD: libc::c_int = 1;
    const IOPOL_THROTTLE: libc::c_int = 3;

    unsafe extern "C" {
        fn setiopolicy_np(
            kind: libc::c_int,
            scope: libc::c_int,
            policy: libc::c_int,
        ) -> libc::c_int;
    }

    pub(super) fn give_way() {
        // SAFETY: as above.
        unsafe {
            setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_THREAD, IOPOL_THROTTLE);
            libc::setpriority(libc::PRIO_PROCESS, 0, 10);
        }
    }
}

#[cfg(windows)]
mod sys {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    };

    /// Background mode lowers the thread's I/O and memory priority as well.
    pub(super) fn give_way() {
        // SAFETY: the pseudo-handle of this thread.
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod sys {
    pub(super) fn give_way() {}
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn a_thread_that_gives_way_is_idle_and_the_next_is_not() {
        let class = || unsafe { libc::syscall(libc::SYS_ioprio_get, 1 as libc::c_long, 0 as libc::c_long) } >> 13;
        std::thread::spawn(move || {
            super::give_way();
            assert_eq!(class(), 3);
        })
        .join()
        .unwrap();
        std::thread::spawn(move || assert_ne!(class(), 3))
            .join()
            .unwrap();
    }
}
