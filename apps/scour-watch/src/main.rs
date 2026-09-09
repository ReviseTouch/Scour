//! Placing the marks, and handing the descriptor over.
//!
//! Linux only, and the gate is here rather than on the crate: a crate-level
//! `#![cfg]` would take `main` with it and stop the workspace building for the
//! platforms the README names. The body is in `linux.rs`.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod mountpoint;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

/// Off Linux there is nothing to place and nothing to hand over: Windows watches
/// through `ReadDirectoryChangesW` and macOS through `FSEvents`, with no helper.
#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "scour-watch: Linux only — it places fanotify marks, which no other \
         platform has.\nStart `scourd` directly here; it watches without a helper."
    );
    std::process::ExitCode::from(2)
}
