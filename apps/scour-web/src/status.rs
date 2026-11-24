//! What the service is doing, and the long poll that says when it moves.

use super::*;
use scour_client::Pool;
use scour_ipc::Client;
use scour_proto::{Request, Response};
use std::net::TcpStream;

pub(super) fn api_status(stream: &mut TcpStream, client: &Pool) {
    match call(client, Request::Status {}) {
        Ok(Response::Status(s)) => http::json(stream, &status_json(&s)),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// The same answer either route produces, waited for or asked outright.
fn status_json(s: &scour_core::Status) -> serde_json::Value {
    serde_json::json!({
        "entries": s.entries,
        "scanning": s.scanning,
        "unwritten": s.unwritten,
        "watching": s.watching,
        "sources": s.sources,
        "pending": s.pending,
        "index_bytes": s.index_bytes,
        "revision": s.revision,
        // Every query reads the unsorted tail: a week of ordinary use makes
        // ordering by path 1.9 ms against 21.5, and one rebuild puts it back.
        "unsorted": s.unsorted,
        "rebuild_advised": s.rebuild_advised,
        // Disks waiting for a yes or a no: the page asks for them when this moves.
        "asking": s.asking,
    })
}

/// Hold the request open until the index changes. Its own connection: half a
/// minute on the shared one is half a minute of keystrokes. A timeout is not
/// an error, and a status comes back either way.
pub(super) fn api_wait(stream: &mut TcpStream, addr: &str, req: &http::Req) {
    let since: u64 = req.param("rev").and_then(|s| s.parse().ok()).unwrap_or(0);
    let timeout_ms: u32 = req
        .param("ms")
        .and_then(|s| s.parse().ok())
        .unwrap_or(25_000)
        .min(60_000);
    let mut client = match Client::connect(addr) {
        Ok(c) => c,
        Err(e) => {
            http::fail(stream, "502 Bad Gateway", &e.to_string());
            return;
        }
    };
    match client.call(Request::Await { since, timeout_ms }) {
        Ok(Response::Status(s)) => http::json(stream, &status_json(&s)),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e.to_string()),
    }
}
