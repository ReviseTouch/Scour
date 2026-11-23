//! One file, read a megabyte at a time and searched as it is read.

use std::fs::File;
use std::io::{ErrorKind, Read as _};
use std::path::Path;

use memchr::{memchr_iter, memrchr};

use crate::excerpt::{Hit, Kind, around};
use crate::pattern::Pattern;
use crate::scan::{Options, Outcome, Skip};
use crate::zip::ZipScratch;

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
            if base == 0
                && self.zip_on
                && !pattern.is_raw_bytes()
                && hay.len() >= 4
                && hay[..4] == [0x50, 0x4B, 0x03, 0x04]
            {
                let (outcome, extra) =
                    self.zip.search_file(&mut file, file_len, hay, eof, pattern, opts);
                return (outcome, total + extra);
            }
            let kind = *kind.get_or_insert_with(|| Kind::sniff(hay));
            if kind == Kind::Binary && !opts.binary {
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
            if eof || more {
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
/// plainly waits for a writer for ever — and without touching its read time.
fn open(path: &Path, max_size: u64) -> Result<(File, u64), Refused> {
    let file = open_quietly(path).map_err(|e| Refused::Io(e.kind()))?;
    let meta = file.metadata().map_err(|e| Refused::Io(e.kind()))?;
    if !meta.is_file() {
        return Err(Refused::Skip(Skip::NotAFile));
    }
    let len = meta.len();
    if max_size != 0 && len > max_size {
        return Err(Refused::Skip(Skip::TooBig));
    }
    Ok((file, len))
}

#[cfg(unix)]
fn open_quietly(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let with = |flags: i32| File::options().read(true).custom_flags(flags).open(path);
    // Only a file's owner may leave its read time alone; anybody else asks again.
    #[cfg(target_os = "linux")]
    match with(libc::O_NONBLOCK | libc::O_NOATIME) {
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {}
        other => return other,
    }
    with(libc::O_NONBLOCK)
}

#[cfg(not(unix))]
fn open_quietly(path: &Path) -> std::io::Result<File> {
    File::open(path)
}
