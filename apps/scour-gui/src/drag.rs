//! Rows dragged out of the window: Slint notices the drag, `scour-drag`
//! hands it to the display server, which the toolkit cannot do itself.

use std::cell::RefCell;
use std::path::Path;

use slint::ComponentHandle;

thread_local! {
    // Leaked, never dropped: it holds the window's own connection, which is
    // gone by the time a thread-local would be.
    static DRAG: RefCell<Option<&'static mut scour_drag::Dragger>> = const { RefCell::new(None) };
    static PUMP: slint::Timer = slint::Timer::default();
    static IDLE: slint::Timer = slint::Timer::default();
}

/// Attach once the window is on screen, before the first press: on Wayland
/// a pointer bound during a press never hears of that press. Tried until it
/// takes — the window may not exist yet when this is first called.
pub fn prepare<C: ComponentHandle + 'static>(w: &C) {
    attach_once(w.window());
    let weak = w.as_weak();
    IDLE.with(|t| {
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(2),
            move || {
                let Some(w) = weak.upgrade() else { return };
                attach_once(w.window());
                // Wayland's pointer hears every motion, which waits until
                // read: read every two seconds, a few microseconds each.
                let pumping = with_drag(|d| {
                    d.pump();
                    d.needs_pumping()
                });
                if pumping == Some(false) {
                    IDLE.with(slint::Timer::stop);
                }
            },
        )
    });
}

/// Drag these files out of the window, where `x`, `y` is the pointer.
/// Started just after the press's handler returns: on Windows the drag runs
/// a loop of its own until the drop. `ended` hears whether a target took
/// them, once the drag is over; never, when it did not start.
pub fn files<C: ComponentHandle + 'static>(
    w: &C,
    paths: Vec<String>,
    x: f32,
    y: f32,
    ended: impl FnOnce(bool) + 'static,
) {
    let weak = w.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        let Some(w) = weak.upgrade() else { return };
        let paths: Vec<&Path> = paths.iter().map(Path::new).collect();
        crate::trace("drag: starting");
        let started = with_drag(|d| d.files(&paths));
        match started {
            Some(Ok(())) => crate::trace(&format!("drag: {} file(s)", paths.len())),
            Some(Err(why)) => {
                crate::trace(&format!("drag: not started: {why:?}"));
                return;
            }
            None => {
                crate::trace("drag: no way to drag on this desktop");
                return;
            }
        }
        // The release went to the drop target, so the row's press is let go
        // here or it stays held.
        w.window()
            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position: slint::LogicalPosition::new(x, y),
                button: slint::platform::PointerEventButton::Left,
            });
        // What the drop target asks for after the drop is answered from here.
        let ended = RefCell::new(Some(ended));
        PUMP.with(|t| {
            t.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(30),
                move || {
                    let going = with_drag(|d| {
                        d.pump();
                        d.dragging()
                    });
                    if going != Some(true) {
                        PUMP.with(slint::Timer::stop);
                        let taken = with_drag(|d| d.dropped()).unwrap_or(false);
                        crate::trace(&format!("drag: over, taken: {taken}"));
                        if let Some(f) = ended.borrow_mut().take() {
                            f(taken);
                        }
                    }
                },
            )
        });
    });
}

fn with_drag<T>(f: impl FnOnce(&mut scour_drag::Dragger) -> T) -> Option<T> {
    DRAG.with(|cell| cell.borrow_mut().as_mut().map(|d| f(d)))
}

fn attach_once(w: &slint::Window) {
    DRAG.with(|cell| {
        let mut held = cell.borrow_mut();
        if held.is_none() {
            *held = attach(w).map(|d| &mut *Box::leak(Box::new(d)));
        }
    });
}

/// The window's own handles, made into a drag where the desktop has one.
fn attach(w: &slint::Window) -> Option<scour_drag::Dragger> {
    use i_slint_backend_winit::WinitWindowAccessor;
    use i_slint_backend_winit::winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
    let found = w.with_winit_window(|ww| {
        let display = ww.display_handle().ok()?.as_raw();
        let window = ww.window_handle().ok()?.as_raw();
        // SAFETY: both are this window's, and it lives as long as the process.
        let drag = unsafe { scour_drag::Dragger::new(display, window) };
        crate::trace(&format!("drag: attached: {}", drag.is_some()));
        drag
    });
    found.flatten()
}
