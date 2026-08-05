//! The connection to the service, on a thread of its own.
//!
//! The rule this file exists to keep: **the window never waits.** A search on
//! three million entries is a few milliseconds and a facet count is twenty,
//! but a rebuild is seconds and a cold service is a connection that has to be
//! retried — and any of those on the UI thread is a frozen window.
//!
//! So the work happens here and the answers arrive as events. Two connections
//! rather than one, because `scour-ipc` is one call at a time with no
//! cancellation and `scourd` is thread-per-connection: an interactive lane for
//! what a keystroke needs, and a background lane for what a keystroke can wait
//! for. Without the split a twenty-millisecond facet count sits in front of the
//! next search.

use std::sync::mpsc::{Receiver, Sender, channel};

use scour_ipc::Client;
use scour_proto::{Request, Response};

/// How many matches an interactive search counts before it stops.
///
/// A thousand is more than a person reads and enough for a meter to say
/// "at least this many". The exact figure follows once the query settles.
pub const TYPING_CAP: u32 = 1_000;

/// What the window asks for.
pub enum Ask {
    /// A search and its facets, tagged with the keystroke that caused them.
    ///
    /// The tag is what makes a stale answer droppable. Without it a slow reply
    /// to `re` arrives after a fast one to `rapor` and the list goes backwards
    /// under the user's hands — which is the single most noticeable defect a
    /// search-as-you-type box can have.
    Search {
        generation: u64,
        query: String,
        sort: String,
        descending: bool,
        limit: u32,
    },
    Facets {
        generation: u64,
        query: String,
    },
    /// How many match, exactly, once the typing has stopped.
    Count {
        generation: u64,
        query: String,
    },
    Stop,
}

/// What comes back.
pub enum Got {
    Search {
        generation: u64,
        reply: Box<Response>,
    },
    Facets {
        generation: u64,
        reply: Box<Response>,
    },
    Count {
        generation: u64,
        reply: Box<Response>,
    },
    /// The service answered, and the answer was no.
    ///
    /// Distinct from [`Got::Down`] because the two want opposite handling: a
    /// refused query — a term too short for the substring index, a field the
    /// engine cannot serve — leaves a perfectly good connection open, and
    /// throwing it away and reconnecting on every keystroke of `ra` would be
    /// a reconnect storm caused by nothing.
    Refused { generation: u64, why: String },
    /// The service could not be reached, with the reason as a sentence.
    Down(String),
    /// It could, after having been down.
    Up,
}

pub struct Link {
    ask: Sender<Ask>,
}

impl Link {
    /// Start the two lanes. `sink` is called from the worker threads.
    pub fn start(addr: String, sink: impl Fn(Got) + Send + Clone + 'static) -> Link {
        let (tx, rx) = channel::<Ask>();
        let (fast_tx, fast_rx) = channel::<Ask>();
        let (slow_tx, slow_rx) = channel::<Ask>();

        // One router, so that the window has a single sender and does not have
        // to know which lane a request belongs on.
        std::thread::spawn(move || {
            for ask in rx {
                let done = matches!(ask, Ask::Stop);
                let to = match ask {
                    Ask::Facets { .. } | Ask::Count { .. } => &slow_tx,
                    _ => &fast_tx,
                };
                if to.send(ask).is_err() || done {
                    let _ = slow_tx.send(Ask::Stop);
                    let _ = fast_tx.send(Ask::Stop);
                    break;
                }
            }
        });

        spawn_lane(addr.clone(), fast_rx, sink.clone());
        spawn_lane(addr, slow_rx, sink);
        Link { ask: tx }
    }

    pub fn send(&self, ask: Ask) {
        // A closed channel means the lane died, and the window finds out from
        // the `Down` event rather than from a panic here.
        let _ = self.ask.send(ask);
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let _ = self.ask.send(Ask::Stop);
    }
}

/// Which kind of answer a request is going to produce.
#[derive(Clone, Copy)]
enum Lane {
    Search,
    Facets,
    Count,
}

/// One lane: connect, serve, reconnect when the service comes back.
fn spawn_lane(addr: String, rx: Receiver<Ask>, sink: impl Fn(Got) + Send + 'static) {
    std::thread::spawn(move || {
        let mut client: Option<Client> = None;
        let mut was_down = false;
        for ask in rx {
            let ask = match ask {
                Ask::Stop => break,
                other => other,
            };
            // Reconnect lazily rather than on a timer: the only moment the
            // window cares whether the service is up is when it has something
            // to ask.
            if client.is_none() {
                match Client::connect(&addr) {
                    Ok(c) => {
                        client = Some(c);
                        if was_down {
                            was_down = false;
                            sink(Got::Up);
                        }
                    }
                    Err(e) => {
                        if !was_down {
                            was_down = true;
                            sink(Got::Down(e.to_string()));
                        }
                        continue;
                    }
                }
            }
            let Some(c) = client.as_mut() else { continue };
            let (generation, request, facets) = match ask {
                Ask::Search {
                    generation,
                    query,
                    sort,
                    descending,
                    limit,
                } => (
                    generation,
                    Request::Search {
                        query,
                        sort: sort_of(&sort),
                        descending,
                        page: scour_core::Page {
                            offset: 0,
                            limit,
                            // **Small, and this is the single largest thing a
                            // keystroke used to cost.** The cap is how many
                            // matches the walk counts before it stops, and
                            // reaching 100,000 of them for `ra` meant visiting
                            // 1,208,951 rows — 23.1 ms — against 35,743 and
                            // 0.9 ms at a thousand. Twenty-five times, to
                            // print a total nobody reads while still typing.
                            //
                            // The exact number arrives separately, after the
                            // typing stops. Until then the meter says `1000+`,
                            // which is true.
                            count_cap: TYPING_CAP,
                        },
                    },
                    Lane::Search,
                ),
                Ask::Facets { generation, query } => (
                    generation,
                    Request::Facets {
                        query,
                        by: vec![scour_core::FacetBy::Kind],
                    },
                    Lane::Facets,
                ),
                Ask::Count { generation, query } => (
                    generation,
                    Request::Count {
                        query,
                        cap: 10_000_000,
                    },
                    Lane::Count,
                ),
                Ask::Stop => break,
            };
            match c.call(request) {
                Ok(reply) => {
                    let reply = Box::new(reply);
                    sink(match facets {
                        Lane::Facets => Got::Facets { generation, reply },
                        Lane::Count => Got::Count { generation, reply },
                        Lane::Search => Got::Search { generation, reply },
                    });
                }
                Err(e) => {
                    // Two failures that look alike and are not. A transport
                    // error means the connection is gone and the next call has
                    // to make a new one; anything else is the service saying
                    // no to this particular question, and the connection is
                    // fine.
                    if matches!(
                        e,
                        scour_core::Error::Unreachable { .. } | scour_core::Error::Io { .. }
                    ) {
                        client = None;
                        was_down = true;
                        sink(Got::Down(e.to_string()));
                    } else {
                        sink(Got::Refused {
                            generation,
                            why: e.to_string(),
                        });
                    }
                }
            }
        }
    });
}

fn sort_of(name: &str) -> scour_core::SortKey {
    use scour_core::SortKey;
    match name {
        "size" => SortKey::Size,
        "modified" => SortKey::Modified,
        "name" => SortKey::Name,
        "path" => SortKey::Path,
        "ext" => SortKey::Ext,
        "kind" => SortKey::Kind,
        _ => SortKey::Relevance,
    }
}
