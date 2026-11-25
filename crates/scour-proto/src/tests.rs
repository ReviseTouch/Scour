use super::*;
use scour_core::{
    Error, FacetBy, FacetResponse, IndexStats, MaintReport, Maintenance, Page, SearchResponse,
    SortKey, Status, TreeNode,
};

#[test]
fn what_changes_something_is_named_one_by_one() {
    // Named one by one: `is_mutating` is exhaustive, so a new variant cannot be
    // forgotten, but an existing one could be quietly moved to the other side.
    let mutating: Vec<Request> = vec![
        Request::Rescan { path: None },
        Request::Maintain {
            level: Maintenance::Compact,
        },
        Request::Shutdown {},
        // A write to the desktop's thumbnail cache, and processes started for it.
        Request::Thumbnails {
            files: Vec::new(),
            size: None,
        },
    ];
    for r in &mutating {
        assert!(r.is_mutating(), "{} has to be refused", r.name());
    }

    let readonly: Vec<Request> = vec![
        Request::Search {
            query: String::new(),
            sort: SortKey::default(),
            descending: true,
            page: Page::default(),
        },
        Request::Count {
            query: String::new(),
            cap: 1,
        },
        Request::Sources {},
        Request::Status {},
        Request::Stats {},
        Request::Syntax {},
        Request::Reread { from_start: true },
        Request::Await {
            since: 0,
            timeout_ms: 1,
        },
    ];
    for r in &readonly {
        assert!(!r.is_mutating(), "{} is not a write", r.name());
    }
}

#[test]
fn a_minimal_search_request_needs_only_a_query() {
    // Everything a caller can reasonably leave out has a default: the common
    // caller is a person typing JSON or a model filling in a schema.
    let call: Call =
        serde_json::from_str(r#"{"id":1,"op":"search","query":"rapor"}"#).expect("parse");
    assert_eq!(call.id, 1);
    let Request::Search {
        query,
        sort,
        descending,
        page,
    } = call.request
    else {
        panic!("expected a search");
    };
    assert_eq!(query, "rapor");
    assert_eq!(sort, SortKey::Modified);
    assert!(descending, "newest first is what a search box shows");
    assert_eq!(page.limit, 200);
}

/// The faces word a disk by these names, so they must be serde's own.
#[test]
fn a_disks_wire_names_are_the_ones_serde_writes() {
    use DiskState::*;
    for st in [Auto, Chosen, Configured, Off, Asking, Later, Manual] {
        let json = serde_json::to_string(&st).expect("state");
        assert_eq!(json, format!("\"{}\"", st.as_str()));
    }
    use scour_volumes::Kind;
    for k in [
        Kind::System,
        Kind::Fixed,
        Kind::Removable,
        Kind::Network,
        Kind::Optical,
    ] {
        let json = serde_json::to_string(&k).expect("kind");
        assert_eq!(json, format!("\"{}\"", k.as_str()));
    }
}

#[test]
fn every_request_round_trips() {
    let all = [
        Request::Search {
            query: "x".into(),
            sort: SortKey::Size,
            descending: false,
            page: Page::new(20, 10),
        },
        Request::Count {
            query: "x".into(),
            cap: 50,
        },
        Request::Facets {
            query: String::new(),
            by: vec![FacetBy::Ext { top: 5 }],
        },
        Request::Tree {
            path: "/a".into(),
            depth: 2,
            limit: 10,
        },
        Request::Stat {
            path: "/a/b".into(),
        },
        Request::Explain {
            query: "size:abc".into(),
            cursor: Some(4),
        },
        Request::Grep {
            query: "ext:rs".into(),
            pattern: "unsafe".into(),
            mode: GrepMode::Regex,
            exact_case: true,
            binary: false,
            sort: SortKey::Size,
            descending: true,
            offset: 64,
            top: 5,
            hits: 1,
            read_budget: 1 << 20,
            max_size: 0,
            timeout_ms: 500,
            rate_mib: 40,
        },
        Request::Volumes {},
        Request::Postpone {
            volume: "uuid:abc".into(),
        },
        Request::Choose {
            volume: "uuid:abc".into(),
            index: true,
        },
        Request::Sources {},
        Request::Status {},
        Request::Stats {},
        Request::Reread { from_start: false },
        Request::Texts {},
        Request::ForgetTexts {},
        Request::Await {
            since: 9,
            timeout_ms: 1_000,
        },
        Request::Export {
            query: "kind:font".into(),
            columns: vec!["name".into(), "size".into()],
        },
        Request::Rescan {
            path: Some("/a".into()),
        },
        Request::Maintain {
            level: Maintenance::Rebuild,
        },
        Request::Syntax {},
        Request::Shutdown {},
        Request::Thumbnails {
            files: vec!["/a/b.png".into()],
            size: Some(512),
        },
    ];
    let mut names = Vec::new();
    for r in all {
        let json = serde_json::to_string(&r).expect("serialise");
        assert_eq!(
            serde_json::from_str::<Request>(&json).expect("deserialise"),
            r
        );
        names.push(r.name());
    }
    names.sort_unstable();
    let n = names.len();
    names.dedup();
    assert_eq!(names.len(), n, "every request needs its own name");
}

/// Every response shape, serialised and read back: an internally tagged enum
/// cannot carry a bare string or a sequence, and serde says so only when sending.
#[test]
fn every_response_round_trips() {
    let all = [
        Response::Search(SearchResponse::default()),
        Response::Count {
            total: 5,
            capped: true,
            misread: vec![scour_core::Span::new(0, 3, scour_core::Role::BadValue)],
            took_us: 0,
        },
        Response::Volumes {
            disks: vec![Disk {
                volume: scour_volumes::Volume {
                    id: "uuid:abc".into(),
                    roots: vec!["/mnt/yedek".into()],
                    label: "Yedek".into(),
                    kind: scour_volumes::Kind::Removable,
                    fs: "exfat".into(),
                    size: 64 << 30,
                },
                state: DiskState::Asking,
                seen: Seen {
                    walked: None,
                    live: true,
                    changed: Some(1_763_380_000),
                    walking: false,
                },
            }],
            folders: vec![],
        },
        Response::Facets(FacetResponse::default()),
        Response::Texts {
            on: true,
            dir: "/s/texts".into(),
            max_mb: 1024,
            documents: 3,
            no_text: 1,
            failed: 2,
            disk: 4096,
            dead: 9,
            reader: true,
        },
        Response::Grep(GrepResponse {
            files: vec![GrepFile {
                path: "/a/b.rs".into(),
                meta: scour_core::Meta::UNKNOWN,
                kind: scour_core::Kind::Code,
                hits: vec![GrepHit {
                    line: Some(3),
                    offset: 40,
                    text: "unsafe {".into(),
                    at: (0, 6),
                }],
                more: true,
            }],
            candidates: 9,
            next: Some(4),
            ..GrepResponse::default()
        }),
        Response::Tree {
            root: TreeNode {
                name: "u".into(),
                path: "/home/u".into(),
                is_dir: true,
                kind: scour_core::Kind::Dir,
                size: 0,
                mtime: 0,
                children: 3,
                nodes: Vec::new(),
                truncated: false,
            },
            took_us: 0,
        },
        Response::Explain {
            description: "everything".into(),
            needs_content: false,
            spans: vec![scour_core::Span::new(0, 3, scour_core::Role::Text)],
            completions: Vec::new(),
        },
        Response::Sources {
            sources: Vec::new(),
        },
        Response::Status(Status::default()),
        Response::Stats(IndexStats::default()),
        Response::Maintained(MaintReport::default()),
        Response::Accepted,
        Response::ExportChunk {
            csv: "a.txt,12\r\n".into(),
        },
        Response::ExportDone { rows: 2 },
        Response::Text {
            text: "hello".into(),
        },
        Response::Thumbnails(scour_thumbs::Made {
            ready: vec!["/a/b.png".into()],
            ran: 1,
        }),
    ];
    for r in all {
        let json = serde_json::to_string(&r)
            .unwrap_or_else(|e| panic!("{r:?} cannot be sent at all: {e}"));
        assert_eq!(
            serde_json::from_str::<Response>(&json).expect("deserialise"),
            r
        );
    }
}

#[test]
fn a_reply_carries_either_an_answer_or_a_typed_failure() {
    let ok = Reply::whole(7, Outcome::Ok(Response::Accepted));
    let json = serde_json::to_string(&ok).expect("serialise");
    assert_eq!(
        serde_json::from_str::<Reply>(&json).expect("deserialise"),
        ok
    );

    let bad = Reply::whole(8, Outcome::Error(Error::QueryTooShort { need: 3 }));
    let json = serde_json::to_string(&bad).expect("serialise");
    let back: Reply = serde_json::from_str(&json).expect("deserialise");
    let Outcome::Error(e) = back.outcome else {
        panic!("expected an error")
    };
    // The code is what a caller matches on; the English is a fallback.
    assert_eq!(e.code(), "query_too_short");
}

/// A whole answer says nothing about being one, and a piece says it. That
/// asymmetry is what makes the field free: only the frames nobody used to ask for
/// carry the extra key.
#[test]
fn only_a_piece_of_an_answer_says_that_more_follows() {
    let whole = serde_json::to_string(&Reply::whole(1, Outcome::Ok(Response::Accepted)))
        .expect("serialise");
    assert!(
        !whole.contains("more"),
        "a whole answer is the frame it always was: {whole}"
    );

    let piece = Reply::piece(
        1,
        Response::ExportChunk {
            csv: "a.txt\r\n".into(),
        },
    );
    let json = serde_json::to_string(&piece).expect("serialise");
    assert!(json.contains("\"more\":true"), "{json}");
    let back: Reply = serde_json::from_str(&json).expect("deserialise");
    assert!(back.more);
    assert_eq!(back.id, 1);

    // Read back as false when absent, as every older frame on the wire is.
    let old: Reply = serde_json::from_str(r#"{"id":4,"ok":{"result":"accepted"}}"#).expect("parse");
    assert!(!old.more);
}

/// Exactly one request is answered in pieces, and a client checks before
/// it asks — see `Request::streams`.
#[test]
fn only_the_export_answers_in_pieces() {
    assert!(
        Request::Export {
            query: String::new(),
            columns: Vec::new(),
        }
        .streams()
    );
    for r in [
        Request::Search {
            query: String::new(),
            sort: SortKey::default(),
            descending: true,
            page: Page::default(),
        },
        Request::Count {
            query: String::new(),
            cap: 1,
        },
        Request::Status {},
        Request::Shutdown {},
    ] {
        assert!(!r.streams(), "{} answers in one frame", r.name());
    }
}

/// A request's fields share one object with the call's number.
#[test]
fn a_disk_is_named_beside_the_calls_own_id() {
    let call: Call =
        serde_json::from_str(r#"{"id":7,"op":"postpone","volume":"uuid:abc"}"#).expect("parse");
    assert_eq!(call.id, 7);
    assert_eq!(
        call.request,
        Request::Postpone {
            volume: "uuid:abc".into()
        }
    );
}

/// The two fields a live client leaves out most of the time.
#[test]
fn waiting_needs_nothing_spelled_out() {
    let call: Call = serde_json::from_str(r#"{"id":3,"op":"await"}"#).expect("parse");
    let Request::Await { since, timeout_ms } = call.request else {
        panic!("expected a wait");
    };
    // Zero is "I have seen nothing", so a service that has applied anything
    // answers at once rather than making a new client wait out the timeout.
    assert_eq!(since, 0);
    assert_eq!(timeout_ms, 25_000);
}

#[test]
fn mutating_requests_are_identified() {
    assert!(
        !Request::Search {
            query: String::new(),
            sort: SortKey::Name,
            descending: true,
            page: Page::default()
        }
        .is_mutating()
    );
    assert!(
        !Request::Tree {
            path: "/".into(),
            depth: 1,
            limit: 1
        }
        .is_mutating()
    );
    assert!(Request::Rescan { path: None }.is_mutating());
    assert!(Request::Shutdown {}.is_mutating());
}
