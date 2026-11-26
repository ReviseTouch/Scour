use std::time::{Duration, Instant};

use scour_i18n::Catalogue;
use scour_proto::{Cpu, Held, HeldKind, Memory, Request, Resources, Running};

use super::*;
use crate::resource_gauges::{cpu_said, machine, mem_lines, percent, spark};
use crate::resource_lines::{foot, held_lines, run_lines, sort_line};
use scour_ui::resources as says;

const MB: u64 = 1 << 20;

/// English punctuation, as `words` sets it for an English catalogue.
fn english() -> Catalogue {
    crate::MARKS.with(|m| m.set((',', '.')));
    Catalogue::for_language("en")
}

fn held(kind: HeldKind, bytes: u64) -> Held {
    Held {
        kind,
        path: format!("/var/tmp/scour-test/{}", token(kind)),
        bytes,
        ..Held::default()
    }
}

/// This machine's figures on the morning of the mock, roughly.
fn answer() -> Resources {
    Resources {
        version: "0.2.0".into(),
        uptime_ms: 4_380_000,
        files: vec![
            Held {
                items: 7_518_093,
                parts: 8,
                ..held(HeldKind::Index, 545 * MB)
            },
            Held {
                items: 23_152,
                limit: 1 << 30,
                ..held(HeldKind::Texts, 6 * MB)
            },
            held(HeldKind::Previews, 5 * MB),
            Held {
                busy: "open".into(),
                ..held(HeldKind::Page, 296 * MB)
            },
            Held {
                busy: "kept".into(),
                ..held(HeldKind::State, MB / 2)
            },
        ],
        unsorted: 1_391_010,
        memory: Memory {
            service: 65 * MB,
            peak: 480 * MB,
            total: 32 << 30,
            available: 21 << 30,
        },
        cpu: Cpu {
            service_ms: 1_000,
            busy: 100,
            total: 1_000,
            threads: 20,
            load_x100: 230,
        },
        running: vec![
            Running {
                kind: "window".into(),
                count: 1,
                bytes: 50 * MB,
                oldest_s: 600,
            },
            Running {
                kind: "mcp".into(),
                count: 14,
                bytes: 28 * MB,
                oldest_s: 97_200,
            },
        ],
        restartable: true,
    }
}

#[test]
fn a_press_is_named_by_its_row_and_asks_what_the_contract_says() {
    assert_eq!(Press::named("index"), Some(Press::Clear(HeldKind::Index)));
    assert_eq!(Press::named("page"), Some(Press::Clear(HeldKind::Page)));
    assert_eq!(Press::named("sort"), Some(Press::Sort));
    assert_eq!(Press::named("restart"), Some(Press::Restart));
    assert_eq!(Press::named("onceki"), None);
    assert!(matches!(
        Press::Clear(HeldKind::Previews).request(),
        Request::Clear {
            what: HeldKind::Previews
        }
    ));
    assert!(matches!(
        Press::Sort.request(),
        Request::Maintain {
            level: scour_core::Maintenance::Rebuild
        }
    ));
    assert!(matches!(Press::Restart.request(), Request::Restart {}));
}

#[test]
fn rows_say_sizes_and_bars_in_the_services_order() {
    let cat = english();
    let p = Panel::default();
    let lines = held_lines(&p, &answer(), &cat);
    let tokens: Vec<&str> = lines.iter().map(|l| l.token.as_str()).collect();
    assert_eq!(tokens, ["index", "texts", "previews", "page", "state"]);
    assert_eq!(lines[0].size, "545.0 MB");
    assert_eq!(lines[0].note, "7,518,093 entries · parts: 8");
    assert_eq!(lines[1].note, "23,152 documents · up to 1.0 GB");
    assert_eq!(lines[0].share, 1.0);
    assert!((lines[3].share - 296.0 / 545.0).abs() < 1e-6);
    assert!(lines[0].danger && lines[0].two_step && lines[0].bar);
    assert_eq!(lines[0].path, "/var/tmp/scour-test/index");
    // The page's browser is open: its button is refused, with the reason.
    assert_eq!(lines[3].refused, says::refused("open").expect("a reason"));
    assert!(lines[2].refused.is_empty());
    // The settings only ever offer to be shown.
    assert_eq!(lines[4].button, says::THINGS[4].act.button);
    assert!(!lines[4].danger && !lines[4].two_step && !lines[4].bar);
    assert!(lines[4].refused.is_empty());
}

#[test]
fn a_note_leaves_out_a_part_whose_number_is_nought() {
    let cat = english();
    let mut r = answer();
    r.files[0].parts = 0;
    r.files[1].limit = 0;
    let lines = held_lines(&Panel::default(), &r, &cat);
    assert_eq!(lines[0].note, "7,518,093 entries");
    assert_eq!(lines[1].note, "23,152 documents");
}

#[test]
fn one_ask_is_open_at_a_time_and_cancel_closes_it() {
    let cat = english();
    let r = answer();
    let mut p = Panel::default();
    assert!(!p.step(Press::Clear(HeldKind::Index), "ask"));
    assert!(held_lines(&p, &r, &cat)[0].asking);
    assert!(!p.step(Press::Sort, "ask"));
    assert!(!held_lines(&p, &r, &cat)[0].asking);
    let sort = sort_line(&p, &r, &cat);
    assert!(sort.asking);
    assert_eq!(sort.ask, says::SORT.ask.replace("{n}", "1,391,010"));
    // The same button again closes it, as Cancel does.
    assert!(!p.step(Press::Sort, "ask"));
    assert_eq!(p.asking, None);
    p.step(Press::Clear(HeldKind::Texts), "ask");
    assert!(!p.step(Press::Clear(HeldKind::Texts), "no"));
    assert_eq!(p.asking, None);
    // The second step is the one that asks the service.
    p.step(Press::Clear(HeldKind::Texts), "ask");
    assert!(p.step(Press::Clear(HeldKind::Texts), "go"));
    assert_eq!(p.asking, None);
}

#[test]
fn a_row_the_service_would_refuse_opens_no_ask_and_sends_nothing() {
    let cat = english();
    let mut p = Panel::default();
    let mut r = answer();
    // An ask opened while the page's browser was shut, then it opened.
    r.files[3].busy.clear();
    p.took(r.clone());
    assert!(!p.step(Press::Clear(HeldKind::Page), "ask"));
    assert!(held_lines(&p, &r, &cat)[3].asking);
    r.files[3].busy = "open".into();
    p.took(r.clone());
    assert!(!held_lines(&p, &r, &cat)[3].asking);
    assert!(!p.step(Press::Clear(HeldKind::Page), "go"));
    assert!(!p.step(Press::Clear(HeldKind::Page), "ask"));
    assert_eq!(p.asking, None);
    // The settings are `kept`, which refuses nothing a button offers.
    assert!(!p.refuses(Press::Clear(HeldKind::State)));
    assert!(p.step(Press::Clear(HeldKind::Previews), "go"));
}

#[test]
fn what_a_press_came_to_is_said_in_its_row_then_goes() {
    let cat = english();
    let r = answer();
    let mut p = Panel::default();
    let t0 = Instant::now();
    p.answered(Press::Clear(HeldKind::Texts), Ok(()), t0);
    let lines = held_lines(&p, &r, &cat);
    assert_eq!(lines[1].said, says::THINGS[1].act.done);
    assert!(!lines[1].said_no);
    assert!(lines[0].said.is_empty(), "only in its own row");
    assert!(!p.expire(t0 + Duration::from_secs(1)));
    assert!(p.expire(t0 + Duration::from_secs(3)));
    assert!(held_lines(&p, &r, &cat)[1].said.is_empty());

    p.answered(
        Press::Clear(HeldKind::Page),
        Err("close it first".into()),
        t0,
    );
    let lines = held_lines(&p, &r, &cat);
    assert_eq!(lines[3].said, "close it first");
    assert!(lines[3].said_no);
}

#[test]
fn the_unsorted_line_shows_only_with_rows_to_order() {
    let cat = english();
    let mut r = answer();
    let mut p = Panel::default();
    assert_eq!(sort_line(&p, &r, &cat).count, "1,391,010");
    r.unsorted = 0;
    assert!(sort_line(&p, &r, &cat).count.is_empty());
    // Pressed, it says so even when the next answer has nothing left.
    p.answered(Press::Sort, Ok(()), Instant::now());
    let sort = sort_line(&p, &r, &cat);
    assert_eq!(sort.count, "0");
    assert_eq!(sort.said, says::SORT.done);
}

#[test]
fn a_restart_is_over_when_an_answer_comes_from_a_new_service() {
    let mut p = Panel::default();
    let mut r = answer();
    r.uptime_ms = 100_000;
    assert!(!p.took(r.clone()));
    assert!(p.step(Press::Restart, "go"));
    p.answered(Press::Restart, Ok(()), Instant::now());
    let cat = english();
    assert_eq!(foot(&p, &r, &cat).restart, says::RESTART.done);
    // The old service, answering once more before it went.
    r.uptime_ms = 102_000;
    assert!(!p.took(r.clone()));
    assert!(p.restarting.is_some());
    r.uptime_ms = 1_500;
    assert!(p.took(r.clone()));
    assert!(p.restarting.is_none());
    assert_eq!(foot(&p, &r, &cat).restart, says::RESTART.button);

    // A service younger than the restart takes: the lost connection tells.
    let mut p = Panel::default();
    r.uptime_ms = 800;
    p.took(r.clone());
    p.step(Press::Restart, "go");
    p.lost();
    r.uptime_ms = 1_200;
    assert!(p.took(r));
}

#[test]
fn a_refused_restart_says_why_in_the_foot() {
    let cat = english();
    let r = answer();
    let mut p = Panel::default();
    p.took(r.clone());
    p.step(Press::Restart, "go");
    p.answered(Press::Restart, Err("not here".into()), Instant::now());
    let f = foot(&p, &r, &cat);
    assert_eq!(f.said, "not here");
    assert!(f.no);
    assert_eq!(f.restart, says::RESTART.button);
    let p = Panel::default();
    assert_eq!(
        foot(&p, &r, &cat).said,
        "Service running · version 0.2.0 · up 1 h 13 min"
    );
}

#[test]
fn shares_fill_the_sparkline_and_a_restart_draws_no_spike() {
    let mut p = Panel::default();
    let mut r = answer();
    let mut at = |p: &mut Panel, uptime_ms, service_ms| {
        r.uptime_ms = uptime_ms;
        r.cpu.service_ms = service_ms;
        p.took(r.clone());
    };
    at(&mut p, 10_000, 1_000);
    assert!(p.spark.is_empty(), "one answer is no share");
    at(&mut p, 12_000, 1_500);
    at(&mut p, 14_000, 1_600);
    assert_eq!(p.spark, [25.0, 5.0]);
    // Started again in between: its uptime went back, and nothing is drawn.
    at(&mut p, 1_000, 100);
    assert_eq!(p.spark.len(), 2);
    for k in 0..40 {
        at(&mut p, 3_000 + k * 2_000, 200 + k * 20);
    }
    assert_eq!(p.spark.len(), SPARK, "a minute, no more");
    p.shut();
    assert!(p.spark.is_empty() && p.machine.is_none());
}

#[test]
fn the_sparkline_is_newest_at_the_right_and_at_least_ten_percent_tall() {
    assert_eq!(spark(&[1.0]), None);
    let (line, area, end) = spark(&[0.0, 5.0]).expect("two samples");
    assert_eq!(line, "M 289.7 56.0 L 300.0 32.0");
    assert_eq!(
        area,
        "M 289.7 56.0 L 300.0 32.0 L 300.0 56.0 L 289.7 56.0 Z"
    );
    assert_eq!(end, 32.0);
    // Above the floor, the highest sample is the top of the box.
    let (line, _, end) = spark(&[20.0, 40.0]).expect("two samples");
    assert_eq!(line, "M 289.7 32.0 L 300.0 8.0");
    assert_eq!(end, 8.0);
    // An idle service is one segment, not twenty-nine in a row.
    let (line, _, _) = spark(&[0.0, 0.0, 0.0, 0.0]).expect("four samples");
    assert_eq!(line, "M 269.0 56.0 L 300.0 56.0");
    let full: Vec<f32> = (0..SPARK).map(|k| k as f32).collect();
    assert!(spark(&full).expect("full").0.starts_with("M 0.0 "));
}

#[test]
fn memory_rows_are_shares_of_the_largest_with_the_peak_as_a_ghost() {
    let cat = english();
    let r = answer();
    let rows = mem_lines(&r, &cat);
    let labels: Vec<&str> = rows.iter().map(|m| m.label.as_str()).collect();
    assert_eq!(labels, ["Service", "Window", "MCP servers"]);
    assert_eq!(rows[0].note, "at most 480.0 MB since it started");
    assert_eq!(rows[0].ghost, 1.0);
    assert!((rows[0].share - 65.0 / 480.0).abs() < 1e-6);
    assert_eq!(rows[2].value, "28.0 MB");
    let m = machine(&r, &cat).expect("the machine says");
    assert_eq!(m.us_said, "Scour 143.0 MB");
    assert_eq!(m.said, "11.0 GB of 32.0 GB in use");
    assert!((m.us + m.others - 11.0 / 32.0).abs() < 1e-4);
    // A system that says nothing gets no rows and no bar.
    let mut quiet = r.clone();
    quiet.memory = Memory::default();
    quiet.running.clear();
    assert!(mem_lines(&quiet, &cat).is_empty());
    assert_eq!(machine(&quiet, &cat), None);
}

#[test]
fn programs_come_in_the_contracts_order_and_mcp_says_why_and_how_old() {
    let cat = english();
    let mut r = answer();
    r.running.reverse();
    let rows = run_lines(&r, &cat);
    assert_eq!(rows[0].label, "Window");
    assert!(rows[0].note.is_empty());
    assert_eq!(rows[1].count, "14");
    assert_eq!(
        rows[1].note,
        format!(
            "{} {}",
            says::MCP_NOTE,
            says::OLDEST.replace("{u}", "1 d 3 h")
        )
    );
}

#[test]
fn the_processor_is_said_short_and_long_and_in_turkish_percent_first() {
    let cat = english();
    assert_eq!(
        percent(&cat, 0.7),
        ("0.7%".to_owned(), "0.7% of one core".to_owned())
    );
    let mut p = Panel::default();
    let mut r = answer();
    p.took(r.clone());
    r.uptime_ms += 2_000;
    r.cpu.service_ms += 14;
    r.cpu.busy += 30;
    r.cpu.total += 1_000;
    p.took(r.clone());
    let (short, _, machine) = cpu_said(&p, &r, &cat);
    assert_eq!(short, "0.7%");
    assert_eq!(machine, "machine 3.0% busy · 20 threads · load 2.30");

    crate::MARKS.with(|m| m.set(('.', ',')));
    let tr = Catalogue::for_language("tr");
    assert_eq!(percent(&tr, 0.7).0, "%0,7");
}
