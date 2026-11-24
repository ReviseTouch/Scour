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
    const SAID: [&str; 17] = [
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
        "for example {a} or {b}",
        scour_hotkey::WINDOWS_REFUSED,
        scour_hotkey::MAC_REFUSED,
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
