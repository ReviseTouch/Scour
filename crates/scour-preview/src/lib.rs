//! What can be shown of a file, and how much of it to hand over.
//!
//! A file search that finds the right *name* and then cannot tell you whether
//! it is the right *file* has stopped one step early. This is that step.
//!
//! ## It knows nothing about Scour, and that is the point
//!
//! No dependency — not `scour-core`, not the index, not the bridge. Everything
//! here is a question about a file and about what a viewer can decode, and the
//! answers are the same whether the caller is the browser bridge, a window, or
//! a native-messaging host written later. Nothing is passed in but a path and
//! whether it is a directory.
//!
//! In particular this deliberately does **not** take `scour_core::Kind`. `Kind`
//! says what a file is to a *person* — code, a document, a config — and what
//! decides a preview is what a decoder can *draw*. The two do not line up:
//! `Kind::Doc` holds a PDF a browser renders and a `.docx` it cannot open at
//! all, while `Kind::Code`, `Kind::Config` and `Kind::Data` are almost all
//! plain text under different names. Taking `Kind` would have looked like
//! reuse and been a coincidence.
//!
//! ## Named where naming is the fact, looked at where it is not
//!
//! There is a short table of extensions, and it is short on purpose: it names
//! only the formats where a decoder's own capability is what decides. Text is
//! not in it. **Text is the first few kilobytes being valid UTF-8 with no NUL
//! in them** — which is what makes a `.log`, a `README` with no extension, a
//! `.gitignore` and somebody's `notes.bak` all previewable without anybody
//! having thought of them, and what keeps a four-gigabyte `.safetensors` out
//! even though it is as much "data" as a `.json` is.
//!
//! ## What a caller still owes
//!
//! Two things, and they are the caller's because they are about the caller's
//! transport and the caller's fences:
//!
//! * **Deciding whether this path may be read at all.** Nothing here checks:
//!   it opens what it is handed. The bridge asks the *index* first, so a path
//!   no source owns is refused before it reaches this.
//! * **The response headers.** [`Shape::shown`] gives the content type and it
//!   is never `text/html` — anything textual, HTML included, is `text/plain` —
//!   but `nosniff` and a `sandbox` policy are the caller's to send, because
//!   only the caller knows it is speaking HTTP.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

/// The desktop's own quick-look, if this desktop has one.
///
/// **Two of the five have one, and saying which is the whole value of this
/// function.** A preview panel drawn by a browser is the same everywhere,
/// which is why it is the half that was built first; handing a file to the
/// *system's* previewer is the half that is not portable, and pretending
/// otherwise would put a button on screen that does nothing on three
/// platforms out of five.
///
/// | desktop | what it has |
/// |---|---|
/// | GNOME | `sushi`, a separate package — installed here |
/// | macOS | `qlmanage -p`, part of the system |
/// | KDE Plasma | nothing callable: the preview lives inside Dolphin's panel |
/// | Hyprland, sway, wlroots | nothing — there is no such protocol |
/// | Windows | nothing callable: Explorer's pane hosts `IPreviewHandler` COM objects in-process |
///
/// So this looks for what is *installed* rather than deciding from
/// `XDG_CURRENT_DESKTOP`: a desktop name is a claim about a session and this is
/// a question about a binary. Somebody running sushi under Hyprland gets it,
/// and somebody on GNOME who never installed it does not get a button that
/// fails.
///
/// **Only ever run on a path the index holds**, like every other launch here.
/// `-p` on `qlmanage` is the flag that means preview rather than generate a
/// thumbnail into the current directory.
///
/// Untested on macOS and Windows — Linux is the only platform any of this has
/// been run on. The README says so.
pub fn quicklook(chosen: Option<&str>) -> Option<Vec<String>> {
    if let Some(cmd) = chosen {
        let parts: Vec<String> = cmd.split_whitespace().map(str::to_owned).collect();
        return (!parts.is_empty()).then_some(parts);
    }
    const CANDIDATES: &[&[&str]] = &[
        // macOS. `-p` previews; without it this writes thumbnails into the
        // working directory, which would be a surprising thing for a search
        // box to do to somebody's home.
        &["qlmanage", "-p"],
        // GNOME. `gnome-sushi` is the package; `sushi` is the binary, and both
        // names have been shipped, so both are tried.
        &["sushi"],
        &["gnome-sushi"],
    ];
    CANDIDATES
        .iter()
        .find(|c| on_path(c[0]))
        .map(|c| c.iter().map(|s| (*s).to_owned()).collect())
}

/// Is this program on the path?
///
/// `PATH` rather than a shell, because a shell would take the name as script
/// and this must never be more than a lookup.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let p = dir.join(name);
                p.is_file() && {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::metadata(&p)
                            .map(|m| m.permissions().mode() & 0o111 != 0)
                            .unwrap_or(false)
                    }
                    #[cfg(not(unix))]
                    {
                        true
                    }
                }
            })
        })
        .unwrap_or(false)
}

/// How much text is worth sending.
///
/// Enough to read the head of any source file or config, far short of what a
/// log grows to. A preview answers "is this the file I meant", and nobody
/// answers that from line forty thousand.
const TEXT_CAP: u64 = 256 * 1024;

/// How much is read to decide whether a file is text at all.
const SNIFF: usize = 8 * 1024;

/// The largest picture that will be sent whole.
///
/// An image has no useful partial rendering, so this is a real ceiling rather
/// than a first page: past it the panel says how big the file is instead of
/// spending a hundred megabytes to say the same thing.
const IMAGE_CAP: u64 = 48 * 1024 * 1024;

/// How much is moved per write while streaming.
const CHUNK: usize = 64 * 1024;

/// What to do with a file, and what to call it on the way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Send the head of it as `text/plain`, whatever it is really called.
    Text,
    /// Send it whole, under this type. Images and PDFs.
    Whole(&'static str),
    /// Send it by range, under this type. Audio and video, which a browser
    /// asks for a piece at a time and cannot seek in without it.
    Streamed(&'static str),
    /// Nothing a browser can draw. The panel shows what is known instead.
    Nothing,
}

impl Shape {
    /// What a page should build for this, and what to label it.
    ///
    /// **Asked of the server rather than guessed from the extension**, which is
    /// the whole reason the probe exists. A page cannot tell that a file called
    /// `notes.bak` is readable and one called `model.safetensors` is not — that
    /// is decided by looking at the bytes, and the bytes are here.
    ///
    /// The category is derived from the type rather than stored beside it, so
    /// adding a format to `DRAWN` cannot put it in the wrong bucket.
    pub fn shown(self) -> (&'static str, &'static str) {
        match self {
            Shape::Text => ("text", "text/plain"),
            Shape::Whole(t) | Shape::Streamed(t) => {
                let what = match t.split('/').next().unwrap_or("") {
                    "image" => "image",
                    "audio" => "audio",
                    "video" => "video",
                    _ => "pdf",
                };
                (what, t)
            }
            Shape::Nothing => ("none", ""),
        }
    }
}

/// What can be shown of one file, as an answer that crosses a wire.
///
/// **The decision, not the bytes.** Which of the two belongs in the protocol
/// is the whole design of this: deciding needs the file's first eight
/// kilobytes and a table of extensions, and getting it wrong is invisible — a
/// frontend that guesses from the name alone will call `notes.bak` unreadable
/// and `model.safetensors` text. Moving the *bytes* would be worse than
/// useless: a browser asks for a video a piece at a time and cannot seek
/// without ranged HTTP, so whoever is speaking to the browser has to serve
/// them.
///
/// So the service says what a file is and hands over the head of it when that
/// is the whole answer; a terminal interface needs nothing else, and a window
/// points an `<img>` at its own transport.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Look {
    /// `text`, `image`, `audio`, `video`, `pdf`, or `none`.
    pub shape: String,
    /// The content type, empty when there is nothing to show.
    pub kind: String,
    /// The file's length in bytes.
    pub len: u64,
    /// For text, the head of it — up to [`TEXT_CAP`]. Empty otherwise.
    pub head: String,
    /// True when `head` stops short of the file's end.
    ///
    /// **Beside the text rather than appended to it**: a note added to the end
    /// would be a note inside the file being previewed.
    pub cut: bool,
}

/// Look at a file and say what can be shown of it.
pub fn look_at(path: &std::path::Path, is_dir: bool) -> Look {
    let shape = shape_of(path, is_dir);
    let (what, kind) = shape.shown();
    let mut look = Look {
        shape: what.to_owned(),
        kind: kind.to_owned(),
        len: len_of(path).unwrap_or(0),
        ..Look::default()
    };
    if shape == Shape::Text
        && let Ok((head, whole, len)) = text_head(path)
    {
        look.head = head;
        look.cut = !whole;
        look.len = len;
    }
    look
}

/// The formats a browser draws, by extension.
///
/// **Extension and not content**, on purpose: this is a question about the
/// browser's decoders, and the browser decides by the type header, which comes
/// from here. Sniffing magic bytes would let a `.png` that is really a JPEG
/// render — and would also let anything at all claim to be one.
const DRAWN: &[(&str, Shape)] = &[
    // Pictures.
    ("png", Shape::Whole("image/png")),
    ("jpg", Shape::Whole("image/jpeg")),
    ("jpeg", Shape::Whole("image/jpeg")),
    ("jfif", Shape::Whole("image/jpeg")),
    ("gif", Shape::Whole("image/gif")),
    ("webp", Shape::Whole("image/webp")),
    ("avif", Shape::Whole("image/avif")),
    ("bmp", Shape::Whole("image/bmp")),
    ("ico", Shape::Whole("image/x-icon")),
    // Safe as a picture because of the `sandbox` header, not despite it: an
    // `<img>` never runs an SVG's script, and a tab opened straight at this URL
    // is sandboxed into an opaque origin where it cannot either.
    ("svg", Shape::Whole("image/svg+xml")),
    // The one document format a browser reads. Streamed, because its viewer
    // asks for the trailer first and then pages in what it needs — handing it
    // eighty megabytes to show page one is work nobody asked for.
    ("pdf", Shape::Streamed("application/pdf")),
    // Sound.
    ("mp3", Shape::Streamed("audio/mpeg")),
    ("m4a", Shape::Streamed("audio/mp4")),
    ("aac", Shape::Streamed("audio/aac")),
    ("oga", Shape::Streamed("audio/ogg")),
    ("ogg", Shape::Streamed("audio/ogg")),
    ("opus", Shape::Streamed("audio/ogg")),
    ("wav", Shape::Streamed("audio/wav")),
    ("flac", Shape::Streamed("audio/flac")),
    // Moving pictures. Whether the *codec* inside plays is the browser's
    // business; the container is what the type header names.
    ("mp4", Shape::Streamed("video/mp4")),
    ("m4v", Shape::Streamed("video/mp4")),
    ("webm", Shape::Streamed("video/webm")),
    ("ogv", Shape::Streamed("video/ogg")),
    ("mov", Shape::Streamed("video/quicktime")),
    ("mkv", Shape::Streamed("video/x-matroska")),
];

/// How this file should be shown, if at all.
///
/// A directory is never previewed: what is inside one is a listing, and the
/// window already has a better answer for that than a panel would.
pub fn shape_of(path: &std::path::Path, is_dir: bool) -> Shape {
    if is_dir {
        return Shape::Nothing;
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if let Some((_, shape)) = DRAWN.iter().find(|(e, _)| *e == ext) {
        return *shape;
    }
    // Not a format with a decoder, so the only question left is whether a
    // person could read it. Asked of the bytes, because the alternative is a
    // list of every extension anyone ever gave a text file.
    if looks_like_text(path) {
        Shape::Text
    } else {
        Shape::Nothing
    }
}

/// Is the head of this file something a person could read?
///
/// Valid UTF-8 with no NUL. **The NUL is the whole test in practice** — every
/// binary format has one in its first few kilobytes and text never does — and
/// the UTF-8 check is what stops a file in some other encoding from arriving as
/// a screen of replacement characters pretending to be a preview.
///
/// A short read is not a failure: an empty file is text, and so is a file of
/// three bytes.
fn looks_like_text(path: &std::path::Path) -> bool {
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut head = vec![0u8; SNIFF];
    let Ok(n) = f.read(&mut head) else {
        return false;
    };
    head.truncate(n);
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(&head) {
        Ok(_) => true,
        // The tail of a buffer cut mid-character is not a reason to refuse:
        // what matters is that everything up to the break decoded.
        Err(e) => e.valid_up_to() + 4 >= head.len(),
    }
}

/// The head of a file as text, however it is really named.
///
/// Returns the text, whether that is all of it, and how long the file is.
/// Lossy on purpose: a file that sniffed as text can still hold one bad byte
/// further in, and a preview that fails on it is worse than one that shows a
/// replacement character in the middle of the line that has it.
///
/// The "whether that is all of it" is not decoration. A reader looking at the
/// first quarter-megabyte of a log has to know it is the first quarter — and
/// the caller must say so *beside* the text rather than appending a note to
/// it, which would put the note inside the file being previewed.
pub fn text_head(path: &std::path::Path) -> std::io::Result<(String, bool, u64)> {
    let f = File::open(path)?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::with_capacity(TEXT_CAP.min(len.max(1)) as usize);
    f.take(TEXT_CAP).read_to_end(&mut buf)?;
    Ok((
        String::from_utf8_lossy(&buf).into_owned(),
        len <= TEXT_CAP,
        len,
    ))
}

/// How big is it, and is that small enough to send whole?
///
/// A picture has no useful partial rendering, so the ceiling is a real
/// refusal: past it a caller should say how big the file is rather than spend
/// fifty megabytes to say the same thing.
pub fn whole_len(path: &std::path::Path) -> std::io::Result<Option<u64>> {
    let len = File::open(path)?.metadata()?.len();
    Ok((len <= IMAGE_CAP).then_some(len))
}

/// How long is it? For the streamed shapes, where the caller needs the length
/// before it can answer a range.
pub fn len_of(path: &std::path::Path) -> std::io::Result<u64> {
    Ok(File::open(path)?.metadata()?.len())
}

/// Write `count` bytes starting at `from` into `out`.
///
/// A chunk at a time and never more than asked for, because the length has
/// already gone into a header by the time this is called: a file that grew
/// between the `stat` and the read would otherwise desynchronise whatever is
/// framing the response.
///
/// A short file stops early rather than erroring. The caller promised more
/// than there turned out to be, and closing the connection is the only honest
/// end to that — which is why every response this feeds carries
/// `Connection: close`.
pub fn write_span(
    path: &std::path::Path,
    from: u64,
    count: u64,
    out: &mut dyn Write,
) -> std::io::Result<u64> {
    let mut f = File::open(path)?;
    if from > 0 {
        f.seek(SeekFrom::Start(from))?;
    }
    let mut buf = vec![0u8; CHUNK.min(count.max(1) as usize)];
    let mut left = count;
    while left > 0 {
        let want = buf.len().min(left as usize);
        let n = f.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        left -= n as u64;
    }
    Ok(count - left)
}

/// `bytes=a-b`, `bytes=a-`, `bytes=-n`. Inclusive, like the specification.
///
/// **Without this a video is not previewable, it is downloadable.** A browser
/// handed `<video src>` sends `Range: bytes=0-` and expects `206`; given `200`
/// it will still play, but it cannot seek and it holds the whole file — which
/// for the four-gigabyte recording somebody was looking for is the difference
/// between a preview and a mistake. A PDF viewer is worse: it reads the
/// trailer at the *end* of the file first, so with no ranges it fetches
/// everything to render page one.
///
/// `None` means "whatever this is, send the whole file" — a request that
/// cannot be understood is better answered completely than refused. One range
/// only; multipart ranges exist in the specification and nothing that plays
/// media asks for them.
pub fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    if len == 0 {
        return None;
    }
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let last = len - 1;
    let (from, to) = match (a.trim(), b.trim()) {
        // The suffix form: the final n bytes. This is the one a PDF viewer
        // opens with, because a PDF's index is at its end.
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (len.saturating_sub(n.max(1)), last)
        }
        (s, "") => (s.parse().ok()?, last),
        (s, e) => (s.parse().ok()?, e.parse::<u64>().ok()?.min(last)),
    };
    if from > to || from > last {
        return None;
    }
    Some((from, to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str, bytes: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("scour-preview-{name}"));
        std::fs::write(&p, bytes).expect("write");
        p
    }

    #[test]
    fn a_picture_is_named_by_its_extension() {
        let p = tmp("a.png", b"\x89PNG\r\n\x1a\n");
        assert_eq!(shape_of(&p, false), Shape::Whole("image/png"));
        let _ = std::fs::remove_file(p);
    }

    /// The rule that keeps the table short: text is decided by looking.
    #[test]
    fn a_readable_file_is_text_whatever_it_is_called() {
        for name in ["notes.bak", "gitignore", "thing.weird", "x.log"] {
            let p = tmp(name, "bir iki üç\ndört\n".as_bytes());
            assert_eq!(shape_of(&p, false), Shape::Text, "{name}");
            let _ = std::fs::remove_file(p);
        }
    }

    /// And the same rule keeps a model file out, though it is `Kind::Data`
    /// exactly as a `.json` is.
    #[test]
    fn something_with_a_nul_in_it_is_not_text() {
        let p = tmp("weights.bin", b"\x93NUMPY\x01\x00\x00binary\x00stuff");
        assert_eq!(shape_of(&p, false), Shape::Nothing);
        let _ = std::fs::remove_file(p);
    }

    /// **The case that produced a screen of replacement characters.** A file
    /// in some other encoding is not text this can show, and saying so beats
    /// showing it wrongly.
    #[test]
    fn a_file_in_another_encoding_is_not_offered_as_utf8() {
        // "Türkçe" in ISO-8859-9. Nothing here is NUL, so only the UTF-8 check
        // can reject it.
        let p = tmp("latin5.txt", b"T\xfcrk\xe7e metin");
        assert_eq!(shape_of(&p, false), Shape::Nothing);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn an_empty_file_is_text_rather_than_a_refusal() {
        let p = tmp("empty.txt", b"");
        assert_eq!(shape_of(&p, false), Shape::Text);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_folder_is_never_previewed() {
        let p = std::env::temp_dir();
        assert_eq!(shape_of(&p, true), Shape::Nothing);
    }

    /// Cut mid-character at the sniff boundary, which is where a naive UTF-8
    /// check calls a Turkish file binary.
    #[test]
    fn a_multibyte_character_split_by_the_sniff_boundary_is_still_text() {
        let mut bytes = vec![b'a'; SNIFF - 1];
        bytes.extend_from_slice("ş".as_bytes()); // two bytes, one of them past the edge
        bytes.extend_from_slice(&[b'b'; 64]);
        let p = tmp("edge.txt", &bytes);
        assert_eq!(shape_of(&p, false), Shape::Text);
        let _ = std::fs::remove_file(p);
    }

    /// The shape a caller builds an element from.
    ///
    /// Derived from the content type rather than stored beside it, so a format
    /// added to `DRAWN` cannot land in the wrong bucket — which is the sort of
    /// mistake that shows up as a `<video>` element holding a picture.
    #[test]
    fn every_drawn_format_says_which_element_it_is() {
        for (ext, shape) in DRAWN {
            let (what, kind) = shape.shown();
            assert!(
                ["image", "audio", "video", "pdf"].contains(&what),
                "{ext} → {what}"
            );
            assert!(kind.contains('/'), "{ext} → {kind:?}");
        }
        assert_eq!(Shape::Text.shown(), ("text", "text/plain"));
        assert_eq!(Shape::Nothing.shown().0, "none");
    }

    /// **Never `text/html`, whatever the file is called.** A page served under
    /// its own type, from the origin whose URL carries the token, is stored
    /// cross-site scripting with the key in the address bar. HTML is text, and
    /// text is what it goes out as.
    #[test]
    fn a_web_page_is_offered_as_text_and_not_as_a_page() {
        for name in ["index.html", "page.htm", "thing.xhtml", "x.js"] {
            let p = tmp(name, b"<script>alert(1)</script>\n");
            assert_eq!(shape_of(&p, false), Shape::Text, "{name}");
            let _ = std::fs::remove_file(p);
        }
        assert!(!DRAWN.iter().any(|(_, s)| s.shown().1.contains("html")));
    }

    /// The bytes, with no socket anywhere near them — which is what moving
    /// this out of the bridge bought.
    #[test]
    fn a_span_is_the_bytes_that_were_asked_for() {
        let p = tmp("span.bin", b"0123456789");
        let mut out = Vec::new();
        assert_eq!(write_span(&p, 3, 4, &mut out).expect("span"), 4);
        assert_eq!(out, b"3456");

        // A file shorter than the promise stops at its end and says how much
        // it moved, rather than blocking or inventing padding.
        let mut out = Vec::new();
        assert_eq!(write_span(&p, 8, 99, &mut out).expect("span"), 2);
        assert_eq!(out, b"89");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn the_head_of_a_long_file_says_it_is_only_the_head() {
        let big = tmp("big.txt", &vec![b'x'; (TEXT_CAP + 10) as usize]);
        let (text, whole, len) = text_head(&big).expect("head");
        assert_eq!(text.len() as u64, TEXT_CAP);
        assert!(!whole);
        assert_eq!(len, TEXT_CAP + 10);
        let _ = std::fs::remove_file(big);

        let small = tmp("small.txt", b"iki satir\n");
        let (text, whole, len) = text_head(&small).expect("head");
        assert_eq!(text, "iki satir\n");
        assert!(whole);
        assert_eq!(len, 10);
        let _ = std::fs::remove_file(small);
    }

    /// A named command is taken as written; a bad one is not turned into a
    /// button that fails.
    #[test]
    fn a_chosen_quicklook_wins_and_an_empty_one_is_no_quicklook() {
        assert_eq!(
            quicklook(Some("gwenview --fullscreen")),
            Some(vec!["gwenview".into(), "--fullscreen".into()])
        );
        assert_eq!(quicklook(Some("   ")), None);
    }

    #[test]
    fn ranges_are_read_the_way_a_browser_writes_them() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
        // The suffix form, which is how a PDF viewer opens a file.
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        // Past the end is clamped, not refused: a browser asks for more than
        // there is whenever it does not know the length yet.
        assert_eq!(parse_range("bytes=0-99999", 1000), Some((0, 999)));
        // What is not understood means "all of it", not "nothing".
        assert_eq!(parse_range("bytes=0-10,20-30", 1000), None);
        assert_eq!(parse_range("items=0-10", 1000), None);
        assert_eq!(parse_range("bytes=abc-", 1000), None);
        // A start past the end is a request that cannot be met.
        assert_eq!(parse_range("bytes=2000-", 1000), None);
        assert_eq!(parse_range("bytes=0-0", 0), None);
    }
}
