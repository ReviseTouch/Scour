//! Pictures for rows: a kind's icon, the desktop's own icon, a thumbnail.

use super::*;
use scour_client::Pool;
use scour_ipc::Client;
use scour_proto::{Request, Response};
use std::net::TcpStream;
use std::sync::Mutex;

/// The thumbnail somebody has already made for a file. Cacheable, unlike every
/// other route: a file in a cache, not an answer about a watched filesystem.
pub(super) fn api_icon(stream: &mut TcpStream, client: &Pool, req: &http::Req, pictures: bool) {
    let Some(path) = req.param("p").filter(|p| !p.is_empty()) else {
        http::fail(stream, "404 Not Found", "no thumbnail");
        return;
    };
    // Freedesktop cache: `path` is only hashed, never opened.
    if let Some(p) = icons::thumbnail(path) {
        http::cached(stream, p.kind, &p.bytes);
        return;
    }
    // Windows Explorer / macOS Quick Look: fence through `stat` before touching disk.
    if pictures
        && scour_thumbs::native::available()
        && let Ok(Response::Stat(entry)) = call(
            client,
            Request::Stat {
                path: path.to_owned(),
            },
        )
        && !entry.is_dir
        && let Some(p) = icons::native_thumbnail(&entry.path)
    {
        http::cached(stream, p.kind, &p.bytes);
        return;
    }
    http::fail(stream, "404 Not Found", "no thumbnail");
}

/// Native desktop file or folder icon keyed by type (`ext:docx`, `folder`,
/// `place:documents`), never touching indexed file paths.
pub(super) fn api_sysicon(stream: &mut TcpStream, req: &http::Req) {
    let key = if let Some(leaf) = req.param("home_dir").filter(|l| !l.is_empty()) {
        scour_thumbs::sysicon::home_child_key(leaf)
    } else if let Some(k) = req.param("k").filter(|k| !k.is_empty()) {
        k.to_owned()
    } else {
        http::fail(stream, "404 Not Found", "no icon");
        return;
    };
    let px = req
        .param("s")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(64);
    match scour_thumbs::sysicon::web_icon(&key, px) {
        Some(ic) => http::cached(stream, ic.content_type, &ic.bytes),
        None => http::fail(stream, "404 Not Found", "no icon"),
    }
}

/// Ask the desktop for the pictures it has not made yet. Carries no bytes: it
/// says which paths have one now, and the page fetches those from `/api/icon`.
pub(super) fn api_thumb(stream: &mut TcpStream, addr: &str, req: &http::Req) {
    // In the body, not the query: a request line past 16 KiB is cut rather
    // than refused.
    let files = paths_in(req);
    if files.is_empty() {
        http::json(stream, &serde_json::json!({ "ready": [], "ran": 0 }));
        return;
    }
    // Its own connection: a batch is seconds of somebody else's video decoding.
    let mut client = match Client::connect(addr) {
        Ok(c) => c,
        Err(e) => {
            http::fail(stream, "502 Bad Gateway", &e.to_string());
            return;
        }
    };
    if scour_thumbs::native::available() {
        let fenced: Vec<String> = files
            .into_iter()
            .take(scour_thumbs::Maker::BATCH)
            .filter_map(|path| match client.call(Request::Stat { path }) {
                Ok(Response::Stat(entry)) if !entry.is_dir => Some(entry.path),
                _ => None,
            })
            .collect();
        let ready = Mutex::new(Vec::new());
        for chunk in fenced.chunks(scour_thumbs::Maker::AT_ONCE) {
            std::thread::scope(|scope| {
                for p in chunk {
                    scope.spawn(|| {
                        let native = scour_core::path::to_path(p);
                        if scour_thumbs::native::picture(&native, 256).is_some() {
                            ready
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push(p.clone());
                        }
                    });
                }
            });
        }
        let ready = ready.into_inner().unwrap_or_else(|e| e.into_inner());
        http::json(
            stream,
            &serde_json::json!({ "ready": ready, "ran": fenced.len() }),
        );
        return;
    }
    match client.call(Request::Thumbnails { files, size: None }) {
        Ok(Response::Thumbnails(made)) => http::json(
            stream,
            // `ran` is how many processes a screenful of unseen files started.
            &serde_json::json!({ "ready": made.ready, "ran": made.ran }),
        ),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e.to_string()),
    }
}
