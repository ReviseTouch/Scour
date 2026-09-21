//! The duplicates workflow, on top of `Request::Duplicates`.
//!
//! A group says which files share a size. Choosing which copy to keep needs a
//! time per path and a group carries none, so each path is asked of the
//! service's `stat` — no read — and the page asks once for the panel
//! instead of once a row.

use std::net::TcpStream;
use std::sync::Mutex;

use scour_proto::{Request, Response};

use crate::http;
use crate::{Link, call};

/// How many paths may be timed in one answer. The page asks for 25 groups; a
/// group of hundreds costs one `stat` a copy, and past this they answer 0.
const TIMED: usize = 600;

/// The groups `/api/dupes` gives, with `mtimes` beside `paths` — unix seconds,
/// 0 where the file is gone or sat past [`TIMED`].
pub fn api_duplicates(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
    let mb = |k: &str, d: u64| -> u64 {
        req.param(k).and_then(|s| s.parse().ok()).unwrap_or(d) * 1024 * 1024
    };
    let request = Request::Duplicates {
        under: req.param("under").unwrap_or_default().to_owned(),
        min_size: mb("min_mb", 1),
        read_budget: mb("budget_mb", 0),
        top: req.param("top").and_then(|s| s.parse().ok()).unwrap_or(25),
    };
    match call(client, request) {
        Ok(Response::Duplicates {
            groups,
            candidates,
            waste,
            proven,
            read,
            unconfirmed,
        }) => {
            let mut left = TIMED;
            let groups: Vec<serde_json::Value> = groups
                .iter()
                .map(|g| {
                    let take = left.min(g.paths.len());
                    left -= take;
                    let mtimes: Vec<i64> = g.paths[..take]
                        .iter()
                        .map(|p| mtime_of(client, p))
                        .chain(std::iter::repeat_n(0, g.paths.len() - take))
                        .collect();
                    serde_json::json!({
                        "size": g.size,
                        "waste": g.waste,
                        "paths": g.paths,
                        "mtimes": mtimes,
                        // "Identical" and "the same length" are different claims.
                        "certainty": g.certainty,
                    })
                })
                .collect();
            http::json(
                stream,
                &serde_json::json!({
                    "groups": groups,
                    "candidates": candidates,
                    "waste": waste,
                    "proven": proven,
                    "read": read,
                    "unconfirmed": unconfirmed,
                }),
            )
        }
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// When the file was last written, or 0. A path the index no longer owns
/// answers nothing rather than failing the whole panel.
fn mtime_of(client: &Mutex<Link>, path: &str) -> i64 {
    match call(
        client,
        Request::Stat {
            path: path.to_owned(),
        },
    ) {
        Ok(Response::Stat(e)) => e.meta.mtime,
        _ => 0,
    }
}
