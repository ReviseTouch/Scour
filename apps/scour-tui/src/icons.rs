//! The window's icons, in a terminal — when the terminal can draw them.
//!
//! **A cell is one column or two and only the terminal knows which.** A glyph
//! the font does not have, or one it draws double-width, moves every character
//! after it: the columns stop lining up, and every press lands one place out.
//! Which is why this asks rather than assumes.
//!
//! The question is asked of the terminal itself, once, at startup: put the
//! cursor at a known column, print the glyph, and ask where the cursor is now.
//! One means it fits, anything else means it does not.
//!
//! `SCOUR_TUI_ICONS=off` and `=on` answer it instead, for a terminal that lies
//! or a font somebody is about to install.

use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::crossterm::{cursor, execute, style, terminal};

static DRAWING: AtomicBool = AtomicBool::new(false);

/// Ask the terminal whether one of these glyphs takes a single column.
///
/// Called after the alternate screen is up and before the first frame: what it
/// prints is on a line the first draw overwrites.
pub fn measure() {
    let forced = std::env::var("SCOUR_TUI_ICONS").ok();
    match forced.as_deref() {
        Some("off") => return,
        Some("on") => {
            DRAWING.store(true, Ordering::Relaxed);
            return;
        }
        _ => {}
    }
    let mut out = std::io::stdout();
    let measured = (|| -> std::io::Result<u16> {
        // **Printed through the same channel as the moves**, or the glyph
        // sits in one buffer while the question about it goes out of another
        // and the answer is about a cursor that has not moved yet. It read
        // zero columns until this was changed.
        execute!(
            out,
            cursor::SavePosition,
            cursor::MoveTo(0, 0),
            // The one that is drawn most: a folder. A font with any of these
            // has this one.
            style::Print(of_kind_always("folder"))
        )?;
        let (col, _) = cursor::position()?;
        execute!(
            out,
            cursor::MoveTo(0, 0),
            terminal::Clear(terminal::ClearType::CurrentLine),
            cursor::RestorePosition
        )?;
        Ok(col)
    })()
    // A terminal that will not answer is a terminal this cannot ask, and the
    // safe reading of no answer is "do not draw them".
    .unwrap_or(0);
    let fits = measured == 1;
    crate::app::trace(&format!(
        "icons: the glyph measured {measured} column(s) — {}",
        if fits { "on" } else { "off" }
    ));
    DRAWING.store(fits, Ordering::Relaxed);
}

/// Whether icons are being drawn.
pub fn drawing() -> bool {
    DRAWING.load(Ordering::Relaxed)
}

/// The glyph for a kind whether or not icons are on — what the probe prints.
fn of_kind_always(token: &str) -> &'static str {
    match token {
        "folder" => "\u{f07b}",
        "code" => "\u{f121}",
        "doc" => "\u{f15c}",
        "image" => "\u{f03e}",
        "video" | "media" => "\u{f03d}",
        "audio" => "\u{f001}",
        "archive" => "\u{f1c6}",
        "exec" => "\u{f085}",
        "data" => "\u{f1c0}",
        "config" => "\u{f013}",
        "font" => "\u{f031}",
        "build" => "\u{f0ad}",
        _ => "\u{f15b}",
    }
}

/// The glyph for a kind, or nothing when icons are off.
///
/// The same fourteen kinds the window draws, in the same order of preference.
/// Nerd Font code points, which is what a terminal font that has icons at all
/// has.
pub fn of_kind(token: &str) -> &'static str {
    if drawing() { of_kind_always(token) } else { "" }
}

/// The glyph for one of the tools on the counter line.
pub fn of_tool(at: usize) -> &'static str {
    if !drawing() {
        return "";
    }
    match at {
        0 => "\u{f0ec}",
        1 => "\u{f1ab}",
        2 => "\u{f05e}",
        3 => "\u{f019}",
        _ => "\u{f11c}",
    }
}
