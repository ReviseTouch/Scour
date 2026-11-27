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
fn only_a_word_processor_flows() {
    assert!(flows("docx") && flows("DOC"));
    assert!(!flows("xlsx") && !flows("pptx") && !flows("pdf"));
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
    let page = html(&flow);
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
    let page = html(&flow);
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
