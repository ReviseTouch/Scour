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
    /// Never ask the desktop to make a thumbnail it has not made yet.
    ///
    /// Pictures already in the cache are still shown — reading them is a
    /// `stat` and this is about the making, which is separate processes doing
    /// image and video decoding on files the page happened to scroll past.
    /// The same reasoning as `--no-preview`: a door widened rather than
    /// opened, and somebody who would rather it stayed shut should be able to
    /// say so. The person using the window has their own switch for it; this
    /// is the one that means the route is not there at all.
    #[arg(long)]
    no_thumbnails: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    // One read for both, and the language half is why it is no longer thrown
    // away: `ui.language` is a machine's answer for a person who has not opened
    // a menu, and reading the file again per request to find it would be a file
    // read on every `/api/kinds`.
    let config = scour_config::Config::load_or_default().0;
    let _ = CONFIGURED_LANGUAGE.set(config.ui.language.clone());
    let addr = args.socket.clone().unwrap_or_else(|| config.socket());

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
            pictures: !args.no_thumbnails,
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
    /// `/api/thumb` at all — ask the desktop to *make* pictures it has not
    /// made. Reading the ones that exist is not behind this.
    pictures: bool,
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
    // `/api/thumb` is on the doing side because it starts programs. It is also
    // the one route here whose GET form would look completely harmless — an
    // `<img src>` that quietly makes a machine decode a video — which is
    // exactly the shape this split exists to stop.
    let acting = req.path == "/api/open"
        || req.path == "/api/thumb"
        || (req.path == "/api/settings" && req.param("set").is_some());
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
        "/api/csv" => api_csv(&mut stream, client, &req),
        "/api/count" => api_count(&mut stream, client, &req),
        "/api/kinds" => api_kinds(&mut stream, client, &req),
        "/api/strings" => api_strings(&mut stream, client, &req),
        "/api/places" => api_places(&mut stream, client),
        "/api/settings" => api_settings(&mut stream, client, &req),
        "/api/facets" => api_facets(&mut stream, client, &req),
        "/api/usage" => api_usage(&mut stream, client, &req),
        "/api/dupes" => api_dupes(&mut stream, client, &req),
        "/api/status" => api_status(&mut stream, client),
        "/api/icon" => api_icon(&mut stream, &req),
        "/api/thumb" if doing.pictures => api_thumb(&mut stream, addr, &req),
        "/api/thumb" => http::fail(
            &mut stream,
            "403 Forbidden",
            "making thumbnails is off (--no-thumbnails)",
        ),
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
                        // What the folder holds, when the index could say. A
                        // folder with no number is a folder whose size is not
                        // known — which is true — where a zero would read as
                        // an empty one.
                        "under": h.under.map(|u| serde_json::json!({
                            "disk": u.disk, "files": u.files
                        })),
                        // Whether a picture of this file already exists, so
                        // the page asks for the ones that do rather than for
                        // two hundred that mostly do not.
                        "thumb": icons::has_thumbnail(&h.path, h.kind),
                        // And whether one *could* be made — the third state a
                        // blank tile was missing. Nothing has ever previewed
                        // this file, but the machine declares a thumbnailer
                        // for its type, so it is worth asking for once it
                        // stops moving. Free: two hash lookups and no syscall,
                        // and it answers no for almost every row.
                        "make": icons::may_thumbnail(&h.path, h.kind),
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

/// The thumbnail somebody has already made for a file.
///
/// **Cacheable, unlike everything else here.** The rest of these routes are
/// about a filesystem being watched, where a cached answer is an answer that
/// stopped being true; a thumbnail is a file in a cache directory, and the one
/// picture on a page that a row genuinely has to fetch.
///
/// It answered for *type* icons too until the page learned to draw those
/// itself — see `icons` for why that was a Linux answer to a question every
/// platform asks. What is left takes a path and nothing else, so the route no
/// longer has a branch where a missing parameter still produces a picture.
fn api_icon(stream: &mut TcpStream, req: &http::Req) {
    // A path is only ever hashed, never opened: what comes back is a file in
    // the thumbnail cache or nothing. See `icons`.
    let picture = match req.param("p") {
        Some(path) if !path.is_empty() => icons::thumbnail(path),
        _ => None,
    };
    match picture {
        Some(p) => http::cached(stream, p.kind, &p.bytes),
        None => http::fail(stream, "404 Not Found", "no thumbnail"),
    }
}

/// Ask the desktop for the pictures it has not made yet.
///
/// **This route carries no bytes.** It says which paths have a picture now,
/// and the page then fetches them from `/api/icon` exactly as it fetches the
/// ones that were already there. That is deliberate: the reading half was made
/// cheap and cacheable and there was no reason to grow a second way to do it.
///
/// **Its own connection to the service, like `/api/wait`.** The shared pool is
/// eight and a batch of thumbnails is seconds of somebody else's video
/// decoding; a search that queued behind one would be the exact failure this
/// whole design is arranged around — nothing on the path a keystroke takes.
/// Opening a socket costs microseconds next to what is about to happen on the
/// other end of it.
///
/// The service is where the bound and the fence are. Nothing here decides how
/// many may run, or whether a path may be touched.
fn api_thumb(stream: &mut TcpStream, addr: &str, req: &http::Req) {
    // **In the body, not the query.** A screenful of paths percent-encoded is
    // several kilobytes and the request line is bounded at sixteen; over that
    // it is cut rather than refused, which surfaces as a 404 for a path
    // nobody asked for. Newline-separated because `\n` is the one byte a
    // filename on any of these platforms cannot hold, and because a body needs
    // no escaping to survive the trip.
    let files: Vec<String> = req
        .body
        .split('\n')
        .map(str::trim_end)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if files.is_empty() {
        http::json(stream, &serde_json::json!({ "ready": [], "ran": 0 }));
        return;
    }
    let mut client = match Client::connect(addr) {
        Ok(c) => c,
        Err(e) => {
            http::fail(stream, "502 Bad Gateway", &e.to_string());
            return;
        }
    };
    match client.call(Request::Thumbnails { files }) {
        Ok(Response::Thumbnails(made)) => http::json(
            stream,
            // `ran` is the number the design has to be judged on — how many
            // processes a screenful of unseen files actually starts. Sent to
            // the page so that the claim can be read out of a running window
            // rather than argued about.
            &serde_json::json!({ "ready": made.ready, "ran": made.ran }),
        ),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e.to_string()),
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
///
/// **`?lang=` rather than one language for the life of the process.** The
/// catalogue used to be built once from the environment, which was right while
/// the language could only be changed by restarting. It can be changed from a
/// menu now, and the labels here are the one part of the rail the page does not
/// hold a msgid for — so a switch that did not reach this route would leave
/// thirteen rows in the old language under a window that had changed.
fn api_kinds(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
    let cat = catalogue_for(client, req);
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

/// The whole catalogue, for the one frontend that cannot link it.
///
/// Every other frontend calls `scour-i18n` directly; a browser cannot, so the
/// words come over the wire once and the page looks them up with the same rule
/// [`Catalog::get`] implements — present means translated, absent means the
/// msgid it already holds is the answer. That is why this hands over a map and
/// not a list of rendered labels: rendering them here would mean this file
/// knowing every string the page shows, which is a second copy of the page's
/// vocabulary and exactly the shape that put thirteen kinds in the engine and
/// eight in the rail.
///
/// The English answer is an empty map, and that is the correct amount rather
/// than a failure: the msgid *is* the English.
///
/// `languages` travels with it so the menu is built from what is shipped. A
/// page with its own list would offer a language nobody wrote a catalogue for.
fn api_strings(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
    let cat = catalogue_for(client, req);
    let strings: serde_json::Map<String, serde_json::Value> = cat
        .entries()
        .map(|(k, v)| (k.to_owned(), serde_json::Value::String(v.to_owned())))
        .collect();
    let languages: Vec<serde_json::Value> = scour_i18n::LANGUAGES
        .iter()
        .map(|(tag, name)| serde_json::json!({ "tag": tag, "name": name }))
        .collect();
    http::json(
        stream,
        &serde_json::json!({
            "lang": cat.locale(),
            "languages": languages,
            "strings": strings,
        }),
    );
}

/// `ui.language` from the config file, read once.
///
/// Once, because the alternative is a file read per request and this is asked
/// on every `/api/kinds`. The service is the thing that would notice a config
/// change, and it does not notice this one either — editing `config.toml` has
/// always meant restarting.
static CONFIGURED_LANGUAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The catalogue this request should answer in.
///
/// `?lang=` when the page names one, because a page that has just been switched
/// must not have to wait for its own `POST` to land before the next route
/// agrees with it — the settings write and the re-fetch are two requests, and
/// between them the service still holds the old answer.
///
/// Otherwise the shared order in [`scour_i18n::choose`]: what was chosen, then
/// the config file, then the environment. Asking the service for the setting
/// costs one round trip on a socket, which is measured in tens of microseconds
/// and happens twice on load.
fn catalogue_for(client: &Mutex<Link>, req: &http::Req) -> scour_i18n::Catalogue {
    if let Some(tag) = req.param("lang").filter(|t| !t.is_empty()) {
        return scour_i18n::Catalogue::for_language(tag);
    }
    let chosen = match call(client, Request::Settings {}) {
        Ok(Response::Settings(s)) => s.language,
        // A service that cannot be asked is not a reason to fall over; the
        // environment is still a usable answer and the page still draws.
        _ => String::new(),
    };
    let configured = CONFIGURED_LANGUAGE.get().map_or("", String::as_str);
    scour_i18n::Catalogue::for_language(&scour_i18n::choose(&chosen, configured))
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
    // What arrives is a **change** — the fields the page set — not the whole
    // object. Everything it does not name belongs to whoever put it there,
    // which on a machine with a terminal interface open is somebody else.
    let request = match req.param("set") {
        Some(text) => match serde_json::from_str(text) {
            Ok(change) => Request::SetSettings { change },
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

/// Where this person keeps things, and what the volumes under them record.
///
/// **Asked, not worked out.** Both halves used to be here: `user-dirs.dirs`
/// parsed in this file, `/proc/self/mounts` read in this file. That is one
/// frontend's copy of a rule four are meant to share — a terminal interface
/// would parse the same file again, and the day one of them got the quoting
/// wrong its sidebar would point at folders that are not there. The same guess
/// was wrong once already at a higher layer: the page shipped with
/// `/home/hasan` written into it, and the fix then moved the guess from
/// JavaScript into this bridge rather than into the service. It is in the
/// service now, in `scour-places`, where every frontend can reach it.
fn api_places(stream: &mut TcpStream, client: &Mutex<Link>) {
    match call(client, Request::Places {}) {
        Ok(Response::Places(p)) => match serde_json::to_value(&p) {
            Ok(v) => http::json(stream, &v),
            Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
        },
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// The whole result set, as a spreadsheet.
///
/// **A pipe now, and it used to be the author.** The URL, the headers and the
/// bytes are unchanged; what left is the loop that made them. This paged the
/// service — `offset`, `offset + 10_000`, and so on — and a page costs what it
/// takes to walk to its offset: 2.1 ms at the start of this index, 25.3 at a
/// hundred thousand, 65.5 at half a million, 117.6 at a million. Linear per
/// page is quadratic in total, so the whole index wrote 1.4 M lines in ten
/// minutes without finishing, and the endpoint stopped at half a million and
/// said so in a trailer. The owner asked for no limit; there is none now,
/// because the service walks the set once and these bytes are what it produced.
///
/// Three things went with the loop, and each is worth naming because each was
/// load-bearing before:
///
/// * **The `CEILING`**, and the `# scour: stopped at N rows` trailer under it.
///   There is nothing left to stop for, so a file that said it had stopped
///   would be a lie. A short file now means a failure, and it carries no
///   trailer either — the connection simply ends, which is what a truncated
///   download looks like to everything that reads one.
/// * **The quoting, the byte-order mark and the date format.** They live in
///   `scour_export`, in the service. Not moved for tidiness: this bridge is
///   one frontend of four, the terminal writes the same file now with `scour
///   export`, and two implementations of RFC 4180 quoting is two chances for a
///   spreadsheet to open and be quietly wrong.
/// * **`sort` and `desc`.** The service streams in the index's own order and
///   cannot be asked for another — see `Request::Export`. The parameters are
///   still accepted and now do nothing, which is said here rather than
///   pretended about; a spreadsheet sorts itself in one click.
///
/// **The set no longer moves while this runs.** Paging a live index meant a
/// row written during the export shifted everything after it, so a file could
/// appear twice or not at all — documented here as inherent, and it was
/// inherent to *paging*. One walk under one read lock is a consistent snapshot.
fn api_csv(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
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

    // **A connection of its own, not one from the pool.** An export holds its
    // connection for as long as it takes to walk the index, and a pooled one
    // would be one the rest of the page cannot use for that whole time — with
    // `POOL` at eight, a handful of concurrent downloads would starve the
    // keystrokes. It is not returned afterwards either: a stream the reader
    // abandons leaves frames in flight, which is the state `Client::stream`
    // refuses to reuse a connection after.
    let addr = match client.lock() {
        Ok(guard) => guard.addr.clone(),
        Err(_) => {
            http::fail(
                stream,
                "500 Internal Server Error",
                "the bridge lost its client",
            );
            return;
        }
    };
    let mut link = match Client::connect(&addr) {
        Ok(c) => c,
        Err(e) => {
            http::fail(stream, "502 Bad Gateway", &e.to_string());
            return;
        }
    };

    // The headers go out before the first row, so there is nowhere left to put
    // a status code if the walk fails after this line. That is the trade the
    // whole endpoint is built on — see `http::attachment` — and it is why the
    // two failures above are checked *before* it.
    http::attachment(stream, "text/csv; charset=utf-8", "scour.csv");
    let result = link.stream(request, |piece| match piece {
        Response::ExportChunk { csv } => stream.write_all(csv.as_bytes()).is_ok(),
        // Not a piece of an export. Ignored rather than taken as the end: a
        // service newer than this bridge may have something to add, and a
        // download that keeps its rows is better than one that stops on a
        // frame it did not recognise.
        _ => true,
    });
    // Nothing to say and nowhere to say it. A reader that went away is the
    // ordinary case — a cancelled download — and an export that failed leaves
    // a short file, which is the only signal HTTP has left once a body has
    // begun.
    let _ = result;
    let _ = stream.flush();
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
    /* **Asked of the entry, not of the disk, and asked once.**
     *
     * This called a second rule that lived in this bridge, which re-`stat`ed
     * the file for a mode bit the `Entry` above already carries, and which
     * decided by that bit alone. Every file on an `ntfs3` volume mounted with
     * `fmask=0022` has it — all of `/mnt/depo` on this machine is `0755` — so
     * double-clicking a PDF there tried to *execute* it and came back as a 500.
     *
     * `scour_core::runs_when_opened` is the rule now, beside `kind_of`, which
     * is where the knowledge about extensions already was. */
    let runnable = !want_folder && scour_core::runs_when_opened(entry.name(), entry.meta.mode);
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
        // **Asked, not decided here.** Whether a file can be shown needs its
        // first eight kilobytes read and a table of extensions consulted, and
        // this bridge was doing both — so a terminal interface or a Slint
        // window would each have had their own idea of what `notes.bak` is.
        // The service answers now; what stays here is the two facts that are
        // about *this* frontend, and the bytes, which a browser wants ranged.
        let look = match call(
            client,
            Request::Preview {
                path: entry.path.clone(),
            },
        ) {
            Ok(Response::Preview(l)) => l,
            Ok(_) => {
                http::fail(stream, "502 Bad Gateway", "unexpected reply");
                return;
            }
            Err(e) => {
                http::fail(stream, "502 Bad Gateway", &e);
                return;
            }
        };
        http::json(
            stream,
            &serde_json::json!({
                "shape": look.shape,
                "type": look.kind,
                "size": entry.meta.size,
                // The picture somebody's file manager already made. It is what
                // the panel falls back to for the formats a browser cannot
                // open at all — a `.docx`, a `.psd`, a video in a codec it
                // does not have — and for those it is the only thing there is.
                "thumb": icons::has_thumbnail(&entry.path, entry.kind()),
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// How long the list is, and whether it says "nothing matched", move together.
    ///
    /// **A structural test, on purpose.** The rule it guards is a rule about
    /// the *page*, and the page is JavaScript compiled in as a string —
    /// nothing in this toolchain runs it and there is no browser in the suite
    /// to run it in. So what can be checked from here is that the shape which
    /// makes the bug impossible is still the shape.
    /// `every_kind_the_engine_names_has_a_glyph_in_the_page` in `icons.rs` is
    /// the same trade for the same reason.
    ///
    /// What it stands in for: `LIST.total` was moved by four things — the
    /// search that lands on a keystroke, a window of rows carrying a total the
    /// count cap did not cut, going offline, and the count refresh. Three of
    /// them also wrote `empty.hidden`; the count refresh did not. Whenever it
    /// got there first — which is what happens while the rows are on a long
    /// leash, and `atMostEvery` gives them `cost x COST` — the branch in
    /// `fillWindow` that hides the message was then skipped for having nothing
    /// left to change, and "Eşleşme yok" stayed on screen over a list with a
    /// row in it. Measured in the running window: the count said `1` at
    /// 13.6 s, the row was drawn at 14.9 s, and the message was still there
    /// thirty seconds later. See docs/MEASUREMENTS.md.
    ///
    /// So: one writer each. A second one is how this happened.
    #[test]
    fn the_length_of_the_list_and_the_empty_message_have_one_writer() {
        for (what, needle) in [
            ("empty.hidden", "empty.hidden ="),
            ("LIST.total", "LIST.total ="),
            ("LIST.exact", "LIST.exact ="),
        ] {
            let n = PAGE.matches(needle).count();
            assert_eq!(
                n, 1,
                "{what} is assigned in {n} places in page.html rather than 1 — \
                 each of them has to move the others, so they belong in `setTotal`"
            );
        }
        // And that the one place is `setTotal`, rather than the three lines
        // having ended up somewhere only half the callers pass through.
        let at = PAGE
            .find("function setTotal(")
            .expect("page.html has no `setTotal`");
        let body = &PAGE[at..];
        let body = &body[..body.find("\n  }").expect("`setTotal` does not end")];
        for needle in [
            "LIST.total =",
            "LIST.exact =",
            "empty.hidden =",
            // The two sweeps decide what the cache may still be trusted for
            // once the length has moved, which is the same fact. A caller that
            // had to remember them separately is the caller that forgot.
            "dropBeyond()",
            "dropShort()",
        ] {
            assert!(body.contains(needle), "`setTotal` does not have {needle}");
        }
    }

    /// **Nor on the catalogue**, which is the same property one layer out.
    ///
    /// The taxonomy and the words now arrive together — `loadLanguage` asks for
    /// both, so that a language switch cannot leave the rail's thirteen labels
    /// a frame behind the headings. That put the kinds request inside a
    /// function, which is what this test used to look for by name; what it is
    /// actually about has not moved. The first page of rows must not wait for
    /// either, and neither may start a second search when it lands.
    #[test]
    fn the_first_search_does_not_wait_for_the_kind_taxonomy() {
        let boot = PAGE
            .find("  q.focus();\n  q.select();")
            .expect("page.html has no list boot sequence");
        let body = &PAGE[boot..];
        let search = body
            .find("\n  render();")
            .expect("the boot sequence does not start a search");
        let words = body
            .find("loadLanguage(\"\")")
            .expect("the boot sequence does not load the language");
        assert!(
            search < words,
            "the first search is still gated on the words and the taxonomy"
        );

        // And what arrives repaints rather than searching again: the rows did
        // not change, only the word for their kind and the format of a number.
        let at = PAGE
            .find("  function applyLanguage() {")
            .expect("the page has no applyLanguage");
        let end = PAGE[at..].find("\n  }").map(|e| at + e).expect("no end");
        let apply = &PAGE[at..end];
        assert!(
            !apply.contains("render()"),
            "changing language starts another index search instead of repainting"
        );
        for needle in [
            "for (const tr of pool) tr.__stamp = \"\";",
            "repaint(true);",
        ] {
            assert!(
                apply.contains(needle),
                "applyLanguage does not redraw the rows: missing {needle}"
            );
        }
    }

    #[test]
    fn sorting_does_not_recount_query_facets() {
        assert!(
            PAGE.contains("const queryChanged = LIST.query !== text;"),
            "render does not distinguish a new query from a new ordering"
        );
        assert!(
            PAGE.contains(
                "if (queryChanged) {\n          sidebarCost = 0;\n          sidebarSoon();"
            ),
            "every ordering change still schedules the facet walk"
        );
        // The same guard the negated form used to spell. It reads the sidebar's
        // own generation and the box, and neither moves for a re-sort; it is
        // written once and shared because there are now up to three answers to
        // admit or drop rather than one, and three copies of a staleness test
        // is how two of them drift.
        assert!(
            PAGE.contains("const current = () => ours === sidebarGeneration && text === q.value;"),
            "facet answers are still invalidated by sort-only generations"
        );
        assert!(
            !PAGE.contains("sidebar(LIST.query, generation)"),
            "a facet request is still coupled to the row ordering generation"
        );
    }

    /// A rail counts what switching to one of its rows would give, so it is
    /// counted with its own term taken out.
    ///
    /// Reported as "selecting one filter zeroes the counts of all the others",
    /// and it did: the service answers a facet with the keys that matched and
    /// no others, so under `kind:image` the kind group came back as one key and
    /// the other twelve rows read `0`. Reproduced in the window — twelve of
    /// thirteen rows at zero — and worse on the age chart, where `dm:27d` left
    /// 21 of 24 bars flat, erasing the control for widening the range.
    #[test]
    fn a_rail_is_not_counted_through_its_own_filter() {
        for needle in [
            "const forKind = without(text, [\"kind\"]);",
            "const forAge = without(text, [\"dm\"]);",
        ] {
            assert!(
                PAGE.contains(needle),
                "the sidebar no longer strips a rail's own term before counting it: {needle}"
            );
        }
        // Only its OWN term. A scope or a size is somebody else's filter and
        // switching kind under it really does give the narrowed count.
        assert!(
            !PAGE.contains("without(text, [\"under\"]")
                && !PAGE.contains("without(text, [\"size\"]"),
            "a rail is stripping a filter that is not its own"
        );
        // **Both groups on every call, and this is not decoration.**
        // `NativeIndex::facets` chooses `AGE_SCAN_CAP` (unbounded) when an age
        // band is asked for and `FACET_SCAN_CAP` (200_000 rows) when it is not,
        // so asking the stripped query for `by=kind` alone moves the rail onto
        // the sampled path. That sample is the first 200,000 rows the walk
        // reaches, not a proportional one: measured on the empty query, images
        // came back 210,551 exact against 1,430 sampled, and video 3.1x
        // overstated. Thirteen plausible wrong numbers is worse than thirteen
        // honest zeros, which is the whole reason this assertion is here.
        assert!(
            PAGE.contains("by: \"kind,age\""),
            "the sidebar asks for one facet group, which puts the rail on the \
             capped scan path and makes its numbers a biased sample"
        );
        // The meter under the list counts what is in the list, so it reads the
        // query in force rather than the broader one a rail was counted with.
        assert!(
            PAGE.contains("const total = inForce.then((res) => {"),
            "the total no longer comes from the query actually in force"
        );
    }

    /// The page knows every kind the engine does, and no kind it does not.
    ///
    /// **The rail above depends on this and nothing said so.** `without` only
    /// strips a `kind:` term it believes is one, and what it believes comes
    /// from the page's own copy of the query grammar — `KIND_VALUES` plus
    /// `KIND_ALIASES`, read by `accepts`. That copy was the mockup's eight
    /// values and had never been told that `Kind` gained Audio, Video, Build,
    /// Data, Config and Font. So for five of the thirteen kinds `without`
    /// found nothing to strip, the rail was counted through its own filter
    /// after all, and twelve of thirteen rows read zero — the exact defect
    /// [`a_rail_is_not_counted_through_its_own_filter`] was written to prevent,
    /// arrived at by a route that test could not see.
    ///
    /// Reproduced in the window on `kind:data`, `kind:config`, `kind:audio`,
    /// `kind:font` and `kind:build`; `kind:image` and `kind:doc` were fine
    /// because they were in the mockup's list, and `kind:video` was fine by
    /// accident because the word is the same in Turkish.
    ///
    /// Both directions, because both are drift. A token the page does not know
    /// is a rail of zeros. A spelling the page accepts and the engine does not
    /// is the opposite mistake: `without` would strip a term the engine reads
    /// as plain text, and the rail would then be counted over a wider set than
    /// the one the box describes.
    #[test]
    fn the_page_takes_the_engines_kind_vocabulary() {
        fn values(name: &str) -> Vec<String> {
            let at = PAGE
                .find(&format!("const {name} = ["))
                .unwrap_or_else(|| panic!("the page has no {name}"));
            let open = PAGE[at..].find('[').expect("no opening bracket") + at;
            let close = PAGE[open..].find(']').expect("no closing bracket") + open;
            PAGE[open + 1..close]
                .split(',')
                .map(|s| s.trim().trim_matches('"').to_string())
                .filter(|s| !s.is_empty())
                .collect()
        }

        let offered = values("KIND_VALUES");
        let aliases = values("KIND_ALIASES");

        // Every kind the rail offers can be typed, because the rail's own
        // buttons put exactly these tokens in the box.
        let engine: Vec<&str> = scour_core::Kind::OFFERED
            .iter()
            .map(|k| k.token())
            .collect();
        assert_eq!(
            offered, engine,
            "the page's kind values are not Kind::OFFERED through Kind::token"
        );

        // And nothing here is a spelling the engine has never heard of.
        for v in offered.iter().chain(aliases.iter()) {
            assert!(
                scour_core::Kind::from_name(v).is_some(),
                "the page accepts kind:{v}, which the engine does not parse"
            );
        }

        // `accepts` reads both lists, or half of this proves nothing.
        assert!(
            PAGE.contains(
                "case \"kind\": return KIND_VALUES.includes(v) || KIND_ALIASES.includes(v);"
            ),
            "accepts() no longer answers for kind: from the two lists this test checks"
        );
    }

    /// Every msgid in the page, exactly as the page writes it.
    ///
    /// Four spellings, because a msgid reaches the catalogue four ways and all
    /// four are literals on purpose:
    ///
    /// * `T("…")` — a lookup in the script.
    /// * `data-t`, `data-t-html`, `data-t-title`, `data-t-aria`, `data-t-ph`
    ///   — a lookup written into the markup, so the element can be empty and
    ///   the key is not duplicated as its own content.
    /// * `msgid: "…"` — a column heading or an age band, resolved by two
    ///   readers each.
    /// * `about: "…"` — a query field's one-line description.
    ///
    /// **Literal-only, and that is the design rather than a limitation of this
    /// function.** `T` is never handed an expression and a msgid is never
    /// built by concatenation, because a msgid a test cannot see is a msgid
    /// that can fall out of the catalogue with nothing failing — which is what
    /// happened to the kind taxonomy twice in one week, in the other
    /// direction.
    fn page_msgids() -> Vec<String> {
        /// The escapes a msgid can carry, and no more. A `\u{...}` in one
        /// would be a msgid nobody could read in the `.po` either.
        fn unescape(s: &str) -> String {
            let mut out = String::with_capacity(s.len());
            let mut chars = s.chars();
            while let Some(c) = chars.next() {
                if c != '\\' {
                    out.push(c);
                    continue;
                }
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some(other) => out.push(other),
                    None => out.push('\\'),
                }
            }
            out
        }

        /// The string literal starting at `from`, up to the first unescaped
        /// closing quote.
        fn literal(rest: &str, quote: char) -> Option<String> {
            let mut out = String::new();
            let mut escaped = false;
            for c in rest.chars() {
                if escaped {
                    out.push(c);
                    escaped = false;
                } else if c == '\\' {
                    out.push(c);
                    escaped = true;
                } else if c == quote {
                    return Some(unescape(&out));
                } else if c == '\n' {
                    return None; // not a literal; a line ended inside it
                } else {
                    out.push(c);
                }
            }
            None
        }

        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut push = |s: String| {
            if !s.is_empty() && seen.insert(s.clone()) {
                out.push(s);
            }
        };

        for (open, quote, entity) in [
            ("T(\"", '"', false),
            ("T('", '\'', false),
            ("msgid: \"", '"', false),
            ("about: \"", '"', false),
            ("data-t=\"", '"', true),
            ("data-t-html=\"", '"', true),
            ("data-t-title=\"", '"', true),
            ("data-t-aria=\"", '"', true),
            ("data-t-ph=\"", '"', true),
        ] {
            let mut at = 0;
            while let Some(found) = PAGE[at..].find(open) {
                let start = at + found + open.len();
                if let Some(text) = literal(&PAGE[start..], quote) {
                    // Attribute values are HTML: the markup a sentence carries
                    // is written `&lt;code&gt;` there and `<code>` in the `.po`.
                    push(if entity {
                        text.replace("&lt;", "<")
                            .replace("&gt;", ">")
                            .replace("&quot;", "\"")
                            .replace("&#39;", "'")
                            .replace("&amp;", "&")
                    } else {
                        text
                    });
                }
                at = start;
            }
        }
        out
    }

    /// **The page says nothing the catalogue has not heard of.**
    ///
    /// The binding this whole change turns on. The words moved out of the page
    /// and into `lang/tr/LC_MESSAGES/scour.po`, and the mechanism that makes
    /// that safe — a missing entry degrades to correct English rather than to
    /// a bare key — is also the mechanism that would let the whole window drift
    /// back into English one string at a time with nothing complaining.
    ///
    /// So the two are pinned together the way
    /// `the_page_takes_the_engines_kind_vocabulary` pins the rail to
    /// `Kind::OFFERED`, and for the same reason: the last two defects in this
    /// file were both a list here disagreeing with a list somewhere else, and
    /// neither was visible from either end.
    #[test]
    fn the_page_says_nothing_the_catalogue_has_not_heard_of() {
        let ids = page_msgids();
        // A floor rather than an exact count, so that adding a string is not a
        // test change — but not zero either, because a regex that silently
        // stopped matching would otherwise pass loudly.
        assert!(
            ids.len() > 140,
            "only {} msgids found; the extraction is broken, not the page",
            ids.len()
        );

        for (tag, _) in scour_i18n::LANGUAGES {
            let cat = scour_i18n::Catalogue::for_language(tag);
            if !cat.is_translated() {
                continue; // English: the msgid is the string.
            }
            let missing: Vec<&String> = ids.iter().filter(|id| !cat.has(id)).collect();
            assert!(
                missing.is_empty(),
                "{tag} has no entry for {} of the window's strings, starting with {:?}",
                missing.len(),
                &missing[..missing.len().min(5)]
            );
        }
    }

    /// **No Turkish left in the page outside the query grammar.**
    ///
    /// The point of the exercise, asserted rather than eyeballed. What may
    /// still carry a Turkish letter, and why:
    ///
    /// * `KIND_ALIASES`, `FIELDS[].alias`, `TIME_RE` and `MISTAKEN` — spellings
    ///   the *engine* parses. `tür:görsel` has to keep finding images in an
    ///   English window, so these are grammar and not vocabulary. They are
    ///   copied from `Kind::from_name` and `fields.rs`, and the test above
    ///   already checks the engine agrees with them.
    /// * `fold`, which collapses the Turkish dotted and dotless i for every
    ///   query in every locale — `DefaultFolder`'s rule, not the window's.
    /// * Comments: characters used as examples of what a byte offset does to
    ///   `İ`, and verbatim quotes of what was reported. A translated quote is
    ///   not a quote.
    ///
    /// Everything else is a string somebody reads, and there are none left.
    #[test]
    fn no_turkish_is_left_where_a_reader_would_see_it() {
        const TURKISH: [char; 12] = ['ç', 'ğ', 'ı', 'ö', 'ş', 'ü', 'Ç', 'Ğ', 'İ', 'Ö', 'Ş', 'Ü'];

        // Comments first: `/* … */`, `// …` and `<!-- … -->` are for whoever
        // maintains this, not for whoever uses it.
        let mut code = String::with_capacity(PAGE.len());
        let mut rest = PAGE;
        loop {
            let next = ["/*", "//", "<!--"]
                .iter()
                .filter_map(|open| rest.find(open).map(|at| (at, *open)))
                .min();
            let Some((at, open)) = next else {
                code.push_str(rest);
                break;
            };
            code.push_str(&rest[..at]);
            let close = match open {
                "/*" => "*/",
                "//" => "\n",
                _ => "-->",
            };
            rest = match rest[at + open.len()..].find(close) {
                Some(end) => &rest[at + open.len() + end + close.len()..],
                None => break,
            };
        }

        // Then the grammar, which is four named places and not a category.
        for (open, close) in [
            ("const KIND_ALIASES = [", "];"),
            ("const TIME_RE =", "\n"),
            ("const MISTAKEN =", "\n"),
            ("const fold =", "\n"),
        ] {
            let at = code
                .find(open)
                .unwrap_or_else(|| panic!("the page no longer has `{open}`"));
            let end = code[at..]
                .find(close)
                .map(|e| at + e + close.len())
                .unwrap_or(code.len());
            code.replace_range(at..end, "");
        }
        // `alias:` lists inside the field table, one line each.
        while let Some(at) = code.find("alias: [") {
            let end = code[at..]
                .find(']')
                .map(|e| at + e + 1)
                .expect("alias list");
            code.replace_range(at..end, "");
        }

        let left: Vec<&str> = code
            .lines()
            .filter(|l| l.contains(TURKISH))
            .map(str::trim)
            .collect();
        assert!(
            left.is_empty(),
            "{} lines of Turkish are still in the page: {left:#?}",
            left.len()
        );
    }

    #[test]
    fn sorting_keeps_the_exact_total_of_the_same_query() {
        let start = PAGE
            .find("function render() {")
            .expect("no render function");
        let end = PAGE[start..]
            .find("\n  /* The service has answered.")
            .map(|at| start + at)
            .expect("render function has no end marker");
        let render = &PAGE[start..end];

        for needle in [
            "const keepExact = !queryChanged && LIST.exact && res.capped;",
            "if (!keepExact) setTotal(res.total, !res.capped);",
        ] {
            assert!(
                render.contains(needle),
                "a capped sort can discard the query's exact total: missing {needle}"
            );
        }
        assert!(
            PAGE.contains("const shown = LIST.exact ? fmt(LIST.total) : fmt(LIST.total) + \"+\";"),
            "the meter still reads the capped sort reply instead of query-scoped state"
        );
    }

    /// **The tile the page counts by is the tile the style sheet draws.**
    ///
    /// The grid's whole arithmetic rests on one number per shape: the
    /// scrollable extent is `lines * pitch` and `pitch` is `TILE[…].h`. The
    /// style sheet has to declare the same height, because `.sizer` is the
    /// only thing in the scroller that reaches that far and the tiles have to
    /// fit under what it claims. Two numbers, in two languages, in one file —
    /// the exact shape of the last two defects here, so they are pinned
    /// together rather than eyeballed.
    #[test]
    fn the_tile_the_page_draws_is_the_tile_it_counts() {
        /// `h: 104` out of the `TILE` table, for one shape.
        fn counted(shape: &str) -> u32 {
            let table = PAGE.find("const TILE = {").expect("no TILE table");
            let end = table + PAGE[table..].find("};").expect("TILE has no end");
            let at = table
                + PAGE[table..end]
                    .find(&format!("{shape}: {{"))
                    .unwrap_or_else(|| panic!("TILE has no {shape}"));
            let h = at + PAGE[at..end].find("h: ").expect("no height") + 3;
            PAGE[h..]
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse().ok())
                .expect("height is not a number")
        }

        /// `--tile-h: 104px` out of the style rule for one shape.
        fn drawn(shape: &str) -> u32 {
            let rule = format!("body.grid[data-view=\"{shape}\"] {{");
            let at = PAGE
                .find(&rule)
                .unwrap_or_else(|| panic!("no style rule for {shape}"));
            let h = at + PAGE[at..].find("--tile-h:").expect("no --tile-h") + 9;
            PAGE[h..]
                .trim_start()
                .split("px")
                .next()
                .and_then(|n| n.trim().parse().ok())
                .expect("--tile-h is not a number of pixels")
        }

        for shape in ["icons", "large"] {
            assert_eq!(
                counted(shape),
                drawn(shape),
                "{shape}: the arithmetic and the style sheet disagree about the tile's height"
            );
        }
    }

    /// **One place writes the scroll position, and it is not the painter.**
    ///
    /// The extent is stated rather than summed precisely so that a paint can
    /// never move the position: an extent that moves makes the browser correct
    /// the position, a correction fires a scroll, a scroll paints — eight
    /// hundred steps in five seconds, recorded in the note above
    /// `paintWindow`. A second shape meant adding the first legitimate write
    /// of `scrollTop` in this file, from a click, and this is what keeps it
    /// the only one.
    #[test]
    fn nothing_on_the_painting_path_moves_the_scroll_position() {
        // The two writes that are allowed are both from something a person
        // did: a new query goes back to the top, and changing shape keeps the
        // item that was at the top. Neither runs from a frame.
        for (open, close) in [
            (
                "function paintWindow() {",
                "\n  /* One row, rewritten in place.",
            ),
            ("function repaint(rowsChanged) {", "\n  const onScroll ="),
            ("function fillWindow() {", "\n  let paintQueued"),
            ("function visibleRange() {", "\n  /* Draw the window"),
        ] {
            let at = PAGE
                .find(open)
                .unwrap_or_else(|| panic!("the page no longer has `{open}`"));
            let end = at
                + PAGE[at..]
                    .find(close)
                    .unwrap_or_else(|| panic!("`{open}` has no end marker"));
            assert!(
                !PAGE[at..end].contains("scrollTop ="),
                "`{open}` writes the scroll position; that is the loop the sizer exists to break"
            );
        }

        // **And the extent is not read back off the element it was written
        // to.** Chromium re-serialises a CSS length to six significant
        // figures, so `1146724px` — a narrow window with large tiles — comes
        // back as `1.14672e+06px`, the guard misses, and the height of the one
        // element that *is* the scrollable extent is rewritten on every frame.
        for asking in [
            "sizer.style.height !==",
            "firstElementChild.style.height !==",
        ] {
            assert!(
                !PAGE.contains(asking),
                "the list asks the DOM what it made of a height it wrote: `{asking}`"
            );
        }

        let mode = PAGE
            .find("function applyMode(next) {")
            .expect("applyMode is absent");
        let end = mode
            + PAGE[mode..]
                .find("\n  viewBox.addEventListener")
                .expect("applyMode has no end marker");
        let body = &PAGE[mode..end];
        let sized = body
            .find("sizer.style.height")
            .expect("applyMode no longer states the extent");
        let moved = body.find("scrollEl.scrollTop =").expect("checked above");
        // Stated before written, or the browser clamps the new position to the
        // extent the shape that is going away needed.
        assert!(
            sized < moved,
            "applyMode writes the scroll position before it states the extent"
        );
    }

    #[test]
    fn every_rendered_row_fact_invalidates_the_live_row_cache() {
        let start = PAGE
            .find("const stamp = f ? [")
            .expect("row stamp is absent");
        let end = PAGE[start..]
            .find("].join(\"\\u0001\")")
            .map(|at| start + at)
            .expect("row stamp has no end");
        let stamp = &PAGE[start..end];

        for field in [
            "f.path",
            "f.name",
            "f.mtime",
            "f.size",
            "f.fresh",
            "f.is_dir",
            "f.thumb",
            "f.ktoken",
            "f.kind",
            "f.ext",
            "f.ctime",
            "f.atime",
            "f.perm",
            "f.user",
            "f.group",
            "f.disk",
            "f.items",
            "under.disk",
            "under.files",
        ] {
            assert!(stamp.contains(field), "row stamp omits {field}");
        }
    }

    #[test]
    fn local_column_changes_do_not_search_again() {
        let heads = PAGE
            .find("function buildHeads() {")
            .expect("column heading builder is absent");
        let heads_end = PAGE[heads..]
            .find("\n  function columnMenu()")
            .map(|at| heads + at)
            .expect("column heading builder has no end marker");
        assert!(
            PAGE[heads..heads_end].contains("tr.__stamp = \"\""),
            "a same-width reorder leaves body cells in their old columns"
        );

        let moved = PAGE
            .find("const endMove = () => {")
            .expect("column move handler is absent");
        let moved_end = PAGE[moved..]
            .find("document.querySelector(\"thead\").addEventListener(\"pointerup\"")
            .map(|at| moved + at)
            .expect("column move handler has no end marker");
        assert!(
            !PAGE[moved..moved_end].contains("render();"),
            "moving a local column searches and discards the row cache"
        );

        let picked = PAGE
            .find("colMenu.addEventListener(\"click\"")
            .expect("column picker handler is absent");
        let picked_end = PAGE[picked..]
            .find("\n  });\n\n  const pathOf")
            .map(|at| picked + at)
            .expect("column picker handler has no end marker");
        assert!(
            !PAGE[picked..picked_end].contains("render();"),
            "showing a local column searches and discards the row cache"
        );
    }

    #[test]
    fn stale_sidebar_work_is_stopped_before_the_service_call() {
        let start = PAGE
            .find("function sidebar(text) {")
            .expect("sidebar function is absent");
        let body = &PAGE[start..];
        let guard = body
            .find("if (text !== q.value) return Promise.resolve();")
            .expect("sidebar has no preflight query guard");
        // Not the argument by name: the query asked for is no longer always
        // the one in force — a rail is counted with its own term stripped —
        // so what matters here is that no call of any shape precedes the guard.
        let call = body
            .find("SERVICE.sidebar(")
            .expect("sidebar service call is absent");
        assert!(
            guard < call,
            "the stale guard runs only after paying for facets"
        );
        assert!(
            PAGE.contains("LIST.query !== null && LIST.query === q.value"),
            "the quiet timer can start work for rows a new query has replaced"
        );
        assert!(
            PAGE.contains("ours === sidebarGeneration && text === q.value"),
            "a stale facet walk can poison the current query's refresh budget"
        );
    }

    #[test]
    fn late_local_metadata_repaints_without_searching() {
        let start = PAGE
            .find("SERVICE.get(\"places\", {})")
            .expect("places request is absent");
        let end = PAGE[start..]
            .find("* The language, and changing it")
            .map(|at| start + at)
            .expect("places request has no end marker");
        let places = &PAGE[start..end];
        for needle in [
            "syncFacets(parse(q.value))",
            "tr.__stamp = \"\"",
            "repaint(true)",
        ] {
            assert!(places.contains(needle), "places does not apply {needle}");
        }
        assert!(
            !places.contains("render();"),
            "local place metadata starts another index search"
        );
    }

    #[test]
    fn the_first_count_does_not_claim_the_index_has_zero_entries() {
        assert!(PAGE.contains("let TOTAL_KNOWN = false;"));
        assert!(
            PAGE.contains("TOTAL_KNOWN ? shown + \" / \" + fmt(TOTAL) : shown"),
            "the first search still prints an unknown denominator as zero"
        );
        assert!(PAGE.contains("TOTAL_KNOWN = true;"));
    }
}
