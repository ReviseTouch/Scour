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

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc::{Receiver, Sender, channel},
};

use scour_ipc::Client;
use scour_proto::{Request, Response};

/// How many matches an interactive search counts before it stops.
///
/// A thousand is more than a person reads and enough for a meter to say
/// "at least this many". The exact figure follows once the query settles.
pub const TYPING_CAP: u32 = 1_000;

/// What the window asks for.
pub enum Ask {
    /// A search, tagged with the interaction that caused it.
    ///
    /// The tag is what makes a stale answer droppable. Without it a slow reply
    /// to `re` arrives after a fast one to `rapor` and the list goes backwards
    /// under the user's hands — which is the single most noticeable defect a
    /// search-as-you-type box can have.
    Search {
        generation: u64,
        query_revision: u64,
        query: String,
        sort: String,
        descending: bool,
        offset: u32,
        limit: u32,
    },
    Facets {
        query_revision: u64,
        query: String,
    },
    /// How many match, exactly, once the typing has stopped.
    /// Read the query back: the runs, and what each one is.
    ///
    /// Sent beside every search, because the colouring has to keep up with the
    /// typing. It goes on the fast lane *and* is allowed to coalesce — the
    /// newest query is the only one whose colours anybody will see.
    Explain {
        query_revision: u64,
        query: String,
    },
    /// What the walk skips, in three groups.
    Rules,
    /// Wait until the index is no longer at `since`, then say so.
    ///
    /// This is what makes the list live: the service answers when something it
    /// holds has changed, the window searches again, and asks to wait once
    /// more. Without it a window shows what was true when it was opened.
    Await {
        since: u64,
    },
    /// How big the index is, how many sources, how many watched.
    ///
    /// Asked once: these move slowly, and a meter that re-asked on every
    /// keystroke would be paying for a number nobody watches change.
    Status,
    /// Keep a choice: the language, the view shape, a column width.
    ///
    /// Fire and forget — the reply is `Accepted` and there is nothing to do
    /// with it. What matters is that it goes to the service rather than to a
    /// file this window owns, so the browser page opens in the same language.
    Remember {
        change: scour_settings::Change,
    },
    /// This desktop's own folders, asked once at start-up.
    ///
    /// **Of the service, not of `scour-places` directly**, even though the
    /// window runs on the same desktop and could read `user-dirs.dirs` itself.
    /// That is the rule the whole architecture rests on: a frontend asks, and
    /// the four of them get the same answer. The browser page cannot read that
    /// file at all, which is what made the rule visible in the first place.
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
        /// Where the page this answers begins.
        ///
        /// **Carried, not remembered.** A page fetch does not advance the
        /// generation, so two searches for the same query at different offsets
        /// are both current; a window that read "which offset did I last ask
        /// for" off its own state put the first answer at the second answer's
        /// place, and drew rows a hundred lines from where they belong.
        offset: u32,
        reply: Box<Response>,
    },
    Facets {
        query_revision: u64,
        reply: Box<Response>,
    },
    Count {
        query_revision: u64,
        reply: Box<Response>,
    },
    Places(Box<Response>),
    Rules(Box<Response>),
    Status(Box<Response>),
    Awake(Box<Response>),
    Explain {
        query_revision: u64,
        reply: Box<Response>,
    },
    /// The service answered, and the answer was no.
    ///
    /// Distinct from [`Got::Down`] because the two want opposite handling: a
    /// refused query — a term too short for the substring index, a field the
    /// engine cannot serve — leaves a perfectly good connection open, and
    /// throwing it away and reconnecting on every keystroke of `ra` would be
    /// a reconnect storm caused by nothing.
    Refused {
        revision: ReplyRevision,
        why: String,
    },
    /// The service could not be reached, with the reason as a sentence.
    Down(String),
    /// It could, after having been down.
    Up,
}

/// The freshness domain of a rejected request.
///
/// Ordering changes advance a search generation without changing the matching
/// set. Keeping that generation separate from the query revision prevents a
/// late facet/count error from being mistaken for the current search error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplyRevision {
    Search(u64),
    Query(u64),
}

pub struct Link {
    fast: Sender<Ask>,
    slow: Sender<Ask>,
    /// **A lane of its own, because it is the one request that is meant to
    /// block.** `Await` sits on the socket until the index moves or the
    /// timeout runs out; on either of the other two lanes it would hold every
    /// keystroke behind it for up to a minute.
    wait: Sender<Ask>,
    freshness: Freshness,
}

/// The newest work the UI can still use.
///
/// Dropping a stale reply protects correctness but saves no work. These two
/// counters are visible to both lanes, so a request waiting behind one slow
/// call can be discarded before it reaches the service. At most the call that
/// was already in flight when a key was pressed remains unavoidable.
#[derive(Clone, Default)]
struct Freshness {
    search: Arc<AtomicU64>,
    query: Arc<AtomicU64>,
}

impl Freshness {
    fn note(&self, ask: &Ask) {
        if let Ask::Search {
            generation,
            query_revision,
            ..
        } = ask
        {
            self.search.fetch_max(*generation, Ordering::Release);
            self.query.fetch_max(*query_revision, Ordering::Release);
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
            // Asked once and never superseded: there is no newer answer to
            // what this desktop's folders are called.
            Ask::Places | Ask::Rules | Ask::Status | Ask::Await { .. } | Ask::Remember { .. } => {
                true
            }
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
        // Publish the new generation before enqueueing it. A worker looking at
        // an older queued request can then skip it even if the router has not
        // forwarded the new message yet.
        self.freshness.note(&ask);
        let lane = match &ask {
            Ask::Await { .. } => &self.wait,
            // **Not the fast lane, and this cost an hour.** That lane
            // coalesces — it takes the newest queued request and drops the
            // rest, which is exactly right for keystrokes and exactly wrong
            // for anything asked once: `Places` went in and the search that
            // followed it a microsecond later swallowed it, every time, with
            // no error anywhere.
            // `Explain` joins them for the same reason `Places` did: the fast
            // lane keeps only the newest queued request, and a search sent a
            // microsecond later takes the colouring with it. Every one of
            // these is cheap enough that the slow lane is not slow for them.
            Ask::Facets { .. }
            | Ask::Count { .. }
            | Ask::Places
            | Ask::Rules
            | Ask::Status
            | Ask::Remember { .. }
            | Ask::Explain { .. } => &self.slow,
            _ => &self.fast,
        };
        // A closed channel means the lane died, and the window finds out from
        // the `Down` event rather than from a panic here.
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
            // Connecting can take longer than a key interval. Recheck at the
            // last point before the service call so that work superseded while
            // reconnecting is not paid for either.
            if !freshness.accepts(&ask) {
                continue;
            }
            let Some(c) = client.as_mut() else { continue };
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
                Ask::Facets {
                    query_revision,
                    query,
                } => (
                    query_revision,
                    // **Both in one request.** The rail's kinds and the
                    // ribbon's ages are the same walk over the same matching
                    // set; asking twice would pay for it twice, and the two
                    // answers could then disagree about a query that changed
                    // between them.
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
                Ask::Rules => (0, Request::Rules {}, Lane::Rules),
                Ask::Status => (0, Request::Status {}, Lane::Status),
                Ask::Await { since } => (
                    0,
                    Request::Await {
                        since,
                        // Long enough that an idle window is nearly silent —
                        // one request a minute — and short enough that a
                        // service restarted underneath is noticed.
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
            // The page this request asked for, read back off the request
            // itself so the answer can say where it goes. See `Got::Search`.
            let offset = match &request {
                Request::Search { page, .. } => page.offset,
                _ => 0,
            };
            match c.call(request) {
                Ok(reply) => {
                    let reply = Box::new(reply);
                    sink(match facets {
                        Lane::Facets => Got::Facets {
                            query_revision: revision,
                            reply,
                        },
                        Lane::Count => Got::Count {
                            query_revision: revision,
                            reply,
                        },
                        Lane::Search => Got::Search {
                            generation: revision,
                            offset,
                            reply,
                        },
                        Lane::Places => Got::Places(reply),
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
                        let revision = match facets {
                            Lane::Search => ReplyRevision::Search(revision),
                            Lane::Facets | Lane::Count => ReplyRevision::Query(revision),
                            Lane::Places
                            | Lane::Rules
                            | Lane::Status
                            | Lane::Await
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

/// Collapse the interactive backlog to its last intent.
///
/// The atomic guard catches a newer search that has not reached this channel
/// yet. Draining here handles the ordinary case in one pass and releases the
/// superseded query strings immediately.
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
        }));
        assert!(!freshness.accepts(&Ask::Count {
            query_revision: 1,
            query: "old".into(),
        }));
    }

    #[test]
    fn rejected_requests_keep_their_freshness_domain() {
        assert_ne!(ReplyRevision::Search(7), ReplyRevision::Query(7));
    }
}
