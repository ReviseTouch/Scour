//! Scour in a terminal.
//!
//! One of the four faces: no index, no walk, the same service over the same
//! socket. Nothing is drawn on a timer — the loop blocks on one channel carrying
//! the keyboard and the service, and the caret is the terminal's own.

mod app;
mod draw;
mod icons;
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
    #[arg(long, value_name = "WxH")]
    once: Option<String>,
    /// Press these before drawing: `--press "down,down,space,f1"`.
    #[arg(long, value_name = "KEYS")]
    press: Option<String>,
    /// Click here before drawing: `--click "5:6,110:21"` — column:row, from
    /// the top left.
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
    // What was chosen, then what was configured, then the desktop. Read here
    // rather than over the socket so the first frame is in the right language.
    let kept = scour_settings::Settings::load(&config.state_dir());
    let catalogue = scour_i18n::Catalogue::for_language(&scour_i18n::choose(
        &kept.language,
        &config.ui.language,
    ));
    let theme = Theme::read(std::env::var("SCOUR_TUI_SCHEME").as_deref() != Ok("light"));

    let (beats, waiting) = channel::<Beat>();
    let (link, answers) = Link::start(addr);
    pump(&beats, answers);
    // Where this desktop keeps things. Asked once: it does not change.
    link.later(Ask::Places);
    // And from here on, whenever the index moves.
    link.doze(Ask::Await { since: 0 });

    let mut state = App {
        query: args.query.clone(),
        caret: args.query.len(),
        words: catalogue,
        ..App::default()
    };
    // The table's shape, for the same reason as the language: the first frame
    // is the one somebody left, not a default that lasts an instant.
    state.columns_from(&kept.columns);

    if let Some(size) = args.once.clone() {
        let outcome = snap(
            &size,
            &mut state,
            &link,
            &waiting,
            &theme,
            args.press.as_deref().unwrap_or_default(),
            args.click.as_deref().unwrap_or_default(),
        );
        link.send(Ask::Done);
        return outcome;
    }

    let mut terminal = ratatui::init();
    // Before anything is drawn: can the terminal put a glyph in one column?
    icons::measure();
    // `init` does not turn the mouse on; without this the terminal sends no
    // press at all. It costs the terminal's own drag-to-select, which is why it
    // is switched off again on the way out. `Shift` still selects.
    let mousing = ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::event::EnableMouseCapture,
        // Pasted text arrives as one event, not a search per character.
        ratatui::crossterm::event::EnableBracketedPaste
    )
    .is_ok();
    let outcome = run(&mut terminal, &mut state, &link, &waiting, &theme);
    if mousing {
        let _ = ratatui::crossterm::execute!(
            std::io::stdout(),
            ratatui::crossterm::event::DisableMouseCapture,
            ratatui::crossterm::event::DisableBracketedPaste
        );
    }
    ratatui::restore();
    // Let the slow lane write what was queued on it — see `Link::finish`.
    link.finish();
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
    press: &str,
    click: &str,
) -> Result<()> {
    let (w, h) = size.split_once('x').unwrap_or(("120", "30"));
    let (w, h) = (w.parse().unwrap_or(120), h.parse().unwrap_or(30));
    // The probe cannot run against a buffer, so only `SCOUR_TUI_ICONS=on`
    // turns icons on for a picture.
    icons::measure();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h))?;
    act(state.resized(draw::room(h)), link);
    act(state.typed(), link);
    // Wait for what the first frame needs, but never for ever: a service that
    // is not running has to produce a picture too.
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
            // Taken, not dropped: the rail's counts arrive on the other lane
            // and usually first, and a picture without them is an empty rail.
            Ok(Beat::Reply(Got::Facets {
                generation,
                age,
                reply,
            })) => {
                state.counted(generation, age, *reply);
            }
            Ok(Beat::Reply(Got::Places(places, mounts))) => {
                state.places = places;
                state.mounts = mounts;
            }
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
        // Everything waiting, not one answer: pairing a key with the next reply
        // drifts by one the moment a key asks for nothing.
        settle(state, link, waiting, 400);
    }
    // The rail and strip must arrive before a click on them lands on the list.
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
    // A last, longer wait for whatever the final key set going: sorting by size
    // walks the whole index. Only when something was pressed — a plain `--once`
    // must not pay 1.5 s of waiting for nothing.
    if !press.is_empty() || !click.is_empty() {
        settle(state, link, waiting, 1_500);
    }
    terminal.draw(|f| draw::frame(f, state, theme))?;
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
            // The timeout is only so that a terminal that goes away is noticed.
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
) -> Result<()> {
    // The size says how many rows to ask for, so the first question goes out
    // after the first frame, not before.
    let size = terminal.size()?;
    act(state.resized(draw::room(size.height)), link);
    act(state.typed(), link);
    terminal.draw(|f| draw::frame(f, state, theme))?;

    // A keystroke waits for the ones after it: a term with a slash in it scans
    // every path in the index, 1.7 s measured, and typing queues one per letter.
    let quiet = Duration::from_millis(120);
    let mut pending: Option<(std::time::Instant, Want)> = None;
    // Which query has already been sent: a page of the one on screen does not
    // wait out the quiet.
    let mut asked = state.generation;
    loop {
        let beat = match &pending {
            Some((due, _)) => {
                let left = due.saturating_duration_since(std::time::Instant::now());
                match waiting.recv_timeout(left) {
                    Ok(beat) => beat,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if let Some((_, want)) = pending.take() {
                            act(want, link);
                        }
                        continue;
                    }
                    Err(_) => break,
                }
            }
            None => match waiting.recv() {
                Ok(beat) => beat,
                Err(_) => break,
            },
        };
        match beat {
            Beat::Shut => break,
            Beat::Key(Event::Key(k)) => {
                let want = keys::press(state, k);
                // A page of the same query goes at once; a *new* query waits
                // to see whether another letter is coming.
                match want {
                    Want::Page { .. } if state.generation != asked => {
                        asked = state.generation;
                        pending = Some((std::time::Instant::now() + quiet, want));
                    }
                    other => act(other, link),
                }
            }
            Beat::Key(Event::Resize(_, h)) => {
                act(state.resized(draw::room(h)), link);
            }
            Beat::Key(Event::Paste(text)) => {
                let want = keys::pasted(state, &text);
                asked = state.generation;
                pending = Some((std::time::Instant::now() + quiet, want));
            }
            Beat::Key(Event::Mouse(m)) => {
                // Where a press landed is the only thing that says what it meant.
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
            Beat::Reply(Got::Facets {
                generation,
                age,
                reply,
            }) => {
                state.counted(generation, age, *reply);
            }
            Beat::Reply(Got::Places(places, mounts)) => {
                state.mounts = mounts;
                state.places = places;
                state.dirty = true;
            }
            Beat::Reply(Got::Stats(stats)) => {
                state.stats = Some(*stats);
                state.dirty = true;
            }
            Beat::Reply(Got::Usage(usage)) => {
                state.usage = Some(*usage);
                state.dirty = true;
            }
            Beat::Reply(Got::Peek(look)) => {
                state.peek = Some(*look);
                state.dirty = true;
            }
            Beat::Reply(Got::Dupes { groups, waste }) => {
                state.dupes = groups;
                state.waste = waste;
                state.dirty = true;
            }
            Beat::Reply(Got::Rules {
                added,
                config,
                builtin,
                off,
            }) => state.ruled(added, config, builtin, off),
            Beat::Reply(Got::Writing(bytes)) => {
                state.note = format!(
                    "{} {}",
                    state.say("writing…"),
                    scour_ui::format::compact_bytes(bytes, state.mark().1)
                );
                state.dirty = true;
            }
            Beat::Reply(Got::Wrote(path)) => {
                state.note = format!("{} {path}", state.say("written to"));
                state.dirty = true;
            }
            Beat::Reply(Got::Failed(why)) => {
                state.note = why;
                state.dirty = true;
            }
            Beat::Reply(Got::Counted { generation, total }) => {
                state.counted_exactly(generation, total);
            }
            Beat::Reply(Got::Explained { generation, spans }) => {
                state.explained(generation, spans);
            }
            Beat::Reply(Got::Awake(revision, walked, stale)) => {
                state.scanning = walked;
                state.rebuild_advised = stale;
                let want = state.awake(revision);
                act(want, link);
                // A beat before waiting again: during a scan the index moves
                // several times a second.
                std::thread::sleep(Duration::from_millis(250));
                link.doze(Ask::Await {
                    since: state.revision,
                });
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
            terminal.draw(|f| draw::frame(f, state, theme))?;
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
            Beat::Reply(Got::Facets {
                generation,
                age,
                reply,
            }) => {
                state.counted(generation, age, *reply);
            }
            Beat::Reply(Got::Places(places, mounts)) => {
                state.places = places;
                state.mounts = mounts;
            }
            Beat::Reply(Got::Stats(stats)) => state.stats = Some(*stats),
            Beat::Reply(Got::Usage(usage)) => state.usage = Some(*usage),
            Beat::Reply(Got::Peek(look)) => state.peek = Some(*look),
            Beat::Reply(Got::Dupes { groups, waste }) => {
                state.dupes = groups;
                state.waste = waste;
            }
            Beat::Reply(Got::Explained { generation, spans }) => {
                state.explained(generation, spans);
            }
            Beat::Reply(Got::Rules {
                added,
                config,
                builtin,
                off,
            }) => state.ruled(added, config, builtin, off),
            Beat::Reply(Got::Writing(_)) => {}
            Beat::Reply(Got::Wrote(path)) => {
                state.note = format!("{} {path}", state.say("written to"))
            }
            Beat::Reply(Got::Failed(why)) => state.note = why,
            Beat::Reply(Got::Counted { generation, total }) => {
                state.counted_exactly(generation, total);
            }
            Beat::Reply(Got::Awake(revision, walked, stale)) => {
                state.scanning = walked;
                state.rebuild_advised = stale;
                let want = state.awake(revision);
                act(want, link);
            }
            _ => {}
        }
    }
}

/// Where the report opens. See `App::report`.
fn state_home() -> String {
    std::env::var("HOME").unwrap_or_default()
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
        "insert" => KeyCode::Insert,
        "v" if mods.contains(KeyModifiers::CONTROL) => KeyCode::Char('v'),
        "enter" => KeyCode::Enter,
        "backspace" => KeyCode::Backspace,
        "esc" => KeyCode::Esc,
        "f1" => KeyCode::F(1),
        "f2" => KeyCode::F(2),
        "f3" => KeyCode::F(3),
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        other => KeyCode::Char(other.chars().next().unwrap_or(' ')),
    };
    KeyEvent::new(code, mods)
}

/// Do what a step asked for. The rail's counts go out beside the first page of
/// a query, on the slow lane: they walk the matching set.
fn act(want: Want, link: &Link) {
    match want {
        Want::Nothing | Want::Leave => {}
        Want::Page {
            generation,
            query,
            counting,
            over,
            sort,
            descending,
            offset,
            limit,
            cap,
        } => {
            // Two questions over different rows: the kinds are the result on
            // screen, the strip is the query without its age term.
            if offset == 0 {
                link.later(Ask::Facets {
                    generation,
                    query: counting,
                    age: false,
                });
                link.later(Ask::Facets {
                    generation,
                    query: over,
                    age: true,
                });
                // And what it comes to exactly: the interactive count stops at
                // a thousand, which makes any large filter look like it did
                // nothing.
                link.later(Ask::Count {
                    generation,
                    query: query.clone(),
                });
                // What the query is, for colour: the parser answers, not the index.
                link.later(Ask::Explain {
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
        // On the slow lane: a keystroke must not queue behind a stat of paths.
        Want::Recheck(paths) => link.later(Ask::Recheck(paths)),
        Want::Report => {
            link.later(Ask::Stats);
            link.later(Ask::Dupes);
            link.later(Ask::Usage { path: state_home() });
        }
        Want::Weigh(path) => link.later(Ask::Usage { path }),
        Want::Peek(path) => link.later(Ask::Preview { path }),
        Want::OffRules(off) => link.later(Ask::OffRules(off)),
        Want::Remember(change) => link.later(Ask::Remember(change)),
        Want::Export { query, to } => link.later(Ask::Export { query, to }),
    }
}
