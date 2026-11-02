//! Rows dragged out of the window: Slint notices the drag, `scour-drag`
//! hands it to the display server, which the toolkit cannot do itself.

use slint::ComponentHandle;

use crate::MainWindow;

/// Start dragging these files out of the window, where `x`, `y` is the
/// pointer; false where this desktop gives no way to.
pub fn files(w: &MainWindow, paths: &[String], x: f32, y: f32) -> bool {
    let started = start(w, paths);
    if started {
        // The compositor has the pointer now and the release goes to the drop
        // target, so the row's press is let go here or it stays held.
        w.window()
            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position: slint::LogicalPosition::new(x, y),
                button: slint::platform::PointerEventButton::Left,
            });
    }
    started
}

#[cfg(target_os = "linux")]
thread_local! {
    // Leaked, never dropped: it holds the window's own connection, which is
    // gone by the time a thread-local would be.
    static WAYLAND: std::cell::RefCell<Option<&'static mut scour_drag::Wayland>> =
        const { std::cell::RefCell::new(None) };
    static PUMP: slint::Timer = slint::Timer::default();
    static IDLE: slint::Timer = slint::Timer::default();
}

/// Attach once the window is on screen, before the first press: a pointer
/// bound during a press never hears of that press. Tried until it takes —
/// the window may not exist yet when this is first called.
pub fn prepare(w: &MainWindow) {
    #[cfg(target_os = "linux")]
    {
        attach_once(w);
        // Its pointer hears every motion, which waits in its queue until
        // read: read every two seconds, a few microseconds each.
        let weak = w.as_weak();
        IDLE.with(|t| {
            t.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_secs(2),
                move || {
                    if let Some(w) = weak.upgrade() {
                        attach_once(&w);
                    }
                    WAYLAND.with(|cell| {
                        if let Some(drag) = cell.borrow_mut().as_mut() {
                            drag.pump();
                        }
                    })
                },
            )
        });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = w;
}

#[cfg(target_os = "linux")]
fn attach_once(w: &MainWindow) {
    WAYLAND.with(|cell| {
        let mut held = cell.borrow_mut();
        if held.is_none() {
            *held = attach(w).map(|d| &mut *Box::leak(Box::new(d)));
        }
    });
}

#[cfg(target_os = "linux")]
fn start(_: &MainWindow, paths: &[String]) -> bool {
    use std::path::Path;
    WAYLAND.with(|cell| {
        let mut held = cell.borrow_mut();
        let Some(drag) = held.as_mut() else {
            crate::trace("drag: no Wayland connection to drag on");
            return false;
        };
        let paths: Vec<&Path> = paths.iter().map(Path::new).collect();
        if let Err(why) = drag.files(&paths) {
            crate::trace(&format!("drag: not started: {why:?}"));
            return false;
        }
        crate::trace(&format!("drag: {} file(s)", paths.len()));
        // The drop target asks for the files after the drop; answered from
        // here until the drag is over.
        PUMP.with(|t| {
            t.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(30),
                || {
                    WAYLAND.with(|cell| {
                        if let Some(drag) = cell.borrow_mut().as_mut() {
                            drag.pump();
                            if !drag.dragging() {
                                PUMP.with(slint::Timer::stop);
                            }
                        }
                    });
                },
            );
        });
        true
    })
}

/// The window's Wayland connection and surface, when it has them: under X11
/// there are none, and nothing is dragged.
#[cfg(target_os = "linux")]
fn attach(w: &MainWindow) -> Option<scour_drag::Wayland> {
    use i_slint_backend_winit::WinitWindowAccessor;
    use i_slint_backend_winit::winit::raw_window_handle::{
        HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
    };
    let found = w.window().with_winit_window(|ww| {
        let display = match ww.display_handle().ok()?.as_raw() {
            RawDisplayHandle::Wayland(d) => d.display,
            _ => return None,
        };
        let surface = match ww.window_handle().ok()?.as_raw() {
            RawWindowHandle::Wayland(s) => s.surface,
            _ => return None,
        };
        // SAFETY: both are this window's, and it lives as long as the process.
        let drag = unsafe { scour_drag::Wayland::new(display.as_ptr(), surface.as_ptr()) };
        crate::trace(&format!("drag: attached to Wayland: {}", drag.is_some()));
        drag
    });
    found.flatten()
}

#[cfg(not(target_os = "linux"))]
fn start(_: &MainWindow, _: &[String]) -> bool {
    false
}
