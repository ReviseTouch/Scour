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

/// **Not boxed**: one per user action, crossing a channel once.
#[allow(clippy::large_enum_variant)]
/// What the terminal asks for.
pub enum Ask {
    /// Look at these paths again, now — this program moved them.
    Recheck(Vec<String>),
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
    /// What the index holds, for the report.
    Stats,
    /// The same file, several times over.
    Dupes,
    /// What a folder weighs, and which of its children weigh the most.
    Usage { path: String },
    /// What can be shown of a file, and the head of it when that is text.
    Preview { path: String },
    /// The query read back: which run of it is what.
    ///
    /// **Beside every search**, because the colouring has to keep up with the
    /// typing — and it is cheap: the parser, not the index.
    Explain { generation: u64, query: String },
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
    /// What the index holds: rows, directories, bytes on disk, sources.
    Stats(Box<scour_core::IndexStats>),
    /// A weighed folder: what it holds, and its heaviest children.
    Usage(Box<scour_core::UsageResponse>),
    /// The head of a file, and what shape it is.
    Peek(Box<scour_preview::Look>),
    /// Duplicate groups, largest saving first, and what they come to.
    Dupes {
        groups: Vec<(u64, u64, String)>,
        waste: u64,
    },
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
    /// The query cut into runs: where each starts, how long, and what it is.
    Explained {
        generation: u64,
        spans: Vec<scour_core::Span>,
    },
    /// The index moved, and what it moved to.
    /// The index moved: its revision, and how far a walk has got when one is
    /// running. **Both, because a wait is answered with the whole status** —
    /// the second is free and is the only thing a person watching a rule they
    /// just switched off has to go on.
    ///
    /// The third is whether the index has grown an unsorted tail worth
    /// rebuilding. Also free, and it had been reaching the command line and
    /// nowhere else — a week of ordinary use takes ordering by path from
    /// 1.9 ms to 21.5, and nothing in a terminal said so.
    Awake(u64, Option<u64>, bool),
    /// A spreadsheet is being written, and how much of it so far.
    ///
    /// **Because a screen that does not move looks like one that has died.**
    /// The whole index is four hundred thousand rows and a hundred megabytes;
    /// during that the terminal had nothing new to draw and somebody
    /// reasonably read it as a crash.
    Writing(u64),
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
    /// The slow lane's thread, so that what was queued on it can be waited
    /// for.
    ///
    /// **Only that one.** The fast lane answers before anybody could leave and
    /// the long poll is asleep in a thirty-second call; this is the lane that
    /// carries the two things somebody might quit immediately after asking
    /// for — a preference, and a spreadsheet being written.
    slow_thread: std::sync::Mutex<Option<thread::JoinHandle<()>>>,
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
        let slow_thread = thread::spawn(move || serve(&slow_addr, &waiting, &slow_out));
        thread::spawn(move || serve(&addr, &dozing, &gots));
        (
            Link {
                asks,
                slow,
                wait,
                slow_thread: std::sync::Mutex::new(Some(slow_thread)),
            },
            answers,
        )
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

    /// Let the slow lane finish what it was given, then go.
    ///
    /// **A queued request is not a sent one.** Switching to another face
    /// writes which face to open next and then quits; the write is a message
    /// on a channel, and a process that exits the moment after leaves it
    /// there. So do quitting after an export, and the file is a file that was
    /// never written.
    ///
    /// Bounded, because a lane whose service has gone away must not keep a
    /// terminal on screen: half a second is more than a socket write and less
    /// than anybody notices.
    pub fn finish(&self) {
        let _ = self.slow.send(Ask::Done);
        let _ = self.asks.send(Ask::Done);
        let handle = self.slow_thread.lock().ok().and_then(|mut h| h.take());
        let Some(handle) = handle else { return };
        let waited = std::time::Instant::now();
        while !handle.is_finished() && waited.elapsed() < std::time::Duration::from_millis(500) {
            thread::sleep(std::time::Duration::from_millis(10));
        }
        if handle.is_finished() {
            let _ = handle.join();
        }
    }
}

/// Write the whole result to a file, in the pieces the service sends it in.
///
/// **Not through `call`.** This is the one request answered in more than one
/// frame, and reading only the first would leave the rest in the buffer for
/// the next question to be answered by.
fn export(link: &mut Client, query: &str, to: &str, out: &Sender<Got>) -> Result<(), String> {
    use std::io::Write;
    let mut file =
        std::io::BufWriter::new(std::fs::File::create(to).map_err(|e| format!("{to}: {e}"))?);
    let mut trouble: Option<String> = None;
    let mut bytes = 0u64;
    let mut said = 0u64;
    link.stream(
        Request::Export {
            query: query.to_string(),
            columns: Vec::new(),
        },
        |piece| match piece {
            Response::ExportChunk { csv } => match file.write_all(csv.as_bytes()) {
                Ok(()) => {
                    bytes += csv.len() as u64;
                    // Every megabyte, which is often enough to look alive and
                    // seldom enough to cost nothing.
                    if bytes - said > 1_000_000 {
                        said = bytes;
                        let _ = out.send(Got::Writing(bytes));
                    }
                    true
                }
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
            | Ask::Explain { generation, .. }
            | Ask::Count { generation, .. } => *generation,
            _ => 0,
        };
        let Some(link) = client.as_mut() else {
            // A msgid rather than a sentence: `App::upset` puts it through
            // the catalogue. The socket it failed on is in `SCOUR_TUI_TRACE`
            // and in the config; what a reader needs on this line is that
            // there is nothing to search.
            let _ = out.send(Got::Trouble {
                generation,
                why: "the service cannot be reached — is scourd running?".into(),
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
            Ask::Explain { query, .. } => Request::Explain {
                query,
                // No caret in a terminal's own idea of the query line — the
                // completions this could return are not drawn yet.
                cursor: None,
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
            Ask::Stats => Request::Stats {},
            Ask::Preview { path } => Request::Preview { path },
            Ask::Usage { path } => Request::Usage {
                path,
                top: 12,
                query: String::new(),
            },
            Ask::Dupes => Request::Duplicates {
                under: String::new(),
                // The service's own floor and budget: the report is the same
                // report in every face, and a terminal that asked for a
                // different one would answer a different question.
                min_size: 1_048_576,
                read_budget: 64 * 1_048_576,
                top: 12,
            },
            Ask::Rules => Request::Rules {},
            Ask::Recheck(paths) => Request::Recheck { paths },
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
                match export(link, &query, &to, out) {
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
            Ok(Response::Explain { spans, .. }) => {
                let _ = out.send(Got::Explained { generation, spans });
            }
            Ok(Response::Count { total, .. }) => {
                let _ = out.send(Got::Counted { generation, total });
            }
            Ok(Response::Status(st)) => {
                let _ = out.send(Got::Awake(
                    st.revision,
                    st.scanning.then_some(st.scanned),
                    st.rebuild_advised,
                ));
            }
            Ok(Response::Preview(look)) => {
                let _ = out.send(Got::Peek(Box::new(look)));
            }
            Ok(Response::Usage(usage)) => {
                let _ = out.send(Got::Usage(Box::new(usage)));
            }
            Ok(Response::Stats(stats)) => {
                let _ = out.send(Got::Stats(Box::new(stats)));
            }
            Ok(Response::Duplicates { groups, waste, .. }) => {
                let _ = out.send(Got::Dupes {
                    groups: groups
                        .into_iter()
                        .map(|g| {
                            let count = g.paths.len() as u64;
                            (g.size, count, g.paths.first().cloned().unwrap_or_default())
                        })
                        .collect(),
                    waste,
                });
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
