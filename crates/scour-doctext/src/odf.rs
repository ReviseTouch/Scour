//! An OpenDocument file — text, spreadsheet or presentation — as a
//! [`scour_flow::Flow`]: office_oxide reads only Microsoft's formats. Its
//! `content.xml` read into a tree, its styles followed up to their parents,
//! and the tree walked once into paragraphs, tables, pictures, sheets or
//! slides, the way the Microsoft formats' flow draws them.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use scour_flow::{Align, Block, Cell, Flow, GridCell, GridRow, Para, Picture, Row, Run};

use crate::flow::{BLANKS, LINK, finish_sheet};

/// A part read at most: past this a document is not a preview's.
const MOST_PART: u64 = 64 << 20;
/// Elements kept of a part's tree.
const MOST_NODES: usize = 2_000_000;
/// The body's size when no paragraph says one.
const PLAIN_PT: f32 = 12.0;
/// Columns one cell or column repeats into in a word processor's table.
const MOST_REPEAT: usize = 64;
/// Ems a list indents by, a level.
const LIST_EM: f32 = 1.6;

/// The document in `bytes`.
pub fn flow(bytes: &[u8]) -> Result<Flow, String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let content = part(&mut zip, "content.xml").ok_or("no content.xml")?;
    let content = parse(&content)?;
    let mut styles = Styles::default();
    if let Some(xml) = part(&mut zip, "styles.xml")
        && let Ok(tree) = parse(&xml)
    {
        styles.read(&tree);
    }
    styles.read(&content);
    write(&content, styles, Some(zip))
}

/// A flat OpenDocument file — `.fodt`, `.fods`, `.fodp` — in `bytes`: the
/// same document as one XML file, its styles and its body in it, and its
/// pictures written into it rather than beside it.
pub fn flow_flat(bytes: &[u8]) -> Result<Flow, String> {
    if bytes.len() as u64 > MOST_PART {
        return Err("too large to draw".into());
    }
    let content = parse(bytes)?;
    let mut styles = Styles::default();
    styles.read(&content);
    write(&content, styles, None)
}

fn write(
    content: &El,
    styles: Styles,
    zip: Option<zip::ZipArchive<Cursor<&[u8]>>>,
) -> Result<Flow, String> {
    let body = content
        .find("office:body")
        .and_then(|b| b.els().next())
        .ok_or("no body")?;
    let mut w = Writer {
        styles,
        frame_style: None,
        zip,
        blocks: Vec::new(),
        text: 0,
        pictures: 0,
        blanks: 0,
        cut: false,
    };
    let blocks = match body.name.as_str() {
        "office:spreadsheet" => w.sheets(body),
        "office:presentation" | "office:drawing" => w.slides(body),
        _ => {
            w.body(body, 0);
            std::mem::take(&mut w.blocks)
        }
    };
    let mut flow = Flow { blocks, cut: w.cut };
    let body_pt = body_pt(&flow.blocks);
    for block in &mut flow.blocks {
        scale(block, body_pt);
    }
    Ok(flow.bounded())
}

fn part(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<Vec<u8>> {
    let file = zip.by_name(name).ok()?;
    if file.size() > MOST_PART {
        return None;
    }
    let mut out = Vec::with_capacity(file.size() as usize);
    file.take(MOST_PART).read_to_end(&mut out).ok()?;
    Some(out)
}

// ---------------------------------------------------------------- the tree

#[derive(Debug, Default)]
struct El {
    name: String,
    attrs: Vec<(String, String)>,
    kids: Vec<Node>,
}

#[derive(Debug)]
enum Node {
    El(El),
    Text(String),
}

impl El {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn els(&self) -> impl Iterator<Item = &El> {
        self.kids.iter().filter_map(|n| match n {
            Node::El(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    /// The first element of this name, here or below.
    fn find(&self, name: &str) -> Option<&El> {
        if self.name == name {
            return Some(self);
        }
        self.els().find_map(|e| e.find(name))
    }

    fn count(&self, name: &str) -> usize {
        self.attr(name)
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1)
    }
}

/// A part's XML as a tree, cut short at [`MOST_NODES`] elements.
fn parse(xml: &[u8]) -> Result<El, String> {
    let text = std::str::from_utf8(xml).map_err(|e| e.to_string())?;
    let mut reader = quick_xml::Reader::from_str(text);
    let mut stack = vec![El::default()];
    let mut nodes = 0usize;
    loop {
        let event = reader.read_event().map_err(|e| e.to_string())?;
        if nodes >= MOST_NODES && matches!(event, Event::Start(_) | Event::Empty(_)) {
            break;
        }
        match event {
            Event::Start(e) => {
                nodes += 1;
                stack.push(element(&e));
            }
            Event::Empty(e) => {
                nodes += 1;
                let el = element(&e);
                top(&mut stack).kids.push(Node::El(el));
            }
            Event::End(_) => close(&mut stack),
            Event::Text(t) => push_text(top(&mut stack), &t.xml_content(XmlVersion::Implicit1_0)),
            Event::CData(c) => push_text(top(&mut stack), &c.xml_content(XmlVersion::Implicit1_0)),
            Event::GeneralRef(r) => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => quick_xml::escape::resolve_predefined_entity(
                        &r.xml_content(XmlVersion::Implicit1_0),
                    )
                    .unwrap_or("")
                    .to_owned(),
                };
                push_text(top(&mut stack), &resolved);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    while stack.len() > 1 {
        close(&mut stack);
    }
    Ok(stack.pop().unwrap_or_default())
}

fn top(stack: &mut [El]) -> &mut El {
    stack.last_mut().expect("the root is never popped")
}

fn close(stack: &mut Vec<El>) {
    if stack.len() > 1 {
        let el = stack.pop().expect("more than the root");
        top(stack).kids.push(Node::El(el));
    }
}

fn element(e: &BytesStart<'_>) -> El {
    let attrs = e
        .attributes()
        .with_checks(false)
        .flatten()
        .map(|a| {
            let value = a
                .normalized_value(XmlVersion::Implicit1_0)
                .map(|v| v.into_owned())
                .unwrap_or_else(|_| a.value.to_string());
            (a.key.as_ref().to_owned(), value)
        })
        .collect();
    El {
        name: e.name().as_ref().to_owned(),
        attrs,
        kids: Vec::new(),
    }
}

fn push_text(el: &mut El, text: &str) {
    if text.is_empty() {
        return;
    }
    match el.kids.last_mut() {
        Some(Node::Text(t)) => t.push_str(text),
        _ => el.kids.push(Node::Text(text.to_owned())),
    }
}

// ---------------------------------------------------------------- styles

/// A size as a style gives it: in points, or against its parent's.
#[derive(Debug, Clone, Copy)]
enum Size {
    Pt(f32),
    Percent(f32),
}

/// How text looks, as far as a style says.
#[derive(Debug, Clone, Default)]
struct Look {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    color: Option<[u8; 3]>,
    marker: Option<[u8; 3]>,
    raise: Option<i8>,
    size: Option<Size>,
}

impl Look {
    /// `self` over `under`, a percentage against what `under` comes to.
    fn over(&self, under: &Look) -> Look {
        let size = match (self.size, under.size) {
            (Some(Size::Percent(p)), Some(Size::Pt(base))) => Some(Size::Pt(base * p / 100.0)),
            (Some(Size::Percent(p)), None) => Some(Size::Pt(PLAIN_PT * p / 100.0)),
            (Some(s), _) => Some(s),
            (None, s) => s,
        };
        Look {
            bold: self.bold.or(under.bold),
            italic: self.italic.or(under.italic),
            underline: self.underline.or(under.underline),
            strike: self.strike.or(under.strike),
            color: self.color.or(under.color),
            marker: self.marker.or(under.marker),
            raise: self.raise.or(under.raise),
            size,
        }
    }

    fn pt(&self) -> Option<f32> {
        match self.size {
            Some(Size::Pt(p)) => Some(p),
            _ => None,
        }
    }
}

/// How a paragraph, a cell, a column, a table or a page is laid out.
#[derive(Debug, Clone, Default)]
struct Shape {
    align: Option<Align>,
    indent: Option<f32>,
    first: Option<f32>,
    before: Option<f32>,
    after: Option<f32>,
    shade: Option<[u8; 3]>,
    border: Option<bool>,
    width: Option<f32>,
    wrap: Option<bool>,
    hidden: Option<bool>,
    outline: Option<u8>,
    list: Option<String>,
    fill: Option<[u8; 3]>,
}

impl Shape {
    fn over(&self, under: &Shape) -> Shape {
        Shape {
            align: self.align.or(under.align),
            indent: self.indent.or(under.indent),
            first: self.first.or(under.first),
            before: self.before.or(under.before),
            after: self.after.or(under.after),
            shade: self.shade.or(under.shade),
            border: self.border.or(under.border),
            width: self.width.or(under.width),
            wrap: self.wrap.or(under.wrap),
            hidden: self.hidden.or(under.hidden),
            outline: self.outline.or(under.outline),
            list: self.list.clone().or_else(|| under.list.clone()),
            fill: self.fill.or(under.fill),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Style {
    parent: Option<String>,
    look: Look,
    shape: Shape,
}

#[derive(Default)]
struct Styles {
    by_name: HashMap<(String, String), Style>,
    defaults: HashMap<String, Style>,
    /// A list style's levels, from one: numbered or not.
    lists: HashMap<String, [bool; 10]>,
}

impl Styles {
    /// The styles a part declares: named, automatic and defaults.
    fn read(&mut self, tree: &El) {
        for section in ["office:styles", "office:automatic-styles"] {
            let Some(s) = tree.find(section) else {
                continue;
            };
            for el in s.els() {
                match el.name.as_str() {
                    "style:style" => {
                        let (Some(name), Some(family)) =
                            (el.attr("style:name"), el.attr("style:family"))
                        else {
                            continue;
                        };
                        let mut style = style_of(el);
                        style.parent = el.attr("style:parent-style-name").map(str::to_owned);
                        self.by_name
                            .insert((family.to_owned(), name.to_owned()), style);
                    }
                    "style:default-style" => {
                        if let Some(family) = el.attr("style:family") {
                            self.defaults.insert(family.to_owned(), style_of(el));
                        }
                    }
                    "text:list-style" => {
                        let Some(name) = el.attr("style:name") else {
                            continue;
                        };
                        let mut levels = [false; 10];
                        for level in el.els() {
                            let n = level
                                .attr("text:level")
                                .and_then(|l| l.parse::<usize>().ok())
                                .unwrap_or(1);
                            if (1..=10).contains(&n) {
                                levels[n - 1] = level.name == "text:list-level-style-number";
                            }
                        }
                        self.lists.insert(name.to_owned(), levels);
                    }
                    _ => {}
                }
            }
        }
    }

    /// A style of `family` with every parent under it, the family's default
    /// at the bottom.
    fn resolve(&self, family: &str, name: Option<&str>) -> Style {
        self.resolve_on(family, name, None)
    }

    /// The same on `under` in place of the family's default: a paragraph in a
    /// frame is set on the frame's style, which says its size.
    fn resolve_on(&self, family: &str, name: Option<&str>, under: Option<&Style>) -> Style {
        let mut chain = Vec::new();
        let mut at = name.map(str::to_owned);
        while let Some(n) = at.take() {
            if chain.len() >= 16 {
                break;
            }
            let Some(style) = self.by_name.get(&(family.to_owned(), n)) else {
                break;
            };
            at.clone_from(&style.parent);
            chain.push(style);
        }
        let mut out = under
            .or_else(|| self.defaults.get(family))
            .cloned()
            .unwrap_or_default();
        for style in chain.into_iter().rev() {
            out = Style {
                parent: None,
                look: style.look.over(&out.look),
                shape: style.shape.over(&out.shape),
            };
        }
        out
    }
}

fn style_of(el: &El) -> Style {
    let mut style = Style::default();
    if let Some(level) = el
        .attr("style:default-outline-level")
        .and_then(|v| v.parse::<u8>().ok())
    {
        style.shape.outline = Some(level);
    }
    style.shape.list = el.attr("style:list-style-name").map(str::to_owned);
    for props in el.els() {
        let a = |name: &str| props.attr(name);
        match props.name.as_str() {
            "style:text-properties" => {
                let look = &mut style.look;
                look.bold = a("fo:font-weight")
                    .map(|w| w == "bold" || w.parse::<u32>().is_ok_and(|n| n >= 600));
                look.italic = a("fo:font-style").map(|s| s == "italic" || s == "oblique");
                look.underline = a("style:text-underline-style").map(|s| s != "none");
                look.strike = a("style:text-line-through-style").map(|s| s != "none");
                look.color = a("fo:color").and_then(color);
                look.marker = a("fo:background-color").and_then(color);
                look.raise = a("style:text-position").map(raise);
                look.size = a("fo:font-size").and_then(size);
            }
            "style:paragraph-properties" => {
                let shape = &mut style.shape;
                shape.align = a("fo:text-align").map(|v| match v {
                    "center" => Align::Center,
                    "end" | "right" => Align::Right,
                    _ => Align::Left,
                });
                shape.indent = a("fo:margin-left").and_then(length);
                shape.first = a("fo:text-indent").and_then(length);
                shape.before = a("fo:margin-top").and_then(length);
                shape.after = a("fo:margin-bottom").and_then(length);
                if let Some(c) = a("fo:background-color").and_then(color) {
                    shape.shade = Some(c);
                }
            }
            "style:table-cell-properties" => {
                let shape = &mut style.shape;
                if let Some(c) = a("fo:background-color").and_then(color) {
                    shape.shade = Some(c);
                }
                let ruled = ["fo:border", "fo:border-top", "fo:border-bottom"]
                    .iter()
                    .filter_map(|k| a(k))
                    .any(|b| b != "none");
                if ruled {
                    shape.border = Some(true);
                }
                shape.wrap = a("fo:wrap-option").map(|w| w == "wrap");
            }
            "style:table-column-properties" => {
                style.shape.width = a("style:column-width").and_then(length);
            }
            "style:table-properties" => {
                style.shape.hidden = a("table:display").map(|d| d == "false");
            }
            "style:drawing-page-properties" | "style:graphic-properties"
                if a("draw:fill") == Some("solid") =>
            {
                style.shape.fill = a("draw:fill-color").and_then(color);
            }
            _ => {}
        }
    }
    style
}

fn color(v: &str) -> Option<[u8; 3]> {
    let hex = v.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(hex, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

fn size(v: &str) -> Option<Size> {
    if let Some(p) = v.strip_suffix('%') {
        return p.trim().parse().ok().map(Size::Percent);
    }
    length(v).map(Size::Pt)
}

/// A length in points.
fn length(v: &str) -> Option<f32> {
    let v = v.trim();
    let split = v.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(v.len());
    let n: f32 = v[..split].parse().ok()?;
    let per = match &v[split..] {
        "pt" | "" => 1.0,
        "in" => 72.0,
        "cm" => 72.0 / 2.54,
        "mm" => 72.0 / 25.4,
        "pc" => 12.0,
        "px" => 0.75,
        _ => return None,
    };
    let pt = n * per;
    pt.is_finite().then_some(pt)
}

/// `super 58%`, `sub 58%`, or a percentage up or down.
fn raise(v: &str) -> i8 {
    let first = v.split_whitespace().next().unwrap_or("");
    match first {
        "super" => 1,
        "sub" => -1,
        p => match p.trim_end_matches('%').parse::<f32>() {
            Ok(n) if n > 0.0 => 1,
            Ok(n) if n < 0.0 => -1,
            _ => 0,
        },
    }
}

// ---------------------------------------------------------------- writing

struct Writer<'a> {
    styles: Styles,
    /// The frame being read: its paragraphs are set on its style.
    frame_style: Option<Style>,
    /// Where the pictures are, by their names; none in a flat file.
    zip: Option<zip::ZipArchive<Cursor<&'a [u8]>>>,
    blocks: Vec<Block>,
    text: usize,
    pictures: usize,
    blanks: usize,
    cut: bool,
}

/// A paragraph's words as they are read: runs, white space collapsed as the
/// format says, and what sits in it that is not words.
#[derive(Default)]
struct Line<'e> {
    runs: Vec<(Run, Option<f32>)>,
    /// White space was the last thing read, or nothing yet.
    spaced: bool,
    frames: Vec<&'e El>,
}

impl<'e> Line<'e> {
    fn new() -> Self {
        Line {
            spaced: true,
            ..Line::default()
        }
    }

    fn push(&mut self, text: &str, look: &Look, link: bool) {
        if text.is_empty() {
            return;
        }
        let run = Run {
            text: text.to_owned(),
            bold: look.bold.unwrap_or(false),
            italic: look.italic.unwrap_or(false),
            underline: look.underline.unwrap_or(false) || (link && look.color.is_none()),
            strike: look.strike.unwrap_or(false),
            color: look.color.or(link.then_some(LINK)),
            marker: look.marker,
            raise: look.raise.unwrap_or(0),
            code: false,
        };
        let pt = look.pt();
        match self.runs.last_mut() {
            Some((last, at)) if same(last, &run) && *at == pt => last.text.push_str(text),
            _ => self.runs.push((run, pt)),
        }
    }

    /// Words from the XML: a run of white space is one space, none at the start.
    fn words(&mut self, raw: &str, look: &Look, link: bool) {
        let mut out = String::with_capacity(raw.len());
        for c in raw.chars() {
            if c.is_whitespace() {
                if !self.spaced {
                    out.push(' ');
                    self.spaced = true;
                }
            } else {
                out.push(c);
                self.spaced = false;
            }
        }
        self.push(&out, look, link);
    }

    /// Characters the markup asks for as they are: `text:s`, a tab, a break.
    fn literal(&mut self, text: &str, look: &Look) {
        self.push(text, look, false);
        self.spaced = text.ends_with('\n');
    }

    fn finish(mut self) -> (Vec<Run>, Option<f32>, Vec<&'e El>) {
        if let Some((last, _)) = self.runs.last_mut() {
            let kept = last.text.trim_end_matches(' ').len();
            last.text.truncate(kept);
        }
        // The size most of the text is set in.
        let mut by: Vec<(f32, usize)> = Vec::new();
        for (r, pt) in &self.runs {
            if let Some(pt) = pt {
                match by.iter_mut().find(|(p, _)| p == pt) {
                    Some((_, n)) => *n += r.text.len(),
                    None => by.push((*pt, r.text.len())),
                }
            }
        }
        let pt = by.into_iter().max_by_key(|&(_, n)| n).map(|(p, _)| p);
        let runs = self
            .runs
            .into_iter()
            .map(|(r, _)| r)
            .filter(|r| !r.text.is_empty())
            .collect();
        (runs, pt, self.frames)
    }
}

fn same(a: &Run, b: &Run) -> bool {
    (
        a.bold,
        a.italic,
        a.underline,
        a.strike,
        a.color,
        a.marker,
        a.raise,
    ) == (
        b.bold,
        b.italic,
        b.underline,
        b.strike,
        b.color,
        b.marker,
        b.raise,
    )
}

impl<'a> Writer<'a> {
    fn push(&mut self, block: Block) {
        if self.blocks.len() >= scour_flow::MOST_BLOCKS || self.text >= scour_flow::MOST_TEXT {
            self.cut = true;
            return;
        }
        if let Block::Para(p) = &block {
            if p.runs.iter().all(|r| r.text.trim().is_empty()) && p.mark.is_empty() {
                self.blanks += 1;
                if self.blanks > BLANKS {
                    return;
                }
            } else {
                self.blanks = 0;
            }
            self.text += p.runs.iter().map(|r| r.text.len()).sum::<usize>();
        } else {
            self.blanks = 0;
        }
        self.blocks.push(block);
    }

    /// Paragraphs, lists, tables and frames, in order.
    fn body(&mut self, el: &El, depth: u8) {
        for kid in el.els() {
            if self.cut {
                return;
            }
            match kid.name.as_str() {
                "text:p" | "text:h" => {
                    for block in self.para(kid) {
                        self.push(block);
                    }
                }
                "text:list" => {
                    for p in self.list(kid, depth, None) {
                        self.push(Block::Para(p));
                    }
                }
                "table:table" => {
                    if let Some(t) = self.table(kid) {
                        self.push(t);
                    }
                }
                "draw:frame" | "draw:a" | "draw:g" | "draw:custom-shape" | "draw:rect" => {
                    for block in self.frame(kid) {
                        self.push(block);
                    }
                }
                "text:section"
                | "text:index-body"
                | "text:table-of-content"
                | "text:alphabetical-index"
                | "text:illustration-index"
                | "text:table-index"
                | "text:user-index"
                | "text:bibliography"
                | "office:text"
                | "draw:text-box" => {
                    self.body(kid, depth);
                }
                _ => {}
            }
        }
    }

    /// A paragraph or a heading, then what was set in it: its pictures, a
    /// frame's paragraphs.
    fn para(&mut self, el: &El) -> Vec<Block> {
        let style = self.styles.resolve_on(
            "paragraph",
            el.attr("text:style-name"),
            self.frame_style.as_ref(),
        );
        let mut line = Line::new();
        self.inline(el, &style.look, false, &mut line);
        let (runs, pt, frames) = line.finish();
        let heading = if el.name == "text:h" {
            el.attr("text:outline-level")
                .and_then(|l| l.parse::<u8>().ok())
                .or(style.shape.outline)
                .unwrap_or(1)
                .clamp(1, 6)
        } else {
            0
        };
        let size = pt.or(style.look.pt()).unwrap_or(match heading {
            0 => PLAIN_PT,
            h => PLAIN_PT * [1.6, 1.35, 1.2, 1.1, 1.0, 1.0][h as usize - 1],
        });
        let s = &style.shape;
        let mut out = vec![Block::Para(Para {
            runs,
            align: s.align.unwrap_or_default(),
            indent: s.indent.unwrap_or(0.0),
            first: s.first.unwrap_or(0.0),
            before: s.before.unwrap_or(0.0),
            after: s.after.unwrap_or(0.0),
            size,
            heading,
            shade: s.shade,
            ..Para::default()
        })];
        for frame in frames {
            out.extend(self.frame(frame));
        }
        out
    }

    fn inline<'e>(&self, el: &'e El, look: &Look, link: bool, line: &mut Line<'e>) {
        for kid in &el.kids {
            let kid = match kid {
                Node::Text(t) => {
                    line.words(t, look, link);
                    continue;
                }
                Node::El(e) => e,
            };
            match kid.name.as_str() {
                "text:span" => {
                    let own = self.styles.resolve("text", kid.attr("text:style-name"));
                    self.inline(kid, &own.look.over(look), link, line);
                }
                "text:a" => self.inline(kid, look, true, line),
                "text:s" => line.literal(&" ".repeat(kid.count("text:c").min(256)), look),
                "text:tab" => line.literal("\t", look),
                "text:line-break" => line.literal("\n", look),
                "text:note" => {
                    if let Some(citation) = kid.find("text:note-citation") {
                        let mut cited = Look {
                            raise: Some(1),
                            ..Look::default()
                        }
                        .over(look);
                        cited.underline = Some(false);
                        let mut words = Line::new();
                        self.inline(citation, &cited, false, &mut words);
                        for (run, _) in words.runs {
                            line.push(&run.text, &cited, false);
                        }
                    }
                }
                "draw:frame" | "draw:a" | "draw:g" | "draw:custom-shape" => line.frames.push(kid),
                "office:annotation"
                | "office:annotation-end"
                | "text:bookmark"
                | "text:bookmark-start"
                | "text:bookmark-end"
                | "text:reference-mark"
                | "text:reference-mark-start"
                | "text:reference-mark-end"
                | "text:soft-page-break"
                | "text:alphabetical-index-mark"
                | "text:toc-mark"
                | "text:sequence-ref" => {}
                _ => self.inline(kid, look, link, line),
            }
        }
    }

    /// A list's items as paragraphs, the first of each marked.
    fn list(&mut self, el: &El, depth: u8, inherited: Option<&str>) -> Vec<Para> {
        let style = el.attr("text:style-name").or(inherited).map(str::to_owned);
        let numbered = style
            .as_deref()
            .and_then(|s| self.styles.lists.get(s))
            .is_some_and(|levels| levels[usize::from(depth.min(9))]);
        let mut out = Vec::new();
        let mut n = 1usize;
        for item in el.els() {
            let header = match item.name.as_str() {
                "text:list-item" => false,
                "text:list-header" => true,
                _ => continue,
            };
            if let Some(start) = item
                .attr("text:start-value")
                .and_then(|v| v.parse::<usize>().ok())
            {
                n = start;
            }
            let mark = if header {
                String::new()
            } else if numbered {
                format!("{n}.")
            } else if depth.is_multiple_of(2) {
                "•".to_owned()
            } else {
                "◦".to_owned()
            };
            let mut first = true;
            for kid in item.els() {
                match kid.name.as_str() {
                    "text:p" | "text:h" => {
                        for block in self.para(kid) {
                            if let Block::Para(mut p) = block {
                                p.indent = p.indent.max(f32::from(depth + 1) * LIST_EM * PLAIN_PT);
                                p.first = 0.0;
                                if first {
                                    p.mark.clone_from(&mark);
                                    first = false;
                                }
                                out.push(p);
                            }
                        }
                    }
                    "text:list" => out.extend(self.list(kid, depth + 1, style.as_deref())),
                    _ => {}
                }
            }
            if !header {
                n += 1;
            }
        }
        out
    }

    fn table(&mut self, el: &El) -> Option<Block> {
        let mut widths = Vec::new();
        columns(el, &mut |column| {
            let width = self
                .styles
                .resolve("table-column", column.attr("table:style-name"))
                .shape
                .width
                .unwrap_or(0.0);
            for _ in 0..column
                .count("table:number-columns-repeated")
                .min(MOST_REPEAT)
            {
                widths.push(width);
            }
        });
        let mut rows = Vec::new();
        let mut ruled = false;
        let mut row_els = Vec::new();
        table_rows(el, false, &mut row_els);
        for (row, head) in row_els {
            if rows.len() >= scour_flow::MOST_ROWS {
                break;
            }
            let mut cells = Vec::new();
            for cell in row.els().filter(|c| c.name == "table:table-cell") {
                let style = self
                    .styles
                    .resolve("table-cell", cell.attr("table:style-name"));
                ruled |= style.shape.border.unwrap_or(false);
                let paras = self.cell(cell);
                for _ in 0..cell.count("table:number-columns-repeated").min(MOST_REPEAT) {
                    cells.push(Cell {
                        paras: paras.clone(),
                        span: cell.count("table:number-columns-spanned") as u32,
                        shade: style.shape.shade,
                    });
                }
            }
            if !cells.is_empty() {
                for _ in 0..row.count("table:number-rows-repeated").min(MOST_REPEAT) {
                    rows.push(Row {
                        cells: cells.clone(),
                        head,
                    });
                }
            }
        }
        if rows.is_empty() {
            return None;
        }
        let total: f32 = widths.iter().sum();
        let widths = if total > 0.0 {
            widths.iter().map(|w| w / total).collect()
        } else {
            Vec::new()
        };
        Some(Block::Table(scour_flow::Table {
            widths,
            rows,
            ruled,
        }))
    }

    /// A cell's paragraphs; a table inside one, a line a row.
    fn cell(&mut self, el: &El) -> Vec<Para> {
        let mut out = Vec::new();
        for kid in el.els() {
            match kid.name.as_str() {
                "text:p" | "text:h" => {
                    for block in self.para(kid) {
                        if let Block::Para(p) = block {
                            out.push(p);
                        }
                    }
                }
                "text:list" => out.extend(self.list(kid, 0, None)),
                "table:table" => {
                    let mut rows = Vec::new();
                    table_rows(kid, false, &mut rows);
                    for (row, _) in rows {
                        let line: Vec<String> = row
                            .els()
                            .filter(|c| c.name == "table:table-cell")
                            .map(|c| {
                                self.cell(c)
                                    .iter()
                                    .map(Para::text)
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                            .collect();
                        out.push(Para {
                            runs: vec![Run {
                                text: line.join(" · "),
                                ..Run::default()
                            }],
                            size: PLAIN_PT,
                            ..Para::default()
                        });
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// What a frame holds: a picture, a box of paragraphs, a table; a group's
    /// frames in turn.
    fn frame(&mut self, el: &El) -> Vec<Block> {
        let graphic = self.styles.resolve("graphic", el.attr("draw:style-name"));
        let own = match el.attr("presentation:style-name") {
            Some(name) => {
                let p = self.styles.resolve("presentation", Some(name));
                Style {
                    parent: None,
                    look: p.look.over(&graphic.look),
                    shape: Shape::default(),
                }
            }
            None => Style {
                parent: None,
                look: graphic.look,
                shape: Shape::default(),
            },
        };
        let held = self.frame_style.replace(own);
        let out = self.frame_inside(el);
        self.frame_style = held;
        out
    }

    fn frame_inside(&mut self, el: &El) -> Vec<Block> {
        let mut out = Vec::new();
        match el.name.as_str() {
            "draw:frame" => {
                let (w, h) = (
                    el.attr("svg:width").and_then(length),
                    el.attr("svg:height").and_then(length),
                );
                // The first picture a face can draw: a frame may offer an SVG
                // and a PNG of the same thing.
                let picture = el
                    .els()
                    .filter(|k| k.name == "draw:image")
                    .find_map(|img| self.picture(img, w, h));
                if let Some(p) = picture {
                    out.push(p);
                } else if let Some(text_box) = el.els().find(|k| k.name == "draw:text-box") {
                    out.extend(self.collect(text_box));
                } else if let Some(t) = el.els().find(|k| k.name == "table:table") {
                    out.extend(self.table(t));
                }
            }
            "draw:a" | "draw:g" => {
                for kid in el.els() {
                    out.extend(self.frame(kid));
                }
            }
            _ => out.extend(self.collect(el)),
        }
        out
    }

    /// The blocks `body` would push, kept apart.
    fn collect(&mut self, el: &El) -> Vec<Block> {
        let held = std::mem::take(&mut self.blocks);
        self.body(el, 0);
        std::mem::replace(&mut self.blocks, held)
    }

    /// A picture by its name in the zip, or written into the file as a flat
    /// document writes it.
    fn picture(&mut self, img: &El, w: Option<f32>, h: Option<f32>) -> Option<Block> {
        let data = match img.attr("xlink:href") {
            Some(href) if !href.contains("://") && !href.starts_with('/') => {
                part(self.zip.as_mut()?, href.trim_start_matches("./"))?
            }
            Some(_) => return None,
            None => {
                let inline = img.els().find(|k| k.name == "office:binary-data")?;
                let text: String = inline
                    .kids
                    .iter()
                    .filter_map(|n| match n {
                        Node::Text(t) => Some(t.as_str()),
                        Node::El(_) => None,
                    })
                    .flat_map(|t| t.chars().filter(|c| !c.is_whitespace()))
                    .collect();
                if text.len() > scour_flow::MOST_PICTURE {
                    return None;
                }
                scour_flow::decode(&text)?
            }
        };
        let mime = if data.starts_with(b"\x89PNG") {
            "image/png"
        } else if data.starts_with(b"\xFF\xD8") {
            "image/jpeg"
        } else if data.starts_with(b"GIF8") {
            "image/gif"
        } else if data.starts_with(b"BM") {
            "image/bmp"
        } else {
            return None;
        };
        let encoded = data.len().div_ceil(3) * 4;
        if encoded > scour_flow::MOST_PICTURE || self.pictures + encoded > scour_flow::MOST_PICTURES
        {
            return None;
        }
        self.pictures += encoded;
        Some(Block::Picture(Picture {
            mime: mime.to_owned(),
            data: scour_flow::encode(&data),
            width: w.unwrap_or(240.0),
            height: h.unwrap_or(160.0),
        }))
    }

    /// A spreadsheet's sheets, each a grid; hidden ones left out.
    fn sheets(&mut self, body: &El) -> Vec<Block> {
        let mut out = Vec::new();
        let mut tables = Vec::new();
        sheet_tables(body, &mut tables);
        for (n, table) in tables.into_iter().enumerate() {
            let shown = self
                .styles
                .resolve("table", table.attr("table:style-name"))
                .shape
                .hidden
                != Some(true);
            if !shown {
                continue;
            }
            let name = table
                .attr("table:name")
                .map_or_else(|| format!("Sheet{}", n + 1), str::to_owned);
            out.push(Block::Sheet(self.sheet(name, table)));
            if out.len() >= scour_flow::MOST_SHEETS {
                break;
            }
        }
        out
    }

    fn sheet(&mut self, name: String, table: &El) -> scour_flow::Sheet {
        let mut widths = Vec::new();
        let mut defaults: Vec<Option<String>> = Vec::new();
        columns(table, &mut |column| {
            let width = self
                .styles
                .resolve("table-column", column.attr("table:style-name"))
                .shape
                .width
                .unwrap_or(0.0);
            let default = column
                .attr("table:default-cell-style-name")
                .map(str::to_owned);
            let room = scour_flow::MOST_SHEET_COLUMNS.saturating_sub(widths.len());
            for _ in 0..column.count("table:number-columns-repeated").min(room) {
                widths.push(width);
                defaults.push(default.clone());
            }
        });
        let mut row_els = Vec::new();
        table_rows(table, false, &mut row_els);
        let mut rows: Vec<GridRow> = Vec::new();
        for (row, _) in row_els {
            let room = (scour_flow::MOST_SHEET_ROWS + 1).saturating_sub(rows.len());
            if room == 0 {
                break;
            }
            let cells = self.grid_cells(row, &defaults);
            let repeat = row.count("table:number-rows-repeated").min(room);
            for _ in 1..repeat {
                rows.push(GridRow {
                    cells: cells.clone(),
                });
            }
            rows.push(GridRow { cells });
        }
        let widths = if widths.iter().any(|w| *w > 0.0) {
            widths
        } else {
            Vec::new()
        };
        finish_sheet(name, rows, widths)
    }

    fn grid_cells(&mut self, row: &El, defaults: &[Option<String>]) -> Vec<GridCell> {
        let mut cells = Vec::new();
        let mut column = 0usize;
        for cell in row.els() {
            if column >= scour_flow::MOST_SHEET_COLUMNS {
                break;
            }
            let repeat = cell.count("table:number-columns-repeated");
            if cell.name == "table:covered-table-cell" {
                column += repeat;
                continue;
            }
            if cell.name != "table:table-cell" {
                continue;
            }
            let style_name = cell
                .attr("table:style-name")
                .map(str::to_owned)
                .or_else(|| defaults.get(column).cloned().flatten());
            let style = self.styles.resolve("table-cell", style_name.as_deref());
            let mut text = String::new();
            for p in cell.els().filter(|k| k.name == "text:p") {
                if !text.is_empty() {
                    text.push('\n');
                }
                let mut line = Line::new();
                self.inline(p, &style.look, false, &mut line);
                let (runs, _, _) = line.finish();
                for r in runs {
                    text.push_str(&r.text);
                }
            }
            text.truncate(text.floor_char_boundary(scour_flow::MOST_CELL_TEXT));
            let numeric = matches!(
                cell.attr("office:value-type"),
                Some("float" | "percentage" | "currency" | "date" | "time")
            );
            let grid = GridCell {
                text,
                span: cell.count("table:number-columns-spanned") as u32,
                bold: style.look.bold.unwrap_or(false),
                italic: style.look.italic.unwrap_or(false),
                color: style.look.color,
                shade: style.shape.shade,
                align: style.shape.align.or(numeric.then_some(Align::Right)),
                wrap: style.shape.wrap.unwrap_or(false),
                ..GridCell::default()
            };
            let room = scour_flow::MOST_SHEET_COLUMNS - column;
            for _ in 0..repeat.min(room) {
                cells.push(grid.clone());
            }
            column += repeat;
        }
        cells
    }

    /// A presentation's slides, each a card: its title, then its boxes top
    /// to bottom and left to right.
    fn slides(&mut self, body: &El) -> Vec<Block> {
        let mut cards = Vec::new();
        for page in body.els().filter(|p| p.name == "draw:page") {
            let background = self
                .styles
                .resolve("drawing-page", page.attr("draw:style-name"))
                .shape
                .fill;
            let mut items: Vec<&El> = Vec::new();
            placed(page, &mut items);
            let place = |e: &El| {
                if e.attr("presentation:class") == Some("title") {
                    return (f32::MIN, f32::MIN);
                }
                (
                    e.attr("svg:y").and_then(length).unwrap_or(0.0),
                    e.attr("svg:x").and_then(length).unwrap_or(0.0),
                )
            };
            items.sort_by(|a, b| {
                place(a)
                    .partial_cmp(&place(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let mut blocks = Vec::new();
            for item in items {
                blocks.extend(self.frame(item));
            }
            // A slide's blank lines are its layout, which a card does not keep.
            blocks.retain(|b| match b {
                Block::Para(p) => {
                    p.runs.iter().any(|r| !r.text.trim().is_empty()) || !p.mark.is_empty()
                }
                _ => true,
            });
            cards.push(Block::Slide(scour_flow::Slide { background, blocks }));
            if cards.len() >= scour_flow::MOST_BLOCKS / 8 {
                self.cut = true;
                break;
            }
        }
        cards
    }
}

/// A table's columns, through the groups that hold them.
fn columns(el: &El, each: &mut dyn FnMut(&El)) {
    for kid in el.els() {
        match kid.name.as_str() {
            "table:table-column" => each(kid),
            "table:table-columns" | "table:table-header-columns" | "table:table-column-group" => {
                columns(kid, each);
            }
            _ => {}
        }
    }
}

/// A table's rows, through the groups that hold them; whether each heads it.
fn table_rows<'e>(el: &'e El, head: bool, out: &mut Vec<(&'e El, bool)>) {
    for kid in el.els() {
        match kid.name.as_str() {
            "table:table-row" => out.push((kid, head)),
            "table:table-header-rows" => table_rows(kid, true, out),
            "table:table-rows" | "table:table-row-group" => table_rows(kid, head, out),
            _ => {}
        }
    }
}

/// A spreadsheet's tables, through the groups that hold them.
fn sheet_tables<'e>(el: &'e El, out: &mut Vec<&'e El>) {
    for kid in el.els() {
        match kid.name.as_str() {
            "table:table" => out.push(kid),
            "table:table-group" => sheet_tables(kid, out),
            _ => {}
        }
    }
}

/// What a slide shows: its frames and shapes, groups opened; notes, empty
/// placeholders and the footer's fields left out.
fn placed<'e>(el: &'e El, out: &mut Vec<&'e El>) {
    for kid in el.els() {
        let class = kid.attr("presentation:class").unwrap_or("");
        if kid.attr("presentation:placeholder") == Some("true")
            || matches!(
                class,
                "notes" | "page-number" | "footer" | "date-time" | "header"
            )
        {
            continue;
        }
        match kid.name.as_str() {
            "draw:g" => placed(kid, out),
            "draw:frame" | "draw:custom-shape" | "draw:rect" | "draw:ellipse" => out.push(kid),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- sizes

/// The size most of the text is set in, in points.
fn body_pt(blocks: &[Block]) -> f32 {
    let mut by: Vec<(f32, usize)> = Vec::new();
    let mut count = |p: &Para| {
        let n: usize = p.runs.iter().map(|r| r.text.len()).sum();
        if n == 0 || p.heading > 0 {
            return;
        }
        match by.iter_mut().find(|(s, _)| (*s - p.size).abs() < 0.25) {
            Some((_, m)) => *m += n,
            None => by.push((p.size, n)),
        }
    };
    fn walk(blocks: &[Block], count: &mut dyn FnMut(&Para)) {
        for b in blocks {
            match b {
                Block::Para(p) => count(p),
                Block::Table(t) => {
                    for r in &t.rows {
                        for c in &r.cells {
                            for p in &c.paras {
                                count(p);
                            }
                        }
                    }
                }
                Block::Slide(s) => walk(&s.blocks, count),
                _ => {}
            }
        }
    }
    walk(blocks, &mut count);
    by.into_iter()
        .max_by_key(|&(_, n)| n)
        .map_or(PLAIN_PT, |(s, _)| s)
        .clamp(6.0, 36.0)
}

/// Points made ems of the body's text.
fn scale(block: &mut Block, body: f32) {
    let para = |p: &mut Para| {
        p.size /= body;
        p.indent /= body;
        p.first /= body;
        p.before /= body;
        p.after /= body;
    };
    match block {
        Block::Para(p) => para(p),
        Block::Table(t) => {
            for r in &mut t.rows {
                for c in &mut r.cells {
                    c.paras.iter_mut().for_each(para);
                }
            }
        }
        Block::Picture(p) => {
            p.width /= body;
            p.height /= body;
        }
        Block::Slide(s) => {
            for b in &mut s.blocks {
                scale(b, body);
            }
        }
        Block::Break | Block::Sheet(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// An OpenDocument package of these parts, stored.
    fn package(mimetype: &str, parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut out);
            let stored = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("mimetype", stored).unwrap();
            zip.write_all(mimetype.as_bytes()).unwrap();
            for (name, bytes) in parts {
                zip.start_file(*name, stored).unwrap();
                zip.write_all(bytes).unwrap();
            }
            zip.finish().unwrap();
        }
        out.into_inner()
    }

    const NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0""#;

    fn content(styles: &str, body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {NS}><office:automatic-styles>{styles}</office:automatic-styles><office:body>{body}</office:body></office:document-content>"#
        )
    }

    #[test]
    fn a_text_document_s_headings_emphasis_lists_and_tables_flow() {
        let xml = content(
            r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>
               <style:style style:name="P1" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>
               <text:list-style style:name="L1"><text:list-level-style-number text:level="1"/></text:list-style>"#,
            r#"<office:text>
                 <text:h text:outline-level="1">Kira   sözleşmesi</text:h>
                 <text:p text:style-name="P1">Davalı <text:span text:style-name="T1">kiracı</text:span>&amp;<text:s text:c="2"/>ödememiştir.</text:p>
                 <text:list text:style-name="L1"><text:list-item><text:p>birinci</text:p></text:list-item><text:list-item><text:p>ikinci</text:p></text:list-item></text:list>
                 <text:list><text:list-item><text:p>madde</text:p></text:list-item></text:list>
                 <table:table><table:table-column table:number-columns-repeated="2"/>
                   <table:table-header-rows><table:table-row><table:table-cell><text:p>Sıfat</text:p></table:table-cell><table:table-cell><text:p>Ad</text:p></table:table-cell></table:table-row></table:table-header-rows>
                   <table:table-row><table:table-cell><text:p>Kiracı</text:p></table:table-cell><table:table-cell><text:p>Selin</text:p></table:table-cell></table:table-row>
                 </table:table>
               </office:text>"#,
        );
        let bytes = package(
            "application/vnd.oasis.opendocument.text",
            &[("content.xml", xml.as_bytes())],
        );
        let flow = flow(&bytes).unwrap();
        let paras: Vec<&Para> = flow
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Para(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(paras[0].text(), "Kira sözleşmesi");
        assert!(
            paras[0].heading == 1 && paras[0].size > 1.0,
            "{:?}",
            paras[0]
        );
        assert_eq!(paras[1].text(), "Davalı kiracı&  ödememiştir.");
        assert_eq!(paras[1].align, Align::Center);
        assert!(paras[1].runs.iter().any(|r| r.bold && r.text == "kiracı"));
        let marks: Vec<&str> = paras.iter().map(|p| p.mark.as_str()).collect();
        assert_eq!(marks[2..5], ["1.", "2.", "•"]);
        let table = flow.blocks.iter().find_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        });
        let table = table.expect("a table");
        assert!(table.rows[0].head && !table.rows[1].head);
        assert_eq!(table.rows[1].cells[1].paras[0].text(), "Selin");
    }

    #[test]
    fn a_spreadsheet_is_its_sheets_numbers_to_the_right_repeats_held() {
        let xml = content(
            r#"<style:style style:name="ce1" style:family="table-cell"><style:text-properties fo:font-weight="bold"/></style:style>
               <style:style style:name="ta2" style:family="table"><style:table-properties table:display="false"/></style:style>"#,
            r#"<office:spreadsheet>
                 <table:table table:name="Kira">
                   <table:table-column table:number-columns-repeated="16384"/>
                   <table:table-row><table:table-cell table:style-name="ce1"><text:p>Ad</text:p></table:table-cell><table:table-cell table:style-name="ce1"><text:p>Tutar</text:p></table:table-cell><table:table-cell table:number-columns-repeated="16382"/></table:table-row>
                   <table:table-row><table:table-cell><text:p>Ali</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="1500"><text:p>1.500</text:p></table:table-cell></table:table-row>
                   <table:table-row table:number-rows-repeated="1048574"><table:table-cell table:number-columns-repeated="16384"/></table:table-row>
                 </table:table>
                 <table:table table:name="Gizli" table:style-name="ta2"><table:table-row><table:table-cell><text:p>x</text:p></table:table-cell></table:table-row></table:table>
               </office:spreadsheet>"#,
        );
        let bytes = package(
            "application/vnd.oasis.opendocument.spreadsheet",
            &[("content.xml", xml.as_bytes())],
        );
        let flow = flow(&bytes).unwrap();
        let sheets: Vec<&scour_flow::Sheet> = flow.sheets().collect();
        assert_eq!(sheets.len(), 1, "the hidden one left out");
        let sheet = sheets[0];
        assert_eq!(sheet.name, "Kira");
        assert_eq!(sheet.rows.len(), 2, "trailing repeats trimmed");
        assert_eq!(sheet.rows[0].cells.len(), 2);
        assert!(sheet.rows[0].cells[0].bold);
        assert_eq!(sheet.rows[1].cells[1].text, "1.500");
        assert_eq!(sheet.rows[1].cells[1].align, Some(Align::Right));
    }

    #[test]
    fn a_presentation_is_its_slides_title_first_then_top_to_bottom() {
        let xml = content(
            r##"<style:style style:name="dp1" style:family="drawing-page"><style:drawing-page-properties draw:fill="solid" draw:fill-color="#102030"/></style:style>"##,
            r#"<office:presentation>
                 <draw:page draw:name="1" draw:style-name="dp1">
                   <draw:frame svg:x="1cm" svg:y="10cm"><draw:text-box><text:p>alttaki</text:p></draw:text-box></draw:frame>
                   <draw:frame svg:x="1cm" svg:y="5cm"><draw:text-box><text:p>üstteki</text:p></draw:text-box></draw:frame>
                   <draw:frame presentation:class="title" svg:x="1cm" svg:y="1cm"><draw:text-box><text:p>Başlık</text:p></draw:text-box></draw:frame>
                   <draw:frame presentation:class="outline" presentation:placeholder="true"><draw:text-box/></draw:frame>
                   <presentation:notes><draw:frame><draw:text-box><text:p>not</text:p></draw:text-box></draw:frame></presentation:notes>
                 </draw:page>
                 <draw:page draw:name="2"><draw:frame><draw:text-box><text:list><text:list-item><text:p>madde</text:p></text:list-item></text:list></draw:text-box></draw:frame></draw:page>
               </office:presentation>"#,
        );
        let bytes = package(
            "application/vnd.oasis.opendocument.presentation",
            &[("content.xml", xml.as_bytes())],
        );
        let flow = flow(&bytes).unwrap();
        let slides: Vec<&scour_flow::Slide> = flow
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Slide(s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(slides.len(), 2);
        let said: Vec<String> = slides[0]
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Para(p) => Some(p.text()),
                _ => None,
            })
            .collect();
        assert_eq!(said, ["Başlık", "üstteki", "alttaki"]);
        assert_eq!(slides[0].background, Some([0x10, 0x20, 0x30]));
        assert!(matches!(&slides[1].blocks[0], Block::Para(p) if p.mark == "•"));
    }

    #[test]
    fn a_picture_in_a_paragraph_follows_it_sized_by_its_frame() {
        let png = b"\x89PNG\r\n\x1a\nrest";
        let xml = content(
            "",
            r#"<office:text><text:p>önce<draw:frame svg:width="2in" svg:height="1in"><draw:image xlink:href="Pictures/a.png"/></draw:frame></text:p></office:text>"#,
        );
        let bytes = package(
            "application/vnd.oasis.opendocument.text",
            &[("content.xml", xml.as_bytes()), ("Pictures/a.png", png)],
        );
        let flow = flow(&bytes).unwrap();
        let picture = flow.blocks.iter().find_map(|b| match b {
            Block::Picture(p) => Some(p),
            _ => None,
        });
        let picture = picture.expect("the picture");
        assert_eq!(picture.mime, "image/png");
        // Two inches against a body of twelve points.
        assert!((picture.width - 12.0).abs() < 0.01, "{}", picture.width);
    }

    #[test]
    fn what_is_not_a_package_is_refused() {
        assert!(flow(b"not a zip").is_err());
        let bytes = package("application/vnd.oasis.opendocument.text", &[]);
        assert!(flow(&bytes).is_err());
    }
}
