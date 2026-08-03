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
    _dir: tempfile::TempDir,
}

impl Running {
    fn start() -> Running {
        let dir = tempfile::tempdir().expect("temp");
        let addr = addr(&dir);
        let stop = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicU64::new(0));
        let server = Server::bind(&addr).expect("bind");
        {
            let (stop, calls) = (Arc::clone(&stop), Arc::clone(&calls));
            std::thread::spawn(move || {
                server.serve(
                    move |req| {
                        calls.fetch_add(1, Ordering::Relaxed);
                        match req {
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
            _dir: dir,
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
