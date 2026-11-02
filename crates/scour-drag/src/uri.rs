//! Paths as the `text/uri-list` a drop target reads.

use std::path::Path;

/// `file://` and the path, with every byte that is not plainly safe in a
/// URI written as `%XX`: a space, a `#` or a non-ASCII name included.
pub fn file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_encoded_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// One URI a line, each ended by CRLF, as RFC 2483 asks.
pub fn uri_list(paths: &[&Path]) -> String {
    paths.iter().map(|p| file_uri(p) + "\r\n").collect()
}
