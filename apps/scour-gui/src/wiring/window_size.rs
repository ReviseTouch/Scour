//! The window's own size.

use super::*;

pub(crate) fn wire(wiring: &Wiring) {
    let Wiring {
        window,
        link,
        opened_at,
        ..
    } = *wiring;
    // Kept under `view.slint.size`, as the page keeps its own under
    // `view.web`. Written once the drag stops, never per frame.
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        // What the settings already say, so an untouched window writes nothing.
        let saved = Rc::new(std::cell::Cell::new(*opened_at));
        // Asked once, as soon as there is a real window behind the Slint one —
        // which is the first moment a monitor can be named at all, and why this
        // is not done at birth.
        let asked = Rc::new(std::cell::Cell::new(false));
        let later = Rc::new(slint::Timer::default());
        window.on_resized(move || {
            use i_slint_backend_winit::WinitWindowAccessor;
            let Some(w) = weak.upgrade() else { return };
            if !asked.get() && w.window().has_winit_window() {
                asked.set(true);
                let screen = screen_size(w.window());
                trace(&format!("screen {screen:?}"));
                let fits = opening_size(Some(window_size(w.window())), screen);
                if fits != window_size(w.window()) {
                    // Lands back here, and the second time it fits. The
                    // settings keep the larger size: this screen is where the
                    // window is now, not what somebody chose.
                    w.window().set_size(slint::LogicalSize::new(fits.0, fits.1));
                    saved.set(fits);
                    return;
                }
            }
            let link = Rc::clone(&link);
            let saved = Rc::clone(&saved);
            let weak = w.as_weak();
            later.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(SIZE_SETTLES_MS),
                move || {
                    let Some(w) = weak.upgrade() else { return };
                    keep_size(w.window(), &link, &saved);
                },
            );
        });
    }
}
