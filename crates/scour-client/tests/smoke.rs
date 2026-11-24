//! A real socket, a pool in front of it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use scour_client::Pool;
use scour_ipc::{Server, is_running};
use scour_proto::{Outcome, Request, Response};

/// A service that refuses `shutdown`, answers the rest, and counts what it is asked.
fn serve(dir: &tempfile::TempDir) -> (String, Arc<AtomicU64>, Arc<AtomicBool>) {
    #[cfg(windows)]
    let addr = format!(r"\\.\pipe\scour-client-test-{}", std::process::id());
    #[cfg(not(windows))]
    let addr = dir.path().join("s.sock").to_string_lossy().into_owned();
    let (calls, stop) = (
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicBool::new(false)),
    );
    let server = Server::bind(&addr).expect("bind");
    let (c, s) = (Arc::clone(&calls), Arc::clone(&stop));
    std::thread::spawn(move || {
        server.serve(
            move |req, _emit| {
                c.fetch_add(1, Ordering::Relaxed);
                match req {
                    Request::Shutdown {} => {
                        Outcome::Error(scour_core::Error::unsupported("shutdown"))
                    }
                    _ => Outcome::Ok(Response::Accepted),
                }
            },
            s,
        );
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !is_running(&addr) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    (addr, calls, stop)
}

/// The page once sent a refused request a second time on a new connection;
/// for a request that writes, that is doing it twice.
#[test]
fn a_refused_request_is_sent_once_and_the_connection_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, calls, stop) = serve(&dir);
    let pool = Pool::new(&addr, 2);
    assert!(matches!(
        pool.call(Request::Sources {}),
        Ok(Response::Accepted)
    ));
    assert_eq!(pool.held(), 1);
    let before = calls.load(Ordering::Relaxed);
    assert!(pool.call(Request::Shutdown {}).is_err());
    assert_eq!(calls.load(Ordering::Relaxed), before + 1, "asked twice");
    assert_eq!(
        pool.held(),
        1,
        "a refusal is an answer; the connection is good"
    );
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn nothing_listening_is_an_error_and_nothing_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::new(dir.path().join("none.sock").to_string_lossy(), 2);
    let err = pool.call(Request::Sources {}).unwrap_err();
    assert!(err.is_transient(), "{err:?}");
    assert_eq!(pool.held(), 0);
}
