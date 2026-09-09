//! The connection to the service, on threads of its own: the window never
//! waits, and answers arrive as events. Three lanes, because `scour-ipc` is one
//! call at a time with no cancellation — interactive, background, and one for
//! `Await`, which is meant to block.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc::{Receiver, Sender, channel},
};

use scour_ipc::Client;
use scour_proto::{Request, Response};

/// How many matches an interactive search counts before it stops. Enough for a
/// meter to say "at least this many"; the exact figure follows.
pub const TYPING_CAP: u32 = 1_000;

/// What the window asks for. Not boxed: one per user action, one channel hop.
#[allow(clippy::large_enum_variant)]
pub enum Ask {
    /// A search, tagged with the interaction that caused it. The tag is what
    /// makes a stale answer droppable, so the list never goes backwards.
    Search {
        generation: u64,
        query_revision: u64,
        query: String,
        sort: String,
        descending: bool,
        offset: u32,
        limit: u32,
    },
    /// The rail's kinds and the ribbon's ages. Each is counted over the query
    /// with its own term taken out; when those two strings are equal — nearly
    /// always — one request answers both, and `half` says so.
    Facets {
        query_revision: u64,
        query: String,
        half: Half,
    },
    /// Read the query back: the runs, and what each one is. Sent beside every
    /// search, on the slow lane, since only the newest colouring is ever seen.
    Explain {
        query_revision: u64,
        query: String,
    },
    /// What a folder weighs, and which of its children weigh the most: the
    /// scope's own total, its children heaviest first, each split by age.
    Usage {
        path: String,
    },
    /// What kinds the weight under a folder is in.
    Kinds {
        path: String,
    },
    /// The heaviest files under a folder.
    Biggest {
        path: String,
    },
    /// The same file, several times over, under a folder.
    Dupes {
        under: String,
        min_size: u64,
        read_budget: u64,
    },
    /// What the walk skips, in three groups.
    Rules,
    /// Wait until the index is no longer at `since`, then say so. This is what
    /// makes the list live: the window searches again and asks to wait once more.
    Await {
        since: u64,
    },
    /// How big the index is, how many sources, how many watched. Asked once.
    Status,
    /// Keep a choice: the language, the view shape, a column width. Fire and
    /// forget, and kept by the service so every face reads the same settings.
    Remember {
        change: scour_settings::Change,
    },
    /// What can be shown of this file. The service decides what a file is: that
    /// needs its first eight kilobytes, not a guess from the name.
    Peek {
        path: String,
    },
    /// Everything known about one file, for the preview panel's fact list. Not
    /// off the row: created, read, mode and owner are in no column.
    PeekFacts {
        path: String,
    },
    /// Make thumbnails for these files, if the desktop declares something that
    /// can. Asked of the service, which holds the bound — four at once on the
    /// machine, not four per face — and decides which paths may be touched.
    Thumbnails {
        files: Vec<String>,
    },
    /// This desktop's own folders, asked once at start-up. Of the service, not
    /// of `scour-places`: a frontend asks, and all four get the same answer.
    Places,
    Count {
        query_revision: u64,
        query: String,
    },
    Stop,
}

/// What comes back.
pub enum Got {
    Search {
        generation: u64,
        /// Where the page this answers begins. Carried, not remembered: a page
        /// fetch does not advance the generation, so two offsets are both current.
        offset: u32,
        /// How many rows were asked for, so a page that comes back short can
        /// be told from one that came back full.
        limit: u32,
        reply: Box<Response>,
    },
    Facets {
        query_revision: u64,
        /// Which half of this answer the window asked for. Both groups are always
        /// in the reply — asking for kinds alone samples, see `AGE_SCAN_CAP`.
        half: Half,
        reply: Box<Response>,
    },
    Count {
        query_revision: u64,
        reply: Box<Response>,
    },
    Places(Box<Response>),
    Usage {
        /// The folder this weighs, so a slow answer for a folder nobody is
        /// looking at any more can be dropped.
        path: String,
        reply: Box<Response>,
    },
    Kinds {
        path: String,
        reply: Box<Response>,
    },
    Biggest {
        path: String,
        reply: Box<Response>,
    },
    Dupes(Box<Response>),
    /// What can be shown of one file. Carries the path so an answer about a
    /// row nobody is looking at any more can be dropped.
    Peek {
        path: String,
        reply: Box<Response>,
    },
    Thumbnails(Box<Response>),
    Rules(Box<Response>),
    Status(Box<Response>),
    Awake(Box<Response>),
    Explain {
        query_revision: u64,
        reply: Box<Response>,
    },
    /// The service answered, and the answer was no. Distinct from [`Got::Down`]:
    /// a refused query leaves a perfectly good connection open.
    Refused {
        revision: ReplyRevision,
        why: String,
    },
    /// The service could not be reached, with the reason as a sentence.
    Down(String),
    /// It could, after having been down.
    Up,
}

/// The freshness domain of a rejected request. Ordering advances a search
/// generation without changing the query revision, so the two stay apart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplyRevision {
    Search(u64),
    Query(u64),
}

pub struct Link {
    fast: Sender<Ask>,
    slow: Sender<Ask>,
    /// A lane of its own: `Await` sits on the socket until the index moves or
    /// the timeout runs out, and would hold every keystroke behind it.
    wait: Sender<Ask>,
    freshness: Freshness,
}

/// The newest work the UI can still use. Visible to every lane, so a request
/// queued behind a slow call is discarded before it reaches the service.
#[derive(Clone, Default)]
struct Freshness {
    search: Arc<AtomicU64>,
    query: Arc<AtomicU64>,
}

impl Freshness {
    fn note(&self, ask: &Ask) {
        match ask {
            Ask::Search {
                generation,
                query_revision,
                ..
            } => {
                self.search.fetch_max(*generation, Ordering::Release);
                self.query.fetch_max(*query_revision, Ordering::Release);
            }
            // Everything carrying a revision, not only the search: `Explain`
            // goes out a moment before its search, and would look stale.
            Ask::Explain { query_revision, .. }
            | Ask::Facets { query_revision, .. }
            | Ask::Count { query_revision, .. } => {
                self.query.fetch_max(*query_revision, Ordering::Release);
            }
            _ => {}
        }
    }

    fn accepts(&self, ask: &Ask) -> bool {
        match ask {
            Ask::Search { generation, .. } => *generation == self.search.load(Ordering::Acquire),
            Ask::Facets { query_revision, .. }
            | Ask::Count { query_revision, .. }
            | Ask::Explain { query_revision, .. } => {
                *query_revision == self.query.load(Ordering::Acquire)
            }
            // Asked once and never superseded.
            Ask::Places
            | Ask::Rules
            | Ask::Status
            | Ask::Await { .. }
            | Ask::Remember { .. }
            | Ask::Usage { .. }
            | Ask::Kinds { .. }
            | Ask::Biggest { .. }
            | Ask::Thumbnails { .. }
            // Dropped on arrival instead: the reply carries its path.
            | Ask::Peek { .. }
            | Ask::PeekFacts { .. }
            | Ask::Dupes { .. } => true,
            Ask::Stop => true,
        }
    }
}

impl Link {
    /// Start the two lanes. `sink` is called from the worker threads.
    pub fn start(addr: String, sink: impl Fn(Got) + Send + Clone + 'static) -> Link {
        let (fast_tx, fast_rx) = channel::<Ask>();
        let (slow_tx, slow_rx) = channel::<Ask>();
        let freshness = Freshness::default();

        let (wait_tx, wait_rx) = channel::<Ask>();
        spawn_lane(addr.clone(), fast_rx, sink.clone(), freshness.clone(), true);
        spawn_lane(
            addr.clone(),
            slow_rx,
            sink.clone(),
            freshness.clone(),
            false,
        );
        spawn_lane(addr, wait_rx, sink, freshness.clone(), false);
        Link {
            fast: fast_tx,
            slow: slow_tx,
            wait: wait_tx,
            freshness,
        }
    }

    pub fn send(&self, ask: Ask) {
        // Published before enqueueing, so a worker can skip an older queued
        // request before the new one has reached it.
        self.freshness.note(&ask);
        let lane = match &ask {
            Ask::Await { .. } => &self.wait,
            // Never the fast lane: it coalesces to the newest queued request,
            // which swallows anything asked once. All of these are cheap.
            Ask::Facets { .. }
            | Ask::Count { .. }
            | Ask::Places
            | Ask::Rules
            | Ask::Status
            | Ask::Remember { .. }
            | Ask::Usage { .. }
            | Ask::Kinds { .. }
            | Ask::Biggest { .. }
            | Ask::Dupes { .. }
            // Several processes decoding video; nothing may queue behind it.
            | Ask::Thumbnails { .. }
            // And this one reads the head of a file off a disk.
            | Ask::Peek { .. }
            | Ask::PeekFacts { .. }
            | Ask::Explain { .. } => &self.slow,
            _ => &self.fast,
        };
        // A closed channel means the lane died; the `Down` event says so.
        let _ = lane.send(ask);
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let _ = self.fast.send(Ask::Stop);
        let _ = self.slow.send(Ask::Stop);
        let _ = self.wait.send(Ask::Stop);
    }
}

/// Which part of the sidebar a facet question is for. The rail and the ribbon
/// are each counted without the term they set, so pressing a bar moves the
/// filter rather than emptying the chart; equal strings mean `Both`, one walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Half {
    Both,
    Kinds,
    Ages,
}

impl Half {
    pub fn kinds(self) -> bool {
        self != Half::Ages
    }
    pub fn ages(self) -> bool {
        self != Half::Kinds
    }
}

/// Which kind of answer a request is going to produce.
#[derive(Clone, Copy)]
enum Lane {
    Search,
    Facets,
    Count,
    /// This desktop's folders, asked once and never superseded.
    Places,
    /// The query read back, for the colouring.
    Explain,
    /// What a folder weighs.
    Usage,
    /// What kinds are under it.
    Kinds,
    /// The heaviest files under it.
    Biggest,
    /// The same file, several times over.
    Dupes,
    /// Pictures the desktop has been asked to make.
    Thumbnails,
    /// What can be shown of one file.
    Peek,
    /// The exclusion rules.
    Rules,
    /// What the service is holding.
    Status,
    /// The long poll that keeps the list live.
    Await,
}

/// One lane: connect, serve, reconnect when the service comes back.
fn spawn_lane(
    addr: String,
    rx: Receiver<Ask>,
    sink: impl Fn(Got) + Send + 'static,
    freshness: Freshness,
    coalesce: bool,
) {
    std::thread::spawn(move || {
        let mut client: Option<Client> = None;
        let mut was_down = false;
        for first in &rx {
            let ask = if coalesce {
                newest_queued(first, &rx)
            } else {
                first
            };
            let ask = match ask {
                Ask::Stop => break,
                other => other,
            };
            if !freshness.accepts(&ask) {
                continue;
            }
            // Reconnect lazily: the window only cares when it has something to ask.
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
            // Connecting can outlast a key interval, so recheck freshness at the
            // last point before the call.
            if !freshness.accepts(&ask) {
                continue;
            }
            let Some(c) = client.as_mut() else { continue };
            // Taken before the match consumes the request: an answer for a folder
            // nobody is looking at any more is dropped rather than drawn.
            let weighed = match &ask {
                Ask::Usage { path }
                | Ask::Kinds { path }
                | Ask::Biggest { path }
                | Ask::Peek { path }
                | Ask::PeekFacts { path } => path.clone(),
                _ => String::new(),
            };
            // Taken before the match consumes the request, as `weighed` is.
            let half = match ask {
                Ask::Facets { half, .. } => half,
                _ => Half::Both,
            };
            let (revision, request, facets) = match ask {
                Ask::Search {
                    generation,
                    query_revision: _,
                    query,
                    sort,
                    descending,
                    offset,
                    limit,
                } => (
                    generation,
                    Request::Search {
                        query,
                        sort: sort_of(&sort),
                        descending,
                        page: scour_core::Page {
                            offset,
                            limit,
                            // How many matches the walk counts before it stops.
                            // At 100,000 `ra` visits 1,208,951 rows (23.1 ms);
                            // at a thousand, 35,743 (0.9 ms). The meter says
                            // `1000+` until the exact count arrives.
                            count_cap: TYPING_CAP,
                        },
                    },
                    Lane::Search,
                ),
                Ask::Facets {
                    query_revision,
                    query,
                    half: _,
                } => (
                    query_revision,
                    // Both in one request: the same walk over the same matching
                    // set, and two walks could disagree about a changing query.
                    Request::Facets {
                        query,
                        by: vec![
                            scour_core::FacetBy::Kind,
                            scour_core::FacetBy::Age {
                                edges: scour_ui::bar_edges(),
                            },
                        ],
                    },
                    Lane::Facets,
                ),
                Ask::Explain {
                    query_revision,
                    query,
                } => (
                    query_revision,
                    Request::Explain {
                        query,
                        cursor: None,
                    },
                    Lane::Explain,
                ),
                Ask::Remember { change } => (0, Request::SetSettings { change }, Lane::Places),
                Ask::Usage { ref path } => (
                    0,
                    Request::Usage {
                        path: path.clone(),
                        // Enough children that the heaviest handful is honest,
                        // few enough that ten thousand of them fit a screen.
                        top: 24,
                        query: String::new(),
                    },
                    Lane::Usage,
                ),
                Ask::Kinds { ref path } => (
                    0,
                    Request::Facets {
                        query: under(path),
                        // The age is asked for and thrown away: a kind count
                        // alone is capped at 200,000 rows, and rows are stored
                        // newest first, so a cap is a prefix and not a sample.
                        // Asking for a distribution too lifts the cap.
                        by: vec![
                            scour_core::FacetBy::Kind,
                            scour_core::FacetBy::Age {
                                edges: scour_ui::bar_edges(),
                            },
                        ],
                    },
                    Lane::Kinds,
                ),
                Ask::Biggest { ref path } => (
                    0,
                    Request::Search {
                        // `file:` and not `!is:dir`: sorted by size a folder is
                        // ordered by what is under it, not by its own size.
                        query: match under(path).as_str() {
                            "" => "file:".to_owned(),
                            scope => format!("{scope} file:"),
                        },
                        sort: scour_core::SortKey::Size,
                        descending: true,
                        page: scour_core::Page {
                            offset: 0,
                            limit: 8,
                            // Nothing here reads the total, and counting is the
                            // one cost proportional to how many match.
                            count_cap: 1,
                        },
                    },
                    Lane::Biggest,
                ),
                Ask::Dupes {
                    ref under,
                    min_size,
                    read_budget,
                } => (
                    0,
                    Request::Duplicates {
                        under: under.clone(),
                        min_size,
                        read_budget,
                        top: 40,
                    },
                    Lane::Dupes,
                ),
                Ask::Thumbnails { files } => (0, Request::Thumbnails { files }, Lane::Thumbnails),
                Ask::Peek { path } => (0, Request::Preview { path }, Lane::Peek),
                Ask::PeekFacts { path } => (0, Request::Stat { path }, Lane::Peek),
                Ask::Rules => (0, Request::Rules {}, Lane::Rules),
                Ask::Status => (0, Request::Status {}, Lane::Status),
                Ask::Await { since } => (
                    0,
                    Request::Await {
                        since,
                        // Long enough that an idle window is nearly silent, short
                        // enough that a service restarted underneath is noticed.
                        timeout_ms: 30_000,
                    },
                    Lane::Await,
                ),
                Ask::Places => (0, Request::Places {}, Lane::Places),
                Ask::Count {
                    query_revision,
                    query,
                } => (
                    query_revision,
                    Request::Count {
                        query,
                        cap: 10_000_000,
                    },
                    Lane::Count,
                ),
                Ask::Stop => break,
            };
            // The page this request asked for, read back off the request so the
            // answer can say where it goes. See `Got::Search`.

            let (offset, limit) = match &request {
                Request::Search { page, .. } => (page.offset, page.limit),
                _ => (0, 0),
            };
            match c.call(request) {
                Ok(reply) => {
                    let reply = Box::new(reply);
                    sink(match facets {
                        Lane::Facets => Got::Facets {
                            query_revision: revision,
                            half,
                            reply,
                        },
                        Lane::Count => Got::Count {
                            query_revision: revision,
                            reply,
                        },
                        Lane::Search => Got::Search {
                            generation: revision,
                            offset,
                            limit,
                            reply,
                        },
                        Lane::Places => Got::Places(reply),
                        Lane::Usage => Got::Usage {
                            path: weighed.clone(),
                            reply,
                        },
                        Lane::Kinds => Got::Kinds {
                            path: weighed.clone(),
                            reply,
                        },
                        Lane::Biggest => Got::Biggest {
                            path: weighed.clone(),
                            reply,
                        },
                        Lane::Dupes => Got::Dupes(reply),
                        Lane::Peek => Got::Peek {
                            path: weighed.clone(),
                            reply,
                        },
                        Lane::Thumbnails => Got::Thumbnails(reply),
                        Lane::Rules => Got::Rules(reply),
                        Lane::Status => Got::Status(reply),
                        Lane::Await => Got::Awake(reply),
                        Lane::Explain => Got::Explain {
                            query_revision: revision,
                            reply,
                        },
                    });
                }
                Err(e) => {
                    // A transport error means the connection is gone; anything
                    // else is the service saying no, with the connection fine.
                    if matches!(
                        e,
                        scour_core::Error::Unreachable { .. } | scour_core::Error::Io { .. }
                    ) {
                        client = None;
                        was_down = true;
                        sink(Got::Down(e.to_string()));
                    } else {
                        let revision = match facets {
                            Lane::Search => ReplyRevision::Search(revision),
                            Lane::Facets | Lane::Count => ReplyRevision::Query(revision),
                            Lane::Places
                            | Lane::Rules
                            | Lane::Status
                            | Lane::Await
                            | Lane::Usage
                            | Lane::Kinds
                            | Lane::Biggest
                            | Lane::Dupes
                            | Lane::Thumbnails
                            | Lane::Peek
                            | Lane::Explain => ReplyRevision::Query(revision),
                        };
                        sink(Got::Refused {
                            revision,
                            why: e.to_string(),
                        });
                    }
                }
            }
        }
    });
}

/// Collapse the interactive backlog to its last intent. The atomic guard covers
/// a newer search that has not reached this channel yet.
fn newest_queued(mut ask: Ask, rx: &Receiver<Ask>) -> Ask {
    while let Ok(next) = rx.try_recv() {
        let stop = matches!(next, Ask::Stop);
        ask = next;
        if stop {
            break;
        }
    }
    ask
}

/// A folder as a query term, and nothing at all for the whole index. Quoted: a
/// path can hold a space, and an unquoted term would end at it.
fn under(path: &str) -> String {
    if path.is_empty() {
        String::new()
    } else {
        format!("under:\"{path}\"")
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn search(generation: u64, query_revision: u64) -> Ask {
        Ask::Search {
            generation,
            query_revision,
            query: String::new(),
            sort: "modified".into(),
            descending: true,
            offset: 0,
            limit: 20,
        }
    }

    #[test]
    fn only_the_newest_queued_search_reaches_the_service() {
        let freshness = Freshness::default();
        let old = search(1, 1);
        freshness.note(&old);
        assert!(freshness.accepts(&old));

        let newest = search(3, 3);
        freshness.note(&newest);
        assert!(!freshness.accepts(&old));
        assert!(freshness.accepts(&newest));
        assert!(!freshness.accepts(&search(2, 2)));
    }

    #[test]
    fn an_interactive_backlog_is_coalesced_to_one_message() {
        let (tx, rx) = channel();
        tx.send(search(2, 2)).expect("queue second search");
        tx.send(search(3, 3)).expect("queue third search");
        let newest = newest_queued(search(1, 1), &rx);
        assert!(matches!(newest, Ask::Search { generation: 3, .. }));
        assert!(rx.try_recv().is_err(), "the backlog was drained");
    }

    #[test]
    fn sorting_does_not_expire_background_work_for_the_same_query() {
        let freshness = Freshness::default();
        let sorted = search(7, 2);
        freshness.note(&sorted);

        assert!(freshness.accepts(&Ask::Facets {
            query_revision: 2,
            query: "rapor".into(),
            half: Half::Both,
        }));
        assert!(!freshness.accepts(&Ask::Count {
            query_revision: 1,
            query: "old".into(),
        }));
    }

    /// The reading is not thrown away for being newer than the search:
    /// `send_search` sends the `Explain` first, so the counter must move for it.
    #[test]
    fn a_reading_sent_before_its_search_is_still_fresh() {
        let freshness = Freshness::default();
        let reading = Ask::Explain {
            query_revision: 7,
            query: "hasan;genel".into(),
        };
        freshness.note(&reading);
        assert!(freshness.accepts(&reading));
        // And it goes stale the moment a later one is asked for.
        freshness.note(&Ask::Explain {
            query_revision: 8,
            query: "hasan;genell".into(),
        });
        assert!(!freshness.accepts(&reading));
    }

    #[test]
    fn rejected_requests_keep_their_freshness_domain() {
        assert_ne!(ReplyRevision::Search(7), ReplyRevision::Query(7));
    }
}
