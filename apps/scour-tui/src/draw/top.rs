//! The query line and the counts under it.

use super::*;
use crate::app::{App, Mode, Spot};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use scour_ui::format;

use super::grepcap::{self, Laid};
use super::placecap::{self, Folded};
use super::valuecap;

/// The query line: the brand, what has been typed, and the caret.
pub(super) fn query(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    // The whole line is the field: a strip of panel, as the page draws one.
    f.render_widget(Block::new().style(Style::new().bg(theme.panel())), area);
    // The text sits on the middle row, with a row of quiet above and below:
    // that is what makes three rows read as one box.
    let field = area;
    let area = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    let triggers_shown = !app.v2.grep_open && area.width >= 96;
    let folds = folds(app, area.width);
    let typed = if app.query.is_empty() {
        // Quiet and short: a hint as bright as a query reads as one.
        vec![Span::styled(
            hint(app),
            Style::new()
                .fg(theme.ink_3())
                .add_modifier(Modifier::ITALIC | Modifier::DIM),
        )]
    } else {
        let plain = Style::new().fg(theme.ink()).add_modifier(Modifier::BOLD);
        with_folds(&app.query, vec![(0, app.query.len(), plain)], &folds, theme)
    };
    // The query in colour once the service has read it back: the same roles
    // and colours the window and the page use.
    let runs: Vec<(usize, usize, Style)> = app
        .spans
        .iter()
        .filter_map(|sp| {
            let from = sp.start as usize;
            let to = from + sp.len as usize;
            app.query.get(from..to)?;
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
            let capsule = matches!(
                sp.role,
                scour_core::Role::Field
                    | scour_core::Role::Value
                    | scour_core::Role::Not
                    | scour_core::Role::Colon
            );
            let mut st = Style::new().fg(colour).add_modifier(Modifier::BOLD);
            if capsule {
                st = st.bg(theme.hover());
            }
            Some((from, to, st))
        })
        .collect();
    let coloured = with_folds(&app.query, runs, &folds, theme);
    let mut parts = vec![Span::styled(
        " SCOUR ",
        Style::new()
            .fg(theme.key())
            .add_modifier(Modifier::BOLD | Modifier::DIM),
    )];
    // The coloured runs cover the whole query when they are here; the plain
    // text stands in until they arrive, so nothing blinks between the two.
    if coloured.is_empty() {
        parts.extend(typed);
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
    let mut grep_caret_x: Option<u16> = None;
    if app.v2.grep_open {
        let pre_cols: usize = parts.iter().map(|s| s.content.chars().count()).sum();
        let mode_lbl = match app.v2.grep_mode {
            scour_proto::GrepMode::Text => app.say("Text"),
            scour_proto::GrepMode::Regex => app.say("Regex"),
            scour_proto::GrepMode::Hex => app.say("Hex"),
        };
        let pat_shown = if app.v2.grep_pattern.is_empty() {
            app.say("Search inside matching files…").into_owned()
        } else {
            app.v2.grep_pattern.clone()
        };
        parts.push(Span::styled(
            "  [ ≣ ",
            Style::new()
                .fg(theme.key())
                .bg(theme.hover())
                .add_modifier(Modifier::BOLD),
        ));
        if app.v2.grep_pattern.is_empty() || app.v2.grep_mode != scour_proto::GrepMode::Text {
            parts.push(Span::styled(
                pat_shown,
                Style::new()
                    .fg(if app.v2.grep_pattern.is_empty() {
                        theme.ink_3()
                    } else {
                        theme.ink()
                    })
                    .bg(theme.hover())
                    .add_modifier(if app.v2.grep_pattern.is_empty() {
                        Modifier::ITALIC
                    } else {
                        Modifier::BOLD
                    }),
            ));
        } else if let Some(laid) = grep_laid(app) {
            parts.extend(grepcap::spans(&laid, theme));
        }
        let flags = format!(
            " · {mode_lbl}{}{}{} · F8/Esc ]",
            if app.v2.grep_exact { " Aa" } else { "" },
            if app.v2.grep_binary { " 01" } else { "" },
            if app.v2.grep_markup { " <>" } else { "" },
        );
        parts.push(Span::styled(
            flags,
            Style::new().fg(theme.ink_3()).bg(theme.hover()),
        ));
        let into = match grep_laid(app) {
            Some(laid) => laid.caret,
            None if app.v2.grep_pattern.is_empty() => 0,
            None => app.grep_caret(),
        };
        debug_assert_eq!(pre_cols + 6, grep_origin(app, field.width));
        grep_caret_x = Some((pre_cols + 6 + into) as u16);
    }
    let line = Line::from(parts);
    f.render_widget(Paragraph::new(line), area);
    // The pill floats on the row above, over its capsule's right end.
    if let Some(laid) = grep_laid(app)
        && area.y > field.y
    {
        let row = Rect {
            y: area.y - 1,
            ..area
        };
        grepcap::pill(f, row, grep_origin(app, field.width), app, &laid, theme);
    }
    if triggers_shown {
        let triggers = Line::from(vec![
            Span::styled(
                "⛁ ^P  ",
                Style::new().fg(theme.ink_3()).add_modifier(Modifier::DIM),
            ),
            Span::styled(
                "≈ ^F  ",
                Style::new().fg(theme.ink_3()).add_modifier(Modifier::DIM),
            ),
            Span::styled(
                "≣ ^G ",
                Style::new().fg(theme.ink_3()).add_modifier(Modifier::DIM),
            ),
        ]);
        f.render_widget(Paragraph::new(triggers).right_aligned(), area);
    }
    // The caret is the terminal's own, which means it blinks the way every
    // other caret on that screen blinks — and costs nothing to keep alive.
    if app.mode == Mode::Search {
        if let Some(gx) = grep_caret_x {
            f.set_cursor_position((area.x + gx.min(area.width.saturating_sub(1)), area.y));
        } else {
            let before = placecap::caret_column(&app.query, app.caret, &folds) as u16;
            f.set_cursor_position((area.x + 7 + before, area.y));
        }
    }
}

/// The query's runs drawn folded, on a line this wide.
fn folds(app: &App, width: u16) -> Vec<Folded> {
    let triggers_shown = !app.v2.grep_open && width >= 96;
    let room = (width as usize).saturating_sub(7 + if triggers_shown { 18 } else { 0 });
    let mut folds = placecap::folded(app, room);
    folds.extend(valuecap::folded(app, room));
    folds.sort_by_key(|f| f.start);
    folds
}

/// The column the content field's pattern starts at, on a line this wide:
/// after the brand, the query as drawn, the filter and the field's `[ ≣ `.
/// The drawing and the mouse both count from it.
pub fn grep_origin(app: &App, width: u16) -> usize {
    let typed = if app.query.is_empty() {
        hint(app).chars().count()
    } else {
        placecap::caret_column(&app.query, app.query.len(), &folds(app, width))
    };
    let chip = app.filter.as_ref().map_or(0, |t| 5 + t.chars().count());
    7 + typed + chip + 6
}

/// The content field's pattern laid out in capsules: while it is open, has
/// something in it, and is read as text (a regex or hex pattern is not).
pub fn grep_laid(app: &App) -> Option<Laid> {
    let open = app.v2.grep_open
        && !app.v2.grep_pattern.is_empty()
        && app.v2.grep_mode == scour_proto::GrepMode::Text;
    open.then(|| {
        grepcap::lay(&app.v2.grep_pattern, app.grep_caret(), |m| {
            app.say(m).into_owned()
        })
    })
}

/// `runs` of the query as spans, each folded run as its label in the place
/// of its bytes.
fn with_folds(
    query: &str,
    runs: Vec<(usize, usize, Style)>,
    folds: &[Folded],
    theme: &Theme,
) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for (from, to, st) in runs {
        let mut at = from;
        while at < to {
            match folds.iter().find(|f| f.start <= at && at < f.end) {
                Some(f) => {
                    let cap = Style::new().bg(theme.hover()).add_modifier(Modifier::BOLD);
                    if at == f.start && f.place {
                        let disk = f.kind.map_or(theme.key(), |k| theme.disk(k));
                        out.push(Span::styled("⛁", cap.fg(disk)));
                        let ink = if f.not { theme.not() } else { theme.val() };
                        out.push(Span::styled(format!(" {}", f.label), cap.fg(ink)));
                    } else if at == f.start {
                        let ink = if f.not { theme.not() } else { theme.val() };
                        out.push(Span::styled(f.label.clone(), cap.fg(ink)));
                    }
                    at = f.end.min(to);
                }
                None => {
                    let stop = folds
                        .iter()
                        .map(|f| f.start)
                        .filter(|&s| s > at && s < to)
                        .min()
                        .unwrap_or(to);
                    out.push(Span::styled(query[at..stop].to_string(), st));
                    at = stop;
                }
            }
        }
    }
    out
}

/// Shown, total, what it cost.
pub(super) fn counts(f: &mut Frame, area: Rect, app: &App, theme: &Theme, mark: (char, char)) {
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
    if app.v2.grep_open && !app.v2.grep_summary.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " ≣ ",
                    Style::new().fg(theme.key()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(app.v2.grep_summary.clone(), Style::new().fg(theme.ink_2())),
            ])),
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
    // A query that reads files: how far, and the keys that move it.
    if let Some(reading) = app.reading {
        use scour_core::ReadingStage::{Done, Paused, Running};
        let said = app
            .say(scour_ui::reading_said(
                reading.stage == Running,
                reading.stage == Done,
            ))
            .replace("{n}", &format::grouped(reading.files, mark.0));
        let keys = match reading.stage {
            Running => String::new(),
            Paused => format!(
                "  F6 {} · F7 {}",
                app.say(scour_ui::READ_ON),
                app.say(scour_ui::READ_AGAIN)
            ),
            Done => format!("  F7 {}", app.say(scour_ui::READ_AGAIN)),
        };
        parts.push(Span::styled("  ·  ", dim));
        if reading.stage == Running {
            const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            parts.push(Span::styled(
                format!("{} ", FRAMES[app.turn % FRAMES.len()]),
                Style::new().fg(theme.key()),
            ));
        }
        parts.push(Span::styled(
            said,
            if reading.stage == Running {
                Style::new().fg(theme.key())
            } else {
                dim
            },
        ));
        parts.push(Span::styled(keys, dim));
    }
    // What the index is doing, beside what the search found: switching a skip
    // rule off starts a walk, and nothing else on this line moves while it runs.
    if app.unwritten > 0 {
        parts.push(Span::styled("  ·  ", dim));
        parts.push(Span::styled(
            app.say(scour_ui::UNWRITTEN).into_owned(),
            Style::new().fg(theme.key()),
        ));
    } else if let Some(walked) = app.scanning {
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
    // A disk that came, asked about until somebody answers: the key opens
    // the panel that holds the answers.
    if let Some(question) = app.disk_question() {
        parts.push(Span::styled("  ·  ", dim));
        parts.push(Span::styled(
            format!("{question} ^D"),
            Style::new().fg(theme.key()),
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
