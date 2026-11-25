//! The status line: counts, the walk, the service's state.

use super::*;
use link::{Ask, Link};
use scour_i18n::Catalogue;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::rc::Rc;

/// Duplicates under the report's scope, by size alone: reading is a second press.
pub(super) fn hunt(w: &MainWindow, link: &Rc<Link>, state: &Rc<RefCell<State>>) {
    link.send(Ask::Dupes {
        under: state.borrow().scope.clone(),
        min_size: FLOORS[w.get_dupe_floor().clamp(0, 3) as usize].0,
        read_budget: 0,
    });
}

/// The engine's own numbers, in three pieces: they are drawn differently.
pub(super) fn meter(w: &MainWindow, count: String, ms: String, rows: String) {
    trace(&format!("meter {count} · {ms} · {rows}"));
    w.set_meter_count(count.into());
    w.set_meter_ms(ms.into());
    w.set_meter_rows(rows.into());
    w.set_meter(slint::SharedString::new());
}

/// What a person's last act came to, held on the status line for a few
/// seconds. Not [`said`]: the search that follows a trash or a rename answers
/// within milliseconds and would write over the sentence before it was read.
pub(super) fn told(w: &MainWindow, sentence: slint::SharedString) {
    thread_local! {
        static CLEAR: slint::Timer = slint::Timer::default();
    }
    w.set_notice(sentence);
    let weak = w.as_weak();
    // One timer, restarted: a second act's sentence gets its own four seconds.
    CLEAR.with(|t| {
        t.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_secs(4),
            move || {
                if let Some(w) = weak.upgrade() {
                    w.set_notice(slint::SharedString::new());
                }
            },
        );
    });
}

/// A sentence instead of numbers: connecting, refused, not running.
pub(super) fn said(w: &MainWindow, sentence: slint::SharedString) {
    w.set_meter_count(slint::SharedString::new());
    w.set_meter_ms(slint::SharedString::new());
    w.set_meter_rows(slint::SharedString::new());
    w.set_meter(sentence);
}

/// The Windows service row: what runs the index now, and the way to the other.
pub(super) fn draw_service(w: &MainWindow, cat: &Catalogue) {
    w.set_service_offered(service_mode::OFFERED);
    let active = service_mode::active();
    w.set_service_active(active);
    w.set_autostart_on(autostart::is_on());
    w.set_set_autostart(t(cat, "Start with Windows"));
    w.set_set_autostart_note(t(
        cat,
        "Without the service: the index is kept current from the moment you sign in.",
    ));
    if active {
        w.set_service_state(t(cat, "Running as the Scour service"));
        w.set_service_note(t(
            cat,
            "From Program Files, started with Windows. Your settings stay in your own folder.",
        ));
        w.set_service_push(t(cat, "Run in my session"));
    } else {
        w.set_service_state(t(cat, "Running in your session"));
        w.set_service_note(t(
            cat,
            "As a Windows service it starts with Windows and keeps working when you sign out. It asks once for administrator rights.",
        ));
        w.set_service_push(t(cat, "Run as a service"));
    }
}

/// Tell the service to look at these paths again, now, on a connection of its
/// own: the window's `Link` coalesces for a search box. Failure is silent.
pub(super) fn recheck(addr: &str, paths: Vec<String>) {
    let addr = addr.to_owned();
    std::thread::spawn(move || {
        if let Ok(mut client) = scour_ipc::Client::connect(&addr) {
            let _ = client.call(scour_proto::Request::Recheck { paths });
        }
    });
}

/// Bytes, the way the meter says them: `636,4 MB`.
/// The disks waiting for an answer, from a status: when the count moves, the
/// list is asked for, which carries the question.
pub(super) fn disks_moved(state: &Rc<RefCell<State>>, link: &Link, asking: u32) {
    let mut s = state.borrow_mut();
    if s.disks_asking != Some(asking) {
        s.disks_asking = Some(asking);
        link.send(Ask::Volumes);
        link.send(Ask::Sources);
    }
}

/// `taranıyor 1.240.000` while the index is walked, nothing otherwise. The
/// wording is `scour_ui::SCANNING`, so every face says the same.
pub(super) fn scanning_note(cat: &Catalogue, st: &scour_core::Status) -> slint::SharedString {
    if st.unwritten > 0 {
        return t(cat, scour_ui::UNWRITTEN);
    }
    if !st.scanning {
        return slint::SharedString::new();
    }
    t(cat, scour_ui::SCANNING)
        .replace("{n}", &grouped(st.scanned))
        .into()
}
