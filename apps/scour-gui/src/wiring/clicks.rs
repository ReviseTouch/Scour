//! `SCOUR_GUI_CLICKS="x,y;ctrl+x,y;alt+x,y"`: presses one after another, 1.2 s
//! apart from `SCOUR_GUI_CLICK_MS`, each with the modifiers it names held — how
//! a test reaches a Ctrl+ or Alt+click, which no key or single press can.

use super::*;

pub(crate) fn wire(window: &MainWindow) {
    let Ok(spec) = std::env::var("SCOUR_GUI_CLICKS") else {
        return;
    };
    let mut presses: Vec<(Vec<slint::SharedString>, f32, f32)> = Vec::new();
    for one in spec.split(';') {
        let (held, at) = one.rsplit_once('+').unwrap_or(("", one));
        let p: Vec<f32> = at
            .split(',')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        let [x, y] = p[..] else { continue };
        let keys = held
            .split('+')
            .filter_map(|m| match m {
                "ctrl" => Some(slint::platform::Key::Control.into()),
                "shift" => Some(slint::platform::Key::Shift.into()),
                "alt" => Some(slint::platform::Key::Alt.into()),
                _ => None,
            })
            .collect();
        presses.push((keys, x, y));
    }
    let after = std::env::var("SCOUR_GUI_CLICK_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
        .unwrap_or(2500);
    let weak = window.as_weak();
    let mut left = presses.into_iter();
    let t = Box::leak(Box::new(slint::Timer::default()));
    slint::Timer::single_shot(std::time::Duration::from_millis(after), move || {
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(1200),
            move || {
                let (Some(w), Some((keys, x, y))) = (weak.upgrade(), left.next()) else {
                    return;
                };
                press(&w, &keys, x, y);
            },
        );
    });
}

fn press(w: &MainWindow, keys: &[slint::SharedString], x: f32, y: f32) {
    use slint::platform::WindowEvent as E;
    trace(&format!(
        "synthetic press at {x},{y} holding {}",
        keys.len()
    ));
    let position = slint::LogicalPosition::new(x, y);
    let button = slint::platform::PointerEventButton::Left;
    for k in keys {
        w.window().dispatch_event(E::KeyPressed { text: k.clone() });
    }
    w.window().dispatch_event(E::PointerMoved { position });
    w.window()
        .dispatch_event(E::PointerPressed { position, button });
    w.window()
        .dispatch_event(E::PointerReleased { position, button });
    for k in keys.iter().rev() {
        w.window()
            .dispatch_event(E::KeyReleased { text: k.clone() });
    }
}
