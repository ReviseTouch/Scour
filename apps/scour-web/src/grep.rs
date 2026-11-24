//! Routes for reading matching files (`/api/grep`), listing indexed folders
//! (`/api/tree`), and asking the desktop's folder picker (`/api/pick-folder`).

use std::net::TcpStream;
use std::path::Path;

use scour_core::SortKey;
use scour_proto::{GrepMode, Request, Response};

use crate::{call, http};
use scour_client::Pool;

pub fn api_grep(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let pattern = req.param("pattern").unwrap_or_default().to_owned();
    if pattern.is_empty() {
        http::json(
            stream,
            &serde_json::json!({
                "files": [],
                "candidates": 0,
                "capped": false,
                "scanned": 0,
                "read": 0,
                "binary": 0,
                "too_big": 0,
                "unreadable": 0,
                "header_only": 0,
                "no_reader": 0,
                "locked": 0,
                "next": serde_json::Value::Null,
                "took_us": 0,
            }),
        );
        return;
    }
    let mode = match req.param("mode").unwrap_or("text") {
        "regex" => GrepMode::Regex,
        "hex" => GrepMode::Hex,
        _ => GrepMode::Text,
    };
    let sort = req
        .param("sort")
        .and_then(SortKey::named)
        .unwrap_or_default();
    let descending = req.param("desc") != Some("0");
    let request = Request::Grep {
        query: req.param("q").unwrap_or_default().to_owned(),
        pattern,
        mode,
        exact_case: req.param("exact") == Some("1"),
        binary: req.param("binary") == Some("1"),
        sort,
        descending,
        offset: req
            .param("offset")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        top: req
            .param("top")
            .and_then(|s| s.parse().ok())
            .unwrap_or(50)
            .min(200),
        hits: req
            .param("hits")
            .and_then(|s| s.parse().ok())
            .unwrap_or(5)
            .min(20),
        read_budget: req
            .param("budget_mb")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(256)
            .saturating_mul(1024 * 1024),
        max_size: 64 << 20,
        timeout_ms: req
            .param("timeout_ms")
            .and_then(|s| s.parse().ok())
            .unwrap_or(5_000)
            .min(15_000),
    };
    match call(client, request) {
        Ok(Response::Grep(r)) => {
            let files: Vec<serde_json::Value> = r
                .files
                .iter()
                .map(|f| {
                    let hits: Vec<serde_json::Value> = f
                        .hits
                        .iter()
                        .map(|h| {
                            let (at, len) = (h.at.0 as usize, h.at.1 as usize);
                            let (pre, hit, post) = match (
                                h.text.get(..at),
                                h.text.get(at..at + len),
                                h.text.get(at + len..),
                            ) {
                                (Some(a), Some(b), Some(c)) => (a, b, c),
                                _ => (h.text.as_str(), "", ""),
                            };
                            serde_json::json!({
                                "line": h.line,
                                "offset": h.offset,
                                "pre": pre,
                                "hit": hit,
                                "post": post,
                            })
                        })
                        .collect();
                    serde_json::json!({
                        "path": f.path,
                        "size": f.meta.size,
                        "mtime": f.meta.mtime,
                        "ctime": f.meta.ctime,
                        "atime": f.meta.atime,
                        "disk": f.meta.disk,
                        "mode": f.meta.mode,
                        "kind": f.kind.token(),
                        "hits": hits,
                        "more": f.more,
                    })
                })
                .collect();
            http::json(
                stream,
                &serde_json::json!({
                    "files": files,
                    "candidates": r.candidates,
                    "capped": r.capped,
                    "scanned": r.scanned,
                    "read": r.read,
                    "binary": r.binary,
                    "too_big": r.too_big,
                    "unreadable": r.unreadable,
                    "header_only": r.header_only,
                    "no_reader": r.no_reader,
                    "locked": r.locked,
                    "next": r.next,
                    "took_us": r.took_us,
                }),
            );
        }
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

pub fn api_tree(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let path = req.param("path").unwrap_or("/").to_owned();
    let limit = req
        .param("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(120)
        .min(500);
    match call(
        client,
        Request::Tree {
            path,
            depth: 1,
            limit,
        },
    ) {
        Ok(Response::Tree { root, took_us }) => {
            let dirs: Vec<serde_json::Value> = root
                .nodes
                .iter()
                .filter(|n| n.is_dir)
                .map(|n| {
                    serde_json::json!({
                        "name": n.name,
                        "path": n.path,
                        "children": n.children,
                        "mtime": n.mtime,
                    })
                })
                .collect();
            http::json(
                stream,
                &serde_json::json!({
                    "path": root.path,
                    "dirs": dirs,
                    "truncated": root.truncated,
                    "took_us": took_us,
                }),
            );
        }
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

pub fn api_pick_folder(stream: &mut TcpStream, req: &http::Req) {
    let start = req.param("start").filter(|s| !s.is_empty()).map(Path::new);
    let picked = scour_openers::pick_folder(start);
    http::json(stream, &serde_json::json!({ "path": picked }));
}
