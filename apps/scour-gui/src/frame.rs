//! A press let go of on the window's frame.
//!
//! On GNOME, winit draws the title bar and the resize edges itself, as
//! subsurfaces of the window. A press that starts in the window and is let go
//! of on one of them — the scrollbar's thumb dragged a pixel past the right
//! edge — has its release handed to the frame, and Slint is told only that the
//! pointer left. The thumb then stays held: every return of the pointer drags
//! the list until the next click. Measured with `WAYLAND_DEBUG=client`: the
//! button went down on the window's surface, the pointer entered the right
//! edge's surface at x 1.6, and the button came up there.
//!
//! Under the press the compositor keeps the pointer on this client, so on
//! Wayland the pointer leaving while the button is down means it went onto
//! the frame. The press is let go of then, at the last place Slint saw.

use std::cell::Cell;
use std::rc::Rc;

use i_slint_backend_winit::winit::event::{ElementState, MouseButton, WindowEvent};
use i_slint_backend_winit::{EventResult, WinitWindowAccessor};

/// Attach once. A no-op off Wayland, where the release is not lost.
pub fn let_go_on_the_frame(w: &slint::Window) {
    // Asked at the first event rather than here: the winit window may not
    // exist yet when this is attached.
    let wayland: Cell<Option<bool>> = Cell::new(None);
    let held = Rc::new(Cell::new(false));
    let at = Rc::new(Cell::new(slint::LogicalPosition::default()));
    w.on_winit_window_event(move |w, event| {
        // Windows' own motion preference is read again on focus, after this
        // filter; less motion is asked for once Slint has done that.
        #[cfg(windows)]
        if matches!(event, WindowEvent::Focused(true)) {
            slint::Timer::single_shot(std::time::Duration::ZERO, crate::start::still_motion);
        }
        let on = wayland.get().unwrap_or_else(|| {
            let on = on_wayland(w);
            wayland.set(Some(on));
            on
        });
        if !on {
            return EventResult::Propagate;
        }
        match event {
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => held.set(*state == ElementState::Pressed),
            WindowEvent::CursorMoved { position, .. } => {
                at.set(slint::LogicalPosition::from_physical(
                    slint::PhysicalPosition::new(position.x as i32, position.y as i32),
                    w.scale_factor(),
                ));
            }
            WindowEvent::CursorLeft { .. } if held.get() => {
                held.set(false);
                crate::trace("frame: a press left for the frame, let go");
                w.dispatch_event(slint::platform::WindowEvent::PointerReleased {
                    position: at.get(),
                    button: slint::platform::PointerEventButton::Left,
                });
            }
            _ => {}
        }
        EventResult::Propagate
    });
}

fn on_wayland(w: &slint::Window) -> bool {
    use i_slint_backend_winit::winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    w.with_winit_window(|ww| {
        matches!(
            ww.display_handle().map(|d| d.as_raw()),
            Ok(RawDisplayHandle::Wayland(_))
        )
    })
    .unwrap_or(false)
}
