//! Routes that search: a page of rows, its count, the sentence, a folder's
//! weight, duplicates, and the whole answer as CSV.

use super::*;
use scour_client::Pool;
use scour_core::{DirUsage, Owner, Page, SortKey};
use scour_ipc::Client;
use scour_proto::{Request, Response};
use std::io::Write;
use std::net::TcpStream;

/// `text`: whether a `content:` find's line is sent with its row. Off under
/// `--no-preview`, which promises the page no file's contents.
pub(super) fn api_search(stream: &mut TcpStream, client: &Pool, req: &http::Req, text: bool) {
    let query = req.param("q").unwrap_or_default().to_owned();
    let limit: u32 = req
        .param("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200)
        .min(1_000);
    let offset: u32 = req
        .param("offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // A keystroke does not pay for an exact count: on 2.1 M entries, exact
    // against a cap of 10,000, `rapor` is 47.9 ms against 6.6. `/api/count`
    // fetches the exact total afterwards for the scrollbar to settle on.
    let cap: u32 = req
        .param("cap")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000);
    let request = Request::Search {
        query,
        sort: sort_of(req.param("sort")),
        descending: req.param("desc").unwrap_or("1") != "0",
        page: Page {
            offset,
            limit,
            count_cap: cap,
        },
    };
    match call(client, request) {
        Ok(Response::Search(r)) => {
            let rows: Vec<serde_json::Value> = r
                .hits
                .iter()
                .map(|h| {
                    // Everything the index holds, because the page decides
                    // which columns to show — but not the full path, which
                    // `dir` and `name` spell and which cost 19-29% of a window.
                    serde_json::json!({
                        "name": h.name(),
                        "dir": h.parent(),
                        "ext": scour_core::ext_of(h.name()),
                        "size": h.meta.size,
                        "disk": h.meta.disk,
                        "mtime": h.meta.mtime,
                        "ctime": h.meta.ctime,
                        "atime": h.meta.atime,
                        "is_dir": h.is_dir,
                        "kind": h.kind.token(),
                        "perm": scour_core::mode_string(h.meta.mode),
                        "user": owner_name(Owner::User, h.meta.uid),
                        "group": owner_name(Owner::Group, h.meta.gid),
                        "items": h.meta.items,
                        // Absent rather than zero: unknown is not empty.
                        "under": h.under.map(|u| serde_json::json!({
                            "disk": u.disk, "files": u.files
                        })),
                        // So the page asks only for the pictures that exist.
                        "thumb": icons::has_thumbnail(&h.path, h.kind),
                        // And whether one could be: two lookups, no syscall.
                        "make": icons::may_thumbnail(&h.path, h.kind),
                        // Where a `content:` query found its text, cut in three
                        // at the find: the page counts in UTF-16, not bytes.
                        "found": h.found.iter().filter(|_| text).map(found_json).collect::<Vec<_>>(),
                    })
                })
                .collect();
            http::json(
                stream,
                &serde_json::json!({
                    "rows": rows,
                    "total": r.total,
                    "capped": r.capped,
                    "took_us": r.took_us,
                    "rows_visited": r.rows_visited,
                    // Only on a query that reads files: how far that has got.
                    "reading": r.reading,
                    // Only on a search that found nothing: what it nearly found.
                    "similar": r.similar,
                }),
            );
        }
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// One place a `content:` query found its text, as the page draws it: the
/// line's number and the line in three pieces, the find in the middle. A find
/// that does not fall on characters is shown as the line alone.
fn found_json(f: &scour_core::FoundLine) -> serde_json::Value {
    let (at, len) = (f.at.0 as usize, f.at.1 as usize);
    let (pre, hit, post) = match (
        f.text.get(..at),
        f.text.get(at..at + len),
        f.text.get(at + len..),
    ) {
        (Some(pre), Some(hit), Some(post)) => (pre, hit, post),
        _ => (f.text.as_str(), "", ""),
    };
    serde_json::json!({ "line": f.line, "pre": pre, "hit": hit, "post": post })
}

/// The whole result set, as a spreadsheet: one walk in the service under one
/// read lock, so there is no ceiling, no trailer and no row seen twice. `sort`
/// and `desc` are accepted and do nothing; the quoting is `scour_export`'s.
pub(super) fn api_csv(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let columns: Vec<String> = req
        .param("cols")
        .map(|c| {
            c.split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let request = Request::Export {
        query: req.param("q").unwrap_or_default().to_owned(),
        columns,
    };

    // Its own connection, held for the whole walk and not returned: an
    // abandoned stream leaves frames in flight.
    let addr = client.addr().to_owned();
    let mut link = match Client::connect(&addr) {
        Ok(c) => c,
        Err(e) => {
            http::fail(stream, "502 Bad Gateway", &e.to_string());
            return;
        }
    };

    // Past this line there is nowhere to put a status code.
    http::attachment(stream, "text/csv; charset=utf-8", "scour.csv");
    let result = link.stream(request, |piece| match piece {
        Response::ExportChunk { csv } => stream.write_all(csv.as_bytes()).is_ok(),
        // Not a piece of an export: ignored rather than taken as the end.
        _ => true,
    });
    // A short file is the only signal left once a body has begun.
    let _ = result;
    let _ = stream.flush();
}

/// How many match, exactly, however long that takes. Separate from the search,
/// whose rows have to be on screen before the next keystroke.
pub(super) fn api_count(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let request = Request::Count {
        query: req.param("q").unwrap_or_default().to_owned(),
        cap: req
            .param("cap")
            .and_then(|s| s.parse().ok())
            .unwrap_or(u32::MAX),
    };
    match call(client, request) {
        // `misread` is dropped: the query line colours the offending run.
        Ok(Response::Count {
            total,
            capped,
            took_us,
            misread: _,
        }) => http::json(
            stream,
            &serde_json::json!({ "total": total, "capped": capped, "took_us": took_us }),
        ),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// The same file, several times over, under one folder. Scoped by the report's
/// `under:` term; sizes only is 84 ms over thirty thousand candidates, and
/// confirming reads disk, on a button.
pub(super) fn api_dupes(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let mb = |k: &str, d: u64| -> u64 {
        req.param(k)
            .and_then(|s| s.parse().ok())
            .unwrap_or(d)
            .saturating_mul(1024 * 1024)
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
        }) => http::json(
            stream,
            &serde_json::json!({
                "groups": groups.iter().map(|g| serde_json::json!({
                    "size": g.size,
                    "waste": g.waste,
                    "paths": g.paths,
                    // "Identical" and "the same length" are different claims.
                    "certainty": g.certainty,
                })).collect::<Vec<_>>(),
                "candidates": candidates,
                "waste": waste,
                "proven": proven,
                "read": read,
                "unconfirmed": unconfirmed,
            }),
        ),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// What a folder weighs, and which of its children weigh the most. `top` come
/// back, heaviest first; `child_count` is how many there were before the cut.
pub(super) fn api_usage(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let request = Request::Usage {
        path: req.param("path").unwrap_or_default().to_owned(),
        top: req.param("top").and_then(|s| s.parse().ok()).unwrap_or(24),
        // Absent and empty mean the same, so the page can send the box unread.
        query: req.param("q").unwrap_or_default().to_owned(),
    };
    match call(client, request) {
        Ok(Response::Usage(r)) => {
            let dir = |d: &DirUsage| {
                serde_json::json!({
                    "path": d.path,
                    "bytes": d.bytes,
                    "disk": d.disk,
                    "files": d.files,
                    "age": d.age,
                })
            };
            http::json(
                stream,
                &serde_json::json!({
                    "root": dir(&r.root),
                    "children": r.children.iter().map(dir).collect::<Vec<_>>(),
                    "child_count": r.child_count,
                    "took_us": r.took_us,
                }),
            );
        }
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// What the query means and what could follow the caret. Tokenised in the
/// service: a second parser here would colour a lie the day the two disagreed.
pub(super) fn api_explain(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let request = Request::Explain {
        query: req.param("q").unwrap_or_default().to_owned(),
        cursor: req.param("cursor").and_then(|s| s.parse().ok()),
    };
    match call(client, request) {
        Ok(Response::Explain {
            description,
            spans,
            completions,
            needs_content,
        }) => {
            // Serialised whole, not field by field, so a field added to the
            // engine arrives without this file being touched. The names are
            // serde's `snake_case`, which is what the page matches on.
            let spans = serde_json::to_value(&spans).unwrap_or_default();
            let completions = serde_json::to_value(&completions).unwrap_or_default();
            http::json(
                stream,
                &serde_json::json!({
                    "description": description,
                    "spans": spans,
                    "completions": completions,
                    "needs_content": needs_content,
                }),
            );
        }
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// The order a column header asked for; every key the engine has. An
/// unrecognised name is `Modified`, which is what changed last.
pub(super) fn sort_of(s: Option<&str>) -> SortKey {
    match s.unwrap_or("modified") {
        "relevance" => SortKey::Relevance,
        "name" => SortKey::Name,
        "path" => SortKey::Path,
        "size" => SortKey::Size,
        "created" => SortKey::Created,
        "accessed" => SortKey::Accessed,
        "ext" => SortKey::Ext,
        "kind" => SortKey::Kind,
        "mode" => SortKey::Mode,
        "uid" => SortKey::Uid,
        "gid" => SortKey::Gid,
        "disk" => SortKey::Disk,
        _ => SortKey::Modified,
    }
}
