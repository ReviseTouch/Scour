//! A drag on Wayland, started on the window's own connection.
//!
//! The compositor starts a drag only for a button press it can name by
//! serial, and the toolkit keeps its serials to itself. So this binds a
//! pointer of its own on the same seat: every pointer of a client is told
//! of every press on its surfaces, and the last one's serial is the one a
//! drag started now belongs to. The objects live on a queue of this crate's,
//! dispatched by [`Wayland::pump`]; the toolkit's event loop reads the
//! socket, and what it reads for this queue waits there.

use std::ffi::c_void;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::path::Path;

use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};

use crate::Refused;

/// What a drop target may ask for, best first.
const URI_LIST: &str = "text/uri-list";
const PLAIN: &str = "text/plain;charset=utf-8";

/// The window's drag, while it lives.
pub(crate) struct Wayland {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    surface: WlSurface,
}

impl std::fmt::Debug for Wayland {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wayland")
            .field("dragging", &self.dragging())
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct State {
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    manager: Option<WlDataDeviceManager>,
    device: Option<WlDataDevice>,
    /// The last press on a surface of this client, and whether it is held.
    press: Option<u32>,
    /// The drag under way: its source and what it offers.
    source: Option<(WlDataSource, Payload)>,
}

struct Payload {
    uris: String,
    plain: String,
}

impl Wayland {
    /// Attach to a window's connection and surface.
    ///
    /// # Safety
    ///
    /// `display` must be the window's live `wl_display` and `surface` its
    /// `wl_surface`, both valid for as long as this value lives.
    pub unsafe fn new(display: *mut c_void, surface: *mut c_void) -> Option<Wayland> {
        // SAFETY: the caller vouches for the display.
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let conn = Connection::from_backend(backend);
        // SAFETY: the caller vouches for the surface; the id only borrows it.
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }.ok()?;
        let surface = WlSurface::from_id(&conn, id).ok()?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        let mut state = State {
            seat: globals.bind::<WlSeat, _, _>(&qh, 1..=7, ()).ok(),
            manager: globals
                .bind::<WlDataDeviceManager, _, _>(&qh, 3..=3, ())
                .ok(),
            ..State::default()
        };
        if let (Some(seat), Some(manager)) = (&state.seat, &state.manager) {
            state.device = Some(manager.get_data_device(seat, &qh, ()));
        }
        // The seat's capabilities arrive here, and with them the pointer.
        queue.roundtrip(&mut state).ok()?;
        Some(Wayland {
            conn,
            queue,
            state,
            surface,
        })
    }

    /// Start dragging these files, on the press that is held now.
    pub fn files(&mut self, paths: &[&Path]) -> Result<(), Refused> {
        // The press was read with the toolkit's events; this hears of it.
        self.pump();
        if self.state.press.is_none() {
            self.queue
                .roundtrip(&mut self.state)
                .map_err(|_| Refused::Broken)?;
        }
        let (Some(manager), Some(device)) = (&self.state.manager, &self.state.device) else {
            return Err(Refused::Unsupported);
        };
        let serial = self.state.press.ok_or(Refused::NoPress)?;
        let qh = self.queue.handle();
        let source = manager.create_data_source(&qh, ());
        source.offer(URI_LIST.into());
        source.offer(PLAIN.into());
        // A copy, never a move: a file found is not a file to take away.
        source.set_actions(DndAction::Copy);
        device.start_drag(Some(&source), &self.surface, None, serial);
        let payload = Payload {
            uris: crate::uri_list(paths),
            plain: paths
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        if let Some((old, _)) = self.state.source.replace((source, payload)) {
            old.destroy();
        }
        self.conn.flush().map_err(|_| Refused::Broken)
    }

    /// Answer what the drop target asked: call while [`Wayland::dragging`].
    pub fn pump(&mut self) {
        let _ = self.queue.dispatch_pending(&mut self.state);
        let _ = self.conn.flush();
    }

    /// A drag is under way: it has neither been dropped nor cancelled.
    pub fn dragging(&self) -> bool {
        self.state.source.is_some()
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if std::env::var_os("SCOUR_DRAG_TRACE").is_some() {
            eprintln!("drag: seat {event:?}");
        }
        // A pointer that goes away — a mouse unplugged, a remote session
        // ended — is let go, and the next one bound when it comes.
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            let has = caps.contains(wl_seat::Capability::Pointer);
            match (&state.pointer, has) {
                (None, true) => state.pointer = Some(seat.get_pointer(qh, ())),
                (Some(_), false) => {
                    if let Some(old) = state.pointer.take()
                        && old.version() >= 3
                    {
                        old.release();
                    }
                    state.press = None;
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if std::env::var_os("SCOUR_DRAG_TRACE").is_some()
            && !matches!(
                event,
                wl_pointer::Event::Motion { .. } | wl_pointer::Event::Frame
            )
        {
            eprintln!("drag: pointer {event:?}");
        }
        if let wl_pointer::Event::Button {
            serial,
            state: WEnum::Value(pressed),
            ..
        } = event
        {
            state.press = (pressed == wl_pointer::ButtonState::Pressed).then_some(serial);
        }
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDevice,
        _: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    // Offers into this window: not this crate's to take.
    wayland_client::event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (
            wayland_client::protocol::wl_data_offer::WlDataOffer, ()
        ),
    ]);
}

impl Dispatch<wayland_client::protocol::wl_data_offer::WlDataOffer, ()> for State {
    fn event(
        _: &mut Self,
        offer: &wayland_client::protocol::wl_data_offer::WlDataOffer,
        _: wayland_client::protocol::wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        offer.destroy();
    }
}

impl Dispatch<WlDataSource, ()> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                if let Some((_, payload)) = &state.source {
                    let body = if mime_type == URI_LIST {
                        &payload.uris
                    } else {
                        &payload.plain
                    };
                    write_all(fd, body.as_bytes());
                }
            }
            // Over, either way: the source goes with it.
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
                source.destroy();
                state.source = None;
            }
            _ => {}
        }
    }
}

/// Write what was asked for and close: the reader waits for the end.
fn write_all(fd: OwnedFd, body: &[u8]) {
    let mut file = std::fs::File::from(fd);
    let _ = file.write_all(body);
}
