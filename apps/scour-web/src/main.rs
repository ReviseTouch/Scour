//! Scour in a browser: a bridge from HTTP to the service's Unix socket.
//!
//! Behind this port is an index of every file the user owns: 127.0.0.1 only
//! with no flag to change it, a per-run token without which every route is
//! 403, an `Origin` and a `Host` that must be ours, a peer that must be this
//! account's process, and `POST` for everything that acts.

mod actions;
mod document;
mod pictures;
mod preview;
mod search;
mod settings;
mod status;
use actions::*;
use document::*;
use pictures::*;
use preview::*;
use search::*;
use settings::*;
use status::*;

mod disks;
mod dupes;
mod grep;
mod hotkey;
mod http;
mod icons;
mod peer;

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use scour_client::Pool;
use scour_core::Catalog;
use scour_ipc::Client;
use scour_proto::{Request, Response};

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
        || (req.path == "/api/texts" && req.param("forget").is_some())
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
        "/api/texts" => disks::api_texts(&mut stream, client, &req),
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

/// `ui.language` from the config file, read once: editing `config.toml` has
/// always meant restarting.
static CONFIGURED_LANGUAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

#[cfg(test)]
mod tests;
