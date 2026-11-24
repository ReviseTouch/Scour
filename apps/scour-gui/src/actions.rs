//! What a press does outside the window: open, another face, an export, a picture of itself.

use super::*;
use link::{Ask, Link};
use scour_i18n::Catalogue;
use slint::ComponentHandle;

/// Hand a path to the desktop, spawned and forgotten.
pub(super) fn open(path: &str) {
    let _ = scour_openers::open(scour_core::path::to_path(path).as_os_str());
}

/// A query as one value of a URL's query string: everything but the unreserved
/// characters escaped, so a `&` or a `#` in it stays in it.
pub(super) fn query_encoded(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for b in query.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Start another way of running Scour, detached and never waited for.
pub(super) fn open_face(window: &MainWindow, which_one: &str, cat: &Catalogue, link: &Link) {
    // A terminal interface without a tty exits at once: see `scour_launch::face`.
    if !matches!(which_one, "browser" | "tui") {
        return;
    }
    let program = which_one;
    let Some(mut command) = scour_launch::face(which_one) else {
        said(
            window,
            format!("{} — {program}", t(cat, "not found")).into(),
        );
        return;
    };
    match command.spawn() {
        Ok(_) => {
            // Switching is also choosing: the desktop entry names no face.
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    face: Some(which_one.to_string()),
                    ..Default::default()
                },
            });
            said(window, t(cat, "starting…"));
            // Switching is moving, not opening a second window onto the same
            // index. After a beat: the preference above is a message on a socket.
            slint::Timer::single_shot(std::time::Duration::from_millis(500), || {
                let _ = slint::quit_event_loop();
            });
        }
        Err(e) => said(window, format!("{program}: {e}").into()),
    }
    window.set_panel("".into());
    window.set_armed_face("".into());
}

/// Write the whole matching set to a file, without stopping the window. **Its own
/// connection and its own thread**: the export is a stream — 2.25 M rows and 3.6 s
/// here — and each piece goes straight to the file rather than into memory.
pub(super) fn export(window: &MainWindow, addr: &str, cat: &Catalogue) {
    let query = window.get_query().to_string();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Where a desktop puts downloads, offered rather than decided.
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let into = scour_places::downloads().unwrap_or(home);
    let name = format!("scour-{stamp}.csv");
    let weak = window.as_weak();
    let addr = addr.to_string();
    let waiting = t(cat, "writing…");
    let wrote = t(cat, "written to");
    let failed = t(cat, "could not be written");

    std::thread::spawn(move || {
        // Not on the drawing thread: a window that stops repainting looks crashed.
        let Some(path) = rfd::FileDialog::new()
            .set_directory(&into)
            .set_file_name(&name)
            .add_filter("CSV", &["csv"])
            .save_file()
        else {
            // Cancelled: the person who closed the dialog knows what they did.
            return;
        };
        let told_path = path.display().to_string();
        let _ = slint::invoke_from_event_loop({
            let weak = weak.clone();
            let waiting = waiting.clone();
            move || {
                if let Some(w) = weak.upgrade() {
                    w.set_note(waiting);
                }
            }
        });
        let outcome = (|| -> std::io::Result<u64> {
            let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
            let mut client = scour_ipc::Client::connect(&addr)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            let mut bytes = 0u64;
            let mut hit: Option<std::io::Error> = None;
            let _ = client.stream(
                scour_proto::Request::Export {
                    query,
                    columns: Vec::new(),
                },
                |piece| match piece {
                    scour_proto::Response::ExportChunk { csv } => {
                        use std::io::Write;
                        match file.write_all(csv.as_bytes()) {
                            Ok(()) => {
                                bytes += csv.len() as u64;
                                true
                            }
                            Err(e) => {
                                hit = Some(e);
                                false
                            }
                        }
                    }
                    _ => true,
                },
            );
            use std::io::Write;
            file.flush()?;
            match hit {
                Some(e) => Err(e),
                None => Ok(bytes),
            }
        })();

        let told = match outcome {
            Ok(bytes) => format!("{wrote} {told_path}  ·  {}", compact(bytes)),
            Err(e) => format!("{failed}: {e}"),
        };
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                // It has to stay put: the meter is rewritten several times a second.
                w.set_note(told.into());
            }
        });
    });
}

/// Save what the window actually looks like, then leave: the compositor will not
/// hand a screenshot to a terminal. `SCOUR_GUI_SNAP=/path/to.ppm`, plain PPM.
pub(super) fn snapshot(window: &MainWindow, path: &str) {
    let Ok(buf) = window.window().take_snapshot() else {
        eprintln!("gui: the window could not be captured");
        return;
    };
    let (w, h) = (buf.width(), buf.height());
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for px in buf.as_slice() {
        out.extend_from_slice(&[px.r, px.g, px.b]);
    }
    match std::fs::write(path, out) {
        Ok(()) => eprintln!("gui: {w}x{h} written to {path}"),
        Err(e) => eprintln!("gui: {path} could not be written: {e}"),
    }
}
