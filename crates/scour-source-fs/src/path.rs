//! One separator, everywhere — and every name representable.
//!
//! Paths cross this boundary as `/`-separated strings on every platform. The
//! index tokenises ancestors by splitting on `/`, the query language matches
//! `path:` fragments, the wire protocol carries them to a model that has never
//! heard of drive letters. Having two separators above this line would mean
//! every one of those places needing to know which platform produced the
//! string.
//!
//! So converting is this crate's job, in both directions, and nothing above it
//! ever sees a backslash.
//!
//! ## The part that is not about separators
//!
//! A Unix filename is a sequence of bytes that is not required to be UTF-8, and
//! this crate used `to_string_lossy`, which replaces every invalid byte with
//! `U+FFFD`. That is not a display blemish. Since a row is identified by its
//! path, two files whose names differ only in bytes the decoder threw away
//! became **one row** — the second silently replaced the first, and `stat`
//! could not name either of them again. Reproduced with `same-\xfe` and
//! `same-\xff`: two files on disk, one row in the index.
//!
//! So invalid bytes are *encoded* rather than replaced, into a private-use
//! character each: byte `0xNN` becomes `U+F7NN`. The mapping is one-to-one and
//! [`to_path`] undoes it, so the path stays a `String` — which is what keeps
//! this from being a change to every crate above — while still naming exactly
//! one file.
//!
//! **What it does not do:** a file whose name legitimately contains a character
//! in `U+F700..=U+F7FF` collides with the encoding of the corresponding byte.
//! Those are private-use code points; nothing assigns them and no locale
//! produces them. It is the same trade Python makes with `surrogateescape`,
//! which cannot be copied exactly here because Rust's `String` cannot hold a
//! lone surrogate. A truly injective key means carrying native bytes through
//! the wire protocol and the on-disk format, which is the right long-term
//! answer and a much larger change than this one.

use std::path::{Path, PathBuf};

/// The private-use block invalid bytes are encoded into. `0xNN` → `BASE + NN`.
const BASE: u32 = 0xF700;

/// A platform path as the rest of Scour sees it.
pub fn normalise(p: &str) -> String {
    if cfg!(windows) {
        p.replace('\\', "/")
    } else {
        p.to_owned()
    }
}

pub fn from_path(p: &Path) -> String {
    from_bytes(&bytes_of(p))
}

/// The same, for a name that is already bytes — a directory entry, say.
pub fn from_bytes(raw: &[u8]) -> String {
    // The common case is a valid name, and it costs one validation.
    match std::str::from_utf8(raw) {
        Ok(s) => normalise(s),
        Err(_) => normalise(&escape(raw)),
    }
}

/// Back to something the operating system will accept.
pub fn to_path(p: &str) -> PathBuf {
    let p = if cfg!(windows) {
        p.replace('/', "\\")
    } else {
        p.to_owned()
    };
    if !p.chars().any(is_escaped) {
        return PathBuf::from(p);
    }
    from_os_bytes(&unescape(&p))
}

fn is_escaped(c: char) -> bool {
    (BASE..BASE + 256).contains(&(c as u32))
}

/// Decode as much as is valid, and encode the bytes that are not.
fn escape(raw: &[u8]) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                return out;
            }
            Err(e) => {
                let good = e.valid_up_to();
                // SAFETY-free: `valid_up_to` is a UTF-8 boundary by definition.
                out.push_str(std::str::from_utf8(&rest[..good]).unwrap_or_default());
                let bad = e.error_len().unwrap_or(rest.len() - good);
                for &b in &rest[good..good + bad] {
                    out.push(char::from_u32(BASE + u32::from(b)).unwrap_or('\u{fffd}'));
                }
                rest = &rest[good + bad..];
            }
        }
    }
}

fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        if is_escaped(c) {
            out.push((c as u32 - BASE) as u8);
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    out
}

/// The name as the operating system holds it.
///
/// A `Cow` so that the two platforms have the same signature: unix hands over
/// the bytes it already has, Windows has to make some. Writing it twice with
/// two return types means every caller needs a conversion that is useless on
/// one of them, which the linter is right to object to.
#[cfg(unix)]
fn bytes_of(p: &Path) -> std::borrow::Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    std::borrow::Cow::Borrowed(p.as_os_str().as_bytes())
}

#[cfg(unix)]
fn from_os_bytes(raw: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(raw))
}

/// Windows names are ill-formed UTF-16 rather than ill-formed UTF-8, and the
/// standard library will not hand out their code units. The lossy conversion
/// stays there for now, and the same collision with it — this is the half of
/// the problem a native key is needed for.
#[cfg(not(unix))]
fn bytes_of(p: &Path) -> std::borrow::Cow<'_, [u8]> {
    std::borrow::Cow::Owned(p.to_string_lossy().into_owned().into_bytes())
}

#[cfg(not(unix))]
fn from_os_bytes(raw: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(raw).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for p in ["/home/u/x.txt", "/a", "relative/thing"] {
            assert_eq!(from_path(&to_path(p)), p);
        }
    }

    #[test]
    #[cfg(unix)]
    fn two_names_that_are_not_utf8_stay_two_names() {
        use std::os::unix::ffi::OsStrExt;
        // The reproduction: `same-\xfe` and `same-\xff` both decoded to
        // `same-\u{fffd}`, took the same path identity, and became one row.
        let a = Path::new(std::ffi::OsStr::from_bytes(b"/t/same-\xfe"));
        let b = Path::new(std::ffi::OsStr::from_bytes(b"/t/same-\xff"));
        assert_ne!(from_path(a), from_path(b), "two files, one name");

        // And each one names its own file again.
        assert_eq!(to_path(&from_path(a)), a);
        assert_eq!(to_path(&from_path(b)), b);

        // A name that is *partly* valid keeps the valid part readable, which
        // is what makes it findable by the letters it does have.
        let mixed = Path::new(std::ffi::OsStr::from_bytes(b"/t/rapor-\xc3(2).pdf"));
        let s = from_path(mixed);
        assert!(s.contains("rapor-"), "{s:?}");
        assert!(s.ends_with("(2).pdf"), "{s:?}");
        assert_eq!(to_path(&s), mixed);
    }

    #[test]
    #[cfg(unix)]
    fn a_valid_name_is_never_encoded() {
        // The escape must cost nothing and change nothing for the names
        // everybody actually has, Turkish ones included.
        for p in ["/home/u/Çalışmalar/rapor.pdf", "/home/u/ЖЖ/файл", "/a/漢字"] {
            assert_eq!(from_path(&to_path(p)), p);
            assert!(!p.chars().any(is_escaped));
        }
    }

    #[test]
    #[cfg(windows)]
    fn backslashes_do_not_escape_this_crate() {
        assert_eq!(normalise(r"C:\Users\hasan"), "C:/Users/hasan");
        assert_eq!(
            to_path("C:/Users/hasan").to_string_lossy(),
            r"C:\Users\hasan"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn a_backslash_is_an_ordinary_character_on_unix() {
        // It is a legal filename character here, and rewriting it would rename
        // the file as far as every lookup is concerned.
        assert_eq!(normalise("/home/u/odd\\name"), "/home/u/odd\\name");
    }
}
