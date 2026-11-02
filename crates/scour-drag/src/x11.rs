//! A drag on X11, by the XDND protocol, from a connection and a window of
//! this crate's own: the toolkit's connection reads every event for its own
//! windows, and a drop target answers the source window.
//!
//! The press stays the toolkit's and nothing is grabbed: where the pointer
//! is gets asked every sixteen milliseconds, and the drop is the moment the
//! button is let go — which reaches the toolkit as an ordinary release too.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, KeyButMask,
    PropMode, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent, Window,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

use crate::Refused;

/// The protocol version this speaks, and the oldest a target may.
const VERSION: u32 = 5;
const OLDEST: u32 = 3;
/// How often the pointer is asked where it is.
const TICK: Duration = Duration::from_millis(16);
/// How long a target that took the drop may take to say it is done.
const FINISH: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
pub(crate) struct X11 {
    busy: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
}

impl X11 {
    /// Only where an X server answers.
    pub fn new() -> Option<X11> {
        x11rb::connect(None).ok()?;
        Some(X11::default())
    }

    pub fn files(&mut self, paths: &[&Path]) -> Result<(), Refused> {
        if self.busy.load(Ordering::Acquire) {
            return Err(Refused::Broken);
        }
        let (conn, screen) = x11rb::connect(None).map_err(|_| Refused::Broken)?;
        let root = conn.setup().roots[screen].root;
        let held = conn
            .query_pointer(root)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|p| p.mask.contains(KeyButMask::BUTTON1));
        if !held {
            return Err(Refused::NoPress);
        }
        let payload = Payload::of(paths);
        let busy = Arc::clone(&self.busy);
        let dropped = Arc::clone(&self.dropped);
        busy.store(true, Ordering::Release);
        dropped.store(false, Ordering::Release);
        std::thread::Builder::new()
            .name("scour-drag-x11".into())
            .spawn(move || {
                let taken = run(&conn, root, &payload).unwrap_or(false);
                dropped.store(taken, Ordering::Release);
                busy.store(false, Ordering::Release);
            })
            .map(|_| ())
            .map_err(|_| {
                self.busy.store(false, Ordering::Release);
                Refused::Broken
            })
    }

    pub fn dragging(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    pub fn dropped(&self) -> bool {
        self.dropped.load(Ordering::Acquire)
    }
}

struct Payload {
    uris: String,
    plain: String,
}

impl Payload {
    fn of(paths: &[&Path]) -> Payload {
        Payload {
            uris: crate::uri_list(paths),
            plain: paths
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// The atoms the protocol speaks in.
struct Atoms {
    aware: u32,
    selection: u32,
    enter: u32,
    position: u32,
    status: u32,
    leave: u32,
    drop: u32,
    finished: u32,
    copy: u32,
    uri_list: u32,
    plain: u32,
    utf8: u32,
    targets: u32,
}

impl Atoms {
    fn new(conn: &RustConnection) -> Option<Atoms> {
        let get = |name: &str| -> Option<u32> {
            Some(
                conn.intern_atom(false, name.as_bytes())
                    .ok()?
                    .reply()
                    .ok()?
                    .atom,
            )
        };
        Some(Atoms {
            aware: get("XdndAware")?,
            selection: get("XdndSelection")?,
            enter: get("XdndEnter")?,
            position: get("XdndPosition")?,
            status: get("XdndStatus")?,
            leave: get("XdndLeave")?,
            drop: get("XdndDrop")?,
            finished: get("XdndFinished")?,
            copy: get("XdndActionCopy")?,
            uri_list: get("text/uri-list")?,
            plain: get("text/plain;charset=utf-8")?,
            utf8: get("UTF8_STRING")?,
            targets: get("TARGETS")?,
        })
    }
}

/// The drag, from the first tick to the target saying it is done; true when
/// a target took the drop.
fn run(conn: &RustConnection, root: Window, payload: &Payload) -> Option<bool> {
    let atoms = Atoms::new(conn)?;
    // A source window of this connection's: the target's answers come here.
    let source = conn.generate_id().ok()?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        source,
        root,
        -10,
        -10,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new().override_redirect(1),
    )
    .ok()?;
    conn.set_selection_owner(source, atoms.selection, CURRENT_TIME)
        .ok()?;
    conn.flush().ok()?;

    let mut over: Option<Window> = None;
    let mut accepted = false;
    let mut last = (i16::MIN, i16::MIN);
    loop {
        while let Ok(Some(event)) = conn.poll_for_event() {
            match event {
                Event::SelectionRequest(req) => answer(conn, &atoms, payload, &req),
                Event::ClientMessage(m) if m.type_ == atoms.status => {
                    accepted = m.data.as_data32()[1] & 1 == 1;
                }
                _ => {}
            }
        }
        let p = conn.query_pointer(root).ok()?.reply().ok()?;
        let target = target_at(conn, &atoms, root, p.root_x, p.root_y);
        if target != over {
            if let Some(old) = over {
                send(conn, old, atoms.leave, [source, 0, 0, 0, 0]);
            }
            if let Some(new) = target {
                let types = [atoms.uri_list, atoms.plain, atoms.utf8];
                send(
                    conn,
                    new,
                    atoms.enter,
                    [source, VERSION << 24, types[0], types[1], types[2]],
                );
            }
            over = target;
            accepted = false;
            last = (i16::MIN, i16::MIN);
        }
        if let Some(t) = over
            && (p.root_x, p.root_y) != last
        {
            let at = ((p.root_x as u16 as u32) << 16) | p.root_y as u16 as u32;
            send(
                conn,
                t,
                atoms.position,
                [source, 0, at, CURRENT_TIME, atoms.copy],
            );
            last = (p.root_x, p.root_y);
        }
        conn.flush().ok()?;
        if !p.mask.contains(KeyButMask::BUTTON1) {
            break;
        }
        std::thread::sleep(TICK);
    }

    let taken = match over {
        Some(t) if accepted => {
            send(conn, t, atoms.drop, [source, 0, CURRENT_TIME, 0, 0]);
            conn.flush().ok()?;
            finish(conn, &atoms, payload);
            true
        }
        Some(t) => {
            send(conn, t, atoms.leave, [source, 0, 0, 0, 0]);
            false
        }
        None => false,
    };
    let _ = conn.destroy_window(source);
    conn.flush().ok()?;
    Some(taken)
}

/// Serve the target's requests for the files until it says it has them.
fn finish(conn: &RustConnection, atoms: &Atoms, payload: &Payload) {
    let until = Instant::now() + FINISH;
    while Instant::now() < until {
        match conn.poll_for_event() {
            Ok(Some(Event::SelectionRequest(req))) => answer(conn, atoms, payload, &req),
            Ok(Some(Event::ClientMessage(m))) if m.type_ == atoms.finished => return,
            Ok(Some(_)) => {}
            Ok(None) => std::thread::sleep(TICK),
            Err(_) => return,
        }
    }
}

/// The window under the pointer that takes drops, and nothing when none does.
fn target_at(conn: &RustConnection, atoms: &Atoms, root: Window, x: i16, y: i16) -> Option<Window> {
    let mut w = root;
    loop {
        if w != root && aware(conn, atoms, w) {
            return Some(w);
        }
        let child = conn
            .translate_coordinates(root, w, x, y)
            .ok()?
            .reply()
            .ok()?
            .child;
        if child == NONE {
            return None;
        }
        w = child;
    }
}

/// Whether a window says it takes drops in a version this speaks.
fn aware(conn: &RustConnection, atoms: &Atoms, w: Window) -> bool {
    conn.get_property(false, w, atoms.aware, AtomEnum::ATOM, 0, 1)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| r.value32().and_then(|mut v| v.next()))
        .is_some_and(|v| v >= OLDEST)
}

fn send(conn: &RustConnection, to: Window, kind: u32, data: [u32; 5]) {
    let event = ClientMessageEvent::new(32, to, kind, data);
    let _ = conn.send_event(false, to, EventMask::NO_EVENT, event);
}

/// Hand the target the files, in the form it asked for.
fn answer(conn: &RustConnection, atoms: &Atoms, payload: &Payload, req: &SelectionRequestEvent) {
    // An old client names no property; the target is used instead.
    let property = if req.property == NONE {
        req.target
    } else {
        req.property
    };
    let stored = if req.selection != atoms.selection {
        false
    } else if req.target == atoms.targets {
        let offered = [atoms.targets, atoms.uri_list, atoms.plain, atoms.utf8];
        conn.change_property32(
            PropMode::REPLACE,
            req.requestor,
            property,
            AtomEnum::ATOM,
            &offered,
        )
        .is_ok()
    } else if req.target == atoms.uri_list {
        conn.change_property8(
            PropMode::REPLACE,
            req.requestor,
            property,
            req.target,
            payload.uris.as_bytes(),
        )
        .is_ok()
    } else if req.target == atoms.plain || req.target == atoms.utf8 {
        conn.change_property8(
            PropMode::REPLACE,
            req.requestor,
            property,
            req.target,
            payload.plain.as_bytes(),
        )
        .is_ok()
    } else {
        false
    };
    let notify = SelectionNotifyEvent {
        response_type: SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: req.time,
        requestor: req.requestor,
        selection: req.selection,
        target: req.target,
        property: if stored { property } else { NONE },
    };
    let _ = conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify);
    let _ = conn.flush();
}
