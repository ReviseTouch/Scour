use super::*;
use scour_core::SortKey;

/// A name may hold a newline. Split on one, `victim\nsuffix.txt` named
/// `victim`, and the trash took the file nobody had chosen.
#[test]
fn a_path_with_a_newline_in_it_arrives_as_one_path() {
    let posted = |body: String| http::Req {
        method: "POST".into(),
        path: "/api/trash".into(),
        query: Default::default(),
        headers: Default::default(),
        body,
    };
    let names = ["/d/victim\nsuffix.txt", "/d/\"q\" ı İ\r"];
    let req = posted(serde_json::to_string(&names).unwrap());
    assert_eq!(paths_in(&req), names);
    // Anything but an array of strings is no paths, never a guess at some.
    for body in ["", "/d/victim", "{\"paths\":[]}", "[1, 2]"] {
        assert!(paths_in(&posted(body.into())).is_empty(), "{body:?}");
    }
}

#[test]
fn only_this_bridge_by_its_own_names_is_ours() {
    assert!(ours("127.0.0.1:7621", 7621));
    assert!(ours("localhost:7621", 7621));
    assert!(ours("LocalHost:7621", 7621));
    // The old check read only the port, which a rebound name keeps.
    assert!(!ours("evil.example:7621", 7621));
    assert!(!ours("127.0.0.1.evil.example:7621", 7621));
    assert!(!ours("127.0.0.1:7622", 7621));
    assert!(!ours("127.0.0.1", 7621));
    assert!(!ours("", 7621));
}

/// The policy runs the script carrying the nonce and nothing else, so the
/// page must carry it on its one script, and on nothing a name could reach.
#[test]
fn the_pages_one_script_carries_the_nonce_the_policy_names() {
    let served = page();
    assert_eq!(
        served.matches("<script").count(),
        1,
        "a second script would not run"
    );
    assert!(served.contains(&format!("<script nonce=\"{}\">", nonce())));
    assert_eq!(nonce().len(), 32);
    let me = include_str!("main.rs");
    assert!(me.contains(concat!(
        "http::page(&mut stream, page()",
        ".as_bytes(), nonce())"
    )));
}

#[test]
fn two_tokens_from_one_process_are_not_the_same() {
    assert_ne!(token(), token());
    assert_eq!(token().len(), 32);
}

#[test]
fn an_unknown_sort_key_is_the_default_rather_than_an_error() {
    // A sort key nobody recognises shows the list, not a 400.
    assert_eq!(sort_of(Some("zurna")), SortKey::Modified);
    assert_eq!(sort_of(None), SortKey::Modified);
    assert_eq!(sort_of(Some("size")), SortKey::Size);
}

/// Structural: the page is a compiled-in string that nothing here runs, so
/// what is checkable is that each field has one writer, and it is `setTotal`.
#[test]
fn the_length_of_the_list_and_the_empty_message_have_one_writer() {
    for (what, needle) in [
        ("empty.hidden", "empty.hidden ="),
        ("LIST.total", "LIST.total ="),
        ("LIST.exact", "LIST.exact ="),
    ] {
        let n = PAGE.matches(needle).count();
        assert_eq!(
            n, 1,
            "{what} is assigned in {n} places in page.html rather than 1 — \
                 each of them has to move the others, so they belong in `setTotal`"
        );
    }
    // And that the one place is `setTotal`.
    let at = PAGE
        .find("function setTotal(")
        .expect("page.html has no `setTotal`");
    let body = &PAGE[at..];
    let body = &body[..body.find("\n  }").expect("`setTotal` does not end")];
    for needle in [
        "LIST.total =",
        "LIST.exact =",
        "empty.hidden =",
        // The two sweeps say what the cache is good for at the new length.
        "dropBeyond()",
        "dropShort()",
    ] {
        assert!(body.contains(needle), "`setTotal` does not have {needle}");
    }
}

/// A class with no rule is not an error in CSS but a run in the layer's own
/// colour. `text` and `space` have none on purpose: `text` is that colour.
#[test]
fn every_role_the_engine_can_send_has_a_colour() {
    for role in scour_core::Role::ALL {
        // From serde: a second spelling of the same words is how they part.
        let wire = serde_json::to_string(&role).expect("a role serialises");
        let wire = wire.trim_matches('"');
        if matches!(role, scour_core::Role::Text | scour_core::Role::Space) {
            continue;
        }
        // A whole selector: `.r-not` must not answer for `.r-not_a_role`.
        let named = PAGE.match_indices(&format!(".r-{wire}")).any(|(at, m)| {
            PAGE[at + m.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '-')
        });
        assert!(
            named,
            "page.html has no `.qshadow .r-{wire}` rule, so {role:?} is \
                 drawn in whatever colour the layer happens to be"
        );
    }
}

/// The rule is `Role::is_telling`; the page keeps its own set of strings,
/// and this is what stops the two drifting.
#[test]
fn the_page_and_the_engine_agree_on_which_roles_are_telling() {
    let at = PAGE
        .find("const TELLING = new Set([")
        .expect("page.html has no TELLING set");
    let body = &PAGE[at..];
    let body = &body[..body.find("]);").expect("TELLING does not end")];
    for role in scour_core::Role::ALL {
        let wire = serde_json::to_string(&role).expect("a role serialises");
        let wire = wire.trim_matches('"');
        assert_eq!(
            body.contains(&format!("\"{wire}\"")),
            role.is_telling(),
            "the page and `Role::is_telling` disagree about {role:?}"
        );
    }
}

/// And on the words that look like operators and are not.
#[test]
fn the_page_and_the_shared_list_agree_on_the_mistaken_words() {
    let at = PAGE
        .find("const MISTAKEN = new Set([")
        .expect("page.html has no MISTAKEN set");
    let body = &PAGE[at..];
    let body = &body[..body.find("]);").expect("MISTAKEN does not end")];
    for word in scour_ui::MISTAKEN {
        assert!(
            body.contains(&format!("\"{word}\"")),
            "page.html's MISTAKEN has no {word:?}"
        );
    }
    assert_eq!(
        body.matches('"').count() / 2,
        scour_ui::MISTAKEN.len(),
        "page.html's MISTAKEN has words `scour_ui::MISTAKEN` does not"
    );
}

/// The icon beside the name and on the tab is the desktop's, decoded back
/// to the same bytes: a `+` or a raw `#` in a data URL breaks the picture.
#[test]
fn the_page_is_served_with_the_applications_icon() {
    let served = page();
    assert!(!served.contains("@LOGO@"), "a marker was left unreplaced");
    let url = data_url(LOGO);
    assert_eq!(served.matches(&url).count(), 2, "the tab and the corner");
    let body = url.strip_prefix("data:image/svg+xml,").expect("prefix");
    assert!(!body.contains(['+', '#', '"', ' ', '<', '>']), "{body:.80}");
    let mut back = Vec::new();
    let mut bytes = body.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hex: String = bytes.by_ref().take(2).map(char::from).collect();
            back.push(u8::from_str_radix(&hex, 16).expect("hex"));
        } else {
            back.push(b);
        }
    }
    assert_eq!(String::from_utf8(back).expect("utf-8"), LOGO);
}

#[test]
fn the_page_is_served_with_the_disk_words_the_shared_crate_holds() {
    let served = page();
    assert!(
        !served.contains("/* @DISKS@ */"),
        "nothing replaced the marker"
    );
    assert!(served.contains(&format!(
        "\"removable\":\"{}\"",
        scour_ui::disks::kind("removable")
    )));
}

/// A renamed marker makes the injection silently do nothing, and every
/// right-click then draws an empty box.
#[test]
fn the_page_is_served_with_the_menu_the_shared_crate_holds() {
    let served = page();
    assert!(
        !served.contains("/* @MENU@ */"),
        "the marker is still in the served page, so nothing replaced it"
    );
    for item in scour_ui::menu::ITEMS {
        if item.except.contains(&scour_ui::faces::Face::Page) {
            assert!(
                !served.contains(&format!("\"id\":\"{}\"", item.id))
                    || scour_ui::menu::ITEMS.iter().any(
                        |o| o.id == item.id && !o.except.contains(&scour_ui::faces::Face::Page)
                    ),
                "{} cannot be done in a browser and was served anyway",
                item.id
            );
            continue;
        }
        assert!(
            served.contains(&format!(
                "\"msgid\":\"{}\"",
                item.msgid.replace('"', "\\\"")
            )),
            "the page was served without {:?}",
            item.msgid
        );
    }
    // The preview's buttons, each a menu item the page was served.
    assert!(!served.contains("/* @PEEKBUTTONS@ */"));
    assert!(served.contains(r#"{"file":["open","folder","copy-path"]"#));
    // And every drawing the menu names, which the page looks up by name.
    assert!(!served.contains("/* @MENUICONS@ */"));
    for name in scour_ui::menu::ICONS {
        assert!(
            served.contains(&format!("\"{name}\":\"<svg")),
            "no drawing for {name}"
        );
    }
}

/// The preview's drag bounds are the shared ones, as the window's are.
#[test]
fn the_preview_is_dragged_within_the_shared_bounds() {
    assert!(PAGE.contains(&format!(
        "const PEEK_MIN = {};",
        scour_ui::preview::PANEL_MIN
    )));
    assert!(PAGE.contains(&format!(
        "const PEEK_MAX = {};",
        scour_ui::preview::PANEL_MAX
    )));
}

/// The page is a single `<script>`, so one `SyntaxError` anywhere means
/// none of it runs. Skipped where there is no `node`, and it says so.
#[test]
fn the_page_script_parses() {
    let script = PAGE
        .split_once("<script")
        .and_then(|(_, rest)| rest.split_once('>'))
        .and_then(|(_, body)| body.split_once("</script>"))
        .map(|(body, _)| body)
        .expect("page.html has no script");
    let path = std::env::temp_dir().join("scour-page-check.js");
    std::fs::write(&path, script).expect("writing the script out");
    match std::process::Command::new("node")
        .arg("--check")
        .arg(&path)
        .output()
    {
        Ok(out) => assert!(
            out.status.success(),
            "page.html's script does not parse, so none of it would run:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) => eprintln!("the_page_script_parses: skipped, no node here ({e})"),
    }
    let _ = std::fs::remove_file(&path);
}

/// That the marker was replaced — a page still carrying `@THEME@` has no
/// colours at all — and that a variable it uses holds the crate's value.
#[test]
fn the_page_takes_its_palette_from_the_shared_crate() {
    let served = super::page();
    assert!(
        !served.contains("@THEME@"),
        "the marker survived: the page is served with no palette"
    );
    for (name, colour) in [
        ("--ground", scour_ui::DARK.ground),
        ("--q-key", scour_ui::DARK.q_key),
        ("--mark", scour_ui::DARK.mark),
        ("--focus", scour_ui::DARK.focus),
    ] {
        let want = format!("{name}: {};", colour.css());
        assert!(
            served.contains(&want),
            "the served page does not carry `{want}`"
        );
    }
    // And that the route calls it, which is the only check here that
    // fails when the wiring is undone rather than the formatting. Spelled
    // in two pieces: this reads the file it is written in.
    let me = include_str!("main.rs");
    let call = concat!("page()", ".as_bytes()");
    assert!(
        me.contains(call),
        "the `/` route no longer serves `page()`, so the palette is not injected"
    );

    // Both blocks that override the media query, or the switch half works.
    let light = scour_ui::LIGHT.panel.css();
    assert_eq!(
        served.matches(&format!("--panel: {light};")).count(),
        2,
        "the light panel colour should appear in both the media query and \
             the `[data-theme=\"light\"]` block"
    );
}

/// Shared is the list, not the painting: which columns exist, their labels,
/// their sort keys and their widths.
#[test]
fn the_page_shows_the_columns_the_shared_crate_names() {
    for c in scour_ui::COLUMNS {
        let decl = format!("id: \"{}\",", c.id);
        let at = PAGE
            .find(&decl)
            .unwrap_or_else(|| panic!("the page has no `{}` column", c.id));
        // The declaration is one line: id, msgid, sort key, width.
        let line = &PAGE[at..PAGE[at..].find('\n').map(|n| at + n).unwrap_or(PAGE.len())];
        assert!(
            line.contains(&format!("msgid: \"{}\"", c.msgid)),
            "`{}` is called something else here: {line}",
            c.id
        );
        assert!(
            line.contains(&format!("key: \"{}\"", c.sort)),
            "`{}` sorts by something else here: {line}",
            c.id
        );
        assert!(
            line.contains(&format!("w: {},", c.width)),
            "`{}` starts at a different width here: {line}",
            c.id
        );
        // A floor a pixel out is a column that vanishes in one face only.
        for (word, want) in [
            ("min", c.min),
            ("near", u32::from(c.near)),
            ("far", u32::from(c.far)),
            ("max", c.max),
        ] {
            assert!(
                line.contains(&format!("{word}: {want},")),
                "`{}`: {word} is not {want} here: {line}",
                c.id
            );
        }
    }
}

/// The numbers being equal is checked above; this runs the page's own
/// arithmetic under `node` and compares it. No node, no check.
#[test]
fn the_page_shares_the_row_out_exactly_as_the_shared_crate_does() {
    let from = PAGE
        .find("  const NARROW = 700, WIDE = 1900;")
        .expect("the page has no NARROW/WIDE");
    // Written out in both places: check they agree before trusting either.
    assert_eq!(
        (scour_ui::NARROW, scour_ui::WIDE),
        (700, 1900),
        "the anchors moved in the crate and not in the page"
    );
    let to = PAGE
        .find("  function applyWidths() {")
        .expect("no applyWidths");
    assert!(to > from, "layOut is not above applyWidths any more");

    let cols: Vec<String> = scour_ui::DEFAULT_COLUMNS
        .iter()
        .filter_map(|id| scour_ui::column(id))
        .map(|c| {
            format!(
                r#"{{"id":"{}","w":{},"min":{},"near":{},"far":{},"max":{}}}"#,
                c.id, c.width, c.min, c.near, c.far, c.max
            )
        })
        .collect();
    // Three sets of drags: a modest one, one past the room that has to
    // stop at the floors, and a fixed column dragged beside a stretching one.
    let drags: [&[(&str, u32)]; 3] = [
        &[("name", 359)],
        &[("name", 900)],
        &[("name", 500), ("kind", 200)],
    ];
    let sets: Vec<String> = drags
        .iter()
        .map(|d| {
            let pairs: Vec<String> = d.iter().map(|(id, w)| format!("\"{id}\":{w}")).collect();
            format!("{{{}}}", pairs.join(","))
        })
        .collect();
    let harness = format!(
        "let WIDTHS = {{}};\n{}\nconst cols = [{}];\nconst out = [];\n             for (const set of [{}]) {{ WIDTHS = set;\n             for (let r = 200; r <= 3600; r += 7) out.push(layOut(cols, r)); }}\n             console.log(JSON.stringify(out));\n",
        &PAGE[from..to],
        cols.join(","),
        sets.join(",")
    );
    let path = std::env::temp_dir().join("scour-layout-check.js");
    std::fs::write(&path, &harness).expect("writing the harness out");
    let out = match std::process::Command::new("node").arg(&path).output() {
        Ok(out) => out,
        Err(e) => {
            eprintln!("the_page_shares_the_row_out…: skipped, no node here ({e})");
            let _ = std::fs::remove_file(&path);
            return;
        }
    };
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "the page's layOut did not run:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    // Parsed by hand: a list of lists of integers needs no dependency.
    let rows: Vec<Vec<u32>> = text
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split("],[")
        .map(|row| {
            row.trim_matches(|c| c == '[' || c == ']')
                .split(',')
                .map(|n| n.trim().parse().expect("a width"))
                .collect()
        })
        .collect();

    let mut n = 0;
    for d in drags {
        for room in (200..=3600).step_by(7) {
            let want = scour_ui::lay_out(
                scour_ui::DEFAULT_COLUMNS,
                |id| d.iter().find(|(k, _)| *k == id).map(|(_, w)| *w),
                room,
            );
            assert_eq!(
                rows[n], want,
                "at {room}px with {d:?} the page and the crate differ"
            );
            n += 1;
        }
    }
    assert!(n > 1200, "only {n} widths were compared");
}

/// The report's arithmetic exists twice: in `scour-chart`, which the window
/// and the terminal draw from, and in the page, which is JavaScript. This
/// runs the page's copy under `node` and compares every number it gives.
#[test]
fn the_report_shares_the_bytes_out_exactly_as_the_chart_crate_does() {
    let from = PAGE
        .find("  function shares(values, decimals) {")
        .expect("the page has no `shares`");
    let to = PAGE
        .find("  const LABEL_PX = 64;")
        .expect("the page has no `LABEL_PX`");
    assert!(to > from, "the mirrored numbers are no longer one block");

    // The mock-up's twelve folders and what they leave over; three equal
    // thirds, where the rounding has to pick a winner; every value zero;
    // one value; a scope split by age; and eight of the same number.
    let fixtures: Vec<Vec<u64>> = vec![
        vec![
            651_571_270_524,
            31_643_455_926,
            15_738_588_363,
            14_110_521_835,
            10_863_393_896,
            10_852_991_366,
            9_242_344_819,
            8_271_160_692,
            7_898_028_191,
            7_372_347_728,
            5_889_237_540,
            4_856_318_717,
            20_649_608_929,
        ],
        vec![1, 1, 1],
        vec![0, 0, 0],
        vec![7],
        vec![
            106_500_000_000,
            25_300_000_000,
            482_000_000_000,
            114_600_000_000,
            3_700_000_000,
            12_600_000_000,
        ],
        vec![5, 5, 5, 5, 5, 5, 5, 5],
    ];
    let json = serde_json::to_string(&fixtures).expect("the fixtures as JSON");
    let harness = format!(
        "{}\nconst C = 2 * Math.PI * 54;\nconst out = {json}.map((v) => ({{\n\
             s0: shares(v, 0), s1: shares(v, 1), s2: shares(v, 2),\n\
             seg: segments(v).map((s) => [s.start, s.width]),\n\
             dash: dashes(v, C).map((d) => [d.length, d.offset]),\n\
             fold: (() => {{\n\
             const f = fold(v.map((x, i) => [x, i]), (p) => p[0], 6);\n\
             return {{ kept: f.kept.map((p) => p[1]),\n\
             rest: f.rest ? [f.rest.count, f.rest.value] : null }};\n\
             }})(),\n}}));\nconsole.log(JSON.stringify(out));\n",
        &PAGE[from..to],
    );
    let path = std::env::temp_dir().join("scour-chart-check.js");
    std::fs::write(&path, &harness).expect("writing the harness out");
    let out = match std::process::Command::new("node").arg(&path).output() {
        Ok(out) => out,
        Err(e) => {
            eprintln!("the_report_shares_the_bytes_out…: skipped, no node here ({e})");
            let _ = std::fs::remove_file(&path);
            return;
        }
    };
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "the page's chart numbers did not run:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said: Vec<serde_json::Value> =
        serde_json::from_slice(&out.stdout).expect("the page printed JSON");

    let circumference = 2.0 * std::f64::consts::PI * 54.0;
    // A tenth of a nanounit: far below anything a bar or a legend shows,
    // and far above the noise of one `f64` division done twice.
    let close = |a: f64, b: f64, what: &str| {
        assert!((a - b).abs() < 1e-9, "{what}: the page {a}, the crate {b}");
    };
    let list = |v: &serde_json::Value| -> Vec<f64> {
        v.as_array()
            .expect("a list")
            .iter()
            .map(|x| x.as_f64().expect("a number"))
            .collect()
    };
    let pairs = |v: &serde_json::Value| -> Vec<(f64, f64)> {
        v.as_array()
            .expect("a list")
            .iter()
            .map(|x| {
                let p = list(x);
                (p[0], p[1])
            })
            .collect()
    };

    for (f, values) in fixtures.iter().enumerate() {
        let got = &said[f];
        for (key, decimals) in [("s0", 0u8), ("s1", 1), ("s2", 2)] {
            let want = scour_chart::shares(values, decimals);
            let mine = list(&got[key]);
            assert_eq!(
                mine.len(),
                want.len(),
                "fixture {f}: {key} is a different length"
            );
            for (i, (a, b)) in mine.iter().zip(&want).enumerate() {
                close(*a, *b, &format!("fixture {f}: {key}[{i}]"));
            }
        }
        for (i, (s, want)) in pairs(&got["seg"])
            .iter()
            .zip(scour_chart::segments(values))
            .enumerate()
        {
            close(s.0, want.start, &format!("fixture {f}: segment {i} start"));
            close(s.1, want.width, &format!("fixture {f}: segment {i} width"));
        }
        for (i, (d, want)) in pairs(&got["dash"])
            .iter()
            .zip(scour_chart::dashes(values, circumference))
            .enumerate()
        {
            close(d.0, want.length, &format!("fixture {f}: dash {i} length"));
            close(d.1, want.offset, &format!("fixture {f}: dash {i} offset"));
        }
        let items: Vec<(u64, usize)> = values.iter().copied().zip(0..).collect();
        let want = scour_chart::fold(items, |x| x.0, 6);
        let kept: Vec<usize> = got["fold"]["kept"]
            .as_array()
            .expect("kept")
            .iter()
            .map(|x| x.as_u64().expect("an index") as usize)
            .collect();
        assert_eq!(
            kept,
            want.kept.iter().map(|x| x.1).collect::<Vec<_>>(),
            "fixture {f}: the folded order differs"
        );
        match (&got["fold"]["rest"], want.rest) {
            (serde_json::Value::Null, None) => {}
            (rest, Some(r)) => {
                let rest = list(rest);
                assert_eq!(
                    (rest[0] as usize, rest[1] as u64),
                    (r.count, r.value),
                    "fixture {f}: the rest differs"
                );
            }
            (rest, None) => panic!("fixture {f}: the page folded {rest} away, the crate none"),
        }
    }
}

/// Neither the words nor the taxonomy starts a second search when it lands.
#[test]
fn the_first_search_does_not_wait_for_the_kind_taxonomy() {
    let boot = PAGE
        .find("  q.focus();\n  q.select();")
        .expect("page.html has no list boot sequence");
    let body = &PAGE[boot..];
    let search = body
        .find("\n  render();")
        .expect("the boot sequence does not start a search");
    let words = body
        .find("loadLanguage(\"\")")
        .expect("the boot sequence does not load the language");
    assert!(
        search < words,
        "the first search is still gated on the words and the taxonomy"
    );

    // What arrives repaints rather than searching: the rows did not change.
    let at = PAGE
        .find("  function applyLanguage() {")
        .expect("the page has no applyLanguage");
    let end = PAGE[at..].find("\n  }").map(|e| at + e).expect("no end");
    let apply = &PAGE[at..end];
    assert!(
        !apply.contains("render()"),
        "changing language starts another index search instead of repainting"
    );
    for needle in [
        "for (const tr of pool) tr.__stamp = \"\";",
        "repaint(true);",
    ] {
        assert!(
            apply.contains(needle),
            "applyLanguage does not redraw the rows: missing {needle}"
        );
    }
}

#[test]
fn sorting_does_not_recount_query_facets() {
    assert!(
        PAGE.contains("const queryChanged = LIST.query !== text;"),
        "render does not distinguish a new query from a new ordering"
    );
    assert!(
        PAGE.contains("if (queryChanged) {\n          sidebarCost = 0;\n          sidebarSoon();"),
        "every ordering change still schedules the facet walk"
    );
    // One staleness test: the sidebar's generation and the box, neither of
    // which moves for a re-sort.
    assert!(
        PAGE.contains("const current = () => ours === sidebarGeneration && text === q.value;"),
        "facet answers are still invalidated by sort-only generations"
    );
    assert!(
        !PAGE.contains("sidebar(LIST.query, generation)"),
        "a facet request is still coupled to the row ordering generation"
    );
}

/// A rail counts what switching to one of its rows would give, so it is
/// counted with its own term taken out: a facet answers with the keys that
/// matched and no others, so otherwise every row but one reads zero.
#[test]
fn a_rail_is_not_counted_through_its_own_filter() {
    for needle in [
        "const forKind = without(text, [\"kind\"]);",
        "const forAge = without(text, [\"dm\"]);",
    ] {
        assert!(
            PAGE.contains(needle),
            "the sidebar no longer strips a rail's own term before counting it: {needle}"
        );
    }
    // Only its own term: kind under a scope does give the narrowed count.
    assert!(
        !PAGE.contains("without(text, [\"under\"]") && !PAGE.contains("without(text, [\"size\"]"),
        "a rail is stripping a filter that is not its own"
    );
    // Both groups on every call: `by=kind` alone puts the walk on
    // `FACET_SCAN_CAP` (200,000 rows), and that sample is the first rows
    // reached, not a proportional one — images 1,430 against 210,551.
    assert!(
        PAGE.contains("by: \"kind,age\""),
        "the sidebar asks for one facet group, which puts the rail on the \
             capped scan path and makes its numbers a biased sample"
    );
    // The meter reads the query in force, not a rail's broader one.
    assert!(
        PAGE.contains("const total = inForce.then((res) => {"),
        "the total no longer comes from the query actually in force"
    );
}

/// The rail above depends on it: `without` strips a `kind:` term only if
/// the page's copy of the grammar knows the value. Both directions are
/// drift — an unknown token is a rail of zeros, an extra one strips text.
#[test]
fn the_page_takes_the_engines_kind_vocabulary() {
    fn values(name: &str) -> Vec<String> {
        let at = PAGE
            .find(&format!("const {name} = ["))
            .unwrap_or_else(|| panic!("the page has no {name}"));
        let open = PAGE[at..].find('[').expect("no opening bracket") + at;
        let close = PAGE[open..].find(']').expect("no closing bracket") + open;
        PAGE[open + 1..close]
            .split(',')
            .map(|s| s.trim().trim_matches('"').to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    let offered = values("KIND_VALUES");
    let aliases = values("KIND_ALIASES");

    let engine: Vec<&str> = scour_core::Kind::OFFERED
        .iter()
        .map(|k| k.token())
        .collect();
    assert_eq!(
        offered, engine,
        "the page's kind values are not Kind::OFFERED through Kind::token"
    );

    for v in offered.iter().chain(aliases.iter()) {
        assert!(
            scour_core::Kind::from_name(v).is_some(),
            "the page accepts kind:{v}, which the engine does not parse"
        );
    }

    // `accepts` reads both lists, or half of this proves nothing.
    assert!(
            PAGE.contains(
                "case \"kind\": return v.split(\";\").some(Boolean) && v.split(\";\").every((k) => !k || KIND_VALUES.includes(k) || KIND_ALIASES.includes(k));"
            ),
            "accepts() no longer answers for kind: from the two lists this test checks"
        );
}

/// Every msgid in the page, exactly as the page writes it: `T("…")`, the
/// `data-t*` attributes, `msgid:` and `about:`. Literals only, by design —
/// a msgid built by concatenation is one no test can see.
fn page_msgids() -> Vec<String> {
    /// The escapes a msgid can carry, and no more.
    fn unescape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        }
        out
    }

    /// The string literal in `rest`, up to the first unescaped `quote`.
    fn literal(rest: &str, quote: char) -> Option<String> {
        let mut out = String::new();
        let mut escaped = false;
        for c in rest.chars() {
            if escaped {
                out.push(c);
                escaped = false;
            } else if c == '\\' {
                out.push(c);
                escaped = true;
            } else if c == quote {
                return Some(unescape(&out));
            } else if c == '\n' {
                return None; // not a literal; a line ended inside it
            } else {
                out.push(c);
            }
        }
        None
    }

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |s: String| {
        if !s.is_empty() && seen.insert(s.clone()) {
            out.push(s);
        }
    };

    for (open, quote, entity) in [
        ("T(\"", '"', false),
        ("T('", '\'', false),
        ("msgid: \"", '"', false),
        ("about: \"", '"', false),
        ("data-t=\"", '"', true),
        ("data-t-html=\"", '"', true),
        ("data-t-title=\"", '"', true),
        ("data-t-aria=\"", '"', true),
        ("data-t-ph=\"", '"', true),
    ] {
        let mut at = 0;
        while let Some(found) = PAGE[at..].find(open) {
            let start = at + found + open.len();
            if let Some(text) = literal(&PAGE[start..], quote) {
                // Attribute values are HTML: `&lt;code&gt;` there is
                // `<code>` in the `.po`.
                push(if entity {
                    text.replace("&lt;", "<")
                        .replace("&gt;", ">")
                        .replace("&quot;", "\"")
                        .replace("&#39;", "'")
                        .replace("&amp;", "&")
                } else {
                    text
                });
            }
            at = start;
        }
    }
    out
}

/// A missing entry degrades to English rather than to a bare key, so a
/// window could drift back into English with nothing else complaining.
#[test]
fn the_page_says_nothing_the_catalogue_has_not_heard_of() {
    let ids = page_msgids();
    // A floor rather than a count: adding a string is not a test change,
    // and an extraction that stopped matching still fails.
    assert!(
        ids.len() > 140,
        "only {} msgids found; the extraction is broken, not the page",
        ids.len()
    );

    for (tag, _) in scour_i18n::LANGUAGES {
        let cat = scour_i18n::Catalogue::for_language(tag);
        if !cat.is_translated() {
            continue; // English: the msgid is the string.
        }
        let missing: Vec<&String> = ids.iter().filter(|id| !cat.has(id)).collect();
        assert!(
            missing.is_empty(),
            "{tag} has no entry for {} of the window's strings, starting with {:?}",
            missing.len(),
            &missing[..missing.len().min(5)]
        );
    }
}

/// No Turkish left in the page outside the query grammar. Exempt: comments,
/// and the spellings the engine parses, which a query still needs.
#[test]
fn no_turkish_is_left_where_a_reader_would_see_it() {
    const TURKISH: [char; 12] = ['ç', 'ğ', 'ı', 'ö', 'ş', 'ü', 'Ç', 'Ğ', 'İ', 'Ö', 'Ş', 'Ü'];

    // Comments first: they are for whoever maintains this.
    let mut code = String::with_capacity(PAGE.len());
    let mut rest = PAGE;
    loop {
        let next = ["/*", "//", "<!--"]
            .iter()
            .filter_map(|open| rest.find(open).map(|at| (at, *open)))
            .min();
        let Some((at, open)) = next else {
            code.push_str(rest);
            break;
        };
        code.push_str(&rest[..at]);
        let close = match open {
            "/*" => "*/",
            "//" => "\n",
            _ => "-->",
        };
        rest = match rest[at + open.len()..].find(close) {
            Some(end) => &rest[at + open.len() + end + close.len()..],
            None => break,
        };
    }

    // Then the grammar, which is four named places and not a category.
    for (open, close) in [
        ("const KIND_ALIASES = [", "];"),
        ("const TIME_RE =", "\n"),
        ("const MISTAKEN =", "\n"),
        ("const fold =", "\n"),
    ] {
        let at = code
            .find(open)
            .unwrap_or_else(|| panic!("the page no longer has `{open}`"));
        let end = code[at..]
            .find(close)
            .map(|e| at + e + close.len())
            .unwrap_or(code.len());
        code.replace_range(at..end, "");
    }
    // `alias:` lists inside the field table, one line each.
    while let Some(at) = code.find("alias: [") {
        let end = code[at..]
            .find(']')
            .map(|e| at + e + 1)
            .expect("alias list");
        code.replace_range(at..end, "");
    }

    let left: Vec<&str> = code
        .lines()
        .filter(|l| l.contains(TURKISH))
        .map(str::trim)
        .collect();
    assert!(
        left.is_empty(),
        "{} lines of Turkish are still in the page: {left:#?}",
        left.len()
    );
}

#[test]
fn sorting_keeps_the_exact_total_of_the_same_query() {
    let start = PAGE
        .find("function render() {")
        .expect("no render function");
    let end = PAGE[start..]
        .find("\n  function draw(text, p, hits, res) {")
        .map(|at| start + at)
        .expect("render function has no end marker");
    let render = &PAGE[start..end];

    for needle in [
        "const keepExact = !queryChanged && LIST.exact && res.capped;",
        "if (!keepExact) setTotal(res.total, !res.capped);",
    ] {
        assert!(
            render.contains(needle),
            "a capped sort can discard the query's exact total: missing {needle}"
        );
    }
    assert!(
        PAGE.contains("const shown = LIST.exact ? fmt(LIST.total) : fmt(LIST.total) + \"+\";"),
        "the meter still reads the capped sort reply instead of query-scoped state"
    );
}

/// The scrollable extent is `lines * pitch` and `pitch` is `TILE[…].h`, so
/// the style sheet has to declare the same height.
#[test]
fn the_tile_the_page_draws_is_the_tile_it_counts() {
    /// `h: 104` out of the `TILE` table, for one shape.
    fn counted(shape: &str) -> u32 {
        let table = PAGE.find("const TILE = {").expect("no TILE table");
        let end = table + PAGE[table..].find("};").expect("TILE has no end");
        let at = table
            + PAGE[table..end]
                .find(&format!("{shape}: {{"))
                .unwrap_or_else(|| panic!("TILE has no {shape}"));
        let h = at + PAGE[at..end].find("h: ").expect("no height") + 3;
        PAGE[h..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|n| n.parse().ok())
            .expect("height is not a number")
    }

    /// `--tile-h: 104px` out of the style rule for one shape.
    fn drawn(shape: &str) -> u32 {
        let rule = format!("body.grid[data-view=\"{shape}\"] {{");
        let at = PAGE
            .find(&rule)
            .unwrap_or_else(|| panic!("no style rule for {shape}"));
        let h = at + PAGE[at..].find("--tile-h:").expect("no --tile-h") + 9;
        PAGE[h..]
            .trim_start()
            .split("px")
            .next()
            .and_then(|n| n.trim().parse().ok())
            .expect("--tile-h is not a number of pixels")
    }

    for shape in ["icons", "large"] {
        assert_eq!(
            counted(shape),
            drawn(shape),
            "{shape}: the arithmetic and the style sheet disagree about the tile's height"
        );
    }
}

/// The extent is stated rather than summed so that a paint cannot move the
/// position: a moved extent is corrected, a correction scrolls, that paints.
#[test]
fn nothing_on_the_painting_path_moves_the_scroll_position() {
    // The two allowed writes are both from something a person did, and
    // neither runs from a frame.
    for (open, close) in [
        (
            "function paintWindow() {",
            "\n  function writeRow(tr, f, cols, parsed, marks) {",
        ),
        ("function repaint(rowsChanged) {", "\n  const onScroll ="),
        ("function fillWindow() {", "\n  let paintQueued"),
        ("function visibleRange() {", "\n  let pool = [];"),
    ] {
        let at = PAGE
            .find(open)
            .unwrap_or_else(|| panic!("the page no longer has `{open}`"));
        let end = at
            + PAGE[at..]
                .find(close)
                .unwrap_or_else(|| panic!("`{open}` has no end marker"));
        assert!(
            !PAGE[at..end].contains("scrollTop ="),
            "`{open}` writes the scroll position; that is the loop the sizer exists to break"
        );
    }

    // And the extent is never read back off the element: Chromium
    // re-serialises `1146724px` as `1.14672e+06px`, so the guard misses
    // and the height is rewritten on every frame.
    for asking in [
        "sizer.style.height !==",
        "firstElementChild.style.height !==",
    ] {
        assert!(
            !PAGE.contains(asking),
            "the list asks the DOM what it made of a height it wrote: `{asking}`"
        );
    }

    let mode = PAGE
        .find("function applyMode(next) {")
        .expect("applyMode is absent");
    let end = mode
        + PAGE[mode..]
            .find("\n  viewBox.addEventListener")
            .expect("applyMode has no end marker");
    let body = &PAGE[mode..end];
    let sized = body
        .find("sizer.style.height")
        .expect("applyMode no longer states the extent");
    let moved = body.find("scrollEl.scrollTop =").expect("checked above");
    // Stated before written, or the browser clamps the new position to
    // the outgoing shape's extent.
    assert!(
        sized < moved,
        "applyMode writes the scroll position before it states the extent"
    );
}

#[test]
fn every_rendered_row_fact_invalidates_the_live_row_cache() {
    let start = PAGE
        .find("const stamp = f ? [")
        .expect("row stamp is absent");
    let end = PAGE[start..]
        .find("].join(\"\\u0001\")")
        .map(|at| start + at)
        .expect("row stamp has no end");
    let stamp = &PAGE[start..end];

    for field in [
        "f.path",
        "f.name",
        "f.mtime",
        "f.size",
        "f.fresh",
        "f.is_dir",
        "f.thumb",
        "f.ktoken",
        "f.kind",
        "f.ext",
        "f.ctime",
        "f.atime",
        "f.perm",
        "f.user",
        "f.group",
        "f.disk",
        "f.items",
        "under.disk",
        "under.files",
    ] {
        assert!(stamp.contains(field), "row stamp omits {field}");
    }
}

#[test]
fn local_column_changes_do_not_search_again() {
    let heads = PAGE
        .find("function buildHeads() {")
        .expect("column heading builder is absent");
    let heads_end = PAGE[heads..]
        .find("\n  function columnMenu()")
        .map(|at| heads + at)
        .expect("column heading builder has no end marker");
    assert!(
        PAGE[heads..heads_end].contains("tr.__stamp = \"\""),
        "a same-width reorder leaves body cells in their old columns"
    );

    let moved = PAGE
        .find("const endMove = () => {")
        .expect("column move handler is absent");
    let moved_end = PAGE[moved..]
        .find("document.querySelector(\"thead\").addEventListener(\"pointerup\"")
        .map(|at| moved + at)
        .expect("column move handler has no end marker");
    assert!(
        !PAGE[moved..moved_end].contains("render();"),
        "moving a local column searches and discards the row cache"
    );

    let picked = PAGE
        .find("colMenu.addEventListener(\"click\"")
        .expect("column picker handler is absent");
    let picked_end = PAGE[picked..]
        .find("\n  });\n\n  const pathOf")
        .map(|at| picked + at)
        .expect("column picker handler has no end marker");
    assert!(
        !PAGE[picked..picked_end].contains("render();"),
        "showing a local column searches and discards the row cache"
    );
}

#[test]
fn stale_sidebar_work_is_stopped_before_the_service_call() {
    let start = PAGE
        .find("function sidebar(text) {")
        .expect("sidebar function is absent");
    let body = &PAGE[start..];
    let guard = body
        .find("if (text !== q.value) return Promise.resolve();")
        .expect("sidebar has no preflight query guard");
    // Not the argument by name: a rail is counted with its own term
    // stripped, so what matters is that no call precedes the guard.
    let call = body
        .find("SERVICE.sidebar(")
        .expect("sidebar service call is absent");
    assert!(
        guard < call,
        "the stale guard runs only after paying for facets"
    );
    assert!(
        PAGE.contains("LIST.query !== null && LIST.query === q.value"),
        "the quiet timer can start work for rows a new query has replaced"
    );
    assert!(
        PAGE.contains("ours === sidebarGeneration && text === q.value"),
        "a stale facet walk can poison the current query's refresh budget"
    );
}

#[test]
fn late_local_metadata_repaints_without_searching() {
    let start = PAGE
        .find("SERVICE.get(\"places\", {})")
        .expect("places request is absent");
    let end = PAGE[start..]
        .find("\n  function applyLanguage() {")
        .map(|at| start + at)
        .expect("places request has no end marker");
    let places = &PAGE[start..end];
    for needle in [
        "syncFacets(parse(q.value))",
        "tr.__stamp = \"\"",
        "repaint(true)",
    ] {
        assert!(places.contains(needle), "places does not apply {needle}");
    }
    assert!(
        !places.contains("render();"),
        "local place metadata starts another index search"
    );
}

#[test]
fn the_first_count_does_not_claim_the_index_has_zero_entries() {
    assert!(PAGE.contains("let TOTAL_KNOWN = false;"));
    assert!(
        PAGE.contains("TOTAL_KNOWN ? shown + \" / \" + fmt(TOTAL) : shown"),
        "the first search still prints an unknown denominator as zero"
    );
    assert!(PAGE.contains("TOTAL_KNOWN = true;"));
}
