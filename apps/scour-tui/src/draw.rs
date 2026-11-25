//! One frame.
//!
//! Immediate mode: everything is written every frame and ratatui sends only the
//! cells that differ, so no widget tree has to be kept in step. The layout is
//! the page's and the window's — query, meter, list, one line at the bottom.

mod list;
mod menu;
mod panels;
mod placecap;
mod settings;
mod side;
mod top;
mod weighed;
pub use list::*;
pub use menu::*;
pub use panels::*;
pub use settings::*;
use side::*;
pub use top::*;
use weighed::*;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

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
    let chosen = theme.chosen(&app.theme);
    let theme = &chosen;
    let area = f.area();
    // The settings are a screen of their own, over everything but the help.
    if app.settings {
        settings_screen(f, area, app, theme);
        if app.helping {
            help(f, area, app, theme);
        }
        return;
    }
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

#[cfg(test)]
mod tests;
