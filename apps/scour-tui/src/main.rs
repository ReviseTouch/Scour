//! Scour in a terminal.
//!
//! The third face. It holds no index and walks no filesystem: it asks the same
//! service the window and the browser page ask, over the same socket, and
//! draws the answers.
//!
//! ## The loop
//!
//! ```text
//!                  ┌───────────── the keyboard (a thread)
//!    one channel ◄─┤
//!                  └───────────── the service (a thread)
//!         │
//!         ▼
//!    App::… → what changed → draw, if anything did
//! ```
//!
//! **Nothing is drawn on a timer.** A loop that redraws sixty times a second
//! keeps a core awake to show a list that has not moved; this one blocks on
//! the channel and draws when something arrives. That is also why the caret is
//! the terminal's own rather than a block this paints: a blinking caret drawn
//! here would be a redraw twice a second, for ever.

mod app;
mod draw;
mod keys;
mod link;
mod theme;

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use ratatui::crossterm::event::{self, Event};

use app::{App, Want};
use link::{Ask, Got, Link};
use theme::Theme;

#[derive(Parser, Debug)]
#[command(name = "scour-tui", about = "Scour in a terminal", version)]
struct Args {
    /// Talk to a service listening here.
    #[arg(long)]
    socket: Option<String>,
    /// Start with this query.
    #[arg(short, long, default_value = "")]
    query: String,
    /// Draw one frame into a buffer of this size and print it as text, then
    /// leave: `--once 120x30`.
    ///
    /// **The terminal's own screen cannot be photographed** — it is the
    /// alternate screen and it is gone the moment this exits — so this is what
    /// stands in for `SCOUR_GUI_SNAP`. It is also the only way to check what
    /// the interface says without a person reading it.
    #[arg(long, value_name = "WxH")]
    once: Option<String>,
    /// Press these before drawing: `--press "down,down,space,f1"`.
    ///
    /// The keyboard cannot be reached from a test and the alternate screen
    /// cannot be photographed, so this is how a picture of anything other than
    /// the first frame is taken.
    #[arg(long, value_name = "KEYS")]
    press: Option<String>,
    /// Click here before drawing: `--click "5:6,110:21"` — column:row, from
    /// the top left. The only way to check that a press lands where it looks
    /// like it lands.
    #[arg(long, value_name = "COL:ROW")]
    click: Option<String>,
}

/// What the loop waits on: one of these, from either thread.
enum Beat {
    Key(Event),
    Reply(Got),
    /// The keyboard thread ended, which means the terminal did.
    Shut,
}

fn main() -> Result<()> {
    let args = Args::parse();
    // One read serves both the language and the socket.
    let config = scour_config::Config::load_or_default().0;
    let addr = args.socket.clone().unwrap_or_else(|| config.socket());
    let catalogue =
        scour_i18n::Catalogue::for_language(&scour_i18n::choose("", &config.ui.language));
    let mark = (
        scour_ui::format::group_mark(catalogue.language()),
        scour_ui::format::decimal_mark(catalogue.language()),
    );
    let theme = Theme::read(std::env::var("SCOUR_TUI_SCHEME").as_deref() != Ok("light"));

    let (beats, waiting) = channel::<Beat>();
    let (link, answers) = Link::start(addr);
    pump(&beats, answers);
    // Where this desktop keeps things. Asked once: it is a file the desktop
    // wrote, not something that changes while somebody searches.
    link.later(Ask::Places);

    let mut state = App {
        query: args.query.clone(),
        caret: args.query.len(),
        ..App::default()
    };

    if let Some(size) = args.once.clone() {
        let outcome = snap(
            &size,
            &mut state,
            &link,
            &waiting,
            &theme,
            mark,
            args.press.as_deref().unwrap_or_default(),
            args.click.as_deref().unwrap_or_default(),
        );
        link.send(Ask::Done);
        return outcome;
    }

    let mut terminal = ratatui::init();
    // **`init` does not turn the mouse on.** Without this the terminal never
    // sends a press and the handling for one may as well not be written —
    // which is exactly how it was: a wheel that did nothing and a click that
    // did nothing, with the code for both sitting there.
    //
    // What it costs is the terminal's own drag-to-select, which is why it is
    // switched off again on the way out rather than left on for whatever runs
    // next in that window. `Shift` still selects in every terminal worth
    // using.
    let mousing = ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::event::EnableMouseCapture
    )
    .is_ok();
    let outcome = run(&mut terminal, &mut state, &link, &waiting, &theme, mark);
    if mousing {
        let _ = ratatui::crossterm::execute!(
            std::io::stdout(),
            ratatui::crossterm::event::DisableMouseCapture
        );
    }
    ratatui::restore();
    link.send(Ask::Done);
    outcome
}

/// One frame, into a buffer of a given size, printed as text.
#[allow(clippy::too_many_arguments)]
fn snap(
    size: &str,
    state: &mut App,
    link: &Link,
    waiting: &Receiver<Beat>,
    theme: &Theme,
    mark: (char, char),
    press: &str,
    click: &str,
) -> Result<()> {
    let (w, h) = size.split_once('x').unwrap_or(("120", "30"));
    let (w, h) = (w.parse().unwrap_or(120), h.parse().unwrap_or(30));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h))?;
    act(state.resized(draw::room(h)), link);
    act(state.typed(), link);
    // Wait for what the first frame needs, but never for ever: a service that
    // is not running has to produce a picture too, saying so.
    let until = std::time::Instant::now() + Duration::from_millis(2_000);
    while std::time::Instant::now() < until {
        let left = until - std::time::Instant::now();
        match waiting.recv_timeout(left) {
            Ok(Beat::Reply(Got::Search {
                generation,
                offset,
                limit,
                reply,
            })) => {
                act(state.landed(generation, offset, limit, *reply), link);
                break;
            }
            Ok(Beat::Reply(Got::Trouble { generation, why })) => {
                act(state.upset(generation, why), link);
                break;
            }
            // **Taken, not dropped.** The rail's counts and the desktop's
            // folders arrive on the other lane and usually first; a wait that
            // threw them away photographed an empty rail every time.
            Ok(Beat::Reply(Got::Facets { generation, reply })) => {
                state.counted(generation, *reply);
            }
            Ok(Beat::Reply(Got::Places(places))) => state.places = places,
            Ok(Beat::Reply(Got::Rules {
                added,
                config,
                builtin,
                off,
            })) => state.ruled(added, config, builtin, off),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    for name in press.split(',').filter(|n| !n.trim().is_empty()) {
        act(keys::press(state, named(name.trim())), link);
        // **Everything waiting, not one answer.** Pairing a key with the next
        // reply on the channel drifts by one the moment a key asks for
        // nothing: the picture then shows the answer to the key before last.
        settle(state, link, waiting, 400);
    }
    // Let the rail and the strip arrive before anything is clicked on them:
    // a click on a strip that is not there yet lands on the list instead.
    if !click.is_empty() {
        settle(state, link, waiting, 600);
    }
    for at in click.split(',').filter(|n| !n.trim().is_empty()) {
        let (col, row) = at.trim().split_once(':').unwrap_or(("0", "0"));
        let press = ratatui::crossterm::event::MouseEvent {
            kind: ratatui::crossterm::event::MouseEventKind::Down(
                ratatui::crossterm::event::MouseButton::Left,
            ),
            column: col.parse().unwrap_or(0),
            row: row.parse().unwrap_or(0),
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        };
        act(keys::mouse(state, press, (w, h)), link);
        let release = ratatui::crossterm::event::MouseEvent {
            kind: ratatui::crossterm::event::MouseEventKind::Up(
                ratatui::crossterm::event::MouseButton::Left,
            ),
            ..press
        };
        act(keys::mouse(state, release, (w, h)), link);
        settle(state, link, waiting, 400);
    }
    // And a last wait, longer, for whatever the final key set going: sorting
    // by size walks the whole index and takes tens of milliseconds.
    settle(state, link, waiting, 1_500);
    terminal.draw(|f| draw::frame(f, state, theme, mark))?;
    for line in terminal.backend().buffer().content.chunks(w as usize) {
        let text: String = line.iter().map(|c| c.symbol()).collect();
        println!("{}", text.trim_end());
    }
    Ok(())
}

/// The service's answers, and the keyboard, on to one channel.
fn pump(beats: &Sender<Beat>, answers: Receiver<Got>) {
    let to = beats.clone();
    std::thread::spawn(move || {
        while let Ok(got) = answers.recv() {
            if to.send(Beat::Reply(got)).is_err() {
                return;
            }
        }
    });
    let to = beats.clone();
    std::thread::spawn(move || {
        loop {
            // Blocking, so this thread sleeps until a key is pressed. The
            // timeout is only so that a terminal that goes away is noticed.
            match event::poll(Duration::from_millis(500)) {
                Ok(true) => match event::read() {
                    Ok(e) => {
                        if to.send(Beat::Key(e)).is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = to.send(Beat::Shut);
                        return;
                    }
                },
                Ok(false) => {}
                Err(_) => {
                    let _ = to.send(Beat::Shut);
                    return;
                }
            }
        }
    });
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    state: &mut App,
    link: &Link,
    waiting: &Receiver<Beat>,
    theme: &Theme,
    mark: (char, char),
) -> Result<()> {
    // The first frame sizes the list, and the size is what says how many rows
    // to ask for — so the first question goes out after it, not before.
    let size = terminal.size()?;
    act(state.resized(draw::room(size.height)), link);
    act(state.typed(), link);
    terminal.draw(|f| draw::frame(f, state, theme, mark))?;

    while let Ok(beat) = waiting.recv() {
        match beat {
            Beat::Shut => break,
            Beat::Key(Event::Key(k)) => {
                let want = keys::press(state, k);
                act(want, link);
            }
            Beat::Key(Event::Resize(_, h)) => {
                act(state.resized(draw::room(h)), link);
            }
            Beat::Key(Event::Mouse(m)) => {
                // The terminal's size, because where a press landed is the
                // only thing that says what it meant.
                let size = terminal
                    .size()
                    .map(|s| (s.width, s.height))
                    .unwrap_or((80, 24));
                let want = keys::mouse(state, m, size);
                act(want, link);
            }
            Beat::Key(_) => {}
            Beat::Reply(Got::Search {
                generation,
                offset,
                limit,
                reply,
            }) => {
                let want = state.landed(generation, offset, limit, *reply);
                act(want, link);
            }
            Beat::Reply(Got::Facets { generation, reply }) => {
                state.counted(generation, *reply);
            }
            Beat::Reply(Got::Places(places)) => {
                state.places = places;
                state.dirty = true;
            }
            Beat::Reply(Got::Rules {
                added,
                config,
                builtin,
                off,
            }) => state.ruled(added, config, builtin, off),
            Beat::Reply(Got::Wrote(path)) => {
                state.note = format!("written to {path}");
                state.dirty = true;
            }
            Beat::Reply(Got::Failed(why)) => {
                state.note = why;
                state.dirty = true;
            }
            Beat::Reply(Got::Trouble { generation, why }) => {
                act(state.upset(generation, why), link);
            }
        }
        if state.leaving {
            break;
        }
        if state.dirty {
            state.dirty = false;
            terminal.draw(|f| draw::frame(f, state, theme, mark))?;
        }
    }
    Ok(())
}

/// Take everything the service has said, until it says nothing for `quiet`
/// milliseconds.
fn settle(state: &mut App, link: &Link, waiting: &Receiver<Beat>, quiet: u64) {
    while let Ok(beat) = waiting.recv_timeout(Duration::from_millis(quiet)) {
        match beat {
            Beat::Reply(Got::Search {
                generation,
                offset,
                limit,
                reply,
            }) => act(state.landed(generation, offset, limit, *reply), link),
            Beat::Reply(Got::Trouble { generation, why }) => {
                act(state.upset(generation, why), link);
            }
            Beat::Reply(Got::Facets { generation, reply }) => {
                state.counted(generation, *reply);
            }
            Beat::Reply(Got::Places(places)) => state.places = places,
            Beat::Reply(Got::Rules {
                added,
                config,
                builtin,
                off,
            }) => state.ruled(added, config, builtin, off),
            Beat::Reply(Got::Wrote(path)) => state.note = format!("written to {path}"),
            Beat::Reply(Got::Failed(why)) => state.note = why,
            _ => {}
        }
    }
}

/// A key by name, for `--press`.
fn named(name: &str) -> ratatui::crossterm::event::KeyEvent {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (mods, name) = match name.strip_prefix("shift+") {
        Some(rest) => (KeyModifiers::SHIFT, rest),
        None => match name.strip_prefix("ctrl+") {
            Some(rest) => (KeyModifiers::CONTROL, rest),
            None => (KeyModifiers::NONE, name),
        },
    };
    let code = match name {
        "down" => KeyCode::Down,
        "up" => KeyCode::Up,
        "pgdn" => KeyCode::PageDown,
        "pgup" => KeyCode::PageUp,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "space" => KeyCode::Char(' '),
        "tab" => KeyCode::Tab,
        "enter" => KeyCode::Enter,
        "backspace" => KeyCode::Backspace,
        "esc" => KeyCode::Esc,
        "f1" => KeyCode::F(1),
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        other => KeyCode::Char(other.chars().next().unwrap_or(' ')),
    };
    KeyEvent::new(code, mods)
}

/// Do what a step asked for.
///
/// The rail's counts go out beside the first page of a query and on the slow
/// lane — they walk the matching set, and a keystroke must not queue behind
/// one.
fn act(want: Want, link: &Link) {
    match want {
        Want::Nothing | Want::Leave => {}
        Want::Page {
            generation,
            query,
            sort,
            descending,
            offset,
            limit,
            cap,
        } => {
            if offset == 0 {
                link.later(Ask::Facets {
                    generation,
                    query: query.clone(),
                });
            }
            link.send(Ask::Search {
                generation,
                query,
                sort,
                descending,
                offset,
                limit,
                cap,
            });
        }
        Want::Rules => link.later(Ask::Rules),
        Want::OffRules(off) => link.later(Ask::OffRules(off)),
        Want::Remember(change) => link.later(Ask::Remember(change)),
        Want::Export { query, to } => link.later(Ask::Export { query, to }),
    }
}
