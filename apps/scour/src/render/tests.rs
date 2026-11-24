use super::*;

#[test]
fn a_name_cannot_talk_to_the_terminal_it_is_shown_on() {
    let clip = "a\x1b]52;c;ZWNobyBoaQ==\x07b.txt";
    assert_eq!(quoted(clip, true), "a?]52;c;ZWNobyBoaQ==?b.txt");
    assert_eq!(quoted("line\nbreak", true), "line?break");
    // A pipe is a program, which wants the name it can open.
    assert_eq!(quoted(clip, false), clip);
    assert!(matches!(
        quoted("kütüphane", true),
        std::borrow::Cow::Borrowed(_)
    ));
}

/// The home the report was drawn from: twelve folders of sixty-three, the
/// live figures `scour-chart`'s own test measures its shares against. Each
/// folder's bytes sit in one age band, so the table's strip is readable;
/// the scope's own six are what the block draws.
fn home() -> scour_core::UsageResponse {
    const FOLDERS: [(&str, u64, u64); 12] = [
        ("Projeler", 651_571_270_524, 896_648),
        (".config", 31_643_455_926, 98_380),
        (".local", 15_738_588_363, 283_461),
        ("İndirilenler", 14_110_521_835, 52),
        ("Android", 10_863_393_896, 75_921),
        (".AffinityLinux", 10_852_991_366, 27_703),
        (".rustup", 9_242_344_819, 244_494),
        (".android", 8_271_160_692, 123),
        ("vm", 7_898_028_191, 7),
        (".gradle", 7_372_347_728, 56_897),
        (".cargo", 5_889_237_540, 209_217),
        ("pdflab", 4_856_318_717, 20_156),
    ];
    // 14.3, 3.4, 64.8, 15.4, 0.5, 1.7 percent of the whole, to the byte.
    let age = [
        114_251_175_399,
        27_164_615_130,
        516_926_646_736,
        123_039_727_353,
        3_994_796_343,
        13_582_307_565,
    ];
    scour_core::UsageResponse {
        root: scour_core::DirUsage {
            path: "/home/hasan".into(),
            bytes: 798_959_268_526,
            disk: 798_959_268_526,
            files: 2_124_324,
            age,
        },
        children: FOLDERS
            .iter()
            .map(|(name, bytes, files)| scour_core::DirUsage {
                path: format!("/home/hasan/{name}"),
                bytes: *bytes,
                disk: *bytes,
                files: *files,
                age: [*bytes, 0, 0, 0, 0, 0],
            })
            .collect(),
        child_count: 63,
        took_us: 821_000,
    }
}

/// The block a terminal gets, to the column: the mock-up's picture, in
/// block characters. Words come through the catalogue, so this reads the
/// same in any language; the columns are the point and they are literal.
/// A hundred and twenty columns, where all six bands fit — eighty is the
/// test below.
#[test]
fn the_block_a_terminal_gets_is_the_report_drawn_in_cells() {
    let d = scour_ui::format::decimal_mark(catalogue().language());
    let g = scour_ui::format::group_mark(catalogue().language());
    // Every band is over a percent here, so every band is named.
    let legend = [
        ("today", 14),
        ("this week", 3),
        ("this month", 65),
        ("six months", 15),
        ("this year", 1),
        ("older", 2),
    ]
    .iter()
    .map(|(band, share)| format!("{} {share}", t(band)))
    .collect::<Vec<_>>()
    .join(" · ");
    let expect = [
        format!(
            "       744 GiB   2{g}124{g}324 {}   /home/hasan",
            t("files")
        ),
        // Twenty cells: 3 today, 1 this week, 13 this month, 3 six months.
        format!("   {}   {legend}", "▇".repeat(20)),
        String::new(),
        format!("       607 GiB  81{d}5%   {}  Projeler", "█".repeat(20)),
        format!("      29.5 GiB   4{d}0%   █                     .config"),
        format!("      14.7 GiB   2{d}0%   ▌                     .local"),
        format!("      13.1 GiB   1{d}8%   ▍                     İndirilenler"),
        format!("      10.1 GiB   1{d}4%   ▍                     Android"),
        format!("      10.1 GiB   1{d}4%   ▍                     .AffinityLinux"),
        format!("      8.61 GiB   1{d}1%   ▎                     .rustup"),
        format!("      7.70 GiB   1{d}0%   ▎                     .android"),
        format!("      7.36 GiB   1{d}0%   ▎                     vm"),
        format!("      6.87 GiB   0{d}9%   ▎                     .gradle"),
        format!("      5.48 GiB   0{d}7%   ▏                     .cargo"),
        format!("      4.52 GiB   0{d}6%   ▏                     pdflab"),
        format!("       …           12 / 63 {}", t("folders, largest first")),
        t("a hard-linked file is counted once, like du"),
        String::new(),
    ]
    .join("\n");
    assert_eq!(usage_block(&home(), false, 120), expect);
}

/// Into a pipe: the table that was there before any of this, byte for byte.
#[test]
fn a_pipe_gets_the_table_it_always_got() {
    let expect = [
        format!(
            "{:>10}  {:>10}  {:>9}  /home/hasan",
            t("size"),
            t("on disk"),
            t("files")
        ),
        "   744 GiB     744 GiB    2124324  ▁▁▇▁▁▁  /home/hasan".into(),
        "   607 GiB     607 GiB     896648  █       /home/hasan/Projeler".into(),
        "  29.5 GiB    29.5 GiB      98380  █       /home/hasan/.config".into(),
        "  14.7 GiB    14.7 GiB     283461  █       /home/hasan/.local".into(),
        "  13.1 GiB    13.1 GiB         52  █       /home/hasan/İndirilenler".into(),
        "  10.1 GiB    10.1 GiB      75921  █       /home/hasan/Android".into(),
        "  10.1 GiB    10.1 GiB      27703  █       /home/hasan/.AffinityLinux".into(),
        "  8.61 GiB    8.61 GiB     244494  █       /home/hasan/.rustup".into(),
        "  7.70 GiB    7.70 GiB        123  █       /home/hasan/.android".into(),
        "  7.36 GiB    7.36 GiB          7  █       /home/hasan/vm".into(),
        "  6.87 GiB    6.87 GiB      56897  █       /home/hasan/.gradle".into(),
        "  5.48 GiB    5.48 GiB     209217  █       /home/hasan/.cargo".into(),
        "  4.52 GiB    4.52 GiB      20156  █       /home/hasan/pdflab".into(),
        format!("12 / 63 {}", t("folders, largest first")),
        t("a hard-linked file is counted once, like du"),
        String::new(),
    ]
    .join("\n");
    assert_eq!(usage_table(&home()), expect);
}

/// A band too small to round to a percent keeps its cells and loses its
/// number, and the strip is twenty cells whatever the split.
#[test]
fn the_legend_names_only_the_bands_worth_a_number() {
    let age = [700, 299, 1, 0, 0, 0];
    let said = age_strip(&age, false, 80).expect("bytes have an age");
    assert_eq!(
        said,
        format!(
            "   {}   {} 70 · {} 30\n",
            "▇".repeat(20),
            t("today"),
            t("this week")
        ),
        "the third band is a tenth of a percent"
    );
    assert_eq!(age_strip(&[0; 6], false, 80), None, "nothing to draw");
}

/// The line never wraps. Six bands over a percent fit in a hundred and
/// twenty columns; in eighty they do not, and the smallest go — one at a
/// time, until what is left fits and the next one back would not.
#[test]
fn a_legend_wider_than_the_terminal_loses_its_smallest_bands() {
    let age = home().root.age;
    // What is written after the strip, at that width.
    let legend = |width: usize| {
        let said = age_strip(&age, false, width).expect("bytes have an age");
        let line = said.trim_end_matches('\n');
        assert!(
            line.chars().count() <= width,
            "wrapped at {width}: {line:?}"
        );
        line.chars().skip(GUTTER + BAR + GUTTER).collect::<String>()
    };

    let all = legend(120);
    let named = |legend: &str| -> Vec<usize> {
        legend
            .split(" · ")
            .filter(|part| !part.is_empty())
            .map(|part| {
                BANDS
                    .iter()
                    .position(|band| part.starts_with(&t(band)))
                    .unwrap_or_else(|| panic!("no band in {part:?}"))
            })
            .collect()
    };
    assert_eq!(named(&all), vec![0, 1, 2, 3, 4, 5], "all six at 120");

    let cut = legend(80);
    let kept = named(&cut);
    assert!(kept.len() < 6, "nothing was dropped at 80: {cut:?}");
    assert!(kept.windows(2).all(|w| w[0] < w[1]), "still in age order");
    // What went is smaller than anything that stayed, and the largest of
    // those would not have fitted either.
    let gone: Vec<usize> = (0..6).filter(|b| !kept.contains(b)).collect();
    let smallest_kept = kept.iter().map(|b| age[*b]).min().expect("some kept");
    assert!(
        gone.iter().all(|b| age[*b] <= smallest_kept),
        "a larger band was dropped before a smaller one: {gone:?}"
    );
    let biggest_gone = gone.iter().max_by_key(|b| age[**b]).expect("some gone");
    let one_more = format!(" · {} 1", t(BANDS[*biggest_gone]));
    assert!(
        GUTTER + BAR + GUTTER + cut.chars().count() + one_more.chars().count() > 80,
        "one more band would have fitted: {cut:?}"
    );
}

/// Colour is a run of cells in the band's own colour, and `false` is the
/// same picture with no escape in it at all.
#[test]
fn the_cells_are_painted_in_the_bands_own_colours() {
    let age = [700, 299, 1, 0, 0, 0];
    let painted = age_strip(&age, true, 80).expect("bytes have an age");
    let c = scour_ui::DARK.t[0];
    assert!(
        painted.contains(&format!(
            "\x1b[38;2;{};{};{}m{}\x1b[0m",
            c.r,
            c.g,
            c.b,
            "▇".repeat(14)
        )),
        "today's cells in today's colour: {painted:?}"
    );
    assert!(
        !age_strip(&age, false, 80).expect("drawn").contains('\x1b'),
        "plain means plain"
    );
}

/// The groups, with the bar a terminal gets and the percentage beside the
/// count. The keys are extensions, so no word here is translated.
fn kinds() -> scour_core::FacetResponse {
    let keys = ["rs", "md", "png", "pdf", "txt", "zip", "ico"];
    let counts = [75_090u64, 49_295, 41_307, 16_128, 5_248, 4_786, 8_146];
    scour_core::FacetResponse {
        facets: keys
            .iter()
            .zip(counts)
            .map(|(key, count)| scour_core::Facet {
                key: (*key).to_owned(),
                count,
            })
            .collect(),
        by: FacetBy::Ext { top: 10 },
        total: counts.iter().sum(),
        ..Default::default()
    }
}

#[test]
fn the_groups_get_a_bar_and_a_share_at_a_terminal() {
    let d = scour_ui::format::decimal_mark(catalogue().language());
    let expect = [
        // Forty cells: 15, 10, 8, 3, 1, 1, 2, in the order they are listed.
        "▓".repeat(40),
        format!("rs        75090  37{d}5%"),
        format!("md        49295  24{d}6%"),
        format!("png       41307  20{d}7%"),
        format!("pdf       16128   8{d}1%"),
        format!("txt        5248   2{d}6%"),
        format!("zip        4786   2{d}4%"),
        format!("ico        8146   4{d}1%"),
        String::new(),
    ]
    .join("\n");
    assert_eq!(facets_said(&kinds(), true, false), expect);
}

/// The same reply into a pipe: the two columns it printed before.
#[test]
fn a_pipe_gets_the_two_columns_it_always_got() {
    let expect = [
        "rs        75090",
        "md        49295",
        "png       41307",
        "pdf       16128",
        "txt        5248",
        "zip        4786",
        "ico        8146",
        "",
    ]
    .join("\n");
    assert_eq!(facets_said(&kinds(), false, false), expect);
}

/// The six largest take the ring's six steps; whatever is left over takes
/// the colour the ring gives the rest — `zip` here, the smallest of seven.
#[test]
fn the_six_largest_groups_take_the_rings_six_steps() {
    let counts: Vec<u64> = kinds().facets.iter().map(|x| x.count).collect();
    let said = facet_bar(&counts, true).expect("something to draw");
    let rest = scour_ui::DARK.kx;
    assert!(
        said.contains(&format!(
            "\x1b[38;2;{};{};{}m▓\x1b[0m",
            rest.r, rest.g, rest.b
        )),
        "the seventh is the rest: {said:?}"
    );
    assert_eq!(facet_bar(&[0, 0], false), None, "nothing to draw");
}

/// In the person's zone: the format is pinned through the pure half, the
/// zone through the same call the code uses.
#[test]
fn timestamps_read_as_dates() {
    use scour_ui::format::{local_offset, stamp_at};
    assert_eq!(stamp(0), "—", "an unknown time is not 1970");
    let at = 1_769_817_600;
    assert_eq!(stamp_at(at, 0), "2026-01-31 00:00", "the shape, zone-free");
    assert_eq!(stamp(at), stamp_at(at, local_offset(at)));
    assert_eq!(
        stamp(at + 3_661),
        stamp_at(at + 3_661, local_offset(at + 3_661))
    );
}

#[test]
fn every_kind_has_a_word_in_every_shipped_language() {
    for (tag, _) in scour_i18n::LANGUAGES {
        let c = scour_i18n::Catalogue::for_language(tag);
        for k in Kind::ALL {
            let word = c.get(k.msgid());
            assert!(!word.is_empty(), "{tag} has no word for {k:?}");
        }
    }
}

/// Every string this file asks for exists in the Turkish catalogue; at run
/// time a missing one falls back to English rather than failing.
#[test]
fn the_turkish_catalogue_covers_what_this_file_uses() {
    let c = scour_i18n::Catalogue::for_language("tr");
    let used = [
        "path",
        "name",
        "kind",
        "size",
        "on disk",
        "items",
        "modified",
        "created",
        "accessed",
        "mode",
        "entries",
        "folders",
        "index",
        "sources",
        "watching",
        "scanning",
        "seen so far",
        "pending",
        "unsorted",
        "segments",
        "contents",
        "yes",
        "no",
        "indexed",
        "not indexed",
        "running",
        "not running",
        "settings",
        "socket",
        "service",
        "problem",
        "desktop",
        "key",
        "command",
        "not bound",
        "This desktop cannot be bound from here. Bind a key of your choice to:",
        "It takes effect after the next login.",
        "accepted",
        "full scan",
        "paths built",
        "identical",
        "same ends",
        "same size only",
        "confirmed",
        "could be freed",
        "candidates",
        "read",
        "groups were not confirmed: raise --budget-mb",
        "warning",
        "columns",
        "sort",
        "history",
        "files",
        "folders, largest first",
        "a hard-linked file is counted once, like du",
        "counts are a lower bound: the scan hit its cap",
        "today",
        "this week",
        "this month",
        "six months",
        "this year",
        "older",
        "was searched for as text",
        "a rebuild would speed searches up",
        "The index is empty. Run `scour rescan`.",
        "reads the files the rest of the query names, so it takes longer",
        "Start `scourd` first — the MCP server is a client of it, like this one.",
    ];
    let missing: Vec<&str> = used.iter().copied().filter(|m| c.get(m) == **m).collect();
    assert!(missing.is_empty(), "untranslated: {missing:?}");
}

/// Writing a script and starting one are one race: a fork on another test's
/// thread inherits the file still open for writing, and the exec then says
/// `Text file busy`. The tests that start a process take turns.
#[cfg(unix)]
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A `gsettings` keeping its settings in a file beside itself, as
/// `scour-hotkey`'s own tests use. Never this machine's desktop.
#[cfg(unix)]
fn stub_gsettings(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("gsettings");
    std::fs::write(
        &script,
        r#"#!/bin/bash
db="$(dirname "$0")/db"; touch "$db"
case "$1" in
  get)
    line=$(grep -F -- "$2 $3=" "$db" | head -1)
    if [ -n "$line" ]; then printf '%s\n' "${line#*=}"
    elif [ "$3" = custom-keybindings ]; then echo "@as []"
    else echo "''"; fi ;;
  set)
    grep -v -F -- "$2 $3=" "$db" > "$db.new" || true
    printf '%s %s=%s\n' "$2" "$3" "$4" >> "$db.new"; mv "$db.new" "$db" ;;
  reset-recursively)
    grep -v -F -- "$2 " "$db" > "$db.new" || true; mv "$db.new" "$db" ;;
  *) echo "stub: $*" >&2; exit 1 ;;
esac
"#,
    )
    .expect("write stub");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    script
}

#[cfg(unix)]
fn gnome(dir: &std::path::Path) -> scour_hotkey::Hotkey {
    scour_hotkey::Hotkey::new(scour_hotkey::Tools {
        gsettings: Some(stub_gsettings(dir)),
        gnome_keys: true,
        desktop_var: "GNOME".into(),
        ..Default::default()
    })
}

/// Unbound, bound, and bound-then-cleared, in both forms.
#[cfg(unix)]
#[test]
fn the_key_reads_the_same_way_before_and_after_it_is_set() {
    let _turn = one_at_a_time();
    let dir = tempfile::tempdir().expect("tempdir");
    let hk = gnome(dir.path());

    // Built with the same helpers, not spelled out: this process speaks
    // whatever the machine's locale is, and the shape is what is being
    // checked.
    let block = |key: String| {
        format!(
            "{}GNOME\n{}{key}\n{}scour-open\n",
            label("desktop"),
            label("key"),
            label("command")
        )
    };

    let said = hotkey_said(&hk, Deed::Show, false);
    assert_eq!(said.code, 0);
    assert_eq!(said.out, block(t("not bound")));

    let said = hotkey_said(&hk, Deed::Set("Super+F"), false);
    assert_eq!(said.code, 0);
    assert_eq!(
        said.out,
        block("super+f".into()),
        "the combination as this crate spells it, not as it was typed"
    );
    // GNOME's is in force at once, so there is nothing to add.
    assert_eq!(said.err, "");
    assert_eq!(hotkey_said(&hk, Deed::Show, false).out, said.out);

    let said = hotkey_said(&hk, Deed::Clear, false);
    assert_eq!(said.code, 0);
    assert_eq!(said.out, block(t("not bound")));
}

/// `--json` is the same five facts without the wording.
#[cfg(unix)]
#[test]
fn the_json_carries_what_a_script_needs() {
    let _turn = one_at_a_time();
    let dir = tempfile::tempdir().expect("tempdir");
    let hk = gnome(dir.path());
    let said = hotkey_said(&hk, Deed::Set("ctrl+alt+s"), true);
    let value: serde_json::Value = serde_json::from_str(&said.out).expect("json");
    assert_eq!(value["desktop"], "gnome");
    assert_eq!(value["can_bind"], true);
    assert_eq!(value["key"], "ctrl+alt+s");
    assert_eq!(value["command"], "scour-open");
    assert_eq!(value["applies"], "now");
    let said = hotkey_said(&hk, Deed::Clear, true);
    let value: serde_json::Value = serde_json::from_str(&said.out).expect("json");
    assert_eq!(value["key"], serde_json::Value::Null);
    assert_eq!(value["applies"], serde_json::Value::Null);
}

#[test]
fn a_combination_that_is_not_one_is_refused_before_anything_is_written() {
    let hk = scour_hotkey::Hotkey::new(scour_hotkey::Tools::default());
    let said = hotkey_said(&hk, Deed::Set("hyper+f"), false);
    assert_eq!(said.code, 2, "not a tool failure");
    assert_eq!(
        said.err,
        format!("{}\n", t("not a modifier: {m}").replace("{m}", "hyper")),
        "the crate's own words, and the value it refused"
    );
    assert!(said.out.is_empty(), "and nothing was said about the state");
}

/// A desktop this program cannot write to says what to bind by hand, and
/// `set` leaves with a code that stops a script.
#[test]
fn a_desktop_that_cannot_be_bound_names_the_command_instead() {
    let hk = scour_hotkey::Hotkey::new(scour_hotkey::Tools {
        desktop_var: "XFCE".into(),
        ..Default::default()
    });
    let said = hotkey_said(&hk, Deed::Show, false);
    assert_eq!(said.code, 0, "being told is not a failure");
    assert_eq!(
        said.out,
        format!(
            "{}other\n{}\n  scour-open\n",
            label("desktop"),
            t("This desktop cannot be bound from here. Bind a key of your choice to:")
        )
    );
    assert_eq!(said.err, "");

    for deed in [Deed::Set("super+f"), Deed::Clear] {
        let said = hotkey_said(&hk, deed, false);
        assert_eq!(said.code, 1, "a script has to be able to see this");
        assert!(said.out.contains("scour-open"), "{}", said.out);
        assert_eq!(said.err, "", "the sentence is not said twice");
    }
    // In JSON the sentence is not printed, so the reason goes to stderr.
    let said = hotkey_said(&hk, Deed::Set("super+f"), true);
    assert_eq!(said.code, 1);
    assert_eq!(
        said.err,
        format!("{}\n", t("this desktop cannot be bound from here"))
    );
}
