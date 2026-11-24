use super::*;

/// An app with a screen to put rows on. `App::default()` has no room, and
/// a view zero rows tall answers every scrolling question the same way.
fn app(room: usize) -> App {
    App {
        room,
        ..Default::default()
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn letters_type_in_search_and_move_in_move() {
    let mut app = App::default();
    press(&mut app, key(KeyCode::Char('j')));
    assert_eq!(app.query, "j", "a letter is a letter while searching");

    app.query.clear();
    app.caret = 0;
    press(&mut app, key(KeyCode::Esc));
    assert_eq!(app.mode, Mode::Move, "an empty query, then the mode");
    press(&mut app, key(KeyCode::Char('j')));
    assert_eq!(app.query, "", "and now it moves instead");
}

#[test]
fn escape_clears_what_was_typed_before_it_changes_anything_else() {
    let mut app = App::default();
    press(&mut app, key(KeyCode::Char('a')));
    press(&mut app, key(KeyCode::Esc));
    assert_eq!(app.query, "");
    assert_eq!(app.mode, Mode::Search, "still typing");
}

#[test]
fn the_arrows_move_in_both_modes() {
    let mut app = app(10);
    app.pages.set_total(100);
    press(&mut app, key(KeyCode::Down));
    assert_eq!(app.cursor, 1);
    app.mode = Mode::Move;
    press(&mut app, key(KeyCode::Down));
    assert_eq!(app.cursor, 2);
}

#[test]
fn space_picks_in_move_mode_and_types_in_search() {
    let mut app = app(4);
    app.pages.set_total(10);
    press(&mut app, key(KeyCode::Char(' ')));
    assert_eq!(app.query, " ", "a space is a space while typing");

    let mut app = App {
        mode: Mode::Move,
        ..App::default()
    };
    app.room = 4;
    press(&mut app, key(KeyCode::Char(' ')));
    assert!(app.picked.is_empty(), "nothing under the cursor to pick");
}

#[test]
fn sorting_is_ctrl_and_never_the_bare_arrows() {
    let mut app = app(4);
    app.pages.set_total(10);
    let was = app.sort;
    press(&mut app, key(KeyCode::Right));
    assert_eq!(app.sort, was, "a bare arrow does not re-sort");
    press(
        &mut app,
        KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL),
    );
    assert_ne!(app.sort, was, "with ctrl it does");
    let down = app.descending;
    press(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
    assert_ne!(app.descending, down);
}

#[test]
fn the_help_is_closed_by_whatever_is_pressed_next() {
    let mut app = App::default();
    press(&mut app, key(KeyCode::F(1)));
    assert!(app.helping);
    press(&mut app, key(KeyCode::Char('x')));
    assert!(!app.helping);
    assert_eq!(app.query, "", "and that key did nothing else");
}

#[test]
fn tab_moves_the_arrows_into_the_rail_and_back() {
    let mut app = app(4);
    app.pages.set_total(100);
    app.kinds = vec![("doc".into(), 9), ("code".into(), 4)];
    press(&mut app, key(KeyCode::Tab));
    assert!(app.in_rail);
    press(&mut app, key(KeyCode::Down));
    assert_eq!(app.rail_at, 1, "in the rail, down moves the rail");
    assert_eq!(app.cursor, 0, "and leaves the list alone");
    press(&mut app, key(KeyCode::Tab));
    assert!(!app.in_rail);
    press(&mut app, key(KeyCode::Down));
    assert_eq!(app.cursor, 1, "and now it moves the list again");
}

#[test]
fn pressing_a_filter_narrows_the_query_and_pressing_it_again_does_not() {
    let mut app = app(4);
    app.kinds = vec![("doc".into(), 9)];
    app.in_rail = true;
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.filter.as_deref(), Some("kind:doc"));
    assert_eq!(
        app.asking(),
        "kind:doc",
        "and it is what the service is asked"
    );
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.filter, None, "the same press clears it");
}

#[test]
fn a_panel_takes_the_arrows_and_gives_them_back() {
    let mut app = app(4);
    app.pages.set_total(100);
    app.rules = vec![
        ("dir:target".into(), "target".into(), false, true),
        ("path:/proc".into(), "/proc".into(), true, false),
    ];
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.panel, Panel::Rules);
    press(&mut app, key(KeyCode::Down));
    assert_eq!(app.panel_at, 1);
    assert_eq!(app.cursor, 0, "the list did not move");
    press(&mut app, key(KeyCode::Esc));
    assert_eq!(app.panel, Panel::None);
    press(&mut app, key(KeyCode::Down));
    assert_eq!(app.cursor, 1, "and the arrows are the list's again");
}

#[test]
fn switching_a_rule_off_sends_the_whole_list_the_service_gave() {
    // A list built from what was pressed, rather than from the service's
    // answer, switches every other rule back on.
    let mut app = App {
        rules: vec![
            ("dir:target".into(), "target".into(), false, true),
            ("path:/proc".into(), "/proc".into(), true, false),
            ("path:/sys".into(), "/sys".into(), true, false),
        ],
        panel: Panel::Rules,
        panel_at: 0,
        ..Default::default()
    };
    let want = panel_press(&mut app);
    match want {
        Want::OffRules(off) => assert_eq!(
            off,
            vec!["dir:target", "path:/proc", "path:/sys"],
            "the one pressed, and everything already off"
        ),
        other => panic!("{other:?}"),
    }
}

/// The whole of the key panel: it asks when it opens, letters go into it
/// rather than into the query, and `Enter` sends what was typed as typed.
#[test]
fn the_key_panel_takes_the_letters_the_query_would_have_had() {
    let mut app = app(4);
    let want = press(
        &mut app,
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.panel, Panel::Key);
    assert_eq!(want, Want::Hotkey(Deed::Read), "asked as it opens");

    for c in "super+f".chars() {
        press(&mut app, key(KeyCode::Char(c)));
    }
    assert_eq!(app.key_typed, "super+f");
    assert_eq!(app.query, "", "and the query underneath is untouched");
    press(&mut app, key(KeyCode::Backspace));
    assert_eq!(app.key_typed, "super+", "backspace is the field's too");
    press(&mut app, key(KeyCode::Char('f')));

    let want = press(&mut app, key(KeyCode::Enter));
    assert_eq!(
        want,
        Want::Hotkey(Deed::Bind("super+f".into())),
        "as typed: what counts as a combination is scour-hotkey's judgement"
    );
    // The key's panel is in the settings now: Escape goes back to their
    // first section, and a second one leaves them.
    press(&mut app, key(KeyCode::Esc));
    assert_eq!(app.panel, Panel::General);
    assert!(app.settings);
    press(&mut app, key(KeyCode::Esc));
    assert_eq!(app.panel, Panel::None);
    assert!(!app.settings);
    press(&mut app, key(KeyCode::Char('a')));
    assert_eq!(app.query, "a", "and the letters are the query's again");
}

/// With Caps Lock on, Ctrl and a letter arrive as the capital: the
/// shortcut is the same one.
#[test]
fn caps_lock_does_not_change_a_shortcut() {
    let mut app = app(4);
    let want = press(
        &mut app,
        KeyEvent::new(KeyCode::Char('K'), KeyModifiers::CONTROL),
    );
    assert_eq!(want, Want::Rules);
    assert!(app.settings);
    assert_eq!(app.query, "", "not typed into the query");
}

/// `x` removes, but only where it cannot be the key somebody is typing.
#[test]
fn x_removes_on_an_empty_line_and_types_on_a_started_one() {
    let mut app = App {
        panel: Panel::Key,
        ..Default::default()
    };
    assert_eq!(
        press(&mut app, key(KeyCode::Char('x'))),
        Want::Hotkey(Deed::Clear)
    );
    for c in "ctrl+".chars() {
        press(&mut app, key(KeyCode::Char(c)));
    }
    assert_eq!(
        press(&mut app, key(KeyCode::Char('x'))),
        Want::Nothing,
        "here it is the key being bound"
    );
    assert_eq!(app.key_typed, "ctrl+x");
    // Delete says it whatever is in the field.
    assert_eq!(
        press(&mut app, key(KeyCode::Delete)),
        Want::Hotkey(Deed::Clear)
    );
    assert_eq!(app.key_typed, "", "and the field is emptied with it");
}

/// Where nothing can be bound the panel is a sentence: no field, nothing
/// to press, and the letters belong to the query as under any other panel.
#[test]
fn a_desktop_that_cannot_be_bound_offers_nothing_to_press() {
    let mut app = App {
        panel: Panel::Key,
        key_can: false,
        key_command: "scour-open".into(),
        ..Default::default()
    };
    assert_eq!(app.panel_lines(), 0, "nothing to walk and nothing to press");
    press(&mut app, key(KeyCode::Char('x')));
    assert_eq!(app.key_typed, "", "there is no field to type into");
    assert_eq!(app.query, "x", "so it went where letters go");
    assert_eq!(press(&mut app, key(KeyCode::Enter)), Want::Nothing);
}

/// The answer is what the desktop says, and it clears the field — unless
/// it refused, where what was typed is what has to be corrected.
#[test]
fn the_answer_clears_the_field_but_a_refusal_leaves_it_to_be_fixed() {
    let mut app = App {
        panel: Panel::Key,
        key_typed: "super+f".into(),
        ..Default::default()
    };
    app.keyed(
        "GNOME".into(),
        Some("super+f".into()),
        "scour-open".into(),
        true,
        String::new(),
        false,
    );
    assert_eq!(app.key_bound.as_deref(), Some("super+f"));
    assert_eq!(app.key_typed, "");
    assert!(!app.key_bad);

    app.key_typed = "hyper+f".into();
    app.keyed(
        "GNOME".into(),
        None,
        "scour-open".into(),
        true,
        "not a modifier: hyper".into(),
        false,
    );
    assert!(app.key_bad);
    assert_eq!(app.key_note, "not a modifier: hyper");
    assert_eq!(app.key_typed, "hyper+f", "still there to be corrected");

    // KDE's remark is not a refusal: the key was written.
    app.keyed(
        "KDE".into(),
        Some("super+f".into()),
        "scour-open".into(),
        true,
        String::new(),
        true,
    );
    assert!(!app.key_bad);
    assert_eq!(
        app.key_note,
        app.say("It takes effect after the next login.")
    );
}

#[test]
fn ctrl_c_leaves_from_either_mode() {
    for mode in [Mode::Search, Mode::Move] {
        let mut app = App {
            mode,
            ..App::default()
        };
        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(app.leaving, "{mode:?}");
    }
}
