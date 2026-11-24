//! A panel over the list: its lines, its box, and what fills it.

use super::*;
use crate::app::{App, Panel, Spot};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

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

/// How wide a panel is on a screen this wide.
fn panel_width(area: Rect) -> u16 {
    66u16.min(area.width.saturating_sub(4))
}

/// A sentence over as many lines as it takes. A panel line wider than the box
/// is silently cut, which is fine for a fact and wrong for an instruction.
pub(super) fn wrapped(text: &str, wide: usize, dimmed: bool) -> Vec<PanelLine> {
    let wide = wide.max(8);
    let mut out: Vec<PanelLine> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        // A word longer than the box — a path — is cut rather than dropped.
        for piece in word
            .chars()
            .collect::<Vec<_>>()
            .chunks(wide)
            .map(|c| c.iter().collect::<String>())
        {
            let room = line.is_empty() || line.chars().count() + 1 + piece.chars().count() <= wide;
            if !room {
                out.push((std::mem::take(&mut line), dimmed).into());
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&piece);
        }
    }
    if !line.is_empty() || out.is_empty() {
        out.push((line, dimmed).into());
    }
    out
}

/// Where a panel of this many lines is drawn. One function, two callers — this
/// and the mouse — because a panel hit-tested elsewhere drifts from where it is.
pub fn panel_rect(area: Rect, lines: usize) -> Rect {
    let wide = panel_width(area);
    let tall = (lines as u16 + 3).min(area.height.saturating_sub(2));
    Rect {
        x: area.x + area.width.saturating_sub(wide) / 2,
        y: area.y + 2,
        width: wide,
        height: tall,
    }
}

/// What a panel says: its title and its lines. The settings screen draws the
/// same lines beside its sections; the panel draws them in a box.
pub(super) fn panel_content(
    app: &App,
    wrap: usize,
) -> Option<(std::borrow::Cow<'_, str>, Vec<PanelLine>)> {
    Some(match app.panel {
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
        Panel::Disks => {
            let (title, lines) = app.disk_panel();
            (
                title.into(),
                lines
                    .into_iter()
                    .map(|(text, dimmed, rule)| PanelLine {
                        text,
                        key: String::new(),
                        dimmed,
                        careful: false,
                        rule,
                    })
                    .collect(),
            )
        }
        // The key in the title, because it is what the panel is about; what to
        // do about it in the lines. The last line is drawn but not walked —
        // see [`App::panel_lines`].
        Panel::Key => {
            let now = || {
                app.key_bound
                    .clone()
                    .unwrap_or_else(|| app.say("not bound").into_owned())
            };
            // Nothing after the title until the desktop has answered, and
            // nothing at all where it cannot hold one: "not bound" there reads
            // as something a press would fix.
            let head = match !app.key_desktop.is_empty() && app.key_can {
                true => format!("{}  ·  {}", app.say("KEYBOARD SHORTCUT"), now()),
                false => app.say("KEYBOARD SHORTCUT").into_owned(),
            };
            let lines = if app.key_can {
                let mut lines = vec![
                    PanelLine {
                        text: format!("› {}█", app.key_typed),
                        key: app.say("type a combination and press Enter").into_owned(),
                        dimmed: false,
                        careful: false,
                        rule: false,
                    },
                    PanelLine {
                        text: app.say("Remove").into_owned(),
                        key: "x".into(),
                        dimmed: false,
                        careful: true,
                        rule: false,
                    },
                ];
                let below = if app.key_note.is_empty() {
                    format!("{}  ·  {}", app.key_desktop, app.key_command)
                } else {
                    app.key_note.clone()
                };
                for (at, mut line) in wrapped(&below, wrap, !app.key_bad).into_iter().enumerate() {
                    line.careful = app.key_bad;
                    line.rule = at == 0;
                    lines.push(line);
                }
                lines
            } else {
                let mut lines = wrapped(
                    &app.say(
                        "This desktop cannot be bound from here. Bind a key of your choice to:",
                    ),
                    wrap,
                    false,
                );
                lines.extend(wrapped(&app.key_command, wrap, true));
                lines
            };
            (std::borrow::Cow::Owned(head), lines)
        }
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
        // Drawn by `menu_panel`, which has a strip across its top.
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
            if app
                .openers
                .first()
                .is_some_and(|(id, _)| id.starts_with("send:"))
            {
                app.say("SEND TO")
            } else {
                app.say("OPEN WITH")
            },
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
        Panel::General => (
            app.say("General"),
            app.general_lines()
                .into_iter()
                .enumerate()
                .map(|(at, (text, key))| PanelLine {
                    text,
                    key,
                    dimmed: false,
                    careful: false,
                    rule: at == 3,
                })
                .collect(),
        ),
        Panel::Later => {
            let folders = app.folders.len();
            (
                app.say("Disks that come later"),
                app.later_lines()
                    .into_iter()
                    .enumerate()
                    .map(|(at, (text, key))| PanelLine {
                        text,
                        key,
                        dimmed: false,
                        careful: false,
                        rule: at == 2 || (folders == 0 && at == 2),
                    })
                    .collect(),
            )
        }
        // Two to walk, and a line to read under them.
        Panel::Searching => (
            app.say("Searching"),
            app.searching_lines()
                .into_iter()
                .map(|(text, key)| PanelLine {
                    text,
                    key,
                    dimmed: false,
                    careful: false,
                    rule: false,
                })
                .chain([(String::new(), true).into()])
                .chain(wrapped(
                    &app.say("letters:scrgui finds scour-gui: those letters in that order, with anything between. fuzzy:word forgives one word its mistakes."),
                    wrap,
                    true,
                ))
                .collect(),
        ),
        // Read, not walked: `panel_lines` is zero.
        Panel::Keys => (
            app.say("KEYS"),
            crate::keys::MAP
                .iter()
                .map(|(key, what)| (format!("{:<26}{}", app.say(key), app.say(what)), false).into())
                .collect(),
        ),
        Panel::About => (
            app.say("About"),
            vec![
                (format!("Scour {}", env!("CARGO_PKG_VERSION")), false).into(),
                (String::new(), false).into(),
                (
                    app.say("scour status says what the service holds.")
                        .into_owned(),
                    true,
                )
                    .into(),
            ],
        ),
        Panel::Lens => {
            let (title, lines) = app.lens_panel_lines();
            (title.into(), lines)
        }
        Panel::None => return None,
    })
}

/// Whatever panel is open, over the middle of the screen. One drawing for all
/// of them: a title, a list, and a cursor on one line of it.
pub(super) fn panel(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if app.panel == Panel::Menu {
        return menu_panel(f, area, app, theme);
    }
    // The same six the drawing takes off below: the indent, the border and the
    // gap the shortcut sits in.
    let wrap = panel_width(area).saturating_sub(6) as usize;
    let Some((title, lines)) = panel_content(app, wrap) else {
        return;
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
    // A panel with nothing to press — a sentence to read — draws no cursor: a
    // mark against a line that answers to nothing is a mark that lies.
    let pressable = app.panel_lines();
    for (at, line) in lines.iter().enumerate().skip(from).take(room) {
        let dimmed = &line.dimmed;
        let on = at == app.panel_at && at < pressable;
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
