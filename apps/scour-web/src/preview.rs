//! A file's bytes for the preview panel: text, a range of it, or the whole.

use super::*;
use scour_client::Pool;
use scour_proto::{Request, Response};
use std::io::Write;
use std::net::TcpStream;

/// The contents of one file, if it is something a browser can draw. Fenced as
/// `/api/open` is, so `..` needs no handling: a traversal that escapes the
/// roots lands somewhere the index has never heard of.
pub(super) fn api_preview(stream: &mut TcpStream, client: &Pool, req: &http::Req) {
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
                at: None,
                line: None,
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
                // A word processor's document that flows: `?flow=1`.
                "flow": look.flow,
            }),
        );
        return;
    }

    // A word processor's document, drawn by this program as a page of its own.
    if req.param("flow") == Some("1") {
        match call(
            client,
            Request::Flow {
                path: entry.path.clone(),
            },
        ) {
            Ok(Response::Flow(flow)) => {
                let locale = scour_flow::Locale::of(req.param("lang").unwrap_or("en"));
                if let Err(e) = send_flow(stream, &flow, &locale) {
                    http::fail(stream, "500 Internal Server Error", &e.to_string());
                }
            }
            Ok(_) => http::fail(stream, "502 Bad Gateway", "unexpected reply"),
            Err(e) => http::fail(stream, "415 Unsupported Media Type", &e),
        }
        return;
    }

    // The framing is here and the bytes are not: the one route answering with
    // bytes this program did not write.
    let result = match shape {
        scour_preview::Shape::Text => send_text(
            stream,
            p,
            req.param("at").and_then(|v| v.parse().ok()),
            req.param("line").and_then(|v| v.parse().ok()),
        ),
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
        // An Office document with no pages: the words the service read of it,
        // never its bytes.
        scour_preview::Shape::Nothing if scour_preview::office::is_office(p) => {
            match call(
                client,
                Request::Preview {
                    path: entry.path.clone(),
                    at: None,
                    line: None,
                },
            ) {
                Ok(Response::Preview(look)) if !look.head.is_empty() => send_words(stream, &look),
                _ => {
                    http::fail(stream, "415 Unsupported Media Type", "nothing to show");
                    return;
                }
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

/// A document's words as the service read them, framed as a text file's head.
fn send_words(stream: &mut TcpStream, look: &scour_preview::Look) -> std::io::Result<bool> {
    let head = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         X-Scour-Complete: {}\r\n\
         X-Scour-Size: {}\r\n\
         X-Scour-From: 0\r\n\
         X-Scour-First-Line: 1\r\n\
         {PREVIEW_HEADERS}\r\n",
        look.head.len(),
        u64::from(!look.cut),
        look.len,
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(look.head.as_bytes())?;
    stream.flush()?;
    Ok(true)
}

/// A flow's page. Its own policy, not [`PREVIEW_HEADERS`]': the page has
/// pictures, as `data:` URLs, and styles, inline — and still no script, nothing
/// fetched, and an opaque origin of its own.
fn send_flow(
    stream: &mut TcpStream,
    flow: &scour_flow::Flow,
    locale: &scour_flow::Locale,
) -> std::io::Result<()> {
    let page = scour_flow::html(flow, locale);
    let head = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'\r\n\
         Referrer-Policy: no-referrer\r\n\
         Connection: close\r\n\r\n",
        page.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(page.as_bytes())?;
    stream.flush()
}

/// The head of a text file, or the stretch around `at` (a byte) or `line`
/// (counted from one): where a content search found something.
fn send_text(
    stream: &mut TcpStream,
    p: &std::path::Path,
    at: Option<u64>,
    line: Option<u64>,
) -> std::io::Result<bool> {
    let near = scour_preview::text_near(p, at, line)?;
    let (text, len) = (near.text, near.len);
    let whole = !near.cut && near.from == 0;
    // A header, not a note in the body, which would read as part of the file.
    let head = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         X-Scour-Complete: {}\r\n\
         X-Scour-Size: {len}\r\n\
         X-Scour-From: {}\r\n\
         X-Scour-First-Line: {}\r\n\
         {PREVIEW_HEADERS}\r\n",
        text.len(),
        u64::from(whole),
        near.from,
        near.first_line,
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
