//! The window's colours, size and zoom: opened as it was left, kept when it moves.

use super::*;
use link::{Ask, Link};
use slint::ComponentHandle;
use std::rc::Rc;

/// One `scour-ui` palette, in the shape the window's generated struct wants.
pub(crate) fn scheme(p: &scour_ui::Palette) -> Scheme {
    let c = |x: &scour_ui::Rgba| {
        let (a, r, g, b) = x.argb();
        slint::Brush::SolidColor(slint::Color::from_argb_u8(a, r, g, b))
    };
    Scheme {
        ground: c(&p.ground),
        panel: c(&p.panel),
        panel_2: c(&p.panel_2),
        line: c(&p.line),
        line_soft: c(&p.line_soft),
        ink: c(&p.ink),
        ink_2: c(&p.ink_2),
        ink_3: c(&p.ink_3),
        mark: c(&p.mark),
        mark_ink: c(&p.mark_ink),
        pick: c(&p.pick),
        focus: c(&p.focus),
        hover: c(&p.hover),
        danger: c(&p.danger),
        t0: c(&p.t[0]),
        t1: c(&p.t[1]),
        t2: c(&p.t[2]),
        t3: c(&p.t[3]),
        t4: c(&p.t[4]),
        t5: c(&p.t[5]),
        k0: c(&p.k[0]),
        k1: c(&p.k[1]),
        k2: c(&p.k[2]),
        k3: c(&p.k[3]),
        k4: c(&p.k[4]),
        k5: c(&p.k[5]),
        kx: c(&p.kx),
        q_term: c(&p.q_term),
        q_key: c(&p.q_key),
        q_val: c(&p.q_val),
        q_glob: c(&p.q_glob),
        q_not: c(&p.q_not),
        q_bad: c(&p.q_bad),
        cap: c(&p.cap),
        cap_ring: c(&p.cap_ring),
        cap_read: c(&p.cap_read),
        cap_read_ring: c(&p.cap_read_ring),
        cap_neg: c(&p.cap_neg),
        cap_neg_ring: c(&p.cap_neg_ring),
        disk_system: c(&p.disk_system),
        disk_fixed: c(&p.disk_fixed),
        disk_removable: c(&p.disk_removable),
        disk_network: c(&p.disk_network),
        disk_optical: c(&p.disk_optical),
    }
}

/// What a first run opens at, in logical pixels. **The same pair as
/// `preferred-width`/`preferred-height` in `main.slint`**, said here because
/// this is what the code applies; the `.slint` numbers only cover the frame
/// before it does.
pub(super) const DEFAULT_SIZE: (f32, f32) = (1100.0, 680.0);

/// The smallest the window may open at — `min-width`/`min-height` in
/// `main.slint`. A screen narrower than this wins nothing by being obeyed.
pub(super) const SMALLEST_SIZE: (f32, f32) = (560.0, 320.0);

/// Stands in for the screen until there is one to ask. Generous on purpose: it
/// is a guard against a nonsense number in the settings, not a layout choice.
pub(super) const NO_SCREEN_IS_BIGGER: (f32, f32) = (3840.0, 2160.0);

/// How long a drag has to stop before the size is written down. A resize is
/// hundreds of changes; this is one call at the end of them.
pub(super) const SIZE_SETTLES_MS: u64 = 700;

/// What size to open at: what was left behind, held inside the screen and never
/// under the window's own minimum; [`DEFAULT_SIZE`] when nothing was left.
/// `screen` is the usable screen in logical pixels, `None` while it is not
/// knowable — before there is a real window, there is no monitor to ask.
pub(super) fn opening_size(kept: Option<(f32, f32)>, screen: Option<(f32, f32)>) -> (f32, f32) {
    let (sw, sh) = screen.unwrap_or(NO_SCREEN_IS_BIGGER);
    let (want_w, want_h) = kept.unwrap_or(DEFAULT_SIZE);
    // The floor beats the screen: a window under `min-width` is drawn wrong,
    // and a screen that small is a misreading of a monitor, not a monitor.
    let fit = |want: f32, floor: f32, screen: f32| want.min(screen.max(floor)).max(floor);
    (
        fit(want_w, SMALLEST_SIZE.0, sw.min(NO_SCREEN_IS_BIGGER.0)),
        fit(want_h, SMALLEST_SIZE.1, sh.min(NO_SCREEN_IS_BIGGER.1)),
    )
}

/// The size a face left behind, out of its own corner of the settings
/// (`view.slint.size` here, `view.web.size` for the page): two positive
/// numbers, and anything else is nobody's opinion.
pub(super) fn kept_size(view: Option<&serde_json::Value>) -> Option<(f32, f32)> {
    let [w, h] = view?.get("size")?.as_array()?.as_slice() else {
        return None;
    };
    let (w, h) = (w.as_f64()? as f32, h.as_f64()? as f32);
    (w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0).then_some((w, h))
}

/// The window's size now, in the desktop's logical pixels — not the zoomed
/// ones, or a window opened at a kept size would grow with every zoom.
pub(super) fn window_size(win: &slint::Window) -> (f32, f32) {
    let size = win.size().to_logical(native_scale(win));
    (size.width, size.height)
}

/// How far the zoom goes either way.
const ZOOM_RANGE: (f32, f32) = (0.6, 1.6);

/// What the zoom starts at: the desktop's own scale, everywhere. Windows once
/// started at 0.85, sized for the older, looser window on a 125% laptop; on a
/// 100% desktop that drew text at ten pixels, thin and faint (17 November).
pub(super) fn default_zoom() -> f32 {
    1.0
}

/// The zoom a person left, under `view.slint.zoom`, or the default.
pub(super) fn kept_zoom(view: Option<&serde_json::Value>) -> f32 {
    view.and_then(|v| v.get("zoom"))
        .and_then(serde_json::Value::as_f64)
        .map(|z| z as f32)
        .filter(|z| z.is_finite())
        .unwrap_or_else(default_zoom)
        .clamp(ZOOM_RANGE.0, ZOOM_RANGE.1)
}

/// The scale the desktop gives this window, whatever the zoom made of it.
fn native_scale(win: &slint::Window) -> f32 {
    use i_slint_backend_winit::WinitWindowAccessor;
    win.with_winit_window(|ww| ww.scale_factor() as f32)
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or_else(|| win.scale_factor())
}

/// Draw at the desktop's scale times the zoom, if it is not already.
/// The values the settings' zoom row offers, as it names them.
const ZOOM_CHOICES: [&str; 5] = ["0.9", "1", "1.1", "1.25", "1.5"];

/// Which of [`ZOOM_CHOICES`] a zoom is, or `""` between them.
pub(super) fn zoom_choice(z: f32) -> &'static str {
    ZOOM_CHOICES
        .into_iter()
        .find(|c| c.parse::<f32>().is_ok_and(|v| (v - z).abs() < 0.001))
        .unwrap_or("")
}

/// Draw at this zoom, show it on the settings' row, and keep it.
pub(super) fn set_zoom(w: &MainWindow, link: &Link, next: f32) {
    let next = next.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
    ZOOM.with(|z| z.set(next));
    apply_zoom(w);
    w.set_zoom_choice(zoom_choice(next).into());
    link.send(Ask::Remember {
        change: scour_settings::Change {
            view: std::collections::BTreeMap::from([(
                "slint".to_owned(),
                serde_json::json!({ "zoom": next }),
            )]),
            ..Default::default()
        },
    });
}

pub(super) fn apply_zoom(w: &MainWindow) {
    let win = w.window();
    let want = native_scale(win) * ZOOM.with(std::cell::Cell::get);
    if (win.scale_factor() - want).abs() > 0.001 {
        w.global::<Theme>()
            .set_hair(if want >= 1.0 { 1.0 } else { 1.05 / want });
        // The window keeps its pixels; what changes is how many logical ones
        // they hold. Said as a resize too, or the old logical size is laid out
        // in a corner of the window and the rest left unpainted.
        let physical = win.size();
        win.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: want });
        win.dispatch_event(slint::platform::WindowEvent::Resized {
            size: physical.to_logical(want),
        });
    }
}

/// The screen this window is on, in logical pixels. `None` until there is a
/// winit window behind the Slint one — which is why the clamp happens on the
/// first resize and not at birth — and on a compositor that will not say.
pub(super) fn screen_size(win: &slint::Window) -> Option<(f32, f32)> {
    use i_slint_backend_winit::WinitWindowAccessor;
    // **The window's scale, not the monitor's.** A Wayland output advertises
    // the integer scale beside its physical size — 2 for a screen this window
    // is drawn on at 1.667 — and a size compared against the window's has to be
    // in the window's frame. Measured here: 3200x2000 at 2 is 1600x1000 and
    // wrong; over 1.667 it is 1920x1200, which is the screen.
    let scale = native_scale(win);
    win.with_winit_window(|ww| {
        let logical = |on: i_slint_backend_winit::winit::monitor::MonitorHandle| {
            let size = on.size();
            let (w, h) = (size.width as f32 / scale, size.height as f32 / scale);
            (w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite()).then_some((w, h))
        };
        // Wayland names no monitor for a window — a client is not told which
        // output it is on. Then the **widest** screen there is, never the
        // nearest: a guess that shrinks a window is worse than no guess.
        ww.current_monitor()
            .and_then(logical)
            .or_else(|| ww.available_monitors().filter_map(logical).reduce(widest))
    })?
}

/// The bigger of two screens, by area.
fn widest(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    if b.0 * b.1 > a.0 * a.1 { b } else { a }
}

/// Is the window spread by the desktop rather than sized by hand? A maximised
/// window's size is the screen's, which is not a choice worth keeping.
fn spread(win: &slint::Window) -> bool {
    use i_slint_backend_winit::WinitWindowAccessor;
    win.with_winit_window(|ww| ww.is_maximized() || ww.fullscreen().is_some())
        .unwrap_or(false)
}

/// Write the window's size down, under `view.slint.size`. A [`scour_settings::Change`]
/// merges `view` key by key, so nothing else the window keeps there is touched.
pub(super) fn keep_size(win: &slint::Window, link: &Rc<Link>, saved: &std::cell::Cell<(f32, f32)>) {
    if spread(win) {
        trace("size not kept: the desktop is spreading the window, not a hand");
        return;
    }
    let (w, h) = window_size(win);
    let was = saved.get();
    // Sub-pixel jitter from a scale factor is not somebody resizing a window.
    if (w - was.0).abs() < 1.0 && (h - was.1).abs() < 1.0 {
        return;
    }
    saved.set((w, h));
    trace(&format!("keeping window size {w:.0}x{h:.0}"));
    link.send(Ask::Remember {
        change: scour_settings::Change {
            view: std::collections::BTreeMap::from([(
                "slint".to_owned(),
                serde_json::json!({ "size": [w.round() as i64, h.round() as i64] }),
            )]),
            ..Default::default()
        },
    });
}
