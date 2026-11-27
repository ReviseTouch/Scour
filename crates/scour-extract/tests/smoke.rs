use std::io::Write as _;
use std::path::PathBuf;

use flate2::Compression;
use flate2::write::DeflateEncoder;
use scour_extract::zip::{self, Entry};
use scour_extract::{document_text, inflate_whole, reads_document};

/// An archive written by hand: each entry deflated or stored, a directory, an end.
fn archive(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data, deflate) in entries {
        let body = if *deflate {
            let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
            enc.write_all(data).unwrap();
            enc.finish().unwrap()
        } else {
            data.to_vec()
        };
        let method: u16 = if *deflate { 8 } else { 0 };
        let offset = out.len() as u32;
        out.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04, 20, 0, 0, 0]);
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&body);

        central.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02, 20, 0, 20, 0, 0, 0]);
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&[0; 8]);
        central.extend_from_slice(&(body.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06, 0, 0, 0, 0]);
    let n = entries.len() as u16;
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn on_disk(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("scour-extract-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn names(buf: &[u8], entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .map(|e| String::from_utf8_lossy(e.name_in(buf)).into_owned())
        .collect()
}

#[test]
fn the_directory_names_every_entry_and_finds_its_bytes() {
    let zip = archive(&[("a.txt", b"first", false), ("b/c.xml", b"<p>x</p>", true)]);
    assert!(zip::is_zip(&zip));
    let mut entries = Vec::new();
    assert!(zip::central_in(&zip, |e| entries.push(e)));
    assert_eq!(names(&zip, &entries), ["a.txt", "b/c.xml"]);
    assert_eq!(zip::data_in(&zip, &entries[0]), Some(&b"first"[..]));

    let mut inflate = flate2::Decompress::new(false);
    let mut out = Vec::new();
    let comp = zip::data_in(&zip, &entries[1]).unwrap();
    assert!(inflate_whole(&mut inflate, 8, comp, 8, &mut out, 1 << 20));
    assert_eq!(out, b"<p>x</p>");
}

/// A directory cut off — a download that stopped — is read by its local
/// headers instead, front to back.
#[test]
fn an_archive_without_its_directory_is_read_by_its_headers() {
    let zip = archive(&[("one", b"1", false), ("two", b"22", false)]);
    let cut = &zip[..zip.len() - 30];
    assert!(!zip::central_in(cut, |_| {}));
    let mut entries = Vec::new();
    zip::local_headers(cut, |e| entries.push(e));
    assert_eq!(names(cut, &entries), ["one", "two"]);
    assert_eq!(zip::data_in(cut, &entries[1]), Some(&b"22"[..]));
}

#[test]
fn a_file_is_read_by_its_tail_and_one_entry_at_a_time() {
    let zip = archive(&[("x", b"hello", true)]);
    let path = on_disk("tail.zip", &zip);
    let mut file = std::fs::File::open(&path).unwrap();
    let (mut cd, mut entries, mut read) = (Vec::new(), Vec::new(), 0u64);
    assert!(zip::load_central(
        &mut file,
        zip.len() as u64,
        &mut cd,
        &mut read,
        |e| entries.push(e)
    ));
    assert_eq!(names(&cd, &entries), ["x"]);
    let mut raw = Vec::new();
    assert!(zip::read_raw(
        &mut file,
        zip.len() as u64,
        &entries[0],
        &mut raw,
        &mut read
    ));
    assert!(read >= zip.len() as u64 - 22);
}

/// More than the cap is never inflated, whatever the directory claims.
#[test]
fn inflating_stops_at_the_cap() {
    let big = vec![b'a'; 100_000];
    let zip = archive(&[("big", &big, true)]);
    let mut entries = Vec::new();
    zip::central_in(&zip, |e| entries.push(e));
    let comp = zip::data_in(&zip, &entries[0]).unwrap();
    let mut out = Vec::new();
    let mut inflate = flate2::Decompress::new(false);
    assert!(inflate_whole(&mut inflate, 8, comp, 0, &mut out, 1000));
    assert!(out.len() <= 1000, "{}", out.len());
}

#[test]
fn a_word_document_reads_as_its_paragraphs_and_cells() {
    let xml = b"<w:document><w:body><w:p><w:r><w:t>Mahke</w:t></w:r><w:r><w:t>mesi &amp; Karar</w:t></w:r></w:p>\
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>";
    let path = on_disk("x.docx", &archive(&[("word/document.xml", xml, true)]));
    assert!(reads_document(&path));
    let file = std::fs::File::open(&path).unwrap();
    let (text, cut) = document_text(&path, file, 4096).unwrap();
    assert!(!cut);
    assert!(text.starts_with("Mahkemesi & Karar\n"), "{text:?}");
    assert!(text.contains('a') && text.contains('b'));
}

/// `slide10` comes after `slide9`, not after `slide1`.
#[test]
fn slides_are_in_their_own_order_each_under_its_number() {
    let slide = |t: &str| format!("<p:sld><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>");
    let (s1, s2, s10) = (slide("one"), slide("two"), slide("ten"));
    let path = on_disk(
        "x.pptx",
        &archive(&[
            ("ppt/slides/slide10.xml", s10.as_bytes(), true),
            ("ppt/slides/slide1.xml", s1.as_bytes(), true),
            ("ppt/slides/slide2.xml", s2.as_bytes(), true),
        ]),
    );
    let file = std::fs::File::open(&path).unwrap();
    let (text, _) = document_text(&path, file, 4096).unwrap();
    let at = |w: &str| text.find(w).unwrap();
    assert!(at("one") < at("two") && at("two") < at("ten"), "{text:?}");
    assert!(text.starts_with("— 1 —"), "{text:?}");
}

#[test]
fn a_sheet_reads_a_row_a_line_with_shared_strings_resolved() {
    let shared = b"<sst><si><t>name</t></si><si><r><t>Ali</t></r><r><t> Veli</t></r></si></sst>";
    let sheet = b"<worksheet><sheetData><row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\"><v>42</v></c></row>\
<row r=\"2\"><c r=\"A2\" t=\"s\"><v>1</v></c></row></sheetData></worksheet>";
    let path = on_disk(
        "x.xlsx",
        &archive(&[
            ("xl/sharedStrings.xml", shared, true),
            ("xl/worksheets/sheet1.xml", sheet, true),
        ]),
    );
    let file = std::fs::File::open(&path).unwrap();
    let (text, _) = document_text(&path, file, 4096).unwrap();
    assert_eq!(text, "name\t42\nAli Veli");
}

#[test]
fn a_budget_cuts_at_a_character_and_says_so() {
    let xml =
        "<w:document><w:body><w:p><w:r><w:t>şşşşşşşşşş</w:t></w:r></w:p></w:body></w:document>";
    let path = on_disk(
        "cut.docx",
        &archive(&[("word/document.xml", xml.as_bytes(), false)]),
    );
    let file = std::fs::File::open(&path).unwrap();
    let (text, cut) = document_text(&path, file, 5).unwrap();
    assert!(cut);
    assert_eq!(text, "şş");
}

#[test]
fn a_string_written_in_its_cell_is_read_too() {
    let sheet = b"<worksheet><sheetData><row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>Belediye &amp; Rapor</t></is></c><c r=\"B1\"><v>7</v></c></row></sheetData></worksheet>";
    let path = on_disk(
        "inline.xlsx",
        &archive(&[("xl/worksheets/sheet1.xml", sheet, true)]),
    );
    let file = std::fs::File::open(&path).unwrap();
    let (text, _) = document_text(&path, file, 4096).unwrap();
    assert_eq!(text, "Belediye & Rapor\t7");
}

/// OpenDocument's sheet and slide show keep their words where its text does.
#[test]
fn an_opendocument_sheet_and_slide_show_read_as_their_words() {
    let sheet = b"<office:document-content><office:body><office:spreadsheet><table:table>\
<table:table-row><table:table-cell><text:p>Ali</text:p></table:table-cell>\
<table:table-cell><text:p>42</text:p></table:table-cell></table:table-row></table:table>\
</office:spreadsheet></office:body></office:document-content>";
    for name in ["x.ods", "x.odp"] {
        let path = on_disk(name, &archive(&[("content.xml", sheet, true)]));
        assert!(reads_document(&path), "{name}");
        let file = std::fs::File::open(&path).unwrap();
        let (text, _) = document_text(&path, file, 4096).unwrap();
        assert!(
            text.contains("Ali") && text.contains("42"),
            "{name}: {text:?}"
        );
    }
}

/// RTF's words, not its markup: what a preview shows of it.
#[test]
fn rtf_reads_as_its_words() {
    let path = on_disk(
        "x.rtf",
        br"{\rtf1\ansi{\fonttbl{\f0 Times;}}\f0 Kira \b s\'f6zle\'fe\'fdmesi\b0\par}",
    );
    assert!(reads_document(&path));
    let file = std::fs::File::open(&path).unwrap();
    let (text, _) = document_text(&path, file, 4096).unwrap();
    assert!(
        text.contains("Kira") && !text.contains("rtf1") && !text.contains("fonttbl"),
        "{text:?}"
    );
}
