//! The report: a folder's weight, its children, its kinds.

use super::*;
use crate::app::App;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use scour_ui::format;

/// What the index holds, the biggest things in it, and what is in it twice —
/// the window's three panels, in one column and from the same requests.
pub(super) fn report(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
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
        Some(usage) => lines.extend(weighed(area, app, theme, mark, usage)),
        None => lines.push(Line::from(Span::styled(
            format!("   {}", app.say("weighing…")),
            Style::new().fg(theme.ink_3()),
        ))),
    }

    lines.push(Line::from(""));
    lines.push(head(&app.say("BY KIND")));
    lines.extend(by_kind(area, app, theme));

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

/// How many folders the kind bar names before the rest becomes one slice: the
/// palette has six steps, and a seventh would repeat one of them.
const KINDS: usize = 6;

/// The weighed folder: what it is, how old its bytes are, and a row for each of
/// the heaviest children with everything else as one line under them.
fn weighed(
    area: Rect,
    app: &App,
    theme: &Theme,
    mark: (char, char),
    usage: &scour_core::UsageResponse,
) -> Vec<Line<'static>> {
    let where_at = match app.weighing.is_empty() {
        true => app.say("everything indexed").into_owned(),
        false => app.weighing.clone(),
    };
    let mut lines = vec![Line::from(vec![
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
    ])];

    let files = crate::report::files_wide(&usage.children, crate::app::WEIGHED, mark.0);
    let fit = crate::report::fit(area.width as usize, files);
    // The words are asked for one by one, not out of a table: the catalogue
    // test reads this file for `say(` and a table would go unchecked.
    let bands = [
        app.say("today").into_owned(),
        app.say("week").into_owned(),
        app.say("month").into_owned(),
        app.say("six months").into_owned(),
        app.say("year").into_owned(),
        app.say("older").into_owned(),
    ];
    lines.extend(strip_of(
        area,
        &usage.root.age,
        &bands,
        &app.say("age"),
        fit.percents,
        |band| theme.band(band),
        theme,
    ));

    // One blank line, and the only one in the block: above it the folder as a
    // whole, below it the folders inside it.
    lines.push(Line::from(""));
    let shown = crate::app::WEIGHED.min(usage.children.len());
    let (share, rest) = crate::report::parts(usage, shown);
    // The heaviest child sets the scale: a bar is this folder against its
    // largest sibling, which is the comparison the eye is making anyway.
    let most = usage.children.first().map(|c| c.bytes).unwrap_or(1).max(1);
    for (at, child) in usage.children.iter().take(shown).enumerate() {
        let row = crate::report::row(child, at == app.weigh_at, most, share[at], &fit, mark);
        lines.push(Line::from(weighed_row(&row, theme)));
    }
    // Only when there are folders under the fold: the line names them, and the
    // bytes of the folder itself are in the total on the line above.
    if let Some(rest) = rest.filter(|rest| rest.count > 0) {
        lines.push(Line::from(Span::styled(
            crate::report::rest_line(
                &rest,
                share.get(shown).copied().unwrap_or(0.0),
                &app.say("the other {n} folders"),
                mark,
            ),
            Style::new().fg(theme.ink_3()),
        )));
    }
    lines
}

/// One weighed row, painted. The text is [`crate::report::Row::text`]'s, span by
/// span: what the columns are is decided in one place and coloured in another.
pub(super) fn weighed_row(row: &crate::report::Row, theme: &Theme) -> Vec<Span<'static>> {
    let ink = match row.here {
        true => theme.ink(),
        false => theme.ink_2(),
    };
    let mut spans = vec![Span::styled(
        format!(" {} {}  ", if row.here { '▸' } else { ' ' }, row.size),
        Style::new().fg(ink),
    )];
    for (band, glyphs) in &row.bar {
        spans.push(Span::styled(
            glyphs.clone(),
            Style::new().fg(theme.band(*band)),
        ));
    }
    spans.push(Span::raw(" ".repeat(row.pad + 2)));
    spans.push(Span::styled(row.name.clone(), Style::new().fg(ink)));
    spans.push(Span::styled(
        format!("  {}", row.share),
        Style::new().fg(theme.ink_3()),
    ));
    if !row.files.is_empty() {
        spans.push(Span::styled(
            format!("  {}", row.files),
            Style::new().fg(theme.ink_3()),
        ));
    }
    spans
}

/// A strip and the legend under it: one line of colour, one line of words. The
/// label is drawn in front of the strip, and the legend lines up with it.
fn strip_of(
    area: Rect,
    values: &[u64],
    words: &[String],
    label: &str,
    percents: bool,
    colour: impl Fn(usize) -> ratatui::style::Color,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let indent = 3 + label.chars().count() + 3;
    let wide = (area.width as usize).saturating_sub(indent);
    let mut bar = vec![Span::styled(
        format!("   {label}   "),
        Style::new().fg(theme.ink_3()),
    )];
    for (at, cells) in crate::report::strip(values, wide) {
        bar.push(Span::styled("█".repeat(cells), Style::new().fg(colour(at))));
    }
    let mut legend = vec![Span::raw(" ".repeat(indent))];
    for (at, said) in crate::report::legend(values, words, percents) {
        legend.push(Span::styled("■ ", Style::new().fg(colour(at))));
        legend.push(Span::styled(
            format!("{said}  "),
            Style::new().fg(theme.ink_2()),
        ));
    }
    vec![Line::from(bar), Line::from(legend)]
}

/// What the weighed folder is made of: the six largest kinds as one bar, the
/// rest as one slice, and a legend naming as many as the width can hold.
fn by_kind(area: Rect, app: &App, theme: &Theme) -> Vec<Line<'static>> {
    if app.scope_kinds.is_empty() {
        return vec![Line::from(Span::styled(
            format!("   {}", app.say("asking…")),
            Style::new().fg(theme.ink_3()),
        ))];
    }
    let wide = (area.width as usize).saturating_sub(3);
    let percents = area.width as usize >= crate::report::NARROW;
    let other = app.say("other").into_owned();
    // How many the legend can name decides how many the bar draws: a slice
    // with no word beside it is a colour nobody can read.
    let (kept, _) = crate::report::kinds(&app.scope_kinds, KINDS);
    let words: Vec<String> = kept
        .iter()
        .map(|(token, _)| kind_word(app, token).into_owned())
        .collect();
    let room = wide.saturating_sub(other.chars().count() + if percents { 5 } else { 2 });
    let (kept, rest) = crate::report::kinds(
        &app.scope_kinds,
        crate::report::named(&words, room, percents),
    );

    let mut values: Vec<u64> = kept.iter().map(|(_, n)| *n).collect();
    let mut words: Vec<String> = kept
        .iter()
        .map(|(token, _)| kind_word(app, token).into_owned())
        .collect();
    if let Some(rest) = &rest {
        values.push(rest.value);
        words.push(other);
    }
    let colour = |at: usize| match at < kept.len() {
        true => theme.step(at),
        false => theme.step_rest(),
    };
    let mut bar = vec![Span::raw("   ")];
    for (at, cells) in crate::report::strip(&values, wide) {
        bar.push(Span::styled("▓".repeat(cells), Style::new().fg(colour(at))));
    }
    let mut legend = vec![Span::raw("   ")];
    for (at, said) in crate::report::legend(&values, &words, percents) {
        let ink = match at < kept.len() {
            true => theme.ink_2(),
            false => theme.ink_3(),
        };
        legend.push(Span::styled(format!("{said}  "), Style::new().fg(ink)));
    }
    vec![Line::from(bar), Line::from(legend)]
}
