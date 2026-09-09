//! The window's icons, in a terminal — when the terminal can draw them.
//!
//! A glyph the font lacks or draws double-width shifts every column after it,
//! so the terminal is asked once at startup: print the glyph, read the cursor
//! column, one means it fits. `SCOUR_TUI_ICONS=off`/`=on` answer it instead.

use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::crossterm::{cursor, execute, style, terminal};

static DRAWING: AtomicBool = AtomicBool::new(false);

/// Ask the terminal whether one of these glyphs takes a single column. Runs
/// after the alternate screen is up: the first frame overwrites what it prints.
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
        // Printed through the same channel as the cursor moves, or the answer
        // is about a cursor that has not seen the glyph yet.
        execute!(
            out,
            cursor::SavePosition,
            cursor::MoveTo(0, 0),
            // The most drawn glyph, and the one a font with any of these has.
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
    // No answer reads as "do not draw them".
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

/// The glyph for a kind, or nothing when icons are off. Nerd Font code points,
/// for the same kinds the window draws.
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
