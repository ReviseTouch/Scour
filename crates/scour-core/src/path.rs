//! One separator, everywhere — and every name representable.
//!
//! Paths cross this boundary as `/`-separated strings on every platform. A Unix
//! name need not be UTF-8, so an invalid byte `0xNN` is *encoded* into `U+F7NN`
//! rather than replaced: `same-\xfe` and `same-\xff` have to stay two rows.

use std::path::{Path, PathBuf};

/// The private-use block invalid bytes are encoded into. `0xNN` → `BASE + NN`.
/// A name legitimately holding `U+F700..=U+F7FF` collides; nothing assigns them.
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
/// A `Cow` so both platforms share one signature: unix borrows, Windows allocates.
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

/// Windows names are ill-formed UTF-16 rather than UTF-8, and the standard library
/// will not hand out their code units; the lossy conversion and its collision stay.
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
        // `same-\xfe` and `same-\xff` both decoded to `same-\u{fffd}`: one row.
        let a = Path::new(std::ffi::OsStr::from_bytes(b"/t/same-\xfe"));
        let b = Path::new(std::ffi::OsStr::from_bytes(b"/t/same-\xff"));
        assert_ne!(from_path(a), from_path(b), "two files, one name");

        // And each one names its own file again.
        assert_eq!(to_path(&from_path(a)), a);
        assert_eq!(to_path(&from_path(b)), b);

        // A partly valid name keeps its valid part findable by the letters it has.
        let mixed = Path::new(std::ffi::OsStr::from_bytes(b"/t/rapor-\xc3(2).pdf"));
        let s = from_path(mixed);
        assert!(s.contains("rapor-"), "{s:?}");
        assert!(s.ends_with("(2).pdf"), "{s:?}");
        assert_eq!(to_path(&s), mixed);
    }

    #[test]
    #[cfg(unix)]
    fn a_valid_name_is_never_encoded() {
        // The escape must cost nothing and change nothing for ordinary names.
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
        // A legal filename character here; rewriting it would rename the file.
        assert_eq!(normalise("/home/u/odd\\name"), "/home/u/odd\\name");
    }
}
