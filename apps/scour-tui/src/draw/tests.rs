use super::*;

/// A press lands on what is drawn under it: a word of the strip by its
/// columns, an item of the list by its row, and nothing on a rule.
#[test]
fn a_press_on_the_menu_lands_on_what_is_drawn_there() {
    let line = |id: &str, label: &str, strip: bool, rule: bool| crate::app::MenuLine {
        id: id.into(),
        label: label.into(),
        key: String::new(),
        rule,
        careful: false,
        heavy: false,
        strip,
    };
    let mut app = App {
        panel: Panel::Menu,
        ..Default::default()
    };
    app.menu = vec![
        line("copy-file", "Dosya", true, false),
        line("copy-path", "Yol", true, false),
        line("open", "Aç", false, false),
        line("search-here", "Ara", false, true),
    ];
    let area = Rect::new(0, 0, 100, 34);
    let b = menu_rect(area, &app);
    // Title, detail, gap, strip, gap, rule, Aç, rule, Ara — under the border.
    let cells = strip_cells(&app, b);
    assert_eq!(menu_item_at(&app, area, cells[1].0, b.y + 4), Some(1));
    assert_eq!(
        menu_item_at(&app, area, cells[1].1, b.y + 4),
        None,
        "the gap between words"
    );
    assert_eq!(menu_item_at(&app, area, b.x + 5, b.y + 7), Some(2));
    assert_eq!(menu_item_at(&app, area, b.x + 5, b.y + 8), None, "a rule");
    assert_eq!(menu_item_at(&app, area, b.x + 5, b.y + 9), Some(3));
}

/// True colour and the dark palette, whatever the terminal running the
/// test says: a colour asserted on has to be the palette's own.
fn theme() -> Theme {
    Theme {
        palette: &scour_ui::DARK,
        truecolor: true,
        theirs: false,
    }
}

/// What is painted is what the layout measured: the spans of a row join
/// back into the row's own text, and the bar is coloured band by band.
#[test]
fn a_painted_row_is_the_row_the_layout_measured() {
    let theme = theme();
    let child = scour_core::DirUsage {
        path: "/home/hasan/Projeler".into(),
        bytes: 651_571_270_524,
        disk: 651_571_270_524,
        files: 896_648,
        age: [89_000_000_000, 0, 499_571_270_524, 47_000_000_000, 0, 0],
    };
    let fit = crate::report::fit(100, 7);
    let row = crate::report::row(&child, true, child.bytes, 81.6, &fit, ('.', ','));
    let painted = weighed_row(&row, &theme);
    let joined: String = painted.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(joined, row.text());
    // Today, this month, six months: the three bands this folder has, in
    // the order `DirUsage.age` holds them.
    let bands: Vec<usize> = row.bar.iter().map(|(band, _)| *band).collect();
    assert_eq!(bands, vec![0, 2, 3]);
    let colours: Vec<ratatui::style::Color> = painted[1..1 + bands.len()]
        .iter()
        .map(|s| s.style.fg.expect("a band is a colour"))
        .collect();
    assert_eq!(
        colours,
        bands.iter().map(|b| theme.band(*b)).collect::<Vec<_>>()
    );
}

/// The kind bar is one hue in six steps, and every slice past the sixth
/// shares the palest of them; what was folded away is quieter still.
#[test]
fn the_kind_bar_steps_through_six_and_stops() {
    let theme = theme();
    assert_ne!(theme.step(0), theme.step(1));
    assert_eq!(theme.step(6), theme.step(5), "and 60 is the fifth as well");
    assert_eq!(theme.step(60), theme.step(5));
    assert_ne!(theme.step(5), theme.step_rest());
}

/// A sentence that does not fit is drawn whole over several lines, and a
/// path with no spaces in it is cut rather than lost off the right edge.
#[test]
fn a_line_too_wide_for_the_panel_is_wrapped_rather_than_cut() {
    let said = "This desktop cannot be bound from here. Bind a key of your choice to:";
    let lines = wrapped(said, 30, false);
    assert!(lines.len() > 1, "one line would have been cut");
    for line in &lines {
        assert!(line.text.chars().count() <= 30, "{:?}", line.text);
    }
    let back: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(back.join(" "), said, "and nothing was lost doing it");

    let path = "/home/somebody/.local/share/applications/a-very-long-name/scour-gui";
    let lines = wrapped(path, 20, true);
    assert!(lines.iter().all(|l| l.text.chars().count() <= 20));
    assert_eq!(
        lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<String>()
            .replace(' ', ""),
        path
    );
    assert_eq!(wrapped("", 20, false).len(), 1, "an empty line is a line");
}
