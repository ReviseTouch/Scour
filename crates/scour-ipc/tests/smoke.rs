//! A real socket, a real client.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use scour_ipc::{Client, Server, is_running};
use scour_proto::{Outcome, Request, Response};

/// A socket path inside a temporary directory, so tests never collide.
fn addr(dir: &tempfile::TempDir) -> String {
    #[cfg(windows)]
    {
        let _ = dir;
        format!(r"\\.\pipe\scour-test-{}", std::process::id())
    }
    #[cfg(not(windows))]
    dir.path().join("s.sock").to_string_lossy().into_owned()
}

struct Running {
    addr: String,
    stop: Arc<AtomicBool>,
    calls: Arc<AtomicU64>,
    /// Pieces the stand-in export managed to write before the reader went.
    ///
    /// The only way a test can see the thing that matters about a cancelled
    /// download: that the *service* stopped producing, rather than running to
    /// the end and writing into a socket nobody was reading.
    wrote: Arc<AtomicU64>,
    _dir: tempfile::TempDir,
}

impl Running {
    fn start() -> Running {
        let dir = tempfile::tempdir().expect("temp");
        let addr = addr(&dir);
        let stop = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicU64::new(0));
        let wrote = Arc::new(AtomicU64::new(0));
        let server = Server::bind(&addr).expect("bind");
        {
            let (stop, calls, wrote) = (Arc::clone(&stop), Arc::clone(&calls), Arc::clone(&wrote));
            std::thread::spawn(move || {
                server.serve(
                    move |req, emit| {
                        calls.fetch_add(1, Ordering::Relaxed);
                        match req {
                            // A stand-in export. The first column names how
                            // many pieces to write and the query is what goes
                            // in each, so a test can ask for far more frames
                            // than any buffer holds without needing an index.
                            Request::Export { query, columns } => {
                                let want: u64 =
                                    columns.first().and_then(|c| c.parse().ok()).unwrap_or(3);
                                let mut sent = 0;
                                for _ in 0..want {
                                    if emit
                                        .piece(Response::ExportChunk { csv: query.clone() })
                                        .is_err()
                                    {
                                        // The reader is gone. Stopping here is
                                        // the whole of what a cancelled
                                        // download costs the service.
                                        break;
                                    }
                                    sent += 1;
                                    wrote.fetch_add(1, Ordering::Relaxed);
                                }
                                Outcome::Ok(Response::ExportDone { rows: sent })
                            }
                            Request::Syntax {} => Outcome::Ok(Response::Text {
                                text: "hello".into(),
                            }),
                            Request::Explain { query, .. } => Outcome::Ok(Response::Explain {
                                description: query,
                                needs_content: false,
                                spans: Vec::new(),
                                completions: Vec::new(),
                            }),
                            Request::Shutdown {} => {
                                Outcome::Error(scour_core::Error::unsupported("shutdown"))
                            }
                            _ => Outcome::Ok(Response::Accepted),
                        }
                    },
                    stop,
                );
            });
        }
        // Binding happened before the thread started, so this is only waiting
        // for the accept loop.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !is_running(&addr) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        Running {
            addr,
            stop,
            calls,
            wrote,
            _dir: dir,
        }
    }

    fn export(want: u64) -> Request {
        Request::Export {
            query: "row\r\n".into(),
            columns: vec![want.to_string()],
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Nudge the accept loop so it notices.
        let _ = Client::connect(&self.addr);
    }
}

#[test]
fn a_request_gets_its_own_reply_back() {
    let s = Running::start();
    let mut c = Client::connect(&s.addr).expect("connect");
    assert_eq!(
        c.call(Request::Syntax {}).expect("call"),
        Response::Text {
            text: "hello".into()
        }
    );
}

#[test]
fn one_connection_carries_many_requests_in_order() {
    // The reason a client holds its connection open: a search box sends one
    // request per keystroke, and a fresh connection each time would put that
    // cost inside the latency this project exists to remove.
    let s = Running::start();
    let mut c = Client::connect(&s.addr).expect("connect");
    for i in 0..50 {
        let want = format!("q{i}");
        let got = c
            .call(Request::Explain {
                query: want.clone(),
                cursor: None,
            })
            .expect("call");
        assert_eq!(
            got,
            Response::Explain {
                description: want,
                needs_content: false,
                spans: Vec::new(),
                completions: Vec::new(),
            }
        );
    }
    assert_eq!(s.calls.load(Ordering::Relaxed), 50);
}

#[test]
fn a_typed_failure_survives_the_trip() {
    let s = Running::start();
    let mut c = Client::connect(&s.addr).expect("connect");
    let err = c.call(Request::Shutdown {}).unwrap_err();
    assert_eq!(err.code(), "unsupported");
}

#[test]
fn several_clients_are_served_at_once() {
    let s = Running::start();
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let addr = s.addr.clone();
            std::thread::spawn(move || {
                let mut c = Client::connect(&addr).expect("connect");
                for j in 0..10 {
                    let q = format!("{i}-{j}");
                    let got = c
                        .call(Request::Explain {
                            query: q.clone(),
                            cursor: None,
                        })
                        .expect("call");
                    assert_eq!(
                        got,
                        Response::Explain {
                            description: q,
                            needs_content: false,
                            spans: Vec::new(),
                            completions: Vec::new(),
                        }
                    );
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("client thread");
    }
    assert_eq!(s.calls.load(Ordering::Relaxed), 80);
}

#[test]
fn connecting_to_nothing_says_so_instead_of_hanging() {
    let dir = tempfile::tempdir().expect("temp");
    let nowhere = addr(&dir) + "-absent";
    assert!(!is_running(&nowhere));
    let err = Client::connect(&nowhere).unwrap_err();
    assert_eq!(err.code(), "unreachable");
    assert!(
        err.is_transient(),
        "the service may simply not be started yet"
    );
}

#[test]
#[cfg(not(windows))]
fn a_socket_left_by_a_crash_is_cleared_rather_than_fatal() {
    // Otherwise the service would never come back after a crash without
    // someone deleting a file by hand.
    let dir = tempfile::tempdir().expect("temp");
    let path = dir.path().join("stale.sock");
    std::fs::write(&path, b"not a socket").expect("write");
    let addr = path.to_string_lossy().into_owned();
    assert!(!is_running(&addr));
    let server = Server::bind(&addr).expect("a dead socket must not block a restart");
    assert_eq!(server.addr(), addr);
}

/// A peer that connects and sends bytes without a newline used to grow the
/// service's memory for as long as it cared to — measured at 19 MB to 282 MB
/// from one 256 MB write. Now it is answered and hung up on.
#[test]
fn a_request_without_an_end_is_refused_rather_than_buffered() {
    use std::io::{BufRead, BufReader, Write};

    let s = Running::start();
    use interprocess::local_socket::Stream;
    use interprocess::local_socket::traits::Stream as _;
    let mut conn = Stream::connect(raw_name(&s.addr)).expect("connect");
    // Two megabytes with no newline in them, against a one megabyte ceiling.
    let flood = vec![b'x'; 2 * 1024 * 1024];
    // The write may fail partway once the far end hangs up, which is itself
    // the behaviour being asserted; either outcome is a pass at this step.
    let _ = conn.write_all(&flood);
    let _ = conn.flush();

    let mut line = String::new();
    let mut reader = BufReader::new(conn);
    let n = reader.read_line(&mut line).unwrap_or(0);
    assert!(n > 0, "the service should answer rather than go silent");
    assert!(
        line.contains("may not exceed"),
        "expected a refusal, got {line}"
    );

    // And the service is still serving everyone else.
    let mut ok = Client::connect(&s.addr).expect("reconnect");
    let reply = ok
        .call(Request::Explain {
            query: "still here".into(),
            cursor: None,
        })
        .expect("call");
    assert!(matches!(reply, Response::Explain { .. }));
}

#[test]
fn an_answer_may_arrive_in_pieces() {
    let s = Running::start();
    let mut c = Client::connect(&s.addr).expect("connect");
    let mut got = String::new();
    let done = c
        .stream(Running::export(2_000), |piece| {
            if let Response::ExportChunk { csv } = piece {
                got.push_str(&csv);
            }
            true
        })
        .expect("stream");
    assert_eq!(done, Response::ExportDone { rows: 2_000 });
    assert_eq!(got.matches("row").count(), 2_000);
    assert_eq!(s.wrote.load(Ordering::Relaxed), 2_000);
}

/// The connection is still usable afterwards, which is what makes the pieces a
/// message rather than a mode.
#[test]
fn a_finished_stream_leaves_the_connection_where_it_found_it() {
    let s = Running::start();
    let mut c = Client::connect(&s.addr).expect("connect");
    c.stream(Running::export(50), |_| true).expect("stream");
    assert_eq!(
        c.call(Request::Syntax {}).expect("call after a stream"),
        Response::Text {
            text: "hello".into()
        }
    );
}

/// A request answered in pieces is refused by `call` rather than half-read.
///
/// The failure this prevents is the one that does not look like a failure:
/// reading the first piece as the whole answer leaves the rest in the buffer,
/// and the *next* request on that connection is answered by the leftovers. A
/// search box would show the wrong files and nothing anywhere would error.
#[test]
fn a_streamed_answer_is_refused_by_the_call_that_cannot_read_it() {
    let s = Running::start();
    let mut c = Client::connect(&s.addr).expect("connect");
    let err = c.call(Running::export(5)).unwrap_err();
    assert_eq!(err.code(), "config");

    // And the connection was never written to, so it still works.
    assert!(matches!(
        c.call(Request::Syntax {}).expect("call"),
        Response::Text { .. }
    ));
    assert_eq!(
        s.calls.load(Ordering::Relaxed),
        1,
        "the refused request never reached the service"
    );
}

/// A reader that goes away mid-export — an ordinary cancelled download.
///
/// **What must not happen is the service carrying on.** It is walking a
/// matching set that can be two million rows, and a walk that runs to the end
/// writing into a socket nobody is reading is a minute of a core and a
/// connection thread spent on an answer that has no reader. So the assertion
/// is not that the client survived — it is that the *service* stopped, which
/// `wrote` is what sees.
///
/// Ten thousand pieces against a client that takes five, so that the stop
/// happens far from either end. The socket buffer absorbs some number of
/// pieces after the reader has gone, which is why the bound below is generous:
/// what is being tested is that it is bounded at all.
#[test]
fn a_reader_that_goes_away_stops_the_service_producing() {
    let s = Running::start();
    let mut c = Client::connect(&s.addr).expect("connect");
    let mut seen = 0;
    let err = c
        .stream(Running::export(10_000), |_| {
            seen += 1;
            seen < 5
        })
        .unwrap_err();
    assert_eq!(err.code(), "unreachable");
    assert_eq!(seen, 5);

    // Drop the connection, which is what a cancelled download is: there is no
    // cancel message, and inventing one would mean a client that dies without
    // sending it leaves the service producing for ever.
    drop(c);

    // The service notices on its next write. Give it a moment — this is the
    // one thing here that is not synchronous.
    let deadline = Instant::now() + Duration::from_secs(5);
    while s.wrote.load(Ordering::Relaxed) >= 10_000 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let wrote = s.wrote.load(Ordering::Relaxed);
    assert!(
        wrote < 10_000,
        "the service wrote all {wrote} pieces to a socket nobody was reading"
    );

    // Nothing is left holding the service: a new client is served normally.
    let mut fresh = Client::connect(&s.addr).expect("reconnect");
    assert!(matches!(
        fresh.call(Request::Syntax {}).expect("call"),
        Response::Text { .. }
    ));
}

/// A connection abandoned mid-stream is never reused.
///
/// The frames nobody read are still in flight, so the next request on it would
/// be answered by the tail of the last one. Refusing to send costs a
/// `connect`, which is what a cancelled download should cost.
#[test]
fn an_abandoned_stream_poisons_only_its_own_connection() {
    let s = Running::start();
    let mut c = Client::connect(&s.addr).expect("connect");
    let _ = c.stream(Running::export(10_000), |_| false);
    let err = c.call(Request::Syntax {}).unwrap_err();
    assert_eq!(err.code(), "unreachable");

    let mut fresh = Client::connect(&s.addr).expect("reconnect");
    assert!(matches!(
        fresh.call(Request::Syntax {}).expect("call"),
        Response::Text { .. }
    ));
}

/// An export that matches nothing is still an answer, not a silence.
#[test]
fn a_stream_with_no_pieces_still_ends_properly() {
    let s = Running::start();
    let mut c = Client::connect(&s.addr).expect("connect");
    let mut pieces = 0;
    let done = c
        .stream(Running::export(0), |_| {
            pieces += 1;
            true
        })
        .expect("stream");
    assert_eq!(pieces, 0);
    assert_eq!(done, Response::ExportDone { rows: 0 });
}

/// The address as `interprocess` wants it — the same rule `Server::name` uses,
/// repeated here because that function is private and this test needs a raw
/// connection rather than a `Client`.
fn raw_name(addr: &str) -> interprocess::local_socket::Name<'_> {
    use interprocess::local_socket::{GenericFilePath, GenericNamespaced, ToFsName, ToNsName};
    if cfg!(windows) {
        addr.to_ns_name::<GenericNamespaced>().expect("name")
    } else {
        addr.to_fs_name::<GenericFilePath>().expect("name")
    }
}
