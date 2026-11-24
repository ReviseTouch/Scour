//! The disks: what the page lists in its panel and asks about in its bar.

use std::net::TcpStream;

use scour_proto::{Request, Response};

use crate::call;
use crate::http;
use scour_client::Pool;

/// Every disk with its state, as the service words them on the wire.
pub fn api_volumes(stream: &mut TcpStream, client: &Pool) {
    match call(client, Request::Volumes {}) {
        Ok(Response::Volumes { disks, folders }) => {
            match (serde_json::to_value(&disks), serde_json::to_value(&folders)) {
                (Ok(d), Ok(f)) => {
                    http::json(stream, &serde_json::json!({ "disks": d, "folders": f }))
                }
                (Err(e), _) | (_, Err(e)) => {
                    http::fail(stream, "500 Internal Server Error", &e.to_string())
                }
            }
        }
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// An answer about one disk: `do` is `index`, `leave` or `later`.
/// Walk a disk's or a folder's root again, now: the refresh button beside it.
pub fn api_rescan(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let Some(path) = req.param("path").filter(|p| !p.is_empty()) else {
        return http::fail(stream, "400 Bad Request", "path is required");
    };
    let request = Request::Rescan {
        path: Some(path.to_owned()),
    };
    match call(client, request) {
        Ok(Response::Accepted) => http::json(stream, &serde_json::json!({ "ok": true })),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// A `content:` query's reading goes on, or with `from=start` begins again.
pub fn api_reread(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let request = Request::Reread {
        from_start: req.param("from") == Some("start"),
    };
    match call(client, request) {
        Ok(Response::Accepted) => http::json(stream, &serde_json::json!({ "ok": true })),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

pub fn api_volume(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let volume = req.param("id").unwrap_or_default().to_owned();
    let request = match req.param("do") {
        Some("index") => Request::Choose {
            volume,
            index: true,
        },
        Some("leave") => Request::Choose {
            volume,
            index: false,
        },
        Some("later") => Request::Postpone { volume },
        _ => return http::fail(stream, "400 Bad Request", "do is index, leave or later"),
    };
    match call(client, request) {
        Ok(Response::Accepted) => http::json(stream, &serde_json::json!({ "ok": true })),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// Where the index is and what it holds: the Index section of the settings.
pub fn api_index(stream: &mut TcpStream, client: &Pool) {
    match call(client, Request::Stats {}) {
        Ok(Response::Stats(s)) => match serde_json::to_value(&s) {
            Ok(v) => http::json(stream, &v),
            Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
        },
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// The Index section's buttons: `do` is `reorganize`, `reset` or `open` —
/// the last opens the folder, so it is behind `--no-launch` like any opening.
pub fn api_index_do(stream: &mut TcpStream, client: &Pool, req: &http::Req, launch: bool) {
    let request = match req.param("do") {
        Some("reorganize") => Request::Maintain {
            level: scour_core::Maintenance::Rebuild,
        },
        Some("reset") => Request::Reset {},
        Some("open") if !launch => {
            return http::fail(stream, "403 Forbidden", "opening is off (--no-launch)");
        }
        // The folder the service names, never one the page names.
        Some("open") => {
            return match call(client, Request::Stats {}) {
                Ok(Response::Stats(s)) if !s.dir.is_empty() => {
                    match scour_openers::open(std::ffi::OsStr::new(&s.dir)) {
                        Ok(()) => http::json(stream, &serde_json::json!({ "ok": true })),
                        Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
                    }
                }
                Ok(_) => http::fail(stream, "404 Not Found", "the index has no folder"),
                Err(e) => http::fail(stream, "502 Bad Gateway", &e),
            };
        }
        _ => return http::fail(stream, "400 Bad Request", "do is reorganize, reset or open"),
    };
    match call(client, request) {
        Ok(Response::Accepted | Response::Maintained(_)) => {
            http::json(stream, &serde_json::json!({ "ok": true }))
        }
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}
