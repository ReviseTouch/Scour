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

use crate::app::{App, Mode, Panel};
use crate::theme::Theme;

/// Lines the list does not get: the query, the meter, the rule under them,
/// the column heading, and the footer.
const CHROME: u16 = 5;

/// Where the list starts, and where the rail starts — one line higher,
/// because the rail takes the heading line as its own.
///
/// **Exported, because the mouse counts in them too.** A press is a row and a
/// column and nothing else; the arithmetic that turns it into a row of the
/// list has to be the arithmetic that drew it.
pub const LIST_TOP: u16 = 4;
pub const RAIL_TOP: u16 = 3;
/// How wide the rail is when it is drawn at all.
pub const RAIL_WIDE: u16 = 22;

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
        3
    } else {
        0
    };
    let [top, meter, rule, heads, list, strip, foot] = Layout::vertical([
        Constraint::Length(1),
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
    // **A line under the two of them.** Without it the query, the counts and
    // the column headings were three rows of text with nothing saying which
    // was which — and the query line has to look like something you type in.
    across(f, rule, theme);
    // **The rail goes when the terminal is narrow.** Twenty-two columns out of
    // eighty is a quarter of the list, and the list is what somebody came for.
    // **A hundred columns before the rail is worth its room.** At eighty it
    // took twenty-two of them and left the name ten characters wide, which is
    // not a name — it is a hint that there was one.
    let wide = area.width >= 100 && app.rail;
    let (heads, list, rail) = if wide {
        let [rail, heads] =
            Layout::horizontal([Constraint::Length(RAIL_WIDE), Constraint::Fill(1)]).areas(heads);
        let [_, list] =
            Layout::horizontal([Constraint::Length(RAIL_WIDE), Constraint::Fill(1)]).areas(list);
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
        // A line between the rail and the list, because the age stripe down
        // the left of every row butted straight against the rail's text and
        // the two read as one column of noise.
        let [rail, rule] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(2)]).areas(area);
        side(f, rail, app, theme, mark);
        f.render_widget(
            Paragraph::new(
                (0..rule.height)
                    .map(|_| Line::from(Span::styled("│ ", Style::new().fg(theme.line()))))
                    .collect::<Vec<_>>(),
            ),
            rule,
        );
    }
    if strip_high > 0 {
        when(f, strip, app, theme);
    }

    footer(f, foot, app, theme, mark);
    if app.panel != Panel::None {
        panel(f, area, app, theme);
    }
    if app.helping {
        help(f, area, theme);
    }
}

/// Where a panel of this many lines is drawn.
///
/// **One function, two callers**: this and the mouse. A panel drawn in one
/// place and hit-tested in another is the fault the window spent two days on,
/// and the only defence a terminal has is that the arithmetic is written once.
pub fn panel_rect(area: Rect, lines: usize) -> Rect {
    let wide = 66u16.min(area.width.saturating_sub(4));
    let tall = (lines as u16 + 3).min(area.height.saturating_sub(2));
    Rect {
        x: area.x + area.width.saturating_sub(wide) / 2,
        y: area.y + 2,
        width: wide,
        height: tall,
    }
}

/// Whatever panel is open, over the middle of the screen.
///
/// One drawing for the three of them, because they are the same shape: a
/// title, a list, and a cursor on one line of it. What differs is the lines.
fn panel(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let (title, lines): (&str, Vec<(String, bool)>) = match app.panel {
        Panel::Rules => (
            "WHAT IS SKIPPED",
            app.rules
                .iter()
                .map(|(_, value, off, added)| {
                    (
                        format!(
                            "{} {}{}",
                            if *off { "☐" } else { "☑" },
                            value,
                            if *added { "  ·  added here" } else { "" }
                        ),
                        false,
                    )
                })
                .collect(),
        ),
        Panel::Language => (
            "LANGUAGE",
            vec![("Türkçe".into(), false), ("English".into(), false)],
        ),
        Panel::Faces => (
            "HOW TO RUN IT",
            vec![
                ("Window".into(), false),
                ("Terminal  ·  running now".into(), true),
                ("Browser  ·  opens a port on 127.0.0.1".into(), false),
            ],
        ),
        Panel::None => return,
    };

    let box_area = panel_rect(area, lines.len());
    f.render_widget(Clear, box_area);
    // Only what fits, scrolled to keep the cursor on it: the skip list is
    // forty rules long and the panel is not.
    let room = box_area.height.saturating_sub(3) as usize;
    let from = app.panel_at.saturating_sub(room.saturating_sub(1));
    let mut drawn = vec![Line::from(Span::styled(
        format!(" {title}"),
        Style::new().fg(theme.ink()).add_modifier(Modifier::BOLD),
    ))];
    for (at, (text, dimmed)) in lines.iter().enumerate().skip(from).take(room) {
        let on = at == app.panel_at;
        drawn.push(Line::from(Span::styled(
            format!("{}{text}", if on { " ▸ " } else { "   " }),
            Style::new().fg(if on {
                theme.key()
            } else if *dimmed {
                theme.ink_3()
            } else {
                theme.ink_2()
            }),
        )));
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

/// The query line: the brand, what has been typed, and the caret.
fn query(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    // The whole line is the field: a strip of panel across the window, the way
    // the browser page draws one. Before this it was text on the background
    // like every other line, and the one thing nobody could tell was where to
    // type.
    f.render_widget(Block::new().style(Style::new().bg(theme.panel())), area);
    let typed = if app.query.is_empty() {
        // **Quiet, and short.** The examples were drawn as brightly as a
        // typed query and filled the line: it read as something already
        // searched for rather than as an empty box.
        Span::styled(
            "search…",
            Style::new()
                .fg(theme.ink_3())
                .add_modifier(Modifier::ITALIC | Modifier::DIM),
        )
    } else {
        Span::styled(
            app.query.as_str(),
            Style::new().fg(theme.ink()).add_modifier(Modifier::BOLD),
        )
    };
    let mut parts = vec![
        Span::styled(
            " SCOUR ",
            Style::new()
                .fg(theme.key())
                .add_modifier(Modifier::BOLD | Modifier::DIM),
        ),
        typed,
    ];
    // **What is pressed is shown beside what was typed.** A filter with a rail
    // row of its own is visible there, but a band of the time strip has none —
    // so a result narrowed by pressing the strip looked, until this, exactly
    // like a result that was simply short.
    if let Some(term) = &app.filter {
        parts.push(Span::styled("  ·  ", Style::new().fg(theme.ink_3())));
        parts.push(Span::styled(
            term.clone(),
            Style::new().fg(theme.key()).add_modifier(Modifier::BOLD),
        ));
    }
    let line = Line::from(parts);
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
    if !app.note.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {}", app.note),
                Style::new().fg(theme.key()),
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
                "{} of {}{}",
                format::grouped(shown as u64, mark.0),
                if app.capped { "at least " } else { "" },
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
    // **What it cost the index is not what a reader came for.** It is the
    // number this was tuned against and it belongs where the tuning happens.
    if app.rows_visited > 0 && std::env::var("SCOUR_TRACE").is_ok() {
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
    let names = ["  NAME", "WHERE", "CHANGED", "SIZE"];
    let cells: Vec<Cell> = names
        .iter()
        .map(|n| Cell::from(Span::styled(*n, style)))
        .collect();
    f.render_widget(
        Table::new(vec![Row::new(cells)], widths_for(area.width)).column_spacing(1),
        area,
    );
}

/// What each column gets. `Fill` on the two that can take it, so a narrow
/// terminal eats the path before it eats the name.
fn widths_for(width: u16) -> [Constraint; 4] {
    [
        // **No kind column.** It was ten columns of the same English word
        // repeated down the screen — `doc doc doc doc` — while the name beside
        // it was already drawn in that kind's colour and the rail already said
        // how many of each there were. The room went to the two columns that
        // were being cut mid-word.
        Constraint::Fill(3),
        Constraint::Fill(4),
        // The time of day goes first when there is no room for it: the date
        // orders the list and the minute is read once in a hundred rows.
        Constraint::Length(if width >= 110 { 16 } else { 10 }),
        Constraint::Length(9),
    ]
}

/// The stamp, cut to what the column can hold.
fn when_of(secs: i64, width: u16) -> String {
    let said = format::stamp(secs);
    if width >= 110 {
        said
    } else {
        said.chars().take(10).collect()
    }
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
    // What each column actually comes to, so that a name can be cut with a
    // mark rather than by the table, which cuts silently and mid-word.
    let columns = Layout::horizontal(widths_for(area.width))
        .spacing(1)
        .split(area);
    let (name_w, where_w) = (
        columns[0].width.saturating_sub(2) as usize,
        columns[1].width as usize,
    );
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
                Span::styled(cut(name, name_w), line),
            ])),
            Cell::from(Span::styled(
                tail(scour_ui::path::folder(&hit.path), where_w),
                Style::new().fg(theme.ink_3()),
            )),
            Cell::from(Span::styled(when_of(hit.meta.mtime, area.width), line)),
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
    f.render_widget(
        Table::new(drawn, widths_for(area.width)).column_spacing(1),
        area,
    );

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
    let (kinds_shown, places_shown) = app.rail_room();
    let most = app.kinds.iter().map(|(_, n)| *n).max().unwrap_or(1).max(1);
    // Where the rail's own cursor is, counted over the lines it offers rather
    // than the lines drawn: the headings and the blanks are not stops.
    let mut at = 0usize;
    let here = |at: usize, app: &App| app.in_rail && app.rail_at == at;
    lines.push(head("KIND"));
    for (token, count) in app.kinds.iter().take(kinds_shown) {
        let term = scour_ui::query::of_kind(token);
        let on = app.filter.as_deref() == Some(term.as_str());
        // **The count is never cut, and the arithmetic says so out loud**: one
        // column for the cursor, four for the bar, whatever the number needs,
        // and the name takes what is left. `507.69` is not a number — it was
        // `507.691` with its last digit run off the end of a line that had
        // been counted at a width the rail does not have.
        let said = format::grouped(*count, mark.0);
        let bar = 4usize;
        let name = (area.width as usize)
            .saturating_sub(2 + bar + said.chars().count())
            .max(3);
        let width = ((*count as f64 / most as f64) * bar as f64).round() as usize;
        let cursor = here(at, app);
        at += 1;
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{}{:<name$}",
                    if cursor { "▸" } else { " " },
                    cut(token, name)
                ),
                Style::new().fg(if on || cursor {
                    theme.key()
                } else {
                    theme.ink_2()
                }),
            ),
            Span::styled(
                format!("{:<bar$}", "▇".repeat(width.clamp(1, bar))),
                Style::new().fg(theme.kind(token)),
            ),
            Span::styled(format!(" {said}"), Style::new().fg(theme.ink_3())),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(head("WHERE"));
    for (label, path) in app.places.iter().take(places_shown) {
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

/// A rule across the window, in the quietest line colour there is.
fn across(f: &mut Frame, area: Rect, theme: &Theme) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::new().fg(theme.line()),
        ))),
        area,
    );
}

/// The twenty-four bars of the time strip, and the axis under them.
///
/// **Eight heights out of one block character**, because a terminal row is
/// one cell tall and the shape of the distribution is the whole point: a
/// bar that is either there or not says nothing about how much of the result
/// is a week old.
fn when(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    // Eight heights in a cell, two cells of height: sixteen steps rather than
    // eight. A distribution drawn in eight is a staircase — which is what the
    // first one looked like, and it was the first thing anybody said about it.
    const BLOCKS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let [upper, lower, axis] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let bands = app.strip.len().max(1);
    let room = upper.width.saturating_sub(2) as usize;
    // **The strip is as wide as the window.** One cell per band left it
    // twenty-four columns wide in the middle of a hundred and twenty, which
    // reads as a decoration rather than a reading of the result. Every band
    // gets the same share, and what is left over is spread from the left so
    // the whole width is used and no band is wider than its neighbour by more
    // than one.
    let each = (room / bands).max(1);
    let spare = room.saturating_sub(each * bands);
    let most = app.strip.iter().map(|(_, n)| *n).max().unwrap_or(1).max(1);

    let mut top: Vec<Span> = vec![Span::raw(" ")];
    let mut bottom: Vec<Span> = vec![Span::raw(" ")];
    for (i, (days, count)) in app.strip.iter().enumerate() {
        let wide = each + usize::from(i < spare);
        // A column of space between bars where there is room for one. Without
        // it the twenty-four bands run together into a mountain range, and a
        // reader cannot tell which of two neighbouring heights is one band.
        let (wide, gap) = if wide >= 3 { (wide - 1, 1) } else { (wide, 0) };
        let step = if *count == 0 {
            0
        } else {
            // Nothing is nothing, and anything is at least a tick: a band of
            // four files out of nine thousand rounds to nought otherwise, and
            // a blank reads as "none that week".
            (((*count as f64 / most as f64) * 16.0).round() as usize).max(1)
        };
        let colour = Style::new().fg(theme.band(scour_ui::band_of(*days as f64)));
        top.push(Span::styled(
            BLOCKS[step.saturating_sub(8).min(8)].repeat(wide),
            colour,
        ));
        bottom.push(Span::styled(BLOCKS[step.min(8)].repeat(wide), colour));
        if gap > 0 {
            top.push(Span::raw(" "));
            bottom.push(Span::raw(" "));
        }
    }
    f.render_widget(Paragraph::new(Line::from(top)), upper);
    f.render_widget(Paragraph::new(Line::from(bottom)), lower);

    // The axis under the ends of the strip, not the ends of the terminal.
    let dim = Style::new().fg(theme.ink_3());
    let left = "two years ago";
    let right = "today";
    let gap = room.saturating_sub(left.chars().count() + right.chars().count());
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {left}{}{right}", " ".repeat(gap)),
            dim,
        ))),
        axis,
    );
}

/// A label, cut to fit rather than wrapped: a rail is one line per thing.
///
/// **With a mark saying it was cut.** Four rows reading `COLPAN_Teknik` are
/// four rows nobody can tell apart; `COLPAN_Teknik…` at least says the name
/// goes on.
fn cut(text: &str, to: usize) -> String {
    if text.chars().count() <= to {
        return text.to_string();
    }
    text.chars().take(to.saturating_sub(1)).collect::<String>() + "…"
}

/// A path, cut from the **front**.
///
/// Every path on this machine starts `/home/hasan/`, and cutting the end
/// throws away the part that says which folder this is — which is the only
/// part being read.
fn tail(text: &str, to: usize) -> String {
    let len = text.chars().count();
    if len <= to {
        return text.to_string();
    }
    let from = len - to.saturating_sub(1);
    "…".to_string() + &text.chars().skip(from).collect::<String>()
}

/// The one line at the bottom: what is picked or where the cursor is, the
/// order, and which mode.
fn footer(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
    // The same strip of panel the query line is drawn on, which closes the
    // window at the bottom without spending a row on a rule.
    f.render_widget(Block::new().style(Style::new().bg(theme.panel())), area);
    let dim = Style::new().fg(theme.ink_3());
    let (picked, folders, bytes) = app.weighed();
    // **What is picked displaces where the cursor is**, because a selection is
    // something somebody is about to act on and a path is something they can
    // already see in the list.
    let room = area.width.saturating_sub(32) as usize;
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
            .map(|h| tail(&h.path, room))
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
