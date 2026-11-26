//! The panel the status bar opens: what the service holds on this machine,
//! and the presses that empty, put in order or restart. Every path opened is
//! one the service names, never one the page sends.

use std::net::TcpStream;

use scour_client::Pool;
use scour_proto::{HeldKind, Request, Response};

use crate::call;
use crate::http;

/// `GET /api/resources`: the service's answer as it is on the wire.
pub fn api_resources(stream: &mut TcpStream, client: &Pool) {
    match call(client, Request::Resources {}) {
        Ok(Response::Resources(r)) => match serde_json::to_value(&r) {
            Ok(v) => http::json(stream, &v),
            Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
        },
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// The wire's token for a thing kept on disk.
fn kind(token: Option<&str>) -> Option<HeldKind> {
    serde_json::from_value(serde_json::Value::String(token?.to_owned())).ok()
}

/// `POST /api/clear?what=index|texts|previews|page`.
pub fn api_clear(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let Some(what) = kind(req.param("what")) else {
        return http::fail(
            stream,
            "400 Bad Request",
            "what is index, texts, previews or page",
        );
    };
    accepted(stream, call(client, Request::Clear { what }));
}

/// `POST /api/restart`.
pub fn api_restart(stream: &mut TcpStream, client: &Pool) {
    accepted(stream, call(client, Request::Restart {}));
}

/// `POST /api/held-open?what=…`: the folder the service names for it, in the
/// file manager. Behind `--no-launch`, as every opening is.
pub fn api_held_open(stream: &mut TcpStream, client: &Pool, req: &http::Req, launch: bool) {
    if !launch {
        return http::fail(stream, "403 Forbidden", "opening is off (--no-launch)");
    }
    let Some(what) = kind(req.param("what")) else {
        return http::fail(stream, "400 Bad Request", "what names a kind of file");
    };
    let path = match call(client, Request::Resources {}) {
        Ok(Response::Resources(r)) => r.files.into_iter().find(|h| h.kind == what).map(|h| h.path),
        Ok(_) => return http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => return http::fail(stream, "502 Bad Gateway", &e),
    };
    match path.filter(|p| !p.is_empty()) {
        Some(p) => match scour_openers::open(std::ffi::OsStr::new(&p)) {
            Ok(()) => http::json(stream, &serde_json::json!({ "ok": true })),
            Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
        },
        None => http::fail(
            stream,
            "404 Not Found",
            "the service keeps nothing of that kind",
        ),
    }
}

fn accepted(stream: &mut TcpStream, got: Result<Response, String>) {
    match got {
        Ok(Response::Accepted) => http::json(stream, &serde_json::json!({ "ok": true })),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        // Refused for a reason a person can act on: the page's browser is open.
        Err(e) => http::fail(stream, "409 Conflict", &e),
    }
}
