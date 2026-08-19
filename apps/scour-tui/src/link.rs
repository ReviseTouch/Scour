//! The connection to the service, on a thread of its own.
//!
//! The rule this file keeps is the window's rule: **the interface never
//! waits.** A search over three million entries is a couple of milliseconds
//! and a facet count is twenty, but a cold service is a connection that has to
//! be retried — and any of that on the drawing thread is a terminal that stops
//! answering the keyboard.
//!
//! So the work happens here and the answers arrive as events, on the same
//! channel the keyboard arrives on. One lane to start with: the terminal asks
//! for a page at a time and nothing else yet. When the rail and the report
//! land this grows a second connection, exactly as the window has, because
//! `scour-ipc` is one call at a time and a twenty-millisecond facet count
//! sitting in front of the next keystroke is what that split exists to stop.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use scour_core::{Page, SortKey};
use scour_ipc::Client;
use scour_proto::{Request, Response};

/// How many matches an interactive search counts before it stops.
///
/// A thousand is more than anybody reads and enough for a meter to say "at
/// least this many". The exact figure follows when the typing settles.
pub const TYPING_CAP: u32 = 1_000;

/// What the terminal asks for.
pub enum Ask {
    Search {
        /// Which keystroke this belongs to. An answer to an older one is
        /// dropped rather than drawn: a slow reply to `re` landing after a
        /// fast one to `rapor` is the list going backwards under somebody's
        /// hands, which is the worst defect a search-as-you-type box has.
        generation: u64,
        query: String,
        sort: SortKey,
        descending: bool,
        offset: u32,
        limit: u32,
        cap: u32,
    },
    /// Stop: the terminal is closing.
    Done,
}

/// What comes back.
pub enum Got {
    /// A page of a search, with the offset it was for — **carried by the
    /// answer**, because two offsets of one query are both current and filing
    /// a page at the offset last asked for puts it a page from where it
    /// belongs the moment somebody scrolls.
    Search {
        generation: u64,
        offset: u32,
        limit: u32,
        reply: Box<scour_core::SearchResponse>,
    },
    /// The service could not be reached, or said no.
    Trouble { generation: u64, why: String },
}

/// The service, at the far end of a thread.
pub struct Link {
    asks: Sender<Ask>,
}

impl Link {
    /// Start talking to the service at `addr`; answers go to the returned
    /// receiver, which the event loop selects on alongside the keyboard.
    pub fn start(addr: String) -> (Link, Receiver<Got>) {
        let (asks, inbox) = channel::<Ask>();
        let (gots, answers) = channel::<Got>();
        thread::spawn(move || serve(&addr, &inbox, &gots));
        (Link { asks }, answers)
    }

    pub fn send(&self, ask: Ask) {
        // A closed channel means the thread is gone, which happens only while
        // shutting down. Nothing to report to anybody who could act on it.
        let _ = self.asks.send(ask);
    }
}

fn serve(addr: &str, inbox: &Receiver<Ask>, out: &Sender<Got>) {
    let mut client: Option<Client> = None;
    while let Ok(ask) = inbox.recv() {
        let Ask::Search {
            generation,
            query,
            sort,
            descending,
            offset,
            limit,
            cap,
        } = ask
        else {
            return;
        };
        // **Reconnect on every failure rather than once at startup.** The
        // service is restarted far more often than this is — a rebuild, a
        // config change, `systemctl restart` — and an interface that dies with
        // it is one somebody has to notice and restart by hand.
        if client.is_none() {
            client = Client::connect(addr).ok();
        }
        let Some(link) = client.as_mut() else {
            let _ = out.send(Got::Trouble {
                generation,
                why: format!("no service at {addr}"),
            });
            continue;
        };
        let request = Request::Search {
            query,
            sort,
            descending,
            page: Page {
                offset,
                limit,
                count_cap: cap,
                ..Page::default()
            },
        };
        match link.call(request) {
            Ok(Response::Search(reply)) => {
                let _ = out.send(Got::Search {
                    generation,
                    offset,
                    limit,
                    reply: Box::new(reply),
                });
            }
            Ok(_) => {
                let _ = out.send(Got::Trouble {
                    generation,
                    why: "unexpected reply".into(),
                });
            }
            Err(e) => {
                // **A refused query and a dead socket arrive the same way**, so
                // the connection is dropped either way: reconnecting costs a
                // hundred microseconds and keeping a dead one costs every
                // question after it.
                client = None;
                let _ = out.send(Got::Trouble {
                    generation,
                    why: e.to_string(),
                });
            }
        }
    }
}
