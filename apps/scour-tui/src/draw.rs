//! One frame.
//!
//! Immediate mode: everything is written every frame and ratatui sends only the
//! cells that differ, so no widget tree has to be kept in step. The layout is
//! the page's and the window's — query, meter, list, one line at the bottom.

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

/// How tall the query field is. Three rows rather than one: a line of text
/// among lines of text does not read as a box to type in.
pub const QUERY_HIGH: u16 = 3;

/// Lines the list does not get: the query field, the meter, the rule under
/// them, the column heading, and the footer.
const CHROME: u16 = QUERY_HIGH + 4;

/// Which row of the field the text is on.
pub const QUERY_ROW: u16 = QUERY_HIGH / 2;
/// Where the list starts, and where the rail starts — one line higher, because
/// the rail takes the heading line. Public: the mouse counts in them too.
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
    // How this language punctuates numbers, off the catalogue rather than the
    // desktop, so switching the language switches the digits with the words.
    let mark = app.mark();
    f.render_widget(Block::new().style(Style::new().bg(theme.back())), area);
    // The strip is worth its rows only above twenty: below that it takes a
    // fifth of the list.
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
    // A line under the two of them, or the query, the counts and the headings
    // read as three rows of text with nothing saying which is which.
    across(f, rule, theme);
    // The rail needs a hundred columns to be worth its twenty-four: at eighty
    // it left the name ten characters wide, which is a hint, not a name.
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
    // The peek takes the bottom of the list, so the row it is about stays on
    // screen. Twelve lines is the ten the facts need plus two of the file, and
    // eighteen is where it stops taking room from the list.
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
        // A line between the rail and the list, or the age stripe down each
        // row butts against the rail's text and the two read as one column.
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

/// One line of a panel: what it says, and how it is drawn.
pub struct PanelLine {
    pub text: String,
    /// Printed at the right edge. Empty where there is none.
    pub key: String,
    pub dimmed: bool,
    pub careful: bool,
    pub rule: bool,
}

impl From<(String, bool)> for PanelLine {
    fn from((text, dimmed): (String, bool)) -> Self {
        PanelLine {
            text,
            key: String::new(),
            dimmed,
            careful: false,
            rule: false,
        }
    }
}

/// Where a panel of this many lines is drawn. One function, two callers — this
/// and the mouse — because a panel hit-tested elsewhere drifts from where it is.
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

/// Whatever panel is open, over the middle of the screen. One drawing for all
/// of them: a title, a list, and a cursor on one line of it.
fn panel(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let (title, lines): (std::borrow::Cow<str>, Vec<PanelLine>) = match app.panel {
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
                        .into()
                })
                .collect(),
        ),
        // Spelled in themselves, as in `scour_i18n::LANGUAGES`: a reader may
        // not recognise what the language they are reading calls theirs.
        Panel::Language => (
            app.say("LANGUAGE"),
            scour_i18n::LANGUAGES
                .iter()
                .map(|(_, endonym)| ((*endonym).to_string(), false).into())
                .collect(),
        ),
        Panel::Faces => (
            app.say("HOW TO RUN IT"),
            vec![
                (app.say("Window").into_owned(), false).into(),
                (
                    format!("{}  ·  {}", app.say("Terminal"), app.say("running now")),
                    true,
                )
                    .into(),
                (
                    format!(
                        "{}  ·  {}",
                        app.say("Browser"),
                        app.say("opens a port on 127.0.0.1")
                    ),
                    false,
                )
                    .into(),
            ],
        ),
        // The menu as `scour-ui::menu` holds it; every face draws that list.
        Panel::Menu => (
            app.say("WHAT CAN BE DONE"),
            app.menu
                .iter()
                .map(|m| PanelLine {
                    text: m.label.clone(),
                    key: m.key.clone(),
                    dimmed: m.heavy,
                    careful: m.careful,
                    rule: m.rule,
                })
                .collect(),
        ),
        // The tick is in the text, not a column: a label that shifts sideways
        // when it is switched is one nobody can aim at twice.
        Panel::Columns => (
            app.say("COLUMNS"),
            scour_ui::COLUMNS
                .iter()
                .map(|c| {
                    let on = app.columns.iter().any(|x| x.id == c.id);
                    PanelLine {
                        text: format!("{} {}", if on { "✓" } else { " " }, app.say(c.msgid)),
                        key: String::new(),
                        dimmed: !on,
                        careful: false,
                        rule: false,
                    }
                })
                .chain(std::iter::once(PanelLine {
                    text: format!("  {}", app.say("Back to the default")),
                    key: String::new(),
                    dimmed: false,
                    careful: false,
                    rule: true,
                }))
                .collect(),
        ),
        Panel::Openers => (
            app.say("OPEN WITH"),
            app.openers
                .iter()
                .map(|(_, name)| (name.clone(), false).into())
                .collect(),
        ),
        // The cursor opens on the first line, which changes nothing.
        Panel::Ask => (
            std::borrow::Cow::Owned(app.ask_title.clone()),
            vec![
                // The line being typed, drawn as a panel line so that what the
                // keyboard is talking to is in the list the cursor walks.
                PanelLine {
                    text: if app.ask_typing {
                        format!("› {}█", app.ask_text)
                    } else {
                        String::new()
                    },
                    key: String::new(),
                    dimmed: false,
                    careful: false,
                    rule: false,
                },
                (app.say("Cancel").into_owned(), false).into(),
                PanelLine {
                    text: app.ask_yes.clone(),
                    key: String::new(),
                    dimmed: false,
                    careful: true,
                    rule: false,
                },
            ],
        ),
        Panel::None => return,
    };

    // The rules take rows too: sized by the item count alone the box comes up
    // short and the last items fall off with no sign there were more.
    let rules = lines.iter().filter(|l| l.rule).count();
    let box_area = panel_rect(area, lines.len() + rules);
    f.render_widget(Clear, box_area);
    // Only what fits, scrolled to keep the cursor on it: the skip list is long.
    let room = (box_area.height.saturating_sub(3) as usize).saturating_sub(rules);
    let from = app.panel_at.saturating_sub(room.saturating_sub(1));
    let mut drawn = vec![Line::from(Span::styled(
        format!(" {title}"),
        Style::new().fg(theme.ink()).add_modifier(Modifier::BOLD),
    ))];
    for (at, line) in lines.iter().enumerate().skip(from).take(room) {
        let dimmed = &line.dimmed;
        let on = at == app.panel_at;
        let lit = if app.pressed == Spot::Panel(at) {
            Style::new().bg(theme.key()).fg(theme.back())
        } else if app.hover == Spot::Panel(at) {
            Style::new().bg(theme.hover())
        } else {
            Style::new()
        };
        // A rule where the group changes, not counted as a line: a separator
        // the cursor can land on is a press that does nothing.
        if line.rule {
            drawn.push(Line::from(Span::styled(
                format!(
                    "   {}",
                    "─".repeat(box_area.width.saturating_sub(8) as usize)
                ),
                Style::new().fg(theme.line()),
            )));
        }
        // The shortcut is quieter than the label: it is to be learned, not read.
        let room_for_text = box_area.width.saturating_sub(6) as usize;
        let body = if line.key.is_empty() {
            line.text.clone()
        } else {
            let pad =
                room_for_text.saturating_sub(line.text.chars().count() + line.key.chars().count());
            format!("{}{}{}", line.text, " ".repeat(pad), line.key)
        };
        drawn.push(
            Line::from(Span::styled(
                format!("{}{body}", if on { " ▸ " } else { "   " }),
                Style::new().fg(if on {
                    theme.key()
                } else if line.careful {
                    theme.danger()
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
    // The whole line is the field: a strip of panel, as the page draws one.
    f.render_widget(Block::new().style(Style::new().bg(theme.panel())), area);
    // The text sits on the middle row, with a row of quiet above and below:
    // that is what makes three rows read as one box.
    let area = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    let typed = if app.query.is_empty() {
        // Quiet and short: a hint as bright as a query reads as one.
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
    // The query in colour once the service has read it back: the same roles
    // and colours the window and the page use.
    let coloured: Vec<Span> = app
        .spans
        .iter()
        .filter_map(|sp| {
            let from = sp.start as usize;
            let to = from + sp.len as usize;
            let text = app.query.get(from..to)?;
            let colour = match sp.role {
                // A mistake outranks a polarity: an unreadable value is
                // searched for as text, which is what matters about it.
                scour_core::Role::UnknownField | scour_core::Role::BadValue => theme.bad(),
                // Excluded is excluded, all of it — not only the `!`.
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
    // What is pressed is shown beside what was typed: a band of the time strip
    // has no rail row of its own to show that it is in force.
    if let Some(term) = &app.filter {
        parts.push(Span::styled("  ·  ", Style::new().fg(theme.ink_3())));
        // Red under the pointer, and gone when pressed: for a `dm:` band this
        // is the only place the term is written, so the only way to remove it.
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
            // Whole sentences, not words glued together: Turkish puts the
            // total first, and "at least" does not sit where English puts it.
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
    // What the index is doing, beside what the search found: switching a skip
    // rule off starts a walk, and nothing else on this line moves while it runs.
    if let Some(walked) = app.scanning {
        parts.push(Span::styled("  ·  ", dim));
        parts.push(Span::styled(
            app.say(scour_ui::SCANNING)
                .replace("{n}", &format::grouped(walked, mark.0)),
            Style::new().fg(theme.key()),
        ));
    }
    // And whether searching is still as fast as it was built to be: every query
    // reads the unsorted tail, which a week of use takes from 1.9 ms to 21.5.
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
    // without them: the keys still work.
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

/// Where the filter is drawn on the query line, if one is pressed. The same
/// arithmetic that draws it, so the pointer lands on what it lit.
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

/// What an empty query says instead of nothing. Its width decides where the
/// filter chip is drawn and hit, and a translated hint is a different width.
fn hint(app: &App) -> std::borrow::Cow<'_, str> {
    app.say("search…")
}

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
fn heading(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
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

fn rows(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
    let total = app.pages.total();
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
                        spans.push(Span::styled(
                            cut(name, left.saturating_sub(icon_wide)),
                            line,
                        ));
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

/// What the index holds, the biggest things in it, and what is in it twice —
/// the window's three panels, in one column and from the same requests.
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
            // The heaviest children, with the share untouched for a year: the
            // size says what a folder costs, that share says whether it earns it.
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

/// The head of the file under the cursor, when the peek is open. The service
/// decides what can be shown; a terminal cannot draw a picture, so it says what
/// the file is instead.
fn head_of(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
    let [rule, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    across(f, rule, theme);
    let mut lines: Vec<Line> = Vec::new();
    match &app.peek {
        Some(look) => {
            // The content type and not the size: for a symlink the two disagree,
            // and the number that stands is the column's.
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
            // What a terminal can always say about a file, for the ones with no
            // head to show. The lines, their order and their labels are
            // `scour_ui::preview`'s: the window's panel without the picture.
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
                // A tab lands where the terminal puts it; two spaces keep shape.
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
    // The widest number decides for every row: measured per row, each bar
    // starts at a different column and the rail reads as a staircase.
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
        // The count is never cut: one column for the cursor, four for the bar,
        // whatever the number needs, and the name takes what is left.
        let said = format::grouped(*count, mark.0);
        // The word, not the token: `kind:` terms travel in the query language's
        // vocabulary, and the reader sees the msgid every face draws.
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
                    // Colour means in force; the arrow means the cursor is here.
                    Style::new().fg(if on { theme.key() } else { theme.ink_2() }),
                ),
                Span::styled(
                    // A kind with none of it gets no bar: a bar means "some".
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

/// The row under the cursor, as the facts a preview panel lists. Off the row,
/// with nothing asked for: a `Hit` already carries its `Meta`.
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

/// The word for a kind, given the token the service counts in: the engine's own
/// msgid, so every face says the same word. An unknown token is drawn as itself.
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

/// The twenty-four bars of the time strip, and the axis under them. Drawn with
/// block characters, because the shape of the distribution is the point.
fn when(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
    // Eight heights in a cell, two cells tall: sixteen steps, because a
    // distribution drawn in eight is a staircase.
    const BLOCKS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let [upper, lower, axis] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let bands = app.strip.len().max(1);
    let room = upper.width.saturating_sub(2) as usize;
    // The strip is as wide as the window: every band gets the same share and
    // the remainder is spread from the left, so no band leads another by more
    // than one column.
    let each = (room / bands).max(1);
    let spare = room.saturating_sub(each * bands);
    let most = app.strip.iter().map(|(_, n)| *n).max().unwrap_or(1).max(1);

    let mut top: Vec<Span> = vec![Span::raw(" ")];
    let mut bottom: Vec<Span> = vec![Span::raw(" ")];
    for (i, (days, count)) in app.strip.iter().enumerate() {
        let wide = each + usize::from(i < spare);
        // A column of space between bars where there is room, or the bands run
        // together and no height belongs to one band.
        let (wide, gap) = if wide >= 3 { (wide - 1, 1) } else { (wide, 0) };
        let step = if *count == 0 {
            0
        } else {
            // Nothing is nothing, anything is at least a tick: four files in
            // nine thousand round to nought, and a blank reads as none.
            (((*count as f64 / most as f64) * 16.0).round() as usize).max(1)
        };
        // Lit while hovered, in the query's colour while held, and the one in
        // force stays lit.
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
    // What the pointer is on, in words: a bar is a shape, and the axis line is
    // free while somebody is reading one.
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
    // The axis under the ends of the strip, not of the terminal.
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

/// A label, cut to fit rather than wrapped: a rail is one line per thing. The
/// ellipsis is what says four identical-looking rows are not identical.
fn cut(text: &str, to: usize) -> String {
    if text.chars().count() <= to {
        return text.to_string();
    }
    text.chars().take(to.saturating_sub(1)).collect::<String>() + "…"
}

/// A path, cut from the front: paths share their beginnings, and the end is the
/// part that says which folder this is.
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
    // The query line's strip of panel, closing the window without a rule.
    f.render_widget(Block::new().style(Style::new().bg(theme.panel())), area);
    let dim = Style::new().fg(theme.ink_3());
    let (picked, folders, bytes) = app.weighed();
    // What is picked displaces where the cursor is: a selection is about to be
    // acted on, and the path is already in the list.
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
            // The column's format, not the meter's: `0,0 MB` says nothing.
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
    // What can be done with a selection displaces the order and the mode.
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
    // Wide enough for its longest line: fifty-five characters in Turkish, and
    // a box of sixty cut it.
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
    // Both columns go through the catalogue and only one changes: keycaps are
    // not in it and come back as themselves, the few sentences are.
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
