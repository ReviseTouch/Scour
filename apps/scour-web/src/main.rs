//! Scour in a browser: a bridge from HTTP to the service's Unix socket.
//!
//! Behind this port is an index of every file the user owns: 127.0.0.1 only
//! with no flag to change it, a per-run token without which every route is
//! 403, an `Origin` that must be ours, and `POST` for everything that acts.

mod http;
mod icons;

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;
use scour_core::{Catalog, DirUsage, FacetBy, Owner, Page, SortKey};
use scour_ipc::Client;
use scour_proto::{Request, Response};

/// The page, built in, so that the binary is the whole of the program.
const PAGE: &str = include_str!("page.html");

/// The page with its palette and its menu written in, built once. The colours
/// live in [`scour_ui`], so this page and `theme.slint` cannot drift.
fn page() -> &'static str {
    static PAGE_WITH_THEME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PAGE_WITH_THEME.get_or_init(|| {
        let dark = scour_ui::css_vars(&scour_ui::DARK);
        let light = scour_ui::css_vars(&scour_ui::LIGHT);
        let metrics = scour_ui::css_metrics();
        // The `[data-theme]` rules come last: the menu's override must win.
        let theme = format!(
            ":root {{\n{dark}{metrics}  }}\n\n               @media (prefers-color-scheme: light) {{\n    :root {{\n{light}    }}\n  }}\n               :root[data-theme=\"light\"] {{\n{light}  }}\n               :root[data-theme=\"dark\"] {{\n{dark}  }}\n"
        );
        // `msgid` is the catalogue key the page looks up; `id` and `key` are not.
        let menu = serde_json::Value::Array(
            scour_ui::menu::ITEMS
                .iter()
                // Left out rather than greyed for ever: a browser cannot.
                .filter(|i| !i.except.contains(&scour_ui::faces::Face::Page))
                .map(|i| {
                    serde_json::json!({
                        "id": i.id,
                        "msgid": i.msgid,
                        "key": i.key,
                        "group": i.group,
                        "weight": match i.weight {
                            scour_ui::menu::Weight::Plain => "plain",
                            scour_ui::menu::Weight::Careful => "careful",
                            scour_ui::menu::Weight::Heavy => "heavy",
                        },
                        "when": match i.when {
                            scour_ui::menu::When::File => "file",
                            scour_ui::menu::When::Folder => "folder",
                            scour_ui::menu::When::One => "one",
                            scour_ui::menu::When::Many => "many",
                        },
                    })
                })
                .collect(),
        );
        PAGE.replacen(
            "/* @THEME@ — see `scour-ui`; the bridge writes this block when it serves\n     the page, so that the window and this page cannot drift apart. */",
            &theme,
            1,
        )
        .replacen("/* @MENU@ */ []", &menu.to_string(), 1)
    })
}

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
    /// Open the folder of an executable rather than running it. The default
    /// is to run it: anyone holding the token can start any binary it can see.
    #[arg(long)]
    no_run: bool,
    /// The command that opens the desktop's own quick-look, if the detected
    /// one is wrong or there is none to detect. The file's path is appended.
    #[arg(long, value_name = "CMD")]
    quicklook: Option<String>,
    /// Refuse `/api/preview`, so the page never receives a file's contents:
    /// the panel then shows what the index knows and nothing else.
    #[arg(long)]
    no_preview: bool,
    /// Never ask the desktop to make a thumbnail it has not made yet. Pictures
    /// already in the cache are still shown; this is about the making.
    #[arg(long)]
    no_thumbnails: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    // Read once: per request it would be a file read on every `/api/kinds`.
    let config = scour_config::Config::load_or_default().0;
    let _ = CONFIGURED_LANGUAGE.set(config.ui.language.clone());
    let addr = args.socket.clone().unwrap_or_else(|| config.socket());

    // Fail here rather than in the browser, which has no command to blame.
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
    // Said because having none is the ordinary case and looks like a fault.
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

    // `/api/wait` blocks for half a minute, so it opens its own connection.
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
        std::thread::spawn(move || serve(stream, &client, &addr, &token, doing));
    }
    Ok(())
}

/// A token nobody can guess.
fn token() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut bytes);
    }
    // A pid and a clock: a platform with no `/dev/urandom` still cannot repeat.
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
    /// `/api/thumb` at all. Reading pictures that exist is not behind this.
    pictures: bool,
}

/// The desktop's quick-look command, resolved once: it is a `PATH` walk.
static QUICKLOOK: std::sync::OnceLock<Option<Vec<String>>> = std::sync::OnceLock::new();

fn serve(mut stream: TcpStream, client: &Mutex<Link>, addr: &str, token: &str, doing: Doing) {
    let Some(req) = http::read_request(&stream) else {
        return;
    };

    // A browser cannot forge `Origin`: a foreign one is refused unread.
    if let Some(origin) = req.header("origin")
        && !origin.ends_with(&format!(
            ":{}",
            stream.local_addr().map(|a| a.port()).unwrap_or(0)
        ))
    {
        http::fail(&mut stream, "403 Forbidden", "cross-origin");
        return;
    }
    // Reading is `GET`, doing is `POST`: an `<img src>` must not open a file.
    let acting = req.path == "/api/open"
        || req.path == "/api/trash"
        || req.path == "/api/rename"
        || req.path == "/api/open-with"
        || req.path == "/api/face"
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
    // Constant work whatever the guess, so nothing leaks about how far it got.
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
            page().as_bytes(),
        ),
        "/api/search" => api_search(&mut stream, client, &req),
        "/api/csv" => api_csv(&mut stream, client, &req),
        "/api/count" => api_count(&mut stream, client, &req),
        "/api/kinds" => api_kinds(&mut stream, client, &req),
        "/api/strings" => api_strings(&mut stream, client, &req),
        "/api/places" => api_places(&mut stream, client),
        "/api/rules" => api_rules(&mut stream, client),
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
        // Starting a face is starting a program: behind `--no-launch`.
        "/api/face" if doing.launch => api_face(&mut stream, client, &req),
        "/api/face" => http::fail(&mut stream, "403 Forbidden", "opening is off (--no-launch)"),
        "/api/open" if doing.launch => api_open(&mut stream, client, &req, doing.run),
        "/api/open" => http::fail(&mut stream, "403 Forbidden", "opening is off (--no-launch)"),
        "/api/trash" => api_trash(&mut stream, client, &req),
        "/api/rename" => api_rename(&mut stream, client, &req),
        "/api/openers" => api_openers(&mut stream, client, &req),
        "/api/open-with" if doing.launch => api_open_with(&mut stream, client, &req),
        "/api/open-with" => http::fail(&mut stream, "403 Forbidden", "launching is off"),
        _ => http::fail(&mut stream, "404 Not Found", "no such route"),
    }
}

/// One call to the service, with the lock held only for as long as it takes.
/// Reconnects once: the service is restarted far more often than this is.
fn call(client: &Mutex<Link>, request: Request) -> Result<Response, String> {
    let (mut link, addr) = {
        let mut guard = client.lock().map_err(|_| "the bridge lost its client")?;
        (guard.idle.pop(), guard.addr.clone())
    };

    if let Some(mut open) = link.take() {
        // A failure here is the service restarted underneath: drop, reconnect.
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

/// The connections, and where to open another one. Several, because one behind
/// a mutex queued keystrokes: one request in forty took 1,056 ms, the rest two.
#[derive(Debug)]
struct Link {
    idle: Vec<Client>,
    addr: String,
}

impl Link {
    /// More than the page can have outstanding, and a bound on service threads.
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

/// The thumbnail somebody has already made for a file. Cacheable, unlike every
/// other route: a file in a cache, not an answer about a watched filesystem.
fn api_icon(stream: &mut TcpStream, req: &http::Req) {
    // A path is only ever hashed, never opened. See `icons`.
    let picture = match req.param("p") {
        Some(path) if !path.is_empty() => icons::thumbnail(path),
        _ => None,
    };
    match picture {
        Some(p) => http::cached(stream, p.kind, &p.bytes),
        None => http::fail(stream, "404 Not Found", "no thumbnail"),
    }
}

/// Ask the desktop for the pictures it has not made yet. Carries no bytes: it
/// says which paths have one now, and the page fetches those from `/api/icon`.
fn api_thumb(stream: &mut TcpStream, addr: &str, req: &http::Req) {
    // In the body, not the query: a request line past 16 KiB is cut rather
    // than refused. Newline-separated, the one byte a filename cannot hold.
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
    // Its own connection: a batch is seconds of somebody else's video decoding.
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
            // `ran` is how many processes a screenful of unseen files started.
            &serde_json::json!({ "ready": made.ready, "ran": made.ran }),
        ),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e.to_string()),
    }
}

/// The kinds a frontend should offer, in order, in the user's language.
/// `?lang=`: these labels are the one part of the rail with no msgid in the page.
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

/// The whole catalogue, for the one frontend that cannot link `scour-i18n`. A
/// map, by [`Catalog::get`]'s rule: absent means the msgid is the answer.
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

/// `ui.language` from the config file, read once: editing `config.toml` has
/// always meant restarting.
static CONFIGURED_LANGUAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The catalogue this request should answer in: `?lang=` when the page names
/// one — the settings write and the re-fetch are two requests — else
/// [`scour_i18n::choose`]: what was chosen, the config file, the environment.
fn catalogue_for(client: &Mutex<Link>, req: &http::Req) -> scour_i18n::Catalogue {
    if let Some(tag) = req.param("lang").filter(|t| !t.is_empty()) {
        return scour_i18n::Catalogue::for_language(tag);
    }
    let chosen = match call(client, Request::Settings {}) {
        Ok(Response::Settings(s)) => s.language,
        // A service that cannot be asked still leaves the environment.
        _ => String::new(),
    };
    let configured = CONFIGURED_LANGUAGE.get().map_or("", String::as_str);
    scour_i18n::Catalogue::for_language(&scour_i18n::choose(&chosen, configured))
}

/// What this person's frontends remember. `POST` only when it is setting, and
/// what it writes is a column list, not a file. See `scour-settings`.
fn api_settings(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
    // A change, not the whole object: what it omits belongs to another frontend.
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

/// What the walk skips, in two lists: built-in and configured stay apart all
/// the way to the browser, because only the second can be edited.
fn api_rules(stream: &mut TcpStream, client: &Mutex<Link>) {
    match call(client, Request::Rules {}) {
        Ok(Response::Rules {
            builtin_paths,
            builtin_dirs,
            builtin_files,
            config_paths,
            config_dirs,
            config_files,
            config_allow,
            added_paths,
            added_dirs,
            added_files,
            added_allow,
            off,
        }) => http::json(
            stream,
            &serde_json::json!({
                "builtin": { "paths": builtin_paths, "dirs": builtin_dirs, "files": builtin_files },
                "config": { "paths": config_paths, "dirs": config_dirs,
                            "files": config_files, "allow": config_allow },
                "added": { "paths": added_paths, "dirs": added_dirs,
                           "files": added_files, "allow": added_allow },
                // Ids across all three groups: an off rule stays where written.
                "off": off,
            }),
        ),
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}

/// Where this person keeps things, so the page does not have to guess. The XDG
/// user directories, from `scour-places`; only the ones that exist are offered.
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

/// The whole result set, as a spreadsheet: one walk in the service under one
/// read lock, so there is no ceiling, no trailer and no row seen twice. `sort`
/// and `desc` are accepted and do nothing; the quoting is `scour_export`'s.
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

    // Its own connection, held for the whole walk and not returned: an
    // abandoned stream leaves frames in flight.
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
fn api_count(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
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
fn api_usage(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
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

/// Everything the sidebar needs, from one walk: `by=kind,age` asks both, and a
/// group per question comes back in the order asked, with the exact total.
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

/// The same answer either route produces, waited for or asked outright.
fn status_json(s: &scour_core::Status) -> serde_json::Value {
    serde_json::json!({
        "entries": s.entries,
        "scanning": s.scanning,
        "watching": s.watching,
        "sources": s.sources,
        "pending": s.pending,
        "index_bytes": s.index_bytes,
        "revision": s.revision,
        // Every query reads the unsorted tail: a week of ordinary use makes
        // ordering by path 1.9 ms against 21.5, and one rebuild puts it back.
        "unsorted": s.unsorted,
        "rebuild_advised": s.rebuild_advised,
    })
}

/// Hold the request open until the index changes. Its own connection: half a
/// minute on the shared one is half a minute of keystrokes. A timeout is not
/// an error, and a status comes back either way.
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

/// What the query means and what could follow the caret. Tokenised in the
/// service: a second parser here would colour a lie the day the two disagreed.
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

/// Start the window or the terminal, once a person has asked for it. Two
/// names, not a path: a program name from the page would run anything.
fn api_face(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
    let which = req.param("face").unwrap_or_default();
    // The terminal needs a tty; which one to open is `scripts/scour-open`'s list.
    let program = match which {
        "window" => "scour-gui",
        "tui" => "scour-open",
        _ => {
            http::fail(stream, "400 Bad Request", "no such face");
            return;
        }
    };
    let Some(binary) = beside_or_path(program) else {
        http::fail(
            stream,
            "404 Not Found",
            &format!("{program} is not installed"),
        );
        return;
    };
    let mut command = std::process::Command::new(&binary);
    command.args(if which == "tui" {
        &["tui"][..]
    } else {
        &[][..]
    });
    // Its own process group, or closing this server closes what it opened.
    scour_ui::faces::detach(&mut command);
    match command.spawn() {
        Ok(_) => {
            // Switching is also choosing: the desktop entry opens the last
            // face moved to. Best effort — the face is already up.
            if let Ok(change) = serde_json::from_value(serde_json::json!({ "face": which })) {
                let _ = call(client, Request::SetSettings { change });
            }
            http::respond(
                stream,
                "200 OK",
                "application/json",
                b"{\"started\":true,\"closing\":true}",
            );
            // And this face closes: switching is moving, not opening a second
            // one. A tab cannot close itself, so the server goes and the page
            // says the tab can be shut — after a beat, because a process that
            // exits while its child is still starting takes the child with it.
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(700));
                std::process::exit(0);
            });
        }
        Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
    }
}

/// The first `name` beside this program, then on the `PATH`.
fn beside_or_path(name: &str) -> Option<std::path::PathBuf> {
    if let Ok(here) = std::env::current_exe()
        && let Some(dir) = here.parent()
    {
        let beside = dir.join(name);
        if beside.is_file() {
            return Some(beside);
        }
        let script = dir.join("../../scripts").join(name);
        if script.is_file() {
            return Some(script);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Which programs on this machine will take this file. A read, so a `GET`; the
/// desktop's own association is marked rather than moved.
fn api_openers(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
    let Some(path) = req.param("path").filter(|p| !p.is_empty()) else {
        http::fail(stream, "400 Bad Request", "no path");
        return;
    };
    if !matches!(
        call(
            client,
            Request::Stat {
                path: path.to_string()
            }
        ),
        Ok(Response::Stat(_))
    ) {
        http::fail(stream, "404 Not Found", "not in the index");
        return;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let mime = scour_thumbs::known::known().mime_of(name).unwrap_or("");
    let list: Vec<serde_json::Value> = scour_openers::openers(mime)
        .into_iter()
        .map(|o| serde_json::json!({ "id": o.id, "name": o.name, "preferred": o.preferred }))
        .collect();
    http::json(stream, &serde_json::json!({ "openers": list }));
}

/// Start one of them, behind `--no-launch` with everything else that runs a
/// program. The list above is a read and stays available.
fn api_open_with(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
    let (Some(path), Some(id)) = (
        req.param("path").filter(|p| !p.is_empty()),
        req.param("id").filter(|p| !p.is_empty()),
    ) else {
        http::fail(stream, "400 Bad Request", "no path or no id");
        return;
    };
    if !matches!(
        call(
            client,
            Request::Stat {
                path: path.to_string()
            }
        ),
        Ok(Response::Stat(_))
    ) {
        http::fail(stream, "404 Not Found", "not in the index");
        return;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let mime = scour_thumbs::known::known().mime_of(name).unwrap_or("");
    match scour_openers::openers(mime)
        .into_iter()
        .find(|o| o.id == id)
    {
        Some(chosen) => match scour_openers::launch(&chosen, std::path::Path::new(path)) {
            Ok(()) => http::json(stream, &serde_json::json!({ "started": chosen.name })),
            Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
        },
        // The list the page was shown is a moment old.
        None => http::fail(stream, "404 Not Found", "no such program any more"),
    }
}

/// Give a file a different name, and tell the index straight away. Renameable
/// only if the index holds the path; what a name may be is `scour-name`'s.
fn api_rename(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
    let (Some(path), Some(name)) = (
        req.param("path").filter(|p| !p.is_empty()),
        req.param("name"),
    ) else {
        http::fail(stream, "400 Bad Request", "no path or no name");
        return;
    };
    if !matches!(
        call(
            client,
            Request::Stat {
                path: path.to_owned()
            }
        ),
        Ok(Response::Stat(_))
    ) {
        http::fail(stream, "404 Not Found", "not in the index");
        return;
    }
    match scour_name::rename(std::path::Path::new(&path), name) {
        Ok(now) => {
            let now = now.to_string_lossy().into_owned();
            // Both ends: the old path is gone and the new one has appeared.
            let _ = call(
                client,
                Request::Recheck {
                    paths: vec![path.to_owned(), now.clone()],
                },
            );
            http::json(stream, &serde_json::json!({ "path": now }));
        }
        // The refusal is a catalogue key, looked up in the reader's language.
        Err(why) => http::json(stream, &serde_json::json!({ "refused": why.msgid() })),
    }
}

/// Send rows to the wastebasket, and tell the index straight away. Trashable
/// only if the index holds the path; the move is this process's, with this
/// user's permissions, and [`Request::Recheck`] only re-reads.
fn api_trash(stream: &mut TcpStream, client: &Mutex<Link>, req: &http::Req) {
    let Some(raw) = req.param("paths").filter(|p| !p.is_empty()) else {
        http::fail(stream, "400 Bad Request", "no paths");
        return;
    };
    let asked: Vec<String> = raw
        .split('\n')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();

    let mut gone = Vec::new();
    let mut refused = Vec::new();
    for path in &asked {
        // In the index, or not ours.
        if !matches!(
            call(client, Request::Stat { path: path.clone() }),
            Ok(Response::Stat(_))
        ) {
            refused.push(format!("{path}: not in the index"));
            continue;
        }
        match scour_trash::trash(std::path::Path::new(path)) {
            Ok(_) => gone.push(path.clone()),
            Err(e) => refused.push(format!("{path}: {e}")),
        }
    }

    // A failure is worth rechecking too: sometimes it is already gone.
    if !asked.is_empty() {
        let _ = call(client, Request::Recheck { paths: asked });
    }

    http::json(
        stream,
        &serde_json::json!({ "gone": gone.len(), "refused": refused }),
    );
}

/// Open a path, or the folder holding it. The index is the fence: the service
/// is asked first, so this cannot be pointed at `/etc/shadow`. An executable
/// is run unless `--no-run`.
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
        // Not in the index, or under no configured root: not ours to open.
        Err(e) => {
            http::fail(stream, "404 Not Found", &e);
            return;
        }
    };

    let p = std::path::Path::new(&entry.path);

    // The desktop's own quick-look: a viewer rather than the handler the
    // extension names, which is why it is its own verb rather than a heuristic.
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
    // Asked of the entry, not the disk: every file on an `ntfs3` mount with
    // `fmask=0022` is 0755, so the mode bit alone would try to run a PDF.
    let runnable = !want_folder && scour_core::runs_when_opened(entry.name(), entry.meta.mode);
    let run = runnable && may_run;
    let target = if want_folder || (runnable && !run) {
        p.parent().unwrap_or(p).to_path_buf()
    } else {
        p.to_path_buf()
    };

    // Started in the directory it lives in, beside what it reads.
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
                // Said, because nothing else on screen will say it was run.
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

/// The contents of one file, if it is something a browser can draw. Fenced as
/// `/api/open` is, so `..` needs no handling: a traversal that escapes the
/// roots lands somewhere the index has never heard of.
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
        // Not in the index, or under no configured root: not ours to read.
        Err(e) => {
            http::fail(stream, "404 Not Found", &e);
            return;
        }
    };

    let p = std::path::Path::new(&entry.path);
    let shape = scour_preview::shape_of(p, entry.is_dir);

    // The probe: a page must not learn a file is unshowable by fetching four
    // gigabytes of it. It asks, then points an `<img>` at the same URL.
    if req.param("meta") == Some("1") {
        // Asked, not decided here: one answer for all four frontends.
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
                // The fallback for formats a browser cannot open at all.
                "thumb": icons::has_thumbnail(&entry.path, entry.kind()),
                // Whether there is a previewer, so the button leads somewhere.
                "native": QUICKLOOK.get().and_then(|q| q.as_ref()).is_some(),
            }),
        );
        return;
    }

    // The framing is here and the bytes are not: the one route answering with
    // bytes this program did not write.
    let result = match shape {
        scour_preview::Shape::Text => send_text(stream, p),
        scour_preview::Shape::Whole(kind) => send_whole(stream, p, kind),
        scour_preview::Shape::Streamed(kind) => send_stream(stream, p, kind, req.header("range")),
        // 415 rather than 404: the file is there, and cannot be shown.
        scour_preview::Shape::Nothing => {
            http::fail(stream, "415 Unsupported Media Type", "nothing to show");
            return;
        }
    };
    match result {
        Ok(true) => {}
        // Refused by size: a picture has no useful partial rendering.
        Ok(false) => http::fail(stream, "413 Payload Too Large", "too big to show"),
        // No header has been written yet, so a status line still answers.
        Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
    }
}

/// The headers every preview carries: `nosniff` so a browser cannot overrule a
/// `text/plain`, and `sandbox` so an SVG opened at its own URL lands in an
/// opaque origin with scripts off.
const PREVIEW_HEADERS: &str = "Cache-Control: no-store\r\n\
     X-Content-Type-Options: nosniff\r\n\
     Content-Security-Policy: sandbox; default-src 'none'\r\n\
     Connection: close\r\n";

fn send_text(stream: &mut TcpStream, p: &std::path::Path) -> std::io::Result<bool> {
    let (text, whole, len) = scour_preview::text_head(p)?;
    // A header, not a note in the body, which would read as part of the file.
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

/// The name behind a numeric id, from this machine.
fn owner_name(which: Owner, id: i64) -> String {
    scour_core::owner_name(which, id)
}

/// The order a column header asked for; every key the engine has. An
/// unrecognised name is `Modified`, which is what changed last.
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
        // A sort key nobody recognises shows the list, not a 400.
        assert_eq!(sort_of(Some("zurna")), SortKey::Modified);
        assert_eq!(sort_of(None), SortKey::Modified);
        assert_eq!(sort_of(Some("size")), SortKey::Size);
    }

    /// Structural: the page is a compiled-in string that nothing here runs, so
    /// what is checkable is that each field has one writer, and it is `setTotal`.
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
        // And that the one place is `setTotal`.
        let at = PAGE
            .find("function setTotal(")
            .expect("page.html has no `setTotal`");
        let body = &PAGE[at..];
        let body = &body[..body.find("\n  }").expect("`setTotal` does not end")];
        for needle in [
            "LIST.total =",
            "LIST.exact =",
            "empty.hidden =",
            // The two sweeps say what the cache is good for at the new length.
            "dropBeyond()",
            "dropShort()",
        ] {
            assert!(body.contains(needle), "`setTotal` does not have {needle}");
        }
    }

    /// A class with no rule is not an error in CSS but a run in the layer's own
    /// colour. `text` and `space` have none on purpose: `text` is that colour.
    #[test]
    fn every_role_the_engine_can_send_has_a_colour() {
        for role in scour_core::Role::ALL {
            // From serde: a second spelling of the same words is how they part.
            let wire = serde_json::to_string(&role).expect("a role serialises");
            let wire = wire.trim_matches('"');
            if matches!(role, scour_core::Role::Text | scour_core::Role::Space) {
                continue;
            }
            // A whole selector: `.r-not` must not answer for `.r-not_a_role`.
            let named = PAGE.match_indices(&format!(".r-{wire}")).any(|(at, m)| {
                PAGE[at + m.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '-')
            });
            assert!(
                named,
                "page.html has no `.qshadow .r-{wire}` rule, so {role:?} is \
                 drawn in whatever colour the layer happens to be"
            );
        }
    }

    /// The rule is `Role::is_telling`; the page keeps its own set of strings,
    /// and this is what stops the two drifting.
    #[test]
    fn the_page_and_the_engine_agree_on_which_roles_are_telling() {
        let at = PAGE
            .find("const TELLING = new Set([")
            .expect("page.html has no TELLING set");
        let body = &PAGE[at..];
        let body = &body[..body.find("]);").expect("TELLING does not end")];
        for role in scour_core::Role::ALL {
            let wire = serde_json::to_string(&role).expect("a role serialises");
            let wire = wire.trim_matches('"');
            assert_eq!(
                body.contains(&format!("\"{wire}\"")),
                role.is_telling(),
                "the page and `Role::is_telling` disagree about {role:?}"
            );
        }
    }

    /// And on the words that look like operators and are not.
    #[test]
    fn the_page_and_the_shared_list_agree_on_the_mistaken_words() {
        let at = PAGE
            .find("const MISTAKEN = new Set([")
            .expect("page.html has no MISTAKEN set");
        let body = &PAGE[at..];
        let body = &body[..body.find("]);").expect("MISTAKEN does not end")];
        for word in scour_ui::MISTAKEN {
            assert!(
                body.contains(&format!("\"{word}\"")),
                "page.html's MISTAKEN has no {word:?}"
            );
        }
        assert_eq!(
            body.matches('"').count() / 2,
            scour_ui::MISTAKEN.len(),
            "page.html's MISTAKEN has words `scour_ui::MISTAKEN` does not"
        );
    }

    /// A renamed marker makes the injection silently do nothing, and every
    /// right-click then draws an empty box.
    #[test]
    fn the_page_is_served_with_the_menu_the_shared_crate_holds() {
        let served = page();
        assert!(
            !served.contains("/* @MENU@ */"),
            "the marker is still in the served page, so nothing replaced it"
        );
        for item in scour_ui::menu::ITEMS {
            if item.except.contains(&scour_ui::faces::Face::Page) {
                assert!(
                    !served.contains(&format!("\"id\":\"{}\"", item.id))
                        || scour_ui::menu::ITEMS
                            .iter()
                            .any(|o| o.id == item.id
                                && !o.except.contains(&scour_ui::faces::Face::Page)),
                    "{} cannot be done in a browser and was served anyway",
                    item.id
                );
                continue;
            }
            assert!(
                served.contains(&format!(
                    "\"msgid\":\"{}\"",
                    item.msgid.replace('"', "\\\"")
                )),
                "the page was served without {:?}",
                item.msgid
            );
        }
    }

    /// The page is a single `<script>`, so one `SyntaxError` anywhere means
    /// none of it runs. Skipped where there is no `node`, and it says so.
    #[test]
    fn the_page_script_parses() {
        let script = PAGE
            .split_once("<script")
            .and_then(|(_, rest)| rest.split_once('>'))
            .and_then(|(_, body)| body.split_once("</script>"))
            .map(|(body, _)| body)
            .expect("page.html has no script");
        let path = std::env::temp_dir().join("scour-page-check.js");
        std::fs::write(&path, script).expect("writing the script out");
        match std::process::Command::new("node")
            .arg("--check")
            .arg(&path)
            .output()
        {
            Ok(out) => assert!(
                out.status.success(),
                "page.html's script does not parse, so none of it would run:\n{}",
                String::from_utf8_lossy(&out.stderr)
            ),
            Err(e) => eprintln!("the_page_script_parses: skipped, no node here ({e})"),
        }
        let _ = std::fs::remove_file(&path);
    }

    /// That the marker was replaced — a page still carrying `@THEME@` has no
    /// colours at all — and that a variable it uses holds the crate's value.
    #[test]
    fn the_page_takes_its_palette_from_the_shared_crate() {
        let served = super::page();
        assert!(
            !served.contains("@THEME@"),
            "the marker survived: the page is served with no palette"
        );
        for (name, colour) in [
            ("--ground", scour_ui::DARK.ground),
            ("--q-key", scour_ui::DARK.q_key),
            ("--mark", scour_ui::DARK.mark),
            ("--focus", scour_ui::DARK.focus),
        ] {
            let want = format!("{name}: {};", colour.css());
            assert!(
                served.contains(&want),
                "the served page does not carry `{want}`"
            );
        }
        // And that the route calls it, which is the only check here that
        // fails when the wiring is undone rather than the formatting. Spelled
        // in two pieces: this reads the file it is written in.
        let me = include_str!("main.rs");
        let call = concat!("page()", ".as_bytes()");
        assert!(
            me.contains(call),
            "the `/` route no longer serves `page()`, so the palette is not injected"
        );

        // Both blocks that override the media query, or the switch half works.
        let light = scour_ui::LIGHT.panel.css();
        assert_eq!(
            served.matches(&format!("--panel: {light};")).count(),
            2,
            "the light panel colour should appear in both the media query and \
             the `[data-theme=\"light\"]` block"
        );
    }

    /// Shared is the list, not the painting: which columns exist, their labels,
    /// their sort keys and their widths.
    #[test]
    fn the_page_shows_the_columns_the_shared_crate_names() {
        for c in scour_ui::COLUMNS {
            let decl = format!("id: \"{}\",", c.id);
            let at = PAGE
                .find(&decl)
                .unwrap_or_else(|| panic!("the page has no `{}` column", c.id));
            // The declaration is one line: id, msgid, sort key, width.
            let line = &PAGE[at..PAGE[at..].find('\n').map(|n| at + n).unwrap_or(PAGE.len())];
            assert!(
                line.contains(&format!("msgid: \"{}\"", c.msgid)),
                "`{}` is called something else here: {line}",
                c.id
            );
            assert!(
                line.contains(&format!("key: \"{}\"", c.sort)),
                "`{}` sorts by something else here: {line}",
                c.id
            );
            assert!(
                line.contains(&format!("w: {},", c.width)),
                "`{}` starts at a different width here: {line}",
                c.id
            );
            // A floor a pixel out is a column that vanishes in one face only.
            for (word, want) in [
                ("min", c.min),
                ("near", u32::from(c.near)),
                ("far", u32::from(c.far)),
                ("max", c.max),
            ] {
                assert!(
                    line.contains(&format!("{word}: {want},")),
                    "`{}`: {word} is not {want} here: {line}",
                    c.id
                );
            }
        }
    }

    /// The numbers being equal is checked above; this runs the page's own
    /// arithmetic under `node` and compares it. No node, no check.
    #[test]
    fn the_page_shares_the_row_out_exactly_as_the_shared_crate_does() {
        let from = PAGE
            .find("  const NARROW = 700, WIDE = 1900;")
            .expect("the page has no NARROW/WIDE");
        // Written out in both places: check they agree before trusting either.
        assert_eq!(
            (scour_ui::NARROW, scour_ui::WIDE),
            (700, 1900),
            "the anchors moved in the crate and not in the page"
        );
        let to = PAGE
            .find("  function applyWidths() {")
            .expect("no applyWidths");
        assert!(to > from, "layOut is not above applyWidths any more");

        let cols: Vec<String> = scour_ui::DEFAULT_COLUMNS
            .iter()
            .filter_map(|id| scour_ui::column(id))
            .map(|c| {
                format!(
                    r#"{{"id":"{}","w":{},"min":{},"near":{},"far":{},"max":{}}}"#,
                    c.id, c.width, c.min, c.near, c.far, c.max
                )
            })
            .collect();
        let harness = format!(
            "const WIDTHS = {{\"name\": 359}};\n{}\nconst cols = [{}];\nconst out = [];\n             for (let r = 200; r <= 3600; r += 7) out.push(layOut(cols, r));\n             console.log(JSON.stringify(out));\n",
            &PAGE[from..to],
            cols.join(",")
        );
        let path = std::env::temp_dir().join("scour-layout-check.js");
        std::fs::write(&path, &harness).expect("writing the harness out");
        let out = match std::process::Command::new("node").arg(&path).output() {
            Ok(out) => out,
            Err(e) => {
                eprintln!("the_page_shares_the_row_out…: skipped, no node here ({e})");
                let _ = std::fs::remove_file(&path);
                return;
            }
        };
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "the page's layOut did not run:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        // Parsed by hand: a list of lists of integers needs no dependency.
        let rows: Vec<Vec<u32>> = text
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split("],[")
            .map(|row| {
                row.trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .map(|n| n.trim().parse().expect("a width"))
                    .collect()
            })
            .collect();

        let mut n = 0;
        for (i, room) in (200..=3600).step_by(7).enumerate() {
            let want = scour_ui::lay_out(
                scour_ui::DEFAULT_COLUMNS,
                |id| (id == "name").then_some(359),
                room,
            );
            assert_eq!(rows[i], want, "at {room}px the page and the crate differ");
            n += 1;
        }
        assert!(n > 400, "only {n} widths were compared");
    }

    /// Neither the words nor the taxonomy starts a second search when it lands.
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

        // What arrives repaints rather than searching: the rows did not change.
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
        // One staleness test: the sidebar's generation and the box, neither of
        // which moves for a re-sort.
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
    /// counted with its own term taken out: a facet answers with the keys that
    /// matched and no others, so otherwise every row but one reads zero.
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
        // Only its own term: kind under a scope does give the narrowed count.
        assert!(
            !PAGE.contains("without(text, [\"under\"]")
                && !PAGE.contains("without(text, [\"size\"]"),
            "a rail is stripping a filter that is not its own"
        );
        // Both groups on every call: `by=kind` alone puts the walk on
        // `FACET_SCAN_CAP` (200,000 rows), and that sample is the first rows
        // reached, not a proportional one — images 1,430 against 210,551.
        assert!(
            PAGE.contains("by: \"kind,age\""),
            "the sidebar asks for one facet group, which puts the rail on the \
             capped scan path and makes its numbers a biased sample"
        );
        // The meter reads the query in force, not a rail's broader one.
        assert!(
            PAGE.contains("const total = inForce.then((res) => {"),
            "the total no longer comes from the query actually in force"
        );
    }

    /// The rail above depends on it: `without` strips a `kind:` term only if
    /// the page's copy of the grammar knows the value. Both directions are
    /// drift — an unknown token is a rail of zeros, an extra one strips text.
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

        let engine: Vec<&str> = scour_core::Kind::OFFERED
            .iter()
            .map(|k| k.token())
            .collect();
        assert_eq!(
            offered, engine,
            "the page's kind values are not Kind::OFFERED through Kind::token"
        );

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

    /// Every msgid in the page, exactly as the page writes it: `T("…")`, the
    /// `data-t*` attributes, `msgid:` and `about:`. Literals only, by design —
    /// a msgid built by concatenation is one no test can see.
    fn page_msgids() -> Vec<String> {
        /// The escapes a msgid can carry, and no more.
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

        /// The string literal in `rest`, up to the first unescaped `quote`.
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
                    // Attribute values are HTML: `&lt;code&gt;` there is
                    // `<code>` in the `.po`.
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

    /// A missing entry degrades to English rather than to a bare key, so a
    /// window could drift back into English with nothing else complaining.
    #[test]
    fn the_page_says_nothing_the_catalogue_has_not_heard_of() {
        let ids = page_msgids();
        // A floor rather than a count: adding a string is not a test change,
        // and an extraction that stopped matching still fails.
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

    /// No Turkish left in the page outside the query grammar. Exempt: comments,
    /// and the spellings the engine parses, which a query still needs.
    #[test]
    fn no_turkish_is_left_where_a_reader_would_see_it() {
        const TURKISH: [char; 12] = ['ç', 'ğ', 'ı', 'ö', 'ş', 'ü', 'Ç', 'Ğ', 'İ', 'Ö', 'Ş', 'Ü'];

        // Comments first: they are for whoever maintains this.
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
            .find("\n  function draw(text, p, hits, res) {")
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

    /// The scrollable extent is `lines * pitch` and `pitch` is `TILE[…].h`, so
    /// the style sheet has to declare the same height.
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

    /// The extent is stated rather than summed so that a paint cannot move the
    /// position: a moved extent is corrected, a correction scrolls, that paints.
    #[test]
    fn nothing_on_the_painting_path_moves_the_scroll_position() {
        // The two allowed writes are both from something a person did, and
        // neither runs from a frame.
        for (open, close) in [
            (
                "function paintWindow() {",
                "\n  function writeRow(tr, f, cols, parsed, marks) {",
            ),
            ("function repaint(rowsChanged) {", "\n  const onScroll ="),
            ("function fillWindow() {", "\n  let paintQueued"),
            ("function visibleRange() {", "\n  let pool = [];"),
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

        // And the extent is never read back off the element: Chromium
        // re-serialises `1146724px` as `1.14672e+06px`, so the guard misses
        // and the height is rewritten on every frame.
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
        // Stated before written, or the browser clamps the new position to
        // the outgoing shape's extent.
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
        // Not the argument by name: a rail is counted with its own term
        // stripped, so what matters is that no call precedes the guard.
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
            .find("\n  function applyLanguage() {")
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
