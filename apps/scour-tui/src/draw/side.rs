//! The side column: the head of a file and its facts.

use super::*;
use crate::app::{App, Spot};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use scour_ui::format;

/// The head of the file under the cursor, when the peek is open. The service
/// decides what can be shown; a terminal cannot draw a picture, so it says what
/// the file is instead.
pub(super) fn head_of(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
    let [rule, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    across(f, rule, theme);
    let mut lines: Vec<Line> = Vec::new();
    // Where the file holds the text a `content:` query asked for, first and
    // without waiting for the file's head: it is why the file is listed.
    if let Some(hit) = app.pages.at(app.cursor)
        && !hit.found.is_empty()
    {
        lines.push(Line::from(Span::styled(
            format!(" {}", app.say("Found in the file")),
            Style::new().fg(theme.ink_3()),
        )));
        let wide = area.width.saturating_sub(2) as usize;
        for found in &hit.found {
            lines.push(found_line(found, wide, theme));
        }
        lines.push(Line::from(""));
    }
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
pub(super) fn side(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
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
    for (label, term) in crate::app::rail_sizes() {
        let label = app.say(label);
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
pub(super) fn kind_word<'a>(app: &'a App, token: &'a str) -> std::borrow::Cow<'a, str> {
    match scour_core::Kind::OFFERED
        .iter()
        .find(|k| k.token() == token)
    {
        Some(kind) => app.say(kind.msgid()),
        None => std::borrow::Cow::Borrowed(token),
    }
}

/// A rule across the window, in the quietest line colour there is.
pub(super) fn across(f: &mut Frame, area: Rect, theme: &Theme) {
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
pub(super) fn when(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
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
