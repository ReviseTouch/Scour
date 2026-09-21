//! The connection to the service, on a thread of its own.
//!
//! The interface never waits: requests go out on worker threads and answers
//! arrive as events on the keyboard's channel. Three lanes, because `scour-ipc`
//! is one call at a time and a 20 ms facet count must not queue before a key.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use scour_core::{FacetBy, Page, SortKey};
use scour_ipc::Client;
use scour_proto::{Request, Response};

/// How many matches an interactive search counts before it stops; the exact
/// figure follows once the typing settles.
pub const TYPING_CAP: u32 = 1_000;

/// What to do about the desktop's key that opens Scour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deed {
    /// What the key is now.
    Read,
    /// Bind this combination, as it was typed: `scour-hotkey` owns the spelling.
    Bind(String),
    Clear,
}

/// What the terminal asks for. Not boxed: one per user action, crossing a
/// channel once.
#[allow(clippy::large_enum_variant)]
pub enum Ask {
    /// Look at these paths again, now — this program moved them.
    Recheck(Vec<String>),
    Search {
        /// Which keystroke this belongs to. An answer to an older one is
        /// dropped, or the list goes backwards under somebody's hands.
        generation: u64,
        query: String,
        sort: SortKey,
        descending: bool,
        offset: u32,
        limit: u32,
        cap: u32,
    },
    /// What the rail shows: the kinds, and the time strip's bands. On a lane of
    /// its own: a facet count walks the matching set, and a keystroke must not
    /// queue behind those 20 ms.
    Facets {
        generation: u64,
        query: String,
        /// Which of the two questions this is; they count over different rows.
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
    /// What the weighed folder is made of, by kind. Counted over the report's
    /// scope rather than over the search, so the two questions never mix.
    Kinds { path: String },
    /// What can be shown of a file, and the head of it when that is text.
    Preview { path: String },
    /// The query read back: which run of it is what. Sent beside every search;
    /// it costs the parser, not the index.
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
    /// The desktop's key. Reaches no service — but travels this lane even so:
    /// binding runs `gsettings`, and a process must not start on the draw path.
    Hotkey(Deed),
    /// Stop: the terminal is closing.
    Done,
}

/// What comes back.
pub enum Got {
    /// A page of a search, with the offset it was asked for: two offsets of one
    /// query are both current, so the answer carries its own.
    Search {
        generation: u64,
        offset: u32,
        limit: u32,
        reply: Box<scour_core::SearchResponse>,
    },
    /// The rail's counts, or the strip's.
    Facets {
        generation: u64,
        /// Which of the two was asked; both groups come back in either reply.
        age: bool,
        reply: Box<scour_core::FacetResponse>,
    },
    /// The desktop's folders, and where the volumes are. The mounts are for the
    /// `Accessed` column: on a `noatime` volume it holds a creation time, and
    /// the table draws a dash instead.
    Places(Vec<(String, String)>, Vec<scour_places::Mount>),
    /// What the index holds: rows, directories, bytes on disk, sources.
    Stats(Box<scour_core::IndexStats>),
    /// A weighed folder: what it holds, and its heaviest children.
    Usage(Box<scour_core::UsageResponse>),
    /// The report's kinds, as `(token, count)`, largest first.
    Kinds(Vec<(String, u64)>),
    /// The head of a file, and what shape it is.
    Peek(Box<scour_preview::Look>),
    /// Duplicate groups, largest saving first, and what they come to.
    Dupes {
        groups: Vec<(u64, u64, String)>,
        waste: u64,
    },
    /// The skip rules: three groups of `(kind, value)`, and the ids switched
    /// off. Kept apart because only the first group can be deleted.
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
    /// The index moved: its revision, how far a walk has got when one is
    /// running, and whether the index has grown an unsorted tail worth
    /// rebuilding — a week of use takes ordering by path from 1.9 ms to 21.5.
    Awake(u64, Option<u64>, bool),
    /// A spreadsheet is being written, and how much of it so far: the whole
    /// index is 400,000 rows and 100 MB, and a still screen reads as a crash.
    Writing(u64),
    /// A spreadsheet was written, and where.
    Wrote(String),
    /// The desktop's key: what it is now, what it runs, and whether this
    /// program can write one here at all.
    Hotkey {
        desktop: String,
        key: Option<String>,
        command: String,
        can_bind: bool,
        /// Why the last deed was refused; empty when it was not.
        trouble: String,
        /// Written, but dead until the next login — KDE.
        later: bool,
    },
    /// Something that is not about a search went wrong. Not a `Trouble`, which
    /// is dropped when its keystroke is old — wrong for a file that failed.
    Failed(String),
    /// Nothing was listening, so a service is being started; the lanes are
    /// waiting on it.
    Booting,
    /// That attempt ended. `Some` is why nothing is listening even so.
    Booted(Option<String>),
    /// The service could not be reached, or said no.
    Trouble { generation: u64, why: String },
}

/// The service, at the far end of a thread.
pub struct Link {
    asks: Sender<Ask>,
    slow: Sender<Ask>,
    wait: Sender<Ask>,
    /// The slow lane's thread, so what was queued on it can be waited for. Only
    /// this one: it carries a preference and an export, either of which somebody
    /// may quit immediately after asking for.
    slow_thread: std::sync::Mutex<Option<thread::JoinHandle<()>>>,
}

impl Link {
    /// Start talking to the service at `addr`; answers go to the returned
    /// receiver, which the event loop selects on alongside the keyboard. A
    /// service that is not there is started, with `log` for its output.
    pub fn start(addr: String, log: std::path::PathBuf) -> (Link, Receiver<Got>) {
        let (asks, inbox) = channel::<Ask>();
        let (slow, waiting) = channel::<Ask>();
        let (wait, dozing) = channel::<Ask>();
        let (gots, answers) = channel::<Got>();
        // Said from a thread of its own: the lanes below block inside `boot`,
        // and the line has to be on screen before they do.
        if scour_launch::wanted() && !scour_ipc::is_running(&addr) {
            let (addr, log, out) = (addr.clone(), log.clone(), gots.clone());
            thread::spawn(move || {
                let _ = out.send(Got::Booting);
                let outcome = boot(&addr, &log);
                let _ = out.send(Got::Booted(outcome.reason().map(str::to_string)));
            });
        }
        let fast_addr = addr.clone();
        let fast_out = gots.clone();
        let fast_log = log.clone();
        thread::spawn(move || serve(&fast_addr, &fast_log, &inbox, &fast_out));
        // Three connections, because `scour-ipc` is one call at a time and the
        // long poll holds its own for thirty seconds.
        let slow_addr = addr.clone();
        let slow_out = gots.clone();
        let slow_log = log.clone();
        let slow_thread = thread::spawn(move || serve(&slow_addr, &slow_log, &waiting, &slow_out));
        thread::spawn(move || serve(&addr, &log, &dozing, &gots));
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
        // A closed channel means the thread is gone, which is shutdown only.
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

    /// Let the slow lane finish what it was given, then go: a queued export or
    /// preference is not a sent one. Bounded at 500 ms, which is more than a
    /// socket write and less than anybody notices.
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
/// Not through `call`: it is the one request answered in more than one frame.
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
                    // Every megabyte: often enough to look alive, cheap enough.
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

/// Have a service at `addr`, starting one if nobody else has. At most once for
/// the whole program, whichever lane arrives first; the others wait here.
fn boot(addr: &str, log: &std::path::Path) -> scour_launch::Outcome {
    scour_launch::ensure_once(&scour_launch::Autostart::new(
        addr,
        log.to_path_buf(),
        &scour_ipc::is_running,
    ))
}

/// Ask the desktop about its key, or change it. Every decision is
/// `scour-hotkey`'s; this turns the answer into an event.
fn hotkey(deed: &Deed) -> Got {
    use scour_hotkey::{Applies, Desktop, Hotkey, Key, Status};
    let hk = Hotkey::detect();
    let mut trouble = String::new();
    let mut later = false;
    match deed {
        Deed::Read => {}
        Deed::Bind(text) => match Key::parse(text) {
            Ok(key) => match hk.bind(&key) {
                Ok(applies) => later = applies == Applies::AfterLogin,
                Err(e) => trouble = e.to_string(),
            },
            Err(e) => trouble = e.to_string(),
        },
        Deed::Clear => {
            if let Err(e) = hk.clear() {
                trouble = e.to_string();
            }
        }
    }
    // Read back afterwards: what the desktop says it is now is the answer, not
    // what was written at it.
    let key = match hk.status() {
        Ok(Status::Bound(k)) => Some(k.to_string()),
        Ok(_) => None,
        Err(e) => {
            if trouble.is_empty() {
                trouble = e.to_string();
            }
            None
        }
    };
    Got::Hotkey {
        // Names, not words: nothing here goes through the catalogue.
        desktop: match hk.desktop() {
            Desktop::Gnome => "GNOME",
            Desktop::Kde => "KDE",
            Desktop::Flatpak => "Flatpak",
            Desktop::Other => "other",
        }
        .to_owned(),
        key,
        command: hk.command(),
        can_bind: hk.can_bind(),
        trouble,
        later,
    }
}

fn serve(addr: &str, log: &std::path::Path, inbox: &Receiver<Ask>, out: &Sender<Got>) {
    let mut client: Option<Client> = None;
    while let Ok(ask) = inbox.recv() {
        if matches!(ask, Ask::Done) {
            return;
        }
        // Before the socket, because this one needs none: a machine with no
        // service running still answers about its keyboard.
        if let Ask::Hotkey(deed) = &ask {
            let _ = out.send(hotkey(deed));
            continue;
        }
        // Reconnect on every failure rather than once at startup: the service
        // is restarted far more often than the terminal is.
        if client.is_none() {
            client = Client::connect(addr).ok();
            if client.is_none() {
                boot(addr, log);
                client = Client::connect(addr).ok();
            }
        }
        let generation = match &ask {
            Ask::Search { generation, .. }
            | Ask::Facets { generation, .. }
            | Ask::Explain { generation, .. }
            | Ask::Count { generation, .. } => *generation,
            _ => 0,
        };
        let Some(link) = client.as_mut() else {
            // A msgid, not a sentence: `App::upset` puts it through the
            // catalogue. The socket it failed on is in `SCOUR_TUI_TRACE`.
            let _ = out.send(Got::Trouble {
                generation,
                why: "the service cannot be reached — is scourd running?".into(),
            });
            continue;
        };
        let for_age = matches!(ask, Ask::Facets { age: true, .. });
        // The report asks the same question over its own scope; the answer
        // goes to the report's block, not to the rail.
        let for_report = matches!(ask, Ask::Kinds { .. });
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
            // Both groups in both requests: the scan cap follows the questions
            // asked, so asking for kinds alone samples the first 200,000 rows
            // the walk reaches — a rail off by a sixth. The walk is the cost.
            Ask::Facets { query, .. } => Request::Facets {
                query,
                by: vec![
                    FacetBy::Kind,
                    FacetBy::Age {
                        edges: scour_ui::bar_edges(),
                    },
                ],
            },
            Ask::Explain { query, .. } => Request::Explain {
                query,
                // No caret: the completions this could return are not drawn.
                cursor: None,
            },
            Ask::Count { query, .. } => Request::Count {
                query,
                // The whole answer, whatever it costs: uncapped counting is
                // 18 ms of a 37 ms keystroke, so it waits for the typing to
                // stop.
                cap: u32::MAX,
            },
            Ask::Await { since } => Request::Await {
                since,
                // Two requests a minute when idle, and short enough that a
                // service restarted underneath is noticed.
                timeout_ms: 30_000,
            },
            Ask::Places => Request::Places {},
            Ask::Stats => Request::Stats {},
            Ask::Preview { path } => Request::Preview { path },
            // The whole scope and both groups: an age group is what lifts the
            // 200,000-row sampling cap, and a report that samples is a report
            // whose percentages are not the folder's.
            Ask::Kinds { path } => Request::Facets {
                query: match path.is_empty() {
                    true => String::new(),
                    false => format!("under:\"{path}\""),
                },
                by: vec![
                    FacetBy::Kind,
                    FacetBy::Age {
                        edges: scour_ui::bar_edges(),
                    },
                ],
            },
            Ask::Usage { path } => Request::Usage {
                path,
                top: 12,
                query: String::new(),
            },
            Ask::Dupes => Request::Duplicates {
                under: String::new(),
                // The service's own floor and budget: one report in every face.
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
                // Answered in pieces, so not through `call` — `Client::stream`.
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
            // Answered above, before the connection. Unreachable rather than a
            // silent fallback: reaching it means that branch is gone.
            Ask::Hotkey(_) => unreachable!("the desktop's key is answered without a socket"),
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
            Ok(Response::Facets(reply)) if for_report => {
                let mut kinds: Vec<(String, u64)> = reply
                    .groups
                    .into_iter()
                    .filter(|g| matches!(g.by, FacetBy::Kind))
                    .flat_map(|g| g.facets)
                    .map(|f| (f.key, f.count))
                    .collect();
                kinds.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
                let _ = out.send(Got::Kinds(kinds));
            }
            Ok(Response::Facets(reply)) => {
                let _ = out.send(Got::Facets {
                    generation,
                    age: for_age,
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
            // Nothing here reads the settings object; asking again is the check.
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
                let mounts = places.mounts.clone();
                let _ = out.send(Got::Places(
                    places
                        .places
                        .into_iter()
                        .map(|p| (p.label, p.path))
                        .collect(),
                    mounts,
                ));
            }
            Ok(other) => {
                let _ = out.send(Got::Trouble {
                    generation,
                    why: format!("unexpected reply: {other:?}"),
                });
            }
            Err(e) => {
                // A refused query and a dead socket arrive the same way, so the
                // connection goes either way: reconnecting is ~100 µs.
                client = None;
                let _ = out.send(Got::Trouble {
                    generation,
                    why: e.to_string(),
                });
            }
        }
    }
}
