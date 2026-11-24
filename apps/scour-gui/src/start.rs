//! Starting: one window per desktop, a crash written down, a display found.

#[cfg(unix)]
use super::*;
#[cfg(unix)]
use slint::ComponentHandle;

/// Write the next panic to a file. `force_capture` is deliberate: backtraces are
/// off unless `RUST_BACKTRACE` is set, and nobody sets it before a crash.
pub(super) fn crash_log(state: &std::path::Path) {
    let file = state.join("gui-crash.log");
    let _ = std::fs::create_dir_all(state);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let when = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let where_ = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "?".into());
        let what = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "?".into());
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| "-".into());
        let note = format!(
            "\n=== scour-gui {} · unix {when}\n\
             at        {where_}\n\
             message   {what}\n\
             renderer  SLINT_BACKEND={} \n\
             display   WAYLAND_DISPLAY={} DISPLAY={}\n\
             backtrace\n{}\n",
            env!("CARGO_PKG_VERSION"),
            env("SLINT_BACKEND"),
            env("WAYLAND_DISPLAY"),
            env("DISPLAY"),
            std::backtrace::Backtrace::force_capture(),
        );
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
        {
            let _ = f.write_all(note.as_bytes());
        }
        eprint!("{note}");
        previous(info);
    }));
}

/// Some terminals (Ptyxis among them) start their programs without
/// `WAYLAND_DISPLAY`, so a window opened from the terminal face fell to
/// XWayland, where winit panics in `get_frame_extents_heuristic`. A compositor
/// that answers on its socket is the session's, whatever the variable says.
/// Called before any thread exists, which is what makes `set_var` sound.
#[cfg(target_os = "linux")]
pub(super) fn wayland_found_again() {
    let unset = std::env::var_os("WAYLAND_DISPLAY").is_none_or(|v| v.is_empty());
    if !unset || std::env::var_os("SLINT_BACKEND").is_some() {
        return;
    }
    let Some(run) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return;
    };
    let run = std::path::PathBuf::from(run);
    for name in ["wayland-0", "wayland-1"] {
        if std::os::unix::net::UnixStream::connect(run.join(name)).is_ok() {
            // SAFETY: the process has one thread at this point.
            unsafe { std::env::set_var("WAYLAND_DISPLAY", name) };
            return;
        }
    }
}

/// The lock a running window holds: a socket beside the service's, removed on
/// exit.
#[cfg(unix)]
pub(super) struct Claim {
    pub(super) listener: std::os::unix::net::UnixListener,
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl Drop for Claim {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Claim the one window, or tell the one that has it to show itself.
///
/// `None` means another window answered and this one should leave. A socket
/// file nobody answers on is a crash's leftover and is taken over.
#[cfg(unix)]
pub(super) fn only_one(service_socket: &str) -> Option<Claim> {
    use std::io::Write;
    use std::os::unix::net::{UnixListener, UnixStream};
    let path = std::path::PathBuf::from(format!("{service_socket}.gui"));
    if let Ok(mut other) = UnixStream::connect(&path) {
        let _ = other.write_all(b"show\n");
        return None;
    }
    let _ = std::fs::remove_file(&path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match UnixListener::bind(&path) {
        Ok(listener) => Some(Claim { listener, path }),
        // Without a lock there is still a window; there may just be two.
        Err(_) => {
            let spare = std::env::temp_dir().join(format!("scour-gui-{}.sock", std::process::id()));
            let _ = std::fs::remove_file(&spare);
            let listener = UnixListener::bind(&spare).ok()?;
            Some(Claim {
                listener,
                path: spare,
            })
        }
    }
}

/// Every connection on the lock is a second start asking for the window.
#[cfg(unix)]
pub(super) fn answer_the_next_start(
    listener: std::os::unix::net::UnixListener,
    weak: slint::Weak<MainWindow>,
) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            drop(stream);
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(w) = weak.upgrade() else { return };
                use i_slint_backend_winit::WinitWindowAccessor;
                w.window().show().ok();
                w.window().with_winit_window(|ww| {
                    ww.set_minimized(false);
                    ww.focus_window();
                });
            });
        }
    });
}
