//! A document's text, for showing: `.docx`, `.odt`, `.pptx`, `.xlsx` and UYAP
//! `.udf`, read in process in milliseconds, without a converter.

use crate::rtf::rtf_text;
use std::fs::File;
use std::path::Path;

use flate2::Decompress;

use crate::zip::{self, Entry};
use crate::{inflate_whole, strip_xml, udf_cdata};

/// A larger archive is left to a converter.
const MAX_ARCHIVE: u64 = 64 << 20;
/// No one entry is inflated past this.
const MAX_ENTRY: usize = 8 << 20;
/// A spreadsheet's first sheets only.
const SHEETS: usize = 8;

/// Where a document's text goes, in order, as it is read.
pub trait Sink {
    fn text(&mut self, chunk: &str);

    /// A part's title — a slide's number. Plain text unless the sink cares.
    fn heading(&mut self, text: &str) {
        self.text(text);
    }

    /// A page, slide or sheet begins: its number, from one.
    fn boundary(&mut self, _ordinal: u32) {}

    /// Nothing more is wanted; reading stops at the next part.
    fn full(&self) -> bool {
        false
    }
}

/// Text kept up to a budget of bytes, cut at a character's edge.
#[derive(Debug, Default)]
pub struct Text {
    out: String,
    budget: usize,
    cut: bool,
}

impl Text {
    pub fn new(budget: usize) -> Text {
        Text {
            out: String::new(),
            budget,
            cut: false,
        }
    }

    /// The text, trimmed, and whether the budget cut it.
    pub fn finish(self) -> (String, bool) {
        (self.out.trim().to_owned(), self.cut)
    }
}

impl Sink for Text {
    fn text(&mut self, chunk: &str) {
        if self.cut {
            return;
        }
        let room = self.budget.saturating_sub(self.out.len());
        if chunk.len() <= room {
            self.out.push_str(chunk);
            return;
        }
        let mut at = room;
        while !chunk.is_char_boundary(at) {
            at -= 1;
        }
        self.out.push_str(&chunk[..at]);
        self.cut = true;
    }

    fn full(&self) -> bool {
        self.cut
    }
}

/// The text of a document, at most `budget` bytes, and whether it was cut.
/// `path` names the kind; `file` is it, opened however the caller opens
/// files. None for a kind not read here, a damaged file, or one with no text.
pub fn document_text(path: &Path, file: File, budget: usize) -> Option<(String, bool)> {
    let mut text = Text::new(budget);
    document(path, file, &mut text)?;
    let (out, cut) = text.finish();
    (!out.is_empty()).then_some((out, cut))
}

/// The kinds [`document`] reads, by extension.
pub fn reads_document(path: &Path) -> bool {
    kind_of(path).is_some()
}

fn kind_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    ["docx", "odt", "ods", "odp", "pptx", "xlsx", "udf", "rtf"]
        .into_iter()
        .find(|k| *k == ext)
}

/// RTF read whole: it is text with its markup inline, no container.
const MAX_RTF: u64 = 32 << 20;

/// A document's text into `sink`, part by part. None when it cannot be read.
pub fn document(path: &Path, mut file: File, sink: &mut dyn Sink) -> Option<()> {
    let kind = kind_of(path)?;
    let len = file.metadata().ok()?.len();
    if kind == "rtf" {
        if len > MAX_RTF {
            return None;
        }
        let mut bytes = Vec::with_capacity(len as usize);
        std::io::Read::read_to_end(&mut file, &mut bytes).ok()?;
        let mut words = String::new();
        if !rtf_text(&bytes, &mut words, MAX_RTF as usize) {
            return None;
        }
        sink.text(&words);
        return Some(());
    }
    if !(22..=MAX_ARCHIVE).contains(&len) {
        return None;
    }
    let mut cd = Vec::new();
    let mut entries = Vec::new();
    let mut read = 0;
    if !zip::load_central(&mut file, len, &mut cd, &mut read, |e| entries.push(e)) {
        return None;
    }
    let mut doc = Doc {
        file,
        len,
        cd,
        entries,
        inflate: Decompress::new(false),
        raw: Vec::new(),
    };
    match kind {
        "docx" => plain(&doc.named(b"word/document.xml")?, sink),
        // A text, a sheet or a slide show: OpenDocument keeps all three in one file.
        "odt" | "ods" | "odp" => plain(&doc.named(b"content.xml")?, sink),
        "udf" => {
            let xml = doc.named(b"content.xml")?;
            match udf_cdata(&xml) {
                Some(body) => sink.text(&String::from_utf8_lossy(body)),
                None => plain(&xml, sink),
            }
        }
        "pptx" => slides(&mut doc, sink),
        "xlsx" => sheets(&mut doc, sink),
        _ => return None,
    }
    Some(())
}

struct Doc {
    file: File,
    len: u64,
    cd: Vec<u8>,
    entries: Vec<Entry>,
    inflate: Decompress,
    raw: Vec<u8>,
}

impl Doc {
    fn named(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        let e = *self.entries.iter().find(|e| e.name_in(&self.cd) == name)?;
        self.inflated(&e)
    }

    fn inflated(&mut self, e: &Entry) -> Option<Vec<u8>> {
        if e.encrypted() || e.comp_size as usize > MAX_ENTRY || e.uncomp_size as usize > MAX_ENTRY {
            return None;
        }
        let mut read = 0;
        if !zip::read_raw(&mut self.file, self.len, e, &mut self.raw, &mut read) {
            return None;
        }
        let mut out = Vec::new();
        inflate_whole(
            &mut self.inflate,
            e.method,
            &self.raw,
            e.uncomp_size,
            &mut out,
            MAX_ENTRY,
        )
        .then_some(out)
    }

    /// The entries whose names start and end so, in the order `key` gives.
    fn parts<K: Ord>(&self, start: &[u8], end: &[u8], key: impl Fn(&str) -> K) -> Vec<Entry> {
        let mut parts: Vec<(K, Entry)> = self
            .entries
            .iter()
            .filter_map(|e| {
                let n = e.name_in(&self.cd);
                (n.starts_with(start) && n.ends_with(end))
                    .then(|| (key(&String::from_utf8_lossy(n)), *e))
            })
            .collect();
        parts.sort_by(|a, b| a.0.cmp(&b.0));
        parts.into_iter().map(|(_, e)| e).collect()
    }
}

fn plain(xml: &[u8], sink: &mut dyn Sink) {
    let mut out = Vec::new();
    strip_xml(xml, &mut out, MAX_ENTRY);
    sink.text(&String::from_utf8_lossy(&out));
}

/// Slides in their own order — `slide10` after `slide9` — each under its number.
fn slides(doc: &mut Doc, sink: &mut dyn Sink) {
    let slides = doc.parts(b"ppt/slides/slide", b".xml", |name| {
        name.trim_start_matches("ppt/slides/slide")
            .trim_end_matches(".xml")
            .parse::<u32>()
            .unwrap_or(0)
    });
    for (i, slide) in slides.iter().enumerate() {
        if sink.full() {
            break;
        }
        let Some(xml) = doc.inflated(slide) else {
            continue;
        };
        let n = i as u32 + 1;
        sink.boundary(n);
        let lead = if i == 0 { "" } else { "\n\n" };
        sink.heading(&format!("{lead}— {n} —\n"));
        plain(&xml, sink);
    }
}

/// The first sheets, a row a line and a cell a tab, shared strings resolved.
fn sheets(doc: &mut Doc, sink: &mut dyn Sink) {
    let shared = doc
        .named(b"xl/sharedStrings.xml")
        .map(|xml| shared_strings(&String::from_utf8_lossy(&xml)))
        .unwrap_or_default();
    let sheets = doc.parts(b"xl/worksheets/sheet", b".xml", str::to_owned);
    for (i, sheet) in sheets.iter().take(SHEETS).enumerate() {
        if sink.full() {
            break;
        }
        if let Some(xml) = doc.inflated(sheet) {
            sink.boundary(i as u32 + 1);
            rows(&String::from_utf8_lossy(&xml), &shared, sink);
        }
    }
}

fn shared_strings(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<si") {
        let after = &rest[start..];
        let Some(end) = after.find("</si>") else {
            break;
        };
        let mut cell = Vec::new();
        strip_xml(&after.as_bytes()[..end], &mut cell, MAX_ENTRY);
        out.push(String::from_utf8_lossy(&cell).into_owned());
        rest = &after[end + 5..];
    }
    out
}

fn rows(xml: &str, shared: &[String], sink: &mut dyn Sink) {
    let mut rest = xml;
    while let Some(rstart) = rest.find("<row") {
        if sink.full() {
            break;
        }
        let after = &rest[rstart..];
        let Some(rend) = after.find("</row>") else {
            break;
        };
        let mut cols = Vec::new();
        let mut c_rest = &after[..rend];
        while let Some(cstart) = c_rest.find("<c ") {
            let cafter = &c_rest[cstart..];
            let Some(cend) = cafter.find("</c>") else {
                break;
            };
            let cell = &cafter[..cend];
            if let Some(vstart) = cell.find("<v>")
                && let Some(vend) = cell[vstart + 3..].find("</v>")
            {
                let raw = &cell[vstart + 3..vstart + 3 + vend];
                let val = if cell.contains("t=\"s\"") {
                    raw.parse::<usize>()
                        .ok()
                        .and_then(|i| shared.get(i))
                        .map_or("", String::as_str)
                } else {
                    raw
                };
                if !val.is_empty() {
                    cols.push(val.to_owned());
                }
            } else if let Some(istart) = cell.find("<is>")
                && let Some(iend) = cell[istart..].find("</is>")
            {
                // A string written in the cell itself, `t="inlineStr"`: what
                // writers other than Excel do, and half the sheets found here.
                let mut text = Vec::new();
                strip_xml(
                    &cell.as_bytes()[istart..istart + iend],
                    &mut text,
                    MAX_ENTRY,
                );
                if !text.is_empty() {
                    cols.push(String::from_utf8_lossy(&text).into_owned());
                }
            }
            c_rest = &cafter[cend + 4..];
        }
        if !cols.is_empty() {
            sink.text(&cols.join("\t"));
            sink.text("\n");
        }
        rest = &after[rend + 6..];
    }
}
