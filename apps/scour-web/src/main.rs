//! Scour in a browser: a bridge from HTTP to the service's Unix socket.
//!
//! Behind this port is an index of every file the user owns: 127.0.0.1 only
//! with no flag to change it, a per-run token without which every route is
//! 403, an `Origin` and a `Host` that must be ours, a peer that must be this
//! account's process, and `POST` for everything that acts.

mod disks;
mod dupes;
mod grep;
mod hotkey;
mod http;
mod icons;
mod peer;

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;
use scour_client::Pool;
use scour_core::{Catalog, DirUsage, FacetBy, Owner, Page, SortKey};
use scour_ipc::Client;
use scour_proto::{Request, Response};

/// The page, built in, so that the binary is the whole of the program.
const PAGE: &str = include_str!("page.html");

/// The application's icon, the one the desktop shows. Written into the page as
/// a data URL: every other path asks for the token, and a tab icon cannot.
const LOGO: &str = include_str!("../../../assets/scour.svg");

fn data_url(svg: &str) -> String {
    let mut out = String::from("data:image/svg+xml,");
    for b in svg.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/:=;,!*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// How a disk is spoken of, by wire name: `scour_ui::disks`, so every face
/// says the same. Msgids, translated by the page.
fn disk_words() -> serde_json::Value {
    use scour_ui::disks::{KINDS, STATES, kind, question, state};
    let table = |names: &[&str], word: fn(&str) -> &'static str| -> serde_json::Value {
        names
            .iter()
            .map(|n| ((*n).to_owned(), serde_json::Value::from(word(n))))
            .collect::<serde_json::Map<_, _>>()
            .into()
    };
    serde_json::json!({
        "kinds": table(&KINDS, kind),
        "states": table(&STATES, state),
        "questions": table(&KINDS, question),
    })
}

/// The menu's drawings, by name, drawn in whatever colour the text around
/// them is: the window reads the same files from `scour-ui`.
fn menu_icons() -> serde_json::Value {
    scour_ui::menu::ICONS
        .iter()
        .chain(&["stack", "chevron"])
        .filter_map(|name| {
            let svg = scour_ui::menu::icon_svg(name)?
                .trim()
                .replace("stroke=\"#000\"", "stroke=\"currentColor\"")
                .replace(" width=\"24\" height=\"24\"", "");
            Some(((*name).to_owned(), serde_json::Value::String(svg)))
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

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
                        "icon": i.icon,
                        "short": i.short,
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
        .replacen("/* @MENUICONS@ */ {}", &menu_icons().to_string(), 1)
        .replacen("/* @DISKS@ */ {}", &disk_words().to_string(), 1)
        .replace("@LOGO@", &data_url(LOGO))
        .replacen("<script>", &format!("<script nonce=\"{}\">", nonce()), 1)
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
    /// Never run an executable, not even from the menu's Open. Without it a
    /// double-click still shows one in its folder; only Open offers to run it,
    /// once the page has asked and been told yes.
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

    // Nothing listening is no longer a reason to refuse to start: one is
    // started instead, and this keeps serving while it comes up. Every route
    // opens its own connection when the pool is empty, and the page already
    // has a state for a service it cannot reach.
    let client = Arc::new(Pool::new(addr.clone(), POOL));
    match Client::connect(&addr) {
        Ok(open) => client.keep(open),
        Err(e) if !scour_launch::wanted() => {
            return Err(e).with_context(|| {
                format!("no Scour service is listening on {addr}. Start one with `scourd`.")
            });
        }
        Err(_) => start_service(&addr, config.state_dir().join("scourd.log")),
    }

    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, args.port)))
        .with_context(|| format!("cannot listen on 127.0.0.1:{}", args.port))?;
    let port = listener.local_addr()?.port();
    let token = token();
    let url = format!("http://127.0.0.1:{port}/?t={token}");

    let _ = QUICKLOOK.set(scour_preview::quicklook(args.quicklook.as_deref()));

    // The size the page was last left at, said before the URL so a launcher
    // reading this log has it by the time it has the address: scour-app
    // starts the browser at it. Nothing said when no service answers yet.
    if let Ok(Response::Settings(s)) = call(&client, Request::Settings {})
        && let Some(size) = s.view.get("web").and_then(|w| w.get("size"))
        && let (Some(w), Some(h)) = (
            size.get(0).and_then(serde_json::Value::as_u64),
            size.get(1).and_then(serde_json::Value::as_u64),
        )
        && w > 0
        && h > 0
    {
        eprintln!("scour-web: window {w}x{h}");
    }
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

/// Whether a `Host`, or an `Origin` past its scheme, names this bridge: the
/// loopback address it listens on, or `localhost`, at its own port.
fn ours(host: &str, port: u16) -> bool {
    host.rsplit_once(':').is_some_and(|(name, p)| {
        p.parse() == Ok(port) && (name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost"))
    })
}

/// The nonce the page's one script carries; the policy runs nothing else.
fn nonce() -> &'static str {
    static NONCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NONCE.get_or_init(token)
}

/// A token nobody can guess.
fn token() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut bytes);
    }
    /* **And the system's randomness by the one door the standard library
     * opens on every platform**: a hash map's keys. Windows has no
     * `/dev/urandom`, and without this the token there was a pid and a clock,
     * four bytes repeated — about thirty bits, for a token that opens, runs
     * and deletes files. */
    for half in bytes.chunks_mut(8) {
        use std::hash::{BuildHasher, Hasher};
        let drawn = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish()
            .to_le_bytes();
        for (b, d) in half.iter_mut().zip(drawn) {
            *b ^= d;
        }
    }
    // A pid and a clock on top: it cannot repeat even if both of those fail.
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
    let _ = scour_openers::open(std::ffi::OsStr::new(url));
}

/// Start the service nobody started, on a thread, and say so on standard
/// error. Not waited for: the browser is already being opened, and a page
/// that cannot reach the service says so and retries by itself.
fn start_service(addr: &str, log: std::path::PathBuf) {
    let cat = scour_i18n::Catalogue::for_language(&scour_i18n::choose(
        "",
        CONFIGURED_LANGUAGE.get().map_or("", String::as_str),
    ));
    eprintln!("scour-web: {}", cat.get("Starting the Scour service…"));
    let hint = cat.get("Start one with `scourd`.").into_owned();
    let addr = addr.to_string();
    std::thread::spawn(move || {
        let outcome = scour_client::boot(&addr, &log);
        match outcome.reason() {
            Some(why) => eprintln!("scour-web: {why} — {hint}"),
            None => eprintln!("scour-web: the service is listening on {addr}"),
        }
    });
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

fn serve(mut stream: TcpStream, client: &Pool, addr: &str, token: &str, doing: Doing) {
    // Another account's process, holding a token it read off a command line.
    static ME: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    if let (Ok(peer), Ok(local)) = (stream.peer_addr(), stream.local_addr())
        && let Some(theirs) = peer::owner(peer, local)
        && Some(theirs) != *ME.get_or_init(peer::me)
    {
        http::fail(&mut stream, "403 Forbidden", "another account's connection");
        return;
    }
    let Some(req) = http::read_request(&stream) else {
        return;
    };

    // A browser cannot forge `Origin`: a foreign one is refused unread. Nor
    // `Host`, which a name rebound to 127.0.0.1 still carries.
    let port = stream.local_addr().map(|a| a.port()).unwrap_or(0);
    if req
        .header("origin")
        .is_some_and(|o| !o.strip_prefix("http://").is_some_and(|h| ours(h, port)))
        || req.header("host").is_some_and(|h| !ours(h, port))
    {
        http::fail(&mut stream, "403 Forbidden", "cross-origin");
        return;
    }
    // Reading is `GET`, doing is `POST`: an `<img src>` must not open a file.
    let acting = req.path == "/api/open"
        || req.path == "/api/trash"
        || req.path == "/api/delete"
        || req.path == "/api/reveal"
        || req.path == "/api/send"
        || req.path == "/api/drag"
        || req.path == "/api/rename"
        || req.path == "/api/open-with"
        || req.path == "/api/face"
        || req.path == "/api/thumb"
        || req.path == "/api/volume"
        || req.path == "/api/rescan"
        || req.path == "/api/reread"
        || req.path == "/api/index-do"
        || req.path == "/api/pick-folder"
        || (req.path == "/api/settings" && req.param("set").is_some())
        // Reading the desktop's key is a `GET`; writing one carries a body.
        || (req.path == "/api/hotkey" && !req.body.is_empty());
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
        "/" => http::page(&mut stream, page().as_bytes(), nonce()),
        "/api/search" => api_search(&mut stream, client, &req),
        "/api/csv" => api_csv(&mut stream, client, &req),
        "/api/count" => api_count(&mut stream, client, &req),
        "/api/kinds" => api_kinds(&mut stream, client, &req),
        "/api/strings" => api_strings(&mut stream, client, &req),
        "/api/places" => api_places(&mut stream, client),
        "/api/volumes" => disks::api_volumes(&mut stream, client),
        "/api/volume" => disks::api_volume(&mut stream, client, &req),
        "/api/rescan" => disks::api_rescan(&mut stream, client, &req),
        "/api/reread" => disks::api_reread(&mut stream, client, &req),
        "/api/index" => disks::api_index(&mut stream, client),
        "/api/index-do" => disks::api_index_do(&mut stream, client, &req, doing.launch),
        "/api/rules" => api_rules(&mut stream, client),
        "/api/settings" => api_settings(&mut stream, client, &req),
        "/api/facets" => api_facets(&mut stream, client, &req),
        "/api/usage" => api_usage(&mut stream, client, &req),
        "/api/tree" => grep::api_tree(&mut stream, client, &req),
        "/api/grep" if doing.preview => grep::api_grep(&mut stream, client, &req),
        "/api/grep" => http::fail(
            &mut stream,
            "403 Forbidden",
            "previewing is off (--no-preview)",
        ),
        "/api/pick-folder" if doing.launch => grep::api_pick_folder(&mut stream, &req),
        "/api/pick-folder" => {
            http::fail(&mut stream, "403 Forbidden", "opening is off (--no-launch)")
        }
        "/api/dupes" => api_dupes(&mut stream, client, &req),
        "/api/status" => api_status(&mut stream, client),
        "/api/icon" => api_icon(&mut stream, client, &req, doing.pictures),
        "/api/sysicon" => api_sysicon(&mut stream, &req),
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
        "/api/delete" => api_delete(&mut stream, client, &req),
        "/api/reveal" if doing.launch => api_reveal(&mut stream, client, &req),
        "/api/reveal" => http::fail(&mut stream, "403 Forbidden", "opening is off (--no-launch)"),
        "/api/send-targets" => api_send_targets(&mut stream),
        "/api/send" if doing.launch => api_send(&mut stream, client, &req),
        "/api/send" => http::fail(&mut stream, "403 Forbidden", "launching is off"),
        "/api/drag" if doing.launch => api_drag(&mut stream, client, &req),
        "/api/drag" => http::fail(&mut stream, "403 Forbidden", "launching is off"),
        "/api/rename" => api_rename(&mut stream, client, &req),
        "/api/openers" => api_openers(&mut stream, client, &req),
        "/api/open-with" if doing.launch => api_open_with(&mut stream, client, &req),
        "/api/open-with" => http::fail(&mut stream, "403 Forbidden", "launching is off"),
        "/api/duplicates" => dupes::api_duplicates(&mut stream, client, &req),
        "/api/hotkey" => hotkey::api_hotkey(&mut stream, &req),
        _ => http::fail(&mut stream, "404 Not Found", "no such route"),
    }
}

/// One call to the service, on a held connection when one is free.
fn call(client: &Pool, request: Request) -> Result<Response, String> {
    client.call(request).map_err(|e| e.to_string())
}

/// Connections held for the page: more than it has outstanding, and a bound on
/// the service's threads.
const POOL: usize = 8;

fn api_search(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
                        "found": h.found.iter().map(found_json).collect::<Vec<_>>(),
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

/// The thumbnail somebody has already made for a file. Cacheable, unlike every
/// other route: a file in a cache, not an answer about a watched filesystem.
fn api_icon(stream: &mut TcpStream, client: &Pool, req: &http::Req, pictures: bool) {
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
fn api_sysicon(stream: &mut TcpStream, req: &http::Req) {
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
fn api_thumb(stream: &mut TcpStream, addr: &str, req: &http::Req) {
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

/// The kinds a frontend should offer, in order, in the user's language.
/// `?lang=`: these labels are the one part of the rail with no msgid in the page.
fn api_kinds(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
fn api_strings(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
fn catalogue_for(client: &Pool, req: &http::Req) -> scour_i18n::Catalogue {
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
fn api_settings(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
fn api_rules(stream: &mut TcpStream, client: &Pool) {
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
fn api_places(stream: &mut TcpStream, client: &Pool) {
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
fn api_csv(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
fn api_count(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
fn api_dupes(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
fn api_usage(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
fn api_facets(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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

fn api_status(stream: &mut TcpStream, client: &Pool) {
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
        "unwritten": s.unwritten,
        "watching": s.watching,
        "sources": s.sources,
        "pending": s.pending,
        "index_bytes": s.index_bytes,
        "revision": s.revision,
        // Every query reads the unsorted tail: a week of ordinary use makes
        // ordering by path 1.9 ms against 21.5, and one rebuild puts it back.
        "unsorted": s.unsorted,
        "rebuild_advised": s.rebuild_advised,
        // Disks waiting for a yes or a no: the page asks for them when this moves.
        "asking": s.asking,
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
fn api_explain(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
fn api_face(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let which = req.param("face").unwrap_or_default();
    // Two names only: a program name from the page would run anything. The
    // terminal needs a tty, which `scour_launch::face` knows how to give.
    if !matches!(which, "window" | "tui") {
        http::fail(stream, "400 Bad Request", "no such face");
        return;
    }
    let Some(mut command) = scour_launch::face(which) else {
        http::fail(
            stream,
            "404 Not Found",
            &format!("{which} is not installed"),
        );
        return;
    };
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
fn api_openers(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
fn api_open_with(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
        Some(chosen) => match scour_openers::launch(&chosen, &scour_core::path::to_path(path)) {
            Ok(()) => http::json(stream, &serde_json::json!({ "started": chosen.name })),
            Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
        },
        // The list the page was shown is a moment old.
        None => http::fail(stream, "404 Not Found", "no such program any more"),
    }
}

/// Give a file a different name, and tell the index straight away. Renameable
/// only if the index holds the path; what a name may be is `scour-name`'s.
fn api_rename(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
    match scour_name::rename(&scour_core::path::to_path(path), name) {
        Ok(now) => {
            let now = scour_core::path::from_path(&now);
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

/// Send rows to the trash, and tell the index straight away. Trashable
/// only if the index holds the path; the move is this process's, with this
/// user's permissions, and [`Request::Recheck`] only re-reads.
fn api_trash(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let asked = paths_in(req);
    if asked.is_empty() {
        http::fail(stream, "400 Bad Request", "no paths");
        return;
    }

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
        match scour_trash::trash(&scour_core::path::to_path(path)) {
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

/// The paths a request carries: a JSON array of strings in its body. Not one
/// a line — a name may hold a newline, and `victim\nsuffix.txt` split there
/// sent `victim` to the trash.
fn paths_in(req: &http::Req) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(&req.body)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect()
}

/// Those paths, kept only where the index knows them: the fence every
/// endpoint that changes the disk stands behind.
fn fenced(client: &Pool, asked: &[String]) -> (Vec<String>, Vec<String>) {
    let mut known = Vec::new();
    let mut refused = Vec::new();
    for path in asked {
        if matches!(
            call(
                client,
                Request::Stat {
                    path: path.to_owned()
                }
            ),
            Ok(Response::Stat(_))
        ) {
            known.push(path.to_owned());
        } else {
            refused.push(format!("{path}: not in the index"));
        }
    }
    (known, refused)
}

/// Remove for good. The page asked first; this only does it, and tells the
/// service to look again as the trash does.
fn api_delete(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let asked = paths_in(req);
    if asked.is_empty() {
        http::fail(stream, "400 Bad Request", "no paths");
        return;
    }
    let (known, mut refused) = fenced(client, &asked);
    let mut gone = 0usize;
    for path in &known {
        match scour_trash::erase(&scour_core::path::to_path(path)) {
            Ok(()) => gone += 1,
            Err(e) => refused.push(format!("{path}: {e}")),
        }
    }
    if !known.is_empty() {
        let _ = call(client, Request::Recheck { paths: known });
    }
    http::json(
        stream,
        &serde_json::json!({ "gone": gone, "refused": refused }),
    );
}

/// Show these in the file manager, each selected in its folder.
fn api_reveal(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let asked = paths_in(req);
    if asked.is_empty() {
        http::fail(stream, "400 Bad Request", "no paths");
        return;
    }
    let (known, refused) = fenced(client, &asked);
    let native: Vec<std::path::PathBuf> =
        known.iter().map(|p| scour_core::path::to_path(p)).collect();
    let paths: Vec<&std::path::Path> = native.iter().map(std::path::PathBuf::as_path).collect();
    scour_openers::reveal(&paths);
    http::json(
        stream,
        &serde_json::json!({ "shown": known.len(), "refused": refused }),
    );
}

/// Where a selection can be sent on this machine now.
fn api_send_targets(stream: &mut TcpStream) {
    let targets: Vec<serde_json::Value> = scour_sendto::targets()
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "label": t.label,
                "named": t.named,
                "files_only": t.files_only,
            })
        })
        .collect();
    http::json(stream, &serde_json::json!({ "targets": targets }));
}

/// Send what the index knows of these to one of the places above.
fn api_send(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let asked = paths_in(req);
    let Some(to) = req
        .param("to")
        .filter(|t| !t.is_empty() && !asked.is_empty())
    else {
        http::fail(stream, "400 Bad Request", "no paths or no target");
        return;
    };
    let (known, refused) = fenced(client, &asked);
    if known.is_empty() {
        http::json(stream, &serde_json::json!({ "error": refused.join("\n") }));
        return;
    }
    let native: Vec<std::path::PathBuf> =
        known.iter().map(|p| scour_core::path::to_path(p)).collect();
    let paths: Vec<&std::path::Path> = native.iter().map(std::path::PathBuf::as_path).collect();
    let answer = match scour_sendto::send(to, &paths) {
        Ok(scour_sendto::Sent::Linked(at)) => {
            serde_json::json!({ "sent": "linked", "at": at.first().map(|p| p.to_string_lossy()) })
        }
        Ok(scour_sendto::Sent::Handed) => serde_json::json!({ "sent": "handed" }),
        Ok(scour_sendto::Sent::Packing(at)) => {
            serde_json::json!({ "sent": "packing", "at": at.to_string_lossy() })
        }
        Ok(scour_sendto::Sent::Copying(at)) => {
            serde_json::json!({ "sent": "copying", "at": at.to_string_lossy() })
        }
        Err(e) => serde_json::json!({ "error": e.msgid(), "detail": e.to_string() }),
    };
    http::json(stream, &answer);
}

/// Open the window's drag tray for these paths. A page cannot drag a file out:
/// Chromium hands its `file://` on as `about:blank#`, and `DownloadURL` is not
/// read on Linux. The tray is a window of its own, whose drag is real.
fn api_drag(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
    let asked = paths_in(req);
    let (known, refused) = fenced(client, &asked);
    if known.is_empty() {
        http::json(stream, &serde_json::json!({ "error": refused.join("\n") }));
        return;
    }
    let Some(binary) = beside_or_path("scour-gui") else {
        http::json(
            stream,
            &serde_json::json!({ "error": "scour-gui is not installed" }),
        );
        return;
    };
    let mut command = std::process::Command::new(&binary);
    command.arg("--drag").args(&known);
    // Its own process group, or closing this server closes the tray.
    scour_ui::faces::detach(&mut command);
    match command.spawn() {
        Ok(_) => http::json(stream, &serde_json::json!({ "opened": known.len() })),
        Err(e) => http::json(stream, &serde_json::json!({ "error": e.to_string() })),
    }
}

/// Open a path, or the folder holding it. The index is the fence: the service
/// is asked first, so this cannot be pointed at `/etc/shadow`. An executable
/// is shown selected in its folder — a double-click in a downloaded folder
/// must not start what it lands on — unless the menu's "Open" asked to run it
/// (`ask=1`), and then only once the page has asked and been told yes, and
/// never under `--no-run`.
fn api_open(stream: &mut TcpStream, client: &Pool, req: &http::Req, may_run: bool) {
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

    let native = scour_core::path::to_path(&entry.path);
    let p = native.as_path();

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

    // "Its folder" is the folder with this one selected in it: opening the
    // folder alone did nothing when it was already open, and showed nothing.
    if req.param("what") == Some("folder") {
        scour_openers::reveal(&[p]);
        http::json(
            stream,
            &serde_json::json!({ "opened": p.parent().unwrap_or(p).to_string_lossy() }),
        );
        return;
    }
    // A directory opened is that directory.
    let want_folder = entry.is_dir;
    // Asked of the entry, not the disk: every file on an `ntfs3` mount with
    // `fmask=0022` is 0755, so the mode bit alone would try to run a PDF.
    let runnable = !want_folder && scour_core::runs_when_opened(entry.name(), entry.meta.mode);
    let asked = req.param("ask") == Some("1") || req.param("run") == Some("yes");
    let run = runnable && may_run && asked;
    if run && req.param("run") != Some("yes") {
        http::json(stream, &serde_json::json!({ "ask_run": entry.name() }));
        return;
    }
    // Not run: shown selected in its folder, as the window shows it.
    if runnable && !run {
        scour_openers::reveal(&[p]);
        http::json(
            stream,
            &serde_json::json!({
                "opened": p.parent().unwrap_or(p).to_string_lossy(),
                "folder_instead": true,
            }),
        );
        return;
    }
    let target = p.to_path_buf();

    // Started in the directory it lives in, beside what it reads.
    let started = if run {
        std::process::Command::new(&target)
            .current_dir(target.parent().unwrap_or(&target))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
    } else {
        scour_openers::open(target.as_os_str())
    };
    match started {
        Ok(_) => http::json(
            stream,
            &serde_json::json!({
                "opened": target.to_string_lossy(),
                // Said, because nothing else on screen will say it was run;
                // the page words it, in the reader's language.
                "ran": run.then(|| target.file_name().unwrap_or_default().to_string_lossy()),
            }),
        ),
        Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
    }
}

/// The contents of one file, if it is something a browser can draw. Fenced as
/// `/api/open` is, so `..` needs no handling: a traversal that escapes the
/// roots lands somewhere the index has never heard of.
fn api_preview(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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

    let native = scour_core::path::to_path(&entry.path);
    let p = native.as_path();
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
                "thumb": icons::has_thumbnail(&entry.path, entry.kind())
                    || (scour_thumbs::native::available()
                        && !scour_thumbs::never_for(entry.kind().token())),
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
        scour_preview::Shape::Streamed(kind) => {
            if scour_preview::office::is_office(p) {
                let Some(pdf) = scour_preview::office::to_pdf(p) else {
                    http::fail(
                        stream,
                        "415 Unsupported Media Type",
                        "could not convert document",
                    );
                    return;
                };
                send_stream(stream, &pdf, kind, req.header("range"))
            } else {
                send_stream(stream, p, kind, req.header("range"))
            }
        }
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
mod tests;
