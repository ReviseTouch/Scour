use super::*;

fn para(text: &str) -> Para {
    Para {
        runs: vec![Run {
            text: text.into(),
            ..Run::default()
        }],
        ..Para::default()
    }
}

#[test]
fn base64_goes_there_and_back() {
    for bytes in [&b""[..], b"a", b"ab", b"abc", b"abcd", &[0, 255, 128, 7, 9]] {
        let text = encode(bytes);
        assert!(b64::is_base64(&text), "{text}");
        assert_eq!(decode(&text).as_deref(), Some(bytes), "{text}");
    }
    assert_eq!(encode(b"Man"), "TWFu");
    assert_eq!(encode(b"Ma"), "TWE=");
    assert_eq!(decode("TW=u"), None);
    assert_eq!(decode("TWF"), None);
    assert_eq!(decode("TW!u"), None);
}

#[test]
fn the_six_formats_of_office_flow() {
    assert!(flows("docx") && flows("DOC") && flows("xlsx") && flows("xls"));
    assert!(flows("pptx") && flows("ppt"));
    assert!(flows("odt") && flows("ods") && flows("odp"));
    assert!(!flows("pdf") && !flows("rtf"));
}

#[test]
fn a_cell_shows_its_number_through_its_format_and_sits_by_its_kind() {
    let date = GridCell {
        value: Some(46273.0),
        format: "dd\\.mm\\.yyyy".into(),
        text: "2026-09-08".into(),
        ..GridCell::default()
    };
    assert_eq!(date.shown(&Locale::of("tr")), "08.09.2026");
    assert_eq!(date.alignment(), Align::Right);
    let plain = GridCell {
        value: Some(1234.5),
        ..GridCell::default()
    };
    assert_eq!(plain.shown(&Locale::of("tr")), "1234,5");
    let word = GridCell {
        text: "Hatay".into(),
        ..GridCell::default()
    };
    assert_eq!(
        (word.shown(&Locale::default()).as_str(), word.alignment()),
        ("Hatay", Align::Left)
    );
}

#[test]
fn a_sheet_is_held_to_its_bounds() {
    let row = |n: usize| GridRow {
        cells: (0..n)
            .map(|i| GridCell {
                text: i.to_string(),
                span: if i == 0 { 99 } else { 1 },
                value: if i == 1 { Some(f64::NAN) } else { None },
                ..GridCell::default()
            })
            .collect(),
    };
    let flow = Flow {
        blocks: vec![Block::Sheet(Sheet {
            name: "Liste".into(),
            widths: vec![f32::NAN, 500.0],
            rows: (0..MOST_SHEET_ROWS + 5)
                .map(|_| row(MOST_SHEET_COLUMNS + 3))
                .collect(),
            frozen: true,
            cut: false,
        })],
        cut: false,
    }
    .bounded();
    let s = flow.sheets().next().unwrap();
    assert!(s.cut);
    assert!(s.rows.len() <= MOST_SHEET_ROWS);
    assert_eq!(s.rows[0].cells.len(), MOST_SHEET_COLUMNS);
    assert_eq!(s.rows[0].cells[0].span, MOST_SHEET_COLUMNS as u32);
    assert_eq!(s.rows[0].cells[1].value, None);
    assert_eq!(s.widths.len(), MOST_SHEET_COLUMNS);
    assert_eq!((s.widths[0], s.widths[1]), (5.0, 40.0));
}

#[test]
fn every_word_is_escaped_and_no_markup_gets_through() {
    let flow = Flow {
        blocks: vec![
            Block::Para(para("<script>alert(1)</script> & \"q\"")),
            Block::Para(Para {
                mark: "<b>".into(),
                ..para("x")
            }),
        ],
        cut: false,
    };
    let page = html(&flow, &Locale::default(), &Marks::default());
    assert!(!page.contains("<script>"), "{page}");
    assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt; &amp; &quot;q&quot;"));
    assert!(page.contains("<span class=mk>&lt;b&gt;</span>"));
}

#[test]
fn a_picture_the_faces_cannot_draw_or_that_lies_is_dropped() {
    let pic = |mime: &str, data: &str| {
        Block::Picture(Picture {
            mime: mime.into(),
            data: data.into(),
            width: 10.0,
            height: 5.0,
        })
    };
    let flow = Flow {
        blocks: vec![
            pic("image/png", &encode(b"\x89PNG")),
            pic("image/svg+xml", &encode(b"<svg/>")),
            pic("image/png", "not base64!"),
            pic("image/png\" onerror=\"x", &encode(b"x")),
        ],
        cut: false,
    }
    .bounded();
    assert_eq!(flow.blocks.len(), 1, "{flow:?}");
    let page = html(&flow, &Locale::default(), &Marks::default());
    assert!(
        page.contains("src=\"data:image/png;base64,iVBORw==\""),
        "{page}"
    );
    assert!(!page.contains("onerror"));
}

#[test]
fn the_bounds_cut_the_text_and_say_so() {
    let big = "ş".repeat(MOST_TEXT);
    let flow = Flow {
        blocks: vec![Block::Para(para(&big)), Block::Para(para("after"))],
        cut: false,
    }
    .bounded();
    assert!(flow.cut);
    assert_eq!(flow.blocks.len(), 1);
    let Block::Para(p) = &flow.blocks[0] else {
        panic!()
    };
    // Cut on a character, not inside one.
    assert!(p.text().len() <= MOST_TEXT && p.text().chars().all(|c| c == 'ş'));
}

#[test]
fn nonsense_numbers_are_held_to_sense() {
    let flow = Flow {
        blocks: vec![Block::Para(Para {
            indent: f32::NAN,
            first: -100.0,
            size: f32::INFINITY,
            before: 99.0,
            heading: 40,
            ..para("x")
        })],
        cut: false,
    }
    .bounded();
    let Block::Para(p) = &flow.blocks[0] else {
        panic!()
    };
    assert_eq!(
        (p.indent, p.first, p.size, p.before, p.heading),
        (0.0, -4.0, 1.0, 3.0, 6)
    );
}

#[test]
fn a_table_s_widths_are_shares_of_its_columns() {
    let cell = |t: &str| Cell {
        paras: vec![para(t)],
        span: 1,
        shade: None,
    };
    let table = |widths: Vec<f32>| {
        Flow {
            blocks: vec![Block::Table(Table {
                widths,
                rows: vec![Row {
                    cells: vec![cell("a"), cell("b"), cell("c")],
                    head: false,
                }],
                ruled: true,
            })],
            cut: false,
        }
        .bounded()
    };
    let widths = |f: Flow| match &f.blocks[0] {
        Block::Table(t) => t.widths.clone(),
        _ => panic!(),
    };
    assert_eq!(widths(table(vec![1.0, 1.0, 2.0])), vec![0.25, 0.25, 0.5]);
    // Missing: even. Too few, or broken: the rest get the mean.
    assert_eq!(widths(table(vec![])), vec![1.0 / 3.0; 3]);
    assert_eq!(widths(table(vec![2.0, f32::NAN])), vec![1.0 / 3.0; 3]);
}

#[test]
fn it_crosses_the_protocol_as_json() {
    let flow = Flow {
        blocks: vec![
            Block::Para(Para {
                heading: 1,
                size: 1.4,
                align: Align::Center,
                ..para("Başlık")
            }),
            Block::Break,
        ],
        cut: true,
    };
    let json = serde_json::to_string(&flow).unwrap();
    assert!(
        json.contains("\"k\":\"para\"") && json.contains("\"k\":\"break\""),
        "{json}"
    );
    assert_eq!(serde_json::from_str::<Flow>(&json).unwrap(), flow);
}

#[test]
fn a_book_is_its_sheets_one_shown_at_a_time_with_no_script() {
    let cell = |text: &str, bold: bool| GridCell {
        text: text.into(),
        bold,
        ..GridCell::default()
    };
    let sheet = |name: &str| {
        Block::Sheet(Sheet {
            name: name.into(),
            widths: vec![6.0, 4.0],
            rows: vec![
                GridRow {
                    cells: vec![cell("Tarih", true), cell("<b>", true)],
                },
                GridRow {
                    cells: vec![
                        GridCell {
                            value: Some(46273.0),
                            format: "dd.mm.yyyy".into(),
                            ..GridCell::default()
                        },
                        cell("x", false),
                    ],
                },
            ],
            frozen: true,
            cut: false,
        })
    };
    let page = html(
        &Flow {
            blocks: vec![sheet("Liste"), sheet("Özet")],
            cut: false,
        },
        &Locale::of("tr"),
        &Marks::default(),
    );
    assert!(!page.contains("<script"), "{page}");
    assert!(page.contains("<section class=sheet id=s0>") && page.contains("id=s1>"));
    assert!(page.contains("<a href=\"#s1\">Özet</a>"), "{page}");
    assert!(
        page.contains("<thead><tr><td style=\"font-weight:600;\">Tarih</td>"),
        "{page}"
    );
    assert!(page.contains("&lt;b&gt;") && !page.contains("<td style=\"font-weight:600;\"><b>"));
    assert!(
        page.contains("text-align:right;\">08.09.2026</td>"),
        "{page}"
    );
}

#[test]
fn a_slide_is_a_card_on_its_own_colour_numbered() {
    let flow = Flow {
        blocks: vec![
            Block::Slide(Slide {
                background: Some([30, 42, 50]),
                blocks: vec![Block::Para(para("Başlık")), Block::Slide(Slide::default())],
            }),
            Block::Slide(Slide {
                background: None,
                blocks: vec![Block::Para(para("İkinci"))],
            }),
        ],
        cut: false,
    }
    .bounded();
    let Block::Slide(first) = &flow.blocks[0] else {
        panic!()
    };
    // No slide inside a slide.
    assert_eq!(first.blocks.len(), 1);
    assert!(first.dark());
    let page = html(&flow, &Locale::default(), &Marks::default());
    assert!(page.contains("<body class=deck>"));
    assert!(page.contains("background:#1e2a32;color:#f6f8fa;"), "{page}");
    assert!(page.contains("<span class=no>2</span>"), "{page}");
    assert!(flow.text().contains("Başlık") && flow.text().contains("İkinci"));
}

#[test]
fn the_finds_are_marked_and_the_first_is_where_the_address_goes() {
    let flow = Flow {
        blocks: vec![
            Block::Para(para("Kira sözleşmesinin feshi <talebimizdir>.")),
            Block::Para(para("Davalı kiracı bedeli ödememiştir.")),
        ],
        cut: false,
    };
    let page = html(&flow, &Locale::default(), &Marks::new(&["KİRA"]));
    assert!(
        page.contains("<mark id=f>Kira</mark> sözleşmesinin"),
        "{page}"
    );
    assert!(page.contains("Davalı <mark>kira</mark>cı"), "{page}");
    assert!(page.contains("&lt;talebimizdir&gt;"));
    assert_eq!(page.matches("id=f").count(), 1);
}
