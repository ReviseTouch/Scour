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

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;
use scour_core::{Catalog, DirUsage, FacetBy, Page, SortKey};
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
    /// The command that opens the desktop's own quick-look, if the detected
    /// one is wrong or there is none to detect.
    ///
    /// The file's path is appended. `--quicklook "gwenview"` on KDE, or
    /// `--quicklook "imv"` under a compositor that has no such thing. Empty
    /// means detect: `qlmanage -p` on macOS, `sushi` where it is installed,
    /// and nothing anywhere else — the button is only offered when there is
    /// something behind it.
    #[arg(long, value_name = "CMD")]
    quicklook: Option<String>,
    /// Refuse `/api/preview`, so the page never receives a file's contents.
    ///
    /// The preview panel reads files the index holds, which is the user's own
    /// home. That is a door widened rather than opened — the same loopback,
    /// token, origin and method fences are in front of it, and the path must
    /// be one the index holds — but it does turn "open this in an editor" into
    /// one GET, and somebody who would rather it did not should be able to say
    /// so. With this the panel shows what the index knows and nothing else.
    #[arg(long)]
    no_preview: bool,
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

    let _ = QUICKLOOK.set(scour_preview::quicklook(args.quicklook.as_deref()));

    eprintln!("scour-web: {url}");
    eprintln!("scour-web: the token is per run — restarting invalidates the link");
    // Said, because its absence is the ordinary case on three desktops out of
    // five and looks like a fault otherwise.
    match QUICKLOOK.get().and_then(|q| q.as_ref()) {
        Some(cmd) => eprintln!("scour-web: system preview: {}", cmd.join(" ")),
        None => eprintln!(
            "scour-web: no system preview found; the panel is the preview \
             (--quicklook names one)"
        ),
    }
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
            preview: !args.no_preview,
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
    /// `/api/preview` at all — hand the *contents* of a file to the page.
    preview: bool,
}

/// The desktop's quick-look command, resolved once at start.
///
/// Once, because it is a `PATH` walk and the answer cannot change while the
/// process runs — and because a lookup per request would be a lookup per
/// keystroke on a list somebody is arrowing through.
static QUICKLOOK: std::sync::OnceLock<Option<Vec<String>>> = std::sync::OnceLock::new();

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
    let acting =
        req.path == "/api/open" || (req.path == "/api/settings" && req.param("set").is_some());
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
        "/api/places" => api_places(&mut stream),
        "/api/settings" => api_settings(&mut stream, client, &req),
        "/api/facets" => api_facets(&mut stream, client, &req),
        "/api/usage" => api_usage(&mut stream, client, &req),
        "/api/dupes" => api_dupes(&mut stream, client, &req),
        "/api/status" => api_status(&mut stream, client),
        "/api/icon" => api_icon(&mut stream, &req),
        "/api/wait" => api_wait(&mut stream, addr, &req),
        "/api/explain" => api_explain(&mut stream, client, &req),
        "/api/preview" if doing.preview => api_preview(&mut stream, client, &req),
        "/api/preview" => http::fail(
            &mut stream,
            "403 Forbidden",
            "previewing is off (--no-preview)",
        ),
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
                        // What the folder holds, when the index could say. A
                        // folder with no number is a folder whose size is not
                        // known — which is true — where a zero would read as
                        // an empty one.
                        "under": h.under.map(|u| serde_json::json!({
                            "disk": u.disk, "files": u.files
                        })),
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

/// Where this person keeps things, so the page does not have to guess.
///
/// **It guessed, and it guessed the author's home directory.** The sidebar's
/// scope shortcuts were four literal paths under `/home/hasan` and the path
/// shortener replaced that same string with `~`. On anyone else's machine the
/// shortcuts point at folders that are not there and no path ever shortens —
/// the two most visible things in the window, both wrong, for everybody but
/// one person.
///
/// The names come from the XDG user directories, which is where a desktop
/// records what its owner calls Documents and Downloads in their own language,
/// and only the ones that exist are offered. A machine with no `user-dirs.dirs`
/// gets the home directory and nothing else, which is honest rather than four
/// dead links.
/// What this person's frontends remember.
///
/// **The second thing on this bridge that writes**, and the accounting is the
/// same as `/api/open`'s: `POST` only when it is setting, so a link or a
/// prefetch cannot change somebody's columns, and the token and the origin are
/// still in front of it. What it can write is a column list — not a file.
///
/// It lives in the service rather than in the browser because a browser loses
/// it. `localStorage` is flushed on a clean shutdown and dropped when the
/// process is killed — measured both ways — and a terminal interface cannot
/// read it at all. See `scour-settings`.
fn api_settings(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
    let request = match req.param("set") {
        Some(text) => match serde_json::from_str(text) {
            Ok(settings) => Request::SetSettings { settings },
            Err(e) => {
                http::fail(
                    stream,
                    "400 Bad Request",
                    &format!("unreadable settings: {e}"),
                );
                return;
            }
        },
        None => Request::Settings {},
    };
    match call(client, request) {
        Ok(Response::Settings(s)) => match serde_json::to_value(&s) {
            Ok(v) => http::json(stream, &v),
            Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
        },
        Ok(_) => http::json(stream, &serde_json::json!({ "saved": true })),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

fn api_places(stream: &mut TcpStream) {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut places: Vec<serde_json::Value> = Vec::new();
    if !home.is_empty() {
        // `user-dirs.dirs` is `XDG_DOCUMENTS_DIR="$HOME/Belgeler"` a line, and
        // the quoting and the `$HOME` are both part of the format.
        let conf = std::path::Path::new(&home).join(".config/user-dirs.dirs");
        let text = std::fs::read_to_string(&conf).unwrap_or_default();
        for line in text.lines() {
            let line = line.trim();
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if !key.starts_with("XDG_") || !key.ends_with("_DIR") {
                continue;
            }
            let path = value.trim().trim_matches('"').replace("$HOME", &home);
            // The desktop's own name for it, which is the last component and
            // is already in the owner's language.
            let label = path.rsplit('/').next().unwrap_or_default().to_owned();
            // `XDG_DESKTOP_DIR` is often the home itself on a headless setup,
            // and a shortcut to everything is not a shortcut.
            if label.is_empty() || path == home || !std::path::Path::new(&path).is_dir() {
                continue;
            }
            places.push(serde_json::json!({ "label": label, "path": path }));
        }
        places.sort_by(|a, b| a["label"].as_str().cmp(&b["label"].as_str()));
        places.dedup_by(|a, b| a["path"] == b["path"]);
    }
    http::json(
        stream,
        &serde_json::json!({ "home": home, "places": places, "mounts": mounts() }),
    );
}

/// Every mount point, and whether the kernel records reads on it.
///
/// **A column that shows a number nobody maintains is worse than an empty
/// one.** With `noatime`, `st_atime` is written once — when the file is made —
/// and never again, so a browser profile rewritten every second reports
/// "accessed eleven days ago", which is the day the application was installed.
/// Every one of those numbers is *true* and none of them answers the question
/// the column's heading asks.
///
/// **All of them, not only the `noatime` ones**, because mount points nest and
/// the deepest one owns the file: `/` is `noatime` on this machine while
/// `/mnt/depo` under it is `relatime`, so a list of just the silent mounts
/// would call the whole disk silent. That was the first version, and it marked
/// every row.
///
/// Read once per run: mount options do not change while a window is open, and
/// reopening it is the ordinary way to find out if they did. Empty on anything
/// without `/proc/self/mounts` — the honest answer where this cannot be asked,
/// and the column then behaves as it always did.
fn mounts() -> Vec<serde_json::Value> {
    let Ok(text) = std::fs::read_to_string("/proc/self/mounts") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            // `device point type options dump pass`, space separated, with
            // octal escapes in the point. A path with a space in it is the
            // only one that needs unescaping, and `\040` is the only escape
            // that turns up.
            let mut parts = line.split_whitespace();
            let point = parts.nth(1)?.replace(r"\040", " ");
            let opts = parts.nth(1)?;
            Some(serde_json::json!({
                "at": point,
                "reads": !opts.split(',').any(|o| o == "noatime"),
            }))
        })
        .collect()
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
        // `misread` is dropped here on purpose. This page asks `explain` on
        // every keystroke and colours the offending run inside the query line,
        // which says it earlier and in a better place than a note beside the
        // count. The surfaces that keep the warning are the ones with no
        // search box to colour.
        Ok(Response::Count {
            total,
            capped,
            misread: _,
        }) => http::json(
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
/// What a folder weighs, and which of its children weigh the most.
///
/// The report tab drew this from a table of twenty-four folders written into
/// the page when it was a mockup — real numbers once, measured against a home
/// directory in August, and frozen ever since. It sat three inches from a
/// sidebar that says *"Aşağısı canlı"*, which made it the one place in the
/// interface that showed somebody invented figures about their own disk.
///
/// The engine has answered this the whole time: `Request::Usage` ships, `scour
/// du` uses it, and it is 96 ms over 658,360 files because the sizes and the
/// parent links are already in the index and nothing has to touch the disk.
/// Only the route between them was missing.
///
/// `top` is how many children come back, heaviest first; `child_count` says
/// how many there were before the cut, so the page can say what it is not
/// showing rather than quietly showing less.
/// The same file, several times over, under one folder.
///
/// **Scoped by the same term the rest of the report is scoped by.** Changing
/// directory in the report changes `under:` and every panel recomputes; this
/// is one more panel and needs no new mechanism.
///
/// Reads nothing unless asked. The free answer — sizes only — is 84 ms over
/// thirty thousand candidates and is what the panel opens with; confirming
/// costs real disk and is a button somebody presses.
fn api_dupes(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
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
        }) => http::json(
            stream,
            &serde_json::json!({
                "groups": groups.iter().map(|g| serde_json::json!({
                    "size": g.size,
                    "waste": g.waste,
                    "paths": g.paths,
                    // The page shows this rather than deciding for itself:
                    // "identical" and "the same length" are different claims
                    // and the difference is what somebody deletes on.
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

fn api_usage(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
    let request = Request::Usage {
        path: req.param("path").unwrap_or_default().to_owned(),
        top: req.param("top").and_then(|s| s.parse().ok()).unwrap_or(24),
        // Absent and empty mean the same thing here, which is why the page can
        // send the box's contents without looking at them first.
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

    // The desktop's own quick-look, when there is one and it was asked for.
    //
    // A launch like any other on this route, and fenced by the same things —
    // `POST`, the token, the origin, and a path the index holds. What makes it
    // *not* like the others is that it is a viewer rather than a handler: the
    // point of pressing this is to see the file without whatever program owns
    // the extension deciding to open, and that is worth its own verb rather
    // than a heuristic on top of "open".
    if req.param("what") == Some("preview")
        && let Some(cmd) = QUICKLOOK.get().and_then(|q| q.as_ref())
    {
        let mut c = std::process::Command::new(&cmd[0]);
        c.args(&cmd[1..]).arg(p);
        match c
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => http::json(
                stream,
                &serde_json::json!({ "opened": entry.path, "with": cmd[0] }),
            ),
            Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
        }
        return;
    }

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

/// The contents of one file, if it is something a browser can draw.
///
/// **Fenced exactly as `/api/open` is, and for the same reason.** The path is
/// not opened as it arrives: the *service* is asked for it first, and a path
/// the index does not hold is refused — so this cannot be pointed at
/// `/etc/shadow`, at a path a page assembled, or at anything outside the roots
/// the user configured. `stat` already refuses a path no source owns, and that
/// refusal is the whole check. `..` needs no special handling because of it: a
/// traversal that escapes the roots lands somewhere the index has never heard
/// of, and a traversal that does not escape them names a file that was already
/// reachable by its ordinary path.
///
/// What comes back and under what headers is [`preview`]'s business, and the
/// headers are the careful part — this is the one route that answers with
/// bytes this program did not write.
fn api_preview(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
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
        // to read.
        Err(e) => {
            http::fail(stream, "404 Not Found", &e);
            return;
        }
    };

    let p = std::path::Path::new(&entry.path);
    let shape = scour_preview::shape_of(p, entry.is_dir);

    // The probe. A page cannot tell that `notes.bak` is readable and
    // `model.safetensors` is not — that is decided by looking at the bytes,
    // which happens in `scour-preview` — and it must not find out by fetching
    // a four-gigabyte video into memory to read its content type. So it asks
    // first, and then points an `<img>` or a `<video>` at the same URL, which
    // streams and seeks the way the browser wants to.
    if req.param("meta") == Some("1") {
        let (what, kind) = shape.shown();
        http::json(
            stream,
            &serde_json::json!({
                "shape": what,
                "type": kind,
                "size": entry.meta.size,
                // The picture somebody's file manager already made. It is what
                // the panel falls back to for the formats a browser cannot
                // open at all — a `.docx`, a `.psd`, a video in a codec it
                // does not have — and for those it is the only thing there is.
                "thumb": icons::has_thumbnail(&entry.path),
                // Whether there is a system previewer to hand this to, so the
                // page offers the button only where it leads somewhere. On
                // KDE, under Hyprland and on Windows there is nothing to
                // offer, and a button that quietly does nothing is worse than
                // no button.
                "native": QUICKLOOK.get().and_then(|q| q.as_ref()).is_some(),
            }),
        );
        return;
    }

    // **The framing is here and the bytes are not**, which is the split: only
    // this file knows it is speaking HTTP, and `scour-preview` knows nothing
    // about a socket. The three headers below are what keep somebody else's
    // file from becoming this page's script, and they are the reason a preview
    // route is more careful than every other route here — every other one
    // answers with JSON this program wrote.
    let result = match shape {
        scour_preview::Shape::Text => send_text(stream, p),
        scour_preview::Shape::Whole(kind) => send_whole(stream, p, kind),
        scour_preview::Shape::Streamed(kind) => send_stream(stream, p, kind, req.header("range")),
        // 415 rather than 404: the file is there, and this is a statement
        // about what can be shown of it. That lets the panel say "no preview"
        // instead of "not found" — two different things to be told about a
        // file you can see in the list.
        scour_preview::Shape::Nothing => {
            http::fail(stream, "415 Unsupported Media Type", "nothing to show");
            return;
        }
    };
    match result {
        Ok(true) => {}
        // Refused by size, and the number is the answer: a picture has no
        // useful partial rendering, so past the ceiling the panel says how big
        // it is rather than spending fifty megabytes to say the same thing.
        Ok(false) => http::fail(stream, "413 Payload Too Large", "too big to show"),
        // Every path that can fail before a header has been written fails
        // here, so a status line is still the right answer.
        Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
    }
}

/// The headers every preview carries.
///
/// `no-store` for the same reason as everywhere else here — the file is being
/// watched and a cached copy is one that stopped being true. The other two are
/// what keep somebody else's bytes from becoming this page's script:
/// `nosniff`, so a browser does not overrule a `text/plain` it disagrees with,
/// and `sandbox`, which does nothing to an `<img>` or a `<video>` and
/// everything to a top-level navigation. An SVG opened straight at its URL
/// lands in an opaque origin with scripts off, and that is what makes serving
/// SVG as a picture safe rather than merely convenient.
const PREVIEW_HEADERS: &str = "Cache-Control: no-store\r\n\
     X-Content-Type-Options: nosniff\r\n\
     Content-Security-Policy: sandbox; default-src 'none'\r\n\
     Connection: close\r\n";

fn send_text(stream: &mut TcpStream, p: &std::path::Path) -> std::io::Result<bool> {
    let (text, whole, len) = scour_preview::text_head(p)?;
    // The page has to know it is looking at the beginning of something rather
    // than the whole of it, and a header says so without touching the bytes.
    // Appending a note to the body would put it *inside* the file being
    // previewed, where it reads as part of the file.
    let head = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         X-Scour-Complete: {}\r\n\
         X-Scour-Size: {len}\r\n\
         {PREVIEW_HEADERS}\r\n",
        text.len(),
        u64::from(whole),
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(text.as_bytes())?;
    stream.flush()?;
    Ok(true)
}

fn send_whole(stream: &mut TcpStream, p: &std::path::Path, kind: &str) -> std::io::Result<bool> {
    let Some(len) = scour_preview::whole_len(p)? else {
        return Ok(false);
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: {kind}\r\n\
         Content-Length: {len}\r\n\
         {PREVIEW_HEADERS}\r\n"
    );
    stream.write_all(head.as_bytes())?;
    scour_preview::write_span(p, 0, len, stream)?;
    stream.flush()?;
    Ok(true)
}

fn send_stream(
    stream: &mut TcpStream,
    p: &std::path::Path,
    kind: &str,
    range: Option<&str>,
) -> std::io::Result<bool> {
    let len = scour_preview::len_of(p)?;
    let asked = range.and_then(|r| scour_preview::parse_range(r, len));
    let (status, from, count) = match asked {
        Some((from, to)) => ("206 Partial Content", from, to - from + 1),
        None => ("200 OK", 0, len),
    };
    let mut head = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {kind}\r\n\
         Content-Length: {count}\r\n\
         Accept-Ranges: bytes\r\n"
    );
    if asked.is_some() {
        head.push_str(&format!(
            "Content-Range: bytes {from}-{}/{len}\r\n",
            from + count - 1
        ));
    }
    head.push_str(PREVIEW_HEADERS);
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    scour_preview::write_span(p, from, count, stream)?;
    stream.flush()?;
    Ok(true)
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

/// The order a column header asked for.
///
/// **Every key the engine has**, and it did not used to be: six of the
/// fourteen were mapped here, so clicking `Erişim`, `İzinler`, `Sahip`, `Grup`
/// or `Diskte` did nothing at all. The engine could sort by all of them the
/// whole time — the page simply had no name to send, and a header that does
/// nothing when clicked reads as a broken sort rather than as a missing
/// mapping.
///
/// `Relevance` is here for completeness and is what an unrecognised name falls
/// back to nowhere: the default stays `Modified`, because a list nobody has
/// ordered is a list of what changed last.
fn sort_of(s: Option<&str>) -> SortKey {
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
