//! A Markdown file as a [`scour_flow::Flow`]: its headings, paragraphs,
//! lists, quotes, tables and code as a word processor's document flows,
//! read with pulldown-cmark (CommonMark, with GitHub's tables,
//! strikethrough and task lists). Pictures are named, not fetched: a file's
//! bytes are all a reader is given, and a picture beside it is not in them.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use scour_flow::{Align, Flow};

use crate::build::Builder;

/// The document in `bytes`, read as UTF-8 (a stray byte becomes U+FFFD).
pub fn flow(bytes: &[u8]) -> Result<Flow, String> {
    let mut b = Builder::default();
    read(&String::from_utf8_lossy(bytes), &mut b);
    Ok(b.finish())
}

/// Markdown `text` into `b`: a notebook's cells share one document.
pub(crate) fn read(text: &str, b: &mut Builder) {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    // A code block's lines, while one is read.
    let mut code: Option<String> = None;
    for event in Parser::new_ext(text, options) {
        if b.full() {
            break;
        }
        if let Some(lines) = code.as_mut() {
            match event {
                Event::Text(t) => lines.push_str(&t),
                Event::End(TagEnd::CodeBlock | TagEnd::MetadataBlock(_)) => {
                    b.code_block(&code.take().unwrap_or_default());
                }
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => b.flush(),
                Tag::Heading { level, .. } => b.heading(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                }),
                Tag::BlockQuote(_) => b.quote(),
                Tag::CodeBlock(kind) => {
                    // A fenced block's language is said above it, quietly.
                    if let CodeBlockKind::Fenced(lang) = kind
                        && !lang.trim().is_empty()
                    {
                        b.caption(lang.trim());
                    }
                    code = Some(String::new());
                }
                Tag::MetadataBlock(_) => code = Some(String::new()),
                Tag::List(first) => b.list(first),
                Tag::Item => b.item(),
                Tag::Table(aligns) => {
                    b.table(
                        aligns
                            .iter()
                            .map(|a| match a {
                                pulldown_cmark::Alignment::Center => Align::Center,
                                pulldown_cmark::Alignment::Right => Align::Right,
                                _ => Align::Left,
                            })
                            .collect(),
                    );
                }
                Tag::TableHead => b.row(true),
                Tag::TableRow => b.row(false),
                Tag::TableCell => b.cell(),
                Tag::Emphasis => b.look.italic += 1,
                Tag::Strong => b.look.bold += 1,
                Tag::Strikethrough => b.look.strike += 1,
                Tag::Link { .. } => b.look.link += 1,
                // The picture's own words, in its place: a file's bytes are
                // all a reader is given, and the picture is beside them.
                Tag::Image { .. } => {
                    b.look.quiet += 1;
                    b.words("🖼 ", false);
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Item => b.flush(),
                TagEnd::Heading(_) => b.end_heading(),
                TagEnd::BlockQuote(_) => b.end_quote(),
                TagEnd::List(_) => b.end_list(),
                TagEnd::TableCell => b.end_cell(),
                TagEnd::TableHead | TagEnd::TableRow => b.end_row(),
                TagEnd::Table => b.end_table(),
                TagEnd::Emphasis => b.look.italic = b.look.italic.saturating_sub(1),
                TagEnd::Strong => b.look.bold = b.look.bold.saturating_sub(1),
                TagEnd::Strikethrough => b.look.strike = b.look.strike.saturating_sub(1),
                TagEnd::Link => b.look.link = b.look.link.saturating_sub(1),
                TagEnd::Image => b.look.quiet = b.look.quiet.saturating_sub(1),
                _ => {}
            },
            Event::Text(t) => b.words(&t, false),
            Event::Code(t) | Event::InlineMath(t) | Event::DisplayMath(t) => b.words(&t, true),
            Event::SoftBreak => b.words(" ", false),
            Event::HardBreak => b.words("\n", false),
            Event::Rule => b.rule(),
            Event::TaskListMarker(done) => b.task(done),
            Event::FootnoteReference(name) => b.words(&format!("[{name}]"), false),
            // HTML written into the file is read as a page is: its words and
            // what it marks, never its tags; a picture by what it says it is.
            Event::Html(t) | Event::InlineHtml(t) => crate::html::read(&t, b, &mut |_| None),
        }
    }
    b.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::QUIET;
    use crate::flow::LINK;
    use scour_flow::{Block, Para};

    fn paras(flow: &Flow) -> Vec<&Para> {
        flow.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Para(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn headings_lists_quotes_and_code_come_through() {
        let md = "# Başlık\n\nBir **kalın** ve *eğik* `kod` [bağ](x).\n\n\
                  - bir\n- iki\n  1. iç\n\n> alıntı\n\n```rust\nfn main() {}\n```\n\n---\n";
        let flow = flow(md.as_bytes()).unwrap();
        let p = paras(&flow);
        assert_eq!((p[0].heading, p[0].text().as_str()), (1, "Başlık"));
        assert!(p[0].size > 1.5 && p[0].runs[0].bold);
        let body = p[1];
        assert!(body.runs.iter().any(|r| r.bold && r.text == "kalın"));
        assert!(body.runs.iter().any(|r| r.italic && r.text == "eğik"));
        assert!(body.runs.iter().any(|r| r.code && r.text == "kod"));
        assert!(
            body.runs
                .iter()
                .any(|r| r.underline && r.color == Some(LINK))
        );
        assert_eq!((p[2].mark.as_str(), p[2].text().as_str()), ("•", "bir"));
        assert_eq!(p[4].mark, "1.");
        assert!(p[4].indent > p[2].indent);
        assert!(p[5].runs[0].color == Some(QUIET) && p[5].indent > 0.0);
        let code = p.iter().find(|p| p.shade.is_some()).unwrap();
        assert_eq!(code.text(), "fn main() {}");
        assert!(code.runs[0].code);
        assert!(matches!(flow.blocks.last(), Some(Block::Break)));
    }

    #[test]
    fn a_table_keeps_its_heading_and_alignment() {
        let md = "| ad | sayı |\n|:---|---:|\n| elma | 3 |\n| armut | 12 |\n";
        let flow = flow(md.as_bytes()).unwrap();
        let Some(Block::Table(t)) = flow.blocks.first() else {
            panic!("a table: {:?}", flow.blocks);
        };
        assert_eq!(t.rows.len(), 3);
        assert!(t.rows[0].head && !t.rows[1].head);
        assert_eq!(t.rows[2].cells[1].paras[0].align, Align::Right);
        assert!((t.widths.iter().sum::<f32>() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn html_in_a_file_is_its_words() {
        let md =
            "<p align=\"center\"><img src=\"a.svg\" alt=\"logo\"></p>\n\nBas <kbd>Ctrl</kbd>.\n";
        let flow = flow(md.as_bytes()).unwrap();
        let p = paras(&flow);
        assert_eq!(p[0].text(), "🖼 logo");
        assert_eq!(p[1].text(), "Bas Ctrl.");
        assert!(p[1].runs.iter().any(|r| r.code && r.text == "Ctrl"));
    }

    #[test]
    fn tasks_and_front_matter_are_kept() {
        let md = "---\ntitle: x\n---\n\n- [x] bitti\n- [ ] kaldı\n";
        let flow = flow(md.as_bytes()).unwrap();
        let p = paras(&flow);
        assert!(p[0].runs[0].code && p[0].text().contains("title: x"));
        assert_eq!(p[1].mark, "☑");
        assert_eq!(p[2].mark, "☐");
    }
}
