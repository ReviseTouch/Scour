//! The row menu and the tool strip, drawn and hit at the same cells.

use crate::app::{App, Spot};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

/// One row of the menu box, top to bottom inside the border.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuRow {
    Title,
    Detail,
    Gap,
    Strip,
    Rule,
    /// An item of the list, by its place in [`App::menu`].
    Item(usize),
}

/// The menu's rows, as `scour-ui::menu` splits it: what it is about, the
/// strip, and the list with a rule wherever the group changes.
fn menu_rows(app: &App) -> Vec<MenuRow> {
    let strip = app.strip_len();
    let mut rows = vec![MenuRow::Title, MenuRow::Detail];
    if strip > 0 {
        rows.extend([MenuRow::Gap, MenuRow::Strip, MenuRow::Gap]);
    }
    rows.push(MenuRow::Rule);
    for (at, line) in app.menu.iter().enumerate().skip(strip) {
        if line.rule {
            rows.push(MenuRow::Rule);
        }
        rows.push(MenuRow::Item(at));
    }
    rows
}

/// Where the menu box sits: as tall as its rows, and no taller than the screen.
pub(super) fn menu_rect(area: Rect, app: &App) -> Rect {
    let wide = 50u16.min(area.width.saturating_sub(4));
    let tall = (menu_rows(app).len() as u16 + 2).min(area.height.saturating_sub(2));
    Rect {
        x: area.x + area.width.saturating_sub(wide) / 2,
        y: area.y + 2,
        width: wide,
        height: tall,
    }
}

/// The strip's words, each as the columns it covers: ` Yol ` and a space.
pub(super) fn strip_cells(app: &App, box_area: Rect) -> Vec<(u16, u16)> {
    let mut x = box_area.x + 3;
    app.menu
        .iter()
        .take_while(|m| m.strip)
        .map(|m| {
            let wide = m.label.chars().count() as u16 + 2;
            let cell = (x, x + wide);
            x += wide + 1;
            cell
        })
        .collect()
}

/// Which item of the menu is at this cell, if any: the mouse's question.
pub fn menu_item_at(app: &App, area: Rect, col: u16, row: u16) -> Option<usize> {
    let box_area = menu_rect(area, app);
    if col <= box_area.x || col + 1 >= box_area.x + box_area.width || row <= box_area.y {
        return None;
    }
    match menu_rows(app).get((row - box_area.y - 1) as usize)? {
        MenuRow::Item(at) => Some(*at),
        MenuRow::Strip => strip_cells(app, box_area)
            .iter()
            .position(|(from, to)| col >= *from && col < *to),
        _ => None,
    }
}

/// The right-click menu: a heading, the strip of a few words, and the list.
pub(super) fn menu_panel(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let box_area = menu_rect(area, app);
    f.render_widget(Clear, box_area);
    let inner = box_area.width.saturating_sub(2) as usize;
    let lit = |at: usize| {
        if app.pressed == Spot::Panel(at) {
            Style::new().bg(theme.key()).fg(theme.back())
        } else if app.hover == Spot::Panel(at) {
            Style::new().bg(theme.hover())
        } else {
            Style::new()
        }
    };
    let mut drawn: Vec<Line> = Vec::new();
    for row in menu_rows(app) {
        drawn.push(match row {
            MenuRow::Title => Line::from(Span::styled(
                format!(" {}", app.menu_title),
                Style::new().fg(theme.ink()).add_modifier(Modifier::BOLD),
            )),
            MenuRow::Detail => Line::from(Span::styled(
                format!(" {}", app.menu_detail),
                Style::new().fg(theme.ink_3()),
            )),
            MenuRow::Gap => Line::from(""),
            MenuRow::Rule => Line::from(Span::styled(
                format!(" {}", "─".repeat(inner.saturating_sub(2))),
                Style::new().fg(theme.line()),
            )),
            // A word each, on a ground of its own; the cursor's is lit. Red
            // only under the cursor, as everywhere.
            MenuRow::Strip => {
                let mut spans = vec![Span::raw("  ")];
                for (at, m) in app.menu.iter().enumerate().take_while(|(_, m)| m.strip) {
                    let on = at == app.panel_at;
                    let style = if on && m.careful {
                        Style::new().bg(theme.danger()).fg(theme.back())
                    } else if on {
                        Style::new()
                            .bg(theme.key())
                            .fg(theme.back())
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::new()
                            .bg(theme.hover())
                            .fg(theme.ink_2())
                            .patch(lit(at))
                    };
                    spans.push(Span::styled(format!(" {} ", m.label), style));
                    spans.push(Span::raw(" "));
                }
                Line::from(spans)
            }
            MenuRow::Item(at) => {
                let line = &app.menu[at];
                let on = at == app.panel_at;
                // The shortcut is quieter than the label: it is to be learned.
                let room = inner.saturating_sub(5);
                let pad =
                    room.saturating_sub(line.label.chars().count() + line.key.chars().count());
                Line::from(vec![
                    Span::styled(
                        format!(
                            "{}{}{}",
                            if on { " ▸ " } else { "   " },
                            line.label,
                            " ".repeat(pad)
                        ),
                        Style::new().fg(if on && line.careful {
                            theme.danger()
                        } else if on {
                            theme.key()
                        } else if line.heavy {
                            theme.ink_3()
                        } else {
                            theme.ink_2()
                        }),
                    ),
                    Span::styled(line.key.clone(), Style::new().fg(theme.ink_3())),
                ])
                .style(lit(at))
            }
        });
    }
    f.render_widget(
        Paragraph::new(drawn).block(
            Block::bordered()
                .border_style(Style::new().fg(theme.line()))
                .style(Style::new().bg(theme.back())),
        ),
        box_area,
    );
}

/// Where each tool is drawn along the counter line, right to left. One function,
/// two callers: the words that are drawn are the words that are hit.
pub fn tool_spans(app: &App, width: u16) -> Vec<(u16, u16, String)> {
    let mut out = Vec::new();
    let mut from = width.saturating_sub(1);
    let all = app.tools();
    for (at, (label, key)) in all.iter().enumerate().rev() {
        let glyph = crate::icons::of_tool(at);
        // The msgid is looked up here, where its width is measured: one string.
        let label = app.say(label);
        let said = if glyph.is_empty() {
            format!("  {label} {key}")
        } else {
            format!("  {glyph} {label} {key}")
        };
        let wide = said.chars().count() as u16;
        from = from.saturating_sub(wide);
        out.push((from, from + wide, said));
    }
    out.reverse();
    out
}

/// Which tool is at this column of the counter line, if any.
pub fn tool_at(app: &App, col: u16, width: u16) -> Option<usize> {
    tool_spans(app, width)
        .iter()
        .position(|(from, to, _)| col >= *from && col < *to)
}

/// Where each of the selection's three buttons is drawn, along the bottom. The
/// same arithmetic that draws them: they are words a press acts on.
pub fn deed_spans(app: &App, width: u16) -> Vec<(u16, u16, String)> {
    let mut out = Vec::new();
    let mut from = width;
    for (label, _) in app.deeds().iter().rev() {
        let said = format!("  {}  ", app.say(label));
        let wide = said.chars().count() as u16;
        from = from.saturating_sub(wide);
        out.push((from, from + wide, said));
    }
    out.reverse();
    out
}

/// Which of them is at this column, if any.
pub fn deed_at(app: &App, col: u16, width: u16) -> Option<usize> {
    deed_spans(app, width)
        .iter()
        .position(|(from, to, _)| col >= *from && col < *to)
}
