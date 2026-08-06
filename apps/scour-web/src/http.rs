//! Enough HTTP/1.1 to serve one page and answer four routes.
//!
//! Not a web framework and not trying to be one. It reads a request line and
//! its headers, hands them over, and writes a response with a length. What it
//! deliberately does not do is as much as what it does: no chunked encoding, no
//! keep-alive negotiation beyond closing, no compression, no ranges. A browser
//! on the same machine asking for 40 rows of JSON needs none of it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

/// One request, as much of it as anything here cares about.
#[derive(Debug)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
}

impl Req {
    pub fn param(&self, key: &str) -> Option<&str> {
        self.query.get(key).map(String::as_str)
    }

    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .get(&key.to_ascii_lowercase())
            .map(String::as_str)
    }
}

/// How much of a request line and its headers will be read before giving up.
///
/// A bound rather than trust: this listens on a port, and a peer that never
/// sends a newline must not be able to grow a buffer until the process dies.
const MAX_HEAD: usize = 16 * 1024;

pub fn read_request(stream: &TcpStream) -> Option<Req> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    let mut read = 0usize;

    reader
        .by_ref()
        .take(MAX_HEAD as u64)
        .read_line(&mut line)
        .ok()?;
    read += line.len();
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();

    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        if read >= MAX_HEAD || reader.read_line(&mut h).ok()? == 0 {
            break;
        }
        read += h.len();
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }

    let (path, raw) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let mut query = HashMap::new();
    for pair in raw.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(percent_decode(k), percent_decode(v));
    }
    Some(Req {
        method,
        path: path.to_owned(),
        query,
        headers,
    })
}

/// Does the desktop treat this as something to **run** rather than to view?
///
/// `xdg-open` is not a viewer. Handed a `.desktop` file it executes what the
/// file says, and handed an executable a file manager will offer to run it —
/// so "open" on those two is not opening, it is launching, and a search box
/// must not be a way to launch things. They get their folder revealed instead,
/// which is what somebody looking for them wanted anyway.
pub fn is_runnable(path: &std::path::Path) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(
        ext.as_str(),
        // Windows decides by extension and has no mode bit to ask about, so
        // its list is the whole answer there and part of the answer here.
        "desktop"
            | "sh"
            | "bash"
            | "appimage"
            | "run"
            | "bin"
            | "exe"
            | "bat"
            | "cmd"
            | "com"
            | "msi"
            | "ps1"
            | "scr"
            | "lnk"
    ) {
        return true;
    }
    // And by mode where there is one. Windows has no execute bit — an ACL is
    // not a bit — so the list above is the whole answer there.
    #[cfg(unix)]
    let by_mode = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    };
    #[cfg(not(unix))]
    let by_mode = false;
    by_mode
}

/// `%XX` and `+`, which is all a query string can carry.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &[u8]) {
    // `no-store` because every one of these answers is about a filesystem that
    // is being watched: a cached page is a page that stopped being true.
    let head = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {kind}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// A response the browser may keep.
///
/// For icons and nothing else. Everything else this serves is about a
/// filesystem that is being watched, where a cached answer is one that has
/// stopped being true — but a theme's drawing of "document" does not change
/// while a window is open, and re-fetching it per row is what makes a list
/// scroll badly.
pub fn cached(stream: &mut TcpStream, kind: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: {kind}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: private, max-age=86400\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

pub fn json(stream: &mut TcpStream, value: &serde_json::Value) {
    respond(
        stream,
        "200 OK",
        "application/json; charset=utf-8",
        value.to_string().as_bytes(),
    );
}

/// A refusal, in the status line as well as in the body.
///
/// It used to go out through [`json`], which answered a request carrying the
/// wrong token with `200 OK` and the refusal buried in the body. Nothing
/// leaked — the answer was still a refusal — but a status line that says the
/// opposite of what happened is a lie to everything that reads one, and here
/// it was the fence around the index doing the lying.
pub fn fail(stream: &mut TcpStream, status: &str, detail: &str) {
    respond(
        stream,
        status,
        "application/json; charset=utf-8",
        serde_json::json!({ "error": detail, "status": status })
            .to_string()
            .as_bytes(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn what_the_desktop_would_run_is_not_something_to_open() {
        use std::path::Path;
        assert!(is_runnable(Path::new("/x/thing.desktop")));
        assert!(is_runnable(Path::new("/x/install.sh")));
        assert!(is_runnable(Path::new("/x/Some.AppImage")));
        // Not by name, and not by luck: an ordinary document stays openable.
        assert!(!is_runnable(Path::new("/x/rapor.pdf")));
        assert!(!is_runnable(Path::new("/x/notes")));
        // And by mode, for the ones with no telling extension.
        let tmp = std::env::temp_dir().join("scour-web-runnable-probe");
        std::fs::write(&tmp, b"#!/bin/sh\n").expect("write");
        assert!(!is_runnable(&tmp), "not executable yet");
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(is_runnable(&tmp), "executable now");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn a_query_string_decodes_the_way_a_browser_wrote_it() {
        assert_eq!(percent_decode("rapor"), "rapor");
        assert_eq!(percent_decode("bir+iki"), "bir iki");
        assert_eq!(percent_decode("ext%3Ars"), "ext:rs");
        // Turkish, which is the whole reason this cannot be byte-wise.
        assert_eq!(percent_decode("k%C3%BCt%C3%BCphane"), "kütüphane");
        // A stray percent is text, not an error: the query is a filename and
        // filenames contain percent signs.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
