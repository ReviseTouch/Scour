//! The list: its headings, its rows, and each cell's value.

use crate::app::{App, Spot};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
};
use scour_ui::format;

/// Which column covers this offset into the list's own width.
pub fn column_at(col: u16, width: u16, cols: &[&'static scour_ui::Column]) -> Option<usize> {
    let area = Rect::new(0, 0, width, 1);
    let columns = Layout::horizontal(widths_for(width, cols))
        .spacing(1)
        .split(area);
    columns
        .iter()
        .position(|c| col >= c.x && col < c.x + c.width)
}

/// The column names, and which one the list is sorted by. The arrow sits on the
/// column it is about; the footer repeats it for a terminal too narrow to draw.
pub(super) fn heading(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let quiet = Style::new().fg(theme.ink_3()).add_modifier(Modifier::DIM);
    let names: Vec<String> = app
        .columns
        .iter()
        .enumerate()
        .map(|(at, c)| {
            let word = app.say(heading_msgid(c.id)).into_owned();
            // The first column carries the mark and the stripe: two cells in.
            if at == 0 { format!("  {word}") } else { word }
        })
        .collect();
    let sorted = app.sorted_column();
    let cells: Vec<Cell> = names
        .iter()
        .enumerate()
        .map(|(at, name)| {
            let by = sorted == Some(at);
            let arrow = if by {
                if app.descending { " ↓" } else { " ↑" }
            } else {
                ""
            };
            let style = if app.pressed == Spot::Head(at) {
                Style::new().bg(theme.key()).fg(theme.back())
            } else if app.hover == Spot::Head(at) {
                Style::new()
                    .fg(theme.ink())
                    .bg(theme.hover())
                    .add_modifier(Modifier::UNDERLINED)
            } else if by {
                Style::new().fg(theme.key()).add_modifier(Modifier::BOLD)
            } else {
                quiet
            };
            Cell::from(Span::styled(format!("{name}{arrow}"), style))
        })
        .collect();
    f.render_widget(
        Table::new(vec![Row::new(cells)], widths_for(area.width, &app.columns)).column_spacing(1),
        area,
    );
}

/// The word this heading is written with: short and upper-case, because a
/// terminal heading is read in a glance — `WHERE` rather than `Location`.
fn heading_msgid(id: &str) -> &'static str {
    match id {
        "name" => "NAME",
        "path" => "WHERE",
        "mtime" => "CHANGED",
        "size" => "SIZE",
        "kind" => "KIND",
        "ext" => "EXTENSION",
        "ctime" => "CREATED",
        "atime" => "ACCESSED",
        "perm" => "MODE",
        "user" => "OWNER",
        "group" => "GROUP",
        "disk" => "ON DISK",
        _ => "",
    }
}

/// What each column gets. `Fill` on the ones that can take it, so a narrow
/// terminal eats the path before the name; the fixed widths are `scour-ui`'s
/// pixels divided by eight, a terminal cell being about that wide.
fn widths_for(width: u16, cols: &[&'static scour_ui::Column]) -> Vec<Constraint> {
    cols.iter()
        .map(|c| match c.id {
            "name" => Constraint::Fill(3),
            "path" => Constraint::Fill(4),
            // The time of day goes first when there is no room: the date orders
            // the list and the minute is read once in a hundred rows.
            "mtime" | "ctime" | "atime" => Constraint::Length(if width >= 110 { 16 } else { 10 }),
            _ => Constraint::Length((c.width as u16 / 8).max(6)),
        })
        .collect()
}

/// What one column says about one hit, as text. Not the name or the path: those
/// two are drawn from several spans of their own.
fn value_of(hit: &scour_core::Hit, id: &str, app: &App, width: u16, decimal: char) -> String {
    match id {
        "kind" => app.say(hit.kind.msgid()).into_owned(),
        "ext" => scour_core::ext_str(scour_ui::path::leaf(&hit.path)).to_owned(),
        "mtime" => when_of(hit.meta.mtime, width),
        "ctime" => when_of(hit.meta.ctime, width),
        // A dash where the volume records nothing: `noatime` freezes the access
        // time at creation, so the column would date it under the wrong heading.
        "atime" => {
            if app.frozen_atime(&hit.path) {
                "—".to_owned()
            } else {
                when_of(hit.meta.atime, width)
            }
        }
        // A folder's size is what the index holds under it, and `~` says so:
        // the scan rules leave things out.
        "size" => match (hit.is_dir, hit.under.as_ref()) {
            (true, Some(u)) => format!("~{}", format::size(u.disk, decimal)),
            (true, None) => String::new(),
            (false, _) => format::size(hit.meta.size.max(0) as u64, decimal),
        },
        "disk" => {
            if hit.meta.disk > 0 {
                format::size(hit.meta.disk as u64, decimal)
            } else {
                String::new()
            }
        }
        "perm" => scour_core::mode_string(hit.meta.mode),
        "user" => scour_core::owner_name(scour_core::Owner::User, hit.meta.uid),
        "group" => scour_core::owner_name(scour_core::Owner::Group, hit.meta.gid),
        _ => String::new(),
    }
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

pub(super) fn rows(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
    let total = app.pages.total();
    // Nothing found and something nearly: a clean rescue card where the rows would be.
    if let (0, Some(similar)) = (total, &app.similar) {
        let count = format!(
            "{}{}",
            format::grouped(similar.total, mark.0),
            if similar.capped { "+" } else { "" }
        );
        let said = app.say("Similar: {n} files —").replace("{n}", &count);
        let card_w = 72u16.min(area.width.saturating_sub(2));
        let at = Rect {
            x: area.x + 1.min(area.width.saturating_sub(card_w)),
            y: area.y + 1.min(area.height.saturating_sub(4)),
            width: card_w,
            height: 4.min(area.height),
        };
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    " ≈ ",
                    Style::new().fg(theme.key()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{said} "), Style::new().fg(theme.ink())),
                Span::styled(
                    similar.query.clone(),
                    Style::new().fg(theme.key()).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("   ", Style::new()),
                Span::styled(
                    format!(" {} ", app.say("Show similar (Enter)")),
                    Style::new()
                        .fg(theme.back())
                        .bg(theme.key())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  ·  {}", app.say("Near matches in the index")),
                    Style::new().fg(theme.ink_3()),
                ),
            ]),
        ];
        f.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_style(Style::new().fg(theme.line()))
                    .style(Style::new().bg(theme.panel())),
            ),
            at,
        );
        return;
    }
    // The scrollbar gets a column of its own: drawn over the list it sits in
    // the size column, where a stray character reads as part of the number.
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
    // What each column comes to, so a name is cut with a mark rather than by
    // the table, which cuts silently and mid-word.
    let columns = Layout::horizontal(widths_for(area.width, &app.columns))
        .spacing(1)
        .split(area);
    // What the kind's glyph takes, when there is one.
    let icon_wide = if crate::icons::drawing() { 2 } else { 0 };
    // How much room each column came out with, so a value can be cut to it.
    let room: Vec<usize> = columns.iter().map(|c| c.width as usize).collect();
    let mut drawn: Vec<Row> = Vec::with_capacity(app.room);
    for row in app.top..(app.top + app.room).min(total.max(app.top)) {
        let here = row == app.cursor;
        let Some(hit) = app.pages.at(row) else {
            // A row whose page has not arrived: blank, so the list keeps shape.
            drawn.push(Row::new(vec![Cell::from("")]));
            continue;
        };
        let band = format::band(now, hit.meta.mtime);
        let name = scour_ui::path::leaf(&hit.path);
        let ink = if here { theme.ink() } else { theme.ink_2() };
        let line = Style::new().fg(ink);
        let picked = app.picked.contains_key(&hit.path);
        let under = app.hover == Spot::Row(row) || app.hover == Spot::Tick(row);
        let pushed = app.pressed == Spot::Row(row) || app.pressed == Spot::Tick(row);
        // The mark answers the pointer on its own: it is what picks a row.
        let ticking = app.hover == Spot::Tick(row);
        // The mark and the stripe are two characters of whichever column comes
        // first, not a column of their own: a heading over them says nothing.
        let mark_spans = vec![
            // The age stripe: one cell of the same six bands the window draws.
            Span::styled("▎", Style::new().fg(theme.band(band))),
            Span::styled(
                if picked {
                    "✓"
                } else if ticking {
                    "·"
                } else if here {
                    "▸"
                } else {
                    " "
                },
                Style::new().fg(if ticking && !picked {
                    theme.ink()
                } else {
                    theme.key()
                }),
            ),
        ];
        let cells: Vec<Cell> = app
            .columns
            .iter()
            .enumerate()
            .map(|(at, c)| {
                let wide = room.get(at).copied().unwrap_or(0);
                let mut spans = if at == 0 {
                    mark_spans.clone()
                } else {
                    Vec::new()
                };
                let left = if at == 0 {
                    wide.saturating_sub(2)
                } else {
                    wide
                };
                match c.id {
                    "name" => {
                        spans.push(Span::styled(
                            crate::icons::of_kind(hit.kind.token()).to_string(),
                            Style::new().fg(theme.kind(hit.kind.token())),
                        ));
                        let avail = left.saturating_sub(icon_wide);
                        let shown = cut(name, avail);
                        let letters = scour_ui::query::letters_terms(&app.query);
                        if let Some(sub) = letters
                            .iter()
                            .find_map(|w| scour_ui::query::subseq_spans(&shown, w))
                            .filter(|v| !v.is_empty())
                        {
                            let mut cur = 0usize;
                            for (s, e) in sub {
                                if s > cur {
                                    spans.push(Span::styled(shown[cur..s].to_string(), line));
                                }
                                spans.push(Span::styled(
                                    shown[s..e].to_string(),
                                    Style::new().fg(theme.key()).add_modifier(Modifier::BOLD),
                                ));
                                cur = e;
                            }
                            if cur < shown.len() {
                                spans.push(Span::styled(shown[cur..].to_string(), line));
                            }
                        } else {
                            // Every plain word of the query, every place it is,
                            // as the window and the page mark them.
                            let words: Vec<&str> = scour_ui::query::terms(&app.query)
                                .into_iter()
                                .filter(|t| !t.starts_with('!') && !t.contains(':'))
                                .flat_map(|t| t.split([';', '|']))
                                .map(|t| t.trim_matches(['"', '*']))
                                .filter(|t| !t.is_empty())
                                .collect();
                            let marks = scour_core::Unaccented::marks(&shown, &words);
                            let mut cur = 0usize;
                            for (s, e) in marks {
                                if s > cur {
                                    spans.push(Span::styled(shown[cur..s].to_string(), line));
                                }
                                spans.push(Span::styled(
                                    shown[s..e].to_string(),
                                    Style::new().fg(theme.key()).add_modifier(Modifier::BOLD),
                                ));
                                cur = e;
                            }
                            if cur < shown.len() {
                                spans.push(Span::styled(shown[cur..].to_string(), line));
                            }
                        }
                        if app.v2.grep_open
                            && let Some(fl) = hit.found.first()
                        {
                            let used = shown.chars().count();
                            let rem = avail.saturating_sub(used + 3);
                            if rem > 8 {
                                let lno = fl.line.map_or_else(String::new, |l| format!("L{l}: "));
                                let clean = fl.text.replace(['\t', '\r', '\n'], " ");
                                spans.push(Span::styled(
                                    format!("  {}", cut(&format!("{lno}{}", clean.trim()), rem)),
                                    Style::new().fg(theme.ink_3()),
                                ));
                            }
                        }
                    }
                    // A path is cut from the front: the end names the folder.
                    "path" => spans.push(Span::styled(
                        tail(scour_ui::path::folder(&hit.path), left),
                        Style::new().fg(theme.ink_3()),
                    )),
                    _ => spans.push(Span::styled(
                        cut(&value_of(hit, c.id, app, area.width, mark.1), left),
                        line,
                    )),
                }
                Cell::from(Line::from(spans))
            })
            .collect();
        // Pressed is brighter than hovered is brighter than nothing.
        let style = if pushed {
            Style::new().bg(theme.key()).fg(theme.back())
        } else if here {
            Style::new().bg(theme.pick())
        } else if under {
            Style::new().bg(theme.hover())
        } else {
            Style::new()
        };
        drawn.push(Row::new(cells).style(style));
    }
    f.render_widget(
        Table::new(drawn, widths_for(area.width, &app.columns)).column_spacing(1),
        area,
    );

    // The list is a window onto a result, not a scrolled buffer, so the bar is
    // told where it is: the top row's place in the whole result.
    if let Some(bar) = bar {
        let mut state = ScrollbarState::new(total.saturating_sub(app.room)).position(app.top);
        // Brighter under the pointer, brightest while it is dragged.
        let held = matches!(app.pressed, Spot::Bar(_));
        let over = matches!(app.hover, Spot::Bar(_));
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .style(Style::new().fg(theme.line()))
                .thumb_style(Style::new().fg(if held {
                    theme.key()
                } else if over {
                    theme.ink()
                } else {
                    theme.ink_3()
                })),
            bar,
            &mut state,
        );
    }
}

/// One place a `content:` query found its text: the line's number, then the
/// line with the find drawn as the window marks it, cut to `wide` characters
/// with the find kept in sight.
pub(super) fn found_line(
    found: &scour_core::FoundLine,
    wide: usize,
    theme: &Theme,
) -> Line<'static> {
    let tidy = |s: &str| s.replace(['\t', '\r', '\n'], " ");
    // Every find the service marked, or `at` alone from an older one.
    let one = [found.at];
    let marks: Vec<(usize, usize)> = (if found.marks.is_empty() {
        &one[..]
    } else {
        &found.marks[..]
    })
    .iter()
    .map(|&(a, l)| (a as usize, (a + l) as usize))
    .filter(|&(a, b)| a < b && found.text.get(a..b).is_some())
    .collect();
    let number = found
        .line
        .map(|n| format!("{n:>5}  "))
        .unwrap_or_else(|| "       ".into());
    let room = wide.saturating_sub(number.chars().count());
    let Some(&(first_a, first_b)) = marks.first() else {
        return Line::from(vec![
            Span::styled(number, Style::new().fg(theme.ink_3())),
            Span::styled(
                cut(&tidy(&found.text), room),
                Style::new().fg(theme.ink_2()),
            ),
        ]);
    };
    let pre = tidy(found.text[..first_a].trim_start());
    let hit = tidy(&found.text[first_a..first_b]);
    // The part before the first find gives way first, from its front.
    let before = pre.chars().count();
    let keep = room.saturating_sub(hit.chars().count()).min(before).min(30);
    let pre: String = if before > keep {
        let cut: String = pre.chars().skip(before - keep).collect();
        format!("…{}", cut.chars().skip(1).collect::<String>())
    } else {
        pre
    };
    let plain = Style::new().fg(theme.ink_2());
    let marked = Style::new().bg(theme.key()).fg(theme.back());
    let mut left = room.saturating_sub(pre.chars().count() + hit.chars().count());
    let mut spans = vec![
        Span::styled(number, Style::new().fg(theme.ink_3())),
        Span::styled(pre, plain),
        Span::styled(hit, marked),
    ];
    // After it, the rest of the line with every later find marked, cut where
    // the room ends.
    let mut pos = first_b;
    let mut rest: Vec<(String, bool)> = Vec::new();
    for &(a, b) in &marks[1..] {
        if a < pos {
            continue;
        }
        rest.push((tidy(&found.text[pos..a]), false));
        rest.push((tidy(&found.text[a..b]), true));
        pos = b;
    }
    rest.push((tidy(&found.text[pos..]), false));
    for (text, is_find) in rest {
        if left == 0 {
            break;
        }
        let n = text.chars().count();
        let shown = if n > left { cut(&text, left) } else { text };
        left = left.saturating_sub(n);
        spans.push(Span::styled(shown, if is_find { marked } else { plain }));
    }
    Line::from(spans)
}

/// What sits in the rail's first column: a dot for the filter in force, an
/// arrow for where the keyboard cursor is, a space for everything else.
pub(super) fn mark_of(on: bool, cursor: bool) -> &'static str {
    if on {
        "●"
    } else if cursor {
        "▸"
    } else {
        " "
    }
}

/// A label, cut to fit rather than wrapped: a rail is one line per thing. The
/// ellipsis is what says four identical-looking rows are not identical.
pub(crate) fn cut(text: &str, to: usize) -> String {
    if text.chars().count() <= to {
        return text.to_string();
    }
    text.chars().take(to.saturating_sub(1)).collect::<String>() + "…"
}

/// A path, cut from the front: paths share their beginnings, and the end is the
/// part that says which folder this is.
pub(super) fn tail(text: &str, to: usize) -> String {
    let len = text.chars().count();
    if len <= to {
        return text.to_string();
    }
    let from = len - to.saturating_sub(1);
    "…".to_string() + &text.chars().skip(from).collect::<String>()
}
