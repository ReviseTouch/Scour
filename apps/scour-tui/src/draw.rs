//! One frame.
//!
//! Immediate mode: everything on screen is written on every frame, and
//! ratatui sends only the cells that differ. There is no widget tree to keep
//! in step with the state, which is the class of bug that cost the window a
//! week — a row destroyed between a press and its release.
//!
//! The layout is the page's and the window's, so that the same thing is in the
//! same place in all three: the query on top, the meter under it, the list
//! filling what is left, one line at the bottom.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
};

use scour_ui::format;

use crate::app::{App, Mode};
use crate::theme::Theme;

/// Lines the list does not get: query, meter, heading, footer.
const CHROME: u16 = 4;

/// How much room the list has, given a terminal this tall.
pub fn room(height: u16) -> usize {
    height.saturating_sub(CHROME).max(1) as usize
}

pub fn frame(f: &mut Frame, app: &App, theme: &Theme, mark: (char, char)) {
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(theme.back())), area);
    let [top, meter, heads, list, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);

    query(f, top, app, theme);
    counts(f, meter, app, theme, mark);
    heading(f, heads, theme);
    rows(f, list, app, theme, mark);
    footer(f, foot, app, theme);
}

/// The query line: the brand, what has been typed, and the caret.
fn query(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let typed = if app.query.is_empty() {
        Span::styled(
            "file name  ·  ext:pdf  ·  kind:image dm:7d  ·  size:>10mb",
            Style::new().fg(theme.ink_3()),
        )
    } else {
        Span::styled(
            app.query.as_str(),
            Style::new().fg(theme.ink()).add_modifier(Modifier::BOLD),
        )
    };
    let line = Line::from(vec![
        Span::styled(
            " SCOUR ",
            Style::new()
                .fg(theme.key())
                .add_modifier(Modifier::BOLD | Modifier::DIM),
        ),
        typed,
    ]);
    f.render_widget(Paragraph::new(line), area);
    // The caret is the terminal's own, which means it blinks the way every
    // other caret on that screen blinks — and costs nothing to keep alive.
    if app.mode == Mode::Search {
        let before = app.query[..app.caret].chars().count() as u16;
        f.set_cursor_position((area.x + 7 + before, area.y));
    }
}

/// Shown, total, what it cost.
fn counts(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
    if !app.trouble.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {}", app.trouble),
                Style::new().fg(theme.bad()),
            ))),
            area,
        );
        return;
    }
    let total = app.pages.total();
    let shown = app.room.min(total);
    let dim = Style::new().fg(theme.ink_3());
    let mut parts = vec![
        Span::styled(" ", dim),
        Span::styled(
            format!(
                "{} / {}{}",
                format::grouped(shown as u64, mark.0),
                if app.capped { "≥" } else { "" },
                format::grouped(total as u64, mark.0)
            ),
            Style::new().fg(theme.ink_2()),
        ),
    ];
    if app.took_us > 0 {
        let ms = app.took_us as f64 / 1000.0;
        parts.push(Span::styled("  ·  ", dim));
        parts.push(Span::styled(
            format!("{ms:.2} ms").replace('.', &mark.1.to_string()),
            Style::new().fg(theme.key()),
        ));
    }
    if app.rows_visited > 0 {
        parts.push(Span::styled("  ·  ", dim));
        parts.push(Span::styled(
            format!("{} rows read", format::grouped(app.rows_visited, mark.0)),
            dim,
        ));
    }
    f.render_widget(Paragraph::new(Line::from(parts)), area);
}

/// The column names, in the page's order.
fn heading(f: &mut Frame, area: Rect, theme: &Theme) {
    let style = Style::new().fg(theme.ink_3()).add_modifier(Modifier::DIM);
    let names = ["  NAME", "KIND", "WHERE", "CHANGED", "SIZE"];
    let cells: Vec<Cell> = names
        .iter()
        .map(|n| Cell::from(Span::styled(*n, style)))
        .collect();
    f.render_widget(
        Table::new(vec![Row::new(cells)], widths()).column_spacing(1),
        area,
    );
}

/// What each column gets. `Fill` on the two that can take it, so a narrow
/// terminal eats the path before it eats the name.
fn widths() -> [Constraint; 5] {
    [
        Constraint::Fill(2),
        Constraint::Length(10),
        Constraint::Fill(3),
        Constraint::Length(16),
        Constraint::Length(10),
    ]
}

fn rows(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
    let total = app.pages.total();
    // The scrollbar gets a column of its own rather than being drawn over the
    // list: on top it sits in the size column, which is the one column where a
    // character in the wrong place reads as part of the number.
    let scrolling = total > app.room;
    let (area, bar) = if scrolling {
        let [list, bar] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
        (list, Some(bar))
    } else {
        (area, None)
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut drawn: Vec<Row> = Vec::with_capacity(app.room);
    for row in app.top..(app.top + app.room).min(total.max(app.top)) {
        let here = row == app.cursor;
        let Some(hit) = app.pages.at(row) else {
            // A row whose page has not arrived. Drawn as a blank rather than
            // skipped, so the list keeps its shape while it comes.
            drawn.push(Row::new(vec![Cell::from("")]));
            continue;
        };
        let band = format::band(now, hit.meta.mtime);
        let name = scour_ui::path::leaf(&hit.path);
        let ink = if here { theme.ink() } else { theme.ink_2() };
        let line = Style::new().fg(ink);
        let cells = vec![
            Cell::from(Line::from(vec![
                // The age stripe: one cell of colour, the same six bands the
                // window draws down the left of every row.
                Span::styled("▎", Style::new().fg(theme.band(band))),
                Span::styled(if here { "▸" } else { " " }, Style::new().fg(theme.key())),
                Span::styled(name.to_string(), line),
            ])),
            Cell::from(Span::styled(
                hit.kind.token().to_string(),
                Style::new().fg(theme.kind(hit.kind.token())),
            )),
            Cell::from(Span::styled(
                scour_ui::path::folder(&hit.path).to_string(),
                Style::new().fg(theme.ink_3()),
            )),
            Cell::from(Span::styled(format::stamp(hit.meta.mtime), line)),
            Cell::from(Span::styled(
                if hit.is_dir {
                    String::new()
                } else {
                    format::size(hit.meta.size.max(0) as u64, mark.1)
                },
                line,
            )),
        ];
        let style = if here {
            Style::new().bg(theme.pick())
        } else {
            Style::new()
        };
        drawn.push(Row::new(cells).style(style));
    }
    f.render_widget(Table::new(drawn, widths()).column_spacing(1), area);

    // Our own scrollbar, on the right, because the list is a window onto a
    // result rather than a scrolled buffer: ratatui's needs to be told where
    // it is, and where it is is the cursor's row in the whole result.
    if let Some(bar) = bar {
        let mut state = ScrollbarState::new(total.saturating_sub(app.room)).position(app.top);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .style(Style::new().fg(theme.line()))
                .thumb_style(Style::new().fg(theme.ink_3())),
            bar,
            &mut state,
        );
    }
}

/// The one line at the bottom: where the cursor is, and which mode.
fn footer(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let dim = Style::new().fg(theme.ink_3());
    let path = app
        .here()
        .map(|h| h.path.clone())
        .unwrap_or_else(|| "—".into());
    let mode = match app.mode {
        Mode::Search => "search",
        Mode::Move => "move",
    };
    let [left, right] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(10)]).areas(area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {path}"), dim))),
        left,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{mode} "),
            Style::new().fg(theme.key()),
        )))
        .right_aligned(),
        right,
    );
}
