//! The disks: what the page lists in its panel and asks about in its bar.

use std::net::TcpStream;
use std::sync::Mutex;

use scour_proto::{Request, Response};

use crate::http;
use crate::{Link, call};

/// Every disk with its state, as the service words them on the wire.
pub fn api_volumes(stream: &mut TcpStream, client: &Mutex<Link>) {
    match call(client, Request::Volumes {}) {
        Ok(Response::Volumes { disks }) => match serde_json::to_value(&disks) {
            Ok(v) => http::json(stream, &serde_json::json!({ "disks": v })),
            Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
        },
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// An answer about one disk: `do` is `index`, `leave` or `later`.
pub fn api_volume(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
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
