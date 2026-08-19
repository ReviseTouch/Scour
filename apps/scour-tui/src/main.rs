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

    let mut state = App {
        query: args.query.clone(),
        caret: args.query.len(),
        ..App::default()
    };

    if let Some(size) = args.once.clone() {
        let outcome = snap(&size, &mut state, &link, &waiting, &theme, mark);
        link.send(Ask::Done);
        return outcome;
    }

    let mut terminal = ratatui::init();
    let outcome = run(&mut terminal, &mut state, &link, &waiting, &theme, mark);
    ratatui::restore();
    link.send(Ask::Done);
    outcome
}

/// One frame, into a buffer of a given size, printed as text.
fn snap(
    size: &str,
    state: &mut App,
    link: &Link,
    waiting: &Receiver<Beat>,
    theme: &Theme,
    mark: (char, char),
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
            Ok(_) => {}
            Err(_) => break,
        }
    }
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
                let want = keys::mouse(state, m);
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

/// Do what a step asked for.
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
        } => link.send(Ask::Search {
            generation,
            query,
            sort,
            descending,
            offset,
            limit,
            cap,
        }),
    }
}
