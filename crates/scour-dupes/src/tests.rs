use super::*;
use std::path::PathBuf;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let p = std::env::temp_dir().join(format!("scour-dupes-{name}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("mkdir");
        Dir(p)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> (String, u64) {
        let p = self.0.join(name);
        std::fs::write(&p, bytes).expect("write");
        (p.to_string_lossy().into_owned(), bytes.len() as u64)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn opts(min: u64) -> Options {
    Options {
        min_size: min,
        read_budget: u64::MAX,
        top: 50,
    }
}

#[test]
fn the_same_bytes_under_three_names_are_one_group() {
    let d = Dir::new("three");
    let body = vec![b'a'; 5000];
    let files = vec![
        d.file("one", &body),
        d.file("two", &body),
        d.file("three", &body),
        d.file("other", &vec![b'b'; 5000]),
    ];
    let r = find(files, &opts(1000));
    assert_eq!(r.groups.len(), 1, "{:?}", r.groups);
    assert_eq!(r.groups[0].paths.len(), 3);
    assert_eq!(r.groups[0].certainty, Certainty::Content);
    assert_eq!(r.groups[0].waste(), 10_000);
    assert_eq!(r.waste, 10_000);
    assert_eq!(
        r.proven, 10_000,
        "read and compared, so all of it is proven"
    );
}

/// Two names of one inode free nothing when one goes: they are one file,
/// and only the real copy beside them is waste.
#[test]
#[cfg(unix)]
fn a_hard_link_is_one_file_and_not_a_copy() {
    let d = Dir::new("links");
    let body = vec![b'h'; 6000];
    let first = d.file("a-first", &body);
    let link = d.0.join("b-link");
    std::fs::hard_link(&first.0, &link).expect("link");
    let link = (link.to_string_lossy().into_owned(), first.1);
    let copy = d.file("c-copy", &body);

    let r = find(vec![first.clone(), link.clone(), copy.clone()], &opts(1000));
    assert_eq!(r.groups.len(), 1, "{:?}", r.groups);
    assert_eq!(r.groups[0].paths, vec![first.0.clone(), copy.0]);
    assert_eq!(r.waste, 6000, "one copy's worth, not two");

    // Names of one file and nothing else are no group at all.
    let r = find(vec![first.clone(), link.clone()], &opts(1000));
    assert!(r.groups.is_empty(), "{:?}", r.groups);

    // Told the names, a size-only answer knows it too, reading nothing.
    let named = |(p, size): (String, u64)| (p, size, 2);
    let r = find_named(
        vec![named(first), named(link)],
        &Options {
            read_budget: 0,
            ..opts(1000)
        },
    );
    assert!(r.groups.is_empty(), "{:?}", r.groups);
    assert_eq!(r.read, 0);
}

/// Same ends, three files, the first unlike the other two. Held only
/// against the first, B and C were never compared and the pair was lost.
fn abc(d: &Dir) -> Vec<(String, u64)> {
    let body = |mid: u8| {
        let mut v = vec![b'X'; 4096];
        v.extend(vec![mid; 16_384]);
        v.extend(vec![b'Z'; 4096]);
        v
    };
    vec![
        d.file("dup_a.bin", &body(b'A')),
        d.file("dup_b.bin", &body(b'B')),
        d.file("dup_c.bin", &body(b'B')),
    ]
}

#[test]
fn two_that_match_each_other_are_found_behind_one_that_matches_neither() {
    let d = Dir::new("abc");
    let files = abc(&d);
    let r = find(files.clone(), &opts(20_000));
    assert_eq!(r.groups.len(), 1, "{:?}", r.groups);
    assert_eq!(
        r.groups[0].paths,
        vec![files[1].0.clone(), files[2].0.clone()]
    );
    assert_eq!(r.groups[0].certainty, Certainty::Content);
    assert_eq!(r.waste, 24_576);
}

/// A budget is what may be read, asked before reading: one byte reads
/// nothing, rather than a whole size group's ends.
#[test]
fn a_budget_smaller_than_the_next_read_reads_nothing_more() {
    let d = Dir::new("budget");
    let r = find(
        abc(&d),
        &Options {
            min_size: 20_000,
            read_budget: 1,
            top: 100,
        },
    );
    assert_eq!(r.read, 0);
    assert_eq!(r.unconfirmed, 1);
    assert_eq!(r.groups[0].certainty, Certainty::Size);

    // Enough for the ends and not for one comparison: ends, unconfirmed.
    let r = find(
        abc(&d),
        &Options {
            min_size: 20_000,
            read_budget: 3 * 2 * EDGE,
            top: 100,
        },
    );
    assert!(r.read <= 3 * 2 * EDGE, "read {}", r.read);
    assert_eq!(r.groups[0].certainty, Certainty::Edges);
}

/// Same size, same edges, different in the middle — an in-place edit.
#[test]
fn two_files_that_differ_only_in_the_middle_are_not_the_same_file() {
    let d = Dir::new("middle");
    let mut a = vec![b'x'; 20_000];
    let mut b = a.clone();
    a[10_000] = b'1';
    b[10_000] = b'2';
    let r = find(vec![d.file("a", &a), d.file("b", &b)], &opts(1000));
    assert!(r.groups.is_empty(), "{:?}", r.groups);
    // And it was read to find that out.
    assert!(r.read > 0);
}

#[test]
fn files_that_differ_at_the_start_are_parted_without_being_read() {
    let d = Dir::new("edges");
    let a = vec![b'a'; 4 * 1024 * 1024];
    let mut b = a.clone();
    b[0] = b'z';
    let r = find(vec![d.file("a", &a), d.file("b", &b)], &opts(1024));
    assert!(r.groups.is_empty());
    // Two edges from each file and nothing more: 16 KB against 8 MB.
    assert!(r.read <= EDGE * 4, "read {} bytes", r.read);
}

/// Zero budget is an answer, not a failure: the size groups are free.
#[test]
fn nothing_read_still_reports_what_might_be_saved() {
    let d = Dir::new("nobudget");
    let body = vec![b'q'; 8000];
    let files = vec![d.file("one", &body), d.file("two", &body)];
    let r = find(
        files,
        &Options {
            min_size: 1000,
            read_budget: 0,
            top: 50,
        },
    );
    assert_eq!(r.read, 0);
    assert_eq!(r.groups.len(), 1);
    assert_eq!(r.groups[0].certainty, Certainty::Size);
    assert_eq!(r.groups[0].waste(), 8000);
    assert_eq!(r.unconfirmed, 1, "a partial answer must say it is partial");
    // Nothing was read, so nothing is proven.
    assert_eq!(r.proven, 0);
}

#[test]
fn the_biggest_saving_comes_first_even_when_it_is_not_the_biggest_file() {
    let d = Dir::new("order");
    let small = vec![b's'; 2000];
    let big = vec![b'B'; 9000];
    let mut files = vec![d.file("big1", &big), d.file("big2", &big)];
    for i in 0..8 {
        files.push(d.file(&format!("small{i}"), &small));
    }
    let r = find(files, &opts(1000));
    assert_eq!(r.groups.len(), 2);
    // 8 small copies waste 14,000; 2 big ones waste 9,000.
    assert_eq!(r.groups[0].size, 2000);
    assert_eq!(r.groups[0].waste(), 14_000);
    assert_eq!(r.groups[1].size, 9000);
}

#[test]
fn a_file_that_disappeared_does_not_take_the_answer_with_it() {
    let d = Dir::new("gone");
    let body = vec![b'g'; 3000];
    let mut files = vec![d.file("a", &body), d.file("b", &body), d.file("c", &body)];
    files.push(("/nonexistent/never-was".into(), 3000));
    let r = find(files, &opts(1000));
    assert_eq!(r.groups.len(), 1);
    assert_eq!(r.groups[0].paths.len(), 3);
}

#[test]
fn anything_under_the_floor_is_not_looked_at() {
    let d = Dir::new("floor");
    let tiny = vec![b't'; 10];
    let r = find(vec![d.file("a", &tiny), d.file("b", &tiny)], &opts(1000));
    assert!(r.groups.is_empty());
    assert_eq!(r.candidates, 0);
    assert_eq!(r.read, 0);
}

#[test]
fn a_comparison_reads_to_the_end_rather_than_to_the_first_short_read() {
    let d = Dir::new("chunky");
    // Longer than one chunk, differing only past the first boundary.
    let mut a = vec![b'c'; CHUNK * 2 + 500];
    let mut b = a.clone();
    a[CHUNK + 7] = b'1';
    b[CHUNK + 7] = b'2';
    let r = find(vec![d.file("a", &a), d.file("b", &b)], &opts(1000));
    assert!(r.groups.is_empty(), "{:?}", r.groups);
}

#[test]
fn empty_input_is_an_empty_report() {
    let r = find(Vec::new(), &opts(1));
    assert!(r.groups.is_empty());
    assert_eq!(r.candidates, 0);
    assert_eq!(r.waste, 0);
}

#[test]
fn the_total_does_not_change_when_the_list_is_cut() {
    let d = Dir::new("top");
    let mut files = Vec::new();
    for i in 0..5 {
        let body = vec![b'a' + i as u8; 2000 + i * 100];
        files.push(d.file(&format!("{i}-one"), &body));
        files.push(d.file(&format!("{i}-two"), &body));
    }
    let all = find(files.clone(), &opts(1000));
    let cut = find(
        files,
        &Options {
            top: 2,
            ..opts(1000)
        },
    );
    assert_eq!(all.groups.len(), 5);
    assert_eq!(cut.groups.len(), 2);
    assert_eq!(all.waste, cut.waste);
}
