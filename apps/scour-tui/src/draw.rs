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

use crate::app::{App, Mode, Panel, Spot};
use crate::theme::Theme;

/// How tall the query field is.
///
/// **Three rows rather than one.** A line of text among other lines of text is
/// not something anybody can see is a box to type in; three rows of panel with
/// the query in the middle of them is.
pub const QUERY_HIGH: u16 = 3;

/// Lines the list does not get: the query field, the meter, the rule under
/// them, the column heading, and the footer.
const CHROME: u16 = QUERY_HIGH + 4;

/// Where the list starts, and where the rail starts — one line higher,
/// because the rail takes the heading line as its own.
///
/// **Exported, because the mouse counts in them too.** A press is a row and a
/// column and nothing else; the arithmetic that turns it into a row of the
/// list has to be the arithmetic that drew it.
/// Which row of the field the text is on.
pub const QUERY_ROW: u16 = QUERY_HIGH / 2;
pub const LIST_TOP: u16 = QUERY_HIGH + 3;
pub const RAIL_TOP: u16 = QUERY_HIGH + 2;
/// How wide the rail is when it is drawn at all.
pub const RAIL_WIDE: u16 = 24;

/// How much room the list has, given a terminal this tall.
pub fn room(height: u16) -> usize {
    height.saturating_sub(CHROME).max(1) as usize
}

pub fn frame(f: &mut Frame, app: &App, theme: &Theme) {
    let area = f.area();
    // How this language punctuates numbers, asked once a frame and handed
    // down. It comes off the catalogue rather than off the desktop, so
    // switching the language switches the digits with the words.
    let mark = app.mark();
    f.render_widget(Block::new().style(Style::new().bg(theme.back())), area);
    // The strip of time is worth two lines and only where there are lines to
    // spare: under twenty rows it would take a fifth of the list.
    let strip_high = if area.height >= 20 && !app.strip.is_empty() {
        3
    } else {
        0
    };
    let [top, meter, rule, heads, list, strip, foot] = Layout::vertical([
        Constraint::Length(QUERY_HIGH),
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
    // The peek takes the bottom of the list, under it rather than over it:
    // what is being looked at has to stay on screen, or a person cannot tell
    // which row the head belongs to.
    //
    // **Measured against what is in it, not as a fraction.** The panel holds a
    // rule, a content type, eight facts and then the head of the file: a third
    // of a short terminal is six lines, which cut off the half that says who
    // owns it and when it was last read. Twelve is the ten it needs plus two
    // of the file; past thirty lines of list, two fifths gives the head more
    // room, and eighteen is where it stops taking it from the list.
    let (list, peek) = if app.peeking && list.height >= 20 {
        let want = (list.height * 2 / 5).clamp(12, 18);
        let [list, peek] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(want)]).areas(list);
        (list, Some(peek))
    } else {
        (list, None)
    };
    if app.reporting {
        // The report takes the heading line as well: it has its own headings.
        report(f, heads.union(list), app, theme, mark);
    } else {
        heading(f, heads, app, theme);
        rows(f, list, app, theme, mark);
    }
    if let Some(area) = peek {
        head_of(f, area, app, theme, mark);
    }
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
        when(f, strip, app, theme, mark);
    }

    footer(f, foot, app, theme, mark);
    if app.panel != Panel::None {
        panel(f, area, app, theme);
    }
    if app.helping {
        help(f, area, app, theme);
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
    let (title, lines): (std::borrow::Cow<str>, Vec<(String, bool)>) = match app.panel {
        Panel::Rules => (
            app.say("WHAT IS SKIPPED"),
            app.rules
                .iter()
                .map(|(_, value, off, added)| {
                    (
                        format!(
                            "{} {}{}",
                            if *off { "☐" } else { "☑" },
                            value,
                            if *added {
                                format!("  ·  {}", app.say("added here"))
                            } else {
                                String::new()
                            }
                        ),
                        false,
                    )
                })
                .collect(),
        ),
        // **The two languages are spelled in themselves**, not translated:
        // somebody looking for their own language recognises "Türkçe" and may
        // not recognise what the language they are currently reading calls it.
        // Same rule as `scour_i18n::LANGUAGES`, which is where these come from.
        Panel::Language => (
            app.say("LANGUAGE"),
            scour_i18n::LANGUAGES
                .iter()
                .map(|(_, endonym)| ((*endonym).to_string(), false))
                .collect(),
        ),
        Panel::Faces => (
            app.say("HOW TO RUN IT"),
            vec![
                (app.say("Window").into_owned(), false),
                (
                    format!("{}  ·  {}", app.say("Terminal"), app.say("running now")),
                    true,
                ),
                (
                    format!(
                        "{}  ·  {}",
                        app.say("Browser"),
                        app.say("opens a port on 127.0.0.1")
                    ),
                    false,
                ),
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
        let lit = if app.pressed == Spot::Panel(at) {
            Style::new().bg(theme.key()).fg(theme.back())
        } else if app.hover == Spot::Panel(at) {
            Style::new().bg(theme.hover())
        } else {
            Style::new()
        };
        drawn.push(
            Line::from(Span::styled(
                format!("{}{text}", if on { " ▸ " } else { "   " }),
                Style::new().fg(if on {
                    theme.key()
                } else if *dimmed {
                    theme.ink_3()
                } else {
                    theme.ink_2()
                }),
            ))
            .style(lit),
        );
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
    // The text sits on the middle row of the field, with a row of quiet above
    // and below it — which is what makes three rows read as one box rather
    // than as three lines that happen to share a colour.
    let area = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    let typed = if app.query.is_empty() {
        // **Quiet, and short.** The examples were drawn as brightly as a
        // typed query and filled the line: it read as something already
        // searched for rather than as an empty box.
        Span::styled(
            hint(app),
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
    // **The query in colour, when the service has read it back.** The same
    // six colours the window and the page use, from the same roles — a field
    // is a field in all three, and `sizE:>1mb` is drawn as the plain text it
    // will be searched for.
    let coloured: Vec<Span> = app
        .spans
        .iter()
        .filter_map(|sp| {
            let from = sp.start as usize;
            let to = from + sp.len as usize;
            let text = app.query.get(from..to)?;
            let colour = match sp.role {
                // **A mistake outranks a polarity.** `!kind:zurna` is
                // excluded, but what matters about it is that the engine
                // cannot read `zurna` and will search for the text instead.
                scour_core::Role::UnknownField | scour_core::Role::BadValue => theme.bad(),
                // **Excluded is excluded, all of it.** The `!` used to be the
                // only red character and the term behind it wore the colour
                // of the thing being looked for.
                _ if sp.not => theme.not(),
                scour_core::Role::Field => theme.key(),
                scour_core::Role::Value => theme.val(),
                scour_core::Role::Glob => theme.glob(),
                scour_core::Role::Not => theme.not(),
                // Blue for what is wanted; the punctuation between terms stays
                // quiet.
                scour_core::Role::Text | scour_core::Role::Phrase => theme.term(),
                // The syntax between terms, said quietly — including a `;`
                // typed instead of a space.
                scour_core::Role::Space
                | scour_core::Role::Sep
                | scour_core::Role::Colon
                | scour_core::Role::Quote => theme.ink_3(),
                _ => theme.ink(),
            };
            Some(Span::styled(
                text.to_string(),
                Style::new().fg(colour).add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    let mut parts = vec![Span::styled(
        " SCOUR ",
        Style::new()
            .fg(theme.key())
            .add_modifier(Modifier::BOLD | Modifier::DIM),
    )];
    // The coloured runs cover the whole query when they are here; the plain
    // text stands in until they arrive, so nothing blinks between the two.
    if coloured.is_empty() {
        parts.push(typed);
    } else {
        parts.extend(coloured);
    }
    // **What is pressed is shown beside what was typed.** A filter with a rail
    // row of its own is visible there, but a band of the time strip has none —
    // so a result narrowed by pressing the strip looked, until this, exactly
    // like a result that was simply short.
    if let Some(term) = &app.filter {
        parts.push(Span::styled("  ·  ", Style::new().fg(theme.ink_3())));
        // **Red under the pointer, and gone when it is pressed.** A filter
        // somebody cannot see how to remove is worse than no filter; the rail
        // row that set it clears it too, but a `dm:` band has no row and this
        // is the only place the term is written.
        let style = if app.pressed == Spot::Chip {
            Style::new()
                .fg(theme.back())
                .bg(theme.bad())
                .add_modifier(Modifier::BOLD)
        } else if app.hover == Spot::Chip {
            Style::new()
                .fg(theme.bad())
                .add_modifier(Modifier::BOLD | Modifier::CROSSED_OUT)
        } else {
            Style::new().fg(theme.key()).add_modifier(Modifier::BOLD)
        };
        parts.push(Span::styled(term.clone(), style));
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
            // **One sentence, not three words glued together.** Turkish puts
            // the total first — `1.000 içinden 23` — so a msgid per word
            // would have come out in English order whatever the translator
            // wrote. Two whole sentences, and the capped one is its own
            // because "at least" does not sit in the same place either.
            app.say(if app.capped {
                "{shown} of at least {total}"
            } else {
                "{shown} of {total}"
            })
            .replace("{shown}", &format::grouped(shown as u64, mark.0))
            .replace("{total}", &format::grouped(total as u64, mark.0)),
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
    // **What the index is doing, beside what the search found.** A walk takes
    // a minute and nothing else on this line moves while it runs; switching a
    // skip rule off starts one, and without this the answer to "did that do
    // anything" is a list that has not changed yet.
    if let Some(walked) = app.scanning {
        parts.push(Span::styled("  ·  ", dim));
        parts.push(Span::styled(
            app.say(scour_ui::SCANNING)
                .replace("{n}", &format::grouped(walked, mark.0)),
            Style::new().fg(theme.key()),
        ));
    }
    // **And whether searching is still as fast as it was built to be.** Every
    // query reads the unsorted tail; a week of ordinary use took ordering by
    // path from 1.9 ms to 21.5, and one rebuild put it back. It says what to do
    // rather than only that something is wrong — a number nobody can act on is
    // a number nobody reads.
    if app.rebuild_advised {
        parts.push(Span::styled("  ·  ", dim));
        parts.push(Span::styled(
            app.say(scour_ui::REBUILD_ADVISED),
            Style::new().fg(theme.not()),
        ));
    }
    if app.rows_visited > 0 && std::env::var("SCOUR_TRACE").is_ok() {
        parts.push(Span::styled("  ·  ", dim));
        parts.push(Span::styled(
            format!(
                "{} {}",
                format::grouped(app.rows_visited, mark.0),
                app.say("rows read")
            ),
            dim,
        ));
    }
    f.render_widget(Paragraph::new(Line::from(parts)), area);

    // The tools, at the other end of the same line. A narrow terminal does
    // without them — the counts are what the line is for, and the keys still
    // work.
    if area.width >= 90 {
        let mut said: Vec<Span> = Vec::new();
        for (at, (_, _, label)) in tool_spans(app, area.width).into_iter().enumerate() {
            let style = if app.pressed == Spot::Tool(at) {
                Style::new().bg(theme.key()).fg(theme.back())
            } else if app.hover == Spot::Tool(at) {
                Style::new().bg(theme.hover()).fg(theme.ink())
            } else {
                Style::new().fg(theme.ink_3())
            };
            said.push(Span::styled(label, style));
        }
        f.render_widget(Paragraph::new(Line::from(said)).right_aligned(), area);
    }
}

/// Where the filter is drawn on the query line, if one is pressed.
///
/// **The same arithmetic that draws it**, so that the thing which turns red
/// under the pointer is the thing a press removes.
pub fn chip_at(app: &App) -> Option<(u16, u16)> {
    let term = app.filter.as_deref()?;
    let typed = if app.query.is_empty() {
        hint(app).chars().count()
    } else {
        app.query.chars().count()
    };
    // " SCOUR " and the "  ·  " between the query and the filter.
    let from = (7 + typed + 5) as u16;
    Some((from, from + term.chars().count() as u16))
}

/// What an empty query says instead of nothing.
///
/// **One function, two callers again**: the width of this decides where the
/// filter chip is drawn and where a press on it lands, and a translated hint
/// is a different width in every language.
fn hint(app: &App) -> std::borrow::Cow<'_, str> {
    app.say("search…")
}

/// Which column covers this offset into the list's own width.
pub fn column_at(col: u16, width: u16) -> Option<usize> {
    let area = Rect::new(0, 0, width, 1);
    let columns = Layout::horizontal(widths_for(width)).spacing(1).split(area);
    columns
        .iter()
        .position(|c| col >= c.x && col < c.x + c.width)
}

/// The column names, and which one the list is sorted by.
///
/// **The arrow is the answer to "sorted how?"** and it is on the column it is
/// about, which is where everybody looks for it — the footer says it too, for
/// the terminal that is too narrow to draw the column at all.
fn heading(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let quiet = Style::new().fg(theme.ink_3()).add_modifier(Modifier::DIM);
    let names = [
        format!("  {}", app.say("NAME")),
        app.say("WHERE").into_owned(),
        app.say("CHANGED").into_owned(),
        app.say("SIZE").into_owned(),
    ];
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
    // What the kind's glyph takes, when there is one.
    let icon_wide = if crate::icons::drawing() { 2 } else { 0 };
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
        let under = app.hover == Spot::Row(row) || app.hover == Spot::Tick(row);
        let pushed = app.pressed == Spot::Row(row) || app.pressed == Spot::Tick(row);
        // The mark answers the pointer on its own, so that the two columns
        // that pick a row look like something that picks a row.
        let ticking = app.hover == Spot::Tick(row);
        let cells = vec![
            Cell::from(Line::from(vec![
                // The age stripe: one cell of colour, the same six bands the
                // window draws down the left of every row.
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
                Span::styled(
                    crate::icons::of_kind(hit.kind.token()).to_string(),
                    Style::new().fg(theme.kind(hit.kind.token())),
                ),
                Span::styled(cut(name, name_w.saturating_sub(icon_wide)), line),
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
        // Pressed is brighter than hovered is brighter than nothing, which is
        // the order every interface anybody has used says it in.
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
        Table::new(drawn, widths_for(area.width)).column_spacing(1),
        area,
    );

    // Our own scrollbar, on the right, because the list is a window onto a
    // result rather than a scrolled buffer: ratatui's needs to be told where
    // it is, and where it is is the cursor's row in the whole result.
    if let Some(bar) = bar {
        let mut state = ScrollbarState::new(total.saturating_sub(app.room)).position(app.top);
        // The thumb answers the pointer like everything else: brighter under
        // it, brightest while it is being dragged.
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

/// What the index holds, the biggest things in it, and what is in it twice.
///
/// **The same three panels the window and the page open with**, and the same
/// numbers — they come from the same two requests. What differs is that a
/// terminal has one column of them rather than three.
fn report(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
    let head = |what: &str| {
        Line::from(Span::styled(
            format!(" {what}"),
            Style::new()
                .fg(theme.ink_3())
                .add_modifier(Modifier::BOLD | Modifier::DIM),
        ))
    };
    let mut lines: Vec<Line> = Vec::new();

    lines.push(head(&app.say("WHAT IS INDEXED")));
    match &app.stats {
        Some(stats) => {
            let say = |what: &str, value: String| {
                Line::from(vec![
                    Span::styled(format!("   {what:<16}"), Style::new().fg(theme.ink_3())),
                    Span::styled(value, Style::new().fg(theme.ink())),
                ])
            };
            lines.push(say(
                &app.say("rows"),
                format::grouped(stats.entries, mark.0),
            ));
            lines.push(say(
                &app.say("directories"),
                format::grouped(stats.dirs, mark.0),
            ));
            lines.push(say(
                &app.say("on disk"),
                format::compact_bytes(stats.bytes_on_disk, mark.1),
            ));
        }
        None => lines.push(Line::from(Span::styled(
            format!("   {}", app.say("asking…")),
            Style::new().fg(theme.ink_3()),
        ))),
    }

    lines.push(Line::from(""));
    lines.push(head(&app.say("WHAT A FOLDER WEIGHS")));
    match &app.usage {
        Some(usage) => {
            let where_at = if app.weighing.is_empty() {
                app.say("everything indexed").into_owned()
            } else {
                app.weighing.clone()
            };
            lines.push(Line::from(vec![
                Span::styled("   ", Style::new()),
                Span::styled(
                    tail(&where_at, area.width.saturating_sub(30) as usize),
                    Style::new().fg(theme.key()),
                ),
                Span::styled(
                    format!(
                        "  {}  ·  {} {}",
                        format::compact_bytes(usage.root.bytes, mark.1),
                        format::grouped(usage.root.files, mark.0),
                        app.say("files")
                    ),
                    Style::new().fg(theme.ink_3()),
                ),
            ]));
            // **The heaviest children, with what is old in them.** A folder's
            // size says what it costs; the share that has not been touched in
            // a year says whether it is worth anything — which is the whole
            // reason this panel exists and the thing `du` cannot tell you.
            let most = usage.children.first().map(|c| c.bytes).unwrap_or(1).max(1);
            for (at, child) in usage.children.iter().take(crate::app::WEIGHED).enumerate() {
                let stale = child.age.last().copied().unwrap_or(0);
                let share = if child.bytes == 0 {
                    0
                } else {
                    (stale * 100 / child.bytes.max(1)) as u32
                };
                let bar = ((child.bytes as f64 / most as f64) * 8.0).round() as usize;
                let here = at == app.weigh_at;
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(
                            " {} {:>10}",
                            if here { "▸" } else { " " },
                            format::compact_bytes(child.bytes, mark.1)
                        ),
                        Style::new().fg(if here { theme.ink() } else { theme.ink_2() }),
                    ),
                    Span::styled(
                        format!("  {:<8}", "▇".repeat(bar.clamp(1, 8))),
                        Style::new().fg(theme.key()),
                    ),
                    Span::styled(
                        format!("{:<40}", cut(scour_ui::path::leaf(&child.path), 40)),
                        Style::new().fg(theme.ink_2()),
                    ),
                    Span::styled(
                        if share >= 5 {
                            app.say("{percent}% of it older than a year")
                                .replace("{percent}", &share.to_string())
                        } else {
                            String::new()
                        },
                        Style::new().fg(theme.ink_3()),
                    ),
                ]));
            }
        }
        None => lines.push(Line::from(Span::styled(
            format!("   {}", app.say("weighing…")),
            Style::new().fg(theme.ink_3()),
        ))),
    }

    lines.push(Line::from(""));
    lines.push(head(&app.say("THE SAME FILE, SEVERAL TIMES OVER")));
    if app.waste > 0 {
        lines.push(Line::from(vec![
            Span::styled("   ", Style::new()),
            Span::styled(
                format!(
                    "{} {}",
                    format::compact_bytes(app.waste, mark.1),
                    app.say("could be freed")
                ),
                Style::new().fg(theme.key()),
            ),
        ]));
    }
    if app.dupes.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("   {}", app.say("reading…")),
            Style::new().fg(theme.ink_3()),
        )));
    }
    let room = area.height.saturating_sub(lines.len() as u16 + 2) as usize;
    for (size, count, path) in app.dupes.iter().take(room) {
        lines.push(Line::from(vec![
            Span::styled(
                format!("   {:>10}", format::size(*size, mark.1)),
                Style::new().fg(theme.ink_2()),
            ),
            Span::styled(format!("  ×{count:<4}"), Style::new().fg(theme.ink_3())),
            Span::styled(
                tail(path, area.width.saturating_sub(24) as usize),
                Style::new().fg(theme.ink_2()),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The head of the file under the cursor, when the peek is open.
///
/// **The same answer the page's panel draws**: the service decides what can be
/// shown of a file and hands back the first of it when that is text. A
/// terminal cannot draw the picture, so it says what the file is instead.
fn head_of(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
    let [rule, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    across(f, rule, theme);
    let mut lines: Vec<Line> = Vec::new();
    match &app.peek {
        Some(look) => {
            // **The content type, and not the size.** The size is in the
            // facts below, off the row — and the two disagreed: a symlink's
            // row is 124 bytes and the file it points at is 28.7 KiB, so the
            // panel contradicted the column beside it. One number, and it is
            // the column's.
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {}", look.kind),
                    Style::new().fg(theme.key()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  ·  {}", look.shape),
                    Style::new().fg(theme.ink_3()),
                ),
            ]));
            // **What a terminal can always say about a file.** A picture has
            // no head, so the panel used to be one line saying there was
            // nothing to show — which is true of the *contents* and useless as
            // an answer: where it is, how big it is and when it changed are
            // exactly what somebody who cannot see the picture is asking. The
            // eight lines, their order and their labels are
            // `scour_ui::preview`'s, so this is the window's panel without the
            // picture.
            let room = body.height.saturating_sub(1) as usize;
            let facts = facts_of(app, mark);
            let widest = facts
                .iter()
                .map(|(label, _)| label.chars().count())
                .max()
                .unwrap_or(0);
            for (label, value) in facts.iter().take(room) {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {label:<widest$}  "),
                        Style::new().fg(theme.ink_3()),
                    ),
                    Span::styled(
                        cut(value, area.width.saturating_sub(widest as u16 + 4) as usize),
                        Style::new().fg(theme.ink_2()),
                    ),
                ]));
            }
            // And the head of it, under a blank line, when there is one.
            if !look.head.is_empty() {
                lines.push(Line::from(""));
            }
            for line in look.head.lines().take(room.saturating_sub(lines.len())) {
                // Tabs are drawn as the terminal would draw them and that is
                // not where the columns are; two spaces keeps the shape.
                let text = line.replace('\t', "  ");
                lines.push(Line::from(Span::styled(
                    format!(" {}", cut(&text, area.width.saturating_sub(2) as usize)),
                    Style::new().fg(theme.ink_2()),
                )));
            }
        }
        None => lines.push(Line::from(Span::styled(
            format!(" {}", app.say("reading…")),
            Style::new().fg(theme.ink_3()),
        ))),
    }
    f.render_widget(Paragraph::new(lines), body);
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
    // **The columns are measured once, for all of the rows.** Measuring each
    // row against its own count made the name column as wide as that row
    // needed — so `audio 332` started its bar four columns right of
    // `file 382.457`, and the rail read as a ragged staircase instead of a
    // comparison. The widest number decides, and every bar starts where every
    // other one does.
    let widest = app
        .kinds
        .iter()
        .take(kinds_shown)
        .map(|(_, n)| format::grouped(*n, mark.0).chars().count())
        .max()
        .unwrap_or(1);
    let bar_wide = 4usize;
    // A space either side of the bar, or `archive▇` runs into it.
    let name_wide = (area.width as usize)
        .saturating_sub(1 + 1 + bar_wide + 1 + widest)
        .max(3);
    // Where the rail's own cursor is, counted over the lines it offers rather
    // than the lines drawn: the headings and the blanks are not stops.
    let mut at = 0usize;
    let here = |at: usize, app: &App| app.in_rail && app.rail_at == at;
    // The rail answers the pointer the same way the list does.
    let touched = |at: usize, app: &App| {
        if app.pressed == Spot::Rail(at) {
            Some(Style::new().bg(theme.key()).fg(theme.back()))
        } else if app.hover == Spot::Rail(at) {
            Some(Style::new().bg(theme.hover()))
        } else {
            None
        }
    };
    lines.push(head(&app.say("KIND")));
    for (token, count) in app.kinds.iter().take(kinds_shown) {
        let term = scour_ui::query::of_kind(token);
        let on = app.filter.as_deref() == Some(term.as_str());
        // **The count is never cut, and the arithmetic says so out loud**: one
        // column for the cursor, four for the bar, whatever the number needs,
        // and the name takes what is left. `507.69` is not a number — it was
        // `507.691` with its last digit run off the end of a line that had
        // been counted at a width the rail does not have.
        let said = format::grouped(*count, mark.0);
        // **The word, not the token.** The service counts in the query
        // language's own vocabulary — `doc`, `exec`, `build` — and that is
        // what goes back to it in a `kind:` term; what a reader sees is the
        // word for it in their language, which is the label the window and the
        // page draw from the same msgid.
        let word = kind_word(app, token);
        let (bar, name) = (bar_wide, name_wide);
        let width = ((*count as f64 / most as f64) * bar as f64).round() as usize;
        let cursor = here(at, app);
        let lit = touched(at, app);
        at += 1;
        lines.push(
            Line::from(vec![
                Span::styled(
                    format!("{}{:<name$} ", mark_of(on, cursor), cut(&word, name)),
                    // **Colour means in force, the arrow means the cursor is
                    // here.** They were the same thing, so a filter somebody
                    // had just taken off left its row lit as though it were
                    // still on.
                    Style::new().fg(if on { theme.key() } else { theme.ink_2() }),
                ),
                Span::styled(
                    // A kind with none of it gets no bar: a bar means "some",
                    // and the shortest one there is would be a lie.
                    format!(
                        "{:<bar$}",
                        "▇".repeat(if *count == 0 { 0 } else { width.clamp(1, bar) })
                    ),
                    Style::new().fg(theme.kind(token)),
                ),
                Span::styled(format!(" {said:>widest$}"), Style::new().fg(theme.ink_3())),
            ])
            .style(lit.unwrap_or_default()),
        );
    }
    lines.push(Line::from(""));
    lines.push(head(&app.say("WHERE")));
    for (label, path) in app.places.iter().take(places_shown) {
        let term = scour_ui::query::of_place(path);
        let on = app.filter.as_deref() == Some(term.as_str());
        let cursor = here(at, app);
        let lit = touched(at, app);
        at += 1;
        lines.push(
            Line::from(Span::styled(
                format!(
                    "{}{}",
                    if on {
                        "●"
                    } else if cursor {
                        "▸"
                    } else {
                        " "
                    },
                    cut(label, 19)
                ),
                Style::new().fg(if on { theme.key() } else { theme.ink_2() }),
            ))
            .style(lit.unwrap_or_default()),
        );
    }
    lines.push(Line::from(""));
    lines.push(head(&app.say("SIZE")));
    for (label, term) in scour_ui::query::SIZES {
        let on = app.filter.as_deref() == Some(term);
        let cursor = here(at, app);
        let lit = touched(at, app);
        at += 1;
        lines.push(
            Line::from(Span::styled(
                format!(
                    "{}{label}",
                    if on {
                        "●"
                    } else if cursor {
                        "▸"
                    } else {
                        " "
                    }
                ),
                Style::new().fg(if on { theme.key() } else { theme.ink_2() }),
            ))
            .style(lit.unwrap_or_default()),
        );
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The row under the cursor, as the facts a preview panel lists.
///
/// **Off the row, with nothing asked for.** A `Hit` already carries its
/// `Meta` — size, the three dates, the mode, the owner — so the panel that the
/// window fills with a `Stat` round trip is filled here by reading what is on
/// screen. The formatting is shared: what a size looks like in binary units
/// and how a date is punctuated are `scour-ui`'s, and a mode and an owner name
/// are `scour-core`'s.
fn facts_of(app: &App, mark: (char, char)) -> Vec<(String, String)> {
    let Some(hit) = app.here() else {
        return Vec::new();
    };
    let m = &hit.meta;
    let items = if hit.is_dir && m.items >= 0 {
        app.say("{n} items")
            .replace("{n}", &format::grouped(m.items as u64, mark.0))
    } else {
        String::new()
    };
    let facts = scour_ui::preview::Facts {
        folder: scour_ui::path::folder(&hit.path),
        kind: &app.say(hit.kind.msgid()),
        is_dir: hit.is_dir,
        size: m.size.max(0) as u64,
        items: &items,
        mtime: m.mtime,
        ctime: m.ctime,
        atime: m.atime,
        mode: &scour_core::mode_string(m.mode),
        owner: &[
            scour_core::owner_name(scour_core::Owner::User, m.uid),
            scour_core::owner_name(scour_core::Owner::Group, m.gid),
        ]
        .join(" · "),
    };
    facts
        .lines(mark.1)
        .into_iter()
        .map(|(msgid, value)| (app.say(msgid).into_owned(), value))
        .collect()
}

/// The word for a kind, given the token the service counts in.
///
/// The engine's own msgid, so the rail says `Belge` where the window says
/// `Belge` — one vocabulary, and a kind the engine learns tomorrow arrives in
/// all three faces at once. A token with no kind is drawn as itself rather
/// than as nothing.
fn kind_word<'a>(app: &'a App, token: &'a str) -> std::borrow::Cow<'a, str> {
    match scour_core::Kind::OFFERED
        .iter()
        .find(|k| k.token() == token)
    {
        Some(kind) => app.say(kind.msgid()),
        None => std::borrow::Cow::Borrowed(token),
    }
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
fn when(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
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
        // **The band under the pointer answers it**, like every other thing
        // here: lit while it is hovered, in the query's own colour while it is
        // held, and the one already pressed stays lit.
        let on = app.filter.as_deref() == Some(scour_ui::query::of_age(*days).as_str());
        let colour = if app.pressed == Spot::Strip(i) {
            Style::new().fg(theme.back()).bg(theme.key())
        } else if app.hover == Spot::Strip(i) {
            Style::new().fg(theme.ink()).bg(theme.hover())
        } else if on {
            Style::new().fg(theme.key())
        } else {
            Style::new().fg(theme.band(scour_ui::band_of(*days as f64)))
        };
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

    let dim = Style::new().fg(theme.ink_3());
    // **What the pointer is on, said in words.** A bar is a shape; how many
    // files it stands for and how long ago that is are what somebody is
    // squinting at it to find out, and the axis is free while they are.
    if let Spot::Strip(at) = app.hover
        && let Some((days, count)) = app.strip.get(at)
    {
        let said = if *days <= 1 {
            app.say("since yesterday").into_owned()
        } else if *days < 60 {
            app.say("last {days} days")
                .replace("{days}", &days.to_string())
        } else {
            app.say("last {n} months")
                .replace("{n}", &(days / 30).to_string())
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {said}"), Style::new().fg(theme.key())),
                Span::styled(
                    format!(
                        "  ·  {} {}",
                        format::grouped(*count, mark.0),
                        app.say("files")
                    ),
                    dim,
                ),
            ])),
            axis,
        );
        return;
    }
    // The axis under the ends of the strip, not the ends of the terminal.
    let left = app.say("2 years ago");
    let right = app.say("today");
    let gap = room.saturating_sub(left.chars().count() + right.chars().count());
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {left}{}{right}", " ".repeat(gap)),
            dim,
        ))),
        axis,
    );
}

/// Where each tool is drawn along the counter line, right to left.
///
/// **One function, two callers**, like everything else here that can be
/// pressed: the words that are drawn are the words that are hit.
pub fn tool_spans(app: &App, width: u16) -> Vec<(u16, u16, String)> {
    let mut out = Vec::new();
    let mut from = width.saturating_sub(1);
    let all = app.tools();
    for (at, (label, key)) in all.iter().enumerate().rev() {
        let glyph = crate::icons::of_tool(at);
        // The label is a msgid; it is looked up *here*, where its width is
        // also measured, because those two have to be the same string.
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

/// Where each of the selection's three buttons is drawn, along the bottom.
///
/// **The same arithmetic that draws them.** They are words on a line, and the
/// only thing that makes them buttons is that a press on one of them does
/// what it says.
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

/// What sits in the rail's first column: a dot for the filter in force, an
/// arrow for where the keyboard cursor is, a space for everything else.
fn mark_of(on: bool, cursor: bool) -> &'static str {
    if on {
        "●"
    } else if cursor {
        "▸"
    } else {
        " "
    }
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
        let mut said = app.say("{n} selected").replace("{n}", &picked.to_string());
        if folders > 0 {
            said.push_str(&format!(
                " · {}",
                app.say("{n} folders").replace("{n}", &folders.to_string())
            ));
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
        app.say("rail")
    } else {
        match app.mode {
            Mode::Search => app.say("search"),
            Mode::Move => app.say("move"),
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
    // **What can be done with a selection displaces the order and the mode**,
    // because it is the thing about to be acted on and they are not.
    if picked > 0 {
        let mut parts: Vec<Span> = Vec::new();
        for (at, (_, _, said)) in deed_spans(app, area.width).into_iter().enumerate() {
            let style = if app.pressed == Spot::Deed(at) {
                Style::new().bg(theme.key()).fg(theme.back())
            } else if app.hover == Spot::Deed(at) {
                Style::new().bg(theme.hover()).fg(theme.ink())
            } else {
                Style::new()
                    .fg(theme.ink_2())
                    .add_modifier(Modifier::UNDERLINED)
            };
            parts.push(Span::styled(said, style));
        }
        f.render_widget(Paragraph::new(Line::from(parts)).right_aligned(), area);
        return;
    }
    let order = format!(
        "{} {}  {mode} ",
        app.say(app.sort_name()),
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
fn help(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    // Wide enough for the longest line in it rather than a round number:
    // `sort by the next column · or click a heading` is forty-four characters
    // in English and fifty-five in Turkish, and at sixty the box cut both of
    // them — the one panel whose whole job is to be read.
    let wide = 88u16.min(area.width.saturating_sub(4));
    let tall = (crate::keys::MAP.len() as u16 + 4).min(area.height);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(wide)) / 2,
        y: area.y + (area.height.saturating_sub(tall)) / 2,
        width: wide,
        height: tall,
    };
    f.render_widget(Clear, box_area);
    let mut lines = vec![Line::from(Span::styled(
        format!(" {}", app.say("KEYS")),
        Style::new().fg(theme.ink()).add_modifier(Modifier::BOLD),
    ))];
    // **Both columns go through the catalogue, and only one of them changes.**
    // The left column is mostly keycaps — `Ctrl+A`, `Tab`, `F1` — which are
    // what is printed on the keyboard in every language and are not in the
    // catalogue, so they come back as themselves. The few that are not
    // keycaps, `type` and `click the ✓ column`, are sentences and are. One
    // rule, and nothing to keep in step.
    for (key, what) in crate::keys::MAP {
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {:<28}", app.say(key)),
                Style::new().fg(theme.key()),
            ),
            Span::styled(app.say(what), Style::new().fg(theme.ink_2())),
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
