//! Zero-allocation SIMD streaming search inside ZIP-based documents (`.udf`,
//! `.docx`, `.xlsx`, `.pptx`, `.odt`, `.epub`) and general `.zip` archives,
//! including nested documents inside `.zip` files.

use std::fs::File;

use flate2::{Decompress, FlushDecompress, Status};
use memchr::{memchr_iter, memrchr};
use scour_extract::zip::{self, Entry};
use scour_extract::{attribute_values, inflate_whole, strip_xml, udf_cdata};

use crate::excerpt::{Hit, Kind, around};
use crate::pattern::Pattern;
use crate::scan::{Options, Outcome, Skip};

const WINDOW: usize = 256 << 10;
const KEEP: usize = 32 << 10;
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
    /// The document's whole running text, one entry of it (a word processor's
    /// body): a find's place in it is its place in the document.
    body: bool,
}

impl EntryMeta {
    /// A directory's entry, not yet sorted or given a role.
    #[inline]
    fn of(e: Entry) -> EntryMeta {
        EntryMeta {
            name_range: e.name,
            method: e.method,
            flags: e.flags,
            comp_size: e.comp_size,
            uncomp_size: e.uncomp_size,
            local_offset: e.local_offset,
            prio: 2,
            role: Role::PlainStream,
            body: false,
        }
    }

    /// Where its bytes are, as the reader wants it.
    #[inline]
    fn at(&self) -> Entry {
        Entry {
            name: self.name_range,
            method: self.method,
            flags: self.flags,
            comp_size: self.comp_size,
            uncomp_size: self.uncomp_size,
            local_offset: self.local_offset,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    DocXml,
    /// A workbook's directory: its sheets' names are attributes.
    SheetNames,
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
    /// entire file in memory and zero extra syscalls are made; `file` is only
    /// read when it is not.
    pub(crate) fn search_file(
        &mut self,
        file: Option<&mut File>,
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
        let extra = match file {
            Some(file) if !eof => {
                self.scan_large_file(file, file_len, head, pattern, opts, &mut tally)
            }
            _ => {
                self.scan_slice(head, None, 0, pattern, opts, &mut tally);
                0
            }
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
        if !zip::central_in(archive, |e| entries.push(EntryMeta::of(e))) {
            zip::local_headers(archive, |e| entries.push(EntryMeta::of(e)));
        }
        let names_src = archive;
        let kind = triage(&mut entries, names_src, opts.binary, opts.markup);
        for e in &entries {
            if tally.more || tally.uncomp_spent >= MAX_ARCHIVE_UNCOMP {
                break;
            }
            if e.role == Role::SkipBin {
                tally.bin_skipped = true;
                continue;
            }
            let Some(comp) = zip::data_in(archive, &e.at()) else {
                continue;
            };
            let name =
                std::str::from_utf8(&names_src[e.name_range.0..e.name_range.1]).unwrap_or("");
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
        let entries = &mut self.entries;
        if !zip::load_central(file, file_len, &mut self.cd, &mut read_bytes, |e| {
            entries.push(EntryMeta::of(e))
        }) {
            return read_bytes;
        }
        let mut work = std::mem::take(&mut self.entries);
        let cd_snap = std::mem::take(&mut self.cd);
        let kind = triage(&mut work, &cd_snap, opts.binary, opts.markup);
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
            if let Some(comp) = zip::data_in(head, &e.at()) {
                self.scan_entry(comp, e, label, 0, pattern, opts, tally);
                continue;
            }
            let mut raw = std::mem::take(&mut self.raw);
            if zip::read_raw(file, file_len, &e.at(), &mut raw, &mut read_bytes) {
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
            Role::SheetNames => {
                let mut xml_raw = std::mem::take(&mut self.out);
                if inflate_whole(
                    &mut self.inflate,
                    e.method,
                    comp,
                    e.uncomp_size,
                    &mut xml_raw,
                    1 << 20,
                ) {
                    attribute_values(&xml_raw, b"sheet", b"name", &mut self.xml, 64 << 10);
                    scan_bytes(&self.xml, Kind::Text, label, pattern, opts, tally);
                }
                self.out = xml_raw;
            }
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
                    let from = tally.hits.len();
                    let text_len = if let Some(cdata) = udf_cdata(&xml_raw) {
                        scan_bytes(cdata, Kind::Text, label, pattern, opts, tally);
                        cdata.len()
                    } else {
                        strip_xml(&xml_raw, &mut self.xml, cap);
                        scan_bytes(&self.xml, Kind::Text, label, pattern, opts, tally);
                        self.xml.len()
                    };
                    if e.body && depth == 0 {
                        for hit in &mut tally.hits[from..] {
                            hit.text_len = Some(text_len as u64);
                        }
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
                    if inflate_whole(&mut self.inflate, 8, comp, e.uncomp_size, &mut nested, cap)
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
            collect_in_chunk(
                hay,
                cut,
                eof,
                base,
                lines_before,
                k,
                label,
                pattern,
                opts,
                tally,
            );
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
    let (mut counted_to, mut nl_count) = (0usize, lines_before);
    for (start, end) in pattern.finds(kind, hay) {
        if start >= cut && !eof {
            break;
        }
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
                    marks: Vec::new(),
                    text_len: None,
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
            marks: vec![at],
            text_len: None,
        });
    }
}

/// Text already in memory — a document's, taken out of it — searched whole.
pub(crate) fn search_whole(text: &[u8], pattern: &Pattern, opts: &Options) -> Outcome {
    let mut tally = Tally {
        hits: Vec::new(),
        more: false,
        text_seen: true,
        bin_skipped: false,
        uncomp_spent: 0,
    };
    scan_bytes(text, Kind::Text, None, pattern, opts, &mut tally);
    for hit in &mut tally.hits {
        hit.text_len = Some(text.len() as u64);
    }
    if tally.hits.is_empty() && !tally.more {
        Outcome::Clean
    } else {
        Outcome::Found {
            hits: tally.hits,
            more: tally.more,
        }
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
    collect_in_chunk(
        hay,
        hay.len(),
        true,
        0,
        0,
        kind,
        label,
        pattern,
        opts,
        tally,
    );
}

/// `markup`: a document is an archive like any other — each of its parts
/// searched as the XML it is, tags and all — for somebody looking for
/// the markup itself.
fn triage(entries: &mut Vec<EntryMeta>, names: &[u8], allow_bin: bool, markup: bool) -> Container {
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

    if !markup && (has_docx || has_xlsx || has_pptx || has_odf_udf) {
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
                        || n == b"word/comments.xml"
                        || n.starts_with(b"word/charts/chart"))
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
                } else if (n.starts_with(b"xl/worksheets/sheet")
                    // Sheet names, cell comments, and the text of charts.
                    || n == b"xl/workbook.xml"
                    || n.starts_with(b"xl/comments")
                    || n.starts_with(b"xl/threadedComments/")
                    || n.starts_with(b"xl/charts/chart"))
                    && n.ends_with(b".xml")
                {
                    e.prio = 1;
                    true
                } else {
                    false
                }
            } else if has_pptx {
                if n.starts_with(b"ppt/slides/slide") && n.ends_with(b".xml") {
                    e.prio = 0;
                    true
                } else if (n.starts_with(b"ppt/notesSlides/notesSlide")
                    || n.starts_with(b"ppt/charts/chart")
                    || n.starts_with(b"ppt/comments/"))
                    && n.ends_with(b".xml")
                {
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
                e.body = n == b"word/document.xml" || (has_odf_udf && n == b"content.xml");
                e.role = if n == b"xl/workbook.xml" {
                    Role::SheetNames
                } else {
                    Role::DocXml
                };
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
        e.role = match classify_archive_entry(n, allow_bin) {
            Role::DocXml if markup => Role::PlainStream,
            role => role,
        };
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
