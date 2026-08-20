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

use scour_core::{FacetBy, Page, SortKey};
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
    /// What the rail shows: the kinds, and the time strip's bands.
    ///
    /// **A lane of its own**, because a facet count walks the matching set and
    /// a keystroke must not queue behind one. The window learned this the same
    /// way: twenty milliseconds in front of every search is a search box that
    /// feels broken.
    Facets {
        generation: u64,
        query: String,
        /// Which of the two questions this is. They are asked separately
        /// because they are asked *about different rows* — see `App::asking`
        /// and the note on the strip in `App::strip_over`.
        age: bool,
    },
    /// Where this desktop keeps things.
    Places,
    /// Exactly how many match, once the typing has stopped.
    Count { generation: u64, query: String },
    /// Wait until the index moves — a long poll, on a lane of its own.
    Await { since: u64 },
    /// What the walk skips, in three groups, and which of them are off.
    Rules,
    /// Switch a rule off or on: the whole list, replaced.
    OffRules(Vec<String>),
    /// Remember something small — the language, the face.
    Remember(scour_settings::Change),
    /// The whole result as a spreadsheet, written where the caller says.
    Export { query: String, to: String },
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
    /// The rail's counts, or the strip's.
    Facets {
        generation: u64,
        reply: Box<scour_core::FacetResponse>,
    },
    /// The desktop's own folders.
    Places(Vec<(String, String)>),
    /// The skip rules: three groups of `(kind, value)`, and the ids switched
    /// off. The groups are kept apart because only the first can be deleted
    /// and a flat list said none of that.
    Rules {
        added: Vec<(String, String)>,
        config: Vec<(String, String)>,
        builtin: Vec<(String, String)>,
        off: Vec<String>,
    },
    /// Exactly how many match.
    Counted { generation: u64, total: u64 },
    /// The index moved, and what it moved to.
    Awake(u64),
    /// A spreadsheet was written, and where.
    Wrote(String),
    /// Something that is not about a search went wrong.
    ///
    /// **Not a `Trouble`**, which carries the keystroke it belongs to and is
    /// dropped when that keystroke is old — which is right for a page and
    /// silently wrong for a file that failed to be written.
    Failed(String),
    /// The service could not be reached, or said no.
    Trouble { generation: u64, why: String },
}

/// The service, at the far end of a thread.
pub struct Link {
    asks: Sender<Ask>,
    slow: Sender<Ask>,
    wait: Sender<Ask>,
}

impl Link {
    /// Start talking to the service at `addr`; answers go to the returned
    /// receiver, which the event loop selects on alongside the keyboard.
    pub fn start(addr: String) -> (Link, Receiver<Got>) {
        let (asks, inbox) = channel::<Ask>();
        let (slow, waiting) = channel::<Ask>();
        let (wait, dozing) = channel::<Ask>();
        let (gots, answers) = channel::<Got>();
        let fast_addr = addr.clone();
        let fast_out = gots.clone();
        thread::spawn(move || serve(&fast_addr, &inbox, &fast_out));
        // Three connections, because `scour-ipc` is one call at a time and
        // `scourd` is a thread per connection. The third exists because the
        // long poll *holds* its connection for thirty seconds: on either of
        // the others it would be thirty seconds of a terminal that answers
        // nothing.
        let slow_addr = addr.clone();
        let slow_out = gots.clone();
        thread::spawn(move || serve(&slow_addr, &waiting, &slow_out));
        thread::spawn(move || serve(&addr, &dozing, &gots));
        (Link { asks, slow, wait }, answers)
    }

    /// What a keystroke needs.
    pub fn send(&self, ask: Ask) {
        // A closed channel means the thread is gone, which happens only while
        // shutting down. Nothing to report to anybody who could act on it.
        let _ = self.asks.send(ask);
    }

    /// What a keystroke can wait for.
    pub fn later(&self, ask: Ask) {
        let _ = self.slow.send(ask);
    }

    /// The long poll, which holds a connection of its own.
    pub fn doze(&self, ask: Ask) {
        let _ = self.wait.send(ask);
    }
}

/// Write the whole result to a file, in the pieces the service sends it in.
///
/// **Not through `call`.** This is the one request answered in more than one
/// frame, and reading only the first would leave the rest in the buffer for
/// the next question to be answered by.
fn export(link: &mut Client, query: &str, to: &str) -> Result<(), String> {
    use std::io::Write;
    let mut file =
        std::io::BufWriter::new(std::fs::File::create(to).map_err(|e| format!("{to}: {e}"))?);
    let mut trouble: Option<String> = None;
    link.stream(
        Request::Export {
            query: query.to_string(),
            columns: Vec::new(),
        },
        |piece| match piece {
            Response::ExportChunk { csv } => match file.write_all(csv.as_bytes()) {
                Ok(()) => true,
                Err(e) => {
                    trouble = Some(e.to_string());
                    false
                }
            },
            _ => true,
        },
    )
    .map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    match trouble {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

fn serve(addr: &str, inbox: &Receiver<Ask>, out: &Sender<Got>) {
    let mut client: Option<Client> = None;
    while let Ok(ask) = inbox.recv() {
        if matches!(ask, Ask::Done) {
            return;
        }
        // **Reconnect on every failure rather than once at startup.** The
        // service is restarted far more often than this is — a rebuild, a
        // config change, `systemctl restart` — and an interface that dies with
        // it is one somebody has to notice and restart by hand.
        if client.is_none() {
            client = Client::connect(addr).ok();
        }
        let generation = match &ask {
            Ask::Search { generation, .. }
            | Ask::Facets { generation, .. }
            | Ask::Count { generation, .. } => *generation,
            _ => 0,
        };
        let Some(link) = client.as_mut() else {
            let _ = out.send(Got::Trouble {
                generation,
                why: format!("no service at {addr}"),
            });
            continue;
        };
        let request = match ask {
            Ask::Search {
                query,
                sort,
                descending,
                offset,
                limit,
                cap,
                ..
            } => Request::Search {
                query,
                sort,
                descending,
                page: Page {
                    offset,
                    limit,
                    count_cap: cap,
                    ..Page::default()
                },
            },
            Ask::Facets { query, age, .. } => Request::Facets {
                query,
                by: if age {
                    vec![FacetBy::Age {
                        edges: scour_ui::bar_edges(),
                    }]
                } else {
                    vec![FacetBy::Kind]
                },
            },
            Ask::Count { query, .. } => Request::Count {
                query,
                // **The whole answer, whatever it costs.** This is the number
                // that says a filter did something, and it goes out once the
                // typing has stopped rather than on every keystroke — where
                // an uncapped count was measured at eighteen milliseconds of a
                // thirty-seven millisecond keystroke.
                cap: u32::MAX,
            },
            Ask::Await { since } => Request::Await {
                since,
                // Long enough that an idle terminal is nearly silent — two
                // requests a minute — and short enough that a service
                // restarted underneath is noticed.
                timeout_ms: 30_000,
            },
            Ask::Places => Request::Places {},
            Ask::Rules => Request::Rules {},
            Ask::OffRules(off) => Request::SetSettings {
                change: scour_settings::Change {
                    exclude_off: Some(off),
                    ..Default::default()
                },
            },
            Ask::Remember(change) => Request::SetSettings { change },
            Ask::Export { query, to } => {
                // The one request answered in pieces, so it cannot go through
                // `call` — see `Client::stream`.
                match export(link, &query, &to) {
                    Ok(()) => {
                        let _ = out.send(Got::Wrote(to));
                    }
                    Err(why) => {
                        let _ = out.send(Got::Failed(why));
                    }
                }
                continue;
            }
            Ask::Done => return,
        };
        let offsets = match &request {
            Request::Search { page, .. } => (page.offset, page.limit),
            _ => (0, 0),
        };
        match link.call(request) {
            Ok(Response::Search(reply)) => {
                let _ = out.send(Got::Search {
                    generation,
                    offset: offsets.0,
                    limit: offsets.1,
                    reply: Box::new(reply),
                });
            }
            Ok(Response::Facets(reply)) => {
                let _ = out.send(Got::Facets {
                    generation,
                    reply: Box::new(reply),
                });
            }
            Ok(Response::Rules {
                builtin_paths,
                builtin_dirs,
                builtin_files,
                config_paths,
                config_dirs,
                config_files,
                config_allow,
                added_paths,
                added_dirs,
                added_files,
                added_allow,
                off,
            }) => {
                let group = |rows: Vec<(&str, Vec<String>)>| -> Vec<(String, String)> {
                    rows.into_iter()
                        .flat_map(|(kind, list)| {
                            list.into_iter().map(move |v| (kind.to_string(), v))
                        })
                        .collect()
                };
                let _ = out.send(Got::Rules {
                    added: group(vec![
                        ("path", added_paths),
                        ("dir", added_dirs),
                        ("file", added_files),
                        ("allow", added_allow),
                    ]),
                    config: group(vec![
                        ("path", config_paths),
                        ("dir", config_dirs),
                        ("file", config_files),
                        ("allow", config_allow),
                    ]),
                    builtin: group(vec![
                        ("path", builtin_paths),
                        ("dir", builtin_dirs),
                        ("file", builtin_files),
                    ]),
                    off,
                });
            }
            // Settings come back as the whole object; nothing here reads it,
            // and asking again is how anything checks what took.
            Ok(Response::Settings(_)) => {}
            Ok(Response::Count { total, .. }) => {
                let _ = out.send(Got::Counted { generation, total });
            }
            Ok(Response::Status(st)) => {
                let _ = out.send(Got::Awake(st.revision));
            }
            Ok(Response::Places(places)) => {
                let _ = out.send(Got::Places(
                    places
                        .places
                        .into_iter()
                        .map(|p| (p.label, p.path))
                        .collect(),
                ));
            }
            Ok(other) => {
                let _ = out.send(Got::Trouble {
                    generation,
                    why: format!("unexpected reply: {other:?}"),
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
