//! An e-book (EPUB 2 and 3) as a flow: its title, then its chapters in the
//! order its spine reads them, each drawn as a page is (`crate::html`), with
//! the pictures the book holds. A rule between chapters.

use std::io::{Cursor, Read};

use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use scour_flow::Flow;

use crate::build::Builder;

/// A part read at most, and the chapters at most.
const MOST_PART: u64 = 16 << 20;
const MOST_CHAPTERS: usize = 2_000;

type Zip<'a> = zip::ZipArchive<Cursor<&'a [u8]>>;

/// The book in `bytes`.
pub fn flow(bytes: &[u8]) -> Result<Flow, String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let container = part(&mut zip, "META-INF/container.xml").ok_or("no container.xml")?;
    let opf_path = attr_of(&container, "rootfile", "full-path").ok_or("no package")?;
    let opf = part(&mut zip, &opf_path).ok_or("no package")?;
    let package = Package::read(&opf);
    let base = folder(&opf_path);
    let mut b = Builder::default();
    if let Some(title) = package.title.as_deref().filter(|t| !t.trim().is_empty()) {
        b.heading(1);
        b.words(title.trim(), false);
        b.end_heading();
    }
    for (n, href) in package.spine().take(MOST_CHAPTERS).enumerate() {
        if b.full() {
            break;
        }
        let path = join(&base, href);
        let Some(chapter) = part(&mut zip, &path) else {
            continue;
        };
        if n > 0 {
            b.rule();
        }
        let here = folder(&path);
        let text = String::from_utf8_lossy(&chapter);
        crate::html::read(&text, &mut b, &mut |src| {
            if src.contains("://") || src.starts_with("data:") {
                return None;
            }
            part(&mut zip, &join(&here, src))
        });
    }
    Ok(b.finish())
}

fn part(zip: &mut Zip<'_>, name: &str) -> Option<Vec<u8>> {
    let file = zip.by_name(name).ok()?;
    if file.size() > MOST_PART {
        return None;
    }
    let mut out = Vec::with_capacity(file.size() as usize);
    file.take(MOST_PART).read_to_end(&mut out).ok()?;
    Some(out)
}

/// The folder a part is in, with its slash; empty at the top.
fn folder(path: &str) -> String {
    path.rfind('/')
        .map_or_else(String::new, |i| path[..=i].to_owned())
}

/// `href` from the folder `base`: `..` climbed, `%20` read, `#here` dropped.
fn join(base: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or("");
    let href = unescape(href);
    let mut parts: Vec<&str> = if href.starts_with('/') {
        Vec::new()
    } else {
        base.split('/').filter(|p| !p.is_empty()).collect()
    };
    for p in href.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// Percent escapes read, as a path in a book is written.
fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(n) = text
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(n);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The first `element`'s `attr` in an XML part.
fn attr_of(xml: &[u8], element: &str, attr: &str) -> Option<String> {
    let text = String::from_utf8_lossy(xml);
    let mut reader = quick_xml::Reader::from_str(&text);
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) | Event::Empty(e) if local(e.name().as_ref()) == element => {
                return value(&e, attr);
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// An attribute's value, by its name without its prefix.
fn value(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .with_checks(false)
        .flatten()
        .find(|a| local(a.key.as_ref()) == name)
        .map(|a| a.value.to_string())
}

/// A name without its namespace's prefix.
fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// What the package says: its title, what it holds, and the order to read it in.
#[derive(Default)]
struct Package {
    title: Option<String>,
    /// Each item's id and where it is, beside the package.
    items: Vec<(String, String)>,
    spine: Vec<String>,
}

impl Package {
    fn read(opf: &[u8]) -> Package {
        let mut p = Package::default();
        let text = String::from_utf8_lossy(opf);
        let mut reader = quick_xml::Reader::from_str(&text);
        let mut in_title = false;
        let mut title = String::new();
        loop {
            let event = match reader.read_event() {
                Ok(Event::Eof) | Err(_) => break,
                Ok(e) => e,
            };
            match event {
                Event::Start(e) | Event::Empty(e) => match local(e.name().as_ref()) {
                    "item" => {
                        if let (Some(id), Some(href)) = (value(&e, "id"), value(&e, "href")) {
                            p.items.push((id, href));
                        }
                    }
                    // A part left out of the reading order is not read.
                    "itemref" if value(&e, "linear").as_deref() != Some("no") => {
                        if let Some(id) = value(&e, "idref") {
                            p.spine.push(id);
                        }
                    }
                    "title" if p.title.is_none() => in_title = true,
                    _ => {}
                },
                // The title's words, its entities among them, until it ends.
                Event::Text(t) if in_title => {
                    title.push_str(&t.xml_content(XmlVersion::Implicit1_0))
                }
                Event::GeneralRef(r) if in_title => match r.resolve_char_ref() {
                    Ok(Some(c)) => title.push(c),
                    _ => title.push_str(
                        quick_xml::escape::resolve_predefined_entity(
                            &r.xml_content(XmlVersion::Implicit1_0),
                        )
                        .unwrap_or(""),
                    ),
                },
                Event::End(_) if in_title => {
                    in_title = false;
                    p.title = Some(std::mem::take(&mut title));
                }
                _ => {}
            }
        }
        p
    }

    /// The parts in reading order, by where they are.
    fn spine(&self) -> impl Iterator<Item = &str> {
        self.spine.iter().filter_map(|id| {
            self.items
                .iter()
                .find(|(item, _)| item == id)
                .map(|(_, href)| href.as_str())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_flow::Block;
    use std::io::Write;

    fn book(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut z = zip::ZipWriter::new(Cursor::new(&mut out));
            let o = zip::write::SimpleFileOptions::default();
            for (name, data) in parts {
                z.start_file(*name, o).unwrap();
                z.write_all(data).unwrap();
            }
            z.finish().unwrap();
        }
        out
    }

    #[test]
    fn chapters_in_the_spines_order_with_the_books_own_pictures() {
        let png =
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";
        let bytes = book(&[
            (
                "META-INF/container.xml",
                br#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#,
            ),
            (
                "OEBPS/content.opf",
                br#"<package><metadata><dc:title>Kitap &amp; ad</dc:title></metadata>
                <manifest><item id="b" href="Text/b.xhtml"/><item id="a" href="Text/a%20x.xhtml"/>
                <item id="n" href="nav.xhtml"/></manifest>
                <spine><itemref idref="a"/><itemref idref="n" linear="no"/><itemref idref="b"/></spine></package>"#,
            ),
            (
                "OEBPS/Text/a x.xhtml",
                b"<html><body><h1>Bir</h1><p>ilk</p><img src=\"../Images/p.png\" alt=\"resim\"/></body></html>",
            ),
            ("OEBPS/Text/b.xhtml", b"<html><body><h1>Iki</h1><p>son</p></body></html>"),
            ("OEBPS/nav.xhtml", b"<html><body><p>icindekiler</p></body></html>"),
            ("OEBPS/Images/p.png", png),
        ]);
        let flow = flow(&bytes).unwrap();
        let words: Vec<String> = flow
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Para(p) => Some(p.text()),
                Block::Picture(_) => Some("[picture]".into()),
                Block::Break => Some("---".into()),
                _ => None,
            })
            .collect();
        assert_eq!(
            words,
            ["Kitap & ad", "Bir", "ilk", "[picture]", "---", "Iki", "son"]
        );
    }

    #[test]
    fn a_path_from_a_chapter() {
        assert_eq!(
            join("OEBPS/Text/", "../Images/a%20b.png#x"),
            "OEBPS/Images/a b.png"
        );
        assert_eq!(join("", "ch1.xhtml"), "ch1.xhtml");
        assert_eq!(folder("OEBPS/content.opf"), "OEBPS/");
    }
}
