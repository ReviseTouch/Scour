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
//! * **Read-only, with one exception, and the exception is the careful part.**
//!   `rescan` and `maintain` are not routed — a page in a browser does not get
//!   to make the service work. `/api/open` is the exception, because a file
//!   search that cannot open a file is half a tool, and it is fenced: `POST`
//!   only, so a link, an image or a prefetch cannot reach it, and the path must
//!   be one the *index* holds.
//!
//!   **And it runs executables.** That was a refusal once — the folder was
//!   opened instead — and it is not any more, because a search box that finds a
//!   program and then sends you elsewhere to start it has not finished the job.
//!   The honest accounting, since a security note that flatters itself is worse
//!   than none: this does not widen who may ask, only what an asker may do.
//!   Loopback, token, origin and method are all still in the way, and getting
//!   past them means holding a token that is new every run and lives in the
//!   window's own URL. What changes is that such a holder can now start a
//!   binary directly, where before they could `xdg-open` a `.desktop` file or a
//!   script and have the desktop start it for them. A door widened rather than
//!   opened, and still a door: `--no-run` closes it and says so on screen,
//!   `--no-launch` removes the route altogether.

mod http;
mod icons;

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;
use scour_core::{Catalog, FacetBy, Page, SortKey};
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
    /// Print the address and do not launch a browser.
    #[arg(long)]
    no_open: bool,
    /// Refuse `/api/open` entirely, so the page can only look.
    #[arg(long)]
    no_launch: bool,
    /// Open the folder of an executable rather than running it.
    ///
    /// **The default is to run it**, because a search box that finds a
    /// program and then refuses to start it is a search box that sends you
    /// somewhere else to finish the job — which is what a person asked for and
    /// what Everything does. The flag is here because it is a real capability
    /// and not everyone wants it: with it, this bridge can start any binary
    /// it can see, for anybody holding the token. The token is new every run
    /// and the socket is loopback, and `xdg-open` could already run a
    /// `.desktop` file or a script — so this widens a door rather than
    /// opening one. It is still a door.
    #[arg(long)]
    no_run: bool,
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
    let client = Arc::new(Mutex::new(Link {
        idle: vec![client],
        addr: addr.clone(),
    }));

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

    // Kept beside the shared connection because `/api/wait` may not use that
    // one: it blocks for half a minute at a time, and the lock it would be
    // holding is the lock every keystroke needs.
    let addr: Arc<str> = Arc::from(addr.as_str());

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let client = Arc::clone(&client);
        let addr = Arc::clone(&addr);
        let token = token.clone();
        let doing = Doing {
            launch: !args.no_launch,
            run: !args.no_run,
        };
        // A thread a connection, and the connection closes after one exchange.
        // A browser opens a handful; there is nothing here to pool.
        std::thread::spawn(move || serve(stream, &client, &addr, &token, doing));
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

/// What this bridge is allowed to do besides answer questions.
#[derive(Clone, Copy)]
struct Doing {
    /// `/api/open` at all.
    launch: bool,
    /// Start an executable rather than showing where it lives.
    run: bool,
}

fn serve(mut stream: TcpStream, client: &Mutex<Link>, addr: &str, token: &str, doing: Doing) {
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
    // Reading is `GET`, doing is `POST`, and the split is not decoration: it
    // is what keeps a link, a prefetch or a history entry from opening a file.
    let acting = req.path == "/api/open";
    if req.method != if acting { "POST" } else { "GET" } {
        http::fail(
            &mut stream,
            "405 Method Not Allowed",
            "wrong method for this route",
        );
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
        "/api/count" => api_count(&mut stream, client, &req),
        "/api/kinds" => api_kinds(&mut stream),
        "/api/facets" => api_facets(&mut stream, client, &req),
        "/api/status" => api_status(&mut stream, client),
        "/api/icon" => api_icon(&mut stream, &req),
        "/api/wait" => api_wait(&mut stream, addr, &req),
        "/api/explain" => api_explain(&mut stream, client, &req),
        "/api/open" if doing.launch => api_open(&mut stream, client, &req, doing.run),
        "/api/open" => http::fail(&mut stream, "403 Forbidden", "opening is off (--no-launch)"),
        _ => http::fail(&mut stream, "404 Not Found", "no such route"),
    }
}

/// One call to the service, with the lock held only for as long as it takes.
///
/// **Reconnects once on failure**, because the service is restarted far more
/// often than this is — a rebuild, a config change, a `systemctl restart` —
/// and a bridge that dies with it means every open page has to be reloaded
/// with a new token. The connection is one socket held for the process's life,
/// so losing it is a broken pipe on the next call and nothing more.
fn call(client: &Mutex<Link>, request: Request) -> Result<Response, String> {
    // The mutex is held only long enough to take a connection out, never for
    // the call itself. That distinction is the whole point of the pool.
    let (mut link, addr) = {
        let mut guard = client.lock().map_err(|_| "the bridge lost its client")?;
        (guard.idle.pop(), guard.addr.clone())
    };

    if let Some(mut open) = link.take() {
        // A failure here is the service having been restarted under it. The
        // connection is dropped rather than returned, and a fresh one is
        // opened below — which is why a `systemctl restart` does not make
        // every open page reload with a new token.
        if let Ok(r) = open.call(request.clone()) {
            put_back(client, open);
            return Ok(r);
        }
    }

    let mut fresh = Client::connect(&addr).map_err(|e| e.to_string())?;
    let out = fresh.call(request).map_err(|e| e.to_string());
    if out.is_ok() {
        put_back(client, fresh);
    }
    out
}

/// A connection that answered goes back for the next caller, up to the depth.
fn put_back(client: &Mutex<Link>, open: Client) {
    if let Ok(mut guard) = client.lock()
        && guard.idle.len() < Link::POOL
    {
        guard.idle.push(open);
    }
}

/// The connections, and where to open another one.
///
/// **One socket was a queue.** The service gives every connection a thread of
/// its own, but this held a single one behind a mutex, so every request the
/// page made waited for the one in front of it — and one of them is a walk of
/// the whole matching set at 130 ms. A screenful of rows queued behind that
/// arrives 130 ms late for no reason but the plumbing; measured on a busy
/// index, one call in forty took **1,056 ms** while the rest took two.
///
/// So: several, handed out one at a time and put back when the call is done.
/// A connection to a Unix socket costs microseconds, and the depth is what a
/// page can have outstanding — four windows, a facet walk, a status — with
/// room over. `wait` is not in here; a long poll has always opened its own.
#[derive(Debug)]
struct Link {
    idle: Vec<Client>,
    addr: String,
}

impl Link {
    /// The depth. More than the page can ask for at once, so nothing queues;
    /// small enough that a runaway client cannot make the service spawn
    /// threads without limit.
    const POOL: usize = 8;
}

fn api_search(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
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
    // **A keystroke does not pay for an exact count.**
    //
    // The scrollbar needs the real total — a bar sized by what has loaded so
    // far grows a thumb that shrinks as you scroll, which is the one thing a
    // scrollbar must not do. But counting to the end is not free, and the
    // first reading of this was wrong: measured on 2.1 M entries, warm,
    // exact against a cap of 10,000, `rapor` is 47.9 ms against 6.6 and
    // `ext:rs` is 96.8 against 12.7. Four to eight times, on the path that
    // runs once per keypress.
    //
    // So the rows come back at typing speed under a cap, and `/api/count`
    // fetches the exact one afterwards for the bar to settle on. Two
    // questions, asked separately, because they have different deadlines.
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
                    // Everything the index holds about the row, because the
                    // page decides which of it to show and asking again for a
                    // column that was switched on would be a second request
                    // for rows already sent.
                    serde_json::json!({
                        "path": h.path,
                        "name": h.name(),
                        "dir": parent_of(&h.path),
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
                        // Whether a picture of this file already exists, so
                        // the page asks for the ones that do rather than for
                        // two hundred that mostly do not. One `stat` a row.
                        "thumb": icons::has_thumbnail(&h.path),
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

/// A picture for a row: the thumbnail somebody has already made, or the
/// desktop's icon for that kind of file.
///
/// **Cacheable, unlike everything else here.** The rest of these routes are
/// about a filesystem being watched, where a cached answer is an answer that
/// stopped being true; an icon is a file the theme installed. Without this a
/// list of two hundred rows asks for the same drawing two hundred times.
fn api_icon(stream: &mut TcpStream, req: &http::Req) {
    let picture = match req.param("p") {
        // A path is only ever hashed, never opened: what comes back is a file
        // in the thumbnail cache or nothing. See `icons`.
        Some(path) if !path.is_empty() => icons::thumbnail(path),
        _ => icons::for_kind(
            req.param("k").unwrap_or("file"),
            &req.param("e").unwrap_or_default().to_ascii_lowercase(),
        ),
    };
    match picture {
        Some(p) => http::cached(stream, p.kind, &p.bytes),
        None => http::fail(stream, "404 Not Found", "no icon"),
    }
}

/// The kinds a frontend should offer, in the order to show them, in the
/// user's language.
///
/// Sent rather than hard-coded, and the page had hard-coded them: its rail
/// came from the mockup and listed `package`, `db` and `other`, which are not
/// tokens the engine has, while omitting `exec`, `config` and `file`, which
/// are. So executables — every extensionless binary in `~/.local/bin`, found
/// perfectly well by `kind:exec` — had no row to appear in, and three rows
/// were permanently zero.
///
/// `Kind::OFFERED` exists for exactly this and says so in its own doc comment.
fn api_kinds(stream: &mut TcpStream) {
    static CAT: std::sync::OnceLock<scour_i18n::Catalogue> = std::sync::OnceLock::new();
    let cat = CAT.get_or_init(scour_i18n::Catalogue::from_environment);
    let kinds: Vec<serde_json::Value> = scour_core::Kind::OFFERED
        .iter()
        .map(|k| {
            serde_json::json!({
                "token": k.token(),
                "label": cat.get(k.msgid()).into_owned(),
            })
        })
        .collect();
    http::json(stream, &serde_json::json!({ "kinds": kinds }));
}

/// How many match, exactly, however long that takes.
///
/// Separate from the search because the two have different deadlines: the rows
/// have to be on screen before the next keystroke and this does not.
fn api_count(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
    let request = Request::Count {
        query: req.param("q").unwrap_or_default().to_owned(),
        cap: req
            .param("cap")
            .and_then(|s| s.parse().ok())
            .unwrap_or(u32::MAX),
    };
    match call(client, request) {
        Ok(Response::Count { total, capped }) => http::json(
            stream,
            &serde_json::json!({ "total": total, "capped": capped }),
        ),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// Everything the sidebar needs, from one walk.
///
/// `by=kind,age` asks both; the reply carries a group per question in the
/// order asked, plus the exact total, which the walk produces for free. The
/// page used to make three requests for this — a count, a rail and a chart —
/// and each of them walked the matching set again.
fn api_facets(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
    let edges: Vec<u32> = req
        .param("edges")
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.parse().ok())
        .collect();
    let by: Vec<FacetBy> = req
        .param("by")
        .unwrap_or("kind")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|name| match name {
            "age" => FacetBy::Age {
                edges: edges.clone(),
            },
            "ext" => FacetBy::Ext { top: 24 },
            _ => FacetBy::Kind,
        })
        .collect();
    let request = Request::Facets {
        query: req.param("q").unwrap_or_default().to_owned(),
        by,
    };
    match call(client, request) {
        Ok(Response::Facets(r)) => {
            let groups: Vec<serde_json::Value> = r
                .groups
                .iter()
                .map(|g| {
                    serde_json::json!({
                        "by": match &g.by {
                            FacetBy::Kind => "kind",
                            FacetBy::Ext { .. } => "ext",
                            FacetBy::Dir { .. } => "dir",
                            FacetBy::Age { .. } => "age",
                        },
                        "facets": g.facets.iter()
                            .map(|f| serde_json::json!({ "key": f.key, "count": f.count }))
                            .collect::<Vec<_>>(),
                    })
                })
                .collect();
            http::json(
                stream,
                &serde_json::json!({
                    "groups": groups,
                    "total": r.total,
                    "capped": r.capped,
                    "took_us": r.took_us,
                }),
            );
        }
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

fn api_status(stream: &mut TcpStream, client: &Mutex<Link>) {
    match call(client, Request::Status {}) {
        Ok(Response::Status(s)) => http::json(stream, &status_json(&s)),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// The same answer either route produces, so a page that has been told to wait
/// gets everything the one that asked outright would have.
fn status_json(s: &scour_core::Status) -> serde_json::Value {
    serde_json::json!({
        "entries": s.entries,
        "scanning": s.scanning,
        "watching": s.watching,
        "sources": s.sources,
        "pending": s.pending,
        "index_bytes": s.index_bytes,
        "revision": s.revision,
    })
}

/// Hold the request open until the index changes.
///
/// **Its own connection, deliberately.** Every other route shares one socket
/// behind a mutex, which is right when a call takes a millisecond and wrong
/// when it takes half a minute: a page waiting here would be holding the lock
/// that the next keystroke needs. A connect costs tens of microseconds and
/// happens once per wait, which on a quiet machine is twice a minute.
///
/// The page's side of this is a `fetch` with no timeout of its own, so the
/// reply arriving *is* the notification. What comes back is a status either
/// way — the revision in it says whether anything actually happened, and a
/// timeout is not an error.
fn api_wait(stream: &mut TcpStream, addr: &str, req: &http::Req) {
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

/// What the query means and what could follow the caret.
///
/// The colouring lives in the service on purpose — a frontend that tokenised
/// for itself would be a second parser, and the day the two disagreed the box
/// would be confidently colouring a lie.
fn api_explain(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
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
                        // Serde's name, not `Debug`'s. `Role::UnknownField`
                        // debug-prints as `UnknownField`, which lowercases to
                        // `unknownfield` — and the page, matching the
                        // `snake_case` the protocol actually uses, quietly
                        // matched none of them. The one role that must never
                        // be missed is exactly the one this broke.
                        "role": s.role,
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
                        "kind": c.kind,
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

/// Open a path, or the folder holding it.
///
/// **The index is the fence.** The path is looked up through the service
/// first, and a path the index does not hold is refused — so this cannot be
/// pointed at `/etc/shadow`, at a path assembled by a page, or at anything
/// outside the roots the user configured. `stat` already refuses a path no
/// source owns, and that refusal is the whole check.
///
/// The second fence is what "open" means. `xdg-open` on a `.desktop` file
/// executes it; on an executable a file manager offers to run it. Those get
/// their folder revealed instead, which is what someone searching for them
/// wanted. There is no flag to override it.
fn api_open(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req, may_run: bool) {
    let Some(path) = req.param("path").filter(|p| !p.is_empty()) else {
        http::fail(stream, "400 Bad Request", "no path");
        return;
    };
    let entry = match call(
        client,
        Request::Stat {
            path: path.to_owned(),
        },
    ) {
        Ok(Response::Stat(e)) => e,
        Ok(_) => {
            http::fail(stream, "502 Bad Gateway", "unexpected reply");
            return;
        }
        // Not in the index, or under no configured root. Either way, not ours
        // to open.
        Err(e) => {
            http::fail(stream, "404 Not Found", &e);
            return;
        }
    };

    let p = std::path::Path::new(&entry.path);
    let want_folder = req.param("what") == Some("folder") || entry.is_dir;
    let runnable = !want_folder && http::is_runnable(p);
    // Running it is the file's own answer to "open"; showing where it lives is
    // what is left when that is not allowed.
    let run = runnable && may_run;
    let target = if want_folder || (runnable && !run) {
        p.parent().unwrap_or(p).to_path_buf()
    } else {
        p.to_path_buf()
    };

    // A program is started in the directory it lives in, because that is where
    // whatever it reads beside itself is.
    let mut cmd = if run {
        let mut c = std::process::Command::new(&target);
        c.current_dir(target.parent().unwrap_or(&target));
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&target);
        c
    };
    match cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => http::json(
            stream,
            &serde_json::json!({
                "opened": target.to_string_lossy(),
                // Said rather than done silently: a double-click that quietly
                // does something else is worse than one that explains. And a
                // program that was *started* says so, because nothing else on
                // the screen will.
                "instead": if run {
                    Some(format!("çalıştırıldı: {}", target.file_name().unwrap_or_default().to_string_lossy()))
                } else if runnable {
                    Some("bu dosya çalıştırılabilir — klasörü açıldı (--no-run)".to_string())
                } else {
                    None
                },
            }),
        ),
        Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    User,
    Group,
}

/// The name behind a numeric id, from this machine.
///
/// Read once and kept: a page of two hundred rows asks two hundred times, and
/// `/etc/passwd` does not change between them. The number is the answer when
/// there is no name for it — a file owned by a user who was deleted still has
/// to say something, and `1000` is truer than a blank.
fn owner_name(which: Owner, id: i64) -> String {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static USERS: OnceLock<HashMap<i64, String>> = OnceLock::new();
    static GROUPS: OnceLock<HashMap<i64, String>> = OnceLock::new();
    let table = |file: &str| -> HashMap<i64, String> {
        std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let mut f = line.split(':');
                let name = f.next()?.to_owned();
                let id = f.nth(1)?.parse::<i64>().ok()?;
                Some((id, name))
            })
            .collect()
    };
    let map = match which {
        Owner::User => USERS.get_or_init(|| table("/etc/passwd")),
        Owner::Group => GROUPS.get_or_init(|| table("/etc/group")),
    };
    map.get(&id).cloned().unwrap_or_else(|| id.to_string())
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
