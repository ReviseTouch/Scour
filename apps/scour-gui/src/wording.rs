//! Every word the window shows, from the catalogue, and how numbers are written.

use super::*;
use anyhow::Result;
use scour_core::Catalog;
use scour_i18n::Catalogue;
use slint::{ComponentHandle, ModelRc, VecModel};

/// A count, short enough for a rail 168 pixels wide.
pub(super) fn compact(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{}k", n / 1_000),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// Write every string the window shows. It runs again when a language is picked.
/// Every word on the settings page.
fn settings_words(w: &MainWindow, cat: &Catalogue) {
    let list = |ids: &[&str]| -> ModelRc<slint::SharedString> {
        ModelRc::new(VecModel::from(
            ids.iter()
                .map(|id| t(cat, id))
                .collect::<Vec<slint::SharedString>>(),
        ))
    };
    w.set_settings_title(t(cat, "Settings"));
    w.set_settings_foot(t(
        cat,
        "Changes apply at once; there is nothing to save. Esc closes.",
    ));
    w.set_section_labels(list(&[
        "General",
        "Searching",
        "List",
        "Disks and folders",
        "Skipped",
        "Keyboard",
        "Index",
        "Performance",
        "About",
    ]));
    w.set_set_help(t(cat, "Keys and the query"));
    w.set_section_leads(list(&[
        "The same for every face: the window, the browser and the terminal read these.",
        "How the words you type are read. The same in every face.",
        "How the list is drawn, and what it shows.",
        "Scour knows every file on these. A disk taken out leaves the index; putting it back takes a scan.",
        "Places inside the scanned disks that are not looked at. A rule switched off is kept, and can be switched on again.",
        "The key that opens Scour, what the keys do, and what the query understands.",
        "Where the list of files is kept. It is built from the disks and kept current by itself; building it again is for when something looks wrong.",
        "How Scour spends memory and the processor. Less of either costs a little speed.",
        "What is running, and what it holds.",
    ]));
    w.set_word_show_rail(t(cat, "Show the sidebar"));
    w.set_word_show_ribbon(t(cat, "Show the time ribbon"));
    w.set_word_top(t(cat, "To the top"));
    w.set_word_new(t(cat, "new"));
    w.set_set_index_where(t(cat, "Kept in"));
    w.set_set_index_open(t(cat, "Open the folder"));
    w.set_set_index_tidy(t(cat, "Reorganize"));
    w.set_set_index_tidy_note(t(
        cat,
        "Folds new files into the ordered part of the index. Nothing is removed and searching goes on; it happens by itself when the machine is idle.",
    ));
    w.set_set_index_tidy_push(t(cat, "Reorganize"));
    w.set_set_texts(t(cat, "Keep documents' text"));
    w.set_set_texts_note(t(
        cat,
        "PDF, Word, Excel and PowerPoint files of the old kind and e-mail are read by a helper; kept, the next search finds their words without opening them. Off, every search reads them again.",
    ));
    w.set_set_texts_kept(t(cat, "Documents' text kept"));
    w.set_set_texts_max(t(cat, "Size limit"));
    w.set_set_texts_max_note(t(
        cat,
        "How much compressed text is kept at most. Full, it keeps what was read within a day and takes nothing new.",
    ));
    w.set_texts_max_labels(ModelRc::new(VecModel::from(
        ["512 MB", "1 GB", "2 GB", "5 GB", "10 GB", "20 GB"]
            .map(slint::SharedString::from)
            .to_vec(),
    )));
    w.set_set_texts_clear(t(cat, "Clear"));
    w.set_set_texts_move(t(cat, "Move…"));
    w.set_set_texts_ahead(t(cat, "Read ahead while idle"));
    w.set_set_texts_ahead_note(t(
        cat,
        "When nobody has searched for five minutes and the machine is quiet, documents are read and kept before they are asked for: slowly, at the lowest priority.",
    ));
    w.set_set_index_reset(t(cat, "Reset and scan again"));
    w.set_set_index_reset_note(t(
        cat,
        "Empties the index and scans every disk again, from nothing.",
    ));
    w.set_set_index_reset_push(t(cat, "Reset…"));
    w.set_set_index_reset_sure(t(
        cat,
        "Searches will find little until the scan is done, a minute or a few on most machines.",
    ));
    w.set_set_index_reset_yes(t(cat, "Reset now"));
    w.set_set_index_cancel(t(cat, "Cancel"));
    w.set_set_language(t(cat, "Language"));
    w.set_set_language_note(t(cat, "Empty follows the desktop."));
    let mut labels = vec![t(cat, "The desktop's")];
    let mut tokens = vec![slint::SharedString::new()];
    for (tag, endonym) in scour_i18n::LANGUAGES {
        labels.push((*endonym).into());
        tokens.push((*tag).into());
    }
    w.set_lang_labels(ModelRc::new(VecModel::from(labels)));
    w.set_lang_tokens(ModelRc::new(VecModel::from(tokens)));
    w.set_set_face(t(cat, "When Scour opens"));
    w.set_set_face_note(t(cat, "Which face the key and the menu entry open."));
    w.set_face_labels(list(&["Window", "Browser", "Terminal"]));
    w.set_set_start(t(cat, "Tabs on opening"));
    w.set_set_start_note(t(
        cat,
        "A new screen of one empty tab, the tabs kept with an empty one in front, or the tabs as they were left.",
    ));
    w.set_start_labels(list(&["New screen", "New empty tab", "Last active tab"]));
    w.set_set_switch(t(cat, "Open another face now"));
    w.set_set_theme(t(cat, "Theme"));
    w.set_set_theme_note(t(cat, "Follows the desktop unless one is chosen."));
    w.set_theme_labels(list(&["The desktop's", "Light", "Dark"]));
    w.set_set_layout(t(cat, "Layout"));
    w.set_layout_labels(list(&["Details", "Icons", "Large icons"]));
    w.set_set_columns(t(cat, "Columns"));
    w.set_set_columns_note(t(cat, "Which ones, and their widths back to the default."));
    w.set_set_columns_push(t(cat, "Choose…"));
    w.set_folder_on_labels(list(&["Scanned", "Off"]));
    w.set_folder_watch_labels(list(&["Watched", "Walked only"]));
    w.set_set_preview(t(cat, "Preview"));
    w.set_set_preview_note(t(cat, "The column beside the list. Space opens it too."));
    w.set_set_office(t(cat, "Documents in the preview"));
    w.set_set_office_note(t(
        cat,
        "Word, Excel and PowerPoint files: drawn as they look, or as their plain text.",
    ));
    w.set_office_labels(list(&["Drawn", "Text"]));
    w.set_set_peek_means(t(cat, "Previews on this machine"));
    w.set_peek_means(peek_means(cat).into());
    w.set_word_office_drawn(t(cat, "Drawn"));
    w.set_word_office_text(t(cat, "Text"));
    w.set_set_service(t(cat, "Windows service"));
    draw_service(w, cat);
    w.set_set_refresh(t(cat, "Refresh the list every"));
    w.set_set_refresh_note(t(
        cat,
        "While the window is in use, how late a new file may reach the list. Scrolling and searching never wait.",
    ));
    w.set_refresh_labels(list(&["1 s", "2 s", "5 s", "15 s", "1 min"]));
    w.set_word_found(t(cat, "Found in the file"));
    w.set_word_peek_menu(t(cat, scour_ui::preview::MENU_SAID));
    w.set_word_details(t(cat, scour_ui::preview::MORE));
    w.set_word_peek_native(t(cat, scour_ui::preview::NATIVE));
    w.set_word_newest(t(cat, scour_ui::preview::NEWEST_SAID));
    w.set_set_memory(t(cat, "Give memory back"));
    w.set_set_memory_note(t(
        cat,
        "What a search brought into memory is given back once nobody asks. Automatic is at once under 8 GB of memory and after a minute above it; at once costs the next search a little time.",
    ));
    w.set_memory_labels(list(&["Automatic", "At once", "1 min", "5 min"]));
    w.set_set_narrow(t(cat, "Narrowing line"));
    w.set_set_narrow_note(t(
        cat,
        "The suggestions under the query bar. Switched off, a new query asks nothing for them: about half a second of the service's work less.",
    ));
    w.set_set_threads(t(cat, "Cores for one search"));
    w.set_set_threads_note(t(
        cat,
        "How many cores a large search is split over. Fewer leaves the machine more to itself and makes a large search slower.",
    ));
    w.set_threads_labels(list(&["Automatic", "1", "2", "4", "8"]));
    w.set_set_fresh(t(cat, "Up to date within"));
    w.set_set_fresh_note(t(
        cat,
        "While no window is open, how late a change may reach the index. Sooner writes to the disk more often.",
    ));
    w.set_fresh_labels(list(&["As configured", "1 s", "5 s", "15 s", "1 min"]));
    w.set_set_zoom(t(cat, "Zoom"));
    w.set_set_zoom_note(t(
        cat,
        "How big everything is drawn. Ctrl + and Ctrl - too.",
    ));
    let percent = t(cat, "{n}%");
    let labels: Vec<slint::SharedString> = ["90", "100", "110", "125", "150"]
        .into_iter()
        .map(|n| percent.replace("{n}", n).into())
        .collect();
    w.set_zoom_labels(ModelRc::new(VecModel::from(labels)));
    w.set_set_tabs(t(cat, "Search tabs"));
    w.set_set_tabs_note(t(cat, "Several searches side by side. Ctrl+T opens one."));
    w.set_set_system_icons(t(cat, "System icons"));
    w.set_set_system_icons_note(t(
        cat,
        "The desktop's own file and folder icons instead of built-in glyphs.",
    ));
    w.set_set_grid(t(cat, "Row lines"));
    w.set_set_grid_note(t(cat, "A line under every row of the list."));
    w.set_set_grid_columns(t(cat, "Column lines"));
    w.set_set_grid_columns_note(t(cat, "A line between the list's columns."));
    w.set_grid_labels(list(&["On", "Off"]));
    w.set_set_accents(t(cat, "Ignore accents"));
    w.set_set_accents_note(t(
        cat,
        "cafe also finds café. A word typed with an accent still finds only that spelling.",
    ));
    w.set_set_typos(t(cat, "Typing mistakes"));
    w.set_word_narrow(t(cat, "Narrow:"));
    w.set_word_filter_button(t(cat, "Filter"));
    w.set_word_peek(t(cat, "Preview"));
    w.set_word_tab(t(cat, "Tab"));
    w.set_word_add_filter(t(cat, "Add a filter"));
    w.set_word_filter_key(t(cat, "filter"));
    w.set_word_tab_key(t(cat, "tab"));
    w.set_set_rail(t(cat, "Sidebar"));
    w.set_set_rail_note(t(
        cat,
        "Places, types and sizes beside the list. The filter strip and the lens do the same from the bar.",
    ));
    w.set_set_ribbon(t(cat, "Time ribbon"));
    w.set_set_ribbon_note(t(cat, "How old the results are, as bars under the list."));
    w.set_typos_said(typos_said(cat, w.get_typos_choice().as_str()).into());
    w.set_set_typos_note(t(
        cat,
        "A letter wrong, missing, extra or swapped, in a word of five letters or more.",
    ));
    w.set_typos_labels(list(&["Off", "When nothing is found", "Always"]));
    w.set_set_typos_rule(t(cat, "Where a mistake is forgiven"));
    w.set_set_typos_rule_note(t(
        cat,
        "From a word's start: anywhere in a word, as long as the word begins where the match does. \
         Strict: never in the first four letters, so a longer word never finds more than a shorter one.",
    ));
    w.set_typos_rule_labels(list(&["From a word's start", "Strict"]));
    w.set_set_field_words(t(cat, "Filter headers"));
    w.set_set_field_words_note(t(
        cat,
        "A filter's name in the query drawn as a picture — a clock for when, a folder for where — or written out. The caret in it writes it out either way.",
    ));
    w.set_field_words_labels(list(&["Pictures", "Words"]));
    w.set_word_calendar(t(cat, "Calendar"));
    w.set_cal_hint(t(
        cat,
        "Press the first day, then the last. The same day twice is that day alone.",
    ));
    w.set_cal_week(list(&scour_ui::calendar::WEEKDAYS));
    w.set_set_search_hint(t(
        cat,
        "letters:scrgui finds scour-gui: those letters in that order, with anything between. fuzzy:word forgives one word its mistakes.",
    ));
    w.set_similar_show(t(cat, "Show similar (Enter)"));
    w.set_word_similar_always(t(cat, "Always forgive typos"));
    w.set_word_loc(t(cat, "Location"));
    w.set_word_time(t(cat, "Modified"));
    w.set_word_fuzzy(t(cat, "Fuzzy"));
    w.set_word_grep(t(cat, "Grep"));
    w.set_word_grep_ph(t(cat, "Search inside matching files… (a;b  !c)"));
    w.set_word_text(t(cat, "Text"));
    w.set_word_regex(t(cat, "Regex"));
    w.set_word_hex(t(cat, "Hex"));
    w.set_word_read_more(t(cat, "Read more"));
    crate::placewire::words(w, cat);
    w.set_word_choose_folder(t(cat, "Choose folder…"));
    w.set_word_this_folder(t(cat, "this folder"));
    w.set_word_subfolders(t(cat, "Subfolders"));
    w.set_word_direct_only(t(cat, "only what is directly inside"));
    w.set_word_spark_hint(t(cat, "Click or drag across bars to pick a range"));
    w.set_word_narrow_pill(t(cat, "Narrow"));
    w.set_word_inside(t(cat, "Inside"));
    w.set_set_later(t(cat, "Disks that come later"));
    w.set_set_removable(t(cat, "A disk plugged in"));
    w.set_set_removable_note(t(cat, "A stick, an external disk, a card."));
    w.set_removable_labels(list(&["Ask", "Always scan", "Never"]));
    w.set_set_every(t(cat, "Walk again every"));
    w.set_set_every_note(t(
        cat,
        "For a folder nothing reports changes for: a share whose server sends none, or one set to walk only.",
    ));
    w.set_every_labels(list(&["1 min", "5 min", "10 min", "30 min", "1 hour"]));
    w.set_set_network(t(cat, "Network disks"));
    w.set_set_network_note(t(
        cat,
        "SMB, NFS, SSHFS. Walked again as often as the folders below.",
    ));
    w.set_network_labels(list(&["Ask", "Never"]));
    w.set_set_optical(t(cat, "CD and DVD discs"));
    w.set_set_optical_note(t(cat, "Left out unless chosen: a disc is slow to read."));
    w.set_optical_labels(list(&["Never", "Ask", "Always scan"]));
    w.set_set_folders(t(cat, "Folders added"));
    w.set_set_folders_note(t(
        cat,
        "One folder rather than a whole disk: a folder on a disk left out, say. One already inside a scanned disk adds nothing.",
    ));
    w.set_folder_hint(t(cat, "/a/folder or ~/a/folder"));
    w.set_folder_add(t(cat, "add"));
}

/// A list of strings as a model.
pub(super) fn strings(list: &[String]) -> ModelRc<slint::SharedString> {
    ModelRc::new(VecModel::from(
        list.iter()
            .map(|s| s.as_str().into())
            .collect::<Vec<slint::SharedString>>(),
    ))
}

/// A folder typed into the settings, made absolute and checked, or what is
/// wrong with it as a catalogue key. `~` is the home folder.
pub(super) fn folder_to_add(typed: &str, have: &[String]) -> Result<String, &'static str> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Err("Type a folder's path first.");
    }
    let path = match typed.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}{rest}")
        }
        _ => typed.to_owned(),
    };
    let native = std::path::Path::new(&path);
    if !native.is_absolute() {
        return Err("A folder's full path, from the root.");
    }
    if !native.is_dir() {
        return Err("There is no folder there.");
    }
    let path = scour_core::path::from_path(native);
    let path = path.trim_end_matches('/').to_owned();
    if have.contains(&path) {
        return Err("That folder is already in the list.");
    }
    Ok(if path.is_empty() { "/".into() } else { path })
}

pub(super) fn words(window: &MainWindow, cat: &Catalogue) {
    disks::words(window, cat);
    // Punctuation is part of the language, and this runs whenever it changes.
    MARKS.with(|m| {
        m.set((
            scour_ui::format::group_mark(cat.language()),
            scour_ui::format::decimal_mark(cat.language()),
        ))
    });
    columns(window, cat);
    let kinds: Vec<String> = scour_core::Kind::ALL
        .iter()
        .map(|k| cat.get(k.msgid()).to_string())
        .collect();
    window.set_kind_words(kinds.join("\n").into());
    window.set_tab_search(t(cat, "Search"));
    window.set_tab_report(t(cat, "Report"));
    // The rail's first section is the kinds, and the page calls it `Kind`.
    window.set_scope_label(t(cat, "Kind"));
    window.set_scope_heading(t(cat, "Scope"));
    window.set_size_heading(t(cat, "Size"));
    // Three bands, fixed rather than counted: a count is a walk per band.
    let sizes: Vec<Facet> = [
        (">10 MB", "size:>10mb"),
        (">1 MB", "size:>1mb"),
        ("= 0", "size:=0"),
    ]
    .iter()
    .map(|(label, token)| Facet {
        label: (*label).into(),
        token: (*token).into(),
        count: slint::SharedString::new(),
        share: 0.0,
        icon: Default::default(),
    })
    .collect();
    window.set_sizes(ModelRc::new(VecModel::from(sizes)));
    window.set_ribbon_label(t(cat, "Time distribution"));
    window.set_help_title(t(cat, "Help"));
    window.set_faces_title(t(cat, "how to run it"));
    window.set_face_window(t(cat, "Window"));
    window.set_face_window_aside(t(cat, "running now"));
    window.set_face_tui(t(cat, "Terminal"));
    window.set_face_tui_note(t(
        cat,
        "Opens a terminal and runs Scour in it. This window stays where it is.",
    ));
    window.set_face_tui_aside(t(cat, "not built yet"));
    window.set_face_web(t(cat, "Browser"));
    // The sentence says what it opens first: a listening port is not what somebody
    // asked for when they asked for a browser.
    window.set_face_web_note(t(
        cat,
        "Starts scour-web, which listens on 127.0.0.1:7621 and opens your browser. Nothing outside this computer can reach it, and the link carries a token that changes every run.",
    ));
    window.set_face_go(t(cat, "open"));
    window.set_restart_as(t(cat, "Scour opens again as:"));
    window.set_word_restart(t(cat, "Restart"));
    // The terminal one is offered when there is something to run.
    window.set_face_tui_ready(scour_launch::face("tui").is_some());
    window.set_report_web(t(cat, "Open the full report in the browser"));
    window.set_report_web_ready(scour_launch::face("browser").is_some());
    window.set_lang_title(t(cat, "language"));
    window.set_rules_title(t(cat, "What is skipped"));
    window.set_rules_added_title(t(cat, "Added here"));
    window.set_rules_added_note(t(
        cat,
        "The only ones this window may delete. Any rule can be switched off.",
    ));
    window.set_rules_config_title(t(cat, "From config.toml"));
    window.set_rules_config_note(t(
        cat,
        "Written by hand, and left alone — deleting it here would rewrite the file. Switch it off instead.",
    ));
    window.set_rules_builtin_title(t(cat, "Built in"));
    window.set_rules_builtin_note(t(
        cat,
        "Part of the program. Mostly build output and package caches, which churn constantly and bury real results.",
    ));
    hotkey::words(window, cat);
    settings_words(window, cat);
    wiring::resources::words(window, cat);
    window.set_rules_hint(t(cat, "a directory name, or a path"));
    window.set_rules_add(t(cat, "add"));
    window.set_rules_off_word(t(cat, "off"));
    // The terms, then the keys: both are what clicking cannot teach. Text as
    // it is — a `<` in `size:` is a comparison, not markup.
    window.set_help_body(
        [
            "A word matches the name. Several words mean all of them.",
            "",
            "  !word           leave it out",
            "  \"two words\"     as written",
            "  *.pdf   rep?rt  wildcards",
            "  a | b           either one",
            "",
            "  ext:pdf         extension",
            "  kind:image      image, video, code, doc, archive, folder…",
            "  size:>10mb      also <, and kb mb gb",
            "  dm:7d           changed in the last 7 days — h, w, m, y too",
            "  dc: da:         created, opened",
            "  under:/home/a   inside that folder",
            "  folder:  file:  folders only, files only",
            "  fuzzy:raporr    give or take a typing mistake",
            "  letters:scrgui  those letters in that order: scour-gui",
            "",
            "Keys",
            "",
            "  Enter           open        Ctrl+Enter   open its folder",
            "  Space           details     Alt+↓        search this folder",
            "  F2              rename      Delete       to trash",
            "  Ctrl+C          copy path   Ctrl+Shift+C copy name",
            "  ↑ ↓ PgUp PgDn   move        Home End     first, last",
            "  Escape          clear the selection, or close this",
            "",
            "The colour down the left of each row is how long ago it changed: warm for minutes, cold for years.",
        ]
        .iter()
        .map(|line| if line.is_empty() { String::new() } else { t(cat, line).to_string() })
        .collect::<Vec<_>>()
        .join("\n")
        .into(),
    );
    let langs: Vec<Facet> = [("English", "en"), ("Türkçe", "tr")]
        .iter()
        .map(|(label, tag)| Facet {
            label: (*label).into(),
            token: (*tag).into(),
            count: slint::SharedString::new(),
            share: 0.0,
            icon: Default::default(),
        })
        .collect();
    window.set_languages(ModelRc::new(VecModel::from(langs)));
    window.set_ribbon_hint(t(cat, "results by date changed"));
    window.set_axis_oldest(t(cat, "2 years ago"));
    window.set_axis_year(t(cat, "1 year"));
    window.set_axis_month(t(cat, "1 month"));
    window.set_axis_week(t(cat, "1 week"));
    window.set_axis_today(t(cat, "today"));
}

/// Hand the window its palette, from `scour-ui`. Both schemes are pushed: which
/// applies is Slint's to decide.
pub(super) fn dress(window: &MainWindow) {
    let theme = window.global::<Theme>();
    theme.set_dark_scheme(scheme(&scour_ui::DARK));
    theme.set_light_scheme(scheme(&scour_ui::LIGHT));
    theme.set_unit(scour_ui::METRICS.unit);
    theme.set_row_height(scour_ui::METRICS.row);
    window.set_base_row(scour_ui::METRICS.row);
    theme.set_radius(scour_ui::METRICS.radius);

    let fonts = window.global::<Fonts>();
    fonts.set_size(scour_ui::METRICS.size);
    fonts.set_mono(mono_family().as_str().into());
}

/// A monospace face this machine actually has. **"monospace" is not a family,
/// and Slint does not treat it as one**: parley falls through to a sans, where
/// `M` is 18.86 px wide and `i` 5.98 under a layer drawn on top of it.
pub(crate) fn mono_family() -> String {
    if let Ok(named) = std::env::var("SCOUR_GUI_MONO") {
        return named;
    }
    #[cfg(target_os = "macos")]
    return "Menlo".to_owned();
    #[cfg(windows)]
    return "Consolas".to_owned();
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let asked = std::process::Command::new("fc-match")
            .args(["-f", "%{family}", "monospace"])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            // fontconfig answers with every name the family goes by.
            .and_then(|names| names.split(',').next().map(str::trim).map(str::to_owned))
            .filter(|name| !name.is_empty());
        trace(&format!("mono family: {asked:?}"));
        // No fontconfig: the old string, which draws in the sans.
        asked.unwrap_or_else(|| "monospace".to_owned())
    }
}

/// The right-click menu for one shape of selection, as `scour_ui::menu` splits
/// it: the strip of short words across the top, and the list under it.
pub(super) fn menu_parts(
    cat: &Catalogue,
    picked: usize,
    is_dir: bool,
) -> (Vec<MenuItem>, Vec<MenuItem>) {
    use scour_ui::menu::Weight;
    let all: Vec<_> =
        scour_ui::menu::items_for(picked, is_dir, scour_ui::faces::Face::Window).collect();
    let n = grouped(picked.max(1) as u64);
    let strip = scour_ui::menu::strip(&all)
        .into_iter()
        .map(|item| MenuItem {
            id: item.id.into(),
            label: t(cat, item.short),
            key: item.key.into(),
            careful: item.weight == Weight::Careful,
            icon: item.icon.into(),
            ..Default::default()
        })
        .collect();
    let mut last: Option<u8> = None;
    let list = scour_ui::menu::list(&all)
        .into_iter()
        .map(|item| {
            let rule = last.is_some_and(|l| l != item.group);
            last = Some(item.group);
            MenuItem {
                id: item.id.into(),
                // Through the catalogue's own `{n}`, wherever it puts it.
                label: t(cat, item.msgid).replace("{n}", &n).into(),
                key: item.key.into(),
                rule,
                careful: item.weight == Weight::Careful,
                heavy: item.weight == Weight::Heavy,
                icon: item.icon.into(),
                ..Default::default()
            }
        })
        .collect();
    (strip, list)
}

/// The menu's heading for one row: its name, where it is and what it weighs,
/// beside its kind's drawing. A folder's weight is not known without asking.
pub(super) fn menu_heading(w: &MainWindow, path: &str, is_dir: bool, bytes: i64) {
    let name = scour_ui::path::leaf(path);
    let kind = scour_core::kind_of(is_dir, name, 0);
    let folder = tray::home_tilde(scour_ui::path::folder(path));
    w.set_menu_title(name.into());
    w.set_menu_detail(if is_dir || bytes < 0 {
        folder.into()
    } else {
        format!(
            "{folder} · {}",
            scour_ui::format::size(bytes as u64, marks().1)
        )
        .into()
    });
    w.set_menu_ktoken(kind.token().into());
    w.set_menu_tint(rows::tint_of(kind.token()));
}

/// The menu's heading for a selection: how many, of which kind, and what the
/// files among them come to.
pub(super) fn menu_heading_many(w: &MainWindow, cat: &Catalogue, picks: &Selection) {
    let dirs = picks.rows.values().filter(|p| p.is_dir).count();
    let files = picks.rows.len() - dirs;
    let bytes: i64 = picks
        .rows
        .values()
        .filter(|p| !p.is_dir)
        .map(|p| p.bytes.max(0))
        .sum();
    let mut said = Vec::new();
    if files > 0 {
        said.push(t(cat, "{n} files").replace("{n}", &grouped(files as u64)));
    }
    if dirs > 0 {
        said.push(t(cat, "{n} folders").replace("{n}", &grouped(dirs as u64)));
    }
    if files > 0 {
        said.push(scour_ui::format::size(bytes as u64, marks().1));
    }
    w.set_menu_title(
        t(cat, "{n} selected")
            .replace("{n}", &grouped(picks.rows.len() as u64))
            .into(),
    );
    w.set_menu_detail(said.join(" · ").into());
    w.set_menu_ktoken("stack".into());
    w.set_menu_tint(rows::tint_of(""));
}

/// The text between two byte offsets, in either order, or nothing when they
/// meet or fall inside a character.
pub(super) fn span_of(text: &str, a: i32, b: i32) -> Option<&str> {
    let (from, to) = (a.min(b).max(0) as usize, a.max(b).max(0) as usize);
    text.get(from..to.min(text.len())).filter(|s| !s.is_empty())
}

pub(super) fn compact_bytes(n: u64) -> String {
    scour_ui::format::compact_bytes(n, marks().1)
}

/// How this window is punctuating numbers, set when the language is.
pub(super) fn marks() -> (char, char) {
    MARKS.with(std::cell::Cell::get)
}

/// A number a person can read: `5356281` becomes `5.356.281`.
pub(super) fn grouped(n: u64) -> String {
    scour_ui::format::grouped(n, marks().0)
}

/// The language the window should speak: what a face's menu chose
/// (`scour_settings::Settings::language`), then the config, then the desktop.
pub(crate) fn language(kept: &scour_settings::Settings, cfg: &scour_config::Config) -> String {
    scour_i18n::choose(&kept.language, &cfg.ui.language)
}

/// What shows previews here, as the settings say it: the same words as the
/// page's, from `scour_preview::means`.
fn peek_means(cat: &Catalogue) -> String {
    let m = scour_preview::means(scour_preview::quicklook(None).as_deref());
    let or = |s: &str, none: &str| {
        if s.is_empty() {
            t(cat, none).to_string()
        } else {
            s.to_owned()
        }
    };
    format!(
        "{}: {} · {}: {} · {}: {} · {}",
        t(cat, "PDF pages"),
        or(&m.pdf_pages, "none"),
        t(cat, "Office pages"),
        or(&m.office_pages, "Scour's own drawing"),
        t(cat, "Quick look (Space)"),
        or(&m.quick_look, "none"),
        t(cat, "Text in UTF-8, UTF-16 and Windows-1254"),
    )
}
