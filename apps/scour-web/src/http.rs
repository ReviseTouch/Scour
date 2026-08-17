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
    /// What a `POST` carried, when it carried anything.
    ///
    /// **One route needs this and the rest never will.** Everything else here
    /// says what it wants in the query string, and that was enough until a
    /// request had to name a screenful of files at once: thirty-two paths
    /// percent-encoded is several kilobytes, [`MAX_HEAD`] is sixteen, and a
    /// request line that runs past it is *silently cut* — the truncation
    /// arrives as a 404 for a path nobody asked for, which is a bad hour.
    pub body: String,
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

/// And how much of a body. The same bound and the same reason.
///
/// Big enough for [`scour_thumbs::Maker::BATCH`] paths at any length a
/// filesystem allows — thirty-two times four kilobytes is a hundred and
/// twenty-eight — and small enough that a peer cannot make this process hold a
/// megabyte per connection.
const MAX_BODY: usize = 192 * 1024;

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

    // Exactly what was announced and not a byte more: reading to end of stream
    // would hang on a browser that keeps its socket open, and reading past the
    // length would eat the next request on a connection this happens not to
    // reuse today.
    let length = headers
        .get("content-length")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_BODY);
    let mut body = vec![0u8; length];
    if length > 0 && reader.read_exact(&mut body).is_err() {
        return None;
    }
    let body = String::from_utf8_lossy(&body).into_owned();

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
        body,
    })
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

/// Headers for an answer whose length is not known when it starts.
///
/// **No `Content-Length`, and that is the point.** An export walks the whole
/// matching set — two million rows here — and the only way to know its length
/// in advance is to build it all in memory first, which is the thing being
/// avoided. HTTP/1.1 allows a body that ends when the connection does, and
/// `Connection: close` is already what this server says, so the browser reads
/// until EOF. Chunked encoding would work too and buys nothing here: there is
/// no keep-alive to preserve.
///
/// `filename` turns it into a download rather than something the browser tries
/// to display. It is quoted and stripped of the two characters that could end
/// the header early; everything else a filesystem allows is legal in it.
pub fn attachment(stream: &mut TcpStream, kind: &str, filename: &str) {
    let safe: String = filename
        .chars()
        .filter(|c| *c != '"' && *c != '\r' && *c != '\n')
        .collect();
    let head = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: {kind}\r\n\
         Content-Disposition: attachment; filename=\"{safe}\"\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\r\n"
    );
    let _ = stream.write_all(head.as_bytes());
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
