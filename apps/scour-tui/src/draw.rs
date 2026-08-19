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
    Block, Cell, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
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
    // The strip of time is worth two lines and only where there are lines to
    // spare: under twenty rows it would take a fifth of the list.
    let strip_high = if area.height >= 20 && !app.strip.is_empty() {
        2
    } else {
        0
    };
    let [top, meter, heads, list, strip, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(strip_high),
        Constraint::Length(1),
    ])
    .areas(area);

    query(f, top, app, theme);
    counts(f, meter, app, theme, mark);
    // **The rail goes when the terminal is narrow.** Twenty-two columns out of
    // eighty is a quarter of the list, and the list is what somebody came for.
    let wide = area.width >= 80 && app.rail;
    let (heads, list, rail) = if wide {
        let [rail, heads] =
            Layout::horizontal([Constraint::Length(22), Constraint::Fill(1)]).areas(heads);
        let [_, list] =
            Layout::horizontal([Constraint::Length(22), Constraint::Fill(1)]).areas(list);
        (
            heads,
            list,
            Some(rail.union(Rect {
                height: list.height + 1,
                ..rail
            })),
        )
    } else {
        (heads, list, None)
    };
    heading(f, heads, theme);
    rows(f, list, app, theme, mark);
    if let Some(area) = rail {
        side(f, area, app, theme, mark);
    }
    if strip_high > 0 {
        when(f, strip, app, theme);
    }
    footer(f, foot, app, theme, mark);
    if app.helping {
        help(f, area, theme);
    }
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
        let picked = app.picked.contains_key(&hit.path);
        let cells = vec![
            Cell::from(Line::from(vec![
                // The age stripe: one cell of colour, the same six bands the
                // window draws down the left of every row.
                Span::styled("▎", Style::new().fg(theme.band(band))),
                Span::styled(
                    if picked {
                        "✓"
                    } else if here {
                        "▸"
                    } else {
                        " "
                    },
                    Style::new().fg(theme.key()),
                ),
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

/// The rail: what the matching rows are made of, where they live, how big
/// they are — and the strip of time under the list.
fn side(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
    let mut lines: Vec<Line> = Vec::new();
    let head = |what: &str| {
        Line::from(Span::styled(
            format!(" {what}"),
            Style::new()
                .fg(theme.ink_3())
                .add_modifier(Modifier::BOLD | Modifier::DIM),
        ))
    };
    let most = app.kinds.iter().map(|(_, n)| *n).max().unwrap_or(1).max(1);
    // Where the rail's own cursor is, counted over the lines it offers rather
    // than the lines drawn: the headings and the blanks are not stops.
    let mut at = 0usize;
    let here = |at: usize, app: &App| app.in_rail && app.rail_at == at;
    lines.push(head("KIND"));
    for (token, count) in app.kinds.iter().take(9) {
        let term = scour_ui::query::of_kind(token);
        let on = app.filter.as_deref() == Some(term.as_str());
        // A bar as wide as the count is large, in the kind's own colour: the
        // same reading the window's rail offers, in eight characters.
        let width = ((*count as f64 / most as f64) * 6.0).round() as usize;
        let said = format::grouped(*count, mark.0);
        let cursor = here(at, app);
        at += 1;
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}{:<7}", if cursor { "▸" } else { " " }, cut(token, 7)),
                Style::new().fg(if on || cursor {
                    theme.key()
                } else {
                    theme.ink_2()
                }),
            ),
            Span::styled(
                format!("{:<6}", "▇".repeat(width.max(1))),
                Style::new().fg(theme.kind(token)),
            ),
            Span::styled(format!("{said:>6}"), Style::new().fg(theme.ink_3())),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(head("WHERE"));
    for (label, path) in app.places.iter().take(6) {
        let term = scour_ui::query::of_place(path);
        let on = app.filter.as_deref() == Some(term.as_str());
        let cursor = here(at, app);
        at += 1;
        lines.push(Line::from(Span::styled(
            format!("{}{}", if cursor { "▸" } else { " " }, cut(label, 19)),
            Style::new().fg(if on || cursor {
                theme.key()
            } else {
                theme.ink_2()
            }),
        )));
    }
    lines.push(Line::from(""));
    lines.push(head("SIZE"));
    for (label, term) in scour_ui::query::SIZES {
        let on = app.filter.as_deref() == Some(term);
        let cursor = here(at, app);
        at += 1;
        lines.push(Line::from(Span::styled(
            format!("{}{label}", if cursor { "▸" } else { " " }),
            Style::new().fg(if on || cursor {
                theme.key()
            } else {
                theme.ink_2()
            }),
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The twenty-four bars of the time strip, and the axis under them.
///
/// **Eight heights out of one block character**, because a terminal row is
/// one cell tall and the shape of the distribution is the whole point: a
/// bar that is either there or not says nothing about how much of the result
/// is a week old.
fn when(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    const BLOCKS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let most = app.strip.iter().map(|(_, n)| *n).max().unwrap_or(1).max(1);
    let bars: Vec<Span> = app
        .strip
        .iter()
        .map(|(days, count)| {
            // **Nothing is nothing, and anything is at least a tick.** A band
            // holding four files out of nine thousand rounds to nought, and a
            // blank there reads as "no files this week" rather than "few".
            let step = if *count == 0 {
                0
            } else {
                (((*count as f64 / most as f64) * 8.0).round() as usize).max(1)
            };
            Span::styled(
                BLOCKS[step.min(8)],
                Style::new().fg(theme.band(scour_ui::band_of(*days as f64))),
            )
        })
        .collect();
    let [bar_line, axis] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let mut line = vec![Span::raw(" ")];
    line.extend(bars);
    f.render_widget(Paragraph::new(Line::from(line)), bar_line);
    let dim = Style::new().fg(theme.ink_3());
    // The axis ends where the bars end, not where the terminal does.
    let span = app.strip.len().max(6);
    let left = "two years ago";
    let gap = span.saturating_sub(left.chars().count() + 5);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {left}{}today", " ".repeat(gap)),
            dim,
        ))),
        axis,
    );
}

/// A label, cut to fit rather than wrapped: a rail is one line per thing.
fn cut(text: &str, to: usize) -> String {
    if text.chars().count() <= to {
        return text.to_string();
    }
    text.chars().take(to.saturating_sub(1)).collect::<String>() + "…"
}

/// The one line at the bottom: what is picked or where the cursor is, the
/// order, and which mode.
fn footer(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
    let dim = Style::new().fg(theme.ink_3());
    let (picked, folders, bytes) = app.weighed();
    // **What is picked displaces where the cursor is**, because a selection is
    // something somebody is about to act on and a path is something they can
    // already see in the list.
    let path = if picked > 0 {
        let mut said = format!("{picked} picked");
        if folders > 0 {
            said.push_str(&format!(" · {folders} folders"));
        }
        if bytes > 0 {
            // The column's format rather than the meter's: a selection of two
            // small files is `13,3 KiB`, and `0,0 MB` says nothing at all.
            said.push_str(&format!(" · {}", format::size(bytes, mark.1)));
        }
        said
    } else {
        app.here()
            .map(|h| h.path.clone())
            .unwrap_or_else(|| "—".into())
    };
    let mode = if app.in_rail {
        "rail"
    } else {
        match app.mode {
            Mode::Search => "search",
            Mode::Move => "move",
        }
    };
    let [left, right] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(30)]).areas(area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {path}"),
            if picked > 0 {
                Style::new().fg(theme.key())
            } else {
                dim
            },
        ))),
        left,
    );
    let order = format!(
        "{} {}  {mode} ",
        app.sort_name(),
        if app.descending { "↓" } else { "↑" }
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            order,
            Style::new().fg(theme.ink_3()),
        )))
        .right_aligned(),
        right,
    );
}

/// The key list, over everything, printed from the one table there is.
fn help(f: &mut Frame, area: Rect, theme: &Theme) {
    let wide = 60u16.min(area.width.saturating_sub(4));
    let tall = (crate::keys::MAP.len() as u16 + 4).min(area.height);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(wide)) / 2,
        y: area.y + (area.height.saturating_sub(tall)) / 2,
        width: wide,
        height: tall,
    };
    f.render_widget(Clear, box_area);
    let mut lines = vec![Line::from(Span::styled(
        " KEYS",
        Style::new().fg(theme.ink()).add_modifier(Modifier::BOLD),
    ))];
    for (key, what) in crate::keys::MAP {
        lines.push(Line::from(vec![
            Span::styled(format!(" {key:<28}"), Style::new().fg(theme.key())),
            Span::styled(*what, Style::new().fg(theme.ink_2())),
        ]));
    }
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_style(Style::new().fg(theme.line()))
                .style(Style::new().bg(theme.back())),
        ),
        box_area,
    );
}
