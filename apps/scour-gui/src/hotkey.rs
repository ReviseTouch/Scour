//! The desktop's key that opens Scour, as the window shows it. Every
//! `scour-hotkey` call spawns a process, so each runs on a thread of its own
//! and comes back as [`Got::Key`]; what the row says about a state is a
//! function of that state alone — [`shown`], [`read_back`], [`nudge`].

use std::cell::RefCell;
use std::rc::Rc;

use scour_core::Catalog;
use scour_hotkey::{Applies, Error, Hotkey, Key, KeyError, Status};
use scour_i18n::Catalogue;
use slint::ComponentHandle;

use crate::link::{Ask, Got, Link};
use crate::{MainWindow, t};

/// What the first-run line offers, in the neutral spelling.
pub const OFFERED: &str = "super+f";

/// A combination is a dozen characters; the cap is the field's, for the same
/// reason the rule field has one — see `rule-kept` in the interface.
const TYPED_CAP: usize = 64;

/// What a worker was asked to do.
pub enum Deed {
    Look,
    /// `nudge` is true when the first-run line asked, so a failure is shown
    /// where that line was rather than in the panel nobody has opened.
    Bind {
        key: Key,
        nudge: bool,
    },
    Clear,
}

/// What it found. Plain data: it crosses a thread.
#[derive(Debug)]
pub enum Done {
    Looked {
        status: Result<Status, Error>,
        can_bind: bool,
    },
    Bound {
        outcome: Result<Applies, Error>,
        nudge: bool,
    },
    Cleared(Result<(), Error>),
}

/// What the window knows about the key between answers.
#[derive(Default)]
struct Desk {
    /// `None` until the first worker has answered.
    status: Option<Result<Status, Error>>,
    can_bind: bool,
    /// `Settings::key_hint_seen`: some face has already offered this.
    seen: bool,
    /// A binding written on KDE, which is not in force until the next login.
    later: bool,
    /// The line under the field: a parse error, or why an action failed.
    error: String,
    /// The neutral spelling of what is typed, while it parses.
    preview: String,
    /// Why the first-run bind failed. The strip stays to say it.
    nudge_error: String,
}

thread_local! {
    /// The UI thread's own copy. Nothing else ever reads it.
    static DESK: RefCell<Desk> = RefCell::new(Desk::default());
}

/// Ask the desktop, off the UI thread; the answer arrives as [`Got::Key`].
pub fn work(deed: Deed) {
    std::thread::spawn(move || {
        let hk = Hotkey::detect();
        let done = match deed {
            Deed::Look => Done::Looked {
                status: hk.status(),
                can_bind: hk.can_bind(),
            },
            Deed::Bind { key, nudge } => Done::Bound {
                outcome: hk.bind(&key),
                nudge,
            },
            Deed::Clear => Done::Cleared(hk.clear()),
        };
        let _ = slint::invoke_from_event_loop(move || crate::deliver(Got::Key(Box::new(done))));
    });
}

/// Read the desktop once, at start-up. `seen` is what the settings say.
pub fn start(seen: bool) {
    DESK.with(|d| d.borrow_mut().seen = seen);
    work(Deed::Look);
}

/// The strings that do not depend on the state, and then the ones that do.
pub fn words(w: &MainWindow, cat: &Catalogue) {
    w.set_key_title(t(cat, "Keyboard shortcut"));
    w.set_key_copy(t(cat, "Copy"));
    w.set_key_remove(t(cat, "Remove"));
    w.set_key_hint(t(cat, "for example super+f or ctrl+alt+s"));
    w.set_key_nudge_text(t(cat, "Open Scour from anywhere: bind Super+F"));
    w.set_key_nudge_yes(t(cat, "Bind Super+F"));
    w.set_key_nudge_no(t(cat, "Not now"));
    draw(w, cat);
}

/// An answer from a worker, on the UI thread.
pub fn landed(w: &MainWindow, cat: &Catalogue, done: Done) {
    let look_again = DESK.with(|d| {
        let mut d = d.borrow_mut();
        match done {
            Done::Looked { status, can_bind } => {
                d.can_bind = can_bind;
                d.status = Some(status);
                false
            }
            Done::Bound {
                outcome: Ok(applies),
                ..
            } => {
                d.later = applies == Applies::AfterLogin;
                d.error.clear();
                d.preview.clear();
                d.nudge_error.clear();
                true
            }
            Done::Bound {
                outcome: Err(e),
                nudge,
            } => {
                let sentence = why(cat, &e);
                if nudge {
                    d.nudge_error = sentence;
                } else {
                    d.error = sentence;
                }
                false
            }
            Done::Cleared(Ok(())) => {
                d.later = false;
                d.error.clear();
                d.preview.clear();
                true
            }
            Done::Cleared(Err(e)) => {
                d.error = why(cat, &e);
                false
            }
        }
    });
    // What is in force is the desktop's answer, never what was just written.
    if look_again {
        w.set_key_typed(slint::SharedString::new());
        work(Deed::Look);
    }
    draw(w, cat);
}

/// The five presses the row and the first-run line make.
pub fn wire(w: &MainWindow, link: &Rc<Link>, cat: &Rc<RefCell<Rc<Catalogue>>>) {
    {
        let weak = w.as_weak();
        let cat = Rc::clone(cat);
        w.on_key_typed_changed(move |text| {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let (error, preview) = read_back(&cat, &text);
            DESK.with(|d| {
                let mut d = d.borrow_mut();
                d.error = error;
                d.preview = preview;
                d.nudge_error.clear();
            });
            draw(&w, &cat);
        });
    }
    {
        let weak = w.as_weak();
        let cat = Rc::clone(cat);
        w.on_key_bind_pressed(move || {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let typed = w.get_key_typed().to_string();
            match Key::parse(&typed) {
                Ok(key) => work(Deed::Bind { key, nudge: false }),
                Err(e) => {
                    DESK.with(|d| d.borrow_mut().error = spelt(&cat, &e));
                    draw(&w, &cat);
                }
            }
        });
    }
    w.on_key_remove_pressed(|| work(Deed::Clear));
    {
        let weak = w.as_weak();
        let cat = Rc::clone(cat);
        w.on_key_command_copied(move || {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            let command = w.get_key_command().to_string();
            let said = match scour_clip::text(&command) {
                Ok(()) => String::new(),
                Err(e) => format!("{e}"),
            };
            DESK.with(|d| d.borrow_mut().error = said);
            draw(&w, &cat);
        });
    }
    {
        let weak = w.as_weak();
        let cat = Rc::clone(cat);
        let link = Rc::clone(link);
        w.on_key_nudged(move |yes| {
            let Some(w) = weak.upgrade() else { return };
            let cat = cat.borrow().clone();
            // Either answer settles it for every face, before anything is bound:
            // a line nobody wants must not come back because a bind failed.
            DESK.with(|d| {
                let mut d = d.borrow_mut();
                d.seen = true;
                d.nudge_error.clear();
            });
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    key_hint_seen: Some(true),
                    ..Default::default()
                },
            });
            if yes && let Ok(key) = Key::parse(OFFERED) {
                w.set_key_typed(OFFERED.into());
                work(Deed::Bind { key, nudge: true });
            }
            draw(&w, &cat);
        });
    }
}

/// Write the row and the first-run line from what is known now.
fn draw(w: &MainWindow, cat: &Catalogue) {
    DESK.with(|d| {
        let d = d.borrow();
        let Shown {
            state,
            command,
            bound,
        } = shown(cat, d.status.as_ref());
        w.set_key_state(state.into());
        w.set_key_command(command.into());
        w.set_key_bound(bound);
        w.set_key_can_bind(d.can_bind);
        w.set_key_bind(t(cat, if bound { "Change" } else { "Bind" }));
        w.set_key_later(if d.later {
            t(cat, "It takes effect after the next login.")
        } else {
            slint::SharedString::new()
        });
        w.set_key_error(d.error.as_str().into());
        w.set_key_preview(d.preview.as_str().into());
        w.set_key_nudge(nudge(d.status.as_ref(), d.can_bind, d.seen));
        w.set_key_nudge_error(d.nudge_error.as_str().into());
    });
}

/// What the row says about the state, and the command to bind by hand.
pub struct Shown {
    pub state: String,
    /// Empty unless this program cannot write the binding itself.
    pub command: String,
    pub bound: bool,
}

/// The row's words for one state. `None` is "no worker has answered yet".
pub fn shown(cat: &Catalogue, status: Option<&Result<Status, Error>>) -> Shown {
    let plain = |state: String| Shown {
        state,
        command: String::new(),
        bound: false,
    };
    match status {
        None => plain(String::new()),
        Some(Ok(Status::Bound(key))) => Shown {
            state: key.to_string(),
            command: String::new(),
            bound: true,
        },
        Some(Ok(Status::Unbound)) => plain(cat.get("not bound").into_owned()),
        Some(Ok(Status::CannotBind { command })) => Shown {
            state: cat
                .get("This desktop cannot be bound from here. Bind a key of your choice to:")
                .into_owned(),
            command: command.clone(),
            bound: false,
        },
        Some(Err(e)) => plain(why(cat, e)),
    }
}

/// What the field says back as it is typed: the error, or what will be written.
/// Only one of the two is ever set.
pub fn read_back(cat: &Catalogue, typed: &str) -> (String, String) {
    if typed.len() > TYPED_CAP || typed.trim().is_empty() {
        return (String::new(), String::new());
    }
    match Key::parse(typed) {
        Ok(key) => (String::new(), key.to_string()),
        Err(e) => (spelt(cat, &e), String::new()),
    }
}

/// The first-run line: only where a key can be written, where none is, and
/// where no face has offered it before.
pub fn nudge(status: Option<&Result<Status, Error>>, can_bind: bool, seen: bool) -> bool {
    !seen && can_bind && matches!(status, Some(Ok(Status::Unbound)))
}

/// A parse failure in the reader's language. The two carrying a word keep it.
fn spelt(cat: &Catalogue, e: &KeyError) -> String {
    match e {
        KeyError::Empty => cat.get("no key given").into_owned(),
        KeyError::NoKey => cat.get("a key has to follow the modifiers").into_owned(),
        KeyError::OnlyModifiers => cat.get("modifiers alone are not a key").into_owned(),
        KeyError::UnknownModifier(m) => cat.get("not a modifier: {m}").replace("{m}", m),
    }
}

/// Why the desktop refused. A tool's own words are left as the tool said them.
fn why(cat: &Catalogue, e: &Error) -> String {
    match e {
        Error::Unsupported => cat
            .get("this desktop cannot be bound from here")
            .into_owned(),
        Error::Sandboxed => cat
            .get("a sandboxed program cannot bind a key")
            .into_owned(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn english() -> Catalogue {
        Catalogue::for_language("en")
    }

    #[test]
    fn the_first_run_line_is_offered_once_and_only_where_it_can_be_taken() {
        let unbound = Ok(Status::Unbound);
        assert!(nudge(Some(&unbound), true, false), "the one case it shows");

        // Already offered, by this face or another.
        assert!(!nudge(Some(&unbound), true, true));
        // A desktop this program cannot write to: the row says what to bind by
        // hand, and the line would be an offer it cannot keep.
        assert!(!nudge(Some(&unbound), false, false));
        let cannot = Ok(Status::CannotBind {
            command: "scour-open".into(),
        });
        assert!(!nudge(Some(&cannot), false, false));
        // Something is already bound: there is nothing to offer.
        let bound = Ok(Status::Bound(Key::parse("super+f").expect("super+f")));
        assert!(!nudge(Some(&bound), true, false));
        // Nothing asked yet, and an answer that was an error.
        assert!(!nudge(None, true, false));
        let broken: Result<Status, Error> = Err(Error::Tool("gsettings: no".into()));
        assert!(!nudge(Some(&broken), true, false));
    }

    #[test]
    fn the_row_says_the_state_in_the_neutral_spelling() {
        let cat = english();
        let bound = Ok(Status::Bound(Key::parse("Ctrl+Alt+S").expect("key")));
        let row = shown(&cat, Some(&bound));
        assert_eq!(row.state, "ctrl+alt+s");
        assert!(row.bound);
        assert!(row.command.is_empty());

        let row = shown(&cat, Some(&Ok(Status::Unbound)));
        assert_eq!(row.state, "not bound");
        assert!(!row.bound);

        // The one state that hands a command over instead.
        let cannot = Ok(Status::CannotBind {
            command: "flatpak run com.revisetouch.Scour".into(),
        });
        let row = shown(&cat, Some(&cannot));
        assert_eq!(row.command, "flatpak run com.revisetouch.Scour");
        assert!(row.state.ends_with(':'), "{}", row.state);

        // Nothing is claimed before a worker has answered.
        assert!(shown(&cat, None).state.is_empty());
        // A tool's refusal is the tool's own words.
        let refused: Result<Status, Error> = Err(Error::Tool("gsettings: no such schema".into()));
        assert_eq!(
            shown(&cat, Some(&refused)).state,
            "gsettings: no such schema"
        );
    }

    #[test]
    fn the_field_answers_every_keystroke_with_one_of_the_two() {
        let cat = english();
        // Halfway through typing: neither an error nor a promise.
        assert_eq!(read_back(&cat, ""), (String::new(), String::new()));
        assert_eq!(read_back(&cat, "   "), (String::new(), String::new()));

        let (error, preview) = read_back(&cat, "Super+F");
        assert!(error.is_empty());
        assert_eq!(preview, "super+f", "the preview is what gets written");
        assert_eq!(read_back(&cat, "win+space").1, "super+space");

        let (error, preview) = read_back(&cat, "hyper+f");
        assert_eq!(error, "not a modifier: hyper");
        assert!(preview.is_empty());
        assert_eq!(
            read_back(&cat, "ctrl+").0,
            "a key has to follow the modifiers"
        );

        // Longer than the field will hold: nothing is said about it.
        assert_eq!(read_back(&cat, &"a+".repeat(80)).0, "");
    }

    /// Every msgid this row can show, in every language that is translated.
    #[test]
    fn the_keyboard_row_has_a_word_in_every_translated_language() {
        const SAID: [&str; 15] = [
            "Keyboard shortcut",
            "not bound",
            "Bind",
            "Change",
            "Remove",
            "Copy",
            "It takes effect after the next login.",
            "Open Scour from anywhere: bind Super+F",
            "Bind Super+F",
            "Not now",
            "This desktop cannot be bound from here. Bind a key of your choice to:",
            "no key given",
            "a key has to follow the modifiers",
            "modifiers alone are not a key",
            "for example super+f or ctrl+alt+s",
        ];
        let translated: Vec<Catalogue> = scour_i18n::LANGUAGES
            .iter()
            .map(|(tag, _)| Catalogue::for_language(tag))
            .filter(Catalogue::is_translated)
            .collect();
        assert!(!translated.is_empty(), "no shipped language is translated");
        for c in translated {
            for msgid in SAID {
                assert!(c.has(msgid), "{} has no word for {msgid:?}", c.locale());
            }
            // The one with a word in it has to keep the word.
            assert!(c.has("not a modifier: {m}"));
            assert!(
                c.get("not a modifier: {m}").contains("{m}"),
                "{} dropped the placeholder",
                c.locale()
            );
            assert!(c.has("this desktop cannot be bound from here"));
            assert!(c.has("a sandboxed program cannot bind a key"));
        }
    }
}
