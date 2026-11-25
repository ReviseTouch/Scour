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
pub(crate) fn machine() -> (App, Vec<(String, Vec<String>)>) {
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

pub(crate) fn key(app: &mut App, c: char) -> Want {
    crate::keys::press(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
}

pub(crate) fn enter(app: &mut App) -> Want {
    crate::keys::press(app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
}

pub(crate) fn texts(app: &App, wide: usize) -> Vec<String> {
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
    assert_eq!(app.disk_row_count(), 1, "no disk until the sources say");

    // The sources read at start name the disks, and count nothing.
    assert_eq!(app.sources_came(0, sources.clone()), Want::Nothing);
    assert!(app.disk_row_count() > 1);
    assert!(!app.v2.disks.fresh);

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
    assert!(lines[3].ends_with("|1,234,567 files"), "{}", lines[3]);
    assert!(lines[1].ends_with('|'), "not counted yet: {}", lines[1]);

    // Opened again: counted again, from nothing.
    app.panel = Panel::None;
    assert!(matches!(
        app.open_place_lens(),
        Want::Place { opening: 2, .. }
    ));
    assert!(app.v2.disks.files.is_empty());
    assert_eq!(
        app.sources_came(1, sources.clone()),
        Want::Nothing,
        "an older opening's"
    );
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
    let t: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(t[0], "Searching in: Everywhere");
    assert!(lines[0].dimmed && !lines[0].rule);
    assert_eq!(lines[1].head, "Disks");
    assert!(lines[1].rule);
    assert_eq!(lines[1].mark.map(|m| m.kind), Some("system"));
    assert!(t[1].contains("CACHY") && t[1].contains("some folders indexed"));
    // The system disk's folders under it, and no "Index it" for it.
    assert!(t[2].contains("/usr · /var"), "{}", t[2]);
    assert!(t[3].contains("Depo") && t[3].contains("/mnt/depo"));
    assert!(t[3].contains("watched live"));
    assert!(t[4].contains("/run/media/demo/STICK") && t[4].contains("waiting for an answer"));
    assert_eq!(&t[5..8], ["    Index it", "    Not this time", "    Never"]);
    let keys: Vec<&str> = lines[5..8].iter().map(|l| l.key.as_str()).collect();
    assert_eq!(keys, ["i", "l", "n"]);
    assert!(t[8].starts_with("+ Add") && lines[8].rule && lines[8].head.is_empty());
    assert!(t[9].starts_with("! Exclude"));

    // The state starts in one column on every disk's line.
    let at = |line: &str, word: &str| line[..line.find(word).unwrap()].chars().count();
    assert_eq!(at(t[1], "some"), at(t[3], "watched"));
    assert_eq!(at(t[1], "some"), at(t[4], "waiting"));

    // Narrow, the root goes before anything is cut.
    let narrow = texts(&app, 48);
    assert!(!narrow[3].contains("/mnt/depo"), "{}", narrow[3]);
    assert!(narrow[3].contains("watched live"), "{}", narrow[3]);
}

#[test]
fn enter_chooses_one_disk_and_again_lets_it_go() {
    let (mut app, sources) = machine();
    app.open_place_lens();
    app.sources_came(1, sources);

    // Depo, alone.
    app.panel_at = 3;
    assert!(matches!(enter(&mut app), Want::Page { .. }));
    assert_eq!(app.query, "under:/mnt/depo");
    assert_eq!(app.panel, Panel::Lens, "the lens stays to show it");
    let lines = texts(&app, 80);
    assert!(lines[3].starts_with('✓') && !lines[1].starts_with('✓'));
    assert!(
        lines[0].starts_with("Searching in: 1 Depo|"),
        "{}",
        lines[0]
    );

    // The system disk instead: its folders as one group, Depo gone.
    app.panel_at = 1;
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
    assert!(texts(&app, 80)[1].starts_with('✓'));
    // Held whole, the disk is one entry by its name.
    assert_eq!(texts(&app, 80)[0], "Searching in: 1 CACHY|");
    // Its folders' line chooses it too: held alone, it lets go.
    app.panel_at = 2;
    enter(&mut app);
    assert_eq!(app.query, "");
    assert!(!texts(&app, 80)[1].starts_with('✓'));

    // The stick waits for an answer: Enter on it writes nothing.
    app.panel_at = 4;
    assert_eq!(enter(&mut app), Want::Nothing);
    assert_eq!(app.query, "");
}

#[test]
fn the_question_keys_answer_only_from_the_answers() {
    let (mut app, sources) = machine();
    app.open_place_lens();
    app.sources_came(1, sources);

    // On the stick's own line a letter is typing.
    app.panel_at = 4;
    key(&mut app, 'n');
    assert_eq!(app.query, "n");

    let answer = |what: &str| Want::Disk {
        volume: "uuid:ab12".into(),
        what: what.into(),
    };
    app.panel_at = 5;
    assert_eq!(key(&mut app, 'n'), answer("leave"));
    assert_eq!(app.panel_at, 7, "the cursor on the answer given");
    assert_eq!(key(&mut app, 'l'), answer("later"));
    assert_eq!(key(&mut app, 'i'), answer("index"));
    app.panel_at = 6;
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
        // Where it searches, the heading, then the disks; a rule above the
        // folders.
        let line_at = |row| crate::draw::panel_line_at(&app, area, col, row);
        assert_eq!(line_at(b.y + 2), Some(0));
        assert_eq!(line_at(b.y + 3), None);
        assert_eq!(line_at(b.y + 4), Some(1));
        assert_eq!(line_at(b.y + 6), Some(3));
        assert_eq!(line_at(b.y + 11), None);
        assert_eq!(line_at(b.y + 12), Some(8));
    }

    // Pressed and let go on Depo's line: Depo is chosen.
    let area = Rect::new(0, 0, 100, 40);
    let b = crate::draw::panel_rect(&app, area, lines.len() + rules);
    let at = |kind| MouseEvent {
        kind,
        column: b.x + 10,
        row: b.y + 6,
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
    app.panel_at = 3;
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

#[test]
fn a_disk_indexed_through_folders_lists_them_while_it_is_walked() {
    let (mut app, sources) = machine();
    app.disks[0].seen.walking = true;
    app.disks[1].seen.walking = true;
    app.open_place_lens();
    app.sources_came(1, sources);
    let lines = texts(&app, 80);
    assert!(lines[1].contains("being walked"), "{}", lines[1]);
    assert!(lines[2].contains("/usr · /var"), "{}", lines[2]);
    assert!(
        lines[3].contains("Depo"),
        "a whole disk has no folders' line"
    );
}

/// The lens open on the owner's machine, browsing `dir`, over `query` with
/// the caret at its start: in no term, so none is being typed.
fn lens_on(query: &str, dir: &str) -> App {
    let (mut app, sources) = machine();
    app.query = query.into();
    app.caret = 0;
    app.open_place_lens();
    app.sources_came(1, sources);
    app.v2.place_dir = dir.into();
    app.rebuild_lens();
    app
}

fn add(app: &mut App) -> Want {
    app.panel_at = app.place_line(0);
    enter(app)
}

#[test]
fn add_on_a_folder_already_held_leaves_it() {
    let mut app = lens_on("rapor under:/mnt/depo|under:/usr", "/usr");
    add(&mut app);
    assert_eq!(app.query, "rapor under:/mnt/depo|under:/usr");
    // The caret rests at the group's trailing edge, not editing it.
    assert_eq!(&app.query[..app.caret], "rapor under:/mnt/depo|under:/usr");
    assert!(!app.v2.editing);
}

#[test]
fn add_narrows_or_widens_a_member_in_place() {
    let h = home();
    let mut app = lens_on("rapor under:~|under:/mnt/depo", &format!("{h}/Belgeler"));
    add(&mut app);
    assert_eq!(app.query, "rapor under:~/Belgeler|under:/mnt/depo");
    app.open_place_lens();
    app.v2.place_dir = h;
    add(&mut app);
    assert_eq!(app.query, "rapor under:~|under:/mnt/depo");
}

#[test]
fn add_lets_a_term_still_being_typed_go_and_the_list_shows_its_folder() {
    let (mut app, _) = machine();
    app.query = "rapor under:~/Bel".into();
    app.caret = app.query.len();
    app.open_place_lens();
    assert_eq!(app.v2.place_dir, home(), "the folder the value names");
    let h = home();
    app.tree_landed(
        h.clone(),
        vec![format!("{h}/Belgeler"), format!("{h}/Projeler")],
    );
    let folders: Vec<&str> = app
        .v2
        .lens_items
        .iter()
        .map(|i| i.0.as_str())
        .filter(|t| t.starts_with('▸'))
        .collect();
    assert_eq!(folders, ["▸ Belgeler/"], "narrowed by what is typed");
    add(&mut app);
    assert_eq!(app.query, "rapor under:~");

    // Opened again where the lens left the caret: nothing is being typed.
    app.open_place_lens();
    assert!(app.v2.place_part.is_none());
}

#[test]
fn exclude_toggles_and_takes_the_folder_out_of_the_group() {
    let mut app = lens_on("under:/mnt/depo|under:/mnt/data", "/mnt/data");
    key(&mut app, '!');
    assert_eq!(app.query, "under:/mnt/depo !under:/mnt/data");
    app.open_place_lens();
    app.v2.place_dir = "/mnt/data".into();
    key(&mut app, '!');
    assert_eq!(app.query, "under:/mnt/depo");
}

/// The query line's row, drawn as the whole screen draws it.
fn bar(app: &App) -> String {
    let row =
        crate::v2::filters::tests::screen(app, 100, 30)[crate::draw::QUERY_ROW as usize].clone();
    row.trim_end().to_string()
}

/// A disk chosen in the lens reads as one unit at once: the caret rests at
/// its edge, nobody is typing there.
#[test]
fn the_system_disk_just_chosen_reads_collapsed() {
    let (mut app, sources) = machine();
    app.query = "rapor ".into();
    app.caret = app.query.len();
    app.open_place_lens();
    app.sources_came(1, sources);
    let tiles = app.place_tiles();
    let cachy = tiles.iter().position(|t| t.name == "CACHY").unwrap();
    let rows = app.disk_rows(&tiles);
    app.panel_at = rows.iter().position(|r| *r == Row::Tile(cachy)).unwrap();
    enter(&mut app);
    assert!(app.query.contains("under:"), "{}", app.query);
    assert_eq!(app.caret, app.query.len());
    assert!(
        bar(&app).starts_with(" SCOUR rapor ⛁ CACHY"),
        "{}",
        bar(&app)
    );

    // Typed into, the member is the person's letters again.
    key(&mut app, 'x');
    assert!(bar(&app).contains("under:/varx"), "{}", bar(&app));
}

/// The sources read at start name a disk in the query line before the place
/// lens was ever opened; a block of exclusions is one negated unit.
#[test]
fn the_query_line_names_disks_before_any_lens() {
    let (mut app, sources) = machine();
    app.query = "under:~|under:/usr|under:/etc|under:/opt|under:/var rapor".into();
    app.caret = app.query.len();
    assert!(
        !bar(&app).contains("CACHY"),
        "its sources unknown: {}",
        bar(&app)
    );
    app.sources_came(0, sources);
    assert!(
        bar(&app).starts_with(" SCOUR ⛁ CACHY rapor"),
        "{}",
        bar(&app)
    );

    app.query = "rapor !under:/mnt/depo !under:/srv/x".into();
    app.caret = 5;
    assert!(
        bar(&app).starts_with(" SCOUR rapor ⛁ !Depo · /srv/x"),
        "{}",
        bar(&app)
    );
}

/// Opened on an exclusion, a press leaves the caret on one.
#[test]
fn a_lens_opened_on_an_exclusion_stays_on_it() {
    let (mut app, sources) = machine();
    app.query = "rapor !under:/tmp".into();
    app.caret = app.query.len();
    app.open_place_lens();
    assert!(app.v2.place_not);
    app.sources_came(1, sources);
    app.v2.place_dir = "/srv".into();
    app.rebuild_lens();
    app.panel_at = app.place_line(1);
    enter(&mut app);
    assert_eq!(app.query, "rapor !under:/tmp !under:/srv");
    assert!(
        app.query[..app.caret].ends_with("!under:/tmp")
            || app.query[..app.caret].ends_with("!under:/srv")
    );
    assert!(!app.v2.editing);
}
