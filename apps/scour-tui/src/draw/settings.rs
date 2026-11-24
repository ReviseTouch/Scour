//! The settings screen: sections on the left, the one chosen on the right.

use super::*;
use crate::app::{App, Spot};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

/// Where the settings screen puts things: the sections down the left, the
/// open section's lines on the right from `first`, one row each.
pub struct SettingsLayout {
    pub side: Rect,
    pub right: Rect,
    pub first: u16,
    pub room: usize,
    pub lead: Vec<String>,
}

/// How wide the sections' column is.
const SECTIONS_WIDE: u16 = 26;

pub fn settings_layout(area: Rect, app: &App) -> SettingsLayout {
    let body_y = area.y + 2;
    let body_h = area.height.saturating_sub(4);
    let side = Rect::new(area.x, body_y, SECTIONS_WIDE.min(area.width), body_h);
    let right_x = area.x + SECTIONS_WIDE + 2;
    let right = Rect::new(
        right_x,
        body_y,
        area.width.saturating_sub(SECTIONS_WIDE + 3),
        body_h,
    );
    let section = crate::app::SECTIONS[App::section_of(app.panel)];
    let lead: Vec<String> = wrapped(
        &app.say(section.2),
        right.width.saturating_sub(2) as usize,
        true,
    )
    .into_iter()
    .map(|l| l.text)
    .collect();
    let first = body_y + 2 + lead.len() as u16;
    let room = (body_y + body_h).saturating_sub(first) as usize;
    SettingsLayout {
        side,
        right,
        first,
        room,
        lead,
    }
}

/// The open section's lines, with the rows a rule takes: the row each line
/// is drawn on, counted from `first`, after scrolling the cursor into view.
fn settings_rows(app: &App, lines: &[PanelLine], room: usize) -> (usize, Vec<(usize, usize)>) {
    let mut rows = Vec::new();
    let mut row = 0usize;
    for (at, line) in lines.iter().enumerate() {
        if line.rule && at > 0 {
            row += 1;
        }
        rows.push((at, row));
        row += 1;
    }
    let cursor_row = rows.get(app.panel_at).map_or(0, |(_, r)| *r);
    let from = cursor_row.saturating_sub(room.saturating_sub(1));
    (from, rows)
}

/// What is under a press on the settings screen.
pub enum SettingsHit {
    Section(usize),
    Line(usize),
}

pub fn settings_hit(app: &App, area: Rect, col: u16, row: u16) -> Option<SettingsHit> {
    let at = settings_layout(area, app);
    if col < at.side.x + at.side.width && row >= at.side.y {
        let i = (row - at.side.y) as usize;
        return (i < crate::app::SECTIONS.len()).then_some(SettingsHit::Section(i));
    }
    if col < at.right.x || row < at.first {
        return None;
    }
    let (_, lines) = panel_content(app, at.right.width.saturating_sub(4) as usize)?;
    let (from, rows) = settings_rows(app, &lines, at.room);
    let want = (row - at.first) as usize + from;
    let pressable = app.panel_lines();
    rows.iter()
        .find(|(i, r)| *r == want && *i < pressable)
        .map(|(i, _)| SettingsHit::Line(*i))
}

pub(super) fn settings_screen(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    f.render_widget(Clear, area);
    f.render_widget(Block::new().style(Style::new().bg(theme.back())), area);
    let at = settings_layout(area, app);
    let wide = area.width as usize;
    let head = app.say("Settings").into_owned();
    let hint = app.say("Esc closes · Tab the next section").into_owned();
    let gap = wide.saturating_sub(head.chars().count() + hint.chars().count() + 3);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {head}"),
                Style::new().fg(theme.ink()).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" ".repeat(gap)),
            Span::styled(format!("{hint} "), Style::new().fg(theme.ink_3())),
        ])),
        Rect::new(area.x, area.y, area.width, 1),
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(wide),
            Style::new().fg(theme.line()),
        )),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );

    // The sections, the open one lit.
    let open = App::section_of(app.panel);
    let mut side = Vec::new();
    for (i, (_, title, _)) in crate::app::SECTIONS.iter().enumerate() {
        let text = format!("  {}", app.say(title));
        let pad = (SECTIONS_WIDE as usize).saturating_sub(text.chars().count());
        let lit = if app.pressed == Spot::Section(i) {
            Style::new().bg(theme.key()).fg(theme.back())
        } else if i == open {
            Style::new()
                .bg(theme.hover())
                .fg(theme.ink())
                .add_modifier(Modifier::BOLD)
        } else if app.hover == Spot::Section(i) {
            Style::new().bg(theme.hover()).fg(theme.ink_2())
        } else {
            Style::new().fg(theme.ink_2())
        };
        side.push(Line::from(Span::styled(
            format!("{text}{}", " ".repeat(pad)),
            lit,
        )));
    }
    f.render_widget(Paragraph::new(side), at.side);
    for y in at.side.y..at.side.y + at.side.height {
        f.render_widget(
            Paragraph::new(Span::styled("│", Style::new().fg(theme.line()))),
            Rect::new(at.side.x + SECTIONS_WIDE, y, 1, 1),
        );
    }

    // The section: its title, the line under it, then its lines.
    let Some((title, lines)) = panel_content(app, at.right.width.saturating_sub(4) as usize) else {
        return;
    };
    let mut drawn = vec![Line::from(Span::styled(
        title.into_owned(),
        Style::new().fg(theme.ink()).add_modifier(Modifier::BOLD),
    ))];
    for l in &at.lead {
        drawn.push(Line::from(Span::styled(
            l.clone(),
            Style::new().fg(theme.ink_3()),
        )));
    }
    drawn.push(Line::from(""));
    let (from, rows) = settings_rows(app, &lines, at.room);
    let pressable = app.panel_lines();
    let width = at.right.width as usize;
    let mut row = 0usize;
    for (at_line, line_row) in &rows {
        let line = &lines[*at_line];
        if line.rule && *at_line > 0 {
            if row >= from && row < from + at.room {
                drawn.push(Line::from(Span::styled(
                    "─".repeat(width.saturating_sub(2)),
                    Style::new().fg(theme.line()),
                )));
            }
            row += 1;
        }
        let _ = line_row;
        if row >= from && row < from + at.room {
            let on = *at_line == app.panel_at && *at_line < pressable;
            let lit = if app.pressed == Spot::Panel(*at_line) {
                Style::new().bg(theme.key()).fg(theme.back())
            } else if app.hover == Spot::Panel(*at_line) && *at_line < pressable {
                Style::new().bg(theme.hover())
            } else {
                Style::new()
            };
            let lead = if on { "▸ " } else { "  " };
            let text_w = line.text.chars().count() + 2;
            let pad = width.saturating_sub(text_w + line.key.chars().count() + 2);
            drawn.push(
                Line::from(vec![
                    Span::styled(
                        format!("{lead}{}", line.text),
                        Style::new().fg(if on && line.careful {
                            theme.danger()
                        } else if on {
                            theme.key()
                        } else if line.dimmed {
                            theme.ink_3()
                        } else {
                            theme.ink_2()
                        }),
                    ),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(
                        line.key.clone(),
                        Style::new().fg(if on { theme.key() } else { theme.ink_3() }),
                    ),
                ])
                .style(lit),
            );
        }
        row += 1;
    }
    f.render_widget(
        Paragraph::new(drawn),
        Rect::new(at.right.x, at.right.y, at.right.width, at.right.height),
    );

    // What the keys do here, at the foot.
    let foot = app
        .say("↑↓ move · ←→ change · Enter press · Tab section · Esc close")
        .into_owned();
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(wide),
            Style::new().fg(theme.line()),
        )),
        Rect::new(
            area.x,
            area.y + area.height.saturating_sub(2),
            area.width,
            1,
        ),
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            format!(" {foot}"),
            Style::new().fg(theme.ink_3()),
        )),
        Rect::new(
            area.x,
            area.y + area.height.saturating_sub(1),
            area.width,
            1,
        ),
    );
    if !app.note.is_empty() {
        let note = app.note.clone();
        let w = note.chars().count() as u16 + 2;
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(" {note} "),
                Style::new().fg(theme.danger()),
            )),
            Rect::new(
                area.x + area.width.saturating_sub(w + 1),
                area.y + area.height.saturating_sub(1),
                w.min(area.width),
                1,
            ),
        );
    }
}
