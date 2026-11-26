//! The content field's pattern drawn as the search reads it: every piece but
//! the one the caret is in as a capsule (`scour_ui::pattern`), a quiet "and"
//! between two capsules, the pill over the chosen one, and the `:` menu
//! under the caret. The pattern is the only model: this is laid out again
//! from its text every frame.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use scour_ui::pattern::{self, AND, Alt, Item, OR, PieceKind};

use crate::app::{App, Spot};
use crate::theme::Theme;
use crate::v2::{Deed, Pill};

/// What a run of the laid-out field is, for its colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// A letter of the piece being typed, or of text outside every piece.
    Letter,
    /// An operator of the piece being typed: `!`, `"`, `|`, `~`…
    Op(char),
    /// Space between pieces, drawn as typed.
    Gap,
    /// The quiet word between two capsules.
    And,
    /// A capsule's own ground: its ends and the space round its words.
    Ground,
    /// A capsule's head: "exact phrase", "either", "without".
    Head,
    /// A one-alternative capsule's words.
    Body,
    /// One alternative of several, on a ground of its own.
    Alt,
    /// The quiet word between two alternatives.
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub ink: Ink,
    /// Part of an excluded piece: drawn in the exclusion's red.
    pub not: bool,
}

/// Where a capsule was drawn, in columns from the field's first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapAt {
    pub piece: usize,
    pub from: usize,
    pub to: usize,
    /// Each alternative's columns, in the piece's order.
    pub alts: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Laid {
    pub runs: Vec<Run>,
    /// The column the caret is drawn at.
    pub caret: usize,
    pub caps: Vec<CapAt>,
    pub width: usize,
}

#[cfg(test)]
impl Laid {
    /// What is drawn, as one string.
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

struct Pen {
    runs: Vec<Run>,
    col: usize,
    caret: Option<usize>,
}

impl Pen {
    fn put(&mut self, text: String, ink: Ink, not: bool) {
        self.col += text.chars().count();
        self.runs.push(Run { text, ink, not });
    }

    /// `chars[a..b]` as typed, one run per character, noting the caret.
    fn raw(
        &mut self,
        chars: &[char],
        a: usize,
        b: usize,
        caret: usize,
        ink: impl Fn(usize) -> Ink,
    ) {
        for (i, &c) in chars.iter().enumerate().take(b).skip(a) {
            if i == caret {
                self.caret.get_or_insert(self.col);
            }
            self.put(c.to_string(), ink(i), false);
        }
        if b == caret {
            self.caret.get_or_insert(self.col);
        }
    }
}

/// An alternative's words as a capsule shows them: `≈` before a forgiving
/// one, curly quotes round a phrase.
fn alt_text(chars: &[char], a: &Alt) -> String {
    let words: String = chars[a.start..a.end].iter().collect();
    let words = if a.phrase {
        format!("“{words}”")
    } else {
        words
    };
    if a.similar {
        format!("≈{words}")
    } else {
        words
    }
}

/// The field for `input` with the caret at character `caret`. `say`
/// translates a msgid.
pub fn lay(input: &str, caret: usize, say: impl Fn(&str) -> String) -> Laid {
    let chars: Vec<char> = input.chars().collect();
    let caret = caret.min(chars.len());
    let pieces = pattern::pieces(input);
    let edited = pattern::piece_at(&pieces, caret);
    let capsule = |k: usize| Some(k) != edited;
    let mut pen = Pen {
        runs: Vec::new(),
        col: 0,
        caret: None,
    };
    let mut caps = Vec::new();
    let mut pos = 0;
    for (k, p) in pieces.iter().enumerate() {
        // "and" only between two capsules, and never over the caret: a caret
        // in the space would have nowhere to stand.
        let and = k > 0 && capsule(k - 1) && capsule(k) && !(pos < caret && caret < p.start);
        if and {
            pen.put(format!(" {} ", say(AND)), Ink::And, false);
        } else {
            pen.raw(&chars, pos, p.start, caret, |_| Ink::Gap);
        }
        pos = p.end;
        if !capsule(k) {
            pen.raw(&chars, p.start, p.end, caret, |i| {
                if p.ops.contains(&i) {
                    Ink::Op(chars[i])
                } else {
                    Ink::Letter
                }
            });
            continue;
        }
        let not = p.kind == PieceKind::Not;
        let from = pen.col;
        pen.put(" ".into(), Ink::Ground, not);
        // A similar word says so with its `≈`: a head as well would say it twice.
        if let Some(head) = p.kind.label().filter(|_| p.kind != PieceKind::Similar) {
            pen.put(format!("{} ", say(head)), Ink::Head, not);
        }
        let inner = p.alts.len() > 1;
        let mut alts = Vec::new();
        if p.alts.is_empty() {
            pen.put(chars[p.start..p.end].iter().collect(), Ink::Body, not);
        }
        for (j, a) in p.alts.iter().enumerate() {
            if j > 0 {
                pen.put(format!(" {} ", say(OR)), Ink::Or, not);
            }
            let s = pen.col;
            let ink = if inner { Ink::Alt } else { Ink::Body };
            pen.put(alt_text(&chars, a), ink, not);
            alts.push((s, pen.col));
        }
        pen.put(" ".into(), Ink::Ground, not);
        caps.push(CapAt {
            piece: k,
            from,
            to: pen.col,
            alts,
        });
    }
    pen.raw(&chars, pos, chars.len(), caret, |_| Ink::Gap);
    let width = pen.col;
    Laid {
        runs: pen.runs,
        caret: pen.caret.unwrap_or(width),
        caps,
        width,
    }
}

/// The runs in their colours, on the field's ground.
pub fn spans(laid: &Laid, theme: &Theme) -> Vec<Span<'static>> {
    let field = theme.hover();
    laid.runs
        .iter()
        .map(|r| {
            let (ground, ring, ink) = if r.not {
                (theme.cap_neg(), theme.cap_neg_ring(), theme.not())
            } else {
                (theme.cap_read(), theme.cap_read_ring(), theme.ink())
            };
            let bold = Modifier::BOLD;
            let style = match r.ink {
                Ink::Letter => Style::new().fg(theme.ink()).bg(field).add_modifier(bold),
                Ink::Op(c) => Style::new()
                    .fg(match c {
                        '!' => theme.not(),
                        '"' => theme.glob(),
                        _ => theme.key(),
                    })
                    .bg(field)
                    .add_modifier(bold),
                Ink::Gap => Style::new().bg(field),
                Ink::And => Style::new().fg(theme.ink_3()).bg(field),
                Ink::Ground => Style::new().bg(ground),
                Ink::Head => Style::new()
                    .fg(if r.not { theme.not() } else { theme.bad() })
                    .bg(ground),
                Ink::Body => Style::new().fg(ink).bg(ground).add_modifier(bold),
                Ink::Alt => Style::new().fg(ink).bg(ring).add_modifier(bold),
                Ink::Or => Style::new().fg(theme.ink_3()).bg(ground),
            };
            Span::styled(r.text.clone(), style)
        })
        .collect()
}

/// What the pill over a piece offers, in its order: a letter off for an
/// alternative that can have it, the exclusion on or off, and away.
pub fn deeds(input: &str, pill: &Pill) -> Vec<Deed> {
    let pieces = pattern::pieces(input);
    let Some(p) = pieces.get(pill.piece) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if p.alts.get(pill.alt).is_some_and(|a| !a.phrase) {
        out.push(Deed::Similar);
    }
    out.push(Deed::Not);
    out.push(Deed::Remove);
    out
}

/// A pill button's face.
fn face(deed: Deed, input: &str, piece: usize) -> &'static str {
    match deed {
        Deed::Similar => "≈",
        Deed::Not if pattern::pieces(input).get(piece).map(|p| p.kind) == Some(PieceKind::Not) => {
            "+"
        }
        Deed::Not => "!",
        Deed::Remove => "✕",
    }
}

/// The keys that do what the pill's buttons do, said after them when the
/// capsule was chosen from the keyboard.
const PILL_KEYS: &str = " ~ ! Del";

/// The pill's buttons, each with its columns on the line, the field starting
/// at `origin`: three columns a button, one between, the last ending where
/// the capsule ends — or as far left as the line allows.
pub fn pill_cells(
    input: &str,
    laid: &Laid,
    pill: &Pill,
    origin: usize,
) -> Vec<(Deed, usize, usize)> {
    let Some(cap) = laid.caps.iter().find(|c| c.piece == pill.piece) else {
        return Vec::new();
    };
    let deeds = deeds(input, pill);
    let wide = deeds.len() * 4 - 1;
    let mut at = (origin + cap.to).saturating_sub(wide);
    deeds
        .into_iter()
        .map(|d| {
            let cell = (d, at, at + 3);
            at += 4;
            cell
        })
        .collect()
}

/// The pill on the row above the field, which starts at column `origin`.
pub(super) fn pill(f: &mut Frame, row: Rect, origin: usize, app: &App, laid: &Laid, theme: &Theme) {
    let Some(pill) = app.v2.grep_pill else {
        return;
    };
    let input = &app.v2.grep_pattern;
    let cells = pill_cells(input, laid, &pill, origin);
    let Some(&(_, first, _)) = cells.first() else {
        return;
    };
    let mut parts = Vec::new();
    for (k, &(deed, _, _)) in cells.iter().enumerate() {
        if k > 0 {
            parts.push(Span::raw(" "));
        }
        let style = if app.pressed == Spot::GrepPill(k) {
            Style::new().fg(theme.back()).bg(theme.key())
        } else if app.hover == Spot::GrepPill(k) {
            Style::new().fg(theme.ink()).bg(theme.hover())
        } else {
            Style::new().fg(theme.ink_2()).bg(theme.line())
        };
        let ink = match deed {
            Deed::Not | Deed::Remove => style.fg(if app.hover == Spot::GrepPill(k) {
                theme.not()
            } else {
                style.fg.unwrap_or(theme.ink_2())
            }),
            Deed::Similar => style,
        };
        parts.push(Span::styled(
            format!(" {} ", face(deed, input, pill.piece)),
            ink.add_modifier(Modifier::BOLD),
        ));
    }
    if pill.by_key {
        parts.push(Span::styled(PILL_KEYS, Style::new().fg(theme.ink_3())));
    }
    let from = row.x + first as u16;
    if from >= row.x + row.width {
        return;
    }
    let area = Rect {
        x: from,
        width: row.x + row.width - from,
        ..row
    };
    f.render_widget(Paragraph::new(Line::from(parts)), area);
}

/// The glyph an item's icon names, a column wide.
fn icon(name: &str) -> &'static str {
    match name {
        "quote" => "“",
        "or" => "∨",
        "not" => "⊘",
        "approx" => "≈",
        "case" => "A",
        _ => " ",
    }
}

/// A line of the `:` menu, without the cursor's three columns.
fn menu_line(app: &App, item: &Item, label: usize, about: usize, sign: usize) -> String {
    let pad = |s: &str, n: usize| format!("{s}{}", " ".repeat(n.saturating_sub(s.chars().count())));
    format!(
        "{} {}  {}  {}{} ",
        icon(item.icon),
        pad(&app.say(item.label), label),
        pad(&app.say(item.about), about),
        " ".repeat(sign.saturating_sub(item.sign.chars().count())),
        item.sign,
    )
}

/// Where the `:` menu is drawn, and what it offers: a box whose left edge is
/// under the `:`, its first line on the row under the field.
pub fn menu_rect(app: &App, area: Rect) -> Option<(Rect, Vec<Item>)> {
    let (_, typed, items) = app.grep_menu()?;
    let laid = super::grep_laid(app)?;
    let w = |f: fn(&Item) -> &str| {
        items
            .iter()
            .map(|i| app.say(f(i)).chars().count())
            .max()
            .unwrap_or(0)
    };
    let (label, about) = (w(|i| i.label), w(|i| i.about));
    let sign = items
        .iter()
        .map(|i| i.sign.chars().count())
        .max()
        .unwrap_or(0);
    // Border, cursor, icon and space, the columns, the gaps, the end space.
    let wide = (2 + 3 + 2 + label + 2 + about + 2 + sign + 1) as u16;
    let at = laid.caret.saturating_sub(typed.chars().count() + 1);
    let x = (area.x + super::grep_origin(app, area.width) as u16 + at as u16)
        .min((area.x + area.width).saturating_sub(wide));
    let y = area.y + super::QUERY_ROW + 1;
    let high = (items.len() as u16 + 2).min(area.height.saturating_sub(y - area.y));
    Some((
        Rect {
            x,
            y,
            width: wide.min(area.width),
            height: high,
        },
        items,
    ))
}

/// The menu's item drawn at this cell.
pub fn content_menu_at(app: &App, area: Rect, col: u16, row: u16) -> Option<usize> {
    let (r, items) = menu_rect(app, area)?;
    let inside = col > r.x && col + 1 < r.x + r.width && row > r.y && row + 1 < r.y + r.height;
    let at = (row - r.y).checked_sub(1)? as usize;
    (inside && at < items.len()).then_some(at)
}

/// The `:` menu, over whatever is under the field.
pub fn menu(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some((r, items)) = menu_rect(app, area) else {
        return;
    };
    let w = |f: fn(&Item) -> &str| {
        items
            .iter()
            .map(|i| app.say(f(i)).chars().count())
            .max()
            .unwrap_or(0)
    };
    let (label, about) = (w(|i| i.label), w(|i| i.about));
    let sign = items
        .iter()
        .map(|i| i.sign.chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(k, item)| {
            let on = k == app.v2.grep_menu_at.min(items.len() - 1);
            let lit = if app.pressed == Spot::GrepMenu(k) {
                Style::new().bg(theme.key()).fg(theme.back())
            } else if app.hover == Spot::GrepMenu(k) {
                Style::new().bg(theme.hover())
            } else {
                Style::new()
            };
            let body = menu_line(app, item, label, about, sign);
            let head: String = body.chars().take(3 + label).collect();
            let rest: String = body.chars().skip(3 + label).collect();
            let cut = rest.chars().count().saturating_sub(sign + 1);
            let about_part: String = rest.chars().take(cut).collect();
            let sign_part: String = rest.chars().skip(cut).collect();
            let ink = if on { theme.key() } else { theme.ink_2() };
            Line::from(vec![
                Span::styled(if on { " ▸ " } else { "   " }, Style::new().fg(ink)),
                Span::styled(head, Style::new().fg(ink)),
                Span::styled(about_part, Style::new().fg(theme.ink_3())),
                Span::styled(sign_part, Style::new().fg(theme.ink_3())),
            ])
            .style(lit)
        })
        .collect();
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_style(Style::new().fg(theme.line()))
                .style(Style::new().bg(theme.back())),
        ),
        r,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(m: &str) -> String {
        m.to_string()
    }

    const ASKED: &str = r#"icra "dava dosyası" ~rapor a|~b !taslak"#;

    #[test]
    fn every_piece_but_the_typed_one_is_a_capsule() {
        let n = ASKED.chars().count();
        let laid = lay(ASKED, n, said);
        assert_eq!(
            laid.text(),
            " icra  and  exact phrase “dava dosyası”  and  ≈rapor  and  either a or ≈b  !taslak"
        );
        assert_eq!(laid.caps.len(), 4);
        assert_eq!(laid.caret, laid.width, "the caret after the letters");
        // The Or capsule: each alternative its own run, "or" between.
        let or = &laid.caps[3];
        let cols: Vec<String> = or
            .alts
            .iter()
            .map(|&(a, b)| laid.text().chars().skip(a).take(b - a).collect())
            .collect();
        assert_eq!(cols, ["a", "≈b"]);
    }

    #[test]
    fn the_piece_the_caret_is_in_stays_letters() {
        // Inside `~rapor`: it is letters, its neighbours capsules, and no
        // "and" on either side of it.
        let at = ASKED.find("rapor").unwrap();
        let caret = ASKED[..at].chars().count() + 2;
        let laid = lay(ASKED, caret, said);
        assert_eq!(
            laid.text(),
            " icra  and  exact phrase “dava dosyası”  ~rapor  either a or ≈b  and  without taslak "
        );
        let before: String = laid.text().chars().take(laid.caret).collect();
        assert!(before.ends_with(" ~ra"), "{before:?}");
        let ops: Vec<Ink> = laid
            .runs
            .iter()
            .filter(|r| matches!(r.ink, Ink::Op(_)))
            .map(|r| r.ink)
            .collect();
        assert_eq!(ops, [Ink::Op('~')]);
    }

    #[test]
    fn an_exclusion_is_red_and_its_alternatives_joined_by_or() {
        let laid = lay("x !a|\"b c\"", 0, said);
        assert_eq!(laid.text(), "x  without a or “b c” ");
        assert!(laid.runs.iter().skip(1).all(|r| r.not || r.ink == Ink::Gap));
        assert_eq!(laid.caps[0].alts.len(), 2);
    }

    #[test]
    fn a_caret_in_the_space_between_two_capsules_keeps_the_space() {
        let laid = lay("ab   cd", 3, said);
        assert_eq!(laid.text(), " ab     cd ");
        assert_eq!(laid.caret, 5);
    }

    #[test]
    fn the_pill_ends_where_its_capsule_ends() {
        let laid = lay("ab cd", 0, said);
        let pill = Pill {
            piece: 1,
            alt: 0,
            by_key: true,
        };
        let cells = pill_cells("ab cd", &laid, &pill, 20);
        let deeds: Vec<Deed> = cells.iter().map(|c| c.0).collect();
        assert_eq!(deeds, [Deed::Similar, Deed::Not, Deed::Remove]);
        assert_eq!(cells.last().unwrap().2, 20 + laid.caps[0].to);
        // Against the line's left edge it starts there instead.
        assert_eq!(pill_cells("ab cd", &laid, &pill, 0)[0].1, 0);
        // A phrase cannot be forgiving: no ≈ over it.
        let laid = lay("\"a b\" x", 7, said);
        let pill = Pill {
            piece: 0,
            alt: 0,
            by_key: false,
        };
        let deeds: Vec<Deed> = pill_cells("\"a b\" x", &laid, &pill, 20)
            .iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(deeds, [Deed::Not, Deed::Remove]);
    }

    /// The whole frame, the mouse and the menu agree on where things are:
    /// a capsule under its columns, the pill above it, the menu under `:`.
    #[test]
    fn what_is_drawn_is_where_the_mouse_finds_it() {
        use crate::app::Mode;
        let theme = crate::theme::Theme {
            palette: &scour_ui::DARK,
            truecolor: true,
            theirs: false,
        };
        let (w, h) = (110u16, 24u16);
        let mut app = App {
            mode: Mode::Search,
            query: "kind:doc".into(),
            ..Default::default()
        };
        app.caret = app.query.len();
        app.v2.grep_open = true;
        app.v2.grep_pattern = ASKED.into();
        app.v2.grep_caret = ASKED.chars().count();
        let shot = |app: &App| {
            let mut term =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            term.draw(|f| crate::draw::frame(f, app, &theme)).unwrap();
            let buf = term.backend().buffer().clone();
            let row = |y: u16| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            };
            (
                (0..h).map(row).collect::<Vec<_>>(),
                term.get_cursor_position().unwrap().x,
            )
        };
        let origin = crate::draw::grep_origin(&app, w);
        let laid = crate::draw::grep_laid(&app).unwrap();
        let (rows, caret) = shot(&app);
        let field: String = rows[1].chars().skip(origin).take(laid.width).collect();
        assert_eq!(field, laid.text());
        assert_eq!(caret as usize, origin + laid.caret);

        // The pointer on the Or capsule's `≈b`: that capsule, that alternative.
        let or = &laid.caps[3];
        let (b_from, _) = or.alts[1];
        let at = (origin + b_from) as u16;
        let spot = crate::keys::spot_at(&app, at, 1, (w, h));
        assert_eq!(spot, Spot::GrepCap(3, 1));
        let moved = ratatui::crossterm::event::MouseEvent {
            kind: ratatui::crossterm::event::MouseEventKind::Moved,
            column: at,
            row: 1,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        };
        crate::keys::mouse(&mut app, moved, (w, h));
        assert_eq!(app.v2.grep_pill.map(|p| (p.piece, p.alt)), Some((3, 1)));
        let (rows, _) = shot(&app);
        let pill: String = rows[0].chars().skip(origin + or.to - 11).take(11).collect();
        assert_eq!(pill, " ≈   !   ✕ ");
        // Its ✕ takes the Or piece away.
        let x = (origin + or.to - 2) as u16;
        assert_eq!(crate::keys::spot_at(&app, x, 0, (w, h)), Spot::GrepPill(2));
        for kind in [
            ratatui::crossterm::event::MouseEventKind::Down(
                ratatui::crossterm::event::MouseButton::Left,
            ),
            ratatui::crossterm::event::MouseEventKind::Up(
                ratatui::crossterm::event::MouseButton::Left,
            ),
        ] {
            let e = ratatui::crossterm::event::MouseEvent {
                kind,
                column: x,
                row: 0,
                modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
            };
            crate::keys::mouse(&mut app, e, (w, h));
        }
        assert_eq!(app.v2.grep_pattern, r#"icra "dava dosyası" ~rapor !taslak"#);

        // `icra :` — the menu's box starts under the `:`.
        app.v2.grep_pattern = "icra :".into();
        app.v2.grep_caret = 6;
        app.v2.grep_pill = None;
        let area = Rect::new(0, 0, w, h);
        let (r, items) = menu_rect(&app, area).unwrap();
        assert_eq!(items.len(), 5);
        let origin = crate::draw::grep_origin(&app, w);
        let colon = crate::draw::grep_laid(&app).unwrap().caret - 1;
        assert_eq!(r.x as usize, origin + colon);
        assert_eq!(colon, 7, "after the capsule ` icra ` and its space");
        assert_eq!(r.y, 2);
        let (rows, _) = shot(&app);
        assert!(rows[3].contains("Exact phrase"), "{}", rows[3]);
        assert_eq!(content_menu_at(&app, area, r.x + 4, r.y + 2), Some(1));
        assert_eq!(
            content_menu_at(&app, area, r.x + 4, r.y),
            None,
            "the border"
        );
    }
}
