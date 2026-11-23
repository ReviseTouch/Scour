//! Zero-allocation SIMD streaming search inside ZIP-based documents (`.udf`,
//! `.docx`, `.xlsx`, `.pptx`, `.odt`, `.epub`) and general `.zip` archives,
//! including nested documents inside `.zip` files.

use std::fs::File;
#[cfg(not(unix))]
use std::io::{Read as _, Seek as _, SeekFrom};

use flate2::{Decompress, FlushDecompress, Status};
use memchr::{memchr_iter, memrchr, memrchr_iter};

use crate::excerpt::{Hit, Kind, around};
use crate::pattern::Pattern;
use crate::scan::{Options, Outcome, Skip};
use crate::xml::{strip_xml, udf_cdata};

const WINDOW: usize = 256 << 10;
const KEEP: usize = 32 << 10;
const MAX_CD: usize = 8 << 20;
const MAX_ENTRY_COMP: usize = 32 << 20;
const MAX_ENTRY_UNCOMP: usize = 32 << 20;
const MAX_ARCHIVE_UNCOMP: usize = 128 << 20;
const MAX_DEPTH: u8 = 2;

#[derive(Clone, Copy, Debug)]
struct EntryMeta {
    name_range: (usize, usize),
    method: u16,
    flags: u16,
    comp_size: u64,
    uncomp_size: u64,
    local_offset: u64,
    prio: u8,
    role: Role,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    DocXml,
    NestedZip,
    PlainStream,
    SkipBin,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Container {
    Doc,
    Archive,
}

/// Per-worker reusable buffers and `zlib-rs` SIMD inflater.
pub(crate) struct ZipScratch {
    inflate: Decompress,
    out: Vec<u8>,
    xml: Vec<u8>,
    cd: Vec<u8>,
    raw: Vec<u8>,
    nested: Vec<u8>,
    entries: Vec<EntryMeta>,
}

impl std::fmt::Debug for ZipScratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ZipScratch")
    }
}

impl Default for ZipScratch {
    fn default() -> Self {
        Self {
            inflate: Decompress::new(false),
            out: Vec::new(),
            xml: Vec::new(),
            cd: Vec::new(),
            raw: Vec::new(),
            nested: Vec::new(),
            entries: Vec::new(),
        }
    }
}

struct Tally {
    hits: Vec<Hit>,
    more: bool,
    text_seen: bool,
    bin_skipped: bool,
    uncomp_spent: usize,
}

impl ZipScratch {
    /// Search a `PK\x03\x04` file. When `eof` is true, `head` already holds the
    /// entire file in memory and zero extra syscalls are made.
    pub(crate) fn search_file(
        &mut self,
        file: &mut File,
        file_len: u64,
        head: &[u8],
        eof: bool,
        pattern: &Pattern,
        opts: &Options,
    ) -> (Outcome, u64) {
        let mut tally = Tally {
            hits: Vec::new(),
            more: false,
            text_seen: false,
            bin_skipped: false,
            uncomp_spent: 0,
        };
        let extra = if eof {
            self.scan_slice(head, None, 0, pattern, opts, &mut tally);
            0
        } else {
            self.scan_large_file(file, file_len, head, pattern, opts, &mut tally)
        };
        let outcome = if !tally.hits.is_empty() || tally.more {
            Outcome::Found {
                hits: tally.hits,
                more: tally.more,
            }
        } else if !tally.text_seen && tally.bin_skipped && !opts.binary {
            Outcome::Skipped(Skip::Binary)
        } else {
            Outcome::Clean
        };
        (outcome, extra)
    }

    fn scan_slice(
        &mut self,
        archive: &[u8],
        prefix: Option<&str>,
        depth: u8,
        pattern: &Pattern,
        opts: &Options,
        tally: &mut Tally,
    ) {
        let mut entries = if depth == 0 {
            let mut v = std::mem::take(&mut self.entries);
            v.clear();
            v
        } else {
            Vec::new()
        };
        let names_src = parse_cd_from_slice(archive, &mut entries)
            .unwrap_or_else(|| parse_local_headers(archive, &mut entries));
        let kind = triage(&mut entries, names_src, opts.binary);
        for e in &entries {
            if tally.more || tally.uncomp_spent >= MAX_ARCHIVE_UNCOMP {
                break;
            }
            if e.role == Role::SkipBin {
                tally.bin_skipped = true;
                continue;
            }
            let Some(comp) = slice_entry_data(archive, e) else {
                continue;
            };
            let name = std::str::from_utf8(&names_src[e.name_range.0..e.name_range.1]).unwrap_or("");
            let joined: String;
            let label = match (prefix, kind) {
                (Some(p), Container::Doc) => Some(p),
                (Some(p), Container::Archive) => {
                    joined = format!("{p} :: {name}");
                    Some(joined.as_str())
                }
                (None, Container::Doc) => None,
                (None, Container::Archive) => Some(name),
            };
            self.scan_entry(comp, e, label, depth, pattern, opts, tally);
        }
        if depth == 0 {
            self.entries = entries;
        }
    }

    fn scan_large_file(
        &mut self,
        file: &mut File,
        file_len: u64,
        head: &[u8],
        pattern: &Pattern,
        opts: &Options,
        tally: &mut Tally,
    ) -> u64 {
        let mut read_bytes = 0u64;
        self.entries.clear();
        if !load_file_cd(file, file_len, &mut self.cd, &mut self.entries, &mut read_bytes) {
            return read_bytes;
        }
        let mut work = std::mem::take(&mut self.entries);
        let cd_snap = std::mem::take(&mut self.cd);
        let kind = triage(&mut work, &cd_snap, opts.binary);
        // Sort strictly forward on disk for monotonic NVMe/HDD reads, except when
        // an Office container prioritises `sharedStrings.xml` / `document.xml`.
        if kind == Container::Archive {
            work.sort_unstable_by_key(|e| e.local_offset);
        }
        for e in &work {
            if tally.more || tally.uncomp_spent >= MAX_ARCHIVE_UNCOMP {
                break;
            }
            if e.role == Role::SkipBin {
                tally.bin_skipped = true;
                continue;
            }
            if e.comp_size as usize > MAX_ENTRY_COMP || e.uncomp_size as usize > MAX_ENTRY_UNCOMP {
                continue;
            }
            let name = std::str::from_utf8(&cd_snap[e.name_range.0..e.name_range.1]).unwrap_or("");
            let label = (kind == Container::Archive).then_some(name);
            // Reuse `head` if the entry already lies inside the initial 1 MiB read!
            if let Some(comp) = slice_entry_data(head, e) {
                self.scan_entry(comp, e, label, 0, pattern, opts, tally);
                continue;
            }
            let mut raw = std::mem::take(&mut self.raw);
            if read_entry_from_file(file, file_len, e, &mut raw, &mut read_bytes) {
                self.scan_entry(&raw, e, label, 0, pattern, opts, tally);
            }
            self.raw = raw;
        }
        self.cd = cd_snap;
        self.entries = work;
        read_bytes
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_entry(
        &mut self,
        comp: &[u8],
        e: &EntryMeta,
        label: Option<&str>,
        depth: u8,
        pattern: &Pattern,
        opts: &Options,
        tally: &mut Tally,
    ) {
        if e.flags & 1 != 0 {
            return;
        }
        match e.role {
            Role::SkipBin => tally.bin_skipped = true,
            Role::DocXml => {
                tally.text_seen = true;
                let cap = MAX_ENTRY_UNCOMP.min(MAX_ARCHIVE_UNCOMP - tally.uncomp_spent);
                let mut xml_raw = std::mem::take(&mut self.out);
                let ok = inflate_whole(
                    &mut self.inflate,
                    e.method,
                    comp,
                    e.uncomp_size,
                    &mut xml_raw,
                    cap,
                );
                if ok {
                    tally.uncomp_spent += xml_raw.len();
                    if let Some(cdata) = udf_cdata(&xml_raw) {
                        scan_bytes(cdata, Kind::Text, label, pattern, opts, tally);
                    } else {
                        strip_xml(&xml_raw, &mut self.xml, cap);
                        scan_bytes(&self.xml, Kind::Text, label, pattern, opts, tally);
                    }
                }
                self.out = xml_raw;
            }
            Role::NestedZip if depth < MAX_DEPTH => {
                let cap = (16 << 20).min(MAX_ARCHIVE_UNCOMP - tally.uncomp_spent);
                if e.method == 0 {
                    if comp.len() >= 4 && comp[..4] == [0x50, 0x4B, 0x03, 0x04] {
                        tally.uncomp_spent += comp.len();
                        self.scan_slice(comp, label, depth + 1, pattern, opts, tally);
                    }
                } else if e.method == 8 {
                    let mut nested = std::mem::take(&mut self.nested);
                    if inflate_whole(
                        &mut self.inflate,
                        8,
                        comp,
                        e.uncomp_size,
                        &mut nested,
                        cap,
                    )
                        && nested.len() >= 4
                        && nested[..4] == [0x50, 0x4B, 0x03, 0x04]
                    {
                        tally.uncomp_spent += nested.len();
                        self.scan_slice(&nested, label, depth + 1, pattern, opts, tally);
                    }
                    self.nested = nested;
                }
            }
            Role::NestedZip => {}
            Role::PlainStream => {
                if e.method == 0 {
                    let kind = Kind::sniff(comp);
                    if kind == Kind::Binary && !opts.binary {
                        tally.bin_skipped = true;
                        return;
                    }
                    tally.text_seen = true;
                    tally.uncomp_spent += comp.len();
                    scan_bytes(comp, kind, label, pattern, opts, tally);
                } else if e.method == 8 {
                    self.stream_deflate(comp, label, pattern, opts, tally);
                }
            }
        }
    }

    fn stream_deflate(
        &mut self,
        comp: &[u8],
        label: Option<&str>,
        pattern: &Pattern,
        opts: &Options,
        tally: &mut Tally,
    ) {
        self.inflate.reset(false);
        if self.out.len() < WINDOW {
            self.out.resize(WINDOW, 0);
        }
        let mut in_pos = 0usize;
        let mut filled = 0usize;
        let mut base = 0u64;
        let mut lines_before = 0u64;
        let mut kind: Option<Kind> = None;

        loop {
            let mut eof = false;
            while filled < WINDOW {
                let before_in = self.inflate.total_in();
                let before_out = self.inflate.total_out();
                let status = self.inflate.decompress(
                    &comp[in_pos..],
                    &mut self.out[filled..WINDOW],
                    FlushDecompress::None,
                );
                let consumed = (self.inflate.total_in() - before_in) as usize;
                let produced = (self.inflate.total_out() - before_out) as usize;
                in_pos += consumed;
                filled += produced;
                tally.uncomp_spent += produced;
                match status {
                    Ok(Status::StreamEnd) => {
                        eof = true;
                        break;
                    }
                    Ok(_) if consumed == 0 && produced == 0 => {
                        eof = true;
                        break;
                    }
                    Ok(_) if tally.uncomp_spent >= MAX_ARCHIVE_UNCOMP => {
                        eof = true;
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
            if filled == 0 {
                break;
            }
            let hay = &self.out[..filled];
            let k = *kind.get_or_insert_with(|| Kind::sniff(hay));
            if k == Kind::Binary && !opts.binary {
                tally.bin_skipped = true;
                return;
            }
            tally.text_seen = true;
            let cut = if eof {
                filled
            } else {
                let floor = filled.saturating_sub(KEEP);
                match memrchr(b'\n', hay) {
                    Some(i) if k == Kind::Text && i + 1 >= floor => i + 1,
                    _ => floor,
                }
            };
            collect_in_chunk(hay, cut, eof, base, lines_before, k, label, pattern, opts, tally);
            if eof || tally.more {
                break;
            }
            if k == Kind::Text {
                lines_before += memchr_iter(b'\n', &hay[..cut]).count() as u64;
            }
            self.out.copy_within(cut..filled, 0);
            filled -= cut;
            base += cut as u64;
        }
    }
}

fn inflate_whole(
    inflate: &mut Decompress,
    method: u16,
    comp: &[u8],
    uncomp_hint: u64,
    out: &mut Vec<u8>,
    cap: usize,
) -> bool {
    out.clear();
    match method {
        0 => {
            if comp.len() > cap {
                return false;
            }
            out.extend_from_slice(comp);
            true
        }
        8 => {
            inflate.reset(false);
            let want = usize::try_from(uncomp_hint)
                .ok()
                .filter(|&n| n > 0 && n <= cap);
            if let Some(exact) = want {
                // Single-call SIMD fast-path (`inflate_fast` with `Finish`): avoids
                // 32 KiB sliding-window copies inside `zlib-rs`.
                let need = (exact + 64).max(64 << 10).min(cap + 64);
                if out.capacity() < need {
                    out.reserve(need - out.len());
                }
                if let Ok(Status::StreamEnd) =
                    inflate.decompress_vec(comp, out, FlushDecompress::Finish)
                {
                    if out.len() > cap {
                        out.truncate(cap);
                    }
                    return true;
                }
                out.clear();
                inflate.reset(false);
            }
            let init = comp.len().saturating_mul(3).clamp(64 << 10, cap.min(1 << 20));
            if out.capacity() < init {
                out.reserve(init - out.len());
            }
            let mut in_pos = 0usize;
            loop {
                if out.len() >= cap {
                    return true;
                }
                if out.capacity() == out.len() {
                    let add = out.len().max(32 << 10).min(cap - out.len());
                    out.reserve(add);
                }
                let before_in = inflate.total_in();
                let status =
                    inflate.decompress_vec(&comp[in_pos..], out, FlushDecompress::None);
                let consumed = (inflate.total_in() - before_in) as usize;
                in_pos += consumed;
                match status {
                    Ok(Status::StreamEnd) => return true,
                    Ok(_) if consumed == 0 && out.len() == out.capacity() => continue,
                    Ok(_) if consumed == 0 => return !out.is_empty(),
                    Ok(_) => {}
                    Err(_) => return false,
                }
            }
        }
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_in_chunk(
    hay: &[u8],
    cut: usize,
    eof: bool,
    base: u64,
    lines_before: u64,
    kind: Kind,
    label: Option<&str>,
    pattern: &Pattern,
    opts: &Options,
    tally: &mut Tally,
) {
    let regex = pattern.reading(kind);
    let (mut counted_to, mut nl_count) = (0usize, lines_before);
    for found in regex.find_iter(hay) {
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
        let line = if kind == Kind::Text {
            nl_count += memchr_iter(b'\n', &hay[counted_to..start]).count() as u64;
            counted_to = start;
            Some(nl_count + 1)
        } else {
            None
        };
        if line.is_some()
            && label.is_none()
            && tally.hits.last().is_some_and(|last| last.line == line)
        {
            continue;
        }
        if opts.hits_per_file == 0 || tally.hits.len() >= opts.hits_per_file {
            tally.more = true;
            if opts.hits_per_file == 0 {
                tally.hits.push(Hit {
                    offset: base + start as u64,
                    line,
                    text: String::new(),
                    at: (0, 0),
                });
                tally.more = false;
            }
            break;
        }
        let (mut text, mut at) = around(hay, start, end, kind, opts.excerpt);
        if let Some(lbl) = label {
            let pfx = format!("[{lbl}] ");
            at.0 += pfx.len() as u32;
            text.insert_str(0, &pfx);
        }
        tally.hits.push(Hit {
            offset: base + start as u64,
            line,
            text,
            at,
        });
    }
}

fn scan_bytes(
    hay: &[u8],
    kind: Kind,
    label: Option<&str>,
    pattern: &Pattern,
    opts: &Options,
    tally: &mut Tally,
) {
    collect_in_chunk(hay, hay.len(), true, 0, 0, kind, label, pattern, opts, tally);
}

fn triage(entries: &mut Vec<EntryMeta>, names: &[u8], allow_bin: bool) -> Container {
    let name_of = |e: &EntryMeta| &names[e.name_range.0..e.name_range.1];
    let has_docx = entries.iter().any(|e| name_of(e) == b"word/document.xml");
    let has_xlsx = entries
        .iter()
        .any(|e| name_of(e) == b"xl/workbook.xml" || name_of(e) == b"xl/sharedStrings.xml");
    let has_pptx = entries
        .iter()
        .any(|e| name_of(e) == b"ppt/presentation.xml" || name_of(e).starts_with(b"ppt/slides/"));
    let has_odf_udf = !has_docx
        && !has_xlsx
        && !has_pptx
        && entries.iter().any(|e| name_of(e) == b"content.xml")
        && (entries.len() <= 4
            || entries
                .iter()
                .any(|e| name_of(e) == b"mimetype" || name_of(e) == b"META-INF/manifest.xml"));

    if has_docx || has_xlsx || has_pptx || has_odf_udf {
        entries.retain_mut(|e| {
            let n = name_of(e);
            let keep = if has_docx {
                if n == b"word/document.xml" {
                    e.prio = 0;
                    true
                } else if n.starts_with(b"word/")
                    && n.ends_with(b".xml")
                    && (n.starts_with(b"word/header")
                        || n.starts_with(b"word/footer")
                        || n == b"word/footnotes.xml"
                        || n == b"word/endnotes.xml"
                        || n == b"word/comments.xml")
                {
                    e.prio = 1;
                    true
                } else {
                    false
                }
            } else if has_xlsx {
                if n == b"xl/sharedStrings.xml" {
                    e.prio = 0;
                    true
                } else if n.starts_with(b"xl/worksheets/sheet") && n.ends_with(b".xml") {
                    e.prio = 1;
                    true
                } else {
                    false
                }
            } else if has_pptx {
                if n.starts_with(b"ppt/slides/slide") && n.ends_with(b".xml") {
                    e.prio = 0;
                    true
                } else if n.starts_with(b"ppt/notesSlides/notesSlide") && n.ends_with(b".xml") {
                    e.prio = 1;
                    true
                } else {
                    false
                }
            } else {
                e.prio = 0;
                n == b"content.xml"
            };
            if keep {
                e.role = Role::DocXml;
            }
            keep
        });
        entries.sort_by_key(|e| (e.prio, e.local_offset));
        return Container::Doc;
    }

    entries.retain_mut(|e| {
        let n = name_of(e);
        if n.is_empty() || n.ends_with(b"/") {
            return false;
        }
        e.role = classify_archive_entry(n, allow_bin);
        true
    });
    Container::Archive
}

fn classify_archive_entry(name: &[u8], allow_bin: bool) -> Role {
    let leaf = match memrchr(b'/', name) {
        Some(i) => &name[i + 1..],
        None => name,
    };
    let ext = match memrchr(b'.', leaf) {
        Some(i) if i > 0 => &leaf[i + 1..],
        _ => return Role::PlainStream,
    };
    if ext.len() > 12 {
        return Role::PlainStream;
    }
    let mut low = [0u8; 12];
    for (d, &s) in low.iter_mut().zip(ext) {
        *d = s.to_ascii_lowercase();
    }
    let ext = &low[..ext.len()];
    match ext {
        b"udf" | b"docx" | b"xlsx" | b"pptx" | b"odt" | b"ods" | b"odp" | b"epub" | b"zip"
        | b"jar" | b"whl" => Role::NestedZip,
        b"xhtml" | b"html" | b"htm" | b"opf" | b"ncx" => Role::DocXml,
        b"png" | b"jpg" | b"jpeg" | b"gif" | b"webp" | b"avif" | b"bmp" | b"ico" | b"tif"
        | b"tiff" | b"mp3" | b"mp4" | b"m4a" | b"mkv" | b"avi" | b"mov" | b"webm" | b"wav"
        | b"flac" | b"ogg" | b"opus" | b"woff" | b"woff2" | b"ttf" | b"otf" | b"eot" | b"so"
        | b"dll" | b"dylib" | b"exe" | b"o" | b"obj" | b"a" | b"lib" | b"rlib" | b"rmeta"
        | b"pdb" | b"class" | b"dex" | b"pyc" | b"wasm" | b"7z" | b"rar" | b"gz" | b"bz2"
        | b"xz" | b"zst" | b"lz4" | b"iso" | b"img" | b"dmg" | b"pdf" | b"db" | b"sqlite"
        | b"parquet"
            if !allow_bin =>
        {
            Role::SkipBin
        }
        _ => Role::PlainStream,
    }
}

fn find_eocd(tail: &[u8]) -> Option<(usize, u64, usize)> {
    if tail.len() < 22 {
        return None;
    }
    for p in memrchr_iter(b'P', &tail[..=tail.len() - 22]) {
        if tail[p..p + 4] == [0x50, 0x4B, 0x05, 0x06] {
            let count = u16::from_le_bytes([tail[p + 10], tail[p + 11]]) as usize;
            let cd_size = u32::from_le_bytes(tail[p + 12..p + 16].try_into().ok()?) as usize;
            let cd_off = u32::from_le_bytes(tail[p + 16..p + 20].try_into().ok()?) as u64;
            if cd_off == 0xFFFF_FFFF || cd_size == 0xFFFF_FFFF {
                return None;
            }
            return Some((count, cd_off, cd_size));
        }
    }
    None
}

fn parse_cd_bytes(cd: &[u8], base_in_buf: usize, max_count: usize, out: &mut Vec<EntryMeta>) {
    let mut at = 0usize;
    while at + 46 <= cd.len() && out.len() < max_count {
        if cd[at..at + 4] != [0x50, 0x4B, 0x01, 0x02] {
            break;
        }
        let flags = u16::from_le_bytes([cd[at + 8], cd[at + 9]]);
        let method = u16::from_le_bytes([cd[at + 10], cd[at + 11]]);
        let comp_size = u32::from_le_bytes([cd[at + 20], cd[at + 21], cd[at + 22], cd[at + 23]]) as u64;
        let uncomp_size = u32::from_le_bytes([cd[at + 24], cd[at + 25], cd[at + 26], cd[at + 27]]) as u64;
        let nlen = u16::from_le_bytes([cd[at + 28], cd[at + 29]]) as usize;
        let elen = u16::from_le_bytes([cd[at + 30], cd[at + 31]]) as usize;
        let clen = u16::from_le_bytes([cd[at + 32], cd[at + 33]]) as usize;
        let local_offset = u32::from_le_bytes([cd[at + 42], cd[at + 43], cd[at + 44], cd[at + 45]]) as u64;
        let Some(name_end) = at.checked_add(46).and_then(|x| x.checked_add(nlen)) else {
            break;
        };
        let Some(next) = name_end.checked_add(elen).and_then(|x| x.checked_add(clen)) else {
            break;
        };
        if next > cd.len() {
            break;
        }
        out.push(EntryMeta {
            name_range: (base_in_buf + at + 46, base_in_buf + name_end),
            method,
            flags,
            comp_size,
            uncomp_size,
            local_offset,
            prio: 2,
            role: Role::PlainStream,
        });
        at = next;
    }
}

fn parse_cd_from_slice<'a>(archive: &'a [u8], out: &mut Vec<EntryMeta>) -> Option<&'a [u8]> {
    let tail_start = archive.len().saturating_sub(65_557);
    let (count, cd_off, cd_size) = find_eocd(&archive[tail_start..])?;
    let cd_off = cd_off as usize;
    let cd_end = cd_off.checked_add(cd_size)?;
    if cd_size > MAX_CD || cd_end > archive.len() {
        return None;
    }
    parse_cd_bytes(&archive[cd_off..cd_end], cd_off, count, out);
    (!out.is_empty()).then_some(archive)
}

fn parse_local_headers<'a>(archive: &'a [u8], out: &mut Vec<EntryMeta>) -> &'a [u8] {
    let mut at = 0usize;
    while at + 30 <= archive.len() {
        if archive[at..at + 4] != [0x50, 0x4B, 0x03, 0x04] {
            break;
        }
        let flags = u16::from_le_bytes([archive[at + 6], archive[at + 7]]);
        let method = u16::from_le_bytes([archive[at + 8], archive[at + 9]]);
        let comp_size =
            u32::from_le_bytes([archive[at + 18], archive[at + 19], archive[at + 20], archive[at + 21]]) as u64;
        let uncomp_size =
            u32::from_le_bytes([archive[at + 22], archive[at + 23], archive[at + 24], archive[at + 25]]) as u64;
        let nlen = u16::from_le_bytes([archive[at + 26], archive[at + 27]]) as usize;
        let elen = u16::from_le_bytes([archive[at + 28], archive[at + 29]]) as usize;
        let Some(data_start) = at.checked_add(30).and_then(|x| x.checked_add(nlen)).and_then(|x| x.checked_add(elen)) else {
            break;
        };
        if data_start > archive.len() {
            break;
        }
        let avail = (archive.len() - data_start) as u64;
        let eff_comp = if comp_size == 0 && flags & 8 != 0 { avail } else { comp_size };
        out.push(EntryMeta {
            name_range: (at + 30, at + 30 + nlen),
            method,
            flags,
            comp_size: eff_comp,
            uncomp_size,
            local_offset: at as u64,
            prio: 2,
            role: Role::PlainStream,
        });
        if comp_size == 0 {
            break;
        }
        let Some(next) = data_start.checked_add(comp_size as usize) else {
            break;
        };
        at = next;
    }
    archive
}

fn slice_entry_data<'a>(buf: &'a [u8], e: &EntryMeta) -> Option<&'a [u8]> {
    let loc = usize::try_from(e.local_offset).ok()?;
    let hdr = buf.get(loc..loc.checked_add(30)?)?;
    if hdr[..4] != [0x50, 0x4B, 0x03, 0x04] {
        return None;
    }
    let nlen = u16::from_le_bytes([hdr[26], hdr[27]]) as usize;
    let elen = u16::from_le_bytes([hdr[28], hdr[29]]) as usize;
    let start = loc.checked_add(30)?.checked_add(nlen)?.checked_add(elen)?;
    let end = start.checked_add(usize::try_from(e.comp_size).ok()?)?;
    buf.get(start..end)
}

fn read_at_exact(file: &mut File, offset: u64, dst: &mut [u8]) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt as _;
        file.read_exact_at(dst, offset).is_ok()
    }
    #[cfg(not(unix))]
    {
        file.seek(SeekFrom::Start(offset)).is_ok() && file.read_exact(dst).is_ok()
    }
}

fn load_file_cd(
    file: &mut File,
    file_len: u64,
    cd_buf: &mut Vec<u8>,
    entries: &mut Vec<EntryMeta>,
    read_bytes: &mut u64,
) -> bool {
    let tail_len = file_len.min(65_557) as usize;
    if tail_len < 22 {
        return false;
    }
    cd_buf.resize(tail_len, 0);
    let tail_off = file_len - tail_len as u64;
    if !read_at_exact(file, tail_off, cd_buf) {
        return false;
    }
    *read_bytes += tail_len as u64;
    let Some((count, cd_off, cd_size)) = find_eocd(cd_buf) else {
        return false;
    };
    if cd_size > MAX_CD || cd_off + cd_size as u64 > file_len {
        return false;
    }
    if cd_off >= tail_off {
        let rel = (cd_off - tail_off) as usize;
        let Some(end) = rel.checked_add(cd_size) else {
            return false;
        };
        if end <= cd_buf.len() {
            parse_cd_bytes(&cd_buf[rel..end], rel, count, entries);
            return !entries.is_empty();
        }
    }
    cd_buf.resize(cd_size, 0);
    if !read_at_exact(file, cd_off, cd_buf) {
        return false;
    }
    *read_bytes += cd_size as u64;
    parse_cd_bytes(cd_buf, 0, count, entries);
    !entries.is_empty()
}

fn read_entry_from_file(
    file: &mut File,
    file_len: u64,
    e: &EntryMeta,
    raw: &mut Vec<u8>,
    read_bytes: &mut u64,
) -> bool {
    if e.local_offset + 30 > file_len {
        return false;
    }
    let mut lhdr = [0u8; 30];
    if !read_at_exact(file, e.local_offset, &mut lhdr) || lhdr[..4] != [0x50, 0x4B, 0x03, 0x04] {
        return false;
    }
    *read_bytes += 30;
    let nlen = u16::from_le_bytes([lhdr[26], lhdr[27]]) as u64;
    let elen = u16::from_le_bytes([lhdr[28], lhdr[29]]) as u64;
    let data_off = e.local_offset + 30 + nlen + elen;
    if data_off.saturating_add(e.comp_size) > file_len {
        return false;
    }
    let len = e.comp_size as usize;
    raw.resize(len, 0);
    if !read_at_exact(file, data_off, raw) {
        return false;
    }
    *read_bytes += e.comp_size;
    true
}
