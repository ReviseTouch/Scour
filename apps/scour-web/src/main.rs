//! Scour in a browser.
//!
//! A bridge, and only a bridge. The service speaks [`scour_proto`] over a Unix
//! socket; a browser cannot open one, and neither can a browser extension —
//! an extension's way in is a native-messaging host, which is a local process
//! talking to the socket, which is this with a different mouth. So the shape
//! of the answer is the same either way and only the transport differs, which
//! is why the page reaches the service through one small object it can swap.
//!
//! ## What listens, and why that is the part to be careful about
//!
//! The socket the service uses is protected by the filesystem: it lives in the
//! user's own runtime directory and nothing else can open it. A TCP port has
//! none of that, and what is behind this one is an index of every file the user
//! owns — where things are, what they are called, when they changed. So:
//!
//! * **127.0.0.1 only**, never `0.0.0.0`, and there is no flag to change it.
//! * **A token**, generated per run and printed with the URL. Without it every
//!   route answers 403. This is what stops another program on the machine —
//!   and any web page that guesses the port — from reading the index.
//! * **Origin checked** on every request, because a page on the internet can
//!   make a browser send one here. Same-origin or nothing.
//! * **Read-only.** `rescan` and `maintain` exist in the protocol and are not
//!   routed. A page in a browser does not get to make the service work.

mod http;

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;
use scour_core::{FacetBy, Page, SortKey};
use scour_ipc::Client;
use scour_proto::{Request, Response};

/// The page, built in.
///
/// Compiled in rather than read from disk so that the binary is the whole of
/// the program: a file path is one more thing to get wrong at install time,
/// and there is nothing here a user would want to edit separately.
const PAGE: &str = include_str!("page.html");

#[derive(Parser, Debug)]
#[command(name = "scour-web", about = "Scour in a browser", version)]
struct Args {
    /// Which port to listen on. Zero asks the system for a free one.
    #[arg(long, default_value_t = 7621)]
    port: u16,
    /// Talk to a service listening here.
    #[arg(long)]
    socket: Option<String>,
    /// Print the address and exit without opening anything.
    #[arg(long)]
    no_open: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let addr = args
        .socket
        .clone()
        .unwrap_or_else(|| scour_config::Config::load_or_default().0.socket());

    // Fail here rather than in the browser: a page that loads and then says
    // "no service" is a worse error than a command that does not start.
    let client = Client::connect(&addr).with_context(|| {
        format!("no Scour service is listening on {addr}. Start one with `scourd`.")
    })?;
    let client = Arc::new(Mutex::new(client));

    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, args.port)))
        .with_context(|| format!("cannot listen on 127.0.0.1:{}", args.port))?;
    let port = listener.local_addr()?.port();
    let token = token();
    let url = format!("http://127.0.0.1:{port}/?t={token}");

    eprintln!("scour-web: {url}");
    eprintln!("scour-web: the token is per run — restarting invalidates the link");
    if !args.no_open {
        open(&url);
    }

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let client = Arc::clone(&client);
        let token = token.clone();
        // A thread a connection, and the connection closes after one exchange.
        // A browser opens a handful; there is nothing here to pool.
        std::thread::spawn(move || serve(stream, &client, &token));
    }
    Ok(())
}

/// A token nobody can guess, from the one source of randomness every platform
/// agrees on.
fn token() -> String {
    let mut bytes = [0u8; 16];
    // `getrandom` through the standard library's hasher would be indirect;
    // reading the OS source directly is one line and says what it means.
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut bytes);
    }
    // A pid and a clock, folded in, so that a platform without `/dev/urandom`
    // still does not produce the same token twice.
    let salt = std::process::id().to_le_bytes();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
        .to_le_bytes();
    for (i, b) in bytes.iter_mut().enumerate() {
        *b ^= salt[i % 4] ^ now[i % 4];
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn open(url: &str) {
    let _ = std::process::Command::new("xdg-open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn serve(mut stream: TcpStream, client: &Mutex<Client>, token: &str) {
    let Some(req) = http::read_request(&stream) else {
        return;
    };

    // A page on the internet can make a browser send a request here, and the
    // browser will attach nothing that proves otherwise. What it cannot do is
    // forge `Origin`, so anything carrying one that is not ours is refused
    // before it is looked at.
    if let Some(origin) = req.header("origin")
        && !origin.ends_with(&format!(
            ":{}",
            stream.local_addr().map(|a| a.port()).unwrap_or(0)
        ))
    {
        http::fail(&mut stream, "403 Forbidden", "cross-origin");
        return;
    }
    if req.method != "GET" {
        http::fail(&mut stream, "405 Method Not Allowed", "GET only");
        return;
    }
    // Constant work regardless of how much of the token is right. The
    // comparison is not the expensive part of a search, so there is no reason
    // to leak how far a guess got.
    let given = req.param("t").unwrap_or_default();
    if given.len() != token.len()
        || given
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            != 0
    {
        http::fail(&mut stream, "403 Forbidden", "bad or missing token");
        return;
    }

    match req.path.as_str() {
        "/" => http::respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            PAGE.as_bytes(),
        ),
        "/api/search" => api_search(&mut stream, client, &req),
        "/api/facets" => api_facets(&mut stream, client, &req),
        "/api/status" => api_status(&mut stream, client),
        "/api/explain" => api_explain(&mut stream, client, &req),
        _ => http::fail(&mut stream, "404 Not Found", "no such route"),
    }
}

/// One call to the service, with the lock held only for as long as it takes.
fn call(client: &Mutex<Client>, request: Request) -> Result<Response, String> {
    let mut guard = client.lock().map_err(|_| "the bridge lost its client")?;
    guard.call(request).map_err(|e| e.to_string())
}

fn api_search(stream: &mut TcpStream, client: &Mutex<Client>, req: &http::Req) {
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
    let request = Request::Search {
        query,
        sort: sort_of(req.param("sort")),
        descending: req.param("desc").unwrap_or("1") != "0",
        page: Page {
            offset,
            limit,
            ..Page::default()
        },
    };
    match call(client, request) {
        Ok(Response::Search(r)) => {
            let rows: Vec<serde_json::Value> = r
                .hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "path": h.path,
                        "name": h.name(),
                        "dir": parent_of(&h.path),
                        "size": h.meta.size,
                        "mtime": h.meta.mtime,
                        "is_dir": h.is_dir,
                        "kind": h.kind.token(),
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
                }),
            );
        }
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

fn api_facets(stream: &mut TcpStream, client: &Mutex<Client>, req: &http::Req) {
    let request = Request::Facets {
        query: req.param("q").unwrap_or_default().to_owned(),
        by: FacetBy::Kind,
    };
    match call(client, request) {
        Ok(Response::Facets(r)) => {
            let facets: Vec<serde_json::Value> = r
                .facets
                .iter()
                .map(|f| serde_json::json!({ "key": f.key, "count": f.count }))
                .collect();
            http::json(
                stream,
                &serde_json::json!({ "facets": facets, "capped": r.capped }),
            );
        }
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

fn api_status(stream: &mut TcpStream, client: &Mutex<Client>) {
    match call(client, Request::Status {}) {
        Ok(Response::Status(s)) => http::json(
            stream,
            &serde_json::json!({
                "entries": s.entries,
                "scanning": s.scanning,
                "watching": s.watching,
                "sources": s.sources,
                "pending": s.pending,
                "index_bytes": s.index_bytes,
            }),
        ),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// What the query means and what could follow the caret.
///
/// The colouring lives in the service on purpose — a frontend that tokenised
/// for itself would be a second parser, and the day the two disagreed the box
/// would be confidently colouring a lie.
fn api_explain(stream: &mut TcpStream, client: &Mutex<Client>, req: &http::Req) {
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
            let spans: Vec<serde_json::Value> = spans
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "start": s.start,
                        "len": s.len,
                        "role": format!("{:?}", s.role).to_lowercase(),
                    })
                })
                .collect();
            let completions: Vec<serde_json::Value> = completions
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "insert": c.insert,
                        "label": c.label,
                        "about": c.about,
                        "kind": format!("{:?}", c.kind).to_lowercase(),
                    })
                })
                .collect();
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

fn sort_of(s: Option<&str>) -> SortKey {
    match s.unwrap_or("modified") {
        "name" => SortKey::Name,
        "path" => SortKey::Path,
        "size" => SortKey::Size,
        "created" => SortKey::Created,
        "ext" => SortKey::Ext,
        "kind" => SortKey::Kind,
        _ => SortKey::Modified,
    }
}

fn parent_of(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((dir, _)) => dir,
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parent_is_the_path_without_its_last_component() {
        assert_eq!(parent_of("/home/u/a.rs"), "/home/u");
        assert_eq!(parent_of("/a.rs"), "/");
        assert_eq!(parent_of("bare"), "");
    }

    #[test]
    fn two_tokens_from_one_process_are_not_the_same() {
        assert_ne!(token(), token());
        assert_eq!(token().len(), 32);
    }

    #[test]
    fn an_unknown_sort_key_is_the_default_rather_than_an_error() {
        // A URL is typed by people and generated by an older page; a sort key
        // nobody recognises should show the list, not a 400.
        assert_eq!(sort_of(Some("zurna")), SortKey::Modified);
        assert_eq!(sort_of(None), SortKey::Modified);
        assert_eq!(sort_of(Some("size")), SortKey::Size);
    }
}
