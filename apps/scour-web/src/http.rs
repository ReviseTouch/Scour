//! Enough HTTP/1.1 to serve one page and answer its routes.
//!
//! Reads a request line and its headers, hands them over, writes a response.
//! No chunked encoding, no keep-alive, no compression, no ranges: a browser on
//! the same machine asking for 40 rows of JSON needs none of it.

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
    /// Paths go here, not in the query: a request line past [`MAX_HEAD`] is cut.
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
/// A peer that never sends a newline must not be able to grow the buffer.
const MAX_HEAD: usize = 16 * 1024;

/// And how much of a body. The same bound and the same reason.
///
/// [`scour_thumbs::Maker::BATCH`] paths at the longest a filesystem allows.
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

    // Exactly what was announced: reading to end of stream hangs on an open socket.
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
    // `no-store`: these answers describe a filesystem that is being watched.
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
/// Thumbnails only; every other answer here goes stale as the filesystem moves.
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
/// No `Content-Length`: an export is not built in memory, so the body ends at close.
pub fn attachment(stream: &mut TcpStream, kind: &str, filename: &str) {
    // Anything that could end the header early; the rest of a filename is legal.
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

/// A refusal, in the status line as well as in the body: never a `200 OK`.
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
        // A stray percent is text, not an error: filenames contain them.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
