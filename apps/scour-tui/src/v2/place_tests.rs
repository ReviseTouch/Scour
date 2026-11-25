use super::*;
use crate::app::Panel;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use scour_proto::{Disk, DiskState, Seen, Volume, VolumeKind};

fn disk(id: &str, label: &str, root: &str, kind: VolumeKind, state: DiskState) -> Disk {
    Disk {
        volume: Volume {
            id: id.into(),
            roots: vec![root.into()],
            label: label.into(),
            kind,
            fs: "ext4".into(),
            size: 1 << 30,
        },
        state,
        seen: Seen::default(),
    }
}

/// The owner's machine: the system disk indexed through `home` and `sistem`
/// only, Depo set in config.toml and watched, and a stick waiting.
fn machine() -> (App, Vec<(String, Vec<String>)>) {
    let mut depo = disk(
        "uuid:depo",
        "Depo",
        "/mnt/depo",
        VolumeKind::Fixed,
        DiskState::Configured,
    );
    depo.seen.live = true;
    let app = App {
        disks: vec![
            disk(
                "uuid:cachy",
                "CACHY",
                "/",
                VolumeKind::System,
                DiskState::Manual,
            ),
            depo,
            disk(
                "uuid:ab12",
                "STICK",
                "/run/media/demo/STICK",
                VolumeKind::Removable,
                DiskState::Asking,
            ),
        ],
        ..App::default()
    };
    let to = |rs: &[&str]| rs.iter().map(|r| r.to_string()).collect::<Vec<_>>();
    let sources = vec![
        ("home".to_string(), vec![home()]),
        ("depo".to_string(), to(&["/mnt/depo"])),
        ("sistem".to_string(), to(&["/usr", "/etc", "/opt", "/var"])),
    ];
    (app, sources)
}

fn key(app: &mut App, c: char) -> Want {
    crate::keys::press(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
}

fn enter(app: &mut App) -> Want {
    crate::keys::press(app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
}

fn texts(app: &App, wide: usize) -> Vec<String> {
    app.lens_panel_lines(wide)
        .1
        .into_iter()
        .map(|l| format!("{}|{}", l.text, l.key))
        .collect()
}

#[test]
fn the_disks_come_once_this_openings_sources_have_and_are_counted_once() {
    let (mut app, sources) = machine();
    assert!(matches!(
        app.open_place_lens(),
        Want::Place { opening: 1, .. }
    ));
    assert_eq!(app.disk_row_count(), 0, "nothing until the sources say");

    // An older opening's answer is dropped.
    assert_eq!(app.sources_came(0, sources.clone()), Want::Nothing);
    assert_eq!(app.disk_row_count(), 0);

    let Want::DiskCounts { opening: 1, asks } = app.sources_came(1, sources.clone()) else {
        panic!("the choosable disks are counted");
    };
    let ids: Vec<&str> = asks.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["uuid:cachy", "uuid:depo"], "not the stick");
    assert!(
        asks[1].1.starts_with("file: under:/mnt/depo"),
        "{}",
        asks[1].1
    );
    // The same disks again, as a status refresh brings them: no second ask.
    assert_eq!(app.disks_came(app.disks.clone()), Want::Nothing);

    app.disk_counted(0, "uuid:depo", 9);
    app.disk_counted(1, "uuid:depo", 1_234_567);
    let lines = texts(&app, 80);
    assert!(lines[2].ends_with("|1,234,567 files"), "{}", lines[2]);
    assert!(lines[0].ends_with('|'), "not counted yet: {}", lines[0]);

    // Opened again: counted again, from nothing.
    app.panel = Panel::None;
    assert!(matches!(
        app.open_place_lens(),
        Want::Place { opening: 2, .. }
    ));
    assert!(app.v2.disks.files.is_empty());
    assert!(matches!(
        app.sources_came(2, sources),
        Want::DiskCounts { opening: 2, .. }
    ));
}

#[test]
fn a_disk_line_says_its_name_root_state_and_count_in_columns() {
    let (mut app, sources) = machine();
    app.open_place_lens();
    app.sources_came(1, sources);
    let (_, lines) = app.lens_panel_lines(80);
    assert_eq!(lines[0].head, "Disks");
    assert!(lines[0].rule);
    assert_eq!(lines[0].mark.map(|m| m.kind), Some("system"));
    let t: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert!(t[0].contains("CACHY") && t[0].contains("some folders indexed"));
    // The system disk's folders under it, and no "Index it" for it.
    assert!(t[1].contains("/usr · /var"), "{}", t[1]);
    assert!(t[2].contains("Depo") && t[2].contains("/mnt/depo"));
    assert!(t[2].contains("watched live"));
    assert!(t[3].contains("/run/media/demo/STICK") && t[3].contains("waiting for an answer"));
    assert_eq!(&t[4..7], ["    Index it", "    Not this time", "    Never"]);
    let keys: Vec<&str> = lines[4..7].iter().map(|l| l.key.as_str()).collect();
    assert_eq!(keys, ["i", "l", "n"]);
    assert!(t[7].starts_with("+ Add") && lines[7].rule && lines[7].head.is_empty());
    assert!(t[8].starts_with("! Exclude"));

    // The state starts in one column on every disk's line.
    let at = |line: &str, word: &str| line[..line.find(word).unwrap()].chars().count();
    assert_eq!(at(t[0], "some"), at(t[2], "watched"));
    assert_eq!(at(t[0], "some"), at(t[3], "waiting"));

    // Narrow, the root goes before anything is cut.
    let narrow = texts(&app, 48);
    assert!(!narrow[2].contains("/mnt/depo"), "{}", narrow[2]);
    assert!(narrow[2].contains("watched live"), "{}", narrow[2]);
}

#[test]
fn enter_chooses_one_disk_and_again_lets_it_go() {
    let (mut app, sources) = machine();
    app.open_place_lens();
    app.sources_came(1, sources);

    // Depo, alone.
    app.panel_at = 2;
    assert!(matches!(enter(&mut app), Want::Page { .. }));
    assert_eq!(app.query, "under:/mnt/depo");
    assert_eq!(app.panel, Panel::Lens, "the lens stays to show it");
    let lines = texts(&app, 80);
    assert!(lines[2].starts_with('✓') && !lines[0].starts_with('✓'));

    // The system disk instead: its folders as one group, Depo gone.
    app.panel_at = 0;
    enter(&mut app);
    assert!(!app.query.contains("/mnt/depo"), "{}", app.query);
    for r in [
        "under:/usr",
        "under:/etc",
        "under:/opt",
        "under:/var",
        "under:~",
    ] {
        assert!(app.query.contains(r), "{r} in {}", app.query);
    }
    assert_eq!(app.query.matches('|').count(), 4);
    assert!(texts(&app, 80)[0].starts_with('✓'));
    // Its folders' line chooses it too: held alone, it lets go.
    app.panel_at = 1;
    enter(&mut app);
    assert_eq!(app.query, "");
    assert!(!texts(&app, 80)[0].starts_with('✓'));

    // The stick waits for an answer: Enter on it writes nothing.
    app.panel_at = 3;
    assert_eq!(enter(&mut app), Want::Nothing);
    assert_eq!(app.query, "");
}

#[test]
fn the_question_keys_answer_only_from_the_answers() {
    let (mut app, sources) = machine();
    app.open_place_lens();
    app.sources_came(1, sources);

    // On the stick's own line a letter is typing.
    app.panel_at = 3;
    key(&mut app, 'n');
    assert_eq!(app.query, "n");

    let answer = |what: &str| Want::Disk {
        volume: "uuid:ab12".into(),
        what: what.into(),
    };
    app.panel_at = 4;
    assert_eq!(key(&mut app, 'n'), answer("leave"));
    assert_eq!(app.panel_at, 6, "the cursor on the answer given");
    assert_eq!(key(&mut app, 'l'), answer("later"));
    assert_eq!(key(&mut app, 'i'), answer("index"));
    app.panel_at = 5;
    assert_eq!(enter(&mut app), answer("later"));
    assert_eq!(app.query, "n", "no answer types");
    // Right on an answer goes nowhere rather than answering.
    let right = crate::keys::press(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(right, Want::Nothing);
}

#[test]
fn a_click_lands_on_the_disk_line_drawn_there() {
    let (mut app, sources) = machine();
    app.open_place_lens();
    app.sources_came(1, sources);
    let lines = app.lens_panel_lines(80).1;
    let rules = lines.iter().filter(|l| l.rule).count();
    for size in [(100, 40), (80, 40)] {
        let area = Rect::new(0, 0, size.0, size.1);
        let b = crate::draw::panel_rect(&app, area, lines.len() + rules);
        let col = b.x + 6;
        // The heading, then the disks; a rule above the folders.
        assert_eq!(crate::draw::panel_line_at(&app, area, col, b.y + 2), None);
        assert_eq!(
            crate::draw::panel_line_at(&app, area, col, b.y + 3),
            Some(0)
        );
        assert_eq!(
            crate::draw::panel_line_at(&app, area, col, b.y + 5),
            Some(2)
        );
        assert_eq!(crate::draw::panel_line_at(&app, area, col, b.y + 10), None);
        assert_eq!(
            crate::draw::panel_line_at(&app, area, col, b.y + 11),
            Some(7)
        );
    }

    // Pressed and let go on Depo's line: Depo is chosen.
    let area = Rect::new(0, 0, 100, 40);
    let b = crate::draw::panel_rect(&app, area, lines.len() + rules);
    let at = |kind| MouseEvent {
        kind,
        column: b.x + 10,
        row: b.y + 5,
        modifiers: KeyModifiers::NONE,
    };
    crate::keys::mouse(
        &mut app,
        at(MouseEventKind::Down(MouseButton::Left)),
        (100, 40),
    );
    crate::keys::mouse(
        &mut app,
        at(MouseEventKind::Up(MouseButton::Left)),
        (100, 40),
    );
    assert_eq!(app.query, "under:/mnt/depo");
}

#[test]
fn the_folders_add_and_exclude_beside_a_chosen_disk() {
    let (mut app, sources) = machine();
    app.open_place_lens();
    app.sources_came(1, sources);
    app.panel_at = 2;
    enter(&mut app);
    app.v2.place_dir = "/mnt/data".into();
    app.rebuild_lens();

    // "Add" joins the group; "!" leaves it as it is.
    app.panel_at = app.place_line(0);
    enter(&mut app);
    assert_eq!(app.query, "under:/mnt/depo|under:/mnt/data");
    app.open_place_lens();
    app.v2.place_dir = "/mnt/old".into();
    key(&mut app, '!');
    assert_eq!(app.query, "under:/mnt/depo|under:/mnt/data !under:/mnt/old");
}
