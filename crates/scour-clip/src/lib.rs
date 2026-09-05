//! Put something on the desktop's clipboard, using the desktop's own program.
//!
//! **Why a subprocess rather than a clipboard library.** Two of Scour's faces
//! need this — a window and a terminal — and neither has any other reason to
//! know whether it is running under Wayland or X11. A clipboard crate would
//! link a protocol binding into both of them, and on Wayland it would also need
//! a surface to own the selection, which a terminal does not have. The display
//! server already ships a program for this; the window already spawns
//! `xdg-open` and `fc-match` without anyone calling that a dependency.
//!
//! ## The part that is not obvious
//!
//! Copying a *file* is not copying its path. A file manager pastes a file when
//! it is offered `text/uri-list` — a list of `file://` URIs — and pastes a
//! string of text when it is offered `text/plain`. Offering the path as plain
//! text produces a paste of the path *into* whatever was focused, which is a
//! different and usually wrong thing.
//!
//! GNOME's Files wants one more: an `x-special/gnome-copied-files` payload
//! beginning with `copy` or `cut`. Sending both is what makes a paste work in
//! Files *and* in every toolkit that only reads the standard type.
//!
//! ## What it cannot promise
//!
//! On Wayland the clipboard belongs to a surface, and `wl-copy` keeps a
//! process alive to hold it. Killing that process empties the clipboard — so a
//! copy from a program that exits immediately afterwards may not survive. That
//! is the display server's rule rather than this crate's, and it is why
//! [`text`] returns once the helper has *taken* the selection rather than once
//! it has exited.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug)]
pub enum Error {
    /// No clipboard helper on this machine.
    NoHelper,
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoHelper => write!(
                f,
                "no clipboard helper found — install wl-clipboard, xclip or xsel"
            ),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// One way of talking to the clipboard, and how to ask it for a MIME type.
struct Helper {
    program: &'static str,
    /// Arguments for plain text.
    plain: &'static [&'static str],
    /// Arguments for a named MIME type, with `{}` standing for it.
    typed: &'static [&'static str],
}

/// In the order they are tried.
///
/// Wayland first because that is what a modern session is, and `wl-copy` under
/// X11 fails immediately rather than doing something surprising. `xclip` before
/// `xsel` because it is the one that can be told a MIME type, which is the
/// whole of [`files`].
const HELPERS: &[Helper] = &[
    Helper {
        program: "wl-copy",
        plain: &["--type", "text/plain;charset=utf-8"],
        typed: &["--type", "{}"],
    },
    Helper {
        program: "xclip",
        plain: &["-selection", "clipboard", "-i"],
        typed: &["-selection", "clipboard", "-t", "{}", "-i"],
    },
    Helper {
        program: "xsel",
        plain: &["--clipboard", "--input"],
        // `xsel` has no way to name a type, so it can only do plain text. The
        // caller finds out from [`files`] returning `NoHelper` rather than from
        // a paste that silently produces a path.
        typed: &[],
    },
];

/// Is there anything on this machine that can do it?
///
/// For a menu that would rather leave an item out than fail after the press.
pub fn available() -> bool {
    HELPERS.iter().any(|h| found(h.program))
}

/// Put text on the clipboard.
pub fn text(s: &str) -> Result<(), Error> {
    for h in HELPERS {
        if !found(h.program) {
            continue;
        }
        return feed(h.program, h.plain, &[], s.as_bytes());
    }
    Err(Error::NoHelper)
}

/// Put files on the clipboard, so that a file manager pastes *the files*.
///
/// Not the same as putting their paths on it as text — see the note at the top
/// of this module.
pub fn files(paths: &[&Path]) -> Result<(), Error> {
    let uris: Vec<String> = paths.iter().map(|p| uri(p)).collect();
    let list = uris.join("\r\n");

    for h in HELPERS {
        if !found(h.program) || h.typed.is_empty() {
            continue;
        }
        feed(h.program, h.typed, &["text/uri-list"], list.as_bytes())?;
        // GNOME's Files reads its own type and nothing else. Failing here is
        // not a failure of the copy: everything that reads the standard type
        // already has what it needs.
        let gnome = format!("copy\n{}", uris.join("\n"));
        let _ = feed(
            h.program,
            h.typed,
            &["x-special/gnome-copied-files"],
            gnome.as_bytes(),
        );
        return Ok(());
    }
    Err(Error::NoHelper)
}

fn feed(program: &str, args: &[&str], subst: &[&str], body: &[u8]) -> Result<(), Error> {
    let mut command = Command::new(program);
    let mut fill = subst.iter();
    for a in args {
        match (*a == "{}", fill.next()) {
            (true, Some(v)) => command.arg(v),
            _ => command.arg(a),
        };
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Error::Io)?;
    child
        .stdin
        .take()
        .ok_or(Error::NoHelper)?
        .write_all(body)
        .map_err(Error::Io)?;
    // **Not waited for.** `wl-copy` stays alive holding the selection, which is
    // how Wayland works: the clipboard belongs to a live client. Waiting here
    // would hang the caller for as long as the copy is useful.
    Ok(())
}

/// `file:///home/a%20b/notes.txt` — the form a file manager expects.
///
/// Percent-encoded as RFC 3986 asks, with `/` left alone so the URI stays
/// something a person can read in a paste that went to the wrong window.
fn uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Is this program on the path?
///
/// `PATH` rather than `which`, because spawning a process to find out whether a
/// process exists is the cost this is trying to avoid — the menu asks once a
/// press.
fn found(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let at = dir.join(program);
                std::fs::metadata(&at).map(|m| m.is_file()).unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoding a paste depends on.
    #[test]
    fn a_uri_is_escaped_where_it_has_to_be_and_readable_where_it_does_not() {
        assert_eq!(
            uri(Path::new("/home/a/notes.txt")),
            "file:///home/a/notes.txt"
        );
        assert_eq!(
            uri(Path::new("/home/a b/x.txt")),
            "file:///home/a%20b/x.txt"
        );
        // Turkish file names are the ordinary case here, not an edge one.
        assert_eq!(uri(Path::new("/ev/çay.md")), "file:///ev/%C3%A7ay.md");
        // A percent that was already in the name must not read as an escape.
        assert_eq!(uri(Path::new("/a/50%.txt")), "file:///a/50%25.txt");
    }

    /// Every helper can do text; only some can name a type.
    ///
    /// The table is the thing that decides whether `files` works, so it is the
    /// thing worth checking — a `typed` list that lost its `{}` would put the
    /// literal two characters on the clipboard as a MIME type.
    #[test]
    fn a_helper_that_can_name_a_type_has_somewhere_to_put_it() {
        for h in HELPERS {
            assert!(!h.plain.is_empty(), "{} has no plain arguments", h.program);
            if !h.typed.is_empty() {
                assert!(
                    h.typed.contains(&"{}"),
                    "{} claims a type it has no slot for",
                    h.program
                );
            }
        }
        assert!(
            HELPERS.iter().any(|h| !h.typed.is_empty()),
            "nothing here can copy a file"
        );
    }

    #[test]
    fn a_program_that_is_not_installed_is_not_found() {
        assert!(!found("scour-a-program-nobody-has"));
        // Something every machine running these tests has.
        assert!(found("sh") || found("cat"));
    }
}
