//! The contract, exercised from outside.
//!
//! These tests are written against the public API only, because that is what
//! every other crate in the workspace sees. A refactor that keeps the internals
//! working but breaks a re-export is exactly the kind of thing this catches.

use std::io::Read;

use scour_core::{
    Caps, Change, DefaultFolder, Entry, EntryId, EntrySink, Error, Flow, Folder, Kind, Meta,
    ScanOptions, ScanReport, SearchRequest, SortKey, Source, SourceId, SourceInfo, SourceKind,
    WatchHandle,
};

fn entry(path: &str, is_dir: bool) -> Entry {
    Entry {
        id: EntryId::path_hash(SourceId(7), path),
        path: path.into(),
        is_dir,
        meta: Meta {
            mtime: 1_700_000_000,
            size: 42,
            ..Meta::UNKNOWN
        },
    }
}

#[test]
fn the_public_surface_is_reachable() {
    let e = entry("/home/u/Belgeler/RAPOR.PDF", false);
    assert_eq!(e.name(), "RAPOR.PDF");
    assert_eq!(e.ext(), "pdf");
    assert_eq!(e.kind(), Kind::Doc);
    assert_eq!(e.parent(), "/home/u/Belgeler");
}

#[test]
fn folding_is_reachable_through_the_trait_object() {
    // The engine holds a `&dyn Folder`; if that stops working, indexing and
    // querying can no longer be guaranteed to agree.
    let f: &dyn Folder = &DefaultFolder;
    assert_eq!(f.fold("İSTANBUL"), "istanbul");
    assert_eq!(f.fold("ısparta"), "isparta");
}

#[test]
fn an_ast_round_trips_through_json() {
    // The wire protocol and the MCP server both carry this tree as JSON. A
    // variant that does not survive the trip is a silently broken query.
    let req = SearchRequest::default();
    let json = serde_json::to_string(&req).expect("serialise");
    let back: SearchRequest = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(req, back);
    assert_eq!(back.sort, SortKey::Modified);
}

#[test]
fn changes_round_trip_through_json() {
    for c in [
        Change::Upsert(entry("/a/b.txt", false)),
        Change::Remove(EntryId::path_hash(SourceId(1), "/a/b.txt")),
        Change::RemoveSubtree { path: "/a".into() },
        Change::Rescan { path: "/a".into() },
    ] {
        let json = serde_json::to_string(&c).expect("serialise");
        assert_eq!(c, serde_json::from_str(&json).expect("deserialise"));
    }
}

/// A source that produces two entries and supports nothing else — enough to
/// prove the trait can actually be implemented outside this crate.
#[derive(Debug)]
struct TinySource;

impl Source for TinySource {
    fn id(&self) -> SourceId {
        SourceId(0)
    }

    fn describe(&self) -> SourceInfo {
        SourceInfo {
            id: SourceId(0),
            name: "tiny".into(),
            kind: SourceKind::Local,
            roots: vec!["/".into()],
            caps: self.caps(),
        }
    }

    fn caps(&self) -> Caps {
        Caps::CASE_SENSITIVE
    }

    fn scan(&self, _o: &ScanOptions, sink: &mut dyn EntrySink) -> scour_core::Result<ScanReport> {
        let mut n = 0;
        for p in ["/one.txt", "/two.rs"] {
            n += 1;
            if sink.push(entry(p, false)).is_stop() {
                return Ok(ScanReport {
                    entries: n,
                    cancelled: true,
                    ..Default::default()
                });
            }
        }
        Ok(ScanReport {
            entries: n,
            ..Default::default()
        })
    }

    fn watch(
        &self,
        _s: Box<dyn scour_core::ChangeSink>,
    ) -> scour_core::Result<Box<dyn WatchHandle>> {
        Err(Error::Unsupported { what: "watch" })
    }

    fn open(&self, _id: &EntryId) -> scour_core::Result<Box<dyn Read + Send>> {
        Err(Error::Unsupported { what: "open" })
    }

    fn stat(&self, path: &str) -> scour_core::Result<Entry> {
        Err(Error::NotFound { path: path.into() })
    }
}

#[derive(Default)]
struct Collect {
    seen: Vec<String>,
    stop_after: usize,
}

impl EntrySink for Collect {
    fn push(&mut self, e: Entry) -> Flow {
        self.seen.push(e.path);
        if self.stop_after > 0 && self.seen.len() >= self.stop_after {
            Flow::Stop
        } else {
            Flow::Continue
        }
    }
}

#[test]
fn a_source_can_be_implemented_and_driven() {
    let mut sink = Collect::default();
    let report = TinySource
        .scan(&ScanOptions::default(), &mut sink)
        .expect("scan");
    assert_eq!(sink.seen, vec!["/one.txt", "/two.rs"]);
    assert_eq!(report.entries, 2);
    assert!(!report.cancelled);
}

#[test]
fn a_sink_can_stop_a_walk() {
    // Backpressure is the reason `push` returns `Flow` at all: a bounded query
    // must not pay for a million entries to show a hundred.
    let mut sink = Collect {
        stop_after: 1,
        ..Default::default()
    };
    let report = TinySource
        .scan(&ScanOptions::default(), &mut sink)
        .expect("scan");
    assert_eq!(sink.seen.len(), 1);
    assert!(report.cancelled);
}

#[test]
fn unsupported_operations_say_so_rather_than_panicking() {
    // `Box<dyn Read>` is not `Debug`, so the Ok side cannot be unwrapped for a
    // message — match instead of reaching for `unwrap_err`.
    let Err(err) = TinySource.open(&EntryId::path_hash(SourceId(0), "/x")) else {
        panic!("a source without Caps::CONTENT must refuse to open");
    };
    assert_eq!(err.code(), "unsupported");
    assert!(!err.is_transient());
}
