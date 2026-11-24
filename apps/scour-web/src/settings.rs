//! What the page reads once and writes back: words, kinds, settings, rules,
//! the desktop's places.

use super::*;
use scour_client::Pool;
use scour_core::Catalog;
use scour_proto::{Request, Response};
use std::net::TcpStream;

/// The kinds a frontend should offer, in order, in the user's language.
/// `?lang=`: these labels are the one part of the rail with no msgid in the page.
pub(super) fn api_kinds(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
pub(super) fn api_strings(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
pub(super) fn api_settings(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
pub(super) fn api_rules(stream: &mut TcpStream, client: &Pool) {
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
pub(super) fn api_places(stream: &mut TcpStream, client: &Pool) {
    match call(client, Request::Places {}) {
        Ok(Response::Places(p)) => match serde_json::to_value(&p) {
            Ok(v) => http::json(stream, &v),
            Err(e) => http::fail(stream, "500 Internal Server Error", &e.to_string()),
        },
        Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
        Err(e) => http::fail(stream, "502 Bad Gateway", &e),
    }
}
