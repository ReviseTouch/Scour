//! Placing the marks, and handing the descriptor over.
//!
//! **Linux only, and the gate is here rather than on the crate.** What this
//! program does is `fanotify_init` + `FAN_MARK_FILESYSTEM`, drop the
//! privilege, and exec `scourd` with the descriptor — a Linux interface with
//! no counterpart elsewhere. A crate-level `#![cfg]` made the whole file
//! vanish off Linux, `main` with it, and the workspace stopped building for
//! the platforms the README names. So the body lives in `linux.rs` and this
//! file is the door: one way in on Linux, one sentence out everywhere else.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod mountpoint;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

/// Off Linux there is nothing to place and nothing to hand over.
///
/// Windows watches through `ReadDirectoryChangesW` and macOS through
/// `FSEvents`; neither needs a privileged helper, so `scourd` is started
/// directly there. Saying that is more use than a binary that will not link.
#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "scour-watch: Linux only — it places fanotify marks, which no other \
         platform has.\nStart `scourd` directly here; it watches without a helper."
    );
    std::process::ExitCode::from(2)
}
