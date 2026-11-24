use super::*;

#[test]
fn the_maximal_directories_of_a_sorted_list_are_found_in_one_pass() {
    // The trap is the sibling that sorts between a directory and its
    // children: `-` is 0x2D and `/` is 0x2F, so `/a-b` lands after `/a`.
    let sorted = [
        "/home",
        "/home/hasan",
        "/home/hasan/Projeler",
        "/mnt",
        "/mnt/depo",
    ];
    assert_eq!(maximal(&sorted), vec!["/home", "/mnt"]);

    assert_eq!(maximal(&["/a", "/a-b", "/a/x"]), vec!["/a", "/a-b"]);
    assert_eq!(maximal(&["/only"]), vec!["/only"]);
    assert!(maximal(&[]).is_empty());
}

/// The loop `finish` runs, in isolation.
fn maximal(sorted: &[&str]) -> Vec<String> {
    let mut tops: Vec<usize> = Vec::new();
    for i in 0..sorted.len() {
        if tops.iter().any(|&t| strictly_below(sorted[i], sorted[t])) {
            continue;
        }
        tops.push(i);
    }
    tops.into_iter().map(|i| sorted[i].to_owned()).collect()
}

#[test]
fn a_child_is_one_level_down_and_a_grandchild_is_not() {
    assert!(is_child("/a/b", "/a"));
    assert!(!is_child("/a/b/c", "/a"));
    assert!(!is_child("/a", "/a"));
    // The sibling that sorts between a directory and its own children, and
    // which a naive range scan swallows: `-` is 0x2D and `/` is 0x2F.
    assert!(!is_child("/a-1/b", "/a"));
    assert!(!strictly_below("/a-1", "/a"));
}

/// The two ways that keep a total per child or per top count what the
/// files say: over siblings that sort between a folder and its children —
/// which the general roll-up's stack gets wrong, closing `/h/a` at
/// `/h/a-b` and handing `/h/a/x` to the parent — a directory the table
/// skips, two sources, two segments, dead rows and a filter.
#[test]
fn a_report_kept_per_child_is_the_report_rolled_up() {
    use scour_core::{Index, SourceId};

    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = crate::NativeIndex::open_or_create(tmp.path()).expect("create");
    let entry = |source: u32, path: &str, size: i64, dir: bool| scour_core::Entry {
        id: scour_core::EntryId::path_hash(SourceId(source), path),
        path: path.to_owned(),
        is_dir: dir,
        meta: scour_core::Meta {
            mtime: 1_000_000 - size * 86_400,
            size,
            disk: size * 2,
            ..scour_core::Meta::UNKNOWN
        },
    };
    let mut first = Vec::new();
    for (source, top) in [(0u32, "/h"), (1, "/m")] {
        for d in ["", "/a", "/a-b", "/a-b/c", "/a/x", "/a.old", "/empty"] {
            first.push(entry(source, &format!("{top}{d}"), 4_096, true));
        }
        for (i, f) in [
            "/a/f",
            "/a-b/f",
            "/a-b/c/rapor",
            "/a/x/f",
            "/a.old/rapor",
            "/f",
        ]
        .iter()
        .enumerate()
        {
            first.push(entry(
                source,
                &format!("{top}{f}"),
                10 * (i as i64 + 1),
                false,
            ));
        }
        first.push(entry(source, &format!("{top}/g/h/i/rapor"), 77, false));
    }
    index
        .apply(&mut first.into_iter().map(scour_core::Change::Upsert))
        .expect("apply");
    index.commit().expect("commit");
    let second: Vec<_> = (0..30)
        .map(|i| entry(0, &format!("/h/a/x/n{i}"), i, i % 4 == 0))
        .chain([entry(0, "/elsewhere/f", 5, false)])
        .collect();
    index
        .apply(&mut second.into_iter().map(scour_core::Change::Upsert))
        .expect("apply");
    index.commit().expect("commit");
    index
        .apply(
            &mut [scour_core::Change::RemoveSubtree {
                path: "/h/a-b/c".into(),
            }]
            .into_iter(),
        )
        .expect("remove");
    index.commit().expect("commit");

    for path in ["", "/", "/h", "/h/", "/h/a", "/m", "/h/g", "/nothing"] {
        for query in ["", "rapor", "n1"] {
            let req = UsageRequest {
                path: path.to_owned(),
                top: 50,
                query: scour_query::parse(query),
            };
            // Counted from the files themselves: what lies under the scope,
            // and under each child of it by the path's next part.
            let files: Vec<scour_core::Hit> = index
                .search(&scour_core::SearchRequest {
                    query: scour_query::parse(query),
                    page: scour_core::Page::new(0, 10_000),
                    ..Default::default()
                })
                .expect("search")
                .hits
                .into_iter()
                .filter(|h| !h.is_dir)
                .collect();
            let scope = path.trim_end_matches('/');
            let mut root = Own::default();
            let mut kids: std::collections::BTreeMap<String, Own> = Default::default();
            for h in &files {
                let Some(rest) = h.path.strip_prefix(&format!("{scope}/")) else {
                    continue;
                };
                let mut own = Own {
                    bytes: h.meta.size as u64,
                    disk: h.meta.disk as u64,
                    files: 1,
                    ..Own::default()
                };
                own.age[band(1_000_000 - h.meta.mtime)] = own.bytes;
                root.add(&own);
                if let Some((child, _)) = rest.split_once('/') {
                    kids.entry(child.to_owned()).or_default().add(&own);
                }
            }
            let got = index
                .with_segments(|segments| {
                    if let Some(mut c) = Children::new(&req, 1_000_000) {
                        for live in segments {
                            c.add_segment(&live.view()?)?;
                        }
                        return c.finish(Instant::now());
                    }
                    let mut e = Everything::new(&req, 1_000_000).expect("no scope");
                    for live in segments {
                        assert!(e.add_segment(&live.view()?)?);
                    }
                    e.finish(Instant::now())
                })
                .expect("kept per child");
            let at = format!("path {path:?} query {query:?}");
            assert_eq!(
                (got.root.bytes, got.root.disk, got.root.files, got.root.age),
                (root.bytes, root.disk, root.files, root.age),
                "{at}"
            );
            if scope.is_empty() {
                // Everything lies under `/`, the one top this index has.
                assert_eq!(got.children.len(), 1, "{at}");
                assert_eq!(got.children[0].path, "/", "{at}");
                assert_eq!(got.children[0].files, root.files, "{at}");
                continue;
            }
            let shown: std::collections::BTreeMap<String, (u64, u64, u64)> = got
                .children
                .iter()
                .filter(|c| c.files > 0)
                .map(|c| (c.path.clone(), (c.bytes, c.disk, c.files)))
                .collect();
            let counted: std::collections::BTreeMap<String, (u64, u64, u64)> = kids
                .iter()
                .map(|(k, o)| (format!("{scope}/{k}"), (o.bytes, o.disk, o.files)))
                .collect();
            assert_eq!(shown, counted, "{at}");
            assert!(got.child_count as usize >= counted.len(), "{at}");
        }
    }
}

#[test]
fn the_bands_are_the_six_they_claim_to_be() {
    assert_eq!(band(0), 0);
    assert_eq!(band(DAY - 1), 0);
    assert_eq!(band(DAY), 1);
    assert_eq!(band(400 * DAY), 5);
    // A file with a timestamp in the future is not older than everything.
    assert_eq!(band(-1), 0);
}
