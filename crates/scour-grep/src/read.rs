//! One file, read a megabyte at a time and searched as it is read.

use std::fs::File;
use std::io::{ErrorKind, Read as _};
use std::path::Path;

use memchr::{memchr_iter, memrchr};
use scour_extract::rtf_text;

use crate::class::{Signature, signature};
use crate::excerpt::{Hit, Kind, around};
use crate::pattern::Pattern;
use crate::scan::{Options, Outcome, Skip, outcome_of};
use crate::texts::{Doc, Texts, extension_of, handed_over};
use crate::zip::{ZipScratch, search_whole};

/// An RTF document's text is kept whole to be searched; past this it is cut.
const RTF_TEXT: usize = 64 << 20;
/// Read at once. A megabyte holds 98% of this machine's files whole.
const CHUNK: usize = 1 << 20;
/// Carried into the next read, so a find across the seam is still whole.
const KEEP: usize = 64 << 10;

/// One thread's buffer, kept between files.
#[derive(Debug)]
pub(crate) struct Reader {
    buf: Vec<u8>,
    zip: ZipScratch,
    zip_on: bool,
}

impl Default for Reader {
    fn default() -> Self {
        Self {
            buf: Vec::new(),
            zip: ZipScratch::default(),
            zip_on: std::env::var_os("SCOUR_NO_ZIP").is_none(),
        }
    }
}

impl Reader {
    /// What `path` holds of the pattern, and how many bytes it took to say.
    pub(crate) fn search(
        &mut self,
        path: &Path,
        pattern: &Pattern,
        opts: &Options,
        texts: Option<&dyn Texts>,
    ) -> (Outcome, u64) {
        let (mut file, file_len) = match open(path, opts.max_size) {
            Ok(pair) => pair,
            Err(Refused::Skip(skip)) => return (Outcome::Skipped(skip), 0),
            Err(Refused::Io(kind)) => return (Outcome::Unreadable(kind), 0),
        };
        self.buf.resize(CHUNK, 0);
        let buf = &mut self.buf[..];
        let (mut filled, mut base, mut eof, mut total) = (0usize, 0u64, false, 0u64);
        let mut kind = None;
        let mut lines = Lines::default();
        let (mut hits, mut more) = (Vec::new(), false);
        loop {
            while filled < CHUNK && !eof {
                match file.read(&mut buf[filled..]) {
                    Ok(0) => eof = true,
                    Ok(n) => {
                        filled += n;
                        total += n as u64;
                        if file_len > 0 && total >= file_len {
                            eof = true;
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(e) => return (Outcome::Unreadable(e.kind()), total),
                }
            }
            let hay = &buf[..filled];
            if base == 0 && !pattern.is_raw_bytes() {
                let extension = extension_of(path);
                let document = matches!(signature(hay), Some(Signature::Unread))
                    || (extension == "eml" && handed_over(&extension));
                if let (Some(texts), true) = (texts, document) {
                    let (outcome, extra) = self.document(
                        &mut file, file_len, filled, eof, path, &extension, texts, pattern, opts,
                    );
                    return (outcome, total + extra);
                }
                match signature(hay) {
                    Some(Signature::Zip) if self.zip_on => {
                        let (outcome, extra) = self
                            .zip
                            .search_file(&mut file, file_len, hay, eof, pattern, opts);
                        return (outcome, total + extra);
                    }
                    Some(Signature::Rtf) => {
                        let (outcome, extra) =
                            self.rtf(&mut file, file_len, filled, eof, pattern, opts);
                        return (outcome, total + extra);
                    }
                    Some(Signature::Media) => return (Outcome::Skipped(Skip::HeaderOnly), total),
                    Some(Signature::Unread) if !opts.binary => {
                        return (Outcome::Skipped(Skip::NoReader), total);
                    }
                    Some(Signature::Unread | Signature::Packed) => kind = Some(Kind::Binary),
                    _ => {}
                }
            }
            if base == 0 && pattern.at_start() {
                kind = Some(Kind::Binary);
            }
            let kind = *kind.get_or_insert_with(|| Kind::sniff(hay));
            // A run of bytes is looked for in binary files without being asked.
            if kind == Kind::Binary && !opts.binary && !pattern.is_raw_bytes() {
                return (Outcome::Skipped(Skip::Binary), total);
            }
            let regex = pattern.reading(kind);
            let cut = if eof { filled } else { seam(hay, kind) };
            for found in regex.find_iter(hay) {
                // Past the seam it is found again, whole, in the next read.
                if found.start() >= cut && !eof {
                    break;
                }
                if found.is_empty() {
                    continue;
                }
                let (start, end) = match kind {
                    Kind::Binary => pattern.settled(hay, found.start(), found.end()),
                    _ => (found.start(), found.end()),
                };
                let line = (kind == Kind::Text).then(|| lines.at(path, base, hay, start));
                // A second find on a line already shown is the same line again.
                if line.is_some() && hits.last().is_some_and(|last: &Hit| last.line == line) {
                    continue;
                }
                if hits.len() >= opts.hits_per_file {
                    more = true;
                    break;
                }
                let (text, at) = around(hay, start, end, kind, opts.excerpt);
                hits.push(Hit {
                    offset: base + start as u64,
                    line,
                    text,
                    at,
                });
            }
            // Anchored at offset zero: a later buffer cannot hold it.
            if eof || more || pattern.at_start() {
                break;
            }
            lines.pass(base, hay, cut);
            buf.copy_within(cut..filled, 0);
            filled -= cut;
            base += cut as u64;
        }
        let outcome = if hits.is_empty() && !more {
            Outcome::Clean
        } else {
            Outcome::Found { hits, more }
        };
        (outcome, total)
    }
}

impl Reader {
    /// A document handed over: read whole, given to `texts`, its words searched.
    #[allow(clippy::too_many_arguments)]
    fn document(
        &mut self,
        file: &mut File,
        file_len: u64,
        filled: usize,
        eof: bool,
        path: &Path,
        extension: &str,
        texts: &dyn Texts,
        pattern: &Pattern,
        opts: &Options,
    ) -> (Outcome, u64) {
        let (whole, extra) = match self.rest_of(file, file_len, filled, eof) {
            Ok(pair) => pair,
            Err(kind) => return (Outcome::Unreadable(kind), 0),
        };
        let mtime = file
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        let doc = Doc {
            path,
            size: file_len,
            mtime,
            extension,
        };
        (outcome_of(texts.read(&doc, &whole), pattern, opts), extra)
    }

    /// The whole file: the first `filled` bytes from the buffer, the rest read.
    fn rest_of(
        &mut self,
        file: &mut File,
        file_len: u64,
        filled: usize,
        eof: bool,
    ) -> Result<(Vec<u8>, u64), ErrorKind> {
        let mut whole = self.buf[..filled].to_vec();
        if eof {
            return Ok((whole, 0));
        }
        let want = file_len.saturating_sub(filled as u64);
        let n = file
            .take(want)
            .read_to_end(&mut whole)
            .map_err(|e| e.kind())?;
        Ok((whole, n as u64))
    }

    /// An RTF document: the rest of it read, its text taken out and searched.
    /// The first `filled` bytes are in the buffer already.
    fn rtf(
        &mut self,
        file: &mut File,
        file_len: u64,
        filled: usize,
        eof: bool,
        pattern: &Pattern,
        opts: &Options,
    ) -> (Outcome, u64) {
        let (whole, extra) = match self.rest_of(file, file_len, filled, eof) {
            Ok(pair) => pair,
            Err(kind) => return (Outcome::Unreadable(kind), 0),
        };
        let mut text = String::new();
        rtf_text(&whole, &mut text, RTF_TEXT);
        (search_whole(text.as_bytes(), pattern, opts), extra)
    }
}

/// Where a full buffer is divided: after its last line when that is near the
/// end, so no line is searched in two halves; otherwise `KEEP` from the end.
fn seam(hay: &[u8], kind: Kind) -> usize {
    let floor = hay.len() - KEEP;
    match memrchr(b'\n', hay) {
        Some(i) if kind == Kind::Text && i + 1 >= floor => i + 1,
        _ => floor,
    }
}

/// Line numbers, counted only once something is found: a file without a find
/// pays nothing, and one with a find past its first megabyte is read again
/// up to it — few files, and in the page cache by then.
#[derive(Debug, Default)]
struct Lines {
    /// A position in the file, and the newlines before it.
    known: Option<(u64, u64)>,
}

impl Lines {
    fn at(&mut self, path: &Path, base: u64, hay: &[u8], pos: usize) -> u64 {
        let (seen, count) = *self
            .known
            .get_or_insert_with(|| (base, newlines_before(path, base)));
        let from = (seen - base) as usize;
        let count = count + memchr_iter(b'\n', &hay[from.min(pos)..pos]).count() as u64;
        self.known = Some(((base + pos as u64).max(seen), count));
        count + 1
    }

    /// The buffer is about to lose everything before `cut`.
    fn pass(&mut self, base: u64, hay: &[u8], cut: usize) {
        if let Some((seen, count)) = self.known {
            let from = ((seen - base) as usize).min(cut);
            let more = memchr_iter(b'\n', &hay[from..cut]).count() as u64;
            self.known = Some((base + cut as u64, count + more));
        }
    }
}

fn newlines_before(path: &Path, end: u64) -> u64 {
    let Ok(file) = File::open(path) else { return 0 };
    let (mut buf, mut count) = (vec![0u8; KEEP], 0u64);
    let mut file = file.take(end);
    while let Ok(n) = file.read(&mut buf) {
        if n == 0 {
            break;
        }
        count += memchr_iter(b'\n', &buf[..n]).count() as u64;
    }
    count
}

enum Refused {
    Skip(Skip),
    Io(ErrorKind),
}

/// Open to read, without being held by what is not a file — a pipe opened
/// plainly waits for a writer for ever — without following a link, whose
/// target is a file of its own, and without touching its read time.
fn open(path: &Path, max_size: u64) -> Result<(File, u64), Refused> {
    if in_the_cloud_before(path) {
        return Err(Refused::Skip(Skip::Offline));
    }
    let file = open_quietly(path).map_err(refused)?;
    let meta = file.metadata().map_err(|e| Refused::Io(e.kind()))?;
    if !meta.is_file() {
        return Err(Refused::Skip(Skip::NotAFile));
    }
    if in_the_cloud(&meta) {
        return Err(Refused::Skip(Skip::Offline));
    }
    let len = meta.len();
    if max_size != 0 && len > max_size {
        return Err(Refused::Skip(Skip::TooBig));
    }
    Ok((file, len))
}

fn refused(e: std::io::Error) -> Refused {
    #[cfg(unix)]
    if e.raw_os_error() == Some(libc::ELOOP) {
        return Refused::Skip(Skip::NotAFile);
    }
    // ERROR_SHARING_VIOLATION and ERROR_LOCK_VIOLATION.
    #[cfg(windows)]
    if matches!(e.raw_os_error(), Some(32 | 33)) {
        return Refused::Skip(Skip::Locked);
    }
    Refused::Io(e.kind())
}

#[cfg(unix)]
fn open_quietly(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let with = |flags: i32| File::options().read(true).custom_flags(flags).open(path);
    let base = libc::O_NONBLOCK | libc::O_NOFOLLOW;
    // Only a file's owner may leave its read time alone; anybody else asks again.
    #[cfg(target_os = "linux")]
    match with(base | libc::O_NOATIME) {
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {}
        other => return other,
    }
    with(base)
}

/// Shared for reading, writing and deletion — `std`'s default — read front
/// to back, which the cache manager is told, and opened without fetching a
/// cloud file's data, so that its attributes can say it is not here.
#[cfg(windows)]
fn open_quietly(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
    const FILE_FLAG_OPEN_NO_RECALL: u32 = 0x0010_0000;
    File::options()
        .read(true)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN | FILE_FLAG_OPEN_NO_RECALL)
        .open(path)
}

/// OneDrive's and other cloud providers' placeholders: offline, or recalled on
/// open or on the first read.
#[cfg(windows)]
fn in_the_cloud(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    recalled(meta.file_attributes())
}

/// Whether Windows file attributes say the data is not on this disk.
#[cfg_attr(not(windows), allow(dead_code))]
fn recalled(attributes: u32) -> bool {
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x0004_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    attributes & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(not(windows))]
fn in_the_cloud(_meta: &std::fs::Metadata) -> bool {
    false
}

/// iCloud's evicted files are `SF_DATALESS`, and reading one downloads it;
/// asked before opening, of the name.
#[cfg(target_os = "macos")]
fn in_the_cloud_before(path: &Path) -> bool {
    use std::os::macos::fs::MetadataExt as _;
    const SF_DATALESS: u32 = 0x4000_0000;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.st_flags() & SF_DATALESS != 0)
}

#[cfg(not(target_os = "macos"))]
fn in_the_cloud_before(_path: &Path) -> bool {
    false
}

#[cfg(not(any(unix, windows)))]
fn open_quietly(path: &Path) -> std::io::Result<File> {
    File::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cloud_placeholder_is_told_by_its_attributes() {
        // A OneDrive file kept online only; one pinned to this disk; a plain file.
        assert!(recalled(0x0040_0000 | 0x20));
        assert!(recalled(0x1000));
        assert!(recalled(0x0004_0000));
        assert!(!recalled(0x0008_0000 | 0x20));
        assert!(!recalled(0x80));
    }
}
